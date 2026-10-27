//! Resolves campus invitation recipients after current Control governance checks.
use super::{ApiError, AppState, authenticated_session, require_browser_origin};
use auth::upsert_actor_with_metadata;
use axum::{
    body::Bytes,
    http::{HeaderMap, Method, Uri},
};
use contracts::membership_governance::{
    CourseRosterImportRequest, CourseRosterRole, CreateCourseMembershipInvitation,
    CreateProjectMembershipInvitation, MembershipInvitationScope,
    ResolvedCourseRosterImportRequest, ResolvedCourseRosterRow, ResolvedMembershipInvitation,
    parse_course_roster_csv,
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

#[allow(
    clippy::too_many_lines,
    reason = "CSV resolution keeps authentication, directory resolution, and identity binding in one auditable boundary."
)]
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
    if let ["", "api", "v1", "courses", id, "roster", "import"] = segments.as_slice() {
        let course_id = CourseId::from_str(id)
            .map_err(|_| ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))?;
        let request = contracts::parse_strict_json::<CourseRosterImportRequest>(&body)
            .map_err(|_| ApiError::bad_request("LW_COURSE_ROSTER_IMPORT_INVALID"))?;
        let session = authenticated_session(state, headers).await?;
        require_browser_origin(state, headers)?;
        let supplied = headers
            .get(state.deployment.browser.csrf_header_name.as_str())
            .and_then(|value| value.to_str().ok());
        auth::verify_csrf_token(&session.csrf_token, supplied).map_err(ApiError::from)?;
        let scope = MembershipInvitationScope::Course {
            course_id,
            object_role: contracts::membership_governance::CourseObjectRole::Participant,
        };
        authorize_governance(state, headers, &scope, &session).await?;
        let rows = parse_course_roster_csv(&request.csv).map_err(ApiError::bad_request)?;
        let mut resolved = Vec::with_capacity(rows.len());
        for (row_number, username, role) in rows {
            let directory = state
                .directory
                .resolve_username(&username)
                .await
                .map_err(|error| match error {
                    super::directory::DirectoryError::UserNotFound => {
                        ApiError::bad_request("LW_COURSE_ROSTER_ACCOUNT_NOT_FOUND")
                    }
                    _ => ApiError::from(error),
                })?;
            let local = upsert_actor_with_metadata(
                &state.pool,
                state.config.issuer.as_str(),
                &directory.subject,
                Some(&directory.username),
                Some(&directory.display_name),
            )
            .await
            .map_err(ApiError::from)?;
            let actor_id = local
                .actor_id
                .to_string()
                .parse()
                .map_err(|_| ApiError::internal("LW_AUTH_ACTOR_INVALID"))?;
            let role = match role.as_deref().unwrap_or("participant") {
                "participant" => CourseRosterRole::Participant,
                "teaching_assistant" => CourseRosterRole::TeachingAssistant,
                _ => return Err(ApiError::bad_request("LW_COURSE_ROSTER_ROLE_INVALID")),
            };
            resolved.push(ResolvedCourseRosterRow {
                row_number,
                campus_username: directory.username,
                actor_id,
                role,
            });
        }
        return serde_json::to_vec(&ResolvedCourseRosterImportRequest { rows: resolved })
            .map(Bytes::from)
            .map_err(|_| ApiError::internal("LW_COURSE_ROSTER_SERIALIZATION_FAILED"));
    }
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
