//! Environment Service process entry point.

#[path = "../../service_runtime.rs"]
mod service_runtime;

#[tokio::main]
async fn main() -> Result<(), MainError> {
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

#[derive(Debug, thiserror::Error)]
enum MainError {
    #[error(transparent)]
    Service(#[from] service_runtime::StartupError),
    #[error(transparent)]
    Resolver(#[from] environment_service::OwnerResolverRuntimeError),
    #[error(transparent)]
    Process(#[from] environment_service::EnvironmentProcessRuntimeError),
}
