//! `PostgreSQL` authority for idempotent submission-freeze attempts.
#![allow(
    missing_docs,
    reason = "the versioned contracts and stable diagnostics define the external surface"
)]

use std::str::FromStr;
use std::time::Duration;

use contracts::events::{CloudEvent, EVENT_CONTRACTS, SPEC_VERSION, SubmissionFrozen, subjects};
use contracts::submission::FrozenSubmission;
use contracts::{
    CourseId, EnvironmentId, EventId, FrozenSubmissionId, Revision, Sequence, Sha256Digest,
    UtcTimestamp,
};
use persistence_sqlx::{Domain, OutboxStore};
use serde_json::Value;
use sqlx::{PgPool, Row};
use time::OffsetDateTime;
use uuid::Uuid;

/// Durable outcome of reserving one idempotent freeze request.
#[derive(Clone, Debug, PartialEq)]
pub enum BeginFreeze {
    /// This worker owns the fenced attempt.
    Acquired(FreezeLease),
    /// The exact request completed previously.
    Replay(Box<FrozenSubmission>),
    /// The same idempotency key was used for a different request.
    Conflict,
    /// Another non-expired worker owns the exact request.
    InProgress,
}

/// Fenced ownership of one append-only freeze attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FreezeLease {
    pub frozen_submission_id: FrozenSubmissionId,
    pub course_id: CourseId,
    pub environment_id: EnvironmentId,
    pub idempotency_key: String,
    pub request_sha256: Sha256Digest,
    pub source_identity_sha256: Sha256Digest,
    pub attempt: u32,
    pub authority_now: UtcTimestamp,
    worker_id: String,
    lease_token: Uuid,
}

/// Evaluation-owned durable freeze store.
#[derive(Clone, Debug)]
pub struct PgFreezeStore {
    pool: PgPool,
}

impl PgFreezeStore {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Reads the database authority clock.
    ///
    /// # Errors
    ///
    /// Returns a stable database or clock diagnostic.
    pub async fn authority_now(&self) -> Result<UtcTimestamp, FreezeStoreError> {
        let value: OffsetDateTime =
            sqlx::query_scalar("SELECT date_trunc('milliseconds',clock_timestamp())")
                .fetch_one(&self.pool)
                .await?;
        UtcTimestamp::from_utc(value).map_err(|_| FreezeStoreError::ClockInvalid)
    }

    /// Acquires a new attempt, reclaims an expired attempt, or returns the durable replay.
    ///
    /// # Errors
    ///
    /// Returns a stable validation, persistence, or contract diagnostic.
    #[allow(clippy::too_many_arguments)]
    #[allow(
        clippy::too_many_lines,
        reason = "reservation and expired-attempt fencing must remain in one visible transaction"
    )]
    pub async fn begin(
        &self,
        course_id: CourseId,
        environment_id: EnvironmentId,
        idempotency_key: &str,
        request_sha256: Sha256Digest,
        source_identity_sha256: Sha256Digest,
        worker_id: &str,
        lease_ttl: Duration,
    ) -> Result<BeginFreeze, FreezeStoreError> {
        validate_token(idempotency_key)?;
        validate_token(worker_id)?;
        let lease_seconds = i64::try_from(lease_ttl.as_secs())
            .ok()
            .filter(|seconds| (30..=1_800).contains(seconds))
            .ok_or(FreezeStoreError::LeaseInvalid)?;
        let mut transaction = self.pool.begin().await?;
        let authority_now: OffsetDateTime =
            sqlx::query_scalar("SELECT date_trunc('milliseconds',clock_timestamp())")
                .fetch_one(&mut *transaction)
                .await?;
        let lease_expires_at = authority_now + time::Duration::seconds(lease_seconds);
        let frozen_submission_id = FrozenSubmissionId::new();
        let inserted = sqlx::query(
            "INSERT INTO evaluation.submission_freeze_requests \
             (frozen_submission_id,course_id,environment_id,idempotency_key,request_sha256,source_identity_sha256,state,current_attempt) \
             VALUES ($1,$2,$3,$4,$5,$6,'active',1) ON CONFLICT (course_id,idempotency_key) DO NOTHING",
        )
        .bind(frozen_submission_id.as_uuid())
        .bind(course_id.as_uuid())
        .bind(environment_id.as_uuid())
        .bind(idempotency_key)
        .bind(request_sha256.to_string())
        .bind(source_identity_sha256.to_string())
        .execute(&mut *transaction)
        .await?;
        if inserted.rows_affected() == 1 {
            let lease_token = Uuid::now_v7();
            insert_attempt(
                &mut transaction,
                frozen_submission_id,
                1,
                worker_id,
                lease_token,
                lease_expires_at,
            )
            .await?;
            transaction.commit().await?;
            return Ok(BeginFreeze::Acquired(FreezeLease {
                frozen_submission_id,
                course_id,
                environment_id,
                idempotency_key: idempotency_key.to_owned(),
                request_sha256,
                source_identity_sha256,
                attempt: 1,
                authority_now: timestamp(authority_now)?,
                worker_id: worker_id.to_owned(),
                lease_token,
            }));
        }

        let request = sqlx::query(
            "SELECT frozen_submission_id,environment_id,request_sha256,source_identity_sha256,state,current_attempt,contract \
             FROM evaluation.submission_freeze_requests WHERE course_id=$1 AND idempotency_key=$2 FOR UPDATE",
        )
        .bind(course_id.as_uuid())
        .bind(idempotency_key)
        .fetch_one(&mut *transaction)
        .await?;
        if request.try_get::<Uuid, _>("environment_id")? != environment_id.as_uuid()
            || request.try_get::<String, _>("request_sha256")? != request_sha256.to_string()
            || request.try_get::<String, _>("source_identity_sha256")?
                != source_identity_sha256.to_string()
        {
            transaction.rollback().await?;
            return Ok(BeginFreeze::Conflict);
        }
        let persisted_id =
            parse_id::<FrozenSubmissionId>(request.try_get::<Uuid, _>("frozen_submission_id")?)?;
        let state: String = request.try_get("state")?;
        if state == "completed" {
            let contract: Value = request.try_get("contract")?;
            let submission: FrozenSubmission =
                serde_json::from_value(contract).map_err(|_| FreezeStoreError::ContractInvalid)?;
            submission
                .validate()
                .map_err(|_| FreezeStoreError::ContractInvalid)?;
            if submission.id != persisted_id
                || submission.course_id != course_id
                || submission.environment.environment_id != environment_id
            {
                return Err(FreezeStoreError::IdentityMismatch);
            }
            transaction.commit().await?;
            return Ok(BeginFreeze::Replay(Box::new(submission)));
        }
        let current_attempt_i32: i32 = request.try_get("current_attempt")?;
        let current_attempt =
            u32::try_from(current_attempt_i32).map_err(|_| FreezeStoreError::ContractInvalid)?;
        if state == "active" {
            let active = sqlx::query(
                "SELECT lease_expires_at FROM evaluation.submission_freeze_attempts \
                 WHERE frozen_submission_id=$1 AND attempt=$2 FOR UPDATE",
            )
            .bind(persisted_id.as_uuid())
            .bind(current_attempt_i32)
            .fetch_one(&mut *transaction)
            .await?;
            let expires_at: OffsetDateTime = active.try_get("lease_expires_at")?;
            if expires_at > authority_now {
                transaction.commit().await?;
                return Ok(BeginFreeze::InProgress);
            }
            sqlx::query(
                "UPDATE evaluation.submission_freeze_attempts SET state='failed',worker_id=NULL,lease_token=NULL,lease_expires_at=NULL, \
                 diagnostic_code='LW_COLLECT_LEASE_EXPIRED',cleanup_verified=false,completed_at=$3,updated_at=$3 \
                 WHERE frozen_submission_id=$1 AND attempt=$2 AND state IN ('reserved','preflighting','uploading')",
            )
            .bind(persisted_id.as_uuid())
            .bind(current_attempt_i32)
            .bind(authority_now)
            .execute(&mut *transaction)
            .await?;
        } else if state != "retryable" {
            return Err(FreezeStoreError::ContractInvalid);
        }
        let attempt = current_attempt
            .checked_add(1)
            .ok_or(FreezeStoreError::AttemptOverflow)?;
        let lease_token = Uuid::now_v7();
        sqlx::query(
            "UPDATE evaluation.submission_freeze_requests SET state='active',current_attempt=$2,updated_at=$3 \
             WHERE frozen_submission_id=$1 AND state IN ('active','retryable')",
        )
        .bind(persisted_id.as_uuid())
        .bind(i32::try_from(attempt).map_err(|_| FreezeStoreError::AttemptOverflow)?)
        .bind(authority_now)
        .execute(&mut *transaction)
        .await?;
        insert_attempt(
            &mut transaction,
            persisted_id,
            attempt,
            worker_id,
            lease_token,
            lease_expires_at,
        )
        .await?;
        transaction.commit().await?;
        Ok(BeginFreeze::Acquired(FreezeLease {
            frozen_submission_id: persisted_id,
            course_id,
            environment_id,
            idempotency_key: idempotency_key.to_owned(),
            request_sha256,
            source_identity_sha256,
            attempt,
            authority_now: timestamp(authority_now)?,
            worker_id: worker_id.to_owned(),
            lease_token,
        }))
    }

    /// Moves an owned reservation into the preflight phase.
    ///
    /// # Errors
    ///
    /// Returns a stable database or lost-fence diagnostic.
    pub async fn mark_preflighting(&self, lease: &FreezeLease) -> Result<(), FreezeStoreError> {
        self.transition(lease, "reserved", "preflighting", None)
            .await
    }

    /// Records the exact object key before starting the immutable upload.
    ///
    /// # Errors
    ///
    /// Returns a stable key, database, or lost-fence diagnostic.
    pub async fn mark_uploading(
        &self,
        lease: &FreezeLease,
        object_key: &str,
    ) -> Result<(), FreezeStoreError> {
        validate_object_key(object_key)?;
        self.transition(lease, "preflighting", "uploading", Some(object_key))
            .await
    }

    /// Atomically writes the authoritative result and matching v2 Outbox event.
    ///
    /// # Errors
    ///
    /// Returns a stable identity, contract, database, or lost-fence diagnostic.
    pub async fn complete(
        &self,
        lease: &FreezeLease,
        object_key: &str,
        submission: &FrozenSubmission,
        trace_id: &str,
    ) -> Result<(), FreezeStoreError> {
        validate_object_key(object_key)?;
        validate_trace_id(trace_id)?;
        submission
            .validate()
            .map_err(|_| FreezeStoreError::ContractInvalid)?;
        if submission.id != lease.frozen_submission_id
            || submission.course_id != lease.course_id
            || submission.environment.environment_id != lease.environment_id
            || submission.attempt != lease.attempt
            || submission.object.object_version.trim().is_empty()
        {
            return Err(FreezeStoreError::IdentityMismatch);
        }
        let contract =
            serde_json::to_value(submission).map_err(|_| FreezeStoreError::ContractInvalid)?;
        let mut transaction = self.pool.begin().await?;
        let attempt =
            i32::try_from(lease.attempt).map_err(|_| FreezeStoreError::AttemptOverflow)?;
        let fenced = sqlx::query(
            "UPDATE evaluation.submission_freeze_attempts SET state='completed',worker_id=NULL,lease_token=NULL,lease_expires_at=NULL, \
             object_version=$5,object_sha256=$6,cleanup_verified=true,completed_at=clock_timestamp(),updated_at=clock_timestamp() \
             WHERE frozen_submission_id=$1 AND attempt=$2 AND worker_id=$3 AND lease_token=$4 AND state='uploading' \
             AND object_key=$7 AND lease_expires_at > clock_timestamp()",
        )
        .bind(lease.frozen_submission_id.as_uuid())
        .bind(attempt)
        .bind(&lease.worker_id)
        .bind(lease.lease_token)
        .bind(&submission.object.object_version)
        .bind(submission.object.sha256.to_string())
        .bind(object_key)
        .execute(&mut *transaction)
        .await?;
        if fenced.rows_affected() != 1 {
            transaction.rollback().await?;
            return Err(FreezeStoreError::FenceLost);
        }
        sqlx::query(
            "INSERT INTO evaluation.frozen_submissions \
             (frozen_submission_id,course_id,environment_id,manifest_sha256,content_sha256,schema_version,tool_version,contract,frozen_at, \
              idempotency_key,source_identity_sha256,object_key,object_version) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)",
        )
        .bind(submission.id.as_uuid())
        .bind(submission.course_id.as_uuid())
        .bind(submission.environment.environment_id.as_uuid())
        .bind(submission.manifest_sha256.to_string())
        .bind(submission.object.sha256.to_string())
        .bind("evaluation.labweaver.io/frozen-submission/v1")
        .bind(env!("CARGO_PKG_VERSION"))
        .bind(&contract)
        .bind(submission.frozen_at.get())
        .bind(&lease.idempotency_key)
        .bind(lease.source_identity_sha256.to_string())
        .bind(object_key)
        .bind(&submission.object.object_version)
        .execute(&mut *transaction)
        .await
        .map_err(map_database_identity)?;
        let request_updated = sqlx::query(
            "UPDATE evaluation.submission_freeze_requests SET state='completed',contract=$2,completed_at=clock_timestamp(),updated_at=clock_timestamp() \
             WHERE frozen_submission_id=$1 AND state='active' AND current_attempt=$3",
        )
        .bind(lease.frozen_submission_id.as_uuid())
        .bind(&contract)
        .bind(attempt)
        .execute(&mut *transaction)
        .await?;
        if request_updated.rows_affected() != 1 {
            transaction.rollback().await?;
            return Err(FreezeStoreError::FenceLost);
        }
        enqueue_frozen_event(&mut transaction, lease, submission, trace_id).await?;
        transaction.commit().await?;
        Ok(())
    }

    /// Retains a failed attempt with a payload-free diagnostic and no publishable contract.
    ///
    /// # Errors
    ///
    /// Returns a stable diagnostic-validation, database, or lost-fence diagnostic.
    pub async fn fail(
        &self,
        lease: &FreezeLease,
        diagnostic_code: &'static str,
        cleanup_verified: bool,
    ) -> Result<(), FreezeStoreError> {
        validate_diagnostic(diagnostic_code)?;
        let mut transaction = self.pool.begin().await?;
        let attempt =
            i32::try_from(lease.attempt).map_err(|_| FreezeStoreError::AttemptOverflow)?;
        let result = sqlx::query(
            "UPDATE evaluation.submission_freeze_attempts SET state='failed',worker_id=NULL,lease_token=NULL,lease_expires_at=NULL, \
             diagnostic_code=$5,cleanup_verified=$6,completed_at=clock_timestamp(),updated_at=clock_timestamp() \
             WHERE frozen_submission_id=$1 AND attempt=$2 AND worker_id=$3 AND lease_token=$4 \
             AND state IN ('reserved','preflighting','uploading')",
        )
        .bind(lease.frozen_submission_id.as_uuid())
        .bind(attempt)
        .bind(&lease.worker_id)
        .bind(lease.lease_token)
        .bind(diagnostic_code)
        .bind(cleanup_verified)
        .execute(&mut *transaction)
        .await?;
        if result.rows_affected() != 1 {
            transaction.rollback().await?;
            return Err(FreezeStoreError::FenceLost);
        }
        sqlx::query(
            "UPDATE evaluation.submission_freeze_requests SET state='retryable',updated_at=clock_timestamp() \
             WHERE frozen_submission_id=$1 AND state='active' AND current_attempt=$2",
        )
        .bind(lease.frozen_submission_id.as_uuid())
        .bind(attempt)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    async fn transition(
        &self,
        lease: &FreezeLease,
        from: &str,
        to: &str,
        object_key: Option<&str>,
    ) -> Result<(), FreezeStoreError> {
        let result = sqlx::query(
            "UPDATE evaluation.submission_freeze_attempts SET state=$6,object_key=COALESCE($7,object_key),updated_at=clock_timestamp() \
             WHERE frozen_submission_id=$1 AND attempt=$2 AND worker_id=$3 AND lease_token=$4 \
             AND state=$5 AND lease_expires_at > clock_timestamp()",
        )
        .bind(lease.frozen_submission_id.as_uuid())
        .bind(i32::try_from(lease.attempt).map_err(|_| FreezeStoreError::AttemptOverflow)?)
        .bind(&lease.worker_id)
        .bind(lease.lease_token)
        .bind(from)
        .bind(to)
        .bind(object_key)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() != 1 {
            return Err(FreezeStoreError::FenceLost);
        }
        Ok(())
    }
}

async fn insert_attempt(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    frozen_submission_id: FrozenSubmissionId,
    attempt: u32,
    worker_id: &str,
    lease_token: Uuid,
    lease_expires_at: OffsetDateTime,
) -> Result<(), FreezeStoreError> {
    sqlx::query(
        "INSERT INTO evaluation.submission_freeze_attempts \
         (frozen_submission_id,attempt,state,worker_id,lease_token,lease_expires_at) \
         VALUES ($1,$2,'reserved',$3,$4,$5)",
    )
    .bind(frozen_submission_id.as_uuid())
    .bind(i32::try_from(attempt).map_err(|_| FreezeStoreError::AttemptOverflow)?)
    .bind(worker_id)
    .bind(lease_token)
    .bind(lease_expires_at)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn enqueue_frozen_event(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    lease: &FreezeLease,
    submission: &FrozenSubmission,
    trace_id: &str,
) -> Result<(), FreezeStoreError> {
    let contract = EVENT_CONTRACTS
        .iter()
        .copied()
        .find(|contract| contract.subject == subjects::SUBMISSION_FROZEN)
        .ok_or(FreezeStoreError::ContractInvalid)?;
    let data = SubmissionFrozen {
        submission: submission.clone(),
        source_identity_sha256: lease.source_identity_sha256,
    };
    data.validate()
        .map_err(|_| FreezeStoreError::ContractInvalid)?;
    let event_id = EventId::new();
    let event = CloudEvent {
        specversion: SPEC_VERSION.to_owned(),
        id: event_id,
        source: contract.source().to_owned(),
        event_type: subjects::SUBMISSION_FROZEN.to_owned(),
        subject: subjects::SUBMISSION_FROZEN.to_owned(),
        time: submission.frozen_at,
        datacontenttype: "application/json".to_owned(),
        dataschema: contract.data_schema(),
        course_id: submission.course_id,
        aggregate_revision: Revision::new(1).map_err(|_| FreezeStoreError::ContractInvalid)?,
        aggregate_sequence: Sequence(1),
        trace_id: trace_id.to_owned(),
        data,
    };
    event
        .validate(contract)
        .map_err(|_| FreezeStoreError::ContractInvalid)?;
    let payload = serde_json::to_value(&event).map_err(|_| FreezeStoreError::ContractInvalid)?;
    let payload_sha256 =
        Sha256Digest::of_canonical(&payload).map_err(|_| FreezeStoreError::ContractInvalid)?;
    OutboxStore::enqueue(
        transaction,
        Domain::Evaluation,
        event_id.as_uuid(),
        subjects::SUBMISSION_FROZEN,
        subjects::SUBMISSION_FROZEN,
        submission.id.as_uuid(),
        1,
        &payload,
        payload_sha256,
    )
    .await
    .map_err(|_| FreezeStoreError::DatabaseBoundary)
}

fn parse_id<T>(value: Uuid) -> Result<T, FreezeStoreError>
where
    T: FromStr<Err = uuid::Error>,
{
    value
        .hyphenated()
        .to_string()
        .parse()
        .map_err(|_| FreezeStoreError::ContractInvalid)
}

fn timestamp(value: OffsetDateTime) -> Result<UtcTimestamp, FreezeStoreError> {
    UtcTimestamp::from_utc(value).map_err(|_| FreezeStoreError::ClockInvalid)
}

fn validate_token(value: &str) -> Result<(), FreezeStoreError> {
    if value.trim().is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
        return Err(FreezeStoreError::TokenInvalid);
    }
    Ok(())
}

fn validate_trace_id(value: &str) -> Result<(), FreezeStoreError> {
    if value.trim().is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
        return Err(FreezeStoreError::TokenInvalid);
    }
    Ok(())
}

fn validate_object_key(value: &str) -> Result<(), FreezeStoreError> {
    if value.is_empty()
        || value.len() > 1_024
        || value.contains("..")
        || value.starts_with('/')
        || value.chars().any(char::is_control)
    {
        return Err(FreezeStoreError::ObjectKeyInvalid);
    }
    Ok(())
}

fn validate_diagnostic(value: &str) -> Result<(), FreezeStoreError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(FreezeStoreError::DiagnosticInvalid);
    }
    Ok(())
}

fn map_database_identity(error: sqlx::Error) -> FreezeStoreError {
    if error
        .as_database_error()
        .is_some_and(sqlx::error::DatabaseError::is_unique_violation)
    {
        FreezeStoreError::IdentityMismatch
    } else {
        FreezeStoreError::Database(error)
    }
}

/// Stable store failures; database details remain in the source chain, never in diagnostics.
#[derive(Debug, thiserror::Error)]
pub enum FreezeStoreError {
    #[error("LW_COLLECT_DATABASE_FAILED")]
    Database(#[source] sqlx::Error),
    #[error("LW_COLLECT_DATABASE_FAILED")]
    DatabaseBoundary,
    #[error("LW_COLLECT_CONTRACT_INVALID")]
    ContractInvalid,
    #[error("LW_COLLECT_IDENTITY_MISMATCH")]
    IdentityMismatch,
    #[error("LW_COLLECT_FENCE_LOST")]
    FenceLost,
    #[error("LW_COLLECT_LEASE_INVALID")]
    LeaseInvalid,
    #[error("LW_COLLECT_ATTEMPT_OVERFLOW")]
    AttemptOverflow,
    #[error("LW_COLLECT_CLOCK_INVALID")]
    ClockInvalid,
    #[error("LW_COLLECT_TOKEN_INVALID")]
    TokenInvalid,
    #[error("LW_COLLECT_OBJECT_KEY_INVALID")]
    ObjectKeyInvalid,
    #[error("LW_COLLECT_DIAGNOSTIC_INVALID")]
    DiagnosticInvalid,
}

impl From<sqlx::Error> for FreezeStoreError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error)
    }
}

impl FreezeStoreError {
    #[must_use]
    pub const fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::Database(_) | Self::DatabaseBoundary => "LW_COLLECT_DATABASE_FAILED",
            Self::ContractInvalid => "LW_COLLECT_CONTRACT_INVALID",
            Self::IdentityMismatch => "LW_COLLECT_IDENTITY_MISMATCH",
            Self::FenceLost => "LW_COLLECT_FENCE_LOST",
            Self::LeaseInvalid => "LW_COLLECT_LEASE_INVALID",
            Self::AttemptOverflow => "LW_COLLECT_ATTEMPT_OVERFLOW",
            Self::ClockInvalid => "LW_COLLECT_CLOCK_INVALID",
            Self::TokenInvalid => "LW_COLLECT_TOKEN_INVALID",
            Self::ObjectKeyInvalid => "LW_COLLECT_OBJECT_KEY_INVALID",
            Self::DiagnosticInvalid => "LW_COLLECT_DIAGNOSTIC_INVALID",
        }
    }
}
