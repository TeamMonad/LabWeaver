//! Project release authorization must accept every role that can own a private Work.

use std::{collections::BTreeSet, str::FromStr};

use auth::{AuthorizationContext, authorize};
use contracts::{
    ActorId, AuthenticatedActor, AuthorizationScope, MembershipState, PlatformRole, ProjectId,
    ProjectMembership, Revision, UtcTimestamp, operation_contract,
};

fn timestamp(value: &str) -> Result<UtcTimestamp, Box<dyn std::error::Error>> {
    Ok(UtcTimestamp::from_str(value)?)
}

#[test]
fn private_work_release_operations_authorize_student_and_platform_admin_owners()
-> Result<(), Box<dyn std::error::Error>> {
    let project_id = ProjectId::new();
    let operation_ids = [
        "listEnvironmentTemplateReleases",
        "createEnvironmentTemplateRelease",
        "getEnvironmentTemplateRelease",
        "withdrawEnvironmentTemplateRelease",
    ];

    for role in [PlatformRole::Student, PlatformRole::PlatformAdmin] {
        let actor_id = ActorId::new();
        let actor = AuthenticatedActor {
            actor_id,
            roles: vec![role],
            expires_at: timestamp("2026-09-09T00:00:00.000Z")?,
        };
        let context = AuthorizationContext {
            actor,
            course_memberships: Vec::new(),
            project_memberships: vec![ProjectMembership {
                course_id: None,
                project_id,
                actor_id,
                role,
                state: MembershipState::Active,
                revision: Revision::new(7)?,
                expires_at: None,
            }],
            now: time::OffsetDateTime::parse(
                "2026-09-08T00:00:00Z",
                &time::format_description::well_known::Rfc3339,
            )?,
        };

        for operation_id in operation_ids {
            let operation =
                operation_contract(operation_id).ok_or("operation is not catalogued")?;
            assert_eq!(operation.scope, contracts::OperationScopeKind::Project);
            assert!(
                operation.allowed_roles.contains(&role),
                "{operation_id} rejects {role:?} project owners"
            );
            let required_roles = operation
                .allowed_roles
                .iter()
                .copied()
                .collect::<BTreeSet<_>>();
            let decision = authorize(
                &context,
                AuthorizationScope::Project { project_id },
                &required_roles,
            )?;
            assert_eq!(decision.actor.actor_id, actor_id);
        }
    }
    Ok(())
}

#[test]
fn shared_experiment_authoring_approval_remains_teacher_or_admin_only()
-> Result<(), Box<dyn std::error::Error>> {
    let operation = operation_contract("completeProjectAuthoringApproval")
        .ok_or_else(|| std::io::Error::other("authoring approval operation is not catalogued"))?;
    assert_eq!(
        operation.allowed_roles,
        &[PlatformRole::Teacher, PlatformRole::PlatformAdmin]
    );
    assert!(!operation.allowed_roles.contains(&PlatformRole::Student));
    Ok(())
}
