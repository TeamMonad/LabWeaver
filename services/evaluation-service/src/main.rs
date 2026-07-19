//! Evaluation Service process entry point.

#[path = "../../service_runtime.rs"]
mod service_runtime;

#[tokio::main]
async fn main() -> Result<(), MainError> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if arguments == ["--mode", "freeze-worker"] {
        evaluation_service::run_freeze_worker().await?;
        return Ok(());
    }
    if !arguments.is_empty() {
        return Err(MainError::Arguments);
    }
    service_runtime::run(env!("CARGO_PKG_NAME")).await?;
    Ok(())
}

#[derive(Debug, thiserror::Error)]
enum MainError {
    #[error("LW_EVALUATION_MODE_INVALID")]
    Arguments,
    #[error(transparent)]
    Startup(#[from] service_runtime::StartupError),
    #[error(transparent)]
    Worker(#[from] evaluation_service::FreezeWorkerError),
}
