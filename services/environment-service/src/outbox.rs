use std::str::FromStr;
use std::time::Duration;

use async_trait::async_trait;
use contracts::events::{CloudEvent, EVENT_CONTRACTS};
use contracts::{EventId, Sha256Digest};
use serde_json::Value;
use sqlx::{PgPool, Row};
use tokio::time::timeout;
use uuid::Uuid;

/// Sanitized transport failure returned by an explicit event publisher implementation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PublishFailure {
    #[error("LW_ENVIRONMENT_EVENT_PUBLISH_UNAVAILABLE")]
    Unavailable,
    #[error("LW_ENVIRONMENT_EVENT_PUBLISH_REJECTED")]
    Rejected,
}

/// Explicit transport boundary used by the transactional Environment Outbox dispatcher.
#[async_trait]
pub trait EnvironmentEventPublisher: Send + Sync {
    async fn publish(&self, subject: &str, event: &CloudEvent<Value>)
    -> Result<(), PublishFailure>;
}

/// Bounded `PostgreSQL` Outbox dispatcher. A transport success and `published_at` commit together.
pub struct OutboxDispatcher<P> {
    pool: PgPool,
    publisher: P,
    publish_timeout: Duration,
}

impl<P> OutboxDispatcher<P>
where
    P: EnvironmentEventPublisher,
{
    pub fn new(
        pool: PgPool,
        publisher: P,
        publish_timeout: Duration,
    ) -> Result<Self, OutboxDispatchError> {
        if publish_timeout.is_zero() || publish_timeout > Duration::from_secs(300) {
            return Err(OutboxDispatchError::InvalidConfiguration);
        }
        Ok(Self {
            pool,
            publisher,
            publish_timeout,
        })
    }

    /// Publishes at most one row. A crash or failure before commit deliberately causes replay.
    pub async fn dispatch_once(&self) -> Result<OutboxDispatchOutcome, OutboxDispatchError> {
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query(
            "SELECT event_id, subject, event_type, payload, payload_sha256 \
             FROM environment.outbox_events WHERE published_at IS NULL \
             ORDER BY created_at, event_id FOR UPDATE SKIP LOCKED LIMIT 1",
        )
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(row) = row else {
            transaction.rollback().await?;
            return Ok(OutboxDispatchOutcome::Idle);
        };
        let event_uuid: Uuid = row.try_get("event_id")?;
        let subject: String = row.try_get("subject")?;
        let event_type: String = row.try_get("event_type")?;
        let payload: Value = row.try_get("payload")?;
        let stored_hash: String = row.try_get("payload_sha256")?;
        let calculated_hash = Sha256Digest::of_canonical(&payload)
            .map_err(|_| OutboxDispatchError::PayloadIdentityInvalid)?;
        if stored_hash != calculated_hash.to_string() {
            return Err(OutboxDispatchError::PayloadIdentityInvalid);
        }
        let event: CloudEvent<Value> = serde_json::from_value(payload)
            .map_err(|_| OutboxDispatchError::PayloadContractInvalid)?;
        let event_id = EventId::from_str(&event_uuid.to_string())
            .map_err(|_| OutboxDispatchError::PayloadIdentityInvalid)?;
        let contract = EVENT_CONTRACTS
            .iter()
            .copied()
            .find(|contract| contract.subject == subject)
            .ok_or(OutboxDispatchError::PayloadContractInvalid)?;
        if event.id != event_id || event_type != subject {
            return Err(OutboxDispatchError::PayloadIdentityInvalid);
        }
        event
            .validate(contract)
            .map_err(|_| OutboxDispatchError::PayloadContractInvalid)?;

        timeout(
            self.publish_timeout,
            self.publisher.publish(&subject, &event),
        )
        .await
        .map_err(|_| OutboxDispatchError::PublishTimeout)??;
        let updated = sqlx::query(
            "UPDATE environment.outbox_events \
             SET published_at=date_trunc('milliseconds', clock_timestamp()) \
             WHERE event_id=$1 AND published_at IS NULL",
        )
        .bind(event_uuid)
        .execute(&mut *transaction)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(OutboxDispatchError::PublishFenceLost);
        }
        transaction.commit().await?;
        Ok(OutboxDispatchOutcome::Published { event_id })
    }
}

/// Outcome of one bounded dispatcher iteration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutboxDispatchOutcome {
    Idle,
    Published { event_id: EventId },
}

/// Stable Outbox dispatch failure without transport payloads or secrets.
#[derive(Debug, thiserror::Error)]
pub enum OutboxDispatchError {
    #[error("LW_ENVIRONMENT_OUTBOX_CONFIGURATION_INVALID")]
    InvalidConfiguration,
    #[error("LW_ENVIRONMENT_OUTBOX_PAYLOAD_IDENTITY_INVALID")]
    PayloadIdentityInvalid,
    #[error("LW_ENVIRONMENT_OUTBOX_PAYLOAD_CONTRACT_INVALID")]
    PayloadContractInvalid,
    #[error("LW_ENVIRONMENT_OUTBOX_PUBLISH_TIMEOUT")]
    PublishTimeout,
    #[error("LW_ENVIRONMENT_OUTBOX_PUBLISH_FENCE_LOST")]
    PublishFenceLost,
    #[error("LW_ENVIRONMENT_OUTBOX_DATABASE_FAILED")]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Publish(#[from] PublishFailure),
}
