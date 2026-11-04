//! Funding stays on the original Resource request, claim, approval, lease and ledger.
use crate::store::{PgResourceStore, ResourceStoreError};
use contracts::resource::{
    CapacityClaim, CapacityClaimState, FixedDecimal, ResourceApproval, ResourceLease,
    ResourceLeaseState, ResourceRequest, ResourceTarget,
};
use contracts::resource::{
    costing::{EstimatedCreditHold, ResourceCostEstimateRequest},
    credits::{CreditHold, CreditHoldRequest, CreditHoldState},
    environment_funding::EnvironmentCreditGrant,
};
use contracts::{LeaseId, Revision, UtcTimestamp};
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

async fn original_approval(
    tx: &mut Transaction<'_, Postgres>,
    request: &ResourceRequest,
    lease: &ResourceLease,
) -> Result<(CapacityClaim, ResourceApproval), ResourceStoreError> {
    let row=sqlx::query("SELECT c.contract AS claim,a.contract AS approval FROM resource.capacity_claims c JOIN resource.resource_approvals a ON a.approval_id=c.approval_id WHERE c.claim_id=$1 AND c.request_id=$2 AND a.request_id=$2").bind(lease.claim_id.as_uuid()).bind(request.id.as_uuid()).fetch_optional(&mut **tx).await?.ok_or(ResourceStoreError::NotFound)?;
    let claim: CapacityClaim = serde_json::from_value(row.try_get("claim")?)?;
    let approval: ResourceApproval = serde_json::from_value(row.try_get("approval")?)?;
    claim.validate()?;
    approval.validate()?;
    if !matches!(request.target, ResourceTarget::Environment { .. })
        || claim.id != lease.claim_id
        || claim.request_id != request.id
        || approval.request_id != request.id
        || claim.approval_id != approval.id
        || claim.workload_resources != approval.approved_resources
        || claim.provider_binding != approval.provider_binding
        || approval.request_revision > request.revision
    {
        return Err(ResourceStoreError::AllocationMismatch);
    }
    Ok((claim, approval))
}
async fn actual_hold(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<CreditHold, ResourceStoreError> {
    let row=sqlx::query("SELECT *,amount::text AS amount_scaled,remaining::text AS remaining_scaled FROM resource.credit_holds WHERE hold_id=$1").bind(id).fetch_one(&mut **tx).await?;
    Ok(crate::credits::decode_hold(&row)?)
}

/// Quotes and reserves the initial interval or only the additional renewal interval.
/// The caller holds lease before request and updates authorization in this same transaction.
#[allow(
    clippy::too_many_lines,
    clippy::too_many_arguments,
    reason = "original approval, interval funding and immutable snapshot are one transaction"
)]
pub(crate) async fn fund_window(
    tx: &mut Transaction<'_, Postgres>,
    request: &ResourceRequest,
    lease: &ResourceLease,
    next_revision: Revision,
    owner: &str,
    from: UtcTimestamp,
    until: UtcTimestamp,
    initial: bool,
) -> Result<(), ResourceStoreError> {
    let (claim, approval) = original_approval(tx, request, lease).await?;
    let compute_released: bool = sqlx::query_scalar(
        "SELECT compute_released FROM resource.capacity_claims WHERE claim_id=$1",
    )
    .bind(claim.id.as_uuid())
    .fetch_one(&mut **tx)
    .await?;
    if compute_released {
        return Err(ResourceStoreError::UsageConflict);
    }
    if !matches!(
        claim.state,
        CapacityClaimState::Provisioning
            | CapacityClaimState::Ready
            | CapacityClaimState::HandedOff
    ) {
        return Err(ResourceStoreError::CapacityClaimStateConflict);
    }
    let now: time::OffsetDateTime =
        sqlx::query_scalar("SELECT date_trunc('milliseconds',clock_timestamp())")
            .fetch_one(&mut **tx)
            .await?;
    if until <= from
        || until.get() <= now
        || approval.valid_until.get() <= now
        || until > approval.valid_until
        || (until.get() - from.get())
            > time::Duration::seconds(
                i64::try_from(approval.approved_duration_seconds)
                    .map_err(|_| ResourceStoreError::NumericOverflow)?,
            )
    {
        return Err(ResourceStoreError::ApprovalInvalid);
    }
    // Only the lease coordinator calls this under its native revision fence.
    if !initial && from.get() <= now {
        return Err(ResourceStoreError::ApprovalInvalid);
    }
    if initial && lease.state != ResourceLeaseState::Allocating
        || !initial && lease.state != ResourceLeaseState::Active
    {
        return Err(ResourceStoreError::CapacityClaimStateConflict);
    }
    crate::costing::rate_lock(tx).await?;
    let input = ResourceCostEstimateRequest {
        resources: claim.workload_resources.clone(),
        compute_from: from,
        compute_until: until,
        storage_from: from,
        storage_until: until,
    };
    let estimate = crate::costing::estimate_in_transaction(
        tx,
        request.project_id,
        &input,
        claim.gpu_allocation.as_ref(),
    )
    .await?;
    if estimate.total.currency != "CREDITS" {
        return Err(ResourceStoreError::RateCurrencyConflict);
    }
    let previous=sqlx::query("SELECT hold_id,covered_until FROM resource.environment_credit_windows WHERE lease_id=$1 ORDER BY lease_revision DESC LIMIT 1").bind(lease.id.as_uuid()).fetch_optional(&mut **tx).await?;
    let hold = if initial {
        if previous.is_some() {
            return Err(ResourceStoreError::UsageConflict);
        }
        if let Some(row) =
            sqlx::query("SELECT snapshot FROM resource.credit_cost_snapshots WHERE request_id=$1")
                .bind(request.id.as_uuid())
                .fetch_optional(&mut **tx)
                .await?
        {
            let existing: EstimatedCreditHold = serde_json::from_value(row.try_get("snapshot")?)?;
            if existing.hold.owner_client_id != owner
                || existing.estimate.project_id != request.project_id
                || existing.estimate.total.currency != "CREDITS"
                || existing.hold.project_id != request.project_id
                || existing.estimate.input.resources != input.resources
                || existing.hold.amount.scaled() < estimate.total.amount.scaled()
                || estimate.lines.iter().any(|line| {
                    !existing.estimate.lines.iter().any(|consent| {
                        consent.rate_id == line.rate_id
                            && consent.rate_revision == line.rate_revision
                            && consent.unit == line.unit
                            && consent.unit_quantity == line.unit_quantity
                            && consent.unit_price == line.unit_price
                    })
                })
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
            actual_hold(tx, existing.hold.id).await?
        } else {
            let existing:Option<Uuid>=sqlx::query_scalar("SELECT hold_id FROM resource.credit_holds WHERE request_id=$1 AND business_key='metered-usage'").bind(request.id.as_uuid()).fetch_optional(&mut **tx).await?;
            let hold = if let Some(id) = existing {
                crate::credits::require_funded_in_transaction(
                    tx,
                    owner,
                    id,
                    &estimate.total.amount,
                )
                .await?;
                actual_hold(tx, id).await?
            } else {
                crate::credits::hold_in_transaction(
                    tx,
                    &format!("environment-cost-{}", lease.id),
                    owner,
                    &CreditHoldRequest {
                        project_id: request.project_id,
                        request_id: request.id,
                        business_key: "metered-usage".into(),
                        amount: estimate.total.amount.clone(),
                    },
                )
                .await?
            };
            if hold.project_id != request.project_id || hold.request_id != Some(request.id) {
                return Err(ResourceStoreError::ScopeConflict);
            }
            crate::credits::require_funded_in_transaction(
                tx,
                owner,
                hold.id,
                &estimate.total.amount,
            )
            .await?;
            let snapshot = EstimatedCreditHold {
                hold: hold.clone(),
                estimate: estimate.clone(),
            };
            sqlx::query("INSERT INTO resource.credit_cost_snapshots(hold_id,request_id,snapshot) VALUES($1,$2,$3)").bind(hold.id).bind(request.id.as_uuid()).bind(serde_json::to_value(snapshot)?).execute(&mut **tx).await?;
            hold
        }
    } else {
        let previous = previous.ok_or(ResourceStoreError::NotFound)?;
        let covered: time::OffsetDateTime = previous.try_get("covered_until")?;
        if covered != from.get() || lease.expires_at != Some(from) {
            return Err(ResourceStoreError::ScopeConflict);
        }
        let required = cumulative_funding_amount(tx, lease.id, &estimate).await?;
        crate::credits::extend_environment_hold(tx, owner, previous.try_get("hold_id")?, &required)
            .await?
    };
    if hold.project_id != request.project_id || hold.request_id != Some(request.id) {
        return Err(ResourceStoreError::ScopeConflict);
    }
    sqlx::query("INSERT INTO resource.environment_credit_windows(lease_id,lease_revision,request_id,claim_id,approval_id,hold_id,covered_from,covered_until,estimate) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)").bind(lease.id.as_uuid()).bind(i64::try_from(next_revision.get()).map_err(|_|ResourceStoreError::NumericOverflow)?).bind(request.id.as_uuid()).bind(claim.id.as_uuid()).bind(approval.id.as_uuid()).bind(hold.id).bind(from.get()).bind(until.get()).bind(serde_json::to_value(estimate)?).execute(&mut **tx).await?;
    Ok(())
}

/// Aggregate exact quantities from immutable rate snapshots before the shared rounding step.
/// Summing rounded delta amounts could reserve zero for two halves of one charge unit.
async fn cumulative_funding_amount(
    tx: &mut Transaction<'_, Postgres>,
    lease: LeaseId,
    latest: &contracts::resource::costing::ResourceCostEstimate,
) -> Result<FixedDecimal, ResourceStoreError> {
    use contracts::resource::{Money, ResourceChargeLine};
    use std::collections::{BTreeMap, btree_map::Entry};
    let rows=sqlx::query("SELECT line-'quantity'-'amount' AS dimensions,sum((line->>'quantity')::numeric)::text AS quantity FROM resource.environment_credit_windows w CROSS JOIN LATERAL jsonb_array_elements(w.estimate->'lines') line WHERE w.lease_id=$1 GROUP BY line-'quantity'-'amount'")
        .bind(lease.as_uuid()).fetch_all(&mut **tx).await?;
    let mut snapshots = Vec::with_capacity(rows.len() + latest.lines.len());
    for row in rows {
        let mut dimensions: serde_json::Value = row.try_get("dimensions")?;
        let quantity = row
            .try_get::<String, _>("quantity")?
            .parse::<u64>()
            .map_err(|_| ResourceStoreError::NumericOverflow)?;
        let fields = dimensions.as_object_mut().ok_or(ResourceStoreError::Wire)?;
        fields.insert("quantity".into(), serde_json::json!(quantity));
        fields.insert(
            "amount".into(),
            serde_json::to_value(Money {
                currency: "CREDITS".into(),
                amount: FixedDecimal::zero(),
            })?,
        );
        snapshots.push(serde_json::from_value::<ResourceChargeLine>(dimensions)?);
    }
    snapshots.extend(latest.lines.iter().cloned());
    let mut lines = BTreeMap::<String, ResourceChargeLine>::new();
    for line in snapshots {
        if line.unit_price.currency != "CREDITS" {
            return Err(ResourceStoreError::RateCurrencyConflict);
        }
        match lines.entry(line.rate_id.to_string()) {
            Entry::Vacant(slot) => {
                slot.insert(line);
            }
            Entry::Occupied(mut slot) => {
                let original = slot.get_mut();
                if original.rate_revision != line.rate_revision
                    || original.unit != line.unit
                    || original.unit_quantity != line.unit_quantity
                    || original.quantity_divisor != line.quantity_divisor
                    || original.unit_price != line.unit_price
                {
                    return Err(ResourceStoreError::UsageConflict);
                }
                original.quantity = original
                    .quantity
                    .checked_add(line.quantity)
                    .ok_or(ResourceStoreError::NumericOverflow)?;
            }
        }
    }
    let total = lines.values().try_fold(0_i128, |total, line| {
        let amount = contracts::resource::rounded_scaled_amount(
            line.unit_price.amount.scaled(),
            line.quantity,
            line.unit_quantity,
            line.quantity_divisor,
        )?;
        total
            .checked_add(amount)
            .ok_or(ResourceStoreError::NumericOverflow)
    })?;
    Ok(FixedDecimal::from_scaled(total)?)
}

impl PgResourceStore {
    /// The worker derives its initial deadline from the exact original approval, not the requested draft.
    pub async fn environment_approval_deadline(
        &self,
        lease_id: LeaseId,
        from: UtcTimestamp,
    ) -> Result<UtcTimestamp, ResourceStoreError> {
        let row=sqlx::query("SELECT a.approved_duration_seconds,a.valid_until FROM resource.resource_leases l JOIN resource.capacity_claims c ON c.claim_id=l.claim_id JOIN resource.resource_approvals a ON a.approval_id=c.approval_id AND a.request_id=l.request_id WHERE l.lease_id=$1").bind(lease_id.as_uuid()).fetch_one(&self.pool()).await?;
        let duration: i64 = row.try_get("approved_duration_seconds")?;
        let limit: time::OffsetDateTime = row.try_get("valid_until")?;
        if duration <= 0 {
            return Err(ResourceStoreError::ApprovalInvalid);
        }
        Ok(UtcTimestamp::from_utc(
            from.get()
                .checked_add(time::Duration::seconds(duration))
                .ok_or(ResourceStoreError::NumericOverflow)?
                .min(limit),
        )?)
    }
    /// Reads current financial coverage without creating a hold or another runtime authority.
    pub async fn environment_credit_grant(
        &self,
        owner: &str,
        lease_id: LeaseId,
    ) -> Result<EnvironmentCreditGrant, ResourceStoreError> {
        let mut tx = self.pool().begin().await?;
        let row=sqlx::query("SELECT w.*,l.contract AS lease_contract,(SELECT min(covered_from) FROM resource.environment_credit_windows WHERE lease_id=w.lease_id) AS initial_from,NOT EXISTS(SELECT 1 FROM resource.environment_cleanup_fences f WHERE f.lease_id=w.lease_id AND f.compute_released) AS compute_not_released FROM resource.environment_credit_windows w JOIN resource.resource_leases l USING(lease_id) WHERE w.lease_id=$1 ORDER BY w.lease_revision DESC LIMIT 1").bind(lease_id.as_uuid()).fetch_optional(&mut *tx).await?.ok_or(ResourceStoreError::NotFound)?;
        let lease: ResourceLease = serde_json::from_value(row.try_get("lease_contract")?)?;
        lease.validate()?;
        let hold = actual_hold(&mut tx, row.try_get("hold_id")?).await?;
        if hold.owner_client_id != owner {
            return Err(crate::credits::CreditError::Denied.into());
        }
        let request: ResourceRequest = serde_json::from_value(
            sqlx::query_scalar::<_, serde_json::Value>(
                "SELECT contract FROM resource.resource_requests WHERE request_id=$1",
            )
            .bind(lease.request_id.as_uuid())
            .fetch_one(&mut *tx)
            .await?,
        )?;
        request.validate()?;
        let (claim, approval) = original_approval(&mut tx, &request, &lease).await?;
        if row.try_get::<Uuid, _>("request_id")? != request.id.as_uuid()
            || row.try_get::<Uuid, _>("claim_id")? != claim.id.as_uuid()
            || row.try_get::<Uuid, _>("approval_id")? != approval.id.as_uuid()
            || hold.project_id != request.project_id
            || hold.request_id != Some(request.id)
        {
            return Err(ResourceStoreError::ScopeConflict);
        }
        let from = UtcTimestamp::from_utc(row.try_get("initial_from")?)?;
        let until = UtcTimestamp::from_utc(row.try_get("covered_until")?)?;
        let now: time::OffsetDateTime = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut *tx)
            .await?;
        let revision = Revision::new(
            u64::try_from(row.try_get::<i64, _>("lease_revision")?)
                .map_err(|_| ResourceStoreError::Wire)?,
        )?;
        let grant = EnvironmentCreditGrant {
            request_id: lease.request_id,
            lease_id,
            lease_revision: revision,
            claim_id: lease.claim_id,
            approval_id: row
                .try_get::<Uuid, _>("approval_id")?
                .to_string()
                .parse()
                .map_err(|_| ResourceStoreError::Wire)?,
            covered_from: from,
            covered_until: until,
            can_authorize_execution: row.try_get::<bool, _>("compute_not_released")?
                && lease.state == ResourceLeaseState::Active
                && request.state == contracts::resource::ResourceRequestState::Active
                && matches!(
                    claim.state,
                    CapacityClaimState::Provisioning
                        | CapacityClaimState::Ready
                        | CapacityClaimState::HandedOff
                )
                && lease.revision == revision
                && lease.expires_at == Some(until)
                && lease.active_from == Some(from)
                && now >= from.get()
                && now < until.get()
                && hold.state == CreditHoldState::Pending
                && !hold.measurement_unknown
                && hold.expired_unknown_at.is_none()
                && (hold.amount == FixedDecimal::zero() || hold.remaining.scaled() > 0),
            hold,
            estimate: serde_json::from_value(row.try_get("estimate")?)?,
        };
        tx.commit().await?;
        Ok(grant)
    }
}

impl PgResourceStore {
    /// A missing final usage/release after paid coverage cannot freeze pending funds indefinitely.
    pub async fn reconcile_environment_funding_deadlines_once(
        &self,
    ) -> Result<bool, ResourceStoreError> {
        let rows=sqlx::query("SELECT h.hold_id,h.owner_client_id FROM resource.credit_holds h WHERE h.state='pending' AND NOT h.measurement_unknown AND (EXISTS(SELECT 1 FROM resource.environment_credit_windows w WHERE w.hold_id=h.hold_id AND w.covered_until<=clock_timestamp() AND NOT EXISTS(SELECT 1 FROM resource.environment_credit_windows later WHERE later.lease_id=w.lease_id AND later.lease_revision>w.lease_revision)) OR (h.business_key='metered-usage' AND NOT EXISTS(SELECT 1 FROM resource.environment_credit_windows w WHERE w.hold_id=h.hold_id) AND EXISTS(SELECT 1 FROM resource.resource_leases l JOIN resource.capacity_claims c ON c.claim_id=l.claim_id JOIN resource.resource_approvals a ON a.approval_id=c.approval_id JOIN resource.resource_requests r ON r.request_id=l.request_id WHERE l.request_id=h.request_id AND r.environment_id IS NOT NULL AND l.state='allocating' AND c.state IN ('reserved','provisioning','ready') AND a.valid_until<=clock_timestamp()))) ORDER BY h.hold_id LIMIT 100").fetch_all(&self.pool()).await?;
        let changed = !rows.is_empty();
        for row in rows {
            let mut tx = self.pool().begin().await?;
            let owner: String = row.try_get("owner_client_id")?;
            let id: Uuid = row.try_get("hold_id")?;
            // Recheck under the same account/hold lock as a concurrent renewal or settlement.
            let project: Uuid =
                sqlx::query_scalar("SELECT project_id FROM resource.credit_holds WHERE hold_id=$1")
                    .bind(id)
                    .fetch_one(&mut *tx)
                    .await?;
            sqlx::query(
                "SELECT project_id FROM resource.credit_accounts WHERE project_id=$1 FOR UPDATE",
            )
            .bind(project)
            .fetch_one(&mut *tx)
            .await?;
            let due:bool=sqlx::query_scalar("SELECT h.state='pending' AND NOT h.measurement_unknown AND (EXISTS(SELECT 1 FROM resource.environment_credit_windows w WHERE w.hold_id=h.hold_id AND w.covered_until<=clock_timestamp() AND NOT EXISTS(SELECT 1 FROM resource.environment_credit_windows later WHERE later.lease_id=w.lease_id AND later.lease_revision>w.lease_revision)) OR (h.business_key='metered-usage' AND NOT EXISTS(SELECT 1 FROM resource.environment_credit_windows w WHERE w.hold_id=h.hold_id) AND EXISTS(SELECT 1 FROM resource.resource_leases l JOIN resource.capacity_claims c ON c.claim_id=l.claim_id JOIN resource.resource_approvals a ON a.approval_id=c.approval_id JOIN resource.resource_requests r ON r.request_id=l.request_id WHERE l.request_id=h.request_id AND r.environment_id IS NOT NULL AND l.state='allocating' AND c.state IN ('reserved','provisioning','ready') AND a.valid_until<=clock_timestamp()))) FROM resource.credit_holds h WHERE h.hold_id=$1 FOR UPDATE").bind(id).fetch_one(&mut *tx).await?;
            if due {
                let authorized:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM resource.environment_credit_windows WHERE hold_id=$1)").bind(id).fetch_one(&mut *tx).await?;
                let resolution = if authorized {
                    contracts::resource::credits::CreditHoldResolutionRequest::Unknown {
                        event_id: None,
                        reason: "Environment financial coverage ended without confirmed final usage/release".into(),
                    }
                } else {
                    contracts::resource::credits::CreditHoldResolutionRequest::Release {
                        reason: "original approval expired before funded Environment activation"
                            .into(),
                    }
                };
                crate::credits::resolve_in_transaction(
                    &mut tx,
                    &owner,
                    id,
                    &resolution,
                    None,
                    None,
                )
                .await?;
            }
            tx.commit().await?;
        }
        Ok(changed)
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "receipt fence, independent release and retained watermark commit together"
)]
pub(crate) async fn consume_cleanup_status(
    pool: &sqlx::PgPool,
    expected: &crate::store::ProvisioningCapacityClaim,
    status: &contracts::environment::ResourceWorkCleanupStatus,
) -> Result<(), ResourceStoreError> {
    use contracts::resource::ResourceTarget;
    status
        .validate()
        .map_err(|_| ResourceStoreError::CapacityReadbackInvalid)?;
    let ResourceTarget::Environment { environment_id, .. } = expected.request.target else {
        return Err(ResourceStoreError::CapacityReadbackInvalid);
    };
    if status.environment_id != environment_id
        || status.project_id != expected.request.project_id
        || status.course_id != expected.request.course_id
        || status.owner_actor_id != expected.request.requester_id
        || status.lease_id != expected.lease.id
        || status.capacity_binding != expected.claim.id.to_string()
        || Some(status.lease_revision) != expected.lease_synced_revision
    {
        return Err(ResourceStoreError::CapacityReadbackInvalid);
    }
    if let Some(usage) = &status.retained_storage_usage {
        let disk = status
            .disk_retention
            .as_ref()
            .ok_or(ResourceStoreError::CapacityReadbackInvalid)?;
        let close_at = disk.purged_at.or(disk.reactivated_at);
        if usage.started_at != disk.started_at
            || disk.compute_retired_at.is_none()
            || disk.retired_generation > status.generation
            || usage.closed != close_at.is_some()
            || close_at.is_some_and(|closed| usage.measured_until != closed)
        {
            return Err(ResourceStoreError::CapacityReadbackInvalid);
        }
    }
    let mut normalized = status.clone();
    normalized.retained_storage_usage = None;
    let receipt = serde_json::to_value(normalized)?;
    let mut tx = pool.begin().await?;
    let result = async {
        let row = sqlx::query("SELECT l.request_id,l.claim_id,l.state AS lease_state,r.project_id,r.course_id,r.requester_id,r.state AS request_state,c.state AS claim_state,c.lease_synced_revision FROM resource.resource_leases l JOIN resource.resource_requests r USING(request_id) JOIN resource.capacity_claims c ON c.claim_id=l.claim_id WHERE l.lease_id=$1 FOR UPDATE OF l,r,c")
            .bind(expected.lease.id.as_uuid()).fetch_optional(&mut *tx).await?.ok_or(ResourceStoreError::NotFound)?;
        if row.try_get::<Uuid, _>("request_id")? != expected.request.id.as_uuid()
            || row.try_get::<Uuid, _>("claim_id")? != expected.claim.id.as_uuid()
            || row.try_get::<Uuid, _>("project_id")? != expected.request.project_id.as_uuid()
            || row.try_get::<Option<Uuid>, _>("course_id")? != expected.request.course_id.map(contracts::CourseId::as_uuid)
            || row.try_get::<Uuid, _>("requester_id")? != expected.request.requester_id.as_uuid()
            || !matches!(row.try_get::<String, _>("request_state")?.as_str(), "active" | "expiring")
            || !matches!(row.try_get::<String, _>("claim_state")?.as_str(), "handed_off" | "releasing")
            || !matches!(row.try_get::<String, _>("lease_state")?.as_str(), "active" | "expiring")
            || row.try_get::<Option<i64>, _>("lease_synced_revision")? != Some(i64::try_from(status.lease_revision.get())?)
        {
            return Err(ResourceStoreError::CapacityReadbackInvalid);
        }
        let prior = sqlx::query("SELECT revision,generation,storage_purged,receipt FROM resource.environment_cleanup_fences WHERE lease_id=$1 FOR UPDATE")
            .bind(expected.lease.id.as_uuid()).fetch_optional(&mut *tx).await?;
        if let Some(row) = prior {
            let revision: i64 = row.try_get("revision")?;
            let generation: i64 = row.try_get("generation")?;
            let purged: bool = row.try_get("storage_purged")?;
            if status.revision.get() < u64::try_from(revision)?
                || status.generation < u64::try_from(generation)?
                || (status.revision.get() == u64::try_from(revision)? && row.try_get::<serde_json::Value, _>("receipt")? != receipt)
                || (purged && !status.storage_purged)
            {
                return Err(ResourceStoreError::CapacityReadbackInvalid);
            }
        }
        let now: time::OffsetDateTime = sqlx::query_scalar("SELECT date_trunc('milliseconds',clock_timestamp())").fetch_one(&mut *tx).await?;
        if let Some(usage) = status.retained_storage_usage.as_ref() {
            let prior = sqlx::query("SELECT measured_until,storage_bytes,closed FROM resource.retained_storage_intervals WHERE environment_id=$1 AND started_at=$2 FOR UPDATE")
                 .bind(environment_id.as_uuid()).bind(usage.started_at.get()).fetch_optional(&mut *tx).await?;
            if let Some(row) = prior {
                let watermark: time::OffsetDateTime = row.try_get("measured_until")?;
                let closed: bool = row.try_get("closed")?;
                if usage.measured_until.get() < watermark
                    || (closed && usage.measured_until.get() != watermark)
                    || (closed && !usage.closed)
                    || row.try_get::<i64, _>("storage_bytes")? != i64::try_from(usage.storage_bytes)?
                {
                    return Err(ResourceStoreError::CapacityReadbackInvalid);
                }
            }
            let disk = status.disk_retention.as_ref();
            let interval_closed = usage.closed || status.storage_purged || disk.is_some_and(|d| d.reactivated_at.is_some());
            let closed_at = disk.and_then(|d| d.reactivated_at.or(d.purged_at).map(UtcTimestamp::get))
                .or_else(|| interval_closed.then_some(usage.measured_until.get()));
            sqlx::query("INSERT INTO resource.retained_storage_intervals(environment_id,started_at,project_id,request_id,lease_id,generation,measured_until,priced_until,storage_bytes,closed,closed_at) VALUES($1,$2,$3,$4,$5,$6,$7,$2,$8,$9,$10) ON CONFLICT(environment_id,started_at) DO UPDATE SET generation=EXCLUDED.generation,measured_until=EXCLUDED.measured_until,closed=resource.retained_storage_intervals.closed OR EXCLUDED.closed,closed_at=COALESCE(resource.retained_storage_intervals.closed_at,EXCLUDED.closed_at) WHERE NOT resource.retained_storage_intervals.closed OR EXCLUDED.closed")
                .bind(environment_id.as_uuid()).bind(usage.started_at.get())
                .bind(expected.request.project_id.as_uuid()).bind(expected.request.id.as_uuid())
                .bind(expected.lease.id.as_uuid()).bind(i64::try_from(status.generation)?)
                .bind(usage.measured_until.get()).bind(i64::try_from(usage.storage_bytes)?)
                .bind(interval_closed).bind(closed_at).execute(&mut *tx).await?;
            match price_retained_interval(&mut tx, environment_id.as_uuid(), usage.started_at.get()).await {
                Ok(()) | Err(ResourceStoreError::RateUnconfigured) => {},
                Err(error) => return Err(error),
            }
        }
        let disk = status.disk_retention.as_ref();
        sqlx::query("INSERT INTO resource.environment_cleanup_fences(lease_id,environment_id,revision,generation,compute_released,compute_retired_at,storage_purged,reactivated_generation,reactivated_at,updated_at,receipt) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) ON CONFLICT(lease_id) DO UPDATE SET revision=EXCLUDED.revision,generation=EXCLUDED.generation,compute_released=resource.environment_cleanup_fences.compute_released OR EXCLUDED.compute_released,compute_retired_at=COALESCE(resource.environment_cleanup_fences.compute_retired_at,EXCLUDED.compute_retired_at),storage_purged=resource.environment_cleanup_fences.storage_purged OR EXCLUDED.storage_purged,reactivated_generation=COALESCE(EXCLUDED.reactivated_generation,resource.environment_cleanup_fences.reactivated_generation),reactivated_at=COALESCE(EXCLUDED.reactivated_at,resource.environment_cleanup_fences.reactivated_at),updated_at=EXCLUDED.updated_at,receipt=EXCLUDED.receipt")
            .bind(expected.lease.id.as_uuid()).bind(environment_id.as_uuid())
            .bind(i64::try_from(status.revision.get())?).bind(i64::try_from(status.generation)?)
            .bind(status.compute_retired).bind(disk.and_then(|d| d.compute_retired_at.map(UtcTimestamp::get)))
            .bind(status.storage_purged)
            .bind(disk.and_then(|d| d.reactivated_at.and(d.restored_generation)).map(i64::try_from).transpose()?)
            .bind(disk.and_then(|d| d.reactivated_at.map(UtcTimestamp::get))).bind(now)
            .bind(&receipt)
            .execute(&mut *tx).await?;
        sqlx::query("UPDATE resource.capacity_claims SET compute_released=compute_released OR $2,storage_released=storage_released OR $3 WHERE claim_id=$1")
            .bind(expected.claim.id.as_uuid()).bind(status.compute_retired).bind(status.storage_purged)
            .execute(&mut *tx).await?;
        if status.compute_retired {
            sqlx::query("UPDATE resource.gpu_capacity_reservations SET state='released',released_at=COALESCE(released_at,$2) WHERE claim_id=$1 AND state='reserved'")
                .bind(expected.claim.id.as_uuid()).bind(now).execute(&mut *tx).await?;
        }
        Ok::<(), ResourceStoreError>(())
    }.await;
    if result.is_ok() {
        tx.commit().await?;
    } else {
        tx.rollback().await?;
    }
    result
}

async fn price_retained_interval(
    tx: &mut Transaction<'_, Postgres>,
    environment_id: Uuid,
    started_at: time::OffsetDateTime,
) -> Result<(), ResourceStoreError> {
    let row = sqlx::query("SELECT measured_until,priced_until,storage_bytes,closed_at FROM resource.retained_storage_intervals WHERE environment_id=$1 AND started_at=$2 FOR UPDATE")
        .bind(environment_id).bind(started_at).fetch_one(&mut **tx).await?;
    let measured_until: time::OffsetDateTime = row.try_get("measured_until")?;
    let closed_at: Option<time::OffsetDateTime> = row.try_get("closed_at")?;
    let until = closed_at.map_or(measured_until, |closed| closed.min(measured_until));
    let from: time::OffsetDateTime = row.try_get("priced_until")?;
    if until <= from {
        return Ok(());
    }
    let storage_bytes = u64::try_from(row.try_get::<i64, _>("storage_bytes")?)?;
    crate::costing::rate_lock(tx).await?;
    let unit = contracts::resource::ResourceBillingUnit::StorageByteSecond;
    let segments = crate::pricing::select_rate_segments(
        tx, unit, None, None, None,
        UtcTimestamp::from_utc(started_at)?, UtcTimestamp::from_utc(until)?,
    ).await?;
    let mut snapshots = Vec::with_capacity(segments.len());
    let mut currency = None;
    let mut amount = 0_i128;
    for (rate, millis) in segments {
        let quantity = u128::from(storage_bytes).checked_mul(millis)
            .ok_or(ResourceStoreError::NumericOverflow)?;
        if currency.as_ref().is_some_and(|value| value != &rate.unit_price.currency) {
            return Err(ResourceStoreError::RateCurrencyConflict);
        }
        if currency.is_none() {
            currency = Some(rate.unit_price.currency.clone());
        }
        let charge = crate::pricing::multiply_money_wide(&rate.unit_price, quantity, rate.unit_quantity, 1000)?;
        amount = amount.checked_add(charge.amount.scaled()).ok_or(ResourceStoreError::NumericOverflow)?;
        snapshots.push(serde_json::json!({
            "rateId": rate.id, "rateRevision": rate.revision, "unit": unit,
            "quantity": quantity.to_string(), "quantityDivisor": 1000,
            "unitQuantity": rate.unit_quantity, "unitPrice": rate.unit_price, "amount": charge,
        }));
    }
    // The provider reports a cumulative meter. Round cumulative rate quantities once,
    // then append only the unpaid delta; frequent sub-microcredit polls cannot lose cost.
    let previous: String = sqlx::query_scalar("SELECT COALESCE(sum(amount),0)::text FROM resource.retained_storage_platform_costs WHERE environment_id=$1 AND started_at=$2")
        .bind(environment_id).bind(started_at).fetch_one(&mut **tx).await?;
    let amount = amount
        .checked_sub(
            previous
                .parse::<i128>()
                .map_err(|_| ResourceStoreError::NumericOverflow)?,
        )
        .filter(|value| *value >= 0)
        .ok_or(ResourceStoreError::NumericOverflow)?;
    let snapshots = serde_json::Value::Array(snapshots);
    sqlx::query("INSERT INTO resource.retained_storage_platform_costs(cost_id,environment_id,started_at,measured_from,measured_until,storage_bytes,rate_snapshots,amount,currency) VALUES($1,$2,$3,$4,$5,$6,$7,$8::text::numeric,$9) ON CONFLICT(environment_id,started_at,measured_until) DO NOTHING")
        .bind(Uuid::now_v7()).bind(environment_id).bind(started_at).bind(from).bind(until)
        .bind(i64::try_from(storage_bytes)?).bind(snapshots).bind(amount.to_string())
        .bind(currency.ok_or(ResourceStoreError::RateUnconfigured)?)
        .execute(&mut **tx).await?;
    sqlx::query("UPDATE resource.retained_storage_intervals SET priced_until=$3 WHERE environment_id=$1 AND started_at=$2 AND priced_until=$4")
        .bind(environment_id).bind(started_at).bind(until).bind(from)
        .execute(&mut **tx).await?;
    Ok(())
}

pub(crate) async fn settle_retained_storage_once(
    pool: &sqlx::PgPool,
) -> Result<bool, ResourceStoreError> {
    let mut tx = pool.begin().await?;
    crate::costing::rate_lock(&mut tx).await?;
    let rows = sqlx::query("SELECT environment_id,started_at FROM resource.retained_storage_intervals WHERE priced_until<LEAST(measured_until,COALESCE(closed_at,measured_until)) AND settlement_next_attempt_at<=clock_timestamp() ORDER BY settlement_next_attempt_at,environment_id,started_at LIMIT 16 FOR UPDATE SKIP LOCKED")
        .fetch_all(&mut *tx).await?;
    let mut did_work = false;
    for row in rows {
        let environment_id = row.try_get("environment_id")?;
        let started_at = row.try_get("started_at")?;
        match price_retained_interval(&mut tx, environment_id, started_at).await {
            Ok(()) => did_work = true,
            Err(ResourceStoreError::RateUnconfigured) => {
                // Same saturating attempts and bounded exponential delay as pending usage.
                sqlx::query("UPDATE resource.retained_storage_intervals SET settlement_attempts=LEAST(settlement_attempts,2147483646)+1,settlement_next_attempt_at=clock_timestamp()+make_interval(secs=>LEAST(60,power(2,LEAST(settlement_attempts,6)))::integer) WHERE environment_id=$1 AND started_at=$2")
                    .bind(environment_id).bind(started_at).execute(&mut *tx).await?;
            },
            Err(error) => { tx.rollback().await?; return Err(error); },
        }
    }
    tx.commit().await?;
    Ok(did_work)
}
