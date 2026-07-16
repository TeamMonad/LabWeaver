//! Real `PostgreSQL` and `JetStream` proof for publication and withdrawal Outbox delivery.

use std::time::Duration;

use contracts::events::{
    CloudEvent, DATA_SCHEMA_BASE, ReleasePublished, ReleaseWithdrawn, SPEC_VERSION, subjects,
};
use contracts::{
    ActorId, CourseId, EventId, ReleaseId, Revision, Sequence, Sha256Digest, UtcTimestamp,
};
use control_service::messaging::ControlOutboxDispatcher;
use sqlx::postgres::PgPoolOptions;
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::{GenericImage, ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;

#[tokio::test]
async fn release_and_withdrawal_are_marked_published_only_after_jetstream_ack()
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
        "CREATE SCHEMA control; SET search_path TO control;\n{}\n{}",
        include_str!("../../../migrations/control/0001_initial.sql"),
        include_str!("../../../migrations/control/0002_control_plane.sql")
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
        ControlOutboxDispatcher::new(pool.clone(), client.clone(), Duration::from_secs(5))?;
    let course_id = CourseId::new();
    let release_id = ReleaseId::new();
    let published_id = EventId::new();
    let withdrawn_id = EventId::new();
    let now = "2026-07-16T08:00:00.000Z".parse::<UtcTimestamp>()?;
    let digest = Sha256Digest::of_bytes(b"release-artifact");
    let published = CloudEvent {
        specversion: SPEC_VERSION.to_owned(),
        id: published_id,
        source: "urn:labweaver:control-service".to_owned(),
        event_type: subjects::ENVIRONMENT_TEMPLATE_RELEASE_PUBLISHED.to_owned(),
        subject: subjects::ENVIRONMENT_TEMPLATE_RELEASE_PUBLISHED.to_owned(),
        time: now,
        datacontenttype: "application/json".to_owned(),
        dataschema: format!(
            "{DATA_SCHEMA_BASE}/environment-template-release-published.schema.json"
        ),
        course_id,
        aggregate_revision: Revision::new(1)?,
        aggregate_sequence: Sequence(1),
        trace_id: "issue-48-control-outbox".to_owned(),
        data: ReleasePublished {
            release_id,
            version: 1,
            environment_spec_sha256: digest,
            artifact_sha256: digest,
        },
    };
    let withdrawn = CloudEvent {
        specversion: SPEC_VERSION.to_owned(),
        id: withdrawn_id,
        source: "urn:labweaver:control-service".to_owned(),
        event_type: subjects::ENVIRONMENT_TEMPLATE_RELEASE_WITHDRAWN.to_owned(),
        subject: subjects::ENVIRONMENT_TEMPLATE_RELEASE_WITHDRAWN.to_owned(),
        time: now,
        datacontenttype: "application/json".to_owned(),
        dataschema: format!(
            "{DATA_SCHEMA_BASE}/environment-template-release-withdrawn.schema.json"
        ),
        course_id,
        aggregate_revision: Revision::new(1)?,
        aggregate_sequence: Sequence(2),
        trace_id: "issue-48-control-outbox".to_owned(),
        data: ReleaseWithdrawn {
            release_id,
            version: 1,
            actor_id: ActorId::new(),
            reason_code: "SECURITY_REVOKED".to_owned(),
            withdrawn_at: now,
        },
    };
    insert_event(&pool, release_id, 1, &published).await?;
    insert_event(&pool, release_id, 2, &withdrawn).await?;

    assert!(dispatcher.dispatch_once().await.is_err());
    assert!(!published_at(&pool, published_id).await?);

    let context = async_nats::jetstream::new(client);
    context
        .create_stream(async_nats::jetstream::stream::Config {
            name: "CONTROL_RELEASES".to_owned(),
            subjects: vec!["labweaver.control.environment_template_release.>".to_owned()],
            ..Default::default()
        })
        .await?;
    assert!(dispatcher.dispatch_once().await?);
    assert!(dispatcher.dispatch_once().await?);
    assert!(published_at(&pool, published_id).await?);
    assert!(published_at(&pool, withdrawn_id).await?);
    assert!(!dispatcher.dispatch_once().await?);
    let mut stream = context.get_stream("CONTROL_RELEASES").await?;
    assert_eq!(stream.info().await?.state.messages, 2);
    Ok(())
}

async fn insert_event<T: serde::Serialize>(
    pool: &sqlx::PgPool,
    release_id: ReleaseId,
    sequence: i64,
    event: &CloudEvent<T>,
) -> Result<(), Box<dyn std::error::Error>> {
    let payload = serde_json::to_value(event)?;
    let hash = Sha256Digest::of_canonical(&payload)?;
    sqlx::query(
        "INSERT INTO control.outbox_events \
         (event_id,subject,event_type,aggregate_id,aggregate_sequence,payload,payload_sha256) \
         VALUES ($1,$2,$2,$3,$4,$5,$6)",
    )
    .bind(event.id.as_uuid())
    .bind(&event.subject)
    .bind(release_id.as_uuid())
    .bind(sequence)
    .bind(payload)
    .bind(hash.to_string())
    .execute(pool)
    .await?;
    Ok(())
}

async fn published_at(pool: &sqlx::PgPool, event_id: EventId) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT published_at IS NOT NULL FROM control.outbox_events WHERE event_id=$1",
    )
    .bind(event_id.as_uuid())
    .fetch_one(pool)
    .await
}
