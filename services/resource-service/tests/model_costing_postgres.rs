//! Real `PostgreSQL` model admission and settlement scenarios.
#![allow(clippy::expect_used, clippy::too_many_lines)]
use contracts::http::CreateResourceRateRequest;
use contracts::resource::credits::CreditAdminOperation;
use contracts::resource::model_costing::{
    ModelCostAdmissionRequest, ModelTokenQuantities, ModelUsageMeasurement,
};
use contracts::resource::{FixedDecimal, Money, ResourceBillingUnit};
use contracts::{EventId, PlatformRole, ProjectId, UtcTimestamp};
use resource_service::credits::PgCreditStore;
use resource_service::store::{PgResourceStore, ResourceStoreError};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;
use time::Duration;
use uuid::Uuid;

fn amount(value: &str) -> FixedDecimal {
    FixedDecimal::parse(value).expect("fixed fixture")
}

async fn fixture() -> Result<
    (
        testcontainers::ContainerAsync<Postgres>,
        PgPool,
        PgResourceStore,
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
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    files.sort();
    let mut ddl = "CREATE SCHEMA resource; SET search_path TO resource;\n".to_owned();
    for file in files {
        if file.extension().is_some_and(|extension| extension == "sql") {
            ddl.push_str(&std::fs::read_to_string(file)?);
            ddl.push('\n');
        }
    }
    sqlx::raw_sql(sqlx::AssertSqlSafe(ddl))
        .execute(&pool)
        .await?;
    Ok((
        container,
        pool.clone(),
        PgResourceStore::new(pool.clone()),
        PgCreditStore::new(pool),
    ))
}

async fn configure_rates(
    store: &PgResourceStore,
    model: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let now = store.current_time().await?;
    for (unit, price) in [
        (ResourceBillingUnit::LlmInputToken, "1.000000"),
        (ResourceBillingUnit::LlmOutputToken, "2.000000"),
        (ResourceBillingUnit::LlmCacheReadToken, "3.000000"),
        (ResourceBillingUnit::LlmCacheWriteToken, "4.000000"),
    ] {
        store
            .create_rate(
                &Uuid::now_v7().to_string(),
                &CreateResourceRateRequest {
                    builder_binding: None,
                    model_binding: model.map(str::to_owned),
                    max_preauthorization: Some(amount("100.000000")),
                    unit,
                    unit_quantity: 1,
                    gpu_class: None,
                    gpu_mode: None,
                    unit_price: Money {
                        currency: "CREDITS".into(),
                        amount: amount(price),
                    },
                    effective_from: UtcTimestamp::from_utc(now.get() - Duration::minutes(1))?,
                    effective_until: None,
                },
            )
            .await?;
    }
    Ok(())
}

async fn grant(
    store: &PgCreditStore,
    project: ProjectId,
    value: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    store
        .admin(
            &Uuid::now_v7().to_string(),
            contracts::ActorId::new(),
            &[PlatformRole::PlatformAdmin],
            &CreditAdminOperation::Grant {
                project_id: project,
                amount: amount(value),
                reason: "model fixture".into(),
            },
        )
        .await?;
    Ok(())
}

fn admission(project: ProjectId, call_id: Uuid) -> ModelCostAdmissionRequest {
    ModelCostAdmissionRequest {
        call_id,
        project_id: project,
        model_binding: "claude-test".into(),
    }
}

#[tokio::test]
async fn model_pg_normal_settlement_charges_all_four_dimensions()
-> Result<(), Box<dyn std::error::Error>> {
    let (_container, _pool, store, credits) = fixture().await?;
    configure_rates(&store, Some("claude-test")).await?;
    let project = ProjectId::new();
    grant(&credits, project, "100.000000").await?;
    let call = Uuid::now_v7();
    let accepted = store
        .admit_model_cost("admit-normal", "agent", &admission(project, call))
        .await?;
    let settled = store
        .settle_model_cost(
            "settle-normal",
            "agent",
            accepted.hold.id,
            call,
            EventId::new(),
            "claude-test",
            &ModelUsageMeasurement::Known {
                quantities: ModelTokenQuantities {
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_read_tokens: 1,
                    cache_write_tokens: 1,
                },
            },
        )
        .await?;
    assert_eq!(
        settled.state,
        contracts::resource::credits::CreditHoldState::Settled
    );
    assert_eq!(settled.remaining, amount("0.000000"));
    Ok(())
}

#[tokio::test]
async fn model_pg_insufficient_balance_rejects_new_call_before_hold()
-> Result<(), Box<dyn std::error::Error>> {
    let (_container, _pool, store, credits) = fixture().await?;
    configure_rates(&store, Some("claude-test")).await?;
    let project = ProjectId::new();
    grant(&credits, project, "1.000000").await?;
    let error = store
        .admit_model_cost("admit-short", "agent", &admission(project, Uuid::now_v7()))
        .await
        .expect_err("insufficient balance");
    assert!(matches!(
        error,
        ResourceStoreError::Credits(resource_service::credits::CreditError::Insufficient)
    ));
    Ok(())
}

#[tokio::test]
async fn model_pg_duplicate_settlement_event_is_idempotent()
-> Result<(), Box<dyn std::error::Error>> {
    let (_container, _pool, store, credits) = fixture().await?;
    configure_rates(&store, Some("claude-test")).await?;
    let project = ProjectId::new();
    grant(&credits, project, "100.000000").await?;
    let call = Uuid::now_v7();
    let accepted = store
        .admit_model_cost("admit-replay", "agent", &admission(project, call))
        .await?;
    let event = EventId::new();
    let known = ModelUsageMeasurement::Known {
        quantities: ModelTokenQuantities {
            input_tokens: 2,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        },
    };
    let first = store
        .settle_model_cost(
            "settle-replay",
            "agent",
            accepted.hold.id,
            call,
            event,
            "claude-test",
            &known,
        )
        .await?;
    let replay = store
        .settle_model_cost(
            "settle-replay",
            "agent",
            accepted.hold.id,
            call,
            event,
            "claude-test",
            &known,
        )
        .await?;
    assert_eq!(first, replay);
    Ok(())
}

#[tokio::test]
async fn model_pg_cache_read_and_write_are_separate_lines() -> Result<(), Box<dyn std::error::Error>>
{
    let (_container, _pool, store, credits) = fixture().await?;
    configure_rates(&store, Some("claude-test")).await?;
    let project = ProjectId::new();
    grant(&credits, project, "100.000000").await?;
    let estimate = store
        .admit_model_cost("admit-cache", "agent", &admission(project, Uuid::now_v7()))
        .await?;
    assert_eq!(
        estimate
            .estimate
            .lines
            .iter()
            .filter(|line| matches!(
                line.unit,
                ResourceBillingUnit::LlmCacheReadToken | ResourceBillingUnit::LlmCacheWriteToken
            ))
            .count(),
        2
    );
    let call = estimate.hold.model_call_id.expect("call identity");
    let settled = store
        .settle_model_cost(
            "settle-cache",
            "agent",
            estimate.hold.id,
            call,
            EventId::new(),
            "claude-test",
            &ModelUsageMeasurement::Known {
                quantities: ModelTokenQuantities {
                    input_tokens: 0,
                    output_tokens: 0,
                    cache_read_tokens: 2,
                    cache_write_tokens: 3,
                },
            },
        )
        .await?;
    assert_eq!(
        settled.state,
        contracts::resource::credits::CreditHoldState::Settled
    );
    Ok(())
}

#[tokio::test]
async fn model_pg_missing_any_rate_rejects_admission() -> Result<(), Box<dyn std::error::Error>> {
    let (_container, _pool, store, credits) = fixture().await?;
    configure_rates(&store, Some("other-model")).await?;
    let project = ProjectId::new();
    grant(&credits, project, "100.000000").await?;
    assert!(matches!(
        store
            .admit_model_cost(
                "admit-no-rate",
                "agent",
                &admission(project, Uuid::now_v7())
            )
            .await,
        Err(ResourceStoreError::RateUnconfigured)
    ));
    Ok(())
}

#[tokio::test]
async fn model_pg_unknown_settlement_keeps_hold_reconcilable()
-> Result<(), Box<dyn std::error::Error>> {
    let (_container, _pool, store, credits) = fixture().await?;
    configure_rates(&store, Some("claude-test")).await?;
    let project = ProjectId::new();
    grant(&credits, project, "100.000000").await?;
    let call = Uuid::now_v7();
    let accepted = store
        .admit_model_cost("admit-unknown", "agent", &admission(project, call))
        .await?;
    let unknown = store
        .settle_model_cost(
            "settle-unknown",
            "agent",
            accepted.hold.id,
            call,
            EventId::new(),
            "claude-test",
            &ModelUsageMeasurement::Unknown {
                reason: "stream interrupted".into(),
            },
        )
        .await?;
    assert_eq!(
        unknown.state,
        contracts::resource::credits::CreditHoldState::Unknown
    );
    assert!(unknown.measurement_unknown);
    Ok(())
}

#[tokio::test]
async fn model_pg_all_null_hold_target_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
    let (_container, pool, _store, credits) = fixture().await?;
    let project = ProjectId::new();
    grant(&credits, project, "10.000000").await?;
    let error = sqlx::query("INSERT INTO resource.credit_holds (hold_id, project_id, request_id, build_request_id, build_attempt, model_call_id, business_key, owner_client_id, amount, remaining, state) VALUES ($1,$2,NULL,NULL,NULL,NULL,'invalid-target','agent',10000000,10000000,'pending')")
        .bind(Uuid::now_v7()).bind(project.as_uuid()).execute(&pool).await.expect_err("all-null target must violate check");
    assert_eq!(
        error
            .as_database_error()
            .and_then(|error| error.constraint()),
        Some("credit_holds_target_check")
    );
    Ok(())
}
