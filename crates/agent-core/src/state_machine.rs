use serde::{Deserialize, Serialize};
use thiserror::Error;

const MAX_AUTOMATIC_REPAIRS: u8 = 2;

/// Explicit states of one candidate-generating Agent run.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    /// Parse untrusted teacher material into bounded inputs.
    Parse,
    /// Read explicitly exposed runtime and tool capabilities.
    RetrieveCapabilities,
    /// Build a structured candidate plan.
    Plan,
    /// Generate candidate artifacts.
    Generate,
    /// Validate candidate structure against versioned schemas.
    SchemaValidate,
    /// Apply deterministic policy validation.
    PolicyValidate,
    /// Execute only approved deterministic validation tools.
    ExecuteValidation,
    /// Verify deterministic evidence.
    Verify,
    /// Repair a rejected candidate within the fixed retry budget.
    Repair,
    /// Wait for approval before executing elevated deterministic validation.
    AwaitingExecutionApproval,
    /// Wait for final teacher approval after deterministic verification.
    AwaitingReleaseApproval,
    /// Agent work is complete and an approved candidate was handed off.
    Completed,
    /// Candidate generation failed permanently.
    Failed,
    /// The run was explicitly cancelled.
    Cancelled,
}

impl AgentState {
    /// Reports whether no further Agent transition is allowed.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

/// Events accepted by the explicit Agent state machine.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentEvent {
    /// Untrusted input parsing completed.
    Parsed,
    /// Required capability retrieval completed.
    CapabilitiesRetrieved,
    /// A structured plan was created.
    PlanCreated,
    /// Candidate generation completed.
    CandidateGenerated,
    /// Candidate schema validation passed.
    SchemaValid,
    /// Candidate schema validation failed and requires repair.
    SchemaInvalid,
    /// Deterministic policy validation allowed execution.
    PolicyAllowed,
    /// Policy requires teacher approval before execution.
    ApprovalRequired,
    /// Deterministic validation tools completed.
    ValidationExecuted,
    /// Deterministic evidence verification passed.
    VerificationPassed,
    /// Deterministic evidence verification failed and requires repair.
    VerificationFailed,
    /// A repaired plan is ready for another generation attempt.
    RepairReady,
    /// Teacher approval was recorded by the authoritative approval owner.
    TeacherApproved,
    /// Teacher rejected the candidate and requested repair.
    TeacherRejected,
    /// Cancel the run without publishing a candidate.
    Cancel,
    /// Record a fail-fast root cause through `AgentRun::apply_failure`.
    FailureRecorded,
}

/// Observable result attached to each accepted transition.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionOutcome {
    /// The state machine advanced normally.
    Advanced,
    /// The automatic repair budget was exhausted and the run failed.
    RepairBudgetExhausted,
    /// An approved candidate was handed to the external publication owner.
    ApprovalHandedOff,
    /// The run was cancelled.
    Cancelled,
    /// A stage reported a fatal root-cause diagnostic.
    FailedFast,
}

impl TransitionOutcome {
    /// Returns an optional stable outcome diagnostic.
    #[must_use]
    pub const fn diagnostic_code(self) -> Option<&'static str> {
        match self {
            Self::Advanced | Self::ApprovalHandedOff | Self::FailedFast => None,
            Self::RepairBudgetExhausted => Some("LW_AGENT_REPAIR_BUDGET_EXHAUSTED"),
            Self::Cancelled => Some("LW_AGENT_RUN_CANCELLED"),
        }
    }
}

/// Immutable evidence for one accepted state transition.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TransitionRecord {
    sequence: u64,
    from: AgentState,
    event: AgentEvent,
    to: AgentState,
    outcome: TransitionOutcome,
    diagnostic_code: Option<String>,
}

impl TransitionRecord {
    /// Returns the monotonic transition sequence.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Returns the previous state.
    #[must_use]
    pub const fn from(&self) -> AgentState {
        self.from
    }

    /// Returns the applied event.
    #[must_use]
    pub const fn event(&self) -> AgentEvent {
        self.event
    }

    /// Returns the resulting state.
    #[must_use]
    pub const fn to(&self) -> AgentState {
        self.to
    }

    /// Returns the observable transition outcome.
    #[must_use]
    pub const fn outcome(&self) -> TransitionOutcome {
        self.outcome
    }

    /// Returns the stable diagnostic attached to a failure or cancellation outcome.
    #[must_use]
    pub fn diagnostic_code(&self) -> Option<&str> {
        self.diagnostic_code.as_deref()
    }
}

/// Validated state and transition history for one Agent run.
#[derive(Clone, Debug, Serialize)]
pub struct AgentRun {
    run_id: String,
    state: AgentState,
    automatic_repairs: u8,
    history: Vec<TransitionRecord>,
}

impl AgentRun {
    /// Creates an Agent run in the Parse state.
    ///
    /// # Errors
    ///
    /// Returns a stable diagnostic when the run identifier is empty.
    pub fn new(run_id: impl Into<String>) -> Result<Self, AgentStateError> {
        let run_id = run_id.into();
        if run_id.trim().is_empty() {
            return Err(AgentStateError::InvalidRunId);
        }
        Ok(Self {
            run_id,
            state: AgentState::Parse,
            automatic_repairs: 0,
            history: Vec::new(),
        })
    }

    /// Returns the stable run identifier.
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Returns the current state.
    #[must_use]
    pub const fn state(&self) -> AgentState {
        self.state
    }

    /// Returns the number of automatic repairs already consumed.
    #[must_use]
    pub const fn automatic_repairs(&self) -> u8 {
        self.automatic_repairs
    }

    /// Returns immutable transition evidence.
    #[must_use]
    pub fn history(&self) -> &[TransitionRecord] {
        &self.history
    }

    /// Applies one event atomically.
    ///
    /// Invalid events return an error without changing state, repair counters, or history.
    ///
    /// # Errors
    ///
    /// Returns a stable diagnostic for illegal transitions or exhausted sequence space.
    pub fn apply(&mut self, event: AgentEvent) -> Result<TransitionRecord, AgentStateError> {
        let (next_state, next_repairs, outcome) = self.next(event)?;
        self.commit_transition(
            event,
            next_state,
            next_repairs,
            outcome,
            outcome.diagnostic_code().map(str::to_owned),
        )
    }

    /// Records a fatal stage failure and atomically enters `Failed`.
    ///
    /// The caller must preserve the stable diagnostic from the owning boundary, for example
    /// `LW_AGENT_TOOL_EXECUTION_FAILED`. Any non-terminal state can fail fast through this method.
    ///
    /// # Errors
    ///
    /// Rejects malformed diagnostic codes, terminal runs, or exhausted sequence space without
    /// modifying the run.
    pub fn apply_failure(
        &mut self,
        diagnostic_code: impl Into<String>,
    ) -> Result<TransitionRecord, AgentStateError> {
        let diagnostic_code = diagnostic_code.into();
        if !is_stable_diagnostic(&diagnostic_code) {
            return Err(AgentStateError::InvalidFailureDiagnostic { diagnostic_code });
        }
        if self.state.is_terminal() {
            return Err(AgentStateError::InvalidTransition {
                from: self.state,
                event: AgentEvent::FailureRecorded,
            });
        }
        self.commit_transition(
            AgentEvent::FailureRecorded,
            AgentState::Failed,
            self.automatic_repairs,
            TransitionOutcome::FailedFast,
            Some(diagnostic_code),
        )
    }

    fn commit_transition(
        &mut self,
        event: AgentEvent,
        next_state: AgentState,
        next_repairs: u8,
        outcome: TransitionOutcome,
        diagnostic_code: Option<String>,
    ) -> Result<TransitionRecord, AgentStateError> {
        let sequence = u64::try_from(self.history.len())
            .ok()
            .and_then(|value| value.checked_add(1))
            .ok_or(AgentStateError::TransitionSequenceOverflow)?;
        let record = TransitionRecord {
            sequence,
            from: self.state,
            event,
            to: next_state,
            outcome,
            diagnostic_code,
        };
        self.history.push(record.clone());
        self.state = next_state;
        self.automatic_repairs = next_repairs;
        Ok(record)
    }

    fn next(
        &self,
        event: AgentEvent,
    ) -> Result<(AgentState, u8, TransitionOutcome), AgentStateError> {
        if event == AgentEvent::Cancel && !self.state.is_terminal() {
            return Ok((
                AgentState::Cancelled,
                self.automatic_repairs,
                TransitionOutcome::Cancelled,
            ));
        }

        let advanced = |state| Ok((state, self.automatic_repairs, TransitionOutcome::Advanced));
        match (self.state, event) {
            (AgentState::Parse, AgentEvent::Parsed) => advanced(AgentState::RetrieveCapabilities),
            (AgentState::RetrieveCapabilities, AgentEvent::CapabilitiesRetrieved) => {
                advanced(AgentState::Plan)
            }
            (AgentState::Plan, AgentEvent::PlanCreated) => advanced(AgentState::Generate),
            (AgentState::Generate, AgentEvent::CandidateGenerated) => {
                advanced(AgentState::SchemaValidate)
            }
            (AgentState::SchemaValidate, AgentEvent::SchemaValid) => {
                advanced(AgentState::PolicyValidate)
            }
            (AgentState::SchemaValidate, AgentEvent::SchemaInvalid)
            | (AgentState::Verify, AgentEvent::VerificationFailed)
            | (
                AgentState::AwaitingExecutionApproval | AgentState::AwaitingReleaseApproval,
                AgentEvent::TeacherRejected,
            ) => advanced(AgentState::Repair),
            (AgentState::PolicyValidate, AgentEvent::PolicyAllowed)
            | (AgentState::AwaitingExecutionApproval, AgentEvent::TeacherApproved) => {
                advanced(AgentState::ExecuteValidation)
            }
            (AgentState::PolicyValidate, AgentEvent::ApprovalRequired) => {
                advanced(AgentState::AwaitingExecutionApproval)
            }
            (AgentState::Verify, AgentEvent::VerificationPassed) => {
                advanced(AgentState::AwaitingReleaseApproval)
            }
            (AgentState::ExecuteValidation, AgentEvent::ValidationExecuted) => {
                advanced(AgentState::Verify)
            }
            (AgentState::Repair, AgentEvent::RepairReady)
                if self.automatic_repairs < MAX_AUTOMATIC_REPAIRS =>
            {
                Ok((
                    AgentState::Generate,
                    self.automatic_repairs + 1,
                    TransitionOutcome::Advanced,
                ))
            }
            (AgentState::Repair, AgentEvent::RepairReady) => Ok((
                AgentState::Failed,
                self.automatic_repairs,
                TransitionOutcome::RepairBudgetExhausted,
            )),
            (AgentState::AwaitingReleaseApproval, AgentEvent::TeacherApproved) => Ok((
                AgentState::Completed,
                self.automatic_repairs,
                TransitionOutcome::ApprovalHandedOff,
            )),
            _ => Err(AgentStateError::InvalidTransition {
                from: self.state,
                event,
            }),
        }
    }
}

/// Fail-fast diagnostics for Agent state transitions.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum AgentStateError {
    /// Run identifiers must be stable and non-empty.
    #[error("Agent run id is required")]
    InvalidRunId,
    /// The event is not valid in the current state.
    #[error("invalid Agent transition: {from:?} + {event:?}")]
    InvalidTransition {
        /// Current state.
        from: AgentState,
        /// Rejected event.
        event: AgentEvent,
    },
    /// Transition evidence can no longer be represented safely.
    #[error("Agent transition sequence overflow")]
    TransitionSequenceOverflow,
    /// Root-cause diagnostic is empty or outside the stable `LW_*` format.
    #[error("invalid Agent failure diagnostic: {diagnostic_code}")]
    InvalidFailureDiagnostic {
        /// Rejected diagnostic.
        diagnostic_code: String,
    },
}

impl AgentStateError {
    /// Returns the stable machine-readable diagnostic code.
    #[must_use]
    pub const fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::InvalidRunId => "LW_AGENT_RUN_ID_INVALID",
            Self::InvalidTransition { .. } => "LW_AGENT_TRANSITION_INVALID",
            Self::TransitionSequenceOverflow => "LW_AGENT_TRANSITION_SEQUENCE_OVERFLOW",
            Self::InvalidFailureDiagnostic { .. } => "LW_AGENT_FAILURE_DIAGNOSTIC_INVALID",
        }
    }
}

fn is_stable_diagnostic(value: &str) -> bool {
    value.strip_prefix("LW_").is_some_and(|suffix| {
        !suffix.is_empty()
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    })
}
