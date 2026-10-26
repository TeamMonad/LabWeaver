//! Real member facts and invitation transactions with synthetic verified identities.
use super::{FixtureObjects, apply_domain_migrations, control_config};
use base64::Engine as _;
use contracts::http::{
    IdempotencyKey, RemoveProjectMembershipRequest, ResolvedProjectMembershipRequest,
};
use contracts::membership_governance::{
    AcceptMembershipInvitation, ControlListQuery, CourseObjectRole, MembershipInvitationScope,
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

#[path = "../support/production_roles.rs"]
mod production_roles;

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn restricted_control_runtime_revokes_project_and_course_memberships() -> TestResult {
    let roles = production_roles::ProductionRoles::start().await?;
    let provisioner = roles.provisioner.clone();
    let control_runtime = roles.control.clone();
    let access_runtime = roles.access.clone();
    let service = ControlService::new(
        control_runtime.clone(),
        Arc::new(FixtureObjects { fail_second: false }),
        control_config()?,
    )?;
    let now = UtcTimestamp::from_utc(
        sqlx::query_scalar("SELECT date_trunc('milliseconds',clock_timestamp())")
            .fetch_one(&provisioner)
            .await?,
    )?;
    let owner = actor(
        &provisioner,
        "restricted-owner",
        vec![PlatformRole::Student],
        now,
    )
    .await?;
    let project_member = actor(
        &provisioner,
        "restricted-project-member",
        vec![PlatformRole::Student],
        now,
    )
    .await?;
    let project_id = project(&service, &owner, now, "restricted-runtime-project").await?;
    let membership = service
        .add_project_membership(
            project_id,
            owner.actor_id,
            false,
            &ResolvedProjectMembershipRequest {
                actor_id: project_member.actor_id,
                role: PlatformRole::Student,
                object_role: ProjectObjectRole::Editor,
                expires_at: None,
            },
            &IdempotencyKey::parse("restricted-runtime-add-project-member")?,
            now,
        )
        .await?;
    // The original unqualified lock must remain denied under Control's read-only grant on access.actors.
    let unsafe_project_lock = sqlx::query(
        "SELECT pm.course_id,pm.project_id,pm.actor_id,pm.role,pm.object_role,pm.state,pm.revision,pm.expires_at, \
                a.username,a.display_name \
         FROM control.project_memberships pm \
         JOIN access.actors a ON a.actor_id=pm.actor_id \
         WHERE pm.project_id=$1 AND pm.actor_id=$2 AND pm.state='active' FOR UPDATE",
    )
    .bind(project_id.as_uuid())
    .bind(project_member.actor_id.as_uuid())
    .fetch_optional(&control_runtime)
    .await;
    let unsafe_lock_error = unsafe_project_lock.err().ok_or_else(|| {
        std::io::Error::other("Control runtime unexpectedly acquired an Access actor row lock")
    })?;
    let database_message = unsafe_lock_error
        .as_database_error()
        .map_or_else(String::new, |error| error.message().to_owned());
    assert!(
        database_message.contains("permission denied for table actors"),
        "expected the existing read-only Access grant to reject the joined row lock: {database_message}\n{}",
        roles.database_errors().await?
    );
    let project_revoke = service
        .remove_project_membership(
            project_id,
            project_member.actor_id,
            owner.actor_id,
            false,
            &removal(membership.revision),
            &IdempotencyKey::parse("restricted-runtime-remove-project-member")?,
            now,
        )
        .await;

    let teacher = actor(
        &provisioner,
        "restricted-course-teacher",
        vec![PlatformRole::Teacher],
        now,
    )
    .await?;
    let course_member = actor(
        &provisioner,
        "restricted-course-member",
        vec![PlatformRole::Student],
        now,
    )
    .await?;
    let course_id = CourseId::new();
    instructor(&provisioner, course_id, &teacher).await?;
    sqlx::query("INSERT INTO control.course_memberships (course_id,actor_id,role,object_role,state,revision) VALUES ($1,$2,'student','participant','active',1)")
        .bind(course_id.as_uuid())
        .bind(course_member.actor_id.as_uuid())
        .execute(&provisioner)
        .await?;
    let course_revoke = service
        .remove_course_membership(
            &teacher,
            course_id,
            course_member.actor_id,
            &removal(Revision::new(1)?),
            &IdempotencyKey::parse("restricted-runtime-remove-course-member")?,
        )
        .await;

    let project_visibility: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM control.project_memberships pm \
         WHERE pm.project_id=$1 AND pm.actor_id=$2 AND pm.state='revoked')",
    )
    .bind(project_id.as_uuid())
    .bind(project_member.actor_id.as_uuid())
    .fetch_one(&access_runtime)
    .await?;
    assert!(
        project_visibility,
        "Access must see project membership revoked state"
    );

    let project_revoke = project_revoke?;
    let course_revoke = course_revoke?;
    assert_eq!(project_revoke.state, contracts::MembershipState::Revoked);
    assert_eq!(course_revoke.state, contracts::MembershipState::Revoked);
    let project_state: String = sqlx::query_scalar(
        "SELECT state FROM control.project_memberships WHERE project_id=$1 AND actor_id=$2",
    )
    .bind(project_id.as_uuid())
    .bind(project_member.actor_id.as_uuid())
    .fetch_one(&provisioner)
    .await?;
    let course_state: String = sqlx::query_scalar(
        "SELECT state FROM control.course_memberships WHERE course_id=$1 AND actor_id=$2",
    )
    .bind(course_id.as_uuid())
    .bind(course_member.actor_id.as_uuid())
    .fetch_one(&provisioner)
    .await?;
    assert_eq!(project_state, "revoked");
    assert_eq!(course_state, "revoked");

    let current_access_role: String = sqlx::query_scalar("SELECT current_user")
        .fetch_one(&access_runtime)
        .await?;
    assert_eq!(current_access_role, Domain::Access.runtime_role());

    let current_role: String = sqlx::query_scalar("SELECT current_user")
        .fetch_one(&control_runtime)
        .await?;
    assert_eq!(current_role, Domain::Control.runtime_role());
    Ok(())
}

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
    )?
    .with_cursor_signing_keys(vec![0x71; 32], None)?;
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
            &ControlListQuery {
                cursor: None,
                limit: Some(2),
            },
        )
        .await?;
    assert_eq!(page.items.len(), 2);
    assert!(page.next_cursor.is_some());
    assert!(matches!(
        service
            .course_memberships(&participant, course, &ControlListQuery::default())
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
            .course_memberships(&assistant, course, &ControlListQuery::default())
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
            .scoped_membership_invitations(
                &manager,
                &invite_request.scope,
                &ControlListQuery::default()
            )
            .await?
            .items
            .len(),
        1
    );
    assert!(matches!(
        service
            .scoped_membership_invitations(
                &recipient,
                &invite_request.scope,
                &ControlListQuery::default()
            )
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
    let key_ring = auth::KeyRing::parse(
        "synthetic".to_owned(),
        &format!(
            "synthetic:{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([7_u8; 32])
        ),
    )?;
    let _admin_session = auth::create_bff_session(
        &pool,
        &key_ring,
        auth::CreateBffSession {
            actor_id: admin.actor_id.as_uuid(),
            roles: vec![PlatformRole::PlatformAdmin],
            authorization_revision: 1,
            expires_at: time::OffsetDateTime::now_utc() + time::Duration::minutes(10),
            idle_ttl: time::Duration::minutes(5),
            oidc_sid: None,
            logout_hint: "synthetic-admin-logout".to_owned(),
        },
        time::OffsetDateTime::now_utc(),
    )
    .await?;
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
            .incoming_membership_invitations(&outsider, &ControlListQuery::default())
            .await?
            .items
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn revoked_admin_cannot_authorize_pending_invitation_acceptance() -> TestResult {
    let (_container, pool, service, now) = fixture().await?;
    let owner = actor(&pool, "owner", vec![PlatformRole::Student], now).await?;
    let admin = actor(&pool, "admin", vec![PlatformRole::PlatformAdmin], now).await?;
    let recipient = actor(&pool, "recipient", vec![PlatformRole::Student], now).await?;
    let project_id = project(&service, &owner, now, "admin-revocation-project").await?;
    let key_ring = auth::KeyRing::parse(
        "synthetic".to_owned(),
        &format!(
            "synthetic:{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([7_u8; 32])
        ),
    )?;
    let admin_session = auth::create_bff_session(
        &pool,
        &key_ring,
        auth::CreateBffSession {
            actor_id: admin.actor_id.as_uuid(),
            roles: vec![PlatformRole::PlatformAdmin],
            authorization_revision: 1,
            expires_at: time::OffsetDateTime::now_utc() + time::Duration::minutes(10),
            idle_ttl: time::Duration::minutes(5),
            oidc_sid: None,
            logout_hint: "synthetic-admin-revocation".to_owned(),
        },
        time::OffsetDateTime::now_utc(),
    )
    .await?;
    let pending = service
        .create_membership_invitation(
            &admin,
            &invitation(
                MembershipInvitationScope::Project {
                    project_id,
                    object_role: ProjectObjectRole::Editor,
                },
                &recipient,
                now,
            )?,
            &IdempotencyKey::parse("admin-revocation-invite")?,
        )
        .await?;
    sqlx::query(
        "UPDATE access.bff_sessions SET platform_roles=ARRAY['student'] WHERE session_id=$1",
    )
    .bind(admin_session.session_id)
    .execute(&pool)
    .await?;
    assert!(matches!(
        service
            .accept_membership_invitation(
                &recipient,
                pending.id,
                &AcceptMembershipInvitation {
                    expected_revision: pending.revision,
                },
                &IdempotencyKey::parse("admin-revocation-accept")?,
            )
            .await,
        Err(ControlError::InvitationDenied)
    ));
    Ok(())
}

fn page_query(cursor: Option<String>, limit: u16) -> ControlListQuery {
    ControlListQuery {
        cursor,
        limit: Some(limit),
    }
}

#[tokio::test]
async fn projects_cursor_survives_metadata_updates_and_rechecks_visibility() -> TestResult {
    let (_container, pool, service, now) = fixture().await?;
    let owner = actor(&pool, "pagination-owner", vec![PlatformRole::Student], now).await?;
    let outsider = actor(
        &pool,
        "pagination-outsider",
        vec![PlatformRole::Student],
        now,
    )
    .await?;
    let mut ids = Vec::new();
    for index in 0..3 {
        ids.push(project(&service, &owner, now, &format!("visible-{index}")).await?);
        project(&service, &outsider, now, &format!("hidden-{index}")).await?;
    }
    ids.sort_unstable();
    let first = service
        .list_projects(owner.actor_id, false, &page_query(None, 1))
        .await?;
    assert_eq!(
        first.items.iter().map(|p| p.id).collect::<Vec<_>>(),
        ids[..1]
    );
    let query = page_query(first.next_cursor, 1);
    for id in &ids {
        let original = service.project(*id).await?;
        service
            .update_project(
                *id,
                owner.actor_id,
                false,
                &contracts::UpdateProjectRequest {
                    expected_revision: original.revision,
                    name: format!("updated-{id}"),
                    description: None,
                },
                &IdempotencyKey::parse(&format!("update-{id}"))?,
                UtcTimestamp::from_utc(now.get() + time::Duration::seconds(1))?,
            )
            .await?;
    }
    let second = service.list_projects(owner.actor_id, false, &query).await?;
    assert_eq!(second.items[0].id, ids[1]);
    let last = service
        .list_projects(owner.actor_id, false, &page_query(second.next_cursor, 1))
        .await?;
    assert_eq!(last.items[0].id, ids[2]);
    assert!(last.next_cursor.is_none());
    assert!(matches!(
        service
            .list_projects(outsider.actor_id, false, &query)
            .await,
        Err(ControlError::CursorInvalid)
    ));
    // A visible non-owner project disappears on the next page after membership revocation.
    let shared = project(&service, &outsider, now, "shared-revocable").await?;
    service
        .add_project_membership(
            shared,
            outsider.actor_id,
            false,
            &ResolvedProjectMembershipRequest {
                actor_id: owner.actor_id,
                role: PlatformRole::Student,
                object_role: ProjectObjectRole::Viewer,
                expires_at: None,
            },
            &IdempotencyKey::parse("share-project")?,
            now,
        )
        .await?;
    sqlx::query("UPDATE control.project_memberships SET state='revoked',revision=revision+1 WHERE project_id=$1 AND actor_id=$2")
        .bind(shared.as_uuid()).bind(owner.actor_id.as_uuid()).execute(&pool).await?;
    let remaining = service
        .list_projects(owner.actor_id, false, &page_query(query.cursor, 100))
        .await?;
    assert_eq!(
        remaining.items.iter().map(|p| p.id).collect::<Vec<_>>(),
        ids[1..]
    );
    Ok(())
}

#[tokio::test]
async fn project_members_cursor_survives_concurrent_insert_delete_and_revocation() -> TestResult {
    let (_container, pool, service, now) = fixture().await?;
    let owner = actor(&pool, "member-owner", vec![PlatformRole::Student], now).await?;
    let project_id = project(&service, &owner, now, "member-pagination").await?;
    let other_project = project(&service, &owner, now, "member-other-scope").await?;
    for index in 0..3 {
        let member = actor(
            &pool,
            &format!("member-{index}"),
            vec![PlatformRole::Student],
            now,
        )
        .await?;
        service
            .add_project_membership(
                project_id,
                owner.actor_id,
                false,
                &ResolvedProjectMembershipRequest {
                    actor_id: member.actor_id,
                    role: PlatformRole::Student,
                    object_role: ProjectObjectRole::Viewer,
                    expires_at: None,
                },
                &IdempotencyKey::parse(&format!("member-{index}"))?,
                now,
            )
            .await?;
    }
    let first = service
        .project_memberships(project_id, owner.actor_id, false, &page_query(None, 2))
        .await?;
    let query = page_query(first.next_cursor.clone(), 100);
    assert!(query.cursor.is_some());
    let anchor = first.items[1].actor_id;
    let new_member = actor(&pool, "member-new", vec![PlatformRole::Student], now).await?;
    let request = ResolvedProjectMembershipRequest {
        actor_id: new_member.actor_id,
        role: PlatformRole::Student,
        object_role: ProjectObjectRole::Viewer,
        expires_at: None,
    };
    let key = IdempotencyKey::parse("member-new")?;
    let (deleted, added) = tokio::join!(
        sqlx::query("DELETE FROM control.project_memberships WHERE project_id=$1 AND actor_id=$2")
            .bind(project_id.as_uuid())
            .bind(anchor.as_uuid())
            .execute(&pool),
        service.add_project_membership(project_id, owner.actor_id, false, &request, &key, now)
    );
    deleted?;
    added?;
    let next = service
        .project_memberships(project_id, owner.actor_id, false, &query)
        .await?;
    let expected: Vec<uuid::Uuid> = sqlx::query_scalar("SELECT actor_id FROM control.project_memberships WHERE project_id=$1 AND actor_id>$2 ORDER BY actor_id")
        .bind(project_id.as_uuid()).bind(anchor.as_uuid()).fetch_all(&pool).await?;
    assert_eq!(
        next.items
            .iter()
            .map(|m| m.actor_id.as_uuid())
            .collect::<Vec<_>>(),
        expected
    );
    assert!(next.next_cursor.is_none());
    assert!(matches!(
        service
            .project_memberships(other_project, owner.actor_id, false, &query)
            .await,
        Err(ControlError::CursorInvalid)
    ));
    // The caller is a regular member; current persisted permission fences old cursors.
    let caller = new_member.actor_id;
    let caller_page = service
        .project_memberships(project_id, caller, false, &page_query(None, 1))
        .await?;
    sqlx::query("UPDATE control.project_memberships SET state='revoked',revision=revision+1 WHERE project_id=$1 AND actor_id=$2")
        .bind(project_id.as_uuid()).bind(caller.as_uuid()).execute(&pool).await?;
    assert!(matches!(
        service
            .project_memberships(
                project_id,
                caller,
                false,
                &page_query(caller_page.next_cursor, 1)
            )
            .await,
        Err(ControlError::ProjectGovernanceDenied)
    ));
    Ok(())
}

#[tokio::test]
async fn course_members_cursor_survives_concurrent_insert_delete_and_revocation() -> TestResult {
    let (_container, pool, service, now) = fixture().await?;
    let teacher = actor(
        &pool,
        "course-page-teacher",
        vec![PlatformRole::Teacher],
        now,
    )
    .await?;
    let course = CourseId::new();
    instructor(&pool, course, &teacher).await?;
    for index in 0..3 {
        let member = actor(
            &pool,
            &format!("course-page-{index}"),
            vec![PlatformRole::Student],
            now,
        )
        .await?;
        sqlx::query("INSERT INTO control.course_memberships(course_id,actor_id,role,object_role,state,revision) VALUES($1,$2,'student','participant','active',1)")
            .bind(course.as_uuid()).bind(member.actor_id.as_uuid()).execute(&pool).await?;
    }
    let first = service
        .course_memberships(&teacher, course, &page_query(None, 2))
        .await?;
    let anchor = first.items[1].membership.actor_id;
    let query = page_query(first.next_cursor, 100);
    assert!(query.cursor.is_some());
    let member = actor(&pool, "course-page-new", vec![PlatformRole::Student], now).await?;
    let (deleted, added) = tokio::join!(
        sqlx::query("DELETE FROM control.course_memberships WHERE course_id=$1 AND actor_id=$2").bind(course.as_uuid()).bind(anchor.as_uuid()).execute(&pool),
        sqlx::query("INSERT INTO control.course_memberships(course_id,actor_id,role,object_role,state,revision) VALUES($1,$2,'student','participant','active',1)").bind(course.as_uuid()).bind(member.actor_id.as_uuid()).execute(&pool)
    );
    deleted?;
    added?;
    let next = service.course_memberships(&teacher, course, &query).await?;
    let expected: Vec<uuid::Uuid> = sqlx::query_scalar("SELECT actor_id FROM control.course_memberships WHERE course_id=$1 AND actor_id>$2 ORDER BY actor_id")
        .bind(course.as_uuid()).bind(anchor.as_uuid()).fetch_all(&pool).await?;
    assert_eq!(
        next.items
            .iter()
            .map(|m| m.membership.actor_id.as_uuid())
            .collect::<Vec<_>>(),
        expected
    );
    assert!(next.next_cursor.is_none());
    assert!(matches!(
        service
            .course_memberships(&teacher, CourseId::new(), &query)
            .await,
        Err(ControlError::CursorInvalid)
    ));
    sqlx::query("UPDATE control.course_memberships SET state='revoked',revision=revision+1 WHERE course_id=$1 AND actor_id=$2")
        .bind(course.as_uuid()).bind(teacher.actor_id.as_uuid()).execute(&pool).await?;
    assert!(matches!(
        service.course_memberships(&teacher, course, &query).await,
        Err(ControlError::CourseGovernanceDenied)
    ));
    Ok(())
}

#[allow(clippy::too_many_lines)]
async fn invitation_cursor_case(course_scope: bool, incoming: bool) -> TestResult {
    let (_container, pool, service, now) = fixture().await?;
    let governor = actor(
        &pool,
        "invitation-page-governor",
        vec![PlatformRole::Teacher],
        now,
    )
    .await?;
    let recipient = actor(
        &pool,
        "invitation-page-recipient",
        vec![PlatformRole::Student],
        now,
    )
    .await?;
    let outsider = actor(
        &pool,
        "invitation-page-outsider",
        vec![PlatformRole::Student],
        now,
    )
    .await?;
    let scope = if course_scope {
        let course_id = CourseId::new();
        instructor(&pool, course_id, &governor).await?;
        MembershipInvitationScope::Course {
            course_id,
            object_role: CourseObjectRole::Participant,
        }
    } else {
        let owner = actor(
            &pool,
            "invitation-page-owner",
            vec![PlatformRole::Student],
            now,
        )
        .await?;
        let project_id = project(&service, &owner, now, "invitation-page-project").await?;
        service
            .add_project_membership(
                project_id,
                owner.actor_id,
                false,
                &ResolvedProjectMembershipRequest {
                    actor_id: governor.actor_id,
                    role: PlatformRole::Teacher,
                    object_role: ProjectObjectRole::Manager,
                    expires_at: None,
                },
                &IdempotencyKey::parse("invitation-page-manager")?,
                now,
            )
            .await?;
        MembershipInvitationScope::Project {
            project_id,
            object_role: ProjectObjectRole::Viewer,
        }
    };
    let request = invitation(scope.clone(), &recipient, now)?;
    for index in 0..4 {
        service
            .create_membership_invitation(
                &governor,
                &request,
                &IdempotencyKey::parse(&format!("page-invitation-{index}"))?,
            )
            .await?;
        // Other recipients must not push the recipient's rows past LIMIT.
        service
            .create_membership_invitation(
                &governor,
                &invitation(scope.clone(), &outsider, now)?,
                &IdempotencyKey::parse(&format!("other-invitation-{index}"))?,
            )
            .await?;
    }
    let first = if incoming {
        service
            .incoming_membership_invitations(&recipient, &page_query(None, 2))
            .await?
    } else {
        service
            .scoped_membership_invitations(&governor, &scope, &page_query(None, 2))
            .await?
    };
    assert_eq!(first.items.len(), 2);
    let anchor = first.items[1].id;
    let query = page_query(first.next_cursor, 100);
    assert!(query.cursor.is_some());
    let delete = sqlx::query("DELETE FROM control.membership_invitations WHERE invitation_id=$1")
        .bind(anchor.as_uuid());
    let key = IdempotencyKey::parse("concurrent-new-invitation")?;
    let (deleted, added) = tokio::join!(
        delete.execute(&pool),
        service.create_membership_invitation(&governor, &request, &key)
    );
    deleted?;
    let new_invitation = added?;
    let next = if incoming {
        service
            .incoming_membership_invitations(&recipient, &query)
            .await?
    } else {
        service
            .scoped_membership_invitations(&governor, &scope, &query)
            .await?
    };
    assert!(
        next.items
            .iter()
            .all(|entry| entry.id < anchor && entry.id != new_invitation.id)
    );
    let expected: Vec<uuid::Uuid> = if incoming {
        sqlx::query_scalar("SELECT invitation_id FROM control.membership_invitations WHERE recipient_actor_id=$1 AND invitation_id<$2 ORDER BY invitation_id DESC")
            .bind(recipient.actor_id.as_uuid()).bind(anchor.as_uuid()).fetch_all(&pool).await?
    } else {
        let (kind, id) = match scope {
            MembershipInvitationScope::Project { project_id, .. } => {
                ("project", project_id.as_uuid())
            }
            MembershipInvitationScope::Course { course_id, .. } => ("course", course_id.as_uuid()),
        };
        sqlx::query_scalar("SELECT invitation_id FROM control.membership_invitations WHERE scope_kind=$1 AND scope_id=$2 AND invitation_id<$3 ORDER BY invitation_id DESC")
            .bind(kind).bind(id).bind(anchor.as_uuid()).fetch_all(&pool).await?
    };
    assert_eq!(
        next.items
            .iter()
            .map(|entry| entry.id.as_uuid())
            .collect::<Vec<_>>(),
        expected
    );
    assert!(next.next_cursor.is_none());
    if incoming {
        assert!(
            next.items
                .iter()
                .all(|entry| entry.recipient_actor_id == recipient.actor_id)
        );
        assert!(matches!(
            service
                .incoming_membership_invitations(&outsider, &query)
                .await,
            Err(ControlError::CursorInvalid)
        ));
        assert!(matches!(
            service
                .scoped_membership_invitations(&governor, &scope, &query)
                .await,
            Err(ControlError::CursorInvalid)
        ));
        sqlx::query("UPDATE access.actors SET disabled_at=clock_timestamp() WHERE actor_id=$1")
            .bind(recipient.actor_id.as_uuid())
            .execute(&pool)
            .await?;
        assert!(matches!(
            service
                .incoming_membership_invitations(&recipient, &query)
                .await,
            Err(ControlError::InvitationDenied)
        ));
    } else {
        let other_scope = if course_scope {
            MembershipInvitationScope::Course {
                course_id: CourseId::new(),
                object_role: CourseObjectRole::Participant,
            }
        } else {
            MembershipInvitationScope::Project {
                project_id: ProjectId::new(),
                object_role: ProjectObjectRole::Viewer,
            }
        };
        assert!(matches!(
            service
                .scoped_membership_invitations(&governor, &other_scope, &query)
                .await,
            Err(ControlError::CursorInvalid)
        ));
        match scope {
            MembershipInvitationScope::Project { project_id, .. } => {
                sqlx::query("UPDATE control.project_memberships SET state='revoked',revision=revision+1 WHERE project_id=$1 AND actor_id=$2").bind(project_id.as_uuid()).bind(governor.actor_id.as_uuid()).execute(&pool).await?;
            }
            MembershipInvitationScope::Course { course_id, .. } => {
                sqlx::query("UPDATE control.course_memberships SET state='revoked',revision=revision+1 WHERE course_id=$1 AND actor_id=$2").bind(course_id.as_uuid()).bind(governor.actor_id.as_uuid()).execute(&pool).await?;
            }
        }
        assert!(matches!(
            service
                .scoped_membership_invitations(&governor, &scope, &query)
                .await,
            Err(ControlError::InvitationDenied)
        ));
    }
    Ok(())
}

#[tokio::test]
async fn project_invitations_cursor_survives_concurrent_insert_delete_and_revocation() -> TestResult
{
    invitation_cursor_case(false, false).await
}

#[tokio::test]
async fn course_invitations_cursor_survives_concurrent_insert_delete_and_revocation() -> TestResult
{
    invitation_cursor_case(true, false).await
}

#[tokio::test]
async fn incoming_invitations_cursor_survives_concurrent_insert_delete_and_account_disable()
-> TestResult {
    invitation_cursor_case(false, true).await
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CursorAttackPayload {
    version: u8,
    key_id: String,
    id: uuid::Uuid,
    scope: serde_json::Value,
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct CursorAttackEnvelope {
    cursor: CursorAttackPayload,
    signature: String,
}

fn decode_attack_cursor(value: &str) -> TestResult<CursorAttackEnvelope> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(value)?;
    let cursor: CursorAttackEnvelope = serde_json::from_slice(&bytes)?;
    assert_eq!(
        serde_json::to_vec(&cursor)?,
        bytes,
        "attack keeps canonical serialization so rejection cannot rely on encoding alone"
    );
    Ok(cursor)
}

fn encode_attack_cursor(value: &CursorAttackEnvelope) -> TestResult<String> {
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(value)?))
}

fn assert_cursor_http_400<T>(result: Result<T, ControlError>) -> TestResult {
    use axum::response::IntoResponse as _;
    let error = result.err().ok_or("a forged/reused cursor must fail")?;
    assert!(matches!(error, ControlError::CursorInvalid));
    assert_eq!(
        control_service::api::ApiError::from(error)
            .into_response()
            .status(),
        axum::http::StatusCode::BAD_REQUEST
    );
    Ok(())
}

async fn signed_member_page_fixture() -> TestResult<(
    ContainerAsync<Postgres>,
    PgPool,
    ControlService,
    AuthenticatedActor,
    ProjectId,
    ProjectId,
    contracts::membership_governance::ProjectMembershipPage,
)> {
    let (container, pool, service, now) = fixture().await?;
    let owner = actor(&pool, "signed-page-owner", vec![PlatformRole::Student], now).await?;
    let project_id = project(&service, &owner, now, "signed-page-project").await?;
    let other_project = project(&service, &owner, now, "signed-page-other").await?;
    for index in 0..3 {
        let member = actor(
            &pool,
            &format!("signed-member-{index}"),
            vec![PlatformRole::Student],
            now,
        )
        .await?;
        service
            .add_project_membership(
                project_id,
                owner.actor_id,
                false,
                &ResolvedProjectMembershipRequest {
                    actor_id: member.actor_id,
                    role: PlatformRole::Student,
                    object_role: ProjectObjectRole::Viewer,
                    expires_at: None,
                },
                &IdempotencyKey::parse(&format!("signed-member-{index}"))?,
                now,
            )
            .await?;
    }
    let first = service
        .project_memberships(project_id, owner.actor_id, false, &page_query(None, 2))
        .await?;
    assert!(first.next_cursor.is_some());
    Ok((
        container,
        pool,
        service,
        owner,
        project_id,
        other_project,
        first,
    ))
}

#[tokio::test]
async fn signed_control_cursor_rejects_modified_boundary_with_http_400() -> TestResult {
    let (_container, _pool, service, owner, project_id, _other, first) =
        signed_member_page_fixture().await?;
    let value = first.next_cursor.ok_or("cursor missing")?;
    let mut attack = decode_attack_cursor(&value)?;
    attack.cursor.id = uuid::Uuid::nil();
    let query = page_query(Some(encode_attack_cursor(&attack)?), 2);
    assert_cursor_http_400(
        service
            .project_memberships(project_id, owner.actor_id, false, &query)
            .await,
    )?;
    Ok(())
}

#[tokio::test]
async fn signed_control_cursor_rejects_recomputed_public_scope_digest_with_http_400() -> TestResult
{
    use sha2::Digest as _;
    let (_container, _pool, service, owner, _project_id, other, first) =
        signed_member_page_fixture().await?;
    let mut attack = decode_attack_cursor(first.next_cursor.as_deref().ok_or("cursor missing")?)?;
    // Use the exact, currently authorized target scope and recompute an ordinary
    // public SHA-256 digest. Neither the old scope-hash attack nor an unkeyed
    // payload hash can substitute for the server's HMAC over the full payload.
    attack.cursor.scope = serde_json::json!(("project-members", other, owner.actor_id, false));
    attack.signature = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(sha2::Sha256::digest(serde_json::to_vec(&attack.cursor)?));
    let query = page_query(Some(encode_attack_cursor(&attack)?), 2);
    assert_cursor_http_400(
        service
            .project_memberships(other, owner.actor_id, false, &query)
            .await,
    )?;
    Ok(())
}

#[tokio::test]
async fn signed_control_cursor_rejects_cross_endpoint_and_actor_with_http_400() -> TestResult {
    let (_container, _pool, service, owner, project_id, _other, first) =
        signed_member_page_fixture().await?;
    let query = page_query(first.next_cursor, 2);
    assert_cursor_http_400(service.list_projects(owner.actor_id, false, &query).await)?;
    // The other actor is a live member with Browse: current permission is not
    // enough to reuse a cursor issued to another actor.
    let other_actor = first
        .items
        .iter()
        .find(|member| member.actor_id != owner.actor_id)
        .ok_or("other actor missing")?
        .actor_id;
    assert_cursor_http_400(
        service
            .project_memberships(project_id, other_actor, false, &query)
            .await,
    )?;
    Ok(())
}

#[tokio::test]
async fn signed_control_cursor_continues_after_anchor_deletion() -> TestResult {
    let (_container, pool, service, owner, project_id, _other, first) =
        signed_member_page_fixture().await?;
    let anchor = first.items.last().ok_or("anchor missing")?.actor_id;
    sqlx::query("DELETE FROM control.project_memberships WHERE project_id=$1 AND actor_id=$2")
        .bind(project_id.as_uuid())
        .bind(anchor.as_uuid())
        .execute(&pool)
        .await?;
    let next = service
        .project_memberships(
            project_id,
            owner.actor_id,
            false,
            &page_query(first.next_cursor, 100),
        )
        .await?;
    let expected: Vec<uuid::Uuid> = sqlx::query_scalar("SELECT actor_id FROM control.project_memberships WHERE project_id=$1 AND actor_id>$2 ORDER BY actor_id")
        .bind(project_id.as_uuid()).bind(anchor.as_uuid()).fetch_all(&pool).await?;
    assert!(!expected.is_empty());
    assert_eq!(
        next.items
            .iter()
            .map(|member| member.actor_id.as_uuid())
            .collect::<Vec<_>>(),
        expected
    );
    assert!(next.next_cursor.is_none());
    Ok(())
}
