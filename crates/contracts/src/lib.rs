//! Versioned HTTP contract primitives and the initial health surface.

use axum::{Json, Router, extract::Request, middleware::Next, response::Response, routing::get};
use http::{HeaderName, HeaderValue, StatusCode};
use serde::Serialize;
use uuid::Uuid;

const REQUEST_ID_HEADER: &str = "x-request-id";

/// Stable machine-readable health response.
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    /// Contract version.
    pub version: &'static str,
    /// Stable service identifier.
    pub service: &'static str,
    /// Current health state.
    pub status: &'static str,
}

/// Stable public error envelope.
#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    /// Contract version.
    pub version: &'static str,
    /// Stable diagnostic code.
    pub diagnostic_code: &'static str,
    /// Safe client-facing message.
    pub message: &'static str,
    /// Request correlation identifier.
    pub request_id: String,
}

/// Builds the only public routes exposed by the Day 1 service shell.
pub fn health_router(service: &'static str) -> Router {
    Router::new()
        .route(
            "/health/live",
            get(move || async move {
                Json(HealthResponse {
                    version: common_domain::API_VERSION,
                    service,
                    status: "live",
                })
            }),
        )
        .route(
            "/health/ready",
            get(move || async move {
                Json(HealthResponse {
                    version: common_domain::API_VERSION,
                    service,
                    status: "ready",
                })
            }),
        )
        .fallback(not_found)
        .layer(axum::middleware::from_fn(request_id))
}

async fn not_found(request: Request) -> (StatusCode, Json<ErrorResponse>) {
    let request_id = request
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("unavailable")
        .to_owned();
    (
        StatusCode::NOT_FOUND,
        Json(ErrorResponse {
            version: common_domain::API_VERSION,
            diagnostic_code: "LW_HTTP_ROUTE_NOT_FOUND",
            message: "route not found",
            request_id,
        }),
    )
}

async fn request_id(mut request: Request, next: Next) -> Response {
    let header_name = HeaderName::from_static(REQUEST_ID_HEADER);
    let request_id = request
        .headers()
        .get(&header_name)
        .cloned()
        .unwrap_or_else(|| {
            HeaderValue::from_str(&Uuid::new_v4().to_string())
                .unwrap_or_else(|_| HeaderValue::from_static("request-id-generation-failed"))
        });
    request
        .headers_mut()
        .insert(header_name.clone(), request_id.clone());
    let mut response = next.run(request).await;
    response.headers_mut().insert(header_name, request_id);
    response
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use http::{Request, StatusCode};
    use tower::ServiceExt;

    use super::health_router;

    #[tokio::test]
    async fn health_routes_are_available_and_correlated() {
        let mut request = Request::new(Body::empty());
        *request.uri_mut() = http::Uri::from_static("/health/live");
        let response = health_router("test-service").oneshot(request).await;
        assert!(response.is_ok());
        let response = response.unwrap_or_else(|error| unreachable!("router failed: {error}"));
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().contains_key("x-request-id"));
    }

    #[tokio::test]
    async fn unknown_routes_fail_with_a_stable_diagnostic() {
        let mut request = Request::new(Body::empty());
        *request.uri_mut() = http::Uri::from_static("/unknown");
        let response = health_router("test-service").oneshot(request).await;
        assert!(response.is_ok());
        let response = response.unwrap_or_else(|error| unreachable!("router failed: {error}"));
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(response.headers().contains_key("x-request-id"));
    }
}
