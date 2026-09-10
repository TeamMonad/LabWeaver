//! Kubernetes resource planning for one isolated read-only Ansible probe attempt.
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
    MaterializeCommand, MaterializeDestination,
    ansible_probe::{AnsibleProbeExecutionRequest, SSH_PORT},
    materializer::DEFAULT_MATERIALIZER_COMMAND_PATH,
    oj::is_sha256_image,
};

const MAX_COMMAND_BYTES: usize = 1024 * 1024;
const WORKER_UID: u32 = 65_532;
/// Fixed bounded margin on top of the probe wall time for image pull, volume
/// setup, and termination; the deadline never exceeds 330 seconds.
const ACTIVE_DEADLINE_MARGIN_SECONDS: u64 = 30;
const WORK_VOLUME_SIZE_LIMIT: &str = "64Mi";
const EVIDENCE_VOLUME_SIZE_LIMIT: &str = "16Mi";
const INPUT_VOLUME_SIZE_LIMIT: &str = "96Mi";
const MATERIALIZER_MEMORY_LIMIT: &str = "256Mi";
const MATERIALIZER_EPHEMERAL_LIMIT: &str = "256Mi";
/// Secret volume mode 0400; the pod `fsGroup` grants the worker group read.
const SECRET_VOLUME_MODE: u32 = 256;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AnsibleProbeJobBinding {
    pub namespace: String,
    pub service_account_name: String,
    pub image_pull_secret_name: String,
    pub worker_image: String,
    pub request: AnsibleProbeExecutionRequest,
    /// Signed package files mounted only into the input materializer init
    /// container; the probe worker sees only the resulting read-only root.
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
pub struct AnsibleProbeCleanupTarget {
    pub namespace: String,
    pub resource: String,
    pub name: String,
    pub propagation_policy: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AnsibleProbeJobResources {
    name: String,
    materializer_secret_name: String,
    ssh_private_key_secret_name: Option<String>,
    ssh_certificate_secret_name: Option<String>,
    namespace: String,
    pub config_map: Value,
    pub materializer_secret: Value,
    ssh_private_key_secret: Option<Value>,
    ssh_certificate_secret: Option<Value>,
    pub network_policy: Value,
    pub job: Value,
}

impl AnsibleProbeJobResources {
    /// Builds the complete immutable command, init materializer, and worker
    /// resource bundle. Package files are per-Pod `EmptyDir` inputs; no PVC or
    /// static playbook path is accepted at this boundary.
    ///
    /// # Errors
    ///
    /// Returns an error when the binding or its embedded command fails
    /// validation.
    pub fn build(binding: &AnsibleProbeJobBinding) -> Result<Self, AnsibleProbeJobError> {
        binding
            .request
            .validate()
            .map_err(|_| AnsibleProbeJobError::BindingInvalid)?;
        validate_binding(binding)?;
        let command = serde_json::to_string(&binding.request)
            .map_err(|_| AnsibleProbeJobError::CommandInvalid)?;
        if command.len() > MAX_COMMAND_BYTES {
            return Err(AnsibleProbeJobError::CommandTooLarge);
        }
        let materializer_command = serde_json::to_vec(&binding.materializer)
            .map_err(|_| AnsibleProbeJobError::MaterializerInvalid)?;
        if materializer_command.len() > MAX_COMMAND_BYTES {
            return Err(AnsibleProbeJobError::MaterializerTooLarge);
        }

        let name = format!(
            "lw-ap-{}",
            &binding.request.attempt_id.simple().to_string()[..20]
        );
        let materializer_secret_name = format!("{name}-materializer");
        let request_sha256 = binding
            .request
            .request_sha256()
            .map_err(|_| AnsibleProbeJobError::CommandInvalid)?;
        let labels = json!({
            "app.kubernetes.io/name":"evaluation-ansible-probe-runner",
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
        let active_deadline_seconds = binding
            .request
            .limits
            .wall_time_seconds
            .checked_add(ACTIVE_DEADLINE_MARGIN_SECONDS)
            .ok_or(AnsibleProbeJobError::BindingInvalid)?;
        let target_egress_cidr = format!("{}/32", binding.request.target.host);
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
                // HTTPS is needed by the init materializer. The probe's SSH
                // exception remains exact; the namespace policy must constrain
                // the object-store destination behind this shared Pod policy.
                "egress":[
                    {"ports":[{"protocol":"TCP","port":443}]},
                    {"to":[{"ipBlock":{"cidr":target_egress_cidr}}],"ports":[{"protocol":"TCP","port":SSH_PORT}]},
                ],
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
                                {"name":"evaluator","mountPath":"/input/evaluator"},
                            ],
                        }],
                        "containers":[{
                            "name":"ansible-probe",
                            "image":binding.worker_image,
                            "imagePullPolicy":"IfNotPresent",
                            "args":["--mode","ansible-probe-worker"],
                            "env":[{"name":"LABWEAVER_ANSIBLE_PROBE_COMMAND_FILE","value":"/command/command.json"}],
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
                                "limits":{"cpu":"500m","memory":"512Mi","ephemeral-storage":"256Mi"},
                            },
                            "volumeMounts":[
                                {"name":"command","mountPath":"/command","readOnly":true},
                                {"name":"evaluator","mountPath":"/input/evaluator","readOnly":true},
                                {"name":"ssh-private-key","mountPath":"/run/secrets/probe/private-key","readOnly":true},
                                {"name":"ssh-certificate","mountPath":"/run/secrets/probe/certificate","readOnly":true},
                                {"name":"work","mountPath":"/work"},
                                {"name":"evidence","mountPath":"/evidence"},
                            ],
                        }],
                        "volumes":[
                            {"name":"command","configMap":{"name":name,"items":[{"key":"command.json","path":"command.json"}]}},
                            {"name":"materializer","secret":materializer_volume(binding, &materializer_secret_name)},
                            {"name":"evaluator","emptyDir":{"sizeLimit":INPUT_VOLUME_SIZE_LIMIT}},
                            {"name":"ssh-private-key","secret":{
                                "secretName":binding.request.ssh_identity.private_key_secret,
                                "defaultMode":SECRET_VOLUME_MODE,
                                "items":[{"key":"key","path":"key"}],
                            }},
                            {"name":"ssh-certificate","secret":{
                                "secretName":binding.request.ssh_identity.certificate_secret,
                                "defaultMode":SECRET_VOLUME_MODE,
                                "items":[{"key":"cert.pub","path":"cert.pub"}],
                            }},
                            {"name":"work","emptyDir":{"sizeLimit":WORK_VOLUME_SIZE_LIMIT}},
                            {"name":"evidence","emptyDir":{"sizeLimit":EVIDENCE_VOLUME_SIZE_LIMIT}},
                        ],
                    },
                },
            },
        });
        Ok(Self {
            name,
            materializer_secret_name,
            ssh_private_key_secret_name: None,
            ssh_certificate_secret_name: None,
            namespace: binding.namespace.clone(),
            config_map,
            materializer_secret,
            ssh_private_key_secret: None,
            ssh_certificate_secret: None,
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

    pub(crate) fn ssh_private_key_secret_name(&self) -> Option<&str> {
        self.ssh_private_key_secret_name.as_deref()
    }

    pub(crate) fn ssh_certificate_secret_name(&self) -> Option<&str> {
        self.ssh_certificate_secret_name.as_deref()
    }

    pub(crate) fn ssh_private_key_secret(&self) -> Option<&Value> {
        self.ssh_private_key_secret.as_ref()
    }

    pub(crate) fn ssh_certificate_secret(&self) -> Option<&Value> {
        self.ssh_certificate_secret.as_ref()
    }

    /// Adds the one-shot private key and certificate Secrets referenced by the
    /// attempt Job.  The material is retained only in this in-memory bundle
    /// until the executor applies and later deletes the exact resources.
    pub(crate) fn attach_ssh_credentials(
        &mut self,
        private_key_openssh: &str,
        certificate_openssh: &str,
    ) -> Result<(), AnsibleProbeJobError> {
        if private_key_openssh.is_empty()
            || private_key_openssh.len() > MAX_COMMAND_BYTES
            || certificate_openssh.is_empty()
            || certificate_openssh.len() > MAX_COMMAND_BYTES
            || certificate_openssh.chars().any(char::is_control)
            || self.ssh_private_key_secret.is_some()
            || self.ssh_certificate_secret.is_some()
        {
            return Err(AnsibleProbeJobError::CredentialsInvalid);
        }
        let metadata = self
            .job
            .get("metadata")
            .cloned()
            .ok_or(AnsibleProbeJobError::BindingInvalid)?;
        let private_key_secret_name = self
            .job
            .pointer("/spec/template/spec/volumes")
            .and_then(Value::as_array)
            .and_then(|volumes| {
                volumes.iter().find_map(|volume| {
                    (volume.get("name").and_then(Value::as_str) == Some("ssh-private-key"))
                        .then(|| {
                            volume
                                .pointer("/secret/secretName")
                                .and_then(Value::as_str)
                                .map(str::to_owned)
                        })
                        .flatten()
                })
            })
            .ok_or(AnsibleProbeJobError::CredentialsInvalid)?;
        let certificate_secret_name = self
            .job
            .pointer("/spec/template/spec/volumes")
            .and_then(Value::as_array)
            .and_then(|volumes| {
                volumes.iter().find_map(|volume| {
                    (volume.get("name").and_then(Value::as_str) == Some("ssh-certificate"))
                        .then(|| {
                            volume
                                .pointer("/secret/secretName")
                                .and_then(Value::as_str)
                                .map(str::to_owned)
                        })
                        .flatten()
                })
            })
            .ok_or(AnsibleProbeJobError::CredentialsInvalid)?;
        let labels = metadata.get("labels").cloned().unwrap_or_else(|| json!({}));
        let annotations = metadata
            .get("annotations")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let private_key_secret = json!({
            "apiVersion":"v1",
            "kind":"Secret",
            "metadata":{
                "name":private_key_secret_name,
                "namespace":self.namespace,
                "labels":labels,
                "annotations":annotations,
                "ownerReferences":[],
            },
            "immutable":true,
            "type":"Opaque",
            "data":{"key":STANDARD.encode(private_key_openssh.as_bytes())},
        });
        let ssh_certificate_secret = json!({
            "apiVersion":"v1",
            "kind":"Secret",
            "metadata":{
                "name":certificate_secret_name,
                "namespace":self.namespace,
                "labels":metadata.get("labels").cloned().unwrap_or_else(|| json!({})),
                "annotations":metadata.get("annotations").cloned().unwrap_or_else(|| json!({})),
                "ownerReferences":[],
            },
            "immutable":true,
            "type":"Opaque",
            "data":{"cert.pub":STANDARD.encode(certificate_openssh.as_bytes())},
        });
        self.ssh_private_key_secret_name = Some(private_key_secret_name);
        self.ssh_certificate_secret_name = Some(certificate_secret_name);
        self.ssh_private_key_secret = Some(private_key_secret);
        self.ssh_certificate_secret = Some(ssh_certificate_secret);
        Ok(())
    }

    #[must_use]
    pub fn cleanup_plan(&self) -> Vec<AnsibleProbeCleanupTarget> {
        [
            ("jobs", self.name.as_str()),
            ("networkpolicies", self.name.as_str()),
            ("configmaps", self.name.as_str()),
            ("secrets", self.materializer_secret_name.as_str()),
        ]
        .into_iter()
        .chain(
            self.ssh_private_key_secret_name
                .as_deref()
                .map(|name| ("secrets", name)),
        )
        .chain(
            self.ssh_certificate_secret_name
                .as_deref()
                .map(|name| ("secrets", name)),
        )
        .map(|(resource, name)| AnsibleProbeCleanupTarget {
            namespace: self.namespace.clone(),
            resource: resource.to_owned(),
            name: name.to_owned(),
            propagation_policy: "Foreground".to_owned(),
        })
        .collect()
    }

    pub(crate) fn document_for_target(&self, target: &AnsibleProbeCleanupTarget) -> Option<&Value> {
        match target.resource.as_str() {
            "jobs" if target.name == self.name => Some(&self.job),
            "networkpolicies" if target.name == self.name => Some(&self.network_policy),
            "configmaps" if target.name == self.name => Some(&self.config_map),
            "secrets" if target.name == self.materializer_secret_name => {
                Some(&self.materializer_secret)
            }
            "secrets"
                if self.ssh_private_key_secret_name.as_deref() == Some(target.name.as_str()) =>
            {
                self.ssh_private_key_secret.as_ref()
            }
            "secrets"
                if self.ssh_certificate_secret_name.as_deref() == Some(target.name.as_str()) =>
            {
                self.ssh_certificate_secret.as_ref()
            }
            _ => None,
        }
    }
}

fn materializer_env(binding: &AnsibleProbeJobBinding) -> Value {
    let mut env = vec![json!({
        "name":"LABWEAVER_ARTIFACT_MATERIALIZER_COMMAND_FILE",
        "value":DEFAULT_MATERIALIZER_COMMAND_PATH,
    })];
    if binding.materializer_ca_bundle.is_some() {
        env.push(json!({
            "name":"LABWEAVER_ARTIFACT_MATERIALIZER_CA_FILE",
            "value":crate::materializer::DEFAULT_MATERIALIZER_CA_FILE,
        }));
    }
    Value::Array(env)
}

fn materializer_volume(binding: &AnsibleProbeJobBinding, secret_name: &str) -> Value {
    let mut items = vec![json!({"key":"command.json","path":"command.json"})];
    if binding.materializer_ca_bundle.is_some() {
        items.push(json!({"key":"ca.crt","path":"ca.crt"}));
    }
    json!({"secretName":secret_name,"items":items})
}

fn validate_binding(binding: &AnsibleProbeJobBinding) -> Result<(), AnsibleProbeJobError> {
    if !is_dns_name(&binding.namespace)
        || !is_dns_name(&binding.service_account_name)
        || !is_dns_name(&binding.image_pull_secret_name)
        || !image_matches_request(&binding.worker_image, &binding.request.runner_image_digest)
    {
        return Err(AnsibleProbeJobError::BindingInvalid);
    }
    binding
        .materializer
        .validate()
        .map_err(|_| AnsibleProbeJobError::MaterializerInvalid)?;
    if !binding
        .materializer
        .artifacts
        .iter()
        .any(|artifact| artifact.destination == MaterializeDestination::Evaluator)
    {
        return Err(AnsibleProbeJobError::MaterializerInvalid);
    }
    Ok(())
}

/// The worker image must be digest-pinned and carry exactly the digest the
/// request pins; registry and repository prefixes may differ.
fn image_matches_request(worker_image: &str, runner_image_digest: &str) -> bool {
    if !is_sha256_image(worker_image) {
        return false;
    }
    runner_image_digest
        .rsplit_once('@')
        .is_some_and(|(_, digest)| worker_image.ends_with(&format!("@{digest}")))
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

#[derive(Debug, Error, Eq, PartialEq)]
pub enum AnsibleProbeJobError {
    #[error("ansible probe Job binding is invalid")]
    BindingInvalid,
    #[error("ansible probe worker command is invalid")]
    CommandInvalid,
    #[error("ansible probe worker command exceeds the bounded ConfigMap size")]
    CommandTooLarge,
    #[error("ansible probe materializer command is invalid")]
    MaterializerInvalid,
    #[error("ansible probe SSH credentials are invalid")]
    CredentialsInvalid,
    #[error("ansible probe materializer command exceeds the bounded Secret size")]
    MaterializerTooLarge,
}

impl AnsibleProbeJobError {
    #[must_use]
    pub const fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::BindingInvalid => "LW_AP_JOB_BINDING_INVALID",
            Self::CommandInvalid => "LW_AP_JOB_COMMAND_INVALID",
            Self::CommandTooLarge => "LW_AP_JOB_COMMAND_TOO_LARGE",
            Self::MaterializerInvalid => "LW_AP_MATERIALIZER_INVALID",
            Self::CredentialsInvalid => "LW_AP_SSH_CREDENTIALS_INVALID",
            Self::MaterializerTooLarge => "LW_AP_MATERIALIZER_TOO_LARGE",
        }
    }
}
