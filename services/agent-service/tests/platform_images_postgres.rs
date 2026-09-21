//! Real `PostgreSQL` proof for the administrator platform image catalog.
#![allow(
    clippy::expect_used,
    clippy::too_many_lines,
    reason = "one live database test proves the complete pin, repin and disable audit trail"
)]

use std::sync::Arc;

use agent_service::api::{AgentApiState, CONTROL_PERMISSION, router};
use agent_service::build_store::PgBuildStore;
use agent_service::generated_artifacts::GeneratedArtifactStore;
use agent_service::llm_review::LlmReviewStore;
use agent_service::oci_registry::RegistryCredentials;
use agent_service::platform_images::{
    DisablePlatformImage, PgPlatformImageCatalog, PlatformImageKind, PlatformImageRegistry,
    PlatformImageSeed, PlatformImageSeedOutcome, PlatformImageStatus, PlatformImageStoreError,
    RegisterPlatformImage, RepinPlatformImage, seed_platform_images,
};
use agent_service::run_store::PostgresAgentRunStore;
use contracts::supply_chain::VirtualMachineDiskFormat;
use contracts::{ActorId, UtcTimestamp};
use sqlx::postgres::PgPoolOptions;
use testcontainers::{ContainerAsync, ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;

mod support;
use support::{FakeObjects, FakeRegistry, apply_agent_migrations};

async fn postgres_fixture()
-> Result<(sqlx::PgPool, ContainerAsync<Postgres>), Box<dyn std::error::Error>> {
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

fn timestamp(value: &str) -> UtcTimestamp {
    value.parse().expect("fixture timestamp")
}

fn register(
    kind: PlatformImageKind,
    binding: &str,
    digest: &str,
    now: &str,
) -> RegisterPlatformImage {
    RegisterPlatformImage {
        kind,
        binding: binding.to_owned(),
        source_reference: format!("harbor.internal/labweaver-system/{binding}:24.04"),
        resolved_digest: digest.to_owned(),
        media_type: "application/vnd.oci.image.manifest.v1+json".to_owned(),
        size_bytes: 4_096,
        capacity_bytes: None,
        disk_sha256: None,
        format: None,
        trust_revision: 1,
        actor_id: ActorId::new(),
        reason: "reviewed base image".to_owned(),
        now: timestamp(now),
    }
}

/// Applies the reviewed disk descriptor of one imported virtual-machine base disk.
fn with_disk_descriptor(
    mut request: RegisterPlatformImage,
    capacity_bytes: u64,
    disk_sha256: &str,
    format: VirtualMachineDiskFormat,
) -> RegisterPlatformImage {
    request.capacity_bytes = Some(capacity_bytes);
    request.disk_sha256 = Some(disk_sha256.to_owned());
    request.format = Some(format);
    request
}

#[tokio::test]
async fn catalog_pins_resolves_and_audits_every_mutation() -> Result<(), Box<dyn std::error::Error>>
{
    let (pool, _container) = postgres_fixture().await?;
    let catalog = PgPlatformImageCatalog::new(pool.clone());
    let ubuntu_digest = format!("sha256:{}", "a".repeat(64));
    let cirros_digest = format!("sha256:{}", "b".repeat(64));
    let repinned_digest = format!("sha256:{}", "c".repeat(64));

    let ubuntu = catalog
        .register(&register(
            PlatformImageKind::Container,
            "ubuntu-24.04",
            &ubuntu_digest,
            "2026-09-20T08:00:00.000Z",
        ))
        .await?;
    assert_eq!(ubuntu.status, PlatformImageStatus::Active);
    assert_eq!(ubuntu.repin_generation, 1);
    assert_eq!(ubuntu.resolved_digest, ubuntu_digest);
    catalog
        .register(&register(
            PlatformImageKind::VirtualMachine,
            "cirros-0.6",
            &cirros_digest,
            "2026-09-20T08:01:00.000Z",
        ))
        .await?;

    let disk_sha256 = "d".repeat(64);
    let vm_disk_digest = format!("sha256:{}", "e".repeat(64));
    let imported = catalog
        .register(&with_disk_descriptor(
            register(
                PlatformImageKind::VirtualMachine,
                "fedora-41-qcow2",
                &vm_disk_digest,
                "2026-09-20T08:01:30.000Z",
            ),
            8 * 1024 * 1024 * 1024,
            &disk_sha256,
            VirtualMachineDiskFormat::Qcow2,
        ))
        .await?;
    assert_eq!(imported.capacity_bytes, Some(8 * 1024 * 1024 * 1024));
    assert_eq!(imported.disk_sha256.as_deref(), Some(disk_sha256.as_str()));
    assert_eq!(imported.format, Some(VirtualMachineDiskFormat::Qcow2));
    let inventory = catalog
        .list(None)
        .await?
        .into_iter()
        .find(|entry| entry.binding == "cirros-0.6")
        .expect("reference inventory entry");
    assert_eq!(inventory.capacity_bytes, None);
    assert_eq!(inventory.disk_sha256, None);
    assert_eq!(inventory.format, None);

    let misdeclared = [
        // A virtual-machine registration cannot carry only part of the disk descriptor.
        {
            let mut request = register(
                PlatformImageKind::VirtualMachine,
                "partial-vm-disk",
                &vm_disk_digest,
                "2026-09-20T08:01:40.000Z",
            );
            request.capacity_bytes = Some(4_096);
            request
        },
        // A zero capacity is never a reviewed disk.
        with_disk_descriptor(
            register(
                PlatformImageKind::VirtualMachine,
                "zero-capacity",
                &vm_disk_digest,
                "2026-09-20T08:01:41.000Z",
            ),
            0,
            &disk_sha256,
            VirtualMachineDiskFormat::Raw,
        ),
        // The disk digest is the raw disk identity, never an arbitrary string.
        with_disk_descriptor(
            register(
                PlatformImageKind::VirtualMachine,
                "malformed-sha",
                &vm_disk_digest,
                "2026-09-20T08:01:42.000Z",
            ),
            4_096,
            "not-a-disk-digest",
            VirtualMachineDiskFormat::Raw,
        ),
        // A container pin never describes a wrapped disk.
        with_disk_descriptor(
            register(
                PlatformImageKind::Container,
                "container-with-disk",
                &vm_disk_digest,
                "2026-09-20T08:01:43.000Z",
            ),
            4_096,
            &disk_sha256,
            VirtualMachineDiskFormat::Raw,
        ),
    ];
    for request in &misdeclared {
        assert!(
            matches!(
                catalog.register(request).await,
                Err(PlatformImageStoreError::InvalidRequest)
            ),
            "{} must be rejected",
            request.binding
        );
    }

    // The persisted shape is a database invariant too, so a validation gap can never store a
    // partially described virtual-machine row.
    let partial_insert = sqlx::query(
        "INSERT INTO agent.platform_image_catalog \
         (catalog_id,kind,binding,source_reference,resolved_digest,media_type,size_bytes,\
          capacity_bytes,status,trust_revision,created_by,pinned_at) \
         VALUES ($1,'virtual_machine','db-partial-disk','harbor.internal/labweaver-system/db:24.04',\
                 $2,'application/vnd.oci.image.manifest.v1+json',4096,4096,'active',1,$3,now())",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(&vm_disk_digest)
    .bind(ActorId::new().as_uuid())
    .execute(&pool)
    .await;
    assert!(partial_insert.is_err());

    assert!(matches!(
        catalog
            .register(&register(
                PlatformImageKind::Container,
                "ubuntu-24.04",
                &repinned_digest,
                "2026-09-20T08:02:00.000Z",
            ))
            .await,
        Err(PlatformImageStoreError::Conflict)
    ));
    let mut invalid = register(
        PlatformImageKind::Container,
        "invalid-digest",
        &ubuntu_digest,
        "2026-09-20T08:03:00.000Z",
    );
    invalid.resolved_digest = "sha256:short".to_owned();
    assert!(matches!(
        catalog.register(&invalid).await,
        Err(PlatformImageStoreError::InvalidRequest)
    ));

    assert_eq!(catalog.active_list().await?.len(), 3);

    let repinned = catalog
        .repin(
            ubuntu.catalog_id,
            &RepinPlatformImage {
                resolved_digest: repinned_digest.clone(),
                media_type: "application/vnd.oci.image.manifest.v1+json".to_owned(),
                size_bytes: 5_120,
                expected_digest: ubuntu_digest.clone(),
                trust_revision: 2,
                actor_id: ActorId::new(),
                reason: "security refresh".to_owned(),
                now: timestamp("2026-09-20T09:00:00.000Z"),
            },
        )
        .await?;
    assert_eq!(repinned.resolved_digest, repinned_digest);
    assert_eq!(repinned.repin_generation, 2);
    assert!(matches!(
        catalog
            .repin(
                ubuntu.catalog_id,
                &RepinPlatformImage {
                    resolved_digest: cirros_digest.clone(),
                    media_type: "application/vnd.oci.image.manifest.v1+json".to_owned(),
                    size_bytes: 5_120,
                    expected_digest: ubuntu_digest.clone(),
                    trust_revision: 2,
                    actor_id: ActorId::new(),
                    reason: "stale administrator".to_owned(),
                    now: timestamp("2026-09-20T09:01:00.000Z"),
                },
            )
            .await,
        Err(PlatformImageStoreError::Conflict)
    ));

    let disabled = catalog
        .disable(
            ubuntu.catalog_id,
            &DisablePlatformImage {
                expected_digest: repinned_digest.clone(),
                actor_id: ActorId::new(),
                reason: "superseded by ubuntu-26.04".to_owned(),
                now: timestamp("2026-09-20T10:00:00.000Z"),
            },
        )
        .await?;
    assert_eq!(disabled.status, PlatformImageStatus::Disabled);
    assert_eq!(catalog.active_list().await?.len(), 2);
    assert_eq!(catalog.list(Some("disabled")).await?.len(), 1);
    assert!(matches!(
        catalog
            .repin(
                ubuntu.catalog_id,
                &RepinPlatformImage {
                    resolved_digest: ubuntu_digest.clone(),
                    media_type: "application/vnd.oci.image.manifest.v1+json".to_owned(),
                    size_bytes: 5_120,
                    expected_digest: repinned_digest.clone(),
                    trust_revision: 3,
                    actor_id: ActorId::new(),
                    reason: "revive is a new registration".to_owned(),
                    now: timestamp("2026-09-20T10:01:00.000Z"),
                },
            )
            .await,
        Err(PlatformImageStoreError::Conflict)
    ));
    assert!(matches!(
        catalog
            .disable(
                ubuntu.catalog_id,
                &DisablePlatformImage {
                    expected_digest: repinned_digest.clone(),
                    actor_id: ActorId::new(),
                    reason: "already disabled".to_owned(),
                    now: timestamp("2026-09-20T10:02:00.000Z"),
                },
            )
            .await,
        Err(PlatformImageStoreError::Conflict)
    ));

    let audit: Vec<(String, Option<String>, Option<String>, i64)> = sqlx::query_as(
        "SELECT action, from_digest, to_digest, repin_generation \
         FROM agent.platform_image_catalog_audit WHERE catalog_id=$1 ORDER BY created_at, audit_id",
    )
    .bind(ubuntu.catalog_id.as_uuid())
    .fetch_all(&pool)
    .await?;
    assert_eq!(audit.len(), 3);
    assert_eq!(audit[0].0, "registered");
    assert_eq!(audit[0].2.as_deref(), Some(ubuntu_digest.as_str()));
    assert_eq!(audit[1].0, "repinned");
    assert_eq!(audit[1].1.as_deref(), Some(ubuntu_digest.as_str()));
    assert_eq!(audit[1].2.as_deref(), Some(repinned_digest.as_str()));
    assert_eq!(audit[1].3, 2);
    assert_eq!(audit[2].0, "disabled");
    assert_eq!(audit[2].1.as_deref(), Some(repinned_digest.as_str()));
    Ok(())
}

fn registry_manifest_bytes(config_size: u64, layer_size: u64) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "schemaVersion": 2,
        "config": {
            "mediaType": "application/vnd.oci.image.config.v1+json",
            "digest": format!("sha256:{}", "1".repeat(64)),
            "size": config_size,
        },
        "layers": [{
            "mediaType": "application/vnd.oci.image.layer.v1.tar",
            "digest": format!("sha256:{}", "2".repeat(64)),
            "size": layer_size,
        }],
    }))
    .expect("manifest json")
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

async fn spawn_api(
    pool: sqlx::PgPool,
    registry_base: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let state = Arc::new(AgentApiState {
        store: PostgresAgentRunStore::new(pool.clone()),
        build_store: PgBuildStore::new(pool.clone()),
        generated_artifacts: GeneratedArtifactStore::new(pool.clone()),
        llm_reviews: LlmReviewStore::new(pool.clone()),
        platform_images: PgPlatformImageCatalog::new(pool),
        platform_registry: Some(PlatformImageRegistry::for_test(
            reqwest::Url::parse(registry_base)?,
            reqwest::Client::builder().no_proxy().build()?,
            RegistryCredentials {
                username: "robot$platform".to_owned(),
                password: "secret".to_owned(),
            },
        )),
        objects: Arc::new(FakeObjects::new(Vec::new())),
    });
    let router = router(state).layer(axum::middleware::from_fn(inject_control_identity));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok(format!("http://{address}"))
}

#[tokio::test]
async fn admin_http_api_resolves_pins_repins_and_disables() -> Result<(), Box<dyn std::error::Error>>
{
    let (pool, _container) = postgres_fixture().await?;

    let (registry, registry_base) = FakeRegistry::spawn().await?;
    let first_manifest = registry_manifest_bytes(128, 256);
    registry.set_manifest("24.04", first_manifest.clone());
    let reference = format!(
        "{}/labweaver-system/ubuntu:24.04",
        FakeRegistry::authority(&registry_base)
    );

    let api = spawn_api(pool.clone(), &registry_base).await?;
    let client = reqwest::Client::new();
    let first_digest = format!(
        "sha256:{}",
        persistence_sqlx::Sha256Digest::of_bytes(&first_manifest)
    );

    let created = client
        .post(format!("{api}/internal/v1/platform-images"))
        .json(&serde_json::json!({
            "kind": "container",
            "binding": "ubuntu-24.04",
            "sourceReference": reference,
            "trustRevision": 1,
            "actorId": ActorId::new(),
            "reason": "reviewed ubuntu base",
        }))
        .send()
        .await?;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let created: serde_json::Value = created.json().await?;
    assert_eq!(created["resolvedDigest"], first_digest);
    assert_eq!(
        created["mediaType"],
        "application/vnd.oci.image.manifest.v1+json"
    );
    assert_eq!(created["sizeBytes"], 384);
    assert_eq!(created["status"], "active");
    let catalog_id = created["catalogId"]
        .as_str()
        .expect("catalog id")
        .to_owned();

    let foreign = client
        .post(format!("{api}/internal/v1/platform-images"))
        .json(&serde_json::json!({
            "kind": "container",
            "binding": "foreign",
            "sourceReference": "quay.io/labweaver-system/ubuntu:24.04",
            "trustRevision": 1,
            "actorId": ActorId::new(),
            "reason": "reviewed foreign base",
        }))
        .send()
        .await?;
    assert_eq!(foreign.status(), reqwest::StatusCode::UNPROCESSABLE_ENTITY);
    let foreign: serde_json::Value = foreign.json().await?;
    assert_eq!(
        foreign["diagnosticCode"],
        "LW_PLATFORM_IMAGE_REFERENCE_INVALID"
    );

    let duplicate = client
        .post(format!("{api}/internal/v1/platform-images"))
        .json(&serde_json::json!({
            "kind": "container",
            "binding": "ubuntu-24.04",
            "sourceReference": reference,
            "trustRevision": 1,
            "actorId": ActorId::new(),
            "reason": "duplicate registration",
        }))
        .send()
        .await?;
    assert_eq!(duplicate.status(), reqwest::StatusCode::CONFLICT);

    let listed: serde_json::Value = client
        .get(format!("{api}/internal/v1/platform-images"))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(listed["entries"].as_array().expect("entries").len(), 1);

    let second_manifest = registry_manifest_bytes(256, 512);
    registry.set_manifest("24.04", second_manifest.clone());
    let second_digest = format!(
        "sha256:{}",
        persistence_sqlx::Sha256Digest::of_bytes(&second_manifest)
    );
    let repinned = client
        .post(format!(
            "{api}/internal/v1/platform-images/{catalog_id}/repin"
        ))
        .json(&serde_json::json!({
            "expectedDigest": first_digest,
            "trustRevision": 2,
            "actorId": ActorId::new(),
            "reason": "security refresh",
        }))
        .send()
        .await?;
    assert_eq!(repinned.status(), reqwest::StatusCode::OK);
    let repinned: serde_json::Value = repinned.json().await?;
    assert_eq!(repinned["resolvedDigest"], second_digest);
    assert_eq!(repinned["repinGeneration"], 2);

    let stale = client
        .post(format!(
            "{api}/internal/v1/platform-images/{catalog_id}/repin"
        ))
        .json(&serde_json::json!({
            "expectedDigest": first_digest,
            "trustRevision": 3,
            "actorId": ActorId::new(),
            "reason": "stale refresh",
        }))
        .send()
        .await?;
    assert_eq!(stale.status(), reqwest::StatusCode::CONFLICT);

    let disabled = client
        .post(format!(
            "{api}/internal/v1/platform-images/{catalog_id}/disable"
        ))
        .json(&serde_json::json!({
            "expectedDigest": second_digest,
            "actorId": ActorId::new(),
            "reason": "superseded",
        }))
        .send()
        .await?;
    assert_eq!(disabled.status(), reqwest::StatusCode::OK);
    let disabled: serde_json::Value = disabled.json().await?;
    assert_eq!(disabled["status"], "disabled");

    let already_disabled = client
        .post(format!(
            "{api}/internal/v1/platform-images/{catalog_id}/disable"
        ))
        .json(&serde_json::json!({
            "expectedDigest": second_digest,
            "actorId": ActorId::new(),
            "reason": "repeat disable",
        }))
        .send()
        .await?;
    assert_eq!(already_disabled.status(), reqwest::StatusCode::CONFLICT);

    let audit_rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM agent.platform_image_catalog_audit WHERE catalog_id=$1",
    )
    .bind(uuid::Uuid::parse_str(&catalog_id)?)
    .fetch_one(&pool)
    .await?;
    assert_eq!(audit_rows, 3);
    Ok(())
}

/// One reviewed seed reference inside the fixture registry.
fn seed(
    kind: PlatformImageKind,
    binding: &str,
    source_reference: String,
    trust_revision: u64,
) -> PlatformImageSeed {
    PlatformImageSeed {
        kind,
        binding: binding.to_owned(),
        source_reference,
        trust_revision,
    }
}

#[tokio::test]
async fn deployment_seeds_register_reviewed_bindings_and_fail_closed()
-> Result<(), Box<dyn std::error::Error>> {
    let (pool, _container) = postgres_fixture().await?;
    let (registry, registry_base) = FakeRegistry::spawn().await?;
    let reviewed = registry_manifest_bytes(128, 256);
    registry.set_manifest("1.97.1-bookworm", reviewed.clone());
    let authority = FakeRegistry::authority(&registry_base);
    let catalog = PgPlatformImageCatalog::new(pool.clone());
    let resolver = platform_registry(&registry_base)?;
    let seeds = vec![
        seed(
            PlatformImageKind::Container,
            "rust-builder-v1",
            format!("{authority}/labweaver-system/rust:1.97.1-bookworm"),
            1,
        ),
        seed(
            PlatformImageKind::Container,
            "absent-base-v1",
            format!("{authority}/labweaver-system/rust:absent"),
            1,
        ),
        seed(
            PlatformImageKind::Container,
            "Not A Binding",
            format!("{authority}/labweaver-system/rust:1.97.1-bookworm"),
            1,
        ),
        seed(
            PlatformImageKind::Container,
            "unrevisioned-v1",
            format!("{authority}/labweaver-system/rust:1.97.1-bookworm"),
            0,
        ),
    ];

    let outcomes = seed_platform_images(
        &catalog,
        &resolver,
        &seeds,
        timestamp("2026-09-20T08:00:00.000Z"),
    )
    .await;

    assert_eq!(
        outcomes,
        vec![
            PlatformImageSeedOutcome::Registered,
            PlatformImageSeedOutcome::Failed {
                cause: "LW_PLATFORM_IMAGE_REGISTRY_REJECTED"
            },
            PlatformImageSeedOutcome::Failed {
                cause: "LW_PLATFORM_IMAGE_REQUEST_INVALID"
            },
            PlatformImageSeedOutcome::Failed {
                cause: "LW_PLATFORM_IMAGE_REQUEST_INVALID"
            },
        ]
    );
    let entries = catalog.list(None).await?;
    assert_eq!(entries.len(), 1);
    let entry = &entries[0];
    assert_eq!(entry.kind, PlatformImageKind::Container);
    assert_eq!(entry.binding, "rust-builder-v1");
    assert_eq!(entry.trust_revision, 1);
    assert_eq!(entry.status, PlatformImageStatus::Active);
    assert_eq!(
        entry.resolved_digest,
        format!(
            "sha256:{}",
            persistence_sqlx::Sha256Digest::of_bytes(&reviewed)
        )
    );
    Ok(())
}

#[tokio::test]
async fn deployment_seeds_never_repin_or_reactivate_an_existing_entry()
-> Result<(), Box<dyn std::error::Error>> {
    let (pool, _container) = postgres_fixture().await?;
    let (registry, registry_base) = FakeRegistry::spawn().await?;
    let first = registry_manifest_bytes(128, 256);
    registry.set_manifest("1.97.1-bookworm", first.clone());
    let authority = FakeRegistry::authority(&registry_base);
    let catalog = PgPlatformImageCatalog::new(pool.clone());
    let resolver = platform_registry(&registry_base)?;
    let reference = format!("{authority}/labweaver-system/rust:1.97.1-bookworm");

    let registered = seed_platform_images(
        &catalog,
        &resolver,
        &[seed(
            PlatformImageKind::Container,
            "rust-builder-v1",
            reference.clone(),
            1,
        )],
        timestamp("2026-09-20T08:00:00.000Z"),
    )
    .await;
    assert_eq!(registered, vec![PlatformImageSeedOutcome::Registered]);
    let seeded = catalog
        .list(None)
        .await?
        .into_iter()
        .next()
        .expect("seeded entry");

    // A moved tag and a redeployed trust revision change nothing: the pin is the administrator's.
    registry.set_manifest("1.97.1-bookworm", registry_manifest_bytes(256, 512));
    let repeated = seed_platform_images(
        &catalog,
        &resolver,
        &[seed(
            PlatformImageKind::Container,
            "rust-builder-v1",
            reference,
            2,
        )],
        timestamp("2026-09-20T09:00:00.000Z"),
    )
    .await;
    assert_eq!(repeated, vec![PlatformImageSeedOutcome::Existing]);
    let unchanged = catalog
        .list(None)
        .await?
        .into_iter()
        .next()
        .expect("unchanged entry");
    assert_eq!(unchanged.resolved_digest, seeded.resolved_digest);
    assert_eq!(unchanged.trust_revision, 1);
    assert_eq!(unchanged.repin_generation, 1);
    assert_eq!(unchanged.updated_at, seeded.updated_at);

    catalog
        .disable(
            seeded.catalog_id,
            &DisablePlatformImage {
                expected_digest: seeded.resolved_digest.clone(),
                actor_id: ActorId::new(),
                reason: "superseded by rust-builder-v2".to_owned(),
                now: timestamp("2026-09-20T09:30:00.000Z"),
            },
        )
        .await?;
    let disabled = seed_platform_images(
        &catalog,
        &resolver,
        &[seed(
            PlatformImageKind::Container,
            "rust-builder-v1",
            format!("{authority}/labweaver-system/rust:1.97.1-bookworm"),
            1,
        )],
        timestamp("2026-09-20T10:00:00.000Z"),
    )
    .await;
    assert_eq!(disabled, vec![PlatformImageSeedOutcome::Existing]);
    assert_eq!(catalog.active_list().await?.len(), 0);
    let listed = catalog.list(None).await?;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].status, PlatformImageStatus::Disabled);

    let audit_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM agent.platform_image_catalog_audit")
            .fetch_one(&pool)
            .await?;
    assert_eq!(audit_rows, 2);
    Ok(())
}
