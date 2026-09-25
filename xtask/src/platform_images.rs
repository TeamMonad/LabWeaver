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
use sha2::{Digest, Sha256, Sha512};

use super::AppError;

const IMAGE_COMPONENTS: [&str; 9] = [
    "access-service",
    "agent-service",
    "authoring-sandbox",
    "control-service",
    "environment-service",
    "evaluation-service",
    "openssh-gateway",
    "resource-service",
    "web",
];
const PACKAGE_SCHEMA: &str = "platform-image-package-manifest.v2";
const PLATFORM_PROFILE: &str = "platform";
const RESOURCE_PROFILE: &str = "resource";
#[cfg(target_os = "linux")]
const DEPLOYMENT_SCHEMA: &str = "platform-image-deployment-manifest.v1";

#[cfg(target_os = "linux")]
#[derive(Debug, Deserialize)]
struct VersionLock {
    platform_images: PlatformImageLock,
    platform_foundation: PlatformFoundationLock,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Deserialize)]
struct PlatformFoundationLock {
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
    helm: String,
    claude_code: String,
    claude_code_linux_x64_sha512: String,
    bases: BaseImageLock,
}

#[cfg(target_os = "linux")]
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
    #[serde(default = "default_package_profile")]
    profile: String,
    run_id: String,
    release_id: String,
    source_commit: String,
    source_date_epoch: u64,
    component_lock_hash: String,
    platform: String,
    registry: String,
    builder: BuilderIdentity,
    images: Vec<ImageIdentity>,
}

fn default_package_profile() -> String {
    PLATFORM_PROFILE.to_owned()
}

#[cfg(any(target_os = "linux", test))]
fn package_components(profile: &str) -> Option<Vec<&'static str>> {
    match profile {
        PLATFORM_PROFILE => Some(
            IMAGE_COMPONENTS
                .iter()
                .copied()
                .filter(|component| *component != "resource-service")
                .collect(),
        ),
        RESOURCE_PROFILE => Some(vec!["resource-service"]),
        _ => None,
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct BuilderIdentity {
    buildkit: String,
    buildx: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ImageIdentity {
    component: String,
    reference: String,
    digest: String,
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
    validate_schema_file(root, "platform-image-package-manifest.v2.schema.json")?;
    if connected {
        let environment = environment.ok_or(AppError::InvalidArgument {
            role: "connected package validation environment",
        })?;
        validate_environment(environment)?;
        connected_validate(&manifest, root)?;
    }
    Ok(())
}

/// Validates a package and requires the exact independently reviewed component profile.
pub(crate) fn validate_profile(
    manifest_path: &Path,
    expected_profile: &str,
    root: &Path,
) -> Result<(), AppError> {
    let manifest = read_manifest(manifest_path)?;
    validate_manifest(&manifest)?;
    validate_schema_file(root, "platform-image-package-manifest.v2.schema.json")?;
    if manifest.profile != expected_profile {
        return Err(AppError::PlatformImage {
            code: "LW_PACKAGE_PROFILE_MISMATCH",
            detail: expected_profile.to_owned(),
        });
    }
    Ok(())
}

pub(crate) fn package(
    environment: &str,
    release: &str,
    profile: &str,
    yes: bool,
    root: &Path,
) -> Result<(), AppError> {
    if !yes {
        return Err(AppError::ConfirmationRequired { command: "package" });
    }
    validate_environment(environment)?;
    validate_release(release)?;
    if !matches!(profile, PLATFORM_PROFILE | RESOURCE_PROFILE) {
        return Err(AppError::InvalidArgument {
            role: "package profile",
        });
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = root;
        Err(AppError::UnsupportedPlatform { command: "package" })
    }
    #[cfg(target_os = "linux")]
    {
        package_linux(environment, release, profile, root)
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

fn read_manifest(path: &Path) -> Result<PackageManifest, AppError> {
    let bytes = fs::read(path).map_err(|error| io_error("read package manifest", error))?;
    serde_json::from_slice(&bytes).map_err(|error| AppError::Io {
        role: "parse package manifest",
        detail: error.to_string(),
    })
}

fn validate_manifest(manifest: &PackageManifest) -> Result<(), AppError> {
    if manifest.schema_version != PACKAGE_SCHEMA
        || manifest.platform != "linux/amd64"
        || !is_commit(&manifest.source_commit)
        || !is_digest(&manifest.component_lock_hash)
    {
        return manifest_invalid("top-level identity is incomplete or incompatible");
    }
    if !matches!(
        manifest.profile.as_str(),
        PLATFORM_PROFILE | RESOURCE_PROFILE
    ) {
        return manifest_invalid("package profile is unknown");
    }
    validate_registry(&manifest.registry)?;
    let mut names = BTreeSet::new();
    for image in &manifest.images {
        if !IMAGE_COMPONENTS.contains(&image.component.as_str())
            || !names.insert(image.component.as_str())
        {
            return manifest_invalid("component set contains an unknown or duplicate name");
        }
        let expected = format!(
            "{}/labweaver-system/{}@{}",
            manifest.registry, image.component, image.digest
        );
        if !is_digest(&image.digest) {
            return manifest_invalid("image identity is incomplete");
        }
        if image.reference != expected || image.reference.contains(":latest") {
            return manifest_invalid("image reference is not the expected Harbor digest reference");
        }
    }
    if names.is_empty() {
        return manifest_invalid("manifest must contain at least one image");
    }
    if manifest.profile == RESOURCE_PROFILE && !names.contains("resource-service") {
        return manifest_invalid("resource package must include resource-service");
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
fn package_linux(
    environment: &str,
    release: &str,
    profile: &str,
    root: &Path,
) -> Result<(), AppError> {
    let source_commit = git_output(root, ["rev-parse", "HEAD"])?;
    if !is_commit(&source_commit) {
        return manifest_invalid("Git source commit is not a full lowercase SHA-1");
    }
    let dirty = !git_output(root, ["status", "--porcelain"])?.is_empty();
    if dirty {
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
    ensure_claude_code_package(root, &lock.platform_images)?;
    ensure_offline_pkg_closure(root, "containers/alpine-3.21-pkgs", ALPINE_V3_21_PKGS)?;
    ensure_offline_pkg_closure(
        root,
        "containers/debian-bookworm-pkgs",
        DEBIAN_BOOKWORM_PKGS,
    )?;
    let registry = required_env("LABWEAVER_PLATFORM_REGISTRY")?;
    validate_registry(&registry)?;
    let run_id = format!("pkg-{environment}-{release}-{}", &source_commit[..12]);
    let run_dir = root.join("artifacts/package").join(&run_id);
    fs::create_dir_all(&run_dir)
        .map_err(|error| io_error("create package run directory", error))?;
    let Some(components) = package_components(profile) else {
        return manifest_invalid("package profile is not supported");
    };
    let mut images = Vec::with_capacity(components.len());
    for component in components {
        images.push(build_image_identity(
            root,
            &registry,
            component,
            &source_commit,
            source_date_epoch,
            &lock.platform_images,
        )?);
    }
    let manifest = PackageManifest {
        schema_version: PACKAGE_SCHEMA.to_owned(),
        profile: profile.to_owned(),
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
fn build_image_identity(
    root: &Path,
    registry: &str,
    component: &str,
    source_commit: &str,
    source_date_epoch: u64,
    lock: &PlatformImageLock,
) -> Result<ImageIdentity, AppError> {
    let tag = format!(
        "{registry}/labweaver-system/{component}:git-{}",
        &source_commit[..12]
    );
    build_image(
        root,
        component,
        source_commit,
        source_date_epoch,
        &tag,
        registry,
        lock,
    )?;
    let first = inspect_platform_digest(&tag)?;
    let reference = format!("{registry}/labweaver-system/{component}@{first}");
    Ok(ImageIdentity {
        component: component.to_owned(),
        reference,
        digest: first,
    })
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
    if component == "authoring-sandbox" {
        command.args(["--target", "authoring-sandbox"]);
    } else if component != "web" && component != "openssh-gateway" {
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
    if component != "web" && component != "authoring-sandbox" {
        command.args([
            "--build-arg",
            &format!("RUST_TOOLCHAIN={}", lock.rust_toolchain),
        ]);
    }
    if component == "agent-service" || component == "authoring-sandbox" {
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
    command.args([
        "--build-arg",
        &format!(
            "CLAUDE_CODE_PACKAGE_PATH=containers/claude-code-linux-x64-{}.tgz",
            lock.claude_code
        ),
    ]);
    command.args(["--tag", tag]);
    for argument in build_proxy_arguments() {
        command.arg("--build-arg").arg(argument);
    }
    for (name, source) in build_base_images(component, lock) {
        command.args([
            "--build-arg",
            &format!("{name}={}", pinned_mirror(registry, name, source)?),
        ]);
    }
    if let Some(reference) = prior_component_reference(root, component)? {
        // The cluster BuildKit keeps only ephemeral storage, so after a daemon
        // restart every layer rebuilds and the sandbox image's npm stage has to
        // fetch from the npm registry, which the deployment proxy cannot reach.
        // Restoring the last published image as the layer cache keeps unchanged
        // stages offline (same Dockerfile + same build args hit the history).
        command.args(["--cache-from", &format!("type=registry,ref={reference}")]);
    }
    command.arg(".");
    run_checked(&mut command, "BuildKit platform image build").map(|_| ())
}

#[cfg(target_os = "linux")]
fn build_proxy_arguments() -> Vec<String> {
    let Ok(proxy) = std::env::var("LABWEAVER_BUILD_PROXY") else {
        return Vec::new();
    };
    if proxy.trim().is_empty() {
        return Vec::new();
    }
    let no_proxy = std::env::var("LABWEAVER_BUILD_NO_PROXY").unwrap_or_else(|_| {
        // dl-cdn.alpinelinux.org must stay direct: the deployment proxy's cached
        // Alpine index flips between revisions and intermittently refuses the
        // builder's fetches (Permission denied), breaking every apk pin.
        "localhost,127.0.0.1,harbor.lab.lan,10.0.0.0/8,10.96.0.0/12,10.99.0.0/16,10.244.0.0/16,49.52.27.0/24,dl-cdn.alpinelinux.org"
            .to_owned()
    });
    vec![
        format!("HTTP_PROXY={proxy}"),
        format!("HTTPS_PROXY={proxy}"),
        format!("NO_PROXY={no_proxy}"),
    ]
}

#[cfg(target_os = "linux")]
fn prior_component_reference(root: &Path, component: &str) -> Result<Option<String>, AppError> {
    let packages = root.join("artifacts/package");
    let entries =
        std::fs::read_dir(&packages).map_err(|error| io_error("read package dir", error))?;
    let mut manifests: Vec<(std::time::SystemTime, String, serde_json::Value)> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let manifest_path = entry.path().join("PlatformImagePackageManifest.json");
            let text = std::fs::read_to_string(manifest_path).ok()?;
            let value = serde_json::from_str(&text).ok()?;
            let modified = entry
                .path()
                .metadata()
                .ok()
                .and_then(|metadata| metadata.modified().ok())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            Some((
                modified,
                entry.file_name().to_string_lossy().into_owned(),
                value,
            ))
        })
        .collect();
    manifests.sort_by_key(|right| std::cmp::Reverse(right.0));
    let images = manifests
        .first()
        .map(|(_, _, manifest)| manifest.get("images").cloned().unwrap_or_default());
    let reference = images
        .as_ref()
        .and_then(|images| images.as_array())
        .and_then(|images| {
            images.iter().find_map(|image| {
                if image.get("component").and_then(serde_json::Value::as_str) == Some(component) {
                    image
                        .get("reference")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                } else {
                    None
                }
            })
        });
    Ok(reference)
}

#[cfg(target_os = "linux")]
fn sha512_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha512::digest(bytes))
}

/// Ensures the pinned Claude Code CLI tarball exists in the build context and
/// matches the reviewed sha512. The deployment proxy cannot reach the npm
/// registry, so the packaging host downloads the tarball directly from npm
/// once; every image build then verifies the same checksum offline.
const ALPINE_V3_21_PKGS: &[(&str, &str, &str)] = &[
    (
        "acl-2.3.2-r1.apk",
        "3baca85a19d33d1e50997b921ad0b42c6c8026377b71bbe444472c50c9833f77",
        "https://dl-cdn.alpinelinux.org/alpine/v3.21/main/x86_64/acl-2.3.2-r1.apk",
    ),
    (
        "acl-libs-2.3.2-r1.apk",
        "97aa6d629b26a4329757aa82629ce283710e78b0edc4ac26117246bac037591d",
        "https://dl-cdn.alpinelinux.org/alpine/v3.21/main/x86_64/acl-libs-2.3.2-r1.apk",
    ),
    (
        "attr-2.5.2-r2.apk",
        "0e2e4c441576605e59a0bc340db4d4ac3cc10df77c4887b9d192575df796dbda",
        "https://dl-cdn.alpinelinux.org/alpine/v3.21/main/x86_64/attr-2.5.2-r2.apk",
    ),
    (
        "coreutils-9.5-r2.apk",
        "e5c4b3445610117b76f54916c280da21a1642da1b9abd8653455ab17ff7a065b",
        "https://dl-cdn.alpinelinux.org/alpine/v3.21/main/x86_64/coreutils-9.5-r2.apk",
    ),
    (
        "coreutils-env-9.5-r2.apk",
        "ec7e1157a303d7c556f8a4c9eb735feb53dea533aa21342ddfc35da401879b47",
        "https://dl-cdn.alpinelinux.org/alpine/v3.21/main/x86_64/coreutils-env-9.5-r2.apk",
    ),
    (
        "coreutils-fmt-9.5-r2.apk",
        "a913cfbb4b13e948b2bfc84c2bcebb90c135e3c53d7607b11e5887d43a76fde7",
        "https://dl-cdn.alpinelinux.org/alpine/v3.21/main/x86_64/coreutils-fmt-9.5-r2.apk",
    ),
    (
        "coreutils-sha512sum-9.5-r2.apk",
        "33897821472e15dc8fdb7859282a55f87aca04772c7315fa2b156589c1b0b057",
        "https://dl-cdn.alpinelinux.org/alpine/v3.21/main/x86_64/coreutils-sha512sum-9.5-r2.apk",
    ),
    (
        "libattr-2.5.2-r2.apk",
        "372689c44dcd5dbf824cc3ff3c465a4a0f5cdb4d4428232f53196e1c766daf5e",
        "https://dl-cdn.alpinelinux.org/alpine/v3.21/main/x86_64/libattr-2.5.2-r2.apk",
    ),
    (
        "libedit-20240808.3.1-r0.apk",
        "975bd0133e92a3df828adabd4bf6654fa7d1a05727963dd4dfcdcf0ed8fbe20e",
        "https://dl-cdn.alpinelinux.org/alpine/v3.21/main/x86_64/libedit-20240808.3.1-r0.apk",
    ),
    (
        "libformw-6.5_p20241006-r3.apk",
        "a056a06631c9e43dbd58d9278f99fc08e48a9816643c2cf533bfca479f2728e3",
        "https://dl-cdn.alpinelinux.org/alpine/v3.21/main/x86_64/libformw-6.5_p20241006-r3.apk",
    ),
    (
        "libmenuw-6.5_p20241006-r3.apk",
        "8771a700324841ad30fd7ce44f6001968a50de484460987feb1d8af8ac90ce5d",
        "https://dl-cdn.alpinelinux.org/alpine/v3.21/main/x86_64/libmenuw-6.5_p20241006-r3.apk",
    ),
    (
        "libncursesw-6.5_p20241006-r3.apk",
        "3919cf673e841d91865213799ccfd5f77a48f5f9f5402723167470295ee32a49",
        "https://dl-cdn.alpinelinux.org/alpine/v3.21/main/x86_64/libncursesw-6.5_p20241006-r3.apk",
    ),
    (
        "libpanelw-6.5_p20241006-r3.apk",
        "f89752780981d11707e7eee3ca7b5a7f52e91e60ee28bd9e1815c7da05741c4a",
        "https://dl-cdn.alpinelinux.org/alpine/v3.21/main/x86_64/libpanelw-6.5_p20241006-r3.apk",
    ),
    (
        "musl-dev-1.2.5-r11.apk",
        "d3b5ab01046a92b9a168b790f516606e320f015cbd4deeb584c5e115a02124ba",
        "https://dl-cdn.alpinelinux.org/alpine/v3.21/main/x86_64/musl-dev-1.2.5-r11.apk",
    ),
    (
        "ncurses-libs-6.5_p20241006-r3.apk",
        "5efd05040495f4377f9addf798b42dbab13849a53991f7135fb7b151eaf5cb3c",
        "https://dl-cdn.alpinelinux.org/alpine/v3.21/main/x86_64/ncurses-libs-6.5_p20241006-r3.apk",
    ),
    (
        "ncurses-terminfo-base-6.5_p20241006-r3.apk",
        "46402464710d165a8fed4b843b3a20d9950e1e9a20923c3869241014bf6b2f51",
        "https://dl-cdn.alpinelinux.org/alpine/v3.21/main/x86_64/ncurses-terminfo-base-6.5_p20241006-r3.apk",
    ),
    (
        "openssh-client-common-9.9_p2-r0.apk",
        "0f2d8bad8eff07e282c42e52bfd15602094b7f1bced21127685f1bcc231aba76",
        "https://dl-cdn.alpinelinux.org/alpine/v3.21/main/x86_64/openssh-client-common-9.9_p2-r0.apk",
    ),
    (
        "openssh-client-default-9.9_p2-r0.apk",
        "1280bc7be47f88342880cda6270a4b736f9daab2eab9d2b0bce8826589b39e2d",
        "https://dl-cdn.alpinelinux.org/alpine/v3.21/main/x86_64/openssh-client-default-9.9_p2-r0.apk",
    ),
    (
        "openssh-keygen-9.9_p2-r0.apk",
        "a0bdfc8424a9346c22bfc0f9c9fa15471bb10147b1cea5d9559f116eca70d9d3",
        "https://dl-cdn.alpinelinux.org/alpine/v3.21/main/x86_64/openssh-keygen-9.9_p2-r0.apk",
    ),
    (
        "openssh-server-9.9_p2-r0.apk",
        "8e3c3f20d86d9bc79b4f01478b2e95f8f8e342db499bdc2511a771545047482a",
        "https://dl-cdn.alpinelinux.org/alpine/v3.21/main/x86_64/openssh-server-9.9_p2-r0.apk",
    ),
    (
        "openssh-server-common-9.9_p2-r0.apk",
        "71c76f2b7c377206553324c3bfb47e58ab5ecba6b42d0a2c3e4d278ec5ed0014",
        "https://dl-cdn.alpinelinux.org/alpine/v3.21/main/x86_64/openssh-server-common-9.9_p2-r0.apk",
    ),
    (
        "skalibs-libs-2.14.3.0-r0.apk",
        "10c8ca82df402bcf01dd1ac00277ee105a6e7528ab182a0dde4a15a20e32ce97",
        "https://dl-cdn.alpinelinux.org/alpine/v3.21/main/x86_64/skalibs-libs-2.14.3.0-r0.apk",
    ),
    (
        "util-linux-2.40.4-r1.apk",
        "780b714d13a198f604a553b7382827da0f427f04dd3c5534abb997c5371a9003",
        "https://dl-cdn.alpinelinux.org/alpine/v3.21/main/x86_64/util-linux-2.40.4-r1.apk",
    ),
    (
        "utmps-libs-0.1.2.3-r2.apk",
        "17b33292e4b080eef9798e73575a6d96c24bc97ad38d78ba4732a9c75096975d",
        "https://dl-cdn.alpinelinux.org/alpine/v3.21/main/x86_64/utmps-libs-0.1.2.3-r2.apk",
    ),
];
const DEBIAN_BOOKWORM_PKGS: &[(&str, &str, &str)] = &[
    (
        "bash_5.2.15-2+b13_amd64.deb",
        "82130bb6a560cd2a7234d8018baf73f188f5dd56413d5aa0accc987b2197a6a1",
        "https://deb.debian.org/debian/pool/main/b/bash/bash_5.2.15-2+b13_amd64.deb",
    ),
    (
        "ca-certificates_20250419~deb12u1_all.deb",
        "62b08a77d985d4253894b1f69aebda5925034ca4e294add364167fad8cb64a44",
        "http://deb.debian.org/debian-security/pool/updates/main/c/ca-certificates/ca-certificates_20250419%7edeb12u1_all.deb",
    ),
    (
        "curl_7.88.1-10+deb12u15_amd64.deb",
        "0dd9b6bf7a0bd11af2d68a52ec44c2a223fa7c11f9104c36ce1047e1137d4a8f",
        "https://deb.debian.org/debian/pool/main/c/curl/curl_7.88.1-10+deb12u15_amd64.deb",
    ),
    (
        "git-man_2.39.5-0+deb12u3_all.deb",
        "904dbd8dbc3db34c6780fb0abfe35816d4a90a490126ef0a055a77a0c5dcab82",
        "https://deb.debian.org/debian/pool/main/g/git/git-man_2.39.5-0+deb12u3_all.deb",
    ),
    (
        "git_2.39.5-0+deb12u3_amd64.deb",
        "637a85ddd6247fab13bdd0592f2f39aff04ce4dbf0655d3ab553ac359a38ce6f",
        "https://deb.debian.org/debian/pool/main/g/git/git_2.39.5-0+deb12u3_amd64.deb",
    ),
    (
        "libbrotli1_1.0.9-2+b6_amd64.deb",
        "563b4caec1aa5e876bd3355b36e7a38e1484baf5a293b48d1e8bd22db786e4d7",
        "https://deb.debian.org/debian/pool/main/b/brotli/libbrotli1_1.0.9-2+b6_amd64.deb",
    ),
    (
        "libcurl3-gnutls_7.88.1-10+deb12u15_amd64.deb",
        "bf430ecf80f7808704e0187cddc582fd57fc98789ea356d4797651edad387ef3",
        "https://deb.debian.org/debian/pool/main/c/curl/libcurl3-gnutls_7.88.1-10+deb12u15_amd64.deb",
    ),
    (
        "libcurl4_7.88.1-10+deb12u15_amd64.deb",
        "3042904de01f9c4fbdcf1452b8f81abedcf2b015f9b9deba109063322b5bd68b",
        "https://deb.debian.org/debian/pool/main/c/curl/libcurl4_7.88.1-10+deb12u15_amd64.deb",
    ),
    (
        "liberror-perl_0.17029-2_all.deb",
        "5a466348531b9c38c8e5ccb18c231f27a98b9fdab61b37ea22592553de5d2ced",
        "https://deb.debian.org/debian/pool/main/libe/liberror-perl/liberror-perl_0.17029-2_all.deb",
    ),
    (
        "libexpat1_2.5.0-1+deb12u3_amd64.deb",
        "04405c19b977d19c5e0fd7210e452b8044a065cb45a82012d45046738920ca75",
        "http://deb.debian.org/debian-security/pool/updates/main/e/expat/libexpat1_2.5.0-1%2bdeb12u3_amd64.deb",
    ),
    (
        "libgdbm-compat4_1.23-3_amd64.deb",
        "4af36a590b68d415a78d9238b932b6a4579f515ec8a8016597498acff5b515a4",
        "https://deb.debian.org/debian/pool/main/g/gdbm/libgdbm-compat4_1.23-3_amd64.deb",
    ),
    (
        "libgdbm6_1.23-3_amd64.deb",
        "95fe4a1336532450e67bd067892f46eaa484139919ea8d067a9ffcbf5a4bf883",
        "https://deb.debian.org/debian/pool/main/g/gdbm/libgdbm6_1.23-3_amd64.deb",
    ),
    (
        "libgssapi-krb5-2_1.20.1-2+deb12u5_amd64.deb",
        "d8b87aea91b956b416c01a28c44ff6228315ae3b73de40acddac3dc5043607d2",
        "https://deb.debian.org/debian/pool/main/k/krb5/libgssapi-krb5-2_1.20.1-2+deb12u5_amd64.deb",
    ),
    (
        "libk5crypto3_1.20.1-2+deb12u5_amd64.deb",
        "7b6e47d15c5c9cdcd456137533d8308aeba608a54cec73b28bd59bfc4b54cf06",
        "https://deb.debian.org/debian/pool/main/k/krb5/libk5crypto3_1.20.1-2+deb12u5_amd64.deb",
    ),
    (
        "libkeyutils1_1.6.3-2_amd64.deb",
        "cfac89e6a7a54ff3c6a4f843310e25efeddaa771baeae470bd98bd588c373563",
        "https://deb.debian.org/debian/pool/main/k/keyutils/libkeyutils1_1.6.3-2_amd64.deb",
    ),
    (
        "libkrb5-3_1.20.1-2+deb12u5_amd64.deb",
        "c90801be09701af221b54dd502a59cb378b7284ff31e480738baca317d2f7653",
        "https://deb.debian.org/debian/pool/main/k/krb5/libkrb5-3_1.20.1-2+deb12u5_amd64.deb",
    ),
    (
        "libkrb5support0_1.20.1-2+deb12u5_amd64.deb",
        "8b9c05f2d8d461a10d657b118f572af5bf8fd6c8f93dcef81e852c5ab0f428ab",
        "https://deb.debian.org/debian/pool/main/k/krb5/libkrb5support0_1.20.1-2+deb12u5_amd64.deb",
    ),
    (
        "libldap-2.5-0_2.5.13+dfsg-5_amd64.deb",
        "4b6c30f6554149c594628d945edc6003f0eea8d0cc1341638c0e71375db147ed",
        "https://deb.debian.org/debian/pool/main/o/openldap/libldap-2.5-0_2.5.13+dfsg-5_amd64.deb",
    ),
    (
        "libncursesw6_6.4-4_amd64.deb",
        "98fa7a53dc565a38b65fb70422ad08001bf5361d8fbc74255280c329996a6bec",
        "https://deb.debian.org/debian/pool/main/n/ncurses/libncursesw6_6.4-4_amd64.deb",
    ),
    (
        "libnghttp2-14_1.52.0-1+deb12u3_amd64.deb",
        "5a5736cee57e51c1baed869979e6ecdbc6495e939e33203d6cfe3b6e5a149e3f",
        "https://deb.debian.org/debian/pool/main/n/nghttp2/libnghttp2-14_1.52.0-1+deb12u3_amd64.deb",
    ),
    (
        "libnsl2_1.3.0-2_amd64.deb",
        "c0d83437fdb016cb289436f49f28a36be44b3e8f1f2498c7e3a095f709c0d6f8",
        "https://deb.debian.org/debian/pool/main/libn/libnsl/libnsl2_1.3.0-2_amd64.deb",
    ),
    (
        "libperl5.36_5.36.0-7+deb12u3_amd64.deb",
        "591903643119c7e1735011369287976eccab1d79b1d2f210760e2138d0a3aa46",
        "https://deb.debian.org/debian/pool/main/p/perl/libperl5.36_5.36.0-7+deb12u3_amd64.deb",
    ),
    (
        "libpsl5_0.21.2-1_amd64.deb",
        "4f0d35610204e4e754b057748719744114621f2f6f4202d846c314860a981afb",
        "https://deb.debian.org/debian/pool/main/libp/libpsl/libpsl5_0.21.2-1_amd64.deb",
    ),
    (
        "libpython3-stdlib_3.11.2-1+b1_amd64.deb",
        "4e58891d5c951a1e360ed9eaa814413cb5e84deadce3f08e801ac680434c786e",
        "https://deb.debian.org/debian/pool/main/p/python3-defaults/libpython3-stdlib_3.11.2-1+b1_amd64.deb",
    ),
    (
        "libpython3.11-minimal_3.11.2-6+deb12u8_amd64.deb",
        "f3beaa03994ffedacf73c43a0843d53b062d347115d8af65bee2034552a4e6f9",
        "https://deb.debian.org/debian/pool/main/p/python3.11/libpython3.11-minimal_3.11.2-6+deb12u8_amd64.deb",
    ),
    (
        "libpython3.11-stdlib_3.11.2-6+deb12u8_amd64.deb",
        "890b3540dad8a1ccc0deeca025db735bcc82629a76adacbe3b50fcc06ed528ca",
        "https://deb.debian.org/debian/pool/main/p/python3.11/libpython3.11-stdlib_3.11.2-6+deb12u8_amd64.deb",
    ),
    (
        "libreadline8_8.2-1.3_amd64.deb",
        "e02ebbd3701cf468dbf98d6d917fbe0325e881f07fe8b316150c8d2a64486e66",
        "https://deb.debian.org/debian/pool/main/r/readline/libreadline8_8.2-1.3_amd64.deb",
    ),
    (
        "librtmp1_2.4+20151223.gitfa8646d.1-2+b2_amd64.deb",
        "e1f69020dc2c466e421ec6a58406b643be8b5c382abf0f8989011c1d3df91c87",
        "https://deb.debian.org/debian/pool/main/r/rtmpdump/librtmp1_2.4+20151223.gitfa8646d.1-2+b2_amd64.deb",
    ),
    (
        "libsasl2-2_2.1.28+dfsg-10_amd64.deb",
        "11ee190ad39f8d7af441d2c8347388b9449434c73acc67b4b372445ac4152efa",
        "https://deb.debian.org/debian/pool/main/c/cyrus-sasl2/libsasl2-2_2.1.28+dfsg-10_amd64.deb",
    ),
    (
        "libsasl2-modules-db_2.1.28+dfsg-10_amd64.deb",
        "3ac4fd6cbe3b3b06e68d24b931bf3eb9385b42f15604a37ed25310e948ca0ee6",
        "https://deb.debian.org/debian/pool/main/c/cyrus-sasl2/libsasl2-modules-db_2.1.28+dfsg-10_amd64.deb",
    ),
    (
        "libsqlite3-0_3.40.1-2+deb12u2_amd64.deb",
        "a8d78b40e9b4e422224aeebfe0e4dfc243f6acf3532490b0c05480d4283d41e2",
        "https://deb.debian.org/debian/pool/main/s/sqlite3/libsqlite3-0_3.40.1-2+deb12u2_amd64.deb",
    ),
    (
        "libssh2-1_1.10.0-3+deb12u1_amd64.deb",
        "fff72a194e493f88e100a2567e22472bb4ab828d429c2956965c6f2f134f1b3a",
        "http://deb.debian.org/debian-security/pool/updates/main/libs/libssh2/libssh2-1_1.10.0-3%2bdeb12u1_amd64.deb",
    ),
    (
        "libssl3_3.0.22-1~deb12u1_amd64.deb",
        "f0a8aa8429209e556c278a9936bbd5f7d2cdb9f7e4e23b1e43ed399217ba80c1",
        "http://deb.debian.org/debian-security/pool/updates/main/o/openssl/libssl3_3.0.22-1%7edeb12u1_amd64.deb",
    ),
    (
        "libtirpc-common_1.3.3+ds-1_all.deb",
        "3e3ef129b4bf61513144236e15e1b4ec57fa5ae3dc8a72137abdbefb7a63af85",
        "https://deb.debian.org/debian/pool/main/libt/libtirpc/libtirpc-common_1.3.3+ds-1_all.deb",
    ),
    (
        "libtirpc3_1.3.3+ds-1_amd64.deb",
        "2a46d5a5e9486da11ffeff5740931740d6deae4f92cd6098df060dc5dff1e1c7",
        "https://deb.debian.org/debian/pool/main/libt/libtirpc/libtirpc3_1.3.3+ds-1_amd64.deb",
    ),
    (
        "media-types_10.0.0_all.deb",
        "aaa46dcb3b39948ae2e0fdb72cfcb2f48c0b59f19785a3da8045c05eb19955dd",
        "https://deb.debian.org/debian/pool/main/m/media-types/media-types_10.0.0_all.deb",
    ),
    (
        "openssl_3.0.22-1~deb12u1_amd64.deb",
        "6f43fb5e9f3ceb0e36c91d0a148282a8eaf174b441c17d3665b6ba049b33d2c2",
        "http://deb.debian.org/debian-security/pool/updates/main/o/openssl/openssl_3.0.22-1%7edeb12u1_amd64.deb",
    ),
    (
        "perl-base_5.36.0-7+deb12u3_amd64.deb",
        "8ec874926e211807cde71e1b0a2311d2534ab3539dffcb2c8553633f542efc1a",
        "https://deb.debian.org/debian/pool/main/p/perl/perl-base_5.36.0-7+deb12u3_amd64.deb",
    ),
    (
        "perl-modules-5.36_5.36.0-7+deb12u3_all.deb",
        "3d237ccb1ea32727b6573d43c67d78337aa928a81148d36cef54b369a07c240a",
        "https://deb.debian.org/debian/pool/main/p/perl/perl-modules-5.36_5.36.0-7+deb12u3_all.deb",
    ),
    (
        "perl_5.36.0-7+deb12u3_amd64.deb",
        "afa50ec7d9b1a407cd0187dae033644ef13578d4f3792435e0c41b962ffec0c4",
        "https://deb.debian.org/debian/pool/main/p/perl/perl_5.36.0-7+deb12u3_amd64.deb",
    ),
    (
        "python3-minimal_3.11.2-1+b1_amd64.deb",
        "30f9618670e686d781afbfc713eb0830c29d2819e9cb2a0488800dad6bb99faa",
        "https://deb.debian.org/debian/pool/main/p/python3-defaults/python3-minimal_3.11.2-1+b1_amd64.deb",
    ),
    (
        "python3.11-minimal_3.11.2-6+deb12u8_amd64.deb",
        "4aba533f7cc5e7b93b7ff24482840e96813f5bcde9cce028395b65a0d799ccee",
        "https://deb.debian.org/debian/pool/main/p/python3.11/python3.11-minimal_3.11.2-6+deb12u8_amd64.deb",
    ),
    (
        "python3.11_3.11.2-6+deb12u8_amd64.deb",
        "cd7b10c24281416a6acb22cd23ed7391c7dddd4a3d4d4a63d37faa786639b5de",
        "https://deb.debian.org/debian/pool/main/p/python3.11/python3.11_3.11.2-6+deb12u8_amd64.deb",
    ),
    (
        "python3_3.11.2-1+b1_amd64.deb",
        "33f6dafbd1a6902d9063172ec7dbd4b2225e12009e0d7ec5c933a72c2f5f3b74",
        "https://deb.debian.org/debian/pool/main/p/python3-defaults/python3_3.11.2-1+b1_amd64.deb",
    ),
    (
        "readline-common_8.2-1.3_all.deb",
        "69317523fe56429aa361545416ad339d138c1500e5a604856a80dd9074b4e35c",
        "https://deb.debian.org/debian/pool/main/r/readline/readline-common_8.2-1.3_all.deb",
    ),
];

#[cfg(target_os = "linux")]
fn ensure_offline_pkg_closure(
    root: &Path,
    dir: &str,
    entries: &[(&str, &str, &str)],
) -> Result<(), AppError> {
    let base = root.join(dir);
    for (file, expected_sha256, url) in entries {
        let path = base.join(file);
        let present = std::fs::read(&path)
            .map(|bytes| format!("{:x}", Sha256::digest(&bytes)) == *expected_sha256)
            .unwrap_or(false);
        if !present {
            let output_path = path.to_string_lossy().into_owned();
            run_checked(
                Command::new("curl").args([
                    "--fail",
                    "--silent",
                    "--show-error",
                    "--location",
                    "--retry",
                    "3",
                    "--noproxy",
                    "*",
                    "--output",
                    &output_path,
                    url,
                ]),
                "download vendored offline package",
            )?;
        }
        let actual = std::fs::read(&path).map_err(|_| AppError::PlatformImage {
            code: "LW_PACKAGE_INPUT_MISSING",
            detail: (*file).to_owned(),
        })?;
        let actual = format!("{:x}", Sha256::digest(&actual));
        if actual != *expected_sha256 {
            return Err(AppError::PlatformImage {
                code: "LW_PACKAGE_INPUT_INVALID",
                detail: format!(
                    "vendored package {file} sha256 mismatch: expected {expected_sha256}, observed {actual}"
                ),
            });
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn ensure_claude_code_package(root: &Path, lock: &PlatformImageLock) -> Result<(), AppError> {
    let version = &lock.claude_code;
    let expected = &lock.claude_code_linux_x64_sha512;
    let relative = format!("containers/claude-code-linux-x64-{version}.tgz");
    if !version
        .chars()
        .all(|character| character.is_ascii_digit() || character == '.')
    {
        return Err(AppError::PlatformImage {
            code: "LW_PACKAGE_INPUT_INVALID",
            detail: format!("claude_code version is not a dotted numeric string: {version}"),
        });
    }
    let path = root.join(&relative);
    let present_and_valid = path.exists()
        && std::fs::read(&path)
            .map(|bytes| sha512_bytes(&bytes) == *expected)
            .unwrap_or(false);
    if !present_and_valid {
        let url = format!(
            "https://registry.npmjs.org/@anthropic-ai/claude-code-linux-x64/-/\
             claude-code-linux-x64-{version}.tgz"
        );
        run_checked(
            Command::new("curl").args([
                "--fail",
                "--silent",
                "--show-error",
                "--location",
                "--retry",
                "3",
                "--output",
                &path.to_string_lossy(),
                &url,
            ]),
            "download pinned claude-code package",
        )?;
    }
    let actual = std::fs::read(&path).map_err(|_| AppError::PlatformImage {
        code: "LW_PACKAGE_INPUT_MISSING",
        detail: relative,
    })?;
    let actual = sha512_bytes(&actual);
    if actual != *expected {
        return Err(AppError::PlatformImage {
            code: "LW_PACKAGE_INPUT_INVALID",
            detail: format!(
                "claude-code package sha512 mismatch: expected {expected}, observed {actual}"
            ),
        });
    }
    Ok(())
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
        ],
        "authoring-sandbox" => vec![
            ("NODE_BUILDER", lock.bases.node_builder.as_str()),
            ("BUILDKIT_IMAGE", lock.buildkit_image.as_str()),
        ],
        _ => vec![
            ("RUST_BUILDER", lock.bases.rust_builder.as_str()),
            ("RUST_RUNTIME", lock.bases.rust_runtime.as_str()),
            ("BUILDKIT_IMAGE", lock.buildkit_image.as_str()),
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
fn verify_tools(lock: &VersionLock) -> Result<(), AppError> {
    let platform = &lock.platform_images;
    let checks = [
        ("docker-buildx", vec!["version"], platform.buildx.as_str()),
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

    verify_remote_buildkit_deployment(&lock.platform_foundation.buildkit_rootless)
}

#[cfg(target_os = "linux")]
fn verify_rust_toolchain(root: &Path, platform: &PlatformImageLock) -> Result<(), AppError> {
    verify_rust_toolchain_inputs(
        root,
        &platform.rust_toolchain,
        &platform.bases.rust_builder,
        &platform.bases.gateway_builder,
    )
}

#[cfg(any(target_os = "linux", test))]
fn verify_rust_toolchain_inputs(
    root: &Path,
    rust_toolchain: &str,
    rust_builder: &str,
    gateway_builder: &str,
) -> Result<(), AppError> {
    let toolchain = fs::read_to_string(root.join("rust-toolchain.toml"))
        .map_err(|error| io_error("read Rust toolchain lock", error))?;
    let workspace = fs::read_to_string(root.join("Cargo.toml"))
        .map_err(|error| io_error("read Rust workspace manifest", error))?;
    let expected = format!("channel = \"{rust_toolchain}\"");
    let expected_msrv = format!("rust-version = \"{rust_toolchain}\"");
    let channels = toolchain
        .lines()
        .filter(|line| line.starts_with("channel"))
        .collect::<Vec<_>>();
    let builder_marker = format!("rust:{rust_toolchain}-");
    let toolchain_argument = format!("ARG RUST_TOOLCHAIN={rust_toolchain}");
    let build_inputs = [
        ("containers/Containerfile.rust", toolchain_argument.as_str()),
        ("access-gateway/Dockerfile", toolchain_argument.as_str()),
        (
            "containers/Containerfile.ansible-probe",
            toolchain_argument.as_str(),
        ),
        (
            "containers/Containerfile.oj-cpp17",
            toolchain_argument.as_str(),
        ),
        ("tools/xtask-container.sh", rust_toolchain),
    ];
    if channels != [expected.as_str()]
        || !workspace
            .lines()
            .any(|line| line.trim() == expected_msrv.as_str())
        || !rust_builder.contains(&builder_marker)
        || !gateway_builder.contains(&builder_marker)
    {
        return Err(AppError::PlatformImage {
            code: "LW_PACKAGE_RUST_TOOLCHAIN_IDENTITY_MISMATCH",
            detail: "Rust workspace, toolchain lock and locked builder images must use the same explicit version".to_owned(),
        });
    }
    for (path, marker) in build_inputs {
        let content = fs::read_to_string(root.join(path))
            .map_err(|error| io_error("read Rust build input", error))?;
        if !content.contains(marker) {
            return Err(AppError::PlatformImage {
                code: "LW_PACKAGE_RUST_TOOLCHAIN_IDENTITY_MISMATCH",
                detail: format!("{path} does not use the locked Rust toolchain"),
            });
        }
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
    let configuration_bundle_sha256 = required_env("LABWEAVER_CONFIGURATION_BUNDLE_SHA256")?;
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
        .args([
            "--set-string",
            &format!("deploymentIdentity.configurationBundleSha256={configuration_bundle_sha256}"),
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
            profile: PLATFORM_PROFILE.to_owned(),
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
            images: package_components(PLATFORM_PROFILE)
                .expect("platform profile is declared")
                .into_iter()
                .enumerate()
                .map(|(index, component)| {
                    let digest_char = ['3', '4', '5', '6', '7', '8', '9', 'a'][index];
                    let image_digest = digest(digest_char);
                    let reference = format!(
                        "harbor.internal.example/labweaver-system/{component}@{image_digest}"
                    );
                    ImageIdentity {
                        component: component.to_owned(),
                        reference: reference.clone(),
                        digest: image_digest,
                    }
                })
                .collect(),
        }
    }

    #[test]
    fn static_manifest_accepts_digest_bound_images() {
        assert!(validate_manifest(&valid_manifest()).is_ok());
    }

    #[test]
    fn resource_profile_accepts_only_the_resource_service_image() {
        let mut manifest = valid_manifest();
        manifest.profile = RESOURCE_PROFILE.to_owned();
        manifest.images = vec![ImageIdentity {
            component: "resource-service".to_owned(),
            reference: format!(
                "harbor.internal.example/labweaver-system/resource-service@{}",
                digest('3')
            ),
            digest: digest('3'),
        }];
        assert!(validate_manifest(&manifest).is_ok());

        manifest.images[0].component = "control-service".to_owned();
        assert!(validate_manifest(&manifest).is_err());
    }

    #[test]
    fn static_manifest_rejects_empty_duplicate_and_external_images() {
        let mut empty = valid_manifest();
        empty.images.clear();
        assert!(validate_manifest(&empty).is_err());

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
        assert!(containerfile.contains("CLAUDE_CODE_PACKAGE_PATH"));
        assert!(containerfile.contains("sha512sum --check --strict"));
        assert!(containerfile.contains("/usr/local/bin/claude"));
        assert!(
            std::fs::read_to_string(root.join("access-gateway/Dockerfile"))?
                .contains("RUSTUP_TOOLCHAIN=${RUST_TOOLCHAIN}")
        );
        Ok(())
    }

    #[test]
    fn rust_toolchain_identity_is_consistent_across_active_build_inputs()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let lock: serde_yaml::Value = serde_yaml::from_str(&std::fs::read_to_string(
            root.join("deploy/versions.lock.yml"),
        )?)?;
        let rust_toolchain = lock
            .get("platform_images")
            .and_then(|value| value.get("rust_toolchain"))
            .and_then(serde_yaml::Value::as_str)
            .ok_or("platform Rust toolchain must be a string")?;
        let bases = lock
            .get("platform_images")
            .and_then(|value| value.get("bases"))
            .ok_or("platform base image locks must exist")?;
        let rust_builder = bases
            .get("rust_builder")
            .and_then(serde_yaml::Value::as_str)
            .ok_or("Rust builder lock must be a string")?;
        let gateway_builder = bases
            .get("gateway_builder")
            .and_then(serde_yaml::Value::as_str)
            .ok_or("gateway builder lock must be a string")?;
        verify_rust_toolchain_inputs(&root, rust_toolchain, rust_builder, gateway_builder)?;
        Ok(())
    }

    #[test]
    fn rust_toolchain_identity_mismatch_has_a_stable_diagnostic()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        std::fs::write(
            root.path().join("rust-toolchain.toml"),
            "[toolchain]\nchannel = \"1.97.1\"\n",
        )?;
        std::fs::write(
            root.path().join("Cargo.toml"),
            "[workspace.package]\nrust-version = \"1.96.0\"\n",
        )?;
        let result = verify_rust_toolchain_inputs(
            root.path(),
            "1.97.1",
            "docker.io/library/rust:1.97.1-bookworm@sha256:locked",
            "docker.io/library/rust:1.97.1-alpine3.21@sha256:locked",
        );
        let error = match result {
            Ok(()) => return Err("mixed Rust toolchain identity must fail closed".into()),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            AppError::PlatformImage {
                code: "LW_PACKAGE_RUST_TOOLCHAIN_IDENTITY_MISMATCH",
                ..
            }
        ));
        Ok(())
    }
}
