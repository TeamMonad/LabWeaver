//! Control-owned object permissions, independent of the issuer's platform role.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Existing Access session termination deadline, now shared by all member revocation consumers.
pub const MEMBER_REVOCATION_BOUND_SECONDS: u64 = 60;
/// Interval between current authorization checks, independent of event delivery.
pub const AUTHORIZATION_RECHECK_SECONDS: u64 = 1;
/// Maximum time spent reading current authorization before a connection fails closed.
pub const AUTHORIZATION_CHECK_TIMEOUT_SECONDS: u64 = 5;

/// An operation on an object. Platform identity alone does not grant these operations.
#[derive(
    Clone, Copy, Debug, Deserialize, Eq, JsonSchema, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ObjectAction {
    Browse,
    Archive,
    Edit,
    Start,
    Stop,
    ConfigureAgent,
    GrantAccess,
    ManageMembers,
    ViewCosts,
    RequestRuntimePermissions,
    ApproveRuntimePermissions,
    ManageRuntimePolicy,
}

/// Responsibilities granted by Control in one project, independent of Teacher/Student identity.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectObjectRole {
    #[default]
    Viewer,
    Operator,
    Editor,
    Manager,
    Owner,
}

impl ProjectObjectRole {
    /// Returns this role's explicit actions. Platform policy management is always administrator-only.
    #[must_use]
    pub const fn actions(self) -> &'static [ObjectAction] {
        use ObjectAction::{
            Archive, Browse, ConfigureAgent, Edit, GrantAccess, ManageMembers,
            RequestRuntimePermissions, Start, Stop, ViewCosts,
        };
        match self {
            Self::Viewer => &[Browse],
            Self::Operator => &[Browse, Start, Stop],
            Self::Editor => &[
                Browse,
                Edit,
                Start,
                Stop,
                ConfigureAgent,
                GrantAccess,
                ViewCosts,
                RequestRuntimePermissions,
            ],
            Self::Manager => &[
                Browse,
                Edit,
                Start,
                Stop,
                ConfigureAgent,
                GrantAccess,
                ManageMembers,
                ViewCosts,
                RequestRuntimePermissions,
            ],
            Self::Owner => &[
                Browse,
                Edit,
                Start,
                Stop,
                ConfigureAgent,
                GrantAccess,
                ManageMembers,
                ViewCosts,
                RequestRuntimePermissions,
                Archive,
            ],
        }
    }

    /// Stable storage representation used by the Control member authority.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Viewer => "viewer",
            Self::Operator => "operator",
            Self::Editor => "editor",
            Self::Manager => "manager",
            Self::Owner => "owner",
        }
    }
}

/// Current actions for the authenticated actor. It is advisory and never replaces a write check.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AvailableObjectActions {
    pub project_id: crate::ProjectId,
    pub actor_id: crate::ActorId,
    pub membership_revision: crate::Revision,
    pub actions: Vec<ObjectAction>,
    pub valid_until: crate::UtcTimestamp,
}

/// Maps current project API operations to their object actions.
/// Unknown operations retain their existing policy and cannot receive a guessed permission.
#[must_use]
pub fn project_operation_action(operation: &str) -> Option<ObjectAction> {
    use ObjectAction::{Archive, Browse, ConfigureAgent, Edit, ManageMembers, ViewCosts};
    match operation {
        "getResourceRuntimeAuthorization"
        | "listEnvironments"
        | "getProject"
        | "streamProjectEvents"
        | "listProjectMemberships"
        | "getProjectAvailableActions"
        | "getProjectAgentRun"
        | "listProjectAgentRuns"
        | "getProjectWorkConfigurationPlan"
        | "getProjectProblemPackage"
        | "getActiveProjectLlmPolicy"
        | "getProjectLlmPolicyOptions"
        | "getProjectAgentCapability"
        | "getProjectRubricModelInputRules"
        | "getFrozenSubmission"
        | "getProjectEnvironmentCandidate"
        | "getProjectEvaluationCandidate"
        | "getProjectCandidateBuild"
        | "getProjectAuthoringApproval"
        | "listProjectTemplateImages"
        | "listEnvironmentTemplateReleases"
        | "getEnvironmentTemplateRelease"
        | "listProjectResourceRequests"
        | "listProjectResourceLeases" => Some(Browse),
        "archiveProject" => Some(Archive),
        "createResourceRequest"
        | "createProjectResourceRequest"
        | "createEnvironment"
        | "freezeSubmission"
        | "updateProject"
        | "createProjectProblemPackageUpload"
        | "completeProjectProblemPackageUpload"
        | "getProjectProblemPackageUpload"
        | "cancelProjectProblemPackageUpload"
        | "createProjectLlmPolicy"
        | "maintainProjectRubricModelInputRule"
        | "appendProjectEnvironmentCandidateDecision"
        | "appendProjectEvaluationCandidateDecision"
        | "completeProjectAuthoringApproval"
        | "createEnvironmentTemplateRelease"
        | "withdrawEnvironmentTemplateRelease" => Some(Edit),
        "upsertProjectResourceBudget"
        | "addProjectMembership"
        | "removeProjectMembership"
        | "createProjectMembershipInvitation"
        | "listProjectMembershipInvitations" => Some(ManageMembers),
        "createProjectAgentRun"
        | "createProjectWorkConfigurationRun"
        | "approveProjectWorkConfigurationRun"
        | "cancelProjectAgentRun"
        | "retryProjectAgentRunTrack"
        | "cancelProjectCandidateBuild" => Some(ConfigureAgent),
        "getProjectResourceBudget" | "listProjectResourceUsage" | "listProjectResourceCharges" => {
            Some(ViewCosts)
        }
        "cancelResourceRequest" => Some(ObjectAction::Stop),
        "renewResourceLease" => Some(ObjectAction::Start),
        _ => None,
    }
}

/// Current environment route actions. The Environment owner fence applies independently.
#[must_use]
pub fn environment_operation_action(operation: &str) -> Option<ObjectAction> {
    use ObjectAction::{Browse, Edit, Start, Stop};
    match operation {
        "getEnvironment"
        | "getEnvironmentOperation"
        | "listEnvironmentOperations"
        | "listEnvironmentEndpoints" => Some(Browse),
        "startEnvironment" | "restartEnvironment" | "upgradeEnvironmentTemplate" => Some(Start),
        "stopEnvironment" | "cancelEnvironmentOperation" => Some(Stop),
        "resetEnvironment" | "retryEnvironment" | "recoverEnvironment" | "deleteEnvironment" => {
            Some(Edit)
        }
        _ => None,
    }
}
