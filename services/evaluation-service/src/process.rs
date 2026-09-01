//! Freeze-only Evaluation API and transactional Outbox process.
#![allow(
    missing_docs,
    clippy::missing_errors_doc,
    reason = "startup exposes only reviewed bindings and stable diagnostics"
)]

use std::{
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    str::FromStr,
    time::Duration,
};

use auth::MtlsFileConfig;
use serde::Deserialize;
use sqlx::postgres::PgPoolOptions;

use crate::{
    EvaluationApiState, EvaluationOutboxDispatcher, EvaluationOutboxError, FreezeCoordinator,
    FreezeCoordinatorConfiguration, FreezeCoordinatorError, PgEvaluationControlStore,
    PgFreezeCommandStore, PgFreezeStore, evaluation_api_router, serve_evaluation_plain,
};

const CONFIG_PATH: &str = "LABWEAVER_EVALUATION_CONFIG_FILE";
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EvaluationConfiguration {
    database_url_file: PathBuf,
    database_max_connections: u32,
    api_mtls: MtlsFileConfig,
    nats: NatsConfiguration,
    outbox_publish_timeout_milliseconds: u64,
    outbox_poll_interval_milliseconds: u64,
    coordinator_poll_interval_milliseconds: u64,
    coordinator: FreezeCoordinatorConfiguration,
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

pub async fn run_evaluation_service() -> Result<(), EvaluationProcessError> {
    telemetry::init("evaluation-service")?;
    let configuration: EvaluationConfiguration = read_configuration()?;
    validate_configuration(&configuration)?;
    let pool = PgPoolOptions::new()
        .max_connections(configuration.database_max_connections)
        .connect(&read_secret(&configuration.database_url_file)?)
        .await?;
    require_schema(&pool).await?;
    let nats = connect_nats(&configuration.nats).await?;
    let address = SocketAddr::from_str(&configuration.api_mtls.bind_addr)
        .map_err(|_| EvaluationProcessError::ConfigurationInvalid)?;
    let listener = tokio::net::TcpListener::bind(address).await?;
    let command_store = PgFreezeCommandStore::new(pool.clone());
    let api = evaluation_api_router(EvaluationApiState::new(
        command_store.clone(),
        PgFreezeStore::new(pool.clone()),
        PgEvaluationControlStore::new(pool.clone()),
    ));
    let coordinator = FreezeCoordinator::new(configuration.coordinator, command_store)?;
    let dispatcher = EvaluationOutboxDispatcher::new(
        pool.clone(),
        nats.clone(),
        Duration::from_millis(configuration.outbox_publish_timeout_milliseconds),
    )?;
    let poll_interval = Duration::from_millis(configuration.outbox_poll_interval_milliseconds);
    let coordinator_poll_interval =
        Duration::from_millis(configuration.coordinator_poll_interval_milliseconds);
    tracing::info!(event = "evaluation.service.started", %address);
    tokio::select! {
        result = serve_evaluation_plain(listener, api) => {
            result.map_err(EvaluationProcessError::Api)?;
        }
        result = outbox_loop(dispatcher, poll_interval) => {
            result?;
        }
        result = coordinator_loop(coordinator, coordinator_poll_interval) => {
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
         AND to_regclass('evaluation.evaluation_step_runs') IS NOT NULL",
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
}
