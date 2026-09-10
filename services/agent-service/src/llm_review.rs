//! Agent-owned queue and worker for bounded advisory LLM reviews.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use contracts::authoring::LlmUsage;
use contracts::http::{
    AgentLlmReviewQuery, AgentLlmReviewState, IdempotencyKey, InternalAgentLlmReviewReceipt,
    InternalAgentLlmReviewRequest,
};
use contracts::{CourseId, ProjectId, TaskRunId, UtcTimestamp};
use persistence_sqlx::{Domain, IdempotencyDecision, IdempotencyStore, Sha256Digest};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use thiserror::Error;
use time::OffsetDateTime;
use tokio::time::interval;
use uuid::Uuid;

use crate::claude_code::{
    ClaudeCodeProcess, ClaudeCodeReviewFailure, ClaudeCodeRuntime, EgressClassifier,
    RunCancellation,
};

const CREATE_OPERATION: &str = "create_agent_llm_review_v1";
const CANCEL_OPERATION: &str = "cancel_agent_llm_review_v1";
const REVIEW_CANCELLED: &str = "LW_LLM_REVIEW_CANCELLED";
const REVIEW_DEADLINE_EXCEEDED: &str = "LW_LLM_REVIEW_DEADLINE_EXCEEDED";
const REVIEW_INTERRUPTED: &str = "LW_LLM_REVIEW_INTERRUPTED";

/// Persistent Agent-owned advisory review queue.
#[derive(Clone, Debug)]
pub struct LlmReviewStore {
    pool: PgPool,
}

impl LlmReviewStore {
    /// Creates a store over the Agent-owned database pool.
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Enqueues one exact request, replaying an existing task identity only when its full request
    /// hash matches.
    pub async fn enqueue(
        &self,
        request: &InternalAgentLlmReviewRequest,
        key: &IdempotencyKey,
        now: UtcTimestamp,
    ) -> Result<InternalAgentLlmReviewReceipt, LlmReviewStoreError> {
        // Validate the immutable request contract before opening the transaction.  The
        // deadline is intentionally checked only after looking up an existing task below:
        // retrying an exact request must replay its durable receipt even when that receipt is
        // already past its execution deadline.
        validate_request_static(request)?;
        let request_json =
            serde_json::to_value(request).map_err(|_| LlmReviewStoreError::InvalidContract)?;
        let request_hash = Sha256Digest::of_canonical(request)
            .map_err(|_| LlmReviewStoreError::InvalidContract)?;
        let request_sha256 = request_hash.to_string();
        let receipt = queued_receipt(request.task_run_id, request_sha256.clone())?;
        let receipt_json =
            serde_json::to_value(&receipt).map_err(|_| LlmReviewStoreError::InvalidContract)?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| LlmReviewStoreError::PersistenceFailed)?;

        if let Some(row) = sqlx::query(
            "SELECT request_sha256,receipt_json FROM agent.llm_review_runs \
             WHERE task_run_id=$1 FOR UPDATE",
        )
        .bind(request.task_run_id.as_uuid())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| LlmReviewStoreError::PersistenceFailed)?
        {
            let existing_hash = row
                .try_get::<String, _>("request_sha256")
                .map_err(|_| LlmReviewStoreError::InvalidContract)?;
            if existing_hash != request_sha256 {
                return Err(LlmReviewStoreError::IdempotencyConflict);
            }
            let existing = row
                .try_get::<Value, _>("receipt_json")
                .map_err(|_| LlmReviewStoreError::InvalidContract)?;
            transaction
                .rollback()
                .await
                .map_err(|_| LlmReviewStoreError::PersistenceFailed)?;
            return decode_receipt(existing);
        }

        // This is a new task identity, so an elapsed deadline is a rejected enqueue rather than
        // a replay.  Keep the check inside the transaction after the exact task lookup so the
        // replay semantics remain atomic with the identity comparison.
        validate_request_deadline(request, now)?;

        match IdempotencyStore::reserve(
            &mut transaction,
            Domain::Agent,
            CREATE_OPERATION,
            key.as_str(),
            request_hash,
        )
        .await
        .map_err(|_| LlmReviewStoreError::PersistenceFailed)?
        {
            IdempotencyDecision::Replay(value) => {
                transaction
                    .rollback()
                    .await
                    .map_err(|_| LlmReviewStoreError::PersistenceFailed)?;
                return decode_receipt(value);
            }
            IdempotencyDecision::Conflict => {
                return Err(LlmReviewStoreError::IdempotencyConflict);
            }
            IdempotencyDecision::InProgress => return Err(LlmReviewStoreError::InProgress),
            IdempotencyDecision::Reserved => {}
        }

        sqlx::query(
            "INSERT INTO agent.llm_review_runs \
             (task_run_id,project_id,course_id,request_json,request_sha256,state,attempt,receipt_json,created_at,updated_at) \
             VALUES ($1,$2,$3,$4,$5,'queued',0,$6,$7,$7)",
        )
        .bind(request.task_run_id.as_uuid())
        .bind(request.project_id.as_uuid())
        .bind(request.course_id.map(CourseId::as_uuid))
        .bind(request_json)
        .bind(&request_sha256)
        .bind(&receipt_json)
        .bind(now.get())
        .execute(&mut *transaction)
        .await
        .map_err(|_| LlmReviewStoreError::PersistenceFailed)?;
        IdempotencyStore::complete(
            &mut transaction,
            Domain::Agent,
            CREATE_OPERATION,
            key.as_str(),
            &receipt_json,
        )
        .await
        .map_err(|_| LlmReviewStoreError::PersistenceFailed)?;
        transaction
            .commit()
            .await
            .map_err(|_| LlmReviewStoreError::PersistenceFailed)?;
        Ok(receipt)
    }

    /// Reads one receipt in the exact project and optional course scope.
    pub async fn get(
        &self,
        task_run_id: TaskRunId,
        query: &AgentLlmReviewQuery,
    ) -> Result<InternalAgentLlmReviewReceipt, LlmReviewStoreError> {
        let value = sqlx::query_scalar::<_, Value>(
            "SELECT receipt_json FROM agent.llm_review_runs \
             WHERE task_run_id=$1 AND project_id=$2 \
               AND course_id IS NOT DISTINCT FROM $3",
        )
        .bind(task_run_id.as_uuid())
        .bind(query.project_id.as_uuid())
        .bind(query.course_id.map(CourseId::as_uuid))
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| LlmReviewStoreError::PersistenceFailed)?
        .ok_or(LlmReviewStoreError::NotFound)?;
        decode_receipt(value)
    }

    /// Persists a cancellation request. Queued work becomes terminal immediately; running work
    /// enters `cancelling` and the worker observes that state through its lease heartbeat.
    #[allow(
        clippy::too_many_lines,
        reason = "cancellation updates idempotency, state, and receipt atomically"
    )]
    pub async fn cancel(
        &self,
        task_run_id: TaskRunId,
        query: &AgentLlmReviewQuery,
        key: &IdempotencyKey,
        now: UtcTimestamp,
    ) -> Result<InternalAgentLlmReviewReceipt, LlmReviewStoreError> {
        let request_hash = Sha256Digest::of_canonical(&json!({
            "taskRunId": task_run_id,
            "projectId": query.project_id,
            "courseId": query.course_id,
        }))
        .map_err(|_| LlmReviewStoreError::InvalidContract)?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| LlmReviewStoreError::PersistenceFailed)?;
        match IdempotencyStore::reserve(
            &mut transaction,
            Domain::Agent,
            CANCEL_OPERATION,
            key.as_str(),
            request_hash,
        )
        .await
        .map_err(|_| LlmReviewStoreError::PersistenceFailed)?
        {
            IdempotencyDecision::Replay(value) => {
                transaction
                    .rollback()
                    .await
                    .map_err(|_| LlmReviewStoreError::PersistenceFailed)?;
                return decode_receipt(value);
            }
            IdempotencyDecision::Conflict => {
                return Err(LlmReviewStoreError::IdempotencyConflict);
            }
            IdempotencyDecision::InProgress => return Err(LlmReviewStoreError::InProgress),
            IdempotencyDecision::Reserved => {}
        }
        let row = sqlx::query(
            "SELECT request_sha256,state,receipt_json FROM agent.llm_review_runs \
             WHERE task_run_id=$1 AND project_id=$2 \
               AND course_id IS NOT DISTINCT FROM $3 FOR UPDATE",
        )
        .bind(task_run_id.as_uuid())
        .bind(query.project_id.as_uuid())
        .bind(query.course_id.map(CourseId::as_uuid))
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| LlmReviewStoreError::PersistenceFailed)?
        .ok_or(LlmReviewStoreError::NotFound)?;
        let request_sha256 = row
            .try_get::<String, _>("request_sha256")
            .map_err(|_| LlmReviewStoreError::InvalidContract)?;
        let state = row
            .try_get::<String, _>("state")
            .map_err(|_| LlmReviewStoreError::InvalidContract)?;
        let mut receipt = decode_receipt(
            row.try_get::<Value, _>("receipt_json")
                .map_err(|_| LlmReviewStoreError::InvalidContract)?,
        )?;
        match state.as_str() {
            "queued" => {
                receipt.state = AgentLlmReviewState::Cancelled;
                receipt.diagnostic_code = Some(REVIEW_CANCELLED.to_owned());
                receipt.finished_at = Some(now);
                update_receipt_state(
                    &mut transaction,
                    task_run_id,
                    "cancelled",
                    &receipt,
                    Some(REVIEW_CANCELLED),
                    Some(now),
                )
                .await?;
            }
            "running" => {
                receipt.state = AgentLlmReviewState::Cancelling;
                update_receipt_state(
                    &mut transaction,
                    task_run_id,
                    "cancelling",
                    &receipt,
                    None,
                    None,
                )
                .await?;
            }
            "cancelling" | "succeeded" | "failed" | "cancelled" => {}
            _ => return Err(LlmReviewStoreError::InvalidContract),
        }
        let value =
            serde_json::to_value(&receipt).map_err(|_| LlmReviewStoreError::InvalidContract)?;
        IdempotencyStore::complete(
            &mut transaction,
            Domain::Agent,
            CANCEL_OPERATION,
            key.as_str(),
            &value,
        )
        .await
        .map_err(|_| LlmReviewStoreError::PersistenceFailed)?;
        transaction
            .commit()
            .await
            .map_err(|_| LlmReviewStoreError::PersistenceFailed)?;
        if receipt.task_run_id != task_run_id || receipt.request_sha256 != request_sha256 {
            return Err(LlmReviewStoreError::InvalidContract);
        }
        Ok(receipt)
    }

    /// Claims one queued review. An expired provider lease is finalized as interrupted and is
    /// never re-executed because the provider may still be running or may already have charged.
    #[allow(
        clippy::too_many_lines,
        reason = "lease claim and expired-lease fencing stay one transaction boundary"
    )]
    pub async fn claim(
        &self,
        worker_id: &str,
        lease_duration: Duration,
        now: UtcTimestamp,
    ) -> Result<Option<LlmReviewLease>, LlmReviewStoreError> {
        if worker_id.trim().is_empty()
            || worker_id.trim() != worker_id
            || lease_duration.is_zero()
            || lease_duration > Duration::from_hours(1)
        {
            return Err(LlmReviewStoreError::WorkerIdentityInvalid);
        }
        let lease_milliseconds = i64::try_from(lease_duration.as_millis())
            .map_err(|_| LlmReviewStoreError::InvalidContract)?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| LlmReviewStoreError::PersistenceFailed)?;
        let row = sqlx::query(
            "SELECT task_run_id,project_id,course_id,request_json,request_sha256,state,attempt \
             FROM agent.llm_review_runs \
             WHERE state IN ('queued','running','cancelling') \
               AND (state='queued' OR lease_expires_at IS NULL OR lease_expires_at <= now()) \
             ORDER BY created_at,task_run_id LIMIT 1 FOR UPDATE SKIP LOCKED",
        )
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| LlmReviewStoreError::PersistenceFailed)?;
        let Some(row) = row else {
            transaction
                .rollback()
                .await
                .map_err(|_| LlmReviewStoreError::PersistenceFailed)?;
            return Ok(None);
        };
        let task_run_id = parse_task_run_id(
            row.try_get::<Uuid, _>("task_run_id")
                .map_err(|_| LlmReviewStoreError::InvalidContract)?,
        )?;
        let project_id = parse_project_id(
            row.try_get::<Uuid, _>("project_id")
                .map_err(|_| LlmReviewStoreError::InvalidContract)?,
        )?;
        let course_id = row
            .try_get::<Option<Uuid>, _>("course_id")
            .map_err(|_| LlmReviewStoreError::InvalidContract)?
            .map(parse_course_id)
            .transpose()?;
        let state = row
            .try_get::<String, _>("state")
            .map_err(|_| LlmReviewStoreError::InvalidContract)?;
        let request_sha256 = row
            .try_get::<String, _>("request_sha256")
            .map_err(|_| LlmReviewStoreError::InvalidContract)?;
        if matches!(state.as_str(), "running" | "cancelling") {
            // An expired lease does not prove that the provider process stopped. Mark the
            // attempt interrupted and never issue a second provider invocation for this task.
            let mut receipt = decode_receipt(
                row.try_get::<Value, _>("receipt_json")
                    .map_err(|_| LlmReviewStoreError::InvalidContract)?,
            )?;
            if receipt.task_run_id != task_run_id
                || receipt.request_sha256 != request_sha256
                || !matches!(
                    receipt.state,
                    AgentLlmReviewState::Running | AgentLlmReviewState::Cancelling
                )
            {
                return Err(LlmReviewStoreError::InvalidContract);
            }
            receipt.state = AgentLlmReviewState::Failed;
            receipt.review = None;
            // Any provider usage from an unfinished invocation is unknown. Do not infer zero.
            receipt.usage = None;
            receipt.diagnostic_code = Some(REVIEW_INTERRUPTED.to_owned());
            receipt.finished_at = Some(now);
            receipt
                .validate()
                .map_err(|_| LlmReviewStoreError::InvalidContract)?;
            let value =
                serde_json::to_value(&receipt).map_err(|_| LlmReviewStoreError::InvalidContract)?;
            let updated = sqlx::query(
                "UPDATE agent.llm_review_runs SET state='failed',receipt_json=$2, \
                 diagnostic_code=$3,finished_at=$4,worker_id=NULL,lease_token=NULL, \
                 lease_expires_at=NULL,heartbeat_at=NULL,updated_at=$4 \
                 WHERE task_run_id=$1 AND state IN ('running','cancelling') \
                   AND (lease_expires_at IS NULL OR lease_expires_at <= now())",
            )
            .bind(task_run_id.as_uuid())
            .bind(value)
            .bind(REVIEW_INTERRUPTED)
            .bind(now.get())
            .execute(&mut *transaction)
            .await
            .map_err(|_| LlmReviewStoreError::PersistenceFailed)?;
            if updated.rows_affected() != 1 {
                return Err(LlmReviewStoreError::LeaseLost);
            }
            transaction
                .commit()
                .await
                .map_err(|_| LlmReviewStoreError::PersistenceFailed)?;
            return Ok(None);
        }
        let request: InternalAgentLlmReviewRequest = serde_json::from_value(
            row.try_get::<Value, _>("request_json")
                .map_err(|_| LlmReviewStoreError::InvalidContract)?,
        )
        .map_err(|_| LlmReviewStoreError::InvalidContract)?;
        validate_request_for_worker(&request)?;
        if request.task_run_id != task_run_id
            || request.project_id != project_id
            || request.course_id != course_id
            || Sha256Digest::of_canonical(&request)
                .map_err(|_| LlmReviewStoreError::InvalidContract)?
                .to_string()
                != request_sha256
        {
            return Err(LlmReviewStoreError::InvalidContract);
        }
        let attempt = row
            .try_get::<i64, _>("attempt")
            .map_err(|_| LlmReviewStoreError::InvalidContract)?
            .checked_add(1)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or(LlmReviewStoreError::InvalidContract)?;
        let token = Uuid::now_v7();
        let mut receipt = queued_receipt(task_run_id, request_sha256.clone())?;
        receipt.state = AgentLlmReviewState::Running;
        receipt.started_at = Some(now);
        let receipt_json =
            serde_json::to_value(&receipt).map_err(|_| LlmReviewStoreError::InvalidContract)?;
        let updated = sqlx::query(
            "UPDATE agent.llm_review_runs SET state='running',attempt=$2,worker_id=$3, \
             lease_token=$4,heartbeat_at=$5,lease_expires_at=$5 + ($6 * interval '1 millisecond'), \
             receipt_json=$7,updated_at=$5 WHERE task_run_id=$1 \
             AND state IN ('queued','running') \
             AND (lease_expires_at IS NULL OR lease_expires_at <= now())",
        )
        .bind(task_run_id.as_uuid())
        .bind(i64::from(attempt))
        .bind(worker_id)
        .bind(token)
        .bind(now.get())
        .bind(lease_milliseconds)
        .bind(receipt_json)
        .execute(&mut *transaction)
        .await
        .map_err(|_| LlmReviewStoreError::PersistenceFailed)?;
        if updated.rows_affected() != 1 {
            return Err(LlmReviewStoreError::LeaseLost);
        }
        transaction
            .commit()
            .await
            .map_err(|_| LlmReviewStoreError::PersistenceFailed)?;
        Ok(Some(LlmReviewLease {
            task_run_id,
            request,
            request_sha256,
            attempt,
            worker_id: worker_id.to_owned(),
            lease_token: token,
        }))
    }

    /// Heartbeats one worker lease and reports whether cancellation was persisted.
    pub async fn heartbeat(
        &self,
        lease: &LlmReviewLease,
        lease_duration: Duration,
    ) -> Result<bool, LlmReviewStoreError> {
        let milliseconds = i64::try_from(lease_duration.as_millis())
            .map_err(|_| LlmReviewStoreError::InvalidContract)?;
        let state = sqlx::query_scalar::<_, String>(
            "UPDATE agent.llm_review_runs SET heartbeat_at=clock_timestamp(), \
             lease_expires_at=clock_timestamp() + ($5 * interval '1 millisecond'),updated_at=now() \
             WHERE task_run_id=$1 AND state IN ('running','cancelling') AND worker_id=$2 \
               AND lease_token=$3 AND attempt=$4 AND lease_expires_at > now() \
             RETURNING state",
        )
        .bind(lease.task_run_id.as_uuid())
        .bind(&lease.worker_id)
        .bind(lease.lease_token)
        .bind(i64::from(lease.attempt))
        .bind(milliseconds)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| LlmReviewStoreError::PersistenceFailed)?
        .ok_or(LlmReviewStoreError::LeaseLost)?;
        Ok(state == "cancelling")
    }

    /// Commits the provider result behind the exact worker lease fence.
    #[allow(
        clippy::too_many_lines,
        reason = "completion validates the receipt and updates idempotency atomically"
    )]
    pub async fn complete(
        &self,
        lease: &LlmReviewLease,
        review: Option<contracts::evaluation::GoalReview>,
        usage: Option<LlmUsage>,
        diagnostic_code: Option<&str>,
        now: UtcTimestamp,
    ) -> Result<InternalAgentLlmReviewReceipt, LlmReviewStoreError> {
        if review.is_some() {
            if diagnostic_code.is_some() || usage.is_none() {
                return Err(LlmReviewStoreError::InvalidContract);
            }
        } else if diagnostic_code.is_none() {
            return Err(LlmReviewStoreError::InvalidContract);
        }
        if let Some(review) = &review {
            review
                .validate()
                .map_err(|_| LlmReviewStoreError::InvalidContract)?;
        }
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| LlmReviewStoreError::PersistenceFailed)?;
        let row = sqlx::query(
            "SELECT state,request_json,request_sha256,lease_expires_at > now() AS lease_current \
             FROM agent.llm_review_runs WHERE task_run_id=$1 AND worker_id=$2 \
               AND lease_token=$3 AND attempt=$4 FOR UPDATE",
        )
        .bind(lease.task_run_id.as_uuid())
        .bind(&lease.worker_id)
        .bind(lease.lease_token)
        .bind(i64::from(lease.attempt))
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| LlmReviewStoreError::PersistenceFailed)?
        .ok_or(LlmReviewStoreError::LeaseLost)?;
        if row
            .try_get::<Option<bool>, _>("lease_current")
            .map_err(|_| LlmReviewStoreError::InvalidContract)?
            != Some(true)
        {
            return Err(LlmReviewStoreError::LeaseLost);
        }
        let request_json = row
            .try_get::<Value, _>("request_json")
            .map_err(|_| LlmReviewStoreError::InvalidContract)?;
        let saved: InternalAgentLlmReviewRequest = serde_json::from_value(request_json)
            .map_err(|_| LlmReviewStoreError::InvalidContract)?;
        if saved != lease.request {
            return Err(LlmReviewStoreError::StateConflict);
        }
        let saved_hash = row
            .try_get::<String, _>("request_sha256")
            .map_err(|_| LlmReviewStoreError::InvalidContract)?;
        if saved_hash != lease.request_sha256 {
            return Err(LlmReviewStoreError::StateConflict);
        }
        let cancelling = row
            .try_get::<String, _>("state")
            .map_err(|_| LlmReviewStoreError::InvalidContract)?
            == "cancelling";
        let (state, review, usage, diagnostic_code) = if cancelling {
            (
                AgentLlmReviewState::Cancelled,
                None,
                None,
                Some(REVIEW_CANCELLED.to_owned()),
            )
        } else if let Some(review) = review {
            (AgentLlmReviewState::Succeeded, Some(review), usage, None)
        } else {
            let diagnostic = diagnostic_code
                .filter(|value| !value.trim().is_empty())
                .ok_or(LlmReviewStoreError::InvalidContract)?
                .to_owned();
            (
                if diagnostic == REVIEW_CANCELLED {
                    AgentLlmReviewState::Cancelled
                } else {
                    AgentLlmReviewState::Failed
                },
                None,
                usage,
                Some(diagnostic),
            )
        };
        let receipt = InternalAgentLlmReviewReceipt {
            task_run_id: lease.task_run_id,
            request_sha256: lease.request_sha256.clone(),
            state,
            review,
            usage,
            diagnostic_code: diagnostic_code.clone(),
            started_at: Some(now),
            finished_at: Some(now),
        };
        receipt
            .validate()
            .map_err(|_| LlmReviewStoreError::InvalidContract)?;
        let receipt_json =
            serde_json::to_value(&receipt).map_err(|_| LlmReviewStoreError::InvalidContract)?;
        let updated = sqlx::query(
            "UPDATE agent.llm_review_runs SET state=$2,receipt_json=$3,diagnostic_code=$4, \
             finished_at=$5,worker_id=NULL,lease_token=NULL,heartbeat_at=NULL,lease_expires_at=NULL, \
             updated_at=$5 WHERE task_run_id=$1 AND worker_id=$6 AND lease_token=$7 AND attempt=$8 \
             AND state IN ('running','cancelling')",
        )
        .bind(lease.task_run_id.as_uuid())
        .bind(state_name(state))
        .bind(receipt_json)
        .bind(diagnostic_code)
        .bind(now.get())
        .bind(&lease.worker_id)
        .bind(lease.lease_token)
        .bind(i64::from(lease.attempt))
        .execute(&mut *transaction)
        .await
        .map_err(|_| LlmReviewStoreError::PersistenceFailed)?;
        if updated.rows_affected() != 1 {
            return Err(LlmReviewStoreError::LeaseLost);
        }
        transaction
            .commit()
            .await
            .map_err(|_| LlmReviewStoreError::PersistenceFailed)?;
        Ok(receipt)
    }
}

/// One claimed review and its fencing identity.
#[derive(Clone, Debug)]
pub struct LlmReviewLease {
    pub task_run_id: TaskRunId,
    pub request: InternalAgentLlmReviewRequest,
    pub request_sha256: String,
    pub attempt: u32,
    worker_id: String,
    lease_token: Uuid,
}

/// Long-running worker for Agent-owned advisory reviews.
#[derive(Clone)]
pub struct LlmReviewWorker {
    pub store: LlmReviewStore,
    pub classifier: Arc<dyn EgressClassifier>,
    pub process: Arc<dyn ClaudeCodeProcess>,
    pub worker_id: String,
    pub lease_duration: Duration,
    pub poll_interval: Duration,
}

impl LlmReviewWorker {
    /// Runs until the service is stopped.
    pub async fn run(self) -> Result<(), LlmReviewStoreError> {
        let mut ticker = interval(self.poll_interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let now = timestamp()?;
            let Some(lease) = self
                .store
                .claim(&self.worker_id, self.lease_duration, now)
                .await?
            else {
                continue;
            };
            self.run_one(lease).await?;
        }
    }

    async fn run_one(&self, lease: LlmReviewLease) -> Result<(), LlmReviewStoreError> {
        let cancellation = RunCancellation::new();
        let heartbeat_cancel = cancellation.clone();
        let store = self.store.clone();
        let heartbeat_lease = lease.clone();
        let heartbeat_interval = (self.lease_duration / 3).max(Duration::from_millis(100));
        let heartbeat_duration = self.lease_duration;
        let heartbeat = tokio::spawn(async move {
            let mut ticker = interval(heartbeat_interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                match store.heartbeat(&heartbeat_lease, heartbeat_duration).await {
                    Ok(true) => heartbeat_cancel.cancel(),
                    Ok(false) => {}
                    Err(_) => {
                        heartbeat_cancel.cancel();
                        break;
                    }
                }
            }
        });
        let result = self.execute(&lease, cancellation.clone()).await;
        heartbeat.abort();
        let now = timestamp()?;
        match result {
            Ok(execution) => {
                match self
                    .store
                    .complete(
                        &lease,
                        Some(execution.review),
                        Some(execution.usage),
                        None,
                        now,
                    )
                    .await
                {
                    Ok(_) | Err(LlmReviewStoreError::LeaseLost) => Ok(()),
                    Err(error) => Err(error),
                }
            }
            Err(diagnostic) => {
                match self
                    .store
                    .complete(
                        &lease,
                        None,
                        diagnostic.usage,
                        Some(diagnostic.diagnostic_code),
                        now,
                    )
                    .await
                {
                    Ok(_) | Err(LlmReviewStoreError::LeaseLost) => Ok(()),
                    Err(error) => Err(error),
                }
            }
        }
    }

    async fn execute(
        &self,
        lease: &LlmReviewLease,
        cancellation: RunCancellation,
    ) -> Result<crate::claude_code::ClaudeCodeReviewExecution, ReviewExecutionFailure> {
        let deadline = lease.request.deadline_at.get();
        let now = OffsetDateTime::now_utc();
        if now >= deadline {
            return Err(review_execution_failure(REVIEW_DEADLINE_EXCEEDED, None));
        }
        if cancellation.is_cancelled() {
            return Err(review_execution_failure(REVIEW_CANCELLED, None));
        }
        let remaining = std::time::Duration::try_from(deadline - now)
            .map_err(|_| review_execution_failure(REVIEW_DEADLINE_EXCEEDED, None))?;
        let deadline_reached = Arc::new(AtomicBool::new(false));
        let timer_flag = Arc::clone(&deadline_reached);
        let timer_cancel = cancellation.clone();
        let deadline_timer = tokio::spawn(async move {
            tokio::time::sleep(remaining).await;
            timer_flag.store(true, Ordering::Release);
            timer_cancel.cancel();
        });
        let result = self
            .execute_inner(&lease.request, cancellation.clone(), &deadline_reached)
            .await;
        deadline_timer.abort();
        let deadline_expired =
            deadline_reached.load(Ordering::Acquire) || OffsetDateTime::now_utc() >= deadline;
        match result {
            Ok(execution) if deadline_expired => Err(review_execution_failure(
                REVIEW_DEADLINE_EXCEEDED,
                Some(execution.usage),
            )),
            Ok(execution) if cancellation.is_cancelled() => Err(review_execution_failure(
                REVIEW_CANCELLED,
                Some(execution.usage),
            )),
            Ok(execution) => Ok(execution),
            Err(failure) if deadline_expired => Err(review_execution_failure(
                REVIEW_DEADLINE_EXCEEDED,
                failure.usage,
            )),
            Err(failure) => Err(failure),
        }
    }

    async fn execute_inner(
        &self,
        request: &InternalAgentLlmReviewRequest,
        cancellation: RunCancellation,
        deadline_reached: &AtomicBool,
    ) -> Result<crate::claude_code::ClaudeCodeReviewExecution, ReviewExecutionFailure> {
        if let Some(failure) = cancellation_failure(&cancellation, deadline_reached, None) {
            return Err(failure);
        }
        validate_request_contents(request)
            .map_err(|_| review_execution_failure("LW_LLM_EGRESS_DENIED", None))?;
        for file in &request.files {
            let denied = tokio::select! {
                biased;
                () = cancellation.cancelled() => {
                    return Err(cancellation_failure(&cancellation, deadline_reached, None)
                        .unwrap_or_else(|| review_execution_failure(REVIEW_CANCELLED, None)));
                }
                result = self.classifier.classify(&file.path, file.content.as_bytes()) => {
                    result.map_err(|_| review_execution_failure("LW_LLM_EGRESS_DENIED", None))?
                }
            };
            if let Some(failure) = cancellation_failure(&cancellation, deadline_reached, None) {
                return Err(failure);
            }
            if !denied.is_empty() {
                return Err(review_execution_failure("LW_LLM_EGRESS_DENIED", None));
            }
        }
        let denied = tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                return Err(cancellation_failure(&cancellation, deadline_reached, None)
                    .unwrap_or_else(|| review_execution_failure(REVIEW_CANCELLED, None)));
            }
            result = self.classifier.classify(&request.rubric.path, request.rubric.content.as_bytes()) => {
                result.map_err(|_| review_execution_failure("LW_LLM_EGRESS_DENIED", None))?
            }
        };
        if let Some(failure) = cancellation_failure(&cancellation, deadline_reached, None) {
            return Err(failure);
        }
        if !denied.is_empty() {
            return Err(review_execution_failure("LW_LLM_EGRESS_DENIED", None));
        }
        let input = review_input(request)
            .map_err(|_| review_execution_failure("LW_LLM_REVIEW_INPUT_INVALID", None))?;
        let allowed_paths = request
            .files
            .iter()
            .map(|file| file.path.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        if let Some(failure) = cancellation_failure(&cancellation, deadline_reached, None) {
            return Err(failure);
        }
        let runtime = ClaudeCodeRuntime::new(request.policy.clone(), Arc::clone(&self.process))
            .map_err(|error| review_execution_failure(error.diagnostic_code(), None))?;
        // Keep the runtime future alive after cancellation is requested. It owns the provider
        // invocation and must let the process boundary perform its cancellation cleanup before
        // returning the cumulative usage observed by earlier repair envelopes.
        let result = runtime
            .review_with_usage(input, &allowed_paths, cancellation.clone())
            .await
            .map_err(|failure| map_review_failure(failure, &cancellation, deadline_reached));
        match result {
            Ok(execution) => {
                if let Some(failure) =
                    cancellation_failure(&cancellation, deadline_reached, Some(execution.usage))
                {
                    Err(failure)
                } else {
                    Ok(execution)
                }
            }
            Err(failure) => Err(failure),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct ReviewExecutionFailure {
    diagnostic_code: &'static str,
    usage: Option<LlmUsage>,
}

fn review_execution_failure(
    diagnostic_code: &'static str,
    usage: Option<LlmUsage>,
) -> ReviewExecutionFailure {
    ReviewExecutionFailure {
        diagnostic_code,
        usage,
    }
}

fn cancellation_failure(
    cancellation: &RunCancellation,
    deadline_reached: &AtomicBool,
    usage: Option<LlmUsage>,
) -> Option<ReviewExecutionFailure> {
    if deadline_reached.load(Ordering::Acquire) {
        Some(review_execution_failure(REVIEW_DEADLINE_EXCEEDED, usage))
    } else if cancellation.is_cancelled() {
        Some(review_execution_failure(REVIEW_CANCELLED, usage))
    } else {
        None
    }
}

fn map_review_failure(
    failure: ClaudeCodeReviewFailure,
    cancellation: &RunCancellation,
    deadline_reached: &AtomicBool,
) -> ReviewExecutionFailure {
    cancellation_failure(cancellation, deadline_reached, failure.usage)
        .unwrap_or_else(|| review_execution_failure(failure.error.diagnostic_code(), failure.usage))
}

fn validate_request_static(
    request: &InternalAgentLlmReviewRequest,
) -> Result<(), LlmReviewStoreError> {
    request
        .validate()
        .map_err(|_| LlmReviewStoreError::InvalidContract)?;
    validate_request_contents(request)
}

fn validate_request_deadline(
    request: &InternalAgentLlmReviewRequest,
    now: UtcTimestamp,
) -> Result<(), LlmReviewStoreError> {
    if request.deadline_at <= now {
        return Err(LlmReviewStoreError::InvalidContract);
    }
    Ok(())
}

fn validate_request_for_worker(
    request: &InternalAgentLlmReviewRequest,
) -> Result<(), LlmReviewStoreError> {
    request
        .validate()
        .map_err(|_| LlmReviewStoreError::InvalidContract)?;
    validate_request_contents(request)
}

fn validate_request_contents(
    request: &InternalAgentLlmReviewRequest,
) -> Result<(), LlmReviewStoreError> {
    let rubric_size = u64::try_from(request.rubric.content.len())
        .map_err(|_| LlmReviewStoreError::InvalidContract)?;
    if rubric_size != request.rubric.artifact.size_bytes {
        return Err(LlmReviewStoreError::InvalidContract);
    }
    if Sha256Digest::of_bytes(request.rubric.content.as_bytes()).to_string()
        != request.rubric.sha256
    {
        return Err(LlmReviewStoreError::InvalidContract);
    }
    for file in &request.files {
        if Sha256Digest::of_bytes(file.content.as_bytes()).to_string() != file.sha256 {
            return Err(LlmReviewStoreError::InvalidContract);
        }
    }
    Ok(())
}

fn review_input(request: &InternalAgentLlmReviewRequest) -> Result<Vec<u8>, LlmReviewStoreError> {
    serde_json::to_vec(&json!({
        "kind": "AgentLlmReviewInput",
        "taskRunId": request.task_run_id,
        "frozenSubmissionId": request.frozen_submission_id,
        "submissionArtifact": request.submission_artifact,
        "files": request.files,
        "rubric": request.rubric,
    }))
    .map_err(|_| LlmReviewStoreError::InvalidContract)
}

fn queued_receipt(
    task_run_id: TaskRunId,
    request_sha256: String,
) -> Result<InternalAgentLlmReviewReceipt, LlmReviewStoreError> {
    let receipt = InternalAgentLlmReviewReceipt {
        task_run_id,
        request_sha256,
        state: AgentLlmReviewState::Queued,
        review: None,
        usage: None,
        diagnostic_code: None,
        started_at: None,
        finished_at: None,
    };
    receipt
        .validate()
        .map_err(|_| LlmReviewStoreError::InvalidContract)?;
    Ok(receipt)
}

async fn update_receipt_state(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    task_run_id: TaskRunId,
    state: &str,
    receipt: &InternalAgentLlmReviewReceipt,
    diagnostic_code: Option<&str>,
    finished_at: Option<UtcTimestamp>,
) -> Result<(), LlmReviewStoreError> {
    let value = serde_json::to_value(receipt).map_err(|_| LlmReviewStoreError::InvalidContract)?;
    sqlx::query(
        "UPDATE agent.llm_review_runs SET state=$2,receipt_json=$3,diagnostic_code=$4, \
         finished_at=$5,updated_at=now() WHERE task_run_id=$1",
    )
    .bind(task_run_id.as_uuid())
    .bind(state)
    .bind(value)
    .bind(diagnostic_code)
    .bind(finished_at.map(UtcTimestamp::get))
    .execute(&mut **transaction)
    .await
    .map_err(|_| LlmReviewStoreError::PersistenceFailed)?;
    Ok(())
}

fn decode_receipt(value: Value) -> Result<InternalAgentLlmReviewReceipt, LlmReviewStoreError> {
    let receipt: InternalAgentLlmReviewReceipt =
        serde_json::from_value(value).map_err(|_| LlmReviewStoreError::InvalidContract)?;
    receipt
        .validate()
        .map_err(|_| LlmReviewStoreError::InvalidContract)?;
    Ok(receipt)
}

fn state_name(state: AgentLlmReviewState) -> &'static str {
    match state {
        AgentLlmReviewState::Queued => "queued",
        AgentLlmReviewState::Running => "running",
        AgentLlmReviewState::Cancelling => "cancelling",
        AgentLlmReviewState::Succeeded => "succeeded",
        AgentLlmReviewState::Failed => "failed",
        AgentLlmReviewState::Cancelled => "cancelled",
    }
}

fn parse_task_run_id(value: Uuid) -> Result<TaskRunId, LlmReviewStoreError> {
    value
        .to_string()
        .parse()
        .map_err(|_| LlmReviewStoreError::InvalidContract)
}

fn parse_project_id(value: Uuid) -> Result<ProjectId, LlmReviewStoreError> {
    value
        .to_string()
        .parse()
        .map_err(|_| LlmReviewStoreError::InvalidContract)
}

fn parse_course_id(value: Uuid) -> Result<CourseId, LlmReviewStoreError> {
    value
        .to_string()
        .parse()
        .map_err(|_| LlmReviewStoreError::InvalidContract)
}

fn timestamp() -> Result<UtcTimestamp, LlmReviewStoreError> {
    let value = OffsetDateTime::now_utc();
    let value = value
        .replace_nanosecond((value.nanosecond() / 1_000_000) * 1_000_000)
        .map_err(|_| LlmReviewStoreError::ClockInvalid)?;
    UtcTimestamp::from_utc(value).map_err(|_| LlmReviewStoreError::ClockInvalid)
}

/// Payload-free queue and worker failures.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum LlmReviewStoreError {
    #[error("LW_AUTH_COURSE_SCOPE_DENIED: review scope does not match")]
    CourseMismatch,
    #[error("LW_LLM_POLICY_REVISION_MISMATCH: review identity does not match")]
    IdentityMismatch,
    #[error("LW_IDEMPOTENCY_CONFLICT: review idempotency identity differs")]
    IdempotencyConflict,
    #[error("LW_AGENT_LLM_REVIEW_IN_PROGRESS: review mutation is still in progress")]
    InProgress,
    #[error("LW_AGENT_LLM_REVIEW_STATE_CONFLICT: review state changed")]
    StateConflict,
    #[error("LW_AGENT_LLM_REVIEW_NOT_FOUND: review does not exist")]
    NotFound,
    #[error("LW_AGENT_LLM_REVIEW_LEASE_LOST: review worker lease was lost")]
    LeaseLost,
    #[error("LW_AGENT_RUNTIME_IDENTITY_INVALID: review worker identity is invalid")]
    WorkerIdentityInvalid,
    #[error("LW_CONTRACT_DOCUMENT_INVALID: review contract is invalid")]
    InvalidContract,
    #[error("LW_AGENT_LLM_REVIEW_CLOCK_INVALID: review clock is invalid")]
    ClockInvalid,
    #[error("LW_AGENT_PERSISTENCE_FAILED: review persistence failed")]
    PersistenceFailed,
}

impl LlmReviewStoreError {
    /// Returns the stable diagnostic code used by the HTTP boundary.
    #[must_use]
    pub const fn diagnostic_code(self) -> &'static str {
        match self {
            Self::CourseMismatch => "LW_AUTH_COURSE_SCOPE_DENIED",
            Self::IdentityMismatch => "LW_LLM_POLICY_REVISION_MISMATCH",
            Self::IdempotencyConflict => "LW_IDEMPOTENCY_CONFLICT",
            Self::InProgress => "LW_AGENT_LLM_REVIEW_IN_PROGRESS",
            Self::StateConflict | Self::NotFound | Self::LeaseLost => {
                "LW_AGENT_LLM_REVIEW_STATE_CONFLICT"
            }
            Self::WorkerIdentityInvalid | Self::InvalidContract | Self::ClockInvalid => {
                "LW_CONTRACT_DOCUMENT_INVALID"
            }
            Self::PersistenceFailed => "LW_AGENT_PERSISTENCE_FAILED",
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    use async_trait::async_trait;
    use contracts::authoring::ProjectLlmEgressPolicy;
    use contracts::http::{AgentLlmReviewFile, AgentLlmReviewRubric};
    use contracts::{ArtifactId, ArtifactRef, FrozenSubmissionId, Revision};
    use serde_json::json;
    use tokio::time::Instant;

    use crate::claude_code::{
        ClaudeCodeCommand, ClaudeCodeProcessError, ClaudeCodeProcessOutput,
        EgressClassificationError,
    };

    struct DelayedClassifier {
        delay: Duration,
    }

    #[async_trait]
    impl EgressClassifier for DelayedClassifier {
        fn binding(&self) -> &'static str {
            "classifier-v1"
        }

        fn revision(&self) -> Revision {
            Revision::new(1).expect("test revision is valid")
        }

        async fn classify(
            &self,
            _path: &str,
            _bytes: &[u8],
        ) -> Result<BTreeSet<contracts::authoring::DeniedDataClass>, EgressClassificationError>
        {
            tokio::time::sleep(self.delay).await;
            Ok(BTreeSet::new())
        }
    }

    struct NeverProcess {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ClaudeCodeProcess for NeverProcess {
        async fn version(&self) -> Result<String, ClaudeCodeProcessError> {
            Err(ClaudeCodeProcessError::Unavailable)
        }

        async fn execute(
            &self,
            _command: ClaudeCodeCommand,
            _cancellation: RunCancellation,
        ) -> Result<ClaudeCodeProcessOutput, ClaudeCodeProcessError> {
            self.calls.fetch_add(1, Ordering::Release);
            Err(ClaudeCodeProcessError::Unavailable)
        }
    }

    fn policy() -> ProjectLlmEgressPolicy {
        serde_json::from_value(json!({
            "id": "01900000-0000-7000-8000-000000000101",
            "projectId": "01900000-0000-7000-8000-000000000102",
            "courseId": "01900000-0000-7000-8000-000000000103",
            "revision": 1,
            "binding": {
                "runtimeBinding": "claude-code-production",
                "model": "claude-sonnet-4-6-20260601",
                "claudeCodeVersion": "2.1.207",
                "maxInFlightPerWorker": 1
            },
            "budget": {
                "maxInputTokens": 1000,
                "maxOutputTokens": 1000,
                "maxRequests": 2,
                "maxCostMicrousd": 1_000_000,
                "timeoutMilliseconds": 1000,
                "maxTransientRetries": 0,
                "maxSchemaRepairs": 0
            },
            "deniedDataClasses": [
                "secret",
                "token",
                "private_key",
                "personally_identifiable_information",
                "unallowlisted_student_submission"
            ],
            "studentContentMode": "manifest_allowlist_only",
            "activatedAt": "2026-09-08T00:00:00.000Z"
        }))
        .expect("test policy is valid")
    }

    fn request(deadline_at: UtcTimestamp) -> InternalAgentLlmReviewRequest {
        let project_id = policy().project_id;
        let course_id = policy().course_id;
        let submission = "submission";
        let rubric = "rubric";
        InternalAgentLlmReviewRequest {
            task_run_id: TaskRunId::new(),
            project_id,
            course_id,
            frozen_submission_id: FrozenSubmissionId::new(),
            submission_artifact: ArtifactRef {
                artifact_id: ArtifactId::new(),
                store_binding: "minio-primary".to_owned(),
                object_version: "version-1".to_owned(),
                size_bytes: submission.len() as u64,
                media_type: "text/plain".to_owned(),
            },
            policy: policy(),
            files: vec![AgentLlmReviewFile {
                path: "submission.md".to_owned(),
                sha256: Sha256Digest::of_bytes(submission.as_bytes()).to_string(),
                content: submission.to_owned(),
            }],
            rubric: AgentLlmReviewRubric {
                artifact: ArtifactRef {
                    artifact_id: ArtifactId::new(),
                    store_binding: "minio-primary".to_owned(),
                    object_version: "version-1".to_owned(),
                    size_bytes: rubric.len() as u64,
                    media_type: "text/plain".to_owned(),
                },
                path: "rubric.md".to_owned(),
                sha256: Sha256Digest::of_bytes(rubric.as_bytes()).to_string(),
                content: rubric.to_owned(),
            },
            deadline_at,
        }
    }

    fn lease(request: InternalAgentLlmReviewRequest) -> LlmReviewLease {
        LlmReviewLease {
            task_run_id: request.task_run_id,
            request,
            request_sha256: "00".repeat(32),
            attempt: 1,
            worker_id: "test-worker".to_owned(),
            lease_token: Uuid::now_v7(),
        }
    }

    fn deadline_after(milliseconds: i64) -> UtcTimestamp {
        let now = OffsetDateTime::now_utc();
        let now = now
            .replace_nanosecond((now.nanosecond() / 1_000_000) * 1_000_000)
            .expect("test clock timestamp is valid");
        UtcTimestamp::from_utc(now + time::Duration::milliseconds(milliseconds))
            .expect("test deadline is valid")
    }

    #[tokio::test]
    async fn deadline_cancels_classification_before_provider_entry() {
        let request = request(deadline_after(20));
        let calls = Arc::new(AtomicUsize::new(0));
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://invalid")
            .expect("test pool URL is valid");
        let worker = LlmReviewWorker {
            store: LlmReviewStore::new(pool),
            classifier: Arc::new(DelayedClassifier {
                delay: Duration::from_millis(200),
            }),
            process: Arc::new(NeverProcess {
                calls: Arc::clone(&calls),
            }),
            worker_id: "test-worker".to_owned(),
            lease_duration: Duration::from_secs(1),
            poll_interval: Duration::from_millis(10),
        };
        let started = Instant::now();
        let failure = worker
            .execute(&lease(request), RunCancellation::new())
            .await
            .expect_err("deadline must stop a slow classifier");
        assert_eq!(failure.diagnostic_code, REVIEW_DEADLINE_EXCEEDED);
        assert!(started.elapsed() < Duration::from_millis(150));
        assert_eq!(calls.load(Ordering::Acquire), 0);
    }
}
