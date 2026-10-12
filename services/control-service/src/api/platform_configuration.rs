//! Administrator configuration routes share the existing Access decision boundary.

use super::{
    ApiError, ApiState, GatewayPrincipal, authorize_global, idempotency, now,
    require_platform_admin,
};
use crate::platform_configuration::{self as configuration, ConfigurationError};
use auth::ServiceTokenVerifier;
use axum::extract::Request;
use axum::{
    Extension, Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use contracts::platform_configuration::{
    ApplyPlatformConfigurationRequest, ReportPlatformConfigurationApplyRequest,
    UpdatePlatformConfigurationRequest,
};
use contracts::{OperationId, Revision};
use std::sync::Arc;

pub(super) fn public_routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route(
            "/api/v1/admin/platform-configuration",
            get(read).patch(update),
        )
        .route(
            "/api/v1/admin/platform-configuration/versions/{revision}",
            get(version),
        )
        .route("/api/v1/admin/platform-configuration/applies", post(apply))
        .route(
            "/api/v1/admin/platform-configuration/applies/{apply_id}",
            get(application),
        )
        .layer(DefaultBodyLimit::max(1024 * 1024))
}

pub(super) fn internal_routes(state: Arc<ApiState>, verifier: Arc<ServiceTokenVerifier>) -> Router {
    Router::new()
        .route(
            "/internal/v1/platform-configuration/applies/{apply_id}",
            put(report),
        )
        .layer(DefaultBodyLimit::max(1024 * 1024))
        .with_state(state)
        .layer(middleware::from_fn_with_state(verifier, require_executor))
}

#[derive(Clone)]
struct Executor(String);

async fn require_executor(
    State(verifier): State<Arc<ServiceTokenVerifier>>,
    mut request: Request,
    next: Next,
) -> Response {
    match verifier
        .authenticate_with_permission(request.headers(), "control.platform_configuration.apply")
        .await
    {
        Ok(identity) if identity.client_id != "labweaver-access" => {
            request
                .extensions_mut()
                .insert(Executor(identity.client_id));
            next.run(request).await
        }
        Ok(_) => ApiError::forbidden("LW_PLATFORM_CONFIGURATION_EXECUTOR_DENIED").into_response(),
        Err(error) => ApiError::from(error).into_response(),
    }
}

async fn admin(
    state: &ApiState,
    principal: &GatewayPrincipal,
    headers: &HeaderMap,
    operation: &str,
) -> Result<contracts::ActorId, ApiError> {
    let decision = authorize_global(state, principal, headers, operation).await?;
    require_platform_admin(&decision)?;
    Ok(decision.actor.actor_id)
}

fn response<T: serde::Serialize>(
    value: T,
    revision: u64,
    status: StatusCode,
) -> Result<Response, ApiError> {
    let mut response = (status, Json(value)).into_response();
    response.headers_mut().insert(
        "ETag",
        HeaderValue::from_str(&format!("\"rev-{revision}\""))
            .map_err(|_| ApiError::internal("LW_PLATFORM_CONFIGURATION_INVALID"))?,
    );
    Ok(response)
}

fn fence(headers: &HeaderMap, revision: u64) -> Result<(), ApiError> {
    if headers.get("If-Match").and_then(|v| v.to_str().ok())
        != Some(format!("\"rev-{revision}\"").as_str())
    {
        return Err(ApiError::precondition(
            "LW_PLATFORM_CONFIGURATION_REVISION_CONFLICT",
        ));
    }
    Ok(())
}

async fn read(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    admin(&state, &principal, &headers, "getPlatformConfiguration").await?;
    let value = configuration::get(&state.control.pool).await?;
    let revision = value.revision;
    response(value, revision, StatusCode::OK)
}

async fn version(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(revision): Path<Revision>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    admin(
        &state,
        &principal,
        &headers,
        "getPlatformConfigurationVersion",
    )
    .await?;
    response(
        configuration::get_version(&state.control.pool, revision).await?,
        revision.get(),
        StatusCode::OK,
    )
}

async fn application(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(id): Path<OperationId>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    admin(
        &state,
        &principal,
        &headers,
        "getPlatformConfigurationApply",
    )
    .await?;
    let value = configuration::get_apply(&state.control.pool, id).await?;
    let revision = value.revision.get();
    response(value, revision, StatusCode::OK)
}

async fn update(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let actor = admin(&state, &principal, &headers, "updatePlatformConfiguration").await?;
    let content_type = headers
        .get("Content-Type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim();
    let request: UpdatePlatformConfigurationRequest = match content_type {
        "application/json" => contracts::parse_strict_json(&body)
            .map_err(|_| ApiError::bad_request("LW_PLATFORM_CONFIGURATION_INVALID"))?,
        "application/yaml" | "application/x-yaml" => parse_yaml(&body)?,
        _ => {
            return Err(ApiError::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "LW_PLATFORM_CONFIGURATION_CONTENT_TYPE_INVALID",
                false,
            ));
        }
    };
    fence(&headers, request.expected_revision)?;
    let value = configuration::update(
        &state.control.pool,
        actor,
        &request,
        &idempotency(&headers)?,
        now()?,
    )
    .await?;
    let revision = value.revision;
    response(value, revision, StatusCode::OK)
}

async fn apply(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let actor = admin(&state, &principal, &headers, "applyPlatformConfiguration").await?;
    let request: ApplyPlatformConfigurationRequest = parse_json(&body)?;
    fence(&headers, request.expected_revision)?;
    let value = configuration::request_apply(
        &state.control.pool,
        actor,
        &request,
        &idempotency(&headers)?,
        now()?,
    )
    .await?;
    let revision = value.revision;
    response(value, revision, StatusCode::ACCEPTED)
}

async fn report(
    State(state): State<Arc<ApiState>>,
    Extension(executor): Extension<Executor>,
    Path(id): Path<OperationId>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let request: ReportPlatformConfigurationApplyRequest = parse_json(&body)?;
    fence(&headers, request.expected_revision.get())?;
    let value = configuration::report(
        &state.control.pool,
        &executor.0,
        id,
        &request,
        &idempotency(&headers)?,
        now()?,
    )
    .await?;
    let revision = value.revision;
    response(value, revision, StatusCode::OK)
}

impl From<ConfigurationError> for ApiError {
    fn from(error: ConfigurationError) -> Self {
        let status = match error {
            ConfigurationError::Invalid | ConfigurationError::Contract(_) => {
                StatusCode::UNPROCESSABLE_ENTITY
            }
            ConfigurationError::NotFound => StatusCode::NOT_FOUND,
            ConfigurationError::Persistence => StatusCode::SERVICE_UNAVAILABLE,
            _ => StatusCode::CONFLICT,
        };
        let retryable = matches!(
            error,
            ConfigurationError::Persistence | ConfigurationError::OperationInProgress
        );
        Self::new(status, &error.to_string(), retryable)
    }
}

fn parse_json<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, ApiError> {
    contracts::parse_strict_json(body)
        .map_err(|_| ApiError::bad_request("LW_PLATFORM_CONFIGURATION_INVALID"))
}

fn parse_yaml<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, ApiError> {
    let value: serde_yaml::Value = serde_yaml::from_slice(body)
        .map_err(|_| ApiError::bad_request("LW_PLATFORM_CONFIGURATION_INVALID"))?;
    serde_yaml::from_value(value)
        .map_err(|_| ApiError::bad_request("LW_PLATFORM_CONFIGURATION_INVALID"))
}

#[cfg(test)]
mod tests {
    use super::{fence, parse_json, parse_yaml};
    use axum::http::HeaderMap;

    #[test]
    fn json_and_yaml_reject_nested_duplicate_keys_and_weak_revision_headers() {
        assert!(parse_json::<serde_json::Value>(br#"{"selector":{"x":1,"x":2}}"#).is_err());
        assert!(parse_yaml::<serde_yaml::Value>(b"selector:\n  x: 1\n  x: 2\n").is_err());
        let mut headers = HeaderMap::new();
        headers.insert(
            "If-Match",
            axum::http::HeaderValue::from_static("W/\"rev-1\""),
        );
        assert!(fence(&headers, 1).is_err());
    }
}
