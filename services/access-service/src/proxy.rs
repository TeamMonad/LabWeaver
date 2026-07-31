//! Authenticated, path-bounded browser forwarding to the Control authority.

use std::{io, time::Duration};

use auth::{ControlGatewayFileConfig, ResourceGatewayFileConfig, TransportSecurityMode};
use axum::{
    body::{Body, Bytes},
    extract::{Query, State},
    http::{HeaderMap, Method, Uri, header},
    response::Response,
};
use futures_util::TryStreamExt;
use reqwest::{Certificate, Client, Identity, Url};
use serde_json::Value;
use sqlx::Row;
use time::OffsetDateTime;

use super::{ApiError, AppState, authenticated_session, require_browser_origin};

const ACTOR_HEADER: &str = "x-labweaver-actor-id";
const SESSION_HEADER: &str = "x-labweaver-session-id";
const RESOURCE_CALLER_HEADER: &str = "x-labweaver-caller-san";
const RESOURCE_CALLER_SAN: &str = "spiffe://labweaver/access-service";
const RESOURCE_ROLES_HEADER: &str = "x-labweaver-platform-roles";

/// A fixed-origin mTLS client; callers cannot select an upstream host.
#[derive(Clone)]
pub(super) struct ControlGatewayProxy {
    client: Client,
    base_uri: Url,
    max_request_bytes: usize,
    max_response_bytes: usize,
}

/// Browser runtime proxy with a derived, namespace-bounded upstream. The
/// browser never receives a cluster address and every request revalidates the
/// exact `EndpointGrant` against both Access and Environment authorities.
#[derive(Clone)]
pub(super) struct RuntimeGatewayProxy {
    client: Client,
    max_request_bytes: usize,
    max_response_bytes: usize,
}

#[derive(Clone)]
pub(super) struct ResourceGatewayProxy {
    client: Client,
    base_uri: Url,
    max_request_bytes: usize,
    max_response_bytes: usize,
}

impl ResourceGatewayProxy {
    pub(super) fn new(config: &ResourceGatewayFileConfig) -> Result<Self, ControlGatewayError> {
        let base_uri = Url::parse(&config.base_uri).map_err(|_| ControlGatewayError::Config)?;
        if base_uri.scheme() != "http"
            || base_uri.host_str().is_none()
            || base_uri.path() != "/"
            || base_uri.query().is_some()
            || base_uri.fragment().is_some()
        {
            return Err(ControlGatewayError::Config);
        }
        Ok(Self {
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(3))
                .timeout(Duration::from_millis(config.timeout_milliseconds))
                .build()
                .map_err(|_| ControlGatewayError::Config)?,
            base_uri,
            max_request_bytes: config.max_request_bytes,
            max_response_bytes: config.max_response_bytes,
        })
    }
}

impl RuntimeGatewayProxy {
    pub(super) fn new(config: &ControlGatewayFileConfig) -> Result<Self, ControlGatewayError> {
        Ok(Self {
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(3))
                .timeout(Duration::from_millis(config.timeout_milliseconds))
                .build()
                .map_err(|_| ControlGatewayError::Config)?,
            max_request_bytes: config.max_request_bytes,
            max_response_bytes: config.max_response_bytes,
        })
    }
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

pub(super) async fn forward_resource(
    State(state): State<std::sync::Arc<AppState>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    if !matches!(method, Method::GET | Method::POST) || !valid_resource_path(uri.path()) {
        return Err(ApiError::bad_request("LW_AUTH_RESOURCE_PATH_REJECTED"));
    }
    if body.len() > state.resource_proxy.max_request_bytes {
        return Err(ApiError::bad_request("LW_AUTH_RESOURCE_REQUEST_TOO_LARGE"));
    }
    let session = authenticated_session(&state, &headers).await?;
    let operation_id = authorize_resource_request(&state, &session, &method, &uri, &body).await?;
    if method != Method::GET {
        require_browser_origin(&state, &headers)?;
        let supplied = headers
            .get(state.deployment.browser.csrf_header_name.as_str())
            .and_then(|value| value.to_str().ok());
        auth::verify_csrf_token(&session.csrf_token, supplied).map_err(ApiError::from)?;
    }
    let roles = resource_roles(&session)?;
    let mut upstream = state.resource_proxy.base_uri.clone();
    upstream.set_path(uri.path());
    upstream.set_query(uri.query());
    let request = state
        .resource_proxy
        .client
        .request(method.clone(), upstream)
        .header(ACTOR_HEADER, session.actor_id.to_string())
        .header(SESSION_HEADER, session.session_id.to_string())
        .header(RESOURCE_CALLER_HEADER, RESOURCE_CALLER_SAN)
        .header(RESOURCE_ROLES_HEADER, roles)
        .body(body);
    let response = copy_request_headers(request, &headers)
        .send()
        .await
        .map_err(|error| {
            tracing::warn!(
                event = "auth.resource_gateway.unavailable",
                diagnostic = "LW_AUTH_RESOURCE_UNAVAILABLE",
                operation_id,
                error = %error
            );
            ApiError::unavailable("LW_AUTH_RESOURCE_UNAVAILABLE")
        })?;
    bounded_resource_response(&state.resource_proxy, response, operation_id).await
}

async fn authorize_resource_request(
    state: &AppState,
    session: &auth::BffSession,
    method: &Method,
    uri: &Uri,
    body: &Bytes,
) -> Result<&'static str, ApiError> {
    let path = uri.path();
    if *method == Method::POST && path == "/api/v1/resource-requests" {
        let request = contracts::parse_strict_json::<contracts::http::CreateResourceRequest>(body)
            .map_err(|_| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))?;
        authorize_resource_scope(
            state,
            session,
            contracts::AuthorizationScope::Course {
                course_id: request.course_id,
            },
            "createResourceRequest",
        )
        .await?;
        return Ok("createResourceRequest");
    }
    if *method == Method::GET
        && matches!(
            path,
            "/api/v1/resource-requests" | "/api/v1/resource-leases"
        )
    {
        #[derive(serde::Deserialize)]
        struct ResourceQuery {
            course_id: contracts::CourseId,
        }
        let Query(query) = Query::<ResourceQuery>::try_from_uri(uri)
            .map_err(|_| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))?;
        let operation = if path.ends_with("leases") {
            "listResourceLeases"
        } else {
            "listResourceRequests"
        };
        authorize_resource_scope(
            state,
            session,
            contracts::AuthorizationScope::Course {
                course_id: query.course_id,
            },
            operation,
        )
        .await?;
        return Ok(operation);
    }
    let (request, operation) = resource_target_request(state, session, method, path).await?;
    if request.project_id.is_none() {
        return Err(ApiError::forbidden("LW_AUTH_PROJECT_SCOPE_DENIED"));
    }
    authorize_resource_scope(
        state,
        session,
        contracts::AuthorizationScope::Course {
            course_id: request.course_id,
        },
        operation,
    )
    .await?;
    Ok(operation)
}

async fn resource_target_request(
    state: &AppState,
    session: &auth::BffSession,
    method: &Method,
    path: &str,
) -> Result<(contracts::resource::ResourceRequest, &'static str), ApiError> {
    let segments = path.split('/').collect::<Vec<_>>();
    match segments.as_slice() {
        ["", "api", "v1", "resource-requests", request_id] if *method == Method::GET => {
            let id = request_id
                .parse()
                .map_err(|_| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))?;
            let request = fetch_resource_request(state, session, id).await?;
            Ok((request, "getResourceRequest"))
        }
        ["", "api", "v1", "resource-requests", request_id, action] if *method == Method::POST => {
            let id = request_id
                .parse()
                .map_err(|_| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))?;
            let operation = match *action {
                "approve" => "approveResourceRequest",
                "resize-and-approve" => "resizeAndApproveResourceRequest",
                "cancel" => "cancelResourceRequest",
                "reject" => "rejectResourceRequest",
                "retry" => "retryResourceRequest",
                _ => return Err(ApiError::bad_request("LW_AUTH_RESOURCE_PATH_REJECTED")),
            };
            Ok((fetch_resource_request(state, session, id).await?, operation))
        }
        ["", "api", "v1", "resource-leases", lease_id] if *method == Method::GET => {
            let id = lease_id
                .parse()
                .map_err(|_| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))?;
            let lease = fetch_resource_lease(state, session, id).await?;
            Ok((
                fetch_resource_request(state, session, lease.request_id).await?,
                "getResourceLease",
            ))
        }
        ["", "api", "v1", "resource-leases", lease_id, action] if *method == Method::POST => {
            let id = lease_id
                .parse()
                .map_err(|_| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))?;
            let operation = match *action {
                "renew" => "renewResourceLease",
                "revoke" => "revokeResourceLease",
                _ => return Err(ApiError::bad_request("LW_AUTH_RESOURCE_PATH_REJECTED")),
            };
            let lease = fetch_resource_lease(state, session, id).await?;
            Ok((
                fetch_resource_request(state, session, lease.request_id).await?,
                operation,
            ))
        }
        _ => Err(ApiError::bad_request("LW_AUTH_RESOURCE_PATH_REJECTED")),
    }
}

async fn fetch_resource_request(
    state: &AppState,
    session: &auth::BffSession,
    request_id: contracts::ResourceRequestId,
) -> Result<contracts::resource::ResourceRequest, ApiError> {
    fetch_resource_json(
        state,
        session,
        &format!("/api/v1/resource-requests/{request_id}"),
    )
    .await
}

async fn fetch_resource_lease(
    state: &AppState,
    session: &auth::BffSession,
    lease_id: contracts::LeaseId,
) -> Result<contracts::resource::ResourceLease, ApiError> {
    fetch_resource_json(
        state,
        session,
        &format!("/api/v1/resource-leases/{lease_id}"),
    )
    .await
}

async fn fetch_resource_json<T: serde::de::DeserializeOwned>(
    state: &AppState,
    session: &auth::BffSession,
    path: &str,
) -> Result<T, ApiError> {
    let mut upstream = state.resource_proxy.base_uri.clone();
    upstream.set_path(path);
    let response = state
        .resource_proxy
        .client
        .get(upstream)
        .header(ACTOR_HEADER, session.actor_id.to_string())
        .header(SESSION_HEADER, session.session_id.to_string())
        .header(RESOURCE_CALLER_HEADER, RESOURCE_CALLER_SAN)
        .header(RESOURCE_ROLES_HEADER, resource_roles(session)?)
        .send()
        .await
        .map_err(|_| ApiError::unavailable("LW_AUTH_RESOURCE_UNAVAILABLE"))?;
    if !response.status().is_success() {
        return Err(ApiError::forbidden("LW_AUTH_SCOPE_DENIED"));
    }
    if response.content_length().is_some_and(|length| {
        length > u64::try_from(state.resource_proxy.max_response_bytes).unwrap_or(u64::MAX)
    }) {
        return Err(ApiError::unavailable("LW_AUTH_RESOURCE_RESPONSE_TOO_LARGE"));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|_| ApiError::unavailable("LW_AUTH_RESOURCE_UNAVAILABLE"))?;
    if bytes.len() > state.resource_proxy.max_response_bytes {
        return Err(ApiError::unavailable("LW_AUTH_RESOURCE_RESPONSE_TOO_LARGE"));
    }
    contracts::parse_strict_json(&bytes)
        .map_err(|_| ApiError::unavailable("LW_AUTH_RESOURCE_RESPONSE_INVALID"))
}

async fn authorize_resource_scope(
    state: &AppState,
    session: &auth::BffSession,
    scope: contracts::AuthorizationScope,
    operation_id: &'static str,
) -> Result<(), ApiError> {
    let actor = super::actor_from_session(session)?;
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
            now: OffsetDateTime::now_utc(),
        },
        scope,
        &policy.allowed_roles.iter().copied().collect(),
    )
    .map_err(ApiError::from)?;
    Ok(())
}

fn resource_roles(session: &auth::BffSession) -> Result<String, ApiError> {
    let roles = session
        .roles
        .iter()
        .map(|role| match role {
            contracts::PlatformRole::Teacher => "teacher",
            contracts::PlatformRole::Student => "student",
            contracts::PlatformRole::PlatformAdmin => "platform_admin",
        })
        .collect::<Vec<_>>();
    if roles.is_empty() {
        return Err(ApiError::forbidden("LW_AUTH_ROLE_DENIED"));
    }
    Ok(roles.join(","))
}

async fn bounded_resource_response(
    proxy: &ResourceGatewayProxy,
    response: reqwest::Response,
    operation_id: &'static str,
) -> Result<Response, ApiError> {
    let status = response.status();
    if response
        .content_length()
        .is_some_and(|length| length > u64::try_from(proxy.max_response_bytes).unwrap_or(u64::MAX))
    {
        return Err(ApiError::unavailable("LW_AUTH_RESOURCE_RESPONSE_TOO_LARGE"));
    }
    let headers = response.headers().clone();
    let bytes = response
        .bytes()
        .await
        .map_err(|_| ApiError::unavailable("LW_AUTH_RESOURCE_UNAVAILABLE"))?;
    if bytes.len() > proxy.max_response_bytes {
        return Err(ApiError::unavailable("LW_AUTH_RESOURCE_RESPONSE_TOO_LARGE"));
    }
    metrics::counter!(
        "labweaver_auth_resource_gateway_requests",
        "operation" => operation_id,
        "status" => status.as_u16().to_string()
    )
    .increment(1);
    let mut downstream = Response::builder().status(status);
    for name in [
        header::CONTENT_TYPE,
        header::CACHE_CONTROL,
        header::ETAG,
        header::LOCATION,
        header::RETRY_AFTER,
    ] {
        if let Some(value) = headers.get(&name) {
            downstream = downstream.header(name, value);
        }
    }
    downstream
        .body(Body::from(bytes))
        .map_err(|_| ApiError::internal("LW_AUTH_RESOURCE_RESPONSE_INVALID"))
}

#[allow(
    clippy::too_many_lines,
    reason = "the handler keeps authorization, bounded forwarding and response filtering in one auditable path"
)]
pub(super) async fn forward_runtime(
    State(state): State<std::sync::Arc<AppState>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    if !matches!(
        method,
        Method::GET
            | Method::HEAD
            | Method::POST
            | Method::PUT
            | Method::PATCH
            | Method::DELETE
            | Method::OPTIONS
    ) {
        return Err(ApiError::bad_request("LW_ACCESS_RUNTIME_METHOD_REJECTED"));
    }
    if headers
        .get(header::UPGRADE)
        .is_some_and(|value| !value.as_bytes().is_empty())
    {
        return Err(ApiError::unavailable(
            "LW_ACCESS_RUNTIME_UPGRADE_UNSUPPORTED",
        ));
    }
    if body.len() > state.runtime_proxy.max_request_bytes {
        return Err(ApiError::bad_request("LW_ACCESS_RUNTIME_REQUEST_TOO_LARGE"));
    }
    if !matches!(method, Method::GET | Method::HEAD | Method::OPTIONS) {
        require_browser_origin(&state, &headers)?;
    }
    let (endpoint_grant_id, runtime_path) = parse_runtime_path(uri.path())?;
    let session = authenticated_session(&state, &headers).await?;
    let target = authorize_runtime(&state, session.actor_id, endpoint_grant_id).await?;
    let mut upstream = Url::parse(&format!(
        "http://runtime.lw-env-{}.svc.cluster.local:8080/",
        target.environment_id
    ))
    .map_err(|_| ApiError::internal("LW_ACCESS_RUNTIME_TARGET_INVALID"))?;
    upstream.set_path(&runtime_path);
    upstream.set_query(uri.query());
    let request = state
        .runtime_proxy
        .client
        .request(method.clone(), upstream)
        .body(body);
    let response = copy_runtime_request_headers(request, &headers)
        .send()
        .await
        .map_err(|error| {
            tracing::warn!(
                event = "access.runtime_proxy.unavailable",
                diagnostic = "LW_ACCESS_RUNTIME_UNAVAILABLE",
                environment_id = %target.environment_id,
                endpoint_grant_id = %endpoint_grant_id,
                error = %error
            );
            ApiError::unavailable("LW_ACCESS_RUNTIME_UNAVAILABLE")
        })?;
    let status = response.status();
    if response.content_length().is_some_and(|length| {
        length > u64::try_from(state.runtime_proxy.max_response_bytes).unwrap_or(u64::MAX)
    }) {
        return Err(ApiError::unavailable(
            "LW_ACCESS_RUNTIME_RESPONSE_TOO_LARGE",
        ));
    }
    let response_headers = response.headers().clone();
    let bytes = response
        .bytes()
        .await
        .map_err(|_| ApiError::unavailable("LW_ACCESS_RUNTIME_UNAVAILABLE"))?;
    if bytes.len() > state.runtime_proxy.max_response_bytes {
        return Err(ApiError::unavailable(
            "LW_ACCESS_RUNTIME_RESPONSE_TOO_LARGE",
        ));
    }
    let mut downstream = Response::builder().status(status);
    for name in [
        header::CONTENT_TYPE,
        header::CONTENT_ENCODING,
        header::CACHE_CONTROL,
        header::ETAG,
        header::LAST_MODIFIED,
        header::CONTENT_RANGE,
        header::ACCEPT_RANGES,
        header::RETRY_AFTER,
    ] {
        if let Some(value) = response_headers.get(&name) {
            downstream = downstream.header(name, value);
        }
    }
    if let Some(location) = response_headers.get(header::LOCATION) {
        downstream = downstream.header(
            header::LOCATION,
            rewrite_runtime_location(location, endpoint_grant_id)?,
        );
    }
    metrics::counter!(
        "labweaver_access_runtime_proxy_requests",
        "method" => method.to_string(),
        "status" => status.as_u16().to_string()
    )
    .increment(1);
    downstream
        .body(Body::from(bytes))
        .map_err(|_| ApiError::internal("LW_ACCESS_RUNTIME_RESPONSE_INVALID"))
}

struct RuntimeTarget {
    environment_id: contracts::EnvironmentId,
}

async fn authorize_runtime(
    state: &AppState,
    actor_id: uuid::Uuid,
    endpoint_grant_id: contracts::EndpointGrantId,
) -> Result<RuntimeTarget, ApiError> {
    let now = OffsetDateTime::now_utc();
    let row = sqlx::query(
        "SELECT g.course_id,g.environment_id,g.environment_revision,g.contract,\
                eg.endpoint_id,eg.endpoint_revision,eg.protocol,eg.expires_at,\
                cm.expires_at AS membership_expires_at \
         FROM access.endpoint_grants eg JOIN access.access_grants g ON g.grant_id=eg.grant_id \
         JOIN access.course_memberships cm ON cm.course_id=g.course_id AND cm.actor_id=g.actor_id \
         WHERE eg.endpoint_grant_id=$1 AND g.actor_id=$2 AND g.state='active' \
           AND g.not_before<=$3 AND g.expires_at>$3 AND eg.expires_at>$3 \
           AND eg.protocol IN ('http','https') AND eg.health='healthy' \
           AND cm.state='active' AND (cm.expires_at IS NULL OR cm.expires_at>$3) \
           AND cm.role=CASE g.contract->>'subjectKind' \
             WHEN 'owner' THEN 'student' WHEN 'course_teacher' THEN 'teacher' ELSE '' END",
    )
    .bind(endpoint_grant_id.as_uuid())
    .bind(actor_id)
    .bind(now)
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?
    .ok_or_else(|| ApiError::forbidden("LW_ACCESS_RUNTIME_DENIED"))?;
    let environment_id = row
        .get::<uuid::Uuid, _>("environment_id")
        .to_string()
        .parse()
        .map_err(|_| ApiError::internal("LW_ACCESS_STORE_CORRUPT"))?;
    let course_id = row
        .get::<uuid::Uuid, _>("course_id")
        .to_string()
        .parse()
        .map_err(|_| ApiError::internal("LW_ACCESS_STORE_CORRUPT"))?;
    let endpoint_id = row
        .get::<uuid::Uuid, _>("endpoint_id")
        .to_string()
        .parse()
        .map_err(|_| ApiError::internal("LW_ACCESS_STORE_CORRUPT"))?;
    let endpoint_revision = contracts::Revision::new(
        u64::try_from(row.get::<i64, _>("endpoint_revision"))
            .map_err(|_| ApiError::internal("LW_ACCESS_STORE_CORRUPT"))?,
    )
    .map_err(|_| ApiError::internal("LW_ACCESS_STORE_CORRUPT"))?;
    let expected_revision = contracts::Revision::new(
        u64::try_from(row.get::<i64, _>("environment_revision"))
            .map_err(|_| ApiError::internal("LW_ACCESS_STORE_CORRUPT"))?,
    )
    .map_err(|_| ApiError::internal("LW_ACCESS_STORE_CORRUPT"))?;
    let subject_kind: contracts::environment::EnvironmentAccessSubjectKind =
        serde_json::from_value(
            row.get::<Value, _>("contract")
                .get("subjectKind")
                .cloned()
                .ok_or_else(|| ApiError::internal("LW_ACCESS_STORE_CORRUPT"))?,
        )
        .map_err(|_| ApiError::internal("LW_ACCESS_STORE_CORRUPT"))?;
    let actor_id = actor_id
        .to_string()
        .parse()
        .map_err(|_| ApiError::internal("LW_ACCESS_STORE_CORRUPT"))?;
    let eligibility = state
        .owner_resolver
        .resolve_endpoint_eligibility(
            &contracts::environment::EnvironmentEndpointEligibilityRequest {
                environment_id,
                course_id,
                actor_id,
                subject_kind,
                expected_revision,
                endpoint_ids: vec![endpoint_id],
            },
            super::utc_timestamp(now)?,
        )
        .await
        .map_err(|error| match error {
            auth::OwnerResolverClientError::ScopeDenied
            | auth::OwnerResolverClientError::ResponseInvalid => {
                ApiError::forbidden("LW_ACCESS_RUNTIME_DENIED")
            }
            _ => ApiError::unavailable("LW_ACCESS_RUNTIME_AUTHORITY_UNAVAILABLE"),
        })?;
    let endpoint = eligibility
        .endpoints
        .first()
        .ok_or_else(|| ApiError::forbidden("LW_ACCESS_RUNTIME_DENIED"))?;
    if endpoint.id != endpoint_id
        || endpoint.revision != endpoint_revision
        || !matches!(
            endpoint.protocol,
            contracts::environment::EndpointProtocol::Http
                | contracts::environment::EndpointProtocol::Https
        )
        || endpoint.health != contracts::environment::EndpointHealth::Healthy
        || eligibility.eligibility_expires_at.get() <= now
    {
        return Err(ApiError::forbidden("LW_ACCESS_RUNTIME_DENIED"));
    }
    Ok(RuntimeTarget { environment_id })
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

fn copy_runtime_request_headers(
    mut request: reqwest::RequestBuilder,
    headers: &HeaderMap,
) -> reqwest::RequestBuilder {
    for name in [
        header::ACCEPT.as_str(),
        header::ACCEPT_ENCODING.as_str(),
        header::ACCEPT_LANGUAGE.as_str(),
        header::CONTENT_TYPE.as_str(),
        header::CONTENT_ENCODING.as_str(),
        header::RANGE.as_str(),
        header::IF_MATCH.as_str(),
        header::IF_NONE_MATCH.as_str(),
        header::IF_MODIFIED_SINCE.as_str(),
        "traceparent",
        "tracestate",
    ] {
        if let Some(value) = headers.get(name) {
            request = request.header(name, value);
        }
    }
    request
}

fn parse_runtime_path(path: &str) -> Result<(contracts::EndpointGrantId, String), ApiError> {
    if !safe_path(path) {
        return Err(ApiError::bad_request("LW_ACCESS_RUNTIME_PATH_REJECTED"));
    }
    let value = path
        .strip_prefix("/connect/")
        .ok_or_else(|| ApiError::bad_request("LW_ACCESS_RUNTIME_PATH_REJECTED"))?;
    let (grant, remainder) = value
        .split_once('/')
        .ok_or_else(|| ApiError::bad_request("LW_ACCESS_RUNTIME_PATH_REJECTED"))?;
    let grant = grant
        .parse()
        .map_err(|_| ApiError::bad_request("LW_ACCESS_RUNTIME_PATH_REJECTED"))?;
    Ok((grant, format!("/{remainder}")))
}

fn rewrite_runtime_location(
    location: &reqwest::header::HeaderValue,
    endpoint_grant_id: contracts::EndpointGrantId,
) -> Result<reqwest::header::HeaderValue, ApiError> {
    let value = location
        .to_str()
        .map_err(|_| ApiError::unavailable("LW_ACCESS_RUNTIME_RESPONSE_INVALID"))?;
    if !value.starts_with('/') || value.starts_with("//") || !safe_path(value) {
        return Err(ApiError::unavailable("LW_ACCESS_RUNTIME_REDIRECT_REJECTED"));
    }
    format!("/connect/{endpoint_grant_id}{value}")
        .parse()
        .map_err(|_| ApiError::unavailable("LW_ACCESS_RUNTIME_RESPONSE_INVALID"))
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

fn valid_resource_path(path: &str) -> bool {
    if !safe_path(path) {
        return false;
    }
    let segments = path.split('/').collect::<Vec<_>>();
    matches!(
        segments.as_slice(),
        ["", "api", "v1", "resource-requests" | "resource-leases"]
            | ["", "api", "v1", "resource-requests" | "resource-leases", _]
    ) || matches!(
        segments.as_slice(),
        ["", "api", "v1", "resource-requests", _, action]
            if matches!(*action, "approve" | "resize-and-approve" | "cancel" | "reject" | "retry")
    ) || matches!(
        segments.as_slice(),
        ["", "api", "v1", "resource-leases", _, action]
            if matches!(*action, "renew" | "revoke")
    )
}

fn safe_path(path: &str) -> bool {
    let lowercase = path.to_ascii_lowercase();
    !path.contains("//")
        && !path.contains('\\')
        && !lowercase.contains("%2f")
        && !lowercase.contains("%5c")
        && !lowercase.contains("%2e")
        && !lowercase.contains("%25")
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
    use super::{
        parse_runtime_path, valid_control_path, valid_evaluation_path, valid_resource_path,
    };

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

    #[test]
    fn runtime_paths_bind_one_endpoint_grant_and_reject_injection() {
        let path = "/connect/01900000-0000-7000-8000-000000000001/workbench/";
        let (_, remainder) = parse_runtime_path(path)
            .unwrap_or_else(|error| unreachable!("static bounded path must parse: {error:?}"));
        assert_eq!(remainder, "/workbench/");
        assert!(parse_runtime_path("/connect/not-a-grant/").is_err());
        assert!(
            parse_runtime_path("/connect/01900000-0000-7000-8000-000000000001/../internal")
                .is_err()
        );
        assert!(parse_runtime_path("/connect/01900000-0000-7000-8000-000000000001/a%2Fb").is_err());
        assert!(
            parse_runtime_path("/connect/01900000-0000-7000-8000-000000000001/%2e%2e/internal")
                .is_err()
        );
    }

    #[test]
    fn resource_paths_are_exact_and_injection_safe() {
        assert!(valid_resource_path("/api/v1/resource-requests"));
        assert!(valid_resource_path(
            "/api/v1/resource-requests/01900000-0000-7000-8000-000000000001/approve"
        ));
        assert!(valid_resource_path(
            "/api/v1/resource-leases/01900000-0000-7000-8000-000000000001/renew"
        ));
        assert!(!valid_resource_path(
            "/api/v1/resource-requests/01900000-0000-7000-8000-000000000001/arbitrary"
        ));
        assert!(!valid_resource_path(
            "/api/v1/resource-requests/../internal"
        ));
    }
}
