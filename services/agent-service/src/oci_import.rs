//! Verified import of a sandbox-exported OCI image layout.
//!
//! A sandbox build exports an OCI layout archive (optionally gzip-compressed) instead of pushing
//! to Harbor directly. The import authority stays in the build executor: this module unpacks the
//! archive with strict path and size bounds, verifies every declared digest against the actual
//! blob bytes, and returns the exact manifest identity that the registry publication step may
//! push by digest. No untrusted path is ever written to disk.

use std::collections::BTreeMap;
use std::io::Read;

use flate2::read::GzDecoder;
use persistence_sqlx::Sha256Digest;
use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

const OCI_LAYOUT_VERSION: &str = "1.0.0";
const MAX_ARCHIVE_ENTRIES: usize = 4_096;
const MANIFEST_MEDIA_TYPES: [&str; 2] = [
    "application/vnd.oci.image.manifest.v1+json",
    "application/vnd.docker.distribution.manifest.v2+json",
];
const CONFIG_MEDIA_TYPES: [&str; 2] = [
    "application/vnd.oci.image.config.v1+json",
    "application/vnd.docker.container.image.v1+json",
];
const LAYER_MEDIA_TYPES: [&str; 6] = [
    "application/vnd.oci.image.layer.v1.tar",
    "application/vnd.oci.image.layer.v1.tar+gzip",
    "application/vnd.oci.image.layer.v1.tar+zstd",
    "application/vnd.docker.image.rootfs.diff.tar",
    "application/vnd.docker.image.rootfs.diff.tar.gzip",
    "application/vnd.docker.image.rootfs.diff.tar.zstd",
];

/// Deployment-independent import bounds; tests may lower them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OciLimits {
    /// Maximum compressed archive size.
    pub max_archive_bytes: u64,
    /// Maximum total uncompressed entry bytes.
    pub max_total_bytes: u64,
    /// Maximum number of distinct blobs.
    pub max_blobs: usize,
}

impl Default for OciLimits {
    fn default() -> Self {
        Self {
            max_archive_bytes: 2 * 1024 * 1024 * 1024,
            max_total_bytes: 8 * 1024 * 1024 * 1024,
            max_blobs: 512,
        }
    }
}

/// One verified content-addressed blob of the imported image.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OciBlob {
    /// `sha256:` digest identity.
    pub digest: String,
    /// Media type declared by the manifest descriptor.
    pub media_type: String,
    /// Exact bytes that hash to the digest.
    pub bytes: Vec<u8>,
}

/// Verified OCI image ready for digest-preserving registry publication.
#[derive(Clone, Debug)]
pub struct OciImage {
    /// Manifest digest recomputed from the exact manifest bytes.
    pub manifest_digest: String,
    /// Manifest media type.
    pub manifest_media_type: String,
    /// Exact manifest bytes pushed under the digest reference.
    pub manifest_bytes: Vec<u8>,
    /// Config blob digest referenced by the manifest.
    pub config_digest: String,
    /// Config followed by layers, deduplicated by digest.
    pub blobs: Vec<OciBlob>,
}

/// Stable fail-closed import errors.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum OciImportError {
    /// The archive is not a well-formed single-image OCI layout.
    #[error("LW_AGENT_OCI_LAYOUT_INVALID")]
    Invalid,
    /// A descriptor uses a media type the importer does not accept.
    #[error("LW_AGENT_OCI_MEDIA_TYPE_UNSUPPORTED")]
    UnsupportedMediaType,
    /// Declared size or digest differs from the actual blob bytes.
    #[error("LW_AGENT_OCI_BLOB_MISMATCH")]
    BlobMismatch,
    /// A configured archive, entry or blob limit was exceeded.
    #[error("LW_AGENT_OCI_LAYOUT_TOO_LARGE")]
    TooLarge,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LayoutFile {
    image_layout_version: String,
}

#[derive(Deserialize)]
struct IndexFile {
    manifests: Vec<Descriptor>,
}

#[derive(Deserialize)]
struct ManifestFile {
    config: Descriptor,
    layers: Vec<Descriptor>,
}

#[derive(Clone, Deserialize)]
struct Descriptor {
    #[serde(rename = "mediaType")]
    media_type: String,
    digest: String,
    size: u64,
}

/// Parses and fully verifies one OCI layout archive.
///
/// # Errors
///
/// Returns a stable [`OciImportError`] for malformed layouts, unsupported media types, size
/// violations or any digest that does not match the exact blob bytes.
pub fn parse_oci_layout(bytes: &[u8]) -> Result<OciImage, OciImportError> {
    parse_oci_layout_with_limits(bytes, OciLimits::default())
}

/// Parses one OCI layout archive with explicit bounds.
///
/// # Errors
///
/// Same as [`parse_oci_layout`].
#[allow(
    clippy::too_many_lines,
    reason = "one audit boundary verifies the archive, descriptor graph and blob digests in order"
)]
pub fn parse_oci_layout_with_limits(
    bytes: &[u8],
    limits: OciLimits,
) -> Result<OciImage, OciImportError> {
    if bytes.is_empty()
        || u64::try_from(bytes.len()).map_or(true, |size| size > limits.max_archive_bytes)
    {
        return Err(OciImportError::TooLarge);
    }
    let archive_bytes = decompress(bytes, limits)?;
    let mut layout: Option<Value> = None;
    let mut index: Option<Value> = None;
    let mut blobs: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut total_bytes = 0_u64;
    let mut archive = tar::Archive::new(archive_bytes.as_slice());
    let entries = archive.entries().map_err(|_| OciImportError::Invalid)?;
    for (position, entry) in entries.enumerate() {
        if position >= MAX_ARCHIVE_ENTRIES {
            return Err(OciImportError::TooLarge);
        }
        let mut entry = entry.map_err(|_| OciImportError::Invalid)?;
        total_bytes = total_bytes
            .checked_add(entry.size())
            .filter(|total| *total <= limits.max_total_bytes)
            .ok_or(OciImportError::TooLarge)?;
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            continue;
        }
        if !kind.is_file() {
            return Err(OciImportError::Invalid);
        }
        let path = entry.path().map_err(|_| OciImportError::Invalid)?;
        if !safe_entry_path(&path) {
            return Err(OciImportError::Invalid);
        }
        let name = path
            .to_str()
            .ok_or(OciImportError::Invalid)?
            .trim_start_matches("./");
        match classify_entry(name) {
            EntryKind::Layout => layout = Some(read_json(&mut entry)?),
            EntryKind::Index => index = Some(read_json(&mut entry)?),
            EntryKind::Blob(digest) => {
                if blobs.len() >= limits.max_blobs {
                    return Err(OciImportError::TooLarge);
                }
                let declared_size = entry.size();
                let mut blob = Vec::with_capacity(
                    usize::try_from(declared_size).map_err(|_| OciImportError::TooLarge)?,
                );
                entry
                    .take(declared_size.saturating_add(1))
                    .read_to_end(&mut blob)
                    .map_err(|_| OciImportError::Invalid)?;
                if u64::try_from(blob.len()).ok() != Some(declared_size) {
                    return Err(OciImportError::Invalid);
                }
                if digest_reference(&blob) != digest {
                    return Err(OciImportError::BlobMismatch);
                }
                if blobs.insert(digest, blob).is_some() {
                    return Err(OciImportError::Invalid);
                }
            }
            EntryKind::Unknown => return Err(OciImportError::Invalid),
        }
    }
    let layout: LayoutFile = serde_json::from_value(layout.ok_or(OciImportError::Invalid)?)
        .map_err(|_| OciImportError::Invalid)?;
    if layout.image_layout_version != OCI_LAYOUT_VERSION {
        return Err(OciImportError::Invalid);
    }
    let index: IndexFile = serde_json::from_value(index.ok_or(OciImportError::Invalid)?)
        .map_err(|_| OciImportError::Invalid)?;
    let manifest_descriptor = index
        .manifests
        .into_iter()
        .next()
        .ok_or(OciImportError::Invalid)?;
    if !MANIFEST_MEDIA_TYPES.contains(&manifest_descriptor.media_type.as_str()) {
        return Err(OciImportError::UnsupportedMediaType);
    }
    validate_descriptor(&manifest_descriptor)?;
    let manifest_bytes = blobs
        .get(&manifest_descriptor.digest)
        .ok_or(OciImportError::Invalid)?
        .clone();
    if u64::try_from(manifest_bytes.len()).ok() != Some(manifest_descriptor.size) {
        return Err(OciImportError::BlobMismatch);
    }
    let manifest: ManifestFile =
        serde_json::from_slice(&manifest_bytes).map_err(|_| OciImportError::Invalid)?;
    if !CONFIG_MEDIA_TYPES.contains(&manifest.config.media_type.as_str()) {
        return Err(OciImportError::UnsupportedMediaType);
    }
    validate_descriptor(&manifest.config)?;
    if manifest.layers.is_empty() {
        return Err(OciImportError::Invalid);
    }
    let mut ordered = vec![manifest.config.clone()];
    for layer in &manifest.layers {
        if !LAYER_MEDIA_TYPES.contains(&layer.media_type.as_str()) {
            return Err(OciImportError::UnsupportedMediaType);
        }
        validate_descriptor(layer)?;
        ordered.push(layer.clone());
    }
    let mut verified = Vec::new();
    for descriptor in ordered {
        let bytes = blobs
            .get(&descriptor.digest)
            .ok_or(OciImportError::Invalid)?;
        if u64::try_from(bytes.len()).ok() != Some(descriptor.size) {
            return Err(OciImportError::BlobMismatch);
        }
        if !verified
            .iter()
            .any(|blob: &OciBlob| blob.digest == descriptor.digest)
        {
            verified.push(OciBlob {
                digest: descriptor.digest,
                media_type: descriptor.media_type,
                bytes: bytes.clone(),
            });
        }
    }
    Ok(OciImage {
        manifest_digest: manifest_descriptor.digest,
        manifest_media_type: manifest_descriptor.media_type,
        manifest_bytes,
        config_digest: manifest.config.digest,
        blobs: verified,
    })
}

enum EntryKind {
    Layout,
    Index,
    Blob(String),
    Unknown,
}

fn classify_entry(name: &str) -> EntryKind {
    match name {
        "oci-layout" => EntryKind::Layout,
        "index.json" => EntryKind::Index,
        _ => {
            let Some(hex) = name.strip_prefix("blobs/sha256/") else {
                return EntryKind::Unknown;
            };
            if hex.len() == 64
                && hex
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                EntryKind::Blob(format!("sha256:{hex}"))
            } else {
                EntryKind::Unknown
            }
        }
    }
}

fn read_json<R: Read>(reader: &mut R) -> Result<Value, OciImportError> {
    serde_json::from_reader(reader).map_err(|_| OciImportError::Invalid)
}

fn validate_descriptor(descriptor: &Descriptor) -> Result<(), OciImportError> {
    if descriptor.size == 0 || !valid_digest(&descriptor.digest) || descriptor.media_type.is_empty()
    {
        return Err(OciImportError::Invalid);
    }
    Ok(())
}

fn safe_entry_path(path: &std::path::Path) -> bool {
    !path.is_absolute()
        && !path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::Prefix(_)
            )
        })
}

fn valid_digest(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn digest_reference(bytes: &[u8]) -> String {
    format!("sha256:{}", Sha256Digest::of_bytes(bytes))
}

fn decompress(bytes: &[u8], limits: OciLimits) -> Result<Vec<u8>, OciImportError> {
    if !bytes.starts_with(&[0x1f, 0x8b]) {
        return Ok(bytes.to_vec());
    }
    let mut decoder = GzDecoder::new(bytes);
    let mut buffer = Vec::new();
    decoder
        .by_ref()
        .take(limits.max_total_bytes.saturating_add(1))
        .read_to_end(&mut buffer)
        .map_err(|_| OciImportError::Invalid)?;
    if u64::try_from(buffer.len()).map_or(true, |size| size > limits.max_total_bytes) {
        return Err(OciImportError::TooLarge);
    }
    Ok(buffer)
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::io::Write;

    use flate2::{Compression, write::GzEncoder};

    use super::{
        OciImportError, OciLimits, digest_reference, parse_oci_layout, parse_oci_layout_with_limits,
    };

    struct LayoutBuilder {
        builder: tar::Builder<Vec<u8>>,
    }

    impl LayoutBuilder {
        fn new() -> Self {
            Self {
                builder: tar::Builder::new(Vec::new()),
            }
        }

        fn entry(&mut self, name: &str, bytes: &[u8], kind: tar::EntryType) -> &mut Self {
            let mut header = tar::Header::new_gnu();
            header.set_size(u64::try_from(bytes.len()).expect("entry size"));
            header.set_entry_type(kind);
            header.set_mode(0o444);
            header.set_cksum();
            if kind == tar::EntryType::Symlink {
                header.set_link_name("/etc/passwd").expect("link name");
            }
            self.builder
                .append_data(&mut header, name, bytes)
                .expect("append entry");
            self
        }

        fn finish(self) -> Vec<u8> {
            self.builder.into_inner().expect("finish archive")
        }
    }

    struct ImageFixture {
        archive: Vec<u8>,
        layer_digest: String,
    }

    fn fixture(tamper_layer: bool, manifest_media_type: &str) -> ImageFixture {
        let config = br#"{"architecture":"amd64","os":"linux"}"#.to_vec();
        let config_digest = digest_reference(&config);
        let layer = b"layer-bytes".to_vec();
        let effective_layer = if tamper_layer {
            b"tampered".to_vec()
        } else {
            layer.clone()
        };
        let layer_digest = digest_reference(&layer);
        let manifest = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": manifest_media_type,
            "config": {
                "mediaType": "application/vnd.oci.image.config.v1+json",
                "digest": config_digest,
                "size": config.len(),
            },
            "layers": [{
                "mediaType": "application/vnd.oci.image.layer.v1.tar",
                "digest": layer_digest,
                "size": layer.len(),
            }],
        });
        let manifest_bytes = serde_json::to_vec(&manifest).expect("manifest json");
        let manifest_digest = digest_reference(&manifest_bytes);
        let index = serde_json::json!({
            "schemaVersion": 2,
            "manifests": [{
                "mediaType": manifest_media_type,
                "digest": manifest_digest,
                "size": manifest_bytes.len(),
            }],
        });
        let index_bytes = serde_json::to_vec(&index).expect("index json");
        let config_hex = &config_digest[7..];
        let layer_hex = &layer_digest[7..];
        let manifest_hex = &manifest_digest[7..];
        let mut builder = LayoutBuilder::new();
        builder
            .entry(
                "oci-layout",
                br#"{"imageLayoutVersion":"1.0.0"}"#,
                tar::EntryType::Regular,
            )
            .entry("index.json", &index_bytes, tar::EntryType::Regular)
            .entry(
                &format!("blobs/sha256/{config_hex}"),
                &config,
                tar::EntryType::Regular,
            )
            .entry(
                &format!("blobs/sha256/{layer_hex}"),
                &effective_layer,
                tar::EntryType::Regular,
            )
            .entry(
                &format!("blobs/sha256/{manifest_hex}"),
                &manifest_bytes,
                tar::EntryType::Regular,
            );
        ImageFixture {
            archive: builder.finish(),
            layer_digest,
        }
    }

    #[test]
    fn valid_layout_binds_the_manifest_and_verified_blobs() -> Result<(), Box<dyn std::error::Error>>
    {
        let fixture = fixture(false, "application/vnd.oci.image.manifest.v1+json");
        let image = parse_oci_layout(&fixture.archive)?;
        assert!(image.manifest_digest.starts_with("sha256:"));
        assert_eq!(image.blobs.len(), 2);
        assert_eq!(image.blobs[1].digest, fixture.layer_digest);
        assert_eq!(image.blobs[1].bytes, b"layer-bytes");
        assert_eq!(
            digest_reference(&image.manifest_bytes),
            image.manifest_digest
        );
        Ok(())
    }

    #[test]
    fn tampered_layer_digest_is_rejected() {
        let fixture = fixture(true, "application/vnd.oci.image.manifest.v1+json");
        assert_eq!(
            parse_oci_layout(&fixture.archive).err(),
            Some(OciImportError::BlobMismatch)
        );
    }

    #[test]
    fn unsupported_manifest_media_type_is_rejected() {
        let fixture = fixture(false, "application/vnd.example.unknown+json");
        assert_eq!(
            parse_oci_layout(&fixture.archive).err(),
            Some(OciImportError::UnsupportedMediaType)
        );
    }

    #[test]
    fn missing_blob_is_rejected() {
        let fixture = fixture(false, "application/vnd.oci.image.manifest.v1+json");
        let mut builder = LayoutBuilder::new();
        let mut archive = tar::Archive::new(fixture.archive.as_slice());
        for entry in archive.entries().expect("entries") {
            let mut entry = entry.expect("entry");
            let path = entry.path().expect("path").to_string_lossy().to_string();
            if path.contains("blobs/sha256/") && !path.ends_with(&fixture.layer_digest[7..]) {
                let mut bytes = Vec::new();
                std::io::Read::read_to_end(&mut entry, &mut bytes).expect("read");
                builder.entry(&path, &bytes, tar::EntryType::Regular);
            }
        }
        assert_eq!(
            parse_oci_layout(&builder.finish()).err(),
            Some(OciImportError::Invalid)
        );
    }

    #[test]
    fn unsafe_entry_paths_and_links_are_rejected() {
        assert!(super::safe_entry_path(std::path::Path::new(
            "blobs/sha256/abc"
        )));
        assert!(!super::safe_entry_path(std::path::Path::new("../escape")));
        assert!(!super::safe_entry_path(std::path::Path::new("/absolute")));
        assert!(!super::safe_entry_path(std::path::Path::new(
            "blobs/../../escape"
        )));

        let mut builder = LayoutBuilder::new();
        builder.entry("blobs/sha256/link", b"", tar::EntryType::Symlink);
        assert_eq!(
            parse_oci_layout(&builder.finish()).err(),
            Some(OciImportError::Invalid)
        );
    }

    #[test]
    fn gzip_and_limits_are_enforced() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = fixture(false, "application/vnd.oci.image.manifest.v1+json");
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&fixture.archive)?;
        let gzipped = encoder.finish()?;
        assert!(parse_oci_layout(&gzipped).is_ok());

        let tight = OciLimits {
            max_archive_bytes: u64::try_from(fixture.archive.len()).unwrap_or(u64::MAX),
            max_total_bytes: 8,
            max_blobs: 1,
        };
        assert_eq!(
            parse_oci_layout_with_limits(&fixture.archive, tight).err(),
            Some(OciImportError::TooLarge)
        );
        Ok(())
    }
}
