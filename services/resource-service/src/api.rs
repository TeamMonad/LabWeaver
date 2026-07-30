//! Access-BFF authenticated Resource request and Lease HTTP boundary.

use std::str::FromStr;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
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
use uuid::Uuid;

use crate::ApprovalPolicy;
use crate::store::{PendingAllocation, PgResourceStore, ResourceStoreError};
use contracts::Sha256Digest;

const ACCESS_CALLER_SAN: &str = "spiffe://labweaver/access-service";

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
    Router::new()
        .route("/api/v1/resource-requests", post(create_request))
        .route("/api/v1/resource-requests", get(list_requests))
        .route("/api/v1/resource-requests/{request_id}", get(get_request))
        .route(
            "/api/v1/resource-requests/{request_id}/approve",
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
        .route(
            "/api/v1/resource-leases/{lease_id}/renew",
            post(renew_lease),
        )
        .route(
            "/api/v1/resource-leases/{lease_id}/revoke",
            post(revoke_lease),
        )
        .with_state(state)
}

#[derive(Debug, Deserialize)]
struct ResourceListQuery {
    course_id: contracts::CourseId,
}

async fn list_requests(
    State(state): State<ResourceApiState>,
    headers: HeaderMap,
    Query(query): Query<ResourceListQuery>,
) -> Result<Json<Vec<ResourceRequest>>, ResourceApiError> {
    authorize(&headers)?;
    let actor = actor(&headers)?;
    Ok(Json(state.store.list_owned(actor, query.course_id).await?))
}

async fn create_request(
    State(state): State<ResourceApiState>,
    headers: HeaderMap,
    Json(input): Json<CreateResourceRequest>,
) -> Result<Response, ResourceApiError> {
    authorize(&headers)?;
    let actor = actor(&headers)?;
    let idempotency = required_header(&headers, "idempotency-key")?;
    let now = state.store.current_time().await?;
    let request = ResourceRequest {
        id: ResourceRequestId::new(),
        generation: 1,
        request_key: input.request_key,
        requester_id: actor,
        course_id: input.course_id,
        project_id: input.project_id,
        target: ResourceTarget {
            environment_id: input.environment_id,
            release_id: input.release_id,
            release_version: input.release_version,
            release_sha256: input.release_sha256,
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
        .create(&idempotency, &request, &trace_id())
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
    headers: HeaderMap,
    Path(request_id): Path<ResourceRequestId>,
) -> Result<Response, ResourceApiError> {
    authorize(&headers)?;
    let request = state.store.load(request_id).await?;
    scoped(&headers, request.requester_id)?;
    let revision = request.revision;
    with_etag(Json(request), revision)
}

async fn approve_request(
    State(state): State<ResourceApiState>,
    headers: HeaderMap,
    Path(request_id): Path<ResourceRequestId>,
    Json(input): Json<ApproveResourceRequest>,
) -> Result<Response, ResourceApiError> {
    authorize(&headers)?;
    let approver = actor(&headers)?;
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
        policy_sha256: Sha256Digest::of_canonical(&input.provider_binding)
            .map_err(|_| ResourceApiError::Invalid)?,
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
        policy_sha256: approval.policy_sha256,
        workload_resources: input.resources.clone(),
        quota_resources: input.resources,
        quota_plan_sha256: Sha256Digest::of_canonical(&request.target)
            .map_err(|_| ResourceApiError::Invalid)?,
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
            &trace_id(),
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
    headers: HeaderMap,
    Path(request_id): Path<ResourceRequestId>,
    Json(input): Json<contracts::http::ResourceRequestMutation>,
) -> Result<Response, ResourceApiError> {
    terminal_request(
        state,
        headers,
        request_id,
        input,
        ResourceRequestState::Cancelled,
    )
    .await
}

async fn reject_request(
    State(state): State<ResourceApiState>,
    headers: HeaderMap,
    Path(request_id): Path<ResourceRequestId>,
    Json(input): Json<contracts::http::ResourceRequestMutation>,
) -> Result<Response, ResourceApiError> {
    terminal_request(
        state,
        headers,
        request_id,
        input,
        ResourceRequestState::Rejected,
    )
    .await
}

async fn retry_request(
    State(state): State<ResourceApiState>,
    headers: HeaderMap,
    Path(request_id): Path<ResourceRequestId>,
    Json(input): Json<contracts::http::ResourceRequestMutation>,
) -> Result<Response, ResourceApiError> {
    authorize(&headers)?;
    let actor = actor(&headers)?;
    let key = required_header(&headers, "idempotency-key")?;
    let request = state.store.load(request_id).await?;
    if request.requester_id != actor {
        return Err(ResourceApiError::ScopeDenied);
    }
    let result = state
        .store
        .retry(
            &key,
            request_id,
            input.expected_revision,
            actor,
            &trace_id(),
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
    headers: HeaderMap,
    request_id: ResourceRequestId,
    input: contracts::http::ResourceRequestMutation,
    terminal: ResourceRequestState,
) -> Result<Response, ResourceApiError> {
    authorize(&headers)?;
    if input.reason.trim().is_empty() || input.reason.chars().count() > 500 {
        return Err(ResourceApiError::Invalid);
    }
    let actor = actor(&headers)?;
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
            &trace_id(),
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
    headers: HeaderMap,
    Path(lease_id): Path<LeaseId>,
) -> Result<Response, ResourceApiError> {
    authorize(&headers)?;
    let lease = state.store.load_lease(lease_id).await?;
    let revision = lease.revision;
    with_etag(Json(lease), revision)
}

async fn renew_lease(
    State(state): State<ResourceApiState>,
    headers: HeaderMap,
    Path(lease_id): Path<LeaseId>,
    Json(input): Json<RenewResourceLease>,
) -> Result<Response, ResourceApiError> {
    authorize(&headers)?;
    let _actor = actor(&headers)?;
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
        .renew_lease(&key, lease_id, input.expected_revision, expires)
        .await?;
    let revision = lease.revision;
    with_etag(Json(lease), revision)
}

async fn revoke_lease(
    State(state): State<ResourceApiState>,
    headers: HeaderMap,
    Path(lease_id): Path<LeaseId>,
    Json(input): Json<contracts::http::ResourceRequestMutation>,
) -> Result<Response, ResourceApiError> {
    authorize(&headers)?;
    let _actor = actor(&headers)?;
    if input.reason.trim().is_empty() || input.reason.chars().count() > 500 {
        return Err(ResourceApiError::Invalid);
    }
    let key = required_header(&headers, "idempotency-key")?;
    let lease = state
        .store
        .revoke_lease(&key, lease_id, input.expected_revision, input.reason)
        .await?;
    let revision = lease.revision;
    with_etag(Json(lease), revision)
}

fn authorize(headers: &HeaderMap) -> Result<(), ResourceApiError> {
    if headers
        .get("x-labweaver-caller-san")
        .and_then(|v| v.to_str().ok())
        != Some(ACCESS_CALLER_SAN)
    {
        return Err(ResourceApiError::CallerDenied);
    }
    Ok(())
}
fn scoped(headers: &HeaderMap, expected: ActorId) -> Result<(), ResourceApiError> {
    if actor(headers)? != expected {
        Err(ResourceApiError::ScopeDenied)
    } else {
        Ok(())
    }
}
fn actor(headers: &HeaderMap) -> Result<ActorId, ResourceApiError> {
    required_header(headers, "x-labweaver-actor-id")
        .and_then(|v| ActorId::from_str(&v).map_err(|_| ResourceApiError::IdentityInvalid))
}
fn required_header(headers: &HeaderMap, name: &'static str) -> Result<String, ResourceApiError> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .ok_or(ResourceApiError::IdentityInvalid)
}
fn trace_id() -> String {
    format!("resource-api-{}", Uuid::now_v7())
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
        let status = match self {
            Self::CallerDenied | Self::IdentityInvalid | Self::ScopeDenied => StatusCode::FORBIDDEN,
            Self::Invalid => StatusCode::BAD_REQUEST,
            Self::RevisionConflict => StatusCode::PRECONDITION_FAILED,
            Self::Store(ResourceStoreError::NotFound | ResourceStoreError::LeaseNotFound) => {
                StatusCode::NOT_FOUND
            }
            _ => StatusCode::CONFLICT,
        };
        (status, self.to_string()).into_response()
    }
}
