//! Exercise the actual Control initialization command with isolated `AtomicWriter` inputs.
#![cfg(unix)]

use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
};

use contracts::platform_configuration::{
    PlatformConfigurationDesired, PlatformConfigurationVersion,
};
use contracts::{ActorId, Revision, UtcTimestamp};
use control_service::platform_configuration::{
    LoadedConfiguration, prepare_projected_configuration,
};
use persistence_sqlx::Sha256Digest;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
const VERSION: &str = "platform-configuration.json";
const SOURCE: &str = "platform-configuration-source.json";

fn inputs(root: &Path) -> TestResult<String> {
    fs::create_dir_all(root.join("generation-one"))?;
    let configuration = PlatformConfigurationDesired::default();
    let version = PlatformConfigurationVersion {
        revision: Revision::new(1)?,
        configuration_sha256: Sha256Digest::of_canonical(&configuration)?.to_string(),
        configuration,
        created_by: ActorId::new(),
        created_at: "2026-10-12T17:00:00.000Z".parse::<UtcTimestamp>()?,
    };
    let source = serde_json::json!({
        "apiVersion":"deploy.labweaver.io/loaded-source/v1", "desiredRevision":1,
        "configurationSha256":version.configuration_sha256, "encryptedBundleSha256":"a".repeat(64),
        "installation":{"namespace":"synthetic-control", "releaseName":"synthetic"}, "expectedControlReplicas":1
    });
    fs::write(
        root.join("generation-one").join(VERSION),
        serde_json::to_vec(&version)?,
    )?;
    fs::write(
        root.join("generation-one").join(SOURCE),
        serde_json::to_vec(&source)?,
    )?;
    for name in [VERSION, SOURCE] {
        fs::set_permissions(
            root.join("generation-one").join(name),
            fs::Permissions::from_mode(0o444),
        )?;
        symlink(format!("..data/{name}"), root.join(name))?;
    }
    symlink("generation-one", root.join("..data"))?;
    checksum(&root.join("generation-one"))
}

fn checksum(directory: &Path) -> TestResult<String> {
    let mut bytes = fs::read(directory.join(VERSION))?;
    bytes.push(b'\n');
    bytes.extend(fs::read(directory.join(SOURCE))?);
    Ok(Sha256Digest::of_bytes(&bytes).to_string())
}

#[test]
fn actual_init_command_copies_one_projection_and_retries_without_credentials() -> TestResult {
    let temporary = tempfile::tempdir()?;
    let projected = temporary.path().join("projected");
    let runtime = temporary.path().join("runtime");
    fs::create_dir(&runtime)?;
    let digest = inputs(&projected)?;
    for _ in 0..2 {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_control-service"))
            .arg("--prepare-managed-configuration")
            .env_remove("LABWEAVER_CONTROL_CONFIG_FILE")
            .env("LABWEAVER_PLATFORM_CONFIGURATION_PROJECTED_DIR", &projected)
            .env("LABWEAVER_PLATFORM_CONFIGURATION_RUNTIME_DIR", &runtime)
            .env("LABWEAVER_PLATFORM_CONFIGURATION_PROJECTED_SHA256", &digest)
            .output()?;
        assert!(
            output.status.success(),
            "initialization failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty());
    }
    let current = runtime.join("current");
    for name in [VERSION, SOURCE] {
        assert_eq!(
            fs::symlink_metadata(current.join(name))?
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert!(
            !fs::symlink_metadata(current.join(name))?
                .file_type()
                .is_symlink()
        );
    }
    assert_eq!(fs::metadata(&current)?.permissions().mode() & 0o777, 0o700);
    LoadedConfiguration::load(&current.join(VERSION), &current.join(SOURCE))?;
    assert_eq!(fs::read_dir(&runtime)?.count(), 1);
    Ok(())
}

#[test]
fn failed_copy_and_changed_projection_keep_prior_runtime_files() -> TestResult {
    let temporary = tempfile::tempdir()?;
    let projected = temporary.path().join("projected");
    let runtime = temporary.path().join("runtime");
    fs::create_dir(&runtime)?;
    let digest = inputs(&projected)?;
    prepare_projected_configuration(&projected, &runtime, &digest)?;
    let prior = fs::read(runtime.join("current").join(VERSION))?;
    assert!(prepare_projected_configuration(&projected, &runtime, &"0".repeat(64)).is_err());
    fs::create_dir(projected.join("generation-two"))?;
    fs::write(projected.join("generation-two").join(VERSION), &prior)?;
    let mut source: serde_json::Value =
        serde_json::from_slice(&fs::read(projected.join("generation-one").join(SOURCE))?)?;
    source["encryptedBundleSha256"] = serde_json::json!("b".repeat(64));
    fs::write(
        projected.join("generation-two").join(SOURCE),
        serde_json::to_vec(&source)?,
    )?;
    fs::remove_file(projected.join("..data"))?;
    symlink("generation-two", projected.join("..data"))?;
    let changed = checksum(&projected.join("generation-two"))?;
    assert!(prepare_projected_configuration(&projected, &runtime, &changed).is_err());
    assert_eq!(fs::read(runtime.join("current").join(VERSION))?, prior);
    assert_eq!(fs::read_dir(&runtime)?.count(), 1);
    Ok(())
}

#[test]
fn missing_files_escape_links_and_invalid_typed_input_do_not_publish_current() -> TestResult {
    let temporary = tempfile::tempdir()?;
    let projected = temporary.path().join("projected");
    let runtime = temporary.path().join("runtime");
    fs::create_dir(&runtime)?;
    let digest = inputs(&projected)?;
    fs::remove_file(projected.join("generation-one").join(SOURCE))?;
    assert!(prepare_projected_configuration(&projected, &runtime, &digest).is_err());
    symlink(
        "../../outside",
        projected.join("generation-one").join(SOURCE),
    )?;
    assert!(prepare_projected_configuration(&projected, &runtime, &digest).is_err());
    fs::remove_file(projected.join("generation-one").join(SOURCE))?;
    fs::write(
        projected.join("generation-one").join(SOURCE),
        b"synthetic-secret-never-log",
    )?;
    let invalid = checksum(&projected.join("generation-one"))?;
    let error = prepare_projected_configuration(&projected, &runtime, &invalid)
        .err()
        .ok_or("invalid source published")?;
    assert!(!error.to_string().contains("synthetic-secret"));
    fs::remove_file(projected.join("..data"))?;
    symlink(temporary.path(), projected.join("..data"))?;
    assert!(prepare_projected_configuration(&projected, &runtime, &digest).is_err());
    assert_eq!(fs::read_dir(&runtime)?.count(), 0);
    Ok(())
}
