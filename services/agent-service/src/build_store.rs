//! `PostgreSQL` authority for durable, idempotent Container build execution.
#![allow(
    missing_docs,
    reason = "stable diagnostics and the contracts crate document the public integration surface"
)]

use persistence_sqlx::Sha256Digest; // internal persistence hash, not contract hash
use std::str::FromStr;
use std::time::Duration;

use contracts::events::{
    AgentBuildCompleted, AgentBuildFailed, AgentBuildRequested, CloudEvent, SPEC_VERSION, subjects,
};
use contracts::http::{
    IdempotencyKey, InternalAgentBuildCancellationRequest, InternalAgentBuildCancellationResult,
    InternalAgentBuildState, InternalAgentBuildStatusQuery,
};
use contracts::supply_chain::ImageArtifact;
use contracts::{
    BuildRequestId, CourseId, EventId, ImageArtifactId, Revision, Sequence, UtcTimestamp,
};
use persistence_sqlx::{
    Domain, IdempotencyDecision, IdempotencyStore, InboxDecision, InboxStore, OutboxStore,
};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::build_pipeline::{
    BuildCancellation, BuildExecutionFence, BuildPipeline, BuildPipelineError, BuildPipelineOutput,
    BuildSupplyChainProvider,
};

const CANCEL_BUILD_OPERATION: &str = "agent.build.cancel";
const CONTROL_AUTHORITY_SAN_URI: &str = "spiffe://labweaver/control-service";
const CANCELLATION_COMMAND_MAX_AGE: time::Duration = time::Duration::minutes(5);

/// Durable Inbox outcome for one Control build command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuildCommandDecision {
    Accepted,
    Duplicate,
    Stale,
    Gap,
}

/// Fenced command owned by exactly one build worker.
#[derive(Clone, Debug)]
pub struct BuildCommandLease {
    pub command: AgentBuildRequested,
    pub attempt: u32,
    pub cancellation_requested: bool,
    worker_id: String,
    lease_token: Uuid,
}

/// Agent-owned repository for command, artifact, policy, and terminal-event atomicity.
#[derive(Clone, Debug)]
pub struct PgBuildStore {
    pool: PgPool,
}

impl PgBuildStore {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn accept_command(
        &self,
        consumer: &str,
        event: &CloudEvent<AgentBuildRequested>,
    ) -> Result<BuildCommandDecision, BuildStoreError> {
        let contract =
            contracts::events::EventContract::by_subject(subjects::AGENT_BUILD_REQUESTED)
                .ok_or(BuildStoreError::ContractInvalid)?;
        event
            .validate(contract)
            .map_err(|_| BuildStoreError::ContractInvalid)?;
        event
            .data
            .validate()
            .map_err(|_| BuildStoreError::ContractInvalid)?;
        if event.subject != subjects::AGENT_BUILD_REQUESTED
            || event.course_id != event.data.request.course_id
            || event.aggregate_revision
                != Revision::new(1).map_err(|_| BuildStoreError::ContractInvalid)?
        {
            return Err(BuildStoreError::IdentityMismatch);
        }
        let payload = serde_json::to_value(event).map_err(|_| BuildStoreError::ContractInvalid)?;
        let payload_sha256 = canonical_hash(&payload)?;
        let mut transaction = self.pool.begin().await?;
        let decision = InboxStore::accept(
            &mut transaction,
            Domain::Agent,
            consumer,
            event.id.as_uuid(),
            event.data.request.id.as_uuid(),
            event.aggregate_sequence.0,
            payload_sha256,
        )
        .await
        .map_err(|_| BuildStoreError::PersistenceFailed)?;
        let outcome = match decision {
            InboxDecision::Accepted => {
                sqlx::query(
                    "INSERT INTO agent.build_commands \
                     (build_request_id,course_id,command_sha256,idempotency_key,state,command) \
                     VALUES ($1,$2,$3,$4,'requested',$5)",
                )
                .bind(event.data.request.id.as_uuid())
                .bind(event.course_id.as_uuid())
                .bind(canonical_hash(&event.data.request)?.to_string())
                .bind(&event.data.idempotency_key)
                .bind(
                    serde_json::to_value(&event.data)
                        .map_err(|_| BuildStoreError::ContractInvalid)?,
                )
                .execute(&mut *transaction)
                .await
                .map_err(|error| {
                    if error
                        .as_database_error()
                        .is_some_and(sqlx::error::DatabaseError::is_unique_violation)
                    {
                        BuildStoreError::IdentityMismatch
                    } else {
                        BuildStoreError::Database(error)
                    }
                })?;
                BuildCommandDecision::Accepted
            }
            InboxDecision::Duplicate => BuildCommandDecision::Duplicate,
            InboxDecision::Stale => BuildCommandDecision::Stale,
            InboxDecision::Gap => {
                transaction.rollback().await?;
                return Ok(BuildCommandDecision::Gap);
            }
        };
        transaction.commit().await?;
        Ok(outcome)
    }

    pub async fn claim_due(
        &self,
        worker_id: &str,
        lease_duration: Duration,
    ) -> Result<Option<BuildCommandLease>, BuildStoreError> {
        if !valid_worker_id(worker_id)
            || lease_duration.is_zero()
            || lease_duration > Duration::from_mins(5)
            || !lease_duration.subsec_nanos().is_multiple_of(1_000_000)
        {
            return Err(BuildStoreError::ConfigurationInvalid);
        }
        let lease_milliseconds = i64::try_from(lease_duration.as_millis())
            .map_err(|_| BuildStoreError::ConfigurationInvalid)?;
        let lease_token = Uuid::new_v4();
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query(
            "SELECT build_request_id,command,attempt,cancellation_requested \
             FROM agent.build_commands \
             WHERE (state='requested' OR (state='running' AND lease_expires_at<=clock_timestamp())) \
               AND next_attempt_at<=clock_timestamp() \
             ORDER BY created_at,build_request_id FOR UPDATE SKIP LOCKED LIMIT 1",
        )
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(row) = row else {
            transaction.rollback().await?;
            return Ok(None);
        };
        let build_request_id: Uuid = row.try_get("build_request_id")?;
        let previous_attempt: i32 = row.try_get("attempt")?;
        let attempt = previous_attempt
            .checked_add(1)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or(BuildStoreError::IdentityMismatch)?;
        let updated = sqlx::query(
            "UPDATE agent.build_commands SET state='running',revision=revision+1,attempt=$2,worker_id=$3,lease_token=$4, \
             lease_expires_at=clock_timestamp()+($5::bigint * interval '1 millisecond'),updated_at=clock_timestamp() \
             WHERE build_request_id=$1",
        )
        .bind(build_request_id)
        .bind(i32::try_from(attempt).map_err(|_| BuildStoreError::IdentityMismatch)?)
        .bind(worker_id)
        .bind(lease_token)
        .bind(lease_milliseconds)
        .execute(&mut *transaction)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(BuildStoreError::FenceLost);
        }
        let command: AgentBuildRequested = serde_json::from_value(row.try_get("command")?)
            .map_err(|_| BuildStoreError::ContractInvalid)?;
        command
            .validate()
            .map_err(|_| BuildStoreError::ContractInvalid)?;
        let cancellation_requested = row.try_get("cancellation_requested")?;
        transaction.commit().await?;
        Ok(Some(BuildCommandLease {
            command,
            attempt,
            cancellation_requested,
            worker_id: worker_id.to_owned(),
            lease_token,
        }))
    }

    /// Renews one exact build lease and returns its durable cancellation flag.
    pub async fn heartbeat(
        &self,
        lease: &BuildCommandLease,
        lease_duration: Duration,
    ) -> Result<bool, BuildStoreError> {
        if !valid_worker_id(&lease.worker_id)
            || lease_duration.is_zero()
            || lease_duration > Duration::from_mins(5)
            || !lease_duration.subsec_nanos().is_multiple_of(1_000_000)
        {
            return Err(BuildStoreError::ConfigurationInvalid);
        }
        let lease_milliseconds = i64::try_from(lease_duration.as_millis())
            .map_err(|_| BuildStoreError::ConfigurationInvalid)?;
        sqlx::query_scalar(
            "UPDATE agent.build_commands SET lease_expires_at=date_trunc('milliseconds',clock_timestamp()) \
                 + ($4::bigint * interval '1 millisecond'),updated_at=clock_timestamp() \
             WHERE build_request_id=$1 AND state='running' AND worker_id=$2 AND lease_token=$3 \
               AND lease_expires_at>clock_timestamp() RETURNING cancellation_requested",
        )
        .bind(lease.command.request.id.as_uuid())
        .bind(&lease.worker_id)
        .bind(lease.lease_token)
        .bind(lease_milliseconds)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(BuildStoreError::FenceLost)
    }

    pub async fn complete(
        &self,
        lease: &BuildCommandLease,
        output: &BuildPipelineOutput,
        trace_id: &str,
    ) -> Result<(), BuildStoreError> {
        let _ = &output.build_identity;
        if output.registry_project.build_request_id != lease.command.request.id
            || output.registry_project.build_identity != output.build_identity
            || !output.registry_project.private
            || output.registry_project.storage_quota_bytes == 0
        {
            return Err(BuildStoreError::IdentityMismatch);
        }
        output
            .artifact
            .validate()
            .map_err(|_| BuildStoreError::ContractInvalid)?;
        let (artifact_id, build_request_id, digest) = container_identity(&output.artifact)?;
        if build_request_id != lease.command.request.id {
            return Err(BuildStoreError::IdentityMismatch);
        }
        let artifact_contract =
            serde_json::to_value(&output.artifact).map_err(|_| BuildStoreError::ContractInvalid)?;
        // Policy evaluation removed: store placeholder empty object for backward-compatible column
        let evaluation_contract = serde_json::json!({});
        let registry_project_contract = serde_json::to_value(&output.registry_project)
            .map_err(|_| BuildStoreError::ContractInvalid)?;
        let artifact_evidence_sha256 = canonical_hash(&artifact_contract)?;
        let mut transaction = self.pool.begin().await?;
        fence_running(&mut transaction, lease).await?;
        let authority_now = transaction_time(&mut transaction).await?;
        sqlx::query(
            "INSERT INTO agent.image_artifacts \
             (image_artifact_id,build_request_id,image_digest,evidence_sha256,state,contract,policy_evaluation,registry_project_evidence) \
             VALUES ($1,$2,$3,$4,'verified',$5,$6,$7)",
        )
        .bind(artifact_id.as_uuid())
        .bind(build_request_id.as_uuid())
        .bind(digest)
        .bind(artifact_evidence_sha256.to_string())
        .bind(artifact_contract)
        .bind(evaluation_contract)
        .bind(registry_project_contract)
        .execute(&mut *transaction)
        .await?;
        terminal_update(&mut transaction, lease, "succeeded", None, None, None).await?;
        let data = AgentBuildCompleted {
            build_request_id,
            artifact_id,
        };
        enqueue_terminal_event(
            &mut transaction,
            lease,
            subjects::AGENT_BUILD_COMPLETED,
            authority_now,
            trace_id,
            &data,
        )
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn fail(
        &self,
        lease: &BuildCommandLease,
        error: BuildPipelineError,
        trace_id: &str,
    ) -> Result<(), BuildStoreError> {
        let data = AgentBuildFailed {
            build_request_id: lease.command.request.id,
            diagnostic_code: error.diagnostic_code().to_owned(),
            retryable: error.retryable,
            cleanup_verified: error.cleanup_verified,
        };
        data.validate()
            .map_err(|_| BuildStoreError::ContractInvalid)?;
        let terminal_state = if error.code == crate::build_pipeline::BuildFailureCode::Cancelled {
            "cancelled"
        } else {
            "failed"
        };
        let mut transaction = self.pool.begin().await?;
        fence_running(&mut transaction, lease).await?;
        let authority_now = transaction_time(&mut transaction).await?;
        terminal_update(
            &mut transaction,
            lease,
            terminal_state,
            Some(&data.diagnostic_code),
            Some(error.retryable),
            Some(error.cleanup_verified),
        )
        .await?;
        enqueue_terminal_event(
            &mut transaction,
            lease,
            subjects::AGENT_BUILD_FAILED,
            authority_now,
            trace_id,
            &data,
        )
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    pub async fn schedule_retry(
        &self,
        lease: &BuildCommandLease,
        error: BuildPipelineError,
        retry_delay: Duration,
    ) -> Result<(), BuildStoreError> {
        if !error.retryable
            || !error.cleanup_verified
            || retry_delay.is_zero()
            || !retry_delay.subsec_nanos().is_multiple_of(1_000_000)
        {
            return Err(BuildStoreError::RetryUnsafe);
        }
        let retry_milliseconds =
            i64::try_from(retry_delay.as_millis()).map_err(|_| BuildStoreError::ClockInvalid)?;
        let result = sqlx::query(
            "UPDATE agent.build_commands SET state='requested',revision=revision+1,worker_id=NULL,lease_token=NULL,lease_expires_at=NULL, \
             next_attempt_at=date_trunc('milliseconds',clock_timestamp()) \
                 + ($3::bigint * interval '1 millisecond'), \
             diagnostic_code=NULL,retryable=NULL,cleanup_verified=NULL,updated_at=clock_timestamp() \
             WHERE build_request_id=$1 AND lease_token=$2 AND state='running' \
               AND lease_expires_at>clock_timestamp()",
        )
        .bind(lease.command.request.id.as_uuid())
        .bind(lease.lease_token)
        .bind(retry_milliseconds)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() != 1 {
            return Err(BuildStoreError::FenceLost);
        }
        Ok(())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the transaction keeps idempotency, authority time, scope and revision fencing together"
    )]
    pub async fn request_cancellation(
        &self,
        request: &InternalAgentBuildCancellationRequest,
        idempotency_key: &IdempotencyKey,
    ) -> Result<InternalAgentBuildCancellationResult, BuildStoreError> {
        if request.authority_san_uri != CONTROL_AUTHORITY_SAN_URI {
            return Err(BuildStoreError::AuthorityMismatch);
        }
        let request_hash = canonical_hash(request)?;
        let mut transaction = self.pool.begin().await?;
        match IdempotencyStore::reserve(
            &mut transaction,
            Domain::Agent,
            CANCEL_BUILD_OPERATION,
            idempotency_key.as_str(),
            request_hash,
        )
        .await
        .map_err(|_| BuildStoreError::PersistenceFailed)?
        {
            IdempotencyDecision::Replay(value) => {
                transaction.rollback().await?;
                return serde_json::from_value(value).map_err(|_| BuildStoreError::ContractInvalid);
            }
            IdempotencyDecision::Conflict => return Err(BuildStoreError::IdempotencyConflict),
            IdempotencyDecision::InProgress => return Err(BuildStoreError::RequestInProgress),
            IdempotencyDecision::Reserved => {}
        }
        let authority_now = transaction_time(&mut transaction).await?;
        let oldest_allowed = authority_now
            .get()
            .checked_sub(CANCELLATION_COMMAND_MAX_AGE)
            .ok_or(BuildStoreError::ClockInvalid)?;
        if request.requested_at.get() > authority_now.get()
            || request.requested_at.get() < oldest_allowed
        {
            return Err(BuildStoreError::RequestExpired);
        }
        let row = sqlx::query(
            "SELECT course_id,command_sha256,state,revision,cancellation_requested \
             FROM agent.build_commands WHERE build_request_id=$1 FOR UPDATE",
        )
        .bind(request.build_request_id.as_uuid())
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(BuildStoreError::NotFound)?;
        let course_id = CourseId::from_str(&row.try_get::<uuid::Uuid, _>("course_id")?.to_string())
            .map_err(|_| BuildStoreError::ContractInvalid)?;
        let _command_sha256 = row
            .try_get::<String, _>("command_sha256")?
            .parse::<Sha256Digest>()
            .map_err(|_| BuildStoreError::ContractInvalid)?;
        let state = parse_build_state(&row.try_get::<String, _>("state")?)?;
        let revision = {
            let val: i64 = row
                .try_get::<i64, _>("revision")
                .map_err(|_| BuildStoreError::ContractInvalid)?;
            Revision::try_from(val).map_err(|_| BuildStoreError::ContractInvalid)?
        };
        if course_id != request.course_id {
            return Err(BuildStoreError::CourseMismatch);
        }
        if state != request.expected_state || revision != request.expected_revision {
            return Err(BuildStoreError::StateConflict);
        }
        if !matches!(
            state,
            InternalAgentBuildState::Requested | InternalAgentBuildState::Running
        ) {
            return Err(BuildStoreError::StateConflict);
        }
        let next_revision = revision.next().ok_or(BuildStoreError::ContractInvalid)?;
        let updated = sqlx::query(
            "UPDATE agent.build_commands SET cancellation_requested=true,cancellation_audit_version=1,revision=$2, \
                 cancellation_actor_id=$3,cancellation_authority_san_uri=$4, \
                 cancellation_requested_at=$5,updated_at=clock_timestamp() \
             WHERE build_request_id=$1 AND revision=$6 AND state=$7",
        )
        .bind(request.build_request_id.as_uuid())
        .bind(next_revision.to_i64().ok_or(BuildStoreError::ContractInvalid)?)
        .bind(request.actor_id.as_uuid())
        .bind(&request.authority_san_uri)
        .bind(request.requested_at.get())
        .bind(revision.to_i64().ok_or(BuildStoreError::ContractInvalid)?)
        .bind(build_state_name(state))
        .execute(&mut *transaction)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(BuildStoreError::StateConflict);
        }
        let result = InternalAgentBuildCancellationResult {
            course_id,
            build_request_id: request.build_request_id,
            state,
            revision: next_revision,
            cancellation_requested: true,
        };
        let value = serde_json::to_value(&result).map_err(|_| BuildStoreError::ContractInvalid)?;
        IdempotencyStore::complete(
            &mut transaction,
            Domain::Agent,
            CANCEL_BUILD_OPERATION,
            idempotency_key.as_str(),
            &value,
        )
        .await
        .map_err(|_| BuildStoreError::PersistenceFailed)?;
        transaction.commit().await?;
        Ok(result)
    }

    pub async fn load_status(
        &self,
        build_request_id: BuildRequestId,
        query: &InternalAgentBuildStatusQuery,
    ) -> Result<InternalAgentBuildCancellationResult, BuildStoreError> {
        let row = sqlx::query(
            "SELECT course_id,command_sha256,state,revision,cancellation_requested \
             FROM agent.build_commands WHERE build_request_id=$1",
        )
        .bind(build_request_id.as_uuid())
        .fetch_optional(&self.pool)
        .await?
        .ok_or(BuildStoreError::NotFound)?;
        let course_id = CourseId::from_str(&row.try_get::<uuid::Uuid, _>("course_id")?.to_string())
            .map_err(|_| BuildStoreError::ContractInvalid)?;
        let _command_sha256 = row
            .try_get::<String, _>("command_sha256")?
            .parse::<Sha256Digest>()
            .map_err(|_| BuildStoreError::ContractInvalid)?;
        if course_id != query.course_id {
            return Err(BuildStoreError::CourseMismatch);
        }
        let rev_val: i64 = row
            .try_get::<i64, _>("revision")
            .map_err(|_| BuildStoreError::ContractInvalid)?;
        let revision = Revision::try_from(rev_val).map_err(|_| BuildStoreError::ContractInvalid)?;
        Ok(InternalAgentBuildCancellationResult {
            course_id,
            build_request_id,
            state: parse_build_state(&row.try_get::<String, _>("state")?)?,
            revision,
            cancellation_requested: row.try_get("cancellation_requested")?,
        })
    }
}

/// One durable worker that runs at most one fenced build command per call.
pub struct BuildWorker<P> {
    store: PgBuildStore,
    pipeline: BuildPipeline<P>,
    worker_id: String,
    lease_duration: Duration,
    retry_delay: Duration,
    max_attempts: u32,
}

impl<P: BuildSupplyChainProvider> BuildWorker<P> {
    pub fn new(
        store: PgBuildStore,
        pipeline: BuildPipeline<P>,
        worker_id: String,
        lease_duration: Duration,
        retry_delay: Duration,
        max_attempts: u32,
    ) -> Result<Self, BuildStoreError> {
        if !valid_worker_id(&worker_id)
            || lease_duration.is_zero()
            || retry_delay.is_zero()
            || max_attempts == 0
            || max_attempts > 100
        {
            return Err(BuildStoreError::ConfigurationInvalid);
        }
        Ok(Self {
            store,
            pipeline,
            worker_id,
            lease_duration,
            retry_delay,
            max_attempts,
        })
    }

    pub async fn run_once(&self, now: UtcTimestamp) -> Result<BuildWorkerOutcome, BuildStoreError> {
        let Some(lease) = self
            .store
            .claim_due(&self.worker_id, self.lease_duration)
            .await?
        else {
            return Ok(BuildWorkerOutcome::Idle);
        };
        match self.execute_with_heartbeat(&lease, now).await? {
            Ok(output) => {
                self.store
                    .complete(
                        &lease,
                        &output,
                        &format!("build:{}", lease.command.request.id),
                    )
                    .await?;
                Ok(BuildWorkerOutcome::Completed {
                    build_request_id: lease.command.request.id,
                })
            }
            Err(error)
                if error.retryable
                    && error.cleanup_verified
                    && lease.attempt < self.max_attempts =>
            {
                self.store
                    .schedule_retry(&lease, error, self.retry_delay)
                    .await?;
                Ok(BuildWorkerOutcome::RetryScheduled {
                    build_request_id: lease.command.request.id,
                    attempt: lease.attempt,
                })
            }
            Err(error) => {
                self.store
                    .fail(
                        &lease,
                        error,
                        &format!("build:{}", lease.command.request.id),
                    )
                    .await?;
                Ok(BuildWorkerOutcome::Failed {
                    build_request_id: lease.command.request.id,
                    diagnostic_code: error.diagnostic_code(),
                })
            }
        }
    }

    async fn execute_with_heartbeat(
        &self,
        lease: &BuildCommandLease,
        now: UtcTimestamp,
    ) -> Result<Result<BuildPipelineOutput, BuildPipelineError>, BuildStoreError> {
        let cancellation = BuildCancellation::new();
        if lease.cancellation_requested {
            cancellation.cancel();
        }
        let execution_deadline = add_duration(
            now,
            Duration::from_millis(lease.command.request.max_duration_milliseconds),
        )?;
        let fence = BuildExecutionFence::new(lease.attempt, lease.lease_token, execution_deadline)
            .map_err(|_| BuildStoreError::ConfigurationInvalid)?;
        let execution = self
            .pipeline
            .execute(&lease.command, now, fence, &cancellation);
        tokio::pin!(execution);
        let heartbeat_period = self
            .lease_duration
            .checked_div(3)
            .unwrap_or(self.lease_duration)
            .max(Duration::from_millis(10))
            .min(Duration::from_secs(1));
        let mut heartbeat = tokio::time::interval(heartbeat_period);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        heartbeat.tick().await;
        loop {
            tokio::select! {
                biased;
                outcome = &mut execution => return Ok(outcome),
                _ = heartbeat.tick() => {
                    match self.store.heartbeat(lease, self.lease_duration).await {
                        Ok(true) => cancellation.cancel(),
                        Ok(false) => {}
                        Err(error) => {
                            cancellation.cancel();
                            let _ = execution.await;
                            return Err(error);
                        }
                    }
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuildWorkerOutcome {
    Idle,
    Completed {
        build_request_id: BuildRequestId,
    },
    RetryScheduled {
        build_request_id: BuildRequestId,
        attempt: u32,
    },
    Failed {
        build_request_id: BuildRequestId,
        diagnostic_code: &'static str,
    },
}

async fn fence_running(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    lease: &BuildCommandLease,
) -> Result<(), BuildStoreError> {
    let current: Option<bool> = sqlx::query_scalar(
        "SELECT lease_expires_at>clock_timestamp() FROM agent.build_commands \
         WHERE build_request_id=$1 AND state='running' AND lease_token=$2 FOR UPDATE",
    )
    .bind(lease.command.request.id.as_uuid())
    .bind(lease.lease_token)
    .fetch_optional(&mut **transaction)
    .await?;
    if current != Some(true) {
        return Err(BuildStoreError::FenceLost);
    }
    Ok(())
}

async fn transaction_time(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<UtcTimestamp, BuildStoreError> {
    let authority_now: time::OffsetDateTime =
        sqlx::query_scalar("SELECT date_trunc('milliseconds', clock_timestamp())")
            .fetch_one(&mut **transaction)
            .await?;
    UtcTimestamp::from_utc(authority_now).map_err(|_| BuildStoreError::ClockInvalid)
}

async fn terminal_update(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    lease: &BuildCommandLease,
    state: &str,
    diagnostic_code: Option<&str>,
    retryable: Option<bool>,
    cleanup_verified: Option<bool>,
) -> Result<(), BuildStoreError> {
    let result = sqlx::query(
        "UPDATE agent.build_commands SET state=$3,revision=revision+1,worker_id=NULL,lease_token=NULL,lease_expires_at=NULL, \
         diagnostic_code=$4,retryable=$5,cleanup_verified=$6,completed_at=clock_timestamp(),updated_at=clock_timestamp() \
         WHERE build_request_id=$1 AND lease_token=$2 AND state='running'",
    )
    .bind(lease.command.request.id.as_uuid())
    .bind(lease.lease_token)
    .bind(state)
    .bind(diagnostic_code)
    .bind(retryable)
    .bind(cleanup_verified)
    .execute(&mut **transaction)
    .await?;
    if result.rows_affected() != 1 {
        return Err(BuildStoreError::FenceLost);
    }
    Ok(())
}

async fn enqueue_terminal_event<T: serde::Serialize>(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    lease: &BuildCommandLease,
    subject: &'static str,
    now: UtcTimestamp,
    trace_id: &str,
    data: &T,
) -> Result<(), BuildStoreError> {
    if trace_id.trim().is_empty() {
        return Err(BuildStoreError::ContractInvalid);
    }
    let contract = contracts::events::EventContract::by_subject(subject)
        .ok_or(BuildStoreError::ContractInvalid)?;
    let event_id = EventId::new();
    let event = CloudEvent {
        specversion: SPEC_VERSION.to_owned(),
        id: event_id,
        source: contract.source().to_owned(),
        event_type: subject.to_owned(),
        subject: subject.to_owned(),
        time: now,
        datacontenttype: "application/json".to_owned(),
        dataschema: contract.data_schema(),
        course_id: lease.command.request.course_id,
        aggregate_revision: Revision::new(1).map_err(|_| BuildStoreError::ContractInvalid)?,
        aggregate_sequence: Sequence(1),
        trace_id: trace_id.to_owned(),
        data,
    };
    event
        .validate(contract)
        .map_err(|_| BuildStoreError::ContractInvalid)?;
    let payload = serde_json::to_value(&event).map_err(|_| BuildStoreError::ContractInvalid)?;
    OutboxStore::enqueue(
        transaction,
        Domain::Agent,
        event_id.as_uuid(),
        subject,
        subject,
        lease.command.request.id.as_uuid(),
        1,
        &payload,
        canonical_hash(&payload)?,
    )
    .await
    .map_err(|_| BuildStoreError::PersistenceFailed)
}

fn container_identity(
    artifact: &ImageArtifact,
) -> Result<(ImageArtifactId, BuildRequestId, &str), BuildStoreError> {
    match artifact {
        ImageArtifact::Container {
            id,
            build_request_id,
            digest,
            ..
        } => Ok((*id, *build_request_id, digest)),
        ImageArtifact::VirtualMachine { .. } => Err(BuildStoreError::IdentityMismatch),
    }
}

fn canonical_hash<T: serde::Serialize>(value: &T) -> Result<Sha256Digest, BuildStoreError> {
    Sha256Digest::of_canonical(value).map_err(|_| BuildStoreError::ContractInvalid)
}

fn parse_build_state(value: &str) -> Result<InternalAgentBuildState, BuildStoreError> {
    match value {
        "requested" => Ok(InternalAgentBuildState::Requested),
        "running" => Ok(InternalAgentBuildState::Running),
        "succeeded" => Ok(InternalAgentBuildState::Succeeded),
        "failed" => Ok(InternalAgentBuildState::Failed),
        "cancelled" => Ok(InternalAgentBuildState::Cancelled),
        _ => Err(BuildStoreError::ContractInvalid),
    }
}

const fn build_state_name(state: InternalAgentBuildState) -> &'static str {
    match state {
        InternalAgentBuildState::Requested => "requested",
        InternalAgentBuildState::Running => "running",
        InternalAgentBuildState::Succeeded => "succeeded",
        InternalAgentBuildState::Failed => "failed",
        InternalAgentBuildState::Cancelled => "cancelled",
    }
}

fn valid_worker_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
}

fn add_duration(
    timestamp: UtcTimestamp,
    duration: Duration,
) -> Result<UtcTimestamp, BuildStoreError> {
    let duration = time::Duration::try_from(duration).map_err(|_| BuildStoreError::ClockInvalid)?;
    let value = timestamp
        .get()
        .checked_add(duration)
        .ok_or(BuildStoreError::ClockInvalid)?;
    UtcTimestamp::from_utc(value).map_err(|_| BuildStoreError::ClockInvalid)
}

#[derive(Debug, thiserror::Error)]
pub enum BuildStoreError {
    #[error("LW_AGENT_BUILD_STORE_CONFIGURATION_INVALID")]
    ConfigurationInvalid,
    #[error("LW_AGENT_BUILD_CONTRACT_INVALID")]
    ContractInvalid,
    #[error("LW_AGENT_BUILD_IDENTITY_MISMATCH")]
    IdentityMismatch,
    #[error("LW_AGENT_BUILD_AUTHORITY_MISMATCH")]
    AuthorityMismatch,
    #[error("LW_AGENT_BUILD_COURSE_MISMATCH")]
    CourseMismatch,
    #[error("LW_AGENT_BUILD_NOT_FOUND")]
    NotFound,
    #[error("LW_AGENT_BUILD_STATE_CONFLICT")]
    StateConflict,
    #[error("LW_AGENT_BUILD_IDEMPOTENCY_CONFLICT")]
    IdempotencyConflict,
    #[error("LW_AGENT_BUILD_REQUEST_IN_PROGRESS")]
    RequestInProgress,
    #[error("LW_AGENT_BUILD_CANCELLATION_EXPIRED")]
    RequestExpired,
    #[error("LW_AGENT_BUILD_FENCE_LOST")]
    FenceLost,
    #[error("LW_AGENT_BUILD_RETRY_WITHOUT_CLEANUP_FORBIDDEN")]
    RetryUnsafe,
    #[error("LW_AGENT_BUILD_CLOCK_INVALID")]
    ClockInvalid,
    #[error("LW_AGENT_BUILD_PERSISTENCE_FAILED")]
    PersistenceFailed,
    #[error("LW_AGENT_BUILD_DATABASE_FAILED")]
    Database(#[from] sqlx::Error),
}

impl BuildStoreError {
    #[must_use]
    pub const fn retryable(&self) -> bool {
        matches!(self, Self::PersistenceFailed | Self::Database(_))
    }
}
