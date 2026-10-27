//! Control-owned course responsibilities and explicit account invitations.

use crate::object_authorization::{ObjectAction, ProjectObjectRole};
use crate::{
    ActorId, CourseId, CourseMembership, MembershipInvitationId, ProjectId, Revision, UtcTimestamp,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
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

/// Stable pages of current course membership facts.
pub type CourseMembershipPage = crate::http::CursorPage<CourseMembershipView>;
pub type MembershipInvitationPage = crate::http::CursorPage<MembershipInvitation>;

/// A course's authoritative schedule and late-submission policy.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CourseCalendar {
    pub course_id: CourseId,
    pub revision: Revision,
    pub open_at: Option<UtcTimestamp>,
    pub due_at: UtcTimestamp,
    pub time_zone: String,
    pub state: CourseCalendarState,
    pub late_policy: CourseLatePolicy,
    pub late_until: Option<UtcTimestamp>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CourseCalendarState {
    NotOpen,
    Open,
    Late,
    Closed,
}

impl CourseCalendar {
    #[must_use]
    pub fn state_at(&self, now: UtcTimestamp) -> CourseCalendarState {
        if self.open_at.is_some_and(|open| now < open) {
            return CourseCalendarState::NotOpen;
        }
        if now < self.due_at {
            return CourseCalendarState::Open;
        }
        match self.late_policy {
            CourseLatePolicy::AllowWithMark if self.late_until.is_none_or(|until| now < until) => {
                CourseCalendarState::Late
            }
            CourseLatePolicy::HardClose | CourseLatePolicy::AllowWithMark => {
                CourseCalendarState::Closed
            }
        }
    }
}

/// Submission behavior at and after the due instant.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CourseLatePolicy {
    HardClose,
    AllowWithMark,
}
impl CourseLatePolicy {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HardClose => "hard_close",
            Self::AllowWithMark => "allow_with_mark",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateCourseCalendar {
    pub open_at: Option<UtcTimestamp>,
    pub due_at: UtcTimestamp,
    pub time_zone: String,
    pub late_policy: CourseLatePolicy,
    pub late_until: Option<UtcTimestamp>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CourseRosterImportRequest {
    /// UTF-8 CSV with header `campusUsername,role`; role is optional.
    pub csv: String,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedCourseRosterImportRequest {
    pub rows: Vec<ResolvedCourseRosterRow>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedCourseRosterRow {
    pub row_number: u32,
    pub campus_username: String,
    pub actor_id: ActorId,
    pub role: CourseRosterRole,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CourseRosterRole {
    Participant,
    TeachingAssistant,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CourseRosterImportResult {
    pub course_id: CourseId,
    pub revision: Revision,
    pub rows: Vec<CourseRosterImportRowResult>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CourseRosterImportRowResult {
    pub row_number: u32,
    pub campus_username: String,
    pub role: Option<CourseRosterRole>,
    pub result: CourseRosterImportRowState,
    pub actor_id: Option<ActorId>,
    pub invitation_id: Option<MembershipInvitationId>,
    pub diagnostic_code: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CourseRosterImportRowState {
    Imported,
    AlreadyMember,
    Invited,
    DuplicateInFile,
    UnknownAccount,
    InvalidRole,
    InvalidRow,
    Rejected,
}

/// Student-facing course facts. It contains no other member's identity.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CourseStudentView {
    pub calendar: CourseCalendar,
    pub membership: CourseMembership,
}
pub type ParsedCourseRosterRow = (u32, String, Option<String>);

/// Parses the deliberately narrow CSV roster format without silently ignoring
/// malformed columns. Quoted fields are supported; escaped quotes use RFC4180
/// doubling. The caller resolves campus usernames through the issuer directory.
pub fn parse_course_roster_csv(csv: &str) -> Result<Vec<ParsedCourseRosterRow>, &'static str> {
    let mut lines = csv.lines();
    let header = lines.next().ok_or("LW_COURSE_ROSTER_IMPORT_INVALID")?;
    let columns = parse_csv_line(header)?;
    if columns.is_empty()
        || columns.len() > 2
        || columns[0].trim() != "campusUsername"
        || (columns.len() == 2 && columns[1].trim() != "role")
    {
        return Err("LW_COURSE_ROSTER_IMPORT_INVALID");
    }
    let mut rows = Vec::new();
    for (offset, line) in lines.enumerate() {
        let row_number =
            u32::try_from(offset + 2).map_err(|_| "LW_COURSE_ROSTER_IMPORT_INVALID")?;
        let fields = parse_csv_line(line)?;
        if fields.len() != columns.len() {
            return Err("LW_COURSE_ROSTER_ROW_INVALID");
        }
        let username = fields[0].trim().to_owned();
        if username.is_empty() || username.chars().any(char::is_control) {
            return Err("LW_COURSE_ROSTER_ROW_INVALID");
        }
        let role = if fields.len() == 1 {
            None
        } else {
            match fields[1].trim() {
                "" => None,
                value if value == "participant" || value == "teaching_assistant" => {
                    Some(value.to_owned())
                }
                _ => return Err("LW_COURSE_ROSTER_ROLE_INVALID"),
            }
        };
        rows.push((row_number, username, role));
    }
    Ok(rows)
}

fn parse_csv_line(line: &str) -> Result<Vec<String>, &'static str> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut chars = line.chars().peekable();
    let mut quoted = false;
    while let Some(ch) = chars.next() {
        match (quoted, ch) {
            (false, '"') if field.is_empty() => quoted = true,
            (true, '"') if chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            (true, '"') => quoted = false,
            (false, ',') => {
                fields.push(field.trim().to_owned());
                field.clear();
            }
            (_, ch) => field.push(ch),
        }
    }
    if quoted {
        return Err("LW_COURSE_ROSTER_ROW_INVALID");
    }
    fields.push(field.trim().to_owned());
    Ok(fields)
}
pub type ProjectPage = crate::http::CursorPage<crate::Project>;
pub type ProjectMembershipPage = crate::http::CursorPage<crate::ProjectMembership>;

/// Control lists use opaque scope-bound keyset cursors, never offsets.
#[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ControlListQuery {
    pub cursor: Option<String>,
    pub limit: Option<u16>,
}

impl ControlListQuery {
    pub fn validate(&self) -> Result<(), crate::http::HttpContractError> {
        crate::http::validate_cursor_page(self.cursor.as_deref(), self.limit)
    }
}

#[must_use]
pub fn course_governance_operation_action(operation: &str) -> Option<ObjectAction> {
    match operation {
        "listCourseMemberships" | "getCourseCalendar" | "getCourseStudentView" => {
            Some(ObjectAction::Browse)
        }
        "createCourseMembershipInvitation"
        | "updateCourseCalendar"
        | "importCourseRoster"
        | "removeCourseMembership" => Some(ObjectAction::ManageMembers),
        _ => None,
    }
}
