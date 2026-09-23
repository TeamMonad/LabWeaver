//! Attempt-scoped Kubernetes executor for isolated Ansible probe resources.
//!
//! The shared mutation, observation, and cleanup mechanics live in
//! [`crate::kubernetes_job`]; this module owns only the probe resource documents, the evidence
//! receipt, and the stable probe diagnostics.
#![allow(
    missing_docs,
    clippy::too_many_lines,
    reason = "the probe role adapter keeps its exact diagnostics and receipt semantics auditable"
)]

use std::path::PathBuf;

use contracts::execution::{ExecutionCleanupStatus, ExecutionObjectRef, ExecutionObservation};
use reqwest::Url;
use serde::Deserialize;
use thiserror::Error;

use crate::{
    ansible_probe::{AnsibleProbeEvidenceReceipt, AnsibleProbeExecutionRequest},
    ansible_probe_job::{AnsibleProbeJobBinding, AnsibleProbeJobError, AnsibleProbeJobResources},
    control_plane::{EvaluationExecutionKind, EvaluationExecutionResources},
    kubernetes_job::{
        KubernetesApiClient, KubernetesApiConfiguration, KubernetesCleanupTarget,
        KubernetesJobBundle, KubernetesJobError, KubernetesJobIdentity, KubernetesJobObservation,
        KubernetesObject, KubernetesOwnership,
    },
};

const FIELD_MANAGER: &str = "labweaver-ansible-probe-executor";
const MANAGED_BY: &str = "evaluation-service";
const EVENT_SCOPE: &str = "evaluation";
const LOG_SCOPE: &str = "probe.ansible";
const DIAGNOSTIC_PREFIX: &str = "LW_AP_";
const RUNNER_DEFAULT_DENY_POLICY: &str = "ansible-probe-default-deny";
const MAIN_CONTAINER: &str = "ansible-probe";
const DEADLINE_DIAGNOSTIC_CODE: &str = "LW_AP_TIMEOUT";
const FAILED_DIAGNOSTIC_CODE: &str = "LW_AP_INFRASTRUCTURE_ERROR";
const OOM_DIAGNOSTIC_CODE: &str = "LW_AP_INFRASTRUCTURE_ERROR";
const STABLE_DIAGNOSTIC_PREFIX: &str = "LW_AP_";

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AnsibleProbeExecutorConfiguration {
    pub kubernetes_api_server: Url,
    pub kubernetes_bearer_token_file: PathBuf,
    pub kubernetes_ca_file: PathBuf,
    pub runner_namespace: String,
    pub request_timeout_milliseconds: u64,
}

#[derive(Clone)]
pub struct AnsibleProbeKubernetesExecutor {
    api: KubernetesApiClient,
}

impl AnsibleProbeKubernetesExecutor {
    /// Builds an HTTPS-only, CA-pinned Kubernetes executor.
    ///
    /// # Errors
    ///
    /// Returns a stable configuration error for invalid or unavailable credentials.
    pub fn new(
        configuration: AnsibleProbeExecutorConfiguration,
    ) -> Result<Self, AnsibleProbeExecutorError> {
        let api = KubernetesApiClient::new(
            api_configuration(configuration),
            FIELD_MANAGER,
            LOG_SCOPE,
            DIAGNOSTIC_PREFIX,
            MANAGED_BY,
            EVENT_SCOPE,
        )?;
        Ok(Self { api })
    }

    /// Applies the exact attempt-scoped network policy, command, and Job bundle.
    ///
    /// The namespace-wide permanent `ansible-probe-default-deny` `NetworkPolicy` is
    /// verified first; the attempt is idempotent on the immutable
    /// `labweaver.io/request-sha256` annotation, so a retry of the same request
    /// reuses the existing bundle while any drift conflicts.
    ///
    /// # Errors
    ///
    /// Fails closed on invalid ownership, a partial old bundle that cannot be removed, or an API
    /// rejection.
    pub async fn start(
        &self,
        binding: &AnsibleProbeJobBinding,
    ) -> Result<AnsibleProbeJobResources, AnsibleProbeExecutorError> {
        self.start_inner(binding, None).await
    }

    /// Applies an attempt bundle together with the fresh Environment-issued
    /// SSH key and certificate Secrets.
    ///
    /// # Errors
    ///
    /// Returns an error when the binding, SSH credentials, or Kubernetes
    /// operations fail validation or cannot be applied.
    pub async fn start_with_ssh_credentials(
        &self,
        binding: &AnsibleProbeJobBinding,
        private_key_openssh: &str,
        certificate_openssh: &str,
    ) -> Result<AnsibleProbeJobResources, AnsibleProbeExecutorError> {
        self.start_inner(binding, Some((private_key_openssh, certificate_openssh)))
            .await
    }

    async fn start_inner(
        &self,
        binding: &AnsibleProbeJobBinding,
        ssh_credentials: Option<(&str, &str)>,
    ) -> Result<AnsibleProbeJobResources, AnsibleProbeExecutorError> {
        let mut resources = AnsibleProbeJobResources::build(binding)?;
        if let Some((private_key_openssh, certificate_openssh)) = ssh_credentials {
            resources.attach_ssh_credentials(private_key_openssh, certificate_openssh)?;
        }
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
        resources: &AnsibleProbeJobResources,
        request: &AnsibleProbeExecutionRequest,
    ) -> Result<AnsibleProbeJobObservation, AnsibleProbeExecutorError> {
        self.observe_job(resources.name(), None, request).await
    }

    /// Observes a recovered attempt using only durable object references.
    ///
    /// Signed materializer data is never reconstructed on this path.
    ///
    /// # Errors
    ///
    /// Returns an error when the recovered resource identity or observed
    /// Kubernetes state is invalid or unavailable.
    pub async fn observe_recovery(
        &self,
        resources: &EvaluationExecutionResources,
        request: &AnsibleProbeExecutionRequest,
    ) -> Result<AnsibleProbeJobObservation, AnsibleProbeExecutorError> {
        if resources.namespace != self.api.runner_namespace()
            || resources.kind != EvaluationExecutionKind::AnsibleProbe
        {
            return Err(AnsibleProbeExecutorError::BindingInvalid);
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
                .map_err(|_| AnsibleProbeExecutorError::BindingInvalid)?;
            self.observe_job(&attempt_job_name(request.attempt_id), None, request)
                .await
        }
    }

    async fn observe_job(
        &self,
        job_name: &str,
        expected_uid: Option<&str>,
        request: &AnsibleProbeExecutionRequest,
    ) -> Result<AnsibleProbeJobObservation, AnsibleProbeExecutorError> {
        let identity = request_identity(self.api.runner_namespace(), job_name, request)?;
        match self.api.observe(&identity, expected_uid).await? {
            KubernetesJobObservation::Missing => Ok(AnsibleProbeJobObservation::Missing),
            KubernetesJobObservation::Running => Ok(AnsibleProbeJobObservation::Running),
            KubernetesJobObservation::Completed {
                message,
                observation,
            } => {
                let receipt: AnsibleProbeEvidenceReceipt = serde_json::from_str(&message)
                    .map_err(|_| AnsibleProbeExecutorError::ReceiptInvalid)?;
                receipt
                    .validate_for(request)
                    .map_err(|_| AnsibleProbeExecutorError::ReceiptInvalid)?;
                Ok(AnsibleProbeJobObservation::Completed {
                    receipt,
                    observation,
                })
            }
            KubernetesJobObservation::Failed {
                diagnostic_code,
                observation,
            } => Ok(AnsibleProbeJobObservation::Failed {
                diagnostic_code,
                observation,
            }),
        }
    }

    /// Captures immutable Kubernetes object identities after applying the
    /// complete attempt bundle.
    ///
    /// # Errors
    ///
    /// Returns an error when any expected object is unavailable, not owned by
    /// this attempt, or has an invalid identity.
    pub async fn capture_object_refs(
        &self,
        resources: &AnsibleProbeJobResources,
        request: &AnsibleProbeExecutionRequest,
    ) -> Result<Vec<ExecutionObjectRef>, AnsibleProbeExecutorError> {
        let identity = request_identity(self.api.runner_namespace(), resources.name(), request)?;
        let cleanup_plan = cleanup_targets(resources);
        Ok(self
            .api
            .capture_object_refs(&identity, &cleanup_plan)
            .await?)
    }

    /// Captures all currently existing objects for a pre-start intent using
    /// only deterministic names and the persisted request.  Partial bundles
    /// return every verified UID found so cleanup remains identity-bound.
    ///
    /// # Errors
    ///
    /// Returns an error when the recovered request, resource identity, or any
    /// observed object fails validation.
    pub async fn capture_intent_object_refs(
        &self,
        resources: &EvaluationExecutionResources,
        request: &AnsibleProbeExecutionRequest,
    ) -> Result<Option<Vec<ExecutionObjectRef>>, AnsibleProbeExecutorError> {
        if resources.namespace != self.api.runner_namespace()
            || resources.kind != EvaluationExecutionKind::AnsibleProbe
            || !resources.objects.is_empty()
        {
            return Err(AnsibleProbeExecutorError::BindingInvalid);
        }
        request
            .validate()
            .map_err(|_| AnsibleProbeExecutorError::BindingInvalid)?;
        if request.run_id != resources.run_id.as_uuid()
            || request.step_run_id != resources.step_run_id.as_uuid()
            || request.attempt_id != resources.task_run_id.as_uuid()
        {
            return Err(AnsibleProbeExecutorError::IdentityConflict);
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

    /// Deletes and verifies a recovered attempt using only persisted refs.
    ///
    /// # Errors
    ///
    /// Returns an error when the recovered identity is invalid or Kubernetes
    /// cannot delete or verify one of the owned objects.
    pub async fn cleanup_recovery(
        &self,
        resources: &EvaluationExecutionResources,
    ) -> Result<ExecutionCleanupStatus, AnsibleProbeExecutorError> {
        if resources.namespace != self.api.runner_namespace()
            || resources.kind != EvaluationExecutionKind::AnsibleProbe
        {
            return Err(AnsibleProbeExecutorError::BindingInvalid);
        }
        if resources.objects.is_empty() {
            let request: AnsibleProbeExecutionRequest =
                serde_json::from_value(resources.request.clone())
                    .map_err(|_| AnsibleProbeExecutorError::BindingInvalid)?;
            request
                .validate()
                .map_err(|_| AnsibleProbeExecutorError::BindingInvalid)?;
            if request.run_id != resources.run_id.as_uuid()
                || request.step_run_id != resources.step_run_id.as_uuid()
                || request.attempt_id != resources.task_run_id.as_uuid()
            {
                return Err(AnsibleProbeExecutorError::IdentityConflict);
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
        resources: &AnsibleProbeJobResources,
    ) -> Result<AnsibleProbeCancellationObservation, AnsibleProbeExecutorError> {
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
        resources: &AnsibleProbeJobResources,
    ) -> Result<ExecutionCleanupStatus, AnsibleProbeExecutorError> {
        let objects = probe_objects(resources);
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

fn api_configuration(
    configuration: AnsibleProbeExecutorConfiguration,
) -> KubernetesApiConfiguration {
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
    request: &AnsibleProbeExecutionRequest,
) -> Result<KubernetesJobIdentity, AnsibleProbeExecutorError> {
    let request_sha256 = request
        .request_sha256()
        .map_err(|_| AnsibleProbeExecutorError::IdentityConflict)?
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
) -> Result<KubernetesJobIdentity, AnsibleProbeExecutorError> {
    let admission = resources
        .admission
        .as_ref()
        .ok_or(AnsibleProbeExecutorError::BindingInvalid)?;
    Ok(KubernetesJobIdentity {
        namespace: resources.namespace.clone(),
        job_name: resources
            .objects
            .iter()
            .find(|object| object.resource == "jobs")
            .map_or_else(
                || {
                    format!(
                        "lw-ap-{}",
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
    resources: &AnsibleProbeJobResources,
    request: &AnsibleProbeExecutionRequest,
) -> Result<KubernetesJobBundle, AnsibleProbeExecutorError> {
    Ok(KubernetesJobBundle {
        identity: request_identity(namespace, resources.name(), request)?,
        objects: probe_objects(resources),
        cleanup_plan: cleanup_targets(resources),
    })
}

fn probe_objects(resources: &AnsibleProbeJobResources) -> Vec<KubernetesObject> {
    let mut objects = vec![
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
    ];
    if let (Some(name), Some(secret)) = (
        resources.ssh_private_key_secret_name(),
        resources.ssh_private_key_secret(),
    ) {
        objects.push(KubernetesObject {
            api_version: "v1",
            plural: "secrets",
            name: name.to_owned(),
            document: secret.clone(),
        });
    }
    if let (Some(name), Some(secret)) = (
        resources.ssh_certificate_secret_name(),
        resources.ssh_certificate_secret(),
    ) {
        objects.push(KubernetesObject {
            api_version: "v1",
            plural: "secrets",
            name: name.to_owned(),
            document: secret.clone(),
        });
    }
    objects.push(KubernetesObject {
        api_version: "batch/v1",
        plural: "jobs",
        name: resources.name().to_owned(),
        document: resources.job.clone(),
    });
    objects
}

fn cleanup_targets(resources: &AnsibleProbeJobResources) -> Vec<KubernetesCleanupTarget> {
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

const fn cancellation_observation(cleanup_complete: bool) -> AnsibleProbeCancellationObservation {
    if cleanup_complete {
        AnsibleProbeCancellationObservation::Cancelled
    } else {
        AnsibleProbeCancellationObservation::CleanupPending
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnsibleProbeCancellationObservation {
    CleanupPending,
    Cancelled,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AnsibleProbeJobObservation {
    Missing,
    Running,
    Completed {
        receipt: AnsibleProbeEvidenceReceipt,
        observation: ExecutionObservation,
    },
    Failed {
        diagnostic_code: String,
        observation: ExecutionObservation,
    },
}

fn attempt_job_name(attempt_id: uuid::Uuid) -> String {
    format!("lw-ap-{}", &attempt_id.simple().to_string()[..20])
}

fn recovery_cleanup_plan(
    namespace: &str,
    request: &AnsibleProbeExecutionRequest,
) -> Vec<KubernetesCleanupTarget> {
    let name = attempt_job_name(request.attempt_id);
    let materializer = format!("{name}-materializer");
    let (private_key, certificate) = (
        request.ssh_identity.private_key_secret.clone(),
        request.ssh_identity.certificate_secret.clone(),
    );
    [
        ("jobs", name.clone()),
        ("networkpolicies", name.clone()),
        ("configmaps", name),
        ("secrets", materializer),
        ("secrets", private_key),
        ("secrets", certificate),
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
pub enum AnsibleProbeExecutorError {
    #[error("ansible probe executor configuration is unavailable during {operation}: {source}")]
    ConfigurationUnavailable {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("ansible probe executor configuration is invalid")]
    ConfigurationInvalid,
    #[error("ansible probe Job binding is invalid")]
    BindingInvalid,
    #[error("ansible probe Kubernetes API is unavailable")]
    KubernetesUnavailable,
    #[error("ansible probe Kubernetes API rejected the operation")]
    KubernetesRejected,
    #[error("ansible probe runner namespace default-deny isolation is unavailable")]
    NetworkIsolationUnavailable,
    #[error("ansible probe attempt identity conflicts with an existing resource")]
    IdentityConflict,
    #[error("ansible probe Job cleanup is pending")]
    CleanupPending,
    #[error("ansible probe Job observation is invalid")]
    ObservationInvalid,
    #[error("ansible probe evidence receipt is invalid")]
    ReceiptInvalid,
    #[error(transparent)]
    Job(#[from] AnsibleProbeJobError),
}

impl From<KubernetesJobError> for AnsibleProbeExecutorError {
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

impl AnsibleProbeExecutorError {
    #[must_use]
    pub const fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::ConfigurationUnavailable { .. } => "LW_AP_EXECUTOR_CONFIG_UNAVAILABLE",
            Self::ConfigurationInvalid => "LW_AP_EXECUTOR_CONFIG_INVALID",
            Self::BindingInvalid => "LW_AP_JOB_BINDING_INVALID",
            Self::KubernetesUnavailable => "LW_AP_KUBERNETES_UNAVAILABLE",
            Self::KubernetesRejected => "LW_AP_KUBERNETES_REJECTED",
            Self::NetworkIsolationUnavailable => "LW_AP_NETWORK_ISOLATION_UNAVAILABLE",
            Self::IdentityConflict => "LW_AP_ATTEMPT_IDENTITY_CONFLICT",
            Self::CleanupPending => "LW_AP_CLEANUP_PENDING",
            Self::ObservationInvalid => "LW_AP_OBSERVATION_INVALID",
            Self::ReceiptInvalid => "LW_AP_RECEIPT_INVALID",
            Self::Job(error) => error.diagnostic_code(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, net::Ipv4Addr};

    use contracts::{EvaluationRunId, EvaluationStepRunId, TaskRunId, evaluation::FactAssertion};
    use persistence_sqlx::Sha256Digest; // internal persistence hash, not contract hash
    use reqwest::StatusCode;
    use serde_json::{Value, json};
    use uuid::Uuid;

    use super::{
        AnsibleProbeCancellationObservation, attempt_job_name, cancellation_observation,
        recovery_cleanup_plan,
    };
    use crate::ansible_probe::{
        ANSIBLE_PROBE_EXECUTION_SCHEMA_VERSION, AnsibleProbeExecutionLimits,
        AnsibleProbeExecutionRequest, AnsibleProbeSshIdentity, AnsibleProbeTarget,
    };
    use crate::control_plane::{EvaluationExecutionKind, EvaluationExecutionResources};
    use crate::kubernetes_job::{
        KubernetesCleanupTarget, KubernetesDeletePreconditions, KubernetesJobError,
        KubernetesJobIdentity, KubernetesOwnership, classify_delete_status, delete_options,
        delete_preconditions, failed_container_diagnostic, read_bound_file, verify_cleanup_owned,
        verify_owned, verify_runner_default_deny,
    };

    use super::MANAGED_BY;

    const TEST_NAMESPACE: &str = "labweaver-evaluation-runs";

    fn assertion(fact: &str, expected: &serde_json::Value) -> FactAssertion {
        match serde_json::from_value(json!({ "fact": fact, "expected": expected })) {
            Ok(assertion) => assertion,
            Err(error) => unreachable!("fixture assertion must deserialize: {error}"),
        }
    }

    fn request() -> AnsibleProbeExecutionRequest {
        AnsibleProbeExecutionRequest {
            schema_version: ANSIBLE_PROBE_EXECUTION_SCHEMA_VERSION.to_owned(),
            run_id: Uuid::now_v7(),
            step_run_id: Uuid::now_v7(),
            attempt_id: Uuid::now_v7(),
            trace_id: "trace-ansible-probe-executor-test".to_owned(),
            runner_image_digest: format!("labweaver/ansible-probe@sha256:{}", "2".repeat(64)),
            playbook_profile: "linux-nginx-probe-v1/playbook.yml".to_owned(),
            module_allowlist: vec!["ansible.builtin.service_facts".to_owned()],
            read_only: true,
            assertions: vec![assertion("host.reachable", &json!(true))],
            target: AnsibleProbeTarget {
                host: Ipv4Addr::new(192, 168, 56, 10),
                port: 22,
                username: "labweaver".to_owned(),
            },
            source_identity: "source-identity".to_owned(),
            ssh_identity: AnsibleProbeSshIdentity {
                private_key_secret: "probe-ssh-key".to_owned(),
                certificate_secret: "probe-ssh-cert".to_owned(),
                expected_host_key_sha256: Sha256Digest::of_bytes(b"host-key"),
            },
            limits: AnsibleProbeExecutionLimits {
                wall_time_seconds: 60,
                facts_max_bytes: 1024 * 1024,
                output_max_bytes: 64 * 1024,
                max_assertions: 8,
            },
            evaluation_spec_sha256: Sha256Digest::of_bytes(b"evaluation-spec"),
        }
    }

    fn ownership(request: &AnsibleProbeExecutionRequest) -> KubernetesOwnership {
        KubernetesOwnership {
            run_id: request.run_id,
            step_run_id: request.step_run_id,
            attempt_id: request.attempt_id,
            request_sha256: match request.request_sha256() {
                Ok(digest) => digest.to_string(),
                Err(error) => unreachable!("fixture request identity must compute: {error}"),
            },
        }
    }

    fn test_identity() -> KubernetesJobIdentity {
        KubernetesJobIdentity {
            namespace: TEST_NAMESPACE.to_owned(),
            job_name: "lw-ap-attempt".to_owned(),
            main_container: "ansible-probe",
            default_deny_policy: "ansible-probe-default-deny",
            deadline_diagnostic_code: "LW_AP_TIMEOUT",
            failed_diagnostic_code: "LW_AP_INFRASTRUCTURE_ERROR",
            oom_diagnostic_code: "LW_AP_INFRASTRUCTURE_ERROR",
            stable_diagnostic_prefix: "LW_AP_",
            ownership: ownership(&request()),
            trace_id: "trace".to_owned(),
        }
    }

    #[test]
    fn read_bound_file_accepts_a_kubernetes_projected_symlink()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir()?;
        let revision = directory.path().join("..2026_09_09_00_00_00.000000001");
        fs::create_dir(&revision)?;
        fs::write(revision.join("ca.crt"), b"projected-ca")?;
        symlink(&revision, directory.path().join("..data"))?;
        let projected = directory.path().join("ca.crt");
        symlink("..data/ca.crt", &projected)?;

        assert_eq!(read_bound_file(&projected)?, b"projected-ca");
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

    #[test]
    fn cancel_empty_intent_plan_is_attempt_scoped_and_covers_ssh_credentials()
    -> Result<(), Box<dyn std::error::Error>> {
        let request = request();
        let run_id: EvaluationRunId = request.run_id.to_string().parse()?;
        let step_run_id: EvaluationStepRunId = request.step_run_id.to_string().parse()?;
        let task_run_id: TaskRunId = request.attempt_id.to_string().parse()?;
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
        let intent = EvaluationExecutionResources {
            schema_version: crate::EVALUATION_EXECUTION_RESOURCES_SCHEMA_VERSION.to_owned(),
            run_id,
            step_run_id,
            task_run_id,
            namespace: TEST_NAMESPACE.to_owned(),
            kind: EvaluationExecutionKind::AnsibleProbe,
            admission: Some(admission),
            request: serde_json::to_value(&request)?,
            objects: Vec::new(),
        };
        intent.validate_for(run_id, step_run_id, task_run_id)?;

        let targets = recovery_cleanup_plan(intent.namespace.as_str(), &request);
        assert_eq!(targets.len(), 6);
        let attempt_name = attempt_job_name(request.attempt_id);
        assert!(
            targets
                .iter()
                .any(|target| target.resource == "jobs" && target.name == attempt_name)
        );
        assert!(
            targets
                .iter()
                .any(|target| { target.resource == "configmaps" && target.name == attempt_name })
        );
        assert!(targets.iter().any(|target| {
            target.resource == "secrets" && target.name == request.ssh_identity.private_key_secret
        }));
        assert!(targets.iter().any(|target| {
            target.resource == "secrets" && target.name == request.ssh_identity.certificate_secret
        }));
        Ok(())
    }

    fn owned_resource(request: &AnsibleProbeExecutionRequest) -> Value {
        json!({
            "apiVersion":"batch/v1",
            "kind":"Job",
            "metadata":{
                "name":"lw-ap-attempt",
                "namespace":TEST_NAMESPACE,
                "uid":"f7780f4c-e8db-4f35-82d1-bc56b5652830",
                "resourceVersion":"1042",
                "labels":{
                    "labweaver.io/managed-by":"evaluation-service",
                    "labweaver.io/run-id":request.run_id.to_string(),
                    "labweaver.io/step-run-id":request.step_run_id.to_string(),
                    "labweaver.io/attempt-id":request.attempt_id.to_string(),
                },
                "annotations":{
                    "labweaver.io/request-sha256": ownership(request).request_sha256,
                },
            },
        })
    }

    #[test]
    fn request_sha256_annotation_reuses_the_same_attempt_and_conflicts_on_drift() {
        let expected = request();
        let resource = owned_resource(&expected);
        assert!(verify_owned(&resource, &ownership(&expected), MANAGED_BY).is_ok());

        // A second attempt identity never reuses the first attempt's resources.
        let other = request();
        assert!(matches!(
            verify_owned(&resource, &ownership(&other), MANAGED_BY),
            Err(KubernetesJobError::IdentityConflict)
        ));

        let mut drifted = owned_resource(&expected);
        drifted["metadata"]["annotations"]["labweaver.io/request-sha256"] = json!("0".repeat(64));
        assert!(matches!(
            verify_owned(&drifted, &ownership(&expected), MANAGED_BY),
            Err(KubernetesJobError::IdentityConflict)
        ));

        let mut drifted = owned_resource(&expected);
        drifted["metadata"]["labels"]["labweaver.io/managed-by"] = json!("other-service");
        assert!(matches!(
            verify_owned(&drifted, &ownership(&expected), MANAGED_BY),
            Err(KubernetesJobError::IdentityConflict)
        ));
    }

    #[test]
    fn cleanup_ownership_rejects_request_or_namespace_drift() {
        let request = request();
        let expected = owned_resource(&request);
        assert!(verify_cleanup_owned(&owned_resource(&request), &expected).is_ok());

        let mut drifted = owned_resource(&request);
        drifted["metadata"]["annotations"]["labweaver.io/request-sha256"] = json!("1".repeat(64));
        assert!(matches!(
            verify_cleanup_owned(&drifted, &expected),
            Err(KubernetesJobError::IdentityConflict)
        ));

        let mut drifted = owned_resource(&request);
        drifted["metadata"]["namespace"] = json!("another-namespace");
        assert!(matches!(
            verify_cleanup_owned(&drifted, &expected),
            Err(KubernetesJobError::IdentityConflict)
        ));
    }

    #[test]
    fn cleanup_delete_is_bound_to_the_observed_resource_identity()
    -> Result<(), Box<dyn std::error::Error>> {
        let request = request();
        let current = owned_resource(&request);
        let preconditions = delete_preconditions(&current)?;
        assert_eq!(
            preconditions,
            KubernetesDeletePreconditions {
                uid: "f7780f4c-e8db-4f35-82d1-bc56b5652830".to_owned(),
                resource_version: "1042".to_owned(),
            }
        );
        let target = KubernetesCleanupTarget {
            namespace: TEST_NAMESPACE.to_owned(),
            resource: "jobs".to_owned(),
            name: "lw-ap-attempt".to_owned(),
            propagation_policy: "Foreground".to_owned(),
        };
        let options = delete_options(&target, &preconditions);
        assert_eq!(
            options
                .pointer("/preconditions/uid")
                .and_then(Value::as_str),
            Some("f7780f4c-e8db-4f35-82d1-bc56b5652830")
        );
        assert_eq!(
            options
                .pointer("/preconditions/resourceVersion")
                .and_then(Value::as_str),
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
                "name":"ansible-probe-default-deny",
                "namespace":TEST_NAMESPACE,
            },
            "spec":{
                "podSelector":{},
                "policyTypes":["Ingress","Egress"],
                "ingress":[],
                "egress":[],
            },
        });
        assert!(
            verify_runner_default_deny(&policy, TEST_NAMESPACE, "ansible-probe-default-deny")
                .is_ok()
        );

        let normalized = json!({
            "apiVersion":"networking.k8s.io/v1",
            "kind":"NetworkPolicy",
            "metadata":{
                "name":"ansible-probe-default-deny",
                "namespace":TEST_NAMESPACE,
            },
            "spec":{
                "podSelector":{},
                "policyTypes":["Ingress","Egress"],
            },
        });
        assert!(
            verify_runner_default_deny(&normalized, TEST_NAMESPACE, "ansible-probe-default-deny")
                .is_ok()
        );

        let mut allows_egress = policy.clone();
        allows_egress["spec"]["egress"] = json!([{}]);
        assert!(matches!(
            verify_runner_default_deny(
                &allows_egress,
                TEST_NAMESPACE,
                "ansible-probe-default-deny"
            ),
            Err(KubernetesJobError::NetworkIsolationUnavailable)
        ));

        let mut wrong_rule_type = normalized;
        wrong_rule_type["spec"]["egress"] = json!({});
        assert!(matches!(
            verify_runner_default_deny(
                &wrong_rule_type,
                TEST_NAMESPACE,
                "ansible-probe-default-deny"
            ),
            Err(KubernetesJobError::NetworkIsolationUnavailable)
        ));

        let mut allows_dns = policy.clone();
        allows_dns["spec"]["egress"] = json!([{"ports":[{"protocol":"UDP","port":53}]}]);
        assert!(matches!(
            verify_runner_default_deny(&allows_dns, TEST_NAMESPACE, "ansible-probe-default-deny"),
            Err(KubernetesJobError::NetworkIsolationUnavailable)
        ));

        let mut selected_only = policy;
        selected_only["spec"]["podSelector"] =
            json!({"matchLabels":{"labweaver.io/attempt-id":"attempt"}});
        assert!(matches!(
            verify_runner_default_deny(
                &selected_only,
                TEST_NAMESPACE,
                "ansible-probe-default-deny"
            ),
            Err(KubernetesJobError::NetworkIsolationUnavailable)
        ));
    }

    #[test]
    fn failed_containers_map_to_stable_terminal_diagnostics() {
        assert_eq!(
            failed_container_diagnostic(
                &json!({"reason":"OOMKilled","exitCode":137}),
                &test_identity()
            ),
            "LW_AP_INFRASTRUCTURE_ERROR"
        );
        assert_eq!(
            failed_container_diagnostic(
                &json!({"reason":"Error","message":"LW_AP_PROFILE_INVALID"}),
                &test_identity()
            ),
            "LW_AP_PROFILE_INVALID"
        );
        // Untrusted pod output never becomes a stable diagnostic.
        assert_eq!(
            failed_container_diagnostic(
                &json!({"reason":"Error","message":"panic: segfault"}),
                &test_identity()
            ),
            "LW_AP_INFRASTRUCTURE_ERROR"
        );
        assert_eq!(
            failed_container_diagnostic(&json!({"reason":"Error"}), &test_identity()),
            "LW_AP_INFRASTRUCTURE_ERROR"
        );
    }

    #[test]
    fn cancellation_tracks_cleanup_completion() {
        assert_eq!(
            cancellation_observation(false),
            AnsibleProbeCancellationObservation::CleanupPending
        );
        assert_eq!(
            cancellation_observation(true),
            AnsibleProbeCancellationObservation::Cancelled
        );
    }
}
