//! Opt-in live `Harbor` import readback for the administrator platform image catalog.
#![allow(
    clippy::too_many_lines,
    reason = "one live acceptance flow keeps the real object store, registry push, tag readback, catalog audit and both fail-closed paths auditable together"
)]
//!
//! The test runs only when `LW_LIVE_HARBOR=1` is set together with every required variable below;
//! otherwise it prints one skip line and returns. It writes one in-test OCI layout archive to the
//! real immutable object store, drives the very same library call the HTTP handler drives, and
//! proves the real registry push, tag-to-digest closure, catalog pin, audit trail and fail-closed
//! behaviour.
//!
//! ```text
//! LW_LIVE_HARBOR=1 \
//! LW_LIVE_HARBOR_REGISTRY=harbor.lab.lan \
//! LW_LIVE_HARBOR_CA_FILE=/tmp/opencode/harbor-ca.crt \
//! LW_LIVE_HARBOR_USERNAME_FILE=/tmp/opencode/harbor-username \
//! LW_LIVE_HARBOR_PASSWORD_FILE=/tmp/opencode/harbor-password \
//! LW_LIVE_HARBOR_REPOSITORY=labweaver-system/admin-import \
//! LW_LIVE_DATABASE_URL='postgres://<user>:<password>@127.0.0.1:30432/labweaver' \
//! LW_LIVE_OBJECT_STORE_ENDPOINT=https://127.0.0.1:30900/ \
//! LW_LIVE_OBJECT_STORE_BUCKET=labweaver-artifacts \
//! LW_LIVE_OBJECT_STORE_REGION=labweaver-internal-1 \
//! LW_LIVE_OBJECT_STORE_ACCESS_KEY_FILE=/tmp/opencode/minio-access-key \
//! LW_LIVE_OBJECT_STORE_SECRET_KEY_FILE=/tmp/opencode/minio-secret-key \
//! LW_LIVE_OBJECT_STORE_CA_FILE=/tmp/opencode/minio-ca.crt \
//! cargo test -p agent-service --test platform_image_import_live -- --nocapture
//! ```
//!
//! `LW_LIVE_HARBOR_REGISTRY` is the bare authority the reviewed Harbor TLS certificate serves,
//! with a port when the stack is reached through a forward. The target reference is built as
//! `<registry>/<repository>:<tag>` and must stay inside that one registry.
//!
//! Optional: `LW_LIVE_OBJECT_STORE_SESSION_TOKEN_FILE`, `LW_LIVE_OBJECT_STORE_BINDING`
//! (default `problem-package-minio-v1`) and `LW_LIVE_OBJECT_STORE_PREFIX`
//! (default `problem-packages`, the reviewed prefix the Agent object-store binding requires).
//! Every credential is read from the file its variable names; no secret is ever inlined here.

use std::{env, error::Error, fs, path::Path, path::PathBuf, time::Duration};

use agent_service::oci_registry::RegistryCredentials;
use agent_service::platform_image_import::{PlatformImageImportError, import_platform_image};
use agent_service::platform_images::{
    DisablePlatformImage, PgPlatformImageCatalog, PlatformImageRegistry,
    PlatformImageRegistryError, PlatformImageStatus, PlatformImageStoreError,
};
use artifact_store::{S3Credential, S3ImmutableObjectStore, S3StoreConfig};
use contracts::http::{
    InternalPlatformImageImportRequest, PLATFORM_IMAGE_ARCHIVE_MEDIA_TYPE, PlatformImageKind,
};
use contracts::{ActorId, ArtifactRef, UtcTimestamp};
use persistence_sqlx::Sha256Digest;
use reqwest::{Certificate, Client, Url};
use sqlx::postgres::PgPoolOptions;
use time::OffsetDateTime;
use uuid::Uuid;

/// Reviewed manifest media type of one imported single-image layout.
const MANIFEST_MEDIA_TYPE: &str = "application/vnd.oci.image.manifest.v1+json";
/// Reviewed config media type of one imported image.
const CONFIG_MEDIA_TYPE: &str = "application/vnd.oci.image.config.v1+json";
/// Reviewed layer media type of one imported image.
const LAYER_MEDIA_TYPE: &str = "application/vnd.oci.image.layer.v1.tar";
/// Reviewed object-store binding recorded in the staged archive reference.
const DEFAULT_OBJECT_STORE_BINDING: &str = "problem-package-minio-v1";
/// Reviewed object-store prefix the Agent object-store binding accepts keys under.
const DEFAULT_OBJECT_STORE_PREFIX: &str = "problem-packages";
/// Reviewed maximum accepted object size of the Agent object-store binding.
const OBJECT_STORE_MAX_OBJECT_BYTES: u64 = 64 * 1024 * 1024;
/// Reviewed presigned upload lifetime; the live test never presigns but must stay in bounds.
const OBJECT_STORE_UPLOAD_TTL_SECONDS: u64 = 900;
/// Bounded registry request timeout, matching the deployed platform registry client.
const REGISTRY_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Cross-registry reference used to prove the fail-closed reference boundary.
const FOREIGN_REGISTRY: &str = "registry.invalid";

/// Live registry, database, object store and credential inputs.
struct LiveEnvironment {
    registry: String,
    repository: String,
    harbor_ca_file: PathBuf,
    harbor_username_file: PathBuf,
    harbor_password_file: PathBuf,
    database_url: String,
    object_store_endpoint: Url,
    object_store_bucket: String,
    object_store_region: String,
    object_store_binding: String,
    object_store_prefix: String,
    object_store_ca_file: PathBuf,
    object_store_access_key_file: PathBuf,
    object_store_secret_key_file: PathBuf,
    object_store_session_token_file: Option<PathBuf>,
}

/// Verified OCI layout archive plus the identity the importer must derive from it.
struct LayoutArchive {
    bytes: Vec<u8>,
    manifest_digest: String,
    manifest_media_type: String,
    size_bytes: u64,
}

/// Reads one required live variable.
fn required(name: &str) -> Result<String, Box<dyn Error>> {
    env::var(name)
        .map_err(|_| format!("{name} must be set while LW_LIVE_HARBOR=1").into())
        .and_then(|value| {
            if value.trim().is_empty() {
                Err(format!("{name} must not be empty while LW_LIVE_HARBOR=1").into())
            } else {
                Ok(value)
            }
        })
}

/// Reads one optional live variable, treating an empty value as absent.
fn optional(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.trim().is_empty())
}

/// Reads one required credential file without ever echoing its content.
fn read_secret(path: &Path) -> Result<String, Box<dyn Error>> {
    let value = fs::read_to_string(path)?.trim().to_owned();
    if value.is_empty() {
        return Err(format!("credential file {} must not be empty", path.display()).into());
    }
    Ok(value)
}

/// Reads the live switch and every bound variable, or reports the test as skipped.
fn live_environment() -> Result<Option<LiveEnvironment>, Box<dyn Error>> {
    if env::var("LW_LIVE_HARBOR").ok().as_deref() != Some("1") {
        return Ok(None);
    }
    Ok(Some(LiveEnvironment {
        registry: required("LW_LIVE_HARBOR_REGISTRY")?,
        repository: required("LW_LIVE_HARBOR_REPOSITORY")?,
        harbor_ca_file: PathBuf::from(required("LW_LIVE_HARBOR_CA_FILE")?),
        harbor_username_file: PathBuf::from(required("LW_LIVE_HARBOR_USERNAME_FILE")?),
        harbor_password_file: PathBuf::from(required("LW_LIVE_HARBOR_PASSWORD_FILE")?),
        database_url: required("LW_LIVE_DATABASE_URL")?,
        object_store_endpoint: Url::parse(&required("LW_LIVE_OBJECT_STORE_ENDPOINT")?)?,
        object_store_bucket: required("LW_LIVE_OBJECT_STORE_BUCKET")?,
        object_store_region: required("LW_LIVE_OBJECT_STORE_REGION")?,
        object_store_binding: optional("LW_LIVE_OBJECT_STORE_BINDING")
            .unwrap_or_else(|| DEFAULT_OBJECT_STORE_BINDING.to_owned()),
        object_store_prefix: optional("LW_LIVE_OBJECT_STORE_PREFIX")
            .unwrap_or_else(|| DEFAULT_OBJECT_STORE_PREFIX.to_owned()),
        object_store_ca_file: PathBuf::from(required("LW_LIVE_OBJECT_STORE_CA_FILE")?),
        object_store_access_key_file: PathBuf::from(required(
            "LW_LIVE_OBJECT_STORE_ACCESS_KEY_FILE",
        )?),
        object_store_secret_key_file: PathBuf::from(required(
            "LW_LIVE_OBJECT_STORE_SECRET_KEY_FILE",
        )?),
        object_store_session_token_file: optional("LW_LIVE_OBJECT_STORE_SESSION_TOKEN_FILE")
            .map(PathBuf::from),
    }))
}

/// Builds the object-store binding the Agent uses for staged archives.
fn object_store_config(environment: &LiveEnvironment) -> S3StoreConfig {
    S3StoreConfig {
        binding: environment.object_store_binding.clone(),
        endpoint: environment.object_store_endpoint.clone(),
        bucket: environment.object_store_bucket.clone(),
        region: environment.object_store_region.clone(),
        object_prefix: environment.object_store_prefix.clone(),
        upload_ttl_seconds: OBJECT_STORE_UPLOAD_TTL_SECONDS,
        max_object_bytes: OBJECT_STORE_MAX_OBJECT_BYTES,
        force_path_style: true,
        ca_bundle_file: Some(environment.object_store_ca_file.display().to_string()),
    }
}

/// Builds the resolver bound to the one configured platform registry.
fn platform_registry(
    environment: &LiveEnvironment,
) -> Result<PlatformImageRegistry, Box<dyn Error>> {
    let base = Url::parse(&format!("https://{}", environment.registry))?;
    let ca = Certificate::from_pem(&fs::read(&environment.harbor_ca_file)?)?;
    let client = Client::builder()
        .https_only(true)
        .timeout(REGISTRY_REQUEST_TIMEOUT)
        .add_root_certificate(ca)
        .build()?;
    Ok(PlatformImageRegistry::new(
        base,
        client,
        RegistryCredentials {
            username: read_secret(&environment.harbor_username_file)?,
            password: read_secret(&environment.harbor_password_file)?,
        },
    )?)
}

/// Returns the exact UTC millisecond timestamp the catalog mutation is recorded with.
fn now() -> Result<UtcTimestamp, Box<dyn Error>> {
    let value = OffsetDateTime::now_utc();
    let value = value.replace_nanosecond((value.nanosecond() / 1_000_000) * 1_000_000)?;
    Ok(UtcTimestamp::from_utc(value)?)
}

/// Returns the content-addressed digest of one blob.
fn digest_of(bytes: &[u8]) -> String {
    format!("sha256:{}", Sha256Digest::of_bytes(bytes))
}

/// Returns the layout entry path of one content-addressed blob.
fn blob_path(digest: &str) -> String {
    format!("blobs/sha256/{}", digest.trim_start_matches("sha256:"))
}

/// Appends one read-only entry to an OCI layout archive.
fn append_entry(
    builder: &mut tar::Builder<Vec<u8>>,
    name: &str,
    bytes: &[u8],
) -> Result<(), Box<dyn Error>> {
    let mut header = tar::Header::new_gnu();
    header.set_size(u64::try_from(bytes.len())?);
    header.set_mode(0o444);
    header.set_cksum();
    builder.append_data(&mut header, name, bytes)?;
    Ok(())
}

/// Builds one single-image OCI layout archive whose every digest matches its bytes.
fn oci_layout_archive(config: &[u8], layer: &[u8]) -> Result<LayoutArchive, Box<dyn Error>> {
    let config_digest = digest_of(config);
    let layer_digest = digest_of(layer);
    let manifest = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": MANIFEST_MEDIA_TYPE,
        "config": {
            "mediaType": CONFIG_MEDIA_TYPE,
            "digest": config_digest,
            "size": config.len(),
        },
        "layers": [{
            "mediaType": LAYER_MEDIA_TYPE,
            "digest": layer_digest,
            "size": layer.len(),
        }],
    });
    let manifest_bytes = serde_json::to_vec(&manifest)?;
    let manifest_digest = digest_of(&manifest_bytes);
    let index = serde_json::json!({
        "schemaVersion": 2,
        "manifests": [{
            "mediaType": MANIFEST_MEDIA_TYPE,
            "digest": manifest_digest,
            "size": manifest_bytes.len(),
        }],
    });
    let mut builder = tar::Builder::new(Vec::new());
    append_entry(
        &mut builder,
        "oci-layout",
        br#"{"imageLayoutVersion":"1.0.0"}"#,
    )?;
    append_entry(&mut builder, "index.json", &serde_json::to_vec(&index)?)?;
    append_entry(&mut builder, &blob_path(&config_digest), config)?;
    append_entry(&mut builder, &blob_path(&layer_digest), layer)?;
    append_entry(&mut builder, &blob_path(&manifest_digest), &manifest_bytes)?;
    let bytes = builder.into_inner()?;
    Ok(LayoutArchive {
        size_bytes: u64::try_from(config.len() + layer.len())?,
        manifest_media_type: MANIFEST_MEDIA_TYPE.to_owned(),
        manifest_digest,
        bytes,
    })
}

/// Builds one import request for a frozen archive object.
fn import_request(
    binding: &str,
    target_reference: &str,
    archive_object_key: &str,
    archive: &ArtifactRef,
) -> InternalPlatformImageImportRequest {
    InternalPlatformImageImportRequest {
        kind: PlatformImageKind::Container,
        binding: binding.to_owned(),
        target_reference: target_reference.to_owned(),
        archive: archive.clone(),
        archive_object_key: archive_object_key.to_owned(),
        disk_format: None,
        disk_path: None,
        capacity_bytes: None,
        trust_revision: 1,
        actor_id: ActorId::new(),
        reason: format!("live archive import for {binding}"),
    }
}

#[tokio::test]
async fn live_import_publishes_tags_pins_and_audits() -> Result<(), Box<dyn Error>> {
    let Some(environment) = live_environment()? else {
        eprintln!("LW_LIVE_HARBOR is not enabled; skipping live platform image import");
        return Ok(());
    };
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&environment.database_url)
        .await?;
    let schema: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('agent.platform_image_catalog')::text")
            .fetch_one(&pool)
            .await?;
    let Some(schema) = schema else {
        return Err(
            "the live database has no agent.platform_image_catalog; apply the agent migrations first"
                .into(),
        );
    };
    println!("live schema readback: {schema} is present");
    let catalog = PgPlatformImageCatalog::new(pool.clone());
    let store = S3ImmutableObjectStore::new(
        object_store_config(&environment),
        S3Credential {
            access_key_id: read_secret(&environment.object_store_access_key_file)?,
            secret_access_key: read_secret(&environment.object_store_secret_key_file)?,
            session_token: environment
                .object_store_session_token_file
                .as_deref()
                .map(read_secret)
                .transpose()?,
        },
    )
    .await?;
    let registry = platform_registry(&environment)?;
    let archive = oci_layout_archive(
        b"{\"architecture\":\"amd64\",\"os\":\"linux\",\"variant\":\"live\"}",
        b"labweaver-live-layer",
    )?;
    let run = Uuid::now_v7().simple().to_string();
    let archive_object_key = format!(
        "{}/live-import/{run}.tar",
        environment.object_store_prefix.trim_matches('/')
    );
    let binding = format!("live-import-{run}");
    let target_reference = format!(
        "{}/{repository}:live-{run}",
        environment.registry,
        repository = environment.repository
    );
    let frozen = store
        .put_versioned_immutable(
            &archive_object_key,
            &archive.bytes,
            PLATFORM_IMAGE_ARCHIVE_MEDIA_TYPE,
        )
        .await?;
    assert_eq!(
        frozen.reference.size_bytes,
        u64::try_from(archive.bytes.len())?,
        "the frozen archive version must report the exact staged size"
    );
    assert_eq!(
        frozen.reference.media_type, PLATFORM_IMAGE_ARCHIVE_MEDIA_TYPE,
        "the frozen archive version must keep the reviewed archive media type"
    );
    println!(
        "live object store readback: key={archive_object_key} version={} size={}",
        frozen.reference.object_version, frozen.reference.size_bytes
    );

    let request = import_request(
        &binding,
        &target_reference,
        &archive_object_key,
        &frozen.reference,
    );
    let entry = import_platform_image(&registry, &catalog, &store, &request, now()?).await?;
    assert_eq!(
        entry.resolved_digest, archive.manifest_digest,
        "the imported pin must be the archive manifest digest"
    );
    assert_eq!(
        entry.media_type, archive.manifest_media_type,
        "the imported pin must keep the archive manifest media type"
    );
    assert_eq!(
        entry.binding, binding,
        "the pin must keep the requested binding"
    );
    assert_eq!(
        entry.size_bytes, archive.size_bytes,
        "the pin must record config plus layers"
    );
    println!(
        "live import readback: catalogId={} digest={} mediaType={}",
        entry.catalog_id, entry.resolved_digest, entry.media_type
    );

    let resolved = registry.resolve(&target_reference).await?;
    assert_eq!(
        resolved.digest, archive.manifest_digest,
        "the tagged reference must resolve back to the imported digest"
    );
    assert_eq!(
        resolved.size_bytes, archive.size_bytes,
        "the registry must report the same reviewed content size"
    );
    println!(
        "live registry tag readback: {target_reference} resolves to {}",
        resolved.digest
    );

    let active = catalog.list(Some("active")).await?;
    let row = active
        .iter()
        .find(|entry| entry.binding == binding)
        .ok_or("the imported pin is absent from the active catalog")?;
    assert_eq!(
        row.status,
        PlatformImageStatus::Active,
        "an import must pin an active entry"
    );
    assert_eq!(
        row.repin_generation, 1,
        "a first import must start at generation one"
    );
    assert_eq!(row.resolved_digest, archive.manifest_digest);
    let disabled = catalog
        .disable(
            row.catalog_id,
            &DisablePlatformImage {
                expected_digest: row.resolved_digest.clone(),
                actor_id: ActorId::new(),
                reason: "live disable readback".to_owned(),
                now: now()?,
            },
        )
        .await?;
    assert_eq!(
        disabled.status,
        PlatformImageStatus::Disabled,
        "disable must keep the entry and report the disabled status"
    );
    let disabled_entries = catalog.list(Some("disabled")).await?;
    assert!(
        disabled_entries
            .iter()
            .any(|entry| entry.binding == binding),
        "the disabled entry must be readable from the disabled catalog listing"
    );
    let mut audit: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM agent.platform_image_catalog_audit \
         WHERE catalog_id=$1 ORDER BY created_at, audit_id",
    )
    .bind(row.catalog_id.as_uuid())
    .fetch_all(&pool)
    .await?;
    // The two audit rows may share one millisecond, so the expected multiset is asserted.
    audit.sort();
    assert_eq!(
        audit,
        vec!["disabled".to_owned(), "registered".to_owned()],
        "the catalog audit must record the registration and the disable"
    );
    println!("live catalog audit readback: {audit:?}");

    let cross_request = import_request(
        &format!("live-cross-{run}"),
        &format!(
            "{FOREIGN_REGISTRY}/{repository}:live-{run}",
            repository = environment.repository
        ),
        &archive_object_key,
        &frozen.reference,
    );
    let cross_error = import_platform_image(&registry, &catalog, &store, &cross_request, now()?)
        .await
        .err()
        .ok_or("a reference outside the configured registry must be rejected")?;
    assert!(
        matches!(
            cross_error,
            PlatformImageImportError::Registry(PlatformImageRegistryError::InvalidReference)
        ),
        "a foreign registry must fail closed as an invalid reference, got: {cross_error}"
    );
    println!("live cross-host rejection readback: {cross_error}");

    let conflict = import_platform_image(&registry, &catalog, &store, &request, now()?)
        .await
        .err()
        .ok_or("a second import of the same binding must conflict")?;
    assert!(
        matches!(
            conflict,
            PlatformImageImportError::Catalog(PlatformImageStoreError::Conflict)
        ),
        "a duplicate binding must fail closed as a catalog conflict, got: {conflict}"
    );
    println!("live duplicate import readback: {conflict}");
    Ok(())
}
