//! `PostgreSQL` coverage for the actor-scoped Environment inventory contract.

#![allow(clippy::too_many_lines)]

#[path = "support/mod.rs"]
mod support;

use contracts::authoring::{EnvironmentClass, RuntimeKind};
use contracts::environment::{
    DesiredEnvironmentState, EnvironmentLeaseAuthorization, ObservedEnvironmentState,
};
use contracts::resource::WorkloadResources;
use contracts::{EnvironmentId, LeaseId, ProjectId, ReleaseId, ResourceRequestId, Revision};
use environment_service::{EnvironmentInventoryFilter, EnvironmentStoreError, PgEnvironmentStore};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;
use time::OffsetDateTime;

#[tokio::test]
async fn inventory_filters_and_keyset_cursor_preserve_scope()
-> Result<(), Box<dyn std::error::Error>> {
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let url = format!(
        "postgres://postgres:postgres@127.0.0.1:{}/postgres",
        container.get_host_port_ipv4(5432).await?
    );
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await?;
    support::apply_environment_migrations(&pool).await?;
    let store = PgEnvironmentStore::new(pool.clone());

    let newest = support::requested_instance();
    let project_id = newest.project_id;
    let course_id = newest.course_id;
    let owner_actor_id = newest.owner_id;

    let mut middle = newest.clone();
    middle.id = EnvironmentId::new();
    middle.operation.id = contracts::OperationId::new();
    middle.release_id = ReleaseId::new();

    let mut oldest = newest.clone();
    oldest.id = EnvironmentId::new();
    oldest.operation.id = contracts::OperationId::new();
    oldest.release_id = ReleaseId::new();
    oldest.class = EnvironmentClass::Work;
    oldest.runtime_kind = RuntimeKind::VirtualMachine;
    oldest.lease_id = Some(LeaseId::new());
    oldest.capacity_binding = Some("inventory-capacity".to_owned());
    let oldest_lease_id = oldest.lease_id.ok_or("lease id set")?;
    oldest.operation.lease_authorization = Some(EnvironmentLeaseAuthorization {
        resource_request_id: ResourceRequestId::new(),
        lease_id: oldest_lease_id,
        lease_revision: Revision::new(1)?,
        environment_id: oldest.id,
        project_id: oldest.project_id,
        course_id: oldest.course_id,
        owner_actor_id: oldest.owner_id,
        capacity_binding: "inventory-capacity".to_owned(),
        approved_resources: WorkloadResources {
            cpu_millicores: 1,
            memory_bytes: 1,
            storage_bytes: 1,
            gpu: None,
        },
        gpu_allocation: None,
        active_from: oldest.operation.accepted_at,
        expires_at: support::timestamp("2026-07-15T00:00:00.000Z"),
    });

    let mut outside_project = newest.clone();
    outside_project.id = EnvironmentId::new();
    outside_project.operation.id = contracts::OperationId::new();
    outside_project.project_id = ProjectId::new();

    for (key, instance) in [
        ("inventory-newest", &newest),
        ("inventory-middle", &middle),
        ("inventory-oldest", &oldest),
        ("inventory-outside-project", &outside_project),
    ] {
        store.create(key, instance).await?;
    }

    update_inventory_row(
        &pool,
        &newest,
        "2026-07-24T00:00:03.123456Z",
        "experiment",
        "container",
        "running",
        "requested",
    )
    .await?;
    update_inventory_row(
        &pool,
        &middle,
        "2026-07-24T00:00:02.123456Z",
        "experiment",
        "container",
        "running",
        "requested",
    )
    .await?;
    update_inventory_row(
        &pool,
        &oldest,
        "2026-07-24T00:00:01.123456Z",
        "work",
        "virtual_machine",
        "stopped",
        "stopped",
    )
    .await?;

    let filter = EnvironmentInventoryFilter {
        project_id,
        course_id,
        owner_actor_id,
        runtime_kind: None,
        class: None,
        desired_state: None,
        observed_state: None,
        release_id: None,
    };
    let first = store.list_owned(filter, None, 1).await?;
    assert_eq!(first.records.len(), 1);
    assert_eq!(first.records[0].instance.id, newest.id);
    let cursor = first
        .next_cursor
        .as_deref()
        .ok_or("expected a cursor after the first page")?;

    let second = store.list_owned(filter, Some(cursor), 1).await?;
    assert_eq!(second.records.len(), 1);
    assert_eq!(second.records[0].instance.id, middle.id);
    let last_cursor = second
        .next_cursor
        .as_deref()
        .ok_or("expected a cursor after the second page")?;
    let third = store.list_owned(filter, Some(last_cursor), 1).await?;
    assert_eq!(third.records.len(), 1);
    assert_eq!(third.records[0].instance.id, oldest.id);
    assert!(third.next_cursor.is_none());

    let work = store
        .list_owned(
            EnvironmentInventoryFilter {
                class: Some(EnvironmentClass::Work),
                ..filter
            },
            None,
            100,
        )
        .await?;
    assert_eq!(
        work.records
            .iter()
            .map(|row| row.instance.id)
            .collect::<Vec<_>>(),
        vec![oldest.id]
    );

    let virtual_machine = store
        .list_owned(
            EnvironmentInventoryFilter {
                runtime_kind: Some(RuntimeKind::VirtualMachine),
                ..filter
            },
            None,
            100,
        )
        .await?;
    assert_eq!(virtual_machine.records.len(), 1);
    assert_eq!(virtual_machine.records[0].instance.id, oldest.id);

    let stopped = store
        .list_owned(
            EnvironmentInventoryFilter {
                desired_state: Some(DesiredEnvironmentState::Stopped),
                ..filter
            },
            None,
            100,
        )
        .await?;
    assert_eq!(stopped.records.len(), 1);
    assert_eq!(stopped.records[0].instance.id, oldest.id);

    let ready = store
        .list_owned(
            EnvironmentInventoryFilter {
                observed_state: Some(ObservedEnvironmentState::Stopped),
                ..filter
            },
            None,
            100,
        )
        .await?;
    assert_eq!(ready.records.len(), 1);
    assert_eq!(ready.records[0].instance.id, oldest.id);

    let release = store
        .list_owned(
            EnvironmentInventoryFilter {
                release_id: Some(middle.release_id),
                ..filter
            },
            None,
            100,
        )
        .await?;
    assert_eq!(release.records.len(), 1);
    assert_eq!(release.records[0].instance.id, middle.id);

    assert!(matches!(
        store
            .list_owned(
                EnvironmentInventoryFilter {
                    class: Some(EnvironmentClass::Work),
                    ..filter
                },
                Some(cursor),
                1,
            )
            .await,
        Err(EnvironmentStoreError::InvalidInventoryCursor)
    ));
    assert!(matches!(
        store.list_owned(filter, Some("not-a-cursor"), 1).await,
        Err(EnvironmentStoreError::InvalidInventoryCursor)
    ));

    let all = store.list_owned(filter, None, 100).await?;
    assert_eq!(all.records.len(), 3);
    assert!(
        !all.records
            .iter()
            .any(|record| record.instance.id == outside_project.id)
    );
    Ok(())
}

async fn update_inventory_row(
    pool: &PgPool,
    instance: &contracts::environment::EnvironmentInstance,
    created_at: &str,
    class: &str,
    runtime_kind: &str,
    desired_state: &str,
    observed_state: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut contract: Value = serde_json::to_value(instance)?;
    contract["class"] = json!(class);
    contract["runtimeKind"] = json!(runtime_kind);
    contract["desiredState"] = json!(desired_state);
    contract["observedState"] = json!(observed_state);
    contract["releaseId"] = json!(instance.release_id);
    let timestamp =
        OffsetDateTime::parse(created_at, &time::format_description::well_known::Rfc3339)?;
    let result = sqlx::query(
        "UPDATE environment.environment_instances \
         SET created_at=$1, updated_at=$1, release_id=$2, desired_state=$3, observed_state=$4, contract=$5 \
         WHERE environment_id=$6",
    )
    .bind(timestamp)
        .bind(instance.release_id.as_uuid())
    .bind(desired_state)
    .bind(observed_state)
    .bind(contract)
    .bind(instance.id.as_uuid())
    .execute(pool)
    .await?;
    assert_eq!(result.rows_affected(), 1);
    Ok(())
}
