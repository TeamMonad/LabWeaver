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
        let row=sqlx::query("SELECT w.*,l.contract AS lease_contract,(SELECT min(covered_from) FROM resource.environment_credit_windows WHERE lease_id=w.lease_id) AS initial_from FROM resource.environment_credit_windows w JOIN resource.resource_leases l USING(lease_id) WHERE w.lease_id=$1 ORDER BY w.lease_revision DESC LIMIT 1").bind(lease_id.as_uuid()).fetch_optional(&mut *tx).await?.ok_or(ResourceStoreError::NotFound)?;
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
            can_authorize_execution: lease.state == ResourceLeaseState::Active
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
