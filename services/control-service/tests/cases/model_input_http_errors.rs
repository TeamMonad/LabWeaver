//! A real Control TLS/JWT failure response must belong to its generated operation error list.
use super::{build_billing, model_authorization};

use serde_json::{Value, json};
use std::collections::BTreeSet;
use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;

#[tokio::test]
async fn resolve_http_failure_code_is_declared_by_its_operation()
-> Result<(), Box<dyn std::error::Error>> {
    let pg = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(12)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            pg.get_host_port_ipv4(5432).await?
        ))
        .await?;
    let billing = build_billing::BuildBillingFixture::start(pool.clone()).await?;
    model_authorization::apply(&pool, persistence_sqlx::Domain::Agent).await?;
    let authority =
        model_authorization::ModelAuthorizationFixture::start(pool.clone(), &billing).await?;
    let binding: Value = sqlx::query_scalar(
        "SELECT request->'inputAuthorization' FROM agent.agent_run_dispatches WHERE run_id=$1",
    )
    .bind(authority.run_id.as_uuid())
    .fetch_one(&pool)
    .await?;
    sqlx::query("UPDATE access.bff_sessions SET revoked_at=clock_timestamp() WHERE session_id=$1")
        .bind(authority.session)
        .execute(&pool)
        .await?;
    let http = reqwest::Client::builder()
        .no_proxy()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .tls_built_in_root_certs(false)
        .add_root_certificate(reqwest::Certificate::from_pem(&std::fs::read(
            &authority.control_ca_file,
        )?)?)
        .build()?;
    let mut headers = reqwest::header::HeaderMap::new();
    billing
        .token_client
        .bearer_auth_for(
            &mut headers,
            "labweaver-control",
            &BTreeSet::from(["control.agent_run.read".to_owned()]),
        )
        .await?;
    let response = http
        .post(
            authority
                .control_base_uri
                .join("internal/v1/model-input-authorizations/resolve")?,
        )
        .headers(headers)
        .json(&json!({"binding":binding,"selectedPaths":["README.md"]}))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
    let problem: contracts::ProblemDetails = serde_json::from_slice(&response.bytes().await?)?;
    assert_eq!(
        problem.diagnostic_code.as_str(),
        "LW_MODEL_INPUT_AUTHORIZATION_DENIED"
    );
    let artifact = contracts::schema::generate_all()?
        .into_iter()
        .find(|artifact| {
            artifact.relative_path == "schemas/openapi/labweaver-gateway-internal.v1.json"
        })
        .ok_or("internal OpenAPI missing")?;
    let document: Value = serde_json::from_slice(&artifact.bytes)?;
    let operation = &document["paths"]["/internal/v1/model-input-authorizations/resolve"]["post"];
    assert_eq!(operation["operationId"], "resolveModelInputAuthorization");
    let errors = operation["x-labweaver-errors"]
        .as_array()
        .ok_or("declared HTTP errors missing")?;
    assert!(
        errors
            .iter()
            .any(|code| code.as_str() == Some(problem.diagnostic_code.as_str()))
    );
    for task_only in [
        "LW_MODEL_INPUT_AUTHORIZATION_REQUIRED",
        "LW_MODEL_INPUT_AUTHORIZATION_UNAVAILABLE",
    ] {
        assert!(!errors.iter().any(|code| code.as_str() == Some(task_only)));
    }
    Ok(())
}
