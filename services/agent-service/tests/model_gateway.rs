//! Gateway integration uses real Agent/Resource PG, JWT/TLS Resource API, and only a stub Anthropic upstream.
#![allow(clippy::too_many_lines, clippy::expect_used)]
#[path = "support/build_billing.rs"]
mod build_billing;
#[path = "support/model_authorization.rs"]
mod model_authorization;
use agent_service::model_gateway::{GatewayState, ModelGatewayRuntime, ScopedTokenIssuer};
use axum::{
    Router,
    body::{Body, Bytes},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use build_billing::BuildBillingFixture;
use contracts::ProjectId;
use contracts::resource::FixedDecimal;
use contracts::resource::credits::{CreditHold, CreditHoldState};
use model_authorization::ModelAuthorizationFixture;
use persistence_sqlx::{Domain, MigrationCatalog};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
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
            axum::Json(
                json!({"id":"msg-test","type":"message","stop_reason":"end_turn","usage":usage}),
            ),
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
                Bytes::from_static(b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"type\":\"message\",\"stop_reason\":null,\"usage\":{\"input_tokens\":2,\"output_tokens\":0,\"cache_read_input_tokens\":5,\"cache_creation_input_tokens\":7}}}\n\n")
            };
            let stream = futures_util::stream::unfold(0, move |step| {
                let chunk = chunk.clone();
                async move {
                    match step {
                        0 => Some((Ok::<_, std::io::Error>(chunk), 1)),
                        1 if matches!(mode, Mode::Sse) => Some((Ok(Bytes::from_static(b"event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":3}}\n\n")), 2)),
                        2 if matches!(mode, Mode::Sse) => Some((Ok(Bytes::from_static(b"event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n")), 3)),
                        1 if matches!(mode, Mode::Interrupt | Mode::Disconnect) => Some((Ok(Bytes::from_static(b"event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":null},\"usage\":{\"output_tokens\":1}}\n\n")), 2)),
                        1 if matches!(mode, Mode::UnknownInterrupt) => Some((Err(std::io::Error::other("stub upstream disconnect without usage")), 3)),
                        2 if matches!(mode, Mode::Interrupt) => {
                            tokio::time::sleep(Duration::from_millis(40)).await;
                            Some((Err(std::io::Error::other("stub upstream disconnect")), 3))
                        }
                        2 if matches!(mode, Mode::Disconnect) => {
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
struct Fixture {
    _pg: testcontainers::ContainerAsync<Postgres>,
    billing: BuildBillingFixture,
    pool: sqlx::PgPool,
    runtime: ModelGatewayRuntime,
    project: ProjectId,
    run_id: contracts::AgentRunId,
    attempt: u32,
    capability: String,
    calls: Arc<AtomicUsize>,
    gateway: tokio::task::JoinHandle<()>,
    upstream: tokio::task::JoinHandle<()>,
    upstream_url: reqwest::Url,
    authority: ModelAuthorizationFixture,
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
        let authority = ModelAuthorizationFixture::start(pool.clone(), &billing).await?;
        let project = authority.project;
        billing
            .model_tariffs(project, "fixture-model", funded)
            .await?;
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
            issuer: ScopedTokenIssuer::new(vec![11; 32], Duration::from_hours(24))
                .ok_or("signer")?,
        };
        let state = GatewayState {
            upstream: upstream_url.parse()?,
            upstream_key: "upstream-private-key".into(),
            issuer: runtime.issuer.clone(),
            pool: pool.clone(),
            resource: billing.client.clone(),
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            authorization: authority.authorization.clone(),
        };
        let gateway = tokio::spawn(async move {
            let _ = agent_service::model_gateway::serve(state, listener).await;
        });
        let capability = runtime.issuer.issue_for(
            project,
            authority.run_id.as_uuid(),
            authority.attempt,
            authority.scope.clone(),
            Duration::from_hours(1),
        );
        Ok(Self {
            _pg: pg,
            billing,
            pool,
            runtime,
            project,
            run_id: authority.run_id,
            attempt: authority.attempt,
            capability,
            calls,
            gateway,
            upstream,
            upstream_url: upstream_url.parse()?,
            authority,
        })
    }
    fn request(&self, key: &str) -> reqwest::RequestBuilder {
        reqwest::Client::new().post(self.runtime.base_url.join("v1/messages").expect("Messages URL"))
            .header("x-api-key", &self.capability)
            .header("x-labweaver-call-id",key).json(&json!({"model":"fixture-model","max_tokens":32,"messages":[{"role":"user","content":"Hello"}]}))
    }
    fn sdk_request(&self, retry: u32) -> reqwest::RequestBuilder {
        reqwest::Client::new().post(self.runtime.base_url.join("v1/messages").expect("Messages URL"))
            .header("x-api-key", &self.capability).header("x-stainless-retry-count", retry)
            .json(&json!({"model":"fixture-model","max_tokens":32,"messages":[{"role":"user","content":"Hello"}]}))
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
        let value: serde_json::Value = sqlx::query_scalar("SELECT contract FROM resource.model_cost_observations WHERE hold_id=$1 AND contract->'measurement'->>'state'='known'")
            .bind(hold.id).fetch_one(&self.pool).await?;
        let fact: contracts::resource::model_costing::ModelCostObservation =
            serde_json::from_value(value)?;
        assert_eq!(fact.lines.len(), 4);
        assert_eq!(fact.charged_amount, FixedDecimal::parse("0.051000")?);
        assert_eq!(
            fact.observed_total.ok_or("known observation cost")?.amount,
            fact.charged_amount
        );
        Ok(())
    }
    async fn assert_partial_unknown(&self) -> Result<(), Box<dyn std::error::Error>> {
        let hold = self.wait_hold(CreditHoldState::Unknown).await?;
        let value: serde_json::Value = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let value = sqlx::query_scalar("SELECT contract FROM resource.model_cost_observations WHERE hold_id=$1 AND contract->'measurement'->'observedQuantities' IS NOT NULL ORDER BY created_at DESC LIMIT 1")
                    .bind(hold.id).fetch_optional(&self.pool).await?;
                if let Some(value) = value { return Ok::<_, sqlx::Error>(value); }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await??;
        let fact: contracts::resource::model_costing::ModelCostObservation =
            serde_json::from_value(value)?;
        let contracts::resource::model_costing::ModelUsageMeasurement::Unknown {
            observed_quantities: Some(usage),
            ..
        } = fact.measurement
        else {
            return Err("partial SSE usage was incorrectly finalized".into());
        };
        assert_eq!(usage.input_tokens, 2);
        assert_eq!(usage.output_tokens, 1);
        assert_eq!(usage.cache_read_tokens, 5);
        assert_eq!(usage.cache_write_tokens, 7);
        assert_eq!(fact.charged_amount, FixedDecimal::parse("0.000000")?);
        assert_eq!(hold.remaining, FixedDecimal::parse("10.000000")?);
        let account = self.billing.credits.account(self.project).await?;
        assert_eq!(account.spent, FixedDecimal::parse("0.000000")?);
        assert_eq!(account.reserved, FixedDecimal::parse("10.000000")?);
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
async fn upstream_disconnect_keeps_unknown_and_persists_partial_usage()
-> Result<(), Box<dyn std::error::Error>> {
    let f = Fixture::new(Mode::Interrupt, true).await?;
    let _ = f.request("interrupted-call").send().await?.bytes().await;
    f.assert_partial_unknown().await
}
#[tokio::test]
async fn client_disconnect_keeps_unknown_and_persists_partial_usage()
-> Result<(), Box<dyn std::error::Error>> {
    let f = Fixture::new(Mode::Disconnect, true).await?;
    let mut response = f.request("cancel-call").send().await?;
    let mut observed = Vec::new();
    while !String::from_utf8_lossy(&observed).contains("message_delta") {
        observed.extend_from_slice(&response.chunk().await?.ok_or("missing delta")?);
    }
    drop(response);
    f.assert_partial_unknown().await
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
    let wrong = f.runtime.issuer.issue_for(
        ProjectId::new(),
        f.run_id.as_uuid(),
        f.attempt,
        f.authority.scope.clone(),
        Duration::from_hours(1),
    );
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
    let expired = issuer.issue_for(
        f.project,
        f.run_id.as_uuid(),
        f.attempt,
        f.authority.scope.clone(),
        Duration::from_secs(1),
    );
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
    sqlx::query("UPDATE agent.agent_runs SET state='cancelled',cancellation_requested_at=clock_timestamp() WHERE run_id=$1")
        .bind(f.run_id.as_uuid())
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
    let worker = f.runtime.environment(
        f.project,
        f.run_id.as_uuid(),
        f.attempt,
        f.authority.scope.clone(),
        Duration::from_hours(1),
    );
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

#[tokio::test]
async fn bare_requests_without_stable_sdk_metadata_are_refused()
-> Result<(), Box<dyn std::error::Error>> {
    let f = Fixture::new(Mode::Json, true).await?;
    let response = reqwest::Client::new()
        .post(f.runtime.base_url.join("v1/messages")?)
        .header("x-api-key", &f.capability)
        .json(&json!({"model":"fixture-model"}))
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(f.calls.load(Ordering::SeqCst), 0);
    assert!(f.holds().await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn headerless_sdk_retries_keep_original_call_and_identical_fresh_calls_stay_distinct()
-> Result<(), Box<dyn std::error::Error>> {
    let f = Fixture::new(Mode::Json, true).await?;
    let _ = f.sdk_request(0).send().await?.bytes().await?;
    assert_eq!(
        f.sdk_request(1).send().await?.status(),
        StatusCode::CONFLICT
    );
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
    assert_eq!(f.holds().await?.len(), 1);
    let _ = f.sdk_request(0).send().await?.bytes().await?;
    assert_eq!(
        f.holds().await?.len(),
        2,
        "identical bodies are distinct original SDK requests"
    );
    assert_eq!(
        f.sdk_request(1).send().await?.status(),
        StatusCode::CONFLICT,
        "ambiguous retry cannot identify either prior call safely"
    );
    assert_eq!(f.calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        f.billing.credits.account(f.project).await?.spent,
        FixedDecimal::parse("0.102000")?
    );
    Ok(())
}

#[tokio::test]
async fn cli_process_headerless_response_loss_never_replays_upstream()
-> Result<(), Box<dyn std::error::Error>> {
    let f = Fixture::new(Mode::Disconnect, true).await?;
    let environment = f.runtime.environment(
        f.project,
        f.run_id.as_uuid(),
        f.attempt,
        f.authority.scope.clone(),
        Duration::from_hours(1),
    );
    let script = r#"
import http.client, json, os, urllib.parse
url=urllib.parse.urlsplit(os.environ['ANTHROPIC_BASE_URL'])
headers={'x-api-key':os.environ['ANTHROPIC_API_KEY'],'content-type':'application/json','x-stainless-retry-count':'0'}
body=json.dumps({'model':'fixture-model','max_tokens':32,'messages':[{'role':'user','content':'Hello'}]})
connection=http.client.HTTPConnection(url.hostname,url.port,timeout=5)
connection.request('POST','/v1/messages',body,headers)
response=connection.getresponse()
assert response.status==200
while True:
    line=response.readline()
    assert line, 'stream ended before partial delta'
    if b'"output_tokens":1' in line: break
response.close(); connection.close()
headers['x-stainless-retry-count']='1'
connection=http.client.HTTPConnection(url.hostname,url.port,timeout=5)
connection.request('POST','/v1/messages',body,headers)
response=connection.getresponse()
assert response.status==409, response.read()
response.close(); connection.close()
"#;
    let output = tokio::process::Command::new("/usr/bin/python3")
        .args(["-c", script])
        .env_clear()
        .envs(environment)
        .output()
        .await?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    f.assert_partial_unknown().await?;
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
    assert_eq!(f.holds().await?.len(), 1);
    Ok(())
}

#[tokio::test]
async fn sdk_retry_after_gateway_restart_retains_original_persisted_call_fence()
-> Result<(), Box<dyn std::error::Error>> {
    let mut fixture = Fixture::new(Mode::Json, true).await?;
    let _ = fixture.sdk_request(0).send().await?.bytes().await?;
    fixture.gateway.abort();
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    fixture.runtime.base_url = format!("http://{}/", listener.local_addr()?).parse()?;
    let state = GatewayState {
        upstream: fixture.upstream_url.clone(),
        upstream_key: "upstream-private-key".into(),
        issuer: fixture.runtime.issuer.clone(),
        pool: fixture.pool.clone(),
        resource: fixture.billing.client.clone(),
        http: reqwest::Client::new(),
        authorization: fixture.authority.authorization.clone(),
    };
    fixture.gateway = tokio::spawn(async move {
        let _ = agent_service::model_gateway::serve(state, listener).await;
    });
    assert_eq!(
        fixture.sdk_request(1).send().await?.status(),
        StatusCode::CONFLICT
    );
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.holds().await?.len(), 1);
    fixture.assert_charged().await
}
