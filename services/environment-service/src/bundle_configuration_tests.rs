//! Actual Bundle-to-client loading; external identity and Resource responses are synthetic.
#![allow(
    clippy::too_many_lines,
    reason = "one isolated fixture follows both deployed input paths"
)]

use std::{collections::HashMap, error::Error, process::Command, sync::Arc};

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use hyper_util::{rt::TokioIo, service::TowerToHyperService};
use serde_json::{Value, json};
use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

const CHILD: &str = "metering::bundle_configuration_tests::loaded_executor_consumer";

#[derive(Clone)]
struct Authority {
    issuer: String,
    token: String,
}

async fn discovery(State(state): State<Authority>) -> Json<Value> {
    Json(json!({
        "issuer": state.issuer,
        "authorization_endpoint": format!("{}/authorize", state.issuer),
        "token_endpoint": format!("{}/token", state.issuer),
        "jwks_uri": format!("{}/jwks", state.issuer),
        "response_types_supported": ["code"], "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["RS256"],
        "grant_types_supported": ["client_credentials"]
    }))
}

async fn token(State(state): State<Authority>, headers: HeaderMap) -> (StatusCode, Json<Value>) {
    let expected = format!(
        "Basic {}",
        STANDARD.encode("synthetic-environment:synthetic-client-secret")
    );
    if headers.get("authorization").and_then(|v| v.to_str().ok()) != Some(expected.as_str()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "invalid_client"})),
        );
    }
    (
        StatusCode::OK,
        Json(json!({"access_token": state.token, "token_type": "Bearer", "expires_in": 300})),
    )
}

async fn resource(State(state): State<Authority>, headers: HeaderMap) -> (StatusCode, Json<Value>) {
    let expected = format!("Bearer {}", state.token);
    if headers.get("authorization").and_then(|v| v.to_str().ok()) != Some(expected.as_str()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "invalid_token"})),
        );
    }
    (StatusCode::OK, Json(json!({"syntheticResourceRead": true})))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires pinned age, Python Bundle producer and isolated Docker PostgreSQL; explicit CI target"]
async fn authenticated_bundle_loads_both_executor_clients() -> Result<(), Box<dyn Error>> {
    let postgres = Postgres::default().with_tag("17.6-alpine").start().await?;
    let database_url = format!(
        "postgres://postgres:postgres@127.0.0.1:{}/postgres",
        postgres.get_host_port_ipv4(5432).await?
    );
    let pool = sqlx::PgPool::connect(&database_url).await?;
    sqlx::raw_sql("CREATE TABLE bundle_loader_marker (value text NOT NULL); INSERT INTO bundle_loader_marker VALUES ('same-owner-database')").execute(&pool).await?;
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()])?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("https://localhost:{}", listener.local_addr()?.port());
    let issuer = format!("{base}/issuer");
    let token_value = format!(
        "{}.{}.synthetic",
        URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256"}"#),
        URL_SAFE_NO_PAD.encode(br#"{"aud":"labweaver-resource"}"#)
    );
    let state = Authority {
        issuer: issuer.clone(),
        token: token_value,
    };
    let app = Router::new()
        .route("/issuer/.well-known/openid-configuration", get(discovery))
        .route("/issuer/token", post(token))
        .route("/issuer/jwks", get(|| async { Json(json!({"keys": []})) }))
        .route(
            "/resource/internal/v1/environment-runtime-authorization",
            post(resource),
        )
        .with_state(state);
    let key = rustls::pki_types::PrivatePkcs8KeyDer::from(certificate.signing_key.serialize_der());
    let tls = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_no_client_auth()
    .with_single_cert(vec![certificate.cert.der().clone()], key.into())?;
    let acceptor = TlsAcceptor::from(Arc::new(tls));
    let server = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            let app = app.clone();
            tokio::spawn(async move {
                if let Ok(stream) = acceptor.accept(stream).await {
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), TowerToHyperService::new(app))
                        .await;
                }
            });
        }
    });
    let directory = tempfile::tempdir()?;
    let settings = directory.path().join("settings.json");
    std::fs::write(
        &settings,
        serde_json::to_vec(
            &json!({"databaseUrl": database_url, "issuer": issuer, "resourceUri": format!("{base}/resource/"), "certificate": certificate.cert.pem(), "clientSecret": "synthetic-client-secret"}),
        )?,
    )?;
    let output = directory.path().join("consumers");
    let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let prepared = Command::new("python3")
        .arg(repository.join("tests/ansible/prepare_executor_bundle_fixture.py"))
        .arg("--input")
        .arg(&settings)
        .arg("--output")
        .arg(&output)
        .output()?;
    assert!(
        prepared.status.success(),
        "synthetic Bundle producer failed: {}",
        String::from_utf8_lossy(&prepared.stderr)
    );
    let bindings: Value =
        serde_json::from_slice(&std::fs::read(output.join("consumer-inputs.json"))?)?;
    assert_eq!(
        bindings["source"]["encryptedBundleSha256"]
            .as_str()
            .map(str::len),
        Some(64)
    );
    for role in ["container-executor", "kubevirt-executor"] {
        let env: HashMap<String, String> = serde_json::from_value(bindings["roles"][role].clone())?;
        for failure in ["none", "wrong-secret", "wrong-ca"] {
            let mut child = Command::new(std::env::current_exe()?);
            child
                .args(["--ignored", "--exact", CHILD, "--test-threads=1"])
                .envs(&env)
                .env("LW_SYNTHETIC_FAILURE", failure);
            if failure == "wrong-secret" {
                let path = directory.path().join("wrong-secret");
                std::fs::write(&path, "wrong-synthetic-secret")?;
                child.env("LABWEAVER_SERVICE_CLIENT_SECRET_FILE", path);
            } else if failure == "wrong-ca" {
                let other = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()])?;
                let path = directory.path().join("wrong-ca");
                std::fs::write(&path, other.cert.pem())?;
                child.env("LABWEAVER_SERVICE_OIDC_CA", path);
            }
            let result = child.output()?;
            assert!(
                result.status.success(),
                "{role}/{failure} consumer failed: {} {}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            let logs = format!(
                "{}{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            assert!(!logs.contains("synthetic-client-secret"));
            assert!(!logs.contains("postgres://"));
        }
    }
    server.abort();
    Ok(())
}

#[tokio::test]
#[ignore = "subprocess consumer of the synthetic authenticated Bundle fixture"]
async fn loaded_executor_consumer() -> Result<(), Box<dyn Error>> {
    let failure = std::env::var("LW_SYNTHETIC_FAILURE")?;
    let config: Value = serde_yaml::from_str(&std::fs::read_to_string(std::env::var(
        "LABWEAVER_RUNTIME_EXECUTOR_CONFIG_FILE",
    )?)?)?;
    let database_url = std::fs::read_to_string(
        config["databaseUrlFile"]
            .as_str()
            .ok_or("missing DB file")?,
    )?;
    let owner_pool = sqlx::PgPool::connect(&std::env::var("LABWEAVER_DATABASE_URL")?).await?;
    let executor_pool = sqlx::PgPool::connect(database_url.trim()).await?;
    for pool in [&owner_pool, &executor_pool] {
        let marker: String = sqlx::query_scalar("SELECT value FROM bundle_loader_marker")
            .fetch_one(pool)
            .await?;
        assert_eq!(marker, "same-owner-database");
    }
    assert!(config["nats"].get("server").is_none());
    let endpoint = std::fs::read_to_string(
        config["nats"]["serverFile"]
            .as_str()
            .ok_or("missing NATS file")?,
    )?;
    assert!(endpoint.starts_with("tls://"));
    let client = super::ResourceUsageClient::from_env().await;
    if failure == "wrong-ca" {
        assert!(client.is_err());
        return Ok(());
    }
    let result = client?
        .post_json(
            "internal/v1/environment-runtime-authorization",
            &json!({"synthetic": true}),
        )
        .await;
    if failure == "wrong-secret" {
        assert!(result.is_err());
    } else {
        assert_eq!(
            result?.json::<Value>().await?["syntheticResourceRead"],
            true
        );
    }
    Ok(())
}
