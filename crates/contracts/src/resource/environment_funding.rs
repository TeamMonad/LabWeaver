//! One original Environment lease and hold, with immutable pricing windows.
use super::{costing::ResourceCostEstimate, credits::CreditHold};
use crate::{
    CapacityClaimId, LeaseId, ResourceApprovalId, ResourceRequestId, Revision, UtcTimestamp,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Financial facts for the configured Environment owner; this is not a second runtime grant.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentCreditGrant {
    pub request_id: ResourceRequestId,
    pub lease_id: LeaseId,
    pub lease_revision: Revision,
    pub claim_id: CapacityClaimId,
    pub approval_id: ResourceApprovalId,
    /// Start of the continuous total coverage, unchanged by renewals.
    pub covered_from: UtcTimestamp,
    /// End of the continuous total coverage after the latest funded renewal.
    pub covered_until: UtcTimestamp,
    pub hold: CreditHold,
    /// The most recently funded initial or additional interval, with original rate versions.
    pub estimate: ResourceCostEstimate,
    /// Only a current active lease and known, unexpired original funds authorize new execution.
    pub can_authorize_execution: bool,
}
