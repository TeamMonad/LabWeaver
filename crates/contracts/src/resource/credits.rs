//! Credits use the existing six-place fixed decimal. Amounts are never floats.
use super::FixedDecimal;
use crate::{
    ActorId, ChargeId, ProjectId, RateId, ResourceRequestId, Revision, UsageRecordId, UtcTimestamp,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreditAccount {
    pub project_id: ProjectId,
    pub currency: String,
    pub balance: FixedDecimal,
    pub available: FixedDecimal,
    pub reserved: FixedDecimal,
    pub spent: FixedDecimal,
    pub pending: FixedDecimal,
    pub unknown: FixedDecimal,
    pub unknown_count: u64,
    pub platform_absorbed: FixedDecimal,
    pub revision: Revision,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CreditHoldState {
    Pending,
    Unknown,
    Settled,
    Released,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreditHold {
    #[schemars(with = "String", extend("format" = "uuid"))]
    pub id: Uuid,
    pub project_id: ProjectId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<ResourceRequestId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_request_id: Option<crate::BuildRequestId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_attempt: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>", extend("format" = "uuid"))]
    pub model_call_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_rate_snapshot: Option<Vec<crate::resource::model_costing::ModelRateSnapshot>>,
    pub business_key: String,
    pub owner_client_id: String,
    pub amount: FixedDecimal,
    pub remaining: FixedDecimal,
    pub state: CreditHoldState,
    pub measurement_unknown: bool,
    pub reason: Option<String>,
    pub created_at: UtcTimestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unknown_since: Option<UtcTimestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reconciliation_due_at: Option<UtcTimestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expired_unknown_at: Option<UtcTimestamp>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreditHoldRequest {
    pub project_id: ProjectId,
    pub request_id: ResourceRequestId,
    pub business_key: String,
    pub amount: FixedDecimal,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(
    tag = "action",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum CreditHoldResolutionRequest {
    Settle {
        event_id: crate::EventId,
        amount: FixedDecimal,
        finalize: bool,
    },
    Release {
        reason: String,
    },
    Unknown {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        event_id: Option<crate::EventId>,
        reason: String,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(
    tag = "action",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum CreditAdminOperation {
    Grant {
        project_id: ProjectId,
        amount: FixedDecimal,
        reason: String,
    },
    Transfer {
        project_id: ProjectId,
        destination_project_id: ProjectId,
        amount: FixedDecimal,
        reason: String,
    },
    Adjust {
        project_id: ProjectId,
        amount: FixedDecimal,
        reason: String,
    },
    Reverse {
        #[schemars(with = "String", extend("format" = "uuid"))]
        entry_id: Uuid,
        reason: String,
    },
}

/// Immutable rate identity used by a Credits settlement ledger entry.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreditRateSnapshot {
    pub rate_id: RateId,
    pub rate_revision: Revision,
}

/// Platform entries have no project ID and balance the project entries.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreditLedgerEntry {
    #[schemars(with = "String", extend("format" = "uuid"))]
    pub id: Uuid,
    #[schemars(with = "String", extend("format" = "uuid"))]
    pub operation_id: Uuid,
    pub project_id: Option<ProjectId>,
    #[schemars(with = "Option<String>", extend("format" = "uuid"))]
    pub hold_id: Option<Uuid>,
    #[schemars(with = "Option<String>", extend("format" = "uuid"))]
    pub reversal_of: Option<Uuid>,
    pub kind: String,
    pub source_event_id: Option<crate::EventId>,
    pub charge_id: Option<ChargeId>,
    pub usage_record_id: Option<UsageRecordId>,
    pub rate_snapshots: Vec<CreditRateSnapshot>,
    pub balance_delta: FixedDecimal,
    pub reserved_delta: FixedDecimal,
    pub spent_delta: FixedDecimal,
    pub platform_absorbed: FixedDecimal,
    pub reason: String,
    pub actor_id: Option<ActorId>,
    pub owner_client_id: Option<String>,
    pub created_at: UtcTimestamp,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreditLedgerPage {
    pub items: Vec<CreditLedgerEntry>,
    pub page: u32,
    pub page_size: u16,
    pub has_more: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreditHoldPage {
    pub items: Vec<CreditHold>,
    pub page: u32,
    pub page_size: u16,
    pub has_more: bool,
}
