//! Access-BFF authenticated public Environment lifecycle API.

use std::{str::FromStr, time::Duration};

use axum::{
    Json, Router,
    body::Bytes,
    extract::{Extension, Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use contracts::{
    ActorId, DiagnosticCode, EnvironmentId, Revision, StreamSequence, UtcTimestamp,
    authoring::{EnvironmentClass, EnvironmentRuntimeSpec},
    environment::{
        EndpointHealth, EnvironmentAccessEligibilityState, EnvironmentAccessEligibilitySummary,
        EnvironmentCreateSpec, EnvironmentInstance, EnvironmentLifecycleCommand,
        EnvironmentOperationKind, EnvironmentOwnerRelation, EnvironmentOwnerSummary,
        EnvironmentSummary,
    },
    http::{
        CreateEnvironmentRequest, DEFAULT_PAGE_LIMIT, EnvironmentInventoryQuery,
        EnvironmentOperationAccepted, IdempotencyKey, SnapshotPage, StrongEtag,
    },
    submission::{EnvironmentFreezeBinding, EnvironmentFreezeBindingRequest},
};
use uuid::Uuid;

use crate::{
    ContainerReleaseResolver, EnvironmentStoreError, FreezeBindingError, FreezeBindingService,
    NatsAccessRevoker, NatsMessagingError, PgEnvironmentStore, PgReleaseProjectionStore,
    ReleaseProjectionError, VerifiedCallerIdentity,
};

const ACCESS_SERVICE_SAN: &str = "spiffe://labweaver/access-service";
const EVALUATION_SERVICE_SAN: &str = "spiffe://labweaver/evaluation-service";
const ACTOR_HEADER: &str = "x-labweaver-actor-id";
const SESSION_HEADER: &str = "x-labweaver-session-id";
const OPERATION_DEADLINE: Duration = Duration::from_secs(15 * 60);

/// Environment-owned dependencies used by the public API.
#[derive(Clone)]
pub struct EnvironmentApiState {
    store: PgEnvironmentStore,
    releases: PgReleaseProjectionStore,
    access_revoker: NatsAccessRevoker,
    freeze_bindings: FreezeBindingService,
}

impl EnvironmentApiState {
    #[must_use]
    pub const fn new(
        store: PgEnvironmentStore,
        releases: PgReleaseProjectionStore,
        access_revoker: NatsAccessRevoker,
        freeze_bindings: FreezeBindingService,
    ) -> Self {
        Self {
            store,
            releases,
            access_revoker,
            freeze_bindings,
        }
    }
}

/// Builds the public routes served only behind the existing mTLS acceptor.
pub fn environment_api_router(state: EnvironmentApiState) -> Router {
    Router::new()
        .route(
            "/api/v1/environments",
            get(list_environments).post(create_environment),
        )
        .route(
            "/api/v1/environments/{environment_id}",
            get(get_environment),
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
        .with_state(state)
}

async fn resolve_freeze_binding(
    State(state): State<EnvironmentApiState>,
    caller: Option<Extension<VerifiedCallerIdentity>>,
    Path(environment_id): Path<EnvironmentId>,
    body: Bytes,
) -> Result<Json<EnvironmentFreezeBinding>, EnvironmentApiError> {
    if !caller.is_some_and(|Extension(identity)| identity.contains_san(EVALUATION_SERVICE_SAN)) {
        return Err(EnvironmentApiError::CallerDenied);
    }
    let request = contracts::parse_strict_json::<EnvironmentFreezeBindingRequest>(&body)
        .map_err(|_| EnvironmentApiError::RequestInvalid)?;
    Ok(Json(
        state
            .freeze_bindings
            .resolve(environment_id, &request)
            .await?,
    ))
}

async fn list_environments(
    State(state): State<EnvironmentApiState>,
    caller: Option<Extension<VerifiedCallerIdentity>>,
    Query(query): Query<EnvironmentInventoryQuery>,
    headers: HeaderMap,
) -> Result<Json<SnapshotPage<EnvironmentSummary>>, EnvironmentApiError> {
    require_access_bff(caller)?;
    require_session(&headers)?;
    query
        .validate()
        .map_err(|_| EnvironmentApiError::RequestInvalid)?;
    if query.project_id.is_some()
        || query.runtime_kind.is_some()
        || query.class.is_some()
        || query.desired_state.is_some()
        || query.observed_state.is_some()
        || query.release_id.is_some()
        || query.cursor.is_some()
    {
        return Err(EnvironmentApiError::InventoryFilterUnsupported);
    }
    let (records, snapshot_at) = state
        .store
        .list_owned(
            query.course_id,
            actor(&headers)?,
            query.limit.unwrap_or(DEFAULT_PAGE_LIMIT),
        )
        .await?;
    let snapshot_sequence = records
        .iter()
        .map(|record| record.stream_sequence)
        .max_by_key(|sequence| sequence.0)
        .unwrap_or(StreamSequence(1));
    let items = records
        .into_iter()
        .map(|record| environment_summary(record, snapshot_at))
        .collect::<Result<Vec<_>, EnvironmentApiError>>()?;
    Ok(Json(SnapshotPage {
        items,
        next_cursor: None,
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
        course_id: record.instance.course_id,
        project_id: None,
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
        current_operation: None,
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
    caller: Option<Extension<VerifiedCallerIdentity>>,
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
        trace_id: trace_id(),
        accepted_at,
        deadline_at,
        access_revocation_revision: None,
        preserve_mutable_disk: false,
        max_attempts: 3,
        reset_target: None,
    };
    let create = EnvironmentCreateSpec {
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
        .accept_api_command(key.as_str(), &command, Some(&create), request.course_id)
        .await?;
    Ok((StatusCode::ACCEPTED, Json(accepted)))
}

async fn get_environment(
    State(state): State<EnvironmentApiState>,
    caller: Option<Extension<VerifiedCallerIdentity>>,
    Path(environment_id): Path<EnvironmentId>,
    headers: HeaderMap,
) -> Result<Response, EnvironmentApiError> {
    require_access_bff(caller)?;
    let instance = load_owned(&state, environment_id, actor(&headers)?).await?;
    instance_response(instance)
}

async fn list_endpoints(
    State(state): State<EnvironmentApiState>,
    caller: Option<Extension<VerifiedCallerIdentity>>,
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
            caller: Option<Extension<VerifiedCallerIdentity>>,
            Path(environment_id): Path<EnvironmentId>,
            headers: HeaderMap,
        ) -> Result<(StatusCode, Json<EnvironmentOperationAccepted>), EnvironmentApiError> {
            accept_lifecycle(
                &state,
                caller,
                environment_id,
                &headers,
                $kind,
                $reason,
                $preserve,
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

async fn accept_lifecycle(
    state: &EnvironmentApiState,
    caller: Option<Extension<VerifiedCallerIdentity>>,
    environment_id: EnvironmentId,
    headers: &HeaderMap,
    kind: EnvironmentOperationKind,
    revocation_reason: Option<&'static str>,
    preserve_mutable_disk: bool,
) -> Result<(StatusCode, Json<EnvironmentOperationAccepted>), EnvironmentApiError> {
    require_access_bff(caller)?;
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
        trace_id: trace_id(),
        accepted_at,
        deadline_at: add_duration(accepted_at, OPERATION_DEADLINE)?,
        access_revocation_revision,
        preserve_mutable_disk,
        max_attempts: 3,
        reset_target: None,
    };
    let accepted = state
        .store
        .accept_api_command(
            idempotency_key(headers)?.as_str(),
            &command,
            None,
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
    caller: Option<Extension<VerifiedCallerIdentity>>,
) -> Result<(), EnvironmentApiError> {
    if caller.is_some_and(|Extension(identity)| identity.contains_san(ACCESS_SERVICE_SAN)) {
        Ok(())
    } else {
        Err(EnvironmentApiError::CallerDenied)
    }
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

fn trace_id() -> String {
    format!("environment-api-{}", Uuid::now_v7())
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
    #[error("LW_ENVIRONMENT_CLOCK_INVALID")]
    ClockInvalid,
    #[error("LW_ENVIRONMENT_RESPONSE_INVALID")]
    ResponseInvalid,
    #[error("LW_ENVIRONMENT_INVENTORY_FILTER_UNSUPPORTED")]
    InventoryFilterUnsupported,
    #[error(transparent)]
    Store(#[from] EnvironmentStoreError),
    #[error(transparent)]
    Release(#[from] ReleaseProjectionError),
    #[error(transparent)]
    Messaging(#[from] NatsMessagingError),
    #[error(transparent)]
    FreezeBinding(#[from] FreezeBindingError),
}

impl IntoResponse for EnvironmentApiError {
    fn into_response(self) -> Response {
        let status = match self {
            Self::CallerDenied | Self::ScopeDenied => StatusCode::FORBIDDEN,
            Self::IdentityInvalid => StatusCode::UNAUTHORIZED,
            Self::RequestInvalid
            | Self::IdempotencyRequired
            | Self::IdempotencyInvalid
            | Self::InventoryFilterUnsupported => StatusCode::BAD_REQUEST,
            Self::RevisionRequired => StatusCode::PRECONDITION_REQUIRED,
            Self::RevisionConflict => StatusCode::PRECONDITION_FAILED,
            Self::ReleaseDenied => StatusCode::UNPROCESSABLE_ENTITY,
            Self::Store(EnvironmentStoreError::EnvironmentNotFound)
            | Self::Release(ReleaseProjectionError::NotFound) => StatusCode::NOT_FOUND,
            Self::FreezeBinding(FreezeBindingError::EnvironmentNotEligible) => {
                StatusCode::UNPROCESSABLE_ENTITY
            }
            Self::Store(
                EnvironmentStoreError::IdempotencyConflict
                | EnvironmentStoreError::IdempotencyInProgress,
            ) => StatusCode::CONFLICT,
            _ => StatusCode::SERVICE_UNAVAILABLE,
        };
        let diagnostic = self.to_string();
        tracing::warn!(event = "environment.api.rejected", %diagnostic, status = status.as_u16());
        (
            status,
            Json(serde_json::json!({"diagnosticCode": diagnostic})),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::{add_duration, trace_id};
    use contracts::UtcTimestamp;
    use std::time::Duration;

    #[test]
    fn command_identity_is_bounded_and_deadline_uses_contract_time()
    -> Result<(), Box<dyn std::error::Error>> {
        let now: UtcTimestamp = "2026-07-19T00:00:00.000Z".parse()?;
        let deadline = add_duration(now, Duration::from_secs(900))?;
        assert_eq!(deadline.to_string(), "2026-07-19T00:15:00.000Z");
        assert!(trace_id().starts_with("environment-api-"));
        Ok(())
    }
}
