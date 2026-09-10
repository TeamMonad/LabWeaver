//! Environment owner process and deployment-owned runtime executor entry point.

use std::{collections::BTreeSet, sync::Arc};

use artifact_store::{S3Credential, S3ImmutableObjectStore, S3StoreConfig};
use auth::{ServiceAuthConfig, ServiceAuthError, ServiceTokenVerifier, TransportSecurityMode};
use environment_service::{
    FencedContainerExecutor, FencedKubeVirtExecutor, KubeVirtConsoleExecutorServerConfig,
    KubeVirtConsoleKubernetesConfiguration, KubernetesContainerExecutor,
    NatsContainerExecutorServer, NatsKubeVirtExecutorServer, PgContainerExecutorFenceStore,
    PgKubeVirtExecutorFenceStore, RuntimeExecutorConfiguration, TerminalExecutorServerConfig,
    connect_nats_mtls,
};
use serde::Deserialize;
use sqlx::postgres::PgPoolOptions;

#[path = "../../service_runtime.rs"]
mod service_runtime;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RuntimeExecutorDeployment {
    database_url_file: String,
    database_max_connections: u32,
    object_store: S3StoreConfig,
    object_store_access_key_file: String,
    object_store_secret_key_file: String,
    object_store_session_token_file: Option<String>,
    nats: RuntimeExecutorNats,
    executor: RuntimeExecutorConfiguration,
    terminal: Option<TerminalExecutorServerConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RuntimeExecutorNats {
    server: String,
    ca_file: String,
    client_certificate_file: String,
    client_private_key_file: String,
    credentials_file: String,
    container_request_subject: String,
    kubevirt_request_subject: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct KubeVirtConsoleDeployment {
    database_url_file: String,
    database_max_connections: u32,
    kubernetes: KubeVirtConsoleKubernetesConfiguration,
    server: KubeVirtConsoleExecutorServerConfig,
}

#[tokio::main]
async fn main() -> Result<(), MainError> {
    let mut arguments = std::env::args().skip(1);
    if arguments.next().as_deref() != Some("--mode") || arguments.size_hint().0 != 1 {
        return Err(MainError::Configuration);
    }
    match arguments.next().as_deref() {
        Some("environment-service") => run_environment_service().await,
        Some("container-executor") => run_runtime_executor(RuntimeKind::Container).await,
        Some("kubevirt-executor") => run_runtime_executor(RuntimeKind::KubeVirt).await,
        Some("kubevirt-console-executor") => run_kubevirt_console_executor().await,
        _ => Err(MainError::Configuration),
    }
}

async fn run_kubevirt_console_executor() -> Result<(), MainError> {
    let path = std::env::var("LABWEAVER_KUBEVIRT_CONSOLE_CONFIG_FILE")
        .map_err(|_| MainError::Configuration)?;
    let deployment: KubeVirtConsoleDeployment =
        serde_yaml::from_str(&std::fs::read_to_string(path)?)
            .map_err(|_| MainError::Configuration)?;
    if deployment.database_max_connections == 0 || deployment.database_max_connections > 8 {
        return Err(MainError::Configuration);
    }
    let pool = PgPoolOptions::new()
        .max_connections(deployment.database_max_connections)
        .connect(&read_secret(&deployment.database_url_file)?)
        .await?;
    let schema_ready: bool = sqlx::query_scalar(
        "SELECT to_regclass('environment.environment_instances') IS NOT NULL \
         AND to_regclass('environment.kubevirt_runtime_observations') IS NOT NULL",
    )
    .fetch_one(&pool)
    .await?;
    if !schema_ready {
        return Err(MainError::SchemaUnavailable);
    }
    let service_verifier = discover_service_verifier().await?;
    let server = environment_service::KubeVirtConsoleExecutorServer::new(
        &deployment.server,
        &deployment.kubernetes,
        pool,
        service_verifier,
    )
    .await?;
    tokio::try_join!(
        async {
            server
                .serve()
                .await
                .map_err(MainError::KubeVirtConsoleExecutor)
        },
        async {
            service_runtime::run("kubevirt-console-executor")
                .await
                .map_err(MainError::Service)
        }
    )?;
    Ok(())
}

#[derive(Clone, Copy)]
enum RuntimeKind {
    Container,
    KubeVirt,
}

async fn run_environment_service() -> Result<(), MainError> {
    let process = environment_service::EnvironmentProcessRuntime::from_env().await?;
    let readiness = process.readiness();
    let owner_resolver =
        environment_service::OwnerResolverRuntime::from_env(process.api_state()).await?;
    tokio::try_join!(
        async {
            service_runtime::run_with_readiness(env!("CARGO_PKG_NAME"), readiness)
                .await
                .map_err(MainError::Service)
        },
        async { owner_resolver.serve().await.map_err(MainError::Resolver) },
        async { Box::pin(process.serve()).await.map_err(MainError::Process) },
    )?;
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "executor startup keeps dependency and server bindings visible together"
)]
async fn run_runtime_executor(kind: RuntimeKind) -> Result<(), MainError> {
    let deployment = load_runtime_executor_deployment()?;
    if deployment.database_max_connections == 0 || deployment.database_max_connections > 32 {
        return Err(MainError::Configuration);
    }
    let pool = PgPoolOptions::new()
        .max_connections(deployment.database_max_connections)
        .connect(&read_secret(&deployment.database_url_file)?)
        .await?;
    let required_table = match kind {
        RuntimeKind::Container => "environment.container_executor_fences",
        RuntimeKind::KubeVirt => "environment.kubevirt_executor_fences",
    };
    let schema_ready: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
        .bind(required_table)
        .fetch_one(&pool)
        .await?;
    if !schema_ready {
        return Err(MainError::SchemaUnavailable);
    }
    let objects = Arc::new(
        S3ImmutableObjectStore::new(
            deployment.object_store,
            S3Credential {
                access_key_id: read_secret(&deployment.object_store_access_key_file)?,
                secret_access_key: read_secret(&deployment.object_store_secret_key_file)?,
                session_token: deployment
                    .object_store_session_token_file
                    .as_deref()
                    .map(read_secret)
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
    let service_verifier = if matches!(kind, RuntimeKind::Container) {
        Some(discover_service_verifier().await?)
    } else {
        None
    };
    let terminal = match kind {
        RuntimeKind::Container => Some(
            environment_service::TerminalExecutorServer::new(
                deployment
                    .terminal
                    .as_ref()
                    .ok_or(MainError::Configuration)?,
                &deployment.executor,
                service_verifier.clone().ok_or(MainError::Configuration)?,
            )
            .await?,
        ),
        RuntimeKind::KubeVirt => None,
    };
    let backend = KubernetesContainerExecutor::new(deployment.executor, objects)
        .map_err(|_| MainError::Configuration)?;
    match kind {
        RuntimeKind::Container => {
            let terminal = terminal.ok_or(MainError::Configuration)?;
            let executor =
                FencedContainerExecutor::new(PgContainerExecutorFenceStore::new(pool), backend);
            let server = NatsContainerExecutorServer::new(
                nats,
                deployment.nats.container_request_subject,
                executor,
            )?;
            tokio::try_join!(
                async { server.serve().await.map_err(MainError::Executor) },
                async { terminal.serve().await.map_err(MainError::TerminalExecutor) },
                async {
                    service_runtime::run("container-executor")
                        .await
                        .map_err(MainError::Service)
                }
            )?;
        }
        RuntimeKind::KubeVirt => {
            let executor =
                FencedKubeVirtExecutor::new(PgKubeVirtExecutorFenceStore::new(pool), backend);
            let server = NatsKubeVirtExecutorServer::new(
                nats,
                deployment.nats.kubevirt_request_subject,
                executor,
            )?;
            tokio::try_join!(
                async { server.serve().await.map_err(MainError::KubeVirtExecutor) },
                async {
                    service_runtime::run("kubevirt-executor")
                        .await
                        .map_err(MainError::Service)
                }
            )?;
        }
    }
    Ok(())
}

fn load_runtime_executor_deployment() -> Result<RuntimeExecutorDeployment, MainError> {
    let path = std::env::var("LABWEAVER_RUNTIME_EXECUTOR_CONFIG_FILE")
        .map_err(|_| MainError::Configuration)?;
    serde_yaml::from_str(&std::fs::read_to_string(path)?).map_err(|_| MainError::Configuration)
}

async fn discover_service_verifier() -> Result<Arc<ServiceTokenVerifier>, MainError> {
    let issuer = required_env("LABWEAVER_SERVICE_OIDC_ISSUER")?;
    let audience = required_env("LABWEAVER_SERVICE_AUDIENCE")?;
    let allowed_client_ids = required_set("LABWEAVER_SERVICE_ALLOWED_CLIENT_IDS")?;
    let algorithms = required_set("LABWEAVER_SERVICE_JWT_ALGORITHMS")?;
    let jwks_refresh_seconds = required_u64("LABWEAVER_SERVICE_JWKS_REFRESH_SECONDS")?;
    let jwks_retry_seconds = required_u64("LABWEAVER_SERVICE_JWKS_RETRY_SECONDS")?;
    let ca_path = required_path("LABWEAVER_SERVICE_OIDC_CA")?;
    let ca = std::fs::read(ca_path)?;
    let http = auth::no_redirect_http_client(Some(&ca), TransportSecurityMode::Strict)
        .map_err(|_| MainError::ServiceAuth(ServiceAuthError::HttpClient))?;
    let config = ServiceAuthConfig::new(
        &issuer,
        audience,
        allowed_client_ids,
        BTreeSet::new(),
        algorithms,
        jwks_refresh_seconds,
        jwks_retry_seconds,
        TransportSecurityMode::Strict,
    )
    .map_err(|_| MainError::ServiceAuth(ServiceAuthError::InvalidConfig))?;
    ServiceTokenVerifier::discover(config, http)
        .await
        .map(Arc::new)
        .map_err(MainError::ServiceAuth)
}

fn required_env(name: &'static str) -> Result<String, MainError> {
    let value = std::env::var(name).map_err(|_| MainError::Configuration)?;
    if value.trim().is_empty() {
        return Err(MainError::Configuration);
    }
    Ok(value)
}

fn required_path(name: &'static str) -> Result<std::path::PathBuf, MainError> {
    let path = std::path::PathBuf::from(required_env(name)?);
    if !path.is_absolute() {
        return Err(MainError::Configuration);
    }
    Ok(path)
}

fn required_set(name: &'static str) -> Result<BTreeSet<String>, MainError> {
    let values = required_env(name)?
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    if values.is_empty() {
        return Err(MainError::Configuration);
    }
    Ok(values)
}

fn required_u64(name: &'static str) -> Result<u64, MainError> {
    let value = required_env(name)?
        .parse::<u64>()
        .map_err(|_| MainError::Configuration)?;
    if value == 0 {
        return Err(MainError::Configuration);
    }
    Ok(value)
}

fn read_secret(path: &str) -> Result<String, MainError> {
    let value = std::fs::read_to_string(path)?;
    let value = value.trim();
    if value.is_empty() || value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(MainError::Configuration);
    }
    Ok(value.to_owned())
}

#[derive(Debug, thiserror::Error)]
enum MainError {
    #[error("LW_ENVIRONMENT_CONFIG_INVALID")]
    Configuration,
    #[error("LW_ENVIRONMENT_SCHEMA_UNAVAILABLE")]
    SchemaUnavailable,
    #[error(transparent)]
    Service(#[from] service_runtime::StartupError),
    #[error(transparent)]
    Resolver(#[from] environment_service::OwnerResolverRuntimeError),
    #[error(transparent)]
    Process(#[from] environment_service::EnvironmentProcessRuntimeError),
    #[error(transparent)]
    Nats(#[from] environment_service::NatsMessagingError),
    #[error(transparent)]
    Executor(#[from] environment_service::ContainerExecutorFenceError),
    #[error(transparent)]
    KubeVirtExecutor(#[from] environment_service::KubeVirtExecutorFenceError),
    #[error(transparent)]
    TerminalExecutor(#[from] environment_service::TerminalExecutorServerError),
    #[error(transparent)]
    KubeVirtConsoleExecutor(#[from] environment_service::KubeVirtConsoleExecutorServerError),
    #[error(transparent)]
    Store(#[from] artifact_store::ObjectStoreError),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    ServiceAuth(#[from] ServiceAuthError),
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::case_sensitive_file_extension_comparisons)]
mod deployment_contract_tests {
    use super::{KubeVirtConsoleDeployment, RuntimeExecutorDeployment};

    #[test]
    fn checked_in_runtime_executor_example_matches_the_v1_contract() {
        let example = include_str!("../../../deploy/config/runtime-executor.yaml.example");
        let deployment: RuntimeExecutorDeployment = serde_yaml::from_str(example)
            .expect("runtime executor deployment example must deserialize");

        assert!(deployment.database_url_file.starts_with('/'));
        assert!(deployment.nats.server.starts_with("tls://"));
        assert!(deployment.nats.container_request_subject.ends_with(".v1"));
        assert!(deployment.nats.kubevirt_request_subject.ends_with(".v1"));
    }

    #[test]
    fn checked_in_kubevirt_console_example_matches_the_v1_contract() {
        let example = include_str!("../../../deploy/config/kubevirt-console-executor.yaml.example");
        let deployment: KubeVirtConsoleDeployment = serde_yaml::from_str(example)
            .expect("KubeVirt console deployment example must deserialize");

        assert!(deployment.database_url_file.starts_with('/'));
        assert_eq!(deployment.server.bind_addr, "0.0.0.0:9451");
    }
}
