//! Resource Service process entry point.

#[path = "../../service_runtime.rs"]
mod service_runtime;

#[derive(Debug, thiserror::Error)]
enum MainError {
    #[error(transparent)]
    Runtime(#[from] resource_service::ResourceProcessRuntimeError),
    #[error(transparent)]
    Startup(#[from] service_runtime::StartupError),
}

#[tokio::main]
async fn main() -> Result<(), MainError> {
    let runtime = resource_service::ResourceProcessRuntime::from_env().await?;
    let readiness = runtime.readiness();
    tokio::spawn(async move {
        if let Err(error) = runtime.run().await {
            tracing::error!(event = "resource.runtime.stopped", error = %error);
        }
    });
    service_runtime::run_with_readiness(env!("CARGO_PKG_NAME"), readiness).await?;
    Ok(())
}
