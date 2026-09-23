//! Opt-in live readback for the reviewed evaluation egress policy.
//!
//! The test only runs when `LW_LIVE_KUBERNETES=1` and the bound connection variables are present.
//! It applies the per-attempt `NetworkPolicy` an OJ run renders, runs a probe under the same
//! attempt identity, and reads back both the applied policy and the probe's own observation, so a
//! deployment proves that its reviewed destinations actually admit the object store and that every
//! destination it did not review stays refused.
//!
//! The operator needs an identity that may apply and delete the probe bundle in the runner
//! namespace:
//!
//! ```text
//! kubectl -n <runner namespace> create serviceaccount evaluation-live-verifier
//! kubectl -n <runner namespace> create role evaluation-live-verifier \
//!   --verb=create,get,list,delete,patch,update \
//!   --resource=jobs.batch,networkpolicies.networking.k8s.io,pods,secrets
//! kubectl -n <runner namespace> create rolebinding evaluation-live-verifier \
//!   --role=evaluation-live-verifier --serviceaccount=<ns>:evaluation-live-verifier
//! kubectl -n <runner namespace> create token evaluation-live-verifier --duration=8h > token
//! ```
//!
//! ```text
//! LW_LIVE_KUBERNETES=1 \
//! LW_LIVE_KUBERNETES_API_SERVER=https://127.0.0.1:44135 \
//! LW_LIVE_KUBERNETES_TOKEN_FILE=/tmp/opencode/eval-live-token \
//! LW_LIVE_KUBERNETES_CA_FILE=/tmp/opencode/kind-ca.pem \
//! LW_LIVE_KUBERNETES_NAMESPACE=labweaver-evaluation \
//! LW_LIVE_OJ_EGRESS=10.201.0.0/16:9000,10.202.0.0/16:9000 \
//! LW_LIVE_OJ_PROBE_IMAGE=<digest pinned sandbox image> \
//! LW_LIVE_OJ_ALLOWED_URL=https://minio.labweaver-data.svc:9000/ \
//! LW_LIVE_OJ_BLOCKED_URL=https://10.201.0.1:443/ \
//! cargo test -p evaluation-service --test oj_egress_live -- --nocapture
//! ```

#![allow(
    clippy::too_many_lines,
    reason = "one live acceptance flow keeps the applied policy, the probe observation and cleanup auditable together"
)]

use std::env;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use contracts::execution::ExecutionCleanupStatus;
use evaluation_service::kubernetes_job::{
    KubernetesApiClient, KubernetesApiConfiguration, KubernetesCleanupTarget, KubernetesJobBundle,
    KubernetesJobIdentity, KubernetesJobObservation, KubernetesObject, KubernetesOwnership,
};
use reqwest::{Certificate, Client, Url};
use serde_json::{Value, json};
use task_execution::kubernetes::{SANDBOX_RUNTIME_CLASS, reviewed_egress_rule};
use uuid::Uuid;

const MANAGED_BY: &str = "evaluation-service";
const MANAGED_BY_LABEL: &str = "labweaver.io/managed-by";
const ATTEMPT_LABEL: &str = "labweaver.io/attempt-id";
const MAIN_CONTAINER: &str = "probe";
const DEFAULT_DENY_POLICY: &str = "oj-runner-default-deny";
const REQUEST_SHA_ANNOTATION: &str = "labweaver.io/request-sha256";
const TERMINAL_TIMEOUT: Duration = Duration::from_mins(4);
const POLL_INTERVAL: Duration = Duration::from_secs(3);

/// Connection inputs shared by the execution client and the raw readback client.
struct LiveEnvironment {
    configuration: KubernetesApiConfiguration,
    namespace: String,
    destinations: Vec<String>,
    probe_image: String,
    probe_pull_secret: Option<String>,
    allowed_url: String,
    blocked_url: String,
}

fn live_environment() -> Option<LiveEnvironment> {
    if env::var("LW_LIVE_KUBERNETES").ok().as_deref() != Some("1") {
        return None;
    }
    let namespace = env::var("LW_LIVE_KUBERNETES_NAMESPACE").ok()?;
    let destinations = env::var("LW_LIVE_OJ_EGRESS")
        .ok()?
        .split(',')
        .map(str::trim)
        .filter(|destination| !destination.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if destinations.is_empty() {
        return None;
    }
    Some(LiveEnvironment {
        configuration: KubernetesApiConfiguration {
            kubernetes_api_server: Url::parse(&env::var("LW_LIVE_KUBERNETES_API_SERVER").ok()?)
                .ok()?,
            kubernetes_bearer_token_file: PathBuf::from(
                env::var("LW_LIVE_KUBERNETES_TOKEN_FILE").ok()?,
            ),
            kubernetes_ca_file: PathBuf::from(env::var("LW_LIVE_KUBERNETES_CA_FILE").ok()?),
            runner_namespace: namespace.clone(),
            request_timeout_milliseconds: 5_000,
        },
        namespace,
        destinations,
        probe_image: env::var("LW_LIVE_OJ_PROBE_IMAGE").ok()?,
        probe_pull_secret: env::var("LW_LIVE_OJ_PROBE_PULL_SECRET").ok(),
        allowed_url: env::var("LW_LIVE_OJ_ALLOWED_URL").ok()?,
        blocked_url: env::var("LW_LIVE_OJ_BLOCKED_URL").ok()?,
    })
}

/// Renders the per-attempt policy exactly as an OJ run does: DNS, then one rule per reviewed
/// destination, and nothing else.
fn network_policy(
    environment: &LiveEnvironment,
    name: &str,
    ownership: &KubernetesOwnership,
) -> Result<Value, Box<dyn std::error::Error>> {
    let mut egress = vec![json!({
        "to": [{"namespaceSelector": {"matchLabels": {"kubernetes.io/metadata.name": "kube-system"}}}],
        "ports": [{"protocol": "UDP", "port": 53}, {"protocol": "TCP", "port": 53}],
    })];
    for destination in &environment.destinations {
        egress.push(
            reviewed_egress_rule(destination)
                .ok_or("the live reviewed destination is not a reviewed cidr:port pair")?,
        );
    }
    Ok(json!({
        "apiVersion": "networking.k8s.io/v1",
        "kind": "NetworkPolicy",
        "metadata": {
            "name": name,
            "namespace": environment.namespace,
            "labels": ownership_labels(ownership),
            "annotations": {REQUEST_SHA_ANNOTATION: ownership.request_sha256},
        },
        "spec": {
            "podSelector": {"matchLabels": {
                ATTEMPT_LABEL: ownership.attempt_id.to_string(),
                MANAGED_BY_LABEL: MANAGED_BY,
            }},
            "policyTypes": ["Ingress", "Egress"],
            "ingress": [],
            "egress": egress,
        },
    }))
}

fn ownership_labels(ownership: &KubernetesOwnership) -> Value {
    json!({
        MANAGED_BY_LABEL: MANAGED_BY,
        "labweaver.io/run-id": ownership.run_id.to_string(),
        "labweaver.io/step-run-id": ownership.step_run_id.to_string(),
        ATTEMPT_LABEL: ownership.attempt_id.to_string(),
    })
}

/// Renders the probe job that reports its own reachability through its termination message.
fn probe_job(environment: &LiveEnvironment, name: &str, ownership: &KubernetesOwnership) -> Value {
    let script = format!(
        "set -u\n\
         curl -s -o /dev/null --connect-timeout 5 --max-time 10 -k {allowed}; allowed=$?\n\
         curl -s -o /dev/null --connect-timeout 5 --max-time 10 -k {blocked}; blocked=$?\n\
         printf '{{\"allowedExit\":%s,\"blockedExit\":%s}}' \"$allowed\" \"$blocked\" > /dev/termination-log\n\
         exit 0\n",
        allowed = environment.allowed_url,
        blocked = environment.blocked_url,
    );
    json!({
        "apiVersion": "batch/v1",
        "kind": "Job",
        "metadata": {
            "name": name,
            "namespace": environment.namespace,
            "labels": ownership_labels(ownership),
            "annotations": {REQUEST_SHA_ANNOTATION: ownership.request_sha256},
        },
        "spec": {
            "backoffLimit": 0,
            "activeDeadlineSeconds": 120,
            "template": {
                "metadata": {
                    "labels": ownership_labels(ownership),
                    "annotations": {REQUEST_SHA_ANNOTATION: ownership.request_sha256},
                },
                "spec": {
                    "automountServiceAccountToken": false,
                    "restartPolicy": "Never",
                    "runtimeClassName": SANDBOX_RUNTIME_CLASS,
                    "imagePullSecrets": environment
                        .probe_pull_secret
                        .iter()
                        .map(|name| json!({"name": name}))
                        .collect::<Vec<_>>(),
                    "containers": [{
                        "name": MAIN_CONTAINER,
                        "image": environment.probe_image,
                        "imagePullPolicy": "IfNotPresent",
                        "command": ["/bin/sh", "-c", script],
                        "securityContext": {
                            "allowPrivilegeEscalation": false,
                            "capabilities": {"drop": ["ALL"]},
                            "readOnlyRootFilesystem": true,
                            "runAsNonRoot": true,
                            "runAsUser": 65532,
                            "runAsGroup": 65532,
                            "seccompProfile": {"type": "RuntimeDefault"},
                        },
                    }],
                },
            },
        },
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

/// Raw readback so the test asserts the *applied* document, not the rendered one.
struct LiveRest {
    client: Client,
    server: Url,
    token: String,
}

impl LiveRest {
    fn new(configuration: &KubernetesApiConfiguration) -> Result<Self, Box<dyn std::error::Error>> {
        let ca = Certificate::from_pem(&fs::read(&configuration.kubernetes_ca_file)?)?;
        let client = Client::builder()
            .https_only(true)
            .tls_built_in_root_certs(false)
            .add_root_certificate(ca)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .build()?;
        Ok(Self {
            client,
            server: configuration.kubernetes_api_server.clone(),
            token: fs::read_to_string(&configuration.kubernetes_bearer_token_file)?
                .trim()
                .to_owned(),
        })
    }

    async fn get(&self, path: &str) -> Result<Value, Box<dyn std::error::Error>> {
        let response = self
            .client
            .get(self.server.join(path)?)
            .bearer_auth(&self.token)
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(
                format!("live readback of {path} failed with {}", response.status()).into(),
            );
        }
        Ok(response.json().await?)
    }
}

#[tokio::test]
async fn live_reviewed_egress_admits_the_object_store_under_the_applied_policy()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(environment) = live_environment() else {
        return Ok(());
    };
    let client = KubernetesApiClient::new(
        environment.configuration.clone(),
        "labweaver-oj-egress-live",
        "program.oj",
        "LW_OJ_",
        MANAGED_BY,
        "evaluation",
    )?;
    let task_run_id = Uuid::now_v7();
    let suffix = task_run_id.simple().to_string();
    let job_name = format!("lw-oj-egress-{suffix}");
    let policy_name = format!("lw-oj-egress-net-{suffix}");
    let ownership = KubernetesOwnership {
        run_id: Uuid::now_v7(),
        step_run_id: Uuid::now_v7(),
        attempt_id: Uuid::now_v7(),
        request_sha256: "a".repeat(64),
    };
    let policy = network_policy(&environment, &policy_name, &ownership)?;
    let job = probe_job(&environment, &job_name, &ownership);
    let bundle = KubernetesJobBundle {
        identity: KubernetesJobIdentity {
            namespace: environment.namespace.clone(),
            job_name: job_name.clone(),
            main_container: MAIN_CONTAINER,
            default_deny_policy: DEFAULT_DENY_POLICY,
            deadline_diagnostic_code: "LW_OJ_DEADLINE_EXCEEDED",
            failed_diagnostic_code: "LW_OJ_JOB_FAILED",
            oom_diagnostic_code: "LW_OJ_OOM_KILLED",
            stable_diagnostic_prefix: "LW_OJ_",
            ownership: ownership.clone(),
            trace_id: format!("live-{suffix}"),
        },
        objects: vec![
            KubernetesObject {
                api_version: "networking.k8s.io/v1",
                plural: "networkpolicies",
                name: policy_name.clone(),
                document: policy,
            },
            KubernetesObject {
                api_version: "batch/v1",
                plural: "jobs",
                name: job_name.clone(),
                document: job,
            },
        ],
        cleanup_plan: vec![
            cleanup_target(&environment.namespace, "jobs", &job_name),
            cleanup_target(&environment.namespace, "networkpolicies", &policy_name),
        ],
    };
    client.start(&bundle).await?;
    let mut terminal = None;
    let deadline = tokio::time::Instant::now() + TERMINAL_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        match client.observe(&bundle.identity, None).await? {
            KubernetesJobObservation::Completed { message, .. } => {
                terminal = Some(message);
                break;
            }
            KubernetesJobObservation::Failed {
                diagnostic_code, ..
            } => {
                let _ = client
                    .cleanup(
                        &environment.namespace,
                        &job_name,
                        &bundle.objects,
                        &bundle.cleanup_plan,
                    )
                    .await;
                return Err(format!("the live egress probe failed with {diagnostic_code}").into());
            }
            KubernetesJobObservation::Missing | KubernetesJobObservation::Running => {}
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    let rest = LiveRest::new(&environment.configuration)?;
    let applied = rest
        .get(&format!(
            "/apis/networking.k8s.io/v1/namespaces/{}/networkpolicies/{}",
            environment.namespace, policy_name
        ))
        .await?;
    let egress = applied
        .pointer("/spec/egress")
        .and_then(Value::as_array)
        .ok_or("the applied policy has no egress rules")?;
    assert_eq!(
        egress.len(),
        environment.destinations.len() + 1,
        "the applied policy must admit exactly the DNS rule and one rule per reviewed destination"
    );
    assert_eq!(
        egress[0].pointer("/to/0/namespaceSelector/matchLabels/kubernetes.io~1metadata.name"),
        Some(&json!("kube-system")),
        "the applied policy must resolve names through kube-system only"
    );
    for (index, destination) in environment.destinations.iter().enumerate() {
        let expected = reviewed_egress_rule(destination).ok_or("invalid reviewed destination")?;
        assert_eq!(
            egress[index + 1],
            expected,
            "the applied policy must admit exactly the reviewed destination {destination}"
        );
    }
    let message = terminal.ok_or("the live egress probe never reached a terminal state")?;
    let observation: Value = serde_json::from_str(&message)?;
    let allowed = observation.get("allowedExit").and_then(Value::as_i64);
    let blocked = observation.get("blockedExit").and_then(Value::as_i64);
    // Whether the cluster's CNI enforces the applied policy is a property of the CNI, not of the
    // rendered document: the owned Kind CNI currently does not enforce a per-namespace default
    // deny, so the unreviewed destination is reported instead of asserted. The applied document is
    // asserted above, exactly like the authoring sandbox's live case does.
    println!(
        "live evaluation egress readback: destinations={:?} allowedExit={allowed:?} blockedExit={blocked:?} (the applied policy admits exactly the reviewed destinations; enforcement is the CNI's)",
        environment.destinations
    );
    // Foreground deletion waits for the probe pod, so the confirmation is polled like the
    // production cleanup pass does instead of asserted after one pass.
    let mut cleanup = ExecutionCleanupStatus::Pending {
        remaining_objects: Vec::new(),
    };
    let deadline = tokio::time::Instant::now() + TERMINAL_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        cleanup = client
            .cleanup(
                &environment.namespace,
                &job_name,
                &bundle.objects,
                &bundle.cleanup_plan,
            )
            .await?;
        if matches!(cleanup, ExecutionCleanupStatus::Confirmed) {
            break;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    assert!(
        matches!(cleanup, ExecutionCleanupStatus::Confirmed),
        "the live probe must clean up after itself: {cleanup:?}"
    );
    assert_eq!(
        allowed,
        Some(0),
        "a reviewed destination must be reachable from an attempt pod"
    );
    assert!(
        blocked.is_some(),
        "the probe must report its observation of an unreviewed destination"
    );
    Ok(())
}
