use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::AppError;

const INPUT_SCHEMA: &str = "schemas/results/sprint2-release-gate-input.v2.schema.json";
const REPORT_SCHEMA: &str = "schemas/results/release-gate-report.v2.schema.json";
const DEPLOYMENT_SCHEMA: &str = "schemas/results/platform-image-deployment-manifest.v1.schema.json";
const RESOURCE_DEPLOYMENT_SCHEMA: &str =
    "schemas/results/resource-deployment-manifest.v1.schema.json";
const PLATFORM_COMPONENTS: [&str; 7] = [
    "access-service",
    "agent-service",
    "control-service",
    "environment-service",
    "evaluation-service",
    "openssh-gateway",
    "web",
];
const RUNTIME_ARTIFACTS: [&str; 2] = ["container-runtime", "kubevirt-runtime"];
const REQUIRED_CHECKS: [&str; 11] = [
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
];
const RESOURCE_COMPONENTS: [&str; 1] = ["resource-service"];

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
        schema_version: "sprint2-release-gate-report.v2",
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
    println!("{}", relative_path(root, &output)?);
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "the gate keeps every fail-closed identity and evidence check visible in one ordered boundary"
)]
fn validate_input(root: &Path, input: &GateInput) -> Result<(), AppError> {
    if input.schema_version != "sprint2-release-gate-input.v2" {
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
    verify_evidence(root, &input.deployment_manifest)?;
    verify_evidence(root, &input.resource_deployment_manifest)?;
    verify_evidence(root, &input.migration_catalog)?;
    let catalog = relative_path(root, &secure_file(root, &input.migration_catalog.path)?)?;
    if catalog != "migrations/catalog.yaml" {
        return Err(gate(
            "LW_RELEASE_GATE_MIGRATION_IDENTITY_INVALID",
            "migrationCatalog must bind migrations/catalog.yaml",
        ));
    }
    super::migration_catalog::validate(root)?;
    unique_nonempty(
        input
            .platform_images
            .iter()
            .map(|item| item.component.as_str()),
        "LW_RELEASE_GATE_IMAGE_IDENTITY_INVALID",
    )?;
    let platform_components = input
        .platform_images
        .iter()
        .map(|item| item.component.as_str())
        .collect::<BTreeSet<_>>();
    if input.platform_images.len() != 7
        || platform_components != PLATFORM_COMPONENTS.into_iter().collect()
        || input
            .platform_images
            .iter()
            .any(|item| !immutable_image(&item.reference))
    {
        return Err(gate(
            "LW_RELEASE_GATE_IMAGE_IDENTITY_INVALID",
            "seven immutable platform image identities are required",
        ));
    }
    unique_nonempty(
        input
            .resource_images
            .iter()
            .map(|item| item.component.as_str()),
        "LW_RELEASE_GATE_RESOURCE_IMAGE_IDENTITY_INVALID",
    )?;
    let resource_components = input
        .resource_images
        .iter()
        .map(|item| item.component.as_str())
        .collect::<BTreeSet<_>>();
    if input.resource_images.len() != 1
        || resource_components != RESOURCE_COMPONENTS.into_iter().collect()
        || input
            .resource_images
            .iter()
            .any(|item| !immutable_image(&item.reference))
    {
        return Err(gate(
            "LW_RELEASE_GATE_RESOURCE_IMAGE_IDENTITY_INVALID",
            "one immutable resource-service image identity is required",
        ));
    }
    unique_nonempty(
        input
            .runtime_artifacts
            .iter()
            .map(|item| item.name.as_str()),
        "LW_RELEASE_GATE_RUNTIME_IDENTITY_INVALID",
    )?;
    let runtime_artifacts = input
        .runtime_artifacts
        .iter()
        .map(|item| item.name.as_str())
        .collect::<BTreeSet<_>>();
    if input.runtime_artifacts.len() < 2
        || !RUNTIME_ARTIFACTS
            .into_iter()
            .all(|name| runtime_artifacts.contains(name))
        || input
            .runtime_artifacts
            .iter()
            .any(|item| !digest(&item.digest))
    {
        return Err(gate(
            "LW_RELEASE_GATE_RUNTIME_IDENTITY_INVALID",
            "Container and KubeVirt runtime digests are required",
        ));
    }
    let names = input
        .checks
        .iter()
        .map(|check| check.name.as_str())
        .collect::<BTreeSet<_>>();
    if names != REQUIRED_CHECKS.into_iter().collect() || input.checks.len() != REQUIRED_CHECKS.len()
    {
        return Err(gate(
            "LW_RELEASE_GATE_CHECK_SET_INVALID",
            "the exact Sprint 2 check set is required",
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
        verify_evidence(root, &check.evidence)?;
    }
    validate_deployment_manifest(root, input)?;
    validate_resource_deployment_manifest(root, input)?;
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
    let image = input.resource_images.first().ok_or_else(|| {
        gate(
            "LW_RELEASE_GATE_RESOURCE_IMAGE_IDENTITY_INVALID",
            "resource image is missing",
        )
    })?;
    if value
        .get("sourceCommit")
        .and_then(serde_json::Value::as_str)
        != Some(input.source_commit.as_str())
        || value.get("runId").and_then(serde_json::Value::as_str) != Some(run_id.as_str())
        || value
            .pointer("/image/component")
            .and_then(serde_json::Value::as_str)
            != Some(image.component.as_str())
        || value
            .pointer("/image/reference")
            .and_then(serde_json::Value::as_str)
            != Some(image.reference.as_str())
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
        || value
            .get("migration_catalog_sha256")
            .and_then(serde_json::Value::as_str)
            != Some(input.migration_catalog.sha256.as_str())
    {
        return Err(gate(
            "LW_RELEASE_GATE_DEPLOYMENT_IDENTITY_MISMATCH",
            "deployment manifest identity differs from the gate input",
        ));
    }
    let manifest_images = value
        .get("images")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            gate(
                "LW_RELEASE_GATE_DEPLOYMENT_MANIFEST_INVALID",
                "images are missing",
            )
        })?
        .iter()
        .filter_map(|image| {
            Some((
                image.get("component")?.as_str()?,
                image.get("reference")?.as_str()?,
            ))
        })
        .collect::<BTreeSet<_>>();
    let input_images = input
        .platform_images
        .iter()
        .map(|image| (image.component.as_str(), image.reference.as_str()))
        .collect::<BTreeSet<_>>();
    if manifest_images != input_images {
        return Err(gate(
            "LW_RELEASE_GATE_DEPLOYMENT_IDENTITY_MISMATCH",
            "deployment image set differs from the gate input",
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

fn verify_evidence(root: &Path, evidence: &EvidenceFile) -> Result<(), AppError> {
    if !digest(&evidence.sha256) {
        return Err(gate(
            "LW_RELEASE_GATE_EVIDENCE_HASH_INVALID",
            &evidence.path,
        ));
    }
    let path = secure_file(root, &evidence.path)?;
    let bytes = fs::read(path)
        .map_err(|error| gate("LW_RELEASE_GATE_EVIDENCE_UNREADABLE", &error.to_string()))?;
    let actual = format!("sha256:{:x}", Sha256::digest(bytes));
    if actual != evidence.sha256 {
        return Err(gate(
            "LW_RELEASE_GATE_EVIDENCE_HASH_MISMATCH",
            &evidence.path,
        ));
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
    let root = root.canonicalize().map_err(|error| {
        gate(
            "LW_RELEASE_GATE_EVIDENCE_LOCATOR_INVALID",
            &error.to_string(),
        )
    })?;
    let path = root.join(relative).canonicalize().map_err(|error| {
        gate(
            "LW_RELEASE_GATE_EVIDENCE_LOCATOR_INVALID",
            &error.to_string(),
        )
    })?;
    if !path.starts_with(&root)
        || !path.is_file()
        || fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
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

fn unique_nonempty<'a>(
    values: impl Iterator<Item = &'a str>,
    code: &'static str,
) -> Result<(), AppError> {
    let values = values.collect::<Vec<_>>();
    if values.iter().any(|value| value.trim().is_empty())
        || values.iter().copied().collect::<BTreeSet<_>>().len() != values.len()
    {
        return Err(gate(code, "identities must be non-empty and unique"));
    }
    Ok(())
}

fn immutable_image(value: &str) -> bool {
    let Some((repository, digest_value)) = value.rsplit_once('@') else {
        return false;
    };
    repository.contains('/') && !repository.contains(char::is_whitespace) && digest(digest_value)
}

fn digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

fn relative_path(root: &Path, path: &Path) -> Result<String, AppError> {
    let root = root.canonicalize().map_err(|error| {
        gate(
            "LW_RELEASE_GATE_EVIDENCE_LOCATOR_INVALID",
            &error.to_string(),
        )
    })?;
    let path = path.canonicalize().map_err(|error| {
        gate(
            "LW_RELEASE_GATE_EVIDENCE_LOCATOR_INVALID",
            &error.to_string(),
        )
    })?;
    path.strip_prefix(root)
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
        .map_err(|error| {
            gate(
                "LW_RELEASE_GATE_EVIDENCE_LOCATOR_INVALID",
                &error.to_string(),
            )
        })
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

    use serde_json::json;
    use sha2::{Digest, Sha256};
    use tempfile::tempdir;

    use super::{REQUIRED_CHECKS, run_with_locator};

    const INPUT_SCHEMA: &str =
        include_str!("../../schemas/results/sprint2-release-gate-input.v2.schema.json");
    const REPORT_SCHEMA: &str =
        include_str!("../../schemas/results/release-gate-report.v2.schema.json");
    const DEPLOYMENT_SCHEMA: &str =
        include_str!("../../schemas/results/platform-image-deployment-manifest.v1.schema.json");
    const RESOURCE_DEPLOYMENT_SCHEMA: &str =
        include_str!("../../schemas/results/resource-deployment-manifest.v1.schema.json");

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one isolated repository fixture proves pass, report creation, and post-run evidence tampering"
    )]
    fn same_identity_connected_evidence_passes_and_tamper_blocks()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempdir()?;
        let root = temporary.path();
        write(
            root,
            "schemas/results/sprint2-release-gate-input.v2.schema.json",
            INPUT_SCHEMA,
        )?;
        write(
            root,
            "schemas/results/release-gate-report.v2.schema.json",
            REPORT_SCHEMA,
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
        for name in REQUIRED_CHECKS {
            write(root, &format!("artifacts/evidence/{name}.json"), "{}\n")?;
        }
        let mut checks = Vec::new();
        for name in REQUIRED_CHECKS {
            let path = format!("artifacts/evidence/{name}.json");
            checks.push(json!({
                "name": name,
                "status": "passed",
                "mode": "connected",
                "sourceCommit": commit,
                "runId": run_id,
                "evidence": {"path": path, "sha256": file_hash(root, &path)?}
            }));
        }
        let images = ["control-service", "access-service", "agent-service", "environment-service", "evaluation-service", "openssh-gateway", "web"]
            .into_iter()
            .enumerate()
            .map(|(index, component)| json!({
                "component": component,
                "reference": format!("harbor.invalid/labweaver/{component}@sha256:{}", format!("{index:x}").repeat(64))
            }))
            .collect::<Vec<_>>();
        let migration_hash = file_hash(root, "migrations/catalog.yaml")?;
        let deployment = json!({
            "schema_version": "platform-image-deployment-manifest.v1",
            "environment": "test",
            "package_manifest_sha256": format!("sha256:{}", "c".repeat(64)),
            "source_commit": commit,
            "run_id": run_id,
            "cluster_uid": "fixture-cluster",
            "helm_revision": 1,
            "migration_catalog_sha256": migration_hash,
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
        let input = json!({
            "schemaVersion": "sprint2-release-gate-input.v2",
            "sourceCommit": commit,
            "runId": run_id,
            "deploymentManifest": {"path": "artifacts/evidence/deployment.json", "sha256": file_hash(root, "artifacts/evidence/deployment.json")?},
            "resourceDeploymentManifest": {"path": "artifacts/evidence/resource-deployment.json", "sha256": file_hash(root, "artifacts/evidence/resource-deployment.json")?},
            "migrationCatalog": {"path": "migrations/catalog.yaml", "sha256": migration_hash},
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

        write(
            root,
            "artifacts/evidence/access-negative.json",
            "tampered\n",
        )?;
        let error = match run_with_locator(root, "artifacts/gate-input.json") {
            Ok(()) => return Err("tamper unexpectedly passed".into()),
            Err(error) => error,
        };
        assert_eq!(
            error.diagnostic_code(),
            "LW_RELEASE_GATE_EVIDENCE_HASH_MISMATCH"
        );
        Ok(())
    }

    fn write(root: &Path, relative: &str, value: &str) -> std::io::Result<()> {
        let path = root.join(relative);
        let parent = path.parent().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "fixture path has no parent",
            )
        })?;
        fs::create_dir_all(parent)?;
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

    fn file_hash(root: &Path, relative: &str) -> std::io::Result<String> {
        Ok(format!(
            "sha256:{:x}",
            Sha256::digest(fs::read(root.join(relative))?)
        ))
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
