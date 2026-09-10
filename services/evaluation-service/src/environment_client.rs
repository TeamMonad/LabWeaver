//! Strict client for Environment-owned VM execution bindings.
//!
//! Evaluation never selects a VM address or reuses a long-lived SSH identity.  It asks
//! Environment for the current identity and a fresh certificate immediately before creating an
//! attempt.  The response is checked against the frozen environment and the generated key before
//! it can cross into the Kubernetes Job bundle.

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
    ActorId, CourseId, EnvironmentId, ProjectId, Revision, UtcTimestamp,
    authoring::RuntimeKind,
    environment::{
        EnvironmentExecutionBinding, EnvironmentExecutionBindingRequest,
        EnvironmentExecutionPurpose,
    },
};
use futures_util::StreamExt;
use rand::random;
use reqwest::{Certificate, Client, Method, StatusCode, Url, header::HeaderMap};
use russh::keys::ssh_key::{
    Certificate as SshCertificate, LineEnding, PrivateKey, private::Ed25519Keypair,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;

const REQUIRED_SCOPE: &str = "environment:resolve_evaluation_execution_binding";
const MAX_CA_BYTES: u64 = 1024 * 1024;
const MIN_TIMEOUT_MILLISECONDS: u64 = 100;
const MAX_TIMEOUT_MILLISECONDS: u64 = 60_000;
const MAX_REQUEST_BYTES: u64 = 1024 * 1024;
const MAX_RESPONSE_BYTES: u64 = 256 * 1024;
const MAX_CERTIFICATE_BYTES: usize = 16 * 1024;
const EVALUATION_PRINCIPAL: &str = "labweaver-evaluation";

/// Returns the Environment permission required for an Evaluation VM binding.
#[must_use]
pub const fn required_scope_for_diagnostics() -> &'static str {
    REQUIRED_SCOPE
}

/// Evaluation's downstream Environment HTTP configuration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentExecutionBindingClientConfiguration {
    pub base_uri: Url,
    pub ca_file: PathBuf,
    pub timeout_milliseconds: u64,
    pub max_request_bytes: u64,
    pub max_response_bytes: u64,
    pub audience: String,
}

impl EnvironmentExecutionBindingClientConfiguration {
    fn validate(&self) -> Result<(), EnvironmentExecutionBindingClientError> {
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
            return Err(EnvironmentExecutionBindingClientError::Configuration);
        }
        Ok(())
    }
}

/// Fresh private key and Environment-issued certificate for one attempt.
///
/// The private key is deliberately excluded from `Debug`; callers may log the result while
/// retaining the credential only long enough to create the attempt Secret.
pub struct ResolvedEnvironmentExecutionBinding {
    pub binding: EnvironmentExecutionBinding,
    private_key_openssh: String,
}

impl std::fmt::Debug for ResolvedEnvironmentExecutionBinding {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResolvedEnvironmentExecutionBinding")
            .field("binding", &self.binding)
            .field("private_key_openssh", &"[REDACTED]")
            .finish()
    }
}

impl ResolvedEnvironmentExecutionBinding {
    /// Returns the generated private key for one-shot Secret materialization.
    #[must_use]
    pub fn private_key_openssh(&self) -> &str {
        &self.private_key_openssh
    }

    /// Returns the Environment-issued OpenSSH certificate.
    #[must_use]
    pub fn certificate_openssh(&self) -> &str {
        match &self.binding.source {
            contracts::environment::EnvironmentExecutionSourceBinding::VirtualMachine {
                execution_certificate_openssh,
                ..
            } => execution_certificate_openssh,
        }
    }
}

/// Authenticated, HTTPS-only Environment binding client.
#[derive(Clone)]
pub struct EnvironmentExecutionBindingClient {
    base_uri: Url,
    client: Client,
    token_client: Arc<ServiceTokenClient>,
    audience: String,
    scopes: BTreeSet<String>,
    max_request_bytes: usize,
    max_response_bytes: usize,
}

impl std::fmt::Debug for EnvironmentExecutionBindingClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EnvironmentExecutionBindingClient")
            .field("base_uri", &self.base_uri)
            .field("audience", &self.audience)
            .field("scopes", &self.scopes)
            .field("max_request_bytes", &self.max_request_bytes)
            .field("max_response_bytes", &self.max_response_bytes)
            .finish_non_exhaustive()
    }
}

impl EnvironmentExecutionBindingClient {
    /// Constructs a client with a caller-supplied strict HTTP client.
    pub fn new(
        configuration: EnvironmentExecutionBindingClientConfiguration,
        client: Client,
        token_client: Arc<ServiceTokenClient>,
        scopes: BTreeSet<String>,
    ) -> Result<Self, EnvironmentExecutionBindingClientError> {
        configuration.validate()?;
        validate_scopes(&scopes)?;
        let max_request_bytes = usize::try_from(configuration.max_request_bytes)
            .map_err(|_| EnvironmentExecutionBindingClientError::Configuration)?;
        let max_response_bytes = usize::try_from(configuration.max_response_bytes)
            .map_err(|_| EnvironmentExecutionBindingClientError::Configuration)?;
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
        configuration: EnvironmentExecutionBindingClientConfiguration,
        token_client: Arc<ServiceTokenClient>,
        scopes: BTreeSet<String>,
    ) -> Result<Self, EnvironmentExecutionBindingClientError> {
        configuration.validate()?;
        let ca = read_bounded_file(&configuration.ca_file, MAX_CA_BYTES)?;
        let certificate = Certificate::from_pem(&ca)
            .map_err(|_| EnvironmentExecutionBindingClientError::Configuration)?;
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .https_only(true)
            .tls_built_in_root_certs(false)
            .add_root_certificate(certificate)
            .timeout(Duration::from_millis(configuration.timeout_milliseconds))
            .build()
            .map_err(|_| EnvironmentExecutionBindingClientError::Configuration)?;
        Self::new(configuration, client, token_client, scopes)
    }

    /// Resolves the current VM identity and generates a new key for one evaluation attempt.
    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors the immutable execution request"
    )]
    pub async fn resolve(
        &self,
        environment_id: EnvironmentId,
        project_id: ProjectId,
        course_id: Option<CourseId>,
        actor_id: ActorId,
        expected_revision: Revision,
        runtime_kind: RuntimeKind,
        run_id: contracts::EvaluationRunId,
        step_run_id: contracts::EvaluationStepRunId,
        attempt: u32,
    ) -> Result<ResolvedEnvironmentExecutionBinding, EnvironmentExecutionBindingClientError> {
        let key = PrivateKey::from(Ed25519Keypair::from_seed(&random::<[u8; 32]>()));
        let private_key_openssh = key
            .to_openssh(LineEnding::LF)
            .map_err(|_| EnvironmentExecutionBindingClientError::CredentialGeneration)?;
        let public_key_openssh = key
            .public_key()
            .to_openssh()
            .map_err(|_| EnvironmentExecutionBindingClientError::CredentialGeneration)?;
        let request = EnvironmentExecutionBindingRequest {
            project_id,
            course_id,
            actor_id,
            expected_revision,
            runtime_kind,
            purpose: EnvironmentExecutionPurpose::EvaluationProbe {
                run_id,
                step_run_id,
                attempt,
            },
            public_key_openssh,
        };
        request
            .validate()
            .map_err(|_| EnvironmentExecutionBindingClientError::RequestInvalid)?;
        let body = serde_json::to_vec(&request)
            .map_err(|_| EnvironmentExecutionBindingClientError::RequestInvalid)?;
        if body.len() > self.max_request_bytes {
            return Err(EnvironmentExecutionBindingClientError::RequestTooLarge);
        }
        let path =
            format!("internal/v1/environments/{environment_id}/execution-binding/evaluation");
        let url = self
            .base_uri
            .join(&path)
            .map_err(|_| EnvironmentExecutionBindingClientError::Configuration)?;
        if url.scheme() != "https" {
            return Err(EnvironmentExecutionBindingClientError::Configuration);
        }
        let mut headers = HeaderMap::new();
        self.token_client
            .bearer_auth_for(&mut headers, &self.audience, &self.scopes)
            .await
            .map_err(EnvironmentExecutionBindingClientError::Token)?;
        let response = self
            .client
            .request(Method::POST, url)
            .headers(headers)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(|_| EnvironmentExecutionBindingClientError::Transport)?;
        let status = response.status();
        if status == StatusCode::NOT_FOUND {
            return Err(EnvironmentExecutionBindingClientError::EnvironmentMissing);
        }
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(EnvironmentExecutionBindingClientError::Denied);
        }
        if status == StatusCode::CONFLICT || status == StatusCode::PRECONDITION_FAILED {
            return Err(EnvironmentExecutionBindingClientError::Conflict);
        }
        if status == StatusCode::PAYLOAD_TOO_LARGE {
            return Err(EnvironmentExecutionBindingClientError::ResponseTooLarge);
        }
        if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
            return Err(EnvironmentExecutionBindingClientError::Unavailable);
        }
        if !status.is_success() {
            return Err(EnvironmentExecutionBindingClientError::Rejected);
        }
        let body = read_bounded_body(response, self.max_response_bytes).await?;
        let binding: EnvironmentExecutionBinding = decode_response(&body)?;
        let now = UtcTimestamp::from_utc(time::OffsetDateTime::now_utc())
            .map_err(|_| EnvironmentExecutionBindingClientError::Clock)?;
        binding
            .validate_for(environment_id, &request, now)
            .map_err(|_| EnvironmentExecutionBindingClientError::ResponseInvalid)?;
        validate_execution_certificate(&binding, &key, now)?;
        Ok(ResolvedEnvironmentExecutionBinding {
            binding,
            private_key_openssh: private_key_openssh.to_string(),
        })
    }

    #[must_use]
    pub fn audience(&self) -> &str {
        &self.audience
    }
}

fn validate_scopes(
    scopes: &BTreeSet<String>,
) -> Result<(), EnvironmentExecutionBindingClientError> {
    if !scopes.contains(REQUIRED_SCOPE)
        || scopes.iter().any(|scope| {
            scope.is_empty()
                || scope.len() > 128
                || scope.chars().any(char::is_control)
                || scope.chars().any(char::is_whitespace)
        })
    {
        return Err(EnvironmentExecutionBindingClientError::Configuration);
    }
    Ok(())
}

fn validate_execution_certificate(
    binding: &EnvironmentExecutionBinding,
    key: &PrivateKey,
    now: UtcTimestamp,
) -> Result<(), EnvironmentExecutionBindingClientError> {
    let (certificate_openssh, expires_at) = match &binding.source {
        contracts::environment::EnvironmentExecutionSourceBinding::VirtualMachine {
            execution_certificate_openssh,
            expires_at,
            ..
        } => (execution_certificate_openssh, *expires_at),
    };
    if certificate_openssh.len() > MAX_CERTIFICATE_BYTES
        || certificate_openssh.chars().any(char::is_control)
    {
        return Err(EnvironmentExecutionBindingClientError::ResponseInvalid);
    }
    let certificate = SshCertificate::from_openssh(certificate_openssh)
        .map_err(|_| EnvironmentExecutionBindingClientError::ResponseInvalid)?;
    let now_seconds = u64::try_from(now.get().unix_timestamp())
        .map_err(|_| EnvironmentExecutionBindingClientError::ResponseInvalid)?;
    let expiry_seconds = u64::try_from(expires_at.get().unix_timestamp())
        .map_err(|_| EnvironmentExecutionBindingClientError::ResponseInvalid)?;
    if certificate.cert_type() != russh::keys::ssh_key::certificate::CertType::User
        || certificate.valid_after() > now_seconds
        || certificate.valid_before() <= now_seconds
        || certificate.valid_before() > expiry_seconds
        || certificate.public_key() != key.public_key().key_data()
        || certificate.valid_principals().len() != 1
        || certificate.valid_principals().first().map(String::as_str) != Some(EVALUATION_PRINCIPAL)
        || !certificate.critical_options().is_empty()
    {
        return Err(EnvironmentExecutionBindingClientError::ResponseInvalid);
    }
    Ok(())
}

fn decode_response<T: DeserializeOwned>(
    body: &[u8],
) -> Result<T, EnvironmentExecutionBindingClientError> {
    if body.is_empty() {
        return Err(EnvironmentExecutionBindingClientError::ResponseInvalid);
    }
    serde_json::from_slice(body)
        .map_err(|_| EnvironmentExecutionBindingClientError::ResponseInvalid)
}

async fn read_bounded_body(
    response: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>, EnvironmentExecutionBindingClientError> {
    if response
        .content_length()
        .is_some_and(|length| usize::try_from(length).map_or(true, |length| length > max_bytes))
    {
        return Err(EnvironmentExecutionBindingClientError::ResponseTooLarge);
    }
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| EnvironmentExecutionBindingClientError::Transport)?;
        let next = body
            .len()
            .checked_add(chunk.len())
            .ok_or(EnvironmentExecutionBindingClientError::ResponseTooLarge)?;
        if next > max_bytes {
            return Err(EnvironmentExecutionBindingClientError::ResponseTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn read_bounded_file(
    path: &Path,
    max_bytes: u64,
) -> Result<Vec<u8>, EnvironmentExecutionBindingClientError> {
    if !path.is_absolute() {
        return Err(EnvironmentExecutionBindingClientError::Configuration);
    }
    let parent = path
        .parent()
        .ok_or(EnvironmentExecutionBindingClientError::Configuration)?;
    let canonical_parent = fs::canonicalize(parent)
        .map_err(|_| EnvironmentExecutionBindingClientError::Configuration)?;
    let canonical = fs::canonicalize(path)
        .map_err(|_| EnvironmentExecutionBindingClientError::Configuration)?;
    let metadata = fs::metadata(&canonical)
        .map_err(|_| EnvironmentExecutionBindingClientError::Configuration)?;
    if !canonical.starts_with(canonical_parent)
        || !metadata.is_file()
        || metadata.len() == 0
        || metadata.len() > max_bytes
    {
        return Err(EnvironmentExecutionBindingClientError::Configuration);
    }
    fs::read(canonical).map_err(|_| EnvironmentExecutionBindingClientError::Configuration)
}

/// Stable failures at the Evaluation-to-Environment execution-binding boundary.
#[derive(Debug, Error)]
pub enum EnvironmentExecutionBindingClientError {
    #[error("LW_EVALUATION_ENVIRONMENT_BINDING_CONFIG_INVALID")]
    Configuration,
    #[error("LW_EVALUATION_ENVIRONMENT_BINDING_TOKEN_FAILED")]
    Token(#[source] ServiceTokenClientError),
    #[error("LW_EVALUATION_ENVIRONMENT_BINDING_TRANSPORT_FAILED")]
    Transport,
    #[error("LW_EVALUATION_ENVIRONMENT_BINDING_REQUEST_INVALID")]
    RequestInvalid,
    #[error("LW_EVALUATION_ENVIRONMENT_BINDING_REQUEST_TOO_LARGE")]
    RequestTooLarge,
    #[error("LW_EVALUATION_ENVIRONMENT_BINDING_RESPONSE_TOO_LARGE")]
    ResponseTooLarge,
    #[error("LW_EVALUATION_ENVIRONMENT_BINDING_RESPONSE_INVALID")]
    ResponseInvalid,
    #[error("LW_EVALUATION_ENVIRONMENT_BINDING_ENVIRONMENT_MISSING")]
    EnvironmentMissing,
    #[error("LW_EVALUATION_ENVIRONMENT_BINDING_DENIED")]
    Denied,
    #[error("LW_EVALUATION_ENVIRONMENT_BINDING_CONFLICT")]
    Conflict,
    #[error("LW_EVALUATION_ENVIRONMENT_BINDING_REJECTED")]
    Rejected,
    #[error("LW_EVALUATION_ENVIRONMENT_BINDING_UNAVAILABLE")]
    Unavailable,
    #[error("LW_EVALUATION_ENVIRONMENT_BINDING_CREDENTIAL_GENERATION_FAILED")]
    CredentialGeneration,
    #[error("LW_EVALUATION_ENVIRONMENT_BINDING_CLOCK_INVALID")]
    Clock,
}
