//! PostgreSQL-authoritative public freeze acceptance and coordinator queue.
#![allow(
    missing_docs,
    clippy::missing_errors_doc,
    reason = "the internal queue is documented by ADR 0010 and stable diagnostics"
)]

use std::str::FromStr;

use contracts::{
    ActorId, CourseId, EnvironmentId, EventId, FrozenSubmissionId, OperationId, Revision, Sequence,
    Sha256Digest, UtcTimestamp,
    events::{
        CloudEvent, EVENT_CONTRACTS, EventContract, SPEC_VERSION, SubmissionFreezeRequested,
        subjects,
    },
    http::OperationAccepted,
    submission::SubmissionManifest,
};
use persistence_sqlx::{Domain, OutboxStore};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Row, Transaction};

/// Persisted command containing no runtime credentials or student file contents.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubmissionFreezeCommand {
    pub frozen_submission_id: FrozenSubmissionId,
    pub operation_id: OperationId,
    pub course_id: CourseId,
    pub environment_id: EnvironmentId,
    pub actor_id: ActorId,
    pub environment_revision: Revision,
    pub manifest_revision: Revision,
    pub manifest: SubmissionManifest,
    pub idempotency_key: String,
    pub trace_id: String,
    pub requested_at: UtcTimestamp,
}

impl SubmissionFreezeCommand {
    pub fn validate(&self) -> Result<(), FreezeCommandStoreError> {
        self.manifest
            .validate()
            .map_err(|_| FreezeCommandStoreError::ContractInvalid)?;
        if self.idempotency_key.trim().is_empty()
            || self.idempotency_key.len() > 512
            || self.idempotency_key.chars().any(char::is_control)
            || self.trace_id.trim().is_empty()
            || self.trace_id.len() > 128
            || self.trace_id.chars().any(char::is_control)
        {
            return Err(FreezeCommandStoreError::ContractInvalid);
        }
        Ok(())
    }
}

/// Idempotent public acceptance result.
#[derive(Clone, Debug)]
pub struct FreezeCommandAccept {
    pub accepted: OperationAccepted,
    pub frozen_submission_id: FrozenSubmissionId,
    pub replay: bool,
}

/// Evaluation-owned durable command store.
#[derive(Clone)]
pub struct PgFreezeCommandStore {
    pool: PgPool,
}

impl PgFreezeCommandStore {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn authority_now(&self) -> Result<UtcTimestamp, FreezeCommandStoreError> {
        let value: time::OffsetDateTime =
            sqlx::query_scalar("SELECT date_trunc('milliseconds', clock_timestamp())")
                .fetch_one(&self.pool)
                .await?;
        UtcTimestamp::from_utc(value).map_err(|_| FreezeCommandStoreError::ContractInvalid)
    }

    /// Atomically claims the oldest queued command for one deterministic Kubernetes Job.
    pub async fn claim_next(
        &self,
    ) -> Result<Option<SubmissionFreezeCommand>, FreezeCommandStoreError> {
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query(
            "SELECT frozen_submission_id,contract FROM evaluation.submission_freeze_commands \
             WHERE state='queued' ORDER BY created_at,frozen_submission_id FOR UPDATE SKIP LOCKED LIMIT 1",
        )
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(row) = row else {
            transaction.commit().await?;
            return Ok(None);
        };
        let frozen_submission_id: uuid::Uuid = row.try_get("frozen_submission_id")?;
        let command: SubmissionFreezeCommand = serde_json::from_value(row.try_get("contract")?)?;
        command.validate()?;
        if command.frozen_submission_id.as_uuid() != frozen_submission_id {
            return Err(FreezeCommandStoreError::DatabaseIdentityInvalid);
        }
        let job_name = format!(
            "lw-freeze-{}",
            &frozen_submission_id.simple().to_string()[..20]
        );
        let updated = sqlx::query(
            "UPDATE evaluation.submission_freeze_commands SET state='running',job_name=$2,updated_at=clock_timestamp() \
             WHERE frozen_submission_id=$1 AND state='queued'",
        )
        .bind(frozen_submission_id)
        .bind(job_name)
        .execute(&mut *transaction)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(FreezeCommandStoreError::FenceConflict);
        }
        transaction.commit().await?;
        Ok(Some(command))
    }

    /// Loads running commands so a restarted coordinator resumes observation and cleanup.
    pub async fn running(
        &self,
        limit: i64,
    ) -> Result<Vec<SubmissionFreezeCommand>, FreezeCommandStoreError> {
        if !(1..=64).contains(&limit) {
            return Err(FreezeCommandStoreError::ContractInvalid);
        }
        let rows = sqlx::query(
            "SELECT contract FROM evaluation.submission_freeze_commands WHERE state='running' \
             ORDER BY updated_at,frozen_submission_id LIMIT $1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                let command: SubmissionFreezeCommand =
                    serde_json::from_value(row.try_get("contract")?)?;
                command.validate()?;
                Ok(command)
            })
            .collect()
    }

    /// Loads failed commands whose Kubernetes residue still requires verified cleanup.
    pub async fn cleanup_pending(
        &self,
        limit: i64,
    ) -> Result<Vec<SubmissionFreezeCommand>, FreezeCommandStoreError> {
        if !(1..=64).contains(&limit) {
            return Err(FreezeCommandStoreError::ContractInvalid);
        }
        let rows = sqlx::query(
            "SELECT contract FROM evaluation.submission_freeze_commands \
             WHERE state='failed' AND cleanup_verified=false \
             ORDER BY updated_at,frozen_submission_id LIMIT $1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                let command: SubmissionFreezeCommand =
                    serde_json::from_value(row.try_get("contract")?)?;
                command.validate()?;
                Ok(command)
            })
            .collect()
    }

    /// Completes a command only after the immutable result exists and all Job residue is gone.
    pub async fn mark_completed(
        &self,
        frozen_submission_id: FrozenSubmissionId,
    ) -> Result<(), FreezeCommandStoreError> {
        let result_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM evaluation.frozen_submissions WHERE frozen_submission_id=$1)",
        )
        .bind(frozen_submission_id.as_uuid())
        .fetch_one(&self.pool)
        .await?;
        if !result_exists {
            return Err(FreezeCommandStoreError::ResultMissing);
        }
        terminal_update(&self.pool, frozen_submission_id, "completed", None).await
    }

    /// Records one stable terminal diagnostic after residue cleanup is verified.
    pub async fn mark_failed(
        &self,
        frozen_submission_id: FrozenSubmissionId,
        diagnostic: &str,
    ) -> Result<(), FreezeCommandStoreError> {
        contracts::DiagnosticCode::parse(diagnostic)
            .map_err(|_| FreezeCommandStoreError::ContractInvalid)?;
        terminal_update(&self.pool, frozen_submission_id, "failed", Some(diagnostic)).await
    }

    /// Fences a failed worker before asynchronous Kubernetes cleanup begins.
    pub async fn mark_failed_pending_cleanup(
        &self,
        frozen_submission_id: FrozenSubmissionId,
        diagnostic: &str,
    ) -> Result<(), FreezeCommandStoreError> {
        contracts::DiagnosticCode::parse(diagnostic)
            .map_err(|_| FreezeCommandStoreError::ContractInvalid)?;
        let updated = sqlx::query(
            "UPDATE evaluation.submission_freeze_commands \
             SET state='failed',diagnostic_code=$2,cleanup_verified=false,\
             completed_at=clock_timestamp(),updated_at=clock_timestamp() \
             WHERE frozen_submission_id=$1 AND state='running'",
        )
        .bind(frozen_submission_id.as_uuid())
        .bind(diagnostic)
        .execute(&self.pool)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(FreezeCommandStoreError::FenceConflict);
        }
        Ok(())
    }

    /// Marks cleanup complete only after every owned Kubernetes object is absent.
    pub async fn mark_cleanup_verified(
        &self,
        frozen_submission_id: FrozenSubmissionId,
    ) -> Result<(), FreezeCommandStoreError> {
        let updated = sqlx::query(
            "UPDATE evaluation.submission_freeze_commands \
             SET cleanup_verified=true,updated_at=clock_timestamp() \
             WHERE frozen_submission_id=$1 AND state='failed' AND cleanup_verified=false",
        )
        .bind(frozen_submission_id.as_uuid())
        .execute(&self.pool)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(FreezeCommandStoreError::FenceConflict);
        }
        Ok(())
    }

    pub async fn accept(
        &self,
        command: &SubmissionFreezeCommand,
    ) -> Result<FreezeCommandAccept, FreezeCommandStoreError> {
        command.validate()?;
        let request_sha256 = request_hash(command)?;
        let manifest_sha256 = Sha256Digest::of_canonical(&command.manifest)
            .map_err(|_| FreezeCommandStoreError::ContractInvalid)?;
        let mut transaction = self.pool.begin().await?;
        let lock_identity = format!("{}:{}", command.course_id, command.idempotency_key);
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 540001))")
            .bind(lock_identity)
            .fetch_one(&mut *transaction)
            .await?;
        if let Some(row) = sqlx::query(
            "SELECT frozen_submission_id,operation_id,request_sha256 FROM evaluation.submission_freeze_commands \
             WHERE course_id=$1 AND idempotency_key=$2",
        )
        .bind(command.course_id.as_uuid())
        .bind(&command.idempotency_key)
        .fetch_optional(&mut *transaction)
        .await?
        {
            if row.try_get::<String, _>("request_sha256")? != request_sha256.to_string() {
                return Err(FreezeCommandStoreError::IdempotencyConflict);
            }
            let frozen_submission_id = parse_id(row.try_get("frozen_submission_id")?)?;
            let operation_id = parse_id(row.try_get("operation_id")?)?;
            transaction.commit().await?;
            return Ok(FreezeCommandAccept {
                accepted: accepted(operation_id, frozen_submission_id)?,
                frozen_submission_id,
                replay: true,
            });
        }
        sqlx::query(
            "INSERT INTO evaluation.submission_freeze_commands \
             (frozen_submission_id,operation_id,course_id,environment_id,actor_id,idempotency_key,request_sha256,manifest_sha256,state,contract) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,'queued',$9)",
        )
        .bind(command.frozen_submission_id.as_uuid())
        .bind(command.operation_id.as_uuid())
        .bind(command.course_id.as_uuid())
        .bind(command.environment_id.as_uuid())
        .bind(command.actor_id.as_uuid())
        .bind(&command.idempotency_key)
        .bind(request_sha256.to_string())
        .bind(manifest_sha256.to_string())
        .bind(serde_json::to_value(command)?)
        .execute(&mut *transaction)
        .await?;
        enqueue_requested(&mut transaction, command, manifest_sha256).await?;
        transaction.commit().await?;
        Ok(FreezeCommandAccept {
            accepted: accepted(command.operation_id, command.frozen_submission_id)?,
            frozen_submission_id: command.frozen_submission_id,
            replay: false,
        })
    }
}

async fn terminal_update(
    pool: &PgPool,
    frozen_submission_id: FrozenSubmissionId,
    state: &'static str,
    diagnostic: Option<&str>,
) -> Result<(), FreezeCommandStoreError> {
    let updated = sqlx::query(
        "UPDATE evaluation.submission_freeze_commands SET state=$2,diagnostic_code=$3,cleanup_verified=true,\
         completed_at=clock_timestamp(),updated_at=clock_timestamp() WHERE frozen_submission_id=$1 AND state='running'",
    )
    .bind(frozen_submission_id.as_uuid())
    .bind(state)
    .bind(diagnostic)
    .execute(pool)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(FreezeCommandStoreError::FenceConflict);
    }
    Ok(())
}

async fn enqueue_requested(
    transaction: &mut Transaction<'_, Postgres>,
    command: &SubmissionFreezeCommand,
    manifest_sha256: Sha256Digest,
) -> Result<(), FreezeCommandStoreError> {
    let contract = event_contract(subjects::SUBMISSION_FREEZE_REQUESTED)?;
    let event_id = EventId::new();
    let envelope = CloudEvent {
        specversion: SPEC_VERSION.to_owned(),
        id: event_id,
        source: contract.source().to_owned(),
        event_type: contract.event_type.to_owned(),
        subject: contract.subject.to_owned(),
        time: command.requested_at,
        datacontenttype: "application/json".to_owned(),
        dataschema: contract.data_schema(),
        course_id: command.course_id,
        aggregate_revision: Revision::new(1)
            .map_err(|_| FreezeCommandStoreError::ContractInvalid)?,
        aggregate_sequence: Sequence(1),
        trace_id: command.trace_id.clone(),
        data: SubmissionFreezeRequested {
            frozen_submission_id: command.frozen_submission_id,
            environment_id: command.environment_id,
            manifest_sha256,
            frozen_by: command.actor_id,
        },
    };
    envelope.validate(contract)?;
    let payload = serde_json::to_value(&envelope)?;
    let payload_sha256 = Sha256Digest::of_canonical(&envelope)
        .map_err(|_| FreezeCommandStoreError::ContractInvalid)?;
    OutboxStore::enqueue(
        transaction,
        Domain::Evaluation,
        event_id.as_uuid(),
        contract.subject,
        contract.event_type,
        command.frozen_submission_id.as_uuid(),
        1,
        &payload,
        payload_sha256,
    )
    .await?;
    Ok(())
}

fn request_hash(
    command: &SubmissionFreezeCommand,
) -> Result<Sha256Digest, FreezeCommandStoreError> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Identity<'a> {
        course_id: CourseId,
        environment_id: EnvironmentId,
        actor_id: ActorId,
        environment_revision: Revision,
        manifest_revision: Revision,
        manifest: &'a SubmissionManifest,
    }
    Sha256Digest::of_canonical(&Identity {
        course_id: command.course_id,
        environment_id: command.environment_id,
        actor_id: command.actor_id,
        environment_revision: command.environment_revision,
        manifest_revision: command.manifest_revision,
        manifest: &command.manifest,
    })
    .map_err(|_| FreezeCommandStoreError::ContractInvalid)
}

fn accepted(
    operation_id: OperationId,
    submission_id: FrozenSubmissionId,
) -> Result<OperationAccepted, FreezeCommandStoreError> {
    Ok(OperationAccepted {
        operation_id,
        revision: Revision::new(1).map_err(|_| FreezeCommandStoreError::ContractInvalid)?,
        status_url: format!("/api/v1/frozen-submissions/{submission_id}"),
    })
}

fn event_contract(subject: &str) -> Result<EventContract, FreezeCommandStoreError> {
    EVENT_CONTRACTS
        .iter()
        .copied()
        .find(|contract| contract.subject == subject)
        .ok_or(FreezeCommandStoreError::ContractInvalid)
}

fn parse_id<T: FromStr<Err = uuid::Error>>(
    value: uuid::Uuid,
) -> Result<T, FreezeCommandStoreError> {
    if value.get_version_num() != 7 {
        return Err(FreezeCommandStoreError::DatabaseIdentityInvalid);
    }
    value
        .to_string()
        .parse()
        .map_err(|_| FreezeCommandStoreError::DatabaseIdentityInvalid)
}

/// Stable acceptance and persistence failures.
#[derive(Debug, thiserror::Error)]
pub enum FreezeCommandStoreError {
    #[error("LW_COLLECT_CONTRACT_INVALID")]
    ContractInvalid,
    #[error("LW_COLLECT_IDEMPOTENCY_CONFLICT")]
    IdempotencyConflict,
    #[error("LW_COLLECT_DATABASE_IDENTITY_INVALID")]
    DatabaseIdentityInvalid,
    #[error("LW_COLLECT_DATABASE_FAILED")]
    Database(#[from] sqlx::Error),
    #[error("LW_COLLECT_CONTRACT_INVALID")]
    Serialization(#[from] serde_json::Error),
    #[error("LW_COLLECT_EVENT_INVALID")]
    Event(#[from] contracts::events::EventError),
    #[error("LW_COLLECT_DATABASE_FAILED")]
    Persistence(#[from] persistence_sqlx::PersistenceError),
    #[error("LW_COLLECT_FENCE_CONFLICT")]
    FenceConflict,
    #[error("LW_COLLECT_RESULT_MISSING")]
    ResultMissing,
}
