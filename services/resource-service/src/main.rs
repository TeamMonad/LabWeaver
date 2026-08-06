//! Resource Service process entry point.

#[path = "../../service_runtime.rs"]
mod service_runtime;

#[derive(Debug, thiserror::Error)]
enum MainError {
    #[error(transparent)]
    Runtime(#[from] resource_service::ResourceProcessRuntimeError),
    #[error(transparent)]
    Startup(#[from] service_runtime::StartupError),
    #[error("LW_RESOURCE_RUNTIME_TASK_FAILED: {0}")]
    RuntimeTask(#[from] tokio::task::JoinError),
    #[error("LW_RESOURCE_MTLS_CONFIGURATION_INVALID")]
    Configuration,
    #[error("LW_RESOURCE_MTLS_LISTENER_FAILED")]
    Listener(#[from] std::io::Error),
}

#[tokio::main]
async fn main() -> Result<(), MainError> {
    let runtime = resource_service::ResourceProcessRuntime::from_env().await?;
    let readiness = runtime.readiness();
    let api = resource_service::api::resource_api_router(runtime.api_state());
    let mtls = load_mtls_config().await?;
    let runtime_task = tokio::spawn(runtime.run());
    tokio::select! {
        result = runtime_task => {
            result??;
            Ok(())
        }
        result = service_runtime::run_with_router(env!("CARGO_PKG_NAME"), readiness, axum::Router::new()) => {
            result?;
            Ok(())
        }
        result = resource_service::api::serve_mtls(
            mtls.listener,
            api,
            mtls.server,
            mtls.delegation_key,
        ) => {
            result?;
            Ok(())
        }
    }
}

struct ResourceMtlsConfig {
    listener: tokio::net::TcpListener,
    server: auth::MtlsServerConfig,
    delegation_key: std::sync::Arc<Vec<u8>>,
}

async fn load_mtls_config() -> Result<ResourceMtlsConfig, MainError> {
    let path = std::env::var("LABWEAVER_RESOURCE_MTLS_CONFIG_FILE")
        .map_err(|_| MainError::Configuration)?;
    if !std::path::Path::new(&path).is_absolute() {
        return Err(MainError::Configuration);
    }
    let content = std::fs::read_to_string(path).map_err(|_| MainError::Configuration)?;
    let config: auth::MtlsFileConfig =
        serde_yaml::from_str(&content).map_err(|_| MainError::Configuration)?;
    if [
        config.server_certificate_file.as_str(),
        config.server_key_file.as_str(),
        config.client_ca_file.as_str(),
        config
            .delegation_key_file
            .as_deref()
            .ok_or(MainError::Configuration)?,
    ]
    .iter()
    .any(|file| !std::path::Path::new(file).is_absolute())
    {
        return Err(MainError::Configuration);
    }
    let bind = config
        .bind_addr
        .parse::<std::net::SocketAddr>()
        .map_err(|_| MainError::Configuration)?;
    let server = auth::load_mtls_server_config(&config).map_err(|_| MainError::Configuration)?;
    let delegation_key_file = config
        .delegation_key_file
        .as_deref()
        .ok_or(MainError::Configuration)?;
    let delegation_key =
        std::fs::read(delegation_key_file).map_err(|_| MainError::Configuration)?;
    if delegation_key.len() < 32 {
        return Err(MainError::Configuration);
    }
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(MainError::Listener)?;
    Ok(ResourceMtlsConfig {
        listener,
        server,
        delegation_key: std::sync::Arc::new(delegation_key),
    })
}
