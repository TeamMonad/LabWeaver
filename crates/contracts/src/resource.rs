//! Resource request, capacity claim, and Lease contracts.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    ActorId, CapacityClaimId, CourseId, EnvironmentId, LeaseId, ProjectId, ReleaseId,
    ResourceApprovalId, ResourceRequestId, Revision, Sha256Digest, UtcTimestamp,
};

/// Requested or approved workload resources, independent of Kubernetes quantity syntax.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkloadResources {
    pub cpu_millicores: u32,
    pub memory_bytes: u64,
    pub storage_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu: Option<GpuRequest>,
}

impl WorkloadResources {
    pub fn validate(&self) -> Result<(), ResourceError> {
        if self.cpu_millicores == 0 || self.memory_bytes == 0 || self.storage_bytes == 0 {
            return Err(ResourceError::InvalidResources);
        }
        if let Some(gpu) = &self.gpu {
            gpu.validate()?;
        }
        Ok(())
    }
}

/// A policy-catalogued GPU class. It intentionally does not expose Kubernetes resource names.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GpuRequest {
    pub class: String,
    pub count: u32,
}

impl GpuRequest {
    fn validate(&self) -> Result<(), ResourceError> {
        if self.class.is_empty()
            || self.class.len() > 63
            || !self
                .class
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            || self.class.starts_with('-')
            || self.class.ends_with('-')
            || self.count == 0
        {
            return Err(ResourceError::InvalidResources);
        }
        Ok(())
    }
}

/// Request lifecycle owned by Resource Service.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceRequestState {
    Reviewing,
    Allocating,
    Active,
    Expiring,
    Expired,
    Rejected,
    Cancelled,
}

impl ResourceRequestState {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Expired | Self::Rejected | Self::Cancelled)
    }
}

/// Closed Lease state consumed by other service boundaries.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceLeaseState {
    Allocating,
    Active,
    Expiring,
    Expired,
    Revoked,
}

/// Immutable identity of a Resource request's target Work environment.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceTarget {
    pub environment_id: EnvironmentId,
    pub release_id: ReleaseId,
    pub release_version: u64,
    pub release_sha256: Sha256Digest,
}

impl ResourceTarget {
    fn validate(&self) -> Result<(), ResourceError> {
        if self.release_version == 0 {
            return Err(ResourceError::InvalidTarget);
        }
        Ok(())
    }
}

/// PostgreSQL-authoritative request projection without provider internals.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceRequest {
    pub id: ResourceRequestId,
    pub generation: u64,
    pub request_key: String,
    pub requester_id: ActorId,
    pub course_id: CourseId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<ProjectId>,
    pub target: ResourceTarget,
    pub requested_resources: WorkloadResources,
    pub requested_duration_seconds: u64,
    pub state: ResourceRequestState,
    pub revision: Revision,
    pub created_at: UtcTimestamp,
    pub updated_at: UtcTimestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic_code: Option<String>,
}

impl ResourceRequest {
    pub fn validate(&self) -> Result<(), ResourceError> {
        if self.generation == 0
            || self.request_key.is_empty()
            || self.request_key.len() > 96
            || !self.request_key.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
            })
            || self.requested_duration_seconds == 0
            || self.updated_at < self.created_at
        {
            return Err(ResourceError::InvalidRequest);
        }
        self.target.validate()?;
        self.requested_resources.validate()
    }
}

/// Append-only administrator decision bound to one request revision and policy snapshot.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceApproval {
    pub id: ResourceApprovalId,
    pub request_id: ResourceRequestId,
    pub request_revision: Revision,
    pub approver_id: ActorId,
    pub provider_binding: String,
    pub policy_sha256: Sha256Digest,
    pub approved_resources: WorkloadResources,
    pub approved_duration_seconds: u64,
    pub reason: String,
    pub valid_until: UtcTimestamp,
    pub created_at: UtcTimestamp,
}

impl ResourceApproval {
    pub fn validate(&self) -> Result<(), ResourceError> {
        if self.provider_binding.is_empty()
            || self.provider_binding.len() > 120
            || self.reason.trim().is_empty()
            || self.reason.chars().count() > 500
            || self.approved_duration_seconds == 0
            || self.valid_until <= self.created_at
        {
            return Err(ResourceError::InvalidApproval);
        }
        self.approved_resources.validate()
    }
}

/// Exact capacity allocation identity. Handles remain private to the provider implementation.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CapacityClaim {
    pub id: CapacityClaimId,
    pub request_id: ResourceRequestId,
    pub approval_id: ResourceApprovalId,
    pub provider_binding: String,
    pub policy_sha256: Sha256Digest,
    pub workload_resources: WorkloadResources,
    pub quota_resources: WorkloadResources,
    pub quota_plan_sha256: Sha256Digest,
    pub state: CapacityClaimState,
    pub revision: Revision,
}

/// Provider-owned capacity-shell lifecycle. Resource remains the authority for every transition.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapacityClaimState {
    Reserved,
    Provisioning,
    Ready,
    HandedOff,
    Releasing,
    Released,
    Blocked,
}

impl CapacityClaim {
    pub fn validate(&self) -> Result<(), ResourceError> {
        if self.provider_binding.is_empty() || self.provider_binding.len() > 120 {
            return Err(ResourceError::InvalidClaim);
        }
        self.workload_resources.validate()?;
        self.quota_resources.validate()
    }
}

/// PostgreSQL-authoritative Lease projection. Its authorization is valid only while Active.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceLease {
    pub id: LeaseId,
    pub request_id: ResourceRequestId,
    pub claim_id: CapacityClaimId,
    pub state: ResourceLeaseState,
    pub revision: Revision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_from: Option<UtcTimestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<UtcTimestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoke_reason_code: Option<String>,
    pub created_at: UtcTimestamp,
    pub updated_at: UtcTimestamp,
}

impl ResourceLease {
    pub fn validate(&self) -> Result<(), ResourceError> {
        let active_window = self.active_from.zip(self.expires_at);
        if self.active_from.is_some() != self.expires_at.is_some() {
            return Err(ResourceError::InvalidLease);
        }
        if self.state == ResourceLeaseState::Allocating {
            if active_window.is_some() {
                return Err(ResourceError::InvalidLease);
            }
        } else if self.state == ResourceLeaseState::Active
            && !active_window.is_some_and(|(from, until)| until > from)
        {
            return Err(ResourceError::InvalidLease);
        }
        if active_window.is_some_and(|(from, until)| until <= from) {
            return Err(ResourceError::InvalidLease);
        }
        Ok(())
    }
}

/// Resource-owned Active Lease authorization passed to Environment Service.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceLeaseAuthorization {
    pub lease_id: LeaseId,
    pub lease_revision: Revision,
    pub claim_id: CapacityClaimId,
    pub environment_id: EnvironmentId,
    pub course_id: CourseId,
    pub owner_actor_id: ActorId,
    pub project_id: Option<ProjectId>,
    pub provider_binding: String,
    pub approved_resources: WorkloadResources,
    pub quota_plan_sha256: Sha256Digest,
    pub active_from: UtcTimestamp,
    pub expires_at: UtcTimestamp,
}

impl ResourceLeaseAuthorization {
    pub fn validate(&self) -> Result<(), ResourceError> {
        if self.provider_binding.is_empty() || self.active_from >= self.expires_at {
            return Err(ResourceError::InvalidLease);
        }
        self.approved_resources.validate()
    }
}

/// Stable contract validation failures. Services map these to diagnostics at their boundary.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ResourceError {
    #[error("invalid resource request")]
    InvalidRequest,
    #[error("invalid resource target")]
    InvalidTarget,
    #[error("invalid resource quantities")]
    InvalidResources,
    #[error("invalid resource approval")]
    InvalidApproval,
    #[error("invalid capacity claim")]
    InvalidClaim,
    #[error("invalid resource lease")]
    InvalidLease,
}

#[cfg(test)]
mod tests {
    use super::{GpuRequest, ResourceError, WorkloadResources};

    #[test]
    fn resources_reject_zero_and_non_catalogued_gpu_syntax() {
        assert!(matches!(
            WorkloadResources {
                cpu_millicores: 0,
                memory_bytes: 1,
                storage_bytes: 1,
                gpu: None
            }
            .validate(),
            Err(ResourceError::InvalidResources)
        ));
        assert!(
            WorkloadResources {
                cpu_millicores: 1,
                memory_bytes: 1,
                storage_bytes: 1,
                gpu: Some(GpuRequest {
                    class: "nvidia.com/gpu".into(),
                    count: 1
                }),
            }
            .validate()
            .is_err()
        );
    }
}
