//! HTTP recovery uses the production router, revoker, and `PostgreSQL` owner.
use super::*;
use contracts::environment::{
    EnvironmentCleanupTarget, EnvironmentDiskState, ObservedEnvironmentState, OperationState,
};

async fn seed_disk_case(
    store: &PgEnvironmentStore,
    pool: &sqlx::PgPool,
    mode: &str,
) -> Result<EnvironmentInstance, Box<dyn Error>> {
    let mut instance = requested_instance();
    instance.course_id = None;
    store
        .create(&format!("http-disk-create-{mode}"), &instance)
        .await?;
    let now = store.current_time().await?;
    let command = contracts::environment::EnvironmentLifecycleCommand {
        environment_id: instance.id,
        kind: EnvironmentOperationKind::Delete,
        expected_revision: instance.revision,
        actor_id: instance.owner_id,
        trace_id: "http-retention".to_owned(),
        accepted_at: now,
        deadline_at: UtcTimestamp::from_utc(now.get() + time::Duration::minutes(10))?,
        access_revocation_revision: Some(instance.revision),
        preserve_mutable_disk: false,
        max_attempts: 3,
        reset_target: None,
    };
    store
        .accept_command(&format!("http-disk-delete-{mode}"), &command)
        .await?;
    instance = store.load(instance.id).await?;
    instance.observed_state = ObservedEnvironmentState::Deleted;
    instance.observed_generation = instance.generation;
    instance.operation.state = OperationState::Succeeded;
    instance.operation.cleanup_target = Some(EnvironmentCleanupTarget {
        namespace: format!("lw-env-{}", instance.id),
        uid: uuid::Uuid::from_u128(901),
        retain_disk: true,
        purge_disk: false,
        objects: vec![contracts::execution::ExecutionObjectRef {
            api_version: "v1".to_owned(),
            resource: "persistentvolumeclaims".to_owned(),
            name: "workspace".to_owned(),
            uid: uuid::Uuid::from_u128(902).to_string(),
        }],
    });
    instance.cleanup_evidence = Some(contracts::ArtifactRef {
        artifact_id: contracts::ArtifactId::new(),
        store_binding: "http-retention-fixture".to_owned(),
        object_version: "verified-retirement".to_owned(),
        size_bytes: 1,
        media_type: "application/json".to_owned(),
    });
    let disk = instance.disk_retention.as_mut().ok_or("retention")?;
    disk.state = EnvironmentDiskState::Retained;
    disk.started_at = now;
    disk.expires_at = UtcTimestamp::from_utc(now.get() + time::Duration::days(7))?;
    disk.compute_retired_at = Some(now);
    if matches!(mode, "expired" | "purged" | "failed-purge") {
        disk.started_at = UtcTimestamp::from_utc(now.get() - time::Duration::days(8))?;
        disk.expires_at = UtcTimestamp::from_utc(disk.started_at.get() + time::Duration::days(7))?;
        disk.compute_retired_at = Some(disk.started_at);
    }
    if mode == "purged" {
        disk.state = EnvironmentDiskState::Purged;
        disk.purged_at = Some(now);
    }
    if matches!(mode, "failed-delete" | "failed-purge") {
        disk.state = if mode == "failed-delete" {
            EnvironmentDiskState::Retiring
        } else {
            EnvironmentDiskState::Purging
        };
        if mode == "failed-delete" {
            disk.compute_retired_at = None;
        }
        instance.operation.kind = if mode == "failed-delete" {
            EnvironmentOperationKind::Delete
        } else {
            EnvironmentOperationKind::Purge
        };
        instance
            .operation
            .cleanup_target
            .as_mut()
            .ok_or("target")?
            .purge_disk = mode == "failed-purge";
        instance.operation.state = OperationState::Failed;
        instance.operation.diagnostic_code =
            Some("LW_ENVIRONMENT_PROVIDER_CLEANUP_FAILED".to_owned());
        instance.last_diagnostic_code = instance.operation.diagnostic_code.clone();
        instance.observed_state = ObservedEnvironmentState::Failed;
        instance.failed_phase = Some(ObservedEnvironmentState::Deleting);
        instance.cleanup_evidence = None;
    }
    instance.validate()?;
    sqlx::query("UPDATE environment.environment_instances SET contract=$2,observed_state=$3,observed_generation=$4,failed_phase=$5 WHERE environment_id=$1")
        .bind(instance.id.as_uuid()).bind(serde_json::to_value(&instance)?)
        .bind(serde_json::to_value(instance.observed_state)?.as_str().ok_or("state")?)
        .bind(i64::try_from(instance.observed_generation)?).bind(instance.failed_phase.map(|_| "deleting"))
        .execute(pool).await?;
    sqlx::query(
        "UPDATE environment.environment_operations SET contract=$2,state=$3 WHERE operation_id=$1",
    )
    .bind(instance.operation.id.as_uuid())
    .bind(serde_json::to_value(&instance.operation)?)
    .bind(
        serde_json::to_value(instance.operation.state)?
            .as_str()
            .ok_or("state")?,
    )
    .execute(pool)
    .await?;
    Ok(instance)
}

pub(super) async fn recovery_http_cases(
    api: &Router,
    store: &PgEnvironmentStore,
    pool: &sqlx::PgPool,
    identity: &auth::ServiceIdentity,
    session: uuid::Uuid,
    revocations: &AtomicUsize,
) -> Result<(), Box<dyn Error>> {
    for mode in [
        "retained",
        "expired",
        "purged",
        "failed-delete",
        "failed-purge",
    ] {
        let before = seed_disk_case(store, pool, mode).await?;
        let uri = format!("/api/v1/environments/{}/recover", before.id);
        let key = format!("http-disk-recover-{mode}");
        let revocations_before = revocations.load(Ordering::SeqCst);
        let response = send(
            api,
            Method::POST,
            uri.clone(),
            identity,
            before.owner_id,
            session,
            Some(before.revision),
            None,
            Some(&key),
        )
        .await?;
        if matches!(mode, "expired" | "purged") {
            assert_eq!(response.0, StatusCode::CONFLICT, "{mode}: {:?}", response.1);
            assert_eq!(
                response.1["diagnosticCode"],
                if mode == "expired" {
                    "LW_ENVIRONMENT_DISK_RETENTION_EXPIRED"
                } else {
                    "LW_ENVIRONMENT_DISK_NOT_RETAINED"
                }
            );
            assert_eq!(store.load(before.id).await?.revision, before.revision);
            continue;
        }
        // This fixture has no Resource lease, so restore has nothing to re-fund here. The
        // signed HTTP/PG restore-funding journey is covered by Control's production-role tests.
        assert_eq!(response.0, StatusCode::ACCEPTED, "{mode}: {:?}", response.1);
        let after = store.load(before.id).await?;
        assert_eq!(
            after.operation.cleanup_target, before.operation.cleanup_target,
            "the original UID checkpoint must survive {mode}"
        );
        assert_eq!(
            after.operation.kind,
            if mode == "retained" {
                EnvironmentOperationKind::Restore
            } else {
                EnvironmentOperationKind::Recover
            }
        );
        if mode == "retained" {
            assert_eq!(after.observed_state, ObservedEnvironmentState::Stopped);
            assert!(after.generation > before.generation);
            assert_eq!(
                revocations.load(Ordering::SeqCst),
                revocations_before + 1,
                "HTTP restore must traverse the real revoker"
            );
        } else {
            assert_eq!(after.observed_state, ObservedEnvironmentState::Deleting);
            assert_eq!(
                after.disk_retention.as_ref().ok_or("disk")?.state,
                before.disk_retention.as_ref().ok_or("disk")?.state
            );
        }
        let count = revocations.load(Ordering::SeqCst);
        let replay = send(
            api,
            Method::POST,
            uri,
            identity,
            before.owner_id,
            session,
            Some(before.revision),
            None,
            Some(&key),
        )
        .await?;
        assert_eq!(
            replay, response,
            "{mode} replay must return the original receipt"
        );
        assert_eq!(revocations.load(Ordering::SeqCst), count);
    }
    Ok(())
}
