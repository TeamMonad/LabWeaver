//! Resource lifecycle guards shared by the HTTP and executor boundaries.
//!
//! Persistence, Kubernetes calls, and NATS delivery remain outside this module so every
//! transition can be tested deterministically before a side effect is scheduled.

#![allow(
    missing_docs,
    reason = "public Resource wire semantics are documented in contracts and generated schemas"
)]

use contracts::resource::{
    ResourceApproval, ResourceError, ResourceLease, ResourceLeaseState, ResourceRequest,
    ResourceRequestState,
};
use contracts::{Revision, UtcTimestamp};

pub mod api;
pub mod capacity;
pub mod messaging;
pub mod outbox;
pub mod process;
pub mod store;

pub use process::{ResourceProcessRuntime, ResourceProcessRuntimeError};

/// Stable Resource-domain diagnostics. These values are safe to return in RFC 9457 responses.
pub mod diagnostic {
    pub const REQUEST_STATE_CONFLICT: &str = "LW_RESOURCE_REQUEST_STATE_CONFLICT";
    pub const APPROVAL_EXPIRED: &str = "LW_RESOURCE_APPROVAL_EXPIRED";
    pub const CAPACITY_EXHAUSTED: &str = "LW_RESOURCE_CAPACITY_EXHAUSTED";
    pub const GPU_CAPACITY_EXHAUSTED: &str = "LW_RESOURCE_CAPACITY_EXHAUSTED";
}

/// A policy snapshot needed before a capacity provider may be invoked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ApprovalPolicy {
    pub min_duration_seconds: u64,
    pub max_duration_seconds: u64,
    /// Sprint 3 deliberately ships with zero GPU capacity. A non-zero GPU request is a
    /// capacity denial, not a malformed request and never triggers a provider fallback.
    pub gpu_capacity: u32,
}

impl ApprovalPolicy {
    pub fn validate(self) -> Result<(), LifecycleError> {
        if self.min_duration_seconds == 0 || self.max_duration_seconds < self.min_duration_seconds {
            return Err(LifecycleError::PolicyInvalid);
        }
        Ok(())
    }
}

/// Side-effect-free lifecycle guards.
pub struct ResourceLifecycle;

impl ResourceLifecycle {
    pub fn accept_approval(
        request: &ResourceRequest,
        approval: &ResourceApproval,
        policy: ApprovalPolicy,
        now: UtcTimestamp,
    ) -> Result<ResourceRequest, LifecycleError> {
        request.validate().map_err(LifecycleError::Contract)?;
        approval.validate().map_err(LifecycleError::Contract)?;
        policy.validate()?;
        if request.state != ResourceRequestState::Reviewing
            || approval.request_id != request.id
            || approval.request_revision != request.revision
        {
            return Err(LifecycleError::StateConflict);
        }
        if approval.valid_until <= now {
            return Err(LifecycleError::ApprovalExpired);
        }
        if approval.approved_duration_seconds < policy.min_duration_seconds
            || approval.approved_duration_seconds > policy.max_duration_seconds
        {
            return Err(LifecycleError::PolicyInvalid);
        }
        if approval
            .approved_resources
            .gpu
            .as_ref()
            .is_some_and(|gpu| gpu.count > policy.gpu_capacity)
        {
            return Err(LifecycleError::CapacityExhausted);
        }
        let mut next = request.clone();
        next.state = ResourceRequestState::Allocating;
        next.revision = increment(request.revision)?;
        next.updated_at = now;
        next.diagnostic_code = None;
        Ok(next)
    }

    pub fn activate(
        request: &ResourceRequest,
        expected_revision: Revision,
        now: UtcTimestamp,
    ) -> Result<ResourceRequest, LifecycleError> {
        transition(
            request,
            expected_revision,
            ResourceRequestState::Allocating,
            ResourceRequestState::Active,
            now,
        )
    }

    pub fn begin_expiry(
        request: &ResourceRequest,
        expected_revision: Revision,
        now: UtcTimestamp,
    ) -> Result<ResourceRequest, LifecycleError> {
        if !matches!(
            request.state,
            ResourceRequestState::Active | ResourceRequestState::Allocating
        ) {
            return Err(LifecycleError::StateConflict);
        }
        transition(
            request,
            expected_revision,
            request.state,
            ResourceRequestState::Expiring,
            now,
        )
    }

    pub fn complete_expiry(
        request: &ResourceRequest,
        expected_revision: Revision,
        now: UtcTimestamp,
    ) -> Result<ResourceRequest, LifecycleError> {
        transition(
            request,
            expected_revision,
            ResourceRequestState::Expiring,
            ResourceRequestState::Expired,
            now,
        )
    }

    pub fn reject_or_cancel(
        request: &ResourceRequest,
        expected_revision: Revision,
        terminal: ResourceRequestState,
        now: UtcTimestamp,
    ) -> Result<ResourceRequest, LifecycleError> {
        if !matches!(
            terminal,
            ResourceRequestState::Rejected | ResourceRequestState::Cancelled
        ) || request.state != ResourceRequestState::Reviewing
        {
            return Err(LifecycleError::StateConflict);
        }
        transition(
            request,
            expected_revision,
            ResourceRequestState::Reviewing,
            terminal,
            now,
        )
    }

    pub fn retry(
        request: &ResourceRequest,
        expected_revision: Revision,
        now: UtcTimestamp,
    ) -> Result<ResourceRequest, LifecycleError> {
        if request.state != ResourceRequestState::Rejected {
            return Err(LifecycleError::StateConflict);
        }
        transition(
            request,
            expected_revision,
            ResourceRequestState::Rejected,
            ResourceRequestState::Reviewing,
            now,
        )
    }

    pub fn activate_lease(
        lease: &ResourceLease,
        expected_revision: Revision,
        active_from: UtcTimestamp,
        expires_at: UtcTimestamp,
    ) -> Result<ResourceLease, LifecycleError> {
        lease.validate().map_err(LifecycleError::Contract)?;
        if lease.state != ResourceLeaseState::Allocating
            || lease.revision != expected_revision
            || expires_at <= active_from
        {
            return Err(LifecycleError::StateConflict);
        }
        let mut next = lease.clone();
        next.state = ResourceLeaseState::Active;
        next.revision = increment(lease.revision)?;
        next.active_from = Some(active_from);
        next.expires_at = Some(expires_at);
        next.updated_at = active_from;
        next.validate().map_err(LifecycleError::Contract)?;
        Ok(next)
    }

    pub fn renew_lease(
        lease: &ResourceLease,
        expected_revision: Revision,
        expires_at: UtcTimestamp,
        now: UtcTimestamp,
    ) -> Result<ResourceLease, LifecycleError> {
        lease.validate().map_err(LifecycleError::Contract)?;
        if lease.state != ResourceLeaseState::Active
            || lease.revision != expected_revision
            || lease.expires_at.is_none_or(|current| expires_at <= current)
            || expires_at <= now
        {
            return Err(LifecycleError::StateConflict);
        }
        transition_lease(
            lease,
            ResourceLeaseState::Active,
            now,
            Some(expires_at),
            None,
        )
    }

    pub fn begin_lease_expiry(
        lease: &ResourceLease,
        expected_revision: Revision,
        now: UtcTimestamp,
        reason: Option<String>,
    ) -> Result<ResourceLease, LifecycleError> {
        lease.validate().map_err(LifecycleError::Contract)?;
        if !matches!(
            lease.state,
            ResourceLeaseState::Allocating | ResourceLeaseState::Active
        ) || lease.revision != expected_revision
        {
            return Err(LifecycleError::StateConflict);
        }
        transition_lease(
            lease,
            ResourceLeaseState::Expiring,
            now,
            lease.expires_at,
            reason,
        )
    }

    pub fn complete_lease_expiry(
        lease: &ResourceLease,
        expected_revision: Revision,
        now: UtcTimestamp,
    ) -> Result<ResourceLease, LifecycleError> {
        lease.validate().map_err(LifecycleError::Contract)?;
        if lease.state != ResourceLeaseState::Expiring || lease.revision != expected_revision {
            return Err(LifecycleError::StateConflict);
        }
        transition_lease(
            lease,
            ResourceLeaseState::Expired,
            now,
            lease.expires_at,
            lease.revoke_reason_code.clone(),
        )
    }

    pub fn revoke_lease(
        lease: &ResourceLease,
        expected_revision: Revision,
        now: UtcTimestamp,
        reason: String,
    ) -> Result<ResourceLease, LifecycleError> {
        lease.validate().map_err(LifecycleError::Contract)?;
        if is_terminal_lease_state(lease.state)
            || lease.revision != expected_revision
            || reason.trim().is_empty()
        {
            return Err(LifecycleError::StateConflict);
        }
        transition_lease(
            lease,
            ResourceLeaseState::Revoked,
            now,
            lease.expires_at,
            Some(reason),
        )
    }
}

const fn is_terminal_lease_state(state: ResourceLeaseState) -> bool {
    matches!(
        state,
        ResourceLeaseState::Expired | ResourceLeaseState::Revoked
    )
}

fn transition(
    request: &ResourceRequest,
    expected_revision: Revision,
    from: ResourceRequestState,
    to: ResourceRequestState,
    now: UtcTimestamp,
) -> Result<ResourceRequest, LifecycleError> {
    request.validate().map_err(LifecycleError::Contract)?;
    if request.revision != expected_revision || request.state != from || now < request.updated_at {
        return Err(LifecycleError::StateConflict);
    }
    let mut next = request.clone();
    next.state = to;
    next.revision = increment(request.revision)?;
    next.updated_at = now;
    next.diagnostic_code = None;
    Ok(next)
}

fn increment(revision: Revision) -> Result<Revision, LifecycleError> {
    Revision::new(
        revision
            .get()
            .checked_add(1)
            .ok_or(LifecycleError::RevisionOverflow)?,
    )
    .map_err(|_| LifecycleError::RevisionOverflow)
}

fn transition_lease(
    lease: &ResourceLease,
    state: ResourceLeaseState,
    now: UtcTimestamp,
    expires_at: Option<UtcTimestamp>,
    reason: Option<String>,
) -> Result<ResourceLease, LifecycleError> {
    if now < lease.updated_at {
        return Err(LifecycleError::StateConflict);
    }
    let mut next = lease.clone();
    next.state = state;
    next.revision = increment(lease.revision)?;
    next.active_from = lease.active_from;
    next.expires_at = expires_at;
    next.revoke_reason_code = reason;
    next.updated_at = now;
    next.validate().map_err(LifecycleError::Contract)?;
    Ok(next)
}

#[derive(Debug, thiserror::Error)]
pub enum LifecycleError {
    #[error("{0}")]
    Contract(#[from] ResourceError),
    #[error("LW_RESOURCE_REQUEST_STATE_CONFLICT")]
    StateConflict,
    #[error("LW_RESOURCE_APPROVAL_EXPIRED")]
    ApprovalExpired,
    #[error("LW_RESOURCE_CAPACITY_EXHAUSTED")]
    CapacityExhausted,
    #[error("LW_RESOURCE_POLICY_INVALID")]
    PolicyInvalid,
    #[error("LW_RESOURCE_REVISION_OVERFLOW")]
    RevisionOverflow,
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use contracts::resource::{
        ResourceApproval, ResourceLease, ResourceLeaseState, ResourceRequest, ResourceRequestState,
        ResourceTarget, WorkloadResources,
    };
    use contracts::{
        ActorId, CapacityClaimId, CourseId, EnvironmentId, LeaseId, ReleaseId, ResourceApprovalId,
        ResourceRequestId, Revision, Sha256Digest, UtcTimestamp,
    };

    use super::{ApprovalPolicy, LifecycleError, ResourceLifecycle};

    #[test]
    fn zero_gpu_capacity_blocks_a_valid_gpu_request_without_transitioning_state()
    -> Result<(), Box<dyn std::error::Error>> {
        let request = request();
        let approval = approval(
            request.id,
            WorkloadResources {
                cpu_millicores: 1000,
                memory_bytes: 1024,
                storage_bytes: 2048,
                gpu: Some(contracts::resource::GpuRequest {
                    class: "gpu-a100".into(),
                    count: 1,
                }),
            },
        );
        let result = ResourceLifecycle::accept_approval(
            &request,
            &approval,
            ApprovalPolicy {
                min_duration_seconds: 60,
                max_duration_seconds: 3600,
                gpu_capacity: 0,
            },
            timestamp("2026-07-30T00:00:00.000Z"),
        );
        assert!(matches!(result, Err(LifecycleError::CapacityExhausted)));
        assert_eq!(request.state, ResourceRequestState::Reviewing);
        Ok(())
    }

    #[test]
    fn approval_then_expiry_requires_exact_revisions() -> Result<(), Box<dyn std::error::Error>> {
        let request = request();
        let allocating = ResourceLifecycle::accept_approval(
            &request,
            &approval(
                request.id,
                WorkloadResources {
                    cpu_millicores: 500,
                    memory_bytes: 512,
                    storage_bytes: 1024,
                    gpu: None,
                },
            ),
            ApprovalPolicy {
                min_duration_seconds: 60,
                max_duration_seconds: 3600,
                gpu_capacity: 0,
            },
            timestamp("2026-07-30T00:00:00.000Z"),
        )?;
        let active = ResourceLifecycle::activate(
            &allocating,
            allocating.revision,
            timestamp("2026-07-30T00:00:01.000Z"),
        )?;
        assert!(
            ResourceLifecycle::begin_expiry(
                &active,
                Revision::new(1)?,
                timestamp("2026-07-30T00:00:02.000Z")
            )
            .is_err()
        );
        let expiring = ResourceLifecycle::begin_expiry(
            &active,
            active.revision,
            timestamp("2026-07-30T00:00:02.000Z"),
        )?;
        let expired = ResourceLifecycle::complete_expiry(
            &expiring,
            expiring.revision,
            timestamp("2026-07-30T00:00:03.000Z"),
        )?;
        assert_eq!(expired.state, ResourceRequestState::Expired);
        Ok(())
    }

    #[test]
    fn lease_activation_renewal_and_revoke_are_exactly_fenced()
    -> Result<(), Box<dyn std::error::Error>> {
        let allocating = ResourceLease {
            id: LeaseId::new(),
            request_id: ResourceRequestId::new(),
            claim_id: CapacityClaimId::new(),
            state: ResourceLeaseState::Allocating,
            revision: Revision::new(1)?,
            active_from: None,
            expires_at: None,
            revoke_reason_code: None,
            created_at: timestamp("2026-07-30T00:00:00.000Z"),
            updated_at: timestamp("2026-07-30T00:00:00.000Z"),
        };
        let active = ResourceLifecycle::activate_lease(
            &allocating,
            allocating.revision,
            timestamp("2026-07-30T00:00:01.000Z"),
            timestamp("2026-07-30T00:10:01.000Z"),
        )?;
        let renewed = ResourceLifecycle::renew_lease(
            &active,
            active.revision,
            timestamp("2026-07-30T00:15:01.000Z"),
            timestamp("2026-07-30T00:00:02.000Z"),
        )?;
        assert!(
            ResourceLifecycle::revoke_lease(
                &renewed,
                Revision::new(1)?,
                timestamp("2026-07-30T00:00:03.000Z"),
                "administrative revoke".into(),
            )
            .is_err()
        );
        let revoked = ResourceLifecycle::revoke_lease(
            &renewed,
            renewed.revision,
            timestamp("2026-07-30T00:00:03.000Z"),
            "administrative revoke".into(),
        )?;
        assert_eq!(revoked.state, ResourceLeaseState::Revoked);
        Ok(())
    }

    fn request() -> ResourceRequest {
        ResourceRequest {
            id: ResourceRequestId::new(),
            generation: 1,
            request_key: "workbench-1".into(),
            requester_id: ActorId::new(),
            course_id: CourseId::new(),
            project_id: None,
            target: ResourceTarget {
                environment_id: EnvironmentId::new(),
                release_id: ReleaseId::new(),
                release_version: 1,
                release_sha256: digest(),
            },
            requested_resources: WorkloadResources {
                cpu_millicores: 500,
                memory_bytes: 512,
                storage_bytes: 1024,
                gpu: None,
            },
            requested_duration_seconds: 600,
            state: ResourceRequestState::Reviewing,
            revision: Revision::new(1).unwrap_or_else(|_| unreachable!()),
            created_at: timestamp("2026-07-30T00:00:00.000Z"),
            updated_at: timestamp("2026-07-30T00:00:00.000Z"),
            diagnostic_code: None,
        }
    }

    fn approval(request_id: ResourceRequestId, resources: WorkloadResources) -> ResourceApproval {
        ResourceApproval {
            id: ResourceApprovalId::new(),
            request_id,
            request_revision: Revision::new(1).unwrap_or_else(|_| unreachable!()),
            approver_id: ActorId::new(),
            provider_binding: "kubernetes-standard".into(),
            policy_sha256: digest(),
            approved_resources: resources,
            approved_duration_seconds: 600,
            reason: "course capacity approved".into(),
            valid_until: timestamp("2026-07-30T00:05:00.000Z"),
            created_at: timestamp("2026-07-30T00:00:00.000Z"),
        }
    }

    fn digest() -> Sha256Digest {
        Sha256Digest::from_str(&"a".repeat(64)).unwrap_or_else(|_| unreachable!())
    }
    fn timestamp(value: &str) -> UtcTimestamp {
        UtcTimestamp::from_str(value).unwrap_or_else(|_| unreachable!())
    }
}
