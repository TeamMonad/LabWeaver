//! Resource Outbox publication with durable claims and JetStream acknowledgements.

#![allow(
    clippy::doc_markdown,
    reason = "wire terms such as JetStream and published_at are protocol identifiers"
)]

use std::str::FromStr;
use std::time::{Duration, Instant};

use async_nats::jetstream::message::PublishMessage;
use contracts::EventId;
use contracts::events::{CloudEvent, EVENT_CONTRACTS};
use persistence_sqlx::Sha256Digest; // internal persistence hash, not contract hash
use serde_json::Value;
use sqlx::{PgPool, Row};
use tokio::time::timeout;
use uuid::Uuid;

/// Bounded dispatcher. A short database statement claims an event before transport starts.
/// Transport acknowledgement precedes `published_at`, so a crash permits an idempotent replay.
#[derive(Clone)]
pub struct ResourceOutboxDispatcher {
    pool: PgPool,
    jetstream: async_nats::jetstream::Context,
    timeout: Duration,
}

impl ResourceOutboxDispatcher {
    pub fn new(
        pool: PgPool,
        client: async_nats::Client,
        timeout: Duration,
    ) -> Result<Self, ResourceOutboxError> {
        if timeout.is_zero() || timeout > Duration::from_mins(5) {
            return Err(ResourceOutboxError::Configuration);
        }
        Ok(Self {
            pool,
            jetstream: async_nats::jetstream::new(client),
            timeout,
        })
    }

    pub async fn dispatch_once(&self) -> Result<ResourceOutboxOutcome, ResourceOutboxError> {
        let started = Instant::now();
        let claim_token = Uuid::new_v4();
        // Allow both bounded transport waits and a recovery margin. Correctness depends on
        // the token: a paused worker or delayed database mark may outlive this lease.
        let lease_millis = i64::try_from((self.timeout * 2 + Duration::from_secs(1)).as_millis())
            .map_err(|_| ResourceOutboxError::Configuration)?;
        let row = sqlx::query(
            "WITH candidate AS (
                 SELECT event_id FROM resource.outbox_events
                 WHERE published_at IS NULL
                   AND (publish_claim_until IS NULL OR publish_claim_until<=clock_timestamp())
                 ORDER BY created_at,event_id FOR UPDATE SKIP LOCKED LIMIT 1
             )
             UPDATE resource.outbox_events AS event
             SET publish_claim_token=$1,
                 publish_claim_until=clock_timestamp()+$2::bigint*interval '1 millisecond'
             FROM candidate WHERE event.event_id=candidate.event_id
             RETURNING event.event_id,event.subject,event.event_type,event.payload,event.payload_sha256",
        )
        .bind(claim_token)
        .bind(lease_millis)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            tracing::debug!(
                event = "resource.outbox.idle",
                component = "outbox",
                operation = "nats.publish",
                outcome = "idle",
                duration_ms = elapsed_millis(started),
            );
            return Ok(ResourceOutboxOutcome::Idle);
        };
        let event_id: Uuid = row.try_get("event_id")?;
        let subject: String = row.try_get("subject")?;
        let event_type: String = row.try_get("event_type")?;
        let payload: Value = row.try_get("payload")?;
        let hash: String = row.try_get("payload_sha256")?;
        let result = self
            .publish_claim(
                event_id,
                &subject,
                &event_type,
                &payload,
                &hash,
                claim_token,
            )
            .await;
        if result.is_err() {
            // Best-effort early retry. Cancellation or a database outage leaves a durable
            // lease for takeover; an old worker can never clear a replacement's token.
            let release = sqlx::query(
                "UPDATE resource.outbox_events
                 SET publish_claim_token=NULL,publish_claim_until=NULL
                 WHERE event_id=$1 AND publish_claim_token=$2 AND published_at IS NULL",
            )
            .bind(event_id)
            .bind(claim_token)
            .execute(&self.pool);
            if !matches!(timeout(self.timeout, release).await, Ok(Ok(_))) {
                tracing::warn!(
                    event = "resource.outbox.claim_release_failed",
                    component = "outbox",
                    event_id = %event_id,
                );
            }
        }
        let event = result?;
        tracing::info!(
            event = "resource.outbox.published",
            component = "outbox",
            operation = "nats.publish",
            outcome = "succeeded",
            duration_ms = elapsed_millis(started),
            trace_id = event.trace_id,
            event_id = %event.id,
            message_id = %event.id,
            subject = event.subject,
        );
        Ok(ResourceOutboxOutcome::Published { event_id: event.id })
    }

    async fn publish_claim(
        &self,
        event_id: Uuid,
        subject: &str,
        event_type: &str,
        payload: &Value,
        hash: &str,
        claim_token: Uuid,
    ) -> Result<CloudEvent<Value>, ResourceOutboxError> {
        if Sha256Digest::of_canonical(payload)
            .map_err(|_| ResourceOutboxError::Identity)?
            .to_string()
            != hash
        {
            return Err(ResourceOutboxError::Identity);
        }
        let event: CloudEvent<Value> =
            serde_json::from_value(payload.clone()).map_err(|_| ResourceOutboxError::Contract)?;
        let expected_id =
            EventId::from_str(&event_id.to_string()).map_err(|_| ResourceOutboxError::Identity)?;
        let contract = EVENT_CONTRACTS
            .iter()
            .copied()
            .find(|candidate| candidate.subject == subject)
            .ok_or(ResourceOutboxError::Contract)?;
        if event.id != expected_id || event_type != subject {
            return Err(ResourceOutboxError::Identity);
        }
        event
            .validate(contract)
            .map_err(|_| ResourceOutboxError::Contract)?;
        let wire = serde_json::to_vec(&event).map_err(|_| ResourceOutboxError::Contract)?;
        let acknowledgement = timeout(
            self.timeout,
            self.jetstream.send_publish(
                subject.to_owned(),
                PublishMessage::build()
                    .payload(wire.into())
                    .message_id(event_id.to_string()),
            ),
        )
        .await
        .map_err(|_| ResourceOutboxError::Timeout)?
        .map_err(|_| ResourceOutboxError::Publish)?;
        timeout(self.timeout, acknowledgement)
            .await
            .map_err(|_| ResourceOutboxError::Timeout)?
            .map_err(|_| ResourceOutboxError::Publish)?;
        let updated = sqlx::query(
            "UPDATE resource.outbox_events
             SET published_at=date_trunc('milliseconds',clock_timestamp()),
                 publish_claim_token=NULL,publish_claim_until=NULL
             WHERE event_id=$1 AND publish_claim_token=$2 AND published_at IS NULL
               AND subject=$3 AND event_type=$4 AND payload_sha256=$5 AND payload=$6",
        )
        .bind(event_id)
        .bind(claim_token)
        .bind(subject)
        .bind(event_type)
        .bind(hash)
        .bind(payload)
        .execute(&self.pool)
        .await?;
        if updated.rows_affected() != 1 {
            // A replacement may already have published the same immutable event. Read in a
            // new statement so its commit is visible even if it raced the conditional UPDATE.
            let published: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM resource.outbox_events
                 WHERE event_id=$1 AND published_at IS NOT NULL
                   AND subject=$2 AND event_type=$3 AND payload_sha256=$4 AND payload=$5)",
            )
            .bind(event_id)
            .bind(subject)
            .bind(event_type)
            .bind(hash)
            .bind(payload)
            .fetch_one(&self.pool)
            .await?;
            if !published {
                return Err(ResourceOutboxError::FenceLost);
            }
        }
        Ok(event)
    }
}

fn elapsed_millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceOutboxOutcome {
    Idle,
    Published { event_id: EventId },
}

#[derive(Debug, thiserror::Error)]
pub enum ResourceOutboxError {
    #[error("LW_RESOURCE_OUTBOX_CONFIGURATION_INVALID")]
    Configuration,
    #[error("LW_RESOURCE_OUTBOX_IDENTITY_INVALID")]
    Identity,
    #[error("LW_RESOURCE_OUTBOX_CONTRACT_INVALID")]
    Contract,
    #[error("LW_RESOURCE_OUTBOX_PUBLISH_TIMEOUT")]
    Timeout,
    #[error("LW_RESOURCE_OUTBOX_PUBLISH_FAILED")]
    Publish,
    #[error("LW_RESOURCE_OUTBOX_FENCE_LOST")]
    FenceLost,
    #[error("LW_RESOURCE_OUTBOX_DATABASE_FAILED")]
    Database(#[from] sqlx::Error),
}

impl ResourceOutboxError {
    pub(crate) fn error_kind(&self) -> &'static str {
        match self {
            Self::Configuration => "configuration",
            Self::Identity => "identity",
            Self::Contract => "contract",
            Self::Timeout => "timeout",
            Self::Publish => "transport",
            Self::FenceLost => "concurrency",
            Self::Database(_) => "database",
        }
    }

    pub(crate) fn failure_stage(&self) -> &'static str {
        match self {
            Self::Configuration => "outbox.configuration",
            Self::Identity | Self::Contract => "outbox.validate",
            Self::Timeout | Self::Publish => "outbox.publish",
            Self::FenceLost => "outbox.mark_published",
            Self::Database(_) => "outbox.database",
        }
    }

    pub(crate) fn safe_detail(&self) -> String {
        match self {
            Self::Configuration => "configuration_invalid".to_owned(),
            Self::Identity => "identity_invalid".to_owned(),
            Self::Contract => "contract_invalid".to_owned(),
            Self::Timeout => "publish_timeout".to_owned(),
            Self::Publish => "publish_failed".to_owned(),
            Self::FenceLost => "fence_lost".to_owned(),
            Self::Database(error) => crate::store::safe_sqlstate_detail(error),
        }
    }
}
