//! Durable idempotent `AgentRun` orchestration over the Agent-owned `PostgreSQL` schema.

use std::str::FromStr;

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

use crate::claude_code::{
    CandidateDocument, ClaudeCodeAudit, ClaudeCodeRuntime, DualCandidateOutcome,
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

/// Main-path result distinguishing a new billable run from an idempotent replay.
#[derive(Clone, Debug, PartialEq)]
pub enum AgentRunDispatch {
    /// The caller owned and completed a new dual-track execution.
    Executed(Box<StoredAgentRunOutcome>),
    /// An exact prior reservation was returned without invoking Claude Code.
    Replayed(AgentRun),
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

    /// Claims a newly requested run by creating one running attempt in each track.
    ///
    /// # Errors
    ///
    /// Returns a stable missing-run, state-conflict, contract or persistence failure.
    pub async fn claim(
        &self,
        run_id: AgentRunId,
        input_sha256: Sha256Digest,
    ) -> Result<AgentRun, AgentRunStoreError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        let mut run = load_run_for_update(&mut transaction, run_id).await?;
        if run.state != AgentRunState::Requested {
            return Err(AgentRunStoreError::StateConflict);
        }
        for track in &mut run.tracks {
            track.attempts.push(AgentAttempt {
                number: 1,
                state: AgentAttemptState::Running,
                input_sha256,
                output_sha256: None,
                checkpoint: None,
                usage: zero_usage(),
                diagnostic_code: None,
            });
        }
        run.state = AgentRunState::Running;
        run.revision = next_revision(run.revision)?;
        run.validate()
            .map_err(|_| AgentRunStoreError::InvalidContract)?;
        update_run(&mut transaction, &run).await?;
        transaction
            .commit()
            .await
            .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        Ok(run)
    }

    /// Atomically retains both checkpoints, the derived terminal run and one terminal event.
    ///
    /// # Errors
    ///
    /// Returns a stable state, identity, contract or persistence failure.
    pub async fn complete(
        &self,
        run_id: AgentRunId,
        outcome: DualCandidateOutcome,
        now: UtcTimestamp,
        trace_id: &str,
    ) -> Result<StoredAgentRunOutcome, AgentRunStoreError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        let mut run = load_run_for_update(&mut transaction, run_id).await?;
        if run.state != AgentRunState::Running
            || run.tracks.iter().any(|track| track.attempts.len() != 1)
        {
            return Err(AgentRunStoreError::StateConflict);
        }

        let environment = environment_checkpoint(&run, outcome.environment, now)?;
        let evaluation = evaluation_checkpoint(&run, outcome.evaluation, now)?;
        apply_checkpoint(&mut run, &environment)?;
        apply_checkpoint(&mut run, &evaluation)?;
        run.state = run
            .derived_state()
            .map_err(|_| AgentRunStoreError::InvalidContract)?;
        run.revision = next_revision(run.revision)?;
        run.validate()
            .map_err(|_| AgentRunStoreError::InvalidContract)?;

        insert_checkpoint(&mut transaction, &environment).await?;
        insert_checkpoint(&mut transaction, &evaluation).await?;
        update_run(&mut transaction, &run).await?;
        let (subject, diagnostic_code) = terminal_event(&run, &environment, &evaluation);
        enqueue_run_event(
            &mut transaction,
            &run,
            subject,
            2,
            1,
            diagnostic_code,
            now,
            trace_id,
        )
        .await?;
        transaction
            .commit()
            .await
            .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        Ok(StoredAgentRunOutcome {
            run,
            environment,
            evaluation,
        })
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
}

/// Coordinates reservation, exactly-one execution ownership and terminal persistence.
#[derive(Clone)]
pub struct AgentRunService {
    store: PostgresAgentRunStore,
    runtime: ClaudeCodeRuntime,
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
            .finish()
    }
}

impl AgentRunService {
    /// Creates a service from explicit durable store and Claude Code runtime bindings.
    #[must_use]
    pub const fn new(store: PostgresAgentRunStore, runtime: ClaudeCodeRuntime) -> Self {
        Self { store, runtime }
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
            AgentRunReservation::Created(run) => run,
            AgentRunReservation::Replayed(run) => {
                return Ok(AgentRunDispatch::Replayed(run));
            }
        };
        self.store.claim(run.id, command.input.sha256()).await?;
        let outcome = self
            .runtime
            .generate_both(command.input, command.cancellation)
            .await;
        let stored = self
            .store
            .complete(run.id, outcome, command.now, command.trace_id)
            .await?;
        Ok(AgentRunDispatch::Executed(Box::new(stored)))
    }
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
    result: Result<crate::claude_code::ClaudeCodeExecution, crate::claude_code::ClaudeCodeFailure>,
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
                    sequence: 1,
                    track: AgentTrackKind::Environment,
                    attempt: 1,
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
                sequence: 1,
                track: AgentTrackKind::Environment,
                attempt: 1,
                audit: execution.audit,
                candidate: Some(StoredCandidate::Environment(candidate)),
            })
        }
        Err(failure) => Ok(AgentTrackCheckpoint {
            run_id: run.id,
            sequence: 1,
            track: AgentTrackKind::Environment,
            attempt: 1,
            audit: failure.audit().clone(),
            candidate: None,
        }),
    }
}

fn evaluation_checkpoint(
    run: &AgentRun,
    result: Result<crate::claude_code::ClaudeCodeExecution, crate::claude_code::ClaudeCodeFailure>,
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
                sequence: 2,
                track: AgentTrackKind::Evaluation,
                attempt: 1,
                audit: execution.audit,
                candidate: Some(StoredCandidate::Evaluation(candidate)),
            })
        }
        Err(failure) => Ok(AgentTrackCheckpoint {
            run_id: run.id,
            sequence: 2,
            track: AgentTrackKind::Evaluation,
            attempt: 1,
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

fn terminal_event<'a>(
    run: &AgentRun,
    environment: &'a AgentTrackCheckpoint,
    evaluation: &'a AgentTrackCheckpoint,
) -> (&'static str, Option<&'a str>) {
    if matches!(
        run.state,
        AgentRunState::Succeeded | AgentRunState::PartiallySucceeded
    ) {
        (subjects::AGENT_RUN_COMPLETED, None)
    } else {
        (
            subjects::AGENT_RUN_FAILED,
            environment
                .audit
                .diagnostic_code
                .as_deref()
                .or(evaluation.audit.diagnostic_code.as_deref()),
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
            Self::InvalidContract => diagnostic::CONTRACT_DOCUMENT_INVALID,
            Self::PersistenceFailed => diagnostic::AGENT_PERSISTENCE_FAILED,
        }
    }
}
