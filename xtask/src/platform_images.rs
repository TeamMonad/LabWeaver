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
    "resource-service",
    "web",
];
const PACKAGE_SCHEMA: &str = "platform-image-package-manifest.v1";
#[cfg(target_os = "linux")]
const DEPLOYMENT_SCHEMA: &str = "platform-image-deployment-manifest.v1";

#[cfg(target_os = "linux")]
#[derive(Debug, Deserialize)]
struct VersionLock {
    platform_images: PlatformImageLock,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Deserialize)]
struct PlatformImageLock {
    platform: String,
    buildkit: String,
    buildkit_image: String,
    buildx: String,
    sbom_generator: String,
    trivy: String,
    cosign: String,
    kyverno_cli: String,
    helm: String,
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
    trust: TrustIdentity,
    images: Vec<ImageEvidence>,
    overall: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct BuilderIdentity {
    buildkit: String,
    buildx: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct TrustIdentity {
    bundle_sha256: String,
    revision: String,
    issuer: String,
    subject: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ImageEvidence {
    component: String,
    reference: String,
    digest: String,
    sbom: String,
    provenance: String,
    scan: ScanEvidence,
    signature: SignatureEvidence,
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

#[derive(Clone, Debug, Deserialize, Serialize)]
struct SignatureEvidence {
    bundle: String,
    certificate_identity: String,
    certificate_oidc_issuer: String,
    transparency_log_verified: bool,
    ct_verified: bool,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Serialize)]
struct DeploymentManifest<'a> {
    schema_version: &'static str,
    environment: &'a str,
    package_manifest_sha256: String,
    source_commit: &'a str,
    cluster_uid: String,
    helm_revision: u64,
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
        || !is_digest(&manifest.trust.bundle_sha256)
        || manifest.trust.revision.is_empty()
        || !manifest.trust.issuer.starts_with("https://")
        || manifest.trust.subject.is_empty()
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
        let locator_prefix = format!("oci://{expected}#");
        if !is_digest(&image.digest)
            || image.scan.critical != 0
            || !is_digest(&image.scan.database_digest)
            || !is_digest(&image.scan.report_sha256)
            || !image.signature.transparency_log_verified
            || !image.signature.ct_verified
            || image.signature.certificate_identity != manifest.trust.subject
            || image.signature.certificate_oidc_issuer != manifest.trust.issuer
            || image.sbom.is_empty()
            || image.provenance.is_empty()
            || image.scan.report.is_empty()
            || image.signature.bundle.is_empty()
            || !image.sbom.starts_with(&locator_prefix)
            || !image.provenance.starts_with(&locator_prefix)
            || !image.scan.report.starts_with(&locator_prefix)
            || !image.signature.bundle.starts_with(&locator_prefix)
        {
            return manifest_invalid(
                "image evidence is incomplete, critical, or identity-mismatched",
            );
        }
        if image.reference != expected || image.reference.contains(":latest") {
            return manifest_invalid("image reference is not the expected GHCR digest reference");
        }
    }
    if names.len() != COMPONENTS.len() || COMPONENTS.iter().any(|name| !names.contains(name)) {
        return manifest_invalid("manifest must contain exactly the seven platform images");
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
    if value == "ghcr.io/teammonad" {
        Ok(())
    } else {
        Err(AppError::InvalidArgument {
            role: "GitHub Packages registry namespace",
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
    verify_tools(&lock.platform_images)?;
    let registry = required_env("LABWEAVER_PLATFORM_REGISTRY")?;
    validate_registry(&registry)?;
    let trust_bundle = PathBuf::from(required_env("LABWEAVER_SIGSTORE_TRUST_BUNDLE")?);
    let trust_bytes =
        fs::read(&trust_bundle).map_err(|error| io_error("read trust bundle", error))?;
    let trust = TrustIdentity {
        bundle_sha256: sha256(&trust_bytes),
        revision: required_env("LABWEAVER_SIGSTORE_TRUST_REVISION")?,
        issuer: required_env("LABWEAVER_SIGSTORE_OIDC_ISSUER")?,
        subject: required_env("LABWEAVER_SIGSTORE_EXPECTED_SUBJECT")?,
    };
    let database_digest = verified_trivy_database()?;
    let run_id = format!("pkg-{environment}-{release}-{}", &source_commit[..12]);
    let run_dir = root.join("artifacts/package").join(&run_id);
    fs::create_dir_all(&run_dir)
        .map_err(|error| io_error("create package run directory", error))?;
    scan_build_context(root, &run_dir)?;
    let mut images = Vec::with_capacity(COMPONENTS.len());
    for component in COMPONENTS {
        images.push(build_scan_sign(
            root,
            &run_dir,
            &registry,
            component,
            &source_commit,
            source_date_epoch,
            &database_digest,
            &trust,
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
        trust,
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
fn build_scan_sign(
    root: &Path,
    run_dir: &Path,
    registry: &str,
    component: &str,
    source_commit: &str,
    source_date_epoch: u64,
    database_digest: &str,
    trust: &TrustIdentity,
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
        &lock.sbom_generator,
    )?;
    build_image(
        root,
        component,
        source_commit,
        source_date_epoch,
        &reproducibility_tag,
        &lock.sbom_generator,
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
    sign_and_attest(
        run_dir,
        component,
        &tag,
        &reference,
        &scan_path(run_dir, component),
        trust,
    )?;
    Ok(ImageEvidence {
        component: component.to_owned(),
        reference: reference.clone(),
        digest: first,
        sbom: format!("oci://{reference}#sbom-spdx"),
        provenance: format!("oci://{reference}#slsa-provenance"),
        scan: ScanEvidence {
            scanner: format!("trivy:{}", lock.trivy),
            database_digest: database_digest.to_owned(),
            critical,
            high,
            report: format!("oci://{reference}#trivy-vulnerability-report"),
            report_sha256: sha256(&scan_bytes),
        },
        signature: SignatureEvidence {
            bundle: format!("oci://{reference}#sigstore-bundle"),
            certificate_identity: trust.subject.clone(),
            certificate_oidc_issuer: trust.issuer.clone(),
            transparency_log_verified: true,
            ct_verified: true,
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
fn sign_and_attest(
    run_dir: &Path,
    component: &str,
    build_reference: &str,
    reference: &str,
    scan: &Path,
    trust: &TrustIdentity,
) -> Result<(), AppError> {
    let trusted_root = required_env("LABWEAVER_SIGSTORE_TRUST_BUNDLE")?;
    let fulcio = required_env("LABWEAVER_SIGSTORE_FULCIO_URL")?;
    let rekor = required_env("LABWEAVER_SIGSTORE_REKOR_URL")?;
    let token = required_env("LABWEAVER_SIGSTORE_IDENTITY_TOKEN_FILE")?;
    run_checked(
        Command::new("cosign").args([
            "sign",
            "--yes",
            "--use-signing-config=false",
            "--identity-token",
            &token,
            "--trusted-root",
            &trusted_root,
            "--fulcio-url",
            &fulcio,
            "--rekor-url",
            &rekor,
            reference,
        ]),
        "Cosign keyless sign",
    )?;
    let sbom_path = run_dir.join(format!("sbom-{component}.json"));
    let provenance_path = run_dir.join(format!("provenance-{component}.json"));
    extract_attestation(build_reference, "{{ json .SBOM.SPDX }}", &sbom_path)?;
    extract_attestation(
        build_reference,
        "{{ json .Provenance.SLSA }}",
        &provenance_path,
    )?;
    cosign_attest(reference, "spdxjson", &sbom_path)?;
    cosign_attest(reference, "slsaprovenance", &provenance_path)?;
    cosign_attest(reference, "vuln", scan)?;
    cosign_verify(reference, trust)
}

#[cfg(target_os = "linux")]
fn build_image(
    root: &Path,
    component: &str,
    source_commit: &str,
    source_date_epoch: u64,
    tag: &str,
    sbom_generator: &str,
) -> Result<(), AppError> {
    let file = if component == "web" {
        "containers/Containerfile.web"
    } else {
        "containers/Containerfile.rust"
    };
    let mut command = Command::new("docker");
    command.current_dir(root).args([
        "buildx",
        "build",
        "--file",
        file,
        "--platform",
        "linux/amd64",
        "--provenance=mode=max",
        "--attest",
        &format!("type=sbom,generator={sbom_generator}"),
        "--output=type=registry,rewrite-timestamp=true,oci-mediatypes=true",
        "--build-arg",
        &format!("SOURCE_COMMIT={source_commit}"),
        "--build-arg",
        &format!("SOURCE_DATE_EPOCH={source_date_epoch}"),
    ]);
    if component != "web" {
        command.args(["--build-arg", &format!("SERVICE={component}")]);
    }
    command.args(["--tag", tag, "."]);
    run_checked(&mut command, "BuildKit platform image build").map(|_| ())
}

#[cfg(target_os = "linux")]
fn inspect_digest(reference: &str) -> Result<String, AppError> {
    let output = run_checked(
        Command::new("docker").args(["buildx", "imagetools", "inspect", reference]),
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
        Command::new("docker").args([
            "buildx",
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
    platform_digest_from_index(&value, reference)
}

#[cfg(any(target_os = "linux", test))]
fn platform_digest_from_index(
    value: &serde_json::Value,
    reference: &str,
) -> Result<String, AppError> {
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
fn verify_tools(lock: &PlatformImageLock) -> Result<(), AppError> {
    let checks = [
        ("docker", vec!["buildx", "version"], lock.buildx.as_str()),
        ("trivy", vec!["--version"], lock.trivy.as_str()),
        ("cosign", vec!["version"], lock.cosign.as_str()),
        ("kyverno", vec!["version"], lock.kyverno_cli.as_str()),
        ("helm", vec!["version", "--short"], lock.helm.as_str()),
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
        Command::new("docker").args(["buildx", "inspect", "--bootstrap"]),
        "verify BuildKit daemon identity",
    )?;
    if !buildkit.contains(&lock.buildkit) {
        return Err(AppError::PlatformImage {
            code: "LW_PACKAGE_TOOL_IDENTITY_MISMATCH",
            detail: format!("BuildKit does not match {}", lock.buildkit),
        });
    }
    if !buildkit.contains(&lock.buildkit_image) {
        return Err(AppError::PlatformImage {
            code: "LW_PACKAGE_TOOL_IDENTITY_MISMATCH",
            detail: "BuildKit driver image digest differs from the component lock".to_owned(),
        });
    }
    let Some((_, expected_generator_digest)) = lock.sbom_generator.rsplit_once('@') else {
        return Err(AppError::PlatformImage {
            code: "LW_PACKAGE_TOOL_IDENTITY_MISMATCH",
            detail: "SBOM generator is not pinned by digest".to_owned(),
        });
    };
    if !is_digest(expected_generator_digest)
        || inspect_digest(&lock.sbom_generator)? != expected_generator_digest
    {
        return Err(AppError::PlatformImage {
            code: "LW_PACKAGE_TOOL_IDENTITY_MISMATCH",
            detail: "SBOM generator registry identity differs from the component lock".to_owned(),
        });
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn extract_attestation(reference: &str, format: &str, destination: &Path) -> Result<(), AppError> {
    let output = run_checked(
        Command::new("docker").args([
            "buildx",
            "imagetools",
            "inspect",
            reference,
            "--format",
            format,
        ]),
        "extract BuildKit attestation",
    )?;
    let value: serde_json::Value =
        serde_json::from_str(&output).map_err(|error| AppError::PlatformImage {
            code: "LW_PACKAGE_ATTESTATION_INVALID",
            detail: error.to_string(),
        })?;
    if value.is_null() {
        return Err(AppError::PlatformImage {
            code: "LW_PACKAGE_ATTESTATION_INVALID",
            detail: reference.to_owned(),
        });
    }
    fs::write(destination, output).map_err(|error| io_error("write BuildKit attestation", error))
}

#[cfg(target_os = "linux")]
fn cosign_attest(reference: &str, kind: &str, predicate: &Path) -> Result<(), AppError> {
    let trusted_root = required_env("LABWEAVER_SIGSTORE_TRUST_BUNDLE")?;
    let fulcio = required_env("LABWEAVER_SIGSTORE_FULCIO_URL")?;
    let rekor = required_env("LABWEAVER_SIGSTORE_REKOR_URL")?;
    let token = required_env("LABWEAVER_SIGSTORE_IDENTITY_TOKEN_FILE")?;
    run_checked(
        Command::new("cosign")
            .args([
                "attest",
                "--yes",
                "--use-signing-config=false",
                "--identity-token",
                &token,
                "--trusted-root",
                &trusted_root,
                "--fulcio-url",
                &fulcio,
                "--rekor-url",
                &rekor,
                "--type",
                kind,
                "--predicate",
            ])
            .arg(predicate)
            .arg(reference),
        "Cosign signed attestation publication",
    )
    .map(|_| ())
}

#[cfg(target_os = "linux")]
fn connected_validate(manifest: &PackageManifest, root: &Path) -> Result<(), AppError> {
    let lock_bytes = fs::read(root.join("deploy/versions.lock.yml"))
        .map_err(|error| io_error("read component lock", error))?;
    let lock: VersionLock = serde_yaml::from_slice(&lock_bytes).map_err(|error| AppError::Io {
        role: "parse component lock",
        detail: error.to_string(),
    })?;
    verify_tools(&lock.platform_images)?;
    if sha256(&lock_bytes) != manifest.component_lock_hash {
        return manifest_invalid("component lock identity changed");
    }
    verify_connected_evidence_identity(manifest)?;
    for image in &manifest.images {
        if inspect_digest(&image.reference)? != image.digest {
            return manifest_invalid("registry digest no longer matches package evidence");
        }
        cosign_verify(&image.reference, &manifest.trust)?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn verify_connected_evidence_identity(manifest: &PackageManifest) -> Result<(), AppError> {
    let trust_bundle = fs::read(required_env("LABWEAVER_SIGSTORE_TRUST_BUNDLE")?)
        .map_err(|error| io_error("read connected trust bundle", error))?;
    let revision = required_env("LABWEAVER_SIGSTORE_TRUST_REVISION")?;
    let issuer = required_env("LABWEAVER_SIGSTORE_OIDC_ISSUER")?;
    let subject = required_env("LABWEAVER_SIGSTORE_EXPECTED_SUBJECT")?;
    let database = verified_trivy_database()?;
    if sha256(&trust_bundle) != manifest.trust.bundle_sha256
        || revision != manifest.trust.revision
        || issuer != manifest.trust.issuer
        || subject != manifest.trust.subject
        || manifest
            .images
            .iter()
            .any(|image| image.scan.database_digest != database)
    {
        return Err(AppError::PlatformImage {
            code: "LW_PACKAGE_CONNECTED_IDENTITY_MISMATCH",
            detail: "trust or scanner identity differs from package evidence".to_owned(),
        });
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
fn cosign_verify(reference: &str, trust: &TrustIdentity) -> Result<(), AppError> {
    let trusted_root = required_env("LABWEAVER_SIGSTORE_TRUST_BUNDLE")?;
    run_checked(
        Command::new("cosign").args([
            "verify",
            "--trusted-root",
            &trusted_root,
            "--certificate-identity",
            &trust.subject,
            "--certificate-oidc-issuer",
            &trust.issuer,
            reference,
        ]),
        "Cosign signature and transparency verification",
    )?;
    for kind in ["spdxjson", "slsaprovenance", "vuln"] {
        run_checked(
            Command::new("cosign").args([
                "verify-attestation",
                "--trusted-root",
                &trusted_root,
                "--certificate-identity",
                &trust.subject,
                "--certificate-oidc-issuer",
                &trust.issuer,
                "--type",
                kind,
                reference,
            ]),
            "Cosign attestation verification",
        )?;
    }
    Ok(())
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
    let fulcio_roots = PathBuf::from(required_env("LABWEAVER_SIGSTORE_FULCIO_ROOTS_FILE")?);
    let rekor_url = required_env("LABWEAVER_SIGSTORE_REKOR_URL")?;
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
        .arg(environment_values)
        .arg("--set-file")
        .arg(format!("trust.fulcioRoots={}", fulcio_roots.display()))
        .args([
            "--set-string",
            &format!("trust.registry={}", manifest.registry),
            "--set-string",
            &format!("trust.revision={}", manifest.trust.revision),
            "--set-string",
            &format!("trust.issuer={}", manifest.trust.issuer),
            "--set-string",
            &format!("trust.subject={}", manifest.trust.subject),
            "--set-string",
            &format!("trust.rekorUrl={rekor_url}"),
        ]);
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
    let deployment = DeploymentManifest {
        schema_version: DEPLOYMENT_SCHEMA,
        environment,
        package_manifest_sha256: sha256(&manifest_bytes),
        source_commit: &manifest.source_commit,
        cluster_uid,
        helm_revision: revision,
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
            registry: "ghcr.io/teammonad".to_owned(),
            builder: BuilderIdentity {
                buildkit: "v0.31.1".to_owned(),
                buildx: "v0.35.0".to_owned(),
            },
            trust: TrustIdentity {
                bundle_sha256: digest('c'),
                revision: "trust-v1".to_owned(),
                issuer: "https://identity.internal.example/realms/labweaver".to_owned(),
                subject: "system:serviceaccount:labweaver-build:platform-builder".to_owned(),
            },
            images: COMPONENTS
                .iter()
                .enumerate()
                .map(|(index, component)| {
                    let digest_char = ['3', '4', '5', '6', '7', '8', '9'][index];
                    let image_digest = digest(digest_char);
                    let reference =
                        format!("ghcr.io/teammonad/labweaver-system/{component}@{image_digest}");
                    ImageEvidence {
                        component: (*component).to_owned(),
                        reference: reference.clone(),
                        digest: image_digest,
                        sbom: format!("oci://{reference}#sbom-spdx"),
                        provenance: format!("oci://{reference}#slsa-provenance"),
                        scan: ScanEvidence {
                            scanner: "trivy:0.72.0".to_owned(),
                            database_digest: digest('1'),
                            critical: 0,
                            high: 2,
                            report: format!("oci://{reference}#trivy-vulnerability-report"),
                            report_sha256: digest('2'),
                        },
                        signature: SignatureEvidence {
                            bundle: format!("oci://{reference}#sigstore-bundle"),
                            certificate_identity:
                                "system:serviceaccount:labweaver-build:platform-builder".to_owned(),
                            certificate_oidc_issuer:
                                "https://identity.internal.example/realms/labweaver".to_owned(),
                            transparency_log_verified: true,
                            ct_verified: true,
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
    fn static_manifest_rejects_critical_or_unverified_evidence() {
        let mut critical = valid_manifest();
        critical.images[0].scan.critical = 1;
        assert!(validate_manifest(&critical).is_err());

        let mut unsigned = valid_manifest();
        unsigned.images[0].signature.transparency_log_verified = false;
        assert!(validate_manifest(&unsigned).is_err());

        let mut wrong_subject = valid_manifest();
        wrong_subject.images[0].signature.certificate_identity = "someone-else".to_owned();
        assert!(validate_manifest(&wrong_subject).is_err());
    }

    #[test]
    fn platform_digest_ignores_run_specific_attestation_manifest() -> Result<(), String> {
        let subject = digest('a');
        for attestation in [digest('b'), digest('c')] {
            let index = serde_json::json!({
                "manifests": [
                    {
                        "digest": subject,
                        "platform": {"os": "linux", "architecture": "amd64"}
                    },
                    {
                        "digest": attestation,
                        "platform": {"os": "unknown", "architecture": "unknown"}
                    }
                ]
            });
            let actual =
                platform_digest_from_index(&index, "fixture").map_err(|error| error.to_string())?;
            assert_eq!(actual, subject);
        }
        Ok(())
    }

    #[test]
    fn package_registry_is_exactly_the_github_organization_namespace() {
        assert!(validate_registry("ghcr.io/teammonad").is_ok());
        assert!(validate_registry("ghcr.io/other-owner").is_err());
        assert!(validate_registry("harbor.internal.example").is_err());
    }
}
