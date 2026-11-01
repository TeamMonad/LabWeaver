//! Environment-owned disk facts, independent of the retired compute generation.
use contracts::UtcTimestamp;
use contracts::environment::{EnvironmentDiskState, EnvironmentInstance, ObservedEnvironmentState};

/// Called only after a successful physical cleanup observation, using the owner's database clock.
pub(crate) fn record_disk_observation(
    instance: &mut EnvironmentInstance,
    now: UtcTimestamp,
) -> Result<(), crate::LifecycleError> {
    if instance.observed_state != ObservedEnvironmentState::Deleted {
        return Ok(());
    }
    if let Some(disk) = &mut instance.disk_retention {
        match disk.state {
            EnvironmentDiskState::Retiring => {
                disk.started_at = now;
                disk.expires_at = UtcTimestamp::from_utc(now.get() + time::Duration::days(7))
                    .map_err(|_| crate::LifecycleError::CommandInvalid)?;
                disk.compute_retired_at = Some(now);
                disk.state = EnvironmentDiskState::Retained;
            }
            EnvironmentDiskState::Purging => {
                disk.purged_at = Some(now);
                disk.state = EnvironmentDiskState::Purged;
            }
            _ => {}
        }
    }
    instance.validate()?;
    Ok(())
}

pub(crate) async fn record_storage_usage(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    instance: &EnvironmentInstance,
    now: UtcTimestamp,
) -> Result<(), crate::EnvironmentStoreError> {
    let Some(disk) = &instance.disk_retention else {
        return Ok(());
    };
    if disk.compute_retired_at.is_none() {
        return Ok(());
    }
    let until = disk.purged_at.unwrap_or(now);
    let closed = matches!(
        disk.state,
        EnvironmentDiskState::Restored | EnvironmentDiskState::Purged
    );
    sqlx::query("INSERT INTO environment.retained_disk_usage (environment_id,started_at,measured_until,storage_bytes,closed_at) VALUES ($1,$2,$3,$4::text::numeric,CASE WHEN $5 THEN $3 ELSE NULL END) ON CONFLICT (environment_id,started_at) DO UPDATE SET measured_until=GREATEST(retained_disk_usage.measured_until,EXCLUDED.measured_until),closed_at=EXCLUDED.closed_at WHERE retained_disk_usage.closed_at IS NULL")
        .bind(instance.id.as_uuid()).bind(disk.started_at.get()).bind(until.get())
        .bind(instance.approved_resources.storage_bytes.to_string()).bind(closed)
        .execute(&mut **transaction).await?;
    Ok(())
}
