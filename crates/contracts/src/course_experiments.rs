//! Course experiment policy and explicit group payer bindings.
use crate::{
    ActorId, CourseExperimentId, CourseId, ExperimentGroupId, ProjectId, ReleaseId, Revision,
    UtcTimestamp,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperimentPolicy {
    Individual,
    Group,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CourseExperiment {
    pub id: CourseExperimentId,
    pub course_id: CourseId,
    pub release_id: ReleaseId,
    pub policy: ExperimentPolicy,
    pub revision: Revision,
    pub created_at: UtcTimestamp,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateCourseExperimentRequest {
    pub release_id: ReleaseId,
    pub policy: ExperimentPolicy,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExperimentGroup {
    pub id: ExperimentGroupId,
    pub experiment_id: CourseExperimentId,
    pub payer_project_id: ProjectId,
    pub payer_actor_id: ActorId,
    pub member_actor_ids: Vec<ActorId>,
    pub payer_consent: bool,
    pub revision: Revision,
    pub created_at: UtcTimestamp,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateExperimentGroupRequest {
    pub payer_project_id: ProjectId,
    pub payer_actor_id: ActorId,
    pub member_actor_ids: Vec<ActorId>,
    pub payer_consent: bool,
}

impl CreateExperimentGroupRequest {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !self.payer_consent
            || self.member_actor_ids.is_empty()
            || self.member_actor_ids.len() > 100
            || self.member_actor_ids.iter().any(|id| id.as_uuid().is_nil())
            || self.payer_actor_id.as_uuid().is_nil()
            || !self.member_actor_ids.contains(&self.payer_actor_id)
        {
            return Err("LW_COURSE_EXPERIMENT_PAYER_CONSENT_REQUIRED");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CourseExperimentGroupView {
    pub experiment: CourseExperiment,
    pub group: ExperimentGroup,
}

pub const CALENDAR_NOT_OPEN: &str = "LW_COURSE_EXPERIMENT_NOT_OPEN";
pub const CALENDAR_CLOSED: &str = "LW_COURSE_EXPERIMENT_CLOSED";
pub const CALENDAR_LATE: &str = "LW_COURSE_EXPERIMENT_LATE";
