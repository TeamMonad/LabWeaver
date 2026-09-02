//! Real `PostgreSQL` and `JetStream` proof that publication acknowledgement fences Outbox state.

use std::time::Duration;

use agent_service::messaging::AgentOutboxDispatcher;
use contracts::events::{AgentRunEvent, CloudEvent, DATA_SCHEMA_BASE, SPEC_VERSION, subjects};
use contracts::{AgentRunId, CourseId, EventId, Revision, Sequence, UtcTimestamp};
use persistence_sqlx::Sha256Digest;
use sqlx::postgres::PgPoolOptions;
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::{GenericImage, ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;

#[tokio::test]
async fn outbox_is_marked_published_only_after_jetstream_ack()
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
        include_str!("../../../migrations/agent/0001_platform_baseline.sql")
    );
    sqlx::raw_sql(&migrations).execute(&pool).await?;

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
        AgentOutboxDispatcher::new(pool.clone(), client.clone(), Duration::from_secs(5))?;
    let event_id = EventId::new();
    let run_id = AgentRunId::new();
    let event = CloudEvent {
        specversion: SPEC_VERSION.to_owned(),
        id: event_id,
        source: "urn:labweaver:agent-service".to_owned(),
        event_type: subjects::AGENT_RUN_REQUESTED.to_owned(),
        subject: subjects::AGENT_RUN_REQUESTED.to_owned(),
        time: "2026-07-15T08:00:00.000Z".parse::<UtcTimestamp>()?,
        datacontenttype: "application/json".to_owned(),
        dataschema: format!("{DATA_SCHEMA_BASE}/agent-run-requested.schema.json"),
        course_id: CourseId::new(),
        aggregate_revision: Revision::new(1)?,
        aggregate_sequence: Sequence(1),
        trace_id: "issue-48-outbox-test".to_owned(),
        data: AgentRunEvent {
            run_id,
            attempt: 0,
            state: "requested".to_owned(),
            diagnostic_code: None,
        },
    };
    let payload = serde_json::to_value(&event)?;
    let hash = Sha256Digest::of_canonical(&payload)?;
    sqlx::query(
        "INSERT INTO agent.outbox_events \
         (event_id,subject,event_type,aggregate_id,aggregate_sequence,payload,payload_sha256) \
         VALUES ($1,$2,$2,$3,1,$4,$5)",
    )
    .bind(event_id.as_uuid())
    .bind(subjects::AGENT_RUN_REQUESTED)
    .bind(run_id.as_uuid())
    .bind(payload)
    .bind(hash.to_string())
    .execute(&pool)
    .await?;

    assert!(dispatcher.dispatch_once().await.is_err());
    assert!(!published(&pool, event_id).await?);

    let context = async_nats::jetstream::new(client);
    context
        .create_stream(async_nats::jetstream::stream::Config {
            name: "AGENT_RUNS".to_owned(),
            subjects: vec!["labweaver.agent.run.>".to_owned()],
            ..Default::default()
        })
        .await?;
    assert!(dispatcher.dispatch_once().await?);
    assert!(published(&pool, event_id).await?);
    assert!(!dispatcher.dispatch_once().await?);
    let mut stream = context.get_stream("AGENT_RUNS").await?;
    assert_eq!(stream.info().await?.state.messages, 1);
    Ok(())
}

async fn published(pool: &sqlx::PgPool, event_id: EventId) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT published_at IS NOT NULL FROM agent.outbox_events WHERE event_id=$1")
        .bind(event_id.as_uuid())
        .fetch_one(pool)
        .await
}
