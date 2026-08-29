//! Transactional Evaluation Outbox publication to `JetStream`.
#![allow(
    missing_docs,
    clippy::missing_errors_doc,
    reason = "the internal dispatcher exposes only stable diagnostics"
)]

use std::{str::FromStr, time::Duration};
use crate::hash_compat::Sha256Digest;

use async_nats::jetstream::message::PublishMessage;
use contracts::{
    EventId,
    events::{CloudEvent, EVENT_CONTRACTS},
};
use serde_json::Value;
use sqlx::{PgPool, Row};
use tokio::time::timeout;

/// Bounded Evaluation Outbox dispatcher with server persistence acknowledgement.
pub struct EvaluationOutboxDispatcher {
    pool: PgPool,
    jetstream: async_nats::jetstream::Context,
    publish_timeout: Duration,
}

impl EvaluationOutboxDispatcher {
    pub fn new(
        pool: PgPool,
        client: async_nats::Client,
        publish_timeout: Duration,
    ) -> Result<Self, EvaluationOutboxError> {
        if publish_timeout.is_zero() || publish_timeout > Duration::from_mins(5) {
            return Err(EvaluationOutboxError::ConfigurationInvalid);
        }
        Ok(Self {
            pool,
            jetstream: async_nats::jetstream::new(client),
            publish_timeout,
        })
    }

    pub async fn dispatch_once(&self) -> Result<bool, EvaluationOutboxError> {
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query(
            "SELECT event_id,subject,event_type,payload,payload_sha256 \
             FROM evaluation.outbox_events WHERE published_at IS NULL \
             ORDER BY created_at,event_id FOR UPDATE SKIP LOCKED LIMIT 1",
        )
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(row) = row else {
            transaction.rollback().await?;
            return Ok(false);
        };
        let event_uuid: uuid::Uuid = row.try_get("event_id")?;
        let subject: String = row.try_get("subject")?;
        let event_type: String = row.try_get("event_type")?;
        let payload: Value = row.try_get("payload")?;
        let stored_hash: String = row.try_get("payload_sha256")?;
        let calculated_hash = Sha256Digest::of_canonical(&payload)
            .map_err(|_| EvaluationOutboxError::PayloadIdentityInvalid)?;
        if stored_hash != calculated_hash.to_string() || event_type != subject {
            return Err(EvaluationOutboxError::PayloadIdentityInvalid);
        }
        let event: CloudEvent<Value> = serde_json::from_value(payload)
            .map_err(|_| EvaluationOutboxError::PayloadContractInvalid)?;
        let event_id = EventId::from_str(&event_uuid.to_string())
            .map_err(|_| EvaluationOutboxError::PayloadIdentityInvalid)?;
        let contract = EVENT_CONTRACTS
            .iter()
            .copied()
            .find(|contract| contract.subject == subject)
            .ok_or(EvaluationOutboxError::PayloadContractInvalid)?;
        if event.id != event_id {
            return Err(EvaluationOutboxError::PayloadIdentityInvalid);
        }
        event
            .validate(contract)
            .map_err(|_| EvaluationOutboxError::PayloadContractInvalid)?;
        let acknowledgement = timeout(
            self.publish_timeout,
            self.jetstream.send_publish(
                subject,
                PublishMessage::build()
                    .payload(serde_json::to_vec(&event)?.into())
                    .message_id(event.id.to_string()),
            ),
        )
        .await
        .map_err(|_| EvaluationOutboxError::PublishTimeout)?
        .map_err(|_| EvaluationOutboxError::PublishUnavailable)?;
        timeout(self.publish_timeout, acknowledgement)
            .await
            .map_err(|_| EvaluationOutboxError::PublishTimeout)?
            .map_err(|_| EvaluationOutboxError::PublishRejected)?;
        let result = sqlx::query(
            "UPDATE evaluation.outbox_events SET published_at=date_trunc('milliseconds',clock_timestamp()) \
             WHERE event_id=$1 AND published_at IS NULL",
        )
        .bind(event_uuid)
        .execute(&mut *transaction)
        .await?;
        if result.rows_affected() != 1 {
            return Err(EvaluationOutboxError::PublishFenceLost);
        }
        transaction.commit().await?;
        Ok(true)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EvaluationOutboxError {
    #[error("LW_EVALUATION_OUTBOX_CONFIG_INVALID")]
    ConfigurationInvalid,
    #[error("LW_EVALUATION_OUTBOX_PAYLOAD_IDENTITY_INVALID")]
    PayloadIdentityInvalid,
    #[error("LW_EVALUATION_OUTBOX_PAYLOAD_CONTRACT_INVALID")]
    PayloadContractInvalid,
    #[error("LW_EVALUATION_OUTBOX_PUBLISH_TIMEOUT")]
    PublishTimeout,
    #[error("LW_EVALUATION_OUTBOX_PUBLISH_UNAVAILABLE")]
    PublishUnavailable,
    #[error("LW_EVALUATION_OUTBOX_PUBLISH_REJECTED")]
    PublishRejected,
    #[error("LW_EVALUATION_OUTBOX_FENCE_LOST")]
    PublishFenceLost,
    #[error("LW_EVALUATION_OUTBOX_DATABASE_FAILED")]
    Database(#[from] sqlx::Error),
    #[error("LW_EVALUATION_OUTBOX_PAYLOAD_CONTRACT_INVALID")]
    Serialization(#[from] serde_json::Error),
}