use std::net::IpAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

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
use contracts::supply_chain::{ImageArtifact, VirtualMachineBaseDisk, VirtualMachineDiskFormat};
use contracts::{
    ArtifactRef, EndpointId, EnvironmentId, OperationId, Revision, Sha256Digest, UtcTimestamp,
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::{
    ContainerReleaseResolver, EnvironmentProvider, ProviderFailure, ProviderFailureCode,
    ProviderObservation, ReconcileAction, ReleaseProjectionError, ResolvedContainerRelease,
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

/// Immutable VM resource plan bound to one deployment-owned CDI source and imported disk hash.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KubeVirtResourcePlan {
    pub environment_id: EnvironmentId,
    pub namespace: String,
    pub virtual_machine_name: String,
    pub data_volume_name: String,
    pub base_disk: VirtualMachineBaseDisk,
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
    request_timeout: Duration,
}

impl NatsKubeVirtProviderBackend {
    pub fn new(
        client: async_nats::Client,
        subject: String,
        request_timeout: Duration,
    ) -> Result<Self, ProviderFailure> {
        if !valid_subject(&subject)
            || request_timeout.is_zero()
            || request_timeout > Duration::from_secs(300)
        {
            return Err(configuration_invalid());
        }
        Ok(Self {
            client,
            subject,
            request_timeout,
        })
    }

    async fn request(
        &self,
        fence: &KubeVirtBackendFence,
        request: KubeVirtExecutorRequest,
    ) -> Result<KubeVirtExecutorResponse, ProviderFailure> {
        if !request.matches_action(fence.action) {
            return Err(invalid_observation());
        }
        let fence = bind_kubevirt_executor_request(*fence, &request)?;
        let payload = serde_json::to_vec(&KubeVirtExecutorRequestEnvelope { fence, request })
            .map_err(|_| invalid_observation())?;
        let request = async_nats::Request::new()
            .timeout(Some(self.request_timeout))
            .payload(payload.into());
        let message = self
            .client
            .send_request(self.subject.clone(), request)
            .await
            .map_err(|error| {
                tracing::warn!(
                    event = "environment.kubevirt_provider.executor_request_failed",
                    diagnostic = "LW_ENVIRONMENT_PROVIDER_UNAVAILABLE",
                    environment_id = %fence.environment_id,
                    operation_id = %fence.operation_id,
                    provider_step = fence.provider_step,
                    attempt = fence.attempt,
                    action = ?fence.action,
                    timeout_milliseconds = self.request_timeout.as_millis(),
                    error = %error
                );
                unavailable()
            })?;
        if message.payload.len() > MAX_RESPONSE_BYTES {
            return Err(invalid_observation());
        }
        let response: KubeVirtExecutorResponseEnvelope =
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
                .request(fence, KubeVirtExecutorRequest::Apply { plan: plan.clone() })
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
                .request(
                    fence,
                    KubeVirtExecutorRequest::Observe { plan: plan.clone() },
                )
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
                .request(fence, KubeVirtExecutorRequest::Start { plan: plan.clone() })
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
            .request(fence, KubeVirtExecutorRequest::Stop { plan: plan.clone() })
            .await?
        {
            KubeVirtExecutorResponse::Stopped {
                plan_sha256,
                observation,
            } if plan_sha256 == plan.plan_sha256 => Ok(observation),
            KubeVirtExecutorResponse::Failed { failure } => Err(failure),
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
                .request(
                    fence,
                    KubeVirtExecutorRequest::Restart { plan: plan.clone() },
                )
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
            .request(
                fence,
                KubeVirtExecutorRequest::DeleteNamespace { plan: plan.clone() },
            )
            .await?
        {
            KubeVirtExecutorResponse::Deleted {
                plan_sha256,
                cleanup_evidence,
            } if plan_sha256 == plan.plan_sha256 && valid_artifact_ref(&cleanup_evidence) => {
                Ok(cleanup_evidence)
            }
            KubeVirtExecutorResponse::Failed { failure } => Err(failure),
            _ => Err(ProviderFailure {
                code: ProviderFailureCode::CleanupFailed,
                retryable: true,
            }),
        }
    }
}

fn running_response(
    response: &KubeVirtExecutorResponse,
    plan: &KubeVirtResourcePlan,
) -> Result<KubeVirtRunningObservation, ProviderFailure> {
    match response {
        KubeVirtExecutorResponse::Running {
            plan_sha256,
            observation,
        } if *plan_sha256 == plan.plan_sha256 => Ok(*observation),
        KubeVirtExecutorResponse::Failed { failure } => Err(*failure),
        _ => Err(invalid_observation()),
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KubeVirtExecutorRequestEnvelope {
    #[serde(flatten)]
    pub fence: KubeVirtBackendFence,
    pub request: KubeVirtExecutorRequest,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(
    tag = "backendAction",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum KubeVirtExecutorRequest {
    Apply { plan: KubeVirtResourcePlan },
    Observe { plan: KubeVirtResourcePlan },
    Start { plan: KubeVirtResourcePlan },
    Stop { plan: KubeVirtResourcePlan },
    Restart { plan: KubeVirtResourcePlan },
    DeleteNamespace { plan: KubeVirtCleanupPlan },
}

impl KubeVirtExecutorRequest {
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

fn bind_kubevirt_executor_request(
    mut fence: KubeVirtBackendFence,
    request: &KubeVirtExecutorRequest,
) -> Result<KubeVirtBackendFence, ProviderFailure> {
    fence.request_id = kubevirt_executor_request_id(fence, request)?;
    Ok(fence)
}

fn kubevirt_executor_request_id(
    fence: KubeVirtBackendFence,
    request: &KubeVirtExecutorRequest,
) -> Result<Sha256Digest, ProviderFailure> {
    Sha256Digest::of_canonical(&json!({
        "protocolVersion": fence.protocol_version,
        "environmentId": fence.environment_id,
        "operationId": fence.operation_id,
        "providerStep": fence.provider_step,
        "environmentGeneration": fence.environment_generation,
        "attempt": fence.attempt,
        "action": fence.action,
        "deadlineAt": fence.deadline_at,
        "request": request,
    }))
    .map_err(|_| invalid_observation())
}

const fn kubevirt_executor_environment_id(request: &KubeVirtExecutorRequest) -> EnvironmentId {
    match request {
        KubeVirtExecutorRequest::Apply { plan }
        | KubeVirtExecutorRequest::Observe { plan }
        | KubeVirtExecutorRequest::Start { plan }
        | KubeVirtExecutorRequest::Stop { plan }
        | KubeVirtExecutorRequest::Restart { plan } => plan.environment_id,
        KubeVirtExecutorRequest::DeleteNamespace { plan } => plan.environment_id,
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KubeVirtExecutorResponseEnvelope {
    pub protocol_version: u8,
    pub environment_id: EnvironmentId,
    pub operation_id: OperationId,
    pub provider_step: u32,
    pub environment_generation: u64,
    pub attempt: u32,
    pub action: ReconcileAction,
    pub request_id: Sha256Digest,
    pub response: KubeVirtExecutorResponse,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum KubeVirtExecutorResponse {
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

/// `KubeVirt` side-effect adapter invoked only after durable executor admission.
#[async_trait]
pub trait KubeVirtExecutorBackend: Send + Sync {
    async fn execute(
        &self,
        fence: &KubeVirtBackendFence,
        request: &KubeVirtExecutorRequest,
    ) -> KubeVirtExecutorResponse;
}

/// Persistent highest-generation and permanent-cleanup ledger for VM operations.
#[derive(Clone, Debug)]
pub struct PgKubeVirtExecutorFenceStore {
    pool: PgPool,
}

impl PgKubeVirtExecutorFenceStore {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the row lock, generation fence and cleanup tombstone form one admission decision"
    )]
    async fn admit(
        &self,
        envelope: &KubeVirtExecutorRequestEnvelope,
    ) -> Result<KubeVirtExecutorAdmission, KubeVirtExecutorFenceError> {
        validate_kubevirt_executor_request(envelope)?;
        let fence = envelope.fence;
        let mut transaction = self.pool.begin().await?;
        let authority_now: time::OffsetDateTime =
            sqlx::query_scalar("SELECT date_trunc('milliseconds',clock_timestamp())")
                .fetch_one(&mut *transaction)
                .await?;
        if authority_now >= fence.deadline_at.get() {
            return Err(KubeVirtExecutorFenceError::DeadlineExceeded);
        }
        let remaining = std::time::Duration::try_from(fence.deadline_at.get() - authority_now)
            .map_err(|_| KubeVirtExecutorFenceError::DeadlineExceeded)?;
        let current = sqlx::query(
            "SELECT highest_generation,operation_id,provider_step,attempt,tombstoned, \
                    last_request_id,last_response,deadline_at \
             FROM environment.kubevirt_executor_fences WHERE environment_id=$1 FOR UPDATE",
        )
        .bind(fence.environment_id.as_uuid())
        .fetch_optional(&mut *transaction)
        .await?;
        if let Some(row) = current {
            let highest_generation = u64::try_from(row.try_get::<i64, _>("highest_generation")?)
                .map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?;
            let operation_id =
                OperationId::from_str(&row.try_get::<Uuid, _>("operation_id")?.to_string())
                    .map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?;
            let provider_step = u32::try_from(row.try_get::<i32, _>("provider_step")?)
                .map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?;
            let attempt = u32::try_from(row.try_get::<i32, _>("attempt")?)
                .map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?;
            let tombstoned: bool = row.try_get("tombstoned")?;
            let last_request_id: String = row.try_get("last_request_id")?;
            let last_response = row.try_get::<Option<Value>, _>("last_response")?;
            let previous_deadline: time::OffsetDateTime = row.try_get("deadline_at")?;
            if last_request_id == fence.request_id.to_string() {
                if let Some(value) = last_response {
                    transaction.rollback().await?;
                    return Ok(KubeVirtExecutorAdmission::Replay(value));
                }
                return Err(KubeVirtExecutorFenceError::InProgress);
            }
            if last_response.is_none() && authority_now < previous_deadline {
                return Err(KubeVirtExecutorFenceError::InProgress);
            }
            let cleanup_succeeded = last_response
                .as_ref()
                .and_then(|value| value.get("status"))
                .and_then(Value::as_str)
                == Some("deleted");
            if tombstoned && cleanup_succeeded {
                if fence.action == ReconcileAction::Cleanup {
                    let value = last_response
                        .as_ref()
                        .ok_or(KubeVirtExecutorFenceError::IdentityMismatch)?
                        .clone();
                    transaction.rollback().await?;
                    return Ok(KubeVirtExecutorAdmission::Replay(value));
                }
                return Err(KubeVirtExecutorFenceError::Tombstoned);
            }
            if fence.environment_generation < highest_generation
                || (fence.environment_generation == highest_generation
                    && (fence.provider_step < provider_step
                        || (fence.provider_step == provider_step && fence.attempt < attempt)))
            {
                return Err(KubeVirtExecutorFenceError::StaleGeneration);
            }
            if fence.environment_generation == highest_generation
                && fence.operation_id != operation_id
            {
                return Err(KubeVirtExecutorFenceError::IdentityMismatch);
            }
            sqlx::query(
                "UPDATE environment.kubevirt_executor_fences SET highest_generation=$2, \
                 operation_id=$3,provider_step=$4,attempt=$5,tombstoned=$6,last_action=$7, \
                 last_request_id=$8,last_response=NULL,deadline_at=$9,updated_at=clock_timestamp() \
                 WHERE environment_id=$1",
            )
            .bind(fence.environment_id.as_uuid())
            .bind(
                i64::try_from(fence.environment_generation)
                    .map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?,
            )
            .bind(fence.operation_id.as_uuid())
            .bind(
                i32::try_from(fence.provider_step)
                    .map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?,
            )
            .bind(
                i32::try_from(fence.attempt)
                    .map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?,
            )
            .bind(false)
            .bind(kubevirt_action_name(fence.action))
            .bind(fence.request_id.to_string())
            .bind(fence.deadline_at.get())
            .execute(&mut *transaction)
            .await?;
        } else {
            sqlx::query(
                "INSERT INTO environment.kubevirt_executor_fences \
                 (environment_id,highest_generation,operation_id,provider_step,attempt,tombstoned, \
                  last_action,last_request_id,deadline_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
            )
            .bind(fence.environment_id.as_uuid())
            .bind(
                i64::try_from(fence.environment_generation)
                    .map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?,
            )
            .bind(fence.operation_id.as_uuid())
            .bind(
                i32::try_from(fence.provider_step)
                    .map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?,
            )
            .bind(
                i32::try_from(fence.attempt)
                    .map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?,
            )
            .bind(false)
            .bind(kubevirt_action_name(fence.action))
            .bind(fence.request_id.to_string())
            .bind(fence.deadline_at.get())
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(KubeVirtExecutorAdmission::Execute(remaining))
    }

    async fn complete(
        &self,
        fence: KubeVirtBackendFence,
        response: &KubeVirtExecutorResponse,
    ) -> Result<(), KubeVirtExecutorFenceError> {
        let value = serde_json::to_value(response)
            .map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?;
        let updated = sqlx::query(
            "UPDATE environment.kubevirt_executor_fences SET last_response=$7, \
                 tombstoned=CASE WHEN $8 THEN TRUE ELSE tombstoned END,updated_at=clock_timestamp() \
             WHERE environment_id=$1 AND highest_generation=$2 AND operation_id=$3 \
               AND provider_step=$4 AND attempt=$5 AND last_request_id=$6 AND last_response IS NULL",
        )
        .bind(fence.environment_id.as_uuid())
        .bind(i64::try_from(fence.environment_generation).map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?)
        .bind(fence.operation_id.as_uuid())
        .bind(i32::try_from(fence.provider_step).map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?)
        .bind(i32::try_from(fence.attempt).map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?)
        .bind(fence.request_id.to_string())
        .bind(value)
        .bind(matches!(response, KubeVirtExecutorResponse::Deleted { .. }))
        .execute(&self.pool)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(KubeVirtExecutorFenceError::StaleGeneration);
        }
        Ok(())
    }
}

enum KubeVirtExecutorAdmission {
    Execute(std::time::Duration),
    Replay(Value),
}

/// Deadline-bounded executor with replay and stale-generation rejection.
pub struct FencedKubeVirtExecutor<B> {
    store: PgKubeVirtExecutorFenceStore,
    backend: B,
}

impl<B: KubeVirtExecutorBackend> FencedKubeVirtExecutor<B> {
    #[must_use]
    pub const fn new(store: PgKubeVirtExecutorFenceStore, backend: B) -> Self {
        Self { store, backend }
    }

    pub async fn execute(
        &self,
        envelope: KubeVirtExecutorRequestEnvelope,
    ) -> Result<KubeVirtExecutorResponseEnvelope, KubeVirtExecutorFenceError> {
        let response = match self.store.admit(&envelope).await? {
            KubeVirtExecutorAdmission::Execute(remaining) => {
                let response = tokio::time::timeout(
                    remaining,
                    self.backend.execute(&envelope.fence, &envelope.request),
                )
                .await
                .map_err(|_| KubeVirtExecutorFenceError::DeadlineExceeded)?;
                self.store.complete(envelope.fence, &response).await?;
                response
            }
            KubeVirtExecutorAdmission::Replay(value) => serde_json::from_value(value)
                .map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?,
        };
        Ok(KubeVirtExecutorResponseEnvelope {
            protocol_version: envelope.fence.protocol_version,
            environment_id: envelope.fence.environment_id,
            operation_id: envelope.fence.operation_id,
            provider_step: envelope.fence.provider_step,
            environment_generation: envelope.fence.environment_generation,
            attempt: envelope.fence.attempt,
            action: envelope.fence.action,
            request_id: envelope.fence.request_id,
            response,
        })
    }
}

/// Typed NATS request/reply server for the `KubeVirt` executor subject.
pub struct NatsKubeVirtExecutorServer<B> {
    client: async_nats::Client,
    subject: String,
    executor: Arc<FencedKubeVirtExecutor<B>>,
}

impl<B: KubeVirtExecutorBackend + 'static> NatsKubeVirtExecutorServer<B> {
    pub fn new(
        client: async_nats::Client,
        subject: String,
        executor: FencedKubeVirtExecutor<B>,
    ) -> Result<Self, KubeVirtExecutorFenceError> {
        if !valid_subject(&subject) {
            return Err(KubeVirtExecutorFenceError::ConfigurationInvalid);
        }
        Ok(Self {
            client,
            subject,
            executor: Arc::new(executor),
        })
    }

    pub async fn serve(self) -> Result<(), KubeVirtExecutorFenceError> {
        let mut subscriber = self
            .client
            .subscribe(self.subject)
            .await
            .map_err(|_| KubeVirtExecutorFenceError::Transport)?;
        while let Some(message) = subscriber.next().await {
            let Some(reply) = message.reply.clone() else {
                tracing::warn!(
                    event = "environment.kubevirt_executor.request_rejected",
                    diagnostic = "LW_ENVIRONMENT_KUBEVIRT_EXECUTOR_REPLY_REQUIRED"
                );
                continue;
            };
            if message.payload.len() > MAX_RESPONSE_BYTES {
                tracing::warn!(
                    event = "environment.kubevirt_executor.request_rejected",
                    diagnostic = "LW_ENVIRONMENT_KUBEVIRT_EXECUTOR_PAYLOAD_TOO_LARGE"
                );
                continue;
            }
            let Ok(envelope) =
                serde_json::from_slice::<KubeVirtExecutorRequestEnvelope>(&message.payload)
            else {
                tracing::warn!(
                    event = "environment.kubevirt_executor.request_rejected",
                    diagnostic = "LW_ENVIRONMENT_KUBEVIRT_EXECUTOR_CONTRACT_INVALID"
                );
                continue;
            };
            let client = self.client.clone();
            let executor = Arc::clone(&self.executor);
            tokio::spawn(async move {
                let fence = envelope.fence;
                let response = executor.execute(envelope).await.unwrap_or_else(|error| {
                    KubeVirtExecutorResponseEnvelope {
                        protocol_version: fence.protocol_version,
                        environment_id: fence.environment_id,
                        operation_id: fence.operation_id,
                        provider_step: fence.provider_step,
                        environment_generation: fence.environment_generation,
                        attempt: fence.attempt,
                        action: fence.action,
                        request_id: fence.request_id,
                        response: KubeVirtExecutorResponse::Failed {
                            failure: kubevirt_executor_failure(&error),
                        },
                    }
                });
                let Ok(payload) = serde_json::to_vec(&response) else {
                    tracing::error!(
                        event = "environment.kubevirt_executor.response_failed",
                        diagnostic = "LW_ENVIRONMENT_KUBEVIRT_EXECUTOR_RESPONSE_INVALID"
                    );
                    return;
                };
                if client.publish(reply, payload.into()).await.is_err() {
                    tracing::warn!(
                        event = "environment.kubevirt_executor.response_failed",
                        diagnostic = "LW_ENVIRONMENT_KUBEVIRT_EXECUTOR_TRANSPORT_FAILED"
                    );
                }
            });
        }
        Err(KubeVirtExecutorFenceError::Transport)
    }
}

fn validate_kubevirt_executor_request(
    envelope: &KubeVirtExecutorRequestEnvelope,
) -> Result<(), KubeVirtExecutorFenceError> {
    let fence = envelope.fence;
    let expected = kubevirt_executor_request_id(fence, &envelope.request)
        .map_err(|_| KubeVirtExecutorFenceError::IdentityMismatch)?;
    if fence.protocol_version != KUBEVIRT_BACKEND_PROTOCOL_VERSION
        || fence.environment_generation == 0
        || fence.request_id != expected
        || !envelope.request.matches_action(fence.action)
        || kubevirt_executor_environment_id(&envelope.request) != fence.environment_id
    {
        return Err(KubeVirtExecutorFenceError::IdentityMismatch);
    }
    Ok(())
}

const fn kubevirt_executor_failure(error: &KubeVirtExecutorFenceError) -> ProviderFailure {
    match error {
        KubeVirtExecutorFenceError::InProgress
        | KubeVirtExecutorFenceError::Database(_)
        | KubeVirtExecutorFenceError::Transport => unavailable(),
        _ => configuration_invalid(),
    }
}

const fn kubevirt_action_name(action: ReconcileAction) -> &'static str {
    match action {
        ReconcileAction::Validate => "validate",
        ReconcileAction::Build => "build",
        ReconcileAction::Provision => "provision",
        ReconcileAction::Observe => "observe",
        ReconcileAction::Start => "start",
        ReconcileAction::Stop => "stop",
        ReconcileAction::Restart => "restart",
        ReconcileAction::Reset => "reset",
        ReconcileAction::Configure => "configure",
        ReconcileAction::Cleanup => "cleanup",
    }
}

#[derive(Debug, thiserror::Error)]
pub enum KubeVirtExecutorFenceError {
    #[error("LW_ENVIRONMENT_KUBEVIRT_EXECUTOR_CONFIGURATION_INVALID")]
    ConfigurationInvalid,
    #[error("LW_ENVIRONMENT_KUBEVIRT_EXECUTOR_IDENTITY_MISMATCH")]
    IdentityMismatch,
    #[error("LW_ENVIRONMENT_KUBEVIRT_EXECUTOR_DEADLINE_EXCEEDED")]
    DeadlineExceeded,
    #[error("LW_ENVIRONMENT_KUBEVIRT_EXECUTOR_STALE_GENERATION")]
    StaleGeneration,
    #[error("LW_ENVIRONMENT_KUBEVIRT_EXECUTOR_TOMBSTONED")]
    Tombstoned,
    #[error("LW_ENVIRONMENT_KUBEVIRT_EXECUTOR_REQUEST_IN_PROGRESS")]
    InProgress,
    #[error("LW_ENVIRONMENT_KUBEVIRT_EXECUTOR_DATABASE_FAILED")]
    Database(#[from] sqlx::Error),
    #[error("LW_ENVIRONMENT_KUBEVIRT_EXECUTOR_TRANSPORT_FAILED")]
    Transport,
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
    pub trust_revision: Revision,
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
        trust_revision: Revision,
        storage: KubeVirtStorageBinding,
        ssh: KubeVirtSshBootstrap,
        resource_budget: KubeVirtResourceBudget,
    ) -> Self {
        Self {
            trust_revision,
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
            || base_disk.validate().is_err()
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
            "labweaver.io/environment": "true",
        });
        let annotations = json!({
            "labweaver.io/release-id": projection.release.id.to_string(),
            "labweaver.io/release-version": projection.release.version.to_string(),
            "labweaver.io/base-disk-binding": base_disk.binding,
            "labweaver.io/base-disk-source-registry": base_disk.source_registry_digest,
            "labweaver.io/base-disk-sha256": base_disk.disk_sha256.to_string(),
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
        let cloud_init_network_data = BASE64_STANDARD.encode(
            b"version: 2\nethernets:\n  default:\n    match:\n      name: \"en*\"\n    dhcp4: true\n",
        );
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
                    "spec":{"podSelector":{"matchLabels":{"labweaver.io/environment-id":instance.id.to_string()}},"policyTypes":["Ingress"],"ingress":[{"from":[
                        {"namespaceSelector":{"matchLabels":{"kubernetes.io/metadata.name":self.configuration.ssh.gateway_namespace}},"podSelector":{"matchLabels":{GATEWAY_LABEL_KEY:self.configuration.ssh.gateway_pod_label}}},
                        {"namespaceSelector":{"matchLabels":{"kubernetes.io/metadata.name":self.configuration.storage.data_source_namespace}},"podSelector":{"matchLabels":{GATEWAY_LABEL_KEY:"kubevirt-executor"}}}
                    ],"ports":[{"protocol":"TCP","port":ssh_port}]}]}
                }),
            ),
            resource(
                "NetworkPolicy",
                Some(&namespace),
                "cdi-clone-ingress",
                json!({
                    "apiVersion":"networking.k8s.io/v1","kind":"NetworkPolicy",
                    "metadata":{"name":"cdi-clone-ingress","namespace":namespace,"labels":labels},
                    "spec":{
                        "podSelector":{"matchLabels":{"cdi.kubevirt.io":"cdi-upload-server"}},
                        "policyTypes":["Ingress"],
                        "ingress":[{
                            "from":[{
                                "namespaceSelector":{"matchLabels":{"kubernetes.io/metadata.name":self.configuration.storage.data_source_namespace}},
                                "podSelector":{"matchLabels":{"cdi.kubevirt.io":"cdi-clone-source"}}
                            }],
                            "ports":[{"protocol":"TCP","port":8443}]
                        }]
                    }
                }),
            ),
            resource(
                "Secret",
                Some(&namespace),
                "cloud-init",
                json!({
                    "apiVersion":"v1","kind":"Secret","type":"Opaque",
                    "metadata":{"name":"cloud-init","namespace":namespace,"labels":labels,"annotations":annotations},
                    "data":{"userdata":cloud_init_data,"networkdata":cloud_init_network_data}
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
                                    {"name":"cloudinit","cloudInitNoCloud":{
                                        "secretRef":{"name":"cloud-init"},
                                        "networkDataSecretRef":{"name":"cloud-init"}
                                    }}
                                ]
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
        if release.approval.trust_revision != self.configuration.trust_revision {
            return Err(ReleaseProjectionError::TrustRevisionMismatch);
        }
        Ok(())
    }

    fn cloud_init_user_data(&self) -> String {
        format!(
            "#cloud-config\nusers:\n  - name: {user}\n    lock_passwd: true\n    shell: /bin/bash\nwrite_files:\n  - path: /etc/ssh/labweaver_user_ca.pub\n    owner: root:root\n    permissions: '0644'\n    content: |\n      {ca}\n  - path: /etc/ssh/auth_principals/{user}\n    owner: root:root\n    permissions: '0644'\n    content: |\n      labweaver-gateway\n      labweaver-collector\n  - path: /etc/ssh/sshd_config.d/99-labweaver.conf\n    owner: root:root\n    permissions: '0644'\n    content: |\n      TrustedUserCAKeys /etc/ssh/labweaver_user_ca.pub\n      AuthorizedPrincipalsFile /etc/ssh/auth_principals/%u\n      AuthorizedKeysFile none\n      PubkeyAuthentication yes\n      AuthenticationMethods publickey\n      AllowUsers {user}\n      PasswordAuthentication no\n      KbdInteractiveAuthentication no\n      PermitRootLogin no\n      AllowTcpForwarding no\n      AllowAgentForwarding no\n      PermitTunnel no\n      X11Forwarding no\nruncmd:\n  - [sshd, -t]\n  - [systemctl, enable, --now, ssh.service]\n",
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
            ssh_host_key_identity_sha256: Some(observed.ssh_host_key_sha256),
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
