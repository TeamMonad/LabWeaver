//! Authenticated roster and recipient invitation HTTP boundaries.
use super::{
    ApiError, ApiState, GatewayPrincipal, authorize_decision, authorize_global, authorize_project,
    etag, idempotency, with_etag,
};
use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use contracts::http::RemoveProjectMembershipRequest;
use contracts::membership_governance::ControlListQuery;
use contracts::membership_governance::{
    AcceptMembershipInvitation, MembershipInvitationScope, ResolvedMembershipInvitation,
};
use contracts::{ActorId, CourseId, MembershipInvitationId, ProjectId};
use std::sync::Arc;

pub(super) async fn list_project_invitations(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(project_id): Path<ProjectId>,
    headers: HeaderMap,
    Query(query): Query<ControlListQuery>,
) -> Result<Response, ApiError> {
    let decision = authorize_project(
        &state,
        &principal,
        &headers,
        "listProjectMembershipInvitations",
        project_id,
    )
    .await?;
    Ok(Json(
        state
            .control
            .scoped_membership_invitations(
                &decision.actor,
                &MembershipInvitationScope::Project {
                    project_id,
                    object_role: contracts::object_authorization::ProjectObjectRole::Viewer,
                },
                &query,
            )
            .await?,
    )
    .into_response())
}

pub(super) async fn list_course_invitations(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(course_id): Path<CourseId>,
    headers: HeaderMap,
    Query(query): Query<ControlListQuery>,
) -> Result<Response, ApiError> {
    let decision = authorize_decision(
        &state,
        &principal,
        &headers,
        "listCourseMembershipInvitations",
        course_id,
    )
    .await?;
    Ok(Json(
        state
            .control
            .scoped_membership_invitations(
                &decision.actor,
                &MembershipInvitationScope::Course {
                    course_id,
                    object_role: contracts::membership_governance::CourseObjectRole::Participant,
                },
                &query,
            )
            .await?,
    )
    .into_response())
}

pub(super) async fn create_project(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(project_id): Path<ProjectId>,
    headers: HeaderMap,
    Json(request): Json<ResolvedMembershipInvitation>,
) -> Result<Response, ApiError> {
    if !matches!(request.scope, MembershipInvitationScope::Project {project_id:id,..} if id==project_id)
    {
        return Err(ApiError::forbidden("LW_MEMBERSHIP_INVITATION_DENIED"));
    }
    let decision = authorize_project(
        &state,
        &principal,
        &headers,
        "createProjectMembershipInvitation",
        project_id,
    )
    .await?;
    let invitation = state
        .control
        .create_membership_invitation(&decision.actor, &request, &idempotency(&headers)?)
        .await?;
    Ok(with_etag(
        StatusCode::CREATED,
        &invitation,
        invitation.revision,
    ))
}

pub(super) async fn create_course(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(course_id): Path<CourseId>,
    headers: HeaderMap,
    Json(request): Json<ResolvedMembershipInvitation>,
) -> Result<Response, ApiError> {
    if !matches!(request.scope, MembershipInvitationScope::Course {course_id:id,..} if id==course_id)
    {
        return Err(ApiError::forbidden("LW_MEMBERSHIP_INVITATION_DENIED"));
    }
    let decision = authorize_decision(
        &state,
        &principal,
        &headers,
        "createCourseMembershipInvitation",
        course_id,
    )
    .await?;
    let invitation = state
        .control
        .create_membership_invitation(&decision.actor, &request, &idempotency(&headers)?)
        .await?;
    Ok(with_etag(
        StatusCode::CREATED,
        &invitation,
        invitation.revision,
    ))
}

pub(super) async fn incoming(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    headers: HeaderMap,
    Query(query): Query<ControlListQuery>,
) -> Result<Response, ApiError> {
    let decision = authorize_global(
        &state,
        &principal,
        &headers,
        "listIncomingMembershipInvitations",
    )
    .await?;
    Ok(Json(
        state
            .control
            .incoming_membership_invitations(&decision.actor, &query)
            .await?,
    )
    .into_response())
}

pub(super) async fn get_invitation(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(id): Path<MembershipInvitationId>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let decision =
        authorize_global(&state, &principal, &headers, "getMembershipInvitation").await?;
    let invitation = state
        .control
        .membership_invitation(&decision.actor, id)
        .await?;
    Ok(with_etag(StatusCode::OK, &invitation, invitation.revision))
}

pub(super) async fn accept(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(id): Path<MembershipInvitationId>,
    headers: HeaderMap,
    Json(request): Json<AcceptMembershipInvitation>,
) -> Result<Response, ApiError> {
    let decision =
        authorize_global(&state, &principal, &headers, "acceptMembershipInvitation").await?;
    if etag(&headers)? != request.expected_revision {
        return Err(ApiError::new(
            StatusCode::PRECONDITION_FAILED,
            "LW_MEMBERSHIP_INVITATION_PRECONDITION_FAILED",
            false,
        ));
    }
    let invitation = state
        .control
        .accept_membership_invitation(&decision.actor, id, &request, &idempotency(&headers)?)
        .await?;
    Ok(with_etag(StatusCode::OK, &invitation, invitation.revision))
}

pub(super) async fn revoke(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(id): Path<MembershipInvitationId>,
    headers: HeaderMap,
    Json(request): Json<RemoveProjectMembershipRequest>,
) -> Result<Response, ApiError> {
    let decision =
        authorize_global(&state, &principal, &headers, "revokeMembershipInvitation").await?;
    if etag(&headers)? != request.expected_revision {
        return Err(ApiError::new(
            StatusCode::PRECONDITION_FAILED,
            "LW_MEMBERSHIP_INVITATION_PRECONDITION_FAILED",
            false,
        ));
    }
    let invitation = state
        .control
        .revoke_membership_invitation(&decision.actor, id, &request, &idempotency(&headers)?)
        .await?;
    Ok(with_etag(StatusCode::OK, &invitation, invitation.revision))
}

pub(super) async fn list_course_members(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path(course_id): Path<CourseId>,
    headers: HeaderMap,
    Query(query): Query<ControlListQuery>,
) -> Result<Response, ApiError> {
    let decision = authorize_decision(
        &state,
        &principal,
        &headers,
        "listCourseMemberships",
        course_id,
    )
    .await?;
    Ok(Json(
        state
            .control
            .course_memberships(&decision.actor, course_id, &query)
            .await?,
    )
    .into_response())
}

pub(super) async fn remove_course_member(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    Path((course_id, target)): Path<(CourseId, ActorId)>,
    headers: HeaderMap,
    Json(request): Json<RemoveProjectMembershipRequest>,
) -> Result<Response, ApiError> {
    let decision = authorize_decision(
        &state,
        &principal,
        &headers,
        "removeCourseMembership",
        course_id,
    )
    .await?;
    if etag(&headers)? != request.expected_revision {
        return Err(ApiError::new(
            StatusCode::PRECONDITION_FAILED,
            "LW_MEMBERSHIP_INVITATION_PRECONDITION_FAILED",
            false,
        ));
    }
    let member = state
        .control
        .remove_course_membership(
            &decision.actor,
            course_id,
            target,
            &request,
            &idempotency(&headers)?,
        )
        .await?;
    Ok(with_etag(StatusCode::OK, &member, member.revision))
}

pub(super) fn routes() -> axum::Router<Arc<ApiState>> {
    axum::Router::new()
        .route(
            "/api/v1/projects/{project_id}/member-invitations",
            axum::routing::post(create_project).get(list_project_invitations),
        )
        .route(
            "/api/v1/courses/{course_id}/member-invitations",
            axum::routing::post(create_course).get(list_course_invitations),
        )
        .route(
            "/api/v1/courses/{course_id}/members",
            axum::routing::get(list_course_members),
        )
        .route(
            "/api/v1/courses/{course_id}/members/{actor_id}",
            axum::routing::delete(remove_course_member),
        )
        .route("/api/v1/member-invitations", axum::routing::get(incoming))
        .route(
            "/api/v1/member-invitations/{invitation_id}",
            axum::routing::get(get_invitation),
        )
        .route(
            "/api/v1/member-invitations/{invitation_id}/accept",
            axum::routing::post(accept),
        )
        .route(
            "/api/v1/member-invitations/{invitation_id}/revoke",
            axum::routing::post(revoke),
        )
        .route_layer(axum::middleware::from_fn(private_responses))
}

async fn private_responses(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}
