//! `PostgreSQL` persistence primitives and controlled Migration execution.
#![allow(clippy::missing_errors_doc)]

mod catalog;
mod coordinator;
mod ledger;
mod schema;

pub use catalog::{CatalogDomain, CatalogMigration, MigrationCatalog};
pub use coordinator::{
    DomainMigrationReport, MigrationCoordinator, MigrationIdentity, MigrationReport,
    MigrationReportEnvelope,
};
pub use ledger::{IdempotencyDecision, IdempotencyStore, InboxDecision, InboxStore, OutboxStore};
pub use schema::{Domain, SchemaStatus, SchemaVerifier};

/// Stable persistence and Migration diagnostics.
#[derive(Debug, thiserror::Error)]
pub enum PersistenceError {
    /// A checked-in catalog or Migration file is invalid.
    #[error("migration catalog is invalid: {0}")]
    Catalog(String),
    /// Required explicit configuration is absent or malformed.
    #[error("migration configuration is invalid: {0}")]
    Configuration(String),
    /// An immutable identity differs from the expected value.
    #[error("migration identity mismatch: {0}")]
    IdentityMismatch(String),
    /// A release cannot continue until a reviewed forward repair resolves an earlier attempt.
    #[error("migration release is blocked: {0}")]
    ReleaseBlocked(String),
    /// A database operation failed.
    #[error("database operation failed: {0}")]
    Database(#[from] sqlx::Error),
    /// A report could not be serialized or written.
    #[error("migration report failed: {0}")]
    Report(String),
}

impl PersistenceError {
    /// Returns a stable, payload-free diagnostic code.
    #[must_use]
    pub const fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::Catalog(_) => "DB_MIGRATION_CATALOG_INVALID",
            Self::Configuration(_) => "DB_MIGRATION_CONFIGURATION_INVALID",
            Self::IdentityMismatch(_) => "DB_MIGRATION_IDENTITY_MISMATCH",
            Self::ReleaseBlocked(_) => "DB_MIGRATION_RELEASE_BLOCKED",
            Self::Database(_) => "DB_MIGRATION_DATABASE_FAILED",
            Self::Report(_) => "DB_MIGRATION_REPORT_FAILED",
        }
    }
}
