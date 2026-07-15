//! Real `JetStream` command, Outbox acknowledgement, and Provider RPC integration coverage.
#![allow(
    clippy::too_many_lines,
    reason = "one integration test retains a single PostgreSQL and NATS build identity"
)]

mod support;

use std::time::Duration;

use contracts::environment::{EnvironmentLifecycleCommandData, EnvironmentOperationKind};
use contracts::events::{DATA_SCHEMA_BASE, SPEC_VERSION, subjects};
use contracts::{ActorId, EventId, Sequence};
use environment_service::{
    EnvironmentProvider, JetStreamCommandConsumer, JetStreamEventPublisher, LifecycleCommand,
    LifecycleCommandMessage, NatsAccessRevoker, NatsEnvironmentProvider, OutboxDispatchOutcome,
    OutboxDispatcher, PgEnvironmentStore, ReconcileAction,
};
use futures_util::StreamExt;
use sqlx::postgres::PgPoolOptions;
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::{GenericImage, ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;

use support::{requested_instance, revision, timestamp};

#[tokio::test]
async fn jetstream_command_outbox_and_provider_rpc_use_durable_identities()
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
        "CREATE SCHEMA environment; SET search_path TO environment;\n{}\n{}",
        include_str!("../../../migrations/environment/0001_initial.sql"),
        include_str!("../../../migrations/environment/0002_reconcile_leases.sql")
    );
    sqlx::raw_sql(&migrations).execute(&pool).await?;

    let nats = GenericImage::new("nats", "2.11.8-alpine")
        .with_exposed_port(4222.tcp())
        .with_wait_for(WaitFor::message_on_stderr("Server is ready"))
        .with_cmd(["-js"])
        .start()
        .await?;
    let nats_url = format!("nats://127.0.0.1:{}", nats.get_host_port_ipv4(4222).await?);
    let client = async_nats::connect(nats_url).await?;
    let context = async_nats::jetstream::new(client.clone());
    context
        .create_stream(async_nats::jetstream::stream::Config {
            name: "ENV_COMMANDS".to_owned(),
            subjects: vec![subjects::ENVIRONMENT_LIFECYCLE_REQUESTED.to_owned()],
            ..Default::default()
        })
        .await?
        .create_consumer(async_nats::jetstream::consumer::pull::Config {
            durable_name: Some("environment-lifecycle-v1".to_owned()),
            filter_subject: subjects::ENVIRONMENT_LIFECYCLE_REQUESTED.to_owned(),
            ack_policy: async_nats::jetstream::consumer::AckPolicy::Explicit,
            ..Default::default()
        })
        .await?;
    let mut quarantine_stream = context
        .create_stream(async_nats::jetstream::stream::Config {
            name: "ENV_QUARANTINE".to_owned(),
            subjects: vec!["private.environment.command.quarantine.v1".to_owned()],
            ..Default::default()
        })
        .await?;
    let mut event_stream = context
        .create_stream(async_nats::jetstream::stream::Config {
            name: "EVENTS".to_owned(),
            subjects: vec![subjects::ENVIRONMENT_OPERATION_ACCEPTED.to_owned()],
            ..Default::default()
        })
        .await?;

    let store = PgEnvironmentStore::new(pool.clone());
    let instance = requested_instance();
    store.create("create-key-jetstream", &instance).await?;
    let outbox = OutboxDispatcher::new(
        pool,
        JetStreamEventPublisher::new(client.clone()),
        Duration::from_secs(2),
    )?;
    assert!(matches!(
        outbox.dispatch_once().await?,
        OutboxDispatchOutcome::Published { .. }
    ));
    assert_eq!(event_stream.info().await?.state.messages, 1);

    let command = LifecycleCommandMessage {
        specversion: SPEC_VERSION.to_owned(),
        id: EventId::new(),
        source: "urn:labweaver:environment-service".to_owned(),
        event_type: subjects::ENVIRONMENT_LIFECYCLE_REQUESTED.to_owned(),
        subject: subjects::ENVIRONMENT_LIFECYCLE_REQUESTED.to_owned(),
        time: timestamp("2026-07-14T00:01:00.000Z"),
        datacontenttype: "application/json".to_owned(),
        dataschema: format!("{DATA_SCHEMA_BASE}/environment-lifecycle-requested.schema.json"),
        course_id: instance.course_id,
        aggregate_revision: instance.revision,
        aggregate_sequence: Sequence(1),
        trace_id: "trace-delete-jetstream".to_owned(),
        data: EnvironmentLifecycleCommandData {
            idempotency_key: "delete-key-jetstream".to_owned(),
            command: LifecycleCommand {
                environment_id: instance.id,
                kind: EnvironmentOperationKind::Delete,
                expected_revision: instance.revision,
                actor_id: ActorId::new(),
                trace_id: "trace-delete-jetstream".to_owned(),
                accepted_at: timestamp("2026-07-14T00:01:00.000Z"),
                deadline_at: timestamp("2026-07-14T00:06:00.000Z"),
                access_revocation_revision: Some(revision(7)),
                preserve_mutable_disk: false,
                max_attempts: 3,
            },
        },
    };
    context
        .publish(
            subjects::ENVIRONMENT_LIFECYCLE_REQUESTED,
            serde_json::to_vec(&command)?.into(),
        )
        .await?
        .await?;
    let mut consumer = JetStreamCommandConsumer::bind(
        client.clone(),
        "ENV_COMMANDS",
        "environment-lifecycle-v1",
        "private.environment.command.quarantine.v1",
    )
    .await?;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), consumer.process_next(&store))
            .await
            .map_err(|_| "command consumer timed out")??,
        environment_service::CommandConsumeOutcome::Applied
    );
    assert_eq!(store.load(instance.id).await?.revision, revision(2));
    context
        .publish(
            subjects::ENVIRONMENT_LIFECYCLE_REQUESTED,
            b"{}".as_slice().into(),
        )
        .await?
        .await?;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), consumer.process_next(&store))
            .await
            .map_err(|_| "invalid command consumer timed out")??,
        environment_service::CommandConsumeOutcome::Rejected
    );
    assert_eq!(quarantine_stream.info().await?.state.messages, 1);

    let provider_subject = "labweaver.provider.container-primary-v1.command.v1";
    let mut requests = client.subscribe(provider_subject).await?;
    let responder = client.clone();
    let response_task = tokio::spawn(async move {
        let message = requests.next().await.ok_or("provider request missing")?;
        let request: serde_json::Value = serde_json::from_slice(&message.payload)?;
        let reply = message.reply.ok_or("provider reply subject missing")?;
        let response = serde_json::json!({
            "status": "succeeded",
            "version": 1,
            "operationId": request["operationId"],
            "observation": {
                "nextState": "validating",
                "endpoints": [],
                "cleanupEvidence": null,
                "operationComplete": false
            }
        });
        responder
            .publish(reply, serde_json::to_vec(&response)?.into())
            .await?;
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
    });
    let access_client = client.clone();
    let provider = NatsEnvironmentProvider::new(
        "container-primary-v1".to_owned(),
        provider_subject.to_owned(),
        client,
    )?;
    let provider_instance = requested_instance();
    let observation = tokio::time::timeout(
        Duration::from_secs(5),
        provider.execute(ReconcileAction::Validate, &provider_instance),
    )
    .await
    .map_err(|_| "provider RPC timed out")?
    .map_err(|_| "provider RPC failed")?;
    assert_eq!(
        observation.next_state,
        contracts::environment::ObservedEnvironmentState::Validating
    );
    response_task
        .await?
        .map_err(|_| "provider response task failed")?;

    let access_subject = "labweaver.access.environment.revoke.v1";
    let mut access_requests = access_client.subscribe(access_subject).await?;
    let access_responder = access_client.clone();
    let access_task = tokio::spawn(async move {
        let message = access_requests
            .next()
            .await
            .ok_or("access revocation request missing")?;
        let request: serde_json::Value = serde_json::from_slice(&message.payload)?;
        let reply = message.reply.ok_or("access reply subject missing")?;
        let response = serde_json::json!({
            "version": 1,
            "environmentId": request["environmentId"],
            "environmentRevision": request["environmentRevision"],
            "accessRevocationRevision": revision(9)
        });
        access_responder
            .publish(reply, serde_json::to_vec(&response)?.into())
            .await?;
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
    });
    let revoker = NatsAccessRevoker::new(
        access_subject.to_owned(),
        access_client,
        Duration::from_secs(2),
    )?;
    assert_eq!(
        revoker.revoke_for_expiry(&provider_instance).await?,
        revision(9)
    );
    access_task
        .await?
        .map_err(|_| "access response task failed")?;
    Ok(())
}
