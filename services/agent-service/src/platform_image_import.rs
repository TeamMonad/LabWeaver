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

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::sync::{Arc, OnceLock};

use artifact_store::{ImmutableObjectStore, ObjectStoreError};
use contracts::UtcTimestamp;
use contracts::http::{InternalPlatformImageImportRequest, PlatformImageEntry};
use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use tempfile::{NamedTempFile, TempPath};
use thiserror::Error;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::containerdisk::wrap_containerdisk_file;
use crate::oci_import::{OciImportError, OciLimits, parse_oci_layout_file};
use crate::platform_images::{
    PgPlatformImageCatalog, PlatformImageRegistry, PlatformImageRegistryError,
    PlatformImageStoreError, RegisterPlatformImage,
};

const MAX_TAR_EXTENSION_BYTES: u64 = 64 * 1024;
// Keep gzip expansion bounded by the same 8 GiB total-entry budget used by OCI imports. The
// staged archive itself remains capped by the object-store limit, while VM capacity remains a
// separate product declaration.
const MAX_EXPANDED_DISK_BYTES: u64 = 8 * 1024 * 1024 * 1024;

// A VM image import needs an archive, extracted disk, and OCI layer on ephemeral storage at
// once. Keep this admission gate process-wide so concurrent admin requests cannot each consume
// the same bounded import budget. The permit is held across the object download, conversion,
// registry push, and catalog write and is released when the future is cancelled or finishes.
static VM_IMPORT_GATE: OnceLock<Arc<Semaphore>> = OnceLock::new();

fn vm_import_gate() -> Arc<Semaphore> {
    VM_IMPORT_GATE
        .get_or_init(|| Arc::new(Semaphore::new(1)))
        .clone()
}

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
    let permit = vm_import_gate()
        .acquire_owned()
        .await
        .map_err(|_| PlatformImageImportError::ObjectStore(ObjectStoreError::ObjectUnavailable))?;
    let object = objects
        .read_verified_file(&request.archive_object_key, &request.archive)
        .await?;
    if request.disk_format.is_none()
        && request.disk_path.is_none()
        && request.capacity_bytes.is_none()
    {
        import_oci_layout_file(registry, catalog, object, request, now, permit).await
    } else {
        import_vm_disk_file(registry, catalog, object, request, now, permit).await
    }
}

/// Publishes one verified OCI layout archive and pins its manifest identity.
async fn import_oci_layout_file(
    registry: &PlatformImageRegistry,
    catalog: &PgPlatformImageCatalog,
    archive: artifact_store::VerifiedObjectFile,
    request: &InternalPlatformImageImportRequest,
    now: UtcTimestamp,
    permit: OwnedSemaphorePermit,
) -> Result<PlatformImageEntry, PlatformImageImportError> {
    let archive_path = archive.path().to_owned();
    let (image, _permit) = tokio::task::spawn_blocking(move || {
        let result = parse_oci_layout_file(&archive_path, OciLimits::default());
        drop(archive);
        result.map(|image| (image, permit))
    })
    .await
    .map_err(|_| PlatformImageImportError::Layout(OciImportError::Invalid))??;
    let digest = registry
        .publish_file(&request.target_reference, &image)
        .await?;
    let size_bytes = image
        .blobs
        .iter()
        .map(|blob| blob.size_bytes)
        .try_fold(0_u64, u64::checked_add)
        .ok_or(PlatformImageImportError::Capacity)?;
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

/// Imports one large VM-disk archive from a temporary object-store file.
async fn import_vm_disk_file(
    registry: &PlatformImageRegistry,
    catalog: &PgPlatformImageCatalog,
    archive: artifact_store::VerifiedObjectFile,
    request: &InternalPlatformImageImportRequest,
    now: UtcTimestamp,
    permit: OwnedSemaphorePermit,
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
    let archive_path = archive.path().to_owned();
    let disk_path_owned = disk_path.to_owned();
    let (disk, permit) = tokio::task::spawn_blocking(move || {
        let result = read_declared_disk_file(&archive_path, &disk_path_owned, capacity_bytes);
        drop(archive);
        result.map(|disk| (disk, permit))
    })
    .await
    .map_err(|_| PlatformImageImportError::Disk)??;
    let DiskFile {
        path: disk_path_file,
        sha256: disk_sha256,
    } = disk;
    let disk_path_owned = disk_path.to_owned();
    let (image, _permit) = tokio::task::spawn_blocking(move || {
        let result = wrap_containerdisk_file(disk_path_file.as_ref(), &disk_path_owned);
        result.map(|image| (image, permit))
    })
    .await
    .map_err(|_| PlatformImageImportError::Disk)??;
    let digest = registry
        .publish_file(&request.target_reference, &image)
        .await?;
    let size_bytes = image
        .blobs
        .iter()
        .map(|blob| blob.size_bytes)
        .try_fold(0_u64, u64::checked_add)
        .ok_or(PlatformImageImportError::Capacity)?;
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

struct DiskFile {
    path: TempPath,
    sha256: String,
}

fn read_declared_disk_file(
    archive_path: &std::path::Path,
    disk_path: &str,
    capacity_bytes: u64,
) -> Result<DiskFile, PlatformImageImportError> {
    let mut source = File::open(archive_path).map_err(|_| PlatformImageImportError::Disk)?;
    let mut magic = [0_u8; 2];
    let is_gzip = source
        .read_exact(&mut magic)
        .is_ok_and(|()| magic == [0x1f, 0x8b]);
    source
        .seek(SeekFrom::Start(0))
        .map_err(|_| PlatformImageImportError::Disk)?;
    let reader: Box<dyn Read> = if is_gzip {
        Box::new(GzDecoder::new(source))
    } else {
        Box::new(source)
    };
    let mut archive = tar::Archive::new(reader);
    let entries = archive
        .entries()
        .map_err(|_| PlatformImageImportError::Disk)?
        .raw(true);
    let mut disk: Option<DiskFile> = None;
    for entry in entries {
        let mut entry = entry.map_err(|_| PlatformImageImportError::Disk)?;
        let entry_type = entry.header().entry_type();
        if entry_type.is_pax_local_extensions() || entry_type.is_pax_global_extensions() {
            consume_pax_extension(&mut entry)?;
            continue;
        }
        if entry_type.is_gnu_longname() || entry_type.is_gnu_longlink() {
            return Err(PlatformImageImportError::Disk);
        }
        if !entry.header().entry_type().is_file() || entry_type.is_gnu_sparse() || disk.is_some() {
            return Err(PlatformImageImportError::Disk);
        }
        if let Some(extensions) = entry
            .pax_extensions()
            .map_err(|_| PlatformImageImportError::Disk)?
        {
            for extension in extensions {
                let extension = extension.map_err(|_| PlatformImageImportError::Disk)?;
                if extension.key_bytes().starts_with(b"GNU.sparse.") {
                    return Err(PlatformImageImportError::Disk);
                }
            }
        }
        let entry_path = entry.path().map_err(|_| PlatformImageImportError::Disk)?;
        let name = entry_path.to_str().ok_or(PlatformImageImportError::Disk)?;
        if name != disk_path {
            return Err(PlatformImageImportError::Disk);
        }
        let declared_size = entry.size();
        if declared_size == 0 {
            return Err(PlatformImageImportError::Capacity);
        }
        if declared_size > capacity_bytes || declared_size > MAX_EXPANDED_DISK_BYTES {
            return Err(PlatformImageImportError::Capacity);
        }
        let temporary = NamedTempFile::new().map_err(|_| PlatformImageImportError::Disk)?;
        let mut output = temporary
            .reopen()
            .map_err(|_| PlatformImageImportError::Disk)?;
        let mut input = entry.take(declared_size.saturating_add(1));
        let mut buffer = vec![0_u8; 1024 * 1024];
        let mut copied = 0_u64;
        let mut hasher = Sha256::new();
        loop {
            let read = input
                .read(&mut buffer)
                .map_err(|_| PlatformImageImportError::Disk)?;
            if read == 0 {
                break;
            }
            copied = copied
                .checked_add(u64::try_from(read).map_err(|_| PlatformImageImportError::Capacity)?)
                .ok_or(PlatformImageImportError::Capacity)?;
            if copied > declared_size {
                return Err(PlatformImageImportError::Disk);
            }
            std::io::Write::write_all(&mut output, &buffer[..read])
                .map_err(|_| PlatformImageImportError::Disk)?;
            hasher.update(&buffer[..read]);
        }
        if copied != declared_size {
            return Err(PlatformImageImportError::Disk);
        }
        output
            .sync_all()
            .map_err(|_| PlatformImageImportError::Disk)?;
        disk = Some(DiskFile {
            path: temporary.into_temp_path(),
            sha256: format!("{:x}", hasher.finalize()),
        });
    }
    disk.ok_or(PlatformImageImportError::Disk)
}

fn consume_pax_extension<R: Read>(
    entry: &mut tar::Entry<'_, R>,
) -> Result<(), PlatformImageImportError> {
    if entry.size() > MAX_TAR_EXTENSION_BYTES {
        return Err(PlatformImageImportError::Capacity);
    }
    let bytes = {
        let mut bytes = Vec::new();
        entry
            .take(MAX_TAR_EXTENSION_BYTES.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|_| PlatformImageImportError::Disk)?;
        bytes
    };
    if u64::try_from(bytes.len()).ok() != Some(entry.size()) {
        return Err(PlatformImageImportError::Disk);
    }
    for record in bytes
        .split(|byte| *byte == b'\n')
        .filter(|record| !record.is_empty())
    {
        let separator = record
            .iter()
            .position(|byte| *byte == b' ')
            .ok_or(PlatformImageImportError::Disk)?;
        let length = std::str::from_utf8(&record[..separator])
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .ok_or(PlatformImageImportError::Disk)?;
        if length != record.len().saturating_add(1) {
            return Err(PlatformImageImportError::Disk);
        }
        let key_end = record[separator + 1..]
            .iter()
            .position(|byte| *byte == b'=')
            .ok_or(PlatformImageImportError::Disk)?
            + separator
            + 1;
        let key = std::str::from_utf8(&record[separator + 1..key_end])
            .map_err(|_| PlatformImageImportError::Disk)?;
        if matches!(key, "path" | "linkpath" | "size") || key.starts_with("GNU.sparse.") {
            return Err(PlatformImageImportError::Disk);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use flate2::{Compression, write::GzEncoder};
    use tempfile::NamedTempFile;

    use super::{PlatformImageImportError, read_declared_disk_file};

    fn tar_bytes(
        entries: &[(&str, &[u8], tar::EntryType)],
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, bytes, entry_type) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(u64::try_from(bytes.len())?);
            header.set_entry_type(*entry_type);
            header.set_mode(0o600);
            if *entry_type == tar::EntryType::Symlink {
                header.set_link_name("/etc/passwd")?;
            }
            header.set_cksum();
            builder.append_data(&mut header, *path, *bytes)?;
        }
        Ok(builder.into_inner()?)
    }

    fn archive_file(bytes: &[u8]) -> Result<NamedTempFile, Box<dyn std::error::Error>> {
        let file = NamedTempFile::new()?;
        std::fs::write(file.path(), bytes)?;
        Ok(file)
    }

    #[test]
    fn streams_declared_disk_and_removes_temp_file_on_drop()
    -> Result<(), Box<dyn std::error::Error>> {
        let bytes = tar_bytes(&[("disk/disk.qcow2", b"disk contents", tar::EntryType::Regular)])?;
        let archive = archive_file(&bytes)?;
        let disk = read_declared_disk_file(archive.path(), "disk/disk.qcow2", 1024)?;
        let path = disk.path.to_path_buf();
        assert_eq!(std::fs::read(&path)?, b"disk contents");
        assert_eq!(disk.sha256.len(), 64);
        drop(disk);
        assert!(!path.exists());
        Ok(())
    }

    #[test]
    fn accepts_gzip_and_applies_the_declared_expanded_disk_bound()
    -> Result<(), Box<dyn std::error::Error>> {
        let bytes = tar_bytes(&[("disk/disk.qcow2", b"disk contents", tar::EntryType::Regular)])?;
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&bytes)?;
        let compressed = encoder.finish()?;
        let archive = archive_file(&compressed)?;
        let disk = read_declared_disk_file(archive.path(), "disk/disk.qcow2", 1024)?;
        drop(disk);
        let error = read_declared_disk_file(archive.path(), "disk/disk.qcow2", 1)
            .err()
            .ok_or("expanded disk exceeded archive budget")?;
        assert!(matches!(error, PlatformImageImportError::Capacity));
        Ok(())
    }

    #[test]
    fn rejects_links_and_multiple_archive_entries() -> Result<(), Box<dyn std::error::Error>> {
        let bytes = tar_bytes(&[
            ("disk/disk.qcow2", b"disk contents", tar::EntryType::Regular),
            ("disk/other", b"other", tar::EntryType::Regular),
        ])?;
        let archive = archive_file(&bytes)?;
        assert!(matches!(
            read_declared_disk_file(archive.path(), "disk/disk.qcow2", 1024),
            Err(PlatformImageImportError::Disk)
        ));

        let link_bytes = tar_bytes(&[("disk/disk.qcow2", b"", tar::EntryType::Symlink)])?;
        let link_archive = archive_file(&link_bytes)?;
        assert!(matches!(
            read_declared_disk_file(link_archive.path(), "disk/disk.qcow2", 1024),
            Err(PlatformImageImportError::Disk)
        ));
        Ok(())
    }
}
