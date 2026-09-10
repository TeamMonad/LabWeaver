//! Freeze-only Evaluation API and transactional Outbox process.
#![allow(
    missing_docs,
    clippy::missing_errors_doc,
    reason = "startup exposes only reviewed bindings and stable diagnostics"
)]

use std::{
    collections::BTreeSet,
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
    time::Duration,
};

use artifact_store::{S3Credential, S3ImmutableObjectStore, S3StoreConfig};
use auth::{
    ServerTlsFileConfig, ServiceAuthConfig, ServiceAuthError, ServiceTokenClient,
    ServiceTokenClientConfig, ServiceTokenClientError, ServiceTokenVerifier, TransportSecurityMode,
};
use serde::Deserialize;
use sqlx::postgres::PgPoolOptions;

use crate::{
    AgentClient, AgentClientConfiguration, AuthoringAdmissionClient,
    AuthoringAdmissionClientConfiguration, EnvironmentExecutionBindingClient, EvaluationApiState,
    EvaluationExecutionConfiguration, EvaluationOutboxDispatcher, EvaluationOutboxError,
    EvaluationWorker, FreezeCoordinator, FreezeCoordinatorConfiguration, FreezeCoordinatorError,
    KubernetesEvaluationRunner, PgEvaluationControlStore, PgFreezeCommandStore, PgFreezeStore,
    ResourceClient, ResourceClientConfiguration, evaluation_api_router, with_service_auth,
};

const CONFIG_PATH: &str = "LABWEAVER_EVALUATION_CONFIG_FILE";
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EvaluationConfiguration {
    database_url_file: PathBuf,
    database_max_connections: u32,
    api_tls: ServerTlsFileConfig,
    nats: NatsConfiguration,
    outbox_publish_timeout_milliseconds: u64,
    outbox_poll_interval_milliseconds: u64,
    coordinator_poll_interval_milliseconds: u64,
    coordinator: FreezeCoordinatorConfiguration,
    resource: ResourceClientConfiguration,
    agent: AgentClientConfiguration,
    authoring_admission: AuthoringAdmissionClientConfiguration,
    object_store: S3StoreConfig,
    object_store_access_key_file: PathBuf,
    object_store_secret_key_file: PathBuf,
    object_store_session_token_file: Option<PathBuf>,
    execution: EvaluationExecutionConfiguration,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NatsConfiguration {
    server: String,
    ca_file: PathBuf,
    client_certificate_file: PathBuf,
    client_private_key_file: PathBuf,
    credentials_file: PathBuf,
}

#[allow(clippy::too_many_lines)]
pub async fn run_evaluation_service() -> Result<(), EvaluationProcessError> {
    telemetry::init("evaluation-service")?;
    let configuration: EvaluationConfiguration = read_configuration()?;
    validate_configuration(&configuration)?;
    let tls = crate::http_transport::load_server_config(
        &configuration.api_tls.server_certificate_file,
        &configuration.api_tls.server_key_file,
    )?;
    let (service_verifier, service_token_client, service_scopes) =
        discover_service_auth(&configuration.resource.audience).await?;
    let authoring_admission = AuthoringAdmissionClient::from_configuration(
        configuration.authoring_admission,
        service_token_client.clone(),
        service_scopes.clone(),
    )?;
    let resource_client = ResourceClient::from_configuration(
        configuration.resource,
        service_token_client.clone(),
        service_scopes.clone(),
    )?;
    let agent_client = AgentClient::from_configuration(
        configuration.agent,
        service_token_client.clone(),
        service_scopes.clone(),
    )
    .map_err(EvaluationProcessError::Agent)?;
    let environment_client = EnvironmentExecutionBindingClient::from_configuration(
        configuration.execution.environment.clone(),
        service_token_client.clone(),
        service_scopes.clone(),
    )
    .map_err(EvaluationProcessError::EnvironmentBinding)?;
    let pool = PgPoolOptions::new()
        .max_connections(configuration.database_max_connections)
        .connect(&read_secret(&configuration.database_url_file)?)
        .await?;
    require_schema(&pool).await?;
    let nats = connect_nats(&configuration.nats).await?;
    let materializer_ca_bundle = configuration
        .object_store
        .ca_bundle_file
        .as_deref()
        .map(Path::new)
        .map(|path| read_mounted_file(path, MAX_CONFIG_BYTES))
        .transpose()?
        .map(Arc::<[u8]>::from);
    let object_store = Arc::new(
        S3ImmutableObjectStore::new(
            configuration.object_store,
            S3Credential {
                access_key_id: read_secret(&configuration.object_store_access_key_file)?,
                secret_access_key: read_secret(&configuration.object_store_secret_key_file)?,
                session_token: configuration
                    .object_store_session_token_file
                    .as_deref()
                    .map(read_secret)
                    .transpose()?,
            },
        )
        .await
        .map_err(EvaluationProcessError::ObjectStore)?,
    );
    let address = SocketAddr::from_str(&configuration.api_tls.bind_addr)
        .map_err(|_| EvaluationProcessError::ConfigurationInvalid)?;
    let listener = tokio::net::TcpListener::bind(address).await?;
    let command_store = PgFreezeCommandStore::new(pool.clone());
    let freeze_store = PgFreezeStore::new(pool.clone());
    let control_store = PgEvaluationControlStore::new(pool.clone());
    let api = with_service_auth(
        evaluation_api_router(EvaluationApiState::new(
            command_store.clone(),
            freeze_store.clone(),
            control_store.clone(),
            Arc::new(authoring_admission.clone()),
        )),
        service_verifier,
    );
    let coordinator = FreezeCoordinator::new(
        configuration.coordinator,
        command_store,
        service_token_client,
        &service_scopes,
    )?;
    let meter_control = control_store.clone();
    let meter_resource = resource_client.clone();
    let meter_poll_interval =
        Duration::from_millis(configuration.execution.resource_poll_interval_milliseconds);
    let dispatcher = EvaluationOutboxDispatcher::new(
        pool.clone(),
        nats.clone(),
        Duration::from_millis(configuration.outbox_publish_timeout_milliseconds),
    )?;
    let poll_interval = Duration::from_millis(configuration.outbox_poll_interval_milliseconds);
    let coordinator_poll_interval =
        Duration::from_millis(configuration.coordinator_poll_interval_milliseconds);
    let execution = KubernetesEvaluationRunner::new(
        control_store.clone(),
        freeze_store,
        object_store,
        resource_client,
        agent_client,
        authoring_admission,
        environment_client,
        configuration.execution.clone(),
        materializer_ca_bundle,
    )?;
    let evaluation_worker = EvaluationWorker::new(
        control_store,
        Arc::new(execution),
        configuration.execution.worker_id.clone(),
        Duration::from_secs(configuration.execution.worker_lease_seconds),
        Duration::from_millis(configuration.execution.scheduler_poll_interval_milliseconds),
    )?;
    tracing::info!(event = "evaluation.service.started", %address);
    tokio::select! {
        result = crate::http_transport::serve_tls(listener, api, tls) => {
            result.map_err(EvaluationProcessError::HttpTransport)?;
        }
        result = outbox_loop(dispatcher, poll_interval) => {
            result?;
        }
        result = coordinator_loop(coordinator, coordinator_poll_interval) => {
            result?;
        }
        result = evaluation_worker.run() => {
            result.map_err(EvaluationProcessError::Execution)?;
        }
        result = resource_meter_loop(meter_control, meter_resource, meter_poll_interval) => {
            result?;
        }
        result = shutdown_signal() => {
            result?;
        }
    }
    nats.drain()
        .await
        .map_err(|_| EvaluationProcessError::NatsDrain)?;
    pool.close().await;
    tracing::info!(event = "evaluation.service.stopped");
    Ok(())
}

async fn coordinator_loop(
    coordinator: FreezeCoordinator,
    poll_interval: Duration,
) -> Result<(), EvaluationProcessError> {
    let mut interval = tokio::time::interval(poll_interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        coordinator.reconcile_once().await?;
    }
}

async fn outbox_loop(
    dispatcher: EvaluationOutboxDispatcher,
    poll_interval: Duration,
) -> Result<(), EvaluationProcessError> {
    loop {
        if !dispatcher.dispatch_once().await? {
            tokio::time::sleep(poll_interval).await;
        }
    }
}

async fn resource_meter_loop(
    control: PgEvaluationControlStore,
    resource: ResourceClient,
    poll_interval: Duration,
) -> Result<(), EvaluationProcessError> {
    loop {
        let Some(delivery) = control.claim_resource_meter_delivery().await? else {
            tokio::time::sleep(poll_interval).await;
            continue;
        };
        match resource.record_resource_usage(&delivery.request).await {
            Ok(_) => {
                control
                    .mark_resource_meter_delivery_delivered(
                        delivery.delivery_id,
                        delivery.source_event_id,
                    )
                    .await?;
            }
            Err(error) => {
                tracing::warn!(
                    event = "evaluation.resource_usage.delivery_failed",
                    delivery_id = %delivery.delivery_id,
                    task_run_id = %delivery.task_run_id,
                    kind = ?delivery.request.kind,
                    attempts = delivery.attempts,
                    error = ?error,
                );
                control
                    .mark_resource_meter_delivery_failed(
                        delivery.delivery_id,
                        delivery.source_event_id,
                        "LW_EVALUATION_RESOURCE_USAGE_DELIVERY_FAILED",
                    )
                    .await?;
            }
        }
    }
}

async fn connect_nats(
    configuration: &NatsConfiguration,
) -> Result<async_nats::Client, EvaluationProcessError> {
    let options = async_nats::ConnectOptions::new()
        .require_tls(true)
        .add_root_certificates(configuration.ca_file.clone())
        .add_client_certificate(
            configuration.client_certificate_file.clone(),
            configuration.client_private_key_file.clone(),
        )
        .credentials_file(configuration.credentials_file.clone())
        .await
        .map_err(|_| EvaluationProcessError::NatsCredentials)?;
    options
        .connect(&configuration.server)
        .await
        .map_err(|_| EvaluationProcessError::NatsConnect)
}

async fn discover_service_auth(
    resource_audience: &str,
) -> Result<
    (
        Arc<ServiceTokenVerifier>,
        Arc<ServiceTokenClient>,
        BTreeSet<String>,
    ),
    EvaluationProcessError,
> {
    let issuer = required_env("LABWEAVER_SERVICE_OIDC_ISSUER")?;
    let inbound_audience = required_env("LABWEAVER_SERVICE_AUDIENCE")?;
    let allowed_client_ids = required_set("LABWEAVER_SERVICE_ALLOWED_CLIENT_IDS")?;
    let algorithms = required_set("LABWEAVER_SERVICE_JWT_ALGORITHMS")?;
    let jwks_refresh_seconds = required_u64("LABWEAVER_SERVICE_JWKS_REFRESH_SECONDS")?;
    let jwks_retry_seconds = required_u64("LABWEAVER_SERVICE_JWKS_RETRY_SECONDS")?;
    let client_id = required_env("LABWEAVER_SERVICE_CLIENT_ID")?;
    let client_secret_file = required_path("LABWEAVER_SERVICE_CLIENT_SECRET_FILE")?;
    let client_secret = read_secret(&client_secret_file)?;
    let refresh_skew_seconds = required_u64("LABWEAVER_SERVICE_TOKEN_REFRESH_SKEW_SECONDS")?;
    let scopes = required_set("LABWEAVER_SERVICE_SCOPES")?;
    validate_resource_scopes(&scopes)?;
    validate_authoring_scopes(&scopes)?;
    validate_agent_scopes(&scopes)?;
    validate_environment_scopes(&scopes)?;
    let oidc_ca_path = required_path("LABWEAVER_SERVICE_OIDC_CA")?;
    let oidc_ca = read_mounted_file(&oidc_ca_path, MAX_CONFIG_BYTES)?;
    let http = auth::no_redirect_http_client(Some(&oidc_ca), TransportSecurityMode::Strict)
        .map_err(|_| EvaluationProcessError::ServiceAuth(ServiceAuthError::HttpClient))?;
    let config = ServiceAuthConfig::new(
        &issuer,
        inbound_audience,
        allowed_client_ids,
        BTreeSet::new(),
        algorithms,
        jwks_refresh_seconds,
        jwks_retry_seconds,
        TransportSecurityMode::Strict,
    )
    .map_err(|_| EvaluationProcessError::ServiceAuth(ServiceAuthError::InvalidConfig))?;
    let verifier = ServiceTokenVerifier::discover(config, http.clone())
        .await
        .map(Arc::new)
        .map_err(EvaluationProcessError::ServiceAuth)?;
    let token_config = ServiceTokenClientConfig::new(
        &issuer,
        client_id,
        client_secret,
        resource_audience.to_owned(),
        scopes.clone(),
        refresh_skew_seconds,
        TransportSecurityMode::Strict,
    )
    .map_err(EvaluationProcessError::ServiceToken)?;
    let token_client = ServiceTokenClient::discover(token_config, http)
        .await
        .map(Arc::new)
        .map_err(EvaluationProcessError::ServiceToken)?;
    tracing::info!(
        event = "evaluation.resource_client.configured",
        audience = resource_audience,
        scopes = ?scopes,
        required_scopes = ?crate::resource_client::required_scopes_for_diagnostics(),
    );
    tracing::info!(
        event = "evaluation.authoring_admission_client.auth_configured",
        required_scope = crate::authoring_client::required_scope_for_diagnostics(),
    );
    Ok((verifier, token_client, scopes))
}

fn validate_resource_scopes(scopes: &BTreeSet<String>) -> Result<(), EvaluationProcessError> {
    if crate::resource_client::required_scopes_for_diagnostics()
        .iter()
        .any(|required| !scopes.contains(*required))
    {
        return Err(EvaluationProcessError::ConfigurationInvalid);
    }
    Ok(())
}

fn validate_authoring_scopes(scopes: &BTreeSet<String>) -> Result<(), EvaluationProcessError> {
    if !scopes.contains(crate::authoring_client::required_scope_for_diagnostics())
        || !scopes.contains(crate::authoring_client::required_policy_scope_for_diagnostics())
    {
        return Err(EvaluationProcessError::ConfigurationInvalid);
    }
    Ok(())
}

fn validate_agent_scopes(scopes: &BTreeSet<String>) -> Result<(), EvaluationProcessError> {
    if crate::agent_client::required_scopes_for_diagnostics()
        .iter()
        .any(|required| !scopes.contains(*required))
    {
        return Err(EvaluationProcessError::ConfigurationInvalid);
    }
    Ok(())
}

fn validate_environment_scopes(scopes: &BTreeSet<String>) -> Result<(), EvaluationProcessError> {
    if !scopes.contains(crate::environment_client::required_scope_for_diagnostics()) {
        return Err(EvaluationProcessError::ConfigurationInvalid);
    }
    Ok(())
}

fn required_env(name: &'static str) -> Result<String, EvaluationProcessError> {
    let value = std::env::var(name).map_err(|_| EvaluationProcessError::ConfigurationInvalid)?;
    if value.trim().is_empty() {
        return Err(EvaluationProcessError::ConfigurationInvalid);
    }
    Ok(value)
}

fn required_path(name: &'static str) -> Result<PathBuf, EvaluationProcessError> {
    let value = PathBuf::from(required_env(name)?);
    if value.is_absolute() {
        Ok(value)
    } else {
        Err(EvaluationProcessError::ConfigurationInvalid)
    }
}

fn required_set(name: &'static str) -> Result<BTreeSet<String>, EvaluationProcessError> {
    let values = required_env(name)?
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    if values.is_empty() {
        return Err(EvaluationProcessError::ConfigurationInvalid);
    }
    Ok(values)
}

fn required_u64(name: &'static str) -> Result<u64, EvaluationProcessError> {
    let value = required_env(name)?
        .parse::<u64>()
        .map_err(|_| EvaluationProcessError::ConfigurationInvalid)?;
    if value == 0 {
        return Err(EvaluationProcessError::ConfigurationInvalid);
    }
    Ok(value)
}

fn read_configuration() -> Result<EvaluationConfiguration, EvaluationProcessError> {
    let path = std::env::var(CONFIG_PATH)
        .map(PathBuf::from)
        .map_err(|_| EvaluationProcessError::ConfigurationMissing)?;
    if !path.is_absolute() {
        return Err(EvaluationProcessError::ConfigurationInvalid);
    }
    serde_yaml::from_slice(&read_mounted_file(&path, MAX_CONFIG_BYTES)?)
        .map_err(|_| EvaluationProcessError::ConfigurationInvalid)
}

fn validate_configuration(
    configuration: &EvaluationConfiguration,
) -> Result<(), EvaluationProcessError> {
    let nats_paths = [
        &configuration.nats.ca_file,
        &configuration.nats.client_certificate_file,
        &configuration.nats.client_private_key_file,
        &configuration.nats.credentials_file,
    ];
    if configuration.database_max_connections == 0
        || !configuration.database_url_file.is_absolute()
        || !configuration.object_store_access_key_file.is_absolute()
        || !configuration.object_store_secret_key_file.is_absolute()
        || configuration
            .object_store_session_token_file
            .as_ref()
            .is_some_and(|path| !path.is_absolute())
        || configuration.nats.server.trim().is_empty()
        || nats_paths.iter().any(|path| !path.is_absolute())
        || !(100..=30_000).contains(&configuration.outbox_publish_timeout_milliseconds)
        || !(10..=10_000).contains(&configuration.outbox_poll_interval_milliseconds)
        || !(100..=10_000).contains(&configuration.coordinator_poll_interval_milliseconds)
    {
        return Err(EvaluationProcessError::ConfigurationInvalid);
    }
    Ok(())
}

fn read_secret(path: &Path) -> Result<String, EvaluationProcessError> {
    if !path.is_absolute() {
        return Err(EvaluationProcessError::ConfigurationInvalid);
    }
    let value = String::from_utf8(read_mounted_file(path, 16 * 1024)?)
        .map_err(|_| EvaluationProcessError::ConfigurationInvalid)?
        .trim()
        .to_owned();
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(EvaluationProcessError::ConfigurationInvalid);
    }
    Ok(value)
}

fn read_mounted_file(path: &Path, max_bytes: u64) -> Result<Vec<u8>, EvaluationProcessError> {
    let parent = path
        .parent()
        .ok_or(EvaluationProcessError::ConfigurationInvalid)?;
    let canonical_parent = fs::canonicalize(parent)?;
    let canonical = fs::canonicalize(path)?;
    let metadata = fs::metadata(&canonical)?;
    if !canonical.starts_with(canonical_parent)
        || !metadata.is_file()
        || metadata.len() == 0
        || metadata.len() > max_bytes
    {
        return Err(EvaluationProcessError::ConfigurationInvalid);
    }
    Ok(fs::read(canonical)?)
}

async fn require_schema(pool: &sqlx::PgPool) -> Result<(), EvaluationProcessError> {
    let ready: bool = sqlx::query_scalar(
        "SELECT to_regclass('evaluation.submission_freeze_commands') IS NOT NULL \
         AND to_regclass('evaluation.outbox_events') IS NOT NULL \
         AND to_regclass('evaluation.evaluation_releases') IS NOT NULL \
         AND to_regclass('evaluation.evaluation_runs') IS NOT NULL \
         AND to_regclass('evaluation.evaluation_step_runs') IS NOT NULL \
         AND to_regclass('evaluation.evaluation_step_attempts') IS NOT NULL \
         AND to_regclass('evaluation.resource_meter_deliveries') IS NOT NULL",
    )
    .fetch_one(pool)
    .await?;
    if !ready {
        return Err(EvaluationProcessError::SchemaUnavailable);
    }
    Ok(())
}

async fn shutdown_signal() -> Result<(), EvaluationProcessError> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .map_err(EvaluationProcessError::Signal)?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result.map_err(EvaluationProcessError::Signal)?,
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c()
        .await
        .map_err(EvaluationProcessError::Signal)?;
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum EvaluationProcessError {
    #[error("LW_EVALUATION_CONFIG_MISSING")]
    ConfigurationMissing,
    #[error("LW_EVALUATION_CONFIG_INVALID")]
    ConfigurationInvalid,
    #[error("LW_EVALUATION_SCHEMA_UNAVAILABLE")]
    SchemaUnavailable,
    #[error("LW_EVALUATION_NATS_CREDENTIALS_INVALID")]
    NatsCredentials,
    #[error("LW_EVALUATION_NATS_UNAVAILABLE")]
    NatsConnect,
    #[error("LW_EVALUATION_NATS_DRAIN_FAILED")]
    NatsDrain,
    #[error("LW_EVALUATION_IO_FAILED")]
    Io(#[from] std::io::Error),
    #[error("LW_EVALUATION_API_FAILED")]
    Api(#[source] std::io::Error),
    #[error(transparent)]
    HttpTransport(#[from] crate::http_transport::HttpTransportError),
    #[error(transparent)]
    ServiceAuth(#[from] ServiceAuthError),
    #[error(transparent)]
    ServiceToken(#[from] ServiceTokenClientError),
    #[error(transparent)]
    EnvironmentBinding(#[from] crate::environment_client::EnvironmentExecutionBindingClientError),
    #[error(transparent)]
    Resource(#[from] crate::resource_client::ResourceClientError),
    #[error(transparent)]
    AuthoringAdmission(#[from] crate::authoring_client::AuthoringAdmissionClientError),
    #[error(transparent)]
    Agent(#[from] crate::agent_client::AgentClientError),
    #[error(transparent)]
    ObjectStore(#[from] artifact_store::ObjectStoreError),
    #[error(transparent)]
    Execution(#[from] crate::execution::ExecutionError),
    #[error("LW_EVALUATION_SIGNAL_FAILED")]
    Signal(#[source] std::io::Error),
    #[error("LW_EVALUATION_DATABASE_FAILED")]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Telemetry(#[from] telemetry::TelemetryError),
    #[error(transparent)]
    Outbox(#[from] EvaluationOutboxError),
    #[error(transparent)]
    Coordinator(#[from] FreezeCoordinatorError),
    #[error(transparent)]
    Control(#[from] crate::control_plane::EvaluationControlStoreError),
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "the checked-in deployment example is a static test fixture"
)]
mod tests {
    use super::EvaluationConfiguration;

    #[test]
    fn checked_in_evaluation_configuration_matches_process_schema() {
        let configuration: EvaluationConfiguration = serde_yaml::from_str(include_str!(
            "../../../deploy/config/evaluation-service.yaml.example"
        ))
        .expect("evaluation service example must deserialize");

        configuration
            .object_store
            .validate()
            .expect("evaluation object store binding is valid");
        assert_eq!(
            configuration.execution.runner_namespace,
            "labweaver-evaluation"
        );
        assert_eq!(
            configuration.execution.oj.runner_namespace,
            configuration.execution.runner_namespace
        );
        assert_eq!(
            configuration
                .execution
                .ansible_probe_executor
                .runner_namespace,
            configuration.execution.runner_namespace
        );
    }
}
