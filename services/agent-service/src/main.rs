//! Production Agent Service internal API and recoverable worker process.

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use agent_service::api::{AgentApiState, router, with_service_auth};
use agent_service::build_executor::{ProductionBuildExecutor, ProductionBuildExecutorConfig};
use agent_service::build_pipeline::{BuildPipeline, BuildPipelinePolicy};
use agent_service::build_provider::NatsBuildSupplyChainProvider;
use agent_service::build_provider::{
    FencedBuildExecutor, NatsBuildExecutorServer, PgBuildExecutorFenceStore,
};
use agent_service::build_store::{BuildWorker, PgBuildStore};
use agent_service::candidate_materializer::S3EnvironmentCandidateMaterializer;
use agent_service::classifier::DeterministicEgressClassifier;
use agent_service::claude_code::{
    ClaudeCodeProcess, EgressClassifier, PackageObjectReadError, ProblemPackageEgressGate,
    ProblemPackageReader, RunCancellation, TokioClaudeCodeProcess,
};
use agent_service::generated_artifacts::GeneratedArtifactStore;
use agent_service::llm_review::{LlmReviewStore, LlmReviewWorker};
use agent_service::messaging::{
    AgentBuildCommandConsumer, AgentOutboxDispatcher, connect_nats_mtls,
};
use agent_service::run_store::{AgentRunService, PostgresAgentRunStore};
use agent_service::work_execution::{
    WorkExecutionClient, WorkExecutionConfiguration, WorkExecutionWorker,
};
use artifact_store::{ImmutableObjectStore, S3Credential, S3ImmutableObjectStore, S3StoreConfig};
use async_trait::async_trait;
use auth::{
    ServerTlsFileConfig, ServiceAuthConfig, ServiceAuthError, ServiceTokenClient,
    ServiceTokenClientConfig, ServiceTokenVerifier, TransportSecurityMode,
};
use contracts::{ArtifactId, ArtifactRef, Revision, UtcTimestamp};
use serde::Deserialize;
use sqlx::postgres::PgPoolOptions;
use time::OffsetDateTime;

#[path = "../../http_transport.rs"]
mod http_transport;
#[path = "../../service_runtime.rs"]
mod service_runtime;

/// The exact provider environment the env-cleared Claude Code worker receives.
///
/// Issue #152 generalizes the provider binding to the three standard Anthropic
/// fields. The set is closed: operator-specific names such as `ECNU_API_KEY`
/// are not read, and no compatibility alias, ambient credential, or fallback
/// provider route may enter this map.
const REQUIRED_WORKER_ENVIRONMENT: [&str; 3] = [
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_MODEL",
];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeploymentFile {
    database_url_file: String,
    database_max_connections: u32,
    control_tls: ServerTlsFileConfig,
    object_store: S3StoreConfig,
    object_store_access_key_file: String,
    object_store_secret_key_file: String,
    object_store_session_token_file: Option<String>,
    classifier_binding: String,
    classifier_revision: Revision,
    worker_id: String,
    dispatch_lease_seconds: u64,
    track_lease_seconds: u64,
    poll_interval_milliseconds: u64,
    worker_environment_files: BTreeMap<String, String>,
    work_execution: WorkExecutionFileConfig,
    build: BuildFileConfig,
    nats: NatsFileConfig,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(
    clippy::struct_field_names,
    reason = "the file contract keeps the Environment-owned fields explicit"
)]
struct WorkExecutionFileConfig {
    /// Environment Service URL used by the durable Work execution consumer.
    environment_base_uri: String,
    /// Mounted CA bundle for the Environment Service mTLS endpoint.
    environment_ca_file: String,
    /// Audience expected on tokens sent to the Environment Service.
    environment_audience: String,
    /// Environment Service permissions requested by the Agent service account.
    environment_scopes: BTreeSet<String>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BuildFileConfig {
    provider_subject: String,
    builder_binding: String,
    registry_binding: String,
    policy_id: String,
    policy_revision: u32,
    registry_robot_name: String,
    stage_timeout_milliseconds: u64,
    worker_lease_seconds: u64,
    retry_delay_milliseconds: u64,
    max_attempts: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NatsFileConfig {
    server: String,
    ca_file: String,
    client_certificate_file: String,
    client_private_key_file: String,
    credentials_file: String,
    build_command_stream_name: String,
    build_command_consumer_name: String,
    build_command_quarantine_subject: String,
    publish_timeout_milliseconds: u64,
    outbox_poll_milliseconds: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BuildExecutorDeploymentFile {
    database_url_file: String,
    database_max_connections: u32,
    object_store: S3StoreConfig,
    object_store_access_key_file: String,
    object_store_secret_key_file: String,
    object_store_session_token_file: Option<String>,
    nats: BuildExecutorNatsFileConfig,
    executor: ProductionBuildExecutorConfig,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BuildExecutorNatsFileConfig {
    server: String,
    ca_file: String,
    client_certificate_file: String,
    client_private_key_file: String,
    credentials_file: String,
    request_subject: String,
}

#[tokio::main]
async fn main() -> Result<(), StartupError> {
    let mut arguments = std::env::args().skip(1);
    if arguments.next().as_deref() != Some("--mode") || arguments.size_hint().0 != 1 {
        return Err(StartupError::Configuration);
    }
    match arguments.next().as_deref() {
        Some("agent-service") => Box::pin(run_agent_service()).await,
        Some("build-executor") => Box::pin(run_build_executor()).await,
        _ => Err(StartupError::Configuration),
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "startup keeps every fail-closed Agent dependency binding in one auditable boundary"
)]
async fn run_agent_service() -> Result<(), StartupError> {
    telemetry::init(env!("CARGO_PKG_NAME"))?;
    let deployment = load_deployment()?;
    validate_deployment(&deployment)?;
    let tls = http_transport::load_server_config(
        &deployment.control_tls.server_certificate_file,
        &deployment.control_tls.server_key_file,
    )?;
    let (service_verifier, service_token_client) = discover_service_auth().await?;
    let work_execution_configuration = WorkExecutionConfiguration::defaults(
        reqwest::Url::parse(&deployment.work_execution.environment_base_uri)
            .map_err(|_| StartupError::Configuration)?,
        deployment.work_execution.environment_ca_file.clone().into(),
        deployment.work_execution.environment_audience.clone(),
        deployment.work_execution.environment_scopes.clone(),
    );
    let environment = WorkExecutionClient::from_configuration(
        &work_execution_configuration,
        service_token_client,
    )
    .map_err(StartupError::WorkExecutionClient)?;
    let pool = PgPoolOptions::new()
        .max_connections(deployment.database_max_connections)
        .connect(&read_trimmed(&deployment.database_url_file)?)
        .await?;
    verify_schema(&pool).await?;
    let store = PostgresAgentRunStore::new(pool.clone());
    let generated_artifacts = GeneratedArtifactStore::new(pool.clone());
    let llm_reviews = LlmReviewStore::new(pool.clone());
    let build_store = PgBuildStore::new(pool);
    let nats = connect_nats_mtls(
        &deployment.nats.server,
        deployment.nats.ca_file.clone().into(),
        deployment.nats.client_certificate_file.clone().into(),
        deployment.nats.client_private_key_file.clone().into(),
        deployment.nats.credentials_file.clone().into(),
    )
    .await?;
    let outbox = AgentOutboxDispatcher::new(
        store.pool().clone(),
        nats.clone(),
        Duration::from_millis(deployment.nats.publish_timeout_milliseconds),
    )?;
    let build_consumer = AgentBuildCommandConsumer::bind(
        nats.clone(),
        &deployment.nats.build_command_stream_name,
        &deployment.nats.build_command_consumer_name,
        &deployment.nats.build_command_quarantine_subject,
    )
    .await?;
    let build_provider = NatsBuildSupplyChainProvider::new(
        nats,
        deployment.build.provider_subject.clone(),
        deployment.build.builder_binding.clone(),
        deployment.build.registry_binding.clone(),
        Duration::from_millis(deployment.build.stage_timeout_milliseconds),
    )
    .map_err(|_| StartupError::Configuration)?;
    let build_pipeline = BuildPipeline::new(
        build_provider,
        BuildPipelinePolicy {
            builder_binding: deployment.build.builder_binding.clone(),
            registry_binding: deployment.build.registry_binding.clone(),
            registry_robot_name: deployment.build.registry_robot_name.clone(),
            stage_timeout: Duration::from_millis(deployment.build.stage_timeout_milliseconds),
        },
    )
    .map_err(|_| StartupError::Configuration)?;
    let build_worker = BuildWorker::new(
        build_store.clone(),
        build_pipeline,
        format!("{}:build", deployment.worker_id),
        Duration::from_secs(deployment.build.worker_lease_seconds),
        Duration::from_millis(deployment.build.retry_delay_milliseconds),
        deployment.build.max_attempts,
    )
    .map_err(|_| StartupError::Configuration)?;
    let outbox_poll = Duration::from_millis(deployment.nats.outbox_poll_milliseconds);
    let objects = Arc::new(
        S3ImmutableObjectStore::new(
            deployment.object_store,
            S3Credential {
                access_key_id: read_trimmed(&deployment.object_store_access_key_file)?,
                secret_access_key: read_trimmed(&deployment.object_store_secret_key_file)?,
                session_token: deployment
                    .object_store_session_token_file
                    .as_deref()
                    .map(read_trimmed)
                    .transpose()?,
            },
        )
        .await?,
    );
    let classifier: Arc<dyn EgressClassifier> = Arc::new(
        DeterministicEgressClassifier::new(
            deployment.classifier_binding,
            deployment.classifier_revision,
        )
        .map_err(|_| StartupError::Configuration)?,
    );
    let process = Arc::new(TokioClaudeCodeProcess::new(read_worker_environment(
        &deployment.worker_environment_files,
    )?));
    let review_process: Arc<dyn ClaudeCodeProcess> = process.clone();
    let state = Arc::new(AgentApiState {
        store: store.clone(),
        build_store: build_store.clone(),
        generated_artifacts: generated_artifacts.clone(),
        llm_reviews: llm_reviews.clone(),
    });
    let bind = SocketAddr::from_str(&deployment.control_tls.bind_addr)
        .map_err(|_| StartupError::Configuration)?;
    let listener = tokio::net::TcpListener::bind(bind).await?;
    let work_execution_store = store.clone();
    let work_execution_objects = Arc::clone(&objects);
    let work_execution_artifacts = generated_artifacts.clone();
    let worker = Worker {
        store,
        objects,
        generated_artifacts,
        classifier: Arc::clone(&classifier),
        process: Arc::clone(&process),
        runtime_identity: deployment.worker_id.clone(),
        dispatch_lease: Duration::from_secs(deployment.dispatch_lease_seconds),
        track_lease: Duration::from_secs(deployment.track_lease_seconds),
        poll_interval: Duration::from_millis(deployment.poll_interval_milliseconds),
    };
    let review_worker = LlmReviewWorker {
        store: llm_reviews,
        classifier: Arc::clone(&classifier),
        process: review_process,
        worker_id: format!("{}:llm-review", deployment.worker_id),
        lease_duration: Duration::from_secs(deployment.track_lease_seconds),
        poll_interval: Duration::from_millis(deployment.poll_interval_milliseconds),
    };
    let work_execution_worker = WorkExecutionWorker::new(
        work_execution_store,
        work_execution_objects,
        work_execution_artifacts,
        environment,
        format!("{}:work-execution", deployment.worker_id),
        Duration::from_secs(deployment.track_lease_seconds),
        &work_execution_configuration,
    )
    .map_err(StartupError::WorkExecution)?;
    tokio::select! {
        result = http_transport::serve_tls(
            listener,
            with_service_auth(router(state), service_verifier),
            tls,
        ) => result?,
        result = worker.run() => result?,
        result = review_worker.run() => result?,
        result = work_execution_worker.run() => result?,
        result = build_command_loop(build_consumer, build_store) => result?,
        result = build_worker_loop(
            build_worker,
            Duration::from_millis(deployment.poll_interval_milliseconds)
        ) => result?,
        result = outbox_loop(outbox, outbox_poll) => result?,
    }
    Ok(())
}

async fn discover_service_auth()
-> Result<(Arc<ServiceTokenVerifier>, Arc<ServiceTokenClient>), StartupError> {
    let issuer = required_env("LABWEAVER_SERVICE_OIDC_ISSUER")?;
    let audience = required_env("LABWEAVER_SERVICE_AUDIENCE")?;
    let allowed_client_ids = required_set("LABWEAVER_SERVICE_ALLOWED_CLIENT_IDS")?;
    let scopes = required_set("LABWEAVER_SERVICE_SCOPES")?;
    if !scopes.contains("environment.work.configure")
        || !scopes.contains("environment:resolve_work_execution_binding")
    {
        return Err(StartupError::Configuration);
    }
    let algorithms = required_set("LABWEAVER_SERVICE_JWT_ALGORITHMS")?;
    let jwks_refresh_seconds = required_u64("LABWEAVER_SERVICE_JWKS_REFRESH_SECONDS")?;
    let jwks_retry_seconds = required_u64("LABWEAVER_SERVICE_JWKS_RETRY_SECONDS")?;
    let refresh_skew_seconds = required_u64("LABWEAVER_SERVICE_TOKEN_REFRESH_SKEW_SECONDS")?;
    let client_id = required_env("LABWEAVER_SERVICE_CLIENT_ID")?;
    let client_secret = read_trimmed(&required_env("LABWEAVER_SERVICE_CLIENT_SECRET_FILE")?)?;
    let oidc_ca = std::fs::read(required_path("LABWEAVER_SERVICE_OIDC_CA")?)?;
    let transport = TransportSecurityMode::Strict;
    let http = auth::no_redirect_http_client(Some(&oidc_ca), transport)
        .map_err(|_| StartupError::ServiceAuth(ServiceAuthError::HttpClient))?;
    let verifier_config = ServiceAuthConfig::new(
        &issuer,
        audience.clone(),
        allowed_client_ids,
        BTreeSet::new(),
        algorithms,
        jwks_refresh_seconds,
        jwks_retry_seconds,
        transport,
    )
    .map_err(|_| StartupError::ServiceAuth(ServiceAuthError::InvalidConfig))?;
    let verifier = ServiceTokenVerifier::discover(verifier_config, http.clone())
        .await
        .map(Arc::new)
        .map_err(StartupError::ServiceAuth)?;
    let client_config = ServiceTokenClientConfig::new(
        &issuer,
        client_id,
        client_secret,
        audience,
        scopes,
        refresh_skew_seconds,
        transport,
    )
    .map_err(StartupError::ServiceToken)?;
    let client = ServiceTokenClient::discover(client_config, http)
        .await
        .map(Arc::new)
        .map_err(StartupError::ServiceToken)?;
    Ok((verifier, client))
}

async fn run_build_executor() -> Result<(), StartupError> {
    let deployment = load_build_executor_deployment()?;
    if deployment.database_max_connections == 0 || deployment.database_max_connections > 32 {
        return Err(StartupError::Configuration);
    }
    let pool = PgPoolOptions::new()
        .max_connections(deployment.database_max_connections)
        .connect(&read_trimmed(&deployment.database_url_file)?)
        .await?;
    let schema_ready: bool = sqlx::query_scalar(
        "SELECT to_regclass('agent.build_executor_fences') IS NOT NULL \
         AND to_regclass('agent.build_executor_artifacts') IS NOT NULL",
    )
    .fetch_one(&pool)
    .await?;
    if !schema_ready {
        return Err(StartupError::SchemaUnavailable);
    }
    let objects = Arc::new(
        S3ImmutableObjectStore::new(
            deployment.object_store,
            S3Credential {
                access_key_id: read_trimmed(&deployment.object_store_access_key_file)?,
                secret_access_key: read_trimmed(&deployment.object_store_secret_key_file)?,
                session_token: deployment
                    .object_store_session_token_file
                    .as_deref()
                    .map(read_trimmed)
                    .transpose()?,
            },
        )
        .await?,
    );
    let nats = connect_nats_mtls(
        &deployment.nats.server,
        deployment.nats.ca_file.into(),
        deployment.nats.client_certificate_file.into(),
        deployment.nats.client_private_key_file.into(),
        deployment.nats.credentials_file.into(),
    )
    .await?;
    let backend = ProductionBuildExecutor::new(deployment.executor, pool.clone(), objects)
        .map_err(|_| StartupError::Configuration)?;
    let executor = FencedBuildExecutor::new(PgBuildExecutorFenceStore::new(pool), backend);
    let server = NatsBuildExecutorServer::new(nats, deployment.nats.request_subject, executor)?;
    tokio::try_join!(
        async { server.serve().await.map_err(StartupError::BuildExecutor) },
        async {
            service_runtime::run("build-executor")
                .await
                .map_err(StartupError::Service)
        }
    )?;
    Ok(())
}

async fn build_command_loop(
    mut consumer: AgentBuildCommandConsumer,
    store: PgBuildStore,
) -> Result<(), StartupError> {
    loop {
        let outcome = consumer.process_next(&store).await?;
        match outcome {
            agent_service::messaging::BuildConsumeOutcome::Applied => {
                tracing::info!(event = "agent.build.command_consumed", outcome = "applied");
            }
            agent_service::messaging::BuildConsumeOutcome::Ignored => {
                tracing::info!(event = "agent.build.command_consumed", outcome = "ignored");
            }
            agent_service::messaging::BuildConsumeOutcome::Deferred => tracing::warn!(
                event = "agent.build.command_deferred",
                outcome = "retry_scheduled",
                failure_stage = "consume",
                error_kind = "dependency",
                retryable = true
            ),
            agent_service::messaging::BuildConsumeOutcome::Rejected => tracing::warn!(
                event = "agent.build.command_rejected",
                outcome = "rejected",
                failure_stage = "consume",
                error_kind = "contract",
                retryable = false
            ),
        }
    }
}

async fn build_worker_loop(
    worker: BuildWorker<NatsBuildSupplyChainProvider>,
    interval: Duration,
) -> Result<(), StartupError> {
    if interval.is_zero() || interval > Duration::from_mins(1) {
        return Err(StartupError::Configuration);
    }
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        let outcome = worker.run_once(timestamp()?).await?;
        match outcome {
            agent_service::build_store::BuildWorkerOutcome::Idle => {
                tracing::debug!(event = "agent.build.worker_idle", outcome = "idle");
            }
            agent_service::build_store::BuildWorkerOutcome::Completed { build_request_id } => {
                tracing::info!(event = "agent.build.worker_completed", outcome = "succeeded", build_request_id = %build_request_id);
            }
            agent_service::build_store::BuildWorkerOutcome::RetryScheduled {
                build_request_id,
                attempt,
            } => {
                tracing::warn!(event = "agent.build.worker_retry_scheduled", outcome = "retry_scheduled", build_request_id = %build_request_id, attempt, failure_stage = "execute", error_kind = "provider", retryable = true);
            }
            agent_service::build_store::BuildWorkerOutcome::Failed {
                build_request_id,
                diagnostic_code,
            } => {
                tracing::error!(event = "agent.build.worker_failed", outcome = "failed", build_request_id = %build_request_id, diagnostic_code, failure_stage = "execute", error_kind = "provider", retryable = false);
            }
        }
    }
}

async fn outbox_loop(
    outbox: AgentOutboxDispatcher,
    interval: Duration,
) -> Result<(), StartupError> {
    if interval.is_zero() || interval > Duration::from_mins(1) {
        return Err(StartupError::Configuration);
    }
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        let published = outbox.dispatch_once().await?;
        if published {
            tracing::debug!(event = "agent.outbox.published");
        }
    }
}

#[derive(Clone)]
struct Worker {
    store: PostgresAgentRunStore,
    objects: Arc<S3ImmutableObjectStore>,
    generated_artifacts: GeneratedArtifactStore,
    classifier: Arc<dyn EgressClassifier>,
    process: Arc<TokioClaudeCodeProcess>,
    runtime_identity: String,
    dispatch_lease: Duration,
    track_lease: Duration,
    poll_interval: Duration,
}

impl Worker {
    #[allow(
        clippy::large_futures,
        reason = "the dispatch loop owns preparation and execution as one durable boundary"
    )]
    async fn run(self) -> Result<(), StartupError> {
        let mut ticker = tokio::time::interval(self.poll_interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let Some(lease) = self.store.claim_dispatch(self.dispatch_lease).await? else {
                continue;
            };
            let reader: Arc<dyn ProblemPackageReader> = Arc::new(DispatchReader {
                objects: Arc::clone(&self.objects),
                locators: lease.object_locators.clone(),
            });
            let gate = ProblemPackageEgressGate::new(reader, Arc::clone(&self.classifier));
            let now = timestamp()?;
            let input = match gate.prepare(&lease.package, &lease.policy).await {
                Ok(input) => input,
                Err(error) => {
                    let run = self
                        .store
                        .fail_dispatch_preparation(&lease, error.diagnostic_code(), now)
                        .await?;
                    tracing::warn!(event = "agent.dispatch.preparation_failed", run_id = %run.id, diagnostic_code = error.diagnostic_code(), failure_stage = "egress_gate", error_kind = "policy", retryable = false, error_detail = %error);
                    continue;
                }
            };
            self.store
                .bind_prepared_dispatch(&lease, input.sha256())
                .await?;
            let materializer = Arc::new(S3EnvironmentCandidateMaterializer::new(
                Arc::clone(&self.objects),
                lease.package.clone(),
                self.generated_artifacts.clone(),
            ));
            let runtime = agent_service::claude_code::ClaudeCodeRuntime::new_with_materializer(
                lease.policy.clone(),
                self.process.clone(),
                materializer,
            )?;
            let service = AgentRunService::new(
                self.store.clone(),
                runtime,
                self.runtime_identity.clone(),
                self.track_lease,
            )?;
            let outcome = service
                .execute_reserved_dispatch(lease, input, RunCancellation::new(), now)
                .await?;
            let run_id = match &outcome {
                agent_service::run_store::AgentRunDispatch::Executed(value) => value.run.id,
                agent_service::run_store::AgentRunDispatch::Replayed(run)
                | agent_service::run_store::AgentRunDispatch::Progressed(run) => run.id,
            };
            let dispatch_outcome = match outcome {
                agent_service::run_store::AgentRunDispatch::Executed(_) => "executed",
                agent_service::run_store::AgentRunDispatch::Replayed(_) => "replayed",
                agent_service::run_store::AgentRunDispatch::Progressed(_) => "progressed",
            };
            tracing::info!(event = "agent.dispatch.completed", run_id = %run_id, outcome = dispatch_outcome);
        }
    }
}

struct DispatchReader {
    objects: Arc<S3ImmutableObjectStore>,
    locators: BTreeMap<ArtifactId, String>,
}

#[async_trait]
impl ProblemPackageReader for DispatchReader {
    async fn read(
        &self,
        reference: &ArtifactRef,
        max_bytes: usize,
    ) -> Result<Vec<u8>, PackageObjectReadError> {
        if reference.store_binding != self.objects.binding()
            || reference.size_bytes > u64::try_from(max_bytes).unwrap_or(0)
        {
            return Err(PackageObjectReadError);
        }
        let key = self
            .locators
            .get(&reference.artifact_id)
            .ok_or(PackageObjectReadError)?;
        self.objects
            .read_verified(key, reference)
            .await
            .map(|object| object.bytes)
            .map_err(|_| PackageObjectReadError)
    }
}

fn load_deployment() -> Result<DeploymentFile, StartupError> {
    let path =
        std::env::var("LABWEAVER_AGENT_CONFIG_FILE").map_err(|_| StartupError::Configuration)?;
    serde_yaml::from_str(&std::fs::read_to_string(path)?).map_err(|_| StartupError::Configuration)
}

fn load_build_executor_deployment() -> Result<BuildExecutorDeploymentFile, StartupError> {
    let path = std::env::var("LABWEAVER_BUILD_EXECUTOR_CONFIG_FILE")
        .map_err(|_| StartupError::Configuration)?;
    serde_yaml::from_str(&std::fs::read_to_string(path)?).map_err(|_| StartupError::Configuration)
}

fn required_env(name: &'static str) -> Result<String, StartupError> {
    let value = std::env::var(name).map_err(|_| StartupError::Configuration)?;
    if value.trim().is_empty() {
        return Err(StartupError::Configuration);
    }
    Ok(value)
}

fn required_path(name: &'static str) -> Result<std::path::PathBuf, StartupError> {
    let path = std::path::PathBuf::from(required_env(name)?);
    if !path.is_absolute() {
        return Err(StartupError::Configuration);
    }
    Ok(path)
}

fn required_set(name: &'static str) -> Result<BTreeSet<String>, StartupError> {
    let values = required_env(name)?
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    if values.is_empty() {
        return Err(StartupError::Configuration);
    }
    Ok(values)
}

fn required_u64(name: &'static str) -> Result<u64, StartupError> {
    let value = required_env(name)?
        .parse::<u64>()
        .map_err(|_| StartupError::Configuration)?;
    if value == 0 {
        return Err(StartupError::Configuration);
    }
    Ok(value)
}

fn validate_deployment(deployment: &DeploymentFile) -> Result<(), StartupError> {
    if deployment.database_max_connections == 0
        || deployment.database_max_connections > 100
        || deployment.worker_id.trim().is_empty()
        || deployment.dispatch_lease_seconds == 0
        || deployment.track_lease_seconds == 0
        || deployment.poll_interval_milliseconds == 0
        || deployment.poll_interval_milliseconds > 60_000
        || deployment.build.worker_lease_seconds == 0
        || deployment.build.worker_lease_seconds > 300
        || deployment.build.retry_delay_milliseconds == 0
        || deployment.build.retry_delay_milliseconds > 300_000
        || deployment.build.max_attempts == 0
        || deployment.build.max_attempts > 100
        || !deployment
            .work_execution
            .environment_scopes
            .contains("environment.work.configure")
        || !deployment
            .work_execution
            .environment_scopes
            .contains("environment:resolve_work_execution_binding")
        || !worker_environment_contract_holds(deployment)
    {
        return Err(StartupError::Configuration);
    }
    Ok(())
}

fn worker_environment_contract_holds(deployment: &DeploymentFile) -> bool {
    let mut keys = deployment
        .worker_environment_files
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    keys.sort_unstable();
    keys == REQUIRED_WORKER_ENVIRONMENT
}

fn read_worker_environment(
    files: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, StartupError> {
    files
        .iter()
        .map(|(name, path)| {
            if name.is_empty()
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
            {
                return Err(StartupError::Configuration);
            }
            Ok((name.clone(), read_trimmed(path)?))
        })
        .collect()
}

fn read_trimmed(path: &str) -> Result<String, StartupError> {
    let value = std::fs::read_to_string(path)?;
    let value = value.trim();
    if value.is_empty() {
        return Err(StartupError::Configuration);
    }
    Ok(value.to_owned())
}

async fn verify_schema(pool: &sqlx::PgPool) -> Result<(), StartupError> {
    let ready: bool = sqlx::query_scalar(
        "SELECT to_regclass('agent.agent_run_dispatches') IS NOT NULL \
          AND to_regclass('agent.agent_track_work_items') IS NOT NULL \
          AND to_regclass('agent.build_commands') IS NOT NULL \
          AND to_regclass('agent.generated_artifacts') IS NOT NULL \
          AND to_regclass('agent.llm_review_runs') IS NOT NULL",
    )
    .fetch_one(pool)
    .await?;
    if !ready {
        return Err(StartupError::SchemaUnavailable);
    }
    Ok(())
}

fn timestamp() -> Result<UtcTimestamp, StartupError> {
    let value = OffsetDateTime::now_utc();
    let value = value
        .replace_nanosecond((value.nanosecond() / 1_000_000) * 1_000_000)
        .map_err(|_| StartupError::Clock)?;
    UtcTimestamp::from_utc(value).map_err(|_| StartupError::Clock)
}

#[derive(Debug, thiserror::Error)]
enum StartupError {
    #[error("LW_AGENT_CONFIG_INVALID")]
    Configuration,
    #[error("LW_AGENT_SCHEMA_UNAVAILABLE")]
    SchemaUnavailable,
    #[error("LW_AGENT_CLOCK_INVALID")]
    Clock,
    #[error(transparent)]
    ServiceAuth(#[from] ServiceAuthError),
    #[error("LW_AGENT_SERVICE_TOKEN_INVALID")]
    ServiceToken(#[source] auth::ServiceTokenClientError),
    #[error("LW_AGENT_WORK_EXECUTION_CLIENT_INVALID")]
    WorkExecutionClient(#[source] agent_service::work_execution::WorkExecutionClientError),
    #[error("LW_AGENT_WORK_EXECUTION_WORKER_FAILED")]
    WorkExecution(#[from] agent_service::work_execution::WorkExecutionWorkerError),
    #[error(transparent)]
    HttpTransport(#[from] http_transport::HttpTransportError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Telemetry(#[from] telemetry::TelemetryError),
    #[error(transparent)]
    ObjectStore(#[from] artifact_store::ObjectStoreError),
    #[error(transparent)]
    Store(#[from] agent_service::run_store::AgentRunStoreError),
    #[error(transparent)]
    LlmReview(#[from] agent_service::llm_review::LlmReviewStoreError),
    #[error(transparent)]
    BuildStore(#[from] agent_service::build_store::BuildStoreError),
    #[error(transparent)]
    Runtime(#[from] agent_service::claude_code::ClaudeCodeRuntimeError),
    #[error(transparent)]
    Messaging(#[from] agent_service::messaging::AgentMessagingError),
    #[error(transparent)]
    BuildExecutor(#[from] agent_service::build_provider::BuildExecutorFenceError),
    #[error(transparent)]
    Service(#[from] service_runtime::StartupError),
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::case_sensitive_file_extension_comparisons)]
mod deployment_contract_tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::{BuildExecutorDeploymentFile, DeploymentFile};

    #[test]
    fn checked_in_platform_example_matches_the_runtime_contract() {
        let example = include_str!("../../../deploy/config/agent-control-plane.yaml.example");
        let deployment: DeploymentFile =
            serde_yaml::from_str(example).expect("agent deployment example must deserialize");

        assert!(deployment.database_url_file.starts_with('/'));
        assert!(
            deployment.nats.server.starts_with("nats://")
                || deployment.nats.server.starts_with("tls://")
        );
        assert!(deployment.build.provider_subject.ends_with(".v1"));
        assert_eq!(
            deployment.worker_environment_files,
            BTreeMap::from([
                (
                    "ANTHROPIC_AUTH_TOKEN".to_owned(),
                    "/etc/labweaver/secrets/anthropic-auth-token".to_owned(),
                ),
                (
                    "ANTHROPIC_BASE_URL".to_owned(),
                    "/etc/labweaver/config/anthropic-base-url".to_owned(),
                ),
                (
                    "ANTHROPIC_MODEL".to_owned(),
                    "/etc/labweaver/config/anthropic-model".to_owned(),
                ),
            ])
        );
        assert!(!example.contains(".v2"));
        assert!(!example.contains("ECNU_API_KEY"));
        assert!(super::worker_environment_contract_holds(&deployment));
        assert_eq!(
            deployment.work_execution.environment_base_uri,
            "https://environment-service:9446/"
        );
        assert_eq!(
            deployment.work_execution.environment_ca_file,
            "/etc/labweaver/secrets/mtls-ca.pem"
        );
        assert_eq!(
            deployment.work_execution.environment_audience,
            "labweaver-environment"
        );
        assert_eq!(
            deployment.work_execution.environment_scopes,
            BTreeSet::from([
                "environment.work.configure".to_owned(),
                "environment:resolve_work_execution_binding".to_owned(),
            ])
        );
    }

    fn example_deployment() -> super::DeploymentFile {
        let example = include_str!("../../../deploy/config/agent-control-plane.yaml.example");
        serde_yaml::from_str(example).expect("agent deployment example must deserialize")
    }

    #[test]
    fn worker_environment_missing_model_is_rejected() {
        let mut deployment = example_deployment();
        deployment
            .worker_environment_files
            .remove("ANTHROPIC_MODEL");
        assert!(!super::worker_environment_contract_holds(&deployment));
    }

    #[test]
    fn worker_environment_rejects_legacy_operator_aliases() {
        let mut deployment = example_deployment();
        deployment.worker_environment_files.insert(
            "ECNU_API_KEY".to_owned(),
            "/etc/labweaver/secrets/legacy".to_owned(),
        );
        assert!(!super::worker_environment_contract_holds(&deployment));
    }

    #[test]
    fn work_execution_requires_both_environment_permissions() {
        let mut deployment = example_deployment();
        deployment
            .work_execution
            .environment_scopes
            .remove("environment:resolve_work_execution_binding");

        assert!(matches!(
            super::validate_deployment(&deployment),
            Err(super::StartupError::Configuration)
        ));
    }

    #[test]
    fn checked_in_build_executor_example_requires_mtls_buildkit() {
        let example = include_str!("../../../deploy/config/build-executor.yaml.example");
        let deployment: BuildExecutorDeploymentFile = serde_yaml::from_str(example)
            .expect("build executor deployment example must deserialize");

        assert!(deployment.executor.buildkit_address.starts_with("tcp://"));
        for path in [
            &deployment.executor.buildkit_ca_file,
            &deployment.executor.buildkit_client_certificate_file,
            &deployment.executor.buildkit_client_private_key_file,
        ] {
            assert!(path.to_string_lossy().starts_with('/'));
        }
        assert!(deployment.nats.request_subject.ends_with(".v1"));
    }
}
