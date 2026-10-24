//! Synthetic `PostgreSQL` scenarios use the same expiration and settlement transactions.
#![allow(
    clippy::too_many_lines,
    clippy::expect_used,
    reason = "accounting transactions keep assertions together"
)]
use contracts::resource::credits::{
    CreditAdminOperation, CreditHoldRequest, CreditHoldResolutionRequest, CreditHoldState,
};
use contracts::resource::{
    FixedDecimal, ResourceRequest, ResourceRequestState, ResourceTarget, WorkloadResources,
};
use contracts::{
    ActorId, EnvironmentId, EventId, PlatformRole, ProjectId, ReleaseId, ResourceRequestId,
    Revision, UtcTimestamp,
};
use resource_service::{credits::PgCreditStore, store::PgResourceStore};
use sqlx::{PgPool, postgres::PgPoolOptions};
use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;
use time::Duration;
use uuid::Uuid;
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
fn amount(value: &str) -> FixedDecimal {
    FixedDecimal::parse(value).expect("six-place fixture")
}
fn at(t: UtcTimestamp, millis: i64) -> UtcTimestamp {
    UtcTimestamp::from_utc(t.get() + Duration::milliseconds(millis))
        .expect("millisecond UTC fixture")
}
async fn fixture() -> Result<(
    testcontainers::ContainerAsync<Postgres>,
    PgPool,
    PgCreditStore,
    ProjectId,
    Uuid,
)> {
    let c = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(32)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            c.get_host_port_ipv4(5432).await?
        ))
        .await?;
    let mut files = std::fs::read_dir(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations/resource"),
    )?
    .map(|entry| entry.map(|entry| entry.path()))
    .collect::<std::result::Result<Vec<_>, _>>()?;
    files.sort();
    let mut ddl = "CREATE SCHEMA resource;SET search_path TO resource;\n".to_owned();
    for file in files {
        if file.extension().is_some_and(|e| e == "sql") {
            ddl.push_str(&std::fs::read_to_string(file)?);
            ddl.push('\n');
        }
    }
    sqlx::raw_sql(sqlx::AssertSqlSafe(&*ddl))
        .execute(&pool)
        .await?;
    let resource = PgResourceStore::new(pool.clone());
    let now = resource.current_time().await?;
    let project = ProjectId::new();
    let id = ResourceRequestId::new();
    resource
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
            },
            "synthetic-unknown",
        )
        .await?;
    let credits = PgCreditStore::new(pool.clone());
    credits
        .admin(
            "synthetic-grant",
            ActorId::new(),
            &[PlatformRole::PlatformAdmin],
            &CreditAdminOperation::Grant {
                project_id: project,
                amount: amount("10.000000"),
                reason: "synthetic deadline allowance".into(),
            },
        )
        .await?;
    let hold = credits
        .hold(
            "synthetic-hold",
            "environment",
            &CreditHoldRequest {
                project_id: project,
                request_id: id,
                business_key: "synthetic-measurement".into(),
                amount: amount("5.000000"),
            },
        )
        .await?;
    Ok((c, pool, credits, project, hold.id))
}
async fn mark_unknown(
    credits: &PgCreditStore,
    id: Uuid,
    key: &str,
) -> Result<contracts::resource::credits::CreditHold> {
    Ok(credits
        .resolve(
            key,
            "environment",
            id,
            &CreditHoldResolutionRequest::Unknown {
                event_id: None,
                reason: "executor timing unavailable".into(),
            },
        )
        .await?)
}
fn settle(event: EventId, value: &str) -> CreditHoldResolutionRequest {
    CreditHoldResolutionRequest::Settle {
        event_id: event,
        amount: amount(value),
        finalize: true,
    }
}
#[tokio::test]
async fn exact_one_hour_boundary_releases_once_preserves_unknown_and_never_resets_on_retry()
-> Result<()> {
    let (_c, pool, credits, project, id) = fixture().await?;
    let first = mark_unknown(&credits, id, "unknown-first").await?;
    let start = first.unknown_since.ok_or("unknown origin missing")?;
    let due = first.reconciliation_due_at.ok_or("deadline missing")?;
    assert_eq!((due.get() - start.get()).whole_seconds(), 3600);
    assert_eq!(mark_unknown(&credits, id, "unknown-first").await?, first);
    let retry = mark_unknown(&credits, id, "unknown-rekey").await?;
    assert_eq!(retry.unknown_since, Some(start));
    assert_eq!(retry.reconciliation_due_at, Some(due));
    assert_eq!(credits.expire_unknown_at(at(due, -1)).await?, 0);
    assert_eq!(credits.account(project).await?.reserved, amount("5.000000"));
    assert_eq!(credits.expire_unknown_at(due).await?, 1);
    assert_eq!(credits.expire_unknown_at(at(due, 1)).await?, 0);
    let account = credits.account(project).await?;
    assert_eq!(account.available, amount("10.000000"));
    assert_eq!(account.reserved, FixedDecimal::zero());
    assert_eq!(account.unknown_count, 1);
    let hold = credits
        .holds(project, 1, 100, 0)
        .await?
        .items
        .into_iter()
        .find(|h| h.id == id)
        .ok_or("hold missing")?;
    assert_eq!(hold.state, CreditHoldState::Released);
    assert!(hold.measurement_unknown);
    assert_eq!(hold.expired_unknown_at, Some(due));
    let releases:i64=sqlx::query_scalar("SELECT count(*) FROM resource.credit_ledger_entries WHERE hold_id=$1 AND contract->>'kind'='release'").bind(id).fetch_one(&pool).await?;
    assert_eq!(releases, 1);
    Ok(())
}
#[tokio::test]
async fn known_before_deadline_settles_normally_and_late_replays_stay_platform_cost() -> Result<()>
{
    let (_c, _pool, credits, project, id) = fixture().await?;
    let unknown = mark_unknown(&credits, id, "timing-unknown").await?;
    let due = unknown.reconciliation_due_at.ok_or("deadline missing")?;
    let event = EventId::new();
    let measured = credits
        .resolve(
            "known-before",
            "environment",
            id,
            &settle(event, "3.000000"),
        )
        .await?;
    assert!(!measured.measurement_unknown);
    assert_eq!(credits.expire_unknown_at(due).await?, 0);
    let account = credits.account(project).await?;
    assert_eq!(account.spent, amount("3.000000"));
    assert_eq!(account.platform_absorbed, FixedDecimal::zero());
    assert_eq!(
        credits
            .resolve("known-rekey", "environment", id, &settle(event, "3.000000"))
            .await?,
        measured
    );
    Ok(())
}
#[tokio::test]
async fn late_known_result_expires_inline_even_before_maintenance_and_debits_only_platform()
-> Result<()> {
    let (_c, pool, credits, project, id) = fixture().await?;
    mark_unknown(&credits, id, "past-unknown").await?;
    // Controlled historical timestamps represent an already elapsed hour, without sleeping.
    sqlx::query("UPDATE resource.credit_holds SET unknown_since=date_trunc('milliseconds',clock_timestamp())-interval '1 hour 1 second',reconciliation_due_at=date_trunc('milliseconds',clock_timestamp())-interval '1 second' WHERE hold_id=$1").bind(id).execute(&pool).await?;
    let event = EventId::new();
    let known = credits
        .resolve("late-known", "environment", id, &settle(event, "7.000000"))
        .await?;
    assert!(!known.measurement_unknown);
    assert!(known.expired_unknown_at.is_some());
    let account = credits.account(project).await?;
    assert_eq!(account.balance, amount("10.000000"));
    assert_eq!(account.spent, FixedDecimal::zero());
    assert_eq!(account.platform_absorbed, amount("7.000000"));
    assert_eq!(
        credits
            .resolve("late-rekey", "environment", id, &settle(event, "7.000000"))
            .await?,
        known
    );
    assert_eq!(credits.expire_unknown_once().await?, 0);
    assert_eq!(
        credits.account(project).await?.platform_absorbed,
        amount("7.000000")
    );
    Ok(())
}
#[tokio::test]
async fn concurrent_expiration_and_same_event_rekeys_release_and_absorb_exactly_once() -> Result<()>
{
    let (_c, pool, credits, project, id) = fixture().await?;
    mark_unknown(&credits, id, "race-unknown").await?;
    sqlx::query("UPDATE resource.credit_holds SET unknown_since=date_trunc('milliseconds',clock_timestamp())-interval '1 hour 1 second',reconciliation_due_at=date_trunc('milliseconds',clock_timestamp())-interval '1 second' WHERE hold_id=$1").bind(id).execute(&pool).await?;
    let event = EventId::new();
    let mut tasks = tokio::task::JoinSet::new();
    for index in 0..16 {
        let credits = credits.clone();
        tasks.spawn(async move {
            credits
                .resolve(
                    &format!("race-known-{index}"),
                    "environment",
                    id,
                    &settle(event, "4.000000"),
                )
                .await
                .map(|_| ())
        });
    }
    for _ in 0..8 {
        let credits = credits.clone();
        tasks.spawn(async move { credits.expire_unknown_once().await.map(|_| ()) });
    }
    while let Some(result) = tasks.join_next().await {
        result??;
    }
    let account = credits.account(project).await?;
    assert_eq!(account.balance, amount("10.000000"));
    assert_eq!(account.reserved, FixedDecimal::zero());
    assert_eq!(account.spent, FixedDecimal::zero());
    assert_eq!(account.platform_absorbed, amount("4.000000"));
    assert_eq!(account.unknown_count, 0);
    let releases:i64=sqlx::query_scalar("SELECT count(*) FROM resource.credit_ledger_entries WHERE hold_id=$1 AND contract->>'kind'='release'").bind(id).fetch_one(&pool).await?;
    assert_eq!(releases, 1);
    let settles:i64=sqlx::query_scalar("SELECT count(*) FROM resource.credit_ledger_entries WHERE hold_id=$1 AND project_id=$2 AND contract->>'kind'='settle'").bind(id).bind(project.as_uuid()).fetch_one(&pool).await?;
    assert_eq!(settles, 1);
    Ok(())
}

#[tokio::test]
async fn migration_recovers_the_first_immutable_unknown_fact_instead_of_the_last_retry()
-> Result<()> {
    let (_c, pool, credits, project, id) = fixture().await?;
    mark_unknown(&credits, id, "legacy-first").await?;
    mark_unknown(&credits, id, "legacy-retry").await?;
    let original:time::OffsetDateTime=sqlx::query_scalar("SELECT min(created_at) FROM resource.credit_ledger_entries WHERE hold_id=$1 AND contract->>'kind'='unknown'").bind(id).fetch_one(&pool).await?;
    // Reconstruct the previous schema in this isolated database; the immutable ledger stays intact.
    sqlx::raw_sql("ALTER TABLE resource.credit_holds DROP COLUMN unknown_since,DROP COLUMN reconciliation_due_at,DROP COLUMN expired_unknown_at;").execute(&pool).await?;
    sqlx::raw_sql(include_str!(
        "../../../migrations/resource/0019_unknown_usage_deadline.sql"
    ))
    .execute(&pool)
    .await?;
    let recovered = credits
        .holds(project, 1, 100, 0)
        .await?
        .items
        .into_iter()
        .find(|h| h.id == id)
        .ok_or("recovered hold missing")?;
    assert_eq!(
        recovered.unknown_since.ok_or("origin missing")?.get(),
        original
    );
    assert_eq!(
        recovered
            .reconciliation_due_at
            .ok_or("deadline missing")?
            .get(),
        original + Duration::hours(1)
    );
    assert!(recovered.measurement_unknown);
    assert_eq!(recovered.remaining, amount("5.000000"));
    Ok(())
}
