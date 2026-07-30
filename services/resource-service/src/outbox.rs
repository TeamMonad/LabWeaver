//! Transactional Resource Outbox dispatcher backed by JetStream acknowledgements.

use std::str::FromStr;
use std::time::Duration;

use async_nats::jetstream::message::PublishMessage;
use contracts::events::{CloudEvent, EVENT_CONTRACTS};
use contracts::{EventId, Sha256Digest};
use serde_json::Value;
use sqlx::{PgPool, Row};
use tokio::time::timeout;
use uuid::Uuid;

/// Bounded dispatcher. Transport acknowledgement precedes the `published_at` commit, so a
/// process crash deliberately yields an idempotent replay.
#[derive(Clone)]
pub struct ResourceOutboxDispatcher {
    pool: PgPool,
    jetstream: async_nats::jetstream::Context,
    timeout: Duration,
}

impl ResourceOutboxDispatcher {
    pub fn new(
        pool: PgPool,
        client: async_nats::Client,
        timeout: Duration,
    ) -> Result<Self, ResourceOutboxError> {
        if timeout.is_zero() || timeout > Duration::from_secs(300) {
            return Err(ResourceOutboxError::Configuration);
        }
        Ok(Self {
            pool,
            jetstream: async_nats::jetstream::new(client),
            timeout,
        })
    }

    pub async fn dispatch_once(&self) -> Result<ResourceOutboxOutcome, ResourceOutboxError> {
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query("SELECT event_id,subject,event_type,payload,payload_sha256 FROM resource.outbox_events WHERE published_at IS NULL ORDER BY created_at,event_id FOR UPDATE SKIP LOCKED LIMIT 1")
            .fetch_optional(&mut *transaction).await?;
        let Some(row) = row else {
            transaction.rollback().await?;
            return Ok(ResourceOutboxOutcome::Idle);
        };
        let event_id: Uuid = row.try_get("event_id")?;
        let subject: String = row.try_get("subject")?;
        let event_type: String = row.try_get("event_type")?;
        let payload: Value = row.try_get("payload")?;
        let hash: String = row.try_get("payload_sha256")?;
        if Sha256Digest::of_canonical(&payload)
            .map_err(|_| ResourceOutboxError::Identity)?
            .to_string()
            != hash
        {
            return Err(ResourceOutboxError::Identity);
        }
        let event: CloudEvent<Value> =
            serde_json::from_value(payload).map_err(|_| ResourceOutboxError::Contract)?;
        let expected_id =
            EventId::from_str(&event_id.to_string()).map_err(|_| ResourceOutboxError::Identity)?;
        let contract = EVENT_CONTRACTS
            .iter()
            .copied()
            .find(|candidate| candidate.subject == subject)
            .ok_or(ResourceOutboxError::Contract)?;
        if event.id != expected_id || event_type != subject {
            return Err(ResourceOutboxError::Identity);
        }
        event
            .validate(contract)
            .map_err(|_| ResourceOutboxError::Contract)?;
        let wire = serde_json::to_vec(&event).map_err(|_| ResourceOutboxError::Contract)?;
        let acknowledgement = timeout(
            self.timeout,
            self.jetstream.send_publish(
                subject.clone(),
                PublishMessage::build()
                    .payload(wire.into())
                    .message_id(event_id.to_string()),
            ),
        )
        .await
        .map_err(|_| ResourceOutboxError::Timeout)?
        .map_err(|_| ResourceOutboxError::Publish)?;
        timeout(self.timeout, acknowledgement)
            .await
            .map_err(|_| ResourceOutboxError::Timeout)?
            .map_err(|_| ResourceOutboxError::Publish)?;
        let updated = sqlx::query("UPDATE resource.outbox_events SET published_at=date_trunc('milliseconds',clock_timestamp()) WHERE event_id=$1 AND published_at IS NULL")
            .bind(event_id).execute(&mut *transaction).await?;
        if updated.rows_affected() != 1 {
            return Err(ResourceOutboxError::FenceLost);
        }
        transaction.commit().await?;
        Ok(ResourceOutboxOutcome::Published {
            event_id: expected_id,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceOutboxOutcome {
    Idle,
    Published { event_id: EventId },
}

#[derive(Debug, thiserror::Error)]
pub enum ResourceOutboxError {
    #[error("LW_RESOURCE_OUTBOX_CONFIGURATION_INVALID")]
    Configuration,
    #[error("LW_RESOURCE_OUTBOX_IDENTITY_INVALID")]
    Identity,
    #[error("LW_RESOURCE_OUTBOX_CONTRACT_INVALID")]
    Contract,
    #[error("LW_RESOURCE_OUTBOX_PUBLISH_TIMEOUT")]
    Timeout,
    #[error("LW_RESOURCE_OUTBOX_PUBLISH_FAILED")]
    Publish,
    #[error("LW_RESOURCE_OUTBOX_FENCE_LOST")]
    FenceLost,
    #[error("LW_RESOURCE_OUTBOX_DATABASE_FAILED")]
    Database(#[from] sqlx::Error),
}
