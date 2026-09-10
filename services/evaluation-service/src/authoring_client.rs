//! Strict, bounded client for the Control authoring-publication admission boundary.
//!
//! Evaluation resolves the authoring publication immediately before it creates a run.  The
//! response is only an admission snapshot; the Evaluation repository still checks the exact
//! release identity while holding its transaction locks.  This client therefore has no cache and
//! never follows a redirect or accepts an unbounded response body.
#![allow(missing_docs, clippy::missing_errors_doc)]

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use auth::{ServiceTokenClient, ServiceTokenClientError};
use contracts::authoring::ProjectLlmEgressPolicy;
use contracts::http::{AuthoringPublicationAdmissionBinding, AuthoringPublicationAdmissionQuery};
use contracts::{ApprovalId, ProjectId};
use futures_util::StreamExt;
use reqwest::{Certificate, Client, StatusCode, Url, header::HeaderMap};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;

const REQUIRED_SCOPE: &str = "control.authoring.read";
const POLICY_REQUIRED_SCOPE: &str = "control.llm_policy.read";
const MAX_CA_BYTES: u64 = 1024 * 1024;
const MIN_TIMEOUT_MILLISECONDS: u64 = 100;
const MAX_TIMEOUT_MILLISECONDS: u64 = 60_000;
const MAX_REQUEST_BYTES: u64 = 1024 * 1024;
const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;

/// Returns the service permission required for an authoring admission lookup.
#[must_use]
pub const fn required_scope_for_diagnostics() -> &'static str {
    REQUIRED_SCOPE
}

/// Returns the Control permission required to resolve the current project LLM policy.
#[must_use]
pub const fn required_policy_scope_for_diagnostics() -> &'static str {
    POLICY_REQUIRED_SCOPE
}

/// Evaluation's Control authoring-admission HTTP configuration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthoringAdmissionClientConfiguration {
    pub base_uri: Url,
    pub ca_file: PathBuf,
    pub timeout_milliseconds: u64,
    pub max_request_bytes: u64,
    pub max_response_bytes: u64,
    pub audience: String,
}

impl AuthoringAdmissionClientConfiguration {
    fn validate(&self) -> Result<(), AuthoringAdmissionClientError> {
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
            return Err(AuthoringAdmissionClientError::Configuration);
        }
        Ok(())
    }
}

/// Authenticated, HTTPS-only Control admission client owned by Evaluation.
#[derive(Clone)]
pub struct AuthoringAdmissionClient {
    base_uri: Url,
    client: Client,
    token_client: Arc<ServiceTokenClient>,
    audience: String,
    scopes: BTreeSet<String>,
    max_response_bytes: usize,
}

impl std::fmt::Debug for AuthoringAdmissionClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthoringAdmissionClient")
            .field("base_uri", &self.base_uri)
            .field("audience", &self.audience)
            .field("scopes", &self.scopes)
            .field("max_response_bytes", &self.max_response_bytes)
            .finish_non_exhaustive()
    }
}

impl AuthoringAdmissionClient {
    /// Constructs a client with a caller-supplied HTTP client.
    ///
    /// The caller must provide the same strict HTTPS policy as [`from_configuration`].  Keeping
    /// this explicit makes transport policy visible to tests and prevents an ambient proxy or
    /// system trust store from being selected by this domain client.
    pub fn new(
        configuration: AuthoringAdmissionClientConfiguration,
        client: Client,
        token_client: Arc<ServiceTokenClient>,
        scopes: BTreeSet<String>,
    ) -> Result<Self, AuthoringAdmissionClientError> {
        configuration.validate()?;
        validate_scopes(&scopes)?;
        let max_response_bytes = usize::try_from(configuration.max_response_bytes)
            .map_err(|_| AuthoringAdmissionClientError::Configuration)?;
        Ok(Self {
            base_uri: configuration.base_uri,
            client,
            token_client,
            audience: configuration.audience,
            scopes,
            max_response_bytes,
        })
    }

    /// Constructs a strict HTTPS client using the configured CA and timeout.
    pub fn from_configuration(
        configuration: AuthoringAdmissionClientConfiguration,
        token_client: Arc<ServiceTokenClient>,
        scopes: BTreeSet<String>,
    ) -> Result<Self, AuthoringAdmissionClientError> {
        configuration.validate()?;
        let ca = read_bounded_file(&configuration.ca_file, MAX_CA_BYTES)?;
        let certificate =
            Certificate::from_pem(&ca).map_err(|_| AuthoringAdmissionClientError::Configuration)?;
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .https_only(true)
            .tls_built_in_root_certs(false)
            .add_root_certificate(certificate)
            .timeout(Duration::from_millis(configuration.timeout_milliseconds))
            .build()
            .map_err(|_| AuthoringAdmissionClientError::Configuration)?;
        Self::new(configuration, client, token_client, scopes)
    }

    /// Resolves one exact publication admission snapshot from Control.
    ///
    /// `courseId` is deliberately omitted when the query has no course association.  The
    /// response is checked against every identity supplied by the request before it is returned.
    pub async fn resolve(
        &self,
        approval_id: ApprovalId,
        query: &AuthoringPublicationAdmissionQuery,
    ) -> Result<AuthoringPublicationAdmissionBinding, AuthoringAdmissionClientError> {
        validate_query(query)?;
        let path = format!("internal/v1/authoring-publications/{approval_id}/admission");
        let url = self
            .base_uri
            .join(&path)
            .map_err(|_| AuthoringAdmissionClientError::Configuration)?;
        if url.scheme() != "https" {
            return Err(AuthoringAdmissionClientError::Configuration);
        }

        let mut headers = HeaderMap::new();
        self.token_client
            .bearer_auth_for(&mut headers, &self.audience, &self.scopes)
            .await
            .map_err(AuthoringAdmissionClientError::Token)?;

        // `reqwest::RequestBuilder::query` uses serde_urlencoded, which omits an `Option::None`
        // field.  Building the pairs explicitly keeps that boundary visible and prevents a
        // future serializer change from turning an absent course association into `courseId=`.
        let mut query_pairs = vec![
            ("projectId", query.project_id.to_string()),
            (
                "approvalRevision",
                query.approval_revision.get().to_string(),
            ),
            (
                "evaluationReleaseId",
                query.evaluation_release_id.to_string(),
            ),
        ];
        if let Some(course_id) = query.course_id {
            query_pairs.push(("courseId", course_id.to_string()));
        }
        let response = self
            .client
            .get(url)
            .headers(headers)
            .query(&query_pairs)
            .send()
            .await
            .map_err(|_| AuthoringAdmissionClientError::Transport)?;
        let status = response.status();
        if status == StatusCode::NOT_FOUND {
            return Err(AuthoringAdmissionClientError::AdmissionMissing);
        }
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(AuthoringAdmissionClientError::Denied);
        }
        if status == StatusCode::CONFLICT || status == StatusCode::PRECONDITION_FAILED {
            return Err(AuthoringAdmissionClientError::Conflict);
        }
        if status == StatusCode::PAYLOAD_TOO_LARGE {
            return Err(AuthoringAdmissionClientError::ResponseTooLarge);
        }
        if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
            return Err(AuthoringAdmissionClientError::Unavailable);
        }
        if !status.is_success() {
            return Err(AuthoringAdmissionClientError::Rejected);
        }
        let body = read_bounded_body(response, self.max_response_bytes).await?;
        let binding: AuthoringPublicationAdmissionBinding = decode_response(&body)?;
        validate_binding(approval_id, query, &binding)?;
        Ok(binding)
    }

    /// Resolves the current project LLM egress policy immediately before an advisory review.
    ///
    /// The policy is intentionally uncached.  Agent receives this exact snapshot and enforces
    /// its ownership and revision before any provider egress.
    pub async fn active_policy(
        &self,
        project_id: ProjectId,
    ) -> Result<ProjectLlmEgressPolicy, AuthoringAdmissionClientError> {
        if project_id.as_uuid().is_nil() {
            return Err(AuthoringAdmissionClientError::RequestInvalid);
        }
        let path = format!("internal/v1/projects/{project_id}/llm-egress-policy");
        let url = self
            .base_uri
            .join(&path)
            .map_err(|_| AuthoringAdmissionClientError::Configuration)?;
        let mut headers = HeaderMap::new();
        self.token_client
            .bearer_auth_for(&mut headers, &self.audience, &self.scopes)
            .await
            .map_err(AuthoringAdmissionClientError::Token)?;
        let response = self
            .client
            .get(url)
            .headers(headers)
            .send()
            .await
            .map_err(|_| AuthoringAdmissionClientError::Transport)?;
        let status = response.status();
        if status == StatusCode::NOT_FOUND {
            return Err(AuthoringAdmissionClientError::AdmissionMissing);
        }
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(AuthoringAdmissionClientError::Denied);
        }
        if status == StatusCode::PAYLOAD_TOO_LARGE {
            return Err(AuthoringAdmissionClientError::ResponseTooLarge);
        }
        if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
            return Err(AuthoringAdmissionClientError::Unavailable);
        }
        if !status.is_success() {
            return Err(AuthoringAdmissionClientError::Rejected);
        }
        let body = read_bounded_body(response, self.max_response_bytes).await?;
        let policy: ProjectLlmEgressPolicy = decode_response(&body)?;
        if policy.project_id != project_id {
            return Err(AuthoringAdmissionClientError::ResponseInvalid);
        }
        policy
            .validate()
            .map_err(|_| AuthoringAdmissionClientError::ResponseInvalid)?;
        Ok(policy)
    }

    #[must_use]
    pub fn audience(&self) -> &str {
        &self.audience
    }
}

fn validate_scopes(scopes: &BTreeSet<String>) -> Result<(), AuthoringAdmissionClientError> {
    if !scopes.contains(REQUIRED_SCOPE)
        || !scopes.contains(POLICY_REQUIRED_SCOPE)
        || scopes.iter().any(|scope| {
            scope.is_empty()
                || scope.len() > 128
                || scope.chars().any(char::is_control)
                || scope.contains(char::is_whitespace)
        })
    {
        return Err(AuthoringAdmissionClientError::Configuration);
    }
    Ok(())
}

fn validate_query(
    query: &AuthoringPublicationAdmissionQuery,
) -> Result<(), AuthoringAdmissionClientError> {
    if query.approval_revision.get() == 0 {
        return Err(AuthoringAdmissionClientError::RequestInvalid);
    }
    Ok(())
}

fn validate_binding(
    approval_id: ApprovalId,
    query: &AuthoringPublicationAdmissionQuery,
    binding: &AuthoringPublicationAdmissionBinding,
) -> Result<(), AuthoringAdmissionClientError> {
    if binding.approval_id != approval_id
        || binding.approval_revision != query.approval_revision
        || binding.project_id != query.project_id
        || binding.course_id != query.course_id
        || binding.evaluation_release_id != query.evaluation_release_id
        || binding.environment_release_version == 0
        || binding.evaluation_release_revision.get() == 0
    {
        return Err(AuthoringAdmissionClientError::ResponseInvalid);
    }
    Ok(())
}

fn decode_response<T: DeserializeOwned>(body: &[u8]) -> Result<T, AuthoringAdmissionClientError> {
    if body.is_empty() {
        return Err(AuthoringAdmissionClientError::ResponseInvalid);
    }
    serde_json::from_slice(body).map_err(|_| AuthoringAdmissionClientError::ResponseInvalid)
}

async fn read_bounded_body(
    response: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>, AuthoringAdmissionClientError> {
    if response
        .content_length()
        .is_some_and(|length| usize::try_from(length).map_or(true, |length| length > max_bytes))
    {
        return Err(AuthoringAdmissionClientError::ResponseTooLarge);
    }
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| AuthoringAdmissionClientError::Transport)?;
        let next = body
            .len()
            .checked_add(chunk.len())
            .ok_or(AuthoringAdmissionClientError::ResponseTooLarge)?;
        if next > max_bytes {
            return Err(AuthoringAdmissionClientError::ResponseTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn read_bounded_file(
    path: &Path,
    max_bytes: u64,
) -> Result<Vec<u8>, AuthoringAdmissionClientError> {
    if !path.is_absolute() {
        return Err(AuthoringAdmissionClientError::Configuration);
    }
    let parent = path
        .parent()
        .ok_or(AuthoringAdmissionClientError::Configuration)?;
    let canonical_parent =
        fs::canonicalize(parent).map_err(|_| AuthoringAdmissionClientError::Configuration)?;
    let canonical =
        fs::canonicalize(path).map_err(|_| AuthoringAdmissionClientError::Configuration)?;
    let metadata =
        fs::metadata(&canonical).map_err(|_| AuthoringAdmissionClientError::Configuration)?;
    if !canonical.starts_with(canonical_parent)
        || !metadata.is_file()
        || metadata.len() == 0
        || metadata.len() > max_bytes
    {
        return Err(AuthoringAdmissionClientError::Configuration);
    }
    fs::read(canonical).map_err(|_| AuthoringAdmissionClientError::Configuration)
}

/// Stable failures raised at the Evaluation-to-Control admission boundary.
#[derive(Debug, Error)]
pub enum AuthoringAdmissionClientError {
    #[error("LW_EVALUATION_AUTHORING_ADMISSION_CONFIG_INVALID")]
    Configuration,
    #[error("LW_EVALUATION_AUTHORING_ADMISSION_TOKEN_FAILED")]
    Token(#[source] ServiceTokenClientError),
    #[error("LW_EVALUATION_AUTHORING_ADMISSION_TRANSPORT_FAILED")]
    Transport,
    #[error("LW_EVALUATION_AUTHORING_ADMISSION_REQUEST_INVALID")]
    RequestInvalid,
    #[error("LW_EVALUATION_AUTHORING_ADMISSION_RESPONSE_TOO_LARGE")]
    ResponseTooLarge,
    #[error("LW_EVALUATION_AUTHORING_ADMISSION_RESPONSE_INVALID")]
    ResponseInvalid,
    #[error("LW_EVALUATION_AUTHORING_ADMISSION_MISSING")]
    AdmissionMissing,
    #[error("LW_EVALUATION_AUTHORING_ADMISSION_DENIED")]
    Denied,
    #[error("LW_EVALUATION_AUTHORING_ADMISSION_CONFLICT")]
    Conflict,
    #[error("LW_EVALUATION_AUTHORING_ADMISSION_REJECTED")]
    Rejected,
    #[error("LW_EVALUATION_AUTHORING_ADMISSION_UNAVAILABLE")]
    Unavailable,
}
