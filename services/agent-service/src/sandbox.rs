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
    KubernetesOwnership, SANDBOX_RUNTIME_CLASS, parse_egress_destination,
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
/// Rootless `BuildKit` sidecar that serves the attempt-local build socket.
pub const SANDBOX_BUILDKIT_CONTAINER: &str = "buildkit";

const MANAGED_BY_LABEL: &str = "labweaver.io/managed-by";
const RUN_ID_LABEL: &str = "labweaver.io/run-id";
const STEP_RUN_ID_LABEL: &str = "labweaver.io/step-run-id";
const ATTEMPT_ID_LABEL: &str = "labweaver.io/attempt-id";
const REQUEST_SHA_ANNOTATION: &str = "labweaver.io/request-sha256";
const ATTEMPT_VOLUME: &str = "attempt";
const WORKSPACE_VOLUME: &str = "workspace";
const MATERIALS_VOLUME: &str = "materials";
const BUILDKIT_RUN_VOLUME: &str = "buildkit-run";
const BUILDKIT_STATE_VOLUME: &str = "buildkit-state";
const BUILDKIT_CONFIG_VOLUME: &str = "buildkit-config";
const BUILDKIT_AUTH_VOLUME: &str = "buildkit-auth";
const BUILDKIT_RUNTIME_VOLUME: &str = "buildkit-runtime";
const BUILDKIT_TMP_VOLUME: &str = "buildkit-tmp";
const ATTEMPT_DIR: &str = "/run/labweaver";
const WORKSPACE_DIR: &str = "/workspace";
const MATERIALS_DIR: &str = "/materials";
const ATTEMPT_VOLUME_BYTES: u64 = 64 * 1024 * 1024;
const MATERIALS_VOLUME_BYTES: u64 = 32 * 1024 * 1024;
const BUILDKIT_RUN_DIR: &str = "/run/buildkit";
const BUILDKIT_SOCKET: &str = "/run/buildkit/buildkitd.sock";
const BUILDKIT_STATE_DIR: &str = "/home/user/.local/share/buildkit";
const BUILDKIT_CONFIG_PATH: &str = "/etc/buildkit/buildkitd.toml";
const BUILDKIT_CA_PATH: &str = "/etc/buildkit/registry-ca.crt";
/// Fixed workspace path the sandbox must export its OCI layout archive to.
const EXPORT_OUTPUT_PATH: &str = "/workspace/labweaver-export.tar";
const BUILDKIT_RUNTIME_DIR: &str = "/run/user/1000";
const BUILDKIT_DOCKER_CONFIG_DIR: &str = "/home/user/.docker";
/// Unprivileged identity every attempt container runs as.
const SANDBOX_ATTEMPT_USER: u64 = 65_532;
/// Group of the rootless `BuildKit` sidecar that owns the attempt-local socket.
const BUILDKIT_SIDECAR_GROUP: u64 = 1_000;
const BUILDKIT_RUN_VOLUME_BYTES: u64 = 64 * 1024 * 1024;
const BUILDKIT_TMP_VOLUME_BYTES: u64 = 256 * 1024 * 1024;

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
    /// Reviewed non-DNS egress destinations as `"<cidr>:<port>"` (object store, model endpoint).
    ///
    /// The port is part of the reviewed destination: an object store or model endpoint is not
    /// always HTTPS on 443, and the attempt must reach exactly the reviewed services.
    pub allowed_egress: BTreeSet<String>,
    /// Optional digest-pinned rootless `BuildKit` sidecar image.
    ///
    /// When set together with the `ConfigMap`, the attempt pod gains a tokenless rootless
    /// `BuildKit` daemon that may only pull from and push to the configured Harbor registry;
    /// the sandbox still never receives registry push credentials for the platform.
    pub buildkit_image: Option<String>,
    /// Optional `ConfigMap` holding the reviewed `buildkitd.toml` with the Harbor registry CA.
    pub buildkit_config_map_name: Option<String>,
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
            || self.allowed_egress.is_empty()
            || self
                .allowed_egress
                .iter()
                .any(|destination| parse_egress_destination(destination).is_none())
        {
            return Err(SandboxBundleError::Invalid);
        }
        match (&self.buildkit_image, &self.buildkit_config_map_name) {
            (None, None) => {}
            (Some(image), Some(config_map)) => {
                if !digest_pinned(image)
                    || !valid_dns_label(config_map)
                    || self.image_pull_secret_name.is_none()
                {
                    return Err(SandboxBundleError::Invalid);
                }
            }
            _ => return Err(SandboxBundleError::Invalid),
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
    /// Pinned Claude Code version the sandbox CLI must report before executing.
    pub expected_claude_version: String,
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
    /// Presigned stderr upload URL.
    pub stderr_upload_url: String,
    /// Headers required by the stderr upload.
    pub stderr_upload_headers: BTreeMap<String, String>,
    /// Maximum accepted result size.
    pub result_max_bytes: u64,
    /// Maximum uploaded stderr size.
    pub stderr_max_bytes: u64,
    /// Optional presigned upload URL for the attempt-built OCI layout archive.
    pub export_upload_url: Option<String>,
    /// Headers required by the layout upload.
    pub export_upload_headers: BTreeMap<String, String>,
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

/// Upper bound on the number of argv entries of one sandbox attempt.
const MAX_COMMAND_ARGUMENTS: usize = 32;
/// Upper bound on one argv entry.
///
/// Instructions travel as argv entries, while the bulk material (the egress envelope with verified
/// teacher files) is transferred separately and bounded by [`MAX_MATERIAL_BYTES`]. Generated
/// instruction prompts are a few kilobytes, so this bound keeps the rendered Job manifest small.
const MAX_COMMAND_ARGUMENT_BYTES: usize = 64 * 1024;
/// Upper bound on the total rendered argv of one sandbox attempt.
const MAX_COMMAND_BYTES: usize = 256 * 1024;
/// Upper bound on the transferred material envelope.
const MAX_MATERIAL_BYTES: u64 = 16 * 1024 * 1024;

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
        || spec.expected_claude_version.is_empty()
        || spec.expected_claude_version.len() > 64
        || spec
            .expected_claude_version
            .chars()
            .any(|character| character.is_control() || character == '"' || character == '\'')
        || spec.command.len() > MAX_COMMAND_ARGUMENTS
        || spec
            .command
            .iter()
            .any(|value| value.len() > MAX_COMMAND_ARGUMENT_BYTES)
        || spec.command.iter().map(String::len).sum::<usize>() > MAX_COMMAND_BYTES
        || spec.trace_id.trim().is_empty()
        || spec.trace_id.len() > 128
        || !valid_sha256(&spec.material_sha256)
        || spec.material_size_bytes == 0
        || spec.material_size_bytes > MAX_MATERIAL_BYTES
        || !spec.material_download_url.starts_with("https://")
        || !spec.result_upload_url.starts_with("https://")
        || !spec.stderr_upload_url.starts_with("https://")
        || !(1_024..=8 * 1024 * 1024).contains(&spec.result_max_bytes)
        || spec.stderr_max_bytes > 1024 * 1024
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
            .chain(spec.stderr_upload_headers.iter())
            .any(|(name, value)| !valid_header(name, value))
        || spec
            .object_store_ca_base64
            .as_ref()
            .is_some_and(|ca| ca.is_empty() || ca.len() > 256 * 1024)
        || spec
            .export_upload_url
            .as_ref()
            .is_some_and(|url| !url.starts_with("https://"))
        || (spec.export_upload_url.is_some()
            && (configuration.buildkit_image.is_none()
                || spec
                    .export_upload_headers
                    .iter()
                    .any(|(name, value)| !valid_header(name, value))))
        || (spec.export_upload_url.is_none() && !spec.export_upload_headers.is_empty())
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
    secret_data.insert(
        "STDERR_UPLOAD_URL".to_owned(),
        spec.stderr_upload_url.clone(),
    );
    secret_data.insert(
        "STDERR_MAX_BYTES".to_owned(),
        spec.stderr_max_bytes.to_string(),
    );
    secret_data.insert(
        "CLAUDE_CODE_VERSION".to_owned(),
        spec.expected_claude_version.clone(),
    );
    for (name, value) in &spec.command_environment {
        secret_data.insert(name.clone(), value.clone());
    }
    for (index, (name, value)) in spec.result_upload_headers.iter().enumerate() {
        secret_data.insert(format!("RESULT_HEADER_{index}"), format!("{name}: {value}"));
    }
    for (index, (name, value)) in spec.stderr_upload_headers.iter().enumerate() {
        secret_data.insert(format!("STDERR_HEADER_{index}"), format!("{name}: {value}"));
    }
    if let Some(url) = &spec.export_upload_url {
        secret_data.insert("EXPORT_UPLOAD_URL".to_owned(), url.clone());
        secret_data.insert(
            "EXPORT_MAX_BYTES".to_owned(),
            configuration.workspace_bytes.to_string(),
        );
        for (index, (name, value)) in spec.export_upload_headers.iter().enumerate() {
            secret_data.insert(format!("EXPORT_HEADER_{index}"), format!("{name}: {value}"));
        }
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
    for destination in &configuration.allowed_egress {
        let Some((cidr, port)) = parse_egress_destination(destination) else {
            continue;
        };
        egress.push(json!({
            "to": [{"ipBlock": {"cidr": cidr}}],
            "ports": [{"protocol": "TCP", "port": port}],
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

fn containers(
    configuration: &SandboxConfiguration,
    secret_name: &str,
    script: &str,
    environment: &[Value],
    container_security: &Value,
    buildkit_image: Option<&str>,
) -> Vec<Value> {
    let requests = json!({
        "cpu": format!("{}m", configuration.cpu_millicores),
        "memory": configuration.memory_bytes.to_string(),
        "ephemeral-storage": configuration.workspace_bytes.to_string(),
    });
    let mut main_mounts = vec![
        json!({"name": ATTEMPT_VOLUME, "mountPath": ATTEMPT_DIR, "readOnly": false}),
        json!({"name": WORKSPACE_VOLUME, "mountPath": WORKSPACE_DIR, "readOnly": false}),
        json!({"name": MATERIALS_VOLUME, "mountPath": MATERIALS_DIR, "readOnly": true}),
    ];
    if buildkit_image.is_some() {
        main_mounts.push(
            json!({"name": BUILDKIT_RUN_VOLUME, "mountPath": BUILDKIT_RUN_DIR, "readOnly": true}),
        );
    }
    let main = json!({
        "name": SANDBOX_MAIN_CONTAINER,
        "image": configuration.image,
        "imagePullPolicy": "IfNotPresent",
        "command": ["/bin/sh", "-c", script],
        "env": environment,
        "envFrom": [{"secretRef": {"name": secret_name, "optional": false}}],
        "resources": {"requests": requests, "limits": requests},
        "securityContext": container_security,
        "volumeMounts": main_mounts,
    });
    let mut containers = Vec::new();
    if let Some(image) = buildkit_image {
        containers.push(buildkit_sidecar(configuration, image));
    }
    containers.push(main);
    containers
}

fn buildkit_sidecar(configuration: &SandboxConfiguration, image: &str) -> Value {
    // Rootless BuildKit needs the exceptions proven by the platform builder: an
    // unconfined seccomp/AppArmor profile, the setuid helpers for the nested user
    // namespace and an SELinux type that may mount snapshot content. The sidecar is
    // tokenless, bounded by the same resources as the attempt and only reachable
    // through the attempt-local socket directory.
    json!({
        "name": SANDBOX_BUILDKIT_CONTAINER,
        "image": image,
        "imagePullPolicy": "IfNotPresent",
        "args": ["--config", BUILDKIT_CONFIG_PATH, "--oci-worker-no-process-sandbox"],
        "env": [
            {"name": "XDG_RUNTIME_DIR", "value": BUILDKIT_RUNTIME_DIR},
            {"name": "TMPDIR", "value": "/tmp"},
            {"name": "HOME", "value": "/home/user"},
        ],
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
        "securityContext": {
            "allowPrivilegeEscalation": true,
            "capabilities": {"drop": ["ALL"], "add": ["SETUID", "SETGID"]},
            "readOnlyRootFilesystem": true,
            "runAsNonRoot": true,
            "runAsUser": 1000,
            "runAsGroup": 1000,
            "seccompProfile": {"type": "Unconfined"},
            "appArmorProfile": {"type": "Unconfined"},
            "seLinuxOptions": {"type": "spc_t"},
        },
        "volumeMounts": [
            {"name": BUILDKIT_RUN_VOLUME, "mountPath": BUILDKIT_RUN_DIR, "readOnly": false},
            {"name": BUILDKIT_STATE_VOLUME, "mountPath": BUILDKIT_STATE_DIR, "readOnly": false},
            {"name": BUILDKIT_RUNTIME_VOLUME, "mountPath": BUILDKIT_RUNTIME_DIR, "readOnly": false},
            {"name": BUILDKIT_TMP_VOLUME, "mountPath": "/tmp", "readOnly": false},
            {
                "name": BUILDKIT_CONFIG_VOLUME,
                "mountPath": BUILDKIT_CONFIG_PATH,
                "subPath": "buildkitd.toml",
                "readOnly": true,
            },
            {
                "name": BUILDKIT_CONFIG_VOLUME,
                "mountPath": BUILDKIT_CA_PATH,
                "subPath": "registry-ca.crt",
                "readOnly": true,
            },
            {"name": BUILDKIT_AUTH_VOLUME, "mountPath": BUILDKIT_DOCKER_CONFIG_DIR, "readOnly": true},
        ],
    })
}

fn volumes(configuration: &SandboxConfiguration, secret_name: &str, buildkit: bool) -> Value {
    let mut volumes = vec![
        json!({"name": ATTEMPT_VOLUME, "emptyDir": {"sizeLimit": ATTEMPT_VOLUME_BYTES.to_string()}}),
        json!({"name": WORKSPACE_VOLUME, "emptyDir": {"sizeLimit": configuration.workspace_bytes.to_string()}}),
        json!({"name": MATERIALS_VOLUME, "emptyDir": {"sizeLimit": MATERIALS_VOLUME_BYTES.to_string()}}),
    ];
    if buildkit {
        let config_map = configuration
            .buildkit_config_map_name
            .as_deref()
            .unwrap_or_default();
        let pull_secret = configuration
            .image_pull_secret_name
            .as_deref()
            .unwrap_or_default();
        volumes.extend([
            json!({"name": BUILDKIT_RUN_VOLUME, "emptyDir": {"sizeLimit": BUILDKIT_RUN_VOLUME_BYTES.to_string()}}),
            json!({"name": BUILDKIT_STATE_VOLUME, "emptyDir": {"sizeLimit": configuration.workspace_bytes.to_string()}}),
            json!({"name": BUILDKIT_RUNTIME_VOLUME, "emptyDir": {"sizeLimit": BUILDKIT_RUN_VOLUME_BYTES.to_string()}}),
            json!({"name": BUILDKIT_TMP_VOLUME, "emptyDir": {"sizeLimit": BUILDKIT_TMP_VOLUME_BYTES.to_string()}}),
            json!({"name": BUILDKIT_CONFIG_VOLUME, "configMap": {"name": config_map}}),
            json!({
                "name": BUILDKIT_AUTH_VOLUME,
                "projected": {
                    "defaultMode": 0o400,
                    "sources": [{
                        "secret": {
                            "name": pull_secret,
                            "items": [{"key": ".dockerconfigjson", "path": "config.json"}],
                        },
                    }],
                },
            }),
        ]);
    }
    let _ = secret_name;
    Value::Array(volumes)
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
    let buildkit_image = configuration.buildkit_image.as_deref();
    if buildkit_image.is_some() {
        environment
            .push(json!({"name": "BUILDKIT_HOST", "value": format!("unix://{BUILDKIT_SOCKET}")}));
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
    script.push_str(
        "version=$(claude --version)\n\
         case \"$version\" in \"$CLAUDE_CODE_VERSION\"*) ;; *)\n\
         \x20 printf 'LW_AGENT_SANDBOX_VERSION_MISMATCH' > /dev/termination-log\n\
         \x20 exit 75\n\
         ;; esac\n",
    );
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
        "  printf 'LW_AGENT_SANDBOX_RESULT_TOO_LARGE' > /dev/termination-log\n  exit 78\nfi\n",
    );
    script.push_str("if [ \"$size\" -gt 0 ]; then\n");
    script.push_str(
        "  curl --fail --silent --show-error --location --retry 2 --max-time 300 --request PUT --upload-file",
    );
    let _ = write!(script, " {ATTEMPT_DIR}/result.json");
    for index in 0..spec.result_upload_headers.len() {
        let _ = write!(script, " --header \"$RESULT_HEADER_{index}\"");
    }
    script.push_str(" \"$RESULT_UPLOAD_URL\"\nfi\n");
    let _ = writeln!(
        script,
        "stderr_size=$(wc -c < {ATTEMPT_DIR}/stderr.log | tr -d ' ')"
    );
    script.push_str(
        "if [ \"$stderr_size\" -gt \"$STDERR_MAX_BYTES\" ]; then stderr_size=$STDERR_MAX_BYTES; fi\n",
    );
    script.push_str(
        "if [ \"$stderr_size\" -gt 0 ]; then\n  head -c \"$stderr_size\" {ATTEMPT_DIR}/stderr.log > {ATTEMPT_DIR}/stderr.upload\n",
    );
    script.push_str(
        "  curl --fail --silent --show-error --location --retry 2 --max-time 300 --request PUT --upload-file",
    );
    let _ = write!(script, " {ATTEMPT_DIR}/stderr.upload");
    for index in 0..spec.stderr_upload_headers.len() {
        let _ = write!(script, " --header \"$STDERR_HEADER_{index}\"");
    }
    script.push_str(" \"$STDERR_UPLOAD_URL\"\nfi\n");
    let _ = writeln!(
        script,
        "sum=$(sha256sum {ATTEMPT_DIR}/result.json | cut -d' ' -f1)"
    );
    script.push_str("stderr_sum=$(sha256sum /dev/null | cut -d' ' -f1)\n");
    script.push_str(
        "if [ \"$stderr_size\" -gt 0 ]; then stderr_sum=$(sha256sum {ATTEMPT_DIR}/stderr.upload | cut -d' ' -f1); fi\n",
    );
    if spec.export_upload_url.is_some() {
        script.push_str("export_size=0\nexport_sum=$(sha256sum /dev/null | cut -d' ' -f1)\n");
        let _ = writeln!(script, "if [ -f {EXPORT_OUTPUT_PATH} ]; then");
        let _ = writeln!(
            script,
            "  export_size=$(wc -c < {EXPORT_OUTPUT_PATH} | tr -d ' ')"
        );
        script.push_str(
            "  if [ \"$export_size\" -gt \"$EXPORT_MAX_BYTES\" ]; then\n\
             \x20   printf 'LW_AGENT_SANDBOX_EXPORT_TOO_LARGE' > /dev/termination-log\n\
             \x20   exit 78\n\
             fi\n",
        );
        let _ = writeln!(
            script,
            "  export_sum=$(sha256sum {EXPORT_OUTPUT_PATH} | cut -d' ' -f1)"
        );
        script.push_str(
            "  curl --fail --silent --show-error --location --retry 2 --max-time 600 --request PUT --upload-file",
        );
        let _ = write!(script, " {EXPORT_OUTPUT_PATH}");
        for index in 0..spec.export_upload_headers.len() {
            let _ = write!(script, " --header \"$EXPORT_HEADER_{index}\"");
        }
        script.push_str(" \"$EXPORT_UPLOAD_URL\"\nfi\n");
    }
    script.push_str(
        "printf '{\"resultSizeBytes\":%s,\"resultSha256\":\"%s\",\"stderrSizeBytes\":%s,\"stderrSha256\":\"%s\",\"exitCode\":%s,\"claudeVersion\":\"%s\",\"exportSizeBytes\":%s,\"exportSha256\":\"%s\"}' \"$size\" \"$sum\" \"$stderr_size\" \"$stderr_sum\" \"$code\" \"$CLAUDE_CODE_VERSION\" \"$export_size\" \"$export_sum\" > /dev/termination-log\nexit 0\n",
    );

    let container_security = json!({
        "allowPrivilegeEscalation": false,
        "capabilities": {"drop": ["ALL"]},
        "readOnlyRootFilesystem": true,
        "runAsNonRoot": true,
        "runAsUser": SANDBOX_ATTEMPT_USER,
        "runAsGroup": SANDBOX_ATTEMPT_USER,
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
                    // The shared observation verifies the complete attempt ownership on the pod,
                    // so the pod carries the same identity labels as the Job.
                    "labels": ownership_labels(&spec.ownership),
                    // The Job controller never copies Job annotations onto its pods.
                    "annotations": { REQUEST_SHA_ANNOTATION: spec.ownership.request_sha256 },
                },
                "spec": {
                    "automountServiceAccountToken": false,
                    "restartPolicy": "Never",
                    "serviceAccountName": configuration.service_account_name,
                    // The one-shot workload always runs under the shared gVisor RuntimeClass.
                    "runtimeClassName": SANDBOX_RUNTIME_CLASS,
                    "securityContext": {
                        "runAsNonRoot": true,
                        "runAsUser": SANDBOX_ATTEMPT_USER,
                        "runAsGroup": SANDBOX_ATTEMPT_USER,
                        // The rootless sidecar owns the attempt-local socket with its own group,
                        // so the pod shares that group and the unprivileged sandbox can reach
                        // the daemon. Without the sidecar there is nothing to share.
                        "fsGroup": attempt_fs_group(configuration),
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
                        "command": ["/bin/sh", "-c", materialize_script(spec.object_store_ca_base64.is_some())],
                        "env": init_environment(spec),
                        "envFrom": [{"secretRef": {"name": secret_name, "optional": false}}],
                        "securityContext": {
                            "allowPrivilegeEscalation": false,
                            "capabilities": {"drop": ["ALL"]},
                            "readOnlyRootFilesystem": true,
                            "runAsNonRoot": true,
                            "runAsUser": SANDBOX_ATTEMPT_USER,
                            "runAsGroup": SANDBOX_ATTEMPT_USER,
                            "seccompProfile": {"type": "RuntimeDefault"},
                        },
                        "volumeMounts": [
                            {"name": ATTEMPT_VOLUME, "mountPath": ATTEMPT_DIR, "readOnly": false},
                            {"name": MATERIALS_VOLUME, "mountPath": MATERIALS_DIR, "readOnly": false},
                        ],
                    }],
                    "containers": containers(
                        configuration,
                        secret_name,
                        &script,
                        &environment,
                        &container_security,
                        buildkit_image,
                    ),
                    "volumes": volumes(configuration, secret_name, buildkit_image.is_some()),
                },
            },
        },
    })
}

/// Environment the materializer init container needs beyond the attempt Secret.
///
/// The signed material URL is served by the object store, so the initializer reads it with the same
/// reviewed trust root the attempt uses. Without a configured CA the image trust store applies.
fn init_environment(spec: &SandboxAttemptSpec) -> Value {
    if spec.object_store_ca_base64.is_some() {
        json!([{"name": "SSL_CERT_FILE", "value": format!("{ATTEMPT_DIR}/ca.pem")}])
    } else {
        json!([])
    }
}

fn materialize_script(object_store_ca: bool) -> String {
    let mut script = String::new();
    script.push_str("set -eu\numask 077\n");
    if object_store_ca {
        // The materializer is the only reader of the signed object-store URL, so
        // it has to trust the same reviewed CA the attempt is given.
        let _ = writeln!(
            script,
            "printf '%s' \"$OBJECT_STORE_CA_BASE64\" | base64 -d > {ATTEMPT_DIR}/ca.pem"
        );
    }
    script.push_str(
        "curl --fail --silent --show-error --location --retry 3 --max-time 300 \
     -o /run/labweaver/input.json \"$MATERIAL_DOWNLOAD_URL\"\n\
     size=$(wc -c < /run/labweaver/input.json | tr -d ' ')\n\
     if [ \"$size\" != \"$MATERIAL_SIZE_BYTES\" ]; then exit 74; fi\n\
     printf '%s  /run/labweaver/input.json\\n' \"$MATERIAL_SHA256\" | sha256sum --check --strict\n\
     python3 -c \"import json,os\n\
     root='/materials'\n\
     envelope=json.load(open('/run/labweaver/input.json'))\n\
     for entry in envelope.get('files',[]):\n\
     \x20   content=entry.get('content','')\n\
     \x20   if not content: continue\n\
     \x20   path=os.path.normpath(os.path.join(root, entry['path']))\n\
     \x20   if not path.startswith(root + os.sep): continue\n\
     \x20   os.makedirs(os.path.dirname(path), exist_ok=True)\n\
     \x20   open(path,'w',encoding='utf-8').write(content)\n\"\n",
    );
    script
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

/// Returns the group the attempt pod shares with its `BuildKit` sidecar, when present.
///
/// The rootless sidecar creates the attempt-local socket with its own group and the sandbox
/// container runs as a different unprivileged user, so the pod must join that group for the
/// daemon to be reachable at all.
fn attempt_fs_group(configuration: &SandboxConfiguration) -> u64 {
    if configuration.buildkit_image.is_some() {
        BUILDKIT_SIDECAR_GROUP
    } else {
        SANDBOX_ATTEMPT_USER
    }
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

fn valid_dns_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !value.starts_with('-')
        && !value.ends_with('-')
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
        MAX_COMMAND_ARGUMENT_BYTES, MAX_COMMAND_BYTES, MAX_MATERIAL_BYTES,
        SANDBOX_BUILDKIT_CONTAINER, SANDBOX_MAIN_CONTAINER, SANDBOX_RUNTIME_CLASS,
        SandboxAttemptSpec, SandboxBundle, SandboxBundleError, SandboxConfiguration,
        build_sandbox_bundle, shell_quote,
    };

    fn job_of(bundle: &SandboxBundle) -> Result<&serde_json::Value, Box<dyn std::error::Error>> {
        Ok(&bundle
            .bundle
            .objects
            .iter()
            .find(|object| object.plural == "jobs")
            .ok_or(SandboxBundleError::Invalid)?
            .document)
    }

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
            allowed_egress: BTreeSet::from(["10.0.0.0/8:443".to_owned()]),
            buildkit_image: None,
            buildkit_config_map_name: None,
        }
    }

    fn buildkit_configuration() -> SandboxConfiguration {
        let mut configuration = configuration();
        configuration.buildkit_image = Some(
            "harbor.lab.lan/labweaver-system/buildkit@sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned(),
        );
        configuration.buildkit_config_map_name = Some("authoring-buildkit-config".to_owned());
        configuration
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
            expected_claude_version: "2.1.215".to_owned(),
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
            stderr_upload_url: "https://objects.example/stderr?sig=3".to_owned(),
            stderr_upload_headers: BTreeMap::from([("if-none-match".to_owned(), "*".to_owned())]),
            result_max_bytes: 4 * 1024 * 1024,
            stderr_max_bytes: 1024 * 1024,
            export_upload_url: None,
            export_upload_headers: BTreeMap::new(),
            object_store_ca_base64: Some("Q0E=".to_owned()),
        }
    }

    #[test]
    fn buildkit_sidecar_uses_rootless_exceptions_and_attempt_local_socket()
    -> Result<(), Box<dyn std::error::Error>> {
        let bundle = build_sandbox_bundle(&buildkit_configuration(), &spec())?;
        let job = bundle
            .bundle
            .objects
            .iter()
            .find(|object| object.plural == "jobs")
            .ok_or(SandboxBundleError::Invalid)?;
        let containers = job.document["spec"]["template"]["spec"]["containers"]
            .as_array()
            .ok_or(SandboxBundleError::Invalid)?;
        assert_eq!(containers.len(), 2);
        let sidecar = containers
            .iter()
            .find(|container| container["name"] == "buildkit")
            .ok_or(SandboxBundleError::Invalid)?;
        assert_eq!(
            sidecar["securityContext"]["seccompProfile"]["type"],
            "Unconfined"
        );
        assert_eq!(
            sidecar["securityContext"]["appArmorProfile"]["type"],
            "Unconfined"
        );
        assert_eq!(sidecar["securityContext"]["runAsUser"], 1000);
        assert_eq!(
            sidecar["securityContext"]["capabilities"]["add"],
            serde_json::json!(["SETUID", "SETGID"])
        );
        let main = containers
            .iter()
            .find(|container| container["name"] == "claude-code")
            .ok_or(SandboxBundleError::Invalid)?;
        assert!(main["env"].as_array().is_some_and(|env| {
            env.iter().any(|entry| {
                entry["name"] == "BUILDKIT_HOST"
                    && entry["value"] == "unix:///run/buildkit/buildkitd.sock"
            })
        }));
        assert!(main["volumeMounts"].as_array().is_some_and(|mounts| {
            mounts
                .iter()
                .any(|mount| mount["name"] == "buildkit-run" && mount["readOnly"] == true)
        }));
        let volumes = job.document["spec"]["template"]["spec"]["volumes"]
            .as_array()
            .ok_or(SandboxBundleError::Invalid)?;
        assert!(volumes.iter().any(|volume| {
            volume["name"] == "buildkit-config"
                && volume["configMap"]["name"] == "authoring-buildkit-config"
        }));
        assert!(volumes.iter().any(|volume| {
            volume["name"] == "buildkit-auth"
                && volume["projected"]["sources"][0]["secret"]["name"] == "harbor-course-pull"
        }));
        assert!(sidecar["volumeMounts"].as_array().is_some_and(|mounts| {
            mounts.iter().any(|mount| {
                mount["mountPath"] == "/etc/buildkit/registry-ca.crt"
                    && mount["subPath"] == "registry-ca.crt"
            })
        }));
        Ok(())
    }

    #[test]
    fn buildkit_export_channel_is_rendered_only_with_the_sidecar()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut spec = spec();
        spec.export_upload_url = Some("https://objects.example.invalid/export".to_owned());
        spec.export_upload_headers =
            BTreeMap::from([("x-upload-token".to_owned(), "t".to_owned())]);
        assert!(matches!(
            build_sandbox_bundle(&configuration(), &spec),
            Err(SandboxBundleError::Invalid)
        ));
        let bundle = build_sandbox_bundle(&buildkit_configuration(), &spec)?;
        let job = bundle
            .bundle
            .objects
            .iter()
            .find(|object| object.plural == "jobs")
            .ok_or(SandboxBundleError::Invalid)?;
        let script = job.document["spec"]["template"]["spec"]["containers"]
            .as_array()
            .and_then(|containers| containers.iter().find(|item| item["name"] == "claude-code"))
            .and_then(|container| container["command"][2].as_str())
            .ok_or(SandboxBundleError::Invalid)?;
        assert!(script.contains("/workspace/labweaver-export.tar"));
        assert!(script.contains("EXPORT_UPLOAD_URL"));
        assert!(script.contains("exportSizeBytes"));
        let secret = bundle
            .bundle
            .objects
            .iter()
            .find(|object| object.plural == "secrets")
            .ok_or(SandboxBundleError::Invalid)?;
        assert_eq!(
            secret.document["stringData"]["EXPORT_UPLOAD_URL"],
            "https://objects.example.invalid/export"
        );
        assert_eq!(
            secret.document["stringData"]["EXPORT_MAX_BYTES"],
            "2147483648"
        );
        Ok(())
    }

    #[test]
    fn partial_buildkit_configuration_is_rejected() {
        let mut configuration = configuration();
        configuration.buildkit_image = Some(
            "harbor.lab.lan/labweaver-system/buildkit@sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc".to_owned(),
        );
        assert!(matches!(
            build_sandbox_bundle(&configuration, &spec()),
            Err(SandboxBundleError::Invalid)
        ));
        let mut configuration = buildkit_configuration();
        configuration.image_pull_secret_name = None;
        assert!(matches!(
            build_sandbox_bundle(&configuration, &spec()),
            Err(SandboxBundleError::Invalid)
        ));
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
            job.document.pointer("/spec/template/spec/runtimeClassName"),
            Some(&serde_json::Value::String(SANDBOX_RUNTIME_CLASS.to_owned()))
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
        assert!(container["volumeMounts"].as_array().is_some_and(|mounts| {
            mounts
                .iter()
                .any(|mount| mount["name"] == "materials" && mount["readOnly"] == true)
        }));
        assert!(
            job.document["spec"]["template"]["spec"]["volumes"]
                .as_array()
                .is_some_and(|volumes| volumes.iter().any(|volume| volume["name"] == "materials"))
        );
        let init = &job.document["spec"]["template"]["spec"]["initContainers"][0];
        assert_eq!(init["name"], "materialize");
        assert!(init["command"][2].as_str().is_some_and(|script| {
            script.contains("sha256sum --check --strict")
                && script.contains("MATERIAL_SIZE_BYTES")
                && script.contains("/materials")
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
        // The shared observation verifies the complete attempt ownership on the pod itself, and
        // the Job controller does not copy Job annotations onto its pods.
        for label in [
            "labweaver.io~1managed-by",
            "labweaver.io~1run-id",
            "labweaver.io~1step-run-id",
            "labweaver.io~1attempt-id",
        ] {
            assert_eq!(
                job.document
                    .pointer(&format!("/spec/template/metadata/labels/{label}")),
                job.document.pointer(&format!("/metadata/labels/{label}")),
                "{label} must match on the pod template"
            );
        }
        assert_eq!(
            job.document
                .pointer("/spec/template/metadata/annotations/labweaver.io~1request-sha256"),
            job.document
                .pointer("/metadata/annotations/labweaver.io~1request-sha256")
        );
        // Without a BuildKit sidecar there is no shared socket to reach.
        assert_eq!(
            job.document
                .pointer("/spec/template/spec/securityContext/fsGroup"),
            Some(&serde_json::json!(65_532))
        );
        Ok(())
    }

    #[test]
    fn materializer_trusts_the_reviewed_object_store_ca() -> Result<(), Box<dyn std::error::Error>>
    {
        // The initializer is the only reader of the signed material URL, so it must trust the same
        // reviewed CA the attempt receives and decode it before it curls the object store.
        let bundle = build_sandbox_bundle(&configuration(), &spec())?;
        let job = bundle
            .bundle
            .objects
            .iter()
            .find(|object| object.plural == "jobs")
            .ok_or(SandboxBundleError::Invalid)?;
        let init = &job.document["spec"]["template"]["spec"]["initContainers"][0];
        assert_eq!(
            init["env"],
            serde_json::json!([{"name": "SSL_CERT_FILE", "value": "/run/labweaver/ca.pem"}])
        );
        let script = init["command"][2]
            .as_str()
            .ok_or(SandboxBundleError::Invalid)?;
        assert!(script.contains("OBJECT_STORE_CA_BASE64"));
        assert!(script.contains("base64 -d > /run/labweaver/ca.pem"));
        assert!(script.starts_with("set -eu"));

        // Without a reviewed CA the initializer relies on the image trust store and is handed no
        // path it cannot read.
        let mut spec = spec();
        spec.object_store_ca_base64 = None;
        let bundle = build_sandbox_bundle(&configuration(), &spec)?;
        let job = bundle
            .bundle
            .objects
            .iter()
            .find(|object| object.plural == "jobs")
            .ok_or(SandboxBundleError::Invalid)?;
        let init = &job.document["spec"]["template"]["spec"]["initContainers"][0];
        assert_eq!(init["env"], serde_json::json!([]));
        assert!(
            !init["command"][2]
                .as_str()
                .ok_or(SandboxBundleError::Invalid)?
                .contains("ca.pem")
        );
        Ok(())
    }

    #[test]
    fn attempt_pod_shares_the_buildkit_sidecar_group() -> Result<(), Box<dyn std::error::Error>> {
        let bundle = build_sandbox_bundle(&buildkit_configuration(), &spec())?;
        let job = bundle
            .bundle
            .objects
            .iter()
            .find(|object| object.plural == "jobs")
            .ok_or(SandboxBundleError::Invalid)?;
        let sidecar = job.document["spec"]["template"]["spec"]["containers"]
            .as_array()
            .ok_or(SandboxBundleError::Invalid)?
            .iter()
            .find(|container| container["name"] == SANDBOX_BUILDKIT_CONTAINER)
            .ok_or(SandboxBundleError::Invalid)?;
        assert_eq!(sidecar["securityContext"]["runAsUser"], 1_000);
        // The sidecar owns the attempt-local socket with its own group, so the pod must join
        // that group or the unprivileged sandbox cannot reach the daemon.
        assert_eq!(
            job.document
                .pointer("/spec/template/spec/securityContext/fsGroup"),
            Some(&sidecar["securityContext"]["runAsGroup"])
        );
        assert_eq!(
            job.document
                .pointer("/spec/template/spec/securityContext/runAsUser"),
            Some(&serde_json::json!(65_532))
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
        assert!(script.contains("claude --version"));
        assert!(script.contains("claudeVersion"));
        assert!(script.contains("STDERR_HEADER_0"));
        assert!(script.contains("stderr.upload"));
        assert!(script.contains("LW_AGENT_SANDBOX_RESULT_TOO_LARGE"));
        assert!(script.contains("LW_AGENT_SANDBOX_VERSION_MISMATCH"));
        assert!(script.contains("exit 0"));
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
        // Egress is exactly the cluster DNS rule followed by the reviewed CIDR rule.
        assert_eq!(
            policy.document["spec"]["egress"].as_array().map(Vec::len),
            Some(2)
        );
        assert_eq!(
            policy.document["spec"]["egress"][0]["to"],
            serde_json::json!([{
                "namespaceSelector": {
                    "matchLabels": {"kubernetes.io/metadata.name": "kube-system"}
                }
            }])
        );
        assert_eq!(
            policy.document["spec"]["egress"][0]["ports"],
            serde_json::json!([
                {"protocol": "UDP", "port": 53},
                {"protocol": "TCP", "port": 53},
            ])
        );
        assert_eq!(
            policy.document["spec"]["egress"][1]["to"][0]["ipBlock"]["cidr"],
            "10.0.0.0/8"
        );
        assert_eq!(
            policy.document["spec"]["egress"][1]["ports"],
            serde_json::json!([{"protocol": "TCP", "port": 443}])
        );
        Ok(())
    }

    #[test]
    fn reviewed_egress_keeps_the_reviewed_port_per_destination()
    -> Result<(), Box<dyn std::error::Error>> {
        // A deployment may run its object store and model endpoint off 443, so every reviewed
        // destination carries its own port and no rule admits a port the deployment did not review.
        let mut configuration = configuration();
        configuration.allowed_egress = BTreeSet::from([
            "10.201.0.0/16:9000".to_owned(),
            "172.18.0.0/16:11434".to_owned(),
        ]);
        let bundle = build_sandbox_bundle(&configuration, &spec())?;
        let policy = bundle
            .bundle
            .objects
            .iter()
            .find(|object| object.plural == "networkpolicies")
            .ok_or(SandboxBundleError::Invalid)?;
        assert_eq!(
            policy.document["spec"]["egress"]
                .as_array()
                .map(|egress| egress[1..].to_vec()),
            Some(vec![
                serde_json::json!({
                    "to": [{"ipBlock": {"cidr": "10.201.0.0/16"}}],
                    "ports": [{"protocol": "TCP", "port": 9000}],
                }),
                serde_json::json!({
                    "to": [{"ipBlock": {"cidr": "172.18.0.0/16"}}],
                    "ports": [{"protocol": "TCP", "port": 11434}],
                }),
            ])
        );
        Ok(())
    }

    #[test]
    fn malformed_reviewed_egress_destinations_fail_closed() {
        for destination in [
            "10.0.0.0/8",
            "10.0.0.0/8:0",
            "10.0.0.0/33:443",
            "10.0.0.0/8:70000",
            "10.0.0.0/8:https",
        ] {
            let mut configuration = configuration();
            configuration.allowed_egress = BTreeSet::from([destination.to_owned()]);
            assert_eq!(
                build_sandbox_bundle(&configuration, &spec()).err(),
                Some(SandboxBundleError::Invalid),
                "{destination}"
            );
        }
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
        invalid.allowed_egress.clear();
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
    fn generated_instruction_prompts_fit_the_command_bounds() {
        // The authored instruction prompt travels as an argv entry, so the bound must accept the
        // generated prompts while still rejecting argv that would bloat the Job manifest.
        let mut realistic = spec();
        realistic.command = vec![
            "claude".to_owned(),
            "--system-prompt".to_owned(),
            "S".repeat(4_477),
            "P".repeat(12_288),
        ];
        assert!(build_sandbox_bundle(&configuration(), &realistic).is_ok());

        let mut oversized_argument = spec();
        oversized_argument.command = vec!["P".repeat(MAX_COMMAND_ARGUMENT_BYTES + 1)];
        assert_eq!(
            build_sandbox_bundle(&configuration(), &oversized_argument).err(),
            Some(SandboxBundleError::Invalid)
        );

        let mut oversized_command = spec();
        oversized_command.command = (0..8)
            .map(|_| "P".repeat(MAX_COMMAND_ARGUMENT_BYTES))
            .collect();
        assert!(
            oversized_command
                .command
                .iter()
                .map(String::len)
                .sum::<usize>()
                > MAX_COMMAND_BYTES
        );
        assert_eq!(
            build_sandbox_bundle(&configuration(), &oversized_command).err(),
            Some(SandboxBundleError::Invalid)
        );

        let mut oversized_material = spec();
        oversized_material.material_size_bytes = MAX_MATERIAL_BYTES + 1;
        assert_eq!(
            build_sandbox_bundle(&configuration(), &oversized_material).err(),
            Some(SandboxBundleError::Invalid)
        );
    }

    #[test]
    fn shell_quoting_never_allows_command_injection() {
        assert_eq!(shell_quote("plain"), "'plain'");
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
        assert_eq!(shell_quote("; rm -rf /"), "'; rm -rf /'");
    }

    #[test]
    fn attempt_job_renders_the_configured_resource_bounds() -> Result<(), Box<dyn std::error::Error>>
    {
        for (configuration, expected_empty_dirs) in [
            (
                configuration(),
                vec![
                    ("attempt", "67108864"),
                    ("workspace", "2147483648"),
                    ("materials", "33554432"),
                ],
            ),
            (
                buildkit_configuration(),
                vec![
                    ("attempt", "67108864"),
                    ("workspace", "2147483648"),
                    ("materials", "33554432"),
                    ("buildkit-run", "67108864"),
                    ("buildkit-state", "2147483648"),
                    ("buildkit-runtime", "67108864"),
                    ("buildkit-tmp", "268435456"),
                ],
            ),
        ] {
            let bundle = build_sandbox_bundle(&configuration, &spec())?;
            let job = job_of(&bundle)?;
            let pod = &job["spec"]["template"]["spec"];
            let main = pod["containers"]
                .as_array()
                .ok_or(SandboxBundleError::Invalid)?
                .iter()
                .find(|container| container["name"] == SANDBOX_MAIN_CONTAINER)
                .ok_or(SandboxBundleError::Invalid)?;
            // The main container is bounded by exactly the reviewed configuration, on both the
            // request and the limit side.
            let expected = serde_json::json!({
                "cpu": format!("{}m", configuration.cpu_millicores),
                "memory": configuration.memory_bytes.to_string(),
                "ephemeral-storage": configuration.workspace_bytes.to_string(),
            });
            assert_eq!(main["resources"]["requests"], expected);
            assert_eq!(main["resources"]["limits"], expected);

            let mut empty_dirs = pod["volumes"]
                .as_array()
                .ok_or(SandboxBundleError::Invalid)?
                .iter()
                .filter_map(|volume| {
                    let limit = volume["emptyDir"]["sizeLimit"].as_str()?;
                    Some((
                        volume["name"].as_str().unwrap_or_default().to_owned(),
                        limit.to_owned(),
                    ))
                })
                .collect::<Vec<_>>();
            for (_, limit) in &empty_dirs {
                assert!(
                    limit.parse::<u64>().is_ok_and(|bytes| bytes > 0),
                    "every emptyDir must carry a finite sizeLimit, found {limit}"
                );
            }
            empty_dirs.sort();
            let mut expected_empty_dirs = expected_empty_dirs
                .iter()
                .map(|(name, limit)| ((*name).to_owned(), (*limit).to_owned()))
                .collect::<Vec<_>>();
            expected_empty_dirs.sort();
            assert_eq!(empty_dirs, expected_empty_dirs);
        }
        Ok(())
    }

    #[test]
    fn attempt_secret_carries_only_attempt_inputs_and_no_registry_credential()
    -> Result<(), Box<dyn std::error::Error>> {
        let attempt = spec();
        let bundle = build_sandbox_bundle(&configuration(), &attempt)?;
        let secret = bundle
            .bundle
            .objects
            .iter()
            .find(|object| object.plural == "secrets")
            .ok_or(SandboxBundleError::Invalid)?;
        // The attempt Secret holds exactly the input keys the bundle itself builds for this
        // attempt, and nothing else.
        let mut expected = BTreeSet::from([
            "MATERIAL_DOWNLOAD_URL".to_owned(),
            "MATERIAL_SHA256".to_owned(),
            "MATERIAL_SIZE_BYTES".to_owned(),
            "RESULT_UPLOAD_URL".to_owned(),
            "RESULT_MAX_BYTES".to_owned(),
            "STDERR_UPLOAD_URL".to_owned(),
            "STDERR_MAX_BYTES".to_owned(),
            "CLAUDE_CODE_VERSION".to_owned(),
        ]);
        expected.extend(attempt.command_environment.keys().cloned());
        for index in 0..attempt.result_upload_headers.len() {
            expected.insert(format!("RESULT_HEADER_{index}"));
        }
        for index in 0..attempt.stderr_upload_headers.len() {
            expected.insert(format!("STDERR_HEADER_{index}"));
        }
        if attempt.object_store_ca_base64.is_some() {
            expected.insert("OBJECT_STORE_CA_BASE64".to_owned());
        }
        let rendered = secret.document["stringData"]
            .as_object()
            .ok_or(SandboxBundleError::Invalid)?;
        let actual = rendered.keys().cloned().collect::<BTreeSet<_>>();
        assert_eq!(actual, expected);
        for forbidden in [
            "username",
            "password",
            "auth",
            "dockercfg",
            ".dockerconfigjson",
            "token",
            "registry",
        ] {
            assert!(
                !actual.contains(forbidden),
                "{forbidden} must never be an attempt Secret key"
            );
        }
        // The bundle renders no second, base64 credential blob beside the input payload.
        assert!(secret.document.get("data").is_none());

        let job = job_of(&bundle)?;
        let pod = &job["spec"]["template"]["spec"];
        let volumes = pod["volumes"]
            .as_array()
            .ok_or(SandboxBundleError::Invalid)?;
        assert_eq!(
            volumes
                .iter()
                .map(|volume| volume["name"].as_str().unwrap_or_default())
                .collect::<Vec<_>>(),
            ["attempt", "workspace", "materials"]
        );
        for volume in volumes {
            // Every attempt volume is a plain emptyDir: none of them mounts a docker config or
            // any other credential-bearing source.
            assert!(volume.get("emptyDir").is_some(), "{volume}");
            assert!(volume.get("projected").is_none(), "{volume}");
            assert!(volume.get("secret").is_none(), "{volume}");
            assert!(volume.get("configMap").is_none(), "{volume}");
        }
        // The only pull-credential reference in the attempt is the configured pull secret.
        assert_eq!(
            pod["imagePullSecrets"],
            serde_json::json!([{"name": "harbor-course-pull"}])
        );
        Ok(())
    }
}
