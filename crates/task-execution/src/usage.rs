//! Usage deliveries one finished one-shot workload owes Resource.
//!
//! Every task owner service that reserves capacity through Resource owes the same observation for
//! the interval its workload occupied the reservation: the compute quantities derived from the
//! reviewed claim and the observed container timing, or an explicit unknown measurement when the
//! timing is unavailable. Deriving it once keeps Agent authoring and Evaluation delivery identical
//! instead of leaving one of them to invent a second accounting.

use contracts::http::{RecordResourceUsageRequest, TaskResourceStatus};
use contracts::resource::{ResourceUsageKind, ResourceUsageQuantities, UsageMeasurement};
use std::str::FromStr;

use contracts::{EventId, TaskRunId, UtcTimestamp};
use thiserror::Error;
use uuid::Uuid;

use crate::timing::ExecutionTiming;

/// Failure to derive the usage deliveries of one finished workload.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum UsageDeliveryError {
    /// The observed timing is not a valid half-open interval.
    #[error("LW_TASK_EXECUTION_USAGE_TIMING_INVALID")]
    TimingInvalid,
    /// A derived quantity does not fit the contract.
    #[error("LW_TASK_EXECUTION_USAGE_QUANTITY_OVERFLOW")]
    QuantityOverflow,
}

/// Builds the compute and storage deliveries one finished workload owes Resource.
///
/// The compute delivery always exists. The storage delivery exists only when the reviewed claim
/// reserved storage, and it carries no compute quantity. `fallback_until` bounds the interval when
/// the workload left no container timing behind: the reservation is then reported as an explicitly
/// unknown measurement instead of a fabricated zero.
///
/// # Errors
///
/// Returns [`UsageDeliveryError::TimingInvalid`] for an inconsistent interval and
/// [`UsageDeliveryError::QuantityOverflow`] for a quantity the contract cannot represent.
pub fn usage_deliveries(
    status: &TaskResourceStatus,
    timing: ExecutionTiming,
    fallback_until: UtcTimestamp,
) -> Result<Vec<RecordResourceUsageRequest>, UsageDeliveryError> {
    timing
        .validate()
        .map_err(|_| UsageDeliveryError::TimingInvalid)?;
    let (measured_from, measured_until, measurement) =
        match (timing.started_at, timing.terminated_at) {
            (Some(started), Some(terminated)) => {
                let milliseconds = usage_milliseconds(started, terminated)?;
                let resources = &status.claim.workload_resources;
                let quantities = ResourceUsageQuantities {
                    cpu_millicore_seconds: quantity_per_millisecond(
                        u64::from(resources.cpu_millicores),
                        milliseconds,
                    )?,
                    memory_byte_seconds: quantity_per_millisecond(
                        resources.memory_bytes,
                        milliseconds,
                    )?,
                    storage_byte_seconds: 0,
                    gpu_unit_seconds: match resources.gpu.as_ref() {
                        Some(gpu) => quantity_per_millisecond(u64::from(gpu.count), milliseconds)?,
                        None => 0,
                    },
                };
                (started, terminated, UsageMeasurement::Known { quantities })
            }
            (None, None) => {
                let started = status
                    .lease
                    .active_from
                    .ok_or(UsageDeliveryError::TimingInvalid)?;
                if fallback_until <= started {
                    return Err(UsageDeliveryError::TimingInvalid);
                }
                (
                    started,
                    fallback_until,
                    UsageMeasurement::Unknown {
                        reason: "executor_timing_unavailable".to_owned(),
                    },
                )
            }
            _ => return Err(UsageDeliveryError::TimingInvalid),
        };
    let compute = RecordResourceUsageRequest {
        project_id: status.project_id,
        course_id: status.request.course_id,
        kind: ResourceUsageKind::Compute,
        request_id: status.request.id,
        lease_id: Some(status.lease.id),
        source_event_id: deterministic_usage_event_id(status.task_run_id, 0x01)?,
        measured_from,
        measured_until,
        measurement: measurement.clone(),
    };
    let storage = status.claim.workload_resources.storage_bytes;
    let mut deliveries = vec![compute];
    if storage > 0 {
        let storage_measurement = match measurement {
            UsageMeasurement::Known { .. } => UsageMeasurement::Known {
                quantities: ResourceUsageQuantities {
                    cpu_millicore_seconds: 0,
                    memory_byte_seconds: 0,
                    storage_byte_seconds: quantity_per_millisecond(
                        storage,
                        usage_milliseconds(measured_from, measured_until)?,
                    )?,
                    gpu_unit_seconds: 0,
                },
            },
            UsageMeasurement::Unknown { reason } => UsageMeasurement::Unknown { reason },
        };
        deliveries.push(RecordResourceUsageRequest {
            project_id: status.project_id,
            course_id: status.request.course_id,
            kind: ResourceUsageKind::Storage,
            request_id: status.request.id,
            lease_id: Some(status.lease.id),
            source_event_id: deterministic_usage_event_id(status.task_run_id, 0x02)?,
            measured_from,
            measured_until,
            measurement: storage_measurement,
        });
    }
    Ok(deliveries)
}

fn usage_milliseconds(
    measured_from: UtcTimestamp,
    measured_until: UtcTimestamp,
) -> Result<u64, UsageDeliveryError> {
    if measured_until <= measured_from {
        return Err(UsageDeliveryError::TimingInvalid);
    }
    u64::try_from(
        (measured_until.get() - measured_from.get())
            .whole_milliseconds()
            .max(1),
    )
    .map_err(|_| UsageDeliveryError::QuantityOverflow)
}

fn quantity_per_millisecond(base: u64, milliseconds: u64) -> Result<u64, UsageDeliveryError> {
    u64::try_from(
        u128::from(base)
            .checked_mul(u128::from(milliseconds))
            .ok_or(UsageDeliveryError::QuantityOverflow)?
            / 1_000,
    )
    .map_err(|_| UsageDeliveryError::QuantityOverflow)
}

fn deterministic_usage_event_id(
    task_run_id: TaskRunId,
    discriminator: u8,
) -> Result<EventId, UsageDeliveryError> {
    let mut bytes = task_run_id.as_uuid().into_bytes();
    // Preserve UUIDv7 version and RFC 9562 variant while deriving stable, category-specific ids
    // from the durable TaskRunId.
    bytes[14] = bytes[14].wrapping_add(discriminator);
    bytes[15] ^= discriminator;
    EventId::from_str(&Uuid::from_bytes(bytes).to_string())
        .map_err(|_| UsageDeliveryError::TimingInvalid)
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use contracts::{EventId, TaskRunId, UtcTimestamp};

    use super::{deterministic_usage_event_id, quantity_per_millisecond, usage_milliseconds};

    fn timestamp(value: &str) -> Result<UtcTimestamp, Box<dyn Error>> {
        Ok(value.parse::<UtcTimestamp>()?)
    }

    #[test]
    fn one_minute_of_two_cores_is_millicore_seconds() -> Result<(), Box<dyn Error>> {
        let from = timestamp("2026-09-22T10:00:00.000Z")?;
        let until = timestamp("2026-09-22T10:01:00.000Z")?;
        let milliseconds = usage_milliseconds(from, until).map_err(|error| error.to_string())?;
        assert_eq!(milliseconds, 60_000);
        assert_eq!(
            quantity_per_millisecond(2_000, milliseconds).ok(),
            Some(120_000)
        );
        assert_eq!(
            quantity_per_millisecond(1_073_741_824, milliseconds).ok(),
            Some(64_424_509_440)
        );
        Ok(())
    }

    #[test]
    fn a_zero_length_interval_fails_closed() -> Result<(), Box<dyn Error>> {
        let instant = timestamp("2026-09-22T10:00:00.000Z")?;
        assert!(usage_milliseconds(instant, instant).is_err());
        Ok(())
    }

    #[test]
    fn a_quantity_that_does_not_fit_the_contract_fails_closed() {
        assert!(quantity_per_millisecond(u64::MAX, u64::MAX).is_err());
    }

    #[test]
    fn derived_event_ids_are_stable_and_distinct_per_category() -> Result<(), Box<dyn Error>> {
        let task_run_id = TaskRunId::new();
        let compute =
            deterministic_usage_event_id(task_run_id, 0x01).map_err(|error| error.to_string())?;
        let storage =
            deterministic_usage_event_id(task_run_id, 0x02).map_err(|error| error.to_string())?;
        assert_ne!(compute, storage);
        assert_eq!(
            deterministic_usage_event_id(task_run_id, 0x01).ok(),
            Some(compute)
        );
        assert_ne!(
            deterministic_usage_event_id(TaskRunId::new(), 0x01).ok(),
            Some(compute)
        );
        assert!(!compute.as_uuid().is_nil());
        assert_ne!(compute, EventId::new());
        Ok(())
    }
}
