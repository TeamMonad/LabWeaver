//! Durable Resource usage boundaries for Environment-owned Work instances.
//!
//! Environment records the intervals at the lifecycle observations that establish them.  The
//! Resource call is deliberately outside the database transaction, so the request is retained
//! until Resource acknowledges the exact `source_event_id`.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use auth::{ServiceTokenClient, ServiceTokenClientConfig, TransportSecurityMode};
use contracts::environment::{
    EnvironmentInstance, EnvironmentLeaseAuthorization, ObservedEnvironmentState,
};
use contracts::http::RecordResourceUsageRequest;
use contracts::resource::{ResourceUsageKind, ResourceUsageQuantities, UsageMeasurement};
use contracts::{EventId, ResourceRequestId, UtcTimestamp};
use reqwest::{Certificate, Client, StatusCode, Url};
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

use crate::{EnvironmentStoreError, PgEnvironmentStore};

const RESOURCE_USAGE_BASE_URI: &str = "LABWEAVER_RESOURCE_USAGE_BASE_URI";
const RESOURCE_USAGE_CA_PATH: &str = "LABWEAVER_RESOURCE_USAGE_CA_PATH";
const SERVICE_OIDC_ISSUER: &str = "LABWEAVER_SERVICE_OIDC_ISSUER";
const SERVICE_OIDC_CA_PATH: &str = "LABWEAVER_SERVICE_OIDC_CA";
const SERVICE_CLIENT_ID: &str = "LABWEAVER_SERVICE_CLIENT_ID";
const SERVICE_CLIENT_SECRET_FILE: &str = "LABWEAVER_SERVICE_CLIENT_SECRET_FILE";
const SERVICE_AUDIENCE: &str = "LABWEAVER_RESOURCE_USAGE_AUDIENCE";
const SERVICE_SCOPES: &str = "LABWEAVER_RESOURCE_USAGE_SCOPES";
const SERVICE_TOKEN_REFRESH_SKEW_SECONDS: &str = "LABWEAVER_SERVICE_TOKEN_REFRESH_SKEW_SECONDS";
const RESOURCE_USAGE_TIMEOUT_MILLISECONDS: &str = "LABWEAVER_RESOURCE_USAGE_TIMEOUT_MILLISECONDS";
const RESOURCE_USAGE_RETRY_DELAY_SECONDS: u64 = 5;
const RESOURCE_USAGE_MAX_TIMEOUT_MILLISECONDS: u64 = 60_000;

/// One pending Resource request claimed by the delivery loop.
#[derive(Clone, Debug)]
pub(crate) struct PendingMeterDelivery {
    pub(crate) delivery_id: Uuid,
    pub(crate) source_event_id: EventId,
    pub(crate) request: RecordResourceUsageRequest,
    pub(crate) attempts: i32,
}

/// Durable Work metering state carried in the Environment schema.
///
/// `compute_started_at` is set only by an actual Ready observation and is cleared only by an
/// actual Stopped or Deleted observation. `storage_started_at` starts with the first actual Ready
/// observation, spans stopped periods, and is closed only after deletion. Cleanup failures split
/// the storage interval at the failure observation so later retention remains measurable while
/// the failed segment is explicitly marked unknown.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MeteringState {
    version: u8,
    environment_id: contracts::EnvironmentId,
    project_id: contracts::ProjectId,
    course_id: Option<contracts::CourseId>,
    owner_actor_id: contracts::ActorId,
    request_id: ResourceRequestId,
    lease_id: contracts::LeaseId,
    lease_revision: contracts::Revision,
    capacity_binding: String,
    approved_resources: contracts::resource::WorkloadResources,
    gpu_allocation: Option<contracts::resource::GpuAllocation>,
    compute_started_at: Option<UtcTimestamp>,
    /// Start of a compute interval whose end is unknown after a failed stop or cleanup.
    ///
    /// This marker remains open until a later Ready observation establishes a new known
    /// boundary or an actual Deleted observation closes the environment. It is optional for
    /// rows written before this field existed.
    #[serde(default)]
    compute_unknown_started_at: Option<UtcTimestamp>,
    storage_started_at: Option<UtcTimestamp>,
    /// Whether the active storage interval has been confirmed by a successful provider
    /// observation. An interval created while cleanup is uncertain remains explicitly unknown
    /// until a later Ready observation confirms a new storage boundary.
    storage_known: bool,
}

impl MeteringState {
    fn from_authorization(instance: &EnvironmentInstance) -> Result<Self, EnvironmentStoreError> {
        let authorization = instance
            .operation
            .lease_authorization
            .as_ref()
            .ok_or(EnvironmentStoreError::LeaseAuthorizationInvalid)?;
        let lease_id = instance
            .lease_id
            .ok_or(EnvironmentStoreError::LeaseAuthorizationInvalid)?;
        let capacity_binding = instance
            .capacity_binding
            .clone()
            .ok_or(EnvironmentStoreError::LeaseAuthorizationInvalid)?;
        validate_authorization(instance, authorization, lease_id, &capacity_binding)?;
        Ok(Self {
            version: 1,
            environment_id: instance.id,
            project_id: instance.project_id,
            course_id: instance.course_id,
            owner_actor_id: instance.owner_id,
            request_id: authorization.resource_request_id,
            lease_id,
            lease_revision: authorization.lease_revision,
            capacity_binding,
            approved_resources: authorization.approved_resources.clone(),
            gpu_allocation: authorization.gpu_allocation.clone(),
            compute_started_at: None,
            compute_unknown_started_at: None,
            storage_started_at: None,
            storage_known: false,
        })
    }

    fn validate_against(
        &self,
        instance: &EnvironmentInstance,
    ) -> Result<(), EnvironmentStoreError> {
        if self.version != 1
            || self.environment_id != instance.id
            || self.project_id != instance.project_id
            || self.course_id != instance.course_id
            || self.owner_actor_id != instance.owner_id
            || self.lease_id
                != instance
                    .lease_id
                    .ok_or(EnvironmentStoreError::MeteringInvalid)?
            || Some(self.capacity_binding.as_str()) != instance.capacity_binding.as_deref()
            || self.lease_revision.get() == 0
            || self.capacity_binding.trim().is_empty()
            || self.storage_started_at.is_none() && self.compute_started_at.is_some()
            || self.compute_started_at.is_some() && self.compute_unknown_started_at.is_some()
            || self.storage_known && self.storage_started_at.is_none()
            || self
                .storage_started_at
                .zip(self.compute_started_at)
                .is_some_and(|(storage, compute)| compute < storage)
        {
            return Err(EnvironmentStoreError::MeteringInvalid);
        }
        self.approved_resources
            .validate()
            .map_err(|_| EnvironmentStoreError::MeteringInvalid)?;
        if let Some(allocation) = &self.gpu_allocation {
            allocation
                .validate()
                .map_err(|_| EnvironmentStoreError::MeteringInvalid)?;
        }
        Ok(())
    }
}

fn validate_authorization(
    instance: &EnvironmentInstance,
    authorization: &EnvironmentLeaseAuthorization,
    lease_id: contracts::LeaseId,
    capacity_binding: &str,
) -> Result<(), EnvironmentStoreError> {
    if authorization.lease_id != lease_id
        || authorization.environment_id != instance.id
        || authorization.project_id != instance.project_id
        || authorization.course_id != instance.course_id
        || authorization.owner_actor_id != instance.owner_id
        || authorization.capacity_binding != capacity_binding
        || authorization.active_from >= authorization.expires_at
    {
        return Err(EnvironmentStoreError::LeaseAuthorizationInvalid);
    }
    authorization
        .validate()
        .map_err(|_| EnvironmentStoreError::LeaseAuthorizationInvalid)
}

/// Creates the durable Work metering state in the same transaction as the aggregate.
pub(crate) async fn initialize(
    transaction: &mut Transaction<'_, Postgres>,
    instance: &EnvironmentInstance,
) -> Result<(), EnvironmentStoreError> {
    if instance.class != contracts::authoring::EnvironmentClass::Work {
        return Ok(());
    }
    let state = MeteringState::from_authorization(instance)?;
    sqlx::query(
        "INSERT INTO environment.resource_metering_state (environment_id, contract) \
         VALUES ($1, $2)",
    )
    .bind(instance.id.as_uuid())
    .bind(serde_json::to_value(state)?)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

/// Records actual lifecycle observations and appends pending usage deliveries atomically with
/// the Environment aggregate update.
#[allow(
    clippy::too_many_lines,
    reason = "meter boundaries and durable delivery ordering stay visible in one transaction"
)]
pub(crate) async fn record_transition(
    transaction: &mut Transaction<'_, Postgres>,
    previous: &EnvironmentInstance,
    updated: &EnvironmentInstance,
    occurred_at: UtcTimestamp,
) -> Result<(), EnvironmentStoreError> {
    if updated.class != contracts::authoring::EnvironmentClass::Work {
        return Ok(());
    }
    let row = sqlx::query(
        "SELECT contract FROM environment.resource_metering_state \
         WHERE environment_id=$1 FOR UPDATE",
    )
    .bind(updated.id.as_uuid())
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or(EnvironmentStoreError::MeteringInvalid)?;
    let mut state: MeteringState = serde_json::from_value(row.try_get("contract")?)?;
    state.validate_against(previous)?;
    state.validate_against(updated)?;

    if previous.observed_state != ObservedEnvironmentState::Ready
        && updated.observed_state == ObservedEnvironmentState::Ready
    {
        let ready_at = ready_observation_at(previous, updated, occurred_at)?;
        if let Some((measured_from, measured_until)) =
            compute_unknown_recovery_boundary(&mut state, ready_at)
        {
            enqueue_compute_unknown(
                transaction,
                updated.id,
                &state,
                measured_from,
                measured_until,
                "compute occupancy remained uncertain until a Ready observation",
            )
            .await?;
        }
        if let Some((measured_from, measured_until)) = storage_ready_boundary(&mut state, ready_at)
        {
            enqueue_storage_unknown(
                transaction,
                updated.id,
                &state,
                measured_from,
                measured_until,
                "storage occupancy remained uncertain until a Ready observation",
            )
            .await?;
        }
    }

    // Provisioning can create a PVC before the workload becomes Ready. If that operation fails,
    // retain an explicit unknown interval from the operation boundary so a later cleanup cannot
    // silently erase the possible storage occupancy.
    let failed_provisioning = updated.observed_state == ObservedEnvironmentState::Failed
        && previous.observed_state == ObservedEnvironmentState::Provisioning
        && state.storage_started_at.is_none();
    if failed_provisioning {
        state.storage_started_at = Some(updated.operation.accepted_at);
        state.storage_known = false;
    }

    if previous.observed_state != ObservedEnvironmentState::Stopped
        && updated.observed_state == ObservedEnvironmentState::Stopped
        && let Some(started_at) = state.compute_started_at.take()
        && occurred_at > started_at
    {
        let request = usage_request(
            &state,
            ResourceUsageKind::Compute,
            started_at,
            occurred_at,
            UsageMeasurement::Known {
                quantities: quantities(
                    &state.approved_resources,
                    state.gpu_allocation.as_ref(),
                    started_at,
                    occurred_at,
                )?,
            },
        )?;
        enqueue_delivery(transaction, updated.id, request, occurred_at).await?;
    }

    // A failed cleanup is represented by the authoritative Failed observation. A later Deleted
    // observation may retain a failed operation diagnostic for cleanup auditability, but it is
    // still a real deletion boundary and must close the known meters.
    let cleanup_failed = updated.desired_state
        == contracts::environment::DesiredEnvironmentState::Deleted
        && updated.observed_state == ObservedEnvironmentState::Failed;
    let stop_failed = updated.observed_state == ObservedEnvironmentState::Failed
        && previous.observed_state == ObservedEnvironmentState::Stopping;
    if stop_failed {
        // A failed stop proves only that the compute stop boundary is uncertain.  The PVC is
        // still present, so its confirmed storage interval must remain known and open until an
        // actual Deleted observation.  Deletion cleanup uses the broader helper below because
        // that is the boundary where storage itself becomes uncertain.
        let (compute_started_at, compute_unknown_started_at) =
            enqueue_unknown_compute_delivery(transaction, updated, &state, occurred_at).await?;
        state.compute_started_at = compute_started_at;
        state.compute_unknown_started_at = compute_unknown_started_at;
    }
    if updated.observed_state == ObservedEnvironmentState::Deleted && !cleanup_failed {
        if let Some(started_at) = state.compute_started_at.take()
            && occurred_at > started_at
        {
            let request = usage_request(
                &state,
                ResourceUsageKind::Compute,
                started_at,
                occurred_at,
                UsageMeasurement::Known {
                    quantities: quantities(
                        &state.approved_resources,
                        state.gpu_allocation.as_ref(),
                        started_at,
                        occurred_at,
                    )?,
                },
            )?;
            enqueue_delivery(transaction, updated.id, request, occurred_at).await?;
        }
        if let Some((measured_from, measured_until)) =
            compute_unknown_until(&mut state, occurred_at)
        {
            enqueue_compute_unknown(
                transaction,
                updated.id,
                &state,
                measured_from,
                measured_until,
                "environment deletion followed an uncertain compute interval",
            )
            .await?;
        }
        if let Some(started_at) = state.storage_started_at.take()
            && occurred_at > started_at
        {
            let measurement = if state.storage_known {
                UsageMeasurement::Known {
                    quantities: storage_quantities(
                        &state.approved_resources,
                        started_at,
                        occurred_at,
                    )?,
                }
            } else {
                UsageMeasurement::Unknown {
                    reason: "environment deletion followed an uncertain storage interval"
                        .to_owned(),
                }
            };
            let request = usage_request(
                &state,
                ResourceUsageKind::Storage,
                started_at,
                occurred_at,
                measurement,
            )?;
            enqueue_delivery(transaction, updated.id, request, occurred_at).await?;
        }
        state.storage_known = false;
    } else if cleanup_failed {
        let (compute_started_at, compute_unknown_started_at, storage_started_at, storage_known) =
            enqueue_unknown_cleanup_deliveries(transaction, updated, &state, occurred_at).await?;
        state.compute_started_at = compute_started_at;
        state.compute_unknown_started_at = compute_unknown_started_at;
        state.storage_started_at = storage_started_at;
        state.storage_known = storage_known;
    } else if stop_failed {
        // The failed stop split only compute.  The confirmed storage interval remains known and
        // open until a later actual deletion boundary.
    } else if failed_provisioning {
        if let Some(started_at) = state.storage_started_at
            && occurred_at > started_at
        {
            enqueue_storage_unknown(
                transaction,
                updated.id,
                &state,
                started_at,
                occurred_at,
                "provisioning failed before storage readiness was observed",
            )
            .await?;
            state.storage_started_at = Some(occurred_at);
        }
        state.storage_known = false;
    }

    sqlx::query(
        "UPDATE environment.resource_metering_state SET contract=$2 \
         WHERE environment_id=$1",
    )
    .bind(updated.id.as_uuid())
    .bind(serde_json::to_value(state)?)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn enqueue_unknown_cleanup_deliveries(
    transaction: &mut Transaction<'_, Postgres>,
    instance: &EnvironmentInstance,
    state: &MeteringState,
    occurred_at: UtcTimestamp,
) -> Result<
    (
        Option<UtcTimestamp>,
        Option<UtcTimestamp>,
        Option<UtcTimestamp>,
        bool,
    ),
    EnvironmentStoreError,
> {
    let mut next = state.clone();
    if let Some((measured_from, measured_until)) = compute_unknown_boundary(&mut next, occurred_at)
    {
        enqueue_compute_unknown(
            transaction,
            instance.id,
            state,
            measured_from,
            measured_until,
            "environment cleanup failed before actual compute stop was observed",
        )
        .await?;
    }
    if let Some((measured_from, measured_until)) = storage_unknown_segment(&mut next, occurred_at) {
        enqueue_storage_unknown(
            transaction,
            instance.id,
            state,
            measured_from,
            measured_until,
            "environment cleanup failed before actual deletion was observed",
        )
        .await?;
    }
    Ok((
        next.compute_started_at,
        next.compute_unknown_started_at,
        next.storage_started_at,
        next.storage_known,
    ))
}

async fn enqueue_unknown_compute_delivery(
    transaction: &mut Transaction<'_, Postgres>,
    instance: &EnvironmentInstance,
    state: &MeteringState,
    occurred_at: UtcTimestamp,
) -> Result<(Option<UtcTimestamp>, Option<UtcTimestamp>), EnvironmentStoreError> {
    let mut next = state.clone();
    if let Some((measured_from, measured_until)) = compute_unknown_boundary(&mut next, occurred_at)
    {
        enqueue_compute_unknown(
            transaction,
            instance.id,
            state,
            measured_from,
            measured_until,
            "environment stop failed before actual compute stop was observed",
        )
        .await?;
    }
    Ok((next.compute_started_at, next.compute_unknown_started_at))
}

fn storage_ready_boundary(
    state: &mut MeteringState,
    occurred_at: UtcTimestamp,
) -> Option<(UtcTimestamp, UtcTimestamp)> {
    if state.storage_known {
        // `validate_against` rejects this state; keep the mutation fail-closed if a corrupt
        // row reaches this helper instead of manufacturing a new storage interval.
        state.storage_started_at?;
        if state.compute_started_at.is_none() {
            state.compute_started_at = Some(occurred_at);
        }
        return None;
    }
    let unknown_segment = state
        .storage_started_at
        .filter(|_| !state.storage_known)
        .filter(|started_at| occurred_at > *started_at)
        .map(|started_at| (started_at, occurred_at));
    state.storage_started_at = Some(occurred_at);
    state.storage_known = true;
    if state.compute_started_at.is_none() {
        state.compute_started_at = Some(occurred_at);
    }
    unknown_segment
}

fn ready_observation_at(
    previous: &EnvironmentInstance,
    updated: &EnvironmentInstance,
    occurred_at: UtcTimestamp,
) -> Result<UtcTimestamp, EnvironmentStoreError> {
    let observed_at = updated
        .endpoints
        .iter()
        .map(|endpoint| endpoint.observed_at)
        .min()
        .ok_or(EnvironmentStoreError::MeteringInvalid)?;
    if observed_at < previous.operation.accepted_at || observed_at > occurred_at {
        return Err(EnvironmentStoreError::MeteringInvalid);
    }
    Ok(observed_at)
}

fn storage_unknown_segment(
    state: &mut MeteringState,
    occurred_at: UtcTimestamp,
) -> Option<(UtcTimestamp, UtcTimestamp)> {
    let started_at = state.storage_started_at?;
    if occurred_at <= started_at {
        return None;
    }
    state.storage_started_at = Some(occurred_at);
    state.storage_known = false;
    Some((started_at, occurred_at))
}

fn compute_unknown_boundary(
    state: &mut MeteringState,
    occurred_at: UtcTimestamp,
) -> Option<(UtcTimestamp, UtcTimestamp)> {
    let started_at = state
        .compute_started_at
        .take()
        .or_else(|| state.compute_unknown_started_at.take())?;
    if occurred_at <= started_at {
        state.compute_unknown_started_at = Some(started_at);
        return None;
    }
    state.compute_unknown_started_at = Some(occurred_at);
    Some((started_at, occurred_at))
}

fn compute_unknown_recovery_boundary(
    state: &mut MeteringState,
    occurred_at: UtcTimestamp,
) -> Option<(UtcTimestamp, UtcTimestamp)> {
    let started_at = state.compute_unknown_started_at.take()?;
    if occurred_at < started_at {
        state.compute_unknown_started_at = Some(started_at);
        return None;
    }
    Some((started_at, occurred_at)).filter(|(from, until)| until > from)
}

fn compute_unknown_until(
    state: &mut MeteringState,
    occurred_at: UtcTimestamp,
) -> Option<(UtcTimestamp, UtcTimestamp)> {
    let started_at = state.compute_unknown_started_at.take()?;
    if occurred_at <= started_at {
        state.compute_unknown_started_at = Some(started_at);
        return None;
    }
    Some((started_at, occurred_at))
}

async fn enqueue_compute_unknown(
    transaction: &mut Transaction<'_, Postgres>,
    environment_id: contracts::EnvironmentId,
    state: &MeteringState,
    measured_from: UtcTimestamp,
    measured_until: UtcTimestamp,
    reason: &str,
) -> Result<(), EnvironmentStoreError> {
    let request = usage_request(
        state,
        ResourceUsageKind::Compute,
        measured_from,
        measured_until,
        UsageMeasurement::Unknown {
            reason: reason.to_owned(),
        },
    )?;
    enqueue_delivery(transaction, environment_id, request, measured_until).await
}

async fn enqueue_storage_unknown(
    transaction: &mut Transaction<'_, Postgres>,
    environment_id: contracts::EnvironmentId,
    state: &MeteringState,
    measured_from: UtcTimestamp,
    measured_until: UtcTimestamp,
    reason: &str,
) -> Result<(), EnvironmentStoreError> {
    let request = usage_request(
        state,
        ResourceUsageKind::Storage,
        measured_from,
        measured_until,
        UsageMeasurement::Unknown {
            reason: reason.to_owned(),
        },
    )?;
    enqueue_delivery(transaction, environment_id, request, measured_until).await
}

fn usage_request(
    state: &MeteringState,
    kind: ResourceUsageKind,
    measured_from: UtcTimestamp,
    measured_until: UtcTimestamp,
    measurement: UsageMeasurement,
) -> Result<RecordResourceUsageRequest, EnvironmentStoreError> {
    if measured_until <= measured_from {
        return Err(EnvironmentStoreError::MeteringInvalid);
    }
    let request = RecordResourceUsageRequest {
        project_id: state.project_id,
        course_id: state.course_id,
        kind,
        request_id: state.request_id,
        lease_id: Some(state.lease_id),
        source_event_id: EventId::new(),
        measured_from,
        measured_until,
        measurement,
    };
    request
        .measurement
        .validate()
        .map_err(|_| EnvironmentStoreError::MeteringInvalid)?;
    Ok(request)
}

fn elapsed_milliseconds(
    measured_from: UtcTimestamp,
    measured_until: UtcTimestamp,
) -> Result<u128, EnvironmentStoreError> {
    if measured_until <= measured_from {
        return Err(EnvironmentStoreError::MeteringInvalid);
    }
    let milliseconds = (measured_until.get() - measured_from.get())
        .whole_milliseconds()
        .max(1)
        .try_into()
        .map_err(|_| EnvironmentStoreError::MeteringInvalid)?;
    Ok(milliseconds)
}

fn multiply_milliseconds(value: u64, milliseconds: u128) -> Result<u64, EnvironmentStoreError> {
    let quantity = u128::from(value)
        .checked_mul(milliseconds)
        .ok_or(EnvironmentStoreError::NumericOverflow("usage quantity"))?
        / 1_000;
    u64::try_from(quantity).map_err(|_| EnvironmentStoreError::NumericOverflow("usage quantity"))
}

fn quantities(
    resources: &contracts::resource::WorkloadResources,
    gpu_allocation: Option<&contracts::resource::GpuAllocation>,
    measured_from: UtcTimestamp,
    measured_until: UtcTimestamp,
) -> Result<ResourceUsageQuantities, EnvironmentStoreError> {
    let milliseconds = elapsed_milliseconds(measured_from, measured_until)?;
    Ok(ResourceUsageQuantities {
        cpu_millicore_seconds: multiply_milliseconds(
            resources.cpu_millicores.into(),
            milliseconds,
        )?,
        memory_byte_seconds: multiply_milliseconds(resources.memory_bytes, milliseconds)?,
        storage_byte_seconds: 0,
        gpu_unit_seconds: multiply_milliseconds(
            gpu_allocation.map_or(0, |allocation| u64::from(allocation.count)),
            milliseconds,
        )?,
    })
}

fn storage_quantities(
    resources: &contracts::resource::WorkloadResources,
    measured_from: UtcTimestamp,
    measured_until: UtcTimestamp,
) -> Result<ResourceUsageQuantities, EnvironmentStoreError> {
    let milliseconds = elapsed_milliseconds(measured_from, measured_until)?;
    Ok(ResourceUsageQuantities {
        cpu_millicore_seconds: 0,
        memory_byte_seconds: 0,
        storage_byte_seconds: multiply_milliseconds(resources.storage_bytes, milliseconds)?,
        gpu_unit_seconds: 0,
    })
}

async fn enqueue_delivery(
    transaction: &mut Transaction<'_, Postgres>,
    environment_id: contracts::EnvironmentId,
    request: RecordResourceUsageRequest,
    next_attempt_at: UtcTimestamp,
) -> Result<(), EnvironmentStoreError> {
    let delivery_id = Uuid::now_v7();
    let request_value = serde_json::to_value(&request)?;
    let kind = wire_kind(request.kind);
    let result = sqlx::query(
        "INSERT INTO environment.resource_meter_deliveries \
         (delivery_id, environment_id, source_event_id, kind, measured_from, measured_until, \
          request, state, attempts, next_attempt_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,'pending',0,$8) \
         ON CONFLICT (environment_id, kind, measured_from, measured_until) DO NOTHING",
    )
    .bind(delivery_id)
    .bind(environment_id.as_uuid())
    .bind(request.source_event_id.as_uuid())
    .bind(kind)
    .bind(request.measured_from.get())
    .bind(request.measured_until.get())
    .bind(request_value)
    .bind(next_attempt_at.get())
    .execute(&mut **transaction)
    .await?;
    if result.rows_affected() == 0 {
        let row = sqlx::query(
            "SELECT source_event_id, request FROM environment.resource_meter_deliveries \
             WHERE environment_id=$1 AND kind=$2 AND measured_from=$3 AND measured_until=$4 \
             FOR UPDATE",
        )
        .bind(environment_id.as_uuid())
        .bind(kind)
        .bind(request.measured_from.get())
        .bind(request.measured_until.get())
        .fetch_optional(&mut **transaction)
        .await?
        .ok_or(EnvironmentStoreError::MeteringInvalid)?;
        let source_event_id: Uuid = row.try_get("source_event_id")?;
        let existing: RecordResourceUsageRequest = serde_json::from_value(row.try_get("request")?)?;
        let mut expected = request;
        expected.source_event_id = EventId::from_str(&source_event_id.to_string())
            .map_err(|_| EnvironmentStoreError::MeteringInvalid)?;
        if existing != expected {
            return Err(EnvironmentStoreError::MeteringInvalid);
        }
    }
    Ok(())
}

fn wire_kind(kind: ResourceUsageKind) -> &'static str {
    match kind {
        ResourceUsageKind::Compute => "compute",
        ResourceUsageKind::Storage => "storage",
    }
}

impl PgEnvironmentStore {
    /// Claims one pending meter delivery. The claim is advanced before network I/O so a
    /// process restart leaves a bounded, retryable attempt behind.
    pub(crate) async fn claim_meter_delivery(
        &self,
    ) -> Result<Option<PendingMeterDelivery>, EnvironmentStoreError> {
        let mut transaction = self.pool().begin().await?;
        let row = sqlx::query(
            "SELECT delivery_id, source_event_id, request, attempts \
             FROM environment.resource_meter_deliveries \
             WHERE state='pending' AND next_attempt_at<=clock_timestamp() \
             ORDER BY next_attempt_at, delivery_id FOR UPDATE SKIP LOCKED LIMIT 1",
        )
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(row) = row else {
            transaction.commit().await?;
            return Ok(None);
        };
        let delivery_id: Uuid = row.try_get("delivery_id")?;
        let source_event_uuid: Uuid = row.try_get("source_event_id")?;
        let attempts: i32 = row.try_get("attempts")?;
        let request: RecordResourceUsageRequest = serde_json::from_value(row.try_get("request")?)?;
        if request.source_event_id.as_uuid() != source_event_uuid || attempts < 0 {
            return Err(EnvironmentStoreError::MeteringInvalid);
        }
        sqlx::query(
            "UPDATE environment.resource_meter_deliveries \
             SET attempts=attempts+1, next_attempt_at=clock_timestamp()+($2 * interval '1 second') \
             WHERE delivery_id=$1 AND state='pending'",
        )
        .bind(delivery_id)
        .bind(
            i64::try_from(RESOURCE_USAGE_RETRY_DELAY_SECONDS)
                .map_err(|_| EnvironmentStoreError::MeteringInvalid)?,
        )
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(Some(PendingMeterDelivery {
            delivery_id,
            source_event_id: request.source_event_id,
            request,
            attempts,
        }))
    }

    pub(crate) async fn mark_meter_delivery_delivered(
        &self,
        delivery_id: Uuid,
        source_event_id: EventId,
    ) -> Result<(), EnvironmentStoreError> {
        let result = sqlx::query(
            "UPDATE environment.resource_meter_deliveries \
             SET state='delivered', delivered_at=clock_timestamp(), last_diagnostic_code=NULL \
             WHERE delivery_id=$1 AND source_event_id=$2 AND state='pending'",
        )
        .bind(delivery_id)
        .bind(source_event_id.as_uuid())
        .execute(&self.pool())
        .await?;
        if result.rows_affected() != 1 {
            return Err(EnvironmentStoreError::MeteringInvalid);
        }
        Ok(())
    }

    pub(crate) async fn mark_meter_delivery_failed(
        &self,
        delivery_id: Uuid,
        source_event_id: EventId,
        diagnostic_code: &str,
    ) -> Result<(), EnvironmentStoreError> {
        if diagnostic_code.is_empty() || diagnostic_code.len() > 128 {
            return Err(EnvironmentStoreError::MeteringInvalid);
        }
        let result = sqlx::query(
            "UPDATE environment.resource_meter_deliveries \
             SET last_diagnostic_code=$3 \
             WHERE delivery_id=$1 AND source_event_id=$2 AND state='pending'",
        )
        .bind(delivery_id)
        .bind(source_event_id.as_uuid())
        .bind(diagnostic_code)
        .execute(&self.pool())
        .await?;
        if result.rows_affected() != 1 {
            return Err(EnvironmentStoreError::MeteringInvalid);
        }
        Ok(())
    }
}

/// Client for the Resource internal usage endpoint.
#[derive(Clone)]
pub(crate) struct ResourceUsageClient {
    base_uri: Url,
    client: Client,
    token_client: ServiceTokenClient,
}

impl ResourceUsageClient {
    pub(crate) async fn from_env() -> Result<Self, ResourceUsageClientError> {
        let base_uri = parse_base_uri(&required(RESOURCE_USAGE_BASE_URI)?)?;
        let timeout_milliseconds = required_u64(RESOURCE_USAGE_TIMEOUT_MILLISECONDS)?;
        if !(1..=RESOURCE_USAGE_MAX_TIMEOUT_MILLISECONDS).contains(&timeout_milliseconds) {
            return Err(ResourceUsageClientError::Configuration);
        }
        let resource_ca = std::fs::read(required_path(RESOURCE_USAGE_CA_PATH)?)
            .map_err(|_| ResourceUsageClientError::Configuration)?;
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .https_only(true)
            .tls_built_in_root_certs(false)
            .add_root_certificate(
                Certificate::from_pem(&resource_ca)
                    .map_err(|_| ResourceUsageClientError::Configuration)?,
            )
            .timeout(Duration::from_millis(timeout_milliseconds))
            .build()
            .map_err(|_| ResourceUsageClientError::Configuration)?;
        let oidc_ca = std::fs::read(required_path(SERVICE_OIDC_CA_PATH)?)
            .map_err(|_| ResourceUsageClientError::Configuration)?;
        let oidc_http =
            auth::no_redirect_http_client(Some(&oidc_ca), TransportSecurityMode::Strict)
                .map_err(|_| ResourceUsageClientError::Configuration)?;
        let audience = required(SERVICE_AUDIENCE)?;
        let scopes = parse_scopes(&required(SERVICE_SCOPES)?)?;
        if !scopes.contains("resource.usage.record") {
            return Err(ResourceUsageClientError::Configuration);
        }
        let token_config = ServiceTokenClientConfig::new(
            &required(SERVICE_OIDC_ISSUER)?,
            required(SERVICE_CLIENT_ID)?,
            read_secret(&required_path(SERVICE_CLIENT_SECRET_FILE)?)?,
            audience,
            scopes,
            required_u64(SERVICE_TOKEN_REFRESH_SKEW_SECONDS)?,
            TransportSecurityMode::Strict,
        )
        .map_err(|_| ResourceUsageClientError::Configuration)?;
        let token_client = ServiceTokenClient::discover(token_config, oidc_http)
            .await
            .map_err(|_| ResourceUsageClientError::TokenDiscovery)?;
        Ok(Self {
            base_uri,
            client,
            token_client,
        })
    }

    pub(crate) async fn deliver(
        &self,
        request: &RecordResourceUsageRequest,
    ) -> Result<(), ResourceUsageClientError> {
        let mut headers = reqwest::header::HeaderMap::new();
        self.token_client
            .bearer_auth(&mut headers)
            .await
            .map_err(|_| ResourceUsageClientError::TokenExchange)?;
        let response = self
            .client
            .post(
                self.base_uri
                    .join("internal/v1/resource/usage")
                    .map_err(|_| ResourceUsageClientError::Configuration)?,
            )
            .headers(headers)
            .json(request)
            .send()
            .await
            .map_err(|_| ResourceUsageClientError::Transport)?;
        if response.status() != StatusCode::OK && response.status() != StatusCode::CREATED {
            return Err(ResourceUsageClientError::Rejected);
        }
        let record: contracts::resource::ResourceUsageRecord = response
            .json()
            .await
            .map_err(|_| ResourceUsageClientError::InvalidResponse)?;
        record
            .validate()
            .map_err(|_| ResourceUsageClientError::InvalidResponse)?;
        if record.source_event_id != request.source_event_id
            || record.project_id != request.project_id
            || record.course_id != request.course_id
            || record.request_id != request.request_id
            || record.lease_id != request.lease_id
            || record.kind != request.kind
            || record.measured_from != request.measured_from
            || record.measured_until != request.measured_until
            || record.measurement != request.measurement
        {
            return Err(ResourceUsageClientError::InvalidResponse);
        }
        Ok(())
    }
}

pub(crate) async fn delivery_loop(
    store: PgEnvironmentStore,
    client: ResourceUsageClient,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> Result<(), EnvironmentStoreError> {
    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                changed.map_err(|_| EnvironmentStoreError::MeteringInvalid)?;
                return Ok(());
            }
            delivery = store.claim_meter_delivery() => {
                let Some(delivery) = delivery? else {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    continue;
                };
                match client.deliver(&delivery.request).await {
                    Ok(()) => {
                        store.mark_meter_delivery_delivered(delivery.delivery_id, delivery.source_event_id).await?;
                        tracing::info!(event="environment.resource_meter.delivered", source_event_id=%delivery.source_event_id, attempts=delivery.attempts+1);
                    }
                    Err(error) => {
                        let diagnostic = error.diagnostic_code();
                        store.mark_meter_delivery_failed(delivery.delivery_id, delivery.source_event_id, diagnostic).await?;
                        tracing::error!(event="environment.resource_meter.delivery_failed", source_event_id=%delivery.source_event_id, attempts=delivery.attempts+1, diagnostic_code=diagnostic);
                    }
                }
            }
        }
    }
}

fn required(name: &'static str) -> Result<String, ResourceUsageClientError> {
    let value = std::env::var(name).map_err(|_| ResourceUsageClientError::Configuration)?;
    if value.trim().is_empty() {
        return Err(ResourceUsageClientError::Configuration);
    }
    Ok(value.trim().to_owned())
}

fn required_path(name: &'static str) -> Result<PathBuf, ResourceUsageClientError> {
    let path = PathBuf::from(required(name)?);
    if !path.is_absolute() {
        return Err(ResourceUsageClientError::Configuration);
    }
    Ok(path)
}

fn required_u64(name: &'static str) -> Result<u64, ResourceUsageClientError> {
    required(name)?
        .parse()
        .map_err(|_| ResourceUsageClientError::Configuration)
}

fn read_secret(path: &PathBuf) -> Result<String, ResourceUsageClientError> {
    let value =
        std::fs::read_to_string(path).map_err(|_| ResourceUsageClientError::Configuration)?;
    let value = value.trim();
    if value.is_empty() || value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(ResourceUsageClientError::Configuration);
    }
    Ok(value.to_owned())
}

fn parse_base_uri(value: &str) -> Result<Url, ResourceUsageClientError> {
    let uri = Url::parse(value).map_err(|_| ResourceUsageClientError::Configuration)?;
    if uri.scheme() != "https" || uri.host_str().is_none() || !uri.path().ends_with('/') {
        return Err(ResourceUsageClientError::Configuration);
    }
    Ok(uri)
}

fn parse_scopes(value: &str) -> Result<BTreeSet<String>, ResourceUsageClientError> {
    let scopes = value
        .split_ascii_whitespace()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    if scopes.is_empty() || scopes.iter().any(String::is_empty) {
        return Err(ResourceUsageClientError::Configuration);
    }
    Ok(scopes)
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ResourceUsageClientError {
    #[error("LW_ENVIRONMENT_RESOURCE_USAGE_CONFIGURATION_INVALID")]
    Configuration,
    #[error("LW_ENVIRONMENT_RESOURCE_USAGE_TOKEN_DISCOVERY_FAILED")]
    TokenDiscovery,
    #[error("LW_ENVIRONMENT_RESOURCE_USAGE_TOKEN_EXCHANGE_FAILED")]
    TokenExchange,
    #[error("LW_ENVIRONMENT_RESOURCE_USAGE_TRANSPORT_FAILED")]
    Transport,
    #[error("LW_ENVIRONMENT_RESOURCE_USAGE_REJECTED")]
    Rejected,
    #[error("LW_ENVIRONMENT_RESOURCE_USAGE_RESPONSE_INVALID")]
    InvalidResponse,
}

impl ResourceUsageClientError {
    pub(crate) const fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::Configuration => "LW_ENVIRONMENT_RESOURCE_USAGE_CONFIGURATION_INVALID",
            Self::TokenDiscovery => "LW_ENVIRONMENT_RESOURCE_USAGE_TOKEN_DISCOVERY_FAILED",
            Self::TokenExchange => "LW_ENVIRONMENT_RESOURCE_USAGE_TOKEN_EXCHANGE_FAILED",
            Self::Transport => "LW_ENVIRONMENT_RESOURCE_USAGE_TRANSPORT_FAILED",
            Self::Rejected => "LW_ENVIRONMENT_RESOURCE_USAGE_REJECTED",
            Self::InvalidResponse => "LW_ENVIRONMENT_RESOURCE_USAGE_RESPONSE_INVALID",
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "fixed test identities and timestamps are intentional"
)]
mod tests {
    use super::{
        MeteringState, compute_unknown_boundary, compute_unknown_recovery_boundary,
        compute_unknown_until, quantities, storage_quantities, storage_ready_boundary,
        storage_unknown_segment,
    };
    use contracts::resource::{GpuAllocation, GpuAllocationMode, WorkloadResources};
    use contracts::{
        ActorId, EnvironmentId, GpuCatalogEntryId, LeaseId, ProjectId, ResourceRequestId, Revision,
    };

    fn timestamp(seconds: i64) -> contracts::UtcTimestamp {
        contracts::UtcTimestamp::from_utc(
            time::OffsetDateTime::from_unix_timestamp(seconds).expect("fixed timestamp"),
        )
        .expect("UTC timestamp")
    }

    fn timestamp_millis(milliseconds: i64) -> contracts::UtcTimestamp {
        contracts::UtcTimestamp::from_utc(
            time::OffsetDateTime::from_unix_timestamp(0).expect("fixed timestamp")
                + time::Duration::milliseconds(milliseconds),
        )
        .expect("UTC timestamp")
    }

    fn state() -> MeteringState {
        MeteringState {
            version: 1,
            environment_id: EnvironmentId::new(),
            project_id: ProjectId::new(),
            course_id: None,
            owner_actor_id: ActorId::new(),
            request_id: ResourceRequestId::new(),
            lease_id: LeaseId::new(),
            lease_revision: Revision::new(1).expect("fixed revision"),
            capacity_binding: "workspace".to_owned(),
            approved_resources: WorkloadResources {
                cpu_millicores: 1,
                memory_bytes: 1,
                storage_bytes: 1,
                gpu: None,
            },
            gpu_allocation: None,
            compute_started_at: None,
            compute_unknown_started_at: None,
            storage_started_at: None,
            storage_known: false,
        }
    }

    #[test]
    fn cleanup_failure_keeps_later_storage_and_recovery_boundaries() {
        let mut state = state();
        let first_ready = timestamp(10);
        let first_failure = timestamp(20);
        let second_failure = timestamp(30);
        let recovery_ready = timestamp(40);
        let deletion = timestamp(50);

        state.storage_started_at = Some(first_ready);
        state.storage_known = true;
        assert_eq!(
            storage_unknown_segment(&mut state, first_failure),
            Some((first_ready, first_failure))
        );
        assert_eq!(state.storage_started_at, Some(first_failure));
        assert!(!state.storage_known);

        assert_eq!(
            storage_unknown_segment(&mut state, second_failure),
            Some((first_failure, second_failure))
        );
        assert_eq!(state.storage_started_at, Some(second_failure));
        assert!(!state.storage_known);

        assert_eq!(
            storage_ready_boundary(&mut state, recovery_ready),
            Some((second_failure, recovery_ready))
        );
        assert_eq!(state.storage_started_at, Some(recovery_ready));
        assert!(state.storage_known);

        // The interval after recovery remains a known meter until the actual deletion boundary.
        assert!(deletion > state.storage_started_at.expect("recovery boundary"));
        assert_eq!(
            (
                state.storage_started_at.expect("recovery boundary"),
                deletion,
            ),
            (recovery_ready, deletion)
        );
    }

    #[test]
    fn uncertain_compute_is_not_closed_as_known_after_cleanup_failure() {
        let mut state = state();
        let started = timestamp(10);
        let failed = timestamp(20);
        state.compute_started_at = Some(started);

        assert_eq!(
            compute_unknown_boundary(&mut state, failed),
            Some((started, failed))
        );
        assert!(state.compute_started_at.is_none());
        assert_eq!(state.compute_unknown_started_at, Some(failed));
        assert_eq!(
            compute_unknown_until(&mut state, timestamp(30)),
            Some((failed, timestamp(30)))
        );
        assert!(state.compute_unknown_started_at.is_none());
    }

    #[test]
    fn failed_stop_compute_interval_stays_unknown_until_delete() {
        let mut state = state();
        let started = timestamp(10);
        let failed_stop = timestamp(20);
        let deletion = timestamp(30);
        state.compute_started_at = Some(started);

        assert_eq!(
            compute_unknown_boundary(&mut state, failed_stop),
            Some((started, failed_stop))
        );
        assert_eq!(state.compute_unknown_started_at, Some(failed_stop));
        assert_eq!(
            compute_unknown_until(&mut state, deletion),
            Some((failed_stop, deletion))
        );
        assert!(state.compute_started_at.is_none());
        assert!(state.compute_unknown_started_at.is_none());
    }

    #[test]
    fn ready_observation_closes_unknown_compute_before_new_known_interval() {
        let mut state = state();
        let failed_stop = timestamp(20);
        let recovery_ready = timestamp(30);
        state.compute_unknown_started_at = Some(failed_stop);

        assert_eq!(
            compute_unknown_recovery_boundary(&mut state, recovery_ready),
            Some((failed_stop, recovery_ready))
        );
        assert!(state.compute_unknown_started_at.is_none());
    }

    #[test]
    fn ready_after_stop_preserves_the_original_known_storage_boundary() {
        let mut state = state();
        let first_ready = timestamp(10);
        let stopped = timestamp(20);
        let restarted = timestamp(30);
        state.storage_started_at = Some(first_ready);
        state.storage_known = true;
        state.compute_started_at = None;

        assert_eq!(storage_ready_boundary(&mut state, restarted), None);
        assert_eq!(state.storage_started_at, Some(first_ready));
        assert!(state.storage_known);
        assert_eq!(state.compute_started_at, Some(restarted));
        assert!(stopped > first_ready);
    }

    #[test]
    fn failed_stop_keeps_known_storage_open_until_deletion()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut state = state();
        let ready = timestamp(10);
        let failed_stop = timestamp(20);
        let deletion = timestamp(30);
        state.compute_started_at = Some(ready);
        state.storage_started_at = Some(ready);
        state.storage_known = true;

        let (compute_started_at, compute_unknown_started_at) = {
            let mut next = state.clone();
            compute_unknown_boundary(&mut next, failed_stop);
            (next.compute_started_at, next.compute_unknown_started_at)
        };
        state.compute_started_at = compute_started_at;
        state.compute_unknown_started_at = compute_unknown_started_at;

        assert_eq!(state.compute_started_at, None);
        assert_eq!(state.compute_unknown_started_at, Some(failed_stop));
        assert_eq!(state.storage_started_at, Some(ready));
        assert!(state.storage_known);

        let storage = storage_quantities(&state.approved_resources, ready, deletion)?;
        assert_eq!(storage.storage_byte_seconds, 20);
        Ok(())
    }

    #[test]
    fn quantities_use_millisecond_intervals_without_rounding_up()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut state = state();
        state.approved_resources = WorkloadResources {
            cpu_millicores: 500,
            memory_bytes: 2_000,
            storage_bytes: 7_000,
            gpu: None,
        };
        let known = quantities(
            &state.approved_resources,
            Some(&GpuAllocation {
                entry_id: GpuCatalogEntryId::new(),
                class: "gpu".to_owned(),
                count: 2,
                mode: GpuAllocationMode::Exclusive,
                provider_binding: "provider".to_owned(),
                allocation_binding: "allocation".to_owned(),
                catalog_revision: Revision::new(1)?,
            }),
            timestamp_millis(0),
            timestamp_millis(1_200),
        )?;
        assert_eq!(known.cpu_millicore_seconds, 600);
        assert_eq!(known.memory_byte_seconds, 2_400);
        assert_eq!(known.gpu_unit_seconds, 2);

        let subsecond = storage_quantities(
            &state.approved_resources,
            timestamp_millis(0),
            timestamp_millis(400),
        )?;
        assert_eq!(subsecond.storage_byte_seconds, 2_800);
        Ok(())
    }
}
