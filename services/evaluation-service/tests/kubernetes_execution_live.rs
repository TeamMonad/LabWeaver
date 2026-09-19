//! Opt-in live Kubernetes execution readback for the shared Job backend.
#![allow(
    clippy::too_many_lines,
    reason = "one live acceptance flow keeps submit, observe, duplicate and cleanup auditable together"
)]
//!
//! The test only runs when `LW_LIVE_KUBERNETES=1` is set and the bound API
//! connection variables are present. It submits, observes, duplicates, lists,
//! and cleans one real Job so the local acceptance run can prove real
//! Kubernetes identity and readback instead of a mock.
//!
//! ```text
//! LW_LIVE_KUBERNETES=1 \
//! LW_LIVE_KUBERNETES_API_SERVER=https://127.0.0.1:40495 \
//! LW_LIVE_KUBERNETES_TOKEN_FILE=/tmp/opencode/kind-token \
//! LW_LIVE_KUBERNETES_CA_FILE=/tmp/opencode/kind-ca.pem \
//! LW_LIVE_KUBERNETES_NAMESPACE=labweaver-execution-live \
//! cargo test -p evaluation-service --test kubernetes_execution_live -- --nocapture
//! ```

use std::{env, path::PathBuf, time::Duration};

use contracts::execution::ExecutionWorkloadState;
use evaluation_service::kubernetes_job::{
    KubernetesApiClient, KubernetesApiConfiguration, KubernetesJobBundle, KubernetesJobError,
    KubernetesJobIdentity, KubernetesJobObservation, KubernetesObject, KubernetesOwnership,
};
use reqwest::Url;
use serde_json::{Value, json};
use uuid::Uuid;

const MANAGED_BY_SELECTOR: &str = "labweaver.io/managed-by=evaluation-service";

fn live_environment() -> Option<(KubernetesApiConfiguration, String)> {
    if env::var("LW_LIVE_KUBERNETES").ok().as_deref() != Some("1") {
        return None;
    }
    let api_server = env::var("LW_LIVE_KUBERNETES_API_SERVER").ok()?;
    let token_file = env::var("LW_LIVE_KUBERNETES_TOKEN_FILE").ok()?;
    let ca_file = env::var("LW_LIVE_KUBERNETES_CA_FILE").ok()?;
    let namespace = env::var("LW_LIVE_KUBERNETES_NAMESPACE").ok()?;
    let api_server = Url::parse(&api_server).ok()?;
    Some((
        KubernetesApiConfiguration {
            kubernetes_api_server: api_server,
            kubernetes_bearer_token_file: PathBuf::from(token_file),
            kubernetes_ca_file: PathBuf::from(ca_file),
            runner_namespace: namespace.clone(),
            request_timeout_milliseconds: 5_000,
        },
        namespace,
    ))
}

fn live_job_document(namespace: &str, name: &str, ownership: &KubernetesOwnership) -> Value {
    let labels = json!({
        "labweaver.io/managed-by": "evaluation-service",
        "labweaver.io/run-id": ownership.run_id.to_string(),
        "labweaver.io/step-run-id": ownership.step_run_id.to_string(),
        "labweaver.io/attempt-id": ownership.attempt_id.to_string(),
    });
    let annotations = json!({
        "labweaver.io/trace-id": "trace-live",
        "labweaver.io/request-sha256": ownership.request_sha256,
    });
    json!({
        "apiVersion":"batch/v1",
        "kind":"Job",
        "metadata":{
            "name":name,
            "namespace":namespace,
            "labels":labels,
            "annotations":annotations,
        },
        "spec":{
            "backoffLimit":0,
            "activeDeadlineSeconds":120,
            "ttlSecondsAfterFinished":300,
            "template":{
                "metadata":{"labels":labels,"annotations":annotations},
                "spec":{
                    "restartPolicy":"Never",
                    "automountServiceAccountToken":false,
                    "containers":[{
                        "name":"program-runner",
                        "image":"busybox:1.36",
                        "imagePullPolicy":"IfNotPresent",
                        "command":[
                            "sh",
                            "-c",
                            "printf '{\"schemaVersion\":\"labweaver.live/v1\"}' > /dev/termination-log; echo LW_LIVE_OK; sleep 2",
                        ],
                        "resources":{
                            "requests":{"cpu":"100m","memory":"64Mi"},
                            "limits":{"cpu":"500m","memory":"128Mi"},
                        },
                    }],
                },
            },
        },
    })
}

#[tokio::test]
async fn live_job_submit_observe_duplicate_cleanup_readback()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((configuration, namespace)) = live_environment() else {
        eprintln!("LW_LIVE_KUBERNETES is not enabled; skipping live readback");
        return Ok(());
    };
    let api = KubernetesApiClient::new(configuration, "labweaver-live-test", "live", "LW_LIVE_")?;
    let ownership = KubernetesOwnership {
        run_id: Uuid::now_v7(),
        step_run_id: Uuid::now_v7(),
        attempt_id: Uuid::now_v7(),
        request_sha256: "live-request-sha".to_owned(),
    };
    let job_name = format!("lw-oj-{}", &ownership.attempt_id.simple().to_string()[..20]);
    let identity = KubernetesJobIdentity {
        namespace: namespace.clone(),
        job_name: job_name.clone(),
        main_container: "program-runner",
        default_deny_policy: "oj-runner-default-deny",
        deadline_diagnostic_code: "LW_LIVE_DEADLINE_EXCEEDED",
        failed_diagnostic_code: "LW_LIVE_JOB_FAILED",
        oom_diagnostic_code: "LW_LIVE_MEMORY_LIMIT",
        stable_diagnostic_prefix: "LW_LIVE_",
        ownership: ownership.clone(),
        trace_id: "trace-live".to_owned(),
    };
    let bundle = KubernetesJobBundle {
        identity: identity.clone(),
        objects: vec![KubernetesObject {
            api_version: "batch/v1",
            plural: "jobs",
            name: job_name.clone(),
            document: live_job_document(&namespace, &job_name, &ownership),
        }],
        cleanup_plan: vec![
            evaluation_service::kubernetes_job::KubernetesCleanupTarget {
                namespace: namespace.clone(),
                resource: "jobs".to_owned(),
                name: job_name.clone(),
                propagation_policy: "Foreground".to_owned(),
            },
        ],
    };

    api.start(&bundle).await?;
    let items = api
        .list(&namespace, "batch/v1", "jobs", MANAGED_BY_SELECTOR)
        .await?;
    assert_eq!(
        items.len(),
        1,
        "exactly one live Job must exist after submit"
    );
    let job_uid = items[0]
        .pointer("/metadata/uid")
        .and_then(Value::as_str)
        .ok_or("live Job must expose a server UID")?
        .to_owned();
    println!("live submit readback: job={job_name} uid={job_uid}");

    // Duplicate submit must keep the exact same live identity.
    api.start(&bundle).await?;
    let items = api
        .list(&namespace, "batch/v1", "jobs", MANAGED_BY_SELECTOR)
        .await?;
    assert_eq!(
        items.len(),
        1,
        "duplicate submit must not create a second Job"
    );
    assert_eq!(
        items[0].pointer("/metadata/uid").and_then(Value::as_str),
        Some(job_uid.as_str())
    );

    let observation = tokio::time::timeout(Duration::from_mins(2), async {
        loop {
            match api.observe(&identity, Some(&job_uid)).await? {
                KubernetesJobObservation::Running => {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
                KubernetesJobObservation::Completed {
                    message,
                    observation,
                } => {
                    assert!(message.contains("labweaver.live/v1"));
                    return Ok::<_, KubernetesJobError>(observation);
                }
                KubernetesJobObservation::Failed { .. } => {
                    return Err(KubernetesJobError::KubernetesRejected);
                }
                KubernetesJobObservation::Missing => {
                    return Err(KubernetesJobError::ObservationInvalid);
                }
            }
        }
    })
    .await
    .map_err(|_| "live observation timed out")??;
    assert_eq!(observation.state, ExecutionWorkloadState::Succeeded);
    println!(
        "live observe readback: state={:?} pod={:?} exit={:?}",
        observation.state, observation.pod_name, observation.exit_code
    );

    let cleanup = tokio::time::timeout(Duration::from_mins(1), async {
        loop {
            let status = api
                .cleanup(&namespace, &job_name, &bundle.objects, &bundle.cleanup_plan)
                .await?;
            if status.is_confirmed() {
                return Ok::<_, KubernetesJobError>(status);
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .map_err(|_| "live cleanup timed out")??;
    assert!(
        cleanup.is_confirmed(),
        "cleanup must be verifiably complete"
    );
    let items = api
        .list(&namespace, "batch/v1", "jobs", MANAGED_BY_SELECTOR)
        .await?;
    assert!(items.is_empty(), "cleaned Job must be absent from readback");
    println!("live cleanup readback: confirmed; managed Jobs remaining=0");
    Ok(())
}
