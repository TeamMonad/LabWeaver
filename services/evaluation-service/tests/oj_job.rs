//! OJ Kubernetes Job isolation, identity, and cleanup-plan tests.

use std::path::Path;

use contracts::Sha256Digest;
use evaluation_service::{
    oj::{
        OjCaseBinding, OjCheckerKind, OjExecutionLimits, OjExecutionPhase, OjExecutionRequest,
        OjFileBinding,
    },
    oj_job::{OjJobBinding, OjJobError, OjJobResources},
};
use serde_json::Value;
use uuid::Uuid;

fn request() -> OjExecutionRequest {
    let empty = Sha256Digest::of_bytes(b"");
    OjExecutionRequest {
        schema_version: "evaluation.labweaver.io/oj-execution/v1".to_owned(),
        run_id: Uuid::now_v7(),
        step_run_id: Uuid::now_v7(),
        attempt_id: Uuid::now_v7(),
        trace_id: "trace-oj-job-test".to_owned(),
        toolchain_profile: "cpp17-approved-v1".to_owned(),
        toolchain_image_digest: format!("sha256:{}", "1".repeat(64)),
        submission_identity: Sha256Digest::of_bytes(b"submission"),
        evaluator_identity: Some(Sha256Digest::of_bytes(b"evaluator")),
        source: OjFileBinding {
            path: "src/main.cpp".to_owned(),
            sha256: empty,
            size_bytes: 1,
        },
        phase: OjExecutionPhase::Test,
        checker: Some(OjCheckerKind::Exact),
        cases: vec![OjCaseBinding {
            id: "basic".to_owned(),
            input: OjFileBinding {
                path: "cases/basic.in".to_owned(),
                sha256: empty,
                size_bytes: 1,
            },
            expected: OjFileBinding {
                path: "cases/basic.out".to_owned(),
                sha256: empty,
                size_bytes: 1,
            },
            max_points: 100,
        }],
        score_max_points: 100,
        limits: OjExecutionLimits {
            compile_wall_milliseconds: 10_000,
            run_wall_milliseconds: 1_000,
            cpu_milliseconds: 500,
            memory_bytes: 32 * 1024 * 1024,
            output_bytes: 1024,
        },
    }
}

fn binding() -> OjJobBinding {
    OjJobBinding {
        namespace: "labweaver-evaluation-runs".to_owned(),
        service_account_name: "evaluation-runner".to_owned(),
        image_pull_secret_name: "harbor-labweaver-system-pull".to_owned(),
        submission_pvc: "submission-019f".to_owned(),
        evaluator_pvc: Some("evaluator-019f".to_owned()),
        evidence_pvc: "evidence-019f".to_owned(),
        worker_image: format!(
            "harbor.internal/labweaver/oj-cpp17@sha256:{}",
            "1".repeat(64)
        ),
        request: request(),
    }
}

fn pointer<'a>(value: &'a Value, path: &str) -> &'a Value {
    value.pointer(path).unwrap_or(&Value::Null)
}

fn error_diagnostic<T>(
    result: Result<T, OjJobError>,
) -> Result<&'static str, Box<dyn std::error::Error>> {
    match result {
        Ok(_) => Err("expected OJ Job validation failure".into()),
        Err(error) => Ok(error.diagnostic_code()),
    }
}

#[test]
fn job_plan_is_non_root_bounded_read_only_and_has_no_network_egress()
-> Result<(), Box<dyn std::error::Error>> {
    let resources = OjJobResources::build(&binding())?;
    let job = &resources.job;
    let policy = &resources.network_policy;

    assert_eq!(pointer(job, "/spec/backoffLimit"), 0);
    assert_eq!(
        pointer(job, "/spec/template/spec/automountServiceAccountToken"),
        false
    );
    assert_eq!(
        pointer(job, "/metadata/annotations/labweaver.io~1trace-id"),
        "trace-oj-job-test"
    );
    assert_eq!(
        pointer(job, "/spec/template/spec/securityContext/runAsNonRoot"),
        true
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
        pointer(
            job,
            "/spec/template/spec/containers/0/resources/limits/memory"
        ),
        "512Mi"
    );
    assert_eq!(
        pointer(
            job,
            "/spec/template/spec/containers/0/volumeMounts/1/readOnly"
        ),
        true
    );
    assert_eq!(
        pointer(
            job,
            "/spec/template/spec/containers/0/volumeMounts/2/readOnly"
        ),
        true
    );
    assert_eq!(pointer(policy, "/spec/policyTypes/0"), "Ingress");
    assert_eq!(pointer(policy, "/spec/policyTypes/1"), "Egress");
    assert_eq!(pointer(policy, "/spec/ingress"), &serde_json::json!([]));
    assert_eq!(pointer(policy, "/spec/egress"), &serde_json::json!([]));

    let serialized = serde_json::to_string(&resources)?;
    assert!(!serialized.contains("basic.in\\n"));
    assert!(!serialized.contains("expected output"));
    Ok(())
}

#[test]
fn job_memory_limit_preserves_the_request_limit_plus_worker_overhead()
-> Result<(), Box<dyn std::error::Error>> {
    let mut binding = binding();
    binding.request.limits.memory_bytes = 2 * 1024 * 1024 * 1024;
    let resources = OjJobResources::build(&binding)?;
    assert_eq!(
        pointer(
            &resources.job,
            "/spec/template/spec/containers/0/resources/limits/memory"
        ),
        "2304Mi"
    );
    Ok(())
}

#[test]
fn compile_job_does_not_mount_the_private_evaluator() -> Result<(), Box<dyn std::error::Error>> {
    let mut binding = binding();
    binding.request.phase = OjExecutionPhase::Compile;
    binding.request.checker = None;
    binding.request.cases.clear();
    binding.request.score_max_points = 0;
    binding.request.evaluator_identity = None;
    binding.evaluator_pvc = None;
    let resources = OjJobResources::build(&binding)?;
    let serialized = serde_json::to_string(&resources.job)?;
    assert!(!serialized.contains("/input/evaluator"));
    assert!(!serialized.contains("evaluator-019f"));
    Ok(())
}

#[test]
fn job_identity_is_attempt_scoped_and_cleanup_never_targets_namespace_or_pvcs()
-> Result<(), Box<dyn std::error::Error>> {
    let binding = binding();
    let resources = OjJobResources::build(&binding)?;
    let name = resources.name();
    assert!(name.starts_with("lw-oj-"));
    assert_eq!(
        pointer(&resources.job, "/metadata/name").as_str(),
        Some(name)
    );
    assert_eq!(
        pointer(&resources.config_map, "/metadata/name").as_str(),
        Some(name)
    );
    assert_eq!(
        pointer(&resources.network_policy, "/metadata/name").as_str(),
        Some(name)
    );

    let cleanup = resources.cleanup_plan();
    assert_eq!(cleanup.len(), 3);
    assert!(
        cleanup
            .iter()
            .all(|target| target.namespace == binding.namespace)
    );
    assert!(cleanup.iter().all(|target| target.name == name));
    assert!(cleanup.iter().all(|target| !matches!(
        target.resource.as_str(),
        "namespaces" | "persistentvolumeclaims"
    )));
    Ok(())
}

#[test]
fn job_plan_rejects_mutable_images_invalid_pvc_names_and_oversized_commands()
-> Result<(), Box<dyn std::error::Error>> {
    let mut value = binding();
    value.worker_image = "gcc:latest".to_owned();
    assert_eq!(
        error_diagnostic(OjJobResources::build(&value))?,
        "LW_OJ_JOB_BINDING_INVALID"
    );

    let mut value = binding();
    value.submission_pvc = "../submission".to_owned();
    assert_eq!(
        error_diagnostic(OjJobResources::build(&value))?,
        "LW_OJ_JOB_BINDING_INVALID"
    );

    let mut value = binding();
    value.request.cases[0].id = "x".repeat(1_100_000);
    assert!(OjJobResources::build(&value).is_err());
    Ok(())
}

#[test]
fn pinned_toolchain_container_is_in_ci_and_version_lock() -> Result<(), Box<dyn std::error::Error>>
{
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let containerfile = std::fs::read_to_string(root.join("containers/Containerfile.oj-cpp17"))?;
    let versions = std::fs::read_to_string(root.join("deploy/versions.lock.yml"))?;
    let workflow = std::fs::read_to_string(root.join(".github/workflows/platform-images.yml"))?;
    let runtime = "cgr.dev/chainguard/gcc-glibc@sha256:43d1e8014e4b3075c60b45f30dd72dd783a67c32fd95849bb6c4870f0a16276c";
    assert!(containerfile.contains(runtime));
    assert!(containerfile.contains("io.labweaver.toolchain-profile=\"cpp17-approved-v1\""));
    assert!(versions.contains(runtime));
    assert!(workflow.contains("- oj-cpp17-runner"));
    Ok(())
}
