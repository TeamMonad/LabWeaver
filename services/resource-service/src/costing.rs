//! Resource-owned pricing and atomically funded consent snapshots.
use crate::store::{PgResourceStore, ResourceStoreError};
use contracts::ProjectId;
use contracts::resource::costing::{
    EstimatedCreditHold, EstimatedCreditHoldRequest, ResourceCostEstimate,
    ResourceCostEstimateRequest,
};
use contracts::resource::credits::CreditHoldRequest;
use contracts::resource::{
    FixedDecimal, Money, ResourceApproval, ResourceBillingUnit, ResourceRequest, ResourceTarget,
    ResourceUsageKind,
};
use sqlx::{Postgres, Row, Transaction};

impl PgResourceStore {
    /// Preview only: this operation never reserves money or capacity.
    pub async fn estimate_cost(
        &self,
        project: ProjectId,
        input: &ResourceCostEstimateRequest,
    ) -> Result<ResourceCostEstimate, ResourceStoreError> {
        let mut tx = self.pool().begin().await?;
        rate_lock(&mut tx).await?;
        let result = estimate_in_transaction(&mut tx, project, input, None).await?;
        tx.commit().await?;
        Ok(result)
    }

    /// Scope, rate consent, funds and immutable snapshot commit in one transaction.
    #[allow(
        clippy::too_many_lines,
        reason = "consent, scope, funds and immutable snapshot stay in one auditable transaction"
    )]
    pub async fn hold_estimated_cost(
        &self,
        key: &str,
        owner: &str,
        environment_owner: bool,
        input: &EstimatedCreditHoldRequest,
    ) -> Result<EstimatedCreditHold, ResourceStoreError> {
        let mut tx = self.pool().begin().await?;
        if let Some(replay) =
            crate::credits::reserve(&mut tx, "estimated_credit_hold", key, &(owner, input)).await?
        {
            return Ok(serde_json::from_value(replay)?);
        }
        rate_lock(&mut tx).await?;
        let row = sqlx::query(
            "SELECT contract FROM resource.resource_requests WHERE request_id=$1 FOR UPDATE",
        )
        .bind(input.request_id.as_uuid())
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ResourceStoreError::NotFound)?;
        let request: ResourceRequest = serde_json::from_value(row.try_get("contract")?)?;
        request.validate()?;
        if matches!(request.target, ResourceTarget::Environment { .. }) != environment_owner {
            return Err(crate::credits::CreditError::Denied.into());
        }
        let approval = sqlx::query("SELECT contract FROM resource.resource_approvals WHERE request_id=$1 ORDER BY created_at DESC, approval_id DESC LIMIT 1")
            .bind(request.id.as_uuid()).fetch_optional(&mut *tx).await?;
        let (resources, duration) = if let Some(approval) = approval {
            let approval: ResourceApproval = serde_json::from_value(approval.try_get("contract")?)?;
            approval.validate()?;
            (
                approval.approved_resources,
                approval.approved_duration_seconds,
            )
        } else {
            (
                request.requested_resources,
                request.requested_duration_seconds,
            )
        };
        let consent = &input.expected_estimate.input;
        let duration = i128::from(duration)
            .checked_mul(1000)
            .ok_or(ResourceStoreError::NumericOverflow)?;
        if resources != consent.resources
            || (consent.compute_until.get() - consent.compute_from.get()).whole_milliseconds()
                != duration
            || (consent.storage_until.get() - consent.storage_from.get()).whole_milliseconds()
                != duration
        {
            return Err(ResourceStoreError::ScopeConflict);
        }
        // Another retry key must retrieve the original consent, even after a rate publication.
        if let Some(row) = sqlx::query(
            "SELECT snapshot FROM resource.credit_cost_snapshots WHERE request_id=$1 FOR UPDATE",
        )
        .bind(request.id.as_uuid())
        .fetch_optional(&mut *tx)
        .await?
        {
            let existing: EstimatedCreditHold = serde_json::from_value(row.try_get("snapshot")?)?;
            if existing.estimate != input.expected_estimate
                || existing.hold.owner_client_id != owner
            {
                return Err(ResourceStoreError::EstimateChanged);
            }
            crate::credits::complete(&mut tx, "estimated_credit_hold", key, &existing).await?;
            tx.commit().await?;
            return Ok(existing);
        }
        let now: time::OffsetDateTime = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut *tx)
            .await?;
        if consent.compute_from.get() < now || consent.storage_from.get() < now {
            return Err(ResourceStoreError::EstimateChanged);
        }
        let claim = sqlx::query("SELECT contract FROM resource.capacity_claims WHERE request_id=$1 ORDER BY created_at DESC,claim_id DESC LIMIT 1").bind(request.id.as_uuid()).fetch_optional(&mut *tx).await?;
        let claim = claim
            .map(
                |row| -> Result<contracts::resource::CapacityClaim, ResourceStoreError> {
                    let claim: contracts::resource::CapacityClaim =
                        serde_json::from_value(row.try_get("contract")?)?;
                    claim.validate()?;
                    Ok(claim)
                },
            )
            .transpose()?;
        let estimate = estimate_in_transaction(
            &mut tx,
            request.project_id,
            consent,
            claim
                .as_ref()
                .and_then(|claim| claim.gpu_allocation.as_ref()),
        )
        .await?;
        if estimate != input.expected_estimate {
            return Err(ResourceStoreError::EstimateChanged);
        }
        let hold = crate::credits::hold_in_transaction(
            &mut tx,
            key,
            owner,
            &CreditHoldRequest {
                project_id: request.project_id,
                request_id: request.id,
                business_key: "metered-usage".into(),
                amount: estimate.total.amount.clone(),
            },
        )
        .await?;
        crate::credits::require_funded_in_transaction(
            &mut tx,
            owner,
            hold.id,
            &estimate.total.amount,
        )
        .await?;
        let result = EstimatedCreditHold { hold, estimate };
        sqlx::query("INSERT INTO resource.credit_cost_snapshots(hold_id,request_id,snapshot) VALUES($1,$2,$3)")
            .bind(result.hold.id).bind(request.id.as_uuid()).bind(serde_json::to_value(&result)?).execute(&mut *tx).await?;
        crate::credits::complete(&mut tx, "estimated_credit_hold", key, &result).await?;
        tx.commit().await?;
        Ok(result)
    }
}

pub(crate) async fn rate_lock(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<(), ResourceStoreError> {
    sqlx::query("SELECT pg_advisory_xact_lock_shared(hashtextextended('resource_rates', 0))")
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn estimate_in_transaction(
    tx: &mut Transaction<'_, Postgres>,
    project: ProjectId,
    input: &ResourceCostEstimateRequest,
    allocation: Option<&contracts::resource::GpuAllocation>,
) -> Result<ResourceCostEstimate, ResourceStoreError> {
    input.validate()?;
    let resources = &input.resources;
    let gpu_mode = resolve_gpu_mode(tx, &input.resources, allocation).await?;
    let mut lines = Vec::new();
    let mut currency = None;
    for (kind, from, until) in [
        (
            ResourceUsageKind::Compute,
            input.compute_from,
            input.compute_until,
        ),
        (
            ResourceUsageKind::Storage,
            input.storage_from,
            input.storage_until,
        ),
    ] {
        let millis = u64::try_from((until.get() - from.get()).whole_milliseconds())
            .map_err(|_| ResourceStoreError::NumericOverflow)?;
        if millis == 0 {
            return Err(ResourceStoreError::NumericOverflow);
        }
        let dimensions = if kind == ResourceUsageKind::Compute {
            vec![
                (
                    ResourceBillingUnit::CpuMillicoreSecond,
                    u64::from(resources.cpu_millicores),
                ),
                (
                    ResourceBillingUnit::MemoryByteSecond,
                    resources.memory_bytes,
                ),
                (
                    ResourceBillingUnit::GpuUnitSecond,
                    resources.gpu.as_ref().map_or(0, |gpu| u64::from(gpu.count)),
                ),
            ]
        } else {
            vec![(
                ResourceBillingUnit::StorageByteSecond,
                resources.storage_bytes,
            )]
        };
        for (unit, quantity) in dimensions {
            if quantity == 0 {
                continue;
            }
            let quantity = quantity
                .checked_mul(millis)
                .ok_or(ResourceStoreError::NumericOverflow)?;
            let gpu = unit == ResourceBillingUnit::GpuUnitSecond;
            crate::store::append_rate_segments(
                tx,
                from,
                until,
                1000,
                unit,
                quantity,
                if gpu {
                    resources.gpu.as_ref().map(|gpu| gpu.class.as_str())
                } else {
                    None
                },
                if gpu { gpu_mode } else { None },
                &mut lines,
                &mut currency,
            )
            .await?;
        }
    }
    if currency.as_deref() != Some("CREDITS") {
        return Err(ResourceStoreError::RateCurrencyConflict);
    }
    let amount = lines.iter().try_fold(0_i128, |sum, line| {
        sum.checked_add(line.amount.amount.scaled())
            .ok_or(ResourceStoreError::NumericOverflow)
    })?;
    Ok(ResourceCostEstimate {
        project_id: project,
        input: input.clone(),
        lines,
        total: Money {
            currency: "CREDITS".into(),
            amount: FixedDecimal::from_scaled(amount)?,
        },
    })
}

/// Called only after Task attempt, revisions, namespace and approval are validated and locked.
pub(crate) async fn fund_task_in_transaction(
    tx: &mut Transaction<'_, Postgres>,
    request: &ResourceRequest,
    claim: &contracts::resource::CapacityClaim,
    owner: &str,
    from: contracts::UtcTimestamp,
    until: contracts::UtcTimestamp,
) -> Result<(), ResourceStoreError> {
    let ResourceTarget::Task { task_run_id } = request.target else {
        return Err(ResourceStoreError::ScopeConflict);
    };
    let input = ResourceCostEstimateRequest {
        resources: claim.workload_resources.clone(),
        compute_from: from,
        compute_until: until,
        storage_from: from,
        storage_until: until,
    };
    let estimate = estimate_in_transaction(
        tx,
        request.project_id,
        &input,
        claim.gpu_allocation.as_ref(),
    )
    .await?;
    if let Some(row) =
        sqlx::query("SELECT snapshot FROM resource.credit_cost_snapshots WHERE request_id=$1")
            .bind(request.id.as_uuid())
            .fetch_optional(&mut **tx)
            .await?
    {
        let existing: EstimatedCreditHold = serde_json::from_value(row.try_get("snapshot")?)?;
        if existing.hold.owner_client_id != owner {
            return Err(crate::credits::CreditError::Denied.into());
        }
        if existing.estimate.input.resources != input.resources
            || existing.hold.amount.scaled() < estimate.total.amount.scaled()
        {
            return Err(ResourceStoreError::EstimateChanged);
        }
        crate::credits::require_funded_in_transaction(
            tx,
            owner,
            existing.hold.id,
            &estimate.total.amount,
        )
        .await?;
        return Ok(());
    }
    let hold = crate::credits::hold_in_transaction(
        tx,
        &format!("task-cost-{task_run_id}"),
        owner,
        &CreditHoldRequest {
            project_id: request.project_id,
            request_id: request.id,
            business_key: "metered-usage".into(),
            amount: estimate.total.amount.clone(),
        },
    )
    .await?;
    crate::credits::require_funded_in_transaction(tx, owner, hold.id, &estimate.total.amount)
        .await?;
    let result = EstimatedCreditHold { hold, estimate };
    sqlx::query(
        "INSERT INTO resource.credit_cost_snapshots(hold_id,request_id,snapshot) VALUES($1,$2,$3)",
    )
    .bind(result.hold.id)
    .bind(request.id.as_uuid())
    .bind(serde_json::to_value(result)?)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn resolve_gpu_mode(
    tx: &mut Transaction<'_, Postgres>,
    resources: &contracts::resource::WorkloadResources,
    allocation: Option<&contracts::resource::GpuAllocation>,
) -> Result<Option<contracts::resource::GpuAllocationMode>, ResourceStoreError> {
    if let Some(allocation) = allocation {
        allocation.validate()?;
        if !resources
            .gpu
            .as_ref()
            .is_some_and(|gpu| gpu.class == allocation.class && gpu.count == allocation.count)
        {
            return Err(ResourceStoreError::GpuAllocationMissing);
        }
        return Ok(Some(allocation.mode));
    }
    let mode = if let Some(gpu) = &resources.gpu {
        let row = sqlx::query(
            "SELECT contract FROM resource.gpu_catalog_entries WHERE active AND class=$1",
        )
        .bind(&gpu.class)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(ResourceStoreError::GpuCatalogMissing)?;
        let entry: contracts::resource::GpuCatalogEntry =
            serde_json::from_value(row.try_get("contract")?)?;
        entry.validate()?;
        if entry.mode == contracts::resource::GpuAllocationMode::ContainerTimeSlice
            && gpu.count != 1
        {
            return Err(ResourceStoreError::GpuCatalogInvalid);
        }
        Some(entry.mode)
    } else {
        None
    };
    Ok(mode)
}
