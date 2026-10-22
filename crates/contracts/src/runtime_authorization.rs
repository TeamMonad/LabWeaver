//! Runtime authorization identities attached to existing Control and Resource approvals.
use crate::authoring::EnvironmentSpec;
use crate::platform_configuration::{
    PlatformConfigurationAppliedState, PlatformConfigurationStatus, PlatformRuntimePolicy,
    RuntimePreauthorization, RuntimePreauthorizationScope,
};
use crate::resource::{ResourceRequest, ResourceTarget};
use crate::{ActorId, ApprovalId, CandidateId, CourseId, ProjectId, Revision};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Captured by Control while the verified teacher and current course instructor are checked.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CourseTeacherApprovalScope {
    pub actor_id: ActorId,
    pub course_id: CourseId,
    pub membership_revision: Revision,
}

/// Control-owned immutable configuration identity. Clients cannot supply a replacement digest.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeConfigurationIdentity {
    pub authoring_project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub candidate_id: CandidateId,
    pub candidate_revision: Revision,
    pub configuration_sha256: String,
    pub authoring_approval_id: Option<ApprovalId>,
    pub teacher_scope: Option<CourseTeacherApprovalScope>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeAdmissionBasis {
    RestrictedBaseline,
    ProjectPreauthorization,
    CoursePreauthorization,
}

/// Part of the existing immutable ResourceApproval, with no independent approval lifecycle.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeAuthorizationBinding {
    pub request_id: crate::ResourceRequestId,
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub request_generation: u64,
    pub target: ResourceTarget,
    pub configuration: RuntimeConfigurationIdentity,
    /// This is the verified applied platform version, never the candidate's LLM policy revision.
    pub platform_configuration_revision: Revision,
    pub platform_configuration_sha256: String,
    pub permissions: RuntimePreauthorization,
    pub basis: RuntimeAdmissionBasis,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimePolicyUse {
    Admission,
    StopOrRecovery,
}

#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub enum RuntimeAuthorizationError {
    #[error("LW_RUNTIME_POLICY_NOT_VERIFIED")]
    PolicyNotVerified,
    #[error("LW_RUNTIME_POLICY_CHANGED")]
    PolicyChanged,
    #[error("LW_RUNTIME_CONFIGURATION_BINDING_MISMATCH")]
    ConfigurationMismatch,
    #[error("LW_RUNTIME_TEACHER_APPROVAL_REQUIRED")]
    TeacherApprovalRequired,
    #[error("LW_RUNTIME_PERMISSION_OUTSIDE_PLATFORM_RANGE")]
    OutsidePlatformRange,
    #[error("LW_RUNTIME_PERMISSION_APPROVAL_REQUIRED")]
    ApprovalRequired,
    #[error("LW_RUNTIME_TARGET_NOT_ENVIRONMENT")]
    TargetNotEnvironment,
}

/// Stopping and recovery do not mint runtime rights and remain available during an uncertain apply.
pub fn check_policy_use(
    status: &PlatformConfigurationStatus,
    usage: RuntimePolicyUse,
) -> Result<(), RuntimeAuthorizationError> {
    if usage == RuntimePolicyUse::StopOrRecovery {
        return Ok(());
    }
    if status.applied_state != PlatformConfigurationAppliedState::Verified
        || status.applied.is_none()
    {
        return Err(RuntimeAuthorizationError::PolicyNotVerified);
    }
    Ok(())
}

/// Reads the entire current security declaration. Adding fields requires updating this projection.
/// #207 owns that extension. This baseline does not accept extra permission parameters.
pub fn declared_runtime_permissions(
    spec: &EnvironmentSpec,
    project_id: ProjectId,
) -> Result<RuntimePreauthorization, RuntimeAuthorizationError> {
    spec.validate()
        .map_err(|_| RuntimeAuthorizationError::ConfigurationMismatch)?;
    Ok(RuntimePreauthorization {
        scope: RuntimePreauthorizationScope::Project { project_id },
        capabilities: spec.security.capabilities.clone(),
        privileged: spec.security.privileged,
    })
}

pub fn check_platform_range(
    policy: &PlatformRuntimePolicy,
    request: &RuntimePreauthorization,
) -> Result<(), RuntimeAuthorizationError> {
    if (request.privileged
        && (!policy.privileged_allowed
            || policy.privileged_execution_location.is_none()
            || !request.capabilities.is_empty()))
        || !request.capabilities.is_subset(&policy.allowed_capabilities)
    {
        return Err(RuntimeAuthorizationError::OutsidePlatformRange);
    }
    Ok(())
}

/// Selects only a matching project or an approved experiment's actual course preauthorization.
#[must_use]
pub fn select_preauthorization(
    policy: &PlatformRuntimePolicy,
    project_id: ProjectId,
    approved_course: Option<CourseId>,
    request: &RuntimePreauthorization,
) -> Option<RuntimePreauthorization> {
    if request.scope != (RuntimePreauthorizationScope::Project { project_id }) {
        return None;
    }
    policy
        .preauthorizations
        .iter()
        .find(|grant| {
            (grant.scope == RuntimePreauthorizationScope::Project { project_id }
                || approved_course.is_some_and(|course_id| {
                    grant.scope == RuntimePreauthorizationScope::Course { course_id }
                }))
                && request.capabilities.is_subset(&grant.capabilities)
                && (!request.privileged || grant.privileged)
        })
        .map(|grant| RuntimePreauthorization {
            scope: grant.scope.clone(),
            capabilities: request.capabilities.clone(),
            privileged: request.privileged,
        })
}

impl RuntimeAuthorizationBinding {
    /// Checks a stored approval against current server-owned configuration and applied policy facts.
    pub fn validate_current(
        &self,
        request: &ResourceRequest,
        configuration: &RuntimeConfigurationIdentity,
        status: &PlatformConfigurationStatus,
    ) -> Result<(), RuntimeAuthorizationError> {
        check_policy_use(status, RuntimePolicyUse::Admission)?;
        if !matches!(request.target, ResourceTarget::Environment { .. }) {
            return Err(RuntimeAuthorizationError::TargetNotEnvironment);
        }
        if self.request_id != request.id
            || self.project_id != request.project_id
            || self.course_id != request.course_id
            || self.request_generation != request.generation
            || self.target != request.target
            || &self.configuration != configuration
        {
            return Err(RuntimeAuthorizationError::ConfigurationMismatch);
        }
        let applied = status
            .applied
            .as_ref()
            .ok_or(RuntimeAuthorizationError::PolicyNotVerified)?;
        if self.platform_configuration_revision != applied.revision
            || self.platform_configuration_sha256 != applied.configuration_sha256
        {
            return Err(RuntimeAuthorizationError::PolicyChanged);
        }
        check_platform_range(&applied.configuration.runtime_policy, &self.permissions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform_configuration::LinuxCapability;
    use std::collections::BTreeSet;

    #[test]
    fn preauthorization_requires_the_matching_project_or_an_approved_course() {
        let payer = ProjectId::new();
        let other = ProjectId::new();
        let course = CourseId::new();
        let policy = PlatformRuntimePolicy {
            allowed_capabilities: BTreeSet::from([LinuxCapability::NetAdmin]),
            preauthorizations: vec![RuntimePreauthorization {
                scope: RuntimePreauthorizationScope::Course { course_id: course },
                capabilities: BTreeSet::from([LinuxCapability::NetAdmin]),
                privileged: false,
            }],
            ..PlatformRuntimePolicy::default()
        };
        let request = RuntimePreauthorization {
            scope: RuntimePreauthorizationScope::Project { project_id: payer },
            capabilities: BTreeSet::from([LinuxCapability::NetAdmin]),
            privileged: false,
        };
        assert!(check_platform_range(&policy, &request).is_ok());
        assert!(select_preauthorization(&policy, payer, None, &request).is_none());
        assert!(select_preauthorization(&policy, other, Some(CourseId::new()), &request).is_none());
        assert_eq!(
            select_preauthorization(&policy, payer, Some(course), &request)
                .map(|grant| grant.scope),
            Some(RuntimePreauthorizationScope::Course { course_id: course })
        );
        let excess = RuntimePreauthorization {
            capabilities: BTreeSet::from([LinuxCapability::SysAdmin]),
            ..request
        };
        assert_eq!(
            check_platform_range(&policy, &excess),
            Err(RuntimeAuthorizationError::OutsidePlatformRange)
        );
        assert!(select_preauthorization(&policy, payer, Some(course), &excess).is_none());
    }

    #[test]
    fn unknown_capability_and_privileged_without_the_platform_boundary_are_rejected() {
        let project_id = ProjectId::new();
        let invalid = serde_json::json!({"scope":{"kind":"project","projectId":project_id},"capabilities":["NEW_UNKNOWN_CAPABILITY"],"privileged":false});
        assert!(serde_json::from_value::<RuntimePreauthorization>(invalid).is_err());
        let request = RuntimePreauthorization {
            scope: RuntimePreauthorizationScope::Project { project_id },
            capabilities: BTreeSet::new(),
            privileged: true,
        };
        assert_eq!(
            check_platform_range(&PlatformRuntimePolicy::default(), &request),
            Err(RuntimeAuthorizationError::OutsidePlatformRange)
        );
        let policy = PlatformRuntimePolicy {
            privileged_allowed: true,
            ..PlatformRuntimePolicy::default()
        };
        assert_eq!(
            check_platform_range(&policy, &request),
            Err(RuntimeAuthorizationError::OutsidePlatformRange)
        );
    }
}
