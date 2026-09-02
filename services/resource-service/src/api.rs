//! Access-BFF authenticated Resource request and Lease HTTP boundary.

use axum::{
    Json, Router,
    extract::{Extension, Path, Query, Request, State},
    http::{HeaderMap, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use contracts::http::{
    ApproveResourceRequest, CreateResourceRequest, RenewResourceLease, ResourceOperationAccepted,
    StrongEtag,
};
use contracts::resource::{ResourceRequest, ResourceRequestState, ResourceTarget};
use contracts::{ActorId, LeaseId, ResourceRequestId, Revision, UtcTimestamp};
use serde::Deserialize;
use std::sync::Arc;

use crate::ApprovalPolicy;
use crate::store::{PendingAllocation, PgResourceStore, ResourceStoreError};

const ACCESS_CALLER_SAN: &str = "spiffe://labweaver/access-service";
const DELEGATION_HEADER: &str = "x-labweaver-resource-delegation";

/// Identity extracted from a CA-verified client certificate by the Resource TLS boundary.
///
/// This type is intentionally not constructible from HTTP headers. The only production
/// constructor is the mTLS accept loop below, which verifies the certificate chain and exact
/// URI SAN before injecting this extension into the Axum request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceCallerPrincipal {
    san_uri: String,
    actor_id: ActorId,
    roles: Vec<contracts::PlatformRole>,
    session_id: contracts::BffSessionId,
}

#[derive(Clone)]
pub struct ResourceApiState {
    store: PgResourceStore,
}

impl ResourceApiState {
    #[must_use]
    pub fn new(store: PgResourceStore) -> Self {
        Self { store }
    }
}

pub fn resource_api_router(state: ResourceApiState) -> Router {
    let router = Router::new()
        .route("/api/v1/resource-requests", post(create_request))
        .route("/api/v1/resource-requests", get(list_requests))
        .route("/api/v1/resource-requests/{request_id}", get(get_request))
        .route(
            "/api/v1/resource-requests/{request_id}/approve",
            post(approve_request),
        )
        .route(
            "/api/v1/resource-requests/{request_id}/resize-and-approve",
            post(approve_request),
        )
        .route(
            "/api/v1/resource-requests/{request_id}/cancel",
            post(cancel_request),
        )
        .route(
            "/api/v1/resource-requests/{request_id}/reject",
            post(reject_request),
        )
        .route(
            "/api/v1/resource-requests/{request_id}/retry",
            post(retry_request),
        )
        .route("/api/v1/resource-leases/{lease_id}", get(get_lease))
        .route("/api/v1/resource-leases", get(list_leases))
        .route(
            "/api/v1/resource-leases/{lease_id}/renew",
            post(renew_lease),
        )
        .route(
            "/api/v1/resource-leases/{lease_id}/revoke",
            post(revoke_lease),
        )
        .with_state(state);
    telemetry::instrument_http(router, "resource-service", "resource-api")
}

/// Serves Resource API routes over plain HTTP for private single-university delivery.
///
/// The outer gateway mTLS is optionally kept at the edge; inner hops are behind `NetworkPolicy`.
pub async fn serve_plain(
    listener: tokio::net::TcpListener,
    router: Router,
    delegation_key: Arc<Vec<u8>>,
) -> Result<(), std::io::Error> {
    let service = router.layer(axum::middleware::from_fn(
        move |mut request: Request, next: Next| {
            let delegation_key = Arc::clone(&delegation_key);
            async move {
                let Some(token) = request
                    .headers()
                    .get(DELEGATION_HEADER)
                    .and_then(|value| value.to_str().ok())
                else {
                    return (
                        StatusCode::FORBIDDEN,
                        "LW_AUTH_RESOURCE_DELEGATION_REQUIRED",
                    )
                        .into_response();
                };
                let delegation =
                    match auth::decode_resource_delegation(delegation_key.as_slice(), token) {
                        Ok(delegation) => delegation,
                        Err(_error) => {
                            tracing::warn!(
                                event = "resource.delegation.denied",
                                diagnostic_code = "LW_AUTH_RESOURCE_DELEGATION_INVALID",
                                error_kind = "delegation",
                                failure_stage = "delegation_validation",
                                retryable = false
                            );
                            return (StatusCode::FORBIDDEN, "LW_AUTH_RESOURCE_DELEGATION_INVALID")
                                .into_response();
                        }
                    };
                request.extensions_mut().insert(ResourceCallerPrincipal {
                    san_uri: ACCESS_CALLER_SAN.to_owned(),
                    actor_id: delegation.actor_id,
                    roles: delegation.roles,
                    session_id: delegation.session_id,
                });
                next.run(request).await
            }
        },
    ));
    axum::serve(listener, service).await
}

#[derive(Debug, Deserialize)]
struct ResourceListQuery {
    course_id: contracts::CourseId,
}

async fn list_requests(
    State(state): State<ResourceApiState>,
    Extension(principal): Extension<ResourceCallerPrincipal>,
    Query(query): Query<ResourceListQuery>,
) -> Result<Json<Vec<ResourceRequest>>, ResourceApiError> {
    authorize(&principal)?;
    let actor = principal.actor_id;
    let requests = if is_admin(&principal)? {
        state.store.list_for_course(query.course_id).await?
    } else {
        state.store.list_owned(actor, query.course_id).await?
    };
    Ok(Json(requests))
}

async fn create_request(
    State(state): State<ResourceApiState>,
    Extension(context): Extension<telemetry::RequestContext>,
    Extension(principal): Extension<ResourceCallerPrincipal>,
    headers: HeaderMap,
    Json(input): Json<CreateResourceRequest>,
) -> Result<Response, ResourceApiError> {
    authorize(&principal)?;
    let actor = principal.actor_id;
    let idempotency = required_header(&headers, "idempotency-key")?;
    let now = state.store.current_time().await?;
    let request = ResourceRequest {
        id: ResourceRequestId::new(),
        generation: 1,
        request_key: input.request_key,
        requester_id: actor,
        course_id: input.course_id,
        project_id: Some(input.project_id),
        target: ResourceTarget {
            environment_id: input.environment_id,
            release_id: input.release_id,
            release_version: input.release_version,
        },
        requested_resources: input.resources,
        requested_duration_seconds: input.duration_seconds,
        state: ResourceRequestState::Reviewing,
        revision: Revision::new(1).map_err(|_| ResourceApiError::Invalid)?,
        created_at: now,
        updated_at: now,
        diagnostic_code: None,
    };
    let stored = state
        .store
        .create(&idempotency, &request, context.trace_id())
        .await?;
    let accepted = ResourceOperationAccepted {
        request_id: stored.id,
        lease_id: None,
        revision: stored.revision,
        status_url: format!("/api/v1/resource-requests/{}", stored.id),
    };
    Ok((StatusCode::ACCEPTED, Json(accepted)).into_response())
}

async fn get_request(
    State(state): State<ResourceApiState>,
    Extension(principal): Extension<ResourceCallerPrincipal>,
    Path(request_id): Path<ResourceRequestId>,
) -> Result<Response, ResourceApiError> {
    authorize(&principal)?;
    let request = state.store.load(request_id).await?;
    scoped_or_admin(&principal, request.requester_id)?;
    let revision = request.revision;
    with_etag(Json(request), revision)
}

async fn approve_request(
    State(state): State<ResourceApiState>,
    Extension(context): Extension<telemetry::RequestContext>,
    Extension(principal): Extension<ResourceCallerPrincipal>,
    headers: HeaderMap,
    Path(request_id): Path<ResourceRequestId>,
    Json(input): Json<ApproveResourceRequest>,
) -> Result<Response, ResourceApiError> {
    authorize(&principal)?;
    require_admin(&principal)?;
    let approver = principal.actor_id;
    let idempotency = required_header(&headers, "idempotency-key")?;
    let request = state.store.load(request_id).await?;
    if request.revision != input.expected_revision {
        return Err(ResourceApiError::RevisionConflict);
    }
    let now = state.store.current_time().await?;
    let valid_until = UtcTimestamp::from_utc(
        now.get()
            + time::Duration::seconds(
                i64::try_from(input.duration_seconds).map_err(|_| ResourceApiError::Invalid)?,
            ),
    )
    .map_err(|_| ResourceApiError::Invalid)?;
    let approval = contracts::resource::ResourceApproval {
        id: contracts::ResourceApprovalId::new(),
        request_id,
        request_revision: input.expected_revision,
        approver_id: approver,
        provider_binding: input.provider_binding.clone(),
        approved_resources: input.resources.clone(),
        approved_duration_seconds: input.duration_seconds,
        reason: input.reason,
        valid_until,
        created_at: now,
    };
    let claim = contracts::resource::CapacityClaim {
        id: contracts::CapacityClaimId::new(),
        request_id,
        approval_id: approval.id,
        provider_binding: approval.provider_binding.clone(),
        workload_resources: input.resources.clone(),
        quota_resources: input.resources,
        state: contracts::resource::CapacityClaimState::Reserved,
        revision: Revision::new(1).map_err(|_| ResourceApiError::Invalid)?,
    };
    let allocation = PendingAllocation {
        claim,
        lease_id: LeaseId::new(),
    };
    let next = state
        .store
        .approve(
            &idempotency,
            request_id,
            &approval,
            &allocation,
            ApprovalPolicy {
                min_duration_seconds: 60,
                max_duration_seconds: 86_400,
                gpu_capacity: 0,
            },
            context.trace_id(),
        )
        .await?;
    let accepted = ResourceOperationAccepted {
        request_id: next.id,
        lease_id: Some(allocation.lease_id),
        revision: next.revision,
        status_url: format!("/api/v1/resource-requests/{}", next.id),
    };
    Ok((StatusCode::ACCEPTED, Json(accepted)).into_response())
}

async fn cancel_request(
    State(state): State<ResourceApiState>,
    Extension(context): Extension<telemetry::RequestContext>,
    Extension(principal): Extension<ResourceCallerPrincipal>,
    headers: HeaderMap,
    Path(request_id): Path<ResourceRequestId>,
    Json(input): Json<contracts::http::ResourceRequestMutation>,
) -> Result<Response, ResourceApiError> {
    terminal_request(
        state,
        principal,
        headers,
        request_id,
        input,
        ResourceRequestState::Cancelled,
        context.trace_id(),
    )
    .await
}

async fn reject_request(
    State(state): State<ResourceApiState>,
    Extension(context): Extension<telemetry::RequestContext>,
    Extension(principal): Extension<ResourceCallerPrincipal>,
    headers: HeaderMap,
    Path(request_id): Path<ResourceRequestId>,
    Json(input): Json<contracts::http::ResourceRequestMutation>,
) -> Result<Response, ResourceApiError> {
    authorize(&principal)?;
    require_admin(&principal)?;
    terminal_request(
        state,
        principal,
        headers,
        request_id,
        input,
        ResourceRequestState::Rejected,
        context.trace_id(),
    )
    .await
}

async fn retry_request(
    State(state): State<ResourceApiState>,
    Extension(context): Extension<telemetry::RequestContext>,
    Extension(principal): Extension<ResourceCallerPrincipal>,
    headers: HeaderMap,
    Path(request_id): Path<ResourceRequestId>,
    Json(input): Json<contracts::http::ResourceRequestMutation>,
) -> Result<Response, ResourceApiError> {
    authorize(&principal)?;
    require_admin(&principal)?;
    let actor = principal.actor_id;
    let key = required_header(&headers, "idempotency-key")?;
    let result = state
        .store
        .retry(
            &key,
            request_id,
            input.expected_revision,
            actor,
            context.trace_id(),
        )
        .await?;
    let accepted = ResourceOperationAccepted {
        request_id: result.id,
        lease_id: None,
        revision: result.revision,
        status_url: format!("/api/v1/resource-requests/{}", result.id),
    };
    Ok((StatusCode::ACCEPTED, Json(accepted)).into_response())
}

async fn terminal_request(
    state: ResourceApiState,
    principal: ResourceCallerPrincipal,
    headers: HeaderMap,
    request_id: ResourceRequestId,
    input: contracts::http::ResourceRequestMutation,
    terminal: ResourceRequestState,
    trace_id: &str,
) -> Result<Response, ResourceApiError> {
    authorize(&principal)?;
    if input.reason.trim().is_empty() || input.reason.chars().count() > 500 {
        return Err(ResourceApiError::Invalid);
    }
    let actor = principal.actor_id;
    let key = required_header(&headers, "idempotency-key")?;
    let request = state.store.load(request_id).await?;
    if request.requester_id != actor && terminal == ResourceRequestState::Cancelled {
        return Err(ResourceApiError::ScopeDenied);
    }
    let result = state
        .store
        .reject_or_cancel(
            &key,
            request_id,
            input.expected_revision,
            terminal,
            actor,
            trace_id,
        )
        .await?;
    let accepted = ResourceOperationAccepted {
        request_id: result.id,
        lease_id: None,
        revision: result.revision,
        status_url: format!("/api/v1/resource-requests/{}", result.id),
    };
    Ok((StatusCode::ACCEPTED, Json(accepted)).into_response())
}

async fn get_lease(
    State(state): State<ResourceApiState>,
    Extension(principal): Extension<ResourceCallerPrincipal>,
    Path(lease_id): Path<LeaseId>,
) -> Result<Response, ResourceApiError> {
    authorize(&principal)?;
    let lease = state.store.load_lease(lease_id).await?;
    let request = state.store.load(lease.request_id).await?;
    scoped_or_admin(&principal, request.requester_id)?;
    let revision = lease.revision;
    with_etag(Json(lease), revision)
}

async fn list_leases(
    State(state): State<ResourceApiState>,
    Extension(principal): Extension<ResourceCallerPrincipal>,
    Query(query): Query<ResourceListQuery>,
) -> Result<Json<Vec<contracts::resource::ResourceLease>>, ResourceApiError> {
    authorize(&principal)?;
    let actor = principal.actor_id;
    let leases = if is_admin(&principal)? {
        state.store.list_leases_for_course(query.course_id).await?
    } else {
        state
            .store
            .list_owned_leases(actor, query.course_id)
            .await?
    };
    Ok(Json(leases))
}

async fn renew_lease(
    State(state): State<ResourceApiState>,
    Extension(context): Extension<telemetry::RequestContext>,
    Extension(principal): Extension<ResourceCallerPrincipal>,
    headers: HeaderMap,
    Path(lease_id): Path<LeaseId>,
    Json(input): Json<RenewResourceLease>,
) -> Result<Response, ResourceApiError> {
    authorize(&principal)?;
    require_admin(&principal)?;
    let key = required_header(&headers, "idempotency-key")?;
    let now = state.store.current_time().await?;
    let expires = UtcTimestamp::from_utc(
        now.get()
            + time::Duration::seconds(
                i64::try_from(input.duration_seconds).map_err(|_| ResourceApiError::Invalid)?,
            ),
    )
    .map_err(|_| ResourceApiError::Invalid)?;
    let lease = state
        .store
        .renew_lease(
            &key,
            lease_id,
            input.expected_revision,
            expires,
            context.trace_id(),
        )
        .await?;
    let revision = lease.revision;
    with_etag(Json(lease), revision)
}

async fn revoke_lease(
    State(state): State<ResourceApiState>,
    Extension(context): Extension<telemetry::RequestContext>,
    Extension(principal): Extension<ResourceCallerPrincipal>,
    headers: HeaderMap,
    Path(lease_id): Path<LeaseId>,
    Json(input): Json<contracts::http::ResourceRequestMutation>,
) -> Result<Response, ResourceApiError> {
    authorize(&principal)?;
    require_admin(&principal)?;
    let actor = principal.actor_id;
    if input.reason.trim().is_empty() || input.reason.chars().count() > 500 {
        return Err(ResourceApiError::Invalid);
    }
    let key = required_header(&headers, "idempotency-key")?;
    let lease = state
        .store
        .revoke_lease(
            &key,
            lease_id,
            input.expected_revision,
            input.reason,
            actor,
            context.trace_id(),
        )
        .await?;
    let revision = lease.revision;
    with_etag(Json(lease), revision)
}

fn authorize(principal: &ResourceCallerPrincipal) -> Result<(), ResourceApiError> {
    tracing::debug!(
        event = "resource.request.authorized",
        actor_id = %principal.actor_id,
        session_id = %principal.session_id,
        san_uri = %principal.san_uri,
    );
    if principal.san_uri != ACCESS_CALLER_SAN {
        return Err(ResourceApiError::CallerDenied);
    }
    Ok(())
}
fn scoped(principal: &ResourceCallerPrincipal, expected: ActorId) -> Result<(), ResourceApiError> {
    if principal.actor_id == expected {
        Ok(())
    } else {
        Err(ResourceApiError::ScopeDenied)
    }
}

fn scoped_or_admin(
    principal: &ResourceCallerPrincipal,
    expected: ActorId,
) -> Result<(), ResourceApiError> {
    if is_admin(principal)? {
        Ok(())
    } else {
        scoped(principal, expected)
    }
}

fn require_admin(principal: &ResourceCallerPrincipal) -> Result<(), ResourceApiError> {
    if is_admin(principal)? {
        Ok(())
    } else {
        Err(ResourceApiError::ScopeDenied)
    }
}

fn is_admin(principal: &ResourceCallerPrincipal) -> Result<bool, ResourceApiError> {
    authorize(principal)?;
    if principal.roles.is_empty() {
        return Err(ResourceApiError::CallerDenied);
    }
    Ok(principal
        .roles
        .contains(&contracts::PlatformRole::PlatformAdmin))
}
fn required_header(headers: &HeaderMap, name: &'static str) -> Result<String, ResourceApiError> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .ok_or(ResourceApiError::IdentityInvalid)
}
fn with_etag<T: serde::Serialize>(
    Json(value): Json<T>,
    revision: Revision,
) -> Result<Response, ResourceApiError> {
    let mut response = Json(value).into_response();
    response.headers_mut().insert(
        header::ETAG,
        StrongEtag::from_revision(revision)
            .header_value()
            .parse()
            .map_err(|_| ResourceApiError::Invalid)?,
    );
    Ok(response)
}

#[derive(Debug, thiserror::Error)]
pub enum ResourceApiError {
    #[error("LW_RESOURCE_GATEWAY_DENIED")]
    CallerDenied,
    #[error("LW_AUTH_IDENTITY_INVALID")]
    IdentityInvalid,
    #[error("LW_AUTH_SCOPE_DENIED")]
    ScopeDenied,
    #[error("LW_RESOURCE_REVISION_CONFLICT")]
    RevisionConflict,
    #[error("LW_RESOURCE_REQUEST_INVALID")]
    Invalid,
    #[error(transparent)]
    Store(#[from] ResourceStoreError),
}

impl IntoResponse for ResourceApiError {
    fn into_response(self) -> Response {
        let diagnostic = self.to_string();
        let diagnostic_code = diagnostic
            .split(':')
            .next()
            .unwrap_or("LW_RESOURCE_REQUEST_FAILED")
            .to_owned();
        let status = match &self {
            Self::CallerDenied | Self::IdentityInvalid | Self::ScopeDenied => StatusCode::FORBIDDEN,
            Self::Invalid => StatusCode::BAD_REQUEST,
            Self::RevisionConflict => StatusCode::PRECONDITION_FAILED,
            Self::Store(ResourceStoreError::NotFound | ResourceStoreError::LeaseNotFound) => {
                StatusCode::NOT_FOUND
            }
            Self::Store(_) => StatusCode::CONFLICT,
        };
        let retryable = matches!(
            self,
            Self::Store(ResourceStoreError::Persistence(_) | ResourceStoreError::Database(_))
        );
        tracing::warn!(
            event = "resource.api.rejected",
            component = "api-error-boundary",
            operation = "http.request",
            outcome = "rejected",
            duration_ms = 0_u64,
            diagnostic_code = diagnostic_code.as_str(),
            error_kind = "request_rejected",
            failure_stage = "resource.request.finalize",
            retryable,
            safe_detail = "request_rejected",
            http_status = status.as_u16(),
        );
        (status, diagnostic_code).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caller_must_be_the_verified_access_service_principal() {
        let principal = ResourceCallerPrincipal {
            san_uri: "spiffe://labweaver/untrusted".to_owned(),
            actor_id: ActorId::new(),
            roles: vec![contracts::PlatformRole::Teacher],
            session_id: contracts::BffSessionId::new(),
        };
        assert!(matches!(
            authorize(&principal),
            Err(ResourceApiError::CallerDenied)
        ));
        let principal = ResourceCallerPrincipal {
            san_uri: ACCESS_CALLER_SAN.to_owned(),
            actor_id: ActorId::new(),
            roles: vec![contracts::PlatformRole::Teacher],
            session_id: contracts::BffSessionId::new(),
        };
        assert!(authorize(&principal).is_ok());
    }

    #[test]
    fn delegated_identity_is_not_read_from_http_headers() {
        let principal = ResourceCallerPrincipal {
            san_uri: ACCESS_CALLER_SAN.to_owned(),
            actor_id: ActorId::new(),
            roles: vec![contracts::PlatformRole::Teacher],
            session_id: contracts::BffSessionId::new(),
        };
        let forged_actor = ActorId::new();
        assert_ne!(principal.actor_id, forged_actor);
        assert!(matches!(
            scoped(&principal, forged_actor),
            Err(ResourceApiError::ScopeDenied)
        ));
        assert!(is_admin(&principal).is_ok_and(|is_admin| !is_admin));
    }

    #[test]
    fn administrator_role_is_explicit_and_unknown_roles_fail_closed() {
        let principal = ResourceCallerPrincipal {
            san_uri: ACCESS_CALLER_SAN.to_owned(),
            actor_id: ActorId::new(),
            roles: vec![contracts::PlatformRole::PlatformAdmin],
            session_id: contracts::BffSessionId::new(),
        };
        assert!(is_admin(&principal).is_ok_and(|value| value));
    }
}
