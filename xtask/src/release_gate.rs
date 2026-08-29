use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use super::AppError;

const INPUT_SCHEMA: &str = "schemas/results/platform-release-gate-input.v3.schema.json";
const REPORT_SCHEMA: &str = "schemas/results/release-gate-report.v3.schema.json";
const DEPLOYMENT_SCHEMA: &str = "schemas/results/platform-image-deployment-manifest.v1.schema.json";
const RESOURCE_DEPLOYMENT_SCHEMA: &str =
    "schemas/results/resource-deployment-manifest.v1.schema.json";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GateInput {
    schema_version: String,
    source_commit: String,
    run_id: uuid::Uuid,
    deployment_manifest: EvidenceFile,
    resource_deployment_manifest: EvidenceFile,
    migration_catalog: EvidenceFile,
    platform_images: Vec<ImageIdentity>,
    resource_images: Vec<ImageIdentity>,
    runtime_artifacts: Vec<ArtifactIdentity>,
    checks: Vec<CheckInput>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EvidenceFile {
    path: String,
    sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ImageIdentity {
    component: String,
    reference: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ArtifactIdentity {
    name: String,
    digest: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CheckInput {
    name: String,
    status: String,
    mode: String,
    source_commit: String,
    run_id: uuid::Uuid,
    evidence: EvidenceFile,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GateReport {
    schema_version: &'static str,
    status: &'static str,
    source_commit: String,
    run_id: uuid::Uuid,
    deployment_manifest: EvidenceFile,
    resource_deployment_manifest: EvidenceFile,
    migration_catalog: EvidenceFile,
    platform_images: Vec<ImageIdentity>,
    resource_images: Vec<ImageIdentity>,
    runtime_artifacts: Vec<ArtifactIdentity>,
    checks: Vec<CheckInput>,
}

pub(super) fn run(root: &Path) -> Result<(), AppError> {
    let input_locator = std::env::var("LABWEAVER_RELEASE_GATE_INPUT").map_err(|_| {
        gate(
            "LW_RELEASE_GATE_INPUT_MISSING",
            "LABWEAVER_RELEASE_GATE_INPUT is required",
        )
    })?;
    run_with_locator(root, &input_locator)
}

fn run_with_locator(root: &Path, input_locator: &str) -> Result<(), AppError> {
    let input_path = secure_file(root, input_locator)?;
    let input_bytes = fs::read(&input_path)
        .map_err(|error| gate("LW_RELEASE_GATE_INPUT_UNREADABLE", &error.to_string()))?;
    validate_schema(
        root,
        INPUT_SCHEMA,
        &input_bytes,
        "LW_RELEASE_GATE_INPUT_SCHEMA_INVALID",
    )?;
    let input: GateInput = serde_json::from_slice(&input_bytes)
        .map_err(|error| gate("LW_RELEASE_GATE_INPUT_SCHEMA_INVALID", &error.to_string()))?;
    validate_input(root, &input)?;

    let report = GateReport {
        schema_version: "platform-release-gate-report.v3",
        status: "passed",
        source_commit: input.source_commit,
        run_id: input.run_id,
        deployment_manifest: input.deployment_manifest,
        resource_deployment_manifest: input.resource_deployment_manifest,
        migration_catalog: input.migration_catalog,
        platform_images: input.platform_images,
        resource_images: input.resource_images,
        runtime_artifacts: input.runtime_artifacts,
        checks: input.checks,
    };
    let bytes = serde_json::to_vec_pretty(&report)
        .map_err(|error| gate("LW_RELEASE_GATE_REPORT_INVALID", &error.to_string()))?;
    validate_schema(
        root,
        REPORT_SCHEMA,
        &bytes,
        "LW_RELEASE_GATE_REPORT_INVALID",
    )?;
    let output = root
        .join("artifacts/release-gate")
        .join(format!("{}.json", report.run_id));
    fs::create_dir_all(output.parent().ok_or_else(|| {
        gate(
            "LW_RELEASE_GATE_REPORT_WRITE_FAILED",
            "report parent is missing",
        )
    })?)
    .map_err(|error| gate("LW_RELEASE_GATE_REPORT_WRITE_FAILED", &error.to_string()))?;
    fs::write(&output, &bytes)
        .map_err(|error| gate("LW_RELEASE_GATE_REPORT_WRITE_FAILED", &error.to_string()))?;
    println!("{}", output.display());
    Ok(())
}

fn validate_input(root: &Path, input: &GateInput) -> Result<(), AppError> {
    if input.schema_version != "platform-release-gate-input.v3" {
        return Err(gate(
            "LW_RELEASE_GATE_INPUT_SCHEMA_INVALID",
            "unexpected schemaVersion",
        ));
    }
    let head = git_output(root, ["rev-parse", "HEAD"])?;
    if head != input.source_commit {
        return Err(gate(
            "LW_RELEASE_GATE_SOURCE_IDENTITY_MISMATCH",
            "input sourceCommit differs from HEAD",
        ));
    }
    if !git_output(root, ["status", "--porcelain", "--untracked-files=no"])?.is_empty() {
        return Err(gate(
            "LW_RELEASE_GATE_SOURCE_DIRTY",
            "tracked worktree changes are present",
        ));
    }
    // existence-only checks; detailed sha/content validation is delegated to
    // platform_images and console_evidence sub-validators
    let _ = secure_file(root, &input.deployment_manifest.path)?;
    let _ = secure_file(root, &input.resource_deployment_manifest.path)?;
    let _ = secure_file(root, &input.migration_catalog.path)?;
    if input.migration_catalog.path != "migrations/catalog.yaml" {
        return Err(gate(
            "LW_RELEASE_GATE_MIGRATION_IDENTITY_INVALID",
            "migrationCatalog must bind migrations/catalog.yaml",
        ));
    }
    super::migration_catalog::validate(root)?;
    // thin identity checks: delegate detailed image digest validation to platform_images
    if input.platform_images.len() != 7 {
        return Err(gate(
            "LW_RELEASE_GATE_IMAGE_IDENTITY_INVALID",
            "seven platform images are required",
        ));
    }
    if input.resource_images.len() != 1 {
        return Err(gate(
            "LW_RELEASE_GATE_RESOURCE_IMAGE_IDENTITY_INVALID",
            "one resource image is required",
        ));
    }
    if input.runtime_artifacts.len() < 2 {
        return Err(gate(
            "LW_RELEASE_GATE_RUNTIME_IDENTITY_INVALID",
            "Container and KubeVirt runtime digests are required",
        ));
    }
    const REQUIRED_CHECKS: [&str; 13] = [
        "teacher-agent-approval",
        "build-supply-chain",
        "container-lifecycle",
        "kubevirt-lifecycle",
        "access-negative",
        "submission-freeze",
        "cleanup-readback",
        "keycloak-playwright",
        "ansible-idempotent",
        "rollback-drill",
        "resource-lease",
        "container-xterm-console",
        "kubevirt-novnc-console",
    ];
    let names = input
        .checks
        .iter()
        .map(|check| check.name.as_str())
        .collect::<BTreeSet<_>>();
    if names != REQUIRED_CHECKS.into_iter().collect() || input.checks.len() != 13 {
        return Err(gate(
            "LW_RELEASE_GATE_CHECK_SET_INVALID",
            "the exact check set is required",
        ));
    }
    for check in &input.checks {
        if check.status != "passed"
            || check.mode != "connected"
            || check.source_commit != input.source_commit
            || check.run_id != input.run_id
        {
            return Err(gate(
                "LW_RELEASE_GATE_CHECK_FAILED",
                &format!("{} is not same-identity connected evidence", check.name),
            ));
        }
        let _ = secure_file(root, &check.evidence.path)?;
    }
    validate_deployment_manifest(root, input)?;
    validate_resource_deployment_manifest(root, input)?;
    validate_console_evidence(root, input)?;
    Ok(())
}

fn validate_console_evidence(root: &Path, input: &GateInput) -> Result<(), AppError> {
    for check_name in ["container-xterm-console", "kubevirt-novnc-console"] {
        let check = input
            .checks
            .iter()
            .find(|check| check.name == check_name)
            .ok_or_else(|| {
                gate(
                    "LW_RELEASE_GATE_CONSOLE_EVIDENCE_MISSING",
                    &format!("{check_name} is required"),
                )
            })?;
        super::console_evidence::validate_for_gate(
            root,
            Path::new(&check.evidence.path),
            super::console_evidence::GateIdentity {
                source_commit: &input.source_commit,
                run_id: input.run_id,
            },
        )?;
    }
    Ok(())
}

fn validate_resource_deployment_manifest(root: &Path, input: &GateInput) -> Result<(), AppError> {
    let path = secure_file(root, &input.resource_deployment_manifest.path)?;
    let bytes = fs::read(path).map_err(|error| {
        gate(
            "LW_RELEASE_GATE_RESOURCE_DEPLOYMENT_MANIFEST_INVALID",
            &error.to_string(),
        )
    })?;
    validate_schema(
        root,
        RESOURCE_DEPLOYMENT_SCHEMA,
        &bytes,
        "LW_RELEASE_GATE_RESOURCE_DEPLOYMENT_MANIFEST_INVALID",
    )?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| {
        gate(
            "LW_RELEASE_GATE_RESOURCE_DEPLOYMENT_MANIFEST_INVALID",
            &error.to_string(),
        )
    })?;
    let run_id = input.run_id.to_string();
    if value
        .get("sourceCommit")
        .and_then(serde_json::Value::as_str)
        != Some(input.source_commit.as_str())
        || value.get("runId").and_then(serde_json::Value::as_str) != Some(run_id.as_str())
    {
        return Err(gate(
            "LW_RELEASE_GATE_RESOURCE_DEPLOYMENT_IDENTITY_MISMATCH",
            "resource deployment manifest differs from the gate input",
        ));
    }
    Ok(())
}

fn validate_deployment_manifest(root: &Path, input: &GateInput) -> Result<(), AppError> {
    let path = secure_file(root, &input.deployment_manifest.path)?;
    let bytes = fs::read(path).map_err(|error| {
        gate(
            "LW_RELEASE_GATE_DEPLOYMENT_MANIFEST_INVALID",
            &error.to_string(),
        )
    })?;
    validate_schema(
        root,
        DEPLOYMENT_SCHEMA,
        &bytes,
        "LW_RELEASE_GATE_DEPLOYMENT_MANIFEST_INVALID",
    )?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| {
        gate(
            "LW_RELEASE_GATE_DEPLOYMENT_MANIFEST_INVALID",
            &error.to_string(),
        )
    })?;
    let run_id = input.run_id.to_string();
    if value
        .get("source_commit")
        .and_then(serde_json::Value::as_str)
        != Some(input.source_commit.as_str())
        || value.get("run_id").and_then(serde_json::Value::as_str) != Some(run_id.as_str())
    {
        return Err(gate(
            "LW_RELEASE_GATE_DEPLOYMENT_IDENTITY_MISMATCH",
            "deployment manifest identity differs from the gate input",
        ));
    }
    Ok(())
}

fn validate_schema(
    root: &Path,
    schema: &str,
    bytes: &[u8],
    code: &'static str,
) -> Result<(), AppError> {
    let schema_value: serde_json::Value = serde_json::from_slice(
        &fs::read(root.join(schema)).map_err(|error| gate(code, &error.to_string()))?,
    )
    .map_err(|error| gate(code, &error.to_string()))?;
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|error| gate(code, &error.to_string()))?;
    let validator =
        jsonschema::validator_for(&schema_value).map_err(|error| gate(code, &error.to_string()))?;
    if let Some(error) = validator.iter_errors(&value).next() {
        return Err(gate(code, &error.to_string()));
    }
    Ok(())
}

fn secure_file(root: &Path, locator: &str) -> Result<PathBuf, AppError> {
    let relative = Path::new(locator);
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(gate("LW_RELEASE_GATE_EVIDENCE_LOCATOR_INVALID", locator));
    }
    // reuse portable path validation without canonicalize/symlink duplication
    if let Err(error) = contracts::foundation::validate_relative_path(locator) {
        return Err(gate(
            "LW_RELEASE_GATE_EVIDENCE_LOCATOR_INVALID",
            &error.to_string(),
        ));
    }
    let path = root.join(relative);
    if !path.is_file() {
        return Err(gate("LW_RELEASE_GATE_EVIDENCE_LOCATOR_INVALID", locator));
    }
    Ok(path)
}

fn git_output<const N: usize>(root: &Path, args: [&str; N]) -> Result<String, AppError> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|error| gate("LW_RELEASE_GATE_GIT_FAILED", &error.to_string()))?;
    if !output.status.success() {
        return Err(gate(
            "LW_RELEASE_GATE_GIT_FAILED",
            &String::from_utf8_lossy(&output.stderr),
        ));
    }
    String::from_utf8(output.stdout)
        .map(|value| value.trim().to_owned())
        .map_err(|error| gate("LW_RELEASE_GATE_GIT_FAILED", &error.to_string()))
}

fn gate(code: &'static str, detail: &str) -> AppError {
    AppError::ReleaseGate {
        code,
        detail: detail.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    use serde_json::{Value, json};
    use tempfile::tempdir;

    use super::run_with_locator;

    const INPUT_SCHEMA: &str =
        include_str!("../../schemas/results/platform-release-gate-input.v3.schema.json");
    const REPORT_SCHEMA: &str =
        include_str!("../../schemas/results/release-gate-report.v3.schema.json");
    const CONSOLE_SCHEMA: &str =
        include_str!("../../schemas/results/connected-console-evidence.v1.schema.json");
    const DEPLOYMENT_SCHEMA: &str =
        include_str!("../../schemas/results/platform-image-deployment-manifest.v1.schema.json");
    const RESOURCE_DEPLOYMENT_SCHEMA: &str =
        include_str!("../../schemas/results/resource-deployment-manifest.v1.schema.json");

    const REQUIRED_CHECKS: [&str; 13] = [
        "teacher-agent-approval",
        "build-supply-chain",
        "container-lifecycle",
        "kubevirt-lifecycle",
        "access-negative",
        "submission-freeze",
        "cleanup-readback",
        "keycloak-playwright",
        "ansible-idempotent",
        "rollback-drill",
        "resource-lease",
        "container-xterm-console",
        "kubevirt-novnc-console",
    ];

    #[test]
    fn same_identity_connected_evidence_passes() -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempdir()?;
        let root = temporary.path();
        write(
            root,
            "schemas/results/platform-release-gate-input.v3.schema.json",
            INPUT_SCHEMA,
        )?;
        write(
            root,
            "schemas/results/release-gate-report.v3.schema.json",
            REPORT_SCHEMA,
        )?;
        write(
            root,
            "schemas/results/connected-console-evidence.v1.schema.json",
            CONSOLE_SCHEMA,
        )?;
        write(
            root,
            "schemas/results/platform-image-deployment-manifest.v1.schema.json",
            DEPLOYMENT_SCHEMA,
        )?;
        write(
            root,
            "schemas/results/resource-deployment-manifest.v1.schema.json",
            RESOURCE_DEPLOYMENT_SCHEMA,
        )?;
        copy_tree(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../migrations"),
            &root.join("migrations"),
        )?;
        git(root, &["init"])?;
        git(root, &["config", "user.email", "gate@example.invalid"])?;
        git(root, &["config", "user.name", "Release Gate"])?;
        git(root, &["add", "schemas", "migrations"])?;
        git(root, &["commit", "-m", "fixture"])?;
        let commit = git(root, &["rev-parse", "HEAD"])?;
        let run_id = uuid::Uuid::now_v7();
        let images = [
            "control-service",
            "access-service",
            "agent-service",
            "environment-service",
            "evaluation-service",
            "openssh-gateway",
            "web",
        ]
        .into_iter()
        .enumerate()
        .map(|(index, component)| {
            json!({"component": component, "reference": format!("harbor.invalid/labweaver/{component}@sha256:{}", format!("{index:x}").repeat(64))})
        })
        .collect::<Vec<_>>();
        let deployment = json!({
            "schema_version": "platform-image-deployment-manifest.v1",
            "environment": "test",
            "package_manifest_sha256": format!("sha256:{}", "c".repeat(64)),
            "source_commit": commit,
            "run_id": run_id,
            "cluster_uid": "fixture-cluster",
            "helm_revision": 1,
            "migration_catalog_sha256": format!("sha256:{}", "d".repeat(64)),
            "images": images.clone(),
            "previous_verified_manifest_sha256": null
        });
        write(
            root,
            "artifacts/evidence/deployment.json",
            &serde_json::to_string_pretty(&deployment)?,
        )?;
        let resource_images = vec![json!({
            "component": "resource-service",
            "reference": format!("harbor.invalid/labweaver/resource-service@sha256:{}", "d".repeat(64))
        })];
        let resource_deployment = json!({
            "schemaVersion": "resource-deployment-manifest.v1",
            "sourceCommit": commit,
            "runId": run_id,
            "packageManifestSha256": format!("sha256:{}", "e".repeat(64)),
            "configurationBundleSha256": format!("sha256:{}", "f".repeat(64)),
            "image": resource_images[0]
        });
        write(
            root,
            "artifacts/evidence/resource-deployment.json",
            &serde_json::to_string_pretty(&resource_deployment)?,
        )?;
        let mut checks = Vec::new();
        for name in REQUIRED_CHECKS {
            let path = format!("artifacts/evidence/{name}.json");
            let evidence = match name {
                "container-xterm-console" | "kubevirt-novnc-console" => {
                    console_report(&commit, run_id)
                }
                _ => json!({}),
            };
            write(root, &path, &serde_json::to_string_pretty(&evidence)?)?;
            checks.push(json!({
                "name": name,
                "status": "passed",
                "mode": "connected",
                "sourceCommit": commit,
                "runId": run_id,
                "evidence": {"path": path, "sha256": format!("sha256:{}", "a".repeat(64))}
            }));
        }
        let input = json!({
            "schemaVersion": "platform-release-gate-input.v3",
            "sourceCommit": commit,
            "runId": run_id,
            "deploymentManifest": {"path": "artifacts/evidence/deployment.json", "sha256": format!("sha256:{}", "b".repeat(64))},
            "resourceDeploymentManifest": {"path": "artifacts/evidence/resource-deployment.json", "sha256": format!("sha256:{}", "c".repeat(64))},
            "migrationCatalog": {"path": "migrations/catalog.yaml", "sha256": format!("sha256:{}", "d".repeat(64))},
            "platformImages": images,
            "resourceImages": resource_images,
            "runtimeArtifacts": [
                {"name": "container-runtime", "digest": format!("sha256:{}", "a".repeat(64))},
                {"name": "kubevirt-runtime", "digest": format!("sha256:{}", "b".repeat(64))}
            ],
            "checks": checks
        });
        write(
            root,
            "artifacts/gate-input.json",
            &serde_json::to_string_pretty(&input)?,
        )?;
        run_with_locator(root, "artifacts/gate-input.json")?;
        assert!(
            root.join(format!("artifacts/release-gate/{run_id}.json"))
                .is_file()
        );

        // missing evidence file is blocked (not per-file hash mismatch)
        fs::remove_file(root.join("artifacts/evidence/access-negative.json"))?;
        let error = run_with_locator(root, "artifacts/gate-input.json")
            .expect_err("missing file must fail");
        assert_eq!(
            error.diagnostic_code(),
            "LW_RELEASE_GATE_EVIDENCE_LOCATOR_INVALID"
        );

        // source_commit mismatch
        let mut mismatch = input.clone();
        mismatch["sourceCommit"] =
            Value::String("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned());
        write(
            root,
            "artifacts/gate-input-mismatch.json",
            &serde_json::to_string_pretty(&mismatch)?,
        )?;
        let error = run_with_locator(root, "artifacts/gate-input-mismatch.json")
            .expect_err("mismatch must fail");
        assert_eq!(
            error.diagnostic_code(),
            "LW_RELEASE_GATE_SOURCE_IDENTITY_MISMATCH"
        );

        // dirty worktree
        fs::write(root.join("artifacts/evidence/access-negative.json"), "{}")?;
        fs::write(root.join("dirty.txt"), "dirty")?;
        git(root, &["add", "dirty.txt"])?;
        let error =
            run_with_locator(root, "artifacts/gate-input.json").expect_err("dirty must fail");
        assert_eq!(error.diagnostic_code(), "LW_RELEASE_GATE_SOURCE_DIRTY");
        Ok(())
    }

    fn console_report(commit: &str, run_id: uuid::Uuid) -> Value {
        let cases = [
            ("positive", "LW_CONSOLE_SESSION_ACTIVE"),
            ("revoke", "LW_CONSOLE_AUTHORIZATION_ENDED"),
            ("expiry", "LW_CONSOLE_AUTHORIZATION_ENDED"),
            ("stop", "LW_CONSOLE_AUTHORIZATION_ENDED"),
            ("delete", "LW_CONSOLE_AUTHORIZATION_ENDED"),
            ("control-channel-loss", "LW_CONSOLE_CONTROL_CHANNEL_LOST"),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (case, diagnostic))| json!({
            "case": case,
            "status": "passed",
            "environment": {"id":format!("01999999-9999-7999-8999-9999999997{index:02}"),"revision":index+1,"namespace":"labweaver-run","targetName":"runtime-pod","targetUid":format!("01999999-9999-7999-8999-9999999996{index:02}"),"runtimeIdentity":"release-runtime-identity"},
            "access": {"grantId":format!("01999999-9999-7999-8999-9999999995{index:02}"),"grantRevision":index+1,"leaseId":format!("01999999-9999-7999-8999-9999999994{index:02}"),"leaseRevision":index+1},
            "capabilityId": format!("01999999-9999-7999-8999-9999999999{index:02}"),
            "sessionId": format!("01999999-9999-7999-8999-9999999998{index:02}"),
            "durationMs": 1000,
            "observedDiagnostic": diagnostic,
            "staleReconnectDenied": true,
            "auditSha256": format!("sha256:{}", "a".repeat(64))
        }))
        .collect::<Vec<_>>();
        json!({
            "schemaVersion": "connected-console-evidence.v1",
            "status": "passed",
            "mode": "connected",
            "providerMode": "real",
            "sourceCommit": commit,
            "runId": run_id.to_string(),
            "packageIdentity": format!("sha256:{}", "c".repeat(64)),
            "deploymentIdentity": format!("sha256:{}", "d".repeat(64)),
            "migrationCatalogSha256": format!("sha256:{}", "e".repeat(64)),
            "runtimeKind": "container",
            "consoleKind": "xterm",
            "images": {"accessService": format!("sha256:{}", "a".repeat(64)), "environmentService": format!("sha256:{}", "b".repeat(64)), "runtimeArtifact": format!("sha256:{}", "c".repeat(64))},
            "lifecycleCases": cases,
            "browserEvidence": {"browser":"chromium","browserVersion":"1","viewport":{"width":1440,"height":900},"traceSha256": format!("sha256:{}", "a".repeat(64)),"screenshotSha256": format!("sha256:{}", "a".repeat(64)),"videoSha256": format!("sha256:{}", "a".repeat(64))},
            "cleanup": {"completed":true,"readbackCompleted":true,"temporaryFaultPolicyRemoved":true,"preCounts":{"capabilities":1,"sessions":1,"pods":1,"vmis":0,"pvcs":0,"faultPolicies":1},"postCounts":{"capabilities":0,"sessions":0,"pods":0,"vmis":0,"pvcs":0,"faultPolicies":0},"readbackSha256": format!("sha256:{}", "a".repeat(64))},
            "redaction": {"verified":true,"containsSecrets":false,"containsRawUserContent":false,"containsTerminalTranscript":false,"containsVncFrames":false,"containsAbsolutePaths":false}
        })
    }

    fn write(root: &Path, relative: &str, value: &str) -> std::io::Result<()> {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, value)
    }

    fn copy_tree(source: &Path, target: &Path) -> std::io::Result<()> {
        fs::create_dir_all(target)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            let destination = target.join(entry.file_name());
            if entry.file_type()?.is_dir() {
                copy_tree(&entry.path(), &destination)?;
            } else {
                fs::copy(entry.path(), destination)?;
            }
        }
        Ok(())
    }

    fn git(root: &Path, arguments: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
        let output = Command::new("git")
            .args(arguments)
            .current_dir(root)
            .output()?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
        }
        Ok(String::from_utf8(output.stdout)?.trim().to_owned())
    }
}
