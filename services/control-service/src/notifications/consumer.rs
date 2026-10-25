//! Deployment-owned `JetStream` consumer. SQL commit precedes ACK, and retries are bounded.
use super::{NotificationError, project};
use async_nats::jetstream::{AckKind, consumer::PullConsumer};
use futures_util::StreamExt;
use sqlx::PgPool;
use std::time::Duration;

/// Durable notification subscription to existing Resource owner events.
pub struct NotificationConsumer {
    messages: async_nats::jetstream::consumer::pull::Stream,
    max_deliver: i64,
}
impl NotificationConsumer {
    /// Binds the reviewed deployment-owned consumer and validates its retry bound.
    pub async fn bind(client: async_nats::Client) -> Result<Self, NotificationError> {
        let context = async_nats::jetstream::new(client);
        let stream = context
            .get_stream("LABWEAVER_RESOURCE_EVENTS")
            .await
            .map_err(|_| NotificationError::Invalid)?;
        let consumer: PullConsumer = stream
            .get_consumer("control-notifications-v1")
            .await
            .map_err(|_| NotificationError::Invalid)?;
        let config = &consumer.cached_info().config;
        if !config.filter_subject.is_empty()
            || config.filter_subjects != ["labweaver.resource.>".to_owned()]
            || !(1..=10).contains(&config.max_deliver)
            || config.ack_policy != async_nats::jetstream::consumer::AckPolicy::Explicit
        {
            return Err(NotificationError::Invalid);
        }
        let max_deliver = config.max_deliver;
        let messages = consumer
            .messages()
            .await
            .map_err(|_| NotificationError::Invalid)?;
        Ok(Self {
            messages,
            max_deliver,
        })
    }
    /// Processes until shutdown is requested. Once a message is received, its projection and ACK
    /// finish before returning; an interrupted transaction remains redeliverable in `JetStream`.
    pub async fn run(
        mut self,
        pool: PgPool,
        windows: Vec<u64>,
        email: bool,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> Result<(), NotificationError> {
        loop {
            if *shutdown.borrow() {
                return Ok(());
            }
            let message = tokio::select! {
                biased;
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() { return Ok(()); }
                    continue;
                }
                message = self.messages.next() => message,
            };
            let Some(message) = message else {
                return Err(NotificationError::Invalid);
            };
            let message = message.map_err(|_| NotificationError::Invalid)?;
            let result = project(&pool, &message.payload, &windows, email).await;
            let ack = match result {
                Ok(()) => AckKind::Ack,
                Err(NotificationError::Persistence(_))
                    if message
                        .info()
                        .map_err(|_| NotificationError::Invalid)?
                        .delivered
                        < self.max_deliver =>
                {
                    AckKind::Nak(Some(Duration::from_secs(5)))
                }
                Err(error) => {
                    tracing::error!(diagnostic=%error, "notification event delivery exhausted or rejected");
                    AckKind::Term
                }
            };
            message
                .double_ack_with(ack)
                .await
                .map_err(|_| NotificationError::Invalid)?;
        }
    }
}
