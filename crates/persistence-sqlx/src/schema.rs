use std::fmt::{Display, Formatter};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::Sha256Digest;
use sqlx::{PgPool, Row};

use crate::{CatalogDomain, PersistenceError};

/// Closed set of `PostgreSQL` business domains in release order.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Domain {
    /// Control Service state.
    Control,
    /// Access Service state.
    Access,
    /// Environment Service state.
    Environment,
    /// Agent Service state.
    Agent,
    /// Evaluation Service state.
    Evaluation,
    /// Resource Service state.
    Resource,
}

impl Domain {
    /// Canonical release order.
    pub const ALL: [Self; 6] = [
        Self::Control,
        Self::Access,
        Self::Environment,
        Self::Agent,
        Self::Evaluation,
        Self::Resource,
    ];

    /// Returns the fixed `PostgreSQL` schema.
    #[must_use]
    pub const fn schema(self) -> &'static str {
        match self {
            Self::Control => "control",
            Self::Access => "access",
            Self::Environment => "environment",
            Self::Agent => "agent",
            Self::Evaluation => "evaluation",
            Self::Resource => "resource",
        }
    }

    /// Returns the fixed Migration login.
    #[must_use]
    pub const fn migration_role(self) -> &'static str {
        match self {
            Self::Control => "lw_control_migration",
            Self::Access => "lw_access_migration",
            Self::Environment => "lw_environment_migration",
            Self::Agent => "lw_agent_migration",
            Self::Evaluation => "lw_evaluation_migration",
            Self::Resource => "lw_resource_migration",
        }
    }

    /// Returns the fixed NOLOGIN schema owner.
    #[must_use]
    pub const fn owner_role(self) -> &'static str {
        match self {
            Self::Control => "lw_control_owner",
            Self::Access => "lw_access_owner",
            Self::Environment => "lw_environment_owner",
            Self::Agent => "lw_agent_owner",
            Self::Evaluation => "lw_evaluation_owner",
            Self::Resource => "lw_resource_owner",
        }
    }

    /// Returns the fixed runtime login.
    #[must_use]
    pub const fn runtime_role(self) -> &'static str {
        match self {
            Self::Control => "lw_control_runtime",
            Self::Access => "lw_access_runtime",
            Self::Environment => "lw_environment_runtime",
            Self::Agent => "lw_agent_runtime",
            Self::Evaluation => "lw_evaluation_runtime",
            Self::Resource => "lw_resource_runtime",
        }
    }
}

impl Display for Domain {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.schema())
    }
}

/// Result of comparing one live schema with the immutable catalog.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SchemaStatus {
    /// History exactly matches the catalog.
    Ready,
    /// Schema or history does not exist.
    Missing,
    /// History contains an unrecognized identity.
    Unknown,
    /// History is newer than this build.
    Ahead,
    /// History is an exact prefix of this build.
    Behind,
    /// History has a gap or non-applied outcome.
    Incomplete,
    /// A known ID has different immutable content.
    ChecksumMismatch,
    /// `PostgreSQL` could not be reached.
    Unavailable,
}

impl SchemaStatus {
    /// Stable startup/readiness diagnostic, if the schema is not ready.
    #[must_use]
    pub const fn diagnostic_code(self) -> Option<&'static str> {
        match self {
            Self::Ready => None,
            Self::Missing => Some("DB_SCHEMA_MISSING"),
            Self::Unknown => Some("DB_SCHEMA_UNKNOWN"),
            Self::Ahead => Some("DB_SCHEMA_AHEAD"),
            Self::Behind => Some("DB_SCHEMA_BEHIND"),
            Self::Incomplete => Some("DB_SCHEMA_INCOMPLETE"),
            Self::ChecksumMismatch => Some("DB_SCHEMA_CHECKSUM_MISMATCH"),
            Self::Unavailable => Some("DB_SCHEMA_UNAVAILABLE"),
        }
    }
}

/// Read-only schema identity verifier. It never applies or repairs a Migration.
pub struct SchemaVerifier;

impl SchemaVerifier {
    /// Classifies the live history against one catalog domain.
    pub async fn classify(pool: &PgPool, expected: &CatalogDomain) -> SchemaStatus {
        let query = format!(
            "SELECT migration_id, sha256, outcome FROM {}.schema_migrations ORDER BY migration_id",
            expected.name.schema()
        );
        let rows = match sqlx::query(&query).fetch_all(pool).await {
            Ok(rows) => rows,
            Err(error) if is_missing_schema(&error) => return SchemaStatus::Missing,
            Err(_) => return SchemaStatus::Unavailable,
        };
        if rows.len() > expected.migrations.len() {
            return SchemaStatus::Ahead;
        }
        for (index, row) in rows.iter().enumerate() {
            let Ok(id) = row.try_get::<i64, _>("migration_id") else {
                return SchemaStatus::Unknown;
            };
            let Ok(hash) = row.try_get::<String, _>("sha256") else {
                return SchemaStatus::Unknown;
            };
            let Ok(outcome) = row.try_get::<String, _>("outcome") else {
                return SchemaStatus::Unknown;
            };
            let Some(migration) = expected.migrations.get(index) else {
                return SchemaStatus::Ahead;
            };
            if u64::try_from(id).ok() != Some(migration.id) {
                return if id > i64::try_from(migration.id).unwrap_or(i64::MAX) {
                    SchemaStatus::Ahead
                } else {
                    SchemaStatus::Unknown
                };
            }
            if Sha256Digest::from_str(&hash).ok() != Some(migration.sha256) {
                return SchemaStatus::ChecksumMismatch;
            }
            if outcome != "applied" {
                return SchemaStatus::Incomplete;
            }
        }
        if rows.len() < expected.migrations.len() {
            SchemaStatus::Behind
        } else {
            SchemaStatus::Ready
        }
    }

    /// Returns an error when a live schema is not ready.
    pub async fn require_ready(
        pool: &PgPool,
        expected: &CatalogDomain,
    ) -> Result<(), PersistenceError> {
        let status = Self::classify(pool, expected).await;
        if status == SchemaStatus::Ready {
            Ok(())
        } else {
            Err(PersistenceError::IdentityMismatch(
                status
                    .diagnostic_code()
                    .unwrap_or("DB_SCHEMA_UNKNOWN")
                    .to_owned(),
            ))
        }
    }
}

fn is_missing_schema(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .is_some_and(|code| code == "42P01" || code == "3F000")
}
