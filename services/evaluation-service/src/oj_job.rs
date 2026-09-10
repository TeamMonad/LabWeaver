//! Kubernetes resource planning for one isolated program evaluation attempt.
#![allow(
    missing_docs,
    clippy::too_many_lines,
    reason = "the fixed resource plan is intentionally colocated for security review"
)]

use std::sync::Arc;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

use crate::{
    MaterializeCommand, MaterializeContent, MaterializeDestination,
    materializer::{
        DEFAULT_MATERIALIZER_CA_FILE, DEFAULT_MATERIALIZER_COMMAND_PATH, MATERIALIZER_CA_FILE_ENV,
    },
    oj::{OjExecutionRequest, is_sha256_image},
};

const MAX_COMMAND_BYTES: usize = 1024 * 1024;
const WORKER_UID: u32 = 65_532;
const MEBIBYTE: u64 = 1024 * 1024;
const MIN_WORKER_MEMORY_MIB: u64 = 512;
const WORKER_MEMORY_OVERHEAD_MIB: u64 = 256;
const INPUT_VOLUME_SIZE_LIMIT: &str = "96Mi";
const EVIDENCE_VOLUME_SIZE_LIMIT: &str = "16Mi";
const MATERIALIZER_MEMORY_LIMIT: &str = "256Mi";
const MATERIALIZER_EPHEMERAL_LIMIT: &str = "256Mi";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OjJobBinding {
    pub namespace: String,
    pub service_account_name: String,
    pub image_pull_secret_name: String,
    pub worker_image: String,
    pub request: OjExecutionRequest,
    /// Short-lived signed downloads and their destination roots. This command
    /// is mounted only by the init container.
    pub materializer: MaterializeCommand,
    /// The object-store trust bundle copied into the per-attempt Secret and
    /// mounted only by the materializer init container. It is deliberately
    /// omitted from binding serialization because it is deployment material,
    /// not part of the execution command.
    #[serde(skip)]
    pub materializer_ca_bundle: Option<Arc<[u8]>>,
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
    materializer_secret_name: String,
    namespace: String,
    pub config_map: Value,
    pub materializer_secret: Value,
    pub network_policy: Value,
    pub job: Value,
}

impl OjJobResources {
    /// Builds the complete immutable command, init materializer, and worker
    /// resource bundle. Input and evidence are per-Pod `EmptyDir` volumes; no
    /// caller-provided PVC can enter this execution boundary.
    ///
    /// # Errors
    ///
    /// Returns an error when the request, materializer command, or generated
    /// resource identities fail validation.
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
        let materializer_command = serde_json::to_vec(&binding.materializer)
            .map_err(|_| OjJobError::MaterializerInvalid)?;
        if materializer_command.len() > MAX_COMMAND_BYTES {
            return Err(OjJobError::MaterializerTooLarge);
        }
        if binding
            .materializer_ca_bundle
            .as_ref()
            .is_some_and(|ca_bundle| {
                ca_bundle.is_empty()
                    || ca_bundle.len()
                        > MAX_COMMAND_BYTES.saturating_sub(materializer_command.len())
            })
        {
            return Err(OjJobError::MaterializerTooLarge);
        }

        let name = format!(
            "lw-oj-{}",
            &binding.request.attempt_id.simple().to_string()[..20]
        );
        let materializer_secret_name = format!("{name}-materializer");
        let request_sha256 = binding
            .request
            .request_sha256()
            .map_err(|_| OjJobError::CommandInvalid)?;
        let labels = json!({
            "app.kubernetes.io/name":"evaluation-program-runner",
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
        let materializer_data = match binding.materializer_ca_bundle.as_deref() {
            Some(ca_bundle) => json!({
                "command.json":STANDARD.encode(materializer_command),
                "ca.crt":STANDARD.encode(ca_bundle),
            }),
            None => json!({"command.json":STANDARD.encode(materializer_command)}),
        };
        let materializer_secret = json!({
            "apiVersion":"v1",
            "kind":"Secret",
            "metadata":{
                "name":materializer_secret_name,
                "namespace":binding.namespace,
                "labels":labels,
                "annotations":annotations,
                "ownerReferences":[],
            },
            "immutable":true,
            "type":"Opaque",
            "data":materializer_data,
        });
        let network_policy = json!({
            "apiVersion":"networking.k8s.io/v1",
            "kind":"NetworkPolicy",
            "metadata":{"name":name,"namespace":binding.namespace,"labels":labels,"annotations":annotations},
            "spec":{
                "podSelector":{"matchLabels":{"labweaver.io/attempt-id":binding.request.attempt_id.to_string()}},
                "policyTypes":["Ingress","Egress"],
                "ingress":[],
                // The init container requires HTTPS to the exact object store
                // URL signed by the scheduler. The worker has no URL Secret,
                // and the platform's namespace policy must constrain this
                // egress to the configured object-store endpoint.
                "egress":[{"ports":[{"protocol":"TCP","port":443}]}],
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
                        "initContainers":[{
                            "name":"materialize-input",
                            "image":binding.worker_image,
                            "imagePullPolicy":"IfNotPresent",
                            "args":["--mode","artifact-materializer"],
                            "env":materializer_env(binding),
                            "terminationMessagePath":"/dev/termination-log",
                            "terminationMessagePolicy":"File",
                            "securityContext":{
                                "allowPrivilegeEscalation":false,
                                "readOnlyRootFilesystem":true,
                                "runAsNonRoot":true,
                                "capabilities":{"drop":["ALL"]},
                            },
                            "resources":{
                                "requests":{"cpu":"100m","memory":"128Mi","ephemeral-storage":"128Mi"},
                                "limits":{"cpu":"500m","memory":MATERIALIZER_MEMORY_LIMIT,"ephemeral-storage":MATERIALIZER_EPHEMERAL_LIMIT},
                            },
                            "volumeMounts":[
                                {"name":"materializer","mountPath":"/run/secrets/materializer","readOnly":true},
                                {"name":"submission","mountPath":"/input/submission"},
                                {"name":"evaluator","mountPath":"/input/evaluator"},
                            ],
                        }],
                        "containers":[{
                            "name":"program-runner",
                            "image":binding.worker_image,
                            "imagePullPolicy":"IfNotPresent",
                            "args":["--mode","oj-worker"],
                            "env":[{"name":"LABWEAVER_OJ_COMMAND_FILE","value":"/etc/labweaver/oj/command.json"}],
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
                            "volumeMounts":[
                                {"name":"command","mountPath":"/etc/labweaver/oj","readOnly":true},
                                {"name":"submission","mountPath":"/input/submission","readOnly":true},
                                {"name":"evaluator","mountPath":"/input/evaluator","readOnly":true},
                                {"name":"evidence","mountPath":"/evidence"},
                                {"name":"work","mountPath":"/work"},
                                {"name":"build","mountPath":"/work/build"},
                                {"name":"support","mountPath":"/support"},
                            ],
                        }],
                        "volumes":[
                            {"name":"command","configMap":{"name":name,"items":[{"key":"command.json","path":"command.json"}]}},
                            {"name":"materializer","secret":materializer_volume(binding, &materializer_secret_name)},
                            {"name":"submission","emptyDir":{"sizeLimit":INPUT_VOLUME_SIZE_LIMIT}},
                            {"name":"evaluator","emptyDir":{"sizeLimit":INPUT_VOLUME_SIZE_LIMIT}},
                            {"name":"evidence","emptyDir":{"sizeLimit":EVIDENCE_VOLUME_SIZE_LIMIT}},
                            {"name":"work","emptyDir":{"sizeLimit":"256Mi"}},
                            {"name":"build","emptyDir":{"sizeLimit":"128Mi"}},
                            {"name":"support","emptyDir":{"sizeLimit":"64Mi"}},
                        ],
                    },
                },
            },
        });
        Ok(Self {
            name,
            materializer_secret_name,
            namespace: binding.namespace.clone(),
            config_map,
            materializer_secret,
            network_policy,
            job,
        })
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn materializer_secret_name(&self) -> &str {
        &self.materializer_secret_name
    }

    #[must_use]
    pub fn cleanup_plan(&self) -> Vec<OjCleanupTarget> {
        [
            ("jobs", self.name.as_str()),
            ("networkpolicies", self.name.as_str()),
            ("configmaps", self.name.as_str()),
            ("secrets", self.materializer_secret_name.as_str()),
        ]
        .into_iter()
        .map(|(resource, name)| OjCleanupTarget {
            namespace: self.namespace.clone(),
            resource: resource.to_owned(),
            name: name.to_owned(),
            propagation_policy: "Foreground".to_owned(),
        })
        .collect()
    }

    pub(crate) fn document_for(&self, resource: &str) -> Option<&Value> {
        match resource {
            "jobs" => Some(&self.job),
            "networkpolicies" => Some(&self.network_policy),
            "configmaps" => Some(&self.config_map),
            "secrets" => Some(&self.materializer_secret),
            _ => None,
        }
    }
}

fn materializer_env(binding: &OjJobBinding) -> Value {
    let mut env = vec![json!({
        "name":"LABWEAVER_ARTIFACT_MATERIALIZER_COMMAND_FILE",
        "value":DEFAULT_MATERIALIZER_COMMAND_PATH,
    })];
    if binding.materializer_ca_bundle.is_some() {
        env.push(json!({
            "name":MATERIALIZER_CA_FILE_ENV,
            "value":DEFAULT_MATERIALIZER_CA_FILE,
        }));
    }
    Value::Array(env)
}

fn materializer_volume(binding: &OjJobBinding, secret_name: &str) -> Value {
    let mut items = vec![json!({"key":"command.json","path":"command.json"})];
    if binding.materializer_ca_bundle.is_some() {
        items.push(json!({"key":"ca.crt","path":"ca.crt"}));
    }
    json!({"secretName":secret_name,"items":items})
}

fn validate_binding(binding: &OjJobBinding) -> Result<(), OjJobError> {
    if !is_dns_name(&binding.namespace)
        || !is_dns_name(&binding.service_account_name)
        || !is_dns_name(&binding.image_pull_secret_name)
        || !is_sha256_image(&binding.worker_image)
        || !binding
            .worker_image
            .ends_with(&binding.request.toolchain_image_digest)
    {
        return Err(OjJobError::BindingInvalid);
    }
    binding
        .materializer
        .validate()
        .map_err(|_| OjJobError::MaterializerInvalid)?;
    let mut required_destinations = vec![MaterializeDestination::Submission];
    if binding.request.phase == crate::oj::OjExecutionPhase::Test {
        required_destinations.push(MaterializeDestination::Evaluator);
    }
    for destination in required_destinations {
        if !binding
            .materializer
            .artifacts
            .iter()
            .any(|artifact| artifact.destination == destination)
        {
            return Err(OjJobError::MaterializerInvalid);
        }
    }
    if !binding.materializer.artifacts.iter().any(|artifact| {
        artifact.destination == MaterializeDestination::Submission
            && matches!(artifact.content, MaterializeContent::FrozenArchive)
            && artifact.expected_sha256 == binding.request.submission_identity
    }) {
        return Err(OjJobError::MaterializerInvalid);
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
    #[error("OJ materializer command is invalid")]
    MaterializerInvalid,
    #[error("OJ materializer command exceeds the bounded Secret size")]
    MaterializerTooLarge,
}

impl OjJobError {
    #[must_use]
    pub const fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::BindingInvalid => "LW_OJ_JOB_BINDING_INVALID",
            Self::CommandInvalid => "LW_OJ_JOB_COMMAND_INVALID",
            Self::CommandTooLarge => "LW_OJ_JOB_COMMAND_TOO_LARGE",
            Self::MaterializerInvalid => "LW_OJ_MATERIALIZER_INVALID",
            Self::MaterializerTooLarge => "LW_OJ_MATERIALIZER_TOO_LARGE",
        }
    }
}
