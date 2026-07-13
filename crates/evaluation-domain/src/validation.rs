use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Component, Path};

use thiserror::Error;

use crate::spec::{AggregationKind, EvaluationSpec};

/// Stable fail-fast diagnostics for `EvaluationSpec` documents.
#[derive(Debug, Error, PartialEq)]
pub enum EvaluationSpecError {
    /// YAML syntax, type, enum, or unknown-field validation failed.
    #[error("EvaluationSpec document is invalid: {0}")]
    InvalidDocument(String),
    /// Required metadata was empty.
    #[error("EvaluationSpec metadata name and version are required")]
    InvalidMetadata,
    /// No evaluation steps were declared.
    #[error("EvaluationSpec must contain at least one step")]
    EmptySteps,
    /// A step identifier occurred more than once.
    #[error("duplicate EvaluationSpec step id: {step_id}")]
    DuplicateStepId {
        /// Duplicated identifier.
        step_id: String,
    },
    /// A dependency did not name an existing step.
    #[error("step {step_id} depends on missing step {dependency}")]
    MissingDependency {
        /// Dependent step.
        step_id: String,
        /// Missing dependency.
        dependency: String,
    },
    /// The step dependency graph contained a cycle.
    #[error("EvaluationSpec step graph contains a dependency cycle")]
    DependencyCycle,
    /// A path escaped the immutable submission root.
    #[error("unsafe relative path in {location}: {path}")]
    UnsafePath {
        /// Field or step containing the path.
        location: String,
        /// Rejected path.
        path: String,
    },
    /// Collector limits or inputs were empty.
    #[error("invalid collector configuration: {0}")]
    InvalidCollector(String),
    /// Runner configuration was inconsistent with its declared kind or phase.
    #[error("invalid configuration for step {step_id}: {detail}")]
    InvalidStepConfiguration {
        /// Step containing the error.
        step_id: String,
        /// Safe diagnostic detail.
        detail: String,
    },
    /// Deterministic score totals did not match the aggregator declaration.
    #[error(
        "deterministic score total {step_total} does not match aggregation max {aggregate_max}"
    )]
    AggregationScoreMismatch {
        /// Sum of deterministic score steps.
        step_total: u32,
        /// Declared aggregation maximum.
        aggregate_max: u32,
    },
    /// Summing deterministic score maxima exceeded the contract's integer range.
    #[error("deterministic score total overflows u32 at step {step_id}")]
    AggregationScoreOverflow {
        /// Step whose score caused the overflow.
        step_id: String,
    },
    /// An aggregation gate did not reference a Gate step.
    #[error("aggregation gate references non-gate or missing step: {step_id}")]
    InvalidAggregationGate {
        /// Invalid gate reference.
        step_id: String,
    },
    /// The release policy attempted to bypass mandatory teacher approval.
    #[error("teacher approval is required before releasing an EvaluationSpec")]
    TeacherApprovalRequired,
}

impl EvaluationSpecError {
    /// Returns the stable diagnostic code for this failure.
    #[must_use]
    pub const fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::InvalidDocument(_) => "LW_EVAL_SPEC_DOCUMENT_INVALID",
            Self::InvalidMetadata => "LW_EVAL_METADATA_INVALID",
            Self::EmptySteps => "LW_EVAL_STEPS_EMPTY",
            Self::DuplicateStepId { .. } => "LW_EVAL_STEP_DUPLICATE",
            Self::MissingDependency { .. } => "LW_EVAL_DEPENDENCY_MISSING",
            Self::DependencyCycle => "LW_EVAL_DAG_CYCLE",
            Self::UnsafePath { .. } => "LW_EVAL_SUBMISSION_PATH_UNSAFE",
            Self::InvalidCollector(_) => "LW_EVAL_COLLECTOR_INVALID",
            Self::InvalidStepConfiguration { .. } => "LW_EVAL_STEP_CONFIG_INVALID",
            Self::AggregationScoreMismatch { .. } => "LW_EVAL_AGGREGATION_SCORE_MISMATCH",
            Self::AggregationScoreOverflow { .. } => "LW_EVAL_AGGREGATION_SCORE_OVERFLOW",
            Self::InvalidAggregationGate { .. } => "LW_EVAL_AGGREGATION_GATE_INVALID",
            Self::TeacherApprovalRequired => "LW_EVAL_TEACHER_APPROVAL_REQUIRED",
        }
    }
}

pub(crate) fn validate_spec(spec: &EvaluationSpec) -> Result<(), EvaluationSpecError> {
    let metadata = spec.metadata();
    if metadata.name.trim().is_empty() || metadata.version.trim().is_empty() {
        return Err(EvaluationSpecError::InvalidMetadata);
    }

    let body = spec.body();
    if body.steps.is_empty() {
        return Err(EvaluationSpecError::EmptySteps);
    }
    if !body.review.teacher_approval_required_for_release() {
        return Err(EvaluationSpecError::TeacherApprovalRequired);
    }
    if body.submission.collector.max_bytes() == 0 {
        return Err(EvaluationSpecError::InvalidCollector(
            "maxBytes must be non-zero".to_owned(),
        ));
    }
    if !body.submission.collector.has_inputs() {
        return Err(EvaluationSpecError::InvalidCollector(
            "collector input list must not be empty".to_owned(),
        ));
    }
    if let Some(paths) = body.submission.collector.included_paths() {
        validate_paths("submission.collector.include", paths)?;
    }
    if let Some(paths) = body.submission.collector.excluded_paths() {
        validate_paths("submission.collector.exclude", paths)?;
    }

    let mut steps = BTreeMap::new();
    for step in &body.steps {
        if step.id().trim().is_empty() {
            return Err(EvaluationSpecError::InvalidStepConfiguration {
                step_id: step.id().to_owned(),
                detail: "step id must not be empty".to_owned(),
            });
        }
        if steps.insert(step.id(), step).is_some() {
            return Err(EvaluationSpecError::DuplicateStepId {
                step_id: step.id().to_owned(),
            });
        }
        if let Some(runner) = step.deterministic_runner() {
            runner.validate(step.id())?;
            validate_paths(step.id(), &runner.submission_paths())?;
        }
        if step.score() == Some(0) {
            return Err(EvaluationSpecError::InvalidStepConfiguration {
                step_id: step.id().to_owned(),
                detail: "score step max must be non-zero".to_owned(),
            });
        }
        if let Some(runner) = step.advisory_runner() {
            let paths = runner.included_paths();
            if paths.is_empty() {
                return Err(EvaluationSpecError::InvalidStepConfiguration {
                    step_id: step.id().to_owned(),
                    detail: "llm_review include must not be empty".to_owned(),
                });
            }
            validate_paths(step.id(), paths)?;
        }
    }

    validate_dependencies(&body.steps, &steps)?;
    validate_aggregation(spec, &steps)
}

fn validate_paths<T: AsRef<str>>(location: &str, paths: &[T]) -> Result<(), EvaluationSpecError> {
    for value in paths {
        let value = value.as_ref();
        let path = Path::new(value);
        let safe = !value.trim().is_empty()
            && !path.is_absolute()
            && path
                .components()
                .all(|component| matches!(component, Component::Normal(_) | Component::CurDir));
        if !safe {
            return Err(EvaluationSpecError::UnsafePath {
                location: location.to_owned(),
                path: value.to_owned(),
            });
        }
    }
    Ok(())
}

fn validate_dependencies<'a>(
    ordered_steps: &'a [crate::spec::EvaluationStep],
    steps: &BTreeMap<&'a str, &'a crate::spec::EvaluationStep>,
) -> Result<(), EvaluationSpecError> {
    let mut incoming = BTreeMap::new();
    let mut outgoing: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for step in ordered_steps {
        incoming.insert(step.id(), step.dependencies().len());
        let mut unique = BTreeSet::new();
        for dependency in step.dependencies() {
            if !steps.contains_key(dependency.as_str()) {
                return Err(EvaluationSpecError::MissingDependency {
                    step_id: step.id().to_owned(),
                    dependency: dependency.clone(),
                });
            }
            if !unique.insert(dependency.as_str()) {
                return Err(EvaluationSpecError::InvalidStepConfiguration {
                    step_id: step.id().to_owned(),
                    detail: format!("duplicate dependency: {dependency}"),
                });
            }
            outgoing
                .entry(dependency.as_str())
                .or_default()
                .push(step.id());
        }
    }

    let mut ready = incoming
        .iter()
        .filter_map(|(id, count)| (*count == 0).then_some(*id))
        .collect::<VecDeque<_>>();
    let mut visited = 0;
    while let Some(id) = ready.pop_front() {
        visited += 1;
        if let Some(dependents) = outgoing.get(id) {
            for dependent in dependents {
                if let Some(count) = incoming.get_mut(dependent) {
                    *count -= 1;
                    if *count == 0 {
                        ready.push_back(dependent);
                    }
                }
            }
        }
    }
    if visited != ordered_steps.len() {
        return Err(EvaluationSpecError::DependencyCycle);
    }
    Ok(())
}

fn validate_aggregation(
    spec: &EvaluationSpec,
    steps: &BTreeMap<&str, &crate::spec::EvaluationStep>,
) -> Result<(), EvaluationSpecError> {
    let aggregation = &spec.body().aggregation;
    match aggregation.kind {
        AggregationKind::DeterministicSum => {}
    }
    let score_total = spec.body().steps.iter().try_fold(0_u32, |total, step| {
        let Some(score) = step.score() else {
            return Ok(total);
        };
        total
            .checked_add(score)
            .ok_or_else(|| EvaluationSpecError::AggregationScoreOverflow {
                step_id: step.id().to_owned(),
            })
    })?;
    if score_total != aggregation.max_score {
        return Err(EvaluationSpecError::AggregationScoreMismatch {
            step_total: score_total,
            aggregate_max: aggregation.max_score,
        });
    }
    for gate in &aggregation.gates {
        if !steps
            .get(gate.step.as_str())
            .is_some_and(|step| step.is_gate())
        {
            return Err(EvaluationSpecError::InvalidAggregationGate {
                step_id: gate.step.clone(),
            });
        }
    }
    Ok(())
}
