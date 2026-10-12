//! A process-loaded settings snapshot, separate from an administrator's desired record.

use std::fs::{File, symlink_metadata};
use std::io::Read;
use std::path::Path;

use contracts::Revision;
use contracts::platform_configuration::{PlatformConfigurationVersion, valid_configuration_sha256};
use persistence_sqlx::Sha256Digest;
use serde::Deserialize;

use super::ConfigurationError;

const MAX_FILE_BYTES: u64 = 1_048_576;
const SOURCE_VERSION: &str = "deploy.labweaver.io/loaded-source/v1";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Source {
    api_version: String,
    desired_revision: Revision,
    configuration_sha256: String,
    encrypted_bundle_sha256: String,
    installation: Installation,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Installation {
    namespace: String,
    release_name: String,
}

/// Immutable settings actually read by a Control process from its private Bundle input.
/// The source is produced only after age authentication and Bundle validation. It is a
/// deployment input, not a public report field or evidence of other services' adoption.
pub struct LoadedConfiguration {
    version: PlatformConfigurationVersion,
    encrypted_bundle_sha256: String,
    installation: Installation,
}

impl LoadedConfiguration {
    /// Reads both private files once, validates settings and recomputes their canonical digest.
    /// Loading a replacement never mutates a previously loaded process snapshot.
    pub fn load(version_file: &Path, source_file: &Path) -> Result<Self, ConfigurationError> {
        let mut version: PlatformConfigurationVersion =
            contracts::parse_strict_json(&private_file(version_file)?)
                .map_err(|_| ConfigurationError::Invalid)?;
        version.configuration.validate()?;
        let digest = Sha256Digest::of_canonical(&version.configuration)
            .map_err(|_| ConfigurationError::Invalid)?
            .to_string();
        let source: Source = contracts::parse_strict_json(&private_file(source_file)?)
            .map_err(|_| ConfigurationError::Invalid)?;
        if source.api_version != SOURCE_VERSION
            || version.revision.get() > i64::MAX as u64
            || version.revision != source.desired_revision
            || version.configuration_sha256 != digest
            || source.configuration_sha256 != digest
            || !valid_configuration_sha256(&source.encrypted_bundle_sha256)
            || !dns_label(&source.installation.namespace)
            || !dns_label(&source.installation.release_name)
        {
            return Err(ConfigurationError::ObservationMismatch);
        }
        // Keep the independently computed value rather than a copied report declaration.
        version.configuration_sha256 = digest;
        Ok(Self {
            version,
            encrypted_bundle_sha256: source.encrypted_bundle_sha256,
            installation: source.installation,
        })
    }

    pub(super) fn matches(
        &self,
        desired: &PlatformConfigurationVersion,
        encrypted_bundle_sha256: &str,
    ) -> bool {
        self.version.revision == desired.revision
            && self.version.configuration == desired.configuration
            && self.version.configuration_sha256 == desired.configuration_sha256
            && self.encrypted_bundle_sha256 == encrypted_bundle_sha256
    }
}

impl std::fmt::Debug for LoadedConfiguration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LoadedConfiguration")
            .field("revision", &self.version.revision)
            .field("configuration_sha256", &self.version.configuration_sha256)
            .field("encrypted_bundle_sha256", &self.encrypted_bundle_sha256)
            .field("namespace", &self.installation.namespace)
            .field("release_name", &self.installation.release_name)
            .finish_non_exhaustive()
    }
}

fn dns_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value.starts_with(|v: char| v.is_ascii_alphanumeric())
        && value.ends_with(|v: char| v.is_ascii_alphanumeric())
        && value
            .bytes()
            .all(|v| v.is_ascii_lowercase() || v.is_ascii_digit() || v == b'-')
}

fn private_file(path: &Path) -> Result<Vec<u8>, ConfigurationError> {
    if path.ancestors().any(|entry| {
        symlink_metadata(entry).is_ok_and(|metadata| metadata.file_type().is_symlink())
    }) {
        return Err(ConfigurationError::Invalid);
    }
    let metadata = symlink_metadata(path).map_err(|_| ConfigurationError::Invalid)?;
    if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
        return Err(ConfigurationError::Invalid);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o777 != 0o600 {
            return Err(ConfigurationError::Invalid);
        }
    }
    let file = File::open(path).map_err(|_| ConfigurationError::Invalid)?;
    let opened = file.metadata().map_err(|_| ConfigurationError::Invalid)?;
    if !same_file(&metadata, &opened) {
        return Err(ConfigurationError::Invalid);
    }
    let mut payload = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut payload)
        .map_err(|_| ConfigurationError::Invalid)?;
    if payload.is_empty() || payload.len() as u64 > MAX_FILE_BYTES {
        return Err(ConfigurationError::Invalid);
    }
    if !same_file(
        &opened,
        &symlink_metadata(path).map_err(|_| ConfigurationError::Invalid)?,
    ) {
        return Err(ConfigurationError::Invalid);
    }
    Ok(payload)
}

fn same_file(first: &std::fs::Metadata, second: &std::fs::Metadata) -> bool {
    if !second.is_file()
        || first.len() != second.len()
        || first.permissions() != second.permissions()
    {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        first.ino() == second.ino() && first.dev() == second.dev()
    }
    #[cfg(not(unix))]
    {
        true
    }
}
