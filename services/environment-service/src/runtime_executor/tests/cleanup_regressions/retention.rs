//! Real owner/store/worker journeys, simulating only Kubernetes HTTP.
use super::*;
use contracts::environment::{EnvironmentDiskState, ObservedEnvironmentState};

fn command(
    instance: &EnvironmentInstance,
    kind: EnvironmentOperationKind,
    now: UtcTimestamp,
) -> TestResult<crate::LifecycleCommand> {
    Ok(crate::LifecycleCommand {
        environment_id: instance.id,
        kind,
        expected_revision: instance.revision,
        actor_id: instance.owner_id,
        trace_id: "disk-retention".to_owned(),
        accepted_at: now,
        deadline_at: UtcTimestamp::from_utc(now.get() + time::Duration::minutes(10))?,
        access_revocation_revision: Some(instance.revision),
        preserve_mutable_disk: kind == EnvironmentOperationKind::Restore,
        max_attempts: 3,
        reset_target: None,
    })
}

async fn retained() -> TestResult<Context> {
    let context = Context::start().await?;
    {
        let mut fixture = context
            .mock
            .lifecycle
            .as_ref()
            .ok_or("fixture")?
            .lock()
            .await;
        fixture.deletion = LifecycleDeletion::Immediate;
        let namespace = format!("lw-env-{}", context.initial.id);
        for (name, uid) in [("workspace", 44), ("unrelated-disk", 45)] {
            fixture.objects.insert(format!("/api/v1/namespaces/{namespace}/persistentvolumeclaims/{name}"), json!({
                "metadata":{"name":name,"namespace":namespace,"uid":uuid::Uuid::from_u128(uid),"resourceVersion":"1",
                    "labels":{"labweaver.io/environment-id":context.initial.id,"labweaver.io/project-id":context.initial.project_id,"labweaver.io/managed":"true"}}
            }));
        }
    }
    let current = context.store.load(context.initial.id).await?;
    let delete = command(
        &current,
        EnvironmentOperationKind::Delete,
        context.store.current_time().await?,
    )?;
    let accepted = context
        .store
        .accept_command("retention-delete", &delete)
        .await?;
    assert_eq!(
        context
            .store
            .accept_command("retention-delete", &delete)
            .await?,
        accepted
    );
    context.pin().await?;
    assert!(matches!(
        context
            .worker(context.executor.clone())?
            .run_once("retire", context.store.current_time().await?)
            .await?,
        crate::ReconcileWorkerOutcome::Advanced { terminal: true, .. }
    ));
    let retained = context.store.load(current.id).await?;
    assert_eq!(
        retained.disk_retention.as_ref().ok_or("retention")?.state,
        EnvironmentDiskState::Retained
    );
    let namespace = format!("lw-env-{}", current.id);
    let fixture = context
        .mock
        .lifecycle
        .as_ref()
        .ok_or("fixture")?
        .lock()
        .await;
    assert!(
        fixture.objects.contains_key(&format!(
            "/api/v1/namespaces/{namespace}/persistentvolumeclaims/workspace"
        )),
        "Delete must retain the disk"
    );
    assert!(
        fixture
            .objects
            .contains_key(&format!("/api/v1/namespaces/{namespace}")),
        "retention must not delete a namespace"
    );
    drop(fixture);
    Ok(context)
}

#[tokio::test]
async fn delete_retained_restore_new_generation_and_idempotent_replays() -> TestResult {
    let context = retained().await?;
    let before = context.store.load(context.initial.id).await?;
    let window = before.disk_retention.clone().ok_or("retention")?;
    context
        .worker(context.executor.clone())?
        .run_once("repeat", context.store.current_time().await?)
        .await?;
    assert_eq!(
        context.store.load(before.id).await?.disk_retention,
        Some(window.clone()),
        "a repeated observation must not restart seven days"
    );
    let restore = command(
        &before,
        EnvironmentOperationKind::Restore,
        context.store.current_time().await?,
    )?;
    let receipt = context
        .store
        .accept_command("restore-disk", &restore)
        .await?;
    assert_eq!(
        context
            .store
            .accept_command("restore-disk", &restore)
            .await?,
        receipt
    );
    let restored = context.store.load(before.id).await?;
    assert!(restored.generation > before.generation);
    assert_eq!(restored.observed_state, ObservedEnvironmentState::Stopped);
    let disk = restored.disk_retention.ok_or("restored retention")?;
    assert_eq!(disk.state, EnvironmentDiskState::Restored);
    assert_eq!(disk.started_at, window.started_at);
    assert_eq!(disk.restored_generation, Some(restored.generation));
    assert_eq!(restored.operation.state, OperationState::Succeeded);
    Ok(())
}

async fn expire(context: &Context) -> TestResult<EnvironmentInstance> {
    let mut current = context.store.load(context.initial.id).await?;
    let now = context.store.current_time().await?;
    let disk = current.disk_retention.as_mut().ok_or("retention")?;
    disk.started_at = UtcTimestamp::from_utc(now.get() - time::Duration::days(8))?;
    disk.expires_at = UtcTimestamp::from_utc(disk.started_at.get() + time::Duration::days(7))?;
    disk.compute_retired_at = Some(disk.started_at);
    current.validate()?;
    // Move only the synthetic test clock boundary, not any lifecycle or physical fact.
    sqlx::query("UPDATE environment.environment_instances SET contract=$2 WHERE environment_id=$1")
        .bind(current.id.as_uuid())
        .bind(serde_json::to_value(&current)?)
        .execute(&context.pool)
        .await?;
    Ok(current)
}

#[tokio::test]
async fn expiry_purges_only_target_disk_and_repeated_purge_is_idempotent() -> TestResult {
    let context = retained().await?;
    let before = expire(&context).await?;
    let purge = command(
        &before,
        EnvironmentOperationKind::Purge,
        context.store.current_time().await?,
    )?;
    let receipt = context.store.accept_command("purge-disk", &purge).await?;
    assert_eq!(
        context.store.accept_command("purge-disk", &purge).await?,
        receipt
    );
    let deletes_before = context
        .mock
        .lifecycle
        .as_ref()
        .ok_or("fixture")?
        .lock()
        .await
        .delete_uids
        .len();
    assert!(matches!(
        context
            .worker(context.executor.clone())?
            .run_once("purge", context.store.current_time().await?)
            .await?,
        crate::ReconcileWorkerOutcome::Advanced { terminal: true, .. }
    ));
    let purged = context.store.load(before.id).await?;
    let disk = purged.disk_retention.as_ref().ok_or("purge fact")?;
    assert_eq!(disk.state, EnvironmentDiskState::Purged);
    assert!(disk.purged_at.is_some());
    assert_eq!(
        context.store.accept_command("purge-disk", &purge).await?,
        receipt
    );
    context
        .worker(context.executor.clone())?
        .run_once("repeat-purge", context.store.current_time().await?)
        .await?;
    let fixture = context
        .mock
        .lifecycle
        .as_ref()
        .ok_or("fixture")?
        .lock()
        .await;
    assert_eq!(
        &fixture.delete_uids[deletes_before..],
        &[uuid::Uuid::from_u128(44)]
    );
    let namespace = format!("lw-env-{}", before.id);
    assert!(fixture.objects.contains_key(&format!(
        "/api/v1/namespaces/{namespace}/persistentvolumeclaims/unrelated-disk"
    )));
    assert!(
        fixture
            .objects
            .contains_key(&format!("/api/v1/namespaces/{namespace}"))
    );
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT tablename FROM pg_tables WHERE schemaname='environment' ORDER BY tablename",
    )
    .fetch_all(&context.pool)
    .await?;
    assert!(tables.iter().any(|table| table == "retained_disk_usage"));
    // Purge cannot reach Resource or Evaluation: neither domain is even migrated in this database.
    let usage: (String, bool) = sqlx::query_as("SELECT storage_bytes::text,closed_at IS NOT NULL FROM environment.retained_disk_usage WHERE environment_id=$1 AND started_at=$2")
        .bind(before.id.as_uuid()).bind(disk.started_at.get()).fetch_one(&context.pool).await?;
    assert_eq!(usage.0, before.approved_resources.storage_bytes.to_string());
    assert!(usage.1);
    Ok(())
}

#[tokio::test]
async fn restore_after_purge_and_restore_at_expiry_are_rejected() -> TestResult {
    let context = retained().await?;
    let before = expire(&context).await?;
    let restore = command(
        &before,
        EnvironmentOperationKind::Restore,
        context.store.current_time().await?,
    )?;
    assert!(
        context
            .store
            .accept_command("expired-restore", &restore)
            .await
            .is_err()
    );
    context
        .worker(context.executor.clone())?
        .run_once("expiry-scheduler", context.store.current_time().await?)
        .await?;
    let purged = context.store.load(before.id).await?;
    assert_eq!(
        purged.disk_retention.as_ref().ok_or("purged")?.state,
        EnvironmentDiskState::Purged
    );
    let restore = command(
        &purged,
        EnvironmentOperationKind::Restore,
        context.store.current_time().await?,
    )?;
    assert!(
        context
            .store
            .accept_command("purged-restore", &restore)
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn old_cleanup_cannot_delete_replacement_disk() -> TestResult {
    let context = retained().await?;
    let before = expire(&context).await?;
    let namespace = format!("lw-env-{}", before.id);
    {
        let mut fixture = context
            .mock
            .lifecycle
            .as_ref()
            .ok_or("fixture")?
            .lock()
            .await;
        let disk = fixture
            .objects
            .get_mut(&format!(
                "/api/v1/namespaces/{namespace}/persistentvolumeclaims/workspace"
            ))
            .ok_or("disk")?;
        disk["metadata"]["uid"] = json!(uuid::Uuid::from_u128(99));
        disk["metadata"]["resourceVersion"] = json!("2");
    }
    context
        .worker(context.executor.clone())?
        .run_once("stale-purge", context.store.current_time().await?)
        .await?;
    let fixture = context
        .mock
        .lifecycle
        .as_ref()
        .ok_or("fixture")?
        .lock()
        .await;
    assert!(!fixture.delete_uids.contains(&uuid::Uuid::from_u128(99)));
    assert!(fixture.objects.contains_key(&format!(
        "/api/v1/namespaces/{namespace}/persistentvolumeclaims/workspace"
    )));
    assert_ne!(
        context
            .store
            .load(before.id)
            .await?
            .disk_retention
            .as_ref()
            .ok_or("disk fact")?
            .state,
        EnvironmentDiskState::Purged
    );
    Ok(())
}

#[tokio::test]
async fn old_generation_write_is_uid_fenced_and_current_write_uses_resource_version() -> TestResult
{
    let (fixture, plan, profile) =
        existing_work_fixture(EnvironmentId::new(), contracts::ProjectId::new());
    let name = plan.placement().resource_name("workspace");
    let path = format!(
        "/api/v1/namespaces/{}/persistentvolumeclaims/{name}",
        plan.namespace
    );
    let mut document = fixture.objects.get(&path).ok_or("claim")?.clone();
    document["kind"] = json!("PersistentVolumeClaim");
    document["apiVersion"] = json!("v1");
    let original_uid = document["metadata"]["uid"].clone();
    let resource = ContainerResource {
        kind: "PersistentVolumeClaim".to_owned(),
        namespace: Some(plan.namespace.clone()),
        name,
        document,
    };
    let mock = spawn_lifecycle_kubernetes(Some(fixture)).await?;
    let (_files, executor) = test_kubevirt_executor(&mock).await?;
    let executor = executor.with_namespace_profile(profile);
    {
        let mut fixture = mock.lifecycle.as_ref().ok_or("fixture")?.lock().await;
        let current = fixture.objects.get_mut(&path).ok_or("claim")?;
        current["metadata"]["uid"] = json!(uuid::Uuid::from_u128(888));
        current["metadata"]["resourceVersion"] = json!("new-revision");
    }
    assert!(executor.apply_resource(&plan, &resource).await.is_err());
    assert!(
        !mock
            .events
            .lock()
            .await
            .iter()
            .any(|event| event.starts_with("PATCH ") || event.starts_with("POST "))
    );
    {
        let mut fixture = mock.lifecycle.as_ref().ok_or("fixture")?.lock().await;
        fixture.objects.get_mut(&path).ok_or("claim")?["metadata"]["uid"] = original_uid;
    }
    // The Kubernetes fixture accepts PATCH only with both the live UID and resourceVersion.
    executor
        .apply_resource(&plan, &resource)
        .await
        .map_err(|error| format!("{error:?}"))?;
    Ok(())
}

#[tokio::test]
async fn repeated_retirement_cannot_delete_restored_generation() -> TestResult {
    let context = retained().await?;
    let before = context.store.load(context.initial.id).await?;
    let restore = command(
        &before,
        EnvironmentOperationKind::Restore,
        context.store.current_time().await?,
    )?;
    context
        .store
        .accept_command("restore-new-compute", &restore)
        .await?;
    let namespace = format!("lw-env-{}", before.id);
    let path = format!("/apis/apps/v1/namespaces/{namespace}/deployments/runtime");
    {
        let mut fixture = context
            .mock
            .lifecycle
            .as_ref()
            .ok_or("fixture")?
            .lock()
            .await;
        fixture.objects.insert(path.clone(), json!({"metadata":{"name":"runtime","namespace":namespace,"uid":uuid::Uuid::from_u128(777),"resourceVersion":"20","labels":{"labweaver.io/environment-id":before.id,"labweaver.io/project-id":before.project_id,"labweaver.io/managed":"true"}}}));
    }
    let plan = ContainerResourcePlan {
        environment_id: before.id,
        project_id: before.project_id,
        namespace,
        namespace_uid: None,
        image: String::new(),
        resources: Vec::new(),
        plan_sha256: Sha256Digest::of_bytes(b"retention-cleanup"),
    };
    let result = context
        .executor
        .delete_namespace(
            &container_test_fence(before.id),
            &plan,
            before.operation.cleanup_target.as_ref(),
        )
        .await;
    assert!(result.is_err());
    let fixture = context
        .mock
        .lifecycle
        .as_ref()
        .ok_or("fixture")?
        .lock()
        .await;
    assert!(fixture.objects.contains_key(&path));
    assert!(!fixture.delete_uids.contains(&uuid::Uuid::from_u128(777)));
    assert!(!fixture.delete_uids.contains(&uuid::Uuid::from_u128(44)));
    Ok(())
}
