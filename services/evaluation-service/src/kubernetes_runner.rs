//! Production Evaluation attempt runner.
//!
//! This module is the single orchestration boundary between the durable
//! Evaluation step lease, Resource's one-shot reservation, immutable object
//! storage, and the attempt-scoped Kubernetes executors.  It deliberately
//! keeps no attempt state outside `PostgreSQL`: a worker restart resumes the
//! existing `TaskRunId` through `claim_next_step` and the executor reuses only
//! a Job whose immutable request identity matches the attempt.

#![allow(missing_docs, clippy::too_many_lines)]

use std::{
    collections::{BTreeMap, BTreeSet},
    str::FromStr,
    sync::Arc,
    time::Duration,
};

use artifact_store::{ImmutableObjectStore, S3ImmutableObjectStore};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use contracts::{
    EventId,
    authoring::{PackageFile, ProblemPackage},
    evaluation::{
        AdvisoryOutputMode, ApprovedProgramProfile, EvaluationStepCompletion, ProgramPhase,
    },
    http::RecordResourceUsageRequest,
    http::{
        AgentLlmReviewFile, AgentLlmReviewRubric, AgentLlmReviewState,
        InternalAgentLlmReviewRequest,
    },
    resource::{ResourceUsageKind, ResourceUsageQuantities, UsageMeasurement, WorkloadResources},
    submission::FrozenSubmission,
};
use persistence_sqlx::Sha256Digest;
use serde::{Deserialize, Serialize};
use tokio::time::Instant;
use uuid::Uuid;

use crate::agent_client::{AgentClient, AgentClientError};
use crate::ansible_probe::{
    ANSIBLE_PROBE_EXECUTION_SCHEMA_VERSION, AnsibleProbeExecutionLimits,
    AnsibleProbeExecutionRequest, AnsibleProbeSshIdentity, AnsibleProbeTarget,
};
use crate::ansible_probe_executor::{
    AnsibleProbeExecutorConfiguration, AnsibleProbeJobObservation, AnsibleProbeKubernetesExecutor,
};
use crate::ansible_probe_job::AnsibleProbeJobBinding;
use crate::ansible_probe_job::AnsibleProbeJobResources;
use crate::authoring_client::AuthoringAdmissionClient;
use crate::control_plane::PgEvaluationControlStore;
use crate::control_plane::{
    EvaluationExecutionKind, EvaluationExecutionObjectRef, EvaluationExecutionResources,
};
use crate::environment_client::{
    EnvironmentExecutionBindingClient, EnvironmentExecutionBindingClientError,
    ResolvedEnvironmentExecutionBinding,
};
use crate::execution::{
    EvaluationAttemptContext, EvaluationAttemptRunner, ExecutionError, ExecutionTiming,
    StepExecutionPlan, TaskResourceError, TaskResourceLifecycle,
};
use crate::freeze_store::PgFreezeStore;
use crate::materializer::{
    ARTIFACT_MATERIALIZER_SCHEMA_VERSION, FROZEN_ARCHIVE_MEDIA_TYPE, MaterializeArtifact,
    MaterializeCommand, MaterializeContent, MaterializeDestination,
};
use crate::oj::{
    OjCaseBinding, OjCheckerKind, OjExecutionLimits, OjExecutionPhase, OjExecutionRequest,
    OjFileBinding, OjTerminalStatus,
};
use crate::oj_executor::OjJobObservation;
use crate::oj_executor::{OjExecutorConfiguration, OjKubernetesExecutor};
use crate::oj_job::{OjJobBinding, OjJobResources};
use crate::resource_client::ResourceClient;

const DEFAULT_RESOURCE_CPU_MILLICORES: u32 = 1_000;
const DEFAULT_RESOURCE_STORAGE_BYTES: u64 = 128 * 1024 * 1024;
const DEFAULT_ANSIBLE_MEMORY_BYTES: u64 = 512 * 1024 * 1024;
const DEFAULT_ANSIBLE_FACTS_BYTES: u64 = 4 * 1024 * 1024;
const DEFAULT_ANSIBLE_OUTPUT_BYTES: u64 = 1024 * 1024;
const DEFAULT_ANSIBLE_MAX_ASSERTIONS: u32 = 32;

enum StartedExecution {
    Program {
        resources: OjJobResources,
        request: OjExecutionRequest,
        recovery: EvaluationExecutionResources,
    },
    AnsibleProbe {
        resources: AnsibleProbeJobResources,
        request: AnsibleProbeExecutionRequest,
        recovery: EvaluationExecutionResources,
    },
}

struct ObservedExecution {
    result: TerminalResult,
    timing: ExecutionTiming,
}

enum RecoveryExecution {
    Program {
        request: OjExecutionRequest,
    },
    AnsibleProbe {
        request: AnsibleProbeExecutionRequest,
    },
}

impl StartedExecution {
    fn recovery(&self) -> &EvaluationExecutionResources {
        match self {
            Self::Program { recovery, .. } | Self::AnsibleProbe { recovery, .. } => recovery,
        }
    }
}

impl ExecutionTiming {
    fn boundaries(
        self,
    ) -> Result<
        (
            Option<contracts::UtcTimestamp>,
            Option<contracts::UtcTimestamp>,
        ),
        ExecutionError,
    > {
        self.validate()?;
        Ok((self.started_at, self.terminated_at))
    }
}

/// Resource limits for a read-only Ansible probe.
///
/// The VM target and SSH credentials are resolved from Environment for each
/// attempt.  Only bounded execution limits remain deployment configuration.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AnsibleProbeTargetConfiguration {
    #[serde(default = "default_ansible_wall_time_seconds")]
    pub wall_time_seconds: u64,
    #[serde(default = "default_ansible_facts_bytes")]
    pub facts_max_bytes: u64,
    #[serde(default = "default_ansible_output_bytes")]
    pub output_max_bytes: u64,
    #[serde(default = "default_ansible_max_assertions")]
    pub max_assertions: u32,
}

fn default_ansible_wall_time_seconds() -> u64 {
    300
}

const fn default_ansible_facts_bytes() -> u64 {
    DEFAULT_ANSIBLE_FACTS_BYTES
}

const fn default_ansible_output_bytes() -> u64 {
    DEFAULT_ANSIBLE_OUTPUT_BYTES
}

const fn default_ansible_max_assertions() -> u32 {
    DEFAULT_ANSIBLE_MAX_ASSERTIONS
}

/// Process configuration for the concrete Kubernetes runner.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvaluationExecutionConfiguration {
    pub runner_namespace: String,
    pub worker_id: String,
    pub worker_lease_seconds: u64,
    pub scheduler_poll_interval_milliseconds: u64,
    pub oj_service_account_name: String,
    pub ansible_probe_service_account_name: String,
    pub image_pull_secret_name: String,
    pub resource_poll_interval_milliseconds: u64,
    pub resource_approval_timeout_seconds: u64,
    pub execution_observe_poll_interval_milliseconds: u64,
    pub cleanup_timeout_seconds: u64,
    pub environment: crate::environment_client::EnvironmentExecutionBindingClientConfiguration,
    pub ansible_probe: AnsibleProbeTargetConfiguration,
    pub oj: OjExecutorConfiguration,
    pub ansible_probe_executor: AnsibleProbeExecutorConfiguration,
}

impl EvaluationExecutionConfiguration {
    fn validate(&self) -> Result<(), ExecutionError> {
        if self.runner_namespace.trim().is_empty()
            || self.worker_id.trim().is_empty()
            || self.worker_id.len() > 96
            || self.worker_id.chars().any(char::is_control)
            || !(30..=1_800).contains(&self.worker_lease_seconds)
            || !(100..=30_000).contains(&self.scheduler_poll_interval_milliseconds)
            || self.oj_service_account_name.trim().is_empty()
            || self.ansible_probe_service_account_name.trim().is_empty()
            || self.image_pull_secret_name.trim().is_empty()
            || !(100..=30_000).contains(&self.resource_poll_interval_milliseconds)
            || !(1..=3_600).contains(&self.resource_approval_timeout_seconds)
            || !(100..=30_000).contains(&self.execution_observe_poll_interval_milliseconds)
            || !(1..=3_600).contains(&self.cleanup_timeout_seconds)
            || self.oj.runner_namespace != self.runner_namespace
            || self.ansible_probe_executor.runner_namespace != self.runner_namespace
        {
            return Err(ExecutionError::WorkerConfigurationInvalid);
        }
        Ok(())
    }
}

/// Concrete evaluator that uses Resource and the audited Kubernetes Jobs.
#[derive(Clone)]
pub struct KubernetesEvaluationRunner {
    control: PgEvaluationControlStore,
    freezes: PgFreezeStore,
    objects: Arc<S3ImmutableObjectStore>,
    resource: ResourceClient,
    agent: AgentClient,
    authoring: AuthoringAdmissionClient,
    environment: EnvironmentExecutionBindingClient,
    oj: OjKubernetesExecutor,
    ansible_probe: AnsibleProbeKubernetesExecutor,
    configuration: EvaluationExecutionConfiguration,
    materializer_ca_bundle: Option<Arc<[u8]>>,
}

impl std::fmt::Debug for KubernetesEvaluationRunner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("KubernetesEvaluationRunner")
            .field("configuration", &self.configuration)
            .finish_non_exhaustive()
    }
}

impl KubernetesEvaluationRunner {
    /// Constructs the production runner from validated downstream bindings.
    ///
    /// # Errors
    ///
    /// Returns an error when the execution configuration or either Kubernetes
    /// executor cannot be validated.
    #[allow(
        clippy::too_many_arguments,
        reason = "the runner constructor binds all independently configured domain clients"
    )]
    pub fn new(
        control: PgEvaluationControlStore,
        freezes: PgFreezeStore,
        objects: Arc<S3ImmutableObjectStore>,
        resource: ResourceClient,
        agent: AgentClient,
        authoring: AuthoringAdmissionClient,
        environment: EnvironmentExecutionBindingClient,
        configuration: EvaluationExecutionConfiguration,
        materializer_ca_bundle: Option<Arc<[u8]>>,
    ) -> Result<Self, ExecutionError> {
        configuration.validate()?;
        let oj = OjKubernetesExecutor::new(configuration.oj.clone()).map_err(|error| {
            tracing::error!(
                event = "evaluation.executor.configuration_invalid",
                executor = "oj",
                diagnostic_code = error.diagnostic_code(),
                error = %error,
                "OJ executor configuration validation failed",
            );
            ExecutionError::WorkerConfigurationInvalid
        })?;
        let ansible_probe =
            AnsibleProbeKubernetesExecutor::new(configuration.ansible_probe_executor.clone())
                .map_err(|error| {
                    tracing::error!(
                        event = "evaluation.executor.configuration_invalid",
                        executor = "ansible_probe",
                        diagnostic_code = error.diagnostic_code(),
                        error = %error,
                        "Ansible probe executor configuration validation failed",
                    );
                    ExecutionError::WorkerConfigurationInvalid
                })?;
        Ok(Self {
            control,
            freezes,
            objects,
            resource,
            agent,
            authoring,
            environment,
            oj,
            ansible_probe,
            configuration,
            materializer_ca_bundle,
        })
    }

    async fn execute_attempt(
        &self,
        context: EvaluationAttemptContext,
    ) -> Result<EvaluationStepCompletion, ExecutionError> {
        let plan = context.execution_plan.clone();
        let frozen = self
            .freezes
            .load_completed(
                context.run.frozen_submission_id,
                context.run.project_id,
                context.run.course_id,
                context.run.actor_id,
            )
            .await
            .map_err(|_| ExecutionError::Backend("frozen_submission_load".to_owned()))?;
        frozen
            .validate()
            .map_err(|_| ExecutionError::IdentityMismatch)?;
        if let StepExecutionPlan::Advisory {
            include,
            rubric,
            output_mode,
        } = &plan
        {
            return self
                .execute_advisory(&context, &frozen, include, rubric, *output_mode)
                .await;
        }
        if let StepExecutionPlan::FileAssertion { required_files } = &plan {
            if context.cancellation.is_cancelled()
                || self.run_is_cancelling(context.lease.run_id).await?
            {
                return TerminalResult::Cancelled.into_completion();
            }
            return Self::execute_file_assertion(&frozen, required_files).into_completion();
        }
        let lifecycle = self.resource_lifecycle(&context, &plan)?;
        if let Some(checkpoint) = self
            .control
            .load_execution_checkpoint(&context.lease)
            .await
            .map_err(ExecutionError::Control)?
        {
            return self
                .resume_attempt(&context, &plan, &lifecycle, checkpoint)
                .await;
        }
        // Resource creation is a one-shot POST.  Await its bounded future before observing
        // cancellation so an in-flight request cannot be dropped and then commit after a GET
        // reports Missing.  A timeout/transport error remains non-terminal; the durable
        // TaskRunId and idempotency key are reconciled by the next worker attempt.
        lifecycle
            .create()
            .await
            .map_err(|error| map_task_resource(&error, "create"))?;
        // A reassigned attempt may already own a Resource request from the previous worker. If
        // cancellation won while it was still before the execution Job, settle that exact
        // TaskRunId before claiming or acknowledging anything. This covers Reviewing,
        // Allocating, and an already-active handoff.
        if context.cancellation.is_cancelled()
            || self.run_is_cancelling(context.lease.run_id).await?
        {
            lifecycle
                .cancel("evaluation step cancelled before execution Job start")
                .await
                .map_err(|error| map_task_resource(&error, "cancel"))?;
            if context.lease_lost.is_cancelled() {
                return Err(ExecutionError::LeaseLost);
            }
            return TerminalResult::Cancelled.into_completion();
        }
        let claimed = match lifecycle
            .claim_after_approval(
                Duration::from_millis(self.configuration.resource_poll_interval_milliseconds),
                Duration::from_secs(self.configuration.resource_approval_timeout_seconds),
                &context.cancellation,
            )
            .await
        {
            Ok(status) => status,
            Err(TaskResourceError::Cancelled) if context.cancellation.is_cancelled() => {
                if context.lease_lost.is_cancelled() {
                    return Err(ExecutionError::LeaseLost);
                }
                if self.run_is_cancelling(context.lease.run_id).await? {
                    return TerminalResult::Cancelled.into_completion();
                }
                return Err(ExecutionError::LeaseLost);
            }
            Err(TaskResourceError::ResourceApprovalTimeout) => {
                return TerminalResult::Failed(
                    "LW_EVALUATION_RESOURCE_APPROVAL_TIMEOUT".to_owned(),
                )
                .into_completion();
            }
            Err(TaskResourceError::ResourceTerminal) => {
                return TerminalResult::Failed("LW_EVALUATION_RESOURCE_TERMINAL".to_owned())
                    .into_completion();
            }
            Err(error) => return Err(map_task_resource(&error, "claim")),
        };
        let mut resource_status = tokio::select! {
            result = lifecycle.acknowledge(&claimed, &self.configuration.runner_namespace) => {
                result.map_err(|error| map_task_resource(&error, "ack"))?
            }
            () = context.cancellation.cancelled() => {
                if context.lease_lost.is_cancelled() {
                    return Err(ExecutionError::LeaseLost);
                }
                lifecycle
                    .cancel("evaluation step lease cancelled before resource handoff")
                    .await
                    .map_err(|error| map_task_resource(&error, "cancel"))?;
                if self.run_is_cancelling(context.lease.run_id).await? {
                    return TerminalResult::Cancelled.into_completion();
                }
                return Err(ExecutionError::LeaseLost);
            }
        };
        if context.lease_lost.is_cancelled() {
            return Err(ExecutionError::LeaseLost);
        }
        if context.cancellation.is_cancelled()
            || self.run_is_cancelling(context.lease.run_id).await?
        {
            let status = lifecycle
                .release(&resource_status)
                .await
                .map_err(|error| map_task_resource(&error, "release"))?;
            if !status.cleanup_confirmed {
                return Err(ExecutionError::Backend(
                    "resource_cleanup_not_confirmed".to_owned(),
                ));
            }
            return TerminalResult::Cancelled.into_completion();
        }
        let started = match plan {
            StepExecutionPlan::Program {
                toolchain_profile,
                phase,
                input,
                test_groups,
                limits,
            } => {
                self.start_program(
                    &context,
                    &frozen,
                    &toolchain_profile,
                    phase,
                    &input,
                    &test_groups,
                    limits,
                )
                .await?
            }
            StepExecutionPlan::AnsibleProbe {
                playbook_profile,
                module_allowlist,
                assertions,
            } => {
                self.start_ansible_probe(
                    &context,
                    &frozen,
                    &playbook_profile,
                    &module_allowlist,
                    &assertions,
                )
                .await?
            }
            StepExecutionPlan::Advisory { .. } => {
                return Err(ExecutionError::Backend(
                    "advisory_resource_mismatch".to_owned(),
                ));
            }
            StepExecutionPlan::FileAssertion { .. } => {
                return Err(ExecutionError::Backend(
                    "file_assertion_resource_mismatch".to_owned(),
                ));
            }
        };
        let recovery = started.recovery().clone();
        self.control
            .mark_execution_started(&context.lease, None, &recovery)
            .await
            .map_err(ExecutionError::Control)?;
        let observed = self.observe_started(&context, &started).await?;
        if context.lease_lost.is_cancelled() {
            return Err(ExecutionError::LeaseLost);
        }
        let terminal_at = self
            .control
            .authority_now()
            .await
            .map_err(ExecutionError::Control)?;
        let (execution_started_at, execution_terminated_at) = observed.timing.boundaries()?;
        let deliveries = Self::usage_deliveries(
            &resource_status,
            observed.timing,
            execution_terminated_at.unwrap_or(terminal_at),
        )?;
        let completion = observed.result.into_completion()?;
        self.control
            .checkpoint_execution_terminal(
                &context.lease,
                execution_started_at,
                execution_terminated_at,
                &completion,
                &deliveries,
            )
            .await
            .map_err(ExecutionError::Control)?;
        self.cleanup_recovery(&context, &recovery).await?;
        resource_status = lifecycle
            .release(&resource_status)
            .await
            .map_err(|error| map_task_resource(&error, "release"))?;
        if !resource_status.cleanup_confirmed {
            return Err(ExecutionError::Backend(
                "resource_cleanup_not_confirmed".to_owned(),
            ));
        }
        Ok(completion)
    }

    async fn resume_attempt(
        &self,
        context: &EvaluationAttemptContext,
        plan: &StepExecutionPlan,
        lifecycle: &TaskResourceLifecycle,
        mut checkpoint: crate::control_plane::EvaluationExecutionCheckpoint,
    ) -> Result<EvaluationStepCompletion, ExecutionError> {
        let mut recovery = checkpoint
            .execution_resources
            .clone()
            .ok_or_else(|| ExecutionError::Backend("execution_resources_missing".to_owned()))?;
        let expected_kind = match plan {
            StepExecutionPlan::Program { .. } => EvaluationExecutionKind::Program,
            StepExecutionPlan::AnsibleProbe { .. } => EvaluationExecutionKind::AnsibleProbe,
            StepExecutionPlan::Advisory { .. } => EvaluationExecutionKind::LlmReview,
            StepExecutionPlan::FileAssertion { .. } => {
                return Err(ExecutionError::Backend(
                    "file_assertion_checkpoint_invalid".to_owned(),
                ));
            }
        };
        if recovery.kind != expected_kind {
            return Err(ExecutionError::IdentityMismatch);
        }
        // A worker may have crashed after one or more immutable objects were
        // applied but before their UIDs reached PostgreSQL.  Discover only
        // the deterministic names from the persisted request and promote all
        // verified objects before observing or cleaning them.
        if recovery.objects.is_empty()
            && let Some(hydrated) = self.hydrate_execution_intent(context, &recovery).await?
        {
            self.control
                .mark_execution_started(&context.lease, None, &hydrated)
                .await
                .map_err(ExecutionError::Control)?;
            recovery = hydrated;
            checkpoint.execution_resources = Some(recovery.clone());
        }
        let mut status = lifecycle
            .load_status()
            .await
            .map_err(|error| map_task_resource(&error, "load"))?;
        if let Some(completion) = checkpoint.terminal_completion {
            self.cleanup_recovery(context, &recovery).await?;
            if !status.cleanup_confirmed {
                status = lifecycle
                    .release(&status)
                    .await
                    .map_err(|error| map_task_resource(&error, "release"))?;
            }
            if !status.cleanup_confirmed {
                return Err(ExecutionError::Backend(
                    "resource_cleanup_not_confirmed".to_owned(),
                ));
            }
            return Ok(completion);
        }
        if context.cancellation.is_cancelled()
            || self.run_is_cancelling(context.lease.run_id).await?
        {
            return self
                .cancel_recovered_attempt(context, lifecycle, &recovery, &checkpoint)
                .await;
        }
        let recovery_execution = Self::rebuild_recovery_execution(context, plan, &recovery)?;
        let observed = self
            .observe_recovery_execution(context, &recovery_execution, &recovery)
            .await?;
        if context.lease_lost.is_cancelled() {
            return Err(ExecutionError::LeaseLost);
        }
        let terminal_at = self
            .control
            .authority_now()
            .await
            .map_err(ExecutionError::Control)?;
        let (observed_started_at, observed_terminated_at) = observed.timing.boundaries()?;
        let execution_started_at = checkpoint.execution_started_at.or(observed_started_at);
        let execution_terminated_at = checkpoint
            .execution_terminated_at
            .or(observed_terminated_at);
        let usage_timing = match (execution_started_at, execution_terminated_at) {
            (Some(started_at), Some(terminated_at)) => ExecutionTiming {
                started_at: Some(started_at),
                terminated_at: Some(terminated_at),
            },
            (None, None) => observed.timing,
            _ => {
                return Err(ExecutionError::Backend(
                    "execution_timing_invalid".to_owned(),
                ));
            }
        };
        let deliveries = Self::usage_deliveries(
            &status,
            usage_timing,
            execution_terminated_at.unwrap_or(terminal_at),
        )?;
        let completion = observed.result.into_completion()?;
        self.control
            .checkpoint_execution_terminal(
                &context.lease,
                execution_started_at,
                execution_terminated_at,
                &completion,
                &deliveries,
            )
            .await
            .map_err(ExecutionError::Control)?;
        self.cleanup_recovery(context, &recovery).await?;
        status = lifecycle
            .release(&status)
            .await
            .map_err(|error| map_task_resource(&error, "release"))?;
        if !status.cleanup_confirmed {
            return Err(ExecutionError::Backend(
                "resource_cleanup_not_confirmed".to_owned(),
            ));
        }
        Ok(completion)
    }

    async fn cancel_recovered_attempt(
        &self,
        context: &EvaluationAttemptContext,
        lifecycle: &TaskResourceLifecycle,
        recovery: &EvaluationExecutionResources,
        checkpoint: &crate::control_plane::EvaluationExecutionCheckpoint,
    ) -> Result<EvaluationStepCompletion, ExecutionError> {
        if context.lease_lost.is_cancelled() {
            return Err(ExecutionError::LeaseLost);
        }
        let mut recovery = recovery.clone();
        let mut checkpoint = checkpoint.clone();
        if recovery.objects.is_empty()
            && let Some(hydrated) = self.hydrate_execution_intent(context, &recovery).await?
        {
            self.control
                .mark_execution_started(&context.lease, None, &hydrated)
                .await
                .map_err(ExecutionError::Control)?;
            recovery = hydrated;
            checkpoint.execution_resources = Some(recovery.clone());
        }
        let mut status = lifecycle
            .load_status()
            .await
            .map_err(|error| map_task_resource(&error, "load"))?;
        let terminal_at = self
            .control
            .authority_now()
            .await
            .map_err(ExecutionError::Control)?;
        let completion = TerminalResult::Cancelled.into_completion()?;
        let timing = match (
            checkpoint.execution_started_at,
            checkpoint.execution_terminated_at,
        ) {
            (Some(started_at), Some(terminated_at)) => ExecutionTiming {
                started_at: Some(started_at),
                terminated_at: Some(terminated_at),
            },
            (None, None) => ExecutionTiming::unknown(),
            _ => {
                return Err(ExecutionError::Backend(
                    "execution_timing_invalid".to_owned(),
                ));
            }
        };
        let (execution_started_at, execution_terminated_at) = timing.boundaries()?;
        let deliveries = Self::usage_deliveries(
            &status,
            timing,
            execution_terminated_at.unwrap_or(terminal_at),
        )?;
        self.control
            .checkpoint_execution_terminal(
                &context.lease,
                execution_started_at,
                execution_terminated_at,
                &completion,
                &deliveries,
            )
            .await
            .map_err(ExecutionError::Control)?;
        self.cleanup_recovery(context, &recovery).await?;
        status = lifecycle
            .release(&status)
            .await
            .map_err(|error| map_task_resource(&error, "release"))?;
        if !status.cleanup_confirmed {
            return Err(ExecutionError::Backend(
                "resource_cleanup_not_confirmed".to_owned(),
            ));
        }
        Ok(completion)
    }

    async fn hydrate_execution_intent(
        &self,
        context: &EvaluationAttemptContext,
        recovery: &EvaluationExecutionResources,
    ) -> Result<Option<EvaluationExecutionResources>, ExecutionError> {
        let objects = match recovery.kind {
            EvaluationExecutionKind::Program => {
                let request = parse_program_recovery_request(recovery, context)?;
                self.oj
                    .capture_intent_object_refs(recovery, &request)
                    .await
                    .map_err(|_| ExecutionError::Backend("oj_intent_capture_failed".to_owned()))?
            }
            EvaluationExecutionKind::AnsibleProbe => {
                let request = parse_probe_recovery_request(recovery, context)?;
                self.ansible_probe
                    .capture_intent_object_refs(recovery, &request)
                    .await
                    .map_err(|_| {
                        ExecutionError::Backend("probe_intent_capture_failed".to_owned())
                    })?
            }
            EvaluationExecutionKind::LlmReview => return Ok(None),
        };
        let Some(objects) = objects else {
            return Ok(None);
        };
        let mut hydrated = recovery.clone();
        hydrated.objects = objects
            .into_iter()
            .map(
                |(api_version, resource, name, uid)| EvaluationExecutionObjectRef {
                    api_version,
                    resource,
                    name,
                    uid,
                },
            )
            .collect();
        hydrated
            .validate_for(
                context.lease.run_id,
                context.lease.step_run_id,
                context.lease.task_run_id,
            )
            .map_err(|_| ExecutionError::Backend("execution_resources_invalid".to_owned()))?;
        Ok(Some(hydrated))
    }

    fn rebuild_recovery_execution(
        context: &EvaluationAttemptContext,
        plan: &StepExecutionPlan,
        recovery: &EvaluationExecutionResources,
    ) -> Result<RecoveryExecution, ExecutionError> {
        match plan {
            StepExecutionPlan::Program { .. } => Ok(RecoveryExecution::Program {
                request: parse_program_recovery_request(recovery, context)?,
            }),
            StepExecutionPlan::AnsibleProbe { .. } => Ok(RecoveryExecution::AnsibleProbe {
                request: parse_probe_recovery_request(recovery, context)?,
            }),
            StepExecutionPlan::Advisory { .. } => Err(ExecutionError::Backend(
                "advisory_recovery_invalid".to_owned(),
            )),
            StepExecutionPlan::FileAssertion { .. } => Err(ExecutionError::Backend(
                "file_assertion_recovery_invalid".to_owned(),
            )),
        }
    }

    async fn observe_started(
        &self,
        context: &EvaluationAttemptContext,
        started: &StartedExecution,
    ) -> Result<ObservedExecution, ExecutionError> {
        match started {
            StartedExecution::Program {
                resources, request, ..
            } => {
                let (result, timing) = self
                    .observe_oj(context, Some(resources), request, None)
                    .await?;
                Ok(ObservedExecution { result, timing })
            }
            StartedExecution::AnsibleProbe {
                resources, request, ..
            } => {
                let (result, timing) = self
                    .observe_probe(context, Some(resources), request, None)
                    .await?;
                Ok(ObservedExecution { result, timing })
            }
        }
    }

    async fn observe_recovery_execution(
        &self,
        context: &EvaluationAttemptContext,
        execution: &RecoveryExecution,
        recovery: &EvaluationExecutionResources,
    ) -> Result<ObservedExecution, ExecutionError> {
        match execution {
            RecoveryExecution::Program { request } => {
                let (result, timing) = self
                    .observe_oj(context, None, request, Some(recovery))
                    .await?;
                Ok(ObservedExecution { result, timing })
            }
            RecoveryExecution::AnsibleProbe { request } => {
                let (result, timing) = self
                    .observe_probe(context, None, request, Some(recovery))
                    .await?;
                Ok(ObservedExecution { result, timing })
            }
        }
    }

    async fn cleanup_recovery(
        &self,
        context: &EvaluationAttemptContext,
        recovery: &EvaluationExecutionResources,
    ) -> Result<(), ExecutionError> {
        let deadline =
            Instant::now() + Duration::from_secs(self.configuration.cleanup_timeout_seconds);
        loop {
            if context.lease_lost.is_cancelled() {
                return Err(ExecutionError::LeaseLost);
            }
            let complete = match recovery.kind {
                EvaluationExecutionKind::Program => self
                    .oj
                    .cleanup_recovery(recovery)
                    .await
                    .map_err(|_| ExecutionError::Backend("oj_cleanup_failed".to_owned()))?,
                EvaluationExecutionKind::AnsibleProbe => self
                    .ansible_probe
                    .cleanup_recovery(recovery)
                    .await
                    .map_err(|_| ExecutionError::Backend("probe_cleanup_failed".to_owned()))?,
                EvaluationExecutionKind::LlmReview => true,
            };
            if complete {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(ExecutionError::Backend(
                    "execution_cleanup_pending".to_owned(),
                ));
            }
            tokio::time::sleep(Duration::from_millis(
                self.configuration
                    .execution_observe_poll_interval_milliseconds,
            ))
            .await;
        }
    }

    fn resource_lifecycle(
        &self,
        context: &EvaluationAttemptContext,
        plan: &StepExecutionPlan,
    ) -> Result<TaskResourceLifecycle, ExecutionError> {
        let memory_bytes = match plan {
            StepExecutionPlan::Program { limits, .. } => limits.memory_bytes(),
            StepExecutionPlan::FileAssertion { .. } => 128 * 1024 * 1024,
            StepExecutionPlan::AnsibleProbe { .. } => DEFAULT_ANSIBLE_MEMORY_BYTES,
            StepExecutionPlan::Advisory { .. } => {
                return Err(ExecutionError::Backend(
                    "advisory_resource_mismatch".to_owned(),
                ));
            }
        };
        let duration_seconds = match plan {
            StepExecutionPlan::Program {
                limits,
                test_groups,
                ..
            } => {
                let groups = u64::try_from(test_groups.len())
                    .map_err(|_| ExecutionError::WorkerConfigurationInvalid)?;
                limits
                    .wall_time_seconds()
                    .checked_mul(groups.max(1))
                    .and_then(|seconds| seconds.checked_add(60))
                    .ok_or(ExecutionError::WorkerConfigurationInvalid)?
            }
            StepExecutionPlan::FileAssertion { .. } => 60,
            StepExecutionPlan::AnsibleProbe { .. } => self
                .configuration
                .ansible_probe
                .wall_time_seconds
                .checked_add(30)
                .ok_or(ExecutionError::WorkerConfigurationInvalid)?,
            StepExecutionPlan::Advisory { .. } => {
                return Err(ExecutionError::Backend(
                    "advisory_resource_mismatch".to_owned(),
                ));
            }
        };
        let resources = WorkloadResources {
            cpu_millicores: DEFAULT_RESOURCE_CPU_MILLICORES,
            memory_bytes,
            storage_bytes: DEFAULT_RESOURCE_STORAGE_BYTES,
            gpu: None,
        };
        let request_key = format!("evaluation-{}", context.lease.task_run_id);
        TaskResourceLifecycle::new(
            self.resource.clone(),
            context.lease.task_run_id,
            context.run.project_id,
            context.run.course_id,
            context.run.actor_id,
            request_key,
            resources,
            duration_seconds,
        )
        .map_err(ExecutionError::TaskResource)
    }

    fn execute_file_assertion(
        frozen: &FrozenSubmission,
        required_files: &[String],
    ) -> TerminalResult {
        let files = frozen
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<BTreeSet<_>>();
        if required_files
            .iter()
            .all(|path| files.contains(path.as_str()))
        {
            TerminalResult::Succeeded { score: None }
        } else {
            TerminalResult::Failed("LW_EVALUATION_REQUIRED_FILE_MISSING".to_owned())
        }
    }

    async fn execute_advisory(
        &self,
        context: &EvaluationAttemptContext,
        frozen: &FrozenSubmission,
        include: &[String],
        rubric: &str,
        output_mode: AdvisoryOutputMode,
    ) -> Result<EvaluationStepCompletion, ExecutionError> {
        let checkpoint = self
            .control
            .load_execution_checkpoint(&context.lease)
            .await
            .map_err(ExecutionError::Control)?;
        let (request, checkpoint) = if let Some(checkpoint) = checkpoint {
            let resources = checkpoint
                .execution_resources
                .as_ref()
                .ok_or_else(|| ExecutionError::Backend("advisory_intent_missing".to_owned()))?;
            if resources.kind != EvaluationExecutionKind::LlmReview {
                return Err(ExecutionError::IdentityMismatch);
            }
            let request = parse_advisory_recovery_request(resources, context)?;
            (request, Some(checkpoint))
        } else {
            let request = self
                .build_advisory_request(context, frozen, include, rubric, output_mode)
                .await?;
            let intent = execution_resources(
                context,
                self.configuration.runner_namespace.as_str(),
                EvaluationExecutionKind::LlmReview,
                &request,
                Vec::new(),
            )?;
            self.control
                .persist_execution_intent(&context.lease, &intent)
                .await
                .map_err(ExecutionError::Control)?;
            (request, None)
        };
        if let Some(checkpoint) = checkpoint
            && let Some(completion) = checkpoint.terminal_completion
        {
            return Ok(completion);
        }
        let key = format!("evaluation-llm-{}", context.lease.task_run_id);
        let cancel_key = format!("{key}-cancel");
        let mut receipt = self.load_or_enqueue_advisory(&request, &key).await?;
        let (receipt, started_at, finished_at) = self
            .observe_advisory(context, &request, &cancel_key, &mut receipt)
            .await?;
        if context.lease_lost.is_cancelled() {
            return Err(ExecutionError::LeaseLost);
        }
        let allowed_paths = request
            .files
            .iter()
            .map(|file| file.path.clone())
            .collect::<Vec<_>>();
        let completion = advisory_completion(receipt, &allowed_paths)?;
        let resources = execution_resources(
            context,
            self.configuration.runner_namespace.as_str(),
            EvaluationExecutionKind::LlmReview,
            &request,
            Vec::new(),
        )?;
        self.control
            .mark_execution_started(&context.lease, None, &resources)
            .await
            .map_err(ExecutionError::Control)?;
        self.control
            .checkpoint_execution_terminal(
                &context.lease,
                started_at,
                finished_at,
                &completion,
                &[],
            )
            .await
            .map_err(ExecutionError::Control)?;
        Ok(completion)
    }

    /// Reconciles the Agent-owned request before attempting a create.
    ///
    /// The execution intent is durable before this method runs.  A worker can therefore be
    /// restarted after Agent committed the review and before Evaluation observed the response;
    /// the exact receipt must be loaded first, even when its request deadline has elapsed.  Only
    /// an explicit missing receipt permits the original immutable request to be posted.
    async fn load_or_enqueue_advisory(
        &self,
        request: &InternalAgentLlmReviewRequest,
        idempotency_key: &str,
    ) -> Result<contracts::http::InternalAgentLlmReviewReceipt, ExecutionError> {
        let expected_request_sha256 = Sha256Digest::of_canonical(request)
            .map_err(|_| ExecutionError::Backend("advisory_request_invalid".to_owned()))?;
        let receipt = match self
            .agent
            .get(request.task_run_id, request.project_id, request.course_id)
            .await
        {
            Ok(receipt) => receipt,
            Err(AgentClientError::ReviewMissing) => self
                .agent
                .enqueue(request, idempotency_key)
                .await
                .map_err(|error| ExecutionError::Backend(error.to_string()))?,
            Err(error) => return Err(ExecutionError::Backend(error.to_string())),
        };
        validate_advisory_receipt_hash(request, expected_request_sha256, &receipt)?;
        Ok(receipt)
    }

    async fn build_advisory_request(
        &self,
        context: &EvaluationAttemptContext,
        frozen: &FrozenSubmission,
        include: &[String],
        rubric: &str,
        output_mode: AdvisoryOutputMode,
    ) -> Result<InternalAgentLlmReviewRequest, ExecutionError> {
        if output_mode != AdvisoryOutputMode::GoalAssessment {
            return Err(ExecutionError::Backend(
                "advisory_output_mode_invalid".to_owned(),
            ));
        }
        let package = self.load_execution_binding(context.run.release_id).await?;
        package
            .validate_ownership(context.run.project_id, context.run.course_id)
            .map_err(|_| ExecutionError::IdentityMismatch)?;
        let (archive, package_bytes) = self.load_archive_and_package(frozen, &package).await?;
        let policy = self
            .authoring
            .active_policy(context.run.project_id)
            .await
            .map_err(|error| ExecutionError::Backend(error.to_string()))?;
        policy
            .validate_ownership(context.run.project_id, context.run.course_id)
            .map_err(|_| ExecutionError::IdentityMismatch)?;
        let mut paths = BTreeSet::new();
        let files = include
            .iter()
            .map(|path| {
                if !paths.insert(path.as_str()) {
                    return Err(ExecutionError::Backend(
                        "advisory_include_duplicate".to_owned(),
                    ));
                }
                let file_bytes = archive.get(path).ok_or_else(|| {
                    ExecutionError::Backend("advisory_include_missing".to_owned())
                })?;
                let file_content = String::from_utf8(file_bytes.clone())
                    .map_err(|_| ExecutionError::Backend("advisory_include_not_utf8".to_owned()))?;
                Ok(AgentLlmReviewFile {
                    path: path.clone(),
                    sha256: Sha256Digest::of_bytes(file_content.as_bytes()).to_string(),
                    content: file_content,
                })
            })
            .collect::<Result<Vec<_>, ExecutionError>>()?;
        if files.is_empty() {
            return Err(ExecutionError::Backend("advisory_include_empty".to_owned()));
        }
        let rubric_path = rubric.strip_prefix("evaluator://").unwrap_or(rubric);
        let rubric_file = find_package_file(&package.package, rubric_path)
            .ok_or_else(|| ExecutionError::Backend("advisory_rubric_missing".to_owned()))?;
        let rubric_bytes = package_bytes
            .get(&rubric_file.object.artifact_id)
            .ok_or_else(|| ExecutionError::Backend("advisory_rubric_unavailable".to_owned()))?;
        let rubric_content = String::from_utf8(rubric_bytes.clone())
            .map_err(|_| ExecutionError::Backend("advisory_rubric_not_utf8".to_owned()))?;
        let now = self
            .control
            .authority_now()
            .await
            .map_err(ExecutionError::Control)?;
        let deadline_at = contracts::UtcTimestamp::from_utc(now.get() + time::Duration::minutes(5))
            .map_err(|_| ExecutionError::Backend("advisory_deadline_invalid".to_owned()))?;
        let request = InternalAgentLlmReviewRequest {
            task_run_id: context.lease.task_run_id,
            project_id: context.run.project_id,
            course_id: context.run.course_id,
            frozen_submission_id: frozen.id,
            submission_artifact: frozen.object.clone(),
            policy,
            files,
            rubric: AgentLlmReviewRubric {
                artifact: rubric_file.object.clone(),
                path: rubric_file.path.clone(),
                sha256: Sha256Digest::of_bytes(rubric_bytes).to_string(),
                content: rubric_content,
            },
            deadline_at,
        };
        request
            .validate()
            .map_err(|_| ExecutionError::Backend("advisory_request_invalid".to_owned()))?;
        Ok(request)
    }

    async fn observe_advisory(
        &self,
        context: &EvaluationAttemptContext,
        request: &InternalAgentLlmReviewRequest,
        cancel_key: &str,
        receipt: &mut contracts::http::InternalAgentLlmReviewReceipt,
    ) -> Result<
        (
            contracts::http::InternalAgentLlmReviewReceipt,
            Option<contracts::UtcTimestamp>,
            Option<contracts::UtcTimestamp>,
        ),
        ExecutionError,
    > {
        let mut cancellation_sent = false;
        loop {
            if context.lease_lost.is_cancelled() {
                return Err(ExecutionError::LeaseLost);
            }
            let cancelling = context.cancellation.is_cancelled()
                || self.run_is_cancelling(context.lease.run_id).await?;
            if cancelling
                && !cancellation_sent
                && matches!(
                    receipt.state,
                    AgentLlmReviewState::Queued
                        | AgentLlmReviewState::Running
                        | AgentLlmReviewState::Cancelling
                )
            {
                *receipt = self
                    .agent
                    .cancel(
                        request.task_run_id,
                        request.project_id,
                        request.course_id,
                        cancel_key,
                    )
                    .await
                    .map_err(|error| ExecutionError::Backend(error.to_string()))?;
                cancellation_sent = true;
            }
            if matches!(
                receipt.state,
                AgentLlmReviewState::Succeeded
                    | AgentLlmReviewState::Failed
                    | AgentLlmReviewState::Cancelled
            ) {
                return Ok((receipt.clone(), receipt.started_at, receipt.finished_at));
            }
            tokio::time::sleep(Duration::from_millis(
                self.configuration
                    .execution_observe_poll_interval_milliseconds,
            ))
            .await;
            *receipt = self
                .agent
                .get(request.task_run_id, request.project_id, request.course_id)
                .await
                .map_err(|error| ExecutionError::Backend(error.to_string()))?;
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "program execution requires the immutable request, profile, phase, test groups, and resource limits"
    )]
    async fn start_program(
        &self,
        context: &EvaluationAttemptContext,
        frozen: &FrozenSubmission,
        toolchain_profile: &str,
        phase: ProgramPhase,
        input: &str,
        test_groups: &[contracts::evaluation::TestGroup],
        limits: contracts::evaluation::ExecutionLimits,
    ) -> Result<StartedExecution, ExecutionError> {
        let (request, package, package_bytes, profile_file, profile) = self
            .build_program_request(
                context,
                frozen,
                toolchain_profile,
                phase,
                input,
                test_groups,
                limits,
            )
            .await?;
        let intent = execution_resources(
            context,
            self.configuration.runner_namespace.as_str(),
            EvaluationExecutionKind::Program,
            &request,
            Vec::new(),
        )?;
        self.control
            .persist_execution_intent(&context.lease, &intent)
            .await
            .map_err(ExecutionError::Control)?;
        let materializer = self
            .materialize_command(
                frozen,
                &package,
                &package_bytes,
                &request,
                phase,
                &profile_file,
                &profile,
            )
            .await?;
        let binding = OjJobBinding {
            namespace: self.configuration.runner_namespace.clone(),
            service_account_name: self.configuration.oj_service_account_name.clone(),
            image_pull_secret_name: self.configuration.image_pull_secret_name.clone(),
            worker_image: context.run.identity.runtime_identity.runner_image.clone(),
            request: request.clone(),
            materializer,
            materializer_ca_bundle: self.materializer_ca_bundle.clone(),
        };
        let resources = self
            .oj
            .start(&binding)
            .await
            .map_err(|_| ExecutionError::Backend("oj_start_failed".to_owned()))?;
        let Ok(objects) = self.oj.capture_object_refs(&resources, &request).await else {
            self.cleanup_oj(&resources, context).await?;
            return Err(ExecutionError::Backend(
                "oj_object_capture_failed".to_owned(),
            ));
        };
        let recovery = execution_resources(
            context,
            self.configuration.runner_namespace.as_str(),
            EvaluationExecutionKind::Program,
            &request,
            objects,
        )?;
        Ok(StartedExecution::Program {
            resources,
            request,
            recovery,
        })
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "request construction validates the complete frozen submission and selected test inputs"
    )]
    async fn build_program_request(
        &self,
        context: &EvaluationAttemptContext,
        frozen: &FrozenSubmission,
        toolchain_profile: &str,
        phase: ProgramPhase,
        input: &str,
        test_groups: &[contracts::evaluation::TestGroup],
        limits: contracts::evaluation::ExecutionLimits,
    ) -> Result<
        (
            OjExecutionRequest,
            contracts::evaluation::EvaluationExecutionBinding,
            BTreeMap<contracts::ArtifactId, Vec<u8>>,
            PackageFile,
            ApprovedProgramProfile,
        ),
        ExecutionError,
    > {
        let package = self.load_execution_binding(context.run.release_id).await?;
        package
            .validate_ownership(context.run.project_id, context.run.course_id)
            .map_err(|_| ExecutionError::IdentityMismatch)?;
        let (archive, package_bytes) = self.load_archive_and_package(frozen, &package).await?;
        let profile_file = find_profile_file(&package.package, toolchain_profile)
            .ok_or_else(|| ExecutionError::Backend("program_profile_missing".to_owned()))?;
        let profile_bytes = package_bytes
            .get(&profile_file.object.artifact_id)
            .ok_or_else(|| ExecutionError::Backend("program_profile_unavailable".to_owned()))?;
        let profile: ApprovedProgramProfile = contracts::parse_strict_json(profile_bytes)
            .map_err(|_| ExecutionError::Backend("program_profile_invalid".to_owned()))?;
        profile
            .validate_for_phase(phase)
            .map_err(|_| ExecutionError::Backend("program_profile_invalid".to_owned()))?;
        let private_test_paths = test_groups
            .iter()
            .flat_map(|group| {
                evaluator_path(group.source())
                    .ok()
                    .into_iter()
                    .flat_map(|base| {
                        [
                            base.clone(),
                            format!("{base}.in"),
                            format!("{base}.input"),
                            format!("{base}.out"),
                            format!("{base}.expected"),
                        ]
                    })
            })
            .collect::<BTreeSet<_>>();
        if profile
            .support_files
            .iter()
            .any(|path| private_test_paths.contains(path))
        {
            return Err(ExecutionError::Backend(
                "profile_support_file_exposes_private_test".to_owned(),
            ));
        }
        let source = archive
            .get(input)
            .ok_or_else(|| ExecutionError::Backend("submission_input_missing".to_owned()))?;
        let image_digest = image_digest(&context.run.identity.runtime_identity.runner_image)?;
        let evaluator_identity = Sha256Digest::of_canonical(&package.package)
            .map_err(|_| ExecutionError::Backend("evaluator_identity_invalid".to_owned()))?;
        let cases = if phase == ProgramPhase::Test {
            test_groups
                .iter()
                .map(|group| {
                    let base = evaluator_path(group.source())?;
                    let input_file = find_package_file(&package.package, &format!("{base}.in"))
                        .or_else(|| find_package_file(&package.package, &format!("{base}.input")))
                        .ok_or_else(|| ExecutionError::Backend("test_input_missing".to_owned()))?;
                    let expected_file = find_package_file(&package.package, &format!("{base}.out"))
                        .or_else(|| {
                            find_package_file(&package.package, &format!("{base}.expected"))
                        })
                        .ok_or_else(|| {
                            ExecutionError::Backend("test_expected_missing".to_owned())
                        })?;
                    Ok(OjCaseBinding {
                        id: group.name().to_owned(),
                        input: package_file_binding(input_file, &package_bytes)?,
                        expected: package_file_binding(expected_file, &package_bytes)?,
                        max_points: group.max_points(),
                    })
                })
                .collect::<Result<Vec<_>, ExecutionError>>()?
        } else {
            Vec::new()
        };
        let request = OjExecutionRequest {
            schema_version: crate::oj::OJ_EXECUTION_SCHEMA_VERSION.to_owned(),
            run_id: context.lease.run_id.as_uuid(),
            step_run_id: context.lease.step_run_id.as_uuid(),
            attempt_id: context.lease.task_run_id.as_uuid(),
            trace_id: context.lease.trace_id.clone(),
            toolchain_profile: profile_file.path.clone(),
            toolchain_image_digest: image_digest,
            submission_identity: frozen
                .content_sha256
                .parse()
                .map_err(|_| ExecutionError::IdentityMismatch)?,
            evaluator_identity: Some(evaluator_identity),
            source: OjFileBinding {
                path: input.to_owned(),
                sha256: Sha256Digest::of_bytes(source),
                size_bytes: u64::try_from(source.len())
                    .map_err(|_| ExecutionError::Backend("submission_input_invalid".to_owned()))?,
            },
            phase: match phase {
                ProgramPhase::Compile => OjExecutionPhase::Compile,
                ProgramPhase::Test => OjExecutionPhase::Test,
            },
            checker: (phase == ProgramPhase::Test).then_some(OjCheckerKind::Exact),
            cases,
            score_max_points: if phase == ProgramPhase::Test {
                context.lease.max_score
            } else {
                0
            },
            limits: OjExecutionLimits {
                compile_wall_milliseconds: limits.wall_time_seconds().saturating_mul(1_000),
                run_wall_milliseconds: limits.wall_time_seconds().saturating_mul(1_000),
                cpu_milliseconds: limits.wall_time_seconds().saturating_mul(1_000),
                memory_bytes: limits.memory_bytes(),
                output_bytes: limits.output_bytes(),
            },
        };
        request
            .validate()
            .map_err(|_| ExecutionError::Backend("oj_request_invalid".to_owned()))?;
        let profile_file = profile_file.clone();
        Ok((request, package, package_bytes, profile_file, profile))
    }

    async fn start_ansible_probe(
        &self,
        context: &EvaluationAttemptContext,
        frozen: &FrozenSubmission,
        playbook_profile: &str,
        module_allowlist: &[String],
        assertions: &[contracts::evaluation::FactAssertion],
    ) -> Result<StartedExecution, ExecutionError> {
        let (request, package, package_bytes, profile_file, credentials) = self
            .build_ansible_probe_request(
                context,
                frozen,
                playbook_profile,
                module_allowlist,
                assertions,
            )
            .await?;
        let intent = execution_resources(
            context,
            self.configuration.runner_namespace.as_str(),
            EvaluationExecutionKind::AnsibleProbe,
            &request,
            Vec::new(),
        )?;
        self.control
            .persist_execution_intent(&context.lease, &intent)
            .await
            .map_err(ExecutionError::Control)?;
        let materializer = self
            .materialize_probe_command(frozen, &package, &package_bytes, &profile_file)
            .await?;
        let binding = AnsibleProbeJobBinding {
            namespace: self.configuration.runner_namespace.clone(),
            service_account_name: self
                .configuration
                .ansible_probe_service_account_name
                .clone(),
            image_pull_secret_name: self.configuration.image_pull_secret_name.clone(),
            worker_image: context.run.identity.runtime_identity.runner_image.clone(),
            request: request.clone(),
            materializer,
            materializer_ca_bundle: self.materializer_ca_bundle.clone(),
        };
        let resources = self
            .ansible_probe
            .start_with_ssh_credentials(
                &binding,
                credentials.private_key_openssh(),
                credentials.certificate_openssh(),
            )
            .await
            .map_err(|_| ExecutionError::Backend("probe_start_failed".to_owned()))?;
        let Ok(objects) = self
            .ansible_probe
            .capture_object_refs(&resources, &request)
            .await
        else {
            self.cleanup_probe(&resources, context).await?;
            return Err(ExecutionError::Backend(
                "probe_object_capture_failed".to_owned(),
            ));
        };
        let recovery = execution_resources(
            context,
            self.configuration.runner_namespace.as_str(),
            EvaluationExecutionKind::AnsibleProbe,
            &request,
            objects,
        )?;
        Ok(StartedExecution::AnsibleProbe {
            resources,
            request,
            recovery,
        })
    }

    async fn build_ansible_probe_request(
        &self,
        context: &EvaluationAttemptContext,
        frozen: &FrozenSubmission,
        playbook_profile: &str,
        module_allowlist: &[String],
        assertions: &[contracts::evaluation::FactAssertion],
    ) -> Result<
        (
            AnsibleProbeExecutionRequest,
            contracts::evaluation::EvaluationExecutionBinding,
            BTreeMap<contracts::ArtifactId, Vec<u8>>,
            PackageFile,
            ResolvedEnvironmentExecutionBinding,
        ),
        ExecutionError,
    > {
        let package = self.load_execution_binding(context.run.release_id).await?;
        package
            .validate_ownership(context.run.project_id, context.run.course_id)
            .map_err(|_| ExecutionError::IdentityMismatch)?;
        let (archive, package_bytes) = self.load_archive_and_package(frozen, &package).await?;
        let _ = archive;
        let profile_path = resolve_playbook_path(&package.package, playbook_profile)
            .ok_or_else(|| ExecutionError::Backend("probe_profile_missing".to_owned()))?;
        let credentials = self
            .environment
            .resolve(
                frozen.environment.environment_id,
                context.run.project_id,
                context.run.course_id,
                context.run.actor_id,
                frozen.environment.environment_revision,
                frozen.environment.runtime_kind,
                context.lease.run_id,
                context.lease.step_run_id,
                context.lease.attempt,
            )
            .await
            .map_err(map_environment_binding_error)?;
        if credentials.binding.environment != frozen.environment {
            return Err(ExecutionError::IdentityMismatch);
        }
        let (namespace, host, port, username, expected_host_key_sha256, source_identity) =
            match &credentials.binding.source {
                contracts::environment::EnvironmentExecutionSourceBinding::VirtualMachine {
                    namespace,
                    host,
                    port,
                    username,
                    expected_host_key_sha256,
                    source_identity,
                    ..
                } => (
                    namespace,
                    host,
                    *port,
                    username,
                    expected_host_key_sha256,
                    source_identity,
                ),
            };
        if namespace != &format!("lw-env-{}", frozen.environment.environment_id)
            || port != crate::ansible_probe::SSH_PORT
        {
            return Err(ExecutionError::IdentityMismatch);
        }
        let expected_host_key_sha256 = expected_host_key_sha256
            .parse()
            .map_err(|_| ExecutionError::IdentityMismatch)?;
        let (private_key_secret, certificate_secret) =
            attempt_ssh_secret_names(context.lease.task_run_id.as_uuid());
        let request = AnsibleProbeExecutionRequest {
            schema_version: ANSIBLE_PROBE_EXECUTION_SCHEMA_VERSION.to_owned(),
            run_id: context.lease.run_id.as_uuid(),
            step_run_id: context.lease.step_run_id.as_uuid(),
            attempt_id: context.lease.task_run_id.as_uuid(),
            trace_id: context.lease.trace_id.clone(),
            runner_image_digest: context.run.identity.runtime_identity.runner_image.clone(),
            playbook_profile: profile_path.path.clone(),
            module_allowlist: module_allowlist.to_vec(),
            read_only: true,
            assertions: assertions.to_vec(),
            target: AnsibleProbeTarget {
                host: host.parse().map_err(|_| ExecutionError::IdentityMismatch)?,
                port: crate::ansible_probe::SSH_PORT,
                username: username.clone(),
            },
            ssh_identity: AnsibleProbeSshIdentity {
                private_key_secret,
                certificate_secret,
                expected_host_key_sha256,
            },
            source_identity: source_identity.clone(),
            limits: AnsibleProbeExecutionLimits {
                wall_time_seconds: self.configuration.ansible_probe.wall_time_seconds,
                facts_max_bytes: self.configuration.ansible_probe.facts_max_bytes,
                output_max_bytes: self.configuration.ansible_probe.output_max_bytes,
                max_assertions: self.configuration.ansible_probe.max_assertions,
            },
            evaluation_spec_sha256: Sha256Digest::of_canonical(&context.release.evaluation_spec)
                .map_err(|_| ExecutionError::Backend("evaluation_identity_invalid".to_owned()))?,
        };
        request
            .validate()
            .map_err(|_| ExecutionError::Backend("probe_request_invalid".to_owned()))?;
        let profile_path = profile_path.clone();
        Ok((request, package, package_bytes, profile_path, credentials))
    }

    async fn observe_oj(
        &self,
        context: &EvaluationAttemptContext,
        resources: Option<&OjJobResources>,
        request: &OjExecutionRequest,
        recovery: Option<&EvaluationExecutionResources>,
    ) -> Result<(TerminalResult, ExecutionTiming), ExecutionError> {
        loop {
            if context.cancellation.is_cancelled()
                || self.run_is_cancelling(context.lease.run_id).await?
            {
                return Ok((TerminalResult::Cancelled, ExecutionTiming::unknown()));
            }
            let observation = match recovery {
                Some(recovery) => self.oj.observe_recovery(recovery, request).await,
                None => {
                    self.oj
                        .observe(
                            resources.ok_or_else(|| {
                                ExecutionError::Backend("oj_resources_missing".to_owned())
                            })?,
                            request,
                        )
                        .await
                }
            }
            .map_err(|_| ExecutionError::Backend("oj_observe_failed".to_owned()))?;
            match observation {
                OjJobObservation::Running => {
                    tokio::time::sleep(Duration::from_millis(
                        self.configuration
                            .execution_observe_poll_interval_milliseconds,
                    ))
                    .await;
                }
                OjJobObservation::Missing => {
                    return Ok((
                        TerminalResult::Failed("LW_OJ_JOB_MISSING".to_owned()),
                        ExecutionTiming::unknown(),
                    ));
                }
                OjJobObservation::Completed { receipt, timing } => {
                    return Ok((
                        oj_receipt_result(
                            request.phase,
                            receipt.terminal_status,
                            receipt.awarded_points,
                            receipt.diagnostic_code,
                        ),
                        timing,
                    ));
                }
                OjJobObservation::Failed {
                    diagnostic_code,
                    timing,
                } => {
                    return Ok((TerminalResult::Failed(diagnostic_code), timing));
                }
            }
        }
    }

    async fn observe_probe(
        &self,
        context: &EvaluationAttemptContext,
        resources: Option<&AnsibleProbeJobResources>,
        request: &AnsibleProbeExecutionRequest,
        recovery: Option<&EvaluationExecutionResources>,
    ) -> Result<(TerminalResult, ExecutionTiming), ExecutionError> {
        loop {
            if context.cancellation.is_cancelled()
                || self.run_is_cancelling(context.lease.run_id).await?
            {
                return Ok((TerminalResult::Cancelled, ExecutionTiming::unknown()));
            }
            let observation = match recovery {
                Some(recovery) => self.ansible_probe.observe_recovery(recovery, request).await,
                None => {
                    self.ansible_probe
                        .observe(
                            resources.ok_or_else(|| {
                                ExecutionError::Backend("probe_resources_missing".to_owned())
                            })?,
                            request,
                        )
                        .await
                }
            }
            .map_err(|_| ExecutionError::Backend("probe_observe_failed".to_owned()))?;
            match observation {
                AnsibleProbeJobObservation::Running => {
                    tokio::time::sleep(Duration::from_millis(
                        self.configuration
                            .execution_observe_poll_interval_milliseconds,
                    ))
                    .await;
                }
                AnsibleProbeJobObservation::Missing => {
                    return Ok((
                        TerminalResult::Failed("LW_AP_JOB_MISSING".to_owned()),
                        ExecutionTiming::unknown(),
                    ));
                }
                AnsibleProbeJobObservation::Completed { receipt, timing } => {
                    if receipt.terminal_status
                        == crate::ansible_probe::AnsibleProbeTerminalStatus::Succeeded
                    {
                        return Ok((TerminalResult::Succeeded { score: None }, timing));
                    }
                    return Ok((TerminalResult::Failed(receipt.diagnostic_code), timing));
                }
                AnsibleProbeJobObservation::Failed {
                    diagnostic_code,
                    timing,
                } => {
                    return Ok((TerminalResult::Failed(diagnostic_code), timing));
                }
            }
        }
    }

    async fn cleanup_oj(
        &self,
        resources: &OjJobResources,
        context: &EvaluationAttemptContext,
    ) -> Result<(), ExecutionError> {
        let deadline =
            Instant::now() + Duration::from_secs(self.configuration.cleanup_timeout_seconds);
        loop {
            if Self::cleanup_must_stop(context) {
                return Err(ExecutionError::LeaseLost);
            }
            let complete = self
                .oj
                .cleanup(resources)
                .await
                .map_err(|_| ExecutionError::Backend("oj_cleanup_failed".to_owned()))?;
            if complete {
                return Ok(());
            }
            if Self::cleanup_must_stop(context) || Instant::now() >= deadline {
                return Err(ExecutionError::Backend("oj_cleanup_pending".to_owned()));
            }
            tokio::time::sleep(Duration::from_millis(
                self.configuration
                    .execution_observe_poll_interval_milliseconds,
            ))
            .await;
        }
    }

    async fn cleanup_probe(
        &self,
        resources: &AnsibleProbeJobResources,
        context: &EvaluationAttemptContext,
    ) -> Result<(), ExecutionError> {
        let deadline =
            Instant::now() + Duration::from_secs(self.configuration.cleanup_timeout_seconds);
        loop {
            if Self::cleanup_must_stop(context) {
                return Err(ExecutionError::LeaseLost);
            }
            let complete = self
                .ansible_probe
                .cleanup(resources)
                .await
                .map_err(|_| ExecutionError::Backend("probe_cleanup_failed".to_owned()))?;
            if complete {
                return Ok(());
            }
            if Self::cleanup_must_stop(context) || Instant::now() >= deadline {
                return Err(ExecutionError::Backend("probe_cleanup_pending".to_owned()));
            }
            tokio::time::sleep(Duration::from_millis(
                self.configuration
                    .execution_observe_poll_interval_milliseconds,
            ))
            .await;
        }
    }

    fn cleanup_must_stop(context: &EvaluationAttemptContext) -> bool {
        context.lease_lost.is_cancelled()
    }

    async fn load_execution_binding(
        &self,
        release_id: contracts::EvaluationReleaseId,
    ) -> Result<contracts::evaluation::EvaluationExecutionBinding, ExecutionError> {
        self.control
            .load_release_execution_binding(release_id)
            .await
            .map_err(ExecutionError::Control)
    }

    async fn load_archive_and_package(
        &self,
        frozen: &FrozenSubmission,
        package: &contracts::evaluation::EvaluationExecutionBinding,
    ) -> Result<
        (
            BTreeMap<String, Vec<u8>>,
            BTreeMap<contracts::ArtifactId, Vec<u8>>,
        ),
        ExecutionError,
    > {
        let key = self
            .freezes
            .load_completed_object_key(
                frozen.id,
                frozen.project_id,
                frozen.course_id,
                frozen.actor_id,
            )
            .await
            .map_err(|_| ExecutionError::Backend("frozen_object_key_missing".to_owned()))?;
        if frozen.object.store_binding != self.objects.binding()
            || frozen.object.media_type != FROZEN_ARCHIVE_MEDIA_TYPE
        {
            return Err(ExecutionError::IdentityMismatch);
        }
        let object = self
            .objects
            .read_verified(&key, &frozen.object)
            .await
            .map_err(|_| ExecutionError::Backend("frozen_object_read_failed".to_owned()))?;
        let archive = decode_archive_metadata(&object.bytes)?;
        validate_frozen_archive(frozen, &archive)?;
        let mut bytes = BTreeMap::new();
        for file in &package.package.files {
            if file.object.store_binding != self.objects.binding() {
                return Err(ExecutionError::IdentityMismatch);
            }
            let key = package
                .object_locators
                .get(&file.object.artifact_id)
                .ok_or(ExecutionError::IdentityMismatch)?;
            let object = self
                .objects
                .read_verified(key, &file.object)
                .await
                .map_err(|_| ExecutionError::Backend("package_object_read_failed".to_owned()))?;
            if u64::try_from(object.bytes.len()).ok() != Some(file.object.size_bytes) {
                return Err(ExecutionError::IdentityMismatch);
            }
            bytes.insert(file.object.artifact_id, object.bytes);
        }
        Ok((archive, bytes))
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "materializer signing binds the package, request, phase, profile, and authority clock"
    )]
    async fn materialize_command(
        &self,
        frozen: &FrozenSubmission,
        package: &contracts::evaluation::EvaluationExecutionBinding,
        package_bytes: &BTreeMap<contracts::ArtifactId, Vec<u8>>,
        request: &OjExecutionRequest,
        phase: ProgramPhase,
        profile_file: &PackageFile,
        profile: &ApprovedProgramProfile,
    ) -> Result<MaterializeCommand, ExecutionError> {
        let now = self
            .control
            .authority_now()
            .await
            .map_err(ExecutionError::Control)?;
        let mut files = vec![profile_file];
        for path in &profile.support_files {
            let file = find_package_file(&package.package, path).ok_or_else(|| {
                ExecutionError::Backend("profile_support_file_missing".to_owned())
            })?;
            files.push(file);
        }
        if phase == ProgramPhase::Test {
            for case in &request.cases {
                let input = find_package_file(&package.package, &case.input.path)
                    .ok_or_else(|| ExecutionError::Backend("test_input_missing".to_owned()))?;
                let expected = find_package_file(&package.package, &case.expected.path)
                    .ok_or_else(|| ExecutionError::Backend("test_expected_missing".to_owned()))?;
                files.push(input);
                files.push(expected);
            }
        }
        files.sort_by(|left, right| left.path.cmp(&right.path));
        files.dedup_by(|left, right| left.path == right.path);
        let mut artifacts = vec![self.signed_frozen_artifact(frozen, now).await?];
        for file in files {
            artifacts.push(
                self.signed_package_artifact(package, package_bytes, file, now)
                    .await?,
            );
        }
        Ok(MaterializeCommand {
            schema_version: ARTIFACT_MATERIALIZER_SCHEMA_VERSION.to_owned(),
            artifacts,
        })
    }

    async fn materialize_probe_command(
        &self,
        frozen: &FrozenSubmission,
        package: &contracts::evaluation::EvaluationExecutionBinding,
        package_bytes: &BTreeMap<contracts::ArtifactId, Vec<u8>>,
        profile_file: &PackageFile,
    ) -> Result<MaterializeCommand, ExecutionError> {
        let now = self
            .control
            .authority_now()
            .await
            .map_err(ExecutionError::Control)?;
        Ok(MaterializeCommand {
            schema_version: ARTIFACT_MATERIALIZER_SCHEMA_VERSION.to_owned(),
            artifacts: vec![
                self.signed_frozen_artifact(frozen, now).await?,
                self.signed_package_artifact(package, package_bytes, profile_file, now)
                    .await?,
            ],
        })
    }

    async fn signed_frozen_artifact(
        &self,
        frozen: &FrozenSubmission,
        now: contracts::UtcTimestamp,
    ) -> Result<MaterializeArtifact, ExecutionError> {
        let key = self
            .freezes
            .load_completed_object_key(
                frozen.id,
                frozen.project_id,
                frozen.course_id,
                frozen.actor_id,
            )
            .await
            .map_err(|_| ExecutionError::Backend("frozen_object_key_missing".to_owned()))?;
        let signed = self
            .objects
            .presign_download(
                &key,
                &frozen.object.object_version,
                frozen.object.size_bytes,
                &frozen.object.media_type,
                now,
            )
            .await
            .map_err(|_| ExecutionError::Backend("frozen_object_sign_failed".to_owned()))?;
        Ok(MaterializeArtifact {
            url: signed.url,
            required_headers: signed.required_headers,
            expected_sha256: frozen
                .content_sha256
                .parse()
                .map_err(|_| ExecutionError::IdentityMismatch)?,
            expected_size_bytes: frozen.object.size_bytes,
            media_type: frozen.object.media_type.clone(),
            destination: MaterializeDestination::Submission,
            content: MaterializeContent::FrozenArchive,
        })
    }

    async fn signed_package_artifact(
        &self,
        package: &contracts::evaluation::EvaluationExecutionBinding,
        package_bytes: &BTreeMap<contracts::ArtifactId, Vec<u8>>,
        file: &PackageFile,
        now: contracts::UtcTimestamp,
    ) -> Result<MaterializeArtifact, ExecutionError> {
        let bytes = package_bytes
            .get(&file.object.artifact_id)
            .ok_or_else(|| ExecutionError::Backend("package_object_unavailable".to_owned()))?;
        let key = package
            .object_locators
            .get(&file.object.artifact_id)
            .ok_or(ExecutionError::IdentityMismatch)?;
        let signed = self
            .objects
            .presign_download(
                key,
                &file.object.object_version,
                file.object.size_bytes,
                &file.object.media_type,
                now,
            )
            .await
            .map_err(|_| ExecutionError::Backend("package_object_sign_failed".to_owned()))?;
        Ok(MaterializeArtifact {
            url: signed.url,
            required_headers: signed.required_headers,
            expected_sha256: Sha256Digest::of_bytes(bytes),
            expected_size_bytes: file.object.size_bytes,
            media_type: file.object.media_type.clone(),
            destination: MaterializeDestination::Evaluator,
            content: MaterializeContent::RawFile {
                path: file.path.clone(),
            },
        })
    }

    fn usage_deliveries(
        status: &contracts::http::TaskResourceStatus,
        timing: ExecutionTiming,
        fallback_until: contracts::UtcTimestamp,
    ) -> Result<Vec<RecordResourceUsageRequest>, ExecutionError> {
        timing.validate()?;
        let (measured_from, measured_until, measurement) = match timing.boundaries()? {
            (Some(started), Some(terminated)) => {
                let milliseconds = usage_milliseconds(started, terminated)?;
                let resources = &status.claim.workload_resources;
                let quantities = ResourceUsageQuantities {
                    cpu_millicore_seconds: quantity_per_millisecond(
                        u64::from(resources.cpu_millicores),
                        milliseconds,
                    )?,
                    memory_byte_seconds: quantity_per_millisecond(
                        resources.memory_bytes,
                        milliseconds,
                    )?,
                    storage_byte_seconds: 0,
                    gpu_unit_seconds: match resources.gpu.as_ref() {
                        Some(gpu) => quantity_per_millisecond(u64::from(gpu.count), milliseconds)?,
                        None => 0,
                    },
                };
                (started, terminated, UsageMeasurement::Known { quantities })
            }
            (None, None) => {
                let started = status.lease.active_from.ok_or_else(|| {
                    ExecutionError::Backend("resource_active_from_missing".to_owned())
                })?;
                if fallback_until <= started {
                    return Err(ExecutionError::Backend("usage_interval_invalid".to_owned()));
                }
                (
                    started,
                    fallback_until,
                    UsageMeasurement::Unknown {
                        reason: "executor_timing_unavailable".to_owned(),
                    },
                )
            }
            _ => {
                return Err(ExecutionError::Backend(
                    "execution_timing_invalid".to_owned(),
                ));
            }
        };
        let compute = RecordResourceUsageRequest {
            project_id: status.project_id,
            course_id: status.request.course_id,
            kind: ResourceUsageKind::Compute,
            request_id: status.request.id,
            lease_id: Some(status.lease.id),
            source_event_id: deterministic_usage_event_id(status.task_run_id, 0x01)?,
            measured_from,
            measured_until,
            measurement: measurement.clone(),
        };
        let storage = status.claim.workload_resources.storage_bytes;
        let mut deliveries = vec![compute];
        if storage > 0 {
            let storage_measurement = match measurement {
                UsageMeasurement::Known { .. } => UsageMeasurement::Known {
                    quantities: ResourceUsageQuantities {
                        cpu_millicore_seconds: 0,
                        memory_byte_seconds: 0,
                        storage_byte_seconds: quantity_per_millisecond(
                            storage,
                            usage_milliseconds(measured_from, measured_until)?,
                        )?,
                        gpu_unit_seconds: 0,
                    },
                },
                UsageMeasurement::Unknown { reason } => UsageMeasurement::Unknown { reason },
            };
            deliveries.push(RecordResourceUsageRequest {
                project_id: status.project_id,
                course_id: status.request.course_id,
                kind: ResourceUsageKind::Storage,
                request_id: status.request.id,
                lease_id: Some(status.lease.id),
                source_event_id: deterministic_usage_event_id(status.task_run_id, 0x02)?,
                measured_from,
                measured_until,
                measurement: storage_measurement,
            });
        }
        Ok(deliveries)
    }

    async fn run_is_cancelling(
        &self,
        run_id: contracts::EvaluationRunId,
    ) -> Result<bool, ExecutionError> {
        let run = self
            .control
            .load_run(run_id)
            .await
            .map_err(ExecutionError::Control)?;
        Ok(run.cancellation_requested
            || run.state == contracts::evaluation::EvaluationRunState::Cancelling)
    }
}

#[async_trait::async_trait]
impl EvaluationAttemptRunner for KubernetesEvaluationRunner {
    async fn execute(
        &self,
        context: EvaluationAttemptContext,
    ) -> Result<EvaluationStepCompletion, ExecutionError> {
        Box::pin(self.execute_attempt(context)).await
    }
}

fn attempt_ssh_secret_names(attempt_id: Uuid) -> (String, String) {
    let prefix = format!("lw-ap-{}", &attempt_id.simple().to_string()[..20]);
    (format!("{prefix}-ssh-key"), format!("{prefix}-ssh-cert"))
}

fn map_environment_binding_error(error: EnvironmentExecutionBindingClientError) -> ExecutionError {
    match error {
        EnvironmentExecutionBindingClientError::Configuration
        | EnvironmentExecutionBindingClientError::CredentialGeneration
        | EnvironmentExecutionBindingClientError::Clock => {
            ExecutionError::WorkerConfigurationInvalid
        }
        error => ExecutionError::Backend(error.to_string()),
    }
}

#[derive(Clone, Debug)]
enum TerminalResult {
    Succeeded { score: Option<u32> },
    Failed(String),
    Cancelled,
}

fn oj_receipt_result(
    phase: OjExecutionPhase,
    status: OjTerminalStatus,
    awarded_points: u32,
    diagnostic_code: String,
) -> TerminalResult {
    match (phase, status) {
        (OjExecutionPhase::Compile, OjTerminalStatus::Accepted) => {
            TerminalResult::Succeeded { score: None }
        }
        (
            OjExecutionPhase::Test,
            OjTerminalStatus::Accepted
            | OjTerminalStatus::WrongAnswer
            | OjTerminalStatus::TimeLimitExceeded
            | OjTerminalStatus::MemoryLimitExceeded
            | OjTerminalStatus::OutputLimitExceeded
            | OjTerminalStatus::RuntimeError,
        ) => TerminalResult::Succeeded {
            score: Some(awarded_points),
        },
        (_, OjTerminalStatus::Cancelled) => TerminalResult::Cancelled,
        _ => TerminalResult::Failed(diagnostic_code),
    }
}

fn advisory_completion(
    receipt: contracts::http::InternalAgentLlmReviewReceipt,
    allowed_paths: &[String],
) -> Result<EvaluationStepCompletion, ExecutionError> {
    let diagnostic = receipt.diagnostic_code.clone();
    match receipt.state {
        AgentLlmReviewState::Succeeded => {
            let review = receipt
                .review
                .ok_or_else(|| ExecutionError::Backend("advisory_review_missing".to_owned()))?;
            let review_json = serde_json::to_string(&review)
                .map_err(|_| ExecutionError::Backend("advisory_review_invalid".to_owned()))?;
            let review =
                contracts::evaluation::GoalReview::from_json_against(&review_json, allowed_paths)
                    .map_err(|_| {
                    ExecutionError::Backend("advisory_review_evidence_invalid".to_owned())
                })?;
            Ok(EvaluationStepCompletion {
                state: contracts::evaluation::EvaluationStepRunState::Succeeded,
                awarded_score: None,
                review: Some(review),
                diagnostic_code: None,
                cleanup_verified: true,
            })
        }
        AgentLlmReviewState::Cancelled => Ok(EvaluationStepCompletion {
            state: contracts::evaluation::EvaluationStepRunState::Cancelled,
            awarded_score: None,
            review: None,
            diagnostic_code: Some(
                contracts::DiagnosticCode::parse(
                    diagnostic.as_deref().unwrap_or("LW_LLM_REVIEW_CANCELLED"),
                )
                .map_err(|_| ExecutionError::Backend("advisory_diagnostic_invalid".to_owned()))?,
            ),
            cleanup_verified: true,
        }),
        AgentLlmReviewState::Failed => Ok(EvaluationStepCompletion {
            state: contracts::evaluation::EvaluationStepRunState::Failed,
            awarded_score: None,
            review: None,
            diagnostic_code: Some(
                contracts::DiagnosticCode::parse(
                    diagnostic.as_deref().unwrap_or("LW_LLM_REVIEW_FAILED"),
                )
                .map_err(|_| ExecutionError::Backend("advisory_diagnostic_invalid".to_owned()))?,
            ),
            cleanup_verified: true,
        }),
        AgentLlmReviewState::Queued
        | AgentLlmReviewState::Running
        | AgentLlmReviewState::Cancelling => {
            Err(ExecutionError::Backend("advisory_not_terminal".to_owned()))
        }
    }
}

fn validate_advisory_receipt_hash(
    request: &InternalAgentLlmReviewRequest,
    expected_request_sha256: Sha256Digest,
    receipt: &contracts::http::InternalAgentLlmReviewReceipt,
) -> Result<(), ExecutionError> {
    if receipt.request_sha256 != expected_request_sha256.to_string() {
        tracing::error!(
            event = "evaluation.advisory.receipt_identity_mismatch",
            task_run_id = %request.task_run_id,
            expected_request_sha256 = %expected_request_sha256,
            observed_request_sha256 = %receipt.request_sha256,
            "Agent returned a receipt for a different immutable request",
        );
        return Err(ExecutionError::IdentityMismatch);
    }
    Ok(())
}

impl TerminalResult {
    fn into_completion(self) -> Result<EvaluationStepCompletion, ExecutionError> {
        let completion = match self {
            Self::Succeeded { score } => EvaluationStepCompletion {
                state: contracts::evaluation::EvaluationStepRunState::Succeeded,
                awarded_score: score,
                review: None,
                diagnostic_code: None,
                cleanup_verified: true,
            },
            Self::Failed(diagnostic_code) => EvaluationStepCompletion {
                state: contracts::evaluation::EvaluationStepRunState::Failed,
                awarded_score: None,
                review: None,
                diagnostic_code: Some(contracts::DiagnosticCode::parse(diagnostic_code).map_err(
                    |_| ExecutionError::Backend("invalid_executor_diagnostic".to_owned()),
                )?),
                cleanup_verified: true,
            },
            Self::Cancelled => EvaluationStepCompletion {
                state: contracts::evaluation::EvaluationStepRunState::Cancelled,
                awarded_score: None,
                review: None,
                diagnostic_code: Some(contracts::DiagnosticCode::registered(
                    "LW_EVALUATION_CANCELLED",
                )),
                cleanup_verified: true,
            },
        };
        Ok(completion)
    }
}

fn map_task_resource(error: &TaskResourceError, stage: &str) -> ExecutionError {
    tracing::warn!(event = "evaluation.resource.lifecycle_failed", stage, error = %error);
    ExecutionError::Backend(format!("resource_{stage}_failed"))
}

fn execution_resources<T: Serialize>(
    context: &EvaluationAttemptContext,
    namespace: &str,
    kind: EvaluationExecutionKind,
    request: &T,
    objects: Vec<(String, String, String, String)>,
) -> Result<EvaluationExecutionResources, ExecutionError> {
    let request = serde_json::to_value(request)
        .map_err(|_| ExecutionError::Backend("execution_request_invalid".to_owned()))?;
    let resources = EvaluationExecutionResources {
        schema_version: crate::control_plane::EVALUATION_EXECUTION_RESOURCES_SCHEMA_VERSION
            .to_owned(),
        run_id: context.lease.run_id,
        step_run_id: context.lease.step_run_id,
        task_run_id: context.lease.task_run_id,
        namespace: namespace.to_owned(),
        kind,
        request,
        objects: objects
            .into_iter()
            .map(
                |(api_version, resource, name, uid)| EvaluationExecutionObjectRef {
                    api_version,
                    resource,
                    name,
                    uid,
                },
            )
            .collect(),
    };
    resources
        .validate_for(
            context.lease.run_id,
            context.lease.step_run_id,
            context.lease.task_run_id,
        )
        .map_err(|_| ExecutionError::Backend("execution_resources_invalid".to_owned()))?;
    Ok(resources)
}

fn parse_program_recovery_request(
    resources: &EvaluationExecutionResources,
    context: &EvaluationAttemptContext,
) -> Result<OjExecutionRequest, ExecutionError> {
    if resources.kind != EvaluationExecutionKind::Program {
        return Err(ExecutionError::IdentityMismatch);
    }
    let request: OjExecutionRequest = serde_json::from_value(resources.request.clone())
        .map_err(|_| ExecutionError::Backend("execution_request_invalid".to_owned()))?;
    request
        .validate()
        .map_err(|_| ExecutionError::Backend("execution_request_invalid".to_owned()))?;
    if request.run_id != context.lease.run_id.as_uuid()
        || request.step_run_id != context.lease.step_run_id.as_uuid()
        || request.attempt_id != context.lease.task_run_id.as_uuid()
    {
        return Err(ExecutionError::IdentityMismatch);
    }
    Ok(request)
}

fn parse_probe_recovery_request(
    resources: &EvaluationExecutionResources,
    context: &EvaluationAttemptContext,
) -> Result<AnsibleProbeExecutionRequest, ExecutionError> {
    if resources.kind != EvaluationExecutionKind::AnsibleProbe {
        return Err(ExecutionError::IdentityMismatch);
    }
    let request: AnsibleProbeExecutionRequest =
        serde_json::from_value(resources.request.clone())
            .map_err(|_| ExecutionError::Backend("execution_request_invalid".to_owned()))?;
    request
        .validate()
        .map_err(|_| ExecutionError::Backend("execution_request_invalid".to_owned()))?;
    if request.run_id != context.lease.run_id.as_uuid()
        || request.step_run_id != context.lease.step_run_id.as_uuid()
        || request.attempt_id != context.lease.task_run_id.as_uuid()
    {
        return Err(ExecutionError::IdentityMismatch);
    }
    Ok(request)
}

fn parse_advisory_recovery_request(
    resources: &EvaluationExecutionResources,
    context: &EvaluationAttemptContext,
) -> Result<InternalAgentLlmReviewRequest, ExecutionError> {
    if resources.kind != EvaluationExecutionKind::LlmReview {
        return Err(ExecutionError::IdentityMismatch);
    }
    let request: InternalAgentLlmReviewRequest = serde_json::from_value(resources.request.clone())
        .map_err(|_| ExecutionError::Backend("advisory_request_invalid".to_owned()))?;
    request
        .validate()
        .map_err(|_| ExecutionError::Backend("advisory_request_invalid".to_owned()))?;
    if request.task_run_id != context.lease.task_run_id
        || request.project_id != context.run.project_id
        || request.course_id != context.run.course_id
        || request.frozen_submission_id != context.run.frozen_submission_id
    {
        return Err(ExecutionError::IdentityMismatch);
    }
    Ok(request)
}

fn usage_milliseconds(
    measured_from: contracts::UtcTimestamp,
    measured_until: contracts::UtcTimestamp,
) -> Result<u64, ExecutionError> {
    if measured_until <= measured_from {
        return Err(ExecutionError::Backend("usage_interval_invalid".to_owned()));
    }
    u64::try_from(
        (measured_until.get() - measured_from.get())
            .whole_milliseconds()
            .max(1),
    )
    .map_err(|_| ExecutionError::Backend("usage_duration_invalid".to_owned()))
}

fn quantity_per_millisecond(base: u64, milliseconds: u64) -> Result<u64, ExecutionError> {
    u64::try_from(
        u128::from(base)
            .checked_mul(u128::from(milliseconds))
            .ok_or_else(|| ExecutionError::Backend("usage_quantity_overflow".to_owned()))?
            / 1_000,
    )
    .map_err(|_| ExecutionError::Backend("usage_quantity_overflow".to_owned()))
}

fn deterministic_usage_event_id(
    task_run_id: contracts::TaskRunId,
    discriminator: u8,
) -> Result<EventId, ExecutionError> {
    let mut bytes = task_run_id.as_uuid().into_bytes();
    // Preserve UUIDv7 version and RFC 9562 variant while deriving stable,
    // category-specific ids from the durable TaskRunId.
    bytes[14] = bytes[14].wrapping_add(discriminator);
    bytes[15] ^= discriminator;
    EventId::from_str(&Uuid::from_bytes(bytes).to_string())
        .map_err(|_| ExecutionError::Backend("usage_event_id_invalid".to_owned()))
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::{
        OjExecutionPhase, OjTerminalStatus, TerminalResult, oj_receipt_result,
        validate_advisory_receipt_hash,
    };
    use contracts::authoring::ProjectLlmEgressPolicy;
    use contracts::http::{
        AgentLlmReviewFile, AgentLlmReviewRubric, AgentLlmReviewState,
        InternalAgentLlmReviewReceipt, InternalAgentLlmReviewRequest,
    };
    use contracts::{
        ArtifactId, ArtifactRef, CourseId, FrozenSubmissionId, ProjectId, TaskRunId, UtcTimestamp,
    };
    use persistence_sqlx::Sha256Digest;
    use serde_json::json;
    use time::OffsetDateTime;

    #[allow(clippy::expect_used)]
    fn advisory_request(deadline_at: UtcTimestamp) -> InternalAgentLlmReviewRequest {
        let project_id = ProjectId::new();
        let course_id = Some(CourseId::new());
        let submission = "submission";
        let rubric = "rubric";
        let policy = serde_json::from_value::<ProjectLlmEgressPolicy>(json!({
            "id": "01900000-0000-7000-8000-000000000101",
            "projectId": project_id,
            "courseId": course_id,
            "revision": 1,
            "binding": {
                "runtimeBinding": "claude-code-production",
                "model": "claude-sonnet-4-6-20260601",
                "claudeCodeVersion": "2.1.207",
                "maxInFlightPerWorker": 1
            },
            "budget": {
                "maxInputTokens": 1000,
                "maxOutputTokens": 1000,
                "maxRequests": 2,
                "maxCostMicrousd": 1_000_000,
                "timeoutMilliseconds": 1000,
                "maxTransientRetries": 0,
                "maxSchemaRepairs": 0
            },
            "deniedDataClasses": [
                "secret",
                "token",
                "private_key",
                "personally_identifiable_information",
                "unallowlisted_student_submission"
            ],
            "studentContentMode": "manifest_allowlist_only",
            "activatedAt": "2026-09-08T00:00:00.000Z"
        }))
        .expect("test policy is valid");
        InternalAgentLlmReviewRequest {
            task_run_id: TaskRunId::new(),
            project_id,
            course_id,
            frozen_submission_id: FrozenSubmissionId::new(),
            submission_artifact: ArtifactRef {
                artifact_id: ArtifactId::new(),
                store_binding: "minio-primary".to_owned(),
                object_version: "version-1".to_owned(),
                size_bytes: submission.len() as u64,
                media_type: "text/plain".to_owned(),
            },
            policy,
            files: vec![AgentLlmReviewFile {
                path: "submission.md".to_owned(),
                sha256: Sha256Digest::of_bytes(submission.as_bytes()).to_string(),
                content: submission.to_owned(),
            }],
            rubric: AgentLlmReviewRubric {
                artifact: ArtifactRef {
                    artifact_id: ArtifactId::new(),
                    store_binding: "minio-primary".to_owned(),
                    object_version: "version-1".to_owned(),
                    size_bytes: rubric.len() as u64,
                    media_type: "text/plain".to_owned(),
                },
                path: "rubric.md".to_owned(),
                sha256: Sha256Digest::of_bytes(rubric.as_bytes()).to_string(),
                content: rubric.to_owned(),
            },
            deadline_at,
        }
    }

    #[test]
    fn student_test_verdict_preserves_receipt_score() {
        for status in [
            OjTerminalStatus::Accepted,
            OjTerminalStatus::WrongAnswer,
            OjTerminalStatus::TimeLimitExceeded,
            OjTerminalStatus::MemoryLimitExceeded,
            OjTerminalStatus::OutputLimitExceeded,
            OjTerminalStatus::RuntimeError,
        ] {
            assert!(matches!(
                oj_receipt_result(
                    OjExecutionPhase::Test,
                    status,
                    17,
                    status.diagnostic_code().to_owned(),
                ),
                TerminalResult::Succeeded { score: Some(17) }
            ));
        }
    }

    #[test]
    fn compile_and_infrastructure_receipts_remain_failures() {
        assert!(matches!(
            oj_receipt_result(
                OjExecutionPhase::Compile,
                OjTerminalStatus::CompileError,
                0,
                OjTerminalStatus::CompileError.diagnostic_code().to_owned(),
            ),
            TerminalResult::Failed(_)
        ));
        assert!(matches!(
            oj_receipt_result(
                OjExecutionPhase::Test,
                OjTerminalStatus::InfrastructureError,
                0,
                OjTerminalStatus::InfrastructureError
                    .diagnostic_code()
                    .to_owned(),
            ),
            TerminalResult::Failed(_)
        ));
    }

    #[test]
    #[allow(clippy::expect_used)]
    fn recovered_terminal_receipt_is_accepted_after_request_deadline() {
        let now = OffsetDateTime::now_utc();
        let now = now
            .replace_nanosecond((now.nanosecond() / 1_000_000) * 1_000_000)
            .expect("test clock timestamp is valid");
        let deadline = UtcTimestamp::from_utc(now - time::Duration::minutes(1))
            .expect("test deadline is valid");
        let request = advisory_request(deadline);
        request.validate().expect("test request is valid");
        let request_sha256 = Sha256Digest::of_canonical(&request).expect("request hash is valid");
        let receipt = InternalAgentLlmReviewReceipt {
            task_run_id: request.task_run_id,
            request_sha256: request_sha256.to_string(),
            state: AgentLlmReviewState::Succeeded,
            review: None,
            usage: None,
            diagnostic_code: None,
            started_at: None,
            finished_at: None,
        };

        validate_advisory_receipt_hash(&request, request_sha256, &receipt)
            .expect("an existing receipt must be reconciled after deadline");
    }
}

fn image_digest(image: &str) -> Result<String, ExecutionError> {
    let (_, digest) = image
        .rsplit_once('@')
        .ok_or_else(|| ExecutionError::IdentityMismatch)?;
    if !digest.starts_with("sha256:") || digest.len() != 71 {
        return Err(ExecutionError::IdentityMismatch);
    }
    Ok(digest.to_owned())
}

fn evaluator_path(source: &str) -> Result<String, ExecutionError> {
    let path = source
        .strip_prefix("evaluator://")
        .unwrap_or(source)
        .to_owned();
    contracts::validate_relative_path(&path)
        .map_err(|_| ExecutionError::Backend("evaluator_path_invalid".to_owned()))?;
    Ok(path)
}

fn find_package_file<'a>(package: &'a ProblemPackage, path: &str) -> Option<&'a PackageFile> {
    package.files.iter().find(|file| file.path == path)
}

fn find_profile_file<'a>(package: &'a ProblemPackage, profile: &str) -> Option<&'a PackageFile> {
    let profile = profile.strip_prefix("evaluator://").unwrap_or(profile);
    [
        profile.to_owned(),
        format!("{profile}.json"),
        format!("profiles/{profile}.json"),
        format!("toolchains/{profile}.json"),
    ]
    .into_iter()
    .find_map(|path| find_package_file(package, &path))
}

fn resolve_playbook_path<'a>(
    package: &'a ProblemPackage,
    profile: &str,
) -> Option<&'a PackageFile> {
    let profile = profile.strip_prefix("evaluator://").unwrap_or(profile);
    [
        profile.to_owned(),
        format!("{profile}/playbook.yml"),
        format!("{profile}/site.yml"),
    ]
    .into_iter()
    .find_map(|path| find_package_file(package, &path))
}

fn package_file_binding(
    file: &PackageFile,
    bytes: &BTreeMap<contracts::ArtifactId, Vec<u8>>,
) -> Result<OjFileBinding, ExecutionError> {
    let body = bytes
        .get(&file.object.artifact_id)
        .ok_or_else(|| ExecutionError::Backend("package_object_unavailable".to_owned()))?;
    Ok(OjFileBinding {
        path: file.path.clone(),
        sha256: Sha256Digest::of_bytes(body),
        size_bytes: file.object.size_bytes,
    })
}

fn decode_archive_metadata(bytes: &[u8]) -> Result<BTreeMap<String, Vec<u8>>, ExecutionError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Archive {
        #[serde(rename = "apiVersion")]
        api_version: String,
        files: Vec<ArchiveFile>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct ArchiveFile {
        path: String,
        #[serde(rename = "contentBase64")]
        content_base64: String,
    }
    let archive: Archive = contracts::parse_strict_json(bytes)
        .map_err(|_| ExecutionError::Backend("frozen_archive_invalid".to_owned()))?;
    if archive.api_version != "evaluation.labweaver.io/frozen-submission-archive/v1" {
        return Err(ExecutionError::Backend("frozen_archive_invalid".to_owned()));
    }
    let mut result = BTreeMap::new();
    for file in archive.files {
        contracts::validate_relative_path(&file.path)
            .map_err(|_| ExecutionError::Backend("frozen_archive_invalid".to_owned()))?;
        let content = STANDARD
            .decode(file.content_base64)
            .map_err(|_| ExecutionError::Backend("frozen_archive_invalid".to_owned()))?;
        if result.insert(file.path, content).is_some() {
            return Err(ExecutionError::Backend("frozen_archive_invalid".to_owned()));
        }
    }
    Ok(result)
}

fn validate_frozen_archive(
    frozen: &FrozenSubmission,
    archive: &BTreeMap<String, Vec<u8>>,
) -> Result<(), ExecutionError> {
    if archive.len() != frozen.files.len()
        || frozen.files.iter().any(|file| {
            archive
                .get(&file.path)
                .is_none_or(|content| u64::try_from(content.len()).ok() != Some(file.size_bytes))
        })
    {
        return Err(ExecutionError::IdentityMismatch);
    }
    Ok(())
}

#[cfg(test)]
#[path = "../tests/support/mod.rs"]
mod runner_test_support;

#[cfg(test)]
mod probe_recovery_tests {
    #![allow(
        clippy::expect_used,
        clippy::too_many_lines,
        reason = "the integration regression intentionally keeps the durable and HTTP fixtures together"
    )]

    use std::{
        collections::{BTreeMap, BTreeSet},
        error::Error,
        io::Cursor,
        path::Path,
        sync::Arc,
        time::Duration,
    };

    use super::{
        EvaluationAttemptContext, EvaluationAttemptRunner, EvaluationExecutionConfiguration,
        EvaluationExecutionKind, KubernetesEvaluationRunner, PgEvaluationControlStore,
        PgFreezeStore, StepExecutionPlan,
    };
    use crate::{
        EvaluationReleaseReservation, EvaluationRunReservation, EvaluationStepLease,
        plan_deterministic_step,
    };
    use artifact_store::{S3Credential, S3ImmutableObjectStore, S3StoreConfig};
    use auth::{ServiceTokenClient, ServiceTokenClientConfig, TransportSecurityMode};
    use axum::{
        Json, Router,
        body::Body,
        extract::State,
        http::{Method, Request, StatusCode},
        response::{IntoResponse, Response},
        routing::{any, get, post},
    };
    use contracts::{
        ActorId, ApprovalId, ArtifactId, ArtifactRef, CandidateId, CourseId, EnvironmentId,
        FrozenSubmissionId, PolicyId, ProblemPackageId, ReleaseId, RetentionClass,
        RetentionDisposition, RetentionSnapshot, Revision, UtcTimestamp,
        authoring::{PackageFile, ProblemPackage, RuntimeKind},
        evaluation::{
            EvaluationExecutionBinding, EvaluationRelease, EvaluationRun, EvaluationRunIdentity,
            EvaluationRuntimeIdentity, EvaluationSpec, EvaluationStepRunState,
        },
        http::{
            AuthoringPublicationAdmissionBinding, InternalCreateEvaluationRunRequest,
            InternalPublishEvaluationReleaseRequest, TaskResourceStatus,
        },
        resource::{
            CapacityClaim, CapacityClaimState, ResourceLease, ResourceLeaseState, ResourceRequest,
            ResourceRequestState, ResourceTarget, WorkloadResources,
        },
        submission::{FrozenEnvironmentIdentity, FrozenFile, FrozenSubmission},
    };
    use hyper_util::{
        rt::{TokioExecutor, TokioIo},
        server::conn::auto::Builder,
        service::TowerToHyperService,
    };
    use persistence_sqlx::Sha256Digest;
    use rcgen::{
        BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa,
        KeyPair, KeyUsagePurpose,
    };
    use reqwest::{Certificate, Client, Url};
    use rustls::{ServerConfig, pki_types::PrivateKeyDer};
    use serde_json::{Value, json};
    use sqlx::{PgPool, postgres::PgPoolOptions};
    use tempfile::TempDir;
    use testcontainers::{ImageExt, runners::AsyncRunner};
    use testcontainers_modules::postgres::Postgres;
    use tokio::{
        net::TcpListener,
        sync::{Mutex, oneshot},
        task::JoinHandle,
    };
    use tokio_rustls::TlsAcceptor;
    use uuid::Uuid;

    use super::AnsibleProbeTargetConfiguration;
    use crate::agent_client::{AgentClient, AgentClientConfiguration};
    use crate::ansible_probe::{
        AnsibleProbeEvidenceReceipt, AnsibleProbeExecutionLimits, AnsibleProbeExecutionRequest,
        AnsibleProbeSshIdentity, AnsibleProbeTarget,
    };
    use crate::authoring_client::{
        AuthoringAdmissionClient, AuthoringAdmissionClientConfiguration,
    };
    use crate::environment_client::{
        EnvironmentExecutionBindingClient, EnvironmentExecutionBindingClientConfiguration,
    };
    use crate::oj_executor::OjExecutorConfiguration;
    use crate::resource_client::{ResourceClient, ResourceClientConfiguration};

    const NAMESPACE: &str = "evaluation-tests";
    const MOCK_TOKEN: &str = "eyJhbGciOiJub25lIn0.eyJhdWQiOiJyZXNvdXJjZSJ9.sig";

    #[derive(Default)]
    struct MockHttpState {
        objects: BTreeMap<String, Value>,
        pods: Option<Value>,
        resource_status: Option<Value>,
        deleted: BTreeSet<String>,
        calls: Vec<(Method, String)>,
    }

    struct MockCluster {
        base_url: Url,
        ca_pem: String,
        state: Arc<Mutex<MockHttpState>>,
        shutdown: Option<oneshot::Sender<()>>,
        task: Option<JoinHandle<()>>,
    }

    impl MockCluster {
        async fn install_probe(
            &self,
            request: &AnsibleProbeExecutionRequest,
            status: &TaskResourceStatus,
        ) -> Result<(), Box<dyn Error>> {
            let request_sha256 = request.request_sha256()?.to_string();
            let labels = json!({
                "labweaver.io/managed-by": "evaluation-service",
                "labweaver.io/run-id": request.run_id,
                "labweaver.io/step-run-id": request.step_run_id,
                "labweaver.io/attempt-id": request.attempt_id,
            });
            let annotations = json!({
                "labweaver.io/request-sha256": request_sha256,
                "labweaver.io/trace-id": request.trace_id,
            });
            let entries = probe_entries(request);
            let mut state = self.state.lock().await;
            for (index, (api_version, resource, name, kind)) in entries.iter().enumerate() {
                let metadata = json!({
                    "name": name,
                    "namespace": NAMESPACE,
                    "uid": format!("probe-object-{index}"),
                    "resourceVersion": format!("{index_plus_one}", index_plus_one = index + 1),
                    "labels": labels,
                    "annotations": annotations,
                });
                let mut object = json!({
                    "apiVersion": api_version,
                    "kind": kind,
                    "metadata": metadata,
                });
                if *resource == "jobs" {
                    object["status"] = json!({"succeeded": 1});
                }
                state
                    .objects
                    .insert(kube_path(api_version, resource, name), object);
            }
            let job_name = entries[0].2.clone();
            let pod_metadata = json!({
                "name": format!("{job_name}-pod"),
                "namespace": NAMESPACE,
                "uid": "probe-pod-uid",
                "resourceVersion": "7",
                "labels": labels,
                "annotations": annotations,
            });
            let receipt = AnsibleProbeEvidenceReceipt {
                schema_version: crate::ansible_probe::ANSIBLE_PROBE_EVIDENCE_RECEIPT_SCHEMA_VERSION
                    .to_owned(),
                run_id: request.run_id,
                step_run_id: request.step_run_id,
                attempt_id: request.attempt_id,
                trace_id: request.trace_id.clone(),
                request_sha256: request.request_sha256()?,
                evidence_sha256: Sha256Digest::of_bytes(b"probe-evidence"),
                evidence_size_bytes: 1,
                terminal_status: crate::ansible_probe::AnsibleProbeTerminalStatus::Succeeded,
                diagnostic_code: "LW_AP_SUCCEEDED".to_owned(),
                passed_assertions: u32::try_from(request.assertions.len())?,
                total_assertions: u32::try_from(request.assertions.len())?,
            };
            let pod = json!({
                "apiVersion": "v1",
                "kind": "Pod",
                "metadata": pod_metadata,
                "status": {
                    "containerStatuses": [{
                        "name": "ansible-probe",
                        "state": {"terminated": {
                            "startedAt": "2026-09-09T00:00:00.000Z",
                            "finishedAt": "2026-09-09T00:00:01.000Z",
                            "message": serde_json::to_string(&receipt)?,
                        }}
                    }]
                }
            });
            state.pods = Some(json!({"items": [pod]}));
            state.resource_status = Some(serde_json::to_value(status)?);
            Ok(())
        }

        async fn calls(&self) -> Vec<(Method, String)> {
            self.state.lock().await.calls.clone()
        }

        async fn stop(&mut self) {
            if let Some(shutdown) = self.shutdown.take() {
                let _ = shutdown.send(());
            }
            if let Some(task) = self.task.take() {
                let _ = task.await;
            }
        }
    }

    impl Drop for MockCluster {
        fn drop(&mut self) {
            if let Some(task) = self.task.take() {
                task.abort();
            }
        }
    }

    struct AuthorityHandle {
        issuer: String,
        task: JoinHandle<()>,
    }

    impl Drop for AuthorityHandle {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    struct DbFixture {
        _container: testcontainers::ContainerAsync<Postgres>,
        pool: PgPool,
        store: PgEvaluationControlStore,
        project_id: contracts::ProjectId,
        course_id: CourseId,
        actor_id: ActorId,
        release: contracts::evaluation::EvaluationRelease,
        run: EvaluationRun,
    }

    #[tokio::test]
    async fn probe_recovery_reuses_task_run_and_cleans_exact_objects_without_second_job()
    -> Result<(), Box<dyn Error>> {
        let mut cluster = spawn_cluster().await?;
        let authority = spawn_authority().await?;
        let temp = TempDir::new()?;
        let fixture = DbFixture::start().await?;
        let runner = build_runner(&cluster, &authority, &temp, &fixture.pool).await?;
        let first_lease = fixture
            .store
            .claim_next_step("probe-worker-a", Duration::from_secs(30))
            .await?
            .ok_or("probe step was not claimable")?;
        let first_context = context(&fixture, &first_lease)?;
        let request = probe_request(&first_context)?;
        let intent = super::execution_resources(
            &first_context,
            NAMESPACE,
            EvaluationExecutionKind::AnsibleProbe,
            &request,
            Vec::new(),
        )?;
        fixture
            .store
            .persist_execution_intent(&first_lease, &intent)
            .await?;
        let resource_status = resource_status(&first_lease, &fixture, false)?;
        cluster.install_probe(&request, &resource_status).await?;
        expire_lease(&fixture.pool, first_lease.step_run_id).await?;
        let recovered_lease = fixture
            .store
            .claim_next_step("probe-worker-b", Duration::from_secs(30))
            .await?
            .ok_or("expired probe step was not reassigned")?;
        assert_eq!(recovered_lease.task_run_id, first_lease.task_run_id);
        assert_eq!(recovered_lease.step_run_id, first_lease.step_run_id);
        assert_eq!(recovered_lease.attempt, first_lease.attempt);
        let recovered_context = context(&fixture, &recovered_lease)?;
        let completion = runner.execute(recovered_context).await?;
        assert_eq!(completion.state, EvaluationStepRunState::Succeeded);
        assert!(completion.cleanup_verified);
        fixture
            .store
            .complete_step(
                fixture.project_id,
                Some(fixture.course_id),
                fixture.run.id,
                recovered_lease.step_run_id,
                recovered_lease.attempt,
                &recovered_lease.worker_id,
                &recovered_lease.runtime_identity,
                recovered_lease.lease_token(),
                &completion,
                &recovered_lease.trace_id,
            )
            .await?;

        let attempt_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM evaluation.evaluation_step_attempts WHERE step_run_id=$1",
        )
        .bind(first_lease.step_run_id.as_uuid())
        .fetch_one(&fixture.pool)
        .await?;
        assert_eq!(attempt_count, 1);
        let persisted_task: Uuid = sqlx::query_scalar(
            "SELECT task_run_id FROM evaluation.evaluation_step_attempts WHERE step_run_id=$1",
        )
        .bind(first_lease.step_run_id.as_uuid())
        .fetch_one(&fixture.pool)
        .await?;
        assert_eq!(persisted_task, first_lease.task_run_id.as_uuid());

        let calls = cluster.calls().await;
        let kube_mutations = calls
            .iter()
            .filter(|(method, path)| {
                (path.starts_with("/api/") || path.starts_with("/apis/")) && *method != Method::GET
            })
            .collect::<Vec<_>>();
        assert_eq!(
            kube_mutations
                .iter()
                .filter(|(method, _)| *method == Method::DELETE)
                .count(),
            6,
            "recovery must delete the six persisted attempt objects"
        );
        assert!(
            kube_mutations
                .iter()
                .all(|(method, _)| *method == Method::DELETE),
            "recovery must not apply a second Kubernetes Job bundle"
        );
        assert_eq!(
            calls
                .iter()
                .filter(|(method, path)| *method == Method::GET && path.ends_with("/pods"))
                .count(),
            1,
            "the recovered Job must be observed exactly once"
        );
        assert_eq!(
            calls
                .iter()
                .filter(|(method, path)| *method == Method::POST && path.starts_with("/apis/"))
                .count(),
            0,
            "the Kubernetes boundary must not receive a second create/apply request"
        );
        assert_eq!(
            calls
                .iter()
                .filter(|(_, path)| path
                    == &format!(
                        "/internal/v1/task-resources/{}/release",
                        first_lease.task_run_id
                    ))
                .count(),
            1,
            "the recovered Resource claim is released once after cleanup"
        );
        cluster.stop().await;
        Ok(())
    }

    #[tokio::test]
    async fn probe_recovery_rejects_stale_step_revision_and_job_uid_before_pod_observation()
    -> Result<(), Box<dyn Error>> {
        let mut cluster = spawn_cluster().await?;
        let authority = spawn_authority().await?;
        let temp = TempDir::new()?;
        let fixture = DbFixture::start().await?;
        let runner = build_runner(&cluster, &authority, &temp, &fixture.pool).await?;
        let lease = fixture
            .store
            .claim_next_step("probe-identity-worker", Duration::from_secs(30))
            .await?
            .ok_or("probe step was not claimable")?;
        let valid_context = context(&fixture, &lease)?;
        let request = probe_request(&valid_context)?;
        let intent = super::execution_resources(
            &valid_context,
            NAMESPACE,
            EvaluationExecutionKind::AnsibleProbe,
            &request,
            Vec::new(),
        )?;
        fixture
            .store
            .persist_execution_intent(&lease, &intent)
            .await?;
        let resource_status = resource_status(&lease, &fixture, false)?;
        cluster.install_probe(&request, &resource_status).await?;

        let mut stale_lease = lease.clone();
        stale_lease.revision = Revision::new(1)?;
        let stale_error = runner.execute(context(&fixture, &stale_lease)?).await;
        assert!(matches!(
            stale_error,
            Err(crate::execution::ExecutionError::Control(
                crate::control_plane::EvaluationControlStoreError::LeaseLost
            ))
        ));
        assert!(
            cluster.calls().await.is_empty(),
            "stale StepRun revision must fence before HTTP observation"
        );

        let wrong_objects = probe_entries(&request)
            .into_iter()
            .enumerate()
            .map(|(index, (api_version, resource, name, _))| {
                (
                    api_version.to_owned(),
                    resource.to_owned(),
                    name,
                    if index == 0 {
                        "stale-job-uid".to_owned()
                    } else {
                        format!("probe-object-{index}")
                    },
                )
            })
            .collect();
        let recovered_resources = super::execution_resources(
            &valid_context,
            NAMESPACE,
            EvaluationExecutionKind::AnsibleProbe,
            &request,
            wrong_objects,
        )?;
        fixture
            .store
            .mark_execution_started(&lease, None, &recovered_resources)
            .await?;
        let identity_error = runner.execute(valid_context).await;
        assert!(matches!(
            identity_error,
            Err(crate::execution::ExecutionError::Backend(code))
                if code == "probe_observe_failed"
        ));
        let calls = cluster.calls().await;
        assert_eq!(
            calls
                .iter()
                .filter(|(method, path)| *method == Method::GET && path.ends_with("/pods"))
                .count(),
            0,
            "a recovered Job UID mismatch must fail before Pod observation"
        );
        assert_eq!(
            calls
                .iter()
                .filter(|(method, path)| *method == Method::DELETE
                    || (*method == Method::PATCH && path.starts_with("/apis/")))
                .count(),
            0,
            "identity rejection must not clean or mutate an unverified object"
        );
        assert_eq!(
            calls
                .iter()
                .filter(|(method, path)| {
                    *method == Method::GET && path.starts_with("/internal/v1/task-resources/")
                })
                .count(),
            1,
            "identity verification may load the exact Resource claim before failing closed"
        );
        assert!(
            calls.iter().all(|(method, path)| {
                !path.starts_with("/internal/v1/task-resources/") || *method == Method::GET
            }),
            "identity rejection must not release an unverified Resource claim"
        );
        cluster.stop().await;
        Ok(())
    }

    async fn spawn_authority() -> Result<AuthorityHandle, Box<dyn Error>> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        let issuer = format!("http://localhost:{port}/realms/test");
        let discovery_issuer = issuer.clone();
        let router = Router::new()
            .route(
                "/realms/test/.well-known/openid-configuration",
                get(move || {
                    let issuer = discovery_issuer.clone();
                    async move {
                        Json(json!({
                            "issuer": issuer,
                            "authorization_endpoint": format!("{issuer}/authorize"),
                            "token_endpoint": format!("{issuer}/token"),
                            "jwks_uri": format!("{issuer}/jwks"),
                            "response_types_supported": ["code"],
                            "subject_types_supported": ["public"],
                            "id_token_signing_alg_values_supported": ["ES256"],
                            "grant_types_supported": ["authorization_code", "client_credentials"]
                        }))
                    }
                }),
            )
            .route(
                "/realms/test/token",
                post(|| async {
                    Json(json!({
                        "access_token": MOCK_TOKEN,
                        "token_type": "Bearer",
                        "expires_in": 300
                    }))
                }),
            )
            .route(
                "/realms/test/jwks",
                get(|| async { Json(json!({"keys": []})) }),
            );
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Ok(AuthorityHandle { issuer, task })
    }

    async fn spawn_cluster() -> Result<MockCluster, Box<dyn Error>> {
        let ca = test_ca()?;
        let (certificate_pem, private_key_pem) = leaf_certificate(&ca)?;
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        let base_url = Url::parse(&format!("https://localhost:{port}/"))?;
        let state = Arc::new(Mutex::new(MockHttpState::default()));
        let router = Router::new()
            .fallback(any(mock_request))
            .with_state(state.clone());
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let tls = tls_config(&certificate_pem, &private_key_pem)?;
        let task = tokio::spawn(serve_tls(listener, router, tls, shutdown_rx));
        Ok(MockCluster {
            base_url,
            ca_pem: ca.pem(),
            state,
            shutdown: Some(shutdown_tx),
            task: Some(task),
        })
    }

    async fn mock_request(
        State(state): State<Arc<Mutex<MockHttpState>>>,
        request: Request<Body>,
    ) -> Response {
        let method = request.method().clone();
        let path = request.uri().path().to_owned();
        let mut state = state.lock().await;
        state.calls.push((method.clone(), path.clone()));
        if method == Method::GET && path.ends_with("/pods") {
            return state.pods.clone().map_or_else(
                || StatusCode::NOT_FOUND.into_response(),
                |pods| (StatusCode::OK, Json(pods)).into_response(),
            );
        }
        let segments = path.trim_matches('/').split('/').collect::<Vec<_>>();
        if segments.first() == Some(&"internal") {
            if method == Method::GET && segments.len() == 4 {
                return state.resource_status.clone().map_or_else(
                    || StatusCode::NOT_FOUND.into_response(),
                    |status| (StatusCode::OK, Json(status)).into_response(),
                );
            }
            if method == Method::POST && segments.last() == Some(&"release") {
                if let Some(status) = state.resource_status.as_mut() {
                    status["cleanupConfirmed"] = json!(true);
                    return (StatusCode::OK, Json(status.clone())).into_response();
                }
                return StatusCode::NOT_FOUND.into_response();
            }
            return StatusCode::METHOD_NOT_ALLOWED.into_response();
        }
        if method == Method::GET {
            if state.deleted.contains(&path) {
                return StatusCode::NOT_FOUND.into_response();
            }
            return state.objects.get(&path).cloned().map_or_else(
                || StatusCode::NOT_FOUND.into_response(),
                |object| (StatusCode::OK, Json(object)).into_response(),
            );
        }
        if method == Method::DELETE {
            if state.objects.contains_key(&path) {
                state.deleted.insert(path);
                return (StatusCode::OK, Json(json!({}))).into_response();
            }
            return StatusCode::NOT_FOUND.into_response();
        }
        StatusCode::METHOD_NOT_ALLOWED.into_response()
    }

    async fn serve_tls(
        listener: TcpListener,
        router: Router,
        config: Arc<ServerConfig>,
        mut shutdown: oneshot::Receiver<()>,
    ) {
        let acceptor = TlsAcceptor::from(config);
        loop {
            let accepted = tokio::select! {
                result = listener.accept() => result,
                _ = &mut shutdown => return,
            };
            let Ok((stream, _)) = accepted else { return };
            let acceptor = acceptor.clone();
            let router = router.clone();
            tokio::spawn(async move {
                let Ok(stream) = acceptor.accept(stream).await else {
                    return;
                };
                let service = TowerToHyperService::new(router);
                let connection = Builder::new(TokioExecutor::new())
                    .serve_connection_with_upgrades(TokioIo::new(stream), service)
                    .into_owned();
                let _ = connection.await;
            });
        }
    }

    fn tls_config(
        certificate_pem: &str,
        private_key_pem: &str,
    ) -> Result<Arc<ServerConfig>, Box<dyn Error>> {
        let certificates = rustls_pemfile::certs(&mut Cursor::new(certificate_pem.as_bytes()))
            .collect::<Result<Vec<_>, _>>()?;
        let key: PrivateKeyDer<'static> =
            rustls_pemfile::private_key(&mut Cursor::new(private_key_pem.as_bytes()))?
                .ok_or("private key missing")?;
        let mut config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certificates, key)?;
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Arc::new(config))
    }

    fn test_ca() -> Result<CertifiedIssuer<'static, KeyPair>, rcgen::Error> {
        let mut parameters = CertificateParams::new(Vec::<String>::new())?;
        parameters.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        parameters.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
        ];
        CertifiedIssuer::self_signed(parameters, KeyPair::generate()?)
    }

    fn leaf_certificate(
        ca: &CertifiedIssuer<'static, KeyPair>,
    ) -> Result<(String, String), rcgen::Error> {
        let mut parameters = CertificateParams::new(vec!["localhost".to_owned()])?;
        parameters.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        parameters.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let key = KeyPair::generate()?;
        let certificate = parameters.signed_by(&key, ca)?;
        Ok((certificate.pem(), key.serialize_pem()))
    }

    async fn build_runner(
        cluster: &MockCluster,
        authority: &AuthorityHandle,
        temp: &TempDir,
        pool: &PgPool,
    ) -> Result<KubernetesEvaluationRunner, Box<dyn Error>> {
        let ca_file = temp.path().join("cluster-ca.pem");
        let token_file = temp.path().join("kubernetes.token");
        std::fs::write(&ca_file, cluster.ca_pem.as_bytes())?;
        std::fs::write(&token_file, "mock-kubernetes-token")?;
        let oidc_http = Client::builder().no_proxy().build()?;
        let token_config = ServiceTokenClientConfig::new(
            &authority.issuer,
            "evaluation-test-client".to_owned(),
            "evaluation-test-secret".to_owned(),
            "resource".to_owned(),
            BTreeSet::from(["resource.task.read".to_owned()]),
            1,
            TransportSecurityMode::InsecureTestOnly,
        )?;
        let token_client = Arc::new(ServiceTokenClient::discover(token_config, oidc_http).await?);
        let ca = Certificate::from_pem(cluster.ca_pem.as_bytes())?;
        let http = Client::builder()
            .no_proxy()
            .https_only(true)
            .tls_built_in_root_certs(false)
            .add_root_certificate(ca)
            .build()?;
        let base = cluster.base_url.clone();
        let resource = ResourceClient::new(
            ResourceClientConfiguration {
                base_uri: base.clone(),
                ca_file: ca_file.clone(),
                timeout_milliseconds: 5_000,
                max_request_bytes: 1024 * 1024,
                max_response_bytes: 1024 * 1024,
                audience: "resource".to_owned(),
            },
            http.clone(),
            token_client.clone(),
            scopes(&[
                "resource.task.create",
                "resource.task.read",
                "resource.task.claim",
                "resource.task.ack",
                "resource.task.release",
                "resource.task.cancel",
                "resource.usage.record",
            ]),
        )?;
        let agent = AgentClient::new(
            AgentClientConfiguration {
                base_uri: base.clone(),
                ca_file: ca_file.clone(),
                timeout_milliseconds: 5_000,
                max_request_bytes: 1024 * 1024,
                max_response_bytes: 1024 * 1024,
                audience: "agent".to_owned(),
            },
            http.clone(),
            token_client.clone(),
            scopes(&[
                "agent.llm_review.create",
                "agent.llm_review.read",
                "agent.llm_review.cancel",
            ]),
        )?;
        let authoring = AuthoringAdmissionClient::new(
            AuthoringAdmissionClientConfiguration {
                base_uri: base.clone(),
                ca_file: ca_file.clone(),
                timeout_milliseconds: 5_000,
                max_request_bytes: 1024 * 1024,
                max_response_bytes: 1024 * 1024,
                audience: "control".to_owned(),
            },
            http.clone(),
            token_client.clone(),
            scopes(&["control.authoring.read", "control.llm_policy.read"]),
        )?;
        let environment = EnvironmentExecutionBindingClient::new(
            EnvironmentExecutionBindingClientConfiguration {
                base_uri: base.clone(),
                ca_file: ca_file.clone(),
                timeout_milliseconds: 5_000,
                max_request_bytes: 1024 * 1024,
                max_response_bytes: 256 * 1024,
                audience: "environment".to_owned(),
            },
            http,
            token_client,
            scopes(&["environment:resolve_evaluation_execution_binding"]),
        )?;
        let objects = Arc::new(
            S3ImmutableObjectStore::new(
                S3StoreConfig {
                    binding: "test-store".to_owned(),
                    endpoint: base.clone(),
                    bucket: "test-bucket".to_owned(),
                    region: "us-east-1".to_owned(),
                    object_prefix: "evaluation-tests".to_owned(),
                    upload_ttl_seconds: 60,
                    max_object_bytes: 1024 * 1024,
                    force_path_style: true,
                    ca_bundle_file: None,
                },
                S3Credential {
                    access_key_id: "test-access".to_owned(),
                    secret_access_key: "test-secret".to_owned(),
                    session_token: None,
                },
            )
            .await?,
        );
        let kube =
            |config: &Path| crate::ansible_probe_executor::AnsibleProbeExecutorConfiguration {
                kubernetes_api_server: base.clone(),
                kubernetes_bearer_token_file: token_file.clone(),
                kubernetes_ca_file: config.to_owned(),
                runner_namespace: NAMESPACE.to_owned(),
                request_timeout_milliseconds: 5_000,
            };
        let configuration = EvaluationExecutionConfiguration {
            runner_namespace: NAMESPACE.to_owned(),
            worker_id: "probe-runner".to_owned(),
            worker_lease_seconds: 60,
            scheduler_poll_interval_milliseconds: 100,
            oj_service_account_name: "oj-runner".to_owned(),
            ansible_probe_service_account_name: "ansible-probe".to_owned(),
            image_pull_secret_name: "pull-secret".to_owned(),
            resource_poll_interval_milliseconds: 100,
            resource_approval_timeout_seconds: 10,
            execution_observe_poll_interval_milliseconds: 100,
            cleanup_timeout_seconds: 5,
            environment: EnvironmentExecutionBindingClientConfiguration {
                base_uri: base.clone(),
                ca_file: ca_file.clone(),
                timeout_milliseconds: 5_000,
                max_request_bytes: 1024 * 1024,
                max_response_bytes: 256 * 1024,
                audience: "environment".to_owned(),
            },
            ansible_probe: AnsibleProbeTargetConfiguration {
                wall_time_seconds: 60,
                facts_max_bytes: 1024 * 1024,
                output_max_bytes: 64 * 1024,
                max_assertions: 8,
            },
            oj: OjExecutorConfiguration {
                kubernetes_api_server: base.clone(),
                kubernetes_bearer_token_file: token_file.clone(),
                kubernetes_ca_file: ca_file.clone(),
                runner_namespace: NAMESPACE.to_owned(),
                request_timeout_milliseconds: 5_000,
            },
            ansible_probe_executor: kube(&ca_file),
        };
        Ok(KubernetesEvaluationRunner::new(
            PgEvaluationControlStore::new(pool.clone()),
            PgFreezeStore::new(pool.clone()),
            objects,
            resource,
            agent,
            authoring,
            environment,
            configuration,
            None,
        )?)
    }

    fn scopes(values: &[&str]) -> BTreeSet<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn kube_path(api_version: &str, resource: &str, name: &str) -> String {
        let prefix = if api_version == "v1" {
            "/api/v1".to_owned()
        } else {
            format!("/apis/{api_version}")
        };
        format!("{prefix}/namespaces/{NAMESPACE}/{resource}/{name}")
    }

    fn probe_entries(
        request: &AnsibleProbeExecutionRequest,
    ) -> Vec<(&'static str, &'static str, String, &'static str)> {
        let job = format!("lw-ap-{}", &request.attempt_id.simple().to_string()[..20]);
        vec![
            ("batch/v1", "jobs", job.clone(), "Job"),
            (
                "networking.k8s.io/v1",
                "networkpolicies",
                job.clone(),
                "NetworkPolicy",
            ),
            ("v1", "configmaps", job, "ConfigMap"),
            (
                "v1",
                "secrets",
                format!(
                    "lw-ap-{}-materializer",
                    &request.attempt_id.simple().to_string()[..20]
                ),
                "Secret",
            ),
            (
                "v1",
                "secrets",
                request.ssh_identity.private_key_secret.clone(),
                "Secret",
            ),
            (
                "v1",
                "secrets",
                request.ssh_identity.certificate_secret.clone(),
                "Secret",
            ),
        ]
    }

    async fn expire_lease(
        pool: &PgPool,
        step_run_id: contracts::EvaluationStepRunId,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "UPDATE evaluation.evaluation_step_attempts SET lease_expires_at=clock_timestamp() - interval '1 second' WHERE step_run_id=$1",
        )
        .bind(step_run_id.as_uuid())
        .execute(pool)
        .await?;
        Ok(())
    }

    fn context(
        fixture: &DbFixture,
        lease: &EvaluationStepLease,
    ) -> Result<EvaluationAttemptContext, Box<dyn Error>> {
        let run = fixture.run.clone();
        let release = fixture.release.clone();
        let source_step = release
            .evaluation_spec
            .body()
            .steps()
            .iter()
            .find(|candidate| candidate.id() == lease.step_id)
            .cloned()
            .ok_or("leased step missing from release")?;
        Ok(EvaluationAttemptContext {
            lease: lease.clone(),
            run,
            release,
            step: source_step.clone(),
            execution_plan: plan_deterministic_step(&source_step)?,
            cancellation: tokio_util::sync::CancellationToken::new(),
            lease_lost: tokio_util::sync::CancellationToken::new(),
        })
    }

    fn probe_request(
        context: &EvaluationAttemptContext,
    ) -> Result<AnsibleProbeExecutionRequest, Box<dyn Error>> {
        let StepExecutionPlan::AnsibleProbe {
            playbook_profile,
            module_allowlist,
            assertions,
        } = &context.execution_plan
        else {
            return Err("test release step is not an Ansible probe".into());
        };
        let (private_key_secret, certificate_secret) =
            super::attempt_ssh_secret_names(context.lease.task_run_id.as_uuid());
        let request = AnsibleProbeExecutionRequest {
            schema_version: crate::ansible_probe::ANSIBLE_PROBE_EXECUTION_SCHEMA_VERSION.to_owned(),
            run_id: context.lease.run_id.as_uuid(),
            step_run_id: context.lease.step_run_id.as_uuid(),
            attempt_id: context.lease.task_run_id.as_uuid(),
            trace_id: context.lease.trace_id.clone(),
            runner_image_digest: context.run.identity.runtime_identity.runner_image.clone(),
            playbook_profile: format!("{playbook_profile}/playbook.yml"),
            module_allowlist: module_allowlist.clone(),
            read_only: true,
            assertions: assertions.clone(),
            target: AnsibleProbeTarget {
                host: "192.168.56.10".parse()?,
                port: 22,
                username: "labweaver".to_owned(),
            },
            source_identity: "environment-source-revision-1".to_owned(),
            ssh_identity: AnsibleProbeSshIdentity {
                private_key_secret,
                certificate_secret,
                expected_host_key_sha256: Sha256Digest::of_bytes(b"host-key"),
            },
            limits: AnsibleProbeExecutionLimits {
                wall_time_seconds: 60,
                facts_max_bytes: 1024 * 1024,
                output_max_bytes: 64 * 1024,
                max_assertions: 8,
            },
            evaluation_spec_sha256: Sha256Digest::of_canonical(&context.release.evaluation_spec)?,
        };
        request.validate()?;
        Ok(request)
    }

    fn resource_status(
        lease: &EvaluationStepLease,
        fixture: &DbFixture,
        cleanup_confirmed: bool,
    ) -> Result<TaskResourceStatus, Box<dyn Error>> {
        let now: UtcTimestamp = "2026-09-09T00:00:00.000Z".parse()?;
        let later: UtcTimestamp = "2026-09-09T00:10:00.000Z".parse()?;
        let resources = WorkloadResources {
            cpu_millicores: 1_000,
            memory_bytes: 512 * 1024 * 1024,
            storage_bytes: 128 * 1024 * 1024,
            gpu: None,
        };
        let request_id = contracts::ResourceRequestId::new();
        let claim_id = contracts::CapacityClaimId::new();
        let lease_id = contracts::LeaseId::new();
        let revision = Revision::new(1)?;
        let request = ResourceRequest {
            id: request_id,
            generation: 1,
            request_key: format!("evaluation-{}", lease.task_run_id),
            requester_id: fixture.actor_id,
            project_id: fixture.project_id,
            course_id: Some(fixture.course_id),
            target: ResourceTarget::Task {
                task_run_id: lease.task_run_id,
            },
            requested_resources: resources.clone(),
            requested_duration_seconds: 90,
            state: ResourceRequestState::Active,
            revision,
            created_at: now,
            updated_at: now,
            diagnostic_code: None,
        };
        let claim = CapacityClaim {
            id: claim_id,
            request_id,
            approval_id: contracts::ResourceApprovalId::new(),
            provider_binding: "kubernetes".to_owned(),
            workload_resources: resources.clone(),
            quota_resources: resources,
            gpu_allocation: None,
            state: CapacityClaimState::HandedOff,
            revision,
        };
        let lease_record = ResourceLease {
            id: lease_id,
            request_id,
            claim_id,
            state: ResourceLeaseState::Active,
            revision,
            active_from: Some(now),
            expires_at: Some(later),
            revoke_reason_code: None,
            created_at: now,
            updated_at: now,
        };
        Ok(TaskResourceStatus {
            task_run_id: lease.task_run_id,
            project_id: fixture.project_id,
            owner_id: fixture.actor_id,
            execution_namespace: Some(NAMESPACE.to_owned()),
            claim_revision: revision,
            lease_revision: revision,
            cleanup_confirmed,
            request,
            claim,
            lease: lease_record,
        })
    }

    impl DbFixture {
        async fn start() -> Result<Self, Box<dyn Error>> {
            let container = Postgres::default().with_tag("17.5-alpine").start().await?;
            let database_url = format!(
                "postgres://postgres:postgres@127.0.0.1:{}/postgres",
                container.get_host_port_ipv4(5432).await?
            );
            let pool = PgPoolOptions::new()
                .max_connections(4)
                .connect(&database_url)
                .await?;
            apply_evaluation_migrations(&pool).await?;
            let store = PgEvaluationControlStore::new(pool.clone());
            let project_id = contracts::ProjectId::new();
            let course_id = CourseId::new();
            let actor_id = ActorId::new();
            let frozen = frozen_submission(project_id, course_id, actor_id)?;
            insert_frozen(&pool, &frozen).await?;
            let publish_request = publish_request(project_id, course_id, actor_id)?;
            let release = match store
                .publish_release(
                    &publish_request,
                    &contracts::http::IdempotencyKey::parse("probe-release-test")?,
                    "2026-09-09T00:00:00.000Z".parse()?,
                    "probe-recovery-test",
                )
                .await?
            {
                EvaluationReleaseReservation::Created(value)
                | EvaluationReleaseReservation::Replayed(value) => value,
            };
            let run_request = InternalCreateEvaluationRunRequest {
                project_id,
                course_id: Some(course_id),
                release_id: release.id,
                release_revision: release.revision,
                frozen_submission_id: frozen.id,
                actor_id,
                identity: EvaluationRunIdentity {
                    runtime_identity: release.runtime_identity.clone(),
                    trace_id: "probe-recovery-test".to_owned(),
                },
            };
            let run = match store
                .create_run(
                    &run_request,
                    &contracts::http::IdempotencyKey::parse("probe-run-test-key")?,
                    "2026-09-09T00:00:00.000Z".parse()?,
                    "probe-recovery-test",
                    &admission(&release),
                )
                .await?
            {
                EvaluationRunReservation::Created(value)
                | EvaluationRunReservation::Replayed(value) => value,
            };
            Ok(Self {
                _container: container,
                pool,
                store,
                project_id,
                course_id,
                actor_id,
                release,
                run,
            })
        }
    }

    async fn apply_evaluation_migrations(pool: &PgPool) -> Result<(), Box<dyn Error>> {
        // Use the repository's migration verifier so this regression cannot silently run against
        // a schema older than the durable execution checkpoint it is exercising.
        super::runner_test_support::apply_evaluation_migrations(pool).await
    }

    fn frozen_submission(
        project_id: contracts::ProjectId,
        course_id: CourseId,
        actor_id: ActorId,
    ) -> Result<FrozenSubmission, Box<dyn Error>> {
        Ok(FrozenSubmission {
            id: FrozenSubmissionId::new(),
            project_id,
            course_id: Some(course_id),
            actor_id,
            agent_run_id: contracts::AgentRunId::new(),
            attempt: 1,
            manifest_revision: Revision::new(1)?,
            files: vec![FrozenFile {
                path: "answer.txt".to_owned(),
                size_bytes: 1,
                media_type: "text/plain".to_owned(),
            }],
            object: ArtifactRef {
                artifact_id: ArtifactId::new(),
                store_binding: "test-store".to_owned(),
                object_version: "v1".to_owned(),
                size_bytes: 7,
                media_type: "application/zip".to_owned(),
            },
            content_sha256: Sha256Digest::of_bytes(b"archive").to_string(),
            environment: FrozenEnvironmentIdentity {
                environment_id: EnvironmentId::new(),
                environment_revision: Revision::new(1)?,
                release_id: ReleaseId::new(),
                release_version: 1,
                runtime_kind: RuntimeKind::VirtualMachine,
                build_request_id: None,
            },
            retention: RetentionSnapshot {
                policy_id: PolicyId::new(),
                policy_revision: Revision::new(1)?,
                class: RetentionClass::StudentSubmission,
                retain_until: "2027-01-01T00:00:00.000Z".parse()?,
                disposition: RetentionDisposition::Delete,
            },
            system_facts: BTreeMap::new(),
            frozen_at: "2026-09-09T00:00:00.000Z".parse()?,
            derived_archive: None,
        })
    }

    async fn insert_frozen(pool: &PgPool, frozen: &FrozenSubmission) -> Result<(), Box<dyn Error>> {
        let now = frozen.frozen_at.get();
        sqlx::query(
            "INSERT INTO evaluation.frozen_submissions
             (frozen_submission_id,project_id,course_id,environment_id,manifest_sha256,content_sha256,
              schema_version,tool_version,contract,frozen_at,idempotency_key,source_identity_sha256,
              object_key,object_version)
             VALUES ($1,$2,$3,$4,$5,$6,'submission.freeze/v1','probe-recovery-test',$7,$8,$9,$10,$11,$12)",
        )
        .bind(frozen.id.as_uuid())
        .bind(frozen.project_id.as_uuid())
        .bind(frozen.course_id.map(CourseId::as_uuid))
        .bind(frozen.environment.environment_id.as_uuid())
        .bind(Sha256Digest::of_bytes(b"manifest").to_string())
        .bind(&frozen.content_sha256)
        .bind(serde_json::to_value(frozen)?)
        .bind(now)
        .bind(format!("freeze:{}", frozen.id))
        .bind(Sha256Digest::of_bytes(b"source-identity").to_string())
        .bind(format!("frozen/{}", frozen.id))
        .bind(&frozen.object.object_version)
        .execute(pool)
        .await?;
        Ok(())
    }

    fn publish_request(
        project_id: contracts::ProjectId,
        course_id: CourseId,
        actor_id: ActorId,
    ) -> Result<InternalPublishEvaluationReleaseRequest, Box<dyn Error>> {
        Ok(InternalPublishEvaluationReleaseRequest {
            project_id,
            course_id: Some(course_id),
            candidate_id: CandidateId::new(),
            candidate_revision: Revision::new(2)?,
            approval_id: ApprovalId::new(),
            approval_revision: Revision::new(3)?,
            evaluation_spec: EvaluationSpec::from_yaml(include_str!(
                "../../../crates/contracts/tests/fixtures/evaluation/linux/evaluation.yaml"
            ))?,
            execution_binding: evaluation_binding(project_id, course_id)?,
            runtime_identity: EvaluationRuntimeIdentity {
                provider_binding: "kubernetes/probe-runner".to_owned(),
                runner_image: format!(
                    "registry.example/labweaver/evaluation-worker@sha256:{}",
                    "a".repeat(64)
                ),
            },
            published_by: actor_id,
        })
    }

    fn evaluation_binding(
        project_id: contracts::ProjectId,
        course_id: CourseId,
    ) -> Result<EvaluationExecutionBinding, Box<dyn Error>> {
        let artifact_id = ArtifactId::new();
        Ok(EvaluationExecutionBinding {
            package: ProblemPackage {
                id: ProblemPackageId::new(),
                project_id,
                course_id: Some(course_id),
                revision: Revision::new(1)?,
                files: vec![PackageFile {
                    path: "linux-nginx-probe-v1/playbook.yml".to_owned(),
                    object: ArtifactRef {
                        artifact_id,
                        store_binding: "test-store".to_owned(),
                        object_version: "v1".to_owned(),
                        size_bytes: 1,
                        media_type: "text/plain".to_owned(),
                    },
                }],
                retention: RetentionSnapshot {
                    policy_id: PolicyId::new(),
                    policy_revision: Revision::new(1)?,
                    class: RetentionClass::CourseMaterial,
                    retain_until: "2027-01-01T00:00:00.000Z".parse()?,
                    disposition: RetentionDisposition::Delete,
                },
                completed_at: "2026-09-09T00:00:00.000Z".parse()?,
            },
            object_locators: BTreeMap::from([(artifact_id, "probe/playbook.yml".to_owned())]),
        })
    }

    fn admission(release: &EvaluationRelease) -> AuthoringPublicationAdmissionBinding {
        AuthoringPublicationAdmissionBinding {
            approval_id: release.approval_id,
            approval_revision: release.approval_revision,
            project_id: release.project_id,
            course_id: release.course_id,
            environment_release_id: ReleaseId::new(),
            environment_release_version: 1,
            evaluation_release_id: release.id,
            evaluation_release_revision: release.revision,
        }
    }
}
