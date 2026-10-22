//! Actual published Agent client and JWT verifier; issuer and object response are synthetic.
#![allow(
    clippy::too_many_lines,
    reason = "one bounded fixture verifies the deployed connection and denial paths"
)]

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
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    service::TowerToHyperService,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeSet, HashMap},
    error::Error,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::{net::TcpListener, sync::RwLock};
use tokio_rustls::TlsAcceptor;

const CHILD: &str =
    "model_input_authorization::bundle_connection_tests::loaded_agent_control_consumer";

#[derive(Clone)]
struct Authority {
    issuer: String,
    jwk: Value,
    private_der: Arc<Vec<u8>>,
    case: Arc<RwLock<String>>,
    audience: Arc<RwLock<String>>,
    roles: Arc<RwLock<Vec<String>>>,
    verifier: Arc<RwLock<Option<Arc<auth::ServiceTokenVerifier>>>>,
    reads: Arc<AtomicUsize>,
}

async fn discovery(State(state): State<Authority>) -> Json<Value> {
    Json(
        json!({"issuer": state.issuer, "authorization_endpoint": format!("{}/authorize", state.issuer), "token_endpoint": format!("{}/token", state.issuer), "jwks_uri": format!("{}/jwks", state.issuer), "response_types_supported": ["code"], "subject_types_supported": ["public"], "id_token_signing_alg_values_supported": ["ES256"], "grant_types_supported": ["client_credentials"]}),
    )
}

async fn token(State(state): State<Authority>, headers: HeaderMap) -> (StatusCode, Json<Value>) {
    let basic = format!(
        "Basic {}",
        STANDARD.encode("labweaver-agent:synthetic-agent-secret")
    );
    if headers.get("authorization").and_then(|v| v.to_str().ok()) != Some(basic.as_str()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "invalid_client"})),
        );
    }
    let case = state.case.read().await;
    let client = if *case == "wrong-caller" {
        "unlisted-agent"
    } else {
        "labweaver-agent"
    };
    let configured_audience = state.audience.read().await.clone();
    let audience = if *case == "wrong-audience" {
        "other-service"
    } else {
        &configured_audience
    };
    let roles = if *case == "missing-role" {
        Vec::new()
    } else {
        state.roles.read().await.clone()
    };
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let claims = json!({"iss": state.issuer, "sub": "synthetic-service-account", "azp": client, "aud": audience, "iat": now, "exp": now + 300, "resource_access": {(configured_audience): {"roles": roles}}});
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::ES256);
    header.kid = Some("synthetic-signing-key".to_owned());
    let encoded = jsonwebtoken::encode(
        &header,
        &claims,
        &jsonwebtoken::EncodingKey::from_ec_der(&state.private_der),
    );
    match encoded {
        Ok(token) => (
            StatusCode::OK,
            Json(json!({"access_token": token, "token_type": "Bearer", "expires_in": 300})),
        ),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "synthetic_issuer_failure"})),
        ),
    }
}

async fn control(
    State(state): State<Authority>,
    headers: HeaderMap,
    Json(request): Json<contracts::http::ModelInputAuthorizationResolveRequest>,
) -> (StatusCode, HeaderMap, Json<Value>) {
    let Some(verifier) = state.verifier.read().await.clone() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            HeaderMap::new(),
            Json(json!({})),
        );
    };
    // The same mature verifier and permission used by Control's original route boundary.
    if verifier
        .authenticate_with_permission(&headers, "control.agent_run.read")
        .await
        .is_err()
    {
        return (
            StatusCode::FORBIDDEN,
            HeaderMap::new(),
            Json(json!({"error": "denied"})),
        );
    }
    if *state.case.read().await == "redirect" {
        let mut headers = HeaderMap::new();
        headers.insert(
            "location",
            axum::http::HeaderValue::from_static("/redirect-target"),
        );
        return (StatusCode::TEMPORARY_REDIRECT, headers, Json(json!({})));
    }
    state.reads.fetch_add(1, Ordering::SeqCst);
    (
        StatusCode::OK,
        HeaderMap::new(),
        Json(json!({"binding": request.binding})),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "explicit frozen-consumer CI target with age and synthetic TLS"]
async fn authenticated_bundle_reaches_control_with_the_existing_role() -> Result<(), Box<dyn Error>>
{
    let signing = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)?;
    let public = signing.public_key_raw();
    let jwk = json!({"kty": "EC", "crv": "P-256", "kid": "synthetic-signing-key", "alg": "ES256", "use": "sig", "x": URL_SAFE_NO_PAD.encode(&public[1..33]), "y": URL_SAFE_NO_PAD.encode(&public[33..65])});
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()])?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let origin = format!("https://localhost:{}/", listener.local_addr()?.port());
    let state = Authority {
        issuer: format!("{}issuer", origin),
        jwk,
        private_der: Arc::new(signing.serialize_der()),
        case: Arc::new(RwLock::new("valid".to_owned())),
        audience: Arc::new(RwLock::new(String::new())),
        roles: Arc::new(RwLock::new(Vec::new())),
        verifier: Arc::new(RwLock::new(None)),
        reads: Arc::new(AtomicUsize::new(0)),
    };
    let redirect_reads = Arc::new(AtomicUsize::new(0));
    let redirect_probe = Arc::clone(&redirect_reads);
    let app = Router::new()
        .route("/issuer/.well-known/openid-configuration", get(discovery))
        .route(
            "/issuer/jwks",
            get(|State(state): State<Authority>| async move { Json(json!({"keys": [state.jwk]})) }),
        )
        .route("/issuer/token", post(token))
        .route(
            "/internal/v1/model-input-authorizations/resolve",
            post(control),
        )
        .route(
            "/redirect-target",
            post(move || {
                let probe = Arc::clone(&redirect_probe);
                async move {
                    probe.fetch_add(1, Ordering::SeqCst);
                    Json(json!({}))
                }
            }),
        )
        .with_state(state.clone());
    let tls = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_no_client_auth()
    .with_single_cert(
        vec![certificate.cert.der().clone()],
        rustls::pki_types::PrivatePkcs8KeyDer::from(certificate.signing_key.serialize_der()).into(),
    )?;
    let acceptor = TlsAcceptor::from(Arc::new(tls));
    let server = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            let app = app.clone();
            tokio::spawn(async move {
                if let Ok(stream) = acceptor.accept(stream).await {
                    let builder =
                        hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());
                    let _ = builder
                        .serve_connection(TokioIo::new(stream), TowerToHyperService::new(app))
                        .await;
                }
            });
        }
    });
    let temporary = tempfile::tempdir()?;
    let settings = temporary.path().join("settings.json");
    std::fs::write(
        &settings,
        serde_json::to_vec(
            &json!({"origin": origin, "issuer": state.issuer, "certificate": certificate.cert.pem(), "clientSecret": "synthetic-agent-secret"}),
        )?,
    )?;
    let output = temporary.path().join("inputs");
    let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let producer = Command::new("python3")
        .arg(repository.join("tests/ansible/prepare_agent_control_fixture.py"))
        .arg("--settings")
        .arg(&settings)
        .arg("--output")
        .arg(&output)
        .output()?;
    assert!(
        producer.status.success(),
        "synthetic authenticated producer failed: {}",
        String::from_utf8_lossy(&producer.stderr)
    );
    let inputs: Value = serde_json::from_slice(&std::fs::read(output.join("consumer.json"))?)?;
    let agent: HashMap<String, String> = serde_json::from_value(inputs["agent"].clone())?;
    let control_env: HashMap<String, String> = serde_json::from_value(inputs["control"].clone())?;
    assert_eq!(
        inputs["source"]["encryptedBundleSha256"]
            .as_str()
            .map(str::len),
        Some(64)
    );
    let audience = control_env
        .get("LABWEAVER_SERVICE_AUDIENCE")
        .ok_or("control audience")?
        .clone();
    *state.audience.write().await = audience.clone();
    let assignments = inputs["assignments"]
        .as_array()
        .ok_or("identity assignments")?;
    let assignment = assignments
        .iter()
        .find(|a| {
            a["caller"] == agent["LABWEAVER_SERVICE_CLIENT_ID"]
                && a["target"] == control_env["LABWEAVER_SERVICE_CLIENT_ID"]
        })
        .ok_or("existing Agent Control assignment")?;
    let roles: Vec<String> = serde_json::from_value(assignment["roles"].clone())?;
    assert_eq!(roles, vec!["control.agent_run.read"]);
    *state.roles.write().await = roles;
    let ca = std::fs::read(&control_env["LABWEAVER_SERVICE_OIDC_CA"])?;
    let verifier = auth::ServiceTokenVerifier::discover(
        auth::ServiceAuthConfig::new(
            &control_env["LABWEAVER_SERVICE_OIDC_ISSUER"],
            audience,
            control_env["LABWEAVER_SERVICE_ALLOWED_CLIENT_IDS"]
                .split(',')
                .map(|s| s.trim().to_owned())
                .collect(),
            BTreeSet::new(),
            BTreeSet::from(["ES256".to_owned()]),
            300,
            5,
            auth::TransportSecurityMode::Strict,
        )?,
        auth::no_redirect_http_client(Some(&ca), auth::TransportSecurityMode::Strict)?,
    )
    .await?;
    *state.verifier.write().await = Some(Arc::new(verifier));
    for case in [
        "valid",
        "missing-role",
        "wrong-caller",
        "wrong-audience",
        "wrong-secret",
        "wrong-ca",
        "timeout",
        "partial",
        "redirect",
    ] {
        *state.case.write().await = case.to_owned();
        let before = state.reads.load(Ordering::SeqCst);
        let mut child = Command::new(std::env::current_exe()?);
        child
            .args(["--ignored", "--exact", CHILD, "--test-threads=1"])
            .envs(&agent)
            .env("LW_AGENT_CONTROL_CASE", case);
        if case == "wrong-secret" {
            let path = temporary.path().join("wrong-secret");
            std::fs::write(&path, "incorrect-synthetic-secret")?;
            child.env("LABWEAVER_SERVICE_CLIENT_SECRET_FILE", path);
        } else if case == "wrong-ca" {
            let other = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()])?;
            let path = temporary.path().join("wrong-ca");
            std::fs::write(&path, other.cert.pem())?;
            child.env("LABWEAVER_CONTROL_SERVICE_CA_PATH", path);
        } else if case == "timeout" {
            child.env("LABWEAVER_CONTROL_SERVICE_TIMEOUT_MILLISECONDS", "5001");
        } else if case == "partial" {
            child.env_remove("LABWEAVER_CONTROL_SERVICE_AUDIENCE");
        }
        let result = child.output()?;
        let logs = format!(
            "{}{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(!logs.contains("synthetic-agent-secret"));
        assert!(
            result.status.success() && logs.contains("1 passed"),
            "{case}: {logs}"
        );
        assert_eq!(
            state.reads.load(Ordering::SeqCst) - before,
            usize::from(case == "valid")
        );
        assert_eq!(redirect_reads.load(Ordering::SeqCst), 0);
    }
    server.abort();
    Ok(())
}

#[tokio::test]
#[ignore = "isolated consumer subprocess"]
async fn loaded_agent_control_consumer() -> Result<(), Box<dyn Error>> {
    let case = std::env::var("LW_AGENT_CONTROL_CASE")?;
    let required = |name: &str| std::env::var(name);
    let ca = std::fs::read(required("LABWEAVER_SERVICE_OIDC_CA")?)?;
    let token = auth::ServiceTokenClient::discover(
        auth::ServiceTokenClientConfig::new(
            &required("LABWEAVER_SERVICE_OIDC_ISSUER")?,
            required("LABWEAVER_SERVICE_CLIENT_ID")?,
            std::fs::read_to_string(required("LABWEAVER_SERVICE_CLIENT_SECRET_FILE")?)?,
            required("LABWEAVER_SERVICE_AUDIENCE")?,
            required("LABWEAVER_SERVICE_SCOPES")?
                .split(',')
                .map(|s| s.trim().to_owned())
                .collect(),
            30,
            auth::TransportSecurityMode::Strict,
        )?,
        auth::no_redirect_http_client(Some(&ca), auth::TransportSecurityMode::Strict)?,
    )
    .await?;
    let client = super::ModelInputAuthorizationClient::from_env(Arc::new(token));
    if matches!(case.as_str(), "timeout" | "partial") {
        assert!(client.is_err());
        return Ok(());
    }
    let client = client?.ok_or("missing connection")?;
    let paths = vec!["selected.md".to_owned()];
    let binding = serde_json::from_value(
        json!({"sessionSha256": "a".repeat(64), "actorId": "00000000-0000-7000-8000-000000000001", "projectId": "00000000-0000-7000-8000-000000000002", "courseId": null, "operationId": "materials.authoring.create", "validUntil": "2030-01-01T00:00:00.000Z", "packageId": "00000000-0000-7000-8000-000000000003", "packageRevision": 1, "policyId": "00000000-0000-7000-8000-000000000004", "policyRevision": 1, "selectionSha256": contracts::http::model_input_selection_sha256(&paths)?}),
    )?;
    let result = client
        .resolve(&contracts::http::ModelInputAuthorizationResolveRequest {
            binding,
            selected_paths: paths,
        })
        .await;
    assert_eq!(result.is_ok(), case == "valid");
    Ok(())
}
