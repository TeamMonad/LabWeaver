//! Explicit mTLS clients for Control-owned authorization and Agent coordination.
#![allow(
    missing_docs,
    clippy::missing_errors_doc,
    reason = "deployment YAML keys are documented by the checked-in example configuration"
)]

use std::time::Duration;

use contracts::authoring::{AgentRun, AgentTrackKind};
use contracts::http::{
    IdempotencyKey, InternalAgentRunMutationRequest, InternalAgentRunOutcome,
    InternalCreateAgentRunRequest, InternalImageArtifactResolution,
};
use contracts::{AgentRunId, AuthorizationDecision, AuthorizationDecisionRequest, ImageArtifactId};
use reqwest::{Certificate, Identity, StatusCode, Url};
use serde::Deserialize;
use thiserror::Error;

/// Non-secret downstream endpoint plus mounted mTLS credential locators.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MtlsClientFileConfig {
    pub base_url: Url,
    pub ca_certificate_file: String,
    pub client_certificate_file: String,
    pub client_private_key_file: String,
    pub timeout_milliseconds: u64,
}

impl MtlsClientFileConfig {
    /// Creates one bounded client without ambient proxies or credentials.
    pub fn build(&self) -> Result<reqwest::Client, DownstreamError> {
        if self.base_url.scheme() != "https"
            || self.base_url.host_str().is_none()
            || self.timeout_milliseconds == 0
            || self.timeout_milliseconds > 30_000
        {
            return Err(DownstreamError::Configuration);
        }
        let ca =
            std::fs::read(&self.ca_certificate_file).map_err(|_| DownstreamError::Configuration)?;
        let mut identity = std::fs::read(&self.client_certificate_file)
            .map_err(|_| DownstreamError::Configuration)?;
        identity.extend_from_slice(b"\n");
        identity.extend_from_slice(
            &std::fs::read(&self.client_private_key_file)
                .map_err(|_| DownstreamError::Configuration)?,
        );
        reqwest::Client::builder()
            .no_proxy()
            .https_only(true)
            .timeout(Duration::from_millis(self.timeout_milliseconds))
            .add_root_certificate(
                Certificate::from_pem(&ca).map_err(|_| DownstreamError::Configuration)?,
            )
            .identity(Identity::from_pem(&identity).map_err(|_| DownstreamError::Configuration)?)
            .build()
            .map_err(|_| DownstreamError::Configuration)
    }

    fn endpoint(&self, path: &str) -> Result<Url, DownstreamError> {
        self.base_url
            .join(path.trim_start_matches('/'))
            .map_err(|_| DownstreamError::Configuration)
    }
}

/// Access Service authorization adapter.
#[derive(Clone, Debug)]
pub struct AccessClient {
    config: MtlsClientFileConfig,
    client: reqwest::Client,
}

impl AccessClient {
    pub fn new(config: MtlsClientFileConfig) -> Result<Self, DownstreamError> {
        let client = config.build()?;
        Ok(Self { config, client })
    }

    pub async fn authorize(
        &self,
        request: &AuthorizationDecisionRequest,
    ) -> Result<AuthorizationDecision, DownstreamError> {
        send_json(
            self.client
                .post(self.config.endpoint("internal/v1/auth/decision")?)
                .json(request),
        )
        .await
    }
}

/// Agent Service authority adapter.
#[derive(Clone, Debug)]
pub struct AgentClient {
    config: MtlsClientFileConfig,
    client: reqwest::Client,
}

impl AgentClient {
    pub fn new(config: MtlsClientFileConfig) -> Result<Self, DownstreamError> {
        let client = config.build()?;
        Ok(Self { config, client })
    }

    pub async fn create(
        &self,
        request: &InternalCreateAgentRunRequest,
        key: &IdempotencyKey,
    ) -> Result<AgentRun, DownstreamError> {
        send_json(
            self.client
                .post(self.config.endpoint("internal/v1/agent-runs")?)
                .header("Idempotency-Key", key.as_str())
                .json(request),
        )
        .await
    }

    pub async fn get(&self, run_id: AgentRunId) -> Result<AgentRun, DownstreamError> {
        send_json(
            self.client.get(
                self.config
                    .endpoint(&format!("internal/v1/agent-runs/{run_id}"))?,
            ),
        )
        .await
    }

    pub async fn cancel(
        &self,
        run_id: AgentRunId,
        request: &InternalAgentRunMutationRequest,
        key: &IdempotencyKey,
    ) -> Result<AgentRun, DownstreamError> {
        send_json(
            self.client
                .post(
                    self.config
                        .endpoint(&format!("internal/v1/agent-runs/{run_id}/cancel"))?,
                )
                .header("Idempotency-Key", key.as_str())
                .json(request),
        )
        .await
    }

    pub async fn retry(
        &self,
        run_id: AgentRunId,
        track: AgentTrackKind,
        request: &InternalAgentRunMutationRequest,
        key: &IdempotencyKey,
    ) -> Result<AgentRun, DownstreamError> {
        let track = match track {
            AgentTrackKind::Environment => "environment",
            AgentTrackKind::Evaluation => "evaluation",
        };
        send_json(
            self.client
                .post(self.config.endpoint(&format!(
                    "internal/v1/agent-runs/{run_id}/tracks/{track}/retry"
                ))?)
                .header("Idempotency-Key", key.as_str())
                .json(request),
        )
        .await
    }

    pub async fn outcome(
        &self,
        run_id: AgentRunId,
    ) -> Result<InternalAgentRunOutcome, DownstreamError> {
        let outcome: InternalAgentRunOutcome = send_json(
            self.client.get(
                self.config
                    .endpoint(&format!("internal/v1/agent-runs/{run_id}/outcome"))?,
            ),
        )
        .await?;
        outcome
            .validate()
            .map_err(|_| DownstreamError::IdentityMismatch)?;
        Ok(outcome)
    }

    pub async fn artifact(
        &self,
        artifact_id: ImageArtifactId,
    ) -> Result<InternalImageArtifactResolution, DownstreamError> {
        let resolution: InternalImageArtifactResolution = send_json(
            self.client.get(
                self.config
                    .endpoint(&format!("internal/v1/image-artifacts/{artifact_id}"))?,
            ),
        )
        .await?;
        resolution
            .validate()
            .map_err(|_| DownstreamError::IdentityMismatch)?;
        Ok(resolution)
    }
}

async fn send_json<T: serde::de::DeserializeOwned>(
    request: reqwest::RequestBuilder,
) -> Result<T, DownstreamError> {
    let response = request
        .send()
        .await
        .map_err(|_| DownstreamError::Unavailable)?;
    let status = response.status();
    if status == StatusCode::NOT_FOUND {
        return Err(DownstreamError::NotFound);
    }
    if status == StatusCode::CONFLICT || status == StatusCode::PRECONDITION_FAILED {
        return Err(DownstreamError::Conflict);
    }
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        return Err(DownstreamError::Denied);
    }
    if !status.is_success() {
        return Err(DownstreamError::Unavailable);
    }
    response
        .json()
        .await
        .map_err(|_| DownstreamError::ProtocolInvalid)
}

/// Payload-free downstream failure classification.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum DownstreamError {
    #[error("LW_CONTROL_DOWNSTREAM_CONFIG_INVALID")]
    Configuration,
    #[error("LW_CONTROL_DOWNSTREAM_UNAVAILABLE")]
    Unavailable,
    #[error("LW_CONTROL_DOWNSTREAM_PROTOCOL_INVALID")]
    ProtocolInvalid,
    #[error("LW_CONTROL_DOWNSTREAM_IDENTITY_MISMATCH")]
    IdentityMismatch,
    #[error("LW_CONTROL_DOWNSTREAM_NOT_FOUND")]
    NotFound,
    #[error("LW_CONTROL_DOWNSTREAM_CONFLICT")]
    Conflict,
    #[error("LW_AUTH_SCOPE_DENIED")]
    Denied,
}
