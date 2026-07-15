//! Agent Outbox publication to `JetStream` with ACK-before-published ordering.
#![allow(clippy::missing_errors_doc)]
#![allow(
    missing_docs,
    reason = "stable diagnostics document transport outcomes"
)]

use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use async_nats::jetstream::message::PublishMessage;
use contracts::events::{CloudEvent, EVENT_CONTRACTS};
use contracts::{EventId, Sha256Digest};
use serde_json::Value;
use sqlx::{PgPool, Row};
use uuid::Uuid;

pub async fn connect_nats_mtls(
    server: &str,
    ca: PathBuf,
    certificate: PathBuf,
    key: PathBuf,
    credentials: PathBuf,
) -> Result<async_nats::Client, AgentMessagingError> {
    if server.trim().is_empty()
        || [&ca, &certificate, &key, &credentials]
            .iter()
            .any(|path| path.as_os_str().is_empty())
    {
        return Err(AgentMessagingError::Configuration);
    }
    async_nats::ConnectOptions::new()
        .require_tls(true)
        .add_root_certificates(ca)
        .add_client_certificate(certificate, key)
        .credentials_file(credentials)
        .await
        .map_err(|_| AgentMessagingError::Credentials)?
        .connect(server)
        .await
        .map_err(|_| AgentMessagingError::Connect)
}

/// Bounded `PostgreSQL` Outbox dispatcher.
pub struct AgentOutboxDispatcher {
    pool: PgPool,
    context: async_nats::jetstream::Context,
    timeout: Duration,
}

impl AgentOutboxDispatcher {
    pub fn new(
        pool: PgPool,
        client: async_nats::Client,
        timeout: Duration,
    ) -> Result<Self, AgentMessagingError> {
        if timeout.is_zero() || timeout > Duration::from_secs(300) {
            return Err(AgentMessagingError::Configuration);
        }
        Ok(Self {
            pool,
            context: async_nats::jetstream::new(client),
            timeout,
        })
    }

    pub async fn dispatch_once(&self) -> Result<bool, AgentMessagingError> {
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query("SELECT event_id,subject,event_type,payload,payload_sha256 FROM agent.outbox_events WHERE published_at IS NULL ORDER BY created_at,event_id FOR UPDATE SKIP LOCKED LIMIT 1").fetch_optional(&mut *transaction).await?;
        let Some(row) = row else {
            transaction.rollback().await?;
            return Ok(false);
        };
        let event_uuid: Uuid = row.try_get("event_id")?;
        let subject: String = row.try_get("subject")?;
        let event_type: String = row.try_get("event_type")?;
        let payload: Value = row.try_get("payload")?;
        let stored_hash: String = row.try_get("payload_sha256")?;
        let hash =
            Sha256Digest::of_canonical(&payload).map_err(|_| AgentMessagingError::Identity)?;
        if hash.to_string() != stored_hash {
            return Err(AgentMessagingError::Identity);
        }
        let event: CloudEvent<Value> =
            serde_json::from_value(payload).map_err(|_| AgentMessagingError::Contract)?;
        let event_id = EventId::from_str(&event_uuid.to_string())
            .map_err(|_| AgentMessagingError::Identity)?;
        let contract = EVENT_CONTRACTS
            .iter()
            .copied()
            .find(|contract| contract.subject == subject)
            .ok_or(AgentMessagingError::Contract)?;
        if event.id != event_id || event_type != subject || event.validate(contract).is_err() {
            return Err(AgentMessagingError::Contract);
        }
        let bytes = serde_json::to_vec(&event).map_err(|_| AgentMessagingError::Contract)?;
        let publish = tokio::time::timeout(
            self.timeout,
            self.context.send_publish(
                subject,
                PublishMessage::build()
                    .payload(bytes.into())
                    .message_id(event.id.to_string()),
            ),
        )
        .await
        .map_err(|_| AgentMessagingError::Timeout)?
        .map_err(|_| AgentMessagingError::Publish)?;
        tokio::time::timeout(self.timeout, publish)
            .await
            .map_err(|_| AgentMessagingError::Timeout)?
            .map_err(|_| AgentMessagingError::Publish)?;
        let updated = sqlx::query("UPDATE agent.outbox_events SET published_at=date_trunc('milliseconds',clock_timestamp()) WHERE event_id=$1 AND published_at IS NULL").bind(event_uuid).execute(&mut *transaction).await?;
        if updated.rows_affected() != 1 {
            return Err(AgentMessagingError::Fence);
        }
        transaction.commit().await?;
        Ok(true)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AgentMessagingError {
    #[error("LW_NATS_CONFIG_INVALID")]
    Configuration,
    #[error("LW_NATS_CREDENTIALS_INVALID")]
    Credentials,
    #[error("LW_NATS_UNAVAILABLE")]
    Connect,
    #[error("LW_AGENT_OUTBOX_IDENTITY_INVALID")]
    Identity,
    #[error("LW_AGENT_OUTBOX_CONTRACT_INVALID")]
    Contract,
    #[error("LW_AGENT_OUTBOX_PUBLISH_FAILED")]
    Publish,
    #[error("LW_AGENT_OUTBOX_PUBLISH_TIMEOUT")]
    Timeout,
    #[error("LW_AGENT_OUTBOX_FENCE_LOST")]
    Fence,
    #[error("LW_AGENT_OUTBOX_DATABASE_FAILED")]
    Database(#[from] sqlx::Error),
}
