//! Production Agent Service internal API and recoverable worker process.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use agent_service::api::{AgentApiState, router, serve_mtls};
use agent_service::build_pipeline::{BuildPipeline, BuildPipelinePolicy};
use agent_service::build_provider::NatsBuildSupplyChainProvider;
use agent_service::build_store::{BuildWorker, PgBuildStore};
use agent_service::classifier::DeterministicEgressClassifier;
use agent_service::claude_code::{
    EgressClassifier, PackageObjectReadError, ProblemPackageEgressGate, ProblemPackageReader,
    RunCancellation, TokioClaudeCodeProcess,
};
use agent_service::messaging::{
    AgentBuildCommandConsumer, AgentOutboxDispatcher, connect_nats_mtls,
};
use agent_service::run_store::{AgentRunService, ExecuteAgentRun, PostgresAgentRunStore};
use artifact_store::{ImmutableObjectStore, S3Credential, S3ImmutableObjectStore, S3StoreConfig};
use async_trait::async_trait;
use auth::{MtlsFileConfig, load_mtls_server_config};
use contracts::{ArtifactId, ArtifactRef, PolicyId, Revision, Sha256Digest, UtcTimestamp};
use serde::Deserialize;
use sqlx::postgres::PgPoolOptions;
use time::OffsetDateTime;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeploymentFile {
    database_url_file: String,
    database_max_connections: u32,
    control_mtls: MtlsFileConfig,
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
    build: BuildFileConfig,
    nats: NatsFileConfig,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BuildFileConfig {
    provider_subject: String,
    builder_binding: String,
    scanner_binding: String,
    signer_binding: String,
    registry_binding: String,
    policy_id: PolicyId,
    policy_revision: Revision,
    scanner_name: String,
    scanner_version: String,
    scanner_database_sha256: Sha256Digest,
    trust_bundle_sha256: Sha256Digest,
    expected_fulcio_issuer: String,
    expected_certificate_subject: String,
    registry_robot_name: String,
    evidence_ttl_milliseconds: u64,
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

#[tokio::main]
#[allow(
    clippy::too_many_lines,
    reason = "startup keeps every fail-closed Agent dependency binding in one auditable boundary"
)]
async fn main() -> Result<(), StartupError> {
    telemetry::init(env!("CARGO_PKG_NAME"))?;
    let deployment = load_deployment()?;
    validate_deployment(&deployment)?;
    let pool = PgPoolOptions::new()
        .max_connections(deployment.database_max_connections)
        .connect(&read_trimmed(&deployment.database_url_file)?)
        .await?;
    verify_schema(&pool).await?;
    let store = PostgresAgentRunStore::new(pool.clone());
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
        deployment.build.scanner_binding.clone(),
        deployment.build.signer_binding.clone(),
        deployment.build.registry_binding.clone(),
    )
    .map_err(|_| StartupError::Configuration)?;
    let build_pipeline = BuildPipeline::new(
        build_provider,
        BuildPipelinePolicy {
            builder_binding: deployment.build.builder_binding.clone(),
            scanner_binding: deployment.build.scanner_binding.clone(),
            signer_binding: deployment.build.signer_binding.clone(),
            registry_binding: deployment.build.registry_binding.clone(),
            policy_id: deployment.build.policy_id,
            policy_revision: deployment.build.policy_revision,
            scanner_name: deployment.build.scanner_name.clone(),
            scanner_version: deployment.build.scanner_version.clone(),
            scanner_database_sha256: deployment.build.scanner_database_sha256,
            trust_bundle_sha256: deployment.build.trust_bundle_sha256,
            expected_fulcio_issuer: deployment.build.expected_fulcio_issuer.clone(),
            expected_certificate_subject: deployment.build.expected_certificate_subject.clone(),
            registry_robot_name: deployment.build.registry_robot_name.clone(),
            evidence_ttl_milliseconds: deployment.build.evidence_ttl_milliseconds,
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
    let state = Arc::new(AgentApiState {
        store: store.clone(),
        build_store: build_store.clone(),
    });
    let bind = SocketAddr::from_str(&deployment.control_mtls.bind_addr)
        .map_err(|_| StartupError::Configuration)?;
    let listener = tokio::net::TcpListener::bind(bind).await?;
    let mtls = load_mtls_server_config(&deployment.control_mtls)?;
    let worker = Worker {
        store,
        objects,
        classifier,
        process,
        runtime_identity: deployment.worker_id,
        dispatch_lease: Duration::from_secs(deployment.dispatch_lease_seconds),
        track_lease: Duration::from_secs(deployment.track_lease_seconds),
        poll_interval: Duration::from_millis(deployment.poll_interval_milliseconds),
    };
    tokio::select! {
        result = serve_mtls(listener, router(state), mtls) => result?,
        result = worker.run() => result?,
        result = build_command_loop(build_consumer, build_store) => result?,
        result = build_worker_loop(
            build_worker,
            Duration::from_millis(deployment.poll_interval_milliseconds)
        ) => result?,
        result = outbox_loop(outbox, outbox_poll) => result?,
    }
    Ok(())
}

async fn build_command_loop(
    mut consumer: AgentBuildCommandConsumer,
    store: PgBuildStore,
) -> Result<(), StartupError> {
    loop {
        let outcome = consumer.process_next(&store).await?;
        tracing::info!(event = "agent.build.command_consumed", ?outcome);
    }
}

async fn build_worker_loop(
    worker: BuildWorker<NatsBuildSupplyChainProvider>,
    interval: Duration,
) -> Result<(), StartupError> {
    if interval.is_zero() || interval > Duration::from_secs(60) {
        return Err(StartupError::Configuration);
    }
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        let outcome = worker.run_once(timestamp()?).await?;
        tracing::debug!(event = "agent.build.worker_completed", ?outcome);
    }
}

async fn outbox_loop(
    outbox: AgentOutboxDispatcher,
    interval: Duration,
) -> Result<(), StartupError> {
    if interval.is_zero() || interval > Duration::from_secs(60) {
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
    classifier: Arc<dyn EgressClassifier>,
    process: Arc<TokioClaudeCodeProcess>,
    runtime_identity: String,
    dispatch_lease: Duration,
    track_lease: Duration,
    poll_interval: Duration,
}

impl Worker {
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
                    tracing::warn!(event = "agent.dispatch.preparation_failed", run_id = %run.id, diagnostic = error.diagnostic_code());
                    continue;
                }
            };
            self.store
                .bind_prepared_dispatch(&lease, input.sha256())
                .await?;
            let runtime = agent_service::claude_code::ClaudeCodeRuntime::new(
                lease.policy.clone(),
                self.process.clone(),
            )?;
            let service = AgentRunService::new(
                self.store.clone(),
                runtime,
                self.runtime_identity.clone(),
                self.track_lease,
            )?;
            let outcome = service
                .execute(ExecuteAgentRun {
                    course_id: lease.run.course_id,
                    request: &lease.request,
                    idempotency_key: &lease.idempotency_key,
                    input,
                    cancellation: RunCancellation::new(),
                    now,
                    trace_id: &lease.trace_id,
                })
                .await?;
            tracing::info!(event = "agent.dispatch.completed", run_id = %lease.run.id, outcome = ?outcome);
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
            .read_verified(
                key,
                &reference.object_version,
                reference.size_bytes,
                reference.sha256,
                &reference.media_type,
            )
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
    {
        return Err(StartupError::Configuration);
    }
    Ok(())
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
         AND to_regclass('agent.build_commands') IS NOT NULL",
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
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Telemetry(#[from] telemetry::TelemetryError),
    #[error(transparent)]
    Mtls(#[from] auth::MtlsError),
    #[error(transparent)]
    ObjectStore(#[from] artifact_store::ObjectStoreError),
    #[error(transparent)]
    Store(#[from] agent_service::run_store::AgentRunStoreError),
    #[error(transparent)]
    BuildStore(#[from] agent_service::build_store::BuildStoreError),
    #[error(transparent)]
    Runtime(#[from] agent_service::claude_code::ClaudeCodeRuntimeError),
    #[error(transparent)]
    Messaging(#[from] agent_service::messaging::AgentMessagingError),
}
