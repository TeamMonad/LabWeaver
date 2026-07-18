use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;

use async_nats::jetstream::AckKind;
use async_nats::jetstream::consumer::PullConsumer;
use async_nats::jetstream::message::PublishMessage;
use async_trait::async_trait;
use contracts::authoring::EnvironmentClass;
use contracts::environment::{
    EnvironmentCreateSpec, EnvironmentInstance, EnvironmentLeaseAuthorization,
    EnvironmentLeaseState, EnvironmentLeaseVerificationRequest,
    EnvironmentLeaseVerificationResponse, EnvironmentLifecycleCommandData,
    EnvironmentOperationKind,
};
use contracts::events::{
    CloudEvent, EVENT_CONTRACTS, ReleasePublished, ReleaseWithdrawn, subjects,
};
use contracts::{EnvironmentId, EventId, OperationId, Revision, Sha256Digest};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    EnvironmentEventPublisher, EnvironmentProvider, InboundCommandDecision,
    InboundLifecycleCommand, PgEnvironmentStore, PgReleaseProjectionStore, ProviderFailure,
    ProviderFailureCode, ProviderObservation, PublishFailure, ReconcileAction,
    ReleaseProjectionDecision,
};

const MAX_COMMAND_BYTES: usize = 1024 * 1024;
const REDELIVERY_DELAY: Duration = Duration::from_secs(1);

/// Durable `CloudEvent` command carried by the catalogued lifecycle `JetStream` subject.
pub type LifecycleCommandMessage = CloudEvent<EnvironmentLifecycleCommandData>;

/// Connects to NATS with mandatory TLS, a private CA, a client certificate, and a credentials file.
pub async fn connect_nats_mtls(
    server: &str,
    ca_path: PathBuf,
    client_certificate_path: PathBuf,
    client_private_key_path: PathBuf,
    credentials_path: PathBuf,
) -> Result<async_nats::Client, NatsMessagingError> {
    if server.trim().is_empty()
        || [
            &ca_path,
            &client_certificate_path,
            &client_private_key_path,
            &credentials_path,
        ]
        .iter()
        .any(|path| !path.is_absolute())
    {
        return Err(NatsMessagingError::Configuration);
    }
    let options = async_nats::ConnectOptions::new()
        .require_tls(true)
        .add_root_certificates(ca_path)
        .add_client_certificate(client_certificate_path, client_private_key_path)
        .credentials_file(credentials_path)
        .await
        .map_err(|_| NatsMessagingError::Credentials)?;
    options
        .connect(server)
        .await
        .map_err(|_| NatsMessagingError::Connect)
}

/// JetStream-backed publisher that waits for the server persistence acknowledgement.
#[derive(Clone)]
pub struct JetStreamEventPublisher {
    context: async_nats::jetstream::Context,
}

impl JetStreamEventPublisher {
    #[must_use]
    pub fn new(client: async_nats::Client) -> Self {
        Self {
            context: async_nats::jetstream::new(client),
        }
    }
}

#[async_trait]
impl EnvironmentEventPublisher for JetStreamEventPublisher {
    async fn publish(
        &self,
        subject: &str,
        event: &CloudEvent<Value>,
    ) -> Result<(), PublishFailure> {
        let payload = serde_json::to_vec(event).map_err(|_| PublishFailure::Rejected)?;
        let acknowledgement = self
            .context
            .send_publish(
                subject.to_owned(),
                PublishMessage::build()
                    .payload(payload.into())
                    .message_id(event.id.to_string()),
            )
            .await
            .map_err(|_| PublishFailure::Unavailable)?;
        acknowledgement
            .await
            .map_err(|_| PublishFailure::Rejected)?;
        Ok(())
    }
}

/// Explicit remote Provider adapter. The configured subject is bound to exactly one provider name.
pub struct NatsEnvironmentProvider {
    binding: String,
    subject: String,
    client: async_nats::Client,
}

/// Fail-closed Access Service adapter used before automatic expiry cleanup.
#[derive(Clone)]
pub struct NatsAccessRevoker {
    subject: String,
    client: async_nats::Client,
    timeout: Duration,
}

/// Exact Resource Service adapter used to verify a Work Lease before command acceptance.
#[derive(Clone)]
pub struct NatsResourceLeaseVerifier {
    subject: String,
    client: async_nats::Client,
    timeout: Duration,
}

impl NatsResourceLeaseVerifier {
    pub fn new(
        subject: String,
        client: async_nats::Client,
        timeout: Duration,
    ) -> Result<Self, NatsMessagingError> {
        if !valid_subject(&subject) || timeout.is_zero() || timeout > Duration::from_secs(30) {
            return Err(NatsMessagingError::Configuration);
        }
        Ok(Self {
            subject,
            client,
            timeout,
        })
    }

    async fn verify(
        &self,
        request: EnvironmentLeaseVerificationRequest,
        authority_now: contracts::UtcTimestamp,
    ) -> Result<EnvironmentLeaseAuthorization, NatsMessagingError> {
        let payload =
            serde_json::to_vec(&request).map_err(|_| NatsMessagingError::Serialization)?;
        let message = tokio::time::timeout(
            self.timeout,
            self.client.request(self.subject.clone(), payload.into()),
        )
        .await
        .map_err(|_| NatsMessagingError::LeaseRequestTimeout)?
        .map_err(|_| NatsMessagingError::LeaseRequestFailed)?;
        if message.payload.len() > MAX_COMMAND_BYTES {
            return Err(NatsMessagingError::LeaseResponseInvalid);
        }
        let response: EnvironmentLeaseVerificationResponse =
            serde_json::from_slice(&message.payload)
                .map_err(|_| NatsMessagingError::LeaseResponseInvalid)?;
        let authorization = response
            .authorization
            .ok_or(NatsMessagingError::LeaseResponseInvalid)?;
        if response.version != 1
            || response.state != EnvironmentLeaseState::Active
            || authorization.lease_id != request.lease_id
            || authorization.environment_id != request.environment_id
            || authorization.course_id != request.course_id
            || authorization.owner_actor_id != request.owner_actor_id
            || authorization.capacity_binding != request.capacity_binding
            || authorization.active_from > authority_now
            || authorization.expires_at <= authority_now
        {
            return Err(NatsMessagingError::LeaseResponseInvalid);
        }
        Ok(authorization)
    }
}

impl NatsAccessRevoker {
    pub fn new(
        subject: String,
        client: async_nats::Client,
        timeout: Duration,
    ) -> Result<Self, NatsMessagingError> {
        if !valid_subject(&subject) || timeout.is_zero() || timeout > Duration::from_secs(30) {
            return Err(NatsMessagingError::Configuration);
        }
        Ok(Self {
            subject,
            client,
            timeout,
        })
    }

    /// Requests revocation and accepts only a response bound to the same Environment revision.
    pub async fn revoke_for_expiry(
        &self,
        instance: &EnvironmentInstance,
    ) -> Result<Revision, NatsMessagingError> {
        let request = AccessRevocationRequest {
            version: 1,
            environment_id: instance.id,
            environment_revision: instance.revision,
            reason: "environment_expired",
        };
        let payload =
            serde_json::to_vec(&request).map_err(|_| NatsMessagingError::Serialization)?;
        let message = tokio::time::timeout(
            self.timeout,
            self.client.request(self.subject.clone(), payload.into()),
        )
        .await
        .map_err(|_| NatsMessagingError::RequestTimeout)?
        .map_err(|_| NatsMessagingError::RequestFailed)?;
        if message.payload.len() > MAX_COMMAND_BYTES {
            return Err(NatsMessagingError::ResponseInvalid);
        }
        let response: AccessRevocationResponse = serde_json::from_slice(&message.payload)
            .map_err(|_| NatsMessagingError::ResponseInvalid)?;
        if response.version != 1
            || response.environment_id != instance.id
            || response.environment_revision != instance.revision
        {
            return Err(NatsMessagingError::ResponseInvalid);
        }
        Ok(response.access_revocation_revision)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AccessRevocationRequest {
    version: u8,
    environment_id: EnvironmentId,
    environment_revision: Revision,
    reason: &'static str,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AccessRevocationResponse {
    version: u8,
    environment_id: EnvironmentId,
    environment_revision: Revision,
    access_revocation_revision: Revision,
}

impl NatsEnvironmentProvider {
    pub fn new(
        binding: String,
        subject: String,
        client: async_nats::Client,
    ) -> Result<Self, NatsMessagingError> {
        if !valid_token(&binding) || !valid_subject(&subject) {
            return Err(NatsMessagingError::Configuration);
        }
        Ok(Self {
            binding,
            subject,
            client,
        })
    }
}

#[async_trait]
impl EnvironmentProvider for NatsEnvironmentProvider {
    fn binding(&self) -> &str {
        &self.binding
    }

    async fn execute(
        &self,
        action: ReconcileAction,
        instance: &EnvironmentInstance,
    ) -> Result<ProviderObservation, ProviderFailure> {
        let request = ProviderRequest {
            version: 1,
            operation_id: instance.operation.id,
            provider_step: instance.operation.provider_step,
            action,
            instance: instance.clone(),
        };
        let payload = serde_json::to_vec(&request).map_err(|_| invalid_observation())?;
        let message = self
            .client
            .request(self.subject.clone(), payload.into())
            .await
            .map_err(|_| unavailable())?;
        if message.payload.len() > MAX_COMMAND_BYTES {
            return Err(invalid_observation());
        }
        let response: ProviderResponse =
            serde_json::from_slice(&message.payload).map_err(|_| invalid_observation())?;
        match response {
            ProviderResponse::Succeeded {
                version,
                operation_id,
                provider_step,
                observation,
            } if version == 1
                && operation_id == instance.operation.id
                && provider_step == instance.operation.provider_step =>
            {
                Ok(observation)
            }
            ProviderResponse::Failed {
                version,
                operation_id,
                provider_step,
                failure,
            } if version == 1
                && operation_id == instance.operation.id
                && provider_step == instance.operation.provider_step =>
            {
                Err(failure)
            }
            _ => Err(invalid_observation()),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProviderRequest {
    version: u8,
    operation_id: OperationId,
    provider_step: u32,
    action: ReconcileAction,
    instance: EnvironmentInstance,
}

#[derive(Deserialize)]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum ProviderResponse {
    Succeeded {
        version: u8,
        operation_id: OperationId,
        provider_step: u32,
        observation: ProviderObservation,
    },
    Failed {
        version: u8,
        operation_id: OperationId,
        provider_step: u32,
        failure: ProviderFailure,
    },
}

/// One durable pull consumer bound to an existing deployment-owned stream and consumer.
pub struct JetStreamCommandConsumer {
    consumer_name: String,
    quarantine_subject: String,
    context: async_nats::jetstream::Context,
    messages: async_nats::jetstream::consumer::pull::Stream,
}

impl JetStreamCommandConsumer {
    pub async fn bind(
        client: async_nats::Client,
        stream_name: &str,
        consumer_name: &str,
        quarantine_subject: &str,
    ) -> Result<Self, NatsMessagingError> {
        if !valid_token(stream_name)
            || !valid_token(consumer_name)
            || !valid_subject(quarantine_subject)
        {
            return Err(NatsMessagingError::Configuration);
        }
        let context = async_nats::jetstream::new(client);
        let stream = context
            .get_stream(stream_name)
            .await
            .map_err(|_| NatsMessagingError::StreamUnavailable)?;
        let consumer: PullConsumer = stream
            .get_consumer(consumer_name)
            .await
            .map_err(|_| NatsMessagingError::ConsumerUnavailable)?;
        let messages = consumer
            .messages()
            .await
            .map_err(|_| NatsMessagingError::ConsumerUnavailable)?;
        Ok(Self {
            consumer_name: consumer_name.to_owned(),
            quarantine_subject: quarantine_subject.to_owned(),
            context,
            messages,
        })
    }

    /// Receives and transactionally applies one command before double-acknowledging it.
    #[allow(
        clippy::too_many_lines,
        reason = "the consumer keeps validation, Lease gating, persistence, quarantine, and acknowledgement ordering visible together"
    )]
    pub async fn process_next(
        &mut self,
        store: &PgEnvironmentStore,
        lease_verifier: &NatsResourceLeaseVerifier,
    ) -> Result<CommandConsumeOutcome, NatsMessagingError> {
        let message = self
            .messages
            .next()
            .await
            .ok_or(NatsMessagingError::ConsumerClosed)?
            .map_err(|_| NatsMessagingError::Receive)?;
        if message.payload.len() > MAX_COMMAND_BYTES {
            self.quarantine(&message, None, "LW_ENVIRONMENT_COMMAND_PAYLOAD_TOO_LARGE")
                .await?;
            message
                .double_ack_with(AckKind::Term)
                .await
                .map_err(|_| NatsMessagingError::Acknowledge)?;
            return Ok(CommandConsumeOutcome::Rejected);
        }
        let command: LifecycleCommandMessage =
            if let Ok(command) = serde_json::from_slice(&message.payload) {
                command
            } else {
                self.quarantine(&message, None, "LW_ENVIRONMENT_COMMAND_PAYLOAD_INVALID")
                    .await?;
                message
                    .double_ack_with(AckKind::Term)
                    .await
                    .map_err(|_| NatsMessagingError::Acknowledge)?;
                return Ok(CommandConsumeOutcome::Rejected);
            };
        let Some(contract) = EVENT_CONTRACTS
            .iter()
            .copied()
            .find(|contract| contract.subject == subjects::ENVIRONMENT_LIFECYCLE_REQUESTED)
        else {
            return Err(NatsMessagingError::Configuration);
        };
        if command.validate(contract).is_err() {
            self.quarantine(
                &message,
                Some(command.id),
                "LW_ENVIRONMENT_COMMAND_CONTRACT_INVALID",
            )
            .await?;
            message
                .double_ack_with(AckKind::Term)
                .await
                .map_err(|_| NatsMessagingError::Acknowledge)?;
            return Ok(CommandConsumeOutcome::Rejected);
        }
        let lease_authorization =
            match resolve_lease_authorization(store, lease_verifier, &command).await {
                Ok(authorization) => authorization,
                Err(LeaseGateFailure::Retryable) => {
                    message
                        .ack_with(AckKind::Nak(Some(REDELIVERY_DELAY)))
                        .await
                        .map_err(|_| NatsMessagingError::Acknowledge)?;
                    return Ok(CommandConsumeOutcome::Deferred);
                }
                Err(LeaseGateFailure::Rejected) => {
                    self.quarantine(
                        &message,
                        Some(command.id),
                        "LW_ENVIRONMENT_LEASE_VERIFICATION_REJECTED",
                    )
                    .await?;
                    message
                        .double_ack_with(AckKind::Term)
                        .await
                        .map_err(|_| NatsMessagingError::Acknowledge)?;
                    return Ok(CommandConsumeOutcome::Rejected);
                }
            };
        let inbound = InboundLifecycleCommand {
            consumer: self.consumer_name.clone(),
            event_id: command.id,
            course_id: command.course_id,
            aggregate_revision: command.aggregate_revision,
            aggregate_sequence: command.aggregate_sequence,
            idempotency_key: command.data.idempotency_key,
            command: command.data.command,
            create: command.data.create,
            lease_authorization,
        };
        match store.accept_inbound_command(&inbound).await {
            Ok(InboundCommandDecision::Applied(_)) => {
                message
                    .double_ack()
                    .await
                    .map_err(|_| NatsMessagingError::Acknowledge)?;
                Ok(CommandConsumeOutcome::Applied)
            }
            Ok(InboundCommandDecision::Duplicate | InboundCommandDecision::Stale) => {
                message
                    .double_ack()
                    .await
                    .map_err(|_| NatsMessagingError::Acknowledge)?;
                Ok(CommandConsumeOutcome::Ignored)
            }
            Ok(InboundCommandDecision::Gap) => {
                message
                    .ack_with(AckKind::Nak(Some(REDELIVERY_DELAY)))
                    .await
                    .map_err(|_| NatsMessagingError::Acknowledge)?;
                Ok(CommandConsumeOutcome::Deferred)
            }
            Err(error) if error.retryable() => {
                message
                    .ack_with(AckKind::Nak(Some(REDELIVERY_DELAY)))
                    .await
                    .map_err(|_| NatsMessagingError::Acknowledge)?;
                Ok(CommandConsumeOutcome::Deferred)
            }
            Err(_) => {
                self.quarantine(
                    &message,
                    Some(command.id),
                    "LW_ENVIRONMENT_COMMAND_REJECTED",
                )
                .await?;
                message
                    .double_ack_with(AckKind::Term)
                    .await
                    .map_err(|_| NatsMessagingError::Acknowledge)?;
                Ok(CommandConsumeOutcome::Rejected)
            }
        }
    }

    async fn quarantine(
        &self,
        message: &async_nats::jetstream::Message,
        event_id: Option<EventId>,
        diagnostic_code: &'static str,
    ) -> Result<(), NatsMessagingError> {
        let payload_hash = Sha256Digest::of_bytes(&message.payload);
        let record = QuarantineRecord {
            version: 1,
            consumer: &self.consumer_name,
            source_subject: message.subject.as_str(),
            event_id,
            payload_sha256: payload_hash,
            size_bytes: u64::try_from(message.payload.len())
                .map_err(|_| NatsMessagingError::Quarantine)?,
            diagnostic_code,
        };
        let payload = serde_json::to_vec(&record).map_err(|_| NatsMessagingError::Quarantine)?;
        let acknowledgement = self
            .context
            .send_publish(
                self.quarantine_subject.clone(),
                PublishMessage::build()
                    .payload(payload.into())
                    .message_id(format!("{payload_hash}:{diagnostic_code}")),
            )
            .await
            .map_err(|_| NatsMessagingError::Quarantine)?;
        acknowledgement
            .await
            .map_err(|_| NatsMessagingError::Quarantine)?;
        Ok(())
    }
}

/// Durable Control release consumer feeding the Environment-owned immutable projection.
pub struct JetStreamReleaseConsumer {
    consumer_name: String,
    quarantine_subject: String,
    context: async_nats::jetstream::Context,
    messages: async_nats::jetstream::consumer::pull::Stream,
}

enum ReleaseEvent {
    Published(Box<CloudEvent<ReleasePublished>>),
    Withdrawn(Box<CloudEvent<ReleaseWithdrawn>>),
}

impl JetStreamReleaseConsumer {
    pub async fn bind(
        client: async_nats::Client,
        stream_name: &str,
        consumer_name: &str,
        quarantine_subject: &str,
    ) -> Result<Self, NatsMessagingError> {
        if !valid_token(stream_name)
            || !valid_token(consumer_name)
            || !valid_subject(quarantine_subject)
        {
            return Err(NatsMessagingError::Configuration);
        }
        let context = async_nats::jetstream::new(client);
        let stream = context
            .get_stream(stream_name)
            .await
            .map_err(|_| NatsMessagingError::StreamUnavailable)?;
        let consumer: PullConsumer = stream
            .get_consumer(consumer_name)
            .await
            .map_err(|_| NatsMessagingError::ConsumerUnavailable)?;
        let expected_subjects = BTreeSet::from([
            subjects::ENVIRONMENT_TEMPLATE_RELEASE_PUBLISHED,
            subjects::ENVIRONMENT_TEMPLATE_RELEASE_WITHDRAWN,
        ]);
        let configured_subjects = consumer
            .cached_info()
            .config
            .filter_subjects
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        if !consumer.cached_info().config.filter_subject.is_empty()
            || configured_subjects != expected_subjects
        {
            return Err(NatsMessagingError::Configuration);
        }
        let messages = consumer
            .messages()
            .await
            .map_err(|_| NatsMessagingError::ConsumerUnavailable)?;
        Ok(Self {
            consumer_name: consumer_name.to_owned(),
            quarantine_subject: quarantine_subject.to_owned(),
            context,
            messages,
        })
    }

    pub async fn process_next(
        &mut self,
        store: &PgReleaseProjectionStore,
    ) -> Result<CommandConsumeOutcome, NatsMessagingError> {
        let message = self
            .messages
            .next()
            .await
            .ok_or(NatsMessagingError::ConsumerClosed)?
            .map_err(|_| NatsMessagingError::Receive)?;
        if message.payload.len() > MAX_COMMAND_BYTES {
            self.quarantine(&message, None, "LW_ENVIRONMENT_RELEASE_PAYLOAD_TOO_LARGE")
                .await?;
            message
                .double_ack_with(AckKind::Term)
                .await
                .map_err(|_| NatsMessagingError::Acknowledge)?;
            return Ok(CommandConsumeOutcome::Rejected);
        }
        let Ok(envelope): Result<Value, _> = serde_json::from_slice(&message.payload) else {
            self.quarantine(&message, None, "LW_ENVIRONMENT_RELEASE_PAYLOAD_INVALID")
                .await?;
            message
                .double_ack_with(AckKind::Term)
                .await
                .map_err(|_| NatsMessagingError::Acknowledge)?;
            return Ok(CommandConsumeOutcome::Rejected);
        };
        let subject = envelope.get("subject").and_then(Value::as_str);
        let parsed = match subject {
            Some(subjects::ENVIRONMENT_TEMPLATE_RELEASE_PUBLISHED) => {
                serde_json::from_value::<CloudEvent<ReleasePublished>>(envelope)
                    .map(|event| (event.id, ReleaseEvent::Published(Box::new(event))))
            }
            Some(subjects::ENVIRONMENT_TEMPLATE_RELEASE_WITHDRAWN) => {
                serde_json::from_value::<CloudEvent<ReleaseWithdrawn>>(envelope)
                    .map(|event| (event.id, ReleaseEvent::Withdrawn(Box::new(event))))
            }
            _ => Err(serde_json::Error::io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "unsupported release subject",
            ))),
        };
        let Ok((event_id, event)) = parsed else {
            self.quarantine(&message, None, "LW_ENVIRONMENT_RELEASE_PAYLOAD_INVALID")
                .await?;
            message
                .double_ack_with(AckKind::Term)
                .await
                .map_err(|_| NatsMessagingError::Acknowledge)?;
            return Ok(CommandConsumeOutcome::Rejected);
        };
        let decision = match &event {
            ReleaseEvent::Published(event) => store.accept(&self.consumer_name, event).await,
            ReleaseEvent::Withdrawn(event) => {
                store.accept_withdrawal(&self.consumer_name, event).await
            }
        };
        match decision {
            Ok(ReleaseProjectionDecision::Applied) => {
                message
                    .double_ack()
                    .await
                    .map_err(|_| NatsMessagingError::Acknowledge)?;
                Ok(CommandConsumeOutcome::Applied)
            }
            Ok(ReleaseProjectionDecision::Duplicate | ReleaseProjectionDecision::Stale) => {
                message
                    .double_ack()
                    .await
                    .map_err(|_| NatsMessagingError::Acknowledge)?;
                Ok(CommandConsumeOutcome::Ignored)
            }
            Ok(ReleaseProjectionDecision::Gap) => {
                message
                    .ack_with(AckKind::Nak(Some(REDELIVERY_DELAY)))
                    .await
                    .map_err(|_| NatsMessagingError::Acknowledge)?;
                Ok(CommandConsumeOutcome::Deferred)
            }
            Err(
                crate::ReleaseProjectionError::Database(_)
                | crate::ReleaseProjectionError::PersistenceFailed,
            ) => {
                message
                    .ack_with(AckKind::Nak(Some(REDELIVERY_DELAY)))
                    .await
                    .map_err(|_| NatsMessagingError::Acknowledge)?;
                Ok(CommandConsumeOutcome::Deferred)
            }
            Err(_) => {
                self.quarantine(&message, Some(event_id), "LW_ENVIRONMENT_RELEASE_REJECTED")
                    .await?;
                message
                    .double_ack_with(AckKind::Term)
                    .await
                    .map_err(|_| NatsMessagingError::Acknowledge)?;
                Ok(CommandConsumeOutcome::Rejected)
            }
        }
    }

    async fn quarantine(
        &self,
        message: &async_nats::jetstream::Message,
        event_id: Option<EventId>,
        diagnostic_code: &'static str,
    ) -> Result<(), NatsMessagingError> {
        let payload_sha256 = Sha256Digest::of_bytes(&message.payload);
        let record = QuarantineRecord {
            version: 1,
            consumer: &self.consumer_name,
            source_subject: message.subject.as_str(),
            event_id,
            payload_sha256,
            size_bytes: u64::try_from(message.payload.len())
                .map_err(|_| NatsMessagingError::Quarantine)?,
            diagnostic_code,
        };
        let payload = serde_json::to_vec(&record).map_err(|_| NatsMessagingError::Quarantine)?;
        let acknowledgement = self
            .context
            .send_publish(
                self.quarantine_subject.clone(),
                PublishMessage::build()
                    .payload(payload.into())
                    .message_id(format!("{payload_sha256}:{diagnostic_code}")),
            )
            .await
            .map_err(|_| NatsMessagingError::Quarantine)?;
        acknowledgement
            .await
            .map_err(|_| NatsMessagingError::Quarantine)?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LeaseGateFailure {
    Retryable,
    Rejected,
}

async fn resolve_lease_authorization(
    store: &PgEnvironmentStore,
    verifier: &NatsResourceLeaseVerifier,
    message: &LifecycleCommandMessage,
) -> Result<Option<EnvironmentLeaseAuthorization>, LeaseGateFailure> {
    let command = &message.data.command;
    let request = if command.kind == EnvironmentOperationKind::Create {
        let spec = message
            .data
            .create
            .as_ref()
            .ok_or(LeaseGateFailure::Rejected)?;
        lease_request_from_create(command.environment_id, spec)?
    } else if matches!(
        command.kind,
        EnvironmentOperationKind::Start
            | EnvironmentOperationKind::Retry
            | EnvironmentOperationKind::Recover
            | EnvironmentOperationKind::Reset
    ) {
        let instance = match store.load(command.environment_id).await {
            Ok(instance) => instance,
            Err(crate::EnvironmentStoreError::EnvironmentNotFound) => return Ok(None),
            Err(error) if error.retryable() => return Err(LeaseGateFailure::Retryable),
            Err(_) => return Err(LeaseGateFailure::Rejected),
        };
        if instance.class == EnvironmentClass::Experiment {
            return Ok(None);
        }
        Some(EnvironmentLeaseVerificationRequest {
            version: 1,
            lease_id: instance.lease_id.ok_or(LeaseGateFailure::Rejected)?,
            environment_id: instance.id,
            course_id: instance.course_id,
            owner_actor_id: instance.owner_id,
            capacity_binding: instance
                .capacity_binding
                .ok_or(LeaseGateFailure::Rejected)?,
        })
    } else {
        None
    };
    let Some(request) = request else {
        return Ok(None);
    };
    let authority_now = store
        .current_time()
        .await
        .map_err(|_| LeaseGateFailure::Retryable)?;
    verifier
        .verify(request, authority_now)
        .await
        .map(Some)
        .map_err(|error| {
            if error.lease_retryable() {
                LeaseGateFailure::Retryable
            } else {
                LeaseGateFailure::Rejected
            }
        })
}

fn lease_request_from_create(
    environment_id: EnvironmentId,
    spec: &EnvironmentCreateSpec,
) -> Result<Option<EnvironmentLeaseVerificationRequest>, LeaseGateFailure> {
    if spec.class == EnvironmentClass::Experiment {
        if spec.lease_id.is_some() || spec.capacity_binding.is_some() {
            return Err(LeaseGateFailure::Rejected);
        }
        return Ok(None);
    }
    Ok(Some(EnvironmentLeaseVerificationRequest {
        version: 1,
        lease_id: spec.lease_id.ok_or(LeaseGateFailure::Rejected)?,
        environment_id,
        course_id: spec.course_id,
        owner_actor_id: spec.owner_actor_id,
        capacity_binding: spec
            .capacity_binding
            .clone()
            .ok_or(LeaseGateFailure::Rejected)?,
    }))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct QuarantineRecord<'a> {
    version: u8,
    consumer: &'a str,
    source_subject: &'a str,
    event_id: Option<EventId>,
    payload_sha256: Sha256Digest,
    size_bytes: u64,
    diagnostic_code: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandConsumeOutcome {
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

const fn unavailable() -> ProviderFailure {
    ProviderFailure {
        code: ProviderFailureCode::Unavailable,
        retryable: true,
    }
}

const fn invalid_observation() -> ProviderFailure {
    ProviderFailure {
        code: ProviderFailureCode::ObservationInvalid,
        retryable: false,
    }
}

/// Stable NATS startup/consumer failures that never expose credentials or payloads.
#[derive(Debug, thiserror::Error)]
pub enum NatsMessagingError {
    #[error("LW_ENVIRONMENT_NATS_CONFIGURATION_INVALID")]
    Configuration,
    #[error("LW_ENVIRONMENT_NATS_CREDENTIALS_INVALID")]
    Credentials,
    #[error("LW_ENVIRONMENT_NATS_CONNECT_FAILED")]
    Connect,
    #[error("LW_ENVIRONMENT_NATS_STREAM_UNAVAILABLE")]
    StreamUnavailable,
    #[error("LW_ENVIRONMENT_NATS_CONSUMER_UNAVAILABLE")]
    ConsumerUnavailable,
    #[error("LW_ENVIRONMENT_NATS_CONSUMER_CLOSED")]
    ConsumerClosed,
    #[error("LW_ENVIRONMENT_NATS_RECEIVE_FAILED")]
    Receive,
    #[error("LW_ENVIRONMENT_NATS_ACKNOWLEDGE_FAILED")]
    Acknowledge,
    #[error("LW_ENVIRONMENT_NATS_SERIALIZATION_FAILED")]
    Serialization,
    #[error("LW_ENVIRONMENT_ACCESS_REVOCATION_TIMEOUT")]
    RequestTimeout,
    #[error("LW_ENVIRONMENT_ACCESS_REVOCATION_UNAVAILABLE")]
    RequestFailed,
    #[error("LW_ENVIRONMENT_ACCESS_REVOCATION_RESPONSE_INVALID")]
    ResponseInvalid,
    #[error("LW_ENVIRONMENT_LEASE_VERIFICATION_TIMEOUT")]
    LeaseRequestTimeout,
    #[error("LW_ENVIRONMENT_LEASE_VERIFICATION_UNAVAILABLE")]
    LeaseRequestFailed,
    #[error("LW_ENVIRONMENT_LEASE_VERIFICATION_RESPONSE_INVALID")]
    LeaseResponseInvalid,
    #[error("LW_ENVIRONMENT_COMMAND_QUARANTINE_FAILED")]
    Quarantine,
}

impl NatsMessagingError {
    const fn lease_retryable(&self) -> bool {
        matches!(self, Self::LeaseRequestTimeout | Self::LeaseRequestFailed)
    }
}
