//! Shared process lifecycle for the six independently deployable service shells.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::{net::SocketAddr, str::FromStr};

use axum::http::StatusCode;
use axum::{
    Json, Router,
    extract::{Extension, Request},
    routing::get,
};
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
#[allow(
    dead_code,
    reason = "the Environment binary supplies dependency-aware readiness"
)]
pub async fn run(service: &'static str) -> Result<(), StartupError> {
    run_with_readiness(service, Arc::new(AtomicBool::new(true))).await
}

/// Starts a service with readiness supplied by its authoritative dependencies.
pub async fn run_with_readiness(
    service: &'static str,
    readiness: Arc<AtomicBool>,
) -> Result<(), StartupError> {
    run_with_router(service, readiness, Router::new()).await
}

/// Starts a service with an application router mounted alongside the health endpoints.
pub async fn run_with_router(
    service: &'static str,
    readiness: Arc<AtomicBool>,
    application: Router,
) -> Result<(), StartupError> {
    telemetry::init(service)?;
    let address = required_bind_address()?;
    let listener = tokio::net::TcpListener::bind(address).await?;
    tracing::info!(
        schema = telemetry::LOG_SCHEMA,
        event = "service.started",
        service,
        component = "process",
        operation = "service.lifecycle",
        outcome = "started",
        duration_ms = 0_u64,
    );
    axum::serve(
        listener,
        health_router(service, readiness).merge(application),
    )
    .with_graceful_shutdown(shutdown_signal(service))
    .await?;
    tracing::info!(
        schema = telemetry::LOG_SCHEMA,
        event = "service.stopped",
        service,
        component = "process",
        operation = "service.lifecycle",
        outcome = "stopped",
        duration_ms = 0_u64,
    );
    Ok(())
}

fn health_router(service: &'static str, readiness: Arc<AtomicBool>) -> Router {
    let router = Router::new()
        .route(
            "/health/live",
            get(move || async move { health(service, "live") }),
        )
        .route(
            "/health/ready",
            get(move || {
                let readiness = Arc::clone(&readiness);
                async move { ready(service, &readiness) }
            }),
        )
        .fallback(not_found);
    telemetry::instrument_http(router, service, "health-api")
}

fn ready(
    service: &'static str,
    readiness: &AtomicBool,
) -> (StatusCode, Json<contracts::HealthResponse>) {
    if readiness.load(Ordering::Acquire) {
        (StatusCode::OK, health(service, "ready"))
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            health(service, "not_ready"),
        )
    }
}

fn health(service: &'static str, status: &'static str) -> Json<contracts::HealthResponse> {
    Json(contracts::HealthResponse {
        version: contracts::API_VERSION,
        service,
        status,
    })
}

async fn not_found(
    Extension(context): Extension<telemetry::RequestContext>,
    request: Request,
) -> (StatusCode, Json<contracts::ProblemDetails>) {
    (
        StatusCode::NOT_FOUND,
        Json(contracts::ProblemDetails {
            problem_type: "urn:labweaver:problem:http-route-not-found".to_owned(),
            title: "Route not found".to_owned(),
            status: StatusCode::NOT_FOUND.as_u16(),
            detail: "The requested route is not available.".to_owned(),
            instance: request.uri().path().to_owned(),
            diagnostic_code: contracts::DiagnosticCode::registered("LW_HTTP_ROUTE_NOT_FOUND"),
            request_id: context.request_id().to_owned(),
            trace_id: Some(context.trace_id().to_owned()),
            retryable: false,
            violations: Vec::new(),
        }),
    )
}

fn required_bind_address() -> Result<SocketAddr, StartupError> {
    let value =
        std::env::var("LABWEAVER_BIND_ADDR").map_err(|_| StartupError::MissingBindAddress)?;
    SocketAddr::from_str(&value)
        .map_err(|error| StartupError::InvalidBindAddress(error.to_string()))
}

async fn shutdown_signal(service: &'static str) {
    #[cfg(unix)]
    {
        let terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate());
        let Ok(mut terminate) = terminate else {
            tracing::error!(event = "service.shutdown_signal_failed", service);
            return;
        };
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                if result.is_err() {
                    log_shutdown_signal_failure(service);
                }
            }
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    if tokio::signal::ctrl_c().await.is_err() {
        log_shutdown_signal_failure(service);
    }
}

fn log_shutdown_signal_failure(service: &'static str) {
    tracing::error!(
        schema = telemetry::LOG_SCHEMA,
        event = "service.shutdown_signal_failed",
        service,
        component = "process",
        operation = "service.shutdown",
        outcome = "failed",
        duration_ms = 0_u64,
        diagnostic_code = "LW_SERVICE_SHUTDOWN_SIGNAL_FAILED",
        error_kind = "signal_registration_failed",
        failure_stage = "service.shutdown.signal",
        retryable = false,
        safe_detail = "redacted_unclassified",
    );
}

#[cfg(test)]
mod readiness_tests {
    use std::sync::atomic::AtomicBool;

    use axum::http::StatusCode;

    use super::ready;

    #[test]
    fn readiness_fails_closed_when_authoritative_dependencies_are_unavailable() {
        let readiness = AtomicBool::new(false);
        let (status, response) = ready("environment-service", &readiness);
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.0.status, "not_ready");
    }

    #[test]
    fn readiness_is_ok_only_after_dependencies_are_ready() {
        let readiness = AtomicBool::new(true);
        let (status, response) = ready("environment-service", &readiness);
        assert_eq!(status, StatusCode::OK);
        assert_eq!(response.0.status, "ready");
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
