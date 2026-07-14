use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use contracts::Sha256Digest;
use serde::{Deserialize, Serialize};

use crate::{Domain, PersistenceError};

/// One immutable SQL file declared by the Migration catalog.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogMigration {
    /// Monotonically increasing domain-local Migration identifier.
    pub id: u64,
    /// Catalog-relative, normalized SQL filename.
    pub file: String,
    /// Exact file content identity.
    pub sha256: Sha256Digest,
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
    /// Loads a catalog and verifies every declared file hash.
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
        self.validate(root)
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
        let observed = Sha256Digest::of_bytes(&bytes);
        if observed != migration.sha256 {
            return Err(PersistenceError::IdentityMismatch(format!(
                "{} expected {} but observed {}",
                migration.file, migration.sha256, observed
            )));
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
            let mut ids = BTreeSet::new();
            let mut files = BTreeSet::new();
            let mut previous = 0;
            if domain.migrations.is_empty() {
                return Err(PersistenceError::Catalog(format!(
                    "{} has no Migration",
                    domain.name
                )));
            }
            for migration in &domain.migrations {
                if migration.id == 0
                    || migration.id <= previous
                    || !ids.insert(migration.id)
                    || !files.insert(&migration.file)
                {
                    return Err(PersistenceError::Catalog(format!(
                        "{} Migration IDs and files must be unique and ordered",
                        domain.name
                    )));
                }
                previous = migration.id;
                Self::verify_file(root, migration)?;
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
    use contracts::Sha256Digest;

    #[test]
    fn repository_catalog_is_valid_and_deterministic() -> Result<(), Box<dyn std::error::Error>> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
        let catalog = MigrationCatalog::load(&root.join("catalog.yaml"))?;
        assert_eq!(catalog.sha256()?, catalog.sha256()?);
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
            sha256: Sha256Digest::of_bytes(b"CREATE TABLE expected_identity (id integer);"),
        };
        std::fs::write(&file, "CREATE TABLE substituted_identity (id integer);")?;
        let Err(error) = MigrationCatalog::read_verified_sql(&root, &migration) else {
            return Err("tampered migration was accepted".into());
        };
        assert_eq!(error.diagnostic_code(), "DB_MIGRATION_IDENTITY_MISMATCH");
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
}
