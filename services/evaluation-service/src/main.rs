//! Evaluation Service process entry point.

#[tokio::main]
async fn main() -> Result<(), MainError> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    match arguments.as_slice() {
        [mode, value] if mode == "--mode" && value == "freeze-worker" => {
            evaluation_service::run_freeze_worker().await?;
            return Ok(());
        }
        [] => {
            evaluation_service::run_evaluation_service().await?;
            return Ok(());
        }
        [mode, value] if mode == "--mode" && value == "evaluation-service" => {
            evaluation_service::run_evaluation_service().await?;
            return Ok(());
        }
        _ => {}
    }
    Err(MainError::Arguments)
}

#[derive(Debug, thiserror::Error)]
enum MainError {
    #[error("LW_EVALUATION_MODE_INVALID")]
    Arguments,
    #[error(transparent)]
    Worker(#[from] evaluation_service::FreezeWorkerError),
    #[error(transparent)]
    Process(#[from] evaluation_service::EvaluationProcessError),
}
