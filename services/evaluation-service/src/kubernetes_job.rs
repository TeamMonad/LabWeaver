//! Shared Kubernetes Job execution mechanics for Evaluation one-shot workloads.
//!
//! This module owns the exact REST mutations, ownership verification, observation, and
//! deterministic cleanup that every Evaluation one-shot execution backend shares. Role-specific
//! document rendering and evidence receipt parsing stay in the role executor; this module never
//! interprets a score, a probe fact, or an Agent result.
#![allow(
    missing_docs,
    clippy::missing_errors_doc,
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
use contracts::execution::{
    ExecutionCleanupStatus, ExecutionObjectRef, ExecutionObservation, ExecutionWorkloadState,
};
use reqwest::{Certificate, Client, Method, StatusCode, Url};
use serde_json::{Value, json};
use thiserror::Error;
use uuid::Uuid;

use crate::execution::ExecutionTiming;

/// Maximum size of a bound projected token or CA file.
pub const MAX_BOUND_FILE_BYTES: u64 = 64 * 1024;

const MANAGED_BY_LABEL: &str = "labweaver.io/managed-by";
const MANAGED_BY_VALUE: &str = "evaluation-service";
const RUN_ID_LABEL: &str = "labweaver.io/run-id";
const STEP_RUN_ID_LABEL: &str = "labweaver.io/step-run-id";
const ATTEMPT_ID_LABEL: &str = "labweaver.io/attempt-id";
const MAX_DELETE_CONFLICT_RETRIES: usize = 2;

/// Connection configuration shared by every Evaluation Kubernetes execution backend.
#[derive(Clone, Debug)]
pub struct KubernetesApiConfiguration {
    pub kubernetes_api_server: Url,
    pub kubernetes_bearer_token_file: PathBuf,
    pub kubernetes_ca_file: PathBuf,
    pub runner_namespace: String,
    pub request_timeout_milliseconds: u64,
}

/// Role-neutral attempt ownership used by every Kubernetes ownership check.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KubernetesOwnership {
    pub run_id: Uuid,
    pub step_run_id: Uuid,
    pub attempt_id: Uuid,
    pub request_sha256: String,
}

/// Durable, role-neutral identity of one admitted Kubernetes workload.
#[derive(Clone, Debug)]
pub struct KubernetesJobIdentity {
    pub namespace: String,
    pub job_name: String,
    pub main_container: &'static str,
    pub default_deny_policy: &'static str,
    pub deadline_diagnostic_code: &'static str,
    pub failed_diagnostic_code: &'static str,
    pub oom_diagnostic_code: &'static str,
    pub stable_diagnostic_prefix: &'static str,
    pub ownership: KubernetesOwnership,
    pub trace_id: String,
}

/// One immutable Kubernetes object in the exact apply order.
#[derive(Clone, Debug)]
pub struct KubernetesObject {
    pub api_version: &'static str,
    pub plural: &'static str,
    pub name: String,
    pub document: Value,
}

/// One deterministic cleanup target. Deletion always uses UID and resource-version preconditions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KubernetesCleanupTarget {
    pub namespace: String,
    pub resource: String,
    pub name: String,
    pub propagation_policy: String,
}

/// Complete immutable description of one attempt-scoped workload bundle.
#[derive(Clone, Debug)]
pub struct KubernetesJobBundle {
    pub identity: KubernetesJobIdentity,
    pub objects: Vec<KubernetesObject>,
    pub cleanup_plan: Vec<KubernetesCleanupTarget>,
}

fn document_for<'a>(
    objects: &'a [KubernetesObject],
    target: &KubernetesCleanupTarget,
) -> Option<&'a Value> {
    objects
        .iter()
        .find(|object| object.plural == target.resource && object.name == target.name)
        .map(|object| &object.document)
}

/// Bounded, payload-free observation returned by the shared Job observation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum KubernetesJobObservation {
    Missing,
    Running,
    Completed {
        message: String,
        observation: ExecutionObservation,
    },
    Failed {
        diagnostic_code: String,
        observation: ExecutionObservation,
    },
}

/// Failure returned by the shared Kubernetes execution mechanics.
#[derive(Debug, Error)]
pub enum KubernetesJobError {
    #[error("Kubernetes API configuration is unavailable during {operation}: {source}")]
    ConfigurationUnavailable {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("Kubernetes API configuration is invalid")]
    ConfigurationInvalid,
    #[error("Kubernetes Job binding is invalid")]
    BindingInvalid,
    #[error("Kubernetes API is unavailable")]
    KubernetesUnavailable,
    #[error("Kubernetes API rejected the operation")]
    KubernetesRejected,
    #[error("runner namespace default-deny isolation is unavailable")]
    NetworkIsolationUnavailable,
    #[error("attempt identity conflicts with an existing resource")]
    IdentityConflict,
    #[error("Job cleanup is pending")]
    CleanupPending,
    #[error("Job observation is invalid")]
    ObservationInvalid,
    #[error("evidence receipt is invalid")]
    ReceiptInvalid,
}

impl KubernetesJobError {
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
        }
    }
}

/// HTTPS-only, CA-pinned Kubernetes REST client shared by the role executors.
#[derive(Clone)]
pub struct KubernetesApiClient {
    configuration: KubernetesApiConfiguration,
    client: Client,
    field_manager: &'static str,
    log_scope: &'static str,
    diagnostic_prefix: &'static str,
}

impl KubernetesApiClient {
    /// Builds the shared client from validated connection configuration.
    ///
    /// # Errors
    ///
    /// Returns a stable configuration error for invalid or unavailable credentials.
    pub fn new(
        configuration: KubernetesApiConfiguration,
        field_manager: &'static str,
        log_scope: &'static str,
        diagnostic_prefix: &'static str,
    ) -> Result<Self, KubernetesJobError> {
        if configuration.kubernetes_api_server.scheme() != "https"
            || configuration.kubernetes_api_server.host_str().is_none()
            || configuration.runner_namespace.trim().is_empty()
            || configuration.request_timeout_milliseconds == 0
            || configuration.request_timeout_milliseconds > 60_000
        {
            return Err(KubernetesJobError::ConfigurationInvalid);
        }
        read_bound_text(&configuration.kubernetes_bearer_token_file)?;
        let ca = Certificate::from_pem(&read_bound_file(&configuration.kubernetes_ca_file)?)
            .map_err(|_| KubernetesJobError::ConfigurationInvalid)?;
        let client = Client::builder()
            .https_only(true)
            .tls_built_in_root_certs(false)
            .add_root_certificate(ca)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_millis(
                configuration.request_timeout_milliseconds,
            ))
            .build()
            .map_err(|_| KubernetesJobError::ConfigurationInvalid)?;
        Ok(Self {
            configuration,
            client,
            field_manager,
            log_scope,
            diagnostic_prefix,
        })
    }

    /// Returns the namespace this backend is bound to.
    #[must_use]
    pub fn runner_namespace(&self) -> &str {
        self.configuration.runner_namespace.as_str()
    }

    /// Builds a client around an already-constructed HTTP client for local tests.
    #[cfg(test)]
    pub(crate) fn for_test(
        configuration: KubernetesApiConfiguration,
        client: Client,
        field_manager: &'static str,
        log_scope: &'static str,
        diagnostic_prefix: &'static str,
    ) -> Self {
        Self {
            configuration,
            client,
            field_manager,
            log_scope,
            diagnostic_prefix,
        }
    }

    /// Applies the exact attempt-scoped bundle, or reuses the complete existing one.
    ///
    /// # Errors
    ///
    /// Fails closed on invalid ownership, a partial old bundle that cannot be removed, or an API
    /// rejection.
    pub async fn start(&self, bundle: &KubernetesJobBundle) -> Result<(), KubernetesJobError> {
        self.ensure_binding(&bundle.identity)?;
        self.require_runner_default_deny(&bundle.identity).await?;
        let mut existing = 0_usize;
        for object in &bundle.objects {
            if let Some(current) = self
                .get(
                    bundle.identity.namespace.as_str(),
                    object.api_version,
                    object.plural,
                    object.name.as_str(),
                )
                .await?
            {
                verify_owned(&current, &bundle.identity.ownership)?;
                verify_immutable_identity(&current, &object.document)?;
                existing = existing
                    .checked_add(1)
                    .ok_or(KubernetesJobError::IdentityConflict)?;
            }
        }
        let complete_existing_bundle = existing == bundle.objects.len();
        if existing != 0
            && !complete_existing_bundle
            && !self
                .cleanup(
                    &bundle.identity.namespace,
                    &bundle.identity.job_name,
                    &bundle.objects,
                    &bundle.cleanup_plan,
                )
                .await?
                .is_confirmed()
        {
            return Err(KubernetesJobError::CleanupPending);
        }

        let apply_result = async {
            for object in &bundle.objects {
                self.apply(
                    bundle.identity.namespace.as_str(),
                    object.api_version,
                    object.plural,
                    object.name.as_str(),
                    &object.document,
                )
                .await?;
            }
            Ok(())
        }
        .await;
        if let Err(error) = apply_result {
            if !complete_existing_bundle {
                let cleanup = self
                    .cleanup(
                        &bundle.identity.namespace,
                        &bundle.identity.job_name,
                        &bundle.objects,
                        &bundle.cleanup_plan,
                    )
                    .await;
                if !matches!(cleanup, Ok(status) if status.is_confirmed()) {
                    return Err(KubernetesJobError::CleanupPending);
                }
            }
            return Err(error);
        }
        Ok(())
    }

    /// Captures the immutable object identities after the bundle is applied.
    ///
    /// # Errors
    ///
    /// Returns an error when an expected object is unavailable, not owned by this attempt, or has
    /// an invalid identity.
    pub async fn capture_object_refs(
        &self,
        identity: &KubernetesJobIdentity,
        cleanup_plan: &[KubernetesCleanupTarget],
    ) -> Result<Vec<ExecutionObjectRef>, KubernetesJobError> {
        self.ensure_binding(identity)?;
        let mut refs = Vec::with_capacity(cleanup_plan.len());
        for target in cleanup_plan {
            let current = self
                .get(
                    target.namespace.as_str(),
                    api_version(target)?,
                    target.resource.as_str(),
                    target.name.as_str(),
                )
                .await?
                .ok_or(KubernetesJobError::ObservationInvalid)?;
            verify_owned(&current, &identity.ownership)?;
            refs.push(object_ref(api_version(target)?, target, &current)?);
        }
        Ok(refs)
    }

    /// Captures a pending intent's object identities without rebuilding the bundle.
    ///
    /// # Errors
    ///
    /// Returns an error when the pending identity or any observed object fails validation.
    pub async fn capture_intent_object_refs(
        &self,
        identity: &KubernetesJobIdentity,
        targets: &[KubernetesCleanupTarget],
    ) -> Result<Option<Vec<ExecutionObjectRef>>, KubernetesJobError> {
        self.ensure_binding(identity)?;
        let mut refs = Vec::with_capacity(targets.len());
        for target in targets {
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
            verify_owned(&current, &identity.ownership)?;
            refs.push(object_ref(api_version(target)?, target, &current)?);
        }
        refs.sort_by(|left, right| {
            left.resource
                .cmp(&right.resource)
                .then_with(|| left.name.cmp(&right.name))
                .then_with(|| left.api_version.cmp(&right.api_version))
        });
        Ok((!refs.is_empty()).then_some(refs))
    }

    /// Observes the exact Job and its single owned Pod without accepting ambiguous evidence.
    ///
    /// # Errors
    ///
    /// Returns a stable error when Kubernetes is unavailable or ownership/evidence is invalid.
    pub async fn observe(
        &self,
        identity: &KubernetesJobIdentity,
        expected_uid: Option<&str>,
    ) -> Result<KubernetesJobObservation, KubernetesJobError> {
        self.ensure_binding(identity)?;
        let Some(job) = self
            .get(
                identity.namespace.as_str(),
                "batch/v1",
                "jobs",
                identity.job_name.as_str(),
            )
            .await?
        else {
            return Ok(KubernetesJobObservation::Missing);
        };
        if expected_uid
            .is_some_and(|uid| job.pointer("/metadata/uid").and_then(Value::as_str) != Some(uid))
        {
            return Err(KubernetesJobError::IdentityConflict);
        }
        verify_owned(&job, &identity.ownership)?;
        let succeeded = job.pointer("/status/succeeded").and_then(Value::as_u64) == Some(1);
        let failed = job
            .pointer("/status/failed")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            > 0;
        if !succeeded && !failed {
            return Ok(KubernetesJobObservation::Running);
        }
        let pods = self
            .list_pods(identity.namespace.as_str(), identity.ownership.attempt_id)
            .await?;
        let items = pods
            .pointer("/items")
            .and_then(Value::as_array)
            .ok_or(KubernetesJobError::ObservationInvalid)?;
        if items.len() != 1 {
            return Err(KubernetesJobError::ObservationInvalid);
        }
        let pod = &items[0];
        verify_owned(pod, &identity.ownership)?;
        let container = main_container_status(pod, identity.main_container)?;
        let timing = container
            .map(execution_timing)
            .transpose()?
            .unwrap_or_else(ExecutionTiming::unknown);
        let terminated = container.and_then(|status| status.pointer("/state/terminated"));
        let pod_name = pod
            .pointer("/metadata/name")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if succeeded {
            let message = terminated
                .ok_or(KubernetesJobError::ObservationInvalid)?
                .pointer("/message")
                .and_then(Value::as_str)
                .ok_or(KubernetesJobError::ReceiptInvalid)?;
            let observation = build_observation(
                ExecutionWorkloadState::Succeeded,
                terminated,
                pod_name,
                timing,
            )?;
            return Ok(KubernetesJobObservation::Completed {
                message: message.to_owned(),
                observation,
            });
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
        let observation =
            build_observation(ExecutionWorkloadState::Failed, terminated, pod_name, timing)?;
        if job_reason == Some("DeadlineExceeded") {
            return Ok(KubernetesJobObservation::Failed {
                diagnostic_code: identity.deadline_diagnostic_code.to_owned(),
                observation,
            });
        }
        let diagnostic_code = terminated.map_or(identity.failed_diagnostic_code, |terminated| {
            failed_container_diagnostic(terminated, identity)
        });
        Ok(KubernetesJobObservation::Failed {
            diagnostic_code: diagnostic_code.to_owned(),
            observation,
        })
    }

    /// Deletes and verifies absence of only the attempt-owned objects.
    ///
    /// # Errors
    ///
    /// Returns a stable Kubernetes or ownership error; [`ExecutionCleanupStatus::Pending`] means
    /// deletion is still pending.
    pub async fn cleanup(
        &self,
        namespace: &str,
        job_name: &str,
        objects: &[KubernetesObject],
        cleanup_plan: &[KubernetesCleanupTarget],
    ) -> Result<ExecutionCleanupStatus, KubernetesJobError> {
        if namespace != self.configuration.runner_namespace || !safe_segment(job_name) {
            return Err(KubernetesJobError::BindingInvalid);
        }
        for target in cleanup_plan {
            if let Some(current) = self
                .get(
                    target.namespace.as_str(),
                    api_version(target)?,
                    target.resource.as_str(),
                    target.name.as_str(),
                )
                .await?
            {
                let expected =
                    document_for(objects, target).ok_or(KubernetesJobError::BindingInvalid)?;
                verify_cleanup_owned(&current, expected)?;
                let preconditions = delete_preconditions(&current)?;
                let expected_uid = preconditions.uid.clone();
                self.delete_owned(
                    target,
                    api_version(target)?,
                    expected_uid.as_str(),
                    preconditions,
                    |current| verify_cleanup_owned(current, expected),
                )
                .await?;
            }
        }
        let mut remaining = Vec::new();
        for target in cleanup_plan {
            if let Some(current) = self
                .get(
                    target.namespace.as_str(),
                    api_version(target)?,
                    target.resource.as_str(),
                    target.name.as_str(),
                )
                .await?
            {
                let expected =
                    document_for(objects, target).ok_or(KubernetesJobError::BindingInvalid)?;
                verify_cleanup_owned(&current, expected)?;
                remaining.push(object_ref(api_version(target)?, target, &current)?);
            }
        }
        Ok(cleanup_status(remaining))
    }

    /// Deletes and verifies a recovered attempt using only persisted references.
    ///
    /// # Errors
    ///
    /// Returns an error when the recovered identity is invalid or Kubernetes cannot delete or
    /// verify one of the owned objects.
    pub async fn cleanup_recovery(
        &self,
        identity: &KubernetesJobIdentity,
        objects: &[ExecutionObjectRef],
    ) -> Result<ExecutionCleanupStatus, KubernetesJobError> {
        self.ensure_binding(identity)?;
        for object in objects {
            let target = target_for(identity.namespace.as_str(), object);
            let current = self
                .get(
                    target.namespace.as_str(),
                    object.api_version.as_str(),
                    target.resource.as_str(),
                    target.name.as_str(),
                )
                .await?;
            let Some(current) = current else { continue };
            verify_recovery_owned(
                &current,
                object,
                identity.namespace.as_str(),
                &identity.ownership,
            )?;
            let preconditions = delete_preconditions(&current)?;
            self.delete_owned(
                &target,
                object.api_version.as_str(),
                object.uid.as_str(),
                preconditions,
                |current| {
                    verify_recovery_owned(
                        current,
                        object,
                        identity.namespace.as_str(),
                        &identity.ownership,
                    )
                },
            )
            .await?;
        }
        let mut remaining = Vec::new();
        for object in objects {
            let current = self
                .get(
                    identity.namespace.as_str(),
                    object.api_version.as_str(),
                    object.resource.as_str(),
                    object.name.as_str(),
                )
                .await?;
            if let Some(current) = current {
                verify_recovery_owned(
                    &current,
                    object,
                    identity.namespace.as_str(),
                    &identity.ownership,
                )?;
                remaining.push(object.clone());
            }
        }
        Ok(cleanup_status(remaining))
    }

    /// Deletes and verifies deterministic intent targets before any UID was persisted.
    ///
    /// # Errors
    ///
    /// Returns an error when an observed object fails ownership validation.
    pub async fn cleanup_intent(
        &self,
        identity: &KubernetesJobIdentity,
        targets: &[KubernetesCleanupTarget],
    ) -> Result<ExecutionCleanupStatus, KubernetesJobError> {
        self.ensure_binding(identity)?;
        for target in targets {
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
            verify_owned(&current, &identity.ownership)?;
            let preconditions = delete_preconditions(&current)?;
            let expected_uid = preconditions.uid.clone();
            self.delete_owned(
                target,
                api_version(target)?,
                expected_uid.as_str(),
                preconditions,
                |current| verify_owned(current, &identity.ownership),
            )
            .await?;
        }
        let mut remaining = Vec::new();
        for target in targets {
            if let Some(current) = self
                .get(
                    target.namespace.as_str(),
                    api_version(target)?,
                    target.resource.as_str(),
                    target.name.as_str(),
                )
                .await?
            {
                verify_owned(&current, &identity.ownership)?;
                remaining.push(object_ref(api_version(target)?, target, &current)?);
            }
        }
        Ok(cleanup_status(remaining))
    }

    fn ensure_binding(&self, identity: &KubernetesJobIdentity) -> Result<(), KubernetesJobError> {
        if identity.namespace != self.configuration.runner_namespace
            || !safe_segment(identity.job_name.as_str())
            || !safe_segment(identity.namespace.as_str())
        {
            return Err(KubernetesJobError::BindingInvalid);
        }
        Ok(())
    }

    async fn require_runner_default_deny(
        &self,
        identity: &KubernetesJobIdentity,
    ) -> Result<(), KubernetesJobError> {
        let policy = self
            .get(
                identity.namespace.as_str(),
                "networking.k8s.io/v1",
                "networkpolicies",
                identity.default_deny_policy,
            )
            .await?
            .ok_or(KubernetesJobError::NetworkIsolationUnavailable)?;
        verify_runner_default_deny(
            &policy,
            identity.namespace.as_str(),
            identity.default_deny_policy,
        )
    }

    async fn apply(
        &self,
        namespace: &str,
        api_version: &str,
        plural: &str,
        name: &str,
        document: &Value,
    ) -> Result<(), KubernetesJobError> {
        let response = self
            .authorized(self.client.request(
                Method::PATCH,
                self.resource_url(namespace, api_version, plural, name)?,
            ))?
            .query(&[("fieldManager", self.field_manager)])
            .header("content-type", "application/apply-patch+yaml")
            .body(serde_json::to_vec(document).map_err(|_| KubernetesJobError::BindingInvalid)?)
            .send()
            .await
            .map_err(|_| KubernetesJobError::KubernetesUnavailable)?;
        if response.status().is_success() {
            Ok(())
        } else if response.status() == StatusCode::CONFLICT {
            Err(KubernetesJobError::IdentityConflict)
        } else {
            Err(KubernetesJobError::KubernetesRejected)
        }
    }

    async fn get(
        &self,
        namespace: &str,
        api_version: &str,
        plural: &str,
        name: &str,
    ) -> Result<Option<Value>, KubernetesJobError> {
        let resource_id = format!("{namespace}/{plural}/{name}");
        let response = self
            .authorized(self.client.get(self.resource_url(
                namespace,
                api_version,
                plural,
                name,
            )?))?
            .send()
            .await
            .map_err(|error| {
                tracing::error!(
                    event = "evaluation.kubernetes.get_failed",
                    log_scope = self.log_scope,
                    resource_id = %resource_id,
                    failure_stage = "execution.kubernetes.get",
                    diagnostic_code = %self.diagnostic("KUBERNETES_UNAVAILABLE"),
                    error_kind = reqwest_error_kind(&error),
                    "Kubernetes GET transport failed",
                );
                KubernetesJobError::KubernetesUnavailable
            })?;
        let status = response.status();
        if status == StatusCode::NOT_FOUND {
            tracing::info!(
                event = "evaluation.kubernetes.get_absent",
                log_scope = self.log_scope,
                resource_id = %resource_id,
                failure_stage = "execution.kubernetes.get",
                outcome = "absent",
                http_status = status.as_u16(),
                "Kubernetes resource is already absent",
            );
            Ok(None)
        } else if status.is_success() {
            response.json().await.map(Some).map_err(|error| {
                tracing::error!(
                    event = "evaluation.kubernetes.get_decode_failed",
                    log_scope = self.log_scope,
                    resource_id = %resource_id,
                    failure_stage = "execution.kubernetes.get",
                    diagnostic_code = %self.diagnostic("OBSERVATION_INVALID"),
                    error_kind = reqwest_error_kind(&error),
                    http_status = status.as_u16(),
                    "Kubernetes GET response could not be decoded",
                );
                KubernetesJobError::ObservationInvalid
            })
        } else {
            tracing::error!(
                event = "evaluation.kubernetes.get_rejected",
                log_scope = self.log_scope,
                resource_id = %resource_id,
                failure_stage = "execution.kubernetes.get",
                diagnostic_code = %self.diagnostic("KUBERNETES_REJECTED"),
                error_kind = "api_rejected",
                http_status = status.as_u16(),
                "Kubernetes GET was rejected",
            );
            Err(KubernetesJobError::KubernetesRejected)
        }
    }

    async fn list_pods(
        &self,
        namespace: &str,
        attempt_id: Uuid,
    ) -> Result<Value, KubernetesJobError> {
        let response = self
            .authorized(
                self.client
                    .get(self.collection_url(namespace, "v1", "pods")?),
            )?
            .query(&[("labelSelector", format!("{ATTEMPT_ID_LABEL}={attempt_id}"))])
            .send()
            .await
            .map_err(|_| KubernetesJobError::KubernetesUnavailable)?;
        if response.status().is_success() {
            response
                .json()
                .await
                .map_err(|_| KubernetesJobError::ObservationInvalid)
        } else {
            Err(KubernetesJobError::KubernetesRejected)
        }
    }

    async fn delete(
        &self,
        target: &KubernetesCleanupTarget,
        preconditions: &KubernetesDeletePreconditions,
    ) -> Result<(), KubernetesJobError> {
        let resource_id = format!("{}/{}/{}", target.namespace, target.resource, target.name);
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
            .map_err(|error| {
                tracing::error!(
                    event = "evaluation.kubernetes.delete_failed",
                    log_scope = self.log_scope,
                    resource_id = %resource_id,
                    failure_stage = "execution.kubernetes.delete",
                    diagnostic_code = %self.diagnostic("KUBERNETES_UNAVAILABLE"),
                    error_kind = reqwest_error_kind(&error),
                    "Kubernetes DELETE transport failed",
                );
                KubernetesJobError::KubernetesUnavailable
            })?;
        let status = response.status();
        let result = classify_delete_status(status);
        match &result {
            Ok(()) => {
                tracing::info!(
                    event = "evaluation.kubernetes.delete_accepted",
                    log_scope = self.log_scope,
                    resource_id = %resource_id,
                    failure_stage = "execution.kubernetes.delete",
                    outcome = if status == StatusCode::NOT_FOUND {
                        "already_absent"
                    } else {
                        "accepted"
                    },
                    http_status = status.as_u16(),
                    "Kubernetes DELETE completed",
                );
            }
            Err(error) => {
                if status == StatusCode::CONFLICT {
                    tracing::warn!(
                        event = "evaluation.kubernetes.delete_precondition_conflict",
                        log_scope = self.log_scope,
                        resource_id = %resource_id,
                        failure_stage = "execution.kubernetes.delete",
                        diagnostic_code = %self.diagnostic("ATTEMPT_IDENTITY_CONFLICT"),
                        error_kind = "precondition_conflict",
                        http_status = status.as_u16(),
                        "Kubernetes DELETE precondition conflicted; identity is rechecked before retry",
                    );
                } else {
                    tracing::error!(
                        event = "evaluation.kubernetes.delete_rejected",
                        log_scope = self.log_scope,
                        resource_id = %resource_id,
                        failure_stage = "execution.kubernetes.delete",
                        diagnostic_code = %self.diagnostic("KUBERNETES_REJECTED"),
                        error_kind = error.error_kind(),
                        http_status = status.as_u16(),
                        "Kubernetes DELETE was rejected",
                    );
                }
            }
        }
        result
    }

    async fn delete_owned<F>(
        &self,
        target: &KubernetesCleanupTarget,
        api_version: &str,
        expected_uid: &str,
        mut preconditions: KubernetesDeletePreconditions,
        verify: F,
    ) -> Result<(), KubernetesJobError>
    where
        F: Fn(&Value) -> Result<(), KubernetesJobError>,
    {
        let mut retry = 0;
        loop {
            match self.delete(target, &preconditions).await {
                Ok(()) => return Ok(()),
                Err(KubernetesJobError::IdentityConflict)
                    if retry < MAX_DELETE_CONFLICT_RETRIES =>
                {
                    let resource_id =
                        format!("{}/{}/{}", target.namespace, target.resource, target.name);
                    let Some(current) = self
                        .get(
                            target.namespace.as_str(),
                            api_version,
                            target.resource.as_str(),
                            target.name.as_str(),
                        )
                        .await?
                    else {
                        tracing::info!(
                            event = "evaluation.kubernetes.delete_retry_absent",
                            log_scope = self.log_scope,
                            resource_id = %resource_id,
                            failure_stage = "execution.kubernetes.delete.retry",
                            outcome = "already_absent",
                            "Kubernetes resource disappeared after delete conflict",
                        );
                        return Ok(());
                    };
                    if current.pointer("/metadata/uid").and_then(Value::as_str)
                        != Some(expected_uid)
                    {
                        tracing::error!(
                            event = "evaluation.kubernetes.delete_retry_identity_changed",
                            log_scope = self.log_scope,
                            resource_id = %resource_id,
                            failure_stage = "execution.kubernetes.delete.retry",
                            diagnostic_code = %self.diagnostic("ATTEMPT_IDENTITY_CONFLICT"),
                            outcome = "uid_changed",
                            "Kubernetes resource UID changed after delete precondition conflict",
                        );
                        return Err(KubernetesJobError::IdentityConflict);
                    }
                    if let Err(error) = verify(&current) {
                        tracing::error!(
                            event = "evaluation.kubernetes.delete_retry_ownership_failed",
                            log_scope = self.log_scope,
                            resource_id = %resource_id,
                            failure_stage = "execution.kubernetes.delete.retry",
                            diagnostic_code = %self.diagnostic("ATTEMPT_IDENTITY_CONFLICT"),
                            outcome = "ownership_rejected",
                            "Kubernetes resource ownership failed after delete precondition conflict",
                        );
                        return Err(error);
                    }
                    preconditions = delete_preconditions(&current)?;
                    tracing::warn!(
                        event = "evaluation.kubernetes.delete_retry",
                        log_scope = self.log_scope,
                        resource_id = %resource_id,
                        failure_stage = "execution.kubernetes.delete.retry",
                        diagnostic_code = %self.diagnostic("ATTEMPT_IDENTITY_CONFLICT"),
                        outcome = "same_uid_refresh",
                        "retrying Kubernetes DELETE after same-UID resource-version conflict",
                    );
                    retry += 1;
                }
                Err(error @ KubernetesJobError::IdentityConflict) => {
                    tracing::error!(
                        event = "evaluation.kubernetes.delete_retry_exhausted",
                        log_scope = self.log_scope,
                        resource_id = %format!(
                            "{}/{}/{}",
                            target.namespace, target.resource, target.name
                        ),
                        failure_stage = "execution.kubernetes.delete.retry",
                        diagnostic_code = %self.diagnostic("ATTEMPT_IDENTITY_CONFLICT"),
                        outcome = "retry_exhausted",
                        "Kubernetes DELETE precondition conflicts exceeded the bounded retry limit",
                    );
                    return Err(error);
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn authorized(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::RequestBuilder, KubernetesJobError> {
        let token = read_bound_text(&self.configuration.kubernetes_bearer_token_file)?;
        Ok(request.bearer_auth(token))
    }

    fn resource_url(
        &self,
        namespace: &str,
        api_version: &str,
        plural: &str,
        name: &str,
    ) -> Result<Url, KubernetesJobError> {
        if !safe_segment(namespace) || !safe_segment(plural) || !safe_segment(name) {
            return Err(KubernetesJobError::BindingInvalid);
        }
        self.configuration
            .kubernetes_api_server
            .join(&format!(
                "{}/namespaces/{namespace}/{plural}/{name}",
                api_prefix(api_version)
            ))
            .map_err(|_| KubernetesJobError::ConfigurationInvalid)
    }

    fn collection_url(
        &self,
        namespace: &str,
        api_version: &str,
        plural: &str,
    ) -> Result<Url, KubernetesJobError> {
        if !safe_segment(namespace) || !safe_segment(plural) {
            return Err(KubernetesJobError::BindingInvalid);
        }
        self.configuration
            .kubernetes_api_server
            .join(&format!(
                "{}/namespaces/{namespace}/{plural}",
                api_prefix(api_version)
            ))
            .map_err(|_| KubernetesJobError::ConfigurationInvalid)
    }

    fn diagnostic(&self, suffix: &str) -> String {
        format!("{}{suffix}", self.diagnostic_prefix)
    }
}

fn object_ref(
    api_version: &'static str,
    target: &KubernetesCleanupTarget,
    resource: &Value,
) -> Result<ExecutionObjectRef, KubernetesJobError> {
    let uid = resource
        .pointer("/metadata/uid")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(KubernetesJobError::IdentityConflict)?;
    Ok(ExecutionObjectRef {
        api_version: api_version.to_owned(),
        resource: target.resource.clone(),
        name: target.name.clone(),
        uid: uid.to_owned(),
    })
}

fn target_for(namespace: &str, object: &ExecutionObjectRef) -> KubernetesCleanupTarget {
    KubernetesCleanupTarget {
        namespace: namespace.to_owned(),
        resource: object.resource.clone(),
        name: object.name.clone(),
        propagation_policy: "Foreground".to_owned(),
    }
}

fn cleanup_status(remaining: Vec<ExecutionObjectRef>) -> ExecutionCleanupStatus {
    if remaining.is_empty() {
        ExecutionCleanupStatus::Confirmed
    } else {
        ExecutionCleanupStatus::Pending {
            remaining_objects: remaining,
        }
    }
}

fn build_observation(
    state: ExecutionWorkloadState,
    terminated: Option<&Value>,
    pod_name: Option<String>,
    timing: ExecutionTiming,
) -> Result<ExecutionObservation, KubernetesJobError> {
    let observation = ExecutionObservation {
        state,
        exit_code: terminated
            .and_then(|value| value.pointer("/exitCode"))
            .and_then(Value::as_i64)
            .and_then(|code| i32::try_from(code).ok()),
        reason_code: terminated
            .and_then(|value| value.pointer("/reason"))
            .and_then(Value::as_str)
            .filter(|reason| valid_observation_reason(reason))
            .map(str::to_owned),
        pod_name,
        started_at: timing.started_at,
        terminated_at: timing.terminated_at,
    };
    observation
        .validate()
        .map_err(|_| KubernetesJobError::ObservationInvalid)?;
    Ok(observation)
}

fn valid_observation_reason(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

/// Classifies a Kubernetes DELETE response into the stable cleanup outcome.
///
/// # Errors
///
/// Returns [`KubernetesJobError::IdentityConflict`] for a precondition conflict and
/// [`KubernetesJobError::KubernetesRejected`] for any other rejection.
pub fn classify_delete_status(status: StatusCode) -> Result<(), KubernetesJobError> {
    if status.is_success() || status == StatusCode::NOT_FOUND {
        Ok(())
    } else if status == StatusCode::CONFLICT {
        Err(KubernetesJobError::IdentityConflict)
    } else {
        Err(KubernetesJobError::KubernetesRejected)
    }
}

/// Returns a bounded transport failure classification for diagnostics.
#[must_use]
pub fn reqwest_error_kind(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "timeout"
    } else if error.is_connect() {
        "connect"
    } else if error.is_request() {
        "request"
    } else if error.is_body() {
        "body"
    } else if error.is_decode() {
        "decode"
    } else {
        "other"
    }
}

pub(crate) fn failed_container_diagnostic<'a>(
    terminated: &'a Value,
    identity: &'a KubernetesJobIdentity,
) -> &'a str {
    if terminated.pointer("/reason").and_then(Value::as_str) == Some("OOMKilled") {
        identity.oom_diagnostic_code
    } else {
        terminated
            .pointer("/message")
            .and_then(Value::as_str)
            .filter(|message| is_stable_diagnostic(message, identity.stable_diagnostic_prefix))
            .unwrap_or(identity.failed_diagnostic_code)
    }
}

fn main_container_status<'a>(
    pod: &'a Value,
    container: &str,
) -> Result<Option<&'a Value>, KubernetesJobError> {
    let statuses = pod
        .pointer("/status/containerStatuses")
        .and_then(Value::as_array)
        .ok_or(KubernetesJobError::ObservationInvalid)?;
    let matches = statuses
        .iter()
        .filter(|status| status.pointer("/name").and_then(Value::as_str) == Some(container))
        .collect::<Vec<_>>();
    if matches.len() > 1 {
        return Err(KubernetesJobError::ObservationInvalid);
    }
    Ok(matches.into_iter().next())
}

pub(crate) fn execution_timing(status: &Value) -> Result<ExecutionTiming, KubernetesJobError> {
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
    if started
        .zip(finished)
        .is_some_and(|(started_at, finished_at)| started_at == finished_at)
    {
        tracing::warn!(
            event = "evaluation.executor.timing_precision_insufficient",
            diagnostic_code = "LW_EVALUATION_EXECUTOR_TIMING_PRECISION_INSUFFICIENT",
            timestamp_precision = "seconds",
            started_at = status
                .pointer("/state/terminated/startedAt")
                .and_then(serde_json::Value::as_str),
            finished_at = status
                .pointer("/state/terminated/finishedAt")
                .and_then(serde_json::Value::as_str),
        );
        return Ok(ExecutionTiming::unknown());
    }
    let timing = ExecutionTiming {
        started_at: started,
        terminated_at: finished,
    };
    timing
        .validate()
        .map_err(|_| KubernetesJobError::ObservationInvalid)?;
    Ok(timing)
}

fn parse_kubernetes_timestamp(value: &str) -> Result<UtcTimestamp, KubernetesJobError> {
    let parsed = time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
        .map_err(|_| KubernetesJobError::ObservationInvalid)?
        .to_offset(time::UtcOffset::UTC);
    let milliseconds = parsed.nanosecond() / 1_000_000 * 1_000_000;
    let normalized = parsed
        .replace_nanosecond(milliseconds)
        .map_err(|_| KubernetesJobError::ObservationInvalid)?;
    UtcTimestamp::from_utc(normalized).map_err(|_| KubernetesJobError::ObservationInvalid)
}

fn verify_recovery_owned(
    resource: &Value,
    object: &ExecutionObjectRef,
    namespace: &str,
    ownership: &KubernetesOwnership,
) -> Result<(), KubernetesJobError> {
    let metadata = resource
        .pointer("/metadata")
        .and_then(Value::as_object)
        .ok_or(KubernetesJobError::IdentityConflict)?;
    if metadata.get("name").and_then(Value::as_str) != Some(object.name.as_str())
        || metadata.get("namespace").and_then(Value::as_str) != Some(namespace)
        || metadata.get("uid").and_then(Value::as_str) != Some(object.uid.as_str())
    {
        return Err(KubernetesJobError::IdentityConflict);
    }
    let labels = metadata
        .get("labels")
        .and_then(Value::as_object)
        .ok_or(KubernetesJobError::IdentityConflict)?;
    let owned = ownership_labels(ownership)
        .into_iter()
        .all(|(key, expected)| labels.get(key).and_then(Value::as_str) == Some(expected.as_str()));
    if owned {
        Ok(())
    } else {
        Err(KubernetesJobError::IdentityConflict)
    }
}

pub(crate) fn verify_owned(
    resource: &Value,
    ownership: &KubernetesOwnership,
) -> Result<(), KubernetesJobError> {
    if ownership.request_sha256.is_empty() {
        return Err(KubernetesJobError::IdentityConflict);
    }
    let labels = resource
        .pointer("/metadata/labels")
        .and_then(Value::as_object)
        .ok_or(KubernetesJobError::IdentityConflict)?;
    let labels_match = ownership_labels(ownership)
        .into_iter()
        .all(|(key, expected)| labels.get(key).and_then(Value::as_str) == Some(expected.as_str()));
    let annotation_matches = resource
        .pointer("/metadata/annotations/labweaver.io~1request-sha256")
        .and_then(Value::as_str)
        == Some(ownership.request_sha256.as_str());
    if labels_match && annotation_matches {
        Ok(())
    } else {
        Err(KubernetesJobError::IdentityConflict)
    }
}

fn ownership_labels(ownership: &KubernetesOwnership) -> [(&'static str, String); 4] {
    [
        (MANAGED_BY_LABEL, MANAGED_BY_VALUE.to_owned()),
        (RUN_ID_LABEL, ownership.run_id.to_string()),
        (STEP_RUN_ID_LABEL, ownership.step_run_id.to_string()),
        (ATTEMPT_ID_LABEL, ownership.attempt_id.to_string()),
    ]
}

fn verify_immutable_identity(current: &Value, expected: &Value) -> Result<(), KubernetesJobError> {
    let current_kind = current.pointer("/kind").and_then(Value::as_str);
    let expected_kind = expected.pointer("/kind").and_then(Value::as_str);
    let current_name = current.pointer("/metadata/name").and_then(Value::as_str);
    let expected_name = expected.pointer("/metadata/name").and_then(Value::as_str);
    if current_kind == expected_kind && current_name == expected_name {
        Ok(())
    } else {
        Err(KubernetesJobError::IdentityConflict)
    }
}

pub(crate) fn verify_cleanup_owned(
    current: &Value,
    expected: &Value,
) -> Result<(), KubernetesJobError> {
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
        .ok_or(KubernetesJobError::IdentityConflict)?;
    let expected_labels = expected
        .pointer("/metadata/labels")
        .and_then(Value::as_object)
        .ok_or(KubernetesJobError::IdentityConflict)?;
    let labels_owned = [
        MANAGED_BY_LABEL,
        RUN_ID_LABEL,
        STEP_RUN_ID_LABEL,
        ATTEMPT_ID_LABEL,
    ]
    .into_iter()
    .all(|key| current_labels.get(key) == expected_labels.get(key));
    let request_identity_owned = current
        .pointer("/metadata/annotations/labweaver.io~1request-sha256")
        == expected.pointer("/metadata/annotations/labweaver.io~1request-sha256");
    if current_namespace == expected_namespace && labels_owned && request_identity_owned {
        Ok(())
    } else {
        Err(KubernetesJobError::IdentityConflict)
    }
}

/// UID and resource-version preconditions for one exact object deletion.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KubernetesDeletePreconditions {
    pub uid: String,
    pub resource_version: String,
}

/// Renders the `DeleteOptions` body with exact preconditions.
#[must_use]
pub fn delete_options(
    target: &KubernetesCleanupTarget,
    preconditions: &KubernetesDeletePreconditions,
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

/// Reads UID and resource-version preconditions from one live object.
///
/// # Errors
///
/// Returns [`KubernetesJobError::IdentityConflict`] when either value is missing.
pub fn delete_preconditions(
    resource: &Value,
) -> Result<KubernetesDeletePreconditions, KubernetesJobError> {
    let uid = resource
        .pointer("/metadata/uid")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(KubernetesJobError::IdentityConflict)?;
    let resource_version = resource
        .pointer("/metadata/resourceVersion")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(KubernetesJobError::IdentityConflict)?;
    Ok(KubernetesDeletePreconditions {
        uid: uid.to_owned(),
        resource_version: resource_version.to_owned(),
    })
}

/// Verifies the permanent namespace-wide default-deny isolation policy.
///
/// # Errors
///
/// Returns [`KubernetesJobError::NetworkIsolationUnavailable`] for any drift.
pub fn verify_runner_default_deny(
    policy: &Value,
    expected_namespace: &str,
    expected_name: &str,
) -> Result<(), KubernetesJobError> {
    let policy_types = policy
        .pointer("/spec/policyTypes")
        .and_then(Value::as_array)
        .ok_or(KubernetesJobError::NetworkIsolationUnavailable)?;
    let has_policy_type = |expected| {
        policy_types
            .iter()
            .filter_map(Value::as_str)
            .filter(|value| *value == expected)
            .count()
            == 1
    };
    let valid = policy.pointer("/kind").and_then(Value::as_str) == Some("NetworkPolicy")
        && policy.pointer("/metadata/name").and_then(Value::as_str) == Some(expected_name)
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
        && is_missing_or_empty_rule_list(policy, "/spec/ingress")
        && is_missing_or_empty_rule_list(policy, "/spec/egress");
    if valid {
        Ok(())
    } else {
        Err(KubernetesJobError::NetworkIsolationUnavailable)
    }
}

fn is_missing_or_empty_rule_list(policy: &Value, path: &str) -> bool {
    match policy.pointer(path) {
        None => true,
        Some(value) => value.as_array().is_some_and(Vec::is_empty),
    }
}

/// Builds the REST prefix for one Kubernetes API version.
#[must_use]
pub fn api_prefix(api_version: &str) -> String {
    if api_version == "v1" {
        "/api/v1".to_owned()
    } else {
        format!("/apis/{api_version}")
    }
}

/// Maps one cleanup resource plural to its exact API version.
///
/// # Errors
///
/// Returns [`KubernetesJobError::BindingInvalid`] for a resource this backend does not own.
pub fn api_version(target: &KubernetesCleanupTarget) -> Result<&'static str, KubernetesJobError> {
    match target.resource.as_str() {
        "jobs" => Ok("batch/v1"),
        "networkpolicies" => Ok("networking.k8s.io/v1"),
        "configmaps" | "secrets" => Ok("v1"),
        _ => Err(KubernetesJobError::BindingInvalid),
    }
}

/// Returns whether one path segment is safe to interpolate into a Kubernetes URL.
#[must_use]
pub fn safe_segment(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.'))
}

fn is_stable_diagnostic(value: &str, prefix: &str) -> bool {
    value.starts_with(prefix)
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

/// Reads one bounded projected token or CA file without following an oversized source.
///
/// # Errors
///
/// Returns a stable configuration error for an unavailable, empty, or oversized file.
pub fn read_bound_file(path: &Path) -> Result<Vec<u8>, KubernetesJobError> {
    let file =
        fs::File::open(path).map_err(|source| KubernetesJobError::ConfigurationUnavailable {
            operation: "open",
            source,
        })?;
    let metadata =
        file.metadata()
            .map_err(|source| KubernetesJobError::ConfigurationUnavailable {
                operation: "metadata",
                source,
            })?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_BOUND_FILE_BYTES {
        return Err(KubernetesJobError::ConfigurationInvalid);
    }
    let mut bytes = Vec::with_capacity(
        usize::try_from(metadata.len()).map_err(|_| KubernetesJobError::ConfigurationInvalid)?,
    );
    file.take(MAX_BOUND_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| KubernetesJobError::ConfigurationUnavailable {
            operation: "read",
            source,
        })?;
    if u64::try_from(bytes.len()).map_err(|_| KubernetesJobError::ConfigurationInvalid)?
        != metadata.len()
        || bytes.len() as u64 > MAX_BOUND_FILE_BYTES
    {
        return Err(KubernetesJobError::ConfigurationInvalid);
    }
    Ok(bytes)
}

/// Reads one bounded projected token as trimmed single-token text.
///
/// # Errors
///
/// Returns a stable configuration error for invalid or non-token content.
pub fn read_bound_text(path: &Path) -> Result<String, KubernetesJobError> {
    let bytes = read_bound_file(path)?;
    let value = String::from_utf8(bytes).map_err(|_| KubernetesJobError::ConfigurationInvalid)?;
    let value = value.trim();
    if value.is_empty() || value.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return Err(KubernetesJobError::ConfigurationInvalid);
    }
    Ok(value.to_owned())
}
