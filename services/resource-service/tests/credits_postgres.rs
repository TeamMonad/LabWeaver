//! Real `PostgreSQL` Credits tests use only fresh synthetic projects and local containers.
#![allow(
    clippy::too_many_lines,
    clippy::expect_used,
    reason = "transaction scenarios keep their assertions together"
)]
use contracts::resource::credits::{
    CreditAdminOperation, CreditHoldRequest, CreditHoldResolutionRequest, CreditHoldState,
};
use contracts::resource::{
    FixedDecimal, ResourceRequest, ResourceRequestState, ResourceTarget, WorkloadResources,
};
use contracts::{
    ActorId, EnvironmentId, EventId, PlatformRole, ProjectId, ReleaseId, ResourceRequestId,
    Revision,
};
use resource_service::credits::{CreditError, PgCreditStore};
use resource_service::store::PgResourceStore;
use sqlx::{PgPool, postgres::PgPoolOptions};
use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;
use uuid::Uuid;
const ADMIN: &[PlatformRole] = &[PlatformRole::PlatformAdmin];
fn amount(value: &str) -> FixedDecimal {
    FixedDecimal::parse(value).expect("fixed six-place fixture")
}
async fn fixture() -> Result<
    (
        testcontainers::ContainerAsync<Postgres>,
        PgPool,
        PgCreditStore,
    ),
    Box<dyn std::error::Error>,
> {
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(32)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            container.get_host_port_ipv4(5432).await?
        ))
        .await?;
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations/resource");
    let mut files = std::fs::read_dir(root)?
        .map(|e| e.map(|e| e.path()))
        .collect::<Result<Vec<_>, _>>()?;
    files.sort();
    let mut migration = "CREATE SCHEMA resource; SET search_path TO resource;\n".to_owned();
    for file in files {
        if file.extension().is_some_and(|e| e == "sql") {
            migration.push_str(&std::fs::read_to_string(file)?);
            migration.push('\n');
        }
    }
    sqlx::raw_sql(&migration).execute(&pool).await?;
    let store = PgCreditStore::new(pool.clone());
    Ok((container, pool, store))
}
async fn request(
    pool: &PgPool,
    project: ProjectId,
) -> Result<ResourceRequestId, Box<dyn std::error::Error>> {
    let store = PgResourceStore::new(pool.clone());
    let now = store.current_time().await?;
    let id = ResourceRequestId::new();
    let request = ResourceRequest {
        id,
        generation: 1,
        request_key: id.to_string(),
        requester_id: ActorId::new(),
        course_id: None,
        project_id: project,
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
    };
    store
        .create(&id.to_string(), &request, "credits-fixture")
        .await?;
    Ok(id)
}
async fn grant(store: &PgCreditStore, project: ProjectId, value: &str) -> Result<(), CreditError> {
    store
        .admin(
            &Uuid::now_v7().to_string(),
            ActorId::new(),
            ADMIN,
            &CreditAdminOperation::Grant {
                project_id: project,
                amount: amount(value),
                reason: "synthetic allowance".into(),
            },
        )
        .await?;
    Ok(())
}
fn settle(event: EventId, value: &str, finalize: bool) -> CreditHoldResolutionRequest {
    CreditHoldResolutionRequest::Settle {
        event_id: event,
        amount: amount(value),
        finalize,
    }
}

#[tokio::test]
async fn concurrent_holds_cannot_spend_the_same_available_balance()
-> Result<(), Box<dyn std::error::Error>> {
    let (_container, pool, store) = fixture().await?;
    let project = ProjectId::new();
    grant(&store, project, "10.000000").await?;
    let mut tasks = tokio::task::JoinSet::new();
    for i in 0..32 {
        let request_id = request(&pool, project).await?;
        let store = store.clone();
        tasks.spawn(async move {
            store
                .hold(
                    &format!("parallel-{i}"),
                    "evaluation",
                    &CreditHoldRequest {
                        project_id: project,
                        request_id,
                        business_key: format!("job-{i}"),
                        amount: amount("1.000000"),
                    },
                )
                .await
        });
    }
    let mut accepted = 0;
    while let Some(result) = tasks.join_next().await {
        match result? {
            Ok(_) => accepted += 1,
            Err(CreditError::Insufficient) => {}
            Err(error) => return Err(error.into()),
        }
    }
    assert_eq!(accepted, 10);
    let account = store.account(project).await?;
    assert_eq!(account.available, amount("0.000000"));
    assert_eq!(account.reserved, amount("10.000000"));
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM resource.credit_holds")
        .fetch_one(&pool)
        .await?;
    assert_eq!(count, 10);
    Ok(())
}

#[tokio::test]
async fn settlement_replays_unknown_and_out_of_order_release_never_overdraw()
-> Result<(), Box<dyn std::error::Error>> {
    let (_container, pool, store) = fixture().await?;
    let project = ProjectId::new();
    grant(&store, project, "10.000000").await?;
    let request_id = request(&pool, project).await?;
    let input = CreditHoldRequest {
        project_id: project,
        request_id,
        business_key: "metered-usage".into(),
        amount: amount("6.000000"),
    };
    let hold = store.hold("credits-hold-1", "evaluation", &input).await?;
    assert_eq!(
        store.hold("hold-rebuilt", "evaluation", &input).await?.id,
        hold.id
    );
    let unknown = CreditHoldResolutionRequest::Unknown {
        reason: "provider measurement missing".into(),
    };
    store
        .resolve("unknown-1", "evaluation", hold.id, &unknown)
        .await?;
    let account = store.account(project).await?;
    assert_eq!(account.unknown, amount("6.000000"));
    assert_eq!(account.pending, amount("0.000000"));
    let event = EventId::new();
    let partial = settle(event, "2.000000", false);
    store
        .resolve("partial-1", "evaluation", hold.id, &partial)
        .await?;
    // Same source event under a different HTTP retry key still produces one debit.
    store
        .resolve("partial-rekeyed", "evaluation", hold.id, &partial)
        .await?;
    assert!(matches!(
        store
            .resolve(
                "changed-event",
                "evaluation",
                hold.id,
                &settle(event, "3.000000", false)
            )
            .await,
        Err(CreditError::Conflict)
    ));
    store
        .resolve(
            "release-1",
            "evaluation",
            hold.id,
            &CreditHoldResolutionRequest::Release {
                reason: "physical release confirmed".into(),
            },
        )
        .await?;
    let late = store
        .resolve(
            "late-cost",
            "evaluation",
            hold.id,
            &settle(EventId::new(), "9.000000", true),
        )
        .await?;
    assert_eq!(late.state, CreditHoldState::Released);
    let account = PgCreditStore::new(pool.clone()).account(project).await?;
    assert_eq!(account.balance, amount("8.000000"));
    assert_eq!(account.spent, amount("2.000000"));
    assert_eq!(account.reserved, amount("0.000000"));
    assert_eq!(account.platform_absorbed, amount("9.000000"));
    let ledger = store.ledger(project, 1, 100, 0).await?;
    assert_eq!(
        ledger.items.iter().filter(|e| e.kind == "settle").count(),
        2
    );
    let balanced:bool=sqlx::query_scalar("SELECT NOT EXISTS(SELECT operation_id FROM resource.credit_ledger_entries GROUP BY operation_id HAVING sum(balance_delta)<>0)").fetch_one(&pool).await?;
    assert!(balanced);
    Ok(())
}

#[tokio::test]
async fn transfer_is_atomic_and_reversal_appends_without_rewriting_history()
-> Result<(), Box<dyn std::error::Error>> {
    let (_container, pool, store) = fixture().await?;
    let from = ProjectId::new();
    let to = ProjectId::new();
    grant(&store, from, "5.000000").await?;
    let actor = ActorId::new();
    let bad = CreditAdminOperation::Transfer {
        project_id: from,
        destination_project_id: to,
        amount: amount("6.000000"),
        reason: "synthetic transfer".into(),
    };
    assert!(matches!(
        store.admin("bad-transfer", actor, ADMIN, &bad).await,
        Err(CreditError::Insufficient)
    ));
    assert_eq!(store.account(from).await?.balance, amount("5.000000"));
    assert_eq!(store.account(to).await?.balance, amount("0.000000"));
    let input = CreditAdminOperation::Transfer {
        project_id: from,
        destination_project_id: to,
        amount: amount("3.000000"),
        reason: "synthetic transfer".into(),
    };
    let entries = store.admin("transfer-1", actor, ADMIN, &input).await?;
    assert_eq!(entries.len(), 2);
    assert_eq!(
        store.admin("transfer-1", actor, ADMIN, &input).await?,
        entries
    );
    let reversal = CreditAdminOperation::Reverse {
        entry_id: entries[0].id,
        reason: "undo wrong synthetic project".into(),
    };
    let request_id = request(&pool, to).await?;
    let hold = store
        .hold(
            "transfer-destination-hold",
            "evaluation",
            &CreditHoldRequest {
                project_id: to,
                request_id,
                business_key: "blocked-reversal".into(),
                amount: amount("3.000000"),
            },
        )
        .await?;
    // The source refund executes first, but destination failure rolls it back too.
    assert!(matches!(
        store.admin("reverse-1", actor, ADMIN, &reversal).await,
        Err(CreditError::Insufficient)
    ));
    assert_eq!(store.account(from).await?.balance, amount("2.000000"));
    store
        .resolve(
            "destination-release",
            "evaluation",
            hold.id,
            &CreditHoldResolutionRequest::Release {
                reason: "synthetic cleanup confirmed".into(),
            },
        )
        .await?;
    let reversed = store.admin("reverse-1", actor, ADMIN, &reversal).await?;
    assert_eq!(reversed.len(), 2);
    assert_eq!(store.account(from).await?.balance, amount("5.000000"));
    assert_eq!(store.account(to).await?.balance, amount("0.000000"));
    assert!(matches!(
        store.admin("reverse-again", actor, ADMIN, &reversal).await,
        Err(CreditError::Conflict)
    ));
    assert!(
        sqlx::query("UPDATE resource.credit_ledger_entries SET balance_delta=0 WHERE entry_id=$1")
            .bind(entries[0].id)
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM resource.credit_ledger_entries WHERE entry_id=$1")
            .bind(entries[0].id)
            .execute(&pool)
            .await
            .is_err()
    );
    let saved: serde_json::Value =
        sqlx::query_scalar("SELECT contract FROM resource.credit_ledger_entries WHERE entry_id=$1")
            .bind(entries[0].id)
            .fetch_one(&pool)
            .await?;
    assert_eq!(saved, serde_json::to_value(&entries[0])?);
    Ok(())
}

#[tokio::test]
async fn admin_scope_owner_scope_and_reason_are_required() -> Result<(), Box<dyn std::error::Error>>
{
    let (_container, pool, store) = fixture().await?;
    let project = ProjectId::new();
    let actor = ActorId::new();
    let input = CreditAdminOperation::Grant {
        project_id: project,
        amount: amount("5.000000"),
        reason: "allowance".into(),
    };
    assert!(matches!(
        store
            .admin("member-grant", actor, &[PlatformRole::Student], &input)
            .await,
        Err(CreditError::Denied)
    ));
    assert!(matches!(
        store
            .admin(
                "no-reason",
                actor,
                ADMIN,
                &CreditAdminOperation::Adjust {
                    project_id: project,
                    amount: amount("1.000000"),
                    reason: " ".into()
                }
            )
            .await,
        Err(CreditError::Invalid)
    ));
    grant(&store, project, "5.000000").await?;
    let request_id = request(&pool, project).await?;
    assert!(matches!(
        store
            .hold(
                "wrong-project",
                "evaluation",
                &CreditHoldRequest {
                    project_id: ProjectId::new(),
                    request_id,
                    business_key: "cost".into(),
                    amount: amount("1.000000")
                }
            )
            .await,
        Err(CreditError::Denied)
    ));
    let hold = store
        .hold(
            "owned-hold",
            "evaluation",
            &CreditHoldRequest {
                project_id: project,
                request_id,
                business_key: "cost".into(),
                amount: amount("3.000000"),
            },
        )
        .await?;
    assert!(matches!(
        store
            .resolve(
                "wrong-owner",
                "agent",
                hold.id,
                &settle(EventId::new(), "1.000000", true)
            )
            .await,
        Err(CreditError::Denied)
    ));
    let adjustment = CreditAdminOperation::Adjust {
        project_id: project,
        amount: amount("-3.000000"),
        reason: "cannot spend held funds".into(),
    };
    assert!(matches!(
        store.admin("held-adjust", actor, ADMIN, &adjustment).await,
        Err(CreditError::Insufficient)
    ));
    assert_eq!(store.account(project).await?.available, amount("2.000000"));
    store
        .resolve(
            "credits-overrun",
            "evaluation",
            hold.id,
            &settle(EventId::new(), "7.000001", true),
        )
        .await?;
    let account = store.account(project).await?;
    assert_eq!(account.balance, amount("2.000000"));
    assert_eq!(account.platform_absorbed, amount("4.000001"));
    Ok(())
}

#[tokio::test]
async fn concurrent_event_rekeys_debit_once_and_database_rejects_unbalanced_commit()
-> Result<(), Box<dyn std::error::Error>> {
    let (_container, pool, store) = fixture().await?;
    let project = ProjectId::new();
    grant(&store, project, "8.000000").await?;
    let request_id = request(&pool, project).await?;
    let hold = store
        .hold(
            "event-race-hold",
            "evaluation",
            &CreditHoldRequest {
                project_id: project,
                request_id,
                business_key: "event-race".into(),
                amount: amount("5.000000"),
            },
        )
        .await?;
    let event = EventId::new();
    let mut tasks = tokio::task::JoinSet::new();
    for i in 0..16 {
        let store = store.clone();
        tasks.spawn(async move {
            store
                .resolve(
                    &format!("event-race-{i}"),
                    "evaluation",
                    hold.id,
                    &settle(event, "2.000000", false),
                )
                .await
        });
    }
    while let Some(result) = tasks.join_next().await {
        result??;
    }
    let account = store.account(project).await?;
    assert_eq!(account.spent, amount("2.000000"));
    assert_eq!(account.reserved, amount("3.000000"));
    let event_rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM resource.credit_ledger_entries WHERE contract->>'sourceEventId'=$1",
    )
    .bind(event.to_string())
    .fetch_one(&pool)
    .await?;
    assert_eq!(event_rows, 2);
    let mut tx = pool.begin().await?;
    sqlx::query("INSERT INTO resource.credit_ledger_entries(entry_id,operation_id,project_id,balance_delta,reserved_delta,spent_delta,platform_absorbed,contract) VALUES($1,$2,$3,1,0,0,0,'{}')").bind(Uuid::now_v7()).bind(Uuid::now_v7()).bind(project.as_uuid()).execute(&mut *tx).await?;
    assert!(tx.commit().await.is_err());
    assert!(
        sqlx::query("TRUNCATE resource.credit_ledger_entries")
            .execute(&pool)
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn recovery_releases_deadline_cancelled_preexecution_holds_once()
-> Result<(), Box<dyn std::error::Error>> {
    let (_container, pool, store) = fixture().await?;
    let project = ProjectId::new();
    grant(&store, project, "5.000000").await?;
    let request_id = request(&pool, project).await?;
    store
        .hold(
            "deadline-recovery-hold",
            "evaluation",
            &CreditHoldRequest {
                project_id: project,
                request_id,
                business_key: "deadline".into(),
                amount: amount("4.000000"),
            },
        )
        .await?;
    assert!(!store.reconcile_terminal_once().await?);
    // Simulate the persisted request transition from the independently owned deadline worker.
    let mut request = PgResourceStore::new(pool.clone()).load(request_id).await?;
    request.state = ResourceRequestState::Cancelled;
    request.revision = Revision::new(2)?;
    sqlx::query("UPDATE resource.resource_requests SET state='cancelled',revision=2,contract=$2 WHERE request_id=$1").bind(request_id.as_uuid()).bind(serde_json::to_value(&request)?).execute(&pool).await?;
    assert!(store.reconcile_terminal_once().await?);
    assert!(!store.reconcile_terminal_once().await?);
    assert_eq!(store.account(project).await?.available, amount("5.000000"));
    Ok(())
}
