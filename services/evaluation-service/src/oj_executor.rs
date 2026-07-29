//! Attempt-scoped Kubernetes executor for isolated OJ resources.
#![allow(
    missing_docs,
    clippy::too_many_lines,
    reason = "the exact Kubernetes mutation and cleanup boundary is intentionally colocated"
)]

use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use reqwest::{Certificate, Client, Method, StatusCode, Url};
use serde::Deserialize;
use serde_json::{Value, json};
use thiserror::Error;

use crate::{
    oj::{OjEvidenceReceipt, OjExecutionRequest},
    oj_job::{OjCleanupTarget, OjJobBinding, OjJobError, OjJobResources},
};

const FIELD_MANAGER: &str = "labweaver-oj-executor";
const MAX_BOUND_FILE_BYTES: u64 = 64 * 1024;
const RUNNER_DEFAULT_DENY_POLICY: &str = "oj-runner-default-deny";

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
    configuration: OjExecutorConfiguration,
    client: Client,
    token: String,
}

impl OjKubernetesExecutor {
    /// Builds an HTTPS-only, CA-pinned Kubernetes executor.
    ///
    /// # Errors
    ///
    /// Returns a stable configuration error for invalid or unavailable credentials.
    pub fn new(configuration: OjExecutorConfiguration) -> Result<Self, OjExecutorError> {
        if configuration.kubernetes_api_server.scheme() != "https"
            || configuration.kubernetes_api_server.host_str().is_none()
            || configuration.runner_namespace.trim().is_empty()
            || configuration.request_timeout_milliseconds == 0
            || configuration.request_timeout_milliseconds > 60_000
        {
            return Err(OjExecutorError::ConfigurationInvalid);
        }
        let token = read_bound_text(&configuration.kubernetes_bearer_token_file)?;
        let ca = Certificate::from_pem(&read_bound_file(&configuration.kubernetes_ca_file)?)
            .map_err(|_| OjExecutorError::ConfigurationInvalid)?;
        let client = Client::builder()
            .https_only(true)
            .tls_built_in_root_certs(false)
            .add_root_certificate(ca)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_millis(
                configuration.request_timeout_milliseconds,
            ))
            .build()
            .map_err(|_| OjExecutorError::ConfigurationInvalid)?;
        Ok(Self {
            configuration,
            client,
            token,
        })
    }

    /// Applies the exact attempt-scoped network policy, command, and Job bundle.
    ///
    /// # Errors
    ///
    /// Fails closed on invalid ownership, a partial old bundle that cannot be removed, or an API
    /// rejection.
    pub async fn start(&self, binding: &OjJobBinding) -> Result<OjJobResources, OjExecutorError> {
        if binding.namespace != self.configuration.runner_namespace {
            return Err(OjExecutorError::BindingInvalid);
        }
        self.require_runner_default_deny().await?;
        let resources = OjJobResources::build(binding)?;
        let expected = [
            ("v1", "configmaps", &resources.config_map),
            (
                "networking.k8s.io/v1",
                "networkpolicies",
                &resources.network_policy,
            ),
            ("batch/v1", "jobs", &resources.job),
        ];
        let mut existing = 0_usize;
        for (api_version, plural, document) in expected {
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
                    .ok_or(OjExecutorError::IdentityConflict)?;
            }
        }
        let complete_existing_bundle = existing == expected.len();
        if existing != 0 && !complete_existing_bundle && !self.cleanup(&resources).await? {
            return Err(OjExecutorError::CleanupPending);
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
                    return Err(OjExecutorError::CleanupPending);
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
        resources: &OjJobResources,
        request: &OjExecutionRequest,
    ) -> Result<OjJobObservation, OjExecutorError> {
        let Some(job) = self
            .get(
                self.configuration.runner_namespace.as_str(),
                "batch/v1",
                "jobs",
                resources.name(),
            )
            .await?
        else {
            return Ok(OjJobObservation::Missing);
        };
        verify_owned(&job, request)?;
        let succeeded = job.pointer("/status/succeeded").and_then(Value::as_u64) == Some(1);
        let failed = job
            .pointer("/status/failed")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            > 0;
        if !succeeded && !failed {
            return Ok(OjJobObservation::Running);
        }
        let pods = self.list_pods(request).await?;
        let items = pods
            .pointer("/items")
            .and_then(Value::as_array)
            .ok_or(OjExecutorError::ObservationInvalid)?;
        if items.len() != 1 {
            return Err(OjExecutorError::ObservationInvalid);
        }
        let pod = &items[0];
        verify_owned(pod, request)?;
        let terminated = pod.pointer("/status/containerStatuses/0/state/terminated");
        if succeeded {
            let message = terminated
                .ok_or(OjExecutorError::ObservationInvalid)?
                .pointer("/message")
                .and_then(Value::as_str)
                .ok_or(OjExecutorError::ReceiptInvalid)?;
            let receipt: OjEvidenceReceipt =
                serde_json::from_str(message).map_err(|_| OjExecutorError::ReceiptInvalid)?;
            receipt
                .validate_for(request)
                .map_err(|_| OjExecutorError::ReceiptInvalid)?;
            return Ok(OjJobObservation::Completed(receipt));
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
            return Ok(OjJobObservation::Failed {
                diagnostic_code: "LW_OJ_JOB_DEADLINE_EXCEEDED".to_owned(),
            });
        }
        let terminated = terminated.ok_or(OjExecutorError::ObservationInvalid)?;
        let diagnostic_code = failed_container_diagnostic(terminated);
        Ok(OjJobObservation::Failed {
            diagnostic_code: diagnostic_code.to_owned(),
        })
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
        self.cleanup(resources).await.map(cancellation_observation)
    }

    /// Deletes and verifies absence of only the attempt-owned Job, policy, and command object.
    ///
    /// # Errors
    ///
    /// Returns a stable Kubernetes or ownership error; `Ok(false)` means deletion is still pending.
    pub async fn cleanup(&self, resources: &OjJobResources) -> Result<bool, OjExecutorError> {
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
                    .document_for(target.resource.as_str())
                    .ok_or(OjExecutorError::BindingInvalid)?;
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
                    .document_for(target.resource.as_str())
                    .ok_or(OjExecutorError::BindingInvalid)?;
                verify_cleanup_owned(&resource, expected)?;
                return Ok(false);
            }
        }
        Ok(true)
    }

    async fn require_runner_default_deny(&self) -> Result<(), OjExecutorError> {
        let policy = self
            .get(
                self.configuration.runner_namespace.as_str(),
                "networking.k8s.io/v1",
                "networkpolicies",
                RUNNER_DEFAULT_DENY_POLICY,
            )
            .await?
            .ok_or(OjExecutorError::NetworkIsolationUnavailable)?;
        verify_runner_default_deny(&policy, self.configuration.runner_namespace.as_str())
    }

    async fn apply(
        &self,
        namespace: &str,
        api_version: &str,
        plural: &str,
        name: &str,
        document: &Value,
    ) -> Result<(), OjExecutorError> {
        let response = self
            .authorized(self.client.request(
                Method::PATCH,
                self.resource_url(namespace, api_version, plural, name)?,
            ))
            .query(&[("fieldManager", FIELD_MANAGER)])
            .header("content-type", "application/apply-patch+yaml")
            .body(serde_json::to_vec(document).map_err(|_| OjExecutorError::BindingInvalid)?)
            .send()
            .await
            .map_err(|_| OjExecutorError::KubernetesUnavailable)?;
        if response.status().is_success() {
            Ok(())
        } else if response.status() == StatusCode::CONFLICT {
            Err(OjExecutorError::IdentityConflict)
        } else {
            Err(OjExecutorError::KubernetesRejected)
        }
    }

    async fn get(
        &self,
        namespace: &str,
        api_version: &str,
        plural: &str,
        name: &str,
    ) -> Result<Option<Value>, OjExecutorError> {
        let response = self
            .authorized(
                self.client
                    .get(self.resource_url(namespace, api_version, plural, name)?),
            )
            .send()
            .await
            .map_err(|_| OjExecutorError::KubernetesUnavailable)?;
        if response.status() == StatusCode::NOT_FOUND {
            Ok(None)
        } else if response.status().is_success() {
            response
                .json()
                .await
                .map(Some)
                .map_err(|_| OjExecutorError::ObservationInvalid)
        } else {
            Err(OjExecutorError::KubernetesRejected)
        }
    }

    async fn list_pods(&self, request: &OjExecutionRequest) -> Result<Value, OjExecutorError> {
        let response = self
            .authorized(self.client.get(self.collection_url(
                self.configuration.runner_namespace.as_str(),
                "v1",
                "pods",
            )?))
            .query(&[(
                "labelSelector",
                format!("labweaver.io/attempt-id={}", request.attempt_id),
            )])
            .send()
            .await
            .map_err(|_| OjExecutorError::KubernetesUnavailable)?;
        if response.status().is_success() {
            response
                .json()
                .await
                .map_err(|_| OjExecutorError::ObservationInvalid)
        } else {
            Err(OjExecutorError::KubernetesRejected)
        }
    }

    async fn delete(
        &self,
        target: &OjCleanupTarget,
        preconditions: &OjDeletePreconditions,
    ) -> Result<(), OjExecutorError> {
        let response = self
            .authorized(self.client.delete(self.resource_url(
                target.namespace.as_str(),
                api_version(target)?,
                target.resource.as_str(),
                target.name.as_str(),
            )?))
            .json(&delete_options(target, preconditions))
            .send()
            .await
            .map_err(|_| OjExecutorError::KubernetesUnavailable)?;
        classify_delete_status(response.status())
    }

    fn authorized(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        request.bearer_auth(&self.token)
    }

    fn resource_url(
        &self,
        namespace: &str,
        api_version: &str,
        plural: &str,
        name: &str,
    ) -> Result<Url, OjExecutorError> {
        if !safe_segment(namespace) || !safe_segment(plural) || !safe_segment(name) {
            return Err(OjExecutorError::BindingInvalid);
        }
        self.configuration
            .kubernetes_api_server
            .join(&format!(
                "{}/namespaces/{namespace}/{plural}/{name}",
                api_prefix(api_version)
            ))
            .map_err(|_| OjExecutorError::ConfigurationInvalid)
    }

    fn collection_url(
        &self,
        namespace: &str,
        api_version: &str,
        plural: &str,
    ) -> Result<Url, OjExecutorError> {
        if !safe_segment(namespace) || !safe_segment(plural) {
            return Err(OjExecutorError::BindingInvalid);
        }
        self.configuration
            .kubernetes_api_server
            .join(&format!(
                "{}/namespaces/{namespace}/{plural}",
                api_prefix(api_version)
            ))
            .map_err(|_| OjExecutorError::ConfigurationInvalid)
    }
}

fn classify_delete_status(status: StatusCode) -> Result<(), OjExecutorError> {
    if status.is_success() || status == StatusCode::NOT_FOUND {
        Ok(())
    } else if status == StatusCode::CONFLICT {
        Err(OjExecutorError::IdentityConflict)
    } else {
        Err(OjExecutorError::KubernetesRejected)
    }
}

fn failed_container_diagnostic(terminated: &Value) -> &str {
    if terminated.pointer("/reason").and_then(Value::as_str) == Some("OOMKilled") {
        "LW_OJ_MEMORY_LIMIT"
    } else {
        terminated
            .pointer("/message")
            .and_then(Value::as_str)
            .filter(|message| is_stable_diagnostic(message))
            .unwrap_or("LW_OJ_JOB_FAILED")
    }
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
    Completed(OjEvidenceReceipt),
    Failed { diagnostic_code: String },
}

fn verify_owned(resource: &Value, request: &OjExecutionRequest) -> Result<(), OjExecutorError> {
    let labels = resource
        .pointer("/metadata/labels")
        .and_then(Value::as_object)
        .ok_or(OjExecutorError::IdentityConflict)?;
    let request_sha256 = request
        .request_sha256()
        .map_err(|_| OjExecutorError::IdentityConflict)?
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
        Err(OjExecutorError::IdentityConflict)
    }
}

fn verify_immutable_identity(current: &Value, expected: &Value) -> Result<(), OjExecutorError> {
    let current_kind = current.pointer("/kind").and_then(Value::as_str);
    let expected_kind = expected.pointer("/kind").and_then(Value::as_str);
    let current_name = current.pointer("/metadata/name").and_then(Value::as_str);
    let expected_name = expected.pointer("/metadata/name").and_then(Value::as_str);
    if current_kind == expected_kind && current_name == expected_name {
        Ok(())
    } else {
        Err(OjExecutorError::IdentityConflict)
    }
}

fn verify_cleanup_owned(current: &Value, expected: &Value) -> Result<(), OjExecutorError> {
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
        .ok_or(OjExecutorError::IdentityConflict)?;
    let expected_labels = expected
        .pointer("/metadata/labels")
        .and_then(Value::as_object)
        .ok_or(OjExecutorError::IdentityConflict)?;
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
        Err(OjExecutorError::IdentityConflict)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct OjDeletePreconditions {
    uid: String,
    resource_version: String,
}

fn delete_options(target: &OjCleanupTarget, preconditions: &OjDeletePreconditions) -> Value {
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

fn delete_preconditions(resource: &Value) -> Result<OjDeletePreconditions, OjExecutorError> {
    let uid = resource
        .pointer("/metadata/uid")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(OjExecutorError::IdentityConflict)?;
    let resource_version = resource
        .pointer("/metadata/resourceVersion")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(OjExecutorError::IdentityConflict)?;
    Ok(OjDeletePreconditions {
        uid: uid.to_owned(),
        resource_version: resource_version.to_owned(),
    })
}

fn verify_runner_default_deny(
    policy: &Value,
    expected_namespace: &str,
) -> Result<(), OjExecutorError> {
    let policy_types = policy
        .pointer("/spec/policyTypes")
        .and_then(Value::as_array)
        .ok_or(OjExecutorError::NetworkIsolationUnavailable)?;
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
        Err(OjExecutorError::NetworkIsolationUnavailable)
    }
}

fn api_prefix(api_version: &str) -> String {
    if api_version == "v1" {
        "/api/v1".to_owned()
    } else {
        format!("/apis/{api_version}")
    }
}

fn api_version(target: &OjCleanupTarget) -> Result<&'static str, OjExecutorError> {
    match target.resource.as_str() {
        "jobs" => Ok("batch/v1"),
        "networkpolicies" => Ok("networking.k8s.io/v1"),
        "configmaps" => Ok("v1"),
        _ => Err(OjExecutorError::BindingInvalid),
    }
}

fn safe_segment(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.'))
}

fn is_stable_diagnostic(value: &str) -> bool {
    value.starts_with("LW_OJ_")
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

fn read_bound_file(path: &Path) -> Result<Vec<u8>, OjExecutorError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| OjExecutorError::ConfigurationUnavailable)?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() == 0
        || metadata.len() > MAX_BOUND_FILE_BYTES
    {
        return Err(OjExecutorError::ConfigurationInvalid);
    }
    let bytes = fs::read(path).map_err(|_| OjExecutorError::ConfigurationUnavailable)?;
    if u64::try_from(bytes.len()).map_err(|_| OjExecutorError::ConfigurationInvalid)?
        != metadata.len()
    {
        return Err(OjExecutorError::ConfigurationInvalid);
    }
    Ok(bytes)
}

fn read_bound_text(path: &Path) -> Result<String, OjExecutorError> {
    let bytes = read_bound_file(path)?;
    let value = String::from_utf8(bytes).map_err(|_| OjExecutorError::ConfigurationInvalid)?;
    let value = value.trim();
    if value.is_empty() || value.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return Err(OjExecutorError::ConfigurationInvalid);
    }
    Ok(value.to_owned())
}

#[derive(Debug, Error)]
pub enum OjExecutorError {
    #[error("OJ executor configuration is unavailable")]
    ConfigurationUnavailable,
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

impl OjExecutorError {
    #[must_use]
    pub const fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::ConfigurationUnavailable => "LW_OJ_EXECUTOR_CONFIG_UNAVAILABLE",
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
    use reqwest::StatusCode;
    use serde_json::json;

    use super::{
        OjCancellationObservation, OjDeletePreconditions, OjExecutorError,
        cancellation_observation, classify_delete_status, delete_options, delete_preconditions,
        failed_container_diagnostic, verify_cleanup_owned, verify_runner_default_deny,
    };
    use crate::oj_job::OjCleanupTarget;

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
            Err(OjExecutorError::IdentityConflict)
        ));

        let mut drifted = resource();
        drifted["metadata"]["namespace"] = json!("another-namespace");
        assert!(matches!(
            verify_cleanup_owned(&drifted, &expected),
            Err(OjExecutorError::IdentityConflict)
        ));
    }

    #[test]
    fn cleanup_delete_is_bound_to_the_observed_resource_identity()
    -> Result<(), Box<dyn std::error::Error>> {
        let current = resource();
        let preconditions = delete_preconditions(&current)?;
        assert_eq!(
            preconditions,
            OjDeletePreconditions {
                uid: "f7780f4c-e8db-4f35-82d1-bc56b5652830".to_owned(),
                resource_version: "1042".to_owned(),
            }
        );
        let target = OjCleanupTarget {
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
            Err(OjExecutorError::IdentityConflict)
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
        assert!(verify_runner_default_deny(&policy, "labweaver-evaluation-runs").is_ok());

        let mut allows_egress = policy.clone();
        allows_egress["spec"]["egress"] = json!([{}]);
        assert!(matches!(
            verify_runner_default_deny(&allows_egress, "labweaver-evaluation-runs"),
            Err(OjExecutorError::NetworkIsolationUnavailable)
        ));

        let mut selected_only = policy;
        selected_only["spec"]["podSelector"] =
            json!({"matchLabels":{"labweaver.io/attempt-id":"attempt"}});
        assert!(matches!(
            verify_runner_default_deny(&selected_only, "labweaver-evaluation-runs"),
            Err(OjExecutorError::NetworkIsolationUnavailable)
        ));
    }

    #[test]
    fn pod_oom_and_explicit_cancel_have_distinct_terminal_outcomes() {
        assert_eq!(
            failed_container_diagnostic(&json!({"reason":"OOMKilled","exitCode":137})),
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
