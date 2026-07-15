//! Durable Agent event consumption with authoritative mTLS readback.
#![allow(clippy::missing_errors_doc)]
#![allow(
    missing_docs,
    reason = "stable diagnostics document transport outcomes"
)]

use std::path::PathBuf;
use std::time::Duration;

use async_nats::jetstream::AckKind;
use async_nats::jetstream::consumer::PullConsumer;
use async_nats::jetstream::message::PublishMessage;
use async_trait::async_trait;
use contracts::events::{AgentRunEvent, CloudEvent, EVENT_CONTRACTS, subjects};
use contracts::{EventId, Sha256Digest};
use futures_util::StreamExt;
use serde::Serialize;

use crate::clients::AgentClient;
use crate::{ControlError, ControlService};

const MAX_EVENT_BYTES: usize = 1024 * 1024;
const REDELIVERY_DELAY: Duration = Duration::from_secs(1);

/// Agent-owned read boundary used by the durable projection consumer.
#[async_trait]
pub trait AgentAuthority: Send + Sync {
    async fn get(
        &self,
        run_id: contracts::AgentRunId,
    ) -> Result<contracts::authoring::AgentRun, crate::clients::DownstreamError>;

    async fn outcome(
        &self,
        run_id: contracts::AgentRunId,
    ) -> Result<contracts::http::InternalAgentRunOutcome, crate::clients::DownstreamError>;
}

#[async_trait]
impl AgentAuthority for AgentClient {
    async fn get(
        &self,
        run_id: contracts::AgentRunId,
    ) -> Result<contracts::authoring::AgentRun, crate::clients::DownstreamError> {
        AgentClient::get(self, run_id).await
    }

    async fn outcome(
        &self,
        run_id: contracts::AgentRunId,
    ) -> Result<contracts::http::InternalAgentRunOutcome, crate::clients::DownstreamError> {
        AgentClient::outcome(self, run_id).await
    }
}

/// Connects to NATS using only explicit private CA, certificate, key, and credentials files.
pub async fn connect_nats_mtls(
    server: &str,
    ca_path: PathBuf,
    certificate_path: PathBuf,
    key_path: PathBuf,
    credentials_path: PathBuf,
) -> Result<async_nats::Client, MessagingError> {
    if server.trim().is_empty()
        || [&ca_path, &certificate_path, &key_path, &credentials_path]
            .iter()
            .any(|path| path.as_os_str().is_empty())
    {
        return Err(MessagingError::Configuration);
    }
    async_nats::ConnectOptions::new()
        .require_tls(true)
        .add_root_certificates(ca_path)
        .add_client_certificate(certificate_path, key_path)
        .credentials_file(credentials_path)
        .await
        .map_err(|_| MessagingError::Credentials)?
        .connect(server)
        .await
        .map_err(|_| MessagingError::Connect)
}

/// Existing deployment-owned durable consumer for all `AgentRun` lifecycle events.
pub struct AgentRunConsumer {
    context: async_nats::jetstream::Context,
    messages: async_nats::jetstream::consumer::pull::Stream,
    quarantine_subject: String,
}

impl AgentRunConsumer {
    pub async fn bind(
        client: async_nats::Client,
        stream_name: &str,
        consumer_name: &str,
        quarantine_subject: &str,
    ) -> Result<Self, MessagingError> {
        if [stream_name, consumer_name, quarantine_subject]
            .iter()
            .any(|value| value.trim().is_empty() || value.chars().any(char::is_whitespace))
        {
            return Err(MessagingError::Configuration);
        }
        let context = async_nats::jetstream::new(client);
        let stream = context
            .get_stream(stream_name)
            .await
            .map_err(|_| MessagingError::Stream)?;
        let consumer: PullConsumer = stream
            .get_consumer(consumer_name)
            .await
            .map_err(|_| MessagingError::Consumer)?;
        let messages = consumer
            .messages()
            .await
            .map_err(|_| MessagingError::Consumer)?;
        Ok(Self {
            context,
            messages,
            quarantine_subject: quarantine_subject.to_owned(),
        })
    }

    /// Applies one event transactionally and acknowledges only after the Control commit.
    pub async fn process_next<A: AgentAuthority>(
        &mut self,
        control: &ControlService,
        agent: &A,
    ) -> Result<(), MessagingError> {
        let message = self
            .messages
            .next()
            .await
            .ok_or(MessagingError::Closed)?
            .map_err(|_| MessagingError::Receive)?;
        if message.payload.len() > MAX_EVENT_BYTES {
            self.quarantine(&message, None, "LW_EVENT_PAYLOAD_TOO_LARGE")
                .await?;
            message
                .double_ack_with(AckKind::Term)
                .await
                .map_err(|_| MessagingError::Ack)?;
            return Ok(());
        }
        let Ok(event): Result<CloudEvent<AgentRunEvent>, _> =
            serde_json::from_slice(&message.payload)
        else {
            self.quarantine(&message, None, "LW_EVENT_ENVELOPE_INVALID")
                .await?;
            message
                .double_ack_with(AckKind::Term)
                .await
                .map_err(|_| MessagingError::Ack)?;
            return Ok(());
        };
        let valid_contract = EVENT_CONTRACTS
            .iter()
            .copied()
            .find(|contract| contract.subject == event.subject);
        if valid_contract.is_none_or(|contract| event.validate(contract).is_err()) {
            self.quarantine(&message, Some(event.id), "LW_EVENT_ENVELOPE_INVALID")
                .await?;
            message
                .double_ack_with(AckKind::Term)
                .await
                .map_err(|_| MessagingError::Ack)?;
            return Ok(());
        }
        let (run, environment, evaluation) = if matches!(
            event.subject.as_str(),
            subjects::AGENT_RUN_COMPLETED | subjects::AGENT_RUN_FAILED
        ) {
            let Ok(outcome) = agent.outcome(event.data.run_id).await else {
                message
                    .ack_with(AckKind::Nak(Some(REDELIVERY_DELAY)))
                    .await
                    .map_err(|_| MessagingError::Ack)?;
                return Ok(());
            };
            if outcome.validate().is_err() {
                self.quarantine(
                    &message,
                    Some(event.id),
                    "LW_AGENT_OUTCOME_IDENTITY_INVALID",
                )
                .await?;
                message
                    .double_ack_with(AckKind::Term)
                    .await
                    .map_err(|_| MessagingError::Ack)?;
                return Ok(());
            }
            (
                outcome.run,
                outcome.environment_candidate,
                outcome.evaluation_candidate,
            )
        } else if event.subject == subjects::AGENT_RUN_REQUESTED {
            let run = if let Ok(run) = control.agent_run(event.course_id, event.data.run_id).await {
                run
            } else if let Ok(run) = agent.get(event.data.run_id).await {
                run
            } else {
                message
                    .ack_with(AckKind::Nak(Some(REDELIVERY_DELAY)))
                    .await
                    .map_err(|_| MessagingError::Ack)?;
                return Ok(());
            };
            (run, None, None)
        } else {
            self.quarantine(&message, Some(event.id), "LW_EVENT_SUBJECT_MISMATCH")
                .await?;
            message
                .double_ack_with(AckKind::Term)
                .await
                .map_err(|_| MessagingError::Ack)?;
            return Ok(());
        };
        match control
            .consume_agent_run_event(&event, &run, environment.as_ref(), evaluation.as_ref())
            .await
        {
            Ok(_) => message
                .double_ack()
                .await
                .map_err(|_| MessagingError::Ack)?,
            Err(ControlError::EventSequenceGap | ControlError::PersistenceFailed) => message
                .ack_with(AckKind::Nak(Some(REDELIVERY_DELAY)))
                .await
                .map_err(|_| MessagingError::Ack)?,
            Err(_) => {
                self.quarantine(&message, Some(event.id), "LW_AGENT_PROJECTION_CONFLICT")
                    .await?;
                message
                    .double_ack_with(AckKind::Term)
                    .await
                    .map_err(|_| MessagingError::Ack)?;
            }
        }
        Ok(())
    }

    async fn quarantine(
        &self,
        message: &async_nats::jetstream::Message,
        event_id: Option<EventId>,
        diagnostic: &'static str,
    ) -> Result<(), MessagingError> {
        let hash = Sha256Digest::of_bytes(&message.payload);
        let payload = serde_json::to_vec(&QuarantineRecord {
            version: 1,
            event_id,
            payload_sha256: hash,
            size_bytes: u64::try_from(message.payload.len())
                .map_err(|_| MessagingError::Quarantine)?,
            diagnostic_code: diagnostic,
        })
        .map_err(|_| MessagingError::Quarantine)?;
        let ack = self
            .context
            .send_publish(
                self.quarantine_subject.clone(),
                PublishMessage::build()
                    .payload(payload.into())
                    .message_id(format!("{hash}:{diagnostic}")),
            )
            .await
            .map_err(|_| MessagingError::Quarantine)?;
        ack.await.map_err(|_| MessagingError::Quarantine)?;
        Ok(())
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct QuarantineRecord {
    version: u8,
    event_id: Option<EventId>,
    payload_sha256: Sha256Digest,
    size_bytes: u64,
    diagnostic_code: &'static str,
}

#[derive(Debug, thiserror::Error)]
pub enum MessagingError {
    #[error("LW_NATS_CONFIG_INVALID")]
    Configuration,
    #[error("LW_NATS_CREDENTIALS_INVALID")]
    Credentials,
    #[error("LW_NATS_UNAVAILABLE")]
    Connect,
    #[error("LW_NATS_STREAM_UNAVAILABLE")]
    Stream,
    #[error("LW_NATS_CONSUMER_UNAVAILABLE")]
    Consumer,
    #[error("LW_NATS_CONSUMER_CLOSED")]
    Closed,
    #[error("LW_NATS_RECEIVE_FAILED")]
    Receive,
    #[error("LW_NATS_ACK_FAILED")]
    Ack,
    #[error("LW_NATS_QUARANTINE_FAILED")]
    Quarantine,
}
