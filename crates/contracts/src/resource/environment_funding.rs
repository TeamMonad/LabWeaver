//! One original Environment lease, with an independent hold for each restored funding epoch.
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
    /// Start of continuous coverage within the current funding epoch, unchanged by renewals.
    pub covered_from: UtcTimestamp,
    /// End of the continuous total coverage after the latest funded renewal.
    pub covered_until: UtcTimestamp,
    pub hold: CreditHold,
    /// The most recently funded initial or additional interval, with original rate versions.
    pub estimate: ResourceCostEstimate,
    /// Only a current active lease and known, unexpired current-epoch funds authorize execution.
    pub can_authorize_execution: bool,
}

/// Environment requests fresh funding after restoring a retained disk, before starting compute.
/// The restored generation remains stopped; its next compute generation consumes this funding.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RestoreEnvironmentFundingRequest {
    pub lease_id: LeaseId,
    pub environment_id: crate::EnvironmentId,
    pub project_id: crate::ProjectId,
    pub requester_id: crate::ActorId,
    pub restored_generation: u64,
}
