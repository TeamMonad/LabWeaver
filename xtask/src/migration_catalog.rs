use std::fs;
use std::path::{Component, Path};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::AppError;

const DOMAINS: [&str; 6] = [
    "control",
    "access",
    "environment",
    "agent",
    "evaluation",
    "resource",
];

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Catalog {
    #[serde(rename = "catalogVersion")]
    version: u8,
    tool_version: String,
    bootstrap: Migration,
    domains: Vec<Domain>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Domain {
    name: String,
    migrations: Vec<Migration>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Migration {
    id: u8,
    file: String,
    sha256: String,
}

pub(super) fn validate(root: &Path) -> Result<(), AppError> {
    let bytes = fs::read(root.join("migrations/catalog.yaml"))
        .map_err(|error| invalid(&format!("catalog unreadable: {error}")))?;
    let catalog: Catalog = serde_yaml::from_slice(&bytes)
        .map_err(|error| invalid(&format!("catalog schema invalid: {error}")))?;
    if catalog.version != 1
        || catalog.tool_version != env!("CARGO_PKG_VERSION")
        || catalog.domains.len() != DOMAINS.len()
    {
        return Err(invalid("catalog identity or domain count is invalid"));
    }
    validate_migration(
        root,
        &catalog.bootstrap,
        1,
        "bootstrap/0001_roles_and_schemas.sql",
    )?;
    if catalog.bootstrap.file != "bootstrap/0001_roles_and_schemas.sql" {
        return Err(invalid("bootstrap migration path is invalid"));
    }
    for (domain, expected) in catalog.domains.iter().zip(DOMAINS) {
        if domain.name != expected || domain.migrations.is_empty() {
            return Err(invalid("domain order or migration count is invalid"));
        }
        for (index, migration) in domain.migrations.iter().enumerate() {
            let expected_id = u8::try_from(index + 1)
                .map_err(|_| invalid("migration sequence exceeds the supported ID range"))?;
            validate_migration(
                root,
                migration,
                expected_id,
                &format!("{expected}/{expected_id:04}_"),
            )?;
            if expected_id == 1 && migration.file != format!("{expected}/0001_platform_baseline.sql")
            {
                return Err(invalid(
                    "the first domain migration must be the Sprint 2 baseline",
                ));
            }
        }
    }
    Ok(())
}

fn validate_migration(
    root: &Path,
    migration: &Migration,
    expected_id: u8,
    expected_path_or_prefix: &str,
) -> Result<(), AppError> {
    let path = Path::new(&migration.file);
    if migration.id != expected_id
        || !migration.file.starts_with(expected_path_or_prefix)
        || std::path::Path::new(&migration.file).extension() != Some(std::ffi::OsStr::new("sql"))
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(invalid("migration path or ID is invalid"));
    }
    let bytes = fs::read(root.join("migrations").join(path))
        .map_err(|error| invalid(&format!("migration unreadable: {error}")))?;
    let actual = format!("{:x}", Sha256::digest(bytes));
    if migration.sha256 != actual {
        return Err(invalid("migration checksum mismatch"));
    }
    Ok(())
}

fn invalid(detail: &str) -> AppError {
    AppError::ReleaseGate {
        code: "LW_MIGRATION_CATALOG_INVALID",
        detail: detail.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::*;

    #[test]
    fn checked_in_catalog_is_exact_and_hash_bound() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        assert!(validate(&root).is_ok());
    }

    #[test]
    fn modified_baseline_is_rejected() -> io::Result<()> {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let temporary = tempfile::tempdir()?;
        let migrations = temporary.path().join("migrations");
        fs::create_dir_all(&migrations)?;
        copy_tree(&source.join("migrations"), &migrations)?;
        fs::write(
            migrations.join("control/0001_platform_baseline.sql"),
            b"SELECT 1;\n",
        )?;
        assert!(validate(temporary.path()).is_err());
        Ok(())
    }

    #[test]
    fn non_sequential_follow_up_migration_is_rejected() -> io::Result<()> {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let temporary = tempfile::tempdir()?;
        let migrations = temporary.path().join("migrations");
        fs::create_dir_all(&migrations)?;
        copy_tree(&source.join("migrations"), &migrations)?;
        let catalog_path = migrations.join("catalog.yaml");
        let catalog = fs::read_to_string(&catalog_path)?.replace(
            "id: 2\n        file: agent/0002_",
            "id: 3\n        file: agent/0003_",
        );
        fs::write(catalog_path, catalog)?;
        assert!(validate(temporary.path()).is_err());
        Ok(())
    }

    fn copy_tree(source: &Path, target: &Path) -> io::Result<()> {
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            let destination = target.join(entry.file_name());
            if entry.file_type()?.is_dir() {
                fs::create_dir_all(&destination)?;
                copy_tree(&entry.path(), &destination)?;
            } else {
                fs::copy(entry.path(), destination)?;
            }
        }
        Ok(())
    }
}
