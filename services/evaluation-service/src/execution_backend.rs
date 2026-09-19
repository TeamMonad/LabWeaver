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
