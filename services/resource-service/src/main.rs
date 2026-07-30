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
}

#[tokio::main]
async fn main() -> Result<(), MainError> {
    let runtime = resource_service::ResourceProcessRuntime::from_env().await?;
    let readiness = runtime.readiness();
    let api = resource_service::api::resource_api_router(runtime.api_state());
    let runtime_task = tokio::spawn(runtime.run());
    tokio::select! {
        result = runtime_task => {
            result??;
            Ok(())
        }
        result = service_runtime::run_with_router(env!("CARGO_PKG_NAME"), readiness, api) => {
            result?;
            Ok(())
        }
    }
}
