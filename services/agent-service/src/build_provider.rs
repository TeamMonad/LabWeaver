//! Typed NATS adapter for the deployment-owned BuildKit/Harbor executor.
#![allow(
    missing_docs,
    reason = "the build pipeline trait and v1 event contracts document the integration semantics"
)]

use async_trait::async_trait;
use contracts::BuildRequestId;
use contracts::events::AgentBuildRequested;
use futures_util::StreamExt;
use persistence_sqlx::Sha256Digest; // internal persistence hash, not contract hash
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{PgPool, Row};
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::build_pipeline::{
    BuildIdentity, BuildProviderFailure, BuildProviderFailureCode, BuildProviderRequestContext,
    BuildProviderStage, BuildSupplyChainProvider, BuiltCandidate, PrivateRegistryProject,
    PublishedImage,
};

const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

/// Exact provider binding and request subject; no fallback subject is permitted.
pub struct NatsBuildSupplyChainProvider {
    client: async_nats::Client,
    subject: String,
    builder_binding: String,
    registry_binding: String,
    request_timeout: Duration,
}

impl NatsBuildSupplyChainProvider {
    pub fn new(
        client: async_nats::Client,
        subject: String,
        builder_binding: String,
        registry_binding: String,
        request_timeout: Duration,
    ) -> Result<Self, BuildProviderFailure> {
        if !valid_subject(&subject)
            || request_timeout.is_zero()
            || request_timeout > Duration::from_hours(1)
            || [builder_binding.as_str(), registry_binding.as_str()]
                .iter()
                .any(|binding| !valid_token(binding))
        {
            return Err(configuration_failure());
        }
        Ok(Self {
            client,
            subject,
            builder_binding,
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
        let request_body_for_query = request.clone();
        let query_only = context.stage == BuildProviderStage::Build
            && context.deadline_at.get() <= time::OffsetDateTime::now_utc();
        let payload = serde_json::to_vec(&BuildExecutorRequestEnvelope {
            context,
            request,
            query_only,
        })
        .map_err(|_| output_invalid())?;
        let timeout = if query_only {
            self.request_timeout
        } else if matches!(
            context.stage,
            BuildProviderStage::Build | BuildProviderStage::Import
        ) {
            std::time::Duration::try_from(
                context.deadline_at.get() - time::OffsetDateTime::now_utc(),
            )
            .map_err(|_| unavailable())?
        } else {
            self.request_timeout
        };
        let request = async_nats::Request::new()
            .timeout(Some(timeout))
            .payload(payload.into());
        let mut queried = query_only;
        let message = match self
            .client
            .send_request(self.subject.clone(), request)
            .await
        {
            Ok(message) => message,
            Err(_) if matches!(context.stage, BuildProviderStage::Build) => {
                queried = true;
                let query_payload = serde_json::to_vec(&BuildExecutorRequestEnvelope {
                    context,
                    request: request_body_for_query.clone(),
                    query_only: true,
                })
                .map_err(|_| output_invalid())?;
                self.client
                    .send_request(
                        self.subject.clone(),
                        async_nats::Request::new()
                            .timeout(Some(self.request_timeout))
                            .payload(query_payload.into()),
                    )
                    .await
                    .map_err(|_| BuildProviderFailure {
                        code: BuildProviderFailureCode::ExecutionUnknown,
                        retryable: false,
                    })?
            }
            Err(_) => return Err(unavailable()),
        };
        let response = decode_executor_response(context, &message.payload)?;
        if !queried
            && context.stage == BuildProviderStage::Build
            && matches!(&response, BuildExecutorResponse::Failed { failure } if matches!(failure.code, BuildProviderFailureCode::ExecutionUnknown | BuildProviderFailureCode::Unavailable))
        {
            let payload = serde_json::to_vec(&BuildExecutorRequestEnvelope {
                context,
                request: request_body_for_query,
                query_only: true,
            })
            .map_err(|_| output_invalid())?;
            let message = self
                .client
                .send_request(
                    self.subject.clone(),
                    async_nats::Request::new()
                        .timeout(Some(self.request_timeout))
                        .payload(payload.into()),
                )
                .await
                .map_err(|_| BuildProviderFailure {
                    code: BuildProviderFailureCode::ExecutionUnknown,
                    retryable: false,
                })?;
            return decode_executor_response(context, &message.payload);
        }
        Ok(response)
    }
}

fn decode_executor_response(
    context: BuildProviderRequestContext,
    payload: &[u8],
) -> Result<BuildExecutorResponse, BuildProviderFailure> {
    if payload.len() > MAX_RESPONSE_BYTES {
        return Err(output_invalid());
    }
    let response: BuildExecutorResponseEnvelope =
        serde_json::from_slice(payload).map_err(|_| output_invalid())?;
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

#[async_trait]
impl BuildSupplyChainProvider for NatsBuildSupplyChainProvider {
    fn builder_binding(&self) -> &str {
        &self.builder_binding
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

    async fn import_candidate(
        &self,
        context: &BuildProviderRequestContext,
        command: &AgentBuildRequested,
        identity: BuildIdentity,
    ) -> Result<BuiltCandidate, BuildProviderFailure> {
        match self
            .request(
                context,
                BuildExecutorRequest::Import {
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
                    timeout_milliseconds: u64::try_from(self.request_timeout.as_millis())
                        .map_err(|_| configuration_failure())?,
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
    #[serde(default)]
    pub query_only: bool,
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
    Import {
        command: AgentBuildRequested,
        identity: BuildIdentity,
    },
    Publish {
        candidate: BuiltCandidate,
    },
    Cleanup {
        build_request_id: BuildRequestId,
        identity: BuildIdentity,
        timeout_milliseconds: u64,
    },
}

impl BuildExecutorRequest {
    const fn stage(&self) -> BuildProviderStage {
        match self {
            Self::EnsurePrivateProject { .. } => BuildProviderStage::EnsurePrivateProject,
            Self::Build { .. } => BuildProviderStage::Build,
            Self::Import { .. } => BuildProviderStage::Import,
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
    pub stage_request_id: Sha256Digest,
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
    async fn query_build_usage(
        &self,
        _context: &BuildProviderRequestContext,
        _request: &BuildExecutorRequest,
    ) -> contracts::resource::build_costing::BuildUsageMeasurement {
        contracts::resource::build_costing::BuildUsageMeasurement::Unknown {
            reason: "authoritative build history unavailable".into(),
        }
    }
    async fn execute(
        &self,
        context: &BuildProviderRequestContext,
        request: &BuildExecutorRequest,
        cancellation: &CancellationToken,
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

    async fn cached_response(
        &self,
        context: BuildProviderRequestContext,
    ) -> Result<Option<Value>, BuildExecutorFenceError> {
        Ok(sqlx::query_scalar("SELECT last_response FROM agent.build_executor_fences WHERE build_request_id=$1 AND highest_generation=$2 AND lease_token=$3 AND last_request_id=$4 AND deadline_at=$5").bind(context.build_request_id.as_uuid()).bind(i32::try_from(context.fence_generation).map_err(|_|BuildExecutorFenceError::IdentityMismatch)?).bind(context.lease_token).bind(context.stage_request_id.to_string()).bind(context.deadline_at.get()).fetch_optional(&self.pool).await?.flatten())
    }
    async fn preflight(
        &self,
        envelope: &BuildExecutorRequestEnvelope,
    ) -> Result<(), BuildExecutorFenceError> {
        let c = envelope.context;
        if c.deadline_at.get() <= time::OffsetDateTime::now_utc() {
            return Err(BuildExecutorFenceError::DeadlineExceeded);
        }

        let row = sqlx::query("SELECT highest_generation,lease_token,tombstone_generation,last_stage_rank,last_response,deadline_at FROM agent.build_executor_fences WHERE build_request_id=$1").bind(c.build_request_id.as_uuid()).fetch_optional(&self.pool).await?;
        if let Some(row) = row {
            let generation = u32::try_from(row.try_get::<i32, _>("highest_generation")?)
                .map_err(|_| BuildExecutorFenceError::IdentityMismatch)?;
            if c.fence_generation < generation {
                return Err(BuildExecutorFenceError::StaleGeneration);
            }
            if c.fence_generation == generation
                && (row.try_get::<uuid::Uuid, _>("lease_token")? != c.lease_token
                    || row.try_get::<time::OffsetDateTime, _>("deadline_at")?
                        != c.deadline_at.get())
            {
                return Err(BuildExecutorFenceError::IdentityMismatch);
            }
            if row.try_get::<Option<Value>, _>("last_response")?.is_none() {
                return Err(BuildExecutorFenceError::InProgress);
            }
            if c.fence_generation == generation
                && (row.try_get::<Option<i32>, _>("tombstone_generation")?
                    == Some(
                        i32::try_from(generation)
                            .map_err(|_| BuildExecutorFenceError::IdentityMismatch)?,
                    )
                    || build_stage_rank(c.stage) < row.try_get::<i16, _>("last_stage_rank")?)
            {
                return Err(BuildExecutorFenceError::Tombstoned);
            }
        }
        Ok(())
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
        let cleanup = context.stage == BuildProviderStage::Cleanup;
        if !cleanup && authority_now >= context.deadline_at.get() {
            return Err(BuildExecutorFenceError::DeadlineExceeded);
        }
        let remaining = if cleanup {
            cleanup_timeout(&envelope.request)?
        } else {
            std::time::Duration::try_from(context.deadline_at.get() - authority_now)
                .map_err(|_| BuildExecutorFenceError::DeadlineExceeded)?
        };
        let current = sqlx::query(
            "SELECT highest_generation,lease_token,tombstone_generation,last_stage_rank, \
                    last_request_id,last_response,deadline_at \
             FROM agent.build_executor_fences WHERE build_request_id=$1 FOR UPDATE",
        )
        .bind(context.build_request_id.as_uuid())
        .fetch_optional(&mut *transaction)
        .await?;
        if let BuildExecutorRequest::EnsurePrivateProject { command, .. }
        | BuildExecutorRequest::Build { command, .. } = &envelope.request
        {
            let live = sqlx::query("SELECT command,state,attempt,lease_token,lease_expires_at,cancellation_requested FROM agent.build_commands WHERE build_request_id=$1 FOR UPDATE").bind(context.build_request_id.as_uuid()).fetch_optional(&mut *transaction).await?.ok_or(BuildExecutorFenceError::IdentityMismatch)?;
            if live.try_get::<Value, _>("command")?
                != serde_json::to_value(command)
                    .map_err(|_| BuildExecutorFenceError::IdentityMismatch)?
                || live.try_get::<String, _>("state")? != "running"
                || live.try_get::<i32, _>("attempt")?
                    != i32::try_from(context.fence_generation)
                        .map_err(|_| BuildExecutorFenceError::IdentityMismatch)?
                || live.try_get::<Option<uuid::Uuid>, _>("lease_token")?
                    != Some(context.lease_token)
                || live
                    .try_get::<Option<time::OffsetDateTime>, _>("lease_expires_at")?
                    .is_none_or(|t| t <= authority_now)
                || live.try_get::<bool, _>("cancellation_requested")?
            {
                return Err(BuildExecutorFenceError::IdentityMismatch);
            }
        }
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
            if cleanup
                && (context.fence_generation != highest_generation
                    || context.lease_token != lease_token
                    || context.deadline_at.get() != previous_deadline)
            {
                return Err(BuildExecutorFenceError::IdentityMismatch);
            }
            let retry_cleanup = cleanup
                && context.fence_generation == highest_generation
                && last_stage_rank == stage_rank
                && tombstone_generation == Some(highest_generation)
                && last_request_id == context.stage_request_id.to_string()
                && last_response
                    .as_ref()
                    .is_some_and(retryable_cleanup_response);
            if context.fence_generation == highest_generation
                && last_request_id == context.stage_request_id.to_string()
                && !retry_cleanup
            {
                if let Some(value) = last_response {
                    transaction.rollback().await?;
                    return Ok(BuildExecutorAdmission::Replay(value));
                }
                return Err(BuildExecutorFenceError::InProgress);
            }
            if last_response.is_none() {
                return Err(BuildExecutorFenceError::InProgress);
            }
            if context.fence_generation == highest_generation
                && ((tombstone_generation == Some(highest_generation) && !retry_cleanup)
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
            if cleanup {
                return Err(BuildExecutorFenceError::IdentityMismatch);
            }
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

pub(crate) fn retryable_cleanup_response(value: &Value) -> bool {
    match serde_json::from_value::<BuildExecutorResponse>(value.clone()) {
        Ok(BuildExecutorResponse::Failed { failure }) => retryable_cleanup_failure(failure),
        _ => false,
    }
}

pub(crate) const fn retryable_cleanup_failure(failure: BuildProviderFailure) -> bool {
    failure.retryable
        && matches!(
            failure.code,
            BuildProviderFailureCode::Unavailable | BuildProviderFailureCode::TimedOut
        )
}

fn cleanup_timeout(request: &BuildExecutorRequest) -> Result<Duration, BuildExecutorFenceError> {
    if let BuildExecutorRequest::Cleanup {
        timeout_milliseconds,
        ..
    } = request
        && *timeout_milliseconds > 0
        && *timeout_milliseconds <= 3_600_000
    {
        return Ok(Duration::from_millis(*timeout_milliseconds));
    }
    Err(BuildExecutorFenceError::IdentityMismatch)
}

/// Server-side executor wrapper that never calls a side-effect adapter before durable admission.
pub struct FencedBuildExecutor<B> {
    store: PgBuildExecutorFenceStore,
    backend: B,
    billing: Option<task_execution::resource::ResourceClient>,
    active: Mutex<HashMap<BuildRequestId, ActiveBuild>>,
}

#[derive(Clone)]
struct ActiveBuild {
    context: BuildProviderRequestContext,
    cancellation: CancellationToken,
    joined: CancellationToken,
}

impl<B: BuildExecutorBackend> FencedBuildExecutor<B> {
    #[must_use]
    pub fn new(store: PgBuildExecutorFenceStore, backend: B) -> Self {
        Self {
            store,
            backend,
            billing: None,
            active: Mutex::new(HashMap::new()),
        }
    }

    /// Enables the actual authenticated Resource owner; absent configuration rejects `BuildKit`.
    #[must_use]
    pub fn with_billing_client(mut self, client: task_execution::resource::ResourceClient) -> Self {
        self.billing = Some(client);
        self
    }

    #[allow(
        clippy::too_many_lines,
        reason = "funding, native fence, execution and joined cleanup share this ordered boundary"
    )]
    pub async fn execute(
        &self,
        envelope: BuildExecutorRequestEnvelope,
    ) -> Result<BuildExecutorResponseEnvelope, BuildExecutorFenceError> {
        validate_executor_request(&envelope)?;
        if envelope.query_only {
            return self.query_original(&envelope).await;
        }
        if let Some(value) = self.store.cached_response(envelope.context).await?
            && !(envelope.context.stage == BuildProviderStage::Cleanup
                && retryable_cleanup_response(&value))
        {
            return Ok(Self::response_envelope(
                envelope.context,
                serde_json::from_value(value)
                    .map_err(|_| BuildExecutorFenceError::IdentityMismatch)?,
            ));
        }
        if envelope.context.stage != BuildProviderStage::Cleanup {
            self.store.preflight(&envelope).await?;
        }
        if let BuildExecutorRequest::EnsurePrivateProject { command, identity }
        | BuildExecutorRequest::Build { command, identity } = &envelope.request
            && matches!(
                command.request.source,
                contracts::supply_chain::BuildSource::Dockerfile { .. }
            )
        {
            let client = self
                .billing
                .as_ref()
                .ok_or(BuildExecutorFenceError::ConfigurationInvalid)?;
            let mut build_context = envelope.context;
            build_context.stage = BuildProviderStage::Build;
            let build_request = BuildExecutorRequest::Build {
                command: command.clone(),
                identity: *identity,
            };
            let build_context = bind_build_executor_request(build_context, &build_request)
                .map_err(|_| BuildExecutorFenceError::IdentityMismatch)?;
            let input = contracts::resource::build_costing::BuildCostAdmissionRequest {
                build_request_id: command.request.id,
                attempt: build_context.fence_generation,
                lease_token: build_context.lease_token,
                execution_deadline: build_context.deadline_at,
                stage_request_id: build_context.stage_request_id.to_string(),
                authorize_execution: envelope.context.stage == BuildProviderStage::Build,
            };
            client
                .admit_build_cost(&input)
                .await
                .map_err(BuildExecutorFenceError::Billing)?;
        }
        if envelope.context.stage == BuildProviderStage::Cleanup {
            validate_executor_request(&envelope)?;
            let active = self
                .active
                .lock()
                .await
                .get(&envelope.context.build_request_id)
                .cloned();
            if let Some(active) = active {
                if active.context.fence_generation != envelope.context.fence_generation
                    || active.context.lease_token != envelope.context.lease_token
                    || active.context.deadline_at != envelope.context.deadline_at
                {
                    return Err(BuildExecutorFenceError::IdentityMismatch);
                }
                if active.context.stage != BuildProviderStage::Cleanup {
                    active.cancellation.cancel();
                }
                tokio::time::timeout(
                    cleanup_timeout(&envelope.request)?,
                    active.joined.cancelled(),
                )
                .await
                .map_err(|_| BuildExecutorFenceError::InProgress)?;
            }
        }
        let admission = self.store.admit(&envelope).await;
        if matches!(
            admission,
            Err(BuildExecutorFenceError::IdentityMismatch
                | BuildExecutorFenceError::DeadlineExceeded)
        ) && matches!(
            envelope.request,
            BuildExecutorRequest::EnsurePrivateProject { .. } | BuildExecutorRequest::Build { .. }
        ) && let Some(client) = &self.billing
        {
            // The local cancellation/deadline fence rejected before any external execution.
            let _ = client
                .observe_build_cost(
                    &contracts::resource::build_costing::ObserveBuildCostRequest {
                        build_request_id: envelope.context.build_request_id,
                        attempt: envelope.context.fence_generation,
                        lease_token: envelope.context.lease_token,
                        measurement:
                            contracts::resource::build_costing::BuildUsageMeasurement::NotStarted {
                                reason: "local build fence rejected before external execution"
                                    .into(),
                            },
                    },
                )
                .await;
        }
        let response = match admission? {
            BuildExecutorAdmission::Execute(remaining) => {
                let active = ActiveBuild {
                    context: envelope.context,
                    cancellation: CancellationToken::new(),
                    joined: CancellationToken::new(),
                };
                self.active
                    .lock()
                    .await
                    .insert(envelope.context.build_request_id, active.clone());
                let execution = self.backend.execute(
                    &envelope.context,
                    &envelope.request,
                    &active.cancellation,
                );
                tokio::pin!(execution);
                let response = tokio::select! {
                    response = &mut execution => response,
                    () = tokio::time::sleep(remaining) => {
                        active.cancellation.cancel();
                        execution.await
                    }
                };
                let observed = if envelope.context.stage == BuildProviderStage::Build {
                    self.report_history(envelope.context, &envelope.request)
                        .await
                } else {
                    Ok(false)
                };
                let unconfirmed = matches!(&response, BuildExecutorResponse::Failed { failure } if failure.code == BuildProviderFailureCode::ExecutionUnknown)
                    && !observed.as_ref().is_ok_and(|confirmed| *confirmed);
                let complete = if unconfirmed {
                    Ok(())
                } else {
                    self.store.complete(envelope.context, &response).await
                };
                self.active
                    .lock()
                    .await
                    .remove(&envelope.context.build_request_id);
                active.joined.cancel();
                complete?;
                observed?;
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
    fn response_envelope(
        context: BuildProviderRequestContext,
        response: BuildExecutorResponse,
    ) -> BuildExecutorResponseEnvelope {
        BuildExecutorResponseEnvelope {
            protocol_version: context.protocol_version,
            build_request_id: context.build_request_id,
            fence_generation: context.fence_generation,
            stage: context.stage,
            stage_request_id: context.stage_request_id,
            response,
        }
    }
    async fn report_history(
        &self,
        context: BuildProviderRequestContext,
        request: &BuildExecutorRequest,
    ) -> Result<bool, BuildExecutorFenceError> {
        let client = self
            .billing
            .as_ref()
            .ok_or(BuildExecutorFenceError::ConfigurationInvalid)?;
        let original = client
            .get_build_cost(
                context.build_request_id,
                context.fence_generation,
                context.lease_token,
            )
            .await
            .map_err(BuildExecutorFenceError::Billing)?
            .ok_or(BuildExecutorFenceError::IdentityMismatch)?;
        let measurement = match original.measurement {
            Some(
                known @ contracts::resource::build_costing::BuildUsageMeasurement::Known { .. },
            ) => known,
            _ => self.backend.query_build_usage(&context, request).await,
        };
        let record = client
            .observe_build_cost(
                &contracts::resource::build_costing::ObserveBuildCostRequest {
                    build_request_id: context.build_request_id,
                    attempt: context.fence_generation,
                    lease_token: context.lease_token,
                    measurement,
                },
            )
            .await
            .map_err(BuildExecutorFenceError::Billing)?;
        Ok(matches!(
            record.measurement,
            Some(contracts::resource::build_costing::BuildUsageMeasurement::Known { .. })
        ))
    }
    async fn query_original(
        &self,
        envelope: &BuildExecutorRequestEnvelope,
    ) -> Result<BuildExecutorResponseEnvelope, BuildExecutorFenceError> {
        if let Some(value) = self.store.cached_response(envelope.context).await? {
            if envelope.context.stage == BuildProviderStage::Build {
                self.report_history(envelope.context, &envelope.request)
                    .await?;
            }
            return Ok(Self::response_envelope(
                envelope.context,
                serde_json::from_value(value)
                    .map_err(|_| BuildExecutorFenceError::IdentityMismatch)?,
            ));
        }
        if envelope.context.stage != BuildProviderStage::Build {
            return Err(BuildExecutorFenceError::IdentityMismatch);
        }
        let client = self
            .billing
            .as_ref()
            .ok_or(BuildExecutorFenceError::ConfigurationInvalid)?;
        let record = client
            .get_build_cost(
                envelope.context.build_request_id,
                envelope.context.fence_generation,
                envelope.context.lease_token,
            )
            .await
            .map_err(BuildExecutorFenceError::Billing)?
            .ok_or(BuildExecutorFenceError::IdentityMismatch)?;
        if record.execution_deadline != Some(envelope.context.deadline_at)
            || record.stage_request_id.as_deref()
                != Some(envelope.context.stage_request_id.to_string().as_str())
        {
            return Err(BuildExecutorFenceError::IdentityMismatch);
        }
        if self
            .report_history(envelope.context, &envelope.request)
            .await?
        {
            // A trustworthy terminal interval proves the physical attempt ended. Keep its result unknown,
            // permit exact-fence cleanup, and never replace cleanup/a newer generation or publish an artifact.
            let response = serde_json::to_value(BuildExecutorResponse::Failed {
                failure: BuildProviderFailure {
                    code: BuildProviderFailureCode::ExecutionUnknown,
                    retryable: false,
                },
            })
            .map_err(|_| BuildExecutorFenceError::IdentityMismatch)?;
            sqlx::query("UPDATE agent.build_executor_fences SET last_response=$6,updated_at=clock_timestamp() WHERE build_request_id=$1 AND highest_generation=$2 AND lease_token=$3 AND last_request_id=$4 AND deadline_at=$5 AND last_stage='build' AND last_response IS NULL AND tombstone_generation IS NULL").bind(envelope.context.build_request_id.as_uuid()).bind(i32::try_from(envelope.context.fence_generation).map_err(|_|BuildExecutorFenceError::IdentityMismatch)?).bind(envelope.context.lease_token).bind(envelope.context.stage_request_id.to_string()).bind(envelope.context.deadline_at.get()).bind(response).execute(&self.store.pool).await?;
        }
        Ok(Self::response_envelope(
            envelope.context,
            BuildExecutorResponse::Failed {
                failure: BuildProviderFailure {
                    code: BuildProviderFailureCode::ExecutionUnknown,
                    retryable: false,
                },
            },
        ))
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
        let executor = std::sync::Arc::clone(&self.executor);
        tokio::try_join!(
            self.serve_requests(),
            reconcile_build_observations(executor)
        )
        .map(|_| ())
    }
    async fn serve_requests(self) -> Result<(), BuildExecutorFenceError> {
        let mut subscriber = self
            .client
            .subscribe(self.subject)
            .await
            .map_err(|_| BuildExecutorFenceError::Transport)?;
        let mut requests = tokio::task::JoinSet::new();
        loop {
            let message = tokio::select! {
                biased;
                () = crate::service_runtime::wait_for_shutdown() => break,
                _ = requests.join_next(), if !requests.is_empty() => continue,
                message = subscriber.next() => message.ok_or(BuildExecutorFenceError::Transport)?,
            };
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
            requests.spawn(async move {
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
        while requests.join_next().await.is_some() {}
        Ok(())
    }
}

async fn reconcile_build_observations<B: BuildExecutorBackend + 'static>(
    executor: std::sync::Arc<FencedBuildExecutor<B>>,
) -> Result<(), BuildExecutorFenceError> {
    let mut interval = tokio::time::interval(Duration::from_secs(10));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        if !crate::service_runtime::worker_tick(&mut interval).await {
            return Ok(());
        }
        let Some(client) = &executor.billing else {
            continue;
        };
        let grants = match client.claim_build_cost_reconciliations().await {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(event="agent.build_cost.query_failed",diagnostic_code=%error,outcome="will_query_again");
                continue;
            }
        };
        for grant in grants {
            if crate::service_runtime::is_stopping() {
                return Ok(());
            }
            let request = BuildExecutorRequest::Build {
                identity: BuildIdentity(Sha256Digest::of_bytes(
                    grant.command.request.id.as_uuid().as_bytes(),
                )),
                command: grant.command,
            };
            let context = BuildProviderRequestContext {
                protocol_version: crate::build_pipeline::BUILD_EXECUTOR_PROTOCOL_VERSION,
                build_request_id: grant.admission.build_request_id,
                fence_generation: grant.admission.attempt,
                lease_token: grant.admission.lease_token,
                stage: BuildProviderStage::Build,
                stage_request_id: grant
                    .admission
                    .stage_request_id
                    .parse()
                    .map_err(|_| BuildExecutorFenceError::IdentityMismatch)?,
                deadline_at: grant.admission.execution_deadline,
            };
            let envelope = BuildExecutorRequestEnvelope {
                context,
                request,
                query_only: true,
            };
            if let Err(error) = validate_executor_request(&envelope) {
                tracing::warn!(event="agent.build_cost.query_invalid",diagnostic_code=%error);
                continue;
            }
            if let Err(error) = executor.query_original(&envelope).await {
                tracing::warn!(event="agent.build_cost.query_failed",build_request_id=%context.build_request_id,diagnostic_code=%error,outcome="will_query_again");
            }
        }
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
        BuildExecutorFenceError::Billing(
            task_execution::resource::ResourceClientError::Denied
            | task_execution::resource::ResourceClientError::Conflict
            | task_execution::resource::ResourceClientError::Rejected
            | task_execution::resource::ResourceClientError::RequestMissing,
        ) => BuildProviderFailure {
            code: BuildProviderFailureCode::FundingRejected,
            retryable: false,
        },
        BuildExecutorFenceError::Billing(_) => BuildProviderFailure {
            code: BuildProviderFailureCode::ExecutionUnknown,
            retryable: false,
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
        | BuildExecutorRequest::Build { command, identity }
        | BuildExecutorRequest::Import { command, identity } => {
            command.validate().is_ok()
                && *identity
                    == BuildIdentity(Sha256Digest::of_bytes(
                        command.request.id.as_uuid().as_bytes(),
                    ))
        }
        BuildExecutorRequest::Publish { candidate } => {
            candidate.digest.starts_with("sha256:")
                && candidate.digest.len() == 71
                && candidate
                    .digest
                    .strip_prefix("sha256:")
                    .is_some_and(|digest| digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
        }
        BuildExecutorRequest::Cleanup {
            build_request_id,
            identity,
            timeout_milliseconds,
        } => {
            *identity
                == BuildIdentity(Sha256Digest::of_bytes(
                    build_request_id.as_uuid().as_bytes(),
                ))
                && *timeout_milliseconds > 0
                && *timeout_milliseconds <= 3_600_000
        }
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
) -> Result<Sha256Digest, BuildExecutorFenceError> {
    Sha256Digest::of_canonical(&serde_json::json!({
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
        | BuildExecutorRequest::Build { command, .. }
        | BuildExecutorRequest::Import { command, .. } => command.request.id,
        BuildExecutorRequest::Publish { candidate } => candidate.build_request_id,
        BuildExecutorRequest::Cleanup {
            build_request_id, ..
        } => *build_request_id,
    }
}

const fn build_stage_rank(stage: BuildProviderStage) -> i16 {
    match stage {
        BuildProviderStage::EnsurePrivateProject => 1,
        BuildProviderStage::Build | BuildProviderStage::Import => 2,
        BuildProviderStage::Publish => 3,
        BuildProviderStage::Cleanup => 4,
    }
}

pub(crate) const fn build_stage_name(stage: BuildProviderStage) -> &'static str {
    match stage {
        BuildProviderStage::EnsurePrivateProject => "ensure_private_project",
        BuildProviderStage::Build => "build",
        BuildProviderStage::Import => "import",
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
    #[error("LW_AGENT_BUILD_CREDITS_FAILED")]
    Billing(#[source] task_execution::resource::ResourceClientError),
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
