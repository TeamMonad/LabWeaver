//! Kubernetes resource planning for one isolated C++17 evaluation attempt.
#![allow(
    missing_docs,
    clippy::too_many_lines,
    reason = "the fixed resource plan is intentionally colocated for security review"
)]

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

use crate::oj::{OjExecutionRequest, is_sha256_image};

const MAX_COMMAND_BYTES: usize = 1024 * 1024;
const WORKER_UID: u32 = 65_532;
const MEBIBYTE: u64 = 1024 * 1024;
const MIN_WORKER_MEMORY_MIB: u64 = 512;
const WORKER_MEMORY_OVERHEAD_MIB: u64 = 256;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OjJobBinding {
    pub namespace: String,
    pub service_account_name: String,
    pub image_pull_secret_name: String,
    pub submission_pvc: String,
    pub evaluator_pvc: Option<String>,
    pub evidence_pvc: String,
    pub worker_image: String,
    pub request: OjExecutionRequest,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OjCleanupTarget {
    pub namespace: String,
    pub resource: String,
    pub name: String,
    pub propagation_policy: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OjJobResources {
    name: String,
    namespace: String,
    pub config_map: Value,
    pub network_policy: Value,
    pub job: Value,
}

impl OjJobResources {
    /// Builds a deterministic, fail-closed Kubernetes resource bundle.
    ///
    /// # Errors
    ///
    /// Returns a stable [`OjJobError`] for invalid identities, bindings, resources, or image pins.
    pub fn build(binding: &OjJobBinding) -> Result<Self, OjJobError> {
        binding
            .request
            .validate()
            .map_err(|_| OjJobError::BindingInvalid)?;
        validate_binding(binding)?;
        let command =
            serde_json::to_string(&binding.request).map_err(|_| OjJobError::CommandInvalid)?;
        if command.len() > MAX_COMMAND_BYTES {
            return Err(OjJobError::CommandTooLarge);
        }

        let name = format!(
            "lw-oj-{}",
            &binding.request.attempt_id.simple().to_string()[..20]
        );
        let request_sha256 = binding
            .request
            .request_sha256()
            .map_err(|_| OjJobError::CommandInvalid)?;
        let labels = json!({
            "app.kubernetes.io/name":"evaluation-oj-runner",
            "app.kubernetes.io/part-of":"labweaver",
            "labweaver.io/managed-by":"evaluation-service",
            "labweaver.io/run-id":binding.request.run_id.to_string(),
            "labweaver.io/step-run-id":binding.request.step_run_id.to_string(),
            "labweaver.io/attempt-id":binding.request.attempt_id.to_string(),
        });
        let annotations = json!({
            "labweaver.io/trace-id":binding.request.trace_id,
            "labweaver.io/request-sha256":request_sha256.to_string(),
        });
        let mut volume_mounts = vec![
            json!({"name":"command","mountPath":"/etc/labweaver/oj","readOnly":true}),
            json!({"name":"submission","mountPath":"/input/submission","readOnly":true}),
            json!({"name":"evidence","mountPath":"/evidence"}),
            json!({"name":"work","mountPath":"/work"}),
        ];
        let mut volumes = vec![
            json!({"name":"command","configMap":{"name":name,"items":[{"key":"command.json","path":"command.json"}]}}),
            json!({"name":"submission","persistentVolumeClaim":{"claimName":binding.submission_pvc,"readOnly":true}}),
            json!({"name":"evidence","persistentVolumeClaim":{"claimName":binding.evidence_pvc}}),
            json!({"name":"work","emptyDir":{"sizeLimit":"256Mi"}}),
        ];
        if let Some(evaluator_pvc) = &binding.evaluator_pvc {
            volume_mounts.insert(
                2,
                json!({"name":"evaluator","mountPath":"/input/evaluator","readOnly":true}),
            );
            volumes.insert(
                2,
                json!({"name":"evaluator","persistentVolumeClaim":{"claimName":evaluator_pvc,"readOnly":true}}),
            );
        }
        let active_deadline_seconds = active_deadline_seconds(&binding.request)?;
        let worker_memory_limit = worker_memory_limit(&binding.request)?;
        let config_map = json!({
            "apiVersion":"v1",
            "kind":"ConfigMap",
            "metadata":{
                "name":name,
                "namespace":binding.namespace,
                "labels":labels,
                "annotations":annotations,
                "ownerReferences":[],
            },
            "immutable":true,
            "data":{"command.json":command},
        });
        let network_policy = json!({
            "apiVersion":"networking.k8s.io/v1",
            "kind":"NetworkPolicy",
            "metadata":{"name":name,"namespace":binding.namespace,"labels":labels,"annotations":annotations},
            "spec":{
                "podSelector":{"matchLabels":{"labweaver.io/attempt-id":binding.request.attempt_id.to_string()}},
                "policyTypes":["Ingress","Egress"],
                "ingress":[],
                "egress":[],
            },
        });
        let job = json!({
            "apiVersion":"batch/v1",
            "kind":"Job",
            "metadata":{"name":name,"namespace":binding.namespace,"labels":labels,"annotations":annotations},
            "spec":{
                "backoffLimit":0,
                "activeDeadlineSeconds":active_deadline_seconds,
                "ttlSecondsAfterFinished":300,
                "template":{
                    "metadata":{"labels":labels,"annotations":annotations},
                    "spec":{
                        "restartPolicy":"Never",
                        "serviceAccountName":binding.service_account_name,
                        "automountServiceAccountToken":false,
                        "terminationGracePeriodSeconds":5,
                        "imagePullSecrets":[{"name":binding.image_pull_secret_name}],
                        "securityContext":{
                            "runAsNonRoot":true,
                            "runAsUser":WORKER_UID,
                            "runAsGroup":WORKER_UID,
                            "fsGroup":WORKER_UID,
                            "fsGroupChangePolicy":"OnRootMismatch",
                            "seccompProfile":{"type":"RuntimeDefault"},
                        },
                        "containers":[{
                            "name":"oj-cpp17",
                            "image":binding.worker_image,
                            "imagePullPolicy":"IfNotPresent",
                            "args":["--mode","oj-worker"],
                            "env":[{
                                "name":"LABWEAVER_OJ_COMMAND_FILE",
                                "value":"/etc/labweaver/oj/command.json",
                            }],
                            "terminationMessagePath":"/dev/termination-log",
                            "terminationMessagePolicy":"File",
                            "securityContext":{
                                "allowPrivilegeEscalation":false,
                                "readOnlyRootFilesystem":true,
                                "runAsNonRoot":true,
                                "capabilities":{"drop":["ALL"]},
                            },
                            "resources":{
                                "requests":{"cpu":"100m","memory":"128Mi","ephemeral-storage":"64Mi"},
                                "limits":{"cpu":"1","memory":worker_memory_limit,"ephemeral-storage":"256Mi"},
                            },
                            "volumeMounts":volume_mounts,
                        }],
                        "volumes":volumes,
                    },
                },
            },
        });
        Ok(Self {
            name,
            namespace: binding.namespace.clone(),
            config_map,
            network_policy,
            job,
        })
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn cleanup_plan(&self) -> Vec<OjCleanupTarget> {
        ["jobs", "networkpolicies", "configmaps"]
            .into_iter()
            .map(|resource| OjCleanupTarget {
                namespace: self.namespace.clone(),
                resource: resource.to_owned(),
                name: self.name.clone(),
                propagation_policy: "Foreground".to_owned(),
            })
            .collect()
    }

    pub(crate) fn document_for(&self, resource: &str) -> Option<&Value> {
        match resource {
            "jobs" => Some(&self.job),
            "networkpolicies" => Some(&self.network_policy),
            "configmaps" => Some(&self.config_map),
            _ => None,
        }
    }
}

fn validate_binding(binding: &OjJobBinding) -> Result<(), OjJobError> {
    let evaluator_binding_valid = match binding.request.phase {
        crate::oj::OjExecutionPhase::Compile => binding.evaluator_pvc.is_none(),
        crate::oj::OjExecutionPhase::Test => {
            binding.evaluator_pvc.as_deref().is_some_and(is_dns_name)
        }
    };
    if !is_dns_name(&binding.namespace)
        || !is_dns_name(&binding.service_account_name)
        || !is_dns_name(&binding.image_pull_secret_name)
        || !is_dns_name(&binding.submission_pvc)
        || !evaluator_binding_valid
        || !is_dns_name(&binding.evidence_pvc)
        || !is_sha256_image(&binding.worker_image)
        || !binding
            .worker_image
            .ends_with(&binding.request.toolchain_image_digest)
    {
        return Err(OjJobError::BindingInvalid);
    }
    Ok(())
}

fn is_dns_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 253
        && value.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
                && label
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
        })
}

fn active_deadline_seconds(request: &OjExecutionRequest) -> Result<u64, OjJobError> {
    let case_count = u64::try_from(request.cases.len()).map_err(|_| OjJobError::BindingInvalid)?;
    let run_budget = request
        .limits
        .run_wall_milliseconds
        .checked_mul(case_count)
        .ok_or(OjJobError::BindingInvalid)?;
    request
        .limits
        .compile_wall_milliseconds
        .checked_add(run_budget)
        .and_then(|milliseconds| milliseconds.checked_add(10_000))
        .and_then(|milliseconds| milliseconds.checked_add(999))
        .map(|milliseconds| milliseconds / 1000)
        .ok_or(OjJobError::BindingInvalid)
}

fn worker_memory_limit(request: &OjExecutionRequest) -> Result<String, OjJobError> {
    let requested_mib = request
        .limits
        .memory_bytes
        .checked_add(MEBIBYTE - 1)
        .map(|bytes| bytes / MEBIBYTE)
        .ok_or(OjJobError::BindingInvalid)?;
    requested_mib
        .checked_add(WORKER_MEMORY_OVERHEAD_MIB)
        .map(|mib| mib.max(MIN_WORKER_MEMORY_MIB))
        .map(|mib| format!("{mib}Mi"))
        .ok_or(OjJobError::BindingInvalid)
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum OjJobError {
    #[error("OJ Job binding is invalid")]
    BindingInvalid,
    #[error("OJ worker command is invalid")]
    CommandInvalid,
    #[error("OJ worker command exceeds the bounded ConfigMap size")]
    CommandTooLarge,
}

impl OjJobError {
    #[must_use]
    pub const fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::BindingInvalid => "LW_OJ_JOB_BINDING_INVALID",
            Self::CommandInvalid => "LW_OJ_JOB_COMMAND_INVALID",
            Self::CommandTooLarge => "LW_OJ_JOB_COMMAND_TOO_LARGE",
        }
    }
}
