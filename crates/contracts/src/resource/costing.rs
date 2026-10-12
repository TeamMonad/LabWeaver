//! Trusted Resource estimates reuse versioned rates and charge lines.
use super::credits::CreditHold;
use super::{Money, ResourceChargeLine, ResourceError, WorkloadResources};
use crate::{ProjectId, ResourceRequestId, UtcTimestamp};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Half-open compute and storage intervals are independent, even when they overlap.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceCostEstimateRequest {
    pub resources: WorkloadResources,
    pub compute_from: UtcTimestamp,
    pub compute_until: UtcTimestamp,
    pub storage_from: UtcTimestamp,
    pub storage_until: UtcTimestamp,
}
impl ResourceCostEstimateRequest {
    pub fn validate(&self) -> Result<(), ResourceError> {
        self.resources.validate()?;
        if self.compute_until <= self.compute_from || self.storage_until <= self.storage_from {
            return Err(ResourceError::InvalidUsage);
        }
        Ok(())
    }
}

/// Calculated lines retain the exact rate IDs, revisions, prices and quantity divisors.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceCostEstimate {
    pub project_id: ProjectId,
    pub input: ResourceCostEstimateRequest,
    pub lines: Vec<ResourceChargeLine>,
    pub total: Money,
}

/// No caller-supplied debit amount is accepted. Resource checks the consent snapshot and scope.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EstimatedCreditHoldRequest {
    pub request_id: ResourceRequestId,
    pub expected_estimate: ResourceCostEstimate,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EstimatedCreditHold {
    pub hold: CreditHold,
    pub estimate: ResourceCostEstimate,
}

/// Confirmed unknown-usage reconciliation deadline, measured from the first unknown fact.
pub const UNKNOWN_USAGE_RECONCILIATION_SECONDS: u32 = 3600;
