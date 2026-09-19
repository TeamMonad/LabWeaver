//! Actual Kubernetes main-container timing observed from Pod status.
//!
//! Kubernetes may omit either boundary while a Job is being deleted or when the kubelet did not
//! publish a complete status. Callers must preserve that uncertainty as an unknown usage
//! measurement instead of inventing an interval.

use contracts::UtcTimestamp;
use thiserror::Error;

/// Actual main-container timing of one observed workload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExecutionTiming {
    pub started_at: Option<UtcTimestamp>,
    pub terminated_at: Option<UtcTimestamp>,
}

/// Rejected execution timing that is not a valid half-open interval.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum ExecutionTimingError {
    #[error("execution timing is not a valid interval")]
    Invalid,
}

impl ExecutionTiming {
    #[must_use]
    pub const fn unknown() -> Self {
        Self {
            started_at: None,
            terminated_at: None,
        }
    }

    /// Validates that both boundaries are either absent or a strict interval.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutionTimingError::Invalid`] when only one boundary exists or the interval
    /// is not strictly increasing.
    pub fn validate(self) -> Result<(), ExecutionTimingError> {
        if self.started_at.is_some() != self.terminated_at.is_some()
            || self
                .started_at
                .zip(self.terminated_at)
                .is_some_and(|(started, terminated)| terminated <= started)
        {
            return Err(ExecutionTimingError::Invalid);
        }
        Ok(())
    }
}
