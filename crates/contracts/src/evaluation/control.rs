use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    ActorId, ApprovalId, CandidateId, CourseId, DiagnosticCode, EvaluationReleaseId,
    EvaluationRunId, EvaluationStepRunId, FrozenSubmissionId, Revision, UtcTimestamp,
};

use super::EvaluationSpec;

/// Versioned schema identity for `EvaluationRelease` projections.
pub const EVALUATION_RELEASE_SCHEMA_VERSION: &str = "evaluation.labweaver.io/evaluation-release/v1";
/// Versioned schema identity for `EvaluationRun` projections.
pub const EVALUATION_RUN_SCHEMA_VERSION: &str = "evaluation.labweaver.io/evaluation-run/v1";

/// Immutable build and deployment identity that must match every run using the release.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvaluationRuntimeIdentity {
    /// Explicit runtime provider binding chosen by Control and enforced by Evaluation.
    pub provider_binding: String,
    /// Digest-pinned runner image reference.
    pub runner_image: String,
}

impl EvaluationRuntimeIdentity {
    /// Validates bounded, immutable, non-secret runtime bindings.
    pub fn validate(&self) -> Result<(), EvaluationControlContractError> {
        validate_token(&self.provider_binding, 128)?;
        if !is_digest_pinned_image(&self.runner_image) {
            return Err(EvaluationControlContractError::RuntimeIdentityInvalid);
        }
        Ok(())
    }
}

/// Immutable release of one validated `EvaluationSpec`.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvaluationRelease {
    pub schema_version: String,
    pub id: EvaluationReleaseId,
    pub course_id: CourseId,
    pub candidate_id: CandidateId,
    pub candidate_revision: Revision,
    pub approval_id: ApprovalId,
    pub approval_revision: Revision,
    pub evaluation_spec: EvaluationSpec,
    pub runtime_identity: EvaluationRuntimeIdentity,
    pub state: EvaluationReleaseState,
    pub revision: Revision,
    pub published_by: ActorId,
    pub published_at: UtcTimestamp,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub withdrawn_at: Option<UtcTimestamp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub withdrawal_diagnostic_code: Option<DiagnosticCode>,
}

impl EvaluationRelease {
    /// Validates release/spec identity and withdrawal consistency.
    pub fn validate(&self) -> Result<(), EvaluationControlContractError> {
        if self.schema_version != EVALUATION_RELEASE_SCHEMA_VERSION {
            return Err(EvaluationControlContractError::SchemaVersionInvalid);
        }
        self.evaluation_spec
            .validate()
            .map_err(|_| EvaluationControlContractError::SpecInvalid)?;
        self.runtime_identity.validate()?;
        match self.state {
            EvaluationReleaseState::Active
                if self.withdrawn_at.is_none() && self.withdrawal_diagnostic_code.is_none() =>
            {
                Ok(())
            }
            EvaluationReleaseState::Withdrawn
                if self.withdrawn_at.is_some() && self.withdrawal_diagnostic_code.is_some() =>
            {
                Ok(())
            }
            EvaluationReleaseState::Active | EvaluationReleaseState::Withdrawn => {
                Err(EvaluationControlContractError::TerminalStateInvalid)
            }
        }
    }
}

/// Public release lifecycle.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationReleaseState {
    Active,
    Withdrawn,
}

/// Immutable run identity joining release, frozen submission and trace evidence.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvaluationRunIdentity {
    pub runtime_identity: EvaluationRuntimeIdentity,
    pub trace_id: String,
}

impl EvaluationRunIdentity {
    /// Validates the non-secret run identity closure.
    pub fn validate(&self) -> Result<(), EvaluationControlContractError> {
        self.runtime_identity.validate()?;
        validate_token(&self.trace_id, 128)
    }
}

/// PostgreSQL-authoritative state for one evaluation run.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvaluationRun {
    pub schema_version: String,
    pub id: EvaluationRunId,
    pub course_id: CourseId,
    pub release_id: EvaluationReleaseId,
    pub release_revision: Revision,
    pub frozen_submission_id: FrozenSubmissionId,
    pub actor_id: ActorId,
    pub state: EvaluationRunState,
    pub revision: Revision,
    pub identity: EvaluationRunIdentity,
    pub max_score: u32,
    pub awarded_score: u32,
    pub steps: Vec<EvaluationStepRun>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic_code: Option<DiagnosticCode>,
    pub cancellation_requested: bool,
    pub cleanup_verified: bool,
    pub created_at: UtcTimestamp,
    pub updated_at: UtcTimestamp,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<UtcTimestamp>,
}

/// Privacy-preserving terminal result exposed to the owning student.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StudentEvaluationResult {
    pub run_id: EvaluationRunId,
    pub course_id: CourseId,
    pub release_id: EvaluationReleaseId,
    pub frozen_submission_id: FrozenSubmissionId,
    pub state: StudentEvaluationResultState,
    pub revision: Revision,
    pub max_score: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub awarded_score: Option<u32>,
    pub steps: Vec<StudentEvaluationStepResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic_code: Option<DiagnosticCode>,
    pub created_at: UtcTimestamp,
    pub updated_at: UtcTimestamp,
    pub completed_at: UtcTimestamp,
}

/// Bounded step projection that deliberately omits private step identifiers and evidence.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StudentEvaluationStepResult {
    #[schemars(extend("minimum" = 1))]
    pub position: u32,
    pub role: EvaluationStepRole,
    pub state: EvaluationStepRunState,
    pub max_score: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub awarded_score: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic_code: Option<DiagnosticCode>,
}

impl StudentEvaluationResult {
    /// Creates a terminal, owner-safe projection. In-progress and malformed runs are rejected.
    pub fn from_terminal(run: &EvaluationRun) -> Result<Self, EvaluationControlContractError> {
        run.validate()?;
        let state = StudentEvaluationResultState::try_from(run.state)?;
        let completed_at = run
            .completed_at
            .ok_or(EvaluationControlContractError::TerminalStateInvalid)?;
        Ok(Self {
            run_id: run.id,
            course_id: run.course_id,
            release_id: run.release_id,
            frozen_submission_id: run.frozen_submission_id,
            state,
            revision: run.revision,
            max_score: run.max_score,
            awarded_score: (run.state == EvaluationRunState::Succeeded)
                .then_some(run.awarded_score),
            steps: run
                .steps
                .iter()
                .map(|step| StudentEvaluationStepResult {
                    position: step.position,
                    role: step.role,
                    state: step.state,
                    max_score: step.max_score,
                    awarded_score: (run.state == EvaluationRunState::Succeeded)
                        .then_some(step.awarded_score)
                        .flatten(),
                    diagnostic_code: step.diagnostic_code.clone(),
                })
                .collect(),
            diagnostic_code: run.diagnostic_code.clone(),
            created_at: run.created_at,
            updated_at: run.updated_at,
            completed_at,
        })
    }
}

/// Terminal-only lifecycle exposed by the student result projection.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StudentEvaluationResultState {
    Succeeded,
    Failed,
    Cancelled,
}

impl TryFrom<EvaluationRunState> for StudentEvaluationResultState {
    type Error = EvaluationControlContractError;

    fn try_from(state: EvaluationRunState) -> Result<Self, Self::Error> {
        match state {
            EvaluationRunState::Succeeded => Ok(Self::Succeeded),
            EvaluationRunState::Failed => Ok(Self::Failed),
            EvaluationRunState::Cancelled => Ok(Self::Cancelled),
            EvaluationRunState::Queued
            | EvaluationRunState::Running
            | EvaluationRunState::Cancelling => {
                Err(EvaluationControlContractError::TerminalStateInvalid)
            }
        }
    }
}

impl EvaluationRun {
    /// Validates aggregate, step, and terminal-state consistency.
    pub fn validate(&self) -> Result<(), EvaluationControlContractError> {
        if self.schema_version != EVALUATION_RUN_SCHEMA_VERSION {
            return Err(EvaluationControlContractError::SchemaVersionInvalid);
        }
        self.identity.validate()?;
        if self.awarded_score > self.max_score || self.steps.is_empty() {
            return Err(EvaluationControlContractError::ScoreInvalid);
        }
        let mut step_ids = std::collections::BTreeSet::new();
        let mut step_run_ids = std::collections::BTreeSet::new();
        let mut summed_max = 0_u32;
        let mut summed_awarded = 0_u32;
        for step in &self.steps {
            step.validate(self.id)?;
            if !step_ids.insert(step.step_id.as_str()) || !step_run_ids.insert(step.id) {
                return Err(EvaluationControlContractError::IdentityMismatch);
            }
            if step.role == EvaluationStepRole::Score {
                summed_max = summed_max
                    .checked_add(step.max_score)
                    .ok_or(EvaluationControlContractError::ScoreInvalid)?;
                summed_awarded = summed_awarded
                    .checked_add(step.awarded_score.unwrap_or(0))
                    .ok_or(EvaluationControlContractError::ScoreInvalid)?;
            }
        }
        if summed_max != self.max_score || summed_awarded != self.awarded_score {
            return Err(EvaluationControlContractError::ScoreInvalid);
        }
        match self.state {
            EvaluationRunState::Queued
            | EvaluationRunState::Running
            | EvaluationRunState::Cancelling
                if self.completed_at.is_none() =>
            {
                Ok(())
            }
            EvaluationRunState::Succeeded
                if self.completed_at.is_some()
                    && self.diagnostic_code.is_none()
                    && self.cleanup_verified =>
            {
                Ok(())
            }
            EvaluationRunState::Failed | EvaluationRunState::Cancelled
                if self.diagnostic_code.is_some()
                    && ((self.cleanup_verified && self.completed_at.is_some())
                        || (!self.cleanup_verified && self.completed_at.is_none())) =>
            {
                Ok(())
            }
            EvaluationRunState::Queued
            | EvaluationRunState::Running
            | EvaluationRunState::Cancelling
            | EvaluationRunState::Succeeded
            | EvaluationRunState::Failed
            | EvaluationRunState::Cancelled => {
                Err(EvaluationControlContractError::TerminalStateInvalid)
            }
        }
    }
}

/// Public run lifecycle.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationRunState {
    Queued,
    Running,
    Cancelling,
    Succeeded,
    Failed,
    Cancelled,
}

/// PostgreSQL-authoritative state for one declared evaluation step.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvaluationStepRun {
    pub id: EvaluationStepRunId,
    pub run_id: EvaluationRunId,
    pub step_id: String,
    #[schemars(extend("minimum" = 1))]
    pub position: u32,
    pub role: EvaluationStepRole,
    pub failure_policy: EvaluationStepFailurePolicy,
    pub depends_on: Vec<String>,
    pub state: EvaluationStepRunState,
    pub revision: Revision,
    pub current_attempt: u32,
    pub max_score: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub awarded_score: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic_code: Option<DiagnosticCode>,
    pub cleanup_verified: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<UtcTimestamp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<UtcTimestamp>,
}

impl EvaluationStepRun {
    /// Validates one step state without reading the release spec.
    pub fn validate(&self, run_id: EvaluationRunId) -> Result<(), EvaluationControlContractError> {
        if self.run_id != run_id
            || self.step_id.trim().is_empty()
            || self.step_id.len() > 96
            || self.step_id.chars().any(char::is_control)
            || self.position == 0
        {
            return Err(EvaluationControlContractError::IdentityMismatch);
        }
        for dependency in &self.depends_on {
            validate_token(dependency, 96)?;
        }
        if self.role != EvaluationStepRole::Score && self.max_score != 0 {
            return Err(EvaluationControlContractError::ScoreInvalid);
        }
        match (self.role, self.failure_policy) {
            (
                EvaluationStepRole::Gate | EvaluationStepRole::Score,
                EvaluationStepFailurePolicy::Stop,
            )
            | (EvaluationStepRole::Score, EvaluationStepFailurePolicy::Continue)
            | (EvaluationStepRole::Advisory, EvaluationStepFailurePolicy::ContinueAdvisory) => {}
            _ => return Err(EvaluationControlContractError::TerminalStateInvalid),
        }
        if self
            .awarded_score
            .is_some_and(|score| score > self.max_score)
            || (self.role != EvaluationStepRole::Score && self.awarded_score.is_some())
        {
            return Err(EvaluationControlContractError::ScoreInvalid);
        }
        match self.state {
            EvaluationStepRunState::Pending
            | EvaluationStepRunState::Retryable
            | EvaluationStepRunState::Running
                if self.completed_at.is_none() =>
            {
                Ok(())
            }
            EvaluationStepRunState::Succeeded
                if self.completed_at.is_some()
                    && self.diagnostic_code.is_none()
                    && self.cleanup_verified
                    && ((self.role == EvaluationStepRole::Score
                        && self.awarded_score.is_some())
                        || (self.role != EvaluationStepRole::Score
                            && self.awarded_score.is_none())) =>
            {
                Ok(())
            }
            EvaluationStepRunState::Failed
            | EvaluationStepRunState::Cancelled
            | EvaluationStepRunState::Skipped
                if self.completed_at.is_some()
                    && self.diagnostic_code.is_some()
                    && self.awarded_score.is_none() =>
            {
                Ok(())
            }
            EvaluationStepRunState::Pending
            | EvaluationStepRunState::Retryable
            | EvaluationStepRunState::Running
            | EvaluationStepRunState::Succeeded
            | EvaluationStepRunState::Failed
            | EvaluationStepRunState::Cancelled
            | EvaluationStepRunState::Skipped => {
                Err(EvaluationControlContractError::TerminalStateInvalid)
            }
        }
    }
}

/// Stable role copied from the immutable `EvaluationSpec` step.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationStepRole {
    Gate,
    Score,
    Advisory,
}

/// Stable failure behavior copied from the immutable `EvaluationSpec` step.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationStepFailurePolicy {
    Stop,
    Continue,
    ContinueAdvisory,
}

/// Public step lifecycle.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationStepRunState {
    Pending,
    Running,
    Retryable,
    Succeeded,
    Failed,
    Cancelled,
    Skipped,
}

/// Terminal worker result accepted by the Evaluation authority.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvaluationStepCompletion {
    pub state: EvaluationStepRunState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub awarded_score: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostic_code: Option<DiagnosticCode>,
    pub cleanup_verified: bool,
}

impl EvaluationStepCompletion {
    /// Validates terminal step completion shape.
    pub fn validate(
        &self,
        role: EvaluationStepRole,
        max_score: u32,
    ) -> Result<(), EvaluationControlContractError> {
        match self.state {
            EvaluationStepRunState::Succeeded
                if self.diagnostic_code.is_none()
                    && self.cleanup_verified
                    && ((role == EvaluationStepRole::Score && self.awarded_score.is_some())
                        || (role != EvaluationStepRole::Score && self.awarded_score.is_none()))
                    && self.awarded_score.unwrap_or(0) <= max_score =>
            {
                Ok(())
            }
            EvaluationStepRunState::Failed | EvaluationStepRunState::Cancelled
                if self.diagnostic_code.is_some() && self.awarded_score.is_none() =>
            {
                Ok(())
            }
            _ => Err(EvaluationControlContractError::TerminalStateInvalid),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EvaluationControlContractError {
    #[error("Evaluation control schema version is unsupported")]
    SchemaVersionInvalid,
    #[error("EvaluationSpec is invalid")]
    SpecInvalid,
    #[error("Evaluation runtime identity is invalid")]
    RuntimeIdentityInvalid,
    #[error("Evaluation identity hash mismatch")]
    IdentityMismatch,
    #[error("Evaluation score is invalid")]
    ScoreInvalid,
    #[error("Evaluation terminal state is invalid")]
    TerminalStateInvalid,
}

impl EvaluationControlContractError {
    #[must_use]
    pub const fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::SchemaVersionInvalid => "LW_EVALUATION_SCHEMA_VERSION_INVALID",
            Self::SpecInvalid => "LW_EVALUATION_SPEC_INVALID",
            Self::RuntimeIdentityInvalid => "LW_EVALUATION_RUNTIME_IDENTITY_INVALID",
            Self::IdentityMismatch => "LW_EVALUATION_IDENTITY_MISMATCH",
            Self::ScoreInvalid => "LW_EVALUATION_SCORE_INVALID",
            Self::TerminalStateInvalid => "LW_EVALUATION_STATE_CONFLICT",
        }
    }
}

fn is_digest_pinned_image(value: &str) -> bool {
    let Some((name, digest)) = value.rsplit_once("@sha256:") else {
        return false;
    };
    !name.trim().is_empty()
        && !name.contains(char::is_whitespace)
        && digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_token(value: &str, max_len: usize) -> Result<(), EvaluationControlContractError> {
    if value.trim().is_empty() || value.len() > max_len || value.chars().any(char::is_control) {
        return Err(EvaluationControlContractError::IdentityMismatch);
    }
    Ok(())
}
