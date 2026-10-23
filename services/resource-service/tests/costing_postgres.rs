//! Isolated `PostgreSQL` accounts verify pricing consent and actual debits together.
#![allow(
    clippy::too_many_lines,
    clippy::expect_used,
    reason = "integration transactions keep assertions together"
)]
use contracts::http::{CreateResourceRateRequest, RecordResourceUsageRequest};
use contracts::resource::costing::{EstimatedCreditHoldRequest, ResourceCostEstimateRequest};
use contracts::resource::credits::CreditAdminOperation;
use contracts::resource::{
    FixedDecimal, Money, ResourceBillingUnit, ResourceRequest, ResourceRequestState,
    ResourceTarget, ResourceUsageKind, ResourceUsageQuantities, ResourceUsageTarget,
    UsageMeasurement, WorkloadResources,
};
use contracts::{
    ActorId, EnvironmentId, EventId, PlatformRole, ProjectId, ReleaseId, ResourceRequestId,
    Revision, UtcTimestamp,
};
use resource_service::credits::PgCreditStore;
use resource_service::store::{PgResourceStore, ResourceStoreError};
use sqlx::{PgPool, postgres::PgPoolOptions};
use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;
use time::Duration;
use uuid::Uuid;
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
fn amount(v: &str) -> FixedDecimal {
    FixedDecimal::parse(v).expect("six-place fixture")
}
fn at(t: UtcTimestamp, millis: i64) -> UtcTimestamp {
    UtcTimestamp::from_utc(t.get() + Duration::milliseconds(millis)).expect("millisecond fixture")
}
async fn fixture() -> Result<(
    testcontainers::ContainerAsync<Postgres>,
    PgPool,
    PgResourceStore,
    PgCreditStore,
)> {
    let c = Postgres::default().with_tag("17.5-alpine").start().await?;
    let p = PgPoolOptions::new()
        .max_connections(32)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            c.get_host_port_ipv4(5432).await?
        ))
        .await?;
    let mut files = std::fs::read_dir(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations/resource"),
    )?
    .map(|e| e.map(|e| e.path()))
    .collect::<std::result::Result<Vec<_>, _>>()?;
    files.sort();
    let mut ddl = "CREATE SCHEMA resource; SET search_path TO resource;\n".to_owned();
    for f in files {
        if f.extension().is_some_and(|e| e == "sql") {
            ddl.push_str(&std::fs::read_to_string(f)?);
            ddl.push('\n');
        }
    }
    sqlx::raw_sql(sqlx::AssertSqlSafe(&*ddl))
        .execute(&p)
        .await?;
    Ok((
        c,
        p.clone(),
        PgResourceStore::new(p.clone()),
        PgCreditStore::new(p),
    ))
}
async fn rate(
    store: &PgResourceStore,
    unit: ResourceBillingUnit,
    from: UtcTimestamp,
    price: &str,
) -> Result<()> {
    store
        .create_rate(
            &Uuid::now_v7().to_string(),
            &CreateResourceRateRequest {
                builder_binding: None,
                model_binding: None,
                max_preauthorization: None,
                unit,
                unit_quantity: 1,
                gpu_class: None,
                gpu_mode: None,
                unit_price: Money {
                    currency: "CREDITS".into(),
                    amount: amount(price),
                },
                effective_from: from,
                effective_until: None,
            },
        )
        .await?;
    Ok(())
}
fn input(from: UtcTimestamp, millis: i64) -> ResourceCostEstimateRequest {
    ResourceCostEstimateRequest {
        resources: WorkloadResources {
            cpu_millicores: 1,
            memory_bytes: 1,
            storage_bytes: 1,
            gpu: None,
        },
        compute_from: from,
        compute_until: at(from, millis),
        storage_from: from,
        storage_until: at(from, millis),
    }
}
async fn request(
    store: &PgResourceStore,
    project: ProjectId,
    estimate: &ResourceCostEstimateRequest,
) -> Result<ResourceRequestId> {
    let now = store.current_time().await?;
    let id = ResourceRequestId::new();
    store
        .create(
            &id.to_string(),
            &ResourceRequest {
                id,
                generation: 1,
                request_key: id.to_string(),
                requester_id: ActorId::new(),
                project_id: project,
                course_id: None,
                target: ResourceTarget::Environment {
                    environment_id: EnvironmentId::new(),
                    release_id: ReleaseId::new(),
                    release_version: 1,
                },
                requested_resources: estimate.resources.clone(),
                requested_duration_seconds: u64::try_from(
                    (estimate.compute_until.get() - estimate.compute_from.get()).whole_seconds(),
                )?,
                state: ResourceRequestState::Reviewing,
                revision: Revision::new(1)?,
                created_at: now,
                updated_at: now,
                diagnostic_code: None,
            },
            "synthetic-cost",
        )
        .await?;
    Ok(id)
}
async fn grant(credits: &PgCreditStore, project: ProjectId, value: &str) -> Result<()> {
    credits
        .admin(
            &Uuid::now_v7().to_string(),
            ActorId::new(),
            &[PlatformRole::PlatformAdmin],
            &CreditAdminOperation::Grant {
                project_id: project,
                amount: amount(value),
                reason: "synthetic costing allowance".into(),
            },
        )
        .await?;
    Ok(())
}
#[tokio::test]
async fn quote_hold_and_cross_version_settlement_use_identical_exact_lines() -> Result<()> {
    let (_c, pool, store, credits) = fixture().await?;
    let now = store.current_time().await?;
    let from = at(now, 60_000);
    let project = ProjectId::new();
    let i = input(from, 60_000);
    for unit in [
        ResourceBillingUnit::CpuMillicoreSecond,
        ResourceBillingUnit::MemoryByteSecond,
        ResourceBillingUnit::StorageByteSecond,
    ] {
        rate(&store, unit, at(now, -60_000), "1.000000").await?;
    }
    rate(
        &store,
        ResourceBillingUnit::CpuMillicoreSecond,
        at(from, 30_000),
        "3.000000",
    )
    .await?;
    let quote = store.estimate_cost(project, &i).await?;
    assert_eq!(quote.total.amount, amount("240.000000"));
    for l in &quote.lines {
        l.validate()?;
        assert_eq!(l.quantity_divisor, 1000);
    }
    grant(&credits, project, "240.000000").await?;
    let id = request(&store, project, &i).await?;
    let h = EstimatedCreditHoldRequest {
        request_id: id,
        expected_estimate: quote.clone(),
    };
    let accepted = store
        .hold_estimated_cost("quoted-hold", "environment", true, &h)
        .await?;
    assert_eq!(accepted.hold.amount, quote.total.amount);
    assert_eq!(
        credits.account(project).await?.available,
        FixedDecimal::zero()
    );
    for (kind, cpu, memory, storage) in [
        (ResourceUsageKind::Compute, 60_000, 60_000, 0),
        (ResourceUsageKind::Storage, 0, 0, 60_000),
    ] {
        let usage = RecordResourceUsageRequest {
            kind,
            target: ResourceUsageTarget::ResourceRequest {
                request_id: id,
                lease_id: None,
            },
            source_event_id: EventId::new(),
            measured_from: from,
            measured_until: at(from, 60_000),
            measurement: UsageMeasurement::Known {
                quantities: ResourceUsageQuantities {
                    quantity_divisor: 1000,
                    cpu_millicore_seconds: cpu,
                    memory_byte_seconds: memory,
                    storage_byte_seconds: storage,
                    gpu_unit_seconds: 0,
                },
            },
        };
        let saved = store.record_usage(&usage, at(from, 60_000)).await?;
        let charge = store.settle_usage(saved.id).await?.expect("known charge");
        charge.validate()?;
        let same = store.record_usage(&usage, at(from, 60_000)).await?;
        assert_eq!(same.id, saved.id);
        assert_eq!(store.settle_usage(saved.id).await?, Some(charge));
    }
    let a = credits.account(project).await?;
    assert_eq!(a.spent, quote.total.amount);
    assert_eq!(a.balance, FixedDecimal::zero());
    assert_eq!(a.platform_absorbed, FixedDecimal::zero());
    // A new HTTP retry key returns the original funded snapshot after publication.
    rate(
        &store,
        ResourceBillingUnit::StorageByteSecond,
        at(from, 20_000),
        "2.000000",
    )
    .await
    .expect_err("settled history must reject rate rewrite");
    assert_eq!(
        store
            .hold_estimated_cost("quoted-rekey", "environment", true, &h)
            .await?,
        accepted
    );
    assert!(
        sqlx::query("UPDATE resource.credit_cost_snapshots SET snapshot='{}'")
            .execute(&pool)
            .await
            .is_err()
    );
    Ok(())
}
#[tokio::test]
async fn consent_changes_scope_and_insufficient_funds_never_leave_partial_holds() -> Result<()> {
    let (_c, pool, store, credits) = fixture().await?;
    let now = store.current_time().await?;
    let from = at(now, 60_000);
    let project = ProjectId::new();
    let i = input(from, 1_000);
    for unit in [
        ResourceBillingUnit::CpuMillicoreSecond,
        ResourceBillingUnit::MemoryByteSecond,
        ResourceBillingUnit::StorageByteSecond,
    ] {
        rate(&store, unit, at(now, -60_000), "1.000000").await?;
    }
    let quote = store.estimate_cost(project, &i).await?;
    let id = request(&store, project, &i).await?;
    let h = EstimatedCreditHoldRequest {
        request_id: id,
        expected_estimate: quote.clone(),
    };
    assert!(
        store
            .hold_estimated_cost("insufficient", "environment", true, &h)
            .await
            .is_err()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM resource.credit_cost_snapshots")
            .fetch_one(&pool)
            .await?,
        0
    );
    grant(&credits, project, "20.000000").await?;
    assert!(matches!(
        store
            .hold_estimated_cost("wrong-owner", "agent", false, &h)
            .await,
        Err(ResourceStoreError::Credits(
            resource_service::credits::CreditError::Denied
        ))
    ));
    rate(
        &store,
        ResourceBillingUnit::CpuMillicoreSecond,
        from,
        "2.000000",
    )
    .await?;
    assert!(matches!(
        store
            .hold_estimated_cost("stale-consent", "environment", true, &h)
            .await,
        Err(ResourceStoreError::EstimateChanged)
    ));
    let fresh = store.estimate_cost(project, &i).await?;
    let mut forged = fresh.clone();
    forged.total.amount = amount("0.000001");
    assert!(matches!(
        store
            .hold_estimated_cost(
                "forged-amount",
                "environment",
                true,
                &EstimatedCreditHoldRequest {
                    request_id: id,
                    expected_estimate: forged
                }
            )
            .await,
        Err(ResourceStoreError::EstimateChanged)
    ));
    let funded = store
        .hold_estimated_cost(
            "fresh-consent",
            "environment",
            true,
            &EstimatedCreditHoldRequest {
                request_id: id,
                expected_estimate: fresh.clone(),
            },
        )
        .await?;
    rate(
        &store,
        ResourceBillingUnit::StorageByteSecond,
        from,
        "4.000000",
    )
    .await?;
    assert_eq!(
        store
            .hold_estimated_cost(
                "fresh-rekey",
                "environment",
                true,
                &EstimatedCreditHoldRequest {
                    request_id: id,
                    expected_estimate: fresh
                }
            )
            .await?,
        funded
    );
    assert_eq!(credits.holds(project, 1, 100, 0).await?.items.len(), 1);
    Ok(())
}
#[tokio::test]
async fn subsecond_estimate_retains_compute_and_storage_fractions_and_missing_rates() -> Result<()>
{
    let (_c, _pool, store, _credits) = fixture().await?;
    let now = store.current_time().await?;
    let project = ProjectId::new();
    let from = at(now, 60_000);
    rate(
        &store,
        ResourceBillingUnit::CpuMillicoreSecond,
        at(now, -60_000),
        "1.000000",
    )
    .await?;
    let mut i = input(from, 400);
    i.storage_until = at(from, 700);
    assert!(matches!(
        store.estimate_cost(project, &i).await,
        Err(ResourceStoreError::RateUnconfigured)
    ));
    rate(
        &store,
        ResourceBillingUnit::MemoryByteSecond,
        at(now, -60_000),
        "1.000000",
    )
    .await?;
    rate(
        &store,
        ResourceBillingUnit::StorageByteSecond,
        at(now, -60_000),
        "1.000000",
    )
    .await?;
    let q = store.estimate_cost(project, &i).await?;
    assert_eq!(q.total.amount, amount("1.500000"));
    assert_eq!(
        q.lines.iter().map(|l| l.quantity).collect::<Vec<_>>(),
        vec![400, 400, 700]
    );
    for l in q.lines {
        l.validate()?;
    }
    Ok(())
}

async fn pending_task(
    store: &PgResourceStore,
    project: ProjectId,
) -> Result<(
    contracts::TaskRunId,
    contracts::http::AcknowledgeTaskResourceRequest,
)> {
    pending_task_with_resources(
        store,
        project,
        input(store.current_time().await?, 1000).resources,
    )
    .await
}

async fn pending_task_with_resources(
    store: &PgResourceStore,
    project: ProjectId,
    resources: WorkloadResources,
) -> Result<(
    contracts::TaskRunId,
    contracts::http::AcknowledgeTaskResourceRequest,
)> {
    use contracts::resource::{CapacityClaim, CapacityClaimState, ResourceApproval};
    use contracts::{CapacityClaimId, LeaseId, ResourceApprovalId, TaskRunId};
    use resource_service::{ApprovalPolicy, store::PendingAllocation};
    let task = TaskRunId::new();
    let id = ResourceRequestId::new();
    let now = store.current_time().await?;
    let req = ResourceRequest {
        id,
        generation: 1,
        request_key: id.to_string(),
        requester_id: ActorId::new(),
        project_id: project,
        course_id: None,
        target: ResourceTarget::Task { task_run_id: task },
        requested_resources: resources.clone(),
        requested_duration_seconds: 1,
        state: ResourceRequestState::Reviewing,
        revision: Revision::new(1)?,
        created_at: now,
        updated_at: now,
        diagnostic_code: None,
    };
    store
        .create(&id.to_string(), &req, "synthetic-task-cost")
        .await?;
    let approval = ResourceApproval {
        runtime_authorization: None,
        id: ResourceApprovalId::new(),
        request_id: id,
        request_revision: req.revision,
        approver_id: ActorId::new(),
        provider_binding: "kubernetes-standard".into(),
        approved_resources: resources.clone(),
        approved_duration_seconds: 1,
        reason: "synthetic task upper bound".into(),
        valid_until: at(now, 60_000),
        created_at: now,
    };
    let allocation = PendingAllocation {
        claim: CapacityClaim {
            id: CapacityClaimId::new(),
            request_id: id,
            approval_id: approval.id,
            provider_binding: approval.provider_binding.clone(),
            workload_resources: resources.clone(),
            quota_resources: resources,
            gpu_allocation: None,
            state: CapacityClaimState::Reserved,
            revision: Revision::new(1)?,
            updated_at: Some(now),
        },
        lease_id: LeaseId::new(),
    };
    store
        .approve(
            &Uuid::now_v7().to_string(),
            id,
            &approval,
            &allocation,
            ApprovalPolicy {
                min_duration_seconds: 1,
                max_duration_seconds: 60,
            },
            "synthetic-task-approve",
        )
        .await?;
    let claim = store
        .claim_task_resource(task, "synthetic-task-claim")
        .await?;
    Ok((
        task,
        contracts::http::AcknowledgeTaskResourceRequest {
            expected_claim_revision: claim.claim_revision,
            expected_lease_revision: claim.lease_revision,
            execution_namespace: "synthetic-task".into(),
        },
    ))
}

#[tokio::test]
async fn task_execution_ack_funds_upper_bound_atomically_and_keeps_failures_unstarted() -> Result<()>
{
    let (_c, pool, store, credits) = fixture().await?;
    let now = store.current_time().await?;
    let project = ProjectId::new();
    for unit in [
        ResourceBillingUnit::CpuMillicoreSecond,
        ResourceBillingUnit::MemoryByteSecond,
        ResourceBillingUnit::StorageByteSecond,
    ] {
        rate(&store, unit, at(now, -60_000), "1.000000").await?;
    }
    let (task, ack) = pending_task(&store, project).await?;
    let error = store
        .acknowledge_funded_task_resource(task, &ack, "evaluation", "task-no-funds")
        .await
        .expect_err("must fund before execution");
    assert!(matches!(
        error,
        ResourceStoreError::Credits(resource_service::credits::CreditError::Insufficient)
    ));
    assert_eq!(
        store.load_task_resource(task).await?.request.state,
        ResourceRequestState::Allocating
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM resource.credit_holds")
            .fetch_one(&pool)
            .await?,
        0
    );
    grant(&credits, project, "6.000000").await?;
    let mut stale = ack.clone();
    stale.expected_claim_revision = Revision::new(999)?;
    assert!(
        store
            .acknowledge_funded_task_resource(task, &stale, "evaluation", "stale-task")
            .await
            .is_err()
    );
    assert_eq!(
        credits.account(project).await?.reserved,
        FixedDecimal::zero()
    );
    let active = store
        .acknowledge_funded_task_resource(task, &ack, "evaluation", "funded-task")
        .await?;
    assert_eq!(active.request.state, ResourceRequestState::Active);
    assert!(matches!(
        store
            .acknowledge_funded_task_resource(task, &ack, "agent", "other-owner-replay")
            .await,
        Err(ResourceStoreError::Credits(
            resource_service::credits::CreditError::Denied
        ))
    ));
    assert_eq!(credits.account(project).await?.reserved, amount("3.000000"));
    let (other, other_ack) = pending_task(&store, project).await?;
    let second = store
        .acknowledge_funded_task_resource(other, &other_ack, "agent", "funded-agent-task")
        .await?;
    assert_eq!(second.request.state, ResourceRequestState::Active);
    let (third, third_ack) = pending_task(&store, project).await?;
    assert!(
        store
            .acknowledge_funded_task_resource(third, &third_ack, "agent", "exhausted-task")
            .await
            .is_err()
    );
    assert_eq!(
        store.load_task_resource(third).await?.request.state,
        ResourceRequestState::Allocating
    );
    assert_eq!(
        credits.account(project).await?.available,
        FixedDecimal::zero()
    );
    Ok(())
}

#[tokio::test]
async fn explicitly_configured_zero_prices_still_authorize_and_record_a_snapshot() -> Result<()> {
    let (_c, pool, store, credits) = fixture().await?;
    let now = store.current_time().await?;
    let project = ProjectId::new();
    for unit in [
        ResourceBillingUnit::CpuMillicoreSecond,
        ResourceBillingUnit::MemoryByteSecond,
        ResourceBillingUnit::StorageByteSecond,
    ] {
        rate(&store, unit, at(now, -60_000), "0.000000").await?;
    }
    let (task, ack) = pending_task(&store, project).await?;
    assert_eq!(
        store
            .acknowledge_funded_task_resource(task, &ack, "evaluation", "free-task")
            .await?
            .request
            .state,
        ResourceRequestState::Active
    );
    assert_eq!(
        credits.account(project).await?.reserved,
        FixedDecimal::zero()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM resource.credit_cost_snapshots")
            .fetch_one(&pool)
            .await?,
        1
    );
    Ok(())
}

#[tokio::test]
async fn released_or_uncertain_quote_cannot_authorize_a_new_task_execution() -> Result<()> {
    use contracts::resource::credits::CreditHoldResolutionRequest;
    let (_c, _pool, store, credits) = fixture().await?;
    let now = store.current_time().await?;
    let project = ProjectId::new();
    for unit in [
        ResourceBillingUnit::CpuMillicoreSecond,
        ResourceBillingUnit::MemoryByteSecond,
        ResourceBillingUnit::StorageByteSecond,
    ] {
        rate(&store, unit, at(now, -60_000), "1.000000").await?;
    }
    grant(&credits, project, "20.000000").await?;
    for uncertain in [false, true] {
        let (task, ack) = pending_task(&store, project).await?;
        let status = store.load_task_resource(task).await?;
        let quote = store
            .estimate_cost(project, &input(at(now, 60_000), 1000))
            .await?;
        let held = store
            .hold_estimated_cost(
                &Uuid::now_v7().to_string(),
                "evaluation",
                false,
                &EstimatedCreditHoldRequest {
                    request_id: status.request.id,
                    expected_estimate: quote,
                },
            )
            .await?;
        let resolution = if uncertain {
            CreditHoldResolutionRequest::Unknown {
                event_id: None,
                reason: "provider response lost".into(),
            }
        } else {
            CreditHoldResolutionRequest::Release {
                reason: "prior preauthorization released".into(),
            }
        };
        credits
            .resolve(
                &Uuid::now_v7().to_string(),
                "evaluation",
                held.hold.id,
                &resolution,
            )
            .await?;
        assert!(matches!(
            store
                .acknowledge_funded_task_resource(task, &ack, "evaluation", "must-not-start")
                .await,
            Err(ResourceStoreError::Credits(
                resource_service::credits::CreditError::Insufficient
            ))
        ));
        assert_eq!(
            store.load_task_resource(task).await?.request.state,
            ResourceRequestState::Allocating
        );
    }
    Ok(())
}

#[tokio::test]
async fn task_upper_bound_keeps_approved_gpu_mode_after_catalog_changes() -> Result<()> {
    use contracts::GpuCatalogEntryId;
    use contracts::resource::{GpuAllocationMode, GpuCatalogEntry, GpuRequest};
    let (_c, pool, store, credits) = fixture().await?;
    let now = store.current_time().await?;
    let project = ProjectId::new();
    for unit in [
        ResourceBillingUnit::CpuMillicoreSecond,
        ResourceBillingUnit::MemoryByteSecond,
        ResourceBillingUnit::StorageByteSecond,
    ] {
        rate(&store, unit, at(now, -60_000), "1.000000").await?;
    }
    let mut catalog = GpuCatalogEntry {
        id: GpuCatalogEntryId::new(),
        class: "synthetic-gpu".into(),
        mode: GpuAllocationMode::Exclusive,
        provider_binding: "kubernetes-standard".into(),
        capacity_units: 1,
        allocation_binding: "synthetic.example/gpu".into(),
        revision: Revision::new(1)?,
        active: true,
    };
    store
        .create_gpu_catalog_entry("synthetic-gpu", &catalog)
        .await?;
    store
        .record_gpu_capacity_observation(
            catalog.id,
            1,
            "isolated-synthetic-observation",
            now,
            at(now, 60_000),
        )
        .await?;
    for (mode, price) in [
        (GpuAllocationMode::Exclusive, "1.000000"),
        (GpuAllocationMode::VmVgpu, "9.000000"),
    ] {
        store
            .create_rate(
                &Uuid::now_v7().to_string(),
                &CreateResourceRateRequest {
                    builder_binding: None,
                    model_binding: None,
                    max_preauthorization: None,
                    unit: ResourceBillingUnit::GpuUnitSecond,
                    unit_quantity: 1,
                    gpu_class: Some(catalog.class.clone()),
                    gpu_mode: Some(mode),
                    unit_price: Money {
                        currency: "CREDITS".into(),
                        amount: amount(price),
                    },
                    effective_from: at(now, -60_000),
                    effective_until: None,
                },
            )
            .await?;
    }
    let mut resources = input(now, 1000).resources;
    resources.gpu = Some(GpuRequest {
        class: catalog.class.clone(),
        count: 1,
    });
    let (task, ack) = pending_task_with_resources(&store, project, resources.clone()).await?;
    grant(&credits, project, "4.000000").await?;
    // Change only this isolated catalog fixture to prove that pricing follows the captured claim.
    catalog.mode = GpuAllocationMode::VmVgpu;
    sqlx::query(
        "UPDATE resource.gpu_catalog_entries SET mode='vm_vgpu',contract=$2 WHERE entry_id=$1",
    )
    .bind(catalog.id.as_uuid())
    .bind(serde_json::to_value(catalog)?)
    .execute(&pool)
    .await?;
    let active = store
        .acknowledge_funded_task_resource(task, &ack, "evaluation", "gpu-priced-ack")
        .await?;
    assert_eq!(
        active
            .claim
            .gpu_allocation
            .ok_or("approved GPU missing")?
            .mode,
        GpuAllocationMode::Exclusive
    );
    assert_eq!(credits.account(project).await?.reserved, amount("4.000000"));
    let mut preview = input(at(now, 60_000), 1000);
    preview.resources = resources;
    assert_eq!(
        store.estimate_cost(project, &preview).await?.total.amount,
        amount("12.000000")
    );
    Ok(())
}
