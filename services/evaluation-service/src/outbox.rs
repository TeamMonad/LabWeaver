//! Transactional Evaluation Outbox publication to `JetStream`.
#![allow(
    missing_docs,
    clippy::missing_errors_doc,
    reason = "the internal dispatcher exposes only stable diagnostics"
)]

use persistence_sqlx::Sha256Digest;
use std::{error::Error as _, str::FromStr, time::Duration}; // internal persistence hash, not contract hash

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
        let diagnostic_subject = subject.clone();
        let diagnostic_event_id = event_id.to_string();
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
        .map_err(|_| EvaluationOutboxError::PublishTimeout {
            subject: diagnostic_subject.clone(),
            event_id: diagnostic_event_id.clone(),
            reason: "timeout",
            server_code: "none".to_owned(),
            stage: "publish_request",
        })?
        .map_err(|error| {
            let context = nats_publish_error_context(&error);
            EvaluationOutboxError::PublishUnavailable {
                subject: diagnostic_subject.clone(),
                event_id: diagnostic_event_id.clone(),
                reason: context.reason,
                server_code: context.server_code,
                stage: "publish_request",
            }
        })?;
        timeout(self.publish_timeout, acknowledgement)
            .await
            .map_err(|_| EvaluationOutboxError::PublishTimeout {
                subject: diagnostic_subject.clone(),
                event_id: diagnostic_event_id.clone(),
                reason: "timeout",
                server_code: "none".to_owned(),
                stage: "publish_ack",
            })?
            .map_err(|error| {
                let context = nats_publish_error_context(&error);
                EvaluationOutboxError::PublishRejected {
                    subject: diagnostic_subject,
                    event_id: diagnostic_event_id,
                    reason: context.reason,
                    server_code: context.server_code,
                    stage: "publish_ack",
                }
            })?;
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
    #[error(
        "LW_EVALUATION_OUTBOX_PUBLISH_TIMEOUT subject={subject} event_id={event_id} reason={reason} server_code={server_code} stage={stage}"
    )]
    PublishTimeout {
        subject: String,
        event_id: String,
        reason: &'static str,
        server_code: String,
        stage: &'static str,
    },
    #[error(
        "LW_EVALUATION_OUTBOX_PUBLISH_UNAVAILABLE subject={subject} event_id={event_id} reason={reason} server_code={server_code} stage={stage}"
    )]
    PublishUnavailable {
        subject: String,
        event_id: String,
        reason: &'static str,
        server_code: String,
        stage: &'static str,
    },
    #[error(
        "LW_EVALUATION_OUTBOX_PUBLISH_REJECTED subject={subject} event_id={event_id} reason={reason} server_code={server_code} stage={stage}"
    )]
    PublishRejected {
        subject: String,
        event_id: String,
        reason: &'static str,
        server_code: String,
        stage: &'static str,
    },
    #[error("LW_EVALUATION_OUTBOX_FENCE_LOST")]
    PublishFenceLost,
    #[error("LW_EVALUATION_OUTBOX_DATABASE_FAILED")]
    Database(#[from] sqlx::Error),
    #[error("LW_EVALUATION_OUTBOX_PAYLOAD_CONTRACT_INVALID")]
    Serialization(#[from] serde_json::Error),
}

struct NatsPublishErrorContext {
    reason: &'static str,
    server_code: String,
}

fn nats_publish_error_context(
    error: &async_nats::jetstream::context::PublishError,
) -> NatsPublishErrorContext {
    use async_nats::jetstream::context::PublishErrorKind;

    let reason = match error.kind() {
        PublishErrorKind::StreamNotFound => "no_responders",
        PublishErrorKind::WrongLastMessageId => "wrong_last_message_id",
        PublishErrorKind::WrongLastSequence => "wrong_last_sequence",
        PublishErrorKind::TimedOut => "timeout",
        PublishErrorKind::BrokenPipe => "broken_pipe",
        PublishErrorKind::MaxAckPending => "max_ack_pending",
        PublishErrorKind::Other => {
            if let Some(server_error) = error
                .source()
                .and_then(|source| source.downcast_ref::<async_nats::ServerError>())
            {
                match server_error {
                    async_nats::ServerError::AuthorizationViolation => {
                        return NatsPublishErrorContext {
                            reason: "authorization_violation",
                            server_code: "none".to_owned(),
                        };
                    }
                    async_nats::ServerError::Other(description)
                        if description.to_ascii_lowercase().contains("permission")
                            || description.to_ascii_lowercase().contains("authorization") =>
                    {
                        return NatsPublishErrorContext {
                            reason: "authorization_violation",
                            server_code: "none".to_owned(),
                        };
                    }
                    async_nats::ServerError::SlowConsumer(_) => "slow_consumer",
                    async_nats::ServerError::Other(_) => "server_error",
                }
            } else if let Some(jetstream_error) = error
                .source()
                .and_then(|source| source.downcast_ref::<async_nats::jetstream::Error>())
            {
                return NatsPublishErrorContext {
                    reason: "jetstream_api_error",
                    server_code: jetstream_error.error_code().0.to_string(),
                };
            } else {
                "other"
            }
        }
    };

    NatsPublishErrorContext {
        reason,
        server_code: "none".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::{EvaluationOutboxError, nats_publish_error_context};

    #[test]
    fn publish_rejection_diagnostic_identifies_event_context_without_payload() {
        let error = EvaluationOutboxError::PublishRejected {
            subject: "labweaver.evaluation.release.published.v1".to_owned(),
            event_id: "01900000-0000-7000-8000-000000000001".to_owned(),
            reason: "authorization_violation",
            server_code: "none".to_owned(),
            stage: "publish_ack",
        };
        let diagnostic = error.to_string();
        assert!(diagnostic.contains("LW_EVALUATION_OUTBOX_PUBLISH_REJECTED"));
        assert!(diagnostic.contains("subject=labweaver.evaluation.release.published.v1"));
        assert!(diagnostic.contains("event_id=01900000-0000-7000-8000-000000000001"));
        assert!(diagnostic.contains("reason=authorization_violation"));
        assert!(diagnostic.contains("server_code=none"));
        assert!(diagnostic.contains("stage=publish_ack"));
        assert!(!diagnostic.contains("token"));
        assert!(!diagnostic.contains("payload"));
    }

    #[test]
    fn publish_failure_context_preserves_no_responders_and_permission_categories() {
        let no_responders = async_nats::jetstream::context::PublishError::new(
            async_nats::jetstream::context::PublishErrorKind::StreamNotFound,
        );
        let no_responders_context = nats_publish_error_context(&no_responders);
        assert_eq!(no_responders_context.reason, "no_responders");
        assert_eq!(no_responders_context.server_code, "none");

        let permission = async_nats::jetstream::context::PublishError::with_source(
            async_nats::jetstream::context::PublishErrorKind::Other,
            async_nats::ServerError::Other("Permissions Violation for Publish".to_owned()),
        );
        let permission_context = nats_publish_error_context(&permission);
        assert_eq!(permission_context.reason, "authorization_violation");
        assert_eq!(permission_context.server_code, "none");
    }
}
