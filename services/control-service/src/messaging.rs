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
use contracts::evaluation::{EvaluationRelease, EvaluationReleaseState};
use contracts::events::{
    AgentBuildCompleted, AgentBuildFailed, AgentRunEvent, AuthoringApprovalCompleted, CloudEvent,
    EVENT_CONTRACTS, subjects,
};
use contracts::http::{
    GeneratedArtifactKind, GeneratedArtifactQuery, GeneratedArtifactRecord, IdempotencyKey,
    InternalPublishEvaluationReleaseRequest,
};
use contracts::{
    ApprovalId, DiagnosticCode, EventId, ImageArtifactId, ProjectId, Revision, UtcTimestamp,
};
use futures_util::StreamExt;
use persistence_sqlx::Sha256Digest; // internal persistence hash, not contract hash
use reqwest::header::HeaderMap;
use serde::Serialize;
use serde_json::Value;
use sqlx::{PgPool, Row};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::clients::{AgentClient, DownstreamError, EvaluationClient};
use crate::{AuthoringPublicationClaim, ControlError, ControlService};

const MAX_EVENT_BYTES: usize = 1024 * 1024;
const REDELIVERY_DELAY: Duration = Duration::from_secs(1);
const AGENT_STREAM_SUBJECT_PREFIX: &str = "labweaver.agent.";
const CONTROL_STREAM_SUBJECT_PREFIX: &str = "labweaver.control.";

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

    async fn generated_artifact(
        &self,
        artifact_id: contracts::ArtifactId,
        query: &GeneratedArtifactQuery,
    ) -> Result<GeneratedArtifactRecord, crate::clients::DownstreamError>;
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

    async fn generated_artifact(
        &self,
        artifact_id: contracts::ArtifactId,
        query: &GeneratedArtifactQuery,
    ) -> Result<GeneratedArtifactRecord, crate::clients::DownstreamError> {
        AgentClient::generated_artifact(self, artifact_id, query).await
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

enum ContextResolutionError {
    Retryable,
    Rejected,
}

async fn resolve_generated_context<A: AgentAuthority>(
    control: &ControlService,
    agent: &A,
    run: &contracts::authoring::AgentRun,
    environment: Option<&contracts::authoring::EnvironmentCandidate>,
) -> Result<Option<GeneratedArtifactRecord>, ContextResolutionError> {
    let Some(candidate) = environment else {
        return Ok(None);
    };
    let contracts::authoring::EnvironmentRuntimeSpec::Container { build_context, .. } =
        &candidate.spec.runtime
    else {
        return Ok(None);
    };
    let package = match control
        .project_package(run.project_id, run.package_id)
        .await
    {
        Ok(package) => package,
        Err(ControlError::PersistenceFailed) => return Err(ContextResolutionError::Retryable),
        Err(_) => return Err(ContextResolutionError::Rejected),
    };
    if package.validate().is_err()
        || package.project_id != run.project_id
        || package.course_id != run.course_id
    {
        return Err(ContextResolutionError::Rejected);
    }
    if package
        .files
        .iter()
        .any(|file| file.object == *build_context)
    {
        return Ok(None);
    }
    let query = GeneratedArtifactQuery {
        project_id: run.project_id,
        course_id: run.course_id,
        package_id: run.package_id,
        package_revision: package.revision,
    };
    let record = match agent
        .generated_artifact(build_context.artifact_id, &query)
        .await
    {
        Ok(record) => record,
        Err(DownstreamError::Unavailable) => return Err(ContextResolutionError::Retryable),
        Err(_) => return Err(ContextResolutionError::Rejected),
    };
    if record.kind != GeneratedArtifactKind::BuildContext
        || record.artifact != *build_context
        || record.project_id != package.project_id
        || record.course_id != package.course_id
        || record.package_id != package.id
        || record.package_revision != package.revision
    {
        return Err(ContextResolutionError::Rejected);
    }
    Ok(Some(record))
}

/// Evaluation-owned release authority used by the authoring publication worker.
#[async_trait]
pub trait EvaluationAuthority: Send + Sync {
    async fn publish(
        &self,
        request: &InternalPublishEvaluationReleaseRequest,
        key: &IdempotencyKey,
        headers: &HeaderMap,
    ) -> Result<EvaluationRelease, DownstreamError>;
}

#[async_trait]
impl EvaluationAuthority for EvaluationClient {
    async fn publish(
        &self,
        request: &InternalPublishEvaluationReleaseRequest,
        key: &IdempotencyKey,
        headers: &HeaderMap,
    ) -> Result<EvaluationRelease, DownstreamError> {
        EvaluationClient::publish(self, request, key, headers).await
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
        if timeout.is_zero() || timeout > Duration::from_mins(5) {
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
            || !quarantine_subject.starts_with(AGENT_STREAM_SUBJECT_PREFIX)
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
            let run = if let Some(course_id) = event.course_id {
                if let Ok(run) = control.agent_run(course_id, event.data.run_id).await {
                    run
                } else if let Ok(run) = agent.get(event.data.run_id).await {
                    run
                } else {
                    message
                        .ack_with(AckKind::Nak(Some(REDELIVERY_DELAY)))
                        .await
                        .map_err(|_| MessagingError::Ack)?;
                    return Ok(());
                }
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
        let generated_context = match resolve_generated_context(
            control,
            agent,
            &run,
            environment.as_ref(),
        )
        .await
        {
            Ok(record) => record,
            Err(ContextResolutionError::Retryable) => {
                message
                    .ack_with(AckKind::Nak(Some(REDELIVERY_DELAY)))
                    .await
                    .map_err(|_| MessagingError::Ack)?;
                return Ok(());
            }
            Err(ContextResolutionError::Rejected) => {
                tracing::error!(
                    event = "control.agent_run_context_resolution_rejected",
                    component = "control-service",
                    operation = "agent_run.context.resolve",
                    outcome = "quarantined",
                    duration_ms = 0_u64,
                    event_id = %event.id,
                    run_id = %run.id,
                    candidate_id = environment.as_ref().map(|candidate| candidate.id.to_string()),
                    diagnostic_code = "LW_AGENT_BUILD_CONTEXT_READBACK_REJECTED",
                    failure_stage = "agent_run.context.resolve",
                    retryable = false,
                );
                self.quarantine(
                    &message,
                    Some(event.id),
                    "LW_AGENT_BUILD_CONTEXT_READBACK_REJECTED",
                )
                .await?;
                message
                    .double_ack_with(AckKind::Term)
                    .await
                    .map_err(|_| MessagingError::Ack)?;
                return Ok(());
            }
        };
        match control
            .consume_agent_run_event(
                &event,
                &run,
                environment.as_ref(),
                evaluation.as_ref(),
                generated_context.as_ref(),
            )
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
            Err(error) => {
                tracing::error!(
                    event = "control.agent_run_projection_conflict",
                    component = "control-service",
                    operation = "agent_run.projection",
                    outcome = "quarantined",
                    duration_ms = 0_u64,
                    event_id = %event.id,
                    run_id = %event.data.run_id,
                    diagnostic_code = "LW_AGENT_PROJECTION_CONFLICT",
                    error_kind = %format!("{error}"),
                    failure_stage = "agent_run.projection.consume",
                    retryable = false,
                );
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
        diagnostic: &str,
    ) -> Result<(), MessagingError> {
        let hash = Sha256Digest::of_bytes(&message.payload);
        let payload = serde_json::to_vec(&QuarantineRecord {
            version: 1,
            event_id,
            payload_sha256: hash,
            size_bytes: u64::try_from(message.payload.len())
                .map_err(|_| MessagingError::Quarantine)?,
            diagnostic_code: diagnostic.to_owned(),
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

/// Durable authoring publication coordinator.  The approval event is only a trigger; the
/// immutable approval and all release inputs are read from Control before downstream calls.
pub struct AuthoringPublicationConsumer {
    context: async_nats::jetstream::Context,
    messages: async_nats::jetstream::consumer::pull::Stream,
    quarantine_subject: String,
    max_deliver: i64,
}

impl AuthoringPublicationConsumer {
    pub async fn bind(
        client: async_nats::Client,
        stream_name: &str,
        consumer_name: &str,
        quarantine_subject: &str,
    ) -> Result<Self, MessagingError> {
        if [stream_name, consumer_name, quarantine_subject]
            .iter()
            .any(|value| value.trim().is_empty() || value.chars().any(char::is_whitespace))
            || !quarantine_subject.starts_with(CONTROL_STREAM_SUBJECT_PREFIX)
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
        let filters = consumer.cached_info().config.filter_subjects.clone();
        if !consumer.cached_info().config.filter_subject.is_empty()
            || filters != vec![subjects::AUTHORING_APPROVAL_COMPLETED.to_owned()]
        {
            return Err(MessagingError::Configuration);
        }
        let max_deliver = consumer.cached_info().config.max_deliver;
        if max_deliver <= 0 {
            return Err(MessagingError::AuthoringDeliveryLimit);
        }
        let messages = consumer
            .messages()
            .await
            .map_err(|_| MessagingError::Consumer)?;
        Ok(Self {
            context,
            messages,
            quarantine_subject: quarantine_subject.to_owned(),
            max_deliver,
        })
    }

    /// Runs one publication trigger and acknowledges it only after the durable status transition.
    pub async fn process_next<E: EvaluationAuthority>(
        &mut self,
        control: &ControlService,
        evaluation: &E,
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
        let Ok(event): Result<CloudEvent<AuthoringApprovalCompleted>, _> =
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
        if contract.subject != subjects::AUTHORING_APPROVAL_COMPLETED
            || event.validate(contract).is_err()
            || event.data.validate().is_err()
            || event.project_id != event.data.approval.project_id
            || event.course_id != event.data.approval.course_id
            || event.aggregate_revision != event.data.approval.revision
            || event.aggregate_sequence.0 != 1
        {
            self.quarantine(&message, Some(event.id), "LW_EVENT_ENVELOPE_INVALID")
                .await?;
            message
                .double_ack_with(AckKind::Term)
                .await
                .map_err(|_| MessagingError::Ack)?;
            return Ok(());
        }

        let now = current_timestamp()?;
        let approval = event.data.approval;
        let claim = match control.claim_authoring_publication(&approval, now).await {
            Ok(claim) => claim,
            Err(error) if retryable_control_error(&error) => {
                let diagnostic = diagnostic_code(&error.to_string());
                self.handle_publication_failure(
                    control,
                    &message,
                    event.id,
                    approval.id,
                    approval.project_id,
                    "authoring_publication.claim",
                    diagnostic,
                    true,
                    now,
                )
                .await?;
                return Ok(());
            }
            Err(error) => {
                let diagnostic = diagnostic_code(&error.to_string());
                self.handle_publication_failure(
                    control,
                    &message,
                    event.id,
                    approval.id,
                    approval.project_id,
                    "authoring_publication.claim",
                    diagnostic,
                    false,
                    now,
                )
                .await?;
                return Ok(());
            }
        };
        if matches!(claim, AuthoringPublicationClaim::AlreadyReady(_)) {
            message
                .double_ack()
                .await
                .map_err(|_| MessagingError::Ack)?;
            return Ok(());
        }

        let environment_release = match control
            .publish_authoring_environment_release(&approval, now, &event.trace_id)
            .await
        {
            Ok(release) => release,
            Err(error) if retryable_control_error(&error) => {
                let diagnostic = diagnostic_code(&error.to_string());
                self.handle_publication_failure(
                    control,
                    &message,
                    event.id,
                    approval.id,
                    approval.project_id,
                    "authoring_publication.environment_release.publish",
                    diagnostic,
                    true,
                    now,
                )
                .await?;
                return Ok(());
            }
            Err(error) => {
                let diagnostic = diagnostic_code(&error.to_string());
                self.handle_publication_failure(
                    control,
                    &message,
                    event.id,
                    approval.id,
                    approval.project_id,
                    "authoring_publication.environment_release.publish",
                    diagnostic,
                    false,
                    now,
                )
                .await?;
                return Ok(());
            }
        };

        let request = match control
            .prepare_authoring_evaluation_release(&approval)
            .await
        {
            Ok(request) => request,
            Err(error) if retryable_control_error(&error) => {
                let diagnostic = diagnostic_code(&error.to_string());
                self.handle_publication_failure(
                    control,
                    &message,
                    event.id,
                    approval.id,
                    approval.project_id,
                    "authoring_publication.evaluation_release.prepare",
                    diagnostic,
                    true,
                    now,
                )
                .await?;
                return Ok(());
            }
            Err(error) => {
                let diagnostic = diagnostic_code(&error.to_string());
                self.handle_publication_failure(
                    control,
                    &message,
                    event.id,
                    approval.id,
                    approval.project_id,
                    "authoring_publication.evaluation_release.prepare",
                    diagnostic,
                    false,
                    now,
                )
                .await?;
                return Ok(());
            }
        };
        let key_value = format!("authoring-publication:{}", approval.id);
        let key =
            IdempotencyKey::parse(key_value.as_str()).map_err(|_| MessagingError::Contract)?;
        let evaluation_release = match evaluation.publish(&request, &key, &HeaderMap::new()).await {
            Ok(release) => release,
            Err(DownstreamError::Unavailable) => {
                self.handle_publication_failure(
                    control,
                    &message,
                    event.id,
                    approval.id,
                    approval.project_id,
                    "authoring_publication.evaluation_release.publish",
                    DiagnosticCode::registered("LW_EVALUATION_RELEASE_PUBLISH_UNAVAILABLE"),
                    true,
                    now,
                )
                .await?;
                return Ok(());
            }
            Err(error) => {
                let diagnostic = diagnostic_code(&error.to_string());
                self.handle_publication_failure(
                    control,
                    &message,
                    event.id,
                    approval.id,
                    approval.project_id,
                    "authoring_publication.evaluation_release.publish",
                    diagnostic,
                    false,
                    now,
                )
                .await?;
                return Ok(());
            }
        };
        if evaluation_release.validate().is_err()
            || evaluation_release.project_id != approval.project_id
            || evaluation_release.course_id != approval.course_id
            || evaluation_release.candidate_id != approval.evaluation_candidate_id
            || evaluation_release.candidate_revision != approval.evaluation_candidate_revision
            || evaluation_release.approval_id != approval.id
            || evaluation_release.approval_revision != approval.revision
            || evaluation_release.state != EvaluationReleaseState::Active
            || evaluation_release.revision
                != Revision::new(1).map_err(|_| MessagingError::Contract)?
        {
            self.handle_publication_failure(
                control,
                &message,
                event.id,
                approval.id,
                approval.project_id,
                "authoring_publication.evaluation_release.validate",
                DiagnosticCode::registered("LW_AUTHORING_EVALUATION_RELEASE_IDENTITY_INVALID"),
                false,
                now,
            )
            .await?;
            return Ok(());
        }
        match control
            .complete_authoring_publication(
                approval.id,
                approval.project_id,
                &environment_release,
                &evaluation_release,
                current_timestamp()?,
            )
            .await
        {
            Ok(()) => message
                .double_ack()
                .await
                .map_err(|_| MessagingError::Ack)?,
            Err(error) => {
                let diagnostic = diagnostic_code(&error.to_string());
                self.handle_publication_failure(
                    control,
                    &message,
                    event.id,
                    approval.id,
                    approval.project_id,
                    "authoring_publication.complete",
                    diagnostic,
                    retryable_control_error(&error),
                    now,
                )
                .await?;
            }
        }
        Ok(())
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "publication failure handling keeps stage and aggregate identity together"
    )]
    async fn handle_publication_failure(
        &self,
        control: &ControlService,
        message: &async_nats::jetstream::Message,
        event_id: EventId,
        approval_id: ApprovalId,
        project_id: ProjectId,
        stage: &'static str,
        diagnostic: DiagnosticCode,
        retryable: bool,
        now: UtcTimestamp,
    ) -> Result<(), MessagingError> {
        let delivery_attempt = Self::delivery_attempt(message)?;
        let retry_action = publication_retry_action(delivery_attempt, self.max_deliver)?;
        if retryable && retry_action == PublicationRetryAction::Retry {
            tracing::warn!(
                event = "control.authoring_publication.retry_scheduled",
                component = "control-service",
                failure_stage = stage,
                event_id = %event_id,
                approval_id = %approval_id,
                project_id = %project_id,
                diagnostic_code = diagnostic.as_str(),
                delivery_attempt,
                max_delivery_attempt = self.max_deliver,
                retryable = true,
                outcome = "retry_scheduled",
            );
            message
                .ack_with(AckKind::Nak(Some(REDELIVERY_DELAY)))
                .await
                .map_err(|_| MessagingError::Ack)?;
            return Ok(());
        }
        tracing::error!(
            event = "control.authoring_publication.terminal_failure",
            component = "control-service",
            failure_stage = stage,
            event_id = %event_id,
            approval_id = %approval_id,
            project_id = %project_id,
            diagnostic_code = diagnostic.as_str(),
            delivery_attempt,
            max_delivery_attempt = self.max_deliver,
            retryable,
            outcome = "persisting_failed",
        );
        match control
            .fail_authoring_publication(approval_id, project_id, diagnostic.clone(), now)
            .await
        {
            Ok(()) => {
                tracing::error!(
                    event = "control.authoring_publication.failed",
                    component = "control-service",
                    failure_stage = stage,
                    event_id = %event_id,
                    approval_id = %approval_id,
                    project_id = %project_id,
                    diagnostic_code = diagnostic.as_str(),
                    delivery_attempt,
                    max_delivery_attempt = self.max_deliver,
                    retryable,
                    outcome = "failed",
                );
                self.quarantine(message, Some(event_id), diagnostic.as_str())
                    .await?;
                message
                    .double_ack_with(AckKind::Term)
                    .await
                    .map_err(|_| MessagingError::Ack)?;
                Ok(())
            }
            Err(error)
                if retryable_control_error(&error)
                    && retry_action == PublicationRetryAction::Retry =>
            {
                let failure_diagnostic = diagnostic_code(&error.to_string());
                tracing::warn!(
                    event = "control.authoring_publication.failure_persist_retry",
                    component = "control-service",
                    operation = stage,
                    event_id = %event_id,
                    approval_id = %approval_id,
                    project_id = %project_id,
                    diagnostic_code = failure_diagnostic.as_str(),
                    delivery_attempt,
                    max_delivery_attempt = self.max_deliver,
                    retryable = true,
                    outcome = "retry_scheduled",
                    failure_stage = "authoring_publication.failure_persist",
                );
                message
                    .ack_with(AckKind::Nak(Some(REDELIVERY_DELAY)))
                    .await
                    .map_err(|_| MessagingError::Ack)?;
                Ok(())
            }
            Err(error) => {
                let failure_diagnostic = diagnostic_code(&error.to_string());
                tracing::error!(
                    event = "control.authoring_publication.failure_persist_failed",
                    component = "control-service",
                    operation = stage,
                    event_id = %event_id,
                    approval_id = %approval_id,
                    project_id = %project_id,
                    diagnostic_code = failure_diagnostic.as_str(),
                    delivery_attempt,
                    max_delivery_attempt = self.max_deliver,
                    retryable,
                    outcome = "consumer_failed",
                    failure_stage = "authoring_publication.failure_persist",
                );
                Err(MessagingError::AuthoringFailurePersistence)
            }
        }
    }

    fn delivery_attempt(message: &async_nats::jetstream::Message) -> Result<i64, MessagingError> {
        let delivered = message
            .info()
            .map_err(|_| MessagingError::DeliveryInfo)?
            .delivered;
        if delivered <= 0 {
            return Err(MessagingError::DeliveryInfo);
        }
        Ok(delivered)
    }

    async fn quarantine(
        &self,
        message: &async_nats::jetstream::Message,
        event_id: Option<EventId>,
        diagnostic: &str,
    ) -> Result<(), MessagingError> {
        let payload_sha256 = Sha256Digest::of_bytes(&message.payload);
        let record = QuarantineRecord {
            version: 1,
            event_id,
            payload_sha256,
            size_bytes: u64::try_from(message.payload.len())
                .map_err(|_| MessagingError::Quarantine)?,
            diagnostic_code: diagnostic.to_owned(),
        };
        let payload = serde_json::to_vec(&record).map_err(|_| MessagingError::Quarantine)?;
        let acknowledgement = self
            .context
            .send_publish(
                self.quarantine_subject.clone(),
                PublishMessage::build()
                    .payload(payload.into())
                    .message_id(format!("{payload_sha256}:{diagnostic}")),
            )
            .await
            .map_err(|_| MessagingError::Quarantine)?;
        acknowledgement
            .await
            .map_err(|_| MessagingError::Quarantine)?;
        Ok(())
    }
}

fn current_timestamp() -> Result<UtcTimestamp, MessagingError> {
    let value = OffsetDateTime::now_utc();
    let value = value
        .replace_nanosecond((value.nanosecond() / 1_000_000) * 1_000_000)
        .map_err(|_| MessagingError::Clock)?;
    UtcTimestamp::from_utc(value).map_err(|_| MessagingError::Clock)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PublicationRetryAction {
    Retry,
    Fail,
}

fn publication_retry_action(
    delivery_attempt: i64,
    max_deliver: i64,
) -> Result<PublicationRetryAction, MessagingError> {
    if delivery_attempt <= 0 || max_deliver <= 0 {
        return Err(MessagingError::DeliveryInfo);
    }
    Ok(if delivery_attempt < max_deliver {
        PublicationRetryAction::Retry
    } else {
        PublicationRetryAction::Fail
    })
}

fn retryable_control_error(error: &ControlError) -> bool {
    matches!(
        error,
        ControlError::PersistenceFailed
            | ControlError::ObjectStore(_)
            | ControlError::OperationInProgress
            | ControlError::OperationLeaseLost
            | ControlError::ArtifactNotAuthoritative
    )
}

fn diagnostic_code(error: &str) -> DiagnosticCode {
    let value = error.split(':').next().unwrap_or(error);
    DiagnosticCode::parse(value)
        .unwrap_or_else(|_| DiagnosticCode::registered("LW_AUTHORING_PUBLICATION_FAILED"))
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
            || !quarantine_subject.starts_with(AGENT_STREAM_SUBJECT_PREFIX)
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
            subjects::AGENT_BUILD_COMPLETED.to_owned(),
            subjects::AGENT_BUILD_FAILED.to_owned(),
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
            subjects::AGENT_BUILD_COMPLETED => {
                let Ok(data): Result<AgentBuildCompleted, _> =
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
                        event.project_id,
                        event.course_id,
                        &resolution.artifact,
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
            subjects::AGENT_BUILD_FAILED => {
                let Ok(data): Result<AgentBuildFailed, _> =
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
                    .project_build_failure(event.id, event.project_id, event.course_id, &data)
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
        diagnostic_code: &str,
    ) -> Result<(), MessagingError> {
        let payload_sha256 = Sha256Digest::of_bytes(&message.payload);
        let record = QuarantineRecord {
            version: 1,
            event_id,
            payload_sha256,
            size_bytes: u64::try_from(message.payload.len())
                .map_err(|_| MessagingError::Quarantine)?,
            diagnostic_code: diagnostic_code.to_owned(),
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

#[allow(dead_code)]
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
    diagnostic_code: String,
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
    #[error("LW_CONTROL_AUTHORING_CONSUMER_DELIVERY_LIMIT_INVALID")]
    AuthoringDeliveryLimit,
    #[error("LW_NATS_DELIVERY_INFO_INVALID")]
    DeliveryInfo,
    #[error("LW_CONTROL_CLOCK_INVALID")]
    Clock,
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
    #[error("LW_CONTROL_AUTHORING_PUBLICATION_FAILURE_PERSIST_FAILED")]
    AuthoringFailurePersistence,
}

impl MessagingError {
    /// Returns the closed diagnostic without formatting wrapped infrastructure errors.
    #[must_use]
    pub const fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::Configuration => "LW_NATS_CONFIG_INVALID",
            Self::Credentials => "LW_NATS_CREDENTIALS_INVALID",
            Self::Connect => "LW_NATS_UNAVAILABLE",
            Self::Stream => "LW_NATS_STREAM_UNAVAILABLE",
            Self::Consumer => "LW_NATS_CONSUMER_UNAVAILABLE",
            Self::Closed => "LW_NATS_CONSUMER_CLOSED",
            Self::AuthoringDeliveryLimit => "LW_CONTROL_AUTHORING_CONSUMER_DELIVERY_LIMIT_INVALID",
            Self::DeliveryInfo => "LW_NATS_DELIVERY_INFO_INVALID",
            Self::Clock => "LW_CONTROL_CLOCK_INVALID",
            Self::Receive => "LW_NATS_RECEIVE_FAILED",
            Self::Ack => "LW_NATS_ACK_FAILED",
            Self::ArtifactAuthority => "LW_CONTROL_AGENT_ARTIFACT_AUTHORITY_INVALID",
            Self::Quarantine => "LW_NATS_QUARANTINE_FAILED",
            Self::Identity => "LW_CONTROL_OUTBOX_IDENTITY_INVALID",
            Self::Contract => "LW_CONTROL_OUTBOX_CONTRACT_INVALID",
            Self::Publish => "LW_CONTROL_OUTBOX_PUBLISH_FAILED",
            Self::PublishTimeout => "LW_CONTROL_OUTBOX_PUBLISH_TIMEOUT",
            Self::Fence => "LW_CONTROL_OUTBOX_FENCE_LOST",
            Self::Database(_) => "LW_CONTROL_OUTBOX_DATABASE_FAILED",
            Self::AuthoringFailurePersistence => {
                "LW_CONTROL_AUTHORING_PUBLICATION_FAILURE_PERSIST_FAILED"
            }
        }
    }

    /// Transport and database outages retain the Outbox row and are safe to retry.
    #[must_use]
    pub const fn retryable(&self) -> bool {
        matches!(
            self,
            Self::Publish | Self::PublishTimeout | Self::Database(_)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{MessagingError, PublicationRetryAction, publication_retry_action};

    #[test]
    fn authoring_publication_retry_uses_the_consumer_delivery_limit() {
        assert!(matches!(
            publication_retry_action(1, 10),
            Ok(PublicationRetryAction::Retry)
        ));
        assert!(matches!(
            publication_retry_action(9, 10),
            Ok(PublicationRetryAction::Retry)
        ));
        assert!(matches!(
            publication_retry_action(10, 10),
            Ok(PublicationRetryAction::Fail)
        ));
        assert!(matches!(
            publication_retry_action(11, 10),
            Ok(PublicationRetryAction::Fail)
        ));
    }

    #[test]
    fn authoring_publication_retry_rejects_invalid_delivery_metadata() {
        assert!(matches!(
            publication_retry_action(0, 10),
            Err(MessagingError::DeliveryInfo)
        ));
        assert!(matches!(
            publication_retry_action(1, 0),
            Err(MessagingError::DeliveryInfo)
        ));
        assert!(matches!(
            publication_retry_action(1, -1),
            Err(MessagingError::DeliveryInfo)
        ));
    }
}
