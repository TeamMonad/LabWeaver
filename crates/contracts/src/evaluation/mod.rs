//! Stable deterministic evaluation and advisory review contracts.

mod control;
mod review;
mod spec;
mod validation;

pub use control::{
    EVALUATION_RELEASE_SCHEMA_VERSION, EVALUATION_RUN_SCHEMA_VERSION,
    EvaluationControlContractError, EvaluationRelease, EvaluationReleaseState, EvaluationRun,
    EvaluationRunIdentity, EvaluationRunState, EvaluationRuntimeIdentity, EvaluationStepCompletion,
    EvaluationStepRole, EvaluationStepRun, EvaluationStepRunState,
};
pub use review::{
    EvidenceLocation, FindingResult, GoalAssessment, GoalFinding, GoalReview, GoalReviewError,
};
pub use spec::{
    AdvisoryFailurePolicy, AdvisoryOutputMode, AdvisoryRunnerSpec, AdvisoryStep, AggregationGate,
    AggregationKind, AggregationSpec, CheckerSpec, CollectorSpec, DeterministicRunnerSpec,
    EvaluationBody, EvaluationMetadata, EvaluationSpec, EvaluationStep, ExecutionLimits,
    ExpectedServiceState, FactAssertion, GateFailurePolicy, GateStep, ManualReviewReason,
    ProgramPhase, RequiredStatus, ReviewPolicy, ScoreFailurePolicy, ScoreSpec, ScoreStep,
    SubmissionSpec, TestGroup, evaluation_spec_schema, goal_review_schema,
};
pub use validation::EvaluationSpecError;
