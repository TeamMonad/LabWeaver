//! Evaluation step scheduling primitives.
//!
//! The control plane owns the durable step lease.  This module owns the small amount of
//! orchestration around that lease: it resolves the immutable release before execution, expands
//! an approved program profile into direct argv, and keeps the one-shot Resource reservation
//! fenced until the attempt has been cleaned up.  Kubernetes-specific runners implement
//! [`EvaluationAttemptRunner`] at the boundary; the scheduler never embeds a second state
//! machine.

#![allow(missing_docs, clippy::missing_errors_doc)]

use std::{path::PathBuf, sync::Arc, time::Duration};

use async_trait::async_trait;
use contracts::evaluation::{
    AdvisoryOutputMode, AdvisoryRunnerSpec, ApprovedProgramProfile, DeterministicRunnerSpec,
    EvaluationRelease, EvaluationRun, EvaluationStep, EvaluationStepCompletion,
    EvaluationStepRunState, ProgramPhase,
};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{EvaluationStepLease, PgEvaluationControlStore};

/// The execution identity used by every direct argv expansion.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProgramCommandPaths {
    /// The materialized submission source file.
    pub source: PathBuf,
    /// The output binary path.
    pub binary: PathBuf,
    /// The read-only submission root.
    pub submission_dir: PathBuf,
    /// The read-only evaluator root.
    pub evaluator_dir: PathBuf,
}

impl ProgramCommandPaths {
    /// Creates paths after checking that they are absolute and contain no control characters.
    pub fn new(
        source: impl Into<PathBuf>,
        binary: impl Into<PathBuf>,
        submission_dir: impl Into<PathBuf>,
        evaluator_dir: impl Into<PathBuf>,
    ) -> Result<Self, ExecutionError> {
        let paths = Self {
            source: source.into(),
            binary: binary.into(),
            submission_dir: submission_dir.into(),
            evaluator_dir: evaluator_dir.into(),
        };
        for path in [
            &paths.source,
            &paths.binary,
            &paths.submission_dir,
            &paths.evaluator_dir,
        ] {
            if !path.is_absolute() || path.to_string_lossy().chars().any(char::is_control) {
                return Err(ExecutionError::PathInvalid);
            }
        }
        Ok(paths)
    }
}

/// Expands one approved profile without shell parsing or environment interpolation.
pub fn expand_program_argv(
    profile: &ApprovedProgramProfile,
    phase: ProgramPhase,
    paths: &ProgramCommandPaths,
) -> Result<Vec<String>, ExecutionError> {
    profile
        .validate_for_phase(phase)
        .map_err(|_| ExecutionError::ProgramProfileInvalid)?;
    let argv = match phase {
        ProgramPhase::Compile => profile
            .compile_argv
            .as_deref()
            .ok_or(ExecutionError::ProgramProfileInvalid)?,
        ProgramPhase::Test => &profile.run_argv,
    };
    if argv.is_empty() {
        return Err(ExecutionError::ProgramProfileInvalid);
    }
    argv.iter()
        .map(|argument| substitute_argument(argument, paths))
        .collect()
}

fn substitute_argument(
    argument: &str,
    paths: &ProgramCommandPaths,
) -> Result<String, ExecutionError> {
    let mut output = String::with_capacity(argument.len());
    let mut remainder = argument;
    while let Some(start) = remainder.find('{') {
        output.push_str(&remainder[..start]);
        let tail = &remainder[start..];
        let end = tail
            .find('}')
            .ok_or(ExecutionError::ProgramProfileInvalid)?;
        let token = &tail[..=end];
        let replacement = match token {
            "{source}" => &paths.source,
            "{binary}" => &paths.binary,
            "{submission_dir}" => &paths.submission_dir,
            "{evaluator_dir}" => &paths.evaluator_dir,
            _ => return Err(ExecutionError::ProgramProfileInvalid),
        };
        output.push_str(&replacement.to_string_lossy());
        remainder = &tail[end + 1..];
    }
    if remainder.contains('}') {
        return Err(ExecutionError::ProgramProfileInvalid);
    }
    output.push_str(remainder);
    if output.is_empty() || output.chars().any(char::is_control) {
        return Err(ExecutionError::ProgramProfileInvalid);
    }
    Ok(output)
}

/// A validated step description resolved from an immutable release.
#[derive(Clone, Debug, PartialEq)]
pub enum StepExecutionPlan {
    /// Checks the existence of each listed submission-relative path.
    FileAssertion { required_files: Vec<String> },
    /// Executes an approved program profile.
    Program {
        toolchain_profile: String,
        phase: ProgramPhase,
        input: String,
        test_groups: Vec<contracts::evaluation::TestGroup>,
        limits: contracts::evaluation::ExecutionLimits,
    },
    /// Executes a read-only Linux probe.
    AnsibleProbe {
        playbook_profile: String,
        module_allowlist: Vec<String>,
        assertions: Vec<contracts::evaluation::FactAssertion>,
    },
    /// Runs one bounded, advisory-only Agent LLM review.
    Advisory {
        include: Vec<String>,
        rubric: String,
        output_mode: AdvisoryOutputMode,
    },
}

/// Resolves a deterministic runner from the exact release step.
pub fn plan_deterministic_step(step: &EvaluationStep) -> Result<StepExecutionPlan, ExecutionError> {
    let Some(runner) = step.deterministic_runner() else {
        let Some(AdvisoryRunnerSpec::LlmReview {
            include,
            rubric,
            output_mode,
        }) = step.advisory_runner()
        else {
            return Err(ExecutionError::StepRoleInvalid);
        };
        return Ok(StepExecutionPlan::Advisory {
            include: include.clone(),
            rubric: rubric.clone(),
            output_mode: *output_mode,
        });
    };
    match runner {
        DeterministicRunnerSpec::FileAssertion { required_files } => {
            Ok(StepExecutionPlan::FileAssertion {
                required_files: required_files.clone(),
            })
        }
        DeterministicRunnerSpec::Program {
            toolchain_profile,
            phase,
            input,
            test_groups,
            limits,
        } => Ok(StepExecutionPlan::Program {
            toolchain_profile: toolchain_profile.clone(),
            phase: *phase,
            input: input.clone(),
            test_groups: test_groups.clone(),
            limits: *limits,
        }),
        DeterministicRunnerSpec::AnsibleProbe {
            playbook_profile,
            module_allowlist,
            assertions,
            read_only,
        } if *read_only => Ok(StepExecutionPlan::AnsibleProbe {
            playbook_profile: playbook_profile.clone(),
            module_allowlist: module_allowlist.clone(),
            assertions: assertions.clone(),
        }),
        DeterministicRunnerSpec::AnsibleProbe { .. } => Err(ExecutionError::StepInvalid),
    }
}

pub use task_execution::resource::{
    ResourceClient, ResourceClientConfiguration, ResourceClientError, TaskResourceError,
    TaskResourceLifecycle, required_scopes_for_diagnostics,
};

/// Context passed to one attempt runner after the durable lease is acquired.
#[derive(Clone, Debug)]
pub struct EvaluationAttemptContext {
    pub lease: EvaluationStepLease,
    pub run: EvaluationRun,
    pub release: EvaluationRelease,
    pub step: EvaluationStep,
    pub execution_plan: StepExecutionPlan,
    /// Cancelled when the durable worker lease is lost.  A runner must stop
    /// external work and complete cleanup before returning after cancellation.
    pub cancellation: CancellationToken,
    /// Cancelled only when the durable lease fence is lost, so cleanup cannot continue under a
    /// stale worker even when the run itself is also being cancelled.
    pub lease_lost: CancellationToken,
}

pub use task_execution::timing::ExecutionTiming;

/// Runner boundary for Kubernetes Jobs, local deterministic checks, or another approved backend.
#[async_trait]
pub trait EvaluationAttemptRunner: Send + Sync + 'static {
    async fn execute(
        &self,
        context: EvaluationAttemptContext,
    ) -> Result<EvaluationStepCompletion, ExecutionError>;
}

/// Durable scheduler for Evaluation step attempts.
pub struct EvaluationWorker<R> {
    control: PgEvaluationControlStore,
    runner: Arc<R>,
    worker_id: String,
    lease_duration: Duration,
    poll_interval: Duration,
}

impl<R> Clone for EvaluationWorker<R> {
    fn clone(&self) -> Self {
        Self {
            control: self.control.clone(),
            runner: self.runner.clone(),
            worker_id: self.worker_id.clone(),
            lease_duration: self.lease_duration,
            poll_interval: self.poll_interval,
        }
    }
}

impl<R: EvaluationAttemptRunner> EvaluationWorker<R> {
    /// Creates a scheduler with bounded lease and polling intervals.
    pub fn new(
        control: PgEvaluationControlStore,
        runner: Arc<R>,
        worker_id: String,
        lease_duration: Duration,
        poll_interval: Duration,
    ) -> Result<Self, ExecutionError> {
        if worker_id.is_empty()
            || worker_id.len() > 96
            || worker_id.chars().any(char::is_control)
            || !(30_000..=1_800_000).contains(&lease_duration.as_millis())
            || poll_interval.is_zero()
        {
            return Err(ExecutionError::WorkerConfigurationInvalid);
        }
        Ok(Self {
            control,
            runner,
            worker_id,
            lease_duration,
            poll_interval,
        })
    }

    /// Runs until shutdown or a durable control-plane failure.
    pub async fn run(self) -> Result<(), ExecutionError> {
        loop {
            if !self.run_once().await? {
                tokio::time::sleep(self.poll_interval).await;
            }
        }
    }

    /// Processes at most one leased step and returns whether work was claimed.
    #[allow(
        clippy::too_many_lines,
        reason = "the one-step loop keeps lease heartbeat, cancellation fencing, execution, and completion in one transaction boundary"
    )]
    pub async fn run_once(&self) -> Result<bool, ExecutionError> {
        let Some(lease) = self
            .control
            .claim_next_step(&self.worker_id, self.lease_duration)
            .await
            .map_err(ExecutionError::Control)?
        else {
            return Ok(false);
        };
        let run = self
            .control
            .load_run(lease.run_id)
            .await
            .map_err(ExecutionError::Control)?;
        run.validate_against_release(
            &self
                .control
                .load_release(run.release_id)
                .await
                .map_err(ExecutionError::Control)?,
        )
        .map_err(|_| ExecutionError::IdentityMismatch)?;
        let release = self
            .control
            .load_release(run.release_id)
            .await
            .map_err(ExecutionError::Control)?;
        let step = release
            .evaluation_spec
            .body()
            .steps()
            .iter()
            .find(|step| step.id() == lease.step_id)
            .cloned()
            .ok_or(ExecutionError::StepMissing)?;
        let execution_plan = plan_deterministic_step(&step)?;
        let cancellation = CancellationToken::new();
        let heartbeat_cancellation = cancellation.clone();
        let lease_lost = CancellationToken::new();
        let attempt_lease_lost = lease_lost.clone();
        let heartbeat_lease_lost = lease_lost.clone();
        let heartbeat_control = self.control.clone();
        let heartbeat_lease = lease.clone();
        let heartbeat_duration = self.lease_duration;
        let heartbeat_interval = heartbeat_duration
            .checked_div(3)
            .ok_or(ExecutionError::WorkerConfigurationInvalid)?
            .max(Duration::from_secs(1));
        let heartbeat = tokio::spawn(async move {
            loop {
                tokio::time::sleep(heartbeat_interval).await;
                match heartbeat_control
                    .renew_step_lease(&heartbeat_lease, heartbeat_duration)
                    .await
                {
                    Ok(cancellation_requested) => {
                        if cancellation_requested {
                            heartbeat_cancellation.cancel();
                        }
                    }
                    Err(error) => {
                        tracing::warn!(
                            event = "evaluation.step_lease.renew_failed",
                            step_run_id = %heartbeat_lease.step_run_id,
                            task_run_id = %heartbeat_lease.task_run_id,
                            worker_id = %heartbeat_lease.worker_id,
                            error = %error,
                            "stopping attempt after durable lease renewal failure",
                        );
                        heartbeat_lease_lost.cancel();
                        heartbeat_cancellation.cancel();
                        break;
                    }
                }
            }
        });
        let context = EvaluationAttemptContext {
            lease: lease.clone(),
            run,
            release,
            step,
            execution_plan,
            cancellation: cancellation.clone(),
            lease_lost,
        };
        let completion = self.runner.execute(context).await;
        heartbeat.abort();
        let mut completion = completion?;
        if attempt_lease_lost.is_cancelled() {
            return Err(ExecutionError::LeaseLost);
        }
        if cancellation.is_cancelled() {
            let current_run = self
                .control
                .load_run(lease.run_id)
                .await
                .map_err(ExecutionError::Control)?;
            if !current_run.cancellation_requested
                && current_run.state != contracts::evaluation::EvaluationRunState::Cancelling
            {
                return Err(ExecutionError::LeaseLost);
            }
            completion = EvaluationStepCompletion {
                state: EvaluationStepRunState::Cancelled,
                awarded_score: None,
                review: None,
                diagnostic_code: Some(contracts::DiagnosticCode::registered(
                    "LW_EVALUATION_CANCELLED",
                )),
                cleanup_verified: completion.cleanup_verified,
            };
        }
        self.control
            .complete_step(
                lease.project_id,
                lease.course_id,
                lease.run_id,
                lease.step_run_id,
                lease.attempt,
                &lease.worker_id,
                &lease.runtime_identity,
                lease.lease_token(),
                &completion,
                &lease.trace_id,
            )
            .await
            .map_err(ExecutionError::Control)?;
        Ok(true)
    }
}

/// Errors while constructing or running an Evaluation attempt.
#[derive(Debug, Error)]
pub enum ExecutionError {
    #[error("LW_EVALUATION_EXECUTION_PATH_INVALID")]
    PathInvalid,
    #[error("LW_EVALUATION_PROGRAM_PROFILE_INVALID")]
    ProgramProfileInvalid,
    #[error("LW_EVALUATION_STEP_ROLE_INVALID")]
    StepRoleInvalid,
    #[error("LW_EVALUATION_STEP_INVALID")]
    StepInvalid,
    #[error("LW_EVALUATION_WORKER_CONFIGURATION_INVALID")]
    WorkerConfigurationInvalid,
    #[error("LW_EVALUATION_STEP_MISSING")]
    StepMissing,
    #[error("LW_EVALUATION_IDENTITY_MISMATCH")]
    IdentityMismatch,
    #[error("LW_EVALUATION_STEP_LEASE_LOST")]
    LeaseLost,
    #[error("LW_EVALUATION_EXECUTION_BACKEND_FAILED: {0}")]
    Backend(String),
    #[error(transparent)]
    Control(#[from] crate::EvaluationControlStoreError),
    #[error(transparent)]
    TaskResource(TaskResourceFailure),
}

impl From<TaskResourceError> for ExecutionError {
    fn from(error: TaskResourceError) -> Self {
        Self::TaskResource(TaskResourceFailure(error))
    }
}

/// Evaluation-stable diagnostic for a shared task resource failure.
///
/// The shared lifecycle reports neutral `LW_TASK_RESOURCE_*` codes; Evaluation keeps its
/// published `LW_EVALUATION_*` diagnostics for the operations and runbook evidence that predate
/// the extraction.
#[derive(Debug)]
pub struct TaskResourceFailure(pub TaskResourceError);

impl std::fmt::Display for TaskResourceFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match &self.0 {
            TaskResourceError::RequestInvalid => "LW_EVALUATION_TASK_RESOURCE_REQUEST_INVALID",
            TaskResourceError::ResponseInvalid => "LW_EVALUATION_TASK_RESOURCE_RESPONSE_INVALID",
            TaskResourceError::IdentityMismatch => "LW_EVALUATION_TASK_RESOURCE_IDENTITY_MISMATCH",
            TaskResourceError::ResourceTerminal => "LW_EVALUATION_TASK_RESOURCE_TERMINAL",
            TaskResourceError::ResourceApprovalTimeout => {
                "LW_EVALUATION_TASK_RESOURCE_APPROVAL_TIMEOUT"
            }
            TaskResourceError::Cancelled => "LW_EVALUATION_TASK_RESOURCE_CANCELLED",
            TaskResourceError::CreateUncertain => "LW_EVALUATION_TASK_RESOURCE_CREATE_UNCERTAIN",
            TaskResourceError::Client(source) => match source {
                ResourceClientError::Configuration => "LW_EVALUATION_RESOURCE_CONFIG_INVALID",
                ResourceClientError::Token(_) => "LW_EVALUATION_RESOURCE_TOKEN_FAILED",
                ResourceClientError::Transport => "LW_EVALUATION_RESOURCE_TRANSPORT_FAILED",
                ResourceClientError::RequestInvalid => "LW_EVALUATION_RESOURCE_REQUEST_INVALID",
                ResourceClientError::RequestTooLarge => "LW_EVALUATION_RESOURCE_REQUEST_TOO_LARGE",
                ResourceClientError::ResponseTooLarge => {
                    "LW_EVALUATION_RESOURCE_RESPONSE_TOO_LARGE"
                }
                ResourceClientError::ResponseInvalid => "LW_EVALUATION_RESOURCE_RESPONSE_INVALID",
                ResourceClientError::RequestMissing => "LW_EVALUATION_RESOURCE_REQUEST_MISSING",
                ResourceClientError::TaskResourceMissing => "LW_EVALUATION_RESOURCE_TASK_MISSING",
                ResourceClientError::Denied => "LW_EVALUATION_RESOURCE_DENIED",
                ResourceClientError::Conflict => "LW_EVALUATION_RESOURCE_CONFLICT",
                ResourceClientError::Rejected => "LW_EVALUATION_RESOURCE_REJECTED",
                ResourceClientError::Unavailable => "LW_EVALUATION_RESOURCE_UNAVAILABLE",
            },
        })
    }
}

impl std::error::Error for TaskResourceFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::{ExecutionError, ProgramCommandPaths, expand_program_argv};
    use contracts::evaluation::{
        APPROVED_PROGRAM_PROFILE_SCHEMA_VERSION, ApprovedProgramProfile, ProgramPhase,
    };

    #[test]
    fn profile_expansion_is_direct_and_closed() -> Result<(), Box<dyn std::error::Error>> {
        let profile = ApprovedProgramProfile {
            schema_version: APPROVED_PROGRAM_PROFILE_SCHEMA_VERSION.to_owned(),
            compile_argv: Some(vec![
                "g++".to_owned(),
                "{source}".to_owned(),
                "-o".to_owned(),
                "{binary}".to_owned(),
            ]),
            run_argv: vec!["{binary}".to_owned()],
            support_files: Vec::new(),
        };
        let root = std::env::temp_dir().join("labweaver-execution-tests");
        let source = root.join("input").join("submission").join("main.cpp");
        let binary = root.join("work").join("submission");
        let submission_dir = root.join("input").join("submission");
        let evaluator_dir = root.join("input").join("evaluator");
        let paths = ProgramCommandPaths::new(
            source.clone(),
            binary.clone(),
            submission_dir,
            evaluator_dir,
        )?;
        assert_eq!(
            expand_program_argv(&profile, ProgramPhase::Compile, &paths)?,
            vec![
                "g++".to_owned(),
                source.to_string_lossy().to_string(),
                "-o".to_owned(),
                binary.to_string_lossy().to_string()
            ]
        );
        assert_eq!(
            expand_program_argv(&profile, ProgramPhase::Test, &paths)?,
            vec![binary.to_string_lossy().to_string()]
        );
        Ok(())
    }

    #[test]
    fn profile_rejects_unknown_tokens() -> Result<(), Box<dyn std::error::Error>> {
        let profile = ApprovedProgramProfile {
            schema_version: APPROVED_PROGRAM_PROFILE_SCHEMA_VERSION.to_owned(),
            compile_argv: None,
            run_argv: vec!["{shell}".to_owned()],
            support_files: Vec::new(),
        };
        let root = std::env::temp_dir().join("labweaver-execution-tests");
        let paths = ProgramCommandPaths::new(
            root.join("source"),
            root.join("binary"),
            root.join("submission"),
            root.join("evaluator"),
        )?;
        assert!(matches!(
            expand_program_argv(&profile, ProgramPhase::Test, &paths),
            Err(ExecutionError::ProgramProfileInvalid)
        ));
        Ok(())
    }
}
