//! Evaluation Service process entry point.

#[tokio::main]
async fn main() -> Result<(), MainError> {
    let helper_mode = is_oj_helper_mode();
    let result = Box::pin(run()).await;
    if let Err(error) = &result {
        write_termination_diagnostic(error.diagnostic_code());
        if helper_mode {
            std::process::exit(evaluation_service::OJ_HELPER_FAILURE_EXIT_CODE);
        }
    }
    result
}

fn is_oj_helper_mode() -> bool {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    arguments.first().is_some_and(|mode| mode == "--mode")
        && arguments
            .get(1)
            .is_some_and(|value| value == "oj-compile-exec" || value == "oj-case-exec")
}

async fn run() -> Result<(), MainError> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    match arguments.as_slice() {
        [mode, value] if mode == "--mode" && value == "freeze-worker" => {
            evaluation_service::run_freeze_worker().await?;
            return Ok(());
        }
        [mode, value] if mode == "--mode" && value == "oj-worker" => {
            let receipt = evaluation_service::run_oj_worker().await?;
            write_termination_receipt(&receipt)?;
            return Ok(());
        }
        [mode, value] if mode == "--mode" && value == "ansible-probe-worker" => {
            let receipt = evaluation_service::run_ansible_probe_worker().await?;
            write_termination_receipt(&receipt)?;
            return Ok(());
        }
        [mode, value] if mode == "--mode" && value == "artifact-materializer" => {
            evaluation_service::run_artifact_materializer().await?;
            return Ok(());
        }
        [mode, value] if mode == "--mode" && value == "oj-compile-exec" => {
            evaluation_service::run_oj_compile_exec()?;
            return Ok(());
        }
        [
            mode,
            value,
            memory_flag,
            memory,
            cpu_flag,
            cpu,
            file_flag,
            file,
        ] if mode == "--mode"
            && value == "oj-case-exec"
            && memory_flag == "--memory-bytes"
            && cpu_flag == "--cpu-seconds"
            && file_flag == "--file-bytes" =>
        {
            let memory = parse_limit(memory)?;
            let cpu = parse_limit(cpu)?;
            let file = parse_limit(file)?;
            evaluation_service::run_oj_case_exec(memory, cpu, file)?;
            return Ok(());
        }
        [] => {
            Box::pin(evaluation_service::run_evaluation_service()).await?;
            return Ok(());
        }
        [mode, value] if mode == "--mode" && value == "evaluation-service" => {
            Box::pin(evaluation_service::run_evaluation_service()).await?;
            return Ok(());
        }
        _ => {}
    }
    Err(MainError::Arguments)
}

fn parse_limit(value: &str) -> Result<u64, MainError> {
    value.parse().map_err(|_| MainError::Arguments)
}

fn write_termination_diagnostic(diagnostic: &'static str) {
    const TERMINATION_LOG: &str = "/dev/termination-log";
    if let Err(error) = std::fs::write(TERMINATION_LOG, diagnostic) {
        eprintln!("LW_EVALUATION_TERMINATION_LOG_FAILED: {error}");
    }
}

fn write_termination_receipt(receipt: &impl serde::Serialize) -> Result<(), MainError> {
    const TERMINATION_LOG: &str = "/dev/termination-log";
    const MAX_TERMINATION_MESSAGE_BYTES: usize = 4096;
    let bytes = serde_json::to_vec(receipt).map_err(|_| MainError::Receipt)?;
    if bytes.is_empty() || bytes.len() > MAX_TERMINATION_MESSAGE_BYTES {
        return Err(MainError::Receipt);
    }
    std::fs::write(TERMINATION_LOG, bytes).map_err(|_| MainError::Receipt)
}

#[derive(Debug, thiserror::Error)]
enum MainError {
    #[error("LW_EVALUATION_MODE_INVALID")]
    Arguments,
    #[error(transparent)]
    Worker(#[from] evaluation_service::FreezeWorkerError),
    #[error(transparent)]
    OjWorker(#[from] evaluation_service::OjWorkerError),
    #[error(transparent)]
    AnsibleProbeWorker(#[from] evaluation_service::AnsibleProbeWorkerError),
    #[error(transparent)]
    Materializer(#[from] evaluation_service::MaterializerError),
    #[error(transparent)]
    Process(#[from] evaluation_service::EvaluationProcessError),
    #[error("LW_OJ_RECEIPT_WRITE_FAILED")]
    Receipt,
}

impl MainError {
    const fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::Arguments => "LW_EVALUATION_MODE_INVALID",
            Self::Worker(error) => error.diagnostic_code(),
            Self::OjWorker(error) => error.diagnostic_code(),
            Self::AnsibleProbeWorker(error) => error.diagnostic_code(),
            Self::Materializer(error) => error.diagnostic_code(),
            Self::Process(_) => "LW_EVALUATION_PROCESS_FAILED",
            Self::Receipt => "LW_OJ_RECEIPT_WRITE_FAILED",
        }
    }
}
