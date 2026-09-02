//! Durable idempotent `AgentRun` orchestration over the Agent-owned `PostgreSQL` schema.

use persistence_sqlx::Sha256Digest; // internal persistence hash, not contract hash
use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;
use std::time::Duration;

use contracts::authoring::{
    AgentAttempt, AgentAttemptState, AgentRun, AgentRunState, AgentTrackKind,
    CourseLlmEgressPolicy, EnvironmentCandidate, EnvironmentClass, EvaluationCandidate, LlmUsage,
    ProblemPackage,
};
use contracts::diagnostic;
use contracts::events::{AgentRunEvent, CloudEvent, SPEC_VERSION, subjects};
use contracts::http::{CreateAgentRunRequest, IdempotencyKey};
use contracts::{
    AgentRunId, ArtifactId, CourseId, EventId, Revision, Sequence, UtcTimestamp,
};
use persistence_sqlx::{Domain, IdempotencyDecision, IdempotencyStore, OutboxStore};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{PgPool, Row, postgres::PgRow};
use thiserror::Error;
use uuid::Uuid;

use crate::claude_code::{
    ClaudeCodeAudit, ClaudeCodeExecution, ClaudeCodeFailure, ClaudeCodeRuntime,
    ImmutableEgressInput, RunCancellation,
};

use crate::run_helpers::{
    append_claimed_attempt, apply_checkpoint, checkpoint_state,
    environment_checkpoint, evaluation_checkpoint, requested_run, terminal_event,
    validate_reserved_run, validate_reservation, validate_worker, lease_milliseconds,
};

const CREATE_OPERATION: &str = "create_agent_run_v1";
const CANCEL_OPERATION: &str = "cancel_agent_run_v1";
const RETRY_OPERATION: &str = "retry_agent_run_track_v1";

/// Input required to reserve one idempotent `AgentRun`.
pub struct ReserveAgentRun<'a> {
    /// Authoritative course from the authenticated route scope.
    pub course_id: CourseId,
    /// Public immutable create request.
    pub request: &'a CreateAgentRunRequest,
    /// Validated HTTP idempotency key.
    pub idempotency_key: &'a IdempotencyKey,
    /// Egress input already verified against the immutable package.
    pub input: &'a ImmutableEgressInput,
    /// Immutable course policy bound to the runtime.
    pub policy: &'a CourseLlmEgressPolicy,
    /// Event timestamp supplied by the service clock.
    pub now: UtcTimestamp,
    /// Sanitized distributed trace identity.
    pub trace_id: &'a str,
}

/// Immutable Control dispatch retained before any object read or LLM invocation.
#[derive(Clone, Debug)]
pub struct AgentRunDispatchLease {
    /// Authoritative reserved run.
    pub run: AgentRun,
    /// Immutable public create request.
    pub request: CreateAgentRunRequest,
    /// Control-authoritative class required for the Environment track.
    pub expected_environment_class: EnvironmentClass,
    /// Control-verified package contract.
    pub package: ProblemPackage,
    /// Opaque object keys indexed by package artifact identity.
    pub object_locators: BTreeMap<ArtifactId, String>,
    /// Control-verified active course policy.
    pub policy: CourseLlmEgressPolicy,
    /// Original request key used only for exact reservation replay.
    pub idempotency_key: IdempotencyKey,
    /// Sanitized distributed trace identity.
    pub trace_id: String,
    /// Canonical pre-preparation dispatch identity.
    pub dispatch_sha256: Sha256Digest,
    /// Opaque preparation fencing token.
    pub lease_token: Uuid,
}

/// Result of atomically reserving an `AgentRun` request.
#[derive(Clone, Debug, PartialEq)]
pub enum AgentRunReservation {
    /// This caller created the durable run and owns its first execution.
    Created(AgentRun),
    /// The exact request already completed reservation; no LLM call is allowed.
    Replayed(AgentRun),
}

/// Candidate retained in an Agent-owned checkpoint.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "kind", content = "candidate", rename_all = "snake_case")]
pub enum StoredCandidate {
    /// Validated Environment candidate.
    Environment(EnvironmentCandidate),
    /// Validated Evaluation candidate.
    Evaluation(EvaluationCandidate),
}

/// One payload-safe durable checkpoint for an independent track.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentTrackCheckpoint {
    /// Parent `AgentRun` identity.
    pub run_id: AgentRunId,
    /// Monotonic run-local checkpoint sequence.
    pub sequence: u64,
    /// Independent candidate track.
    pub track: AgentTrackKind,
    /// Monotonic track-local attempt number.
    pub attempt: u32,
    /// Hash-only Claude Code runtime evidence.
    pub audit: ClaudeCodeAudit,
    /// Validated candidate, absent on a failed or cancelled attempt.
    pub candidate: Option<StoredCandidate>,
}

/// Atomically retained terminal result.
#[derive(Clone, Debug, PartialEq)]
pub struct StoredAgentRunOutcome {
    /// Terminal aggregate run.
    pub run: AgentRun,
    /// Environment checkpoint.
    pub environment: AgentTrackCheckpoint,
    /// Evaluation checkpoint.
    pub evaluation: AgentTrackCheckpoint,
}

/// One independently committed track result and the aggregate state derived in that transaction.
#[derive(Clone, Debug, PartialEq)]
pub struct StoredAgentTrackOutcome {
    /// Aggregate state after applying this track checkpoint.
    pub run: AgentRun,
    /// Checkpoint committed by this lease owner.
    pub checkpoint: AgentTrackCheckpoint,
}

/// Main-path result distinguishing a new billable run from an idempotent replay.
#[derive(Clone, Debug, PartialEq)]
pub enum AgentRunDispatch {
    /// The caller owned and completed a new dual-track execution.
    Executed(Box<StoredAgentRunOutcome>),
    /// An exact prior reservation was returned without invoking Claude Code.
    Replayed(AgentRun),
    /// This caller completed at least one track while another live lease remains active.
    Progressed(AgentRun),
}

/// One PostgreSQL-authoritative track lease. Its fencing token is opaque to callers.
#[derive(Clone, Debug)]
pub struct AgentTrackLease {
    /// Parent run identity.
    pub run_id: AgentRunId,
    /// Independently scheduled track.
    pub track: AgentTrackKind,
    /// Monotonic attempt owned by this lease.
    pub attempt: u32,
    /// Sanitized worker identity.
    pub worker_id: String,
    lease_token: Uuid,
    cancellation_requested: bool,
}

/// Agent-owned `PostgreSQL` repository using the fixed runtime identity/search path.
#[derive(Clone, Debug)]
pub struct PostgresAgentRunStore {
    pool: PgPool,
}

impl PostgresAgentRunStore {
    /// Creates a repository from an Agent-runtime-only pool.
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Returns the Agent-role pool for authority-local read models.
    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Atomically reserves a Control-verified dispatch for background preparation.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid identities, conflicting idempotency, or persistence failure.
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_lines)]
    pub async fn reserve_dispatch(
        &self,
        course_id: CourseId,
        request: &CreateAgentRunRequest,
        expected_environment_class: EnvironmentClass,
        package: &ProblemPackage,
        object_locators: &BTreeMap<ArtifactId, String>,
        policy: &CourseLlmEgressPolicy,
        idempotency_key: &IdempotencyKey,
        now: UtcTimestamp,
        trace_id: &str,
    ) -> Result<AgentRunReservation, AgentRunStoreError> {
        package
            .validate()
            .map_err(|_| AgentRunStoreError::InvalidContract)?;
        policy
            .validate()
            .map_err(|_| AgentRunStoreError::InvalidContract)?;
        if package.course_id != course_id
            || policy.course_id != course_id
            || request.package_id != package.id
            || request.package_revision != package.revision
            || request.policy_id != policy.id
            || request.policy_revision != policy.revision
        {
            return Err(AgentRunStoreError::IdentityMismatch);
        }
        let expected_artifacts = package
            .files
            .iter()
            .map(|file| file.object.artifact_id)
            .collect::<BTreeSet<_>>();
        if object_locators.keys().copied().collect::<BTreeSet<_>>() != expected_artifacts
            || object_locators.values().any(|key| {
                key.trim().is_empty()
                    || key.contains("..")
                    || key.bytes().any(|byte| byte.is_ascii_control())
            })
        {
            return Err(AgentRunStoreError::IdentityMismatch);
        }
        let request_hash = Sha256Digest::of_canonical(&serde_json::json!({
            "courseId": course_id,
            "request": request,
            "expectedEnvironmentClass": expected_environment_class,
        }))
        .map_err(|_| AgentRunStoreError::InvalidContract)?;
        let dispatch_sha256 = Sha256Digest::of_canonical(&serde_json::json!({
            "request": request,
            "expectedEnvironmentClass": expected_environment_class,
            "package": package,
            "objectLocators": object_locators,
            "policy": policy,
        }))
        .map_err(|_| AgentRunStoreError::InvalidContract)?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        match IdempotencyStore::reserve(
            &mut transaction,
            Domain::Agent,
            CREATE_OPERATION,
            idempotency_key.as_str(),
            request_hash,
        )
        .await
        .map_err(|_| AgentRunStoreError::PersistenceFailed)?
        {
            IdempotencyDecision::Replay(value) => {
                let reserved = decode_run(value)?;
                let run = load_run_for_update(&mut transaction, reserved.id).await?;
                transaction
                    .rollback()
                    .await
                    .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
                return Ok(AgentRunReservation::Replayed(run));
            }
            IdempotencyDecision::Conflict => return Err(AgentRunStoreError::IdempotencyConflict),
            IdempotencyDecision::InProgress => return Err(AgentRunStoreError::RunInProgress),
            IdempotencyDecision::Reserved => {}
        }
        let run = requested_run(request, course_id)?;
        let contract =
            serde_json::to_value(&run).map_err(|_| AgentRunStoreError::InvalidContract)?;
        sqlx::query(
            "INSERT INTO agent.agent_runs (run_id,course_id,problem_package_id,revision,state,provider_binding,input_sha256,policy_revision,contract) \
             VALUES ($1,$2,$3,$4,'requested',$5,$6,$7,$8)",
        )
        .bind(run.id.as_uuid()).bind(course_id.as_uuid()).bind(package.id.as_uuid())
        .bind(run.revision.to_i64().ok_or(AgentRunStoreError::InvalidContract)?).bind(&policy.binding.runtime_binding)
        .bind(dispatch_sha256.to_string()).bind(policy.revision.to_i64().ok_or(AgentRunStoreError::InvalidContract)?).bind(&contract)
        .execute(&mut *transaction).await.map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        for track in [AgentTrackKind::Environment, AgentTrackKind::Evaluation] {
            sqlx::query("INSERT INTO agent.agent_track_work_items (run_id,track,state,input_sha256) VALUES ($1,$2,'requested',$3)")
                .bind(run.id.as_uuid()).bind(track_name(track)).bind(dispatch_sha256.to_string())
                .execute(&mut *transaction).await.map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        }
        sqlx::query(
            "INSERT INTO agent.agent_run_dispatches (run_id,dispatch_sha256,idempotency_key,request,expected_environment_class,package,object_locators,policy,trace_id,state) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,'pending')",
        )
        .bind(run.id.as_uuid()).bind(dispatch_sha256.to_string()).bind(idempotency_key.as_str())
        .bind(serde_json::to_value(request).map_err(|_| AgentRunStoreError::InvalidContract)?)
        .bind(environment_class_name(expected_environment_class))
        .bind(serde_json::to_value(package).map_err(|_| AgentRunStoreError::InvalidContract)?)
        .bind(
            serde_json::to_value(object_locators)
                .map_err(|_| AgentRunStoreError::InvalidContract)?,
        )
        .bind(serde_json::to_value(policy).map_err(|_| AgentRunStoreError::InvalidContract)?)
        .bind(trace_id)
        .execute(&mut *transaction).await.map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        enqueue_run_event(
            &mut transaction,
            &run,
            subjects::AGENT_RUN_REQUESTED,
            1,
            0,
            None,
            now,
            trace_id,
        )
        .await?;
        IdempotencyStore::complete(
            &mut transaction,
            Domain::Agent,
            CREATE_OPERATION,
            idempotency_key.as_str(),
            &contract,
        )
        .await
        .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        transaction
            .commit()
            .await
            .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        Ok(AgentRunReservation::Created(run))
    }

    /// Claims one pending or expired preparation dispatch with a fencing token.
    ///
    /// # Errors
    ///
    /// Returns an error when the lease is invalid or persistence fails.
    pub async fn claim_dispatch(
        &self,
        lease_duration: Duration,
    ) -> Result<Option<AgentRunDispatchLease>, AgentRunStoreError> {
        let lease_milliseconds = lease_milliseconds(lease_duration)?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        let row = sqlx::query(
            "SELECT run_id,dispatch_sha256,idempotency_key,request,expected_environment_class,package,object_locators,policy,trace_id \
             FROM agent.agent_run_dispatches \
             WHERE (state IN ('pending','prepared') OR (state='preparing' AND lease_expires_at <= now())) \
               AND EXISTS (SELECT 1 FROM agent.agent_track_work_items work \
                           WHERE work.run_id=agent_run_dispatches.run_id \
                             AND (work.state='requested' OR \
                                  (work.state='running' AND work.lease_expires_at <= now()))) \
             ORDER BY created_at FOR UPDATE SKIP LOCKED LIMIT 1",
        )
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        let Some(row) = row else {
            transaction
                .rollback()
                .await
                .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
            return Ok(None);
        };
        let run_id = AgentRunId::from_str(
            &row.try_get::<Uuid, _>("run_id")
                .map_err(|_| AgentRunStoreError::InvalidContract)?
                .to_string(),
        )
        .map_err(|_| AgentRunStoreError::InvalidContract)?;
        let lease_token = Uuid::now_v7();
        sqlx::query("UPDATE agent.agent_run_dispatches SET state='preparing',lease_token=$2,lease_expires_at=now()+($3*interval '1 millisecond'),updated_at=now() WHERE run_id=$1")
            .bind(run_id.as_uuid()).bind(lease_token).bind(lease_milliseconds)
            .execute(&mut *transaction).await.map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        let lease = AgentRunDispatchLease {
            run: load_run_for_update(&mut transaction, run_id).await?,
            request: serde_json::from_value(
                row.try_get("request")
                    .map_err(|_| AgentRunStoreError::InvalidContract)?,
            )
            .map_err(|_| AgentRunStoreError::InvalidContract)?,
            expected_environment_class: serde_json::from_value(Value::String(
                row.try_get("expected_environment_class")
                    .map_err(|_| AgentRunStoreError::InvalidContract)?,
            ))
            .map_err(|_| AgentRunStoreError::InvalidContract)?,
            package: serde_json::from_value(
                row.try_get("package")
                    .map_err(|_| AgentRunStoreError::InvalidContract)?,
            )
            .map_err(|_| AgentRunStoreError::InvalidContract)?,
            object_locators: serde_json::from_value(
                row.try_get("object_locators")
                    .map_err(|_| AgentRunStoreError::InvalidContract)?,
            )
            .map_err(|_| AgentRunStoreError::InvalidContract)?,
            policy: serde_json::from_value(
                row.try_get("policy")
                    .map_err(|_| AgentRunStoreError::InvalidContract)?,
            )
            .map_err(|_| AgentRunStoreError::InvalidContract)?,
            idempotency_key: IdempotencyKey::parse(
                &row.try_get::<String, _>("idempotency_key")
                    .map_err(|_| AgentRunStoreError::InvalidContract)?,
            )
            .map_err(|_| AgentRunStoreError::InvalidContract)?,
            trace_id: row
                .try_get("trace_id")
                .map_err(|_| AgentRunStoreError::InvalidContract)?,
            dispatch_sha256: row
                .try_get::<String, _>("dispatch_sha256")
                .map_err(|_| AgentRunStoreError::InvalidContract)?
                .parse()
                .map_err(|_| AgentRunStoreError::InvalidContract)?,
            lease_token,
        };
        transaction
            .commit()
            .await
            .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        Ok(Some(lease))
    }

    /// Rebinds requested work items to the verified prepared input under the dispatch fence.
    ///
    /// # Errors
    ///
    /// Returns an error when the lease is lost, identities conflict, or persistence fails.
    pub async fn bind_prepared_dispatch(
        &self,
        lease: &AgentRunDispatchLease,
        input_sha256: Sha256Digest,
    ) -> Result<(), AgentRunStoreError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        let updated = sqlx::query("UPDATE agent.agent_run_dispatches SET state='prepared',prepared_input_sha256=$3,lease_token=NULL,lease_expires_at=NULL,updated_at=now() WHERE run_id=$1 AND state='preparing' AND lease_token=$2 AND lease_expires_at>now() AND (prepared_input_sha256 IS NULL OR prepared_input_sha256=$3)")
            .bind(lease.run.id.as_uuid()).bind(lease.lease_token).bind(input_sha256.to_string())
            .execute(&mut *transaction).await.map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        if updated.rows_affected() != 1 {
            return Err(AgentRunStoreError::LeaseLost);
        }
        let run = sqlx::query("UPDATE agent.agent_runs SET input_sha256=$2,updated_at=now() WHERE run_id=$1 AND input_sha256 IN ($2,$3)")
            .bind(lease.run.id.as_uuid()).bind(input_sha256.to_string()).bind(lease.dispatch_sha256.to_string())
            .execute(&mut *transaction).await.map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        if run.rows_affected() != 1 {
            return Err(AgentRunStoreError::StateConflict);
        }
        let work = sqlx::query("UPDATE agent.agent_track_work_items SET input_sha256=$2,updated_at=now() WHERE run_id=$1 AND input_sha256 IN ($2,$3)")
            .bind(lease.run.id.as_uuid()).bind(input_sha256.to_string()).bind(lease.dispatch_sha256.to_string())
            .execute(&mut *transaction).await.map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        if work.rows_affected() != 2 {
            return Err(AgentRunStoreError::StateConflict);
        }
        transaction
            .commit()
            .await
            .map_err(|_| AgentRunStoreError::PersistenceFailed)
    }

    /// Terminates both tracks when deterministic preparation fails before any invocation.
    ///
    /// # Errors
    ///
    /// Returns an error when the lease is lost, the diagnostic is invalid, or persistence fails.
    pub async fn fail_dispatch_preparation(
        &self,
        lease: &AgentRunDispatchLease,
        diagnostic_code: &str,
        now: UtcTimestamp,
    ) -> Result<AgentRun, AgentRunStoreError> {
        if diagnostic_code.trim().is_empty() {
            return Err(AgentRunStoreError::InvalidContract);
        }
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        let fenced = sqlx::query_scalar::<_, bool>("SELECT lease_expires_at>now() FROM agent.agent_run_dispatches WHERE run_id=$1 AND state='preparing' AND lease_token=$2 FOR UPDATE")
            .bind(lease.run.id.as_uuid()).bind(lease.lease_token).fetch_optional(&mut *transaction).await.map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        if fenced != Some(true) {
            return Err(AgentRunStoreError::LeaseLost);
        }
        let mut run = load_run_for_update(&mut transaction, lease.run.id).await?;
        let cancellation_requested = sqlx::query_scalar::<_, bool>(
            "SELECT cancellation_requested_at IS NOT NULL FROM agent.agent_runs WHERE run_id=$1",
        )
        .bind(run.id.as_uuid())
        .fetch_one(&mut *transaction)
        .await
        .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        if run.tracks.iter().any(|track| !track.attempts.is_empty()) {
            return Err(AgentRunStoreError::StateConflict);
        }
        let terminal_diagnostic = if cancellation_requested {
            "LW_LLM_CANCELLED"
        } else {
            diagnostic_code
        };
        let attempt_state = if cancellation_requested {
            AgentAttemptState::Cancelled
        } else {
            AgentAttemptState::Failed
        };
        for track in &mut run.tracks {
            track.attempts.push(AgentAttempt {
                number: 1,
                state: attempt_state,
                checkpoint: None,
                usage: zero_usage(),
                usage_observed: false,
                diagnostic_code: Some(terminal_diagnostic.to_owned()),
            });
        }
        run.state = if cancellation_requested {
            AgentRunState::Cancelled
        } else {
            AgentRunState::Failed
        };
        run.revision = run
            .revision
            .next()
            .ok_or(AgentRunStoreError::InvalidContract)?;
        run.validate()
            .map_err(|_| AgentRunStoreError::InvalidContract)?;
        update_run(&mut transaction, &run).await?;
        let work_state = if cancellation_requested {
            "cancelled"
        } else {
            "failed"
        };
        let work = sqlx::query("UPDATE agent.agent_track_work_items SET state=$2,attempt_number=1,worker_id=NULL,lease_token=NULL,lease_expires_at=NULL,heartbeat_at=NULL,updated_at=now() WHERE run_id=$1 AND state='requested'")
            .bind(run.id.as_uuid()).bind(work_state).execute(&mut *transaction).await.map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        if work.rows_affected() != 2 {
            return Err(AgentRunStoreError::StateConflict);
        }
        sqlx::query("UPDATE agent.agent_run_dispatches SET state='failed',terminal_diagnostic=$3,lease_token=NULL,lease_expires_at=NULL,updated_at=now() WHERE run_id=$1 AND lease_token=$2")
            .bind(run.id.as_uuid()).bind(lease.lease_token).bind(terminal_diagnostic).execute(&mut *transaction).await.map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        let sequence = next_outbox_sequence(&mut transaction, run.id).await?;
        enqueue_run_event(
            &mut transaction,
            &run,
            subjects::AGENT_RUN_FAILED,
            sequence,
            1,
            Some(terminal_diagnostic),
            now,
            &lease.trace_id,
        )
        .await?;
        transaction
            .commit()
            .await
            .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        Ok(run)
    }

    /// Atomically reserves an idempotency key, run row and requested Outbox event.
    ///
    /// # Errors
    ///
    /// Returns a stable identity, idempotency, contract or persistence failure.
    pub async fn reserve(
        &self,
        command: ReserveAgentRun<'_>,
    ) -> Result<AgentRunReservation, AgentRunStoreError> {
        validate_reservation(&command)?;
        let request_hash = Sha256Digest::of_canonical(&serde_json::json!({
            "courseId": command.course_id,
            "request": command.request,
        }))
        .map_err(|_| AgentRunStoreError::InvalidContract)?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        match IdempotencyStore::reserve(
            &mut transaction,
            Domain::Agent,
            CREATE_OPERATION,
            command.idempotency_key.as_str(),
            request_hash,
        )
        .await
        .map_err(|_| AgentRunStoreError::PersistenceFailed)?
        {
            IdempotencyDecision::Replay(value) => {
                let reserved = decode_run(value)?;
                let run = load_run_for_update(&mut transaction, reserved.id).await?;
                transaction
                    .rollback()
                    .await
                    .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
                return Ok(AgentRunReservation::Replayed(run));
            }
            IdempotencyDecision::Conflict => return Err(AgentRunStoreError::IdempotencyConflict),
            IdempotencyDecision::InProgress => return Err(AgentRunStoreError::RunInProgress),
            IdempotencyDecision::Reserved => {}
        }

        let run = requested_run(command.request, command.course_id)?;
        let contract =
            serde_json::to_value(&run).map_err(|_| AgentRunStoreError::InvalidContract)?;
        sqlx::query(
            "INSERT INTO agent.agent_runs \
             (run_id, course_id, problem_package_id, revision, state, provider_binding, \
              input_sha256, policy_revision, contract) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(run.id.as_uuid())
        .bind(run.course_id.as_uuid())
        .bind(run.package_id.as_uuid())
        .bind(
            run.revision
                .to_i64()
                .ok_or(AgentRunStoreError::InvalidContract)?,
        )
        .bind("requested")
        .bind(&command.policy.binding.runtime_binding)
        .bind(command.input.sha256().to_string())
        .bind(
            command
                .policy
                .revision
                .to_i64()
                .ok_or(AgentRunStoreError::InvalidContract)?,
        )
        .bind(&contract)
        .execute(&mut *transaction)
        .await
        .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        for track in [AgentTrackKind::Environment, AgentTrackKind::Evaluation] {
            sqlx::query(
                "INSERT INTO agent.agent_track_work_items \
                 (run_id, track, state, input_sha256) VALUES ($1, $2, 'requested', $3)",
            )
            .bind(run.id.as_uuid())
            .bind(track_name(track))
            .bind(command.input.sha256().to_string())
            .execute(&mut *transaction)
            .await
            .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        }
        enqueue_run_event(
            &mut transaction,
            &run,
            subjects::AGENT_RUN_REQUESTED,
            1,
            0,
            None,
            command.now,
            command.trace_id,
        )
        .await?;
        IdempotencyStore::complete(
            &mut transaction,
            Domain::Agent,
            CREATE_OPERATION,
            command.idempotency_key.as_str(),
            &contract,
        )
        .await
        .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        transaction
            .commit()
            .await
            .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        Ok(AgentRunReservation::Created(run))
    }

    /// Atomically claims one requested or expired track with a fencing lease.
    ///
    /// A live or terminal track returns `None`; an expired running attempt is retained as failed
    /// before a new monotonic attempt is appended.
    ///
    /// # Errors
    ///
    /// Returns a stable worker, identity, contract or persistence failure.
    pub async fn claim_track(
        &self,
        run_id: AgentRunId,
        track_kind: AgentTrackKind,
        input_sha256: Sha256Digest,
        worker_id: &str,
        lease_duration: Duration,
    ) -> Result<Option<AgentTrackLease>, AgentRunStoreError> {
        validate_worker(worker_id, lease_duration)?;
        let lease_milliseconds = lease_milliseconds(lease_duration)?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        let mut run = load_run_for_update(&mut transaction, run_id).await?;
        let row = sqlx::query(
            "SELECT work.state, work.input_sha256, work.attempt_number, \
                    work.next_retry_at <= now() AS due, \
                    work.lease_expires_at > now() AS lease_current, \
                    run.cancellation_requested_at IS NOT NULL AS cancellation_requested \
             FROM agent.agent_track_work_items work \
             JOIN agent.agent_runs run ON run.run_id=work.run_id \
             WHERE work.run_id=$1 AND work.track=$2 FOR UPDATE OF work",
        )
        .bind(run_id.as_uuid())
        .bind(track_name(track_kind))
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| AgentRunStoreError::PersistenceFailed)?
        .ok_or(AgentRunStoreError::StateConflict)?;
        let Some(claim) = decode_claimable_track(&row, input_sha256)? else {
            transaction
                .rollback()
                .await
                .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
            return Ok(None);
        };
        let attempt = append_claimed_attempt(&mut run, track_kind, input_sha256, &claim)?;

        let lease_token = Uuid::now_v7();
        let updated = sqlx::query(
            "UPDATE agent.agent_track_work_items \
             SET state='running', attempt_number=$3, worker_id=$4, lease_token=$5, \
                 heartbeat_at=date_trunc('milliseconds', clock_timestamp()), \
                 lease_expires_at=date_trunc('milliseconds', clock_timestamp()) \
                     + ($6 * interval '1 millisecond'), updated_at=now() \
             WHERE run_id=$1 AND track=$2",
        )
        .bind(run_id.as_uuid())
        .bind(track_name(track_kind))
        .bind(i64::from(attempt))
        .bind(worker_id)
        .bind(lease_token)
        .bind(lease_milliseconds)
        .execute(&mut *transaction)
        .await
        .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        if updated.rows_affected() != 1 {
            return Err(AgentRunStoreError::StateConflict);
        }
        update_run(&mut transaction, &run).await?;
        transaction
            .commit()
            .await
            .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        Ok(Some(AgentTrackLease {
            run_id,
            track: track_kind,
            attempt,
            worker_id: worker_id.to_owned(),
            lease_token,
            cancellation_requested: claim.cancellation_requested,
        }))
    }

    /// Renews one exact track lease and reports durable cancellation state.
    ///
    /// # Errors
    ///
    /// Returns `LeaseLost` when ownership, token, state or expiry no longer matches.
    pub async fn heartbeat_track(
        &self,
        lease: &AgentTrackLease,
        lease_duration: Duration,
    ) -> Result<bool, AgentRunStoreError> {
        validate_worker(&lease.worker_id, lease_duration)?;
        let lease_milliseconds = lease_milliseconds(lease_duration)?;
        let cancellation = sqlx::query_scalar::<_, bool>(
            "UPDATE agent.agent_track_work_items work \
             SET heartbeat_at=date_trunc('milliseconds', clock_timestamp()), \
                 lease_expires_at=date_trunc('milliseconds', clock_timestamp()) \
                     + ($5 * interval '1 millisecond'), updated_at=now() \
             FROM agent.agent_runs run \
             WHERE work.run_id=$1 AND work.track=$2 AND work.worker_id=$3 \
               AND work.lease_token=$4 AND work.lease_expires_at > now() \
               AND work.state='running' AND run.run_id=work.run_id \
             RETURNING run.cancellation_requested_at IS NOT NULL",
        )
        .bind(lease.run_id.as_uuid())
        .bind(track_name(lease.track))
        .bind(&lease.worker_id)
        .bind(lease.lease_token)
        .bind(lease_milliseconds)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        cancellation.ok_or(AgentRunStoreError::LeaseLost)
    }

    /// Durably requests cancellation so any current lease owner observes it on heartbeat.
    ///
    /// # Errors
    ///
    /// Returns a stable missing-run, contract or persistence failure.
    pub async fn request_cancellation(
        &self,
        run_id: AgentRunId,
        now: UtcTimestamp,
    ) -> Result<AgentRun, AgentRunStoreError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        let mut run = load_run_for_update(&mut transaction, run_id).await?;
        if matches!(
            run.state,
            AgentRunState::PartiallySucceeded
                | AgentRunState::Succeeded
                | AgentRunState::Failed
                | AgentRunState::Cancelled
        ) {
            transaction
                .rollback()
                .await
                .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
            return Ok(run);
        }
        if run.state == AgentRunState::Running {
            run.state = AgentRunState::Cancelling;
            run.revision = run
                .revision
                .next()
                .ok_or(AgentRunStoreError::InvalidContract)?;
            run.validate()
                .map_err(|_| AgentRunStoreError::InvalidContract)?;
            update_run(&mut transaction, &run).await?;
        }
        sqlx::query(
            "UPDATE agent.agent_runs \
             SET cancellation_requested_at=COALESCE(cancellation_requested_at, $2), \
                 updated_at=now() WHERE run_id=$1",
        )
        .bind(run_id.as_uuid())
        .bind(now.get())
        .execute(&mut *transaction)
        .await
        .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        transaction
            .commit()
            .await
            .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        Ok(run)
    }

    /// Idempotently requests cancellation at one exact run revision.
    ///
    /// # Errors
    ///
    /// Returns an error for stale revision, conflicting idempotency, or persistence failure.
    pub async fn request_cancellation_revisioned(
        &self,
        course_id: CourseId,
        run_id: AgentRunId,
        expected_revision: Revision,
        idempotency_key: &IdempotencyKey,
        now: UtcTimestamp,
    ) -> Result<AgentRun, AgentRunStoreError> {
        let request_hash = Sha256Digest::of_canonical(&serde_json::json!({
            "courseId": course_id,
            "runId": run_id,
            "expectedRevision": expected_revision,
        }))
        .map_err(|_| AgentRunStoreError::InvalidContract)?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        match IdempotencyStore::reserve(
            &mut transaction,
            Domain::Agent,
            CANCEL_OPERATION,
            idempotency_key.as_str(),
            request_hash,
        )
        .await
        .map_err(|_| AgentRunStoreError::PersistenceFailed)?
        {
            IdempotencyDecision::Replay(value) => {
                transaction
                    .rollback()
                    .await
                    .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
                return decode_run(value);
            }
            IdempotencyDecision::Conflict => return Err(AgentRunStoreError::IdempotencyConflict),
            IdempotencyDecision::InProgress => return Err(AgentRunStoreError::RunInProgress),
            IdempotencyDecision::Reserved => {}
        }
        let mut run = load_run_for_update(&mut transaction, run_id).await?;
        if run.course_id != course_id {
            return Err(AgentRunStoreError::CourseMismatch);
        }
        if run.revision != expected_revision {
            return Err(AgentRunStoreError::StateConflict);
        }
        if !is_terminal_run(run.state) {
            if run.state == AgentRunState::Running {
                run.state = AgentRunState::Cancelling;
                run.revision = run
                    .revision
                    .next()
                    .ok_or(AgentRunStoreError::InvalidContract)?;
                run.validate()
                    .map_err(|_| AgentRunStoreError::InvalidContract)?;
                update_run(&mut transaction, &run).await?;
            }
            sqlx::query("UPDATE agent.agent_runs SET cancellation_requested_at=COALESCE(cancellation_requested_at,$2),updated_at=now() WHERE run_id=$1")
                .bind(run_id.as_uuid()).bind(now.get()).execute(&mut *transaction).await.map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        }
        let result = serde_json::to_value(&run).map_err(|_| AgentRunStoreError::InvalidContract)?;
        IdempotencyStore::complete(
            &mut transaction,
            Domain::Agent,
            CANCEL_OPERATION,
            idempotency_key.as_str(),
            &result,
        )
        .await
        .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        transaction
            .commit()
            .await
            .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        Ok(run)
    }

    /// Idempotently requeues one failed or cancelled track at an exact run revision.
    ///
    /// # Errors
    ///
    /// Returns an error for stale revision, invalid track state, or persistence failure.
    pub async fn retry_track_revisioned(
        &self,
        course_id: CourseId,
        run_id: AgentRunId,
        track: AgentTrackKind,
        expected_revision: Revision,
        idempotency_key: &IdempotencyKey,
    ) -> Result<AgentRun, AgentRunStoreError> {
        let request_hash = Sha256Digest::of_canonical(&serde_json::json!({
            "courseId": course_id,
            "runId": run_id,
            "track": track,
            "expectedRevision": expected_revision,
        }))
        .map_err(|_| AgentRunStoreError::InvalidContract)?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        match IdempotencyStore::reserve(
            &mut transaction,
            Domain::Agent,
            RETRY_OPERATION,
            idempotency_key.as_str(),
            request_hash,
        )
        .await
        .map_err(|_| AgentRunStoreError::PersistenceFailed)?
        {
            IdempotencyDecision::Replay(value) => {
                transaction
                    .rollback()
                    .await
                    .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
                return decode_run(value);
            }
            IdempotencyDecision::Conflict => return Err(AgentRunStoreError::IdempotencyConflict),
            IdempotencyDecision::InProgress => return Err(AgentRunStoreError::RunInProgress),
            IdempotencyDecision::Reserved => {}
        }
        let mut run = load_run_for_update(&mut transaction, run_id).await?;
        if run.course_id != course_id {
            return Err(AgentRunStoreError::CourseMismatch);
        }
        if run.revision != expected_revision
            || !matches!(
                run.state,
                AgentRunState::Failed
                    | AgentRunState::PartiallySucceeded
                    | AgentRunState::Cancelled
            )
        {
            return Err(AgentRunStoreError::StateConflict);
        }
        let selected = run
            .tracks
            .iter()
            .find(|candidate| candidate.kind == track)
            .ok_or(AgentRunStoreError::InvalidContract)?;
        if !matches!(
            selected.attempts.last().map(|attempt| attempt.state),
            Some(AgentAttemptState::Failed | AgentAttemptState::Cancelled)
        ) {
            return Err(AgentRunStoreError::StateConflict);
        }
        let updated = sqlx::query("UPDATE agent.agent_track_work_items SET state='requested',next_retry_at=now(),worker_id=NULL,lease_token=NULL,lease_expires_at=NULL,heartbeat_at=NULL,updated_at=now() WHERE run_id=$1 AND track=$2 AND state IN ('failed','cancelled')")
            .bind(run_id.as_uuid()).bind(track_name(track)).execute(&mut *transaction).await.map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        if updated.rows_affected() != 1 {
            return Err(AgentRunStoreError::StateConflict);
        }
        run.revision = run
            .revision
            .next()
            .ok_or(AgentRunStoreError::InvalidContract)?;
        run.validate()
            .map_err(|_| AgentRunStoreError::InvalidContract)?;
        update_run(&mut transaction, &run).await?;
        let result = serde_json::to_value(&run).map_err(|_| AgentRunStoreError::InvalidContract)?;
        IdempotencyStore::complete(
            &mut transaction,
            Domain::Agent,
            RETRY_OPERATION,
            idempotency_key.as_str(),
            &result,
        )
        .await
        .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        transaction
            .commit()
            .await
            .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        Ok(run)
    }

    /// Commits one terminal track checkpoint through its current fencing token.
    ///
    /// The checkpoint is durable immediately; aggregate terminal state and the terminal Outbox
    /// event are emitted only when both tracks have reached a terminal attempt.
    ///
    /// # Errors
    ///
    /// Returns a stable lease, identity, contract or persistence failure.
    pub async fn complete_track(
        &self,
        lease: &AgentTrackLease,
        outcome: Result<ClaudeCodeExecution, ClaudeCodeFailure>,
        now: UtcTimestamp,
        trace_id: &str,
    ) -> Result<StoredAgentTrackOutcome, AgentRunStoreError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        let mut run = load_run_for_update(&mut transaction, lease.run_id).await?;
        let row = sqlx::query(
            "SELECT work.lease_expires_at > now() AS lease_current, \
                    run.cancellation_requested_at IS NOT NULL AS cancellation_requested \
             FROM agent.agent_track_work_items work \
             JOIN agent.agent_runs run ON run.run_id=work.run_id \
             WHERE work.run_id=$1 AND work.track=$2 AND work.state='running' \
               AND work.worker_id=$3 AND work.lease_token=$4 \
               AND work.attempt_number=$5 FOR UPDATE OF work",
        )
        .bind(lease.run_id.as_uuid())
        .bind(track_name(lease.track))
        .bind(&lease.worker_id)
        .bind(lease.lease_token)
        .bind(i64::from(lease.attempt))
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| AgentRunStoreError::PersistenceFailed)?
        .ok_or(AgentRunStoreError::LeaseLost)?;
        if row
            .try_get::<Option<bool>, _>("lease_current")
            .map_err(|_| AgentRunStoreError::InvalidContract)?
            != Some(true)
        {
            return Err(AgentRunStoreError::LeaseLost);
        }
        let cancellation_requested = row
            .try_get::<bool, _>("cancellation_requested")
            .map_err(|_| AgentRunStoreError::InvalidContract)?;
        let checkpoint = match lease.track {
            AgentTrackKind::Environment => {
                environment_checkpoint(&run, lease.attempt, outcome, now)?
            }
            AgentTrackKind::Evaluation => evaluation_checkpoint(&run, lease.attempt, outcome, now)?,
        };
        apply_checkpoint(&mut run, &checkpoint)?;
        let derived = run
            .derived_state()
            .map_err(|_| AgentRunStoreError::InvalidContract)?;
        run.state = if cancellation_requested && derived == AgentRunState::Running {
            AgentRunState::Cancelling
        } else {
            derived
        };
        run.revision = run
            .revision
            .next()
            .ok_or(AgentRunStoreError::InvalidContract)?;
        run.validate()
            .map_err(|_| AgentRunStoreError::InvalidContract)?;

        insert_checkpoint(&mut transaction, &checkpoint).await?;
        update_run(&mut transaction, &run).await?;
        let work_state = checkpoint_state(&checkpoint);
        let updated = sqlx::query(
            "UPDATE agent.agent_track_work_items \
             SET state=$6, worker_id=NULL, lease_token=NULL, lease_expires_at=NULL, \
                 heartbeat_at=NULL, updated_at=now() \
             WHERE run_id=$1 AND track=$2 AND worker_id=$3 AND lease_token=$4 \
               AND attempt_number=$5",
        )
        .bind(lease.run_id.as_uuid())
        .bind(track_name(lease.track))
        .bind(&lease.worker_id)
        .bind(lease.lease_token)
        .bind(i64::from(lease.attempt))
        .bind(work_state)
        .execute(&mut *transaction)
        .await
        .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        if updated.rows_affected() != 1 {
            return Err(AgentRunStoreError::LeaseLost);
        }
        if is_terminal_run(run.state) {
            let (subject, diagnostic_code) = terminal_event(&run);
            let sequence = next_outbox_sequence(&mut transaction, run.id).await?;
            enqueue_run_event(
                &mut transaction,
                &run,
                subject,
                sequence,
                u64::from(lease.attempt),
                diagnostic_code,
                now,
                trace_id,
            )
            .await?;
        }
        transaction
            .commit()
            .await
            .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        Ok(StoredAgentTrackOutcome { run, checkpoint })
    }

    /// Loads and validates the authoritative run contract.
    ///
    /// # Errors
    ///
    /// Returns a stable missing-run, contract or persistence failure.
    pub async fn load(&self, run_id: AgentRunId) -> Result<AgentRun, AgentRunStoreError> {
        let row = sqlx::query(
            "SELECT course_id, problem_package_id, revision, state, input_sha256, \
                    policy_revision, contract \
             FROM agent.agent_runs WHERE run_id = $1",
        )
        .bind(run_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| AgentRunStoreError::PersistenceFailed)?
        .ok_or(AgentRunStoreError::RunNotFound)?;
        decode_run_row(&row)
    }

    /// Loads both retained checkpoints in sequence order for recovery or projection.
    ///
    /// # Errors
    ///
    /// Returns a stable contract or persistence failure.
    pub async fn load_checkpoints(
        &self,
        run_id: AgentRunId,
    ) -> Result<Vec<AgentTrackCheckpoint>, AgentRunStoreError> {
        let values = sqlx::query_scalar::<_, Value>(
            "SELECT contract FROM agent.agent_checkpoints \
             WHERE run_id = $1 ORDER BY checkpoint_sequence",
        )
        .bind(run_id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        values
            .into_iter()
            .map(|value| {
                serde_json::from_value(value).map_err(|_| AgentRunStoreError::InvalidContract)
            })
            .collect()
    }

    /// Loads the terminal aggregate together with the latest checkpoint for each track.
    ///
    /// # Errors
    ///
    /// Returns a stable state, contract or persistence failure.
    pub async fn load_terminal_outcome(
        &self,
        run_id: AgentRunId,
    ) -> Result<Option<StoredAgentRunOutcome>, AgentRunStoreError> {
        let run = self.load(run_id).await?;
        if !is_terminal_run(run.state) {
            return Ok(None);
        }
        let checkpoints = self.load_checkpoints(run_id).await?;
        let environment = checkpoints
            .iter()
            .rev()
            .find(|checkpoint| checkpoint.track == AgentTrackKind::Environment)
            .cloned()
            .ok_or(AgentRunStoreError::InvalidContract)?;
        let evaluation = checkpoints
            .iter()
            .rev()
            .find(|checkpoint| checkpoint.track == AgentTrackKind::Evaluation)
            .cloned()
            .ok_or(AgentRunStoreError::InvalidContract)?;
        Ok(Some(StoredAgentRunOutcome {
            run,
            environment,
            evaluation,
        }))
    }
}

/// Coordinates reservation, exactly-one execution ownership and terminal persistence.
#[derive(Clone)]
pub struct AgentRunService {
    store: PostgresAgentRunStore,
    runtime: ClaudeCodeRuntime,
    worker_id: String,
    lease_duration: Duration,
}

/// Complete service command for an idempotent dual-track execution.
pub struct ExecuteAgentRun<'a> {
    /// Authoritative course route scope.
    pub course_id: CourseId,
    /// Immutable public create request.
    pub request: &'a CreateAgentRunRequest,
    /// Control-authoritative class required for the Environment candidate.
    pub expected_environment_class: EnvironmentClass,
    /// Validated HTTP idempotency key.
    pub idempotency_key: &'a IdempotencyKey,
    /// Verified and classified immutable egress input.
    pub input: ImmutableEgressInput,
    /// Authoritative cancellation channel.
    pub cancellation: RunCancellation,
    /// Service clock value used by all records in this operation.
    pub now: UtcTimestamp,
    /// Sanitized distributed trace identity.
    pub trace_id: &'a str,
}

impl std::fmt::Debug for AgentRunService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentRunService")
            .field("store", &self.store)
            .field("runtime", &self.runtime)
            .field("worker_id", &self.worker_id)
            .field("lease_duration", &self.lease_duration)
            .finish()
    }
}

impl AgentRunService {
    /// Creates a service from explicit durable store, runtime and worker lease bindings.
    ///
    /// # Errors
    ///
    /// Rejects an unsafe worker identity or lease duration.
    pub fn new(
        store: PostgresAgentRunStore,
        runtime: ClaudeCodeRuntime,
        worker_id: String,
        lease_duration: Duration,
    ) -> Result<Self, AgentRunStoreError> {
        validate_worker(&worker_id, lease_duration)?;
        Ok(Self {
            store,
            runtime,
            worker_id,
            lease_duration,
        })
    }

    /// Executes a new idempotent run or returns the exact reserved run without another LLM call.
    ///
    /// # Errors
    ///
    /// Returns the stable reservation, runtime-state, contract or persistence failure.
    pub async fn execute(
        &self,
        command: ExecuteAgentRun<'_>,
    ) -> Result<AgentRunDispatch, AgentRunStoreError> {
        let reservation = self
            .store
            .reserve(ReserveAgentRun {
                course_id: command.course_id,
                request: command.request,
                idempotency_key: command.idempotency_key,
                input: &command.input,
                policy: self.runtime.policy(),
                now: command.now,
                trace_id: command.trace_id,
            })
            .await?;
        let run = match reservation {
            AgentRunReservation::Created(run) | AgentRunReservation::Replayed(run) => run,
        };
        self.execute_reserved(command, run).await
    }

    /// Executes a dispatch whose durable run was already reserved by the Control-facing
    /// dispatch boundary.
    ///
    /// `reserve_dispatch` and `reserve` intentionally use different request hashes: the
    /// former binds the Control-verified package, policy and required environment class, while
    /// the latter is the public `AgentRun` reservation path. Calling `execute` from the background
    /// dispatch worker therefore attempts to reserve the same idempotency key a second time and
    /// turns every valid Work dispatch into `LW_IDEMPOTENCY_CONFLICT`. The worker must execute
    /// the already reserved run through this method instead.
    ///
    /// # Errors
    ///
    /// Returns the stable runtime-state, contract or persistence failure from the reserved run.
    pub async fn execute_reserved(
        &self,
        command: ExecuteAgentRun<'_>,
        run: AgentRun,
    ) -> Result<AgentRunDispatch, AgentRunStoreError> {
        validate_reserved_run(&command, &run)?;
        let input_sha256 = command.input.sha256();
        let environment = self
            .store
            .claim_track(
                run.id,
                AgentTrackKind::Environment,
                input_sha256,
                &self.worker_id,
                self.lease_duration,
            )
            .await?;
        let evaluation = self
            .store
            .claim_track(
                run.id,
                AgentTrackKind::Evaluation,
                input_sha256,
                &self.worker_id,
                self.lease_duration,
            )
            .await?;
        let environment_execution = self.execute_track(
            environment,
            command.input.clone(),
            command.cancellation.clone(),
            command.now,
            command.trace_id,
            command.expected_environment_class,
        );
        let evaluation_execution = self.execute_track(
            evaluation,
            command.input,
            command.cancellation,
            command.now,
            command.trace_id,
            command.expected_environment_class,
        );
        let (environment_executed, evaluation_executed) =
            tokio::join!(environment_execution, evaluation_execution);
        let executed = environment_executed? | evaluation_executed?;
        let current = self.store.load(run.id).await?;
        if is_terminal_run(current.state) {
            let stored = self
                .store
                .load_terminal_outcome(run.id)
                .await?
                .ok_or(AgentRunStoreError::InvalidContract)?;
            return if executed {
                Ok(AgentRunDispatch::Executed(Box::new(stored)))
            } else {
                Ok(AgentRunDispatch::Replayed(stored.run))
            };
        }
        if executed {
            Ok(AgentRunDispatch::Progressed(current))
        } else {
            Ok(AgentRunDispatch::Replayed(current))
        }
    }

    async fn execute_track(
        &self,
        lease: Option<AgentTrackLease>,
        input: ImmutableEgressInput,
        cancellation: RunCancellation,
        now: UtcTimestamp,
        trace_id: &str,
        expected_environment_class: EnvironmentClass,
    ) -> Result<bool, AgentRunStoreError> {
        let Some(lease) = lease else {
            return Ok(false);
        };
        if lease.cancellation_requested {
            cancellation.cancel();
        }
        let generation = self.runtime.generate_for_class(
            lease.track,
            input,
            cancellation.clone(),
            expected_environment_class,
        );
        tokio::pin!(generation);
        let heartbeat_period = self
            .lease_duration
            .checked_div(3)
            .unwrap_or(self.lease_duration)
            .max(Duration::from_millis(10));
        let mut heartbeat = tokio::time::interval(heartbeat_period);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        heartbeat.tick().await;
        let outcome = loop {
            tokio::select! {
                biased;
                outcome = &mut generation => break outcome,
                _ = heartbeat.tick() => {
                    match self.store.heartbeat_track(&lease, self.lease_duration).await {
                        Ok(true) => cancellation.cancel(),
                        Ok(false) => {}
                        Err(error) => return Err(error),
                    }
                }
            }
        };
        self.store
            .complete_track(&lease, outcome, now, trace_id)
            .await?;
        Ok(true)
    }
}

pub(crate) struct TrackClaim {
    pub(crate) state: String,
    pub(crate) attempt_number: i64,
    pub(crate) cancellation_requested: bool,
}

fn decode_claimable_track(
    row: &PgRow,
    input_sha256: Sha256Digest,
) -> Result<Option<TrackClaim>, AgentRunStoreError> {
    let state = row
        .try_get::<String, _>("state")
        .map_err(|_| AgentRunStoreError::InvalidContract)?;
    let stored_input = row
        .try_get::<String, _>("input_sha256")
        .ok()
        .and_then(|value| Sha256Digest::from_str(&value).ok())
        .ok_or(AgentRunStoreError::InvalidContract)?;
    if stored_input != input_sha256 {
        return Err(AgentRunStoreError::IdentityMismatch);
    }
    let due = row
        .try_get::<bool, _>("due")
        .map_err(|_| AgentRunStoreError::InvalidContract)?;
    let lease_current = row
        .try_get::<Option<bool>, _>("lease_current")
        .map_err(|_| AgentRunStoreError::InvalidContract)?
        .unwrap_or(false);
    if matches!(state.as_str(), "succeeded" | "failed" | "cancelled")
        || !due
        || (state == "running" && lease_current)
    {
        return Ok(None);
    }
    if !matches!(state.as_str(), "requested" | "running") {
        return Err(AgentRunStoreError::InvalidContract);
    }
    Ok(Some(TrackClaim {
        state,
        attempt_number: row
            .try_get("attempt_number")
            .map_err(|_| AgentRunStoreError::InvalidContract)?,
        cancellation_requested: row
            .try_get("cancellation_requested")
            .map_err(|_| AgentRunStoreError::InvalidContract)?,
    }))
}

const fn environment_class_name(value: EnvironmentClass) -> &'static str {
    match value {
        EnvironmentClass::Experiment => "experiment",
        EnvironmentClass::Work => "work",
    }
}

async fn load_run_for_update(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    run_id: AgentRunId,
) -> Result<AgentRun, AgentRunStoreError> {
    let row = sqlx::query(
        "SELECT course_id, problem_package_id, revision, state, input_sha256, \
                policy_revision, contract \
         FROM agent.agent_runs WHERE run_id = $1 FOR UPDATE",
    )
    .bind(run_id.as_uuid())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|_| AgentRunStoreError::PersistenceFailed)?
    .ok_or(AgentRunStoreError::RunNotFound)?;
    decode_run_row(&row)
}

fn decode_run(value: Value) -> Result<AgentRun, AgentRunStoreError> {
    let run = serde_json::from_value::<AgentRun>(value)
        .map_err(|_| AgentRunStoreError::InvalidContract)?;
    run.validate()
        .map_err(|_| AgentRunStoreError::InvalidContract)?;
    Ok(run)
}

fn decode_run_row(row: &PgRow) -> Result<AgentRun, AgentRunStoreError> {
    let value = row
        .try_get::<Value, _>("contract")
        .map_err(|_| AgentRunStoreError::InvalidContract)?;
    let run = decode_run(value)?;
    let course_id = row
        .try_get::<uuid::Uuid, _>("course_id")
        .map_err(|_| AgentRunStoreError::InvalidContract)?;
    let package_id = row
        .try_get::<uuid::Uuid, _>("problem_package_id")
        .map_err(|_| AgentRunStoreError::InvalidContract)?;
    let revision = row
        .try_get::<i64, _>("revision")
        .map_err(|_| AgentRunStoreError::InvalidContract)?;
    let policy_revision = row
        .try_get::<i64, _>("policy_revision")
        .map_err(|_| AgentRunStoreError::InvalidContract)?;
    let state = row
        .try_get::<String, _>("state")
        .map_err(|_| AgentRunStoreError::InvalidContract)?;
    let _input_sha256 = row
        .try_get::<String, _>("input_sha256")
        .ok()
        .and_then(|value| Sha256Digest::from_str(&value).ok());
    if course_id != run.course_id.as_uuid()
        || package_id != run.package_id.as_uuid()
        || u64::try_from(revision).ok() != Some(run.revision.get())
        || u64::try_from(policy_revision).ok() != Some(run.policy_revision.get())
        || state != run_state(run.state)
    {
        return Err(AgentRunStoreError::InvalidContract);
    }
    Ok(run)
}

async fn update_run(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    run: &AgentRun,
) -> Result<(), AgentRunStoreError> {
    let contract = serde_json::to_value(run).map_err(|_| AgentRunStoreError::InvalidContract)?;
    let updated = sqlx::query(
        "UPDATE agent.agent_runs SET revision = $2, state = $3, contract = $4, updated_at = now() \
         WHERE run_id = $1 AND revision = $5",
    )
    .bind(run.id.as_uuid())
    .bind(
        run.revision
            .to_i64()
            .ok_or(AgentRunStoreError::InvalidContract)?,
    )
    .bind(run_state(run.state))
    .bind(contract)
    .bind(
        i64::try_from(
            run.revision
                .get()
                .checked_sub(1)
                .ok_or(AgentRunStoreError::InvalidContract)?,
        )
        .map_err(|_| AgentRunStoreError::InvalidContract)?,
    )
    .execute(&mut **transaction)
    .await
    .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
    if updated.rows_affected() != 1 {
        return Err(AgentRunStoreError::StateConflict);
    }
    Ok(())
}

async fn insert_checkpoint(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    checkpoint: &AgentTrackCheckpoint,
) -> Result<(), AgentRunStoreError> {
    let contract =
        serde_json::to_value(checkpoint).map_err(|_| AgentRunStoreError::InvalidContract)?;
    let hash =
        Sha256Digest::of_canonical(&contract).map_err(|_| AgentRunStoreError::InvalidContract)?;
    sqlx::query(
        "INSERT INTO agent.agent_checkpoints \
         (run_id, checkpoint_sequence, checkpoint_sha256, state, contract) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(checkpoint.run_id.as_uuid())
    .bind(i64::try_from(checkpoint.sequence).map_err(|_| AgentRunStoreError::InvalidContract)?)
    .bind(hash.to_string())
    .bind(checkpoint_state(checkpoint))
    .bind(contract)
    .execute(&mut **transaction)
    .await
    .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn enqueue_run_event(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    run: &AgentRun,
    subject: &'static str,
    sequence: u64,
    attempt: u64,
    diagnostic_code: Option<&str>,
    now: UtcTimestamp,
    trace_id: &str,
) -> Result<(), AgentRunStoreError> {
    let contract = contracts::events::EventContract::by_subject(subject)
        .ok_or(AgentRunStoreError::InvalidContract)?;
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
        course_id: run.course_id,
        aggregate_revision: run.revision,
        aggregate_sequence: Sequence(sequence),
        trace_id: trace_id.to_owned(),
        data: AgentRunEvent {
            run_id: run.id,
            attempt,
            state: run_state(run.state).to_owned(),
            diagnostic_code: diagnostic_code.map(str::to_owned),
        },
    };
    event
        .validate(contract)
        .map_err(|_| AgentRunStoreError::InvalidContract)?;
    let payload = serde_json::to_value(&event).map_err(|_| AgentRunStoreError::InvalidContract)?;
    let payload_hash =
        Sha256Digest::of_canonical(&payload).map_err(|_| AgentRunStoreError::InvalidContract)?;
    OutboxStore::enqueue(
        transaction,
        Domain::Agent,
        event_id.as_uuid(),
        subject,
        subject,
        run.id.as_uuid(),
        sequence,
        &payload,
        payload_hash,
    )
    .await
    .map_err(|_| AgentRunStoreError::PersistenceFailed)
}

async fn next_outbox_sequence(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    run_id: AgentRunId,
) -> Result<u64, AgentRunStoreError> {
    let next = sqlx::query_scalar::<_, i64>(
        "SELECT COALESCE(MAX(aggregate_sequence),0)+1 FROM agent.outbox_events WHERE aggregate_id=$1",
    )
    .bind(run_id.as_uuid())
    .fetch_one(&mut **transaction)
    .await
    .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
    u64::try_from(next).map_err(|_| AgentRunStoreError::InvalidContract)
}

const fn track_name(track: AgentTrackKind) -> &'static str {
    match track {
        AgentTrackKind::Environment => "environment",
        AgentTrackKind::Evaluation => "evaluation",
    }
}

const fn is_terminal_run(state: AgentRunState) -> bool {
    matches!(
        state,
        AgentRunState::PartiallySucceeded
            | AgentRunState::Succeeded
            | AgentRunState::Failed
            | AgentRunState::Cancelled
    )
}

const fn run_state(state: AgentRunState) -> &'static str {
    match state {
        AgentRunState::Requested => "requested",
        AgentRunState::Running => "running",
        AgentRunState::PartiallySucceeded => "partially_succeeded",
        AgentRunState::Succeeded => "succeeded",
        AgentRunState::Failed => "failed",
        AgentRunState::Cancelling => "cancelling",
        AgentRunState::Cancelled => "cancelled",
    }
}

pub(crate) const fn zero_usage() -> LlmUsage {
    LlmUsage {
        input_tokens: 0,
        output_tokens: 0,
        requests: 0,
        cost_microusd: 0,
    }
}

/// Stable, payload-free durable `AgentRun` failures.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum AgentRunStoreError {
    /// The caller's course authority does not own the target run.
    #[error("LW_AUTH_COURSE_SCOPE_DENIED: AgentRun course authority does not match")]
    CourseMismatch,
    /// Request, package, policy or input identity differs.
    #[error("LW_LLM_POLICY_REVISION_MISMATCH: AgentRun immutable identity does not match")]
    IdentityMismatch,
    /// Same idempotency key was used for a different request.
    #[error("LW_IDEMPOTENCY_CONFLICT: idempotency key request identity differs")]
    IdempotencyConflict,
    /// A prior reservation has not reached its durable result.
    #[error("LW_AGENT_RUN_STATE_CONFLICT: AgentRun reservation is still in progress")]
    RunInProgress,
    /// Requested transition does not own the current state.
    #[error("LW_AGENT_RUN_STATE_CONFLICT: AgentRun state or revision changed")]
    StateConflict,
    /// Requested run does not exist.
    #[error("LW_AGENT_RUN_STATE_CONFLICT: AgentRun does not exist")]
    RunNotFound,
    /// Worker identity or lease duration is unsafe.
    #[error("LW_AGENT_RUNTIME_IDENTITY_INVALID: Agent worker lease binding is invalid")]
    WorkerIdentityInvalid,
    /// The worker no longer owns the current live fencing token.
    #[error("LW_AGENT_RUN_STATE_CONFLICT: Agent track lease was lost")]
    LeaseLost,
    /// A typed contract, event, checkpoint or numeric identity is invalid.
    #[error("LW_CONTRACT_DOCUMENT_INVALID: AgentRun durable contract is invalid")]
    InvalidContract,
    /// `PostgreSQL` or domain-ledger operation failed.
    #[error("LW_AGENT_PERSISTENCE_FAILED: AgentRun persistence failed")]
    PersistenceFailed,
}

impl AgentRunStoreError {
    /// Returns the stable root-cause diagnostic.
    #[must_use]
    pub const fn diagnostic_code(self) -> &'static str {
        match self {
            Self::CourseMismatch => diagnostic::ACCESS_DENIED,
            Self::IdentityMismatch
            | Self::LeaseLost
            | Self::RunInProgress
            | Self::StateConflict
            | Self::RunNotFound => diagnostic::CONFLICT,
            Self::IdempotencyConflict => diagnostic::IDEMPOTENCY_CONFLICT,
            Self::WorkerIdentityInvalid => diagnostic::INVALID_REQUEST,
            Self::InvalidContract => diagnostic::CONTRACT_DOCUMENT_INVALID,
            Self::PersistenceFailed => diagnostic::DATABASE_FAILED,
        }
    }
}
