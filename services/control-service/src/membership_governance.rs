//! Control's course roster and recipient-bound invitations.

use super::pagination::ListPage;
use super::{ControlError, ControlService, canonical_hash, db, i64_revision};
use contracts::http::{IdempotencyKey, RemoveProjectMembershipRequest};
use contracts::membership_governance::ControlListQuery;
use contracts::membership_governance::{
    AcceptMembershipInvitation, CourseCalendar, CourseCalendarState, CourseLatePolicy,
    CourseMembershipPage, CourseMembershipView, CourseObjectRole, CourseRosterImportResult,
    CourseRosterImportRowResult, CourseRosterImportRowState, CourseRosterRole,
    MembershipInvitation, MembershipInvitationPage, MembershipInvitationScope,
    MembershipInvitationState, ResolvedCourseRosterImportRequest, ResolvedMembershipInvitation,
    UpdateCourseCalendar,
};
use contracts::object_authorization::{ObjectAction, ProjectObjectRole};
use contracts::{
    ActorId, AuthenticatedActor, CourseId, MembershipInvitationId, MembershipState, PlatformRole,
    Revision, UtcTimestamp,
};
use persistence_sqlx::{Domain, IdempotencyDecision, IdempotencyStore};
use serde_json::{Value, json};
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

const CREATE: &str = "control_create_membership_invitation_v1";
const ACCEPT: &str = "control_accept_membership_invitation_v1";
const REVOKE: &str = "control_revoke_membership_invitation_v1";
const REMOVE: &str = "control_remove_course_membership_v1";

struct InvitationRecord {
    invitation: MembershipInvitation,
    issuer_revision: Option<Revision>,
    target_revision: Option<Revision>,
}

fn scope_key(scope: &MembershipInvitationScope) -> (&'static str, Uuid) {
    match scope {
        MembershipInvitationScope::Project { project_id, .. } => ("project", project_id.as_uuid()),
        MembershipInvitationScope::Course { course_id, .. } => ("course", course_id.as_uuid()),
    }
}

fn is_admin(actor: &AuthenticatedActor) -> bool {
    actor.roles.contains(&PlatformRole::PlatformAdmin)
}
async fn admin_authorization_revision(
    tx: &mut Transaction<'_, Postgres>,
    actor: ActorId,
) -> Result<Option<Revision>, ControlError> {
    let active: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM access.current_authorization_sessions WHERE actor_id=$1 AND 'platform_admin'=ANY(platform_roles) AND expires_at>clock_timestamp() AND idle_expires_at>clock_timestamp())",
    )
    .bind(actor.as_uuid())
    .fetch_one(&mut **tx)
    .await
    .map_err(db)?;
    if active {
        Ok(Some(
            Revision::new(1).map_err(|_| ControlError::PersistenceIdentityMismatch)?,
        ))
    } else {
        Ok(None)
    }
}

async fn database_now(tx: &mut Transaction<'_, Postgres>) -> Result<UtcTimestamp, ControlError> {
    let value = sqlx::query_scalar("SELECT date_trunc('milliseconds',clock_timestamp())")
        .fetch_one(&mut **tx)
        .await
        .map_err(db)?;
    UtcTimestamp::from_utc(value).map_err(|_| ControlError::PersistenceIdentityMismatch)
}

async fn account_active(
    tx: &mut Transaction<'_, Postgres>,
    actor: ActorId,
) -> Result<bool, ControlError> {
    sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM access.actors WHERE actor_id=$1 AND disabled_at IS NULL)",
    )
    .bind(actor.as_uuid())
    .fetch_one(&mut **tx)
    .await
    .map_err(db)
}

async fn require_actor(
    tx: &mut Transaction<'_, Postgres>,
    actor: &AuthenticatedActor,
) -> Result<UtcTimestamp, ControlError> {
    let now = database_now(tx).await?;
    if actor.roles.is_empty()
        || actor.expires_at <= now
        || !account_active(tx, actor.actor_id).await?
    {
        return Err(ControlError::InvitationDenied);
    }
    Ok(now)
}

async fn lock_scope(
    tx: &mut Transaction<'_, Postgres>,
    scope: &MembershipInvitationScope,
    archived_ok: bool,
) -> Result<(), ControlError> {
    match scope {
        MembershipInvitationScope::Project { project_id, .. } => {
            let project = super::project_from_tx(tx, *project_id).await?;
            if !archived_ok && project.state == contracts::project::ProjectState::Archived {
                return Err(ControlError::ProjectArchived);
            }
        }
        MembershipInvitationScope::Course { course_id, .. } => {
            // All course member mutations use this concrete fence until Course has its aggregate row.
            sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
                .bind(format!("control-course-membership:{course_id}"))
                .execute(&mut **tx)
                .await
                .map_err(db)?;
            let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM control.course_memberships WHERE course_id=$1) OR EXISTS(SELECT 1 FROM control.projects WHERE course_id=$1)")
                .bind(course_id.as_uuid()).fetch_one(&mut **tx).await.map_err(db)?;
            if !exists {
                return Err(ControlError::NotFound);
            }
        }
    }
    Ok(())
}

async fn governor_revision(
    tx: &mut Transaction<'_, Postgres>,
    scope: &MembershipInvitationScope,
    actor: ActorId,
) -> Result<Option<Revision>, ControlError> {
    let row = match scope {
        MembershipInvitationScope::Project { project_id, .. } => sqlx::query("SELECT object_role,role,revision FROM control.project_memberships WHERE project_id=$1 AND actor_id=$2 AND state='active' AND (expires_at IS NULL OR expires_at>clock_timestamp()) FOR UPDATE").bind(project_id.as_uuid()).bind(actor.as_uuid()).fetch_optional(&mut **tx).await.map_err(db)?,
        MembershipInvitationScope::Course { course_id, .. } => sqlx::query("SELECT object_role,role,revision FROM control.course_memberships WHERE course_id=$1 AND actor_id=$2 AND state='active' AND (expires_at IS NULL OR expires_at>clock_timestamp()) FOR UPDATE").bind(course_id.as_uuid()).bind(actor.as_uuid()).fetch_optional(&mut **tx).await.map_err(db)?,
    };
    let Some(row) = row else {
        return Ok(None);
    };
    let object_role: String = row.try_get("object_role").map_err(db)?;
    let permits = match scope {
        MembershipInvitationScope::Project { .. } => {
            serde_json::from_value::<ProjectObjectRole>(json!(object_role))
                .map_err(|_| ControlError::PersistenceIdentityMismatch)?
                .actions()
                .contains(&ObjectAction::ManageMembers)
        }
        MembershipInvitationScope::Course { .. } => {
            object_role == "instructor"
                && row.try_get::<String, _>("role").map_err(db)? == "teacher"
        }
    };
    if permits {
        revision(row.try_get("revision").map_err(db)?).map(Some)
    } else {
        Ok(None)
    }
}

async fn govern(
    tx: &mut Transaction<'_, Postgres>,
    scope: &MembershipInvitationScope,
    actor: &AuthenticatedActor,
) -> Result<Option<Revision>, ControlError> {
    if is_admin(actor) {
        return admin_authorization_revision(tx, actor.actor_id).await;
    }
    if matches!(scope, MembershipInvitationScope::Course { .. })
        && !actor.roles.contains(&PlatformRole::Teacher)
    {
        return Err(ControlError::CourseGovernanceDenied);
    }
    governor_revision(tx, scope, actor.actor_id)
        .await?
        .map(Some)
        .ok_or(ControlError::InvitationDenied)
}

fn revision(value: i64) -> Result<Revision, ControlError> {
    Revision::new(u64::try_from(value).map_err(|_| ControlError::PersistenceIdentityMismatch)?)
        .map_err(|_| ControlError::PersistenceIdentityMismatch)
}

fn next_revision(value: Option<Revision>) -> Result<Revision, ControlError> {
    Revision::new(
        value
            .map_or(Some(1), |value| value.get().checked_add(1))
            .ok_or(ControlError::ContractInvalid)?,
    )
    .map_err(|_| ControlError::ContractInvalid)
}

async fn member_revision(
    tx: &mut Transaction<'_, Postgres>,
    scope: &MembershipInvitationScope,
    actor: ActorId,
) -> Result<Option<Revision>, ControlError> {
    let (kind, id) = scope_key(scope);
    let sql = if kind == "project" {
        "SELECT revision FROM control.project_memberships WHERE project_id=$1 AND actor_id=$2 FOR UPDATE"
    } else {
        "SELECT revision FROM control.course_memberships WHERE course_id=$1 AND actor_id=$2 FOR UPDATE"
    };
    let value: Option<i64> = sqlx::query_scalar(sql)
        .bind(id)
        .bind(actor.as_uuid())
        .fetch_optional(&mut **tx)
        .await
        .map_err(db)?;
    value.map(revision).transpose()
}

async fn reserve(
    tx: &mut Transaction<'_, Postgres>,
    operation: &str,
    key: &IdempotencyKey,
    intent: Value,
) -> Result<Option<Value>, ControlError> {
    match IdempotencyStore::reserve(
        tx,
        Domain::Control,
        operation,
        key.as_str(),
        canonical_hash(&intent)?,
    )
    .await
    .map_err(|_| ControlError::PersistenceFailed)?
    {
        IdempotencyDecision::Replay(value) => Ok(Some(value)),
        IdempotencyDecision::Reserved => Ok(None),
        IdempotencyDecision::Conflict => Err(ControlError::IdempotencyConflict),
        IdempotencyDecision::InProgress => Err(ControlError::OperationInProgress),
    }
}

async fn complete(
    tx: &mut Transaction<'_, Postgres>,
    operation: &str,
    key: &IdempotencyKey,
    result: &impl serde::Serialize,
) -> Result<(), ControlError> {
    IdempotencyStore::complete(
        tx,
        Domain::Control,
        operation,
        key.as_str(),
        &serde_json::to_value(result).map_err(|_| ControlError::ContractInvalid)?,
    )
    .await
    .map_err(|_| ControlError::PersistenceFailed)
}

fn decode(row: &sqlx::postgres::PgRow) -> Result<InvitationRecord, ControlError> {
    let invitation: MembershipInvitation =
        serde_json::from_value(row.try_get("contract").map_err(db)?)
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
    let (kind, scope) = scope_key(&invitation.scope);
    if invitation.id.as_uuid() != row.try_get::<Uuid, _>("invitation_id").map_err(db)?
        || invitation.recipient_actor_id.as_uuid()
            != row.try_get::<Uuid, _>("recipient_actor_id").map_err(db)?
        || invitation.created_by.as_uuid() != row.try_get::<Uuid, _>("created_by").map_err(db)?
        || kind != row.try_get::<String, _>("scope_kind").map_err(db)?
        || scope != row.try_get::<Uuid, _>("scope_id").map_err(db)?
        || i64_revision(invitation.revision)? != row.try_get::<i64, _>("revision").map_err(db)?
        || invitation.expires_at.get()
            != row
                .try_get::<time::OffsetDateTime, _>("expires_at")
                .map_err(db)?
        || serde_json::to_value(invitation.state).map_err(|_| ControlError::ContractInvalid)?
            != json!(row.try_get::<String, _>("state").map_err(db)?)
    {
        return Err(ControlError::PersistenceIdentityMismatch);
    }
    Ok(InvitationRecord {
        invitation,
        issuer_revision: row
            .try_get::<Option<i64>, _>("issuer_membership_revision")
            .map_err(db)?
            .map(revision)
            .transpose()?,
        target_revision: row
            .try_get::<Option<i64>, _>("target_membership_revision")
            .map_err(db)?
            .map(revision)
            .transpose()?,
    })
}

fn current(mut value: MembershipInvitation, now: UtcTimestamp) -> MembershipInvitation {
    if value.state == MembershipInvitationState::Pending && value.expires_at <= now {
        value.state = MembershipInvitationState::Expired;
    }
    value
}

async fn save(
    tx: &mut Transaction<'_, Postgres>,
    value: &MembershipInvitation,
) -> Result<(), ControlError> {
    let state = serde_json::to_value(value.state).map_err(|_| ControlError::ContractInvalid)?;
    sqlx::query("UPDATE control.membership_invitations SET state=$2,revision=$3,contract=$4 WHERE invitation_id=$1").bind(value.id.as_uuid()).bind(state.as_str().ok_or(ControlError::ContractInvalid)?)
        .bind(i64_revision(value.revision)?).bind(serde_json::to_value(value).map_err(|_| ControlError::ContractInvalid)?).execute(&mut **tx).await.map_err(db)?;
    Ok(())
}

async fn event(
    tx: &mut Transaction<'_, Postgres>,
    scope: &MembershipInvitationScope,
    id: Uuid,
    revision: Revision,
    payload: Value,
) -> Result<(), ControlError> {
    match scope {
        MembershipInvitationScope::Project { project_id, .. } => {
            super::append_project_sse(
                tx,
                *project_id,
                "member_authorization.changed.v1",
                id,
                revision,
                payload,
            )
            .await
        }
        MembershipInvitationScope::Course { course_id, .. } => {
            super::append_sse(
                tx,
                *course_id,
                "member_authorization.changed.v1",
                id,
                revision,
                payload,
            )
            .await
        }
    }
}

impl ControlService {
    /// Lists invitations for current governors without exposing them to ordinary members.
    pub async fn scoped_membership_invitations(
        &self,
        actor: &AuthenticatedActor,
        scope: &MembershipInvitationScope,
        query: &ControlListQuery,
    ) -> Result<MembershipInvitationPage, ControlError> {
        let page = ListPage::new(
            query,
            &("scoped-invitations", actor.actor_id, scope_key(scope)),
            self.cursor_signing_keys.as_deref(),
        )?;
        let mut tx = self.pool.begin().await.map_err(db)?;
        let now = require_actor(&mut tx, actor).await?;
        lock_scope(&mut tx, scope, true).await?;
        govern(&mut tx, scope, actor).await?;
        let (kind, id) = scope_key(scope);
        let rows=sqlx::query("SELECT *,invitation_id AS cursor_id FROM control.membership_invitations WHERE scope_kind=$1 AND scope_id=$2 AND ($4::uuid IS NULL OR invitation_id<$4) ORDER BY invitation_id DESC LIMIT $3")
            .bind(kind).bind(id).bind(i64::from(page.limit)+1).bind(page.after).fetch_all(&mut *tx).await.map_err(db)?;
        let result = page.finish(&rows, |row| {
            decode(row).map(|record| current(record.invitation, now))
        })?;
        tx.commit().await.map_err(db)?;
        Ok(result)
    }
    /// Creates an invitation for one resolved recipient under current governance.
    pub async fn create_membership_invitation(
        &self,
        actor: &AuthenticatedActor,
        request: &ResolvedMembershipInvitation,
        key: &IdempotencyKey,
    ) -> Result<MembershipInvitation, ControlError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let now = require_actor(&mut tx, actor).await?;
        lock_scope(&mut tx, &request.scope, false).await?;
        let issuer_revision = govern(&mut tx, &request.scope, actor).await?;
        if request.recipient_actor_id == actor.actor_id
            || request.expires_at <= now
            || request
                .membership_expires_at
                .is_some_and(|expiry| expiry <= now)
            || !account_active(&mut tx, request.recipient_actor_id).await?
        {
            return Err(ControlError::ContractInvalid);
        }
        if let MembershipInvitationScope::Project {
            project_id,
            object_role,
        } = &request.scope
            && (*object_role == ProjectObjectRole::Owner
                || super::project_from_tx(&mut tx, *project_id)
                    .await?
                    .owner_actor_id
                    == request.recipient_actor_id)
        {
            return Err(ControlError::OwnerMembershipProtected);
        }
        if let Some(value) = reserve(
            &mut tx,
            CREATE,
            key,
            json!({"actor":actor.actor_id,"request":request}),
        )
        .await?
        {
            return serde_json::from_value(value)
                .map_err(|_| ControlError::PersistenceIdentityMismatch);
        }
        let target_revision =
            member_revision(&mut tx, &request.scope, request.recipient_actor_id).await?;
        let invitation = MembershipInvitation {
            id: MembershipInvitationId::new(),
            scope: request.scope.clone(),
            recipient_actor_id: request.recipient_actor_id,
            created_by: actor.actor_id,
            created_at: now,
            expires_at: request.expires_at,
            membership_expires_at: request.membership_expires_at,
            state: MembershipInvitationState::Pending,
            revision: next_revision(None)?,
            accepted_membership_revision: None,
        };
        let (kind, scope) = scope_key(&invitation.scope);
        sqlx::query("INSERT INTO control.membership_invitations (invitation_id,scope_kind,scope_id,recipient_actor_id,created_by,issuer_membership_revision,target_membership_revision,state,revision,contract,expires_at,created_at) VALUES ($1,$2,$3,$4,$5,$6,$7,'pending',1,$8,$9,$10)")
            .bind(invitation.id.as_uuid()).bind(kind).bind(scope).bind(invitation.recipient_actor_id.as_uuid()).bind(actor.actor_id.as_uuid()).bind(issuer_revision.map(i64_revision).transpose()?).bind(target_revision.map(i64_revision).transpose()?)
            .bind(serde_json::to_value(&invitation).map_err(|_| ControlError::ContractInvalid)?).bind(invitation.expires_at.get()).bind(now.get()).execute(&mut *tx).await.map_err(db)?;
        // Proposed accounts are private. Broadcast only the accepted member fact.
        complete(&mut tx, CREATE, key, &invitation).await?;
        tx.commit().await.map_err(db)?;
        Ok(invitation)
    }

    /// Returns only this actor's invitations with bounded pagination.
    pub async fn incoming_membership_invitations(
        &self,
        actor: &AuthenticatedActor,
        query: &ControlListQuery,
    ) -> Result<MembershipInvitationPage, ControlError> {
        let page = ListPage::new(
            query,
            &("incoming-invitations", actor.actor_id),
            self.cursor_signing_keys.as_deref(),
        )?;
        let mut tx = self.pool.begin().await.map_err(db)?;
        let now = require_actor(&mut tx, actor).await?;
        let rows=sqlx::query("SELECT *,invitation_id AS cursor_id FROM control.membership_invitations WHERE recipient_actor_id=$1 AND ($3::uuid IS NULL OR invitation_id<$3) ORDER BY invitation_id DESC LIMIT $2").bind(actor.actor_id.as_uuid()).bind(i64::from(page.limit)+1).bind(page.after).fetch_all(&mut *tx).await.map_err(db)?;
        let result = page.finish(&rows, |row| {
            decode(row).map(|record| current(record.invitation, now))
        })?;
        tx.commit().await.map_err(db)?;
        Ok(result)
    }

    /// A recipient can read an invitation before it has membership.
    pub async fn membership_invitation(
        &self,
        actor: &AuthenticatedActor,
        id: MembershipInvitationId,
    ) -> Result<MembershipInvitation, ControlError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let now = require_actor(&mut tx, actor).await?;
        let row =
            sqlx::query("SELECT * FROM control.membership_invitations WHERE invitation_id=$1")
                .bind(id.as_uuid())
                .fetch_optional(&mut *tx)
                .await
                .map_err(db)?
                .ok_or(if is_admin(actor) {
                    ControlError::InvitationNotFound
                } else {
                    ControlError::InvitationDenied
                })?;
        let record = decode(&row)?;
        if record.invitation.recipient_actor_id != actor.actor_id {
            govern(&mut tx, &record.invitation.scope, actor).await?;
        }
        tx.commit().await.map_err(db)?;
        Ok(current(record.invitation, now))
    }

    /// Commits acceptance, the current member fact, its event and replay result together.
    pub async fn accept_membership_invitation(
        &self,
        actor: &AuthenticatedActor,
        id: MembershipInvitationId,
        request: &AcceptMembershipInvitation,
        key: &IdempotencyKey,
    ) -> Result<MembershipInvitation, ControlError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let now = require_actor(&mut tx, actor).await?;
        let row = sqlx::query(
            "SELECT * FROM control.membership_invitations WHERE invitation_id=$1 FOR UPDATE",
        )
        .bind(id.as_uuid())
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?
        .ok_or(ControlError::InvitationDenied)?;
        let record = decode(&row)?;
        let mut invitation = record.invitation;
        if invitation.recipient_actor_id != actor.actor_id {
            return Err(ControlError::InvitationDenied);
        }
        if let Some(value) = reserve(
            &mut tx,
            ACCEPT,
            key,
            json!({"actor":actor.actor_id,"id":id,"request":request}),
        )
        .await?
        {
            return serde_json::from_value(value)
                .map_err(|_| ControlError::PersistenceIdentityMismatch);
        }
        if invitation.state != MembershipInvitationState::Pending {
            return Err(ControlError::InvitationStateConflict);
        }
        if invitation.expires_at <= now
            || invitation
                .membership_expires_at
                .is_some_and(|expiry| expiry <= now)
        {
            return Err(ControlError::InvitationExpired);
        }
        if invitation.revision != request.expected_revision {
            return Err(ControlError::RevisionConflict);
        }
        lock_scope(&mut tx, &invitation.scope, false).await?;
        if !account_active(&mut tx, invitation.created_by).await? {
            return Err(ControlError::InvitationDenied);
        }
        if let Some(expected) = record.issuer_revision
            && governor_revision(&mut tx, &invitation.scope, invitation.created_by).await?
                != Some(expected)
        {
            return Err(ControlError::InvitationDenied);
        }
        let current = member_revision(&mut tx, &invitation.scope, actor.actor_id).await?;
        if current != record.target_revision {
            return Err(ControlError::RevisionConflict);
        }
        let member_revision = next_revision(current)?;
        match &invitation.scope {
            MembershipInvitationScope::Project {
                project_id,
                object_role,
            } => {
                let project = super::project_from_tx(&mut tx, *project_id).await?;
                if project.owner_actor_id == actor.actor_id
                    || *object_role == ProjectObjectRole::Owner
                {
                    return Err(ControlError::OwnerMembershipProtected);
                }
                let role = if actor.roles.contains(&PlatformRole::Teacher) {
                    "teacher"
                } else if actor.roles.contains(&PlatformRole::Student) {
                    "student"
                } else {
                    return Err(ControlError::InvitationDenied);
                };
                let role = match role {
                    "teacher" => PlatformRole::Teacher,
                    _ => PlatformRole::Student,
                };
                let member = super::object_authorization::upsert_project_member(
                    &mut tx,
                    &project,
                    &contracts::http::ResolvedProjectMembershipRequest {
                        actor_id: actor.actor_id,
                        role,
                        object_role: *object_role,
                        expires_at: invitation.membership_expires_at,
                    },
                    now,
                )
                .await?;
                if member.revision != member_revision {
                    return Err(ControlError::PersistenceIdentityMismatch);
                }
            }
            MembershipInvitationScope::Course {
                course_id,
                object_role,
            } => {
                if *object_role == CourseObjectRole::Instructor
                    && !actor.roles.contains(&PlatformRole::Teacher)
                {
                    return Err(ControlError::InvitationDenied);
                }
                let role = if *object_role == CourseObjectRole::Instructor {
                    "teacher"
                } else {
                    "student"
                };
                sqlx::query("INSERT INTO control.course_memberships (course_id,actor_id,role,object_role,state,revision,expires_at,updated_at) VALUES ($1,$2,$3,$4,'active',$5,$6,$7) ON CONFLICT (course_id,actor_id) DO UPDATE SET role=EXCLUDED.role,object_role=EXCLUDED.object_role,state='active',revision=EXCLUDED.revision,expires_at=EXCLUDED.expires_at,updated_at=EXCLUDED.updated_at")
                    .bind(course_id.as_uuid()).bind(actor.actor_id.as_uuid()).bind(role).bind(object_role.as_str()).bind(i64_revision(member_revision)?).bind(invitation.membership_expires_at.map(UtcTimestamp::get)).bind(now.get()).execute(&mut *tx).await.map_err(db)?;
            }
        }
        event(&mut tx,&invitation.scope,actor.actor_id.as_uuid(),member_revision,json!({"actorId":actor.actor_id,"performedBy":actor.actor_id,"authorizedBy":invitation.created_by,"invitationId":id,"revision":member_revision,"state":MembershipState::Active,"scope":invitation.scope})).await?;
        invitation.state = MembershipInvitationState::Accepted;
        invitation.revision = next_revision(Some(invitation.revision))?;
        invitation.accepted_membership_revision = Some(member_revision);
        save(&mut tx, &invitation).await?;
        complete(&mut tx, ACCEPT, key, &invitation).await?;
        tx.commit().await.map_err(db)?;
        Ok(invitation)
    }

    /// Cancels a pending invitation. Accepted membership uses the member revocation API.
    pub async fn revoke_membership_invitation(
        &self,
        actor: &AuthenticatedActor,
        id: MembershipInvitationId,
        request: &RemoveProjectMembershipRequest,
        key: &IdempotencyKey,
    ) -> Result<MembershipInvitation, ControlError> {
        if request.reason.trim().is_empty() {
            return Err(ControlError::ContractInvalid);
        }
        let mut tx = self.pool.begin().await.map_err(db)?;
        require_actor(&mut tx, actor).await?;
        let row = sqlx::query(
            "SELECT * FROM control.membership_invitations WHERE invitation_id=$1 FOR UPDATE",
        )
        .bind(id.as_uuid())
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?
        .ok_or(ControlError::InvitationDenied)?;
        let mut invitation = decode(&row)?.invitation;
        lock_scope(&mut tx, &invitation.scope, true).await?;
        govern(&mut tx, &invitation.scope, actor).await?;
        if let Some(value) = reserve(
            &mut tx,
            REVOKE,
            key,
            json!({"actor":actor.actor_id,"id":id,"request":request}),
        )
        .await?
        {
            return serde_json::from_value(value)
                .map_err(|_| ControlError::PersistenceIdentityMismatch);
        }
        if invitation.state != MembershipInvitationState::Pending {
            return Err(ControlError::InvitationStateConflict);
        }
        if invitation.revision != request.expected_revision {
            return Err(ControlError::RevisionConflict);
        }
        invitation.state = MembershipInvitationState::Revoked;
        invitation.revision = next_revision(Some(invitation.revision))?;
        save(&mut tx, &invitation).await?;
        complete(&mut tx, REVOKE, key, &invitation).await?;
        tx.commit().await.map_err(db)?;
        Ok(invitation)
    }

    /// Returns safe account labels to instructors and teaching assistants only.
    pub async fn course_memberships(
        &self,
        actor: &AuthenticatedActor,
        course_id: CourseId,
        query: &ControlListQuery,
    ) -> Result<CourseMembershipPage, ControlError> {
        let page = ListPage::new(
            query,
            &("course-members", course_id, actor.actor_id),
            self.cursor_signing_keys.as_deref(),
        )?;
        let mut tx = self.pool.begin().await.map_err(db)?;
        require_actor(&mut tx, actor).await?;
        if is_admin(actor) {
            let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM control.course_memberships WHERE course_id=$1) OR EXISTS(SELECT 1 FROM control.projects WHERE course_id=$1)")
                .bind(course_id.as_uuid()).fetch_one(&mut *tx).await.map_err(db)?;
            if !exists {
                return Err(ControlError::NotFound);
            }
        }
        if !is_admin(actor) {
            let staff: bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM control.course_memberships WHERE course_id=$1 AND actor_id=$2 AND object_role IN ('instructor','teaching_assistant') AND state='active' AND (expires_at IS NULL OR expires_at>clock_timestamp()))").bind(course_id.as_uuid()).bind(actor.actor_id.as_uuid()).fetch_one(&mut *tx).await.map_err(db)?;
            if !staff {
                return Err(ControlError::CourseGovernanceDenied);
            }
        }
        let rows=sqlx::query("SELECT cm.*,cm.actor_id AS cursor_id,a.username,a.display_name FROM control.course_memberships cm JOIN access.actors a ON a.actor_id=cm.actor_id WHERE cm.course_id=$1 AND ($3::uuid IS NULL OR cm.actor_id>$3) ORDER BY cm.actor_id LIMIT $2").bind(course_id.as_uuid()).bind(i64::from(page.limit)+1).bind(page.after).fetch_all(&mut *tx).await.map_err(db)?;
        let result = page.finish(&rows, |row| {
            Ok(CourseMembershipView {
                membership: auth::repository::course_membership_from_row(row)
                    .map_err(|_| ControlError::PersistenceIdentityMismatch)?,
                username: row.try_get("username").map_err(db)?,
                display_name: row.try_get("display_name").map_err(db)?,
            })
        })?;
        tx.commit().await.map_err(db)?;
        Ok(result)
    }

    /// Revokes a current member with an exact revision and protects the last live instructor.
    pub async fn remove_course_membership(
        &self,
        actor: &AuthenticatedActor,
        course_id: CourseId,
        target: ActorId,
        request: &RemoveProjectMembershipRequest,
        key: &IdempotencyKey,
    ) -> Result<contracts::CourseMembership, ControlError> {
        if request.reason.trim().is_empty() {
            return Err(ControlError::ContractInvalid);
        }
        let scope = MembershipInvitationScope::Course {
            course_id,
            object_role: CourseObjectRole::Participant,
        };
        let mut tx = self.pool.begin().await.map_err(db)?;
        let now = require_actor(&mut tx, actor).await?;
        lock_scope(&mut tx, &scope, true).await?;
        govern(&mut tx, &scope, actor).await?;
        if let Some(value) = reserve(
            &mut tx,
            REMOVE,
            key,
            json!({"actor":actor.actor_id,"course":course_id,"target":target,"request":request}),
        )
        .await?
        {
            return serde_json::from_value(value)
                .map_err(|_| ControlError::PersistenceIdentityMismatch);
        }
        let row=sqlx::query("SELECT * FROM control.course_memberships WHERE course_id=$1 AND actor_id=$2 AND state='active' FOR UPDATE").bind(course_id.as_uuid()).bind(target.as_uuid()).fetch_optional(&mut *tx).await.map_err(db)?.ok_or(ControlError::MembershipNotFound)?;
        let mut membership = auth::repository::course_membership_from_row(&row)
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        if membership.revision != request.expected_revision {
            return Err(ControlError::RevisionConflict);
        }
        if membership.object_role == CourseObjectRole::Instructor {
            let remaining: i64=sqlx::query_scalar("SELECT count(*) FROM control.course_memberships WHERE course_id=$1 AND actor_id<>$2 AND object_role='instructor' AND state='active' AND (expires_at IS NULL OR expires_at>clock_timestamp())").bind(course_id.as_uuid()).bind(target.as_uuid()).fetch_one(&mut *tx).await.map_err(db)?;
            if remaining == 0 {
                return Err(ControlError::CourseInstructorProtected);
            }
        }
        membership.state = MembershipState::Revoked;
        membership.revision = next_revision(Some(membership.revision))?;
        sqlx::query("UPDATE control.course_memberships SET state='revoked',revision=$3,updated_at=$4 WHERE course_id=$1 AND actor_id=$2").bind(course_id.as_uuid()).bind(target.as_uuid()).bind(i64_revision(membership.revision)?).bind(now.get()).execute(&mut *tx).await.map_err(db)?;
        event(&mut tx,&scope,target.as_uuid(),membership.revision,json!({"courseId":course_id,"actorId":target,"performedBy":actor.actor_id,"revision":membership.revision,"state":membership.state,"objectRole":membership.object_role})).await?;
        complete(&mut tx, REMOVE, key, &membership).await?;
        tx.commit().await.map_err(db)?;
        Ok(membership)
    }
}
impl ControlService {
    /// Reads the authoritative course calendar for an authenticated member.
    pub async fn course_calendar(
        &self,
        _actor: &AuthenticatedActor,
        course_id: CourseId,
    ) -> Result<CourseCalendar, ControlError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let now = database_now(&mut tx).await?;
        let row = sqlx::query(
            "SELECT course_id,revision,contract FROM control.course_calendars WHERE course_id=$1",
        )
        .bind(course_id.as_uuid())
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?
        .ok_or(ControlError::CourseCalendarNotFound)?;
        let mut calendar: CourseCalendar =
            serde_json::from_value(row.try_get("contract").map_err(db)?)
                .map_err(|_| ControlError::CourseCalendarInvalid)?;
        if calendar.course_id != course_id
            || i64_revision(calendar.revision)? != row.try_get::<i64, _>("revision").map_err(db)?
        {
            return Err(ControlError::PersistenceIdentityMismatch);
        }
        calendar.state = calendar.state_at(now);
        tx.commit().await.map_err(db)?;
        Ok(calendar)
    }

    /// Replaces calendar facts under instructor governance and revision fencing.
    pub async fn update_course_calendar(
        &self,
        actor: &AuthenticatedActor,
        course_id: CourseId,
        request: &UpdateCourseCalendar,
        expected_revision: Option<Revision>,
    ) -> Result<CourseCalendar, ControlError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let now = require_actor(&mut tx, actor).await?;
        let scope = MembershipInvitationScope::Course {
            course_id,
            object_role: CourseObjectRole::Participant,
        };
        lock_scope(&mut tx, &scope, true).await?;
        govern(&mut tx, &scope, actor).await?;
        if request.time_zone.trim().is_empty()
            || request.time_zone.chars().any(char::is_control)
            || request.open_at.is_some_and(|open| open >= request.due_at)
            || (request.late_policy == CourseLatePolicy::HardClose && request.late_until.is_some())
            || request
                .late_until
                .is_some_and(|until| until <= request.due_at)
        {
            return Err(ControlError::CourseCalendarInvalid);
        }
        let zone_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_timezone_names WHERE name=$1)",
        )
        .bind(&request.time_zone)
        .fetch_one(&mut *tx)
        .await
        .map_err(db)?;
        if !zone_exists {
            return Err(ControlError::CourseCalendarInvalid);
        }
        let current: Option<i64> = sqlx::query_scalar(
            "SELECT revision FROM control.course_calendars WHERE course_id=$1 FOR UPDATE",
        )
        .bind(course_id.as_uuid())
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        let revision = if let Some(value) = current {
            let current = revision(value)?;
            if expected_revision != Some(current) {
                return Err(ControlError::CourseCalendarConflict);
            }
            Revision::new(
                current
                    .get()
                    .checked_add(1)
                    .ok_or(ControlError::CourseCalendarInvalid)?,
            )
            .map_err(|_| ControlError::CourseCalendarInvalid)?
        } else {
            if expected_revision.is_some() {
                return Err(ControlError::CourseCalendarConflict);
            }
            Revision::new(1).map_err(|_| ControlError::CourseCalendarInvalid)?
        };
        let mut calendar = CourseCalendar {
            course_id,
            revision,
            open_at: request.open_at,
            due_at: request.due_at,
            time_zone: request.time_zone.clone(),
            state: CourseCalendarState::Open,
            late_policy: request.late_policy,
            late_until: request.late_until,
        };
        calendar.state = calendar.state_at(now);
        let value =
            serde_json::to_value(&calendar).map_err(|_| ControlError::CourseCalendarInvalid)?;
        sqlx::query("INSERT INTO control.course_calendars(course_id,revision,open_at,due_at,time_zone,late_policy,late_until,contract,created_at,updated_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$9) ON CONFLICT(course_id) DO UPDATE SET revision=EXCLUDED.revision,open_at=EXCLUDED.open_at,due_at=EXCLUDED.due_at,time_zone=EXCLUDED.time_zone,late_policy=EXCLUDED.late_policy,late_until=EXCLUDED.late_until,contract=EXCLUDED.contract,updated_at=EXCLUDED.updated_at")
            .bind(course_id.as_uuid()).bind(i64_revision(revision)?).bind(request.open_at.map(UtcTimestamp::get)).bind(request.due_at.get()).bind(&request.time_zone).bind(request.late_policy.as_str()).bind(request.late_until.map(UtcTimestamp::get)).bind(value).bind(now.get()).execute(&mut *tx).await.map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(calendar)
    }

    /// Applies resolved directory rows idempotently to the course roster.
    pub async fn import_course_roster(
        &self,
        actor: &AuthenticatedActor,
        course_id: CourseId,
        request: &ResolvedCourseRosterImportRequest,
        key: &IdempotencyKey,
    ) -> Result<CourseRosterImportResult, ControlError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let now = require_actor(&mut tx, actor).await?;
        let scope = MembershipInvitationScope::Course {
            course_id,
            object_role: CourseObjectRole::Participant,
        };
        lock_scope(&mut tx, &scope, true).await?;
        govern(&mut tx, &scope, actor)
            .await
            .map_err(|_| ControlError::CourseRosterGovernanceDenied)?;
        if let Some(value) = reserve(
            &mut tx,
            "control_import_course_roster_v1",
            key,
            json!({"actor":actor.actor_id,"courseId":course_id,"request":request}),
        )
        .await?
        {
            return serde_json::from_value(value)
                .map_err(|_| ControlError::PersistenceIdentityMismatch);
        }
        let mut results = Vec::with_capacity(request.rows.len());
        let mut seen = std::collections::BTreeSet::new();
        let mut max_revision = 1_u64;
        for row in &request.rows {
            let duplicate = !seen.insert(row.actor_id);
            let current = sqlx::query("SELECT revision,state,expires_at FROM control.course_memberships WHERE course_id=$1 AND actor_id=$2 FOR UPDATE")
                .bind(course_id.as_uuid()).bind(row.actor_id.as_uuid())
                .fetch_optional(&mut *tx).await.map_err(db)?;
            let active = if let Some(member) = &current {
                member.try_get::<&str, _>("state").map_err(db)? == "active"
                    && member
                        .try_get::<Option<time::OffsetDateTime>, _>("expires_at")
                        .map_err(db)?
                        .is_none_or(|expiry| expiry > now.get())
            } else {
                false
            };
            let result = if duplicate {
                CourseRosterImportRowState::DuplicateInFile
            } else if active {
                CourseRosterImportRowState::AlreadyMember
            } else {
                CourseRosterImportRowState::Imported
            };
            let member_revision = if let Some(member) = &current {
                let existing = revision(member.try_get("revision").map_err(db)?)?;
                if result == CourseRosterImportRowState::Imported {
                    next_revision(Some(existing))?
                } else {
                    existing
                }
            } else {
                Revision::new(1).map_err(|_| ControlError::CourseRosterImportInvalid)?
            };
            max_revision = max_revision.max(member_revision.get());
            if result == CourseRosterImportRowState::Imported {
                let object_role = match row.role {
                    CourseRosterRole::Participant => CourseObjectRole::Participant,
                    CourseRosterRole::TeachingAssistant => CourseObjectRole::TeachingAssistant,
                };
                sqlx::query("INSERT INTO control.course_memberships(course_id,actor_id,role,object_role,state,revision,updated_at) VALUES($1,$2,'student',$3,'active',$4,$5) ON CONFLICT(course_id,actor_id) DO UPDATE SET role='student',object_role=EXCLUDED.object_role,state='active',revision=EXCLUDED.revision,expires_at=NULL,updated_at=EXCLUDED.updated_at")
                    .bind(course_id.as_uuid()).bind(row.actor_id.as_uuid()).bind(object_role.as_str())
                    .bind(i64_revision(member_revision)?).bind(now.get()).execute(&mut *tx).await.map_err(db)?;
                event(&mut tx,&MembershipInvitationScope::Course {course_id,object_role},row.actor_id.as_uuid(),member_revision,
                    json!({"courseId":course_id,"actorId":row.actor_id,"performedBy":actor.actor_id,"revision":member_revision,"state":MembershipState::Active,"objectRole":object_role})).await?;
            }
            results.push(CourseRosterImportRowResult {
                row_number: row.row_number,
                campus_username: row.campus_username.clone(),
                role: Some(row.role),
                result,
                actor_id: Some(row.actor_id),
                invitation_id: None,
                diagnostic_code: None,
            });
        }
        let output = CourseRosterImportResult {
            course_id,
            revision: Revision::new(max_revision)
                .map_err(|_| ControlError::CourseRosterImportInvalid)?,
            rows: results,
        };
        complete(&mut tx, "control_import_course_roster_v1", key, &output).await?;
        tx.commit().await.map_err(db)?;
        Ok(output)
    }
}
impl ControlService {
    /// Reads only the authenticated actor's active course membership.
    pub async fn course_membership_for_actor(
        &self,
        actor: &AuthenticatedActor,
        course_id: CourseId,
    ) -> Result<contracts::CourseMembership, ControlError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        require_actor(&mut tx, actor).await?;
        let row = sqlx::query("SELECT course_id,actor_id,role,object_role,state,revision,expires_at FROM control.course_memberships WHERE course_id=$1 AND actor_id=$2 AND state='active' AND (expires_at IS NULL OR expires_at>clock_timestamp())")
            .bind(course_id.as_uuid()).bind(actor.actor_id.as_uuid()).fetch_optional(&mut *tx).await.map_err(db)?
            .ok_or(ControlError::CourseGovernanceDenied)?;
        let membership = auth::repository::course_membership_from_row(&row)
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        tx.commit().await.map_err(db)?;
        Ok(membership)
    }
}
