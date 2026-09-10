//! Durable idempotent `AgentRun` orchestration over the Agent-owned `PostgreSQL` schema.

use persistence_sqlx::Sha256Digest; // internal persistence hash, not contract hash
use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;
use std::time::Duration;

use contracts::authoring::{
    AgentAttempt, AgentAttemptState, AgentRun, AgentRunPurpose, AgentRunState, AgentTrack,
    AgentTrackKind, EnvironmentCandidate, EnvironmentClass, EvaluationCandidate, LlmUsage,
    ProblemPackage, ProjectLlmEgressPolicy, WorkConfigurationPlan,
    WorkConfigurationPreauthorization,
};
use contracts::diagnostic;
use contracts::events::{
    AgentRunEvent, CloudEvent, EVENT_CONTRACTS, EventContract, SPEC_VERSION, subjects,
};
use contracts::http::{
    AgentWorkExecutionIntentMetadata, AgentWorkExecutionIntentQuery, ContainerWorkExecutionRequest,
    CreateAgentRunRequest, IdempotencyKey, InternalAgentRunRequest,
    InternalApproveWorkConfigurationRequest, InternalCreateAgentRunRequest,
};
use contracts::{
    AgentRunId, ArtifactId, CandidateId, CourseId, EventId, ProjectId, Revision, Sequence,
    UtcTimestamp, WorkConfigurationPlanId,
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
const CANCEL_OPERATION: &str = "cancel_agent_run_v1";
const RETRY_OPERATION: &str = "retry_agent_run_track_v1";
const APPROVE_WORK_CONFIGURATION_OPERATION: &str = "approve_work_configuration_v1";

/// Input required to reserve one idempotent `AgentRun`.
pub struct ReserveAgentRun<'a> {
    /// Authoritative project from the authenticated route scope.
    pub project_id: ProjectId,
    /// Optional teaching course associated with the project.
    pub course_id: Option<CourseId>,
    /// Public immutable create request.
    pub request: &'a CreateAgentRunRequest,
    /// Validated HTTP idempotency key.
    pub idempotency_key: &'a IdempotencyKey,
    /// Egress input already verified against the immutable package.
    pub input: &'a ImmutableEgressInput,
    /// Immutable course policy bound to the runtime.
    pub policy: &'a ProjectLlmEgressPolicy,
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
    pub request: InternalAgentRunRequest,
    /// Control-authoritative immutable purpose for the run.
    pub purpose: AgentRunPurpose,
    /// Exact Work configuration grant, when the request reuses an approved plan.
    pub preauthorization: Option<WorkConfigurationPreauthorization>,
    /// Control-verified package contract.
    pub package: ProblemPackage,
    /// Opaque object keys indexed by package artifact identity.
    pub object_locators: BTreeMap<ArtifactId, String>,
    /// Control-verified active course policy.
    pub policy: ProjectLlmEgressPolicy,
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
    /// Immutable run snapshot used to bind a Work configuration plan.
    pub run: AgentRun,
    /// Independently scheduled track.
    pub track: AgentTrackKind,
    /// Monotonic attempt owned by this lease.
    pub attempt: u32,
    /// Sanitized worker identity.
    pub worker_id: String,
    lease_token: Uuid,
    cancellation_requested: bool,
}

/// Durable ownership of one approved Work execution side effect.
///
/// The request is kept as JSON because the VM request is an Agent-private contract while the
/// container request is shared with Environment.  Both variants are validated by the execution
/// transport before they are persisted and again when a receipt is committed.
#[derive(Clone, Debug)]
pub struct WorkExecutionLease {
    /// Parent run snapshot at claim time.
    pub run: AgentRun,
    /// Parent run identity.
    pub run_id: AgentRunId,
    /// Approved Work attempt number.
    pub attempt: u32,
    /// Fenced worker identity.
    pub worker_id: String,
    /// Persisted private execution intent.
    pub request: Value,
    /// Whether this claim created the durable intent before the first side effect.
    pub fresh: bool,
    pub(crate) lease_token: Uuid,
}

/// Minimal shape used to project a persisted private VM request into the Control-facing
/// recovery metadata contract.  Keep this private so the request itself cannot become an API
/// surface accidentally.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedVmExecutionIntent {
    kind: String,
    execution_id: Uuid,
    run_id: AgentRunId,
    run_revision: Revision,
    plan_id: WorkConfigurationPlanId,
    plan_revision: Revision,
    environment_id: contracts::EnvironmentId,
    environment_revision: Revision,
    actor_id: contracts::ActorId,
    script_content: String,
    verification_script_content: Option<String>,
    target: PersistedVmExecutionTarget,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedVmExecutionTarget {
    source_identity: String,
}

/// Returns the JSON representation of the shared container execution request.
pub fn container_execution_request_value(
    request: &ContainerWorkExecutionRequest,
) -> Result<Value, AgentRunStoreError> {
    serde_json::to_value(request).map_err(|_| AgentRunStoreError::InvalidContract)
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

    /// Atomically reserves the full Control-to-Agent command, including its immutable purpose.
    #[allow(clippy::too_many_lines)]
    pub async fn reserve_internal_dispatch(
        &self,
        command: &InternalCreateAgentRunRequest,
        idempotency_key: &IdempotencyKey,
        now: UtcTimestamp,
        trace_id: &str,
    ) -> Result<AgentRunReservation, AgentRunStoreError> {
        command
            .validate()
            .map_err(|_| AgentRunStoreError::InvalidContract)?;
        if trace_id.trim().is_empty() {
            return Err(AgentRunStoreError::InvalidContract);
        }
        command
            .package
            .validate()
            .map_err(|_| AgentRunStoreError::InvalidContract)?;
        command
            .policy
            .validate()
            .map_err(|_| AgentRunStoreError::InvalidContract)?;
        let expected_artifacts = command
            .package
            .files
            .iter()
            .map(|file| file.object.artifact_id)
            .collect::<BTreeSet<_>>();
        if command
            .object_locators
            .keys()
            .copied()
            .collect::<BTreeSet<_>>()
            != expected_artifacts
            || command.object_locators.values().any(|key| {
                key.trim().is_empty()
                    || key.contains("..")
                    || key.bytes().any(|byte| byte.is_ascii_control())
            })
        {
            return Err(AgentRunStoreError::IdentityMismatch);
        }
        let request_hash =
            Sha256Digest::of_canonical(command).map_err(|_| AgentRunStoreError::InvalidContract)?;
        let dispatch_sha256 = Sha256Digest::of_canonical(&serde_json::json!({
            "request": command.request,
            "purpose": command.purpose,
            "preauthorization": command.preauthorization,
            "package": command.package,
            "objectLocators": command.object_locators,
            "policy": command.policy,
        }))
        .map_err(|_| AgentRunStoreError::InvalidContract)?;
        let mut transaction = self.pool.begin().await.map_err(|error| {
            tracing::error!(
                event = "agent.persistence_failed",
                operation = "reserve_internal_dispatch.begin",
                error = %error,
            );
            AgentRunStoreError::PersistenceFailed
        })?;
        match IdempotencyStore::reserve(
            &mut transaction,
            Domain::Agent,
            CREATE_OPERATION,
            idempotency_key.as_str(),
            request_hash,
        )
        .await
        .map_err(|error| {
            tracing::error!(
                event = "agent.persistence_failed",
                operation = "reserve_internal_dispatch.idempotency_reserve",
                error = %error,
            );
            AgentRunStoreError::PersistenceFailed
        })? {
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
        let run = requested_internal_run(&command.request, command.purpose)?;
        let contract =
            serde_json::to_value(&run).map_err(|_| AgentRunStoreError::InvalidContract)?;
        let purpose = serde_json::to_value(command.purpose)
            .map_err(|_| AgentRunStoreError::InvalidContract)?;
        let preauthorization = command
            .preauthorization
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(|_| AgentRunStoreError::InvalidContract)?;
        sqlx::query(
            "INSERT INTO agent.agent_runs (run_id,project_id,course_id,problem_package_id,revision,state,provider_binding,input_sha256,policy_revision,purpose,plan,contract) \
             VALUES ($1,$2,$3,$4,$5,'requested',$6,$7,$8,$9,$10,$11)",
        )
        .bind(run.id.as_uuid())
        .bind(run.project_id.as_uuid())
        .bind(run.course_id.map(CourseId::as_uuid))
        .bind(run.package_id.as_uuid())
        .bind(revision_i64(run.revision)?)
        .bind(&command.policy.binding.runtime_binding)
        .bind(dispatch_sha256.to_string())
        .bind(revision_i64(command.policy.revision)?)
        .bind(&purpose)
        .bind(Option::<Value>::None)
        .bind(&contract)
        .execute(&mut *transaction)
        .await
        .map_err(|error| {
            tracing::error!(
                event = "agent.persistence_failed",
                operation = "reserve_internal_dispatch.insert_run",
                error = %error,
            );
            AgentRunStoreError::PersistenceFailed
        })?;
        for track in tracks_for_purpose(command.purpose) {
            sqlx::query(
                "INSERT INTO agent.agent_track_work_items \
                 (run_id,track,state,input_sha256) VALUES ($1,$2,'requested',$3)",
            )
            .bind(run.id.as_uuid())
            .bind(track_name(track))
            .bind(dispatch_sha256.to_string())
            .execute(&mut *transaction)
            .await
            .map_err(|error| {
                tracing::error!(
                    event = "agent.persistence_failed",
                    operation = "reserve_internal_dispatch.insert_track",
                    track = track_name(track),
                    error = %error,
                );
                AgentRunStoreError::PersistenceFailed
            })?;
        }
        sqlx::query(
            "INSERT INTO agent.agent_run_dispatches \
             (run_id,dispatch_sha256,idempotency_key,request,purpose,preauthorization,package,object_locators,policy,trace_id,state) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,'pending')",
        )
        .bind(run.id.as_uuid())
        .bind(dispatch_sha256.to_string())
        .bind(idempotency_key.as_str())
        .bind(serde_json::to_value(&command.request).map_err(|_| AgentRunStoreError::InvalidContract)?)
        .bind(purpose)
        .bind(preauthorization)
        .bind(serde_json::to_value(&command.package).map_err(|_| AgentRunStoreError::InvalidContract)?)
        .bind(serde_json::to_value(&command.object_locators).map_err(|_| AgentRunStoreError::InvalidContract)?)
        .bind(serde_json::to_value(&command.policy).map_err(|_| AgentRunStoreError::InvalidContract)?)
        .bind(trace_id)
        .execute(&mut *transaction)
        .await
        .map_err(|error| {
            tracing::error!(
                event = "agent.persistence_failed",
                operation = "reserve_internal_dispatch.insert_dispatch",
                error = %error,
            );
            AgentRunStoreError::PersistenceFailed
        })?;
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
        .map_err(|error| {
            tracing::error!(
                event = "agent.persistence_failed",
                operation = "reserve_internal_dispatch.idempotency_complete",
                error = %error,
            );
            AgentRunStoreError::PersistenceFailed
        })?;
        transaction.commit().await.map_err(|error| {
            tracing::error!(
                event = "agent.persistence_failed",
                operation = "reserve_internal_dispatch.commit",
                error = %error,
            );
            AgentRunStoreError::PersistenceFailed
        })?;
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
            "SELECT run_id,dispatch_sha256,idempotency_key,request,purpose,preauthorization,package,object_locators,policy,trace_id \
             FROM agent.agent_run_dispatches \
             WHERE (state IN ('pending','prepared') OR (state='preparing' AND lease_expires_at <= now())) \
               AND EXISTS (SELECT 1 FROM agent.agent_track_work_items work \
                           JOIN agent.agent_runs run ON run.run_id=work.run_id \
                           WHERE work.run_id=agent_run_dispatches.run_id \
                             AND (work.track <> 'work_configuration' \
                                  OR (run.plan IS NULL AND work.execution_request IS NULL)) \
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
            purpose: serde_json::from_value(
                row.try_get("purpose")
                    .map_err(|_| AgentRunStoreError::InvalidContract)?,
            )
            .map_err(|_| AgentRunStoreError::InvalidContract)?,
            preauthorization: row
                .try_get::<Option<Value>, _>("preauthorization")
                .map_err(|_| AgentRunStoreError::InvalidContract)?
                .map(serde_json::from_value)
                .transpose()
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
        if work.rows_affected()
            != u64::try_from(lease.run.tracks.len())
                .map_err(|_| AgentRunStoreError::InvalidContract)?
        {
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
        run.revision = next_revision(run.revision)?;
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
        if work.rows_affected()
            != u64::try_from(run.tracks.len()).map_err(|_| AgentRunStoreError::InvalidContract)?
        {
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
            "projectId": command.project_id,
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

        let run = requested_run(command.request)?;
        let contract =
            serde_json::to_value(&run).map_err(|_| AgentRunStoreError::InvalidContract)?;
        sqlx::query(
            "INSERT INTO agent.agent_runs \
             (run_id, project_id, course_id, problem_package_id, revision, state, provider_binding, \
              input_sha256, policy_revision, purpose, plan, contract) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        )
        .bind(run.id.as_uuid())
        .bind(run.project_id.as_uuid())
        .bind(run.course_id.map(CourseId::as_uuid))
        .bind(run.package_id.as_uuid())
        .bind(revision_i64(run.revision)?)
        .bind("requested")
        .bind(&command.policy.binding.runtime_binding)
        .bind(command.input.sha256().to_string())
        .bind(revision_i64(command.policy.revision)?)
        .bind(serde_json::to_value(run.purpose).map_err(|_| AgentRunStoreError::InvalidContract)?)
        .bind(Option::<Value>::None)
        .bind(&contract)
        .execute(&mut *transaction)
        .await
        .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        for track in tracks_for_purpose(run.purpose) {
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
                    run.plan IS NOT NULL AS has_plan, \
                    work.execution_request IS NOT NULL AS has_execution_request, \
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
        if track_kind == AgentTrackKind::WorkConfiguration
            && (row
                .try_get::<bool, _>("has_plan")
                .map_err(|_| AgentRunStoreError::InvalidContract)?
                || row
                    .try_get::<bool, _>("has_execution_request")
                    .map_err(|_| AgentRunStoreError::InvalidContract)?)
        {
            transaction
                .rollback()
                .await
                .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
            return Ok(None);
        }
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
            run: run.clone(),
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

    /// Idempotently requests cancellation at one exact run revision.
    ///
    /// # Errors
    ///
    /// Returns an error for stale revision, conflicting idempotency, or persistence failure.
    pub async fn request_cancellation_revisioned(
        &self,
        project_id: ProjectId,
        course_id: Option<CourseId>,
        run_id: AgentRunId,
        expected_revision: Revision,
        idempotency_key: &IdempotencyKey,
        now: UtcTimestamp,
    ) -> Result<AgentRun, AgentRunStoreError> {
        let request_hash = Sha256Digest::of_canonical(&serde_json::json!({
            "projectId": project_id,
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
        if run.project_id != project_id || run.course_id != course_id {
            return Err(AgentRunStoreError::IdentityMismatch);
        }
        if run.revision != expected_revision {
            return Err(AgentRunStoreError::StateConflict);
        }
        if !is_terminal_run(run.state) {
            if run.state == AgentRunState::Running {
                run.state = AgentRunState::Cancelling;
                run.revision = next_revision(run.revision)?;
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
    #[allow(
        clippy::too_many_lines,
        reason = "revisioned retry keeps the idempotency and state transition atomic"
    )]
    pub async fn retry_track_revisioned(
        &self,
        project_id: ProjectId,
        course_id: Option<CourseId>,
        run_id: AgentRunId,
        track: AgentTrackKind,
        expected_revision: Revision,
        idempotency_key: &IdempotencyKey,
    ) -> Result<AgentRun, AgentRunStoreError> {
        let request_hash = Sha256Digest::of_canonical(&serde_json::json!({
            "projectId": project_id,
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
        if run.project_id != project_id || run.course_id != course_id {
            return Err(AgentRunStoreError::IdentityMismatch);
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
        if track == AgentTrackKind::WorkConfiguration {
            let has_execution_request = sqlx::query_scalar::<_, bool>(
                "SELECT execution_request IS NOT NULL
                 FROM agent.agent_track_work_items
                 WHERE run_id=$1 AND track='work_configuration'
                 FOR UPDATE",
            )
            .bind(run_id.as_uuid())
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|_| AgentRunStoreError::PersistenceFailed)?
            .ok_or(AgentRunStoreError::StateConflict)?;
            // A generated plan is an immutable proposal.  Retrying that same track would
            // silently re-enter LLM generation while leaving the old proposal attached to the
            // run.  A new Work configuration request must create a new run instead.
            if run.plan.is_some() || has_execution_request {
                return Err(AgentRunStoreError::StateConflict);
            }
        }
        let updated = sqlx::query("UPDATE agent.agent_track_work_items SET state='requested',next_retry_at=now(),worker_id=NULL,lease_token=NULL,lease_expires_at=NULL,heartbeat_at=NULL,updated_at=now() WHERE run_id=$1 AND track=$2 AND state IN ('failed','cancelled')")
            .bind(run_id.as_uuid()).bind(track_name(track)).execute(&mut *transaction).await.map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        if updated.rows_affected() != 1 {
            return Err(AgentRunStoreError::StateConflict);
        }
        run.revision = next_revision(run.revision)?;
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
        self.complete_track_with_plan(lease, outcome, now, trace_id, None)
            .await
    }

    /// Commits a Work configuration result and its immutable generated plan.
    ///
    /// The plan is written in the same transaction as the successful track checkpoint, so a
    /// downstream admission reader cannot observe a successful script proposal without its exact
    /// artifact bindings.
    pub async fn complete_work_track(
        &self,
        lease: &AgentTrackLease,
        outcome: Result<ClaudeCodeExecution, ClaudeCodeFailure>,
        package: &ProblemPackage,
        preauthorization: Option<&WorkConfigurationPreauthorization>,
        now: UtcTimestamp,
        trace_id: &str,
    ) -> Result<StoredAgentTrackOutcome, AgentRunStoreError> {
        if lease.track != AgentTrackKind::WorkConfiguration {
            return Err(AgentRunStoreError::InvalidContract);
        }
        let plan = match &outcome {
            Ok(execution) => Some(bind_work_configuration_plan(
                &lease.run,
                package,
                preauthorization,
                execution,
                now,
            )?),
            Err(_) => None,
        };
        self.complete_track_with_plan(lease, outcome, now, trace_id, plan)
            .await
    }

    #[allow(
        clippy::too_many_lines,
        reason = "track completion keeps receipt, plan, and aggregate updates atomic"
    )]
    async fn complete_track_with_plan(
        &self,
        lease: &AgentTrackLease,
        outcome: Result<ClaudeCodeExecution, ClaudeCodeFailure>,
        now: UtcTimestamp,
        trace_id: &str,
        plan: Option<WorkConfigurationPlan>,
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
            AgentTrackKind::WorkConfiguration => {
                work_configuration_checkpoint(&run, lease.attempt, outcome, now)?
            }
        };
        apply_checkpoint(&mut run, &checkpoint)?;
        if let Some(plan) = plan {
            if lease.track != AgentTrackKind::WorkConfiguration {
                return Err(AgentRunStoreError::InvalidContract);
            }
            run.plan = Some(plan);
        }
        let derived = run
            .derived_state()
            .map_err(|_| AgentRunStoreError::InvalidContract)?;
        run.state = if cancellation_requested && derived == AgentRunState::Running {
            AgentRunState::Cancelling
        } else if lease.track == AgentTrackKind::WorkConfiguration
            && checkpoint.candidate.is_none()
            && checkpoint.audit.outcome == RuntimeAuditOutcome::Succeeded
        {
            // Generating and materializing a plan never executes it. A Control grant is
            // consumed by the separate execution path after explicit approval.
            AgentRunState::AwaitingApproval
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
            "SELECT project_id, course_id, problem_package_id, revision, state, input_sha256, \
                    policy_revision, plan, contract \
             FROM agent.agent_runs WHERE run_id = $1",
        )
        .bind(run_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| AgentRunStoreError::PersistenceFailed)?
        .ok_or(AgentRunStoreError::RunNotFound)?;
        decode_run_row(&row)
    }

    /// Applies one exact Control-issued Work preauthorization and queues the existing generated
    /// plan for execution. Approval never marks the run successful; the Work runtime must report
    /// execution and verification separately before a terminal success can be recorded.
    #[allow(
        clippy::too_many_lines,
        reason = "approval validates and persists the plan binding in one transaction"
    )]
    pub async fn approve_work_configuration(
        &self,
        run_id: AgentRunId,
        request: &InternalApproveWorkConfigurationRequest,
        idempotency_key: &IdempotencyKey,
        now: UtcTimestamp,
    ) -> Result<AgentRun, AgentRunStoreError> {
        if request.project_id != request.preauthorization.project_id
            || request.preauthorization.expires_at <= now
        {
            return Err(AgentRunStoreError::IdentityMismatch);
        }
        let request_hash =
            Sha256Digest::of_canonical(request).map_err(|_| AgentRunStoreError::InvalidContract)?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        match IdempotencyStore::reserve(
            &mut transaction,
            Domain::Agent,
            APPROVE_WORK_CONFIGURATION_OPERATION,
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
                return serde_json::from_value(value)
                    .map_err(|_| AgentRunStoreError::InvalidContract);
            }
            IdempotencyDecision::Conflict => return Err(AgentRunStoreError::IdempotencyConflict),
            IdempotencyDecision::InProgress => return Err(AgentRunStoreError::RunInProgress),
            IdempotencyDecision::Reserved => {}
        }
        let mut run = load_run_for_update(&mut transaction, run_id).await?;
        if run.project_id != request.project_id
            || run.course_id != request.course_id
            || run.revision != request.expected_run_revision
            || run.state != AgentRunState::AwaitingApproval
        {
            return Err(AgentRunStoreError::StateConflict);
        }
        let AgentRunPurpose::WorkConfiguration {
            environment_id,
            environment_revision,
            actor_id,
            ..
        } = run.purpose
        else {
            return Err(AgentRunStoreError::IdentityMismatch);
        };
        let plan = run.plan.as_ref().ok_or(AgentRunStoreError::StateConflict)?;
        if request.preauthorization.environment_id != environment_id
            || request.preauthorization.environment_revision != environment_revision
            || request.preauthorization.actor_id != actor_id
            || request.preauthorization.project_id != run.project_id
            || request
                .preauthorization
                .validate_against_plan(plan)
                .is_err()
        {
            return Err(AgentRunStoreError::IdentityMismatch);
        }
        // The plan remains immutable.  Approval moves the existing proposal attempt into the
        // execution phase; it must never append another LLM attempt.
        let track = run
            .tracks
            .iter_mut()
            .find(|track| track.kind == AgentTrackKind::WorkConfiguration)
            .ok_or(AgentRunStoreError::InvalidContract)?;
        let attempt = track
            .attempts
            .last_mut()
            .filter(|attempt| attempt.state == AgentAttemptState::AwaitingApproval)
            .ok_or(AgentRunStoreError::StateConflict)?;
        let attempt_number = attempt.number;
        attempt.state = AgentAttemptState::Running;
        attempt.diagnostic_code = None;
        run.state = AgentRunState::Running;
        run.revision = next_revision(run.revision)?;
        run.validate()
            .map_err(|_| AgentRunStoreError::InvalidContract)?;
        update_run(&mut transaction, &run).await?;
        let queued = sqlx::query(
            "UPDATE agent.agent_track_work_items
             SET state='requested', attempt_number=$2, worker_id=NULL, lease_token=NULL,
                  lease_expires_at=NULL, heartbeat_at=NULL, execution_request=NULL,
                  execution_receipt=NULL, updated_at=now()
              WHERE run_id=$1 AND track='work_configuration' AND state='awaiting_approval'
                AND attempt_number=$2 AND execution_request IS NULL AND execution_receipt IS NULL",
        )
        .bind(run.id.as_uuid())
        .bind(i64::from(attempt_number))
        .execute(&mut *transaction)
        .await
        .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        if queued.rows_affected() != 1 {
            return Err(AgentRunStoreError::StateConflict);
        }
        let value = serde_json::to_value(&run).map_err(|_| AgentRunStoreError::InvalidContract)?;
        IdempotencyStore::complete(
            &mut transaction,
            Domain::Agent,
            APPROVE_WORK_CONFIGURATION_OPERATION,
            idempotency_key.as_str(),
            &value,
        )
        .await
        .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        transaction
            .commit()
            .await
            .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        Ok(run)
    }

    /// Claims the approved Work execution side effect or recovers an expired owner.
    ///
    /// A fresh intent is written in the same transaction as the first execution lease.  Recovery
    /// accepts no replacement intent: it returns the previously persisted JSON and therefore keeps
    /// a restart bound to the same run, plan and revision.  An expired owner is never allowed to
    /// issue a second intent.
    #[allow(
        clippy::too_many_lines,
        reason = "execution claim and recovery fencing share one transaction"
    )]
    pub async fn claim_work_execution(
        &self,
        run_id: AgentRunId,
        worker_id: &str,
        lease_duration: Duration,
        fresh_request: Option<Value>,
    ) -> Result<Option<WorkExecutionLease>, AgentRunStoreError> {
        validate_worker(worker_id, lease_duration)?;
        let lease_milliseconds = lease_milliseconds(lease_duration)?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        let run = load_run_for_update(&mut transaction, run_id).await?;
        if !matches!(run.purpose, AgentRunPurpose::WorkConfiguration { .. })
            || run.plan.is_none()
            || !matches!(
                run.state,
                AgentRunState::Running | AgentRunState::Cancelling
            )
        {
            transaction
                .rollback()
                .await
                .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
            return Ok(None);
        }
        let row = sqlx::query(
            "SELECT state,attempt_number,execution_request,execution_receipt,
                    lease_expires_at > now() AS lease_current
             FROM agent.agent_track_work_items
             WHERE run_id=$1 AND track='work_configuration' FOR UPDATE",
        )
        .bind(run_id.as_uuid())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| AgentRunStoreError::PersistenceFailed)?
        .ok_or(AgentRunStoreError::StateConflict)?;
        let state = row
            .try_get::<String, _>("state")
            .map_err(|_| AgentRunStoreError::InvalidContract)?;
        let attempt_number = row
            .try_get::<i64, _>("attempt_number")
            .map_err(|_| AgentRunStoreError::InvalidContract)?;
        let expected_attempt = work_execution_attempt(&run)?;
        if attempt_number != i64::from(expected_attempt) {
            transaction
                .rollback()
                .await
                .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
            return Ok(None);
        }
        let lease_current = row
            .try_get::<Option<bool>, _>("lease_current")
            .map_err(|_| AgentRunStoreError::InvalidContract)?
            .unwrap_or(false);
        let persisted_request = row
            .try_get::<Option<Value>, _>("execution_request")
            .map_err(|_| AgentRunStoreError::InvalidContract)?;
        let persisted_receipt = row
            .try_get::<Option<Value>, _>("execution_receipt")
            .map_err(|_| AgentRunStoreError::InvalidContract)?;
        if persisted_receipt.is_some() {
            transaction
                .rollback()
                .await
                .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
            return Ok(None);
        }
        let (request, fresh) = match (
            state.as_str(),
            lease_current,
            persisted_request,
            fresh_request,
        ) {
            ("requested", false, None, Some(request)) if run.state == AgentRunState::Running => {
                if !request.is_object() {
                    return Err(AgentRunStoreError::InvalidContract);
                }
                (request, true)
            }
            // A cancellation racing with approval must not start a fresh side effect.  Recovery
            // with an already persisted request remains allowed in the running branch below.
            ("requested", false, None, Some(_)) => {
                transaction
                    .rollback()
                    .await
                    .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
                return Ok(None);
            }
            ("running", false, Some(request), None) => (request, false),
            ("running" | "requested", true, _, _) => {
                transaction
                    .rollback()
                    .await
                    .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
                return Ok(None);
            }
            _ => return Err(AgentRunStoreError::StateConflict),
        };
        let lease_token = Uuid::now_v7();
        let updated = sqlx::query(
            "UPDATE agent.agent_track_work_items
             SET state='running', worker_id=$2, lease_token=$3,
                 heartbeat_at=date_trunc('milliseconds', clock_timestamp()),
                 lease_expires_at=date_trunc('milliseconds', clock_timestamp())
                     + ($4 * interval '1 millisecond'),
                 execution_request=COALESCE(execution_request,$5), updated_at=now()
             WHERE run_id=$1 AND track='work_configuration' AND attempt_number=$6
               AND state IN ('requested','running')
               AND (lease_expires_at IS NULL OR lease_expires_at <= now())",
        )
        .bind(run_id.as_uuid())
        .bind(worker_id)
        .bind(lease_token)
        .bind(lease_milliseconds)
        .bind(&request)
        .bind(attempt_number)
        .execute(&mut *transaction)
        .await
        .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        if updated.rows_affected() != 1 {
            return Err(AgentRunStoreError::LeaseLost);
        }
        transaction
            .commit()
            .await
            .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        Ok(Some(WorkExecutionLease {
            run,
            run_id,
            attempt: expected_attempt,
            worker_id: worker_id.to_owned(),
            request,
            fresh,
            lease_token,
        }))
    }

    /// Returns approved Work execution rows that are ready for a fresh claim or recovery.
    ///
    /// A requested row has no persisted intent and must be supplied a newly built request by the
    /// caller. A running row is returned only after its owner lease expires; its existing intent
    /// is the sole input accepted by [`Self::claim_work_execution`].
    pub async fn work_execution_candidates(
        &self,
        limit: u16,
    ) -> Result<Vec<(AgentRunId, bool)>, AgentRunStoreError> {
        if limit == 0 || limit > 128 {
            return Err(AgentRunStoreError::InvalidContract);
        }
        let rows = sqlx::query(
            "SELECT work.run_id,work.state FROM agent.agent_track_work_items work
             JOIN agent.agent_runs run ON run.run_id=work.run_id
             WHERE work.track='work_configuration' AND run.plan IS NOT NULL
               AND ((work.state='requested' AND run.state='running'
                     AND work.execution_request IS NULL)
                    OR (work.state='running' AND run.state IN ('running','cancelling')
                        AND work.execution_request IS NOT NULL
                        AND work.lease_expires_at <= now()))
             ORDER BY work.updated_at,work.run_id LIMIT $1",
        )
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        rows.into_iter()
            .map(|row| {
                let run_id = row
                    .try_get::<Uuid, _>("run_id")
                    .map_err(|_| AgentRunStoreError::InvalidContract)?;
                let state = row
                    .try_get::<String, _>("state")
                    .map_err(|_| AgentRunStoreError::InvalidContract)?;
                let id = AgentRunId::from_str(&run_id.to_string())
                    .map_err(|_| AgentRunStoreError::InvalidContract)?;
                match state.as_str() {
                    "requested" => Ok((id, true)),
                    "running" => Ok((id, false)),
                    _ => Err(AgentRunStoreError::InvalidContract),
                }
            })
            .collect()
    }

    /// Reads the metadata of one persisted VM execution intent for Control.
    ///
    /// The complete private request remains Agent-owned.  This endpoint exposes only the exact
    /// identity and byte digests that Control needs to fence Environment recovery; scripts,
    /// credentials, and provider details never cross this boundary.
    pub async fn work_execution_intent_metadata(
        &self,
        run_id: AgentRunId,
        query: &AgentWorkExecutionIntentQuery,
    ) -> Result<AgentWorkExecutionIntentMetadata, AgentRunStoreError> {
        let row = sqlx::query(
            "SELECT run.project_id,run.course_id,work.execution_request
             FROM agent.agent_track_work_items work
             JOIN agent.agent_runs run ON run.run_id=work.run_id
             WHERE work.run_id=$1 AND work.track='work_configuration'",
        )
        .bind(run_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| AgentRunStoreError::PersistenceFailed)?
        .ok_or(AgentRunStoreError::RunNotFound)?;

        let project_id = row
            .try_get::<Uuid, _>("project_id")
            .map_err(|_| AgentRunStoreError::InvalidContract)?;
        let project_id = ProjectId::from_str(&project_id.to_string())
            .map_err(|_| AgentRunStoreError::InvalidContract)?;
        let course_id = row
            .try_get::<Option<Uuid>, _>("course_id")
            .map_err(|_| AgentRunStoreError::InvalidContract)?
            .map(|value| CourseId::from_str(&value.to_string()))
            .transpose()
            .map_err(|_| AgentRunStoreError::InvalidContract)?;
        if project_id != query.project_id || course_id != query.course_id {
            return Err(AgentRunStoreError::IdentityMismatch);
        }
        let request = row
            .try_get::<Option<Value>, _>("execution_request")
            .map_err(|_| AgentRunStoreError::InvalidContract)?
            .ok_or(AgentRunStoreError::StateConflict)?;
        let request: PersistedVmExecutionIntent =
            serde_json::from_value(request).map_err(|_| AgentRunStoreError::InvalidContract)?;
        if request.kind != "virtual_machine"
            || request.run_id != run_id
            || request.execution_id != query.execution_id
        {
            return Err(AgentRunStoreError::IdentityMismatch);
        }
        let metadata = AgentWorkExecutionIntentMetadata {
            execution_id: request.execution_id,
            run_id: request.run_id,
            run_revision: request.run_revision,
            project_id,
            course_id,
            environment_id: request.environment_id,
            environment_revision: request.environment_revision,
            actor_id: request.actor_id,
            plan_id: request.plan_id,
            plan_revision: request.plan_revision,
            source_identity: request.target.source_identity,
            script_sha256: Sha256Digest::of_bytes(request.script_content.as_bytes()).to_string(),
            verification_script_sha256: request
                .verification_script_content
                .as_deref()
                .map(|value| Sha256Digest::of_bytes(value.as_bytes()).to_string()),
        };
        metadata
            .validate()
            .map_err(|_| AgentRunStoreError::InvalidContract)?;
        Ok(metadata)
    }

    /// Renews one exact Work execution lease and reports durable run cancellation.
    ///
    /// The execution worker may spend longer than one lease polling a remote runtime. The
    /// same worker/token fence therefore covers every poll and completion, just like an Agent
    /// generation track lease.
    pub async fn heartbeat_work_execution(
        &self,
        lease: &WorkExecutionLease,
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
             WHERE work.run_id=$1 AND work.track='work_configuration' \
               AND work.worker_id=$2 AND work.lease_token=$3 \
               AND work.attempt_number=$4 AND work.lease_expires_at > now() \
               AND work.state='running' AND run.run_id=work.run_id \
             RETURNING run.cancellation_requested_at IS NOT NULL",
        )
        .bind(lease.run_id.as_uuid())
        .bind(&lease.worker_id)
        .bind(lease.lease_token)
        .bind(i64::from(lease.attempt))
        .bind(lease_milliseconds)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        cancellation.ok_or(AgentRunStoreError::LeaseLost)
    }

    /// Persists one terminal Work receipt and transitions the already approved attempt.
    ///
    /// The lease fence covers both the receipt and the public run aggregate.  Callers must validate
    /// the receipt against the saved request before invoking this method; the database row is still
    /// checked for ownership and a duplicate receipt is rejected.
    #[allow(
        clippy::too_many_lines,
        reason = "receipt completion updates the fenced lease and aggregate atomically"
    )]
    pub async fn complete_work_execution(
        &self,
        lease: &WorkExecutionLease,
        receipt: Value,
        succeeded: bool,
        diagnostic_code: Option<&str>,
        now: UtcTimestamp,
        trace_id: &str,
    ) -> Result<AgentRun, AgentRunStoreError> {
        if !receipt.is_object() || trace_id.trim().is_empty() {
            return Err(AgentRunStoreError::InvalidContract);
        }
        if !succeeded && diagnostic_code.is_none_or(str::is_empty) {
            return Err(AgentRunStoreError::InvalidContract);
        }
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        let mut run = load_run_for_update(&mut transaction, lease.run_id).await?;
        let row = sqlx::query(
            "SELECT execution_request,execution_receipt,lease_expires_at > now() AS lease_current
             FROM agent.agent_track_work_items
             WHERE run_id=$1 AND track='work_configuration' AND state='running'
               AND worker_id=$2 AND lease_token=$3 AND attempt_number=$4 FOR UPDATE",
        )
        .bind(lease.run_id.as_uuid())
        .bind(&lease.worker_id)
        .bind(lease.lease_token)
        .bind(i64::from(lease.attempt))
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| AgentRunStoreError::PersistenceFailed)?
        .ok_or(AgentRunStoreError::LeaseLost)?;
        let lease_current = row
            .try_get::<Option<bool>, _>("lease_current")
            .map_err(|_| AgentRunStoreError::InvalidContract)?;
        if lease_current != Some(true) {
            return Err(AgentRunStoreError::LeaseLost);
        }
        let saved_request = row
            .try_get::<Value, _>("execution_request")
            .map_err(|_| AgentRunStoreError::InvalidContract)?;
        if saved_request != lease.request
            || row
                .try_get::<Option<Value>, _>("execution_receipt")
                .map_err(|_| AgentRunStoreError::InvalidContract)?
                .is_some()
        {
            return Err(AgentRunStoreError::StateConflict);
        }
        let track = run
            .tracks
            .iter_mut()
            .find(|track| track.kind == AgentTrackKind::WorkConfiguration)
            .ok_or(AgentRunStoreError::InvalidContract)?;
        let attempt = track
            .attempts
            .last_mut()
            .filter(|attempt| {
                attempt.number == lease.attempt && attempt.state == AgentAttemptState::Running
            })
            .ok_or(AgentRunStoreError::StateConflict)?;
        attempt.state = if succeeded {
            AgentAttemptState::Succeeded
        } else if diagnostic_code == Some("LW_AGENT_WORK_EXECUTION_CANCELLED") {
            AgentAttemptState::Cancelled
        } else {
            AgentAttemptState::Failed
        };
        attempt.diagnostic_code = if succeeded {
            None
        } else {
            diagnostic_code.map(str::to_owned)
        };
        run.state = run
            .derived_state()
            .map_err(|_| AgentRunStoreError::InvalidContract)?;
        run.revision = next_revision(run.revision)?;
        run.validate()
            .map_err(|_| AgentRunStoreError::InvalidContract)?;
        update_run(&mut transaction, &run).await?;
        let updated = sqlx::query(
            "UPDATE agent.agent_track_work_items
             SET state=$6, execution_receipt=$5, worker_id=NULL, lease_token=NULL,
                 lease_expires_at=NULL, heartbeat_at=NULL, updated_at=now()
             WHERE run_id=$1 AND track='work_configuration' AND worker_id=$2
               AND lease_token=$3 AND attempt_number=$4 AND state='running'",
        )
        .bind(lease.run_id.as_uuid())
        .bind(&lease.worker_id)
        .bind(lease.lease_token)
        .bind(i64::from(lease.attempt))
        .bind(receipt)
        .bind(if succeeded {
            "succeeded"
        } else if diagnostic_code == Some("LW_AGENT_WORK_EXECUTION_CANCELLED") {
            "cancelled"
        } else {
            "failed"
        })
        .execute(&mut *transaction)
        .await
        .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        if updated.rows_affected() != 1 {
            return Err(AgentRunStoreError::LeaseLost);
        }
        if is_terminal_run(run.state) {
            let (subject, diagnostic) = terminal_event(&run);
            let sequence = next_outbox_sequence(&mut transaction, run.id).await?;
            enqueue_run_event(
                &mut transaction,
                &run,
                subject,
                sequence,
                u64::from(lease.attempt),
                diagnostic,
                now,
                trace_id,
            )
            .await?;
        }
        transaction
            .commit()
            .await
            .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        Ok(run)
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
    /// Authoritative project route scope.
    pub project_id: ProjectId,
    /// Optional teaching course route scope.
    pub course_id: Option<CourseId>,
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
    #[allow(
        clippy::large_futures,
        reason = "the public dispatch preserves one reservation and execution boundary"
    )]
    pub async fn execute(
        &self,
        command: ExecuteAgentRun<'_>,
    ) -> Result<AgentRunDispatch, AgentRunStoreError> {
        let reservation = self
            .store
            .reserve(ReserveAgentRun {
                project_id: command.project_id,
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

    /// Executes one Control-reserved typed dispatch without attempting a second reservation.
    ///
    /// Authoring keeps its existing dual-track execution, while Work configuration owns one
    /// independently fenced track and binds its generated plan in the completion transaction.
    #[allow(
        clippy::large_futures,
        reason = "the background dispatch preserves the reserved run boundary"
    )]
    pub async fn execute_reserved_dispatch(
        &self,
        lease: AgentRunDispatchLease,
        input: ImmutableEgressInput,
        cancellation: RunCancellation,
        now: UtcTimestamp,
    ) -> Result<AgentRunDispatch, AgentRunStoreError> {
        match lease.request {
            InternalAgentRunRequest::Authoring(request) => {
                let AgentRunPurpose::Authoring { environment_class } = lease.purpose else {
                    return Err(AgentRunStoreError::IdentityMismatch);
                };
                self.execute_reserved(
                    ExecuteAgentRun {
                        project_id: lease.run.project_id,
                        course_id: lease.run.course_id,
                        request: &request,
                        expected_environment_class: environment_class,
                        idempotency_key: &lease.idempotency_key,
                        input,
                        cancellation,
                        now,
                        trace_id: &lease.trace_id,
                    },
                    lease.run,
                )
                .await
            }
            InternalAgentRunRequest::WorkConfiguration(_) => {
                if !matches!(lease.purpose, AgentRunPurpose::WorkConfiguration { .. }) {
                    return Err(AgentRunStoreError::IdentityMismatch);
                }
                let track = self
                    .store
                    .claim_track(
                        lease.run.id,
                        AgentTrackKind::WorkConfiguration,
                        input.sha256(),
                        &self.worker_id,
                        self.lease_duration,
                    )
                    .await?;
                let executed = self
                    .execute_work_track(
                        track,
                        input,
                        cancellation,
                        now,
                        &lease.trace_id,
                        &lease.package,
                        lease.preauthorization.as_ref(),
                    )
                    .await?;
                let current = self.store.load(lease.run.id).await?;
                if executed {
                    Ok(AgentRunDispatch::Progressed(current))
                } else {
                    Ok(AgentRunDispatch::Replayed(current))
                }
            }
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

    #[allow(
        clippy::too_many_arguments,
        reason = "work execution carries the persisted plan and authorization context"
    )]
    async fn execute_work_track(
        &self,
        lease: Option<AgentTrackLease>,
        input: ImmutableEgressInput,
        cancellation: RunCancellation,
        now: UtcTimestamp,
        trace_id: &str,
        package: &ProblemPackage,
        preauthorization: Option<&WorkConfigurationPreauthorization>,
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
            EnvironmentClass::Work,
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
            .complete_work_track(&lease, outcome, package, preauthorization, now, trace_id)
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
    _input_sha256: Sha256Digest,
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
        previous.diagnostic_code = Some(diagnostic::PROVIDER_UNAVAILABLE.to_owned());
    }
    let attempt = u32::try_from(track.attempts.len().saturating_add(1))
        .map_err(|_| AgentRunStoreError::InvalidContract)?;
    if i64::from(attempt) != claim.attempt_number.saturating_add(1) {
        return Err(AgentRunStoreError::InvalidContract);
    }
    track.attempts.push(AgentAttempt {
        number: attempt,
        state: AgentAttemptState::Running,
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

fn work_execution_attempt(run: &AgentRun) -> Result<u32, AgentRunStoreError> {
    let track = run
        .tracks
        .iter()
        .find(|track| track.kind == AgentTrackKind::WorkConfiguration)
        .ok_or(AgentRunStoreError::InvalidContract)?;
    let attempt = track
        .attempts
        .last()
        .filter(|attempt| attempt.state == AgentAttemptState::Running)
        .ok_or(AgentRunStoreError::StateConflict)?;
    Ok(attempt.number)
}

fn validate_reservation(command: &ReserveAgentRun<'_>) -> Result<(), AgentRunStoreError> {
    if command.trace_id.trim().is_empty()
        || command.project_id != command.input.project_id()
        || command.request.project_id != command.project_id
        || command.request.course_id != command.course_id
        || command.course_id != command.input.course_id()
        || command.request.package_id != command.input.package_id()
        || command.request.package_revision != command.input.package_revision()
        || command.request.policy_id != command.policy.id
        || command.request.policy_revision != command.policy.revision
        || command.input.policy_id() != command.policy.id
        || command.input.policy_revision() != command.policy.revision
        || command.policy.project_id != command.project_id
        || command.policy.course_id != command.course_id
    {
        return Err(AgentRunStoreError::IdentityMismatch);
    }
    command
        .policy
        .validate()
        .map_err(|_| AgentRunStoreError::IdentityMismatch)
}

fn validate_reserved_run(
    command: &ExecuteAgentRun<'_>,
    run: &AgentRun,
) -> Result<(), AgentRunStoreError> {
    if run.project_id != command.project_id
        || run.course_id != command.course_id
        || run.package_id != command.request.package_id
        || run.policy_id != command.request.policy_id
        || run.state == AgentRunState::Failed
        || run.state == AgentRunState::Cancelled
    {
        return Err(AgentRunStoreError::IdentityMismatch);
    }
    if command.trace_id.trim().is_empty()
        || command.input.project_id() != command.project_id
        || command.input.course_id() != command.course_id
        || command.input.package_id() != command.request.package_id
        || command.input.package_revision() != command.request.package_revision
    {
        return Err(AgentRunStoreError::IdentityMismatch);
    }
    Ok(())
}

fn requested_run(request: &CreateAgentRunRequest) -> Result<AgentRun, AgentRunStoreError> {
    let run = AgentRun {
        id: AgentRunId::new(),
        project_id: request.project_id,
        course_id: request.course_id,
        package_id: request.package_id,
        policy_id: request.policy_id,
        policy_revision: request.policy_revision,
        purpose: AgentRunPurpose::Authoring {
            environment_class: request.environment_class,
        },
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
        plan: None,
    };
    run.validate()
        .map_err(|_| AgentRunStoreError::InvalidContract)?;
    Ok(run)
}

fn requested_internal_run(
    request: &InternalAgentRunRequest,
    purpose: AgentRunPurpose,
) -> Result<AgentRun, AgentRunStoreError> {
    let (project_id, course_id, package_id, policy_id, policy_revision) = match request {
        InternalAgentRunRequest::Authoring(request) => (
            request.project_id,
            request.course_id,
            request.package_id,
            request.policy_id,
            request.policy_revision,
        ),
        InternalAgentRunRequest::WorkConfiguration(request) => (
            request.project_id,
            request.course_id,
            request.package_id,
            request.policy_id,
            request.policy_revision,
        ),
    };
    let tracks = tracks_for_purpose(purpose)
        .into_iter()
        .map(|kind| AgentTrack {
            kind,
            attempts: Vec::new(),
            candidate_id: None,
        })
        .collect();
    let run = AgentRun {
        id: AgentRunId::new(),
        project_id,
        course_id,
        package_id,
        policy_id,
        policy_revision,
        purpose,
        state: AgentRunState::Requested,
        revision: Revision::new(1).map_err(|_| AgentRunStoreError::InvalidContract)?,
        tracks,
        plan: None,
    };
    run.validate()
        .map_err(|_| AgentRunStoreError::InvalidContract)?;
    Ok(run)
}

fn tracks_for_purpose(purpose: AgentRunPurpose) -> Vec<AgentTrackKind> {
    match purpose {
        AgentRunPurpose::Authoring { .. } => {
            vec![AgentTrackKind::Environment, AgentTrackKind::Evaluation]
        }
        AgentRunPurpose::WorkConfiguration { .. } => vec![AgentTrackKind::WorkConfiguration],
    }
}

async fn load_run_for_update(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    run_id: AgentRunId,
) -> Result<AgentRun, AgentRunStoreError> {
    let row = sqlx::query(
        "SELECT project_id, course_id, problem_package_id, revision, state, input_sha256, \
                policy_revision, plan, contract \
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
    let persisted_plan = row
        .try_get::<Option<Value>, _>("plan")
        .map_err(|_| AgentRunStoreError::InvalidContract)?;
    let contract_plan = run
        .plan
        .as_ref()
        .map(serde_json::to_value)
        .transpose()
        .map_err(|_| AgentRunStoreError::InvalidContract)?;
    if persisted_plan != contract_plan {
        return Err(AgentRunStoreError::InvalidContract);
    }
    let project_id = row
        .try_get::<uuid::Uuid, _>("project_id")
        .map_err(|_| AgentRunStoreError::InvalidContract)?;
    let course_id = row
        .try_get::<Option<uuid::Uuid>, _>("course_id")
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
    if project_id != run.project_id.as_uuid()
        || course_id != run.course_id.map(CourseId::as_uuid)
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
    let plan = run
        .plan
        .as_ref()
        .map(serde_json::to_value)
        .transpose()
        .map_err(|_| AgentRunStoreError::InvalidContract)?;
    let contract = serde_json::to_value(run).map_err(|_| AgentRunStoreError::InvalidContract)?;
    let updated = sqlx::query(
        "UPDATE agent.agent_runs SET revision = $2, state = $3, plan = $4, contract = $5, updated_at = now() \
         WHERE run_id = $1 AND revision = $6",
    )
    .bind(run.id.as_uuid())
    .bind(revision_i64(run.revision)?)
    .bind(run_state(run.state))
    .bind(plan)
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
            let candidate = EnvironmentCandidate {
                id: CandidateId::new(),
                run_id: run.id,
                project_id: run.project_id,
                course_id: run.course_id,
                revision: Revision::new(1).map_err(|_| AgentRunStoreError::InvalidContract)?,
                spec,
                policy_revision: run.policy_revision,
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
                project_id: run.project_id,
                course_id: run.course_id,
                revision: Revision::new(1).map_err(|_| AgentRunStoreError::InvalidContract)?,
                spec,
                policy_revision: run.policy_revision,
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

fn work_configuration_checkpoint(
    run: &AgentRun,
    attempt: u32,
    result: Result<ClaudeCodeExecution, ClaudeCodeFailure>,
    _now: UtcTimestamp,
) -> Result<AgentTrackCheckpoint, AgentRunStoreError> {
    let audit = match result {
        Ok(execution) => {
            if !matches!(execution.document, CandidateDocument::WorkConfiguration(_)) {
                return Err(AgentRunStoreError::InvalidContract);
            }
            execution.audit
        }
        Err(failure) => failure.audit().clone(),
    };
    Ok(AgentTrackCheckpoint {
        run_id: run.id,
        sequence: checkpoint_sequence(AgentTrackKind::WorkConfiguration, attempt)?,
        track: AgentTrackKind::WorkConfiguration,
        attempt,
        audit,
        candidate: None,
    })
}

fn bind_work_configuration_plan(
    run: &AgentRun,
    package: &ProblemPackage,
    _preauthorization: Option<&WorkConfigurationPreauthorization>,
    execution: &ClaudeCodeExecution,
    _now: UtcTimestamp,
) -> Result<WorkConfigurationPlan, AgentRunStoreError> {
    package
        .validate()
        .map_err(|_| AgentRunStoreError::InvalidContract)?;
    if package.project_id != run.project_id
        || package.course_id != run.course_id
        || package.id != run.package_id
    {
        return Err(AgentRunStoreError::IdentityMismatch);
    }
    let CandidateDocument::WorkConfiguration(draft) = &execution.document else {
        return Err(AgentRunStoreError::InvalidContract);
    };
    draft
        .validate()
        .map_err(|()| AgentRunStoreError::InvalidContract)?;
    let script_artifact = draft
        .script_artifact
        .clone()
        .ok_or(AgentRunStoreError::InvalidContract)?;
    let verification_script_artifact = draft.verification_script_artifact.clone();
    let (environment_id, environment_revision) = match run.purpose {
        AgentRunPurpose::WorkConfiguration {
            environment_id,
            environment_revision,
            ..
        } => (environment_id, environment_revision),
        AgentRunPurpose::Authoring { .. } => return Err(AgentRunStoreError::InvalidContract),
    };
    let plan = WorkConfigurationPlan {
        id: WorkConfigurationPlanId::new(),
        revision: Revision::new(1).unwrap_or_else(|error| unreachable!("one is valid: {error}")),
        script_artifact,
        verification_script_artifact,
        summary: draft.summary.clone(),
        requires_restart: draft.requires_restart,
        environment_id,
        environment_revision,
    };
    plan.validate()
        .map_err(|_| AgentRunStoreError::InvalidContract)?;
    // A preauthorization belongs to an already generated plan. A new Agent invocation always
    // creates a new plan identity, so a stale or mismatched grant is deliberately ignored and the
    // fresh plan remains AwaitingApproval. Control rebinds a new grant only after reviewing this
    // exact plan.
    Ok(plan)
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
        || checkpoint.audit.track != checkpoint.track
        || checkpoint.audit.project_id != run.project_id
        || checkpoint.audit.course_id != run.course_id
        || checkpoint.audit.package_id != run.package_id
        || checkpoint.audit.policy_id != run.policy_id
        || checkpoint.audit.policy_revision != run.policy_revision
    {
        return Err(AgentRunStoreError::IdentityMismatch);
    }
    attempt.usage = checkpoint.audit.usage;
    attempt.usage_observed = checkpoint.audit.usage_observed;
    let work_succeeded = checkpoint.track == AgentTrackKind::WorkConfiguration
        && checkpoint.audit.outcome == RuntimeAuditOutcome::Succeeded;
    if let Some(candidate) = &checkpoint.candidate {
        attempt.state = AgentAttemptState::Succeeded;
        attempt.diagnostic_code = None;
        track.candidate_id = Some(match candidate {
            StoredCandidate::Environment(candidate) => candidate.id,
            StoredCandidate::Evaluation(candidate) => candidate.id,
        });
    } else if work_succeeded {
        // A generated Work configuration is a proposal.  Keep the attempt in the
        // approval state until the separately authorized runtime execution reports
        // its result; marking it succeeded here would make the proposal look
        // complete and would violate the Work run state machine.
        attempt.state = AgentAttemptState::AwaitingApproval;
        attempt.diagnostic_code = None;
        track.candidate_id = None;
    } else {
        attempt.state = if checkpoint.audit.outcome == RuntimeAuditOutcome::Cancelled {
            AgentAttemptState::Cancelled
        } else {
            AgentAttemptState::Failed
        };
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
    if checkpoint.track == AgentTrackKind::WorkConfiguration
        && checkpoint.audit.outcome == RuntimeAuditOutcome::Succeeded
    {
        "awaiting_approval"
    } else if checkpoint.candidate.is_some() {
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
        project_id: run.project_id,
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
        AgentTrackKind::WorkConfiguration => 3,
    })
    .ok_or(AgentRunStoreError::InvalidContract)
}

const fn track_name(track: AgentTrackKind) -> &'static str {
    match track {
        AgentTrackKind::Environment => "environment",
        AgentTrackKind::Evaluation => "evaluation",
        AgentTrackKind::WorkConfiguration => "work_configuration",
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
        || lease_duration > Duration::from_hours(1)
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
        AgentRunState::AwaitingApproval => "awaiting_approval",
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
