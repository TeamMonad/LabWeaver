//! Admitted Kubernetes sandbox execution for authoring attempts.
//!
//! One `ClaudeCodeProcess::execute` call for an authoring scope runs exactly one Resource-admitted
//! Kubernetes Job: the attempt first proves its Resource binding through the shared admission
//! witness, persists that binding before any cluster object exists, materializes the classified
//! egress envelope through short-lived object-store URLs, observes the Job and then persists the
//! terminal receipt before cleaning up and releasing the reservation.

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use artifact_store::{ImmutableObjectStore, S3ImmutableObjectStore};
use async_trait::async_trait;
use contracts::execution::{ExecutionCleanupStatus, ExecutionObjectRef, TaskExecutionBinding};
use contracts::resource::WorkloadResources;
use contracts::{TaskRunId, UtcTimestamp};
use persistence_sqlx::Sha256Digest;
use serde::Deserialize;
use task_execution::admission::{AdmittedExecution, cleanup_unknown};
use task_execution::kubernetes::{
    KubernetesApiClient, KubernetesApiConfiguration, KubernetesJobBundle, KubernetesJobIdentity,
    KubernetesJobObservation, KubernetesOwnership,
};
use task_execution::resource::{ResourceClient, TaskResourceError, TaskResourceLifecycle};
use time::OffsetDateTime;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::claude_code::{
    AuthoringAttemptScope, ClaudeCodeCommand, ClaudeCodeProcess, ClaudeCodeProcessError,
    ClaudeCodeProcessOutput, ExecutionScope, RunCancellation,
};
use crate::run_store::{PostgresAgentRunStore, SandboxAttemptCheckpoint, SandboxAttemptIntent};
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
const SANDBOX_DEADLINE_SLACK_SECONDS: u64 = 300;
const OBSERVE_POLL: Duration = Duration::from_secs(2);

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
}

/// Admitted Kubernetes execution backend for authoring attempts.
#[derive(Clone)]
pub struct SandboxAuthoringProcess {
    api: KubernetesApiClient,
    resources: ResourceClient,
    store: PostgresAgentRunStore,
    objects: Arc<S3ImmutableObjectStore>,
    configuration: SandboxProcessConfiguration,
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
        {
            return Err(SandboxBundleError::Invalid);
        }
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
        })
    }

    async fn execute_authoring(
        &self,
        scope: &AuthoringAttemptScope,
        command: ClaudeCodeCommand,
        cancellation: RunCancellation,
    ) -> Result<ClaudeCodeProcessOutput, ClaudeCodeProcessError> {
        if let Some(checkpoint) = self
            .store
            .load_sandbox_attempt(scope.run_id, scope.track, scope.attempt)
            .await
            .map_err(|_| ClaudeCodeProcessError::Io)?
        {
            return self.finish_recovered_attempt(scope, &checkpoint).await;
        }
        self.execute_new_attempt(scope, command, cancellation).await
    }

    #[allow(clippy::too_many_lines)]
    async fn execute_new_attempt(
        &self,
        scope: &AuthoringAttemptScope,
        command: ClaudeCodeCommand,
        cancellation: RunCancellation,
    ) -> Result<ClaudeCodeProcessOutput, ClaudeCodeProcessError> {
        let task_run_id = TaskRunId::new();
        let workload_name = workload_name(task_run_id.as_uuid());
        let request_key = request_key(scope, task_run_id);
        let lifecycle = TaskResourceLifecycle::new(
            self.resources.clone(),
            task_run_id,
            scope.project_id,
            scope.course_id,
            scope.actor_id,
            request_key,
            WorkloadResources {
                cpu_millicores: self.configuration.sandbox.cpu_millicores,
                memory_bytes: self.configuration.sandbox.memory_bytes,
                storage_bytes: self.configuration.sandbox.workspace_bytes,
                gpu: None,
            },
            self.configuration.sandbox.wall_time_seconds,
        )
        .map_err(|_| ClaudeCodeProcessError::Io)?;
        let (cancel_token, _bridge) = bridge_cancellation(&cancellation);
        let approval_timeout = Duration::from_secs(self.configuration.sandbox.wall_time_seconds);
        let approval = lifecycle
            .claim_after_approval(OBSERVE_POLL, approval_timeout, &cancel_token)
            .await
            .map_err(|error| map_task_resource(&error))?;
        let status = lifecycle
            .acknowledge(&approval, &self.configuration.sandbox.namespace)
            .await
            .map_err(|error| map_task_resource(&error))?;
        let admitted =
            AdmittedExecution::admit(&status, 1, workload_name.clone(), scope.trace_id.clone())
                .map_err(|_| ClaudeCodeProcessError::Io)?;
        let binding = admitted.binding().clone();
        let ownership = attempt_ownership(scope, task_run_id);
        let intent = SandboxAttemptIntent {
            run_id: scope.run_id,
            track: scope.track,
            attempt: scope.attempt,
            task_run_id: task_run_id.as_uuid(),
            execution_generation: 1,
            namespace: self.configuration.sandbox.namespace.clone(),
            workload_name: workload_name.clone(),
            binding: serde_json::to_value(&binding).map_err(|_| ClaudeCodeProcessError::Io)?,
        };
        self.store
            .begin_sandbox_attempt(&intent)
            .await
            .map_err(|_| ClaudeCodeProcessError::Io)?;

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
            .map_err(|_| ClaudeCodeProcessError::Io)?;
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
        let result_key = object_key(
            &self.configuration.object_prefix,
            task_run_id,
            "result.json",
        );
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
        let stderr_key = object_key(&self.configuration.object_prefix, task_run_id, "stderr.log");
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

        let spec = SandboxAttemptSpec {
            task_run_id: task_run_id.as_uuid(),
            ownership,
            trace_id: scope.trace_id.clone(),
            command: command_argv(&command),
            command_environment: command.env().clone(),
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
            object_store_ca_base64: None,
        };
        let sandbox_bundle = build_sandbox_bundle(&self.configuration.sandbox, &spec)
            .map_err(|_| ClaudeCodeProcessError::Io)?;
        let bundle = sandbox_bundle.bundle;
        self.api
            .start(&bundle)
            .await
            .map_err(|_| ClaudeCodeProcessError::Io)?;
        let refs = self
            .api
            .capture_object_refs(&bundle.identity, &bundle.cleanup_plan)
            .await
            .map_err(|_| ClaudeCodeProcessError::Io)?;
        self.store
            .record_sandbox_objects(
                &intent,
                &serde_json::to_value(&refs).map_err(|_| ClaudeCodeProcessError::Io)?,
            )
            .await
            .map_err(|_| ClaudeCodeProcessError::Io)?;

        let expected_uid = job_uid(&refs);
        let deadline = tokio::time::Instant::now()
            + Duration::from_secs(
                self.configuration.sandbox.wall_time_seconds + SANDBOX_DEADLINE_SLACK_SECONDS,
            );
        loop {
            if cancellation.is_cancelled() {
                self.cleanup_and_release(&intent, &bundle, &lifecycle, &status)
                    .await;
                return Err(ClaudeCodeProcessError::Cancelled);
            }
            let observation = self
                .api
                .observe(&bundle.identity, expected_uid.as_deref())
                .await
                .map_err(|_| ClaudeCodeProcessError::Io)?;
            match observation {
                KubernetesJobObservation::Completed { message, .. } => {
                    let receipt =
                        parse_receipt(&message).map_err(|_| ClaudeCodeProcessError::Io)?;
                    receipt
                        .validate(
                            scope,
                            self.configuration.result_max_bytes,
                            self.configuration.stderr_max_bytes,
                        )
                        .map_err(|_| ClaudeCodeProcessError::Io)?;
                    let output = self
                        .assemble_output(&receipt, result_key.as_str(), stderr_key.as_str())
                        .await?;
                    self.store
                        .complete_sandbox_attempt(
                            &intent,
                            Some((
                                result_key.as_str(),
                                receipt.result_sha256.as_str(),
                                receipt.result_size_bytes,
                            )),
                            receipt.exit_code,
                            None,
                        )
                        .await
                        .map_err(|_| ClaudeCodeProcessError::Io)?;
                    self.cleanup_and_release(&intent, &bundle, &lifecycle, &status)
                        .await;
                    return Ok(output);
                }
                KubernetesJobObservation::Failed {
                    diagnostic_code, ..
                } => {
                    self.store
                        .complete_sandbox_attempt(&intent, None, 1, Some(diagnostic_code.as_str()))
                        .await
                        .map_err(|_| ClaudeCodeProcessError::Io)?;
                    self.cleanup_and_release(&intent, &bundle, &lifecycle, &status)
                        .await;
                    return Err(ClaudeCodeProcessError::Io);
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
                self.cleanup_and_release(&intent, &bundle, &lifecycle, &status)
                    .await;
                return Err(ClaudeCodeProcessError::TimedOut);
            }
            tokio::time::sleep(OBSERVE_POLL).await;
        }
    }

    async fn finish_recovered_attempt(
        &self,
        scope: &AuthoringAttemptScope,
        checkpoint: &SandboxAttemptCheckpoint,
    ) -> Result<ClaudeCodeProcessOutput, ClaudeCodeProcessError> {
        let task_run_id = TaskRunId::from_str(&checkpoint.task_run_id.to_string())
            .map_err(|_| ClaudeCodeProcessError::Io)?;
        let lifecycle = TaskResourceLifecycle::new(
            self.resources.clone(),
            task_run_id,
            scope.project_id,
            scope.course_id,
            scope.actor_id,
            request_key(scope, task_run_id),
            WorkloadResources {
                cpu_millicores: self.configuration.sandbox.cpu_millicores,
                memory_bytes: self.configuration.sandbox.memory_bytes,
                storage_bytes: self.configuration.sandbox.workspace_bytes,
                gpu: None,
            },
            self.configuration.sandbox.wall_time_seconds,
        )
        .map_err(|_| ClaudeCodeProcessError::Io)?;
        let status = lifecycle
            .load_status()
            .await
            .map_err(|error| map_task_resource(&error))?;
        let binding: TaskExecutionBinding = serde_json::from_value(checkpoint.binding.clone())
            .map_err(|_| ClaudeCodeProcessError::Io)?;
        AdmittedExecution::recover(binding, &status, checkpoint.execution_generation)
            .map_err(|_| ClaudeCodeProcessError::Io)?;
        let objects: Vec<ExecutionObjectRef> = serde_json::from_value(checkpoint.objects.clone())
            .map_err(|_| ClaudeCodeProcessError::Io)?;
        let identity = Self::attempt_identity(
            &checkpoint.namespace,
            &checkpoint.workload_name,
            attempt_ownership(scope, task_run_id),
            &scope.trace_id,
        );
        let cleanup = self
            .api
            .cleanup_recovery(&identity, &objects)
            .await
            .unwrap_or_else(|_| cleanup_unknown("LW_AGENT_SANDBOX_CLEANUP_UNKNOWN"));
        if cleanup == ExecutionCleanupStatus::Confirmed {
            let intent = SandboxAttemptIntent {
                run_id: scope.run_id,
                track: scope.track,
                attempt: scope.attempt,
                task_run_id: checkpoint.task_run_id,
                execution_generation: checkpoint.execution_generation,
                namespace: checkpoint.namespace.clone(),
                workload_name: checkpoint.workload_name.clone(),
                binding: checkpoint.binding.clone(),
            };
            let _ = self.store.confirm_sandbox_cleanup(&intent).await;
            let latest = lifecycle
                .load_status()
                .await
                .map_err(|error| map_task_resource(&error))?;
            if lifecycle.release(&latest).await.is_ok() {
                let _ = self.store.mark_sandbox_released(&intent).await;
            }
        }
        Err(ClaudeCodeProcessError::Io)
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

    async fn assemble_output(
        &self,
        receipt: &SandboxReceipt,
        result_key: &str,
        stderr_key: &str,
    ) -> Result<ClaudeCodeProcessOutput, ClaudeCodeProcessError> {
        let result = if receipt.result_size_bytes == 0 {
            Vec::new()
        } else {
            let frozen = self
                .objects
                .freeze_current(result_key, receipt.result_size_bytes, RESULT_MEDIA_TYPE)
                .await
                .map_err(|_| ClaudeCodeProcessError::Io)?;
            if Sha256Digest::of_bytes(&frozen.bytes).to_string() != receipt.result_sha256 {
                return Err(ClaudeCodeProcessError::Io);
            }
            frozen.bytes
        };
        let stderr = if receipt.stderr_size_bytes == 0 {
            Vec::new()
        } else {
            let frozen = self
                .objects
                .freeze_current(stderr_key, receipt.stderr_size_bytes, STDERR_MEDIA_TYPE)
                .await
                .map_err(|_| ClaudeCodeProcessError::Io)?;
            if Sha256Digest::of_bytes(&frozen.bytes).to_string() != receipt.stderr_sha256 {
                return Err(ClaudeCodeProcessError::Io);
            }
            frozen.bytes
        };
        Ok(ClaudeCodeProcessOutput::from_raw(
            Some(receipt.exit_code),
            result,
            &stderr,
        ))
    }

    async fn cleanup_and_release(
        &self,
        intent: &SandboxAttemptIntent,
        bundle: &KubernetesJobBundle,
        lifecycle: &TaskResourceLifecycle,
        status: &contracts::http::TaskResourceStatus,
    ) {
        let cleanup = self
            .api
            .cleanup(
                &intent.namespace,
                &intent.workload_name,
                &bundle.objects,
                &bundle.cleanup_plan,
            )
            .await
            .unwrap_or_else(|_| cleanup_unknown("LW_AGENT_SANDBOX_CLEANUP_UNKNOWN"));
        if cleanup == ExecutionCleanupStatus::Confirmed {
            let _ = self.store.confirm_sandbox_cleanup(intent).await;
            let latest = match lifecycle.load_status().await {
                Ok(latest) => latest,
                Err(_) => status.clone(),
            };
            if lifecycle.release(&latest).await.is_ok() {
                let _ = self.store.mark_sandbox_released(intent).await;
            }
        }
    }
}

/// Terminal receipt written by the sandbox attempt.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxReceipt {
    pub result_size_bytes: u64,
    pub result_sha256: String,
    pub stderr_size_bytes: u64,
    pub stderr_sha256: String,
    pub exit_code: i32,
    pub claude_version: String,
}

impl SandboxReceipt {
    fn validate(
        &self,
        scope: &AuthoringAttemptScope,
        result_max_bytes: u64,
        stderr_max_bytes: u64,
    ) -> Result<(), SandboxReceiptError> {
        if self.result_size_bytes > result_max_bytes
            || self.stderr_size_bytes > stderr_max_bytes
            || (self.result_size_bytes > 0 && !valid_sha256(&self.result_sha256))
            || (self.stderr_size_bytes > 0 && !valid_sha256(&self.stderr_sha256))
            || self.claude_version != scope.claude_code_version
        {
            return Err(SandboxReceiptError::Invalid);
        }
        Ok(())
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
        _ => ClaudeCodeProcessError::Io,
    }
}

fn attempt_ownership(scope: &AuthoringAttemptScope, task_run_id: TaskRunId) -> KubernetesOwnership {
    let request_sha256 =
        Sha256Digest::of_bytes(format!("{}:{}", scope.trace_id, task_run_id).as_bytes())
            .to_string();
    KubernetesOwnership {
        run_id: scope.run_id.as_uuid(),
        step_run_id: task_run_id.as_uuid(),
        attempt_id: task_run_id.as_uuid(),
        request_sha256,
    }
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
        self.execute_authoring(scope, command, cancellation).await
    }
}

#[cfg(test)]
mod tests {
    use super::{SandboxReceiptError, parse_receipt};

    #[test]
    #[allow(clippy::expect_used)]
    fn receipt_parsing_is_exact_and_bounded() {
        let message = r#"{"resultSizeBytes":12,"resultSha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","stderrSizeBytes":0,"stderrSha256":"","exitCode":0,"claudeVersion":"2.1.215"}"#;
        let receipt = parse_receipt(message).expect("receipt must parse");
        assert_eq!(receipt.result_size_bytes, 12);
        assert_eq!(receipt.claude_version, "2.1.215");
        assert_eq!(receipt.exit_code, 0);
        assert!(parse_receipt("not json").is_err());
        assert!(parse_receipt(&"x".repeat(4_097)).is_err());
        assert_eq!(
            parse_receipt(r#"{"extra":true}"#).err(),
            Some(SandboxReceiptError::Invalid)
        );
    }
}
