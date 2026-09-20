//! Real `PostgreSQL` proof for the administrator platform image catalog.
#![allow(
    clippy::expect_used,
    clippy::too_many_lines,
    reason = "one live database test proves the complete pin, repin and disable audit trail"
)]

use agent_service::platform_images::{
    DisablePlatformImage, PgPlatformImageCatalog, PlatformImageKind, PlatformImageStatus,
    PlatformImageStoreError, RegisterPlatformImage, RepinPlatformImage,
};
use contracts::{ActorId, UtcTimestamp};
use sqlx::postgres::PgPoolOptions;
use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;

mod support;
use support::apply_agent_migrations;

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
        trust_revision: 1,
        actor_id: ActorId::new(),
        reason: "reviewed base image".to_owned(),
        now: timestamp(now),
    }
}

#[tokio::test]
async fn catalog_pins_resolves_and_audits_every_mutation() -> Result<(), Box<dyn std::error::Error>>
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

    assert_eq!(catalog.active_list().await?.len(), 2);

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
    assert_eq!(catalog.active_list().await?.len(), 1);
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
    .bind(ubuntu.catalog_id)
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
