//! Teacher authority captured inside the existing complete authoring approval transaction.
use super::{ControlError, db};
use contracts::runtime_authorization::CourseTeacherApprovalScope;
use contracts::{AuthenticatedActor, CourseId, PlatformRole, Revision, UtcTimestamp};
use sqlx::{Postgres, Row, Transaction};

pub(super) async fn teacher_scope(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AuthenticatedActor,
    course_id: Option<CourseId>,
    now: UtcTimestamp,
) -> Result<Option<CourseTeacherApprovalScope>, ControlError> {
    // Administrator and Teacher identities do not substitute for this course's instructor fact.
    if !actor.roles.contains(&PlatformRole::Teacher) || actor.expires_at <= now {
        return Err(ControlError::TeacherApprovalDenied);
    }
    let Some(course_id) = course_id else {
        return Ok(None);
    };
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!("control-course-membership:{course_id}"))
        .execute(&mut **tx)
        .await
        .map_err(db)?;
    let row=sqlx::query("SELECT revision FROM control.course_memberships WHERE course_id=$1 AND actor_id=$2 AND role='teacher' AND object_role='instructor' AND state='active' AND (expires_at IS NULL OR expires_at>$3) FOR SHARE")
        .bind(course_id.as_uuid()).bind(actor.actor_id.as_uuid()).bind(now.get()).fetch_optional(&mut **tx).await.map_err(db)?.ok_or(ControlError::TeacherApprovalDenied)?;
    let revision: i64 = row.try_get("revision").map_err(db)?;
    Ok(Some(CourseTeacherApprovalScope {
        actor_id: actor.actor_id,
        course_id,
        membership_revision: Revision::new(
            u64::try_from(revision).map_err(|_| ControlError::PersistenceIdentityMismatch)?,
        )
        .map_err(|_| ControlError::PersistenceIdentityMismatch)?,
    }))
}
