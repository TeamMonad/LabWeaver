use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::Sha256Digest;

use crate::{Domain, PersistenceError};

/// One immutable SQL file declared by the Migration catalog.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogMigration {
    /// Monotonically increasing domain-local Migration identifier.
    pub id: u64,
    /// Catalog-relative, normalized SQL filename.
    pub file: String,
    /// Optional file content identity; when None only existence is verified (thinned catalog).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<Sha256Digest>,
}

/// Ordered Migration list for one authoritative domain.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogDomain {
    /// Closed domain name.
    pub name: Domain,
    /// Ordered immutable SQL files.
    pub migrations: Vec<CatalogMigration>,
}

/// Version-controlled Migration catalog. Runtime release identity is deliberately separate.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MigrationCatalog {
    /// Catalog wire format version.
    pub catalog_version: u32,
    /// Exact Migration tool contract version.
    pub tool_version: String,
    /// Bootstrap SQL identity.
    pub bootstrap: CatalogMigration,
    /// Fixed complete domain order.
    pub domains: Vec<CatalogDomain>,
}

impl MigrationCatalog {
    /// Loads a catalog and verifies declared hashes and complete SQL file registration.
    pub fn load(path: &Path) -> Result<Self, PersistenceError> {
        let bytes = fs::read(path).map_err(|error| {
            PersistenceError::Catalog(format!("cannot read {}: {error}", path.display()))
        })?;
        let catalog: Self = serde_yaml::from_slice(&bytes)
            .map_err(|error| PersistenceError::Catalog(error.to_string()))?;
        let root = path.parent().ok_or_else(|| {
            PersistenceError::Catalog("catalog path has no parent directory".to_owned())
        })?;
        catalog.verify_files(root)?;
        Ok(catalog)
    }

    /// Returns the RFC 8785 canonical catalog identity.
    pub fn sha256(&self) -> Result<Sha256Digest, PersistenceError> {
        Sha256Digest::of_canonical(self)
            .map_err(|error| PersistenceError::Catalog(error.to_string()))
    }

    /// Verifies the complete catalog shape and every checked-in SQL file.
    pub fn verify_files(&self, root: &Path) -> Result<(), PersistenceError> {
        self.validate(root)?;
        let registered = std::iter::once(&self.bootstrap)
            .chain(self.domains.iter().flat_map(|domain| &domain.migrations))
            .map(|migration| Path::new(&migration.file))
            .collect();
        Self::reject_unregistered_sql(root, root, &registered)
    }

    /// Reads one SQL file and verifies its exact bytes immediately before use.
    pub fn read_verified_sql(
        root: &Path,
        migration: &CatalogMigration,
    ) -> Result<String, PersistenceError> {
        let path = Self::migration_path(root, &migration.file)?;
        let bytes = fs::read(&path).map_err(|error| {
            PersistenceError::Catalog(format!("cannot read {}: {error}", path.display()))
        })?;
        if let Some(expected) = &migration.sha256 {
            let observed = Sha256Digest::of_bytes(&bytes);
            if &observed != expected {
                return Err(PersistenceError::IdentityMismatch(format!(
                    "{} expected {} but observed {}",
                    migration.file, expected, observed
                )));
            }
        }
        let sql = String::from_utf8(bytes).map_err(|error| {
            PersistenceError::Catalog(format!("{} is not UTF-8: {error}", migration.file))
        })?;
        reject_non_transactional_sql(&migration.file, &sql)?;
        Ok(sql)
    }

    /// Resolves a catalog-relative SQL path without allowing traversal.
    pub fn migration_path(root: &Path, file: &str) -> Result<PathBuf, PersistenceError> {
        let relative = Path::new(file);
        if relative.is_absolute()
            || relative
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
            || relative.extension().and_then(|value| value.to_str()) != Some("sql")
        {
            return Err(PersistenceError::Catalog(format!(
                "unsafe Migration filename: {file}"
            )));
        }
        Ok(root.join(relative))
    }

    fn validate(&self, root: &Path) -> Result<(), PersistenceError> {
        if self.catalog_version != 1 || self.tool_version != env!("CARGO_PKG_VERSION") {
            return Err(PersistenceError::Catalog(
                "unsupported catalog or tool version".to_owned(),
            ));
        }
        if self.domains.len() != Domain::ALL.len()
            || self.domains.iter().map(|entry| entry.name).ne(Domain::ALL)
        {
            return Err(PersistenceError::Catalog(
                "domain order must be control, access, environment, agent, evaluation, resource"
                    .to_owned(),
            ));
        }
        Self::verify_file(root, &self.bootstrap)?;
        for domain in &self.domains {
            Self::validate_contiguous_ids(domain)?;
            let mut ids = BTreeSet::new();
            let mut files = BTreeSet::new();
            if domain.migrations.is_empty() {
                return Err(PersistenceError::Catalog(format!(
                    "{} has no Migration",
                    domain.name
                )));
            }
            for migration in &domain.migrations {
                if migration.id == 0 || !ids.insert(migration.id) || !files.insert(&migration.file)
                {
                    return Err(PersistenceError::Catalog(format!(
                        "{} Migration IDs and files must be unique and ordered",
                        domain.name
                    )));
                }
                Self::verify_file(root, migration)?;
            }
        }
        Ok(())
    }

    fn validate_contiguous_ids(domain: &CatalogDomain) -> Result<(), PersistenceError> {
        for (index, migration) in domain.migrations.iter().enumerate() {
            let expected = u64::try_from(index + 1).map_err(|_| {
                PersistenceError::Catalog(format!("{} Migration index exceeds u64", domain.name))
            })?;
            if migration.id != expected {
                return Err(PersistenceError::Catalog(format!(
                    "{} Migration IDs must be contiguous starting at 1: expected {}, found {}",
                    domain.name, expected, migration.id
                )));
            }
        }
        Ok(())
    }

    fn reject_unregistered_sql(
        root: &Path,
        directory: &Path,
        registered: &BTreeSet<&Path>,
    ) -> Result<(), PersistenceError> {
        let entries = fs::read_dir(directory)
            .map_err(|error| PersistenceError::Catalog(error.to_string()))?;
        for entry in entries {
            let entry = entry.map_err(|error| PersistenceError::Catalog(error.to_string()))?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|error| PersistenceError::Catalog(error.to_string()))?;
            if file_type.is_dir() {
                Self::reject_unregistered_sql(root, &path, registered)?;
            } else if path.extension().and_then(|extension| extension.to_str()) == Some("sql") {
                let relative = path
                    .strip_prefix(root)
                    .map_err(|error| PersistenceError::Catalog(error.to_string()))?;
                if !registered.contains(relative) {
                    return Err(PersistenceError::Catalog(format!(
                        "Migration SQL file is not registered in catalog: {}",
                        relative.display()
                    )));
                }
            }
        }
        Ok(())
    }

    fn verify_file(root: &Path, migration: &CatalogMigration) -> Result<(), PersistenceError> {
        Self::read_verified_sql(root, migration).map(|_| ())
    }
}

fn reject_non_transactional_sql(file: &str, sql: &str) -> Result<(), PersistenceError> {
    let normalized = sql.to_ascii_uppercase();
    for forbidden in [
        "BEGIN;",
        "COMMIT;",
        "ROLLBACK;",
        "CREATE INDEX CONCURRENTLY",
        "DROP INDEX CONCURRENTLY",
        "VACUUM ",
    ] {
        if normalized.contains(forbidden) {
            return Err(PersistenceError::Catalog(format!(
                "{file} contains unsupported non-transactional statement: {forbidden}"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{CatalogMigration, MigrationCatalog};
    use crate::Sha256Digest;

    #[test]
    fn repository_catalog_is_valid_and_deterministic() -> Result<(), Box<dyn std::error::Error>> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
        let catalog = MigrationCatalog::load(&root.join("catalog.yaml"))?;
        assert_eq!(catalog.sha256()?, catalog.sha256()?);
        Ok(())
    }

    #[test]
    fn catalog_rejects_unregistered_migration_files() -> Result<(), Box<dyn std::error::Error>> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
        let mut catalog = MigrationCatalog::load(&root.join("catalog.yaml"))?;
        // Leave the real SQL file in place, removing only its otherwise-contiguous registration.
        let orphan = catalog
            .domains
            .iter_mut()
            .find(|domain| domain.name == crate::Domain::Agent)
            .and_then(|domain| domain.migrations.pop())
            .ok_or("missing Agent migration")?;
        let error = catalog
            .verify_files(&root)
            .err()
            .ok_or("unregistered SQL file was accepted")?;
        assert!(error.to_string().contains("not registered in catalog"));
        assert!(error.to_string().contains(&orphan.file));
        Ok(())
    }

    #[test]
    fn catalog_rejects_domain_migration_gaps() -> Result<(), Box<dyn std::error::Error>> {
        let domain = super::CatalogDomain {
            name: crate::Domain::Resource,
            migrations: vec![
                CatalogMigration {
                    id: 1,
                    file: "resource/0001.sql".to_owned(),
                    sha256: None,
                },
                CatalogMigration {
                    id: 3,
                    file: "resource/0003.sql".to_owned(),
                    sha256: None,
                },
            ],
        };
        let error = MigrationCatalog::validate_contiguous_ids(&domain)
            .err()
            .ok_or("catalog gap was accepted")?;
        assert!(error.to_string().contains("expected 2, found 3"));
        Ok(())
    }

    #[test]
    fn execution_time_read_rejects_post_load_tampering() -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!("labweaver-catalog-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(root.join("control"))?;
        let file = root.join("control/0001.sql");
        std::fs::write(&file, "CREATE TABLE expected_identity (id integer);")?;
        let migration = CatalogMigration {
            id: 1,
            file: "control/0001.sql".to_owned(),
            sha256: Some(Sha256Digest::of_bytes(
                b"CREATE TABLE expected_identity (id integer);",
            )),
        };
        std::fs::write(&file, "CREATE TABLE substituted_identity (id integer);")?;
        let Err(error) = MigrationCatalog::read_verified_sql(&root, &migration) else {
            return Err("tampered migration was accepted".into());
        };
        assert_eq!(error.diagnostic_code(), "DB_MIGRATION_IDENTITY_MISMATCH");
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn thinned_catalog_without_sha256_passes_existence_check()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!("labweaver-catalog-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(root.join("control"))?;
        let file = root.join("control/0001.sql");
        std::fs::write(&file, "CREATE TABLE t (id integer);")?;
        let migration = CatalogMigration {
            id: 1,
            file: "control/0001.sql".to_owned(),
            sha256: None,
        };
        assert!(MigrationCatalog::read_verified_sql(&root, &migration).is_ok());
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
