//! Administrator-curated platform image catalog.
//!
//! The catalog is the single platform-owned list of base images the sandbox may build from.
//! Registrations resolve one tag reference into an immutable digest and persist both; later
//! repins and disables are compare-and-set on the observed digest and append an audit row in
//! the same transaction. Referenced entries are disabled, never deleted.

use std::str::FromStr;

use contracts::supply_chain::VirtualMachineDiskFormat;
use contracts::{ActorId, PlatformImageId, UtcTimestamp};
use reqwest::{Client, Url};
use serde::Deserialize;
use sqlx::{PgPool, Row};
use thiserror::Error;
use uuid::Uuid;

use crate::oci_import::OciImage;
use crate::oci_registry::{
    OciRegistryError, OciRegistryPublisher, RegistryCredentials, ResolvedRegistryImage,
};

pub use contracts::http::{
    InternalPlatformImageDisableRequest, InternalPlatformImageRegistrationRequest,
    InternalPlatformImageRepinRequest, PlatformImageCatalog, PlatformImageEntry, PlatformImageKind,
    PlatformImageStatus,
};

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
    /// Reviewed capacity of the wrapped virtual-machine disk; absent for container images and for
    /// virtual-machine rows that only inventory an already-published containerdisk reference.
    pub capacity_bytes: Option<u64>,
    /// Lowercase hex SHA-256 of the raw virtual-machine disk bytes inside the published archive.
    pub disk_sha256: Option<String>,
    /// Declared virtual-machine disk encoding.
    pub format: Option<VirtualMachineDiskFormat>,
    /// Trust revision the administrator pinned under.
    pub trust_revision: u64,
    /// Authenticated administrator.
    pub actor_id: ActorId,
    /// Human-readable change reason.
    pub reason: String,
    /// Mutation time.
    pub now: UtcTimestamp,
}

/// One deployment-seeded platform image catalog entry.
///
/// The deployment reviews the container base images it copied into the platform registry and
/// seeds them here, so a sandbox build can only choose an identity the deployment already
/// published. A seed resolves its reference once at startup and pins the observed digest under
/// its binding; it never replaces an existing pin and never reactivates a disabled entry.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformImageSeed {
    /// Reviewed kind.
    pub kind: PlatformImageKind,
    /// Stable resolution key the seeded entry is pinned under.
    pub binding: String,
    /// `<registry-host>/<repository>:<tag>` inside the configured platform registry.
    pub source_reference: String,
    /// Trust revision the deployment pinned under.
    pub trust_revision: u64,
}

/// Outcome of one deployment seed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlatformImageSeedOutcome {
    /// The seed resolved and pinned its exact digest.
    Registered,
    /// The binding already exists, so the seed changed nothing.
    Existing,
    /// The seed was rejected and stays absent from the catalog.
    Failed {
        /// Stable diagnostic of the underlying resolution or catalog failure.
        cause: &'static str,
    },
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

impl PlatformImageStoreError {
    /// Stable diagnostic code for API error mapping.
    #[must_use]
    pub fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::InvalidRequest => "LW_PLATFORM_IMAGE_REQUEST_INVALID",
            Self::NotFound => "LW_PLATFORM_IMAGE_NOT_FOUND",
            Self::Conflict => "LW_PLATFORM_IMAGE_STATE_CONFLICT",
            Self::Persistence => "LW_PLATFORM_IMAGE_PERSISTENCE_FAILED",
        }
    }
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
        let catalog_id = Uuid::now_v7();
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| PlatformImageStoreError::Persistence)?;
        let inserted = sqlx::query(
            "INSERT INTO agent.platform_image_catalog \
             (catalog_id,kind,binding,source_reference,resolved_digest,media_type,size_bytes,\
              capacity_bytes,disk_sha256,format,\
              status,trust_revision,created_by,created_at,updated_at,pinned_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,'active',$11,$12,$13,$13,$13) \
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
            request
                .capacity_bytes
                .map(|capacity| {
                    i64::try_from(capacity).map_err(|_| PlatformImageStoreError::InvalidRequest)
                })
                .transpose()?,
        )
        .bind(request.disk_sha256.as_deref())
        .bind(request.format.map(disk_format_str))
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
        catalog_id: PlatformImageId,
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
        let catalog_id = catalog_id.as_uuid();
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
        catalog_id: PlatformImageId,
        request: &DisablePlatformImage,
    ) -> Result<PlatformImageEntry, PlatformImageStoreError> {
        validate_digest(&request.expected_digest)?;
        validate_reason(&request.reason)?;
        let catalog_id = catalog_id.as_uuid();
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
                    capacity_bytes,disk_sha256,format,\
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

/// Reason recorded in the audit row of a deployment-seeded registration.
const SEED_REASON: &str = "deployment platform image seed";

/// Actor recorded for deployment-seeded registrations.
///
/// These pins belong to the deployment, not to an authenticated administrator, so the audit row
/// carries the nil UUID instead of a fabricated actor. Every console session actor is a `UUIDv7`,
/// so a seed is distinguishable from an administrator action in the audit trail.
const SEED_ACTOR: &str = "00000000-0000-0000-0000-000000000000";

/// Resolves and pins every deployment seed the catalog does not already hold.
///
/// Seeds are processed in configuration order, one at a time, and each is resolved exactly once:
/// the resolved digest is registered only when no entry exists for its `(kind, binding)`, so a
/// moved tag, a repinned entry, and a disabled entry all stay untouched, and a rerun registers
/// nothing new. A seed that cannot be validated, resolved, or registered is reported as
/// [`PlatformImageSeedOutcome::Failed`] with the underlying stable diagnostic and leaves the
/// binding absent from the catalog rather than pinning a weaker identity.
pub async fn seed_platform_images(
    catalog: &PgPlatformImageCatalog,
    registry: &PlatformImageRegistry,
    seeds: &[PlatformImageSeed],
    now: UtcTimestamp,
) -> Vec<PlatformImageSeedOutcome> {
    let Ok(actor_id) = ActorId::from_str(SEED_ACTOR) else {
        return fail_all(seeds, &PlatformImageStoreError::Persistence);
    };
    let mut pinned = match catalog.list(None).await {
        Ok(entries) => entries
            .into_iter()
            .map(|entry| (entry.kind, entry.binding))
            .collect::<Vec<_>>(),
        Err(error) => return fail_all(seeds, &error),
    };
    let mut outcomes = Vec::with_capacity(seeds.len());
    for seed in seeds {
        if let Err(error) = validate_registration_identity(
            &seed.binding,
            &seed.source_reference,
            seed.trust_revision,
        ) {
            outcomes.push(PlatformImageSeedOutcome::Failed {
                cause: error.diagnostic_code(),
            });
            continue;
        }
        if is_pinned(&pinned, seed.kind, &seed.binding) {
            outcomes.push(PlatformImageSeedOutcome::Existing);
            continue;
        }
        let resolved = match registry.resolve(&seed.source_reference).await {
            Ok(resolved) => resolved,
            Err(error) => {
                // The outcome only carries the stable code; log the closed error
                // kind too, otherwise an unreachable registry and a wrong
                // credential are indistinguishable in production.
                tracing::error!(
                    event = "agent.platform_image.seed_resolve_failed",
                    binding = %seed.binding,
                    source_reference = %seed.source_reference,
                    error_kind = ?error,
                    diagnostic_code = error.diagnostic_code(),
                    outcome = "failed",
                );
                outcomes.push(PlatformImageSeedOutcome::Failed {
                    cause: error.diagnostic_code(),
                });
                continue;
            }
        };
        let registration = RegisterPlatformImage {
            kind: seed.kind,
            binding: seed.binding.clone(),
            source_reference: seed.source_reference.clone(),
            resolved_digest: resolved.digest,
            media_type: resolved.media_type,
            size_bytes: resolved.size_bytes,
            capacity_bytes: None,
            disk_sha256: None,
            format: None,
            trust_revision: seed.trust_revision,
            actor_id,
            reason: SEED_REASON.to_owned(),
            now,
        };
        match catalog.register(&registration).await {
            Ok(_) => {
                pinned.push((seed.kind, seed.binding.clone()));
                outcomes.push(PlatformImageSeedOutcome::Registered);
            }
            // A concurrent registration of the same binding wins; a seed never overwrites a pin.
            Err(PlatformImageStoreError::Conflict) => {
                outcomes.push(PlatformImageSeedOutcome::Existing);
            }
            Err(error) => outcomes.push(PlatformImageSeedOutcome::Failed {
                cause: error.diagnostic_code(),
            }),
        }
    }
    outcomes
}

fn is_pinned(
    pinned: &[(PlatformImageKind, String)],
    kind: PlatformImageKind,
    binding: &str,
) -> bool {
    pinned
        .iter()
        .any(|(pinned_kind, pinned_binding)| *pinned_kind == kind && pinned_binding == binding)
}

fn fail_all(
    seeds: &[PlatformImageSeed],
    error: &PlatformImageStoreError,
) -> Vec<PlatformImageSeedOutcome> {
    let cause = error.diagnostic_code();
    seeds
        .iter()
        .map(|_| PlatformImageSeedOutcome::Failed { cause })
        .collect()
}

impl RegisterPlatformImage {
    fn validate(&self) -> Result<(), PlatformImageStoreError> {
        validate_registration_identity(&self.binding, &self.source_reference, self.trust_revision)?;
        validate_digest(&self.resolved_digest)?;
        validate_reason(&self.reason)?;
        self.validate_disk_descriptor()?;
        if self.media_type.trim().is_empty() || self.media_type.len() > 255 || self.size_bytes == 0
        {
            return Err(PlatformImageStoreError::InvalidRequest);
        }
        Ok(())
    }

    /// Validates the virtual-machine disk descriptor as a consistent triple.
    ///
    /// A registration either inventories an already-published containerdisk reference (none of
    /// the three) or pins the raw/qcow2 disk the importer wrapped: all three present, the entry a
    /// virtual machine, a positive capacity and the lowercase hex digest of the exact disk bytes.
    /// A partially declared descriptor and a container carrying one are both misdeclared uploads.
    fn validate_disk_descriptor(&self) -> Result<(), PlatformImageStoreError> {
        match (
            self.capacity_bytes,
            self.disk_sha256.as_deref(),
            self.format,
        ) {
            (None, None, None) => Ok(()),
            (Some(capacity), Some(disk_sha256), Some(_)) => {
                if self.kind != PlatformImageKind::VirtualMachine
                    || capacity == 0
                    || !valid_disk_sha256(disk_sha256)
                {
                    return Err(PlatformImageStoreError::InvalidRequest);
                }
                Ok(())
            }
            _ => Err(PlatformImageStoreError::InvalidRequest),
        }
    }
}

/// Validates the reviewed registration identity every catalog write depends on.
///
/// Deployment seeds pass through the same check as an authenticated registration, so a
/// misdeclared seed can never pin a binding an administrator could not.
fn validate_registration_identity(
    binding: &str,
    source_reference: &str,
    trust_revision: u64,
) -> Result<(), PlatformImageStoreError> {
    if !contracts::http::valid_platform_image_binding(binding)
        || source_reference.trim().is_empty()
        || source_reference.len() > 512
        || trust_revision == 0
    {
        return Err(PlatformImageStoreError::InvalidRequest);
    }
    Ok(())
}

/// Returns the persisted lowercase discriminator for one declared disk encoding.
const fn disk_format_str(format: VirtualMachineDiskFormat) -> &'static str {
    match format {
        VirtualMachineDiskFormat::Qcow2 => "qcow2",
        VirtualMachineDiskFormat::Raw => "raw",
    }
}

fn parse_disk_format(value: &str) -> Result<VirtualMachineDiskFormat, PlatformImageStoreError> {
    match value {
        "qcow2" => Ok(VirtualMachineDiskFormat::Qcow2),
        "raw" => Ok(VirtualMachineDiskFormat::Raw),
        _ => Err(PlatformImageStoreError::Persistence),
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
                capacity_bytes,disk_sha256,format,\
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
        catalog_id: PlatformImageId::from_str(
            &row.try_get::<Uuid, _>("catalog_id")
                .map_err(|_| PlatformImageStoreError::Persistence)?
                .to_string(),
        )
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
        capacity_bytes: row
            .try_get::<Option<i64>, _>("capacity_bytes")
            .map_err(|_| PlatformImageStoreError::Persistence)?
            .map(|capacity| {
                u64::try_from(capacity).map_err(|_| PlatformImageStoreError::Persistence)
            })
            .transpose()?,
        disk_sha256: row
            .try_get("disk_sha256")
            .map_err(|_| PlatformImageStoreError::Persistence)?,
        format: row
            .try_get::<Option<String>, _>("format")
            .map_err(|_| PlatformImageStoreError::Persistence)?
            .map(|format| parse_disk_format(&format))
            .transpose()?,
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

fn validate_digest(value: &str) -> Result<(), PlatformImageStoreError> {
    if value.strip_prefix("sha256:").is_some_and(valid_disk_sha256) {
        Ok(())
    } else {
        Err(PlatformImageStoreError::InvalidRequest)
    }
}

/// Whether `value` is a lowercase hex SHA-256 digest body.
fn valid_disk_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_reason(value: &str) -> Result<(), PlatformImageStoreError> {
    if value.trim().is_empty() || value.len() > 512 {
        return Err(PlatformImageStoreError::InvalidRequest);
    }
    Ok(())
}

/// Fail-closed resolution errors for administrator-entered image references.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum PlatformImageRegistryError {
    /// The reference does not name a repository in the configured registry, or its tag is unusable.
    #[error("LW_PLATFORM_IMAGE_REFERENCE_INVALID")]
    InvalidReference,
    /// The registry denied the request, rejected the manifest, or confirmed another digest.
    #[error("LW_PLATFORM_IMAGE_REGISTRY_REJECTED")]
    RegistryRejected,
    /// The registry transport failed or the endpoint is unavailable.
    #[error("LW_PLATFORM_IMAGE_REGISTRY_UNAVAILABLE")]
    RegistryUnavailable,
}

/// Resolves administrator references against the one configured platform registry.
///
/// The resolver is scoped to a single registry host: a reference naming any other host is
/// rejected before a request is made, so the catalog can only pin platform-approved content.
#[derive(Clone)]
pub struct PlatformImageRegistry {
    base: Url,
    host: String,
    client: Client,
    credentials: RegistryCredentials,
}

impl std::fmt::Debug for PlatformImageRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PlatformImageRegistry")
            .field("base", &self.base)
            .field("credentials", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

impl PlatformImageRegistry {
    /// Binds the resolver to the platform registry base URL.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformImageRegistryError::InvalidReference`] unless the base is an HTTPS
    /// origin without path, query or fragment.
    pub fn new(
        base: Url,
        client: Client,
        credentials: RegistryCredentials,
    ) -> Result<Self, PlatformImageRegistryError> {
        if base.scheme() != "https"
            || base.path() != "/"
            || base.query().is_some()
            || base.fragment().is_some()
        {
            return Err(PlatformImageRegistryError::InvalidReference);
        }
        let host = authority(&base).ok_or(PlatformImageRegistryError::InvalidReference)?;
        Ok(Self {
            base,
            host,
            client,
            credentials,
        })
    }

    /// Builds a resolver for local contract tests that does not require HTTPS.
    #[doc(hidden)]
    #[must_use]
    pub fn for_test(base: Url, client: Client, credentials: RegistryCredentials) -> Self {
        let host = authority(&base).unwrap_or_default();
        Self {
            base,
            host,
            client,
            credentials,
        }
    }

    /// Resolves one `registry/repository:tag` reference into its immutable manifest identity.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformImageRegistryError::InvalidReference`] when the reference names another
    /// registry or has no usable tag, and the mapped registry failure otherwise.
    pub async fn resolve(
        &self,
        reference: &str,
    ) -> Result<ResolvedRegistryImage, PlatformImageRegistryError> {
        let (repository, tag) = self.parse_reference(reference)?;
        let publisher = self.publisher(repository)?;
        publisher.resolve_tag(tag).await.map_err(map_registry_error)
    }

    /// Pushes one verified OCI image by digest and tags it as the reviewed reference.
    ///
    /// The immutable identity is the digest: the manifest is pushed by digest, confirmed by
    /// readback and only then tagged, so a tag can never become the runtime identity.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformImageRegistryError::InvalidReference`] when the reference leaves the
    /// configured registry host, and the mapped registry failure otherwise.
    pub async fn publish(
        &self,
        reference: &str,
        image: &OciImage,
    ) -> Result<String, PlatformImageRegistryError> {
        let (repository, tag) = self.parse_reference(reference)?;
        let publisher = self.publisher(repository)?;
        publisher.publish(image).await.map_err(map_registry_error)?;
        publisher.tag(tag, image).await.map_err(map_registry_error)
    }

    fn publisher(
        &self,
        repository: String,
    ) -> Result<OciRegistryPublisher, PlatformImageRegistryError> {
        if self.base.scheme() == "https" {
            OciRegistryPublisher::new(
                self.base.clone(),
                repository,
                self.client.clone(),
                self.credentials.clone(),
            )
            .map_err(|_| PlatformImageRegistryError::InvalidReference)
        } else {
            Ok(OciRegistryPublisher::for_test(
                self.base.clone(),
                repository,
                self.client.clone(),
                self.credentials.clone(),
            ))
        }
    }

    /// Splits one reviewed reference into its repository and tag inside this registry.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformImageRegistryError::InvalidReference`] unless the reference names this
    /// exact registry host with a non-empty repository and tag.
    pub fn parse_reference<'a>(
        &self,
        reference: &'a str,
    ) -> Result<(String, &'a str), PlatformImageRegistryError> {
        let reference = reference.trim();
        if reference.is_empty()
            || reference.len() > 512
            || reference.contains("://")
            || reference.contains('@')
            || reference.bytes().any(|byte| byte.is_ascii_whitespace())
        {
            return Err(PlatformImageRegistryError::InvalidReference);
        }
        let (host, remainder) = reference
            .split_once('/')
            .ok_or(PlatformImageRegistryError::InvalidReference)?;
        if host != self.host
            || remainder.is_empty()
            || remainder.starts_with('/')
            || remainder.ends_with('/')
        {
            return Err(PlatformImageRegistryError::InvalidReference);
        }
        let (repository, tag) = remainder
            .rsplit_once(':')
            .ok_or(PlatformImageRegistryError::InvalidReference)?;
        if repository.is_empty() || repository.contains(':') || tag.is_empty() {
            return Err(PlatformImageRegistryError::InvalidReference);
        }
        Ok((repository.to_owned(), tag))
    }
}

impl PlatformImageRegistryError {
    /// Stable diagnostic code for API error mapping.
    #[must_use]
    pub fn diagnostic_code(self) -> &'static str {
        match self {
            Self::InvalidReference => "LW_PLATFORM_IMAGE_REFERENCE_INVALID",
            Self::RegistryRejected => "LW_PLATFORM_IMAGE_REGISTRY_REJECTED",
            Self::RegistryUnavailable => "LW_PLATFORM_IMAGE_REGISTRY_UNAVAILABLE",
        }
    }
}

fn authority(base: &Url) -> Option<String> {
    let host = base.host_str()?;
    Some(match base.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    })
}

fn map_registry_error(error: OciRegistryError) -> PlatformImageRegistryError {
    match error {
        OciRegistryError::Configuration => PlatformImageRegistryError::InvalidReference,
        OciRegistryError::Unavailable => PlatformImageRegistryError::RegistryUnavailable,
        OciRegistryError::Denied
        | OciRegistryError::Rejected
        | OciRegistryError::DigestMismatch => PlatformImageRegistryError::RegistryRejected,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use reqwest::{Client, Url};

    use super::{
        PlatformImageRegistry, PlatformImageRegistryError, RegistryCredentials, map_registry_error,
    };
    use crate::oci_registry::OciRegistryError;

    fn resolver(base: &str) -> PlatformImageRegistry {
        PlatformImageRegistry::for_test(
            Url::parse(base).expect("base url"),
            Client::builder().no_proxy().build().expect("client"),
            RegistryCredentials {
                username: "robot$platform".to_owned(),
                password: "secret".to_owned(),
            },
        )
    }

    #[test]
    fn new_rejects_non_https_and_path_bearing_bases() {
        let client = Client::builder().no_proxy().build().expect("client");
        let credentials = RegistryCredentials {
            username: "robot$platform".to_owned(),
            password: "secret".to_owned(),
        };
        assert!(matches!(
            PlatformImageRegistry::new(
                Url::parse("http://harbor.internal").expect("url"),
                client.clone(),
                credentials.clone()
            ),
            Err(PlatformImageRegistryError::InvalidReference)
        ));
        assert!(matches!(
            PlatformImageRegistry::new(
                Url::parse("https://harbor.internal/v2").expect("url"),
                client,
                credentials
            ),
            Err(PlatformImageRegistryError::InvalidReference)
        ));
    }

    #[test]
    fn parse_reference_scopes_references_to_the_configured_registry_host() {
        let registry = resolver("https://harbor.internal");
        let (repository, tag) = registry
            .parse_reference("harbor.internal/labweaver-system/ubuntu:24.04")
            .expect("valid reference");
        assert_eq!(repository, "labweaver-system/ubuntu");
        assert_eq!(tag, "24.04");

        for rejected in [
            "quay.io/labweaver-system/ubuntu:24.04",
            "harbor.internal/labweaver-system/ubuntu",
            "harbor.internal/:24.04",
            "harbor.internal/labweaver-system/ubuntu:",
            "https://harbor.internal/labweaver-system/ubuntu:24.04",
            "harbor.internal/labweaver-system/ubuntu:24.04@sha256:deadbeef",
            "harbor.internal/labweaver-system/ ubuntu:24.04",
        ] {
            assert!(
                matches!(
                    registry.parse_reference(rejected),
                    Err(PlatformImageRegistryError::InvalidReference)
                ),
                "{rejected} must be rejected"
            );
        }
    }

    #[test]
    fn registry_failures_map_to_stable_categories() {
        assert_eq!(
            map_registry_error(OciRegistryError::Configuration),
            PlatformImageRegistryError::InvalidReference
        );
        assert_eq!(
            map_registry_error(OciRegistryError::Unavailable),
            PlatformImageRegistryError::RegistryUnavailable
        );
        for rejected in [
            OciRegistryError::Denied,
            OciRegistryError::Rejected,
            OciRegistryError::DigestMismatch,
        ] {
            assert_eq!(
                map_registry_error(rejected),
                PlatformImageRegistryError::RegistryRejected
            );
        }
    }
}
