//! Fail-closed platform image packaging and deployment workflow.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
#[cfg(target_os = "linux")]
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::process::Command;

use serde::{Deserialize, Serialize};
#[cfg(target_os = "linux")]
use sha2::{Digest, Sha256};

use super::AppError;

const COMPONENTS: [&str; 7] = [
    "access-service",
    "agent-service",
    "control-service",
    "environment-service",
    "evaluation-service",
    "openssh-gateway",
    "web",
];
const PACKAGE_SCHEMA: &str = "platform-image-package-manifest.v1";
#[cfg(target_os = "linux")]
const DEPLOYMENT_SCHEMA: &str = "platform-image-deployment-manifest.v1";

#[cfg(target_os = "linux")]
#[derive(Debug, Deserialize)]
struct VersionLock {
    platform_images: PlatformImageLock,
    sprint2_foundation: Sprint2FoundationLock,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Deserialize)]
struct Sprint2FoundationLock {
    buildkit_rootless: String,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Deserialize)]
struct PlatformImageLock {
    platform: String,
    rust_toolchain: String,
    buildkit: String,
    buildkit_image: String,
    buildx: String,
    trivy: String,
    helm: String,
    claude_code: String,
    claude_code_linux_x64_sha512: String,
    ci_images: CiImageLock,
    bases: BaseImageLock,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Deserialize)]
struct CiImageLock {
    trivy: String,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Deserialize)]
struct BaseImageLock {
    rust_builder: String,
    rust_runtime: String,
    node_builder: String,
    web_runtime: String,
    gateway_builder: String,
    gateway_runtime: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct PackageManifest {
    schema_version: String,
    run_id: String,
    release_id: String,
    source_commit: String,
    source_date_epoch: u64,
    component_lock_hash: String,
    platform: String,
    registry: String,
    builder: BuilderIdentity,
    images: Vec<ImageEvidence>,
    overall: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct BuilderIdentity {
    buildkit: String,
    buildx: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ImageEvidence {
    component: String,
    reference: String,
    digest: String,
    scan: ScanEvidence,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ScanEvidence {
    scanner: String,
    database_digest: String,
    critical: u64,
    high: u64,
    report: String,
    report_sha256: String,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Serialize)]
struct DeploymentManifest<'a> {
    schema_version: &'static str,
    environment: &'a str,
    package_manifest_sha256: String,
    source_commit: &'a str,
    run_id: String,
    cluster_uid: String,
    helm_revision: u64,
    migration_catalog_sha256: String,
    images: Vec<DeploymentImage<'a>>,
    previous_verified_manifest_sha256: Option<String>,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Serialize)]
struct DeploymentImage<'a> {
    component: &'a str,
    reference: &'a str,
}

pub(crate) fn validate(
    manifest_path: &Path,
    connected: bool,
    environment: Option<&str>,
    root: &Path,
) -> Result<(), AppError> {
    let manifest = read_manifest(manifest_path)?;
    validate_manifest(&manifest)?;
    validate_schema_file(root, "platform-image-package-manifest.v1.schema.json")?;
    if connected {
        let environment = environment.ok_or(AppError::InvalidArgument {
            role: "connected package validation environment",
        })?;
        validate_environment(environment)?;
        connected_validate(&manifest, root)?;
    }
    Ok(())
}

pub(crate) fn package(
    environment: &str,
    release: &str,
    yes: bool,
    root: &Path,
) -> Result<(), AppError> {
    if !yes {
        return Err(AppError::ConfirmationRequired { command: "package" });
    }
    validate_environment(environment)?;
    validate_release(release)?;
    #[cfg(not(target_os = "linux"))]
    {
        let _ = root;
        Err(AppError::UnsupportedPlatform { command: "package" })
    }
    #[cfg(target_os = "linux")]
    {
        package_linux(environment, release, root)
    }
}

pub(crate) fn deploy(environment: &str, manifest_path: &Path, root: &Path) -> Result<(), AppError> {
    validate_environment(environment)?;
    let manifest = read_manifest(manifest_path)?;
    validate_manifest(&manifest)?;
    #[cfg(not(target_os = "linux"))]
    {
        let _ = root;
        Err(AppError::UnsupportedPlatform { command: "deploy" })
    }
    #[cfg(target_os = "linux")]
    {
        deploy_linux(environment, manifest_path, &manifest, root)
    }
}

pub(crate) fn rollback(
    environment: &str,
    release_revision: &str,
    yes: bool,
    root: &Path,
) -> Result<(), AppError> {
    if !yes {
        return Err(AppError::ConfirmationRequired {
            command: "rollback",
        });
    }
    validate_environment(environment)?;
    let revision = release_revision
        .parse::<u64>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or(AppError::InvalidArgument {
            role: "positive Helm release revision",
        })?;
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (revision, root);
        Err(AppError::UnsupportedPlatform {
            command: "rollback",
        })
    }
    #[cfg(target_os = "linux")]
    {
        rollback_linux(environment, revision, root)
    }
}

fn read_manifest(path: &Path) -> Result<PackageManifest, AppError> {
    let bytes = fs::read(path).map_err(|error| io_error("read package manifest", error))?;
    serde_json::from_slice(&bytes).map_err(|error| AppError::Io {
        role: "parse package manifest",
        detail: error.to_string(),
    })
}

fn validate_manifest(manifest: &PackageManifest) -> Result<(), AppError> {
    if manifest.schema_version != PACKAGE_SCHEMA
        || manifest.overall != "passed"
        || manifest.platform != "linux/amd64"
        || !is_commit(&manifest.source_commit)
        || !is_digest(&manifest.component_lock_hash)
    {
        return manifest_invalid("top-level identity is incomplete or incompatible");
    }
    validate_registry(&manifest.registry)?;
    let mut names = BTreeSet::new();
    for image in &manifest.images {
        if !COMPONENTS.contains(&image.component.as_str())
            || !names.insert(image.component.as_str())
        {
            return manifest_invalid("component set contains an unknown or duplicate name");
        }
        let expected = format!(
            "{}/labweaver-system/{}@{}",
            manifest.registry, image.component, image.digest
        );
        if !is_digest(&image.digest)
            || image.scan.critical != 0
            || !is_digest(&image.scan.database_digest)
            || !is_digest(&image.scan.report_sha256)
            || image.scan.report.is_empty()
            || !image.scan.report.starts_with("artifact://")
        {
            return manifest_invalid(
                "image evidence is incomplete, critical, or identity-mismatched",
            );
        }
        if image.reference != expected || image.reference.contains(":latest") {
            return manifest_invalid("image reference is not the expected Harbor digest reference");
        }
    }
    if names.len() != COMPONENTS.len() || COMPONENTS.iter().any(|name| !names.contains(name)) {
        return manifest_invalid(
            "manifest must contain exactly the seven Sprint 2 platform images",
        );
    }
    Ok(())
}

fn validate_schema_file(root: &Path, name: &str) -> Result<(), AppError> {
    let path = root.join("schemas/results").join(name);
    let value: serde_json::Value = serde_json::from_slice(
        &fs::read(&path).map_err(|error| io_error("read result schema", error))?,
    )
    .map_err(|error| AppError::Io {
        role: "parse result schema",
        detail: error.to_string(),
    })?;
    if value
        .get("$schema")
        .and_then(serde_json::Value::as_str)
        .is_none()
    {
        return Err(AppError::ContractDrift {
            path: path.to_string_lossy().into_owned(),
        });
    }
    Ok(())
}

fn validate_environment(value: &str) -> Result<(), AppError> {
    if portable_identifier(value, 32) {
        Ok(())
    } else {
        Err(AppError::InvalidArgument {
            role: "platform image environment",
        })
    }
}

fn validate_release(value: &str) -> Result<(), AppError> {
    if portable_identifier(value, 64) {
        Ok(())
    } else {
        Err(AppError::InvalidArgument {
            role: "platform image release",
        })
    }
}

fn validate_registry(value: &str) -> Result<(), AppError> {
    if !value.is_empty()
        && value.len() <= 253
        && !value.contains("//")
        && !value.contains('@')
        && !value.contains('/')
        && !value.contains(char::is_whitespace)
        && value.contains('.')
    {
        Ok(())
    } else {
        Err(AppError::InvalidArgument {
            role: "Harbor registry host",
        })
    }
}

fn portable_identifier(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn is_commit(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn is_digest(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn manifest_invalid(detail: &str) -> Result<(), AppError> {
    Err(AppError::PlatformImage {
        code: "LW_PACKAGE_MANIFEST_INVALID",
        detail: detail.to_owned(),
    })
}

fn io_error(role: &'static str, error: impl std::fmt::Display) -> AppError {
    AppError::Io {
        role,
        detail: error.to_string(),
    }
}

#[cfg(target_os = "linux")]
fn sha256(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

#[cfg(target_os = "linux")]
fn required_env(name: &'static str) -> Result<String, AppError> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or(AppError::PlatformImage {
            code: "LW_PACKAGE_CONFIGURATION_MISSING",
            detail: name.to_owned(),
        })
}

#[cfg(target_os = "linux")]
fn run_checked(command: &mut Command, role: &'static str) -> Result<String, AppError> {
    let output = command
        .output()
        .map_err(|error| AppError::ExternalCommand {
            role,
            code: None,
            detail: Some(error.to_string()),
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.lines().last().map(str::to_owned);
        return Err(AppError::ExternalCommand {
            role,
            code: output.status.code(),
            detail,
        });
    }
    String::from_utf8(output.stdout).map_err(|error| AppError::Io {
        role: "decode external command output",
        detail: error.to_string(),
    })
}

#[cfg(target_os = "linux")]
fn package_linux(environment: &str, release: &str, root: &Path) -> Result<(), AppError> {
    let source_commit = git_output(root, ["rev-parse", "HEAD"])?;
    if !is_commit(&source_commit) {
        return manifest_invalid("Git source commit is not a full lowercase SHA-1");
    }
    if !git_output(root, ["status", "--porcelain"])?.is_empty() {
        return Err(AppError::PlatformImage {
            code: "LW_PACKAGE_INPUT_DIRTY",
            detail: "package requires a clean tracked and untracked source tree".to_owned(),
        });
    }
    let source_date_epoch = git_output(root, ["show", "-s", "--format=%ct", "HEAD"])?
        .parse::<u64>()
        .map_err(|error| AppError::Io {
            role: "read source timestamp",
            detail: error.to_string(),
        })?;
    let lock_bytes = fs::read(root.join("deploy/versions.lock.yml"))
        .map_err(|error| io_error("read component lock", error))?;
    let lock: VersionLock = serde_yaml::from_slice(&lock_bytes).map_err(|error| AppError::Io {
        role: "parse component lock",
        detail: error.to_string(),
    })?;
    verify_tools(&lock)?;
    verify_rust_toolchain(root, &lock.platform_images)?;
    let registry = required_env("LABWEAVER_PLATFORM_REGISTRY")?;
    validate_registry(&registry)?;
    let database_digest = verified_trivy_database()?;
    let run_id = format!("pkg-{environment}-{release}-{}", &source_commit[..12]);
    let run_dir = root.join("artifacts/package").join(&run_id);
    fs::create_dir_all(&run_dir)
        .map_err(|error| io_error("create package run directory", error))?;
    scan_build_context(root, &run_dir)?;
    let mut images = Vec::with_capacity(COMPONENTS.len());
    for component in COMPONENTS {
        images.push(build_scan(
            root,
            &run_dir,
            &registry,
            component,
            &source_commit,
            source_date_epoch,
            &database_digest,
            &lock.platform_images,
        )?);
    }
    let manifest = PackageManifest {
        schema_version: PACKAGE_SCHEMA.to_owned(),
        run_id,
        release_id: release.to_owned(),
        source_commit,
        source_date_epoch,
        component_lock_hash: sha256(&lock_bytes),
        platform: lock.platform_images.platform,
        registry,
        builder: BuilderIdentity {
            buildkit: lock.platform_images.buildkit,
            buildx: lock.platform_images.buildx,
        },
        images,
        overall: "passed".to_owned(),
    };
    validate_manifest(&manifest)?;
    let bytes = serde_jcs::to_vec(&manifest).map_err(|error| AppError::Io {
        role: "canonicalize package manifest",
        detail: error.to_string(),
    })?;
    let temporary = run_dir.join("PlatformImagePackageManifest.json.tmp");
    let destination = run_dir.join("PlatformImagePackageManifest.json");
    fs::write(&temporary, bytes)
        .map_err(|error| io_error("write temporary package manifest", error))?;
    fs::rename(temporary, destination).map_err(|error| io_error("publish package manifest", error))
}

#[cfg(target_os = "linux")]
fn scan_build_context(root: &Path, run_dir: &Path) -> Result<(), AppError> {
    let report = run_dir.join("trivy-build-context.json");
    run_checked(
        Command::new("trivy")
            .current_dir(root)
            .args([
                "fs",
                "--format",
                "json",
                "--scanners",
                "secret",
                "--exit-code",
                "0",
                "--no-progress",
                "--skip-dirs",
                ".git",
                "--skip-dirs",
                ".agents",
                "--skip-dirs",
                ".ansible",
                "--skip-dirs",
                ".codex",
                "--skip-dirs",
                ".github",
                "--skip-dirs",
                ".idea",
                "--skip-dirs",
                ".private",
                "--skip-dirs",
                ".pytest_cache",
                "--skip-dirs",
                ".tmp",
                "--skip-dirs",
                "agent_team",
                "--skip-dirs",
                "artifacts",
                "--skip-dirs",
                "deploy",
                "--skip-dirs",
                "docs",
                "--skip-dirs",
                "examples",
                "--skip-dirs",
                "schemas",
                "--skip-dirs",
                "scripts",
                "--skip-dirs",
                "target",
                "--skip-dirs",
                "tests",
                "--skip-dirs",
                "web/node_modules",
                "--skip-dirs",
                "web/dist",
                "--output",
            ])
            .arg(&report)
            .arg("."),
        "Trivy build-context secret scan",
    )?;
    let bytes = fs::read(report).map_err(|error| io_error("read context scan report", error))?;
    let (_, _, secrets) = vulnerability_counts(&bytes)?;
    if secrets != 0 {
        return Err(AppError::PlatformImage {
            code: "LW_PACKAGE_SECRET_DETECTED",
            detail: "build-context".to_owned(),
        });
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
fn build_scan(
    root: &Path,
    run_dir: &Path,
    registry: &str,
    component: &str,
    source_commit: &str,
    source_date_epoch: u64,
    database_digest: &str,
    lock: &PlatformImageLock,
) -> Result<ImageEvidence, AppError> {
    let tag = format!(
        "{registry}/labweaver-system/{component}:git-{}",
        &source_commit[..12]
    );
    let reproducibility_tag = format!("{tag}-repro");
    build_image(
        root,
        component,
        source_commit,
        source_date_epoch,
        &tag,
        registry,
        lock,
    )?;
    build_image(
        root,
        component,
        source_commit,
        source_date_epoch,
        &reproducibility_tag,
        registry,
        lock,
    )?;
    let first = inspect_platform_digest(&tag)?;
    let second = inspect_platform_digest(&reproducibility_tag)?;
    if first != second {
        return Err(AppError::PlatformImage {
            code: "LW_PACKAGE_BUILD_NOT_REPRODUCIBLE",
            detail: component.to_owned(),
        });
    }
    let reference = format!("{registry}/labweaver-system/{component}@{first}");
    let (scan_bytes, critical, high) = scan_image(run_dir, component, &reference)?;
    Ok(ImageEvidence {
        component: component.to_owned(),
        reference: reference.clone(),
        digest: first,
        scan: ScanEvidence {
            scanner: format!("trivy:{}", lock.trivy),
            database_digest: database_digest.to_owned(),
            critical,
            high,
            report: format!("artifact://package/{component}/trivy.json"),
            report_sha256: sha256(&scan_bytes),
        },
    })
}

#[cfg(target_os = "linux")]
fn scan_image(
    run_dir: &Path,
    component: &str,
    reference: &str,
) -> Result<(Vec<u8>, u64, u64), AppError> {
    let scan_path = scan_path(run_dir, component);
    run_checked(
        Command::new("trivy")
            .args([
                "image",
                "--format",
                "json",
                "--scanners",
                "vuln,secret",
                "--severity",
                "HIGH,CRITICAL",
                "--exit-code",
                "0",
                "--ignore-unfixed=false",
                "--output",
            ])
            .arg(&scan_path)
            .arg(reference),
        "Trivy image and secret scan",
    )?;
    let scan_bytes = fs::read(&scan_path).map_err(|error| io_error("read Trivy report", error))?;
    let (critical, high, secrets) = vulnerability_counts(&scan_bytes)?;
    if secrets != 0 {
        return Err(AppError::PlatformImage {
            code: "LW_PACKAGE_SECRET_DETECTED",
            detail: component.to_owned(),
        });
    }
    if critical != 0 {
        return Err(AppError::PlatformImage {
            code: "LW_PACKAGE_CRITICAL_VULNERABILITY",
            detail: component.to_owned(),
        });
    }
    Ok((scan_bytes, critical, high))
}

#[cfg(target_os = "linux")]
fn scan_path(run_dir: &Path, component: &str) -> PathBuf {
    run_dir.join(format!("trivy-{component}.json"))
}

#[cfg(target_os = "linux")]
fn build_image(
    root: &Path,
    component: &str,
    source_commit: &str,
    source_date_epoch: u64,
    tag: &str,
    registry: &str,
    lock: &PlatformImageLock,
) -> Result<(), AppError> {
    let file = match component {
        "web" => "containers/Containerfile.web",
        "openssh-gateway" => "access-gateway/Dockerfile",
        _ => "containers/Containerfile.rust",
    };
    let mut command = Command::new("docker-buildx");
    command.current_dir(root).args([
        "build",
        "--file",
        file,
        "--platform",
        "linux/amd64",
        "--provenance=false",
        "--output=type=registry,rewrite-timestamp=true,oci-mediatypes=true",
        "--build-arg",
        &format!("SOURCE_COMMIT={source_commit}"),
        "--build-arg",
        &format!("SOURCE_DATE_EPOCH={source_date_epoch}"),
    ]);
    if component != "web" && component != "openssh-gateway" {
        command.args(["--build-arg", &format!("SERVICE={component}")]);
        command.args([
            "--target",
            if component == "agent-service" {
                "agent-runtime"
            } else {
                "runtime"
            },
        ]);
    }
    if component != "web" {
        command.args([
            "--build-arg",
            &format!("RUST_TOOLCHAIN={}", lock.rust_toolchain),
        ]);
    }
    if component == "agent-service" {
        command.args([
            "--build-arg",
            &format!("CLAUDE_CODE_VERSION={}", lock.claude_code),
            "--build-arg",
            &format!(
                "CLAUDE_CODE_LINUX_X64_SHA512={}",
                lock.claude_code_linux_x64_sha512
            ),
        ]);
    }
    command.args(["--tag", tag]);
    for (name, source) in build_base_images(component, lock) {
        command.args([
            "--build-arg",
            &format!("{name}={}", pinned_mirror(registry, name, source)?),
        ]);
    }
    command.arg(".");
    run_checked(&mut command, "BuildKit platform image build").map(|_| ())
}

#[cfg(target_os = "linux")]
fn build_base_images<'a>(
    component: &str,
    lock: &'a PlatformImageLock,
) -> Vec<(&'static str, &'a str)> {
    match component {
        "web" => vec![
            ("NODE_BUILDER", lock.bases.node_builder.as_str()),
            ("WEB_RUNTIME", lock.bases.web_runtime.as_str()),
        ],
        "openssh-gateway" => vec![
            ("RUST_BUILDER", lock.bases.gateway_builder.as_str()),
            ("GATEWAY_RUNTIME", lock.bases.gateway_runtime.as_str()),
        ],
        "agent-service" => vec![
            ("RUST_BUILDER", lock.bases.rust_builder.as_str()),
            ("RUST_RUNTIME", lock.bases.rust_runtime.as_str()),
            ("NODE_BUILDER", lock.bases.node_builder.as_str()),
            ("BUILDKIT_IMAGE", lock.buildkit_image.as_str()),
            ("TRIVY_IMAGE", lock.ci_images.trivy.as_str()),
        ],
        _ => vec![
            ("RUST_BUILDER", lock.bases.rust_builder.as_str()),
            ("RUST_RUNTIME", lock.bases.rust_runtime.as_str()),
            ("BUILDKIT_IMAGE", lock.buildkit_image.as_str()),
            ("TRIVY_IMAGE", lock.ci_images.trivy.as_str()),
        ],
    }
}

#[cfg(any(target_os = "linux", test))]
fn pinned_mirror(registry: &str, name: &str, source: &str) -> Result<String, AppError> {
    let (_, digest) = source.rsplit_once('@').ok_or(AppError::PlatformImage {
        code: "LW_PACKAGE_MANIFEST_INVALID",
        detail: format!("{name} base image is not digest pinned"),
    })?;
    if !is_digest(digest) {
        return Err(AppError::PlatformImage {
            code: "LW_PACKAGE_MANIFEST_INVALID",
            detail: format!("{name} base image digest is invalid"),
        });
    }
    Ok(format!(
        "{registry}/labweaver-system/base-{}@{digest}",
        name.to_ascii_lowercase().replace('_', "-")
    ))
}

#[cfg(target_os = "linux")]
fn inspect_digest(reference: &str) -> Result<String, AppError> {
    let output = run_checked(
        Command::new("docker-buildx").args(["imagetools", "inspect", reference]),
        "inspect OCI image",
    )?;
    let digest = output
        .lines()
        .find_map(|line| line.trim().strip_prefix("Digest:").map(str::trim))
        .ok_or(AppError::PlatformImage {
            code: "LW_PACKAGE_DIGEST_MISSING",
            detail: reference.to_owned(),
        })?;
    if is_digest(digest) {
        Ok(digest.to_owned())
    } else {
        manifest_invalid("registry returned an invalid digest").and(Ok(String::new()))
    }
}

#[cfg(target_os = "linux")]
fn inspect_platform_digest(reference: &str) -> Result<String, AppError> {
    let output = run_checked(
        Command::new("docker-buildx").args([
            "imagetools",
            "inspect",
            reference,
            "--format",
            "{{json .Manifest}}",
        ]),
        "inspect OCI platform manifest",
    )?;
    let value: serde_json::Value =
        serde_json::from_str(&output).map_err(|error| AppError::PlatformImage {
            code: "LW_PACKAGE_DIGEST_MISSING",
            detail: error.to_string(),
        })?;
    platform_digest_from_manifest(&value, reference)
}

#[cfg(any(target_os = "linux", test))]
fn platform_digest_from_manifest(
    value: &serde_json::Value,
    reference: &str,
) -> Result<String, AppError> {
    let single_manifest = matches!(
        value.get("mediaType").and_then(serde_json::Value::as_str),
        Some(
            "application/vnd.oci.image.manifest.v1+json"
                | "application/vnd.docker.distribution.manifest.v2+json"
        )
    );
    if single_manifest {
        let digest = value
            .get("digest")
            .and_then(serde_json::Value::as_str)
            .ok_or(AppError::PlatformImage {
                code: "LW_PACKAGE_DIGEST_MISSING",
                detail: format!("{reference} single-platform manifest has no digest"),
            })?;
        if is_digest(digest) {
            return Ok(digest.to_owned());
        }
        return Err(AppError::PlatformImage {
            code: "LW_PACKAGE_DIGEST_MISSING",
            detail: "single-platform manifest digest is invalid".to_owned(),
        });
    }
    let digest = value
        .get("manifests")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .find(|descriptor| {
            descriptor
                .pointer("/platform/os")
                .and_then(serde_json::Value::as_str)
                == Some("linux")
                && descriptor
                    .pointer("/platform/architecture")
                    .and_then(serde_json::Value::as_str)
                    == Some("amd64")
        })
        .and_then(|descriptor| descriptor.get("digest"))
        .and_then(serde_json::Value::as_str)
        .ok_or(AppError::PlatformImage {
            code: "LW_PACKAGE_DIGEST_MISSING",
            detail: format!("{reference} has no linux/amd64 subject manifest"),
        })?;
    if !is_digest(digest) {
        return Err(AppError::PlatformImage {
            code: "LW_PACKAGE_DIGEST_MISSING",
            detail: "platform manifest digest is invalid".to_owned(),
        });
    }
    Ok(digest.to_owned())
}

#[cfg(target_os = "linux")]
fn verified_trivy_database() -> Result<String, AppError> {
    let reference = required_env("LABWEAVER_TRIVY_DATABASE_REFERENCE")?;
    let expected = required_env("LABWEAVER_TRIVY_DATABASE_DIGEST")?;
    if !is_digest(&expected) || !reference.ends_with(&format!("@{expected}")) {
        return Err(AppError::PlatformImage {
            code: "LW_PACKAGE_MANIFEST_INVALID",
            detail: "Trivy database must be an exact OCI digest reference".to_owned(),
        });
    }
    if inspect_digest(&reference)? != expected {
        return Err(AppError::PlatformImage {
            code: "LW_PACKAGE_CONNECTED_IDENTITY_MISMATCH",
            detail: "Trivy database registry identity differs from configuration".to_owned(),
        });
    }
    Ok(expected)
}

#[cfg(target_os = "linux")]
fn vulnerability_counts(bytes: &[u8]) -> Result<(u64, u64, u64), AppError> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|error| AppError::Io {
        role: "parse Trivy report",
        detail: error.to_string(),
    })?;
    let mut critical = 0;
    let mut high = 0;
    let mut secrets = 0;
    for result in value
        .get("Results")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        for vulnerability in result
            .get("Vulnerabilities")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
        {
            match vulnerability
                .get("Severity")
                .and_then(serde_json::Value::as_str)
            {
                Some("CRITICAL") => critical += 1,
                Some("HIGH") => high += 1,
                _ => {}
            }
        }
        secrets += result
            .get("Secrets")
            .and_then(serde_json::Value::as_array)
            .map_or(0, |items| u64::try_from(items.len()).unwrap_or(u64::MAX));
    }
    Ok((critical, high, secrets))
}

#[cfg(target_os = "linux")]
fn verify_tools(lock: &VersionLock) -> Result<(), AppError> {
    let platform = &lock.platform_images;
    let checks = [
        ("docker-buildx", vec!["version"], platform.buildx.as_str()),
        ("trivy", vec!["--version"], platform.trivy.as_str()),
        ("helm", vec!["version", "--short"], platform.helm.as_str()),
    ];
    for (program, arguments, expected) in checks {
        let output = run_checked(
            Command::new(program).args(arguments),
            "verify locked tool identity",
        )?;
        if !output.contains(expected) {
            return Err(AppError::PlatformImage {
                code: "LW_PACKAGE_TOOL_IDENTITY_MISMATCH",
                detail: format!("{program} does not match {expected}"),
            });
        }
    }
    let buildkit = run_checked(
        Command::new("docker-buildx").args(["inspect", "--bootstrap"]),
        "verify BuildKit daemon identity",
    )?;
    if !buildkit.contains(&platform.buildkit) {
        return Err(AppError::PlatformImage {
            code: "LW_PACKAGE_TOOL_IDENTITY_MISMATCH",
            detail: format!("BuildKit does not match {}", platform.buildkit),
        });
    }
    if buildkit.contains(&platform.buildkit_image) {
        return Ok(());
    }

    verify_remote_buildkit_deployment(&lock.sprint2_foundation.buildkit_rootless)
}

#[cfg(target_os = "linux")]
fn verify_rust_toolchain(root: &Path, platform: &PlatformImageLock) -> Result<(), AppError> {
    let toolchain = fs::read_to_string(root.join("rust-toolchain.toml"))
        .map_err(|error| io_error("read Rust toolchain lock", error))?;
    let expected = format!("channel = \"{}\"", platform.rust_toolchain);
    let channels = toolchain
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("channel"))
        .collect::<Vec<_>>();
    let builder_marker = format!("rust:{}-", platform.rust_toolchain);
    if channels != [expected.as_str()]
        || !platform.bases.rust_builder.contains(&builder_marker)
        || !platform.bases.gateway_builder.contains(&builder_marker)
    {
        return Err(AppError::PlatformImage {
            code: "LW_PACKAGE_RUST_TOOLCHAIN_IDENTITY_MISMATCH",
            detail: "rust-toolchain.toml and both locked Rust builder images must use the same explicit version".to_owned(),
        });
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn verify_remote_buildkit_deployment(expected_image: &str) -> Result<(), AppError> {
    let kubeconfig = required_env("LABWEAVER_KUBECONFIG")?;
    let output = run_checked(
        Command::new("kubectl").args([
            "--kubeconfig",
            &kubeconfig,
            "--namespace",
            "labweaver-build",
            "get",
            "deployment",
            "buildkit",
            "--output",
            "json",
        ]),
        "read remote BuildKit deployment identity",
    )?;
    let value: serde_json::Value = serde_json::from_str(&output).map_err(|error| AppError::Io {
        role: "parse remote BuildKit deployment identity",
        detail: error.to_string(),
    })?;
    let image = value
        .pointer("/spec/template/spec/containers/0/image")
        .and_then(serde_json::Value::as_str);
    let configured = value
        .pointer("/spec/template/metadata/annotations/labweaver.io~1configuration-sha256")
        .and_then(serde_json::Value::as_str);
    let ready = value
        .pointer("/status/readyReplicas")
        .and_then(serde_json::Value::as_u64);
    let updated = value
        .pointer("/status/updatedReplicas")
        .and_then(serde_json::Value::as_u64);
    let configuration_is_sha256 = configured.is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    });
    if image != Some(expected_image)
        || ready != Some(1)
        || updated != Some(1)
        || !configuration_is_sha256
    {
        return Err(AppError::PlatformImage {
            code: "LW_PACKAGE_TOOL_IDENTITY_MISMATCH",
            detail: "remote BuildKit deployment image, configuration, or readiness differs from the component lock".to_owned(),
        });
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn connected_validate(manifest: &PackageManifest, root: &Path) -> Result<(), AppError> {
    let lock_bytes = fs::read(root.join("deploy/versions.lock.yml"))
        .map_err(|error| io_error("read component lock", error))?;
    let lock: VersionLock = serde_yaml::from_slice(&lock_bytes).map_err(|error| AppError::Io {
        role: "parse component lock",
        detail: error.to_string(),
    })?;
    verify_tools(&lock)?;
    verify_rust_toolchain(root, &lock.platform_images)?;
    if sha256(&lock_bytes) != manifest.component_lock_hash {
        return manifest_invalid("component lock identity changed");
    }
    let database = verified_trivy_database()?;
    if manifest
        .images
        .iter()
        .any(|image| image.scan.database_digest != database)
    {
        return Err(AppError::PlatformImage {
            code: "LW_PACKAGE_CONNECTED_IDENTITY_MISMATCH",
            detail: "scanner database identity differs from package evidence".to_owned(),
        });
    }
    for image in &manifest.images {
        if inspect_digest(&image.reference)? != image.digest {
            return manifest_invalid("registry digest no longer matches package evidence");
        }
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn connected_validate(_manifest: &PackageManifest, _root: &Path) -> Result<(), AppError> {
    Err(AppError::UnsupportedPlatform {
        command: "package-validate --mode connected",
    })
}

#[cfg(target_os = "linux")]
fn deploy_linux(
    environment: &str,
    manifest_path: &Path,
    manifest: &PackageManifest,
    root: &Path,
) -> Result<(), AppError> {
    connected_validate(manifest, root)?;
    let kubeconfig = required_env("LABWEAVER_KUBECONFIG")?;
    let values = root.join("deploy/helm/labweaver/values.yaml");
    let environment_values = PathBuf::from(required_env("LABWEAVER_PLATFORM_VALUES_FILE")?);
    let mut command = Command::new("helm");
    command
        .env("KUBECONFIG", &kubeconfig)
        .args([
            "upgrade",
            "--install",
            "labweaver",
            "deploy/helm/labweaver",
            "--namespace",
            "labweaver-system",
            "--create-namespace",
            "--atomic",
            "--wait",
            "--timeout",
            "10m",
            "--values",
        ])
        .arg(values)
        .arg("--values")
        .arg(environment_values);
    for image in &manifest.images {
        command.args([
            "--set-string",
            &format!(
                "images.{}={}",
                image.component.replace('-', "_"),
                image.reference
            ),
        ]);
    }
    run_checked(&mut command, "Helm platform rollout")?;
    let cluster_uid = cluster_uid(&kubeconfig)?;
    let revision = helm_revision(&kubeconfig)?;
    let manifest_bytes =
        fs::read(manifest_path).map_err(|error| io_error("read package manifest", error))?;
    let run_id = std::env::var("LABWEAVER_RUN_ID").map_err(|_| AppError::PlatformImage {
        code: "LW_PACKAGE_DEPLOYMENT_RUN_ID_MISSING",
        detail: "LABWEAVER_RUN_ID is required".to_owned(),
    })?;
    uuid::Uuid::parse_str(&run_id).map_err(|_| AppError::PlatformImage {
        code: "LW_PACKAGE_DEPLOYMENT_RUN_ID_INVALID",
        detail: "LABWEAVER_RUN_ID must be a UUID".to_owned(),
    })?;
    let migration_catalog = fs::read(root.join("migrations/catalog.yaml"))
        .map_err(|error| io_error("read migration catalog", error))?;
    let deployment = DeploymentManifest {
        schema_version: DEPLOYMENT_SCHEMA,
        environment,
        package_manifest_sha256: sha256(&manifest_bytes),
        source_commit: &manifest.source_commit,
        run_id,
        cluster_uid,
        helm_revision: revision,
        migration_catalog_sha256: sha256(&migration_catalog),
        images: manifest
            .images
            .iter()
            .map(|image| DeploymentImage {
                component: &image.component,
                reference: &image.reference,
            })
            .collect(),
        previous_verified_manifest_sha256: std::env::var(
            "LABWEAVER_PREVIOUS_PLATFORM_DEPLOYMENT_MANIFEST_SHA256",
        )
        .ok(),
    };
    let bytes = serde_jcs::to_vec(&deployment).map_err(|error| AppError::Io {
        role: "canonicalize deployment manifest",
        detail: error.to_string(),
    })?;
    let output = root
        .join("artifacts/deployment")
        .join(format!("platform-{environment}-{revision}.json"));
    let parent = output.parent().ok_or(AppError::Io {
        role: "resolve deployment manifest parent",
        detail: output.display().to_string(),
    })?;
    fs::create_dir_all(parent)
        .map_err(|error| io_error("create deployment evidence directory", error))?;
    fs::write(output, bytes).map_err(|error| io_error("write deployment manifest", error))
}

#[cfg(target_os = "linux")]
fn cluster_uid(kubeconfig: &str) -> Result<String, AppError> {
    let uid = run_checked(
        Command::new("kubectl").env("KUBECONFIG", kubeconfig).args([
            "get",
            "namespace",
            "kube-system",
            "--output",
            "jsonpath={.metadata.uid}",
        ]),
        "read cluster UID",
    )?;
    if uid.trim().is_empty() {
        return Err(AppError::PlatformImage {
            code: "LW_PACKAGE_CLUSTER_IDENTITY_MISSING",
            detail: "kube-system namespace UID is empty".to_owned(),
        });
    }
    Ok(uid)
}

#[cfg(target_os = "linux")]
fn rollback_linux(environment: &str, revision: u64, root: &Path) -> Result<(), AppError> {
    let manifest_path = PathBuf::from(required_env("LABWEAVER_PLATFORM_ROLLBACK_MANIFEST")?);
    let manifest = read_manifest(&manifest_path)?;
    validate_manifest(&manifest)?;
    connected_validate(&manifest, root)?;
    let kubeconfig = required_env("LABWEAVER_KUBECONFIG")?;
    run_checked(
        Command::new("helm").env("KUBECONFIG", kubeconfig).args([
            "rollback",
            "labweaver",
            &revision.to_string(),
            "--namespace",
            "labweaver-system",
            "--wait",
            "--timeout",
            "10m",
        ]),
        "Helm verified digest rollback",
    )?;
    let _ = environment;
    Ok(())
}

#[cfg(target_os = "linux")]
fn helm_revision(kubeconfig: &str) -> Result<u64, AppError> {
    let output = run_checked(
        Command::new("helm").env("KUBECONFIG", kubeconfig).args([
            "status",
            "labweaver",
            "--namespace",
            "labweaver-system",
            "--output",
            "json",
        ]),
        "read Helm rollout identity",
    )?;
    let value: serde_json::Value = serde_json::from_str(&output).map_err(|error| AppError::Io {
        role: "parse Helm rollout identity",
        detail: error.to_string(),
    })?;
    value
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .filter(|revision| *revision > 0)
        .ok_or(AppError::PlatformImage {
            code: "LW_PACKAGE_DEPLOYMENT_REVISION_MISSING",
            detail: "Helm status did not contain a positive revision".to_owned(),
        })
}

#[cfg(target_os = "linux")]
fn git_output<const N: usize>(root: &Path, arguments: [&str; N]) -> Result<String, AppError> {
    run_checked(
        Command::new("git").current_dir(root).args(arguments),
        "read Git package identity",
    )
    .map(|value| value.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn digest(character: char) -> String {
        format!("sha256:{}", character.to_string().repeat(64))
    }

    fn valid_manifest() -> PackageManifest {
        PackageManifest {
            schema_version: PACKAGE_SCHEMA.to_owned(),
            run_id: "pkg-test-0001".to_owned(),
            release_id: "test-0001".to_owned(),
            source_commit: "a".repeat(40),
            source_date_epoch: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(1, |duration| duration.as_secs()),
            component_lock_hash: digest('b'),
            platform: "linux/amd64".to_owned(),
            registry: "harbor.internal.example".to_owned(),
            builder: BuilderIdentity {
                buildkit: "v0.31.1".to_owned(),
                buildx: "v0.35.0".to_owned(),
            },
            images: COMPONENTS
                .iter()
                .enumerate()
                .map(|(index, component)| {
                    let digest_char = ['3', '4', '5', '6', '7', '8', '9'][index];
                    let image_digest = digest(digest_char);
                    let reference = format!(
                        "harbor.internal.example/labweaver-system/{component}@{image_digest}"
                    );
                    ImageEvidence {
                        component: (*component).to_owned(),
                        reference: reference.clone(),
                        digest: image_digest,
                        scan: ScanEvidence {
                            scanner: "trivy:0.72.0".to_owned(),
                            database_digest: digest('1'),
                            critical: 0,
                            high: 2,
                            report: format!("artifact://package/{component}/trivy.json"),
                            report_sha256: digest('2'),
                        },
                    }
                })
                .collect(),
            overall: "passed".to_owned(),
        }
    }

    #[test]
    fn static_manifest_accepts_exact_complete_digest_set() {
        assert!(validate_manifest(&valid_manifest()).is_ok());
    }

    #[test]
    fn static_manifest_rejects_missing_duplicate_and_external_images() {
        let mut missing = valid_manifest();
        missing.images.pop();
        assert!(validate_manifest(&missing).is_err());

        let mut duplicate = valid_manifest();
        duplicate.images[1] = duplicate.images[0].clone();
        assert!(validate_manifest(&duplicate).is_err());

        let mut external = valid_manifest();
        external.images[0].reference = format!(
            "external.example/labweaver-system/access-service@{}",
            external.images[0].digest
        );
        assert!(validate_manifest(&external).is_err());
    }

    #[test]
    fn static_manifest_rejects_critical_or_invalid_scan_evidence() {
        let mut critical = valid_manifest();
        critical.images[0].scan.critical = 1;
        assert!(validate_manifest(&critical).is_err());

        let mut unpinned_database = valid_manifest();
        unpinned_database.images[0].scan.database_digest = "latest".to_owned();
        assert!(validate_manifest(&unpinned_database).is_err());

        let mut unbound_report = valid_manifest();
        unbound_report.images[0].scan.report = "trivy.json".to_owned();
        assert!(validate_manifest(&unbound_report).is_err());
    }

    #[test]
    fn platform_digest_ignores_non_runtime_index_entries() -> Result<(), String> {
        let subject = digest('a');
        for auxiliary in [digest('b'), digest('c')] {
            let index = serde_json::json!({
                "manifests": [
                    {
                        "digest": subject,
                        "platform": {"os": "linux", "architecture": "amd64"}
                    },
                    {
                        "digest": auxiliary,
                        "platform": {"os": "unknown", "architecture": "unknown"}
                    }
                ]
            });
            let actual = platform_digest_from_manifest(&index, "fixture")
                .map_err(|error| error.to_string())?;
            assert_eq!(actual, subject);
        }
        Ok(())
    }

    #[test]
    fn platform_digest_accepts_single_platform_manifest_descriptor() -> Result<(), String> {
        let expected = digest('a');
        let manifest = serde_json::json!({
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "digest": expected,
            "size": 3978
        });
        let actual = platform_digest_from_manifest(&manifest, "fixture")
            .map_err(|error| error.to_string())?;
        assert_eq!(actual, expected);
        Ok(())
    }

    #[test]
    fn package_registry_requires_a_bare_harbor_host() {
        assert!(validate_registry("harbor.internal.example").is_ok());
        assert!(validate_registry("harbor.internal.example:5443").is_ok());
        assert!(validate_registry("https://harbor.internal.example").is_err());
        assert!(validate_registry("harbor.internal.example/project").is_err());
    }

    #[test]
    fn pinned_mirror_preserves_the_reviewed_digest() -> Result<(), String> {
        let expected = digest('a');
        let actual = pinned_mirror(
            "harbor.lab.lan",
            "RUST_BUILDER",
            &format!("docker.io/library/rust:locked@{expected}"),
        )
        .map_err(|error| error.to_string())?;
        assert_eq!(
            actual,
            format!("harbor.lab.lan/labweaver-system/base-rust-builder@{expected}")
        );
        assert!(pinned_mirror("harbor.lab.lan", "RUST_BUILDER", "rust:latest").is_err());
        Ok(())
    }

    #[test]
    fn rust_container_build_context_includes_every_workspace_member() -> std::io::Result<()> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let dockerignore = std::fs::read_to_string(root.join(".dockerignore"))?;
        let containerfile =
            std::fs::read_to_string(root.join("containers").join("Containerfile.rust"))?;
        for directory in ["access-gateway", "crates", "services", "xtask"] {
            assert!(
                dockerignore
                    .lines()
                    .any(|line| line == format!("!{directory}/**")),
                "{directory} must be included in the Docker build context"
            );
            assert!(
                containerfile.contains(&format!("COPY {directory} {directory}")),
                "{directory} must be copied by the Rust Containerfile"
            );
        }
        assert!(containerfile.contains("RUSTUP_TOOLCHAIN=${RUST_TOOLCHAIN}"));
        assert!(containerfile.contains("@anthropic-ai/claude-code-linux-x64@"));
        assert!(containerfile.contains("sha512sum --check --strict"));
        assert!(containerfile.contains("/usr/local/bin/claude"));
        assert!(
            std::fs::read_to_string(root.join("access-gateway/Dockerfile"))?
                .contains("RUSTUP_TOOLCHAIN=${RUST_TOOLCHAIN}")
        );
        Ok(())
    }

    #[test]
    fn image_ci_scans_the_extracted_oci_layout() -> std::io::Result<()> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let workflow = std::fs::read_to_string(
            root.join(".github")
                .join("workflows")
                .join("platform-images.yml"),
        )?;
        assert!(workflow.contains("tar -xf \"$RUNNER_TEMP/$COMPONENT-first.tar\""));
        assert!(workflow.contains("image --input \"/evidence/$COMPONENT-oci\""));
        assert!(workflow.contains("LW_PACKAGE_OCI_LAYOUT_INVALID"));
        Ok(())
    }
}
