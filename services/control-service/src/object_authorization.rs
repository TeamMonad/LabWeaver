//! Concrete Control member writes and project action checks.
use auth::RepositoryError;
use contracts::{
    ActorId, CourseId, MembershipState, PlatformRole, ProjectId, ProjectMembership, Revision,
};
use sqlx::{Postgres, Transaction};
use time::OffsetDateTime;
fn role_name(role: PlatformRole) -> &'static str {
    match role {
        PlatformRole::Teacher => "teacher",
        PlatformRole::Student => "student",
        PlatformRole::PlatformAdmin => "platform_admin",
    }
}
/// Inserts the initial active Project membership for a newly-created project.
///
/// Control calls this Control-owned helper while its project row is held in the
/// same Postgres transaction. The database grant for the Control runtime is
/// intentionally limited to this table; the helper therefore does not expose
/// a generic membership mutation surface.
pub async fn insert_project_owner_membership(
    transaction: &mut Transaction<'_, Postgres>,
    project_id: ProjectId,
    actor_id: ActorId,
    course_id: Option<CourseId>,
    role: PlatformRole,
) -> Result<ProjectMembership, RepositoryError> {
    let revision = Revision::new(1).map_err(|_| RepositoryError::MembershipInvalid)?;
    sqlx::query(
        "INSERT INTO control.project_memberships \
         (course_id, project_id, actor_id, role, object_role, state, revision, expires_at) \
         VALUES ($1,$2,$3,$4,'owner','active',1,NULL)",
    )
    .bind(course_id.map(CourseId::as_uuid))
    .bind(project_id.as_uuid())
    .bind(actor_id.as_uuid())
    .bind(role_name(role))
    .execute(&mut **transaction)
    .await?;
    Ok(ProjectMembership {
        object_role: contracts::object_authorization::ProjectObjectRole::Owner,
        course_id,
        project_id,
        actor_id,
        username: None,
        display_name: None,
        role,
        state: MembershipState::Active,
        revision,
        expires_at: None,
    })
}

/// Verifies that an actor has a live course membership with the role used to
/// associate a newly-created project. The check runs on the caller's
/// transaction so project creation cannot commit an unvalidated association.
pub async fn require_course_membership(
    transaction: &mut Transaction<'_, Postgres>,
    course_id: CourseId,
    actor_id: ActorId,
    role: PlatformRole,
    now: OffsetDateTime,
) -> Result<(), RepositoryError> {
    let exists = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM control.course_memberships \
         WHERE course_id=$1 AND actor_id=$2 AND role=$3 AND state='active' \
           AND (expires_at IS NULL OR expires_at > $4))",
    )
    .bind(course_id.as_uuid())
    .bind(actor_id.as_uuid())
    .bind(role_name(role))
    .bind(now)
    .fetch_one(&mut **transaction)
    .await?;
    if !exists {
        return Err(RepositoryError::MembershipInvalid);
    }
    Ok(())
}

/// Reads the Control fact while a governance transaction holds the project row.
pub(super) async fn permits_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    project_id: ProjectId,
    actor_id: ActorId,
    action: contracts::object_authorization::ObjectAction,
) -> Result<bool, crate::ControlError> {
    let role: Option<String> = sqlx::query_scalar("SELECT object_role FROM control.project_memberships WHERE project_id=$1 AND actor_id=$2 AND state='active' AND (expires_at IS NULL OR expires_at>clock_timestamp())")
        .bind(project_id.as_uuid()).bind(actor_id.as_uuid()).fetch_optional(&mut **transaction).await.map_err(super::db)?;
    let Some(role) = role else {
        return Ok(false);
    };
    let role: contracts::object_authorization::ProjectObjectRole =
        serde_json::from_value(serde_json::Value::String(role))
            .map_err(|_| crate::ControlError::PersistenceIdentityMismatch)?;
    Ok(role.actions().contains(&action))
}
