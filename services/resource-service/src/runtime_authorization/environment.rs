//! Read-only execution evidence from the original Resource request, approval, claim and lease.
use super::{RuntimeBindingError, validated_policy};
use contracts::environment::{
    EnvironmentLeaseAuthorization, ResolveEnvironmentRuntimeAuthorizationRequest,
};
use contracts::platform_configuration::PlatformConfigurationVersion;
use contracts::resource::{
    CapacityClaim, CapacityClaimState, ResourceApproval, ResourceLease, ResourceLeaseState,
    ResourceRequest, ResourceRequestState, ResourceTarget,
};
use contracts::runtime_authorization::{RuntimeAuthorizationBinding, RuntimeAuthorizationError};
use contracts::{LeaseId, ResourceApprovalId, ResourceRequestId, UtcTimestamp};
use sqlx::{PgPool, Row};

/// Transient verified facts for the Environment resolver. This does not create a grant or approval.
#[derive(Clone, Debug)]
pub struct CurrentEnvironmentAuthorization {
    pub binding: RuntimeAuthorizationBinding,
    pub lease: EnvironmentLeaseAuthorization,
    pub approval_id: ResourceApprovalId,
    pub approval_valid_until: UtcTimestamp,
    pub provider_binding: String,
    /// Exactly the verified version checked against binding, including UID and execution location.
    pub applied_configuration: PlatformConfigurationVersion,
}

/// Consumes an active original lease. Work supplies its exact lease ID; Experiment may resolve it
/// from the original request ID. The caller must compare returned identities to the real instance.
pub async fn current_environment_authorization(
    pool: &PgPool,
    request_id: ResourceRequestId,
    expected_lease_id: Option<LeaseId>,
) -> Result<CurrentEnvironmentAuthorization, RuntimeBindingError> {
    load_environment_authorization(pool, request_id, expected_lease_id, None).await
}

/// Resolves attachment scope before distinguishing allocation from an inactive lease.
pub async fn resolve_environment_authorization(
    pool: &PgPool,
    input: &ResolveEnvironmentRuntimeAuthorizationRequest,
) -> Result<CurrentEnvironmentAuthorization, RuntimeBindingError> {
    load_environment_authorization(
        pool,
        input.resource_request_id,
        input.expected_lease_id,
        Some(input),
    )
    .await
}

#[allow(
    clippy::too_many_lines,
    reason = "Scoped allocation classification and active evidence share one original lease fence"
)]
async fn load_environment_authorization(
    pool: &PgPool,
    request_id: ResourceRequestId,
    expected_lease_id: Option<LeaseId>,
    input: Option<&ResolveEnvironmentRuntimeAuthorizationRequest>,
) -> Result<CurrentEnvironmentAuthorization, RuntimeBindingError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|_| RuntimeBindingError::Unavailable)?;
    // Lease writers lock lease before request. Only lock the lease here, then read the committed
    // chain in one statement; do not reverse that order against cancellation/activation writers.
    let row = sqlx::query(
        "SELECT r.contract AS request_contract,l.contract AS lease_contract,
                c.contract AS claim_contract,a.contract AS approval_contract,
                (r.state='active' AND l.state='active' AND l.expires_at>clock_timestamp()
                 AND c.state IN ('provisioning','ready','handed_off')) AS active,
                (r.state='allocating' AND l.state='allocating' AND l.active_from IS NULL AND l.expires_at IS NULL
                 AND c.state IN ('reserved','provisioning','blocked')) AS allocating
         FROM resource.resource_leases l
         JOIN resource.resource_requests r ON r.request_id=l.request_id
         JOIN resource.capacity_claims c ON c.claim_id=l.claim_id AND c.request_id=r.request_id
         JOIN resource.resource_approvals a ON a.approval_id=c.approval_id AND a.request_id=r.request_id
         WHERE r.request_id=$1 AND ($2::uuid IS NULL OR l.lease_id=$2)
           AND a.revoked_at IS NULL AND a.valid_until>clock_timestamp()
         FOR SHARE OF l",
    ).bind(request_id.as_uuid()).bind(expected_lease_id.map(LeaseId::as_uuid))
        .fetch_optional(&mut *tx).await.map_err(|_| RuntimeBindingError::Unavailable)?
        .ok_or(RuntimeBindingError::LeaseNotActive)?;
    let request: ResourceRequest = decode(row.try_get("request_contract"))?;
    let lease: ResourceLease = decode(row.try_get("lease_contract"))?;
    let claim: CapacityClaim = decode(row.try_get("claim_contract"))?;
    let approval: ResourceApproval = decode(row.try_get("approval_contract"))?;
    let now = UtcTimestamp::from_utc(
        sqlx::query_scalar("SELECT date_trunc('milliseconds',clock_timestamp())")
            .fetch_one(&mut *tx)
            .await
            .map_err(|_| RuntimeBindingError::Unavailable)?,
    )
    .map_err(|_| RuntimeBindingError::Unavailable)?;
    if let Some(input) = input {
        verify_input(&request, input)?;
        if input.environment_id.is_none()
            && expected_lease_id.is_none()
            && row
                .try_get::<bool, _>("allocating")
                .map_err(|_| RuntimeBindingError::Unavailable)?
            && request.state == ResourceRequestState::Allocating
            && lease.state == ResourceLeaseState::Allocating
            && lease.active_from.is_none()
            && lease.expires_at.is_none()
            && matches!(
                claim.state,
                CapacityClaimState::Reserved
                    | CapacityClaimState::Provisioning
                    | CapacityClaimState::Blocked
            )
            && approval.created_at <= now
            && approval.valid_until > now
        {
            verify_identity(request_id, &request, &lease, &claim, &approval)?;
            let binding = approval
                .runtime_authorization
                .as_ref()
                .ok_or(RuntimeAuthorizationError::ApprovalRequired)?;
            validated_policy(&mut tx, &request, binding).await?;
            return Err(RuntimeBindingError::AllocationPending);
        }
    }
    if !row
        .try_get::<bool, _>("active")
        .map_err(|_| RuntimeBindingError::Unavailable)?
    {
        return Err(RuntimeBindingError::LeaseNotActive);
    }
    let authorization = verify_chain(request_id, &request, &lease, &claim, &approval, now)?;
    if expected_lease_id.is_some_and(|expected| expected != lease.id) {
        return Err(RuntimeAuthorizationError::ConfigurationMismatch.into());
    }
    let binding = approval
        .runtime_authorization
        .as_ref()
        .ok_or(RuntimeAuthorizationError::ApprovalRequired)?;
    let applied_configuration = validated_policy(&mut tx, &request, binding).await?;
    if let Some(input) = input {
        validate_release(&mut tx, &request, binding, input).await?;
    }
    let result = CurrentEnvironmentAuthorization {
        binding: binding.clone(),
        lease: authorization,
        approval_id: approval.id,
        approval_valid_until: approval.valid_until,
        provider_binding: claim.provider_binding,
        applied_configuration,
    };
    tx.commit()
        .await
        .map_err(|_| RuntimeBindingError::Unavailable)?;
    Ok(result)
}

fn verify_input(
    request: &ResourceRequest,
    input: &ResolveEnvironmentRuntimeAuthorizationRequest,
) -> Result<(), RuntimeBindingError> {
    let ResourceTarget::Environment {
        environment_id,
        release_id,
        release_version,
    } = request.target
    else {
        return Err(RuntimeAuthorizationError::TargetNotEnvironment.into());
    };
    if request.id != input.resource_request_id
        || request.project_id != input.project_id
        || request.course_id != input.course_id
        || request.requester_id != input.owner_actor_id
        || input
            .environment_id
            .is_some_and(|expected| expected != environment_id)
        || input.release_version == 0
        || (input.environment_id.is_none()
            && (input.release_id != release_id || input.release_version != release_version))
    {
        return Err(RuntimeAuthorizationError::ConfigurationMismatch.into());
    }
    Ok(())
}

async fn validate_release(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    request: &ResourceRequest,
    binding: &RuntimeAuthorizationBinding,
    input: &ResolveEnvironmentRuntimeAuthorizationRequest,
) -> Result<(), RuntimeBindingError> {
    if input.release_id == binding.configuration.release_id
        && input.release_version == binding.configuration.release_version
    {
        return Ok(());
    }
    let mut target = request.clone();
    let ResourceTarget::Environment { environment_id, .. } = request.target else {
        return Err(RuntimeAuthorizationError::TargetNotEnvironment.into());
    };
    target.target = ResourceTarget::Environment {
        environment_id,
        release_id: input.release_id,
        release_version: input.release_version,
    };
    let (identity, spec, _) = super::current_inputs(tx, &target).await?;
    if spec.class != contracts::authoring::EnvironmentClass::Work
        || spec.runtime.kind() != contracts::authoring::RuntimeKind::Container
        || input.release_version <= binding.configuration.release_version
    {
        return Err(RuntimeAuthorizationError::ConfigurationMismatch.into());
    }
    binding.configuration.validate_compatible(&identity)?;
    Ok(())
}

fn decode<T: serde::de::DeserializeOwned>(
    value: Result<serde_json::Value, sqlx::Error>,
) -> Result<T, RuntimeBindingError> {
    serde_json::from_value(value.map_err(|_| RuntimeBindingError::Unavailable)?)
        .map_err(|_| RuntimeBindingError::Unavailable)
}

fn verify_chain(
    request_id: ResourceRequestId,
    request: &ResourceRequest,
    lease: &ResourceLease,
    claim: &CapacityClaim,
    approval: &ResourceApproval,
    now: UtcTimestamp,
) -> Result<EnvironmentLeaseAuthorization, RuntimeBindingError> {
    verify_identity(request_id, request, lease, claim, approval)?;
    let ResourceTarget::Environment { environment_id, .. } = request.target else {
        return Err(RuntimeAuthorizationError::TargetNotEnvironment.into());
    };
    let (Some(active_from), Some(expires_at)) = (lease.active_from, lease.expires_at) else {
        return Err(RuntimeBindingError::LeaseNotActive);
    };
    if request.state != ResourceRequestState::Active
        || lease.state != ResourceLeaseState::Active
        || !matches!(
            claim.state,
            CapacityClaimState::Provisioning
                | CapacityClaimState::Ready
                | CapacityClaimState::HandedOff
        )
        || active_from > now
        || expires_at <= now
        || approval.created_at > now
        || approval.valid_until <= now
    {
        return Err(RuntimeBindingError::LeaseNotActive);
    }
    let authorization = EnvironmentLeaseAuthorization {
        resource_request_id: request_id,
        lease_id: lease.id,
        lease_revision: lease.revision,
        environment_id,
        project_id: request.project_id,
        course_id: request.course_id,
        owner_actor_id: request.requester_id,
        capacity_binding: claim.id.to_string(),
        approved_resources: claim.workload_resources.clone(),
        gpu_allocation: claim.gpu_allocation.clone(),
        active_from,
        expires_at,
    };
    authorization
        .validate()
        .map_err(|_| RuntimeAuthorizationError::ConfigurationMismatch)?;
    Ok(authorization)
}

fn verify_identity(
    request_id: ResourceRequestId,
    request: &ResourceRequest,
    lease: &ResourceLease,
    claim: &CapacityClaim,
    approval: &ResourceApproval,
) -> Result<(), RuntimeBindingError> {
    for valid in [
        request.validate(),
        lease.validate(),
        claim.validate(),
        approval.validate(),
    ] {
        valid.map_err(|_| RuntimeAuthorizationError::ConfigurationMismatch)?;
    }
    if request.id != request_id
        || lease.request_id != request_id
        || claim.request_id != request_id
        || approval.request_id != request_id
        || lease.claim_id != claim.id
        || claim.approval_id != approval.id
        || approval.request_revision > request.revision
        || claim.provider_binding != approval.provider_binding
        || claim.workload_resources != approval.approved_resources
    {
        return Err(RuntimeAuthorizationError::ConfigurationMismatch.into());
    }
    Ok(())
}
