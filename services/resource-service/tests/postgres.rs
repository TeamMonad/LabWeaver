//! PostgreSQL evidence for Resource authority schema and pending Lease semantics.

#![allow(
    clippy::doc_markdown,
    clippy::expect_used,
    clippy::too_many_lines,
    reason = "integration fixtures intentionally keep the full transactional scenario visible"
)]

use std::time::Duration;

use sqlx::postgres::PgPoolOptions;
use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;
use uuid::Uuid;

use contracts::http::{
    CreateResourceRateRequest, RecordResourceUsageRequest, UpsertResourceBudgetRequest,
};
use contracts::resource::{
    CapacityClaim, FixedDecimal, GpuAllocationMode, GpuCatalogEntry, GpuRequest, Money,
    ResourceApproval, ResourceBillingUnit, ResourceRequest, ResourceRequestState, ResourceTarget,
    ResourceUsageKind, ResourceUsageQuantities, UsageMeasurement, WorkloadResources,
};
use contracts::{
    ActorId, CapacityClaimId, CourseId, EnvironmentId, GpuCatalogEntryId, LeaseId, ProjectId,
    ReleaseId, ResourceApprovalId, ResourceRequestId, Revision, TaskRunId, UtcTimestamp,
};
use resource_service::ApprovalPolicy;
use resource_service::LifecycleError;
use resource_service::outbox::{ResourceOutboxDispatcher, ResourceOutboxOutcome};
use resource_service::store::{PendingAllocation, PgResourceStore};
use testcontainers::GenericImage;
use testcontainers::core::{IntoContainerPort, WaitFor};

#[tokio::test]
async fn resource_migrations_preserve_pending_terminal_lease_and_claim_quota_invariants()
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
    sqlx::raw_sql(&format!(
        "CREATE SCHEMA resource; SET search_path TO resource;\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}",
        include_str!("../../../migrations/resource/0001_platform_baseline.sql"),
        include_str!("../../../migrations/resource/0002_resource_request_capacity_lease.sql"),
        include_str!("../../../migrations/resource/0003_resource_contract_snapshots.sql"),
        include_str!("../../../migrations/resource/0004_resource_claim_quota_resources.sql"),
        include_str!(
            "../../../migrations/resource/0005_resource_lease_pending_terminal_states.sql"
        ),
        include_str!("../../../migrations/resource/0006_resource_lease_reconciliation.sql"),
        include_str!("../../../migrations/resource/0007_resource_outbox_trigger_fix.sql"),
        include_str!("../../../migrations/resource/0008_v3_project_gpu_billing.sql"),
        include_str!("../../../migrations/resource/0009_settlement_retry.sql"),
        include_str!("../../../migrations/resource/0010_task_run_identity.sql"),
        include_str!("../../../migrations/resource/0011_gpu_catalog_pool_uniqueness.sql"),
    ))
    .execute(&pool)
    .await?;

    let request_id = Uuid::now_v7();
    let approval_id = Uuid::now_v7();
    let claim_id = Uuid::now_v7();
    let lease_id = Uuid::now_v7();
    sqlx::query("INSERT INTO resource.resource_requests (request_id,generation,request_key,requester_id,course_id,project_id,environment_id,release_id,release_version,requested_cpu_millicores,requested_memory_bytes,requested_storage_bytes,requested_duration_seconds,state,revision,contract) VALUES ($1,1,'request-1',$2,$3,$4,$5,$6,1,1,1,1,60,'allocating',2,$7)")
        .bind(request_id).bind(Uuid::now_v7()).bind(Uuid::now_v7()).bind(Uuid::now_v7()).bind(Uuid::now_v7()).bind(Uuid::now_v7()).bind(serde_json::json!({"request": "snapshot"})).execute(&pool).await?;
    sqlx::query("INSERT INTO resource.resource_approvals (approval_id,request_id,request_revision,approver_id,provider_binding,approved_cpu_millicores,approved_memory_bytes,approved_storage_bytes,approved_duration_seconds,reason,valid_until,contract) VALUES ($1,$2,1,$3,'kubernetes-standard',1,1,1,60,'approved',now()+interval '1 hour',$4)")
        .bind(approval_id).bind(request_id).bind(Uuid::now_v7()).bind(serde_json::json!({"approval": "snapshot"})).execute(&pool).await?;
    sqlx::query("INSERT INTO resource.capacity_claims (claim_id,request_id,approval_id,provider_binding,state,revision,workload_cpu_millicores,workload_memory_bytes,workload_storage_bytes,quota_cpu_millicores,quota_memory_bytes,quota_storage_bytes,contract) VALUES ($1,$2,$3,'kubernetes-standard','reserved',1,1,1,1,2,2,2,$4)")
        .bind(claim_id).bind(request_id).bind(approval_id).bind(serde_json::json!({"claim": "snapshot"})).execute(&pool).await?;
    sqlx::query("INSERT INTO resource.resource_leases (lease_id,request_id,claim_id,state,revision,contract) VALUES ($1,$2,$3,'revoked',1,$4)")
        .bind(lease_id).bind(request_id).bind(claim_id).bind(serde_json::json!({"lease": "pending-terminal"})).execute(&pool).await?;
    let synced_revision: i64 = sqlx::query_scalar(
        "SELECT lease_synced_revision FROM resource.capacity_claims WHERE claim_id=$1",
    )
    .bind(claim_id)
    .fetch_one(&pool)
    .await?;
    assert_eq!(synced_revision, 0);

    assert!(sqlx::query("INSERT INTO resource.capacity_claims (claim_id,request_id,approval_id,provider_binding,state,revision,workload_cpu_millicores,workload_memory_bytes,workload_storage_bytes,workload_gpu_class,quota_cpu_millicores,quota_memory_bytes,quota_storage_bytes,contract) VALUES ($1,$2,$3,'kubernetes-standard','reserved',1,1,1,1,'gpu-a100',2,2,2,$4)")
        .bind(Uuid::now_v7()).bind(Uuid::now_v7()).bind(Uuid::now_v7()).bind(serde_json::json!({})).execute(&pool).await.is_err());
    Ok(())
}

#[tokio::test]
async fn resource_store_commits_request_approval_claim_lease_and_renewal_as_fenced_authority()
-> Result<(), Box<dyn std::error::Error>> {
    let (_container, pool) = migrated_pool().await?;
    let store = PgResourceStore::new(pool.clone());
    let now = store.current_time().await?;
    let resources = WorkloadResources {
        cpu_millicores: 500,
        memory_bytes: 512 * 1024 * 1024,
        storage_bytes: 1024 * 1024 * 1024,
        gpu: None,
    };
    let request = ResourceRequest {
        id: ResourceRequestId::new(),
        generation: 1,
        request_key: "workbench-1".into(),
        requester_id: ActorId::new(),
        course_id: Some(CourseId::new()),
        project_id: ProjectId::new(),
        target: ResourceTarget::Environment {
            environment_id: EnvironmentId::new(),
            release_id: ReleaseId::new(),
            release_version: 1,
        },
        requested_resources: resources.clone(),
        requested_duration_seconds: 600,
        state: ResourceRequestState::Reviewing,
        revision: Revision::new(1)?,
        created_at: now,
        updated_at: now,
        diagnostic_code: None,
    };
    store
        .create("resource-create-1", &request, "trace-resource-create")
        .await?;
    let approval = ResourceApproval {
        id: ResourceApprovalId::new(),
        request_id: request.id,
        request_revision: Revision::new(1)?,
        approver_id: ActorId::new(),
        provider_binding: "kubernetes-standard".into(),
        approved_resources: resources.clone(),
        approved_duration_seconds: 600,
        reason: "capacity approved".into(),
        valid_until: UtcTimestamp::from_utc(now.get() + time::Duration::days(1))?,
        created_at: now,
    };
    let allocation = PendingAllocation {
        claim: CapacityClaim {
            id: CapacityClaimId::new(),
            request_id: request.id,
            approval_id: approval.id,
            provider_binding: approval.provider_binding.clone(),
            workload_resources: resources.clone(),
            quota_resources: resources,
            gpu_allocation: None,
            state: contracts::resource::CapacityClaimState::Reserved,
            revision: Revision::new(1)?,
        },
        lease_id: LeaseId::new(),
    };
    let allocating = store
        .approve(
            "resource-approve-1",
            request.id,
            &approval,
            &allocation,
            ApprovalPolicy {
                min_duration_seconds: 60,
                max_duration_seconds: 3600,
            },
            "trace-resource-approve",
        )
        .await?;
    assert_eq!(allocating.state, ResourceRequestState::Allocating);
    let provisioning = store
        .claim_next_capacity_shell()
        .await?
        .expect("one reserved capacity claim");
    assert_eq!(
        provisioning.claim.state,
        contracts::resource::CapacityClaimState::Provisioning
    );
    assert!(
        store.claim_next_capacity_shell().await?.is_none(),
        "a fresh provisioning fence must not be reclaimed concurrently"
    );
    sqlx::query(
        "UPDATE resource.capacity_claims SET updated_at=clock_timestamp()-interval '2 minutes' WHERE claim_id=$1",
    )
    .bind(provisioning.claim.id.as_uuid())
    .execute(&pool)
    .await?;
    let recovered = store
        .claim_next_capacity_shell()
        .await?
        .expect("a stale provisioning fence must be reclaimed after restart");
    assert_eq!(
        recovered.claim.state,
        contracts::resource::CapacityClaimState::Provisioning
    );
    assert_eq!(
        recovered.claim.revision.get(),
        provisioning.claim.revision.get() + 2
    );
    let ready = store
        .mark_capacity_shell_ready(
            recovered.claim.id,
            recovered.claim.revision,
            "lw-work-test",
            "namespace-uid",
            "quota-uid",
        )
        .await?;
    assert_eq!(ready.state, contracts::resource::CapacityClaimState::Ready);
    assert!(store.claim_next_capacity_shell().await?.is_none());
    let active_from = store.current_time().await?;
    let active_expires = UtcTimestamp::from_utc(active_from.get() + time::Duration::minutes(10))?;
    let active = store
        .activate_lease(
            allocation.lease_id,
            Revision::new(1)?,
            active_from,
            active_expires,
            approval.approver_id,
            "trace-resource-activate",
        )
        .await?;
    assert_eq!(
        store.load(request.id).await?.state,
        ResourceRequestState::Active
    );
    let handoff = store
        .next_ready_capacity_handoff()
        .await?
        .expect("ready shell remains Resource-owned until Environment acknowledges");
    assert_eq!(handoff.lease.id, active.id);
    let retrying = store
        .retry_or_block_capacity_handoff(
            handoff.claim.id,
            handoff.claim.revision,
            "LW_RESOURCE_ENVIRONMENT_HANDOFF_UNAVAILABLE",
        )
        .await?;
    assert_eq!(
        retrying.state,
        contracts::resource::CapacityClaimState::Ready
    );
    let (step, state, diagnostic): (String, String, String) = sqlx::query_as(
        "SELECT step,state,diagnostic_code FROM resource.capacity_attempts WHERE claim_id=$1 AND attempt=1",
    )
    .bind(handoff.claim.id.as_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(step, "handoff_environment");
    assert_eq!(state, "retry");
    assert_eq!(diagnostic, "LW_RESOURCE_ENVIRONMENT_HANDOFF_UNAVAILABLE");
    assert_eq!(
        store
            .mark_capacity_handed_off(
                retrying.id,
                retrying.revision,
                handoff.lease.id,
                handoff.lease.revision,
            )
            .await?
            .state,
        contracts::resource::CapacityClaimState::HandedOff
    );
    let handoff_synced_revision: i64 = sqlx::query_scalar(
        "SELECT lease_synced_revision FROM resource.capacity_claims WHERE claim_id=$1",
    )
    .bind(retrying.id.as_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        handoff_synced_revision,
        i64::try_from(handoff.lease.revision.get())?
    );
    assert!(store.next_ready_capacity_handoff().await?.is_none());
    let renewed_expires = UtcTimestamp::from_utc(active_from.get() + time::Duration::minutes(15))?;
    let renewed = store
        .renew_lease(
            "resource-renew-1",
            active.id,
            active.revision,
            renewed_expires,
            "resource-renew-trace",
        )
        .await?;
    assert!(renewed.expires_at > active.expires_at);
    assert_eq!(store.load_lease(active.id).await?, renewed);

    let unsynced = store
        .next_unsynced_active_lease()
        .await?
        .expect("a renewed Environment lease needs an owner sync");
    assert_eq!(unsynced.lease.revision, renewed.revision);
    assert_eq!(unsynced.lease_synced_revision, Some(handoff.lease.revision));
    let expiring = store
        .begin_lease_expiry(
            renewed.id,
            renewed.revision,
            Some("researcher requested reclaim".to_owned()),
            approval.approver_id,
            "trace-resource-expire",
        )
        .await?;
    assert_eq!(
        expiring.revision.get(),
        renewed.revision.get() + 1,
        "Resource advances its own release fence when expiry begins"
    );

    // The owner may acknowledge the revision sent before the revoke raced with the
    // Resource update. The durable scalar must retain that exact acknowledged revision.
    store
        .mark_lease_synced(handoff.claim.id, renewed.revision)
        .await?;
    let synced_after_race: i64 = sqlx::query_scalar(
        "SELECT lease_synced_revision FROM resource.capacity_claims WHERE claim_id=$1",
    )
    .bind(handoff.claim.id.as_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(synced_after_race, i64::try_from(renewed.revision.get())?);
    assert!(
        store
            .mark_lease_synced(handoff.claim.id, Revision::new(5)?)
            .await
            .is_err(),
        "an acknowledgement newer than the Resource lease must remain fenced"
    );
    assert!(
        store
            .mark_lease_synced(handoff.claim.id, handoff.lease.revision)
            .await
            .is_err(),
        "a lease acknowledgement cannot roll back"
    );

    let cleanup = store
        .next_lease_cleanup(approval.approver_id)
        .await?
        .expect("the expiring Environment lease remains pending cleanup");
    assert_eq!(cleanup.lease.revision, expiring.revision);
    assert_eq!(cleanup.lease_synced_revision, Some(renewed.revision));
    let releasing = store
        .mark_capacity_releasing(cleanup.claim.id, cleanup.claim.revision)
        .await?;
    assert!(
        store
            .complete_capacity_release(
                cleanup.claim.id,
                releasing.revision,
                cleanup.lease.id,
                renewed.revision,
                approval.approver_id,
                "trace-resource-release-stale-fence",
            )
            .await
            .is_err(),
        "a stale Environment fence cannot complete Resource release"
    );
    let finalized = store
        .complete_capacity_release(
            cleanup.claim.id,
            releasing.revision,
            cleanup.lease.id,
            cleanup.lease.revision,
            approval.approver_id,
            "trace-resource-release",
        )
        .await?;
    assert_eq!(
        finalized.state,
        contracts::resource::ResourceLeaseState::Revoked
    );
    let final_claim_state: String =
        sqlx::query_scalar("SELECT state FROM resource.capacity_claims WHERE claim_id=$1")
            .bind(cleanup.claim.id.as_uuid())
            .fetch_one(&pool)
            .await?;
    assert_eq!(final_claim_state, "released");

    let request_subjects: Vec<String> = sqlx::query_scalar(
        "SELECT subject FROM resource.outbox_events WHERE aggregate_id=$1 ORDER BY aggregate_sequence",
    )
    .bind(request.id.as_uuid())
    .fetch_all(&pool)
    .await?;
    assert!(
        request_subjects
            .iter()
            .any(|subject| { subject == contracts::events::subjects::RESOURCE_REQUEST_SUBMITTED })
    );
    assert!(
        request_subjects
            .iter()
            .any(|subject| { subject == contracts::events::subjects::RESOURCE_REQUEST_APPROVED })
    );
    assert!(
        request_subjects.iter().any(|subject| {
            subject == contracts::events::subjects::RESOURCE_REQUEST_STATE_CHANGED
        })
    );

    let lease_subjects: Vec<String> = sqlx::query_scalar(
        "SELECT subject FROM resource.outbox_events WHERE aggregate_id=$1 ORDER BY aggregate_sequence",
    )
    .bind(active.id.as_uuid())
    .fetch_all(&pool)
    .await?;
    assert!(
        lease_subjects
            .iter()
            .any(|subject| subject == contracts::events::subjects::RESOURCE_LEASE_ACTIVATED)
    );
    assert!(
        lease_subjects
            .iter()
            .any(|subject| subject == contracts::events::subjects::RESOURCE_LEASE_RENEWED)
    );

    let before_failed_renewal: i64 =
        sqlx::query_scalar("SELECT count(*) FROM resource.outbox_events WHERE aggregate_id=$1")
            .bind(active.id.as_uuid())
            .fetch_one(&pool)
            .await?;
    assert!(
        store
            .renew_lease(
                "resource-renew-invalid-revision",
                active.id,
                Revision::new(1)?,
                renewed_expires,
                "resource-renew-invalid-trace",
            )
            .await
            .is_err()
    );
    let after_failed_renewal: i64 =
        sqlx::query_scalar("SELECT count(*) FROM resource.outbox_events WHERE aggregate_id=$1")
            .bind(active.id.as_uuid())
            .fetch_one(&pool)
            .await?;
    assert_eq!(
        before_failed_renewal, after_failed_renewal,
        "a failed fenced transition must not leave an outbox row"
    );
    Ok(())
}

#[tokio::test]
async fn task_resource_lifecycle_returns_owner_scope_and_confirms_fenced_cleanup()
-> Result<(), Box<dyn std::error::Error>> {
    let (_container, pool) = migrated_pool().await?;
    let store = PgResourceStore::new(pool.clone());
    let now = store.current_time().await?;
    let owner_id = ActorId::new();
    let project_id = ProjectId::new();
    let task_run_id = TaskRunId::new();
    let resources = WorkloadResources {
        cpu_millicores: 250,
        memory_bytes: 256 * 1024 * 1024,
        storage_bytes: 512 * 1024 * 1024,
        gpu: None,
    };
    let request = ResourceRequest {
        id: ResourceRequestId::new(),
        generation: 1,
        request_key: "task-resource-1".into(),
        requester_id: owner_id,
        course_id: None,
        project_id,
        target: ResourceTarget::Task { task_run_id },
        requested_resources: resources.clone(),
        requested_duration_seconds: 300,
        state: ResourceRequestState::Reviewing,
        revision: Revision::new(1)?,
        created_at: now,
        updated_at: now,
        diagnostic_code: None,
    };
    store
        .create("task-resource-create-1", &request, "trace-task-create")
        .await?;
    assert_eq!(store.load_task_request(task_run_id).await?, request);
    assert_eq!(
        store
            .create("task-resource-create-1", &request, "trace-task-replay")
            .await?,
        request
    );
    let mut conflicting_request = request.clone();
    conflicting_request.request_key = "task-resource-conflict".into();
    assert!(matches!(
        store
            .create(
                "task-resource-create-conflict",
                &conflicting_request,
                "trace-task-conflict",
            )
            .await,
        Err(resource_service::store::ResourceStoreError::Database(_))
    ));
    let approval = ResourceApproval {
        id: ResourceApprovalId::new(),
        request_id: request.id,
        request_revision: Revision::new(1)?,
        approver_id: ActorId::new(),
        provider_binding: "kubernetes-standard".into(),
        approved_resources: resources.clone(),
        approved_duration_seconds: 60,
        reason: "task approved".into(),
        valid_until: UtcTimestamp::from_utc(now.get() + time::Duration::hours(1))?,
        created_at: now,
    };
    let allocation = PendingAllocation {
        claim: CapacityClaim {
            id: CapacityClaimId::new(),
            request_id: request.id,
            approval_id: approval.id,
            provider_binding: approval.provider_binding.clone(),
            workload_resources: resources.clone(),
            quota_resources: resources,
            gpu_allocation: None,
            state: contracts::resource::CapacityClaimState::Reserved,
            revision: Revision::new(1)?,
        },
        lease_id: LeaseId::new(),
    };
    let _approved = store
        .approve(
            "task-resource-approve-1",
            request.id,
            &approval,
            &allocation,
            ApprovalPolicy {
                min_duration_seconds: 60,
                max_duration_seconds: 3600,
            },
            "trace-task-approve",
        )
        .await?;

    let claimed = store
        .claim_task_resource(task_run_id, "trace-task-claim")
        .await?;
    assert_eq!(claimed.task_run_id, task_run_id);
    assert_eq!(claimed.project_id, project_id);
    assert_eq!(claimed.owner_id, owner_id);
    assert_eq!(
        claimed.claim.state,
        contracts::resource::CapacityClaimState::Provisioning
    );
    assert_eq!(claimed.claim_revision, claimed.claim.revision);
    assert_eq!(claimed.lease_revision, claimed.lease.revision);
    assert!(!claimed.cleanup_confirmed);

    let stale_ack = store
        .acknowledge_task_resource(
            task_run_id,
            Revision::new(1)?,
            claimed.lease_revision,
            "labweaver-evaluation-runs",
            "trace-task-stale-ack",
        )
        .await;
    assert!(matches!(
        stale_ack,
        Err(resource_service::store::ResourceStoreError::CapacityClaimStateConflict)
    ));
    sqlx::query(
        "UPDATE resource.resource_approvals
         SET created_at=clock_timestamp() - interval '1 hour',
             valid_until=clock_timestamp() - interval '1 minute'
         WHERE approval_id=$1",
    )
    .bind(approval.id.as_uuid())
    .execute(&pool)
    .await?;
    let expired_ack = store
        .acknowledge_task_resource(
            task_run_id,
            claimed.claim_revision,
            claimed.lease_revision,
            "labweaver-evaluation-runs",
            "trace-task-expired-ack",
        )
        .await;
    assert!(matches!(
        expired_ack,
        Err(resource_service::store::ResourceStoreError::ApprovalInvalid)
    ));
    sqlx::query(
        "UPDATE resource.resource_approvals
         SET valid_until=clock_timestamp() + interval '1 hour'
         WHERE approval_id=$1",
    )
    .bind(approval.id.as_uuid())
    .execute(&pool)
    .await?;
    let acknowledged = store
        .acknowledge_task_resource(
            task_run_id,
            claimed.claim_revision,
            claimed.lease_revision,
            "labweaver-evaluation-runs",
            "trace-task-ack",
        )
        .await?;
    assert_eq!(acknowledged.task_run_id, task_run_id);
    assert_eq!(acknowledged.project_id, project_id);
    assert_eq!(acknowledged.owner_id, owner_id);
    assert_eq!(
        acknowledged.execution_namespace.as_deref(),
        Some("labweaver-evaluation-runs")
    );
    assert_eq!(
        acknowledged.claim.state,
        contracts::resource::CapacityClaimState::HandedOff
    );
    assert_eq!(acknowledged.claim_revision, acknowledged.claim.revision);
    assert_eq!(acknowledged.lease_revision, acknowledged.lease.revision);
    assert!(!acknowledged.cleanup_confirmed);
    assert_eq!(store.load_task_resource(task_run_id).await?, acknowledged);
    let lease_window = acknowledged
        .lease
        .expires_at
        .expect("active task lease has an expiry")
        .get()
        - acknowledged
            .lease
            .active_from
            .expect("active task lease has a start")
            .get();
    assert!(lease_window <= time::Duration::seconds(60));

    let stale_release = store
        .release_task_resource(
            task_run_id,
            claimed.claim_revision,
            acknowledged.lease_revision,
            "trace-task-stale-release",
        )
        .await;
    assert!(matches!(
        stale_release,
        Err(resource_service::store::ResourceStoreError::CapacityClaimStateConflict)
    ));
    let released = store
        .release_task_resource(
            task_run_id,
            acknowledged.claim_revision,
            acknowledged.lease_revision,
            "trace-task-release",
        )
        .await?;
    assert_eq!(released.task_run_id, task_run_id);
    assert_eq!(released.project_id, project_id);
    assert_eq!(released.owner_id, owner_id);
    assert_eq!(released.request.state, ResourceRequestState::Expired);
    assert_eq!(
        released.claim.state,
        contracts::resource::CapacityClaimState::Released
    );
    assert_eq!(
        released.lease.state,
        contracts::resource::ResourceLeaseState::Revoked
    );
    assert_eq!(released.claim_revision, released.claim.revision);
    assert_eq!(released.lease_revision, released.lease.revision);
    assert!(released.cleanup_confirmed);
    assert_eq!(store.load_task_resource(task_run_id).await?, released);
    Ok(())
}

#[tokio::test]
async fn task_resource_reviewing_cancel_is_idempotent_and_rejects_approval_race()
-> Result<(), Box<dyn std::error::Error>> {
    let (_container, pool) = migrated_pool().await?;
    let store = PgResourceStore::new(pool);
    let now = store.current_time().await?;
    let owner_id = ActorId::new();
    let project_id = ProjectId::new();
    let resources = WorkloadResources {
        cpu_millicores: 250,
        memory_bytes: 256 * 1024 * 1024,
        storage_bytes: 512 * 1024 * 1024,
        gpu: None,
    };
    let make_request = |task_run_id: TaskRunId, request_key: &str| ResourceRequest {
        id: ResourceRequestId::new(),
        generation: 1,
        request_key: request_key.to_owned(),
        requester_id: owner_id,
        course_id: None,
        project_id,
        target: ResourceTarget::Task { task_run_id },
        requested_resources: resources.clone(),
        requested_duration_seconds: 300,
        state: ResourceRequestState::Reviewing,
        revision: Revision::new(1).expect("fixed revision"),
        created_at: now,
        updated_at: now,
        diagnostic_code: None,
    };

    let pending = make_request(TaskRunId::new(), "task-cancel");
    store
        .create("task-cancel-create", &pending, "trace-task-cancel-create")
        .await?;
    let cancelled = store
        .reject_or_cancel(
            "task-cancel-mutation",
            pending.id,
            pending.revision,
            ResourceRequestState::Cancelled,
            owner_id,
            "trace-task-cancel",
        )
        .await?;
    assert_eq!(cancelled.state, ResourceRequestState::Cancelled);
    assert_eq!(cancelled.revision.get(), 2);
    assert_eq!(
        store
            .reject_or_cancel(
                "task-cancel-mutation",
                pending.id,
                pending.revision,
                ResourceRequestState::Cancelled,
                owner_id,
                "trace-task-cancel-replay",
            )
            .await?,
        cancelled,
        "repeating the same cancellation key replays the durable result"
    );

    let approving = make_request(TaskRunId::new(), "task-approval-race");
    store
        .create(
            "task-approval-race-create",
            &approving,
            "trace-task-approval-race-create",
        )
        .await?;
    let approval = ResourceApproval {
        id: ResourceApprovalId::new(),
        request_id: approving.id,
        request_revision: approving.revision,
        approver_id: ActorId::new(),
        provider_binding: "kubernetes-standard".into(),
        approved_resources: resources.clone(),
        approved_duration_seconds: 60,
        reason: "task approval race".into(),
        valid_until: UtcTimestamp::from_utc(now.get() + time::Duration::hours(1))?,
        created_at: now,
    };
    let allocation = PendingAllocation {
        claim: CapacityClaim {
            id: CapacityClaimId::new(),
            request_id: approving.id,
            approval_id: approval.id,
            provider_binding: approval.provider_binding.clone(),
            workload_resources: resources.clone(),
            quota_resources: resources,
            gpu_allocation: None,
            state: contracts::resource::CapacityClaimState::Reserved,
            revision: Revision::new(1)?,
        },
        lease_id: LeaseId::new(),
    };
    store
        .approve(
            "task-approval-race-approve",
            approving.id,
            &approval,
            &allocation,
            ApprovalPolicy {
                min_duration_seconds: 60,
                max_duration_seconds: 3600,
            },
            "trace-task-approval-race-approve",
        )
        .await?;
    assert!(matches!(
        store
            .reject_or_cancel(
                "task-approval-race-cancel",
                approving.id,
                approving.revision,
                ResourceRequestState::Cancelled,
                owner_id,
                "trace-task-approval-race-cancel",
            )
            .await,
        Err(resource_service::store::ResourceStoreError::Lifecycle(
            LifecycleError::StateConflict
        ))
    ));
    Ok(())
}

#[tokio::test]
async fn gpu_catalog_rejects_cross_mode_binding_collision() -> Result<(), Box<dyn std::error::Error>>
{
    let (_container, pool) = migrated_pool().await?;
    let store = PgResourceStore::new(pool);
    let exclusive = GpuCatalogEntry {
        id: GpuCatalogEntryId::new(),
        class: "a100-exclusive".into(),
        mode: GpuAllocationMode::Exclusive,
        provider_binding: "kubernetes-standard".into(),
        capacity_units: 4,
        allocation_binding: "nvidia.com/gpu".into(),
        revision: Revision::new(1)?,
        active: true,
    };
    store
        .create_gpu_catalog_entry("gpu-catalog-exclusive-1", &exclusive)
        .await?;
    let shared = GpuCatalogEntry {
        id: GpuCatalogEntryId::new(),
        class: "a100-shared".into(),
        mode: GpuAllocationMode::ContainerTimeSlice,
        provider_binding: exclusive.provider_binding.clone(),
        capacity_units: 8,
        allocation_binding: exclusive.allocation_binding.clone(),
        revision: Revision::new(1)?,
        active: true,
    };
    let result = store
        .create_gpu_catalog_entry("gpu-catalog-shared-1", &shared)
        .await;
    assert!(matches!(
        result,
        Err(resource_service::store::ResourceStoreError::GpuCatalogModeCollision)
    ));
    Ok(())
}

#[tokio::test]
async fn gpu_catalog_rejects_cross_provider_active_alias() -> Result<(), Box<dyn std::error::Error>>
{
    let (_container, pool) = migrated_pool().await?;
    let store = PgResourceStore::new(pool);
    let first = GpuCatalogEntry {
        id: GpuCatalogEntryId::new(),
        class: "a100-provider-one".into(),
        mode: GpuAllocationMode::Exclusive,
        provider_binding: "kubernetes-one".into(),
        capacity_units: 4,
        allocation_binding: "nvidia.com/gpu".into(),
        revision: Revision::new(1)?,
        active: true,
    };
    store
        .create_gpu_catalog_entry("gpu-catalog-provider-one", &first)
        .await?;
    let alias = GpuCatalogEntry {
        id: GpuCatalogEntryId::new(),
        class: "a100-provider-two".into(),
        mode: GpuAllocationMode::Exclusive,
        provider_binding: "kubernetes-two".into(),
        capacity_units: 4,
        allocation_binding: first.allocation_binding.clone(),
        revision: Revision::new(1)?,
        active: true,
    };
    let result = store
        .create_gpu_catalog_entry("gpu-catalog-provider-two", &alias)
        .await;
    assert!(matches!(
        result,
        Err(resource_service::store::ResourceStoreError::GpuCatalogPoolCollision)
    ));
    Ok(())
}

#[tokio::test]
async fn gpu_time_slice_approval_rejects_count_above_one_with_stable_catalog_error()
-> Result<(), Box<dyn std::error::Error>> {
    let (_container, pool) = migrated_pool().await?;
    let store = PgResourceStore::new(pool);
    let now = store.current_time().await?;
    let project_id = ProjectId::new();
    let course_id = Some(CourseId::new());
    let provider_binding = "kubernetes-standard".to_owned();
    let resources = WorkloadResources {
        cpu_millicores: 1,
        memory_bytes: 1,
        storage_bytes: 1,
        gpu: Some(GpuRequest {
            class: "a100-shared".to_owned(),
            count: 2,
        }),
    };
    let catalog = GpuCatalogEntry {
        id: GpuCatalogEntryId::new(),
        class: "a100-shared".to_owned(),
        mode: GpuAllocationMode::ContainerTimeSlice,
        provider_binding: provider_binding.clone(),
        capacity_units: 8,
        allocation_binding: "nvidia.com/gpu".to_owned(),
        revision: Revision::new(1)?,
        active: true,
    };
    store
        .create_gpu_catalog_entry("gpu-time-slice-catalog", &catalog)
        .await?;
    store
        .record_gpu_capacity_observation(
            catalog.id,
            catalog.capacity_units,
            "test-observer",
            now,
            UtcTimestamp::from_utc(now.get() + time::Duration::hours(1))?,
        )
        .await?;

    let request = ResourceRequest {
        id: ResourceRequestId::new(),
        generation: 1,
        request_key: "time-slice-count".to_owned(),
        requester_id: ActorId::new(),
        course_id,
        project_id,
        target: ResourceTarget::Environment {
            environment_id: EnvironmentId::new(),
            release_id: ReleaseId::new(),
            release_version: 1,
        },
        requested_resources: resources.clone(),
        requested_duration_seconds: 60,
        state: ResourceRequestState::Reviewing,
        revision: Revision::new(1)?,
        created_at: now,
        updated_at: now,
        diagnostic_code: None,
    };
    store
        .create("gpu-time-slice-request", &request, "gpu-time-slice-create")
        .await?;
    let approval = ResourceApproval {
        id: ResourceApprovalId::new(),
        request_id: request.id,
        request_revision: request.revision,
        approver_id: ActorId::new(),
        provider_binding,
        approved_resources: resources.clone(),
        approved_duration_seconds: 60,
        reason: "time slice count regression".to_owned(),
        valid_until: UtcTimestamp::from_utc(now.get() + time::Duration::hours(1))?,
        created_at: now,
    };
    let allocation = PendingAllocation {
        claim: CapacityClaim {
            id: CapacityClaimId::new(),
            request_id: request.id,
            approval_id: approval.id,
            provider_binding: approval.provider_binding.clone(),
            workload_resources: resources.clone(),
            quota_resources: resources,
            gpu_allocation: None,
            state: contracts::resource::CapacityClaimState::Reserved,
            revision: Revision::new(1)?,
        },
        lease_id: LeaseId::new(),
    };
    let result = store
        .approve(
            "gpu-time-slice-approve",
            request.id,
            &approval,
            &allocation,
            ApprovalPolicy {
                min_duration_seconds: 60,
                max_duration_seconds: 3600,
            },
            "gpu-time-slice-approve-trace",
        )
        .await;
    assert!(
        matches!(
            &result,
            Err(resource_service::store::ResourceStoreError::GpuCatalogInvalid)
        ),
        "unexpected result: {result:?}"
    );
    Ok(())
}

#[tokio::test]
async fn gpu_reservation_counts_across_catalog_revisions_for_one_physical_pool()
-> Result<(), Box<dyn std::error::Error>> {
    let (_container, pool) = migrated_pool().await?;
    let store = PgResourceStore::new(pool);
    let now = store.current_time().await?;
    let project_id = ProjectId::new();
    let provider_binding = "kubernetes-standard".to_owned();
    let allocation_binding = "nvidia.com/gpu".to_owned();
    let first_catalog = GpuCatalogEntry {
        id: GpuCatalogEntryId::new(),
        class: "a100-exclusive".to_owned(),
        mode: GpuAllocationMode::Exclusive,
        provider_binding: provider_binding.clone(),
        capacity_units: 1,
        allocation_binding: allocation_binding.clone(),
        revision: Revision::new(1)?,
        active: true,
    };
    store
        .create_gpu_catalog_entry("gpu-revision-catalog-1", &first_catalog)
        .await?;
    store
        .record_gpu_capacity_observation(
            first_catalog.id,
            1,
            "test-observer",
            now,
            UtcTimestamp::from_utc(now.get() + time::Duration::hours(1))?,
        )
        .await?;

    let (first_request, first_approval, first_allocation) = gpu_bundle(
        now,
        project_id,
        "gpu-revision-request-1",
        "a100-exclusive",
        1,
    );
    store
        .create(
            "gpu-revision-create-1",
            &first_request,
            "gpu-revision-trace-1",
        )
        .await?;
    store
        .approve(
            "gpu-revision-approve-1",
            first_request.id,
            &first_approval,
            &first_allocation,
            ApprovalPolicy {
                min_duration_seconds: 60,
                max_duration_seconds: 3600,
            },
            "gpu-revision-approve-trace-1",
        )
        .await?;

    let remapped_catalog = GpuCatalogEntry {
        id: GpuCatalogEntryId::new(),
        class: first_catalog.class.clone(),
        mode: first_catalog.mode,
        provider_binding: "kubernetes-other".to_owned(),
        capacity_units: 1,
        allocation_binding: first_catalog.allocation_binding.clone(),
        revision: Revision::new(2)?,
        active: true,
    };
    let remapped = store
        .create_gpu_catalog_entry("gpu-revision-catalog-remapped", &remapped_catalog)
        .await;
    assert!(matches!(
        remapped,
        Err(resource_service::store::ResourceStoreError::GpuCatalogMappingConflict)
    ));

    let second_catalog = GpuCatalogEntry {
        id: GpuCatalogEntryId::new(),
        class: first_catalog.class.clone(),
        mode: first_catalog.mode,
        provider_binding,
        capacity_units: 1,
        allocation_binding,
        revision: Revision::new(2)?,
        active: true,
    };
    store
        .create_gpu_catalog_entry("gpu-revision-catalog-2", &second_catalog)
        .await?;
    store
        .record_gpu_capacity_observation(
            second_catalog.id,
            1,
            "test-observer",
            now,
            UtcTimestamp::from_utc(now.get() + time::Duration::hours(1))?,
        )
        .await?;

    let (second_request, second_approval, second_allocation) = gpu_bundle(
        now,
        project_id,
        "gpu-revision-request-2",
        "a100-exclusive",
        1,
    );
    store
        .create(
            "gpu-revision-create-2",
            &second_request,
            "gpu-revision-trace-2",
        )
        .await?;
    let result = store
        .approve(
            "gpu-revision-approve-2",
            second_request.id,
            &second_approval,
            &second_allocation,
            ApprovalPolicy {
                min_duration_seconds: 60,
                max_duration_seconds: 3600,
            },
            "gpu-revision-approve-trace-2",
        )
        .await;
    assert!(matches!(
        result,
        Err(resource_service::store::ResourceStoreError::GpuCapacityExhausted)
    ));
    Ok(())
}

#[tokio::test]
async fn concurrent_gpu_approvals_serialize_on_the_shared_admission_lock()
-> Result<(), Box<dyn std::error::Error>> {
    let (_container, pool) = migrated_pool().await?;
    let store = PgResourceStore::new(pool);
    let now = store.current_time().await?;
    let catalog = GpuCatalogEntry {
        id: GpuCatalogEntryId::new(),
        class: "a100-exclusive".to_owned(),
        mode: GpuAllocationMode::Exclusive,
        provider_binding: "kubernetes-standard".to_owned(),
        capacity_units: 1,
        allocation_binding: "nvidia.com/gpu".to_owned(),
        revision: Revision::new(1)?,
        active: true,
    };
    store
        .create_gpu_catalog_entry("gpu-concurrent-catalog", &catalog)
        .await?;
    store
        .record_gpu_capacity_observation(
            catalog.id,
            1,
            "test-observer",
            now,
            UtcTimestamp::from_utc(now.get() + time::Duration::hours(1))?,
        )
        .await?;
    let project_id = ProjectId::new();
    let (first_request, first_approval, first_allocation) = gpu_bundle(
        now,
        project_id,
        "gpu-concurrent-request-1",
        "a100-exclusive",
        1,
    );
    let (second_request, second_approval, second_allocation) = gpu_bundle(
        now,
        project_id,
        "gpu-concurrent-request-2",
        "a100-exclusive",
        1,
    );
    store
        .create(
            "gpu-concurrent-create-1",
            &first_request,
            "gpu-concurrent-trace-1",
        )
        .await?;
    store
        .create(
            "gpu-concurrent-create-2",
            &second_request,
            "gpu-concurrent-trace-2",
        )
        .await?;

    let first_store = store.clone();
    let second_store = store.clone();
    let first = first_store.approve(
        "gpu-concurrent-approve-1",
        first_request.id,
        &first_approval,
        &first_allocation,
        ApprovalPolicy {
            min_duration_seconds: 60,
            max_duration_seconds: 3600,
        },
        "gpu-concurrent-approve-trace-1",
    );
    let second = second_store.approve(
        "gpu-concurrent-approve-2",
        second_request.id,
        &second_approval,
        &second_allocation,
        ApprovalPolicy {
            min_duration_seconds: 60,
            max_duration_seconds: 3600,
        },
        "gpu-concurrent-approve-trace-2",
    );
    let (first, second) = tokio::join!(first, second);
    assert!(first.is_ok() ^ second.is_ok());
    assert!(
        matches!(
            &first,
            Err(resource_service::store::ResourceStoreError::GpuCapacityExhausted)
        ) || matches!(
            &second,
            Err(resource_service::store::ResourceStoreError::GpuCapacityExhausted)
        )
    );
    Ok(())
}

#[tokio::test]
async fn resource_billing_uses_valid_leases_and_is_idempotent_across_rates_unknowns_and_adjustments()
-> Result<(), Box<dyn std::error::Error>> {
    let (_container, pool) = migrated_pool().await?;
    let store = PgResourceStore::new(pool.clone());
    let project_id = ProjectId::new();
    let actor_id = ActorId::new();
    let now = store.current_time().await?;

    let budget = store
        .upsert_budget(
            "billing-budget-1",
            &UpsertResourceBudgetRequest {
                project_id,
                course_id: None,
                limit: Money {
                    currency: "USD".to_owned(),
                    amount: FixedDecimal::parse("10.000000")?,
                },
                warning_at: Money {
                    currency: "USD".to_owned(),
                    amount: FixedDecimal::parse("5.000000")?,
                },
            },
            now,
        )
        .await?;
    assert_eq!(budget.spent.amount.as_str(), "0.000000");

    let (request, lease) =
        create_active_request(&store, now, project_id, actor_id, "billing-workload").await?;
    assert_eq!(lease.state, contracts::resource::ResourceLeaseState::Active);

    let boundary = UtcTimestamp::from_utc(now.get() + time::Duration::minutes(1))?;
    let end = UtcTimestamp::from_utc(now.get() + time::Duration::minutes(2))?;
    let first_rate = store
        .create_rate(
            "billing-rate-cpu-1",
            &CreateResourceRateRequest {
                unit: ResourceBillingUnit::CpuMillicoreSecond,
                unit_quantity: 1,
                gpu_class: None,
                gpu_mode: None,
                unit_price: Money {
                    currency: "USD".to_owned(),
                    amount: FixedDecimal::parse("0.100000")?,
                },
                effective_from: now,
                effective_until: None,
            },
        )
        .await?;

    let first_input = RecordResourceUsageRequest {
        project_id,
        course_id: None,
        kind: ResourceUsageKind::Compute,
        request_id: request.id,
        lease_id: Some(lease.id),
        source_event_id: contracts::EventId::new(),
        measured_from: now,
        measured_until: boundary,
        measurement: UsageMeasurement::Known {
            quantities: ResourceUsageQuantities {
                cpu_millicore_seconds: 10,
                memory_byte_seconds: 0,
                storage_byte_seconds: 0,
                gpu_unit_seconds: 0,
            },
        },
    };
    let first_usage = store.record_usage(&first_input, boundary).await?;
    let replayed_usage = store.record_usage(&first_input, boundary).await?;
    assert_eq!(replayed_usage.id, first_usage.id);

    let second_rate = store
        .create_rate(
            "billing-rate-cpu-2",
            &CreateResourceRateRequest {
                unit: ResourceBillingUnit::CpuMillicoreSecond,
                unit_quantity: 1,
                gpu_class: None,
                gpu_mode: None,
                unit_price: Money {
                    currency: "USD".to_owned(),
                    amount: FixedDecimal::parse("0.200000")?,
                },
                effective_from: boundary,
                effective_until: None,
            },
        )
        .await?;

    let second_input = RecordResourceUsageRequest {
        source_event_id: contracts::EventId::new(),
        measured_from: boundary,
        measured_until: end,
        measurement: UsageMeasurement::Known {
            quantities: ResourceUsageQuantities {
                cpu_millicore_seconds: 5,
                memory_byte_seconds: 0,
                storage_byte_seconds: 0,
                gpu_unit_seconds: 0,
            },
        },
        ..first_input.clone()
    };
    let second_usage = store.record_usage(&second_input, end).await?;
    let (crossing_request, crossing_lease) = create_active_request(
        &store,
        now,
        project_id,
        actor_id,
        "billing-crossing-workload",
    )
    .await?;
    let crossing_input = RecordResourceUsageRequest {
        request_id: crossing_request.id,
        lease_id: Some(crossing_lease.id),
        source_event_id: contracts::EventId::new(),
        measured_from: now,
        measured_until: end,
        measurement: UsageMeasurement::Known {
            quantities: ResourceUsageQuantities {
                cpu_millicore_seconds: 10,
                memory_byte_seconds: 0,
                storage_byte_seconds: 0,
                gpu_unit_seconds: 0,
            },
        },
        ..first_input.clone()
    };
    let crossing_usage = store.record_usage(&crossing_input, end).await?;
    let first_charge = store
        .settle_usage(first_usage.id)
        .await
        .map_err(|error| format!("first settle: {error}"))?
        .ok_or("first known usage was not priced")?;
    let first_charge_replay = store
        .settle_usage(first_usage.id)
        .await
        .map_err(|error| format!("first replay settle: {error}"))?
        .ok_or("first charge replay was not returned")?;
    let second_charge = store
        .settle_usage(second_usage.id)
        .await
        .map_err(|error| format!("second settle: {error}"))?
        .ok_or("successor usage was not priced")?;
    assert_eq!(first_charge, first_charge_replay);
    assert_eq!(first_charge.total.amount.as_str(), "1.000000");
    assert_eq!(second_charge.total.amount.as_str(), "1.000000");
    assert_eq!(first_charge.lines[0].rate_id, first_rate.id);
    assert_eq!(second_charge.lines[0].rate_id, second_rate.id);
    let crossing_charge = store
        .settle_usage(crossing_usage.id)
        .await
        .map_err(|error| format!("crossing settle: {error}"))?
        .ok_or("crossing usage was not priced")?;
    assert_eq!(crossing_charge.lines.len(), 2);
    assert_eq!(crossing_charge.lines[0].rate_id, first_rate.id);
    assert_eq!(crossing_charge.lines[0].quantity, 5);
    assert_eq!(crossing_charge.lines[0].amount.amount.as_str(), "0.500000");
    assert_eq!(crossing_charge.lines[1].rate_id, second_rate.id);
    assert_eq!(crossing_charge.lines[1].quantity, 5);
    assert_eq!(crossing_charge.lines[1].amount.amount.as_str(), "1.000000");
    assert_eq!(crossing_charge.total.amount.as_str(), "1.500000");
    let base_charge_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM resource.resource_charges
         WHERE usage_record_id=$1 AND adjustment_of IS NULL",
    )
    .bind(first_usage.id.as_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(base_charge_count, 1);

    let unknown_input = RecordResourceUsageRequest {
        source_event_id: contracts::EventId::new(),
        kind: ResourceUsageKind::Storage,
        measured_from: end,
        measured_until: UtcTimestamp::from_utc(end.get() + time::Duration::minutes(1))?,
        measurement: UsageMeasurement::Unknown {
            reason: "provider did not report retained storage".to_owned(),
        },
        ..first_input.clone()
    };
    let unknown = store
        .record_usage(&unknown_input, unknown_input.measured_until)
        .await?;
    assert_eq!(
        unknown.settlement,
        contracts::resource::UsageSettlementState::Unsettled
    );
    assert!(
        store
            .settle_usage(unknown.id)
            .await
            .map_err(|error| format!("unknown settle: {error}"))?
            .is_none()
    );
    let unknown_charge_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM resource.resource_charges WHERE usage_record_id=$1",
    )
    .bind(unknown.id.as_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(unknown_charge_count, 0);

    let unconfigured_input = RecordResourceUsageRequest {
        source_event_id: contracts::EventId::new(),
        kind: ResourceUsageKind::Storage,
        measured_from: unknown_input.measured_until,
        measured_until: UtcTimestamp::from_utc(
            unknown_input.measured_until.get() + time::Duration::minutes(1),
        )?,
        measurement: UsageMeasurement::Known {
            quantities: ResourceUsageQuantities {
                cpu_millicore_seconds: 0,
                memory_byte_seconds: 0,
                storage_byte_seconds: 7,
                gpu_unit_seconds: 0,
            },
        },
        ..first_input.clone()
    };
    let unconfigured = store
        .record_usage(&unconfigured_input, unconfigured_input.measured_until)
        .await?;
    assert!(
        store
            .settle_pending_usage_once()
            .await
            .map_err(|error| format!("pending settle: {error}"))?
    );
    let unconfigured_settlement: String = sqlx::query_scalar(
        "SELECT settlement FROM resource.resource_usage_records WHERE usage_record_id=$1",
    )
    .bind(unconfigured.id.as_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(unconfigured_settlement, "pending");
    let unconfigured_charge_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM resource.resource_charges WHERE usage_record_id=$1",
    )
    .bind(unconfigured.id.as_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(unconfigured_charge_count, 0);

    let adjustment_amount = Money {
        currency: "USD".to_owned(),
        amount: FixedDecimal::parse("-0.250000")?,
    };
    let adjustment = store
        .create_adjustment(
            "billing-adjustment-1",
            project_id,
            first_charge.id,
            adjustment_amount.clone(),
            "corrected provider overcount".to_owned(),
            actor_id,
            end,
        )
        .await
        .map_err(|error| format!("create adjustment: {error}"))?;
    let adjustment_replay = store
        .create_adjustment(
            "billing-adjustment-1",
            project_id,
            first_charge.id,
            adjustment_amount,
            "corrected provider overcount".to_owned(),
            actor_id,
            end,
        )
        .await
        .map_err(|error| format!("replay adjustment: {error}"))?;
    assert_eq!(adjustment, adjustment_replay);
    assert_eq!(adjustment.total.amount.as_str(), "-0.250000");
    let charges = store
        .list_charges(project_id)
        .await
        .map_err(|error| format!("list charges: {error}"))?;
    assert_eq!(charges.len(), 4);
    let budget = store.get_budget(project_id).await?;
    assert_eq!(budget.spent.amount.as_str(), "3.250000");
    let adjustment_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM resource.resource_charges
         WHERE adjustment_of=$1",
    )
    .bind(first_charge.id.as_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(adjustment_count, 1);
    Ok(())
}

#[tokio::test]
async fn settlement_retry_caps_exponent_before_integer_cast_for_large_attempts()
-> Result<(), Box<dyn std::error::Error>> {
    let (_container, pool) = migrated_pool().await?;
    let store = PgResourceStore::new(pool.clone());
    let project_id = ProjectId::new();
    let actor_id = ActorId::new();
    let now = store.current_time().await?;
    let (request, lease) = create_active_request(
        &store,
        now,
        project_id,
        actor_id,
        "settlement-large-attempts",
    )
    .await?;
    let measured_until = UtcTimestamp::from_utc(now.get() + time::Duration::minutes(1))?;
    let usage = store
        .record_usage(
            &RecordResourceUsageRequest {
                project_id,
                course_id: None,
                kind: ResourceUsageKind::Compute,
                request_id: request.id,
                lease_id: Some(lease.id),
                source_event_id: contracts::EventId::new(),
                measured_from: now,
                measured_until,
                measurement: UsageMeasurement::Known {
                    quantities: ResourceUsageQuantities {
                        cpu_millicore_seconds: 1,
                        memory_byte_seconds: 0,
                        storage_byte_seconds: 0,
                        gpu_unit_seconds: 0,
                    },
                },
            },
            measured_until,
        )
        .await?;

    for (attempts_before, expected_attempts) in
        [(31_i32, 32_i32), (1024, 1025), (i32::MAX, i32::MAX)]
    {
        sqlx::query(
            "UPDATE resource.resource_usage_records
             SET settlement_attempts=$2,
                 settlement_next_attempt_at=clock_timestamp() - interval '1 second'
             WHERE usage_record_id=$1",
        )
        .bind(usage.id.as_uuid())
        .bind(attempts_before)
        .execute(&pool)
        .await?;

        assert!(store.settle_pending_usage_once().await?);

        let (attempts, next_attempt_at, diagnostic): (i32, time::OffsetDateTime, Option<String>) =
            sqlx::query_as(
                "SELECT settlement_attempts, settlement_next_attempt_at,
                        settlement_diagnostic_code
                 FROM resource.resource_usage_records
                 WHERE usage_record_id=$1",
            )
            .bind(usage.id.as_uuid())
            .fetch_one(&pool)
            .await?;
        let delay = next_attempt_at - store.current_time().await?.get();
        assert_eq!(attempts, expected_attempts);
        assert_eq!(diagnostic.as_deref(), Some("LW_RESOURCE_RATE_UNCONFIGURED"));
        assert!(
            delay >= time::Duration::seconds(59) && delay <= time::Duration::seconds(61),
            "retry delay should remain capped at 60 seconds, got {delay}"
        );
    }
    Ok(())
}

async fn create_active_request(
    store: &PgResourceStore,
    now: UtcTimestamp,
    project_id: ProjectId,
    requester_id: ActorId,
    request_key: &str,
) -> Result<(ResourceRequest, contracts::resource::ResourceLease), Box<dyn std::error::Error>> {
    let resources = WorkloadResources {
        cpu_millicores: 1000,
        memory_bytes: 1024 * 1024 * 1024,
        storage_bytes: 1024 * 1024 * 1024,
        gpu: None,
    };
    let request = ResourceRequest {
        id: ResourceRequestId::new(),
        generation: 1,
        request_key: request_key.to_owned(),
        requester_id,
        course_id: None,
        project_id,
        target: ResourceTarget::Environment {
            environment_id: EnvironmentId::new(),
            release_id: ReleaseId::new(),
            release_version: 1,
        },
        requested_resources: resources.clone(),
        requested_duration_seconds: 3600,
        state: ResourceRequestState::Reviewing,
        revision: Revision::new(1)?,
        created_at: now,
        updated_at: now,
        diagnostic_code: None,
    };
    let create_key = format!("billing-request-{request_key}");
    store
        .create(&create_key, &request, "billing-request-trace")
        .await
        .map_err(|error| format!("create: {error}"))?;
    let approval = ResourceApproval {
        id: ResourceApprovalId::new(),
        request_id: request.id,
        request_revision: Revision::new(1)?,
        approver_id: ActorId::new(),
        provider_binding: "kubernetes-standard".to_owned(),
        approved_resources: resources.clone(),
        approved_duration_seconds: 3600,
        reason: "billing test approval".to_owned(),
        valid_until: UtcTimestamp::from_utc(now.get() + time::Duration::hours(2))?,
        created_at: now,
    };
    let lease_id = LeaseId::new();
    let approve_key = format!("billing-approve-{request_key}");
    let approved = store
        .approve(
            &approve_key,
            request.id,
            &approval,
            &PendingAllocation {
                claim: CapacityClaim {
                    id: CapacityClaimId::new(),
                    request_id: request.id,
                    approval_id: approval.id,
                    provider_binding: approval.provider_binding.clone(),
                    workload_resources: resources.clone(),
                    quota_resources: resources,
                    gpu_allocation: None,
                    state: contracts::resource::CapacityClaimState::Reserved,
                    revision: Revision::new(1)?,
                },
                lease_id,
            },
            ApprovalPolicy {
                min_duration_seconds: 60,
                max_duration_seconds: 7200,
            },
            "billing-approve-trace",
        )
        .await
        .map_err(|error| format!("approve: {error}"))?;
    assert_eq!(approved.state, ResourceRequestState::Allocating);
    let active_from = store.current_time().await?;
    let expires_at = UtcTimestamp::from_utc(active_from.get() + time::Duration::hours(1))?;
    let lease = store
        .activate_lease(
            lease_id,
            Revision::new(1)?,
            active_from,
            expires_at,
            approval.approver_id,
            "billing-activate-trace",
        )
        .await
        .map_err(|error| format!("activate: {error}"))?;
    Ok((request, lease))
}

async fn migrated_pool()
-> Result<(testcontainers::ContainerAsync<Postgres>, sqlx::PgPool), Box<dyn std::error::Error>> {
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let url = format!(
        "postgres://postgres:postgres@127.0.0.1:{}/postgres",
        container.get_host_port_ipv4(5432).await?
    );
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await?;
    sqlx::raw_sql(&format!(
        "CREATE SCHEMA resource; SET search_path TO resource;\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}",
        include_str!("../../../migrations/resource/0001_platform_baseline.sql"),
        include_str!("../../../migrations/resource/0002_resource_request_capacity_lease.sql"),
        include_str!("../../../migrations/resource/0003_resource_contract_snapshots.sql"),
        include_str!("../../../migrations/resource/0004_resource_claim_quota_resources.sql"),
        include_str!(
            "../../../migrations/resource/0005_resource_lease_pending_terminal_states.sql"
        ),
        include_str!("../../../migrations/resource/0006_resource_lease_reconciliation.sql"),
        include_str!("../../../migrations/resource/0007_resource_outbox_trigger_fix.sql"),
        include_str!("../../../migrations/resource/0008_v3_project_gpu_billing.sql"),
        include_str!("../../../migrations/resource/0009_settlement_retry.sql"),
        include_str!("../../../migrations/resource/0010_task_run_identity.sql"),
        include_str!("../../../migrations/resource/0011_gpu_catalog_pool_uniqueness.sql")
    ))
    .execute(&pool)
    .await?;
    Ok((container, pool))
}

fn gpu_bundle(
    now: UtcTimestamp,
    project_id: ProjectId,
    request_key: &str,
    class: &str,
    count: u32,
) -> (ResourceRequest, ResourceApproval, PendingAllocation) {
    let requester_id = ActorId::new();
    let course_id = Some(CourseId::new());
    let resources = WorkloadResources {
        cpu_millicores: 1,
        memory_bytes: 1,
        storage_bytes: 1,
        gpu: Some(GpuRequest {
            class: class.to_owned(),
            count,
        }),
    };
    let request = ResourceRequest {
        id: ResourceRequestId::new(),
        generation: 1,
        request_key: request_key.to_owned(),
        requester_id,
        course_id,
        project_id,
        target: ResourceTarget::Environment {
            environment_id: EnvironmentId::new(),
            release_id: ReleaseId::new(),
            release_version: 1,
        },
        requested_resources: resources.clone(),
        requested_duration_seconds: 60,
        state: ResourceRequestState::Reviewing,
        revision: Revision::new(1).expect("fixed revision"),
        created_at: now,
        updated_at: now,
        diagnostic_code: None,
    };
    let approval = ResourceApproval {
        id: ResourceApprovalId::new(),
        request_id: request.id,
        request_revision: request.revision,
        approver_id: ActorId::new(),
        provider_binding: "kubernetes-standard".to_owned(),
        approved_resources: resources.clone(),
        approved_duration_seconds: 60,
        reason: "GPU capacity approved".to_owned(),
        valid_until: UtcTimestamp::from_utc(now.get() + time::Duration::hours(1))
            .expect("fixed approval window"),
        created_at: now,
    };
    let allocation = PendingAllocation {
        claim: CapacityClaim {
            id: CapacityClaimId::new(),
            request_id: request.id,
            approval_id: approval.id,
            provider_binding: approval.provider_binding.clone(),
            workload_resources: resources.clone(),
            quota_resources: resources,
            gpu_allocation: None,
            state: contracts::resource::CapacityClaimState::Reserved,
            revision: Revision::new(1).expect("fixed claim revision"),
        },
        lease_id: LeaseId::new(),
    };
    (request, approval, allocation)
}

#[allow(dead_code)]
fn digest() -> persistence_sqlx::Sha256Digest {
    "a".repeat(64).parse().expect("fixed SHA-256 digest")
}

#[tokio::test]
async fn resource_outbox_waits_for_jetstream_ack_before_marking_published()
-> Result<(), Box<dyn std::error::Error>> {
    let (_postgres, pool) = migrated_pool().await?;
    let request = outbox_request()?;
    let event = contracts::events::CloudEvent {
        specversion: contracts::events::SPEC_VERSION.into(),
        id: contracts::EventId::new(),
        source: "urn:labweaver:resource-service".into(),
        event_type: contracts::events::subjects::RESOURCE_REQUEST_SUBMITTED.into(),
        subject: contracts::events::subjects::RESOURCE_REQUEST_SUBMITTED.into(),
        time: request.created_at,
        datacontenttype: "application/json".into(),
        dataschema: format!(
            "{}/resource-request-submitted.schema.json",
            contracts::events::DATA_SCHEMA_BASE
        ),
        project_id: request.project_id,
        course_id: request.course_id,
        aggregate_revision: request.revision,
        aggregate_sequence: contracts::Sequence(1),
        trace_id: "resource-outbox-jetstream".into(),
        data: contracts::events::ResourceRequestChanged { request },
    };
    let payload = serde_json::to_value(&event)?;
    sqlx::query("INSERT INTO resource.outbox_events (event_id,subject,event_type,aggregate_id,aggregate_sequence,payload,payload_sha256) VALUES ($1,$2,$2,$3,1,$4,$5)")
        .bind(event.id.as_uuid()).bind(&event.subject).bind(event.data.request.id.as_uuid()).bind(&payload).bind(persistence_sqlx::Sha256Digest::of_canonical(&payload)?.to_string()).execute(&pool).await?;
    let nats = GenericImage::new("nats", "2.11.8-alpine")
        .with_exposed_port(4222.tcp())
        .with_wait_for(WaitFor::message_on_stderr("Server is ready"))
        .with_cmd(["-js"])
        .start()
        .await?;
    let client = async_nats::connect(format!(
        "nats://127.0.0.1:{}",
        nats.get_host_port_ipv4(4222).await?
    ))
    .await?;
    let dispatcher =
        ResourceOutboxDispatcher::new(pool.clone(), client.clone(), Duration::from_secs(5))?;
    assert!(dispatcher.dispatch_once().await.is_err());
    assert!(!published(&pool, event.id).await?);
    let context = async_nats::jetstream::new(client);
    context
        .create_stream(async_nats::jetstream::stream::Config {
            name: "RESOURCE_EVENTS".into(),
            subjects: vec!["labweaver.resource.>".into()],
            ..Default::default()
        })
        .await?;
    assert!(
        matches!(dispatcher.dispatch_once().await?, ResourceOutboxOutcome::Published { event_id } if event_id == event.id)
    );
    assert!(published(&pool, event.id).await?);
    Ok(())
}

async fn published(pool: &sqlx::PgPool, event_id: contracts::EventId) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT published_at IS NOT NULL FROM resource.outbox_events WHERE event_id=$1",
    )
    .bind(event_id.as_uuid())
    .fetch_one(pool)
    .await
}

fn outbox_request() -> Result<ResourceRequest, Box<dyn std::error::Error>> {
    let now: UtcTimestamp = "2026-07-30T00:00:00.000Z".parse()?;
    Ok(ResourceRequest {
        id: ResourceRequestId::new(),
        generation: 1,
        request_key: "outbox-1".into(),
        requester_id: ActorId::new(),
        course_id: Some(CourseId::new()),
        project_id: ProjectId::new(),
        target: ResourceTarget::Environment {
            environment_id: EnvironmentId::new(),
            release_id: ReleaseId::new(),
            release_version: 1,
        },
        requested_resources: WorkloadResources {
            cpu_millicores: 1,
            memory_bytes: 1,
            storage_bytes: 1,
            gpu: None,
        },
        requested_duration_seconds: 60,
        state: ResourceRequestState::Reviewing,
        revision: Revision::new(1)?,
        created_at: now,
        updated_at: now,
        diagnostic_code: None,
    })
}
