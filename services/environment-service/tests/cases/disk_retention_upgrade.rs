//! Upgrade real protocol-1 persistence without rebinding a destructive UID checkpoint.
use super::*;

#[tokio::test]
async fn protocol_one_upgrade_preserves_normal_instances_and_quarantines_cleanup()
-> Result<(), Box<dyn std::error::Error>> {
    let postgres = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            postgres.get_host_port_ipv4(5432).await?
        ))
        .await?;
    support::apply_environment_migrations_through(&pool, 9).await?;
    let store = PgEnvironmentStore::new(pool.clone());
    let mut rows = Vec::new();
    for state in [
        ObservedEnvironmentState::Ready,
        ObservedEnvironmentState::Stopped,
        ObservedEnvironmentState::Deleting,
    ] {
        let mut row = requested_instance();
        store
            .create(&format!("upgrade-create-{state:?}"), &row)
            .await?;
        row.observed_state = state;
        row.observed_generation = row.generation;
        row.operation.state = OperationState::Succeeded;
        if state == ObservedEnvironmentState::Ready {
            row.endpoints = support::ready_instance().endpoints;
        } else {
            row.operation.access_revocation_revision = Some(row.revision);
            row.operation.kind = EnvironmentOperationKind::Stop;
        }
        if state == ObservedEnvironmentState::Deleting {
            row.desired_state = DesiredEnvironmentState::Deleted;
            row.operation.kind = EnvironmentOperationKind::Delete;
            row.operation.state = OperationState::Running;
            row.operation.cleanup_target = Some(contracts::environment::EnvironmentCleanupTarget {
                namespace: format!("lw-env-{}", row.id),
                uid: uuid::Uuid::from_u128(123),
                objects: Vec::new(),
                retain_disk: false,
                purge_disk: false,
            });
        }
        row.validate()?;
        // These are the exact marker values persisted by the base release, not a missing marker.
        row.operation.cleanup_protocol_version = Some(1);
        sqlx::query("UPDATE environment.environment_instances SET contract=$2,desired_state=$3,observed_state=$4,observed_generation=$5 WHERE environment_id=$1")
            .bind(row.id.as_uuid()).bind(serde_json::to_value(&row)?)
            .bind(serde_json::to_value(row.desired_state)?.as_str().ok_or("state")?)
            .bind(serde_json::to_value(row.observed_state)?.as_str().ok_or("state")?)
            .bind(i64::try_from(row.observed_generation)?).execute(&pool).await?;
        sqlx::query("UPDATE environment.environment_operations SET contract=$2,state=$3,operation_kind=$4 WHERE operation_id=$1")
            .bind(row.operation.id.as_uuid()).bind(serde_json::to_value(&row.operation)?)
            .bind(serde_json::to_value(row.operation.state)?.as_str().ok_or("state")?)
            .bind(serde_json::to_value(row.operation.kind)?.as_str().ok_or("kind")?).execute(&pool).await?;
        rows.push(row);
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
    let catalog = persistence_sqlx::MigrationCatalog::load(&root.join("catalog.yaml"))?;
    let domain = catalog
        .domains
        .iter()
        .find(|domain| domain.name == persistence_sqlx::Domain::Environment)
        .ok_or("domain")?;
    let migration = domain
        .migrations
        .iter()
        .find(|migration| migration.id == 10)
        .ok_or("cutover")?;
    let sql = persistence_sqlx::MigrationCatalog::read_verified_sql(&root, migration)?;
    let mut connection = pool.acquire().await?;
    sqlx::query("SET search_path=environment,pg_catalog")
        .execute(&mut *connection)
        .await?;
    for _ in 0..2 {
        sqlx::raw_sql(sqlx::AssertSqlSafe(&*sql))
            .execute(&mut *connection)
            .await?;
    }
    drop(connection);
    for before in rows {
        let after = store.load(before.id).await?;
        assert_eq!(after.operation.cleanup_protocol_version, Some(2));
        assert_eq!(
            after.operation.cleanup_target,
            before.operation.cleanup_target
        );
        let operation: Value = sqlx::query_scalar(
            "SELECT contract FROM environment.environment_operations WHERE operation_id=$1",
        )
        .bind(after.operation.id.as_uuid())
        .fetch_one(&pool)
        .await?;
        assert_eq!(operation, serde_json::to_value(&after.operation)?);
        if before.observed_state == ObservedEnvironmentState::Deleting {
            assert_eq!(after.observed_state, ObservedEnvironmentState::Failed);
            assert_eq!(after.operation.state, OperationState::Failed);
            assert_eq!(
                after.last_diagnostic_code.as_deref(),
                Some("LW_ENVIRONMENT_CLEANUP_PROTOCOL_UPGRADE_REQUIRED")
            );
            let now = store.current_time().await?;
            let command = LifecycleCommand {
                environment_id: after.id,
                kind: EnvironmentOperationKind::Recover,
                expected_revision: after.revision,
                actor_id: after.owner_id,
                trace_id: "upgrade-recover".to_owned(),
                accepted_at: now,
                deadline_at: UtcTimestamp::from_utc(now.get() + time::Duration::minutes(10))?,
                access_revocation_revision: after.operation.access_revocation_revision,
                preserve_mutable_disk: true,
                max_attempts: 3,
                reset_target: None,
            };
            assert!(matches!(
                store
                    .accept_command("upgrade-must-not-rebind", &command)
                    .await,
                Err(EnvironmentStoreError::Lifecycle(
                    LifecycleError::CleanupProtocolUpgradeRequired
                ))
            ));
        } else {
            assert_eq!(after.observed_state, before.observed_state);
            assert_eq!(after.endpoints, before.endpoints);
        }
    }
    assert!(
        store
            .claim_due("upgrade-worker", Duration::from_secs(10))
            .await?
            .is_none()
    );
    Ok(())
}
