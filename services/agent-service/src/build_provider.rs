//! Typed NATS adapter for the deployment-owned BuildKit/Harbor/Trivy executor.
#![allow(
    missing_docs,
    reason = "the build pipeline trait and v1 event contracts document the integration semantics"
)]

use async_trait::async_trait;
use contracts::BuildRequestId;
use contracts::events::AgentBuildRequested;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{PgPool, Row};
use std::time::Duration;

use crate::build_pipeline::{
    BuildIdentity, BuildProviderFailure, BuildProviderFailureCode, BuildProviderRequestContext,
    BuildProviderStage, BuildSupplyChainProvider, BuiltCandidate, PrivateRegistryProject,
    PublishedImage, ScanEvidence,
};

const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

/// Exact provider binding and request subject; no fallback subject is permitted.
pub struct NatsBuildSupplyChainProvider {
    client: async_nats::Client,
    subject: String,
    builder_binding: String,
    scanner_binding: String,
    registry_binding: String,
    request_timeout: Duration,
}

impl NatsBuildSupplyChainProvider {
    pub fn new(
        client: async_nats::Client,
        subject: String,
        builder_binding: String,
        scanner_binding: String,
        registry_binding: String,
        request_timeout: Duration,
    ) -> Result<Self, BuildProviderFailure> {
        if !valid_subject(&subject)
            || request_timeout.is_zero()
            || request_timeout > Duration::from_hours(1)
            || [
                builder_binding.as_str(),
                scanner_binding.as_str(),
                registry_binding.as_str(),
            ]
            .iter()
            .any(|binding| !valid_token(binding))
        {
            return Err(configuration_failure());
        }
        Ok(Self {
            client,
            subject,
            builder_binding,
            scanner_binding,
            registry_binding,
            request_timeout,
        })
    }

    async fn request(
        &self,
        context: &BuildProviderRequestContext,
        request: BuildExecutorRequest,
    ) -> Result<BuildExecutorResponse, BuildProviderFailure> {
        if context.stage != request.stage() {
            return Err(identity_mismatch());
        }
        let context = bind_build_executor_request(*context, &request)?;
        let payload = serde_json::to_vec(&BuildExecutorRequestEnvelope { context, request })
            .map_err(|_| output_invalid())?;
        let request = async_nats::Request::new()
            .timeout(Some(self.request_timeout))
            .payload(payload.into());
        let message = self
            .client
            .send_request(self.subject.clone(), request)
            .await
            .map_err(|_| unavailable())?;
        if message.payload.len() > MAX_RESPONSE_BYTES {
            return Err(output_invalid());
        }
        let response: BuildExecutorResponseEnvelope =
            serde_json::from_slice(&message.payload).map_err(|_| output_invalid())?;
        if response.protocol_version != context.protocol_version
            || response.build_request_id != context.build_request_id
            || response.fence_generation != context.fence_generation
            || response.stage != context.stage
            || response.stage_request_id != context.stage_request_id
        {
            return Err(identity_mismatch());
        }
        Ok(response.response)
    }
}

#[async_trait]
impl BuildSupplyChainProvider for NatsBuildSupplyChainProvider {
    fn builder_binding(&self) -> &str {
        &self.builder_binding
    }

    fn scanner_binding(&self) -> &str {
        &self.scanner_binding
    }

    fn registry_binding(&self) -> &str {
        &self.registry_binding
    }

    async fn ensure_private_project(
        &self,
        context: &BuildProviderRequestContext,
        command: &AgentBuildRequested,
        identity: BuildIdentity,
    ) -> Result<PrivateRegistryProject, BuildProviderFailure> {
        match self
            .request(
                context,
                BuildExecutorRequest::EnsurePrivateProject {
                    command: command.clone(),
                    identity,
                },
            )
            .await?
        {
            BuildExecutorResponse::PrivateProjectReady { project }
                if project.build_request_id == command.request.id
                    && project.build_identity == identity =>
            {
                Ok(project)
            }
            BuildExecutorResponse::Failed { failure } => Err(failure),
            _ => Err(identity_mismatch()),
        }
    }

    async fn build_candidate(
        &self,
        context: &BuildProviderRequestContext,
        command: &AgentBuildRequested,
        identity: BuildIdentity,
    ) -> Result<BuiltCandidate, BuildProviderFailure> {
        match self
            .request(
                context,
                BuildExecutorRequest::Build {
                    command: command.clone(),
                    identity,
                },
            )
            .await?
        {
            BuildExecutorResponse::Built { candidate }
                if candidate.build_request_id == command.request.id
                    && candidate.build_identity == identity =>
            {
                Ok(candidate)
            }
            BuildExecutorResponse::Failed { failure } => Err(failure),
            _ => Err(identity_mismatch()),
        }
    }

    async fn scan_candidate(
        &self,
        context: &BuildProviderRequestContext,
        candidate: &BuiltCandidate,
    ) -> Result<ScanEvidence, BuildProviderFailure> {
        match self
            .request(
                context,
                BuildExecutorRequest::Scan {
                    candidate: candidate.clone(),
                },
            )
            .await?
        {
            BuildExecutorResponse::Scanned { evidence }
                if evidence.build_identity == candidate.build_identity
                    && evidence.digest == candidate.digest =>
            {
                Ok(evidence)
            }
            BuildExecutorResponse::Failed { failure } => Err(failure),
            _ => Err(identity_mismatch()),
        }
    }

    async fn publish_immutable(
        &self,
        context: &BuildProviderRequestContext,
        candidate: &BuiltCandidate,
    ) -> Result<PublishedImage, BuildProviderFailure> {
        match self
            .request(
                context,
                BuildExecutorRequest::Publish {
                    candidate: candidate.clone(),
                },
            )
            .await?
        {
            BuildExecutorResponse::Published { image }
                if image.build_identity == candidate.build_identity
                    && image.digest == candidate.digest =>
            {
                Ok(image)
            }
            BuildExecutorResponse::Failed { failure } => Err(failure),
            _ => Err(identity_mismatch()),
        }
    }

    async fn cleanup_candidate(
        &self,
        context: &BuildProviderRequestContext,
        build_request_id: BuildRequestId,
        identity: BuildIdentity,
    ) -> Result<(), BuildProviderFailure> {
        match self
            .request(
                context,
                BuildExecutorRequest::Cleanup {
                    build_request_id,
                    identity,
                },
            )
            .await?
        {
            BuildExecutorResponse::Cleaned {
                build_request_id: observed_request_id,
                build_identity,
            } if observed_request_id == build_request_id && build_identity == identity => Ok(()),
            BuildExecutorResponse::Failed { failure } => Err(failure),
            _ => Err(identity_mismatch()),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BuildExecutorRequestEnvelope {
    #[serde(flatten)]
    pub context: BuildProviderRequestContext,
    pub request: BuildExecutorRequest,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum BuildExecutorRequest {
    EnsurePrivateProject {
        command: AgentBuildRequested,
        identity: BuildIdentity,
    },
    Build {
        command: AgentBuildRequested,
        identity: BuildIdentity,
    },
    Scan {
        candidate: BuiltCandidate,
    },
    Publish {
        candidate: BuiltCandidate,
    },
    Cleanup {
        build_request_id: BuildRequestId,
        identity: BuildIdentity,
    },
}

impl BuildExecutorRequest {
    const fn stage(&self) -> BuildProviderStage {
        match self {
            Self::EnsurePrivateProject { .. } => BuildProviderStage::EnsurePrivateProject,
            Self::Build { .. } => BuildProviderStage::Build,
            Self::Scan { .. } => BuildProviderStage::Scan,
            Self::Publish { .. } => BuildProviderStage::Publish,
            Self::Cleanup { .. } => BuildProviderStage::Cleanup,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BuildExecutorResponseEnvelope {
    pub protocol_version: u8,
    pub build_request_id: BuildRequestId,
    pub fence_generation: u32,
    pub stage: BuildProviderStage,
    pub stage_request_id: contracts::Sha256Digest,
    pub response: BuildExecutorResponse,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum BuildExecutorResponse {
    PrivateProjectReady {
        project: PrivateRegistryProject,
    },
    Built {
        candidate: BuiltCandidate,
    },
    Scanned {
        evidence: ScanEvidence,
    },
    Published {
        image: PublishedImage,
    },
    Cleaned {
        build_request_id: BuildRequestId,
        build_identity: BuildIdentity,
    },
    Failed {
        failure: BuildProviderFailure,
    },
}

/// Side-effect adapter used only after the durable executor fence admits a request.
#[async_trait]
pub trait BuildExecutorBackend: Send + Sync {
    async fn execute(
        &self,
        context: &BuildProviderRequestContext,
        request: &BuildExecutorRequest,
    ) -> BuildExecutorResponse;
}

/// Executor-side `PostgreSQL` fence and response ledger.
#[derive(Clone, Debug)]
pub struct PgBuildExecutorFenceStore {
    pool: PgPool,
}

impl PgBuildExecutorFenceStore {
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
        envelope: &BuildExecutorRequestEnvelope,
    ) -> Result<BuildExecutorAdmission, BuildExecutorFenceError> {
        validate_executor_request(envelope)?;
        let context = envelope.context;
        let mut transaction = self.pool.begin().await?;
        let authority_now: time::OffsetDateTime =
            sqlx::query_scalar("SELECT date_trunc('milliseconds',clock_timestamp())")
                .fetch_one(&mut *transaction)
                .await?;
        if authority_now >= context.deadline_at.get() {
            return Err(BuildExecutorFenceError::DeadlineExceeded);
        }
        let remaining = std::time::Duration::try_from(context.deadline_at.get() - authority_now)
            .map_err(|_| BuildExecutorFenceError::DeadlineExceeded)?;
        let current = sqlx::query(
            "SELECT highest_generation,lease_token,tombstone_generation,last_stage_rank, \
                    last_request_id,last_response,deadline_at \
             FROM agent.build_executor_fences WHERE build_request_id=$1 FOR UPDATE",
        )
        .bind(context.build_request_id.as_uuid())
        .fetch_optional(&mut *transaction)
        .await?;
        let stage_rank = build_stage_rank(context.stage);
        if let Some(row) = current {
            let highest_generation = u32::try_from(row.try_get::<i32, _>("highest_generation")?)
                .map_err(|_| BuildExecutorFenceError::IdentityMismatch)?;
            let lease_token: uuid::Uuid = row.try_get("lease_token")?;
            let tombstone_generation = row
                .try_get::<Option<i32>, _>("tombstone_generation")?
                .map(u32::try_from)
                .transpose()
                .map_err(|_| BuildExecutorFenceError::IdentityMismatch)?;
            let last_stage_rank: i16 = row.try_get("last_stage_rank")?;
            let last_request_id: String = row.try_get("last_request_id")?;
            let last_response = row.try_get::<Option<Value>, _>("last_response")?;
            let previous_deadline: time::OffsetDateTime = row.try_get("deadline_at")?;
            if context.fence_generation < highest_generation
                || (context.fence_generation == highest_generation
                    && context.lease_token != lease_token)
            {
                return Err(BuildExecutorFenceError::StaleGeneration);
            }
            if context.fence_generation == highest_generation
                && last_request_id == context.stage_request_id.to_string()
            {
                if let Some(value) = last_response {
                    transaction.rollback().await?;
                    return Ok(BuildExecutorAdmission::Replay(value));
                }
                return Err(BuildExecutorFenceError::InProgress);
            }
            if last_response.is_none() && authority_now < previous_deadline {
                return Err(BuildExecutorFenceError::InProgress);
            }
            if context.fence_generation == highest_generation
                && (tombstone_generation == Some(highest_generation)
                    || (stage_rank < last_stage_rank
                        && context.stage != BuildProviderStage::Cleanup))
            {
                return Err(BuildExecutorFenceError::Tombstoned);
            }
            sqlx::query(
                "UPDATE agent.build_executor_fences SET highest_generation=$2,lease_token=$3, \
                     tombstone_generation=$4,last_stage=$5,last_stage_rank=$6,last_request_id=$7, \
                     last_response=NULL,deadline_at=$8,updated_at=clock_timestamp() \
                 WHERE build_request_id=$1",
            )
            .bind(context.build_request_id.as_uuid())
            .bind(
                i32::try_from(context.fence_generation)
                    .map_err(|_| BuildExecutorFenceError::IdentityMismatch)?,
            )
            .bind(context.lease_token)
            .bind(
                (context.stage == BuildProviderStage::Cleanup).then_some(
                    i32::try_from(context.fence_generation)
                        .map_err(|_| BuildExecutorFenceError::IdentityMismatch)?,
                ),
            )
            .bind(build_stage_name(context.stage))
            .bind(stage_rank)
            .bind(context.stage_request_id.to_string())
            .bind(context.deadline_at.get())
            .execute(&mut *transaction)
            .await?;
        } else {
            sqlx::query(
                "INSERT INTO agent.build_executor_fences \
                 (build_request_id,highest_generation,lease_token,tombstone_generation,last_stage, \
                  last_stage_rank,last_request_id,deadline_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
            )
            .bind(context.build_request_id.as_uuid())
            .bind(
                i32::try_from(context.fence_generation)
                    .map_err(|_| BuildExecutorFenceError::IdentityMismatch)?,
            )
            .bind(context.lease_token)
            .bind(
                (context.stage == BuildProviderStage::Cleanup).then_some(
                    i32::try_from(context.fence_generation)
                        .map_err(|_| BuildExecutorFenceError::IdentityMismatch)?,
                ),
            )
            .bind(build_stage_name(context.stage))
            .bind(stage_rank)
            .bind(context.stage_request_id.to_string())
            .bind(context.deadline_at.get())
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(BuildExecutorAdmission::Execute(remaining))
    }

    async fn complete(
        &self,
        context: BuildProviderRequestContext,
        response: &BuildExecutorResponse,
    ) -> Result<(), BuildExecutorFenceError> {
        let value = serde_json::to_value(response)
            .map_err(|_| BuildExecutorFenceError::IdentityMismatch)?;
        let updated = sqlx::query(
            "UPDATE agent.build_executor_fences SET last_response=$5,updated_at=clock_timestamp() \
             WHERE build_request_id=$1 AND highest_generation=$2 AND lease_token=$3 \
               AND last_request_id=$4 AND last_response IS NULL",
        )
        .bind(context.build_request_id.as_uuid())
        .bind(
            i32::try_from(context.fence_generation)
                .map_err(|_| BuildExecutorFenceError::IdentityMismatch)?,
        )
        .bind(context.lease_token)
        .bind(context.stage_request_id.to_string())
        .bind(value)
        .execute(&self.pool)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(BuildExecutorFenceError::StaleGeneration);
        }
        Ok(())
    }
}

enum BuildExecutorAdmission {
    Execute(std::time::Duration),
    Replay(Value),
}

/// Server-side executor wrapper that never calls a side-effect adapter before durable admission.
pub struct FencedBuildExecutor<B> {
    store: PgBuildExecutorFenceStore,
    backend: B,
}

impl<B: BuildExecutorBackend> FencedBuildExecutor<B> {
    #[must_use]
    pub const fn new(store: PgBuildExecutorFenceStore, backend: B) -> Self {
        Self { store, backend }
    }

    pub async fn execute(
        &self,
        envelope: BuildExecutorRequestEnvelope,
    ) -> Result<BuildExecutorResponseEnvelope, BuildExecutorFenceError> {
        let response = match self.store.admit(&envelope).await? {
            BuildExecutorAdmission::Execute(remaining) => {
                let response = tokio::time::timeout(
                    remaining,
                    self.backend.execute(&envelope.context, &envelope.request),
                )
                .await
                .map_err(|_| BuildExecutorFenceError::DeadlineExceeded)?;
                self.store.complete(envelope.context, &response).await?;
                response
            }
            BuildExecutorAdmission::Replay(value) => serde_json::from_value(value)
                .map_err(|_| BuildExecutorFenceError::IdentityMismatch)?,
        };
        Ok(BuildExecutorResponseEnvelope {
            protocol_version: envelope.context.protocol_version,
            build_request_id: envelope.context.build_request_id,
            fence_generation: envelope.context.fence_generation,
            stage: envelope.context.stage,
            stage_request_id: envelope.context.stage_request_id,
            response,
        })
    }
}

/// NATS request/reply server used by the deployment-owned build executor process.
pub struct NatsBuildExecutorServer<B> {
    client: async_nats::Client,
    subject: String,
    executor: std::sync::Arc<FencedBuildExecutor<B>>,
}

impl<B: BuildExecutorBackend + 'static> NatsBuildExecutorServer<B> {
    pub fn new(
        client: async_nats::Client,
        subject: String,
        executor: FencedBuildExecutor<B>,
    ) -> Result<Self, BuildExecutorFenceError> {
        if !valid_subject(&subject) {
            return Err(BuildExecutorFenceError::ConfigurationInvalid);
        }
        Ok(Self {
            client,
            subject,
            executor: std::sync::Arc::new(executor),
        })
    }

    /// Serves bounded requests; malformed/no-reply messages are rejected without side effects.
    pub async fn serve(self) -> Result<(), BuildExecutorFenceError> {
        let mut subscriber = self
            .client
            .subscribe(self.subject)
            .await
            .map_err(|_| BuildExecutorFenceError::Transport)?;
        while let Some(message) = subscriber.next().await {
            let Some(reply) = message.reply.clone() else {
                tracing::warn!(
                    event = "agent.build_executor.request_rejected",
                    diagnostic = "LW_AGENT_BUILD_EXECUTOR_REPLY_REQUIRED"
                );
                continue;
            };
            if message.payload.len() > MAX_RESPONSE_BYTES {
                tracing::warn!(
                    event = "agent.build_executor.request_rejected",
                    diagnostic = "LW_AGENT_BUILD_EXECUTOR_PAYLOAD_TOO_LARGE"
                );
                continue;
            }
            let Ok(envelope) =
                serde_json::from_slice::<BuildExecutorRequestEnvelope>(&message.payload)
            else {
                tracing::warn!(
                    event = "agent.build_executor.request_rejected",
                    diagnostic = "LW_AGENT_BUILD_EXECUTOR_CONTRACT_INVALID"
                );
                continue;
            };
            let client = self.client.clone();
            let executor = std::sync::Arc::clone(&self.executor);
            tokio::spawn(async move {
                let context = envelope.context;
                let response = match executor.execute(envelope).await {
                    Ok(response) => response,
                    Err(error) => BuildExecutorResponseEnvelope {
                        protocol_version: context.protocol_version,
                        build_request_id: context.build_request_id,
                        fence_generation: context.fence_generation,
                        stage: context.stage,
                        stage_request_id: context.stage_request_id,
                        response: BuildExecutorResponse::Failed {
                            failure: executor_failure(&error),
                        },
                    },
                };
                let Ok(payload) = serde_json::to_vec(&response) else {
                    tracing::error!(
                        event = "agent.build_executor.response_failed",
                        diagnostic = "LW_AGENT_BUILD_EXECUTOR_RESPONSE_INVALID"
                    );
                    return;
                };
                if client.publish(reply, payload.into()).await.is_err() {
                    tracing::warn!(
                        event = "agent.build_executor.response_failed",
                        diagnostic = "LW_AGENT_BUILD_EXECUTOR_TRANSPORT_FAILED"
                    );
                }
            });
        }
        Err(BuildExecutorFenceError::Transport)
    }
}

const fn executor_failure(error: &BuildExecutorFenceError) -> BuildProviderFailure {
    match error {
        BuildExecutorFenceError::InProgress
        | BuildExecutorFenceError::Database(_)
        | BuildExecutorFenceError::Transport => BuildProviderFailure {
            code: BuildProviderFailureCode::Unavailable,
            retryable: true,
        },
        BuildExecutorFenceError::IdentityMismatch => BuildProviderFailure {
            code: BuildProviderFailureCode::IdentityMismatch,
            retryable: false,
        },
        BuildExecutorFenceError::ConfigurationInvalid
        | BuildExecutorFenceError::DeadlineExceeded
        | BuildExecutorFenceError::StaleGeneration
        | BuildExecutorFenceError::Tombstoned => BuildProviderFailure {
            code: BuildProviderFailureCode::Rejected,
            retryable: false,
        },
    }
}

fn validate_executor_request(
    envelope: &BuildExecutorRequestEnvelope,
) -> Result<(), BuildExecutorFenceError> {
    let context = envelope.context;
    let expected_request_id = build_executor_request_id(context, &envelope.request)?;
    if context.protocol_version != crate::build_pipeline::BUILD_EXECUTOR_PROTOCOL_VERSION
        || context.fence_generation == 0
        || context.stage != envelope.request.stage()
        || context.stage_request_id != expected_request_id
        || executor_request_build_id(&envelope.request) != context.build_request_id
        || !executor_request_identity_valid(&envelope.request)
    {
        return Err(BuildExecutorFenceError::IdentityMismatch);
    }
    Ok(())
}

fn executor_request_identity_valid(request: &BuildExecutorRequest) -> bool {
    match request {
        BuildExecutorRequest::EnsurePrivateProject { command, identity }
        | BuildExecutorRequest::Build { command, identity } => {
            command.validate().is_ok() && *identity == BuildIdentity(command.command_sha256)
        }
        BuildExecutorRequest::Scan { candidate } | BuildExecutorRequest::Publish { candidate } => {
            candidate.digest.starts_with("sha256:")
                && candidate.digest.len() == 71
                && candidate
                    .digest
                    .strip_prefix("sha256:")
                    .is_some_and(|digest| digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
        }
        BuildExecutorRequest::Cleanup { .. } => true,
    }
}

fn bind_build_executor_request(
    mut context: BuildProviderRequestContext,
    request: &BuildExecutorRequest,
) -> Result<BuildProviderRequestContext, BuildProviderFailure> {
    context.stage_request_id =
        build_executor_request_id(context, request).map_err(|_| output_invalid())?;
    Ok(context)
}

fn build_executor_request_id(
    context: BuildProviderRequestContext,
    request: &BuildExecutorRequest,
) -> Result<contracts::Sha256Digest, BuildExecutorFenceError> {
    contracts::Sha256Digest::of_canonical(&serde_json::json!({
        "protocolVersion": context.protocol_version,
        "buildRequestId": context.build_request_id,
        "fenceGeneration": context.fence_generation,
        "leaseToken": context.lease_token,
        "stage": context.stage,
        "deadlineAt": context.deadline_at,
        "request": request,
    }))
    .map_err(|_| BuildExecutorFenceError::IdentityMismatch)
}

const fn executor_request_build_id(request: &BuildExecutorRequest) -> BuildRequestId {
    match request {
        BuildExecutorRequest::EnsurePrivateProject { command, .. }
        | BuildExecutorRequest::Build { command, .. } => command.request.id,
        BuildExecutorRequest::Scan { candidate } | BuildExecutorRequest::Publish { candidate } => {
            candidate.build_request_id
        }
        BuildExecutorRequest::Cleanup {
            build_request_id, ..
        } => *build_request_id,
    }
}

const fn build_stage_rank(stage: BuildProviderStage) -> i16 {
    match stage {
        BuildProviderStage::EnsurePrivateProject => 1,
        BuildProviderStage::Build => 2,
        BuildProviderStage::Scan => 3,
        BuildProviderStage::Publish => 4,
        BuildProviderStage::Cleanup => 5,
    }
}

const fn build_stage_name(stage: BuildProviderStage) -> &'static str {
    match stage {
        BuildProviderStage::EnsurePrivateProject => "ensure_private_project",
        BuildProviderStage::Build => "build",
        BuildProviderStage::Scan => "scan",
        BuildProviderStage::Publish => "publish",
        BuildProviderStage::Cleanup => "cleanup",
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BuildExecutorFenceError {
    #[error("LW_AGENT_BUILD_EXECUTOR_CONFIGURATION_INVALID")]
    ConfigurationInvalid,
    #[error("LW_AGENT_BUILD_EXECUTOR_IDENTITY_MISMATCH")]
    IdentityMismatch,
    #[error("LW_AGENT_BUILD_EXECUTOR_DEADLINE_EXCEEDED")]
    DeadlineExceeded,
    #[error("LW_AGENT_BUILD_EXECUTOR_STALE_GENERATION")]
    StaleGeneration,
    #[error("LW_AGENT_BUILD_EXECUTOR_TOMBSTONED")]
    Tombstoned,
    #[error("LW_AGENT_BUILD_EXECUTOR_REQUEST_IN_PROGRESS")]
    InProgress,
    #[error("LW_AGENT_BUILD_EXECUTOR_DATABASE_FAILED")]
    Database(#[from] sqlx::Error),
    #[error("LW_AGENT_BUILD_EXECUTOR_TRANSPORT_FAILED")]
    Transport,
}

fn valid_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
}

fn valid_subject(value: &str) -> bool {
    valid_token(value) && !value.contains('*') && !value.contains('>')
}

const fn configuration_failure() -> BuildProviderFailure {
    BuildProviderFailure {
        code: BuildProviderFailureCode::Rejected,
        retryable: false,
    }
}

const fn unavailable() -> BuildProviderFailure {
    BuildProviderFailure {
        code: BuildProviderFailureCode::Unavailable,
        retryable: true,
    }
}

const fn identity_mismatch() -> BuildProviderFailure {
    BuildProviderFailure {
        code: BuildProviderFailureCode::IdentityMismatch,
        retryable: false,
    }
}

const fn output_invalid() -> BuildProviderFailure {
    BuildProviderFailure {
        code: BuildProviderFailureCode::OutputInvalid,
        retryable: false,
    }
}
