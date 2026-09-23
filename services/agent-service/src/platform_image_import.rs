//! Administrator image import into the platform image catalog.
//!
//! The archive is staged by Control and read back from the immutable object store by exact
//! version. This module verifies every entry and blob digest before pushing the exact manifest
//! by digest, tags the reviewed reference, and registers the catalog pin. The registry push
//! authority stays in the Agent; Control never holds registry credentials.
//!
//! Two archive shapes are accepted. Without a declared disk descriptor the archive is a frozen
//! single-image OCI layout, imported as it was reviewed. With one, the archive holds the raw or
//! qcow2 virtual-machine disk itself: the disk is verified against the declared path and
//! capacity, wrapped into the single-layer containerdisk shape `KubeVirt`'s CDI pulls, and pinned
//! with the digest of the exact disk bytes.

use std::io::{Cursor, Read};

use artifact_store::{ImmutableObjectStore, ObjectStoreError};
use contracts::UtcTimestamp;
use contracts::http::{InternalPlatformImageImportRequest, PlatformImageEntry};
use persistence_sqlx::Sha256Digest;
use thiserror::Error;

use crate::containerdisk::wrap_containerdisk;
use crate::oci_import::{OciImportError, parse_oci_layout};
use crate::platform_images::{
    PgPlatformImageCatalog, PlatformImageRegistry, PlatformImageRegistryError,
    PlatformImageStoreError, RegisterPlatformImage,
};

/// Administrator archive import failure.
#[derive(Debug, Error)]
pub enum PlatformImageImportError {
    /// The staged archive could not be read back from the immutable object store.
    #[error("LW_AGENT_PERSISTENCE_FAILED")]
    ObjectStore(#[from] ObjectStoreError),
    /// The archive is not a verified single-image OCI layout.
    #[error(transparent)]
    Layout(#[from] OciImportError),
    /// The archive does not hold the declared virtual-machine disk.
    #[error("LW_PLATFORM_IMAGE_DISK_INVALID")]
    Disk,
    /// The declared virtual-machine disk capacity is not satisfied by the staged disk.
    #[error("LW_PLATFORM_IMAGE_CAPACITY_INVALID")]
    Capacity,
    /// The registry refused the push or the reviewed reference.
    #[error(transparent)]
    Registry(#[from] PlatformImageRegistryError),
    /// The catalog refused the registration.
    #[error(transparent)]
    Catalog(#[from] PlatformImageStoreError),
}

impl PlatformImageImportError {
    /// Stable diagnostic code for API error mapping.
    #[must_use]
    pub fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::ObjectStore(_) => "LW_AGENT_PERSISTENCE_FAILED",
            Self::Layout(error) => error.diagnostic_code(),
            Self::Disk => "LW_PLATFORM_IMAGE_DISK_INVALID",
            Self::Capacity => "LW_PLATFORM_IMAGE_CAPACITY_INVALID",
            Self::Registry(error) => error.diagnostic_code(),
            Self::Catalog(error) => error.diagnostic_code(),
        }
    }
}

/// Imports one staged archive into the configured registry and catalog.
///
/// The push happens before the catalog write, so a registry failure never leaves a pinned
/// digest that the registry cannot resolve, and the catalog write is the only step that makes
/// the binding visible to authoring. Every verification runs before the push, so a rejected
/// upload leaves both the registry and the catalog untouched.
///
/// # Errors
///
/// Fails closed on an unreadable or mismatched archive object, a malformed OCI layout, a disk
/// archive that is not the declared disk, a capacity violation, a registry rejection, or a
/// conflicting catalog binding.
pub async fn import_platform_image(
    registry: &PlatformImageRegistry,
    catalog: &PgPlatformImageCatalog,
    objects: &dyn ImmutableObjectStore,
    request: &InternalPlatformImageImportRequest,
    now: UtcTimestamp,
) -> Result<PlatformImageEntry, PlatformImageImportError> {
    let object = objects
        .read_verified(&request.archive_object_key, &request.archive)
        .await?;
    if request.disk_format.is_none()
        && request.disk_path.is_none()
        && request.capacity_bytes.is_none()
    {
        import_oci_layout(registry, catalog, &object.bytes, request, now).await
    } else {
        import_vm_disk(registry, catalog, &object.bytes, request, now).await
    }
}

/// Publishes one verified OCI layout archive and pins its manifest identity.
async fn import_oci_layout(
    registry: &PlatformImageRegistry,
    catalog: &PgPlatformImageCatalog,
    archive: &[u8],
    request: &InternalPlatformImageImportRequest,
    now: UtcTimestamp,
) -> Result<PlatformImageEntry, PlatformImageImportError> {
    let image = parse_oci_layout(archive)?;
    let digest = registry.publish(&request.target_reference, &image).await?;
    let size_bytes = image
        .blobs
        .iter()
        .map(|blob| u64::try_from(blob.bytes.len()).unwrap_or(u64::MAX))
        .sum();
    let entry = catalog
        .register(&RegisterPlatformImage {
            kind: request.kind,
            binding: request.binding.clone(),
            source_reference: request.target_reference.clone(),
            resolved_digest: digest,
            media_type: image.manifest_media_type.clone(),
            size_bytes,
            capacity_bytes: None,
            disk_sha256: None,
            format: None,
            trust_revision: request.trust_revision,
            actor_id: request.actor_id,
            reason: request.reason.clone(),
            now,
        })
        .await?;
    Ok(entry)
}

/// Wraps one declared virtual-machine base disk and pins the containerdisk identity.
async fn import_vm_disk(
    registry: &PlatformImageRegistry,
    catalog: &PgPlatformImageCatalog,
    archive: &[u8],
    request: &InternalPlatformImageImportRequest,
    now: UtcTimestamp,
) -> Result<PlatformImageEntry, PlatformImageImportError> {
    let disk_path = request.disk_path.as_deref().unwrap_or_default();
    if !contracts::http::valid_vm_disk_upload(
        request.kind,
        request.disk_format,
        request.disk_path.as_deref(),
        request.capacity_bytes,
    ) {
        return Err(PlatformImageImportError::Disk);
    }
    let capacity_bytes = request
        .capacity_bytes
        .ok_or(PlatformImageImportError::Disk)?;
    let disk = read_declared_disk(archive, disk_path, capacity_bytes)?;
    let disk_sha256 = Sha256Digest::of_bytes(&disk).to_string();
    let image = wrap_containerdisk(&disk, disk_path)?;
    let digest = registry.publish(&request.target_reference, &image).await?;
    let size_bytes = image
        .blobs
        .iter()
        .map(|blob| u64::try_from(blob.bytes.len()).unwrap_or(u64::MAX))
        .sum();
    let entry = catalog
        .register(&RegisterPlatformImage {
            kind: request.kind,
            binding: request.binding.clone(),
            source_reference: request.target_reference.clone(),
            resolved_digest: digest,
            media_type: image.manifest_media_type.clone(),
            size_bytes,
            capacity_bytes: Some(capacity_bytes),
            disk_sha256: Some(disk_sha256),
            format: request.disk_format,
            trust_revision: request.trust_revision,
            actor_id: request.actor_id,
            reason: request.reason.clone(),
            now,
        })
        .await?;
    Ok(entry)
}

/// Reads the one regular-file entry the reviewed disk archive declares.
///
/// The archive must hold exactly one entry, it must be a regular file whose stored name equals
/// `disk_path` byte for byte, and its declared size must be positive and at most `capacity_bytes`
/// so a misdeclared upload can never be wrapped, published, or pinned. Each entry is read as it
/// is visited: the tar reader is not seekable here, so an entry may only be read before the
/// iteration advances.
fn read_declared_disk(
    archive_bytes: &[u8],
    disk_path: &str,
    capacity_bytes: u64,
) -> Result<Vec<u8>, PlatformImageImportError> {
    let mut archive = tar::Archive::new(Cursor::new(archive_bytes));
    let entries = archive
        .entries()
        .map_err(|_| PlatformImageImportError::Disk)?;
    let mut disk: Option<Vec<u8>> = None;
    for entry in entries {
        let entry = entry.map_err(|_| PlatformImageImportError::Disk)?;
        if !entry.header().entry_type().is_file() {
            return Err(PlatformImageImportError::Disk);
        }
        let name = entry
            .path()
            .map_err(|_| PlatformImageImportError::Disk)?
            .to_str()
            .map(str::to_owned)
            .ok_or(PlatformImageImportError::Disk)?;
        if name != disk_path || disk.is_some() {
            return Err(PlatformImageImportError::Disk);
        }
        let declared_size = entry.size();
        if declared_size == 0 || declared_size > capacity_bytes {
            return Err(PlatformImageImportError::Capacity);
        }
        let mut bytes = Vec::with_capacity(
            usize::try_from(declared_size).map_err(|_| PlatformImageImportError::Capacity)?,
        );
        entry
            .take(declared_size.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|_| PlatformImageImportError::Disk)?;
        if u64::try_from(bytes.len()).ok() != Some(declared_size) {
            return Err(PlatformImageImportError::Disk);
        }
        disk = Some(bytes);
    }
    disk.ok_or(PlatformImageImportError::Disk)
}
