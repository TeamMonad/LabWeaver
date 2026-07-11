//! Environment Service process entry point.

#[path = "../../service_runtime.rs"]
mod service_runtime;

#[tokio::main]
async fn main() -> Result<(), service_runtime::StartupError> {
    service_runtime::run(env!("CARGO_PKG_NAME")).await
}
