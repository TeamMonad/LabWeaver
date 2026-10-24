//! Gateway integration uses real Agent/Resource PG, JWT/TLS Resource API, and only a stub Anthropic upstream.
#![allow(clippy::too_many_lines, clippy::expect_used)]
#[path = "support/build_billing.rs"]
mod build_billing;
use agent_service::{
    llm_review::LlmReviewStore,
    model_gateway::{GatewayState, ModelGatewayRuntime, ScopedTokenIssuer},
};
use axum::{
    Router,
    body::{Body, Bytes},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use build_billing::BuildBillingFixture;
use contracts::http::{IdempotencyKey, InternalAgentLlmReviewRequest};
use contracts::resource::credits::{CreditAdminOperation, CreditHold, CreditHoldState};
use contracts::resource::{FixedDecimal, Money, ResourceBillingUnit};
use contracts::{
    ActorId, ArtifactId, EventId, FrozenSubmissionId, PlatformRole, PolicyId, ProjectId, TaskRunId,
    UtcTimestamp,
};
use persistence_sqlx::{Domain, MigrationCatalog, Sha256Digest};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};
use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;
use tokio::net::TcpListener;

#[derive(Clone, Copy)]
enum Mode {
    Json,
    Sse,
    Interrupt,
    Disconnect,
    Missing,
    UnknownInterrupt,
}
#[derive(Clone)]
struct Upstream {
    mode: Mode,
    calls: Arc<AtomicUsize>,
}
async fn upstream(
    axum::extract::State(state): axum::extract::State<Upstream>,
    headers: HeaderMap,
) -> Response {
    if headers
        .get("x-api-key")
        .and_then(|value| value.to_str().ok())
        != Some("upstream-private-key")
    {
        return (StatusCode::UNAUTHORIZED, axum::Json(json!({"type":"error","error":{"type":"authentication_error","message":"upstream credential required"}}))).into_response();
    }
    state.calls.fetch_add(1, Ordering::SeqCst);
    let usage = json!({"input_tokens":2,"output_tokens":3,"cache_read_input_tokens":5,"cache_creation_input_tokens":7});
    match state.mode {
        Mode::Json => (
            StatusCode::OK,
            axum::Json(json!({"id":"msg-test","usage":usage})),
        )
            .into_response(),
        Mode::Missing => (
            StatusCode::OK,
            axum::Json(json!({"id":"msg-unknown","content":[]})),
        )
            .into_response(),
        mode => {
            let chunk = if matches!(mode, Mode::UnknownInterrupt) {
                Bytes::from_static(b"event: ping\ndata: {\"type\":\"ping\"}\n\n")
            } else {
                Bytes::from(format!(
                    "event: message_start\ndata: {{\"message\":{{\"usage\":{usage}}}}}\n\n"
                ))
            };
            let stream = futures_util::stream::unfold(0, move |step| {
                let chunk = chunk.clone();
                async move {
                    match step {
                        0 => Some((Ok::<_, std::io::Error>(chunk), 1)),
                        1 if matches!(mode, Mode::Sse) => Some((
                            Ok(Bytes::from_static(
                                b"event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
                            )),
                            2,
                        )),
                        1 if matches!(mode, Mode::Interrupt | Mode::UnknownInterrupt) => {
                            tokio::time::sleep(Duration::from_millis(40)).await;
                            Some((Err(std::io::Error::other("stub upstream disconnect")), 2))
                        }
                        1 if matches!(mode, Mode::Disconnect) => {
                            tokio::time::sleep(Duration::from_secs(30)).await;
                            None
                        }
                        _ => None,
                    }
                }
            });
            (
                [("content-type", "text/event-stream")],
                Body::from_stream(stream),
            )
                .into_response()
        }
    }
}
fn review_request(
    project: ProjectId,
    task: TaskRunId,
    now: UtcTimestamp,
) -> Result<InternalAgentLlmReviewRequest, Box<dyn std::error::Error>> {
    let request: InternalAgentLlmReviewRequest = serde_json::from_value(json!({
        "taskRunId":task,"projectId":project,"courseId":null,"frozenSubmissionId":FrozenSubmissionId::new(),
        "submissionArtifact":{"artifactId":ArtifactId::new(),"storeBinding":"fixture","objectVersion":"1","sizeBytes":10,"mediaType":"text/plain"},
        "policy":{"id":PolicyId::new(),"projectId":project,"courseId":null,"revision":1,"binding":{"runtimeBinding":"claude-code","model":"fixture-model","claudeCodeVersion":"2.1.207","maxInFlightPerWorker":1},
            "budget":{"maxInputTokens":1000,"maxOutputTokens":1000,"maxRequests":8,"maxCostMicrousd":1_000_000,"timeoutMilliseconds":120_000,"maxTransientRetries":0,"maxSchemaRepairs":0},
            "deniedDataClasses":["secret","token","private_key","personally_identifiable_information","unallowlisted_student_submission"],"studentContentMode":"manifest_allowlist_only","activatedAt":now},
        "files":[{"path":"submission.md","sha256":Sha256Digest::of_bytes(b"submission").to_string(),"content":"submission"}],
        "rubric":{"artifact":{"artifactId":ArtifactId::new(),"storeBinding":"fixture","objectVersion":"1","sizeBytes":6,"mediaType":"text/plain"},"path":"rubric.md","sha256":Sha256Digest::of_bytes(b"rubric").to_string(),"content":"rubric"},
        "deadlineAt":UtcTimestamp::from_utc(now.get()+time::Duration::minutes(3))?
    }))?;
    request.validate()?;
    Ok(request)
}
struct Fixture {
    _pg: testcontainers::ContainerAsync<Postgres>,
    billing: BuildBillingFixture,
    pool: sqlx::PgPool,
    runtime: ModelGatewayRuntime,
    project: ProjectId,
    task: TaskRunId,
    attempt: u32,
    calls: Arc<AtomicUsize>,
    gateway: tokio::task::JoinHandle<()>,
    upstream: tokio::task::JoinHandle<()>,
    upstream_url: reqwest::Url,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.gateway.abort();
        self.upstream.abort();
    }
}
impl Fixture {
    async fn new(mode: Mode, funded: bool) -> Result<Self, Box<dyn std::error::Error>> {
        let pg = Postgres::default().with_tag("17.5-alpine").start().await?;
        let pool = PgPoolOptions::new()
            .max_connections(12)
            .connect(&format!(
                "postgres://postgres:postgres@127.0.0.1:{}/postgres",
                pg.get_host_port_ipv4(5432).await?
            ))
            .await?;
        let billing = BuildBillingFixture::start(pool.clone()).await?;
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
        let catalog = MigrationCatalog::load(&root.join("catalog.yaml"))?;
        let mut connection = pool.acquire().await?;
        sqlx::raw_sql("CREATE SCHEMA agent; SET search_path TO agent,pg_catalog")
            .execute(&mut *connection)
            .await?;
        for migration in &catalog
            .domains
            .iter()
            .find(|domain| domain.name == Domain::Agent)
            .ok_or("Agent catalog")?
            .migrations
        {
            sqlx::raw_sql(sqlx::AssertSqlSafe(MigrationCatalog::read_verified_sql(
                &root, migration,
            )?))
            .execute(&mut *connection)
            .await?;
        }
        sqlx::query("RESET search_path")
            .execute(&mut *connection)
            .await?;
        drop(connection);
        let project = ProjectId::new();
        let task = TaskRunId::new();
        let now = billing.store.current_time().await?;
        for (unit, price) in [
            (ResourceBillingUnit::LlmInputToken, "1.000000"),
            (ResourceBillingUnit::LlmOutputToken, "2.000000"),
            (ResourceBillingUnit::LlmCacheReadToken, "3.000000"),
            (ResourceBillingUnit::LlmCacheWriteToken, "4.000000"),
        ] {
            billing
                .store
                .create_rate(
                    &EventId::new().to_string(),
                    &contracts::http::CreateResourceRateRequest {
                        builder_binding: None,
                        model_binding: Some("fixture-model".into()),
                        max_preauthorization: Some(FixedDecimal::parse("10.000000")?),
                        unit,
                        unit_quantity: 1000,
                        gpu_class: None,
                        gpu_mode: None,
                        unit_price: Money {
                            currency: "CREDITS".into(),
                            amount: FixedDecimal::parse(price)?,
                        },
                        effective_from: now,
                        effective_until: None,
                    },
                )
                .await?;
        }
        if funded {
            billing
                .credits
                .admin(
                    "model-fixture-grant",
                    ActorId::new(),
                    &[PlatformRole::PlatformAdmin],
                    &CreditAdminOperation::Grant {
                        project_id: project,
                        amount: FixedDecimal::parse("100.000000")?,
                        reason: "isolated gateway test".into(),
                    },
                )
                .await?;
        }
        let reviews = LlmReviewStore::new(pool.clone());
        reviews
            .enqueue(
                &review_request(project, task, now)?,
                &IdempotencyKey::parse("gateway-review")?,
                now,
            )
            .await?;
        let lease = reviews
            .claim("gateway-fixture", Duration::from_mins(2), now)
            .await?
            .ok_or("review claim")?;
        let calls = Arc::new(AtomicUsize::new(0));
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let upstream_url = format!("http://{}/", listener.local_addr()?);
        let app = Router::new()
            .route("/v1/messages", post(upstream))
            .with_state(Upstream {
                mode,
                calls: calls.clone(),
            });
        let upstream = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let runtime = ModelGatewayRuntime {
            base_url: format!("http://{}/", listener.local_addr()?).parse()?,
            issuer: ScopedTokenIssuer::new(vec![11; 32], Duration::from_mins(2)).ok_or("signer")?,
        };
        let state = GatewayState {
            upstream: upstream_url.parse()?,
            upstream_key: "upstream-private-key".into(),
            issuer: runtime.issuer.clone(),
            pool: pool.clone(),
            resource: billing.client.clone(),
            sequence: Arc::new(AtomicU64::new(0)),
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
        };
        let gateway = tokio::spawn(async move {
            let _ = agent_service::model_gateway::serve(state, listener).await;
        });
        Ok(Self {
            _pg: pg,
            billing,
            pool,
            runtime,
            project,
            task,
            attempt: lease.attempt,
            calls,
            gateway,
            upstream,
            upstream_url: upstream_url.parse()?,
        })
    }
    fn request(&self, key: &str) -> reqwest::RequestBuilder {
        reqwest::Client::new().post(self.runtime.base_url.join("v1/messages").expect("Messages URL"))
            .header("x-api-key",self.runtime.issuer.issue(self.project,self.task.as_uuid(),self.attempt))
            .header("x-labweaver-call-id",key).json(&json!({"model":"fixture-model","max_tokens":32,"messages":[{"role":"user","content":"Hello"}]}))
    }
    async fn holds(&self) -> Result<Vec<CreditHold>, Box<dyn std::error::Error>> {
        // PgCreditStore provides the authoritative decoded hold, not the initial admission receipt.
        Ok(self
            .billing
            .credits
            .holds(self.project, 1, 100, 0)
            .await?
            .items)
    }
    async fn wait_hold(
        &self,
        state: CreditHoldState,
    ) -> Result<CreditHold, Box<dyn std::error::Error>> {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(hold) = self
                    .holds()
                    .await?
                    .into_iter()
                    .find(|hold| hold.state == state)
                {
                    return Ok(hold);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await?
    }
    async fn assert_charged(&self) -> Result<(), Box<dyn std::error::Error>> {
        let hold = self.wait_hold(CreditHoldState::Settled).await?;
        assert_eq!(hold.remaining, FixedDecimal::parse("0.000000")?);
        assert_eq!(
            self.billing.credits.account(self.project).await?.spent,
            FixedDecimal::parse("0.051000")?
        );
        Ok(())
    }
}
#[tokio::test]
async fn nonstream_four_token_dimensions_settle_real_hold() -> Result<(), Box<dyn std::error::Error>>
{
    let f = Fixture::new(Mode::Json, true).await?;
    let response = f.request("json-call").send().await?;
    assert!(response.status().is_success());
    let _ = response.bytes().await?;
    f.assert_charged().await
}
#[tokio::test]
async fn sse_four_token_dimensions_settle_real_hold() -> Result<(), Box<dyn std::error::Error>> {
    let f = Fixture::new(Mode::Sse, true).await?;
    let response = f.request("sse-call").send().await?;
    assert!(response.status().is_success());
    let bytes = response.bytes().await?;
    assert!(String::from_utf8_lossy(&bytes).contains("message_stop"));
    f.assert_charged().await
}
#[tokio::test]
async fn upstream_disconnect_settles_observed_usage() -> Result<(), Box<dyn std::error::Error>> {
    let f = Fixture::new(Mode::Interrupt, true).await?;
    let _ = f.request("interrupted-call").send().await?.bytes().await;
    f.assert_charged().await
}
#[tokio::test]
async fn client_disconnect_settles_observed_usage() -> Result<(), Box<dyn std::error::Error>> {
    let f = Fixture::new(Mode::Disconnect, true).await?;
    let mut response = f.request("cancel-call").send().await?;
    assert!(response.chunk().await?.is_some());
    drop(response);
    f.assert_charged().await
}
#[tokio::test]
async fn missing_usage_keeps_bounded_unknown_hold() -> Result<(), Box<dyn std::error::Error>> {
    let f = Fixture::new(Mode::Missing, true).await?;
    let _ = f.request("unknown-call").send().await?.bytes().await?;
    let hold = f.wait_hold(CreditHoldState::Unknown).await?;
    assert_eq!(hold.remaining, FixedDecimal::parse("10.000000")?);
    assert!(hold.reconciliation_due_at.is_some());
    assert_eq!(
        f.billing.credits.account(f.project).await?.spent,
        FixedDecimal::parse("0.000000")?
    );
    Ok(())
}
#[tokio::test]
async fn rejected_admission_never_forwards_or_creates_hold()
-> Result<(), Box<dyn std::error::Error>> {
    let f = Fixture::new(Mode::Json, false).await?;
    assert_eq!(
        f.request("denied").send().await?.status(),
        StatusCode::PAYMENT_REQUIRED
    );
    assert_eq!(f.calls.load(Ordering::SeqCst), 0);
    assert!(f.holds().await?.is_empty());
    Ok(())
}
#[tokio::test]
async fn repeated_call_is_one_hold_and_one_upstream_request()
-> Result<(), Box<dyn std::error::Error>> {
    let f = Fixture::new(Mode::Json, true).await?;
    let _ = f.request("stable").send().await?.bytes().await?;
    assert!(!f.request("stable").send().await?.status().is_success());
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
    assert_eq!(f.holds().await?.len(), 1);
    f.assert_charged().await
}
#[tokio::test]
async fn scoped_token_rejects_wrong_project_expiry_and_finished_run()
-> Result<(), Box<dyn std::error::Error>> {
    let f = Fixture::new(Mode::Json, true).await?;
    let url = f.runtime.base_url.join("v1/messages")?;
    let wrong = f
        .runtime
        .issuer
        .issue(ProjectId::new(), f.task.as_uuid(), f.attempt);
    assert_eq!(
        reqwest::Client::new()
            .post(url.clone())
            .bearer_auth(wrong)
            .json(&json!({"model":"fixture-model"}))
            .send()
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );
    let issuer = ScopedTokenIssuer::new(vec![11; 32], Duration::from_nanos(1)).ok_or("issuer")?;
    let expired = issuer.issue(f.project, f.task.as_uuid(), f.attempt);
    assert_eq!(
        reqwest::Client::new()
            .post(url)
            .bearer_auth(expired)
            .json(&json!({"model":"fixture-model"}))
            .send()
            .await?
            .status(),
        StatusCode::UNAUTHORIZED
    );
    sqlx::query("UPDATE agent.llm_review_runs SET state='cancelled' WHERE task_run_id=$1")
        .bind(f.task.as_uuid())
        .execute(&f.pool)
        .await?;
    assert_eq!(
        f.request("finished").send().await?.status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(f.calls.load(Ordering::SeqCst), 0);
    assert!(f.holds().await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn upstream_interruption_without_usage_keeps_unknown_hold()
-> Result<(), Box<dyn std::error::Error>> {
    let f = Fixture::new(Mode::UnknownInterrupt, true).await?;
    let _ = f.request("unknown-sse").send().await?.bytes().await;
    let hold = f.wait_hold(CreditHoldState::Unknown).await?;
    assert_eq!(hold.remaining, FixedDecimal::parse("10.000000")?);
    assert!(hold.reconciliation_due_at.is_some());
    assert_eq!(
        f.billing.credits.account(f.project).await?.spent,
        FixedDecimal::parse("0.000000")?
    );
    Ok(())
}

#[tokio::test]
async fn scoped_worker_key_cannot_bypass_gateway_or_create_hold()
-> Result<(), Box<dyn std::error::Error>> {
    let f = Fixture::new(Mode::Json, true).await?;
    let worker = f
        .runtime
        .environment(f.project, f.task.as_uuid(), f.attempt);
    let response = reqwest::Client::new()
        .post(f.upstream_url.join("v1/messages")?)
        .header("x-api-key", &worker["ANTHROPIC_API_KEY"])
        .json(&json!({"model":"fixture-model"}))
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(f.calls.load(Ordering::SeqCst), 0);
    assert!(f.holds().await?.is_empty());
    Ok(())
}
