//! Reviewed VM base resolution across the deployment policy and the Agent image catalog.
//!
//! Control owns the reviewed policy bounds; the Agent owns the catalog identities. A virtual
//! machine candidate is publishable only when its declared base matches an exact reviewed entry
//! from one of the two sources, and every catalog that violates a deployment bound fails closed
//! instead of being silently truncated.

use contracts::http::{PlatformImageEntry, PlatformImageKind, PlatformImageStatus};
use contracts::supply_chain::{ImageArtifact, VirtualMachineBaseDisk, VirtualMachineDiskFormat};
use contracts::{ImageArtifactId, PlatformImageId, UtcTimestamp};
use control_service::{VirtualMachineBaseCatalog, VirtualMachineBasePolicy};

const PROVIDER: &str = "kubevirt-primary-v1";
const STORAGE: &str = "vm-rwo-primary-v1";
const STATIC_BINDING: &str = "ubuntu-24.04-v1";
const STATIC_DIGEST: &str =
    "sha256:d28194a16351320fa9a093e18233033508a745566eb8ba3b309c32924bf155a5";
const CATALOG_BINDING: &str = "rocky-9-v1";
const CATALOG_DIGEST: &str =
    "sha256:1d6d3f5cd1d3d1a26f5c0a7b7d2d5a6e1f4f3a2b1c0d9e8f7a6b5c4d3e2f1a0b";
const CATALOG_CAPACITY_BYTES: u64 = 21_474_836_480;
const MAX_CAPACITY_BYTES: u64 = 137_438_953_472;
const DISK_SHA256: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn timestamp() -> Result<UtcTimestamp, Box<dyn std::error::Error>> {
    Ok("2026-08-01T00:00:00.000Z".parse()?)
}

fn base_disk(binding: &str, digest: &str, capacity_bytes: u64) -> VirtualMachineBaseDisk {
    VirtualMachineBaseDisk {
        binding: binding.to_owned(),
        source_registry_digest: format!("docker://quay.io/containerdisks/{binding}@{digest}"),
        capacity_bytes,
    }
}

fn static_catalog() -> VirtualMachineBaseCatalog {
    VirtualMachineBaseCatalog {
        provider_binding: PROVIDER.to_owned(),
        storage_class_binding: STORAGE.to_owned(),
        max_bases: 8,
        max_capacity_bytes: MAX_CAPACITY_BYTES,
        bases: vec![VirtualMachineBasePolicy {
            artifact_id: ImageArtifactId::new(),
            base_disk: base_disk(STATIC_BINDING, STATIC_DIGEST, 10_737_418_240),
            format: VirtualMachineDiskFormat::Qcow2,
        }],
    }
}

#[allow(clippy::too_many_arguments)]
fn image_entry(
    binding: &str,
    digest: &str,
    capacity_bytes: Option<u64>,
    format: Option<VirtualMachineDiskFormat>,
    disk_sha256: Option<&str>,
    kind: PlatformImageKind,
    status: PlatformImageStatus,
    now: UtcTimestamp,
) -> PlatformImageEntry {
    PlatformImageEntry {
        catalog_id: PlatformImageId::new(),
        kind,
        binding: binding.to_owned(),
        source_reference: format!("harbor.example.invalid/labweaver-system/{binding}:1"),
        resolved_digest: digest.to_owned(),
        media_type: "application/vnd.oci.image.manifest.v1+json".to_owned(),
        size_bytes: capacity_bytes.unwrap_or(0),
        status,
        trust_revision: 1,
        repin_generation: 1,
        capacity_bytes,
        disk_sha256: disk_sha256.map(str::to_owned),
        format,
        pinned_at: now,
        updated_at: now,
    }
}

fn cataloged_vm_entry(status: PlatformImageStatus, now: UtcTimestamp) -> PlatformImageEntry {
    image_entry(
        CATALOG_BINDING,
        CATALOG_DIGEST,
        Some(CATALOG_CAPACITY_BYTES),
        Some(VirtualMachineDiskFormat::Raw),
        Some(DISK_SHA256),
        PlatformImageKind::VirtualMachine,
        status,
        now,
    )
}

/// Mirrors the reviewed resolution Control performs before an approval is accepted.
fn resolved_artifact(
    catalog: &VirtualMachineBaseCatalog,
    provider_binding: &str,
    storage_class_binding: &str,
    declared: &VirtualMachineBaseDisk,
    images: &[PlatformImageEntry],
) -> Option<ImageArtifact> {
    catalog
        .resolve_with_catalog(provider_binding, storage_class_binding, declared, images)
        .map(|(id, format)| ImageArtifact::VirtualMachine {
            id,
            base_disk: declared.clone(),
            format,
        })
}

fn catalog_artifact_id(
    entry: &PlatformImageEntry,
) -> Result<ImageArtifactId, Box<dyn std::error::Error>> {
    Ok(entry.catalog_id.to_string().parse()?)
}

#[test]
fn active_catalog_entry_resolves_with_its_catalog_identity() -> TestResult {
    let now = timestamp()?;
    let catalog = static_catalog();
    let images = vec![cataloged_vm_entry(PlatformImageStatus::Active, now)];
    let declared = base_disk(CATALOG_BINDING, CATALOG_DIGEST, CATALOG_CAPACITY_BYTES);
    let resolved = resolved_artifact(&catalog, PROVIDER, STORAGE, &declared, &images);

    assert_eq!(
        resolved,
        Some(ImageArtifact::VirtualMachine {
            id: catalog_artifact_id(&images[0])?,
            base_disk: declared,
            format: VirtualMachineDiskFormat::Raw,
        })
    );
    assert_ne!(
        resolved.map(|artifact| artifact.id()),
        Some(catalog.bases[0].artifact_id),
        "a catalog entry must never inherit the static artifact identity"
    );
    Ok(())
}

#[test]
fn static_policy_entry_resolves_exactly_and_wins_over_the_catalog() -> TestResult {
    let now = timestamp()?;
    let catalog = static_catalog();
    let declared = catalog.bases[0].base_disk.clone();
    let expected = ImageArtifact::VirtualMachine {
        id: catalog.bases[0].artifact_id,
        base_disk: declared.clone(),
        format: VirtualMachineDiskFormat::Qcow2,
    };
    assert_eq!(
        resolved_artifact(&catalog, PROVIDER, STORAGE, &declared, &[]),
        Some(expected.clone())
    );
    // A catalog entry that shadows the static binding never changes the reviewed resolution,
    // whether it is active or disabled.
    for status in [PlatformImageStatus::Active, PlatformImageStatus::Disabled] {
        let shadowing = vec![image_entry(
            STATIC_BINDING,
            CATALOG_DIGEST,
            Some(declared.capacity_bytes),
            Some(VirtualMachineDiskFormat::Raw),
            Some(DISK_SHA256),
            PlatformImageKind::VirtualMachine,
            status,
            now,
        )];
        assert_eq!(
            resolved_artifact(&catalog, PROVIDER, STORAGE, &declared, &shadowing),
            Some(expected.clone())
        );
    }
    Ok(())
}

#[test]
fn catalog_entry_requires_the_full_reviewed_disk_descriptor() -> TestResult {
    let now = timestamp()?;
    let catalog = static_catalog();
    let declared = base_disk(CATALOG_BINDING, CATALOG_DIGEST, CATALOG_CAPACITY_BYTES);
    assert!(
        resolved_artifact(
            &catalog,
            PROVIDER,
            STORAGE,
            &declared,
            &[cataloged_vm_entry(PlatformImageStatus::Active, now)]
        )
        .is_some()
    );

    let mut drifted_digest = cataloged_vm_entry(PlatformImageStatus::Active, now);
    drifted_digest.resolved_digest = format!("sha256:{}", "e".repeat(64));
    let mut capacity_drift = cataloged_vm_entry(PlatformImageStatus::Active, now);
    capacity_drift.capacity_bytes = Some(declared.capacity_bytes + 1);
    let mut missing_format = cataloged_vm_entry(PlatformImageStatus::Active, now);
    missing_format.format = None;
    let mut inventory_only = cataloged_vm_entry(PlatformImageStatus::Active, now);
    inventory_only.disk_sha256 = None;
    let mut missing_capacity = cataloged_vm_entry(PlatformImageStatus::Active, now);
    missing_capacity.capacity_bytes = None;
    let mut container_entry = cataloged_vm_entry(PlatformImageStatus::Active, now);
    container_entry.kind = PlatformImageKind::Container;

    for (label, entry) in [
        ("a drifted digest", drifted_digest),
        ("a different capacity", capacity_drift),
        ("no declared format", missing_format),
        ("no unpacked disk digest", inventory_only),
        ("no reviewed capacity", missing_capacity),
        ("a container kind", container_entry),
        (
            "a disabled status",
            cataloged_vm_entry(PlatformImageStatus::Disabled, now),
        ),
    ] {
        assert_eq!(
            resolved_artifact(&catalog, PROVIDER, STORAGE, &declared, &[entry]),
            None,
            "a catalog entry with {label} must not be resolvable"
        );
    }
    Ok(())
}

#[test]
fn declared_artifact_format_must_match_the_resolved_catalog_entry() -> TestResult {
    let now = timestamp()?;
    let catalog = static_catalog();
    let declared = base_disk(CATALOG_BINDING, CATALOG_DIGEST, CATALOG_CAPACITY_BYTES);
    let images = vec![cataloged_vm_entry(PlatformImageStatus::Active, now)];
    let accepted = resolved_artifact(&catalog, PROVIDER, STORAGE, &declared, &images)
        .ok_or("the active catalog entry must resolve")?;
    assert_eq!(
        accepted,
        ImageArtifact::VirtualMachine {
            id: catalog_artifact_id(&images[0])?,
            base_disk: declared.clone(),
            format: VirtualMachineDiskFormat::Raw,
        }
    );
    let misdeclared = ImageArtifact::VirtualMachine {
        id: catalog_artifact_id(&images[0])?,
        base_disk: declared,
        format: VirtualMachineDiskFormat::Qcow2,
    };
    assert_ne!(accepted, misdeclared);
    Ok(())
}

#[test]
fn provider_or_storage_binding_mismatch_is_rejected() -> TestResult {
    let now = timestamp()?;
    let catalog = static_catalog();
    let declared = base_disk(CATALOG_BINDING, CATALOG_DIGEST, CATALOG_CAPACITY_BYTES);
    let images = vec![cataloged_vm_entry(PlatformImageStatus::Active, now)];
    assert_eq!(
        resolved_artifact(&catalog, "other-provider", STORAGE, &declared, &images),
        None
    );
    assert_eq!(
        resolved_artifact(&catalog, PROVIDER, "other-storage", &declared, &images),
        None
    );
    Ok(())
}

#[test]
fn catalog_bounds_fail_closed_across_both_sources() -> TestResult {
    let now = timestamp()?;
    let catalog = static_catalog();
    let declared = base_disk(CATALOG_BINDING, CATALOG_DIGEST, CATALOG_CAPACITY_BYTES);

    let mut over_capacity = cataloged_vm_entry(PlatformImageStatus::Active, now);
    over_capacity.capacity_bytes = Some(MAX_CAPACITY_BYTES + 1);
    assert_eq!(
        resolved_artifact(&catalog, PROVIDER, STORAGE, &declared, &[over_capacity]),
        None,
        "a catalog capacity above the reviewed maximum must fail closed"
    );

    let mut second = cataloged_vm_entry(PlatformImageStatus::Active, now);
    second.binding = "debian-13-v1".to_owned();
    second.resolved_digest = format!("sha256:{}", "f".repeat(64));
    let mut over_bases = catalog.clone();
    over_bases.max_bases = 2;
    let images = vec![cataloged_vm_entry(PlatformImageStatus::Active, now), second];
    assert_eq!(
        resolved_artifact(&over_bases, PROVIDER, STORAGE, &declared, &images),
        None,
        "static bases plus distinct active catalog bindings must fit max_bases"
    );
    // The same bound rejects a static resolution instead of truncating the catalog.
    let static_declared = catalog.bases[0].base_disk.clone();
    assert_eq!(
        resolved_artifact(&over_bases, PROVIDER, STORAGE, &static_declared, &images),
        None
    );
    // One static base plus one active catalog binding still fits the same deployment.
    assert!(
        resolved_artifact(&over_bases, PROVIDER, STORAGE, &declared, &images[..1]).is_some(),
        "a catalog inside both bounds must still resolve"
    );

    // A capacity bound is a deployment property, so an unrelated oversized entry is rejected too.
    let mut oversized_foreign = cataloged_vm_entry(PlatformImageStatus::Active, now);
    oversized_foreign.binding = "debian-13-v1".to_owned();
    oversized_foreign.capacity_bytes = Some(MAX_CAPACITY_BYTES + 1);
    assert_eq!(
        resolved_artifact(
            &catalog,
            PROVIDER,
            STORAGE,
            &static_declared,
            &[oversized_foreign]
        ),
        None
    );
    Ok(())
}
