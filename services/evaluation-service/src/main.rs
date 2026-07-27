//! Evaluation Service process entry point.

#[tokio::main]
async fn main() -> Result<(), MainError> {
    let result = run().await;
    if let Err(error) = &result {
        write_termination_diagnostic(error.diagnostic_code());
    }
    result
}

async fn run() -> Result<(), MainError> {
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

fn write_termination_diagnostic(diagnostic: &'static str) {
    const TERMINATION_LOG: &str = "/dev/termination-log";
    if let Err(error) = std::fs::write(TERMINATION_LOG, diagnostic) {
        eprintln!("LW_EVALUATION_TERMINATION_LOG_FAILED: {error}");
    }
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

impl MainError {
    const fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::Arguments => "LW_EVALUATION_MODE_INVALID",
            Self::Worker(error) => error.diagnostic_code(),
            Self::Process(_) => "LW_EVALUATION_PROCESS_FAILED",
        }
    }
}
