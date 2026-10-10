//! Admitted Kubernetes sandbox execution for authoring attempts.
//!
//! One `ClaudeCodeProcess::execute` call for an authoring scope runs exactly one Resource-admitted
//! Kubernetes Job: the attempt first proves its Resource binding through the shared admission
//! witness, persists that binding before any cluster object exists, materializes the classified
//! egress envelope through short-lived object-store URLs, observes the Job and then persists the
//! terminal receipt before cleaning up and releasing the reservation.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use artifact_store::{ImmutableObjectStore, S3ImmutableObjectStore};
use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use contracts::execution::{
    ExecutionCleanupStatus, ExecutionObjectRef, ExecutionObservation, TaskExecutionBinding,
};
use contracts::http::{RecordResourceUsageRequest, TaskResourceStatus};
use contracts::resource::WorkloadResources;
use contracts::{ArtifactRef, TaskRunId, UtcTimestamp};
use persistence_sqlx::Sha256Digest;
use serde::{Deserialize, Serialize};
use task_execution::admission::{AdmittedExecution, cleanup_unknown};
use task_execution::kubernetes::{
    KubernetesApiClient, KubernetesApiConfiguration, KubernetesJobBundle, KubernetesJobIdentity,
    KubernetesJobObservation, KubernetesOwnership,
};
use task_execution::resource::{ResourceClient, TaskResourceError, TaskResourceLifecycle};
use task_execution::{ExecutionTiming, usage_deliveries};
use time::OffsetDateTime;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

#[path = "sandbox_cleanup.rs"]
mod sandbox_cleanup;

use crate::claude_code::{
    AuthoringAttemptScope, ClaudeCodeCommand, ClaudeCodeProcess, ClaudeCodeProcessError,
    ClaudeCodeProcessOutput, ExecutionScope, RunCancellation,
};
use crate::run_store::{
    AgentRunStoreError, PostgresAgentRunStore, SandboxAttemptCheckpoint, SandboxAttemptIntent,
};
use crate::sandbox::{
    SANDBOX_DEFAULT_DENY_POLICY, SANDBOX_EVENT_SCOPE, SANDBOX_MAIN_CONTAINER, SANDBOX_MANAGED_BY,
    SandboxAttemptSpec, SandboxBundleError, SandboxConfiguration, build_sandbox_bundle,
};

const FIELD_MANAGER: &str = "labweaver-authoring-executor";
const LOG_SCOPE: &str = "agent.authoring.sandbox";
const DIAGNOSTIC_PREFIX: &str = "LW_AGENT_";
const MATERIAL_MEDIA_TYPE: &str = "application/json";
const RESULT_MEDIA_TYPE: &str = "application/json";
const STDERR_MEDIA_TYPE: &str = "text/plain";
const EXPORT_MEDIA_TYPE: &str = contracts::http::PLATFORM_IMAGE_ARCHIVE_MEDIA_TYPE;
/// Bounded retries for one usage delivery after workload cleanup and Resource release.
const USAGE_DELIVERY_ATTEMPTS: u32 = 3;
/// Delay between two usage delivery attempts.
const USAGE_DELIVERY_RETRY: Duration = Duration::from_millis(500);
const OBSERVE_POLL: Duration = Duration::from_secs(2);
const USAGE_TIMING_UNAVAILABLE: &str = "LW_AGENT_SANDBOX_USAGE_TIMING_UNAVAILABLE";
const USAGE_DERIVATION_FAILED: &str = "LW_AGENT_SANDBOX_USAGE_DERIVATION_FAILED";

#[derive(Clone, Debug)]
pub(super) struct SandboxUsageCheckpoint {
    pub(super) deliveries: Vec<RecordResourceUsageRequest>,
    pub(super) delivered: bool,
}

/// Deployment-owned boundaries for admitted authoring sandbox executions.
#[derive(Clone, Debug)]
pub struct SandboxProcessConfiguration {
    pub sandbox: SandboxConfiguration,
    pub object_prefix: String,
    pub result_max_bytes: u64,
    pub stderr_max_bytes: u64,
    pub kubernetes_api_server: String,
    pub kubernetes_bearer_token_file: String,
    pub kubernetes_ca_file: String,
    pub request_timeout_milliseconds: u64,
    /// Reviewed object-store trust root the materializer and the attempt read signed URLs with.
    ///
    /// Absent means the object store is trusted by the image trust store. The file is read once at
    /// startup so an attempt never depends on a path inside its own pod.
    pub object_store_ca_file: Option<PathBuf>,
    /// Reviewed provider environment every attempt process runs with.
    ///
    /// The sandboxed Claude Code CLI is a separate process in a separate pod, so the provider
    /// endpoint, model and credential have to travel with the attempt instead of being inherited
    /// from the service environment. Entries are rendered into the per-attempt Secret, which the
    /// attempt owns and deletes with its bundle.
    pub worker_environment: BTreeMap<String, String>,
}

/// Admitted Kubernetes execution backend for authoring attempts.
#[derive(Clone)]
pub struct SandboxAuthoringProcess {
    api: KubernetesApiClient,
    resources: ResourceClient,
    store: PostgresAgentRunStore,
    objects: Arc<S3ImmutableObjectStore>,
    configuration: SandboxProcessConfiguration,
    object_store_ca_base64: Option<String>,
}

impl SandboxAuthoringProcess {
    /// Builds the backend from validated deployment configuration.
    ///
    /// # Errors
    ///
    /// Returns [`SandboxBundleError::Invalid`] for malformed configuration.
    pub fn new(
        configuration: SandboxProcessConfiguration,
        resources: ResourceClient,
        store: PostgresAgentRunStore,
        objects: Arc<S3ImmutableObjectStore>,
    ) -> Result<Self, SandboxBundleError> {
        configuration.sandbox.validate()?;
        if configuration.object_prefix.trim().is_empty()
            || !(1_024..=8 * 1024 * 1024).contains(&configuration.result_max_bytes)
            || configuration.stderr_max_bytes > 1024 * 1024
            || !configuration.kubernetes_api_server.starts_with("https://")
            || !(100..=60_000).contains(&configuration.request_timeout_milliseconds)
            || configuration
                .object_store_ca_file
                .as_ref()
                .is_some_and(|path| !path.is_absolute())
        {
            return Err(SandboxBundleError::Invalid);
        }
        let object_store_ca_base64 = match configuration.object_store_ca_file.as_deref() {
            Some(path) => Some(read_object_store_ca(path)?),
            None => None,
        };
        let api = KubernetesApiClient::new(
            KubernetesApiConfiguration {
                kubernetes_api_server: reqwest::Url::parse(&configuration.kubernetes_api_server)
                    .map_err(|_| SandboxBundleError::Invalid)?,
                kubernetes_bearer_token_file: configuration
                    .kubernetes_bearer_token_file
                    .clone()
                    .into(),
                kubernetes_ca_file: configuration.kubernetes_ca_file.clone().into(),
                runner_namespace: configuration.sandbox.namespace.clone(),
                request_timeout_milliseconds: configuration.request_timeout_milliseconds,
            },
            FIELD_MANAGER,
            LOG_SCOPE,
            DIAGNOSTIC_PREFIX,
            SANDBOX_MANAGED_BY,
            SANDBOX_EVENT_SCOPE,
        )
        .map_err(|_| SandboxBundleError::Invalid)?;
        Ok(Self {
            api,
            resources,
            store,
            objects,
            configuration,
            object_store_ca_base64,
        })
    }

    /// Starts the durable sandbox cleanup reconciler owned by the Agent process.
    ///
    /// The caller keeps the returned handle in the service's worker select so shutdown and
    /// startup failures remain visible to the process supervisor.
    #[must_use = "the cleanup worker handle must be retained by the service supervisor"]
    pub fn spawn_cleanup_worker(&self) -> Option<JoinHandle<()>> {
        sandbox_cleanup::spawn(
            self.api.clone(),
            self.resources.clone(),
            self.store.clone(),
            self.objects.clone(),
            self.configuration.clone(),
        )
    }

    async fn execute_authoring(
        &self,
        scope: &AuthoringAttemptScope,
        command: ClaudeCodeCommand,
        cancellation: RunCancellation,
    ) -> Result<ClaudeCodeProcessOutput, ClaudeCodeProcessError> {
        let existing = self
            .store
            .load_sandbox_attempt(
                scope.run_id,
                scope.track,
                scope.attempt,
                scope.execution_generation,
            )
            .await
            .map_err(|_| ClaudeCodeProcessError::Io)?;
        if let Some(checkpoint) = existing {
            if checkpoint.state != "creating" {
                return self
                    .finish_recovered_attempt(scope, &checkpoint, command.deadline(), cancellation)
                    .await;
            }
            return Box::pin(self.execute_new_attempt(
                scope,
                command,
                cancellation,
                Some(checkpoint),
            ))
            .await;
        }
        if cancellation.is_cancelled() {
            return Err(ClaudeCodeProcessError::Cancelled);
        }
        if tokio::time::Instant::now() >= command.deadline() {
            return Err(ClaudeCodeProcessError::TimedOut);
        }
        Box::pin(self.execute_new_attempt(scope, command, cancellation, None)).await
    }

    #[allow(clippy::too_many_lines)]
    async fn execute_new_attempt(
        &self,
        scope: &AuthoringAttemptScope,
        command: ClaudeCodeCommand,
        cancellation: RunCancellation,
        checkpoint: Option<SandboxAttemptCheckpoint>,
    ) -> Result<ClaudeCodeProcessOutput, ClaudeCodeProcessError> {
        let task_run_id = match &checkpoint {
            Some(saved) => TaskRunId::from_str(&saved.task_run_id.to_string())
                .map_err(|_| ClaudeCodeProcessError::Io)?,
            None => TaskRunId::new(),
        };
        let workload_name = workload_name(task_run_id.as_uuid());
        let request_key = request_key(scope, task_run_id);
        let requested = SandboxResourceRequest {
            project_id: scope.project_id,
            course_id: scope.course_id,
            actor_id: scope.actor_id,
            request_key,
            trace_id: scope.trace_id.clone(),
            resources: WorkloadResources {
                cpu_millicores: self.configuration.sandbox.cpu_millicores,
                memory_bytes: self.configuration.sandbox.memory_bytes,
                storage_bytes: self.configuration.sandbox.workspace_bytes,
                gpu: None,
            },
            duration_seconds: self.configuration.sandbox.wall_time_seconds,
        };
        let result_key = object_key(
            &self.configuration.object_prefix,
            task_run_id,
            "result.json",
        );
        let stderr_key = object_key(&self.configuration.object_prefix, task_run_id, "stderr.log");
        let export_key = object_key(&self.configuration.object_prefix, task_run_id, "export.tar");
        let saved = match checkpoint {
            Some(saved) => saved,
            None => self
                .store
                .reserve_sandbox_generation(
                    scope,
                    task_run_id.as_uuid(),
                    &self.configuration.sandbox.namespace,
                    &workload_name,
                    [&result_key, &stderr_key, &export_key],
                    &serde_json::to_value(&requested).map_err(|_| ClaudeCodeProcessError::Io)?,
                )
                .await
                .map_err(|_| ClaudeCodeProcessError::Io)?,
        };
        if saved.task_run_id != task_run_id.as_uuid()
            || saved.namespace != self.configuration.sandbox.namespace
            || saved.workload_name != workload_name
            || saved.execution_generation != scope.execution_generation
            || saved.result_object_key.as_deref() != Some(result_key.as_str())
            || saved.stderr_object_key.as_deref() != Some(stderr_key.as_str())
            || saved.export_object_key.as_deref() != Some(export_key.as_str())
        {
            return Err(ClaudeCodeProcessError::Io);
        }
        let request: SandboxResourceRequest =
            serde_json::from_value(saved.request_payload.ok_or(ClaudeCodeProcessError::Io)?)
                .map_err(|_| ClaudeCodeProcessError::Io)?;
        if request.project_id != scope.project_id
            || request.course_id != scope.course_id
            || request.actor_id != scope.actor_id
            || request.request_key != requested.request_key
        {
            return Err(ClaudeCodeProcessError::Io);
        }
        let lifecycle = request.lifecycle(&self.resources, task_run_id)?;
        self.store
            .fence_sandbox_generation(scope)
            .await
            .map_err(|_| ClaudeCodeProcessError::Io)?;
        let deadline = command.deadline();
        let (cancel_token, _bridge) = bridge_cancellation(&cancellation);
        // The reservation has to exist before it can be claimed: the claim path reads the
        // authoritative request projection and never invents one, so an attempt that skipped this
        // step would fail before any cluster object existed. The POST is awaited before
        // cancellation is observed so an in-flight request cannot be dropped and then commit after
        // the attempt already stopped.
        lifecycle.create().await.map_err(|error| {
            tracing::error!(
                event = "agent.authoring.sandbox.stage_failed",
                failure_stage = "sandbox.resource.create",
                error_kind = ?error,
                task_run_id = %task_run_id.as_uuid(),
                "authoring attempt could not create its Resource request",
            );
            map_task_resource(&error)
        })?;
        if cancellation.is_cancelled() || tokio::time::Instant::now() >= deadline {
            lifecycle
                .cancel("authoring attempt cancelled before resource claim")
                .await
                .map_err(|error| map_task_resource(&error))?;
            return Err(if cancellation.is_cancelled() {
                ClaudeCodeProcessError::Cancelled
            } else {
                ClaudeCodeProcessError::TimedOut
            });
        }
        let approval_timeout = deadline.saturating_duration_since(tokio::time::Instant::now());
        let approval = lifecycle
            .claim_after_approval(OBSERVE_POLL, approval_timeout, &cancel_token)
            .await
            .map_err(|error| map_task_resource(&error))?;
        let status = lifecycle
            .acknowledge(&approval, &self.configuration.sandbox.namespace)
            .await
            .map_err(|error| map_task_resource(&error))?;
        let admitted = AdmittedExecution::admit(
            &status,
            scope.execution_generation,
            workload_name.clone(),
            scope.trace_id.clone(),
        )
        .map_err(|_| ClaudeCodeProcessError::Io)?;
        let binding = admitted.binding().clone();
        let ownership = attempt_ownership(scope, task_run_id);
        let intent = SandboxAttemptIntent {
            run_id: scope.run_id,
            track: scope.track,
            attempt: scope.attempt,
            task_run_id: task_run_id.as_uuid(),
            execution_generation: scope.execution_generation,
            namespace: self.configuration.sandbox.namespace.clone(),
            workload_name: workload_name.clone(),
            binding: serde_json::to_value(&binding).map_err(|_| ClaudeCodeProcessError::Io)?,
        };
        self.store
            .begin_sandbox_attempt(&intent)
            .await
            .map_err(|_| ClaudeCodeProcessError::Io)?;
        self.store
            .fence_sandbox_generation(scope)
            .await
            .map_err(|_| ClaudeCodeProcessError::Io)?;
        if cancellation.is_cancelled() || tokio::time::Instant::now() >= deadline {
            lifecycle
                .cancel("authoring deadline or cancellation before submission")
                .await
                .map_err(|e| map_task_resource(&e))?;
            return Err(if cancellation.is_cancelled() {
                ClaudeCodeProcessError::Cancelled
            } else {
                ClaudeCodeProcessError::TimedOut
            });
        }

        let now = authority_now()?;
        let material_key = object_key(
            &self.configuration.object_prefix,
            task_run_id,
            "material.json",
        );
        let material = self
            .objects
            .put_versioned_immutable(material_key.as_str(), command.stdin(), MATERIAL_MEDIA_TYPE)
            .await
            .map_err(|error| stage_failure("sandbox.material", &error))?;
        let material_download = self
            .objects
            .presign_download(
                material_key.as_str(),
                material.reference.object_version.as_str(),
                material.reference.size_bytes,
                MATERIAL_MEDIA_TYPE,
                now,
            )
            .await
            .map_err(|_| ClaudeCodeProcessError::Io)?;

        let result_upload = self
            .objects
            .presign_upload(
                result_key.as_str(),
                self.configuration.result_max_bytes,
                RESULT_MEDIA_TYPE,
                now,
            )
            .await
            .map_err(|_| ClaudeCodeProcessError::Io)?;

        let export_upload = if self.configuration.sandbox.buildkit_image.is_some() {
            Some(
                self.objects
                    .presign_upload(
                        export_key.as_str(),
                        self.configuration.sandbox.workspace_bytes.max(1),
                        EXPORT_MEDIA_TYPE,
                        now,
                    )
                    .await
                    .map_err(|_| ClaudeCodeProcessError::Io)?,
            )
        } else {
            None
        };

        let stderr_upload = self
            .objects
            .presign_upload(
                stderr_key.as_str(),
                self.configuration.stderr_max_bytes.max(1),
                STDERR_MEDIA_TYPE,
                now,
            )
            .await
            .map_err(|_| ClaudeCodeProcessError::Io)?;

        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() || cancellation.is_cancelled() {
            lifecycle
                .cancel("authoring deadline before Job start")
                .await
                .map_err(|e| map_task_resource(&e))?;
            return Err(if cancellation.is_cancelled() {
                ClaudeCodeProcessError::Cancelled
            } else {
                ClaudeCodeProcessError::TimedOut
            });
        }
        let spec = SandboxAttemptSpec {
            timeout_seconds: remaining
                .as_secs()
                .saturating_add(u64::from(remaining.subsec_nanos() > 0))
                .min(self.configuration.sandbox.wall_time_seconds),
            task_run_id: task_run_id.as_uuid(),
            ownership,
            trace_id: scope.trace_id.clone(),
            command: command_argv(&command),
            command_environment: attempt_environment(
                &self.configuration.worker_environment,
                command.env(),
            ),
            expected_claude_version: scope.claude_code_version.clone(),
            material_download_url: material_download.url,
            material_sha256: command.stdin_sha256().to_string(),
            material_size_bytes: material.reference.size_bytes,
            result_upload_url: result_upload.url,
            result_upload_headers: result_upload.required_headers,
            stderr_upload_url: stderr_upload.url,
            stderr_upload_headers: stderr_upload.required_headers,
            result_max_bytes: self.configuration.result_max_bytes,
            stderr_max_bytes: self.configuration.stderr_max_bytes,
            export_upload_url: export_upload.as_ref().map(|upload| upload.url.clone()),
            export_upload_headers: export_upload
                .map(|upload| upload.required_headers)
                .unwrap_or_default(),
            object_store_ca_base64: self.object_store_ca_base64.clone(),
        };
        let sandbox_bundle = build_sandbox_bundle(&self.configuration.sandbox, &spec)
            .map_err(|error| stage_failure("sandbox.bundle", &error))?;
        let bundle = sandbox_bundle.bundle;
        self.store
            .fence_sandbox_generation(scope)
            .await
            .map_err(|_| ClaudeCodeProcessError::Io)?;
        if cancellation.is_cancelled() || tokio::time::Instant::now() >= deadline {
            lifecycle
                .cancel("authoring deadline before Job apply")
                .await
                .map_err(|e| map_task_resource(&e))?;
            return Err(if cancellation.is_cancelled() {
                ClaudeCodeProcessError::Cancelled
            } else {
                ClaudeCodeProcessError::TimedOut
            });
        }
        self.api
            .start(&bundle)
            .await
            .map_err(|error| stage_failure("sandbox.apply", &error))?;
        let refs = self
            .api
            .capture_object_refs(&bundle.identity, &bundle.cleanup_plan)
            .await
            .map_err(|error| stage_failure("sandbox.capture", &error))?;
        self.store
            .record_sandbox_objects(
                &intent,
                &serde_json::to_value(&refs)
                    .map_err(|error| stage_failure("sandbox.serialize", &error))?,
            )
            .await
            .map_err(|error| stage_failure("sandbox.record", &error))?;

        let expected_uid = job_uid(&refs);
        loop {
            if cancellation.is_cancelled() {
                let _ = self
                    .store
                    .complete_sandbox_attempt(&intent, None, 1, Some("LW_AGENT_SANDBOX_CANCELLED"))
                    .await;
                self.cleanup_and_release(&intent, &bundle, &status, ExecutionTiming::unknown())
                    .await;
                return Err(ClaudeCodeProcessError::Cancelled);
            }
            let observation = self
                .api
                .observe(&bundle.identity, expected_uid.as_deref())
                .await
                .map_err(|_| ClaudeCodeProcessError::Io)?;
            match observation {
                KubernetesJobObservation::Completed {
                    message,
                    observation,
                } => {
                    let receipt =
                        parse_receipt(&message).map_err(|_| ClaudeCodeProcessError::Io)?;
                    receipt
                        .validate(
                            scope,
                            self.configuration.result_max_bytes,
                            self.configuration.stderr_max_bytes,
                            self.configuration.sandbox.workspace_bytes,
                        )
                        .map_err(|_| ClaudeCodeProcessError::Io)?;
                    let frozen = freeze_terminal_receipt(
                        self.objects.as_ref(),
                        task_run_id.as_uuid(),
                        &receipt,
                        [&result_key, &stderr_key, &export_key],
                    )
                    .await?;
                    let canonical = self
                        .store
                        .checkpoint_sandbox_receipt(
                            &intent,
                            self.objects.binding(),
                            &serde_json::to_value(&frozen)
                                .map_err(|_| ClaudeCodeProcessError::Io)?,
                        )
                        .await
                        .map_err(|_| ClaudeCodeProcessError::Io)?;
                    let frozen: FrozenSandboxReceipt = serde_json::from_value(canonical)
                        .map_err(|_| ClaudeCodeProcessError::Io)?;
                    let output = frozen.assemble(self.objects.as_ref(), false).await?;
                    self.store
                        .complete_sandbox_attempt(
                            &intent,
                            Some((
                                &result_key,
                                &receipt.result_sha256,
                                receipt.result_size_bytes,
                            )),
                            receipt.exit_code,
                            None,
                        )
                        .await
                        .map_err(|_| ClaudeCodeProcessError::Io)?;
                    self.cleanup_and_release(
                        &intent,
                        &bundle,
                        &status,
                        attempt_timing(&observation),
                    )
                    .await;
                    return Ok(output);
                }
                KubernetesJobObservation::Failed {
                    diagnostic_code,
                    observation,
                } => {
                    self.store
                        .complete_sandbox_attempt(&intent, None, 1, Some(diagnostic_code.as_str()))
                        .await
                        .map_err(|_| ClaudeCodeProcessError::Io)?;
                    self.cleanup_and_release(
                        &intent,
                        &bundle,
                        &status,
                        attempt_timing(&observation),
                    )
                    .await;
                    return Err(sandbox_failure(&diagnostic_code));
                }
                KubernetesJobObservation::Missing | KubernetesJobObservation::Running => {}
            }
            if tokio::time::Instant::now() >= deadline {
                self.store
                    .complete_sandbox_attempt(
                        &intent,
                        None,
                        1,
                        Some("LW_AGENT_SANDBOX_DEADLINE_EXCEEDED"),
                    )
                    .await
                    .map_err(|_| ClaudeCodeProcessError::Io)?;
                self.cleanup_and_release(&intent, &bundle, &status, ExecutionTiming::unknown())
                    .await;
                return Err(ClaudeCodeProcessError::TimedOut);
            }
            tokio::time::sleep(OBSERVE_POLL).await;
        }
    }

    // This ordered recovery path must checkpoint usage before deletion and release independently
    // from delivery, so keep its state transitions together for review.
    #[allow(clippy::too_many_lines)]
    async fn finish_recovered_attempt(
        &self,
        scope: &AuthoringAttemptScope,
        checkpoint: &SandboxAttemptCheckpoint,
        deadline: tokio::time::Instant,
        cancellation: RunCancellation,
    ) -> Result<ClaudeCodeProcessOutput, ClaudeCodeProcessError> {
        let task_run_id = TaskRunId::from_str(&checkpoint.task_run_id.to_string())
            .map_err(|_| ClaudeCodeProcessError::Io)?;
        if let Some(diagnostic) = &checkpoint.diagnostic_code {
            return Err(sandbox_failure(diagnostic));
        }
        let binding: TaskExecutionBinding = serde_json::from_value(
            checkpoint
                .binding
                .clone()
                .ok_or(ClaudeCodeProcessError::Io)?,
        )
        .map_err(|_| ClaudeCodeProcessError::Io)?;
        if binding.validate().is_err()
            || binding.task_run_id != task_run_id
            || binding.execution_generation != checkpoint.execution_generation
            || checkpoint.execution_generation != scope.execution_generation
            || binding.namespace != checkpoint.namespace
            || binding.workload_name != checkpoint.workload_name
        {
            return Err(ClaudeCodeProcessError::Io);
        }
        let objects: Vec<ExecutionObjectRef> = serde_json::from_value(checkpoint.objects.clone())
            .map_err(|_| ClaudeCodeProcessError::Io)?;
        let identity = Self::attempt_identity(
            &checkpoint.namespace,
            &checkpoint.workload_name,
            attempt_ownership_from_parts(scope.run_id.as_uuid(), task_run_id, &binding.trace_id),
            &binding.trace_id,
        );
        let intent = SandboxAttemptIntent {
            run_id: scope.run_id,
            track: scope.track,
            attempt: scope.attempt,
            task_run_id: checkpoint.task_run_id,
            execution_generation: checkpoint.execution_generation,
            namespace: checkpoint.namespace.clone(),
            workload_name: checkpoint.workload_name.clone(),
            binding: checkpoint
                .binding
                .clone()
                .ok_or(ClaudeCodeProcessError::Io)?,
        };
        let output = self
            .recover_output(
                scope,
                checkpoint,
                &intent,
                &identity,
                &objects,
                deadline,
                cancellation.clone(),
            )
            .await?;
        if self
            .store
            .fence_sandbox_read(scope)
            .await
            .map_err(|_| ClaudeCodeProcessError::Io)?
        {
            cancellation.cancel();
        }
        let cleanup = async {
        let persisted_usage = load_usage_checkpoint(&self.store, &intent)
            .await
            .map_err(|_| ClaudeCodeProcessError::Io)?;
        let observed = match self
            .api
            .observe(&identity, job_uid(&objects).as_deref())
            .await
        {
            Ok(
                KubernetesJobObservation::Completed { observation, .. }
                | KubernetesJobObservation::Failed { observation, .. },
            ) => Some(observation),
            Ok(KubernetesJobObservation::Missing) => None,
            Ok(KubernetesJobObservation::Running) => {
                tracing::warn!(
                    event = "agent.authoring.sandbox.recovery_deferred",
                    failure_stage = "sandbox.observation",
                    task_run_id = %task_run_id.as_uuid(),
                    "recovered authoring attempt is still running; cleanup will wait for terminal state",
                );
                return Ok(());
            }
            Err(error) => {
                tracing::warn!(
                    event = "agent.authoring.sandbox.recovery_deferred",
                    failure_stage = "sandbox.observation",
                    task_run_id = %task_run_id.as_uuid(),
                    error_kind = error.error_kind(),
                    "Kubernetes could not confirm an ended recovered attempt",
                );
                return Ok(());
            }
        };
        let usage_observation = match self
            .store
            .load_sandbox_usage_observation(&intent)
            .await
            .map_err(|_| ClaudeCodeProcessError::Io)?
        {
            Some(observation) => Some(observation),
            None => match observed.as_ref() {
                Some(observation) if reproducible_timing(observation).is_some() => {
                    checkpoint_sandbox_usage_observation(&self.store, &intent, observation)
                        .await
                        .map_err(|_| ClaudeCodeProcessError::Io)?
                }
                _ => None,
            },
        };
        let usage = match persisted_usage {
            Some(usage) => Some(usage),
            None if usage_observation.is_some() => checkpoint_usage_from_observation(
                &self.store,
                &self.resources,
                &intent,
                &binding,
                usage_observation.as_ref(),
            )
            .await
            .map_err(|_| ClaudeCodeProcessError::Io)?,
            None => mark_usage_unavailable(&self.store, &intent, USAGE_TIMING_UNAVAILABLE)
                .await
                .map_err(|_| ClaudeCodeProcessError::Io)?,
        };
        let cleanup = self.cleanup_recovery_with_poll(&identity, &objects).await;
        if cleanup == ExecutionCleanupStatus::Confirmed {
            if let Err(error) =
                release_after_confirmed_cleanup(&self.store, &intent, &self.resources).await
            {
                let (failure_stage, error_kind) = error.log_fields();
                tracing::warn!(
                    event = "agent.authoring.sandbox.release_deferred",
                    failure_stage,
                    task_run_id = %intent.task_run_id,
                    error_kind,
                    "recovered sandbox cleanup is confirmed but Resource release remains pending",
                );
            }
            if usage.is_none() && usage_observation.is_some() {
                tracing::warn!(
                    event = "agent.authoring.sandbox.usage_checkpoint_deferred",
                    failure_stage = "sandbox.usage_payload",
                    task_run_id = %intent.task_run_id,
                    "terminal timing is durable and usage construction will retry after cleanup",
                );
            } else {
                self.deliver_checkpoint_usage(&intent, usage).await;
            }
        }
        Ok::<(),ClaudeCodeProcessError>(())
        }.await;
        if let Err(error) = cleanup {
            tracing::warn!(event="agent.authoring.sandbox.recovery_deferred",failure_stage="sandbox.cleanup",task_run_id=%task_run_id.as_uuid(),error_kind=?error,"validated terminal output retained while cleanup or metering persistence retries");
        }
        if self
            .store
            .fence_sandbox_read(scope)
            .await
            .map_err(|_| ClaudeCodeProcessError::Io)?
        {
            cancellation.cancel();
        }
        // A cancelled terminal envelope is retained for the runtime's truthful usage audit;
        // the runtime and locked completion fence refuse its candidate.
        Ok(output)
    }

    // Keep exact generation reads, immutable receipt verification and terminal observation together.
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    async fn recover_output(
        &self,
        scope: &AuthoringAttemptScope,
        checkpoint: &SandboxAttemptCheckpoint,
        intent: &SandboxAttemptIntent,
        identity: &KubernetesJobIdentity,
        objects: &[ExecutionObjectRef],
        deadline: tokio::time::Instant,
        cancellation: RunCancellation,
    ) -> Result<ClaudeCodeProcessOutput, ClaudeCodeProcessError> {
        if checkpoint.execution_generation != scope.execution_generation {
            return Err(ClaudeCodeProcessError::Io);
        }
        loop {
            let checkpoint = self
                .store
                .load_sandbox_attempt(
                    scope.run_id,
                    scope.track,
                    scope.attempt,
                    scope.execution_generation,
                )
                .await
                .map_err(|_| ClaudeCodeProcessError::Io)?
                .ok_or(ClaudeCodeProcessError::Io)?;
            if let Some(diagnostic) = &checkpoint.diagnostic_code {
                return Err(sandbox_failure(diagnostic));
            }
            if let Some(value) = &checkpoint.terminal_receipt {
                let frozen: FrozenSandboxReceipt = serde_json::from_value(value.clone())
                    .map_err(|_| ClaudeCodeProcessError::Io)?;
                frozen
                    .receipt
                    .validate(
                        scope,
                        self.configuration.result_max_bytes,
                        self.configuration.stderr_max_bytes,
                        self.configuration.sandbox.workspace_bytes,
                    )
                    .map_err(|_| ClaudeCodeProcessError::Io)?;
                let keys = [
                    checkpoint
                        .result_object_key
                        .as_deref()
                        .ok_or(ClaudeCodeProcessError::Io)?,
                    checkpoint
                        .stderr_object_key
                        .as_deref()
                        .ok_or(ClaudeCodeProcessError::Io)?,
                    checkpoint
                        .export_object_key
                        .as_deref()
                        .ok_or(ClaudeCodeProcessError::Io)?,
                ];
                if !frozen.matches_identity(checkpoint.task_run_id, keys) {
                    return Err(ClaudeCodeProcessError::Io);
                }
                for (key, name) in keys.iter().zip(["result.json", "stderr.log", "export.tar"]) {
                    if *key
                        != object_key(
                            &self.configuration.object_prefix,
                            TaskRunId::from_str(&checkpoint.task_run_id.to_string())
                                .map_err(|_| ClaudeCodeProcessError::Io)?,
                            name,
                        )
                    {
                        return Err(ClaudeCodeProcessError::Io);
                    }
                }
                return frozen
                    .assemble(self.objects.as_ref(), true)
                    .await
                    .map_err(Into::into);
            }
            match self
                .api
                .observe(identity, job_uid(objects).as_deref())
                .await
                .map_err(|_| ClaudeCodeProcessError::Io)?
            {
                KubernetesJobObservation::Completed { message, .. } => {
                    let receipt =
                        parse_receipt(&message).map_err(|_| ClaudeCodeProcessError::Io)?;
                    receipt
                        .validate(
                            scope,
                            self.configuration.result_max_bytes,
                            self.configuration.stderr_max_bytes,
                            self.configuration.sandbox.workspace_bytes,
                        )
                        .map_err(|_| ClaudeCodeProcessError::Io)?;
                    let keys = [
                        checkpoint
                            .result_object_key
                            .as_deref()
                            .ok_or(ClaudeCodeProcessError::Io)?,
                        checkpoint
                            .stderr_object_key
                            .as_deref()
                            .ok_or(ClaudeCodeProcessError::Io)?,
                        checkpoint
                            .export_object_key
                            .as_deref()
                            .ok_or(ClaudeCodeProcessError::Io)?,
                    ];
                    let frozen = freeze_terminal_receipt(
                        self.objects.as_ref(),
                        checkpoint.task_run_id,
                        &receipt,
                        keys,
                    )
                    .await?;
                    let value = self
                        .store
                        .checkpoint_sandbox_receipt(
                            intent,
                            self.objects.binding(),
                            &serde_json::to_value(frozen)
                                .map_err(|_| ClaudeCodeProcessError::Io)?,
                        )
                        .await
                        .map_err(|_| ClaudeCodeProcessError::Io)?;
                    let canonical: FrozenSandboxReceipt =
                        serde_json::from_value(value).map_err(|_| ClaudeCodeProcessError::Io)?;
                    return canonical
                        .assemble(self.objects.as_ref(), false)
                        .await
                        .map_err(Into::into);
                }
                KubernetesJobObservation::Running
                    if !cancellation.is_cancelled() && tokio::time::Instant::now() < deadline =>
                {
                    tokio::time::sleep(
                        OBSERVE_POLL
                            .min(deadline.saturating_duration_since(tokio::time::Instant::now())),
                    )
                    .await;
                }
                KubernetesJobObservation::Failed {
                    diagnostic_code, ..
                } => {
                    return Err(sandbox_failure(&diagnostic_code));
                }
                KubernetesJobObservation::Running if cancellation.is_cancelled() => {
                    return Err(ClaudeCodeProcessError::Cancelled);
                }
                KubernetesJobObservation::Running if tokio::time::Instant::now() >= deadline => {
                    return Err(ClaudeCodeProcessError::TimedOut);
                }
                KubernetesJobObservation::Missing => {
                    let latest = self
                        .store
                        .load_sandbox_attempt(
                            scope.run_id,
                            scope.track,
                            scope.attempt,
                            scope.execution_generation,
                        )
                        .await
                        .map_err(|_| ClaudeCodeProcessError::Io)?
                        .ok_or(ClaudeCodeProcessError::Io)?;
                    if let Some(diagnostic) = &latest.diagnostic_code {
                        return Err(sandbox_failure(diagnostic));
                    }
                    if latest.terminal_receipt.is_some() {
                        continue;
                    }
                    return Err(ClaudeCodeProcessError::Io);
                }
                KubernetesJobObservation::Running => return Err(ClaudeCodeProcessError::Io),
            }
        }
    }

    fn attempt_identity(
        namespace: &str,
        workload_name: &str,
        ownership: KubernetesOwnership,
        trace_id: &str,
    ) -> KubernetesJobIdentity {
        KubernetesJobIdentity {
            namespace: namespace.to_owned(),
            job_name: workload_name.to_owned(),
            main_container: SANDBOX_MAIN_CONTAINER,
            default_deny_policy: SANDBOX_DEFAULT_DENY_POLICY,
            deadline_diagnostic_code: "LW_AGENT_SANDBOX_DEADLINE_EXCEEDED",
            failed_diagnostic_code: "LW_AGENT_SANDBOX_FAILED",
            oom_diagnostic_code: "LW_AGENT_SANDBOX_MEMORY_LIMIT",
            stable_diagnostic_prefix: DIAGNOSTIC_PREFIX,
            ownership,
            trace_id: trace_id.to_owned(),
        }
    }

    async fn deliver_checkpoint_usage(
        &self,
        intent: &SandboxAttemptIntent,
        usage: Option<SandboxUsageCheckpoint>,
    ) {
        let Some(usage) = usage else {
            return;
        };
        if usage.delivered {
            return;
        }
        let Ok(task_run_id) = TaskRunId::from_str(&intent.task_run_id.to_string()) else {
            return;
        };
        if deliver_usage_payload(&self.resources, &task_run_id, &usage.deliveries).await
            && let Err(error) = self.store.mark_sandbox_usage_delivered(intent).await
        {
            tracing::error!(
                event = "agent.authoring.sandbox.usage_checkpoint_failed",
                failure_stage = "sandbox.usage_delivered",
                task_run_id = %intent.task_run_id,
                error_kind = ?error,
                "Resource accepted usage but Agent could not persist the delivery checkpoint",
            );
        }
    }

    async fn cleanup_and_release(
        &self,
        intent: &SandboxAttemptIntent,
        bundle: &KubernetesJobBundle,
        status: &contracts::http::TaskResourceStatus,
        timing: ExecutionTiming,
    ) {
        // The exact Resource request is checkpointed before any owned Pod is deleted. Delivery
        // runs after cleanup and release, so a Resource outage cannot strand the reservation or
        // keep the deleted workload alive; the worker retries the same payload later.
        let usage = match ensure_usage_checkpoint(&self.store, intent, status, timing).await {
            Ok(usage) => usage,
            Err(error) => {
                tracing::error!(
                    event = "agent.authoring.sandbox.usage_checkpoint_failed",
                    failure_stage = "sandbox.usage_checkpoint",
                    task_run_id = %intent.task_run_id,
                    error_kind = ?error,
                    "authoring attempt could not persist its usage payload before cleanup",
                );
                return;
            }
        };
        let usage = if usage.is_none() {
            match mark_usage_unavailable(&self.store, intent, USAGE_DERIVATION_FAILED).await {
                Ok(usage) => usage,
                Err(error) => {
                    tracing::error!(
                        event = "agent.authoring.sandbox.usage_checkpoint_failed",
                        failure_stage = "sandbox.usage_diagnostic",
                        task_run_id = %intent.task_run_id,
                        error_kind = ?error,
                        "authoring attempt could not persist why its usage payload is unavailable",
                    );
                    return;
                }
            }
        } else {
            usage
        };
        let cleanup = self
            .cleanup_bundle_with_poll(
                &intent.namespace,
                &intent.workload_name,
                &bundle.objects,
                &bundle.cleanup_plan,
            )
            .await;
        if cleanup == ExecutionCleanupStatus::Confirmed {
            if let Err(error) =
                release_after_confirmed_cleanup(&self.store, intent, &self.resources).await
            {
                let (failure_stage, error_kind) = error.log_fields();
                tracing::warn!(
                    event = "agent.authoring.sandbox.release_deferred",
                    failure_stage,
                    task_run_id = %intent.task_run_id,
                    error_kind,
                    "owned sandbox cleanup is confirmed but Resource release remains pending",
                );
            }
            self.deliver_checkpoint_usage(intent, usage).await;
        }
    }

    async fn cleanup_bundle_with_poll(
        &self,
        namespace: &str,
        workload_name: &str,
        objects: &[task_execution::kubernetes::KubernetesObject],
        cleanup_plan: &[task_execution::kubernetes::KubernetesCleanupTarget],
    ) -> ExecutionCleanupStatus {
        const ATTEMPTS: usize = 4;
        const RETRY: Duration = Duration::from_millis(500);
        for attempt in 0..ATTEMPTS {
            let cleanup = match self
                .api
                .cleanup(namespace, workload_name, objects, cleanup_plan)
                .await
            {
                Ok(cleanup) => cleanup,
                Err(error) => {
                    tracing::warn!(
                        event = "agent.authoring.sandbox.cleanup_retryable",
                        failure_stage = "sandbox.cleanup",
                        error_kind = error.error_kind(),
                        attempt = attempt + 1,
                        "Kubernetes cleanup call failed; retrying the exact owned object set",
                    );
                    cleanup_unknown("LW_AGENT_SANDBOX_CLEANUP_UNKNOWN")
                }
            };
            if cleanup == ExecutionCleanupStatus::Confirmed || attempt + 1 == ATTEMPTS {
                return cleanup;
            }
            tokio::time::sleep(RETRY).await;
        }
        unreachable!("bounded cleanup loop always returns")
    }

    async fn cleanup_recovery_with_poll(
        &self,
        identity: &KubernetesJobIdentity,
        objects: &[ExecutionObjectRef],
    ) -> ExecutionCleanupStatus {
        cleanup_recovery_with_poll(&self.api, identity, objects).await
    }
}

async fn load_usage_checkpoint(
    store: &PostgresAgentRunStore,
    intent: &SandboxAttemptIntent,
) -> Result<Option<SandboxUsageCheckpoint>, AgentRunStoreError> {
    let Some((payload, delivered)) = store.load_sandbox_usage(intent).await? else {
        return Ok(None);
    };
    let deliveries: Vec<RecordResourceUsageRequest> =
        serde_json::from_value(payload).map_err(|_| AgentRunStoreError::InvalidContract)?;
    if deliveries.is_empty() {
        return Err(AgentRunStoreError::InvalidContract);
    }
    Ok(Some(SandboxUsageCheckpoint {
        deliveries,
        delivered,
    }))
}

async fn ensure_usage_checkpoint(
    store: &PostgresAgentRunStore,
    intent: &SandboxAttemptIntent,
    status: &TaskResourceStatus,
    timing: ExecutionTiming,
) -> Result<Option<SandboxUsageCheckpoint>, AgentRunStoreError> {
    if let Some(existing) = load_usage_checkpoint(store, intent).await? {
        return Ok(Some(existing));
    }
    let Some(deliveries) = derive_usage_deliveries(status, timing) else {
        tracing::warn!(
            event = "agent.authoring.sandbox.usage_checkpoint_unavailable",
            failure_stage = "sandbox.usage_derivation",
            task_run_id = %status.task_run_id.as_uuid(),
            "authoring attempt could not derive its Resource usage payload",
        );
        return Ok(None);
    };
    let payload =
        serde_json::to_value(&deliveries).map_err(|_| AgentRunStoreError::InvalidContract)?;
    match store.checkpoint_sandbox_usage(intent, &payload).await {
        Ok(()) | Err(AgentRunStoreError::StateConflict) => {}
        Err(error) => return Err(error),
    }
    // A competing cleanup worker may have won the checkpoint with a different fallback boundary.
    // Resource must always receive the exact payload persisted by the winner.
    load_usage_checkpoint(store, intent)
        .await?
        .ok_or(AgentRunStoreError::StateConflict)
        .map(Some)
}

async fn mark_usage_unavailable(
    store: &PostgresAgentRunStore,
    intent: &SandboxAttemptIntent,
    diagnostic: &str,
) -> Result<Option<SandboxUsageCheckpoint>, AgentRunStoreError> {
    match store
        .mark_sandbox_usage_unavailable(intent, diagnostic)
        .await
    {
        Ok(()) => load_usage_checkpoint(store, intent).await,
        Err(AgentRunStoreError::StateConflict) => {
            match load_usage_checkpoint(store, intent).await? {
                Some(usage) => Ok(Some(usage)),
                None => Err(AgentRunStoreError::StateConflict),
            }
        }
        Err(error) => Err(error),
    }
}

pub(super) async fn checkpoint_sandbox_usage_observation(
    store: &PostgresAgentRunStore,
    intent: &SandboxAttemptIntent,
    observation: &ExecutionObservation,
) -> Result<Option<(ExecutionObservation, UtcTimestamp)>, AgentRunStoreError> {
    if reproducible_timing(observation).is_none() {
        return Ok(None);
    }
    let measured_until = authority_now().map_err(|_| AgentRunStoreError::InvalidContract)?;
    match store
        .checkpoint_sandbox_usage_observation(intent, observation, measured_until)
        .await
    {
        Ok(observation) => Ok(Some(observation)),
        Err(AgentRunStoreError::StateConflict) => {
            store.load_sandbox_usage_observation(intent).await
        }
        Err(error) => Err(error),
    }
}

pub(super) async fn checkpoint_usage_from_observation(
    store: &PostgresAgentRunStore,
    resources: &ResourceClient,
    intent: &SandboxAttemptIntent,
    binding: &TaskExecutionBinding,
    observation: Option<&(ExecutionObservation, UtcTimestamp)>,
) -> Result<Option<SandboxUsageCheckpoint>, AgentRunStoreError> {
    let Some((observation, fixed_until)) = observation else {
        return Ok(None);
    };
    let task_run_id = TaskRunId::from_str(&intent.task_run_id.to_string())
        .map_err(|_| AgentRunStoreError::InvalidContract)?;
    let status = match resources.get_task_resource(task_run_id).await {
        Ok(status) => status,
        Err(error) => {
            tracing::warn!(
                event = "agent.authoring.sandbox.usage_checkpoint_deferred",
                failure_stage = "resource.read",
                task_run_id = %intent.task_run_id,
                error_kind = ?error,
                "the saved terminal timing is retained while Resource is unavailable",
            );
            return Ok(None);
        }
    };
    if !binding.same_reservation(&status) {
        tracing::error!(
            event = "agent.authoring.sandbox.usage_checkpoint_rejected",
            failure_stage = "resource.identity",
            task_run_id = %intent.task_run_id,
            "Resource reservation does not match the saved execution binding",
        );
        return Ok(None);
    }
    let deliveries = usage_deliveries(&status, attempt_timing(observation), *fixed_until)
        .map_err(|_| AgentRunStoreError::InvalidContract)?;
    let payload =
        serde_json::to_value(deliveries).map_err(|_| AgentRunStoreError::InvalidContract)?;
    match store.checkpoint_sandbox_usage(intent, &payload).await {
        Ok(()) | Err(AgentRunStoreError::StateConflict) => {}
        Err(error) => return Err(error),
    }
    load_usage_checkpoint(store, intent).await
}

#[derive(Debug)]
pub(super) enum CleanupReleaseError {
    Persistence(AgentRunStoreError),
    Resource(TaskResourceError),
}

impl CleanupReleaseError {
    fn log_fields(&self) -> (&'static str, String) {
        match self {
            Self::Persistence(error) => ("agent.persistence", format!("{error:?}")),
            Self::Resource(error) => ("resource", format!("{error:?}")),
        }
    }
}

/// Confirms one exact owned cleanup and releases its Resource reservation independently of usage
/// delivery. A persistence failure is returned after attempting the Resource release, so the
/// recovery worker can replay the idempotent cleanup and finish the durable state transition.
pub(super) async fn release_after_confirmed_cleanup(
    store: &PostgresAgentRunStore,
    intent: &SandboxAttemptIntent,
    resources: &ResourceClient,
) -> Result<(), CleanupReleaseError> {
    let confirmation_failed = match store.confirm_sandbox_cleanup(intent).await {
        Ok(()) => None,
        Err(error) => {
            tracing::error!(
                event = "agent.authoring.sandbox.cleanup_checkpoint_failed",
                failure_stage = "sandbox.cleanup_confirmed",
                task_run_id = %intent.task_run_id,
                error_kind = ?error,
                "Kubernetes cleanup is confirmed but Agent could not persist its confirmation",
            );
            Some(error)
        }
    };
    let binding: TaskExecutionBinding = serde_json::from_value(intent.binding.clone())
        .map_err(|_| CleanupReleaseError::Persistence(AgentRunStoreError::InvalidContract))?;
    let initial = resources
        .get_task_resource(
            TaskRunId::from_str(&intent.task_run_id.to_string()).map_err(|_| {
                CleanupReleaseError::Persistence(AgentRunStoreError::InvalidContract)
            })?,
        )
        .await
        .map_err(|error| CleanupReleaseError::Resource(TaskResourceError::from(error)))?;
    if !binding.same_reservation(&initial) {
        return Err(CleanupReleaseError::Resource(
            TaskResourceError::IdentityMismatch,
        ));
    }
    let lifecycle = TaskResourceLifecycle::new(
        resources.clone(),
        initial.task_run_id,
        initial.project_id,
        initial.request.course_id,
        initial.owner_id,
        initial.request.request_key.clone(),
        initial.request.requested_resources.clone(),
        initial.request.requested_duration_seconds,
    )
    .map_err(CleanupReleaseError::Resource)?;
    let latest = lifecycle
        .load_status()
        .await
        .map_err(CleanupReleaseError::Resource)?;
    if !binding.same_reservation(&latest) {
        return Err(CleanupReleaseError::Resource(
            TaskResourceError::IdentityMismatch,
        ));
    }
    if !latest.cleanup_confirmed {
        lifecycle
            .release(&latest)
            .await
            .map_err(CleanupReleaseError::Resource)?;
    }
    if let Some(error) = confirmation_failed {
        return Err(CleanupReleaseError::Persistence(error));
    }
    store
        .mark_sandbox_released(intent)
        .await
        .map_err(CleanupReleaseError::Persistence)
}

fn derive_usage_deliveries(
    status: &TaskResourceStatus,
    timing: ExecutionTiming,
) -> Option<Vec<RecordResourceUsageRequest>> {
    let until = authority_now().ok()?;
    usage_deliveries(status, timing, until).ok()
}

pub(super) async fn cleanup_recovery_with_poll(
    api: &KubernetesApiClient,
    identity: &KubernetesJobIdentity,
    objects: &[ExecutionObjectRef],
) -> ExecutionCleanupStatus {
    const ATTEMPTS: usize = 4;
    const RETRY: Duration = Duration::from_millis(500);
    for attempt in 0..ATTEMPTS {
        let cleanup = match api.cleanup_recovery(identity, objects).await {
            Ok(cleanup) => cleanup,
            Err(error) => {
                tracing::warn!(
                    event = "agent.authoring.sandbox.cleanup_retryable",
                    failure_stage = "sandbox.cleanup_recovery",
                    error_kind = error.error_kind(),
                    attempt = attempt + 1,
                    "Kubernetes recovery cleanup failed; retrying the exact owned object set",
                );
                cleanup_unknown("LW_AGENT_SANDBOX_CLEANUP_UNKNOWN")
            }
        };
        if cleanup == ExecutionCleanupStatus::Confirmed || attempt + 1 == ATTEMPTS {
            return cleanup;
        }
        tokio::time::sleep(RETRY).await;
    }
    unreachable!("bounded cleanup loop always returns")
}

pub(super) fn reproducible_timing(observation: &ExecutionObservation) -> Option<ExecutionTiming> {
    let timing = attempt_timing(observation);
    timing.validate().ok()?;
    timing.started_at.zip(timing.terminated_at).map(|_| timing)
}

async fn deliver_usage_payload(
    resources: &ResourceClient,
    task_run_id: &TaskRunId,
    deliveries: &[RecordResourceUsageRequest],
) -> bool {
    let mut all_delivered = true;
    for delivery in deliveries {
        let mut delivered = false;
        for _ in 0..USAGE_DELIVERY_ATTEMPTS {
            if resources.record_resource_usage(delivery).await.is_ok() {
                delivered = true;
                break;
            }
            tokio::time::sleep(USAGE_DELIVERY_RETRY).await;
        }
        if !delivered {
            all_delivered = false;
            tracing::warn!(
                event = "agent.authoring.sandbox.usage_delivery_failed",
                failure_stage = "sandbox.usage",
                task_run_id = %task_run_id.as_uuid(),
                kind = ?delivery.kind,
                "authoring attempt could not deliver its usage observation",
            );
        }
    }
    all_delivered
}

/// Reports one failed authoring stage with the exact stage and cause.
///
/// The sandbox boundary collapses every internal failure into one process error, so the stage has
/// to be recorded where it happens or an attempt is indistinguishable from a provider outage.
/// Reports one failed attempt stage and fails the attempt closed.
///
/// Every step between the persisted attempt intent and the observed terminal Job reports its own
/// stage, so an operator can tell an unreachable object store from a rejected bundle or a refused
/// cluster apply without reading the attempt's pod.
fn stage_failure<E>(stage: &'static str, error: &E) -> ClaudeCodeProcessError
where
    E: std::fmt::Debug,
{
    tracing::error!(
        event = "agent.authoring.sandbox.stage_failed",
        failure_stage = stage,
        error_kind = ?error,
        "authoring attempt stage failed",
    );
    ClaudeCodeProcessError::Io
}

/// Reads the reviewed object-store trust root once, bounded like every other deployment input.
///
/// # Errors
///
/// Returns [`SandboxBundleError::Invalid`] for an unreadable, empty or oversized bundle.
fn read_object_store_ca(path: &Path) -> Result<String, SandboxBundleError> {
    const MAX_CA_BYTES: u64 = 1024 * 1024;
    let metadata = fs::metadata(path).map_err(|_| SandboxBundleError::Invalid)?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_CA_BYTES {
        return Err(SandboxBundleError::Invalid);
    }
    let bytes = fs::read(path).map_err(|_| SandboxBundleError::Invalid)?;
    Ok(STANDARD.encode(bytes))
}

/// Terminal receipt written by the sandbox attempt.
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxReceipt {
    pub result_size_bytes: u64,
    pub result_sha256: String,
    pub stderr_size_bytes: u64,
    pub stderr_sha256: String,
    pub exit_code: i32,
    pub claude_version: String,
    /// Zero when the attempt exported no OCI layout.
    pub export_size_bytes: u64,
    /// SHA256 of the empty stream when the attempt exported no OCI layout.
    pub export_sha256: String,
}

impl SandboxReceipt {
    fn validate(
        &self,
        scope: &AuthoringAttemptScope,
        result_max_bytes: u64,
        stderr_max_bytes: u64,
        export_max_bytes: u64,
    ) -> Result<(), SandboxReceiptError> {
        self.validate_version(
            &scope.claude_code_version,
            result_max_bytes,
            stderr_max_bytes,
            export_max_bytes,
        )
    }
    pub(super) fn validate_version(
        &self,
        version: &str,
        result_max_bytes: u64,
        stderr_max_bytes: u64,
        export_max_bytes: u64,
    ) -> Result<(), SandboxReceiptError> {
        if self.result_size_bytes > result_max_bytes
            || self.stderr_size_bytes > stderr_max_bytes
            || self.export_size_bytes > export_max_bytes
            || !valid_receipt_sha(self.result_size_bytes, &self.result_sha256)
            || !valid_receipt_sha(self.stderr_size_bytes, &self.stderr_sha256)
            || !valid_receipt_sha(self.export_size_bytes, &self.export_sha256)
            || self.claude_version != version
        {
            return Err(SandboxReceiptError::Invalid);
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(super) enum TerminalReceiptError {
    Invalid,
    Unavailable,
}

fn terminal_object_error(error: &artifact_store::ObjectStoreError) -> TerminalReceiptError {
    match error {
        artifact_store::ObjectStoreError::ObjectIdentityInvalid
        | artifact_store::ObjectStoreError::ObjectIdentityMismatch
        | artifact_store::ObjectStoreError::ObjectTooLarge
        | artifact_store::ObjectStoreError::ObjectNotFound => TerminalReceiptError::Invalid,
        _ => TerminalReceiptError::Unavailable,
    }
}
impl From<TerminalReceiptError> for ClaudeCodeProcessError {
    fn from(_: TerminalReceiptError) -> Self {
        Self::Io
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FrozenSandboxReceipt {
    task_run_id: uuid::Uuid,
    receipt: SandboxReceipt,
    keys: [String; 3],
    references: [Option<ArtifactRef>; 3],
}

impl FrozenSandboxReceipt {
    pub(crate) fn matches_identity(&self, task: uuid::Uuid, keys: [&str; 3]) -> bool {
        self.task_run_id == task && self.keys.iter().zip(keys).all(|(a, b)| a == b)
    }
    pub(crate) fn valid_metadata(&self, binding: &str) -> bool {
        let sizes = [
            self.receipt.result_size_bytes,
            self.receipt.stderr_size_bytes,
            self.receipt.export_size_bytes,
        ];
        let media = [RESULT_MEDIA_TYPE, STDERR_MEDIA_TYPE, EXPORT_MEDIA_TYPE];
        let shas = [
            &self.receipt.result_sha256,
            &self.receipt.stderr_sha256,
            &self.receipt.export_sha256,
        ];
        (0..3).all(|i| match (sizes[i], &self.references[i]) {
            (0, None) => valid_receipt_sha(0, shas[i]),
            (size, Some(reference)) => {
                size > 0
                    && valid_sha256(shas[i])
                    && reference.size_bytes == size
                    && reference.store_binding == binding
                    && !reference.object_version.is_empty()
                    && reference.media_type == media[i]
            }
            _ => false,
        })
    }
    pub(crate) fn same_outputs(&self, other: &Self) -> bool {
        self.task_run_id == other.task_run_id
            && self.receipt == other.receipt
            && self.keys == other.keys
            && self
                .references
                .iter()
                .zip(&other.references)
                .all(|(a, b)| match (a, b) {
                    (None, None) => true,
                    (Some(a), Some(b)) => {
                        a.store_binding == b.store_binding
                            && a.object_version == b.object_version
                            && a.size_bytes == b.size_bytes
                            && a.media_type == b.media_type
                    }
                    _ => false,
                })
    }
    async fn assemble(
        &self,
        objects: &dyn ImmutableObjectStore,
        verify_export: bool,
    ) -> Result<ClaudeCodeProcessOutput, TerminalReceiptError> {
        let sizes = [
            self.receipt.result_size_bytes,
            self.receipt.stderr_size_bytes,
            self.receipt.export_size_bytes,
        ];
        let shas = [
            &self.receipt.result_sha256,
            &self.receipt.stderr_sha256,
            &self.receipt.export_sha256,
        ];
        let media = [RESULT_MEDIA_TYPE, STDERR_MEDIA_TYPE, EXPORT_MEDIA_TYPE];
        for index in 0..3 {
            match (sizes[index], &self.references[index]) {
                (0, None) => {}
                (size, Some(reference))
                    if size > 0
                        && reference.size_bytes == size
                        && reference.store_binding == objects.binding()
                        && !reference.object_version.is_empty()
                        && reference.media_type == media[index] => {}
                _ => return Err(TerminalReceiptError::Invalid),
            }
        }
        let mut bytes = [Vec::new(), Vec::new()];
        for index in 0..2 {
            if sizes[index] == 0 {
                if self.references[index].is_some() {
                    return Err(TerminalReceiptError::Invalid);
                }
                continue;
            }
            let reference = self.references[index]
                .as_ref()
                .ok_or(TerminalReceiptError::Invalid)?;
            if reference.size_bytes != sizes[index] {
                return Err(TerminalReceiptError::Invalid);
            }
            let object = objects
                .read_verified(&self.keys[index], reference)
                .await
                .map_err(|error| terminal_object_error(&error))?;
            if Sha256Digest::of_bytes(&object.bytes).to_string() != *shas[index] {
                return Err(TerminalReceiptError::Invalid);
            }
            bytes[index] = object.bytes;
        }
        let mut output = ClaudeCodeProcessOutput::from_raw(
            Some(self.receipt.exit_code),
            std::mem::take(&mut bytes[0]),
            &bytes[1],
        );
        if self.receipt.export_size_bytes == 0 && self.references[2].is_some() {
            return Err(TerminalReceiptError::Invalid);
        }
        if self.receipt.export_size_bytes > 0 {
            let reference = self.references[2]
                .as_ref()
                .ok_or(TerminalReceiptError::Invalid)?;
            if reference.size_bytes != self.receipt.export_size_bytes {
                return Err(TerminalReceiptError::Invalid);
            }
            if verify_export {
                let file = objects
                    .read_verified_file(&self.keys[2], reference)
                    .await
                    .map_err(|error| terminal_object_error(&error))?;
                if file.sha256() != self.receipt.export_sha256 {
                    return Err(TerminalReceiptError::Invalid);
                }
            }
            output = output.with_image_export(contracts::supply_chain::ExportedOciImage {
                layout: reference.clone(),
                layout_object_key: self.keys[2].clone(),
            });
        }
        Ok(output)
    }
}

pub(super) async fn freeze_terminal_receipt(
    objects: &dyn ImmutableObjectStore,
    task_run_id: uuid::Uuid,
    receipt: &SandboxReceipt,
    keys: [&str; 3],
) -> Result<FrozenSandboxReceipt, TerminalReceiptError> {
    let sizes = [
        receipt.result_size_bytes,
        receipt.stderr_size_bytes,
        receipt.export_size_bytes,
    ];
    let shas = [
        &receipt.result_sha256,
        &receipt.stderr_sha256,
        &receipt.export_sha256,
    ];
    let media = [RESULT_MEDIA_TYPE, STDERR_MEDIA_TYPE, EXPORT_MEDIA_TYPE];
    let mut references = [None, None, None];
    for index in 0..2 {
        if sizes[index] == 0 {
            continue;
        }
        let object = objects
            .freeze_current(keys[index], sizes[index], media[index])
            .await
            .map_err(|error| terminal_object_error(&error))?;
        if Sha256Digest::of_bytes(&object.bytes).to_string() != *shas[index] {
            return Err(TerminalReceiptError::Invalid);
        }
        references[index] = Some(object.reference);
    }
    if receipt.export_size_bytes > 0 {
        let file = objects
            .freeze_current_file(keys[2], receipt.export_size_bytes, EXPORT_MEDIA_TYPE)
            .await
            .map_err(|error| terminal_object_error(&error))?;
        if file.sha256() != receipt.export_sha256 {
            return Err(TerminalReceiptError::Invalid);
        }
        references[2] = Some(file.reference().clone());
    }
    Ok(FrozenSandboxReceipt {
        task_run_id,
        receipt: receipt.clone(),
        keys: keys.map(str::to_owned),
        references,
    })
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SandboxResourceRequest {
    project_id: contracts::ProjectId,
    course_id: Option<contracts::CourseId>,
    actor_id: contracts::ActorId,
    request_key: String,
    pub(super) trace_id: String,
    resources: WorkloadResources,
    duration_seconds: u64,
}
impl SandboxResourceRequest {
    pub(super) fn lifecycle(
        &self,
        resources: &ResourceClient,
        task: TaskRunId,
    ) -> Result<TaskResourceLifecycle, ClaudeCodeProcessError> {
        TaskResourceLifecycle::new(
            resources.clone(),
            task,
            self.project_id,
            self.course_id,
            self.actor_id,
            self.request_key.clone(),
            self.resources.clone(),
            self.duration_seconds,
        )
        .map_err(|_| ClaudeCodeProcessError::Io)
    }
}

/// Rejected sandbox receipt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SandboxReceiptError {
    /// The receipt is malformed or differs from the pinned execution identity.
    Invalid,
}

/// Parses the bounded termination receipt.
///
/// # Errors
///
/// Returns [`SandboxReceiptError::Invalid`] when the payload is not the exact receipt.
pub fn parse_receipt(message: &str) -> Result<SandboxReceipt, SandboxReceiptError> {
    if message.len() > 4_096 {
        return Err(SandboxReceiptError::Invalid);
    }
    serde_json::from_str(message).map_err(|_| SandboxReceiptError::Invalid)
}

struct CancellationBridge(JoinHandle<()>);

impl Drop for CancellationBridge {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn bridge_cancellation(cancellation: &RunCancellation) -> (CancellationToken, CancellationBridge) {
    let token = CancellationToken::new();
    if cancellation.is_cancelled() {
        token.cancel();
        return (token, CancellationBridge(tokio::spawn(async {})));
    }
    let bridge = token.clone();
    let receiver = cancellation.clone();
    let handle = tokio::spawn(async move {
        receiver.cancelled().await;
        bridge.cancel();
    });
    (token, CancellationBridge(handle))
}

fn map_task_resource(error: &TaskResourceError) -> ClaudeCodeProcessError {
    match error {
        TaskResourceError::Cancelled => ClaudeCodeProcessError::Cancelled,
        TaskResourceError::ResourceApprovalTimeout => {
            ClaudeCodeProcessError::ResourceApprovalTimeout
        }
        _ => ClaudeCodeProcessError::Io,
    }
}

fn attempt_ownership(scope: &AuthoringAttemptScope, task_run_id: TaskRunId) -> KubernetesOwnership {
    attempt_ownership_from_parts(scope.run_id.as_uuid(), task_run_id, &scope.trace_id)
}

fn attempt_ownership_from_parts(
    run_id: uuid::Uuid,
    task_run_id: TaskRunId,
    trace_id: &str,
) -> KubernetesOwnership {
    let request_sha256 =
        Sha256Digest::of_bytes(format!("{trace_id}:{task_run_id}").as_bytes()).to_string();
    KubernetesOwnership {
        run_id,
        step_run_id: task_run_id.as_uuid(),
        attempt_id: task_run_id.as_uuid(),
        request_sha256,
    }
}

fn sandbox_failure(diagnostic: &str) -> ClaudeCodeProcessError {
    match diagnostic {
        "LW_AGENT_SANDBOX_DEADLINE_EXCEEDED" => ClaudeCodeProcessError::TimedOut,
        "LW_TASK_RESOURCE_APPROVAL_TIMEOUT" => ClaudeCodeProcessError::ResourceApprovalTimeout,
        _ => ClaudeCodeProcessError::Io,
    }
}

fn valid_receipt_sha(size: u64, value: &str) -> bool {
    valid_sha256(value) && (size > 0 || value == Sha256Digest::of_bytes(&[]).to_string())
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn workload_name(task_run_id: uuid::Uuid) -> String {
    format!("lw-auth-{}", &task_run_id.simple().to_string()[..20])
}

fn request_key(scope: &AuthoringAttemptScope, task_run_id: TaskRunId) -> String {
    format!(
        "authoring-{}-{}-{}-{}",
        scope.run_id.as_uuid().simple(),
        track_name(scope),
        scope.attempt,
        task_run_id.as_uuid().simple()
    )
}

fn track_name(scope: &AuthoringAttemptScope) -> &'static str {
    match scope.track {
        contracts::authoring::AgentTrackKind::Environment => "environment",
        contracts::authoring::AgentTrackKind::Evaluation => "evaluation",
        contracts::authoring::AgentTrackKind::WorkConfiguration => "work_configuration",
    }
}

fn object_key(prefix: &str, task_run_id: TaskRunId, name: &str) -> String {
    format!("{prefix}/{}/{name}", task_run_id.as_uuid().simple())
}

/// Merges the reviewed provider environment with the per-command CLI overrides.
///
/// Command entries win, because the runtime owns the CLI switches it sets; every other reviewed
/// entry (provider endpoint, model, credential) is required for the CLI to reach the model at all.
fn attempt_environment(
    worker_environment: &BTreeMap<String, String>,
    command_environment: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut environment = worker_environment.clone();
    environment.extend(
        command_environment
            .iter()
            .map(|(key, value)| (key.clone(), value.clone())),
    );
    environment
}

/// Timing of one observed attempt, taken from the shared observation.
fn attempt_timing(observation: &ExecutionObservation) -> ExecutionTiming {
    ExecutionTiming {
        started_at: observation.started_at,
        terminated_at: observation.terminated_at,
    }
}

fn command_argv(command: &ClaudeCodeCommand) -> Vec<String> {
    let mut argv = Vec::with_capacity(command.args().len() + 1);
    argv.push(command.program().to_owned());
    argv.extend(command.args().iter().cloned());
    argv
}

fn job_uid(refs: &[ExecutionObjectRef]) -> Option<String> {
    refs.iter()
        .find(|object| object.resource == "jobs")
        .map(|object| object.uid.clone())
}

fn authority_now() -> Result<UtcTimestamp, ClaudeCodeProcessError> {
    let value = OffsetDateTime::now_utc();
    let value = value
        .replace_nanosecond((value.nanosecond() / 1_000_000) * 1_000_000)
        .map_err(|_| ClaudeCodeProcessError::Io)?;
    UtcTimestamp::from_utc(value).map_err(|_| ClaudeCodeProcessError::Io)
}

#[async_trait]
impl ClaudeCodeProcess for SandboxAuthoringProcess {
    async fn recover_authoring_terminal(
        &self,
        scope: &AuthoringAttemptScope,
        cancellation: RunCancellation,
    ) -> Result<ClaudeCodeProcessOutput, ClaudeCodeProcessError> {
        if cancellation.is_cancelled() {
            return Err(ClaudeCodeProcessError::Cancelled);
        }
        let checkpoint = self
            .store
            .load_sandbox_attempt(
                scope.run_id,
                scope.track,
                scope.attempt,
                scope.execution_generation,
            )
            .await
            .map_err(|_| ClaudeCodeProcessError::Io)?
            .ok_or(ClaudeCodeProcessError::TimedOut)?;
        if checkpoint.state == "creating" {
            return Err(ClaudeCodeProcessError::TimedOut);
        }
        if checkpoint.state == "failed" {
            return Err(checkpoint
                .diagnostic_code
                .as_deref()
                .map_or(ClaudeCodeProcessError::Io, sandbox_failure));
        }
        Box::pin(self.finish_recovered_attempt(
            scope,
            &checkpoint,
            tokio::time::Instant::now(),
            cancellation,
        ))
        .await
    }

    async fn version(&self) -> Result<String, ClaudeCodeProcessError> {
        Err(ClaudeCodeProcessError::Unavailable)
    }

    fn verifies_identity_in_execution(&self) -> bool {
        true
    }

    async fn execute(
        &self,
        scope: &ExecutionScope,
        command: ClaudeCodeCommand,
        cancellation: RunCancellation,
    ) -> Result<ClaudeCodeProcessOutput, ClaudeCodeProcessError> {
        let ExecutionScope::Authoring(scope) = scope else {
            return Err(ClaudeCodeProcessError::Unavailable);
        };
        Box::pin(self.execute_authoring(scope, command, cancellation)).await
    }
}

#[cfg(test)]
mod tests {
    use super::{
        SandboxReceipt, SandboxReceiptError, attempt_environment, map_task_resource, parse_receipt,
        sandbox_failure,
    };
    use crate::claude_code::ClaudeCodeProcessError;
    use persistence_sqlx::Sha256Digest;
    use std::collections::BTreeMap;
    use task_execution::resource::TaskResourceError;

    use crate::claude_code::AuthoringAttemptScope;
    use contracts::authoring::AgentTrackKind;
    use contracts::{ActorId, AgentRunId, CourseId, ProjectId};

    #[test]
    fn resource_approval_timeout_keeps_its_resource_diagnostic() {
        assert_eq!(
            map_task_resource(&TaskResourceError::ResourceApprovalTimeout),
            ClaudeCodeProcessError::ResourceApprovalTimeout
        );
        assert_eq!(
            sandbox_failure("LW_TASK_RESOURCE_APPROVAL_TIMEOUT"),
            ClaudeCodeProcessError::ResourceApprovalTimeout
        );
        assert_eq!(
            map_task_resource(&TaskResourceError::Cancelled),
            ClaudeCodeProcessError::Cancelled
        );
        assert_eq!(
            map_task_resource(&TaskResourceError::ResourceTerminal),
            ClaudeCodeProcessError::Io
        );
    }

    #[test]
    fn attempt_environment_always_carries_the_reviewed_provider_environment() {
        // The sandboxed CLI runs in its own pod, so the reviewed provider endpoint, model and
        // credential must travel with the attempt; the per-command CLI switches still win.
        let worker = BTreeMap::from([
            (
                "ANTHROPIC_BASE_URL".to_owned(),
                "https://model.example".to_owned(),
            ),
            ("ANTHROPIC_MODEL".to_owned(), "model-1".to_owned()),
            ("ANTHROPIC_AUTH_TOKEN".to_owned(), "token-1".to_owned()),
            ("API_TIMEOUT_MS".to_owned(), "1".to_owned()),
        ]);
        let command = BTreeMap::from([("API_TIMEOUT_MS".to_owned(), "120000".to_owned())]);
        let environment = attempt_environment(&worker, &command);
        assert_eq!(
            environment.get("ANTHROPIC_BASE_URL").map(String::as_str),
            Some("https://model.example")
        );
        assert_eq!(
            environment.get("ANTHROPIC_MODEL").map(String::as_str),
            Some("model-1")
        );
        assert_eq!(
            environment.get("ANTHROPIC_AUTH_TOKEN").map(String::as_str),
            Some("token-1")
        );
        assert_eq!(
            environment.get("API_TIMEOUT_MS").map(String::as_str),
            Some("120000")
        );
    }

    const RESULT_MAX_BYTES: u64 = 4 * 1024 * 1024;
    const STDERR_MAX_BYTES: u64 = 1024 * 1024;
    const EXPORT_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;

    #[allow(clippy::expect_used)]
    fn scope() -> AuthoringAttemptScope {
        AuthoringAttemptScope {
            run_id: AgentRunId::new(),
            project_id: ProjectId::new(),
            course_id: Some(CourseId::new()),
            actor_id: ActorId::new(),
            track: AgentTrackKind::Environment,
            attempt: 1,
            execution_generation: 1,
            started_at: contracts::UtcTimestamp::from_utc(
                time::OffsetDateTime::now_utc()
                    .replace_nanosecond(0)
                    .expect("valid nanosecond"),
            )
            .expect("valid timestamp"),
            worker_id: "test-worker".to_owned(),
            lease_token: uuid::Uuid::now_v7(),
            trace_id: "trace-receipt".to_owned(),
            claude_code_version: "2.1.215".to_owned(),
        }
    }

    fn receipt() -> SandboxReceipt {
        SandboxReceipt {
            result_size_bytes: 12,
            result_sha256: "a".repeat(64),
            stderr_size_bytes: 0,
            stderr_sha256: Sha256Digest::of_bytes(&[]).to_string(),
            exit_code: 0,
            claude_version: "2.1.215".to_owned(),
            export_size_bytes: 0,
            export_sha256: Sha256Digest::of_bytes(&[]).to_string(),
        }
    }

    fn validate(receipt: &SandboxReceipt) -> Result<(), SandboxReceiptError> {
        receipt.validate(
            &scope(),
            RESULT_MAX_BYTES,
            STDERR_MAX_BYTES,
            EXPORT_MAX_BYTES,
        )
    }

    #[test]
    fn receipt_validation_fails_closed_outside_the_scope_bounds() {
        // Every size exactly at its bound, with matching digests, is accepted.
        let mut at_bounds = receipt();
        at_bounds.result_size_bytes = RESULT_MAX_BYTES;
        at_bounds.stderr_size_bytes = STDERR_MAX_BYTES;
        at_bounds.stderr_sha256 = "b".repeat(64);
        at_bounds.export_size_bytes = EXPORT_MAX_BYTES;
        at_bounds.export_sha256 = "c".repeat(64);
        assert_eq!(validate(&at_bounds), Ok(()));

        let mut oversized = receipt();
        oversized.result_size_bytes = RESULT_MAX_BYTES + 1;
        assert_eq!(
            validate(&oversized).err(),
            Some(SandboxReceiptError::Invalid)
        );

        let mut oversized = receipt();
        oversized.stderr_size_bytes = STDERR_MAX_BYTES + 1;
        oversized.stderr_sha256 = "b".repeat(64);
        assert_eq!(
            validate(&oversized).err(),
            Some(SandboxReceiptError::Invalid)
        );

        let mut oversized = receipt();
        oversized.export_size_bytes = EXPORT_MAX_BYTES + 1;
        oversized.export_sha256 = "c".repeat(64);
        assert_eq!(
            validate(&oversized).err(),
            Some(SandboxReceiptError::Invalid)
        );

        // A non-zero size must be paired with a lowercase hex sha256.
        let mut non_hex = receipt();
        non_hex.result_size_bytes = 1;
        non_hex.result_sha256 = "z".repeat(64);
        assert_eq!(validate(&non_hex).err(), Some(SandboxReceiptError::Invalid));

        let mut non_hex = receipt();
        non_hex.stderr_size_bytes = 1;
        non_hex.stderr_sha256 = "Z".repeat(64);
        assert_eq!(validate(&non_hex).err(), Some(SandboxReceiptError::Invalid));

        let mut non_hex = receipt();
        non_hex.export_size_bytes = 1;
        non_hex.export_sha256 = "g".repeat(64);
        assert_eq!(validate(&non_hex).err(), Some(SandboxReceiptError::Invalid));

        // The receipt must report the scope's pinned Claude Code version.
        let mut drifted = receipt();
        drifted.claude_version = "2.1.999".to_owned();
        assert_eq!(validate(&drifted).err(), Some(SandboxReceiptError::Invalid));
        let mut malformed_empty = receipt();
        malformed_empty.stderr_sha256.clear();
        assert_eq!(
            validate(&malformed_empty).err(),
            Some(SandboxReceiptError::Invalid)
        );
    }

    #[test]
    #[allow(clippy::expect_used)]
    fn receipt_parsing_is_exact_and_bounded() {
        let message = r#"{"resultSizeBytes":12,"resultSha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","stderrSizeBytes":0,"stderrSha256":"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","exitCode":0,"claudeVersion":"2.1.215","exportSizeBytes":0,"exportSha256":"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"}"#;
        let receipt = parse_receipt(message).expect("receipt must parse");
        assert_eq!(receipt.result_size_bytes, 12);
        assert_eq!(receipt.claude_version, "2.1.215");
        assert_eq!(receipt.exit_code, 0);
        assert!(parse_receipt("not json").is_err());
        let mut missing: serde_json::Value = serde_json::from_str(message).expect("fixture JSON");
        missing
            .as_object_mut()
            .expect("object")
            .remove("exportSha256");
        assert!(parse_receipt(&missing.to_string()).is_err());
        assert!(parse_receipt(&"x".repeat(4_097)).is_err());
        assert_eq!(
            parse_receipt(r#"{"extra":true}"#).err(),
            Some(SandboxReceiptError::Invalid)
        );
    }
    struct ReceiptObjects {
        export: Vec<u8>,
        stream_reads: std::sync::atomic::AtomicUsize,
        paths: std::sync::Mutex<Vec<std::path::PathBuf>>,
    }

    impl ReceiptObjects {
        fn reference(size: u64, media: &str) -> contracts::ArtifactRef {
            contracts::ArtifactRef {
                artifact_id: contracts::ArtifactId::new(),
                store_binding: "receipt-test".to_owned(),
                object_version: "version-1".to_owned(),
                size_bytes: size,
                media_type: media.to_owned(),
            }
        }
        fn file(
            &self,
            reference: contracts::ArtifactRef,
        ) -> Result<artifact_store::VerifiedObjectFile, artifact_store::ObjectStoreError> {
            let file = artifact_store::VerifiedObjectFile::from_bytes(reference, &self.export)?;
            self.paths
                .lock()
                .map_err(|_| artifact_store::ObjectStoreError::ObjectUnavailable)?
                .push(file.path().to_path_buf());
            Ok(file)
        }
    }

    #[async_trait::async_trait]
    impl artifact_store::ImmutableObjectStore for ReceiptObjects {
        async fn delete_orphan(
            &self,
            _key: &str,
            _version: &str,
        ) -> Result<(), artifact_store::ObjectStoreError> {
            Err(artifact_store::ObjectStoreError::StreamingUnsupported)
        }

        fn binding(&self) -> &'static str {
            "receipt-test"
        }
        async fn presign_upload(
            &self,
            _key: &str,
            _size: u64,
            _media: &str,
            _now: contracts::UtcTimestamp,
        ) -> Result<artifact_store::PresignedUpload, artifact_store::ObjectStoreError> {
            Err(artifact_store::ObjectStoreError::SigningFailed)
        }
        async fn read_verified(
            &self,
            _key: &str,
            _expected: &contracts::ArtifactRef,
        ) -> Result<artifact_store::VerifiedObject, artifact_store::ObjectStoreError> {
            // This fixture has only an export: accepting a byte read would hide a streaming regression.
            Err(artifact_store::ObjectStoreError::StreamingUnsupported)
        }
        async fn freeze_current(
            &self,
            _key: &str,
            _size: u64,
            _media: &str,
        ) -> Result<artifact_store::VerifiedObject, artifact_store::ObjectStoreError> {
            Err(artifact_store::ObjectStoreError::StreamingUnsupported)
        }
        async fn freeze_current_file(
            &self,
            _key: &str,
            size: u64,
            media: &str,
        ) -> Result<artifact_store::VerifiedObjectFile, artifact_store::ObjectStoreError> {
            self.file(Self::reference(size, media))
        }
        async fn read_verified_file(
            &self,
            _key: &str,
            expected: &contracts::ArtifactRef,
        ) -> Result<artifact_store::VerifiedObjectFile, artifact_store::ObjectStoreError> {
            self.stream_reads
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.file(expected.clone())
        }
    }

    #[tokio::test]
    async fn frozen_export_uses_exact_version_file_reads_and_drops_owned_files()
    -> Result<(), Box<dyn std::error::Error>> {
        let objects = ReceiptObjects {
            export: b"oci-layout-test".to_vec(),
            stream_reads: std::sync::atomic::AtomicUsize::new(0),
            paths: std::sync::Mutex::new(Vec::new()),
        };
        let empty = Sha256Digest::of_bytes(&[]).to_string();
        let receipt = SandboxReceipt {
            result_size_bytes: 0,
            result_sha256: empty.clone(),
            stderr_size_bytes: 0,
            stderr_sha256: empty,
            exit_code: 7,
            claude_version: "2.1.215".to_owned(),
            export_size_bytes: u64::try_from(objects.export.len())?,
            export_sha256: Sha256Digest::of_bytes(&objects.export).to_string(),
        };
        let frozen = super::freeze_terminal_receipt(
            &objects,
            uuid::Uuid::now_v7(),
            &receipt,
            ["own/result", "own/stderr", "own/export"],
        )
        .await
        .map_err(|e| format!("{e:?}"))?;
        assert!(frozen.valid_metadata("receipt-test"));
        let output = frozen
            .assemble(&objects, true)
            .await
            .map_err(|e| format!("{e:?}"))?;
        assert!(!output.is_success());
        assert_eq!(
            objects
                .stream_reads
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        assert!(
            objects
                .paths
                .lock()
                .map_err(|_| "lock poisoned")?
                .iter()
                .all(|p| !p.exists())
        );
        let mut same = frozen.clone();
        same.references[2]
            .as_mut()
            .ok_or("export missing")?
            .artifact_id = contracts::ArtifactId::new();
        assert!(frozen.same_outputs(&same));
        same.references[2]
            .as_mut()
            .ok_or("export missing")?
            .object_version = "different-version".to_owned();
        assert!(!frozen.same_outputs(&same));
        let mut wrong = frozen;
        wrong.receipt.export_sha256 = "a".repeat(64);
        assert!(matches!(
            wrong.assemble(&objects, true).await,
            Err(super::TerminalReceiptError::Invalid)
        ));
        assert!(
            objects
                .paths
                .lock()
                .map_err(|_| "lock poisoned")?
                .iter()
                .all(|p| !p.exists())
        );
        Ok(())
    }
}
