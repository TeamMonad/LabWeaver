//! Attempt-scoped Kubernetes executor for isolated OJ resources.
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
    control_plane::EvaluationExecutionResources,
    execution::ExecutionTiming,
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
        read_bound_text(&configuration.kubernetes_bearer_token_file)?;
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
            ("v1", "secrets", &resources.materializer_secret),
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
                "v1",
                "secrets",
                resources.materializer_secret_name(),
                &resources.materializer_secret,
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
        self.observe_job(resources.name(), None, request).await
    }

    /// Observes a recovered attempt using only its durable object references.
    ///
    /// The rendered Job bundle is intentionally unavailable on this path: it
    /// contains signed materializer URLs and is never reconstructed for
    /// cleanup.  The persisted UID is checked before accepting any status.
    ///
    /// # Errors
    ///
    /// Returns an error when the recovered resource identity or observed
    /// Kubernetes state is invalid or unavailable.
    pub async fn observe_recovery(
        &self,
        resources: &EvaluationExecutionResources,
        request: &OjExecutionRequest,
    ) -> Result<OjJobObservation, OjExecutorError> {
        if resources.namespace != self.configuration.runner_namespace
            || resources.kind != crate::control_plane::EvaluationExecutionKind::Program
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
        let Some(job) = self
            .get(
                self.configuration.runner_namespace.as_str(),
                "batch/v1",
                "jobs",
                job_name,
            )
            .await?
        else {
            return Ok(OjJobObservation::Missing);
        };
        if expected_uid
            .is_some_and(|uid| job.pointer("/metadata/uid").and_then(Value::as_str) != Some(uid))
        {
            return Err(OjExecutorError::IdentityConflict);
        }
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
        let container = main_container_status(pod)?;
        let timing = container
            .map(execution_timing)
            .transpose()?
            .unwrap_or_else(ExecutionTiming::unknown);
        let terminated = container.and_then(|status| status.pointer("/state/terminated"));
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
            return Ok(OjJobObservation::Completed { receipt, timing });
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
                timing,
            });
        }
        let diagnostic_code = terminated.map_or("LW_OJ_JOB_FAILED", failed_container_diagnostic);
        Ok(OjJobObservation::Failed {
            diagnostic_code: diagnostic_code.to_owned(),
            timing,
        })
    }

    /// Captures the immutable object identities after the bundle is applied.
    ///
    /// The caller persists these references before observing the Job.  UID and
    /// ownership checks make a later recovery unable to delete a replacement
    /// object with the same name.
    ///
    /// # Errors
    ///
    /// Returns an error when an expected object is unavailable, not owned by
    /// this attempt, or has an invalid identity.
    pub async fn capture_object_refs(
        &self,
        resources: &OjJobResources,
        request: &OjExecutionRequest,
    ) -> Result<Vec<(String, String, String, String)>, OjExecutorError> {
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
                .ok_or(OjExecutorError::ObservationInvalid)?;
            verify_owned(&current, request)?;
            let uid = current
                .pointer("/metadata/uid")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or(OjExecutorError::IdentityConflict)?;
            refs.push((api_version, target.resource, target.name, uid.to_owned()));
        }
        Ok(refs)
    }

    /// Captures a pending intent's object identities without rebuilding the
    /// materializer command.  A complete bundle is promoted to a normal
    /// recovery checkpoint; an absent or partial bundle returns `None` so the
    /// caller can clean the deterministic names before retrying the same
    /// attempt.
    ///
    /// # Errors
    ///
    /// Returns an error when the pending request or any observed object fails
    /// identity or ownership validation.
    pub async fn capture_intent_object_refs(
        &self,
        resources: &EvaluationExecutionResources,
        request: &OjExecutionRequest,
    ) -> Result<Option<Vec<(String, String, String, String)>>, OjExecutorError> {
        if resources.namespace != self.configuration.runner_namespace
            || resources.kind != crate::control_plane::EvaluationExecutionKind::Program
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
                .ok_or(OjExecutorError::IdentityConflict)?;
            refs.push((
                api_version,
                target.resource.clone(),
                target.name.clone(),
                uid.to_owned(),
            ));
        }
        refs.sort_by(|left, right| {
            left.1
                .cmp(&right.1)
                .then_with(|| left.2.cmp(&right.2))
                .then_with(|| left.0.cmp(&right.0))
        });
        Ok((!refs.is_empty()).then_some(refs))
    }

    /// Deletes and verifies a recovered attempt from persisted references.
    ///
    /// # Errors
    ///
    /// Returns an error when the recovered identity is invalid or Kubernetes
    /// cannot delete or verify one of the owned objects.
    pub async fn cleanup_recovery(
        &self,
        resources: &EvaluationExecutionResources,
    ) -> Result<bool, OjExecutorError> {
        if resources.namespace != self.configuration.runner_namespace
            || resources.kind != crate::control_plane::EvaluationExecutionKind::Program
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
            return self
                .cleanup_intent(resources.namespace.as_str(), &request)
                .await;
        }
        for object in &resources.objects {
            let target = OjCleanupTarget {
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
        request: &OjExecutionRequest,
    ) -> Result<bool, OjExecutorError> {
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
            ))?
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
            .authorized(self.client.get(self.resource_url(
                namespace,
                api_version,
                plural,
                name,
            )?))?
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
            )?))?
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
            )?))?
            .json(&delete_options(target, preconditions))
            .send()
            .await
            .map_err(|_| OjExecutorError::KubernetesUnavailable)?;
        classify_delete_status(response.status())
    }

    fn authorized(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::RequestBuilder, OjExecutorError> {
        let token = read_bound_text(&self.configuration.kubernetes_bearer_token_file)?;
        Ok(request.bearer_auth(token))
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
    Completed {
        receipt: OjEvidenceReceipt,
        timing: ExecutionTiming,
    },
    Failed {
        diagnostic_code: String,
        timing: ExecutionTiming,
    },
}

fn main_container_status(pod: &Value) -> Result<Option<&Value>, OjExecutorError> {
    let statuses = pod
        .pointer("/status/containerStatuses")
        .and_then(Value::as_array)
        .ok_or(OjExecutorError::ObservationInvalid)?;
    let matches = statuses
        .iter()
        .filter(|status| status.pointer("/name").and_then(Value::as_str) == Some("program-runner"))
        .collect::<Vec<_>>();
    if matches.len() > 1 {
        return Err(OjExecutorError::ObservationInvalid);
    }
    Ok(matches.into_iter().next())
}

fn execution_timing(status: &Value) -> Result<ExecutionTiming, OjExecutorError> {
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
        .map_err(|_| OjExecutorError::ObservationInvalid)?;
    Ok(timing)
}

fn parse_kubernetes_timestamp(value: &str) -> Result<UtcTimestamp, OjExecutorError> {
    let parsed = time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
        .map_err(|_| OjExecutorError::ObservationInvalid)?
        .to_offset(time::UtcOffset::UTC);
    let milliseconds = parsed.nanosecond() / 1_000_000 * 1_000_000;
    let normalized = parsed
        .replace_nanosecond(milliseconds)
        .map_err(|_| OjExecutorError::ObservationInvalid)?;
    UtcTimestamp::from_utc(normalized).map_err(|_| OjExecutorError::ObservationInvalid)
}

fn verify_recovery_owned(
    resource: &Value,
    object: &crate::control_plane::EvaluationExecutionObjectRef,
    resources: &EvaluationExecutionResources,
) -> Result<(), OjExecutorError> {
    let metadata = resource
        .pointer("/metadata")
        .and_then(Value::as_object)
        .ok_or(OjExecutorError::IdentityConflict)?;
    if metadata.get("name").and_then(Value::as_str) != Some(object.name.as_str())
        || metadata.get("namespace").and_then(Value::as_str) != Some(resources.namespace.as_str())
        || metadata.get("uid").and_then(Value::as_str) != Some(object.uid.as_str())
    {
        return Err(OjExecutorError::IdentityConflict);
    }
    let labels = metadata
        .get("labels")
        .and_then(Value::as_object)
        .ok_or(OjExecutorError::IdentityConflict)?;
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
        Err(OjExecutorError::IdentityConflict)
    }
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

fn attempt_job_name(attempt_id: uuid::Uuid) -> String {
    format!("lw-oj-{}", &attempt_id.simple().to_string()[..20])
}

fn recovery_cleanup_plan(namespace: &str, request: &OjExecutionRequest) -> Vec<OjCleanupTarget> {
    let name = attempt_job_name(request.attempt_id);
    let materializer = format!("{name}-materializer");
    [
        ("jobs", name.clone()),
        ("networkpolicies", name.clone()),
        ("configmaps", name),
        ("secrets", materializer),
    ]
    .into_iter()
    .map(|(resource, name)| OjCleanupTarget {
        namespace: namespace.to_owned(),
        resource: resource.to_owned(),
        name,
        propagation_policy: "Foreground".to_owned(),
    })
    .collect()
}

fn api_version(target: &OjCleanupTarget) -> Result<&'static str, OjExecutorError> {
    match target.resource.as_str() {
        "jobs" => Ok("batch/v1"),
        "networkpolicies" => Ok("networking.k8s.io/v1"),
        "configmaps" | "secrets" => Ok("v1"),
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
    let file =
        fs::File::open(path).map_err(|source| OjExecutorError::ConfigurationUnavailable {
            operation: "open",
            source,
        })?;
    let metadata = file
        .metadata()
        .map_err(|source| OjExecutorError::ConfigurationUnavailable {
            operation: "metadata",
            source,
        })?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_BOUND_FILE_BYTES {
        return Err(OjExecutorError::ConfigurationInvalid);
    }
    let mut bytes = Vec::with_capacity(
        usize::try_from(metadata.len()).map_err(|_| OjExecutorError::ConfigurationInvalid)?,
    );
    file.take(MAX_BOUND_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| OjExecutorError::ConfigurationUnavailable {
            operation: "read",
            source,
        })?;
    if u64::try_from(bytes.len()).map_err(|_| OjExecutorError::ConfigurationInvalid)?
        != metadata.len()
        || bytes.len() as u64 > MAX_BOUND_FILE_BYTES
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

impl OjExecutorError {
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

    use super::{
        OjCancellationObservation, OjDeletePreconditions, OjExecutorConfiguration, OjExecutorError,
        OjJobObservation, OjKubernetesExecutor, api_version, attempt_job_name,
        cancellation_observation, classify_delete_status, delete_options, delete_preconditions,
        failed_container_diagnostic, read_bound_file, recovery_cleanup_plan, verify_cleanup_owned,
        verify_runner_default_deny,
    };
    use crate::oj_job::OjCleanupTarget;
    use crate::{
        EVALUATION_EXECUTION_RESOURCES_SCHEMA_VERSION,
        control_plane::{
            EvaluationExecutionKind, EvaluationExecutionObjectRef, EvaluationExecutionResources,
        },
        oj::{
            OJ_EXECUTION_SCHEMA_VERSION, OjExecutionLimits, OjExecutionPhase, OjExecutionRequest,
            OjFileBinding,
        },
    };
    use contracts::{EvaluationRunId, EvaluationStepRunId, TaskRunId};
    use persistence_sqlx::Sha256Digest;
    use reqwest::{Client, StatusCode, Url};
    use serde_json::json;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        task::JoinHandle,
    };

    const TEST_NAMESPACE: &str = "labweaver-evaluation-runs";

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
            Err(OjExecutorError::ConfigurationInvalid)
        ));

        let oversized = directory.path().join("oversized");
        fs::write(
            &oversized,
            vec![b'x'; usize::try_from(super::MAX_BOUND_FILE_BYTES + 1)?],
        )?;
        assert!(matches!(
            read_bound_file(&oversized),
            Err(OjExecutorError::ConfigurationInvalid)
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
            Err(OjExecutorError::ConfigurationUnavailable { .. })
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
        let executor = OjKubernetesExecutor {
            configuration: OjExecutorConfiguration {
                kubernetes_api_server: Url::parse("https://kubernetes.example.test/")?,
                kubernetes_bearer_token_file: projected,
                kubernetes_ca_file: directory.path().join("ca.crt"),
                runner_namespace: TEST_NAMESPACE.to_owned(),
                request_timeout_milliseconds: 2_000,
            },
            client: Client::new(),
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

    struct FakeExecutor {
        executor: OjKubernetesExecutor,
        objects: Arc<Mutex<BTreeMap<String, serde_json::Value>>>,
        server: JoinHandle<()>,
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
        let intent = EvaluationExecutionResources {
            schema_version: EVALUATION_EXECUTION_RESOURCES_SCHEMA_VERSION.to_owned(),
            run_id,
            step_run_id,
            task_run_id,
            namespace: TEST_NAMESPACE.to_owned(),
            kind: EvaluationExecutionKind::Program,
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
    ) -> std::io::Result<()> {
        let mut request = Vec::new();
        let mut chunk = [0_u8; 4 * 1024];
        loop {
            let read = stream.read(&mut chunk).await?;
            if read == 0 {
                return Ok(());
            }
            request.extend_from_slice(&chunk[..read]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
            if request.len() > 64 * 1024 {
                return fake_response(&mut stream, 413, &json!({})).await;
            }
        }
        let Some(line_end) = request.windows(2).position(|window| window == b"\r\n") else {
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
                let removed = objects
                    .lock()
                    .map_err(|_| std::io::Error::other("fake object state poisoned"))?
                    .remove(path)
                    .is_some();
                if removed {
                    (200, json!({}))
                } else {
                    (404, json!({}))
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
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let address = listener.local_addr()?;
        let objects = Arc::new(Mutex::new(objects));
        let server_objects = Arc::clone(&objects);
        let server = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let connection_objects = Arc::clone(&server_objects);
                tokio::spawn(async move {
                    let _ = fake_connection(stream, connection_objects).await;
                });
            }
        });
        let client = Client::builder().no_proxy().build()?;
        let executor = OjKubernetesExecutor {
            configuration: OjExecutorConfiguration {
                kubernetes_api_server: Url::parse(&format!("http://{address}/"))?,
                kubernetes_bearer_token_file: PathBuf::from("unused-token"),
                kubernetes_ca_file: PathBuf::from("unused-ca"),
                runner_namespace: TEST_NAMESPACE.to_owned(),
                request_timeout_milliseconds: 2_000,
            },
            client,
        };
        Ok(FakeExecutor {
            executor,
            objects,
            server,
        })
    }

    fn hydrated(
        intent: &EvaluationExecutionResources,
        refs: Vec<(String, String, String, String)>,
    ) -> EvaluationExecutionResources {
        EvaluationExecutionResources {
            objects: refs
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
            ..intent.clone()
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
        assert_eq!(refs[0].1, "configmaps");
        assert_eq!(refs[1].1, "networkpolicies");
        assert_eq!(refs[2].1, "secrets");
        let recovered = hydrated(&intent, refs);
        recovered.validate_for(intent.run_id, intent.step_run_id, intent.task_run_id)?;

        // No Job means a terminal missing-job outcome; recovery must not
        // recreate the Job or poll indefinitely.
        assert!(matches!(
            fake.executor.observe_recovery(&recovered, &request).await?,
            OjJobObservation::Missing
        ));
        assert!(fake.executor.cleanup_recovery(&recovered).await?);
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
        assert!(cleanup_complete);
        assert_eq!(
            cancellation_observation(cleanup_complete),
            OjCancellationObservation::Cancelled
        );
        // A replay is finite and idempotent; it never starts a new Job.
        assert!(fake.executor.cleanup_recovery(&intent).await?);
        assert!(
            fake.objects
                .lock()
                .map_err(|_| "fake object state poisoned")?
                .is_empty()
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
