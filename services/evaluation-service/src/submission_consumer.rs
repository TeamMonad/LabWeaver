//! Durable Evaluation consumer for completed submission freezes.
#![allow(missing_docs, clippy::missing_errors_doc)]

use std::time::Duration;

use async_nats::jetstream::AckKind;
use async_nats::jetstream::consumer::PullConsumer;
use async_nats::jetstream::message::PublishMessage;
use contracts::EventId;
use contracts::events::{CloudEvent, EVENT_CONTRACTS, SubmissionFrozen, subjects};
use contracts::http::{
    EnvironmentPublicationAdmissionQuery, IdempotencyKey, InternalCreateEvaluationRunRequest,
};
use futures_util::StreamExt;
use persistence_sqlx::Sha256Digest;
use serde::Serialize;
use serde_json::Value;

use crate::authoring_client::{AuthoringAdmissionClient, AuthoringAdmissionClientError};
use crate::freeze_store::FreezeStoreError;
use crate::{EvaluationControlStoreError, PgEvaluationControlStore, PgFreezeStore};

const MAX_EVENT_BYTES: usize = 1024 * 1024;
const REDELIVERY_DELAY: Duration = Duration::from_secs(1);
const SUBMISSION_STREAM_SUBJECT_PREFIX: &str = "labweaver.evaluation.";

/// Durable consumer for `SUBMISSION_FROZEN` events.
pub struct SubmissionFrozenConsumer {
    context: async_nats::jetstream::Context,
    messages: async_nats::jetstream::consumer::pull::Stream,
    quarantine_subject: String,
}

impl SubmissionFrozenConsumer {
    /// Binds only to the deployment-owned exact durable consumer filter.
    pub async fn bind(
        client: async_nats::Client,
        stream_name: &str,
        consumer_name: &str,
        quarantine_subject: &str,
    ) -> Result<Self, SubmissionConsumerError> {
        if [stream_name, consumer_name, quarantine_subject]
            .iter()
            .any(|value| value.trim().is_empty() || value.chars().any(char::is_whitespace))
            || !quarantine_subject.starts_with(SUBMISSION_STREAM_SUBJECT_PREFIX)
        {
            return Err(SubmissionConsumerError::Configuration);
        }
        let context = async_nats::jetstream::new(client);
        let stream = context
            .get_stream(stream_name)
            .await
            .map_err(|_| SubmissionConsumerError::Stream)?;
        let consumer: PullConsumer = stream
            .get_consumer(consumer_name)
            .await
            .map_err(|_| SubmissionConsumerError::Consumer)?;
        let mut filters = consumer.cached_info().config.filter_subjects.clone();
        filters.sort_unstable();
        if !consumer.cached_info().config.filter_subject.is_empty()
            || filters != [subjects::SUBMISSION_FROZEN.to_owned()]
        {
            return Err(SubmissionConsumerError::Configuration);
        }
        let messages = consumer
            .messages()
            .await
            .map_err(|_| SubmissionConsumerError::Consumer)?;
        Ok(Self {
            context,
            messages,
            quarantine_subject: quarantine_subject.to_owned(),
        })
    }

    /// Processes one event and acknowledges only after a run reservation or a durable terminal
    /// quarantine has completed.
    #[allow(
        clippy::too_many_lines,
        reason = "the ordered event validation, admission, reservation, and acknowledgement flow is one durable boundary"
    )]
    pub async fn process_next(
        &mut self,
        freeze_store: &PgFreezeStore,
        evaluation: &PgEvaluationControlStore,
        authoring: &AuthoringAdmissionClient,
    ) -> Result<(), SubmissionConsumerError> {
        let message = self
            .messages
            .next()
            .await
            .ok_or(SubmissionConsumerError::Closed)?
            .map_err(|_| SubmissionConsumerError::Receive)?;
        if message.payload.len() > MAX_EVENT_BYTES {
            self.quarantine(&message, None, "LW_EVENT_PAYLOAD_TOO_LARGE")
                .await?;
            ack_terminal(&message).await?;
            return Ok(());
        }
        let event: CloudEvent<Value> =
            if let Ok(event) = contracts::parse_strict_json(&message.payload) {
                event
            } else {
                self.quarantine(&message, None, "LW_EVENT_ENVELOPE_INVALID")
                    .await?;
                ack_terminal(&message).await?;
                return Ok(());
            };
        let Some(contract) = EVENT_CONTRACTS
            .iter()
            .copied()
            .find(|contract| contract.subject == event.subject)
        else {
            self.quarantine(&message, Some(event.id), "LW_EVENT_SUBJECT_MISMATCH")
                .await?;
            ack_terminal(&message).await?;
            return Ok(());
        };
        if event.subject != subjects::SUBMISSION_FROZEN || event.validate(contract).is_err() {
            self.quarantine(&message, Some(event.id), "LW_EVENT_ENVELOPE_INVALID")
                .await?;
            ack_terminal(&message).await?;
            return Ok(());
        }
        let data: SubmissionFrozen = if let Ok(data) = serde_json::from_value(event.data) {
            data
        } else {
            self.quarantine(&message, Some(event.id), "LW_SUBMISSION_EVENT_INVALID")
                .await?;
            ack_terminal(&message).await?;
            return Ok(());
        };
        if data.validate().is_err()
            || data.submission.project_id != event.project_id
            || data.submission.course_id != event.course_id
        {
            self.quarantine(&message, Some(event.id), "LW_SUBMISSION_EVENT_INVALID")
                .await?;
            ack_terminal(&message).await?;
            return Ok(());
        }

        let persisted = match freeze_store
            .load_completed(
                data.submission.id,
                data.submission.project_id,
                data.submission.course_id,
                data.submission.actor_id,
            )
            .await
        {
            Ok(persisted) => persisted,
            Err(error) if retry_freeze_store(&error) => {
                return ack_retry(&message, event.id, error.diagnostic_code(), "freeze_store")
                    .await;
            }
            Err(FreezeStoreError::NotFound | FreezeStoreError::IdentityMismatch) => {
                self.quarantine(&message, Some(event.id), "LW_SUBMISSION_NOT_PERSISTED")
                    .await?;
                ack_terminal(&message).await?;
                return Ok(());
            }
            Err(error) => {
                let diagnostic_code = error.diagnostic_code();
                self.quarantine(&message, Some(event.id), diagnostic_code)
                    .await?;
                ack_terminal(&message).await?;
                return Ok(());
            }
        };
        if persisted != data.submission {
            self.quarantine(&message, Some(event.id), "LW_SUBMISSION_EVENT_MISMATCH")
                .await?;
            ack_terminal(&message).await?;
            return Ok(());
        }

        let idempotency_key = IdempotencyKey::parse(&format!("submission-frozen:{}", persisted.id))
            .map_err(|_| SubmissionConsumerError::Configuration)?;
        let stable_trace_id = idempotency_key.as_str().to_owned();
        let existing_run = match evaluation
            .load_run_by_idempotency_key(persisted.project_id, &idempotency_key)
            .await
        {
            Ok(existing_run) => existing_run,
            Err(
                EvaluationControlStoreError::Database(_)
                | EvaluationControlStoreError::Persistence(_),
            ) => {
                return ack_retry(
                    &message,
                    event.id,
                    "LW_EVALUATION_DATABASE_FAILED",
                    "run_lookup",
                )
                .await;
            }
            Err(error) => {
                let diagnostic_code = error.diagnostic_code();
                self.quarantine(&message, Some(event.id), diagnostic_code)
                    .await?;
                ack_terminal(&message).await?;
                return Ok(());
            }
        };
        if let Some(existing_run) = existing_run {
            if existing_run.project_id != persisted.project_id
                || existing_run.course_id != persisted.course_id
                || existing_run.frozen_submission_id != persisted.id
                || existing_run.actor_id != persisted.actor_id
            {
                self.quarantine(
                    &message,
                    Some(event.id),
                    "LW_EVALUATION_RUN_IDENTITY_MISMATCH",
                )
                .await?;
                ack_terminal(&message).await?;
                return Ok(());
            }
            tracing::info!(
                event = "evaluation.submission_frozen.run_replay",
                frozen_submission_id = %persisted.id,
                run_id = %existing_run.id,
            );
            message
                .double_ack()
                .await
                .map_err(|_| SubmissionConsumerError::Ack)?;
            return Ok(());
        }

        let admission = match authoring
            .resolve_environment(
                persisted.environment.release_id,
                &EnvironmentPublicationAdmissionQuery {
                    project_id: persisted.project_id,
                    course_id: persisted.course_id,
                    environment_release_version: persisted.environment.release_version,
                },
            )
            .await
        {
            Ok(admission) => admission,
            Err(
                error @ (AuthoringAdmissionClientError::Unavailable
                | AuthoringAdmissionClientError::Transport
                | AuthoringAdmissionClientError::Token(_)),
            ) => {
                return ack_retry(
                    &message,
                    event.id,
                    authoring_diagnostic_code(&error),
                    "authoring_admission",
                )
                .await;
            }
            Err(error) => {
                let diagnostic_code = authoring_diagnostic_code(&error);
                self.quarantine(&message, Some(event.id), diagnostic_code)
                    .await?;
                ack_terminal(&message).await?;
                return Ok(());
            }
        };
        let Some(admission) = admission else {
            // Control has identified this exact release as Work. Work freezes remain bounded
            // snapshots and do not create an Evaluation run.
            tracing::info!(
                event = "evaluation.submission_frozen.work_snapshot",
                frozen_submission_id = %persisted.id,
                project_id = %persisted.project_id,
            );
            message
                .double_ack()
                .await
                .map_err(|_| SubmissionConsumerError::Ack)?;
            return Ok(());
        };

        let release = match evaluation
            .load_release(admission.evaluation_release_id)
            .await
        {
            Ok(release) => release,
            Err(
                EvaluationControlStoreError::Database(_)
                | EvaluationControlStoreError::Persistence(_),
            ) => {
                return ack_retry(
                    &message,
                    event.id,
                    "LW_EVALUATION_DATABASE_FAILED",
                    "release_lookup",
                )
                .await;
            }
            Err(error) => {
                let diagnostic_code = error.diagnostic_code();
                self.quarantine(&message, Some(event.id), diagnostic_code)
                    .await?;
                ack_terminal(&message).await?;
                return Ok(());
            }
        };
        if release.id != admission.evaluation_release_id
            || release.revision != admission.evaluation_release_revision
            || release.project_id != persisted.project_id
            || release.course_id != persisted.course_id
            || release.validate().is_err()
        {
            self.quarantine(&message, Some(event.id), "LW_EVALUATION_RELEASE_INVALID")
                .await?;
            ack_terminal(&message).await?;
            return Ok(());
        }
        let request = InternalCreateEvaluationRunRequest {
            project_id: persisted.project_id,
            course_id: persisted.course_id,
            release_id: release.id,
            release_revision: release.revision,
            frozen_submission_id: persisted.id,
            actor_id: persisted.actor_id,
            identity: contracts::evaluation::EvaluationRunIdentity {
                runtime_identity: release.runtime_identity.clone(),
                trace_id: stable_trace_id.clone(),
            },
        };
        let now = match evaluation.authority_now().await {
            Ok(now) => now,
            Err(
                EvaluationControlStoreError::Database(_)
                | EvaluationControlStoreError::Persistence(_),
            ) => {
                return ack_retry(
                    &message,
                    event.id,
                    "LW_EVALUATION_DATABASE_FAILED",
                    "authority_clock",
                )
                .await;
            }
            Err(error) => {
                let diagnostic_code = error.diagnostic_code();
                self.quarantine(&message, Some(event.id), diagnostic_code)
                    .await?;
                ack_terminal(&message).await?;
                return Ok(());
            }
        };
        match evaluation
            .create_run(
                &request,
                &idempotency_key,
                now,
                &stable_trace_id,
                &admission,
            )
            .await
        {
            Ok(reservation) => {
                tracing::info!(
                    event = "evaluation.submission_frozen.run_reserved",
                    frozen_submission_id = %persisted.id,
                    evaluation_release_id = %release.id,
                    replay = matches!(
                        reservation,
                        crate::EvaluationRunReservation::Replayed(_)
                    ),
                );
                message
                    .double_ack()
                    .await
                    .map_err(|_| SubmissionConsumerError::Ack)?;
            }
            Err(
                error @ (EvaluationControlStoreError::Database(_)
                | EvaluationControlStoreError::Persistence(_)
                | EvaluationControlStoreError::RequestInProgress
                | EvaluationControlStoreError::LeaseLost),
            ) => {
                ack_retry(&message, event.id, error.diagnostic_code(), "run_create").await?;
            }
            Err(EvaluationControlStoreError::ReleaseWithdrawn) => {
                self.quarantine(&message, Some(event.id), "LW_EVALUATION_RELEASE_WITHDRAWN")
                    .await?;
                ack_terminal(&message).await?;
            }
            Err(error) => {
                let diagnostic_code = error.diagnostic_code();
                self.quarantine(&message, Some(event.id), diagnostic_code)
                    .await?;
                ack_terminal(&message).await?;
            }
        }
        Ok(())
    }

    async fn quarantine(
        &self,
        message: &async_nats::jetstream::Message,
        event_id: Option<EventId>,
        diagnostic_code: &str,
    ) -> Result<(), SubmissionConsumerError> {
        let payload_sha256 = Sha256Digest::of_bytes(&message.payload);
        let record = QuarantineRecord {
            version: 1,
            event_id,
            payload_sha256,
            size_bytes: u64::try_from(message.payload.len())
                .map_err(|_| SubmissionConsumerError::Quarantine)?,
            diagnostic_code: diagnostic_code.to_owned(),
        };
        let payload =
            serde_json::to_vec(&record).map_err(|_| SubmissionConsumerError::Quarantine)?;
        let acknowledgement = self
            .context
            .send_publish(
                self.quarantine_subject.clone(),
                PublishMessage::build()
                    .payload(payload.into())
                    .message_id(format!("{payload_sha256}:{diagnostic_code}")),
            )
            .await
            .map_err(|_| SubmissionConsumerError::Quarantine)?;
        acknowledgement
            .await
            .map_err(|_| SubmissionConsumerError::Quarantine)?;
        tracing::warn!(
            event = "evaluation.submission_frozen.quarantined",
            event_id = ?event_id,
            diagnostic_code,
        );
        Ok(())
    }
}

async fn ack_terminal(
    message: &async_nats::jetstream::Message,
) -> Result<(), SubmissionConsumerError> {
    message
        .double_ack_with(AckKind::Term)
        .await
        .map_err(|_| SubmissionConsumerError::Ack)
}

async fn ack_retry(
    message: &async_nats::jetstream::Message,
    event_id: EventId,
    diagnostic_code: &str,
    stage: &str,
) -> Result<(), SubmissionConsumerError> {
    tracing::warn!(
        event = "evaluation.submission_frozen.retry",
        event_id = %event_id,
        diagnostic_code,
        failure_stage = stage,
    );
    message
        .ack_with(AckKind::Nak(Some(REDELIVERY_DELAY)))
        .await
        .map_err(|_| SubmissionConsumerError::Ack)
}

fn retry_freeze_store(error: &FreezeStoreError) -> bool {
    matches!(
        error,
        FreezeStoreError::Database(_) | FreezeStoreError::DatabaseBoundary
    )
}

fn authoring_diagnostic_code(error: &AuthoringAdmissionClientError) -> &'static str {
    match error {
        AuthoringAdmissionClientError::Configuration => {
            "LW_EVALUATION_AUTHORING_ADMISSION_CONFIG_INVALID"
        }
        AuthoringAdmissionClientError::Token(_) => "LW_EVALUATION_AUTHORING_ADMISSION_TOKEN_FAILED",
        AuthoringAdmissionClientError::Transport => {
            "LW_EVALUATION_AUTHORING_ADMISSION_TRANSPORT_FAILED"
        }
        AuthoringAdmissionClientError::RequestInvalid => {
            "LW_EVALUATION_AUTHORING_ADMISSION_REQUEST_INVALID"
        }
        AuthoringAdmissionClientError::ResponseTooLarge => {
            "LW_EVALUATION_AUTHORING_ADMISSION_RESPONSE_TOO_LARGE"
        }
        AuthoringAdmissionClientError::ResponseInvalid => {
            "LW_EVALUATION_AUTHORING_ADMISSION_RESPONSE_INVALID"
        }
        AuthoringAdmissionClientError::AdmissionMissing => {
            "LW_EVALUATION_AUTHORING_ADMISSION_MISSING"
        }
        AuthoringAdmissionClientError::Denied => "LW_EVALUATION_AUTHORING_ADMISSION_DENIED",
        AuthoringAdmissionClientError::Conflict => "LW_EVALUATION_AUTHORING_ADMISSION_CONFLICT",
        AuthoringAdmissionClientError::Rejected => "LW_EVALUATION_AUTHORING_ADMISSION_REJECTED",
        AuthoringAdmissionClientError::Unavailable => {
            "LW_EVALUATION_AUTHORING_ADMISSION_UNAVAILABLE"
        }
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

/// Stable consumer failures.
#[derive(Debug, thiserror::Error)]
pub enum SubmissionConsumerError {
    #[error("LW_NATS_CONFIG_INVALID")]
    Configuration,
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
