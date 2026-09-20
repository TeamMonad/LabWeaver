//! Administrator-curated platform image catalog.
//!
//! The catalog is the single platform-owned list of base images the sandbox may build from.
//! Registrations resolve one tag reference into an immutable digest and persist both; later
//! repins and disables are compare-and-set on the observed digest and append an audit row in
//! the same transaction. Referenced entries are disabled, never deleted.

use contracts::{ActorId, UtcTimestamp};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use uuid::Uuid;

/// Reviewed image kinds.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformImageKind {
    /// Container base image for sandbox builds.
    Container,
    /// Virtual machine base disk template.
    VirtualMachine,
}

impl PlatformImageKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Container => "container",
            Self::VirtualMachine => "virtual_machine",
        }
    }
}

/// Catalog lifecycle status.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformImageStatus {
    /// Listed for authoring and usable by the sandbox prompt.
    Active,
    /// Hidden from new authoring; retained while a release still references it.
    Disabled,
}

/// One pinned platform image identity.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlatformImageEntry {
    /// Catalog row identity.
    pub catalog_id: Uuid,
    /// Reviewed kind.
    pub kind: PlatformImageKind,
    /// Stable resolution key used by authoring templates.
    pub binding: String,
    /// Administrator-provided reference; the tag is only a resolution entry point.
    pub source_reference: String,
    /// Persisted immutable identity; downstream consumers use only this digest.
    pub resolved_digest: String,
    /// Manifest media type observed at resolution time.
    pub media_type: String,
    /// Reviewed content size in bytes.
    pub size_bytes: u64,
    /// Lifecycle status.
    pub status: PlatformImageStatus,
    /// Trust revision the administrator pinned under.
    pub trust_revision: u64,
    /// Monotonic repin generation, incremented on every repin.
    pub repin_generation: u64,
    /// Time the current digest was pinned.
    pub pinned_at: UtcTimestamp,
    /// Last mutation time.
    pub updated_at: UtcTimestamp,
}

/// Registration input with an already resolved digest.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegisterPlatformImage {
    /// Reviewed kind.
    pub kind: PlatformImageKind,
    /// Stable resolution key.
    pub binding: String,
    /// Reference the administrator entered; kept for audit and repin.
    pub source_reference: String,
    /// Resolved digest persisted as the authoritative identity.
    pub resolved_digest: String,
    /// Manifest media type observed during resolution.
    pub media_type: String,
    /// Reviewed content size in bytes.
    pub size_bytes: u64,
    /// Trust revision the administrator pinned under.
    pub trust_revision: u64,
    /// Authenticated administrator.
    pub actor_id: ActorId,
    /// Human-readable change reason.
    pub reason: String,
    /// Mutation time.
    pub now: UtcTimestamp,
}

/// Repin input. The write only succeeds when `expected_digest` is still current.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RepinPlatformImage {
    /// Resolved digest replacing the current pin.
    pub resolved_digest: String,
    /// Manifest media type observed during resolution.
    pub media_type: String,
    /// Reviewed content size in bytes.
    pub size_bytes: u64,
    /// Digest the administrator observed before repinning.
    pub expected_digest: String,
    /// Trust revision the administrator pinned under.
    pub trust_revision: u64,
    /// Authenticated administrator.
    pub actor_id: ActorId,
    /// Human-readable change reason.
    pub reason: String,
    /// Mutation time.
    pub now: UtcTimestamp,
}

/// Disable input. The write only succeeds when `expected_digest` is still current.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DisablePlatformImage {
    /// Digest the administrator observed before disabling.
    pub expected_digest: String,
    /// Authenticated administrator.
    pub actor_id: ActorId,
    /// Human-readable change reason.
    pub reason: String,
    /// Mutation time.
    pub now: UtcTimestamp,
}

/// Catalog failure modes.
#[derive(Debug, thiserror::Error)]
pub enum PlatformImageStoreError {
    /// The request is not a valid catalog mutation.
    #[error("LW_PLATFORM_IMAGE_REQUEST_INVALID")]
    InvalidRequest,
    /// The catalog entry does not exist.
    #[error("LW_PLATFORM_IMAGE_NOT_FOUND")]
    NotFound,
    /// The binding or digest changed since the administrator observed it.
    #[error("LW_PLATFORM_IMAGE_STATE_CONFLICT")]
    Conflict,
    /// The durable store failed.
    #[error("LW_PLATFORM_IMAGE_PERSISTENCE_FAILED")]
    Persistence,
}

/// Durable platform image catalog.
#[derive(Clone, Debug)]
pub struct PgPlatformImageCatalog {
    pool: PgPool,
}

impl PgPlatformImageCatalog {
    /// Builds the catalog over the Agent database.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Persists one registration and its audit row.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformImageStoreError::InvalidRequest`] for an invalid identity,
    /// [`PlatformImageStoreError::Conflict`] when the binding already exists, and
    /// [`PlatformImageStoreError::Persistence`] when the durable write fails.
    pub async fn register(
        &self,
        request: &RegisterPlatformImage,
    ) -> Result<PlatformImageEntry, PlatformImageStoreError> {
        request.validate()?;
        let catalog_id = Uuid::new_v4();
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| PlatformImageStoreError::Persistence)?;
        let inserted = sqlx::query(
            "INSERT INTO agent.platform_image_catalog \
             (catalog_id,kind,binding,source_reference,resolved_digest,media_type,size_bytes,\
              status,trust_revision,created_by,created_at,updated_at,pinned_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,'active',$8,$9,$10,$10,$10) \
             ON CONFLICT (kind,binding) DO NOTHING",
        )
        .bind(catalog_id)
        .bind(request.kind.as_str())
        .bind(&request.binding)
        .bind(&request.source_reference)
        .bind(&request.resolved_digest)
        .bind(&request.media_type)
        .bind(
            i64::try_from(request.size_bytes)
                .map_err(|_| PlatformImageStoreError::InvalidRequest)?,
        )
        .bind(
            i64::try_from(request.trust_revision)
                .map_err(|_| PlatformImageStoreError::InvalidRequest)?,
        )
        .bind(request.actor_id.as_uuid())
        .bind(request.now.get())
        .execute(&mut *transaction)
        .await
        .map_err(|_| PlatformImageStoreError::Persistence)?;
        if inserted.rows_affected() != 1 {
            return Err(PlatformImageStoreError::Conflict);
        }
        insert_audit(
            &mut transaction,
            catalog_id,
            "registered",
            None,
            Some(&request.resolved_digest),
            1,
            request.actor_id,
            &request.reason,
            request.now,
        )
        .await?;
        let entry = load_entry(&mut transaction, catalog_id).await?;
        transaction
            .commit()
            .await
            .map_err(|_| PlatformImageStoreError::Persistence)?;
        Ok(entry)
    }

    /// Replaces the pinned digest of an active entry.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformImageStoreError::NotFound`] for an unknown entry,
    /// [`PlatformImageStoreError::Conflict`] when the entry is disabled or the observed digest
    /// changed, and [`PlatformImageStoreError::Persistence`] when the durable write fails.
    pub async fn repin(
        &self,
        catalog_id: Uuid,
        request: &RepinPlatformImage,
    ) -> Result<PlatformImageEntry, PlatformImageStoreError> {
        validate_digest(&request.resolved_digest)?;
        validate_digest(&request.expected_digest)?;
        validate_reason(&request.reason)?;
        if request.media_type.trim().is_empty()
            || request.media_type.len() > 255
            || request.size_bytes == 0
        {
            return Err(PlatformImageStoreError::InvalidRequest);
        }
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| PlatformImageStoreError::Persistence)?;
        let current = lock_entry(&mut transaction, catalog_id).await?;
        if current.resolved_digest != request.expected_digest || current.status != "active" {
            return Err(PlatformImageStoreError::Conflict);
        }
        sqlx::query(
            "UPDATE agent.platform_image_catalog \
             SET resolved_digest=$2, media_type=$3, size_bytes=$4, trust_revision=$5, \
                 repin_generation=repin_generation+1, updated_at=$6, pinned_at=$6 \
             WHERE catalog_id=$1",
        )
        .bind(catalog_id)
        .bind(&request.resolved_digest)
        .bind(&request.media_type)
        .bind(
            i64::try_from(request.size_bytes)
                .map_err(|_| PlatformImageStoreError::InvalidRequest)?,
        )
        .bind(
            i64::try_from(request.trust_revision)
                .map_err(|_| PlatformImageStoreError::InvalidRequest)?,
        )
        .bind(request.now.get())
        .execute(&mut *transaction)
        .await
        .map_err(|_| PlatformImageStoreError::Persistence)?;
        insert_audit(
            &mut transaction,
            catalog_id,
            "repinned",
            Some(&request.expected_digest),
            Some(&request.resolved_digest),
            current.repin_generation + 1,
            request.actor_id,
            &request.reason,
            request.now,
        )
        .await?;
        let entry = load_entry(&mut transaction, catalog_id).await?;
        transaction
            .commit()
            .await
            .map_err(|_| PlatformImageStoreError::Persistence)?;
        Ok(entry)
    }

    /// Disables an active entry while keeping it for referenced releases.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformImageStoreError::NotFound`] for an unknown entry,
    /// [`PlatformImageStoreError::Conflict`] when the entry is already disabled or the observed
    /// digest changed, and [`PlatformImageStoreError::Persistence`] when the write fails.
    pub async fn disable(
        &self,
        catalog_id: Uuid,
        request: &DisablePlatformImage,
    ) -> Result<PlatformImageEntry, PlatformImageStoreError> {
        validate_digest(&request.expected_digest)?;
        validate_reason(&request.reason)?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| PlatformImageStoreError::Persistence)?;
        let current = lock_entry(&mut transaction, catalog_id).await?;
        if current.resolved_digest != request.expected_digest || current.status != "active" {
            return Err(PlatformImageStoreError::Conflict);
        }
        sqlx::query(
            "UPDATE agent.platform_image_catalog SET status='disabled', updated_at=$2 \
             WHERE catalog_id=$1",
        )
        .bind(catalog_id)
        .bind(request.now.get())
        .execute(&mut *transaction)
        .await
        .map_err(|_| PlatformImageStoreError::Persistence)?;
        insert_audit(
            &mut transaction,
            catalog_id,
            "disabled",
            Some(&request.expected_digest),
            None,
            current.repin_generation,
            request.actor_id,
            &request.reason,
            request.now,
        )
        .await?;
        let entry = load_entry(&mut transaction, catalog_id).await?;
        transaction
            .commit()
            .await
            .map_err(|_| PlatformImageStoreError::Persistence)?;
        Ok(entry)
    }

    /// Lists active catalog entries for authoring.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformImageStoreError::Persistence`] when the read fails.
    pub async fn active_list(&self) -> Result<Vec<PlatformImageEntry>, PlatformImageStoreError> {
        self.list(Some("active")).await
    }

    /// Lists catalog entries, optionally filtered by status.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformImageStoreError::Persistence`] when the read fails.
    pub async fn list(
        &self,
        status: Option<&str>,
    ) -> Result<Vec<PlatformImageEntry>, PlatformImageStoreError> {
        let rows = sqlx::query(
            "SELECT catalog_id,kind,binding,source_reference,resolved_digest,media_type,size_bytes,\
                    status,trust_revision,repin_generation,pinned_at,updated_at \
             FROM agent.platform_image_catalog \
             WHERE $1::text IS NULL OR status=$1 \
             ORDER BY kind, binding",
        )
        .bind(status)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| PlatformImageStoreError::Persistence)?;
        rows.iter().map(entry_from_row).collect()
    }
}

impl RegisterPlatformImage {
    fn validate(&self) -> Result<(), PlatformImageStoreError> {
        validate_digest(&self.resolved_digest)?;
        validate_reason(&self.reason)?;
        if !valid_binding(&self.binding)
            || self.source_reference.trim().is_empty()
            || self.source_reference.len() > 512
            || self.media_type.trim().is_empty()
            || self.media_type.len() > 255
            || self.size_bytes == 0
            || self.trust_revision == 0
        {
            return Err(PlatformImageStoreError::InvalidRequest);
        }
        Ok(())
    }
}

struct LockedEntry {
    resolved_digest: String,
    status: String,
    repin_generation: i64,
}

async fn lock_entry(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    catalog_id: Uuid,
) -> Result<LockedEntry, PlatformImageStoreError> {
    let row = sqlx::query(
        "SELECT resolved_digest,status,repin_generation FROM agent.platform_image_catalog \
         WHERE catalog_id=$1 FOR UPDATE",
    )
    .bind(catalog_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|_| PlatformImageStoreError::Persistence)?
    .ok_or(PlatformImageStoreError::NotFound)?;
    Ok(LockedEntry {
        resolved_digest: row
            .try_get("resolved_digest")
            .map_err(|_| PlatformImageStoreError::Persistence)?,
        status: row
            .try_get("status")
            .map_err(|_| PlatformImageStoreError::Persistence)?,
        repin_generation: row
            .try_get("repin_generation")
            .map_err(|_| PlatformImageStoreError::Persistence)?,
    })
}

async fn load_entry(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    catalog_id: Uuid,
) -> Result<PlatformImageEntry, PlatformImageStoreError> {
    let row = sqlx::query(
        "SELECT catalog_id,kind,binding,source_reference,resolved_digest,media_type,size_bytes,\
                status,trust_revision,repin_generation,pinned_at,updated_at \
         FROM agent.platform_image_catalog WHERE catalog_id=$1",
    )
    .bind(catalog_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(|_| PlatformImageStoreError::Persistence)?;
    entry_from_row(&row)
}

#[allow(clippy::too_many_arguments)]
async fn insert_audit(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    catalog_id: Uuid,
    action: &str,
    from_digest: Option<&str>,
    to_digest: Option<&str>,
    repin_generation: i64,
    actor_id: ActorId,
    reason: &str,
    now: UtcTimestamp,
) -> Result<(), PlatformImageStoreError> {
    sqlx::query(
        "INSERT INTO agent.platform_image_catalog_audit \
         (audit_id,catalog_id,action,from_digest,to_digest,repin_generation,actor_id,reason,created_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
    )
    .bind(Uuid::new_v4())
    .bind(catalog_id)
    .bind(action)
    .bind(from_digest)
    .bind(to_digest)
    .bind(repin_generation)
    .bind(actor_id.as_uuid())
    .bind(reason)
    .bind(now.get())
    .execute(&mut **transaction)
    .await
    .map_err(|_| PlatformImageStoreError::Persistence)?;
    Ok(())
}

fn entry_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<PlatformImageEntry, PlatformImageStoreError> {
    let kind: String = row
        .try_get("kind")
        .map_err(|_| PlatformImageStoreError::Persistence)?;
    let status: String = row
        .try_get("status")
        .map_err(|_| PlatformImageStoreError::Persistence)?;
    Ok(PlatformImageEntry {
        catalog_id: row
            .try_get("catalog_id")
            .map_err(|_| PlatformImageStoreError::Persistence)?,
        kind: match kind.as_str() {
            "container" => PlatformImageKind::Container,
            "virtual_machine" => PlatformImageKind::VirtualMachine,
            _ => return Err(PlatformImageStoreError::Persistence),
        },
        binding: row
            .try_get("binding")
            .map_err(|_| PlatformImageStoreError::Persistence)?,
        source_reference: row
            .try_get("source_reference")
            .map_err(|_| PlatformImageStoreError::Persistence)?,
        resolved_digest: row
            .try_get("resolved_digest")
            .map_err(|_| PlatformImageStoreError::Persistence)?,
        media_type: row
            .try_get("media_type")
            .map_err(|_| PlatformImageStoreError::Persistence)?,
        size_bytes: u64::try_from(
            row.try_get::<i64, _>("size_bytes")
                .map_err(|_| PlatformImageStoreError::Persistence)?,
        )
        .map_err(|_| PlatformImageStoreError::Persistence)?,
        status: match status.as_str() {
            "active" => PlatformImageStatus::Active,
            "disabled" => PlatformImageStatus::Disabled,
            _ => return Err(PlatformImageStoreError::Persistence),
        },
        trust_revision: u64::try_from(
            row.try_get::<i64, _>("trust_revision")
                .map_err(|_| PlatformImageStoreError::Persistence)?,
        )
        .map_err(|_| PlatformImageStoreError::Persistence)?,
        repin_generation: u64::try_from(
            row.try_get::<i64, _>("repin_generation")
                .map_err(|_| PlatformImageStoreError::Persistence)?,
        )
        .map_err(|_| PlatformImageStoreError::Persistence)?,
        pinned_at: parse_timestamp(
            row.try_get("pinned_at")
                .map_err(|_| PlatformImageStoreError::Persistence)?,
        )?,
        updated_at: parse_timestamp(
            row.try_get("updated_at")
                .map_err(|_| PlatformImageStoreError::Persistence)?,
        )?,
    })
}

fn parse_timestamp(value: time::OffsetDateTime) -> Result<UtcTimestamp, PlatformImageStoreError> {
    UtcTimestamp::from_utc(value).map_err(|_| PlatformImageStoreError::Persistence)
}

fn valid_binding(value: &str) -> bool {
    let mut bytes = value.bytes();
    match bytes.next() {
        Some(byte) if byte.is_ascii_lowercase() || byte.is_ascii_digit() => {}
        _ => return false,
    }
    value.len() <= 128
        && bytes.all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
}

fn validate_digest(value: &str) -> Result<(), PlatformImageStoreError> {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err(PlatformImageStoreError::InvalidRequest);
    };
    if hex.len() == 64
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(PlatformImageStoreError::InvalidRequest)
    }
}

fn validate_reason(value: &str) -> Result<(), PlatformImageStoreError> {
    if value.trim().is_empty() || value.len() > 512 {
        return Err(PlatformImageStoreError::InvalidRequest);
    }
    Ok(())
}
