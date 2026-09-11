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
use contracts::{
    ActorId, CourseId, ProjectId, TaskRunId, UtcTimestamp,
    evaluation::{
        AdvisoryOutputMode, AdvisoryRunnerSpec, ApprovedProgramProfile, DeterministicRunnerSpec,
        EvaluationRelease, EvaluationRun, EvaluationStep, EvaluationStepCompletion,
        EvaluationStepRunState, ProgramPhase,
    },
    http::{
        AcknowledgeTaskResourceRequest, InternalCreateTaskResourceRequest,
        ReleaseTaskResourceRequest, ResourceRequestMutation,
    },
    resource::{
        CapacityClaimState, ResourceLeaseState, ResourceRequest, ResourceRequestState,
        ResourceTarget, WorkloadResources,
    },
};
use thiserror::Error;
use tokio::time::{Instant, timeout};
use tokio_util::sync::CancellationToken;

use crate::{EvaluationStepLease, PgEvaluationControlStore, ResourceClient, ResourceClientError};

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
    if argv.is_empty() || argv[0].contains('{') || argv[0].contains('}') {
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

/// A Resource reservation bound to one durable Evaluation attempt.
#[derive(Clone)]
pub struct TaskResourceLifecycle {
    client: ResourceClient,
    task_run_id: TaskRunId,
    project_id: ProjectId,
    course_id: Option<CourseId>,
    owner_id: ActorId,
    request_key: String,
    resources: WorkloadResources,
    duration_seconds: u64,
}

impl std::fmt::Debug for TaskResourceLifecycle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TaskResourceLifecycle")
            .field("task_run_id", &self.task_run_id)
            .field("project_id", &self.project_id)
            .field("course_id", &self.course_id)
            .field("owner_id", &self.owner_id)
            .field("request_key", &self.request_key)
            .field("resources", &self.resources)
            .field("duration_seconds", &self.duration_seconds)
            .finish_non_exhaustive()
    }
}

impl TaskResourceLifecycle {
    /// The Resource client has a bounded HTTP timeout, but this explicit ceiling also protects
    /// callers that construct the client with a custom `reqwest::Client`.  A timeout is an
    /// uncertain outcome: the server may have committed the request after the client stopped
    /// waiting.  The durable `TaskRunId` and idempotency key let the next worker reconcile it.
    const CREATE_SETTLE_TIMEOUT: Duration = Duration::from_mins(1);

    /// Builds an attempt-scoped reservation coordinator.
    #[allow(
        clippy::too_many_arguments,
        reason = "the constructor binds one durable reservation to its complete ownership identity"
    )]
    pub fn new(
        client: ResourceClient,
        task_run_id: TaskRunId,
        project_id: ProjectId,
        course_id: Option<CourseId>,
        owner_id: ActorId,
        request_key: String,
        resources: WorkloadResources,
        duration_seconds: u64,
    ) -> Result<Self, TaskResourceError> {
        if !(16..=96).contains(&request_key.len())
            || !request_key.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
            })
            || duration_seconds == 0
        {
            return Err(TaskResourceError::RequestInvalid);
        }
        resources
            .validate()
            .map_err(|_| TaskResourceError::RequestInvalid)?;
        Ok(Self {
            client,
            task_run_id,
            project_id,
            course_id,
            owner_id,
            request_key,
            resources,
            duration_seconds,
        })
    }

    /// Creates the request once, or verifies the same request after an idempotent retry.
    ///
    /// This operation intentionally does not race the cancellation token.  Dropping an in-flight
    /// POST and then treating a transient GET-missing response as proof that no request exists can
    /// leave a Resource reservation behind when the server commits the POST later.  The bounded
    /// wait below either settles the call or returns an explicit uncertain result; an uncertain
    /// result remains a worker error and therefore cannot complete the Evaluation step or claim
    /// cleanup was confirmed.
    pub async fn create(&self) -> Result<ResourceRequest, TaskResourceError> {
        let request = InternalCreateTaskResourceRequest {
            task_run_id: self.task_run_id,
            project_id: self.project_id,
            course_id: self.course_id,
            owner_id: self.owner_id,
            request_key: self.request_key.clone(),
            resources: self.resources.clone(),
            duration_seconds: self.duration_seconds,
        };
        let result = timeout(
            Self::CREATE_SETTLE_TIMEOUT,
            self.client.create_task_resource(&request),
        )
        .await
        .map_err(|_| TaskResourceError::CreateUncertain)?;
        match result {
            Ok(created) => Ok(created),
            Err(ResourceClientError::Conflict) => {
                let current = match self
                    .client
                    .get_task_resource_request(self.task_run_id)
                    .await
                {
                    Ok(current) => current,
                    // A conflict can mean that the original idempotent POST is still in
                    // progress.  A missing read at this point is therefore uncertain, not a
                    // proof that the reservation never existed.
                    Err(ResourceClientError::RequestMissing) => {
                        return Err(TaskResourceError::CreateUncertain);
                    }
                    Err(error) => return Err(error.into()),
                };
                validate_request(&current, &request)?;
                Ok(current)
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Waits for Resource approval/allocation and then claims the task reservation.
    pub async fn claim_after_approval(
        &self,
        poll_interval: Duration,
        timeout: Duration,
        cancellation: &CancellationToken,
    ) -> Result<contracts::http::TaskResourceStatus, TaskResourceError> {
        if poll_interval.is_zero() || timeout.is_zero() {
            return Err(TaskResourceError::RequestInvalid);
        }
        let deadline = Instant::now() + timeout;
        loop {
            if cancellation.is_cancelled() {
                self.cancel_or_release_request(
                    "evaluation step lease cancelled before resource claim",
                )
                .await?;
                return Err(TaskResourceError::Cancelled);
            }
            let request = tokio::select! {
                result = self.client.get_task_resource_request(self.task_run_id) => result?,
                () = cancellation.cancelled() => {
                    self.cancel_or_release_request("evaluation step lease cancelled before resource claim")
                        .await?;
                    return Err(TaskResourceError::Cancelled);
                }
            };
            match request.state {
                ResourceRequestState::Reviewing => {
                    let now = Instant::now();
                    if now >= deadline {
                        self.cancel_or_release_request(
                            "evaluation resource approval deadline exceeded",
                        )
                        .await?;
                        return Err(TaskResourceError::ResourceApprovalTimeout);
                    }
                    tokio::select! {
                        () = cancellation.cancelled() => {
                            self.cancel_or_release_request("evaluation step lease cancelled before resource claim")
                                .await?;
                            return Err(TaskResourceError::Cancelled);
                        }
                        () = tokio::time::sleep(poll_interval.min(deadline - now)) => {}
                    }
                }
                ResourceRequestState::Allocating | ResourceRequestState::Active => {
                    let status = tokio::select! {
                        result = self.client.claim_task_resource(self.task_run_id) => result?,
                        () = cancellation.cancelled() => {
                            self.cancel_or_release_request("evaluation step lease cancelled before resource claim")
                                .await?;
                            return Err(TaskResourceError::Cancelled);
                        }
                    };
                    if cancellation.is_cancelled() {
                        self.release(&status).await?;
                        return Err(TaskResourceError::Cancelled);
                    }
                    return Ok(status);
                }
                ResourceRequestState::Expiring
                | ResourceRequestState::Expired
                | ResourceRequestState::Rejected
                | ResourceRequestState::Cancelled => {
                    return Err(TaskResourceError::ResourceTerminal);
                }
            }
        }
    }

    /// Cancels a pending request or releases a claim observed during a cancellation race.
    pub async fn cancel(&self, reason: &str) -> Result<(), TaskResourceError> {
        self.cancel_or_release_request(reason).await
    }

    async fn cancel_or_release_request(&self, reason: &str) -> Result<(), TaskResourceError> {
        let mut request = match self
            .client
            .get_task_resource_request(self.task_run_id)
            .await
        {
            Ok(request) => request,
            // This method is only called after the create operation has been settled or while
            // reconciling the same durable attempt.  A missing read can still race an earlier
            // POST, so it must stay non-terminal until a later worker retries the exact
            // TaskRunId/idempotency key.
            Err(ResourceClientError::RequestMissing) => {
                return Err(TaskResourceError::CreateUncertain);
            }
            Err(error) => return Err(error.into()),
        };
        let idempotency_key = format!("{}-cancel", self.request_key);
        for _ in 0..4 {
            match request.state {
                ResourceRequestState::Reviewing => {
                    let mutation = ResourceRequestMutation {
                        expected_revision: request.revision,
                        reason: reason.to_owned(),
                    };
                    match self
                        .client
                        .cancel_task_resource(self.task_run_id, &mutation, &idempotency_key)
                        .await
                    {
                        Ok(cancelled) => {
                            if cancelled.state != ResourceRequestState::Cancelled {
                                return Err(TaskResourceError::ResponseInvalid);
                            }
                            return Ok(());
                        }
                        Err(ResourceClientError::Conflict) => {
                            request = self
                                .client
                                .get_task_resource_request(self.task_run_id)
                                .await?;
                        }
                        Err(error) => return Err(error.into()),
                    }
                }
                ResourceRequestState::Allocating | ResourceRequestState::Active => {
                    let status = self.client.claim_task_resource(self.task_run_id).await?;
                    let released = self.release(&status).await?;
                    if !released.cleanup_confirmed {
                        return Err(TaskResourceError::ResponseInvalid);
                    }
                    return Ok(());
                }
                ResourceRequestState::Expiring
                | ResourceRequestState::Expired
                | ResourceRequestState::Rejected
                | ResourceRequestState::Cancelled => return Ok(()),
            }
        }
        Err(TaskResourceError::Client(ResourceClientError::Conflict))
    }

    /// Activates and hands off the exact claim after validating the namespace
    /// selected by the scheduler for the real execution Job.
    pub async fn acknowledge(
        &self,
        status: &contracts::http::TaskResourceStatus,
        execution_namespace: &str,
    ) -> Result<contracts::http::TaskResourceStatus, TaskResourceError> {
        if status.task_run_id != self.task_run_id
            || status.project_id != self.project_id
            || status.owner_id != self.owner_id
        {
            return Err(TaskResourceError::IdentityMismatch);
        }
        if !valid_execution_namespace(execution_namespace) {
            return Err(TaskResourceError::RequestInvalid);
        }
        if status
            .execution_namespace
            .as_deref()
            .is_some_and(|known| known != execution_namespace)
        {
            return Err(TaskResourceError::IdentityMismatch);
        }
        // A worker can crash after Resource commits the handoff and before the
        // Evaluation execution checkpoint is written.  Reconcile that durable
        // state instead of attempting the one-way Allocating -> Active
        // transition a second time.
        if status.execution_namespace.as_deref() == Some(execution_namespace)
            && status.request.state == ResourceRequestState::Active
            && status.claim.state == CapacityClaimState::HandedOff
            && status.lease.state == ResourceLeaseState::Active
        {
            return Ok(status.clone());
        }
        let request = AcknowledgeTaskResourceRequest {
            expected_claim_revision: status.claim_revision,
            expected_lease_revision: status.lease_revision,
            execution_namespace: execution_namespace.to_owned(),
        };
        self.client
            .acknowledge_task_resource(self.task_run_id, &request)
            .await
            .map_err(Into::into)
    }

    /// Loads the authoritative task resource state while resuming an attempt.
    ///
    /// A resumed attempt must inspect this state before deciding whether a
    /// release is needed.  In particular, an earlier worker may have already
    /// released the claim after its cleanup commit became durable.
    pub async fn load_status(
        &self,
    ) -> Result<contracts::http::TaskResourceStatus, TaskResourceError> {
        let status = self.client.get_task_resource(self.task_run_id).await?;
        if status.project_id != self.project_id
            || status.owner_id != self.owner_id
            || !matches!(
                status.request.target,
                ResourceTarget::Task { task_run_id } if task_run_id == self.task_run_id
            )
        {
            return Err(TaskResourceError::IdentityMismatch);
        }
        Ok(status)
    }

    /// Releases the Resource claim using the latest revision fence.
    pub async fn release(
        &self,
        status: &contracts::http::TaskResourceStatus,
    ) -> Result<contracts::http::TaskResourceStatus, TaskResourceError> {
        if status.task_run_id != self.task_run_id
            || status.project_id != self.project_id
            || status.owner_id != self.owner_id
        {
            return Err(TaskResourceError::IdentityMismatch);
        }
        let request = ReleaseTaskResourceRequest {
            expected_claim_revision: status.claim_revision,
            expected_lease_revision: status.lease_revision,
        };
        self.client
            .release_task_resource(self.task_run_id, &request)
            .await
            .map_err(Into::into)
    }
}

fn valid_execution_namespace(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !value.starts_with('-')
        && !value.ends_with('-')
}

fn validate_request(
    current: &ResourceRequest,
    expected: &InternalCreateTaskResourceRequest,
) -> Result<(), TaskResourceError> {
    current
        .validate()
        .map_err(|_| TaskResourceError::ResponseInvalid)?;
    if current.request_key != expected.request_key
        || current.requester_id != expected.owner_id
        || current.project_id != expected.project_id
        || current.course_id != expected.course_id
        || current.requested_resources != expected.resources
        || current.requested_duration_seconds != expected.duration_seconds
        || !matches!(
            current.target,
            ResourceTarget::Task { task_run_id } if task_run_id == expected.task_run_id
        )
    {
        return Err(TaskResourceError::IdentityMismatch);
    }
    Ok(())
}

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

/// Actual Kubernetes main-container timing observed from Pod status.
///
/// Kubernetes may omit either boundary while a Job is being deleted or when the
/// kubelet did not publish a complete status.  Callers must preserve that
/// uncertainty as an unknown usage measurement instead of inventing an interval.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExecutionTiming {
    pub started_at: Option<UtcTimestamp>,
    pub terminated_at: Option<UtcTimestamp>,
}

impl ExecutionTiming {
    #[must_use]
    pub const fn unknown() -> Self {
        Self {
            started_at: None,
            terminated_at: None,
        }
    }

    pub fn validate(self) -> Result<(), ExecutionError> {
        if self.started_at.is_some() != self.terminated_at.is_some()
            || self
                .started_at
                .zip(self.terminated_at)
                .is_some_and(|(started, terminated)| terminated <= started)
        {
            return Err(ExecutionError::Backend(
                "execution_timing_invalid".to_owned(),
            ));
        }
        Ok(())
    }
}

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
    TaskResource(#[from] TaskResourceError),
}

/// Failures at a Resource task reservation boundary.
#[derive(Debug, Error)]
pub enum TaskResourceError {
    #[error("LW_EVALUATION_TASK_RESOURCE_REQUEST_INVALID")]
    RequestInvalid,
    #[error("LW_EVALUATION_TASK_RESOURCE_RESPONSE_INVALID")]
    ResponseInvalid,
    #[error("LW_EVALUATION_TASK_RESOURCE_IDENTITY_MISMATCH")]
    IdentityMismatch,
    #[error("LW_EVALUATION_TASK_RESOURCE_TERMINAL")]
    ResourceTerminal,
    #[error("LW_EVALUATION_TASK_RESOURCE_APPROVAL_TIMEOUT")]
    ResourceApprovalTimeout,
    #[error("LW_EVALUATION_TASK_RESOURCE_CANCELLED")]
    Cancelled,
    #[error("LW_EVALUATION_TASK_RESOURCE_CREATE_UNCERTAIN")]
    CreateUncertain,
    #[error(transparent)]
    Client(#[from] ResourceClientError),
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
            run_argv: vec!["runner".to_owned(), "{binary}".to_owned()],
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
            vec!["runner".to_owned(), binary.to_string_lossy().to_string()]
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
