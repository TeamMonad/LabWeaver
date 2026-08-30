//! Runtime-neutral bounded snapshot engine.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read as _;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt as _};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions};
use contracts::PathRule;
use contracts::submission::{FrozenFile, SubmissionManifest};
use persistence_sqlx::Sha256Digest; // internal persistence hash, not contract hash
use serde::{Deserialize, Serialize};

const ARCHIVE_MEDIA_TYPE: &str = "application/vnd.labweaver.frozen-submission.v1+json";
const DEFAULT_MAX_SOURCE_BYTES: u64 = 64 * 1024 * 1024;
const DEFAULT_MAX_ARCHIVE_BYTES: u64 = 96 * 1024 * 1024;
const DEFAULT_MAX_FILES: u32 = 10_000;

/// Runtime transport bound to the immutable Environment identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SnapshotTransport {
    /// Read-only mounted Container PVC.
    Pvc,
    /// Certificate-authenticated VM SFTP session.
    Ssh,
}

/// Runtime source entry type returned without following symbolic links.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceKind {
    /// Regular file.
    File,
    /// Directory.
    Directory,
    /// Symbolic link, always rejected.
    Symlink,
    /// Device, socket, FIFO, or an entry with incomplete type metadata.
    Other,
}

/// Source metadata used by both PVC and SFTP collectors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceMetadata {
    /// Entry kind without following symbolic links.
    pub kind: SourceKind,
    /// File size reported by the source.
    pub size_bytes: u64,
}

/// One immediate child returned by a bounded directory read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceEntry {
    /// Single UTF-8 path component.
    pub name: String,
    /// Metadata obtained without following symbolic links.
    pub metadata: SourceMetadata,
}

/// Read-only filesystem boundary implemented by PVC and SSH/SFTP sources.
#[async_trait]
pub trait SnapshotSource: Send + Sync {
    /// Runtime transport used to obtain this snapshot.
    fn transport(&self) -> SnapshotTransport;

    /// Immutable runtime identity bound by the owning Environment service.
    fn identity(&self) -> Sha256Digest;

    /// Rejects a symbolic-link ancestor or source-specific root escape.
    async fn validate_path(&self, path: &str) -> Result<(), CollectError>;

    /// Reads metadata without following the final symbolic link.
    async fn metadata(&self, path: &str) -> Result<Option<SourceMetadata>, CollectError>;

    /// Lists one directory without recursively following entries.
    async fn read_dir(&self, path: &str) -> Result<Vec<SourceEntry>, CollectError>;

    /// Reads one regular file with a hard byte limit.
    async fn read_file(&self, path: &str, max_bytes: u64) -> Result<Vec<u8>, CollectError>;
}

/// Capability-scoped PVC source. The directory is opened once and all access remains relative.
pub struct PvcSnapshotSource {
    root: Arc<Dir>,
    identity: Sha256Digest,
}

impl std::fmt::Debug for PvcSnapshotSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PvcSnapshotSource")
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}

impl PvcSnapshotSource {
    /// Opens an already-mounted read-only PVC root as a filesystem capability.
    ///
    /// # Errors
    ///
    /// Returns `LW_COLLECT_SOURCE_UNAVAILABLE` when the root cannot be opened.
    pub fn open(root: &Path, identity: Sha256Digest) -> Result<Self, CollectError> {
        let root = Dir::open_ambient_dir(root, ambient_authority())
            .map_err(|_| CollectError::SourceUnavailable)?;
        Ok(Self {
            root: Arc::new(root),
            identity,
        })
    }

    fn metadata_sync(&self, path: &str) -> Result<Option<SourceMetadata>, CollectError> {
        match self.root.symlink_metadata(path) {
            Ok(metadata) => Ok(Some(SourceMetadata {
                kind: cap_kind(&metadata),
                size_bytes: metadata.len(),
            })),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(CollectError::SourceUnavailable),
        }
    }
}

#[async_trait]
impl SnapshotSource for PvcSnapshotSource {
    fn transport(&self) -> SnapshotTransport {
        SnapshotTransport::Pvc
    }

    fn identity(&self) -> Sha256Digest {
        self.identity
    }

    async fn validate_path(&self, path: &str) -> Result<(), CollectError> {
        validate_relative_ancestors(path, |prefix| self.metadata_sync(prefix))
    }

    async fn metadata(&self, path: &str) -> Result<Option<SourceMetadata>, CollectError> {
        self.metadata_sync(path)
    }

    async fn read_dir(&self, path: &str) -> Result<Vec<SourceEntry>, CollectError> {
        self.validate_path(path).await?;
        let entries = self
            .root
            .read_dir(path)
            .map_err(|_| CollectError::SourceUnavailable)?;
        let mut result = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|_| CollectError::SourceUnavailable)?;
            let name = entry
                .file_name()
                .to_str()
                .filter(|value| safe_component(value))
                .ok_or(CollectError::UnsafePath)?
                .to_owned();
            let child = join_relative(path, &name)?;
            let metadata = self
                .metadata_sync(&child)?
                .ok_or(CollectError::SourceChanged)?;
            result.push(SourceEntry { name, metadata });
        }
        result.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(result)
    }

    async fn read_file(&self, path: &str, max_bytes: u64) -> Result<Vec<u8>, CollectError> {
        self.validate_path(path).await?;
        let before = self
            .metadata_sync(path)?
            .ok_or(CollectError::RequiredPathMissing)?;
        if before.kind == SourceKind::Symlink {
            return Err(CollectError::SymlinkRejected);
        }
        if before.kind != SourceKind::File {
            return Err(CollectError::UnsupportedEntry);
        }
        if before.size_bytes > max_bytes {
            return Err(CollectError::ByteLimitExceeded);
        }
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        let file = self
            .root
            .open_with(path, &options)
            .map_err(|_| CollectError::SourceUnavailable)?;
        let mut bytes = Vec::new();
        file.take(max_bytes.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|_| CollectError::SourceUnavailable)?;
        if u64::try_from(bytes.len()).map_err(|_| CollectError::ByteLimitExceeded)? > max_bytes {
            return Err(CollectError::ByteLimitExceeded);
        }
        let after = self
            .metadata_sync(path)?
            .ok_or(CollectError::SourceChanged)?;
        self.validate_path(path).await?;
        if before != after || u64::try_from(bytes.len()).ok() != Some(after.size_bytes) {
            return Err(CollectError::SourceChanged);
        }
        Ok(bytes)
    }
}

/// Immutable preflight identity which must match the subsequent freeze read.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreflightReport {
    /// Runtime source identity.
    pub source_identity: Sha256Digest,
    /// Sorted file identity list.
    pub files: Vec<FrozenFile>,
    /// Canonical hash of `files`.
    pub manifest_sha256: Sha256Digest,
    /// Total raw bytes.
    pub total_bytes: u64,
}

/// Deterministic archive ready for immutable object storage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrozenArchive {
    /// Sorted file identity list.
    pub files: Vec<FrozenFile>,
    /// Canonical hash of the file manifest.
    pub manifest_sha256: Sha256Digest,
    /// Canonical archive bytes; never log this field.
    pub bytes: Vec<u8>,
    /// Exact archive digest.
    pub sha256: Sha256Digest,
    /// Stable archive media type.
    pub media_type: &'static str,
}

/// Explicit service-side limits independent from teacher-authored manifest limits.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CollectorLimits {
    /// Maximum raw bytes read across selected files.
    pub max_source_bytes: u64,
    /// Maximum serialized canonical archive bytes.
    pub max_archive_bytes: u64,
    /// Maximum selected regular files.
    pub max_files: u32,
}

impl Default for CollectorLimits {
    fn default() -> Self {
        Self {
            max_source_bytes: DEFAULT_MAX_SOURCE_BYTES,
            max_archive_bytes: DEFAULT_MAX_ARCHIVE_BYTES,
            max_files: DEFAULT_MAX_FILES,
        }
    }
}

/// Bounded collector shared by the PVC and SFTP paths.
#[derive(Clone, Copy, Debug, Default)]
pub struct SnapshotCollector {
    limits: CollectorLimits,
}

impl SnapshotCollector {
    /// Creates a collector with deployment-owned hard limits.
    ///
    /// # Errors
    ///
    /// Returns `LW_COLLECT_LIMIT_CONFIG_INVALID` for zero or inverted limits.
    pub fn new(limits: CollectorLimits) -> Result<Self, CollectError> {
        if limits.max_source_bytes == 0
            || limits.max_archive_bytes == 0
            || limits.max_files == 0
            || limits.max_archive_bytes < limits.max_source_bytes
        {
            return Err(CollectError::LimitConfigurationInvalid);
        }
        Ok(Self { limits })
    }

    /// Reads and hashes the selected files without creating a publishable artifact.
    ///
    /// # Errors
    ///
    /// Returns a stable [`CollectError`] when manifest, source, path, or limits fail.
    pub async fn preflight(
        &self,
        source: &dyn SnapshotSource,
        manifest: &SubmissionManifest,
    ) -> Result<PreflightReport, CollectError> {
        let snapshot = collect_snapshot(source, manifest, self.limits).await?;
        Ok(PreflightReport {
            source_identity: source.identity(),
            files: snapshot.files,
            manifest_sha256: snapshot.manifest_sha256,
            total_bytes: snapshot.total_bytes,
        })
    }

    /// Re-reads the source, requires an exact preflight match, and creates canonical bytes.
    ///
    /// # Errors
    ///
    /// Returns a stable [`CollectError`] when the source changed or output is invalid.
    pub async fn freeze(
        &self,
        source: &dyn SnapshotSource,
        manifest: &SubmissionManifest,
        preflight: &PreflightReport,
    ) -> Result<FrozenArchive, CollectError> {
        if preflight.source_identity != source.identity() {
            return Err(CollectError::SourceIdentityMismatch);
        }
        let snapshot = collect_snapshot(source, manifest, self.limits).await?;
        if snapshot.files != preflight.files
            || snapshot.manifest_sha256 != preflight.manifest_sha256
            || snapshot.total_bytes != preflight.total_bytes
        {
            return Err(CollectError::SourceChanged);
        }
        let archive = CanonicalArchive {
            api_version: "evaluation.labweaver.io/frozen-submission-archive/v1",
            files: snapshot
                .contents
                .into_iter()
                .map(|(path, bytes)| CanonicalArchiveFile {
                    path,
                    content_base64: STANDARD.encode(bytes),
                })
                .collect(),
        };
        let bytes = serde_jcs::to_vec(&archive).map_err(|_| CollectError::ArchiveFailed)?;
        if u64::try_from(bytes.len()).map_err(|_| CollectError::OutputLimitExceeded)?
            > self.limits.max_archive_bytes
        {
            return Err(CollectError::OutputLimitExceeded);
        }
        let sha256 = Sha256Digest::of_bytes(&bytes);
        Ok(FrozenArchive {
            files: snapshot.files,
            manifest_sha256: snapshot.manifest_sha256,
            bytes,
            sha256,
            media_type: ARCHIVE_MEDIA_TYPE,
        })
    }
}

struct Snapshot {
    files: Vec<FrozenFile>,
    contents: BTreeMap<String, Vec<u8>>,
    manifest_sha256: Sha256Digest,
    total_bytes: u64,
}

async fn collect_snapshot(
    source: &dyn SnapshotSource,
    manifest: &SubmissionManifest,
    limits: CollectorLimits,
) -> Result<Snapshot, CollectError> {
    manifest
        .validate()
        .map_err(|_| CollectError::ManifestInvalid)?;
    if manifest.max_total_bytes > limits.max_source_bytes {
        return Err(CollectError::ByteLimitExceeded);
    }
    if manifest.max_files > limits.max_files {
        return Err(CollectError::FileLimitExceeded);
    }
    let mut selected = BTreeSet::new();
    let mut existing_directories = BTreeSet::new();
    for include in &manifest.include {
        match include {
            PathRule::ExactFile { path } => {
                if excluded(manifest, path) {
                    continue;
                }
                source.validate_path(path).await?;
                if let Some(metadata) = source.metadata(path).await? {
                    validate_selected_metadata(metadata)?;
                    if metadata.kind != SourceKind::File {
                        return Err(CollectError::UnsupportedEntry);
                    }
                    selected.insert(path.clone());
                }
            }
            PathRule::DirectoryTree { path } => {
                source.validate_path(path).await?;
                let Some(metadata) = source.metadata(path).await? else {
                    continue;
                };
                validate_selected_metadata(metadata)?;
                if metadata.kind != SourceKind::Directory {
                    return Err(CollectError::UnsupportedEntry);
                }
                existing_directories.insert(path.clone());
                walk_directory(
                    source,
                    path,
                    manifest,
                    &mut selected,
                    &mut existing_directories,
                )
                .await?;
            }
        }
    }
    validate_required(manifest, &selected, &existing_directories)?;
    if selected.is_empty() {
        return Err(CollectError::RequiredPathMissing);
    }
    if selected.len() > usize::try_from(manifest.max_files).unwrap_or(usize::MAX) {
        return Err(CollectError::FileLimitExceeded);
    }

    let mut files = Vec::with_capacity(selected.len());
    let mut contents = BTreeMap::new();
    let mut total_bytes = 0_u64;
    for path in selected {
        source.validate_path(&path).await?;
        let remaining = manifest
            .max_total_bytes
            .checked_sub(total_bytes)
            .ok_or(CollectError::ByteLimitExceeded)?;
        let bytes = source.read_file(&path, remaining).await?;
        let size_bytes = u64::try_from(bytes.len()).map_err(|_| CollectError::ByteLimitExceeded)?;
        total_bytes = total_bytes
            .checked_add(size_bytes)
            .ok_or(CollectError::ByteLimitExceeded)?;
        if total_bytes > manifest.max_total_bytes {
            return Err(CollectError::ByteLimitExceeded);
        }
        files.push(FrozenFile {
            path: path.clone(),
            sha256: Sha256Digest::of_bytes(&bytes),
            size_bytes,
            media_type: "application/octet-stream".to_owned(),
        });
        contents.insert(path, bytes);
    }
    let manifest_sha256 =
        Sha256Digest::of_canonical(&files).map_err(|_| CollectError::ArchiveFailed)?;
    Ok(Snapshot {
        files,
        contents,
        manifest_sha256,
        total_bytes,
    })
}

async fn walk_directory(
    source: &dyn SnapshotSource,
    root: &str,
    manifest: &SubmissionManifest,
    selected: &mut BTreeSet<String>,
    existing_directories: &mut BTreeSet<String>,
) -> Result<(), CollectError> {
    let mut pending = vec![root.to_owned()];
    while let Some(directory) = pending.pop() {
        let entries = source.read_dir(&directory).await?;
        for entry in entries.into_iter().rev() {
            let path = join_relative(&directory, &entry.name)?;
            if entry.metadata.kind == SourceKind::Symlink {
                return Err(CollectError::SymlinkRejected);
            }
            if excluded(manifest, &path) {
                continue;
            }
            match entry.metadata.kind {
                SourceKind::File => {
                    selected.insert(path);
                    if selected.len() > usize::try_from(manifest.max_files).unwrap_or(usize::MAX) {
                        return Err(CollectError::FileLimitExceeded);
                    }
                }
                SourceKind::Directory => {
                    existing_directories.insert(path.clone());
                    pending.push(path);
                }
                SourceKind::Symlink => return Err(CollectError::SymlinkRejected),
                SourceKind::Other => return Err(CollectError::UnsupportedEntry),
            }
        }
    }
    Ok(())
}

fn validate_required(
    manifest: &SubmissionManifest,
    selected: &BTreeSet<String>,
    directories: &BTreeSet<String>,
) -> Result<(), CollectError> {
    for required in &manifest.required {
        let present = match required {
            PathRule::ExactFile { path } => selected.contains(path),
            PathRule::DirectoryTree { path } => directories.contains(path),
        };
        if !present {
            return Err(CollectError::RequiredPathMissing);
        }
    }
    Ok(())
}

fn validate_selected_metadata(metadata: SourceMetadata) -> Result<(), CollectError> {
    match metadata.kind {
        SourceKind::Symlink => Err(CollectError::SymlinkRejected),
        SourceKind::Other => Err(CollectError::UnsupportedEntry),
        SourceKind::File | SourceKind::Directory => Ok(()),
    }
}

fn validate_relative_ancestors(
    path: &str,
    mut metadata: impl FnMut(&str) -> Result<Option<SourceMetadata>, CollectError>,
) -> Result<(), CollectError> {
    contracts::validate_relative_path(path).map_err(|_| CollectError::UnsafePath)?;
    let mut prefix = String::new();
    let components = path.split('/').collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(component);
        let Some(observed) = metadata(&prefix)? else {
            return Ok(());
        };
        if observed.kind == SourceKind::Symlink {
            return Err(CollectError::SymlinkRejected);
        }
        if index + 1 != components.len() && observed.kind != SourceKind::Directory {
            return Err(CollectError::UnsupportedEntry);
        }
    }
    Ok(())
}

fn excluded(manifest: &SubmissionManifest, path: &str) -> bool {
    manifest.exclude.iter().any(|rule| covers(rule, path))
}

fn covers(rule: &PathRule, path: &str) -> bool {
    match rule {
        PathRule::ExactFile { path: exact } => exact == path,
        PathRule::DirectoryTree { path: directory } => {
            directory == path
                || (path.starts_with(directory)
                    && path.as_bytes().get(directory.len()).copied() == Some(b'/'))
        }
    }
}

fn join_relative(parent: &str, name: &str) -> Result<String, CollectError> {
    if !safe_component(name) {
        return Err(CollectError::UnsafePath);
    }
    let path = format!("{parent}/{name}");
    contracts::validate_relative_path(&path).map_err(|_| CollectError::UnsafePath)?;
    Ok(path)
}

fn safe_component(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && !value.contains('/')
        && !value.contains('\\')
        && !value.chars().any(char::is_control)
}

fn cap_kind(metadata: &cap_std::fs::Metadata) -> SourceKind {
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        SourceKind::Symlink
    } else if file_type.is_file() {
        SourceKind::File
    } else if file_type.is_dir() {
        SourceKind::Directory
    } else {
        SourceKind::Other
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CanonicalArchive {
    api_version: &'static str,
    files: Vec<CanonicalArchiveFile>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CanonicalArchiveFile {
    path: String,
    content_base64: String,
}

/// Stable fail-closed Collector failures. Variants never contain paths or student content.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CollectError {
    /// Deployment-owned Collector limits are invalid.
    #[error("LW_COLLECT_LIMIT_CONFIG_INVALID")]
    LimitConfigurationInvalid,
    /// `SubmissionManifest` failed its versioned validation.
    #[error("LW_COLLECT_MANIFEST_INVALID")]
    ManifestInvalid,
    /// A runtime path or directory entry was not a normalized relative path.
    #[error("LW_COLLECT_PATH_UNSAFE")]
    UnsafePath,
    /// A selected or traversed entry was a symbolic link.
    #[error("LW_COLLECT_SYMLINK_REJECTED")]
    SymlinkRejected,
    /// A selected entry was not a regular file or directory.
    #[error("LW_COLLECT_ENTRY_UNSUPPORTED")]
    UnsupportedEntry,
    /// At least one required path was absent.
    #[error("LW_COLLECT_REQUIRED_PATH_MISSING")]
    RequiredPathMissing,
    /// The file-count limit was exceeded.
    #[error("LW_COLLECT_FILE_LIMIT_EXCEEDED")]
    FileLimitExceeded,
    /// The byte limit was exceeded.
    #[error("LW_COLLECT_BYTE_LIMIT_EXCEEDED")]
    ByteLimitExceeded,
    /// Canonical output exceeds the bounded archive limit.
    #[error("LW_COLLECT_OUTPUT_LIMIT_EXCEEDED")]
    OutputLimitExceeded,
    /// PVC or SFTP source access failed.
    #[error("LW_COLLECT_SOURCE_UNAVAILABLE")]
    SourceUnavailable,
    /// Runtime source identity did not match the preflight identity.
    #[error("LW_COLLECT_SOURCE_IDENTITY_MISMATCH")]
    SourceIdentityMismatch,
    /// Source contents changed between observations or during one read.
    #[error("LW_COLLECT_SOURCE_CHANGED")]
    SourceChanged,
    /// Canonical archive encoding failed.
    #[error("LW_COLLECT_ARCHIVE_FAILED")]
    ArchiveFailed,
    /// SSH credential or endpoint identity was invalid or expired.
    #[error("LW_COLLECT_SSH_CREDENTIAL_INVALID")]
    SshCredentialInvalid,
    /// SSH host-key identity did not match the Environment observation.
    #[error("LW_COLLECT_SSH_HOST_KEY_MISMATCH")]
    SshHostKeyMismatch,
    /// SSH or SFTP exceeded its bounded deadline.
    #[error("LW_COLLECT_SSH_TIMEOUT")]
    SshTimeout,
}

impl CollectError {
    /// Returns the stable payload-free diagnostic code.
    #[must_use]
    pub const fn diagnostic_code(self) -> &'static str {
        match self {
            Self::LimitConfigurationInvalid => "LW_COLLECT_LIMIT_CONFIG_INVALID",
            Self::ManifestInvalid => "LW_COLLECT_MANIFEST_INVALID",
            Self::UnsafePath => "LW_COLLECT_PATH_UNSAFE",
            Self::SymlinkRejected => "LW_COLLECT_SYMLINK_REJECTED",
            Self::UnsupportedEntry => "LW_COLLECT_ENTRY_UNSUPPORTED",
            Self::RequiredPathMissing => "LW_COLLECT_REQUIRED_PATH_MISSING",
            Self::FileLimitExceeded => "LW_COLLECT_FILE_LIMIT_EXCEEDED",
            Self::ByteLimitExceeded => "LW_COLLECT_BYTE_LIMIT_EXCEEDED",
            Self::OutputLimitExceeded => "LW_COLLECT_OUTPUT_LIMIT_EXCEEDED",
            Self::SourceUnavailable => "LW_COLLECT_SOURCE_UNAVAILABLE",
            Self::SourceIdentityMismatch => "LW_COLLECT_SOURCE_IDENTITY_MISMATCH",
            Self::SourceChanged => "LW_COLLECT_SOURCE_CHANGED",
            Self::ArchiveFailed => "LW_COLLECT_ARCHIVE_FAILED",
            Self::SshCredentialInvalid => "LW_COLLECT_SSH_CREDENTIAL_INVALID",
            Self::SshHostKeyMismatch => "LW_COLLECT_SSH_HOST_KEY_MISMATCH",
            Self::SshTimeout => "LW_COLLECT_SSH_TIMEOUT",
        }
    }
}
