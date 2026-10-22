//! Resolves campus invitation recipients after current Control governance checks.
use super::{ApiError, AppState, authenticated_session, require_browser_origin};
use auth::upsert_actor_with_metadata;
use axum::{
    body::Bytes,
    http::{HeaderMap, Method, Uri},
};
use contracts::membership_governance::{
    CreateCourseMembershipInvitation, CreateProjectMembershipInvitation, MembershipInvitationScope,
    ResolvedMembershipInvitation,
};
use contracts::{CourseId, ProjectId};
use std::str::FromStr;

pub(super) fn valid_invitation_path(path: &str) -> bool {
    match path.split('/').collect::<Vec<_>>().as_slice() {
        ["", "api", "v1", "member-invitations"] => true,
        ["", "api", "v1", "member-invitations", id]
        | [
            "",
            "api",
            "v1",
            "member-invitations",
            id,
            "accept" | "revoke",
        ] => id.parse::<contracts::MembershipInvitationId>().is_ok(),
        _ => false,
    }
}

pub(super) async fn resolve_request(
    state: &AppState,
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    body: Bytes,
) -> Result<Bytes, ApiError> {
    if *method != Method::POST {
        return Ok(body);
    }
    let segments = uri.path().split('/').collect::<Vec<_>>();
    let (scope, username, expires_at, membership_expires_at) = match segments.as_slice() {
        ["", "api", "v1", "projects", id, "member-invitations"] => {
            let project_id = ProjectId::from_str(id)
                .map_err(|_| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))?;
            let request = contracts::parse_strict_json::<CreateProjectMembershipInvitation>(&body)
                .map_err(|_| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))?;
            (
                MembershipInvitationScope::Project {
                    project_id,
                    object_role: request.object_role,
                },
                request.username,
                request.expires_at,
                request.membership_expires_at,
            )
        }
        ["", "api", "v1", "courses", id, "member-invitations"] => {
            let course_id = CourseId::from_str(id)
                .map_err(|_| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))?;
            let request = contracts::parse_strict_json::<CreateCourseMembershipInvitation>(&body)
                .map_err(|_| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))?;
            (
                MembershipInvitationScope::Course {
                    course_id,
                    object_role: request.object_role,
                },
                request.username,
                request.expires_at,
                request.membership_expires_at,
            )
        }
        _ => return Ok(body),
    };
    let session = authenticated_session(state, headers).await?;
    require_browser_origin(state, headers)?;
    let supplied = headers
        .get(state.deployment.browser.csrf_header_name.as_str())
        .and_then(|value| value.to_str().ok());
    auth::verify_csrf_token(&session.csrf_token, supplied).map_err(ApiError::from)?;
    authorize_governance(state, headers, &scope, &session).await?;
    let username = username.trim();
    if username.is_empty() || username.chars().count() > 128 {
        return Err(ApiError::bad_request(
            "LW_ACCESS_DIRECTORY_USERNAME_INVALID",
        ));
    }
    let resolved = state
        .directory
        .resolve_username(username)
        .await
        .map_err(ApiError::from)?;
    let local = upsert_actor_with_metadata(
        &state.pool,
        state.config.issuer.as_str(),
        &resolved.subject,
        Some(&resolved.username),
        Some(&resolved.display_name),
    )
    .await
    .map_err(ApiError::from)?;
    let recipient_actor_id = local
        .actor_id
        .to_string()
        .parse()
        .map_err(|_| ApiError::internal("LW_AUTH_ACTOR_INVALID"))?;
    serde_json::to_vec(&ResolvedMembershipInvitation {
        recipient_actor_id,
        scope,
        expires_at,
        membership_expires_at,
    })
    .map(Bytes::from)
    .map_err(|_| ApiError::internal("LW_CONTRACT_SERIALIZATION_FAILED"))
}

async fn authorize_governance(
    state: &AppState,
    headers: &HeaderMap,
    scope: &MembershipInvitationScope,
    session: &auth::BffSession,
) -> Result<(), ApiError> {
    match scope {
        MembershipInvitationScope::Project { project_id, .. } => {
            super::proxy::authorize_project_membership_governance(
                state,
                headers,
                *project_id,
                session,
            )
            .await?;
        }
        MembershipInvitationScope::Course { course_id, .. } => {
            let membership = auth::load_membership_snapshot(&state.pool, session.actor_id)
                .await
                .map_err(ApiError::from)?;
            auth::object_authorization::authorize_course_governance_action(
                &auth::AuthorizationContext {
                    actor: super::actor_from_session(session)?,
                    course_memberships: membership.course_memberships,
                    project_memberships: membership.project_memberships,
                    now: time::OffsetDateTime::now_utc(),
                },
                *course_id,
                contracts::object_authorization::ObjectAction::ManageMembers,
            )
            .map_err(ApiError::from)?;
        }
    }
    Ok(())
}
