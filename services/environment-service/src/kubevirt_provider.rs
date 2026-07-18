use std::net::IpAddr;
use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use contracts::access::validate_ssh_public_key;
use contracts::authoring::{
    EnvironmentRuntimeSpec, NetworkPolicySpec, PrivilegeEscalationPolicy, PublicExposurePolicy,
    RootFilesystemPolicy, RuntimeKind, RuntimeUserPolicy,
};
use contracts::environment::{
    EndpointHealth, EnvironmentEndpoint, EnvironmentInstance, ObservedEnvironmentState,
};
use contracts::supply_chain::{ImageArtifact, VirtualMachineDiskFormat};
use contracts::{
    ArtifactRef, EndpointId, EnvironmentId, OperationId, Revision, Sha256Digest, UtcTimestamp,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::{
    ContainerReleasePolicy, ContainerReleaseResolver, EnvironmentProvider, ProviderFailure,
    ProviderFailureCode, ProviderObservation, ReconcileAction, ReleaseProjectionError,
    ResolvedContainerRelease,
};

pub const KUBEVIRT_BACKEND_PROTOCOL_VERSION: u8 = 1;
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const GATEWAY_LABEL_KEY: &str = "app.kubernetes.io/name";
const KUBEVIRT_NODE_LABEL_KEY: &str = "labweaver.io/kubevirt";
const KUBEVIRT_NODE_LABEL_VALUE: &str = "true";

/// Durable Environment operation identity carried across the KubeVirt/CDI boundary.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KubeVirtBackendFence {
    pub protocol_version: u8,
    pub environment_id: EnvironmentId,
    pub operation_id: OperationId,
    pub provider_step: u32,
    pub environment_generation: u64,
    pub attempt: u32,
    pub action: ReconcileAction,
    pub request_id: Sha256Digest,
    pub deadline_at: UtcTimestamp,
}

impl KubeVirtBackendFence {
    fn for_action(
        instance: &EnvironmentInstance,
        action: ReconcileAction,
    ) -> Result<Self, ProviderFailure> {
        let request_id = Sha256Digest::of_canonical(&json!({
            "protocolVersion": KUBEVIRT_BACKEND_PROTOCOL_VERSION,
            "environmentId": instance.id,
            "operationId": instance.operation.id,
            "providerStep": instance.operation.provider_step,
            "environmentGeneration": instance.generation,
            "attempt": instance.operation.attempt,
            "action": action,
        }))
        .map_err(|_| invalid_observation())?;
        Ok(Self {
            protocol_version: KUBEVIRT_BACKEND_PROTOCOL_VERSION,
            environment_id: instance.id,
            operation_id: instance.operation.id,
            provider_step: instance.operation.provider_step,
            environment_generation: instance.generation,
            attempt: instance.operation.attempt,
            action,
            request_id,
            deadline_at: instance.operation.deadline_at,
        })
    }
}

/// One deterministic Kubernetes/KubeVirt object applied by the reviewed backend.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KubeVirtResource {
    pub kind: String,
    pub namespace: Option<String>,
    pub name: String,
    pub document: Value,
}

/// Immutable VM resource plan. The backend resolves the private `ArtifactRef` without exposing URLs.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KubeVirtResourcePlan {
    pub environment_id: EnvironmentId,
    pub namespace: String,
    pub virtual_machine_name: String,
    pub data_volume_name: String,
    pub base_disk: ArtifactRef,
    pub base_disk_format: VirtualMachineDiskFormat,
    pub storage_class_name: String,
    pub resources: Vec<KubeVirtResource>,
    pub plan_sha256: Sha256Digest,
}

/// Minimal deterministic namespace deletion plan used after Access revocation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KubeVirtCleanupPlan {
    pub environment_id: EnvironmentId,
    pub namespace: String,
    pub virtual_machine_name: String,
    pub plan_sha256: Sha256Digest,
}

/// Complete readiness identity returned only after VM, guest agent and SSH converge.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KubeVirtRunningObservation {
    pub observed_environment_generation: u64,
    pub vm_resource_generation: u64,
    pub observed_vm_resource_generation: u64,
    pub vm_uid: Uuid,
    pub vmi_uid: Uuid,
    pub root_disk_uid: Uuid,
    pub guest_ip: IpAddr,
    pub service_cluster_ip: IpAddr,
    pub ssh_host_key_sha256: Sha256Digest,
    pub guest_agent_connected: bool,
    pub ssh_ready: bool,
    pub observed_at: UtcTimestamp,
}

/// Stop result proving the VMI disappeared while the VM and root disk identities were preserved.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KubeVirtStoppedObservation {
    pub observed_environment_generation: u64,
    pub vm_uid: Uuid,
    pub root_disk_uid: Uuid,
    pub vmi_absent: bool,
    pub observed_at: UtcTimestamp,
}

/// Exact backend seam for `KubeVirt` server-side apply, lifecycle subresources and cleanup.
#[async_trait]
pub trait KubeVirtProviderBackend: Send + Sync {
    async fn apply(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtResourcePlan,
    ) -> Result<KubeVirtRunningObservation, ProviderFailure>;

    async fn observe(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtResourcePlan,
    ) -> Result<KubeVirtRunningObservation, ProviderFailure>;

    async fn start(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtResourcePlan,
    ) -> Result<KubeVirtRunningObservation, ProviderFailure>;

    async fn stop(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtResourcePlan,
    ) -> Result<KubeVirtStoppedObservation, ProviderFailure>;

    async fn restart(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtResourcePlan,
    ) -> Result<KubeVirtRunningObservation, ProviderFailure>;

    async fn delete_namespace(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtCleanupPlan,
    ) -> Result<ArtifactRef, ProviderFailure>;
}

/// NATS adapter for the deployment-owned `KubeVirt` executor.
pub struct NatsKubeVirtProviderBackend {
    client: async_nats::Client,
    subject: String,
}

impl NatsKubeVirtProviderBackend {
    pub fn new(client: async_nats::Client, subject: String) -> Result<Self, ProviderFailure> {
        if !valid_subject(&subject) {
            return Err(configuration_invalid());
        }
        Ok(Self { client, subject })
    }

    async fn request(
        &self,
        fence: &KubeVirtBackendFence,
        request: KubeVirtBackendRequest<'_>,
    ) -> Result<KubeVirtBackendResponse, ProviderFailure> {
        if !request.matches_action(fence.action) {
            return Err(invalid_observation());
        }
        let payload = serde_json::to_vec(&KubeVirtBackendRequestEnvelope {
            fence: *fence,
            request,
        })
        .map_err(|_| invalid_observation())?;
        let message = self
            .client
            .request(self.subject.clone(), payload.into())
            .await
            .map_err(|_| unavailable())?;
        if message.payload.len() > MAX_RESPONSE_BYTES {
            return Err(invalid_observation());
        }
        let response: KubeVirtBackendResponseEnvelope =
            serde_json::from_slice(&message.payload).map_err(|_| invalid_observation())?;
        if response.protocol_version != fence.protocol_version
            || response.environment_id != fence.environment_id
            || response.operation_id != fence.operation_id
            || response.provider_step != fence.provider_step
            || response.environment_generation != fence.environment_generation
            || response.attempt != fence.attempt
            || response.action != fence.action
            || response.request_id != fence.request_id
        {
            return Err(invalid_observation());
        }
        Ok(response.response)
    }
}

#[async_trait]
impl KubeVirtProviderBackend for NatsKubeVirtProviderBackend {
    async fn apply(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtResourcePlan,
    ) -> Result<KubeVirtRunningObservation, ProviderFailure> {
        running_response(
            &self
                .request(fence, KubeVirtBackendRequest::Apply { plan })
                .await?,
            plan,
        )
    }

    async fn observe(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtResourcePlan,
    ) -> Result<KubeVirtRunningObservation, ProviderFailure> {
        running_response(
            &self
                .request(fence, KubeVirtBackendRequest::Observe { plan })
                .await?,
            plan,
        )
    }

    async fn start(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtResourcePlan,
    ) -> Result<KubeVirtRunningObservation, ProviderFailure> {
        running_response(
            &self
                .request(fence, KubeVirtBackendRequest::Start { plan })
                .await?,
            plan,
        )
    }

    async fn stop(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtResourcePlan,
    ) -> Result<KubeVirtStoppedObservation, ProviderFailure> {
        match self
            .request(fence, KubeVirtBackendRequest::Stop { plan })
            .await?
        {
            KubeVirtBackendResponse::Stopped {
                plan_sha256,
                observation,
            } if plan_sha256 == plan.plan_sha256 => Ok(observation),
            KubeVirtBackendResponse::Failed { failure } => Err(failure),
            _ => Err(invalid_observation()),
        }
    }

    async fn restart(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtResourcePlan,
    ) -> Result<KubeVirtRunningObservation, ProviderFailure> {
        running_response(
            &self
                .request(fence, KubeVirtBackendRequest::Restart { plan })
                .await?,
            plan,
        )
    }

    async fn delete_namespace(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtCleanupPlan,
    ) -> Result<ArtifactRef, ProviderFailure> {
        match self
            .request(fence, KubeVirtBackendRequest::DeleteNamespace { plan })
            .await?
        {
            KubeVirtBackendResponse::Deleted {
                plan_sha256,
                cleanup_evidence,
            } if plan_sha256 == plan.plan_sha256 && valid_artifact_ref(&cleanup_evidence) => {
                Ok(cleanup_evidence)
            }
            KubeVirtBackendResponse::Failed { failure } => Err(failure),
            _ => Err(ProviderFailure {
                code: ProviderFailureCode::CleanupFailed,
                retryable: true,
            }),
        }
    }
}

fn running_response(
    response: &KubeVirtBackendResponse,
    plan: &KubeVirtResourcePlan,
) -> Result<KubeVirtRunningObservation, ProviderFailure> {
    match response {
        KubeVirtBackendResponse::Running {
            plan_sha256,
            observation,
        } if *plan_sha256 == plan.plan_sha256 => Ok(*observation),
        KubeVirtBackendResponse::Failed { failure } => Err(*failure),
        _ => Err(invalid_observation()),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct KubeVirtBackendRequestEnvelope<'a> {
    #[serde(flatten)]
    fence: KubeVirtBackendFence,
    request: KubeVirtBackendRequest<'a>,
}

#[derive(Serialize)]
#[serde(
    tag = "backendAction",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
enum KubeVirtBackendRequest<'a> {
    Apply { plan: &'a KubeVirtResourcePlan },
    Observe { plan: &'a KubeVirtResourcePlan },
    Start { plan: &'a KubeVirtResourcePlan },
    Stop { plan: &'a KubeVirtResourcePlan },
    Restart { plan: &'a KubeVirtResourcePlan },
    DeleteNamespace { plan: &'a KubeVirtCleanupPlan },
}

impl KubeVirtBackendRequest<'_> {
    const fn matches_action(&self, action: ReconcileAction) -> bool {
        matches!(
            (self, action),
            (
                Self::Apply { .. },
                ReconcileAction::Provision | ReconcileAction::Reset
            ) | (Self::Observe { .. }, ReconcileAction::Observe)
                | (Self::Start { .. }, ReconcileAction::Start)
                | (Self::Stop { .. }, ReconcileAction::Stop)
                | (Self::Restart { .. }, ReconcileAction::Restart)
                | (Self::DeleteNamespace { .. }, ReconcileAction::Cleanup)
        )
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct KubeVirtBackendResponseEnvelope {
    protocol_version: u8,
    environment_id: EnvironmentId,
    operation_id: OperationId,
    provider_step: u32,
    environment_generation: u64,
    attempt: u32,
    action: ReconcileAction,
    request_id: Sha256Digest,
    response: KubeVirtBackendResponse,
}

#[derive(Deserialize)]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum KubeVirtBackendResponse {
    Running {
        plan_sha256: Sha256Digest,
        observation: KubeVirtRunningObservation,
    },
    Stopped {
        plan_sha256: Sha256Digest,
        observation: KubeVirtStoppedObservation,
    },
    Deleted {
        plan_sha256: Sha256Digest,
        cleanup_evidence: ArtifactRef,
    },
    Failed {
        failure: ProviderFailure,
    },
}

/// Deployment-owned mapping from an approved artifact to a CDI `DataSource` and `StorageClass`.
#[derive(Clone, Debug)]
pub struct KubeVirtStorageBinding {
    pub binding: String,
    pub storage_class_name: String,
    pub data_source_namespace: String,
    pub data_source_name: String,
}

impl KubeVirtStorageBinding {
    pub fn new(
        binding: String,
        storage_class_name: String,
        data_source_namespace: String,
        data_source_name: String,
    ) -> Result<Self, ReleaseProjectionError> {
        if !valid_binding(&binding)
            || !valid_dns_label(&storage_class_name)
            || !valid_dns_label(&data_source_namespace)
            || !valid_dns_label(&data_source_name)
        {
            return Err(ReleaseProjectionError::ConfigurationInvalid);
        }
        Ok(Self {
            binding,
            storage_class_name,
            data_source_namespace,
            data_source_name,
        })
    }
}

/// Public-only SSH bootstrap material. No private key or per-user credential is accepted.
#[derive(Clone, Debug)]
pub struct KubeVirtSshBootstrap {
    pub gateway_namespace: String,
    pub gateway_pod_label: String,
    pub guest_user: String,
    pub user_ca_public_key: String,
}

impl KubeVirtSshBootstrap {
    pub fn new(
        gateway_namespace: String,
        gateway_pod_label: String,
        guest_user: String,
        user_ca_public_key: &str,
    ) -> Result<Self, ReleaseProjectionError> {
        let public_key = validate_ssh_public_key(user_ca_public_key)
            .map_err(|_| ReleaseProjectionError::ConfigurationInvalid)?;
        if !valid_dns_label(&gateway_namespace)
            || !valid_dns_label(&gateway_pod_label)
            || !valid_guest_user(&guest_user)
        {
            return Err(ReleaseProjectionError::ConfigurationInvalid);
        }
        Ok(Self {
            gateway_namespace,
            gateway_pod_label,
            guest_user,
            user_ca_public_key: public_key.normalized_openssh,
        })
    }
}

/// Reviewed non-secret configuration for one exact `KubeVirt` Provider binding.
#[derive(Clone, Debug)]
pub struct KubeVirtProviderConfiguration {
    pub release_policy: ContainerReleasePolicy,
    pub storage: KubeVirtStorageBinding,
    pub ssh: KubeVirtSshBootstrap,
    pub resource_budget: KubeVirtResourceBudget,
}

/// Deployment-owned capacity reserved beyond the approved guest resources.
#[derive(Clone, Copy, Debug)]
pub struct KubeVirtResourceBudget {
    vmi_memory_overhead_bytes: u64,
    cdi_importer_cpu_request_millicores: u32,
    cdi_importer_cpu_limit_millicores: u32,
    cdi_importer_memory_request_bytes: u64,
    cdi_importer_memory_limit_bytes: u64,
    cdi_scratch_storage_bytes: u64,
}

impl KubeVirtResourceBudget {
    pub const fn new(
        vmi_memory_overhead_bytes: u64,
        cdi_importer_cpu_request_millicores: u32,
        cdi_importer_cpu_limit_millicores: u32,
        cdi_importer_memory_request_bytes: u64,
        cdi_importer_memory_limit_bytes: u64,
        cdi_scratch_storage_bytes: u64,
    ) -> Result<Self, ReleaseProjectionError> {
        if vmi_memory_overhead_bytes == 0
            || cdi_importer_cpu_request_millicores == 0
            || cdi_importer_cpu_limit_millicores < cdi_importer_cpu_request_millicores
            || cdi_importer_memory_request_bytes == 0
            || cdi_importer_memory_limit_bytes < cdi_importer_memory_request_bytes
            || cdi_scratch_storage_bytes == 0
        {
            return Err(ReleaseProjectionError::ConfigurationInvalid);
        }
        Ok(Self {
            vmi_memory_overhead_bytes,
            cdi_importer_cpu_request_millicores,
            cdi_importer_cpu_limit_millicores,
            cdi_importer_memory_request_bytes,
            cdi_importer_memory_limit_bytes,
            cdi_scratch_storage_bytes,
        })
    }
}

impl KubeVirtProviderConfiguration {
    #[must_use]
    pub const fn new(
        release_policy: ContainerReleasePolicy,
        storage: KubeVirtStorageBinding,
        ssh: KubeVirtSshBootstrap,
        resource_budget: KubeVirtResourceBudget,
    ) -> Self {
        Self {
            release_policy,
            storage,
            ssh,
            resource_budget,
        }
    }
}

/// Environment-owned durable VM identity projection, independent from executor memory.
#[async_trait]
pub trait KubeVirtObservationStore: Send + Sync {
    async fn record_running(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtResourcePlan,
        observation: &KubeVirtRunningObservation,
    ) -> Result<(), KubeVirtObservationStoreError>;

    async fn record_stopped(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtResourcePlan,
        observation: &KubeVirtStoppedObservation,
    ) -> Result<(), KubeVirtObservationStoreError>;

    async fn record_deleted(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtCleanupPlan,
        cleanup_evidence: &ArtifactRef,
    ) -> Result<(), KubeVirtObservationStoreError>;
}

/// `PostgreSQL` projection of the last accepted `KubeVirt` identity and deletion tombstone.
#[derive(Clone)]
pub struct PgKubeVirtObservationStore {
    pool: PgPool,
}

impl PgKubeVirtObservationStore {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    async fn record(
        &self,
        fence: &KubeVirtBackendFence,
        mut record: ObservationRecord,
    ) -> Result<(), KubeVirtObservationStoreError> {
        let mut transaction = self.pool.begin().await?;
        // `SELECT .. FOR UPDATE` cannot lock an absent first-observation row.
        // Serialize that insert race on the full environment identity before
        // reading so an older first completion cannot win `ON CONFLICT` last.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, 530001))")
            .bind(fence.environment_id.as_uuid())
            .fetch_one(&mut *transaction)
            .await?;
        let existing = sqlx::query(
            "SELECT state,environment_generation,attempt,provider_step,request_id, \
                    observation_sha256,vm_uid,root_disk_uid,ssh_host_key_sha256 \
             FROM environment.kubevirt_runtime_observations \
             WHERE environment_id=$1 FOR UPDATE",
        )
        .bind(fence.environment_id.as_uuid())
        .fetch_optional(&mut *transaction)
        .await?
        .map(|row| decode_stored_observation(&row))
        .transpose()?;

        if let Some(existing) = &existing {
            if existing.request_id == fence.request_id {
                if existing.state == record.state
                    && existing.observation_sha256 == record.observation_sha256
                {
                    transaction.commit().await?;
                    return Ok(());
                }
                return Err(KubeVirtObservationStoreError::IdentityMismatch);
            }
            if existing.state == "deleted" {
                return Err(KubeVirtObservationStoreError::Tombstoned);
            }
            if fence_tuple(fence)? <= existing.fence_tuple {
                return Err(KubeVirtObservationStoreError::StaleFence);
            }
        }
        validate_record_transition(existing.as_ref(), fence.action, &record)?;
        if let Some(existing) = &existing {
            record.vm_uid = record.vm_uid.or(existing.vm_uid);
            record.root_disk_uid = record.root_disk_uid.or(existing.root_disk_uid);
            if record.ssh_host_key_sha256.is_none() {
                record.ssh_host_key_sha256 = existing
                    .ssh_host_key_sha256
                    .map(|digest| digest.to_string());
            }
        }

        sqlx::query(
            "INSERT INTO environment.kubevirt_runtime_observations \
             (environment_id,state,operation_id,provider_step,environment_generation,attempt,request_id, \
              namespace,virtual_machine_name,vm_resource_generation,observed_vm_resource_generation, \
              vm_uid,vmi_uid,root_disk_uid,guest_ip,service_cluster_ip,ssh_host_key_sha256, \
              observation_sha256,cleanup_evidence,observed_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19, \
                     COALESCE($20,clock_timestamp())) \
             ON CONFLICT (environment_id) DO UPDATE SET \
              state=EXCLUDED.state,operation_id=EXCLUDED.operation_id,provider_step=EXCLUDED.provider_step, \
              environment_generation=EXCLUDED.environment_generation,attempt=EXCLUDED.attempt, \
              request_id=EXCLUDED.request_id,namespace=EXCLUDED.namespace, \
              virtual_machine_name=EXCLUDED.virtual_machine_name, \
              vm_resource_generation=EXCLUDED.vm_resource_generation, \
              observed_vm_resource_generation=EXCLUDED.observed_vm_resource_generation, \
              vm_uid=COALESCE(EXCLUDED.vm_uid,kubevirt_runtime_observations.vm_uid), \
              vmi_uid=EXCLUDED.vmi_uid, \
              root_disk_uid=COALESCE(EXCLUDED.root_disk_uid,kubevirt_runtime_observations.root_disk_uid), \
              guest_ip=EXCLUDED.guest_ip,service_cluster_ip=EXCLUDED.service_cluster_ip, \
              ssh_host_key_sha256=COALESCE(EXCLUDED.ssh_host_key_sha256,kubevirt_runtime_observations.ssh_host_key_sha256), \
              observation_sha256=EXCLUDED.observation_sha256,cleanup_evidence=EXCLUDED.cleanup_evidence, \
              observed_at=EXCLUDED.observed_at,updated_at=clock_timestamp()",
        )
        .bind(fence.environment_id.as_uuid())
        .bind(record.state)
        .bind(fence.operation_id.as_uuid())
        .bind(i32::try_from(fence.provider_step).map_err(|_| KubeVirtObservationStoreError::InvalidObservation)?)
        .bind(i64::try_from(fence.environment_generation).map_err(|_| KubeVirtObservationStoreError::InvalidObservation)?)
        .bind(i32::try_from(fence.attempt).map_err(|_| KubeVirtObservationStoreError::InvalidObservation)?)
        .bind(fence.request_id.to_string())
        .bind(record.namespace)
        .bind(record.virtual_machine_name)
        .bind(record.vm_resource_generation)
        .bind(record.observed_vm_resource_generation)
        .bind(record.vm_uid)
        .bind(record.vmi_uid)
        .bind(record.root_disk_uid)
        .bind(record.guest_ip)
        .bind(record.service_cluster_ip)
        .bind(record.ssh_host_key_sha256)
        .bind(record.observation_sha256.to_string())
        .bind(record.cleanup_evidence)
        .bind(record.observed_at.map(UtcTimestamp::get))
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(())
    }
}

#[async_trait]
impl KubeVirtObservationStore for PgKubeVirtObservationStore {
    async fn record_running(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtResourcePlan,
        observation: &KubeVirtRunningObservation,
    ) -> Result<(), KubeVirtObservationStoreError> {
        if plan.environment_id != fence.environment_id
            || !matches!(
                fence.action,
                ReconcileAction::Provision
                    | ReconcileAction::Observe
                    | ReconcileAction::Start
                    | ReconcileAction::Restart
                    | ReconcileAction::Reset
            )
            || !valid_running_observation(fence.environment_generation, observation)
        {
            return Err(KubeVirtObservationStoreError::InvalidObservation);
        }
        let observation_sha256 = Sha256Digest::of_canonical(observation)
            .map_err(|_| KubeVirtObservationStoreError::InvalidObservation)?;
        self.record(
            fence,
            ObservationRecord {
                state: "running",
                namespace: plan.namespace.clone(),
                virtual_machine_name: plan.virtual_machine_name.clone(),
                vm_resource_generation: Some(as_i64(observation.vm_resource_generation)?),
                observed_vm_resource_generation: Some(as_i64(
                    observation.observed_vm_resource_generation,
                )?),
                vm_uid: Some(observation.vm_uid),
                vmi_uid: Some(observation.vmi_uid),
                root_disk_uid: Some(observation.root_disk_uid),
                guest_ip: Some(observation.guest_ip.to_string()),
                service_cluster_ip: Some(observation.service_cluster_ip.to_string()),
                ssh_host_key_sha256: Some(observation.ssh_host_key_sha256.to_string()),
                observation_sha256,
                cleanup_evidence: None,
                observed_at: Some(observation.observed_at),
            },
        )
        .await
    }

    async fn record_stopped(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtResourcePlan,
        observation: &KubeVirtStoppedObservation,
    ) -> Result<(), KubeVirtObservationStoreError> {
        if plan.environment_id != fence.environment_id
            || fence.action != ReconcileAction::Stop
            || !valid_stopped_observation(fence.environment_generation, observation)
        {
            return Err(KubeVirtObservationStoreError::InvalidObservation);
        }
        let observation_sha256 = Sha256Digest::of_canonical(observation)
            .map_err(|_| KubeVirtObservationStoreError::InvalidObservation)?;
        self.record(
            fence,
            ObservationRecord {
                state: "stopped",
                namespace: plan.namespace.clone(),
                virtual_machine_name: plan.virtual_machine_name.clone(),
                vm_resource_generation: None,
                observed_vm_resource_generation: None,
                vm_uid: Some(observation.vm_uid),
                vmi_uid: None,
                root_disk_uid: Some(observation.root_disk_uid),
                guest_ip: None,
                service_cluster_ip: None,
                ssh_host_key_sha256: None,
                observation_sha256,
                cleanup_evidence: None,
                observed_at: Some(observation.observed_at),
            },
        )
        .await
    }

    async fn record_deleted(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtCleanupPlan,
        cleanup_evidence: &ArtifactRef,
    ) -> Result<(), KubeVirtObservationStoreError> {
        if plan.environment_id != fence.environment_id
            || fence.action != ReconcileAction::Cleanup
            || !valid_artifact_ref(cleanup_evidence)
        {
            return Err(KubeVirtObservationStoreError::InvalidObservation);
        }
        let observation_sha256 = Sha256Digest::of_canonical(&json!({
            "plan": plan,
            "cleanupEvidence": cleanup_evidence,
        }))
        .map_err(|_| KubeVirtObservationStoreError::InvalidObservation)?;
        self.record(
            fence,
            ObservationRecord {
                state: "deleted",
                namespace: plan.namespace.clone(),
                virtual_machine_name: plan.virtual_machine_name.clone(),
                vm_resource_generation: None,
                observed_vm_resource_generation: None,
                vm_uid: None,
                vmi_uid: None,
                root_disk_uid: None,
                guest_ip: None,
                service_cluster_ip: None,
                ssh_host_key_sha256: None,
                observation_sha256,
                cleanup_evidence: Some(
                    serde_json::to_value(cleanup_evidence)
                        .map_err(|_| KubeVirtObservationStoreError::InvalidObservation)?,
                ),
                observed_at: None,
            },
        )
        .await
    }
}

struct ObservationRecord {
    state: &'static str,
    namespace: String,
    virtual_machine_name: String,
    vm_resource_generation: Option<i64>,
    observed_vm_resource_generation: Option<i64>,
    vm_uid: Option<Uuid>,
    vmi_uid: Option<Uuid>,
    root_disk_uid: Option<Uuid>,
    guest_ip: Option<String>,
    service_cluster_ip: Option<String>,
    ssh_host_key_sha256: Option<String>,
    observation_sha256: Sha256Digest,
    cleanup_evidence: Option<Value>,
    observed_at: Option<UtcTimestamp>,
}

struct StoredObservation {
    state: String,
    fence_tuple: (i64, i32, i32),
    request_id: Sha256Digest,
    observation_sha256: Sha256Digest,
    vm_uid: Option<Uuid>,
    root_disk_uid: Option<Uuid>,
    ssh_host_key_sha256: Option<Sha256Digest>,
}

fn decode_stored_observation(
    row: &sqlx::postgres::PgRow,
) -> Result<StoredObservation, KubeVirtObservationStoreError> {
    Ok(StoredObservation {
        state: row.try_get("state")?,
        fence_tuple: (
            row.try_get("environment_generation")?,
            row.try_get("attempt")?,
            row.try_get("provider_step")?,
        ),
        request_id: row
            .try_get::<String, _>("request_id")?
            .parse()
            .map_err(|_| KubeVirtObservationStoreError::InvalidObservation)?,
        observation_sha256: row
            .try_get::<String, _>("observation_sha256")?
            .parse()
            .map_err(|_| KubeVirtObservationStoreError::InvalidObservation)?,
        vm_uid: row.try_get("vm_uid")?,
        root_disk_uid: row.try_get("root_disk_uid")?,
        ssh_host_key_sha256: row
            .try_get::<Option<String>, _>("ssh_host_key_sha256")?
            .map(|value| value.parse())
            .transpose()
            .map_err(|_| KubeVirtObservationStoreError::InvalidObservation)?,
    })
}

fn validate_record_transition(
    existing: Option<&StoredObservation>,
    action: ReconcileAction,
    record: &ObservationRecord,
) -> Result<(), KubeVirtObservationStoreError> {
    match record.state {
        "deleted" => Ok(()),
        "stopped" => {
            let existing = existing.ok_or(KubeVirtObservationStoreError::IdentityMismatch)?;
            if !matches!(existing.state.as_str(), "running" | "stopped")
                || existing.vm_uid != record.vm_uid
                || existing.root_disk_uid != record.root_disk_uid
            {
                return Err(KubeVirtObservationStoreError::IdentityMismatch);
            }
            Ok(())
        }
        "running" => {
            if let Some(existing) = existing {
                if action == ReconcileAction::Start && existing.state != "stopped" {
                    return Err(KubeVirtObservationStoreError::IdentityMismatch);
                }
                let replacing_disk = action == ReconcileAction::Reset;
                let record_host_key = record
                    .ssh_host_key_sha256
                    .as_deref()
                    .and_then(|value| value.parse().ok());
                if !replacing_disk
                    && (existing.vm_uid != record.vm_uid
                        || existing.root_disk_uid != record.root_disk_uid
                        || existing.ssh_host_key_sha256 != record_host_key)
                {
                    return Err(KubeVirtObservationStoreError::IdentityMismatch);
                }
            } else if !matches!(
                action,
                ReconcileAction::Provision | ReconcileAction::Reset | ReconcileAction::Observe
            ) {
                return Err(KubeVirtObservationStoreError::IdentityMismatch);
            }
            Ok(())
        }
        _ => Err(KubeVirtObservationStoreError::InvalidObservation),
    }
}

fn fence_tuple(
    fence: &KubeVirtBackendFence,
) -> Result<(i64, i32, i32), KubeVirtObservationStoreError> {
    Ok((
        as_i64(fence.environment_generation)?,
        i32::try_from(fence.attempt)
            .map_err(|_| KubeVirtObservationStoreError::InvalidObservation)?,
        i32::try_from(fence.provider_step)
            .map_err(|_| KubeVirtObservationStoreError::InvalidObservation)?,
    ))
}

fn as_i64(value: u64) -> Result<i64, KubeVirtObservationStoreError> {
    i64::try_from(value).map_err(|_| KubeVirtObservationStoreError::InvalidObservation)
}

#[derive(Debug, thiserror::Error)]
pub enum KubeVirtObservationStoreError {
    #[error("LW_ENVIRONMENT_KUBEVIRT_OBSERVATION_INVALID")]
    InvalidObservation,
    #[error("LW_ENVIRONMENT_KUBEVIRT_OBSERVATION_IDENTITY_MISMATCH")]
    IdentityMismatch,
    #[error("LW_ENVIRONMENT_KUBEVIRT_FENCE_STALE")]
    StaleFence,
    #[error("LW_ENVIRONMENT_KUBEVIRT_TOMBSTONED")]
    Tombstoned,
    #[error("LW_ENVIRONMENT_KUBEVIRT_OBSERVATION_DATABASE_FAILED")]
    Database(#[from] sqlx::Error),
}

/// `KubeVirt` implementation of the frozen Environment Provider state machine.
pub struct KubeVirtProvider<B, R, S> {
    binding: String,
    backend: Arc<B>,
    releases: Arc<R>,
    observations: Arc<S>,
    configuration: KubeVirtProviderConfiguration,
}

impl<B, R, S> KubeVirtProvider<B, R, S>
where
    B: KubeVirtProviderBackend,
    R: ContainerReleaseResolver,
    S: KubeVirtObservationStore,
{
    pub fn new(
        binding: String,
        backend: Arc<B>,
        releases: Arc<R>,
        observations: Arc<S>,
        configuration: KubeVirtProviderConfiguration,
    ) -> Result<Self, ReleaseProjectionError> {
        if !valid_binding(&binding) {
            return Err(ReleaseProjectionError::ConfigurationInvalid);
        }
        Ok(Self {
            binding,
            backend,
            releases,
            observations,
            configuration,
        })
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the complete deterministic KubeVirt security bundle is reviewed as one projection"
    )]
    pub fn plan(
        &self,
        instance: &EnvironmentInstance,
        resolved: &ResolvedContainerRelease,
        action: ReconcileAction,
    ) -> Result<KubeVirtResourcePlan, ReleaseProjectionError> {
        let projection = &resolved.projection;
        projection
            .validate()
            .map_err(|_| ReleaseProjectionError::ContractInvalid)?;
        self.validate_release_use(resolved, action)?;
        if instance.runtime_kind != RuntimeKind::VirtualMachine
            || instance.release_id != projection.release.id
            || instance.release_version != projection.release.version
            || instance.course_id != projection.release.course_id
            || instance.provider_binding != self.binding
        {
            return Err(ReleaseProjectionError::IdentityMismatch);
        }
        let EnvironmentRuntimeSpec::VirtualMachine {
            provider_binding,
            base_disk: spec_base_disk,
            storage_class_binding,
            ssh_port,
        } = &projection.environment_spec.runtime
        else {
            return Err(ReleaseProjectionError::IdentityMismatch);
        };
        let ImageArtifact::VirtualMachine {
            base_disk, format, ..
        } = &projection.release.artifact
        else {
            return Err(ReleaseProjectionError::IdentityMismatch);
        };
        if provider_binding != &self.binding
            || storage_class_binding != &self.configuration.storage.binding
            || *ssh_port != 22
            || spec_base_disk != base_disk
            || projection.release.image_policy_evaluation.artifact_sha256 != base_disk.sha256
            || projection.environment_spec.security.user_policy
                != RuntimeUserPolicy::NonRootRequired
            || projection.environment_spec.security.root_filesystem_policy
                != RootFilesystemPolicy::MutableRequired
            || projection
                .environment_spec
                .security
                .privilege_escalation_policy
                != PrivilegeEscalationPolicy::Deny
            || projection.environment_spec.security.public_exposure_policy
                != PublicExposurePolicy::Deny
            || !valid_artifact_ref(base_disk)
        {
            return Err(ReleaseProjectionError::SecurityPostureInvalid);
        }
        let [entry] = projection.environment_spec.entries.as_slice() else {
            return Err(ReleaseProjectionError::SecurityPostureInvalid);
        };
        if entry.protocol != contracts::environment::EndpointProtocol::Ssh
            || entry.service_port != *ssh_port
        {
            return Err(ReleaseProjectionError::SecurityPostureInvalid);
        }

        let namespace = format!("lw-env-{}", instance.id);
        let virtual_machine_name = "runtime".to_owned();
        let data_volume_name = "rootdisk".to_owned();
        let labels = json!({
            "app.kubernetes.io/name": "labweaver-vm-runtime",
            "labweaver.io/environment-id": instance.id.to_string(),
            "labweaver.io/course-id": instance.course_id.to_string(),
            "labweaver.io/managed": "true",
        });
        let annotations = json!({
            "labweaver.io/release-id": projection.release.id.to_string(),
            "labweaver.io/release-version": projection.release.version.to_string(),
            "labweaver.io/base-disk-store-binding": base_disk.store_binding,
            "labweaver.io/base-disk-object-version": base_disk.object_version,
            "labweaver.io/base-disk-sha256": base_disk.sha256.to_string(),
            "labweaver.io/environment-generation": instance.generation.to_string(),
        });
        let resources = &projection.environment_spec.resources;
        let cpu = format!("{}m", resources.cpu_millicores);
        let memory = resources.memory_bytes.to_string();
        let storage = resources.storage_bytes.to_string();
        let budget = self.configuration.resource_budget;
        if resources.storage_bytes > budget.cdi_scratch_storage_bytes {
            return Err(ReleaseProjectionError::SecurityPostureInvalid);
        }
        let quota_cpu_request_millicores = resources
            .cpu_millicores
            .checked_add(budget.cdi_importer_cpu_request_millicores)
            .ok_or(ReleaseProjectionError::SecurityPostureInvalid)?;
        let quota_cpu_limit_millicores = resources
            .cpu_millicores
            .checked_add(budget.cdi_importer_cpu_limit_millicores)
            .ok_or(ReleaseProjectionError::SecurityPostureInvalid)?;
        let vmi_memory_limit_bytes = resources
            .memory_bytes
            .checked_add(budget.vmi_memory_overhead_bytes)
            .ok_or(ReleaseProjectionError::SecurityPostureInvalid)?;
        let quota_memory_request_bytes = vmi_memory_limit_bytes
            .checked_add(budget.cdi_importer_memory_request_bytes)
            .ok_or(ReleaseProjectionError::SecurityPostureInvalid)?;
        let quota_memory_limit_bytes = vmi_memory_limit_bytes
            .checked_add(budget.cdi_importer_memory_limit_bytes)
            .ok_or(ReleaseProjectionError::SecurityPostureInvalid)?;
        let quota_storage_bytes = resources
            .storage_bytes
            .checked_add(budget.cdi_scratch_storage_bytes)
            .ok_or(ReleaseProjectionError::SecurityPostureInvalid)?;
        let quota_cpu_request = format!("{quota_cpu_request_millicores}m");
        let quota_cpu_limit = format!("{quota_cpu_limit_millicores}m");
        let quota_memory_request = quota_memory_request_bytes.to_string();
        let quota_memory_limit = quota_memory_limit_bytes.to_string();
        let quota_storage = quota_storage_bytes.to_string();
        let vmi_memory_limit = vmi_memory_limit_bytes.to_string();
        let quota_annotations = json!({
            "labweaver.io/vmi-memory-overhead-bytes": budget.vmi_memory_overhead_bytes.to_string(),
            "labweaver.io/cdi-importer-cpu-request-millicores": budget.cdi_importer_cpu_request_millicores.to_string(),
            "labweaver.io/cdi-importer-cpu-limit-millicores": budget.cdi_importer_cpu_limit_millicores.to_string(),
            "labweaver.io/cdi-importer-memory-request-bytes": budget.cdi_importer_memory_request_bytes.to_string(),
            "labweaver.io/cdi-importer-memory-limit-bytes": budget.cdi_importer_memory_limit_bytes.to_string(),
            "labweaver.io/cdi-scratch-storage-bytes": budget.cdi_scratch_storage_bytes.to_string(),
        });
        let cloud_init = self.cloud_init_user_data();
        let cloud_init_data = BASE64_STANDARD.encode(cloud_init.as_bytes());
        let mut documents = vec![
            resource(
                "Namespace",
                None,
                &namespace,
                json!({
                    "apiVersion":"v1","kind":"Namespace",
                    "metadata":{"name":namespace,"labels":labels,"finalizers":["labweaver.io/environment-cleanup"]}
                }),
            ),
            resource(
                "ResourceQuota",
                Some(&namespace),
                "runtime-quota",
                json!({
                    "apiVersion":"v1","kind":"ResourceQuota",
                    "metadata":{"name":"runtime-quota","namespace":namespace,"labels":labels,"annotations":quota_annotations},
                    "spec":{"hard":{"requests.cpu":quota_cpu_request,"limits.cpu":quota_cpu_limit,"requests.memory":quota_memory_request,"limits.memory":quota_memory_limit,"requests.storage":quota_storage,"persistentvolumeclaims":"2","pods":"2"}}
                }),
            ),
            resource(
                "NetworkPolicy",
                Some(&namespace),
                "default-deny",
                json!({
                    "apiVersion":"networking.k8s.io/v1","kind":"NetworkPolicy",
                    "metadata":{"name":"default-deny","namespace":namespace,"labels":labels},
                    "spec":{"podSelector":{},"policyTypes":["Ingress","Egress"]}
                }),
            ),
            resource(
                "NetworkPolicy",
                Some(&namespace),
                "openssh-gateway-ingress",
                json!({
                    "apiVersion":"networking.k8s.io/v1","kind":"NetworkPolicy",
                    "metadata":{"name":"openssh-gateway-ingress","namespace":namespace,"labels":labels},
                    "spec":{"podSelector":{"matchLabels":{"labweaver.io/environment-id":instance.id.to_string()}},"policyTypes":["Ingress"],"ingress":[{"from":[{"namespaceSelector":{"matchLabels":{"kubernetes.io/metadata.name":self.configuration.ssh.gateway_namespace}},"podSelector":{"matchLabels":{GATEWAY_LABEL_KEY:self.configuration.ssh.gateway_pod_label}}}],"ports":[{"protocol":"TCP","port":ssh_port}]}]}
                }),
            ),
            resource(
                "Secret",
                Some(&namespace),
                "cloud-init",
                json!({
                    "apiVersion":"v1","kind":"Secret","type":"Opaque",
                    "metadata":{"name":"cloud-init","namespace":namespace,"labels":labels,"annotations":annotations},
                    "data":{"userdata":cloud_init_data}
                }),
            ),
            resource(
                "DataVolume",
                Some(&namespace),
                &data_volume_name,
                json!({
                    "apiVersion":"cdi.kubevirt.io/v1beta1","kind":"DataVolume",
                    "metadata":{"name":data_volume_name,"namespace":namespace,"labels":labels,"annotations":annotations},
                    "spec":{
                        "sourceRef":{"kind":"DataSource","namespace":self.configuration.storage.data_source_namespace,"name":self.configuration.storage.data_source_name},
                        "storage":{"storageClassName":self.configuration.storage.storage_class_name,"accessModes":["ReadWriteOnce"],"resources":{"requests":{"storage":storage}}}
                    }
                }),
            ),
            resource(
                "VirtualMachine",
                Some(&namespace),
                &virtual_machine_name,
                json!({
                    "apiVersion":"kubevirt.io/v1","kind":"VirtualMachine",
                    "metadata":{"name":virtual_machine_name,"namespace":namespace,"labels":labels,"annotations":annotations},
                    "spec":{
                        "runStrategy":"Always",
                        "template":{
                            "metadata":{"labels":{"app":"runtime","labweaver.io/environment-id":instance.id.to_string()}},
                            "spec":{
                                "terminationGracePeriodSeconds":30,
                                "nodeSelector":{KUBEVIRT_NODE_LABEL_KEY:KUBEVIRT_NODE_LABEL_VALUE},
                                "domain":{
                                    "resources":{"requests":{"cpu":cpu,"memory":memory},"limits":{"cpu":cpu,"memory":vmi_memory_limit}},
                                    "devices":{
                                        "autoattachGraphicsDevice":false,
                                        "autoattachSerialConsole":true,
                                        "disks":[
                                            {"name":"rootdisk","disk":{"bus":"virtio"},"bootOrder":1},
                                            {"name":"cloudinit","disk":{"bus":"virtio"}}
                                        ],
                                        "interfaces":[{"name":"default","masquerade":{}}]
                                    }
                                },
                                "networks":[{"name":"default","pod":{}}],
                                "volumes":[
                                    {"name":"rootdisk","persistentVolumeClaim":{"claimName":data_volume_name}},
                                    {"name":"cloudinit","cloudInitNoCloud":{"secretRef":{"name":"cloud-init"}}}
                                ],
                                "readinessProbe":{"tcpSocket":{"port":ssh_port},"periodSeconds":2,"failureThreshold":60}
                            }
                        }
                    }
                }),
            ),
            resource(
                "Service",
                Some(&namespace),
                "ssh",
                json!({
                    "apiVersion":"v1","kind":"Service",
                    "metadata":{"name":"ssh","namespace":namespace,"labels":labels,"annotations":{"labweaver.io/access-controlled":"true"}},
                    "spec":{"type":"ClusterIP","selector":{"labweaver.io/environment-id":instance.id.to_string()},"ports":[{"name":"ssh","protocol":"TCP","port":ssh_port,"targetPort":ssh_port}]}
                }),
            ),
        ];
        if let NetworkPolicySpec::Restricted { policy_binding } =
            &projection.environment_spec.network
        {
            documents.push(resource(
                "NetworkPolicy",
                Some(&namespace),
                "restricted-egress",
                json!({
                    "apiVersion":"networking.k8s.io/v1","kind":"NetworkPolicy",
                    "metadata":{"name":"restricted-egress","namespace":namespace,"labels":labels},
                    "spec":{"podSelector":{"matchLabels":{"labweaver.io/environment-id":instance.id.to_string()}},"policyTypes":["Egress"],"egress":[{"to":[{"namespaceSelector":{"matchLabels":{"labweaver.io/egress-policy":policy_binding}}}]}]}
                }),
            ));
        }
        let plan_sha256 = canonical_hash(&json!({
            "environmentId": instance.id,
            "releaseId": projection.release.id,
            "releaseVersion": projection.release.version,
            "baseDisk": base_disk,
            "format": format,
            "storageClassName": self.configuration.storage.storage_class_name,
            "resources": documents,
        }))?;
        Ok(KubeVirtResourcePlan {
            environment_id: instance.id,
            namespace,
            virtual_machine_name,
            data_volume_name,
            base_disk: base_disk.clone(),
            base_disk_format: *format,
            storage_class_name: self.configuration.storage.storage_class_name.clone(),
            resources: documents,
            plan_sha256,
        })
    }

    fn validate_release_use(
        &self,
        resolved: &ResolvedContainerRelease,
        action: ReconcileAction,
    ) -> Result<(), ReleaseProjectionError> {
        if action == ReconcileAction::Stop {
            return Ok(());
        }
        let release = &resolved.projection.release;
        if resolved.withdrawn_at.is_some() {
            return Err(ReleaseProjectionError::Withdrawn);
        }
        if resolved.authority_now >= release.image_policy_evaluation.valid_until {
            return Err(ReleaseProjectionError::EvidenceExpired);
        }
        let policy = self.configuration.release_policy;
        if release.image_policy_evaluation.policy_id != policy.image_policy_id
            || release.image_policy_evaluation.policy_revision != policy.image_policy_revision
            || release.approval.trust_revision != policy.trust_revision
        {
            return Err(ReleaseProjectionError::TrustRevisionMismatch);
        }
        Ok(())
    }

    fn cloud_init_user_data(&self) -> String {
        format!(
            "#cloud-config\nusers:\n  - name: {user}\n    lock_passwd: true\n    shell: /bin/bash\nwrite_files:\n  - path: /etc/ssh/labweaver_user_ca.pub\n    owner: root:root\n    permissions: '0644'\n    content: |\n      {ca}\n  - path: /etc/ssh/auth_principals/{user}\n    owner: root:root\n    permissions: '0644'\n    content: |\n      labweaver-gateway\n      labweaver-collector\n  - path: /etc/ssh/sshd_config.d/99-labweaver.conf\n    owner: root:root\n    permissions: '0644'\n    content: |\n      TrustedUserCAKeys /etc/ssh/labweaver_user_ca.pub\n      AuthorizedPrincipalsFile /etc/ssh/auth_principals/%u\n      AuthorizedKeysFile none\n      PubkeyAuthentication yes\n      AuthenticationMethods publickey\n      AllowUsers {user}\n      PasswordAuthentication no\n      KbdInteractiveAuthentication no\n      PermitRootLogin no\n      AllowTcpForwarding no\n      PermitAgentForwarding no\n      PermitTunnel no\n      X11Forwarding no\nruncmd:\n  - [systemctl, reload, ssh]\n",
            user = self.configuration.ssh.guest_user,
            ca = self.configuration.ssh.user_ca_public_key,
        )
    }

    fn cleanup_plan(
        &self,
        instance: &EnvironmentInstance,
    ) -> Result<KubeVirtCleanupPlan, ReleaseProjectionError> {
        if instance.runtime_kind != RuntimeKind::VirtualMachine
            || instance.provider_binding != self.binding
        {
            return Err(ReleaseProjectionError::IdentityMismatch);
        }
        let namespace = format!("lw-env-{}", instance.id);
        let virtual_machine_name = "runtime".to_owned();
        let plan_sha256 = canonical_hash(&json!({
            "environmentId": instance.id,
            "namespace": namespace,
            "virtualMachineName": virtual_machine_name,
            "action": "cleanup",
        }))?;
        Ok(KubeVirtCleanupPlan {
            environment_id: instance.id,
            namespace,
            virtual_machine_name,
            plan_sha256,
        })
    }
}

#[async_trait]
impl<B, R, S> EnvironmentProvider for KubeVirtProvider<B, R, S>
where
    B: KubeVirtProviderBackend,
    R: ContainerReleaseResolver,
    S: KubeVirtObservationStore,
{
    fn binding(&self) -> &str {
        &self.binding
    }

    async fn execute(
        &self,
        action: ReconcileAction,
        instance: &EnvironmentInstance,
    ) -> Result<ProviderObservation, ProviderFailure> {
        let fence = KubeVirtBackendFence::for_action(instance, action)?;
        if action == ReconcileAction::Cleanup
            && instance.observed_state == ObservedEnvironmentState::Deleting
        {
            let plan = self
                .cleanup_plan(instance)
                .map_err(|error| projection_failure(&error))?;
            let cleanup_evidence = self.backend.delete_namespace(&fence, &plan).await?;
            if !valid_artifact_ref(&cleanup_evidence) {
                return Err(ProviderFailure {
                    code: ProviderFailureCode::CleanupFailed,
                    retryable: true,
                });
            }
            self.observations
                .record_deleted(&fence, &plan, &cleanup_evidence)
                .await
                .map_err(|error| observation_store_failure(&error))?;
            return Ok(ProviderObservation {
                next_state: ObservedEnvironmentState::Deleted,
                endpoints: Vec::new(),
                cleanup_evidence: Some(cleanup_evidence),
                operation_complete: true,
            });
        }
        let resolved = self
            .releases
            .resolve(instance.release_id, instance.release_version)
            .await
            .map_err(|error| projection_failure(&error))?;
        let plan = self
            .plan(instance, &resolved, action)
            .map_err(|error| projection_failure(&error))?;
        let no_endpoints = |next_state, operation_complete| ProviderObservation {
            next_state,
            endpoints: Vec::new(),
            cleanup_evidence: None,
            operation_complete,
        };
        match (action, instance.observed_state) {
            (ReconcileAction::Validate, ObservedEnvironmentState::Requested) => {
                Ok(no_endpoints(ObservedEnvironmentState::Validating, false))
            }
            (ReconcileAction::Validate, ObservedEnvironmentState::Validating) => {
                Ok(no_endpoints(ObservedEnvironmentState::Building, false))
            }
            (ReconcileAction::Build, ObservedEnvironmentState::Building) => {
                Ok(no_endpoints(ObservedEnvironmentState::Provisioning, false))
            }
            (
                ReconcileAction::Provision | ReconcileAction::Reset,
                ObservedEnvironmentState::Provisioning,
            ) => {
                let observed = self.backend.apply(&fence, &plan).await?;
                self.accept_running_observation(&fence, &plan, instance, observed)
                    .await
            }
            (ReconcileAction::Observe, _) => {
                let observed = self.backend.observe(&fence, &plan).await?;
                self.accept_running_observation(&fence, &plan, instance, observed)
                    .await
            }
            (ReconcileAction::Start, ObservedEnvironmentState::Stopped) => {
                let observed = self.backend.start(&fence, &plan).await?;
                self.accept_running_observation(&fence, &plan, instance, observed)
                    .await
            }
            (ReconcileAction::Restart, ObservedEnvironmentState::Provisioning) => {
                let observed = self.backend.restart(&fence, &plan).await?;
                self.accept_running_observation(&fence, &plan, instance, observed)
                    .await
            }
            (
                ReconcileAction::Stop,
                ObservedEnvironmentState::Stopping | ObservedEnvironmentState::Expiring,
            ) => {
                let observed = self.backend.stop(&fence, &plan).await?;
                validate_stopped_observation(instance, observed)?;
                self.observations
                    .record_stopped(&fence, &plan, &observed)
                    .await
                    .map_err(|error| observation_store_failure(&error))?;
                Ok(no_endpoints(ObservedEnvironmentState::Stopped, true))
            }
            _ => Err(ProviderFailure {
                code: ProviderFailureCode::Rejected,
                retryable: false,
            }),
        }
    }
}

impl<B, R, S> KubeVirtProvider<B, R, S>
where
    B: KubeVirtProviderBackend,
    R: ContainerReleaseResolver,
    S: KubeVirtObservationStore,
{
    async fn accept_running_observation(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtResourcePlan,
        instance: &EnvironmentInstance,
        observed: KubeVirtRunningObservation,
    ) -> Result<ProviderObservation, ProviderFailure> {
        if running_observation_ready(instance, &observed) {
            self.observations
                .record_running(fence, plan, &observed)
                .await
                .map_err(|error| observation_store_failure(&error))?;
        }
        ready_observation(instance, observed)
    }
}

fn ready_observation(
    instance: &EnvironmentInstance,
    observed: KubeVirtRunningObservation,
) -> Result<ProviderObservation, ProviderFailure> {
    if !running_observation_ready(instance, &observed) {
        return Ok(ProviderObservation {
            next_state: ObservedEnvironmentState::Provisioning,
            endpoints: Vec::new(),
            cleanup_evidence: None,
            operation_complete: false,
        });
    }
    let revision = next_revision(instance.revision)?;
    Ok(ProviderObservation {
        next_state: ObservedEnvironmentState::Ready,
        endpoints: vec![EnvironmentEndpoint {
            id: deterministic_endpoint_id(instance.id)?,
            protocol: contracts::environment::EndpointProtocol::Ssh,
            revision,
            health: EndpointHealth::Healthy,
            observed_at: observed.observed_at,
        }],
        cleanup_evidence: None,
        operation_complete: true,
    })
}

fn running_observation_ready(
    instance: &EnvironmentInstance,
    observed: &KubeVirtRunningObservation,
) -> bool {
    valid_running_observation(instance.generation, observed)
}

fn valid_running_observation(
    environment_generation: u64,
    observed: &KubeVirtRunningObservation,
) -> bool {
    observed.observed_environment_generation == environment_generation
        && observed.vm_resource_generation > 0
        && observed.observed_vm_resource_generation >= observed.vm_resource_generation
        && !observed.vm_uid.is_nil()
        && !observed.vmi_uid.is_nil()
        && !observed.root_disk_uid.is_nil()
        && private_route_ip(observed.guest_ip)
        && private_route_ip(observed.service_cluster_ip)
        && observed.ssh_host_key_sha256 != Sha256Digest::of_bytes(&[])
        && observed.guest_agent_connected
        && observed.ssh_ready
}

fn observation_store_failure(error: &KubeVirtObservationStoreError) -> ProviderFailure {
    match error {
        KubeVirtObservationStoreError::Database(_) => unavailable(),
        KubeVirtObservationStoreError::InvalidObservation
        | KubeVirtObservationStoreError::IdentityMismatch
        | KubeVirtObservationStoreError::StaleFence
        | KubeVirtObservationStoreError::Tombstoned => invalid_observation(),
    }
}

fn validate_stopped_observation(
    instance: &EnvironmentInstance,
    observed: KubeVirtStoppedObservation,
) -> Result<(), ProviderFailure> {
    if !valid_stopped_observation(instance.generation, &observed) {
        return Err(invalid_observation());
    }
    Ok(())
}

fn valid_stopped_observation(
    environment_generation: u64,
    observed: &KubeVirtStoppedObservation,
) -> bool {
    observed.observed_environment_generation == environment_generation
        && !observed.vm_uid.is_nil()
        && !observed.root_disk_uid.is_nil()
        && observed.vmi_absent
}

fn next_revision(revision: Revision) -> Result<Revision, ProviderFailure> {
    revision
        .get()
        .checked_add(1)
        .and_then(|value| Revision::new(value).ok())
        .ok_or_else(invalid_observation)
}

fn deterministic_endpoint_id(environment_id: EnvironmentId) -> Result<EndpointId, ProviderFailure> {
    let mut bytes = *environment_id.as_uuid().as_bytes();
    bytes[15] ^= 2;
    EndpointId::from_str(&Uuid::from_bytes(bytes).to_string()).map_err(|_| invalid_observation())
}

fn private_route_ip(value: IpAddr) -> bool {
    match value {
        IpAddr::V4(value) => value.is_private(),
        IpAddr::V6(value) => value.is_unique_local(),
    }
}

fn resource(kind: &str, namespace: Option<&str>, name: &str, document: Value) -> KubeVirtResource {
    KubeVirtResource {
        kind: kind.to_owned(),
        namespace: namespace.map(str::to_owned),
        name: name.to_owned(),
        document,
    }
}

fn canonical_hash<T: Serialize>(value: &T) -> Result<Sha256Digest, ReleaseProjectionError> {
    Sha256Digest::of_canonical(value).map_err(|_| ReleaseProjectionError::ContractInvalid)
}

fn projection_failure(error: &ReleaseProjectionError) -> ProviderFailure {
    match error {
        ReleaseProjectionError::Database(_)
        | ReleaseProjectionError::PersistenceFailed
        | ReleaseProjectionError::NotFound => unavailable(),
        ReleaseProjectionError::ConfigurationInvalid
        | ReleaseProjectionError::ContractInvalid
        | ReleaseProjectionError::IdentityMismatch
        | ReleaseProjectionError::SecurityPostureInvalid
        | ReleaseProjectionError::Withdrawn
        | ReleaseProjectionError::EvidenceExpired
        | ReleaseProjectionError::TrustRevisionMismatch => ProviderFailure {
            code: ProviderFailureCode::Rejected,
            retryable: false,
        },
    }
}

fn valid_binding(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
}

fn valid_subject(value: &str) -> bool {
    valid_binding(value) && !value.contains('*') && !value.contains('>')
}

fn valid_dns_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && !value.starts_with('-')
        && !value.ends_with('-')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn valid_guest_user(value: &str) -> bool {
    valid_dns_label(value)
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase())
}

fn valid_artifact_ref(artifact: &ArtifactRef) -> bool {
    artifact.size_bytes > 0
        && artifact.sha256 != Sha256Digest::of_bytes(&[])
        && !artifact.store_binding.trim().is_empty()
        && !artifact.object_version.trim().is_empty()
        && !artifact.media_type.trim().is_empty()
        && !artifact
            .store_binding
            .bytes()
            .any(|byte| byte.is_ascii_whitespace())
}

const fn unavailable() -> ProviderFailure {
    ProviderFailure {
        code: ProviderFailureCode::Unavailable,
        retryable: true,
    }
}

const fn invalid_observation() -> ProviderFailure {
    ProviderFailure {
        code: ProviderFailureCode::ObservationInvalid,
        retryable: false,
    }
}

const fn configuration_invalid() -> ProviderFailure {
    ProviderFailure {
        code: ProviderFailureCode::Rejected,
        retryable: false,
    }
}
