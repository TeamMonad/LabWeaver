//! Turn one `ConfigMap` projection generation into private, regular startup inputs.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

use contracts::platform_configuration::valid_configuration_sha256;
use persistence_sqlx::Sha256Digest;

use super::{ConfigurationError, LoadedConfiguration};

const FILES: [&str; 2] = [
    "platform-configuration.json",
    "platform-configuration-source.json",
];
const MAX_BYTES: u64 = 1_048_576;

/// Copies the exact two non-secret inputs from one `AtomicWriter` generation.
/// The digest is derived by Helm from the same Bundle input. Existing runtime
/// files are accepted only for an exact retry; a failed copy never replaces them.
pub fn prepare_projected_configuration(
    projected: &Path,
    runtime: &Path,
    expected_sha256: &str,
) -> Result<(), ConfigurationError> {
    if !valid_configuration_sha256(expected_sha256)
        || fs::symlink_metadata(projected)
            .map_err(|_| ConfigurationError::Invalid)?
            .file_type()
            .is_symlink()
        || fs::symlink_metadata(runtime)
            .map_err(|_| ConfigurationError::Invalid)?
            .file_type()
            .is_symlink()
        || !runtime.is_dir()
    {
        return Err(ConfigurationError::Invalid);
    }
    let root = projected
        .canonicalize()
        .map_err(|_| ConfigurationError::Invalid)?;
    let generation = root
        .join("..data")
        .canonicalize()
        .map_err(|_| ConfigurationError::Invalid)?;
    if generation == root || !generation.starts_with(&root) || !generation.is_dir() {
        return Err(ConfigurationError::Invalid);
    }
    let first = read_file(&generation.join(FILES[0]))?;
    let second = read_file(&generation.join(FILES[1]))?;
    let mut joined = first.clone();
    joined.push(b'\n');
    joined.extend_from_slice(&second);
    if Sha256Digest::of_bytes(&joined).to_string() != expected_sha256 {
        return Err(ConfigurationError::ObservationMismatch);
    }
    let candidate = runtime.join(format!(".candidate-{}", uuid::Uuid::now_v7()));
    fs::create_dir(&candidate).map_err(|_| ConfigurationError::Invalid)?;
    let result = (|| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&candidate, fs::Permissions::from_mode(0o700))
                .map_err(|_| ConfigurationError::Invalid)?;
        }
        for (name, bytes) in FILES.into_iter().zip([&first, &second]) {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options
                .open(candidate.join(name))
                .map_err(|_| ConfigurationError::Invalid)?;
            file.write_all(bytes)
                .map_err(|_| ConfigurationError::Invalid)?;
            file.sync_all().map_err(|_| ConfigurationError::Invalid)?;
        }
        LoadedConfiguration::load(&candidate.join(FILES[0]), &candidate.join(FILES[1]))?;
        let current = runtime.join("current");
        if current.exists() || current.is_symlink() {
            LoadedConfiguration::load(&current.join(FILES[0]), &current.join(FILES[1]))?;
            if read_file(&current.join(FILES[0]))? != first
                || read_file(&current.join(FILES[1]))? != second
            {
                return Err(ConfigurationError::ObservationMismatch);
            }
        } else {
            fs::rename(&candidate, &current).map_err(|_| ConfigurationError::Invalid)?;
        }
        Ok(())
    })();
    if candidate.exists() {
        // This invocation created the UUID directory and these two fixed files.
        for name in FILES {
            let _ = fs::remove_file(candidate.join(name));
        }
        let _ = fs::remove_dir(&candidate);
    }
    result
}

fn read_file(path: &Path) -> Result<Vec<u8>, ConfigurationError> {
    let before = fs::symlink_metadata(path).map_err(|_| ConfigurationError::Invalid)?;
    if !before.is_file() || before.len() == 0 || before.len() > MAX_BYTES {
        return Err(ConfigurationError::Invalid);
    }
    let file = fs::File::open(path).map_err(|_| ConfigurationError::Invalid)?;
    if !super::loaded::same_file(
        &before,
        &file.metadata().map_err(|_| ConfigurationError::Invalid)?,
    ) {
        return Err(ConfigurationError::Invalid);
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ConfigurationError::Invalid)?;
    let after = fs::symlink_metadata(path).map_err(|_| ConfigurationError::Invalid)?;
    if !super::loaded::same_file(&before, &after) || bytes.len() as u64 != before.len() {
        return Err(ConfigurationError::Invalid);
    }
    Ok(bytes)
}
