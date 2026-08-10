//! NATS request/reply boundary for Resource-owned Lease verification.

use contracts::environment::{
    EnvironmentLeaseState, EnvironmentLeaseVerificationRequest,
    EnvironmentLeaseVerificationResponse,
};
use futures_util::StreamExt;
use tokio::sync::watch;

use crate::store::PgResourceStore;

const MAX_VERIFICATION_BYTES: usize = 1024 * 1024;

/// Serves the one catalogued Environment Lease verification subject.
#[derive(Clone)]
pub struct NatsLeaseVerificationResponder {
    subject: String,
    client: async_nats::Client,
}

impl NatsLeaseVerificationResponder {
    pub fn new(
        subject: String,
        client: async_nats::Client,
    ) -> Result<Self, NatsLeaseResponderError> {
        if !valid_subject(&subject) {
            return Err(NatsLeaseResponderError::Configuration);
        }
        Ok(Self { subject, client })
    }

    /// Runs until shutdown. Invalid requests get a closed, non-authorizing response.
    pub async fn serve(
        &self,
        store: PgResourceStore,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<(), NatsLeaseResponderError> {
        let mut subscription = self
            .client
            .subscribe(self.subject.clone())
            .await
            .map_err(|_| NatsLeaseResponderError::Subscribe)?;
        loop {
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() { return Ok(()); }
                }
                message = subscription.next() => {
                    let Some(message) = message else { return Err(NatsLeaseResponderError::SubscriptionClosed); };
                    let Some(reply) = message.reply else {
                        tracing::warn!(event = "resource.lease_verification.request_rejected", diagnostic_code = "LW_RESOURCE_LEASE_REPLY_MISSING");
                        continue;
                    };
                    let response = match parse_request(&message.payload) {
                        Ok(request) => {
                            let now = match store.current_time().await {
                                Ok(now) => now,
                                Err(_error) => {
                                    tracing::error!(event = "resource.lease_verification.authority_failed", diagnostic_code = "LW_RESOURCE_LEASE_VERIFY_FAILED", error_kind = "persistence", failure_stage = "read_clock", retryable = false);
                                    continue;
                                }
                            };
                            match store.verify_environment_lease(&request, now).await {
                                Ok(response) => response,
                                Err(_error) => {
                                    tracing::error!(event = "resource.lease_verification.authority_failed", diagnostic_code = "LW_RESOURCE_LEASE_VERIFY_FAILED", error_kind = "persistence", failure_stage = "verify_lease", retryable = false);
                                    continue;
                                }
                            }
                        }
                        Err(()) => inactive_response(),
                    };
                    let payload = serde_json::to_vec(&response).map_err(|_| NatsLeaseResponderError::Serialization)?;
                    self.client.publish(reply, payload.into()).await.map_err(|_| NatsLeaseResponderError::Publish)?;
                }
            }
        }
    }
}

fn parse_request(payload: &[u8]) -> Result<EnvironmentLeaseVerificationRequest, ()> {
    if payload.len() > MAX_VERIFICATION_BYTES {
        return Err(());
    }
    contracts::parse_strict_json(payload).map_err(|_| ())
}

fn inactive_response() -> EnvironmentLeaseVerificationResponse {
    EnvironmentLeaseVerificationResponse {
        version: 1,
        state: EnvironmentLeaseState::Revoked,
        authorization: None,
    }
}

fn valid_subject(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && value.split('.').all(|token| {
            !token.is_empty()
                && token
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        })
}

#[derive(Debug, thiserror::Error)]
pub enum NatsLeaseResponderError {
    #[error("LW_RESOURCE_NATS_CONFIGURATION_INVALID")]
    Configuration,
    #[error("LW_RESOURCE_NATS_SUBSCRIBE_FAILED")]
    Subscribe,
    #[error("LW_RESOURCE_NATS_SUBSCRIPTION_CLOSED")]
    SubscriptionClosed,
    #[error("LW_RESOURCE_NATS_PUBLISH_FAILED")]
    Publish,
    #[error("LW_RESOURCE_NATS_SERIALIZATION_FAILED")]
    Serialization,
}
