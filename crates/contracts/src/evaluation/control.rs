use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    ActorId, ApprovalId, ArtifactId, CandidateId, CourseId, DiagnosticCode, EvaluationReleaseId,
    EvaluationRunId, EvaluationStepRunId, FrozenSubmissionId, ProblemPackageId, ProjectId,
    Revision, UtcTimestamp,
};

use super::{EvaluationSpec, GoalReview, ProgramPhase};

/// Versioned schema identity for `EvaluationRelease` projections.
pub const EVALUATION_RELEASE_SCHEMA_VERSION: &str = "evaluation.labweaver.io/evaluation-release/v1";
/// Versioned schema identity for `EvaluationRun` projections.
pub const EVALUATION_RUN_SCHEMA_VERSION: &str = "evaluation.labweaver.io/evaluation-run/v1";
/// Versioned schema identity for the package file that describes an approved program command.
pub const APPROVED_PROGRAM_PROFILE_SCHEMA_VERSION: &str =
    "evaluation.labweaver.io/program-profile/v1";

/// Immutable package and object-store binding used to materialize an evaluation.
///
/// The package is retained in the private Evaluation release row together with the exact object
/// locators.  Evaluation workers therefore never resolve a mutable package or object-store
/// listing at execution time.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvaluationExecutionBinding {
    pub package: crate::authoring::ProblemPackage,
    pub object_locators: BTreeMap<ArtifactId, String>,
}

impl EvaluationExecutionBinding {
    /// Validates package ownership and exact artifact-to-locator coverage.
    pub fn validate(&self) -> Result<(), EvaluationControlContractError> {
        self.package
            .validate()
            .map_err(|_| EvaluationControlContractError::ExecutionBindingInvalid)?;

        let mut package_artifacts = BTreeSet::new();
        for file in &self.package.files {
            if !package_artifacts.insert(file.object.artifact_id)
                || !self.object_locators.contains_key(&file.object.artifact_id)
            {
                return Err(EvaluationControlContractError::ExecutionBindingInvalid);
            }
            let Some(locator) = self.object_locators.get(&file.object.artifact_id) else {
                return Err(EvaluationControlContractError::ExecutionBindingInvalid);
            };
            if locator.trim().is_empty()
                || locator.len() > 1024
                || locator.chars().any(char::is_control)
            {
                return Err(EvaluationControlContractError::ExecutionBindingInvalid);
            }
        }
        if package_artifacts.len() != self.object_locators.len()
            || self
                .object_locators
                .keys()
                .any(|artifact_id| !package_artifacts.contains(artifact_id))
        {
            return Err(EvaluationControlContractError::ExecutionBindingInvalid);
        }
        Ok(())
    }

    /// Verifies that the immutable package belongs to the supplied project context.
    pub fn validate_ownership(
        &self,
        project_id: ProjectId,
        course_id: Option<CourseId>,
    ) -> Result<(), EvaluationControlContractError> {
        if self.package.project_id != project_id || self.package.course_id != course_id {
            return Err(EvaluationControlContractError::IdentityMismatch);
        }
        Ok(())
    }

    /// Returns the immutable package identity after validating the binding.
    #[must_use]
    pub const fn package_id(&self) -> ProblemPackageId {
        self.package.id
    }
}

/// Approved direct-exec command vectors stored as a package file.
///
/// Every argument is passed directly to the process.  Only the four closed path tokens accepted
/// by `validate_argument` may be substituted by an execution worker; no shell parsing is part of
/// this contract.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApprovedProgramProfile {
    pub schema_version: String,
    pub compile_argv: Option<Vec<String>>,
    pub run_argv: Vec<String>,
    /// Package-relative auxiliary scripts or modules explicitly approved for evaluator reads.
    ///
    /// An empty list means that no auxiliary package files are authorized; it does not select a
    /// fallback evaluator directory. Environment removes entries that overlap private test
    /// sources before the profile reaches an execution attempt.
    #[serde(default)]
    pub support_files: Vec<String>,
}

impl ApprovedProgramProfile {
    /// Validates the profile independent of the selected evaluation phase.
    pub fn validate(&self) -> Result<(), EvaluationControlContractError> {
        if self.schema_version != APPROVED_PROGRAM_PROFILE_SCHEMA_VERSION {
            return Err(EvaluationControlContractError::ProgramProfileInvalid);
        }
        validate_argv(&self.run_argv)?;
        if let Some(compile_argv) = &self.compile_argv {
            validate_argv(compile_argv)?;
        }
        if self.support_files.len() > 128 {
            return Err(EvaluationControlContractError::ProgramProfileInvalid);
        }
        let mut support_files = BTreeSet::new();
        for path in &self.support_files {
            crate::validate_relative_path(path)
                .map_err(|_| EvaluationControlContractError::ProgramProfileInvalid)?;
            if !support_files.insert(path) {
                return Err(EvaluationControlContractError::ProgramProfileInvalid);
            }
        }
        Ok(())
    }

    /// Validates that the selected phase has a complete direct-exec command.
    pub fn validate_for_phase(
        &self,
        phase: ProgramPhase,
    ) -> Result<(), EvaluationControlContractError> {
        self.validate()?;
        if phase == ProgramPhase::Compile && self.compile_argv.is_none() {
            return Err(EvaluationControlContractError::ProgramProfileInvalid);
        }
        Ok(())
    }
}

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
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
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

    /// Verifies that the release belongs to the exact project context supplied by Control.
    pub fn validate_ownership(
        &self,
        project_id: ProjectId,
        course_id: Option<CourseId>,
    ) -> Result<(), EvaluationControlContractError> {
        if self.project_id != project_id || self.course_id != course_id {
            return Err(EvaluationControlContractError::IdentityMismatch);
        }
        Ok(())
    }

    /// Verifies the release and candidate share the same immutable identity and project context.
    pub fn validate_against_candidate(
        &self,
        candidate: &crate::authoring::EvaluationCandidate,
    ) -> Result<(), EvaluationControlContractError> {
        if self.project_id != candidate.project_id
            || self.course_id != candidate.course_id
            || self.candidate_id != candidate.id
            || self.candidate_revision != candidate.revision
        {
            return Err(EvaluationControlContractError::IdentityMismatch);
        }
        Ok(())
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
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
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
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StudentEvaluationResult {
    pub run_id: EvaluationRunId,
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
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
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StudentEvaluationStepResult {
    #[schemars(extend("minimum" = 1))]
    pub position: u32,
    pub role: EvaluationStepRole,
    pub state: EvaluationStepRunState,
    pub max_score: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub awarded_score: Option<u32>,
    /// Optional advisory review emitted by a successful non-scoring step.
    ///
    /// This field is informational and is never included in deterministic score aggregation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review: Option<GoalReview>,
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
            project_id: run.project_id,
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
                    review: step.review.clone(),
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
    /// Verifies that a run remains bound to the release's exact project context.
    pub fn validate_against_release(
        &self,
        release: &EvaluationRelease,
    ) -> Result<(), EvaluationControlContractError> {
        if self.project_id != release.project_id
            || self.course_id != release.course_id
            || self.release_id != release.id
        {
            return Err(EvaluationControlContractError::IdentityMismatch);
        }
        Ok(())
    }

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
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
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
    /// Optional advisory review. Only a successful Advisory step may carry a review.
    ///
    /// The review is an informational projection and never contributes to `awarded_score`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review: Option<GoalReview>,
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
        if self.review.is_some()
            && (self.role != EvaluationStepRole::Advisory
                || self.state != EvaluationStepRunState::Succeeded)
        {
            return Err(EvaluationControlContractError::TerminalStateInvalid);
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
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvaluationStepCompletion {
    pub state: EvaluationStepRunState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub awarded_score: Option<u32>,
    /// Optional advisory review produced by an Agent-backed Advisory step.
    ///
    /// Reviews are informational and never affect the deterministic score.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review: Option<GoalReview>,
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
        if self.review.is_some()
            && (role != EvaluationStepRole::Advisory
                || self.state != EvaluationStepRunState::Succeeded)
        {
            return Err(EvaluationControlContractError::TerminalStateInvalid);
        }
        if let Some(review) = &self.review {
            review
                .validate()
                .map_err(|_| EvaluationControlContractError::TerminalStateInvalid)?;
        }
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
    #[error("Evaluation execution binding is invalid")]
    ExecutionBindingInvalid,
    #[error("Approved program profile is invalid")]
    ProgramProfileInvalid,
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
            Self::ExecutionBindingInvalid => "LW_EVALUATION_EXECUTION_BINDING_INVALID",
            Self::ProgramProfileInvalid => "LW_EVALUATION_PROGRAM_PROFILE_INVALID",
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

fn validate_argv(argv: &[String]) -> Result<(), EvaluationControlContractError> {
    if argv.is_empty() || argv.len() > 128 {
        return Err(EvaluationControlContractError::ProgramProfileInvalid);
    }
    for argument in argv {
        if argument.trim().is_empty()
            || argument.len() > 1024
            || argument.chars().any(char::is_control)
        {
            return Err(EvaluationControlContractError::ProgramProfileInvalid);
        }
        let mut remainder = argument.as_str();
        while let Some(start) = remainder.find('{') {
            let Some(end_offset) = remainder[start..].find('}') else {
                return Err(EvaluationControlContractError::ProgramProfileInvalid);
            };
            let end = start + end_offset + 1;
            let token = &remainder[start..end];
            if !matches!(
                token,
                "{source}" | "{binary}" | "{submission_dir}" | "{evaluator_dir}"
            ) {
                return Err(EvaluationControlContractError::ProgramProfileInvalid);
            }
            remainder = &remainder[end..];
        }
        if remainder.contains('}') {
            return Err(EvaluationControlContractError::ProgramProfileInvalid);
        }
    }
    Ok(())
}
