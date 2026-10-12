//! Versioned price lookup and exact usage calculation. The caller owns the transaction.
use crate::store::{ResourceStoreError, decode_rate, load_usage_gpu_allocation, wire};
use contracts::resource::{
    FixedDecimal, GpuAllocationMode, Money, ResourceBillingUnit, ResourceCharge,
    ResourceChargeLine, ResourceRate, ResourceUsageKind, ResourceUsageRecord, UsageMeasurement,
    UsageSettlementState,
};
use contracts::{ChargeId, UtcTimestamp};
use sqlx::{Postgres, Row, Transaction};
use std::cmp::{max, min};
pub(crate) async fn calculate_charge(
    transaction: &mut Transaction<'_, Postgres>,
    usage: &ResourceUsageRecord,
) -> Result<Option<ResourceCharge>, ResourceStoreError> {
    let UsageMeasurement::Known { quantities } = &usage.measurement else {
        return Ok(None);
    };
    let mut lines = Vec::new();
    let mut currency: Option<String> = None;

    if usage.kind == ResourceUsageKind::Compute {
        if quantities.cpu_millicore_seconds > 0 {
            append_rate_segments(
                transaction,
                usage.measured_from,
                usage.measured_until,
                quantities.quantity_divisor,
                ResourceBillingUnit::CpuMillicoreSecond,
                quantities.cpu_millicore_seconds,
                None,
                None,
                &mut lines,
                &mut currency,
            )
            .await?;
        }
        if quantities.memory_byte_seconds > 0 {
            append_rate_segments(
                transaction,
                usage.measured_from,
                usage.measured_until,
                quantities.quantity_divisor,
                ResourceBillingUnit::MemoryByteSecond,
                quantities.memory_byte_seconds,
                None,
                None,
                &mut lines,
                &mut currency,
            )
            .await?;
        }
        if quantities.gpu_unit_seconds > 0 {
            let allocation = load_usage_gpu_allocation(transaction, usage).await?;
            append_rate_segments(
                transaction,
                usage.measured_from,
                usage.measured_until,
                quantities.quantity_divisor,
                ResourceBillingUnit::GpuUnitSecond,
                quantities.gpu_unit_seconds,
                Some(&allocation.class),
                Some(allocation.mode),
                &mut lines,
                &mut currency,
            )
            .await?;
        }
    } else if quantities.storage_byte_seconds > 0 {
        append_rate_segments(
            transaction,
            usage.measured_from,
            usage.measured_until,
            quantities.quantity_divisor,
            ResourceBillingUnit::StorageByteSecond,
            quantities.storage_byte_seconds,
            None,
            None,
            &mut lines,
            &mut currency,
        )
        .await?;
    }

    if lines.is_empty() {
        return Ok(None);
    }
    let currency = currency.ok_or(ResourceStoreError::RateCurrencyConflict)?;
    let total_scaled = lines.iter().try_fold(0_i128, |total, line| {
        total
            .checked_add(line.amount.amount.scaled())
            .ok_or(ResourceStoreError::NumericOverflow)
    })?;
    let total = Money {
        currency,
        amount: FixedDecimal::from_scaled(total_scaled)?,
    };
    Ok(Some(ResourceCharge {
        id: ChargeId::new(),
        usage_record_id: usage.id,
        project_id: usage.project_id,
        course_id: usage.course_id,
        lines,
        total,
        settlement: UsageSettlementState::Settled,
        // The settle transaction replaces this with the database timestamp.
        created_at: usage.observed_at,
        adjustment_of: None,
        adjustment_reason: None,
        adjusted_by: None,
        diagnostic_code: None,
    }))
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn append_rate_segments(
    transaction: &mut Transaction<'_, Postgres>,
    from: UtcTimestamp,
    until: UtcTimestamp,
    quantity_divisor: u64,
    unit: ResourceBillingUnit,
    quantity: u64,
    gpu_class: Option<&str>,
    gpu_mode: Option<GpuAllocationMode>,
    lines: &mut Vec<ResourceChargeLine>,
    currency: &mut Option<String>,
) -> Result<(), ResourceStoreError> {
    let segments =
        select_rate_segments(transaction, unit, gpu_class, gpu_mode, from, until).await?;
    let total_millis = (until.get() - from.get()).whole_milliseconds();
    if total_millis <= 0 {
        return Err(ResourceStoreError::NumericOverflow);
    }
    let total_millis =
        u128::try_from(total_millis).map_err(|_| ResourceStoreError::NumericOverflow)?;
    let mut allocated = 0_u64;
    for (index, (rate, segment_millis)) in segments.iter().enumerate() {
        let segment_quantity = if index + 1 == segments.len() {
            quantity
                .checked_sub(allocated)
                .ok_or(ResourceStoreError::NumericOverflow)?
        } else {
            let product = u128::from(quantity)
                .checked_mul(*segment_millis)
                .ok_or(ResourceStoreError::NumericOverflow)?;
            u64::try_from(product / total_millis)
                .map_err(|_| ResourceStoreError::NumericOverflow)?
        };
        allocated = allocated
            .checked_add(segment_quantity)
            .ok_or(ResourceStoreError::NumericOverflow)?;
        append_charge_line(
            unit,
            segment_quantity,
            quantity_divisor,
            rate,
            lines,
            currency,
        )?;
    }
    if allocated != quantity {
        return Err(ResourceStoreError::NumericOverflow);
    }
    Ok(())
}

fn append_charge_line(
    unit: ResourceBillingUnit,
    quantity: u64,
    quantity_divisor: u64,
    rate: &ResourceRate,
    lines: &mut Vec<ResourceChargeLine>,
    currency: &mut Option<String>,
) -> Result<(), ResourceStoreError> {
    if quantity == 0 {
        return Ok(());
    }
    if let Some(expected) = currency.as_ref()
        && expected != &rate.unit_price.currency
    {
        return Err(ResourceStoreError::RateCurrencyConflict);
    }
    *currency = Some(rate.unit_price.currency.clone());
    let amount = multiply_money(
        &rate.unit_price,
        quantity,
        rate.unit_quantity,
        quantity_divisor,
    )?;
    lines.push(ResourceChargeLine {
        quantity_divisor,
        rate_id: rate.id,
        rate_revision: rate.revision,
        unit,
        quantity,
        unit_quantity: rate.unit_quantity,
        unit_price: rate.unit_price.clone(),
        amount,
    });
    Ok(())
}

async fn select_rate_segments(
    transaction: &mut Transaction<'_, Postgres>,
    unit: ResourceBillingUnit,
    gpu_class: Option<&str>,
    gpu_mode: Option<GpuAllocationMode>,
    from: UtcTimestamp,
    until: UtcTimestamp,
) -> Result<Vec<(ResourceRate, u128)>, ResourceStoreError> {
    let unit = wire(unit)?;
    let gpu_mode = gpu_mode.map(wire).transpose()?;
    let rows = sqlx::query(
        "SELECT contract FROM resource.resource_rates
         WHERE unit=$1 AND gpu_class IS NOT DISTINCT FROM $2
           AND gpu_mode IS NOT DISTINCT FROM $3
           AND effective_from < $4
           AND (effective_until IS NULL OR effective_until > $5)
         ORDER BY effective_from ASC, revision ASC, rate_id ASC",
    )
    .bind(&unit)
    .bind(gpu_class)
    .bind(gpu_mode.as_deref())
    .bind(until.get())
    .bind(from.get())
    .fetch_all(&mut **transaction)
    .await?;
    if rows.is_empty() {
        return Err(ResourceStoreError::RateUnconfigured);
    }
    let from = from.get();
    let until = until.get();
    let mut cursor = from;
    let mut segments = Vec::with_capacity(rows.len());
    for row in rows {
        let rate = decode_rate(row.try_get("contract")?)?;
        let segment_from = max(from, rate.effective_from.get());
        let segment_until = min(until, rate.effective_until.map_or(until, UtcTimestamp::get));
        if segment_until <= segment_from {
            continue;
        }
        if segment_from < cursor {
            return Err(ResourceStoreError::RateOverlap);
        }
        if segment_from > cursor {
            return Err(ResourceStoreError::RateUnconfigured);
        }
        let duration = (segment_until - segment_from).whole_milliseconds();
        let duration = u128::try_from(duration).map_err(|_| ResourceStoreError::NumericOverflow)?;
        if duration == 0 {
            return Err(ResourceStoreError::NumericOverflow);
        }
        segments.push((rate, duration));
        cursor = segment_until;
    }
    if cursor != until {
        return Err(ResourceStoreError::RateUnconfigured);
    }
    Ok(segments)
}

fn multiply_money(
    unit_price: &Money,
    quantity: u64,
    unit_quantity: u64,
    quantity_divisor: u64,
) -> Result<Money, ResourceStoreError> {
    Ok(Money {
        currency: unit_price.currency.clone(),
        amount: FixedDecimal::from_scaled(contracts::resource::rounded_scaled_amount(
            unit_price.amount.scaled(),
            quantity,
            unit_quantity,
            quantity_divisor,
        )?)?,
    })
}
