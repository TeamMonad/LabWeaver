//! Shared process lifecycle for the six independently deployable service shells.

use std::{net::SocketAddr, str::FromStr};

use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::{
    Json, Router,
    extract::{Extension, Request},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::get,
};
use thiserror::Error;

const REQUEST_ID_HEADER: &str = "x-request-id";

#[derive(Clone)]
struct RequestIdentity(String);

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
    axum::serve(listener, health_router(service))
        .with_graceful_shutdown(shutdown_signal(service))
        .await?;
    tracing::info!(event = "service.stopped", service);
    Ok(())
}

fn health_router(service: &'static str) -> Router {
    Router::new()
        .route(
            "/health/live",
            get(move || async move { health(service, "live") }),
        )
        .route(
            "/health/ready",
            get(move || async move { health(service, "ready") }),
        )
        .fallback(not_found)
        .layer(axum::middleware::from_fn(request_id))
}

fn health(service: &'static str, status: &'static str) -> Json<contracts::HealthResponse> {
    Json(contracts::HealthResponse {
        version: contracts::API_VERSION,
        service,
        status,
    })
}

async fn not_found(
    Extension(RequestIdentity(request_id)): Extension<RequestIdentity>,
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
            request_id,
            trace_id: None,
            retryable: false,
            violations: Vec::new(),
        }),
    )
}

async fn request_id(mut request: Request, next: Next) -> Response {
    let header_name = HeaderName::from_static(REQUEST_ID_HEADER);
    let request_id_text = if let Some(value) = request.headers().get(&header_name) {
        let Ok(value) = value.to_str() else {
            tracing::warn!(event = "service.request_id_rejected", reason = "non_ascii");
            return invalid_request_id().into_response();
        };
        if !(8..=128).contains(&value.len())
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
        {
            tracing::warn!(
                event = "service.request_id_rejected",
                reason = "invalid_syntax"
            );
            return invalid_request_id().into_response();
        }
        value.to_owned()
    } else {
        contracts::EventId::new().to_string()
    };
    let Ok(request_id) = HeaderValue::from_str(&request_id_text) else {
        tracing::error!(event = "service.request_id_encoding_failed");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    request
        .headers_mut()
        .insert(header_name.clone(), request_id.clone());
    request
        .extensions_mut()
        .insert(RequestIdentity(request_id_text));
    let mut response = next.run(request).await;
    response.headers_mut().insert(header_name, request_id);
    response
}

fn invalid_request_id() -> (StatusCode, Json<contracts::ProblemDetails>) {
    (
        StatusCode::BAD_REQUEST,
        Json(contracts::ProblemDetails {
            problem_type: "urn:labweaver:problem:invalid-request-id".to_owned(),
            title: "Invalid request identity".to_owned(),
            status: StatusCode::BAD_REQUEST.as_u16(),
            detail: "x-request-id must be 8-128 portable ASCII characters.".to_owned(),
            instance: String::new(),
            diagnostic_code: contracts::DiagnosticCode::registered("LW_HTTP_REQUEST_ID_INVALID"),
            request_id: contracts::EventId::new().to_string(),
            trace_id: None,
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
