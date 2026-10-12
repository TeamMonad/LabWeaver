//! Resource owns build prices, preauthorization and settlement in its own SQLx transactions.
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

/// Called by the Resource coordinator after reading the authoritative Agent command.
pub(crate) async fn admit_in_transaction(
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
        execution_authorized: false,
        execution_deadline: None,
        stage_request_id: None,
        estimate,
        hold,
        measurement: None,
        first_unknown_observation: None,
        charged_lines: None,
        charged_total: None,
    })
}

/// Persists the provider fact and its ledger effect atomically. Unknown facts remain observable.
#[allow(
    clippy::too_many_lines,
    reason = "history fact, price coverage and ledger effect are one atomic observation"
)]
pub(crate) async fn observe_in_transaction(
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
        if old.measurement.as_ref() != Some(measurement)
            && !matches!(measurement, BuildUsageMeasurement::Unknown { .. })
        {
            return Err(ResourceStoreError::UsageConflict);
        }
        return Ok(old);
    }
    if matches!(measurement, BuildUsageMeasurement::Unknown { .. })
        && old.first_unknown_observation.is_some()
    {
        return Ok(old);
    }
    if matches!(old.measurement, Some(BuildUsageMeasurement::Known { .. }))
        && old.measurement.as_ref() != Some(measurement)
    {
        if matches!(measurement, BuildUsageMeasurement::Unknown { .. }) {
            return Ok(old);
        }
        return Err(ResourceStoreError::UsageConflict);
    }
    if matches!(measurement, BuildUsageMeasurement::Known { .. }) && !old.execution_authorized {
        return Err(ResourceStoreError::ScopeConflict);
    }
    let resolution = match measurement {
        BuildUsageMeasurement::Known {
            started_at,
            finished_at,
            ..
        } => {
            let priced = price(
                tx,
                &old.estimate.input.builder_binding,
                *started_at,
                *finished_at,
            )
            .await;
            let (lines, total) = match priced {
                Ok(value) => value,
                Err(
                    ResourceStoreError::RateUnconfigured | ResourceStoreError::RateCurrencyConflict,
                ) => {
                    // Keep the actual history interval. Missing prices are financial unknowns, never zero usage.
                    let unknown = BuildUsageMeasurement::Unknown {
                        reason: "authoritative build history has incomplete CREDITS price coverage"
                            .into(),
                    };
                    sqlx::query("UPDATE resource.build_metering_attempts SET measurement=$3,first_unknown_observation=COALESCE(first_unknown_observation,$4) WHERE build_request_id=$1 AND attempt=$2").bind(build.as_uuid()).bind(i32::try_from(attempt).map_err(|_|ResourceStoreError::ScopeConflict)?).bind(serde_json::to_value(measurement)?).bind(serde_json::to_value(&unknown)?).execute(&mut **tx).await?;
                    if old.first_unknown_observation.is_none() {
                        crate::credits::resolve_in_transaction(
                            tx,
                            owner,
                            old.hold.id,
                            &CreditHoldResolutionRequest::Unknown {
                                reason: "build history awaiting configured prices".into(),
                            },
                        )
                        .await?;
                    }
                    let row = sqlx::query("SELECT * FROM resource.build_metering_attempts WHERE build_request_id=$1 AND attempt=$2").bind(build.as_uuid()).bind(i32::try_from(attempt).map_err(|_|ResourceStoreError::ScopeConflict)?).fetch_one(&mut **tx).await?;
                    return decode_record(tx, &row).await;
                }
                Err(error) => return Err(error),
            };
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
        execution_authorized: row.try_get("execution_authorized")?,
        execution_deadline: row
            .try_get::<Option<time::OffsetDateTime>, _>("execution_deadline")?
            .map(UtcTimestamp::from_utc)
            .transpose()?,
        stage_request_id: row.try_get("build_stage_request_id")?,
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
}

pub(crate) async fn settled_after_boundary(
    tx: &mut Transaction<'_, Postgres>,
    rate: contracts::RateId,
    boundary: UtcTimestamp,
) -> Result<bool, ResourceStoreError> {
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM resource.build_metering_attempts WHERE charged_total IS NOT NULL AND (measurement->>'finishedAt')::timestamptz>$1 AND EXISTS(SELECT 1 FROM jsonb_array_elements(charged_lines) line WHERE line->>'rateId'=$2))").bind(boundary.get()).bind(rate.to_string()).fetch_one(&mut **tx).await?)
}

impl PgResourceStore {
    /// The authenticated Agent identity is checked by the API before this bounded authority read.
    pub async fn admit_build_cost(
        &self,
        owner: &str,
        input: &contracts::resource::build_costing::BuildCostAdmissionRequest,
    ) -> Result<BuildCostRecord, ResourceStoreError> {
        if input.attempt == 0
            || input.stage_request_id.len() != 64
            || !input
                .stage_request_id
                .bytes()
                .all(|c| c.is_ascii_hexdigit())
        {
            return Err(ResourceStoreError::ScopeConflict);
        }
        let mut tx = self.pool().begin().await?;
        let row=sqlx::query("SELECT command,state,attempt,lease_token,lease_expires_at,cancellation_requested FROM agent.build_commands WHERE build_request_id=$1").bind(input.build_request_id.as_uuid()).fetch_optional(&mut *tx).await?.ok_or(ResourceStoreError::NotFound)?;
        let command: contracts::events::AgentBuildRequested =
            serde_json::from_value(row.try_get("command")?)?;
        let now: time::OffsetDateTime =
            sqlx::query_scalar("SELECT date_trunc('milliseconds',clock_timestamp())")
                .fetch_one(&mut *tx)
                .await?;
        if row.try_get::<String, _>("state")? != "running"
            || row.try_get::<bool, _>("cancellation_requested")?
            || row.try_get::<i32, _>("attempt")?
                != i32::try_from(input.attempt).map_err(|_| ResourceStoreError::ScopeConflict)?
            || row.try_get::<Option<Uuid>, _>("lease_token")? != Some(input.lease_token)
            || row
                .try_get::<Option<time::OffsetDateTime>, _>("lease_expires_at")?
                .is_none_or(|t| t <= now)
            || input.execution_deadline.get() <= now
            || input.execution_deadline.get()
                > now
                    + time::Duration::milliseconds(
                        i64::try_from(command.request.max_duration_milliseconds)
                            .map_err(|_| ResourceStoreError::NumericOverflow)?,
                    )
        {
            return Err(ResourceStoreError::ScopeConflict);
        }
        if !matches!(
            command.request.source,
            contracts::supply_chain::BuildSource::Dockerfile { .. }
        ) {
            return Err(ResourceStoreError::ScopeConflict);
        }
        let record = admit_in_transaction(
            &mut tx,
            owner,
            &command.request,
            input.attempt,
            input.lease_token,
        )
        .await?;
        let updated=sqlx::query("UPDATE resource.build_metering_attempts SET execution_deadline=COALESCE(execution_deadline,$3),build_stage_request_id=COALESCE(build_stage_request_id,$4),execution_authorized=execution_authorized OR $5 WHERE build_request_id=$1 AND attempt=$2 AND (execution_deadline IS NULL OR execution_deadline=$3) AND (build_stage_request_id IS NULL OR build_stage_request_id=$4)").bind(input.build_request_id.as_uuid()).bind(i32::try_from(input.attempt).map_err(|_|ResourceStoreError::ScopeConflict)?).bind(input.execution_deadline.get()).bind(&input.stage_request_id).bind(input.authorize_execution).execute(&mut *tx).await?;
        if updated.rows_affected() != 1 {
            return Err(ResourceStoreError::ScopeConflict);
        }
        tx.commit().await?;
        Ok(BuildCostRecord {
            execution_authorized: input.authorize_execution || record.execution_authorized,
            execution_deadline: Some(input.execution_deadline),
            stage_request_id: Some(input.stage_request_id.clone()),
            ..record
        })
    }
    pub async fn get_build_cost(
        &self,
        owner: &str,
        build: BuildRequestId,
        attempt: u32,
    ) -> Result<BuildCostRecord, ResourceStoreError> {
        let mut tx = self.pool().begin().await?;
        let row=sqlx::query("SELECT * FROM resource.build_metering_attempts WHERE build_request_id=$1 AND attempt=$2 AND owner_client_id=$3").bind(build.as_uuid()).bind(i32::try_from(attempt).map_err(|_|ResourceStoreError::ScopeConflict)?).bind(owner).fetch_optional(&mut *tx).await?.ok_or(ResourceStoreError::NotFound)?;
        let record = decode_record(&mut tx, &row).await?;
        tx.commit().await?;
        Ok(record)
    }
    pub async fn observe_build_cost(
        &self,
        owner: &str,
        input: &contracts::resource::build_costing::ObserveBuildCostRequest,
    ) -> Result<BuildCostRecord, ResourceStoreError> {
        let mut tx = self.pool().begin().await?;
        let token:Option<Uuid>=sqlx::query_scalar("SELECT lease_token FROM resource.build_metering_attempts WHERE build_request_id=$1 AND attempt=$2 AND owner_client_id=$3").bind(input.build_request_id.as_uuid()).bind(i32::try_from(input.attempt).map_err(|_|ResourceStoreError::ScopeConflict)?).bind(owner).fetch_optional(&mut *tx).await?;
        if token != Some(input.lease_token) {
            return Err(ResourceStoreError::ScopeConflict);
        }
        let record = observe_in_transaction(
            &mut tx,
            owner,
            input.build_request_id,
            input.attempt,
            &input.measurement,
        )
        .await?;
        tx.commit().await?;
        Ok(record)
    }
}

impl PgResourceStore {
    pub async fn get_project_build_cost(
        &self,
        project: ProjectId,
        build: BuildRequestId,
        attempt: u32,
    ) -> Result<BuildCostRecord, ResourceStoreError> {
        let mut tx = self.pool().begin().await?;
        let row=sqlx::query("SELECT * FROM resource.build_metering_attempts WHERE project_id=$1 AND build_request_id=$2 AND attempt=$3").bind(project.as_uuid()).bind(build.as_uuid()).bind(i32::try_from(attempt).map_err(|_|ResourceStoreError::ScopeConflict)?).fetch_optional(&mut *tx).await?.ok_or(ResourceStoreError::NotFound)?;
        let record = decode_record(&mut tx, &row).await?;
        tx.commit().await?;
        Ok(record)
    }
}

impl PgResourceStore {
    pub async fn get_build_cost_with_fence(
        &self,
        owner: &str,
        build: BuildRequestId,
        attempt: u32,
        token: Uuid,
    ) -> Result<BuildCostRecord, ResourceStoreError> {
        let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM resource.build_metering_attempts WHERE build_request_id=$1 AND attempt=$2 AND owner_client_id=$3 AND lease_token=$4)").bind(build.as_uuid()).bind(i32::try_from(attempt).map_err(|_|ResourceStoreError::ScopeConflict)?).bind(owner).bind(token).fetch_one(&self.pool()).await?;
        if !exists {
            return Err(ResourceStoreError::NotFound);
        }
        self.get_build_cost(owner, build, attempt).await
    }
}

impl PgResourceStore {
    /// A crashed sender cannot leave a pending build preauthorization frozen indefinitely.
    pub async fn reconcile_build_deadlines_once(&self) -> Result<bool, ResourceStoreError> {
        let rows=sqlx::query("SELECT build_request_id,attempt,owner_client_id,execution_authorized FROM resource.build_metering_attempts WHERE charged_total IS NULL AND first_unknown_observation IS NULL AND execution_deadline<=clock_timestamp() AND (measurement IS NULL OR measurement->>'state'<>'not_started') ORDER BY execution_deadline,build_request_id LIMIT 100").fetch_all(&self.pool()).await?;
        let changed = !rows.is_empty();
        for row in rows {
            let mut tx = self.pool().begin().await?;
            let build: BuildRequestId = row
                .try_get::<Uuid, _>("build_request_id")?
                .to_string()
                .parse()
                .map_err(|_| ResourceStoreError::Wire)?;
            let attempt = u32::try_from(row.try_get::<i32, _>("attempt")?)
                .map_err(|_| ResourceStoreError::Wire)?;
            let owner: String = row.try_get("owner_client_id")?;
            observe_in_transaction(
                &mut tx,
                &owner,
                build,
                attempt,
                &if row.try_get::<bool, _>("execution_authorized")? {
                    BuildUsageMeasurement::Unknown {
                        reason: "build deadline passed without authoritative terminal history"
                            .into(),
                    }
                } else {
                    BuildUsageMeasurement::NotStarted {
                        reason: "build deadline passed without BuildKit execution authorization"
                            .into(),
                    }
                },
            )
            .await?;
            tx.commit().await?;
        }
        Ok(changed)
    }
}

impl PgResourceStore {
    pub async fn claim_build_cost_reconciliations(
        &self,
        owner: &str,
    ) -> Result<Vec<contracts::resource::build_costing::BuildCostReconciliation>, ResourceStoreError>
    {
        let mut tx = self.pool().begin().await?;
        let rows = sqlx::query("UPDATE resource.build_metering_attempts AS b SET query_after=clock_timestamp()+interval '1 minute' WHERE (build_request_id,attempt) IN (SELECT build_request_id,attempt FROM resource.build_metering_attempts WHERE owner_client_id=$1 AND charged_total IS NULL AND execution_deadline IS NOT NULL AND (measurement IS NULL OR measurement->>'state'<>'not_started') AND (first_unknown_observation IS NOT NULL OR execution_deadline<=clock_timestamp()) AND query_after<=clock_timestamp() ORDER BY query_after,build_request_id,attempt LIMIT 25 FOR UPDATE SKIP LOCKED) RETURNING b.*").bind(owner).fetch_all(&mut *tx).await?;
        let mut result = Vec::new();
        for row in rows {
            let command: serde_json::Value = sqlx::query_scalar(
                "SELECT command FROM agent.build_commands WHERE build_request_id=$1",
            )
            .bind(row.try_get::<Uuid, _>("build_request_id")?)
            .fetch_one(&mut *tx)
            .await?;
            result.push(
                contracts::resource::build_costing::BuildCostReconciliation {
                    admission: contracts::resource::build_costing::BuildCostAdmissionRequest {
                        build_request_id: row
                            .try_get::<Uuid, _>("build_request_id")?
                            .to_string()
                            .parse()
                            .map_err(|_| ResourceStoreError::Wire)?,
                        attempt: u32::try_from(row.try_get::<i32, _>("attempt")?)
                            .map_err(|_| ResourceStoreError::Wire)?,
                        lease_token: row.try_get("lease_token")?,
                        execution_deadline: UtcTimestamp::from_utc(
                            row.try_get("execution_deadline")?,
                        )?,
                        stage_request_id: row.try_get("build_stage_request_id")?,
                        authorize_execution: row.try_get("execution_authorized")?,
                    },
                    command: serde_json::from_value(command)?,
                },
            );
        }
        tx.commit().await?;
        Ok(result)
    }
}
