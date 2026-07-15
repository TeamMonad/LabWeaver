//! Durable idempotent `AgentRun` orchestration over the Agent-owned `PostgreSQL` schema.

use std::str::FromStr;
use std::time::Duration;

use contracts::authoring::{
    AgentAttempt, AgentAttemptState, AgentRun, AgentRunState, AgentTrack, AgentTrackKind,
    CourseLlmEgressPolicy, EnvironmentCandidate, EvaluationCandidate, LlmUsage,
};
use contracts::diagnostic;
use contracts::events::{
    AgentRunEvent, CloudEvent, EVENT_CONTRACTS, EventContract, SPEC_VERSION, subjects,
};
use contracts::http::{CreateAgentRunRequest, IdempotencyKey};
use contracts::{
    AgentRunId, CandidateId, CourseId, EventId, Revision, Sequence, Sha256Digest, UtcTimestamp,
};
use persistence_sqlx::{Domain, IdempotencyDecision, IdempotencyStore, OutboxStore};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{PgPool, Row, postgres::PgRow};
use thiserror::Error;
use uuid::Uuid;

use crate::claude_code::{
    CandidateDocument, ClaudeCodeAudit, ClaudeCodeExecution, ClaudeCodeFailure, ClaudeCodeRuntime,
    ImmutableEgressInput, RunCancellation, RuntimeAuditOutcome,
};

const CREATE_OPERATION: &str = "create_agent_run_v1";

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
        .bind(revision_i64(run.revision)?)
        .bind("requested")
        .bind(&command.policy.binding.runtime_binding)
        .bind(command.input.sha256().to_string())
        .bind(revision_i64(command.policy.revision)?)
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
            run.revision = next_revision(run.revision)?;
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
        run.revision = next_revision(run.revision)?;
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
            enqueue_run_event(
                &mut transaction,
                &run,
                subject,
                2,
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
        if is_terminal_run(run.state) {
            return Ok(AgentRunDispatch::Replayed(run));
        }
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
        );
        let evaluation_execution = self.execute_track(
            evaluation,
            command.input,
            command.cancellation,
            command.now,
            command.trace_id,
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
    ) -> Result<bool, AgentRunStoreError> {
        let Some(lease) = lease else {
            return Ok(false);
        };
        if lease.cancellation_requested {
            cancellation.cancel();
        }
        let generation = self
            .runtime
            .generate(lease.track, input, cancellation.clone());
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

struct TrackClaim {
    state: String,
    attempt_number: i64,
    cancellation_requested: bool,
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

fn append_claimed_attempt(
    run: &mut AgentRun,
    track_kind: AgentTrackKind,
    input_sha256: Sha256Digest,
    claim: &TrackClaim,
) -> Result<u32, AgentRunStoreError> {
    let track = run
        .tracks
        .iter_mut()
        .find(|track| track.kind == track_kind)
        .ok_or(AgentRunStoreError::InvalidContract)?;
    if claim.state == "running" {
        let previous = track
            .attempts
            .last_mut()
            .filter(|attempt| attempt.state == AgentAttemptState::Running)
            .ok_or(AgentRunStoreError::InvalidContract)?;
        previous.state = AgentAttemptState::Failed;
        previous.diagnostic_code = Some(diagnostic::AGENT_RUNTIME_FAILED.to_owned());
    }
    let attempt = u32::try_from(track.attempts.len().saturating_add(1))
        .map_err(|_| AgentRunStoreError::InvalidContract)?;
    if i64::from(attempt) != claim.attempt_number.saturating_add(1) {
        return Err(AgentRunStoreError::InvalidContract);
    }
    track.attempts.push(AgentAttempt {
        number: attempt,
        state: AgentAttemptState::Running,
        input_sha256,
        output_sha256: None,
        checkpoint: None,
        usage: zero_usage(),
        usage_observed: false,
        diagnostic_code: None,
    });
    let derived = run
        .derived_state()
        .map_err(|_| AgentRunStoreError::InvalidContract)?;
    run.state = if claim.cancellation_requested && derived == AgentRunState::Running {
        AgentRunState::Cancelling
    } else {
        derived
    };
    run.revision = next_revision(run.revision)?;
    run.validate()
        .map_err(|_| AgentRunStoreError::InvalidContract)?;
    Ok(attempt)
}

fn validate_reservation(command: &ReserveAgentRun<'_>) -> Result<(), AgentRunStoreError> {
    if command.trace_id.trim().is_empty()
        || command.course_id != command.input.course_id()
        || command.request.package_id != command.input.package_id()
        || command.request.package_revision != command.input.package_revision()
        || command.request.package_sha256 != command.input.package_manifest_sha256()
        || command.request.policy_id != command.policy.id
        || command.request.policy_revision != command.policy.revision
        || command.input.policy_id() != command.policy.id
        || command.input.policy_revision() != command.policy.revision
        || command.policy.course_id != command.course_id
    {
        return Err(AgentRunStoreError::IdentityMismatch);
    }
    command
        .policy
        .validate()
        .map_err(|_| AgentRunStoreError::IdentityMismatch)
}

fn requested_run(
    request: &CreateAgentRunRequest,
    course_id: CourseId,
) -> Result<AgentRun, AgentRunStoreError> {
    let run = AgentRun {
        id: AgentRunId::new(),
        course_id,
        package_id: request.package_id,
        policy_id: request.policy_id,
        policy_revision: request.policy_revision,
        requested_runtime: request.requested_runtime,
        state: AgentRunState::Requested,
        revision: Revision::new(1).map_err(|_| AgentRunStoreError::InvalidContract)?,
        tracks: vec![
            AgentTrack {
                kind: AgentTrackKind::Environment,
                attempts: Vec::new(),
                candidate_id: None,
            },
            AgentTrack {
                kind: AgentTrackKind::Evaluation,
                attempts: Vec::new(),
                candidate_id: None,
            },
        ],
    };
    run.validate()
        .map_err(|_| AgentRunStoreError::InvalidContract)?;
    Ok(run)
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
    let input_sha256 = row
        .try_get::<String, _>("input_sha256")
        .ok()
        .and_then(|value| Sha256Digest::from_str(&value).ok())
        .ok_or(AgentRunStoreError::InvalidContract)?;
    if course_id != run.course_id.as_uuid()
        || package_id != run.package_id.as_uuid()
        || u64::try_from(revision).ok() != Some(run.revision.get())
        || u64::try_from(policy_revision).ok() != Some(run.policy_revision.get())
        || state != run_state(run.state)
        || run
            .tracks
            .iter()
            .flat_map(|track| &track.attempts)
            .any(|attempt| attempt.input_sha256 != input_sha256)
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
    .bind(revision_i64(run.revision)?)
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

fn environment_checkpoint(
    run: &AgentRun,
    attempt: u32,
    result: Result<ClaudeCodeExecution, ClaudeCodeFailure>,
    now: UtcTimestamp,
) -> Result<AgentTrackCheckpoint, AgentRunStoreError> {
    match result {
        Ok(execution) => {
            let CandidateDocument::Environment(spec) = execution.document else {
                return Err(AgentRunStoreError::InvalidContract);
            };
            if spec.runtime.kind() != run.requested_runtime {
                let mut audit = execution.audit;
                audit.outcome = RuntimeAuditOutcome::Failed;
                audit.diagnostic_code = Some(diagnostic::LLM_SCHEMA_INVALID.to_owned());
                return Ok(AgentTrackCheckpoint {
                    run_id: run.id,
                    sequence: checkpoint_sequence(AgentTrackKind::Environment, attempt)?,
                    track: AgentTrackKind::Environment,
                    attempt,
                    audit,
                    candidate: None,
                });
            }
            let candidate = EnvironmentCandidate {
                id: CandidateId::new(),
                run_id: run.id,
                revision: Revision::new(1).map_err(|_| AgentRunStoreError::InvalidContract)?,
                spec,
                spec_sha256: execution
                    .audit
                    .output_sha256
                    .ok_or(AgentRunStoreError::InvalidContract)?,
                policy_revision: run.policy_revision,
                schema_sha256: execution.audit.schema_sha256,
                model: execution.audit.model.clone(),
                created_at: now,
            };
            candidate
                .validate()
                .map_err(|_| AgentRunStoreError::InvalidContract)?;
            Ok(AgentTrackCheckpoint {
                run_id: run.id,
                sequence: checkpoint_sequence(AgentTrackKind::Environment, attempt)?,
                track: AgentTrackKind::Environment,
                attempt,
                audit: execution.audit,
                candidate: Some(StoredCandidate::Environment(candidate)),
            })
        }
        Err(failure) => Ok(AgentTrackCheckpoint {
            run_id: run.id,
            sequence: checkpoint_sequence(AgentTrackKind::Environment, attempt)?,
            track: AgentTrackKind::Environment,
            attempt,
            audit: failure.audit().clone(),
            candidate: None,
        }),
    }
}

fn evaluation_checkpoint(
    run: &AgentRun,
    attempt: u32,
    result: Result<ClaudeCodeExecution, ClaudeCodeFailure>,
    now: UtcTimestamp,
) -> Result<AgentTrackCheckpoint, AgentRunStoreError> {
    match result {
        Ok(execution) => {
            let CandidateDocument::Evaluation(spec) = execution.document else {
                return Err(AgentRunStoreError::InvalidContract);
            };
            let candidate = EvaluationCandidate {
                id: CandidateId::new(),
                run_id: run.id,
                revision: Revision::new(1).map_err(|_| AgentRunStoreError::InvalidContract)?,
                spec,
                spec_sha256: execution
                    .audit
                    .output_sha256
                    .ok_or(AgentRunStoreError::InvalidContract)?,
                policy_revision: run.policy_revision,
                schema_sha256: execution.audit.schema_sha256,
                model: execution.audit.model.clone(),
                created_at: now,
            };
            candidate
                .validate()
                .map_err(|_| AgentRunStoreError::InvalidContract)?;
            Ok(AgentTrackCheckpoint {
                run_id: run.id,
                sequence: checkpoint_sequence(AgentTrackKind::Evaluation, attempt)?,
                track: AgentTrackKind::Evaluation,
                attempt,
                audit: execution.audit,
                candidate: Some(StoredCandidate::Evaluation(candidate)),
            })
        }
        Err(failure) => Ok(AgentTrackCheckpoint {
            run_id: run.id,
            sequence: checkpoint_sequence(AgentTrackKind::Evaluation, attempt)?,
            track: AgentTrackKind::Evaluation,
            attempt,
            audit: failure.audit().clone(),
            candidate: None,
        }),
    }
}

fn apply_checkpoint(
    run: &mut AgentRun,
    checkpoint: &AgentTrackCheckpoint,
) -> Result<(), AgentRunStoreError> {
    let track = run
        .tracks
        .iter_mut()
        .find(|track| track.kind == checkpoint.track)
        .ok_or(AgentRunStoreError::InvalidContract)?;
    let attempt = track
        .attempts
        .last_mut()
        .ok_or(AgentRunStoreError::InvalidContract)?;
    if attempt.number != checkpoint.attempt
        || attempt.input_sha256 != checkpoint.audit.input_sha256
        || checkpoint.audit.track != checkpoint.track
        || checkpoint.audit.course_id != run.course_id
        || checkpoint.audit.package_id != run.package_id
        || checkpoint.audit.policy_id != run.policy_id
        || checkpoint.audit.policy_revision != run.policy_revision
    {
        return Err(AgentRunStoreError::IdentityMismatch);
    }
    attempt.usage = checkpoint.audit.usage;
    attempt.usage_observed = checkpoint.audit.usage_observed;
    if let Some(candidate) = &checkpoint.candidate {
        attempt.state = AgentAttemptState::Succeeded;
        attempt.output_sha256 = checkpoint.audit.output_sha256;
        attempt.diagnostic_code = None;
        track.candidate_id = Some(match candidate {
            StoredCandidate::Environment(candidate) => candidate.id,
            StoredCandidate::Evaluation(candidate) => candidate.id,
        });
    } else {
        attempt.state = if checkpoint.audit.outcome == RuntimeAuditOutcome::Cancelled {
            AgentAttemptState::Cancelled
        } else {
            AgentAttemptState::Failed
        };
        attempt.output_sha256 = None;
        attempt
            .diagnostic_code
            .clone_from(&checkpoint.audit.diagnostic_code);
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

fn checkpoint_state(checkpoint: &AgentTrackCheckpoint) -> &'static str {
    if checkpoint.candidate.is_some() {
        "succeeded"
    } else if checkpoint.audit.outcome == RuntimeAuditOutcome::Cancelled {
        "cancelled"
    } else {
        "failed"
    }
}

fn terminal_event(run: &AgentRun) -> (&'static str, Option<&str>) {
    if matches!(
        run.state,
        AgentRunState::Succeeded | AgentRunState::PartiallySucceeded
    ) {
        (subjects::AGENT_RUN_COMPLETED, None)
    } else {
        (
            subjects::AGENT_RUN_FAILED,
            run.tracks
                .iter()
                .filter_map(|track| track.attempts.last())
                .find_map(|attempt| attempt.diagnostic_code.as_deref()),
        )
    }
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
    let contract = event_contract(subject)?;
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

fn event_contract(subject: &str) -> Result<EventContract, AgentRunStoreError> {
    EVENT_CONTRACTS
        .iter()
        .copied()
        .find(|contract| contract.subject == subject)
        .ok_or(AgentRunStoreError::InvalidContract)
}

fn next_revision(revision: Revision) -> Result<Revision, AgentRunStoreError> {
    Revision::new(
        revision
            .get()
            .checked_add(1)
            .ok_or(AgentRunStoreError::InvalidContract)?,
    )
    .map_err(|_| AgentRunStoreError::InvalidContract)
}

fn revision_i64(revision: Revision) -> Result<i64, AgentRunStoreError> {
    i64::try_from(revision.get()).map_err(|_| AgentRunStoreError::InvalidContract)
}

fn checkpoint_sequence(track: AgentTrackKind, attempt: u32) -> Result<u64, AgentRunStoreError> {
    let base = u64::from(
        attempt
            .checked_sub(1)
            .ok_or(AgentRunStoreError::InvalidContract)?,
    )
    .checked_mul(2)
    .ok_or(AgentRunStoreError::InvalidContract)?;
    base.checked_add(match track {
        AgentTrackKind::Environment => 1,
        AgentTrackKind::Evaluation => 2,
    })
    .ok_or(AgentRunStoreError::InvalidContract)
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

fn validate_worker(worker_id: &str, lease_duration: Duration) -> Result<(), AgentRunStoreError> {
    if worker_id.is_empty()
        || worker_id.len() > 256
        || !worker_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
        || lease_duration.is_zero()
        || lease_duration > Duration::from_secs(3_600)
    {
        return Err(AgentRunStoreError::WorkerIdentityInvalid);
    }
    Ok(())
}

fn lease_milliseconds(lease_duration: Duration) -> Result<i64, AgentRunStoreError> {
    i64::try_from(lease_duration.as_millis()).map_err(|_| AgentRunStoreError::WorkerIdentityInvalid)
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

const fn zero_usage() -> LlmUsage {
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
            Self::IdentityMismatch => diagnostic::LLM_POLICY_REVISION_MISMATCH,
            Self::IdempotencyConflict => diagnostic::IDEMPOTENCY_CONFLICT,
            Self::RunInProgress | Self::StateConflict | Self::RunNotFound => {
                diagnostic::AGENT_RUN_STATE_CONFLICT
            }
            Self::WorkerIdentityInvalid => diagnostic::AGENT_RUNTIME_IDENTITY_INVALID,
            Self::LeaseLost => diagnostic::AGENT_RUN_STATE_CONFLICT,
            Self::InvalidContract => diagnostic::CONTRACT_DOCUMENT_INVALID,
            Self::PersistenceFailed => diagnostic::AGENT_PERSISTENCE_FAILED,
        }
    }
}
