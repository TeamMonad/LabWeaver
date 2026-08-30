//! Agent Outbox publication to `JetStream` with ACK-before-published ordering.
#![allow(clippy::missing_errors_doc)]
#![allow(
    missing_docs,
    reason = "stable diagnostics document transport outcomes"
)]

use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;
use std::time::Instant;

use async_nats::jetstream::AckKind;
use async_nats::jetstream::consumer::PullConsumer;
use async_nats::jetstream::message::PublishMessage;
use contracts::EventId;
use contracts::events::{AgentBuildRequested, CloudEvent, EVENT_CONTRACTS, subjects};
use futures_util::StreamExt;
use persistence_sqlx::Sha256Digest; // internal persistence hash, not contract hash
use serde::Serialize;
use serde_json::Value;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::build_store::{BuildCommandDecision, PgBuildStore};

const MAX_BUILD_COMMAND_BYTES: usize = 512 * 1024;
const BUILD_REDELIVERY_DELAY: Duration = Duration::from_secs(2);

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
        if timeout.is_zero() || timeout > Duration::from_mins(5) {
            return Err(AgentMessagingError::Configuration);
        }
        Ok(Self {
            pool,
            context: async_nats::jetstream::new(client),
            timeout,
        })
    }

    pub async fn dispatch_once(&self) -> Result<bool, AgentMessagingError> {
        let started = Instant::now();
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query("SELECT event_id,subject,event_type,payload,payload_sha256 FROM agent.outbox_events WHERE published_at IS NULL ORDER BY created_at,event_id FOR UPDATE SKIP LOCKED LIMIT 1").fetch_optional(&mut *transaction).await?;
        let Some(row) = row else {
            transaction.rollback().await?;
            tracing::debug!(
                event = "agent.outbox.idle",
                component = "outbox",
                operation = "nats.publish",
                outcome = "idle",
                duration_ms = elapsed_millis(started),
            );
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
        tracing::info!(
            event = "agent.outbox.published",
            component = "outbox",
            operation = "nats.publish",
            outcome = "succeeded",
            duration_ms = elapsed_millis(started),
            trace_id = event.trace_id,
            event_id = %event.id,
            message_id = %event.id,
            subject = event.subject,
        );
        Ok(true)
    }
}

/// Durable `JetStream` consumer for approved v1 build commands.
pub struct AgentBuildCommandConsumer {
    stream_name: String,
    consumer_name: String,
    quarantine_subject: String,
    context: async_nats::jetstream::Context,
    messages: async_nats::jetstream::consumer::pull::Stream,
}

impl AgentBuildCommandConsumer {
    pub async fn bind(
        client: async_nats::Client,
        stream_name: &str,
        consumer_name: &str,
        quarantine_subject: &str,
    ) -> Result<Self, AgentMessagingError> {
        if !valid_token(stream_name)
            || !valid_token(consumer_name)
            || !valid_subject(quarantine_subject)
        {
            return Err(AgentMessagingError::Configuration);
        }
        let context = async_nats::jetstream::new(client);
        let stream = context
            .get_stream(stream_name)
            .await
            .map_err(|_| AgentMessagingError::StreamUnavailable)?;
        let consumer: PullConsumer = stream
            .get_consumer(consumer_name)
            .await
            .map_err(|_| AgentMessagingError::ConsumerUnavailable)?;
        if consumer.cached_info().config.filter_subject != subjects::AGENT_BUILD_REQUESTED
            || !consumer.cached_info().config.filter_subjects.is_empty()
        {
            return Err(AgentMessagingError::Configuration);
        }
        let messages = consumer
            .messages()
            .await
            .map_err(|_| AgentMessagingError::ConsumerUnavailable)?;
        Ok(Self {
            stream_name: stream_name.to_owned(),
            consumer_name: consumer_name.to_owned(),
            quarantine_subject: quarantine_subject.to_owned(),
            context,
            messages,
        })
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one JetStream delivery owns parsing, durable decision, acknowledgement, and its correlated boundary log"
    )]
    pub async fn process_next(
        &mut self,
        store: &PgBuildStore,
    ) -> Result<BuildConsumeOutcome, AgentMessagingError> {
        let message = self
            .messages
            .next()
            .await
            .ok_or(AgentMessagingError::ConsumerClosed)?
            .map_err(|_| AgentMessagingError::Receive)?;
        let info = message.info().map_err(|_| AgentMessagingError::Receive)?;
        let delivery_attempt =
            u64::try_from(info.delivered).map_err(|_| AgentMessagingError::Receive)?;
        let started = Instant::now();
        if message.payload.len() > MAX_BUILD_COMMAND_BYTES {
            self.quarantine(&message, None, "LW_AGENT_BUILD_COMMAND_PAYLOAD_TOO_LARGE")
                .await?;
            message
                .double_ack_with(AckKind::Term)
                .await
                .map_err(|_| AgentMessagingError::Acknowledge)?;
            self.log_consumed(
                "rejected",
                started,
                delivery_attempt,
                None,
                None,
                "LW_AGENT_BUILD_COMMAND_PAYLOAD_TOO_LARGE",
            );
            return Ok(BuildConsumeOutcome::Rejected);
        }
        let Ok(event): Result<CloudEvent<AgentBuildRequested>, _> =
            serde_json::from_slice(&message.payload)
        else {
            self.quarantine(&message, None, "LW_AGENT_BUILD_COMMAND_PAYLOAD_INVALID")
                .await?;
            message
                .double_ack_with(AckKind::Term)
                .await
                .map_err(|_| AgentMessagingError::Acknowledge)?;
            self.log_consumed(
                "rejected",
                started,
                delivery_attempt,
                None,
                None,
                "LW_AGENT_BUILD_COMMAND_PAYLOAD_INVALID",
            );
            return Ok(BuildConsumeOutcome::Rejected);
        };
        match store.accept_command(&self.consumer_name, &event).await {
            Ok(BuildCommandDecision::Accepted) => {
                message
                    .double_ack()
                    .await
                    .map_err(|_| AgentMessagingError::Acknowledge)?;
                self.log_consumed(
                    "applied",
                    started,
                    delivery_attempt,
                    Some(&event.trace_id),
                    Some(event.id),
                    "LW_OK",
                );
                Ok(BuildConsumeOutcome::Applied)
            }
            Ok(BuildCommandDecision::Duplicate | BuildCommandDecision::Stale) => {
                message
                    .double_ack()
                    .await
                    .map_err(|_| AgentMessagingError::Acknowledge)?;
                self.log_consumed(
                    "ignored",
                    started,
                    delivery_attempt,
                    Some(&event.trace_id),
                    Some(event.id),
                    "LW_OK",
                );
                Ok(BuildConsumeOutcome::Ignored)
            }
            Ok(BuildCommandDecision::Gap) => {
                message
                    .ack_with(AckKind::Nak(Some(BUILD_REDELIVERY_DELAY)))
                    .await
                    .map_err(|_| AgentMessagingError::Acknowledge)?;
                self.log_consumed(
                    "deferred",
                    started,
                    delivery_attempt,
                    Some(&event.trace_id),
                    Some(event.id),
                    "LW_AGENT_BUILD_COMMAND_GAP",
                );
                Ok(BuildConsumeOutcome::Deferred)
            }
            Err(error) if error.retryable() => {
                message
                    .ack_with(AckKind::Nak(Some(BUILD_REDELIVERY_DELAY)))
                    .await
                    .map_err(|_| AgentMessagingError::Acknowledge)?;
                self.log_consumed(
                    "deferred",
                    started,
                    delivery_attempt,
                    Some(&event.trace_id),
                    Some(event.id),
                    "LW_AGENT_BUILD_COMMAND_RETRY_SCHEDULED",
                );
                Ok(BuildConsumeOutcome::Deferred)
            }
            Err(_) => {
                self.quarantine(&message, Some(event.id), "LW_AGENT_BUILD_COMMAND_REJECTED")
                    .await?;
                message
                    .double_ack_with(AckKind::Term)
                    .await
                    .map_err(|_| AgentMessagingError::Acknowledge)?;
                self.log_consumed(
                    "rejected",
                    started,
                    delivery_attempt,
                    Some(&event.trace_id),
                    Some(event.id),
                    "LW_AGENT_BUILD_COMMAND_REJECTED",
                );
                Ok(BuildConsumeOutcome::Rejected)
            }
        }
    }

    fn log_consumed(
        &self,
        outcome: &'static str,
        started: Instant,
        delivery_attempt: u64,
        trace_id: Option<&str>,
        event_id: Option<EventId>,
        diagnostic_code: &'static str,
    ) {
        if let (Some(trace_id), Some(event_id)) = (trace_id, event_id) {
            tracing::info!(
                event = "agent.build_command.consumed",
                component = "build-command-consumer",
                operation = "nats.consume",
                outcome,
                duration_ms = elapsed_millis(started),
                stream = self.stream_name,
                consumer = self.consumer_name,
                subject = subjects::AGENT_BUILD_REQUESTED,
                delivery_attempt,
                trace_id,
                event_id = %event_id,
                diagnostic_code,
                retryable = outcome == "deferred",
            );
        } else {
            tracing::info!(
                event = "agent.build_command.consumed",
                component = "build-command-consumer",
                operation = "nats.consume",
                outcome,
                duration_ms = elapsed_millis(started),
                stream = self.stream_name,
                consumer = self.consumer_name,
                subject = subjects::AGENT_BUILD_REQUESTED,
                delivery_attempt,
                diagnostic_code,
                retryable = false,
            );
        }
    }

    async fn quarantine(
        &self,
        message: &async_nats::jetstream::Message,
        event_id: Option<EventId>,
        diagnostic_code: &'static str,
    ) -> Result<(), AgentMessagingError> {
        let payload_sha256 = Sha256Digest::of_bytes(&message.payload);
        let record = BuildQuarantineRecord {
            version: 1,
            consumer: &self.consumer_name,
            source_subject: message.subject.as_str(),
            event_id,
            payload_sha256,
            size_bytes: u64::try_from(message.payload.len())
                .map_err(|_| AgentMessagingError::Quarantine)?,
            diagnostic_code,
        };
        let payload = serde_json::to_vec(&record).map_err(|_| AgentMessagingError::Quarantine)?;
        let acknowledgement = self
            .context
            .send_publish(
                self.quarantine_subject.clone(),
                PublishMessage::build()
                    .payload(payload.into())
                    .message_id(format!("{payload_sha256}:{diagnostic_code}")),
            )
            .await
            .map_err(|_| AgentMessagingError::Quarantine)?;
        acknowledgement
            .await
            .map_err(|_| AgentMessagingError::Quarantine)?;
        Ok(())
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BuildQuarantineRecord<'a> {
    version: u8,
    consumer: &'a str,
    source_subject: &'a str,
    event_id: Option<EventId>,
    payload_sha256: Sha256Digest,
    size_bytes: u64,
    diagnostic_code: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuildConsumeOutcome {
    Applied,
    Ignored,
    Deferred,
    Rejected,
}

fn valid_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
}

fn valid_subject(value: &str) -> bool {
    valid_token(value) && !value.contains('*') && !value.contains('>')
}

fn elapsed_millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[derive(Debug, thiserror::Error)]
pub enum AgentMessagingError {
    #[error("LW_NATS_CONFIG_INVALID")]
    Configuration,
    #[error("LW_NATS_CREDENTIALS_INVALID")]
    Credentials,
    #[error("LW_NATS_UNAVAILABLE")]
    Connect,
    #[error("LW_AGENT_BUILD_STREAM_UNAVAILABLE")]
    StreamUnavailable,
    #[error("LW_AGENT_BUILD_CONSUMER_UNAVAILABLE")]
    ConsumerUnavailable,
    #[error("LW_AGENT_BUILD_CONSUMER_CLOSED")]
    ConsumerClosed,
    #[error("LW_AGENT_BUILD_RECEIVE_FAILED")]
    Receive,
    #[error("LW_AGENT_BUILD_ACKNOWLEDGE_FAILED")]
    Acknowledge,
    #[error("LW_AGENT_BUILD_QUARANTINE_FAILED")]
    Quarantine,
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
