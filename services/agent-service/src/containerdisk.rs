//! Containerdisk wrapper for an uploaded virtual-machine base disk.
//!
//! CDI imports a virtual-machine base disk from a registry by pulling one image and copying the
//! file its layer holds onto the target `DataVolume`. The catalog therefore stores exactly that
//! shape: this module wraps the reviewed raw/qcow2 bytes of an administrator upload into a
//! single-layer image whose tar holds `disk/<basename>` with the exact uploaded bytes. Every
//! digest is computed from the bytes built here, so the published manifest identity can only
//! describe content the importer actually received.

use std::fs::File;
use std::io::{Cursor, Read, Write};
use std::path::Path;

use contracts::http::PLATFORM_IMAGE_DISK_PATH_MAX_BYTES;
use persistence_sqlx::Sha256Digest;
use serde_json::json;
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;

use crate::oci_import::{OciBlob, OciImage, OciImportError};
use crate::oci_registry::{OciFileBlob, OciFileImage};

/// Media type of the containerdisk manifest.
const MANIFEST_MEDIA_TYPE: &str = "application/vnd.oci.image.manifest.v1+json";
/// Media type of the containerdisk config blob.
const CONFIG_MEDIA_TYPE: &str = "application/vnd.oci.image.config.v1+json";
/// Media type of the containerdisk layer: one uncompressed tar holding the disk.
const LAYER_MEDIA_TYPE: &str = "application/vnd.oci.image.layer.v1.tar";
/// Layer directory that holds the virtual-machine disk.
const DISK_DIRECTORY: &str = "disk";

/// Wraps one uploaded virtual-machine disk into a publishable containerdisk image.
///
/// The image carries one config blob and one uncompressed tar layer; the layer holds exactly one
/// regular-file entry, `disk/<basename of disk_path>`, with the raw disk bytes. The returned
/// manifest digest is the SHA-256 of the manifest bytes built here, which reference only the
/// digests of those two blobs.
///
/// # Errors
///
/// Returns [`OciImportError::Invalid`] for an empty disk, an empty or non-relative `disk_path`,
/// a path holding a `..` segment, or a path longer than
/// [`PLATFORM_IMAGE_DISK_PATH_MAX_BYTES`].
pub fn wrap_containerdisk(disk: &[u8], disk_path: &str) -> Result<OciImage, OciImportError> {
    if disk.is_empty() || !valid_disk_path(disk_path) {
        return Err(OciImportError::Invalid);
    }
    let entry_name = format!(
        "{DISK_DIRECTORY}/{}",
        disk_path.rsplit('/').next().unwrap_or_default()
    );
    let layer = tar_layer(&entry_name, disk)?;
    let layer_digest = digest_reference(&layer);
    let config = serde_json::to_vec(&json!({
        // The disk bytes are opaque, so the config only satisfies the image config media type:
        // the platform's virtual machines run on linux/amd64 hosts and CDI copies layer content
        // without executing the image.
        "architecture": "amd64",
        "os": "linux",
        "rootfs": {"type": "layers", "diff_ids": [layer_digest.clone()]},
    }))
    .map_err(|_| OciImportError::Invalid)?;
    let config_digest = digest_reference(&config);
    let manifest = serde_json::to_vec(&json!({
        "schemaVersion": 2,
        "mediaType": MANIFEST_MEDIA_TYPE,
        "config": {
            "mediaType": CONFIG_MEDIA_TYPE,
            "digest": config_digest,
            "size": config.len(),
        },
        "layers": [{
            "mediaType": LAYER_MEDIA_TYPE,
            "digest": layer_digest,
            "size": layer.len(),
        }],
    }))
    .map_err(|_| OciImportError::Invalid)?;
    let manifest_digest = digest_reference(&manifest);
    Ok(OciImage {
        manifest_digest,
        manifest_media_type: MANIFEST_MEDIA_TYPE.to_owned(),
        manifest_bytes: manifest,
        config_digest: config_digest.clone(),
        blobs: vec![
            OciBlob {
                digest: config_digest,
                media_type: CONFIG_MEDIA_TYPE.to_owned(),
                bytes: config,
            },
            OciBlob {
                digest: layer_digest,
                media_type: LAYER_MEDIA_TYPE.to_owned(),
                bytes: layer,
            },
        ],
    })
}

/// Wraps one file-backed virtual-machine disk without retaining its bytes in memory.
///
/// The caller runs this synchronous tar operation on a blocking worker.  The returned temporary
/// files own cleanup until the registry has confirmed the manifest and tag.
pub fn wrap_containerdisk_file(
    disk_file: &Path,
    disk_path: &str,
) -> Result<OciFileImage, OciImportError> {
    if !valid_disk_path(disk_path) {
        return Err(OciImportError::Invalid);
    }
    let disk_size = File::open(disk_file)
        .and_then(|file| file.metadata())
        .map_err(|_| OciImportError::Invalid)?
        .len();
    if disk_size == 0 {
        return Err(OciImportError::Invalid);
    }
    let entry_name = format!(
        "{DISK_DIRECTORY}/{}",
        disk_path.rsplit('/').next().unwrap_or_default()
    );
    let layer = NamedTempFile::new().map_err(|_| OciImportError::Invalid)?;
    {
        let output = layer.reopen().map_err(|_| OciImportError::Invalid)?;
        let mut builder = tar::Builder::new(output);
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(disk_size);
        header.set_mode(0o444);
        header.set_mtime(0);
        header.set_cksum();
        let mut input = File::open(disk_file).map_err(|_| OciImportError::Invalid)?;
        builder
            .append_data(&mut header, entry_name, &mut input)
            .map_err(|_| OciImportError::Invalid)?;
        builder
            .into_inner()
            .map_err(|_| OciImportError::Invalid)?
            .sync_all()
            .map_err(|_| OciImportError::Invalid)?;
    }
    let (layer_size, layer_digest) = hash_file(layer.path())?;
    let config = serde_json::to_vec(&json!({
        "architecture": "amd64",
        "os": "linux",
        "rootfs": {"type": "layers", "diff_ids": [layer_digest.clone()]},
    }))
    .map_err(|_| OciImportError::Invalid)?;
    let config_digest = digest_reference(&config);
    let config_file = NamedTempFile::new().map_err(|_| OciImportError::Invalid)?;
    config_file
        .as_file()
        .write_all(&config)
        .and_then(|()| config_file.as_file().sync_all())
        .map_err(|_| OciImportError::Invalid)?;
    let manifest = serde_json::to_vec(&json!({
        "schemaVersion": 2,
        "mediaType": MANIFEST_MEDIA_TYPE,
        "config": {
            "mediaType": CONFIG_MEDIA_TYPE,
            "digest": config_digest,
            "size": config.len(),
        },
        "layers": [{
            "mediaType": LAYER_MEDIA_TYPE,
            "digest": layer_digest,
            "size": layer_size,
        }],
    }))
    .map_err(|_| OciImportError::Invalid)?;
    let manifest_digest = digest_reference(&manifest);
    Ok(OciFileImage {
        manifest_digest,
        manifest_media_type: MANIFEST_MEDIA_TYPE.to_owned(),
        manifest_bytes: manifest,
        blobs: vec![
            OciFileBlob {
                digest: config_digest,
                media_type: CONFIG_MEDIA_TYPE.to_owned(),
                size_bytes: u64::try_from(config.len()).map_err(|_| OciImportError::TooLarge)?,
                path: config_file.into_temp_path(),
            },
            OciFileBlob {
                digest: layer_digest,
                media_type: LAYER_MEDIA_TYPE.to_owned(),
                size_bytes: layer_size,
                path: layer.into_temp_path(),
            },
        ],
    })
}

/// Builds the single-entry uncompressed tar layer.
fn tar_layer(entry_name: &str, disk: &[u8]) -> Result<Vec<u8>, OciImportError> {
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_size(u64::try_from(disk.len()).map_err(|_| OciImportError::TooLarge)?);
    header.set_mode(0o444);
    header.set_mtime(0);
    header.set_cksum();
    builder
        .append_data(&mut header, entry_name, Cursor::new(disk))
        .map_err(|_| OciImportError::Invalid)?;
    builder.into_inner().map_err(|_| OciImportError::Invalid)
}

/// Returns whether `disk_path` is the reviewed in-archive relative disk path.
///
/// Mirrors the accepted shape of `contracts::http::valid_vm_disk_upload`'s `disk_path`, so the
/// wrapper rejects a misdeclared path even when a caller reaches it without the import gate.
fn valid_disk_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= PLATFORM_IMAGE_DISK_PATH_MAX_BYTES
        && !value.starts_with('/')
        && !value.ends_with('/')
        && !value.contains("..")
        && !value.split('/').any(str::is_empty)
}

fn digest_reference(bytes: &[u8]) -> String {
    format!("sha256:{}", Sha256Digest::of_bytes(bytes))
}

fn hash_file(path: &Path) -> Result<(u64, String), OciImportError> {
    let mut file = File::open(path).map_err(|_| OciImportError::Invalid)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    let mut size = 0_u64;
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| OciImportError::Invalid)?;
        if read == 0 {
            break;
        }
        size = size
            .checked_add(u64::try_from(read).map_err(|_| OciImportError::TooLarge)?)
            .ok_or(OciImportError::TooLarge)?;
        hasher.update(&buffer[..read]);
    }
    Ok((size, format!("sha256:{:x}", hasher.finalize())))
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::io::Read;

    use super::{DISK_DIRECTORY, LAYER_MEDIA_TYPE, wrap_containerdisk};
    use crate::oci_import::OciImportError;
    use persistence_sqlx::Sha256Digest;

    fn digest_of(bytes: &[u8]) -> String {
        format!("sha256:{}", Sha256Digest::of_bytes(bytes))
    }

    fn layer_entries(image: &crate::oci_import::OciImage) -> Vec<(String, Vec<u8>)> {
        let layer = image
            .blobs
            .iter()
            .find(|blob| blob.media_type == LAYER_MEDIA_TYPE)
            .expect("layer blob");
        assert_eq!(layer.digest, digest_of(&layer.bytes));
        let mut archive = tar::Archive::new(layer.bytes.as_slice());
        archive
            .entries()
            .expect("entries")
            .map(|entry| {
                let mut entry = entry.expect("entry");
                let name = entry.path().expect("path").to_string_lossy().into_owned();
                let mut bytes = Vec::new();
                entry.read_to_end(&mut bytes).expect("read entry");
                (name, bytes)
            })
            .collect()
    }

    #[test]
    fn wraps_one_disk_entry_and_derives_every_digest_from_the_exact_bytes() {
        let disk = b"qcow2-disk-bytes".to_vec();
        let image = wrap_containerdisk(&disk, "disk/disk.img").expect("wrapped image");

        assert_eq!(
            layer_entries(&image),
            vec![("disk/disk.img".to_owned(), disk)]
        );
        assert_eq!(image.manifest_digest, digest_of(&image.manifest_bytes));
        assert_eq!(image.blobs.len(), 2);
        assert!(
            image
                .blobs
                .iter()
                .any(|blob| blob.digest == image.config_digest)
        );
    }

    #[test]
    fn flattens_the_declared_path_to_the_reviewed_disk_directory() {
        let image = wrap_containerdisk(b"disk", "images/nested/fedora.qcow2").expect("wrapped");
        assert_eq!(
            layer_entries(&image),
            vec![(format!("{DISK_DIRECTORY}/fedora.qcow2"), b"disk".to_vec())]
        );
    }

    #[test]
    fn rejects_misdeclared_disks_and_paths() {
        assert_eq!(
            wrap_containerdisk(b"", "disk/disk.img").err(),
            Some(OciImportError::Invalid)
        );
        for path in ["", "/disk.img", "disk/", "a/../b.img", ".."] {
            assert_eq!(
                wrap_containerdisk(b"disk", path).err(),
                Some(OciImportError::Invalid),
                "path {path} must be rejected"
            );
        }
        let too_long = format!("disk/{}.img", "a".repeat(300));
        assert_eq!(
            wrap_containerdisk(b"disk", &too_long).err(),
            Some(OciImportError::Invalid)
        );
        let longest = format!("{}.img", "a".repeat(252));
        assert!(wrap_containerdisk(b"disk", &longest).is_ok());
    }
}
