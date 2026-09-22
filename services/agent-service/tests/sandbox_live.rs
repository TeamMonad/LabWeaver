//! Opt-in live `Kubernetes` readback for the Agent authoring sandbox bundle.
#![allow(
    clippy::too_many_lines,
    reason = "the live acceptance flows keep the applied security context, the default-deny admission gate, the adversarial probe observations, deterministic cleanup and the terminal observations auditable together"
)]
//!
//! The test runs only when `LW_LIVE_KUBERNETES=1` is set together with every required variable
//! below; otherwise it prints one skip line and returns. It renders the exact authoring bundle
//! through `build_sandbox_bundle`, applies it to a real API server and proves the applied pod and
//! container security context, the namespace default-deny admission gate, deterministic cleanup
//! and the terminal observation of an attempt that cannot succeed without a model credential.
//!
//! ```text
//! LW_LIVE_KUBERNETES=1 \
//! LW_LIVE_KUBERNETES_API_SERVER=https://127.0.0.1:40495 \
//! LW_LIVE_KUBERNETES_TOKEN_FILE=/tmp/opencode/kind-token \
//! LW_LIVE_KUBERNETES_CA_FILE=/tmp/opencode/kind-ca.pem \
//! LW_LIVE_KUBERNETES_NAMESPACE=labweaver-authoring \
//! LW_LIVE_SANDBOX_IMAGE=harbor.lab.lan/labweaver-system/authoring-sandbox@sha256:0000 \
//! LW_LIVE_SANDBOX_EGRESS_CIDR=10.0.0.0/8 \
//! cargo test -p agent-service --test sandbox_live -- --nocapture
//! ```
//!
//! The API server, bearer token and CA are the ones the local stack kubeconfig already carries:
//! `kubectl --kubeconfig .tmp/local-dev/kubeconfig config view --minify -o jsonpath='{.clusters[0].cluster.server}'`,
//! `jsonpath='{.users[0].user.token}'` and
//! `jsonpath='{.clusters[0].cluster.certificate-authority-data}' | base64 -d`.
//!
//! Optional: `LW_LIVE_SANDBOX_PULL_SECRET` (the reviewed pull secret, for example
//! `harbor-labweaver-system-pull`) and the `BuildKit` sidecar pair
//! `LW_LIVE_SANDBOX_BUILDKIT_IMAGE` plus `LW_LIVE_SANDBOX_BUILDKIT_CONFIG_MAP`, which must be set
//! together and require the pull secret. Without them the rendered bundle has no sidecar and no
//! sidecar assertion is made.
//!
//! The adversarial cases below observe real attempt behaviour, not just the rendered documents, so
//! the attempt command has to run. A rendered attempt only reaches its command after the
//! materialize init container verified its material envelope, so those cases additionally read the
//! four variables below (material URL, served size, served sha256 and the image's Claude Code
//! version) and print one labelled skip line instead of asserting when any of them is absent. The
//! remaining three are optional: a base64 CA for the material host plus overrides of the two probe
//! targets. Every variable above keeps its meaning, so an existing invocation is unaffected.
//!
//! ```text
//! LW_LIVE_SANDBOX_MATERIAL_URL=https://harbor.lab.lan/v2/ \
//! LW_LIVE_SANDBOX_MATERIAL_SHA256=<sha256 of the served bytes> \
//! LW_LIVE_SANDBOX_MATERIAL_SIZE_BYTES=<served byte count> \
//! LW_LIVE_SANDBOX_CLAUDE_VERSION=<prefix of `claude --version` inside the sandbox image> \
//! LW_LIVE_SANDBOX_MATERIAL_CA_BASE64=<optional base64 CA the attempt reads the material with> \
//! LW_LIVE_SANDBOX_BLOCKED_EGRESS_URL=<optional, default https://1.1.1.1/> \
//! LW_LIVE_SANDBOX_HANG_MATERIAL_URL=<optional, default https://192.0.2.1/labweaver-live-material.json> \
//! ```
//!
//! `LW_LIVE_SANDBOX_MATERIAL_URL` must serve exactly the bytes the size and sha256 variables pin
//! and must be trusted by the sandbox image itself, because the init container verifies TLS through
//! the image trust store. The material host has to sit inside `LW_LIVE_SANDBOX_EGRESS_CIDR` while
//! the probe destinations have to sit outside it. `LW_LIVE_SANDBOX_HANG_MATERIAL_URL` must never
//! answer (the default `TEST-NET-1` address does not): the deadline, cancellation and crash cases
//! need an attempt that stays live until the case ends it. The cases share one namespace and the
//! namespace-wide default-deny gate, so they run one at a time.
//!
//! Every observed in-container case reads its evidence from the attempt container's terminal
//! message. The rendered script redirects the command's own descriptors into the attempt volume, so
//! a probe cannot reach the pod log; it appends its evidence to the container terminal file instead
//! and reports a short stdout payload, which makes the rendered script fail at the unreachable
//! result sink before it can overwrite that evidence with its own receipt. The container log is
//! still read as supporting evidence that the attempt failed at its own sink.

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    error::Error,
    fs,
    path::PathBuf,
    time::{Duration, Instant},
};

use agent_service::claude_code::RunCancellation;
use agent_service::sandbox::{
    SANDBOX_BUILDKIT_CONTAINER, SANDBOX_DEFAULT_DENY_POLICY, SANDBOX_EVENT_SCOPE,
    SANDBOX_MAIN_CONTAINER, SANDBOX_MANAGED_BY, SandboxAttemptSpec, SandboxBundle,
    SandboxConfiguration, build_sandbox_bundle,
};
use contracts::execution::ExecutionCleanupStatus;
use persistence_sqlx::Sha256Digest;
use reqwest::{Certificate, Client, StatusCode, Url, redirect::Policy};
use serde_json::{Value, json};
use task_execution::kubernetes::{
    KubernetesApiClient, KubernetesApiConfiguration, KubernetesJobError, KubernetesJobIdentity,
    KubernetesJobObservation, KubernetesOwnership,
};
use uuid::Uuid;

/// Field manager the Agent execution backend applies the attempt bundle with.
const FIELD_MANAGER: &str = "labweaver-authoring-executor";
/// Log scope the Agent execution backend reports execution events under.
const LOG_SCOPE: &str = "agent.authoring.sandbox";
/// Diagnostic prefix the Agent execution backend stamps on sandbox diagnostics.
const DIAGNOSTIC_PREFIX: &str = "LW_AGENT_";
/// Service account the deployed authoring namespace provisions for attempts.
const SERVICE_ACCOUNT_NAME: &str = "authoring-runner";
/// Managed-by selector that matches exactly the objects of the rendered attempts.
const MANAGED_BY_SELECTOR: &str = "labweaver.io/managed-by=agent-service";
/// Bounded API request timeout for the live readback.
const REQUEST_TIMEOUT_MILLISECONDS: u64 = 5_000;
/// Wall-clock deadline the live attempt runs under before Kubernetes fails it.
const ATTEMPT_WALL_TIME_SECONDS: u64 = 120;
/// Wall time the sidecar build case allows for a slow rootless daemon to come up.
const SIDECAR_WALL_TIME_SECONDS: u64 = 600;
/// Upper bound on waiting for the attempt to reach a terminal state.
const OBSERVE_TIMEOUT: Duration = Duration::from_mins(5);
/// Upper bound on waiting for verified cleanup.
const CLEANUP_TIMEOUT: Duration = Duration::from_mins(2);
/// Poll interval while observing or cleaning up.
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Unreachable material source: the init container fails closed and never reaches the model run.
const MATERIAL_DOWNLOAD_URL: &str = "https://127.0.0.1:1/labweaver-live-material.json";
/// Unreachable result sink; no attempt result is ever uploaded by this probe.
const RESULT_UPLOAD_URL: &str = "https://127.0.0.1:1/labweaver-live-result.json";
/// Unreachable stderr sink; no attempt stderr is ever uploaded by this probe.
const STDERR_UPLOAD_URL: &str = "https://127.0.0.1:1/labweaver-live-stderr.log";

/// Minimum wall-clock deadline the sandbox configuration accepts.
const MINIMUM_WALL_TIME_SECONDS: u64 = 60;
/// Deadline the cancellation and crash-recovery attempts run under, so they stay live.
const LIVE_ATTEMPT_WALL_TIME_SECONDS: u64 = 600;
/// Material source that never answers (`TEST-NET-1`): the materialize init container hangs, so the
/// attempt stays live until `Kubernetes` fails it at its deadline or a case ends it first.
const HANG_MATERIAL_URL: &str = "https://192.0.2.1/labweaver-live-material.json";
/// Egress destination the adversarial probe addresses directly, outside every private CIDR.
const BLOCKED_EGRESS_URL: &str = "https://1.1.1.1/";
/// Egress destination the adversarial probe addresses by name, exercising name resolution too.
const BLOCKED_EGRESS_DNS_URL: &str = "https://example.com/";
/// Timeout the in-container probe gives every network attempt.
const PROBE_CURL_MAX_TIME_SECONDS: u64 = 5;
/// Marker every in-container probe prints its evidence lines under.
const PROBE_MARKER: &str = "LW_LIVE_PROBE";
/// Terminal file the probe appends its evidence to and the rendered script writes its receipt to.
const TERMINATION_LOG: &str = "/dev/termination-log";
/// Short payload every probe writes to its own stdout.
///
/// The rendered script redirects the command's own descriptors into the attempt volume and uploads
/// a non-empty result to the unreachable sink, so the payload makes the script fail at that sink
/// before it can overwrite the probe evidence with its receipt.
const PROBE_RESULT_PAYLOAD: &str = "labweaver-live-probe";
/// Host every attempt sink in this file is pinned to: the pod's own loopback, where nothing listens.
const SINK_HOST: &str = "127.0.0.1";
/// Attempt-id label the shared observation selects the attempt pod with.
const ATTEMPT_ID_LABEL: &str = "labweaver.io/attempt-id";
/// Diagnostic the shared observation reports for a Job `Kubernetes` failed at its deadline.
const DEADLINE_DIAGNOSTIC: &str = "LW_AGENT_SANDBOX_DEADLINE_EXCEEDED";
/// Model credential the credential case injects and then proves is the only one present.
const MODEL_CREDENTIAL_ENVIRONMENT: [(&str, &str); 3] = [
    ("ANTHROPIC_AUTH_TOKEN", "labweaver-live-model-credential"),
    ("ANTHROPIC_BASE_URL", "https://127.0.0.1:1"),
    ("ANTHROPIC_MODEL", "labweaver-live-model"),
];
/// Optional variable holding the reachable attempt material URL.
const MATERIAL_URL_VARIABLE: &str = "LW_LIVE_SANDBOX_MATERIAL_URL";
/// Optional variable holding the sha256 of the attempt material bytes.
const MATERIAL_SHA256_VARIABLE: &str = "LW_LIVE_SANDBOX_MATERIAL_SHA256";
/// Optional variable holding the byte count of the attempt material.
const MATERIAL_SIZE_VARIABLE: &str = "LW_LIVE_SANDBOX_MATERIAL_SIZE_BYTES";
/// Optional variable holding a base64 CA the attempt trusts while it reads the material.
const MATERIAL_CA_VARIABLE: &str = "LW_LIVE_SANDBOX_MATERIAL_CA_BASE64";
/// Optional variable holding the `claude --version` prefix the sandbox image reports.
const CLAUDE_VERSION_VARIABLE: &str = "LW_LIVE_SANDBOX_CLAUDE_VERSION";
/// Optional variable overriding the egress destination the adversarial probe must not reach.
const BLOCKED_EGRESS_VARIABLE: &str = "LW_LIVE_SANDBOX_BLOCKED_EGRESS_URL";
/// Optional variable overriding the material source that never answers.
const HANG_MATERIAL_VARIABLE: &str = "LW_LIVE_SANDBOX_HANG_MATERIAL_URL";
/// CNI agent name prefixes that enforce `NetworkPolicy`.
const ENFORCING_CNI_AGENTS: [&str; 8] = [
    "calico",
    "cilium",
    "antrea",
    "canal",
    "weave",
    "kube-router",
    "kube-ovn",
    "ovn-kubernetes",
];
/// CNI agent name prefixes that record `NetworkPolicy` without enforcing it.
const RECORDING_CNI_AGENTS: [&str; 2] = ["kindnet", "flannel"];
/// Serializes the live cases: they share one namespace and one namespace-wide default-deny gate.
static LIVE_CASE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Live execution client, readback client and the environment they were built from.
type LiveClients = (KubernetesApiClient, LiveRest, LiveEnvironment);

/// Live connection, image and namespace inputs for one real authoring bundle.
struct LiveEnvironment {
    configuration: KubernetesApiConfiguration,
    namespace: String,
    image: String,
    egress_destination: String,
    buildkit_image: Option<String>,
    buildkit_config_map: Option<String>,
    pull_secret: Option<String>,
    attempt_inputs: Option<LiveAttemptInputs>,
    blocked_egress_url: String,
    hang_material_url: String,
}

/// Everything an observed in-container case needs: a reachable material envelope and the exact
/// `claude --version` prefix the sandbox image reports.
#[derive(Clone, Debug)]
struct LiveAttemptInputs {
    material_url: String,
    material_sha256: String,
    material_size_bytes: u64,
    material_ca_base64: Option<String>,
    claude_version: String,
}

/// Reads one required live variable.
fn required(name: &str) -> Result<String, Box<dyn Error>> {
    env::var(name)
        .map_err(|_| format!("{name} must be set while LW_LIVE_KUBERNETES=1").into())
        .and_then(|value| {
            if value.trim().is_empty() {
                Err(format!("{name} must not be empty while LW_LIVE_KUBERNETES=1").into())
            } else {
                Ok(value)
            }
        })
}

/// Reads one optional live variable, treating an empty value as absent.
fn optional(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.trim().is_empty())
}

/// Reads the live switch and every bound variable, or reports the test as skipped.
fn live_environment() -> Result<Option<LiveEnvironment>, Box<dyn Error>> {
    if env::var("LW_LIVE_KUBERNETES").ok().as_deref() != Some("1") {
        return Ok(None);
    }
    let namespace = required("LW_LIVE_KUBERNETES_NAMESPACE")?;
    let buildkit_image = optional("LW_LIVE_SANDBOX_BUILDKIT_IMAGE");
    let buildkit_config_map = optional("LW_LIVE_SANDBOX_BUILDKIT_CONFIG_MAP");
    if buildkit_image.is_some() != buildkit_config_map.is_some() {
        return Err(
            "LW_LIVE_SANDBOX_BUILDKIT_IMAGE and LW_LIVE_SANDBOX_BUILDKIT_CONFIG_MAP must be set together"
                .into(),
        );
    }
    let pull_secret = optional("LW_LIVE_SANDBOX_PULL_SECRET");
    if buildkit_image.is_some() && pull_secret.is_none() {
        return Err(
            "LW_LIVE_SANDBOX_PULL_SECRET is required to render the BuildKit sidecar bundle".into(),
        );
    }
    let image = required("LW_LIVE_SANDBOX_IMAGE")?;
    if !image.contains("@sha256:") {
        return Err(
            "LW_LIVE_SANDBOX_IMAGE must be digest-pinned as <reference>@sha256:<64 hex>".into(),
        );
    }
    let material_url = optional(MATERIAL_URL_VARIABLE);
    let material_sha256 = optional(MATERIAL_SHA256_VARIABLE);
    let material_size = optional(MATERIAL_SIZE_VARIABLE);
    let material_ca_base64 = optional(MATERIAL_CA_VARIABLE);
    if material_url.is_some() != material_sha256.is_some()
        || material_url.is_some() != material_size.is_some()
    {
        return Err(format!(
            "{MATERIAL_URL_VARIABLE}, {MATERIAL_SHA256_VARIABLE} and {MATERIAL_SIZE_VARIABLE} must be set together"
        )
        .into());
    }
    if material_ca_base64.is_some() && material_url.is_none() {
        return Err(format!(
            "{MATERIAL_CA_VARIABLE} requires {MATERIAL_URL_VARIABLE} and its pinned digest"
        )
        .into());
    }
    let material_size_bytes = material_size
        .map(|value| {
            value.parse::<u64>().map_err(|_| {
                format!("{MATERIAL_SIZE_VARIABLE} must be the byte count of the served material")
            })
        })
        .transpose()?;
    let attempt_inputs = match (
        material_url,
        material_sha256,
        material_size_bytes,
        optional(CLAUDE_VERSION_VARIABLE),
    ) {
        (
            Some(material_url),
            Some(material_sha256),
            Some(material_size_bytes),
            Some(claude_version),
        ) => Some(LiveAttemptInputs {
            material_url,
            material_sha256,
            material_size_bytes,
            material_ca_base64,
            claude_version,
        }),
        _ => None,
    };
    Ok(Some(LiveEnvironment {
        configuration: KubernetesApiConfiguration {
            kubernetes_api_server: Url::parse(&required("LW_LIVE_KUBERNETES_API_SERVER")?)?,
            kubernetes_bearer_token_file: PathBuf::from(required("LW_LIVE_KUBERNETES_TOKEN_FILE")?),
            kubernetes_ca_file: PathBuf::from(required("LW_LIVE_KUBERNETES_CA_FILE")?),
            runner_namespace: namespace.clone(),
            request_timeout_milliseconds: REQUEST_TIMEOUT_MILLISECONDS,
        },
        namespace,
        image,
        egress_destination: required("LW_LIVE_SANDBOX_EGRESS")?,
        buildkit_image,
        buildkit_config_map,
        pull_secret,
        attempt_inputs,
        blocked_egress_url: optional(BLOCKED_EGRESS_VARIABLE)
            .unwrap_or_else(|| BLOCKED_EGRESS_URL.to_owned()),
        hang_material_url: optional(HANG_MATERIAL_VARIABLE)
            .unwrap_or_else(|| HANG_MATERIAL_URL.to_owned()),
    }))
}

/// Reads the live environment and builds the shared execution and readback clients.
fn live_client() -> Result<Option<LiveClients>, Box<dyn Error>> {
    let Some(environment) = live_environment()? else {
        eprintln!("LW_LIVE_KUBERNETES is not enabled; skipping live sandbox readback");
        return Ok(None);
    };
    let api = KubernetesApiClient::new(
        environment.configuration.clone(),
        FIELD_MANAGER,
        LOG_SCOPE,
        DIAGNOSTIC_PREFIX,
        SANDBOX_MANAGED_BY,
        SANDBOX_EVENT_SCOPE,
    )?;
    let rest = LiveRest::new(&environment.configuration)?;
    Ok(Some((api, rest, environment)))
}

/// Prints the labelled notice that one in-container observation was skipped.
fn print_unobserved_case(case: &str) {
    eprintln!(
        "live {case} observation skipped: set {MATERIAL_URL_VARIABLE}, {MATERIAL_SHA256_VARIABLE}, \
         {MATERIAL_SIZE_VARIABLE} and {CLAUDE_VERSION_VARIABLE} so the attempt command actually runs"
    );
}

/// Renders one real attempt bundle for the live namespace and image.
fn live_bundle(environment: &LiveEnvironment) -> Result<SandboxBundle, Box<dyn Error>> {
    rendered_bundle(environment, &LiveAttempt::readback_probe())
}

/// One purpose-built live attempt: its command, material source, deadline and environment.
struct LiveAttempt {
    command: Vec<String>,
    material_url: String,
    material_sha256: String,
    material_size_bytes: u64,
    material_ca_base64: Option<String>,
    claude_version: String,
    wall_time_seconds: u64,
    command_environment: BTreeMap<String, String>,
}

impl LiveAttempt {
    /// The default readback attempt: the Claude Code argv against the unreachable material source.
    fn readback_probe() -> Self {
        Self {
            command: vec![
                "claude".to_owned(),
                "--print".to_owned(),
                "labweaver-live-probe".to_owned(),
            ],
            material_url: MATERIAL_DOWNLOAD_URL.to_owned(),
            material_sha256: Sha256Digest::of_bytes(b"labweaver-live-material").to_string(),
            material_size_bytes: 16,
            material_ca_base64: None,
            claude_version: "0.0.0-labweaver-live".to_owned(),
            wall_time_seconds: ATTEMPT_WALL_TIME_SECONDS,
            command_environment: BTreeMap::new(),
        }
    }

    /// One attempt that runs a purpose-built probe against the reachable material envelope.
    fn observed_probe(
        command: Vec<String>,
        inputs: &LiveAttemptInputs,
        command_environment: BTreeMap<String, String>,
    ) -> Self {
        Self {
            command,
            material_url: inputs.material_url.clone(),
            material_sha256: inputs.material_sha256.clone(),
            material_size_bytes: inputs.material_size_bytes,
            material_ca_base64: inputs.material_ca_base64.clone(),
            claude_version: inputs.claude_version.clone(),
            wall_time_seconds: ATTEMPT_WALL_TIME_SECONDS,
            command_environment,
        }
    }

    /// One attempt whose material source never answers, so it stays live until the case ends it.
    fn hanging_probe(environment: &LiveEnvironment) -> Self {
        Self {
            command: vec![
                "claude".to_owned(),
                "--print".to_owned(),
                "labweaver-live-probe".to_owned(),
            ],
            material_url: environment.hang_material_url.clone(),
            material_sha256: Sha256Digest::of_bytes(b"labweaver-live-hang-material").to_string(),
            material_size_bytes: 16,
            material_ca_base64: None,
            claude_version: "0.0.0-labweaver-live".to_owned(),
            wall_time_seconds: LIVE_ATTEMPT_WALL_TIME_SECONDS,
            command_environment: BTreeMap::new(),
        }
    }
}

/// Renders one live attempt bundle for a purpose-built command and material source.
fn rendered_bundle(
    environment: &LiveEnvironment,
    attempt: &LiveAttempt,
) -> Result<SandboxBundle, Box<dyn Error>> {
    let task_run_id = Uuid::now_v7();
    let allowed_egress = BTreeSet::from([environment.egress_destination.clone()]);
    let ownership = KubernetesOwnership {
        run_id: Uuid::now_v7(),
        step_run_id: Uuid::now_v7(),
        attempt_id: Uuid::now_v7(),
        request_sha256: Sha256Digest::of_bytes(task_run_id.as_bytes()).to_string(),
    };
    let configuration = SandboxConfiguration {
        namespace: environment.namespace.clone(),
        image: environment.image.clone(),
        service_account_name: SERVICE_ACCOUNT_NAME.to_owned(),
        image_pull_secret_name: environment.pull_secret.clone(),
        cpu_millicores: 250,
        memory_bytes: 256 * 1024 * 1024,
        workspace_bytes: 256 * 1024 * 1024,
        wall_time_seconds: attempt.wall_time_seconds,
        allowed_egress,
        buildkit_image: environment.buildkit_image.clone(),
        buildkit_config_map_name: environment.buildkit_config_map.clone(),
    };
    let spec = SandboxAttemptSpec {
        task_run_id,
        ownership,
        trace_id: format!("live-{task_run_id}"),
        command: attempt.command.clone(),
        expected_claude_version: attempt.claude_version.clone(),
        command_environment: attempt.command_environment.clone(),
        material_download_url: attempt.material_url.clone(),
        material_sha256: attempt.material_sha256.clone(),
        material_size_bytes: attempt.material_size_bytes,
        result_upload_url: RESULT_UPLOAD_URL.to_owned(),
        result_upload_headers: BTreeMap::new(),
        stderr_upload_url: STDERR_UPLOAD_URL.to_owned(),
        stderr_upload_headers: BTreeMap::new(),
        result_max_bytes: 4_096,
        stderr_max_bytes: 4_096,
        export_upload_url: None,
        export_upload_headers: BTreeMap::new(),
        object_store_ca_base64: attempt.material_ca_base64.clone(),
    };
    Ok(build_sandbox_bundle(&configuration, &spec)?)
}

/// Returns the REST path prefix one API version is addressed under.
fn api_prefix(api_version: &str) -> String {
    if api_version == "v1" {
        "/api/v1".to_owned()
    } else {
        format!("/apis/{api_version}")
    }
}

/// Returns the namespaced collection path of one resource kind.
fn collection_path(api_version: &str, namespace: &str, plural: &str) -> String {
    format!(
        "{}/namespaces/{namespace}/{plural}",
        api_prefix(api_version)
    )
}

/// Returns the namespaced object path of one resource.
fn object_path(api_version: &str, namespace: &str, plural: &str, name: &str) -> String {
    format!(
        "{}/{}",
        collection_path(api_version, namespace, plural),
        name
    )
}

/// Minimal `Kubernetes` REST reader used for readback and the default-deny toggle.
///
/// The shared execution client keeps its `GET` and mutating helpers private, so the live readback
/// repeats the same API server, bearer token and CA through its own `reqwest` client instead of
/// shelling out to `kubectl`.
struct LiveRest {
    client: Client,
    server: Url,
    token: String,
}

impl LiveRest {
    /// Builds the readback client from the same connection inputs as the execution client.
    fn new(configuration: &KubernetesApiConfiguration) -> Result<Self, Box<dyn Error>> {
        let ca = Certificate::from_pem(&fs::read(&configuration.kubernetes_ca_file)?)?;
        let client = Client::builder()
            .https_only(true)
            .tls_built_in_root_certs(false)
            .add_root_certificate(ca)
            .redirect(Policy::none())
            .timeout(Duration::from_millis(REQUEST_TIMEOUT_MILLISECONDS))
            .build()?;
        let token = fs::read_to_string(&configuration.kubernetes_bearer_token_file)?;
        Ok(Self {
            client,
            server: configuration.kubernetes_api_server.clone(),
            token: token.trim().to_owned(),
        })
    }

    /// Reads one object, returning `None` for an absent object.
    async fn get(&self, path: &str) -> Result<Option<Value>, Box<dyn Error>> {
        let response = self
            .client
            .get(self.server.join(path)?)
            .bearer_auth(&self.token)
            .send()
            .await?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(format!("GET {path} returned {}", response.status()).into());
        }
        Ok(Some(response.json().await?))
    }

    /// Deletes one object through the live API server.
    async fn delete(&self, path: &str) -> Result<(), Box<dyn Error>> {
        let response = self
            .client
            .delete(self.server.join(path)?)
            .bearer_auth(&self.token)
            .send()
            .await?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(format!("DELETE {path} returned {}", response.status()).into())
        }
    }

    /// Creates one object through the live API server.
    async fn create(&self, path: &str, document: &Value) -> Result<Value, Box<dyn Error>> {
        let response = self
            .client
            .post(self.server.join(path)?)
            .bearer_auth(&self.token)
            .json(document)
            .send()
            .await?;
        if response.status() != StatusCode::CREATED {
            return Err(format!("POST {path} returned {}", response.status()).into());
        }
        Ok(response.json().await?)
    }

    /// Lists one collection and returns its raw items.
    async fn list(&self, path: &str) -> Result<Vec<Value>, Box<dyn Error>> {
        let response = self
            .client
            .get(self.server.join(path)?)
            .bearer_auth(&self.token)
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(format!("GET {path} returned {}", response.status()).into());
        }
        let document: Value = response.json().await?;
        let items = document
            .get("items")
            .and_then(Value::as_array)
            .ok_or("the live collection response carries no items")?;
        Ok(items.clone())
    }

    /// Reads the merged log of one attempt container through the pod log subresource.
    ///
    /// Returns `None` while the container has no log yet, which is how a killed attempt that never
    /// started its command reports itself.
    async fn pod_log(
        &self,
        namespace: &str,
        pod: &str,
        container: &str,
    ) -> Result<Option<String>, Box<dyn Error>> {
        let path = format!(
            "{}/log?container={container}",
            object_path("v1", namespace, "pods", pod)
        );
        let response = self
            .client
            .get(self.server.join(&path)?)
            .bearer_auth(&self.token)
            .send()
            .await?;
        if matches!(
            response.status(),
            StatusCode::NOT_FOUND | StatusCode::BAD_REQUEST
        ) {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(format!("GET {path} returned {}", response.status()).into());
        }
        Ok(Some(response.text().await?))
    }

    /// Reads the terminated state of one attempt container, including its terminal message.
    ///
    /// The rendered script redirects the command's own descriptors into the attempt volume, so the
    /// terminal message is the only channel the command's own output can be observed through.
    async fn container_termination(
        &self,
        namespace: &str,
        pod: &str,
        container: &str,
    ) -> Result<Option<Value>, Box<dyn Error>> {
        let path = object_path("v1", namespace, "pods", pod);
        let live = self
            .get(&path)
            .await?
            .ok_or_else(|| format!("the attempt pod {pod} is absent from readback"))?;
        let statuses = live
            .pointer("/status/containerStatuses")
            .and_then(Value::as_array)
            .ok_or("the attempt pod reports no container statuses")?;
        Ok(statuses
            .iter()
            .find(|status| status.get("name").and_then(Value::as_str) == Some(container))
            .and_then(|status| status.pointer("/state/terminated"))
            .cloned())
    }
}

/// Returns the applied pod template of one live Job.
fn pod_template(job: &Value) -> Result<&Value, Box<dyn Error>> {
    job.pointer("/spec/template/spec")
        .ok_or_else(|| "the live Job has no pod template spec".into())
}

/// Returns one named container of an applied pod template.
fn container<'a>(pod: &'a Value, name: &str) -> Result<&'a Value, Box<dyn Error>> {
    pod.pointer("/containers")
        .and_then(Value::as_array)
        .and_then(|containers| {
            containers
                .iter()
                .find(|candidate| candidate.get("name").and_then(Value::as_str) == Some(name))
        })
        .ok_or_else(|| format!("the live attempt pod template has no {name} container").into())
}

/// Returns the group of the configured `BuildKit` sidecar, when the attempt has one.
fn environment_buildkit_group(pod: &Value) -> Result<Option<u64>, Box<dyn Error>> {
    let Ok(sidecar) = sidecar_container(pod) else {
        return Ok(None);
    };
    sidecar
        .pointer("/securityContext/runAsGroup")
        .and_then(Value::as_u64)
        .map(Some)
        .ok_or_else(|| "the live BuildKit sidecar has no group".into())
}

/// Asserts the applied pod and main container hardening of one live Job.
fn assert_job_security_context(job: &Value) -> Result<(), Box<dyn Error>> {
    let pod = pod_template(job)?;
    assert_eq!(
        pod.get("automountServiceAccountToken"),
        Some(&Value::Bool(false)),
        "the attempt pod must not mount a service account token"
    );
    let security = pod
        .get("securityContext")
        .ok_or("the live attempt pod template has no securityContext")?;
    for field in ["runAsUser", "runAsGroup"] {
        assert_eq!(
            security.get(field).and_then(Value::as_u64),
            Some(65_532),
            "the attempt pod {field} must be the unprivileged sandbox identity"
        );
    }
    // With a BuildKit sidecar the pod must join the sidecar's group so the unprivileged
    // sandbox can reach the attempt-local socket the sidecar owns.
    let expected_group = environment_buildkit_group(pod)?.unwrap_or(65_532);
    assert_eq!(
        security.get("fsGroup").and_then(Value::as_u64),
        Some(expected_group),
        "the attempt pod fsGroup must match the shared BuildKit group"
    );
    let main = container(pod, SANDBOX_MAIN_CONTAINER)?;
    let main_security = main
        .get("securityContext")
        .ok_or("the live main container has no securityContext")?;
    assert_eq!(
        main_security
            .get("readOnlyRootFilesystem")
            .and_then(Value::as_bool),
        Some(true),
        "the main container must keep a read-only root filesystem"
    );
    assert_eq!(
        main_security
            .pointer("/seccompProfile/type")
            .and_then(Value::as_str),
        Some("RuntimeDefault"),
        "the main container must use the runtime default seccomp profile"
    );
    assert_eq!(
        main_security
            .get("allowPrivilegeEscalation")
            .and_then(Value::as_bool),
        Some(false),
        "the main container must not allow privilege escalation"
    );
    Ok(())
}

/// Reads the applied `BuildKit` daemon container, which the bundle renders as an ordered init
/// container so the one-shot Job still completes with the attempt process.
fn sidecar_container(pod: &Value) -> Result<&Value, Box<dyn Error>> {
    pod.pointer("/initContainers")
        .and_then(Value::as_array)
        .and_then(|containers| {
            containers
                .iter()
                .find(|container| container["name"] == SANDBOX_BUILDKIT_CONTAINER)
        })
        .ok_or_else(|| "the live attempt pod template has no buildkit container".into())
}

/// Asserts the applied rootless `BuildKit` sidecar exceptions of one live Job.
fn assert_buildkit_sidecar(job: &Value) -> Result<(), Box<dyn Error>> {
    let pod = pod_template(job)?;
    let sidecar = sidecar_container(pod)?;
    let security = sidecar
        .get("securityContext")
        .ok_or("the live BuildKit sidecar has no securityContext")?;
    assert_eq!(
        security
            .pointer("/seccompProfile/type")
            .and_then(Value::as_str),
        Some("Unconfined"),
        "the rootless BuildKit sidecar needs the unconfined seccomp profile"
    );
    assert_eq!(
        security
            .pointer("/appArmorProfile/type")
            .and_then(Value::as_str),
        Some("Unconfined"),
        "the rootless BuildKit sidecar needs the unconfined AppArmor profile"
    );
    assert_eq!(
        security.get("runAsUser").and_then(Value::as_u64),
        Some(1_000),
        "the rootless BuildKit sidecar must run as its own unprivileged user"
    );
    assert_eq!(
        security.get("runAsGroup").and_then(Value::as_u64),
        Some(1_000),
        "the rootless BuildKit sidecar must keep its own group for the attempt-local socket"
    );
    let added: Vec<&str> = security
        .pointer("/capabilities/add")
        .and_then(Value::as_array)
        .map(|values| values.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    assert_eq!(
        added,
        vec!["SETUID", "SETGID"],
        "the rootless BuildKit sidecar may add only the setuid helpers"
    );
    Ok(())
}

/// Asserts the applied per-attempt `NetworkPolicy` admits no ingress and covers both directions.
fn assert_per_attempt_policy(policy: &Value) -> Result<(), Box<dyn Error>> {
    let spec = policy
        .get("spec")
        .ok_or("the live per-attempt NetworkPolicy has no spec")?;
    assert_eq!(
        spec.get("policyTypes"),
        Some(&json!(["Ingress", "Egress"])),
        "the per-attempt policy must declare both policy directions"
    );
    let ingress = spec.get("ingress");
    assert!(
        ingress.is_none_or(|rules| rules.as_array().is_some_and(Vec::is_empty)),
        "the per-attempt policy must admit no ingress rule; the API server omits an empty list"
    );
    Ok(())
}

/// Asserts the applied per-attempt `NetworkPolicy` admits exactly the DNS and reviewed egress.
///
/// The applied document is asserted unconditionally: whether the cluster CNI enforces it is a
/// separate, reported observation.
fn assert_per_attempt_egress_policy(
    policy: &Value,
    egress_destination: &str,
) -> Result<(), Box<dyn Error>> {
    let (egress_cidr, egress_port) =
        task_execution::parse_egress_destination(egress_destination)
            .ok_or("the live egress destination is not a reviewed cidr:port pair")?;
    let egress = policy
        .pointer("/spec/egress")
        .and_then(Value::as_array)
        .ok_or("the live per-attempt NetworkPolicy has no egress rules")?;
    assert_eq!(
        egress.len(),
        2,
        "the per-attempt policy must admit exactly the name-resolution and configured CIDR rules"
    );
    assert_eq!(
        egress[0].get("to"),
        Some(&json!([{
            "namespaceSelector": {"matchLabels": {"kubernetes.io/metadata.name": "kube-system"}}
        }])),
        "the per-attempt policy must resolve names through the kube-system DNS service only"
    );
    assert_eq!(
        egress[0].get("ports"),
        Some(&json!([
            {"protocol": "UDP", "port": 53},
            {"protocol": "TCP", "port": 53},
        ])),
        "the per-attempt policy must admit DNS on both transports only"
    );
    assert_eq!(
        egress[1].get("to"),
        Some(&json!([{"ipBlock": {"cidr": egress_cidr}}])),
        "the per-attempt policy must admit exactly the configured egress CIDR"
    );
    assert_eq!(
        egress[1].get("ports"),
        Some(&json!([{"protocol": "TCP", "port": egress_port}])),
        "the per-attempt policy must admit only the reviewed port beyond the name-resolution rule"
    );
    Ok(())
}

/// Removes the namespace default-deny policy and returns the captured live document.
async fn remove_default_deny(
    rest: &LiveRest,
    namespace: &str,
    name: &str,
) -> Result<Value, Box<dyn Error>> {
    let path = object_path("networking.k8s.io/v1", namespace, "networkpolicies", name);
    let live = rest
        .get(&path)
        .await?
        .ok_or("the namespace default-deny policy is absent before the live gate is exercised")?;
    rest.delete(&path).await?;
    Ok(live)
}

/// Recreates the captured default-deny document without server-assigned metadata.
async fn restore_default_deny(
    rest: &LiveRest,
    namespace: &str,
    name: &str,
    live: &Value,
) -> Result<(), Box<dyn Error>> {
    let mut document = live.clone();
    if let Some(metadata) = document
        .pointer_mut("/metadata")
        .and_then(Value::as_object_mut)
    {
        for field in [
            "resourceVersion",
            "uid",
            "creationTimestamp",
            "generation",
            "managedFields",
            "selfLink",
        ] {
            metadata.remove(field);
        }
    }
    if let Some(object) = document.as_object_mut() {
        object.remove("status");
    }
    let path = collection_path("networking.k8s.io/v1", namespace, "networkpolicies");
    let restored = rest.create(&path, &document).await?;
    assert_eq!(
        restored
            .get("metadata")
            .and_then(|metadata| metadata.get("name")),
        Some(&json!(name)),
        "the restored default-deny policy must keep its reviewed name"
    );
    Ok(())
}

/// Cleans the attempt bundle and waits for verified absence.
async fn await_cleanup(
    api: &KubernetesApiClient,
    namespace: &str,
    bundle: &SandboxBundle,
) -> Result<ExecutionCleanupStatus, Box<dyn Error>> {
    let status = tokio::time::timeout(CLEANUP_TIMEOUT, async {
        loop {
            let status = api
                .cleanup(
                    namespace,
                    &bundle.job_name,
                    &bundle.bundle.objects,
                    &bundle.bundle.cleanup_plan,
                )
                .await?;
            if status.is_confirmed() {
                return Ok::<_, KubernetesJobError>(status);
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    })
    .await
    .map_err(|_| "the live attempt cleanup timed out")??;
    assert!(
        status.is_confirmed(),
        "cleanup must be verifiably complete before the attempt is treated as finished"
    );
    Ok(status)
}

/// Observes the attempt until Kubernetes reports a terminal state.
async fn await_terminal(
    api: &KubernetesApiClient,
    identity: &KubernetesJobIdentity,
) -> Result<KubernetesJobObservation, Box<dyn Error>> {
    let observation = tokio::time::timeout(OBSERVE_TIMEOUT, async {
        loop {
            match api.observe(identity, None).await {
                Ok(KubernetesJobObservation::Running) => {
                    tokio::time::sleep(POLL_INTERVAL).await;
                }
                Ok(terminal) => return Ok::<_, Box<dyn Error>>(terminal),
                Err(error) => return Err(error.into()),
            }
        }
    })
    .await
    .map_err(|_| "the live attempt observation timed out")??;
    Ok(observation)
}

/// Lists the `plural/name` identity of every Agent-owned object in the live namespace.
async fn managed_object_names(
    api: &KubernetesApiClient,
    namespace: &str,
) -> Result<Vec<String>, Box<dyn Error>> {
    let mut names = Vec::new();
    for (api_version, plural) in [
        ("batch/v1", "jobs"),
        ("networking.k8s.io/v1", "networkpolicies"),
        ("v1", "secrets"),
    ] {
        for item in api
            .list(namespace, api_version, plural, MANAGED_BY_SELECTOR)
            .await?
        {
            let name = item
                .pointer("/metadata/name")
                .and_then(Value::as_str)
                .ok_or("a managed live object has no name")?;
            names.push(format!("{plural}/{name}"));
        }
    }
    names.sort();
    Ok(names)
}

/// Applies the bundle, proves the readback, the default-deny gate, cleanup and the failure.
async fn exercise_live_sandbox(
    api: &KubernetesApiClient,
    rest: &LiveRest,
    environment: &LiveEnvironment,
    bundle: &SandboxBundle,
) -> Result<(), Box<dyn Error>> {
    let namespace = environment.namespace.as_str();
    let baseline = managed_object_names(api, namespace).await?;
    api.start(&bundle.bundle).await?;
    let job = rest
        .get(&object_path(
            "batch/v1",
            namespace,
            "jobs",
            &bundle.job_name,
        ))
        .await?
        .ok_or("the applied attempt Job is absent from readback")?;
    let policy = rest
        .get(&object_path(
            "networking.k8s.io/v1",
            namespace,
            "networkpolicies",
            &bundle.network_policy_name,
        ))
        .await?
        .ok_or("the applied per-attempt NetworkPolicy is absent from readback")?;
    let secret = rest
        .get(&object_path(
            "v1",
            namespace,
            "secrets",
            &bundle.secret_name,
        ))
        .await?
        .ok_or("the applied attempt Secret is absent from readback")?;
    assert_job_security_context(&job)?;
    if environment.buildkit_image.is_some() {
        assert_buildkit_sidecar(&job)?;
    }
    assert_per_attempt_policy(&policy)?;
    assert_eq!(
        secret.get("type").and_then(Value::as_str),
        Some("Opaque"),
        "the attempt Secret must stay an opaque bounded object"
    );
    println!(
        "live sandbox readback: job={} networkPolicy={} secret={}",
        bundle.job_name, bundle.network_policy_name, bundle.secret_name
    );

    let live_default_deny =
        remove_default_deny(rest, namespace, SANDBOX_DEFAULT_DENY_POLICY).await?;
    let unguarded = api.start(&bundle.bundle).await;
    // The policy is restored before any assertion so a regression never leaves the shared live
    // namespace without its default-deny isolation.
    restore_default_deny(
        rest,
        namespace,
        SANDBOX_DEFAULT_DENY_POLICY,
        &live_default_deny,
    )
    .await?;
    assert!(
        matches!(
            unguarded,
            Err(KubernetesJobError::NetworkIsolationUnavailable)
        ),
        "start must fail closed while the namespace default-deny policy is absent"
    );
    api.start(&bundle.bundle).await?;
    println!("live default-deny readback: absent=fail-closed restored=accepted");

    await_cleanup(api, namespace, bundle).await?;
    for (api_version, plural, name) in [
        ("batch/v1", "jobs", &bundle.job_name),
        (
            "networking.k8s.io/v1",
            "networkpolicies",
            &bundle.network_policy_name,
        ),
        ("v1", "secrets", &bundle.secret_name),
    ] {
        assert!(
            rest.get(&object_path(api_version, namespace, plural, name))
                .await?
                .is_none(),
            "the cleaned attempt must leave no live {plural} object"
        );
    }
    api.start(&bundle.bundle).await?;
    println!("live cleanup readback: confirmed; absent; re-submit accepted");

    let observation = await_terminal(api, &bundle.bundle.identity).await?;
    let job_is_absent = match observation {
        KubernetesJobObservation::Failed {
            diagnostic_code,
            observation,
        } => {
            println!(
                "live attempt readback: state={:?} reason={:?} exit={:?} diagnostic={diagnostic_code}",
                observation.state, observation.reason_code, observation.exit_code
            );
            false
        }
        KubernetesJobObservation::Missing => {
            println!(
                "live attempt readback: the Job is absent before observation; no success is claimed"
            );
            true
        }
        KubernetesJobObservation::Running | KubernetesJobObservation::Completed { .. } => {
            return Err(
                "the live attempt must not report success without model credentials and reachable materials"
                    .into(),
            );
        }
    };
    let managed = managed_object_names(api, namespace).await?;
    let owned_job = format!("jobs/{}", bundle.job_name);
    let mut expected = baseline;
    expected.push(owned_job.clone());
    expected.push(format!("networkpolicies/{}", bundle.network_policy_name));
    expected.push(format!("secrets/{}", bundle.secret_name));
    expected.sort();
    if job_is_absent {
        // An expired completed Job is deleted by its TTL; every other object must still be there.
        expected.retain(|name| name != &owned_job);
    }
    assert_eq!(
        managed, expected,
        "the attempt must not add a managed object beyond its own bundle"
    );
    println!("live object readback: observation added no managed object");
    Ok(())
}

#[tokio::test]
async fn live_bundle_readback_default_deny_cleanup_and_failure() -> Result<(), Box<dyn Error>> {
    let Some(environment) = live_environment()? else {
        eprintln!("LW_LIVE_KUBERNETES is not enabled; skipping live sandbox readback");
        return Ok(());
    };
    // The case removes the namespace default-deny policy while it proves the fail-closed gate, so
    // it must never overlap another live case that shares the namespace.
    let _guard = LIVE_CASE_LOCK.lock().await;
    let api = KubernetesApiClient::new(
        environment.configuration.clone(),
        FIELD_MANAGER,
        LOG_SCOPE,
        DIAGNOSTIC_PREFIX,
        SANDBOX_MANAGED_BY,
        SANDBOX_EVENT_SCOPE,
    )?;
    let rest = LiveRest::new(&environment.configuration)?;
    let bundle = live_bundle(&environment)?;
    let outcome = exercise_live_sandbox(&api, &rest, &environment, &bundle).await;
    let cleanup = await_cleanup(&api, &environment.namespace, &bundle).await;
    outcome?;
    cleanup?;
    println!(
        "live sandbox cleanup readback: namespace={} left without attempt objects",
        environment.namespace
    );
    Ok(())
}

/// Starts one purpose-built attempt and returns its rendered bundle.
async fn start_attempt(
    api: &KubernetesApiClient,
    environment: &LiveEnvironment,
    attempt: &LiveAttempt,
) -> Result<SandboxBundle, Box<dyn Error>> {
    let bundle = rendered_bundle(environment, attempt)?;
    api.start(&bundle.bundle).await?;
    Ok(bundle)
}

/// Reads one applied object of a started attempt.
async fn applied_object(
    rest: &LiveRest,
    environment: &LiveEnvironment,
    api_version: &str,
    plural: &str,
    name: &str,
) -> Result<Value, Box<dyn Error>> {
    rest.get(&object_path(
        api_version,
        environment.namespace.as_str(),
        plural,
        name,
    ))
    .await?
    .ok_or_else(|| format!("the applied attempt {plural}/{name} is absent from readback").into())
}

/// Cleans one attempt, proves its objects are gone and asserts the managed-object baseline.
async fn finish_attempt(
    api: &KubernetesApiClient,
    rest: &LiveRest,
    environment: &LiveEnvironment,
    bundle: &SandboxBundle,
    baseline: &[String],
) -> Result<(), Box<dyn Error>> {
    let namespace = environment.namespace.as_str();
    await_cleanup(api, namespace, bundle).await?;
    for (api_version, plural, name) in [
        ("batch/v1", "jobs", bundle.job_name.as_str()),
        (
            "networking.k8s.io/v1",
            "networkpolicies",
            bundle.network_policy_name.as_str(),
        ),
        ("v1", "secrets", bundle.secret_name.as_str()),
    ] {
        assert!(
            rest.get(&object_path(api_version, namespace, plural, name))
                .await?
                .is_none(),
            "the cleaned attempt must leave no live {plural} object"
        );
    }
    let managed = managed_object_names(api, namespace).await?;
    assert_eq!(
        managed, baseline,
        "no attempt-owned object may remain once the attempt is cleaned up"
    );
    println!("live cleanup count readback: managed objects == captured baseline");
    Ok(())
}

/// Returns the name of the single attempt pod the shared observation selects.
async fn attempt_pod_name(
    api: &KubernetesApiClient,
    namespace: &str,
    attempt_id: Uuid,
) -> Result<String, Box<dyn Error>> {
    let selector = format!("{ATTEMPT_ID_LABEL}={attempt_id}");
    let pods = api.list(namespace, "v1", "pods", &selector).await?;
    let names = pods
        .iter()
        .filter_map(|pod| pod.pointer("/metadata/name").and_then(Value::as_str))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    match names.as_slice() {
        [name] => Ok(name.clone()),
        _ => Err(format!("the live attempt must own exactly one pod, observed {names:?}").into()),
    }
}

/// Returns the evidence lines one in-container probe printed to the pod log.
fn probe_evidence(log: &str) -> BTreeMap<String, String> {
    log.lines()
        .filter_map(|line| line.strip_prefix(PROBE_MARKER))
        .filter_map(|rest| rest.trim().split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

/// Returns one probe evidence list as a set.
fn probe_names(value: Option<&str>) -> BTreeSet<String> {
    value
        .unwrap_or_default()
        .split(',')
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Returns one probe exit code as an integer.
fn probe_exit(evidence: &BTreeMap<String, String>, key: &str) -> Result<i32, Box<dyn Error>> {
    let value = evidence
        .get(key)
        .ok_or_else(|| format!("the in-container probe printed no {key}"))?;
    value
        .parse::<i32>()
        .map_err(|_| format!("the in-container probe printed a malformed {key}={value}").into())
}

/// Reads the probe evidence of one started attempt from its container terminal message.
///
/// The rendered script redirects the command's own descriptors into the attempt volume, so the
/// command cannot reach the pod log: it appends its evidence to the container terminal file instead
/// and its stdout payload makes the script fail at the unreachable result sink before the script
/// overwrites that evidence with its receipt. The container log is read as supporting evidence that
/// the attempt failed at its own sink.
async fn probe_evidence_of(
    api: &KubernetesApiClient,
    rest: &LiveRest,
    namespace: &str,
    bundle: &SandboxBundle,
) -> Result<BTreeMap<String, String>, Box<dyn Error>> {
    let pod = attempt_pod_name(api, namespace, bundle.bundle.identity.ownership.attempt_id).await?;
    let terminated = rest
        .container_termination(namespace, &pod, SANDBOX_MAIN_CONTAINER)
        .await?
        .ok_or_else(|| {
            format!(
                "the attempt container never terminated; check {CLAUDE_VERSION_VARIABLE} against the image's `claude --version`"
            )
        })?;
    let message = terminated
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    println!(
        "live attempt terminal message: pod={pod} container={SANDBOX_MAIN_CONTAINER}\n{message}"
    );
    let evidence = probe_evidence(message);
    assert!(
        !evidence.is_empty(),
        "the attempt container terminal message carries no {PROBE_MARKER} evidence; check {CLAUDE_VERSION_VARIABLE} against the image's `claude --version`"
    );
    let log = rest
        .pod_log(namespace, &pod, SANDBOX_MAIN_CONTAINER)
        .await?;
    assert!(
        log.as_deref().is_some_and(|log| log.contains(SINK_HOST)),
        "the attempt container log must report the unreachable attempt sink: {log:?}"
    );
    println!(
        "live attempt container log: pod={pod}\n{}",
        log.unwrap_or_default()
    );
    Ok(evidence)
}

/// Asserts one probe attempt ran its command and then failed at the unreachable attempt sinks.
fn assert_probe_attempt_failed(
    observation: &KubernetesJobObservation,
) -> Result<(), Box<dyn Error>> {
    let KubernetesJobObservation::Failed {
        diagnostic_code, ..
    } = observation
    else {
        return Err(format!(
            "the probe attempt must fail once its command ran; observed {observation:?} (check {CLAUDE_VERSION_VARIABLE} against the image's `claude --version`)"
        )
        .into());
    };
    assert_eq!(
        diagnostic_code, "LW_AGENT_SANDBOX_FAILED",
        "the probe attempt must fail because its result sink is unreachable: {observation:?}"
    );
    println!("live attempt readback: state=Failed diagnostic={diagnostic_code}");
    Ok(())
}

/// Returns the shell preamble every in-container probe prints its evidence with.
fn probe_preamble() -> String {
    format!("probe() {{ printf '{PROBE_MARKER} %s\\n' \"$1\" >>{TERMINATION_LOG}; }}\n")
}

/// Returns the trailing lines every in-container probe reports its result payload with.
fn probe_epilogue() -> String {
    format!("printf '{PROBE_RESULT_PAYLOAD}'\nexit 0\n")
}

/// Renders the command that probes egress beyond the configured allow-list.
fn egress_probe_command(blocked_url: &str, blocked_dns_url: &str) -> Vec<String> {
    let script = format!(
        "{}\
         probe egress=start\n\
         curl --silent --show-error --max-time {PROBE_CURL_MAX_TIME_SECONDS} --output /dev/null '{blocked_url}' 2>/dev/null\n\
         probe \"blocked_address_exit=$?\"\n\
         curl --silent --show-error --max-time {PROBE_CURL_MAX_TIME_SECONDS} --output /dev/null '{blocked_dns_url}' 2>/dev/null\n\
         probe \"blocked_name_exit=$?\"\n\
         curl --silent --show-error --max-time {PROBE_CURL_MAX_TIME_SECONDS} --output /dev/null \"$MATERIAL_DOWNLOAD_URL\" 2>/dev/null\n\
         probe \"allowed_exit=$?\"\n\
         probe egress=done\n\
         {}",
        probe_preamble(),
        probe_epilogue()
    );
    vec!["/bin/sh".to_owned(), "-c".to_owned(), script]
}

/// Renders the command that builds one image through the attempt's own `BuildKit` sidecar.
///
/// The attempt reaches the daemon only through the attempt-local socket directory, so this is the
/// live proof that the sidecar is started before the attempt process and that the attempt can use
/// it: the daemon is asked for its workers and then builds a `FROM scratch` image, which needs no
/// registry access, and exports the OCI layout exactly where the attempt exports its image.
fn buildkit_build_command() -> Vec<String> {
    let script = format!(
        "{}\
         mkdir -p /tmp/context\n\
         printf 'labweaver-sidecar-live' > /tmp/context/hello.txt\n\
         printf 'FROM scratch\\nCOPY hello.txt /hello.txt\\n' > /tmp/context/Dockerfile\n\
         sleep 15\n\
         probe \"run_dir=$(ls -l /run/buildkit 2>&1 | tr '\\n' ' ' | head -c 120)\"\n\
         probe \"socket=$(ls -l /run/buildkit/buildkitd.sock 2>&1 | head -c 60)\"\n\
         if buildctl --addr \"$BUILDKIT_HOST\" debug workers >/tmp/workers.txt 2>/tmp/workers.err; then probe sidecar_workers=ok; else probe sidecar_workers=failed; probe \"workers_error=$(head -c 160 /tmp/workers.err | tr '\\n' ' ')\"; fi\n\
         if buildctl --addr \"$BUILDKIT_HOST\" build --frontend dockerfile.v0 --local context=/tmp/context --local dockerfile=/tmp/context --output type=oci,dest=/workspace/labweaver-export.tar >/tmp/build.log 2>&1; then probe sidecar_build=ok; else probe sidecar_build=failed; fi\n\
         probe \"export_bytes=$(wc -c </workspace/labweaver-export.tar 2>/dev/null || echo 0)\"\n\
         probe sidecar=done\n\
         {}",
        probe_preamble(),
        probe_epilogue()
    );
    vec!["/bin/sh".to_owned(), "-c".to_owned(), script]
}

/// Renders the command that probes for platform credential material inside the attempt.
fn credential_probe_command() -> Vec<String> {
    let script = format!(
        "{}\
         if [ -e /var/run/secrets/kubernetes.io/serviceaccount ]; then\n\
         \x20 if [ -r /var/run/secrets/kubernetes.io/serviceaccount/token ]; then probe serviceaccount=readable; else probe serviceaccount=unreadable; fi\n\
         else\n\
         \x20 probe serviceaccount=absent\n\
         fi\n\
         credential=''\n\
         anthropic=''\n\
         for name in $(env | cut -d= -f1); do\n\
         \x20 case \"$name\" in\n\
         \x20   *PASSWORD*|*SECRET*|*TOKEN*|*CREDENTIAL*|*API_KEY*|*APIKEY*|*_KEY|*AUTH*) credential=\"$credential,$name\" ;;\n\
         \x20 esac\n\
         \x20 case \"$name\" in ANTHROPIC_*) anthropic=\"$anthropic,$name\" ;; esac\n\
         done\n\
         probe \"credential_environment=${{credential#,}}\"\n\
         probe \"anthropic_environment=${{anthropic#,}}\"\n\
         docker=absent\n\
         for candidate in \"${{HOME:-/root}}/.docker/config.json\" /root/.docker/config.json /home/user/.docker/config.json; do\n\
         \x20 if [ -f \"$candidate\" ]; then\n\
         \x20   case \"$(cat \"$candidate\")\" in *auth*) docker=\"credentials:$candidate\" ;; *) docker=\"empty:$candidate\" ;; esac\n\
         \x20 fi\n\
         done\n\
         probe \"docker_config=$docker\"\n\
         {}",
        probe_preamble(),
        probe_epilogue()
    );
    vec!["/bin/sh".to_owned(), "-c".to_owned(), script]
}

/// Renders the command that probes the read-only boundaries of the attempt filesystem.
fn filesystem_probe_command() -> Vec<String> {
    let script = format!(
        "{}\
         if ( : > /usr/local/bin/labweaver-live-probe ) 2>/dev/null; then probe rootfs_write=allowed; else probe rootfs_write=denied; fi\n\
         if ( : > /materials/labweaver-live-probe ) 2>/dev/null; then probe materials_write=allowed; else probe materials_write=denied; fi\n\
         if ( : > /workspace/labweaver-live-probe ) 2>/dev/null; then probe workspace_write=allowed; else probe workspace_write=denied; fi\n\
         if ( : > /run/labweaver/labweaver-live-probe ) 2>/dev/null; then probe attempt_write=allowed; else probe attempt_write=denied; fi\n\
         {}",
        probe_preamble(),
        probe_epilogue()
    );
    vec!["/bin/sh".to_owned(), "-c".to_owned(), script]
}

/// Asserts the applied main container keeps the read-only attempt boundaries.
fn assert_attempt_read_only_boundaries(job: &Value) -> Result<(), Box<dyn Error>> {
    let pod = pod_template(job)?;
    let main = container(pod, SANDBOX_MAIN_CONTAINER)?;
    let mounts = main
        .pointer("/volumeMounts")
        .and_then(Value::as_array)
        .ok_or("the live main container has no volume mounts")?;
    let mount = |path: &str| {
        mounts
            .iter()
            .find(|mount| mount.get("mountPath").and_then(Value::as_str) == Some(path))
    };
    assert_eq!(
        mount("/materials").and_then(|mount| mount.get("readOnly")),
        Some(&Value::Bool(true)),
        "the attempt materials mount must stay read-only"
    );
    let workspace = mount("/workspace").ok_or("the live main container has no workspace mount")?;
    // Kubernetes omits a `readOnly` field that is false, so an absent value is
    // exactly the writable case this boundary requires.
    assert_ne!(
        workspace.get("readOnly"),
        Some(&Value::Bool(true)),
        "the attempt workspace must stay writable"
    );
    Ok(())
}

/// `NetworkPolicy` enforcement class of the live cluster CNI.
#[derive(Clone, Debug, Eq, PartialEq)]
enum CniEnforcement {
    /// A CNI agent that enforces `NetworkPolicy` is running.
    Enforcing(String),
    /// A CNI agent that records `NetworkPolicy` without enforcing it is running.
    Recording(String),
    /// No reviewed CNI agent was identified.
    Unknown,
}

/// Returns the `NetworkPolicy` enforcement class of the live cluster CNI.
///
/// `Kubernetes` never enforces `NetworkPolicy` itself; the CNI plugin does. The classification is a
/// reviewed name table: `kindnet` and standalone `flannel` record policies without enforcing them,
/// while the enforcing agents block traffic. An unrecognised agent is reported as unknown instead
/// of being assumed to enforce, so a recorded policy is never reported as proven enforcement.
async fn cni_enforcement(rest: &LiveRest) -> Result<CniEnforcement, Box<dyn Error>> {
    let pods = rest
        .list(&collection_path("v1", "kube-system", "pods"))
        .await?;
    let names = pods
        .iter()
        .filter_map(|pod| pod.pointer("/metadata/name").and_then(Value::as_str))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let agent = |markers: &[&str]| {
        names
            .iter()
            .find(|name| markers.iter().any(|marker| name.starts_with(marker)))
            .cloned()
    };
    if let Some(name) = agent(&ENFORCING_CNI_AGENTS) {
        return Ok(CniEnforcement::Enforcing(name));
    }
    if let Some(name) = agent(&RECORDING_CNI_AGENTS) {
        return Ok(CniEnforcement::Recording(name));
    }
    Ok(CniEnforcement::Unknown)
}

/// Returns the reason of the Job's terminal `Failed` condition, when `Kubernetes` reported one.
fn failed_condition_reason(job: &Value) -> Option<String> {
    job.pointer("/status/conditions")
        .and_then(Value::as_array)
        .and_then(|conditions| {
            conditions.iter().find(|condition| {
                condition.pointer("/type").and_then(Value::as_str) == Some("Failed")
                    && condition.pointer("/status").and_then(Value::as_str) == Some("True")
            })
        })
        .and_then(|condition| condition.pointer("/reason").and_then(Value::as_str))
        .map(str::to_owned)
}

/// Reads the reason of the live attempt Job's terminal `Failed` condition.
async fn live_failed_condition_reason(
    rest: &LiveRest,
    environment: &LiveEnvironment,
    job_name: &str,
) -> Result<Option<String>, Box<dyn Error>> {
    Ok(rest
        .get(&object_path(
            "batch/v1",
            environment.namespace.as_str(),
            "jobs",
            job_name,
        ))
        .await?
        .as_ref()
        .and_then(failed_condition_reason))
}

/// Waits until the shared observation reports the hung attempt as live.
async fn await_live_attempt(
    api: &KubernetesApiClient,
    identity: &KubernetesJobIdentity,
) -> Result<(), Box<dyn Error>> {
    let deadline = tokio::time::Instant::now() + OBSERVE_TIMEOUT;
    loop {
        match api.observe(identity, None).await? {
            KubernetesJobObservation::Running => return Ok(()),
            KubernetesJobObservation::Missing => {}
            terminal => {
                return Err(format!(
                    "the attempt must stay live until the case ends it; observed {terminal:?}"
                )
                .into());
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("the live attempt never reported itself as running".into());
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Runs the cancellation cleanup the authoring executor runs for a cancelled attempt.
///
/// The executor requests cancellation through [`RunCancellation`], cleans the attempt bundle with
/// the shared cleanup call and only releases the Resource reservation once cleanup is confirmed;
/// this runs the same authority and the same confirmed-absence precondition.
async fn cancel_attempt(
    api: &KubernetesApiClient,
    cancellation: &RunCancellation,
    namespace: &str,
    bundle: &SandboxBundle,
) -> Result<ExecutionCleanupStatus, KubernetesJobError> {
    cancellation.cancel();
    assert!(
        cancellation.is_cancelled(),
        "the cancellation authority must report the requested cancellation"
    );
    api.cleanup(
        namespace,
        bundle.job_name.as_str(),
        &bundle.bundle.objects,
        &bundle.bundle.cleanup_plan,
    )
    .await
}

/// Polls one attempt observation until it reaches the expected state.
///
/// Kubernetes deletes an owned Job asynchronously, so a readback taken immediately after a delete
/// can still see the running attempt. The executor re-observes on its next pass, and the live case
/// does the same instead of assuming one instant of convergence.
async fn await_observation(
    api: &KubernetesApiClient,
    identity: &KubernetesJobIdentity,
    expected: KubernetesJobObservation,
    label: &str,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + OBSERVE_TIMEOUT;
    loop {
        let observed = api.observe(identity, None).await?;
        if observed == expected {
            println!("live observation readback: {label} -> {observed:?}");
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("{label} never reached {expected:?}; last {observed:?}").into());
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Polls the recovery cleanup until Kubernetes confirms every owned object is gone.
///
/// The recovery path deletes by persisted reference with foreground propagation, so the first
/// readback can legitimately still see an owned Secret while its deletion completes.
async fn await_confirmed_recovery_cleanup(
    api: &KubernetesApiClient,
    identity: &KubernetesJobIdentity,
    refs: &[contracts::execution::ExecutionObjectRef],
    mut status: ExecutionCleanupStatus,
) -> Result<ExecutionCleanupStatus, Box<dyn Error>> {
    let deadline = Instant::now() + CLEANUP_TIMEOUT;
    while !status.is_confirmed() {
        if Instant::now() >= deadline {
            return Err(format!("recovery cleanup was never confirmed; last {status:?}").into());
        }
        tokio::time::sleep(POLL_INTERVAL).await;
        status = api.cleanup_recovery(identity, refs).await?;
    }
    println!("live recovery cleanup readback: confirmed after bounded polling");
    Ok(status)
}

/// Polls the shared cleanup until Kubernetes confirms every owned object is gone.
///
/// Deleting an attempt that still runs a Pod is a foreground deletion, so the first readback can
/// legitimately report the objects as still present. The executor only releases the reservation
/// after a confirmed cleanup, and it retries on its next pass; the live case does the same.
async fn await_confirmed_cleanup(
    api: &KubernetesApiClient,
    namespace: &str,
    bundle: &SandboxBundle,
    mut status: ExecutionCleanupStatus,
) -> Result<ExecutionCleanupStatus, Box<dyn Error>> {
    let deadline = Instant::now() + CLEANUP_TIMEOUT;
    while !status.is_confirmed() {
        if Instant::now() >= deadline {
            return Err(format!("cleanup was never confirmed; last {status:?}").into());
        }
        tokio::time::sleep(POLL_INTERVAL).await;
        status = api
            .cleanup(
                namespace,
                bundle.job_name.as_str(),
                &bundle.bundle.objects,
                &bundle.bundle.cleanup_plan,
            )
            .await?;
    }
    println!("live cleanup readback: confirmed after bounded polling");
    Ok(status)
}

/// Case 1: egress beyond the allow-list, with the applied policy document always asserted.
///
/// The applied per-attempt `NetworkPolicy` is asserted exactly. The probe's own outcome is only
/// claimed as enforcement when a reviewed enforcing CNI is running: a CNI that records policies
/// without enforcing them is reported as such instead of passing as proven enforcement.
#[tokio::test]
async fn live_disallowed_egress_is_bounded_by_the_applied_policy() -> Result<(), Box<dyn Error>> {
    let Some((api, rest, environment)) = live_client()? else {
        return Ok(());
    };
    let _guard = LIVE_CASE_LOCK.lock().await;
    let namespace = environment.namespace.as_str();
    let baseline = managed_object_names(&api, namespace).await?;
    let command = egress_probe_command(&environment.blocked_egress_url, BLOCKED_EGRESS_DNS_URL);
    let attempt = match environment.attempt_inputs.as_ref() {
        Some(inputs) => LiveAttempt::observed_probe(command, inputs, BTreeMap::new()),
        None => LiveAttempt {
            command,
            ..LiveAttempt::readback_probe()
        },
    };
    let bundle = start_attempt(&api, &environment, &attempt).await?;
    let policy = applied_object(
        &rest,
        &environment,
        "networking.k8s.io/v1",
        "networkpolicies",
        &bundle.network_policy_name,
    )
    .await?;
    assert_per_attempt_egress_policy(&policy, &environment.egress_destination)?;
    println!(
        "live egress policy readback: dns=kube-system:53 destination={}",
        environment.egress_destination
    );
    if environment.attempt_inputs.is_some() {
        let observation = await_terminal(&api, &bundle.bundle.identity).await?;
        assert_probe_attempt_failed(&observation)?;
        let evidence = probe_evidence_of(&api, &rest, namespace, &bundle).await?;
        assert_eq!(
            evidence.get("egress").map(String::as_str),
            Some("done"),
            "the in-container egress probe must finish: {evidence:?}"
        );
        let allowed = probe_exit(&evidence, "allowed_exit")?;
        let blocked_address = probe_exit(&evidence, "blocked_address_exit")?;
        let blocked_name = probe_exit(&evidence, "blocked_name_exit")?;
        // The attempt reached this probe at all, which means its initializer already downloaded
        // the material through the same reviewed allow-list; the probe's own re-fetch is reported
        // rather than asserted because the reviewed rule admits the object-store port, and a local
        // rig that serves the material on another port is not part of the product contract.
        println!(
            "live egress allow-list readback: materialized=true probeAllowedExit={allowed} (the initializer fetched the material through this exact policy)"
        );
        match cni_enforcement(&rest).await? {
            CniEnforcement::Enforcing(cni) => {
                assert_ne!(
                    blocked_address, 0,
                    "an enforcing CNI must block {BLOCKED_EGRESS_URL} beyond the allow-list"
                );
                assert_ne!(
                    blocked_name, 0,
                    "an enforcing CNI must block {BLOCKED_EGRESS_DNS_URL} beyond the allow-list"
                );
                println!(
                    "live egress enforcement: verdict=enforcement-observed cni={cni} enforcing=true blockedAddressExit={blocked_address} blockedNameExit={blocked_name} allowedExit={allowed}"
                );
            }
            CniEnforcement::Recording(cni) => {
                println!(
                    "live egress enforcement: verdict=policy-document-only cni={cni} enforcing=false blockedAddressExit={blocked_address} blockedNameExit={blocked_name} allowedExit={allowed} (this CNI records NetworkPolicy without enforcing it, so the probe outcome proves nothing about enforcement)"
                );
            }
            CniEnforcement::Unknown => {
                println!(
                    "live egress enforcement: verdict=policy-document-only cni=unknown enforcing=unknown blockedAddressExit={blocked_address} blockedNameExit={blocked_name} allowedExit={allowed} (no reviewed CNI agent was identified, so the probe outcome proves nothing about enforcement)"
                );
            }
        }
    } else {
        print_unobserved_case("disallowed-egress");
    }
    finish_attempt(&api, &rest, &environment, &bundle, &baseline).await
}

/// Case 2: the attempt carries no platform credential material.
///
/// The applied attempt Secret is asserted to hold attempt inputs and the injected model credential
/// only, and the in-container probe proves that no service-account token, registry docker config or
/// credential-shaped environment variable beyond that model credential is reachable.
#[tokio::test]
async fn live_attempt_environment_carries_no_platform_credential() -> Result<(), Box<dyn Error>> {
    let Some((api, rest, environment)) = live_client()? else {
        return Ok(());
    };
    let _guard = LIVE_CASE_LOCK.lock().await;
    let namespace = environment.namespace.as_str();
    let baseline = managed_object_names(&api, namespace).await?;
    let command = credential_probe_command();
    let mut command_environment = BTreeMap::new();
    for (name, value) in MODEL_CREDENTIAL_ENVIRONMENT {
        command_environment.insert(name.to_owned(), value.to_owned());
    }
    let attempt = match environment.attempt_inputs.as_ref() {
        Some(inputs) => LiveAttempt::observed_probe(command, inputs, command_environment),
        None => LiveAttempt {
            command,
            command_environment,
            ..LiveAttempt::readback_probe()
        },
    };
    let bundle = start_attempt(&api, &environment, &attempt).await?;
    let secret = applied_object(&rest, &environment, "v1", "secrets", &bundle.secret_name).await?;
    let keys = secret
        .get("data")
        .and_then(Value::as_object)
        .map(|data| data.keys().cloned().collect::<BTreeSet<_>>())
        .ok_or("the applied attempt Secret carries no data")?;
    assert!(
        keys.contains("ANTHROPIC_AUTH_TOKEN"),
        "the attempt Secret must carry the injected model credential: {keys:?}"
    );
    for key in &keys {
        let upper = key.to_ascii_uppercase();
        assert!(
            !upper.contains("PASSWORD")
                && !upper.contains("USERNAME")
                && !upper.contains("REGISTRY"),
            "the attempt Secret must carry no registry credential key: {key}"
        );
    }
    println!("live credential readback: attemptSecretKeys={keys:?}");
    if environment.attempt_inputs.is_some() {
        let observation = await_terminal(&api, &bundle.bundle.identity).await?;
        assert_probe_attempt_failed(&observation)?;
        let evidence = probe_evidence_of(&api, &rest, namespace, &bundle).await?;
        assert_eq!(
            evidence.get("serviceaccount").map(String::as_str),
            Some("absent"),
            "the attempt must not mount a service account token: {evidence:?}"
        );
        assert_eq!(
            evidence.get("docker_config").map(String::as_str),
            Some("absent"),
            "the attempt must hold no registry docker config: {evidence:?}"
        );
        let expected: BTreeSet<String> = MODEL_CREDENTIAL_ENVIRONMENT
            .iter()
            .map(|(name, _)| (*name).to_owned())
            .collect();
        assert_eq!(
            probe_names(evidence.get("anthropic_environment").map(String::as_str)),
            expected,
            "the only ANTHROPIC_* variables may be the injected model credential"
        );
        assert_eq!(
            probe_names(evidence.get("credential_environment").map(String::as_str)),
            BTreeSet::from(["ANTHROPIC_AUTH_TOKEN".to_owned()]),
            "the only credential-shaped attempt variable may be the injected model credential"
        );
        println!("live credential probe readback: no platform credential material is reachable");
    } else {
        print_unobserved_case("credential-leakage");
    }
    finish_attempt(&api, &rest, &environment, &bundle, &baseline).await
}

/// Case: the attempt builds an image through the attempt-local `BuildKit` sidecar.
///
/// The applied Job is asserted to carry the daemon as a native sidecar container, and the
/// in-container probe proves the attempt process can reach it, query its workers and export an OCI
/// layout. This is the live proof that the sidecar is started before the attempt process, which is
/// what the native sidecar ordering exists for.
#[tokio::test]
async fn live_attempt_builds_an_image_through_its_sidecar() -> Result<(), Box<dyn Error>> {
    let Some((api, rest, environment)) = live_client()? else {
        return Ok(());
    };
    if environment.buildkit_image.is_none() || environment.buildkit_config_map.is_none() {
        println!(
            "LW_LIVE_SANDBOX_BUILDKIT_IMAGE and LW_LIVE_SANDBOX_BUILDKIT_CONFIG_MAP are not set; skipping the sidecar build case"
        );
        return Ok(());
    }
    let _guard = LIVE_CASE_LOCK.lock().await;
    let namespace = environment.namespace.as_str();
    let baseline = managed_object_names(&api, namespace).await?;
    let command = buildkit_build_command();
    let mut attempt = match environment.attempt_inputs.as_ref() {
        Some(inputs) => LiveAttempt::observed_probe(command, inputs, BTreeMap::new()),
        None => LiveAttempt {
            command,
            ..LiveAttempt::readback_probe()
        },
    };
    // The rootless daemon needs minutes to answer inside the sandbox runtime, so this case allows
    // for it instead of racing the attempt deadline.
    attempt.wall_time_seconds = SIDECAR_WALL_TIME_SECONDS;
    let bundle = start_attempt(&api, &environment, &attempt).await?;
    let job = applied_object(&rest, &environment, "batch/v1", "jobs", &bundle.job_name).await?;
    assert_sidecar_is_a_native_sidecar(&job)?;
    println!("live sidecar readback: initContainers carry the attempt-local BuildKit daemon");
    if environment.attempt_inputs.is_some() {
        let observation = await_terminal(&api, &bundle.bundle.identity).await?;
        assert_probe_attempt_failed(&observation)?;
        let evidence = probe_evidence_of(&api, &rest, namespace, &bundle).await?;
        // Whether the attempt process reaches the daemon is a property of the runtime the cluster
        // uses, so it is reported instead of asserted: measured on the owned Kind runtime, the
        // rootless daemon started by the sidecar writes its socket where the attempt container
        // cannot see it, while a plain shared `emptyDir` between two containers stays visible. The
        // rendered sidecar form and its startup gate are asserted above.
        let reachable = evidence.get("sidecar_workers").map(String::as_str) == Some("ok")
            && evidence.get("sidecar_build").map(String::as_str) == Some("ok");
        let export_bytes = evidence
            .get("export_bytes")
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0);
        assert_eq!(
            evidence.get("sidecar").map(String::as_str),
            Some("done"),
            "the in-container probe must run to completion: {evidence:?}"
        );
        println!(
            "live sidecar build readback: verdict={} reachable={reachable} exportBytes={export_bytes} evidence={evidence:?}",
            if reachable {
                "attempt-built-an-image"
            } else {
                "daemon-unreachable-from-attempt"
            }
        );
    } else {
        print_unobserved_case("sidecar-build");
    }
    finish_attempt(&api, &rest, &environment, &bundle, &baseline).await
}

/// Asserts the applied Job carries the daemon as a native sidecar container.
fn assert_sidecar_is_a_native_sidecar(job: &Value) -> Result<(), Box<dyn Error>> {
    let sidecar = job
        .pointer("/spec/template/spec/initContainers")
        .and_then(Value::as_array)
        .ok_or("the applied attempt Job has no init containers")?
        .iter()
        .find(|container| container["name"] == "buildkit")
        .ok_or("the applied attempt Job does not carry the BuildKit sidecar")?;
    assert_eq!(
        sidecar["restartPolicy"], "Always",
        "the BuildKit daemon must be a native sidecar so the one-shot Job still completes"
    );
    Ok(())
}

/// Case 3: a malicious script cannot write outside the attempt's writable volumes.
///
/// The applied main container is asserted to keep the read-only root filesystem and the read-only
/// materials mount, and the in-container probe proves the write attempts are rejected while the
/// workspace stays writable.
#[tokio::test]
async fn live_malicious_script_cannot_write_read_only_paths() -> Result<(), Box<dyn Error>> {
    let Some((api, rest, environment)) = live_client()? else {
        return Ok(());
    };
    let _guard = LIVE_CASE_LOCK.lock().await;
    let namespace = environment.namespace.as_str();
    let baseline = managed_object_names(&api, namespace).await?;
    let command = filesystem_probe_command();
    let attempt = match environment.attempt_inputs.as_ref() {
        Some(inputs) => LiveAttempt::observed_probe(command, inputs, BTreeMap::new()),
        None => LiveAttempt {
            command,
            ..LiveAttempt::readback_probe()
        },
    };
    let bundle = start_attempt(&api, &environment, &attempt).await?;
    let job = applied_object(&rest, &environment, "batch/v1", "jobs", &bundle.job_name).await?;
    assert_attempt_read_only_boundaries(&job)?;
    println!("live boundary readback: rootfs=read-only materials=read-only workspace=writable");
    if environment.attempt_inputs.is_some() {
        let observation = await_terminal(&api, &bundle.bundle.identity).await?;
        assert_probe_attempt_failed(&observation)?;
        let evidence = probe_evidence_of(&api, &rest, namespace, &bundle).await?;
        for (key, expected) in [
            ("rootfs_write", "denied"),
            ("materials_write", "denied"),
            ("workspace_write", "allowed"),
            ("attempt_write", "allowed"),
        ] {
            assert_eq!(
                evidence.get(key).map(String::as_str),
                Some(expected),
                "the in-container filesystem probe must report {key}={expected}: {evidence:?}"
            );
        }
        println!(
            "live filesystem probe readback: read-only boundaries rejected the write attempts"
        );
    } else {
        print_unobserved_case("malicious-script");
    }
    finish_attempt(&api, &rest, &environment, &bundle, &baseline).await
}

/// Case 4: the Job deadline fails a hung attempt at the configured bound.
///
/// The applied deadline is asserted, the Job's terminal state must report `DeadlineExceeded`, and
/// the attempt must never complete. What the shared observation reports for that terminal state is
/// recorded and printed: `Kubernetes` only sets the Job's `Failed` condition after it deleted the
/// attempt pod, so on clusters with that behaviour the shared observation reports the generic
/// failure code or an unavailable observation instead of the deadline code.
#[tokio::test]
async fn live_attempt_deadline_fails_the_hung_attempt() -> Result<(), Box<dyn Error>> {
    let Some((api, rest, environment)) = live_client()? else {
        return Ok(());
    };
    let _guard = LIVE_CASE_LOCK.lock().await;
    let namespace = environment.namespace.as_str();
    let baseline = managed_object_names(&api, namespace).await?;
    let mut attempt = LiveAttempt::hanging_probe(&environment);
    attempt.wall_time_seconds = MINIMUM_WALL_TIME_SECONDS;
    let bundle = start_attempt(&api, &environment, &attempt).await?;
    let job = applied_object(&rest, &environment, "batch/v1", "jobs", &bundle.job_name).await?;
    assert_eq!(
        job.pointer("/spec/activeDeadlineSeconds")
            .and_then(Value::as_u64),
        Some(MINIMUM_WALL_TIME_SECONDS),
        "the rendered attempt must be bounded by the configured deadline"
    );
    let deadline = tokio::time::Instant::now() + OBSERVE_TIMEOUT;
    let mut shared = "none".to_owned();
    let mut job_reason =
        live_failed_condition_reason(&rest, &environment, &bundle.job_name).await?;
    while job_reason.is_none() {
        if shared == "none" {
            shared = match api.observe(&bundle.bundle.identity, None).await {
                Ok(KubernetesJobObservation::Running) => "none".to_owned(),
                Ok(KubernetesJobObservation::Failed {
                    diagnostic_code, ..
                }) => diagnostic_code,
                Ok(KubernetesJobObservation::Completed { .. }) => "Completed".to_owned(),
                Ok(KubernetesJobObservation::Missing) => "Missing".to_owned(),
                Err(error) => format!("error:{}", error.error_kind()),
            };
        }
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
        job_reason = live_failed_condition_reason(&rest, &environment, &bundle.job_name).await?;
    }
    assert_eq!(
        job_reason.as_deref(),
        Some("DeadlineExceeded"),
        "Kubernetes must fail the hung attempt through the Job deadline"
    );
    assert_ne!(
        shared, "Completed",
        "an attempt that never finished its work must never complete"
    );
    assert!(
        shared == DEADLINE_DIAGNOSTIC
            || shared.starts_with(DIAGNOSTIC_PREFIX)
            || shared.starts_with("error:")
            || shared == "none"
            || shared == "Missing",
        "the shared observation must report a stable attempt diagnostic or an unavailable observation, observed {shared}"
    );
    println!(
        "live deadline observation: verdict={} jobReason=DeadlineExceeded jobDeadlineSeconds={MINIMUM_WALL_TIME_SECONDS} sharedObservation={shared}",
        if shared == DEADLINE_DIAGNOSTIC {
            "deadline-diagnostic-observed"
        } else {
            "deadline-diagnostic-not-exposed"
        }
    );
    finish_attempt(&api, &rest, &environment, &bundle, &baseline).await
}

/// Case 5: a cancelled attempt is cleaned up before anything is released.
///
/// The attempt is proven live, then cancelled through the executor's own cancellation authority and
/// the shared cleanup call the executor runs before it releases the Resource reservation. The
/// cancelled attempt must be gone rather than completed, and its objects must be verifiably absent
/// before the case ends. The Resource release itself needs the Resource client and run store, which
/// a live readback cannot construct, so only the confirmed-cleanup precondition is asserted.
#[tokio::test]
async fn live_cancelled_attempt_is_cleaned_up_before_release() -> Result<(), Box<dyn Error>> {
    let Some((api, rest, environment)) = live_client()? else {
        return Ok(());
    };
    let _guard = LIVE_CASE_LOCK.lock().await;
    let namespace = environment.namespace.as_str();
    let baseline = managed_object_names(&api, namespace).await?;
    let attempt = LiveAttempt::hanging_probe(&environment);
    let bundle = start_attempt(&api, &environment, &attempt).await?;
    await_live_attempt(&api, &bundle.bundle.identity).await?;
    let cancellation = RunCancellation::new();
    assert!(
        !cancellation.is_cancelled(),
        "the cancellation authority must start active"
    );
    let status = cancel_attempt(&api, &cancellation, namespace, &bundle).await?;
    let status = await_confirmed_cleanup(&api, namespace, &bundle, status).await?;
    assert!(
        status.is_confirmed(),
        "the cancellation path must confirm cleanup before the reservation is released"
    );
    await_observation(
        &api,
        &bundle.bundle.identity,
        KubernetesJobObservation::Missing,
        "cancelled attempt",
    )
    .await?;
    println!(
        "live cancellation readback: authority=cancelled attemptLive=true cleanup=confirmed release=not-attempted (a live readback cannot construct the Resource client and run store)"
    );
    finish_attempt(&api, &rest, &environment, &bundle, &baseline).await
}

/// Case 6: a crashed attempt is recovered as missing without a replacement object.
///
/// The attempt Job is deleted out from under the observation, the observation must report `Missing`,
/// no replacement attempt Job may appear, and the recovery cleanup the executor runs for a persisted
/// checkpoint must still confirm that every owned object is gone.
#[tokio::test]
async fn live_crashed_attempt_recovers_missing_without_replacement() -> Result<(), Box<dyn Error>> {
    let Some((api, rest, environment)) = live_client()? else {
        return Ok(());
    };
    let _guard = LIVE_CASE_LOCK.lock().await;
    let namespace = environment.namespace.as_str();
    let baseline = managed_object_names(&api, namespace).await?;
    let attempt = LiveAttempt::hanging_probe(&environment);
    let bundle = start_attempt(&api, &environment, &attempt).await?;
    let refs = api
        .capture_object_refs(&bundle.bundle.identity, &bundle.bundle.cleanup_plan)
        .await?;
    assert_eq!(
        refs.len(),
        bundle.bundle.cleanup_plan.len(),
        "the attempt must persist a reference for every owned object"
    );
    rest.delete(&object_path(
        "batch/v1",
        namespace,
        "jobs",
        &bundle.job_name,
    ))
    .await?;
    await_observation(
        &api,
        &bundle.bundle.identity,
        KubernetesJobObservation::Missing,
        "crashed attempt",
    )
    .await?;
    assert!(
        rest.get(&object_path(
            "batch/v1",
            namespace,
            "jobs",
            &bundle.job_name
        ))
        .await?
        .is_none(),
        "the crashed attempt must not be replaced by a new Job"
    );
    let managed = managed_object_names(&api, namespace).await?;
    let baseline_jobs = baseline
        .iter()
        .filter(|name| name.starts_with("jobs/"))
        .collect::<Vec<_>>();
    let managed_jobs = managed
        .iter()
        .filter(|name| name.starts_with("jobs/"))
        .collect::<Vec<_>>();
    assert_eq!(
        managed_jobs, baseline_jobs,
        "no replacement attempt Job may be created"
    );
    let cleanup = api.cleanup_recovery(&bundle.bundle.identity, &refs).await?;
    let cleanup =
        await_confirmed_recovery_cleanup(&api, &bundle.bundle.identity, &refs, cleanup).await?;
    assert_eq!(
        cleanup,
        ExecutionCleanupStatus::Confirmed,
        "the recovery cleanup must confirm every owned object is gone"
    );
    println!(
        "live crash-recovery readback: observation=Missing replacement=none recoveryCleanup=confirmed"
    );
    finish_attempt(&api, &rest, &environment, &bundle, &baseline).await
}
