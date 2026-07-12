//! Structured, JSON-only telemetry initialization for service processes.

use thiserror::Error;
use tracing_subscriber::EnvFilter;

/// Telemetry initialization failure.
#[derive(Debug, Error)]
pub enum TelemetryError {
    /// The log filter is invalid.
    #[error("LW_CONFIG_LOG_FILTER_INVALID: {0}")]
    InvalidFilter(String),
    /// A global subscriber was already installed.
    #[error("LW_TELEMETRY_ALREADY_INITIALIZED: {0}")]
    AlreadyInitialized(String),
}

/// Installs a JSON subscriber without logging request bodies or secrets.
///
/// # Errors
///
/// Returns a stable configuration or initialization error when the filter is invalid or another
/// global subscriber has already been installed.
pub fn init(service: &'static str) -> Result<(), TelemetryError> {
    let filter = std::env::var("LABWEAVER_LOG_FILTER").unwrap_or_else(|_| "info".to_owned());
    let filter = EnvFilter::try_new(filter)
        .map_err(|error| TelemetryError::InvalidFilter(error.to_string()))?;
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
        .with_target(true)
        .with_current_span(true)
        .with_span_list(true)
        .with_writer(std::io::stderr)
        .try_init()
        .map_err(|error| TelemetryError::AlreadyInitialized(format!("{service}: {error}")))
}
