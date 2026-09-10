//! Resource request, capacity claim, and Lease contracts.

use std::fmt::{Display, Formatter};
use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    ActorId, BudgetId, CapacityClaimId, ChargeId, CourseId, EnvironmentId, EventId,
    GpuCatalogEntryId, LeaseId, ProjectId, RateId, ReleaseId, ResourceApprovalId,
    ResourceRequestId, Revision, TaskRunId, UsageRecordId, UtcTimestamp,
};

/// Requested or approved workload resources, independent of Kubernetes quantity syntax.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkloadResources {
    pub cpu_millicores: u32,
    pub memory_bytes: u64,
    pub storage_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu: Option<GpuRequest>,
}

impl WorkloadResources {
    pub fn validate(&self) -> Result<(), ResourceError> {
        if self.cpu_millicores == 0 || self.memory_bytes == 0 || self.storage_bytes == 0 {
            return Err(ResourceError::InvalidResources);
        }
        if let Some(gpu) = &self.gpu {
            gpu.validate()?;
        }
        Ok(())
    }
}

/// A policy-catalogued GPU class. It intentionally does not expose Kubernetes resource names.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GpuRequest {
    pub class: String,
    pub count: u32,
}

impl GpuRequest {
    fn validate(&self) -> Result<(), ResourceError> {
        if self.class.is_empty()
            || self.class.len() > 63
            || !self
                .class
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            || self.class.starts_with('-')
            || self.class.ends_with('-')
            || self.count == 0
        {
            return Err(ResourceError::InvalidResources);
        }
        Ok(())
    }
}

/// Allocation mode selected by the Resource GPU catalog.
///
/// Callers submit only a catalog class and count. The mode is resolved from the
/// active catalog entry and is never accepted as an untrusted request override.
#[derive(
    Clone, Copy, Debug, Deserialize, Eq, JsonSchema, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum GpuAllocationMode {
    Exclusive,
    ContainerTimeSlice,
    VmVgpu,
}

/// Versioned GPU capacity catalog entry owned by Resource Service.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GpuCatalogEntry {
    pub id: GpuCatalogEntryId,
    pub class: String,
    pub mode: GpuAllocationMode,
    pub provider_binding: String,
    pub capacity_units: u32,
    /// Opaque provider mapping resolved by the selected capacity provider.
    pub allocation_binding: String,
    pub revision: Revision,
    pub active: bool,
}

impl GpuCatalogEntry {
    pub fn validate(&self) -> Result<(), ResourceError> {
        let request = GpuRequest {
            class: self.class.clone(),
            count: self.capacity_units,
        };
        request.validate()?;
        if self.provider_binding.trim().is_empty()
            || self.provider_binding.len() > 120
            || self.allocation_binding.trim().is_empty()
            || self.allocation_binding.len() > 256
            || self.revision.get() == 0
            || self.capacity_units == 0
        {
            return Err(ResourceError::InvalidGpuCatalog);
        }
        Ok(())
    }
}

/// The catalog resolution captured on an approved capacity claim.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GpuAllocation {
    /// Exact catalog row used to resolve this allocation.
    pub entry_id: GpuCatalogEntryId,
    pub class: String,
    pub count: u32,
    pub mode: GpuAllocationMode,
    pub provider_binding: String,
    pub allocation_binding: String,
    pub catalog_revision: Revision,
}

impl GpuAllocation {
    pub fn validate(&self) -> Result<(), ResourceError> {
        GpuRequest {
            class: self.class.clone(),
            count: self.count,
        }
        .validate()?;
        if self.provider_binding.trim().is_empty()
            || self.allocation_binding.trim().is_empty()
            || self.catalog_revision.get() == 0
            || (self.mode == GpuAllocationMode::ContainerTimeSlice && self.count != 1)
        {
            return Err(ResourceError::InvalidGpuCatalog);
        }
        Ok(())
    }
}

/// Request lifecycle owned by Resource Service.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceRequestState {
    Reviewing,
    Allocating,
    Active,
    Expiring,
    Expired,
    Rejected,
    Cancelled,
}

impl ResourceRequestState {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Expired | Self::Rejected | Self::Cancelled)
    }
}

/// Closed Lease state consumed by other service boundaries.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceLeaseState {
    Allocating,
    Active,
    Expiring,
    Expired,
    Revoked,
}

/// Immutable identity of a Resource request's target.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ResourceTarget {
    /// Capacity for a long-lived Environment/Work aggregate that already owns its identity.
    Environment {
        environment_id: EnvironmentId,
        release_id: ReleaseId,
        release_version: u64,
    },
    /// Capacity for a one-shot task. Resource does not create a fake Environment row.
    Task { task_run_id: TaskRunId },
}

impl ResourceTarget {
    fn validate(&self) -> Result<(), ResourceError> {
        if let Self::Environment {
            release_version, ..
        } = self
            && *release_version == 0
        {
            return Err(ResourceError::InvalidTarget);
        }
        Ok(())
    }

    #[must_use]
    pub const fn environment_id(&self) -> Option<EnvironmentId> {
        match self {
            Self::Environment { environment_id, .. } => Some(*environment_id),
            Self::Task { .. } => None,
        }
    }

    #[must_use]
    pub const fn release(&self) -> Option<(ReleaseId, u64)> {
        match self {
            Self::Environment {
                release_id,
                release_version,
                ..
            } => Some((*release_id, *release_version)),
            Self::Task { .. } => None,
        }
    }
}

/// PostgreSQL-authoritative request projection without provider internals.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceRequest {
    pub id: ResourceRequestId,
    pub generation: u64,
    pub request_key: String,
    pub requester_id: ActorId,
    pub project_id: ProjectId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub course_id: Option<CourseId>,
    pub target: ResourceTarget,
    pub requested_resources: WorkloadResources,
    pub requested_duration_seconds: u64,
    pub state: ResourceRequestState,
    pub revision: Revision,
    pub created_at: UtcTimestamp,
    pub updated_at: UtcTimestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic_code: Option<String>,
}

impl ResourceRequest {
    pub fn validate(&self) -> Result<(), ResourceError> {
        if self.generation == 0
            || self.request_key.is_empty()
            || self.request_key.len() > 96
            || !self.request_key.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
            })
            || self.requested_duration_seconds == 0
            || self.updated_at < self.created_at
        {
            return Err(ResourceError::InvalidRequest);
        }
        self.target.validate()?;
        self.requested_resources.validate()
    }
}

/// Append-only administrator decision bound to one request revision and policy snapshot.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceApproval {
    pub id: ResourceApprovalId,
    pub request_id: ResourceRequestId,
    pub request_revision: Revision,
    pub approver_id: ActorId,
    pub provider_binding: String,
    pub approved_resources: WorkloadResources,
    pub approved_duration_seconds: u64,
    pub reason: String,
    pub valid_until: UtcTimestamp,
    pub created_at: UtcTimestamp,
}

impl ResourceApproval {
    pub fn validate(&self) -> Result<(), ResourceError> {
        if self.provider_binding.is_empty()
            || self.provider_binding.len() > 120
            || self.reason.trim().is_empty()
            || self.reason.chars().count() > 500
            || self.approved_duration_seconds == 0
            || self.valid_until <= self.created_at
        {
            return Err(ResourceError::InvalidApproval);
        }
        self.approved_resources.validate()
    }
}

/// Exact capacity allocation identity. Handles remain private to the provider implementation.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CapacityClaim {
    pub id: CapacityClaimId,
    pub request_id: ResourceRequestId,
    pub approval_id: ResourceApprovalId,
    pub provider_binding: String,
    pub workload_resources: WorkloadResources,
    pub quota_resources: WorkloadResources,
    /// Exact GPU catalog resolution captured before provider side effects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_allocation: Option<GpuAllocation>,
    pub state: CapacityClaimState,
    pub revision: Revision,
}

/// Provider-owned capacity-shell lifecycle. Resource remains the authority for every transition.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapacityClaimState {
    Reserved,
    Provisioning,
    Ready,
    HandedOff,
    Releasing,
    Released,
    Blocked,
}

impl CapacityClaim {
    /// Private single-university keeps only provider_binding non-empty check;
    /// quota/workload resources still validated but hash/binding coupling is
    /// intentionally loose to allow TTL+PVC mapping without 5-table strictness.
    pub fn validate(&self) -> Result<(), ResourceError> {
        if self.provider_binding.is_empty() || self.provider_binding.len() > 120 {
            return Err(ResourceError::InvalidClaim);
        }
        self.workload_resources.validate()?;
        self.quota_resources.validate()?;
        match (&self.workload_resources.gpu, &self.gpu_allocation) {
            (None, None) => Ok(()),
            (Some(request), Some(allocation))
                if request.class == allocation.class && request.count == allocation.count =>
            {
                allocation.validate()
            }
            _ => Err(ResourceError::InvalidGpuCatalog),
        }
    }
}

/// PostgreSQL-authoritative Lease projection. Its authorization is valid only while Active.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceLease {
    pub id: LeaseId,
    pub request_id: ResourceRequestId,
    pub claim_id: CapacityClaimId,
    pub state: ResourceLeaseState,
    pub revision: Revision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_from: Option<UtcTimestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<UtcTimestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoke_reason_code: Option<String>,
    pub created_at: UtcTimestamp,
    pub updated_at: UtcTimestamp,
}

impl ResourceLease {
    /// Private deployment simplifies Lease window validation: only requires
    /// paired timestamps and `expires_at > active_from` when present.
    /// State-specific `Allocating` vs `Active` windows are not enforced
    /// here because TTL and PVC long-lived bindings are managed via
    /// Experiment TTL + Work PVC directly.
    pub fn validate(&self) -> Result<(), ResourceError> {
        if self.active_from.is_some() != self.expires_at.is_some() {
            return Err(ResourceError::InvalidLease);
        }
        if let Some((from, until)) = self.active_from.zip(self.expires_at)
            && until <= from
        {
            return Err(ResourceError::InvalidLease);
        }
        Ok(())
    }
}

/// Resource-owned Active Lease authorization passed to Environment Service.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceLeaseAuthorization {
    pub lease_id: LeaseId,
    pub lease_revision: Revision,
    pub claim_id: CapacityClaimId,
    pub environment_id: EnvironmentId,
    pub course_id: Option<CourseId>,
    pub owner_actor_id: ActorId,
    pub project_id: ProjectId,
    pub provider_binding: String,
    pub approved_resources: WorkloadResources,
    pub active_from: UtcTimestamp,
    pub expires_at: UtcTimestamp,
}

impl ResourceLeaseAuthorization {
    pub fn validate(&self) -> Result<(), ResourceError> {
        if self.provider_binding.is_empty() || self.active_from >= self.expires_at {
            return Err(ResourceError::InvalidLease);
        }
        self.approved_resources.validate()
    }
}

/// Canonical signed decimal with exactly six fractional digits.
///
/// Decimal values cross the API as strings so JavaScript clients and SQL
/// drivers cannot round monetary values through binary floating point. The
/// internal scaled representation is available to Resource arithmetic without
/// introducing a database-specific decimal dependency into the contracts crate.
#[derive(Clone, Debug, Eq, JsonSchema, Ord, PartialEq, PartialOrd)]
#[schemars(
    with = "String",
    extend("pattern" = r"^-?(0|[1-9][0-9]*)[.][0-9]{6}$", "format" = "fixed-decimal-6")
)]
pub struct FixedDecimal {
    canonical: String,
    scaled_value: i128,
}

impl FixedDecimal {
    pub const SCALE: i128 = 1_000_000;

    pub fn parse(value: &str) -> Result<Self, ResourceError> {
        let (negative, unsigned) = value
            .strip_prefix('-')
            .map_or((false, value), |value| (true, value));
        let Some((whole, fractional)) = unsigned.split_once('.') else {
            return Err(ResourceError::InvalidDecimal);
        };
        if whole.is_empty()
            || fractional.len() != 6
            || !whole.bytes().all(|byte| byte.is_ascii_digit())
            || !fractional.bytes().all(|byte| byte.is_ascii_digit())
            || (whole.len() > 1 && whole.starts_with('0'))
            || (negative && whole == "0" && fractional == "000000")
        {
            return Err(ResourceError::InvalidDecimal);
        }
        let scaled_whole = whole
            .parse::<i128>()
            .map_err(|_| ResourceError::InvalidDecimal)?
            .checked_mul(Self::SCALE)
            .ok_or(ResourceError::InvalidDecimal)?;
        let scaled_fraction = fractional
            .parse::<i128>()
            .map_err(|_| ResourceError::InvalidDecimal)?;
        let scaled = scaled_whole
            .checked_add(scaled_fraction)
            .ok_or(ResourceError::InvalidDecimal)?;
        let scaled = if negative { -scaled } else { scaled };
        Self::from_scaled(scaled)
    }

    pub fn from_scaled(value: i128) -> Result<Self, ResourceError> {
        let scaled_value = value;
        let negative = value < 0;
        let magnitude = value.unsigned_abs();
        let whole = magnitude / Self::SCALE as u128;
        let fractional = magnitude % Self::SCALE as u128;
        // Keep every value representable by `scaled` so a decoded charge cannot
        // panic while checking arithmetic.  `i128::MIN` has an unsigned
        // magnitude one larger than `i128::MAX` and must therefore be rejected.
        if whole > i128::MAX as u128 / Self::SCALE as u128
            || (whole == i128::MAX as u128 / Self::SCALE as u128
                && fractional > i128::MAX as u128 % Self::SCALE as u128)
        {
            return Err(ResourceError::InvalidDecimal);
        }
        let value = if negative {
            format!("-{whole}.{fractional:06}")
        } else {
            format!("{whole}.{fractional:06}")
        };
        Ok(Self {
            canonical: value,
            scaled_value,
        })
    }

    #[must_use]
    pub fn zero() -> Self {
        Self {
            canonical: "0.000000".to_owned(),
            scaled_value: 0,
        }
    }

    #[must_use]
    pub fn scaled(&self) -> i128 {
        self.scaled_value
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.canonical
    }
}

impl Display for FixedDecimal {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.canonical)
    }
}

impl FromStr for FixedDecimal {
    type Err = ResourceError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl Serialize for FixedDecimal {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.canonical)
    }
}

impl<'de> Deserialize<'de> for FixedDecimal {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

/// A fixed-precision amount with an explicit ISO-4217-like currency code.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Money {
    pub currency: String,
    pub amount: FixedDecimal,
}

impl Money {
    pub fn validate(&self) -> Result<(), ResourceError> {
        if self.currency.is_empty()
            || self.currency.len() > 32
            || !self
                .currency
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            return Err(ResourceError::InvalidMoney);
        }
        Ok(())
    }
}

/// Resource dimension used by a versioned rate card.
#[derive(
    Clone, Copy, Debug, Deserialize, Eq, JsonSchema, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ResourceBillingUnit {
    CpuMillicoreSecond,
    MemoryByteSecond,
    StorageByteSecond,
    GpuUnitSecond,
}

/// Metering scope for one usage interval.
///
/// Compute and retained-storage intervals are independent meters and may overlap in time.
/// A known record must set non-applicable dimensions to zero so a stored interval cannot be
/// interpreted as both compute and storage usage.
#[derive(
    Clone, Copy, Debug, Deserialize, Eq, JsonSchema, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ResourceUsageKind {
    Compute,
    Storage,
}

/// Immutable versioned unit price. A later rate never mutates a prior charge.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceRate {
    pub id: RateId,
    pub revision: Revision,
    pub unit: ResourceBillingUnit,
    /// Number of base units represented by `unit_price`.
    pub unit_quantity: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_mode: Option<GpuAllocationMode>,
    pub unit_price: Money,
    pub effective_from: UtcTimestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_until: Option<UtcTimestamp>,
}

impl ResourceRate {
    pub fn validate(&self) -> Result<(), ResourceError> {
        if self.revision.get() == 0
            || self.unit_quantity == 0
            || self
                .effective_until
                .is_some_and(|until| until <= self.effective_from)
            || (self.gpu_mode.is_some() != self.gpu_class.is_some())
            || (self.unit == ResourceBillingUnit::GpuUnitSecond)
                != (self.gpu_mode.is_some() && self.gpu_class.is_some())
        {
            return Err(ResourceError::InvalidRate);
        }
        if let Some(class) = &self.gpu_class {
            GpuRequest {
                class: class.clone(),
                count: 1,
            }
            .validate()?;
        }
        self.unit_price.validate()?;
        if self.unit_price.amount.scaled() < 0 {
            return Err(ResourceError::InvalidRate);
        }
        Ok(())
    }
}

/// Quantities captured from a provider meter. Values are integral base units.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceUsageQuantities {
    pub cpu_millicore_seconds: u64,
    pub memory_byte_seconds: u64,
    pub storage_byte_seconds: u64,
    pub gpu_unit_seconds: u64,
}

/// Known or explicitly unknown provider measurement.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum UsageMeasurement {
    Known { quantities: ResourceUsageQuantities },
    Unknown { reason: String },
}

impl UsageMeasurement {
    pub fn validate(&self) -> Result<(), ResourceError> {
        if let Self::Unknown { reason } = self
            && (reason.trim().is_empty() || reason.len() > 500)
        {
            return Err(ResourceError::InvalidUsage);
        }
        Ok(())
    }
}

/// Settlement state for one immutable usage observation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageSettlementState {
    Pending,
    Settled,
    Unsettled,
}

/// Provider usage observation bound to a Project and optional teaching Course.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceUsageRecord {
    pub id: UsageRecordId,
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub kind: ResourceUsageKind,
    pub request_id: ResourceRequestId,
    pub lease_id: Option<LeaseId>,
    pub source_event_id: EventId,
    pub measured_from: UtcTimestamp,
    pub measured_until: UtcTimestamp,
    pub measurement: UsageMeasurement,
    pub settlement: UsageSettlementState,
    pub observed_at: UtcTimestamp,
}

impl ResourceUsageRecord {
    pub fn validate(&self) -> Result<(), ResourceError> {
        if self.measured_until <= self.measured_from || self.observed_at < self.measured_until {
            return Err(ResourceError::InvalidUsage);
        }
        self.measurement.validate()?;
        if let UsageMeasurement::Known { quantities } = &self.measurement {
            let invalid_scope = match self.kind {
                ResourceUsageKind::Compute => quantities.storage_byte_seconds != 0,
                ResourceUsageKind::Storage => {
                    quantities.cpu_millicore_seconds != 0
                        || quantities.memory_byte_seconds != 0
                        || quantities.gpu_unit_seconds != 0
                }
            };
            if invalid_scope {
                return Err(ResourceError::InvalidUsage);
            }
        }
        if matches!(self.measurement, UsageMeasurement::Unknown { .. })
            && self.settlement == UsageSettlementState::Settled
        {
            return Err(ResourceError::UnknownUsageCannotSettle);
        }
        Ok(())
    }
}

/// One immutable line in a calculated charge.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceChargeLine {
    pub rate_id: RateId,
    pub rate_revision: Revision,
    pub unit: ResourceBillingUnit,
    pub quantity: u64,
    pub unit_quantity: u64,
    pub unit_price: Money,
    pub amount: Money,
}

impl ResourceChargeLine {
    pub fn validate(&self) -> Result<(), ResourceError> {
        if self.rate_revision.get() == 0 || self.unit_quantity == 0 {
            return Err(ResourceError::InvalidCharge);
        }
        self.unit_price.validate()?;
        self.amount.validate()?;
        if self.unit_price.currency != self.amount.currency {
            return Err(ResourceError::InvalidCharge);
        }
        let expected = rounded_scaled_amount(
            self.unit_price.amount.scaled(),
            self.quantity,
            self.unit_quantity,
        )?;
        if expected != self.amount.amount.scaled() {
            return Err(ResourceError::InvalidCharge);
        }
        Ok(())
    }
}

/// Computes a six-decimal amount with exact integer arithmetic and midpoint
/// ties rounded to the nearest even result.  The Resource service uses the
/// same formula through `rust_decimal`; keeping this checked contract rule
/// makes malformed persisted charges fail closed during decoding.
fn rounded_scaled_amount(
    unit_price_scaled: i128,
    quantity: u64,
    unit_quantity: u64,
) -> Result<i128, ResourceError> {
    if unit_quantity == 0 {
        return Err(ResourceError::InvalidCharge);
    }
    let product = unit_price_scaled
        .checked_mul(i128::from(quantity))
        .ok_or(ResourceError::InvalidCharge)?;
    let negative = product < 0;
    let magnitude = product.unsigned_abs();
    let denominator = u128::from(unit_quantity);
    let quotient = magnitude / denominator;
    let remainder = magnitude % denominator;
    let twice_remainder = remainder
        .checked_mul(2)
        .ok_or(ResourceError::InvalidCharge)?;
    let round_up =
        twice_remainder > denominator || (twice_remainder == denominator && quotient % 2 == 1);
    let rounded = quotient
        .checked_add(u128::from(round_up))
        .ok_or(ResourceError::InvalidCharge)?;
    if rounded > i128::MAX as u128 {
        return Err(ResourceError::InvalidCharge);
    }
    let rounded = i128::try_from(rounded).map_err(|_| ResourceError::InvalidCharge)?;
    Ok(if negative { -rounded } else { rounded })
}

/// Resource-owned charge projection. This is a calculation record, not a payment.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceCharge {
    pub id: ChargeId,
    pub usage_record_id: UsageRecordId,
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub lines: Vec<ResourceChargeLine>,
    pub total: Money,
    pub settlement: UsageSettlementState,
    pub created_at: UtcTimestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adjustment_of: Option<ChargeId>,
    /// Human supplied reason retained with an administrator adjustment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adjustment_reason: Option<String>,
    /// Actor that authorized an administrator adjustment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adjusted_by: Option<ActorId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic_code: Option<String>,
}

impl ResourceCharge {
    pub fn validate(&self) -> Result<(), ResourceError> {
        if self.lines.is_empty() {
            return Err(ResourceError::InvalidCharge);
        }
        self.total.validate()?;
        let mut total = 0_i128;
        for line in &self.lines {
            line.validate()?;
            if line.unit_price.currency != self.total.currency {
                return Err(ResourceError::InvalidCharge);
            }
            total = total
                .checked_add(line.amount.amount.scaled())
                .ok_or(ResourceError::InvalidCharge)?;
        }
        if total != self.total.amount.scaled() {
            return Err(ResourceError::InvalidCharge);
        }
        if self.diagnostic_code.is_some() && self.settlement == UsageSettlementState::Settled {
            return Err(ResourceError::InvalidCharge);
        }
        match (
            self.adjustment_of,
            &self.adjustment_reason,
            self.adjusted_by,
        ) {
            (None, None, None) => {}
            (Some(_), Some(reason), Some(_))
                if !reason.trim().is_empty() && reason.chars().count() <= 500 => {}
            _ => return Err(ResourceError::InvalidCharge),
        }
        Ok(())
    }
}

/// Project budget and current calculated spend. Strongly consistent updates live in Resource.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceBudget {
    pub id: BudgetId,
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub limit: Money,
    pub warning_at: Money,
    pub spent: Money,
    pub revision: Revision,
    pub updated_at: UtcTimestamp,
}

impl ResourceBudget {
    pub fn validate(&self) -> Result<(), ResourceError> {
        if self.revision.get() == 0
            || self.warning_at.currency != self.limit.currency
            || self.spent.currency != self.limit.currency
            || self.warning_at.amount.scaled() > self.limit.amount.scaled()
            || self.warning_at.amount.scaled() < 0
            || self.limit.amount.scaled() < 0
            || self.spent.amount.scaled() < 0
        {
            return Err(ResourceError::InvalidBudget);
        }
        self.limit.validate()?;
        self.warning_at.validate()?;
        self.spent.validate()
    }
}

/// Stable contract validation failures. Services map these to diagnostics at their boundary.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ResourceError {
    #[error("invalid resource request")]
    InvalidRequest,
    #[error("invalid resource target")]
    InvalidTarget,
    #[error("invalid resource quantities")]
    InvalidResources,
    #[error("invalid resource approval")]
    InvalidApproval,
    #[error("invalid capacity claim")]
    InvalidClaim,
    #[error("invalid resource lease")]
    InvalidLease,
    #[error("invalid GPU catalog entry")]
    InvalidGpuCatalog,
    #[error("invalid fixed-precision decimal")]
    InvalidDecimal,
    #[error("invalid money amount")]
    InvalidMoney,
    #[error("invalid usage observation")]
    InvalidUsage,
    #[error("unknown usage cannot be settled")]
    UnknownUsageCannotSettle,
    #[error("invalid rate")]
    InvalidRate,
    #[error("invalid charge")]
    InvalidCharge,
    #[error("invalid budget")]
    InvalidBudget,
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::{
        GpuAllocation, GpuAllocationMode, GpuCatalogEntryId, GpuRequest, ResourceError,
        WorkloadResources,
    };
    use crate::Revision;

    #[test]
    fn resources_reject_zero_and_non_catalogued_gpu_syntax() {
        assert!(matches!(
            WorkloadResources {
                cpu_millicores: 0,
                memory_bytes: 1,
                storage_bytes: 1,
                gpu: None
            }
            .validate(),
            Err(ResourceError::InvalidResources)
        ));
        assert!(
            WorkloadResources {
                cpu_millicores: 1,
                memory_bytes: 1,
                storage_bytes: 1,
                gpu: Some(GpuRequest {
                    class: "nvidia.com/gpu".into(),
                    count: 1
                }),
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn container_time_slice_allocations_are_one_shared_workload_unit() -> Result<(), Box<dyn Error>>
    {
        let allocation = GpuAllocation {
            entry_id: GpuCatalogEntryId::new(),
            class: "a100-shared".into(),
            count: 2,
            mode: GpuAllocationMode::ContainerTimeSlice,
            provider_binding: "kubernetes-standard".into(),
            allocation_binding: "nvidia.com/gpu".into(),
            catalog_revision: Revision::new(1)?,
        };

        assert!(matches!(
            allocation.validate(),
            Err(ResourceError::InvalidGpuCatalog)
        ));
        Ok(())
    }
}
