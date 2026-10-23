//! Real PostgreSQL/JetStream failure boundaries for the Resource Outbox.

#![allow(
    clippy::expect_used,
    reason = "fixture assertions identify the failed protocol boundary"
)]

use std::time::Duration;

use contracts::events::{CloudEvent, DATA_SCHEMA_BASE, SPEC_VERSION, subjects};
use contracts::resource::{
    ResourceRequest, ResourceRequestState, ResourceTarget, WorkloadResources,
};
use contracts::{
    ActorId, EnvironmentId, EventId, ProjectId, ReleaseId, ResourceRequestId, Revision, Sequence,
    UtcTimestamp,
};
use futures_util::StreamExt;
use persistence_sqlx::{Domain, InboxDecision, InboxStore, OutboxStore, Sha256Digest};
use resource_service::outbox::{
    ResourceOutboxDispatcher, ResourceOutboxError, ResourceOutboxOutcome,
};
use serde_json::Value;
use sqlx::{PgPool, postgres::PgPoolOptions};
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::{ContainerAsync, GenericImage, ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    sync::{mpsc, oneshot},
    task::JoinSet,
    time::timeout,
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;
const DEADLINE: Duration = Duration::from_secs(10);

struct Fixture {
    pool: PgPool,
    observer: PgPool,
    url: String,
    client: async_nats::Client,
    stream: async_nats::jetstream::stream::Stream,
    publications: async_nats::Subscriber,
    _postgres: ContainerAsync<Postgres>,
    nats: ContainerAsync<GenericImage>,
}

impl Fixture {
    async fn new(no_ack: bool) -> TestResult<Self> {
        let postgres = Postgres::default().with_tag("17.5-alpine").start().await?;
        let url = format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            postgres.get_host_port_ipv4(5432).await?
        );
        let pool = single_connection_pool(&url).await?;
        // Use the real immutable Outbox/Inbox schema, without unrelated lifecycle fixtures.
        sqlx::raw_sql(&format!(
            "CREATE SCHEMA resource; SET search_path TO resource;\n{}\n{}",
            include_str!("../../../migrations/resource/0001_platform_baseline.sql"),
            include_str!("../../../migrations/resource/0021_outbox_publication_claims.sql")
        ))
        .execute(&pool)
        .await?;
        let observer = PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await?;
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
        let stream = async_nats::jetstream::new(client.clone())
            .create_stream(async_nats::jetstream::stream::Config {
                name: "RESOURCE_EVENTS".into(),
                subjects: vec!["labweaver.resource.>".into()],
                no_ack,
                ..Default::default()
            })
            .await?;
        let publications = client
            .subscribe(subjects::RESOURCE_REQUEST_SUBMITTED)
            .await?;
        client.flush().await?;
        Ok(Self {
            pool,
            observer,
            url,
            client,
            stream,
            publications,
            _postgres: postgres,
            nats,
        })
    }

    fn dispatcher(&self, budget: Duration) -> TestResult<ResourceOutboxDispatcher> {
        Ok(ResourceOutboxDispatcher::new(
            self.pool.clone(),
            self.client.clone(),
            budget,
        )?)
    }

    async fn enqueue(&self) -> TestResult<EventId> {
        let now: UtcTimestamp = "2026-10-16T02:00:00.000Z".parse()?;
        let request = ResourceRequest {
            id: ResourceRequestId::new(),
            generation: 1,
            request_key: "outbox-recovery".into(),
            requester_id: ActorId::new(),
            course_id: None,
            project_id: ProjectId::new(),
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
        let event = CloudEvent {
            specversion: SPEC_VERSION.into(),
            id: EventId::new(),
            source: "urn:labweaver:resource-service".into(),
            event_type: subjects::RESOURCE_REQUEST_SUBMITTED.into(),
            subject: subjects::RESOURCE_REQUEST_SUBMITTED.into(),
            time: now,
            datacontenttype: "application/json".into(),
            dataschema: format!("{DATA_SCHEMA_BASE}/resource-request-submitted.schema.json"),
            project_id: request.project_id,
            course_id: None,
            aggregate_revision: request.revision,
            aggregate_sequence: Sequence(1),
            trace_id: "resource-outbox-recovery".into(),
            data: contracts::events::ResourceRequestChanged { request },
        };
        let payload = serde_json::to_value(&event)?;
        let mut transaction = self.observer.begin().await?;
        OutboxStore::enqueue(
            &mut transaction,
            Domain::Resource,
            event.id.as_uuid(),
            &event.subject,
            &event.event_type,
            event.data.request.id.as_uuid(),
            1,
            &payload,
            Sha256Digest::of_canonical(&payload)?,
        )
        .await?;
        transaction.commit().await?;
        Ok(event.id)
    }

    async fn received(&mut self, event_id: EventId) -> TestResult {
        let message = timeout(DEADLINE, self.publications.next())
            .await?
            .ok_or("publication subscription ended")?;
        let event: CloudEvent<Value> = serde_json::from_slice(&message.payload)?;
        assert_eq!(event.id, event_id);
        assert_eq!(
            message
                .headers
                .as_ref()
                .and_then(|h| h.get(async_nats::header::NATS_MESSAGE_ID)),
            Some(&event_id.to_string().into())
        );
        Ok(())
    }

    async fn stored(&mut self, count: u64) -> TestResult {
        timeout(DEADLINE, async {
            loop {
                if self.stream.info().await?.state.messages == count {
                    return Ok::<_, Box<dyn std::error::Error + Send + Sync>>(());
                }
                tokio::task::yield_now().await;
            }
        })
        .await??;
        Ok(())
    }

    async fn enable_ack(&self) -> TestResult {
        let mut config = self.stream.cached_info().config.clone();
        config.no_ack = false;
        async_nats::jetstream::new(self.client.clone())
            .update_stream(config)
            .await?;
        Ok(())
    }

    async fn expire_claim(&self, event_id: EventId) -> TestResult {
        // Move the real database deadline past the DB clock, without wall-clock sleeps.
        sqlx::query(
            "UPDATE resource.outbox_events
             SET publish_claim_until=clock_timestamp()-interval '1 millisecond'
             WHERE event_id=$1 AND publish_claim_token IS NOT NULL",
        )
        .bind(event_id.as_uuid())
        .execute(&self.observer)
        .await?;
        Ok(())
    }
}

async fn single_connection_pool(url: &str) -> TestResult<PgPool> {
    Ok(PgPoolOptions::new().max_connections(1).connect(url).await?)
}

async fn published(pool: &PgPool, event_id: EventId) -> TestResult<bool> {
    Ok(sqlx::query_scalar(
        "SELECT published_at IS NOT NULL FROM resource.outbox_events WHERE event_id=$1",
    )
    .bind(event_id.as_uuid())
    .fetch_one(pool)
    .await?)
}

/// Delays one genuine server `PubAck`; all protocol bytes and its payload remain unchanged.
struct AckRelay {
    address: std::net::SocketAddr,
    held: mpsc::Receiver<oneshot::Sender<()>>,
    _tasks: JoinSet<std::io::Result<()>>,
}

impl AckRelay {
    async fn new(fixture: &Fixture) -> TestResult<Self> {
        let upstream = format!("127.0.0.1:{}", fixture.nats.get_host_port_ipv4(4222).await?);
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let (send, held) = mpsc::channel(1);
        let mut tasks = JoinSet::new();
        tasks.spawn(async move {
            let (client, _) = listener.accept().await?;
            let server = TcpStream::connect(upstream).await?;
            let (mut client_read, mut client_write) = client.into_split();
            let (server_read, mut server_write) = server.into_split();
            let forward = tokio::io::copy(&mut client_read, &mut server_write);
            let replies = async {
                let mut reader = BufReader::new(server_read);
                let mut delayed = false;
                loop {
                    let mut frame = Vec::new();
                    if reader.read_until(b'\n', &mut frame).await? == 0 {
                        return Ok::<_, std::io::Error>(());
                    }
                    let line = std::str::from_utf8(&frame).map_err(std::io::Error::other)?;
                    let fields: Vec<_> = line.split_whitespace().collect();
                    if fields.first().is_some_and(|v| *v == "MSG" || *v == "HMSG") {
                        let length: usize = fields
                            .last()
                            .ok_or_else(|| std::io::Error::other("missing frame length"))?
                            .parse()
                            .map_err(std::io::Error::other)?;
                        let header_length: usize = if fields[0] == "HMSG" {
                            fields[fields.len() - 2]
                                .parse()
                                .map_err(std::io::Error::other)?
                        } else {
                            0
                        };
                        let mut body = vec![0; length + 2];
                        reader.read_exact(&mut body).await?;
                        let ack = serde_json::from_slice::<Value>(&body[header_length..length]);
                        if !delayed
                            && ack.is_ok_and(|v| {
                                v["stream"] == "RESOURCE_EVENTS" && v["seq"].is_u64()
                            })
                        {
                            delayed = true;
                            let (release, wait) = oneshot::channel();
                            send.send(release).await.map_err(std::io::Error::other)?;
                            if wait.await.is_err() {
                                // Dropping the gate models losing an ACK after real storage.
                                continue;
                            }
                        }
                        frame.extend(body);
                    }
                    client_write.write_all(&frame).await?;
                }
            };
            tokio::try_join!(forward, replies)?;
            Ok(())
        });
        Ok(Self {
            address,
            held,
            _tasks: tasks,
        })
    }

    async fn client(&self) -> TestResult<async_nats::Client> {
        Ok(async_nats::connect(format!("nats://{}", self.address)).await?)
    }

    async fn held_ack(&mut self) -> TestResult<oneshot::Sender<()>> {
        Ok(timeout(DEADLINE, self.held.recv())
            .await?
            .ok_or("ACK relay ended before a real PubAck")?)
    }
}

#[tokio::test]
async fn withheld_ack_releases_connection_and_row_lock_then_cancel_recovers() -> TestResult {
    let mut fixture = Fixture::new(true).await?;
    let event_id = fixture.enqueue().await?;
    let dispatcher = fixture.dispatcher(DEADLINE)?;
    let mut tasks = JoinSet::new();
    let pending = tasks.spawn(async move { dispatcher.dispatch_once().await });
    fixture.received(event_id).await?;
    fixture.stored(1).await?;

    let value: i32 = timeout(
        Duration::from_secs(1),
        sqlx::query_scalar("SELECT 42").fetch_one(&fixture.pool),
    )
    .await
    .expect("a withheld ACK must leave the only pool connection available")?;
    assert_eq!(value, 42);
    let mut observer = fixture.observer.begin().await?;
    sqlx::query("SELECT event_id FROM resource.outbox_events WHERE event_id=$1 FOR UPDATE NOWAIT")
        .bind(event_id.as_uuid())
        .fetch_one(&mut *observer)
        .await?;
    observer.rollback().await?;
    assert!(!published(&fixture.observer, event_id).await?);
    assert!(
        !pending.is_finished(),
        "the real broker still withholds ACK"
    );

    pending.abort();
    let cancelled = timeout(DEADLINE, tasks.join_next())
        .await?
        .ok_or("missing publisher")?;
    assert!(cancelled.is_err_and(|error| error.is_cancelled()));
    fixture.pool.close().await;
    assert!(!published(&fixture.observer, event_id).await?);
    fixture.pool = single_connection_pool(&fixture.url).await?;
    fixture.enable_ack().await?;
    // A crashed worker leaves its claim until the persisted lease expires.
    assert_eq!(
        fixture.dispatcher(DEADLINE)?.dispatch_once().await?,
        ResourceOutboxOutcome::Idle
    );
    fixture.expire_claim(event_id).await?;
    assert_eq!(
        fixture.dispatcher(DEADLINE)?.dispatch_once().await?,
        ResourceOutboxOutcome::Published { event_id }
    );
    fixture.stored(1).await?;
    fixture.pool.close().await;
    assert!(published(&fixture.observer, event_id).await?);
    Ok(())
}

#[tokio::test]
async fn real_jetstream_storage_without_ack_stays_pending_and_retries() -> TestResult {
    let mut fixture = Fixture::new(true).await?;
    let event_id = fixture.enqueue().await?;
    let error = fixture
        .dispatcher(Duration::from_millis(300))?
        .dispatch_once()
        .await
        .expect_err("storage alone is not a positive publish ACK");
    assert!(matches!(error, ResourceOutboxError::Timeout));
    fixture.received(event_id).await?;
    fixture.stored(1).await?;
    assert!(!published(&fixture.observer, event_id).await?);
    fixture.enable_ack().await?;
    assert_eq!(
        fixture.dispatcher(DEADLINE)?.dispatch_once().await?,
        ResourceOutboxOutcome::Published { event_id }
    );
    fixture.received(event_id).await?;
    fixture.stored(1).await?;
    assert_eq!(
        fixture.dispatcher(DEADLINE)?.dispatch_once().await?,
        ResourceOutboxOutcome::Idle
    );
    Ok(())
}

#[tokio::test]
async fn ack_followed_by_database_failure_leaves_a_replayable_event() -> TestResult {
    let mut fixture = Fixture::new(false).await?;
    let event_id = fixture.enqueue().await?;
    sqlx::raw_sql(
        "CREATE FUNCTION resource.reject_outbox_mark() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN RAISE EXCEPTION 'injected mark failure' USING ERRCODE='40001'; END $$;
         CREATE TRIGGER reject_outbox_mark BEFORE UPDATE ON resource.outbox_events
         FOR EACH ROW WHEN (NEW.published_at IS NOT NULL)
         EXECUTE FUNCTION resource.reject_outbox_mark();",
    )
    .execute(&fixture.observer)
    .await?;
    assert!(matches!(
        fixture.dispatcher(DEADLINE)?.dispatch_once().await,
        Err(ResourceOutboxError::Database(_))
    ));
    fixture.received(event_id).await?;
    fixture.stored(1).await?;
    assert!(!published(&fixture.observer, event_id).await?);
    sqlx::query("DROP TRIGGER reject_outbox_mark ON resource.outbox_events")
        .execute(&fixture.observer)
        .await?;
    assert_eq!(
        fixture.dispatcher(DEADLINE)?.dispatch_once().await?,
        ResourceOutboxOutcome::Published { event_id }
    );
    fixture.received(event_id).await?;
    fixture.stored(1).await?;
    fixture.pool.close().await;
    assert!(published(&fixture.observer, event_id).await?);
    Ok(())
}

#[tokio::test]
async fn another_worker_publishes_the_next_event_while_the_first_ack_is_delayed() -> TestResult {
    let mut fixture = Fixture::new(false).await?;
    let first = fixture.enqueue().await?;
    let second = fixture.enqueue().await?;
    let mut relay = AckRelay::new(&fixture).await?;
    let dispatcher =
        ResourceOutboxDispatcher::new(fixture.pool.clone(), relay.client().await?, DEADLINE)?;
    let mut tasks = JoinSet::new();
    tasks.spawn(async move { dispatcher.dispatch_once().await });
    let release = relay.held_ack().await?;
    fixture.received(first).await?;

    // The same single-connection pool can claim and mark another event while A waits.
    assert_eq!(
        timeout(
            Duration::from_secs(2),
            fixture.dispatcher(DEADLINE)?.dispatch_once()
        )
        .await??,
        ResourceOutboxOutcome::Published { event_id: second }
    );
    fixture.received(second).await?;
    assert!(!published(&fixture.observer, first).await?);
    assert!(published(&fixture.observer, second).await?);
    release.send(()).map_err(|()| "publisher dropped its ACK")?;
    assert_eq!(
        timeout(DEADLINE, tasks.join_next())
            .await?
            .ok_or("missing publisher")???,
        ResourceOutboxOutcome::Published { event_id: first }
    );
    fixture.stored(2).await?;
    Ok(())
}

#[tokio::test]
async fn expired_owner_ack_cannot_clear_a_new_claim_and_converges_after_completion() -> TestResult {
    let mut fixture = Fixture::new(false).await?;
    for replacement_finishes_first in [false, true] {
        let event_id = fixture.enqueue().await?;
        let mut old_relay = AckRelay::new(&fixture).await?;
        let mut new_relay = AckRelay::new(&fixture).await?;
        let old = ResourceOutboxDispatcher::new(
            fixture.pool.clone(),
            old_relay.client().await?,
            DEADLINE,
        )?;
        let new = ResourceOutboxDispatcher::new(
            fixture.pool.clone(),
            new_relay.client().await?,
            DEADLINE,
        )?;
        let mut old_tasks = JoinSet::new();
        let mut new_tasks = JoinSet::new();
        old_tasks.spawn(async move { old.dispatch_once().await });
        let old_ack = old_relay.held_ack().await?;
        fixture.received(event_id).await?;
        fixture.expire_claim(event_id).await?;
        new_tasks.spawn(async move { new.dispatch_once().await });
        let new_ack = new_relay.held_ack().await?;
        fixture.received(event_id).await?;
        let new_token: uuid::Uuid = sqlx::query_scalar(
            "SELECT publish_claim_token FROM resource.outbox_events WHERE event_id=$1",
        )
        .bind(event_id.as_uuid())
        .fetch_one(&fixture.observer)
        .await?;

        if replacement_finishes_first {
            new_ack.send(()).map_err(|()| "replacement dropped ACK")?;
            assert_eq!(
                timeout(DEADLINE, new_tasks.join_next())
                    .await?
                    .ok_or("missing replacement")???,
                ResourceOutboxOutcome::Published { event_id }
            );
            let timestamp: time::OffsetDateTime = sqlx::query_scalar(
                "SELECT published_at FROM resource.outbox_events WHERE event_id=$1",
            )
            .bind(event_id.as_uuid())
            .fetch_one(&fixture.observer)
            .await?;
            old_ack.send(()).map_err(|()| "old publisher dropped ACK")?;
            assert_eq!(
                timeout(DEADLINE, old_tasks.join_next())
                    .await?
                    .ok_or("missing old publisher")???,
                ResourceOutboxOutcome::Published { event_id }
            );
            let after: time::OffsetDateTime = sqlx::query_scalar(
                "SELECT published_at FROM resource.outbox_events WHERE event_id=$1",
            )
            .bind(event_id.as_uuid())
            .fetch_one(&fixture.observer)
            .await?;
            assert_eq!(
                timestamp, after,
                "a late ACK must preserve the original completion"
            );
        } else {
            old_ack.send(()).map_err(|()| "old publisher dropped ACK")?;
            assert!(matches!(
                timeout(DEADLINE, old_tasks.join_next())
                    .await?
                    .ok_or("missing old publisher")??,
                Err(ResourceOutboxError::FenceLost)
            ));
            let still_owned: uuid::Uuid = sqlx::query_scalar(
                "SELECT publish_claim_token FROM resource.outbox_events WHERE event_id=$1",
            )
            .bind(event_id.as_uuid())
            .fetch_one(&fixture.observer)
            .await?;
            assert_eq!(still_owned, new_token);
            assert!(!published(&fixture.observer, event_id).await?);
            new_ack.send(()).map_err(|()| "replacement dropped ACK")?;
            assert_eq!(
                timeout(DEADLINE, new_tasks.join_next())
                    .await?
                    .ok_or("missing replacement")???,
                ResourceOutboxOutcome::Published { event_id }
            );
        }
    }
    fixture.stored(2).await?;
    fixture.pool.close().await;
    let pending: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM resource.outbox_events
         WHERE published_at IS NULL OR publish_claim_token IS NOT NULL OR publish_claim_until IS NOT NULL",
    ).fetch_one(&fixture.observer).await?;
    assert_eq!(pending, 0);
    Ok(())
}

#[tokio::test]
async fn cancellation_after_positive_ack_and_during_mark_converges_after_pool_close() -> TestResult
{
    let mut fixture = Fixture::new(false).await?;
    let event_id = fixture.enqueue().await?;
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&fixture.pool)
        .await?;
    let mut relay = AckRelay::new(&fixture).await?;
    let dispatcher =
        ResourceOutboxDispatcher::new(fixture.pool.clone(), relay.client().await?, DEADLINE)?;
    let mut tasks = JoinSet::new();
    let pending = tasks.spawn(async move { dispatcher.dispatch_once().await });
    let ack = relay.held_ack().await?;
    fixture.received(event_id).await?;
    let mut blocker = fixture.observer.begin().await?;
    sqlx::query("SELECT event_id FROM resource.outbox_events WHERE event_id=$1 FOR UPDATE")
        .bind(event_id.as_uuid())
        .fetch_one(&mut *blocker)
        .await?;
    ack.send(()).map_err(|()| "publisher dropped ACK")?;
    timeout(DEADLINE, async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_stat_activity
                 WHERE pid=$1 AND wait_event_type='Lock'
                   AND query LIKE 'UPDATE resource.outbox_events%')",
            )
            .bind(pid)
            .fetch_one(&fixture.observer)
            .await?;
            if waiting {
                return Ok::<_, sqlx::Error>(());
            }
            tokio::task::yield_now().await;
        }
    })
    .await??;
    pending.abort();
    let cancelled = timeout(DEADLINE, tasks.join_next())
        .await?
        .ok_or("missing publisher")?;
    assert!(cancelled.is_err_and(|error| error.is_cancelled()));
    blocker.commit().await?;
    timeout(DEADLINE, fixture.pool.close()).await?;
    // Dropping a SQL future does not retract an UPDATE already sent to PostgreSQL.
    // Its commit is legal because the real broker ACK preceded the mark.
    let committed = published(&fixture.observer, event_id).await?;
    fixture.pool = single_connection_pool(&fixture.url).await?;
    fixture.expire_claim(event_id).await?;
    let result = fixture.dispatcher(DEADLINE)?.dispatch_once().await?;
    assert_eq!(
        result,
        if committed {
            ResourceOutboxOutcome::Idle
        } else {
            ResourceOutboxOutcome::Published { event_id }
        }
    );
    fixture.pool.close().await;
    assert!(published(&fixture.observer, event_id).await?);
    fixture.stored(1).await?;
    Ok(())
}

#[tokio::test]
async fn invalid_identity_and_inflight_payload_drift_never_mark_the_wrong_snapshot() -> TestResult {
    let mut fixture = Fixture::new(false).await?;
    let event_id = fixture.enqueue().await?;
    let payload: Value =
        sqlx::query_scalar("SELECT payload FROM resource.outbox_events WHERE event_id=$1")
            .bind(event_id.as_uuid())
            .fetch_one(&fixture.observer)
            .await?;
    let hash = Sha256Digest::of_canonical(&payload)?.to_string();
    sqlx::query("UPDATE resource.outbox_events SET payload_sha256=$2 WHERE event_id=$1")
        .bind(event_id.as_uuid())
        .bind("0".repeat(64))
        .execute(&fixture.observer)
        .await?;
    assert!(matches!(
        fixture.dispatcher(DEADLINE)?.dispatch_once().await,
        Err(ResourceOutboxError::Identity)
    ));
    let mut invalid = payload.clone();
    invalid["dataschema"] = "https://invalid.example/schema.json".into();
    sqlx::query("UPDATE resource.outbox_events SET payload=$2,payload_sha256=$3 WHERE event_id=$1")
        .bind(event_id.as_uuid())
        .bind(&invalid)
        .bind(Sha256Digest::of_canonical(&invalid)?.to_string())
        .execute(&fixture.observer)
        .await?;
    assert!(matches!(
        fixture.dispatcher(DEADLINE)?.dispatch_once().await,
        Err(ResourceOutboxError::Contract)
    ));
    assert_eq!(fixture.stream.info().await?.state.messages, 0);
    sqlx::query("UPDATE resource.outbox_events SET payload=$2,payload_sha256=$3 WHERE event_id=$1")
        .bind(event_id.as_uuid())
        .bind(&payload)
        .bind(&hash)
        .execute(&fixture.observer)
        .await?;

    let mut relay = AckRelay::new(&fixture).await?;
    let dispatcher =
        ResourceOutboxDispatcher::new(fixture.pool.clone(), relay.client().await?, DEADLINE)?;
    let mut tasks = JoinSet::new();
    tasks.spawn(async move { dispatcher.dispatch_once().await });
    let ack = relay.held_ack().await?;
    fixture.received(event_id).await?;
    let mut changed = payload.clone();
    changed["traceId"] = "changed-after-publication".into();
    // Simulate corruption that deliberately fails to update the stored digest.
    sqlx::query("UPDATE resource.outbox_events SET payload=$2 WHERE event_id=$1")
        .bind(event_id.as_uuid())
        .bind(changed)
        .execute(&fixture.observer)
        .await?;
    ack.send(()).map_err(|()| "publisher dropped ACK")?;
    assert!(matches!(
        timeout(DEADLINE, tasks.join_next())
            .await?
            .ok_or("missing publisher")??,
        Err(ResourceOutboxError::FenceLost)
    ));
    assert!(!published(&fixture.observer, event_id).await?);
    sqlx::query("UPDATE resource.outbox_events SET payload=$2 WHERE event_id=$1")
        .bind(event_id.as_uuid())
        .bind(payload)
        .execute(&fixture.observer)
        .await?;
    assert_eq!(
        fixture.dispatcher(DEADLINE)?.dispatch_once().await?,
        ResourceOutboxOutcome::Published { event_id }
    );
    fixture.stored(1).await?;
    Ok(())
}

#[tokio::test]
async fn duplicate_real_deliveries_use_the_shared_inbox_in_the_side_effect_transaction()
-> TestResult {
    let mut fixture = Fixture::new(false).await?;
    let event_id = fixture.enqueue().await?;
    fixture.dispatcher(DEADLINE)?.dispatch_once().await?;
    let first = fixture.stream.get_raw_message(1).await?;
    // Deliberately omit transport dedup on this real second copy. Consumer correctness
    // must not depend on the finite JetStream dedup window retaining this message ID.
    async_nats::jetstream::new(fixture.client.clone())
        .publish(subjects::RESOURCE_REQUEST_SUBMITTED, first.payload.clone())
        .await?
        .await?;
    fixture.stored(2).await?;
    sqlx::query("CREATE TABLE public.observed_effects (event_id uuid NOT NULL)")
        .execute(&fixture.observer)
        .await?;
    let mut decisions = Vec::new();
    for sequence in [1, 2] {
        let message = fixture.stream.get_raw_message(sequence).await?;
        let event: CloudEvent<Value> = serde_json::from_slice(&message.payload)?;
        assert_eq!(event.id, event_id);
        let aggregate_id: uuid::Uuid = event.data["request"]["id"]
            .as_str()
            .ok_or("missing request ID")?
            .parse()?;
        let mut transaction = fixture.observer.begin().await?;
        let decision = InboxStore::accept(
            &mut transaction,
            Domain::Resource,
            "outbox-test-consumer",
            event.id.as_uuid(),
            aggregate_id,
            event.aggregate_sequence.0,
            Sha256Digest::of_canonical(&serde_json::to_value(&event)?)?,
        )
        .await?;
        if decision == InboxDecision::Accepted {
            sqlx::query("INSERT INTO public.observed_effects VALUES ($1)")
                .bind(event_id.as_uuid())
                .execute(&mut *transaction)
                .await?;
        }
        transaction.commit().await?;
        decisions.push(decision);
    }
    assert_eq!(
        decisions,
        [InboxDecision::Accepted, InboxDecision::Duplicate]
    );
    fixture.pool.close().await;
    let effects: i64 = sqlx::query_scalar("SELECT count(*) FROM public.observed_effects")
        .fetch_one(&fixture.observer)
        .await?;
    assert_eq!(effects, 1);
    assert!(published(&fixture.observer, event_id).await?);
    Ok(())
}
