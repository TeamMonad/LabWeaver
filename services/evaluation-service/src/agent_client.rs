//! Strict client for Agent-owned advisory LLM reviews.
//!
//! Evaluation persists the immutable review request before calling this client.  A retry uses
//! the same task identity and idempotency key, so a transport timeout is reconciled by reading
//! the Agent receipt instead of creating a second provider job.

#![allow(missing_docs, clippy::missing_errors_doc)]

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use auth::{ServiceTokenClient, ServiceTokenClientError};
use contracts::{
    CourseId, ProjectId, TaskRunId,
    http::{
        AgentLlmReviewState, IdempotencyKey, InternalAgentLlmReviewReceipt,
        InternalAgentLlmReviewRequest,
    },
};
use futures_util::StreamExt;
use reqwest::{
    Certificate, Client, Method, StatusCode, Url,
    header::{CONTENT_TYPE, HeaderMap, HeaderValue},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;

const REQUIRED_SCOPES: [&str; 3] = [
    "agent.llm_review.create",
    "agent.llm_review.read",
    "agent.llm_review.cancel",
];
const MAX_CA_BYTES: u64 = 1024 * 1024;
const MIN_TIMEOUT_MILLISECONDS: u64 = 100;
const MAX_TIMEOUT_MILLISECONDS: u64 = 60_000;
const MAX_REQUEST_BYTES: u64 = 4 * 1024 * 1024;
const MAX_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;

/// Returns the service scopes required for the advisory review lifecycle.
#[must_use]
pub const fn required_scopes_for_diagnostics() -> &'static [&'static str] {
    &REQUIRED_SCOPES
}

/// Evaluation's downstream Agent HTTP configuration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentClientConfiguration {
    pub base_uri: Url,
    pub ca_file: PathBuf,
    pub timeout_milliseconds: u64,
    pub max_request_bytes: u64,
    pub max_response_bytes: u64,
    pub audience: String,
}

impl AgentClientConfiguration {
    fn validate(&self) -> Result<(), AgentClientError> {
        if self.base_uri.scheme() != "https"
            || self.base_uri.host_str().is_none()
            || !self.base_uri.username().is_empty()
            || self.base_uri.password().is_some()
            || self.base_uri.query().is_some()
            || self.base_uri.fragment().is_some()
            || !self.base_uri.path().ends_with('/')
            || !self.ca_file.is_absolute()
            || !(MIN_TIMEOUT_MILLISECONDS..=MAX_TIMEOUT_MILLISECONDS)
                .contains(&self.timeout_milliseconds)
            || self.max_request_bytes == 0
            || self.max_request_bytes > MAX_REQUEST_BYTES
            || self.max_response_bytes == 0
            || self.max_response_bytes > MAX_RESPONSE_BYTES
            || self.audience.trim().is_empty()
            || self.audience.chars().any(char::is_control)
        {
            return Err(AgentClientError::Configuration);
        }
        Ok(())
    }
}

/// Authenticated, HTTPS-only client for Agent's advisory review queue.
#[derive(Clone)]
pub struct AgentClient {
    base_uri: Url,
    client: Client,
    token_client: Arc<ServiceTokenClient>,
    audience: String,
    scopes: BTreeSet<String>,
    max_request_bytes: usize,
    max_response_bytes: usize,
}

impl std::fmt::Debug for AgentClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentClient")
            .field("base_uri", &self.base_uri)
            .field("audience", &self.audience)
            .field("scopes", &self.scopes)
            .field("max_request_bytes", &self.max_request_bytes)
            .field("max_response_bytes", &self.max_response_bytes)
            .finish_non_exhaustive()
    }
}

impl AgentClient {
    /// Constructs a client with a caller-supplied strict HTTP client.
    pub fn new(
        configuration: AgentClientConfiguration,
        client: Client,
        token_client: Arc<ServiceTokenClient>,
        scopes: BTreeSet<String>,
    ) -> Result<Self, AgentClientError> {
        configuration.validate()?;
        validate_scopes(&scopes)?;
        let max_request_bytes = usize::try_from(configuration.max_request_bytes)
            .map_err(|_| AgentClientError::Configuration)?;
        let max_response_bytes = usize::try_from(configuration.max_response_bytes)
            .map_err(|_| AgentClientError::Configuration)?;
        Ok(Self {
            base_uri: configuration.base_uri,
            client,
            token_client,
            audience: configuration.audience,
            scopes,
            max_request_bytes,
            max_response_bytes,
        })
    }

    /// Constructs a strict HTTPS client from mounted deployment configuration.
    pub fn from_configuration(
        configuration: AgentClientConfiguration,
        token_client: Arc<ServiceTokenClient>,
        scopes: BTreeSet<String>,
    ) -> Result<Self, AgentClientError> {
        configuration.validate()?;
        let ca = read_bounded_file(&configuration.ca_file, MAX_CA_BYTES)?;
        let certificate =
            Certificate::from_pem(&ca).map_err(|_| AgentClientError::Configuration)?;
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .https_only(true)
            .tls_built_in_root_certs(false)
            .add_root_certificate(certificate)
            .timeout(Duration::from_millis(configuration.timeout_milliseconds))
            .build()
            .map_err(|_| AgentClientError::Configuration)?;
        Self::new(configuration, client, token_client, scopes)
    }

    /// Returns the configured Agent audience.
    #[must_use]
    pub fn audience(&self) -> &str {
        &self.audience
    }

    /// Enqueues one immutable advisory review request.
    pub async fn enqueue(
        &self,
        request: &InternalAgentLlmReviewRequest,
        idempotency_key: &str,
    ) -> Result<InternalAgentLlmReviewReceipt, AgentClientError> {
        request
            .validate()
            .map_err(|_| AgentClientError::RequestInvalid)?;
        validate_idempotency_key(idempotency_key)?;
        let body = self.encode_request(request)?;
        let response = self
            .execute(
                Method::POST,
                "internal/v1/llm-reviews",
                Some(body),
                Some(idempotency_key),
                NotFound::Review,
            )
            .await?;
        let receipt: InternalAgentLlmReviewReceipt = decode_response(&response)?;
        validate_receipt(&receipt, request.task_run_id)?;
        if !matches!(
            receipt.state,
            AgentLlmReviewState::Queued
                | AgentLlmReviewState::Running
                | AgentLlmReviewState::Cancelling
                | AgentLlmReviewState::Succeeded
                | AgentLlmReviewState::Failed
                | AgentLlmReviewState::Cancelled
        ) {
            return Err(AgentClientError::ResponseInvalid);
        }
        Ok(receipt)
    }

    /// Reads the current Agent receipt for one exact project/course scope.
    pub async fn get(
        &self,
        task_run_id: TaskRunId,
        project_id: ProjectId,
        course_id: Option<CourseId>,
    ) -> Result<InternalAgentLlmReviewReceipt, AgentClientError> {
        if task_run_id.as_uuid().is_nil() || project_id.as_uuid().is_nil() {
            return Err(AgentClientError::RequestInvalid);
        }
        let path = format!("internal/v1/llm-reviews/{task_run_id}");
        let mut query = vec![("projectId", project_id.to_string())];
        if let Some(course_id) = course_id {
            query.push(("courseId", course_id.to_string()));
        }
        let response = self
            .execute_with_query(Method::GET, &path, &query, None, None, NotFound::Review)
            .await?;
        let receipt: InternalAgentLlmReviewReceipt = decode_response(&response)?;
        validate_receipt(&receipt, task_run_id)?;
        Ok(receipt)
    }

    /// Requests cancellation of one queued or running review.
    pub async fn cancel(
        &self,
        task_run_id: TaskRunId,
        project_id: ProjectId,
        course_id: Option<CourseId>,
        idempotency_key: &str,
    ) -> Result<InternalAgentLlmReviewReceipt, AgentClientError> {
        if task_run_id.as_uuid().is_nil() || project_id.as_uuid().is_nil() {
            return Err(AgentClientError::RequestInvalid);
        }
        validate_idempotency_key(idempotency_key)?;
        let mut body_query = vec![("projectId", project_id.to_string())];
        if let Some(course_id) = course_id {
            body_query.push(("courseId", course_id.to_string()));
        }
        let path = format!("internal/v1/llm-reviews/{task_run_id}/cancel");
        let response = self
            .execute_with_query(
                Method::POST,
                &path,
                &body_query,
                None,
                Some(idempotency_key),
                NotFound::Review,
            )
            .await?;
        let receipt: InternalAgentLlmReviewReceipt = decode_response(&response)?;
        validate_receipt(&receipt, task_run_id)?;
        Ok(receipt)
    }

    fn encode_request<T: Serialize>(&self, request: &T) -> Result<Vec<u8>, AgentClientError> {
        let body = serde_json::to_vec(request).map_err(|_| AgentClientError::RequestInvalid)?;
        if body.len() > self.max_request_bytes {
            return Err(AgentClientError::RequestTooLarge);
        }
        Ok(body)
    }

    async fn execute(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
        idempotency_key: Option<&str>,
        not_found: NotFound,
    ) -> Result<Vec<u8>, AgentClientError> {
        self.execute_with_query(method, path, &[], body, idempotency_key, not_found)
            .await
    }

    async fn execute_with_query(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<Vec<u8>>,
        idempotency_key: Option<&str>,
        not_found: NotFound,
    ) -> Result<Vec<u8>, AgentClientError> {
        let url = self
            .base_uri
            .join(path)
            .map_err(|_| AgentClientError::Configuration)?;
        if url.scheme() != "https" {
            return Err(AgentClientError::Configuration);
        }
        let mut headers = HeaderMap::new();
        self.token_client
            .bearer_auth_for(&mut headers, &self.audience, &self.scopes)
            .await
            .map_err(AgentClientError::Token)?;
        if body.is_some() {
            headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        }
        if let Some(key) = idempotency_key {
            let value = HeaderValue::from_str(key).map_err(|_| AgentClientError::RequestInvalid)?;
            headers.insert("idempotency-key", value);
        }
        let request = self.client.request(method, url).headers(headers);
        let request = if let Some(body) = body {
            request.body(body)
        } else {
            request
        };
        let response = request
            .query(query)
            .send()
            .await
            .map_err(|_| AgentClientError::Transport)?;
        let status = response.status();
        if status == StatusCode::NOT_FOUND {
            return Err(match not_found {
                NotFound::Review => AgentClientError::ReviewMissing,
            });
        }
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(AgentClientError::Denied);
        }
        if status == StatusCode::CONFLICT || status == StatusCode::PRECONDITION_FAILED {
            return Err(AgentClientError::Conflict);
        }
        if status == StatusCode::PAYLOAD_TOO_LARGE {
            return Err(AgentClientError::ResponseTooLarge);
        }
        if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
            return Err(AgentClientError::Unavailable);
        }
        if !status.is_success() {
            return Err(AgentClientError::Rejected);
        }
        read_bounded_body(response, self.max_response_bytes).await
    }
}

fn validate_scopes(scopes: &BTreeSet<String>) -> Result<(), AgentClientError> {
    if REQUIRED_SCOPES
        .iter()
        .any(|required| !scopes.contains(*required))
        || scopes.iter().any(|scope| {
            scope.is_empty()
                || scope.len() > 128
                || scope.chars().any(char::is_control)
                || scope.contains(char::is_whitespace)
        })
    {
        return Err(AgentClientError::Configuration);
    }
    Ok(())
}

fn validate_idempotency_key(value: &str) -> Result<(), AgentClientError> {
    IdempotencyKey::parse(value)
        .map(|_| ())
        .map_err(|_| AgentClientError::RequestInvalid)
}

fn validate_receipt(
    receipt: &InternalAgentLlmReviewReceipt,
    task_run_id: TaskRunId,
) -> Result<(), AgentClientError> {
    receipt
        .validate()
        .map_err(|_| AgentClientError::ResponseInvalid)?;
    if receipt.task_run_id != task_run_id {
        return Err(AgentClientError::IdentityMismatch);
    }
    Ok(())
}

fn decode_response<T: DeserializeOwned>(body: &[u8]) -> Result<T, AgentClientError> {
    if body.is_empty() {
        return Err(AgentClientError::ResponseInvalid);
    }
    serde_json::from_slice(body).map_err(|_| AgentClientError::ResponseInvalid)
}

async fn read_bounded_body(
    response: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>, AgentClientError> {
    if response
        .content_length()
        .is_some_and(|length| usize::try_from(length).map_or(true, |value| value > max_bytes))
    {
        return Err(AgentClientError::ResponseTooLarge);
    }
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| AgentClientError::Transport)?;
        let next = body
            .len()
            .checked_add(chunk.len())
            .ok_or(AgentClientError::ResponseTooLarge)?;
        if next > max_bytes {
            return Err(AgentClientError::ResponseTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn read_bounded_file(path: &Path, max_bytes: u64) -> Result<Vec<u8>, AgentClientError> {
    if !path.is_absolute() {
        return Err(AgentClientError::Configuration);
    }
    let parent = path.parent().ok_or(AgentClientError::Configuration)?;
    let canonical_parent = fs::canonicalize(parent).map_err(|_| AgentClientError::Configuration)?;
    let canonical = fs::canonicalize(path).map_err(|_| AgentClientError::Configuration)?;
    let metadata = fs::metadata(&canonical).map_err(|_| AgentClientError::Configuration)?;
    if !canonical.starts_with(canonical_parent)
        || !metadata.is_file()
        || metadata.len() == 0
        || metadata.len() > max_bytes
    {
        return Err(AgentClientError::Configuration);
    }
    fs::read(canonical).map_err(|_| AgentClientError::Configuration)
}

#[derive(Clone, Copy)]
enum NotFound {
    Review,
}

/// Failures raised at the Evaluation-to-Agent advisory review boundary.
#[derive(Debug, Error)]
pub enum AgentClientError {
    #[error("LW_EVALUATION_AGENT_CLIENT_CONFIG_INVALID")]
    Configuration,
    #[error("LW_EVALUATION_AGENT_CLIENT_TOKEN_FAILED")]
    Token(#[source] ServiceTokenClientError),
    #[error("LW_EVALUATION_AGENT_CLIENT_TRANSPORT_FAILED")]
    Transport,
    #[error("LW_EVALUATION_AGENT_CLIENT_REQUEST_INVALID")]
    RequestInvalid,
    #[error("LW_EVALUATION_AGENT_CLIENT_REQUEST_TOO_LARGE")]
    RequestTooLarge,
    #[error("LW_EVALUATION_AGENT_CLIENT_RESPONSE_TOO_LARGE")]
    ResponseTooLarge,
    #[error("LW_EVALUATION_AGENT_CLIENT_RESPONSE_INVALID")]
    ResponseInvalid,
    #[error("LW_EVALUATION_AGENT_CLIENT_REVIEW_MISSING")]
    ReviewMissing,
    #[error("LW_EVALUATION_AGENT_CLIENT_DENIED")]
    Denied,
    #[error("LW_EVALUATION_AGENT_CLIENT_CONFLICT")]
    Conflict,
    #[error("LW_EVALUATION_AGENT_CLIENT_REJECTED")]
    Rejected,
    #[error("LW_EVALUATION_AGENT_CLIENT_UNAVAILABLE")]
    Unavailable,
    #[error("LW_EVALUATION_AGENT_CLIENT_IDENTITY_MISMATCH")]
    IdentityMismatch,
}
