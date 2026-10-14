//! Priced admission calls the one Credits authority inside the caller's authorization transaction.
use crate::costing::estimate_in_transaction;
use crate::store::ResourceStoreError;
use contracts::resource::costing::{EstimatedCreditHold, ResourceCostEstimateRequest};
use contracts::resource::credits::CreditHoldRequest;
use contracts::resource::{ResourceRequest, ResourceTarget};
use sqlx::{Postgres, Row, Transaction};
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
