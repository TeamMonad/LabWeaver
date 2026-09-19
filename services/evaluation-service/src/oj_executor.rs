//! Attempt-scoped Kubernetes executor for isolated OJ resources.
//!
//! The shared mutation, observation, and cleanup mechanics live in
//! [`crate::kubernetes_job`]; this module owns only the OJ resource documents, the evidence
//! receipt, and the stable OJ diagnostics.
#![allow(
    missing_docs,
    clippy::too_many_lines,
    reason = "the OJ role adapter keeps its exact diagnostics and receipt semantics auditable"
)]

use std::path::PathBuf;

use contracts::execution::{ExecutionCleanupStatus, ExecutionObjectRef, ExecutionObservation};
use reqwest::Url;
use serde::Deserialize;
use thiserror::Error;

use crate::{
    control_plane::{EvaluationExecutionKind, EvaluationExecutionResources},
    kubernetes_job::{
        KubernetesApiClient, KubernetesApiConfiguration, KubernetesCleanupTarget,
        KubernetesJobBundle, KubernetesJobError, KubernetesJobIdentity, KubernetesJobObservation,
        KubernetesObject, KubernetesOwnership,
    },
    oj::{OjEvidenceReceipt, OjExecutionRequest},
    oj_job::{OjJobBinding, OjJobError, OjJobResources},
};

const FIELD_MANAGER: &str = "labweaver-oj-executor";
const LOG_SCOPE: &str = "program.oj";
const DIAGNOSTIC_PREFIX: &str = "LW_OJ_";
const RUNNER_DEFAULT_DENY_POLICY: &str = "oj-runner-default-deny";
const MAIN_CONTAINER: &str = "program-runner";
const DEADLINE_DIAGNOSTIC_CODE: &str = "LW_OJ_JOB_DEADLINE_EXCEEDED";
const FAILED_DIAGNOSTIC_CODE: &str = "LW_OJ_JOB_FAILED";
const OOM_DIAGNOSTIC_CODE: &str = "LW_OJ_MEMORY_LIMIT";
const STABLE_DIAGNOSTIC_PREFIX: &str = "LW_OJ_";

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OjExecutorConfiguration {
    pub kubernetes_api_server: Url,
    pub kubernetes_bearer_token_file: PathBuf,
    pub kubernetes_ca_file: PathBuf,
    pub runner_namespace: String,
    pub request_timeout_milliseconds: u64,
}

#[derive(Clone)]
pub struct OjKubernetesExecutor {
    api: KubernetesApiClient,
}

impl OjKubernetesExecutor {
    /// Builds an HTTPS-only, CA-pinned Kubernetes executor.
    ///
    /// # Errors
    ///
    /// Returns a stable configuration error for invalid or unavailable credentials.
    pub fn new(configuration: OjExecutorConfiguration) -> Result<Self, OjExecutorError> {
        let api = KubernetesApiClient::new(
            api_configuration(configuration),
            FIELD_MANAGER,
            LOG_SCOPE,
            DIAGNOSTIC_PREFIX,
        )?;
        Ok(Self { api })
    }

    /// Applies the exact attempt-scoped network policy, command, and Job bundle.
    ///
    /// # Errors
    ///
    /// Fails closed on invalid ownership, a partial old bundle that cannot be removed, or an API
    /// rejection.
    pub async fn start(&self, binding: &OjJobBinding) -> Result<OjJobResources, OjExecutorError> {
        let resources = OjJobResources::build(binding)?;
        let bundle = job_bundle(&binding.namespace, &resources, &binding.request)?;
        self.api.start(&bundle).await?;
        Ok(resources)
    }

    /// Observes the exact Job and its single owned Pod without accepting ambiguous evidence.
    ///
    /// # Errors
    ///
    /// Returns a stable error when Kubernetes is unavailable or ownership/evidence is invalid.
    pub async fn observe(
        &self,
        resources: &OjJobResources,
        request: &OjExecutionRequest,
    ) -> Result<OjJobObservation, OjExecutorError> {
        self.observe_job(resources.name(), None, request).await
    }

    /// Observes a recovered attempt using only its durable object references.
    ///
    /// # Errors
    ///
    /// Returns an error when the recovered resource identity or observed Kubernetes state is
    /// invalid or unavailable.
    pub async fn observe_recovery(
        &self,
        resources: &EvaluationExecutionResources,
        request: &OjExecutionRequest,
    ) -> Result<OjJobObservation, OjExecutorError> {
        if resources.namespace != self.api.runner_namespace()
            || resources.kind != EvaluationExecutionKind::Program
        {
            return Err(OjExecutorError::BindingInvalid);
        }
        let job = resources
            .objects
            .iter()
            .find(|object| object.resource == "jobs");
        if let Some(job) = job {
            self.observe_job(&job.name, Some(job.uid.as_str()), request)
                .await
        } else {
            request
                .validate()
                .map_err(|_| OjExecutorError::BindingInvalid)?;
            self.observe_job(&attempt_job_name(request.attempt_id), None, request)
                .await
        }
    }

    async fn observe_job(
        &self,
        job_name: &str,
        expected_uid: Option<&str>,
        request: &OjExecutionRequest,
    ) -> Result<OjJobObservation, OjExecutorError> {
        let identity = request_identity(self.api.runner_namespace(), job_name, request)?;
        match self.api.observe(&identity, expected_uid).await? {
            KubernetesJobObservation::Missing => Ok(OjJobObservation::Missing),
            KubernetesJobObservation::Running => Ok(OjJobObservation::Running),
            KubernetesJobObservation::Completed {
                message,
                observation,
            } => {
                let receipt: OjEvidenceReceipt =
                    serde_json::from_str(&message).map_err(|_| OjExecutorError::ReceiptInvalid)?;
                receipt
                    .validate_for(request)
                    .map_err(|_| OjExecutorError::ReceiptInvalid)?;
                Ok(OjJobObservation::Completed {
                    receipt,
                    observation,
                })
            }
            KubernetesJobObservation::Failed {
                diagnostic_code,
                observation,
            } => Ok(OjJobObservation::Failed {
                diagnostic_code,
                observation,
            }),
        }
    }

    /// Captures the immutable object identities after the bundle is applied.
    ///
    /// # Errors
    ///
    /// Returns an error when an expected object is unavailable, not owned by this attempt, or has
    /// an invalid identity.
    pub async fn capture_object_refs(
        &self,
        resources: &OjJobResources,
        request: &OjExecutionRequest,
    ) -> Result<Vec<ExecutionObjectRef>, OjExecutorError> {
        let identity = request_identity(self.api.runner_namespace(), resources.name(), request)?;
        let cleanup_plan = cleanup_targets(resources);
        Ok(self
            .api
            .capture_object_refs(&identity, &cleanup_plan)
            .await?)
    }

    /// Captures a pending intent's object identities without rebuilding the bundle.
    ///
    /// # Errors
    ///
    /// Returns an error when the pending request or any observed object fails identity or
    /// ownership validation.
    pub async fn capture_intent_object_refs(
        &self,
        resources: &EvaluationExecutionResources,
        request: &OjExecutionRequest,
    ) -> Result<Option<Vec<ExecutionObjectRef>>, OjExecutorError> {
        if resources.namespace != self.api.runner_namespace()
            || resources.kind != EvaluationExecutionKind::Program
            || !resources.objects.is_empty()
        {
            return Err(OjExecutorError::BindingInvalid);
        }
        request
            .validate()
            .map_err(|_| OjExecutorError::BindingInvalid)?;
        if request.run_id != resources.run_id.as_uuid()
            || request.step_run_id != resources.step_run_id.as_uuid()
            || request.attempt_id != resources.task_run_id.as_uuid()
        {
            return Err(OjExecutorError::IdentityConflict);
        }
        let targets = recovery_cleanup_plan(resources.namespace.as_str(), request);
        let identity = request_identity(
            resources.namespace.as_str(),
            &attempt_job_name(request.attempt_id),
            request,
        )?;
        Ok(self
            .api
            .capture_intent_object_refs(&identity, &targets)
            .await?)
    }

    /// Deletes and verifies a recovered attempt from persisted references.
    ///
    /// # Errors
    ///
    /// Returns an error when the recovered identity is invalid or Kubernetes cannot delete or
    /// verify one of the owned objects.
    pub async fn cleanup_recovery(
        &self,
        resources: &EvaluationExecutionResources,
    ) -> Result<ExecutionCleanupStatus, OjExecutorError> {
        if resources.namespace != self.api.runner_namespace()
            || resources.kind != EvaluationExecutionKind::Program
        {
            return Err(OjExecutorError::BindingInvalid);
        }
        if resources.objects.is_empty() {
            let request: OjExecutionRequest = serde_json::from_value(resources.request.clone())
                .map_err(|_| OjExecutorError::BindingInvalid)?;
            request
                .validate()
                .map_err(|_| OjExecutorError::BindingInvalid)?;
            if request.run_id != resources.run_id.as_uuid()
                || request.step_run_id != resources.step_run_id.as_uuid()
                || request.attempt_id != resources.task_run_id.as_uuid()
            {
                return Err(OjExecutorError::IdentityConflict);
            }
            let targets = recovery_cleanup_plan(resources.namespace.as_str(), &request);
            let identity = request_identity(
                resources.namespace.as_str(),
                &attempt_job_name(request.attempt_id),
                &request,
            )?;
            return Ok(self.api.cleanup_intent(&identity, &targets).await?);
        }
        let identity = recovery_identity(resources)?;
        Ok(self
            .api
            .cleanup_recovery(&identity, &resources.objects)
            .await?)
    }

    /// Cancels an attempt by invoking the same exact cleanup boundary.
    ///
    /// # Errors
    ///
    /// Returns a stable Kubernetes or ownership error; cancellation becomes terminal only after
    /// every exact attempt resource is absent.
    pub async fn cancel(
        &self,
        resources: &OjJobResources,
    ) -> Result<OjCancellationObservation, OjExecutorError> {
        self.cleanup(resources)
            .await
            .map(|status| cancellation_observation(status.is_confirmed()))
    }

    /// Deletes and verifies absence of only the attempt-owned Job, policy, and command object.
    ///
    /// # Errors
    ///
    /// Returns a stable Kubernetes or ownership error; `Ok(false)` means deletion is still pending.
    pub async fn cleanup(
        &self,
        resources: &OjJobResources,
    ) -> Result<ExecutionCleanupStatus, OjExecutorError> {
        let objects = oj_objects(resources);
        let cleanup_plan = cleanup_targets(resources);
        Ok(self
            .api
            .cleanup(
                resources.namespace(),
                resources.name(),
                &objects,
                &cleanup_plan,
            )
            .await?)
    }
}

fn api_configuration(configuration: OjExecutorConfiguration) -> KubernetesApiConfiguration {
    KubernetesApiConfiguration {
        kubernetes_api_server: configuration.kubernetes_api_server,
        kubernetes_bearer_token_file: configuration.kubernetes_bearer_token_file,
        kubernetes_ca_file: configuration.kubernetes_ca_file,
        runner_namespace: configuration.runner_namespace,
        request_timeout_milliseconds: configuration.request_timeout_milliseconds,
    }
}

fn request_identity(
    namespace: &str,
    job_name: &str,
    request: &OjExecutionRequest,
) -> Result<KubernetesJobIdentity, OjExecutorError> {
    let request_sha256 = request
        .request_sha256()
        .map_err(|_| OjExecutorError::IdentityConflict)?
        .to_string();
    Ok(KubernetesJobIdentity {
        namespace: namespace.to_owned(),
        job_name: job_name.to_owned(),
        main_container: MAIN_CONTAINER,
        default_deny_policy: RUNNER_DEFAULT_DENY_POLICY,
        deadline_diagnostic_code: DEADLINE_DIAGNOSTIC_CODE,
        failed_diagnostic_code: FAILED_DIAGNOSTIC_CODE,
        oom_diagnostic_code: OOM_DIAGNOSTIC_CODE,
        stable_diagnostic_prefix: STABLE_DIAGNOSTIC_PREFIX,
        ownership: KubernetesOwnership {
            run_id: request.run_id,
            step_run_id: request.step_run_id,
            attempt_id: request.attempt_id,
            request_sha256,
        },
        trace_id: request.trace_id.clone(),
    })
}

fn recovery_identity(
    resources: &EvaluationExecutionResources,
) -> Result<KubernetesJobIdentity, OjExecutorError> {
    let admission = resources
        .admission
        .as_ref()
        .ok_or(OjExecutorError::BindingInvalid)?;
    Ok(KubernetesJobIdentity {
        namespace: resources.namespace.clone(),
        job_name: resources
            .objects
            .iter()
            .find(|object| object.resource == "jobs")
            .map_or_else(
                || {
                    format!(
                        "lw-oj-{}",
                        &resources.task_run_id.as_uuid().simple().to_string()[..20]
                    )
                },
                |object| object.name.clone(),
            ),
        main_container: MAIN_CONTAINER,
        default_deny_policy: RUNNER_DEFAULT_DENY_POLICY,
        deadline_diagnostic_code: DEADLINE_DIAGNOSTIC_CODE,
        failed_diagnostic_code: FAILED_DIAGNOSTIC_CODE,
        oom_diagnostic_code: OOM_DIAGNOSTIC_CODE,
        stable_diagnostic_prefix: STABLE_DIAGNOSTIC_PREFIX,
        ownership: KubernetesOwnership {
            run_id: resources.run_id.as_uuid(),
            step_run_id: resources.step_run_id.as_uuid(),
            attempt_id: resources.task_run_id.as_uuid(),
            // Recovery ownership verifies labels and UID only; the persisted
            // annotation is checked against the rendered bundle during cleanup.
            request_sha256: String::new(),
        },
        trace_id: admission.trace_id.clone(),
    })
}

fn job_bundle(
    namespace: &str,
    resources: &OjJobResources,
    request: &OjExecutionRequest,
) -> Result<KubernetesJobBundle, OjExecutorError> {
    Ok(KubernetesJobBundle {
        identity: request_identity(namespace, resources.name(), request)?,
        objects: oj_objects(resources),
        cleanup_plan: cleanup_targets(resources),
    })
}

fn oj_objects(resources: &OjJobResources) -> Vec<KubernetesObject> {
    vec![
        KubernetesObject {
            api_version: "networking.k8s.io/v1",
            plural: "networkpolicies",
            name: resources.name().to_owned(),
            document: resources.network_policy.clone(),
        },
        KubernetesObject {
            api_version: "v1",
            plural: "configmaps",
            name: resources.name().to_owned(),
            document: resources.config_map.clone(),
        },
        KubernetesObject {
            api_version: "v1",
            plural: "secrets",
            name: resources.materializer_secret_name().to_owned(),
            document: resources.materializer_secret.clone(),
        },
        KubernetesObject {
            api_version: "batch/v1",
            plural: "jobs",
            name: resources.name().to_owned(),
            document: resources.job.clone(),
        },
    ]
}

fn cleanup_targets(resources: &OjJobResources) -> Vec<KubernetesCleanupTarget> {
    resources
        .cleanup_plan()
        .into_iter()
        .map(|target| KubernetesCleanupTarget {
            namespace: target.namespace,
            resource: target.resource,
            name: target.name,
            propagation_policy: target.propagation_policy,
        })
        .collect()
}

const fn cancellation_observation(cleanup_complete: bool) -> OjCancellationObservation {
    if cleanup_complete {
        OjCancellationObservation::Cancelled
    } else {
        OjCancellationObservation::CleanupPending
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OjCancellationObservation {
    CleanupPending,
    Cancelled,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OjJobObservation {
    Missing,
    Running,
    Completed {
        receipt: OjEvidenceReceipt,
        observation: ExecutionObservation,
    },
    Failed {
        diagnostic_code: String,
        observation: ExecutionObservation,
    },
}

fn attempt_job_name(attempt_id: uuid::Uuid) -> String {
    format!("lw-oj-{}", &attempt_id.simple().to_string()[..20])
}

fn recovery_cleanup_plan(
    namespace: &str,
    request: &OjExecutionRequest,
) -> Vec<KubernetesCleanupTarget> {
    let name = attempt_job_name(request.attempt_id);
    let materializer = format!("{name}-materializer");
    [
        ("jobs", name.clone()),
        ("networkpolicies", name.clone()),
        ("configmaps", name),
        ("secrets", materializer),
    ]
    .into_iter()
    .map(|(resource, name)| KubernetesCleanupTarget {
        namespace: namespace.to_owned(),
        resource: resource.to_owned(),
        name,
        propagation_policy: "Foreground".to_owned(),
    })
    .collect()
}

#[derive(Debug, Error)]
pub enum OjExecutorError {
    #[error("OJ executor configuration is unavailable during {operation}: {source}")]
    ConfigurationUnavailable {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("OJ executor configuration is invalid")]
    ConfigurationInvalid,
    #[error("OJ Job binding is invalid")]
    BindingInvalid,
    #[error("OJ Kubernetes API is unavailable")]
    KubernetesUnavailable,
    #[error("OJ Kubernetes API rejected the operation")]
    KubernetesRejected,
    #[error("OJ runner namespace default-deny isolation is unavailable")]
    NetworkIsolationUnavailable,
    #[error("OJ attempt identity conflicts with an existing resource")]
    IdentityConflict,
    #[error("OJ Job cleanup is pending")]
    CleanupPending,
    #[error("OJ Job observation is invalid")]
    ObservationInvalid,
    #[error("OJ evidence receipt is invalid")]
    ReceiptInvalid,
    #[error(transparent)]
    Job(#[from] OjJobError),
}

impl From<KubernetesJobError> for OjExecutorError {
    fn from(error: KubernetesJobError) -> Self {
        match error {
            KubernetesJobError::ConfigurationUnavailable { operation, source } => {
                Self::ConfigurationUnavailable { operation, source }
            }
            KubernetesJobError::ConfigurationInvalid => Self::ConfigurationInvalid,
            KubernetesJobError::BindingInvalid => Self::BindingInvalid,
            KubernetesJobError::KubernetesUnavailable => Self::KubernetesUnavailable,
            KubernetesJobError::KubernetesRejected => Self::KubernetesRejected,
            KubernetesJobError::NetworkIsolationUnavailable => Self::NetworkIsolationUnavailable,
            KubernetesJobError::IdentityConflict => Self::IdentityConflict,
            KubernetesJobError::CleanupPending => Self::CleanupPending,
            KubernetesJobError::ObservationInvalid => Self::ObservationInvalid,
            KubernetesJobError::ReceiptInvalid => Self::ReceiptInvalid,
        }
    }
}

impl OjExecutorError {
    #[must_use]
    pub const fn error_kind(&self) -> &'static str {
        match self {
            Self::ConfigurationUnavailable { .. } => "configuration_unavailable",
            Self::ConfigurationInvalid => "configuration_invalid",
            Self::BindingInvalid => "binding_invalid",
            Self::KubernetesUnavailable => "kubernetes_unavailable",
            Self::KubernetesRejected => "kubernetes_rejected",
            Self::NetworkIsolationUnavailable => "network_isolation_unavailable",
            Self::IdentityConflict => "identity_conflict",
            Self::CleanupPending => "cleanup_pending",
            Self::ObservationInvalid => "observation_invalid",
            Self::ReceiptInvalid => "receipt_invalid",
            Self::Job(_) => "job",
        }
    }

    #[must_use]
    pub const fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::ConfigurationUnavailable { .. } => "LW_OJ_EXECUTOR_CONFIG_UNAVAILABLE",
            Self::ConfigurationInvalid => "LW_OJ_EXECUTOR_CONFIG_INVALID",
            Self::BindingInvalid => "LW_OJ_JOB_BINDING_INVALID",
            Self::KubernetesUnavailable => "LW_OJ_KUBERNETES_UNAVAILABLE",
            Self::KubernetesRejected => "LW_OJ_KUBERNETES_REJECTED",
            Self::NetworkIsolationUnavailable => "LW_OJ_NETWORK_ISOLATION_UNAVAILABLE",
            Self::IdentityConflict => "LW_OJ_ATTEMPT_IDENTITY_CONFLICT",
            Self::CleanupPending => "LW_OJ_CLEANUP_PENDING",
            Self::ObservationInvalid => "LW_OJ_OBSERVATION_INVALID",
            Self::ReceiptInvalid => "LW_OJ_RECEIPT_INVALID",
            Self::Job(error) => error.diagnostic_code(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        fs,
        path::PathBuf,
        sync::{Arc, Mutex},
    };

    use contracts::execution::ExecutionObjectRef;
    use contracts::{EvaluationRunId, EvaluationStepRunId, TaskRunId};
    use persistence_sqlx::Sha256Digest;
    use reqwest::{Client, StatusCode, Url};
    use serde_json::{Value, json};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        task::JoinHandle,
    };
    use uuid::Uuid;

    use super::{
        OjCancellationObservation, OjExecutorError, OjJobObservation, OjKubernetesExecutor,
        attempt_job_name, cancellation_observation, recovery_cleanup_plan,
    };
    use crate::kubernetes_job::{
        KubernetesApiClient, KubernetesApiConfiguration, KubernetesCleanupTarget,
        KubernetesDeletePreconditions, KubernetesJobError, KubernetesJobIdentity,
        KubernetesOwnership, api_version, classify_delete_status, delete_options,
        delete_preconditions, execution_timing, failed_container_diagnostic, read_bound_file,
        verify_cleanup_owned, verify_runner_default_deny,
    };
    use crate::{
        control_plane::{EvaluationExecutionKind, EvaluationExecutionResources},
        execution::ExecutionTiming,
        oj::{
            OJ_EXECUTION_SCHEMA_VERSION, OjExecutionLimits, OjExecutionPhase, OjExecutionRequest,
            OjFileBinding,
        },
    };

    const TEST_NAMESPACE: &str = "labweaver-evaluation-runs";

    fn test_identity() -> KubernetesJobIdentity {
        KubernetesJobIdentity {
            namespace: TEST_NAMESPACE.to_owned(),
            job_name: "lw-oj-attempt".to_owned(),
            main_container: "program-runner",
            default_deny_policy: "oj-runner-default-deny",
            deadline_diagnostic_code: "LW_OJ_JOB_DEADLINE_EXCEEDED",
            failed_diagnostic_code: "LW_OJ_JOB_FAILED",
            oom_diagnostic_code: "LW_OJ_MEMORY_LIMIT",
            stable_diagnostic_prefix: "LW_OJ_",
            ownership: KubernetesOwnership {
                run_id: Uuid::nil(),
                step_run_id: Uuid::nil(),
                attempt_id: Uuid::nil(),
                request_sha256: "request".to_owned(),
            },
            trace_id: "trace".to_owned(),
        }
    }

    #[test]
    fn execution_timing_handles_second_precision_without_inventing_duration()
    -> Result<(), Box<dyn std::error::Error>> {
        let equal = json!({
            "state": {"terminated": {
                "startedAt": "2026-09-14T06:26:32Z",
                "finishedAt": "2026-09-14T06:26:32Z"
            }}
        });
        assert_eq!(execution_timing(&equal)?, ExecutionTiming::unknown());

        let reversed = json!({
            "state": {"terminated": {
                "startedAt": "2026-09-14T06:26:33Z",
                "finishedAt": "2026-09-14T06:26:32Z"
            }}
        });
        assert!(matches!(
            execution_timing(&reversed),
            Err(KubernetesJobError::ObservationInvalid)
        ));

        let positive = json!({
            "state": {"terminated": {
                "startedAt": "2026-09-14T06:26:32Z",
                "finishedAt": "2026-09-14T06:26:33Z"
            }}
        });
        let timing = execution_timing(&positive)?;
        assert!(timing.started_at.is_some());
        assert!(timing.terminated_at.is_some());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn read_bound_file_accepts_a_kubernetes_projected_symlink()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir()?;
        let revision = directory.path().join("..2026_09_09_00_00_00.000000001");
        fs::create_dir(&revision)?;
        fs::write(revision.join("token"), b"projected-token")?;
        symlink(&revision, directory.path().join("..data"))?;
        let projected = directory.path().join("token");
        symlink("..data/token", &projected)?;

        assert_eq!(read_bound_file(&projected)?, b"projected-token");
        Ok(())
    }

    #[test]
    fn read_bound_file_rejects_empty_and_oversized_files() -> Result<(), Box<dyn std::error::Error>>
    {
        let directory = tempfile::tempdir()?;
        let empty = directory.path().join("empty");
        fs::write(&empty, [])?;
        assert!(matches!(
            read_bound_file(&empty),
            Err(KubernetesJobError::ConfigurationInvalid)
        ));

        let oversized = directory.path().join("oversized");
        fs::write(
            &oversized,
            vec![b'x'; usize::try_from(crate::kubernetes_job::MAX_BOUND_FILE_BYTES + 1)?],
        )?;
        assert!(matches!(
            read_bound_file(&oversized),
            Err(KubernetesJobError::ConfigurationInvalid)
        ));
        Ok(())
    }

    #[test]
    fn read_bound_file_preserves_unavailable_source_errors()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let missing = directory.path().join("missing");
        assert!(matches!(
            read_bound_file(&missing),
            Err(KubernetesJobError::ConfigurationUnavailable { .. })
        ));
        Ok(())
    }

    struct FakeExecutor {
        executor: OjKubernetesExecutor,
        objects: Arc<Mutex<BTreeMap<String, serde_json::Value>>>,
        delete_conflict: Arc<Mutex<Option<FakeDeleteConflict>>>,
        server: JoinHandle<()>,
        _token_file: tempfile::NamedTempFile,
    }

    #[derive(Clone, Debug)]
    struct FakeDeleteConflict {
        path: String,
        replacement_uid: Option<String>,
    }

    impl Drop for FakeExecutor {
        fn drop(&mut self) {
            self.server.abort();
        }
    }

    fn compile_request()
    -> Result<(OjExecutionRequest, EvaluationExecutionResources), Box<dyn std::error::Error>> {
        let run_id = EvaluationRunId::new();
        let step_run_id = EvaluationStepRunId::new();
        let task_run_id = TaskRunId::new();
        let request = OjExecutionRequest {
            schema_version: OJ_EXECUTION_SCHEMA_VERSION.to_owned(),
            run_id: run_id.as_uuid(),
            step_run_id: step_run_id.as_uuid(),
            attempt_id: task_run_id.as_uuid(),
            trace_id: "trace-oj-recovery-test".to_owned(),
            toolchain_profile: "cpp17-approved-v1".to_owned(),
            toolchain_image_digest: format!("sha256:{}", "a".repeat(64)),
            submission_identity: Sha256Digest::of_bytes(b"submission"),
            evaluator_identity: Some(Sha256Digest::of_bytes(b"evaluator")),
            source: OjFileBinding {
                path: "src/main.cpp".to_owned(),
                sha256: Sha256Digest::of_bytes(b"source"),
                size_bytes: 6,
            },
            phase: OjExecutionPhase::Compile,
            checker: None,
            cases: Vec::new(),
            score_max_points: 0,
            limits: OjExecutionLimits {
                compile_wall_milliseconds: 10_000,
                run_wall_milliseconds: 1_000,
                cpu_milliseconds: 500,
                memory_bytes: 32 * 1024 * 1024,
                output_bytes: 1_024,
            },
        };
        request.validate()?;
        let admission = contracts::execution::TaskExecutionBinding {
            task_run_id,
            execution_generation: 1,
            resource_request_id: contracts::ResourceRequestId::new(),
            capacity_claim_id: contracts::CapacityClaimId::new(),
            lease_id: contracts::LeaseId::new(),
            claim_revision: contracts::Revision::new(1)?,
            lease_revision: contracts::Revision::new(1)?,
            project_id: contracts::ProjectId::new(),
            provider_binding: "kubernetes-job".to_owned(),
            namespace: TEST_NAMESPACE.to_owned(),
            workload_name: attempt_job_name(request.attempt_id),
            trace_id: request.trace_id.clone(),
        };
        admission.validate()?;
        let intent = EvaluationExecutionResources {
            schema_version: crate::EVALUATION_EXECUTION_RESOURCES_SCHEMA_VERSION.to_owned(),
            run_id,
            step_run_id,
            task_run_id,
            namespace: TEST_NAMESPACE.to_owned(),
            kind: EvaluationExecutionKind::Program,
            admission: Some(admission),
            request: serde_json::to_value(&request)?,
            objects: Vec::new(),
        };
        intent.validate_for(run_id, step_run_id, task_run_id)?;
        Ok((request, intent))
    }

    fn object_document(
        request: &OjExecutionRequest,
        name: &str,
        uid: &str,
        resource: &str,
    ) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
        Ok(json!({
            "apiVersion": match resource {
                "jobs" => "batch/v1",
                "networkpolicies" => "networking.k8s.io/v1",
                "configmaps" | "secrets" => "v1",
                _ => return Err("unsupported fake Kubernetes resource".into()),
            },
            "kind": "Job",
            "metadata": {
                "name": name,
                "namespace": TEST_NAMESPACE,
                "uid": uid,
                "resourceVersion": format!("rv-{uid}"),
                "labels": {
                    "labweaver.io/managed-by": "evaluation-service",
                    "labweaver.io/run-id": request.run_id.to_string(),
                    "labweaver.io/step-run-id": request.step_run_id.to_string(),
                    "labweaver.io/attempt-id": request.attempt_id.to_string(),
                },
                "annotations": {
                    "labweaver.io/request-sha256": request.request_sha256()?.to_string(),
                },
            },
        }))
    }

    fn resource_path(api: &str, plural: &str, name: &str) -> String {
        let prefix = if api == "v1" {
            "/api/v1".to_owned()
        } else {
            format!("/apis/{api}")
        };
        format!("{prefix}/namespaces/{TEST_NAMESPACE}/{plural}/{name}")
    }

    async fn fake_connection(
        mut stream: TcpStream,
        objects: Arc<Mutex<BTreeMap<String, serde_json::Value>>>,
        delete_conflict: Arc<Mutex<Option<FakeDeleteConflict>>>,
    ) -> std::io::Result<()> {
        let mut request = Vec::new();
        let mut chunk = [0_u8; 4 * 1024];
        loop {
            let read = stream.read(&mut chunk).await?;
            if read == 0 {
                return Ok(());
            }
            request.extend_from_slice(&chunk[..read]);
            let Some(header_end) = request
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|position| position + 4)
            else {
                if request.len() > 64 * 1024 {
                    return fake_response(&mut stream, 413, &json!({})).await;
                }
                continue;
            };
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                })
                .flatten()
                .unwrap_or(0);
            let request_length = header_end.saturating_add(content_length);
            if request_length > 64 * 1024 {
                return fake_response(&mut stream, 413, &json!({})).await;
            }
            if request.len() >= request_length {
                break;
            }
        }
        let Some(header_end) = request
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|position| position + 4)
        else {
            return fake_response(&mut stream, 400, &json!({})).await;
        };
        let Some(line_end) = request[..header_end]
            .windows(2)
            .position(|window| window == b"\r\n")
        else {
            return fake_response(&mut stream, 400, &json!({})).await;
        };
        let line = String::from_utf8_lossy(&request[..line_end]);
        let mut fields = line.split_ascii_whitespace();
        let Some(method) = fields.next() else {
            return fake_response(&mut stream, 400, &json!({})).await;
        };
        let Some(path) = fields.next() else {
            return fake_response(&mut stream, 400, &json!({})).await;
        };
        let (status, body) = match method {
            "GET" => {
                let current = objects
                    .lock()
                    .map_err(|_| std::io::Error::other("fake object state poisoned"))?
                    .get(path)
                    .cloned();
                match current {
                    Some(value) => (200, value),
                    None => (404, json!({})),
                }
            }
            "DELETE" => {
                let body = request.get(header_end..).unwrap_or_default();
                let options = serde_json::from_slice::<serde_json::Value>(body).ok();
                let conflict = {
                    let mut configured = delete_conflict.lock().map_err(|_| {
                        std::io::Error::other("fake delete conflict state poisoned")
                    })?;
                    configured
                        .as_ref()
                        .is_some_and(|conflict| conflict.path == path)
                        .then(|| configured.take())
                        .flatten()
                };
                if let Some(conflict) = conflict {
                    let mut state = objects
                        .lock()
                        .map_err(|_| std::io::Error::other("fake object state poisoned"))?;
                    if let Some(current) = state.get_mut(path) {
                        current["metadata"]["resourceVersion"] = json!("rv-conflict");
                        if let Some(replacement_uid) = conflict.replacement_uid {
                            current["metadata"]["uid"] = json!(replacement_uid);
                        }
                        (409, json!({}))
                    } else {
                        (404, json!({}))
                    }
                } else {
                    let expected_uid = options
                        .as_ref()
                        .and_then(|options| options.pointer("/preconditions/uid"))
                        .and_then(serde_json::Value::as_str);
                    let expected_resource_version = options
                        .as_ref()
                        .and_then(|options| options.pointer("/preconditions/resourceVersion"))
                        .and_then(serde_json::Value::as_str);
                    {
                        let mut state = objects
                            .lock()
                            .map_err(|_| std::io::Error::other("fake object state poisoned"))?;
                        match state.get(path) {
                            None => (404, json!({})),
                            Some(current) => {
                                let matches =
                                    current.pointer("/metadata/uid").and_then(Value::as_str)
                                        == expected_uid
                                        && current
                                            .pointer("/metadata/resourceVersion")
                                            .and_then(Value::as_str)
                                            == expected_resource_version;
                                if matches {
                                    state.remove(path);
                                    (200, json!({}))
                                } else {
                                    (409, json!({}))
                                }
                            }
                        }
                    }
                }
            }
            _ => (405, json!({})),
        };
        fake_response(&mut stream, status, &body).await
    }

    async fn fake_response(
        stream: &mut TcpStream,
        status: u16,
        body: &serde_json::Value,
    ) -> std::io::Result<()> {
        let body =
            serde_json::to_vec(body).map_err(|error| std::io::Error::other(error.to_string()))?;
        let header = format!(
            "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(header.as_bytes()).await?;
        stream.write_all(&body).await
    }

    async fn fake_executor(
        objects: BTreeMap<String, serde_json::Value>,
    ) -> Result<FakeExecutor, Box<dyn std::error::Error>> {
        let token_file = tempfile::NamedTempFile::new()?;
        fs::write(token_file.path(), b"fake-kubernetes-token")?;
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let address = listener.local_addr()?;
        let objects = Arc::new(Mutex::new(objects));
        let delete_conflict = Arc::new(Mutex::new(None));
        let server_objects = Arc::clone(&objects);
        let server_delete_conflict = Arc::clone(&delete_conflict);
        let server = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let connection_objects = Arc::clone(&server_objects);
                let connection_delete_conflict = Arc::clone(&server_delete_conflict);
                tokio::spawn(async move {
                    let _ = fake_connection(stream, connection_objects, connection_delete_conflict)
                        .await;
                });
            }
        });
        let client = Client::builder().no_proxy().build()?;
        let api = KubernetesApiClient::for_test(
            KubernetesApiConfiguration {
                kubernetes_api_server: Url::parse(&format!("http://{address}/"))?,
                kubernetes_bearer_token_file: token_file.path().to_owned(),
                kubernetes_ca_file: PathBuf::from("unused-ca"),
                runner_namespace: TEST_NAMESPACE.to_owned(),
                request_timeout_milliseconds: 2_000,
            },
            client,
            "labweaver-oj-executor",
            "program.oj",
            "LW_OJ_",
        );
        let executor = OjKubernetesExecutor { api };
        Ok(FakeExecutor {
            executor,
            objects,
            delete_conflict,
            server,
            _token_file: token_file,
        })
    }

    fn hydrated(
        intent: &EvaluationExecutionResources,
        refs: Vec<ExecutionObjectRef>,
    ) -> EvaluationExecutionResources {
        EvaluationExecutionResources {
            objects: refs,
            ..intent.clone()
        }
    }

    fn execution_ref(resource: &str, name: &str, uid: &str) -> ExecutionObjectRef {
        let api_version = match resource {
            "jobs" => "batch/v1",
            "networkpolicies" => "networking.k8s.io/v1",
            _ => "v1",
        };
        ExecutionObjectRef {
            api_version: api_version.to_owned(),
            resource: resource.to_owned(),
            name: name.to_owned(),
            uid: uid.to_owned(),
        }
    }

    #[tokio::test]
    async fn crash_after_create_before_uid_checkpoint_is_recovered_and_cleaned()
    -> Result<(), Box<dyn std::error::Error>> {
        let (request, intent) = compile_request()?;
        let job_name = attempt_job_name(request.attempt_id);
        let materializer_name = format!("{job_name}-materializer");
        let mut objects = BTreeMap::new();
        let network_policy_path =
            resource_path("networking.k8s.io/v1", "networkpolicies", &job_name);
        objects.insert(
            network_policy_path,
            object_document(&request, &job_name, "uid-network-policy", "networkpolicies")?,
        );
        let config_map_path = resource_path("v1", "configmaps", &job_name);
        objects.insert(
            config_map_path,
            object_document(&request, &job_name, "uid-config-map", "configmaps")?,
        );
        let materializer_path = resource_path("v1", "secrets", &materializer_name);
        objects.insert(
            materializer_path,
            object_document(&request, &materializer_name, "uid-materializer", "secrets")?,
        );
        let fake = fake_executor(objects).await?;

        // The persisted intent has no UIDs, while the worker did create a
        // partial bundle before it crashed. Recovery discovers only the
        // deterministic names and promotes the verified UIDs.
        let refs = fake
            .executor
            .capture_intent_object_refs(&intent, &request)
            .await?
            .ok_or("partial bundle must be discoverable")?;
        assert_eq!(refs.len(), 3);
        assert_eq!(refs[0].resource, "configmaps");
        assert_eq!(refs[1].resource, "networkpolicies");
        assert_eq!(refs[2].resource, "secrets");
        let recovered = hydrated(&intent, refs);
        recovered.validate_for(intent.run_id, intent.step_run_id, intent.task_run_id)?;

        // No Job means a terminal missing-job outcome; recovery must not
        // recreate the Job or poll indefinitely.
        assert!(matches!(
            fake.executor.observe_recovery(&recovered, &request).await?,
            OjJobObservation::Missing
        ));
        assert!(
            fake.executor
                .cleanup_recovery(&recovered)
                .await?
                .is_confirmed()
        );
        assert!(
            fake.objects
                .lock()
                .map_err(|_| "fake object state poisoned")?
                .is_empty()
        );
        Ok(())
    }

    #[tokio::test]
    async fn cancel_empty_intent_cleans_partial_bundle_by_deterministic_identity()
    -> Result<(), Box<dyn std::error::Error>> {
        let (request, intent) = compile_request()?;
        let mut objects = BTreeMap::new();
        for (index, target) in recovery_cleanup_plan(TEST_NAMESPACE, &request)
            .into_iter()
            .enumerate()
        {
            let api = api_version(&target)?;
            let path = resource_path(api, &target.resource, &target.name);
            let uid = format!("uid-{index}");
            objects.insert(
                path,
                object_document(&request, &target.name, &uid, &target.resource)?,
            );
        }
        let fake = fake_executor(objects).await?;

        // Cancellation can arrive before UIDs are checkpointed. The empty
        // intent still carries the request identity needed to remove every
        // exact attempt object and returns a terminal cancellation only after
        // the second GET confirms absence.
        let cleanup_complete = fake.executor.cleanup_recovery(&intent).await?;
        assert!(cleanup_complete.is_confirmed());
        assert_eq!(
            cancellation_observation(cleanup_complete.is_confirmed()),
            OjCancellationObservation::Cancelled
        );
        // A replay is finite and idempotent; it never starts a new Job.
        assert!(
            fake.executor
                .cleanup_recovery(&intent)
                .await?
                .is_confirmed()
        );
        assert!(
            fake.objects
                .lock()
                .map_err(|_| "fake object state poisoned")?
                .is_empty()
        );
        Ok(())
    }

    #[tokio::test]
    async fn cleanup_retries_same_uid_resource_version_conflict()
    -> Result<(), Box<dyn std::error::Error>> {
        let (request, intent) = compile_request()?;
        let job_name = attempt_job_name(request.attempt_id);
        let path = resource_path("batch/v1", "jobs", &job_name);
        let mut objects = BTreeMap::new();
        objects.insert(
            path.clone(),
            object_document(&request, &job_name, "uid-job", "jobs")?,
        );
        let fake = fake_executor(objects).await?;
        *fake
            .delete_conflict
            .lock()
            .map_err(|_| "fake delete conflict state poisoned")? = Some(FakeDeleteConflict {
            path,
            replacement_uid: None,
        });
        let recovered = hydrated(&intent, vec![execution_ref("jobs", &job_name, "uid-job")]);

        assert!(
            fake.executor
                .cleanup_recovery(&recovered)
                .await?
                .is_confirmed()
        );
        assert!(
            fake.objects
                .lock()
                .map_err(|_| "fake object state poisoned")?
                .is_empty()
        );
        Ok(())
    }

    #[tokio::test]
    async fn cleanup_rejects_uid_replacement_after_resource_version_conflict()
    -> Result<(), Box<dyn std::error::Error>> {
        let (request, intent) = compile_request()?;
        let job_name = attempt_job_name(request.attempt_id);
        let path = resource_path("batch/v1", "jobs", &job_name);
        let mut objects = BTreeMap::new();
        objects.insert(
            path.clone(),
            object_document(&request, &job_name, "uid-job", "jobs")?,
        );
        let fake = fake_executor(objects).await?;
        *fake
            .delete_conflict
            .lock()
            .map_err(|_| "fake delete conflict state poisoned")? = Some(FakeDeleteConflict {
            path: path.clone(),
            replacement_uid: Some("uid-replacement".to_owned()),
        });
        let recovered = hydrated(&intent, vec![execution_ref("jobs", &job_name, "uid-job")]);

        assert!(matches!(
            fake.executor.cleanup_recovery(&recovered).await,
            Err(OjExecutorError::IdentityConflict)
        ));
        let objects = fake
            .objects
            .lock()
            .map_err(|_| "fake object state poisoned")?;
        let current = objects.get(&path).ok_or("replacement object disappeared")?;
        assert_eq!(
            current.pointer("/metadata/uid").and_then(Value::as_str),
            Some("uid-replacement")
        );
        Ok(())
    }

    fn resource() -> serde_json::Value {
        json!({
            "apiVersion":"batch/v1",
            "kind":"Job",
            "metadata":{
                "name":"lw-oj-attempt",
                "namespace":"labweaver-evaluation-runs",
                "uid":"f7780f4c-e8db-4f35-82d1-bc56b5652830",
                "resourceVersion":"1042",
                "labels":{
                    "labweaver.io/managed-by":"evaluation-service",
                    "labweaver.io/run-id":"run",
                    "labweaver.io/step-run-id":"step",
                    "labweaver.io/attempt-id":"attempt",
                },
                "annotations":{"labweaver.io/request-sha256":"request"},
            },
        })
    }

    #[test]
    fn cleanup_ownership_rejects_request_or_namespace_drift() {
        let expected = resource();
        assert!(verify_cleanup_owned(&resource(), &expected).is_ok());

        let mut drifted = resource();
        drifted["metadata"]["annotations"]["labweaver.io/request-sha256"] = json!("different");
        assert!(matches!(
            verify_cleanup_owned(&drifted, &expected),
            Err(KubernetesJobError::IdentityConflict)
        ));

        let mut drifted = resource();
        drifted["metadata"]["namespace"] = json!("another-namespace");
        assert!(matches!(
            verify_cleanup_owned(&drifted, &expected),
            Err(KubernetesJobError::IdentityConflict)
        ));
    }

    #[test]
    fn cleanup_delete_is_bound_to_the_observed_resource_identity()
    -> Result<(), Box<dyn std::error::Error>> {
        let current = resource();
        let preconditions = delete_preconditions(&current)?;
        assert_eq!(
            preconditions,
            KubernetesDeletePreconditions {
                uid: "f7780f4c-e8db-4f35-82d1-bc56b5652830".to_owned(),
                resource_version: "1042".to_owned(),
            }
        );
        let target = KubernetesCleanupTarget {
            namespace: "labweaver-evaluation-runs".to_owned(),
            resource: "jobs".to_owned(),
            name: "lw-oj-attempt".to_owned(),
            propagation_policy: "Foreground".to_owned(),
        };
        let options = delete_options(&target, &preconditions);
        assert_eq!(
            options
                .pointer("/preconditions/uid")
                .and_then(serde_json::Value::as_str),
            Some("f7780f4c-e8db-4f35-82d1-bc56b5652830")
        );
        assert_eq!(
            options
                .pointer("/preconditions/resourceVersion")
                .and_then(serde_json::Value::as_str),
            Some("1042")
        );
        assert!(matches!(
            classify_delete_status(StatusCode::CONFLICT),
            Err(KubernetesJobError::IdentityConflict)
        ));
        Ok(())
    }

    #[test]
    fn runner_default_deny_must_be_namespace_wide_and_complete() {
        let policy = json!({
            "apiVersion":"networking.k8s.io/v1",
            "kind":"NetworkPolicy",
            "metadata":{
                "name":"oj-runner-default-deny",
                "namespace":"labweaver-evaluation-runs",
            },
            "spec":{
                "podSelector":{},
                "policyTypes":["Ingress","Egress"],
                "ingress":[],
                "egress":[],
            },
        });
        assert!(
            verify_runner_default_deny(
                &policy,
                "labweaver-evaluation-runs",
                "oj-runner-default-deny"
            )
            .is_ok()
        );

        let normalized = json!({
            "apiVersion":"networking.k8s.io/v1",
            "kind":"NetworkPolicy",
            "metadata":{
                "name":"oj-runner-default-deny",
                "namespace":"labweaver-evaluation-runs",
            },
            "spec":{
                "podSelector":{},
                "policyTypes":["Ingress","Egress"],
            },
        });
        assert!(
            verify_runner_default_deny(
                &normalized,
                "labweaver-evaluation-runs",
                "oj-runner-default-deny"
            )
            .is_ok()
        );

        let mut allows_egress = policy.clone();
        allows_egress["spec"]["egress"] = json!([{}]);
        assert!(matches!(
            verify_runner_default_deny(
                &allows_egress,
                "labweaver-evaluation-runs",
                "oj-runner-default-deny"
            ),
            Err(KubernetesJobError::NetworkIsolationUnavailable)
        ));

        let mut wrong_rule_type = normalized;
        wrong_rule_type["spec"]["egress"] = json!({});
        assert!(matches!(
            verify_runner_default_deny(
                &wrong_rule_type,
                "labweaver-evaluation-runs",
                "oj-runner-default-deny"
            ),
            Err(KubernetesJobError::NetworkIsolationUnavailable)
        ));

        let mut selected_only = policy;
        selected_only["spec"]["podSelector"] =
            json!({"matchLabels":{"labweaver.io/attempt-id":"attempt"}});
        assert!(matches!(
            verify_runner_default_deny(
                &selected_only,
                "labweaver-evaluation-runs",
                "oj-runner-default-deny"
            ),
            Err(KubernetesJobError::NetworkIsolationUnavailable)
        ));
    }

    #[test]
    fn pod_oom_and_explicit_cancel_have_distinct_terminal_outcomes() {
        assert_eq!(
            failed_container_diagnostic(
                &json!({"reason":"OOMKilled","exitCode":137}),
                &test_identity()
            ),
            "LW_OJ_MEMORY_LIMIT"
        );
        assert_eq!(
            cancellation_observation(false),
            OjCancellationObservation::CleanupPending
        );
        assert_eq!(
            cancellation_observation(true),
            OjCancellationObservation::Cancelled
        );
    }
}
