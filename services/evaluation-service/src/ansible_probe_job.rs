//! Kubernetes resource planning for one isolated read-only Ansible probe attempt.
//!
//! The plan is a fixed three-object bundle: an immutable `ConfigMap` carrying the
//! validated execution request, an attempt-scoped `NetworkPolicy` that denies all
//! ingress and all egress except TCP/22 to the exact probe target IPv4, and a
//! batch/v1 Job running the shell-free probe worker as UID/GID 65532 with a
//! read-only root filesystem. SSH identity material is never copied into the
//! plan: the Job mounts the two request-referenced Secrets read-only at
//! `/run/secrets/probe/private-key/key` (Secret data key `key`) and
//! `/run/secrets/probe/certificate/cert.pub` (Secret data key `cert.pub`).
#![allow(
    missing_docs,
    clippy::too_many_lines,
    reason = "the fixed resource plan is intentionally colocated for security review"
)]

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

use crate::{
    ansible_probe::{AnsibleProbeExecutionRequest, SSH_PORT},
    oj::is_sha256_image,
};

const MAX_COMMAND_BYTES: usize = 1024 * 1024;
const WORKER_UID: u32 = 65_532;
/// Fixed bounded margin on top of the probe wall time for image pull, volume
/// setup, and termination; the deadline never exceeds 330 seconds.
const ACTIVE_DEADLINE_MARGIN_SECONDS: u64 = 30;
const WORK_VOLUME_SIZE_LIMIT: &str = "64Mi";
const EVIDENCE_VOLUME_SIZE_LIMIT: &str = "16Mi";
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
    namespace: String,
    pub config_map: Value,
    pub network_policy: Value,
    pub job: Value,
}

impl AnsibleProbeJobResources {
    /// Builds a deterministic, fail-closed Kubernetes resource bundle.
    ///
    /// # Errors
    ///
    /// Returns a stable [`AnsibleProbeJobError`] for an invalid request, binding,
    /// or command document.
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

        let name = format!(
            "lw-ap-{}",
            &binding.request.attempt_id.simple().to_string()[..20]
        );
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
        // Standard networking.k8s.io/v1 semantics: no ingress rule, and the only
        // egress permitted is TCP/22 to the validated private target IPv4. DNS
        // resolution is deliberately unavailable inside the probe Pod.
        let network_policy = json!({
            "apiVersion":"networking.k8s.io/v1",
            "kind":"NetworkPolicy",
            "metadata":{"name":name,"namespace":binding.namespace,"labels":labels,"annotations":annotations},
            "spec":{
                "podSelector":{"matchLabels":{"labweaver.io/attempt-id":binding.request.attempt_id.to_string()}},
                "policyTypes":["Ingress","Egress"],
                "ingress":[],
                "egress":[{
                    "to":[{"ipBlock":{"cidr":target_egress_cidr}}],
                    "ports":[{"protocol":"TCP","port":SSH_PORT}],
                }],
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
                            "name":"ansible-probe",
                            "image":binding.worker_image,
                            "imagePullPolicy":"IfNotPresent",
                            "args":["--mode","ansible-probe-worker"],
                            "env":[{
                                "name":"LABWEAVER_ANSIBLE_PROBE_COMMAND_FILE",
                                "value":"/command/command.json",
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
                                "limits":{"cpu":"500m","memory":"512Mi","ephemeral-storage":"256Mi"},
                            },
                            "volumeMounts":[
                                {"name":"command","mountPath":"/command","readOnly":true},
                                {"name":"ssh-private-key","mountPath":"/run/secrets/probe/private-key","readOnly":true},
                                {"name":"ssh-certificate","mountPath":"/run/secrets/probe/certificate","readOnly":true},
                                {"name":"work","mountPath":"/work"},
                                {"name":"evidence","mountPath":"/evidence"},
                            ],
                        }],
                        "volumes":[
                            {"name":"command","configMap":{"name":name,"items":[{"key":"command.json","path":"command.json"}]}},
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
    pub fn cleanup_plan(&self) -> Vec<AnsibleProbeCleanupTarget> {
        ["jobs", "networkpolicies", "configmaps"]
            .into_iter()
            .map(|resource| AnsibleProbeCleanupTarget {
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

fn validate_binding(binding: &AnsibleProbeJobBinding) -> Result<(), AnsibleProbeJobError> {
    if !is_dns_name(&binding.namespace)
        || !is_dns_name(&binding.service_account_name)
        || !is_dns_name(&binding.image_pull_secret_name)
        || !image_matches_request(&binding.worker_image, &binding.request.runner_image_digest)
    {
        return Err(AnsibleProbeJobError::BindingInvalid);
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
}

impl AnsibleProbeJobError {
    #[must_use]
    pub const fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::BindingInvalid => "LW_AP_JOB_BINDING_INVALID",
            Self::CommandInvalid => "LW_AP_JOB_COMMAND_INVALID",
            Self::CommandTooLarge => "LW_AP_JOB_COMMAND_TOO_LARGE",
        }
    }
}
