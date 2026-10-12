//! BuildKit charges use observed builder time, not unenforced CPU or memory declarations.
use super::{Money, ResourceChargeLine};
use crate::{BuildRequestId, ProjectId, UtcTimestamp};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Agent supplies only its execution identity; Resource reads the approved build command.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BuildCostAdmissionRequest {
    pub build_request_id: BuildRequestId,
    pub attempt: u32,
    #[schemars(with = "String", extend("format" = "uuid"))]
    pub lease_token: uuid::Uuid,
    pub execution_deadline: UtcTimestamp,
    pub stage_request_id: String,
    pub authorize_execution: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ObserveBuildCostRequest {
    pub build_request_id: BuildRequestId,
    pub attempt: u32,
    #[schemars(with = "String", extend("format" = "uuid"))]
    pub lease_token: uuid::Uuid,
    pub measurement: BuildUsageMeasurement,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BuildCostEstimateRequest {
    pub builder_binding: String,
    pub max_duration_milliseconds: u64,
    pub from: UtcTimestamp,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BuildCostEstimate {
    pub project_id: ProjectId,
    pub input: BuildCostEstimateRequest,
    pub until: UtcTimestamp,
    pub lines: Vec<ResourceChargeLine>,
    pub total: Money,
}

/// A provider history observation. Missing bounds remain unknown.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(
    tag = "state",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum BuildUsageMeasurement {
    Known {
        history_ref: String,
        started_at: UtcTimestamp,
        finished_at: UtcTimestamp,
    },
    Unknown {
        reason: String,
    },
    /// The fenced executor rejected the stage before calling BuildKit.
    NotStarted {
        reason: String,
    },
}

impl BuildUsageMeasurement {
    pub fn validate(&self) -> Result<(), super::ResourceError> {
        match self {
            Self::Known {
                history_ref,
                started_at,
                finished_at,
            } if !history_ref.trim().is_empty() && finished_at > started_at => Ok(()),
            Self::Unknown { reason } | Self::NotStarted { reason } if !reason.trim().is_empty() => {
                Ok(())
            }
            _ => Err(super::ResourceError::InvalidUsage),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BuildCostRecord {
    pub build_request_id: BuildRequestId,
    pub attempt: u32,
    pub execution_authorized: bool,
    pub estimate: BuildCostEstimate,
    pub hold: super::credits::CreditHold,
    pub measurement: Option<BuildUsageMeasurement>,
    pub first_unknown_observation: Option<BuildUsageMeasurement>,
    pub charged_lines: Option<Vec<ResourceChargeLine>>,
    pub charged_total: Option<Money>,
}
