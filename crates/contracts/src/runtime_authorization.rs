//! Runtime authorization identities attached to existing Control and Resource approvals.
use crate::authoring::{
    EnvironmentSecuritySpec, EnvironmentSpec, PrivilegeEscalationPolicy, RuntimeUserPolicy,
};
use crate::platform_configuration::{
    PlatformConfigurationAppliedState, PlatformConfigurationStatus, PlatformRuntimePolicy,
    RuntimePreauthorization, RuntimePreauthorizationScope,
};
use crate::resource::{ResourceRequest, ResourceTarget};
use crate::{ActorId, ApprovalId, CourseId, ProjectId, ReleaseId, Revision};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

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
    pub release_id: ReleaseId,
    pub release_version: u64,
    pub configuration_sha256: String,
    /// Absent only on pre-cutover approvals; never authorizes a new compute effect.
    #[serde(default)]
    pub permission_projection_sha256: Option<String>,
    #[serde(default)]
    pub template_family_id: Option<String>,
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
    #[error("LW_RUNTIME_AUTHORIZATION_REAPPROVAL_REQUIRED")]
    ReapprovalRequired,
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
pub fn declared_baseline_permissions(
    spec: &EnvironmentSpec,
    project_id: ProjectId,
) -> Result<RuntimePreauthorization, RuntimeAuthorizationError> {
    let EnvironmentSecuritySpec {
        user_policy: runtime_user_policy,
        root_filesystem_policy: _,
        privilege_escalation_policy,
        public_exposure_policy: _,
        security_profile_binding: _,
    } = &spec.security;
    match (runtime_user_policy, privilege_escalation_policy) {
        (RuntimeUserPolicy::NonRootRequired, PrivilegeEscalationPolicy::Deny) => {}
    }
    spec.validate()
        .map_err(|_| RuntimeAuthorizationError::ConfigurationMismatch)?;
    Ok(RuntimePreauthorization {
        scope: RuntimePreauthorizationScope::Project { project_id },
        capabilities: BTreeSet::new(),
        privileged: false,
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

/// Borrowed, exhaustive permission projection shared by upgrade and fresh admission.
#[derive(Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimePermissionProjection<'a> {
    pub class: crate::authoring::EnvironmentClass,
    pub resources: &'a crate::authoring::ResourceRequirements,
    pub network: &'a crate::authoring::NetworkPolicySpec,
    pub security: &'a EnvironmentSecuritySpec,
    pub entries: &'a [crate::authoring::EnvironmentEntrySpec],
    pub runtime: RuntimePermissionProjectionKind<'a>,
}

#[derive(Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuntimePermissionProjectionKind<'a> {
    Container {
        provider_binding: &'a str,
        service_port: u16,
    },
    VirtualMachine {
        runtime: &'a crate::authoring::EnvironmentRuntimeSpec,
    },
}

impl RuntimeConfigurationIdentity {
    /// Release/content provenance stays immutable; only equal permissions may reuse its grant.
    pub fn validate_compatible(&self, current: &Self) -> Result<(), RuntimeAuthorizationError> {
        let projection = self
            .permission_projection_sha256
            .as_ref()
            .ok_or(RuntimeAuthorizationError::ReapprovalRequired)?;
        if current.permission_projection_sha256.as_ref() != Some(projection)
            || self.template_family_id.is_none()
            || self.template_family_id != current.template_family_id
            || self.authoring_project_id != current.authoring_project_id
            || self.course_id != current.course_id
            || self.teacher_scope != current.teacher_scope
        {
            return Err(RuntimeAuthorizationError::ConfigurationMismatch);
        }
        Ok(())
    }
}
impl RuntimeAuthorizationBinding {
    /// Checks a stored approval against current server-owned configuration and applied policy facts.
    pub fn validate_current(
        &self,
        request: &ResourceRequest,
        configuration: &RuntimeConfigurationIdentity,
        status: &PlatformConfigurationStatus,
    ) -> Result<(), RuntimeAuthorizationError> {
        if self.configuration.permission_projection_sha256.is_none() {
            return Err(RuntimeAuthorizationError::ReapprovalRequired);
        }
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
    fn compatible_projection_preserves_scope_and_rejects_legacy_approval()
    -> Result<(), Box<dyn std::error::Error>> {
        let original = RuntimeConfigurationIdentity {
            authoring_project_id: ProjectId::new(),
            course_id: None,
            release_id: ReleaseId::new(),
            release_version: 1,
            configuration_sha256: "a".repeat(64),
            permission_projection_sha256: Some("b".repeat(64)),
            template_family_id: Some("same-family".into()),
            authoring_approval_id: None,
            teacher_scope: None,
        };
        let mut current = original.clone();
        current.release_id = ReleaseId::new();
        current.release_version = 2;
        current.configuration_sha256 = "c".repeat(64);
        assert!(original.validate_compatible(&current).is_ok());
        for changed in [
            RuntimeConfigurationIdentity {
                permission_projection_sha256: Some("d".repeat(64)),
                ..current.clone()
            },
            RuntimeConfigurationIdentity {
                template_family_id: Some("foreign".into()),
                ..current.clone()
            },
            RuntimeConfigurationIdentity {
                authoring_project_id: ProjectId::new(),
                ..current.clone()
            },
            RuntimeConfigurationIdentity {
                teacher_scope: Some(CourseTeacherApprovalScope {
                    actor_id: ActorId::new(),
                    course_id: CourseId::new(),
                    membership_revision: Revision::new(1)?,
                }),
                ..current.clone()
            },
        ] {
            assert_eq!(
                original.validate_compatible(&changed),
                Err(RuntimeAuthorizationError::ConfigurationMismatch)
            );
        }
        let mut legacy_json = serde_json::to_value(&original)?;
        legacy_json
            .as_object_mut()
            .ok_or("identity")?
            .remove("permissionProjectionSha256");
        let legacy: RuntimeConfigurationIdentity = serde_json::from_value(legacy_json)?;
        assert_eq!(
            legacy.validate_compatible(&current),
            Err(RuntimeAuthorizationError::ReapprovalRequired)
        );
        Ok(())
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
