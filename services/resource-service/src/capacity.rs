//! Explicit Kubernetes capacity provider for Resource-owned quota shells.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use contracts::UtcTimestamp;
use contracts::environment::{
    ObservedEnvironmentState, ResourceWorkCleanup, ResourceWorkCleanupStatus, ResourceWorkHandoff,
    ResourceWorkLeaseUpdate,
};
use contracts::resource::{CapacityClaim, ResourceLeaseState, ResourceRequest};
use reqwest::{Certificate, Client, Identity, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::watch;
use url::Url;

const FIELD_MANAGER: &str = "labweaver-resource-service";
const QUOTA_NAME: &str = "resource-quota";

/// Resource capacity worker configuration. Every binding is explicit and unique.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceCapacityConfiguration {
    pub poll_interval_milliseconds: u64,
    pub providers: Vec<KubernetesCapacityProviderConfiguration>,
    pub environment_handoff: EnvironmentHandoffConfiguration,
}

impl ResourceCapacityConfiguration {
    pub fn build_worker(
        self,
        store: crate::store::PgResourceStore,
    ) -> Result<CapacityReconcileWorker, CapacityProviderError> {
        if !(100..=60_000).contains(&self.poll_interval_milliseconds) || self.providers.is_empty() {
            return Err(CapacityProviderError::Configuration);
        }
        let mut providers = BTreeMap::new();
        for configuration in self.providers {
            let binding = configuration.binding.clone();
            if providers
                .insert(binding, KubernetesCapacityProvider::new(configuration)?)
                .is_some()
            {
                return Err(CapacityProviderError::Configuration);
            }
        }
        Ok(CapacityReconcileWorker {
            store,
            providers,
            environment_handoff: EnvironmentHandoffClient::new(self.environment_handoff)?,
            poll_interval: Duration::from_millis(self.poll_interval_milliseconds),
        })
    }
}

/// Explicit mTLS destination for the Environment-owned Work command. No ambient roots,
/// proxy, discovery, or alternate endpoint is permitted.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentHandoffConfiguration {
    pub base_uri: Url,
    pub ca_file: PathBuf,
    pub client_certificate_file: PathBuf,
    pub client_private_key_file: PathBuf,
    pub timeout_milliseconds: u64,
    pub system_actor_id: contracts::ActorId,
}

#[derive(Clone)]
struct EnvironmentHandoffClient {
    base_uri: Url,
    client: Client,
    system_actor_id: contracts::ActorId,
}

impl EnvironmentHandoffClient {
    fn new(configuration: EnvironmentHandoffConfiguration) -> Result<Self, CapacityProviderError> {
        if configuration.base_uri.scheme() != "https"
            || configuration.base_uri.host_str().is_none()
            || !configuration.ca_file.is_absolute()
            || !configuration.client_certificate_file.is_absolute()
            || !configuration.client_private_key_file.is_absolute()
            || !(1..=30_000).contains(&configuration.timeout_milliseconds)
        {
            return Err(CapacityProviderError::Configuration);
        }
        let ca = std::fs::read(&configuration.ca_file)
            .map_err(|_| CapacityProviderError::Configuration)?;
        let mut identity = std::fs::read(&configuration.client_certificate_file)
            .map_err(|_| CapacityProviderError::Configuration)?;
        identity.push(b'\n');
        identity.extend(
            std::fs::read(&configuration.client_private_key_file)
                .map_err(|_| CapacityProviderError::Configuration)?,
        );
        let client = Client::builder()
            .no_proxy()
            .https_only(true)
            .tls_built_in_root_certs(false)
            .add_root_certificate(
                Certificate::from_pem(&ca).map_err(|_| CapacityProviderError::Configuration)?,
            )
            .identity(
                Identity::from_pem(&identity).map_err(|_| CapacityProviderError::Configuration)?,
            )
            .timeout(Duration::from_millis(configuration.timeout_milliseconds))
            .build()
            .map_err(|_| CapacityProviderError::Configuration)?;
        Ok(Self {
            base_uri: configuration.base_uri,
            client,
            system_actor_id: configuration.system_actor_id,
        })
    }

    async fn handoff(
        &self,
        item: &crate::store::ProvisioningCapacityClaim,
    ) -> Result<(), CapacityProviderError> {
        let lease = &item.lease;
        if lease.state != ResourceLeaseState::Active {
            return Err(CapacityProviderError::HandoffFence);
        }
        let handoff = ResourceWorkHandoff {
            version: 1,
            request_id: item.request.id,
            request_revision: item.request.revision,
            lease_id: lease.id,
            lease_revision: lease.revision,
            claim_id: item.claim.id,
            claim_revision: item.claim.revision,
            environment_id: item.request.target.environment_id,
            course_id: item.request.course_id,
            owner_actor_id: item.request.requester_id,
            display_label: item.request.request_key.clone(),
            project_id: item.request.project_id,
            release_id: item.request.target.release_id,
            release_version: item.request.target.release_version,
            release_sha256: item.request.target.release_sha256,
            provider_binding: item.claim.provider_binding.clone(),
            capacity_binding: item.claim.id.to_string(),
            trace_id: format!("resource-handoff-{}", item.claim.id),
        };
        handoff
            .validate()
            .map_err(|_| CapacityProviderError::HandoffFence)?;
        let url = self
            .base_uri
            .join("internal/v1/resource/work-handoffs")
            .map_err(|_| CapacityProviderError::Configuration)?;
        let response = self
            .client
            .post(url)
            .json(&handoff)
            .send()
            .await
            .map_err(|_| CapacityProviderError::Unavailable)?;
        if response.status() == StatusCode::ACCEPTED {
            Ok(())
        } else {
            Err(CapacityProviderError::HandoffRejected)
        }
    }

    async fn sync_lease(
        &self,
        item: &crate::store::ProvisioningCapacityClaim,
    ) -> Result<(), CapacityProviderError> {
        let lease = &item.lease;
        let expires_at = lease
            .expires_at
            .ok_or(CapacityProviderError::HandoffFence)?;
        let update = ResourceWorkLeaseUpdate {
            version: 1,
            lease_id: lease.id,
            lease_revision: lease.revision,
            environment_id: item.request.target.environment_id,
            course_id: item.request.course_id,
            owner_actor_id: item.request.requester_id,
            capacity_binding: item.claim.id.to_string(),
            expires_at,
            trace_id: format!("resource-lease-sync-{}", lease.id),
        };
        update
            .validate()
            .map_err(|_| CapacityProviderError::HandoffFence)?;
        let response = self
            .client
            .post(
                self.base_uri
                    .join("internal/v1/resource/work-lease-updates")
                    .map_err(|_| CapacityProviderError::Configuration)?,
            )
            .json(&update)
            .send()
            .await
            .map_err(|_| CapacityProviderError::Unavailable)?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(CapacityProviderError::LeaseSyncRejected)
        }
    }

    async fn request_cleanup(
        &self,
        item: &crate::store::ProvisioningCapacityClaim,
    ) -> Result<(), CapacityProviderError> {
        let cleanup = ResourceWorkCleanup {
            version: 1,
            lease_id: item.lease.id,
            lease_revision: item.lease.revision,
            environment_id: item.request.target.environment_id,
            course_id: item.request.course_id,
            owner_actor_id: item.request.requester_id,
            capacity_binding: item.claim.id.to_string(),
            reason_code: if item.lease.revoke_reason_code.is_some() {
                "LW_RESOURCE_LEASE_REVOKED"
            } else {
                "LW_RESOURCE_LEASE_EXPIRED"
            }
            .into(),
            trace_id: format!("resource-work-cleanup-{}", item.lease.id),
        };
        cleanup
            .validate()
            .map_err(|_| CapacityProviderError::HandoffFence)?;
        let response = self
            .client
            .post(
                self.base_uri
                    .join("internal/v1/resource/work-cleanups")
                    .map_err(|_| CapacityProviderError::Configuration)?,
            )
            .json(&cleanup)
            .send()
            .await
            .map_err(|_| CapacityProviderError::Unavailable)?;
        if response.status() == StatusCode::ACCEPTED || response.status() == StatusCode::CONFLICT {
            Ok(())
        } else {
            Err(CapacityProviderError::CleanupRejected)
        }
    }

    async fn cleanup_status(
        &self,
        environment_id: contracts::EnvironmentId,
    ) -> Result<ResourceWorkCleanupStatus, CapacityProviderError> {
        let response = self
            .client
            .get(
                self.base_uri
                    .join(&format!(
                        "internal/v1/resource/work-cleanups/{environment_id}"
                    ))
                    .map_err(|_| CapacityProviderError::Configuration)?,
            )
            .send()
            .await
            .map_err(|_| CapacityProviderError::Unavailable)?;
        if !response.status().is_success() {
            return Err(CapacityProviderError::CleanupRejected);
        }
        response
            .json()
            .await
            .map_err(|_| CapacityProviderError::Readback)
    }
}

/// Reviewed non-secret binding for one capacity Provider. Selection is exact by `binding`.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KubernetesCapacityProviderConfiguration {
    pub binding: String,
    pub api_server: String,
    pub bearer_token_file: PathBuf,
    pub cluster_ca_file: PathBuf,
    pub request_timeout_milliseconds: u64,
    #[serde(default)]
    pub namespace_labels: BTreeMap<String, String>,
}

impl KubernetesCapacityProviderConfiguration {
    fn validate(&self) -> Result<(), CapacityProviderError> {
        if !valid_dns_label(&self.binding)
            || Url::parse(&self.api_server)
                .ok()
                .is_none_or(|url| url.scheme() != "https" || url.host_str().is_none())
            || !self.bearer_token_file.is_absolute()
            || !self.cluster_ca_file.is_absolute()
            || !(1..=30_000).contains(&self.request_timeout_milliseconds)
            || self.namespace_labels.iter().any(|(key, value)| {
                key.is_empty() || key.len() > 253 || value.len() > 63 || value.is_empty()
            })
        {
            return Err(CapacityProviderError::Configuration);
        }
        Ok(())
    }
}

/// Exact Resource-owned Kubernetes plan, safe to persist as a capacity claim snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KubernetesQuotaShellPlan {
    pub binding: String,
    pub namespace: String,
    pub claim_id: contracts::CapacityClaimId,
    pub claim_revision: contracts::Revision,
    pub quota_plan_sha256: contracts::Sha256Digest,
    pub namespace_document: Value,
    pub quota_document: Value,
}

impl KubernetesQuotaShellPlan {
    pub fn from_claim(
        configuration: &KubernetesCapacityProviderConfiguration,
        request: &ResourceRequest,
        claim: &CapacityClaim,
    ) -> Result<Self, CapacityProviderError> {
        configuration.validate()?;
        request
            .validate()
            .map_err(|_| CapacityProviderError::Plan)?;
        claim.validate().map_err(|_| CapacityProviderError::Plan)?;
        if claim.provider_binding != configuration.binding || claim.request_id != request.id {
            return Err(CapacityProviderError::BindingMismatch);
        }
        if !matches!(
            claim.state,
            contracts::resource::CapacityClaimState::Reserved
                | contracts::resource::CapacityClaimState::Provisioning
                | contracts::resource::CapacityClaimState::HandedOff
                | contracts::resource::CapacityClaimState::Releasing
        ) {
            return Err(CapacityProviderError::Plan);
        }
        // GPU classes are catalog values, not Kubernetes extended-resource names. Sprint 3
        // admits zero GPU capacity, so any such request must have been rejected before here.
        if claim.quota_resources.gpu.is_some() || claim.workload_resources.gpu.is_some() {
            return Err(CapacityProviderError::GpuUnsupported);
        }
        // Environment adopts this exact shell. A parallel `lw-work-*`
        // namespace would leave the runtime outside the approved quota and
        // make capacity release unverifiable.
        let namespace = format!("lw-env-{}", request.target.environment_id);
        if !valid_dns_label(&namespace) {
            return Err(CapacityProviderError::Plan);
        }
        let mut labels = configuration.namespace_labels.clone();
        labels.insert("labweaver.io/managed-by".into(), "resource-service".into());
        labels.insert(
            "labweaver.io/environment-id".into(),
            request.target.environment_id.to_string(),
        );
        labels.insert(
            "labweaver.io/capacity-claim-id".into(),
            claim.id.to_string(),
        );
        let annotations = json!({
            "labweaver.io/capacity-claim-revision": claim.revision.get().to_string(),
            "labweaver.io/quota-plan-sha256": claim.quota_plan_sha256.to_string(),
            "labweaver.io/provider-binding": claim.provider_binding,
        });
        let namespace_document = json!({
            "apiVersion": "v1", "kind": "Namespace",
            "metadata": {"name": namespace, "labels": labels, "annotations": annotations}
        });
        let hard = json!({
            "requests.cpu": format!("{}m", claim.quota_resources.cpu_millicores),
            "limits.cpu": format!("{}m", claim.quota_resources.cpu_millicores),
            "requests.memory": claim.quota_resources.memory_bytes.to_string(),
            "limits.memory": claim.quota_resources.memory_bytes.to_string(),
            "requests.storage": claim.quota_resources.storage_bytes.to_string(),
            "persistentvolumeclaims": "1",
            "pods": "2"
        });
        let quota_document = json!({
            "apiVersion": "v1", "kind": "ResourceQuota",
            "metadata": {"name": QUOTA_NAME, "namespace": namespace, "labels": labels, "annotations": annotations},
            "spec": {"hard": hard}
        });
        Ok(Self {
            binding: configuration.binding.clone(),
            namespace,
            claim_id: claim.id,
            claim_revision: claim.revision,
            quota_plan_sha256: claim.quota_plan_sha256,
            namespace_document,
            quota_document,
        })
    }
}

/// Provider readback used to fence later Environment adoption.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KubernetesQuotaShellReadback {
    pub namespace_uid: String,
    pub quota_uid: String,
}

/// Kubernetes API provider; no command execution, discovery fallback, or implicit binding exists.
#[derive(Clone)]
pub struct KubernetesCapacityProvider {
    configuration: KubernetesCapacityProviderConfiguration,
    api_server: Url,
    client: Client,
    token: String,
}

/// Executes at most one durable capacity transition per poll. Errors stay auditable as `blocked`.
pub struct CapacityReconcileWorker {
    store: crate::store::PgResourceStore,
    providers: BTreeMap<String, KubernetesCapacityProvider>,
    environment_handoff: EnvironmentHandoffClient,
    poll_interval: Duration,
}

impl CapacityReconcileWorker {
    /// Reconciles a single leased shell and returns whether work was found.
    pub async fn reconcile_once(&self) -> Result<bool, crate::store::ResourceStoreError> {
        let Some(item) = self.store.claim_next_capacity_shell().await? else {
            self.reconcile_ready_handoff().await?;
            self.reconcile_lease_sync().await?;
            self.reconcile_lease_cleanup().await?;
            return Ok(false);
        };
        let Some(provider) = self.providers.get(&item.claim.provider_binding) else {
            self.store
                .mark_capacity_shell_blocked(
                    item.claim.id,
                    item.claim.revision,
                    "LW_RESOURCE_CAPACITY_PROVIDER_UNAVAILABLE",
                )
                .await?;
            return Ok(true);
        };
        let plan = match KubernetesQuotaShellPlan::from_claim(
            &provider.configuration,
            &item.request,
            &item.claim,
        ) {
            Ok(plan) => plan,
            Err(error) => {
                tracing::error!(event = "resource.capacity.plan_rejected", claim_id = %item.claim.id, diagnostic_code = %error.diagnostic());
                if matches!(error, CapacityProviderError::Unavailable) {
                    self.store
                        .retry_or_block_capacity_shell(
                            item.claim.id,
                            item.claim.revision,
                            error.diagnostic(),
                        )
                        .await?;
                } else {
                    self.store
                        .mark_capacity_shell_blocked(
                            item.claim.id,
                            item.claim.revision,
                            error.diagnostic(),
                        )
                        .await?;
                }
                return Ok(true);
            }
        };
        match provider.reserve(&plan).await {
            Ok(readback) => {
                self.store
                    .mark_capacity_shell_ready(
                        item.claim.id,
                        item.claim.revision,
                        &plan.namespace,
                        &readback.namespace_uid,
                        &readback.quota_uid,
                    )
                    .await?;
            }
            Err(error) => {
                tracing::error!(event = "resource.capacity.reserve_failed", claim_id = %item.claim.id, diagnostic_code = %error.diagnostic());
                self.store
                    .mark_capacity_shell_blocked(
                        item.claim.id,
                        item.claim.revision,
                        error.diagnostic(),
                    )
                    .await?;
            }
        }
        self.reconcile_ready_handoff().await?;
        self.reconcile_lease_sync().await?;
        self.reconcile_lease_cleanup().await?;
        Ok(true)
    }

    async fn reconcile_ready_handoff(&self) -> Result<(), crate::store::ResourceStoreError> {
        let Some(item) = self.store.next_ready_capacity_handoff().await? else {
            return Ok(());
        };
        if item.lease.state != ResourceLeaseState::Active {
            let now = self.store.current_time().await?;
            let expires_at = UtcTimestamp::from_utc(
                now.get()
                    + time::Duration::seconds(
                        i64::try_from(item.request.requested_duration_seconds).map_err(|_| {
                            crate::store::ResourceStoreError::CapacityReadbackInvalid
                        })?,
                    ),
            )
            .map_err(|_| crate::store::ResourceStoreError::CapacityReadbackInvalid)?;
            self.store
                .activate_lease(
                    item.lease.id,
                    item.lease.revision,
                    now,
                    expires_at,
                    self.environment_handoff.system_actor_id,
                    &format!("resource-lease-activate-{}", item.claim.id),
                )
                .await?;
        }
        let refreshed = self
            .store
            .next_ready_capacity_handoff()
            .await?
            .ok_or(crate::store::ResourceStoreError::CapacityClaimStateConflict)?;
        match self.environment_handoff.handoff(&refreshed).await {
            Ok(()) => {
                self.store
                    .mark_capacity_handed_off(refreshed.claim.id, refreshed.claim.revision)
                    .await?;
            }
            Err(error) => {
                tracing::error!(event="resource.capacity.handoff_failed", claim_id=%refreshed.claim.id, diagnostic_code=%error.diagnostic());
                self.store
                    .retry_or_block_capacity_handoff(
                        refreshed.claim.id,
                        refreshed.claim.revision,
                        error.diagnostic(),
                    )
                    .await?;
            }
        }
        Ok(())
    }

    async fn reconcile_lease_sync(&self) -> Result<(), crate::store::ResourceStoreError> {
        let Some(item) = self.store.next_unsynced_active_lease().await? else {
            return Ok(());
        };
        match self.environment_handoff.sync_lease(&item).await {
            Ok(()) => {
                self.store
                    .mark_lease_synced(item.claim.id, item.lease.revision)
                    .await?;
            }
            Err(error) => {
                tracing::error!(
                    event = "resource.lease.sync_failed",
                    lease_id = %item.lease.id,
                    diagnostic_code = %error.diagnostic()
                );
            }
        }
        Ok(())
    }

    async fn reconcile_lease_cleanup(&self) -> Result<(), crate::store::ResourceStoreError> {
        let Some(item) = self
            .store
            .next_lease_cleanup(self.environment_handoff.system_actor_id)
            .await?
        else {
            return Ok(());
        };
        if item.claim.state == contracts::resource::CapacityClaimState::HandedOff {
            if let Err(error) = self.environment_handoff.request_cleanup(&item).await {
                tracing::error!(
                    event = "resource.lease.cleanup_request_failed",
                    lease_id = %item.lease.id,
                    diagnostic_code = %error.diagnostic()
                );
                self.store
                    .record_reconciliation_failure(
                        item.claim.id,
                        "expire_environment",
                        error.diagnostic(),
                    )
                    .await?;
                return Ok(());
            }
            self.store
                .mark_capacity_releasing(item.claim.id, item.claim.revision)
                .await?;
            return Ok(());
        }
        if item.claim.state == contracts::resource::CapacityClaimState::Blocked {
            let Some(provider) = self.providers.get(&item.claim.provider_binding) else {
                self.store
                    .record_reconciliation_failure(
                        item.claim.id,
                        "release_capacity",
                        "LW_RESOURCE_CAPACITY_PROVIDER_UNAVAILABLE",
                    )
                    .await?;
                return Ok(());
            };
            let plan = KubernetesQuotaShellPlan::from_claim(
                &provider.configuration,
                &item.request,
                &item.claim,
            )
            .map_err(|_| crate::store::ResourceStoreError::CapacityReadbackInvalid)?;
            if let Err(error) = provider.release_before_handoff(&plan).await {
                tracing::error!(
                    event = "resource.capacity.pre_handoff_release_failed",
                    claim_id = %item.claim.id,
                    diagnostic_code = %error.diagnostic()
                );
                self.store
                    .record_reconciliation_failure(
                        item.claim.id,
                        "release_capacity",
                        error.diagnostic(),
                    )
                    .await?;
                return Ok(());
            }
            self.store
                .mark_capacity_releasing(item.claim.id, item.claim.revision)
                .await?;
            return Ok(());
        }
        self.reconcile_releasing_capacity(&item).await
    }

    async fn reconcile_releasing_capacity(
        &self,
        item: &crate::store::ProvisioningCapacityClaim,
    ) -> Result<(), crate::store::ResourceStoreError> {
        let status = match self
            .environment_handoff
            .cleanup_status(item.request.target.environment_id)
            .await
        {
            Ok(status) => status,
            Err(error) => {
                tracing::error!(
                    event = "resource.lease.cleanup_readback_failed",
                    lease_id = %item.lease.id,
                    diagnostic_code = %error.diagnostic()
                );
                self.store
                    .record_reconciliation_failure(
                        item.claim.id,
                        "expire_environment",
                        error.diagnostic(),
                    )
                    .await?;
                return Ok(());
            }
        };
        if !status.cleanup_complete {
            if status.observed_state == ObservedEnvironmentState::Failed {
                let diagnostic = status
                    .diagnostic_code
                    .as_deref()
                    .unwrap_or("LW_RESOURCE_ENVIRONMENT_CLEANUP_FAILED");
                tracing::error!(
                    event = "resource.lease.cleanup_blocked",
                    lease_id = %item.lease.id,
                    diagnostic_code = diagnostic
                );
                self.store
                    .record_reconciliation_failure(item.claim.id, "expire_environment", diagnostic)
                    .await?;
            }
            return Ok(());
        }
        let Some(provider) = self.providers.get(&item.claim.provider_binding) else {
            tracing::error!(
                event = "resource.capacity.release_provider_missing",
                claim_id = %item.claim.id,
                diagnostic_code = "LW_RESOURCE_CAPACITY_PROVIDER_UNAVAILABLE"
            );
            self.store
                .record_reconciliation_failure(
                    item.claim.id,
                    "release_capacity",
                    "LW_RESOURCE_CAPACITY_PROVIDER_UNAVAILABLE",
                )
                .await?;
            return Ok(());
        };
        let plan = KubernetesQuotaShellPlan::from_claim(
            &provider.configuration,
            &item.request,
            &item.claim,
        )
        .map_err(|_| crate::store::ResourceStoreError::CapacityReadbackInvalid)?;
        match provider.verify_released(&plan).await {
            Ok(()) => {
                self.store
                    .complete_capacity_release(
                        item.claim.id,
                        item.claim.revision,
                        item.lease.id,
                        item.lease.revision,
                        self.environment_handoff.system_actor_id,
                    )
                    .await?;
            }
            Err(error) => {
                tracing::error!(
                    event = "resource.capacity.release_readback_failed",
                    claim_id = %item.claim.id,
                    diagnostic_code = %error.diagnostic()
                );
                self.store
                    .record_reconciliation_failure(
                        item.claim.id,
                        "release_capacity",
                        error.diagnostic(),
                    )
                    .await?;
            }
        }
        Ok(())
    }

    /// Runs until shutdown; a retry always acquires a new durable claim fence.
    pub async fn run(
        &self,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<(), crate::store::ResourceStoreError> {
        let mut interval = tokio::time::interval(self.poll_interval);
        loop {
            tokio::select! {
                changed = shutdown.changed() => { if changed.is_err() || *shutdown.borrow() { return Ok(()); } }
                _ = interval.tick() => { let _ = self.reconcile_once().await?; }
            }
        }
    }
}

impl KubernetesCapacityProvider {
    pub fn new(
        configuration: KubernetesCapacityProviderConfiguration,
    ) -> Result<Self, CapacityProviderError> {
        configuration.validate()?;
        let api_server = Url::parse(&configuration.api_server)
            .map_err(|_| CapacityProviderError::Configuration)?;
        let token = std::fs::read_to_string(&configuration.bearer_token_file)
            .map_err(|_| CapacityProviderError::Configuration)?;
        if token.trim().is_empty() {
            return Err(CapacityProviderError::Configuration);
        }
        let certificate = Certificate::from_pem(
            &std::fs::read(&configuration.cluster_ca_file)
                .map_err(|_| CapacityProviderError::Configuration)?,
        )
        .map_err(|_| CapacityProviderError::Configuration)?;
        let client = Client::builder()
            .https_only(true)
            .add_root_certificate(certificate)
            .timeout(Duration::from_millis(
                configuration.request_timeout_milliseconds,
            ))
            .build()
            .map_err(|_| CapacityProviderError::Configuration)?;
        Ok(Self {
            configuration,
            api_server,
            client,
            token: token.trim().into(),
        })
    }

    /// Creates a quota shell, or verifies a conflicting existing shell has the exact claim fence.
    pub async fn reserve(
        &self,
        plan: &KubernetesQuotaShellPlan,
    ) -> Result<KubernetesQuotaShellReadback, CapacityProviderError> {
        if plan.binding != self.configuration.binding {
            return Err(CapacityProviderError::BindingMismatch);
        }
        self.create_or_verify(
            "api/v1/namespaces",
            &plan.namespace_document,
            &plan.namespace_document,
        )
        .await?;
        let quota_path = format!("api/v1/namespaces/{}/resourcequotas", plan.namespace);
        self.create_or_verify(&quota_path, &plan.quota_document, &plan.quota_document)
            .await?;
        let namespace = self
            .get(&format!("api/v1/namespaces/{}", plan.namespace))
            .await?;
        let quota = self
            .get(&format!(
                "api/v1/namespaces/{}/resourcequotas/{QUOTA_NAME}",
                plan.namespace
            ))
            .await?;
        verify_document(&namespace, &plan.namespace_document)?;
        verify_document(&quota, &plan.quota_document)?;
        Ok(KubernetesQuotaShellReadback {
            namespace_uid: metadata_uid(&namespace)?,
            quota_uid: metadata_uid(&quota)?,
        })
    }

    /// Used only before successful handoff. Environment owns deletion after handoff.
    pub async fn release_before_handoff(
        &self,
        plan: &KubernetesQuotaShellPlan,
    ) -> Result<(), CapacityProviderError> {
        let response = self
            .client
            .delete(self.url(&format!("api/v1/namespaces/{}", plan.namespace))?)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|_| CapacityProviderError::Unavailable)?;
        if response.status().is_success() || response.status() == StatusCode::NOT_FOUND {
            Ok(())
        } else {
            Err(classify(response.status()))
        }
    }

    /// Proves that the exact capacity namespace is absent. A namespace with a
    /// mismatched fence is never treated as released.
    pub async fn verify_released(
        &self,
        plan: &KubernetesQuotaShellPlan,
    ) -> Result<(), CapacityProviderError> {
        if plan.binding != self.configuration.binding {
            return Err(CapacityProviderError::BindingMismatch);
        }
        let response = self
            .client
            .get(self.url(&format!("api/v1/namespaces/{}", plan.namespace))?)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|_| CapacityProviderError::Unavailable)?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(());
        }
        if !response.status().is_success() {
            return Err(classify(response.status()));
        }
        let actual: Value = response
            .json()
            .await
            .map_err(|_| CapacityProviderError::Readback)?;
        verify_document(&actual, &plan.namespace_document)?;
        Err(CapacityProviderError::ReleasePending)
    }

    async fn create_or_verify(
        &self,
        collection: &str,
        document: &Value,
        expected: &Value,
    ) -> Result<(), CapacityProviderError> {
        let response = self
            .client
            .post(self.url(collection)?)
            .bearer_auth(&self.token)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .header("X-Field-Manager", FIELD_MANAGER)
            .json(document)
            .send()
            .await
            .map_err(|_| CapacityProviderError::Unavailable)?;
        match response.status() {
            status if status.is_success() => Ok(()),
            StatusCode::CONFLICT => {
                let metadata = expected
                    .get("metadata")
                    .and_then(Value::as_object)
                    .ok_or(CapacityProviderError::Plan)?;
                let name = metadata
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or(CapacityProviderError::Plan)?;
                let namespace = metadata.get("namespace").and_then(Value::as_str);
                let path = namespace.map_or_else(
                    || format!("api/v1/namespaces/{name}"),
                    |value| format!("api/v1/namespaces/{value}/resourcequotas/{name}"),
                );
                verify_document(&self.get(&path).await?, expected)
            }
            status => Err(classify(status)),
        }
    }

    async fn get(&self, path: &str) -> Result<Value, CapacityProviderError> {
        let response = self
            .client
            .get(self.url(path)?)
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|_| CapacityProviderError::Unavailable)?;
        if !response.status().is_success() {
            return Err(classify(response.status()));
        }
        response
            .json()
            .await
            .map_err(|_| CapacityProviderError::Readback)
    }

    fn url(&self, path: &str) -> Result<Url, CapacityProviderError> {
        self.api_server
            .join(path)
            .map_err(|_| CapacityProviderError::Configuration)
    }
}

fn verify_document(actual: &Value, expected: &Value) -> Result<(), CapacityProviderError> {
    for pointer in [
        "/metadata/name",
        "/metadata/labels/labweaver.io~1managed-by",
        "/metadata/labels/labweaver.io~1environment-id",
        "/metadata/labels/labweaver.io~1capacity-claim-id",
        "/metadata/annotations/labweaver.io~1capacity-claim-revision",
        "/metadata/annotations/labweaver.io~1quota-plan-sha256",
        "/metadata/annotations/labweaver.io~1provider-binding",
    ] {
        if actual.pointer(pointer) != expected.pointer(pointer) {
            return Err(CapacityProviderError::IdentityMismatch);
        }
    }
    if expected.get("kind").and_then(Value::as_str) == Some("ResourceQuota")
        && actual.pointer("/spec/hard") != expected.pointer("/spec/hard")
    {
        return Err(CapacityProviderError::IdentityMismatch);
    }
    Ok(())
}

fn metadata_uid(document: &Value) -> Result<String, CapacityProviderError> {
    document
        .pointer("/metadata/uid")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or(CapacityProviderError::Readback)
}
fn classify(status: StatusCode) -> CapacityProviderError {
    if matches!(
        status,
        StatusCode::UNAUTHORIZED
            | StatusCode::FORBIDDEN
            | StatusCode::CONFLICT
            | StatusCode::UNPROCESSABLE_ENTITY
    ) {
        CapacityProviderError::Rejected
    } else {
        CapacityProviderError::Unavailable
    }
}
fn valid_dns_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !value.starts_with('-')
        && !value.ends_with('-')
}

#[derive(Debug, thiserror::Error)]
pub enum CapacityProviderError {
    #[error("LW_RESOURCE_CAPACITY_PROVIDER_CONFIGURATION_INVALID")]
    Configuration,
    #[error("LW_RESOURCE_CAPACITY_PROVIDER_BINDING_MISMATCH")]
    BindingMismatch,
    #[error("LW_RESOURCE_CAPACITY_PLAN_INVALID")]
    Plan,
    #[error("LW_RESOURCE_CAPACITY_EXHAUSTED")]
    GpuUnsupported,
    #[error("LW_RESOURCE_CAPACITY_IDENTITY_MISMATCH")]
    IdentityMismatch,
    #[error("LW_RESOURCE_CAPACITY_READBACK_INVALID")]
    Readback,
    #[error("LW_RESOURCE_CAPACITY_REJECTED")]
    Rejected,
    #[error("LW_RESOURCE_CAPACITY_UNAVAILABLE")]
    Unavailable,
    #[error("LW_RESOURCE_ENVIRONMENT_HANDOFF_FENCE_INVALID")]
    HandoffFence,
    #[error("LW_RESOURCE_ENVIRONMENT_HANDOFF_REJECTED")]
    HandoffRejected,
    #[error("LW_RESOURCE_ENVIRONMENT_LEASE_SYNC_REJECTED")]
    LeaseSyncRejected,
    #[error("LW_RESOURCE_ENVIRONMENT_CLEANUP_REJECTED")]
    CleanupRejected,
    #[error("LW_RESOURCE_CAPACITY_RELEASE_PENDING")]
    ReleasePending,
}

impl CapacityProviderError {
    const fn diagnostic(&self) -> &'static str {
        match self {
            Self::Configuration => "LW_RESOURCE_CAPACITY_PROVIDER_CONFIGURATION_INVALID",
            Self::BindingMismatch => "LW_RESOURCE_CAPACITY_PROVIDER_BINDING_MISMATCH",
            Self::Plan => "LW_RESOURCE_CAPACITY_PLAN_INVALID",
            Self::GpuUnsupported => "LW_RESOURCE_CAPACITY_EXHAUSTED",
            Self::IdentityMismatch => "LW_RESOURCE_CAPACITY_IDENTITY_MISMATCH",
            Self::Readback => "LW_RESOURCE_CAPACITY_READBACK_INVALID",
            Self::Rejected => "LW_RESOURCE_CAPACITY_REJECTED",
            Self::Unavailable => "LW_RESOURCE_CAPACITY_UNAVAILABLE",
            Self::HandoffFence => "LW_RESOURCE_ENVIRONMENT_HANDOFF_FENCE_INVALID",
            Self::HandoffRejected => "LW_RESOURCE_ENVIRONMENT_HANDOFF_REJECTED",
            Self::LeaseSyncRejected => "LW_RESOURCE_ENVIRONMENT_LEASE_SYNC_REJECTED",
            Self::CleanupRejected => "LW_RESOURCE_ENVIRONMENT_CLEANUP_REJECTED",
            Self::ReleasePending => "LW_RESOURCE_CAPACITY_RELEASE_PENDING",
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "test fixtures use expect to make invalid fixed vectors fail loudly"
)]
mod tests {
    use std::str::FromStr;

    use contracts::resource::{ResourceTarget, WorkloadResources};
    use contracts::{
        ActorId, CourseId, EnvironmentId, ReleaseId, ResourceApprovalId, ResourceRequestId,
    };

    use super::*;

    #[test]
    fn quota_shell_plan_is_fenced_to_claim_and_never_translates_gpu_catalog_values() {
        let configuration = KubernetesCapacityProviderConfiguration {
            binding: "kubernetes-standard".into(),
            api_server: "https://kubernetes.example.test/".into(),
            bearer_token_file: std::env::temp_dir().join("token"),
            cluster_ca_file: std::env::temp_dir().join("ca.pem"),
            request_timeout_milliseconds: 5_000,
            namespace_labels: BTreeMap::new(),
        };
        let request = ResourceRequest {
            id: ResourceRequestId::new(),
            generation: 1,
            request_key: "workbench-1".into(),
            requester_id: ActorId::new(),
            course_id: CourseId::new(),
            project_id: None,
            target: ResourceTarget {
                environment_id: EnvironmentId::new(),
                release_id: ReleaseId::new(),
                release_version: 1,
                release_sha256: digest(),
            },
            requested_resources: resources(),
            requested_duration_seconds: 600,
            state: contracts::resource::ResourceRequestState::Reviewing,
            revision: contracts::Revision::new(1).expect("positive"),
            created_at: timestamp(),
            updated_at: timestamp(),
            diagnostic_code: None,
        };
        let claim = CapacityClaim {
            id: contracts::CapacityClaimId::new(),
            request_id: request.id,
            approval_id: ResourceApprovalId::new(),
            provider_binding: configuration.binding.clone(),
            policy_sha256: digest(),
            workload_resources: resources(),
            quota_resources: resources(),
            quota_plan_sha256: digest(),
            state: contracts::resource::CapacityClaimState::Reserved,
            revision: contracts::Revision::new(1).expect("positive"),
        };
        let plan = KubernetesQuotaShellPlan::from_claim(&configuration, &request, &claim)
            .expect("valid plan");
        assert_eq!(
            plan.namespace,
            format!("lw-env-{}", request.target.environment_id)
        );
        assert_eq!(
            plan.quota_document
                .pointer("/spec/hard/requests.cpu")
                .and_then(Value::as_str),
            Some("500m")
        );
        let expected_claim_id = claim.id.to_string();
        assert_eq!(
            plan.namespace_document
                .pointer("/metadata/labels/labweaver.io~1capacity-claim-id")
                .and_then(Value::as_str),
            Some(expected_claim_id.as_str())
        );
        let mut gpu_claim = claim.clone();
        gpu_claim.quota_resources.gpu = Some(contracts::resource::GpuRequest {
            class: "gpu-a100".into(),
            count: 1,
        });
        assert!(matches!(
            KubernetesQuotaShellPlan::from_claim(&configuration, &request, &gpu_claim),
            Err(CapacityProviderError::GpuUnsupported)
        ));
    }

    fn resources() -> WorkloadResources {
        WorkloadResources {
            cpu_millicores: 500,
            memory_bytes: 512,
            storage_bytes: 1024,
            gpu: None,
        }
    }
    fn digest() -> contracts::Sha256Digest {
        contracts::Sha256Digest::from_str(&"a".repeat(64)).expect("digest")
    }
    fn timestamp() -> contracts::UtcTimestamp {
        contracts::UtcTimestamp::from_str("2026-07-30T00:00:00.000Z").expect("timestamp")
    }
}
