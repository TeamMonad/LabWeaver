//! Shared process lifecycle for the six independently deployable service shells.

use std::{net::SocketAddr, str::FromStr};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum StartupError {
    #[error("LW_CONFIG_BIND_ADDR_MISSING: LABWEAVER_BIND_ADDR is required")]
    MissingBindAddress,
    #[error("LW_CONFIG_BIND_ADDR_INVALID: {0}")]
    InvalidBindAddress(String),
    #[error("LW_TELEMETRY_INIT_FAILED: {0}")]
    Telemetry(#[from] telemetry::TelemetryError),
    #[error("LW_SERVER_BIND_FAILED: {0}")]
    Bind(#[from] std::io::Error),
}

/// Starts a service and waits for a graceful shutdown request.
///
/// # Errors
///
/// Returns a stable startup error when required configuration, telemetry initialization, or the
/// network listener cannot be established.
pub async fn run(service: &'static str) -> Result<(), StartupError> {
    telemetry::init(service)?;
    let address = required_bind_address()?;
    let listener = tokio::net::TcpListener::bind(address).await?;
    tracing::info!(event = "service.started", service, %address);
    axum::serve(listener, contracts::health_router(service))
        .with_graceful_shutdown(shutdown_signal(service))
        .await?;
    tracing::info!(event = "service.stopped", service);
    Ok(())
}

fn required_bind_address() -> Result<SocketAddr, StartupError> {
    let value =
        std::env::var("LABWEAVER_BIND_ADDR").map_err(|_| StartupError::MissingBindAddress)?;
    SocketAddr::from_str(&value)
        .map_err(|error| StartupError::InvalidBindAddress(error.to_string()))
}

async fn shutdown_signal(service: &'static str) {
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::error!(event = "service.shutdown_signal_failed", service, %error);
    }
}

#[cfg(test)]
mod tests {
    use super::StartupError;

    #[test]
    fn missing_binding_is_a_stable_blocking_diagnostic() {
        assert!(
            StartupError::MissingBindAddress
                .to_string()
                .starts_with("LW_CONFIG_BIND_ADDR_MISSING")
        );
    }
}
