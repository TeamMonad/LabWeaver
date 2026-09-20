//! Administrator OCI layout archive import into the platform image catalog.
//!
//! The archive is staged by Control and read back from the immutable object store by exact
//! version. This module verifies every entry and blob digest before pushing the exact manifest
//! by digest, tags the reviewed reference, and registers the catalog pin. The registry push
//! authority stays in the Agent; Control never holds registry credentials.

use artifact_store::{ImmutableObjectStore, ObjectStoreError};
use contracts::UtcTimestamp;
use contracts::http::{InternalPlatformImageImportRequest, PlatformImageEntry};
use thiserror::Error;

use crate::oci_import::{OciImportError, parse_oci_layout};
use crate::platform_images::{
    PgPlatformImageCatalog, PlatformImageRegistry, PlatformImageRegistryError,
    PlatformImageStoreError, RegisterPlatformImage,
};

/// Administrator archive import failure.
#[derive(Debug, Error)]
pub enum PlatformImageImportError {
    /// The staged archive could not be read back from the immutable object store.
    #[error("LW_AGENT_PERSISTENCE_FAILED")]
    ObjectStore(#[from] ObjectStoreError),
    /// The archive is not a verified single-image OCI layout.
    #[error(transparent)]
    Layout(#[from] OciImportError),
    /// The registry refused the push or the reviewed reference.
    #[error(transparent)]
    Registry(#[from] PlatformImageRegistryError),
    /// The catalog refused the registration.
    #[error(transparent)]
    Catalog(#[from] PlatformImageStoreError),
}

/// Imports one frozen OCI layout archive into the configured registry and catalog.
///
/// The push happens before the catalog write, so a registry failure never leaves a pinned
/// digest that the registry cannot resolve, and the catalog write is the only step that makes
/// the binding visible to authoring.
///
/// # Errors
///
/// Fails closed on an unreadable or mismatched archive object, a malformed OCI layout, a registry
/// rejection, or a conflicting catalog binding.
pub async fn import_platform_image(
    registry: &PlatformImageRegistry,
    catalog: &PgPlatformImageCatalog,
    objects: &dyn ImmutableObjectStore,
    request: &InternalPlatformImageImportRequest,
    now: UtcTimestamp,
) -> Result<PlatformImageEntry, PlatformImageImportError> {
    let object = objects
        .read_verified(&request.archive_object_key, &request.archive)
        .await?;
    let image = parse_oci_layout(&object.bytes)?;
    let digest = registry.publish(&request.target_reference, &image).await?;
    let size_bytes = image
        .blobs
        .iter()
        .map(|blob| u64::try_from(blob.bytes.len()).unwrap_or(u64::MAX))
        .sum();
    let entry = catalog
        .register(&RegisterPlatformImage {
            kind: request.kind,
            binding: request.binding.clone(),
            source_reference: request.target_reference.clone(),
            resolved_digest: digest,
            media_type: image.manifest_media_type.clone(),
            size_bytes,
            trust_revision: request.trust_revision,
            actor_id: request.actor_id,
            reason: request.reason.clone(),
            now,
        })
        .await?;
    Ok(entry)
}
