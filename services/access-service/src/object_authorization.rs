//! Read-only Control member checks for Access's established transport sessions.

use sqlx::PgPool;

/// Rechecks a console's actor, BFF, grant and Control member facts.
pub(super) async fn console_active(pool: &PgPool, session_id: uuid::Uuid) -> bool {
    tokio::time::timeout(
        std::time::Duration::from_secs(contracts::object_authorization::AUTHORIZATION_CHECK_TIMEOUT_SECONDS),
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM access.console_sessions c \
             JOIN access.bff_sessions b ON b.session_id=c.bff_session_id AND b.actor_id=c.actor_id \
             JOIN access.actors a ON a.actor_id=c.actor_id \
             JOIN access.access_grants g ON g.grant_id=c.access_grant_id \
             JOIN control.project_memberships pm ON pm.project_id=c.project_id AND pm.actor_id=c.actor_id \
             WHERE c.session_id=$1 AND c.state='active' AND c.authorization_expires_at>clock_timestamp() \
               AND a.disabled_at IS NULL AND b.revoked_at IS NULL AND b.expires_at>clock_timestamp() AND b.idle_expires_at>clock_timestamp() \
               AND g.actor_id=c.actor_id AND g.project_id=c.project_id AND g.environment_id=c.environment_id \
               AND g.revision=c.access_grant_revision AND g.environment_revision=c.environment_revision \
               AND g.state='active' AND g.not_before<=clock_timestamp() AND g.expires_at>clock_timestamp() \
               AND pm.state='active' AND pm.object_role IN ('editor','manager','owner') \
               AND (pm.expires_at IS NULL OR pm.expires_at>clock_timestamp()) \
               AND (c.course_id IS NULL OR EXISTS(SELECT 1 FROM control.course_memberships cm \
                   WHERE cm.course_id=c.course_id AND cm.actor_id=c.actor_id AND cm.role=pm.role \
                     AND cm.state='active' AND (cm.expires_at IS NULL OR cm.expires_at>clock_timestamp()))))",
        ).bind(session_id).fetch_one(pool),
    ).await.is_ok_and(|result| result.unwrap_or(false))
}

/// Marks an SSH session for termination when the current grant or Control scope no longer permits it.
pub(super) async fn recheck_gateway(
    pool: &PgPool,
    session_id: contracts::GatewaySessionId,
    gateway: &str,
    connection: &str,
) -> Result<(), super::ApiError> {
    tokio::time::timeout(
        std::time::Duration::from_secs(contracts::object_authorization::AUTHORIZATION_CHECK_TIMEOUT_SECONDS),
        sqlx::query(
            "UPDATE access.gateway_sessions s SET state='terminating',revision=revision+1, \
             termination_requested_at=clock_timestamp(),terminate_by=clock_timestamp()+interval '60 seconds' \
             WHERE session_id=$1 AND gateway_identity=$2 AND connection_id=$3 AND state='active' \
               AND NOT EXISTS(SELECT 1 FROM access.access_grants g \
                 JOIN access.actors a ON a.actor_id=g.actor_id \
                 JOIN control.project_memberships pm ON pm.project_id=g.project_id AND pm.actor_id=g.actor_id \
                 WHERE g.grant_id=s.grant_id AND g.revision=s.grant_revision AND g.state='active' \
                   AND g.not_before<=clock_timestamp() AND g.expires_at>clock_timestamp() AND a.disabled_at IS NULL \
                   AND pm.state='active' AND pm.object_role IN ('editor','manager','owner') \
                   AND (pm.expires_at IS NULL OR pm.expires_at>clock_timestamp()) \
                   AND (g.course_id IS NULL OR EXISTS(SELECT 1 FROM control.course_memberships cm \
                     WHERE cm.course_id=g.course_id AND cm.actor_id=g.actor_id AND cm.role=pm.role \
                       AND cm.state='active' AND (cm.expires_at IS NULL OR cm.expires_at>clock_timestamp()))))",
        ).bind(session_id.as_uuid()).bind(gateway).bind(connection).execute(pool),
    ).await.map_err(|_| super::ApiError::unavailable("LW_AUTH_MEMBERSHIP_UNAVAILABLE"))?
        .map_err(|_| super::ApiError::unavailable("LW_AUTH_MEMBERSHIP_UNAVAILABLE"))?;
    Ok(())
}

/// Parses only existing environment routes. Each action binds the persisted object's project.
pub(super) fn environment_route_action(
    method: &axum::http::Method,
    path: &str,
) -> Result<
    Option<(
        contracts::EnvironmentId,
        contracts::object_authorization::ObjectAction,
    )>,
    super::ApiError,
> {
    use axum::http::Method;

    let segments: Vec<_> = path.split('/').collect();
    let (environment, action) = match (method, segments.as_slice()) {
        (
            &Method::GET,
            ["", "api", "v1", "environments", id]
            | [
                "",
                "api",
                "v1",
                "environments",
                id,
                "endpoints" | "operations",
            ]
            | ["", "api", "v1", "environments", id, "operations", _],
        ) => (*id, "getEnvironment"),
        (&Method::DELETE, ["", "api", "v1", "environments", id])
        | (
            &Method::POST,
            [
                "",
                "api",
                "v1",
                "environments",
                id,
                "reset" | "retry" | "recover",
            ],
        ) => (*id, "resetEnvironment"),
        (
            &Method::POST,
            [
                "",
                "api",
                "v1",
                "environments",
                id,
                "start" | "restart" | "template-upgrade",
            ],
        ) => (*id, "startEnvironment"),
        (&Method::POST, ["", "api", "v1", "environments", id, "stop" | "cancel"]) => {
            (*id, "stopEnvironment")
        }
        _ => return Ok(None),
    };
    let action = contracts::object_authorization::environment_operation_action(action)
        .ok_or_else(|| super::ApiError::forbidden("LW_AUTH_SCOPE_DENIED"))?;
    let environment = environment
        .parse()
        .map_err(|_| super::ApiError::bad_request("LW_CONTRACT_DOCUMENT_INVALID"))?;
    Ok(Some((environment, action)))
}

pub(super) async fn authorize_current_project(
    state: &super::AppState,
    headers: &axum::http::HeaderMap,
    project_id: contracts::ProjectId,
    course_id: Option<contracts::CourseId>,
    action: contracts::object_authorization::ObjectAction,
) -> Result<(), super::ApiError> {
    let session = super::authenticated_session(state, headers).await?;
    let actor = super::actor_from_session(&session)?;
    let memberships = auth::load_membership_snapshot(&state.pool, session.actor_id)
        .await
        .map_err(super::ApiError::from)?;
    let context = auth::AuthorizationContext {
        actor,
        project_memberships: memberships.project_memberships,
        course_memberships: memberships.course_memberships,
        now: time::OffsetDateTime::now_utc(),
    };
    auth::object_authorization::authorize_project_action(&context, project_id, action)
        .map_err(super::ApiError::from)?;
    if let Some(course_id) = course_id {
        auth::authorize(
            &context,
            contracts::AuthorizationScope::Course { course_id },
            &std::collections::BTreeSet::from([
                contracts::PlatformRole::Teacher,
                contracts::PlatformRole::Student,
                contracts::PlatformRole::PlatformAdmin,
            ]),
        )
        .map_err(super::ApiError::from)?;
    }
    Ok(())
}
