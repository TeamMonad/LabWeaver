//! Real `PostgreSQL` integration coverage using a disposable Docker container.

use std::collections::BTreeMap;
use std::path::Path;
use std::str::FromStr;

use contracts::Sha256Digest;
use persistence_sqlx::{
    Domain, IdempotencyDecision, IdempotencyStore, MigrationCatalog, MigrationCoordinator,
    MigrationIdentity, OutboxStore, SchemaStatus, SchemaVerifier,
};
use serde_json::json;
use sqlx::{
    ConnectOptions, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;
use uuid::Uuid;

const TEST_PASSWORD: &str = "labweaver-test-only-password";

#[tokio::test]
async fn bootstrap_migrate_and_enforce_domain_boundaries() -> Result<(), Box<dyn std::error::Error>>
{
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let provisioner_url = format!(
        "postgres://postgres:postgres@127.0.0.1:{}/postgres",
        container.get_host_port_ipv4(5432).await?
    );
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
    let catalog = MigrationCatalog::load(&root.join("catalog.yaml"))?;
    let provisioner = PgPoolOptions::new()
        .max_connections(2)
        .connect(&provisioner_url)
        .await?;
    MigrationCoordinator::bootstrap(&provisioner, &catalog, &root).await?;
    set_role_passwords(&provisioner).await?;

    let coordinator = role_pool(&provisioner_url, "lw_release_coordinator").await?;
    let migration_pools = pools_for(&provisioner_url, "migration").await?;
    let identity = MigrationIdentity {
        cluster_uuid: "integration-cluster".to_owned(),
        release_id: "integration-release".to_owned(),
        git_commit: "integration-commit".to_owned(),
        build_digest: "sha256:integration-build".to_owned(),
        job_id: "integration-job".to_owned(),
        attempt_id: Uuid::now_v7().to_string(),
    };
    let coordinator = MigrationCoordinator::new(coordinator, migration_pools, identity)?;
    let report = coordinator.apply(&catalog, &root).await?;
    assert_eq!(report.report.outcome, "succeeded");

    let runtime_pools = pools_for(&provisioner_url, "runtime").await?;
    for catalog_domain in &catalog.domains {
        assert_eq!(
            SchemaVerifier::classify(&runtime_pools[&catalog_domain.name], catalog_domain).await,
            SchemaStatus::Ready
        );
    }

    let control = &runtime_pools[&Domain::Control];
    let request_hash = Sha256Digest::of_bytes(b"request");
    let mut transaction = control.begin().await?;
    assert_eq!(
        IdempotencyStore::reserve(
            &mut transaction,
            Domain::Control,
            "create",
            "key-1",
            request_hash
        )
        .await?,
        IdempotencyDecision::Reserved
    );
    OutboxStore::enqueue(
        &mut transaction,
        Domain::Control,
        Uuid::now_v7(),
        "labweaver.control.environment_template_release.published.v1",
        "labweaver.control.environment_template_release.published.v1",
        Uuid::now_v7(),
        1,
        &json!({"event": "safe"}),
        Sha256Digest::of_bytes(b"payload"),
    )
    .await?;
    IdempotencyStore::complete(
        &mut transaction,
        Domain::Control,
        "create",
        "key-1",
        &json!({"id": "one"}),
    )
    .await?;
    transaction.commit().await?;
    let mut replay = control.begin().await?;
    assert_eq!(
        IdempotencyStore::reserve(
            &mut replay,
            Domain::Control,
            "create",
            "key-1",
            request_hash
        )
        .await?,
        IdempotencyDecision::Replay(json!({"id": "one"}))
    );
    replay.rollback().await?;

    assert!(
        sqlx::query("CREATE TABLE forbidden_runtime_ddl (id integer)")
            .execute(control)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("SELECT * FROM access.access_grants")
            .fetch_all(control)
            .await
            .is_err()
    );
    assert!(sqlx::query("INSERT INTO shared_audit.audit_records (event_id, source_domain, event_type, aggregate_id, aggregate_sequence, sanitized_record) VALUES ($1, 'control', 'test', $2, 1, '{}'::jsonb)").bind(Uuid::now_v7()).bind(Uuid::now_v7()).execute(control).await.is_err());
    Ok(())
}

async fn set_role_passwords(pool: &PgPool) -> Result<(), sqlx::Error> {
    for role in [
        "lw_release_coordinator",
        "lw_control_migration",
        "lw_access_migration",
        "lw_environment_migration",
        "lw_agent_migration",
        "lw_evaluation_migration",
        "lw_resource_migration",
        "lw_control_runtime",
        "lw_access_runtime",
        "lw_environment_runtime",
        "lw_agent_runtime",
        "lw_evaluation_runtime",
        "lw_resource_runtime",
    ] {
        sqlx::query(&format!("ALTER ROLE {role} PASSWORD '{TEST_PASSWORD}'"))
            .execute(pool)
            .await?;
    }
    Ok(())
}

async fn pools_for(url: &str, kind: &str) -> Result<BTreeMap<Domain, PgPool>, sqlx::Error> {
    let mut pools = BTreeMap::new();
    for domain in Domain::ALL {
        let role = if kind == "migration" {
            domain.migration_role()
        } else {
            domain.runtime_role()
        };
        pools.insert(domain, role_pool(url, role).await?);
    }
    Ok(pools)
}

async fn role_pool(url: &str, role: &str) -> Result<PgPool, sqlx::Error> {
    let options = PgConnectOptions::from_str(url)?
        .username(role)
        .password(TEST_PASSWORD)
        .log_statements(log::LevelFilter::Off);
    PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await
}
