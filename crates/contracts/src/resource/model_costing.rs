//! Model token pricing, preauthorization, and settlement contracts.
use super::credits::CreditHold;
use super::{FixedDecimal, ResourceChargeLine};
use crate::{EventId, ProjectId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelTokenQuantities {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelRateSnapshot {
    pub unit: super::ResourceBillingUnit,
    pub rate_id: crate::RateId,
    pub rate_revision: crate::Revision,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelCostEstimateRequest {
    #[schemars(with = "String", extend("format" = "uuid"))]
    pub call_id: Uuid,
    pub project_id: ProjectId,
    pub model_binding: String,
    pub quantities: ModelTokenQuantities,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelCostEstimate {
    #[schemars(with = "String", extend("format" = "uuid"))]
    pub call_id: Uuid,
    pub project_id: ProjectId,
    pub model_binding: String,
    pub lines: Vec<ResourceChargeLine>,
    pub total: super::Money,
    pub max_preauthorization: FixedDecimal,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelCostAdmissionRequest {
    #[schemars(with = "String", extend("format" = "uuid"))]
    pub call_id: Uuid,
    pub project_id: ProjectId,
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
    #[schemars(with = "String", extend("format" = "uuid"))]
    pub call_id: Uuid,
    pub measurement: ModelUsageMeasurement,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(
    tag = "state",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ModelUsageMeasurement {
    Known {
        quantities: ModelTokenQuantities,
    },
    Unknown {
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        observed_quantities: Option<ModelTokenQuantities>,
    },
}

/// Immutable Resource-owned observation and the exact price arithmetic used for reconciliation.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelCostObservation {
    #[schemars(with = "String", extend("format" = "uuid"))]
    pub hold_id: Uuid,
    #[schemars(with = "String", extend("format" = "uuid"))]
    pub call_id: Uuid,
    pub event_id: EventId,
    pub model_binding: String,
    pub measurement: ModelUsageMeasurement,
    pub lines: Vec<ResourceChargeLine>,
    pub observed_total: Option<super::Money>,
    pub charged_amount: FixedDecimal,
}
