//! Credits reads use the existing project authority. Admin writes stay global.
use auth::{AuthorizationContext, AuthorizationError, authorize};
use contracts::{
    ActorId, AuthenticatedActor, AuthorizationScope, MembershipState, PlatformRole, ProjectId,
    ProjectMembership, Revision, UtcTimestamp,
};
#[test]
fn credits_reads_reject_other_projects_revocation_and_member_minting()
-> Result<(), Box<dyn std::error::Error>> {
    let actor_id = ActorId::new();
    let project = ProjectId::new();
    let now: UtcTimestamp = "2026-10-12T00:00:00.000Z".parse()?;
    let mut context = AuthorizationContext {
        actor: AuthenticatedActor {
            actor_id,
            roles: vec![PlatformRole::Student],
            expires_at: UtcTimestamp::from_utc(now.get() + time::Duration::hours(1))?,
        },
        course_memberships: vec![],
        project_memberships: vec![ProjectMembership {
            course_id: None,
            object_role: contracts::object_authorization::ProjectObjectRole::Viewer,
            project_id: project,
            actor_id,
            username: None,
            display_name: None,
            role: PlatformRole::Student,
            state: MembershipState::Active,
            revision: Revision::new(1)?,
            expires_at: None,
        }],
        now: now.get(),
    };
    for id in [
        "getProjectCredits",
        "listProjectCreditLedger",
        "listProjectCreditHolds",
        "estimateProjectResourceCost",
    ] {
        let policy = contracts::operation_contract(id).ok_or("operation missing")?;
        let roles = policy.allowed_roles.iter().copied().collect();
        assert!(
            authorize(
                &context,
                AuthorizationScope::Project {
                    project_id: project
                },
                &roles
            )
            .is_ok()
        );
        assert!(matches!(
            authorize(
                &context,
                AuthorizationScope::Project {
                    project_id: ProjectId::new()
                },
                &roles
            ),
            Err(AuthorizationError::ProjectScopeDenied)
        ));
    }
    let policy = contracts::operation_contract("createCreditAdminOperation")
        .ok_or("admin operation missing")?;
    assert!(matches!(
        authorize(
            &context,
            AuthorizationScope::Global,
            &policy.allowed_roles.iter().copied().collect()
        ),
        Err(AuthorizationError::RoleDenied)
    ));
    context.project_memberships[0].state = MembershipState::Revoked;
    let policy =
        contracts::operation_contract("getProjectCredits").ok_or("read operation missing")?;
    assert!(matches!(
        authorize(
            &context,
            AuthorizationScope::Project {
                project_id: project
            },
            &policy.allowed_roles.iter().copied().collect()
        ),
        Err(AuthorizationError::ProjectScopeDenied)
    ));
    Ok(())
}
