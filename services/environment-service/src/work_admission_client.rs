//! Strict client for the Control-owned Work configuration admission boundary.
//!
//! Environment asks Control for the current `AgentRun` admission immediately before issuing a
//! short-lived execution certificate. The request is a GET with an explicit query, and the
//! response is checked against every identity supplied by the request before any VM credential
//! is signed.
#![allow(missing_docs, clippy::missing_errors_doc)]

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;

use auth::{
    ServiceTokenClient, ServiceTokenClientConfig, ServiceTokenClientError, TransportSecurityMode,
};
use contracts::http::{WorkConfigurationAdmissionBinding, WorkConfigurationAdmissionQuery};
use contracts::{AgentRunId, UtcTimestamp};
use futures_util::StreamExt;
use reqwest::{Certificate, Client, StatusCode, Url, header::HeaderMap};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;

const REQUIRED_SCOPE: &str = "control.agent_run.read";
const MAX_CA_BYTES: u64 = 1024 * 1024;
const MIN_TIMEOUT_MILLISECONDS: u64 = 100;
const MAX_TIMEOUT_MILLISECONDS: u64 = 60_000;
const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;

const CONTROL_BASE_URI: &str = "LABWEAVER_CONTROL_SERVICE_BASE_URI";
const CONTROL_CA_PATH: &str = "LABWEAVER_CONTROL_SERVICE_CA_PATH";
const CONTROL_AUDIENCE: &str = "LABWEAVER_CONTROL_SERVICE_AUDIENCE";
const CONTROL_TIMEOUT_MILLISECONDS: &str = "LABWEAVER_CONTROL_SERVICE_TIMEOUT_MILLISECONDS";

/// Control admission lookup used by Environment work execution.
///
/// The trait keeps the durable execution owner independent from the HTTP
/// transport while production continues to use [`WorkAdmissionClient`].
#[async_trait]
pub trait WorkAdmissionResolver: Send + Sync {
    async fn resolve(
        &self,
        run_id: AgentRunId,
        query: &WorkConfigurationAdmissionQuery,
        now: UtcTimestamp,
    ) -> Result<WorkConfigurationAdmissionBinding, WorkAdmissionClientError>;
}

/// Returns the Control permission required for a Work configuration admission lookup.
#[must_use]
pub const fn required_scope_for_diagnostics() -> &'static str {
    REQUIRED_SCOPE
}

/// Environment's Control Work admission HTTP configuration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkAdmissionClientConfiguration {
    pub base_uri: Url,
    pub ca_file: PathBuf,
    pub timeout_milliseconds: u64,
    pub max_response_bytes: u64,
    pub audience: String,
}

impl WorkAdmissionClientConfiguration {
    fn validate(&self) -> Result<(), WorkAdmissionClientError> {
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
            || self.max_response_bytes == 0
            || self.max_response_bytes > MAX_RESPONSE_BYTES
            || self.audience.trim().is_empty()
            || self.audience.chars().any(char::is_control)
        {
            return Err(WorkAdmissionClientError::Configuration);
        }
        Ok(())
    }
}

/// Authenticated, HTTPS-only Control admission client owned by Environment.
#[derive(Clone)]
pub struct WorkAdmissionClient {
    base_uri: Url,
    client: Client,
    token_client: Arc<ServiceTokenClient>,
    audience: String,
    scopes: BTreeSet<String>,
    max_response_bytes: usize,
}

impl std::fmt::Debug for WorkAdmissionClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkAdmissionClient")
            .field("base_uri", &self.base_uri)
            .field("audience", &self.audience)
            .field("scopes", &self.scopes)
            .field("max_response_bytes", &self.max_response_bytes)
            .finish_non_exhaustive()
    }
}

impl WorkAdmissionClient {
    /// Builds the client from the process environment and the common service-account settings.
    pub async fn from_env() -> Result<Self, WorkAdmissionClientError> {
        let configuration = WorkAdmissionClientConfiguration {
            base_uri: parse_url(&required(CONTROL_BASE_URI)?)?,
            ca_file: required_path(CONTROL_CA_PATH)?,
            timeout_milliseconds: required_u64(CONTROL_TIMEOUT_MILLISECONDS)?,
            max_response_bytes: MAX_RESPONSE_BYTES,
            audience: required(CONTROL_AUDIENCE)?,
        };
        configuration.validate()?;
        let ca = read_bounded_file(&configuration.ca_file, MAX_CA_BYTES)?;
        let certificate =
            Certificate::from_pem(&ca).map_err(|_| WorkAdmissionClientError::Configuration)?;
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .https_only(true)
            .tls_built_in_root_certs(false)
            .add_root_certificate(certificate)
            .timeout(Duration::from_millis(configuration.timeout_milliseconds))
            .build()
            .map_err(|_| WorkAdmissionClientError::Configuration)?;

        let issuer = required("LABWEAVER_SERVICE_OIDC_ISSUER")?;
        let oidc_ca =
            read_bounded_file(&required_path("LABWEAVER_SERVICE_OIDC_CA")?, MAX_CA_BYTES)?;
        let oidc_http =
            auth::no_redirect_http_client(Some(&oidc_ca), TransportSecurityMode::Strict)
                .map_err(|_| WorkAdmissionClientError::Configuration)?;
        let scopes = parse_scopes(&required("LABWEAVER_SERVICE_SCOPES")?)?;
        validate_scopes(&scopes)?;
        let token_config = ServiceTokenClientConfig::new(
            &issuer,
            required("LABWEAVER_SERVICE_CLIENT_ID")?,
            read_secret(&required_path("LABWEAVER_SERVICE_CLIENT_SECRET_FILE")?)?,
            configuration.audience.clone(),
            scopes.clone(),
            required_u64("LABWEAVER_SERVICE_TOKEN_REFRESH_SKEW_SECONDS")?,
            TransportSecurityMode::Strict,
        )
        .map_err(|_| WorkAdmissionClientError::Configuration)?;
        let token_client = ServiceTokenClient::discover(token_config, oidc_http)
            .await
            .map(Arc::new)
            .map_err(WorkAdmissionClientError::Token)?;
        Ok(Self {
            base_uri: configuration.base_uri,
            client,
            token_client,
            audience: configuration.audience,
            scopes,
            max_response_bytes: usize::try_from(configuration.max_response_bytes)
                .map_err(|_| WorkAdmissionClientError::Configuration)?,
        })
    }

    /// Constructs a client with a caller-supplied strict HTTP client.
    pub fn new(
        configuration: WorkAdmissionClientConfiguration,
        client: Client,
        token_client: Arc<ServiceTokenClient>,
        scopes: BTreeSet<String>,
    ) -> Result<Self, WorkAdmissionClientError> {
        configuration.validate()?;
        validate_scopes(&scopes)?;
        Ok(Self {
            base_uri: configuration.base_uri,
            client,
            token_client,
            audience: configuration.audience,
            scopes,
            max_response_bytes: usize::try_from(configuration.max_response_bytes)
                .map_err(|_| WorkAdmissionClientError::Configuration)?,
        })
    }

    /// Resolves one exact Work configuration admission snapshot from Control.
    ///
    /// `courseId` is omitted when the project has no teaching association. No request body is
    /// sent; all admission identities remain visible in the query string.
    pub async fn resolve(
        &self,
        run_id: AgentRunId,
        query: &WorkConfigurationAdmissionQuery,
        now: UtcTimestamp,
    ) -> Result<WorkConfigurationAdmissionBinding, WorkAdmissionClientError> {
        validate_query(query)?;
        let path = format!("internal/v1/agent-runs/{run_id}/work-configuration-admission");
        let url = self
            .base_uri
            .join(&path)
            .map_err(|_| WorkAdmissionClientError::Configuration)?;
        if url.scheme() != "https" {
            return Err(WorkAdmissionClientError::Configuration);
        }
        let mut headers = HeaderMap::new();
        self.token_client
            .bearer_auth_for(&mut headers, &self.audience, &self.scopes)
            .await
            .map_err(WorkAdmissionClientError::Token)?;
        let mut query_pairs = vec![
            ("projectId", query.project_id.to_string()),
            ("environmentId", query.environment_id.to_string()),
            (
                "environmentRevision",
                query.environment_revision.get().to_string(),
            ),
            ("actorId", query.actor_id.to_string()),
            ("runRevision", query.run_revision.get().to_string()),
        ];
        if let Some(execution_id) = query.execution_id {
            query_pairs.push(("executionId", execution_id.to_string()));
        }
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
            .map_err(|_| WorkAdmissionClientError::Transport)?;
        let status = response.status();
        if status == StatusCode::NOT_FOUND {
            return Err(WorkAdmissionClientError::AdmissionMissing);
        }
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(WorkAdmissionClientError::Denied);
        }
        if status == StatusCode::CONFLICT || status == StatusCode::PRECONDITION_FAILED {
            return Err(WorkAdmissionClientError::Conflict);
        }
        if status == StatusCode::PAYLOAD_TOO_LARGE {
            return Err(WorkAdmissionClientError::ResponseTooLarge);
        }
        if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
            return Err(WorkAdmissionClientError::Unavailable);
        }
        if !status.is_success() {
            return Err(WorkAdmissionClientError::Rejected);
        }
        let body = read_bounded_body(response, self.max_response_bytes).await?;
        let binding: WorkConfigurationAdmissionBinding = decode_response(&body)?;
        validate_binding(run_id, query, &binding, now)?;
        Ok(binding)
    }

    #[must_use]
    pub fn audience(&self) -> &str {
        &self.audience
    }
}

#[async_trait]
impl WorkAdmissionResolver for WorkAdmissionClient {
    async fn resolve(
        &self,
        run_id: AgentRunId,
        query: &WorkConfigurationAdmissionQuery,
        now: UtcTimestamp,
    ) -> Result<WorkConfigurationAdmissionBinding, WorkAdmissionClientError> {
        WorkAdmissionClient::resolve(self, run_id, query, now).await
    }
}

fn validate_scopes(scopes: &BTreeSet<String>) -> Result<(), WorkAdmissionClientError> {
    if !scopes.contains(REQUIRED_SCOPE)
        || scopes.iter().any(|scope| {
            scope.is_empty()
                || scope.len() > 128
                || scope.chars().any(char::is_control)
                || scope.chars().any(char::is_whitespace)
        })
    {
        return Err(WorkAdmissionClientError::Configuration);
    }
    Ok(())
}

fn validate_query(query: &WorkConfigurationAdmissionQuery) -> Result<(), WorkAdmissionClientError> {
    if query.environment_revision.get() == 0 || query.run_revision.get() == 0 {
        return Err(WorkAdmissionClientError::RequestInvalid);
    }
    Ok(())
}

fn validate_binding(
    run_id: AgentRunId,
    query: &WorkConfigurationAdmissionQuery,
    binding: &WorkConfigurationAdmissionBinding,
    now: UtcTimestamp,
) -> Result<(), WorkAdmissionClientError> {
    if binding.run_id != run_id
        || binding.project_id != query.project_id
        || binding.course_id != query.course_id
        || binding.environment_id != query.environment_id
        || binding.environment_revision != query.environment_revision
        || binding.actor_id != query.actor_id
        || binding.run_revision != query.run_revision
        || binding.state != contracts::authoring::AgentRunState::Running
    {
        return Err(WorkAdmissionClientError::ResponseInvalid);
    }
    if !valid_sha256(&binding.script_sha256)
        || binding
            .verification_script_sha256
            .as_deref()
            .is_some_and(|value| !valid_sha256(value))
    {
        return Err(WorkAdmissionClientError::ResponseInvalid);
    }
    // A Running AgentRun is not sufficient to issue an execution credential.  The run must
    // carry the exact generated plan and a concrete approved grant for that plan; otherwise a
    // caller could turn a merely started run into arbitrary Work execution.
    let plan = binding
        .plan
        .as_ref()
        .ok_or(WorkAdmissionClientError::ResponseInvalid)?;
    plan.validate()
        .map_err(|_| WorkAdmissionClientError::ResponseInvalid)?;
    if plan.environment_id != query.environment_id
        || plan.environment_revision != query.environment_revision
    {
        return Err(WorkAdmissionClientError::ResponseInvalid);
    }

    let preauthorization = binding
        .preauthorization
        .as_ref()
        .ok_or(WorkAdmissionClientError::ResponseInvalid)?;
    preauthorization
        .validate_against_plan(plan)
        .map_err(|_| WorkAdmissionClientError::ResponseInvalid)?;
    if preauthorization.project_id != query.project_id
        || preauthorization.environment_id != query.environment_id
        || preauthorization.environment_revision != query.environment_revision
        || preauthorization.actor_id != query.actor_id
        || preauthorization.expires_at <= now
    {
        return Err(WorkAdmissionClientError::ResponseInvalid);
    }
    Ok(())
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn decode_response<T: DeserializeOwned>(body: &[u8]) -> Result<T, WorkAdmissionClientError> {
    if body.is_empty() {
        return Err(WorkAdmissionClientError::ResponseInvalid);
    }
    serde_json::from_slice(body).map_err(|_| WorkAdmissionClientError::ResponseInvalid)
}

async fn read_bounded_body(
    response: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>, WorkAdmissionClientError> {
    if response
        .content_length()
        .is_some_and(|length| usize::try_from(length).map_or(true, |length| length > max_bytes))
    {
        return Err(WorkAdmissionClientError::ResponseTooLarge);
    }
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| WorkAdmissionClientError::Transport)?;
        let next = body
            .len()
            .checked_add(chunk.len())
            .ok_or(WorkAdmissionClientError::ResponseTooLarge)?;
        if next > max_bytes {
            return Err(WorkAdmissionClientError::ResponseTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn read_bounded_file(path: &Path, max_bytes: u64) -> Result<Vec<u8>, WorkAdmissionClientError> {
    if !path.is_absolute() {
        return Err(WorkAdmissionClientError::Configuration);
    }
    let parent = path
        .parent()
        .ok_or(WorkAdmissionClientError::Configuration)?;
    let canonical_parent =
        fs::canonicalize(parent).map_err(|_| WorkAdmissionClientError::Configuration)?;
    let canonical = fs::canonicalize(path).map_err(|_| WorkAdmissionClientError::Configuration)?;
    let metadata = fs::metadata(&canonical).map_err(|_| WorkAdmissionClientError::Configuration)?;
    if !canonical.starts_with(canonical_parent)
        || !metadata.is_file()
        || metadata.len() == 0
        || metadata.len() > max_bytes
    {
        return Err(WorkAdmissionClientError::Configuration);
    }
    fs::read(canonical).map_err(|_| WorkAdmissionClientError::Configuration)
}

fn required(name: &'static str) -> Result<String, WorkAdmissionClientError> {
    let value = std::env::var(name).map_err(|_| WorkAdmissionClientError::Configuration)?;
    if value.trim().is_empty() {
        return Err(WorkAdmissionClientError::Configuration);
    }
    Ok(value.trim().to_owned())
}

fn required_path(name: &'static str) -> Result<PathBuf, WorkAdmissionClientError> {
    let path = PathBuf::from(required(name)?);
    if !path.is_absolute() {
        return Err(WorkAdmissionClientError::Configuration);
    }
    Ok(path)
}

fn required_u64(name: &'static str) -> Result<u64, WorkAdmissionClientError> {
    required(name)?
        .parse()
        .map_err(|_| WorkAdmissionClientError::Configuration)
}

fn read_secret(path: &Path) -> Result<String, WorkAdmissionClientError> {
    let value = String::from_utf8(read_bounded_file(path, 16 * 1024)?)
        .map_err(|_| WorkAdmissionClientError::Configuration)?;
    let value = value.trim();
    if value.is_empty() || value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(WorkAdmissionClientError::Configuration);
    }
    Ok(value.to_owned())
}

fn parse_url(value: &str) -> Result<Url, WorkAdmissionClientError> {
    Url::parse(value).map_err(|_| WorkAdmissionClientError::Configuration)
}

fn parse_scopes(value: &str) -> Result<BTreeSet<String>, WorkAdmissionClientError> {
    let scopes = value
        .split([',', ' ', '\t', '\n', '\r'])
        .filter(|scope| !scope.is_empty())
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    if scopes.is_empty() {
        return Err(WorkAdmissionClientError::Configuration);
    }
    Ok(scopes)
}

/// Stable failures raised at the Environment-to-Control admission boundary.
#[derive(Debug, Error)]
pub enum WorkAdmissionClientError {
    #[error("LW_ENVIRONMENT_WORK_ADMISSION_CONFIG_INVALID")]
    Configuration,
    #[error("LW_ENVIRONMENT_WORK_ADMISSION_TOKEN_FAILED")]
    Token(#[source] ServiceTokenClientError),
    #[error("LW_ENVIRONMENT_WORK_ADMISSION_TRANSPORT_FAILED")]
    Transport,
    #[error("LW_ENVIRONMENT_WORK_ADMISSION_REQUEST_INVALID")]
    RequestInvalid,
    #[error("LW_ENVIRONMENT_WORK_ADMISSION_RESPONSE_TOO_LARGE")]
    ResponseTooLarge,
    #[error("LW_ENVIRONMENT_WORK_ADMISSION_RESPONSE_INVALID")]
    ResponseInvalid,
    #[error("LW_ENVIRONMENT_WORK_ADMISSION_MISSING")]
    AdmissionMissing,
    #[error("LW_ENVIRONMENT_WORK_ADMISSION_DENIED")]
    Denied,
    #[error("LW_ENVIRONMENT_WORK_ADMISSION_CONFLICT")]
    Conflict,
    #[error("LW_ENVIRONMENT_WORK_ADMISSION_REJECTED")]
    Rejected,
    #[error("LW_ENVIRONMENT_WORK_ADMISSION_UNAVAILABLE")]
    Unavailable,
}

#[cfg(test)]
mod tests {
    use super::{WorkAdmissionClientError, validate_binding};
    use contracts::authoring::{
        AgentRunState, WorkConfigurationPlan, WorkConfigurationPreauthorization,
    };
    use contracts::http::{WorkConfigurationAdmissionBinding, WorkConfigurationAdmissionQuery};
    use contracts::{
        ActorId, AgentRunId, ArtifactId, ArtifactRef, EnvironmentId, ProjectId, Revision,
        UtcTimestamp, WorkConfigurationPlanId, WorkConfigurationPreauthorizationId,
    };
    use persistence_sqlx::Sha256Digest;

    #[allow(clippy::expect_used)]
    fn fixture() -> (
        AgentRunId,
        WorkConfigurationAdmissionQuery,
        WorkConfigurationAdmissionBinding,
        UtcTimestamp,
    ) {
        let run_id = AgentRunId::new();
        let project_id = ProjectId::new();
        let environment_id = EnvironmentId::new();
        let actor_id = ActorId::new();
        let environment_revision = Revision::new(3).expect("fixed revision");
        let run_revision = Revision::new(4).expect("fixed revision");
        let plan_id = WorkConfigurationPlanId::new();
        let artifact = ArtifactRef {
            artifact_id: ArtifactId::new(),
            store_binding: "test-store".to_owned(),
            object_version: "object-v1".to_owned(),
            size_bytes: 1,
            media_type: "text/plain".to_owned(),
        };
        let plan = WorkConfigurationPlan {
            id: plan_id,
            revision: Revision::new(1).expect("fixed revision"),
            script_artifact: artifact.clone(),
            verification_script_artifact: None,
            summary: "approved configuration".to_owned(),
            requires_restart: false,
            environment_id,
            environment_revision,
        };
        let now: UtcTimestamp = "2026-09-08T00:00:00.000Z".parse().expect("fixed timestamp");
        let binding = WorkConfigurationAdmissionBinding {
            run_id,
            project_id,
            course_id: None,
            environment_id,
            environment_revision,
            actor_id,
            run_revision,
            state: AgentRunState::Running,
            recovery: None,
            preauthorization: Some(WorkConfigurationPreauthorization {
                id: WorkConfigurationPreauthorizationId::new(),
                project_id,
                environment_id,
                environment_revision,
                actor_id,
                plan_id,
                plan_revision: plan.revision,
                script_artifact: artifact,
                verification_script_artifact: None,
                expires_at: "2026-09-08T00:05:00.000Z".parse().expect("fixed timestamp"),
                revision: Revision::new(1).expect("fixed revision"),
            }),
            plan: Some(plan),
            script_sha256: Sha256Digest::of_bytes(b"x").to_string(),
            verification_script_sha256: None,
        };
        let query = WorkConfigurationAdmissionQuery {
            project_id,
            course_id: None,
            environment_id,
            environment_revision,
            actor_id,
            run_revision,
            execution_id: None,
        };
        (run_id, query, binding, now)
    }

    #[test]
    fn running_admission_requires_exact_plan_and_grant() {
        let (run_id, query, binding, now) = fixture();
        assert!(validate_binding(run_id, &query, &binding, now).is_ok());

        let mut missing_plan = binding.clone();
        missing_plan.plan = None;
        assert!(matches!(
            validate_binding(run_id, &query, &missing_plan, now),
            Err(WorkAdmissionClientError::ResponseInvalid)
        ));

        let mut missing_grant = binding;
        missing_grant.preauthorization = None;
        assert!(matches!(
            validate_binding(run_id, &query, &missing_grant, now),
            Err(WorkAdmissionClientError::ResponseInvalid)
        ));
    }
}
