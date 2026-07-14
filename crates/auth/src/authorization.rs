use std::collections::BTreeSet;

use contracts::{
    AuthenticatedActor, AuthorizationDecision, AuthorizationScope, CourseMembership,
    MembershipState, PlatformRole, ProjectMembership, Revision, UtcTimestamp,
};
use time::OffsetDateTime;

/// Inputs supplied by the Access Service after token/session verification.
#[derive(Clone, Debug)]
pub struct AuthorizationContext {
    /// Verified OIDC actor.
    pub actor: AuthenticatedActor,
    /// Current course memberships loaded from the Access authority.
    pub course_memberships: Vec<CourseMembership>,
    /// Current project memberships loaded from the Access authority.
    pub project_memberships: Vec<ProjectMembership>,
    /// Time of decision supplied by the service clock.
    pub now: OffsetDateTime,
}

/// Evaluates base-role and authoritative course/project scope.
pub fn authorize(
    context: &AuthorizationContext,
    scope: AuthorizationScope,
    required_roles: &BTreeSet<PlatformRole>,
) -> Result<AuthorizationDecision, AuthorizationError> {
    if required_roles.is_empty()
        || !context
            .actor
            .roles
            .iter()
            .any(|role| required_roles.contains(role))
    {
        return Err(AuthorizationError::RoleDenied);
    }
    let (revision, valid_until) = match &scope {
        AuthorizationScope::Global | AuthorizationScope::Service { .. } => (
            Revision::new(1).map_err(|_| AuthorizationError::RoleDenied)?,
            context.actor.expires_at,
        ),
        AuthorizationScope::Course { course_id }
        | AuthorizationScope::Environment { course_id, .. } => {
            let membership = active_course_membership(context, *course_id, required_roles)?;
            (
                membership.revision,
                earliest(context.actor.expires_at, membership.expires_at),
            )
        }
        AuthorizationScope::Project {
            course_id,
            project_id,
        } => {
            let membership = context
                .project_memberships
                .iter()
                .find(|membership| {
                    membership.course_id == *course_id
                        && membership.project_id == *project_id
                        && membership.actor_id == context.actor.actor_id
                        && required_roles.contains(&membership.role)
                        && membership.state == MembershipState::Active
                        && not_expired(membership.expires_at, context.now)
                })
                .ok_or(AuthorizationError::ProjectScopeDenied)?;
            (
                membership.revision,
                earliest(context.actor.expires_at, membership.expires_at),
            )
        }
    };
    if context.actor.expires_at.get() <= context.now || valid_until.get() <= context.now {
        return Err(AuthorizationError::IdentityExpired);
    }
    Ok(AuthorizationDecision {
        actor: context.actor.clone(),
        scope,
        authorization_revision: revision,
        scope_revision: revision,
        valid_until,
        diagnostic_code: None,
    })
}

fn active_course_membership<'a>(
    context: &'a AuthorizationContext,
    course_id: contracts::CourseId,
    required_roles: &BTreeSet<PlatformRole>,
) -> Result<&'a CourseMembership, AuthorizationError> {
    context
        .course_memberships
        .iter()
        .find(|membership| {
            membership.course_id == course_id
                && membership.actor_id == context.actor.actor_id
                && required_roles.contains(&membership.role)
                && membership.state == MembershipState::Active
                && not_expired(membership.expires_at, context.now)
        })
        .ok_or(AuthorizationError::CourseScopeDenied)
}

fn not_expired(expiry: Option<UtcTimestamp>, now: OffsetDateTime) -> bool {
    expiry.is_none_or(|value| value.get() > now)
}

fn earliest(actor_expiry: UtcTimestamp, membership_expiry: Option<UtcTimestamp>) -> UtcTimestamp {
    membership_expiry
        .filter(|expiry| expiry.get() < actor_expiry.get())
        .unwrap_or(actor_expiry)
}

/// Fail-closed authorization rejections.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum AuthorizationError {
    /// No verified base role grants this operation.
    #[error("LW_AUTH_ROLE_DENIED")]
    RoleDenied,
    /// No active current course membership grants this operation.
    #[error("LW_AUTH_COURSE_SCOPE_DENIED")]
    CourseScopeDenied,
    /// No active current project membership grants this operation.
    #[error("LW_AUTH_PROJECT_SCOPE_DENIED")]
    ProjectScopeDenied,
    /// The identity or bound authorization has expired.
    #[error("LW_AUTH_IDENTITY_EXPIRED")]
    IdentityExpired,
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, str::FromStr};

    use contracts::{
        ActorId, AuthenticatedActor, AuthorizationScope, CourseId, CourseMembership,
        MembershipState, PlatformRole, Revision, UtcTimestamp,
    };
    use time::OffsetDateTime;

    use super::{AuthorizationContext, authorize};

    fn timestamp(value: &str) -> Result<UtcTimestamp, contracts::foundation::FoundationError> {
        UtcTimestamp::from_str(value)
    }

    #[test]
    fn course_scope_requires_matching_active_membership_and_role()
    -> Result<(), Box<dyn std::error::Error>> {
        let actor_id = ActorId::new();
        let course_id = CourseId::new();
        let actor = AuthenticatedActor {
            actor_id,
            roles: vec![PlatformRole::Teacher],
            expires_at: timestamp("2026-07-15T00:00:00.000Z")?,
        };
        let context = AuthorizationContext {
            actor,
            course_memberships: vec![CourseMembership {
                course_id,
                actor_id,
                role: PlatformRole::Teacher,
                state: MembershipState::Active,
                revision: Revision::new(3)?,
                expires_at: Some(timestamp("2026-07-14T12:00:00.000Z")?),
            }],
            project_memberships: Vec::new(),
            now: OffsetDateTime::parse(
                "2026-07-14T00:00:00Z",
                &time::format_description::well_known::Rfc3339,
            )?,
        };
        let roles = BTreeSet::from([PlatformRole::Teacher]);
        let decision = authorize(&context, AuthorizationScope::Course { course_id }, &roles)?;
        assert_eq!(decision.authorization_revision.get(), 3);
        assert_eq!(decision.valid_until, timestamp("2026-07-14T12:00:00.000Z")?);

        let wrong_course = authorize(
            &context,
            AuthorizationScope::Course {
                course_id: CourseId::new(),
            },
            &roles,
        );
        assert!(wrong_course.is_err());
        Ok(())
    }
}
