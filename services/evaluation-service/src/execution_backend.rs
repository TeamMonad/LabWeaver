//! One-shot execution admission witness and backend boundary helpers.
//!
//! Business services keep their own task state; this module only proves that one execution
//! generation holds an admitted Resource binding and exposes the small conversion helpers shared
//! by the Kubernetes execution backends.
#![allow(missing_docs, clippy::missing_errors_doc)]

use contracts::DiagnosticCode;
use contracts::execution::{
    ExecutionCleanupStatus, ExecutionContractError, ExecutionObservation, TaskExecutionBinding,
};
use contracts::http::TaskResourceStatus;
use thiserror::Error;

use crate::execution::ExecutionTiming;

/// Failure returned when an execution generation cannot prove its admission.
#[derive(Clone, Debug, Eq, PartialEq, Error)]
pub enum ExecutionAdmissionError {
    #[error("task execution admission state mismatch")]
    StateMismatch,
    #[error("invalid task execution binding")]
    InvalidBinding,
}

/// Proof that one execution generation holds an admitted Resource binding.
///
/// The binding field is private so a backend cannot receive an execution identity that was not
/// created from an acknowledged `TaskResourceStatus` or restored from a persisted checkpoint that
/// still names the same reservation.
#[derive(Clone, Debug)]
pub struct AdmittedExecution {
    binding: TaskExecutionBinding,
}

impl AdmittedExecution {
    /// Admits a fresh execution generation from an authoritative Resource status.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutionAdmissionError::StateMismatch`] unless the status is active, handed off,
    /// and bound to the exact namespace and revisions, or
    /// [`ExecutionAdmissionError::InvalidBinding`] for malformed identity values.
    pub fn admit(
        status: &TaskResourceStatus,
        generation: u64,
        workload_name: impl Into<String>,
        trace_id: impl Into<String>,
    ) -> Result<Self, ExecutionAdmissionError> {
        TaskExecutionBinding::from_admitted_status(status, generation, workload_name, trace_id)
            .map(|binding| Self { binding })
            .map_err(|error| match error {
                ExecutionContractError::AdmissionStateMismatch => {
                    ExecutionAdmissionError::StateMismatch
                }
                _ => ExecutionAdmissionError::InvalidBinding,
            })
    }

    /// Restores a persisted execution generation and proves it still names the same reservation.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutionAdmissionError::StateMismatch`] when the persisted binding no longer
    /// matches the authoritative reservation or generation.
    pub fn recover(
        binding: TaskExecutionBinding,
        status: &TaskResourceStatus,
        generation: u64,
    ) -> Result<Self, ExecutionAdmissionError> {
        binding
            .validate()
            .map_err(|_| ExecutionAdmissionError::InvalidBinding)?;
        if binding.execution_generation != generation || !binding.same_reservation(status) {
            return Err(ExecutionAdmissionError::StateMismatch);
        }
        Ok(Self { binding })
    }

    /// Returns the admitted binding.
    #[must_use]
    pub const fn binding(&self) -> &TaskExecutionBinding {
        &self.binding
    }
}

/// Returns the observed main-container timing of one execution observation.
#[must_use]
pub const fn observation_timing(observation: &ExecutionObservation) -> ExecutionTiming {
    ExecutionTiming {
        started_at: observation.started_at,
        terminated_at: observation.terminated_at,
    }
}

/// Builds the unresolved cleanup status for a bounded diagnostic.
///
/// Unknown cleanup never becomes [`ExecutionCleanupStatus::Confirmed`] and therefore never
/// releases Resource.
#[must_use]
pub fn cleanup_unknown(diagnostic: &str) -> ExecutionCleanupStatus {
    let diagnostic = DiagnosticCode::parse(diagnostic.to_owned())
        .unwrap_or_else(|_| DiagnosticCode::registered("LW_EXECUTION_CLEANUP_UNKNOWN"));
    ExecutionCleanupStatus::Unknown { diagnostic }
}

#[cfg(test)]
mod tests {
    use contracts::execution::{
        ExecutionCleanupStatus, ExecutionObservation, ExecutionWorkloadState, TaskExecutionBinding,
    };
    use contracts::http::TaskResourceStatus;
    use contracts::resource::{
        CapacityClaim, CapacityClaimState, ResourceLease, ResourceLeaseState, ResourceRequest,
        ResourceRequestState, ResourceTarget, WorkloadResources,
    };
    use contracts::{
        ActorId, CapacityClaimId, LeaseId, ProjectId, ResourceApprovalId, ResourceRequestId,
        Revision, TaskRunId, UtcTimestamp,
    };

    use super::{AdmittedExecution, ExecutionAdmissionError, cleanup_unknown, observation_timing};

    fn timestamp(value: &str) -> Result<UtcTimestamp, Box<dyn std::error::Error>> {
        Ok(value.parse()?)
    }

    fn resources() -> WorkloadResources {
        WorkloadResources {
            cpu_millicores: 1_000,
            memory_bytes: 1 << 30,
            storage_bytes: 1 << 30,
            gpu: None,
        }
    }

    fn admitted_status() -> Result<(TaskResourceStatus, TaskRunId), Box<dyn std::error::Error>> {
        let task_run_id = TaskRunId::new();
        let request_id = ResourceRequestId::new();
        let claim_id = CapacityClaimId::new();
        let lease_id = LeaseId::new();
        let project_id = ProjectId::new();
        let owner_id = ActorId::new();
        let created_at = timestamp("2026-09-19T07:00:00.000Z")?;
        let expires_at = timestamp("2026-09-19T08:00:00.000Z")?;
        let revision = Revision::new(2)?;
        let status = TaskResourceStatus {
            task_run_id,
            project_id,
            owner_id,
            execution_namespace: Some("labweaver-evaluation".to_owned()),
            claim_revision: revision,
            lease_revision: revision,
            cleanup_confirmed: false,
            request: ResourceRequest {
                id: request_id,
                generation: 1,
                request_key: format!("evaluation-{task_run_id}"),
                requester_id: owner_id,
                project_id,
                course_id: None,
                target: ResourceTarget::Task { task_run_id },
                requested_resources: resources(),
                requested_duration_seconds: 600,
                state: ResourceRequestState::Active,
                revision,
                created_at,
                updated_at: created_at,
                diagnostic_code: None,
            },
            claim: CapacityClaim {
                id: claim_id,
                request_id,
                approval_id: ResourceApprovalId::new(),
                provider_binding: "kubernetes-job".to_owned(),
                workload_resources: resources(),
                quota_resources: resources(),
                gpu_allocation: None,
                state: CapacityClaimState::HandedOff,
                revision,
            },
            lease: ResourceLease {
                id: lease_id,
                request_id,
                claim_id,
                state: ResourceLeaseState::Active,
                revision,
                active_from: Some(created_at),
                expires_at: Some(expires_at),
                revoke_reason_code: None,
                created_at,
                updated_at: created_at,
            },
        };
        Ok((status, task_run_id))
    }

    #[test]
    fn admit_requires_an_active_handoff() -> Result<(), Box<dyn std::error::Error>> {
        let (status, _) = admitted_status()?;
        let admitted = AdmittedExecution::admit(&status, 1, "lw-oj-0123456789abcdef0123", "trace")?;
        assert_eq!(admitted.binding().task_run_id, status.task_run_id);
        assert_eq!(admitted.binding().execution_generation, 1);

        let mut reviewing = status.clone();
        reviewing.request.state = ResourceRequestState::Reviewing;
        assert!(matches!(
            AdmittedExecution::admit(&reviewing, 1, "lw-oj-1", "trace"),
            Err(ExecutionAdmissionError::StateMismatch)
        ));

        let mut blocked = status;
        blocked.claim.state = CapacityClaimState::Blocked;
        assert!(matches!(
            AdmittedExecution::admit(&blocked, 1, "lw-oj-1", "trace"),
            Err(ExecutionAdmissionError::StateMismatch)
        ));
        Ok(())
    }

    #[test]
    fn recover_rejects_stale_generation_and_other_reservations()
    -> Result<(), Box<dyn std::error::Error>> {
        let (status, _) = admitted_status()?;
        let admitted = AdmittedExecution::admit(&status, 1, "lw-oj-0123456789abcdef0123", "trace")?;
        let binding = admitted.binding().clone();

        assert!(
            AdmittedExecution::recover(binding.clone(), &status, 1).is_ok(),
            "same reservation and generation restores"
        );
        assert!(matches!(
            AdmittedExecution::recover(binding.clone(), &status, 2),
            Err(ExecutionAdmissionError::StateMismatch)
        ));

        let (other_status, _) = admitted_status()?;
        assert!(matches!(
            AdmittedExecution::recover(binding, &other_status, 1),
            Err(ExecutionAdmissionError::StateMismatch)
        ));
        Ok(())
    }

    #[test]
    fn unknown_cleanup_is_never_confirmed() {
        let status = cleanup_unknown("LW_OJ_CLEANUP_UNKNOWN");
        assert!(!status.is_confirmed());
        assert!(matches!(status, ExecutionCleanupStatus::Unknown { .. }));

        let invalid = cleanup_unknown("not a diagnostic");
        assert!(!invalid.is_confirmed());
    }

    #[test]
    fn observation_timing_round_trips() -> Result<(), Box<dyn std::error::Error>> {
        let observation = ExecutionObservation {
            state: ExecutionWorkloadState::Succeeded,
            exit_code: Some(0),
            reason_code: Some("Completed".to_owned()),
            pod_name: None,
            started_at: Some(timestamp("2026-09-19T07:00:01.000Z")?),
            terminated_at: Some(timestamp("2026-09-19T07:00:02.000Z")?),
        };
        let timing = observation_timing(&observation);
        assert_eq!(timing.started_at, observation.started_at);
        assert_eq!(timing.terminated_at, observation.terminated_at);
        Ok(())
    }

    #[test]
    fn witness_hands_out_the_contract_binding() -> Result<(), Box<dyn std::error::Error>> {
        let (status, _) = admitted_status()?;
        let admitted = AdmittedExecution::admit(&status, 1, "lw-oj-0123456789abcdef0123", "trace")?;
        // Compile-time proof that the witness hands out the exact contract binding.
        let binding: &TaskExecutionBinding = admitted.binding();
        assert_eq!(binding.task_run_id, status.task_run_id);
        Ok(())
    }
}
