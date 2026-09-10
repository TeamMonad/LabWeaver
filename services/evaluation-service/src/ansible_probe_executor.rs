//! Attempt-scoped Kubernetes executor for isolated Ansible probe resources.
#![allow(
    missing_docs,
    clippy::too_many_lines,
    reason = "the exact Kubernetes mutation and cleanup boundary is intentionally colocated"
)]

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

use contracts::UtcTimestamp;
use reqwest::{Certificate, Client, Method, StatusCode, Url};
use serde::Deserialize;
use serde_json::{Value, json};
use thiserror::Error;

use crate::{
    ansible_probe::{AnsibleProbeEvidenceReceipt, AnsibleProbeExecutionRequest},
    ansible_probe_job::{
        AnsibleProbeCleanupTarget, AnsibleProbeJobBinding, AnsibleProbeJobError,
        AnsibleProbeJobResources,
    },
    control_plane::EvaluationExecutionResources,
    execution::ExecutionTiming,
};

const FIELD_MANAGER: &str = "labweaver-ansible-probe-executor";
const MAX_BOUND_FILE_BYTES: u64 = 64 * 1024;
const RUNNER_DEFAULT_DENY_POLICY: &str = "ansible-probe-default-deny";

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
    configuration: AnsibleProbeExecutorConfiguration,
    client: Client,
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
        if configuration.kubernetes_api_server.scheme() != "https"
            || configuration.kubernetes_api_server.host_str().is_none()
            || configuration.runner_namespace.trim().is_empty()
            || configuration.request_timeout_milliseconds == 0
            || configuration.request_timeout_milliseconds > 60_000
        {
            return Err(AnsibleProbeExecutorError::ConfigurationInvalid);
        }
        read_bound_text(&configuration.kubernetes_bearer_token_file)?;
        let ca = Certificate::from_pem(&read_bound_file(&configuration.kubernetes_ca_file)?)
            .map_err(|_| AnsibleProbeExecutorError::ConfigurationInvalid)?;
        let client = Client::builder()
            .https_only(true)
            .tls_built_in_root_certs(false)
            .add_root_certificate(ca)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_millis(
                configuration.request_timeout_milliseconds,
            ))
            .build()
            .map_err(|_| AnsibleProbeExecutorError::ConfigurationInvalid)?;
        Ok(Self {
            configuration,
            client,
        })
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
        if binding.namespace != self.configuration.runner_namespace {
            return Err(AnsibleProbeExecutorError::BindingInvalid);
        }
        self.require_runner_default_deny().await?;
        let mut resources = AnsibleProbeJobResources::build(binding)?;
        if let Some((private_key_openssh, certificate_openssh)) = ssh_credentials {
            resources.attach_ssh_credentials(private_key_openssh, certificate_openssh)?;
        }
        let mut expected = vec![
            ("v1", "configmaps", &resources.config_map),
            ("v1", "secrets", &resources.materializer_secret),
            (
                "networking.k8s.io/v1",
                "networkpolicies",
                &resources.network_policy,
            ),
            ("batch/v1", "jobs", &resources.job),
        ];
        if let Some(secret) = resources.ssh_private_key_secret() {
            expected.push(("v1", "secrets", secret));
        }
        if let Some(secret) = resources.ssh_certificate_secret() {
            expected.push(("v1", "secrets", secret));
        }
        let mut existing = 0_usize;
        for &(api_version, plural, document) in &expected {
            if let Some(current) = self
                .get(
                    binding.namespace.as_str(),
                    api_version,
                    plural,
                    resources.name(),
                )
                .await?
            {
                verify_owned(&current, &binding.request)?;
                verify_immutable_identity(&current, document)?;
                existing = existing
                    .checked_add(1)
                    .ok_or(AnsibleProbeExecutorError::IdentityConflict)?;
            }
        }
        let complete_existing_bundle = existing == expected.len();
        if existing != 0 && !complete_existing_bundle && !self.cleanup(&resources).await? {
            return Err(AnsibleProbeExecutorError::CleanupPending);
        }

        let apply_result = async {
            self.apply(
                binding.namespace.as_str(),
                "networking.k8s.io/v1",
                "networkpolicies",
                resources.name(),
                &resources.network_policy,
            )
            .await?;
            self.apply(
                binding.namespace.as_str(),
                "v1",
                "configmaps",
                resources.name(),
                &resources.config_map,
            )
            .await?;
            self.apply(
                binding.namespace.as_str(),
                "v1",
                "secrets",
                resources.materializer_secret_name(),
                &resources.materializer_secret,
            )
            .await?;
            if let (Some(name), Some(secret)) = (
                resources.ssh_private_key_secret_name(),
                resources.ssh_private_key_secret(),
            ) {
                self.apply(binding.namespace.as_str(), "v1", "secrets", name, secret)
                    .await?;
            }
            if let (Some(name), Some(secret)) = (
                resources.ssh_certificate_secret_name(),
                resources.ssh_certificate_secret(),
            ) {
                self.apply(binding.namespace.as_str(), "v1", "secrets", name, secret)
                    .await?;
            }
            self.apply(
                binding.namespace.as_str(),
                "batch/v1",
                "jobs",
                resources.name(),
                &resources.job,
            )
            .await
        }
        .await;
        if let Err(error) = apply_result {
            if !complete_existing_bundle {
                let cleanup = self.cleanup(&resources).await;
                if !matches!(cleanup, Ok(true)) {
                    return Err(AnsibleProbeExecutorError::CleanupPending);
                }
            }
            return Err(error);
        }
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
        if resources.namespace != self.configuration.runner_namespace
            || resources.kind != crate::control_plane::EvaluationExecutionKind::AnsibleProbe
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
        let Some(job) = self
            .get(
                self.configuration.runner_namespace.as_str(),
                "batch/v1",
                "jobs",
                job_name,
            )
            .await?
        else {
            return Ok(AnsibleProbeJobObservation::Missing);
        };
        if expected_uid
            .is_some_and(|uid| job.pointer("/metadata/uid").and_then(Value::as_str) != Some(uid))
        {
            return Err(AnsibleProbeExecutorError::IdentityConflict);
        }
        verify_owned(&job, request)?;
        let succeeded = job.pointer("/status/succeeded").and_then(Value::as_u64) == Some(1);
        let failed = job
            .pointer("/status/failed")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            > 0;
        if !succeeded && !failed {
            return Ok(AnsibleProbeJobObservation::Running);
        }
        let pods = self.list_pods(request).await?;
        let items = pods
            .pointer("/items")
            .and_then(Value::as_array)
            .ok_or(AnsibleProbeExecutorError::ObservationInvalid)?;
        if items.len() != 1 {
            return Err(AnsibleProbeExecutorError::ObservationInvalid);
        }
        let pod = &items[0];
        verify_owned(pod, request)?;
        let container = main_container_status(pod)?;
        let timing = container
            .map(execution_timing)
            .transpose()?
            .unwrap_or_else(ExecutionTiming::unknown);
        let terminated = container.and_then(|status| status.pointer("/state/terminated"));
        if succeeded {
            let message = terminated
                .ok_or(AnsibleProbeExecutorError::ObservationInvalid)?
                .pointer("/message")
                .and_then(Value::as_str)
                .ok_or(AnsibleProbeExecutorError::ReceiptInvalid)?;
            let receipt: AnsibleProbeEvidenceReceipt = serde_json::from_str(message)
                .map_err(|_| AnsibleProbeExecutorError::ReceiptInvalid)?;
            receipt
                .validate_for(request)
                .map_err(|_| AnsibleProbeExecutorError::ReceiptInvalid)?;
            return Ok(AnsibleProbeJobObservation::Completed { receipt, timing });
        }
        let job_reason = job
            .pointer("/status/conditions")
            .and_then(Value::as_array)
            .and_then(|conditions| {
                conditions.iter().find(|condition| {
                    condition.pointer("/type").and_then(Value::as_str) == Some("Failed")
                        && condition.pointer("/status").and_then(Value::as_str) == Some("True")
                })
            })
            .and_then(|condition| condition.pointer("/reason").and_then(Value::as_str));
        if job_reason == Some("DeadlineExceeded") {
            return Ok(AnsibleProbeJobObservation::Failed {
                diagnostic_code: AnsibleProbeTerminalFailure::Timeout
                    .diagnostic_code()
                    .to_owned(),
                timing,
            });
        }
        let diagnostic_code =
            terminated.map_or("LW_AP_INFRASTRUCTURE_ERROR", failed_container_diagnostic);
        Ok(AnsibleProbeJobObservation::Failed {
            diagnostic_code: diagnostic_code.to_owned(),
            timing,
        })
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
    ) -> Result<Vec<(String, String, String, String)>, AnsibleProbeExecutorError> {
        let mut refs = Vec::with_capacity(resources.cleanup_plan().len());
        for target in resources.cleanup_plan() {
            let api_version = api_version(&target)?.to_owned();
            let current = self
                .get(
                    target.namespace.as_str(),
                    api_version.as_str(),
                    target.resource.as_str(),
                    target.name.as_str(),
                )
                .await?
                .ok_or(AnsibleProbeExecutorError::ObservationInvalid)?;
            verify_owned(&current, request)?;
            let uid = current
                .pointer("/metadata/uid")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or(AnsibleProbeExecutorError::IdentityConflict)?;
            refs.push((api_version, target.resource, target.name, uid.to_owned()));
        }
        Ok(refs)
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
    ) -> Result<Option<Vec<(String, String, String, String)>>, AnsibleProbeExecutorError> {
        if resources.namespace != self.configuration.runner_namespace
            || resources.kind != crate::control_plane::EvaluationExecutionKind::AnsibleProbe
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
        let mut refs = Vec::with_capacity(targets.len());
        for target in &targets {
            let api_version = api_version(target)?.to_owned();
            let Some(current) = self
                .get(
                    target.namespace.as_str(),
                    api_version.as_str(),
                    target.resource.as_str(),
                    target.name.as_str(),
                )
                .await?
            else {
                continue;
            };
            verify_owned(&current, request)?;
            let uid = current
                .pointer("/metadata/uid")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or(AnsibleProbeExecutorError::IdentityConflict)?;
            refs.push((
                api_version,
                target.resource.clone(),
                target.name.clone(),
                uid.to_owned(),
            ));
        }
        Ok((!refs.is_empty()).then_some(refs))
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
    ) -> Result<bool, AnsibleProbeExecutorError> {
        if resources.namespace != self.configuration.runner_namespace
            || resources.kind != crate::control_plane::EvaluationExecutionKind::AnsibleProbe
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
            return self
                .cleanup_intent(resources.namespace.as_str(), &request)
                .await;
        }
        for object in &resources.objects {
            let target = AnsibleProbeCleanupTarget {
                namespace: resources.namespace.clone(),
                resource: object.resource.clone(),
                name: object.name.clone(),
                propagation_policy: "Foreground".to_owned(),
            };
            let current = self
                .get(
                    target.namespace.as_str(),
                    object.api_version.as_str(),
                    target.resource.as_str(),
                    target.name.as_str(),
                )
                .await?;
            let Some(current) = current else { continue };
            verify_recovery_owned(&current, object, resources)?;
            let preconditions = delete_preconditions(&current)?;
            self.delete(&target, &preconditions).await?;
        }
        for object in &resources.objects {
            let current = self
                .get(
                    resources.namespace.as_str(),
                    object.api_version.as_str(),
                    object.resource.as_str(),
                    object.name.as_str(),
                )
                .await?;
            if let Some(current) = current {
                verify_recovery_owned(&current, object, resources)?;
                return Ok(false);
            }
        }
        Ok(true)
    }

    async fn cleanup_intent(
        &self,
        namespace: &str,
        request: &AnsibleProbeExecutionRequest,
    ) -> Result<bool, AnsibleProbeExecutorError> {
        let targets = recovery_cleanup_plan(namespace, request);
        for target in &targets {
            let Some(current) = self
                .get(
                    target.namespace.as_str(),
                    api_version(target)?,
                    target.resource.as_str(),
                    target.name.as_str(),
                )
                .await?
            else {
                continue;
            };
            verify_owned(&current, request)?;
            let preconditions = delete_preconditions(&current)?;
            self.delete(target, &preconditions).await?;
        }
        for target in &targets {
            if let Some(current) = self
                .get(
                    target.namespace.as_str(),
                    api_version(target)?,
                    target.resource.as_str(),
                    target.name.as_str(),
                )
                .await?
            {
                verify_owned(&current, request)?;
                return Ok(false);
            }
        }
        Ok(true)
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
        self.cleanup(resources).await.map(cancellation_observation)
    }

    /// Deletes and verifies absence of only the attempt-owned Job, policy, and command object.
    ///
    /// # Errors
    ///
    /// Returns a stable Kubernetes or ownership error; `Ok(false)` means deletion is still pending.
    pub async fn cleanup(
        &self,
        resources: &AnsibleProbeJobResources,
    ) -> Result<bool, AnsibleProbeExecutorError> {
        for target in resources.cleanup_plan() {
            if let Some(current) = self
                .get(
                    target.namespace.as_str(),
                    api_version(&target)?,
                    target.resource.as_str(),
                    target.name.as_str(),
                )
                .await?
            {
                let expected = resources
                    .document_for_target(&target)
                    .ok_or(AnsibleProbeExecutorError::BindingInvalid)?;
                verify_cleanup_owned(&current, expected)?;
                let preconditions = delete_preconditions(&current)?;
                self.delete(&target, &preconditions).await?;
            }
        }
        for target in resources.cleanup_plan() {
            if let Some(resource) = self
                .get(
                    target.namespace.as_str(),
                    api_version(&target)?,
                    target.resource.as_str(),
                    target.name.as_str(),
                )
                .await?
            {
                let expected = resources
                    .document_for_target(&target)
                    .ok_or(AnsibleProbeExecutorError::BindingInvalid)?;
                verify_cleanup_owned(&resource, expected)?;
                return Ok(false);
            }
        }
        Ok(true)
    }

    async fn require_runner_default_deny(&self) -> Result<(), AnsibleProbeExecutorError> {
        let policy = self
            .get(
                self.configuration.runner_namespace.as_str(),
                "networking.k8s.io/v1",
                "networkpolicies",
                RUNNER_DEFAULT_DENY_POLICY,
            )
            .await?
            .ok_or(AnsibleProbeExecutorError::NetworkIsolationUnavailable)?;
        verify_runner_default_deny(&policy, self.configuration.runner_namespace.as_str())
    }

    async fn apply(
        &self,
        namespace: &str,
        api_version: &str,
        plural: &str,
        name: &str,
        document: &Value,
    ) -> Result<(), AnsibleProbeExecutorError> {
        let response = self
            .authorized(self.client.request(
                Method::PATCH,
                self.resource_url(namespace, api_version, plural, name)?,
            ))?
            .query(&[("fieldManager", FIELD_MANAGER)])
            .header("content-type", "application/apply-patch+yaml")
            .body(
                serde_json::to_vec(document)
                    .map_err(|_| AnsibleProbeExecutorError::BindingInvalid)?,
            )
            .send()
            .await
            .map_err(|_| AnsibleProbeExecutorError::KubernetesUnavailable)?;
        if response.status().is_success() {
            Ok(())
        } else if response.status() == StatusCode::CONFLICT {
            Err(AnsibleProbeExecutorError::IdentityConflict)
        } else {
            Err(AnsibleProbeExecutorError::KubernetesRejected)
        }
    }

    async fn get(
        &self,
        namespace: &str,
        api_version: &str,
        plural: &str,
        name: &str,
    ) -> Result<Option<Value>, AnsibleProbeExecutorError> {
        let response = self
            .authorized(self.client.get(self.resource_url(
                namespace,
                api_version,
                plural,
                name,
            )?))?
            .send()
            .await
            .map_err(|_| AnsibleProbeExecutorError::KubernetesUnavailable)?;
        if response.status() == StatusCode::NOT_FOUND {
            Ok(None)
        } else if response.status().is_success() {
            response
                .json()
                .await
                .map(Some)
                .map_err(|_| AnsibleProbeExecutorError::ObservationInvalid)
        } else {
            Err(AnsibleProbeExecutorError::KubernetesRejected)
        }
    }

    async fn list_pods(
        &self,
        request: &AnsibleProbeExecutionRequest,
    ) -> Result<Value, AnsibleProbeExecutorError> {
        let response = self
            .authorized(self.client.get(self.collection_url(
                self.configuration.runner_namespace.as_str(),
                "v1",
                "pods",
            )?))?
            .query(&[(
                "labelSelector",
                format!("labweaver.io/attempt-id={}", request.attempt_id),
            )])
            .send()
            .await
            .map_err(|_| AnsibleProbeExecutorError::KubernetesUnavailable)?;
        if response.status().is_success() {
            response
                .json()
                .await
                .map_err(|_| AnsibleProbeExecutorError::ObservationInvalid)
        } else {
            Err(AnsibleProbeExecutorError::KubernetesRejected)
        }
    }

    async fn delete(
        &self,
        target: &AnsibleProbeCleanupTarget,
        preconditions: &AnsibleProbeDeletePreconditions,
    ) -> Result<(), AnsibleProbeExecutorError> {
        let response = self
            .authorized(self.client.delete(self.resource_url(
                target.namespace.as_str(),
                api_version(target)?,
                target.resource.as_str(),
                target.name.as_str(),
            )?))?
            .json(&delete_options(target, preconditions))
            .send()
            .await
            .map_err(|_| AnsibleProbeExecutorError::KubernetesUnavailable)?;
        classify_delete_status(response.status())
    }

    fn authorized(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::RequestBuilder, AnsibleProbeExecutorError> {
        let token = read_bound_text(&self.configuration.kubernetes_bearer_token_file)?;
        Ok(request.bearer_auth(token))
    }

    fn resource_url(
        &self,
        namespace: &str,
        api_version: &str,
        plural: &str,
        name: &str,
    ) -> Result<Url, AnsibleProbeExecutorError> {
        if !safe_segment(namespace) || !safe_segment(plural) || !safe_segment(name) {
            return Err(AnsibleProbeExecutorError::BindingInvalid);
        }
        self.configuration
            .kubernetes_api_server
            .join(&format!(
                "{}/namespaces/{namespace}/{plural}/{name}",
                api_prefix(api_version)
            ))
            .map_err(|_| AnsibleProbeExecutorError::ConfigurationInvalid)
    }

    fn collection_url(
        &self,
        namespace: &str,
        api_version: &str,
        plural: &str,
    ) -> Result<Url, AnsibleProbeExecutorError> {
        if !safe_segment(namespace) || !safe_segment(plural) {
            return Err(AnsibleProbeExecutorError::BindingInvalid);
        }
        self.configuration
            .kubernetes_api_server
            .join(&format!(
                "{}/namespaces/{namespace}/{plural}",
                api_prefix(api_version)
            ))
            .map_err(|_| AnsibleProbeExecutorError::ConfigurationInvalid)
    }
}

fn classify_delete_status(status: StatusCode) -> Result<(), AnsibleProbeExecutorError> {
    if status.is_success() || status == StatusCode::NOT_FOUND {
        Ok(())
    } else if status == StatusCode::CONFLICT {
        Err(AnsibleProbeExecutorError::IdentityConflict)
    } else {
        Err(AnsibleProbeExecutorError::KubernetesRejected)
    }
}

/// Maps a failed container to the stable terminal diagnostic it reported.
///
/// The worker writes only payload-free `LW_AP_*` diagnostics or a receipt; any
/// other message, an OOM kill, or a missing message is infrastructure failure,
/// never a probe outcome.
fn failed_container_diagnostic(terminated: &Value) -> &str {
    if terminated.pointer("/reason").and_then(Value::as_str) == Some("OOMKilled") {
        AnsibleProbeTerminalFailure::InfrastructureError.diagnostic_code()
    } else {
        terminated
            .pointer("/message")
            .and_then(Value::as_str)
            .filter(|message| is_stable_diagnostic(message))
            .unwrap_or(AnsibleProbeTerminalFailure::InfrastructureError.diagnostic_code())
    }
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
        timing: ExecutionTiming,
    },
    Failed {
        diagnostic_code: String,
        timing: ExecutionTiming,
    },
}

fn main_container_status(pod: &Value) -> Result<Option<&Value>, AnsibleProbeExecutorError> {
    let statuses = pod
        .pointer("/status/containerStatuses")
        .and_then(Value::as_array)
        .ok_or(AnsibleProbeExecutorError::ObservationInvalid)?;
    let matches = statuses
        .iter()
        .filter(|status| status.pointer("/name").and_then(Value::as_str) == Some("ansible-probe"))
        .collect::<Vec<_>>();
    if matches.len() > 1 {
        return Err(AnsibleProbeExecutorError::ObservationInvalid);
    }
    Ok(matches.into_iter().next())
}

fn execution_timing(status: &Value) -> Result<ExecutionTiming, AnsibleProbeExecutorError> {
    let terminated = status.pointer("/state/terminated");
    let started = status
        .pointer("/state/terminated/startedAt")
        .and_then(Value::as_str)
        .map(parse_kubernetes_timestamp)
        .transpose()?;
    let finished = terminated
        .and_then(|value| value.pointer("/finishedAt"))
        .and_then(Value::as_str)
        .map(parse_kubernetes_timestamp)
        .transpose()?;
    if started.is_some() != finished.is_some() {
        return Ok(ExecutionTiming::unknown());
    }
    let timing = ExecutionTiming {
        started_at: started,
        terminated_at: finished,
    };
    timing
        .validate()
        .map_err(|_| AnsibleProbeExecutorError::ObservationInvalid)?;
    Ok(timing)
}

fn parse_kubernetes_timestamp(value: &str) -> Result<UtcTimestamp, AnsibleProbeExecutorError> {
    let parsed = time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
        .map_err(|_| AnsibleProbeExecutorError::ObservationInvalid)?
        .to_offset(time::UtcOffset::UTC);
    let milliseconds = parsed.nanosecond() / 1_000_000 * 1_000_000;
    let normalized = parsed
        .replace_nanosecond(milliseconds)
        .map_err(|_| AnsibleProbeExecutorError::ObservationInvalid)?;
    UtcTimestamp::from_utc(normalized).map_err(|_| AnsibleProbeExecutorError::ObservationInvalid)
}

fn verify_recovery_owned(
    resource: &Value,
    object: &crate::control_plane::EvaluationExecutionObjectRef,
    resources: &EvaluationExecutionResources,
) -> Result<(), AnsibleProbeExecutorError> {
    let metadata = resource
        .pointer("/metadata")
        .and_then(Value::as_object)
        .ok_or(AnsibleProbeExecutorError::IdentityConflict)?;
    if metadata.get("name").and_then(Value::as_str) != Some(object.name.as_str())
        || metadata.get("namespace").and_then(Value::as_str) != Some(resources.namespace.as_str())
        || metadata.get("uid").and_then(Value::as_str) != Some(object.uid.as_str())
    {
        return Err(AnsibleProbeExecutorError::IdentityConflict);
    }
    let labels = metadata
        .get("labels")
        .and_then(Value::as_object)
        .ok_or(AnsibleProbeExecutorError::IdentityConflict)?;
    let owned = [
        ("labweaver.io/managed-by", "evaluation-service".to_owned()),
        ("labweaver.io/run-id", resources.run_id.to_string()),
        (
            "labweaver.io/step-run-id",
            resources.step_run_id.to_string(),
        ),
        ("labweaver.io/attempt-id", resources.task_run_id.to_string()),
    ]
    .into_iter()
    .all(|(key, expected)| labels.get(key).and_then(Value::as_str) == Some(expected.as_str()));
    if owned {
        Ok(())
    } else {
        Err(AnsibleProbeExecutorError::IdentityConflict)
    }
}

/// The two terminal diagnostics the executor itself can assign to a failed
/// Job: the deadline mapping and the generic infrastructure fallback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AnsibleProbeTerminalFailure {
    Timeout,
    InfrastructureError,
}

impl AnsibleProbeTerminalFailure {
    const fn diagnostic_code(self) -> &'static str {
        match self {
            Self::Timeout => "LW_AP_TIMEOUT",
            Self::InfrastructureError => "LW_AP_INFRASTRUCTURE_ERROR",
        }
    }
}

fn verify_owned(
    resource: &Value,
    request: &AnsibleProbeExecutionRequest,
) -> Result<(), AnsibleProbeExecutorError> {
    let labels = resource
        .pointer("/metadata/labels")
        .and_then(Value::as_object)
        .ok_or(AnsibleProbeExecutorError::IdentityConflict)?;
    let request_sha256 = request
        .request_sha256()
        .map_err(|_| AnsibleProbeExecutorError::IdentityConflict)?
        .to_string();
    let labels_match = [
        ("labweaver.io/managed-by", "evaluation-service".to_owned()),
        ("labweaver.io/run-id", request.run_id.to_string()),
        ("labweaver.io/step-run-id", request.step_run_id.to_string()),
        ("labweaver.io/attempt-id", request.attempt_id.to_string()),
    ]
    .into_iter()
    .all(|(key, expected)| labels.get(key).and_then(Value::as_str) == Some(expected.as_str()));
    let annotation_matches = resource
        .pointer("/metadata/annotations/labweaver.io~1request-sha256")
        .and_then(Value::as_str)
        == Some(request_sha256.as_str());
    if labels_match && annotation_matches {
        Ok(())
    } else {
        Err(AnsibleProbeExecutorError::IdentityConflict)
    }
}

fn verify_immutable_identity(
    current: &Value,
    expected: &Value,
) -> Result<(), AnsibleProbeExecutorError> {
    let current_kind = current.pointer("/kind").and_then(Value::as_str);
    let expected_kind = expected.pointer("/kind").and_then(Value::as_str);
    let current_name = current.pointer("/metadata/name").and_then(Value::as_str);
    let expected_name = expected.pointer("/metadata/name").and_then(Value::as_str);
    if current_kind == expected_kind && current_name == expected_name {
        Ok(())
    } else {
        Err(AnsibleProbeExecutorError::IdentityConflict)
    }
}

fn verify_cleanup_owned(
    current: &Value,
    expected: &Value,
) -> Result<(), AnsibleProbeExecutorError> {
    verify_immutable_identity(current, expected)?;
    let current_namespace = current
        .pointer("/metadata/namespace")
        .and_then(Value::as_str);
    let expected_namespace = expected
        .pointer("/metadata/namespace")
        .and_then(Value::as_str);
    let current_labels = current
        .pointer("/metadata/labels")
        .and_then(Value::as_object)
        .ok_or(AnsibleProbeExecutorError::IdentityConflict)?;
    let expected_labels = expected
        .pointer("/metadata/labels")
        .and_then(Value::as_object)
        .ok_or(AnsibleProbeExecutorError::IdentityConflict)?;
    let labels_owned = [
        "labweaver.io/managed-by",
        "labweaver.io/run-id",
        "labweaver.io/step-run-id",
        "labweaver.io/attempt-id",
    ]
    .into_iter()
    .all(|key| current_labels.get(key) == expected_labels.get(key));
    let request_identity_owned = current
        .pointer("/metadata/annotations/labweaver.io~1request-sha256")
        == expected.pointer("/metadata/annotations/labweaver.io~1request-sha256");
    if current_namespace == expected_namespace && labels_owned && request_identity_owned {
        Ok(())
    } else {
        Err(AnsibleProbeExecutorError::IdentityConflict)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AnsibleProbeDeletePreconditions {
    uid: String,
    resource_version: String,
}

fn delete_options(
    target: &AnsibleProbeCleanupTarget,
    preconditions: &AnsibleProbeDeletePreconditions,
) -> Value {
    json!({
        "apiVersion":"v1",
        "kind":"DeleteOptions",
        "propagationPolicy":target.propagation_policy,
        "preconditions":{
            "uid":preconditions.uid,
            "resourceVersion":preconditions.resource_version,
        },
    })
}

fn delete_preconditions(
    resource: &Value,
) -> Result<AnsibleProbeDeletePreconditions, AnsibleProbeExecutorError> {
    let uid = resource
        .pointer("/metadata/uid")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(AnsibleProbeExecutorError::IdentityConflict)?;
    let resource_version = resource
        .pointer("/metadata/resourceVersion")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(AnsibleProbeExecutorError::IdentityConflict)?;
    Ok(AnsibleProbeDeletePreconditions {
        uid: uid.to_owned(),
        resource_version: resource_version.to_owned(),
    })
}

/// The permanent namespace isolation gate: a single namespace-wide policy that
/// selects every Pod and allows no ingress or egress at all. Attempt policies
/// then add exactly one egress exception. Missing or weakened isolation fails
/// closed before any attempt resource is created.
fn verify_runner_default_deny(
    policy: &Value,
    expected_namespace: &str,
) -> Result<(), AnsibleProbeExecutorError> {
    let policy_types = policy
        .pointer("/spec/policyTypes")
        .and_then(Value::as_array)
        .ok_or(AnsibleProbeExecutorError::NetworkIsolationUnavailable)?;
    let has_policy_type = |expected| {
        policy_types
            .iter()
            .filter_map(Value::as_str)
            .filter(|value| *value == expected)
            .count()
            == 1
    };
    let valid = policy.pointer("/kind").and_then(Value::as_str) == Some("NetworkPolicy")
        && policy.pointer("/metadata/name").and_then(Value::as_str)
            == Some(RUNNER_DEFAULT_DENY_POLICY)
        && policy
            .pointer("/metadata/namespace")
            .and_then(Value::as_str)
            == Some(expected_namespace)
        && policy
            .pointer("/spec/podSelector")
            .and_then(Value::as_object)
            .is_some_and(serde_json::Map::is_empty)
        && has_policy_type("Ingress")
        && has_policy_type("Egress")
        && policy_types.len() == 2
        && policy
            .pointer("/spec/ingress")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
        && policy
            .pointer("/spec/egress")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty);
    if valid {
        Ok(())
    } else {
        Err(AnsibleProbeExecutorError::NetworkIsolationUnavailable)
    }
}

fn api_prefix(api_version: &str) -> String {
    if api_version == "v1" {
        "/api/v1".to_owned()
    } else {
        format!("/apis/{api_version}")
    }
}

fn attempt_job_name(attempt_id: uuid::Uuid) -> String {
    format!("lw-ap-{}", &attempt_id.simple().to_string()[..20])
}

fn recovery_cleanup_plan(
    namespace: &str,
    request: &AnsibleProbeExecutionRequest,
) -> Vec<AnsibleProbeCleanupTarget> {
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
    .map(|(resource, name)| AnsibleProbeCleanupTarget {
        namespace: namespace.to_owned(),
        resource: resource.to_owned(),
        name,
        propagation_policy: "Foreground".to_owned(),
    })
    .collect()
}

fn api_version(
    target: &AnsibleProbeCleanupTarget,
) -> Result<&'static str, AnsibleProbeExecutorError> {
    match target.resource.as_str() {
        "jobs" => Ok("batch/v1"),
        "networkpolicies" => Ok("networking.k8s.io/v1"),
        "configmaps" | "secrets" => Ok("v1"),
        _ => Err(AnsibleProbeExecutorError::BindingInvalid),
    }
}

fn safe_segment(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.'))
}

fn is_stable_diagnostic(value: &str) -> bool {
    value.starts_with("LW_AP_")
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

fn read_bound_file(path: &Path) -> Result<Vec<u8>, AnsibleProbeExecutorError> {
    let file = fs::File::open(path).map_err(|source| {
        AnsibleProbeExecutorError::ConfigurationUnavailable {
            operation: "open",
            source,
        }
    })?;
    let metadata =
        file.metadata().map_err(
            |source| AnsibleProbeExecutorError::ConfigurationUnavailable {
                operation: "metadata",
                source,
            },
        )?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_BOUND_FILE_BYTES {
        return Err(AnsibleProbeExecutorError::ConfigurationInvalid);
    }
    let mut bytes = Vec::with_capacity(
        usize::try_from(metadata.len())
            .map_err(|_| AnsibleProbeExecutorError::ConfigurationInvalid)?,
    );
    file.take(MAX_BOUND_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(
            |source| AnsibleProbeExecutorError::ConfigurationUnavailable {
                operation: "read",
                source,
            },
        )?;
    if u64::try_from(bytes.len()).map_err(|_| AnsibleProbeExecutorError::ConfigurationInvalid)?
        != metadata.len()
        || bytes.len() as u64 > MAX_BOUND_FILE_BYTES
    {
        return Err(AnsibleProbeExecutorError::ConfigurationInvalid);
    }
    Ok(bytes)
}

fn read_bound_text(path: &Path) -> Result<String, AnsibleProbeExecutorError> {
    let bytes = read_bound_file(path)?;
    let value =
        String::from_utf8(bytes).map_err(|_| AnsibleProbeExecutorError::ConfigurationInvalid)?;
    let value = value.trim();
    if value.is_empty() || value.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return Err(AnsibleProbeExecutorError::ConfigurationInvalid);
    }
    Ok(value.to_owned())
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
        AnsibleProbeCancellationObservation, AnsibleProbeDeletePreconditions,
        AnsibleProbeExecutorError, attempt_job_name, cancellation_observation,
        classify_delete_status, delete_options, delete_preconditions, failed_container_diagnostic,
        read_bound_file, recovery_cleanup_plan, verify_cleanup_owned, verify_owned,
        verify_runner_default_deny,
    };
    #[cfg(unix)]
    use super::{AnsibleProbeExecutorConfiguration, AnsibleProbeKubernetesExecutor};
    use crate::EVALUATION_EXECUTION_RESOURCES_SCHEMA_VERSION;
    use crate::ansible_probe::{
        ANSIBLE_PROBE_EXECUTION_SCHEMA_VERSION, AnsibleProbeExecutionLimits,
        AnsibleProbeExecutionRequest, AnsibleProbeSshIdentity, AnsibleProbeTarget,
    };
    use crate::ansible_probe_job::AnsibleProbeCleanupTarget;
    use crate::control_plane::{EvaluationExecutionKind, EvaluationExecutionResources};

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

    fn request_identity(request: &AnsibleProbeExecutionRequest) -> String {
        match request.request_sha256() {
            Ok(digest) => digest.to_string(),
            Err(error) => unreachable!("fixture request identity must compute: {error}"),
        }
    }

    #[cfg(unix)]
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
            Err(AnsibleProbeExecutorError::ConfigurationInvalid)
        ));

        let oversized = directory.path().join("oversized");
        fs::write(
            &oversized,
            vec![b'x'; usize::try_from(super::MAX_BOUND_FILE_BYTES + 1)?],
        )?;
        assert!(matches!(
            read_bound_file(&oversized),
            Err(AnsibleProbeExecutorError::ConfigurationInvalid)
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
            Err(AnsibleProbeExecutorError::ConfigurationUnavailable { .. })
        ));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn authorized_reads_a_rotated_projected_token_for_each_request()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir()?;
        let first_revision = directory.path().join("..2026_09_09_00_00_00.000000001");
        let second_revision = directory.path().join("..2026_09_09_00_00_01.000000001");
        fs::create_dir(&first_revision)?;
        fs::create_dir(&second_revision)?;
        fs::write(first_revision.join("token"), b"first-token")?;
        fs::write(second_revision.join("token"), b"second-token")?;
        symlink(&first_revision, directory.path().join("..data"))?;
        let projected = directory.path().join("token");
        symlink("..data/token", &projected)?;
        let executor = AnsibleProbeKubernetesExecutor {
            configuration: AnsibleProbeExecutorConfiguration {
                kubernetes_api_server: reqwest::Url::parse("https://kubernetes.example.test/")?,
                kubernetes_bearer_token_file: projected,
                kubernetes_ca_file: directory.path().join("ca.crt"),
                runner_namespace: "labweaver-evaluation-runs".to_owned(),
                request_timeout_milliseconds: 2_000,
            },
            client: reqwest::Client::new(),
        };

        let first = executor
            .authorized(executor.client.get("https://kubernetes.example.test/"))?
            .build()?;
        assert_eq!(
            first.headers()[reqwest::header::AUTHORIZATION],
            "Bearer first-token"
        );

        fs::remove_file(directory.path().join("..data"))?;
        symlink(&second_revision, directory.path().join("..data"))?;
        let second = executor
            .authorized(executor.client.get("https://kubernetes.example.test/"))?
            .build()?;
        assert_eq!(
            second.headers()[reqwest::header::AUTHORIZATION],
            "Bearer second-token"
        );
        Ok(())
    }

    #[test]
    fn cancel_empty_intent_plan_is_attempt_scoped_and_covers_ssh_credentials()
    -> Result<(), Box<dyn std::error::Error>> {
        let request = request();
        let run_id: EvaluationRunId = request.run_id.to_string().parse()?;
        let step_run_id: EvaluationStepRunId = request.step_run_id.to_string().parse()?;
        let task_run_id: TaskRunId = request.attempt_id.to_string().parse()?;
        let intent = EvaluationExecutionResources {
            schema_version: EVALUATION_EXECUTION_RESOURCES_SCHEMA_VERSION.to_owned(),
            run_id,
            step_run_id,
            task_run_id,
            namespace: "labweaver-evaluation-runs".to_owned(),
            kind: EvaluationExecutionKind::AnsibleProbe,
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
                "namespace":"labweaver-evaluation-runs",
                "uid":"f7780f4c-e8db-4f35-82d1-bc56b5652830",
                "resourceVersion":"1042",
                "labels":{
                    "labweaver.io/managed-by":"evaluation-service",
                    "labweaver.io/run-id":request.run_id.to_string(),
                    "labweaver.io/step-run-id":request.step_run_id.to_string(),
                    "labweaver.io/attempt-id":request.attempt_id.to_string(),
                },
                "annotations":{
                    "labweaver.io/request-sha256":request_identity(request),
                },
            },
        })
    }

    #[test]
    fn request_sha256_annotation_reuses_the_same_attempt_and_conflicts_on_drift() {
        let expected = request();
        let resource = owned_resource(&expected);
        assert!(verify_owned(&resource, &expected).is_ok());

        // A second attempt identity never reuses the first attempt's resources.
        let other = request();
        assert!(matches!(
            verify_owned(&resource, &other),
            Err(AnsibleProbeExecutorError::IdentityConflict)
        ));

        let mut drifted = owned_resource(&expected);
        drifted["metadata"]["annotations"]["labweaver.io/request-sha256"] = json!("0".repeat(64));
        assert!(matches!(
            verify_owned(&drifted, &expected),
            Err(AnsibleProbeExecutorError::IdentityConflict)
        ));

        let mut drifted = owned_resource(&expected);
        drifted["metadata"]["labels"]["labweaver.io/managed-by"] = json!("other-service");
        assert!(matches!(
            verify_owned(&drifted, &expected),
            Err(AnsibleProbeExecutorError::IdentityConflict)
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
            Err(AnsibleProbeExecutorError::IdentityConflict)
        ));

        let mut drifted = owned_resource(&request);
        drifted["metadata"]["namespace"] = json!("another-namespace");
        assert!(matches!(
            verify_cleanup_owned(&drifted, &expected),
            Err(AnsibleProbeExecutorError::IdentityConflict)
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
            AnsibleProbeDeletePreconditions {
                uid: "f7780f4c-e8db-4f35-82d1-bc56b5652830".to_owned(),
                resource_version: "1042".to_owned(),
            }
        );
        let target = AnsibleProbeCleanupTarget {
            namespace: "labweaver-evaluation-runs".to_owned(),
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
            Err(AnsibleProbeExecutorError::IdentityConflict)
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
                "namespace":"labweaver-evaluation-runs",
            },
            "spec":{
                "podSelector":{},
                "policyTypes":["Ingress","Egress"],
                "ingress":[],
                "egress":[],
            },
        });
        assert!(verify_runner_default_deny(&policy, "labweaver-evaluation-runs").is_ok());

        let mut allows_egress = policy.clone();
        allows_egress["spec"]["egress"] = json!([{}]);
        assert!(matches!(
            verify_runner_default_deny(&allows_egress, "labweaver-evaluation-runs"),
            Err(AnsibleProbeExecutorError::NetworkIsolationUnavailable)
        ));

        let mut allows_dns = policy.clone();
        allows_dns["spec"]["egress"] = json!([{"ports":[{"protocol":"UDP","port":53}]}]);
        assert!(matches!(
            verify_runner_default_deny(&allows_dns, "labweaver-evaluation-runs"),
            Err(AnsibleProbeExecutorError::NetworkIsolationUnavailable)
        ));

        let mut selected_only = policy;
        selected_only["spec"]["podSelector"] =
            json!({"matchLabels":{"labweaver.io/attempt-id":"attempt"}});
        assert!(matches!(
            verify_runner_default_deny(&selected_only, "labweaver-evaluation-runs"),
            Err(AnsibleProbeExecutorError::NetworkIsolationUnavailable)
        ));
    }

    #[test]
    fn failed_containers_map_to_stable_terminal_diagnostics() {
        assert_eq!(
            failed_container_diagnostic(&json!({"reason":"OOMKilled","exitCode":137})),
            "LW_AP_INFRASTRUCTURE_ERROR"
        );
        assert_eq!(
            failed_container_diagnostic(
                &json!({"reason":"Error","message":"LW_AP_PROFILE_INVALID"})
            ),
            "LW_AP_PROFILE_INVALID"
        );
        // Untrusted pod output never becomes a stable diagnostic.
        assert_eq!(
            failed_container_diagnostic(&json!({"reason":"Error","message":"panic: segfault"})),
            "LW_AP_INFRASTRUCTURE_ERROR"
        );
        assert_eq!(
            failed_container_diagnostic(&json!({"reason":"Error"})),
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
