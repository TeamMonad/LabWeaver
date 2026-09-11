//! Project ownership contracts shared by Control and Access.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{ActorId, CourseId, ProjectId, Revision, UtcTimestamp};

/// Durable lifecycle of a project. Archiving preserves independent research work.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectState {
    Active,
    Archived,
}

/// Control-owned project aggregate and the single source of project metadata.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Project {
    pub id: ProjectId,
    pub owner_actor_id: ActorId,
    pub name: String,
    pub description: Option<String>,
    pub course_id: Option<CourseId>,
    pub state: ProjectState,
    pub revision: Revision,
    pub created_at: UtcTimestamp,
    pub updated_at: UtcTimestamp,
}

impl Project {
    pub fn validate(&self) -> Result<(), ProjectError> {
        if self.name.trim().is_empty()
            || self.name.chars().count() > 120
            || self.name.chars().any(char::is_control)
            || self.description.as_ref().is_some_and(|description| {
                description.chars().count() > 2_000 || description.chars().any(char::is_control)
            })
            || self.revision.get() == 0
            || self.updated_at < self.created_at
        {
            return Err(ProjectError::Invalid);
        }
        Ok(())
    }
}

/// Browser request for a new independent or course-associated project.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateProjectRequest {
    pub name: String,
    pub description: Option<String>,
    pub course_id: Option<CourseId>,
}

/// Revision-fenced project metadata mutation.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateProjectRequest {
    pub expected_revision: Revision,
    pub name: String,
    pub description: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ProjectError {
    #[error("invalid project")]
    Invalid,
}
