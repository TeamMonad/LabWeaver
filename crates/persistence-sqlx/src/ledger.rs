use serde_json::Value;

use crate::Sha256Digest;
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

use crate::{Domain, PersistenceError};

/// Result of reserving an idempotency key inside the caller's transaction.
#[derive(Clone, Debug, PartialEq)]
pub enum IdempotencyDecision {
    /// The caller owns a new reservation and may create the object.
    Reserved,
    /// The exact request completed previously; return this stored result.
    Replay(Value),
    /// The same key was used with a different request identity.
    Conflict,
    /// The exact request is already executing in another transaction.
    InProgress,
}

/// Domain-local idempotency ledger operations.
pub struct IdempotencyStore;

impl IdempotencyStore {
    /// Reserves a key without committing independently from the business transaction.
    pub async fn reserve(
        transaction: &mut Transaction<'_, Postgres>,
        domain: Domain,
        operation: &str,
        key: &str,
        request_hash: Sha256Digest,
    ) -> Result<IdempotencyDecision, PersistenceError> {
        validate_token("operation", operation)?;
        validate_token("idempotency key", key)?;
        let insert = format!(
            "INSERT INTO {}.idempotency_ledger (operation, idempotency_key, request_sha256, state) \
             VALUES ($1, $2, $3, 'in_progress') ON CONFLICT DO NOTHING",
            domain.schema()
        );
        let result = sqlx::query(&insert)
            .bind(operation)
            .bind(key)
            .bind(request_hash.to_string())
            .execute(&mut **transaction)
            .await?;
        if result.rows_affected() == 1 {
            return Ok(IdempotencyDecision::Reserved);
        }
        let select = format!(
            "SELECT request_sha256, state, result FROM {}.idempotency_ledger \
             WHERE operation = $1 AND idempotency_key = $2 FOR UPDATE",
            domain.schema()
        );
        let row = sqlx::query(&select)
            .bind(operation)
            .bind(key)
            .fetch_one(&mut **transaction)
            .await?;
        let observed: String = row.try_get("request_sha256")?;
        if observed != request_hash.to_string() {
            return Ok(IdempotencyDecision::Conflict);
        }
        let state: String = row.try_get("state")?;
        if state == "completed" {
            let result = row.try_get::<Value, _>("result")?;
            Ok(IdempotencyDecision::Replay(result))
        } else {
            Ok(IdempotencyDecision::InProgress)
        }
    }

    /// Completes a reservation in the same transaction as the authoritative result.
    pub async fn complete(
        transaction: &mut Transaction<'_, Postgres>,
        domain: Domain,
        operation: &str,
        key: &str,
        result: &Value,
    ) -> Result<(), PersistenceError> {
        let query = format!(
            "UPDATE {}.idempotency_ledger SET state = 'completed', result = $3, completed_at = now() \
             WHERE operation = $1 AND idempotency_key = $2 AND state = 'in_progress'",
            domain.schema()
        );
        let updated = sqlx::query(&query)
            .bind(operation)
            .bind(key)
            .bind(result)
            .execute(&mut **transaction)
            .await?;
        if updated.rows_affected() != 1 {
            return Err(PersistenceError::IdentityMismatch(
                "idempotency reservation is absent or already completed".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Domain-local transactional Outbox operations.
pub struct OutboxStore;

impl OutboxStore {
    /// Enqueues one immutable event inside the caller's business transaction.
    #[allow(clippy::too_many_arguments)]
    pub async fn enqueue(
        transaction: &mut Transaction<'_, Postgres>,
        domain: Domain,
        event_id: Uuid,
        subject: &str,
        event_type: &str,
        aggregate_id: Uuid,
        sequence: u64,
        payload: &Value,
        payload_hash: Sha256Digest,
    ) -> Result<(), PersistenceError> {
        if sequence == 0 {
            return Err(PersistenceError::Configuration(
                "Outbox sequence must be non-zero".to_owned(),
            ));
        }
        validate_token("subject", subject)?;
        validate_token("event type", event_type)?;
        let sequence = i64::try_from(sequence).map_err(|_| {
            PersistenceError::Configuration("Outbox sequence exceeds BIGINT".to_owned())
        })?;
        let query = format!(
            "INSERT INTO {}.outbox_events \
             (event_id, subject, event_type, aggregate_id, aggregate_sequence, payload, payload_sha256) \
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
            domain.schema()
        );
        sqlx::query(&query)
            .bind(event_id)
            .bind(subject)
            .bind(event_type)
            .bind(aggregate_id)
            .bind(sequence)
            .bind(payload)
            .bind(payload_hash.to_string())
            .execute(&mut **transaction)
            .await?;
        Ok(())
    }
}

/// Result of reserving an Inbox event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InboxDecision {
    /// The event is next in sequence and may produce a side effect in this transaction.
    Accepted,
    /// The exact event was already processed.
    Duplicate,
    /// Its sequence is older than the durable consumer watermark.
    Stale,
    /// A preceding aggregate event has not been durably processed.
    Gap,
}

/// Domain-local Inbox and aggregate-watermark operations.
pub struct InboxStore;

impl InboxStore {
    /// Reserves the next event in the same transaction as its business side effect.
    pub async fn accept(
        transaction: &mut Transaction<'_, Postgres>,
        domain: Domain,
        consumer: &str,
        event_id: Uuid,
        aggregate_id: Uuid,
        sequence: u64,
        payload_hash: Sha256Digest,
    ) -> Result<InboxDecision, PersistenceError> {
        validate_token("consumer", consumer)?;
        let sequence = i64::try_from(sequence).map_err(|_| {
            PersistenceError::Configuration("Inbox sequence exceeds BIGINT".to_owned())
        })?;
        if sequence == 0 {
            return Err(PersistenceError::Configuration(
                "Inbox sequence must be non-zero".to_owned(),
            ));
        }
        let duplicate = format!(
            "SELECT payload_sha256 FROM {}.inbox_events WHERE consumer = $1 AND event_id = $2",
            domain.schema()
        );
        if let Some(row) = sqlx::query(&duplicate)
            .bind(consumer)
            .bind(event_id)
            .fetch_optional(&mut **transaction)
            .await?
        {
            let observed: String = row.try_get("payload_sha256")?;
            if observed != payload_hash.to_string() {
                return Err(PersistenceError::IdentityMismatch(
                    "duplicate event ID has a different payload hash".to_owned(),
                ));
            }
            return Ok(InboxDecision::Duplicate);
        }
        let create_watermark = format!(
            "INSERT INTO {}.inbox_watermarks (consumer, aggregate_id, last_sequence) VALUES ($1, $2, 0) \
             ON CONFLICT (consumer, aggregate_id) DO NOTHING",
            domain.schema()
        );
        sqlx::query(&create_watermark)
            .bind(consumer)
            .bind(aggregate_id)
            .execute(&mut **transaction)
            .await?;
        let watermark_query = format!(
            "SELECT last_sequence FROM {}.inbox_watermarks \
             WHERE consumer = $1 AND aggregate_id = $2 FOR UPDATE",
            domain.schema()
        );
        let last = sqlx::query(&watermark_query)
            .bind(consumer)
            .bind(aggregate_id)
            .fetch_one(&mut **transaction)
            .await?
            .try_get::<i64, _>("last_sequence")?;
        let expected = last.saturating_add(1);
        if sequence < expected {
            return Ok(InboxDecision::Stale);
        }
        if sequence > expected {
            return Ok(InboxDecision::Gap);
        }
        let insert = format!(
            "INSERT INTO {}.inbox_events \
             (consumer, event_id, aggregate_id, aggregate_sequence, payload_sha256) \
             VALUES ($1, $2, $3, $4, $5)",
            domain.schema()
        );
        sqlx::query(&insert)
            .bind(consumer)
            .bind(event_id)
            .bind(aggregate_id)
            .bind(sequence)
            .bind(payload_hash.to_string())
            .execute(&mut **transaction)
            .await?;
        let update = format!(
            "UPDATE {}.inbox_watermarks SET last_sequence = $3, updated_at = now() \
             WHERE consumer = $1 AND aggregate_id = $2 AND last_sequence = $4",
            domain.schema()
        );
        let updated = sqlx::query(&update)
            .bind(consumer)
            .bind(aggregate_id)
            .bind(sequence)
            .bind(last)
            .execute(&mut **transaction)
            .await?;
        if updated.rows_affected() != 1 {
            return Err(PersistenceError::IdentityMismatch(
                "Inbox watermark changed while its row lock was held".to_owned(),
            ));
        }
        Ok(InboxDecision::Accepted)
    }
}

fn validate_token(label: &str, value: &str) -> Result<(), PersistenceError> {
    if value.trim().is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
        return Err(PersistenceError::Configuration(format!(
            "{label} is empty, oversized, or contains control characters"
        )));
    }
    Ok(())
}
