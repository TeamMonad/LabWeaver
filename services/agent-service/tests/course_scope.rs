//! Real `PostgreSQL` proof that Agent mutations bind the authoritative course before state changes.

use agent_service::run_store::{AgentRunStoreError, PostgresAgentRunStore};
use contracts::authoring::{AgentRun, AgentRunState, AgentTrack, AgentTrackKind, RuntimeKind};
use contracts::http::IdempotencyKey;
use contracts::{
    AgentRunId, CourseId, PolicyId, ProblemPackageId, Revision, Sha256Digest, UtcTimestamp,
};
use sqlx::postgres::PgPoolOptions;
use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;

#[tokio::test]
async fn revisioned_mutation_rejects_cross_course_before_writing_state()
-> Result<(), Box<dyn std::error::Error>> {
    let postgres = Postgres::default().with_tag("17.5-alpine").start().await?;
    let database_url = format!(
        "postgres://postgres:postgres@127.0.0.1:{}/postgres",
        postgres.get_host_port_ipv4(5432).await?
    );
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&database_url)
        .await?;
    let migrations = format!(
        "CREATE SCHEMA agent; SET search_path TO agent;\n{}",
        include_str!("../../../migrations/agent/0001_sprint2_baseline.sql")
    );
    sqlx::raw_sql(&migrations).execute(&pool).await?;

    let run = requested_run()?;
    let contract = serde_json::to_value(&run)?;
    sqlx::query(
        "INSERT INTO agent.agent_runs \
         (run_id,course_id,problem_package_id,revision,state,provider_binding,input_sha256, \
          policy_revision,contract) VALUES ($1,$2,$3,$4,'requested',$5,$6,$7,$8)",
    )
    .bind(run.id.as_uuid())
    .bind(run.course_id.as_uuid())
    .bind(run.package_id.as_uuid())
    .bind(i64::try_from(run.revision.get())?)
    .bind("claude-code-v1")
    .bind(Sha256Digest::of_bytes(b"input").to_string())
    .bind(i64::try_from(run.policy_revision.get())?)
    .bind(contract)
    .execute(&pool)
    .await?;
    let store = PostgresAgentRunStore::new(pool.clone());
    let now = "2026-07-16T08:00:00.000Z".parse::<UtcTimestamp>()?;
    let result = store
        .request_cancellation_revisioned(
            CourseId::new(),
            run.id,
            run.revision,
            &IdempotencyKey::parse("cross-course-cancel")?,
            now,
        )
        .await;
    assert_eq!(result, Err(AgentRunStoreError::CourseMismatch));
    assert!(!cancellation_requested(&pool, run.id).await?);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM agent.idempotency_ledger")
            .fetch_one(&pool)
            .await?,
        0
    );

    store
        .request_cancellation_revisioned(
            run.course_id,
            run.id,
            run.revision,
            &IdempotencyKey::parse("same-course-cancel")?,
            now,
        )
        .await?;
    assert!(cancellation_requested(&pool, run.id).await?);
    Ok(())
}

fn requested_run() -> Result<AgentRun, Box<dyn std::error::Error>> {
    let run = AgentRun {
        id: AgentRunId::new(),
        course_id: CourseId::new(),
        package_id: ProblemPackageId::new(),
        policy_id: PolicyId::new(),
        policy_revision: Revision::new(1)?,
        requested_runtime: RuntimeKind::Container,
        state: AgentRunState::Requested,
        revision: Revision::new(1)?,
        tracks: vec![
            AgentTrack {
                kind: AgentTrackKind::Environment,
                attempts: Vec::new(),
                candidate_id: None,
            },
            AgentTrack {
                kind: AgentTrackKind::Evaluation,
                attempts: Vec::new(),
                candidate_id: None,
            },
        ],
    };
    run.validate()?;
    Ok(run)
}

async fn cancellation_requested(
    pool: &sqlx::PgPool,
    run_id: AgentRunId,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT cancellation_requested_at IS NOT NULL FROM agent.agent_runs WHERE run_id=$1",
    )
    .bind(run_id.as_uuid())
    .fetch_one(pool)
    .await
}
