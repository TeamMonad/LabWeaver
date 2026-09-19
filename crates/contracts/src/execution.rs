//! One-shot task execution admission and workload boundary contracts.
//!
//! Business services own task meaning and results. Resource owns admission. This module owns the
//! durable identity that binds one execution generation to an admitted Resource reservation and
//! the mechanical workload lifecycle outcomes that every execution backend reports. It
//! deliberately contains no backend implementation, Kubernetes object rendering, score, or
//! provider-specific field.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::http::TaskResourceStatus;
use crate::resource::{
    CapacityClaimState, ResourceLeaseState, ResourceRequestState, ResourceTarget,
};
use crate::{
    CapacityClaimId, DiagnosticCode, LeaseId, ProjectId, ResourceRequestId, Revision, TaskRunId,
    UtcTimestamp,
};

/// Durable identity of one admitted execution generation of a one-shot task.
///
/// The value is created from an authoritative [`TaskResourceStatus`] that is already active and
/// handed off, then persisted in the owning business service's recovery checkpoint. A resumed
/// worker proves it still holds the same reservation by matching the identity fields; a new
/// execution generation always has a new [`TaskRunId`] and therefore a new Resource reservation.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskExecutionBinding {
    /// Durable one-shot task identity; unique per execution generation.
    pub task_run_id: TaskRunId,
    /// Monotonic per-task execution generation. A new generation never reuses a released lease.
    pub execution_generation: u64,
    /// Authoritative Resource request that admitted this generation.
    pub resource_request_id: ResourceRequestId,
    /// Capacity claim created by Resource approval.
    pub capacity_claim_id: CapacityClaimId,
    /// Active lease that authorizes this exact generation to occupy capacity.
    pub lease_id: LeaseId,
    /// Claim revision observed when this generation was admitted.
    pub claim_revision: Revision,
    /// Lease revision observed when this generation was admitted.
    pub lease_revision: Revision,
    /// Project that owns the task.
    pub project_id: ProjectId,
    /// Capacity provider binding resolved by Resource for this reservation.
    pub provider_binding: String,
    /// Exact namespace in which the admitted workload must execute.
    pub namespace: String,
    /// Deterministic workload name owned by this execution generation.
    pub workload_name: String,
    /// Request correlation identity for diagnostics.
    pub trace_id: String,
}

impl TaskExecutionBinding {
    /// Creates the binding from an authoritative status that is admitted and executable.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutionContractError::AdmissionStateMismatch`] unless the status still holds an
    /// active request, a handed-off claim, and an active lease for the exact namespace and
    /// revisions, or [`ExecutionContractError::InvalidBinding`] for malformed identity values.
    pub fn from_admitted_status(
        status: &TaskResourceStatus,
        execution_generation: u64,
        workload_name: impl Into<String>,
        trace_id: impl Into<String>,
    ) -> Result<Self, ExecutionContractError> {
        let workload_name = workload_name.into();
        let trace_id = trace_id.into();
        let namespace = status
            .execution_namespace
            .clone()
            .ok_or(ExecutionContractError::AdmissionStateMismatch)?;
        let binding = Self {
            task_run_id: status.task_run_id,
            execution_generation,
            resource_request_id: status.request.id,
            capacity_claim_id: status.claim.id,
            lease_id: status.lease.id,
            claim_revision: status.claim_revision,
            lease_revision: status.lease_revision,
            project_id: status.project_id,
            provider_binding: status.claim.provider_binding.clone(),
            namespace,
            workload_name,
            trace_id,
        };
        binding.validate()?;
        if !binding.matches_admitted_status(status) {
            return Err(ExecutionContractError::AdmissionStateMismatch);
        }
        Ok(binding)
    }

    /// Validates bounded identity values independently of any Resource state.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutionContractError::InvalidBinding`] for malformed identity values.
    pub fn validate(&self) -> Result<(), ExecutionContractError> {
        if self.execution_generation == 0
            || !valid_binding_token(&self.provider_binding, 96)
            || !valid_namespace(&self.namespace)
            || !valid_workload_name(&self.workload_name)
            || !valid_trace_id(&self.trace_id)
        {
            return Err(ExecutionContractError::InvalidBinding);
        }
        Ok(())
    }

    /// Returns whether the binding still names the same Resource reservation.
    ///
    /// Identity fields are immutable for the lifetime of the reservation. Revisions can advance
    /// through renewal or expiry and are therefore not part of this comparison.
    #[must_use]
    pub fn same_reservation(&self, status: &TaskResourceStatus) -> bool {
        status.task_run_id == self.task_run_id
            && status.request.id == self.resource_request_id
            && status.claim.id == self.capacity_claim_id
            && status.lease.id == self.lease_id
            && status.project_id == self.project_id
            && matches!(
                status.request.target,
                ResourceTarget::Task { task_run_id } if task_run_id == self.task_run_id
            )
    }

    /// Returns whether the authoritative status admits this exact generation right now.
    #[must_use]
    pub fn matches_admitted_status(&self, status: &TaskResourceStatus) -> bool {
        self.same_reservation(status)
            && status.request.state == ResourceRequestState::Active
            && status.claim.state == CapacityClaimState::HandedOff
            && status.lease.state == ResourceLeaseState::Active
            && status.execution_namespace.as_deref() == Some(self.namespace.as_str())
            && status.claim.revision == status.claim_revision
            && status.lease.revision == status.lease_revision
            && status.claim_revision == self.claim_revision
            && status.lease_revision == self.lease_revision
            && status.claim.provider_binding == self.provider_binding
    }
}

/// Mechanical state of the Kubernetes workload behind one execution generation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionWorkloadState {
    /// The workload object exists but has not started its main container.
    Pending,
    /// The main container is running.
    Running,
    /// The main container terminated with a successful exit.
    Succeeded,
    /// The main container terminated with a non-zero exit or an execution deadline.
    Failed,
    /// The workload object is absent while the durable attempt still expects it.
    Missing,
    /// The backend could not determine the state; callers must keep the attempt unresolved.
    Unknown,
}

impl ExecutionWorkloadState {
    /// Returns whether this state ends the workload's life.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Missing)
    }
}

/// Bounded, payload-free observation of one admitted workload.
///
/// The observation never carries container output, terminal text, or role-specific receipts.
/// Business services map their own receipt format from the execution backend's role adapter.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionObservation {
    pub state: ExecutionWorkloadState,
    /// Main container exit code, present only for a terminated main container.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Bounded Kubernetes reason such as `DeadlineExceeded`, `OOMKilled`, or `Error`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    /// Deterministic pod name observed for the workload, when one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pod_name: Option<String>,
    /// Main container start time as reported by Kubernetes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<UtcTimestamp>,
    /// Main container termination time as reported by Kubernetes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminated_at: Option<UtcTimestamp>,
}

impl ExecutionObservation {
    /// Validates state, exit-code, reason, and timing consistency.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutionContractError::InvalidObservation`] for an inconsistent observation.
    pub fn validate(&self) -> Result<(), ExecutionContractError> {
        let terminated = matches!(
            self.state,
            ExecutionWorkloadState::Succeeded | ExecutionWorkloadState::Failed
        );
        // A failed Job may report a deadline without a terminated container, so
        // an exit code is only rejected outside a terminal state.
        if (self.exit_code.is_some() && !terminated)
            || self
                .reason_code
                .as_deref()
                .is_some_and(|reason| !valid_reason_code(reason))
            || self
                .pod_name
                .as_deref()
                .is_some_and(|name| !valid_workload_name(name))
            || self.started_at.is_some() != self.terminated_at.is_some()
            || self
                .started_at
                .zip(self.terminated_at)
                .is_some_and(|(started, terminated)| terminated <= started)
        {
            return Err(ExecutionContractError::InvalidObservation);
        }
        Ok(())
    }
}

/// One Kubernetes object owned by an execution generation.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionObjectRef {
    /// Exact API version used to address the object, such as `batch/v1`.
    pub api_version: String,
    /// Kubernetes collection name, such as `jobs` or `configmaps`.
    pub resource: String,
    pub name: String,
    /// Server-assigned UID proving ownership; never reused after deletion.
    pub uid: String,
}

impl ExecutionObjectRef {
    /// Validates the bounded object identity.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutionContractError::InvalidCleanupStatus`] for a malformed identity.
    pub fn validate(&self) -> Result<(), ExecutionContractError> {
        if !valid_api_version(&self.api_version)
            || !valid_resource_name(&self.resource)
            || !valid_workload_name(&self.name)
            || self.uid.is_empty()
            || self.uid.len() > 128
            || self.uid.chars().any(char::is_control)
        {
            return Err(ExecutionContractError::InvalidCleanupStatus);
        }
        Ok(())
    }
}

/// Deterministic result of one cleanup attempt.
///
/// A caller may only confirm Resource release for [`Self::Confirmed`]. `Pending` and `Unknown`
/// must keep the attempt unresolved and must never be rewritten as released.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ExecutionCleanupStatus {
    /// Every owned object is verifiably gone.
    Confirmed,
    /// At least one owned object still exists; the next cleanup pass continues.
    Pending {
        remaining_objects: Vec<ExecutionObjectRef>,
    },
    /// Cleanup could not be determined; the caller keeps the unresolved diagnostic.
    Unknown { diagnostic: DiagnosticCode },
}

impl ExecutionCleanupStatus {
    /// Validates the bounded object list and diagnostic.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutionContractError::InvalidCleanupStatus`] for malformed content.
    pub fn validate(&self) -> Result<(), ExecutionContractError> {
        match self {
            Self::Pending { remaining_objects } => {
                if remaining_objects.is_empty() || remaining_objects.len() > 16 {
                    return Err(ExecutionContractError::InvalidCleanupStatus);
                }
                for object in remaining_objects {
                    object.validate()?;
                }
                Ok(())
            }
            Self::Confirmed | Self::Unknown { .. } => Ok(()),
        }
    }

    /// Returns whether cleanup is verifiably complete.
    #[must_use]
    pub const fn is_confirmed(&self) -> bool {
        matches!(self, Self::Confirmed)
    }
}

/// Failure returned for malformed execution contract values.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ExecutionContractError {
    /// The binding identity is malformed.
    #[error("invalid task execution binding")]
    InvalidBinding,
    /// The binding does not match an admitted and executable Resource state.
    #[error("task execution admission state mismatch")]
    AdmissionStateMismatch,
    /// The observation is internally inconsistent.
    #[error("invalid execution observation")]
    InvalidObservation,
    /// The cleanup status is malformed.
    #[error("invalid execution cleanup status")]
    InvalidCleanupStatus,
}

fn valid_binding_token(value: &str, max_len: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_len
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'.'
        })
        && !value.starts_with('-')
        && !value.ends_with('-')
}

fn valid_namespace(value: &str) -> bool {
    valid_binding_token(value, 63)
}

fn valid_workload_name(value: &str) -> bool {
    !value.is_empty() && value.len() <= 253 && value.split('.').all(valid_namespace)
}

fn valid_trace_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && !value.chars().any(char::is_control)
        && value.bytes().all(|byte| byte.is_ascii_graphic())
}

fn valid_reason_code(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn valid_api_version(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 32
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || byte == b'/'
                || byte == b'.'
                || byte == b'-'
        })
}

fn valid_resource_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !value.starts_with('-')
        && !value.ends_with('-')
}
