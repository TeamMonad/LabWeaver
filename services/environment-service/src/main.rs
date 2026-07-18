//! Environment owner process and deployment-owned runtime executor entry point.

use std::sync::Arc;

use artifact_store::{S3Credential, S3ImmutableObjectStore, S3StoreConfig};
use environment_service::{
    FencedContainerExecutor, FencedKubeVirtExecutor, KubernetesContainerExecutor,
    NatsContainerExecutorServer, NatsKubeVirtExecutorServer, PgContainerExecutorFenceStore,
    PgKubeVirtExecutorFenceStore, RuntimeExecutorConfiguration, connect_nats_mtls,
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
        _ => Err(MainError::Configuration),
    }
}

#[derive(Clone, Copy)]
enum RuntimeKind {
    Container,
    KubeVirt,
}

async fn run_environment_service() -> Result<(), MainError> {
    let process = environment_service::EnvironmentProcessRuntime::from_env().await?;
    let readiness = process.readiness();
    let owner_resolver = environment_service::OwnerResolverRuntime::from_env().await?;
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
    let backend = KubernetesContainerExecutor::new(deployment.executor, objects)
        .map_err(|_| MainError::Configuration)?;
    match kind {
        RuntimeKind::Container => {
            let executor =
                FencedContainerExecutor::new(PgContainerExecutorFenceStore::new(pool), backend);
            let server = NatsContainerExecutorServer::new(
                nats,
                deployment.nats.container_request_subject,
                executor,
            )?;
            tokio::try_join!(
                async { server.serve().await.map_err(MainError::Executor) },
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
    Store(#[from] artifact_store::ObjectStoreError),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
