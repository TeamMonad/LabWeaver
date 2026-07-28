//! Bounded same-image submission freeze worker.
#![allow(
    missing_docs,
    reason = "stable diagnostics define the closed worker failure surface"
)]

use std::{
    fs,
    net::IpAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use artifact_store::{S3Credential, S3ImmutableObjectStore, S3StoreConfig};
use contracts::{Sha256Digest, UtcTimestamp};
use serde::Deserialize;
use sqlx::postgres::PgPoolOptions;

use crate::{
    CollectorLimits, FreezeRequest, FreezeService, PgFreezeStore, PvcSnapshotSource,
    SnapshotCollector, SnapshotSource, SshSnapshotConfig, SshSnapshotSource,
};

const CONFIG_PATH: &str = "LABWEAVER_EVALUATION_CONFIG_FILE";
const COMMAND_PATH: &str = "LABWEAVER_FREEZE_COMMAND_FILE";
const MAX_CONFIGURATION_BYTES: u64 = 1024 * 1024;
const PVC_WORKSPACE_ROOT: &str = "/workspace";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorkerConfiguration {
    database_url_file: PathBuf,
    database_max_connections: u32,
    object_store: S3StoreConfig,
    object_store_access_key_file: PathBuf,
    object_store_secret_key_file: PathBuf,
    object_store_session_token_file: Option<PathBuf>,
    object_prefix: String,
    worker_id: String,
    collector_limits: CollectorLimits,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FreezeWorkerCommand {
    request: FreezeRequest,
    source: FreezeWorkerSource,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum FreezeWorkerSource {
    Pvc {
        #[serde(rename = "workspaceRoot")]
        workspace_root: PathBuf,
        #[serde(rename = "sourceIdentity")]
        source_identity: Sha256Digest,
    },
    Ssh {
        host: IpAddr,
        port: u16,
        username: String,
        #[serde(rename = "workspaceRoot")]
        workspace_root: String,
        #[serde(rename = "privateKeyPath")]
        private_key_path: PathBuf,
        #[serde(rename = "certificatePath")]
        certificate_path: PathBuf,
        #[serde(rename = "expectedHostKeySha256")]
        expected_host_key_sha256: Sha256Digest,
        #[serde(rename = "sourceIdentity")]
        source_identity: Sha256Digest,
        #[serde(rename = "expiresAt")]
        expires_at: UtcTimestamp,
        #[serde(rename = "connectTimeoutMilliseconds")]
        connect_timeout_milliseconds: u64,
        #[serde(rename = "operationTimeoutMilliseconds")]
        operation_timeout_milliseconds: u64,
    },
}

/// Executes one immutable freeze command and exits after persisting its authoritative outcome.
///
/// # Errors
///
/// Returns a stable blocking diagnostic when configuration, source identity, persistence, or
/// immutable object publication cannot be verified.
pub async fn run_freeze_worker() -> Result<(), FreezeWorkerError> {
    telemetry::init("evaluation-freeze-worker")?;
    let configuration: WorkerConfiguration = read_yaml(&required_absolute_path(CONFIG_PATH)?)?;
    let command: FreezeWorkerCommand = read_yaml(&required_absolute_path(COMMAND_PATH)?)?;
    validate_configuration(&configuration)?;

    let pool = PgPoolOptions::new()
        .max_connections(configuration.database_max_connections)
        .connect(&read_secret(&configuration.database_url_file)?)
        .await?;
    let store = PgFreezeStore::new(pool);
    let authority_now = store.authority_now().await?;
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
        .await?,
    );
    let service = FreezeService::new(
        store,
        object_store,
        SnapshotCollector::new(configuration.collector_limits)?,
        &configuration.object_prefix,
        &configuration.worker_id,
    )?;
    let source: Box<dyn SnapshotSource> = match command.source {
        FreezeWorkerSource::Pvc {
            workspace_root,
            source_identity,
        } => {
            if workspace_root != Path::new(PVC_WORKSPACE_ROOT) {
                return Err(FreezeWorkerError::SourceBindingInvalid);
            }
            Box::new(PvcSnapshotSource::open(&workspace_root, source_identity)?)
        }
        FreezeWorkerSource::Ssh {
            host,
            port,
            username,
            workspace_root,
            private_key_path,
            certificate_path,
            expected_host_key_sha256,
            source_identity,
            expires_at,
            connect_timeout_milliseconds,
            operation_timeout_milliseconds,
        } => Box::new(
            SshSnapshotSource::connect(
                SshSnapshotConfig {
                    host,
                    port,
                    username,
                    workspace_root,
                    private_key_path,
                    certificate_path,
                    expected_host_key_sha256,
                    source_identity,
                    expires_at,
                    connect_timeout: bounded_duration(connect_timeout_milliseconds)?,
                    operation_timeout: bounded_duration(operation_timeout_milliseconds)?,
                },
                authority_now,
            )
            .await?,
        ),
    };
    let submission = service.freeze(&command.request, source.as_ref()).await?;
    tracing::info!(
        event = "evaluation.freeze_worker.completed",
        frozen_submission_id = %submission.id,
        environment_id = %submission.environment.environment_id,
        manifest_sha256 = %submission.manifest_sha256,
        object_sha256 = %submission.object.sha256,
    );
    Ok(())
}

fn validate_configuration(configuration: &WorkerConfiguration) -> Result<(), FreezeWorkerError> {
    if configuration.database_max_connections == 0
        || !configuration.database_url_file.is_absolute()
        || !configuration.object_store_access_key_file.is_absolute()
        || !configuration.object_store_secret_key_file.is_absolute()
        || configuration
            .object_store_session_token_file
            .as_ref()
            .is_some_and(|path| !path.is_absolute())
    {
        tracing::error!(
            event = "evaluation.freeze_worker.configuration_invalid",
            database_max_connections = configuration.database_max_connections,
            database_url_file_absolute = configuration.database_url_file.is_absolute(),
            access_key_file_absolute = configuration.object_store_access_key_file.is_absolute(),
            secret_key_file_absolute = configuration.object_store_secret_key_file.is_absolute(),
            session_token_file_absolute = configuration
                .object_store_session_token_file
                .as_ref()
                .is_none_or(|path| path.is_absolute()),
        );
        return Err(FreezeWorkerError::ConfigurationInvalid);
    }
    Ok(())
}

fn bounded_duration(milliseconds: u64) -> Result<Duration, FreezeWorkerError> {
    if !(100..=30_000).contains(&milliseconds) {
        return Err(FreezeWorkerError::SourceBindingInvalid);
    }
    Ok(Duration::from_millis(milliseconds))
}

fn required_absolute_path(name: &'static str) -> Result<PathBuf, FreezeWorkerError> {
    let value = std::env::var(name).map_err(|_| {
        tracing::error!(
            event = "evaluation.freeze_worker.configuration_missing",
            variable = name
        );
        FreezeWorkerError::ConfigurationMissing(name)
    })?;
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        tracing::error!(
            event = "evaluation.freeze_worker.configuration_path_invalid",
            variable = name
        );
        return Err(FreezeWorkerError::ConfigurationMissing(name));
    }
    Ok(path)
}

fn read_yaml<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, FreezeWorkerError> {
    let bytes = read_mounted_file(path, MAX_CONFIGURATION_BYTES)?;
    serde_yaml::from_slice(&bytes).map_err(|error| {
        tracing::error!(
            event = "evaluation.freeze_worker.configuration_parse_failed",
            path = %path.display(),
            error = %error,
        );
        FreezeWorkerError::ConfigurationInvalid
    })
}

fn read_secret(path: &Path) -> Result<String, FreezeWorkerError> {
    if !path.is_absolute() {
        return Err(FreezeWorkerError::ConfigurationInvalid);
    }
    let value = String::from_utf8(read_mounted_file(path, 16 * 1024)?)
        .map_err(|_| FreezeWorkerError::ConfigurationInvalid)?
        .trim()
        .to_owned();
    if value.is_empty() || value.chars().any(char::is_control) {
        tracing::error!(event = "evaluation.freeze_worker.secret_invalid", path = %path.display());
        return Err(FreezeWorkerError::ConfigurationInvalid);
    }
    Ok(value)
}

fn read_mounted_file(path: &Path, max_bytes: u64) -> Result<Vec<u8>, FreezeWorkerError> {
    let parent = path
        .parent()
        .ok_or(FreezeWorkerError::ConfigurationInvalid)?;
    let canonical_parent = fs::canonicalize(parent)?;
    let canonical = fs::canonicalize(path)?;
    let metadata = fs::metadata(&canonical)?;
    if !canonical.starts_with(canonical_parent)
        || !metadata.is_file()
        || metadata.len() == 0
        || metadata.len() > max_bytes
    {
        return Err(FreezeWorkerError::ConfigurationInvalid);
    }
    Ok(fs::read(canonical)?)
}

/// Stable worker startup and execution failures.
#[derive(Debug, thiserror::Error)]
pub enum FreezeWorkerError {
    #[error("LW_EVALUATION_CONFIG_MISSING: {0}")]
    ConfigurationMissing(&'static str),
    #[error("LW_EVALUATION_CONFIG_INVALID")]
    ConfigurationInvalid,
    #[error("LW_COLLECT_SOURCE_IDENTITY_MISMATCH")]
    SourceBindingInvalid,
    #[error("LW_EVALUATION_IO_FAILED")]
    Io(#[from] std::io::Error),
    #[error("LW_EVALUATION_DATABASE_FAILED")]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Telemetry(#[from] telemetry::TelemetryError),
    #[error(transparent)]
    ObjectStore(#[from] artifact_store::ObjectStoreError),
    #[error(transparent)]
    Collect(#[from] crate::CollectError),
    #[error(transparent)]
    Freeze(#[from] crate::FreezeServiceError),
    #[error(transparent)]
    Store(#[from] crate::freeze_store::FreezeStoreError),
}

impl FreezeWorkerError {
    /// Returns the bounded diagnostic that may be persisted in a Kubernetes termination message.
    #[must_use]
    pub const fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::ConfigurationMissing(_) => "LW_EVALUATION_CONFIG_MISSING",
            Self::ConfigurationInvalid => "LW_EVALUATION_CONFIG_INVALID",
            Self::SourceBindingInvalid => "LW_COLLECT_SOURCE_IDENTITY_MISMATCH",
            Self::Io(_) => "LW_EVALUATION_IO_FAILED",
            Self::Database(_) => "LW_EVALUATION_DATABASE_FAILED",
            Self::Telemetry(_) => "LW_EVALUATION_TELEMETRY_FAILED",
            Self::ObjectStore(error) => error.diagnostic_code(),
            Self::Collect(error) => error.diagnostic_code(),
            Self::Freeze(error) => error.diagnostic_code(),
            Self::Store(error) => error.diagnostic_code(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{FreezeWorkerError, FreezeWorkerSource, bounded_duration};

    #[test]
    fn worker_timeouts_are_strictly_bounded() {
        assert!(bounded_duration(100).is_ok());
        assert!(bounded_duration(30_000).is_ok());
        assert!(matches!(
            bounded_duration(99),
            Err(FreezeWorkerError::SourceBindingInvalid)
        ));
        assert!(matches!(
            bounded_duration(30_001),
            Err(FreezeWorkerError::SourceBindingInvalid)
        ));
    }

    #[test]
    fn worker_command_uses_public_camel_case_source_fields()
    -> Result<(), Box<dyn std::error::Error>> {
        let source: FreezeWorkerSource = serde_json::from_value(serde_json::json!({
            "kind": "pvc",
            "workspaceRoot": "/workspace",
            "sourceIdentity": contracts::Sha256Digest::of_bytes(b"source").to_string(),
        }))?;
        assert!(matches!(source, FreezeWorkerSource::Pvc { .. }));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn mounted_symlink_must_resolve_inside_its_binding_directory()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir()?;
        let mount = temporary.path().join("mount");
        std::fs::create_dir(&mount)?;
        let data = mount.join("..data");
        std::fs::create_dir(&data)?;
        std::fs::write(data.join("value"), b"bound")?;
        symlink("..data/value", mount.join("value"))?;
        assert_eq!(
            super::read_mounted_file(&mount.join("value"), 16)?,
            b"bound"
        );

        std::fs::write(temporary.path().join("outside"), b"escape")?;
        symlink("../outside", mount.join("escape"))?;
        assert!(matches!(
            super::read_mounted_file(&mount.join("escape"), 16),
            Err(FreezeWorkerError::ConfigurationInvalid)
        ));
        Ok(())
    }
}
