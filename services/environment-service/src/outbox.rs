use std::str::FromStr;
use std::time::Duration;

use async_trait::async_trait;
use contracts::EventId;
use contracts::events::{CloudEvent, EVENT_CONTRACTS};
use persistence_sqlx::Sha256Digest; // internal persistence hash, not contract hash
use serde_json::Value;
use sqlx::{PgPool, Row};
use tokio::time::timeout;
use uuid::Uuid;

/// Sanitized transport failure returned by an explicit event publisher implementation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PublishFailure {
    #[error("LW_ENVIRONMENT_EVENT_PUBLISH_UNAVAILABLE")]
    Unavailable,
    #[error("LW_ENVIRONMENT_EVENT_PUBLISH_REJECTED")]
    Rejected,
}

/// Explicit transport boundary; success means the broker acknowledged durable publication.
#[async_trait]
pub trait EnvironmentEventPublisher: Send + Sync {
    async fn publish(&self, subject: &str, event: &CloudEvent<Value>)
    -> Result<(), PublishFailure>;
}

/// Bounded Outbox dispatcher with durable claims; network waits never retain a connection.
pub struct OutboxDispatcher<P> {
    pool: PgPool,
    publisher: P,
    publish_timeout: Duration,
}

impl<P> OutboxDispatcher<P>
where
    P: EnvironmentEventPublisher,
{
    pub fn new(
        pool: PgPool,
        publisher: P,
        publish_timeout: Duration,
    ) -> Result<Self, OutboxDispatchError> {
        if publish_timeout.is_zero() || publish_timeout > Duration::from_mins(5) {
            return Err(OutboxDispatchError::InvalidConfiguration);
        }
        Ok(Self {
            pool,
            publisher,
            publish_timeout,
        })
    }

    /// Runs the production polling loop, stopping acquisition before bounded process drain.
    pub async fn run(
        &self,
        interval: Duration,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> Result<(), OutboxDispatchError> {
        if interval.is_zero() || interval > Duration::from_mins(1) {
            return Err(OutboxDispatchError::InvalidConfiguration);
        }
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            if *shutdown.borrow() {
                return Ok(());
            }
            tokio::select! {
                biased;
                _ = shutdown.changed() => return Ok(()),
                _ = ticker.tick() => {}
            }
            match self.dispatch_once().await {
                Ok(OutboxDispatchOutcome::Published { event_id }) => {
                    tracing::info!(event = "environment.outbox.published", event_id = %event_id);
                }
                Ok(OutboxDispatchOutcome::Idle) => {}
                Err(error) if error.retryable() => {
                    tracing::warn!(event = "environment.outbox.retry_scheduled", error = %error, retryable = true);
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Publishes at most one row. Cancellation leaves a lease for at-least-once replay.
    pub async fn dispatch_once(&self) -> Result<OutboxDispatchOutcome, OutboxDispatchError> {
        let token = Uuid::new_v4();
        let lease_millis =
            i64::try_from((self.publish_timeout + Duration::from_secs(1)).as_millis())
                .map_err(|_| OutboxDispatchError::InvalidConfiguration)?;
        let row = sqlx::query(
            "WITH candidate AS (
                 SELECT event_id FROM environment.outbox_events
                 WHERE published_at IS NULL
                   AND (publish_claim_until IS NULL OR publish_claim_until<=clock_timestamp())
                 ORDER BY created_at,event_id FOR UPDATE SKIP LOCKED LIMIT 1
             )
             UPDATE environment.outbox_events AS event
             SET publish_claim_token=$1,
                 publish_claim_until=clock_timestamp()+$2::bigint*interval '1 millisecond'
             FROM candidate WHERE event.event_id=candidate.event_id
             RETURNING event.event_id,event.subject,event.event_type,event.payload,event.payload_sha256",
        )
        .bind(token)
        .bind(lease_millis)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(OutboxDispatchOutcome::Idle);
        };
        let event_uuid: Uuid = row.try_get("event_id")?;
        let subject: String = row.try_get("subject")?;
        let event_type: String = row.try_get("event_type")?;
        let payload: Value = row.try_get("payload")?;
        let stored_hash: String = row.try_get("payload_sha256")?;
        let result = self
            .publish_claim(
                event_uuid,
                &subject,
                &event_type,
                &payload,
                &stored_hash,
                token,
            )
            .await;
        if result.is_err() {
            // Failure permits an immediate retry. Cancellation/DB failure relies on expiry.
            // A late owner must not release a replacement's claim.
            let release = sqlx::query(
                "UPDATE environment.outbox_events SET publish_claim_token=NULL,publish_claim_until=NULL
                 WHERE event_id=$1 AND publish_claim_token=$2 AND published_at IS NULL",
            ).bind(event_uuid).bind(token).execute(&self.pool);
            if !matches!(timeout(self.publish_timeout, release).await, Ok(Ok(_))) {
                tracing::warn!(event = "environment.outbox.claim_release_failed", event_id = %event_uuid);
            }
        }
        result
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the immutable event snapshot is the publication fence"
    )]
    async fn publish_claim(
        &self,
        event_uuid: Uuid,
        subject: &str,
        event_type: &str,
        payload: &Value,
        stored_hash: &str,
        token: Uuid,
    ) -> Result<OutboxDispatchOutcome, OutboxDispatchError> {
        let calculated_hash = Sha256Digest::of_canonical(payload)
            .map_err(|_| OutboxDispatchError::PayloadIdentityInvalid)?;
        if stored_hash != calculated_hash.to_string() {
            return Err(OutboxDispatchError::PayloadIdentityInvalid);
        }
        let event: CloudEvent<Value> = serde_json::from_value(payload.clone())
            .map_err(|_| OutboxDispatchError::PayloadContractInvalid)?;
        let event_id = EventId::from_str(&event_uuid.to_string())
            .map_err(|_| OutboxDispatchError::PayloadIdentityInvalid)?;
        let contract = EVENT_CONTRACTS
            .iter()
            .copied()
            .find(|contract| contract.subject == subject)
            .ok_or(OutboxDispatchError::PayloadContractInvalid)?;
        if event.id != event_id || event_type != subject {
            return Err(OutboxDispatchError::PayloadIdentityInvalid);
        }
        event
            .validate(contract)
            .map_err(|_| OutboxDispatchError::PayloadContractInvalid)?;
        timeout(
            self.publish_timeout,
            self.publisher.publish(subject, &event),
        )
        .await
        .map_err(|_| OutboxDispatchError::PublishTimeout)??;
        let updated = sqlx::query(
            "UPDATE environment.outbox_events
             SET published_at=date_trunc('milliseconds',clock_timestamp()),
                 publish_claim_token=NULL,publish_claim_until=NULL
             WHERE event_id=$1 AND publish_claim_token=$2 AND published_at IS NULL
               AND subject=$3 AND event_type=$4 AND payload_sha256=$5 AND payload=$6",
        )
        .bind(event_uuid)
        .bind(token)
        .bind(subject)
        .bind(event_type)
        .bind(stored_hash)
        .bind(payload)
        .execute(&self.pool)
        .await?;
        if updated.rows_affected() != 1 {
            let published: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM environment.outbox_events
                 WHERE event_id=$1 AND published_at IS NOT NULL
                   AND subject=$2 AND event_type=$3 AND payload_sha256=$4 AND payload=$5)",
            )
            .bind(event_uuid)
            .bind(subject)
            .bind(event_type)
            .bind(stored_hash)
            .bind(payload)
            .fetch_one(&self.pool)
            .await?;
            if !published {
                return Err(OutboxDispatchError::PublishFenceLost);
            }
        }
        Ok(OutboxDispatchOutcome::Published { event_id })
    }
}

/// Outcome of one bounded dispatcher iteration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutboxDispatchOutcome {
    Idle,
    Published { event_id: EventId },
}

/// Stable Outbox dispatch failure without transport payloads or secrets.
#[derive(Debug, thiserror::Error)]
pub enum OutboxDispatchError {
    #[error("LW_ENVIRONMENT_OUTBOX_CONFIGURATION_INVALID")]
    InvalidConfiguration,
    #[error("LW_ENVIRONMENT_OUTBOX_PAYLOAD_IDENTITY_INVALID")]
    PayloadIdentityInvalid,
    #[error("LW_ENVIRONMENT_OUTBOX_PAYLOAD_CONTRACT_INVALID")]
    PayloadContractInvalid,
    #[error("LW_ENVIRONMENT_OUTBOX_PUBLISH_TIMEOUT")]
    PublishTimeout,
    #[error("LW_ENVIRONMENT_OUTBOX_PUBLISH_FENCE_LOST")]
    PublishFenceLost,
    #[error("LW_ENVIRONMENT_OUTBOX_DATABASE_FAILED")]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Publish(#[from] PublishFailure),
}

impl OutboxDispatchError {
    /// Transport interruption and transient database failure retain the durable event for retry.
    #[must_use]
    pub fn retryable(&self) -> bool {
        match self {
            Self::Database(sqlx::Error::Database(error)) => error.code().is_some_and(|code| {
                code.starts_with("08")
                    || code.starts_with("40")
                    || code.starts_with("53")
                    || code.starts_with("57")
                    || code == "55P03"
            }),
            Self::PublishTimeout
            | Self::Publish(_)
            | Self::PublishFenceLost
            | Self::Database(_) => true,
            Self::InvalidConfiguration
            | Self::PayloadIdentityInvalid
            | Self::PayloadContractInvalid => false,
        }
    }
}
