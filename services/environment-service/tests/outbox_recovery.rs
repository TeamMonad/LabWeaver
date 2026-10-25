//! Real PostgreSQL/JetStream recovery and process signal boundaries.

use std::time::Duration;

use contracts::events::{CloudEvent, DATA_SCHEMA_BASE, EnvironmentEvent, SPEC_VERSION, subjects};
use contracts::{EnvironmentId, EventId, ProjectId, Revision, Sequence, UtcTimestamp};
use environment_service::{
    JetStreamEventPublisher, OutboxDispatchError, OutboxDispatchOutcome, OutboxDispatcher,
};
use futures_util::StreamExt;
use persistence_sqlx::{Domain, OutboxStore, Sha256Digest};
use serde_json::Value;
use sqlx::{PgPool, postgres::PgPoolOptions};
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::{ContainerAsync, GenericImage, ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinSet;
use tokio::time::timeout;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;
const DEADLINE: Duration = Duration::from_secs(10);

struct Fixture {
    pool: PgPool,
    observer: PgPool,
    database_url: String,
    nats_url: String,
    client: async_nats::Client,
    stream: async_nats::jetstream::stream::Stream,
    publications: async_nats::Subscriber,
    _postgres: ContainerAsync<Postgres>,
    _nats: ContainerAsync<GenericImage>,
}

impl Fixture {
    async fn new(no_ack: bool) -> TestResult<Self> {
        let postgres = Postgres::default().with_tag("17.5-alpine").start().await?;
        let database_url = format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            postgres.get_host_port_ipv4(5432).await?
        );
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "CREATE SCHEMA environment; SET search_path TO environment;\n{}\n{}",
            include_str!("../../../migrations/environment/0001_platform_baseline.sql"),
            include_str!("../../../migrations/environment/0009_outbox_publication_claims.sql"),
        )))
        .execute(&pool)
        .await?;
        let observer = PgPoolOptions::new()
            .max_connections(2)
            .connect(&database_url)
            .await?;
        let nats = GenericImage::new("nats", "2.11.8-alpine")
            .with_exposed_port(4222.tcp())
            .with_wait_for(WaitFor::message_on_stderr("Server is ready"))
            .with_cmd(["-js"])
            .start()
            .await?;
        let nats_url = format!("nats://127.0.0.1:{}", nats.get_host_port_ipv4(4222).await?);
        let client = async_nats::connect(&nats_url).await?;
        let stream = async_nats::jetstream::new(client.clone())
            .create_stream(async_nats::jetstream::stream::Config {
                name: "ENVIRONMENT_EVENTS".into(),
                subjects: vec!["labweaver.environment.instance.>".into()],
                no_ack,
                ..Default::default()
            })
            .await?;
        let publications = client
            .subscribe(subjects::ENVIRONMENT_STATE_CHANGED)
            .await?;
        client.flush().await?;
        Ok(Self {
            pool,
            observer,
            database_url,
            nats_url,
            client,
            stream,
            publications,
            _postgres: postgres,
            _nats: nats,
        })
    }

    fn dispatcher(
        &self,
        budget: Duration,
    ) -> TestResult<OutboxDispatcher<JetStreamEventPublisher>> {
        Ok(OutboxDispatcher::new(
            self.pool.clone(),
            JetStreamEventPublisher::new(self.client.clone(), budget),
            budget,
        )?)
    }

    async fn enqueue(&self) -> TestResult<EventId> {
        let environment_id = EnvironmentId::new();
        let event = CloudEvent {
            specversion: SPEC_VERSION.into(),
            id: EventId::new(),
            source: "urn:labweaver:environment-service".into(),
            event_type: subjects::ENVIRONMENT_STATE_CHANGED.into(),
            subject: subjects::ENVIRONMENT_STATE_CHANGED.into(),
            time: "2026-10-25T02:00:00.000Z".parse::<UtcTimestamp>()?,
            datacontenttype: "application/json".into(),
            dataschema: format!("{DATA_SCHEMA_BASE}/environment-state-changed.schema.json"),
            project_id: ProjectId::new(),
            course_id: None,
            aggregate_revision: Revision::new(1)?,
            aggregate_sequence: Sequence(1),
            trace_id: "environment-outbox-recovery".into(),
            data: EnvironmentEvent {
                environment_id,
                generation: 1,
                state: "running".into(),
                operation_id: None,
                diagnostic_code: None,
            },
        };
        let payload = serde_json::to_value(&event)?;
        let mut transaction = self.observer.begin().await?;
        OutboxStore::enqueue(
            &mut transaction,
            Domain::Environment,
            event.id.as_uuid(),
            &event.subject,
            &event.event_type,
            environment_id.as_uuid(),
            1,
            &payload,
            Sha256Digest::of_canonical(&payload)?,
        )
        .await?;
        transaction.commit().await?;
        Ok(event.id)
    }

    async fn received(&mut self, id: EventId) -> TestResult {
        let message = timeout(DEADLINE, self.publications.next())
            .await?
            .ok_or("publication subscription ended")?;
        let event: CloudEvent<Value> = serde_json::from_slice(&message.payload)?;
        assert_eq!(event.id, id);
        assert_eq!(
            message
                .headers
                .as_ref()
                .and_then(|headers| headers.get(async_nats::header::NATS_MESSAGE_ID)),
            Some(&id.to_string().into())
        );
        Ok(())
    }

    async fn expire(&self, id: EventId) -> TestResult {
        sqlx::query("UPDATE environment.outbox_events SET publish_claim_until=clock_timestamp()-interval '1 millisecond' WHERE event_id=$1 AND publish_claim_token IS NOT NULL")
            .bind(id.as_uuid()).execute(&self.observer).await?;
        Ok(())
    }

    async fn enable_ack(&self) -> TestResult {
        let mut configuration = self.stream.cached_info().config.clone();
        configuration.no_ack = false;
        async_nats::jetstream::new(self.client.clone())
            .update_stream(configuration)
            .await?;
        Ok(())
    }

    async fn published(&self, id: EventId) -> TestResult<bool> {
        Ok(sqlx::query_scalar(
            "SELECT published_at IS NOT NULL FROM environment.outbox_events WHERE event_id=$1",
        )
        .bind(id.as_uuid())
        .fetch_one(&self.observer)
        .await?)
    }

    async fn token(&self, id: EventId) -> TestResult<Option<uuid::Uuid>> {
        Ok(sqlx::query_scalar(
            "SELECT publish_claim_token FROM environment.outbox_events WHERE event_id=$1",
        )
        .bind(id.as_uuid())
        .fetch_one(&self.observer)
        .await?)
    }
}

/// Holds exactly one genuine broker `PubAck` without changing its bytes.
struct AckRelay {
    address: std::net::SocketAddr,
    held: mpsc::Receiver<oneshot::Sender<()>>,
    _tasks: JoinSet<std::io::Result<()>>,
}

impl AckRelay {
    async fn new(upstream: &str) -> TestResult<Self> {
        let upstream = upstream.trim_start_matches("nats://").to_owned();
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let (sender, held) = mpsc::channel(1);
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
                    if fields
                        .first()
                        .is_some_and(|field| *field == "MSG" || *field == "HMSG")
                    {
                        let length: usize = fields
                            .last()
                            .ok_or_else(|| std::io::Error::other("missing frame length"))?
                            .parse()
                            .map_err(std::io::Error::other)?;
                        let headers: usize = if fields[0] == "HMSG" {
                            fields[fields.len() - 2]
                                .parse()
                                .map_err(std::io::Error::other)?
                        } else {
                            0
                        };
                        let mut body = vec![0; length + 2];
                        reader.read_exact(&mut body).await?;
                        if !delayed
                            && serde_json::from_slice::<Value>(&body[headers..length]).is_ok_and(
                                |value| {
                                    value["stream"] == "ENVIRONMENT_EVENTS" && value["seq"].is_u64()
                                },
                            )
                        {
                            delayed = true;
                            let (release, wait) = oneshot::channel();
                            sender.send(release).await.map_err(std::io::Error::other)?;
                            if wait.await.is_err() {
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

    async fn dispatcher(
        &self,
        fixture: &Fixture,
    ) -> TestResult<OutboxDispatcher<JetStreamEventPublisher>> {
        let client = async_nats::connect(format!("nats://{}", self.address)).await?;
        Ok(OutboxDispatcher::new(
            fixture.pool.clone(),
            JetStreamEventPublisher::new(client, DEADLINE),
            DEADLINE,
        )?)
    }

    async fn held_ack(&mut self) -> TestResult<oneshot::Sender<()>> {
        timeout(DEADLINE, self.held.recv())
            .await?
            .ok_or_else(|| "relay did not receive a genuine PubAck".into())
    }
}

#[tokio::test]
async fn ack_wait_releases_single_connection_and_row_lock_and_cancel_replays_same_id() -> TestResult
{
    let mut fixture = Fixture::new(true).await?;
    let id = fixture.enqueue().await?;
    let dispatcher = fixture.dispatcher(DEADLINE)?;
    let mut tasks = JoinSet::new();
    let pending = tasks.spawn(async move { dispatcher.dispatch_once().await });
    fixture.received(id).await?;
    timeout(Duration::from_secs(1), async {
        let mut transaction = fixture.pool.begin().await?;
        sqlx::query("CREATE TABLE public.ack_wait_committed (id integer)")
            .execute(&mut *transaction)
            .await?;
        sqlx::query("INSERT INTO public.ack_wait_committed VALUES (1)")
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await
    })
    .await??;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM public.ack_wait_committed")
            .fetch_one(&fixture.observer)
            .await?,
        1
    );
    let mut transaction = fixture.observer.begin().await?;
    sqlx::query(
        "SELECT event_id FROM environment.outbox_events WHERE event_id=$1 FOR UPDATE NOWAIT",
    )
    .bind(id.as_uuid())
    .fetch_one(&mut *transaction)
    .await?;
    transaction.rollback().await?;
    assert!(!fixture.published(id).await?);
    pending.abort();
    assert!(
        tasks
            .join_next()
            .await
            .ok_or("missing publisher")?
            .is_err_and(|error| error.is_cancelled())
    );
    fixture.enable_ack().await?;
    assert_eq!(
        fixture.dispatcher(DEADLINE)?.dispatch_once().await?,
        OutboxDispatchOutcome::Idle
    );
    fixture.expire(id).await?;
    assert_eq!(
        fixture.dispatcher(DEADLINE)?.dispatch_once().await?,
        OutboxDispatchOutcome::Published { event_id: id }
    );
    fixture.received(id).await?;
    assert!(fixture.published(id).await?);
    Ok(())
}

#[tokio::test]
async fn expired_owner_ack_and_release_cannot_clear_replacement_claim() -> TestResult {
    let fixture = Fixture::new(false).await?;
    let id = fixture.enqueue().await?;
    let mut first_relay = AckRelay::new(&fixture.nats_url).await?;
    let first = first_relay.dispatcher(&fixture).await?;
    let mut tasks = JoinSet::new();
    tasks.spawn(async move { first.dispatch_once().await });
    let first_ack = first_relay.held_ack().await?;
    let old_token = fixture.token(id).await?.ok_or("first claim missing")?;
    fixture.expire(id).await?;
    let mut second_relay = AckRelay::new(&fixture.nats_url).await?;
    let second = second_relay.dispatcher(&fixture).await?;
    tasks.spawn(async move { second.dispatch_once().await });
    let second_ack = second_relay.held_ack().await?;
    let new_token = fixture
        .token(id)
        .await?
        .ok_or("replacement claim missing")?;
    assert_ne!(new_token, old_token);
    first_ack.send(()).map_err(|()| "first ACK gate closed")?;
    assert!(matches!(
        timeout(DEADLINE, tasks.join_next())
            .await?
            .ok_or("first owner missing")??,
        Err(OutboxDispatchError::PublishFenceLost)
    ));
    assert_eq!(fixture.token(id).await?, Some(new_token));
    assert!(!fixture.published(id).await?);
    second_ack.send(()).map_err(|()| "second ACK gate closed")?;
    assert_eq!(
        timeout(DEADLINE, tasks.join_next())
            .await?
            .ok_or("second owner missing")???,
        OutboxDispatchOutcome::Published { event_id: id }
    );
    assert!(fixture.published(id).await?);
    Ok(())
}

#[tokio::test]
async fn acknowledged_event_snapshot_mutation_is_not_marked_published() -> TestResult {
    let fixture = Fixture::new(false).await?;
    let id = fixture.enqueue().await?;
    let mut relay = AckRelay::new(&fixture.nats_url).await?;
    let dispatcher = relay.dispatcher(&fixture).await?;
    let mut tasks = JoinSet::new();
    tasks.spawn(async move { dispatcher.dispatch_once().await });
    let ack = relay.held_ack().await?;
    sqlx::query("UPDATE environment.outbox_events SET payload=jsonb_set(payload,'{traceId}','\"changed-after-publication\"') WHERE event_id=$1").bind(id.as_uuid()).execute(&fixture.observer).await?;
    ack.send(()).map_err(|()| "ACK gate closed")?;
    assert!(matches!(
        tasks.join_next().await.ok_or("publisher missing")??,
        Err(OutboxDispatchError::PublishFenceLost)
    ));
    assert!(!fixture.published(id).await?);
    assert!(fixture.token(id).await?.is_none());
    Ok(())
}

#[tokio::test]
async fn transient_mark_failure_releases_claim_and_recovers_after_real_ack() -> TestResult {
    let mut fixture = Fixture::new(false).await?;
    let id = fixture.enqueue().await?;
    sqlx::raw_sql("CREATE FUNCTION environment.reject_mark() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected serialization failure' USING ERRCODE='40001'; END $$; CREATE TRIGGER reject_mark BEFORE UPDATE ON environment.outbox_events FOR EACH ROW WHEN (NEW.published_at IS NOT NULL) EXECUTE FUNCTION environment.reject_mark();").execute(&fixture.observer).await?;
    assert!(matches!(
        fixture.dispatcher(DEADLINE)?.dispatch_once().await,
        Err(OutboxDispatchError::Database(_))
    ));
    fixture.received(id).await?;
    assert!(fixture.token(id).await?.is_none());
    assert!(!fixture.published(id).await?);
    sqlx::query("DROP TRIGGER reject_mark ON environment.outbox_events")
        .execute(&fixture.observer)
        .await?;
    assert_eq!(
        fixture.dispatcher(DEADLINE)?.dispatch_once().await?,
        OutboxDispatchOutcome::Published { event_id: id }
    );
    fixture.received(id).await?;
    Ok(())
}

/// Subprocess entry point, invoked with real fixture URLs by the signal test below.
#[tokio::test]
#[ignore = "subprocess worker helper; parent test launches this exact test with --ignored"]
async fn outbox_worker_process() -> TestResult {
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&std::env::var("LW_TEST_OUTBOX_DATABASE")?)
        .await?;
    let client = async_nats::connect(std::env::var("LW_TEST_OUTBOX_NATS")?).await?;
    let dispatcher = OutboxDispatcher::new(
        pool,
        JetStreamEventPublisher::new(client, Duration::from_mins(1)),
        Duration::from_mins(1),
    )?;
    let observer = tokio::spawn(async {
        environment_service::service_runtime::wait_for_shutdown().await;
        println!("OUTBOX_STOPPING");
    });
    environment_service::service_runtime::supervise(
        "environment-outbox-worker",
        dispatcher.run(
            Duration::from_millis(10),
            environment_service::service_runtime::subscribe_shutdown(),
        ),
    )
    .await?;
    observer.abort();
    Ok(())
}

fn spawn_worker(fixture: &Fixture, nats_url: &str) -> TestResult<tokio::process::Child> {
    Ok(tokio::process::Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "outbox_worker_process",
            "--ignored",
            "--nocapture",
        ])
        .env("LW_TEST_OUTBOX_DATABASE", &fixture.database_url)
        .env("LW_TEST_OUTBOX_NATS", nats_url)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true)
        .spawn()?)
}

async fn signal(child: &tokio::process::Child, kind: &str) -> TestResult {
    let status = tokio::process::Command::new("kill")
        .args([
            kind,
            &child.id().ok_or("worker already exited")?.to_string(),
        ])
        .status()
        .await?;
    assert!(status.success());
    Ok(())
}

#[tokio::test]
async fn real_worker_sigterm_stops_claiming_then_restart_recovers_crashed_lease() -> TestResult {
    let mut fixture = Fixture::new(false).await?;
    let first_id = fixture.enqueue().await?;
    let mut relay = AckRelay::new(&fixture.nats_url).await?;
    let mut child = spawn_worker(&fixture, &format!("nats://{}", relay.address))?;
    let ack = relay.held_ack().await?;
    fixture.received(first_id).await?;
    signal(&child, "-TERM").await?;
    let mut output = BufReader::new(child.stdout.take().ok_or("missing worker stdout")?).lines();
    timeout(DEADLINE, async {
        while let Some(line) = output.next_line().await? {
            if line.contains("OUTBOX_STOPPING") {
                return Ok::<_, std::io::Error>(());
            }
        }
        Err(std::io::Error::other("worker did not observe shutdown"))
    })
    .await??;
    let second_id = fixture.enqueue().await?;
    ack.send(()).map_err(|()| "ACK gate closed")?;
    assert!(
        timeout(Duration::from_secs(25), child.wait())
            .await??
            .success()
    );
    assert!(fixture.published(first_id).await?);
    assert!(!fixture.published(second_id).await?);
    assert!(
        fixture.token(second_id).await?.is_none(),
        "shutdown must not claim a later event"
    );

    // A hard process loss, unlike completed drain, leaves a durable claim for lease takeover.
    let mut crash_relay = AckRelay::new(&fixture.nats_url).await?;
    let mut crashed = spawn_worker(&fixture, &format!("nats://{}", crash_relay.address))?;
    let held_ack = crash_relay.held_ack().await?;
    fixture.received(second_id).await?;
    let abandoned_token = fixture
        .token(second_id)
        .await?
        .ok_or("crashed worker did not claim")?;
    signal(&crashed, "-KILL").await?;
    assert!(!timeout(DEADLINE, crashed.wait()).await??.success());
    drop(held_ack);
    assert_eq!(fixture.token(second_id).await?, Some(abandoned_token));
    assert_eq!(
        fixture.dispatcher(DEADLINE)?.dispatch_once().await?,
        OutboxDispatchOutcome::Idle
    );
    fixture.expire(second_id).await?;
    let mut restarted = spawn_worker(&fixture, &fixture.nats_url)?;
    fixture.received(second_id).await?;
    timeout(DEADLINE, async {
        while !fixture.published(second_id).await? {
            tokio::task::yield_now().await;
        }
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
    })
    .await??;
    signal(&restarted, "-TERM").await?;
    assert!(
        timeout(Duration::from_secs(25), restarted.wait())
            .await??
            .success()
    );
    assert!(fixture.token(second_id).await?.is_none());
    Ok(())
}

#[tokio::test]
async fn real_worker_drain_deadline_abandons_unacknowledged_claim_for_takeover() -> TestResult {
    let mut fixture = Fixture::new(true).await?;
    let first_id = fixture.enqueue().await?;
    let mut child = spawn_worker(&fixture, &fixture.nats_url)?;
    fixture.received(first_id).await?;
    let token = fixture
        .token(first_id)
        .await?
        .ok_or("worker claim missing")?;
    signal(&child, "-TERM").await?;
    let mut output = BufReader::new(child.stdout.take().ok_or("missing worker stdout")?).lines();
    timeout(DEADLINE, async {
        while let Some(line) = output.next_line().await? {
            if line.contains("OUTBOX_STOPPING") {
                return Ok::<_, std::io::Error>(());
            }
        }
        Err(std::io::Error::other("worker did not observe shutdown"))
    })
    .await??;
    let second_id = fixture.enqueue().await?;
    assert!(
        timeout(Duration::from_secs(25), child.wait())
            .await??
            .success(),
        "the 60-second broker wait must not outlive the 20-second process drain"
    );
    assert_eq!(fixture.token(first_id).await?, Some(token));
    assert!(!fixture.published(first_id).await?);
    assert!(fixture.token(second_id).await?.is_none());
    let mut transaction = fixture.pool.begin().await?;
    sqlx::query(
        "SELECT event_id FROM environment.outbox_events WHERE event_id=$1 FOR UPDATE NOWAIT",
    )
    .bind(first_id.as_uuid())
    .fetch_one(&mut *transaction)
    .await?;
    transaction.commit().await?;
    fixture.enable_ack().await?;
    fixture.expire(first_id).await?;
    let mut restarted = spawn_worker(&fixture, &fixture.nats_url)?;
    fixture.received(first_id).await?;
    fixture.received(second_id).await?;
    timeout(DEADLINE, async {
        while !fixture.published(second_id).await? {
            tokio::task::yield_now().await;
        }
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
    })
    .await??;
    signal(&restarted, "-TERM").await?;
    assert!(
        timeout(Duration::from_secs(25), restarted.wait())
            .await??
            .success()
    );
    assert!(fixture.published(first_id).await?);
    assert!(fixture.token(first_id).await?.is_none());
    Ok(())
}

#[tokio::test]
async fn another_worker_bypasses_claimed_event_without_borrowing_an_extra_connection() -> TestResult
{
    let mut fixture = Fixture::new(false).await?;
    let first_id = fixture.enqueue().await?;
    let mut relay = AckRelay::new(&fixture.nats_url).await?;
    let first = relay.dispatcher(&fixture).await?;
    let mut tasks = JoinSet::new();
    tasks.spawn(async move { first.dispatch_once().await });
    let ack = relay.held_ack().await?;
    fixture.received(first_id).await?;
    let second_id = fixture.enqueue().await?;
    assert_eq!(
        timeout(DEADLINE, fixture.dispatcher(DEADLINE)?.dispatch_once()).await??,
        OutboxDispatchOutcome::Published {
            event_id: second_id
        }
    );
    fixture.received(second_id).await?;
    assert!(!fixture.published(first_id).await?);
    ack.send(()).map_err(|()| "ACK gate closed")?;
    assert_eq!(
        tasks.join_next().await.ok_or("publisher missing")???,
        OutboxDispatchOutcome::Published { event_id: first_id }
    );
    Ok(())
}

#[tokio::test]
async fn late_ack_converges_only_after_replacement_publishes_identical_snapshot() -> TestResult {
    let mut fixture = Fixture::new(false).await?;
    let id = fixture.enqueue().await?;
    let mut relay = AckRelay::new(&fixture.nats_url).await?;
    let old = relay.dispatcher(&fixture).await?;
    let mut tasks = JoinSet::new();
    tasks.spawn(async move { old.dispatch_once().await });
    let ack = relay.held_ack().await?;
    fixture.received(id).await?;
    fixture.expire(id).await?;
    assert_eq!(
        fixture.dispatcher(DEADLINE)?.dispatch_once().await?,
        OutboxDispatchOutcome::Published { event_id: id }
    );
    fixture.received(id).await?;
    ack.send(()).map_err(|()| "ACK gate closed")?;
    assert_eq!(
        tasks.join_next().await.ok_or("publisher missing")???,
        OutboxDispatchOutcome::Published { event_id: id }
    );
    assert!(fixture.published(id).await?);
    assert!(fixture.token(id).await?.is_none());
    Ok(())
}
