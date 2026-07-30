//! PostgreSQL evidence for Resource authority schema and pending Lease semantics.

use sqlx::postgres::PgPoolOptions;
use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;
use uuid::Uuid;

use contracts::resource::{
    CapacityClaim, ResourceApproval, ResourceRequest, ResourceRequestState, ResourceTarget,
    WorkloadResources,
};
use contracts::{
    ActorId, CapacityClaimId, CourseId, EnvironmentId, LeaseId, ReleaseId, ResourceApprovalId,
    ResourceRequestId, Revision, Sha256Digest, UtcTimestamp,
};
use resource_service::ApprovalPolicy;
use resource_service::store::{PendingAllocation, PgResourceStore};

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
        "CREATE SCHEMA resource; SET search_path TO resource;\n{}\n{}\n{}\n{}\n{}",
        include_str!("../../../migrations/resource/0001_sprint2_baseline.sql"),
        include_str!("../../../migrations/resource/0002_resource_request_capacity_lease.sql"),
        include_str!("../../../migrations/resource/0003_resource_contract_snapshots.sql"),
        include_str!("../../../migrations/resource/0004_resource_claim_quota_resources.sql"),
        include_str!(
            "../../../migrations/resource/0005_resource_lease_pending_terminal_states.sql"
        ),
    ))
    .execute(&pool)
    .await?;

    let request_id = Uuid::now_v7();
    let approval_id = Uuid::now_v7();
    let claim_id = Uuid::now_v7();
    let lease_id = Uuid::now_v7();
    sqlx::query("INSERT INTO resource.resource_requests (request_id,generation,request_key,requester_id,course_id,environment_id,release_id,release_version,release_sha256,requested_cpu_millicores,requested_memory_bytes,requested_storage_bytes,requested_duration_seconds,state,revision,contract) VALUES ($1,1,'request-1',$2,$3,$4,$5,1,$6,1,1,1,60,'allocating',2,$7)")
        .bind(request_id).bind(Uuid::now_v7()).bind(Uuid::now_v7()).bind(Uuid::now_v7()).bind(Uuid::now_v7()).bind("a".repeat(64)).bind(serde_json::json!({"request": "snapshot"})).execute(&pool).await?;
    sqlx::query("INSERT INTO resource.resource_approvals (approval_id,request_id,request_revision,approver_id,provider_binding,policy_sha256,approved_cpu_millicores,approved_memory_bytes,approved_storage_bytes,approved_duration_seconds,reason,valid_until,contract) VALUES ($1,$2,1,$3,'kubernetes-standard',$4,1,1,1,60,'approved',now()+interval '1 hour',$5)")
        .bind(approval_id).bind(request_id).bind(Uuid::now_v7()).bind("b".repeat(64)).bind(serde_json::json!({"approval": "snapshot"})).execute(&pool).await?;
    sqlx::query("INSERT INTO resource.capacity_claims (claim_id,request_id,approval_id,provider_binding,policy_sha256,quota_plan_sha256,state,revision,workload_cpu_millicores,workload_memory_bytes,workload_storage_bytes,quota_cpu_millicores,quota_memory_bytes,quota_storage_bytes,contract) VALUES ($1,$2,$3,'kubernetes-standard',$4,$5,'reserved',1,1,1,1,2,2,2,$6)")
        .bind(claim_id).bind(request_id).bind(approval_id).bind("b".repeat(64)).bind("c".repeat(64)).bind(serde_json::json!({"claim": "snapshot"})).execute(&pool).await?;
    sqlx::query("INSERT INTO resource.resource_leases (lease_id,request_id,claim_id,state,revision,contract) VALUES ($1,$2,$3,'revoked',1,$4)")
        .bind(lease_id).bind(request_id).bind(claim_id).bind(serde_json::json!({"lease": "pending-terminal"})).execute(&pool).await?;

    assert!(sqlx::query("INSERT INTO resource.capacity_claims (claim_id,request_id,approval_id,provider_binding,policy_sha256,quota_plan_sha256,state,revision,workload_cpu_millicores,workload_memory_bytes,workload_storage_bytes,workload_gpu_class,quota_cpu_millicores,quota_memory_bytes,quota_storage_bytes,contract) VALUES ($1,$2,$3,'kubernetes-standard',$4,$5,'reserved',1,1,1,1,'gpu-a100',2,2,2,$6)")
        .bind(Uuid::now_v7()).bind(Uuid::now_v7()).bind(Uuid::now_v7()).bind("b".repeat(64)).bind("c".repeat(64)).bind(serde_json::json!({})).execute(&pool).await.is_err());
    Ok(())
}

#[tokio::test]
async fn resource_store_commits_request_approval_claim_lease_and_renewal_as_fenced_authority()
-> Result<(), Box<dyn std::error::Error>> {
    let (_container, pool) = migrated_pool().await?;
    let store = PgResourceStore::new(pool);
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
        course_id: CourseId::new(),
        project_id: None,
        target: ResourceTarget {
            environment_id: EnvironmentId::new(),
            release_id: ReleaseId::new(),
            release_version: 1,
            release_sha256: digest(),
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
        policy_sha256: digest(),
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
            policy_sha256: approval.policy_sha256.clone(),
            workload_resources: resources.clone(),
            quota_resources: resources,
            quota_plan_sha256: digest(),
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
                gpu_capacity: 0,
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
    let ready = store
        .mark_capacity_shell_ready(
            provisioning.claim.id,
            provisioning.claim.revision,
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
    let renewed_expires = UtcTimestamp::from_utc(active_from.get() + time::Duration::minutes(15))?;
    let renewed = store
        .renew_lease(
            "resource-renew-1",
            active.id,
            active.revision,
            renewed_expires,
        )
        .await?;
    assert!(renewed.expires_at > active.expires_at);
    assert_eq!(store.load_lease(active.id).await?, renewed);
    Ok(())
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
        "CREATE SCHEMA resource; SET search_path TO resource;\n{}\n{}\n{}\n{}\n{}",
        include_str!("../../../migrations/resource/0001_sprint2_baseline.sql"),
        include_str!("../../../migrations/resource/0002_resource_request_capacity_lease.sql"),
        include_str!("../../../migrations/resource/0003_resource_contract_snapshots.sql"),
        include_str!("../../../migrations/resource/0004_resource_claim_quota_resources.sql"),
        include_str!(
            "../../../migrations/resource/0005_resource_lease_pending_terminal_states.sql"
        )
    ))
    .execute(&pool)
    .await?;
    Ok((container, pool))
}

fn digest() -> Sha256Digest {
    "a".repeat(64).parse().expect("fixed SHA-256 digest")
}
