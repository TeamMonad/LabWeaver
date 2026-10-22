//! Shared non-business dependencies for real configuration integration tests.
use artifact_store::{ImmutableObjectStore, ObjectStoreError, PresignedUpload, VerifiedObject};
use async_trait::async_trait;
use contracts::supply_chain::BuildNetworkPolicy;
use contracts::{ArtifactRef, PolicyId, Revision, UtcTimestamp};
use control_service::{ContainerBuildPolicy, ControlConfig, EvaluationRuntimePolicy};
use persistence_sqlx::Sha256Digest;

pub(super) struct NoObjects;
#[async_trait]
impl ImmutableObjectStore for NoObjects {
    async fn delete_orphan(&self, _: &str, _: &str) -> Result<(), ObjectStoreError> {
        Err(ObjectStoreError::ObjectUnavailable)
    }
    fn binding(&self) -> &'static str {
        "unused-fixture"
    }
    async fn presign_upload(
        &self,
        _: &str,
        _: u64,
        _: &str,
        _: UtcTimestamp,
    ) -> Result<PresignedUpload, ObjectStoreError> {
        Err(ObjectStoreError::ObjectUnavailable)
    }
    async fn read_verified(
        &self,
        _: &str,
        _: &ArtifactRef,
    ) -> Result<VerifiedObject, ObjectStoreError> {
        Err(ObjectStoreError::ObjectUnavailable)
    }
    async fn freeze_current(
        &self,
        _: &str,
        _: u64,
        _: &str,
    ) -> Result<VerifiedObject, ObjectStoreError> {
        Err(ObjectStoreError::ObjectUnavailable)
    }
}

pub(super) fn config() -> Result<ControlConfig, Box<dyn std::error::Error>> {
    Ok(ControlConfig {
        package_object_prefix: "problem-packages".to_owned(),
        upload_ttl_seconds: 900,
        completion_lease_seconds: 300,
        max_package_files: 100,
        max_package_bytes: 1_048_576,
        retention_policy_id: PolicyId::new(),
        retention_seconds: 86_400,
        sse_retention_seconds: 3_600,
        trust_revision: Revision::new(1)?,
        image_policy_id: PolicyId::new(),
        image_policy_revision: Revision::new(1)?,
        environment_schema_sha256: Sha256Digest::of_bytes(b"environment"),
        evaluation_schema_sha256: Sha256Digest::of_bytes(b"evaluation"),
        container_build: ContainerBuildPolicy {
            builder_binding: "buildkit-primary-v1".to_owned(),
            output_repository_prefix: "harbor.internal/labweaver-system".to_owned(),
            dockerfile_path: "Dockerfile".to_owned(),
            runner_dockerfile_path: "evaluation/Dockerfile".to_owned(),
            network: BuildNetworkPolicy::DenyAll,
            max_duration_milliseconds: 600_000,
            max_cpu_millicores: 2_000,
            max_memory_bytes: 2_147_483_648,
        },
        virtual_machine_bases: control_service::VirtualMachineBaseCatalog {
            provider_binding: "kubevirt-primary-v1".to_owned(),
            storage_class_binding: "vm-rwo-primary-v1".to_owned(),
            max_bases: 8,
            max_capacity_bytes: 137_438_953_472,
            bases: vec![control_service::VirtualMachineBasePolicy {
                artifact_id: contracts::ImageArtifactId::new(),
                base_disk: contracts::supply_chain::VirtualMachineBaseDisk {
                    binding: "ubuntu-24.04-v1".to_owned(),
                    source_registry_digest: concat!(
                        "docker://quay.io/containerdisks/ubuntu@",
                        "sha256:d28194a16351320fa9a093e18233033508a745566eb8ba3b309c32924bf155a5"
                    )
                    .to_owned(),
                    capacity_bytes: 10_737_418_240,
                },
                format: contracts::supply_chain::VirtualMachineDiskFormat::Qcow2,
            }],
        },
        evaluation_runtime: EvaluationRuntimePolicy {
            provider_binding: "evaluation-primary-v1".to_owned(),
            runner_image: format!("runner@sha256:{}", "a".repeat(64)),
        },
        llm_policy_options: contracts::authoring::ProjectLlmPolicyOptions {
            models: vec![contracts::authoring::ProjectLlmPolicyModelOption {
                model: "fixture-provider-v1".to_owned(),
                label: "Fixture model".to_owned(),
            }],
            default_model: "fixture-provider-v1".to_owned(),
            runtime_binding: "claude-code-test".to_owned(),
            claude_code_version: "2.1.207".to_owned(),
            max_in_flight_per_worker: 2,
        },
    })
}

fn age_binary(name: &str, fallback: &str) -> Result<String, Box<dyn std::error::Error>> {
    let value = std::env::var(name).unwrap_or_else(|_| fallback.to_owned());
    if value.trim().is_empty() {
        return Err(format!("LW_CONTROL_TEST_AGE_BINARY_MISSING:{name}").into());
    }
    Ok(value)
}

pub(super) fn ciphertext() -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let identity = directory.path().join("synthetic-identity");
    let plaintext = directory.path().join("synthetic-plaintext");
    let archive = directory.path().join("synthetic.age");
    let age_keygen = age_binary("AGE_KEYGEN", "age-keygen")?;
    let age = age_binary("AGE", "age")?;
    let generate = std::process::Command::new(&age_keygen)
        .arg("-o")
        .arg(&identity)
        .output()
        .map_err(|_| "LW_CONTROL_TEST_AGE_KEYGEN_MISSING")?;
    if !generate.status.success() {
        return Err("LW_CONTROL_TEST_AGE_KEYGEN_FAILED".into());
    }
    let recipient = std::process::Command::new(&age_keygen)
        .arg("-y")
        .arg(&identity)
        .output()
        .map_err(|_| "LW_CONTROL_TEST_AGE_KEYGEN_MISSING")?;
    if !recipient.status.success() {
        return Err("LW_CONTROL_TEST_AGE_RECIPIENT_FAILED".into());
    }
    std::fs::write(&plaintext, b"synthetic-only-configuration-and-secret")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&plaintext, std::fs::Permissions::from_mode(0o600))?;
    }
    let encrypted = std::process::Command::new(&age)
        .arg("-r")
        .arg(String::from_utf8(recipient.stdout)?.trim())
        .arg("-o")
        .arg(&archive)
        .arg(&plaintext)
        .output()
        .map_err(|_| "LW_CONTROL_TEST_AGE_MISSING")?;
    if !encrypted.status.success() {
        return Err("LW_CONTROL_TEST_AGE_ENCRYPTION_FAILED".into());
    }
    Ok(std::fs::read(archive)?)
}

/// Full age/Bundle producer reused by actual loading and consumer integration targets.
#[allow(
    dead_code,
    reason = "other targets reuse the shared fixture without executing the full Bundle producer"
)]
pub(super) fn actual_loaded_bundle(
    version: &contracts::platform_configuration::PlatformConfigurationVersion,
    root: &std::path::Path,
) -> Result<
    (
        Vec<u8>,
        control_service::platform_configuration::LoadedConfiguration,
    ),
    Box<dyn std::error::Error>,
> {
    actual_loaded_bundle_replicas(version, root, 1)
}

/// Uses the same single Bundle with an explicit Control replica count.
#[allow(
    dead_code,
    reason = "only replica integration targets need a non-default replica count"
)]
pub(super) fn actual_loaded_bundle_replicas(
    version: &contracts::platform_configuration::PlatformConfigurationVersion,
    root: &std::path::Path,
    replicas: u32,
) -> Result<
    (
        Vec<u8>,
        control_service::platform_configuration::LoadedConfiguration,
    ),
    Box<dyn std::error::Error>,
> {
    std::fs::create_dir_all(root.join(".private"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            root.join(".private"),
            std::fs::Permissions::from_mode(0o700),
        )?;
    }
    let input = root.join(".private/version.json");
    std::fs::write(&input, serde_json::to_vec(version)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&input, std::fs::Permissions::from_mode(0o600))?;
    }
    let output = root.join(".private/loaded");
    let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let producer = repository.join("tests/ansible/prepare_loaded_configuration_fixture.py");
    let command = format!(
        "python3 {} --version {} --output {} --control-replicas {}",
        producer.display(),
        input.display(),
        output.display(),
        replicas
    );
    let result = std::process::Command::new("python3")
        .arg(&producer)
        .arg("--version")
        .arg(&input)
        .arg("--output")
        .arg(&output)
        .arg("--control-replicas")
        .arg(replicas.to_string())
        .output()
        .map_err(|error| {
            format!("LW_BUNDLE_TEST_PRODUCER_SPAWN_FAILED command={command} error={error}")
        })?;
    if !result.status.success() {
        return Err(format!(
            "LW_BUNDLE_TEST_PRODUCER_FAILED\ncommand={command}\nexit_code={:?}\nstderr:\n{}",
            result.status.code(),
            String::from_utf8_lossy(&result.stderr).trim()
        )
        .into());
    }
    assert!(result.stdout.is_empty());
    let cipher = std::fs::read(output.join("synthetic.age"))?;
    let loaded = control_service::platform_configuration::LoadedConfiguration::load(
        &output.join("platform-configuration.json"),
        &output.join("platform-configuration-source.json"),
    )?;
    Ok((cipher, loaded))
}
