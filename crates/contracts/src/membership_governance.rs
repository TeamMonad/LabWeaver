//! Control-owned course responsibilities and explicit account invitations.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::object_authorization::{ObjectAction, ProjectObjectRole};
use crate::{ActorId, CourseId, MembershipInvitationId, ProjectId, Revision, UtcTimestamp};

/// Course responsibility does not change the actor's verified platform identity.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CourseObjectRole {
    Instructor,
    TeachingAssistant,
    #[default]
    Participant,
}

impl CourseObjectRole {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Instructor => "instructor",
            Self::TeachingAssistant => "teaching_assistant",
            Self::Participant => "participant",
        }
    }

    /// Teaching assistants can read the roster. They cannot grant membership or runtime privileges.
    #[must_use]
    pub const fn actions(self) -> &'static [ObjectAction] {
        match self {
            Self::Instructor => &[ObjectAction::Browse, ObjectAction::ManageMembers],
            Self::TeachingAssistant | Self::Participant => &[ObjectAction::Browse],
        }
    }
}

/// Proposed responsibility. Acceptance does not create any unrelated membership.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MembershipInvitationScope {
    Project {
        #[serde(rename = "projectId")]
        project_id: ProjectId,
        #[serde(rename = "objectRole")]
        object_role: ProjectObjectRole,
    },
    Course {
        #[serde(rename = "courseId")]
        course_id: CourseId,
        #[serde(rename = "objectRole")]
        object_role: CourseObjectRole,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MembershipInvitationState {
    Pending,
    Accepted,
    Revoked,
    /// The stored deadline makes a pending invitation unusable without a cleanup worker.
    Expired,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MembershipInvitation {
    pub id: MembershipInvitationId,
    pub scope: MembershipInvitationScope,
    pub recipient_actor_id: ActorId,
    pub created_by: ActorId,
    pub created_at: UtcTimestamp,
    pub expires_at: UtcTimestamp,
    pub membership_expires_at: Option<UtcTimestamp>,
    pub state: MembershipInvitationState,
    pub revision: Revision,
    pub accepted_membership_revision: Option<Revision>,
}

/// The recipient must use this exact campus username. Access resolves it before forwarding.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateProjectMembershipInvitation {
    pub username: String,
    pub object_role: ProjectObjectRole,
    pub expires_at: UtcTimestamp,
    pub membership_expires_at: Option<UtcTimestamp>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateCourseMembershipInvitation {
    pub username: String,
    pub object_role: CourseObjectRole,
    pub expires_at: UtcTimestamp,
    pub membership_expires_at: Option<UtcTimestamp>,
}

/// Internal Control request, supplied after Access verifies governance and resolves the directory.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedMembershipInvitation {
    pub recipient_actor_id: ActorId,
    pub scope: MembershipInvitationScope,
    pub expires_at: UtcTimestamp,
    pub membership_expires_at: Option<UtcTimestamp>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AcceptMembershipInvitation {
    pub expected_revision: Revision,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CourseMembershipView {
    pub membership: crate::CourseMembership,
    pub username: Option<String>,
    pub display_name: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CourseMembershipPage {
    pub items: Vec<CourseMembershipView>,
    pub page: u32,
    pub page_size: u16,
    pub has_more: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MembershipInvitationPage {
    pub items: Vec<MembershipInvitation>,
    pub page: u32,
    pub page_size: u16,
    pub has_more: bool,
}

#[must_use]
pub fn course_governance_operation_action(operation: &str) -> Option<ObjectAction> {
    match operation {
        "listCourseMemberships" => Some(ObjectAction::Browse),
        "createCourseMembershipInvitation"
        | "listCourseMembershipInvitations"
        | "removeCourseMembership" => Some(ObjectAction::ManageMembers),
        _ => None,
    }
}
