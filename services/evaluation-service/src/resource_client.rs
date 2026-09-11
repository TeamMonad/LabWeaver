//! Strict, bounded client for Evaluation's one-shot Resource reservations.
//!
//! The Resource service owns approval, capacity, leases, and usage records.  Evaluation only
//! submits a typed task request and observes the authoritative response.  This module keeps the
//! HTTP boundary deliberately small: HTTPS with an explicit CA, no redirects, bounded request
//! and response bodies, and an audience-scoped service token for every call.

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
    TaskRunId,
    http::{
        AcknowledgeTaskResourceRequest, InternalCreateTaskResourceRequest,
        RecordResourceUsageRequest, ReleaseTaskResourceRequest, ResourceRequestMutation,
        TaskResourceStatus,
    },
    resource::{ResourceRequest, ResourceTarget, ResourceUsageRecord},
};
use futures_util::StreamExt;
use reqwest::{
    Certificate, Client, Method, StatusCode, Url,
    header::{CONTENT_TYPE, HeaderMap, HeaderValue},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;

const MAX_CA_BYTES: u64 = 1024 * 1024;
const MIN_TIMEOUT_MILLISECONDS: u64 = 100;
const MAX_TIMEOUT_MILLISECONDS: u64 = 60_000;
const MAX_REQUEST_BYTES: u64 = 1024 * 1024;
const MAX_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;

const REQUIRED_SCOPES: [&str; 7] = [
    "resource.task.create",
    "resource.task.read",
    "resource.task.claim",
    "resource.task.ack",
    "resource.task.release",
    "resource.task.cancel",
    "resource.usage.record",
];

/// Returns the service scopes required for the complete task-resource lifecycle.
#[must_use]
pub const fn required_scopes_for_diagnostics() -> &'static [&'static str] {
    &REQUIRED_SCOPES
}

/// Evaluation's downstream Resource HTTP configuration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceClientConfiguration {
    pub base_uri: Url,
    pub ca_file: PathBuf,
    pub timeout_milliseconds: u64,
    pub max_request_bytes: u64,
    pub max_response_bytes: u64,
    pub audience: String,
}

impl ResourceClientConfiguration {
    fn validate(&self) -> Result<(), ResourceClientError> {
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
            return Err(ResourceClientError::Configuration);
        }
        Ok(())
    }
}

/// A bounded, authenticated Resource API client.
#[derive(Clone)]
pub struct ResourceClient {
    base_uri: Url,
    client: Client,
    token_client: Arc<ServiceTokenClient>,
    audience: String,
    scopes: BTreeSet<String>,
    max_request_bytes: usize,
    max_response_bytes: usize,
}

impl std::fmt::Debug for ResourceClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResourceClient")
            .field("base_uri", &self.base_uri)
            .field("audience", &self.audience)
            .field("scopes", &self.scopes)
            .field("max_request_bytes", &self.max_request_bytes)
            .field("max_response_bytes", &self.max_response_bytes)
            .finish_non_exhaustive()
    }
}

impl ResourceClient {
    /// Constructs a client with a caller-supplied HTTP client.
    ///
    /// The supplied client is expected to implement the same strict transport policy as
    /// [`from_configuration`].  Keeping this constructor explicit makes transport policy
    /// visible in tests and prevents this module from silently choosing a system trust store.
    pub fn new(
        configuration: ResourceClientConfiguration,
        client: Client,
        token_client: Arc<ServiceTokenClient>,
        scopes: BTreeSet<String>,
    ) -> Result<Self, ResourceClientError> {
        configuration.validate()?;
        validate_scopes(&scopes)?;
        let max_request_bytes = usize::try_from(configuration.max_request_bytes)
            .map_err(|_| ResourceClientError::Configuration)?;
        let max_response_bytes = usize::try_from(configuration.max_response_bytes)
            .map_err(|_| ResourceClientError::Configuration)?;
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
        configuration: ResourceClientConfiguration,
        token_client: Arc<ServiceTokenClient>,
        scopes: BTreeSet<String>,
    ) -> Result<Self, ResourceClientError> {
        configuration.validate()?;
        let ca = read_bounded_file(&configuration.ca_file, MAX_CA_BYTES)?;
        let certificate =
            Certificate::from_pem(&ca).map_err(|_| ResourceClientError::Configuration)?;
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .https_only(true)
            .tls_built_in_root_certs(false)
            .add_root_certificate(certificate)
            .timeout(Duration::from_millis(configuration.timeout_milliseconds))
            .build()
            .map_err(|_| ResourceClientError::Configuration)?;
        Self::new(configuration, client, token_client, scopes)
    }

    #[must_use]
    pub fn audience(&self) -> &str {
        &self.audience
    }

    /// Creates the Resource-owned one-shot reservation request.
    pub async fn create_task_resource(
        &self,
        request: &InternalCreateTaskResourceRequest,
    ) -> Result<ResourceRequest, ResourceClientError> {
        validate_create_request(request)?;
        let body = self.encode_request(request)?;
        let response = self
            .execute(
                Method::POST,
                "internal/v1/task-resources",
                Some(body),
                Some(request.request_key.as_str()),
                NotFound::Request,
            )
            .await?;
        let resource_request: ResourceRequest = decode_response(&response)?;
        validate_created_request(&resource_request, request)?;
        Ok(resource_request)
    }

    /// Reads the pending or terminal request projection before capacity is claimed.
    pub async fn get_task_resource_request(
        &self,
        task_run_id: TaskRunId,
    ) -> Result<ResourceRequest, ResourceClientError> {
        let path = format!("internal/v1/task-resources/{task_run_id}/request");
        let response = self
            .execute(Method::GET, &path, None, None, NotFound::Request)
            .await?;
        let resource_request: ResourceRequest = decode_response(&response)?;
        validate_task_request_identity(&resource_request, task_run_id)?;
        Ok(resource_request)
    }

    /// Cancels a pending task request after a durable Evaluation lease is lost.
    ///
    /// Resource remains the authority for the transition.  The idempotency key is supplied by
    /// Evaluation so a retry after a transport failure cannot create a second mutation.
    pub async fn cancel_task_resource(
        &self,
        task_run_id: TaskRunId,
        request: &ResourceRequestMutation,
        idempotency_key: &str,
    ) -> Result<ResourceRequest, ResourceClientError> {
        validate_mutation(request)?;
        if !valid_idempotency_key(idempotency_key) {
            return Err(ResourceClientError::RequestInvalid);
        }
        let body = self.encode_request(request)?;
        let path = format!("internal/v1/task-resources/{task_run_id}/cancel");
        let response = self
            .execute(
                Method::POST,
                &path,
                Some(body),
                Some(idempotency_key),
                NotFound::Request,
            )
            .await?;
        let resource_request: ResourceRequest = decode_response(&response)?;
        validate_task_request_identity(&resource_request, task_run_id)?;
        Ok(resource_request)
    }

    /// Claims an approved task reservation for the Evaluation attempt.
    pub async fn claim_task_resource(
        &self,
        task_run_id: TaskRunId,
    ) -> Result<TaskResourceStatus, ResourceClientError> {
        let path = format!("internal/v1/task-resources/{task_run_id}/claim");
        let response = self
            .execute(Method::POST, &path, None, None, NotFound::TaskResource)
            .await?;
        let status: TaskResourceStatus = decode_response(&response)?;
        validate_task_resource_status(&status, task_run_id)?;
        Ok(status)
    }

    /// Acknowledges the handoff after Evaluation has verified the exact claim and lease.
    pub async fn acknowledge_task_resource(
        &self,
        task_run_id: TaskRunId,
        request: &AcknowledgeTaskResourceRequest,
    ) -> Result<TaskResourceStatus, ResourceClientError> {
        validate_revision(request.expected_claim_revision)?;
        validate_revision(request.expected_lease_revision)?;
        if !valid_execution_namespace(&request.execution_namespace) {
            return Err(ResourceClientError::RequestInvalid);
        }
        let body = self.encode_request(request)?;
        let path = format!("internal/v1/task-resources/{task_run_id}/ack");
        let response = self
            .execute(
                Method::POST,
                &path,
                Some(body),
                None,
                NotFound::TaskResource,
            )
            .await?;
        let status: TaskResourceStatus = decode_response(&response)?;
        validate_task_resource_status(&status, task_run_id)?;
        if status.execution_namespace.as_deref() != Some(request.execution_namespace.as_str()) {
            return Err(ResourceClientError::ResponseInvalid);
        }
        Ok(status)
    }

    /// Reads the complete claim and lease projection while a task is executing.
    pub async fn get_task_resource(
        &self,
        task_run_id: TaskRunId,
    ) -> Result<TaskResourceStatus, ResourceClientError> {
        let path = format!("internal/v1/task-resources/{task_run_id}");
        let response = self
            .execute(Method::GET, &path, None, None, NotFound::TaskResource)
            .await?;
        let status: TaskResourceStatus = decode_response(&response)?;
        validate_task_resource_status(&status, task_run_id)?;
        Ok(status)
    }

    /// Releases a task reservation after execution and usage delivery are durable.
    pub async fn release_task_resource(
        &self,
        task_run_id: TaskRunId,
        request: &ReleaseTaskResourceRequest,
    ) -> Result<TaskResourceStatus, ResourceClientError> {
        validate_revision(request.expected_claim_revision)?;
        validate_revision(request.expected_lease_revision)?;
        let body = self.encode_request(request)?;
        let path = format!("internal/v1/task-resources/{task_run_id}/release");
        let response = self
            .execute(
                Method::POST,
                &path,
                Some(body),
                None,
                NotFound::TaskResource,
            )
            .await?;
        let status: TaskResourceStatus = decode_response(&response)?;
        validate_task_resource_status(&status, task_run_id)?;
        Ok(status)
    }

    /// Delivers one known or explicitly unknown usage observation to Resource.
    pub async fn record_resource_usage(
        &self,
        request: &RecordResourceUsageRequest,
    ) -> Result<ResourceUsageRecord, ResourceClientError> {
        validate_usage_request(request)?;
        let body = self.encode_request(request)?;
        let response = self
            .execute(
                Method::POST,
                "internal/v1/resource/usage",
                Some(body),
                None,
                NotFound::Request,
            )
            .await?;
        let record: ResourceUsageRecord = decode_response(&response)?;
        record
            .validate()
            .map_err(|_| ResourceClientError::ResponseInvalid)?;
        if record.project_id != request.project_id
            || record.course_id != request.course_id
            || record.kind != request.kind
            || record.request_id != request.request_id
            || record.lease_id != request.lease_id
            || record.source_event_id != request.source_event_id
            || record.measured_from != request.measured_from
            || record.measured_until != request.measured_until
            || record.measurement != request.measurement
        {
            return Err(ResourceClientError::ResponseInvalid);
        }
        Ok(record)
    }

    fn encode_request<T: Serialize>(&self, request: &T) -> Result<Vec<u8>, ResourceClientError> {
        let body = serde_json::to_vec(request).map_err(|_| ResourceClientError::RequestInvalid)?;
        if body.len() > self.max_request_bytes {
            return Err(ResourceClientError::RequestTooLarge);
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
    ) -> Result<Vec<u8>, ResourceClientError> {
        let url = self
            .base_uri
            .join(path)
            .map_err(|_| ResourceClientError::Configuration)?;
        if url.scheme() != "https" {
            return Err(ResourceClientError::Configuration);
        }
        let mut headers = HeaderMap::new();
        self.token_client
            .bearer_auth_for(&mut headers, &self.audience, &self.scopes)
            .await
            .map_err(ResourceClientError::Token)?;
        if body.is_some() {
            headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        }
        if let Some(idempotency_key) = idempotency_key {
            let value = HeaderValue::from_str(idempotency_key)
                .map_err(|_| ResourceClientError::RequestInvalid)?;
            headers.insert("idempotency-key", value);
        }
        let request = self.client.request(method, url).headers(headers);
        let request = if let Some(body) = body {
            request.body(body)
        } else {
            request
        };
        let response = request
            .send()
            .await
            .map_err(|_| ResourceClientError::Transport)?;
        let status = response.status();
        if status == StatusCode::NOT_FOUND {
            return Err(match not_found {
                NotFound::Request => ResourceClientError::RequestMissing,
                NotFound::TaskResource => ResourceClientError::TaskResourceMissing,
            });
        }
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(ResourceClientError::Denied);
        }
        if status == StatusCode::CONFLICT || status == StatusCode::PRECONDITION_FAILED {
            return Err(ResourceClientError::Conflict);
        }
        if status == StatusCode::PAYLOAD_TOO_LARGE {
            return Err(ResourceClientError::RequestTooLarge);
        }
        if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
            return Err(ResourceClientError::Unavailable);
        }
        if !status.is_success() {
            return Err(ResourceClientError::Rejected);
        }
        read_bounded_body(response, self.max_response_bytes).await
    }
}

#[derive(Clone, Copy)]
enum NotFound {
    Request,
    TaskResource,
}

fn validate_scopes(scopes: &BTreeSet<String>) -> Result<(), ResourceClientError> {
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
        return Err(ResourceClientError::Configuration);
    }
    Ok(())
}

fn validate_create_request(
    request: &InternalCreateTaskResourceRequest,
) -> Result<(), ResourceClientError> {
    if !(16..=96).contains(&request.request_key.len())
        || !request.request_key.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
        })
        || request.duration_seconds == 0
    {
        return Err(ResourceClientError::RequestInvalid);
    }
    request
        .resources
        .validate()
        .map_err(|_| ResourceClientError::RequestInvalid)
}

fn validate_mutation(request: &ResourceRequestMutation) -> Result<(), ResourceClientError> {
    validate_revision(request.expected_revision)?;
    if request.reason.trim().is_empty()
        || request.reason.chars().count() > 500
        || request.reason.chars().any(char::is_control)
    {
        return Err(ResourceClientError::RequestInvalid);
    }
    Ok(())
}

fn valid_idempotency_key(value: &str) -> bool {
    (16..=128).contains(&value.len())
        && !value.chars().any(char::is_control)
        && !value.chars().any(char::is_whitespace)
}

fn validate_created_request(
    response: &ResourceRequest,
    request: &InternalCreateTaskResourceRequest,
) -> Result<(), ResourceClientError> {
    response
        .validate()
        .map_err(|_| ResourceClientError::ResponseInvalid)?;
    if response.request_key != request.request_key
        || response.requester_id != request.owner_id
        || response.project_id != request.project_id
        || response.course_id != request.course_id
        || response.requested_resources != request.resources
        || response.requested_duration_seconds != request.duration_seconds
    {
        return Err(ResourceClientError::ResponseInvalid);
    }
    validate_task_request_identity(response, request.task_run_id)
}

fn validate_task_request_identity(
    request: &ResourceRequest,
    task_run_id: TaskRunId,
) -> Result<(), ResourceClientError> {
    if !matches!(
        request.target,
        ResourceTarget::Task { task_run_id: id } if id == task_run_id
    ) {
        return Err(ResourceClientError::ResponseInvalid);
    }
    request
        .validate()
        .map_err(|_| ResourceClientError::ResponseInvalid)
}

fn validate_task_resource_status(
    status: &TaskResourceStatus,
    task_run_id: TaskRunId,
) -> Result<(), ResourceClientError> {
    if status.task_run_id != task_run_id
        || status.request.project_id != status.project_id
        || status.request.requester_id != status.owner_id
        || status.claim.request_id != status.request.id
        || status.lease.request_id != status.request.id
        || status.lease.claim_id != status.claim.id
        || status.claim_revision != status.claim.revision
        || status.lease_revision != status.lease.revision
    {
        return Err(ResourceClientError::ResponseInvalid);
    }
    validate_task_request_identity(&status.request, task_run_id)?;
    status
        .claim
        .validate()
        .map_err(|_| ResourceClientError::ResponseInvalid)?;
    status
        .lease
        .validate()
        .map_err(|_| ResourceClientError::ResponseInvalid)
}

fn validate_revision(revision: contracts::Revision) -> Result<(), ResourceClientError> {
    (revision.get() > 0)
        .then_some(())
        .ok_or(ResourceClientError::RequestInvalid)
}

fn valid_execution_namespace(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !value.starts_with('-')
        && !value.ends_with('-')
}

fn validate_usage_request(request: &RecordResourceUsageRequest) -> Result<(), ResourceClientError> {
    if request.measured_until <= request.measured_from {
        return Err(ResourceClientError::RequestInvalid);
    }
    request
        .measurement
        .validate()
        .map_err(|_| ResourceClientError::RequestInvalid)
}

fn decode_response<T: DeserializeOwned>(body: &[u8]) -> Result<T, ResourceClientError> {
    if body.is_empty() {
        return Err(ResourceClientError::ResponseInvalid);
    }
    serde_json::from_slice(body).map_err(|_| ResourceClientError::ResponseInvalid)
}

async fn read_bounded_body(
    response: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>, ResourceClientError> {
    if response
        .content_length()
        .is_some_and(|length| usize::try_from(length).map_or(true, |length| length > max_bytes))
    {
        return Err(ResourceClientError::ResponseTooLarge);
    }
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| ResourceClientError::Transport)?;
        let next = body
            .len()
            .checked_add(chunk.len())
            .ok_or(ResourceClientError::ResponseTooLarge)?;
        if next > max_bytes {
            return Err(ResourceClientError::ResponseTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn read_bounded_file(path: &Path, max_bytes: u64) -> Result<Vec<u8>, ResourceClientError> {
    if !path.is_absolute() {
        return Err(ResourceClientError::Configuration);
    }
    let parent = path.parent().ok_or(ResourceClientError::Configuration)?;
    let canonical_parent =
        fs::canonicalize(parent).map_err(|_| ResourceClientError::Configuration)?;
    let canonical = fs::canonicalize(path).map_err(|_| ResourceClientError::Configuration)?;
    let metadata = fs::metadata(&canonical).map_err(|_| ResourceClientError::Configuration)?;
    if !canonical.starts_with(canonical_parent)
        || !metadata.is_file()
        || metadata.len() == 0
        || metadata.len() > max_bytes
    {
        return Err(ResourceClientError::Configuration);
    }
    fs::read(canonical).map_err(|_| ResourceClientError::Configuration)
}

/// Stable failures raised at the Resource HTTP boundary.
#[derive(Debug, Error)]
pub enum ResourceClientError {
    #[error("LW_EVALUATION_RESOURCE_CONFIG_INVALID")]
    Configuration,
    #[error("LW_EVALUATION_RESOURCE_TOKEN_FAILED")]
    Token(#[source] ServiceTokenClientError),
    #[error("LW_EVALUATION_RESOURCE_TRANSPORT_FAILED")]
    Transport,
    #[error("LW_EVALUATION_RESOURCE_REQUEST_INVALID")]
    RequestInvalid,
    #[error("LW_EVALUATION_RESOURCE_REQUEST_TOO_LARGE")]
    RequestTooLarge,
    #[error("LW_EVALUATION_RESOURCE_RESPONSE_TOO_LARGE")]
    ResponseTooLarge,
    #[error("LW_EVALUATION_RESOURCE_RESPONSE_INVALID")]
    ResponseInvalid,
    #[error("LW_EVALUATION_RESOURCE_REQUEST_MISSING")]
    RequestMissing,
    #[error("LW_EVALUATION_RESOURCE_TASK_MISSING")]
    TaskResourceMissing,
    #[error("LW_EVALUATION_RESOURCE_DENIED")]
    Denied,
    #[error("LW_EVALUATION_RESOURCE_CONFLICT")]
    Conflict,
    #[error("LW_EVALUATION_RESOURCE_REJECTED")]
    Rejected,
    #[error("LW_EVALUATION_RESOURCE_UNAVAILABLE")]
    Unavailable,
}
