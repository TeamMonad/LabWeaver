//! Pure, stateless evaluation naming helpers extracted from `control_plane`.
//!
//! These functions convert evaluation domain types to their stable string
//! representations used in SQL, logs, and event payloads.

use contracts::evaluation::{EvaluationRunState, EvaluationStepFailurePolicy, EvaluationStepRole, EvaluationStepRunState};

/// Returns the canonical name for an evaluation run state.
#[must_use]
pub const fn run_state_name(state: EvaluationRunState) -> &'static str {
    match state {
        EvaluationRunState::Queued => "queued",
        EvaluationRunState::Running => "running",
        EvaluationRunState::Cancelling => "cancelling",
        EvaluationRunState::Succeeded => "succeeded",
        EvaluationRunState::Failed => "failed",
        EvaluationRunState::Cancelled => "cancelled",
    }
}

/// Returns the canonical name for an evaluation step run state.
#[must_use]
pub const fn step_state_name(state: EvaluationStepRunState) -> &'static str {
    match state {
        EvaluationStepRunState::Pending => "pending",
        EvaluationStepRunState::Running => "running",
        EvaluationStepRunState::Retryable => "retryable",
        EvaluationStepRunState::Succeeded => "succeeded",
        EvaluationStepRunState::Failed => "failed",
        EvaluationStepRunState::Cancelled => "cancelled",
        EvaluationStepRunState::Skipped => "skipped",
    }
}

/// Returns the canonical name for an evaluation step role.
#[must_use]
pub const fn step_role_name(role: EvaluationStepRole) -> &'static str {
    match role {
        EvaluationStepRole::Gate => "gate",
        EvaluationStepRole::Score => "score",
        EvaluationStepRole::Advisory => "advisory",
    }
}

/// Returns the canonical name for an evaluation step failure policy.
#[must_use]
pub const fn step_failure_policy_name(policy: EvaluationStepFailurePolicy) -> &'static str {
    match policy {
        EvaluationStepFailurePolicy::Stop => "stop",
        EvaluationStepFailurePolicy::Continue => "continue",
        EvaluationStepFailurePolicy::ContinueAdvisory => "continue_advisory",
    }
}
