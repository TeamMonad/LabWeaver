//! Ansible probe Kubernetes Job isolation, identity, and cleanup-plan tests.

use std::net::Ipv4Addr;

use contracts::evaluation::FactAssertion;
use evaluation_service::ansible_probe::{
    ANSIBLE_PROBE_EXECUTION_SCHEMA_VERSION, AnsibleProbeExecutionLimits,
    AnsibleProbeExecutionRequest, AnsibleProbeSshIdentity, AnsibleProbeTarget,
};
use evaluation_service::ansible_probe_job::{
    AnsibleProbeJobBinding, AnsibleProbeJobError, AnsibleProbeJobResources,
};
use persistence_sqlx::Sha256Digest;
use serde_json::{Value, json};
use uuid::Uuid;

fn assertion(fact: &str, expected: &serde_json::Value) -> FactAssertion {
    match serde_json::from_value(json!({ "fact": fact, "expected": expected })) {
        Ok(assertion) => assertion,
        Err(error) => unreachable!("fixture assertion must deserialize: {error}"),
    }
}

fn request() -> AnsibleProbeExecutionRequest {
    AnsibleProbeExecutionRequest {
        schema_version: ANSIBLE_PROBE_EXECUTION_SCHEMA_VERSION.to_owned(),
        run_id: Uuid::now_v7(),
        step_run_id: Uuid::now_v7(),
        attempt_id: Uuid::now_v7(),
        trace_id: "trace-ansible-probe-job-test".to_owned(),
        runner_image_digest: format!("labweaver/ansible-probe@sha256:{}", "2".repeat(64)),
        playbook_profile: "linux-nginx-probe-v1".to_owned(),
        module_allowlist: vec![
            "ansible.builtin.package_facts".to_owned(),
            "ansible.builtin.service_facts".to_owned(),
            "ansible.builtin.stat".to_owned(),
        ],
        read_only: true,
        assertions: vec![
            assertion("host.reachable", &json!(true)),
            assertion("service.nginx.active", &json!(true)),
        ],
        target: AnsibleProbeTarget {
            host: Ipv4Addr::new(192, 168, 56, 10),
            port: 22,
            username: "labweaver".to_owned(),
        },
        ssh_identity: AnsibleProbeSshIdentity {
            private_key_secret: "probe-ssh-key".to_owned(),
            certificate_secret: "probe-ssh-cert".to_owned(),
            expected_host_key_sha256: Sha256Digest::of_bytes(b"host-key"),
        },
        limits: AnsibleProbeExecutionLimits {
            wall_time_seconds: 60,
            facts_max_bytes: 1024 * 1024,
            output_max_bytes: 64 * 1024,
            max_assertions: 8,
        },
        evaluation_spec_sha256: Sha256Digest::of_bytes(b"evaluation-spec"),
    }
}

fn binding() -> AnsibleProbeJobBinding {
    AnsibleProbeJobBinding {
        namespace: "labweaver-evaluation-runs".to_owned(),
        service_account_name: "evaluation-ansible-probe".to_owned(),
        image_pull_secret_name: "harbor-labweaver-system-pull".to_owned(),
        worker_image: format!(
            "harbor.internal/labweaver/ansible-probe@sha256:{}",
            "2".repeat(64)
        ),
        request: request(),
    }
}

fn pointer<'a>(value: &'a Value, path: &str) -> &'a Value {
    value.pointer(path).unwrap_or(&Value::Null)
}

fn error_diagnostic<T>(
    result: Result<T, AnsibleProbeJobError>,
) -> Result<&'static str, Box<dyn std::error::Error>> {
    match result {
        Ok(_) => Err("expected ansible probe Job validation failure".into()),
        Err(error) => Ok(error.diagnostic_code()),
    }
}

#[test]
fn job_plan_is_non_root_bounded_and_read_only() -> Result<(), Box<dyn std::error::Error>> {
    let binding = binding();
    let request_sha256 = binding.request.request_sha256()?.to_string();
    let resources = AnsibleProbeJobResources::build(&binding)?;
    let job = &resources.job;

    assert_eq!(pointer(job, "/spec/backoffLimit"), 0);
    assert_eq!(pointer(job, "/spec/activeDeadlineSeconds"), 90);
    assert_eq!(
        pointer(job, "/spec/template/spec/automountServiceAccountToken"),
        false
    );
    assert_eq!(
        pointer(job, "/spec/template/spec/securityContext/runAsNonRoot"),
        true
    );
    assert_eq!(
        pointer(job, "/spec/template/spec/securityContext/runAsUser"),
        65_532
    );
    assert_eq!(
        pointer(job, "/spec/template/spec/securityContext/runAsGroup"),
        65_532
    );
    assert_eq!(
        pointer(
            job,
            "/spec/template/spec/securityContext/seccompProfile/type"
        ),
        "RuntimeDefault"
    );
    assert_eq!(
        pointer(
            job,
            "/spec/template/spec/containers/0/securityContext/allowPrivilegeEscalation"
        ),
        false
    );
    assert_eq!(
        pointer(
            job,
            "/spec/template/spec/containers/0/securityContext/readOnlyRootFilesystem"
        ),
        true
    );
    assert_eq!(
        pointer(
            job,
            "/spec/template/spec/containers/0/securityContext/capabilities/drop/0"
        ),
        "ALL"
    );
    assert_eq!(
        pointer(job, "/spec/template/spec/containers/0/args"),
        &json!(["--mode", "ansible-probe-worker"])
    );
    assert_eq!(
        pointer(job, "/metadata/annotations/labweaver.io~1trace-id"),
        "trace-ansible-probe-job-test"
    );
    assert_eq!(
        pointer(job, "/metadata/annotations/labweaver.io~1request-sha256").as_str(),
        Some(request_sha256.as_str())
    );
    for labels in [
        pointer(&resources.config_map, "/metadata/labels"),
        pointer(&resources.network_policy, "/metadata/labels"),
        pointer(job, "/metadata/labels"),
        pointer(job, "/spec/template/metadata/labels"),
    ] {
        assert!(labels.as_object().is_some_and(|labels| {
            labels
                .values()
                .all(|value| value.as_str().is_some_and(|value| value.len() <= 63))
        }));
    }
    Ok(())
}

#[test]
fn network_policy_allows_only_target_ssh_egress() -> Result<(), Box<dyn std::error::Error>> {
    let resources = AnsibleProbeJobResources::build(&binding())?;
    let policy = &resources.network_policy;

    // The attempt policy denies all ingress and permits only TCP/22 egress to
    // the exact target IPv4; no DNS or other destination is allowed.
    assert_eq!(pointer(policy, "/spec/policyTypes/0"), "Ingress");
    assert_eq!(pointer(policy, "/spec/policyTypes/1"), "Egress");
    assert_eq!(pointer(policy, "/spec/ingress"), &json!([]));
    assert_eq!(
        pointer(policy, "/spec/egress"),
        &json!([{
            "to":[{"ipBlock":{"cidr":"192.168.56.10/32"}}],
            "ports":[{"protocol":"TCP","port":22}],
        }])
    );
    Ok(())
}

#[test]
fn ssh_identity_volumes_are_read_only_and_bounded() -> Result<(), Box<dyn std::error::Error>> {
    let resources = AnsibleProbeJobResources::build(&binding())?;
    let job = &resources.job;

    // SSH identity is mounted read-only from the two request-referenced
    // Secrets with owner-read-only file modes; command and volumes are bounded.
    let serialized = serde_json::to_string(&resources.job)?;
    assert!(!serialized.contains("probe-ssh-key-content"));
    assert_eq!(
        pointer(job, "/spec/template/spec/volumes/1/secret/secretName"),
        "probe-ssh-key"
    );
    assert_eq!(
        pointer(job, "/spec/template/spec/volumes/1/secret/defaultMode"),
        256
    );
    assert_eq!(
        pointer(job, "/spec/template/spec/volumes/2/secret/secretName"),
        "probe-ssh-cert"
    );
    assert_eq!(
        pointer(job, "/spec/template/spec/volumes/2/secret/defaultMode"),
        256
    );
    for index in 0..3 {
        assert_eq!(
            pointer(
                job,
                &format!("/spec/template/spec/containers/0/volumeMounts/{index}/readOnly")
            ),
            &Value::Bool(true)
        );
    }
    assert_eq!(
        pointer(
            job,
            "/spec/template/spec/containers/0/volumeMounts/1/mountPath"
        ),
        "/run/secrets/probe/private-key"
    );
    assert_eq!(
        pointer(
            job,
            "/spec/template/spec/containers/0/volumeMounts/2/mountPath"
        ),
        "/run/secrets/probe/certificate"
    );
    assert_eq!(
        pointer(job, "/spec/template/spec/volumes/3/emptyDir/sizeLimit"),
        "64Mi"
    );
    assert_eq!(
        pointer(job, "/spec/template/spec/volumes/4/emptyDir/sizeLimit"),
        "16Mi"
    );
    Ok(())
}

#[test]
fn command_config_map_is_immutable_and_carries_the_validated_request()
-> Result<(), Box<dyn std::error::Error>> {
    let binding = binding();
    let resources = AnsibleProbeJobResources::build(&binding)?;
    assert_eq!(pointer(&resources.config_map, "/immutable"), true);
    let command = resources
        .config_map
        .pointer("/data/command.json")
        .and_then(Value::as_str)
        .ok_or("command.json missing")?;
    let parsed: AnsibleProbeExecutionRequest = serde_json::from_str(command)?;
    assert_eq!(parsed, binding.request);
    Ok(())
}

#[test]
fn job_identity_is_attempt_scoped_and_cleanup_never_targets_secrets_or_namespaces()
-> Result<(), Box<dyn std::error::Error>> {
    let binding = binding();
    let resources = AnsibleProbeJobResources::build(&binding)?;
    let name = resources.name();
    assert!(name.starts_with("lw-ap-"));
    assert!(name.len() <= 63);
    for document in [
        &resources.config_map,
        &resources.network_policy,
        &resources.job,
    ] {
        assert_eq!(pointer(document, "/metadata/name").as_str(), Some(name));
    }

    let cleanup = resources.cleanup_plan();
    assert_eq!(cleanup.len(), 3);
    assert!(
        cleanup
            .iter()
            .all(|target| target.namespace == binding.namespace)
    );
    assert!(cleanup.iter().all(|target| target.name == name));
    assert!(
        cleanup
            .iter()
            .all(|target| target.propagation_policy == "Foreground")
    );
    assert!(cleanup.iter().all(|target| !matches!(
        target.resource.as_str(),
        "namespaces" | "secrets" | "persistentvolumeclaims"
    )));
    Ok(())
}

#[test]
fn job_plan_rejects_mutable_or_mismatched_images_and_invalid_bindings()
-> Result<(), Box<dyn std::error::Error>> {
    let mut value = binding();
    value.worker_image = "ansible:latest".to_owned();
    assert_eq!(
        error_diagnostic(AnsibleProbeJobResources::build(&value))?,
        "LW_AP_JOB_BINDING_INVALID"
    );

    let mut value = binding();
    value.worker_image = format!(
        "harbor.internal/labweaver/ansible-probe@sha256:{}",
        "9".repeat(64)
    );
    assert_eq!(
        error_diagnostic(AnsibleProbeJobResources::build(&value))?,
        "LW_AP_JOB_BINDING_INVALID"
    );

    let mut value = binding();
    value.namespace = "../escape".to_owned();
    assert_eq!(
        error_diagnostic(AnsibleProbeJobResources::build(&value))?,
        "LW_AP_JOB_BINDING_INVALID"
    );

    let mut value = binding();
    value.request.target.port = 2222;
    assert_eq!(
        error_diagnostic(AnsibleProbeJobResources::build(&value))?,
        "LW_AP_JOB_BINDING_INVALID"
    );
    Ok(())
}
