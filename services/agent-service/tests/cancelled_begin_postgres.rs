//! Cancellation regressions on an isolated `PostgreSQL` and the production TLS/router stack.

use std::{collections::BTreeSet, error::Error, sync::Arc, time::Duration};

use agent_service::service_runtime;
use agent_service::{
    api::{AgentApiState, CONTROL_PERMISSION, router},
    build_store::PgBuildStore,
    generated_artifacts::GeneratedArtifactStore,
    llm_review::LlmReviewStore,
    platform_image_jobs::PlatformImageImportJobStore,
    platform_images::PgPlatformImageCatalog,
    run_store::PostgresAgentRunStore,
};
use axum::{extract::Request, middleware::Next, response::Response};
use contracts::http::{
    InternalPlatformImageImportEnqueueRequest, InternalPlatformImageImportRequest,
    PlatformImageImportJobState, PlatformImageKind,
};
use contracts::{ActorId, ArtifactId, ArtifactRef, UploadSessionId, UtcTimestamp};
use futures_util::FutureExt;
use sqlx::{
    Connection, PgPool,
    postgres::{PgConnectOptions, PgPoolOptions, PgSslMode},
};
use testcontainers::{ContainerAsync, ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{Mutex, oneshot},
};
use tokio_util::task::AbortOnDropHandle;

#[path = "../../http_transport.rs"]
#[allow(
    dead_code,
    reason = "this integration fixture uses the production TLS serving entrypoint"
)]
mod http_transport;
#[path = "support/pg_response_gate.rs"]
mod pg_response_gate;
mod support;

type TestResult = Result<(), Box<dyn Error>>;
const LIMIT: Duration = Duration::from_secs(15);

struct Fixture {
    writer: PgPool,
    observer: PgPool,
    gate: pg_response_gate::PgResponseGate,
    _postgres: ContainerAsync<Postgres>,
}

impl Fixture {
    async fn start() -> Result<Self, Box<dyn Error>> {
        let postgres = Postgres::default().with_tag("17.5-alpine").start().await?;
        let port = postgres.get_host_port_ipv4(5432).await?;
        let options = PgConnectOptions::new()
            .host("127.0.0.1")
            .port(port)
            .username("postgres")
            .password("postgres")
            .database("postgres")
            .ssl_mode(PgSslMode::Disable);
        let observer = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options.clone())
            .await?;
        support::apply_agent_migrations(&observer).await?;
        sqlx::query("CREATE TABLE public.begin_markers (tag text PRIMARY KEY)")
            .execute(&observer)
            .await?;
        let gate = pg_response_gate::PgResponseGate::start(port).await?;
        let writer = PgPoolOptions::new()
            .max_connections(1)
            .min_connections(1)
            .connect_with(options.port(gate.port))
            .await?;
        Ok(Self {
            writer,
            observer,
            gate,
            _postgres: postgres,
        })
    }

    async fn same_idle_connection(&self, pid: i32) -> TestResult {
        let mut connection = tokio::time::timeout(LIMIT, self.writer.acquire()).await??;
        assert_eq!(
            sqlx::query_scalar::<_, i32>("SELECT pg_backend_pid()")
                .fetch_one(&mut *connection)
                .await?,
            pid,
            "the test must reuse the cancelled connection"
        );
        let state: String = sqlx::query_scalar("SELECT state FROM pg_stat_activity WHERE pid=$1")
            .bind(pid)
            .fetch_one(&self.observer)
            .await?;
        assert_eq!(
            state, "idle",
            "reused connection still has a server transaction"
        );
        assert!(!connection.is_in_transaction());
        Ok(())
    }
}

fn now() -> Result<UtcTimestamp, Box<dyn Error>> {
    let time = time::OffsetDateTime::now_utc();
    Ok(UtcTimestamp::from_utc(time.replace_nanosecond(
        time.nanosecond() / 1_000_000 * 1_000_000,
    )?)?)
}

fn import_request() -> InternalPlatformImageImportEnqueueRequest {
    InternalPlatformImageImportEnqueueRequest {
        upload_id: UploadSessionId::new(),
        request: InternalPlatformImageImportRequest {
            kind: PlatformImageKind::Container,
            binding: "cancelled-begin".to_owned(),
            target_reference: "registry.test/platform/cancelled-begin:v1".to_owned(),
            archive: ArtifactRef {
                artifact_id: ArtifactId::new(),
                store_binding: "test-object-store".to_owned(),
                object_version: "version-1".to_owned(),
                size_bytes: 1,
                media_type: contracts::http::PLATFORM_IMAGE_ARCHIVE_MEDIA_TYPE.to_owned(),
            },
            archive_object_key: "fixture/archive.tar".to_owned(),
            disk_format: None,
            disk_path: None,
            capacity_bytes: None,
            trust_revision: 1,
            actor_id: ActorId::new(),
            reason: "fixture reviewed import".to_owned(),
        },
    }
}

async fn cancel_begin(
    fixture: &Fixture,
    statement: Option<&'static str>,
    server_status: u8,
) -> TestResult {
    let query = statement.unwrap_or("BEGIN");
    let (ready, release) = fixture.gate.arm(query).await;
    let pool = fixture.writer.clone();
    let task = tokio::spawn(async move {
        if let Some(statement) = statement {
            pool.begin_with(statement).await
        } else {
            pool.begin().await
        }
    });
    assert_eq!(tokio::time::timeout(LIMIT, ready).await??, server_status);
    task.abort();
    assert!(task.await.is_err_and(|error| error.is_cancelled()));
    release.send(()).map_err(|()| "response gate stopped")?;
    Ok(())
}

#[tokio::test]
async fn cancelled_begin_keeps_business_failure_durable_on_reuse_and_pool_close() -> TestResult {
    let fixture = Fixture::start().await?;
    let observer_jobs = PlatformImageImportJobStore::new(fixture.observer.clone());
    let request = import_request();
    observer_jobs.enqueue(&request, now()?).await?;
    let claim = observer_jobs
        .claim("fixture-owner", Duration::from_mins(1), now()?)
        .await?
        .ok_or("fixture job not claimed")?;
    // Preserve the multi-statement witness, with a protocol gate instead of a timing sleep.
    cancel_begin(&fixture, Some("BEGIN; SELECT 1"), b'T').await?;
    let writer_jobs = PlatformImageImportJobStore::new(fixture.writer.clone());
    writer_jobs
        .fail(
            claim.upload_id,
            claim.lease_token,
            "FIXTURE_FAILURE",
            now()?,
        )
        .await?;
    let local = writer_jobs.status(request.upload_id).await?;
    let durable = observer_jobs.status(request.upload_id).await?;
    fixture.writer.close().await;
    let after_close = observer_jobs.status(request.upload_id).await?;
    eprintln!(
        "cancelled_begin_witness local_state={:?} local_revision={} observer_state={:?} observer_revision={} after_close_state={:?} after_close_revision={}",
        local.state,
        local.revision.get(),
        durable.state,
        durable.revision.get(),
        after_close.state,
        after_close.revision.get(),
    );
    assert_eq!(local.state, PlatformImageImportJobState::Failed);
    assert_eq!(
        durable, local,
        "fail returned Ok but the independent observer missed the write"
    );
    assert_eq!(durable.revision.get(), 3);
    assert_eq!(after_close, durable);
    assert!(
        PgPlatformImageCatalog::new(fixture.observer.clone())
            .list(None)
            .await?
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn cancelled_plain_begin_returns_the_same_idle_connection_and_commits_new_writes()
-> TestResult {
    let fixture = Fixture::start().await?;
    let pid = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&fixture.writer)
        .await?;
    cancel_begin(&fixture, None, b'T').await?;
    fixture.same_idle_connection(pid).await?;
    sqlx::query("INSERT INTO public.begin_markers VALUES ('after-cancel')")
        .execute(&fixture.writer)
        .await?;
    fixture.writer.close().await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM public.begin_markers")
            .fetch_one(&fixture.observer)
            .await?,
        1
    );
    Ok(())
}

#[expect(
    clippy::disallowed_methods,
    reason = "this test intentionally covers fresh and nested transaction boundaries"
)]
#[tokio::test]
async fn nested_begin_is_rejected_until_upstream_4445_is_fixed() -> TestResult {
    let fixture = Fixture::start().await?;
    let mut connection = fixture.writer.acquire().await?;
    let mut outer = connection.begin().await?;
    sqlx::query("INSERT INTO public.begin_markers VALUES ('outer')")
        .execute(&mut *outer)
        .await?;

    // Production source uses clippy.toml to reject nested transaction methods.
    // This test intentionally exercises the boundary and documents SQLx #4445.
    assert!(matches!(
        (*outer).begin_with("BEGIN").await,
        Err(sqlx::Error::InvalidSavePointStatement)
    ));
    outer.rollback().await?;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM public.begin_markers")
            .fetch_one(&fixture.observer)
            .await?,
        0
    );
    Ok(())
}

#[expect(
    clippy::disallowed_methods,
    reason = "this test intentionally covers fresh and nested transaction boundaries"
)]
#[tokio::test]
async fn failed_begin_normal_commit_and_cancellation_boundaries_remain_usable() -> TestResult {
    let fixture = Fixture::start().await?;
    // Dropping a never-polled future must not send BEGIN or alter pool state.
    drop(fixture.writer.begin());
    // Cancellation while waiting for the single pool slot must not claim a transaction.
    let connection = fixture.writer.acquire().await?;
    assert!(fixture.writer.begin().now_or_never().is_none());
    drop(connection);
    assert!(fixture.writer.begin_with("BEGIN INVALID").await.is_err());
    assert!(matches!(
        fixture.writer.begin_with("SELECT 1").await,
        Err(sqlx::Error::BeginFailed)
    ));
    let mut transaction = fixture.writer.begin().await?;
    sqlx::query("INSERT INTO public.begin_markers VALUES ('committed')")
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    let mut transaction = fixture.writer.begin().await?;
    {
        let mut nested = (*transaction).begin().await?;
        sqlx::query("INSERT INTO public.begin_markers VALUES ('savepoint')")
            .execute(&mut *nested)
            .await?;
        nested.rollback().await?;
    }
    sqlx::query("INSERT INTO public.begin_markers VALUES ('dropped')")
        .execute(&mut *transaction)
        .await?;
    drop(transaction);
    let pid = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&fixture.writer)
        .await?;
    fixture.same_idle_connection(pid).await?;
    fixture.writer.close().await;
    assert_eq!(
        sqlx::query_scalar::<_, Vec<String>>(
            "SELECT ARRAY_AGG(tag ORDER BY tag) FROM public.begin_markers"
        )
        .fetch_one(&fixture.observer)
        .await?,
        ["committed"]
    );
    Ok(())
}

#[expect(
    clippy::disallowed_methods,
    reason = "this test intentionally covers fresh and nested transaction boundaries"
)]
#[tokio::test]
async fn cancelled_aborted_begin_and_failed_savepoint_do_not_commit_partial_writes() -> TestResult {
    let fixture = Fixture::start().await?;
    cancel_begin(&fixture, Some("BEGIN; SELECT 1/0"), b'E').await?;
    let mut connection = fixture.writer.acquire().await?;
    let mut outer = connection.begin().await?;
    sqlx::query("INSERT INTO public.begin_markers VALUES ('before-error')")
        .execute(&mut *outer)
        .await?;
    assert!(
        sqlx::query("SELECT 1/0")
            .execute(&mut *outer)
            .await
            .is_err()
    );
    assert!((*outer).begin().await.is_err());
    // A failed savepoint must leave the outer transaction aborted, not start autocommit.
    assert!(
        sqlx::query("INSERT INTO public.begin_markers VALUES ('after-error')")
            .execute(&mut *outer)
            .await
            .is_err()
    );
    outer.rollback().await?;
    drop(connection);
    fixture.writer.close().await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM public.begin_markers")
            .fetch_one(&fixture.observer)
            .await?,
        0
    );
    Ok(())
}

struct HandlerDrop(Option<oneshot::Sender<()>>);
impl Drop for HandlerDrop {
    fn drop(&mut self) {
        if let Some(signal) = self.0.take() {
            let _ = signal.send(());
        }
    }
}

async fn track_handler(
    axum::extract::State(signal): axum::extract::State<Arc<Mutex<Option<oneshot::Sender<()>>>>>,
    request: Request,
    next: Next,
) -> Response {
    let _guard = HandlerDrop(signal.lock().await.take());
    next.run(request).await
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one real TLS/h2 stream verifies cancellation and independent durable business state"
)]
async fn tls_h2_rst_drops_cancel_handler_during_plain_begin_without_poisoning_pool() -> TestResult {
    let fixture = Fixture::start().await?;
    let observer_jobs = PlatformImageImportJobStore::new(fixture.observer.clone());
    let request = import_request();
    observer_jobs.enqueue(&request, now()?).await?;
    let claim = observer_jobs
        .claim("h2-owner", Duration::from_mins(1), now()?)
        .await?
        .ok_or("fixture job not claimed")?;
    let writer_jobs = PlatformImageImportJobStore::new(fixture.writer.clone());
    let pid = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&fixture.writer)
        .await?;
    let state = Arc::new(AgentApiState {
        store: PostgresAgentRunStore::new(fixture.writer.clone()),
        build_store: PgBuildStore::new(fixture.writer.clone()),
        generated_artifacts: GeneratedArtifactStore::new(fixture.writer.clone()),
        llm_reviews: LlmReviewStore::new(fixture.writer.clone()),
        platform_images: PgPlatformImageCatalog::new(fixture.writer.clone()),
        platform_registry: None,
        platform_image_import_jobs: writer_jobs.clone(),
        resource: None,
        objects: Arc::new(support::FakeObjects::new(Vec::new())),
    });
    let identity = auth::ServiceIdentity {
        issuer: "https://fixture.test".to_owned(),
        subject: "control".to_owned(),
        client_id: "labweaver-control".to_owned(),
        expires_at: time::OffsetDateTime::now_utc() + time::Duration::hours(1),
        permissions: BTreeSet::from([CONTROL_PERMISSION.to_owned()]),
    };
    let (dropped_tx, dropped_rx) = oneshot::channel();
    let app =
        router(state)
            .layer(axum::Extension(identity))
            .layer(axum::middleware::from_fn_with_state(
                Arc::new(Mutex::new(Some(dropped_tx))),
                track_handler,
            ));
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()])?;
    let tls = http_transport::server_config(
        certificate.cert.pem().as_bytes(),
        certificate.signing_key.serialize_pem().as_bytes(),
    )?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let _server =
        AbortOnDropHandle::new(tokio::spawn(http_transport::serve_tls(listener, app, tls)));
    let mut roots = rustls::RootCertStore::empty();
    roots.add(certificate.cert.der().clone())?;
    let mut client_tls = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    client_tls.alpn_protocols = vec![b"h2".to_vec()];
    let stream = tokio_rustls::TlsConnector::from(Arc::new(client_tls))
        .connect(
            rustls::pki_types::ServerName::try_from("localhost")?,
            TcpStream::connect(address).await?,
        )
        .await?;
    assert_eq!(stream.get_ref().1.alpn_protocol(), Some(b"h2".as_slice()));
    let (mut client, connection) = h2::client::handshake(stream).await?;
    let _connection = AbortOnDropHandle::new(tokio::spawn(connection));
    let body = serde_json::to_vec(&serde_json::json!({"uploadId": request.upload_id}))?;
    let message = http::Request::builder()
        .method("POST")
        .uri(format!(
            "https://localhost:{}/internal/v1/platform-images/import-jobs/{}/cancel",
            address.port(),
            request.upload_id
        ))
        .header("content-type", "application/json")
        .header("idempotency-key", "fixture-h2-cancel")
        .body(())?;
    let (ready, release) = fixture.gate.arm("BEGIN").await;
    let (_response, mut stream) = client.send_request(message, false)?;
    stream.send_data(body.into(), true)?;
    assert_eq!(
        tokio::time::timeout(LIMIT, ready).await??,
        b'T',
        "ordinary BEGIN must reach PostgreSQL before reset"
    );
    stream.send_reset(h2::Reason::CANCEL);
    tokio::time::timeout(LIMIT, dropped_rx).await??;
    release.send(()).map_err(|()| "response gate stopped")?;
    fixture.same_idle_connection(pid).await?;
    assert_eq!(
        observer_jobs
            .status(request.upload_id)
            .await?
            .revision
            .get(),
        2,
        "abandoned cancellation must not change the job"
    );
    writer_jobs
        .fail(
            claim.upload_id,
            claim.lease_token,
            "FIXTURE_H2_FAILURE",
            now()?,
        )
        .await?;
    let durable = observer_jobs.status(request.upload_id).await?;
    assert_eq!(durable.state, PlatformImageImportJobState::Failed);
    assert_eq!(durable.revision.get(), 3);
    assert_eq!(writer_jobs.status(request.upload_id).await?, durable);
    fixture.writer.close().await;
    assert_eq!(observer_jobs.status(request.upload_id).await?, durable);
    Ok(())
}
