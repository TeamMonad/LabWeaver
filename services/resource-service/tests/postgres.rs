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

use contracts::resource::{
    CapacityClaim, ResourceApproval, ResourceRequest, ResourceRequestState, ResourceTarget,
    WorkloadResources,
};
use contracts::{
    ActorId, CapacityClaimId, CourseId, EnvironmentId, LeaseId, ReleaseId, ResourceApprovalId,
    ResourceRequestId, Revision, UtcTimestamp,
};
use resource_service::ApprovalPolicy;
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
        "CREATE SCHEMA resource; SET search_path TO resource;\n{}\n{}\n{}\n{}\n{}\n{}",
        include_str!("../../../migrations/resource/0001_platform_baseline.sql"),
        include_str!("../../../migrations/resource/0002_resource_request_capacity_lease.sql"),
        include_str!("../../../migrations/resource/0003_resource_contract_snapshots.sql"),
        include_str!("../../../migrations/resource/0004_resource_claim_quota_resources.sql"),
        include_str!(
            "../../../migrations/resource/0005_resource_lease_pending_terminal_states.sql"
        ),
        include_str!("../../../migrations/resource/0006_resource_lease_reconciliation.sql"),
    ))
    .execute(&pool)
    .await?;

    let request_id = Uuid::now_v7();
    let approval_id = Uuid::now_v7();
    let claim_id = Uuid::now_v7();
    let lease_id = Uuid::now_v7();
    sqlx::query("INSERT INTO resource.resource_requests (request_id,generation,request_key,requester_id,course_id,environment_id,release_id,release_version,requested_cpu_millicores,requested_memory_bytes,requested_storage_bytes,requested_duration_seconds,state,revision,contract) VALUES ($1,1,'request-1',$2,$3,$4,$5,1,1,1,1,60,'allocating',2,$6)")
        .bind(request_id).bind(Uuid::now_v7()).bind(Uuid::now_v7()).bind(Uuid::now_v7()).bind(Uuid::now_v7()).bind(serde_json::json!({"request": "snapshot"})).execute(&pool).await?;
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
        course_id: CourseId::new(),
        project_id: None,
        target: ResourceTarget {
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
            .mark_capacity_handed_off(retrying.id, retrying.revision)
            .await?
            .state,
        contracts::resource::CapacityClaimState::HandedOff
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
        "CREATE SCHEMA resource; SET search_path TO resource;\n{}\n{}\n{}\n{}\n{}\n{}",
        include_str!("../../../migrations/resource/0001_platform_baseline.sql"),
        include_str!("../../../migrations/resource/0002_resource_request_capacity_lease.sql"),
        include_str!("../../../migrations/resource/0003_resource_contract_snapshots.sql"),
        include_str!("../../../migrations/resource/0004_resource_claim_quota_resources.sql"),
        include_str!(
            "../../../migrations/resource/0005_resource_lease_pending_terminal_states.sql"
        ),
        include_str!("../../../migrations/resource/0006_resource_lease_reconciliation.sql")
    ))
    .execute(&pool)
    .await?;
    Ok((container, pool))
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
        course_id: CourseId::new(),
        project_id: None,
        target: ResourceTarget {
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
