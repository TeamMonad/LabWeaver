use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use contracts::Sha256Digest;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Acquire, PgConnection, PgPool, Row};

use crate::{Domain, MigrationCatalog, PersistenceError, SchemaStatus, SchemaVerifier};

/// Runtime identity supplied by immutable Job metadata, not by the checked-in catalog.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MigrationIdentity {
    /// Deployment cluster identity used to derive advisory locks.
    pub cluster_uuid: String,
    /// Release identity.
    pub release_id: String,
    /// Source Git commit.
    pub git_commit: String,
    /// Immutable build or image digest.
    pub build_digest: String,
    /// Migration Job identity.
    pub job_id: String,
    /// Unique execution attempt identity.
    pub attempt_id: String,
}

impl MigrationIdentity {
    /// Validates bounded, printable, non-secret identity fields.
    pub fn validate(&self) -> Result<(), PersistenceError> {
        for (name, value) in [
            ("cluster UUID", &self.cluster_uuid),
            ("release ID", &self.release_id),
            ("Git commit", &self.git_commit),
            ("build digest", &self.build_digest),
            ("Job ID", &self.job_id),
            ("attempt ID", &self.attempt_id),
        ] {
            if value.trim().is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
                return Err(PersistenceError::Configuration(format!(
                    "{name} is empty, oversized, or contains control characters"
                )));
            }
        }
        Ok(())
    }
}

/// Per-domain Migration result included in machine-readable evidence.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DomainMigrationReport {
    /// Domain identity.
    pub domain: Domain,
    /// Number of newly applied files.
    pub applied: u64,
    /// Final schema identity status.
    pub status: String,
}

/// Canonical report body. It does not contain its own hash.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MigrationReport {
    /// Report schema version.
    pub schema_version: u32,
    /// Catalog identity.
    pub catalog_sha256: Sha256Digest,
    /// Runtime execution identity.
    pub identity: MigrationIdentity,
    /// Final outcome.
    pub outcome: String,
    /// Ordered domain outcomes.
    pub domains: Vec<DomainMigrationReport>,
    /// Stable diagnostic on failure.
    pub diagnostic: Option<String>,
}

/// Self-verifying report envelope. The hash covers only `report` canonical JSON.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MigrationReportEnvelope {
    /// Canonical report body.
    pub report: MigrationReport,
    /// RFC 8785 canonical hash of `report`.
    pub report_sha256: Sha256Digest,
}

impl MigrationReportEnvelope {
    /// Creates a self-verifying envelope for a completed operation.
    pub fn success(
        catalog_sha256: Sha256Digest,
        identity: MigrationIdentity,
        domains: Vec<DomainMigrationReport>,
    ) -> Result<Self, PersistenceError> {
        Self::new(MigrationReport {
            schema_version: 1,
            catalog_sha256,
            identity,
            outcome: "succeeded".to_owned(),
            domains,
            diagnostic: None,
        })
    }

    fn new(report: MigrationReport) -> Result<Self, PersistenceError> {
        let report_sha256 = Sha256Digest::of_canonical(&report)
            .map_err(|error| PersistenceError::Report(error.to_string()))?;
        Ok(Self {
            report,
            report_sha256,
        })
    }

    /// Writes stable pretty JSON after recomputing the envelope hash.
    pub fn write(&self, path: &Path) -> Result<(), PersistenceError> {
        let expected = Sha256Digest::of_canonical(&self.report)
            .map_err(|error| PersistenceError::Report(error.to_string()))?;
        if expected != self.report_sha256 {
            return Err(PersistenceError::IdentityMismatch(
                "report envelope hash does not match its body".to_owned(),
            ));
        }
        let parent = path.parent().ok_or_else(|| {
            PersistenceError::Report("report path has no parent directory".to_owned())
        })?;
        fs::create_dir_all(parent).map_err(|error| {
            PersistenceError::Report(format!("cannot create report directory: {error}"))
        })?;
        let mut bytes = serde_json::to_vec_pretty(self)
            .map_err(|error| PersistenceError::Report(error.to_string()))?;
        bytes.push(b'\n');
        fs::write(path, bytes)
            .map_err(|error| PersistenceError::Report(format!("cannot write report: {error}")))
    }
}

/// Controlled release coordinator with explicit, identity-isolated pools.
pub struct MigrationCoordinator {
    coordinator: PgPool,
    domains: BTreeMap<Domain, PgPool>,
    identity: MigrationIdentity,
}

impl MigrationCoordinator {
    /// Bootstraps fixed roles and schemas through an explicitly supplied provisioner pool.
    pub async fn bootstrap(
        provisioner: &PgPool,
        catalog: &MigrationCatalog,
        catalog_root: &Path,
    ) -> Result<(), PersistenceError> {
        let path = MigrationCatalog::migration_path(catalog_root, &catalog.bootstrap.file)?;
        let sql = fs::read_to_string(&path).map_err(|error| {
            PersistenceError::Catalog(format!("cannot read {}: {error}", path.display()))
        })?;
        let mut connection = provisioner.acquire().await?;
        let row = sqlx::query(
            "SELECT rolsuper OR rolcreaterole AS permitted FROM pg_roles WHERE rolname = current_user",
        )
        .fetch_one(&mut *connection)
        .await?;
        if !row.try_get::<bool, _>("permitted")? {
            return Err(PersistenceError::IdentityMismatch(
                "provisioner lacks the required role-creation capability".to_owned(),
            ));
        }
        let mut transaction = connection.begin().await?;
        sqlx::raw_sql(&sql).execute(&mut *transaction).await?;
        transaction.commit().await?;
        Ok(())
    }

    /// Creates a coordinator only when every domain has one explicit pool.
    pub fn new(
        coordinator: PgPool,
        domains: BTreeMap<Domain, PgPool>,
        identity: MigrationIdentity,
    ) -> Result<Self, PersistenceError> {
        identity.validate()?;
        if domains.len() != Domain::ALL.len()
            || Domain::ALL
                .iter()
                .any(|domain| !domains.contains_key(domain))
        {
            return Err(PersistenceError::Configuration(
                "one explicit Migration pool is required for every domain".to_owned(),
            ));
        }
        Ok(Self {
            coordinator,
            domains,
            identity,
        })
    }

    /// Applies every pending catalog Migration while holding the global release lock.
    pub async fn apply(
        &self,
        catalog: &MigrationCatalog,
        catalog_root: &Path,
    ) -> Result<MigrationReportEnvelope, PersistenceError> {
        let catalog_hash = catalog.sha256()?;
        let mut coordinator = self.coordinator.acquire().await?;
        require_current_user(&mut coordinator, "lw_release_coordinator").await?;
        let (global_a, global_b) =
            lock_keys(&self.identity.cluster_uuid, "labweaver:migration-release");
        sqlx::query("SELECT pg_advisory_lock($1, $2)")
            .bind(global_a)
            .bind(global_b)
            .execute(&mut *coordinator)
            .await?;
        self.prepare_attempt(&mut coordinator, catalog_hash).await?;

        let mut reports = Vec::new();
        for domain_catalog in &catalog.domains {
            self.set_current_domain(&mut coordinator, domain_catalog.name)
                .await?;
            match self
                .apply_domain(domain_catalog, catalog_root, catalog_hash)
                .await
            {
                Ok(applied) => reports.push(DomainMigrationReport {
                    domain: domain_catalog.name,
                    applied,
                    status: "ready".to_owned(),
                }),
                Err(error) => {
                    self.fail_attempt(&mut coordinator, error.diagnostic_code())
                        .await?;
                    return Err(error);
                }
            }
        }
        let envelope =
            MigrationReportEnvelope::success(catalog_hash, self.identity.clone(), reports)?;
        sqlx::query(
            "UPDATE platform_meta.release_attempts SET state = 'succeeded', finished_at = now(), \
             report_sha256 = $2 WHERE attempt_id = $1 AND state = 'running'",
        )
        .bind(&self.identity.attempt_id)
        .bind(envelope.report_sha256.to_string())
        .execute(&mut *coordinator)
        .await?;
        Ok(envelope)
    }

    async fn prepare_attempt(
        &self,
        connection: &mut PgConnection,
        catalog_hash: Sha256Digest,
    ) -> Result<(), PersistenceError> {
        sqlx::query(
            "UPDATE platform_meta.release_attempts SET state = 'failed', finished_at = now(), \
             diagnostic = 'DB_MIGRATION_ABANDONED_ATTEMPT' WHERE state = 'running' AND attempt_id <> $1",
        )
        .bind(&self.identity.attempt_id)
        .execute(&mut *connection)
        .await?;
        let blocked = sqlx::query(
            "SELECT release_id, catalog_sha256, git_commit, build_digest FROM platform_meta.release_attempts \
             WHERE state = 'failed' AND resolved_at IS NULL ORDER BY started_at DESC LIMIT 1",
        )
        .fetch_optional(&mut *connection)
        .await?;
        if let Some(row) = blocked {
            let same = row.try_get::<String, _>("release_id")? == self.identity.release_id
                && row.try_get::<String, _>("catalog_sha256")? == catalog_hash.to_string()
                && row.try_get::<String, _>("git_commit")? == self.identity.git_commit
                && row.try_get::<String, _>("build_digest")? == self.identity.build_digest;
            if !same {
                return Err(PersistenceError::ReleaseBlocked(
                    "an unresolved failed attempt has a different release identity".to_owned(),
                ));
            }
        }
        sqlx::query(
            "INSERT INTO platform_meta.release_attempts \
             (attempt_id, release_id, catalog_sha256, git_commit, build_digest, job_id, state) \
             VALUES ($1, $2, $3, $4, $5, $6, 'running') \
             ON CONFLICT (attempt_id) DO NOTHING",
        )
        .bind(&self.identity.attempt_id)
        .bind(&self.identity.release_id)
        .bind(catalog_hash.to_string())
        .bind(&self.identity.git_commit)
        .bind(&self.identity.build_digest)
        .bind(&self.identity.job_id)
        .execute(&mut *connection)
        .await?;
        Ok(())
    }

    async fn set_current_domain(
        &self,
        connection: &mut PgConnection,
        domain: Domain,
    ) -> Result<(), PersistenceError> {
        sqlx::query(
            "UPDATE platform_meta.release_attempts SET current_domain = $2 \
             WHERE attempt_id = $1 AND state = 'running'",
        )
        .bind(&self.identity.attempt_id)
        .bind(domain.to_string())
        .execute(connection)
        .await?;
        Ok(())
    }

    async fn fail_attempt(
        &self,
        connection: &mut PgConnection,
        diagnostic: &str,
    ) -> Result<(), PersistenceError> {
        sqlx::query(
            "UPDATE platform_meta.release_attempts SET state = 'failed', finished_at = now(), diagnostic = $2 \
             WHERE attempt_id = $1 AND state = 'running'",
        )
        .bind(&self.identity.attempt_id)
        .bind(diagnostic)
        .execute(connection)
        .await?;
        Ok(())
    }

    async fn apply_domain(
        &self,
        catalog: &crate::CatalogDomain,
        root: &Path,
        catalog_hash: Sha256Digest,
    ) -> Result<u64, PersistenceError> {
        let pool = self.domains.get(&catalog.name).ok_or_else(|| {
            PersistenceError::Configuration(format!("missing {} Migration pool", catalog.name))
        })?;
        let mut connection = pool.acquire().await?;
        require_current_user(&mut connection, catalog.name.migration_role()).await?;
        let (lock_a, lock_b) = lock_keys(
            &self.identity.cluster_uuid,
            &format!("labweaver:migration-domain:{}", catalog.name),
        );
        sqlx::query("SELECT pg_advisory_lock($1, $2)")
            .bind(lock_a)
            .bind(lock_b)
            .execute(&mut *connection)
            .await?;
        let status = SchemaVerifier::classify(pool, catalog).await;
        if !matches!(status, SchemaStatus::Ready | SchemaStatus::Behind) {
            return Err(PersistenceError::IdentityMismatch(
                status
                    .diagnostic_code()
                    .unwrap_or("DB_SCHEMA_UNKNOWN")
                    .to_owned(),
            ));
        }
        let count_query = format!(
            "SELECT count(*)::bigint AS count FROM {}.schema_migrations",
            catalog.name.schema()
        );
        let applied_count: i64 = sqlx::query(&count_query)
            .fetch_one(&mut *connection)
            .await?
            .try_get("count")?;
        let start = usize::try_from(applied_count).map_err(|_| {
            PersistenceError::IdentityMismatch("negative Migration history count".to_owned())
        })?;
        let mut applied = 0_u64;
        for migration in catalog.migrations.iter().skip(start) {
            let path = MigrationCatalog::migration_path(root, &migration.file)?;
            let sql = fs::read_to_string(&path).map_err(|error| {
                PersistenceError::Catalog(format!("cannot read {}: {error}", path.display()))
            })?;
            let mut transaction = connection.begin().await?;
            let set_role = format!("SET LOCAL ROLE {}", catalog.name.owner_role());
            sqlx::query(&set_role).execute(&mut *transaction).await?;
            sqlx::raw_sql(&sql).execute(&mut *transaction).await?;
            sqlx::query("RESET ROLE").execute(&mut *transaction).await?;
            let history = format!(
                "INSERT INTO {}.schema_migrations \
                 (migration_id, filename, sha256, outcome, executor_identity, release_id, catalog_sha256) \
                 VALUES ($1, $2, $3, 'applied', $4, $5, $6)",
                catalog.name.schema()
            );
            let migration_id = i64::try_from(migration.id)
                .map_err(|_| PersistenceError::Catalog("Migration ID exceeds BIGINT".to_owned()))?;
            sqlx::query(&history)
                .bind(migration_id)
                .bind(&migration.file)
                .bind(migration.sha256.to_string())
                .bind(&self.identity.job_id)
                .bind(&self.identity.release_id)
                .bind(catalog_hash.to_string())
                .execute(&mut *transaction)
                .await?;
            transaction.commit().await?;
            applied = applied.saturating_add(1);
        }
        Ok(applied)
    }
}

async fn require_current_user(
    connection: &mut PgConnection,
    expected: &str,
) -> Result<(), PersistenceError> {
    let observed: String = sqlx::query("SELECT current_user::text AS current_user")
        .fetch_one(connection)
        .await?
        .try_get("current_user")?;
    if observed != expected {
        return Err(PersistenceError::IdentityMismatch(format!(
            "expected database role {expected}, observed {observed}"
        )));
    }
    Ok(())
}

fn lock_keys(cluster_uuid: &str, purpose: &str) -> (i32, i32) {
    let mut digest = Sha256::new();
    digest.update(cluster_uuid.as_bytes());
    digest.update(purpose.as_bytes());
    let bytes = digest.finalize();
    (
        i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        i32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
    )
}

#[cfg(test)]
mod tests {
    use super::{MigrationIdentity, MigrationReport, MigrationReportEnvelope};
    use contracts::{Sha256Digest, foundation::FoundationError};

    #[test]
    fn report_hash_excludes_the_envelope() -> Result<(), Box<dyn std::error::Error>> {
        let report = MigrationReport {
            schema_version: 1,
            catalog_sha256: "00".repeat(32).parse::<Sha256Digest>()?,
            identity: MigrationIdentity {
                cluster_uuid: "cluster".to_owned(),
                release_id: "release".to_owned(),
                git_commit: "commit".to_owned(),
                build_digest: "sha256:build".to_owned(),
                job_id: "job".to_owned(),
                attempt_id: "attempt".to_owned(),
            },
            outcome: "succeeded".to_owned(),
            domains: Vec::new(),
            diagnostic: None,
        };
        let envelope = MigrationReportEnvelope::new(report)?;
        assert_eq!(
            envelope.report_sha256,
            Sha256Digest::of_canonical(&envelope.report).map_err(|error: FoundationError| error)?
        );
        Ok(())
    }
}
