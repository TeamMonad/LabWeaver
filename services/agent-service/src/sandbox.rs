//! Authoring sandbox Job bundle construction.
//!
//! Agent-owned one-shot authoring attempts execute inside a dedicated Kubernetes Job in the
//! `labweaver-authoring` namespace. This module renders the immutable bundle (attempt Secret,
//! per-attempt `NetworkPolicy` and the Job with its materialize init container) that the shared
//! execution backend applies, observes and cleans up. It never talks to Kubernetes itself and it
//! never interprets a model result.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde_json::{Value, json};
use task_execution::kubernetes::{
    KubernetesCleanupTarget, KubernetesJobBundle, KubernetesJobIdentity, KubernetesObject,
    KubernetesOwnership,
};
use uuid::Uuid;

/// The managed-by label value that marks Agent-owned sandbox objects.
pub const SANDBOX_MANAGED_BY: &str = "agent-service";
/// Kubernetes log event scope for the Agent execution backend.
pub const SANDBOX_EVENT_SCOPE: &str = "agent";
/// Namespace-wide default-deny policy that must exist before any attempt is admitted.
pub const SANDBOX_DEFAULT_DENY_POLICY: &str = "authoring-default-deny";
/// Main container that runs the pinned Claude Code CLI.
pub const SANDBOX_MAIN_CONTAINER: &str = "claude-code";

const MANAGED_BY_LABEL: &str = "labweaver.io/managed-by";
const RUN_ID_LABEL: &str = "labweaver.io/run-id";
const STEP_RUN_ID_LABEL: &str = "labweaver.io/step-run-id";
const ATTEMPT_ID_LABEL: &str = "labweaver.io/attempt-id";
const REQUEST_SHA_ANNOTATION: &str = "labweaver.io/request-sha256";
const ATTEMPT_VOLUME: &str = "attempt";
const WORKSPACE_VOLUME: &str = "workspace";
const ATTEMPT_DIR: &str = "/run/labweaver";
const WORKSPACE_DIR: &str = "/workspace";
const ATTEMPT_VOLUME_BYTES: u64 = 64 * 1024 * 1024;

/// Rejected sandbox configuration or attempt specification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SandboxBundleError {
    /// A configuration or attempt value is malformed or out of bounds.
    Invalid,
}

impl std::fmt::Display for SandboxBundleError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("LW_AGENT_SANDBOX_BUNDLE_INVALID")
    }
}

impl std::error::Error for SandboxBundleError {}

/// Deployment-owned limits for one authoring sandbox attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SandboxConfiguration {
    /// Execution namespace that must match the admitted Resource namespace.
    pub namespace: String,
    /// Digest-pinned sandbox image (Claude Code CLI, shell, curl and coreutils).
    pub image: String,
    /// Service account used by the attempt Job; it must not mount a token.
    pub service_account_name: String,
    /// Optional digest-pinned pull secret for the sandbox image.
    pub image_pull_secret_name: Option<String>,
    /// CPU request and limit in millicores.
    pub cpu_millicores: u32,
    /// Memory request and limit in bytes.
    pub memory_bytes: u64,
    /// Upper bound on the writable workspace volume.
    pub workspace_bytes: u64,
    /// Wall-clock deadline for the attempt Job.
    pub wall_time_seconds: u64,
    /// Allowed non-DNS egress destinations (Harbor, object store, model endpoint).
    pub allowed_egress_cidrs: BTreeSet<String>,
}

impl SandboxConfiguration {
    /// Validates every bound before a bundle can be rendered.
    ///
    /// # Errors
    ///
    /// Returns [`SandboxBundleError::Invalid`] for empty, oversized or inconsistent values.
    pub fn validate(&self) -> Result<(), SandboxBundleError> {
        if !valid_namespace(&self.namespace)
            || self.image.trim().is_empty()
            || !digest_pinned(&self.image)
            || self.service_account_name.trim().is_empty()
            || self.cpu_millicores == 0
            || self.memory_bytes == 0
            || self.workspace_bytes == 0
            || !(60..=86_400).contains(&self.wall_time_seconds)
            || self.allowed_egress_cidrs.is_empty()
            || self
                .allowed_egress_cidrs
                .iter()
                .any(|cidr| !valid_cidr(cidr))
        {
            return Err(SandboxBundleError::Invalid);
        }
        Ok(())
    }
}

/// Everything one admitted attempt needs to render its immutable bundle.
#[derive(Clone, Debug)]
pub struct SandboxAttemptSpec {
    /// Durable one-shot task identity; also the deterministic object-name suffix.
    pub task_run_id: Uuid,
    /// Role-neutral ownership identity persisted with the execution binding.
    pub ownership: KubernetesOwnership,
    /// Request correlation identity.
    pub trace_id: String,
    /// Exact Claude Code argv executed inside the sandbox.
    pub command: Vec<String>,
    /// Environment entries visible to the Claude Code process.
    pub command_environment: BTreeMap<String, String>,
    /// Presigned material download URL.
    pub material_download_url: String,
    /// Expected sha256 of the material envelope object.
    pub material_sha256: String,
    /// Expected size of the material envelope object.
    pub material_size_bytes: u64,
    /// Presigned result upload URL.
    pub result_upload_url: String,
    /// Headers required by the result upload.
    pub result_upload_headers: BTreeMap<String, String>,
    /// Maximum accepted result size.
    pub result_max_bytes: u64,
    /// Optional base64-encoded object-store CA bundle mounted for curl.
    pub object_store_ca_base64: Option<String>,
}

/// Rendered attempt bundle that keeps the deterministic object names for recovery.
#[derive(Clone, Debug)]
pub struct SandboxBundle {
    /// Exact bundle applied by the shared execution backend.
    pub bundle: KubernetesJobBundle,
    /// Deterministic Secret name.
    pub secret_name: String,
    /// Deterministic per-attempt `NetworkPolicy` name.
    pub network_policy_name: String,
    /// Deterministic Job name.
    pub job_name: String,
}

/// Renders the immutable attempt bundle for one admitted sandbox attempt.
///
/// # Errors
///
/// Returns [`SandboxBundleError::Invalid`] for any malformed configuration or attempt value.
#[allow(clippy::too_many_lines)]
pub fn build_sandbox_bundle(
    configuration: &SandboxConfiguration,
    spec: &SandboxAttemptSpec,
) -> Result<SandboxBundle, SandboxBundleError> {
    configuration.validate()?;
    if spec.command.is_empty()
        || spec.command.len() > 32
        || spec.command.iter().any(|value| value.len() > 4_096)
        || spec.trace_id.trim().is_empty()
        || spec.trace_id.len() > 128
        || !valid_sha256(&spec.material_sha256)
        || spec.material_size_bytes == 0
        || spec.material_size_bytes > 16 * 1024 * 1024
        || !spec.material_download_url.starts_with("https://")
        || !spec.result_upload_url.starts_with("https://")
        || !(1_024..=8 * 1024 * 1024).contains(&spec.result_max_bytes)
        || spec
            .command_environment
            .keys()
            .any(|key| !valid_env_name(key) || control_env_name(key))
        || spec.command_environment.values().any(|value| {
            value.is_empty() || value.len() > 4_096 || value.chars().any(char::is_control)
        })
        || spec
            .result_upload_headers
            .iter()
            .any(|(name, value)| !valid_header(name, value))
        || spec
            .object_store_ca_base64
            .as_ref()
            .is_some_and(|ca| ca.is_empty() || ca.len() > 256 * 1024)
    {
        return Err(SandboxBundleError::Invalid);
    }
    let suffix = &spec.task_run_id.simple().to_string()[..20];
    let secret_name = format!("lw-auth-secret-{suffix}");
    let network_policy_name = format!("lw-auth-net-{suffix}");
    let job_name = format!("lw-auth-{suffix}");

    let mut secret_data = BTreeMap::new();
    secret_data.insert(
        "MATERIAL_DOWNLOAD_URL".to_owned(),
        spec.material_download_url.clone(),
    );
    secret_data.insert("MATERIAL_SHA256".to_owned(), spec.material_sha256.clone());
    secret_data.insert(
        "MATERIAL_SIZE_BYTES".to_owned(),
        spec.material_size_bytes.to_string(),
    );
    secret_data.insert(
        "RESULT_UPLOAD_URL".to_owned(),
        spec.result_upload_url.clone(),
    );
    secret_data.insert(
        "RESULT_MAX_BYTES".to_owned(),
        spec.result_max_bytes.to_string(),
    );
    for (name, value) in &spec.command_environment {
        secret_data.insert(name.clone(), value.clone());
    }
    for (index, (name, value)) in spec.result_upload_headers.iter().enumerate() {
        secret_data.insert(format!("RESULT_HEADER_{index}"), format!("{name}: {value}"));
    }
    if let Some(ca) = &spec.object_store_ca_base64 {
        secret_data.insert("OBJECT_STORE_CA_BASE64".to_owned(), ca.clone());
    }

    let secret_document = secret_document(
        &secret_name,
        &configuration.namespace,
        &spec.ownership,
        &secret_data,
    );
    let network_policy =
        network_policy_document(configuration, &network_policy_name, &spec.ownership);
    let job = job_document(configuration, &job_name, &secret_name, spec);

    let identity = KubernetesJobIdentity {
        namespace: configuration.namespace.clone(),
        job_name: job_name.clone(),
        main_container: SANDBOX_MAIN_CONTAINER,
        default_deny_policy: SANDBOX_DEFAULT_DENY_POLICY,
        deadline_diagnostic_code: "LW_AGENT_SANDBOX_DEADLINE_EXCEEDED",
        failed_diagnostic_code: "LW_AGENT_SANDBOX_FAILED",
        oom_diagnostic_code: "LW_AGENT_SANDBOX_MEMORY_LIMIT",
        stable_diagnostic_prefix: "LW_AGENT_",
        ownership: spec.ownership.clone(),
        trace_id: spec.trace_id.clone(),
    };
    let objects = vec![
        KubernetesObject {
            api_version: "v1",
            plural: "secrets",
            name: secret_name.clone(),
            document: secret_document,
        },
        KubernetesObject {
            api_version: "networking.k8s.io/v1",
            plural: "networkpolicies",
            name: network_policy_name.clone(),
            document: network_policy,
        },
        KubernetesObject {
            api_version: "batch/v1",
            plural: "jobs",
            name: job_name.clone(),
            document: job,
        },
    ];
    let cleanup_plan = vec![
        cleanup_target(&configuration.namespace, "jobs", &job_name),
        cleanup_target(
            &configuration.namespace,
            "networkpolicies",
            &network_policy_name,
        ),
        cleanup_target(&configuration.namespace, "secrets", &secret_name),
    ];
    Ok(SandboxBundle {
        bundle: KubernetesJobBundle {
            identity,
            objects,
            cleanup_plan,
        },
        secret_name,
        network_policy_name,
        job_name,
    })
}

/// Renders one shell command line from a trusted argv with strict quoting.
#[must_use]
pub fn shell_command(argv: &[String]) -> String {
    argv.iter()
        .map(|argument| shell_quote(argument))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_quote(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('\'');
    for character in value.chars() {
        if character == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(character);
        }
    }
    quoted.push('\'');
    quoted
}

fn secret_document(
    name: &str,
    namespace: &str,
    ownership: &KubernetesOwnership,
    data: &BTreeMap<String, String>,
) -> Value {
    let string_data = data
        .iter()
        .map(|(key, value)| (key.clone(), Value::String(value.clone())))
        .collect::<serde_json::Map<_, _>>();
    json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": name,
            "namespace": namespace,
            "labels": ownership_labels(ownership),
            "annotations": { REQUEST_SHA_ANNOTATION: ownership.request_sha256 },
        },
        "type": "Opaque",
        "stringData": string_data,
        "immutable": true,
    })
}

fn network_policy_document(
    configuration: &SandboxConfiguration,
    name: &str,
    ownership: &KubernetesOwnership,
) -> Value {
    let mut egress = vec![json!({
        "to": [{"namespaceSelector": {"matchLabels": {"kubernetes.io/metadata.name": "kube-system"}}}],
        "ports": [
            {"protocol": "UDP", "port": 53},
            {"protocol": "TCP", "port": 53},
        ],
    })];
    if !configuration.allowed_egress_cidrs.is_empty() {
        egress.push(json!({
            "to": configuration
                .allowed_egress_cidrs
                .iter()
                .map(|cidr| json!({"ipBlock": {"cidr": cidr}}))
                .collect::<Vec<_>>(),
            "ports": [{"protocol": "TCP", "port": 443}],
        }));
    }
    json!({
        "apiVersion": "networking.k8s.io/v1",
        "kind": "NetworkPolicy",
        "metadata": {
            "name": name,
            "namespace": configuration.namespace,
            "labels": ownership_labels(ownership),
            "annotations": { REQUEST_SHA_ANNOTATION: ownership.request_sha256 },
        },
        "spec": {
            "podSelector": {"matchLabels": pod_selector_labels(ownership)},
            "policyTypes": ["Ingress", "Egress"],
            "ingress": [],
            "egress": egress,
        },
    })
}

#[allow(clippy::too_many_lines)]
fn job_document(
    configuration: &SandboxConfiguration,
    name: &str,
    secret_name: &str,
    spec: &SandboxAttemptSpec,
) -> Value {
    let mut environment = vec![
        json!({
            "name": "POD_NAME",
            "valueFrom": {"fieldRef": {"fieldPath": "metadata.name"}},
        }),
        json!({
            "name": "SSL_CERT_FILE",
            "value": format!("{ATTEMPT_DIR}/ca.pem"),
        }),
    ];
    if spec.object_store_ca_base64.is_none() {
        environment.pop();
    }

    let mut script = String::new();
    script.push_str("set -eu\numask 077\n");
    let _ = writeln!(script, "cd {WORKSPACE_DIR}");
    if spec.object_store_ca_base64.is_some() {
        let _ = writeln!(
            script,
            "printf '%s' \"$OBJECT_STORE_CA_BASE64\" | base64 -d > {ATTEMPT_DIR}/ca.pem"
        );
    }
    script.push_str("set +e\n");
    let _ = writeln!(
        script,
        "{} < {ATTEMPT_DIR}/input.json > {ATTEMPT_DIR}/result.json 2> {ATTEMPT_DIR}/stderr.log",
        shell_command(&spec.command)
    );
    script.push_str("code=$?\nset -e\n");
    let _ = writeln!(
        script,
        "size=$(wc -c < {ATTEMPT_DIR}/result.json | tr -d ' ')"
    );
    script.push_str("if [ \"$size\" -gt \"$RESULT_MAX_BYTES\" ]; then\n");
    script.push_str(
        "  printf '{\"failure\":\"LW_AGENT_SANDBOX_RESULT_TOO_LARGE\"}' > /dev/termination-log\n  exit 78\nfi\n",
    );
    script.push_str(
        "curl --fail --silent --show-error --location --retry 2 --max-time 300 --request PUT --upload-file",
    );
    let _ = write!(script, " {ATTEMPT_DIR}/result.json");
    for index in 0..spec.result_upload_headers.len() {
        let _ = write!(script, " --header \"$RESULT_HEADER_{index}\"");
    }
    script.push_str(" \"$RESULT_UPLOAD_URL\"\n");
    let _ = writeln!(
        script,
        "sum=$(sha256sum {ATTEMPT_DIR}/result.json | cut -d' ' -f1)"
    );
    script.push_str(
        "printf '{\"resultSizeBytes\":%s,\"resultSha256\":\"%s\",\"exitCode\":%s}' \"$size\" \"$sum\" \"$code\" > /dev/termination-log\nexit \"$code\"\n",
    );

    let container_security = json!({
        "allowPrivilegeEscalation": false,
        "capabilities": {"drop": ["ALL"]},
        "readOnlyRootFilesystem": true,
        "runAsNonRoot": true,
        "runAsUser": 65532,
        "runAsGroup": 65532,
        "seccompProfile": {"type": "RuntimeDefault"},
    });
    json!({
        "apiVersion": "batch/v1",
        "kind": "Job",
        "metadata": {
            "name": name,
            "namespace": configuration.namespace,
            "labels": ownership_labels(&spec.ownership),
            "annotations": { REQUEST_SHA_ANNOTATION: spec.ownership.request_sha256 },
        },
        "spec": {
            "backoffLimit": 0,
            "activeDeadlineSeconds": configuration.wall_time_seconds,
            "ttlSecondsAfterFinished": 300,
            "template": {
                "metadata": {
                    "labels": pod_selector_labels(&spec.ownership),
                },
                "spec": {
                    "automountServiceAccountToken": false,
                    "restartPolicy": "Never",
                    "serviceAccountName": configuration.service_account_name,
                    "securityContext": {
                        "runAsNonRoot": true,
                        "runAsUser": 65532,
                        "runAsGroup": 65532,
                        "fsGroup": 65532,
                        "seccompProfile": {"type": "RuntimeDefault"},
                    },
                    "imagePullSecrets": configuration
                        .image_pull_secret_name
                        .iter()
                        .map(|name| json!({"name": name}))
                        .collect::<Vec<_>>(),
                    "initContainers": [{
                        "name": "materialize",
                        "image": configuration.image,
                        "imagePullPolicy": "IfNotPresent",
                        "command": ["/bin/sh", "-c", materialize_script()],
                        "envFrom": [{"secretRef": {"name": secret_name, "optional": false}}],
                        "securityContext": {
                            "allowPrivilegeEscalation": false,
                            "capabilities": {"drop": ["ALL"]},
                            "readOnlyRootFilesystem": true,
                            "runAsNonRoot": true,
                            "runAsUser": 65532,
                            "runAsGroup": 65532,
                            "seccompProfile": {"type": "RuntimeDefault"},
                        },
                        "volumeMounts": [
                            {"name": ATTEMPT_VOLUME, "mountPath": ATTEMPT_DIR, "readOnly": false},
                        ],
                    }],
                    "containers": [{
                        "name": SANDBOX_MAIN_CONTAINER,
                        "image": configuration.image,
                        "imagePullPolicy": "IfNotPresent",
                        "command": ["/bin/sh", "-c", script],
                        "env": environment,
                        "envFrom": [{"secretRef": {"name": secret_name, "optional": false}}],
                        "resources": {
                            "requests": {
                                "cpu": format!("{}m", configuration.cpu_millicores),
                                "memory": configuration.memory_bytes.to_string(),
                                "ephemeral-storage": configuration.workspace_bytes.to_string(),
                            },
                            "limits": {
                                "cpu": format!("{}m", configuration.cpu_millicores),
                                "memory": configuration.memory_bytes.to_string(),
                                "ephemeral-storage": configuration.workspace_bytes.to_string(),
                            },
                        },
                        "securityContext": container_security,
                        "volumeMounts": [
                            {"name": ATTEMPT_VOLUME, "mountPath": ATTEMPT_DIR, "readOnly": false},
                            {"name": WORKSPACE_VOLUME, "mountPath": WORKSPACE_DIR, "readOnly": false},
                        ],
                    }],
                    "volumes": [
                        {
                            "name": ATTEMPT_VOLUME,
                            "emptyDir": {"sizeLimit": ATTEMPT_VOLUME_BYTES.to_string()},
                        },
                        {
                            "name": WORKSPACE_VOLUME,
                            "emptyDir": {"sizeLimit": configuration.workspace_bytes.to_string()},
                        },
                    ],
                },
            },
        },
    })
}

fn materialize_script() -> &'static str {
    "set -eu\n\
     umask 077\n\
     curl --fail --silent --show-error --location --retry 3 --max-time 300 \
     -o /run/labweaver/input.json \"$MATERIAL_DOWNLOAD_URL\"\n\
     size=$(wc -c < /run/labweaver/input.json | tr -d ' ')\n\
     if [ \"$size\" != \"$MATERIAL_SIZE_BYTES\" ]; then exit 74; fi\n\
     printf '%s  /run/labweaver/input.json\\n' \"$MATERIAL_SHA256\" | sha256sum --check --strict\n"
}

fn ownership_labels(ownership: &KubernetesOwnership) -> Value {
    json!({
        MANAGED_BY_LABEL: SANDBOX_MANAGED_BY,
        RUN_ID_LABEL: ownership.run_id.to_string(),
        STEP_RUN_ID_LABEL: ownership.step_run_id.to_string(),
        ATTEMPT_ID_LABEL: ownership.attempt_id.to_string(),
    })
}

fn pod_selector_labels(ownership: &KubernetesOwnership) -> Value {
    json!({
        MANAGED_BY_LABEL: SANDBOX_MANAGED_BY,
        ATTEMPT_ID_LABEL: ownership.attempt_id.to_string(),
    })
}

fn cleanup_target(namespace: &str, resource: &str, name: &str) -> KubernetesCleanupTarget {
    KubernetesCleanupTarget {
        namespace: namespace.to_owned(),
        resource: resource.to_owned(),
        name: name.to_owned(),
        propagation_policy: "Foreground".to_owned(),
    }
}

fn control_env_name(value: &str) -> bool {
    value.starts_with("MATERIAL_")
        || value.starts_with("RESULT_")
        || value.starts_with("OBJECT_STORE_")
        || value == "POD_NAME"
        || value == "SSL_CERT_FILE"
}

fn valid_namespace(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !value.starts_with('-')
        && !value.ends_with('-')
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn digest_pinned(value: &str) -> bool {
    value
        .rsplit_once('@')
        .is_some_and(|(_, digest)| digest.starts_with("sha256:") && valid_sha256(&digest[7..]))
}

fn valid_cidr(value: &str) -> bool {
    let Some((address, prefix)) = value.split_once('/') else {
        return false;
    };
    let Ok(prefix) = prefix.parse::<u8>() else {
        return false;
    };
    if address.contains(':') {
        !address.is_empty() && prefix <= 128
    } else {
        address.split('.').count() == 4
            && address.split('.').all(|part| part.parse::<u8>().is_ok())
            && prefix <= 32
    }
}

fn valid_env_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

fn valid_header(name: &str, value: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        && !value.is_empty()
        && value.len() <= 512
        && !value.contains(['\r', '\n'])
        && !value.contains('"')
        && !value.contains('\'')
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use task_execution::kubernetes::KubernetesOwnership;
    use uuid::Uuid;

    use super::{
        SandboxAttemptSpec, SandboxBundleError, SandboxConfiguration, build_sandbox_bundle,
        shell_quote,
    };

    fn configuration() -> SandboxConfiguration {
        SandboxConfiguration {
            namespace: "labweaver-authoring".to_owned(),
            image: "harbor.lab.lan/labweaver-system/authoring-sandbox@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
            service_account_name: "authoring-runner".to_owned(),
            image_pull_secret_name: Some("harbor-course-pull".to_owned()),
            cpu_millicores: 2_000,
            memory_bytes: 2 * 1024 * 1024 * 1024,
            workspace_bytes: 2 * 1024 * 1024 * 1024,
            wall_time_seconds: 3_600,
            allowed_egress_cidrs: BTreeSet::from(["10.0.0.0/8".to_owned()]),
        }
    }

    fn spec() -> SandboxAttemptSpec {
        SandboxAttemptSpec {
            task_run_id: Uuid::now_v7(),
            ownership: KubernetesOwnership {
                run_id: Uuid::now_v7(),
                step_run_id: Uuid::now_v7(),
                attempt_id: Uuid::now_v7(),
                request_sha256: "b".repeat(64),
            },
            trace_id: "trace-1".to_owned(),
            command: vec!["claude".to_owned(), "--bare".to_owned()],
            command_environment: BTreeMap::from([
                (
                    "ANTHROPIC_BASE_URL".to_owned(),
                    "https://model.example".to_owned(),
                ),
                ("ANTHROPIC_MODEL".to_owned(), "model-1".to_owned()),
                ("ANTHROPIC_AUTH_TOKEN".to_owned(), "secret-token".to_owned()),
            ]),
            material_download_url: "https://objects.example/material?sig=1".to_owned(),
            material_sha256: "c".repeat(64),
            material_size_bytes: 1_024,
            result_upload_url: "https://objects.example/result?sig=2".to_owned(),
            result_upload_headers: BTreeMap::from([("if-none-match".to_owned(), "*".to_owned())]),
            result_max_bytes: 4 * 1024 * 1024,
            object_store_ca_base64: Some("Q0E=".to_owned()),
        }
    }

    #[test]
    fn bundle_is_non_root_tokenless_and_bounded() -> Result<(), Box<dyn std::error::Error>> {
        let bundle = build_sandbox_bundle(&configuration(), &spec())?;
        let job = bundle
            .bundle
            .objects
            .iter()
            .find(|object| object.plural == "jobs")
            .ok_or(SandboxBundleError::Invalid)?;
        assert_eq!(
            job.document
                .pointer("/spec/template/spec/automountServiceAccountToken"),
            Some(&serde_json::Value::Bool(false))
        );
        assert_eq!(
            job.document.pointer("/spec/activeDeadlineSeconds"),
            Some(&serde_json::json!(3_600))
        );
        assert_eq!(
            job.document.pointer("/spec/ttlSecondsAfterFinished"),
            Some(&serde_json::json!(300))
        );
        let container = &job.document["spec"]["template"]["spec"]["containers"][0];
        assert_eq!(container["name"], "claude-code");
        assert_eq!(container["securityContext"]["runAsNonRoot"], true);
        assert_eq!(
            container["securityContext"]["allowPrivilegeEscalation"],
            false
        );
        assert_eq!(
            container["securityContext"]["capabilities"]["drop"],
            serde_json::json!(["ALL"])
        );
        assert_eq!(container["securityContext"]["readOnlyRootFilesystem"], true);
        let init = &job.document["spec"]["template"]["spec"]["initContainers"][0];
        assert_eq!(init["name"], "materialize");
        assert!(init["command"][2].as_str().is_some_and(|script| {
            script.contains("sha256sum --check --strict") && script.contains("MATERIAL_SIZE_BYTES")
        }));
        assert_eq!(
            job.document
                .pointer("/metadata/annotations/labweaver.io~1request-sha256"),
            Some(&serde_json::json!("b".repeat(64)))
        );
        assert_eq!(
            job.document
                .pointer("/metadata/labels/labweaver.io~1managed-by"),
            Some(&serde_json::json!("agent-service"))
        );
        Ok(())
    }

    #[test]
    fn main_container_uploads_the_result_and_reports_the_receipt()
    -> Result<(), Box<dyn std::error::Error>> {
        let bundle = build_sandbox_bundle(&configuration(), &spec())?;
        let job = bundle
            .bundle
            .objects
            .iter()
            .find(|object| object.plural == "jobs")
            .ok_or(SandboxBundleError::Invalid)?;
        let script = job.document["spec"]["template"]["spec"]["containers"][0]["command"][2]
            .as_str()
            .ok_or(SandboxBundleError::Invalid)?;
        assert!(script.contains("claude' '--bare' < /run/labweaver/input.json"));
        assert!(script.contains("--upload-file /run/labweaver/result.json"));
        assert!(script.contains("RESULT_HEADER_0"));
        assert!(script.contains("resultSha256"));
        Ok(())
    }

    #[test]
    fn network_policy_denies_ingress_and_restricts_egress() -> Result<(), Box<dyn std::error::Error>>
    {
        let bundle = build_sandbox_bundle(&configuration(), &spec())?;
        let policy = bundle
            .bundle
            .objects
            .iter()
            .find(|object| object.plural == "networkpolicies")
            .ok_or(SandboxBundleError::Invalid)?;
        assert_eq!(policy.document["spec"]["ingress"], serde_json::json!([]));
        assert_eq!(
            policy.document["spec"]["policyTypes"],
            serde_json::json!(["Ingress", "Egress"])
        );
        assert_eq!(
            policy.document["spec"]["egress"][1]["to"][0]["ipBlock"]["cidr"],
            "10.0.0.0/8"
        );
        Ok(())
    }

    #[test]
    fn cleanup_plan_only_names_attempt_owned_objects() -> Result<(), Box<dyn std::error::Error>> {
        let bundle = build_sandbox_bundle(&configuration(), &spec())?;
        let resources = bundle
            .bundle
            .cleanup_plan
            .iter()
            .map(|target| target.resource.as_str())
            .collect::<Vec<_>>();
        assert_eq!(resources, ["jobs", "networkpolicies", "secrets"]);
        assert!(bundle.bundle.cleanup_plan.iter().all(|target| {
            target.namespace == "labweaver-authoring" && target.propagation_policy == "Foreground"
        }));
        assert!(bundle.bundle.cleanup_plan.iter().all(|target| {
            target.name == bundle.job_name
                || target.name == bundle.network_policy_name
                || target.name == bundle.secret_name
        }));
        Ok(())
    }

    #[test]
    fn invalid_attempt_inputs_fail_closed() {
        let mut invalid = spec();
        invalid.result_upload_url = "http://objects.example/result".to_owned();
        assert_eq!(
            build_sandbox_bundle(&configuration(), &invalid).err(),
            Some(SandboxBundleError::Invalid)
        );
        let mut invalid = spec();
        invalid.material_sha256 = "C".repeat(64);
        assert_eq!(
            build_sandbox_bundle(&configuration(), &invalid).err(),
            Some(SandboxBundleError::Invalid)
        );
        let mut invalid = spec();
        invalid.command_environment.insert(
            "RESULT_UPLOAD_URL".to_owned(),
            "https://attacker.example".to_owned(),
        );
        assert_eq!(
            build_sandbox_bundle(&configuration(), &invalid).err(),
            Some(SandboxBundleError::Invalid)
        );
        let mut invalid = configuration();
        invalid.allowed_egress_cidrs.clear();
        assert_eq!(
            build_sandbox_bundle(&invalid, &spec()).err(),
            Some(SandboxBundleError::Invalid)
        );
        let mut invalid = configuration();
        invalid.image = "harbor.lab.lan/sandbox:latest".to_owned();
        assert_eq!(
            build_sandbox_bundle(&invalid, &spec()).err(),
            Some(SandboxBundleError::Invalid)
        );
    }

    #[test]
    fn shell_quoting_never_allows_command_injection() {
        assert_eq!(shell_quote("plain"), "'plain'");
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
        assert_eq!(shell_quote("; rm -rf /"), "'; rm -rf /'");
    }
}
