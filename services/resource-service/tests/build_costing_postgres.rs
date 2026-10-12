//! Real isolated accounts and build identities verify the builder-time money boundary.
use contracts::events::AgentBuildRequested;
use contracts::http::CreateResourceRateRequest;
use contracts::resource::build_costing::{
    BuildCostAdmissionRequest, BuildUsageMeasurement, ObserveBuildCostRequest,
};
use contracts::resource::credits::CreditAdminOperation;
use contracts::resource::{FixedDecimal, Money, ResourceBillingUnit};
use contracts::supply_chain::{BuildNetworkPolicy, BuildRequest, BuildSource};
use contracts::{
    ActorId, ArtifactId, BuildRequestId, CandidateId, PlatformRole, ProjectId, Revision,
    UtcTimestamp,
};
use resource_service::{credits::PgCreditStore, store::PgResourceStore};
use sqlx::{PgPool, postgres::PgPoolOptions};
use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;
use uuid::Uuid;
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
fn amount(v: &str) -> Result<FixedDecimal> {
    Ok(FixedDecimal::parse(v)?)
}
fn at(t: UtcTimestamp, ms: i64) -> Result<UtcTimestamp> {
    Ok(UtcTimestamp::from_utc(
        t.get() + time::Duration::milliseconds(ms),
    )?)
}
async fn fixture() -> Result<(
    testcontainers::ContainerAsync<Postgres>,
    PgPool,
    PgResourceStore,
    PgCreditStore,
    BuildCostAdmissionRequest,
)> {
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(12)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            container.get_host_port_ipv4(5432).await?
        ))
        .await?;
    for domain in ["resource", "agent"] {
        let mut files = std::fs::read_dir(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("../../migrations/{domain}")),
        )?
        .map(|f| f.map(|f| f.path()))
        .collect::<std::result::Result<Vec<_>, _>>()?;
        files.sort();
        let mut ddl = format!("CREATE SCHEMA {domain};SET search_path TO {domain};\n");
        for f in files {
            if f.extension().is_some_and(|v| v == "sql") {
                ddl.push_str(&std::fs::read_to_string(f)?);
                ddl.push('\n');
            }
        }
        sqlx::raw_sql(&ddl).execute(&pool).await?;
    }
    let store = PgResourceStore::new(pool.clone());
    let credits = PgCreditStore::new(pool.clone());
    let now = store.current_time().await?;
    let request = BuildRequest {
        id: BuildRequestId::new(),
        project_id: ProjectId::new(),
        course_id: None,
        candidate_id: CandidateId::new(),
        candidate_revision: Revision::new(1)?,
        builder_binding: "synthetic-builder".into(),
        source: BuildSource::Dockerfile {
            context: contracts::ArtifactRef {
                artifact_id: ArtifactId::new(),
                store_binding: "synthetic-object-store".into(),
                object_version: "version-1".into(),
                size_bytes: 1,
                media_type: "application/vnd.oci.image.layer.v1.tar+gzip".into(),
            },
            context_object_key: "synthetic/context.tar.gz".into(),
            dockerfile_path: "Dockerfile".into(),
        },
        output_repository: "harbor.internal/synthetic/course-build".into(),
        network: BuildNetworkPolicy::DenyAll,
        max_duration_milliseconds: 60_000,
        max_cpu_millicores: 2000,
        max_memory_bytes: 2_147_483_648,
        created_at: now,
    };
    let token = Uuid::new_v4();
    let command = AgentBuildRequested {
        request: request.clone(),
        idempotency_key: request.id.to_string(),
    };
    sqlx::query("INSERT INTO agent.build_commands(build_request_id,project_id,course_id,command_sha256,idempotency_key,state,command,attempt,worker_id,lease_token,lease_expires_at) VALUES($1,$2,NULL,$3,$4,'running',$5,1,'synthetic-worker',$6,clock_timestamp()+interval '5 minutes')")
 .bind(request.id.as_uuid()).bind(request.project_id.as_uuid()).bind("a".repeat(64)).bind(request.id.to_string()).bind(serde_json::to_value(command)?).bind(token).execute(&pool).await?;
    let input = BuildCostAdmissionRequest {
        build_request_id: request.id,
        attempt: 1,
        lease_token: token,
        execution_deadline: at(now, 50_000)?,
        stage_request_id: "b".repeat(64),
        authorize_execution: true,
    };
    Ok((container, pool, store, credits, input))
}
async fn rate(store: &PgResourceStore, from: UtcTimestamp, value: &str) -> Result<()> {
    store
        .create_rate(
            &Uuid::now_v7().to_string(),
            &CreateResourceRateRequest {
                unit: ResourceBillingUnit::BuildSecond,
                unit_quantity: 1,
                gpu_class: None,
                gpu_mode: None,
                builder_binding: Some("synthetic-builder".into()),
                unit_price: Money {
                    currency: "CREDITS".into(),
                    amount: amount(value)?,
                },
                effective_from: from,
                effective_until: None,
            },
        )
        .await?;
    Ok(())
}
async fn grant(credits: &PgCreditStore, project: ProjectId, value: &str) -> Result<()> {
    credits
        .admin(
            &Uuid::now_v7().to_string(),
            ActorId::new(),
            &[PlatformRole::PlatformAdmin],
            &CreditAdminOperation::Grant {
                project_id: project,
                amount: amount(value)?,
                reason: "isolated synthetic build quota".into(),
            },
        )
        .await?;
    Ok(())
}
async fn project(pool: &PgPool, build: BuildRequestId) -> Result<ProjectId> {
    let id: Uuid =
        sqlx::query_scalar("SELECT project_id FROM agent.build_commands WHERE build_request_id=$1")
            .bind(build.as_uuid())
            .fetch_one(pool)
            .await?;
    Ok(id.to_string().parse()?)
}
#[tokio::test]
async fn missing_rate_and_insufficient_balance_never_authorize_or_create_partial_holds()
-> Result<()> {
    let (_c, pool, store, credits, input) = fixture().await?;
    let p = project(&pool, input.build_request_id).await?;
    assert!(
        store
            .admit_build_cost("synthetic-agent", &input)
            .await
            .is_err()
    );
    rate(
        &store,
        at(store.current_time().await?, -60_000)?,
        "1.000000",
    )
    .await?;
    assert!(
        store
            .admit_build_cost("synthetic-agent", &input)
            .await
            .is_err()
    );
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM resource.credit_holds")
        .fetch_one(&pool)
        .await?;
    assert_eq!(n, 0);
    assert_eq!(credits.account(p).await?.reserved, amount("0.000000")?);
    grant(&credits, p, "100.000000").await?;
    let admitted = store.admit_build_cost("synthetic-agent", &input).await?;
    assert!(admitted.execution_authorized);
    assert!(admitted.hold.request_id.is_none());
    assert_eq!(admitted.hold.build_request_id, Some(input.build_request_id));
    assert_eq!(admitted.estimate.total.amount, amount("60.000000")?);
    assert_eq!(
        store
            .admit_build_cost("synthetic-agent", &input)
            .await?
            .hold
            .id,
        admitted.hold.id
    );
    let requests: i64 = sqlx::query_scalar("SELECT count(*) FROM resource.resource_requests")
        .fetch_one(&pool)
        .await?;
    assert_eq!(requests, 0);
    Ok(())
}
#[tokio::test]
async fn millisecond_history_settles_once_and_late_known_cost_is_platform_cost() -> Result<()> {
    let (_c, pool, store, credits, input) = fixture().await?;
    let p = project(&pool, input.build_request_id).await?;
    let now = store.current_time().await?;
    rate(&store, at(now, -60_000)?, "1.000000").await?;
    grant(&credits, p, "100.000000").await?;
    let hold = store.admit_build_cost("synthetic-agent", &input).await?;
    let unknown = ObserveBuildCostRequest {
        build_request_id: input.build_request_id,
        attempt: 1,
        lease_token: input.lease_token,
        measurement: BuildUsageMeasurement::Unknown {
            reason: "no authoritative terminal history".into(),
        },
    };
    let first = store
        .observe_build_cost("synthetic-agent", &unknown)
        .await?;
    assert_eq!(
        store
            .observe_build_cost("synthetic-agent", &unknown)
            .await?
            .hold
            .unknown_since,
        first.hold.unknown_since
    );
    sqlx::query("UPDATE resource.credit_holds SET unknown_since=date_trunc('milliseconds',clock_timestamp())-interval '1 hour 1 second',reconciliation_due_at=date_trunc('milliseconds',clock_timestamp())-interval '1 second' WHERE hold_id=$1").bind(hold.hold.id).execute(&pool).await?;
    let known = ObserveBuildCostRequest {
        measurement: BuildUsageMeasurement::Known {
            history_ref: "synthetic-build-history-ref".into(),
            started_at: now,
            finished_at: at(now, 400)?,
        },
        ..unknown
    };
    let settled = store.observe_build_cost("synthetic-agent", &known).await?;
    assert_eq!(
        settled.charged_total.as_ref().map(|v| v.amount.clone()),
        Some(amount("0.400000")?)
    );
    assert!(settled.first_unknown_observation.is_some());
    assert_eq!(
        store.observe_build_cost("synthetic-agent", &known).await?,
        settled
    );
    let account = credits.account(p).await?;
    assert_eq!(account.reserved, amount("0.000000")?);
    assert_eq!(account.spent, amount("0.000000")?);
    assert_eq!(account.platform_absorbed, amount("0.400000")?);
    Ok(())
}
