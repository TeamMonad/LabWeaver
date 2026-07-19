//! Authenticated, path-bounded browser forwarding to the Control authority.

use std::{io, time::Duration};

use auth::{ControlGatewayFileConfig, TransportSecurityMode};
use axum::{
    body::{Body, Bytes},
    extract::{Query, State},
    http::{HeaderMap, Method, Uri, header},
    response::Response,
};
use futures_util::TryStreamExt;
use reqwest::{Certificate, Client, Identity, Url};

use super::{ApiError, AppState, authenticated_session, require_browser_origin};

const ACTOR_HEADER: &str = "x-labweaver-actor-id";
const SESSION_HEADER: &str = "x-labweaver-session-id";

/// A fixed-origin mTLS client; callers cannot select an upstream host.
#[derive(Clone)]
pub(super) struct ControlGatewayProxy {
    client: Client,
    base_uri: Url,
    max_request_bytes: usize,
    max_response_bytes: usize,
}

impl ControlGatewayProxy {
    pub(super) fn new(
        config: &ControlGatewayFileConfig,
        ca_certificate_pem: &[u8],
        client_certificate_pem: &[u8],
        client_private_key_pem: &[u8],
        transport_security: TransportSecurityMode,
    ) -> Result<Self, ControlGatewayError> {
        let base_uri = Url::parse(&config.base_uri).map_err(|_| ControlGatewayError::Config)?;
        let host = base_uri.host_str().ok_or(ControlGatewayError::Config)?;
        if base_uri.scheme() != "https"
            || base_uri.path() != "/"
            || base_uri.query().is_some()
            || base_uri.fragment().is_some()
            || !config.allowed_server_sans.iter().any(|san| san == host)
        {
            return Err(ControlGatewayError::Config);
        }
        if transport_security == TransportSecurityMode::InsecureTestOnly
            && !host.eq_ignore_ascii_case("localhost")
            && !host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
        {
            return Err(ControlGatewayError::Config);
        }
        let roots = Certificate::from_pem_bundle(ca_certificate_pem)
            .map_err(|_| ControlGatewayError::Certificate)?;
        if roots.is_empty() {
            return Err(ControlGatewayError::Certificate);
        }
        let mut identity_pem =
            Vec::with_capacity(client_certificate_pem.len() + client_private_key_pem.len() + 1);
        identity_pem.extend_from_slice(client_certificate_pem);
        identity_pem.push(b'\n');
        identity_pem.extend_from_slice(client_private_key_pem);
        let identity =
            Identity::from_pem(&identity_pem).map_err(|_| ControlGatewayError::Certificate)?;
        let mut builder = Client::builder()
            .https_only(true)
            .tls_built_in_root_certs(false)
            .redirect(reqwest::redirect::Policy::none())
            .identity(identity)
            .timeout(Duration::from_millis(config.timeout_milliseconds));
        if transport_security == TransportSecurityMode::InsecureTestOnly {
            builder = builder.danger_accept_invalid_certs(true);
        }
        for root in roots {
            builder = builder.add_root_certificate(root);
        }
        Ok(Self {
            client: builder
                .build()
                .map_err(|_| ControlGatewayError::Certificate)?,
            base_uri,
            max_request_bytes: config.max_request_bytes,
            max_response_bytes: config.max_response_bytes,
        })
    }
}

pub(super) async fn forward_control(
    State(state): State<std::sync::Arc<AppState>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    forward(
        &state,
        &state.control_proxy,
        method,
        uri,
        headers,
        body,
        valid_control_path,
    )
    .await
}

pub(super) async fn forward_environment(
    State(state): State<std::sync::Arc<AppState>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    if method == Method::POST && uri.path() == "/api/v1/environments" {
        authorize_environment_create(&state, &headers, &body).await?;
    } else if method == Method::GET && uri.path() == "/api/v1/environments" {
        let Query(query) = Query::<contracts::http::EnvironmentInventoryQuery>::try_from_uri(&uri)
            .map_err(|_| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))?;
        query
            .validate()
            .map_err(|_| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))?;
        authorize_environment_course(&state, &headers, query.course_id, "listEnvironments").await?;
    }
    forward(
        &state,
        &state.environment_proxy,
        method,
        uri,
        headers,
        body,
        valid_environment_path,
    )
    .await
}

pub(super) async fn forward_evaluation(
    State(state): State<std::sync::Arc<AppState>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    if method == Method::POST {
        let request =
            contracts::parse_strict_json::<contracts::http::FreezeSubmissionRequest>(&body)
                .map_err(|_| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))?;
        request
            .manifest
            .validate()
            .map_err(|_| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))?;
        authorize_environment_course(&state, &headers, request.course_id, "freezeSubmission")
            .await?;
    }
    forward(
        &state,
        &state.evaluation_proxy,
        method,
        uri,
        headers,
        body,
        valid_evaluation_path,
    )
    .await
}

async fn forward(
    state: &AppState,
    proxy: &ControlGatewayProxy,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
    valid_path: fn(&str) -> bool,
) -> Result<Response, ApiError> {
    if !matches!(method, Method::GET | Method::POST | Method::DELETE) {
        return Err(ApiError::bad_request("LW_AUTH_CONTROL_METHOD_REJECTED"));
    }
    let path = uri.path();
    if !valid_path(path) {
        return Err(ApiError::bad_request("LW_AUTH_CONTROL_PATH_REJECTED"));
    }
    if body.len() > proxy.max_request_bytes {
        return Err(ApiError::bad_request("LW_AUTH_CONTROL_REQUEST_TOO_LARGE"));
    }
    let session = authenticated_session(state, &headers).await?;
    if method != Method::GET {
        require_browser_origin(state, &headers)?;
        let supplied = headers
            .get(state.deployment.browser.csrf_header_name.as_str())
            .and_then(|value| value.to_str().ok());
        auth::verify_csrf_token(&session.csrf_token, supplied).map_err(ApiError::from)?;
    }

    let mut upstream = proxy.base_uri.clone();
    upstream.set_path(path);
    upstream.set_query(uri.query());
    let request = proxy
        .client
        .request(method.clone(), upstream)
        .header(ACTOR_HEADER, session.actor_id.to_string())
        .header(SESSION_HEADER, session.session_id.to_string())
        .body(body);
    let request = copy_request_headers(request, &headers);
    let response = request.send().await.map_err(|error| {
        tracing::warn!(
            event = "auth.control_gateway.unavailable",
            diagnostic = "LW_AUTH_CONTROL_UNAVAILABLE",
            error = %error
        );
        ApiError::unavailable("LW_AUTH_CONTROL_UNAVAILABLE")
    })?;
    let status = response.status();
    let content_length = response.content_length();
    if content_length
        .is_some_and(|length| length > u64::try_from(proxy.max_response_bytes).unwrap_or(u64::MAX))
    {
        return Err(ApiError::unavailable("LW_AUTH_CONTROL_RESPONSE_TOO_LARGE"));
    }
    let response_headers = response.headers().clone();
    let is_sse = response_headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/event-stream"));
    let response_body = if is_sse {
        Body::from_stream(response.bytes_stream().map_err(io::Error::other))
    } else {
        let bytes = response
            .bytes()
            .await
            .map_err(|_| ApiError::unavailable("LW_AUTH_CONTROL_UNAVAILABLE"))?;
        if bytes.len() > proxy.max_response_bytes {
            return Err(ApiError::unavailable("LW_AUTH_CONTROL_RESPONSE_TOO_LARGE"));
        }
        Body::from(bytes)
    };
    let mut downstream = Response::builder().status(status);
    for name in [
        header::CONTENT_TYPE,
        header::CACHE_CONTROL,
        header::ETAG,
        header::LOCATION,
        header::RETRY_AFTER,
    ] {
        if let Some(value) = response_headers.get(&name) {
            downstream = downstream.header(name, value);
        }
    }
    metrics::counter!(
        "labweaver_auth_control_gateway_requests",
        "method" => method.to_string(),
        "status" => status.as_u16().to_string()
    )
    .increment(1);
    downstream
        .body(response_body)
        .map_err(|_| ApiError::internal("LW_AUTH_CONTROL_RESPONSE_INVALID"))
}

async fn authorize_environment_create(
    state: &AppState,
    headers: &HeaderMap,
    body: &Bytes,
) -> Result<(), ApiError> {
    let request = contracts::parse_strict_json::<contracts::http::CreateEnvironmentRequest>(body)
        .map_err(|_| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))?;
    authorize_environment_course(state, headers, request.course_id, "createEnvironment").await
}

async fn authorize_environment_course(
    state: &AppState,
    headers: &HeaderMap,
    course_id: contracts::CourseId,
    operation_id: &'static str,
) -> Result<(), ApiError> {
    let session = authenticated_session(state, headers).await?;
    let actor = super::actor_from_session(&session)?;
    let memberships = auth::load_membership_snapshot(&state.pool, session.actor_id)
        .await
        .map_err(ApiError::from)?;
    let policy = contracts::operation_authorization(operation_id)
        .ok_or_else(|| ApiError::forbidden("LW_AUTH_SCOPE_DENIED"))?;
    auth::authorize(
        &auth::AuthorizationContext {
            actor,
            course_memberships: memberships.course_memberships,
            project_memberships: memberships.project_memberships,
            now: time::OffsetDateTime::now_utc(),
        },
        contracts::AuthorizationScope::Course { course_id },
        &policy.allowed_roles.iter().copied().collect(),
    )
    .map_err(ApiError::from)?;
    Ok(())
}

fn copy_request_headers(
    mut request: reqwest::RequestBuilder,
    headers: &HeaderMap,
) -> reqwest::RequestBuilder {
    for name in [
        header::ACCEPT.as_str(),
        header::CONTENT_TYPE.as_str(),
        header::IF_MATCH.as_str(),
        header::IF_NONE_MATCH.as_str(),
        "idempotency-key",
        "last-event-id",
        "traceparent",
        "tracestate",
    ] {
        if let Some(value) = headers.get(name) {
            request = request.header(name, value);
        }
    }
    request
}

fn valid_control_path(path: &str) -> bool {
    let lowercase = path.to_ascii_lowercase();
    path.starts_with("/api/v1/courses/")
        && !path.contains("//")
        && !path.contains('\\')
        && !lowercase.contains("%2f")
        && !lowercase.contains("%5c")
        && path
            .split('/')
            .all(|segment| segment != "." && segment != "..")
}

fn valid_environment_path(path: &str) -> bool {
    path == "/api/v1/environments" || (path.starts_with("/api/v1/environments/") && safe_path(path))
}

fn valid_evaluation_path(path: &str) -> bool {
    let segments = path.split('/').collect::<Vec<_>>();
    safe_path(path)
        && (matches!(
            segments.as_slice(),
            ["", "api", "v1", "environments", _, "freeze"]
        ) || matches!(
            segments.as_slice(),
            ["", "api", "v1", "frozen-submissions", _]
        ))
}

fn safe_path(path: &str) -> bool {
    let lowercase = path.to_ascii_lowercase();
    !path.contains("//")
        && !path.contains('\\')
        && !lowercase.contains("%2f")
        && !lowercase.contains("%5c")
        && path
            .split('/')
            .all(|segment| segment != "." && segment != "..")
}

/// Startup-only proxy construction failures.
#[derive(Debug, thiserror::Error)]
pub(super) enum ControlGatewayError {
    #[error("LW_AUTH_CONFIG_BINDING_MISSING")]
    Config,
    #[error("LW_AUTH_CONFIG_BINDING_MISSING")]
    Certificate,
}

#[cfg(test)]
mod tests {
    use super::{valid_control_path, valid_evaluation_path};

    #[test]
    fn control_paths_are_bounded_to_the_course_api() {
        assert!(valid_control_path("/api/v1/courses/course-1/agent-runs"));
        assert!(!valid_control_path("/internal/v1/auth/decision"));
        assert!(!valid_control_path("/api/v1/courses/../internal"));
        assert!(!valid_control_path("/api/v1/courses/a%2Finternal"));
        assert!(!valid_control_path("/api/v1/courses//agent-runs"));
    }

    #[test]
    fn evaluation_paths_are_exact_and_injection_safe() {
        assert!(valid_evaluation_path(
            "/api/v1/environments/01900000-0000-7000-8000-000000000001/freeze"
        ));
        assert!(valid_evaluation_path(
            "/api/v1/frozen-submissions/01900000-0000-7000-8000-000000000001"
        ));
        assert!(!valid_evaluation_path("/api/v1/environments/a/freeze/more"));
        assert!(!valid_evaluation_path(
            "/api/v1/frozen-submissions/../internal"
        ));
    }
}
