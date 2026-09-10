//! Access-BFF authenticated public Environment lifecycle API.

use std::{str::FromStr, sync::Arc, time::Duration};

use axum::{
    Json, Router,
    body::Bytes,
    extract::{Extension, Path, Query, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use contracts::{
    ActorId, DiagnosticCode, EnvironmentId, Revision, UtcTimestamp,
    authoring::{EnvironmentClass, EnvironmentRuntimeSpec},
    environment::{
        EndpointHealth, EnvironmentAccessEligibilityState, EnvironmentAccessEligibilitySummary,
        EnvironmentCreateSpec, EnvironmentExecutionBinding, EnvironmentExecutionBindingRequest,
        EnvironmentExecutionPurpose, EnvironmentInstance, EnvironmentLeaseVerificationRequest,
        EnvironmentLifecycleCommand, EnvironmentOperationKind, EnvironmentOwnerRelation,
        EnvironmentOwnerSummary, EnvironmentResetTarget, EnvironmentSummary,
        EnvironmentWorkConfigurationTarget, EnvironmentWorkConfigurationTargetQuery,
        ResourceWorkCleanup, ResourceWorkCleanupStatus, ResourceWorkHandoff,
        ResourceWorkLeaseUpdate,
    },
    http::{
        ContainerWorkExecutionQuery, ContainerWorkExecutionReceipt, ContainerWorkExecutionRequest,
        CreateEnvironmentRequest, DEFAULT_PAGE_LIMIT, EnvironmentInventoryQuery,
        EnvironmentOperationAccepted, EnvironmentOperationListQuery, IdempotencyKey,
        ResetEnvironmentRequest, SnapshotPage, StrongEtag,
    },
    submission::{EnvironmentFreezeBinding, EnvironmentFreezeBindingRequest},
};
use uuid::Uuid;

use crate::{
    ContainerReleaseResolver, ContainerWorkExecutionService, EnvironmentInventoryFilter,
    EnvironmentStoreError, FreezeBindingError, FreezeBindingService, NatsAccessRevoker,
    NatsMessagingError, NatsResourceLeaseVerifier, PgEnvironmentStore, PgReleaseProjectionStore,
    ReleaseProjectionError, WorkExecutionError, work_execution::validate_work_environment,
};

const ACCESS_PERMISSION: &str = "access.environment.forward";
const EVALUATION_PERMISSION: &str = "evaluation.environment.freeze";
const EVALUATION_EXECUTION_PERMISSION: &str = "environment:resolve_evaluation_execution_binding";
const WORK_EXECUTION_PERMISSION: &str = "environment:resolve_work_execution_binding";
const WORK_READ_PERMISSION: &str = "environment.work.read";
const RESOURCE_PERMISSION: &str = "resource.environment.manage";
const WORK_CONFIGURATION_PERMISSION: &str = "environment.work.configure";
const ACTOR_HEADER: &str = "x-labweaver-actor-id";
const SESSION_HEADER: &str = "x-labweaver-session-id";
const OPERATION_DEADLINE: Duration = Duration::from_mins(15);

/// Environment-owned dependencies used by the public API.
#[derive(Clone)]
pub struct EnvironmentApiState {
    pub(crate) store: PgEnvironmentStore,
    pub(crate) releases: PgReleaseProjectionStore,
    access_revoker: NatsAccessRevoker,
    lease_verifier: NatsResourceLeaseVerifier,
    freeze_bindings: FreezeBindingService,
    pub(crate) work_executions: Option<ContainerWorkExecutionService>,
}

impl EnvironmentApiState {
    #[must_use]
    pub const fn new(
        store: PgEnvironmentStore,
        releases: PgReleaseProjectionStore,
        access_revoker: NatsAccessRevoker,
        lease_verifier: NatsResourceLeaseVerifier,
        freeze_bindings: FreezeBindingService,
    ) -> Self {
        Self {
            store,
            releases,
            access_revoker,
            lease_verifier,
            freeze_bindings,
            work_executions: None,
        }
    }

    /// Installs the production Work execution owner after startup dependencies are ready.
    #[must_use]
    pub fn with_work_executions(mut self, service: ContainerWorkExecutionService) -> Self {
        self.work_executions = Some(service);
        self
    }
}

/// Builds the Environment API routes.
pub fn environment_api_router(state: EnvironmentApiState) -> Router {
    let router = Router::new()
        .route(
            "/api/v1/environments",
            get(list_environments).post(create_environment),
        )
        .route(
            "/api/v1/environments/{environment_id}",
            get(get_environment),
        )
        .route(
            "/api/v1/environments/{environment_id}/operations",
            get(list_environment_operations),
        )
        .route(
            "/api/v1/environments/{environment_id}/operations/{operation_id}",
            get(get_environment_operation),
        )
        .route(
            "/api/v1/environments/{environment_id}/start",
            post(start_environment),
        )
        .route(
            "/api/v1/environments/{environment_id}/stop",
            post(stop_environment),
        )
        .route(
            "/api/v1/environments/{environment_id}/restart",
            post(restart_environment),
        )
        .route(
            "/api/v1/environments/{environment_id}/retry",
            post(retry_environment),
        )
        .route(
            "/api/v1/environments/{environment_id}/reset",
            post(reset_environment),
        )
        .route(
            "/api/v1/environments/{environment_id}/cancel",
            post(cancel_environment),
        )
        .route(
            "/api/v1/environments/{environment_id}/recover",
            post(recover_environment),
        )
        .route(
            "/api/v1/environments/{environment_id}",
            axum::routing::delete(delete_environment),
        )
        .route(
            "/api/v1/environments/{environment_id}/endpoints",
            get(list_endpoints),
        )
        .route(
            "/internal/v1/environments/{environment_id}/freeze-binding",
            post(resolve_freeze_binding),
        )
        .route(
            "/internal/v1/environments/{environment_id}/execution-binding/evaluation",
            post(resolve_evaluation_execution_binding),
        )
        .route(
            "/internal/v1/environments/{environment_id}/execution-binding/work",
            post(resolve_work_execution_binding),
        )
        .route(
            "/internal/v1/environments/{environment_id}/work-configuration-target",
            get(resolve_work_configuration_target),
        )
        .route(
            "/internal/v1/resource/work-handoffs",
            post(accept_resource_work_handoff),
        )
        .route(
            "/internal/v1/resource/work-lease-updates",
            post(accept_resource_work_lease_update),
        )
        .route(
            "/internal/v1/resource/work-cleanups",
            post(accept_resource_work_cleanup),
        )
        .route(
            "/internal/v1/resource/work-cleanups/{environment_id}",
            get(read_resource_work_cleanup),
        )
        .route(
            "/internal/v1/work-configurations",
            post(start_work_configuration),
        )
        .route(
            "/internal/v1/work-configurations/{run_id}",
            get(query_work_configuration),
        )
        .route(
            "/internal/v1/work-configurations/{run_id}/cancel",
            post(cancel_work_configuration),
        )
        .with_state(state);
    telemetry::instrument_http(router, "environment-service", "environment-api")
}

async fn start_work_configuration(
    State(state): State<EnvironmentApiState>,
    caller: Option<Extension<auth::ServiceIdentity>>,
    body: Bytes,
) -> Result<(StatusCode, Json<ContainerWorkExecutionReceipt>), EnvironmentApiError> {
    require_permission(caller, WORK_CONFIGURATION_PERMISSION)?;
    let request = contracts::parse_strict_json::<ContainerWorkExecutionRequest>(&body)
        .map_err(|_| EnvironmentApiError::RequestInvalid)?;
    let service = state
        .work_executions
        .ok_or(EnvironmentApiError::WorkExecutionUnavailable)?;
    let receipt = service
        .start(request)
        .await
        .map_err(EnvironmentApiError::WorkExecution)?;
    Ok((StatusCode::ACCEPTED, Json(receipt)))
}

async fn query_work_configuration(
    State(state): State<EnvironmentApiState>,
    caller: Option<Extension<auth::ServiceIdentity>>,
    Path(run_id): Path<contracts::AgentRunId>,
    Query(query): Query<ContainerWorkExecutionQuery>,
) -> Result<Json<ContainerWorkExecutionReceipt>, EnvironmentApiError> {
    require_permission(caller, WORK_CONFIGURATION_PERMISSION)?;
    let service = state
        .work_executions
        .ok_or(EnvironmentApiError::WorkExecutionUnavailable)?;
    Ok(Json(
        service
            .query(run_id, &query)
            .await
            .map_err(EnvironmentApiError::WorkExecution)?,
    ))
}

async fn cancel_work_configuration(
    State(state): State<EnvironmentApiState>,
    caller: Option<Extension<auth::ServiceIdentity>>,
    Path(run_id): Path<contracts::AgentRunId>,
    Query(query): Query<ContainerWorkExecutionQuery>,
) -> Result<Json<ContainerWorkExecutionReceipt>, EnvironmentApiError> {
    require_permission(caller, WORK_CONFIGURATION_PERMISSION)?;
    let service = state
        .work_executions
        .ok_or(EnvironmentApiError::WorkExecutionUnavailable)?;
    Ok(Json(
        service
            .cancel(run_id, &query)
            .await
            .map_err(EnvironmentApiError::WorkExecution)?,
    ))
}

/// Applies the service-account JWT boundary to an Environment route tree.
///
/// The TLS transport protects the bearer token in transit. The verifier is the
/// only source of the caller identity; no request header or client certificate
/// is treated as an identity assertion.
pub fn with_service_auth(router: Router, verifier: Arc<auth::ServiceTokenVerifier>) -> Router {
    router.layer(middleware::from_fn_with_state(
        verifier,
        require_service_token,
    ))
}

async fn require_service_token(
    State(verifier): State<Arc<auth::ServiceTokenVerifier>>,
    request: Request,
    next: Next,
) -> Response {
    match verifier.authenticate(request.headers()).await {
        Ok(identity) => {
            let mut request = request;
            request.extensions_mut().insert(identity);
            next.run(request).await
        }
        Err(error) => EnvironmentApiError::ServiceAuth(error).into_response(),
    }
}

async fn accept_resource_work_lease_update(
    State(state): State<EnvironmentApiState>,
    caller: Option<Extension<auth::ServiceIdentity>>,
    body: Bytes,
) -> Result<Json<EnvironmentInstance>, EnvironmentApiError> {
    require_resource_service(caller)?;
    let update = contracts::parse_strict_json::<ResourceWorkLeaseUpdate>(&body)
        .map_err(|_| EnvironmentApiError::RequestInvalid)?;
    update
        .validate()
        .map_err(|_| EnvironmentApiError::RequestInvalid)?;
    let now = state.store.current_time().await?;
    let authorization = state
        .lease_verifier
        .verify(
            EnvironmentLeaseVerificationRequest {
                version: 1,
                lease_id: update.lease_id,
                environment_id: update.environment_id,
                project_id: update.project_id,
                course_id: update.course_id,
                owner_actor_id: update.owner_actor_id,
                capacity_binding: update.capacity_binding,
            },
            now,
        )
        .await?;
    if authorization.lease_revision != update.lease_revision
        || authorization.expires_at != update.expires_at
    {
        return Err(EnvironmentApiError::LeaseFenceInvalid);
    }
    Ok(Json(
        state
            .store
            .refresh_work_lease(update.environment_id, authorization)
            .await?,
    ))
}

async fn accept_resource_work_cleanup(
    State(state): State<EnvironmentApiState>,
    caller: Option<Extension<auth::ServiceIdentity>>,
    body: Bytes,
) -> Result<(StatusCode, Json<EnvironmentOperationAccepted>), EnvironmentApiError> {
    require_resource_service(caller)?;
    let cleanup = contracts::parse_strict_json::<ResourceWorkCleanup>(&body)
        .map_err(|_| EnvironmentApiError::RequestInvalid)?;
    cleanup
        .validate()
        .map_err(|_| EnvironmentApiError::RequestInvalid)?;
    let instance = state.store.load(cleanup.environment_id).await?;
    if instance.class != EnvironmentClass::Work
        || instance.lease_id != Some(cleanup.lease_id)
        || instance.project_id != cleanup.project_id
        || instance.course_id != cleanup.course_id
        || instance.owner_id != cleanup.owner_actor_id
        || instance.capacity_binding.as_deref() != Some(cleanup.capacity_binding.as_str())
    {
        return Err(EnvironmentApiError::LeaseFenceInvalid);
    }
    if instance
        .operation
        .lease_authorization
        .as_ref()
        .is_none_or(|authorization| authorization.lease_revision != cleanup.lease_revision)
    {
        return Err(EnvironmentApiError::LeaseFenceInvalid);
    }
    if instance.observed_state == contracts::environment::ObservedEnvironmentState::Deleted
        || matches!(
            instance.operation.kind,
            EnvironmentOperationKind::Expire
                | EnvironmentOperationKind::Delete
                | EnvironmentOperationKind::Cleanup
        )
    {
        return Ok((
            StatusCode::ACCEPTED,
            Json(EnvironmentOperationAccepted {
                environment_id: instance.id,
                operation_id: instance.operation.id,
                revision: instance.operation.accepted_revision,
                status_url: format!(
                    "/api/v1/environments/{}/operations/{}",
                    instance.id, instance.operation.id
                ),
            }),
        ));
    }
    let access_revocation_revision = state
        .access_revoker
        .revoke(&instance, "environment_expired")
        .await?;
    let accepted_at = state.store.current_time().await?;
    let command = EnvironmentLifecycleCommand {
        environment_id: instance.id,
        kind: EnvironmentOperationKind::Expire,
        expected_revision: instance.revision,
        actor_id: instance.owner_id,
        trace_id: cleanup.trace_id,
        accepted_at,
        deadline_at: add_duration(accepted_at, OPERATION_DEADLINE)?,
        access_revocation_revision: Some(access_revocation_revision),
        preserve_mutable_disk: false,
        max_attempts: 3,
        reset_target: None,
    };
    let accepted = state
        .store
        .accept_api_command(
            &format!(
                "resource-work-cleanup-{}-{}",
                cleanup.lease_id,
                cleanup.lease_revision.get()
            ),
            &command,
            None,
            None,
            instance.project_id,
            instance.course_id,
        )
        .await?;
    Ok((StatusCode::ACCEPTED, Json(accepted)))
}

async fn read_resource_work_cleanup(
    State(state): State<EnvironmentApiState>,
    caller: Option<Extension<auth::ServiceIdentity>>,
    Path(environment_id): Path<EnvironmentId>,
) -> Result<Json<ResourceWorkCleanupStatus>, EnvironmentApiError> {
    require_resource_service(caller)?;
    Ok(Json(state.store.load_cleanup_status(environment_id).await?))
}

fn require_resource_service(
    caller: Option<Extension<auth::ServiceIdentity>>,
) -> Result<(), EnvironmentApiError> {
    require_permission(caller, RESOURCE_PERMISSION)
}

async fn accept_resource_work_handoff(
    State(state): State<EnvironmentApiState>,
    caller: Option<Extension<auth::ServiceIdentity>>,
    body: Bytes,
) -> Result<(StatusCode, Json<EnvironmentOperationAccepted>), EnvironmentApiError> {
    require_permission(caller, RESOURCE_PERMISSION)?;
    let handoff = contracts::parse_strict_json::<ResourceWorkHandoff>(&body)
        .map_err(|_| EnvironmentApiError::RequestInvalid)?;
    handoff
        .validate()
        .map_err(|_| EnvironmentApiError::RequestInvalid)?;
    let release = state
        .releases
        .resolve(handoff.release_id, handoff.release_version)
        .await?;
    if release.withdrawn_at.is_some()
        || release.projection.release.project_id != handoff.project_id
        || release.projection.release.course_id != handoff.course_id
        || release.projection.environment_spec.class != EnvironmentClass::Work
    {
        return Err(EnvironmentApiError::ReleaseDenied);
    }
    let provider_binding = match &release.projection.environment_spec.runtime {
        EnvironmentRuntimeSpec::Container {
            provider_binding, ..
        }
        | EnvironmentRuntimeSpec::VirtualMachine {
            provider_binding, ..
        } => provider_binding,
    };
    if provider_binding != &handoff.provider_binding {
        return Err(EnvironmentApiError::ReleaseDenied);
    }
    let now = state.store.current_time().await?;
    let authorization = state
        .lease_verifier
        .verify(
            EnvironmentLeaseVerificationRequest {
                version: 1,
                lease_id: handoff.lease_id,
                environment_id: handoff.environment_id,
                project_id: handoff.project_id,
                course_id: handoff.course_id,
                owner_actor_id: handoff.owner_actor_id,
                capacity_binding: handoff.capacity_binding.clone(),
            },
            now,
        )
        .await?;
    if authorization.lease_revision != handoff.lease_revision {
        return Err(EnvironmentApiError::LeaseFenceInvalid);
    }
    let command = EnvironmentLifecycleCommand {
        environment_id: handoff.environment_id,
        kind: EnvironmentOperationKind::Create,
        expected_revision: Revision::new(1).map_err(|_| EnvironmentApiError::RequestInvalid)?,
        actor_id: handoff.owner_actor_id,
        trace_id: handoff.trace_id.clone(),
        accepted_at: now,
        deadline_at: add_duration(now, OPERATION_DEADLINE)?,
        access_revocation_revision: None,
        preserve_mutable_disk: false,
        max_attempts: 3,
        reset_target: None,
    };
    let create = EnvironmentCreateSpec {
        project_id: handoff.project_id,
        course_id: handoff.course_id,
        owner_actor_id: handoff.owner_actor_id,
        display_label: handoff.display_label,
        class: EnvironmentClass::Work,
        runtime_kind: release.projection.release.runtime_kind,
        release_id: handoff.release_id,
        release_version: handoff.release_version,
        provider_binding: handoff.provider_binding,
        lease_id: Some(handoff.lease_id),
        capacity_binding: Some(handoff.capacity_binding),
        eligibility_expires_at: release.projection.environment_spec.retention.retain_until,
    };
    let idempotency_key = format!(
        "resource-work-{}-{}",
        handoff.request_id,
        handoff.request_revision.get()
    );
    let accepted = state
        .store
        .accept_api_command(
            &idempotency_key,
            &command,
            Some(&create),
            Some(authorization),
            handoff.project_id,
            handoff.course_id,
        )
        .await?;
    Ok((StatusCode::ACCEPTED, Json(accepted)))
}

async fn resolve_freeze_binding(
    State(state): State<EnvironmentApiState>,
    caller: Option<Extension<auth::ServiceIdentity>>,
    Path(environment_id): Path<EnvironmentId>,
    body: Bytes,
) -> Result<Json<EnvironmentFreezeBinding>, EnvironmentApiError> {
    require_permission(caller, EVALUATION_PERMISSION)?;
    let request = contracts::parse_strict_json::<EnvironmentFreezeBindingRequest>(&body)
        .map_err(|_| EnvironmentApiError::RequestInvalid)?;
    Ok(Json(
        state
            .freeze_bindings
            .resolve(environment_id, &request)
            .await?,
    ))
}

async fn resolve_evaluation_execution_binding(
    State(state): State<EnvironmentApiState>,
    caller: Option<Extension<auth::ServiceIdentity>>,
    Path(environment_id): Path<EnvironmentId>,
    body: Bytes,
) -> Result<Json<EnvironmentExecutionBinding>, EnvironmentApiError> {
    require_permission(caller, EVALUATION_EXECUTION_PERMISSION)?;
    let request = parse_execution_binding_request(&body)?;
    if !matches!(
        &request.purpose,
        EnvironmentExecutionPurpose::EvaluationProbe { .. }
    ) {
        return Err(EnvironmentApiError::RequestInvalid);
    }
    Ok(Json(
        state
            .freeze_bindings
            .resolve_execution(environment_id, &request)
            .await?,
    ))
}

async fn resolve_work_execution_binding(
    State(state): State<EnvironmentApiState>,
    caller: Option<Extension<auth::ServiceIdentity>>,
    Path(environment_id): Path<EnvironmentId>,
    body: Bytes,
) -> Result<Json<EnvironmentExecutionBinding>, EnvironmentApiError> {
    require_permission(caller, WORK_EXECUTION_PERMISSION)?;
    let request = parse_execution_binding_request(&body)?;
    if !matches!(
        &request.purpose,
        EnvironmentExecutionPurpose::WorkConfiguration { .. }
            | EnvironmentExecutionPurpose::WorkConfigurationRecovery { .. }
    ) {
        return Err(EnvironmentApiError::RequestInvalid);
    }
    Ok(Json(
        state
            .freeze_bindings
            .resolve_execution(environment_id, &request)
            .await?,
    ))
}

async fn resolve_work_configuration_target(
    State(state): State<EnvironmentApiState>,
    caller: Option<Extension<auth::ServiceIdentity>>,
    Path(environment_id): Path<EnvironmentId>,
    Query(query): Query<EnvironmentWorkConfigurationTargetQuery>,
) -> Result<Response, EnvironmentApiError> {
    require_permission(caller, WORK_READ_PERMISSION)?;
    let now = state.store.current_time().await?;
    let instance = state.store.load(environment_id).await?;
    if instance.project_id != query.project_id
        || instance.course_id != query.course_id
        || instance.owner_id != query.actor_id
    {
        return Err(EnvironmentApiError::ScopeDenied);
    }
    if instance.revision != query.expected_revision {
        return Err(EnvironmentApiError::RevisionConflict);
    }
    validate_work_environment(
        &instance,
        query.project_id,
        query.course_id,
        query.actor_id,
        query.expected_revision,
        now,
    )
    .map_err(EnvironmentApiError::WorkExecution)?;
    let target = EnvironmentWorkConfigurationTarget {
        environment_id,
        environment_revision: instance.revision,
        project_id: instance.project_id,
        course_id: instance.course_id,
        actor_id: instance.owner_id,
        runtime_kind: instance.runtime_kind,
    };
    target
        .validate_for(environment_id, &query)
        .map_err(|_| EnvironmentApiError::ResponseInvalid)?;
    let mut response = Json(target).into_response();
    response.headers_mut().insert(
        header::ETAG,
        HeaderValue::from_str(&StrongEtag::from_revision(instance.revision).header_value())
            .map_err(|_| EnvironmentApiError::ResponseInvalid)?,
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

fn parse_execution_binding_request(
    body: &Bytes,
) -> Result<EnvironmentExecutionBindingRequest, EnvironmentApiError> {
    let request = contracts::parse_strict_json::<EnvironmentExecutionBindingRequest>(body)
        .map_err(|_| EnvironmentApiError::RequestInvalid)?;
    request
        .validate()
        .map_err(|_| EnvironmentApiError::RequestInvalid)?;
    Ok(request)
}

async fn list_environments(
    State(state): State<EnvironmentApiState>,
    caller: Option<Extension<auth::ServiceIdentity>>,
    Query(query): Query<EnvironmentInventoryQuery>,
    headers: HeaderMap,
) -> Result<Json<SnapshotPage<EnvironmentSummary>>, EnvironmentApiError> {
    require_access_bff(caller)?;
    require_session(&headers)?;
    query
        .validate()
        .map_err(|_| EnvironmentApiError::RequestInvalid)?;
    let page = state
        .store
        .list_owned(
            EnvironmentInventoryFilter {
                project_id: query.project_id,
                course_id: query.course_id,
                owner_actor_id: actor(&headers)?,
                runtime_kind: query.runtime_kind,
                class: query.class,
                desired_state: query.desired_state,
                observed_state: query.observed_state,
                release_id: query.release_id,
            },
            query.cursor.as_deref(),
            query.limit.unwrap_or(DEFAULT_PAGE_LIMIT),
        )
        .await?;
    let snapshot_at = page.snapshot_at;
    let records = page.records;
    let next_cursor = page.next_cursor;
    let snapshot_sequence = page.snapshot_sequence;
    let items = records
        .into_iter()
        .map(|record| environment_summary(record, snapshot_at))
        .collect::<Result<Vec<_>, EnvironmentApiError>>()?;
    Ok(Json(SnapshotPage {
        items,
        next_cursor,
        snapshot_sequence,
        snapshot_at,
    }))
}

fn environment_summary(
    record: crate::StoredEnvironmentInventory,
    snapshot_at: UtcTimestamp,
) -> Result<EnvironmentSummary, EnvironmentApiError> {
    let healthy_endpoint_count = u32::try_from(
        record
            .instance
            .endpoints
            .iter()
            .filter(|endpoint| endpoint.health == EndpointHealth::Healthy)
            .count(),
    )
    .map_err(|_| EnvironmentApiError::ResponseInvalid)?;
    let eligible =
        healthy_endpoint_count > 0 && record.instance.eligibility_expires_at > snapshot_at;
    let summary = EnvironmentSummary {
        id: record.instance.id,
        display_label: record.instance.display_label,
        project_id: record.instance.project_id,
        course_id: record.instance.course_id,
        owner: EnvironmentOwnerSummary {
            relation: EnvironmentOwnerRelation::SelfOwned,
            display_label: None,
        },
        class: record.instance.class,
        runtime_kind: record.instance.runtime_kind,
        release_id: record.instance.release_id,
        release_version: record.instance.release_version,
        desired_state: record.instance.desired_state,
        observed_state: record.instance.observed_state,
        revision: record.instance.revision,
        eligibility_expires_at: record.instance.eligibility_expires_at,
        created_at: record.created_at,
        updated_at: record.updated_at,
        last_changed_stream_sequence: record.stream_sequence,
        current_operation: record.current_operation,
        access: EnvironmentAccessEligibilitySummary {
            state: if eligible {
                EnvironmentAccessEligibilityState::Eligible
            } else {
                EnvironmentAccessEligibilityState::Ineligible
            },
            reason_code: (!eligible)
                .then(|| DiagnosticCode::registered("LW_ENVIRONMENT_ENDPOINT_UNAVAILABLE")),
            healthy_endpoint_count,
            active_grant_count: 0,
        },
    };
    summary
        .validate()
        .map_err(|_| EnvironmentApiError::ResponseInvalid)?;
    Ok(summary)
}

async fn create_environment(
    State(state): State<EnvironmentApiState>,
    Extension(context): Extension<telemetry::RequestContext>,
    caller: Option<Extension<auth::ServiceIdentity>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<EnvironmentOperationAccepted>), EnvironmentApiError> {
    require_access_bff(caller)?;
    let actor_id = actor(&headers)?;
    require_session(&headers)?;
    let key = idempotency_key(&headers)?;
    let request = contracts::parse_strict_json::<CreateEnvironmentRequest>(&body)
        .map_err(|_| EnvironmentApiError::RequestInvalid)?;
    request
        .validate()
        .map_err(|_| EnvironmentApiError::RequestInvalid)?;
    let release = state
        .releases
        .resolve(request.release_id, request.release_version)
        .await?;
    if release.withdrawn_at.is_some()
        || release.projection.release.project_id != request.project_id
        || release.projection.release.course_id != request.course_id
        || release.projection.environment_spec.class != EnvironmentClass::Experiment
    {
        return Err(EnvironmentApiError::ReleaseDenied);
    }
    let provider_binding = match &release.projection.environment_spec.runtime {
        EnvironmentRuntimeSpec::Container {
            provider_binding, ..
        }
        | EnvironmentRuntimeSpec::VirtualMachine {
            provider_binding, ..
        } => provider_binding.clone(),
    };
    let accepted_at = state.store.current_time().await?;
    let deadline_at = add_duration(accepted_at, OPERATION_DEADLINE)?;
    let environment_id = EnvironmentId::new();
    let command = EnvironmentLifecycleCommand {
        environment_id,
        kind: EnvironmentOperationKind::Create,
        expected_revision: Revision::new(1).map_err(|_| EnvironmentApiError::RequestInvalid)?,
        actor_id,
        trace_id: context.trace_id().to_owned(),
        accepted_at,
        deadline_at,
        access_revocation_revision: None,
        preserve_mutable_disk: false,
        max_attempts: 3,
        reset_target: None,
    };
    let create = EnvironmentCreateSpec {
        project_id: request.project_id,
        course_id: request.course_id,
        owner_actor_id: actor_id,
        display_label: request
            .display_label
            .clone()
            .unwrap_or_else(|| release.projection.environment_spec.name.clone()),
        class: release.projection.environment_spec.class,
        runtime_kind: release.projection.release.runtime_kind,
        release_id: request.release_id,
        release_version: request.release_version,
        provider_binding,
        lease_id: None,
        capacity_binding: None,
        eligibility_expires_at: release.projection.environment_spec.retention.retain_until,
    };
    let accepted = state
        .store
        .accept_api_command(
            key.as_str(),
            &command,
            Some(&create),
            None,
            request.project_id,
            request.course_id,
        )
        .await?;
    Ok((StatusCode::ACCEPTED, Json(accepted)))
}

async fn get_environment(
    State(state): State<EnvironmentApiState>,
    caller: Option<Extension<auth::ServiceIdentity>>,
    Path(environment_id): Path<EnvironmentId>,
    headers: HeaderMap,
) -> Result<Response, EnvironmentApiError> {
    require_access_bff(caller)?;
    let instance = load_owned(&state, environment_id, actor(&headers)?).await?;
    instance_response(instance)
}

async fn get_environment_operation(
    State(state): State<EnvironmentApiState>,
    caller: Option<Extension<auth::ServiceIdentity>>,
    Path((environment_id, operation_id)): Path<(EnvironmentId, contracts::OperationId)>,
    headers: HeaderMap,
) -> Result<Json<contracts::environment::EnvironmentOperationSnapshot>, EnvironmentApiError> {
    require_access_bff(caller)?;
    require_session(&headers)?;
    let actor_id = actor(&headers)?;
    load_owned(&state, environment_id, actor_id).await?;
    let operation = state
        .store
        .get_operation(environment_id, actor_id, operation_id)
        .await?;
    Ok(Json(operation.snapshot))
}

async fn list_environment_operations(
    State(state): State<EnvironmentApiState>,
    caller: Option<Extension<auth::ServiceIdentity>>,
    Path(environment_id): Path<EnvironmentId>,
    Query(query): Query<EnvironmentOperationListQuery>,
    headers: HeaderMap,
) -> Result<
    Json<SnapshotPage<contracts::environment::EnvironmentOperationSnapshot>>,
    EnvironmentApiError,
> {
    require_access_bff(caller)?;
    require_session(&headers)?;
    query
        .validate()
        .map_err(|_| EnvironmentApiError::RequestInvalid)?;
    let actor_id = actor(&headers)?;
    load_owned(&state, environment_id, actor_id).await?;
    let page = state
        .store
        .list_operations(
            environment_id,
            actor_id,
            query.kind,
            query.state,
            query.cursor.as_deref(),
            query.limit.unwrap_or(DEFAULT_PAGE_LIMIT),
        )
        .await?;
    let items = page
        .records
        .into_iter()
        .map(|record| record.snapshot)
        .collect();
    Ok(Json(SnapshotPage {
        items,
        next_cursor: page.next_cursor,
        snapshot_sequence: page.snapshot_sequence,
        snapshot_at: page.snapshot_at,
    }))
}

async fn list_endpoints(
    State(state): State<EnvironmentApiState>,
    caller: Option<Extension<auth::ServiceIdentity>>,
    Path(environment_id): Path<EnvironmentId>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, EnvironmentApiError> {
    require_access_bff(caller)?;
    let instance = load_owned(&state, environment_id, actor(&headers)?).await?;
    Ok(Json(serde_json::json!({"items": instance.endpoints})))
}

macro_rules! lifecycle_handler {
    ($name:ident, $kind:expr, $reason:expr, $preserve:expr) => {
        async fn $name(
            State(state): State<EnvironmentApiState>,
            Extension(context): Extension<telemetry::RequestContext>,
            caller: Option<Extension<auth::ServiceIdentity>>,
            Path(environment_id): Path<EnvironmentId>,
            headers: HeaderMap,
        ) -> Result<(StatusCode, Json<EnvironmentOperationAccepted>), EnvironmentApiError> {
            require_access_bff(caller)?;
            accept_lifecycle(
                &state,
                environment_id,
                &headers,
                $kind,
                $reason,
                $preserve,
                context.trace_id(),
                None,
            )
            .await
        }
    };
}

lifecycle_handler!(
    start_environment,
    EnvironmentOperationKind::Start,
    None,
    true
);
lifecycle_handler!(
    stop_environment,
    EnvironmentOperationKind::Stop,
    Some("environment_stopped"),
    true
);
lifecycle_handler!(
    restart_environment,
    EnvironmentOperationKind::Restart,
    Some("environment_restarted"),
    true
);
lifecycle_handler!(
    retry_environment,
    EnvironmentOperationKind::Retry,
    None,
    true
);
lifecycle_handler!(
    delete_environment,
    EnvironmentOperationKind::Delete,
    Some("environment_deleted"),
    false
);

async fn reset_environment(
    State(state): State<EnvironmentApiState>,
    Extension(context): Extension<telemetry::RequestContext>,
    caller: Option<Extension<auth::ServiceIdentity>>,
    Path(environment_id): Path<EnvironmentId>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<EnvironmentOperationAccepted>), EnvironmentApiError> {
    require_access_bff(caller)?;
    let request = contracts::parse_strict_json::<ResetEnvironmentRequest>(&body)
        .map_err(|_| EnvironmentApiError::RequestInvalid)?;
    request
        .validate()
        .map_err(|_| EnvironmentApiError::RequestInvalid)?;
    accept_lifecycle(
        &state,
        environment_id,
        &headers,
        EnvironmentOperationKind::Reset,
        Some("environment_reset"),
        false,
        context.trace_id(),
        Some(request.reset_target),
    )
    .await
}

async fn cancel_environment(
    State(state): State<EnvironmentApiState>,
    Extension(context): Extension<telemetry::RequestContext>,
    caller: Option<Extension<auth::ServiceIdentity>>,
    Path(environment_id): Path<EnvironmentId>,
    headers: HeaderMap,
) -> Result<(StatusCode, Json<EnvironmentOperationAccepted>), EnvironmentApiError> {
    require_access_bff(caller)?;
    accept_lifecycle(
        &state,
        environment_id,
        &headers,
        EnvironmentOperationKind::Cancel,
        Some("environment_cancelled"),
        false,
        context.trace_id(),
        None,
    )
    .await
}

async fn recover_environment(
    State(state): State<EnvironmentApiState>,
    Extension(context): Extension<telemetry::RequestContext>,
    caller: Option<Extension<auth::ServiceIdentity>>,
    Path(environment_id): Path<EnvironmentId>,
    headers: HeaderMap,
) -> Result<(StatusCode, Json<EnvironmentOperationAccepted>), EnvironmentApiError> {
    require_access_bff(caller)?;
    accept_lifecycle(
        &state,
        environment_id,
        &headers,
        EnvironmentOperationKind::Recover,
        None,
        true,
        context.trace_id(),
        None,
    )
    .await
}

#[allow(
    clippy::too_many_arguments,
    reason = "the HTTP boundary passes each lifecycle precondition and immutable command field explicitly"
)]
async fn accept_lifecycle(
    state: &EnvironmentApiState,
    environment_id: EnvironmentId,
    headers: &HeaderMap,
    kind: EnvironmentOperationKind,
    revocation_reason: Option<&'static str>,
    preserve_mutable_disk: bool,
    trace_id: &str,
    reset_target: Option<EnvironmentResetTarget>,
) -> Result<(StatusCode, Json<EnvironmentOperationAccepted>), EnvironmentApiError> {
    require_session(headers)?;
    let instance = load_owned(state, environment_id, actor(headers)?).await?;
    let expected_revision = if_match(headers)?;
    if expected_revision != instance.revision {
        return Err(EnvironmentApiError::RevisionConflict);
    }
    let access_revocation_revision = if let Some(reason) = revocation_reason {
        Some(state.access_revoker.revoke(&instance, reason).await?)
    } else if matches!(
        kind,
        EnvironmentOperationKind::Retry | EnvironmentOperationKind::Recover
    ) {
        instance.operation.access_revocation_revision
    } else {
        None
    };
    let accepted_at = state.store.current_time().await?;
    let command = EnvironmentLifecycleCommand {
        environment_id,
        kind,
        expected_revision,
        actor_id: instance.owner_id,
        trace_id: trace_id.to_owned(),
        accepted_at,
        deadline_at: add_duration(accepted_at, OPERATION_DEADLINE)?,
        access_revocation_revision,
        preserve_mutable_disk,
        max_attempts: 3,
        reset_target,
    };
    let accepted = state
        .store
        .accept_api_command(
            idempotency_key(headers)?.as_str(),
            &command,
            None,
            None,
            instance.project_id,
            instance.course_id,
        )
        .await?;
    Ok((StatusCode::ACCEPTED, Json(accepted)))
}

async fn load_owned(
    state: &EnvironmentApiState,
    environment_id: EnvironmentId,
    actor_id: ActorId,
) -> Result<EnvironmentInstance, EnvironmentApiError> {
    let instance = state.store.load(environment_id).await?;
    if instance.owner_id != actor_id {
        return Err(EnvironmentApiError::ScopeDenied);
    }
    Ok(instance)
}

fn require_access_bff(
    caller: Option<Extension<auth::ServiceIdentity>>,
) -> Result<(), EnvironmentApiError> {
    require_permission(caller, ACCESS_PERMISSION)
}

fn require_permission(
    caller: Option<Extension<auth::ServiceIdentity>>,
    permission: &str,
) -> Result<(), EnvironmentApiError> {
    caller
        .is_some_and(|Extension(identity)| identity.allows(permission))
        .then_some(())
        .ok_or(EnvironmentApiError::CallerDenied)
}

fn actor(headers: &HeaderMap) -> Result<ActorId, EnvironmentApiError> {
    headers
        .get(ACTOR_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| ActorId::from_str(value).ok())
        .ok_or(EnvironmentApiError::IdentityInvalid)
}

fn require_session(headers: &HeaderMap) -> Result<(), EnvironmentApiError> {
    headers
        .get(SESSION_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| Uuid::parse_str(value).ok())
        .map(|_| ())
        .ok_or(EnvironmentApiError::IdentityInvalid)
}

fn idempotency_key(headers: &HeaderMap) -> Result<IdempotencyKey, EnvironmentApiError> {
    headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .ok_or(EnvironmentApiError::IdempotencyRequired)
        .and_then(|value| {
            IdempotencyKey::parse(value).map_err(|_| EnvironmentApiError::IdempotencyInvalid)
        })
}

fn if_match(headers: &HeaderMap) -> Result<Revision, EnvironmentApiError> {
    headers
        .get(header::IF_MATCH)
        .and_then(|value| value.to_str().ok())
        .ok_or(EnvironmentApiError::RevisionRequired)
        .and_then(|value| {
            StrongEtag::parse(value)
                .map(|etag| etag.revision())
                .map_err(|_| EnvironmentApiError::RevisionConflict)
        })
}

fn add_duration(
    value: UtcTimestamp,
    duration: Duration,
) -> Result<UtcTimestamp, EnvironmentApiError> {
    let seconds =
        i64::try_from(duration.as_secs()).map_err(|_| EnvironmentApiError::ClockInvalid)?;
    UtcTimestamp::from_utc(value.get() + time::Duration::seconds(seconds))
        .map_err(|_| EnvironmentApiError::ClockInvalid)
}

fn instance_response(instance: EnvironmentInstance) -> Result<Response, EnvironmentApiError> {
    let etag = StrongEtag::from_revision(instance.revision).header_value();
    let mut response = Json(instance).into_response();
    response.headers_mut().insert(
        header::ETAG,
        HeaderValue::from_str(&etag).map_err(|_| EnvironmentApiError::ResponseInvalid)?,
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

/// Stable public API failures with no provider or credential detail.
#[derive(Debug, thiserror::Error)]
pub enum EnvironmentApiError {
    #[error("LW_ENVIRONMENT_GATEWAY_DENIED")]
    CallerDenied,
    #[error("LW_AUTH_SESSION_REJECTED")]
    IdentityInvalid,
    #[error("LW_AUTH_SCOPE_DENIED")]
    ScopeDenied,
    #[error("LW_CONTRACT_DOCUMENT_INVALID")]
    RequestInvalid,
    #[error("LW_IDEMPOTENCY_REQUIRED")]
    IdempotencyRequired,
    #[error("LW_IDEMPOTENCY_INVALID")]
    IdempotencyInvalid,
    #[error("LW_REVISION_REQUIRED")]
    RevisionRequired,
    #[error("LW_ENVIRONMENT_REVISION_CONFLICT")]
    RevisionConflict,
    #[error("LW_ENVIRONMENT_RELEASE_DENIED")]
    ReleaseDenied,
    #[error("LW_ENVIRONMENT_RESOURCE_LEASE_FENCE_INVALID")]
    LeaseFenceInvalid,
    #[error("LW_ENVIRONMENT_CLOCK_INVALID")]
    ClockInvalid,
    #[error("LW_ENVIRONMENT_RESPONSE_INVALID")]
    ResponseInvalid,
    #[error("LW_ENVIRONMENT_WORK_EXECUTION_UNAVAILABLE")]
    WorkExecutionUnavailable,
    #[error(transparent)]
    ServiceAuth(#[from] auth::ServiceAuthError),
    #[error(transparent)]
    Store(#[from] EnvironmentStoreError),
    #[error(transparent)]
    Release(#[from] ReleaseProjectionError),
    #[error(transparent)]
    Messaging(#[from] NatsMessagingError),
    #[error(transparent)]
    FreezeBinding(#[from] FreezeBindingError),
    #[error(transparent)]
    WorkExecution(#[from] WorkExecutionError),
}

impl IntoResponse for EnvironmentApiError {
    fn into_response(self) -> Response {
        let status = match self {
            Self::CallerDenied
            | Self::ScopeDenied
            | Self::ServiceAuth(auth::ServiceAuthError::PermissionDenied) => StatusCode::FORBIDDEN,
            Self::IdentityInvalid
            | Self::ServiceAuth(
                auth::ServiceAuthError::CredentialsMissing
                | auth::ServiceAuthError::TokenRejected
                | auth::ServiceAuthError::TokenExpired,
            ) => StatusCode::UNAUTHORIZED,
            Self::RequestInvalid
            | Self::IdempotencyRequired
            | Self::IdempotencyInvalid
            | Self::FreezeBinding(FreezeBindingError::ExecutionBindingInvalid)
            | Self::WorkExecution(
                WorkExecutionError::RequestInvalid
                | WorkExecutionError::ReceiptInvalid
                | WorkExecutionError::ObservationInvalid,
            )
            | Self::Store(
                EnvironmentStoreError::InvalidInventoryCursor
                | EnvironmentStoreError::InvalidOperationCursor
                | EnvironmentStoreError::InvalidLimit,
            ) => StatusCode::BAD_REQUEST,
            Self::RevisionRequired => StatusCode::PRECONDITION_REQUIRED,
            Self::RevisionConflict
            | Self::LeaseFenceInvalid
            | Self::WorkExecution(
                WorkExecutionError::IdentityMismatch | WorkExecutionError::AdmissionMismatch,
            ) => StatusCode::PRECONDITION_FAILED,
            Self::ReleaseDenied => StatusCode::UNPROCESSABLE_ENTITY,
            Self::Store(
                EnvironmentStoreError::EnvironmentNotFound
                | EnvironmentStoreError::OperationNotFound,
            )
            | Self::Release(ReleaseProjectionError::NotFound)
            | Self::WorkExecution(WorkExecutionError::NotFound) => StatusCode::NOT_FOUND,
            Self::WorkExecution(WorkExecutionError::EnvironmentNotEligible)
            | Self::FreezeBinding(FreezeBindingError::EnvironmentNotEligible) => {
                StatusCode::UNPROCESSABLE_ENTITY
            }
            Self::Store(
                EnvironmentStoreError::IdempotencyConflict
                | EnvironmentStoreError::IdempotencyInProgress,
            ) => StatusCode::CONFLICT,
            _ => StatusCode::SERVICE_UNAVAILABLE,
        };
        let rendered = self.to_string();
        let diagnostic = rendered
            .split(':')
            .next()
            .unwrap_or("LW_ENVIRONMENT_REQUEST_FAILED");
        let retryable = status == StatusCode::SERVICE_UNAVAILABLE;
        tracing::warn!(
            event = "environment.api.rejected",
            component = "api-error-boundary",
            operation = "http.request",
            outcome = "rejected",
            duration_ms = 0_u64,
            diagnostic_code = diagnostic,
            error_kind = "request_rejected",
            failure_stage = "environment.request.finalize",
            retryable,
            safe_detail = "request_rejected",
            http_status = status.as_u16(),
        );
        (
            status,
            Json(serde_json::json!({"diagnosticCode": diagnostic})),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::add_duration;
    use contracts::UtcTimestamp;
    use std::time::Duration;

    #[test]
    fn command_identity_is_bounded_and_deadline_uses_contract_time()
    -> Result<(), Box<dyn std::error::Error>> {
        let now: UtcTimestamp = "2026-07-19T00:00:00.000Z".parse()?;
        let deadline = add_duration(now, Duration::from_mins(15))?;
        assert_eq!(deadline.to_string(), "2026-07-19T00:15:00.000Z");
        Ok(())
    }
}
