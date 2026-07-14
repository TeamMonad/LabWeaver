//! Stable deterministic evaluation and advisory review contracts.

mod review;
mod spec;
mod validation;

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
