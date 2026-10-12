//! Synthetic accounts, real Resource approval/lease/ledger transactions.
#![allow(
    clippy::too_many_lines,
    reason = "keep each full transaction invariant visible"
)]
use contracts::http::CreateResourceRateRequest;
use contracts::resource::credits::{CreditAdminOperation, CreditHoldResolutionRequest};
use contracts::resource::{
    CapacityClaim, CapacityClaimState, FixedDecimal, Money, ResourceApproval, ResourceBillingUnit,
    ResourceRequest, ResourceRequestState, ResourceTarget, WorkloadResources,
};
use contracts::{
    ActorId, CapacityClaimId, EnvironmentId, LeaseId, PlatformRole, ProjectId, ReleaseId,
    ResourceApprovalId, ResourceRequestId, Revision, UtcTimestamp,
};
use resource_service::{
    ApprovalPolicy,
    credits::PgCreditStore,
    store::{PendingAllocation, PgResourceStore},
};
use sqlx::{PgPool, postgres::PgPoolOptions};
use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;
use uuid::Uuid;
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const OWNER: &str = "synthetic-environment";
fn money(value: &str) -> Result<FixedDecimal> {
    Ok(FixedDecimal::parse(value)?)
}
fn at(from: UtcTimestamp, seconds: i64) -> Result<UtcTimestamp> {
    Ok(UtcTimestamp::from_utc(
        from.get() + time::Duration::seconds(seconds),
    )?)
}
async fn fixture() -> Result<(
    testcontainers::ContainerAsync<Postgres>,
    PgPool,
    PgResourceStore,
    PgCreditStore,
    ResourceRequest,
    LeaseId,
)> {
    fixture_with_approval_seconds(3600).await
}
async fn fixture_with_approval_seconds(
    valid_seconds: i64,
) -> Result<(
    testcontainers::ContainerAsync<Postgres>,
    PgPool,
    PgResourceStore,
    PgCreditStore,
    ResourceRequest,
    LeaseId,
)> {
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(12)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            container.get_host_port_ipv4(5432).await?
        ))
        .await?;
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
    let catalog = persistence_sqlx::MigrationCatalog::load(&root.join("catalog.yaml"))?;
    let domain = catalog
        .domains
        .iter()
        .find(|d| d.name == persistence_sqlx::Domain::Resource)
        .ok_or("resource catalog")?;
    let mut ddl = "CREATE SCHEMA resource; SET search_path=resource;\n".to_owned();
    for migration in &domain.migrations {
        ddl.push_str(&persistence_sqlx::MigrationCatalog::read_verified_sql(
            &root, migration,
        )?);
        ddl.push('\n');
    }
    sqlx::raw_sql(&ddl).execute(&pool).await?;
    let store = PgResourceStore::new(pool.clone()).with_environment_credit_owner(OWNER.into());
    let credits = PgCreditStore::new(pool.clone());
    let now = store.current_time().await?;
    let resources = WorkloadResources {
        cpu_millicores: 1000,
        memory_bytes: 1,
        storage_bytes: 1,
        gpu: None,
    };
    let request = ResourceRequest {
        id: ResourceRequestId::new(),
        generation: 1,
        request_key: format!("env-{}", Uuid::new_v4()),
        requester_id: ActorId::new(),
        project_id: ProjectId::new(),
        course_id: None,
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
        .create(&Uuid::new_v4().to_string(), &request, "synthetic-create")
        .await?;
    let approval = ResourceApproval {
        id: ResourceApprovalId::new(),
        request_id: request.id,
        request_revision: request.revision,
        approver_id: ActorId::new(),
        provider_binding: "synthetic-provider".into(),
        approved_resources: resources.clone(),
        approved_duration_seconds: 60,
        reason: "isolated original environment approval".into(),
        valid_until: at(now, valid_seconds)?,
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
            state: CapacityClaimState::Reserved,
            revision: Revision::new(1)?,
        },
        lease_id: LeaseId::new(),
    };
    store
        .approve(
            &Uuid::new_v4().to_string(),
            request.id,
            &approval,
            &allocation,
            ApprovalPolicy {
                min_duration_seconds: 1,
                max_duration_seconds: 3600,
            },
            "synthetic-approve",
        )
        .await?;
    store.claim_next_capacity_shell().await?.ok_or("claim")?;
    Ok((
        container,
        pool,
        store,
        credits,
        request,
        allocation.lease_id,
    ))
}
async fn prices(store: &PgResourceStore, price: &str) -> Result<()> {
    for (unit, quantity, value) in [
        (ResourceBillingUnit::CpuMillicoreSecond, 1000, price),
        (ResourceBillingUnit::MemoryByteSecond, 1, "0.000000"),
        (ResourceBillingUnit::StorageByteSecond, 1, "0.000000"),
    ] {
        store
            .create_rate(
                &Uuid::new_v4().to_string(),
                &CreateResourceRateRequest {
                    unit,
                    unit_quantity: quantity,
                    gpu_class: None,
                    gpu_mode: None,
                    builder_binding: None,
                    unit_price: Money {
                        currency: "CREDITS".into(),
                        amount: money(value)?,
                    },
                    effective_from: at(store.current_time().await?, -3600)?,
                    effective_until: None,
                },
            )
            .await?;
    }
    Ok(())
}
async fn allowance(credits: &PgCreditStore, project: ProjectId, value: &str) -> Result<()> {
    credits
        .admin(
            &Uuid::new_v4().to_string(),
            ActorId::new(),
            &[PlatformRole::PlatformAdmin],
            &CreditAdminOperation::Grant {
                project_id: project,
                amount: money(value)?,
                reason: "isolated environment allowance".into(),
            },
        )
        .await?;
    Ok(())
}
async fn activate(
    store: &PgResourceStore,
    lease: LeaseId,
    from: UtcTimestamp,
    until: UtcTimestamp,
) -> Result<contracts::resource::ResourceLease> {
    Ok(store
        .activate_lease(
            lease,
            Revision::new(1)?,
            from,
            until,
            ActorId::new(),
            "synthetic-activate",
        )
        .await?)
}
#[tokio::test]
async fn initial_missing_price_and_insufficient_funds_leave_no_grant_or_active_lease() -> Result<()>
{
    let (_c, pool, store, credits, request, lease) = fixture().await?;
    let from = store.current_time().await?;
    let until = at(from, 10)?;
    assert!(activate(&store, lease, from, until).await.is_err());
    prices(&store, "1.000000").await?;
    assert!(activate(&store, lease, from, until).await.is_err());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM resource.credit_holds")
            .fetch_one(&pool)
            .await?,
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM resource.resource_leases")
            .fetch_one(&pool)
            .await?,
        "allocating"
    );
    allowance(&credits, request.project_id, "50.000000").await?;
    let first = activate(&store, lease, from, until).await?;
    let replay = activate(&store, lease, from, until).await?;
    assert_eq!(first, replay);
    let grant = store.environment_credit_grant(OWNER, lease).await?;
    assert!(grant.can_authorize_execution);
    assert_eq!(grant.estimate.total.amount, money("10.000000")?);
    assert_eq!(
        credits.account(request.project_id).await?.reserved,
        money("10.000000")?
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM resource.credit_holds")
            .fetch_one(&pool)
            .await?,
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM resource.environment_credit_windows")
            .fetch_one(&pool)
            .await?,
        1
    );
    assert!(
        store
            .environment_credit_grant("wrong-owner", lease)
            .await
            .is_err()
    );
    Ok(())
}
#[tokio::test]
async fn renewal_replay_and_concurrent_rekeys_add_only_the_new_interval_to_one_hold() -> Result<()>
{
    let (_c, pool, store, credits, request, lease) = fixture().await?;
    prices(&store, "1.000000").await?;
    allowance(&credits, request.project_id, "50.000000").await?;
    let from = store.current_time().await?;
    let until = at(from, 10)?;
    let active = activate(&store, lease, from, until).await?;
    let initial = store.environment_credit_grant(OWNER, lease).await?;
    let extended = at(until, 5)?;
    let key = Uuid::new_v4().to_string();
    let renewed = store
        .renew_lease(&key, lease, active.revision, extended, "synthetic-renew")
        .await?;
    assert_eq!(
        store
            .renew_lease(&key, lease, active.revision, extended, "synthetic-renew")
            .await?,
        renewed
    );
    let current = store.environment_credit_grant(OWNER, lease).await?;
    assert_eq!(current.hold.id, initial.hold.id);
    assert_eq!(current.hold.amount, money("15.000000")?);
    assert_eq!(current.estimate.total.amount, money("5.000000")?);
    let one = store.clone();
    let two = store.clone();
    let next = at(extended, 5)?;
    let key_one = Uuid::new_v4().to_string();
    let key_two = Uuid::new_v4().to_string();
    let (a, b) = tokio::join!(
        one.renew_lease(&key_one, lease, renewed.revision, next, "parallel-one"),
        two.renew_lease(&key_two, lease, renewed.revision, next, "parallel-two")
    );
    assert_ne!(a.is_ok(), b.is_ok());
    assert_eq!(
        credits.account(request.project_id).await?.reserved,
        money("20.000000")?
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM resource.credit_holds")
            .fetch_one(&pool)
            .await?,
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM resource.environment_credit_windows")
            .fetch_one(&pool)
            .await?,
        3
    );
    assert!(sqlx::query("UPDATE resource.environment_credit_windows SET covered_until=covered_until+interval '1 second'").execute(&pool).await.is_err());
    Ok(())
}
#[tokio::test]
async fn failed_renewal_rolls_back_lease_and_money_and_unknown_keeps_the_original_deadline()
-> Result<()> {
    let (_c, pool, store, credits, request, lease) = fixture().await?;
    prices(&store, "1.000000").await?;
    allowance(&credits, request.project_id, "10.000000").await?;
    let from = store.current_time().await?;
    let until = at(from, 10)?;
    let active = activate(&store, lease, from, until).await?;
    let original = store.environment_credit_grant(OWNER, lease).await?;
    assert!(
        store
            .renew_lease(
                &Uuid::new_v4().to_string(),
                lease,
                active.revision,
                at(until, 5)?,
                "no-money"
            )
            .await
            .is_err()
    );
    assert_eq!(
        store.environment_credit_grant(OWNER, lease).await?,
        original
    );
    let unknown = credits
        .resolve(
            &Uuid::new_v4().to_string(),
            OWNER,
            original.hold.id,
            &CreditHoldResolutionRequest::Unknown {
                reason: "authoritative stop/time unknown".into(),
            },
        )
        .await?;
    let due = unknown.reconciliation_due_at.ok_or("unknown due")?;
    allowance(&credits, request.project_id, "50.000000").await?;
    assert!(
        store
            .renew_lease(
                &Uuid::new_v4().to_string(),
                lease,
                active.revision,
                at(until, 5)?,
                "unknown-funds"
            )
            .await
            .is_err()
    );
    let still = store.environment_credit_grant(OWNER, lease).await?;
    assert!(!still.can_authorize_execution);
    assert_eq!(still.hold.reconciliation_due_at, Some(due));
    assert_eq!(still.covered_until, until);
    assert_eq!(credits.expire_unknown_at(due).await?, 1);
    assert_eq!(
        credits.account(request.project_id).await?.reserved,
        FixedDecimal::zero()
    );
    assert!(
        store
            .renew_lease(
                &Uuid::new_v4().to_string(),
                lease,
                active.revision,
                at(until, 5)?,
                "released-funds"
            )
            .await
            .is_err()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM resource.environment_credit_windows")
            .fetch_one(&pool)
            .await?,
        1
    );
    Ok(())
}

#[tokio::test]
async fn original_prehold_is_consumed_once_without_an_extra_reservation() -> Result<()> {
    use contracts::resource::credits::CreditHoldRequest;
    let (_c, pool, store, credits, request, lease) = fixture().await?;
    prices(&store, "1.000000").await?;
    allowance(&credits, request.project_id, "50.000000").await?;
    let hold = credits
        .hold(
            "preexisting",
            OWNER,
            &CreditHoldRequest {
                project_id: request.project_id,
                request_id: request.id,
                business_key: "metered-usage".into(),
                amount: money("50.000000")?,
            },
        )
        .await?;
    let from = store.current_time().await?;
    let active = activate(&store, lease, from, at(from, 10)?).await?;
    assert_eq!(
        store.environment_approval_deadline(lease, from).await?,
        at(from, 60)?
    );
    assert_eq!(
        store.environment_credit_grant(OWNER, lease).await?.hold.id,
        hold.id
    );
    let grant = store.environment_credit_grant(OWNER, lease).await?;
    assert_eq!(grant.hold.amount, money("50.000000")?);
    assert_eq!(grant.estimate.total.amount, money("10.000000")?);
    assert_eq!(
        credits.account(request.project_id).await?.reserved,
        money("50.000000")?
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM resource.credit_ledger_entries WHERE contract->>'kind'='hold'"
        )
        .fetch_one(&pool)
        .await?,
        1
    );
    assert_eq!(activate(&store, lease, from, at(from, 10)?).await?, active);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM resource.credit_holds")
            .fetch_one(&pool)
            .await?,
        1
    );
    store
        .renew_lease(
            "reuse-original-buffer",
            lease,
            active.revision,
            at(from, 15)?,
            "same-prehold",
        )
        .await?;
    let renewed = store.environment_credit_grant(OWNER, lease).await?;
    assert_eq!(renewed.hold.id, hold.id);
    assert_eq!(renewed.hold.amount, money("50.000000")?);
    assert_eq!(
        credits.account(request.project_id).await?.reserved,
        money("50.000000")?
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM resource.credit_ledger_entries WHERE contract->>'kind'='hold'"
        )
        .fetch_one(&pool)
        .await?,
        1
    );
    Ok(())
}
#[tokio::test]
async fn renewal_uses_new_rate_version_and_keeps_total_continuous_coverage() -> Result<()> {
    let (_c, pool, store, credits, request, lease) = fixture().await?;
    prices(&store, "1.000000").await?;
    allowance(&credits, request.project_id, "100.000000").await?;
    let from = store.current_time().await?;
    let until = at(from, 10)?;
    let active = activate(&store, lease, from, until).await?;
    let original = store.environment_credit_grant(OWNER, lease).await?;
    let new = store
        .create_rate(
            "new-cpu-version",
            &CreateResourceRateRequest {
                unit: ResourceBillingUnit::CpuMillicoreSecond,
                unit_quantity: 1000,
                gpu_class: None,
                gpu_mode: None,
                builder_binding: None,
                unit_price: Money {
                    currency: "CREDITS".into(),
                    amount: money("3.000000")?,
                },
                effective_from: until,
                effective_until: None,
            },
        )
        .await?;
    let renewed = store
        .renew_lease(
            "new-window",
            lease,
            active.revision,
            at(until, 5)?,
            "versioned-renew",
        )
        .await?;
    let grant = store.environment_credit_grant(OWNER, lease).await?;
    assert_eq!(grant.covered_from, from);
    assert_eq!(grant.covered_until, at(until, 5)?);
    assert_eq!(grant.lease_revision, renewed.revision);
    assert_eq!(grant.estimate.input.compute_from, until);
    assert_eq!(grant.estimate.total.amount, money("15.000000")?);
    assert!(
        grant
            .estimate
            .lines
            .iter()
            .any(|line| line.rate_id == new.id)
    );
    assert_eq!(grant.hold.id, original.hold.id);
    assert_eq!(grant.hold.amount, money("25.000000")?);
    assert!(grant.can_authorize_execution);
    let first:serde_json::Value=sqlx::query_scalar("SELECT estimate FROM resource.environment_credit_windows WHERE lease_id=$1 AND lease_revision=2").bind(lease.as_uuid()).fetch_one(&pool).await?;
    assert_eq!(
        serde_json::from_value::<contracts::resource::costing::ResourceCostEstimate>(first)?,
        original.estimate
    );
    Ok(())
}
#[tokio::test]
async fn expired_financial_window_marks_unknown_once_and_cannot_be_revived() -> Result<()> {
    let (_c, pool, store, credits, request, lease) = fixture().await?;
    prices(&store, "1.000000").await?;
    allowance(&credits, request.project_id, "20.000000").await?;
    let from = store.current_time().await?;
    let active = activate(&store, lease, from, at(from, 2)?).await?;
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    assert!(
        !store
            .environment_credit_grant(OWNER, lease)
            .await?
            .can_authorize_execution
    );
    assert!(store.reconcile_environment_funding_deadlines_once().await?);
    let first = store.environment_credit_grant(OWNER, lease).await?;
    let seen = first.hold.unknown_since.ok_or("first unknown")?;
    let due = first.hold.reconciliation_due_at.ok_or("unknown deadline")?;
    assert_eq!(due, at(seen, 3600)?);
    assert!(!store.reconcile_environment_funding_deadlines_once().await?);
    assert!(
        store
            .renew_lease(
                "expired-renew",
                lease,
                active.revision,
                at(from, 7)?,
                "expired-reject"
            )
            .await
            .is_err()
    );
    assert_eq!(
        store
            .environment_credit_grant(OWNER, lease)
            .await?
            .hold
            .reconciliation_due_at,
        Some(due)
    );
    assert_eq!(credits.expire_unknown_at(due).await?, 1);
    assert_eq!(
        credits.account(request.project_id).await?.reserved,
        FixedDecimal::zero()
    );
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM resource.resource_leases")
            .fetch_one(&pool)
            .await?,
        "active"
    );
    assert!(
        !store
            .environment_credit_grant(OWNER, lease)
            .await?
            .can_authorize_execution
    );
    Ok(())
}

#[tokio::test]
async fn estimated_original_hold_and_consent_survive_activation_without_a_second_hold() -> Result<()>
{
    use contracts::resource::costing::{EstimatedCreditHoldRequest, ResourceCostEstimateRequest};
    let (_c, pool, store, credits, request, lease) = fixture().await?;
    prices(&store, "1.000000").await?;
    allowance(&credits, request.project_id, "100.000000").await?;
    let from = store.current_time().await?;
    let estimate = store
        .estimate_cost(
            request.project_id,
            &ResourceCostEstimateRequest {
                resources: request.requested_resources.clone(),
                compute_from: from,
                compute_until: at(from, 60)?,
                storage_from: from,
                storage_until: at(from, 60)?,
            },
        )
        .await?;
    let original = store
        .hold_estimated_cost(
            "estimated-original",
            OWNER,
            true,
            &EstimatedCreditHoldRequest {
                request_id: request.id,
                expected_estimate: estimate.clone(),
            },
        )
        .await?;
    activate(&store, lease, from, at(from, 10)?).await?;
    let grant = store.environment_credit_grant(OWNER, lease).await?;
    assert_eq!(grant.hold.id, original.hold.id);
    assert_eq!(grant.hold.amount, money("60.000000")?);
    let snapshot: serde_json::Value = sqlx::query_scalar(
        "SELECT snapshot FROM resource.credit_cost_snapshots WHERE request_id=$1",
    )
    .bind(request.id.as_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        serde_json::from_value::<contracts::resource::costing::EstimatedCreditHold>(snapshot)?,
        original
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM resource.credit_holds")
            .fetch_one(&pool)
            .await?,
        1
    );
    Ok(())
}
#[tokio::test]
async fn foreign_original_hold_cannot_activate_or_create_a_second_hold() -> Result<()> {
    use contracts::resource::credits::CreditHoldRequest;
    let (_c, pool, store, credits, request, lease) = fixture().await?;
    prices(&store, "1.000000").await?;
    allowance(&credits, request.project_id, "100.000000").await?;
    credits
        .hold(
            "foreign-original",
            "another-synthetic-owner",
            &CreditHoldRequest {
                project_id: request.project_id,
                request_id: request.id,
                business_key: "metered-usage".into(),
                amount: money("50.000000")?,
            },
        )
        .await?;
    let from = store.current_time().await?;
    assert!(activate(&store, lease, from, at(from, 10)?).await.is_err());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM resource.environment_credit_windows")
            .fetch_one(&pool)
            .await?,
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM resource.credit_holds")
            .fetch_one(&pool)
            .await?,
        1
    );
    assert_eq!(store.load_lease(lease).await?.revision, Revision::new(1)?);
    Ok(())
}

#[tokio::test]
async fn unactivated_original_prehold_releases_when_its_exact_approval_expires() -> Result<()> {
    use contracts::resource::credits::{CreditHoldRequest, CreditHoldState};
    let (_c, pool, store, credits, request, lease) = fixture_with_approval_seconds(2).await?;
    allowance(&credits, request.project_id, "20.000000").await?;
    let original = credits
        .hold(
            "unactivated-original",
            OWNER,
            &CreditHoldRequest {
                project_id: request.project_id,
                request_id: request.id,
                business_key: "metered-usage".into(),
                amount: money("5.000000")?,
            },
        )
        .await?;
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    assert!(store.reconcile_environment_funding_deadlines_once().await?);
    assert!(!store.reconcile_environment_funding_deadlines_once().await?);
    let hold = credits
        .holds(request.project_id, 1, 25, 0)
        .await?
        .items
        .into_iter()
        .find(|h| h.id == original.id)
        .ok_or("original hold")?;
    assert_eq!(hold.state, CreditHoldState::Released);
    assert!(!hold.measurement_unknown);
    assert_eq!(
        credits.account(request.project_id).await?.reserved,
        FixedDecimal::zero()
    );
    assert_eq!(
        store.load_lease(lease).await?.state,
        contracts::resource::ResourceLeaseState::Allocating
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM resource.environment_credit_windows")
            .fetch_one(&pool)
            .await?,
        0
    );
    Ok(())
}
