//! Resource is the sole build pricing/ledger owner. Agent supplies its existing SQLx transaction.
use crate::store::{PgResourceStore, ResourceStoreError};
use contracts::resource::build_costing::{
    BuildCostEstimate, BuildCostEstimateRequest, BuildCostRecord, BuildUsageMeasurement,
};
use contracts::resource::credits::CreditHoldResolutionRequest;
use contracts::resource::{FixedDecimal, Money, ResourceBillingUnit};
use contracts::supply_chain::BuildRequest;
use contracts::{BuildRequestId, ProjectId, UtcTimestamp};
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

pub async fn estimate_in_transaction(
    tx: &mut Transaction<'_, Postgres>,
    project: ProjectId,
    input: &BuildCostEstimateRequest,
) -> Result<BuildCostEstimate, ResourceStoreError> {
    if input.builder_binding.trim().is_empty()
        || input.builder_binding.len() > 128
        || input.max_duration_milliseconds == 0
    {
        return Err(ResourceStoreError::ScopeConflict);
    }
    let millis = i64::try_from(input.max_duration_milliseconds)
        .map_err(|_| ResourceStoreError::NumericOverflow)?;
    let until = UtcTimestamp::from_utc(
        input
            .from
            .get()
            .checked_add(time::Duration::milliseconds(millis))
            .ok_or(ResourceStoreError::NumericOverflow)?,
    )?;
    let (lines, total) = price(tx, &input.builder_binding, input.from, until).await?;
    Ok(BuildCostEstimate {
        project_id: project,
        input: input.clone(),
        until,
        lines,
        total,
    })
}

async fn price(
    tx: &mut Transaction<'_, Postgres>,
    builder: &str,
    from: UtcTimestamp,
    until: UtcTimestamp,
) -> Result<(Vec<contracts::resource::ResourceChargeLine>, Money), ResourceStoreError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('resource_rates', 0))")
        .execute(&mut **tx)
        .await?;
    let quantity = u64::try_from((until.get() - from.get()).whole_milliseconds())
        .map_err(|_| ResourceStoreError::NumericOverflow)?;
    if quantity == 0 {
        return Err(ResourceStoreError::NumericOverflow);
    }
    let mut lines = Vec::new();
    let mut currency = None;
    crate::pricing::append_rate_segments(
        tx,
        from,
        until,
        1000,
        ResourceBillingUnit::BuildSecond,
        quantity,
        None,
        None,
        Some(builder),
        &mut lines,
        &mut currency,
    )
    .await?;
    if currency.as_deref() != Some("CREDITS") {
        return Err(ResourceStoreError::RateCurrencyConflict);
    }
    let sum = lines.iter().try_fold(0_i128, |v, l| {
        v.checked_add(l.amount.amount.scaled())
            .ok_or(ResourceStoreError::NumericOverflow)
    })?;
    Ok((
        lines,
        Money {
            currency: "CREDITS".into(),
            amount: FixedDecimal::from_scaled(sum)?,
        },
    ))
}

/// Called while the real Agent command and its attempt are locked; no remote operation runs here.
pub async fn admit_in_transaction(
    tx: &mut Transaction<'_, Postgres>,
    owner: &str,
    request: &BuildRequest,
    attempt: u32,
    lease_token: Uuid,
) -> Result<BuildCostRecord, ResourceStoreError> {
    request
        .validate()
        .map_err(|_| ResourceStoreError::ScopeConflict)?;
    let attempt_db = i32::try_from(attempt).map_err(|_| ResourceStoreError::ScopeConflict)?;
    if owner.is_empty() || attempt == 0 {
        return Err(ResourceStoreError::ScopeConflict);
    }
    let existing=sqlx::query("SELECT * FROM resource.build_metering_attempts WHERE build_request_id=$1 AND attempt=$2 FOR UPDATE").bind(request.id.as_uuid()).bind(attempt_db).fetch_optional(&mut **tx).await?;
    if let Some(row) = existing {
        if row.try_get::<String, _>("owner_client_id")? != owner
            || row.try_get::<Uuid, _>("lease_token")? != lease_token
            || row.try_get::<serde_json::Value, _>("command")? != serde_json::to_value(request)?
        {
            return Err(ResourceStoreError::ScopeConflict);
        }
        let record = decode_record(tx, &row).await?;
        crate::credits::require_funded_in_transaction(
            tx,
            owner,
            record.hold.id,
            &record.estimate.total.amount,
        )
        .await?;
        return Ok(record);
    }
    let from: time::OffsetDateTime =
        sqlx::query_scalar("SELECT date_trunc('milliseconds',clock_timestamp())")
            .fetch_one(&mut **tx)
            .await?;
    let input = BuildCostEstimateRequest {
        builder_binding: request.builder_binding.clone(),
        max_duration_milliseconds: request.max_duration_milliseconds,
        from: UtcTimestamp::from_utc(from)?,
    };
    let estimate = estimate_in_transaction(tx, request.project_id, &input).await?;
    let hold = crate::credits::build_hold_in_transaction(
        tx,
        &format!("build-{}-{attempt}", request.id),
        owner,
        request.project_id,
        request.id,
        attempt,
        &estimate.total.amount,
    )
    .await?;
    crate::credits::require_funded_in_transaction(tx, owner, hold.id, &estimate.total.amount)
        .await?;
    sqlx::query("INSERT INTO resource.build_metering_attempts(build_request_id,attempt,project_id,owner_client_id,lease_token,command,estimate,hold_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8)")
      .bind(request.id.as_uuid()).bind(attempt_db).bind(request.project_id.as_uuid()).bind(owner).bind(lease_token).bind(serde_json::to_value(request)?).bind(serde_json::to_value(&estimate)?).bind(hold.id).execute(&mut **tx).await?;
    Ok(BuildCostRecord {
        build_request_id: request.id,
        attempt,
        estimate,
        hold,
        measurement: None,
        first_unknown_observation: None,
        charged_lines: None,
        charged_total: None,
    })
}

/// A terminal fact is settled together with the Agent transition. Unknown facts remain observable.
pub async fn observe_in_transaction(
    tx: &mut Transaction<'_, Postgres>,
    owner: &str,
    build: BuildRequestId,
    attempt: u32,
    measurement: &BuildUsageMeasurement,
) -> Result<BuildCostRecord, ResourceStoreError> {
    measurement.validate()?;
    let row=sqlx::query("SELECT * FROM resource.build_metering_attempts WHERE build_request_id=$1 AND attempt=$2 FOR UPDATE").bind(build.as_uuid()).bind(i32::try_from(attempt).map_err(|_|ResourceStoreError::ScopeConflict)?).fetch_optional(&mut **tx).await?.ok_or(ResourceStoreError::NotFound)?;
    if row.try_get::<String, _>("owner_client_id")? != owner {
        return Err(ResourceStoreError::ScopeConflict);
    }
    let old = decode_record(tx, &row).await?;
    if old.charged_total.is_some()
        || matches!(
            old.measurement,
            Some(BuildUsageMeasurement::NotStarted { .. })
        )
    {
        if old.measurement.as_ref() != Some(measurement) {
            return Err(ResourceStoreError::UsageConflict);
        }
        return Ok(old);
    }
    if matches!(measurement,BuildUsageMeasurement::Unknown{..}) && old.first_unknown_observation.is_some() {return Ok(old);}
    let resolution = match measurement {
        BuildUsageMeasurement::Known {
            started_at,
            finished_at,
            ..
        } => {
            let (lines, total) = price(
                tx,
                &old.estimate.input.builder_binding,
                *started_at,
                *finished_at,
            )
            .await?;
            sqlx::query("UPDATE resource.build_metering_attempts SET measurement=$3,charged_lines=$4,charged_total=$5 WHERE build_request_id=$1 AND attempt=$2").bind(build.as_uuid()).bind(i32::try_from(attempt).map_err(|_|ResourceStoreError::ScopeConflict)?).bind(serde_json::to_value(measurement)?).bind(serde_json::to_value(lines)?).bind(serde_json::to_value(&total)?).execute(&mut **tx).await?;
            CreditHoldResolutionRequest::Settle {
                event_id: contracts::EventId::new(),
                amount: total.amount,
                finalize: true,
            }
        }
        BuildUsageMeasurement::Unknown { reason } => {
            sqlx::query("UPDATE resource.build_metering_attempts SET measurement=$3,first_unknown_observation=COALESCE(first_unknown_observation,$3) WHERE build_request_id=$1 AND attempt=$2").bind(build.as_uuid()).bind(i32::try_from(attempt).map_err(|_|ResourceStoreError::ScopeConflict)?).bind(serde_json::to_value(measurement)?).execute(&mut **tx).await?;
            CreditHoldResolutionRequest::Unknown {
                reason: reason.clone(),
            }
        }
        BuildUsageMeasurement::NotStarted { reason } => {
            sqlx::query("UPDATE resource.build_metering_attempts SET measurement=$3 WHERE build_request_id=$1 AND attempt=$2").bind(build.as_uuid()).bind(i32::try_from(attempt).map_err(|_|ResourceStoreError::ScopeConflict)?).bind(serde_json::to_value(measurement)?).execute(&mut **tx).await?;
            CreditHoldResolutionRequest::Release {
                reason: reason.clone(),
            }
        }
    };
    crate::credits::resolve_in_transaction(tx, owner, old.hold.id, &resolution).await?;
    let row = sqlx::query(
        "SELECT * FROM resource.build_metering_attempts WHERE build_request_id=$1 AND attempt=$2",
    )
    .bind(build.as_uuid())
    .bind(i32::try_from(attempt).map_err(|_| ResourceStoreError::ScopeConflict)?)
    .fetch_one(&mut **tx)
    .await?;
    decode_record(tx, &row).await
}

async fn decode_record(
    tx: &mut Transaction<'_, Postgres>,
    row: &sqlx::postgres::PgRow,
) -> Result<BuildCostRecord, ResourceStoreError> {
    let hold=sqlx::query("SELECT *,amount::text AS amount_scaled,remaining::text AS remaining_scaled FROM resource.credit_holds WHERE hold_id=$1").bind(row.try_get::<Uuid,_>("hold_id")?).fetch_one(&mut **tx).await?;
    Ok(BuildCostRecord {
        build_request_id: row
            .try_get::<Uuid, _>("build_request_id")?
            .to_string()
            .parse()
            .map_err(|_| ResourceStoreError::Wire)?,
        attempt: u32::try_from(row.try_get::<i32, _>("attempt")?)
            .map_err(|_| ResourceStoreError::Wire)?,
        estimate: serde_json::from_value(row.try_get("estimate")?)?,
        hold: crate::credits::decode_hold(&hold)?,
        measurement: row
            .try_get::<Option<serde_json::Value>, _>("measurement")?
            .map(serde_json::from_value)
            .transpose()?,
        first_unknown_observation: row
            .try_get::<Option<serde_json::Value>, _>("first_unknown_observation")?
            .map(serde_json::from_value)
            .transpose()?,
        charged_lines: row
            .try_get::<Option<serde_json::Value>, _>("charged_lines")?
            .map(serde_json::from_value)
            .transpose()?,
        charged_total: row
            .try_get::<Option<serde_json::Value>, _>("charged_total")?
            .map(serde_json::from_value)
            .transpose()?,
    })
}

impl PgResourceStore {
    pub async fn estimate_build_cost(
        &self,
        project: ProjectId,
        input: &BuildCostEstimateRequest,
    ) -> Result<BuildCostEstimate, ResourceStoreError> {
        let mut tx = self.pool().begin().await?;
        let estimate = estimate_in_transaction(&mut tx, project, input).await?;
        tx.commit().await?;
        Ok(estimate)
    }
    pub async fn build_cost_records(
        &self,
        project: ProjectId,
    ) -> Result<Vec<BuildCostRecord>, ResourceStoreError> {
        let mut tx = self.pool().begin().await?;
        let rows=sqlx::query("SELECT * FROM resource.build_metering_attempts WHERE project_id=$1 ORDER BY created_at DESC,build_request_id,attempt LIMIT 100").bind(project.as_uuid()).fetch_all(&mut *tx).await?;
        let mut records = Vec::new();
        for row in rows {
            records.push(decode_record(&mut tx, &row).await?);
        }
        tx.commit().await?;
        Ok(records)
    }
}

pub(crate) async fn settled_after_boundary(
    tx: &mut Transaction<'_, Postgres>,
    rate: contracts::RateId,
    boundary: UtcTimestamp,
) -> Result<bool, ResourceStoreError> {
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM resource.build_metering_attempts WHERE charged_total IS NOT NULL AND (measurement->>'finishedAt')::timestamptz>$1 AND EXISTS(SELECT 1 FROM jsonb_array_elements(charged_lines) line WHERE line->>'rateId'=$2))").bind(boundary.get()).bind(rate.to_string()).fetch_one(&mut **tx).await?)
}
