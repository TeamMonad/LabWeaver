//! Original lease/claim/approval facts verified through the production Resource reader.
use super::TestResult;
use contracts::resource::{ResourceApproval, ResourceRequest};
use contracts::runtime_authorization::RuntimeAuthorizationError;
use contracts::{LeaseId, ResourceRequestId, Revision, UtcTimestamp};
use resource_service::{
    runtime_authorization::{
        RuntimeBindingError, current_authorization, current_environment_authorization,
    },
    store::{PendingAllocation, PgResourceStore},
};
use sqlx::PgPool;

// Keep the real allocation and overdue projection checks in one ordered scenario.
#[allow(clippy::too_many_lines)]
pub(super) async fn assert_original_lease_chain(
    pool: &PgPool,
    store: &PgResourceStore,
    approval: &ResourceApproval,
    allocation: &PendingAllocation,
) -> TestResult<ResourceRequest> {
    assert!(matches!(
        current_environment_authorization(pool, approval.request_id, None).await,
        Err(RuntimeBindingError::LeaseNotActive)
    ));
    let provisioning = store
        .claim_next_capacity_shell()
        .await?
        .ok_or("no capacity claim")?;
    assert_eq!(provisioning.claim.id, allocation.claim.id);
    let active_from = store.current_time().await?;
    let lease = store
        .activate_lease(
            allocation.lease_id,
            Revision::new(1)?,
            active_from,
            UtcTimestamp::from_utc(active_from.get() + time::Duration::minutes(10))?,
            approval.approver_id,
            "runtime-chain-active",
        )
        .await?;
    let result =
        current_environment_authorization(pool, approval.request_id, Some(lease.id)).await?;
    let binding = approval
        .runtime_authorization
        .as_ref()
        .ok_or("no runtime binding")?;
    assert_eq!(&result.binding, binding);
    assert_eq!(result.approval_id, approval.id);
    assert_eq!(result.approval_valid_until, approval.valid_until);
    assert_eq!(result.lease.lease_id, lease.id);
    assert_eq!(result.lease.resource_request_id, approval.request_id);
    assert_eq!(
        result.lease.capacity_binding,
        allocation.claim.id.to_string()
    );
    assert_eq!(
        Some(result.lease.environment_id),
        binding.target.environment_id()
    );
    assert_eq!(result.lease.project_id, binding.project_id);
    assert_eq!(result.lease.course_id, binding.course_id);
    assert_eq!(result.provider_binding, approval.provider_binding);
    assert_eq!(
        result.applied_configuration.revision,
        binding.platform_configuration_revision
    );
    assert_eq!(
        result.applied_configuration.configuration_sha256,
        binding.platform_configuration_sha256
    );
    assert_eq!(
        current_environment_authorization(pool, approval.request_id, None)
            .await?
            .lease,
        result.lease
    );
    assert!(matches!(
        current_environment_authorization(pool, approval.request_id, Some(LeaseId::new())).await,
        Err(RuntimeBindingError::LeaseNotActive)
    ));
    assert!(matches!(
        current_environment_authorization(pool, ResourceRequestId::new(), Some(lease.id)).await,
        Err(RuntimeBindingError::LeaseNotActive)
    ));
    let active = store.load(approval.request_id).await?;
    assert_eq!(result.lease.owner_actor_id, active.requester_id);
    assert_eq!(binding.target, active.target);
    // Overdue/revoked database facts must fail before the background state transition runs.
    let mut overdue_approval = approval.clone();
    overdue_approval.valid_until =
        UtcTimestamp::from_utc(approval.created_at.get() + time::Duration::milliseconds(1))?;
    sqlx::query(
        "UPDATE resource.resource_approvals SET valid_until=$2,contract=$3 WHERE approval_id=$1",
    )
    .bind(approval.id.as_uuid())
    .bind(overdue_approval.valid_until.get())
    .bind(serde_json::to_value(&overdue_approval)?)
    .execute(pool)
    .await?;
    assert!(matches!(
        current_environment_authorization(pool, approval.request_id, Some(lease.id)).await,
        Err(RuntimeBindingError::LeaseNotActive)
    ));
    sqlx::query("UPDATE resource.resource_approvals SET valid_until=$2,contract=$3,revoked_at=clock_timestamp() WHERE approval_id=$1")
        .bind(approval.id.as_uuid()).bind(approval.valid_until.get())
        .bind(serde_json::to_value(approval)?).execute(pool).await?;
    assert!(matches!(
        current_environment_authorization(pool, approval.request_id, Some(lease.id)).await,
        Err(RuntimeBindingError::LeaseNotActive)
    ));
    assert!(matches!(
        current_authorization(pool, &active).await,
        Err(RuntimeBindingError::Authorization(
            RuntimeAuthorizationError::ApprovalRequired
        ))
    ));
    sqlx::query("UPDATE resource.resource_approvals SET revoked_at=NULL WHERE approval_id=$1")
        .bind(approval.id.as_uuid())
        .execute(pool)
        .await?;
    let mut overdue_lease = lease.clone();
    overdue_lease.expires_at = Some(UtcTimestamp::from_utc(
        active_from.get() + time::Duration::milliseconds(1),
    )?);
    sqlx::query("UPDATE resource.resource_leases SET expires_at=$2,contract=$3 WHERE lease_id=$1")
        .bind(lease.id.as_uuid())
        .bind(overdue_lease.expires_at.ok_or("no expired window")?.get())
        .bind(serde_json::to_value(&overdue_lease)?)
        .execute(pool)
        .await?;
    assert!(matches!(
        current_environment_authorization(pool, approval.request_id, Some(lease.id)).await,
        Err(RuntimeBindingError::LeaseNotActive)
    ));
    sqlx::query("UPDATE resource.resource_leases SET expires_at=$2,contract=$3 WHERE lease_id=$1")
        .bind(lease.id.as_uuid())
        .bind(lease.expires_at.ok_or("no expiry")?.get())
        .bind(serde_json::to_value(&lease)?)
        .execute(pool)
        .await?;
    assert_eq!(
        current_environment_authorization(pool, approval.request_id, Some(lease.id))
            .await?
            .binding,
        *binding
    );
    Box::pin(super::funded_handoff::exercise(pool, store, &result)).await?;
    Ok(active)
}
