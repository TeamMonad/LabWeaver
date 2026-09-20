//! Opt-in live `Kubernetes` readback for the Agent authoring sandbox bundle.
#![allow(
    clippy::too_many_lines,
    reason = "one live acceptance flow keeps the applied security context, the default-deny admission gate, deterministic cleanup and the terminal observation auditable together"
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

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    error::Error,
    fs,
    path::PathBuf,
    time::Duration,
};

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

/// Live connection, image and namespace inputs for one real authoring bundle.
struct LiveEnvironment {
    configuration: KubernetesApiConfiguration,
    namespace: String,
    image: String,
    egress_cidr: String,
    buildkit_image: Option<String>,
    buildkit_config_map: Option<String>,
    pull_secret: Option<String>,
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
        egress_cidr: required("LW_LIVE_SANDBOX_EGRESS_CIDR")?,
        buildkit_image,
        buildkit_config_map,
        pull_secret,
    }))
}

/// Renders one real attempt bundle for the live namespace and image.
fn live_bundle(environment: &LiveEnvironment) -> Result<SandboxBundle, Box<dyn Error>> {
    let task_run_id = Uuid::now_v7();
    let mut allowed_egress_cidrs = BTreeSet::new();
    allowed_egress_cidrs.insert(environment.egress_cidr.clone());
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
        wall_time_seconds: ATTEMPT_WALL_TIME_SECONDS,
        allowed_egress_cidrs,
        buildkit_image: environment.buildkit_image.clone(),
        buildkit_config_map_name: environment.buildkit_config_map.clone(),
    };
    let spec = SandboxAttemptSpec {
        task_run_id,
        ownership,
        trace_id: format!("live-{task_run_id}"),
        command: vec![
            "claude".to_owned(),
            "--print".to_owned(),
            "labweaver-live-probe".to_owned(),
        ],
        expected_claude_version: "0.0.0-labweaver-live".to_owned(),
        command_environment: BTreeMap::new(),
        material_download_url: MATERIAL_DOWNLOAD_URL.to_owned(),
        material_sha256: Sha256Digest::of_bytes(b"labweaver-live-material").to_string(),
        material_size_bytes: 16,
        result_upload_url: RESULT_UPLOAD_URL.to_owned(),
        result_upload_headers: BTreeMap::new(),
        stderr_upload_url: STDERR_UPLOAD_URL.to_owned(),
        stderr_upload_headers: BTreeMap::new(),
        result_max_bytes: 4_096,
        stderr_max_bytes: 4_096,
        export_upload_url: None,
        export_upload_headers: BTreeMap::new(),
        object_store_ca_base64: None,
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
    let Ok(sidecar) = container(pod, SANDBOX_BUILDKIT_CONTAINER) else {
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

/// Asserts the applied rootless `BuildKit` sidecar exceptions of one live Job.
fn assert_buildkit_sidecar(job: &Value) -> Result<(), Box<dyn Error>> {
    let pod = pod_template(job)?;
    let sidecar = container(pod, SANDBOX_BUILDKIT_CONTAINER)?;
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
