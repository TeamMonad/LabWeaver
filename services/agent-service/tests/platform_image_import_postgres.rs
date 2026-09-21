//! Real `PostgreSQL` proof for the administrator OCI layout archive import.
#![allow(
    clippy::expect_used,
    reason = "the fixtures assert on archives they build themselves"
)]

use std::io::Read;
use std::sync::Arc;

use agent_service::api::{AgentApiState, CONTROL_PERMISSION, router};
use agent_service::build_store::PgBuildStore;
use agent_service::generated_artifacts::GeneratedArtifactStore;
use agent_service::llm_review::LlmReviewStore;
use agent_service::oci_registry::RegistryCredentials;
use agent_service::platform_images::{PgPlatformImageCatalog, PlatformImageRegistry};
use agent_service::run_store::PostgresAgentRunStore;
use contracts::{ActorId, ArtifactId};
use persistence_sqlx::Sha256Digest;
use reqwest::StatusCode;
use sqlx::postgres::PgPoolOptions;
use testcontainers::{ContainerAsync, ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;

mod support;
use support::{FakeObjects, FakeRegistry, apply_agent_migrations};

const MANIFEST_MEDIA_TYPE: &str = "application/vnd.oci.image.manifest.v1+json";

/// Verified OCI layout archive plus the identity the importer must derive from it.
struct LayoutArchive {
    bytes: Vec<u8>,
    manifest_digest: String,
    manifest_media_type: String,
    size_bytes: u64,
}

fn digest_of(bytes: &[u8]) -> String {
    format!("sha256:{}", Sha256Digest::of_bytes(bytes))
}

fn append(builder: &mut tar::Builder<Vec<u8>>, name: &str, bytes: &[u8]) {
    let mut header = tar::Header::new_gnu();
    header.set_size(u64::try_from(bytes.len()).expect("entry size"));
    header.set_mode(0o444);
    header.set_cksum();
    builder
        .append_data(&mut header, name, bytes)
        .expect("append entry");
}

/// Builds one single-image OCI layout archive.
///
/// `layer_digest` overrides the layer identity the manifest and the blob entry name declare, so a
/// test can stage an archive whose stored bytes no longer hash to what it claims.
fn oci_layout_archive(config: &[u8], layer: &[u8], layer_digest: Option<&str>) -> LayoutArchive {
    let config_digest = digest_of(config);
    let declared_layer_digest = layer_digest.map_or_else(|| digest_of(layer), str::to_owned);
    let manifest = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": MANIFEST_MEDIA_TYPE,
        "config": {
            "mediaType": "application/vnd.oci.image.config.v1+json",
            "digest": config_digest,
            "size": config.len(),
        },
        "layers": [{
            "mediaType": "application/vnd.oci.image.layer.v1.tar",
            "digest": declared_layer_digest,
            "size": layer.len(),
        }],
    });
    let manifest_bytes = serde_json::to_vec(&manifest).expect("manifest json");
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
    append(
        &mut builder,
        "oci-layout",
        br#"{"imageLayoutVersion":"1.0.0"}"#,
    );
    append(
        &mut builder,
        "index.json",
        &serde_json::to_vec(&index).expect("index json"),
    );
    append(
        &mut builder,
        &format!("blobs/sha256/{}", &config_digest["sha256:".len()..]),
        config,
    );
    append(
        &mut builder,
        &format!("blobs/sha256/{}", &declared_layer_digest["sha256:".len()..]),
        layer,
    );
    append(
        &mut builder,
        &format!("blobs/sha256/{}", &manifest_digest["sha256:".len()..]),
        &manifest_bytes,
    );
    let bytes = builder.into_inner().expect("finish archive");
    LayoutArchive {
        size_bytes: u64::try_from(config.len() + layer.len()).expect("archive content size"),
        manifest_media_type: MANIFEST_MEDIA_TYPE.to_owned(),
        manifest_digest,
        bytes,
    }
}

fn import_request(binding: &str, target_reference: &str, archive_bytes: u64) -> serde_json::Value {
    serde_json::json!({
        "kind": "container",
        "binding": binding,
        "targetReference": target_reference,
        "archive": {
            "artifactId": ArtifactId::new(),
            "storeBinding": "test-object-store",
            "objectVersion": "staged-version-1",
            "sizeBytes": archive_bytes,
            "mediaType": contracts::http::PLATFORM_IMAGE_ARCHIVE_MEDIA_TYPE,
        },
        "archiveObjectKey": "problem-packages/platform-image-uploads/staged.tar",
        "trustRevision": 1,
        "actorId": ActorId::new(),
        "reason": "reviewed archive import",
    })
}

/// Builds one tar archive holding a single virtual-machine disk entry.
///
/// The archive is the reviewed upload shape: exactly one regular file, named as `disk_path`,
/// holding the raw disk bytes.
fn disk_archive(disk: &[u8], disk_path: &str) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    append(&mut builder, disk_path, disk);
    builder.into_inner().expect("finish disk archive")
}

/// Builds one administrator request declaring the raw disk inside the archive.
fn vm_import_request(
    binding: &str,
    target_reference: &str,
    archive_bytes: u64,
    disk_format: &str,
    disk_path: &str,
    capacity_bytes: u64,
) -> serde_json::Value {
    let mut request = import_request(binding, target_reference, archive_bytes);
    request["kind"] = serde_json::Value::String("virtual_machine".to_owned());
    request["diskFormat"] = serde_json::Value::String(disk_format.to_owned());
    request["diskPath"] = serde_json::Value::String(disk_path.to_owned());
    request["capacityBytes"] = serde_json::json!(capacity_bytes);
    request
}

fn digest_hex(bytes: &[u8]) -> String {
    Sha256Digest::of_bytes(bytes).to_string()
}

/// Returns the one blob the registry holds as a disk tar holding `disk_path`.
///
/// The published layer is located by content rather than by recomputing the wrapper here, so the
/// assertion reads what a CDI pull would actually extract.
fn published_disk_entry(registry: &FakeRegistry, disk_path: &str) -> Option<Vec<u8>> {
    let mut found: Option<Vec<u8>> = None;
    for digest in registry.blob_digests() {
        let Some(bytes) = registry.blob(&digest) else {
            continue;
        };
        let mut archive = tar::Archive::new(bytes.as_slice());
        let Ok(entries) = archive.entries() else {
            continue;
        };
        let mut names = Vec::new();
        for entry in entries {
            let Ok(mut entry) = entry else {
                continue;
            };
            if !entry.header().entry_type().is_file() {
                continue;
            }
            let Ok(path) = entry.path().map(|path| path.to_string_lossy().into_owned()) else {
                continue;
            };
            let mut content = Vec::new();
            if entry.read_to_end(&mut content).is_err() {
                continue;
            }
            names.push((path, content));
        }
        if names.len() == 1 && names[0].0 == disk_path {
            let (_, content) = names.swap_remove(0);
            if found.replace(content).is_some() {
                return None;
            }
        }
    }
    found
}

async fn inject_control_identity(
    mut request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    request.extensions_mut().insert(auth::ServiceIdentity {
        issuer: "https://issuer.test".to_owned(),
        subject: "control".to_owned(),
        client_id: "labweaver-control".to_owned(),
        expires_at: time::OffsetDateTime::now_utc() + time::Duration::hours(1),
        permissions: std::collections::BTreeSet::from([CONTROL_PERMISSION.to_owned()]),
    });
    next.run(request).await
}

fn platform_registry(base: &str) -> Result<PlatformImageRegistry, Box<dyn std::error::Error>> {
    Ok(PlatformImageRegistry::for_test(
        reqwest::Url::parse(base)?,
        reqwest::Client::builder().no_proxy().build()?,
        RegistryCredentials {
            username: "robot$platform".to_owned(),
            password: "secret".to_owned(),
        },
    ))
}

async fn spawn_api(
    pool: sqlx::PgPool,
    registry: Option<PlatformImageRegistry>,
    staged: Vec<u8>,
) -> Result<String, Box<dyn std::error::Error>> {
    let state = Arc::new(AgentApiState {
        store: PostgresAgentRunStore::new(pool.clone()),
        build_store: PgBuildStore::new(pool.clone()),
        generated_artifacts: GeneratedArtifactStore::new(pool.clone()),
        llm_reviews: LlmReviewStore::new(pool.clone()),
        platform_images: PgPlatformImageCatalog::new(pool),
        platform_registry: registry,
        objects: Arc::new(FakeObjects::new(staged)),
    });
    let router = router(state).layer(axum::middleware::from_fn(inject_control_identity));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok(format!("http://{address}"))
}

async fn postgres() -> Result<(sqlx::PgPool, ContainerAsync<Postgres>), Box<dyn std::error::Error>>
{
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let url = format!(
        "postgres://postgres:postgres@127.0.0.1:{}/postgres",
        container.get_host_port_ipv4(5432).await?
    );
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await?;
    apply_agent_migrations(&pool).await?;
    Ok((pool, container))
}

#[tokio::test]
async fn import_publishes_every_blob_tags_the_reference_and_pins_the_digest()
-> Result<(), Box<dyn std::error::Error>> {
    let (pool, _container) = postgres().await?;
    let (registry, base) = FakeRegistry::spawn().await?;
    let host = FakeRegistry::authority(&base);
    let target = format!("{host}/labweaver-system/admin-import:24.04");
    let config = br#"{"architecture":"amd64","os":"linux"}"#.to_vec();
    let layer = b"labweaver-platform-image-layer-payload".to_vec();
    let archive = oci_layout_archive(&config, &layer, None);
    let api = spawn_api(
        pool.clone(),
        Some(platform_registry(&base)?),
        archive.bytes.clone(),
    )
    .await?;
    let client = reqwest::Client::new();

    let imported = client
        .post(format!("{api}/internal/v1/platform-images/imports"))
        .json(&import_request(
            "ubuntu-24.04",
            &target,
            u64::try_from(archive.bytes.len())?,
        ))
        .send()
        .await?;
    assert_eq!(imported.status(), StatusCode::CREATED);
    let imported: serde_json::Value = imported.json().await?;
    assert_eq!(imported["resolvedDigest"], archive.manifest_digest);
    assert_eq!(imported["mediaType"], archive.manifest_media_type);
    assert_eq!(imported["sizeBytes"], archive.size_bytes);
    assert_eq!(imported["sourceReference"], target);
    assert_eq!(imported["status"], "active");
    assert_eq!(imported["repinGeneration"], 1);
    assert_eq!(imported["trustRevision"], 1);

    let mut expected_blobs = vec![digest_of(&config), digest_of(&layer)];
    expected_blobs.sort();
    assert_eq!(registry.blob_digests(), expected_blobs);
    assert_eq!(
        registry.manifest_digest("24.04"),
        Some(archive.manifest_digest.clone())
    );

    let duplicate = client
        .post(format!("{api}/internal/v1/platform-images/imports"))
        .json(&import_request(
            "ubuntu-24.04",
            &target,
            u64::try_from(archive.bytes.len())?,
        ))
        .send()
        .await?;
    assert_eq!(duplicate.status(), StatusCode::CONFLICT);
    let duplicate: serde_json::Value = duplicate.json().await?;
    assert_eq!(
        duplicate["diagnosticCode"],
        "LW_PLATFORM_IMAGE_STATE_CONFLICT"
    );

    let listed: serde_json::Value = client
        .get(format!("{api}/internal/v1/platform-images"))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(listed["entries"].as_array().expect("entries").len(), 1);
    Ok(())
}

#[tokio::test]
async fn import_fails_closed_before_publishing_or_pinning() -> Result<(), Box<dyn std::error::Error>>
{
    let (pool, _container) = postgres().await?;
    let (registry, base) = FakeRegistry::spawn().await?;
    let host = FakeRegistry::authority(&base);
    let client = reqwest::Client::new();

    let config = br#"{"architecture":"amd64","os":"linux"}"#.to_vec();
    let layer = b"labweaver-platform-image-layer-payload".to_vec();
    let corrupted =
        oci_layout_archive(&config, &layer, Some(&format!("sha256:{}", "a".repeat(64))));
    let api = spawn_api(
        pool.clone(),
        Some(platform_registry(&base)?),
        corrupted.bytes.clone(),
    )
    .await?;
    let mismatched = client
        .post(format!("{api}/internal/v1/platform-images/imports"))
        .json(&import_request(
            "corrupted",
            &format!("{host}/labweaver-system/admin-import:24.04"),
            u64::try_from(corrupted.bytes.len())?,
        ))
        .send()
        .await?;
    assert_eq!(mismatched.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let mismatched: serde_json::Value = mismatched.json().await?;
    assert_eq!(mismatched["diagnosticCode"], "LW_AGENT_OCI_BLOB_MISMATCH");

    let api = spawn_api(
        pool.clone(),
        Some(platform_registry(&base)?),
        b"not an OCI layout archive".to_vec(),
    )
    .await?;
    let malformed = client
        .post(format!("{api}/internal/v1/platform-images/imports"))
        .json(&import_request(
            "malformed",
            &format!("{host}/labweaver-system/admin-import:24.04"),
            25,
        ))
        .send()
        .await?;
    assert_eq!(malformed.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let malformed: serde_json::Value = malformed.json().await?;
    assert_eq!(malformed["diagnosticCode"], "LW_AGENT_OCI_LAYOUT_INVALID");

    let trusted = oci_layout_archive(&config, &layer, None);
    let api = spawn_api(
        pool.clone(),
        Some(platform_registry(&base)?),
        trusted.bytes.clone(),
    )
    .await?;
    let foreign = client
        .post(format!("{api}/internal/v1/platform-images/imports"))
        .json(&import_request(
            "foreign",
            "quay.io/labweaver-system/admin-import:24.04",
            u64::try_from(trusted.bytes.len())?,
        ))
        .send()
        .await?;
    assert_eq!(foreign.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let foreign: serde_json::Value = foreign.json().await?;
    assert_eq!(
        foreign["diagnosticCode"],
        "LW_PLATFORM_IMAGE_REFERENCE_INVALID"
    );

    assert!(registry.blob_digests().is_empty());
    assert!(registry.manifest_digest("24.04").is_none());
    let listed: serde_json::Value = client
        .get(format!("{api}/internal/v1/platform-images"))
        .send()
        .await?
        .json()
        .await?;
    assert!(listed["entries"].as_array().expect("entries").is_empty());
    Ok(())
}

#[tokio::test]
async fn import_requires_a_configured_platform_registry() -> Result<(), Box<dyn std::error::Error>>
{
    let (pool, _container) = postgres().await?;
    let (_registry, base) = FakeRegistry::spawn().await?;
    let host = FakeRegistry::authority(&base);
    let archive = oci_layout_archive(
        br#"{"architecture":"amd64","os":"linux"}"#,
        b"labweaver-platform-image-layer-payload",
        None,
    );
    let api = spawn_api(pool.clone(), None, archive.bytes.clone()).await?;

    let refused = reqwest::Client::new()
        .post(format!("{api}/internal/v1/platform-images/imports"))
        .json(&import_request(
            "ubuntu-24.04",
            &format!("{host}/labweaver-system/admin-import:24.04"),
            u64::try_from(archive.bytes.len())?,
        ))
        .send()
        .await?;
    assert_eq!(refused.status(), StatusCode::SERVICE_UNAVAILABLE);
    let refused: serde_json::Value = refused.json().await?;
    assert_eq!(
        refused["diagnosticCode"],
        "LW_PLATFORM_IMAGE_REGISTRY_NOT_CONFIGURED"
    );
    Ok(())
}

async fn listed_entries(
    client: &reqwest::Client,
    api: &str,
) -> Result<Vec<serde_json::Value>, Box<dyn std::error::Error>> {
    let listed: serde_json::Value = client
        .get(format!("{api}/internal/v1/platform-images"))
        .send()
        .await?
        .json()
        .await?;
    Ok(listed["entries"].as_array().cloned().unwrap_or_default())
}

#[tokio::test]
async fn vm_disk_import_wraps_the_disk_publishes_it_and_pins_the_reviewed_identity()
-> Result<(), Box<dyn std::error::Error>> {
    let (pool, _container) = postgres().await?;
    let (registry, base) = FakeRegistry::spawn().await?;
    let host = FakeRegistry::authority(&base);
    let target = format!("{host}/labweaver-system/vm-base:qcow2");
    let disk = b"qcow2-disk-payload-of-the-reviewed-fedora-base".to_vec();
    let archive = disk_archive(&disk, "disk/disk.img");
    let capacity_bytes = u64::try_from(disk.len())? * 4;
    let api = spawn_api(
        pool.clone(),
        Some(platform_registry(&base)?),
        archive.clone(),
    )
    .await?;
    let client = reqwest::Client::new();

    let imported = client
        .post(format!("{api}/internal/v1/platform-images/imports"))
        .json(&vm_import_request(
            "fedora-41-qcow2",
            &target,
            u64::try_from(archive.len())?,
            "qcow2",
            "disk/disk.img",
            capacity_bytes,
        ))
        .send()
        .await?;
    assert_eq!(imported.status(), StatusCode::CREATED);
    let imported: serde_json::Value = imported.json().await?;
    assert_eq!(imported["kind"], "virtual_machine");
    assert_eq!(imported["sourceReference"], target);
    assert_eq!(imported["status"], "active");
    assert_eq!(imported["capacityBytes"], capacity_bytes);
    assert_eq!(imported["diskSha256"], digest_hex(&disk));
    assert_eq!(imported["format"], "qcow2");
    let resolved = imported["resolvedDigest"]
        .as_str()
        .expect("resolved digest")
        .to_owned();
    assert_eq!(registry.manifest_digest("qcow2"), Some(resolved.clone()));
    // The published layer is the exact reviewed disk wrapped as the one entry CDI extracts, and
    // the pinned digest is the wrapper manifest, not the raw disk identity.
    assert_eq!(
        published_disk_entry(&registry, "disk/disk.img").as_deref(),
        Some(disk.as_slice())
    );
    assert_ne!(resolved, format!("sha256:{}", digest_hex(&disk)));

    let entries = listed_entries(&client, &api).await?;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["diskSha256"], digest_hex(&disk));
    Ok(())
}

#[tokio::test]
async fn vm_disk_import_fails_closed_when_the_disk_exceeds_the_declared_capacity()
-> Result<(), Box<dyn std::error::Error>> {
    let (pool, _container) = postgres().await?;
    let (registry, base) = FakeRegistry::spawn().await?;
    let host = FakeRegistry::authority(&base);
    let disk = vec![0x51_u8; 4_096];
    let archive = disk_archive(&disk, "disk/disk.img");
    let api = spawn_api(
        pool.clone(),
        Some(platform_registry(&base)?),
        archive.clone(),
    )
    .await?;
    let client = reqwest::Client::new();

    let oversized = client
        .post(format!("{api}/internal/v1/platform-images/imports"))
        .json(&vm_import_request(
            "oversized-disk",
            &format!("{host}/labweaver-system/vm-base:oversized"),
            u64::try_from(archive.len())?,
            "raw",
            "disk/disk.img",
            4_095,
        ))
        .send()
        .await?;
    assert_eq!(oversized.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let oversized: serde_json::Value = oversized.json().await?;
    assert_eq!(
        oversized["diagnosticCode"],
        "LW_PLATFORM_IMAGE_CAPACITY_INVALID"
    );

    let empty_archive = disk_archive(&[], "disk/disk.img");
    let empty_api = spawn_api(
        pool.clone(),
        Some(platform_registry(&base)?),
        empty_archive.clone(),
    )
    .await?;
    let empty = client
        .post(format!("{empty_api}/internal/v1/platform-images/imports"))
        .json(&vm_import_request(
            "empty-disk",
            &format!("{host}/labweaver-system/vm-base:empty"),
            u64::try_from(empty_archive.len())?,
            "raw",
            "disk/disk.img",
            1,
        ))
        .send()
        .await?;
    assert_eq!(empty.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let empty: serde_json::Value = empty.json().await?;
    assert_eq!(
        empty["diagnosticCode"],
        "LW_PLATFORM_IMAGE_CAPACITY_INVALID"
    );

    assert!(registry.blob_digests().is_empty());
    assert!(registry.manifest_digest("oversized").is_none());
    assert!(listed_entries(&client, &api).await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn vm_disk_import_rejects_an_archive_that_is_not_the_declared_disk()
-> Result<(), Box<dyn std::error::Error>> {
    let (pool, _container) = postgres().await?;
    let (registry, base) = FakeRegistry::spawn().await?;
    let host = FakeRegistry::authority(&base);
    let disk = b"qcow2-disk-payload".to_vec();
    let misnamed_archive = disk_archive(&disk, "disk/other.img");
    let api = spawn_api(
        pool.clone(),
        Some(platform_registry(&base)?),
        misnamed_archive.clone(),
    )
    .await?;
    let client = reqwest::Client::new();

    let misnamed = client
        .post(format!("{api}/internal/v1/platform-images/imports"))
        .json(&vm_import_request(
            "misnamed-disk",
            &format!("{host}/labweaver-system/vm-base:misnamed"),
            u64::try_from(misnamed_archive.len())?,
            "qcow2",
            "disk/disk.img",
            4_096,
        ))
        .send()
        .await?;
    assert_eq!(misnamed.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let misnamed: serde_json::Value = misnamed.json().await?;
    assert_eq!(misnamed["diagnosticCode"], "LW_PLATFORM_IMAGE_DISK_INVALID");

    let mut partial = vm_import_request(
        "partial-disk",
        &format!("{host}/labweaver-system/vm-base:partial"),
        u64::try_from(misnamed_archive.len())?,
        "qcow2",
        "disk/disk.img",
        4_096,
    );
    partial["capacityBytes"] = serde_json::Value::Null;
    let partial = client
        .post(format!("{api}/internal/v1/platform-images/imports"))
        .json(&partial)
        .send()
        .await?;
    assert_eq!(partial.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let partial: serde_json::Value = partial.json().await?;
    assert_eq!(partial["diagnosticCode"], "LW_PLATFORM_IMAGE_DISK_INVALID");

    let mut builder = tar::Builder::new(Vec::new());
    append(&mut builder, "disk/disk.img", &disk);
    append(&mut builder, "disk/extra.img", b"extra");
    let additional = builder.into_inner()?;
    let additional_api = spawn_api(
        pool.clone(),
        Some(platform_registry(&base)?),
        additional.clone(),
    )
    .await?;
    let extra = client
        .post(format!(
            "{additional_api}/internal/v1/platform-images/imports"
        ))
        .json(&vm_import_request(
            "extra-entry",
            &format!("{host}/labweaver-system/vm-base:extra"),
            u64::try_from(additional.len())?,
            "qcow2",
            "disk/disk.img",
            4_096,
        ))
        .send()
        .await?;
    assert_eq!(extra.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let extra: serde_json::Value = extra.json().await?;
    assert_eq!(extra["diagnosticCode"], "LW_PLATFORM_IMAGE_DISK_INVALID");

    assert!(registry.blob_digests().is_empty());
    assert!(registry.manifest_digest("misnamed").is_none());
    assert!(listed_entries(&client, &api).await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn container_registrations_never_carry_a_disk_descriptor()
-> Result<(), Box<dyn std::error::Error>> {
    let (pool, _container) = postgres().await?;
    let (registry, base) = FakeRegistry::spawn().await?;
    let host = FakeRegistry::authority(&base);
    let disk = b"qcow2-disk-payload".to_vec();
    let archive = disk_archive(&disk, "disk/disk.img");
    let api = spawn_api(
        pool.clone(),
        Some(platform_registry(&base)?),
        archive.clone(),
    )
    .await?;
    let client = reqwest::Client::new();

    let mut container_with_disk = vm_import_request(
        "container-with-disk",
        &format!("{host}/labweaver-system/admin-import:24.04"),
        u64::try_from(archive.len())?,
        "qcow2",
        "disk/disk.img",
        4_096,
    );
    container_with_disk["kind"] = serde_json::Value::String("container".to_owned());
    let container_with_disk = client
        .post(format!("{api}/internal/v1/platform-images/imports"))
        .json(&container_with_disk)
        .send()
        .await?;
    assert_eq!(
        container_with_disk.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let container_with_disk: serde_json::Value = container_with_disk.json().await?;
    assert_eq!(
        container_with_disk["diagnosticCode"],
        "LW_PLATFORM_IMAGE_DISK_INVALID"
    );

    let zero_capacity = client
        .post(format!("{api}/internal/v1/platform-images/imports"))
        .json(&vm_import_request(
            "zero-capacity",
            &format!("{host}/labweaver-system/vm-base:zero"),
            u64::try_from(archive.len())?,
            "qcow2",
            "disk/disk.img",
            0,
        ))
        .send()
        .await?;
    assert_eq!(zero_capacity.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let zero_capacity: serde_json::Value = zero_capacity.json().await?;
    assert_eq!(
        zero_capacity["diagnosticCode"],
        "LW_PLATFORM_IMAGE_DISK_INVALID"
    );

    assert!(registry.blob_digests().is_empty());
    assert!(registry.manifest_digest("24.04").is_none());
    assert!(listed_entries(&client, &api).await?.is_empty());
    Ok(())
}
