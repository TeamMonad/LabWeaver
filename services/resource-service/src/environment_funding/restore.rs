//! Restore is a new funding epoch, not a renewal of the retired hold.
use super::{PgResourceStore, ResourceStoreError, original_approval};
use contracts::resource::{
    ResourceLeaseState, ResourceRequest, ResourceRequestState, ResourceTarget,
    costing::ResourceCostEstimateRequest,
    credits::CreditHoldRequest,
    environment_funding::{EnvironmentCreditGrant, RestoreEnvironmentFundingRequest},
};
use contracts::{Revision, UtcTimestamp};
use sqlx::Row;

impl PgResourceStore {
    /// Serializes by lease, rechecks live scope/policy, and commits pricing, hold and revision together.
    #[allow(clippy::too_many_lines, reason = "one atomic restore admission")]
    pub async fn restore_environment_funding(
        &self,
        key: &str,
        owner: &str,
        input: &RestoreEnvironmentFundingRequest,
    ) -> Result<EnvironmentCreditGrant, ResourceStoreError> {
        let mut tx = self.pool().begin().await?;
        let lease = crate::store::load_locked_lease(&mut tx, input.lease_id).await?;
        let request: ResourceRequest = serde_json::from_value(
            sqlx::query_scalar(
                "SELECT contract FROM resource.resource_requests WHERE request_id=$1 FOR UPDATE",
            )
            .bind(lease.request_id.as_uuid())
            .fetch_one(&mut *tx)
            .await?,
        )?;
        request.validate()?;
        let (claim, approval) = original_approval(&mut tx, &request, &lease).await?;
        if input.restored_generation == 0
            || input.project_id != request.project_id
            || input.requester_id != request.requester_id
            || !matches!(request.target, ResourceTarget::Environment { environment_id, .. } if environment_id == input.environment_id)
            || request.state != ResourceRequestState::Active
            || lease.state != ResourceLeaseState::Active
        {
            return Err(ResourceStoreError::ScopeConflict);
        }
        // This is deliberately before either replay path: old membership is never authority.
        let binding = approval
            .runtime_authorization
            .as_ref()
            .ok_or(contracts::runtime_authorization::RuntimeAuthorizationError::ApprovalRequired)
            .map_err(crate::runtime_authorization::RuntimeBindingError::from)?;
        crate::runtime_authorization::validate_binding(&mut tx, &request, binding).await?;
        let now = UtcTimestamp::from_utc(
            sqlx::query_scalar("SELECT date_trunc('milliseconds',clock_timestamp())")
                .fetch_one(&mut *tx)
                .await?,
        )?;
        let revoked: bool = sqlx::query_scalar(
            "SELECT revoked_at IS NOT NULL FROM resource.resource_approvals WHERE approval_id=$1",
        )
        .bind(approval.id.as_uuid())
        .fetch_one(&mut *tx)
        .await?;
        if revoked || approval.created_at > now || approval.valid_until <= now {
            return Err(ResourceStoreError::ApprovalInvalid);
        }
        if let Some(replay) =
            crate::credits::reserve(&mut tx, "environment_restore_funding", key, &(owner, input))
                .await?
        {
            tx.commit().await?;
            return Ok(serde_json::from_value(replay)?);
        }
        let generation = i64::try_from(input.restored_generation)?;
        if let Some(row) = sqlx::query(
            "SELECT request,result FROM resource.environment_restore_funding WHERE lease_id=$1 AND restored_generation=$2",
        ).bind(lease.id.as_uuid()).bind(generation).fetch_optional(&mut *tx).await? {
            if row.try_get::<serde_json::Value, _>("request")? != serde_json::to_value(input)? {
                return Err(crate::credits::CreditError::Conflict.into());
            }
            let result: EnvironmentCreditGrant = serde_json::from_value(row.try_get("result")?)?;
            if result.hold.owner_client_id != owner {
                return Err(crate::credits::CreditError::Denied.into());
            }
            crate::credits::complete(&mut tx, "environment_restore_funding", key, &result).await?;
            tx.commit().await?;
            return Ok(result);
        }
        let retired = sqlx::query(
            "SELECT c.compute_released,c.storage_released,f.generation FROM resource.capacity_claims c JOIN resource.environment_cleanup_fences f ON f.lease_id=$2 WHERE c.claim_id=$1",
        ).bind(claim.id.as_uuid()).bind(lease.id.as_uuid()).fetch_optional(&mut *tx).await?
            .ok_or(ResourceStoreError::UsageConflict)?;
        if !retired.try_get::<bool, _>("compute_released")?
            || retired.try_get::<bool, _>("storage_released")?
            || retired.try_get::<i64, _>("generation")? > generation
        {
            return Err(ResourceStoreError::UsageConflict);
        }
        let until = UtcTimestamp::from_utc(
            now.get()
                .checked_add(time::Duration::seconds(i64::try_from(
                    approval.approved_duration_seconds,
                )?))
                .ok_or(ResourceStoreError::NumericOverflow)?
                .min(approval.valid_until.get()),
        )?;
        crate::costing::rate_lock(&mut tx).await?;
        let estimate = crate::costing::estimate_in_transaction(
            &mut tx,
            request.project_id,
            &ResourceCostEstimateRequest {
                resources: claim.workload_resources.clone(),
                compute_from: now,
                compute_until: until,
                storage_from: now,
                storage_until: until,
            },
            claim.gpu_allocation.as_ref(),
        )
        .await?;
        if estimate.total.currency != "CREDITS" {
            return Err(ResourceStoreError::RateCurrencyConflict);
        }
        if let Some(allocation) = claim.gpu_allocation.as_ref() {
            self.validate_existing_gpu_allocation(&mut tx, allocation, None)
                .await?;
            let changed = sqlx::query("UPDATE resource.gpu_capacity_reservations SET state='reserved',released_at=NULL WHERE claim_id=$1 AND state='released'")
                .bind(claim.id.as_uuid()).execute(&mut *tx).await?;
            if changed.rows_affected() != 1 {
                return Err(ResourceStoreError::AllocationMismatch);
            }
        }
        let hold = crate::credits::hold_in_transaction(
            &mut tx,
            &format!("restore-{}-{generation}", lease.id),
            owner,
            &CreditHoldRequest {
                project_id: request.project_id,
                request_id: request.id,
                business_key: format!("restore-{}-{generation}", lease.id),
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
        let mut next = lease.clone();
        next.revision = Revision::new(
            lease
                .revision
                .get()
                .checked_add(1)
                .ok_or(ResourceStoreError::NumericOverflow)?,
        )?;
        next.active_from = Some(now);
        next.expires_at = Some(until);
        next.updated_at = now;
        next.validate()?;
        crate::store::update_lease(&mut tx, &lease, &next).await?;
        sqlx::query("INSERT INTO resource.environment_credit_windows(lease_id,lease_revision,request_id,claim_id,approval_id,hold_id,covered_from,covered_until,estimate) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)")
            .bind(lease.id.as_uuid()).bind(i64::try_from(next.revision.get())?)
            .bind(request.id.as_uuid()).bind(claim.id.as_uuid()).bind(approval.id.as_uuid())
            .bind(hold.id).bind(now.get()).bind(until.get()).bind(serde_json::to_value(&estimate)?)
            .execute(&mut *tx).await?;
        let result = EnvironmentCreditGrant {
            request_id: request.id,
            lease_id: lease.id,
            lease_revision: next.revision,
            claim_id: claim.id,
            approval_id: approval.id,
            covered_from: now,
            covered_until: until,
            hold,
            estimate,
            can_authorize_execution: true,
        };
        sqlx::query("INSERT INTO resource.environment_restore_funding(lease_id,restored_generation,execution_generation,lease_revision,request,result,hold_id) VALUES($1,$2,$2+1,$3,$4,$5,$6)")
            .bind(lease.id.as_uuid()).bind(generation).bind(i64::try_from(next.revision.get())?)
            .bind(serde_json::to_value(input)?).bind(serde_json::to_value(&result)?)
            .bind(result.hold.id).execute(&mut *tx).await?;
        crate::credits::complete(&mut tx, "environment_restore_funding", key, &result).await?;
        tx.commit().await?;
        Ok(result)
    }
}
