//! Real Control/Agent/Resource `PostgreSQL` and JWT/TLS APIs; only the model upstream is captured.
use super::{build_billing, model_authorization};

use agent_service::model_gateway::{GatewayState, ModelGatewayRuntime, ScopedTokenIssuer};
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::State,
    http::{HeaderMap, Request, StatusCode},
    routing::post,
};
use build_billing::BuildBillingFixture;
use contracts::{
    ProjectId,
    resource::{FixedDecimal, credits::CreditHoldState},
};
use model_authorization::{ModelAuthorizationFixture, apply};
use persistence_sqlx::Domain;
use serde_json::{Value, json};
use sqlx::postgres::PgPoolOptions;
use std::{sync::Arc, time::Duration};
use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;
use tower::ServiceExt;

type Result<T, E = Box<dyn std::error::Error>> = std::result::Result<T, E>;
async fn captured_provider(
    State(captured): State<Arc<tokio::sync::Mutex<Vec<Value>>>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, StatusCode> {
    if headers
        .get("x-api-key")
        .and_then(|value| value.to_str().ok())
        != Some("provider-secret-kept-in-agent")
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    captured.lock().await.push(body);
    Ok(Json(
        json!({"id":"provider-fixture","type":"message","stop_reason":"end_turn","content":[],"usage":{"input_tokens":2,"output_tokens":3,"cache_read_input_tokens":5,"cache_creation_input_tokens":7}}),
    ))
}
fn model_body(content: &str) -> Value {
    json!({"model":"fixture-model","max_tokens":20,"messages":[{"role":"user","content":content}]})
}
async fn send_raw(app: &Router, capability: &str, call: &str, body: String) -> Result<StatusCode> {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/messages")
                .header("x-api-key", capability)
                .header("x-labweaver-call-id", call)
                .header("content-type", "application/json")
                .body(Body::from(body))?,
        )
        .await?;
    let status = response.status();
    let _ = to_bytes(response.into_body(), 16 * 1024 * 1024).await?;
    Ok(status)
}
async fn send_model(
    app: &Router,
    capability: &str,
    call: &str,
    content: &str,
) -> Result<StatusCode> {
    send_raw(
        app,
        capability,
        call,
        serde_json::to_string(&model_body(content))?,
    )
    .await
}
async fn send_tool(app: &Router, capability: &str) -> Result<StatusCode> {
    Ok(app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/authorize")
                .header("x-api-key", capability)
                .body(Body::from(serde_json::to_vec(
                    &json!({"tool_name":"Read","tool_input":{"file_path":"/materials/README.md"}}),
                )?))?,
        )
        .await?
        .status())
}
async fn settled(billing: &BuildBillingFixture, project: ProjectId, count: usize) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let holds = billing.credits.holds(project, 1, 100, 0).await?.items;
            if holds.len() == count
                && holds
                    .iter()
                    .all(|hold| hold.state == CreditHoldState::Settled)
            {
                return Ok::<_, Box<dyn std::error::Error>>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one native authority and wallet fixture proves each new action uses the original identity"
)]
async fn sandbox_tools_and_each_model_request_recheck_original_authority() -> Result<()> {
    let postgres = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(12)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            postgres.get_host_port_ipv4(5432).await?
        ))
        .await?;
    let billing = BuildBillingFixture::start(pool.clone()).await?;
    apply(&pool, Domain::Agent).await?;
    let authority = ModelAuthorizationFixture::start(pool.clone(), &billing).await?;
    billing
        .model_tariffs(authority.project, "fixture-model", true)
        .await?;
    let captured = Arc::new(tokio::sync::Mutex::new(Vec::<Value>::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let upstream_url = format!("http://{}/", listener.local_addr()?).parse()?;
    let provider = Router::new()
        .route("/v1/messages", post(captured_provider))
        .with_state(captured.clone());
    let upstream = tokio::spawn(async move {
        let _ = axum::serve(listener, provider).await;
    });
    let runtime = ModelGatewayRuntime {
        base_url: "http://agent-service.labweaver-system.svc:9460/".parse()?,
        issuer: ScopedTokenIssuer::new(vec![11; 32], Duration::from_hours(24)).ok_or("signer")?,
    };
    let capability = runtime.issuer.issue_for(
        authority.project,
        authority.run_id.as_uuid(),
        authority.attempt,
        authority.scope.clone(),
        Duration::from_mins(2),
    );
    let app = GatewayState {
        background: agent_service::service_runtime::BackgroundTasks::default(),
        upstream: upstream_url,
        upstream_key: "provider-secret-kept-in-agent".to_owned(),
        issuer: runtime.issuer.clone(),
        pool: pool.clone(),
        resource: billing.client.clone(),
        http: reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()?,
        authorization: authority.authorization.clone(),
    }
    .router();
    assert_eq!(send_tool(&app, &capability).await?, StatusCode::NO_CONTENT);
    for tool in [
        json!({"tool_name":"Write","tool_input":{"content":"-----BEGIN PRIVATE KEY-----"}}),
        json!({"tool_name":"Read","tool_response":{"AKIAIOSFODNN7EXAMPLE":"harmless-value"}}),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/authorize")
                    .header("x-api-key", &capability)
                    .body(Body::from(serde_json::to_vec(&tool)?))?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(captured.lock().await.is_empty());
        assert!(
            billing
                .credits
                .holds(authority.project, 1, 100, 0)
                .await?
                .items
                .is_empty()
        );
    }
    for secret in [
        "-----BEGIN PRIVATE KEY-----",
        "AKIAIOSFODNN7EXAMPLE",
        "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJzZWNyZXQifQ.signaturemarker",
    ] {
        let input = json!({(secret):"harmless-value"});
        let plain = serde_json::to_string(
            &json!({"model":"fixture-model","messages":[{"role":"assistant","content":[{"type":"tool_use","id":"read-fixture","name":"Read","input":input}]}]}),
        )?;
        let encoded = serde_json::to_string(secret)?;
        let escaped = format!("\"\\u{:04x}{}", secret.as_bytes()[0], &encoded[2..]);
        let unicode = plain.replacen(&encoded, &escaped, 1);
        let nested = serde_json::to_string(
            &json!({"model":"fixture-model","messages":[{"role":"assistant","content":[{"type":"tool_use","id":"read-fixture","name":"Read","input":{"nested":[{"deeper":{(secret):"harmless-value"}}]}}]}]}),
        )?;
        for body in [plain, unicode, nested] {
            assert_eq!(
                send_raw(&app, &capability, "denied-key", body).await?,
                StatusCode::FORBIDDEN
            );
            assert!(captured.lock().await.is_empty());
        }
    }
    for secret in [
        "-----BEGIN PRIVATE KEY-----\nsensitive-marker",
        capability.as_str(),
        "provider-secret-kept-in-agent",
    ] {
        assert_eq!(
            send_model(&app, &capability, "denied-value", secret).await?,
            StatusCode::FORBIDDEN
        );
    }
    let escaped = format!("\\u{:04x}{}", capability.as_bytes()[0], &capability[1..]);
    for body in [
        format!(
            r#"{{"model":"fixture-model","messages":[{{"role":"user","content":"{escaped}"}}]}}"#
        ),
        format!(
            r#"{{"model":"fixture-model","messages":[],"metadata":{{"{escaped}":"no-leak"}}}}"#
        ),
    ] {
        assert_eq!(
            send_raw(&app, &capability, "denied-escaped", body).await?,
            StatusCode::FORBIDDEN
        );
    }
    assert!(
        billing
            .credits
            .holds(authority.project, 1, 100, 0)
            .await?
            .items
            .is_empty()
    );
    let account = billing.credits.account(authority.project).await?;
    assert_eq!(account.spent, FixedDecimal::zero());
    assert_eq!(account.reserved, FixedDecimal::zero());
    assert_eq!(
        send_model(&app, &capability, "allowed-1", "SELECTED_FILE_MARKER").await?,
        StatusCode::OK
    );
    assert_eq!(
        send_model(&app, &capability, "allowed-2", "TOOL_RESULT_MARKER").await?,
        StatusCode::OK
    );
    settled(&billing, authority.project, 2).await?;
    assert_eq!(
        billing.credits.account(authority.project).await?.spent,
        FixedDecimal::parse("0.102000")?
    );
    assert_eq!(captured.lock().await.len(), 2);
    assert_eq!(
        send_model(&app, &capability, "allowed-1", "SELECTED_FILE_MARKER").await?,
        StatusCode::CONFLICT
    );
    assert_eq!(
        captured.lock().await.len(),
        2,
        "a transport replay cannot spend or send twice"
    );
    for mutation in [
        "UPDATE access.bff_sessions SET revoked_at=clock_timestamp() WHERE session_id=$1",
        "UPDATE access.bff_sessions SET idle_expires_at=clock_timestamp()-interval '1 second' WHERE session_id=$1",
    ] {
        sqlx::query(sqlx::AssertSqlSafe(mutation))
            .bind(authority.session)
            .execute(&pool)
            .await?;
        assert_eq!(send_tool(&app, &capability).await?, StatusCode::FORBIDDEN);
        assert_eq!(
            send_model(&app, &capability, "denied-session", "MUST_NOT_SEND").await?,
            StatusCode::FORBIDDEN
        );
        sqlx::query("UPDATE access.bff_sessions SET revoked_at=NULL,idle_expires_at=clock_timestamp()+interval '20 minutes' WHERE session_id=$1").bind(authority.session).execute(&pool).await?;
    }
    sqlx::query("UPDATE control.project_memberships SET object_role='viewer' WHERE project_id=$1 AND actor_id=$2").bind(authority.project.as_uuid()).bind(authority.actor.as_uuid()).execute(&pool).await?;
    assert_eq!(send_tool(&app, &capability).await?, StatusCode::FORBIDDEN);
    assert_eq!(
        send_model(&app, &capability, "denied-member", "MUST_NOT_SEND").await?,
        StatusCode::FORBIDDEN
    );
    sqlx::query("UPDATE control.project_memberships SET object_role='owner' WHERE project_id=$1 AND actor_id=$2").bind(authority.project.as_uuid()).bind(authority.actor.as_uuid()).execute(&pool).await?;
    for mutation in [
        "UPDATE agent.agent_track_work_items SET lease_expires_at=clock_timestamp()-interval '1 second' WHERE run_id=$1",
        "UPDATE agent.agent_track_work_items SET attempt_started_at=clock_timestamp()-interval '1 hour' WHERE run_id=$1",
        "UPDATE agent.agent_track_work_items SET lease_token=gen_random_uuid() WHERE run_id=$1",
    ] {
        sqlx::query(sqlx::AssertSqlSafe(mutation))
            .bind(authority.run_id.as_uuid())
            .execute(&pool)
            .await?;
        assert_eq!(send_tool(&app, &capability).await?, StatusCode::FORBIDDEN);
        assert_eq!(
            send_model(&app, &capability, "denied-lease", "MUST_NOT_SEND").await?,
            StatusCode::FORBIDDEN
        );
        sqlx::query("UPDATE agent.agent_track_work_items SET lease_expires_at=clock_timestamp()+interval '1 minute',attempt_started_at=$2,lease_token=$3 WHERE run_id=$1").bind(authority.run_id.as_uuid()).bind(authority.now.get()).bind(authority.lease).execute(&pool).await?;
    }
    sqlx::query(
        "UPDATE agent.agent_runs SET cancellation_requested_at=clock_timestamp() WHERE run_id=$1",
    )
    .bind(authority.run_id.as_uuid())
    .execute(&pool)
    .await?;
    assert_eq!(send_tool(&app, &capability).await?, StatusCode::FORBIDDEN);
    assert_eq!(
        send_model(&app, &capability, "denied-cancel", "MUST_NOT_SEND").await?,
        StatusCode::FORBIDDEN
    );
    assert_eq!(captured.lock().await.len(), 2);
    assert_eq!(
        billing
            .credits
            .holds(authority.project, 1, 100, 0)
            .await?
            .items
            .len(),
        2
    );
    assert_eq!(
        billing.credits.account(authority.project).await?.spent,
        FixedDecimal::parse("0.102000")?
    );
    assert_eq!(
        billing.credits.account(authority.project).await?.reserved,
        FixedDecimal::zero()
    );
    let actual = serde_json::to_string(&*captured.lock().await)?;
    for secret in [
        "MUST_NOT_SEND",
        "sensitive-marker",
        capability.as_str(),
        "provider-secret-kept-in-agent",
    ] {
        assert!(!actual.contains(secret));
    }
    upstream.abort();
    Ok(())
}
