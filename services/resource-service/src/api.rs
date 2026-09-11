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
    AcknowledgeTaskResourceRequest, ApproveResourceRequest, CreateResourceAdjustmentRequest,
    CreateResourceRateRequest, CreateResourceRequest, InternalCreateTaskResourceRequest,
    RecordResourceUsageRequest, ReleaseTaskResourceRequest, RenewResourceLease,
    ResourceOperationAccepted, StrongEtag, TaskResourceStatus, UpsertResourceBudgetRequest,
};
use contracts::resource::{
    GpuCatalogEntry, ResourceCharge, ResourceRate, ResourceRequest, ResourceRequestState,
    ResourceUsageRecord,
};
use contracts::{
    ActorId, ChargeId, LeaseId, ProjectId, ResourceRequestId, Revision, TaskRunId, UtcTimestamp,
};
use serde::Deserialize;
use std::sync::Arc;

use crate::ApprovalPolicy;
use crate::store::{PendingAllocation, PgResourceStore, ResourceStoreError};

const DELEGATION_HEADER: &str = "x-labweaver-resource-delegation";

/// User identity extracted from an Access-signed resource delegation.
///
/// This type is intentionally not constructible from HTTP headers. The production constructor
/// verifies the signed delegation before injecting this extension into the Axum request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceCallerPrincipal {
    actor_id: ActorId,
    roles: Vec<contracts::PlatformRole>,
    session_id: contracts::BffSessionId,
}

#[derive(Clone)]
pub struct ResourceApiState {
    store: PgResourceStore,
    service_verifier: Option<Arc<auth::ServiceTokenVerifier>>,
    access_service_client_id: Option<String>,
    environment_service_client_id: Option<String>,
    evaluation_service_client_id: Option<String>,
}

impl ResourceApiState {
    #[must_use]
    pub fn new(store: PgResourceStore) -> Self {
        Self {
            store,
            service_verifier: None,
            access_service_client_id: None,
            environment_service_client_id: None,
            evaluation_service_client_id: None,
        }
    }

    /// Installs the JWKS-backed verifier used only by internal service routes.
    #[must_use]
    pub fn with_service_verifier(mut self, verifier: Arc<auth::ServiceTokenVerifier>) -> Self {
        self.service_verifier = Some(verifier);
        self
    }

    /// Restricts browser-facing calls to the configured Access service client ID after JWT
    /// validation. The caller identity remains separate from the signed user delegation.
    #[must_use]
    pub fn with_access_service_client_id(mut self, client_id: String) -> Self {
        self.access_service_client_id = Some(client_id);
        self
    }

    /// Restricts Environment-owned usage records to the configured Environment service account.
    #[must_use]
    pub fn with_environment_service_client_id(mut self, client_id: String) -> Self {
        self.environment_service_client_id = Some(client_id);
        self
    }

    /// Restricts Task resources and Task usage records to the configured Evaluation service
    /// account.
    #[must_use]
    pub fn with_evaluation_service_client_id(mut self, client_id: String) -> Self {
        self.evaluation_service_client_id = Some(client_id);
        self
    }
}

pub fn resource_api_router(state: ResourceApiState) -> Router {
    let public = Router::new()
        .route("/api/v1/resource-requests", post(create_request))
        .route("/api/v1/resource-requests", get(list_requests))
        .route(
            "/api/v1/projects/{project_id}/resource-requests",
            post(create_request_for_project).get(list_requests_for_project),
        )
        .route(
            "/api/v1/projects/{project_id}/resource-leases",
            get(list_leases_for_project),
        )
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
        .route("/api/v1/resource/usage", post(record_usage))
        .route("/api/v1/resource/rates", get(list_rates).post(create_rate))
        .route(
            "/api/v1/resource/gpu-catalog",
            get(list_gpu_catalog).post(create_gpu_catalog_entry),
        )
        .route(
            "/api/v1/projects/{project_id}/resource-budget",
            get(get_budget).put(upsert_budget),
        )
        .route("/api/v1/projects/{project_id}/charges", get(list_charges))
        .route(
            "/api/v1/projects/{project_id}/charges/{charge_id}/adjustments",
            post(create_adjustment),
        )
        .layer(axum::middleware::from_fn(public_service_auth))
        .layer(Extension(state.service_verifier.clone()))
        .layer(Extension(state.access_service_client_id.clone()));
    let internal = Router::new()
        .route("/internal/v1/task-resources", post(create_task_resource))
        .route(
            "/internal/v1/task-resources/{task_run_id}/request",
            get(get_task_resource_request),
        )
        .route(
            "/internal/v1/task-resources/{task_run_id}/claim",
            post(claim_task_resource),
        )
        .route(
            "/internal/v1/task-resources/{task_run_id}/ack",
            post(acknowledge_task_resource),
        )
        .route(
            "/internal/v1/task-resources/{task_run_id}",
            get(get_task_resource),
        )
        .route(
            "/internal/v1/task-resources/{task_run_id}/release",
            post(release_task_resource),
        )
        .route(
            "/internal/v1/task-resources/{task_run_id}/cancel",
            post(cancel_task_resource),
        )
        .route("/internal/v1/resource/usage", post(record_internal_usage))
        .layer(axum::middleware::from_fn(internal_service_auth))
        .layer(Extension(state.service_verifier.clone()))
        .layer(Extension(state.environment_service_client_id.clone()))
        .layer(Extension(state.evaluation_service_client_id.clone()));
    let router = Router::new()
        .merge(public)
        .merge(internal)
        .with_state(state);
    telemetry::instrument_http(router, "resource-service", "resource-api")
}

/// Applies the Access delegation boundary to public routes. Internal routes use the service JWT
/// middleware installed by [`resource_api_router`] and deliberately bypass this layer.
pub fn with_delegation(router: Router, delegation_key: Arc<Vec<u8>>) -> Router {
    router.layer(axum::middleware::from_fn(
        move |mut request: Request, next: Next| {
            let delegation_key = Arc::clone(&delegation_key);
            async move {
                // Internal service routes authenticate with a Keycloak service JWT in their
                // route-local middleware. They must not be accepted through the browser BFF
                // delegation boundary.
                if request.uri().path().starts_with("/internal/") {
                    return next.run(request).await;
                }
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
                    actor_id: delegation.actor_id,
                    roles: delegation.roles,
                    session_id: delegation.session_id,
                });
                next.run(request).await
            }
        },
    ))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyListQuery {}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProjectListQuery {
    course_id: Option<contracts::CourseId>,
}

async fn list_requests(
    State(state): State<ResourceApiState>,
    Extension(principal): Extension<ResourceCallerPrincipal>,
    Query(_query): Query<EmptyListQuery>,
) -> Result<Json<Vec<ResourceRequest>>, ResourceApiError> {
    authorize(&principal);
    require_admin(&principal)?;
    let requests = state.store.list_all_requests().await?;
    Ok(Json(requests))
}

async fn create_request(
    State(state): State<ResourceApiState>,
    Extension(context): Extension<telemetry::RequestContext>,
    Extension(principal): Extension<ResourceCallerPrincipal>,
    headers: HeaderMap,
    Json(input): Json<CreateResourceRequest>,
) -> Result<Response, ResourceApiError> {
    authorize(&principal);
    if matches!(
        input.target,
        contracts::resource::ResourceTarget::Task { .. }
    ) {
        // Task reservations are Evaluation-owned. Exposing the Task target through the browser
        // request would let a delegated user bypass the task owner and execution lifecycle.
        return Err(ResourceApiError::ScopeDenied);
    }
    let actor = principal.actor_id;
    let idempotency = required_header(&headers, "idempotency-key")?;
    let now = state.store.current_time().await?;
    let request = ResourceRequest {
        id: ResourceRequestId::new(),
        generation: 1,
        request_key: input.request_key,
        requester_id: actor,
        course_id: input.course_id,
        project_id: input.project_id,
        target: input.target,
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

async fn create_request_for_project(
    State(state): State<ResourceApiState>,
    Extension(context): Extension<telemetry::RequestContext>,
    Extension(principal): Extension<ResourceCallerPrincipal>,
    headers: HeaderMap,
    Path(project_id): Path<ProjectId>,
    Json(input): Json<CreateResourceRequest>,
) -> Result<Response, ResourceApiError> {
    if input.project_id != project_id {
        return Err(ResourceApiError::ScopeDenied);
    }
    create_request_with_input(state, context, principal, headers, input).await
}

async fn create_request_with_input(
    state: ResourceApiState,
    context: telemetry::RequestContext,
    principal: ResourceCallerPrincipal,
    headers: HeaderMap,
    input: CreateResourceRequest,
) -> Result<Response, ResourceApiError> {
    authorize(&principal);
    if matches!(
        input.target,
        contracts::resource::ResourceTarget::Task { .. }
    ) {
        // Task reservations are Evaluation-owned. Exposing the Task target through the browser
        // request would let a delegated user bypass the task owner and execution lifecycle.
        return Err(ResourceApiError::ScopeDenied);
    }
    let actor = principal.actor_id;
    let idempotency = required_header(&headers, "idempotency-key")?;
    let now = state.store.current_time().await?;
    let request = ResourceRequest {
        id: ResourceRequestId::new(),
        generation: 1,
        request_key: input.request_key,
        requester_id: actor,
        course_id: input.course_id,
        project_id: input.project_id,
        target: input.target,
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

async fn list_requests_for_project(
    State(state): State<ResourceApiState>,
    Extension(principal): Extension<ResourceCallerPrincipal>,
    Path(project_id): Path<ProjectId>,
    Query(query): Query<ProjectListQuery>,
) -> Result<Json<Vec<ResourceRequest>>, ResourceApiError> {
    authorize(&principal);
    let course_id = query.course_id;
    let requests = if is_admin(&principal)? {
        state.store.list_for_project(project_id, course_id).await?
    } else {
        state
            .store
            .list_owned(principal.actor_id, project_id, course_id)
            .await?
    };
    Ok(Json(requests))
}

async fn get_request(
    State(state): State<ResourceApiState>,
    Extension(principal): Extension<ResourceCallerPrincipal>,
    Path(request_id): Path<ResourceRequestId>,
) -> Result<Response, ResourceApiError> {
    authorize(&principal);
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
    authorize(&principal);
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
        gpu_allocation: None,
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
    authorize(&principal);
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
    authorize(&principal);
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
    authorize(&principal);
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
    authorize(&principal);
    let lease = state.store.load_lease(lease_id).await?;
    let request = state.store.load(lease.request_id).await?;
    scoped_or_admin(&principal, request.requester_id)?;
    let revision = lease.revision;
    with_etag(Json(lease), revision)
}

async fn list_leases(
    State(state): State<ResourceApiState>,
    Extension(principal): Extension<ResourceCallerPrincipal>,
    Query(_query): Query<EmptyListQuery>,
) -> Result<Json<Vec<contracts::resource::ResourceLease>>, ResourceApiError> {
    authorize(&principal);
    require_admin(&principal)?;
    let leases = state.store.list_all_leases().await?;
    Ok(Json(leases))
}

async fn list_leases_for_project(
    State(state): State<ResourceApiState>,
    Extension(principal): Extension<ResourceCallerPrincipal>,
    Path(project_id): Path<ProjectId>,
    Query(query): Query<ProjectListQuery>,
) -> Result<Json<Vec<contracts::resource::ResourceLease>>, ResourceApiError> {
    authorize(&principal);
    let leases = if is_admin(&principal)? {
        state
            .store
            .list_leases_for_project(project_id, query.course_id)
            .await?
    } else {
        state
            .store
            .list_owned_leases(principal.actor_id, project_id, query.course_id)
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
    authorize(&principal);
    let lease = state.store.load_lease(lease_id).await?;
    let request = state.store.load(lease.request_id).await?;
    scoped_or_admin(&principal, request.requester_id)?;
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
    authorize(&principal);
    let actor = principal.actor_id;
    if input.reason.trim().is_empty() || input.reason.chars().count() > 500 {
        return Err(ResourceApiError::Invalid);
    }
    let key = required_header(&headers, "idempotency-key")?;
    let lease = state.store.load_lease(lease_id).await?;
    let request = state.store.load(lease.request_id).await?;
    scoped_or_admin(&principal, request.requester_id)?;
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

async fn record_usage(
    State(state): State<ResourceApiState>,
    Extension(principal): Extension<ResourceCallerPrincipal>,
    Json(input): Json<RecordResourceUsageRequest>,
) -> Result<Json<ResourceUsageRecord>, ResourceApiError> {
    authorize(&principal);
    require_admin(&principal)?;
    let observed_at = state.store.current_time().await?;
    Ok(Json(state.store.record_usage(&input, observed_at).await?))
}

async fn create_task_resource(
    State(state): State<ResourceApiState>,
    Extension(_identity): Extension<auth::ServiceIdentity>,
    Extension(context): Extension<telemetry::RequestContext>,
    headers: HeaderMap,
    Json(input): Json<InternalCreateTaskResourceRequest>,
) -> Result<Response, ResourceApiError> {
    let idempotency_key = required_header(&headers, "idempotency-key")?;
    let now = state.store.current_time().await?;
    let request = ResourceRequest {
        id: ResourceRequestId::new(),
        generation: 1,
        request_key: input.request_key,
        requester_id: input.owner_id,
        project_id: input.project_id,
        course_id: input.course_id,
        target: contracts::resource::ResourceTarget::Task {
            task_run_id: input.task_run_id,
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
        .create(&idempotency_key, &request, context.trace_id())
        .await?;
    Ok((StatusCode::CREATED, Json(stored)).into_response())
}

async fn get_task_resource_request(
    State(state): State<ResourceApiState>,
    Extension(_identity): Extension<auth::ServiceIdentity>,
    Path(task_run_id): Path<TaskRunId>,
) -> Result<Json<ResourceRequest>, ResourceApiError> {
    Ok(Json(state.store.load_task_request(task_run_id).await?))
}

/// Cancels a pending Evaluation task reservation while it is still awaiting approval.
///
/// The task id is resolved through Resource's indexed target projection before the existing
/// request-id transaction is invoked. This prevents a caller from cancelling a different target
/// by guessing a request id and keeps the Reviewing-only state fence in one lifecycle function.
async fn cancel_task_resource(
    State(state): State<ResourceApiState>,
    Extension(_identity): Extension<auth::ServiceIdentity>,
    Extension(context): Extension<telemetry::RequestContext>,
    headers: HeaderMap,
    Path(task_run_id): Path<TaskRunId>,
    Json(input): Json<contracts::http::ResourceRequestMutation>,
) -> Result<Json<ResourceRequest>, ResourceApiError> {
    if input.reason.trim().is_empty() || input.reason.chars().count() > 500 {
        return Err(ResourceApiError::Invalid);
    }
    let idempotency_key = required_header(&headers, "idempotency-key")?;
    let request = state.store.load_task_request(task_run_id).await?;
    let cancelled = state
        .store
        .reject_or_cancel(
            &idempotency_key,
            request.id,
            input.expected_revision,
            ResourceRequestState::Cancelled,
            request.requester_id,
            context.trace_id(),
        )
        .await
        .map_err(|error| match error {
            ResourceStoreError::Lifecycle(crate::LifecycleError::StateConflict) => {
                ResourceApiError::RevisionConflict
            }
            other => ResourceApiError::Store(other),
        })?;
    Ok(Json(cancelled))
}

async fn claim_task_resource(
    State(state): State<ResourceApiState>,
    Extension(_identity): Extension<auth::ServiceIdentity>,
    Extension(context): Extension<telemetry::RequestContext>,
    Path(task_run_id): Path<TaskRunId>,
) -> Result<Json<TaskResourceStatus>, ResourceApiError> {
    Ok(Json(
        state
            .store
            .claim_task_resource(task_run_id, context.trace_id())
            .await?,
    ))
}

async fn acknowledge_task_resource(
    State(state): State<ResourceApiState>,
    Extension(_identity): Extension<auth::ServiceIdentity>,
    Extension(context): Extension<telemetry::RequestContext>,
    Path(task_run_id): Path<TaskRunId>,
    Json(input): Json<AcknowledgeTaskResourceRequest>,
) -> Result<Json<TaskResourceStatus>, ResourceApiError> {
    Ok(Json(
        state
            .store
            .acknowledge_task_resource(
                task_run_id,
                input.expected_claim_revision,
                input.expected_lease_revision,
                &input.execution_namespace,
                context.trace_id(),
            )
            .await?,
    ))
}

async fn get_task_resource(
    State(state): State<ResourceApiState>,
    Extension(_identity): Extension<auth::ServiceIdentity>,
    Path(task_run_id): Path<TaskRunId>,
) -> Result<Json<TaskResourceStatus>, ResourceApiError> {
    Ok(Json(state.store.load_task_resource(task_run_id).await?))
}

async fn release_task_resource(
    State(state): State<ResourceApiState>,
    Extension(_identity): Extension<auth::ServiceIdentity>,
    Extension(context): Extension<telemetry::RequestContext>,
    Path(task_run_id): Path<TaskRunId>,
    Json(input): Json<ReleaseTaskResourceRequest>,
) -> Result<Json<TaskResourceStatus>, ResourceApiError> {
    Ok(Json(
        state
            .store
            .release_task_resource(
                task_run_id,
                input.expected_claim_revision,
                input.expected_lease_revision,
                context.trace_id(),
            )
            .await?,
    ))
}

async fn record_internal_usage(
    State(state): State<ResourceApiState>,
    Extension(identity): Extension<auth::ServiceIdentity>,
    Json(input): Json<RecordResourceUsageRequest>,
) -> Result<Json<ResourceUsageRecord>, ResourceApiError> {
    let environment_service_client_id = state
        .environment_service_client_id
        .as_deref()
        .ok_or(ResourceApiError::ServiceConfiguration)?;
    let evaluation_service_client_id = state
        .evaluation_service_client_id
        .as_deref()
        .ok_or(ResourceApiError::ServiceConfiguration)?;
    let observed_at = state.store.current_time().await?;
    Ok(Json(
        state
            .store
            .record_usage_internal(
                &input,
                observed_at,
                &identity,
                environment_service_client_id,
                evaluation_service_client_id,
            )
            .await?,
    ))
}

async fn list_rates(
    State(state): State<ResourceApiState>,
    Extension(principal): Extension<ResourceCallerPrincipal>,
) -> Result<Json<Vec<ResourceRate>>, ResourceApiError> {
    authorize(&principal);
    Ok(Json(state.store.list_rates().await?))
}

async fn create_rate(
    State(state): State<ResourceApiState>,
    Extension(principal): Extension<ResourceCallerPrincipal>,
    headers: HeaderMap,
    Json(input): Json<CreateResourceRateRequest>,
) -> Result<Response, ResourceApiError> {
    authorize(&principal);
    require_admin(&principal)?;
    let idempotency_key = required_header(&headers, "idempotency-key")?;
    let rate = state.store.create_rate(&idempotency_key, &input).await?;
    let revision = rate.revision;
    let mut response = (StatusCode::CREATED, Json(rate)).into_response();
    response.headers_mut().insert(
        header::ETAG,
        StrongEtag::from_revision(revision)
            .header_value()
            .parse()
            .map_err(|_| ResourceApiError::Invalid)?,
    );
    Ok(response)
}

async fn list_gpu_catalog(
    State(state): State<ResourceApiState>,
    Extension(principal): Extension<ResourceCallerPrincipal>,
) -> Result<Json<Vec<GpuCatalogEntry>>, ResourceApiError> {
    authorize(&principal);
    Ok(Json(state.store.list_gpu_catalog().await?))
}

async fn create_gpu_catalog_entry(
    State(state): State<ResourceApiState>,
    Extension(principal): Extension<ResourceCallerPrincipal>,
    headers: HeaderMap,
    Json(input): Json<GpuCatalogEntry>,
) -> Result<Response, ResourceApiError> {
    authorize(&principal);
    require_admin(&principal)?;
    let idempotency_key = required_header(&headers, "idempotency-key")?;
    let entry = state
        .store
        .create_gpu_catalog_entry(&idempotency_key, &input)
        .await?;
    let revision = entry.revision;
    let mut response = (StatusCode::CREATED, Json(entry)).into_response();
    response.headers_mut().insert(
        header::ETAG,
        StrongEtag::from_revision(revision)
            .header_value()
            .parse()
            .map_err(|_| ResourceApiError::Invalid)?,
    );
    Ok(response)
}

async fn get_budget(
    State(state): State<ResourceApiState>,
    Extension(principal): Extension<ResourceCallerPrincipal>,
    Path(project_id): Path<ProjectId>,
) -> Result<Response, ResourceApiError> {
    authorize(&principal);
    require_admin(&principal)?;
    let budget = state.store.get_budget(project_id).await?;
    let revision = budget.revision;
    with_etag(Json(budget), revision)
}

async fn upsert_budget(
    State(state): State<ResourceApiState>,
    Extension(principal): Extension<ResourceCallerPrincipal>,
    headers: HeaderMap,
    Path(project_id): Path<ProjectId>,
    Json(input): Json<UpsertResourceBudgetRequest>,
) -> Result<Response, ResourceApiError> {
    authorize(&principal);
    require_admin(&principal)?;
    if input.project_id != project_id {
        return Err(ResourceApiError::ScopeDenied);
    }
    let idempotency_key = required_header(&headers, "idempotency-key")?;
    let now = state.store.current_time().await?;
    let budget = state
        .store
        .upsert_budget(&idempotency_key, &input, now)
        .await?;
    let revision = budget.revision;
    with_etag(Json(budget), revision)
}

async fn list_charges(
    State(state): State<ResourceApiState>,
    Extension(principal): Extension<ResourceCallerPrincipal>,
    Path(project_id): Path<ProjectId>,
) -> Result<Json<Vec<ResourceCharge>>, ResourceApiError> {
    authorize(&principal);
    require_admin(&principal)?;
    Ok(Json(state.store.list_charges(project_id).await?))
}

async fn create_adjustment(
    State(state): State<ResourceApiState>,
    Extension(principal): Extension<ResourceCallerPrincipal>,
    headers: HeaderMap,
    Path((project_id, charge_id)): Path<(ProjectId, ChargeId)>,
    Json(input): Json<CreateResourceAdjustmentRequest>,
) -> Result<Response, ResourceApiError> {
    authorize(&principal);
    require_admin(&principal)?;
    let idempotency_key = required_header(&headers, "idempotency-key")?;
    let created_at = state.store.current_time().await?;
    let charge = state
        .store
        .create_adjustment(
            &idempotency_key,
            project_id,
            charge_id,
            input.amount,
            input.reason,
            principal.actor_id,
            created_at,
        )
        .await?;
    Ok((StatusCode::CREATED, Json(charge)).into_response())
}

/// Authenticates Access service calls with a service-account JWT, route permission, and client ID.
///
/// The verifier is injected by the process runtime. A missing verifier is a deployment error and
/// returns 503 rather than silently accepting a request through the browser delegation path.
async fn public_service_auth(
    Extension(verifier): Extension<Option<Arc<auth::ServiceTokenVerifier>>>,
    Extension(access_client_id): Extension<Option<String>>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(verifier) = verifier else {
        tracing::error!(
            event = "resource.public_auth.unconfigured",
            diagnostic_code = "LW_AUTH_SERVICE_CONFIG_INVALID",
            retryable = false,
        );
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "LW_AUTH_SERVICE_CONFIG_INVALID",
        )
            .into_response();
    };
    let Some(access_client_id) = access_client_id else {
        tracing::error!(
            event = "resource.public_auth.unconfigured",
            diagnostic_code = "LW_AUTH_ACCESS_CLIENT_ID_MISSING",
            retryable = false,
        );
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "LW_AUTH_ACCESS_CLIENT_ID_MISSING",
        )
            .into_response();
    };
    match verifier
        .authenticate_with_permission(request.headers(), "resource.api.invoke")
        .await
    {
        Ok(identity) if identity.client_id == access_client_id => {
            request.extensions_mut().insert(identity);
            next.run(request).await
        }
        Ok(identity) => {
            tracing::warn!(
                event = "resource.public_auth.denied",
                diagnostic_code = "LW_AUTH_ACCESS_CLIENT_ID_MISMATCH",
                client_id = identity.client_id,
                retryable = false,
            );
            (StatusCode::FORBIDDEN, "LW_AUTH_ACCESS_CLIENT_ID_MISMATCH").into_response()
        }
        Err(error @ auth::ServiceAuthError::PermissionDenied) => {
            tracing::warn!(
                event = "resource.public_auth.denied",
                diagnostic_code = %error,
                retryable = false,
            );
            (StatusCode::FORBIDDEN, error.to_string()).into_response()
        }
        Err(
            error @ (auth::ServiceAuthError::CredentialsMissing
            | auth::ServiceAuthError::TokenRejected
            | auth::ServiceAuthError::TokenExpired),
        ) => {
            tracing::warn!(
                event = "resource.public_auth.rejected",
                diagnostic_code = %error,
                retryable = false,
            );
            (StatusCode::UNAUTHORIZED, error.to_string()).into_response()
        }
        Err(error) => {
            tracing::error!(
                event = "resource.public_auth.unavailable",
                diagnostic_code = %error,
                retryable = true,
            );
            (StatusCode::SERVICE_UNAVAILABLE, error.to_string()).into_response()
        }
    }
}

/// Authenticates internal callers with a service-account JWT and route permission.
///
/// The verifier is injected by the process runtime. A missing verifier is a deployment error and
/// returns 503 rather than silently accepting a request through the browser delegation path.
async fn internal_service_auth(
    Extension(verifier): Extension<Option<Arc<auth::ServiceTokenVerifier>>>,
    Extension(evaluation_service_client_id): Extension<Option<String>>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(verifier) = verifier else {
        tracing::error!(
            event = "resource.internal_auth.unconfigured",
            diagnostic_code = "LW_AUTH_SERVICE_CONFIG_INVALID",
            retryable = false,
        );
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "LW_AUTH_SERVICE_CONFIG_INVALID",
        )
            .into_response();
    };
    let Some(permission) = internal_route_permission(request.uri().path()) else {
        return (StatusCode::NOT_FOUND, "LW_RESOURCE_ROUTE_NOT_FOUND").into_response();
    };
    let task_route = permission.starts_with("resource.task.");
    let expected_task_client_id = if task_route {
        let Some(client_id) = evaluation_service_client_id else {
            tracing::error!(
                event = "resource.internal_auth.unconfigured",
                permission,
                diagnostic_code = "LW_AUTH_EVALUATION_CLIENT_ID_MISSING",
                retryable = false,
            );
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "LW_AUTH_EVALUATION_CLIENT_ID_MISSING",
            )
                .into_response();
        };
        Some(client_id)
    } else {
        None
    };
    match verifier
        .authenticate_with_permission(request.headers(), permission)
        .await
    {
        Ok(identity)
            if expected_task_client_id
                .as_deref()
                .is_none_or(|client_id| identity.client_id == client_id) =>
        {
            request.extensions_mut().insert(identity);
            next.run(request).await
        }
        Ok(identity) => {
            tracing::warn!(
                event = "resource.internal_auth.denied",
                permission,
                diagnostic_code = "LW_AUTH_EVALUATION_CLIENT_ID_MISMATCH",
                client_id = identity.client_id,
                retryable = false,
            );
            (
                StatusCode::FORBIDDEN,
                "LW_AUTH_EVALUATION_CLIENT_ID_MISMATCH",
            )
                .into_response()
        }
        Err(error @ auth::ServiceAuthError::PermissionDenied) => {
            tracing::warn!(
                event = "resource.internal_auth.denied",
                permission,
                diagnostic_code = %error,
                retryable = false,
            );
            (StatusCode::FORBIDDEN, error.to_string()).into_response()
        }
        Err(
            error @ (auth::ServiceAuthError::CredentialsMissing
            | auth::ServiceAuthError::TokenRejected
            | auth::ServiceAuthError::TokenExpired),
        ) => {
            tracing::warn!(
                event = "resource.internal_auth.rejected",
                permission,
                diagnostic_code = %error,
                retryable = false,
            );
            (StatusCode::UNAUTHORIZED, error.to_string()).into_response()
        }
        Err(error) => {
            tracing::error!(
                event = "resource.internal_auth.unavailable",
                permission,
                diagnostic_code = %error,
                retryable = true,
            );
            (StatusCode::SERVICE_UNAVAILABLE, error.to_string()).into_response()
        }
    }
}

fn internal_route_permission(path: &str) -> Option<&'static str> {
    if path == "/internal/v1/resource/usage" {
        return Some("resource.usage.record");
    }
    if path == "/internal/v1/task-resources" {
        return Some("resource.task.create");
    }
    let task_path = path.strip_prefix("/internal/v1/task-resources/")?;
    if task_path.ends_with("/request") {
        let task_id = task_path.strip_suffix("/request")?;
        if !task_id.is_empty() && !task_id.contains('/') {
            return Some("resource.task.read");
        }
        return None;
    }
    if !task_path.is_empty() && !task_path.contains('/') {
        return Some("resource.task.read");
    }
    let (task_id, action) = task_path.split_once('/')?;
    if task_id.is_empty() || task_id.contains('/') || action.is_empty() || action.contains('/') {
        return None;
    }
    match action {
        "claim" => Some("resource.task.claim"),
        "ack" => Some("resource.task.ack"),
        "release" => Some("resource.task.release"),
        "cancel" => Some("resource.task.cancel"),
        _ => None,
    }
}

fn authorize(principal: &ResourceCallerPrincipal) {
    tracing::debug!(
        event = "resource.request.authorized",
        actor_id = %principal.actor_id,
        session_id = %principal.session_id,
    );
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
    authorize(principal);
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
    #[error("LW_AUTH_SERVICE_CONFIG_INVALID")]
    ServiceConfiguration,
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
            Self::ServiceConfiguration => StatusCode::SERVICE_UNAVAILABLE,
            Self::Invalid => StatusCode::BAD_REQUEST,
            Self::RevisionConflict => StatusCode::PRECONDITION_FAILED,
            Self::Store(
                ResourceStoreError::NotFound
                | ResourceStoreError::LeaseNotFound
                | ResourceStoreError::BudgetNotFound
                | ResourceStoreError::ChargeNotFound,
            ) => StatusCode::NOT_FOUND,
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
    fn delegated_identity_is_not_read_from_http_headers() {
        let principal = ResourceCallerPrincipal {
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
            actor_id: ActorId::new(),
            roles: vec![contracts::PlatformRole::PlatformAdmin],
            session_id: contracts::BffSessionId::new(),
        };
        assert!(is_admin(&principal).is_ok_and(|value| value));
    }

    #[test]
    fn global_list_query_is_empty_and_project_list_query_is_camel_case() {
        let empty = axum::http::Uri::from_static("/api/v1/resource-requests");
        assert!(Query::<EmptyListQuery>::try_from_uri(&empty).is_ok());
        let forged_scope = axum::http::Uri::from_static(
            "/api/v1/resource-requests?projectId=01900000-0000-7000-8000-000000000001",
        );
        assert!(Query::<EmptyListQuery>::try_from_uri(&forged_scope).is_err());

        let project = axum::http::Uri::from_static(
            "/api/v1/projects/01900000-0000-7000-8000-000000000001/resource-leases?courseId=01900000-0000-7000-8000-000000000002",
        );
        assert!(
            Query::<ProjectListQuery>::try_from_uri(&project)
                .is_ok_and(|Query(query)| query.course_id.is_some())
        );
        let snake_case = axum::http::Uri::from_static(
            "/api/v1/projects/01900000-0000-7000-8000-000000000001/resource-leases?course_id=01900000-0000-7000-8000-000000000002",
        );
        assert!(Query::<ProjectListQuery>::try_from_uri(&snake_case).is_err());
    }

    #[test]
    fn internal_route_permission_uses_the_task_id_segment() {
        assert_eq!(
            internal_route_permission("/internal/v1/task-resources"),
            Some("resource.task.create")
        );
        assert_eq!(
            internal_route_permission("/internal/v1/task-resources/01JABC/request"),
            Some("resource.task.read")
        );
        assert_eq!(
            internal_route_permission("/internal/v1/task-resources/01JABC/claim"),
            Some("resource.task.claim")
        );
        assert_eq!(
            internal_route_permission("/internal/v1/task-resources/01JABC/ack"),
            Some("resource.task.ack")
        );
        assert_eq!(
            internal_route_permission("/internal/v1/task-resources/01JABC/release"),
            Some("resource.task.release")
        );
        assert_eq!(
            internal_route_permission("/internal/v1/task-resources/01JABC/cancel"),
            Some("resource.task.cancel")
        );
        assert_eq!(
            internal_route_permission("/internal/v1/task-resources/01JABC"),
            Some("resource.task.read")
        );
        assert_eq!(
            internal_route_permission("/internal/v1/task-resources/01JABC/claim/extra"),
            None
        );
        assert_eq!(
            internal_route_permission(
                "/internal/v1/task-resources/00000000-0000-0000-0000-000000000001"
            ),
            Some("resource.task.read")
        );
    }
}
