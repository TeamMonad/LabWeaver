//! Current object action views through the verified Control API boundary.
use super::{ApiError, ApiState, GatewayPrincipal, authorize_project};
use axum::{
    Extension, Json,
    extract::{Path, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
};
use contracts::ProjectId;
use std::sync::Arc;

pub(super) async fn available_actions(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(project_id): Path<ProjectId>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    // Authorization runs first: unknown or inaccessible project IDs both return 403 to nonmembers.
    let decision = authorize_project(
        &state,
        &principal,
        &headers,
        "getProjectAvailableActions",
        project_id,
    )
    .await?;
    state.control.project(project_id).await?;
    let members =
        auth::load_membership_snapshot(&state.control.pool, decision.actor.actor_id.as_uuid())
            .await
            .map_err(|_| ApiError::internal("LW_AUTH_MEMBERSHIP_UNAVAILABLE"))?;
    let context = auth::AuthorizationContext {
        actor: decision.actor.clone(),
        course_memberships: members.course_memberships,
        project_memberships: members.project_memberships,
        now: time::OffsetDateTime::now_utc(),
    };
    let current = auth::object_authorization::authorize_project_action(
        &context,
        project_id,
        contracts::object_authorization::ObjectAction::Browse,
    )
    .map_err(|_| ApiError::forbidden("LW_AUTH_SCOPE_DENIED"))?;
    let mut actions = Vec::new();
    for action in [
        contracts::object_authorization::ObjectAction::Browse,
        contracts::object_authorization::ObjectAction::Edit,
        contracts::object_authorization::ObjectAction::Start,
        contracts::object_authorization::ObjectAction::Stop,
        contracts::object_authorization::ObjectAction::ConfigureAgent,
        contracts::object_authorization::ObjectAction::GrantAccess,
        contracts::object_authorization::ObjectAction::ManageMembers,
        contracts::object_authorization::ObjectAction::ViewCosts,
        contracts::object_authorization::ObjectAction::RequestRuntimePermissions,
        contracts::object_authorization::ObjectAction::ManageRuntimePolicy,
        contracts::object_authorization::ObjectAction::Archive,
    ] {
        if auth::object_authorization::authorize_project_action(&context, project_id, action)
            .is_ok()
        {
            actions.push(action);
        }
    }
    Ok(
        Json(contracts::object_authorization::AvailableObjectActions {
            project_id,
            actor_id: decision.actor.actor_id,
            membership_revision: current.authorization_revision,
            actions,
            valid_until: std::cmp::min(current.valid_until, decision.valid_until),
        })
        .into_response(),
    )
}
