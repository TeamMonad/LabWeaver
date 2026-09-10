//! Resource Service process entry point.

#[path = "../../http_transport.rs"]
mod http_transport;
#[path = "../../service_runtime.rs"]
mod service_runtime;

use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
enum MainError {
    #[error(transparent)]
    Runtime(#[from] resource_service::ResourceProcessRuntimeError),
    #[error(transparent)]
    Startup(#[from] service_runtime::StartupError),
    #[error("LW_RESOURCE_RUNTIME_TASK_FAILED: {0}")]
    RuntimeTask(#[from] tokio::task::JoinError),
    #[error("LW_RESOURCE_HTTP_CONFIGURATION_INVALID")]
    Configuration,
    #[error("LW_RESOURCE_HTTP_LISTENER_FAILED")]
    Listener(#[from] std::io::Error),
    #[error(transparent)]
    HttpTransport(#[from] http_transport::HttpTransportError),
}

#[tokio::main]
async fn main() -> Result<(), MainError> {
    let runtime = resource_service::ResourceProcessRuntime::from_env().await?;
    let readiness = runtime.readiness();
    let api = resource_service::api::resource_api_router(runtime.api_state());
    let http = load_http_config().await?;
    let api = resource_service::api::with_delegation(api, http.delegation_key);
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
        result = http_transport::serve_tls(http.listener, api, http.tls) => {
            result?;
            Ok(())
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResourceHttpFileConfig {
    bind_addr: String,
    server_certificate_file: String,
    server_key_file: String,
    delegation_key_file: String,
}

struct ResourceHttpConfig {
    listener: tokio::net::TcpListener,
    tls: std::sync::Arc<rustls::ServerConfig>,
    delegation_key: std::sync::Arc<Vec<u8>>,
}

async fn load_http_config() -> Result<ResourceHttpConfig, MainError> {
    let path = std::env::var("LABWEAVER_RESOURCE_HTTP_CONFIG_FILE")
        .map_err(|_| MainError::Configuration)?;
    if !std::path::Path::new(&path).is_absolute() {
        return Err(MainError::Configuration);
    }
    let content = std::fs::read_to_string(path).map_err(|_| MainError::Configuration)?;
    let config: ResourceHttpFileConfig =
        serde_yaml::from_str(&content).map_err(|_| MainError::Configuration)?;
    if !std::path::Path::new(&config.server_certificate_file).is_absolute()
        || !std::path::Path::new(&config.server_key_file).is_absolute()
        || !std::path::Path::new(&config.delegation_key_file).is_absolute()
    {
        return Err(MainError::Configuration);
    }
    let bind = config
        .bind_addr
        .parse::<std::net::SocketAddr>()
        .map_err(|_| MainError::Configuration)?;
    let tls = http_transport::load_server_config(
        &config.server_certificate_file,
        &config.server_key_file,
    )?;
    let delegation_key =
        std::fs::read(&config.delegation_key_file).map_err(|_| MainError::Configuration)?;
    if delegation_key.len() < 32 {
        return Err(MainError::Configuration);
    }
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(MainError::Listener)?;
    Ok(ResourceHttpConfig {
        listener,
        tls,
        delegation_key: std::sync::Arc::new(delegation_key),
    })
}
