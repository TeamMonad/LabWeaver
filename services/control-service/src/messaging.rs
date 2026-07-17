//! Durable Agent event consumption with authoritative mTLS readback.
#![allow(clippy::missing_errors_doc)]
#![allow(
    missing_docs,
    reason = "stable diagnostics document transport outcomes"
)]

use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use async_nats::jetstream::AckKind;
use async_nats::jetstream::consumer::PullConsumer;
use async_nats::jetstream::message::PublishMessage;
use async_trait::async_trait;
use contracts::events::{
    AgentBuildCompletedV2, AgentBuildFailedV2, AgentRunEvent, CloudEvent, EVENT_CONTRACTS, subjects,
};
use contracts::{EventId, ImageArtifactId, Sha256Digest};
use futures_util::StreamExt;
use serde::Serialize;
use serde_json::Value;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::clients::{AgentClient, DownstreamError};
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

/// Agent-owned immutable artifact readback used by build completion projection.
#[async_trait]
pub trait BuildArtifactAuthority: Send + Sync {
    async fn artifact(
        &self,
        artifact_id: ImageArtifactId,
    ) -> Result<contracts::http::InternalImageArtifactResolution, crate::clients::DownstreamError>;
}

#[async_trait]
impl BuildArtifactAuthority for AgentClient {
    async fn artifact(
        &self,
        artifact_id: ImageArtifactId,
    ) -> Result<contracts::http::InternalImageArtifactResolution, crate::clients::DownstreamError>
    {
        AgentClient::artifact(self, artifact_id).await
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

/// Bounded Control Outbox publisher that marks rows only after a `JetStream` ACK.
pub struct ControlOutboxDispatcher {
    pool: PgPool,
    context: async_nats::jetstream::Context,
    timeout: Duration,
}

impl ControlOutboxDispatcher {
    pub fn new(
        pool: PgPool,
        client: async_nats::Client,
        timeout: Duration,
    ) -> Result<Self, MessagingError> {
        if timeout.is_zero() || timeout > Duration::from_secs(300) {
            return Err(MessagingError::Configuration);
        }
        Ok(Self {
            pool,
            context: async_nats::jetstream::new(client),
            timeout,
        })
    }

    pub async fn dispatch_once(&self) -> Result<bool, MessagingError> {
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query(
            "SELECT event_id,subject,event_type,payload,payload_sha256 \
             FROM control.outbox_events WHERE published_at IS NULL \
             ORDER BY created_at,event_id FOR UPDATE SKIP LOCKED LIMIT 1",
        )
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(row) = row else {
            transaction.rollback().await?;
            return Ok(false);
        };
        let event_uuid: Uuid = row.try_get("event_id")?;
        let subject: String = row.try_get("subject")?;
        let event_type: String = row.try_get("event_type")?;
        let payload: Value = row.try_get("payload")?;
        let stored_hash: String = row.try_get("payload_sha256")?;
        let hash = Sha256Digest::of_canonical(&payload).map_err(|_| MessagingError::Identity)?;
        if hash.to_string() != stored_hash {
            return Err(MessagingError::Identity);
        }
        let event: CloudEvent<Value> =
            serde_json::from_value(payload).map_err(|_| MessagingError::Contract)?;
        let event_id =
            EventId::from_str(&event_uuid.to_string()).map_err(|_| MessagingError::Identity)?;
        let contract = EVENT_CONTRACTS
            .iter()
            .copied()
            .find(|contract| contract.subject == subject)
            .ok_or(MessagingError::Contract)?;
        if event.id != event_id || event_type != subject || event.validate(contract).is_err() {
            return Err(MessagingError::Contract);
        }
        let bytes = serde_json::to_vec(&event).map_err(|_| MessagingError::Contract)?;
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
        .map_err(|_| MessagingError::PublishTimeout)?
        .map_err(|_| MessagingError::Publish)?;
        tokio::time::timeout(self.timeout, publish)
            .await
            .map_err(|_| MessagingError::PublishTimeout)?
            .map_err(|_| MessagingError::Publish)?;
        let updated = sqlx::query(
            "UPDATE control.outbox_events \
             SET published_at=date_trunc('milliseconds',clock_timestamp()) \
             WHERE event_id=$1 AND published_at IS NULL",
        )
        .bind(event_uuid)
        .execute(&mut *transaction)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(MessagingError::Fence);
        }
        transaction.commit().await?;
        Ok(true)
    }
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

/// Dedicated completion/failure consumer; deployment filtering must match only Agent build events.
pub struct AgentBuildConsumer {
    context: async_nats::jetstream::Context,
    messages: async_nats::jetstream::consumer::pull::Stream,
    quarantine_subject: String,
}

impl AgentBuildConsumer {
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
        let mut filters = consumer.cached_info().config.filter_subjects.clone();
        filters.sort_unstable();
        let mut expected_filters = vec![
            subjects::AGENT_BUILD_COMPLETED_V2.to_owned(),
            subjects::AGENT_BUILD_FAILED_V2.to_owned(),
        ];
        expected_filters.sort_unstable();
        if !consumer.cached_info().config.filter_subject.is_empty() || filters != expected_filters {
            return Err(MessagingError::Configuration);
        }
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

    pub async fn process_next<A: BuildArtifactAuthority>(
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
        let Ok(event): Result<CloudEvent<Value>, _> = serde_json::from_slice(&message.payload)
        else {
            self.quarantine(&message, None, "LW_EVENT_ENVELOPE_INVALID")
                .await?;
            message
                .double_ack_with(AckKind::Term)
                .await
                .map_err(|_| MessagingError::Ack)?;
            return Ok(());
        };
        let Some(contract) = EVENT_CONTRACTS
            .iter()
            .copied()
            .find(|contract| contract.subject == event.subject)
        else {
            self.quarantine(&message, Some(event.id), "LW_EVENT_SUBJECT_MISMATCH")
                .await?;
            message
                .double_ack_with(AckKind::Term)
                .await
                .map_err(|_| MessagingError::Ack)?;
            return Ok(());
        };
        if event.validate(contract).is_err() {
            self.quarantine(&message, Some(event.id), "LW_EVENT_ENVELOPE_INVALID")
                .await?;
            message
                .double_ack_with(AckKind::Term)
                .await
                .map_err(|_| MessagingError::Ack)?;
            return Ok(());
        }
        match event.subject.as_str() {
            subjects::AGENT_BUILD_COMPLETED_V2 => {
                let Ok(data): Result<AgentBuildCompletedV2, _> =
                    serde_json::from_value(event.data.clone())
                else {
                    self.quarantine(
                        &message,
                        Some(event.id),
                        "LW_AGENT_BUILD_OUTCOME_IDENTITY_INVALID",
                    )
                    .await?;
                    message
                        .double_ack_with(AckKind::Term)
                        .await
                        .map_err(|_| MessagingError::Ack)?;
                    return Ok(());
                };
                let resolution = match agent.artifact(data.artifact_id).await {
                    Ok(resolution) => resolution,
                    Err(DownstreamError::Unavailable) => {
                        message
                            .ack_with(AckKind::Nak(Some(REDELIVERY_DELAY)))
                            .await
                            .map_err(|_| MessagingError::Ack)?;
                        return Ok(());
                    }
                    Err(DownstreamError::Configuration | DownstreamError::Denied) => {
                        return Err(MessagingError::ArtifactAuthority);
                    }
                    Err(
                        DownstreamError::ProtocolInvalid
                        | DownstreamError::IdentityMismatch
                        | DownstreamError::NotFound
                        | DownstreamError::Conflict,
                    ) => {
                        self.quarantine(
                            &message,
                            Some(event.id),
                            "LW_AGENT_BUILD_ARTIFACT_READBACK_REJECTED",
                        )
                        .await?;
                        message
                            .double_ack_with(AckKind::Term)
                            .await
                            .map_err(|_| MessagingError::Ack)?;
                        return Ok(());
                    }
                };
                if resolution.validate().is_err()
                    || resolution.artifact_id != data.artifact_id
                    || resolution.artifact.content_sha256().ok() != Some(data.artifact_sha256)
                    || canonical_hash(&resolution.policy_evaluation)?
                        != data.policy_evaluation_sha256
                    || container_build_request_id(&resolution.artifact)
                        != Some(data.build_request_id)
                {
                    self.quarantine(
                        &message,
                        Some(event.id),
                        "LW_AGENT_BUILD_OUTCOME_IDENTITY_INVALID",
                    )
                    .await?;
                    message
                        .double_ack_with(AckKind::Term)
                        .await
                        .map_err(|_| MessagingError::Ack)?;
                    return Ok(());
                }
                match control
                    .project_artifact(
                        event.id,
                        event.course_id,
                        &resolution.artifact,
                        &resolution.policy_evaluation,
                    )
                    .await
                {
                    Ok(()) => message
                        .double_ack()
                        .await
                        .map_err(|_| MessagingError::Ack)?,
                    Err(crate::ControlError::PersistenceFailed) => message
                        .ack_with(AckKind::Nak(Some(REDELIVERY_DELAY)))
                        .await
                        .map_err(|_| MessagingError::Ack)?,
                    Err(_) => {
                        self.quarantine(
                            &message,
                            Some(event.id),
                            "LW_AGENT_BUILD_PROJECTION_REJECTED",
                        )
                        .await?;
                        message
                            .double_ack_with(AckKind::Term)
                            .await
                            .map_err(|_| MessagingError::Ack)?;
                    }
                }
            }
            subjects::AGENT_BUILD_FAILED_V2 => {
                let Ok(data): Result<AgentBuildFailedV2, _> =
                    serde_json::from_value(event.data.clone())
                else {
                    self.quarantine(
                        &message,
                        Some(event.id),
                        "LW_AGENT_BUILD_OUTCOME_IDENTITY_INVALID",
                    )
                    .await?;
                    message
                        .double_ack_with(AckKind::Term)
                        .await
                        .map_err(|_| MessagingError::Ack)?;
                    return Ok(());
                };
                if data.validate().is_err() {
                    self.quarantine(
                        &message,
                        Some(event.id),
                        "LW_AGENT_BUILD_OUTCOME_IDENTITY_INVALID",
                    )
                    .await?;
                    message
                        .double_ack_with(AckKind::Term)
                        .await
                        .map_err(|_| MessagingError::Ack)?;
                } else if let Err(error) = control
                    .project_build_failure(event.id, event.course_id, &data)
                    .await
                {
                    if matches!(error, crate::ControlError::PersistenceFailed) {
                        message
                            .ack_with(AckKind::Nak(Some(REDELIVERY_DELAY)))
                            .await
                            .map_err(|_| MessagingError::Ack)?;
                        return Ok(());
                    }
                    self.quarantine(
                        &message,
                        Some(event.id),
                        "LW_AGENT_BUILD_PROJECTION_REJECTED",
                    )
                    .await?;
                    message
                        .double_ack_with(AckKind::Term)
                        .await
                        .map_err(|_| MessagingError::Ack)?;
                } else {
                    tracing::warn!(
                        event = "control.agent_build.failed",
                        build_request_id = %data.build_request_id,
                        diagnostic_code = %data.diagnostic_code,
                        retryable = data.retryable,
                        cleanup_verified = data.cleanup_verified,
                    );
                    message
                        .double_ack()
                        .await
                        .map_err(|_| MessagingError::Ack)?;
                }
            }
            _ => {
                self.quarantine(&message, Some(event.id), "LW_EVENT_SUBJECT_MISMATCH")
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
        diagnostic_code: &'static str,
    ) -> Result<(), MessagingError> {
        let payload_sha256 = Sha256Digest::of_bytes(&message.payload);
        let record = QuarantineRecord {
            version: 1,
            event_id,
            payload_sha256,
            size_bytes: u64::try_from(message.payload.len())
                .map_err(|_| MessagingError::Quarantine)?,
            diagnostic_code,
        };
        let payload = serde_json::to_vec(&record).map_err(|_| MessagingError::Quarantine)?;
        let acknowledgement = self
            .context
            .send_publish(
                self.quarantine_subject.clone(),
                PublishMessage::build()
                    .payload(payload.into())
                    .message_id(format!("{payload_sha256}:{diagnostic_code}")),
            )
            .await
            .map_err(|_| MessagingError::Quarantine)?;
        acknowledgement
            .await
            .map_err(|_| MessagingError::Quarantine)?;
        Ok(())
    }
}

fn canonical_hash<T: Serialize>(value: &T) -> Result<Sha256Digest, MessagingError> {
    Sha256Digest::of_canonical(value).map_err(|_| MessagingError::Contract)
}

fn container_build_request_id(
    artifact: &contracts::supply_chain::ImageArtifact,
) -> Option<contracts::BuildRequestId> {
    match artifact {
        contracts::supply_chain::ImageArtifact::Container {
            build_request_id, ..
        } => Some(*build_request_id),
        contracts::supply_chain::ImageArtifact::VirtualMachine { .. } => None,
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
    #[error("LW_CONTROL_AGENT_ARTIFACT_AUTHORITY_INVALID")]
    ArtifactAuthority,
    #[error("LW_NATS_QUARANTINE_FAILED")]
    Quarantine,
    #[error("LW_CONTROL_OUTBOX_IDENTITY_INVALID")]
    Identity,
    #[error("LW_CONTROL_OUTBOX_CONTRACT_INVALID")]
    Contract,
    #[error("LW_CONTROL_OUTBOX_PUBLISH_FAILED")]
    Publish,
    #[error("LW_CONTROL_OUTBOX_PUBLISH_TIMEOUT")]
    PublishTimeout,
    #[error("LW_CONTROL_OUTBOX_FENCE_LOST")]
    Fence,
    #[error("LW_CONTROL_OUTBOX_DATABASE_FAILED")]
    Database(#[from] sqlx::Error),
}

impl MessagingError {
    /// Transport and database outages retain the Outbox row and are safe to retry.
    #[must_use]
    pub const fn retryable(&self) -> bool {
        matches!(
            self,
            Self::Publish | Self::PublishTimeout | Self::Database(_)
        )
    }
}
