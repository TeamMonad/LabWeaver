//! A process-loaded settings snapshot, separate from an administrator's desired record.

use std::fs::{File, symlink_metadata};
use std::io::Read;
use std::path::Path;

use contracts::platform_configuration::{
    ControlConfigurationDeploymentObservation, ControlConfigurationReplicaObservation,
    PlatformConfigurationVersion, valid_configuration_sha256,
};
use contracts::{Revision, UtcTimestamp};
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
    #[serde(default = "one_replica")]
    expected_control_replicas: u32,
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
    expected_control_replicas: u32,
    replica: Option<ReplicaIdentity>,
    instance_id: String,
}

struct ReplicaIdentity {
    name: String,
    namespace: String,
    uid: String,
}

impl LoadedConfiguration {
    /// Lifecycle defaults from this authenticated process-loaded Bundle, not unapplied edits.
    #[must_use]
    pub fn expiry_reminder_seconds(&self) -> &[u64] {
        &self.version.configuration.lifecycle.expiry_reminder_seconds
    }
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
            || !(1..=16).contains(&source.expected_control_replicas)
        {
            return Err(ConfigurationError::ObservationMismatch);
        }
        // Keep the independently computed value rather than a copied report declaration.
        version.configuration_sha256 = digest;
        Ok(Self {
            version,
            encrypted_bundle_sha256: source.encrypted_bundle_sha256,
            installation: source.installation,
            expected_control_replicas: source.expected_control_replicas,
            replica: None,
            instance_id: uuid::Uuid::now_v7().to_string(),
        })
    }

    /// Binds the actual Downward API identity once, before this service instance is published.
    pub fn on_replica(
        mut self,
        name: &str,
        namespace: &str,
        uid: &str,
    ) -> Result<Self, ConfigurationError> {
        if !dns_label(name)
            || namespace != self.installation.namespace
            || !valid_uid(uid)
            || self.replica.is_some()
        {
            return Err(ConfigurationError::Invalid);
        }
        self.replica = Some(ReplicaIdentity {
            name: name.to_owned(),
            namespace: namespace.to_owned(),
            uid: uid.to_owned(),
        });
        Ok(self)
    }

    /// Returns only the actual process snapshot and identity. It does not reread files.
    pub fn observe(
        &self,
        now: UtcTimestamp,
    ) -> Result<ControlConfigurationReplicaObservation, ConfigurationError> {
        let replica = self
            .replica
            .as_ref()
            .ok_or(ConfigurationError::ObservationMismatch)?;
        Ok(ControlConfigurationReplicaObservation {
            pod_name: replica.name.clone(),
            pod_uid: replica.uid.clone(),
            namespace: replica.namespace.clone(),
            instance_id: self.instance_id.clone(),
            desired_revision: self.version.revision,
            configuration_sha256: self.version.configuration_sha256.clone(),
            encrypted_bundle_sha256: self.encrypted_bundle_sha256.clone(),
            observed_at: now,
        })
    }

    /// Whether a startup without the Downward API identity is incomplete for this source.
    #[must_use]
    pub const fn requires_replica_identity(&self) -> bool {
        self.expected_control_replicas > 1
    }

    pub(super) fn replicas_match(
        &self,
        observed: Option<&ControlConfigurationDeploymentObservation>,
        requested_at: UtcTimestamp,
        now: UtcTimestamp,
    ) -> bool {
        let Some(observed) = observed else {
            return self.replica.is_none() && self.expected_control_replicas == 1;
        };
        let Some(current) = self.replica.as_ref() else {
            return false;
        };
        if observed.expected_replicas != self.expected_control_replicas
            || observed.replicas.len() != self.expected_control_replicas as usize
            || observed.namespace != self.installation.namespace
            || observed.release_name != self.installation.release_name
            || !valid_uid(&observed.deployment_uid)
            || observed.deployment_generation == 0
        {
            return false;
        }
        let mut uids = std::collections::BTreeSet::new();
        let mut names = std::collections::BTreeSet::new();
        let mut instances = std::collections::BTreeSet::new();
        let mut contains_current = false;
        for replica in &observed.replicas {
            if !valid_uid(&replica.pod_uid)
                || !valid_uid(&replica.instance_id)
                || !dns_label(&replica.pod_name)
                || !uids.insert(&replica.pod_uid)
                || !names.insert(&replica.pod_name)
                || !instances.insert(&replica.instance_id)
                || replica.namespace != self.installation.namespace
                || replica.desired_revision != self.version.revision
                || replica.configuration_sha256 != self.version.configuration_sha256
                || replica.encrypted_bundle_sha256 != self.encrypted_bundle_sha256
                || replica.observed_at < requested_at
                || replica.observed_at > now
                || (now.get() - replica.observed_at.get()) > time::Duration::seconds(30)
            {
                return false;
            }
            if replica.pod_uid == current.uid {
                if replica.pod_name != current.name || replica.instance_id != self.instance_id {
                    return false;
                }
                contains_current = true;
            }
        }
        contains_current
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
            .field("expected_control_replicas", &self.expected_control_replicas)
            .finish_non_exhaustive()
    }
}

const fn one_replica() -> u32 {
    1
}

fn valid_uid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|uid| !uid.is_nil() && uid.to_string() == value)
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

pub(super) fn same_file(first: &std::fs::Metadata, second: &std::fs::Metadata) -> bool {
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
