//! Model token pricing, preauthorization, and settlement contracts.
use super::credits::CreditHold;
use super::{FixedDecimal, ResourceChargeLine};
use crate::{EventId, ProjectId, ResourceRequestId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelTokenQuantities {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

impl ModelTokenQuantities {
    pub fn validate(&self) -> Result<(), super::ResourceError> {
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelCostEstimateRequest {
    pub request_id: ResourceRequestId,
    pub model_binding: String,
    pub quantities: ModelTokenQuantities,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelCostEstimate {
    pub request_id: ResourceRequestId,
    pub project_id: ProjectId,
    pub model_binding: String,
    pub lines: Vec<ResourceChargeLine>,
    pub total: super::Money,
    pub max_preauthorization: FixedDecimal,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelCostAdmissionRequest {
    pub request_id: ResourceRequestId,
    pub model_binding: String,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelCostAdmission {
    pub hold: CreditHold,
    pub estimate: ModelCostEstimate,
}
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelCostSettlementRequest {
    #[schemars(with = "String", extend("format" = "uuid"))]
    pub hold_id: uuid::Uuid,
    pub event_id: EventId,
    pub model_binding: String,
    pub measurement: ModelUsageMeasurement,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModelUsageMeasurement {
    Known { quantities: ModelTokenQuantities },
    Unknown { reason: String },
}
