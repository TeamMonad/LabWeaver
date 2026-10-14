//! Real member facts and invitation transactions with synthetic verified identities.
use super::{FixtureObjects, apply_domain_migrations, control_config};
use contracts::http::{
    IdempotencyKey, PageQuery, RemoveProjectMembershipRequest, ResolvedProjectMembershipRequest,
};
use contracts::membership_governance::{
    AcceptMembershipInvitation, CourseObjectRole, MembershipInvitationScope,
    MembershipInvitationState, ResolvedMembershipInvitation,
};
use contracts::object_authorization::{ObjectAction, ProjectObjectRole};
use contracts::{
    ActorId, AuthenticatedActor, CourseId, PlatformRole, ProjectId, Revision, UtcTimestamp,
};
use control_service::{ControlError, ControlService};
use persistence_sqlx::{Domain, Sha256Digest};
use sqlx::{PgPool, postgres::PgPoolOptions};
use std::sync::Arc;
use testcontainers::{ContainerAsync, ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

async fn fixture() -> TestResult<(
    ContainerAsync<Postgres>,
    PgPool,
    ControlService,
    UtcTimestamp,
)> {
    let container = Postgres::default().with_tag("17.6-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            container.get_host_port_ipv4(5432).await?
        ))
        .await?;
    apply_domain_migrations(&pool, Domain::Control).await?;
    sqlx::query("CREATE ROLE lw_control_runtime NOLOGIN")
        .execute(&pool)
        .await?;
    apply_domain_migrations(&pool, Domain::Access).await?;
    let service = ControlService::new(
        pool.clone(),
        Arc::new(FixtureObjects { fail_second: false }),
        control_config()?,
    )?;
    let now = UtcTimestamp::from_utc(
        sqlx::query_scalar("SELECT date_trunc('milliseconds',clock_timestamp())")
            .fetch_one(&pool)
            .await?,
    )?;
    Ok((container, pool, service, now))
}

async fn actor(
    pool: &PgPool,
    name: &str,
    roles: Vec<PlatformRole>,
    now: UtcTimestamp,
) -> TestResult<AuthenticatedActor> {
    let id = ActorId::new();
    sqlx::query("INSERT INTO access.actors (actor_id,issuer,subject_sha256,username,display_name) VALUES ($1,'https://synthetic.invalid',$2,$3,$3)").bind(id.as_uuid()).bind(Sha256Digest::of_bytes(id.as_uuid().as_bytes()).to_string()).bind(name).execute(pool).await?;
    Ok(AuthenticatedActor {
        actor_id: id,
        roles,
        expires_at: UtcTimestamp::from_utc(now.get() + time::Duration::hours(1))?,
    })
}

async fn instructor(pool: &PgPool, course: CourseId, actor: &AuthenticatedActor) -> TestResult {
    sqlx::query("INSERT INTO control.course_memberships (course_id,actor_id,role,object_role,state,revision) VALUES ($1,$2,'teacher','instructor','active',1)").bind(course.as_uuid()).bind(actor.actor_id.as_uuid()).execute(pool).await?;
    Ok(())
}

fn invitation(
    scope: MembershipInvitationScope,
    actor: &AuthenticatedActor,
    now: UtcTimestamp,
) -> TestResult<ResolvedMembershipInvitation> {
    Ok(ResolvedMembershipInvitation {
        recipient_actor_id: actor.actor_id,
        scope,
        expires_at: UtcTimestamp::from_utc(now.get() + time::Duration::minutes(10))?,
        membership_expires_at: None,
    })
}

async fn project(
    service: &ControlService,
    owner: &AuthenticatedActor,
    now: UtcTimestamp,
    key: &str,
) -> TestResult<ProjectId> {
    Ok(service
        .create_project(
            owner.actor_id,
            PlatformRole::Student,
            &contracts::CreateProjectRequest {
                name: key.to_owned(),
                description: None,
                course_id: None,
            },
            &IdempotencyKey::parse(key)?,
            now,
        )
        .await?
        .id)
}

fn removal(revision: Revision) -> RemoveProjectMembershipRequest {
    RemoveProjectMembershipRequest {
        expected_revision: revision,
        reason: "scope revoked".to_owned(),
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn course_assistant_acceptance_is_atomic_scoped_and_revocable() -> TestResult {
    let (_container, pool, service, now) = fixture().await?;
    let teacher = actor(&pool, "instructor", vec![PlatformRole::Teacher], now).await?;
    let assistant = actor(&pool, "assistant", vec![PlatformRole::Student], now).await?;
    let participant = actor(&pool, "participant", vec![PlatformRole::Student], now).await?;
    let course = CourseId::new();
    instructor(&pool, course, &teacher).await?;
    sqlx::query("INSERT INTO control.course_memberships (course_id,actor_id,role,object_role,state,revision) VALUES ($1,$2,'student','participant','active',1)").bind(course.as_uuid()).bind(participant.actor_id.as_uuid()).execute(&pool).await?;
    let invite = service
        .create_membership_invitation(
            &teacher,
            &invitation(
                MembershipInvitationScope::Course {
                    course_id: course,
                    object_role: CourseObjectRole::TeachingAssistant,
                },
                &assistant,
                now,
            )?,
            &IdempotencyKey::parse("create-assistant")?,
        )
        .await?;
    assert!(matches!(
        service
            .accept_membership_invitation(
                &participant,
                invite.id,
                &AcceptMembershipInvitation {
                    expected_revision: invite.revision
                },
                &IdempotencyKey::parse("wrong-recipient")?
            )
            .await,
        Err(ControlError::InvitationDenied)
    ));
    let mut accepts = Vec::new();
    for _ in 0..8 {
        let service = service.clone();
        let actor = assistant.clone();
        accepts.push(tokio::spawn(async move {
            service
                .accept_membership_invitation(
                    &actor,
                    invite.id,
                    &AcceptMembershipInvitation {
                        expected_revision: Revision::new(1)?,
                    },
                    &IdempotencyKey::parse("accept-assistant")?,
                )
                .await
                .map_err(Into::<Box<dyn std::error::Error + Send + Sync>>::into)
        }));
    }
    for accept in accepts {
        let value = accept
            .await?
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        assert_eq!(value.state, MembershipInvitationState::Accepted);
        assert_eq!(value.accepted_membership_revision, Some(Revision::new(1)?));
    }
    let members = auth::load_membership_snapshot(&pool, assistant.actor_id.as_uuid()).await?;
    assert_eq!(members.course_memberships.len(), 1);
    assert_eq!(
        members.course_memberships[0].object_role,
        CourseObjectRole::TeachingAssistant
    );
    assert_eq!(members.course_memberships[0].role, PlatformRole::Student);
    let context = auth::AuthorizationContext {
        actor: assistant.clone(),
        course_memberships: members.course_memberships.clone(),
        project_memberships: members.project_memberships,
        now: now.get(),
    };
    assert!(
        auth::object_authorization::authorize_course_governance_action(
            &context,
            course,
            ObjectAction::Browse
        )
        .is_ok()
    );
    assert!(
        auth::object_authorization::authorize_course_governance_action(
            &context,
            course,
            ObjectAction::ManageMembers
        )
        .is_err()
    );
    assert!(
        auth::object_authorization::authorize_course_governance_action(
            &context,
            CourseId::new(),
            ObjectAction::Browse
        )
        .is_err()
    );
    let mut stacked = context.clone();
    stacked.actor.roles.push(PlatformRole::Teacher);
    assert!(
        auth::object_authorization::authorize_course_governance_action(
            &stacked,
            course,
            ObjectAction::ManageMembers
        )
        .is_err()
    );
    let page = service
        .course_memberships(
            &assistant,
            course,
            &PageQuery {
                page: Some(1),
                page_size: Some(2),
            },
        )
        .await?;
    assert_eq!(page.items.len(), 2);
    assert!(page.has_more);
    assert!(matches!(
        service
            .course_memberships(&participant, course, &PageQuery::default())
            .await,
        Err(ControlError::CourseGovernanceDenied)
    ));
    let events: i64=sqlx::query_scalar("SELECT count(*) FROM control.sse_events WHERE course_id=$1 AND event_type='member_authorization.changed.v1' AND aggregate_id=$2").bind(course.as_uuid()).bind(assistant.actor_id.as_uuid()).fetch_one(&pool).await?;
    assert_eq!(events, 1, "concurrent accepts must append one member event");
    let payload: serde_json::Value = sqlx::query_scalar(
        "SELECT payload FROM control.sse_events WHERE course_id=$1 AND aggregate_id=$2",
    )
    .bind(course.as_uuid())
    .bind(assistant.actor_id.as_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        payload["performedBy"],
        serde_json::json!(assistant.actor_id)
    );
    assert_eq!(payload["authorizedBy"], serde_json::json!(teacher.actor_id));
    let removed = service
        .remove_course_membership(
            &teacher,
            course,
            assistant.actor_id,
            &removal(Revision::new(1)?),
            &IdempotencyKey::parse("remove-assistant")?,
        )
        .await?;
    assert_eq!(removed.state, contracts::MembershipState::Revoked);
    assert!(matches!(
        service
            .course_memberships(&assistant, course, &PageQuery::default())
            .await,
        Err(ControlError::CourseGovernanceDenied)
    ));
    // Transport replay returns history, and cannot grant the removed scope again.
    service
        .accept_membership_invitation(
            &assistant,
            invite.id,
            &AcceptMembershipInvitation {
                expected_revision: invite.revision,
            },
            &IdempotencyKey::parse("accept-assistant")?,
        )
        .await?;
    assert_eq!(
        auth::load_membership_snapshot(&pool, assistant.actor_id.as_uuid())
            .await?
            .course_memberships[0]
            .state,
        contracts::MembershipState::Revoked
    );
    assert!(matches!(
        service
            .remove_course_membership(
                &teacher,
                course,
                teacher.actor_id,
                &removal(Revision::new(1)?),
                &IdempotencyKey::parse("last-instructor")?
            )
            .await,
        Err(ControlError::CourseInstructorProtected)
    ));
    let instructor_invite = service
        .create_membership_invitation(
            &teacher,
            &invitation(
                MembershipInvitationScope::Course {
                    course_id: course,
                    object_role: CourseObjectRole::Instructor,
                },
                &participant,
                now,
            )?,
            &IdempotencyKey::parse("student-not-instructor")?,
        )
        .await?;
    assert!(matches!(
        service
            .accept_membership_invitation(
                &participant,
                instructor_invite.id,
                &AcceptMembershipInvitation {
                    expected_revision: instructor_invite.revision
                },
                &IdempotencyKey::parse("no-platform-promotion")?
            )
            .await,
        Err(ControlError::InvitationDenied)
    ));
    Ok(())
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn pending_project_invitation_cannot_outlive_issuer_or_target_revision() -> TestResult {
    let (_container, pool, service, now) = fixture().await?;
    let owner = actor(&pool, "owner", vec![PlatformRole::Student], now).await?;
    let manager = actor(&pool, "manager", vec![PlatformRole::Student], now).await?;
    let recipient = actor(&pool, "recipient", vec![PlatformRole::Student], now).await?;
    let project_id = project(&service, &owner, now, "invitation-project").await?;
    let request = ResolvedProjectMembershipRequest {
        actor_id: manager.actor_id,
        role: PlatformRole::Student,
        object_role: ProjectObjectRole::Manager,
        expires_at: None,
    };
    let member = service
        .add_project_membership(
            project_id,
            owner.actor_id,
            false,
            &request,
            &IdempotencyKey::parse("add-manager")?,
            now,
        )
        .await?;
    let invite_request = invitation(
        MembershipInvitationScope::Project {
            project_id,
            object_role: ProjectObjectRole::Editor,
        },
        &recipient,
        now,
    )?;
    let pending = service
        .create_membership_invitation(
            &manager,
            &invite_request,
            &IdempotencyKey::parse("manager-invites")?,
        )
        .await?;
    assert!(
        service
            .membership_invitation(&recipient, pending.id)
            .await
            .is_ok()
    );
    assert_eq!(
        service
            .scoped_membership_invitations(&manager, &invite_request.scope, &PageQuery::default())
            .await?
            .items
            .len(),
        1
    );
    assert!(matches!(
        service
            .scoped_membership_invitations(&recipient, &invite_request.scope, &PageQuery::default())
            .await,
        Err(ControlError::InvitationDenied)
    ));
    service
        .remove_project_membership(
            project_id,
            manager.actor_id,
            owner.actor_id,
            false,
            &removal(member.revision),
            &IdempotencyKey::parse("remove-manager")?,
            now,
        )
        .await?;
    assert!(matches!(
        service
            .accept_membership_invitation(
                &recipient,
                pending.id,
                &AcceptMembershipInvitation {
                    expected_revision: pending.revision
                },
                &IdempotencyKey::parse("removed-issuer")?
            )
            .await,
        Err(ControlError::InvitationDenied)
    ));
    service
        .add_project_membership(
            project_id,
            owner.actor_id,
            false,
            &request,
            &IdempotencyKey::parse("rejoin-manager")?,
            now,
        )
        .await?;
    assert!(matches!(
        service
            .accept_membership_invitation(
                &recipient,
                pending.id,
                &AcceptMembershipInvitation {
                    expected_revision: pending.revision
                },
                &IdempotencyKey::parse("rejoined-issuer")?
            )
            .await,
        Err(ControlError::InvitationDenied)
    ));
    let fresh = service
        .create_membership_invitation(
            &owner,
            &invite_request,
            &IdempotencyKey::parse("owner-invites")?,
        )
        .await?;
    service
        .add_project_membership(
            project_id,
            owner.actor_id,
            false,
            &ResolvedProjectMembershipRequest {
                actor_id: recipient.actor_id,
                role: PlatformRole::Student,
                object_role: ProjectObjectRole::Viewer,
                expires_at: None,
            },
            &IdempotencyKey::parse("target-changes")?,
            now,
        )
        .await?;
    assert!(matches!(
        service
            .accept_membership_invitation(
                &recipient,
                fresh.id,
                &AcceptMembershipInvitation {
                    expected_revision: fresh.revision
                },
                &IdempotencyKey::parse("stale-target")?
            )
            .await,
        Err(ControlError::RevisionConflict)
    ));
    assert!(matches!(
        service
            .create_membership_invitation(
                &manager,
                &invitation(
                    MembershipInvitationScope::Project {
                        project_id,
                        object_role: ProjectObjectRole::Owner
                    },
                    &recipient,
                    now
                )?,
                &IdempotencyKey::parse("no-owner-promotion")?
            )
            .await,
        Err(ControlError::OwnerMembershipProtected)
    ));
    let snapshot = auth::load_membership_snapshot(&pool, recipient.actor_id.as_uuid()).await?;
    assert_eq!(
        snapshot.project_memberships[0].object_role,
        ProjectObjectRole::Viewer
    );
    Ok(())
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn invitation_expiry_cancel_and_account_disable_deny_acceptance() -> TestResult {
    let (_container, pool, service, now) = fixture().await?;
    let owner = actor(&pool, "owner", vec![PlatformRole::Student], now).await?;
    let recipient = actor(&pool, "recipient", vec![PlatformRole::Student], now).await?;
    let outsider = actor(&pool, "outsider", vec![PlatformRole::Student], now).await?;
    let admin = actor(&pool, "admin", vec![PlatformRole::PlatformAdmin], now).await?;
    let project_id = project(&service, &owner, now, "deadline-project").await?;
    let request = invitation(
        MembershipInvitationScope::Project {
            project_id,
            object_role: ProjectObjectRole::Editor,
        },
        &recipient,
        now,
    )?;
    let cancel = service
        .create_membership_invitation(&owner, &request, &IdempotencyKey::parse("cancel-create")?)
        .await?;
    assert!(matches!(
        service.membership_invitation(&outsider, cancel.id).await,
        Err(ControlError::InvitationDenied)
    ));
    let canceled = service
        .revoke_membership_invitation(
            &owner,
            cancel.id,
            &removal(cancel.revision),
            &IdempotencyKey::parse("cancel-invitation")?,
        )
        .await?;
    assert_eq!(canceled.state, MembershipInvitationState::Revoked);
    assert!(matches!(
        service
            .accept_membership_invitation(
                &recipient,
                cancel.id,
                &AcceptMembershipInvitation {
                    expected_revision: cancel.revision
                },
                &IdempotencyKey::parse("cancelled-accept")?
            )
            .await,
        Err(ControlError::InvitationStateConflict)
    ));
    let mut short = request.clone();
    let fresh_now = UtcTimestamp::from_utc(
        sqlx::query_scalar("SELECT date_trunc('milliseconds',clock_timestamp())")
            .fetch_one(&pool)
            .await?,
    )?;
    short.expires_at = UtcTimestamp::from_utc(fresh_now.get() + time::Duration::seconds(1))?;
    let expiring = service
        .create_membership_invitation(&owner, &short, &IdempotencyKey::parse("expiry-create")?)
        .await?;
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    assert_eq!(
        service
            .membership_invitation(&recipient, expiring.id)
            .await?
            .state,
        MembershipInvitationState::Expired
    );
    assert!(matches!(
        service
            .accept_membership_invitation(
                &recipient,
                expiring.id,
                &AcceptMembershipInvitation {
                    expected_revision: expiring.revision
                },
                &IdempotencyKey::parse("expired-accept")?
            )
            .await,
        Err(ControlError::InvitationExpired)
    ));
    let disabled = service
        .create_membership_invitation(&admin, &request, &IdempotencyKey::parse("admin-invites")?)
        .await?;
    sqlx::query("UPDATE access.actors SET disabled_at=clock_timestamp() WHERE actor_id=$1")
        .bind(admin.actor_id.as_uuid())
        .execute(&pool)
        .await?;
    assert!(matches!(
        service
            .accept_membership_invitation(
                &recipient,
                disabled.id,
                &AcceptMembershipInvitation {
                    expected_revision: disabled.revision
                },
                &IdempotencyKey::parse("disabled-issuer")?
            )
            .await,
        Err(ControlError::InvitationDenied)
    ));
    assert!(
        auth::load_membership_snapshot(&pool, recipient.actor_id.as_uuid())
            .await?
            .project_memberships
            .is_empty()
    );
    assert!(
        service
            .incoming_membership_invitations(&outsider, &PageQuery::default())
            .await?
            .items
            .is_empty()
    );
    Ok(())
}
