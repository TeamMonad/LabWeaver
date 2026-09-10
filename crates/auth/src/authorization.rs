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
        AuthorizationScope::Course { course_id } => {
            let membership = active_course_membership(context, *course_id, required_roles)?;
            (
                membership.revision,
                earliest(context.actor.expires_at, membership.expires_at),
            )
        }
        AuthorizationScope::Project { project_id } => {
            let membership = active_project_membership(context, *project_id, required_roles)?;
            (
                membership.revision,
                earliest(context.actor.expires_at, membership.expires_at),
            )
        }
        AuthorizationScope::Environment {
            project_id,
            course_id,
            ..
        } => {
            let project_membership =
                active_project_membership(context, *project_id, required_roles)?;
            let course_membership = course_id
                .map(|course_id| active_course_membership(context, course_id, required_roles))
                .transpose()?;
            let revision = course_membership.map_or(project_membership.revision, |membership| {
                max_revision(project_membership.revision, membership.revision)
            });
            let valid_until =
                course_membership.map_or(project_membership.expires_at, |membership| {
                    earliest_optional(project_membership.expires_at, membership.expires_at)
                });
            (revision, earliest(context.actor.expires_at, valid_until))
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

fn active_project_membership<'a>(
    context: &'a AuthorizationContext,
    project_id: contracts::ProjectId,
    required_roles: &BTreeSet<PlatformRole>,
) -> Result<&'a ProjectMembership, AuthorizationError> {
    context
        .project_memberships
        .iter()
        .find(|membership| {
            membership.project_id == project_id
                && membership.actor_id == context.actor.actor_id
                && required_roles.contains(&membership.role)
                && membership.state == MembershipState::Active
                && not_expired(membership.expires_at, context.now)
        })
        .ok_or(AuthorizationError::ProjectScopeDenied)
}

fn not_expired(expiry: Option<UtcTimestamp>, now: OffsetDateTime) -> bool {
    expiry.is_none_or(|value| value.get() > now)
}

fn earliest(actor_expiry: UtcTimestamp, membership_expiry: Option<UtcTimestamp>) -> UtcTimestamp {
    membership_expiry
        .filter(|expiry| expiry.get() < actor_expiry.get())
        .unwrap_or(actor_expiry)
}

fn earliest_optional(
    left: Option<UtcTimestamp>,
    right: Option<UtcTimestamp>,
) -> Option<UtcTimestamp> {
    match (left, right) {
        (Some(left), Some(right)) => Some(if left.get() <= right.get() {
            left
        } else {
            right
        }),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn max_revision(left: Revision, right: Revision) -> Revision {
    if right.get() > left.get() {
        right
    } else {
        left
    }
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
        MembershipState, PlatformRole, ProjectId, ProjectMembership, Revision, UtcTimestamp,
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

    #[test]
    fn environment_scope_expiry_and_revision_include_course_membership()
    -> Result<(), Box<dyn std::error::Error>> {
        let actor_id = ActorId::new();
        let project_id = ProjectId::new();
        let course_id = CourseId::new();
        let actor = AuthenticatedActor {
            actor_id,
            roles: vec![PlatformRole::Student],
            expires_at: timestamp("2026-07-15T00:00:00.000Z")?,
        };
        let context = AuthorizationContext {
            actor,
            course_memberships: vec![CourseMembership {
                course_id,
                actor_id,
                role: PlatformRole::Student,
                state: MembershipState::Active,
                revision: Revision::new(9)?,
                expires_at: Some(timestamp("2026-07-14T02:00:00.000Z")?),
            }],
            project_memberships: vec![ProjectMembership {
                course_id: Some(course_id),
                project_id,
                actor_id,
                role: PlatformRole::Student,
                state: MembershipState::Active,
                revision: Revision::new(4)?,
                expires_at: Some(timestamp("2026-07-14T12:00:00.000Z")?),
            }],
            now: OffsetDateTime::parse(
                "2026-07-14T00:00:00Z",
                &time::format_description::well_known::Rfc3339,
            )?,
        };
        let decision = authorize(
            &context,
            AuthorizationScope::Environment {
                project_id,
                course_id: Some(course_id),
                environment_id: contracts::EnvironmentId::new(),
                environment_revision: Revision::new(3)?,
            },
            &BTreeSet::from([PlatformRole::Student]),
        )?;
        assert_eq!(decision.authorization_revision.get(), 9);
        assert_eq!(decision.scope_revision.get(), 9);
        assert_eq!(decision.valid_until, timestamp("2026-07-14T02:00:00.000Z")?);
        Ok(())
    }

    #[test]
    fn environment_scope_rejects_cross_project_without_course_context()
    -> Result<(), Box<dyn std::error::Error>> {
        let actor_id = ActorId::new();
        let member_project = ProjectId::new();
        let requested_project = ProjectId::new();
        let actor = AuthenticatedActor {
            actor_id,
            roles: vec![PlatformRole::Student],
            expires_at: timestamp("2026-07-15T00:00:00.000Z")?,
        };
        let context = AuthorizationContext {
            actor,
            course_memberships: Vec::new(),
            project_memberships: vec![ProjectMembership {
                course_id: None,
                project_id: member_project,
                actor_id,
                role: PlatformRole::Student,
                state: MembershipState::Active,
                revision: Revision::new(2)?,
                expires_at: None,
            }],
            now: OffsetDateTime::parse(
                "2026-07-14T00:00:00Z",
                &time::format_description::well_known::Rfc3339,
            )?,
        };
        let result = authorize(
            &context,
            AuthorizationScope::Environment {
                project_id: requested_project,
                course_id: None,
                environment_id: contracts::EnvironmentId::new(),
                environment_revision: Revision::new(1)?,
            },
            &BTreeSet::from([PlatformRole::Student]),
        );
        assert_eq!(result, Err(super::AuthorizationError::ProjectScopeDenied));
        Ok(())
    }
}
