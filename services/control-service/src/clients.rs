//! Explicit TLS clients for Control-owned authorization and Agent coordination.
#![allow(
    missing_docs,
    clippy::missing_errors_doc,
    reason = "deployment YAML keys are documented by the checked-in example configuration"
)]

use std::{
    collections::BTreeSet,
    fmt,
    sync::Arc,
    time::{Duration, Instant},
};

use auth::ServiceTokenClient;

use contracts::authoring::{AgentRun, AgentTrackKind};
use contracts::environment::{
    EnvironmentWorkConfigurationTarget, EnvironmentWorkConfigurationTargetQuery,
};
use contracts::evaluation::EvaluationRelease;
use contracts::http::{
    AgentWorkExecutionIntentMetadata, AgentWorkExecutionIntentQuery, CursorPage,
    EvaluationReleaseListQuery, GeneratedArtifactQuery, GeneratedArtifactRecord, IdempotencyKey,
    InternalAgentBuildCancellationRequest, InternalAgentBuildCancellationResult,
    InternalAgentBuildStatusQuery, InternalAgentRunMutationRequest, InternalAgentRunOutcome,
    InternalApproveWorkConfigurationRequest, InternalCreateAgentRunRequest,
    InternalImageArtifactResolution, InternalPublishEvaluationReleaseRequest,
    InternalWithdrawEvaluationReleaseRequest,
};
use contracts::{
    AgentRunId, AuthorizationDecision, AuthorizationDecisionRequest, BuildRequestId,
    EvaluationReleaseId, ImageArtifactId,
};
use reqwest::{Certificate, StatusCode, Url};
use serde::Deserialize;
use thiserror::Error;

/// Non-secret downstream endpoint plus the CA used for server-only TLS.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceHttpClientConfig {
    pub base_url: Url,
    pub ca_certificate_file: String,
    pub timeout_milliseconds: u64,
}

impl ServiceHttpClientConfig {
    /// Creates one bounded TLS client without ambient proxies or credentials.
    pub fn build(&self) -> Result<reqwest::Client, DownstreamError> {
        if self.base_url.scheme() != "https"
            || self.base_url.host_str().is_none()
            || self.timeout_milliseconds == 0
            || self.timeout_milliseconds > 30_000
            || self.ca_certificate_file.trim().is_empty()
        {
            return Err(DownstreamError::Configuration);
        }
        let mut builder = reqwest::Client::builder()
            .no_proxy()
            .https_only(true)
            .tls_built_in_root_certs(false)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_millis(self.timeout_milliseconds));
        let ca =
            std::fs::read(&self.ca_certificate_file).map_err(|_| DownstreamError::Configuration)?;
        let roots =
            Certificate::from_pem_bundle(&ca).map_err(|_| DownstreamError::Configuration)?;
        if roots.is_empty() {
            return Err(DownstreamError::Configuration);
        }
        for root in roots {
            builder = builder.add_root_certificate(root);
        }
        builder.build().map_err(|_| DownstreamError::Configuration)
    }

    fn endpoint(&self, path: &str) -> Result<Url, DownstreamError> {
        self.base_url
            .join(path.trim_start_matches('/'))
            .map_err(|_| DownstreamError::Configuration)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ServiceTokenTarget {
    audience: &'static str,
    scopes: &'static [&'static str],
}

const ACCESS_SERVICE_TARGET: ServiceTokenTarget = ServiceTokenTarget {
    audience: "labweaver-access",
    scopes: &["access.authorization.decide"],
};
const AGENT_SERVICE_TARGET: ServiceTokenTarget = ServiceTokenTarget {
    audience: "labweaver-agent",
    scopes: &["agent.control.invoke"],
};
const ENVIRONMENT_SERVICE_TARGET: ServiceTokenTarget = ServiceTokenTarget {
    audience: "labweaver-environment",
    scopes: &["environment.work.read"],
};
const EVALUATION_SERVICE_TARGET: ServiceTokenTarget = ServiceTokenTarget {
    audience: "labweaver-evaluation",
    scopes: &["evaluation.control.invoke"],
};

impl ServiceTokenTarget {
    fn scopes(self) -> BTreeSet<String> {
        self.scopes
            .iter()
            .map(|scope| (*scope).to_owned())
            .collect()
    }
}

/// Access Service authorization adapter.
#[derive(Clone)]
pub struct AccessClient {
    config: ServiceHttpClientConfig,
    client: reqwest::Client,
    service_token_client: Arc<ServiceTokenClient>,
    token_target: ServiceTokenTarget,
}

impl AccessClient {
    pub fn new_authenticated(
        config: ServiceHttpClientConfig,
        service_token_client: Arc<ServiceTokenClient>,
    ) -> Result<Self, DownstreamError> {
        let client = config.build()?;
        Ok(Self {
            config,
            client,
            service_token_client,
            token_target: ACCESS_SERVICE_TARGET,
        })
    }

    pub async fn authorize(
        &self,
        request: &AuthorizationDecisionRequest,
        headers: &reqwest::header::HeaderMap,
    ) -> Result<AuthorizationDecision, DownstreamError> {
        send_json(
            correlate(
                self.client
                    .post(self.config.endpoint("internal/v1/auth/decision")?)
                    .json(request),
                headers,
            ),
            &self.service_token_client,
            self.token_target,
        )
        .await
    }
}

/// Agent Service authority adapter.
#[derive(Clone)]
pub struct AgentClient {
    config: ServiceHttpClientConfig,
    client: reqwest::Client,
    service_token_client: Arc<ServiceTokenClient>,
    token_target: ServiceTokenTarget,
}

impl AgentClient {
    pub fn new_authenticated(
        config: ServiceHttpClientConfig,
        service_token_client: Arc<ServiceTokenClient>,
    ) -> Result<Self, DownstreamError> {
        let client = config.build()?;
        Ok(Self {
            config,
            client,
            service_token_client,
            token_target: AGENT_SERVICE_TARGET,
        })
    }

    pub async fn create(
        &self,
        request: &InternalCreateAgentRunRequest,
        key: &IdempotencyKey,
        headers: &reqwest::header::HeaderMap,
    ) -> Result<AgentRun, DownstreamError> {
        send_json(
            correlate(
                self.client
                    .post(self.config.endpoint("internal/v1/agent-runs")?)
                    .header("Idempotency-Key", key.as_str())
                    .json(request),
                headers,
            ),
            &self.service_token_client,
            self.token_target,
        )
        .await
    }

    pub async fn get(&self, run_id: AgentRunId) -> Result<AgentRun, DownstreamError> {
        send_json(
            self.client.get(
                self.config
                    .endpoint(&format!("internal/v1/agent-runs/{run_id}"))?,
            ),
            &self.service_token_client,
            self.token_target,
        )
        .await
    }

    pub async fn cancel(
        &self,
        run_id: AgentRunId,
        request: &InternalAgentRunMutationRequest,
        key: &IdempotencyKey,
        headers: &reqwest::header::HeaderMap,
    ) -> Result<AgentRun, DownstreamError> {
        send_json(
            correlate(
                self.client
                    .post(
                        self.config
                            .endpoint(&format!("internal/v1/agent-runs/{run_id}/cancel"))?,
                    )
                    .header("Idempotency-Key", key.as_str())
                    .json(request),
                headers,
            ),
            &self.service_token_client,
            self.token_target,
        )
        .await
    }

    pub async fn retry(
        &self,
        run_id: AgentRunId,
        track: AgentTrackKind,
        request: &InternalAgentRunMutationRequest,
        key: &IdempotencyKey,
        headers: &reqwest::header::HeaderMap,
    ) -> Result<AgentRun, DownstreamError> {
        let track = match track {
            AgentTrackKind::Environment => "environment",
            AgentTrackKind::Evaluation => "evaluation",
            AgentTrackKind::WorkConfiguration => "work_configuration",
        };
        send_json(
            correlate(
                self.client
                    .post(self.config.endpoint(&format!(
                        "internal/v1/agent-runs/{run_id}/tracks/{track}/retry"
                    ))?)
                    .header("Idempotency-Key", key.as_str())
                    .json(request),
                headers,
            ),
            &self.service_token_client,
            self.token_target,
        )
        .await
    }

    /// Binds one exact Control-issued Work configuration grant to an awaiting Agent run.
    pub async fn approve_work_configuration(
        &self,
        run_id: AgentRunId,
        request: &InternalApproveWorkConfigurationRequest,
        key: &IdempotencyKey,
        headers: &reqwest::header::HeaderMap,
    ) -> Result<AgentRun, DownstreamError> {
        let run: AgentRun = send_json(
            correlate(
                self.client
                    .post(self.config.endpoint(&format!(
                        "internal/v1/agent-runs/{run_id}/work-configuration/approve"
                    ))?)
                    .header("Idempotency-Key", key.as_str())
                    .json(request),
                headers,
            ),
            &self.service_token_client,
            self.token_target,
        )
        .await?;
        if run.id != run_id
            || run.project_id != request.project_id
            || run.course_id != request.course_id
        {
            return Err(DownstreamError::IdentityMismatch);
        }
        Ok(run)
    }

    /// Resolves one Agent-owned generated artifact metadata record for an exact package scope.
    pub async fn generated_artifact(
        &self,
        artifact_id: contracts::ArtifactId,
        query: &GeneratedArtifactQuery,
    ) -> Result<GeneratedArtifactRecord, DownstreamError> {
        let record: GeneratedArtifactRecord = send_json(
            self.client
                .get(
                    self.config
                        .endpoint(&format!("internal/v1/generated-artifacts/{artifact_id}"))?,
                )
                .query(query),
            &self.service_token_client,
            self.token_target,
        )
        .await?;
        if record.artifact.artifact_id != artifact_id
            || record.project_id != query.project_id
            || record.course_id != query.course_id
            || record.package_id != query.package_id
            || record.package_revision != query.package_revision
        {
            return Err(DownstreamError::IdentityMismatch);
        }
        Ok(record)
    }

    /// Reads the private VM execution intent metadata needed by Control to issue an exact
    /// recovery admission.  Scripts and credentials remain entirely Agent-owned.
    pub async fn work_execution_intent(
        &self,
        run_id: AgentRunId,
        query: &AgentWorkExecutionIntentQuery,
    ) -> Result<AgentWorkExecutionIntentMetadata, DownstreamError> {
        let metadata: AgentWorkExecutionIntentMetadata = send_json(
            self.client
                .get(self.config.endpoint(&format!(
                    "internal/v1/agent-runs/{run_id}/work-execution-intent"
                ))?)
                .query(query),
            &self.service_token_client,
            self.token_target,
        )
        .await?;
        metadata
            .validate()
            .map_err(|_| DownstreamError::IdentityMismatch)?;
        if metadata.run_id != run_id
            || metadata.project_id != query.project_id
            || metadata.course_id != query.course_id
            || metadata.execution_id != query.execution_id
        {
            return Err(DownstreamError::IdentityMismatch);
        }
        Ok(metadata)
    }

    /// Sends one fully fenced build cancellation over the existing Control mTLS identity.
    pub async fn cancel_build(
        &self,
        build_request_id: BuildRequestId,
        request: &InternalAgentBuildCancellationRequest,
        key: &IdempotencyKey,
    ) -> Result<InternalAgentBuildCancellationResult, DownstreamError> {
        if request.build_request_id != build_request_id {
            return Err(DownstreamError::IdentityMismatch);
        }
        send_json(
            self.client
                .post(self.config.endpoint(&format!(
                    "internal/v1/build-requests/{build_request_id}/cancel"
                ))?)
                .header("Idempotency-Key", key.as_str())
                .json(request),
            &self.service_token_client,
            self.token_target,
        )
        .await
    }

    /// Reads the authoritative Agent build state before a revision-fenced mutation.
    pub async fn get_build(
        &self,
        build_request_id: BuildRequestId,
        query: &InternalAgentBuildStatusQuery,
    ) -> Result<InternalAgentBuildCancellationResult, DownstreamError> {
        send_json(
            self.client
                .get(
                    self.config
                        .endpoint(&format!("internal/v1/build-requests/{build_request_id}"))?,
                )
                .query(query),
            &self.service_token_client,
            self.token_target,
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
            &self.service_token_client,
            self.token_target,
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
            &self.service_token_client,
            self.token_target,
        )
        .await?;
        resolution
            .validate()
            .map_err(|_| DownstreamError::IdentityMismatch)?;
        Ok(resolution)
    }
}

/// Evaluation authority adapter. All targets are fixed by deployment configuration.
#[derive(Clone)]
pub struct EvaluationClient {
    config: ServiceHttpClientConfig,
    client: reqwest::Client,
    service_token_client: Arc<ServiceTokenClient>,
    token_target: ServiceTokenTarget,
}

/// Environment authority adapter for the narrow Work configuration target lookup.
#[derive(Clone)]
pub struct EnvironmentClient {
    config: ServiceHttpClientConfig,
    client: reqwest::Client,
    service_token_client: Arc<ServiceTokenClient>,
    token_target: ServiceTokenTarget,
}

impl EnvironmentClient {
    pub fn new_authenticated(
        config: ServiceHttpClientConfig,
        service_token_client: Arc<ServiceTokenClient>,
    ) -> Result<Self, DownstreamError> {
        let client = config.build()?;
        Ok(Self {
            config,
            client,
            service_token_client,
            token_target: ENVIRONMENT_SERVICE_TARGET,
        })
    }

    /// Resolves the runtime only from Environment's authoritative Work aggregate and lease.
    pub async fn work_configuration_target(
        &self,
        environment_id: contracts::EnvironmentId,
        query: &EnvironmentWorkConfigurationTargetQuery,
        headers: &reqwest::header::HeaderMap,
    ) -> Result<EnvironmentWorkConfigurationTarget, DownstreamError> {
        let target: EnvironmentWorkConfigurationTarget = send_json(
            correlate(
                self.client
                    .get(self.config.endpoint(&format!(
                        "internal/v1/environments/{environment_id}/work-configuration-target"
                    ))?)
                    .query(query),
                headers,
            ),
            &self.service_token_client,
            self.token_target,
        )
        .await?;
        target
            .validate_for(environment_id, query)
            .map_err(|_| DownstreamError::IdentityMismatch)?;
        Ok(target)
    }
}

impl fmt::Debug for AccessClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AccessClient")
            .field("config", &self.config)
            .field("service_token_configured", &true)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for AgentClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentClient")
            .field("config", &self.config)
            .field("service_token_configured", &true)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for EnvironmentClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EnvironmentClient")
            .field("config", &self.config)
            .field("service_token_configured", &true)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for EvaluationClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EvaluationClient")
            .field("config", &self.config)
            .field("service_token_configured", &true)
            .finish_non_exhaustive()
    }
}

impl EvaluationClient {
    pub fn new_authenticated(
        config: ServiceHttpClientConfig,
        service_token_client: Arc<ServiceTokenClient>,
    ) -> Result<Self, DownstreamError> {
        let client = config.build()?;
        Ok(Self {
            config,
            client,
            service_token_client,
            token_target: EVALUATION_SERVICE_TARGET,
        })
    }

    pub async fn publish(
        &self,
        request: &InternalPublishEvaluationReleaseRequest,
        key: &IdempotencyKey,
        headers: &reqwest::header::HeaderMap,
    ) -> Result<EvaluationRelease, DownstreamError> {
        let release: EvaluationRelease = send_json(
            correlate(
                self.client
                    .post(self.config.endpoint("internal/v1/evaluation-releases")?)
                    .header("Idempotency-Key", key.as_str())
                    .json(request),
                headers,
            ),
            &self.service_token_client,
            self.token_target,
        )
        .await?;
        validate_release(release)
    }

    pub async fn list(
        &self,
        course_id: contracts::CourseId,
        query: &EvaluationReleaseListQuery,
        headers: &reqwest::header::HeaderMap,
    ) -> Result<CursorPage<EvaluationRelease>, DownstreamError> {
        let mut page: CursorPage<EvaluationRelease> = send_json(
            correlate(
                self.client
                    .get(self.config.endpoint("internal/v1/evaluation-releases")?)
                    .header("x-labweaver-course-id", course_id.to_string())
                    .query(query),
                headers,
            ),
            &self.service_token_client,
            self.token_target,
        )
        .await?;
        if page
            .items
            .iter()
            .any(|release| release.course_id != Some(course_id))
        {
            return Err(DownstreamError::IdentityMismatch);
        }
        page.items = page
            .items
            .into_iter()
            .map(validate_release)
            .collect::<Result<_, _>>()?;
        Ok(page)
    }

    pub async fn get(
        &self,
        release_id: EvaluationReleaseId,
        headers: &reqwest::header::HeaderMap,
    ) -> Result<EvaluationRelease, DownstreamError> {
        let release: EvaluationRelease = send_json(
            correlate(
                self.client.get(
                    self.config
                        .endpoint(&format!("internal/v1/evaluation-releases/{release_id}"))?,
                ),
                headers,
            ),
            &self.service_token_client,
            self.token_target,
        )
        .await?;
        if release.id != release_id {
            return Err(DownstreamError::IdentityMismatch);
        }
        validate_release(release)
    }

    pub async fn withdraw(
        &self,
        release_id: EvaluationReleaseId,
        request: &InternalWithdrawEvaluationReleaseRequest,
        key: &IdempotencyKey,
        headers: &reqwest::header::HeaderMap,
    ) -> Result<EvaluationRelease, DownstreamError> {
        let release: EvaluationRelease = send_json(
            correlate(
                self.client
                    .post(self.config.endpoint(&format!(
                        "internal/v1/evaluation-releases/{release_id}/withdraw"
                    ))?)
                    .header("Idempotency-Key", key.as_str())
                    .header(
                        "If-Match",
                        contracts::http::StrongEtag::from_revision(request.expected_revision)
                            .header_value(),
                    )
                    .json(request),
                headers,
            ),
            &self.service_token_client,
            self.token_target,
        )
        .await?;
        if release.id != release_id || release.course_id != request.course_id {
            return Err(DownstreamError::IdentityMismatch);
        }
        validate_release(release)
    }
}

fn correlate(
    mut request: reqwest::RequestBuilder,
    headers: &reqwest::header::HeaderMap,
) -> reqwest::RequestBuilder {
    for name in [telemetry::REQUEST_ID_HEADER, telemetry::TRACEPARENT_HEADER] {
        if let Some(value) = headers.get(name) {
            request = request.header(name, value);
        }
    }
    request
}

fn validate_release(release: EvaluationRelease) -> Result<EvaluationRelease, DownstreamError> {
    release
        .validate()
        .map_err(|_| DownstreamError::ProtocolInvalid)?;
    Ok(release)
}

async fn send_json<T: serde::de::DeserializeOwned>(
    request: reqwest::RequestBuilder,
    service_token_client: &ServiceTokenClient,
    target: ServiceTokenTarget,
) -> Result<T, DownstreamError> {
    let mut headers = reqwest::header::HeaderMap::new();
    let scopes = target.scopes();
    service_token_client
        .bearer_auth_for(&mut headers, target.audience, &scopes)
        .await
        .map_err(|_| DownstreamError::Unavailable)?;
    let request = request.headers(headers);
    let started = Instant::now();
    let response = request.send().await.map_err(|error| {
        tracing::warn!(
            event = "control.downstream.request_failed",
            component = "downstream-client",
            operation = "http.request",
            outcome = "failed",
            duration_ms = elapsed_millis(started),
            binding = target.audience,
            error_kind = reqwest_error_kind(&error),
            failure_stage = "control.downstream.request",
            retryable = error.is_timeout() || error.is_connect(),
            safe_detail = "redacted_unclassified",
        );
        DownstreamError::Unavailable
    })?;
    let status = response.status();
    if !status.is_success() {
        tracing::warn!(
            event = "control.downstream.response_rejected",
            component = "downstream-client",
            operation = "http.response",
            outcome = "rejected",
            duration_ms = elapsed_millis(started),
            binding = target.audience,
            http_status = status.as_u16(),
            error_kind = "upstream_http",
            failure_stage = "control.downstream.response",
            retryable = status.is_server_error(),
            safe_detail = "redacted_unclassified",
        );
    }
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
    response.json().await.map_err(|error| {
        tracing::warn!(
            event = "control.downstream.response_invalid",
            component = "downstream-client",
            operation = "http.response.decode",
            outcome = "failed",
            duration_ms = elapsed_millis(started),
            binding = target.audience,
            http_status = status.as_u16(),
            error_kind = reqwest_error_kind(&error),
            failure_stage = "control.downstream.response.decode",
            retryable = false,
            safe_detail = "redacted_unclassified",
        );
        DownstreamError::ProtocolInvalid
    })
}

fn elapsed_millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn reqwest_error_kind(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "connect"
    } else if error.is_builder() {
        "builder"
    } else if error.is_request() {
        "request"
    } else if error.is_body() {
        "body"
    } else if error.is_decode() {
        "decode"
    } else {
        "transport"
    }
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
