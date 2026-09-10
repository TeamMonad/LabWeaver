//! Durable execution consumer for approved Work configuration plans.
//!
//! The Agent owns the decision to start a plan, while Environment owns the container
//! execution record and VM identity.  This module keeps those boundaries explicit: a
//! container request is submitted once and subsequently observed by its run id; a VM
//! request retains only the target identity and observes the same remote execution
//! directory after a reconnect.  Neither recovery path submits a second script.

use std::{
    collections::BTreeSet,
    future::Future,
    net::IpAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use auth::{ServiceTokenClient, ServiceTokenClientError};
use contracts::{
    ActorId, AgentRunId, CourseId, EnvironmentId, ProjectId, Revision, UtcTimestamp,
    authoring::{AgentRun, AgentRunPurpose, AgentRunState, RuntimeKind, WorkConfigurationPlan},
    environment::{
        EnvironmentExecutionBinding, EnvironmentExecutionBindingRequest,
        EnvironmentExecutionPurpose, EnvironmentExecutionSourceBinding,
    },
    http::{
        ContainerWorkExecutionQuery, ContainerWorkExecutionReceipt, ContainerWorkExecutionRequest,
        ContainerWorkExecutionState, GeneratedArtifactKind,
    },
};
use futures_util::StreamExt;
use rand::random;
use reqwest::{Certificate, Client, Method, StatusCode, Url, header::HeaderMap};
use russh::{
    ChannelMsg, client,
    keys::ssh_key::{
        Certificate as SshCertificate, LineEnding, PrivateKey, private::Ed25519Keypair,
    },
};
use russh_sftp::{
    client::{SftpSession, error::Error as SftpError},
    protocol::StatusCode as SftpStatusCode,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use crate::{
    generated_artifacts::{GeneratedArtifactStore, GeneratedArtifactStoreError},
    run_store::{AgentRunStoreError, PostgresAgentRunStore, WorkExecutionLease},
};

const VM_AGENT_PRINCIPAL: &str = "labweaver-agent";
const VM_SSH_PORT: u16 = 22;
const VM_EXECUTION_ROOT: &str = "/tmp/labweaver-work-executions";
const VM_RUNNER: &[u8] = include_bytes!("../../work-configuration-runner.sh");
const TRACE_ID: &str = "agent-work-execution";
const MAX_SSH_FILE_BYTES: usize = ContainerWorkExecutionReceipt::MAX_OUTPUT_BYTES + 1024;
// The contract bounds decoded script bytes at 512 KiB per script. JSON escaping can expand
// control-heavy UTF-8 input several times, so the transport limit must leave room for the encoded
// request while the typed contract remains the authoritative content bound.
const MAX_HTTP_REQUEST_BYTES: usize = 8 * 1024 * 1024;
const MAX_HTTP_RESPONSE_BYTES: usize = 262_144;
const POLL_GRACE: Duration = Duration::from_secs(30);
const VM_CREDENTIAL_REFRESH_SKEW: Duration = Duration::from_mins(1);
const DEFAULT_EXECUTION_TIMEOUT: Duration = Duration::from_mins(15);
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Shared Agent Work execution configuration.
#[derive(Clone, Debug)]
pub struct WorkExecutionConfiguration {
    /// Environment Service base URI. It must be an HTTPS origin ending in `/`.
    pub environment_base_uri: Url,
    /// CA certificate used for the Environment Service connection.
    pub environment_ca_file: PathBuf,
    /// HTTP request timeout.
    pub request_timeout: Duration,
    /// Poll period for an accepted execution.
    pub poll_interval: Duration,
    /// Maximum lifetime assigned to a newly accepted execution.
    pub execution_timeout: Duration,
    /// Audience requested for Environment Service tokens.
    pub audience: String,
    /// OAuth scopes requested for the two Environment execution APIs.
    pub scopes: BTreeSet<String>,
}

impl WorkExecutionConfiguration {
    /// Default execution duration and polling policy used by the service startup wiring.
    #[must_use]
    pub fn defaults(
        environment_base_uri: Url,
        environment_ca_file: PathBuf,
        audience: String,
        scopes: BTreeSet<String>,
    ) -> Self {
        Self {
            environment_base_uri,
            environment_ca_file,
            request_timeout: Duration::from_secs(30),
            poll_interval: DEFAULT_POLL_INTERVAL,
            execution_timeout: DEFAULT_EXECUTION_TIMEOUT,
            audience,
            scopes,
        }
    }

    fn validate(&self) -> Result<(), WorkExecutionClientError> {
        if self.environment_base_uri.scheme() != "https"
            || self.environment_base_uri.host_str().is_none()
            || !self.environment_base_uri.username().is_empty()
            || self.environment_base_uri.password().is_some()
            || self.environment_base_uri.query().is_some()
            || self.environment_base_uri.fragment().is_some()
            || !self.environment_base_uri.path().ends_with('/')
            || !self.environment_ca_file.is_absolute()
            || self.request_timeout.is_zero()
            || self.request_timeout > Duration::from_mins(1)
            || self.poll_interval.is_zero()
            || self.poll_interval > Duration::from_mins(1)
            || self.execution_timeout.is_zero()
            || self.execution_timeout > Duration::from_hours(24)
            || self.audience.trim().is_empty()
            || self.audience.chars().any(char::is_control)
            || self.scopes.is_empty()
            || self.scopes.iter().any(|scope| {
                scope.trim().is_empty()
                    || scope.len() > 128
                    || scope.chars().any(char::is_control)
                    || scope.chars().any(char::is_whitespace)
            })
        {
            return Err(WorkExecutionClientError::Configuration);
        }
        Ok(())
    }
}

/// Authenticated Environment HTTP client for Work execution and VM bindings.
#[derive(Clone)]
pub struct WorkExecutionClient {
    base_uri: Url,
    client: Client,
    token_client: Arc<ServiceTokenClient>,
    audience: String,
    scopes: BTreeSet<String>,
    max_request_bytes: usize,
    max_response_bytes: usize,
}

impl std::fmt::Debug for WorkExecutionClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkExecutionClient")
            .field("base_uri", &self.base_uri)
            .field("audience", &self.audience)
            .field("scopes", &self.scopes)
            .finish_non_exhaustive()
    }
}

impl WorkExecutionClient {
    /// Constructs a client with an already configured TLS client.
    pub fn new(
        configuration: &WorkExecutionConfiguration,
        client: Client,
        token_client: Arc<ServiceTokenClient>,
    ) -> Result<Self, WorkExecutionClientError> {
        configuration.validate()?;
        Ok(Self {
            base_uri: configuration.environment_base_uri.clone(),
            client,
            token_client,
            audience: configuration.audience.clone(),
            scopes: configuration.scopes.clone(),
            max_request_bytes: MAX_HTTP_REQUEST_BYTES,
            max_response_bytes: MAX_HTTP_RESPONSE_BYTES,
        })
    }

    /// Constructs a client from a mounted CA certificate.
    pub fn from_configuration(
        configuration: &WorkExecutionConfiguration,
        token_client: Arc<ServiceTokenClient>,
    ) -> Result<Self, WorkExecutionClientError> {
        configuration.validate()?;
        let ca = read_bounded_file(
            &configuration.environment_ca_file,
            MAX_HTTP_REQUEST_BYTES as u64,
        )?;
        let certificate =
            Certificate::from_pem(&ca).map_err(|_| WorkExecutionClientError::Configuration)?;
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .https_only(true)
            .tls_built_in_root_certs(false)
            .add_root_certificate(certificate)
            .timeout(configuration.request_timeout)
            .build()
            .map_err(|_| WorkExecutionClientError::Configuration)?;
        Self::new(configuration, client, token_client)
    }

    /// Resolves one fresh Environment-owned VM identity and credential.
    #[allow(
        clippy::too_many_arguments,
        reason = "the Environment binding request carries its complete identity and revision"
    )]
    pub async fn resolve_work_binding(
        &self,
        environment_id: EnvironmentId,
        project_id: ProjectId,
        course_id: Option<CourseId>,
        actor_id: ActorId,
        environment_revision: Revision,
        run_id: AgentRunId,
        run_revision: Revision,
    ) -> Result<ResolvedVmBinding, WorkExecutionClientError> {
        self.resolve_work_binding_with_purpose(
            environment_id,
            project_id,
            course_id,
            actor_id,
            environment_revision,
            EnvironmentExecutionPurpose::WorkConfiguration {
                agent_run_id: run_id,
                run_revision,
            },
        )
        .await
    }

    /// Resolves a fresh credential for one already persisted VM execution intent.
    ///
    /// Environment receives the recovery purpose and validates the source identity before it
    /// signs the credential.  This path deliberately does not depend on the current membership
    /// or preauthorization state because it only observes or cleans up an existing side effect.
    async fn resolve_work_binding_recovery(
        &self,
        request: &VmWorkExecutionRequest,
    ) -> Result<ResolvedVmBinding, WorkExecutionClientError> {
        self.resolve_work_binding_with_purpose(
            request.environment_id,
            request.project_id,
            request.course_id,
            request.actor_id,
            request.environment_revision,
            EnvironmentExecutionPurpose::WorkConfigurationRecovery {
                agent_run_id: request.run_id,
                run_revision: request.run_revision,
                execution_id: request.execution_id,
                plan_id: request.plan_id,
                plan_revision: request.plan_revision,
                source_identity: request.target.source_identity.clone(),
            },
        )
        .await
    }

    async fn resolve_work_binding_with_purpose(
        &self,
        environment_id: EnvironmentId,
        project_id: ProjectId,
        course_id: Option<CourseId>,
        actor_id: ActorId,
        environment_revision: Revision,
        purpose: EnvironmentExecutionPurpose,
    ) -> Result<ResolvedVmBinding, WorkExecutionClientError> {
        let key = PrivateKey::from(Ed25519Keypair::from_seed(&random::<[u8; 32]>()));
        let private_key_openssh = key
            .to_openssh(LineEnding::LF)
            .map_err(|_| WorkExecutionClientError::CredentialGeneration)?;
        let public_key_openssh = key
            .public_key()
            .to_openssh()
            .map_err(|_| WorkExecutionClientError::CredentialGeneration)?;
        let request = EnvironmentExecutionBindingRequest {
            project_id,
            course_id,
            actor_id,
            expected_revision: environment_revision,
            runtime_kind: RuntimeKind::VirtualMachine,
            purpose,
            public_key_openssh,
        };
        request
            .validate()
            .map_err(|_| WorkExecutionClientError::RequestInvalid)?;
        let binding: EnvironmentExecutionBinding = self
            .post_json(
                &format!("internal/v1/environments/{environment_id}/execution-binding/work"),
                &request,
            )
            .await?;
        let now = timestamp_now().map_err(|_| WorkExecutionClientError::Clock)?;
        binding
            .validate_for(environment_id, &request, now)
            .map_err(|_| WorkExecutionClientError::ResponseInvalid)?;
        let source = match &binding.source {
            EnvironmentExecutionSourceBinding::VirtualMachine {
                namespace,
                host,
                port,
                username,
                workspace_root,
                expected_host_key_sha256,
                source_identity,
                execution_certificate_openssh,
                expires_at,
            } => {
                let target = VmExecutionTarget {
                    namespace: namespace.clone(),
                    host: host.clone(),
                    port: *port,
                    username: username.clone(),
                    workspace_root: workspace_root.clone(),
                    expected_host_key_sha256: expected_host_key_sha256.clone(),
                    source_identity: source_identity.clone(),
                };
                validate_vm_target(&target)
                    .map_err(|_| WorkExecutionClientError::ResponseInvalid)?;
                validate_execution_certificate(
                    execution_certificate_openssh,
                    &key,
                    *expires_at,
                    now,
                )?;
                (target, execution_certificate_openssh.clone(), *expires_at)
            }
        };
        Ok(ResolvedVmBinding {
            target: source.0,
            private_key_openssh: private_key_openssh.to_string(),
            certificate_openssh: source.1,
            expires_at: source.2,
        })
    }

    /// Starts one container execution. The caller must never repeat this call after an uncertain response.
    pub async fn start_container(
        &self,
        request: &ContainerWorkExecutionRequest,
    ) -> Result<ContainerWorkExecutionReceipt, WorkExecutionClientError> {
        request
            .validate()
            .map_err(|_| WorkExecutionClientError::RequestInvalid)?;
        self.post_json("internal/v1/work-configurations", request)
            .await
    }

    /// Reads the Environment-owned container receipt for the same run.
    pub async fn query_container(
        &self,
        run_id: AgentRunId,
        query: &ContainerWorkExecutionQuery,
    ) -> Result<ContainerWorkExecutionReceipt, WorkExecutionClientError> {
        let path = format!("internal/v1/work-configurations/{run_id}");
        self.get_json(&path, query).await
    }

    /// Requests cancellation of the Environment-owned container execution.
    pub async fn cancel_container(
        &self,
        run_id: AgentRunId,
        query: &ContainerWorkExecutionQuery,
    ) -> Result<ContainerWorkExecutionReceipt, WorkExecutionClientError> {
        let path = format!("internal/v1/work-configurations/{run_id}/cancel");
        self.post_json_with_query(&path, query).await
    }

    async fn post_json<T: Serialize, R: DeserializeOwned>(
        &self,
        path: &str,
        body: &T,
    ) -> Result<R, WorkExecutionClientError> {
        let body =
            serde_json::to_vec(body).map_err(|_| WorkExecutionClientError::RequestInvalid)?;
        if body.len() > self.max_request_bytes {
            return Err(WorkExecutionClientError::RequestTooLarge);
        }
        self.request_json(Method::POST, path, Some(body), None)
            .await
    }

    async fn post_json_with_query<R: DeserializeOwned>(
        &self,
        path: &str,
        query: &ContainerWorkExecutionQuery,
    ) -> Result<R, WorkExecutionClientError> {
        self.request_json(
            Method::POST,
            path,
            None,
            Some(container_query_string(query)),
        )
        .await
    }

    async fn get_json<R: DeserializeOwned>(
        &self,
        path: &str,
        query: &ContainerWorkExecutionQuery,
    ) -> Result<R, WorkExecutionClientError> {
        self.request_json(Method::GET, path, None, Some(container_query_string(query)))
            .await
    }

    async fn request_json<R: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
        query: Option<String>,
    ) -> Result<R, WorkExecutionClientError> {
        let mut url = self
            .base_uri
            .join(path)
            .map_err(|_| WorkExecutionClientError::Configuration)?;
        if let Some(query) = query {
            url.set_query(Some(&query));
        }
        if url.scheme() != "https" {
            return Err(WorkExecutionClientError::Configuration);
        }
        let mut headers = HeaderMap::new();
        self.token_client
            .bearer_auth_for(&mut headers, &self.audience, &self.scopes)
            .await
            .map_err(WorkExecutionClientError::Token)?;
        let mut request = self.client.request(method, url).headers(headers);
        if let Some(body) = body {
            request = request
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body);
        }
        let response = request
            .send()
            .await
            .map_err(|_| WorkExecutionClientError::Transport)?;
        let status = response.status();
        if status == StatusCode::NOT_FOUND {
            return Err(WorkExecutionClientError::NotFound);
        }
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(WorkExecutionClientError::Denied);
        }
        if status == StatusCode::UNPROCESSABLE_ENTITY {
            return Err(WorkExecutionClientError::NotEligible);
        }
        if status == StatusCode::CONFLICT || status == StatusCode::PRECONDITION_FAILED {
            return Err(WorkExecutionClientError::Conflict);
        }
        if status == StatusCode::PAYLOAD_TOO_LARGE {
            return Err(WorkExecutionClientError::ResponseTooLarge);
        }
        if status == StatusCode::TOO_MANY_REQUESTS {
            return Err(WorkExecutionClientError::UnavailableWithStatus {
                status: status.as_u16(),
            });
        }
        if status.is_server_error() {
            let body = read_bounded_response(response, self.max_response_bytes).await?;
            return Err(classify_server_error(status, &body));
        }
        if !status.is_success() {
            return Err(WorkExecutionClientError::Rejected);
        }
        let body = read_bounded_response(response, self.max_response_bytes).await?;
        serde_json::from_slice(&body).map_err(|_| WorkExecutionClientError::ResponseInvalid)
    }
}

/// A fresh VM credential and the exact target identity it is bound to.
pub struct ResolvedVmBinding {
    pub target: VmExecutionTarget,
    private_key_openssh: String,
    certificate_openssh: String,
    expires_at: UtcTimestamp,
}

impl std::fmt::Debug for ResolvedVmBinding {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResolvedVmBinding")
            .field("target", &self.target)
            .field("private_key_openssh", &"[REDACTED]")
            .field("certificate_openssh", &"[REDACTED]")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

impl ResolvedVmBinding {
    fn private_key(&self) -> Result<PrivateKey, VmExecutionError> {
        PrivateKey::from_openssh(&self.private_key_openssh)
            .map_err(|_| VmExecutionError::CredentialInvalid)
    }

    fn certificate(&self) -> Result<SshCertificate, VmExecutionError> {
        SshCertificate::from_openssh(&self.certificate_openssh)
            .map_err(|_| VmExecutionError::CredentialInvalid)
    }

    fn expires_within(
        &self,
        now: UtcTimestamp,
        skew: Duration,
    ) -> Result<bool, WorkExecutionWorkerError> {
        let milliseconds =
            i64::try_from(skew.as_millis()).map_err(|_| WorkExecutionWorkerError::Configuration)?;
        let threshold = now
            .get()
            .checked_add(time::Duration::milliseconds(milliseconds))
            .ok_or(WorkExecutionWorkerError::Configuration)?;
        Ok(self.expires_at.get() <= threshold)
    }
}

/// VM target identity retained in the Agent execution intent.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VmExecutionTarget {
    pub namespace: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub workspace_root: String,
    pub expected_host_key_sha256: String,
    pub source_identity: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum VmWorkExecutionKind {
    VirtualMachine,
}

/// Private Agent intent for one VM execution. Credentials are deliberately not persisted.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VmWorkExecutionRequest {
    kind: VmWorkExecutionKind,
    execution_id: Uuid,
    run_id: AgentRunId,
    run_revision: Revision,
    plan_id: contracts::WorkConfigurationPlanId,
    plan_revision: Revision,
    project_id: ProjectId,
    course_id: Option<CourseId>,
    environment_id: EnvironmentId,
    environment_revision: Revision,
    actor_id: ActorId,
    script_content: String,
    verification_script_content: Option<String>,
    deadline_at: UtcTimestamp,
    target: VmExecutionTarget,
}

impl VmWorkExecutionRequest {
    fn validate(&self) -> Result<(), VmExecutionError> {
        if self.kind != VmWorkExecutionKind::VirtualMachine
            || self.run_revision.get() == 0
            || self.plan_revision.get() == 0
            || self.environment_revision.get() == 0
            || self.script_content.is_empty()
            || self.script_content.len() > ContainerWorkExecutionRequest::MAX_SCRIPT_BYTES
            || self
                .verification_script_content
                .as_ref()
                .is_some_and(|script| {
                    script.is_empty()
                        || script.len() > ContainerWorkExecutionRequest::MAX_SCRIPT_BYTES
                })
        {
            return Err(VmExecutionError::RequestInvalid);
        }
        validate_vm_target(&self.target)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AbortReason {
    UserCancellation,
    Deadline,
}

#[derive(Clone, Debug)]
struct VmObservation {
    execution_id: Uuid,
    run_id: AgentRunId,
    plan_id: contracts::WorkConfigurationPlanId,
    plan_revision: Revision,
    environment_id: EnvironmentId,
    environment_revision: Revision,
    source_identity: String,
    state: VmObservationState,
    exit_code: Option<i32>,
    verification_exit_code: Option<i32>,
    output: String,
    output_truncated: bool,
    diagnostic_code: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VmObservationState {
    Succeeded,
    Failed,
    Cancelled,
    CleanupFailed,
}

impl VmObservation {
    fn terminal() -> bool {
        true
    }

    fn into_value(self) -> Value {
        json!({
            "kind": "virtual_machine",
            "executionId": self.execution_id,
            "runId": self.run_id,
            "planId": self.plan_id,
            "planRevision": self.plan_revision,
            "environmentId": self.environment_id,
            "environmentRevision": self.environment_revision,
            "sourceIdentity": self.source_identity,
            "state": match self.state {
                VmObservationState::Succeeded => "succeeded",
                VmObservationState::Failed => "failed",
                VmObservationState::Cancelled => "cancelled",
                VmObservationState::CleanupFailed => "cleanup_failed",
            },
            "exitCode": self.exit_code,
            "verificationExitCode": self.verification_exit_code,
            "output": self.output,
            "outputTruncated": self.output_truncated,
            "diagnosticCode": self.diagnostic_code,
        })
    }
}

/// Durable consumer for approved Work configuration attempts.
#[derive(Clone)]
pub struct WorkExecutionWorker {
    store: PostgresAgentRunStore,
    objects: Arc<artifact_store::S3ImmutableObjectStore>,
    generated_artifacts: GeneratedArtifactStore,
    environment: WorkExecutionClient,
    worker_id: String,
    lease_duration: Duration,
    poll_interval: Duration,
    execution_timeout: Duration,
}

impl std::fmt::Debug for WorkExecutionWorker {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkExecutionWorker")
            .field("worker_id", &self.worker_id)
            .field("lease_duration", &self.lease_duration)
            .field("poll_interval", &self.poll_interval)
            .field("execution_timeout", &self.execution_timeout)
            .finish_non_exhaustive()
    }
}

impl WorkExecutionWorker {
    /// Builds a production consumer.
    pub fn new(
        store: PostgresAgentRunStore,
        objects: Arc<artifact_store::S3ImmutableObjectStore>,
        generated_artifacts: GeneratedArtifactStore,
        environment: WorkExecutionClient,
        worker_id: String,
        lease_duration: Duration,
        configuration: &WorkExecutionConfiguration,
    ) -> Result<Self, WorkExecutionWorkerError> {
        if worker_id.trim().is_empty()
            || lease_duration.is_zero()
            || configuration.poll_interval.is_zero()
            || configuration.execution_timeout.is_zero()
        {
            return Err(WorkExecutionWorkerError::Configuration);
        }
        Ok(Self {
            store,
            objects,
            generated_artifacts,
            environment,
            worker_id,
            lease_duration,
            poll_interval: configuration.poll_interval,
            execution_timeout: configuration.execution_timeout,
        })
    }

    /// Processes at most 128 approved or recoverable executions.
    pub async fn run_once(&self) -> Result<u32, WorkExecutionWorkerError> {
        let candidates = self.store.work_execution_candidates(128).await?;
        let mut processed = 0;
        for (run_id, fresh) in candidates {
            match self.process_candidate(run_id, fresh).await {
                Ok(()) => processed += 1,
                Err(error) => {
                    tracing::error!(
                        event = "agent.work_execution.consumer_failed",
                        run_id = %run_id,
                        fresh,
                        error = %error,
                        error_kind = "work_execution_consumer",
                        retryable = error.retryable(),
                    );
                    if !error.retryable() {
                        return Err(error);
                    }
                }
            }
        }
        Ok(processed)
    }

    /// Runs the durable consumer until the service is stopped or a fatal worker error occurs.
    pub async fn run(self) -> Result<(), WorkExecutionWorkerError> {
        run_worker_loop(self.poll_interval, || self.run_once()).await
    }

    #[allow(
        clippy::too_many_lines,
        reason = "fresh submission and recovery share one candidate state machine"
    )]
    async fn process_candidate(
        &self,
        run_id: AgentRunId,
        fresh: bool,
    ) -> Result<(), WorkExecutionWorkerError> {
        let run = self.store.load(run_id).await?;
        let plan = run
            .plan
            .clone()
            .ok_or(WorkExecutionWorkerError::InvalidState)?;
        let (environment_id, environment_revision, actor_id) = work_identity(&run)?;
        let runtime_kind = run
            .purpose
            .work_runtime_kind()
            .ok_or(WorkExecutionWorkerError::InvalidState)?;
        if fresh {
            match runtime_kind {
                RuntimeKind::Container => {
                    let request = self.build_container_request(&run, &plan).await?;
                    let value = serde_json::to_value(&request)
                        .map_err(|_| WorkExecutionWorkerError::InvalidState)?;
                    let Some(lease) = self
                        .store
                        .claim_work_execution(
                            run.id,
                            &self.worker_id,
                            self.lease_duration,
                            Some(value),
                        )
                        .await?
                    else {
                        return Ok(());
                    };
                    self.process_container(lease, request).await
                }
                RuntimeKind::VirtualMachine => {
                    let binding = self
                        .environment
                        .resolve_work_binding(
                            environment_id,
                            run.project_id,
                            run.course_id,
                            actor_id,
                            environment_revision,
                            run.id,
                            run.revision,
                        )
                        .await?;
                    let request = self
                        .build_vm_request(&run, &plan, binding.target.clone())
                        .await?;
                    let value = serde_json::to_value(&request)
                        .map_err(|_| WorkExecutionWorkerError::InvalidState)?;
                    let Some(lease) = self
                        .store
                        .claim_work_execution(
                            run.id,
                            &self.worker_id,
                            self.lease_duration,
                            Some(value),
                        )
                        .await?
                    else {
                        return Ok(());
                    };
                    self.process_vm(lease, request, binding).await
                }
            }
        } else {
            let Some(lease) = self
                .store
                .claim_work_execution(run.id, &self.worker_id, self.lease_duration, None)
                .await?
            else {
                return Ok(());
            };
            match (runtime_kind, parse_persisted_request(&lease.request)?) {
                (RuntimeKind::Container, PersistedWorkRequest::Container(request)) => {
                    self.process_container(lease, request).await
                }
                (RuntimeKind::VirtualMachine, PersistedWorkRequest::Vm(request)) => {
                    request.validate()?;
                    let binding = match self
                        .environment
                        .resolve_work_binding_recovery(&request)
                        .await
                    {
                        Ok(binding) => binding,
                        Err(error) if error.uncertain() => {
                            return Err(WorkExecutionWorkerError::from(error));
                        }
                        Err(error) => {
                            return self
                                .complete_failure(
                                    lease,
                                    request.execution_id,
                                    error.diagnostic_code(),
                                )
                                .await;
                        }
                    };
                    if binding.target != request.target {
                        return self
                            .complete_failure(
                                lease,
                                request.execution_id,
                                "LW_AGENT_WORK_EXECUTION_TARGET_IDENTITY_CHANGED",
                            )
                            .await;
                    }
                    self.process_vm(lease, request, binding).await
                }
                (_, _) => {
                    self.complete_failure(
                        lease,
                        Uuid::nil(),
                        "LW_AGENT_WORK_EXECUTION_RUNTIME_KIND_MISMATCH",
                    )
                    .await
                }
            }
        }
    }

    async fn build_container_request(
        &self,
        run: &AgentRun,
        plan: &WorkConfigurationPlan,
    ) -> Result<ContainerWorkExecutionRequest, WorkExecutionWorkerError> {
        let (environment_id, environment_revision, actor_id) = work_identity(run)?;
        let script_content = self
            .read_script(
                run,
                &plan.script_artifact,
                GeneratedArtifactKind::WorkScript,
            )
            .await?;
        let verification_script_content = match &plan.verification_script_artifact {
            Some(reference) => Some(
                self.read_script(run, reference, GeneratedArtifactKind::VerificationScript)
                    .await?,
            ),
            None => None,
        };
        let deadline_at = add_duration(
            timestamp_now().map_err(|_| WorkExecutionWorkerError::Clock)?,
            self.execution_timeout,
        )?;
        let request = ContainerWorkExecutionRequest {
            run_id: run.id,
            run_revision: run.revision,
            plan_id: plan.id,
            plan_revision: plan.revision,
            project_id: run.project_id,
            course_id: run.course_id,
            environment_id,
            environment_revision,
            actor_id,
            script_content,
            verification_script_content,
            deadline_at,
        };
        request
            .validate()
            .map_err(|_| WorkExecutionWorkerError::InvalidState)?;
        Ok(request)
    }

    async fn build_vm_request(
        &self,
        run: &AgentRun,
        plan: &WorkConfigurationPlan,
        target: VmExecutionTarget,
    ) -> Result<VmWorkExecutionRequest, WorkExecutionWorkerError> {
        let (environment_id, environment_revision, actor_id) = work_identity(run)?;
        let script_content = self
            .read_script(
                run,
                &plan.script_artifact,
                GeneratedArtifactKind::WorkScript,
            )
            .await?;
        let verification_script_content = match &plan.verification_script_artifact {
            Some(reference) => Some(
                self.read_script(run, reference, GeneratedArtifactKind::VerificationScript)
                    .await?,
            ),
            None => None,
        };
        let request = VmWorkExecutionRequest {
            kind: VmWorkExecutionKind::VirtualMachine,
            execution_id: Uuid::now_v7(),
            run_id: run.id,
            run_revision: run.revision,
            plan_id: plan.id,
            plan_revision: plan.revision,
            project_id: run.project_id,
            course_id: run.course_id,
            environment_id,
            environment_revision,
            actor_id,
            script_content,
            verification_script_content,
            deadline_at: add_duration(
                timestamp_now().map_err(|_| WorkExecutionWorkerError::Clock)?,
                self.execution_timeout,
            )?,
            target,
        };
        request.validate()?;
        Ok(request)
    }

    async fn read_script(
        &self,
        run: &AgentRun,
        reference: &contracts::ArtifactRef,
        kind: GeneratedArtifactKind,
    ) -> Result<String, WorkExecutionWorkerError> {
        let (_, bytes) = self
            .generated_artifacts
            .read_verified_reference(
                &self.objects,
                reference,
                run.project_id,
                run.course_id,
                run.package_id,
                kind,
            )
            .await?;
        if reference.media_type != "text/x-shellscript" {
            return Err(WorkExecutionWorkerError::InvalidState);
        }
        String::from_utf8(bytes).map_err(|_| WorkExecutionWorkerError::InvalidState)
    }

    async fn process_container(
        &self,
        lease: WorkExecutionLease,
        request: ContainerWorkExecutionRequest,
    ) -> Result<(), WorkExecutionWorkerError> {
        request
            .validate()
            .map_err(|_| WorkExecutionWorkerError::InvalidState)?;
        if lease.run.state == AgentRunState::Cancelling {
            return self
                .complete_failure(lease, Uuid::nil(), "LW_AGENT_WORK_EXECUTION_CANCELLED")
                .await;
        }
        // A persisted intent means the start side effect may already have happened.  Recovery
        // therefore enters the query-only path; repeating POST after an expired lease could start
        // a second Environment execution for the same AgentRun.
        let (initial, uncertain_start) = if lease.fresh {
            match self.environment.start_container(&request).await {
                Ok(receipt) => (Some(receipt), false),
                Err(error) if error.uncertain() => {
                    tracing::warn!(
                        event = "agent.work_execution.submit_uncertain",
                        component = "work-execution",
                        operation = "work.start",
                        outcome = "uncertain",
                        run_id = %request.run_id,
                        http_status = ?error.http_status(),
                        diagnostic_code = error.diagnostic_code(),
                        error_kind = "submission_outcome_uncertain",
                        failure_stage = "environment.work_execution.submit",
                        retryable = true,
                        safe_detail = "submission_outcome_uncertain",
                    );
                    (None, true)
                }
                Err(error) => {
                    tracing::warn!(
                        event = "agent.work_execution.submit_rejected",
                        component = "work-execution",
                        operation = "work.start",
                        outcome = "rejected",
                        run_id = %request.run_id,
                        http_status = ?error.http_status(),
                        diagnostic_code = error.diagnostic_code(),
                        error_kind = "request_rejected",
                        failure_stage = "environment.work_execution.submit",
                        retryable = false,
                        safe_detail = "request_rejected",
                    );
                    return self
                        .complete_failure(lease, Uuid::nil(), error.diagnostic_code())
                        .await;
                }
            }
        } else {
            (None, true)
        };
        self.drive_container(lease, request, initial, uncertain_start)
            .await?;
        Ok(())
    }

    async fn drive_container(
        &self,
        lease: WorkExecutionLease,
        request: ContainerWorkExecutionRequest,
        initial: Option<ContainerWorkExecutionReceipt>,
        uncertain_start: bool,
    ) -> Result<(), WorkExecutionWorkerError> {
        let query = ContainerWorkExecutionQuery {
            project_id: request.project_id,
            environment_id: request.environment_id,
            plan_id: request.plan_id,
            plan_revision: request.plan_revision,
        };
        let mut observed = initial;
        let mut uncertain = uncertain_start;
        let mut abort_reason = None;
        let mut cancel_sent = false;
        let observation_deadline = add_duration(request.deadline_at, POLL_GRACE)?;
        let mut heartbeat = tokio::time::interval(
            self.lease_duration
                .checked_div(3)
                .unwrap_or(self.lease_duration)
                .max(Duration::from_millis(10)),
        );
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        heartbeat.tick().await;
        loop {
            if let Some(receipt) = observed.take() {
                validate_container_receipt(&request, &receipt)?;
                if receipt_terminal(receipt.state) {
                    self.finish_container(lease, receipt, abort_reason).await?;
                    return Ok(());
                }
                observed = Some(receipt);
            }
            let now = timestamp_now().map_err(|_| WorkExecutionWorkerError::Clock)?;
            let cancellation_requested = self
                .store
                .heartbeat_work_execution(&lease, self.lease_duration)
                .await?;
            if cancellation_requested && abort_reason.is_none() {
                abort_reason = Some(AbortReason::UserCancellation);
            }
            if now >= request.deadline_at && abort_reason.is_none() {
                abort_reason = Some(AbortReason::Deadline);
            }
            if abort_reason.is_some() && !cancel_sent {
                cancel_sent = true;
                if let Err(error) = self
                    .environment
                    .cancel_container(lease.run_id, &query)
                    .await
                    && !error.uncertain()
                    && !matches!(error, WorkExecutionClientError::NotFound)
                {
                    return self
                        .complete_failure(lease, Uuid::nil(), error.diagnostic_code())
                        .await;
                }
            }
            match self.environment.query_container(lease.run_id, &query).await {
                Ok(receipt) => {
                    uncertain = false;
                    validate_container_receipt(&request, &receipt)?;
                    if receipt_terminal(receipt.state) {
                        self.finish_container(lease, receipt, abort_reason).await?;
                        return Ok(());
                    }
                    observed = Some(receipt);
                }
                Err(error)
                    if error.uncertain()
                        || (uncertain && matches!(error, WorkExecutionClientError::NotFound)) =>
                {
                    uncertain = true;
                }
                Err(error) if matches!(error, WorkExecutionClientError::NotFound) && !uncertain => {
                    return self
                        .complete_failure(lease, Uuid::nil(), error.diagnostic_code())
                        .await;
                }
                Err(error) => {
                    return self
                        .complete_failure(lease, Uuid::nil(), error.diagnostic_code())
                        .await;
                }
            }
            if timestamp_now().map_err(|_| WorkExecutionWorkerError::Clock)? >= observation_deadline
            {
                let code = match abort_reason {
                    Some(AbortReason::UserCancellation) => "LW_AGENT_WORK_EXECUTION_CANCELLED",
                    Some(AbortReason::Deadline) => "LW_AGENT_WORK_EXECUTION_DEADLINE_EXCEEDED",
                    None => "LW_AGENT_WORK_EXECUTION_OBSERVATION_UNAVAILABLE",
                };
                self.complete_failure(lease, Uuid::nil(), code).await?;
                return Ok(());
            }
            tokio::select! {
                _ = heartbeat.tick() => {},
                () = tokio::time::sleep(self.poll_interval) => {},
            }
        }
    }

    async fn finish_container(
        &self,
        lease: WorkExecutionLease,
        receipt: ContainerWorkExecutionReceipt,
        abort_reason: Option<AbortReason>,
    ) -> Result<(), WorkExecutionWorkerError> {
        let (succeeded, code) = match abort_reason {
            Some(AbortReason::UserCancellation) => (false, "LW_AGENT_WORK_EXECUTION_CANCELLED"),
            Some(AbortReason::Deadline) => (false, "LW_AGENT_WORK_EXECUTION_DEADLINE_EXCEEDED"),
            None => match receipt.state {
                ContainerWorkExecutionState::Succeeded
                    if receipt.exit_code == Some(0)
                        && receipt.verification_exit_code.is_none_or(|code| code == 0) =>
                {
                    (true, "")
                }
                ContainerWorkExecutionState::Cancelled => {
                    (false, "LW_AGENT_WORK_EXECUTION_CANCELLED")
                }
                ContainerWorkExecutionState::CleanupFailed => {
                    (false, "LW_AGENT_WORK_EXECUTION_CLEANUP_FAILED")
                }
                _ => (
                    false,
                    receipt
                        .diagnostic_code
                        .as_ref()
                        .map_or("LW_AGENT_WORK_EXECUTION_FAILED", |code| code.as_str()),
                ),
            },
        };
        let value =
            serde_json::to_value(&receipt).map_err(|_| WorkExecutionWorkerError::InvalidState)?;
        self.store
            .complete_work_execution(
                &lease,
                value,
                succeeded,
                (!succeeded).then_some(code),
                timestamp_now().map_err(|_| WorkExecutionWorkerError::Clock)?,
                TRACE_ID,
            )
            .await?;
        Ok(())
    }

    async fn process_vm(
        &self,
        lease: WorkExecutionLease,
        request: VmWorkExecutionRequest,
        binding: ResolvedVmBinding,
    ) -> Result<(), WorkExecutionWorkerError> {
        request.validate()?;
        if binding.target != request.target {
            return self
                .complete_failure(
                    lease,
                    request.execution_id,
                    "LW_AGENT_WORK_EXECUTION_TARGET_IDENTITY_CHANGED",
                )
                .await;
        }
        if lease.fresh {
            if self
                .store
                .heartbeat_work_execution(&lease, self.lease_duration)
                .await?
            {
                return self
                    .complete_failure(
                        lease,
                        request.execution_id,
                        "LW_AGENT_WORK_EXECUTION_CANCELLED",
                    )
                    .await;
            }
            if let Err(error) = VmExecutionTransport::start(&binding, &request).await
                && !error.uncertain()
            {
                return self
                    .complete_failure(lease, request.execution_id, error.diagnostic_code())
                    .await;
            }
        }
        self.drive_vm(lease, request, binding).await
    }

    #[allow(
        clippy::too_many_lines,
        reason = "VM execution keeps refresh, cancellation, observation, and fencing in one loop"
    )]
    async fn drive_vm(
        &self,
        lease: WorkExecutionLease,
        request: VmWorkExecutionRequest,
        mut binding: ResolvedVmBinding,
    ) -> Result<(), WorkExecutionWorkerError> {
        let mut abort_reason = None;
        let mut cancel_sent = false;
        let observation_deadline = add_duration(request.deadline_at, POLL_GRACE)?;
        let mut heartbeat = tokio::time::interval(
            self.lease_duration
                .checked_div(3)
                .unwrap_or(self.lease_duration)
                .max(Duration::from_millis(10)),
        );
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        heartbeat.tick().await;
        loop {
            let now = timestamp_now().map_err(|_| WorkExecutionWorkerError::Clock)?;
            if binding.expires_within(now, VM_CREDENTIAL_REFRESH_SKEW)? {
                let refreshed = self
                    .environment
                    .resolve_work_binding_recovery(&request)
                    .await
                    .map_err(WorkExecutionWorkerError::from)?;
                if refreshed.target != request.target {
                    return self
                        .complete_failure(
                            lease,
                            request.execution_id,
                            "LW_AGENT_WORK_EXECUTION_TARGET_IDENTITY_CHANGED",
                        )
                        .await;
                }
                binding = refreshed;
            }
            if self
                .store
                .heartbeat_work_execution(&lease, self.lease_duration)
                .await?
                && abort_reason.is_none()
            {
                abort_reason = Some(AbortReason::UserCancellation);
            }
            if now >= request.deadline_at && abort_reason.is_none() {
                abort_reason = Some(AbortReason::Deadline);
            }
            if abort_reason.is_some() && !cancel_sent {
                cancel_sent = true;
                if let Err(error) = VmExecutionTransport::cancel(&binding, &request).await
                    && !error.uncertain()
                {
                    return self
                        .complete_failure(lease, request.execution_id, error.diagnostic_code())
                        .await;
                }
            }
            match VmExecutionTransport::observe(&binding, &request, abort_reason).await {
                Ok(Some(observation)) => {
                    if VmObservation::terminal() {
                        let (succeeded, code) = match abort_reason {
                            Some(AbortReason::UserCancellation) => {
                                (false, "LW_AGENT_WORK_EXECUTION_CANCELLED".to_owned())
                            }
                            Some(AbortReason::Deadline) => (
                                false,
                                "LW_AGENT_WORK_EXECUTION_DEADLINE_EXCEEDED".to_owned(),
                            ),
                            None if observation.state == VmObservationState::Succeeded
                                && observation.exit_code == Some(0)
                                && observation
                                    .verification_exit_code
                                    .is_none_or(|code| code == 0) =>
                            {
                                (true, String::new())
                            }
                            None if observation.state == VmObservationState::Cancelled => {
                                (false, "LW_AGENT_WORK_EXECUTION_CANCELLED".to_owned())
                            }
                            None if observation.state == VmObservationState::CleanupFailed => {
                                (false, "LW_AGENT_WORK_EXECUTION_CLEANUP_FAILED".to_owned())
                            }
                            None => (
                                false,
                                observation
                                    .diagnostic_code
                                    .clone()
                                    .unwrap_or_else(|| "LW_AGENT_WORK_EXECUTION_FAILED".to_owned()),
                            ),
                        };
                        let code = (!succeeded).then_some(code);
                        self.store
                            .complete_work_execution(
                                &lease,
                                observation.into_value(),
                                succeeded,
                                code.as_deref(),
                                timestamp_now().map_err(|_| WorkExecutionWorkerError::Clock)?,
                                TRACE_ID,
                            )
                            .await?;
                        return Ok(());
                    }
                }
                Ok(None) => {}
                Err(error) if error.uncertain() => {}
                Err(error) => {
                    return self
                        .complete_failure(lease, request.execution_id, error.diagnostic_code())
                        .await;
                }
            }
            if timestamp_now().map_err(|_| WorkExecutionWorkerError::Clock)? >= observation_deadline
            {
                let code = match abort_reason {
                    Some(AbortReason::UserCancellation) => "LW_AGENT_WORK_EXECUTION_CANCELLED",
                    Some(AbortReason::Deadline) => "LW_AGENT_WORK_EXECUTION_DEADLINE_EXCEEDED",
                    None => "LW_AGENT_WORK_EXECUTION_OBSERVATION_UNAVAILABLE",
                };
                return self
                    .complete_failure(lease, request.execution_id, code)
                    .await;
            }
            tokio::select! {
                _ = heartbeat.tick() => {},
                () = tokio::time::sleep(self.poll_interval) => {},
            }
        }
    }

    async fn complete_failure(
        &self,
        lease: WorkExecutionLease,
        execution_id: Uuid,
        diagnostic_code: &'static str,
    ) -> Result<(), WorkExecutionWorkerError> {
        let value = json!({
            "kind": "agent_work_execution_failure",
            "executionId": execution_id,
            "runId": lease.run_id,
            "attempt": lease.attempt,
            "diagnosticCode": diagnostic_code,
        });
        self.store
            .complete_work_execution(
                &lease,
                value,
                false,
                Some(diagnostic_code),
                timestamp_now().map_err(|_| WorkExecutionWorkerError::Clock)?,
                TRACE_ID,
            )
            .await?;
        Ok(())
    }
}

async fn run_worker_loop<F, Fut>(
    poll_interval: Duration,
    mut poll: F,
) -> Result<(), WorkExecutionWorkerError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<u32, WorkExecutionWorkerError>>,
{
    if poll_interval.is_zero() || poll_interval > Duration::from_mins(1) {
        return Err(WorkExecutionWorkerError::Configuration);
    }
    let mut ticker = tokio::time::interval(poll_interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        let processed = poll().await?;
        if processed == 0 {
            tracing::debug!(event = "agent.work_execution.worker_idle", outcome = "idle",);
        } else {
            tracing::info!(
                event = "agent.work_execution.worker_processed",
                outcome = "processed",
                processed,
            );
        }
    }
}

#[derive(Clone, Debug)]
enum PersistedWorkRequest {
    Container(ContainerWorkExecutionRequest),
    Vm(VmWorkExecutionRequest),
}

fn parse_persisted_request(
    value: &Value,
) -> Result<PersistedWorkRequest, WorkExecutionWorkerError> {
    if let Ok(request) = serde_json::from_value::<ContainerWorkExecutionRequest>(value.clone()) {
        return Ok(PersistedWorkRequest::Container(request));
    }
    serde_json::from_value::<VmWorkExecutionRequest>(value.clone())
        .map(PersistedWorkRequest::Vm)
        .map_err(|_| WorkExecutionWorkerError::InvalidState)
}

fn container_query_string(query: &ContainerWorkExecutionQuery) -> String {
    format!(
        "projectId={}&environmentId={}&planId={}&planRevision={}",
        query.project_id,
        query.environment_id,
        query.plan_id,
        query.plan_revision.get()
    )
}

fn work_identity(
    run: &AgentRun,
) -> Result<(EnvironmentId, Revision, ActorId), WorkExecutionWorkerError> {
    match run.purpose {
        AgentRunPurpose::WorkConfiguration {
            environment_id,
            environment_revision,
            actor_id,
            ..
        } => Ok((environment_id, environment_revision, actor_id)),
        AgentRunPurpose::Authoring { .. } => Err(WorkExecutionWorkerError::InvalidState),
    }
}

fn validate_container_receipt(
    request: &ContainerWorkExecutionRequest,
    receipt: &ContainerWorkExecutionReceipt,
) -> Result<(), WorkExecutionWorkerError> {
    receipt
        .validate()
        .map_err(|_| WorkExecutionWorkerError::InvalidState)?;
    if receipt.run_id != request.run_id
        || receipt.plan_id != request.plan_id
        || receipt.plan_revision != request.plan_revision
        || receipt.environment_id != request.environment_id
        || receipt.environment_revision != request.environment_revision
        || receipt.target_pod_uid.trim().is_empty()
    {
        return Err(WorkExecutionWorkerError::InvalidState);
    }
    if matches!(
        receipt.state,
        ContainerWorkExecutionState::Succeeded
            | ContainerWorkExecutionState::Failed
            | ContainerWorkExecutionState::Cancelled
            | ContainerWorkExecutionState::CleanupFailed
    ) && receipt.exit_code.is_none()
    {
        return Err(WorkExecutionWorkerError::InvalidState);
    }
    Ok(())
}

const fn receipt_terminal(state: ContainerWorkExecutionState) -> bool {
    matches!(
        state,
        ContainerWorkExecutionState::Succeeded
            | ContainerWorkExecutionState::Failed
            | ContainerWorkExecutionState::Cancelled
            | ContainerWorkExecutionState::CleanupFailed
    )
}

fn validate_vm_target(target: &VmExecutionTarget) -> Result<(), VmExecutionError> {
    let host = target
        .host
        .parse::<IpAddr>()
        .map_err(|_| VmExecutionError::TargetInvalid)?;
    if !private_ip(host)
        || target.namespace.trim().is_empty()
        || target.namespace.len() > 63
        || target.port != VM_SSH_PORT
        || target.username.trim().is_empty()
        || target.username.len() > 64
        || !target
            .username
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        || !safe_absolute_path(&target.workspace_root)
        || target
            .expected_host_key_sha256
            .parse::<persistence_sqlx::Sha256Digest>()
            .is_err()
        || target
            .source_identity
            .parse::<persistence_sqlx::Sha256Digest>()
            .is_err()
    {
        return Err(VmExecutionError::TargetInvalid);
    }
    Ok(())
}

fn validate_execution_certificate(
    encoded: &str,
    key: &PrivateKey,
    expires_at: UtcTimestamp,
    now: UtcTimestamp,
) -> Result<(), WorkExecutionClientError> {
    if encoded.is_empty() || encoded.len() > 16_384 || encoded.chars().any(char::is_control) {
        return Err(WorkExecutionClientError::ResponseInvalid);
    }
    let certificate = SshCertificate::from_openssh(encoded)
        .map_err(|_| WorkExecutionClientError::ResponseInvalid)?;
    let now_seconds = u64::try_from(now.get().unix_timestamp())
        .map_err(|_| WorkExecutionClientError::ResponseInvalid)?;
    let expiry_seconds = u64::try_from(expires_at.get().unix_timestamp())
        .map_err(|_| WorkExecutionClientError::ResponseInvalid)?;
    if certificate.cert_type() != russh::keys::ssh_key::certificate::CertType::User
        || certificate.valid_after() > now_seconds
        || certificate.valid_before() <= now_seconds
        || certificate.valid_before() > expiry_seconds
        || certificate.public_key() != key.public_key().key_data()
        || certificate.valid_principals().len() != 1
        || certificate.valid_principals().first().map(String::as_str) != Some(VM_AGENT_PRINCIPAL)
        || !certificate.critical_options().is_empty()
    {
        return Err(WorkExecutionClientError::ResponseInvalid);
    }
    Ok(())
}

struct VmExecutionTransport;

impl VmExecutionTransport {
    async fn start(
        binding: &ResolvedVmBinding,
        request: &VmWorkExecutionRequest,
    ) -> Result<(), VmExecutionError> {
        let (session, sftp) = connect_sftp(binding).await?;
        let directory = vm_execution_directory(request.execution_id);
        ensure_directory(&sftp, &directory).await?;
        upload_file(&sftp, &format!("{directory}/runner.sh"), VM_RUNNER).await?;
        upload_file(
            &sftp,
            &format!("{directory}/primary.sh"),
            request.script_content.as_bytes(),
        )
        .await?;
        if let Some(script) = &request.verification_script_content {
            upload_file(
                &sftp,
                &format!("{directory}/verification.sh"),
                script.as_bytes(),
            )
            .await?;
        }
        let deadline = deadline_epoch_seconds(request.deadline_at)?;
        let verification = if request.verification_script_content.is_some() {
            "1"
        } else {
            "0"
        };
        let command = format!(
            "/bin/sh -c 'setsid /bin/sh \"$1\" \"$2\" \"$3\" \"$4\" \"$5\" </dev/null >\"$2/agent.log\" 2>&1 &' labweaver-start {runner} {directory} {workdir} {verification} {deadline}",
            runner = shell_quote(&format!("{directory}/runner.sh")),
            directory = shell_quote(&directory),
            workdir = shell_quote(&binding.target.workspace_root),
        );
        exec_command(&session, &command).await
    }

    async fn observe(
        binding: &ResolvedVmBinding,
        request: &VmWorkExecutionRequest,
        abort_reason: Option<AbortReason>,
    ) -> Result<Option<VmObservation>, VmExecutionError> {
        let (session, _) = connect_sftp(binding).await?;
        let directory = vm_execution_directory(request.execution_id);
        let command = format!(
            "/bin/sh {} --observe {} {}",
            shell_quote(&format!("{directory}/runner.sh")),
            shell_quote(&directory),
            if request.verification_script_content.is_some() {
                "1"
            } else {
                "0"
            },
        );
        let (status, stdout, stderr) = exec_capture(&session, &command).await?;
        match status {
            3 => return Ok(None),
            0 => {}
            _ => {
                tracing::warn!(
                    event = "agent.work_execution.vm_runner_observe_failed",
                    execution_id = %request.execution_id,
                    remote_status = status,
                    stderr_sha256 = %persistence_sqlx::Sha256Digest::of_bytes(&stderr),
                );
                return Err(vm_runner_error(&stderr));
            }
        }
        let (primary_exit, verification_exit, output_truncated, output) =
            parse_runner_observation(&stdout)?;
        let state = if abort_reason == Some(AbortReason::UserCancellation) {
            VmObservationState::Cancelled
        } else if abort_reason == Some(AbortReason::Deadline) {
            VmObservationState::Failed
        } else if primary_exit == 0 && verification_exit.is_none_or(|code| code == 0) {
            VmObservationState::Succeeded
        } else {
            VmObservationState::Failed
        };
        Ok(Some(VmObservation {
            execution_id: request.execution_id,
            run_id: request.run_id,
            plan_id: request.plan_id,
            plan_revision: request.plan_revision,
            environment_id: request.environment_id,
            environment_revision: request.environment_revision,
            source_identity: request.target.source_identity.clone(),
            state,
            exit_code: Some(primary_exit),
            verification_exit_code: verification_exit,
            output,
            output_truncated,
            diagnostic_code: None,
        }))
    }

    async fn cancel(
        binding: &ResolvedVmBinding,
        request: &VmWorkExecutionRequest,
    ) -> Result<(), VmExecutionError> {
        let (session, _) = connect_sftp(binding).await?;
        let directory = vm_execution_directory(request.execution_id);
        let command = format!(
            "/bin/sh -c {} labweaver-cancel {}",
            shell_quote(
                "set -eu; mkdir -p -- \"$1\"; : > \"$1/cancel\"; if [ -x \"$1/runner.sh\" ]; then /bin/sh \"$1/runner.sh\" --cancel \"$1\"; fi",
            ),
            shell_quote(&directory),
        );
        let (status, _stdout, stderr) = exec_capture(&session, &command).await?;
        if status != 0 {
            tracing::warn!(
                event = "agent.work_execution.vm_runner_cancel_failed",
                execution_id = %request.execution_id,
                remote_status = status,
                stderr_sha256 = %persistence_sqlx::Sha256Digest::of_bytes(&stderr),
            );
            return Err(vm_runner_error(&stderr));
        }
        Ok(())
    }
}

async fn connect_sftp(
    binding: &ResolvedVmBinding,
) -> Result<(client::Handle<HostKeyVerifier>, SftpSession), VmExecutionError> {
    let now = timestamp_now().map_err(|_| VmExecutionError::Clock)?;
    if binding.expires_at <= now {
        return Err(VmExecutionError::CredentialExpired);
    }
    let host = binding
        .target
        .host
        .parse::<IpAddr>()
        .map_err(|_| VmExecutionError::TargetInvalid)?;
    let expected = binding
        .target
        .expected_host_key_sha256
        .parse::<persistence_sqlx::Sha256Digest>()
        .map_err(|_| VmExecutionError::TargetInvalid)?;
    let config = Arc::new(client::Config {
        preferred: russh::Preferred::default(),
        inactivity_timeout: Some(Duration::from_secs(30)),
        ..client::Config::default()
    });
    let session = tokio::time::timeout(
        Duration::from_secs(30),
        client::connect(config, (host, VM_SSH_PORT), HostKeyVerifier { expected }),
    )
    .await
    .map_err(|_| VmExecutionError::Timeout)?
    .map_err(|error| match error {
        russh::Error::UnknownKey => VmExecutionError::HostKeyMismatch,
        _ => VmExecutionError::Transport,
    })?;
    let private_key = binding.private_key()?;
    let certificate = binding.certificate()?;
    let mut session = session;
    let authenticated = tokio::time::timeout(
        Duration::from_secs(30),
        session.authenticate_openssh_cert(
            binding.target.username.clone(),
            Arc::new(private_key),
            certificate,
        ),
    )
    .await
    .map_err(|_| VmExecutionError::Timeout)?
    .map_err(|_| VmExecutionError::Transport)?;
    if !authenticated.success() {
        return Err(VmExecutionError::CredentialInvalid);
    }
    let channel = tokio::time::timeout(Duration::from_secs(30), session.channel_open_session())
        .await
        .map_err(|_| VmExecutionError::Timeout)?
        .map_err(|_| VmExecutionError::Transport)?;
    tokio::time::timeout(
        Duration::from_secs(30),
        channel.request_subsystem(true, "sftp"),
    )
    .await
    .map_err(|_| VmExecutionError::Timeout)?
    .map_err(|_| VmExecutionError::Transport)?;
    let sftp = tokio::time::timeout(
        Duration::from_secs(30),
        SftpSession::new(channel.into_stream()),
    )
    .await
    .map_err(|_| VmExecutionError::Timeout)?
    .map_err(|_| VmExecutionError::Transport)?;
    Ok((session, sftp))
}

async fn ensure_directory(sftp: &SftpSession, directory: &str) -> Result<(), VmExecutionError> {
    if exists(sftp, directory).await? {
        return Ok(());
    }
    sftp.create_dir(directory)
        .await
        .map_err(|_| VmExecutionError::Transport)
}

async fn upload_file(
    sftp: &SftpSession,
    path: &str,
    contents: &[u8],
) -> Result<(), VmExecutionError> {
    if contents.is_empty()
        || contents.len() > ContainerWorkExecutionRequest::MAX_SCRIPT_BYTES + VM_RUNNER.len()
    {
        return Err(VmExecutionError::RequestInvalid);
    }
    let mut file = sftp
        .create(path)
        .await
        .map_err(|_| VmExecutionError::Transport)?;
    file.write_all(contents)
        .await
        .map_err(|_| VmExecutionError::Transport)?;
    file.shutdown()
        .await
        .map_err(|_| VmExecutionError::Transport)
}

async fn exec_command(
    session: &client::Handle<HostKeyVerifier>,
    command: &str,
) -> Result<(), VmExecutionError> {
    let mut channel = session
        .channel_open_session()
        .await
        .map_err(|_| VmExecutionError::Transport)?;
    channel
        .exec(true, command)
        .await
        .map_err(|_| VmExecutionError::Transport)?;
    let mut exit_status = None;
    while let Some(message) = channel.wait().await {
        match message {
            ChannelMsg::ExitStatus {
                exit_status: status,
            } => exit_status = Some(status),
            ChannelMsg::Close => break,
            _ => {}
        }
    }
    match exit_status {
        Some(0) => Ok(()),
        Some(_) => Err(VmExecutionError::RemoteCommandFailed),
        None => Err(VmExecutionError::Transport),
    }
}

async fn exec_capture(
    session: &client::Handle<HostKeyVerifier>,
    command: &str,
) -> Result<(u32, Vec<u8>, Vec<u8>), VmExecutionError> {
    let mut channel = session
        .channel_open_session()
        .await
        .map_err(|_| VmExecutionError::Transport)?;
    channel
        .exec(true, command)
        .await
        .map_err(|_| VmExecutionError::Transport)?;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut exit_status = None;
    while let Some(message) = channel.wait().await {
        match message {
            ChannelMsg::Data { data } => {
                append_bounded(&mut stdout, &data)?;
            }
            ChannelMsg::ExtendedData { data, ext: 1 } => {
                append_bounded(&mut stderr, &data)?;
            }
            ChannelMsg::ExitStatus {
                exit_status: status,
            } => exit_status = Some(status),
            ChannelMsg::Close => break,
            _ => {}
        }
    }
    Ok((
        exit_status.ok_or(VmExecutionError::Transport)?,
        stdout,
        stderr,
    ))
}

fn append_bounded(buffer: &mut Vec<u8>, data: &[u8]) -> Result<(), VmExecutionError> {
    let next = buffer
        .len()
        .checked_add(data.len())
        .ok_or(VmExecutionError::ObservationInvalid)?;
    if next > MAX_SSH_FILE_BYTES {
        return Err(VmExecutionError::ObservationInvalid);
    }
    buffer.extend_from_slice(data);
    Ok(())
}

fn parse_runner_observation(
    stdout: &[u8],
) -> Result<(i32, Option<i32>, bool, String), VmExecutionError> {
    let mut lines = stdout.splitn(4, |byte| *byte == b'\n');
    let primary = parse_runner_exit(lines.next().ok_or(VmExecutionError::ObservationInvalid)?)?;
    let verification = lines.next().ok_or(VmExecutionError::ObservationInvalid)?;
    let verification = if verification == b"-" {
        None
    } else {
        Some(parse_runner_exit(verification)?)
    };
    let output_truncated = match lines.next().ok_or(VmExecutionError::ObservationInvalid)? {
        b"0" => false,
        b"1" => true,
        _ => return Err(VmExecutionError::ObservationInvalid),
    };
    let output = String::from_utf8(lines.next().unwrap_or_default().to_vec())
        .map_err(|_| VmExecutionError::ObservationInvalid)?;
    Ok((
        primary,
        verification,
        output_truncated,
        bound_output(&output),
    ))
}

fn parse_runner_exit(value: &[u8]) -> Result<i32, VmExecutionError> {
    std::str::from_utf8(value)
        .map_err(|_| VmExecutionError::ObservationInvalid)?
        .trim()
        .parse::<i32>()
        .map_err(|_| VmExecutionError::ObservationInvalid)
}

fn vm_runner_error(stderr: &[u8]) -> VmExecutionError {
    if stderr.contains(&b'\n') {
        let text = String::from_utf8_lossy(stderr);
        if text.contains("LW_ENVIRONMENT_WORK_EXECUTION_TARGET_IDENTITY_CHANGED") {
            return VmExecutionError::TargetInvalid;
        }
    }
    VmExecutionError::ObservationInvalid
}

async fn exists(sftp: &SftpSession, path: &str) -> Result<bool, VmExecutionError> {
    match sftp.try_exists(path).await {
        Ok(value) => Ok(value),
        Err(SftpError::Status(status)) if status.status_code == SftpStatusCode::NoSuchFile => {
            Ok(false)
        }
        Err(_) => Err(VmExecutionError::Transport),
    }
}

fn vm_execution_directory(execution_id: Uuid) -> String {
    format!("{VM_EXECUTION_ROOT}/{execution_id}")
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn safe_absolute_path(value: &str) -> bool {
    value.starts_with('/')
        && value.len() <= 1_024
        && value != "/"
        && !value.ends_with('/')
        && !value.contains("//")
        && !value.split('/').any(|part| part == "." || part == "..")
        && !value.chars().any(char::is_control)
}

fn private_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(value) => value.is_private() || value.is_loopback() || value.is_link_local(),
        IpAddr::V6(value) => {
            value.is_unique_local() || value.is_loopback() || value.is_unicast_link_local()
        }
    }
}

fn bound_output(value: &str) -> String {
    let max_bytes = ContainerWorkExecutionReceipt::MAX_OUTPUT_BYTES;
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

fn deadline_epoch_seconds(deadline: UtcTimestamp) -> Result<u64, VmExecutionError> {
    let value = deadline.get();
    let seconds = value
        .unix_timestamp()
        .checked_add(i64::from(value.nanosecond() != 0))
        .ok_or(VmExecutionError::DeadlineExceeded)?;
    u64::try_from(seconds).map_err(|_| VmExecutionError::DeadlineExceeded)
}

fn timestamp_now() -> Result<UtcTimestamp, WorkExecutionWorkerError> {
    let value = time::OffsetDateTime::now_utc();
    let value = value
        .replace_nanosecond((value.nanosecond() / 1_000_000) * 1_000_000)
        .map_err(|_| WorkExecutionWorkerError::Clock)?;
    UtcTimestamp::from_utc(value).map_err(|_| WorkExecutionWorkerError::Clock)
}

fn add_duration(
    timestamp: UtcTimestamp,
    duration: Duration,
) -> Result<UtcTimestamp, WorkExecutionWorkerError> {
    let milliseconds =
        i64::try_from(duration.as_millis()).map_err(|_| WorkExecutionWorkerError::Configuration)?;
    let value = timestamp
        .get()
        .checked_add(time::Duration::milliseconds(milliseconds))
        .ok_or(WorkExecutionWorkerError::Configuration)?;
    UtcTimestamp::from_utc(value).map_err(|_| WorkExecutionWorkerError::Clock)
}

fn read_bounded_file(path: &Path, max_bytes: u64) -> Result<Vec<u8>, WorkExecutionClientError> {
    if !path.is_absolute() {
        return Err(WorkExecutionClientError::Configuration);
    }
    let metadata = std::fs::metadata(path).map_err(|_| WorkExecutionClientError::Configuration)?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > max_bytes {
        return Err(WorkExecutionClientError::Configuration);
    }
    std::fs::read(path).map_err(|_| WorkExecutionClientError::Configuration)
}

async fn read_bounded_response(
    response: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>, WorkExecutionClientError> {
    if response
        .content_length()
        .is_some_and(|length| usize::try_from(length).map_or(true, |length| length > max_bytes))
    {
        return Err(WorkExecutionClientError::ResponseTooLarge);
    }
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| WorkExecutionClientError::Transport)?;
        let next = body
            .len()
            .checked_add(chunk.len())
            .ok_or(WorkExecutionClientError::ResponseTooLarge)?;
        if next > max_bytes {
            return Err(WorkExecutionClientError::ResponseTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EnvironmentErrorResponse {
    diagnostic_code: String,
}

const PRE_ADMISSION_UNAVAILABLE_DIAGNOSTIC: &str = "LW_ENVIRONMENT_WORK_ADMISSION_UNAVAILABLE";

fn classify_server_error(status: StatusCode, body: &[u8]) -> WorkExecutionClientError {
    if status == StatusCode::SERVICE_UNAVAILABLE
        && contracts::parse_strict_json::<EnvironmentErrorResponse>(body)
            .ok()
            .is_some_and(|error| error.diagnostic_code == PRE_ADMISSION_UNAVAILABLE_DIAGNOSTIC)
    {
        return WorkExecutionClientError::PreAdmissionUnavailable;
    }
    WorkExecutionClientError::UnavailableWithStatus {
        status: status.as_u16(),
    }
}

#[derive(Clone, Copy, Debug)]
struct HostKeyVerifier {
    expected: persistence_sqlx::Sha256Digest,
}

impl client::Handler for HostKeyVerifier {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        public_key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        let fingerprint = public_key
            .fingerprint(russh::keys::HashAlg::Sha256)
            .to_string();
        Ok(persistence_sqlx::Sha256Digest::of_bytes(fingerprint.as_bytes()) == self.expected)
    }
}

/// Failures at the Agent-to-Environment HTTP boundary.
#[derive(Debug, Error)]
pub enum WorkExecutionClientError {
    #[error("LW_AGENT_WORK_EXECUTION_CONFIG_INVALID")]
    Configuration,
    #[error("LW_AGENT_WORK_EXECUTION_TOKEN_FAILED")]
    Token(#[source] ServiceTokenClientError),
    #[error("LW_AGENT_WORK_EXECUTION_TRANSPORT_FAILED")]
    Transport,
    #[error("LW_AGENT_WORK_EXECUTION_REQUEST_INVALID")]
    RequestInvalid,
    #[error("LW_AGENT_WORK_EXECUTION_REQUEST_TOO_LARGE")]
    RequestTooLarge,
    #[error("LW_AGENT_WORK_EXECUTION_RESPONSE_TOO_LARGE")]
    ResponseTooLarge,
    #[error("LW_AGENT_WORK_EXECUTION_RESPONSE_INVALID")]
    ResponseInvalid,
    #[error("LW_AGENT_WORK_EXECUTION_NOT_FOUND")]
    NotFound,
    #[error("LW_AGENT_WORK_EXECUTION_DENIED")]
    Denied,
    #[error("LW_AGENT_WORK_EXECUTION_CONFLICT")]
    Conflict,
    #[error("LW_AGENT_WORK_EXECUTION_NOT_ELIGIBLE")]
    NotEligible,
    #[error("LW_AGENT_WORK_EXECUTION_UNAVAILABLE")]
    UnavailableWithStatus { status: u16 },
    #[error("LW_ENVIRONMENT_WORK_ADMISSION_UNAVAILABLE")]
    PreAdmissionUnavailable,
    #[error("LW_AGENT_WORK_EXECUTION_REJECTED")]
    Rejected,
    #[error("LW_AGENT_WORK_EXECUTION_CREDENTIAL_GENERATION_FAILED")]
    CredentialGeneration,
    #[error("LW_AGENT_WORK_EXECUTION_CLOCK_INVALID")]
    Clock,
}

impl WorkExecutionClientError {
    fn uncertain(&self) -> bool {
        matches!(
            self,
            Self::Transport | Self::UnavailableWithStatus { .. } | Self::ResponseTooLarge
        )
    }

    fn http_status(&self) -> Option<u16> {
        match self {
            Self::UnavailableWithStatus { status } => Some(*status),
            Self::PreAdmissionUnavailable => Some(StatusCode::SERVICE_UNAVAILABLE.as_u16()),
            Self::NotFound => Some(StatusCode::NOT_FOUND.as_u16()),
            Self::NotEligible => Some(StatusCode::UNPROCESSABLE_ENTITY.as_u16()),
            _ => None,
        }
    }

    fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::NotEligible => "LW_AGENT_WORK_EXECUTION_ENVIRONMENT_NOT_ELIGIBLE",
            Self::Denied => "LW_AGENT_WORK_EXECUTION_DENIED",
            Self::Conflict => "LW_AGENT_WORK_EXECUTION_CONFLICT",
            Self::NotFound => "LW_AGENT_WORK_EXECUTION_NOT_FOUND",
            Self::CredentialGeneration => "LW_AGENT_WORK_EXECUTION_CREDENTIAL_FAILED",
            Self::ResponseInvalid | Self::ResponseTooLarge => {
                "LW_AGENT_WORK_EXECUTION_RECEIPT_INVALID"
            }
            Self::Transport | Self::UnavailableWithStatus { .. } | Self::Token(_) => {
                "LW_AGENT_WORK_EXECUTION_TRANSPORT_FAILED"
            }
            Self::PreAdmissionUnavailable => PRE_ADMISSION_UNAVAILABLE_DIAGNOSTIC,
            Self::RequestInvalid
            | Self::RequestTooLarge
            | Self::Configuration
            | Self::Clock
            | Self::Rejected => "LW_AGENT_WORK_EXECUTION_REQUEST_INVALID",
        }
    }
}

/// Failures while speaking the VM's restricted execution protocol.
#[derive(Debug, Error)]
pub enum VmExecutionError {
    #[error("LW_AGENT_WORK_EXECUTION_VM_TARGET_INVALID")]
    TargetInvalid,
    #[error("LW_AGENT_WORK_EXECUTION_VM_REQUEST_INVALID")]
    RequestInvalid,
    #[error("LW_AGENT_WORK_EXECUTION_VM_CREDENTIAL_INVALID")]
    CredentialInvalid,
    #[error("LW_AGENT_WORK_EXECUTION_VM_CREDENTIAL_EXPIRED")]
    CredentialExpired,
    #[error("LW_AGENT_WORK_EXECUTION_VM_CLOCK_INVALID")]
    Clock,
    #[error("LW_AGENT_WORK_EXECUTION_VM_HOST_KEY_MISMATCH")]
    HostKeyMismatch,
    #[error("LW_AGENT_WORK_EXECUTION_VM_TIMEOUT")]
    Timeout,
    #[error("LW_AGENT_WORK_EXECUTION_VM_TRANSPORT_FAILED")]
    Transport,
    #[error("LW_AGENT_WORK_EXECUTION_VM_REMOTE_COMMAND_FAILED")]
    RemoteCommandFailed,
    #[error("LW_AGENT_WORK_EXECUTION_VM_OBSERVATION_INVALID")]
    ObservationInvalid,
    #[error("LW_AGENT_WORK_EXECUTION_VM_DEADLINE_EXCEEDED")]
    DeadlineExceeded,
}

impl VmExecutionError {
    fn uncertain(&self) -> bool {
        matches!(self, Self::Transport | Self::Timeout)
    }

    fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::TargetInvalid => "LW_AGENT_WORK_EXECUTION_VM_TARGET_INVALID",
            Self::RequestInvalid => "LW_AGENT_WORK_EXECUTION_REQUEST_INVALID",
            Self::CredentialInvalid | Self::CredentialExpired => {
                "LW_AGENT_WORK_EXECUTION_VM_CREDENTIAL_INVALID"
            }
            Self::HostKeyMismatch => "LW_AGENT_WORK_EXECUTION_VM_HOST_KEY_MISMATCH",
            Self::Timeout | Self::Transport => "LW_AGENT_WORK_EXECUTION_VM_TRANSPORT_FAILED",
            Self::RemoteCommandFailed => "LW_AGENT_WORK_EXECUTION_VM_REMOTE_COMMAND_FAILED",
            Self::ObservationInvalid => "LW_AGENT_WORK_EXECUTION_VM_OBSERVATION_INVALID",
            Self::DeadlineExceeded => "LW_AGENT_WORK_EXECUTION_DEADLINE_EXCEEDED",
            Self::Clock => "LW_AGENT_WORK_EXECUTION_VM_CLOCK_INVALID",
        }
    }
}

/// Worker-level failures. Retryable errors leave the durable execution lease for recovery.
#[derive(Debug, Error)]
pub enum WorkExecutionWorkerError {
    #[error("LW_AGENT_WORK_EXECUTION_CONFIGURATION_INVALID")]
    Configuration,
    #[error("LW_AGENT_WORK_EXECUTION_CLOCK_INVALID")]
    Clock,
    #[error("LW_AGENT_WORK_EXECUTION_INVALID_STATE")]
    InvalidState,
    #[error(transparent)]
    Store(#[from] AgentRunStoreError),
    #[error(transparent)]
    GeneratedArtifact(#[from] GeneratedArtifactStoreError),
    #[error(transparent)]
    Client(#[from] WorkExecutionClientError),
    #[error(transparent)]
    Vm(#[from] VmExecutionError),
}

impl WorkExecutionWorkerError {
    fn retryable(&self) -> bool {
        matches!(
            self,
            Self::Client(
                WorkExecutionClientError::Transport
                    | WorkExecutionClientError::UnavailableWithStatus { .. }
            ) | Self::Vm(VmExecutionError::Transport | VmExecutionError::Timeout)
                | Self::Store(AgentRunStoreError::PersistenceFailed)
        )
    }
}

#[cfg(test)]
mod composition_tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::Duration;

    use reqwest::StatusCode;

    use super::{
        PRE_ADMISSION_UNAVAILABLE_DIAGNOSTIC, WorkExecutionClientError, WorkExecutionWorkerError,
        classify_server_error, run_worker_loop,
    };

    #[tokio::test]
    async fn worker_loop_invokes_the_poller_and_propagates_fatal_failures() {
        let calls = Arc::new(AtomicUsize::new(0));
        let observed_calls = Arc::clone(&calls);
        let result = run_worker_loop(Duration::from_millis(1), move || {
            observed_calls.fetch_add(1, Ordering::Relaxed);
            async { Err(WorkExecutionWorkerError::InvalidState) }
        })
        .await;

        assert!(matches!(
            result,
            Err(WorkExecutionWorkerError::InvalidState)
        ));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn worker_loop_rejects_an_invalid_cadence_before_polling() {
        let result = run_worker_loop(Duration::ZERO, || async { Ok(0) }).await;

        assert!(matches!(
            result,
            Err(WorkExecutionWorkerError::Configuration)
        ));
    }

    #[test]
    fn trusted_pre_admission_unavailable_is_terminal_and_keeps_its_diagnostic() {
        let error = classify_server_error(
            StatusCode::SERVICE_UNAVAILABLE,
            format!(r#"{{"diagnosticCode":"{PRE_ADMISSION_UNAVAILABLE_DIAGNOSTIC}"}}"#).as_bytes(),
        );

        assert!(matches!(
            &error,
            WorkExecutionClientError::PreAdmissionUnavailable
        ));
        assert!(!error.uncertain());
        assert_eq!(
            error.diagnostic_code(),
            PRE_ADMISSION_UNAVAILABLE_DIAGNOSTIC
        );
    }

    #[test]
    fn unclassified_server_errors_remain_uncertain() {
        for (status, body) in [
            (
                StatusCode::SERVICE_UNAVAILABLE,
                br#"{"diagnosticCode":"LW_ENVIRONMENT_WORK_EXECUTION_BACKEND_FAILED"}"#.as_slice(),
            ),
            (
                StatusCode::BAD_GATEWAY,
                br#"{"diagnosticCode":"LW_ENVIRONMENT_WORK_ADMISSION_UNAVAILABLE"}"#.as_slice(),
            ),
            (
                StatusCode::SERVICE_UNAVAILABLE,
                br#"{"diagnosticCode":"LW_ENVIRONMENT_WORK_ADMISSION_UNAVAILABLE","extra":true}"#
                    .as_slice(),
            ),
        ] {
            let error = classify_server_error(status, body);
            assert!(matches!(
                &error,
                WorkExecutionClientError::UnavailableWithStatus { status: value }
                    if *value == status.as_u16()
            ));
            assert!(error.uncertain());
            assert_eq!(error.http_status(), Some(status.as_u16()));
        }
    }
}
