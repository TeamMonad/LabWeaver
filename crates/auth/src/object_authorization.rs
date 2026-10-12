//! Object action decisions from Control's current member facts.

use contracts::object_authorization::{ObjectAction, project_operation_action};
use contracts::{
    AuthorizationDecision, AuthorizationScope, MembershipState, PlatformRole, Revision,
};

use crate::{AuthorizationContext, AuthorizationError};

/// Checks an object action without treating the issuer's role as project responsibility.
pub fn authorize_project_action(
    context: &AuthorizationContext,
    project_id: contracts::ProjectId,
    action: ObjectAction,
) -> Result<AuthorizationDecision, AuthorizationError> {
    if context.actor.roles.is_empty() || context.actor.expires_at.get() <= context.now {
        return Err(AuthorizationError::IdentityExpired);
    }
    let admin = context.actor.roles.contains(&PlatformRole::PlatformAdmin);
    let (revision, valid_until) = if admin {
        (
            Revision::new(1).map_err(|_| AuthorizationError::RoleDenied)?,
            context.actor.expires_at,
        )
    } else {
        let member = context
            .project_memberships
            .iter()
            .find(|member| {
                member.project_id == project_id
                    && member.actor_id == context.actor.actor_id
                    && member.state == MembershipState::Active
                    && member
                        .expires_at
                        .is_none_or(|expiry| expiry.get() > context.now)
            })
            .ok_or(AuthorizationError::ProjectScopeDenied)?;
        if !member.object_role.actions().contains(&action) {
            return Err(AuthorizationError::RoleDenied);
        }
        let valid_until = member
            .expires_at
            .filter(|expiry| *expiry < context.actor.expires_at)
            .unwrap_or(context.actor.expires_at);
        (member.revision, valid_until)
    };
    Ok(AuthorizationDecision {
        actor: context.actor.clone(),
        scope: AuthorizationScope::Project { project_id },
        authorization_revision: revision,
        scope_revision: revision,
        valid_until,
        diagnostic_code: None,
    })
}

/// Applies object actions to recognized project operations and preserves explicit legacy policies elsewhere.
pub fn authorize_operation(
    context: &AuthorizationContext,
    scope: AuthorizationScope,
    operation: &contracts::http::OperationContract,
) -> Result<AuthorizationDecision, AuthorizationError> {
    if let AuthorizationScope::Project { project_id } = scope
        && let Some(action) = project_operation_action(operation.operation_id)
    {
        if !context
            .actor
            .roles
            .iter()
            .any(|role| operation.allowed_roles.contains(role))
        {
            return Err(AuthorizationError::RoleDenied);
        }
        return authorize_project_action(context, project_id, action);
    }
    crate::authorize(
        context,
        scope,
        &operation.allowed_roles.iter().copied().collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use contracts::object_authorization::ProjectObjectRole;

    fn context() -> Result<
        (
            AuthorizationContext,
            contracts::ProjectId,
            contracts::ProjectId,
        ),
        Box<dyn std::error::Error>,
    > {
        let actor_id = contracts::ActorId::new();
        let viewer_project = contracts::ProjectId::new();
        let editor_project = contracts::ProjectId::new();
        let revision = Revision::new(4)?;
        let member = |project_id, object_role| contracts::ProjectMembership {
            project_id,
            object_role,
            course_id: None,
            actor_id,
            username: None,
            display_name: None,
            role: PlatformRole::Teacher,
            state: MembershipState::Active,
            revision,
            expires_at: None,
        };
        Ok((
            AuthorizationContext {
                actor: contracts::AuthenticatedActor {
                    actor_id,
                    roles: vec![PlatformRole::Teacher],
                    expires_at: "2026-10-13T00:00:00.000Z".parse()?,
                },
                course_memberships: vec![],
                project_memberships: vec![
                    member(viewer_project, ProjectObjectRole::Viewer),
                    member(editor_project, ProjectObjectRole::Editor),
                ],
                now: "2026-10-12T00:00:00.000Z"
                    .parse::<contracts::UtcTimestamp>()?
                    .get(),
            },
            viewer_project,
            editor_project,
        ))
    }

    #[test]
    fn same_teacher_has_independent_project_actions() -> Result<(), Box<dyn std::error::Error>> {
        let (context, viewer, editor) = context()?;
        assert!(authorize_project_action(&context, viewer, ObjectAction::Browse).is_ok());
        for action in [
            ObjectAction::Edit,
            ObjectAction::Start,
            ObjectAction::Stop,
            ObjectAction::ConfigureAgent,
            ObjectAction::ManageMembers,
            ObjectAction::ViewCosts,
            ObjectAction::RequestRuntimePermissions,
        ] {
            assert!(authorize_project_action(&context, viewer, action).is_err());
        }
        assert!(authorize_project_action(&context, editor, ObjectAction::Edit).is_ok());
        assert!(authorize_project_action(&context, editor, ObjectAction::ConfigureAgent).is_ok());
        assert!(
            authorize_project_action(&context, editor, ObjectAction::ManageRuntimePolicy).is_err()
        );
        assert!(authorize_project_action(&context, editor, ObjectAction::ManageMembers).is_err());
        assert!(
            authorize_project_action(&context, contracts::ProjectId::new(), ObjectAction::Browse)
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn revocation_suspension_and_expiry_deny_even_with_a_teacher_identity()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut context, viewer, _) = context()?;
        for state in [MembershipState::Suspended, MembershipState::Revoked] {
            context.project_memberships[0].state = state;
            assert!(authorize_project_action(&context, viewer, ObjectAction::Browse).is_err());
        }
        context.project_memberships[0].state = MembershipState::Active;
        context.project_memberships[0].expires_at =
            Some(contracts::UtcTimestamp::from_utc(context.now)?);
        assert!(authorize_project_action(&context, viewer, ObjectAction::Browse).is_err());
        Ok(())
    }

    #[test]
    fn admin_authority_still_requires_a_live_identity() -> Result<(), Box<dyn std::error::Error>> {
        let (mut context, _, _) = context()?;
        context.actor.roles = vec![PlatformRole::PlatformAdmin];
        context.project_memberships.clear();
        assert!(
            authorize_project_action(
                &context,
                contracts::ProjectId::new(),
                ObjectAction::ManageRuntimePolicy
            )
            .is_ok()
        );
        context.actor.expires_at = contracts::UtcTimestamp::from_utc(context.now)?;
        assert!(
            authorize_project_action(&context, contracts::ProjectId::new(), ObjectAction::Browse)
                .is_err()
        );
        Ok(())
    }
    #[test]
    fn admin_course_authority_requires_an_explicit_operation_grant()
    -> Result<(), Box<dyn std::error::Error>> {
        let (mut context, _, _) = context()?;
        context.actor.roles = vec![PlatformRole::PlatformAdmin];
        let scope = AuthorizationScope::Course {
            course_id: contracts::CourseId::new(),
        };
        assert!(
            crate::authorize(
                &context,
                scope.clone(),
                &std::collections::BTreeSet::from([
                    PlatformRole::Teacher,
                    PlatformRole::PlatformAdmin
                ])
            )
            .is_ok()
        );
        assert!(
            crate::authorize(
                &context,
                scope,
                &std::collections::BTreeSet::from([PlatformRole::Teacher])
            )
            .is_err()
        );
        Ok(())
    }
}
