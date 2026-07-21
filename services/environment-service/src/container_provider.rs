//! Fail-closed Container release projection and protected Kubernetes resource planning.

use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;
use contracts::authoring::{
    EnvironmentRuntimeSpec, NetworkPolicySpec, RootFilesystemPolicy, RuntimeKind,
};
use contracts::environment::{
    EndpointHealth, EnvironmentEndpoint, EnvironmentInstance, ObservedEnvironmentState,
};
use contracts::events::{
    CloudEvent, EVENT_CONTRACTS, ReleasePublished, ReleaseWithdrawn, subjects,
};
use contracts::supply_chain::ImageArtifact;
use contracts::{
    ArtifactRef, EndpointId, EnvironmentId, OperationId, PolicyId, ReleaseId, Revision,
    Sha256Digest, UtcTimestamp,
};
use futures_util::StreamExt;
use persistence_sqlx::{Domain, InboxDecision, InboxStore};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};

use crate::{
    EnvironmentProvider, ProviderFailure, ProviderFailureCode, ProviderObservation, ReconcileAction,
};

pub const CONTAINER_BACKEND_PROTOCOL_VERSION: u8 = 2;
const MAX_CONTAINER_EXECUTOR_MESSAGE_BYTES: usize = 1024 * 1024;

/// One server-side-apply document. The name is deterministic and never user-controlled.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContainerResource {
    pub kind: String,
    pub namespace: Option<String>,
    pub name: String,
    pub document: Value,
}

/// Complete least-privilege Kubernetes projection for one immutable release.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContainerResourcePlan {
    pub environment_id: contracts::EnvironmentId,
    pub namespace: String,
    pub image: String,
    pub resources: Vec<ContainerResource>,
    pub plan_sha256: Sha256Digest,
}

/// Sanitized backend observation; raw Kubernetes objects are never persisted.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContainerApplyObservation {
    pub ready: bool,
    pub observed_at: UtcTimestamp,
}

/// Durable Environment operation identity carried across the remote Kubernetes boundary.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContainerBackendFence {
    pub protocol_version: u8,
    pub environment_id: EnvironmentId,
    pub operation_id: OperationId,
    pub provider_step: u32,
    pub operation_generation: u64,
    pub attempt: u32,
    pub action: ReconcileAction,
    pub request_id: Sha256Digest,
    pub deadline_at: UtcTimestamp,
}

impl ContainerBackendFence {
    fn for_action(
        instance: &EnvironmentInstance,
        action: ReconcileAction,
    ) -> Result<Self, ProviderFailure> {
        let request_id = Sha256Digest::of_canonical(&serde_json::json!({
            "protocolVersion": CONTAINER_BACKEND_PROTOCOL_VERSION,
            "environmentId": instance.id,
            "operationId": instance.operation.id,
            "providerStep": instance.operation.provider_step,
            "operationGeneration": instance.generation,
            "attempt": instance.operation.attempt,
            "action": action,
        }))
        .map_err(|_| invalid_observation())?;
        Ok(Self {
            protocol_version: CONTAINER_BACKEND_PROTOCOL_VERSION,
            environment_id: instance.id,
            operation_id: instance.operation.id,
            provider_step: instance.operation.provider_step,
            operation_generation: instance.generation,
            attempt: instance.operation.attempt,
            action,
            request_id,
            deadline_at: instance.operation.deadline_at,
        })
    }
}

/// Exact backend seam for Kubernetes server-side apply, observation, and cleanup.
#[async_trait]
pub trait ContainerProviderBackend: Send + Sync {
    async fn apply(
        &self,
        fence: &ContainerBackendFence,
        plan: &ContainerResourcePlan,
    ) -> Result<ContainerApplyObservation, ProviderFailure>;

    async fn observe(
        &self,
        fence: &ContainerBackendFence,
        plan: &ContainerResourcePlan,
    ) -> Result<ContainerApplyObservation, ProviderFailure>;

    async fn scale(
        &self,
        fence: &ContainerBackendFence,
        plan: &ContainerResourcePlan,
        replicas: u32,
    ) -> Result<ContainerApplyObservation, ProviderFailure>;

    async fn restart(
        &self,
        fence: &ContainerBackendFence,
        plan: &ContainerResourcePlan,
        operation_revision: Revision,
    ) -> Result<ContainerApplyObservation, ProviderFailure>;

    async fn delete_namespace(
        &self,
        fence: &ContainerBackendFence,
        plan: &ContainerResourcePlan,
    ) -> Result<ArtifactRef, ProviderFailure>;
}

/// Typed NATS backend for the deployment-owned Kubernetes apply/observe executor.
pub struct NatsContainerProviderBackend {
    client: async_nats::Client,
    subject: String,
}

impl NatsContainerProviderBackend {
    pub fn new(
        client: async_nats::Client,
        subject: String,
    ) -> Result<Self, ReleaseProjectionError> {
        if !valid_subject(&subject) {
            return Err(ReleaseProjectionError::ConfigurationInvalid);
        }
        Ok(Self { client, subject })
    }

    async fn request(
        &self,
        fence: &ContainerBackendFence,
        request: ContainerExecutorRequest,
    ) -> Result<ContainerExecutorResponse, ProviderFailure> {
        if !request.matches_action(fence.action) {
            return Err(invalid_observation());
        }
        let fence = bind_container_executor_request(*fence, &request)?;
        let payload = serde_json::to_vec(&ContainerExecutorRequestEnvelope { fence, request })
            .map_err(|_| invalid_observation())?;
        let message = self
            .client
            .request(self.subject.clone(), payload.into())
            .await
            .map_err(|_| unavailable())?;
        if message.payload.len() > MAX_CONTAINER_EXECUTOR_MESSAGE_BYTES {
            return Err(invalid_observation());
        }
        let response: ContainerExecutorResponseEnvelope =
            serde_json::from_slice(&message.payload).map_err(|_| invalid_observation())?;
        if response.protocol_version != fence.protocol_version
            || response.environment_id != fence.environment_id
            || response.operation_id != fence.operation_id
            || response.provider_step != fence.provider_step
            || response.operation_generation != fence.operation_generation
            || response.attempt != fence.attempt
            || response.request_id != fence.request_id
            || response.action != fence.action
        {
            return Err(invalid_observation());
        }
        Ok(response.response)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContainerExecutorRequestEnvelope {
    #[serde(flatten)]
    pub fence: ContainerBackendFence,
    pub request: ContainerExecutorRequest,
}

#[async_trait]
impl ContainerProviderBackend for NatsContainerProviderBackend {
    async fn apply(
        &self,
        fence: &ContainerBackendFence,
        plan: &ContainerResourcePlan,
    ) -> Result<ContainerApplyObservation, ProviderFailure> {
        match self
            .request(
                fence,
                ContainerExecutorRequest::Apply { plan: plan.clone() },
            )
            .await?
        {
            ContainerExecutorResponse::Observed {
                plan_sha256,
                observation,
            } if plan_sha256 == plan.plan_sha256 => Ok(observation),
            ContainerExecutorResponse::Failed { failure } => Err(failure),
            _ => Err(invalid_observation()),
        }
    }

    async fn observe(
        &self,
        fence: &ContainerBackendFence,
        plan: &ContainerResourcePlan,
    ) -> Result<ContainerApplyObservation, ProviderFailure> {
        match self
            .request(
                fence,
                ContainerExecutorRequest::Observe { plan: plan.clone() },
            )
            .await?
        {
            ContainerExecutorResponse::Observed {
                plan_sha256,
                observation,
            } if plan_sha256 == plan.plan_sha256 => Ok(observation),
            ContainerExecutorResponse::Failed { failure } => Err(failure),
            _ => Err(invalid_observation()),
        }
    }

    async fn scale(
        &self,
        fence: &ContainerBackendFence,
        plan: &ContainerResourcePlan,
        replicas: u32,
    ) -> Result<ContainerApplyObservation, ProviderFailure> {
        match self
            .request(
                fence,
                ContainerExecutorRequest::Scale {
                    plan: plan.clone(),
                    replicas,
                },
            )
            .await?
        {
            ContainerExecutorResponse::Observed {
                plan_sha256,
                observation,
            } if plan_sha256 == plan.plan_sha256 => Ok(observation),
            ContainerExecutorResponse::Failed { failure } => Err(failure),
            _ => Err(invalid_observation()),
        }
    }

    async fn restart(
        &self,
        fence: &ContainerBackendFence,
        plan: &ContainerResourcePlan,
        operation_revision: Revision,
    ) -> Result<ContainerApplyObservation, ProviderFailure> {
        match self
            .request(
                fence,
                ContainerExecutorRequest::Restart {
                    plan: plan.clone(),
                    operation_revision,
                },
            )
            .await?
        {
            ContainerExecutorResponse::Observed {
                plan_sha256,
                observation,
            } if plan_sha256 == plan.plan_sha256 => Ok(observation),
            ContainerExecutorResponse::Failed { failure } => Err(failure),
            _ => Err(invalid_observation()),
        }
    }

    async fn delete_namespace(
        &self,
        fence: &ContainerBackendFence,
        plan: &ContainerResourcePlan,
    ) -> Result<ArtifactRef, ProviderFailure> {
        match self
            .request(
                fence,
                ContainerExecutorRequest::DeleteNamespace { plan: plan.clone() },
            )
            .await?
        {
            ContainerExecutorResponse::Deleted {
                plan_sha256,
                cleanup_evidence,
            } if plan_sha256 == plan.plan_sha256 && valid_artifact_ref(&cleanup_evidence) => {
                Ok(cleanup_evidence)
            }
            ContainerExecutorResponse::Failed { failure } => Err(failure),
            _ => Err(ProviderFailure {
                code: ProviderFailureCode::CleanupFailed,
                retryable: true,
            }),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(
    tag = "backendAction",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ContainerExecutorRequest {
    Apply {
        plan: ContainerResourcePlan,
    },
    Observe {
        plan: ContainerResourcePlan,
    },
    Scale {
        plan: ContainerResourcePlan,
        replicas: u32,
    },
    Restart {
        plan: ContainerResourcePlan,
        operation_revision: Revision,
    },
    DeleteNamespace {
        plan: ContainerResourcePlan,
    },
}

impl ContainerExecutorRequest {
    const fn matches_action(&self, action: ReconcileAction) -> bool {
        matches!(
            (self, action),
            (
                Self::Apply { .. },
                ReconcileAction::Provision | ReconcileAction::Reset
            ) | (Self::Observe { .. }, ReconcileAction::Observe)
                | (Self::Scale { replicas: 0, .. }, ReconcileAction::Stop)
                | (Self::Scale { .. }, ReconcileAction::Start)
                | (Self::Restart { .. }, ReconcileAction::Restart)
                | (Self::DeleteNamespace { .. }, ReconcileAction::Cleanup)
        )
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContainerExecutorResponseEnvelope {
    pub protocol_version: u8,
    pub environment_id: EnvironmentId,
    pub operation_id: OperationId,
    pub provider_step: u32,
    pub operation_generation: u64,
    pub attempt: u32,
    pub request_id: Sha256Digest,
    pub action: ReconcileAction,
    pub response: ContainerExecutorResponse,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ContainerExecutorResponse {
    Observed {
        plan_sha256: Sha256Digest,
        observation: ContainerApplyObservation,
    },
    Deleted {
        plan_sha256: Sha256Digest,
        cleanup_evidence: ArtifactRef,
    },
    Failed {
        failure: ProviderFailure,
    },
}

/// Kubernetes side-effect adapter invoked only after durable executor admission.
#[async_trait]
pub trait ContainerExecutorBackend: Send + Sync {
    async fn execute(
        &self,
        fence: &ContainerBackendFence,
        request: &ContainerExecutorRequest,
    ) -> ContainerExecutorResponse;
}

/// Executor-side `PostgreSQL` highest-generation and permanent-delete ledger.
#[derive(Clone, Debug)]
pub struct PgContainerExecutorFenceStore {
    pool: PgPool,
}

impl PgContainerExecutorFenceStore {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "admission keeps the complete row-lock, generation and tombstone decision auditable"
    )]
    async fn admit(
        &self,
        envelope: &ContainerExecutorRequestEnvelope,
    ) -> Result<ContainerExecutorAdmission, ContainerExecutorFenceError> {
        validate_container_executor_request(envelope)?;
        let fence = envelope.fence;
        let mut transaction = self.pool.begin().await?;
        let authority_now: time::OffsetDateTime =
            sqlx::query_scalar("SELECT date_trunc('milliseconds',clock_timestamp())")
                .fetch_one(&mut *transaction)
                .await?;
        if authority_now >= fence.deadline_at.get() {
            return Err(ContainerExecutorFenceError::DeadlineExceeded);
        }
        let remaining = std::time::Duration::try_from(fence.deadline_at.get() - authority_now)
            .map_err(|_| ContainerExecutorFenceError::DeadlineExceeded)?;
        let current = sqlx::query(
            "SELECT highest_generation,operation_id,provider_step,attempt,tombstoned, \
                    last_request_id,last_response,deadline_at \
             FROM environment.container_executor_fences WHERE environment_id=$1 FOR UPDATE",
        )
        .bind(fence.environment_id.as_uuid())
        .fetch_optional(&mut *transaction)
        .await?;
        if let Some(row) = current {
            let highest_generation = u64::try_from(row.try_get::<i64, _>("highest_generation")?)
                .map_err(|_| ContainerExecutorFenceError::IdentityMismatch)?;
            let operation_id =
                OperationId::from_str(&row.try_get::<uuid::Uuid, _>("operation_id")?.to_string())
                    .map_err(|_| ContainerExecutorFenceError::IdentityMismatch)?;
            let provider_step = u32::try_from(row.try_get::<i32, _>("provider_step")?)
                .map_err(|_| ContainerExecutorFenceError::IdentityMismatch)?;
            let attempt = u32::try_from(row.try_get::<i32, _>("attempt")?)
                .map_err(|_| ContainerExecutorFenceError::IdentityMismatch)?;
            let tombstoned: bool = row.try_get("tombstoned")?;
            let last_request_id: String = row.try_get("last_request_id")?;
            let last_response = row.try_get::<Option<Value>, _>("last_response")?;
            let previous_deadline: time::OffsetDateTime = row.try_get("deadline_at")?;
            if last_request_id == fence.request_id.to_string() {
                if let Some(value) = last_response {
                    transaction.rollback().await?;
                    return Ok(ContainerExecutorAdmission::Replay(value));
                }
                return Err(ContainerExecutorFenceError::InProgress);
            }
            if last_response.is_none() && authority_now < previous_deadline {
                return Err(ContainerExecutorFenceError::InProgress);
            }
            if tombstoned {
                return Err(ContainerExecutorFenceError::Tombstoned);
            }
            if fence.operation_generation < highest_generation
                || (fence.operation_generation == highest_generation
                    && (fence.provider_step < provider_step
                        || (fence.provider_step == provider_step && fence.attempt < attempt)))
            {
                return Err(ContainerExecutorFenceError::StaleGeneration);
            }
            if fence.operation_generation == highest_generation
                && fence.operation_id != operation_id
            {
                return Err(ContainerExecutorFenceError::IdentityMismatch);
            }
            sqlx::query(
                "UPDATE environment.container_executor_fences SET highest_generation=$2, \
                     operation_id=$3,provider_step=$4,attempt=$5,tombstoned=$6,last_action=$7, \
                     last_request_id=$8,last_response=NULL,deadline_at=$9,updated_at=clock_timestamp() \
                 WHERE environment_id=$1",
            )
            .bind(fence.environment_id.as_uuid())
            .bind(i64::try_from(fence.operation_generation).map_err(|_| ContainerExecutorFenceError::IdentityMismatch)?)
            .bind(fence.operation_id.as_uuid())
            .bind(i32::try_from(fence.provider_step).map_err(|_| ContainerExecutorFenceError::IdentityMismatch)?)
            .bind(i32::try_from(fence.attempt).map_err(|_| ContainerExecutorFenceError::IdentityMismatch)?)
            .bind(fence.action == ReconcileAction::Cleanup)
            .bind(reconcile_action_name(fence.action))
            .bind(fence.request_id.to_string())
            .bind(fence.deadline_at.get())
            .execute(&mut *transaction)
            .await?;
        } else {
            sqlx::query(
                "INSERT INTO environment.container_executor_fences \
                 (environment_id,highest_generation,operation_id,provider_step,attempt,tombstoned, \
                  last_action,last_request_id,deadline_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
            )
            .bind(fence.environment_id.as_uuid())
            .bind(
                i64::try_from(fence.operation_generation)
                    .map_err(|_| ContainerExecutorFenceError::IdentityMismatch)?,
            )
            .bind(fence.operation_id.as_uuid())
            .bind(
                i32::try_from(fence.provider_step)
                    .map_err(|_| ContainerExecutorFenceError::IdentityMismatch)?,
            )
            .bind(
                i32::try_from(fence.attempt)
                    .map_err(|_| ContainerExecutorFenceError::IdentityMismatch)?,
            )
            .bind(fence.action == ReconcileAction::Cleanup)
            .bind(reconcile_action_name(fence.action))
            .bind(fence.request_id.to_string())
            .bind(fence.deadline_at.get())
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(ContainerExecutorAdmission::Execute(remaining))
    }

    async fn complete(
        &self,
        fence: ContainerBackendFence,
        response: &ContainerExecutorResponse,
    ) -> Result<(), ContainerExecutorFenceError> {
        let value = serde_json::to_value(response)
            .map_err(|_| ContainerExecutorFenceError::IdentityMismatch)?;
        let updated = sqlx::query(
            "UPDATE environment.container_executor_fences SET last_response=$7,updated_at=clock_timestamp() \
             WHERE environment_id=$1 AND highest_generation=$2 AND operation_id=$3 \
               AND provider_step=$4 AND attempt=$5 AND last_request_id=$6 AND last_response IS NULL",
        )
        .bind(fence.environment_id.as_uuid())
        .bind(i64::try_from(fence.operation_generation).map_err(|_| ContainerExecutorFenceError::IdentityMismatch)?)
        .bind(fence.operation_id.as_uuid())
        .bind(i32::try_from(fence.provider_step).map_err(|_| ContainerExecutorFenceError::IdentityMismatch)?)
        .bind(i32::try_from(fence.attempt).map_err(|_| ContainerExecutorFenceError::IdentityMismatch)?)
        .bind(fence.request_id.to_string())
        .bind(value)
        .execute(&self.pool)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(ContainerExecutorFenceError::StaleGeneration);
        }
        Ok(())
    }
}

enum ContainerExecutorAdmission {
    Execute(std::time::Duration),
    Replay(Value),
}

/// Server-side wrapper that persists fencing/tombstones before Kubernetes side effects.
pub struct FencedContainerExecutor<B> {
    store: PgContainerExecutorFenceStore,
    backend: B,
}

impl<B: ContainerExecutorBackend> FencedContainerExecutor<B> {
    #[must_use]
    pub const fn new(store: PgContainerExecutorFenceStore, backend: B) -> Self {
        Self { store, backend }
    }

    pub async fn execute(
        &self,
        envelope: ContainerExecutorRequestEnvelope,
    ) -> Result<ContainerExecutorResponseEnvelope, ContainerExecutorFenceError> {
        let response = match self.store.admit(&envelope).await? {
            ContainerExecutorAdmission::Execute(remaining) => {
                let response = tokio::time::timeout(
                    remaining,
                    self.backend.execute(&envelope.fence, &envelope.request),
                )
                .await
                .map_err(|_| ContainerExecutorFenceError::DeadlineExceeded)?;
                self.store.complete(envelope.fence, &response).await?;
                response
            }
            ContainerExecutorAdmission::Replay(value) => serde_json::from_value(value)
                .map_err(|_| ContainerExecutorFenceError::IdentityMismatch)?,
        };
        Ok(ContainerExecutorResponseEnvelope {
            protocol_version: envelope.fence.protocol_version,
            environment_id: envelope.fence.environment_id,
            operation_id: envelope.fence.operation_id,
            provider_step: envelope.fence.provider_step,
            operation_generation: envelope.fence.operation_generation,
            attempt: envelope.fence.attempt,
            request_id: envelope.fence.request_id,
            action: envelope.fence.action,
            response,
        })
    }
}

/// NATS request/reply server used by the deployment-owned Kubernetes executor process.
pub struct NatsContainerExecutorServer<B> {
    client: async_nats::Client,
    subject: String,
    executor: Arc<FencedContainerExecutor<B>>,
}

impl<B: ContainerExecutorBackend + 'static> NatsContainerExecutorServer<B> {
    pub fn new(
        client: async_nats::Client,
        subject: String,
        executor: FencedContainerExecutor<B>,
    ) -> Result<Self, ContainerExecutorFenceError> {
        if !valid_subject(&subject) {
            return Err(ContainerExecutorFenceError::ConfigurationInvalid);
        }
        Ok(Self {
            client,
            subject,
            executor: Arc::new(executor),
        })
    }

    /// Serves bounded requests and never invokes Kubernetes for malformed/no-reply messages.
    pub async fn serve(self) -> Result<(), ContainerExecutorFenceError> {
        let mut subscriber = self
            .client
            .subscribe(self.subject)
            .await
            .map_err(|_| ContainerExecutorFenceError::Transport)?;
        while let Some(message) = subscriber.next().await {
            let Some(reply) = message.reply.clone() else {
                tracing::warn!(
                    event = "environment.container_executor.request_rejected",
                    diagnostic = "LW_ENVIRONMENT_CONTAINER_EXECUTOR_REPLY_REQUIRED"
                );
                continue;
            };
            if message.payload.len() > MAX_CONTAINER_EXECUTOR_MESSAGE_BYTES {
                tracing::warn!(
                    event = "environment.container_executor.request_rejected",
                    diagnostic = "LW_ENVIRONMENT_CONTAINER_EXECUTOR_PAYLOAD_TOO_LARGE"
                );
                continue;
            }
            let Ok(envelope) =
                serde_json::from_slice::<ContainerExecutorRequestEnvelope>(&message.payload)
            else {
                tracing::warn!(
                    event = "environment.container_executor.request_rejected",
                    diagnostic = "LW_ENVIRONMENT_CONTAINER_EXECUTOR_CONTRACT_INVALID"
                );
                continue;
            };
            let client = self.client.clone();
            let executor = Arc::clone(&self.executor);
            tokio::spawn(async move {
                let fence = envelope.fence;
                let response = match executor.execute(envelope).await {
                    Ok(response) => response,
                    Err(error) => ContainerExecutorResponseEnvelope {
                        protocol_version: fence.protocol_version,
                        environment_id: fence.environment_id,
                        operation_id: fence.operation_id,
                        provider_step: fence.provider_step,
                        operation_generation: fence.operation_generation,
                        attempt: fence.attempt,
                        request_id: fence.request_id,
                        action: fence.action,
                        response: ContainerExecutorResponse::Failed {
                            failure: container_executor_failure(&error),
                        },
                    },
                };
                let Ok(payload) = serde_json::to_vec(&response) else {
                    tracing::error!(
                        event = "environment.container_executor.response_failed",
                        diagnostic = "LW_ENVIRONMENT_CONTAINER_EXECUTOR_RESPONSE_INVALID"
                    );
                    return;
                };
                if client.publish(reply, payload.into()).await.is_err() {
                    tracing::warn!(
                        event = "environment.container_executor.response_failed",
                        diagnostic = "LW_ENVIRONMENT_CONTAINER_EXECUTOR_TRANSPORT_FAILED"
                    );
                }
            });
        }
        Err(ContainerExecutorFenceError::Transport)
    }
}

const fn container_executor_failure(error: &ContainerExecutorFenceError) -> ProviderFailure {
    match error {
        ContainerExecutorFenceError::InProgress
        | ContainerExecutorFenceError::Database(_)
        | ContainerExecutorFenceError::Transport => ProviderFailure {
            code: ProviderFailureCode::Unavailable,
            retryable: true,
        },
        ContainerExecutorFenceError::ConfigurationInvalid
        | ContainerExecutorFenceError::IdentityMismatch
        | ContainerExecutorFenceError::DeadlineExceeded
        | ContainerExecutorFenceError::StaleGeneration
        | ContainerExecutorFenceError::Tombstoned => ProviderFailure {
            code: ProviderFailureCode::Rejected,
            retryable: false,
        },
    }
}

fn validate_container_executor_request(
    envelope: &ContainerExecutorRequestEnvelope,
) -> Result<(), ContainerExecutorFenceError> {
    let fence = envelope.fence;
    let expected_request_id = container_executor_request_id(fence, &envelope.request)?;
    if fence.protocol_version != CONTAINER_BACKEND_PROTOCOL_VERSION
        || fence.operation_generation == 0
        || fence.request_id != expected_request_id
        || !envelope.request.matches_action(fence.action)
        || container_executor_plan(&envelope.request).environment_id != fence.environment_id
    {
        return Err(ContainerExecutorFenceError::IdentityMismatch);
    }
    Ok(())
}

fn bind_container_executor_request(
    mut fence: ContainerBackendFence,
    request: &ContainerExecutorRequest,
) -> Result<ContainerBackendFence, ProviderFailure> {
    fence.request_id =
        container_executor_request_id(fence, request).map_err(|_| invalid_observation())?;
    Ok(fence)
}

fn container_executor_request_id(
    fence: ContainerBackendFence,
    request: &ContainerExecutorRequest,
) -> Result<Sha256Digest, ContainerExecutorFenceError> {
    Sha256Digest::of_canonical(&serde_json::json!({
        "protocolVersion": fence.protocol_version,
        "environmentId": fence.environment_id,
        "operationId": fence.operation_id,
        "providerStep": fence.provider_step,
        "operationGeneration": fence.operation_generation,
        "attempt": fence.attempt,
        "action": fence.action,
        "deadlineAt": fence.deadline_at,
        "request": request,
    }))
    .map_err(|_| ContainerExecutorFenceError::IdentityMismatch)
}

const fn container_executor_plan(request: &ContainerExecutorRequest) -> &ContainerResourcePlan {
    match request {
        ContainerExecutorRequest::Apply { plan }
        | ContainerExecutorRequest::Observe { plan }
        | ContainerExecutorRequest::Scale { plan, .. }
        | ContainerExecutorRequest::Restart { plan, .. }
        | ContainerExecutorRequest::DeleteNamespace { plan } => plan,
    }
}

const fn reconcile_action_name(action: ReconcileAction) -> &'static str {
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
pub enum ContainerExecutorFenceError {
    #[error("LW_ENVIRONMENT_CONTAINER_EXECUTOR_CONFIGURATION_INVALID")]
    ConfigurationInvalid,
    #[error("LW_ENVIRONMENT_CONTAINER_EXECUTOR_IDENTITY_MISMATCH")]
    IdentityMismatch,
    #[error("LW_ENVIRONMENT_CONTAINER_EXECUTOR_DEADLINE_EXCEEDED")]
    DeadlineExceeded,
    #[error("LW_ENVIRONMENT_CONTAINER_EXECUTOR_STALE_GENERATION")]
    StaleGeneration,
    #[error("LW_ENVIRONMENT_CONTAINER_EXECUTOR_TOMBSTONED")]
    Tombstoned,
    #[error("LW_ENVIRONMENT_CONTAINER_EXECUTOR_REQUEST_IN_PROGRESS")]
    InProgress,
    #[error("LW_ENVIRONMENT_CONTAINER_EXECUTOR_DATABASE_FAILED")]
    Database(#[from] sqlx::Error),
    #[error("LW_ENVIRONMENT_CONTAINER_EXECUTOR_TRANSPORT_FAILED")]
    Transport,
}

/// Exact immutable Release lookup used by a Provider action.
#[async_trait]
pub trait ContainerReleaseResolver: Send + Sync {
    async fn resolve(
        &self,
        release_id: ReleaseId,
        release_version: u64,
    ) -> Result<ResolvedContainerRelease, ReleaseProjectionError>;
}

/// Release projection paired with Environment's authority clock and append-only withdrawal state.
#[derive(Clone, Debug)]
pub struct ResolvedContainerRelease {
    pub projection: ReleasePublished,
    pub authority_now: UtcTimestamp,
    pub withdrawn_at: Option<UtcTimestamp>,
}

/// Deployment authority for the currently accepted scan policy and approval revision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContainerReleasePolicy {
    pub image_policy_id: PolicyId,
    pub image_policy_revision: Revision,
    pub trust_revision: Revision,
}

impl ContainerReleasePolicy {
    pub fn new(
        image_policy_id: PolicyId,
        image_policy_revision: Revision,
        trust_revision: Revision,
    ) -> Result<Self, ReleaseProjectionError> {
        Ok(Self {
            image_policy_id,
            image_policy_revision,
            trust_revision,
        })
    }
}

/// Reviewed non-secret configuration for one exact Container Provider binding.
#[derive(Clone, Debug)]
pub struct ContainerProviderConfiguration {
    pub release_policy: ContainerReleasePolicy,
    pub access_namespace: String,
    pub access_pod_label: String,
    pub image_pull_secret_name: String,
    pub workspace_storage_class_name: String,
}

impl ContainerProviderConfiguration {
    pub fn new(
        release_policy: ContainerReleasePolicy,
        access_namespace: String,
        access_pod_label: String,
        image_pull_secret_name: String,
        workspace_storage_class_name: String,
    ) -> Result<Self, ReleaseProjectionError> {
        if !valid_dns_label(&access_namespace)
            || !valid_dns_label(&access_pod_label)
            || !valid_dns_label(&image_pull_secret_name)
            || !valid_dns_label(&workspace_storage_class_name)
        {
            return Err(ReleaseProjectionError::ConfigurationInvalid);
        }
        Ok(Self {
            release_policy,
            access_namespace,
            access_pod_label,
            image_pull_secret_name,
            workspace_storage_class_name,
        })
    }
}

/// Durable projection result for a release `CloudEvent`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReleaseProjectionDecision {
    Applied,
    Duplicate,
    Stale,
    Gap,
}

/// Environment-owned immutable release projection.
#[derive(Clone)]
pub struct PgReleaseProjectionStore {
    pool: PgPool,
}

impl PgReleaseProjectionStore {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn accept(
        &self,
        consumer: &str,
        event: &CloudEvent<ReleasePublished>,
    ) -> Result<ReleaseProjectionDecision, ReleaseProjectionError> {
        let contract = EVENT_CONTRACTS
            .iter()
            .copied()
            .find(|contract| contract.subject == subjects::ENVIRONMENT_TEMPLATE_RELEASE_PUBLISHED)
            .ok_or(ReleaseProjectionError::ContractInvalid)?;
        event
            .validate(contract)
            .map_err(|_| ReleaseProjectionError::ContractInvalid)?;
        event
            .data
            .validate()
            .map_err(|_| ReleaseProjectionError::ContractInvalid)?;
        if event.subject != subjects::ENVIRONMENT_TEMPLATE_RELEASE_PUBLISHED
            || event.course_id != event.data.release.course_id
            || event.aggregate_sequence.0 != 1
            || event.aggregate_revision
                != Revision::new(event.data.release.version)
                    .map_err(|_| ReleaseProjectionError::ContractInvalid)?
        {
            return Err(ReleaseProjectionError::IdentityMismatch);
        }
        let provider_binding = container_provider_binding(&event.data)?;
        let payload =
            serde_json::to_value(event).map_err(|_| ReleaseProjectionError::ContractInvalid)?;
        let payload_sha256 = canonical_hash(&payload)?;
        let mut transaction = self.pool.begin().await?;
        let decision = InboxStore::accept(
            &mut transaction,
            Domain::Environment,
            consumer,
            event.id.as_uuid(),
            event.data.release.id.as_uuid(),
            event.aggregate_sequence.0,
            payload_sha256,
        )
        .await
        .map_err(|_| ReleaseProjectionError::PersistenceFailed)?;
        let outcome = match decision {
            InboxDecision::Accepted => {
                let contract = serde_json::to_value(&event.data)
                    .map_err(|_| ReleaseProjectionError::ContractInvalid)?;
                sqlx::query(
                    "INSERT INTO environment.release_projections \
                     (release_id,course_id,release_version,provider_binding,projection_sha256,contract,projected_event_id,aggregate_sequence) \
                     VALUES ($1,$2,$3,$4,$5,$6,$7,1)",
                )
                .bind(event.data.release.id.as_uuid())
                .bind(event.course_id.as_uuid())
                .bind(i64::try_from(event.data.release.version).map_err(|_| ReleaseProjectionError::IdentityMismatch)?)
                .bind(provider_binding)
                .bind(event.data.projection_sha256.to_string())
                .bind(contract)
                .bind(event.id.as_uuid())
                .execute(&mut *transaction)
                .await
                .map_err(|error| {
                    if is_unique_violation(&error) {
                        ReleaseProjectionError::IdentityMismatch
                    } else {
                        ReleaseProjectionError::Database(error)
                    }
                })?;
                ReleaseProjectionDecision::Applied
            }
            InboxDecision::Duplicate => ReleaseProjectionDecision::Duplicate,
            InboxDecision::Stale => ReleaseProjectionDecision::Stale,
            InboxDecision::Gap => {
                transaction.rollback().await?;
                return Ok(ReleaseProjectionDecision::Gap);
            }
        };
        transaction.commit().await?;
        Ok(outcome)
    }

    pub async fn accept_withdrawal(
        &self,
        consumer: &str,
        event: &CloudEvent<ReleaseWithdrawn>,
    ) -> Result<ReleaseProjectionDecision, ReleaseProjectionError> {
        let contract = EVENT_CONTRACTS
            .iter()
            .copied()
            .find(|contract| contract.subject == subjects::ENVIRONMENT_TEMPLATE_RELEASE_WITHDRAWN)
            .ok_or(ReleaseProjectionError::ContractInvalid)?;
        event
            .validate(contract)
            .map_err(|_| ReleaseProjectionError::ContractInvalid)?;
        if event.subject != subjects::ENVIRONMENT_TEMPLATE_RELEASE_WITHDRAWN
            || event.aggregate_sequence.0 != 2
            || event.aggregate_revision
                != Revision::new(event.data.version)
                    .map_err(|_| ReleaseProjectionError::ContractInvalid)?
            || event.data.reason_code.trim().is_empty()
            || event.data.withdrawn_at != event.time
        {
            return Err(ReleaseProjectionError::IdentityMismatch);
        }
        let payload =
            serde_json::to_value(event).map_err(|_| ReleaseProjectionError::ContractInvalid)?;
        let payload_sha256 = canonical_hash(&payload)?;
        let mut transaction = self.pool.begin().await?;
        let decision = InboxStore::accept(
            &mut transaction,
            Domain::Environment,
            consumer,
            event.id.as_uuid(),
            event.data.release_id.as_uuid(),
            event.aggregate_sequence.0,
            payload_sha256,
        )
        .await
        .map_err(|_| ReleaseProjectionError::PersistenceFailed)?;
        let outcome = match decision {
            InboxDecision::Accepted => {
                let result = sqlx::query(
                    "UPDATE environment.release_projections \
                     SET aggregate_sequence=2,withdrawn_at=$4,withdrawal_reason_code=$5, \
                         withdrawal_event_id=$6,updated_at=clock_timestamp() \
                     WHERE release_id=$1 AND course_id=$2 AND release_version=$3 \
                       AND aggregate_sequence=1 AND withdrawn_at IS NULL",
                )
                .bind(event.data.release_id.as_uuid())
                .bind(event.course_id.as_uuid())
                .bind(
                    i64::try_from(event.data.version)
                        .map_err(|_| ReleaseProjectionError::IdentityMismatch)?,
                )
                .bind(event.data.withdrawn_at.get())
                .bind(&event.data.reason_code)
                .bind(event.id.as_uuid())
                .execute(&mut *transaction)
                .await?;
                if result.rows_affected() != 1 {
                    transaction.rollback().await?;
                    return Err(ReleaseProjectionError::IdentityMismatch);
                }
                ReleaseProjectionDecision::Applied
            }
            InboxDecision::Duplicate => ReleaseProjectionDecision::Duplicate,
            InboxDecision::Stale => ReleaseProjectionDecision::Stale,
            InboxDecision::Gap => {
                transaction.rollback().await?;
                return Ok(ReleaseProjectionDecision::Gap);
            }
        };
        transaction.commit().await?;
        Ok(outcome)
    }
}

#[async_trait]
impl ContainerReleaseResolver for PgReleaseProjectionStore {
    async fn resolve(
        &self,
        release_id: ReleaseId,
        release_version: u64,
    ) -> Result<ResolvedContainerRelease, ReleaseProjectionError> {
        let row = sqlx::query(
            "SELECT release_version,contract,projection_sha256,withdrawn_at, \
                    date_trunc('milliseconds',clock_timestamp()) AS authority_now \
             FROM environment.release_projections \
             WHERE release_id=$1",
        )
        .bind(release_id.as_uuid())
        .fetch_optional(&self.pool)
        .await?
        .ok_or(ReleaseProjectionError::NotFound)?;
        let stored_version: i64 = row.try_get("release_version")?;
        if u64::try_from(stored_version).ok() != Some(release_version) {
            return Err(ReleaseProjectionError::IdentityMismatch);
        }
        let projection: ReleasePublished = serde_json::from_value(row.try_get("contract")?)
            .map_err(|_| ReleaseProjectionError::ContractInvalid)?;
        projection
            .validate()
            .map_err(|_| ReleaseProjectionError::ContractInvalid)?;
        let stored_sha256: String = row.try_get("projection_sha256")?;
        if projection.release.id != release_id
            || projection.release.version != release_version
            || projection.projection_sha256.to_string() != stored_sha256
        {
            return Err(ReleaseProjectionError::IdentityMismatch);
        }
        let authority_now: time::OffsetDateTime = row.try_get("authority_now")?;
        let withdrawn_at: Option<time::OffsetDateTime> = row.try_get("withdrawn_at")?;
        Ok(ResolvedContainerRelease {
            projection,
            authority_now: UtcTimestamp::from_utc(authority_now)
                .map_err(|_| ReleaseProjectionError::ContractInvalid)?,
            withdrawn_at: withdrawn_at
                .map(UtcTimestamp::from_utc)
                .transpose()
                .map_err(|_| ReleaseProjectionError::ContractInvalid)?,
        })
    }
}

/// Container implementation of the existing Environment Provider state machine.
pub struct ContainerProvider<B, R> {
    binding: String,
    backend: Arc<B>,
    releases: Arc<R>,
    release_policy: ContainerReleasePolicy,
    access_namespace: String,
    access_pod_label: String,
    image_pull_secret_name: String,
    workspace_storage_class_name: String,
}

impl<B, R> ContainerProvider<B, R>
where
    B: ContainerProviderBackend,
    R: ContainerReleaseResolver,
{
    pub fn new(
        binding: String,
        backend: Arc<B>,
        releases: Arc<R>,
        configuration: ContainerProviderConfiguration,
    ) -> Result<Self, ReleaseProjectionError> {
        if !valid_binding(&binding) {
            return Err(ReleaseProjectionError::ConfigurationInvalid);
        }
        Ok(Self {
            binding,
            backend,
            releases,
            release_policy: configuration.release_policy,
            access_namespace: configuration.access_namespace,
            access_pod_label: configuration.access_pod_label,
            image_pull_secret_name: configuration.image_pull_secret_name,
            workspace_storage_class_name: configuration.workspace_storage_class_name,
        })
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the complete deterministic Kubernetes security bundle is reviewed as one projection"
    )]
    pub fn plan(
        &self,
        instance: &EnvironmentInstance,
        resolved: &ResolvedContainerRelease,
        action: ReconcileAction,
    ) -> Result<ContainerResourcePlan, ReleaseProjectionError> {
        let projection = &resolved.projection;
        projection
            .validate()
            .map_err(|_| ReleaseProjectionError::ContractInvalid)?;
        self.validate_release_use(resolved, action)?;
        if instance.runtime_kind != RuntimeKind::Container
            || instance.release_id != projection.release.id
            || instance.release_version != projection.release.version
            || instance.course_id != projection.release.course_id
            || instance.provider_binding != self.binding
            || container_provider_binding(projection)? != self.binding
        {
            return Err(ReleaseProjectionError::IdentityMismatch);
        }
        let ImageArtifact::Container {
            repository, digest, ..
        } = &projection.release.artifact
        else {
            return Err(ReleaseProjectionError::IdentityMismatch);
        };
        if !digest.starts_with("sha256:")
            || repository.contains('@')
            || repository.contains(char::is_whitespace)
        {
            return Err(ReleaseProjectionError::ContractInvalid);
        }
        let mut repository_parts = repository.split('/');
        let registry = repository_parts.next().unwrap_or_default();
        let project = repository_parts.next().unwrap_or_default();
        let image_name = repository_parts.next().unwrap_or_default();
        if registry.is_empty()
            || project != format!("course-{}", projection.release.course_id)
            || image_name != projection.release.candidate_id.to_string()
            || repository_parts.next().is_some()
        {
            return Err(ReleaseProjectionError::IdentityMismatch);
        }
        let image = format!("{repository}@{digest}");
        let namespace = format!("lw-env-{}", instance.id);
        let app_name = "runtime";
        let labels = json!({
            "app.kubernetes.io/name": "labweaver-environment",
            "labweaver.io/environment-id": instance.id.to_string(),
            "labweaver.io/course-id": instance.course_id.to_string(),
            "labweaver.io/managed": "true",
            "labweaver.io/environment": "true",
        });
        let resources = &projection.environment_spec.resources;
        let service_port = match &projection.environment_spec.runtime {
            EnvironmentRuntimeSpec::Container { service_port, .. } => *service_port,
            EnvironmentRuntimeSpec::VirtualMachine { .. } => {
                return Err(ReleaseProjectionError::IdentityMismatch);
            }
        };
        if !projection.environment_spec.entries.iter().any(|entry| {
            entry.service_port == service_port
                && entry.protocol == contracts::environment::EndpointProtocol::Https
        }) {
            return Err(ReleaseProjectionError::SecurityPostureInvalid);
        }
        if projection.environment_spec.security.root_filesystem_policy
            != RootFilesystemPolicy::ReadOnlyRequired
        {
            return Err(ReleaseProjectionError::SecurityPostureInvalid);
        }
        let cpu = format!("{}m", resources.cpu_millicores);
        let memory = resources.memory_bytes.to_string();
        let storage = resources.storage_bytes.to_string();
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
                    "metadata":{"name":"runtime-quota","namespace":namespace,"labels":labels},
                    "spec":{"hard":{"requests.cpu":cpu,"limits.cpu":cpu,"requests.memory":memory,"limits.memory":memory,"requests.storage":storage,"persistentvolumeclaims":"1","pods":"1"}}
                }),
            ),
            resource(
                "LimitRange",
                Some(&namespace),
                "runtime-limits",
                json!({
                    "apiVersion":"v1","kind":"LimitRange",
                    "metadata":{"name":"runtime-limits","namespace":namespace,"labels":labels},
                    "spec":{"limits":[{"type":"Container","default":{"cpu":cpu,"memory":memory},"defaultRequest":{"cpu":cpu,"memory":memory}}]}
                }),
            ),
            resource(
                "ServiceAccount",
                Some(&namespace),
                "runtime",
                json!({
                    "apiVersion":"v1","kind":"ServiceAccount",
                    "metadata":{"name":"runtime","namespace":namespace,"labels":labels},
                    "automountServiceAccountToken":false,
                    "imagePullSecrets":[{"name":self.image_pull_secret_name}]
                }),
            ),
            resource(
                "PersistentVolumeClaim",
                Some(&namespace),
                "workspace",
                json!({
                    "apiVersion":"v1","kind":"PersistentVolumeClaim",
                    "metadata":{"name":"workspace","namespace":namespace,"labels":labels},
                    "spec":{"accessModes":["ReadWriteMany"],"storageClassName":self.workspace_storage_class_name,"resources":{"requests":{"storage":storage}}}
                }),
            ),
            resource(
                "NetworkPolicy",
                Some(&namespace),
                "default-deny-ingress",
                json!({
                    "apiVersion":"networking.k8s.io/v1","kind":"NetworkPolicy",
                    "metadata":{"name":"default-deny-ingress","namespace":namespace,"labels":labels},
                    "spec":{"podSelector":{},"policyTypes":["Ingress"]}
                }),
            ),
            resource(
                "NetworkPolicy",
                Some(&namespace),
                "access-service-ingress",
                json!({
                    "apiVersion":"networking.k8s.io/v1","kind":"NetworkPolicy",
                    "metadata":{"name":"access-service-ingress","namespace":namespace,"labels":labels},
                    "spec":{"podSelector":{"matchLabels":{"app":app_name}},"policyTypes":["Ingress"],"ingress":[{"from":[{"namespaceSelector":{"matchLabels":{"kubernetes.io/metadata.name":self.access_namespace}},"podSelector":{"matchLabels":{"app.kubernetes.io/name":self.access_pod_label}}}],"ports":[{"protocol":"TCP","port":service_port}]}]}
                }),
            ),
            resource(
                "Deployment",
                Some(&namespace),
                app_name,
                json!({
                    "apiVersion":"apps/v1","kind":"Deployment",
                    "metadata":{"name":app_name,"namespace":namespace,"labels":labels},
                    "spec":{
                        "replicas":1,
                        "selector":{"matchLabels":{"app":app_name}},
                        "template":{
                            "metadata":{"labels":{"app":app_name,"labweaver.io/environment-id":instance.id.to_string()}},
                            "spec":{
                                "serviceAccountName":"runtime","automountServiceAccountToken":false,
                                "securityContext":{"runAsNonRoot":true,"seccompProfile":{"type":"RuntimeDefault"}},
                                "containers":[{
                                    "name":"runtime","image":image,"imagePullPolicy":"IfNotPresent",
                                    "ports":[{"name":"service","containerPort":service_port}],
                                    "resources":{"requests":{"cpu":cpu,"memory":memory},"limits":{"cpu":cpu,"memory":memory}},
                                    "securityContext":{"allowPrivilegeEscalation":false,"readOnlyRootFilesystem":true,"runAsNonRoot":true,"capabilities":{"drop":["ALL"]}},
                                    "volumeMounts":[{"name":"workspace","mountPath":"/workspace"}],
                                    "readinessProbe":{"tcpSocket":{"port":"service"},"periodSeconds":2,"failureThreshold":30}
                                }],
                                "volumes":[{"name":"workspace","persistentVolumeClaim":{"claimName":"workspace"}}]
                            }
                        }
                    }
                }),
            ),
            resource(
                "Service",
                Some(&namespace),
                app_name,
                json!({
                    "apiVersion":"v1","kind":"Service",
                    "metadata":{"name":app_name,"namespace":namespace,"labels":labels},
                    "spec":{"type":"ClusterIP","selector":{"app":app_name},"ports":[{"name":"http","port":8080,"targetPort":"service"}]}
                }),
            ),
        ];
        match &projection.environment_spec.network {
            NetworkPolicySpec::AllowAll => {}
            NetworkPolicySpec::DenyAll => {
                documents.push(resource("NetworkPolicy", Some(&namespace), "deny-all-egress", json!({
                    "apiVersion":"networking.k8s.io/v1","kind":"NetworkPolicy",
                    "metadata":{"name":"deny-all-egress","namespace":namespace,"labels":labels},
                    "spec":{"podSelector":{"matchLabels":{"app":app_name}},"policyTypes":["Egress"]}
                })));
            }
            NetworkPolicySpec::Restricted { policy_binding } => {
                documents.push(resource("NetworkPolicy", Some(&namespace), "restricted-egress", json!({
                    "apiVersion":"networking.k8s.io/v1","kind":"NetworkPolicy",
                    "metadata":{"name":"restricted-egress","namespace":namespace,"labels":labels},
                    "spec":{"podSelector":{"matchLabels":{"app":app_name}},"policyTypes":["Egress"],"egress":[{"to":[{"namespaceSelector":{"matchLabels":{"labweaver.io/egress-policy":policy_binding}}}]}]}
                })));
            }
        }
        let plan_sha256 = canonical_hash(&json!({
            "environmentId": instance.id,
            "releaseId": projection.release.id,
            "releaseVersion": projection.release.version,
            "image": image,
            "resources": documents,
        }))?;
        Ok(ContainerResourcePlan {
            environment_id: instance.id,
            namespace,
            image,
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
        let evaluation = release
            .image_policy_evaluation
            .as_ref()
            .ok_or(ReleaseProjectionError::ContractInvalid)?;
        if resolved.authority_now >= evaluation.valid_until {
            return Err(ReleaseProjectionError::EvidenceExpired);
        }
        if evaluation.policy_id != self.release_policy.image_policy_id
            || evaluation.policy_revision != self.release_policy.image_policy_revision
            || release.approval.trust_revision != self.release_policy.trust_revision
        {
            return Err(ReleaseProjectionError::TrustRevisionMismatch);
        }
        Ok(())
    }

    fn cleanup_plan(
        &self,
        instance: &EnvironmentInstance,
    ) -> Result<ContainerResourcePlan, ReleaseProjectionError> {
        if instance.runtime_kind != RuntimeKind::Container
            || instance.provider_binding != self.binding
        {
            return Err(ReleaseProjectionError::IdentityMismatch);
        }
        let namespace = format!("lw-env-{}", instance.id);
        let plan_sha256 = canonical_hash(&json!({
            "environmentId": instance.id,
            "namespace": namespace,
            "action": "cleanup",
        }))?;
        Ok(ContainerResourcePlan {
            environment_id: instance.id,
            namespace,
            image: String::new(),
            resources: Vec::new(),
            plan_sha256,
        })
    }
}

#[async_trait]
impl<B, R> EnvironmentProvider for ContainerProvider<B, R>
where
    B: ContainerProviderBackend,
    R: ContainerReleaseResolver,
{
    fn binding(&self) -> &str {
        &self.binding
    }

    async fn execute(
        &self,
        action: ReconcileAction,
        instance: &EnvironmentInstance,
    ) -> Result<ProviderObservation, ProviderFailure> {
        let fence = ContainerBackendFence::for_action(instance, action)?;
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
                ready_observation(instance, observed)
            }
            (ReconcileAction::Observe, _) => {
                let observed = self.backend.observe(&fence, &plan).await?;
                ready_observation(instance, observed)
            }
            (ReconcileAction::Start, ObservedEnvironmentState::Stopped) => {
                let observed = self.backend.scale(&fence, &plan, 1).await?;
                ready_observation(instance, observed)
            }
            (ReconcileAction::Restart, ObservedEnvironmentState::Provisioning) => {
                let observed = self
                    .backend
                    .restart(&fence, &plan, instance.revision)
                    .await?;
                ready_observation(instance, observed)
            }
            (
                ReconcileAction::Stop,
                ObservedEnvironmentState::Stopping | ObservedEnvironmentState::Expiring,
            ) => {
                self.backend.scale(&fence, &plan, 0).await?;
                Ok(no_endpoints(ObservedEnvironmentState::Stopped, true))
            }
            _ => Err(ProviderFailure {
                code: ProviderFailureCode::Rejected,
                retryable: false,
            }),
        }
    }
}

fn ready_observation(
    instance: &EnvironmentInstance,
    observed: ContainerApplyObservation,
) -> Result<ProviderObservation, ProviderFailure> {
    if !observed.ready {
        return Ok(ProviderObservation {
            next_state: ObservedEnvironmentState::Provisioning,
            endpoints: Vec::new(),
            cleanup_evidence: None,
            operation_complete: false,
        });
    }
    let revision = instance
        .revision
        .get()
        .checked_add(1)
        .and_then(|value| Revision::new(value).ok())
        .ok_or(ProviderFailure {
            code: ProviderFailureCode::ObservationInvalid,
            retryable: false,
        })?;
    Ok(ProviderObservation {
        next_state: ObservedEnvironmentState::Ready,
        endpoints: vec![EnvironmentEndpoint {
            id: deterministic_endpoint_id(instance.id)?,
            protocol: contracts::environment::EndpointProtocol::Https,
            revision,
            health: EndpointHealth::Healthy,
            ssh_host_key_identity_sha256: None,
            observed_at: observed.observed_at,
        }],
        cleanup_evidence: None,
        operation_complete: true,
    })
}

fn deterministic_endpoint_id(
    environment_id: contracts::EnvironmentId,
) -> Result<EndpointId, ProviderFailure> {
    let mut bytes = *environment_id.as_uuid().as_bytes();
    bytes[15] ^= 1;
    EndpointId::from_str(&uuid::Uuid::from_bytes(bytes).to_string())
        .map_err(|_| invalid_observation())
}

fn resource(kind: &str, namespace: Option<&str>, name: &str, document: Value) -> ContainerResource {
    ContainerResource {
        kind: kind.to_owned(),
        namespace: namespace.map(str::to_owned),
        name: name.to_owned(),
        document,
    }
}

fn container_provider_binding(
    projection: &ReleasePublished,
) -> Result<&str, ReleaseProjectionError> {
    match &projection.environment_spec.runtime {
        EnvironmentRuntimeSpec::Container {
            provider_binding, ..
        } => Ok(provider_binding),
        EnvironmentRuntimeSpec::VirtualMachine { .. } => {
            Err(ReleaseProjectionError::IdentityMismatch)
        }
    }
}

fn projection_failure(error: &ReleaseProjectionError) -> ProviderFailure {
    match error {
        ReleaseProjectionError::Database(_) | ReleaseProjectionError::PersistenceFailed => {
            ProviderFailure {
                code: ProviderFailureCode::Unavailable,
                retryable: true,
            }
        }
        ReleaseProjectionError::NotFound => ProviderFailure {
            code: ProviderFailureCode::Unavailable,
            retryable: true,
        },
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

fn canonical_hash<T: Serialize>(value: &T) -> Result<Sha256Digest, ReleaseProjectionError> {
    Sha256Digest::of_canonical(value).map_err(|_| ReleaseProjectionError::ContractInvalid)
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

fn valid_dns_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && !value.starts_with('-')
        && !value.ends_with('-')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn is_unique_violation(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .is_some_and(|code| code == "23505")
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

#[derive(Debug, thiserror::Error)]
pub enum ReleaseProjectionError {
    #[error("LW_ENVIRONMENT_CONTAINER_CONFIGURATION_INVALID")]
    ConfigurationInvalid,
    #[error("LW_ENVIRONMENT_RELEASE_NOT_FOUND")]
    NotFound,
    #[error("LW_ENVIRONMENT_RELEASE_CONTRACT_INVALID")]
    ContractInvalid,
    #[error("LW_ENVIRONMENT_RELEASE_IDENTITY_MISMATCH")]
    IdentityMismatch,
    #[error("LW_ENVIRONMENT_CONTAINER_SECURITY_POSTURE_INVALID")]
    SecurityPostureInvalid,
    #[error("LW_ENVIRONMENT_RELEASE_WITHDRAWN")]
    Withdrawn,
    #[error("LW_ENVIRONMENT_RELEASE_EVIDENCE_EXPIRED")]
    EvidenceExpired,
    #[error("LW_ENVIRONMENT_RELEASE_TRUST_REVISION_MISMATCH")]
    TrustRevisionMismatch,
    #[error("LW_ENVIRONMENT_RELEASE_PERSISTENCE_FAILED")]
    PersistenceFailed,
    #[error("LW_ENVIRONMENT_RELEASE_DATABASE_FAILED")]
    Database(#[from] sqlx::Error),
}
