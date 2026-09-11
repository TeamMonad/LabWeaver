//! Authenticated, path-bounded browser forwarding to the Control authority.

use std::{
    io,
    sync::Arc,
    time::{Duration, Instant},
};

use auth::{
    ControlGatewayFileConfig, ResourceGatewayFileConfig, ServiceTokenClient, TransportSecurityMode,
};
use axum::{
    body::{Body, Bytes},
    extract::{Query, State},
    http::{HeaderMap, Method, Uri, header},
    response::Response,
};
use futures_util::TryStreamExt;
use reqwest::{Certificate, Client, Url};
use serde_json::Value;
use sqlx::Row;
use time::OffsetDateTime;

use super::{
    ApiError, AppState, ServiceTokenTarget, authenticated_session, require_browser_origin,
};

const RESOURCE_DELEGATION_HEADER: &str = "x-labweaver-resource-delegation";
const ACTOR_HEADER: &str = "x-labweaver-actor-id";
const SESSION_HEADER: &str = "x-labweaver-session-id";

/// A fixed-origin TLS client; callers cannot select an upstream host.
#[derive(Clone)]
pub(super) struct ControlGatewayProxy {
    client: Client,
    base_uri: Url,
    service_token_client: Arc<ServiceTokenClient>,
    service_token_target: ServiceTokenTarget,
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
    service_token_client: Arc<ServiceTokenClient>,
    service_token_target: ServiceTokenTarget,
    delegation_key: Vec<u8>,
    max_request_bytes: usize,
    max_response_bytes: usize,
}

struct ForwardRequest {
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
    valid_path: fn(&str) -> bool,
    scope: Option<EnvironmentFreezeScope>,
}

impl ResourceGatewayProxy {
    pub(super) fn new(
        config: &ResourceGatewayFileConfig,
        ca_certificate_pem: &[u8],
        delegation_key: &[u8],
        transport_security: TransportSecurityMode,
        service_token_client: Arc<ServiceTokenClient>,
        service_token_target: ServiceTokenTarget,
    ) -> Result<Self, ControlGatewayError> {
        let base_uri = Url::parse(&config.base_uri).map_err(|_| ControlGatewayError::Config)?;
        let host = base_uri.host_str().ok_or(ControlGatewayError::Config)?;
        if !matches!(base_uri.scheme(), "http" | "https")
            || base_uri.host_str().is_none()
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
        if delegation_key.len() < 32 {
            return Err(ControlGatewayError::Config);
        }
        if base_uri.scheme() == "http" {
            let client = Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(3))
                .timeout(Duration::from_millis(config.timeout_milliseconds))
                .build()
                .map_err(|_| ControlGatewayError::Certificate)?;
            return Ok(Self {
                client,
                base_uri,
                service_token_client,
                service_token_target,
                delegation_key: delegation_key.to_vec(),
                max_request_bytes: config.max_request_bytes,
                max_response_bytes: config.max_response_bytes,
            });
        }
        let roots = Certificate::from_pem_bundle(ca_certificate_pem)
            .map_err(|_| ControlGatewayError::Certificate)?;
        if roots.is_empty() {
            return Err(ControlGatewayError::Certificate);
        }
        let mut builder = Client::builder()
            .https_only(true)
            .tls_built_in_root_certs(false)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(3))
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
            service_token_client,
            service_token_target,
            delegation_key: delegation_key.to_vec(),
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
        transport_security: TransportSecurityMode,
        service_token_client: Arc<ServiceTokenClient>,
        service_token_target: ServiceTokenTarget,
    ) -> Result<Self, ControlGatewayError> {
        let base_uri = Url::parse(&config.base_uri).map_err(|_| ControlGatewayError::Config)?;
        let host = base_uri.host_str().ok_or(ControlGatewayError::Config)?;
        if !matches!(base_uri.scheme(), "http" | "https")
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
        // Loopback HTTP is only available under the explicit test transport mode.
        if base_uri.scheme() == "http" {
            let client = Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_millis(config.timeout_milliseconds))
                .build()
                .map_err(|_| ControlGatewayError::Certificate)?;
            return Ok(Self {
                client,
                base_uri,
                service_token_client,
                service_token_target,
                max_request_bytes: config.max_request_bytes,
                max_response_bytes: config.max_response_bytes,
            });
        }
        let roots = Certificate::from_pem_bundle(ca_certificate_pem)
            .map_err(|_| ControlGatewayError::Certificate)?;
        if roots.is_empty() {
            return Err(ControlGatewayError::Certificate);
        }
        let mut builder = Client::builder()
            .https_only(true)
            .tls_built_in_root_certs(false)
            .redirect(reqwest::redirect::Policy::none())
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
            service_token_client,
            service_token_target,
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
    authorize_evaluation_release_path(&state, &method, &uri, &headers).await?;
    forward(
        &state,
        &state.control_proxy,
        ForwardRequest {
            method,
            uri,
            headers,
            body,
            valid_path: valid_control_path,
            scope: None,
        },
    )
    .await
}

async fn authorize_evaluation_release_path(
    state: &AppState,
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
) -> Result<(), ApiError> {
    let segments = uri.path().split('/').collect::<Vec<_>>();
    let (course, operation) = match (method, segments.as_slice()) {
        (method, ["", "api", "v1", "courses", course, "evaluation-releases"])
            if *method == Method::POST =>
        {
            (*course, "createEvaluationRelease")
        }
        (method, ["", "api", "v1", "courses", course, "evaluation-releases"])
            if *method == Method::GET =>
        {
            (*course, "listEvaluationReleases")
        }
        (method, ["", "api", "v1", "courses", course, "evaluation-releases", _])
            if *method == Method::GET =>
        {
            (*course, "getEvaluationRelease")
        }
        (
            method,
            [
                "",
                "api",
                "v1",
                "courses",
                course,
                "evaluation-releases",
                _,
                "withdraw",
            ],
        ) if *method == Method::POST => (*course, "withdrawEvaluationRelease"),
        _ => return Ok(()),
    };
    authorize_environment_course(
        state,
        headers,
        course
            .parse()
            .map_err(|_| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))?,
        operation,
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
        authorize_environment_project(&state, &headers, query.project_id, "listEnvironments")
            .await?;
    }
    forward(
        &state,
        &state.environment_proxy,
        ForwardRequest {
            method,
            uri,
            headers,
            body,
            valid_path: valid_environment_path,
            scope: None,
        },
    )
    .await
}

pub(super) async fn forward_evaluation(
    State(state): State<std::sync::Arc<AppState>>,
    method: Method,
    uri: Uri,
    mut headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let mut scope = None;
    if method == Method::POST {
        let environment_id = uri
            .path()
            .split('/')
            .collect::<Vec<_>>()
            .as_slice()
            .get(4)
            .ok_or_else(|| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))?
            .parse()
            .map_err(|_| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))?;
        let request =
            contracts::parse_strict_json::<contracts::http::FreezeSubmissionRequest>(&body)
                .map_err(|_| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))?;
        request
            .manifest
            .validate()
            .map_err(|_| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))?;
        let resolved_scope = authorize_environment_freeze(
            &state,
            &headers,
            environment_id,
            request.course_id,
            "freezeSubmission",
        )
        .await?;
        // Evaluation is a separate service and cannot infer project ownership
        // from the browser session.  Only the scope resolved from the active
        // Access grant is forwarded; caller-supplied values are never trusted.
        headers.insert(
            "x-labweaver-project-id",
            resolved_scope
                .project_id
                .to_string()
                .parse()
                .map_err(|_| ApiError::internal("LW_ACCESS_SCOPE_INVALID"))?,
        );
        if let Some(course_id) = resolved_scope.course_id {
            headers.insert(
                "x-labweaver-course-id",
                course_id
                    .to_string()
                    .parse()
                    .map_err(|_| ApiError::internal("LW_ACCESS_SCOPE_INVALID"))?,
            );
        } else {
            headers.remove("x-labweaver-course-id");
        }
        scope = Some(resolved_scope);
    } else if method == Method::GET {
        let segments = uri.path().split('/').collect::<Vec<_>>();
        if let [
            "",
            "api",
            "v1",
            "courses",
            course,
            "me",
            "evaluation-results",
            rest @ ..,
        ] = segments.as_slice()
        {
            let course_id = course
                .parse()
                .map_err(|_| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))?;
            let operation = if rest.is_empty() {
                "listOwnEvaluationResults"
            } else {
                "getOwnEvaluationResult"
            };
            authorize_environment_course(&state, &headers, course_id, operation).await?;
        }
    }
    forward(
        &state,
        &state.evaluation_proxy,
        ForwardRequest {
            method,
            uri,
            headers,
            body,
            valid_path: valid_evaluation_path,
            scope,
        },
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
    if !matches!(method, Method::GET | Method::POST | Method::PUT)
        || !valid_resource_path(uri.path())
    {
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
    let delegation = auth::encode_resource_delegation(
        &state.resource_proxy.delegation_key,
        &session,
        OffsetDateTime::now_utc(),
    )
    .map_err(|_| ApiError::unavailable("LW_AUTH_RESOURCE_DELEGATION_INVALID"))?;
    let mut upstream = state.resource_proxy.base_uri.clone();
    upstream.set_path(uri.path());
    upstream.set_query(uri.query());
    let request = state
        .resource_proxy
        .client
        .request(method.clone(), upstream)
        .header(RESOURCE_DELEGATION_HEADER, delegation)
        .body(body);
    let request = copy_request_headers(request, &headers);
    let request = attach_service_token(
        request,
        &state.resource_proxy.service_token_client,
        &state.resource_proxy.service_token_target,
    )
    .await?;
    let response = request.send().await.map_err(|_error| {
        tracing::warn!(
            event = "auth.resource_gateway.unavailable",
            diagnostic_code = "LW_AUTH_RESOURCE_UNAVAILABLE",
            operation_id,
            error_kind = "upstream_transport",
            failure_stage = "resource_request",
            retryable = true
        );
        ApiError::unavailable("LW_AUTH_RESOURCE_UNAVAILABLE")
    })?;
    bounded_resource_response(&state.resource_proxy, response, operation_id).await
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProjectResourceListQuery {
    course_id: Option<contracts::CourseId>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyResourceListQuery {}

#[allow(
    clippy::too_many_lines,
    reason = "resource authorization keeps every public path, body identity, and scope fence together"
)]
async fn authorize_resource_request(
    state: &AppState,
    session: &auth::BffSession,
    method: &Method,
    uri: &Uri,
    body: &Bytes,
) -> Result<&'static str, ApiError> {
    let path = uri.path();
    let segments = path.split('/').collect::<Vec<_>>();
    match segments.as_slice() {
        ["", "api", "v1", "projects", project_id, "resource-requests"] => {
            let project_id = project_id
                .parse()
                .map_err(|_| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))?;
            match *method {
                Method::GET => {
                    let Query(query) = Query::<ProjectResourceListQuery>::try_from_uri(uri)
                        .map_err(|_| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))?;
                    let _ = query.course_id;
                    authorize_resource_scope(
                        state,
                        session,
                        contracts::AuthorizationScope::Project { project_id },
                        "listProjectResourceRequests",
                    )
                    .await?;
                    return Ok("listProjectResourceRequests");
                }
                Method::POST => {
                    let request = contracts::parse_strict_json::<
                        contracts::http::CreateResourceRequest,
                    >(body)
                    .map_err(|_| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))?;
                    if request.project_id != project_id {
                        return Err(ApiError::forbidden("LW_AUTH_SCOPE_DENIED"));
                    }
                    authorize_resource_scope(
                        state,
                        session,
                        contracts::AuthorizationScope::Project { project_id },
                        "createProjectResourceRequest",
                    )
                    .await?;
                    return Ok("createProjectResourceRequest");
                }
                _ => return Err(ApiError::bad_request("LW_AUTH_RESOURCE_PATH_REJECTED")),
            }
        }
        ["", "api", "v1", "projects", project_id, "resource-leases"] if *method == Method::GET => {
            let project_id = project_id
                .parse()
                .map_err(|_| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))?;
            let Query(query) = Query::<ProjectResourceListQuery>::try_from_uri(uri)
                .map_err(|_| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))?;
            let _ = query.course_id;
            authorize_resource_scope(
                state,
                session,
                contracts::AuthorizationScope::Project { project_id },
                "listProjectResourceLeases",
            )
            .await?;
            return Ok("listProjectResourceLeases");
        }
        ["", "api", "v1", "resource", "gpu-catalog"] => match *method {
            Method::GET => {
                authorize_resource_scope(
                    state,
                    session,
                    contracts::AuthorizationScope::Global,
                    "listResourceGpuCatalog",
                )
                .await?;
                return Ok("listResourceGpuCatalog");
            }
            Method::POST => {
                let _entry =
                    contracts::parse_strict_json::<contracts::resource::GpuCatalogEntry>(body)
                        .map_err(|_| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))?;
                authorize_resource_scope(
                    state,
                    session,
                    contracts::AuthorizationScope::Global,
                    "createResourceGpuCatalogEntry",
                )
                .await?;
                return Ok("createResourceGpuCatalogEntry");
            }
            _ => return Err(ApiError::bad_request("LW_AUTH_RESOURCE_PATH_REJECTED")),
        },
        ["", "api", "v1", "resource", "rates"] => match *method {
            Method::GET => {
                authorize_resource_scope(
                    state,
                    session,
                    contracts::AuthorizationScope::Global,
                    "listResourceRates",
                )
                .await?;
                return Ok("listResourceRates");
            }
            Method::POST => {
                let _rate = contracts::parse_strict_json::<
                    contracts::http::CreateResourceRateRequest,
                >(body)
                .map_err(|_| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))?;
                authorize_resource_scope(
                    state,
                    session,
                    contracts::AuthorizationScope::Global,
                    "createResourceRate",
                )
                .await?;
                return Ok("createResourceRate");
            }
            _ => return Err(ApiError::bad_request("LW_AUTH_RESOURCE_PATH_REJECTED")),
        },
        ["", "api", "v1", "resource", "usage"] if *method == Method::POST => {
            let usage =
                contracts::parse_strict_json::<contracts::http::RecordResourceUsageRequest>(body)
                    .map_err(|_| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))?;
            authorize_resource_scope(
                state,
                session,
                contracts::AuthorizationScope::Project {
                    project_id: usage.project_id,
                },
                "recordResourceUsage",
            )
            .await?;
            return Ok("recordResourceUsage");
        }
        ["", "api", "v1", "projects", project_id, "resource-budget"] => {
            let project_id = project_id
                .parse()
                .map_err(|_| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))?;
            match *method {
                Method::GET => {
                    authorize_resource_scope(
                        state,
                        session,
                        contracts::AuthorizationScope::Project { project_id },
                        "getProjectResourceBudget",
                    )
                    .await?;
                    return Ok("getProjectResourceBudget");
                }
                Method::PUT => {
                    let budget = contracts::parse_strict_json::<
                        contracts::http::UpsertResourceBudgetRequest,
                    >(body)
                    .map_err(|_| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))?;
                    if budget.project_id != project_id {
                        return Err(ApiError::forbidden("LW_AUTH_SCOPE_DENIED"));
                    }
                    authorize_resource_scope(
                        state,
                        session,
                        contracts::AuthorizationScope::Project { project_id },
                        "upsertProjectResourceBudget",
                    )
                    .await?;
                    return Ok("upsertProjectResourceBudget");
                }
                _ => return Err(ApiError::bad_request("LW_AUTH_RESOURCE_PATH_REJECTED")),
            }
        }
        ["", "api", "v1", "projects", project_id, "charges"] if *method == Method::GET => {
            let project_id = project_id
                .parse()
                .map_err(|_| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))?;
            authorize_resource_scope(
                state,
                session,
                contracts::AuthorizationScope::Project { project_id },
                "listProjectResourceCharges",
            )
            .await?;
            return Ok("listProjectResourceCharges");
        }
        [
            "",
            "api",
            "v1",
            "projects",
            project_id,
            "charges",
            charge_id,
            "adjustments",
        ] if *method == Method::POST => {
            let project_id = project_id
                .parse()
                .map_err(|_| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))?;
            let _charge_id = charge_id
                .parse::<contracts::ChargeId>()
                .map_err(|_| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))?;
            let _adjustment = contracts::parse_strict_json::<
                contracts::http::CreateResourceAdjustmentRequest,
            >(body)
            .map_err(|_| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))?;
            authorize_resource_scope(
                state,
                session,
                contracts::AuthorizationScope::Project { project_id },
                "createProjectResourceChargeAdjustment",
            )
            .await?;
            return Ok("createProjectResourceChargeAdjustment");
        }
        _ => {}
    }
    if *method == Method::POST && path == "/api/v1/resource-requests" {
        let request = contracts::parse_strict_json::<contracts::http::CreateResourceRequest>(body)
            .map_err(|_| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))?;
        authorize_resource_scope(
            state,
            session,
            contracts::AuthorizationScope::Project {
                project_id: request.project_id,
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
        let Query(_query) = Query::<EmptyResourceListQuery>::try_from_uri(uri)
            .map_err(|_| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))?;
        let operation = if path.ends_with("leases") {
            "listResourceLeases"
        } else {
            "listResourceRequests"
        };
        authorize_resource_scope(
            state,
            session,
            contracts::AuthorizationScope::Global,
            operation,
        )
        .await?;
        return Ok(operation);
    }
    let (request, operation) = resource_target_request(state, session, method, path).await?;
    authorize_resource_scope(
        state,
        session,
        contracts::AuthorizationScope::Project {
            project_id: request.project_id,
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
    let request = state.resource_proxy.client.get(upstream).header(
        RESOURCE_DELEGATION_HEADER,
        auth::encode_resource_delegation(
            &state.resource_proxy.delegation_key,
            session,
            OffsetDateTime::now_utc(),
        )
        .map_err(|_| ApiError::unavailable("LW_AUTH_RESOURCE_DELEGATION_INVALID"))?,
    );
    let request = attach_service_token(
        request,
        &state.resource_proxy.service_token_client,
        &state.resource_proxy.service_token_target,
    )
    .await?;
    let response = request
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
    let policy = contracts::operation_contract(operation_id)
        .ok_or_else(|| ApiError::forbidden("LW_AUTH_SCOPE_DENIED"))?;
    authorize_resource_actor(actor, memberships, scope, policy, OffsetDateTime::now_utc())
        .map_err(ApiError::from)?;
    Ok(())
}

fn authorize_resource_actor(
    actor: contracts::AuthenticatedActor,
    memberships: auth::MembershipSnapshot,
    scope: contracts::AuthorizationScope,
    policy: &contracts::http::OperationContract,
    now: OffsetDateTime,
) -> Result<(), auth::AuthorizationError> {
    let scope = resource_authorization_scope(&actor, scope, policy);
    auth::authorize(
        &auth::AuthorizationContext {
            actor,
            course_memberships: memberships.course_memberships,
            project_memberships: memberships.project_memberships,
            now,
        },
        scope,
        &policy.allowed_roles.iter().copied().collect(),
    )
    .map(|_| ())
}

fn resource_authorization_scope(
    actor: &contracts::AuthenticatedActor,
    scope: contracts::AuthorizationScope,
    policy: &contracts::http::OperationContract,
) -> contracts::AuthorizationScope {
    if policy.scope == contracts::OperationScopeKind::Project
        && matches!(scope, contracts::AuthorizationScope::Project { .. })
        && actor
            .roles
            .contains(&contracts::PlatformRole::PlatformAdmin)
        && policy
            .allowed_roles
            .contains(&contracts::PlatformRole::PlatformAdmin)
    {
        contracts::AuthorizationScope::Global
    } else {
        scope
    }
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
        .map_err(|_error| {
            tracing::warn!(
                event = "access.runtime_proxy.unavailable",
                diagnostic_code = "LW_ACCESS_RUNTIME_UNAVAILABLE",
                environment_id = %target.environment_id,
                endpoint_grant_id = %endpoint_grant_id,
                error_kind = "upstream_transport",
                failure_stage = "runtime_request",
                retryable = true
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

#[allow(
    clippy::too_many_lines,
    reason = "runtime authorization keeps grant, environment, and endpoint fences together"
)]
async fn authorize_runtime(
    state: &AppState,
    actor_id: uuid::Uuid,
    endpoint_grant_id: contracts::EndpointGrantId,
) -> Result<RuntimeTarget, ApiError> {
    let now = OffsetDateTime::now_utc();
    let row = sqlx::query(
        "SELECT g.project_id,g.course_id,g.environment_id,g.environment_revision,g.contract,\
                eg.endpoint_id,eg.endpoint_revision,eg.protocol,eg.expires_at,\
                pm.expires_at AS project_membership_expires_at,cm.expires_at AS course_membership_expires_at \
         FROM access.endpoint_grants eg JOIN access.access_grants g ON g.grant_id=eg.grant_id \
         JOIN access.project_memberships pm ON pm.project_id=g.project_id AND pm.actor_id=g.actor_id \
         LEFT JOIN access.course_memberships cm ON g.course_id IS NOT NULL AND cm.course_id=g.course_id AND cm.actor_id=g.actor_id \
           AND cm.role=CASE g.contract->>'subjectKind' WHEN 'owner' THEN 'student' WHEN 'course_teacher' THEN 'teacher' ELSE '' END \
         WHERE eg.endpoint_grant_id=$1 AND g.actor_id=$2 AND g.state='active' \
            AND g.not_before<=$3 AND g.expires_at>$3 AND eg.expires_at>$3 \
            AND eg.protocol IN ('http','https') AND eg.health='healthy' \
            AND pm.state='active' AND (pm.expires_at IS NULL OR pm.expires_at>$3) \
            AND pm.role=CASE g.contract->>'subjectKind' \
              WHEN 'owner' THEN 'student' WHEN 'course_teacher' THEN 'teacher' ELSE '' END \
            AND (g.course_id IS NULL OR (cm.state='active' AND (cm.expires_at IS NULL OR cm.expires_at>$3)))",
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
    let project_id = row
        .get::<uuid::Uuid, _>("project_id")
        .to_string()
        .parse()
        .map_err(|_| ApiError::internal("LW_ACCESS_STORE_CORRUPT"))?;
    let course_id = row
        .get::<Option<uuid::Uuid>, _>("course_id")
        .map(|value| {
            value
                .to_string()
                .parse()
                .map_err(|_| ApiError::internal("LW_ACCESS_STORE_CORRUPT"))
        })
        .transpose()?;
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
                project_id,
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
    request: ForwardRequest,
) -> Result<Response, ApiError> {
    let ForwardRequest {
        method,
        uri,
        headers,
        body,
        valid_path,
        scope,
    } = request;
    if !matches!(
        method,
        Method::GET | Method::POST | Method::DELETE | Method::PATCH
    ) {
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
    let request = if let Some(scope) = scope {
        let request = request.header("x-labweaver-project-id", scope.project_id.to_string());
        match scope.course_id {
            Some(course_id) => request.header("x-labweaver-course-id", course_id.to_string()),
            None => request,
        }
    } else {
        request
    };
    let request = attach_service_token(
        request,
        &proxy.service_token_client,
        &proxy.service_token_target,
    )
    .await?;
    let started = Instant::now();
    let response = request.send().await.map_err(|error| {
        tracing::warn!(
            event = "auth.control_gateway.unavailable",
            diagnostic_code = "LW_AUTH_CONTROL_UNAVAILABLE",
            error_kind = reqwest_error_kind(&error),
            failure_stage = "control_request",
            retryable = error.is_timeout() || error.is_connect(),
            duration_ms = elapsed_millis(started),
            safe_detail = "redacted_unclassified",
        );
        ApiError::unavailable("LW_AUTH_CONTROL_UNAVAILABLE")
    })?;
    forward_control_response(proxy, &method, response).await
}

async fn forward_control_response(
    proxy: &ControlGatewayProxy,
    method: &Method,
    response: reqwest::Response,
) -> Result<Response, ApiError> {
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

async fn attach_service_token(
    request: reqwest::RequestBuilder,
    service_token_client: &ServiceTokenClient,
    service_token_target: &ServiceTokenTarget,
) -> Result<reqwest::RequestBuilder, ApiError> {
    let mut headers = HeaderMap::new();
    service_token_client
        .bearer_auth_for(
            &mut headers,
            &service_token_target.audience,
            &service_token_target.scopes,
        )
        .await
        .map_err(|_| ApiError::unavailable("LW_AUTH_SERVICE_TOKEN_UNAVAILABLE"))?;
    Ok(request.headers(headers))
}

fn elapsed_millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn reqwest_error_kind(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "connect"
    } else if error.is_builder() {
        "builder"
    } else if error.is_request() {
        "request"
    } else if error.is_body() {
        "body"
    } else if error.is_decode() {
        "decode"
    } else {
        "transport"
    }
}

async fn authorize_environment_create(
    state: &AppState,
    headers: &HeaderMap,
    body: &Bytes,
) -> Result<(), ApiError> {
    let request = contracts::parse_strict_json::<contracts::http::CreateEnvironmentRequest>(body)
        .map_err(|_| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))?;
    authorize_environment_project(state, headers, request.project_id, "createEnvironment").await
}

async fn authorize_environment_project(
    state: &AppState,
    headers: &HeaderMap,
    project_id: contracts::ProjectId,
    operation_id: &'static str,
) -> Result<(), ApiError> {
    let session = authenticated_session(state, headers).await?;
    let actor = super::actor_from_session(&session)?;
    let memberships = auth::load_membership_snapshot(&state.pool, session.actor_id)
        .await
        .map_err(ApiError::from)?;
    let policy = contracts::operation_contract(operation_id)
        .ok_or_else(|| ApiError::forbidden("LW_AUTH_SCOPE_DENIED"))?;
    auth::authorize(
        &auth::AuthorizationContext {
            actor,
            course_memberships: memberships.course_memberships,
            project_memberships: memberships.project_memberships,
            now: time::OffsetDateTime::now_utc(),
        },
        contracts::AuthorizationScope::Project { project_id },
        &policy.allowed_roles.iter().copied().collect(),
    )
    .map_err(ApiError::from)?;
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct EnvironmentFreezeScope {
    project_id: contracts::ProjectId,
    course_id: Option<contracts::CourseId>,
}

/// Authorizes a freeze against the project recorded by the active Access
/// grant for this exact environment.  A course is an optional association;
/// when present it must agree with the grant and its membership is checked in
/// addition to the project membership.
async fn authorize_environment_freeze(
    state: &AppState,
    headers: &HeaderMap,
    environment_id: contracts::EnvironmentId,
    requested_course_id: Option<contracts::CourseId>,
    operation_id: &'static str,
) -> Result<EnvironmentFreezeScope, ApiError> {
    let session = authenticated_session(state, headers).await?;
    let now = OffsetDateTime::now_utc();
    let rows = sqlx::query(
        "SELECT DISTINCT project_id,course_id FROM access.access_grants \
         WHERE actor_id=$1 AND environment_id=$2 AND state='active' \
           AND not_before <= $3 AND expires_at > $3",
    )
    .bind(session.actor_id)
    .bind(environment_id.as_uuid())
    .bind(now)
    .fetch_all(&state.pool)
    .await
    .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    if rows.is_empty() {
        return Err(ApiError::forbidden("LW_AUTH_SCOPE_DENIED"));
    }

    let mut scope = None;
    for row in rows {
        let project_id = row
            .get::<uuid::Uuid, _>("project_id")
            .to_string()
            .parse()
            .map_err(|_| ApiError::internal("LW_ACCESS_STORE_CORRUPT"))?;
        let course_id = row
            .get::<Option<uuid::Uuid>, _>("course_id")
            .map(|value| {
                value
                    .to_string()
                    .parse()
                    .map_err(|_| ApiError::internal("LW_ACCESS_STORE_CORRUPT"))
            })
            .transpose()?;
        if requested_course_id.is_some() && requested_course_id != course_id {
            continue;
        }
        let candidate = EnvironmentFreezeScope {
            project_id,
            course_id,
        };
        if scope.is_some_and(|existing| existing != candidate) {
            return Err(ApiError::forbidden("LW_AUTH_SCOPE_DENIED"));
        }
        scope = Some(candidate);
    }
    let scope = scope.ok_or_else(|| ApiError::forbidden("LW_AUTH_SCOPE_DENIED"))?;

    authorize_environment_project(state, headers, scope.project_id, operation_id).await?;
    if let Some(course_id) = scope.course_id {
        authorize_environment_course(state, headers, course_id, operation_id).await?;
    }
    Ok(scope)
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
    let policy = contracts::operation_contract(operation_id)
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
        "x-request-id",
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
        "x-request-id",
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
    (path.starts_with("/api/v1/courses/")
        || path == "/api/v1/projects"
        || path.starts_with("/api/v1/projects/"))
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
        ) || matches!(
            segments.as_slice(),
            ["", "api", "v1", "courses", _, "me", "evaluation-results"]
                | ["", "api", "v1", "courses", _, "me", "evaluation-results", _]
        ))
}

#[allow(
    clippy::unnested_or_patterns,
    reason = "the route whitelist groups related resource paths for direct review"
)]
fn valid_resource_path(path: &str) -> bool {
    if !safe_path(path) {
        return false;
    }
    let segments = path.split('/').collect::<Vec<_>>();
    matches!(
        segments.as_slice(),
        ["", "api", "v1", "resource-requests" | "resource-leases"]
            | ["", "api", "v1", "resource-requests" | "resource-leases", _]
            | ["", "api", "v1", "projects", _, "resource-requests"]
            | ["", "api", "v1", "projects", _, "resource-leases"]
            | ["", "api", "v1", "resource", "gpu-catalog"]
            | ["", "api", "v1", "resource", "rates"]
            | ["", "api", "v1", "resource", "usage"]
            | ["", "api", "v1", "projects", _, "resource-budget"]
            | ["", "api", "v1", "projects", _, "charges"]
    ) || matches!(
        segments.as_slice(),
        ["", "api", "v1", "resource-requests", _, action]
            if matches!(*action, "approve" | "resize-and-approve" | "cancel" | "reject" | "retry")
    ) || matches!(
        segments.as_slice(),
        ["", "api", "v1", "resource-leases", _, action]
            if matches!(*action, "renew" | "revoke")
    ) || matches!(
        segments.as_slice(),
        ["", "api", "v1", "projects", _, "charges", _, "adjustments"]
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
    use std::str::FromStr;

    use contracts::{
        ActorId, AuthenticatedActor, MembershipState, PlatformRole, ProjectId, ProjectMembership,
        Revision, UtcTimestamp,
    };
    use time::OffsetDateTime;

    use super::{
        authorize_resource_actor, parse_runtime_path, valid_control_path, valid_evaluation_path,
        valid_resource_path,
    };

    fn timestamp(value: &str) -> UtcTimestamp {
        UtcTimestamp::from_str(value)
            .unwrap_or_else(|error| unreachable!("test timestamp must be valid: {error}"))
    }

    fn decision_time(value: &str) -> OffsetDateTime {
        OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|error| unreachable!("test decision time must be valid: {error}"))
    }

    fn actor(actor_id: ActorId, roles: Vec<PlatformRole>, expires_at: &str) -> AuthenticatedActor {
        AuthenticatedActor {
            actor_id,
            roles,
            expires_at: timestamp(expires_at),
        }
    }

    fn project_membership(
        actor_id: ActorId,
        project_id: ProjectId,
        role: PlatformRole,
    ) -> ProjectMembership {
        ProjectMembership {
            course_id: None,
            project_id,
            actor_id,
            role,
            state: MembershipState::Active,
            revision: Revision::new(1)
                .unwrap_or_else(|error| unreachable!("test revision must be nonzero: {error}")),
            expires_at: None,
        }
    }

    fn authorize_resource_operation(
        actor: AuthenticatedActor,
        project_id: ProjectId,
        project_memberships: Vec<ProjectMembership>,
        operation_id: &str,
        now: OffsetDateTime,
    ) -> Result<(), auth::AuthorizationError> {
        let policy = contracts::operation_contract(operation_id).unwrap_or_else(|| {
            unreachable!("resource operation must exist in the contract catalog")
        });
        authorize_resource_actor(
            actor,
            auth::MembershipSnapshot {
                course_memberships: Vec::new(),
                project_memberships,
            },
            contracts::AuthorizationScope::Project { project_id },
            policy,
            now,
        )
    }

    #[test]
    fn control_paths_are_bounded_to_course_and_project_apis() {
        assert!(valid_control_path("/api/v1/courses/course-1/agent-runs"));
        assert!(valid_control_path("/api/v1/projects/project-1/events"));
        assert!(valid_control_path("/api/v1/projects"));
        assert!(!valid_control_path("/internal/v1/auth/decision"));
        assert!(!valid_control_path("/api/v1/courses/../internal"));
        assert!(!valid_control_path("/api/v1/courses/a%2Finternal"));
        assert!(!valid_control_path("/api/v1/courses//agent-runs"));
        assert!(!valid_control_path("/api/v1/projects/../internal"));
        assert!(!valid_control_path("/api/v1/projects/a%2Finternal"));
    }

    #[test]
    fn evaluation_paths_are_exact_and_injection_safe() {
        assert!(valid_evaluation_path(
            "/api/v1/environments/01900000-0000-7000-8000-000000000001/freeze"
        ));
        assert!(valid_evaluation_path(
            "/api/v1/frozen-submissions/01900000-0000-7000-8000-000000000001"
        ));
        assert!(valid_evaluation_path(
            "/api/v1/courses/01900000-0000-7000-8000-000000000001/me/evaluation-results"
        ));
        assert!(valid_evaluation_path(
            "/api/v1/courses/01900000-0000-7000-8000-000000000001/me/evaluation-results/01900000-0000-7000-8000-000000000002"
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
            "/api/v1/projects/01900000-0000-7000-8000-000000000001/resource-requests"
        ));
        assert!(valid_resource_path(
            "/api/v1/projects/01900000-0000-7000-8000-000000000001/resource-leases"
        ));
        assert!(valid_resource_path("/api/v1/resource/gpu-catalog"));
        assert!(valid_resource_path("/api/v1/resource/rates"));
        assert!(valid_resource_path("/api/v1/resource/usage"));
        assert!(valid_resource_path(
            "/api/v1/projects/01900000-0000-7000-8000-000000000001/resource-budget"
        ));
        assert!(valid_resource_path(
            "/api/v1/projects/01900000-0000-7000-8000-000000000001/charges"
        ));
        assert!(valid_resource_path(
            "/api/v1/projects/01900000-0000-7000-8000-000000000001/charges/01900000-0000-7000-8000-000000000002/adjustments"
        ));
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
        assert!(!valid_resource_path(
            "/api/v1/resource/gpu-catalog/internal"
        ));
        assert!(!valid_resource_path(
            "/api/v1/projects/01900000-0000-7000-8000-000000000001/charges//adjustments"
        ));
        assert!(!valid_resource_path(
            "/api/v1/projects/01900000-0000-7000-8000-000000000001/resource-budget/extra"
        ));
    }

    #[test]
    fn platform_admin_can_read_and_approve_without_project_membership() {
        let actor_id = ActorId::new();
        let actor = actor(
            actor_id,
            vec![PlatformRole::PlatformAdmin],
            "2026-07-15T00:00:00.000Z",
        );
        let project_id = ProjectId::new();
        let now = decision_time("2026-07-14T00:00:00Z");

        for operation_id in ["getResourceRequest", "approveResourceRequest"] {
            assert_eq!(
                authorize_resource_operation(
                    actor.clone(),
                    project_id,
                    Vec::new(),
                    operation_id,
                    now,
                ),
                Ok(()),
                "PlatformAdmin must use global authorization for {operation_id}",
            );
        }
    }

    #[test]
    fn ordinary_actor_cannot_read_a_cross_project_resource_request() {
        let actor_id = ActorId::new();
        let member_project_id = ProjectId::new();
        let requested_project_id = ProjectId::new();
        let result = authorize_resource_operation(
            actor(
                actor_id,
                vec![PlatformRole::Student],
                "2026-07-15T00:00:00.000Z",
            ),
            requested_project_id,
            vec![project_membership(
                actor_id,
                member_project_id,
                PlatformRole::Student,
            )],
            "getResourceRequest",
            decision_time("2026-07-14T00:00:00Z"),
        );

        assert_eq!(result, Err(auth::AuthorizationError::ProjectScopeDenied));
    }

    #[test]
    fn expired_platform_admin_is_rejected_by_global_resource_authorization() {
        let result = authorize_resource_operation(
            actor(
                ActorId::new(),
                vec![PlatformRole::PlatformAdmin],
                "2026-07-13T00:00:00.000Z",
            ),
            ProjectId::new(),
            Vec::new(),
            "approveResourceRequest",
            decision_time("2026-07-14T00:00:00Z"),
        );

        assert_eq!(result, Err(auth::AuthorizationError::IdentityExpired));
    }

    #[test]
    fn admin_role_does_not_bypass_operations_that_exclude_platform_admin() {
        let result = authorize_resource_operation(
            actor(
                ActorId::new(),
                vec![PlatformRole::PlatformAdmin, PlatformRole::Student],
                "2026-07-15T00:00:00.000Z",
            ),
            ProjectId::new(),
            Vec::new(),
            "cancelResourceRequest",
            decision_time("2026-07-14T00:00:00Z"),
        );

        assert_eq!(result, Err(auth::AuthorizationError::ProjectScopeDenied));
    }
}
