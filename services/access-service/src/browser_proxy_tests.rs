//! Browser-router coverage with persisted sessions and external service boundaries.

use std::{
    collections::BTreeSet,
    error::Error,
    sync::{Arc, LazyLock},
};

use auth::{CreateBffSession, ServiceTokenClient, ServiceTokenClientConfig, create_bff_session};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{Form, State},
    http::{HeaderMap, Method, StatusCode, Uri, header},
    response::{IntoResponse, Response},
    routing::{any, get, post},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use rcgen::KeyPair;
use serde_json::{Value, json};
use sqlx::postgres::PgPoolOptions;
use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
    sync::Mutex,
};
use url::Url;

use super::*;

#[path = "oidc_login_tests.rs"]
mod oidc_login_tests;

#[path = "freeze_context_tests.rs"]
mod freeze_context_tests;

#[path = "platform_configuration_browser_tests.rs"]
mod platform_configuration_browser_tests;

#[path = "../tests/support/mod.rs"]
mod support;

const ORIGIN: &str = "https://portal.example.invalid";
const AUDIENCE: &str = "labweaver-control";
const PERMISSION: &str = "control.platform-images.manage";
static METRICS: LazyLock<Result<telemetry::PrometheusHandle, telemetry::TelemetryError>> =
    LazyLock::new(|| telemetry::init_metrics("access-browser-test"));

struct Tasks(Vec<tokio::task::JoinHandle<()>>);

impl Drop for Tasks {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

async fn assert_problem_response(
    response: reqwest::Response,
    status: StatusCode,
    diagnostic: &str,
) -> Result<contracts::ProblemDetails, Box<dyn Error>> {
    assert_eq!(response.status(), status);
    let headers = response.headers().clone();
    let body = response.json::<Value>().await?;
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/problem+json"),
        "actual error body: {body}"
    );
    let problem: contracts::ProblemDetails = serde_json::from_value(body)?;
    assert_eq!(problem.status, status.as_u16());
    assert_eq!(problem.diagnostic_code.as_str(), diagnostic);
    assert!(!problem.problem_type.is_empty());
    assert!(!problem.title.is_empty());
    assert!(!problem.detail.is_empty());
    assert_eq!(
        problem.instance,
        format!("urn:labweaver:request:{}", problem.request_id)
    );
    assert_eq!(
        headers.get("x-request-id").and_then(|v| v.to_str().ok()),
        Some(problem.request_id.as_str())
    );
    let trace_id = headers
        .get("traceparent")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split('-').nth(1));
    assert!(trace_id.is_some());
    assert_eq!(problem.trace_id.as_deref(), trace_id);
    Ok(problem)
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn stale_csrf_write_returns_problem_details_without_writing_to_replacement_session()
-> Result<(), Box<dyn Error>> {
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            container.get_host_port_ipv4(5432).await?
        ))
        .await?;
    support::apply_access_migrations(&pool).await?;
    let mut tasks = Tasks(Vec::new());
    let actor_a = Uuid::now_v7();
    let (state, _) = Box::pin(state(pool, actor_a, &mut tasks)).await?;
    let a = session(
        &state.pool,
        &state.key_ring,
        actor_a,
        contracts::PlatformRole::Student,
    )
    .await?;
    let actor_b = Uuid::now_v7();
    let b = session(
        &state.pool,
        &state.key_ring,
        actor_b,
        contracts::PlatformRole::Student,
    )
    .await?;
    let cookie = format!(
        "{}={}",
        state.deployment.browser.session_cookie_name, b.session_id
    );
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let router = browser_router(Arc::clone(&state));
    tasks.0.push(tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    }));
    let client = reqwest::Client::builder().no_proxy().build()?;
    let endpoint = format!("{base}/api/v1/me/ssh-public-keys");
    let request = json!({
        "publicKeyOpenssh": "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA synthetic-contract-test"
    });
    let anonymous = assert_problem_response(
        client
            .get(format!("{base}/api/v1/auth/session"))
            .send()
            .await?,
        StatusCode::UNAUTHORIZED,
        "LW_AUTH_REQUIRED",
    )
    .await?;
    assert!(!anonymous.retryable);
    for (origin, csrf, diagnostic) in [
        (
            Some(ORIGIN),
            Some(a.csrf_token.expose()),
            "LW_AUTH_CSRF_REJECTED",
        ),
        (Some(ORIGIN), None, "LW_AUTH_CSRF_REJECTED"),
        (
            Some("https://attacker.example.invalid"),
            Some(b.csrf_token.expose()),
            "LW_AUTH_CSRF_REJECTED",
        ),
        (None, Some(b.csrf_token.expose()), "LW_AUTH_CSRF_REJECTED"),
    ] {
        let request_id = format!("csrf-contract-{}", Uuid::now_v7());
        let mut post = client
            .post(&endpoint)
            .header(header::COOKIE, &cookie)
            .header("x-request-id", &request_id)
            .header("Idempotency-Key", Uuid::now_v7().to_string())
            .json(&request);
        if let Some(origin) = origin {
            post = post.header(header::ORIGIN, origin);
        }
        if let Some(csrf) = csrf {
            post = post.header("x-csrf-token", csrf);
        }
        let problem =
            assert_problem_response(post.send().await?, StatusCode::FORBIDDEN, diagnostic).await?;
        assert_eq!(problem.request_id, request_id);
        assert!(
            !problem.retryable,
            "a stale write must not be retried unchanged"
        );
        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM access.ssh_public_keys")
            .fetch_one(&state.pool)
            .await?;
        assert_eq!(rows, 0, "CSRF rejection must happen before key writes");
        let ledger: i64 = sqlx::query_scalar("SELECT count(*) FROM access.idempotency_ledger")
            .fetch_one(&state.pool)
            .await?;
        assert_eq!(
            ledger, 0,
            "CSRF rejection must happen before idempotency reservation"
        );
    }
    let response = client
        .get(format!("{base}/api/v1/auth/session"))
        .header(header::COOKIE, &cookie)
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let identity: contracts::AuthSession = response.json().await?;
    assert_eq!(identity.actor.actor_id.to_string(), actor_b.to_string());
    let response = client
        .post(&endpoint)
        .header(header::COOKIE, &cookie)
        .header(header::ORIGIN, ORIGIN)
        .header("x-csrf-token", b.csrf_token.expose())
        .header("Idempotency-Key", Uuid::now_v7().to_string())
        .json(&request)
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/json")
    );
    let owner: Uuid = sqlx::query_scalar("SELECT actor_id FROM access.ssh_public_keys")
        .fetch_one(&state.pool)
        .await?;
    assert_eq!(owner, actor_b);
    Ok(())
}

#[derive(Clone)]
struct Authority {
    issuer: String,
    signing_key: Arc<EncodingKey>,
    decoding_key: DecodingKey,
    jwk: Value,
    admin: Uuid,
    requests: Arc<Mutex<Vec<Value>>>,
    upstream: &'static str,
    environment: Option<contracts::environment::EnvironmentInstance>,
}

async fn discovery(State(state): State<Authority>) -> Json<Value> {
    let issuer = state.issuer;
    Json(json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{issuer}/authorize"),
        "token_endpoint": format!("{issuer}/token"),
        "jwks_uri": format!("{issuer}/jwks"),
        "end_session_endpoint": format!("{issuer}/logout"),
        "response_types_supported": ["code"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["ES256"]
    }))
}

async fn jwks(State(state): State<Authority>) -> Json<Value> {
    Json(json!({"keys": [state.jwk]}))
}

async fn token(
    State(state): State<Authority>,
    headers: HeaderMap,
    Form(form): Form<std::collections::HashMap<String, String>>,
) -> Response {
    let credentials = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode("access-test:fixture-secret")
    );
    if form.get("grant_type").map(String::as_str) != Some("client_credentials")
        || headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            != Some(credentials.as_str())
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let audience = form.get("audience").map_or(AUDIENCE, String::as_str);
    let mut header = Header::new(Algorithm::ES256);
    header.kid = Some("fixture".to_owned());
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let claims = json!({
        "iss": state.issuer, "aud": audience, "azp": "access-test", "sub": "service-account-access",
        "iat": now, "nbf": now - 1, "exp": now + 300,
        "resource_access": {audience: {"roles": [PERMISSION]}}
    });
    match jsonwebtoken::encode(&header, &claims, &state.signing_key) {
        Ok(token) => {
            Json(json!({"access_token": token, "token_type": "Bearer", "expires_in": 300}))
                .into_response()
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

fn directory_users_response() -> Response {
    (
        StatusCode::OK,
        Json(json!([
            {
                "id": Uuid::now_v7(),
                "username": "alice",
                "firstName": "Alice",
                "lastName": "Researcher",
                "enabled": true
            }
        ])),
    )
        .into_response()
}

fn control_request_record(
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    body: &Bytes,
    actor: Option<&str>,
    upstream: &'static str,
) -> Value {
    json!({
        "method": method.as_str(), "path": uri.path(), "query": uri.query(), "actor": actor,
        "upstream": upstream,
        "session": headers.get("x-labweaver-session-id").and_then(|v| v.to_str().ok()),
        "ifMatch": headers.get(header::IF_MATCH).and_then(|v| v.to_str().ok()),
        "idempotencyKey": headers.get("idempotency-key").and_then(|v| v.to_str().ok()),
        "body": serde_json::from_slice::<Value>(body).ok(),
        "browserCookie": headers.contains_key(header::COOKIE),
        "browserCsrf": headers.contains_key("x-csrf-token"),
        "project": headers.get("x-labweaver-project-id").and_then(|v| v.to_str().ok()),
        "course": headers.get("x-labweaver-course-id").and_then(|v| v.to_str().ok()),
        "modelContextCount": headers.get_all("x-labweaver-model-input-authorization-context").iter().count(),
        "modelContext": headers.get("x-labweaver-model-input-authorization-context").and_then(|v| v.to_str().ok()).and_then(|v| serde_json::from_str::<Value>(v).ok())
    })
}

#[allow(
    clippy::too_many_lines,
    reason = "the fixture keeps downstream authorization and response boundaries in one handler"
)]
async fn control(
    State(state): State<Authority>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(token) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let mut validation = Validation::new(Algorithm::ES256);
    validation.set_issuer(&[&state.issuer]);
    validation.set_audience(&[AUDIENCE]);
    let Ok(claims) = jsonwebtoken::decode::<Value>(token, &state.decoding_key, &validation) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if claims.claims["azp"] != "access-test"
        || claims.claims["resource_access"][AUDIENCE]["roles"] != json!([PERMISSION])
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let delegated_actor = headers
        .get("x-labweaver-resource-delegation")
        .and_then(|value| value.to_str().ok())
        .and_then(|token| auth::decode_resource_delegation(&[7_u8; 32], token).ok())
        .map(|delegation| delegation.actor_id.to_string());
    let actor = headers
        .get("x-labweaver-actor-id")
        .and_then(|v| v.to_str().ok())
        .or(delegated_actor.as_deref());
    let record = control_request_record(&method, &uri, &headers, &body, actor, state.upstream);
    state.requests.lock().await.push(record);
    if state.upstream == "environment"
        && let Some(environment) = &state.environment
    {
        if method == Method::GET && uri.path() == format!("/api/v1/environments/{}", environment.id)
        {
            return Json(environment.clone()).into_response();
        }
        if method == Method::POST
            && uri.path() == format!("/api/v1/environments/{}/stop", environment.id)
        {
            return (
                StatusCode::ACCEPTED,
                Json(json!({"boundary": "request_record_only"})),
            )
                .into_response();
        }
    }
    if method == Method::GET && uri.path() == "/admin/realms/test/users" {
        return directory_users_response();
    }
    let project_membership_read = method == Method::GET
        && state.upstream == "control"
        && uri
            .path()
            .strip_prefix("/api/v1/projects/")
            .is_some_and(|value| value.split('/').count() == 2 && value.ends_with("/members"));
    let environment_inventory_read = method == Method::GET
        && state.upstream == "environment"
        && uri.path() == "/api/v1/environments";
    if (project_membership_read || environment_inventory_read)
        && actor != Some(state.admin.to_string().as_str())
    {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"diagnosticCode": "LW_AUTH_SCOPE_DENIED"})),
        )
            .into_response();
    }
    if project_membership_read {
        return (
            StatusCode::OK,
            Json(json!([{
                "actorId": state.admin,
                "courseId": null,
                "username": "admin",
                "displayName": "Platform Admin",
                "role": "teacher",
                "state": "active",
                "revision": 1,
                "expiresAt": null
            }])),
        )
            .into_response();
    }
    if environment_inventory_read {
        return (
            StatusCode::OK,
            Json(json!({
                "items": [],
                "nextCursor": null,
                "snapshotSequence": 1,
                "snapshotAt": "2026-01-01T00:00:00.000Z"
            })),
        )
            .into_response();
    }
    if actor != Some(state.admin.to_string().as_str()) {
        if method == Method::GET
            && uri
                .path()
                .strip_prefix("/api/v1/projects/")
                .is_some_and(|value| value.split('/').count() == 1)
        {
            let project_id = uri
                .path()
                .strip_prefix("/api/v1/projects/")
                .unwrap_or_default();
            return (
                StatusCode::OK,
                Json(json!({
                    "id": project_id,
                    "ownerActorId": state.admin,
                    "name": "fixture project",
                    "description": null,
                    "courseId": null,
                    "state": "active",
                    "revision": 1,
                    "createdAt": "2026-01-01T00:00:00.000Z",
                    "updatedAt": "2026-01-01T00:00:00.000Z"
                })),
            )
                .into_response();
        }
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"diagnosticCode": "LW_PLATFORM_ADMIN_REQUIRED"})),
        )
            .into_response();
    }
    if uri.path().contains("/builds/") && method == Method::GET {
        (
            StatusCode::OK,
            Json(json!({"status": {"state": "running", "revision": 3}})),
        )
            .into_response()
    } else if method == Method::GET {
        (
            StatusCode::OK,
            Json(json!({"state": "importing", "revision": 3})),
        )
            .into_response()
    } else if uri.path().contains("/builds/")
        && serde_json::from_slice::<Value>(&body).is_ok_and(|value| value["expectedRevision"] == 3)
    {
        (
            StatusCode::ACCEPTED,
            Json(json!({"status": {"state": "running", "revision": 4, "cancellationRequested": true}})),
        )
            .into_response()
    } else if uri.path().contains("/builds/") {
        (
            StatusCode::CONFLICT,
            Json(json!({"diagnosticCode": "LW_AGENT_BUILD_REVISION_CONFLICT", "retryable": false})),
        )
            .into_response()
    } else {
        (
            StatusCode::ACCEPTED,
            Json(json!({"state": "cancelling", "revision": 4})),
        )
            .into_response()
    }
}

async fn session(
    pool: &PgPool,
    key_ring: &KeyRing,
    actor: Uuid,
    role: contracts::PlatformRole,
) -> Result<BffSession, Box<dyn Error>> {
    sqlx::query("INSERT INTO access.actors (actor_id,issuer,subject_sha256) VALUES ($1,'https://fixture.example.invalid',$2)")
        .bind(actor).bind(Sha256Digest::of_bytes(actor.as_bytes()).to_string()).execute(pool).await?;
    let now = OffsetDateTime::now_utc();
    Ok(create_bff_session(
        pool,
        key_ring,
        CreateBffSession {
            actor_id: actor,
            roles: vec![role],
            authorization_revision: 1,
            expires_at: now + Duration::minutes(10),
            idle_ttl: Duration::minutes(5),
            oidc_sid: None,
            logout_hint: fixture_logout_hint(),
        },
        now,
    )
    .await?)
}

fn fixture_logout_hint() -> String {
    "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJpc3MiOiJodHRwczovL2ZpeHR1cmUuZXhhbXBsZS5pbnZhbGlkIiwiYXVkIjoibGFid2VhdmVyLXdlYiIsInN1YiI6ImZpeHR1cmUtdXNlciIsImlhdCI6MTcwMDAwMDAwMCwiZXhwIjo0MTAyNDQ0ODAwfQ.c2lnbmF0dXJl".to_owned()
}

async fn nats_boundary(tasks: &mut Tasks) -> Result<async_nats::Client, Box<dyn Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tasks.0.push(tokio::spawn(async move {
        let Ok((stream, _)) = listener.accept().await else { return; };
        let (read, mut write) = stream.into_split();
        if write.write_all(b"INFO {\"server_id\":\"fixture\",\"server_name\":\"fixture\",\"version\":\"2.11.0\",\"proto\":1,\"max_payload\":1048576}\r\n").await.is_err() { return; }
        let mut lines = BufReader::new(read).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if line == "PING" && write.write_all(b"PONG\r\n").await.is_err() { return; }
        }
    }));
    Ok(async_nats::connect(address.to_string()).await?)
}

#[allow(
    clippy::too_many_lines,
    reason = "the real router fixture assembles existing auth and downstream clients without alternate production wiring"
)]
async fn state_with_environment(
    pool: PgPool,
    admin: Uuid,
    tasks: &mut Tasks,
    environment: Option<contracts::environment::EnvironmentInstance>,
) -> Result<(Arc<AppState>, Arc<Mutex<Vec<Value>>>), Box<dyn Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("https://{}/", listener.local_addr()?);
    let issuer = format!("{base}realms/test");
    let tls = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_owned()])?;
    let ca = tls.cert.pem();
    let server_config =
        http_transport::server_config(ca.as_bytes(), tls.signing_key.serialize_pem().as_bytes())?;
    let key = KeyPair::generate()?;
    let public = key.public_key_raw();
    let x = URL_SAFE_NO_PAD.encode(&public[1..33]);
    let y = URL_SAFE_NO_PAD.encode(&public[33..65]);
    let authority = Authority {
        issuer: issuer.clone(),
        admin,
        requests: Arc::new(Mutex::new(Vec::new())),
        signing_key: Arc::new(EncodingKey::from_ec_der(&key.serialize_der())),
        decoding_key: DecodingKey::from_ec_components(&x, &y)?,
        jwk: json!({"kty": "EC", "crv": "P-256", "x": x, "y": y, "use": "sig", "alg": "ES256", "kid": "fixture"}),
        upstream: "control",
        environment,
    };
    let router = Router::new()
        .route(
            "/realms/test/.well-known/openid-configuration",
            get(discovery),
        )
        .route("/realms/test/jwks", get(jwks))
        .route("/realms/test/token", post(token))
        .route("/api/v1/admin/images/uploads/{id}", any(control))
        .route("/api/v1/admin/images/uploads/{id}/cancel", any(control))
        .route(
            "/api/v1/projects/{project_id}/candidates/{candidate_id}/builds/{target}",
            any(control),
        )
        .route(
            "/api/v1/projects/{project_id}/candidates/{candidate_id}/builds/{target}/cancel",
            any(control),
        )
        .route(
            "/api/v1/environments/{environment_id}/freeze",
            post(control),
        )
        .route("/api/v1/projects/{project_id}", any(control))
        .route("/api/v1/projects/{project_id}/members", any(control))
        .route("/admin/realms/test/users", any(control))
        .with_state(authority.clone());
    let control_server_config = Arc::clone(&server_config);
    tasks.0.push(tokio::spawn(async move {
        let _ = http_transport::serve_tls(listener, router, control_server_config).await;
    }));
    let environment_listener = TcpListener::bind("127.0.0.1:0").await?;
    let environment_base = format!("https://{}/", environment_listener.local_addr()?);
    let environment_authority = Authority {
        upstream: "environment",
        ..authority.clone()
    };
    let environment_router = Router::new()
        .route("/api/v1/environments", any(control))
        .route("/api/v1/environments/{environment_id}", any(control))
        .route("/api/v1/environments/{environment_id}/stop", any(control))
        .with_state(environment_authority);
    let environment_server_config = Arc::clone(&server_config);
    tasks.0.push(tokio::spawn(async move {
        let _ = http_transport::serve_tls(
            environment_listener,
            environment_router,
            environment_server_config,
        )
        .await;
    }));
    let resource_listener = TcpListener::bind("127.0.0.1:0").await?;
    let resource_base = format!("https://{}/", resource_listener.local_addr()?);
    let resource_authority = Authority {
        upstream: "resource",
        ..authority.clone()
    };
    let resource_router = Router::new()
        .route("/api/v1/projects/{project_id}/usage", get(control))
        .with_state(resource_authority);
    tasks.0.push(tokio::spawn(async move {
        let _ = http_transport::serve_tls(resource_listener, resource_router, server_config).await;
    }));
    let mut deployment = AccessAuthFile::parse_yaml(include_str!(
        "../../../deploy/config/access-auth.yaml.example"
    ))?;
    deployment.oidc.issuer = issuer.clone();
    deployment.oidc.jwt_algorithms = BTreeSet::from(["ES256".to_owned()]);
    for gateway in [
        &mut deployment.control_gateway,
        &mut deployment.environment_gateway,
        &mut deployment.evaluation_gateway,
    ] {
        gateway.base_uri.clone_from(&base);
        gateway.allowed_server_sans = vec!["127.0.0.1".to_owned()];
    }
    deployment.environment_gateway.base_uri = environment_base;
    deployment.environment_owner_resolver.resolver_uri = base.trim_end_matches('/').to_owned();
    deployment.resource_gateway.base_uri = resource_base;
    deployment.resource_gateway.allowed_server_sans = vec!["127.0.0.1".to_owned()];
    let config = AuthConfig::new(
        &issuer,
        deployment.oidc.client_id.clone(),
        &deployment.oidc.redirect_uri,
        &deployment.oidc.post_logout_redirect_uri,
        deployment.oidc.audience.clone(),
        deployment.browser.allowed_origins.clone(),
        900,
    )?;
    let oidc_http = no_redirect_http_client(Some(ca.as_bytes()), TransportSecurityMode::Strict)?;
    let provider = OidcProvider::discover_with_http_client(&config, oidc_http.clone()).await?;
    let bearer_authorizer =
        Arc::new(build_bearer_authorizer(&config, &deployment.oidc, oidc_http.clone()).await?);
    let backchannel_logout_authorizer = Arc::new(
        build_backchannel_logout_authorizer(&config, &deployment.oidc, oidc_http.clone()).await?,
    );
    let service_token_verifier = Arc::new(
        ServiceTokenVerifier::discover(
            ServiceAuthConfig::new(
                &issuer,
                AUDIENCE.to_owned(),
                BTreeSet::from(["access-test".to_owned()]),
                BTreeSet::new(),
                BTreeSet::from(["ES256".to_owned()]),
                600,
                1,
                TransportSecurityMode::Strict,
            )?,
            oidc_http.clone(),
        )
        .await?,
    );
    let token_client = Arc::new(
        ServiceTokenClient::discover(
            ServiceTokenClientConfig::new(
                &issuer,
                "access-test".to_owned(),
                "fixture-secret".to_owned(),
                AUDIENCE.to_owned(),
                BTreeSet::from([PERMISSION.to_owned()]),
                30,
                TransportSecurityMode::Strict,
            )?,
            oidc_http.clone(),
        )
        .await?,
    );
    let target = ServiceTokenTarget {
        audience: AUDIENCE.to_owned(),
        scopes: BTreeSet::from([PERMISSION.to_owned()]),
    };
    let control_proxy = proxy::ControlGatewayProxy::new(
        &deployment.control_gateway,
        ca.as_bytes(),
        TransportSecurityMode::Strict,
        Arc::clone(&token_client),
        target.clone(),
    )?;
    let environment_proxy = proxy::ControlGatewayProxy::new(
        &deployment.environment_gateway,
        ca.as_bytes(),
        TransportSecurityMode::Strict,
        Arc::clone(&token_client),
        target.clone(),
    )?;
    let evaluation_proxy = control_proxy.clone();
    let owner_resolver = EnvironmentOwnerResolverClient::new(
        &deployment.environment_owner_resolver.contract(),
        ca.as_bytes(),
        token_client.as_ref().clone(),
        AUDIENCE.to_owned(),
        target.scopes.clone(),
        std::time::Duration::from_millis(5),
        TransportSecurityMode::Strict,
    )?;
    let console_gateway = console::ConsoleGateway::new(
        &base,
        ca.as_bytes(),
        Arc::clone(&token_client),
        target.clone(),
    )
    .map_err(|_| std::io::Error::other("fixture console configuration rejected"))?;
    let resource_proxy = proxy::ResourceGatewayProxy::new(
        &deployment.resource_gateway,
        ca.as_bytes(),
        &[7_u8; 32],
        TransportSecurityMode::Strict,
        Arc::clone(&token_client),
        target,
    )?;
    let runtime_proxy = proxy::RuntimeGatewayProxy::new(&deployment.environment_gateway)?;
    let directory =
        directory::KeycloakDirectory::new(&issuer, oidc_http.clone(), Arc::clone(&token_client))
            .map_err(|_| std::io::Error::other("fixture directory configuration rejected"))?;
    let key_material = format!("fixture:{}", URL_SAFE_NO_PAD.encode([7_u8; 32]));
    let key_ring = KeyRing::parse("fixture".to_owned(), &key_material)?;
    let state = Arc::new(AppState {
        role_mappings: RoleMappings::parse(deployment.oidc.role_mappings.clone())?,
        config,
        deployment,
        provider,
        oidc_http,
        bearer_authorizer,
        backchannel_logout_authorizer,
        service_token_verifier,
        directory,
        pool,
        key_ring,
        owner_resolver,
        console_gateway,
        console_registry: console::ConsoleRegistry::default(),
        console_proxy_owner: "fixture".to_owned(),
        control_proxy,
        environment_proxy,
        evaluation_proxy,
        resource_proxy,
        runtime_proxy,
        metrics: METRICS
            .as_ref()
            .map_err(|error| std::io::Error::other(error.to_string()))?
            .clone(),
        nats: nats_boundary(tasks).await?,
    });
    Ok((state, authority.requests))
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn oidc_wrong_browser_cannot_consume_login_transaction() -> Result<(), Box<dyn Error>> {
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            container.get_host_port_ipv4(5432).await?
        ))
        .await?;
    support::apply_access_migrations(&pool).await?;
    let mut tasks = Tasks(Vec::new());
    let (state, _) = Box::pin(state(pool, Uuid::now_v7(), &mut tasks)).await?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let router = browser_router(state.clone());
    tasks.0.push(tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    }));
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let login = client.get(format!("{base}/auth/login")).send().await?;
    assert_eq!(login.status(), StatusCode::TEMPORARY_REDIRECT);
    let authorization = Url::parse(
        login
            .headers()
            .get(header::LOCATION)
            .ok_or("missing authorization redirect")?
            .to_str()?,
    )?;
    let returned_state = authorization
        .query_pairs()
        .find(|(name, _)| name == "state")
        .ok_or("missing authorization state")?
        .1
        .into_owned();
    let set_cookie = login
        .headers()
        .get(header::SET_COOKIE)
        .ok_or("login did not issue transaction cookie")?
        .to_str()?
        .to_owned();
    for attribute in [
        "__Host-labweaver_oidc_",
        "; Path=/",
        "; Secure",
        "; HttpOnly",
        "; SameSite=Lax",
        "; Max-Age=300",
    ] {
        assert!(set_cookie.contains(attribute));
    }
    assert_eq!(
        login
            .headers()
            .get(header::CACHE_CONTROL)
            .ok_or("missing cache policy")?,
        "no-store"
    );
    let cookie = set_cookie.split(';').next().ok_or("missing cookie pair")?;
    let cookie_name = cookie.split_once('=').ok_or("missing cookie value")?.0;
    let rejected = client
        .get(format!("{base}/auth/callback"))
        .query(&[("state", returned_state.as_str()), ("code", "invalid")])
        .send()
        .await?;
    let consumed: bool = sqlx::query_scalar(
        "SELECT consumed_at IS NOT NULL FROM access.oidc_transactions WHERE state_sha256=$1",
    )
    .bind(Sha256Digest::of_bytes(returned_state.as_bytes()).to_string())
    .fetch_one(&state.pool)
    .await?;
    assert!(
        !consumed,
        "a browser without transaction proof consumed another browser's login"
    );
    assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);
    assert_problem_response(
        rejected,
        StatusCode::UNAUTHORIZED,
        "LW_AUTH_OIDC_BROWSER_REJECTED",
    )
    .await?;
    // A forged cookie with the correct transaction name is still not browser proof.
    let rejected = client
        .get(format!("{base}/auth/callback"))
        .query(&[("state", returned_state.as_str()), ("code", "invalid")])
        .header(header::COOKIE, format!("{cookie_name}={returned_state}"))
        .send()
        .await?;
    assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);
    assert_problem_response(
        rejected,
        StatusCode::UNAUTHORIZED,
        "LW_AUTH_OIDC_BROWSER_REJECTED",
    )
    .await?;
    // Start another tab, then reject the first at the identity provider.
    let other = client.get(format!("{base}/auth/login")).send().await?;
    let other_cookie = other
        .headers()
        .get(header::SET_COOKIE)
        .ok_or("missing second tab cookie")?
        .to_str()?
        .split(';')
        .next()
        .ok_or("invalid cookie")?;
    assert_ne!(
        cookie.split_once('=').map(|p| p.0),
        other_cookie.split_once('=').map(|p| p.0)
    );
    let refused = client
        .get(format!("{base}/auth/callback"))
        .query(&[
            ("state", returned_state.as_str()),
            ("error", "access_denied"),
        ])
        .header(header::COOKIE, format!("{cookie}; {other_cookie}"))
        .send()
        .await?;
    assert_eq!(refused.status(), StatusCode::UNAUTHORIZED);
    let cleared = refused
        .headers()
        .get(header::SET_COOKIE)
        .ok_or("missing cookie cleanup")?
        .to_str()?;
    assert!(cleared.starts_with(&format!("{cookie_name}=")) && cleared.contains("Max-Age=0"));
    assert_eq!(
        refused.headers().get_all(header::SET_COOKIE).iter().count(),
        1
    );
    let live: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM access.oidc_transactions WHERE consumed_at IS NULL",
    )
    .fetch_one(&state.pool)
    .await?;
    assert_eq!(live, 1, "provider refusal invalidated a different tab");
    let replay = client
        .get(format!("{base}/auth/callback"))
        .query(&[("state", returned_state.as_str()), ("code", "invalid")])
        .header(header::COOKIE, cookie)
        .send()
        .await?;
    assert_eq!(replay.status(), StatusCode::UNAUTHORIZED);
    assert_problem_response(
        replay,
        StatusCode::UNAUTHORIZED,
        "LW_AUTH_OIDC_STATE_REJECTED",
    )
    .await?;
    for target in [
        "//attacker.invalid/",
        "/\\attacker.invalid/",
        "https://attacker.invalid/",
    ] {
        let response = client
            .get(format!("{base}/auth/login"))
            .query(&[("return_to", target)])
            .send()
            .await?;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(response.headers().get(header::SET_COOKIE).is_none());
    }
    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM access.oidc_transactions")
        .fetch_one(&state.pool)
        .await?;
    assert_eq!(
        total, 2,
        "invalid return target created an orphan login transaction"
    );
    Ok(())
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn session_cookie_headers_support_http2_and_reject_duplicates() -> Result<(), Box<dyn Error>>
{
    // The parser needs configuration only; it never connects to this lazy pool.
    let pool = PgPoolOptions::new().connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")?;
    let mut tasks = Tasks(Vec::new());
    let (state, _) = Box::pin(state(pool, Uuid::now_v7(), &mut tasks)).await?;
    let session_id = Uuid::now_v7();
    let mut headers = HeaderMap::new();
    headers.append(
        header::COOKIE,
        HeaderValue::from_static("__Host-labweaver_oidc_other=pending"),
    );
    headers.append(
        header::COOKIE,
        HeaderValue::from_str(&format!(
            "{}={session_id}",
            state.deployment.browser.session_cookie_name
        ))?,
    );
    assert_eq!(
        cookie_session_id(&state, &headers),
        Some(session_id),
        "HTTP/2 can split independent cookies into separate fields"
    );
    headers.append(
        header::COOKIE,
        HeaderValue::from_str(&format!(
            "{}={session_id}",
            state.deployment.browser.session_cookie_name
        ))?,
    );
    assert!(
        cookie_session_id(&state, &headers).is_none(),
        "duplicate session cookies must not choose a value"
    );
    Ok(())
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn browser_logout_rejects_csrf_and_returns_exact_provider_url_after_revocation()
-> Result<(), Box<dyn Error>> {
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            container.get_host_port_ipv4(5432).await?
        ))
        .await?;
    support::apply_access_migrations(&pool).await?;
    let mut tasks = Tasks(Vec::new());
    let actor = Uuid::now_v7();
    let (state, _requests) = Box::pin(state(pool, actor, &mut tasks)).await?;
    let browser_session = session(
        &state.pool,
        &state.key_ring,
        actor,
        contracts::PlatformRole::Teacher,
    )
    .await?;
    let cookie = format!(
        "{}={}",
        state.deployment.browser.session_cookie_name, browser_session.session_id
    );
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let router = browser_router(state.clone());
    tasks.0.push(tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    }));
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let logout_endpoint = format!("{base}/auth/logout");

    for (origin, csrf) in [
        (
            "https://attacker.example.invalid",
            browser_session.csrf_token.expose(),
        ),
        (ORIGIN, "wrong"),
    ] {
        let response = client
            .post(&logout_endpoint)
            .header(header::COOKIE, &cookie)
            .header(header::ORIGIN, origin)
            .header("x-csrf-token", csrf)
            .send()
            .await?;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_problem_response(response, StatusCode::FORBIDDEN, "LW_AUTH_CSRF_REJECTED").await?;
    }
    let still_active: bool = sqlx::query_scalar(
        "SELECT revoked_at IS NULL FROM access.bff_sessions WHERE session_id=$1",
    )
    .bind(browser_session.session_id)
    .fetch_one(&state.pool)
    .await?;
    assert!(still_active);

    let response = client
        .post(&logout_endpoint)
        .header(header::COOKIE, &cookie)
        .header(header::ORIGIN, ORIGIN)
        .header("x-csrf-token", browser_session.csrf_token.expose())
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next()),
        Some("application/json")
    );
    assert_eq!(
        response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some("no-store")
    );
    let set_cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|value| value.to_str().ok())
        .ok_or("logout did not clear the BFF cookie")?
        .to_owned();
    let body = response
        .json::<contracts::LogoutBrowserSessionResponse>()
        .await?;
    let provider_url = Url::parse(&body.logout_url)?;
    assert_eq!(provider_url.path(), "/realms/test/logout");
    let query = provider_url
        .query_pairs()
        .collect::<std::collections::BTreeMap<_, _>>();
    assert!(
        query
            .get("client_id")
            .is_some_and(|value| *value == state.deployment.oidc.client_id)
    );
    let expected_logout_hint = fixture_logout_hint();
    assert!(
        query
            .get("id_token_hint")
            .is_some_and(|value| *value == expected_logout_hint)
    );
    assert!(
        query
            .get("post_logout_redirect_uri")
            .is_some_and(|value| *value == state.config.post_logout_redirect_uri.as_str())
    );
    assert!(set_cookie.contains("Max-Age=0"));
    let revoked: bool = sqlx::query_scalar(
        "SELECT revoked_at IS NOT NULL FROM access.bff_sessions WHERE session_id=$1",
    )
    .bind(browser_session.session_id)
    .fetch_one(&state.pool)
    .await?;
    assert!(revoked);
    Ok(())
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one session-backed scenario protects route forwarding, identity, mutation and permission checks"
)]
async fn admin_upload_status_and_cancel_reach_control_with_session_auth()
-> Result<(), Box<dyn Error>> {
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            container.get_host_port_ipv4(5432).await?
        ))
        .await?;
    support::apply_access_migrations(&pool).await?;
    let mut tasks = Tasks(Vec::new());
    let admin = Uuid::now_v7();
    let (state, requests) = Box::pin(state(pool, admin, &mut tasks)).await?;
    let admin_session = session(
        &state.pool,
        &state.key_ring,
        admin,
        contracts::PlatformRole::PlatformAdmin,
    )
    .await?;
    let student_session = session(
        &state.pool,
        &state.key_ring,
        Uuid::now_v7(),
        contracts::PlatformRole::Student,
    )
    .await?;
    let cookie = format!(
        "{}={}",
        state.deployment.browser.session_cookie_name, admin_session.session_id
    );
    let student_cookie = format!(
        "{}={}",
        state.deployment.browser.session_cookie_name, student_session.session_id
    );
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let router = browser_router(state);
    tasks.0.push(tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    }));
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let path = format!("/api/v1/admin/images/uploads/{}", Uuid::now_v7());
    assert_eq!(
        client.get(format!("{base}{path}")).send().await?.status(),
        StatusCode::UNAUTHORIZED
    );
    let status = client
        .get(format!("{base}{path}"))
        .header(header::COOKIE, &cookie)
        .header(header::AUTHORIZATION, "Bearer spoofed")
        .header("x-labweaver-actor-id", student_session.actor_id.to_string())
        .send()
        .await?;
    assert_eq!(status.status(), StatusCode::OK);
    assert_eq!(status.json::<Value>().await?["state"], "importing");
    let cancel = format!("{base}{path}/cancel");
    for (origin, csrf) in [
        (None, Some(admin_session.csrf_token.expose())),
        (Some(ORIGIN), None),
        (
            Some("https://attacker.example.invalid"),
            Some(admin_session.csrf_token.expose()),
        ),
        (Some(ORIGIN), Some("wrong")),
    ] {
        let mut request = client
            .post(&cancel)
            .header(header::COOKIE, &cookie)
            .json(&json!({"expectedRevision":3}));
        if let Some(origin) = origin {
            request = request.header(header::ORIGIN, origin);
        }
        if let Some(csrf) = csrf {
            request = request.header("x-csrf-token", csrf);
        }
        assert_eq!(request.send().await?.status(), StatusCode::FORBIDDEN);
    }
    let accepted = client
        .post(&cancel)
        .header(header::COOKIE, &cookie)
        .header(header::ORIGIN, ORIGIN)
        .header("x-csrf-token", admin_session.csrf_token.expose())
        .header(header::IF_MATCH, "\"3\"")
        .header(
            "x-labweaver-session-id",
            student_session.session_id.to_string(),
        )
        .json(&json!({"expectedRevision":3}))
        .send()
        .await?;
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    assert_eq!(accepted.json::<Value>().await?["state"], "cancelling");
    assert_eq!(
        client
            .get(format!("{base}{path}"))
            .header(header::COOKIE, &student_cookie)
            .send()
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        client
            .get(&cancel)
            .header(header::COOKIE, &cookie)
            .send()
            .await?
            .status(),
        StatusCode::METHOD_NOT_ALLOWED
    );
    assert_eq!(
        client
            .post(format!("{base}{path}"))
            .header(header::COOKIE, &cookie)
            .send()
            .await?
            .status(),
        StatusCode::METHOD_NOT_ALLOWED
    );
    for suffix in ["/other", "/cancel/extra"] {
        assert_eq!(
            client
                .post(format!("{base}{path}{suffix}"))
                .header(header::COOKIE, &cookie)
                .send()
                .await?
                .status(),
            StatusCode::NOT_FOUND
        );
    }
    assert_eq!(
        client
            .get(format!("{base}/api/v1/admin/images/uploads/not-an-id"))
            .header(header::COOKIE, &cookie)
            .send()
            .await?
            .status(),
        StatusCode::BAD_REQUEST
    );
    let requests = requests.lock().await;
    assert_eq!(
        requests.len(),
        3,
        "rejected routes and mutations never reach Control"
    );
    assert_eq!(requests[0]["actor"], admin.to_string());
    assert_eq!(requests[1]["actor"], admin.to_string());
    assert_eq!(requests[1]["session"], admin_session.session_id.to_string());
    assert_eq!(requests[1]["body"], json!({"expectedRevision":3}));
    assert_eq!(requests[1]["ifMatch"], "\"3\"");
    assert_eq!(requests[1]["browserCookie"], false);
    assert_eq!(requests[1]["browserCsrf"], false);
    assert_eq!(requests[2]["actor"], student_session.actor_id.to_string());
    Ok(())
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one public-router scenario verifies session authentication, CSRF, delegation and exact build forwarding"
)]
async fn candidate_build_status_and_cancel_reach_control_with_session_auth()
-> Result<(), Box<dyn Error>> {
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            container.get_host_port_ipv4(5432).await?
        ))
        .await?;
    support::apply_access_migrations(&pool).await?;
    let mut tasks = Tasks(Vec::new());
    let teacher = Uuid::now_v7();
    let (state, requests) = Box::pin(state(pool, teacher, &mut tasks)).await?;
    let owner = session(
        &state.pool,
        &state.key_ring,
        teacher,
        contracts::PlatformRole::Teacher,
    )
    .await?;
    let other = session(
        &state.pool,
        &state.key_ring,
        Uuid::now_v7(),
        contracts::PlatformRole::Student,
    )
    .await?;
    let cookie = format!(
        "{}={}",
        state.deployment.browser.session_cookie_name, owner.session_id
    );
    let other_cookie = format!(
        "{}={}",
        state.deployment.browser.session_cookie_name, other.session_id
    );
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let router = browser_router(state);
    tasks.0.push(tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    }));
    let client = reqwest::Client::builder().no_proxy().build()?;
    let project = Uuid::now_v7();
    let candidate = Uuid::now_v7();
    let body = json!({"buildRequestId": Uuid::now_v7(), "expectedRevision": 3, "expectedState": "running"});
    let key = Uuid::now_v7().to_string();
    for target in ["environment", "evaluation_runner"] {
        let path = format!("/api/v1/projects/{project}/candidates/{candidate}/builds/{target}");
        let url = format!("{base}{path}");
        assert_eq!(
            client.get(&url).send().await?.status(),
            StatusCode::UNAUTHORIZED
        );
        let status = client
            .get(&url)
            .header(header::COOKIE, &cookie)
            .header(header::AUTHORIZATION, "Bearer spoofed")
            .header("x-labweaver-actor-id", other.actor_id.to_string())
            .send()
            .await?;
        assert_eq!(status.status(), StatusCode::OK);
        assert_eq!(
            status.json::<Value>().await?,
            json!({"status": {"state": "running", "revision": 3}})
        );
        let cancel = format!("{url}/cancel");
        for (origin, csrf) in [
            (None, Some(owner.csrf_token.expose())),
            (Some(ORIGIN), None),
            (Some(ORIGIN), Some("wrong")),
        ] {
            let mut request = client
                .post(&cancel)
                .header(header::COOKIE, &cookie)
                .json(&body);
            if let Some(origin) = origin {
                request = request.header(header::ORIGIN, origin);
            }
            if let Some(csrf) = csrf {
                request = request.header("x-csrf-token", csrf);
            }
            assert_eq!(request.send().await?.status(), StatusCode::FORBIDDEN);
        }
        let accepted = client
            .post(&cancel)
            .header(header::COOKIE, &cookie)
            .header(header::ORIGIN, ORIGIN)
            .header("x-csrf-token", owner.csrf_token.expose())
            .header(header::IF_MATCH, "\"rev-3\"")
            .header("Idempotency-Key", &key)
            .header("x-labweaver-session-id", other.session_id.to_string())
            .json(&body)
            .send()
            .await?;
        assert_eq!(accepted.status(), StatusCode::ACCEPTED);
        assert_eq!(
            accepted.json::<Value>().await?,
            json!({"status": {"state": "running", "revision": 4, "cancellationRequested": true}})
        );
        let conflict = client.post(&cancel)
            .header(header::COOKIE, &cookie)
            .header(header::ORIGIN, ORIGIN)
            .header("x-csrf-token", owner.csrf_token.expose())
            .header(header::IF_MATCH, "\"rev-2\"")
            .header("Idempotency-Key", Uuid::now_v7().to_string())
            .json(&json!({"buildRequestId": body["buildRequestId"], "expectedRevision": 2, "expectedState": "running"}))
            .send().await?;
        assert_eq!(conflict.status(), StatusCode::CONFLICT);
        assert_eq!(
            conflict.json::<Value>().await?,
            json!({"diagnosticCode": "LW_AGENT_BUILD_REVISION_CONFLICT", "retryable": false})
        );
        assert_eq!(
            client
                .get(&url)
                .header(header::COOKIE, &other_cookie)
                .send()
                .await?
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            client
                .get(&cancel)
                .header(header::COOKIE, &cookie)
                .send()
                .await?
                .status(),
            StatusCode::METHOD_NOT_ALLOWED
        );
        assert_eq!(
            client
                .post(&url)
                .header(header::COOKIE, &cookie)
                .send()
                .await?
                .status(),
            StatusCode::METHOD_NOT_ALLOWED
        );
        assert_eq!(
            client
                .post(format!("{cancel}/extra"))
                .header(header::COOKIE, &cookie)
                .send()
                .await?
                .status(),
            StatusCode::NOT_FOUND
        );
    }
    let requests = requests.lock().await;
    assert_eq!(
        requests.len(),
        8,
        "rejected browser requests never reach Control"
    );
    for (records, target) in requests
        .chunks_exact(4)
        .zip(["environment", "evaluation_runner"])
    {
        let path = format!("/api/v1/projects/{project}/candidates/{candidate}/builds/{target}");
        assert_eq!(records[0]["path"], path);
        assert_eq!(records[0]["actor"], teacher.to_string());
        assert_eq!(records[1]["path"], format!("{path}/cancel"));
        assert_eq!(records[1]["method"], "POST");
        assert_eq!(records[1]["body"], body);
        assert_eq!(records[1]["ifMatch"], "\"rev-3\"");
        assert_eq!(records[1]["idempotencyKey"], key);
        assert_eq!(records[1]["actor"], teacher.to_string());
        assert_eq!(records[1]["session"], owner.session_id.to_string());
        assert_eq!(records[1]["browserCookie"], false);
        assert_eq!(records[1]["browserCsrf"], false);
        assert_eq!(records[2]["body"]["expectedRevision"], 2);
        assert_eq!(records[3]["actor"], other.actor_id.to_string());
    }
    Ok(())
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn project_usage_reaches_resource_with_session_auth() -> Result<(), Box<dyn Error>> {
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            container.get_host_port_ipv4(5432).await?
        ))
        .await?;
    support::apply_access_migrations(&pool).await?;
    let mut tasks = Tasks(Vec::new());
    let admin = Uuid::now_v7();
    let (state, requests) = Box::pin(state(pool, admin, &mut tasks)).await?;
    let admin_session = session(
        &state.pool,
        &state.key_ring,
        admin,
        contracts::PlatformRole::PlatformAdmin,
    )
    .await?;
    let student_session = session(
        &state.pool,
        &state.key_ring,
        Uuid::now_v7(),
        contracts::PlatformRole::Student,
    )
    .await?;
    let cookie = format!(
        "{}={}",
        state.deployment.browser.session_cookie_name, admin_session.session_id
    );
    let student_cookie = format!(
        "{}={}",
        state.deployment.browser.session_cookie_name, student_session.session_id
    );
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let router = browser_router(state);
    tasks.0.push(tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    }));
    let client = reqwest::Client::builder().no_proxy().build()?;
    let project_id = Uuid::now_v7();
    let path = format!("/api/v1/projects/{project_id}/usage?page=1&pageSize=1");

    assert_eq!(
        client.get(format!("{base}{path}")).send().await?.status(),
        StatusCode::UNAUTHORIZED
    );
    let denied = client
        .get(format!("{base}{path}"))
        .header(header::COOKIE, student_cookie)
        .send()
        .await?;
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    assert_problem_response(denied, StatusCode::FORBIDDEN, "LW_AUTH_SCOPE_DENIED").await?;
    let response = client
        .get(format!("{base}{path}"))
        .header(header::COOKIE, &cookie)
        .send()
        .await?;
    let response_status = response.status();
    let response_body = response.text().await?;
    assert_eq!(response_status, StatusCode::OK, "{response_body}");
    assert_eq!(
        serde_json::from_str::<Value>(&response_body)?,
        json!({"state": "importing", "revision": 3})
    );

    let requests = requests.lock().await;
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0]["path"],
        format!("/api/v1/projects/{project_id}/usage")
    );
    assert_eq!(requests[0]["query"], "page=1&pageSize=1");
    assert_eq!(requests[0]["method"], "GET");
    assert_eq!(requests[0]["actor"], admin.to_string());
    assert_eq!(requests[0]["upstream"], "resource");
    Ok(())
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "the browser scenario covers two authorized upstreams and two denied roles"
)]
async fn platform_admin_project_reads_reach_their_distinct_upstreams_without_membership()
-> Result<(), Box<dyn Error>> {
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            container.get_host_port_ipv4(5432).await?
        ))
        .await?;
    support::apply_access_migrations(&pool).await?;
    let mut tasks = Tasks(Vec::new());
    let admin = Uuid::now_v7();
    let (state, requests) = Box::pin(state(pool, admin, &mut tasks)).await?;
    let admin_session = session(
        &state.pool,
        &state.key_ring,
        admin,
        contracts::PlatformRole::PlatformAdmin,
    )
    .await?;
    let student = Uuid::now_v7();
    let student_session = session(
        &state.pool,
        &state.key_ring,
        student,
        contracts::PlatformRole::Student,
    )
    .await?;
    let project_id = Uuid::now_v7();
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let router = browser_router(state.clone());
    tasks.0.push(tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    }));
    let client = reqwest::Client::builder().no_proxy().build()?;
    let admin_cookie = format!(
        "{}={}",
        state.deployment.browser.session_cookie_name, admin_session.session_id
    );
    let student_cookie = format!(
        "{}={}",
        state.deployment.browser.session_cookie_name, student_session.session_id
    );

    let project = client
        .get(format!("{base}/api/v1/projects/{project_id}"))
        .header(header::COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(project.status(), StatusCode::OK);

    let members = client
        .get(format!("{base}/api/v1/projects/{project_id}/members"))
        .header(header::COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(members.status(), StatusCode::OK);
    assert_eq!(
        members.json::<Value>().await?,
        json!([{
            "actorId": admin,
            "courseId": null,
            "username": "admin",
            "displayName": "Platform Admin",
            "role": "teacher",
            "state": "active",
            "revision": 1,
            "expiresAt": null
        }])
    );

    let environments = client
        .get(format!(
            "{base}/api/v1/environments?projectId={project_id}&class=work&limit=10"
        ))
        .header(header::COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(environments.status(), StatusCode::OK);
    assert_eq!(
        environments.json::<Value>().await?,
        json!({
            "items": [],
            "nextCursor": null,
            "snapshotSequence": 1,
            "snapshotAt": "2026-01-01T00:00:00.000Z"
        })
    );

    for path in [
        format!("/api/v1/projects/{project_id}/members"),
        format!("/api/v1/environments?projectId={project_id}&class=work&limit=10"),
    ] {
        let response = client
            .get(format!("{base}{path}"))
            .header(header::COOKIE, &student_cookie)
            .send()
            .await?;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        if path.starts_with("/api/v1/environments") {
            assert_problem_response(response, StatusCode::FORBIDDEN, "LW_AUTH_SCOPE_DENIED")
                .await?;
        } else {
            // The external Control fixture's response is forwarded unchanged.
            assert_eq!(
                response.json::<Value>().await?,
                json!({"diagnosticCode": "LW_AUTH_SCOPE_DENIED"})
            );
        }
    }

    let requests = requests.lock().await;
    assert_eq!(requests.len(), 4);
    assert!(requests.iter().any(|request| {
        request["upstream"] == "control"
            && request["path"] == format!("/api/v1/projects/{project_id}")
            && request["method"] == "GET"
            && request["actor"] == admin.to_string()
    }));
    assert!(requests.iter().any(|request| {
        request["upstream"] == "control"
            && request["path"] == format!("/api/v1/projects/{project_id}/members")
            && request["method"] == "GET"
            && request["actor"] == admin.to_string()
    }));
    assert!(requests.iter().any(|request| {
        request["upstream"] == "environment"
            && request["path"] == "/api/v1/environments"
            && request["query"] == format!("projectId={project_id}&class=work&limit=10")
            && request["method"] == "GET"
            && request["actor"] == admin.to_string()
    }));
    assert!(requests.iter().any(|request| {
        request["upstream"] == "control"
            && request["path"] == format!("/api/v1/projects/{project_id}/members")
            && request["actor"] == student.to_string()
    }));
    assert!(!requests.iter().any(|request| {
        request["upstream"] == "environment" && request["actor"] == student.to_string()
    }));
    Ok(())
}

#[tokio::test]
async fn non_owner_membership_add_is_rejected_before_directory_resolution()
-> Result<(), Box<dyn Error>> {
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            container.get_host_port_ipv4(5432).await?
        ))
        .await?;
    support::apply_access_migrations(&pool).await?;
    let mut tasks = Tasks(Vec::new());
    let owner = Uuid::now_v7();
    let (state, requests) = Box::pin(state(pool, owner, &mut tasks)).await?;
    let owner_session = session(
        &state.pool,
        &state.key_ring,
        owner,
        contracts::PlatformRole::Teacher,
    )
    .await?;
    let member = Uuid::now_v7();
    let member_session = session(
        &state.pool,
        &state.key_ring,
        member,
        contracts::PlatformRole::Student,
    )
    .await?;
    let project_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO control.project_memberships \
         (course_id,project_id,actor_id,role,state,revision) \
         VALUES (NULL,$1,$2,'student','active',1)",
    )
    .bind(project_id)
    .bind(member)
    .execute(&state.pool)
    .await?;
    let actor_count_before: i64 = sqlx::query_scalar("SELECT count(*) FROM access.actors")
        .fetch_one(&state.pool)
        .await?;

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let router = browser_router(state.clone());
    tasks.0.push(tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    }));
    let client = reqwest::Client::builder().no_proxy().build()?;
    let cookie = format!(
        "{}={}",
        state.deployment.browser.session_cookie_name, member_session.session_id
    );
    let response = client
        .post(format!("{base}/api/v1/projects/{project_id}/members"))
        .header(header::COOKIE, cookie)
        .header(header::ORIGIN, ORIGIN)
        .header("x-csrf-token", member_session.csrf_token.expose())
        .header("Idempotency-Key", Uuid::now_v7().to_string())
        .json(&json!({"username": "target-user", "role": "student", "expiresAt": null}))
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_problem_response(response, StatusCode::FORBIDDEN, "LW_AUTH_SCOPE_DENIED").await?;

    let actor_count_after: i64 = sqlx::query_scalar("SELECT count(*) FROM access.actors")
        .fetch_one(&state.pool)
        .await?;
    assert_eq!(actor_count_after, actor_count_before);
    let requests = requests.lock().await;
    assert!(requests.iter().any(|request| {
        request["method"] == "GET" && request["path"] == format!("/api/v1/projects/{project_id}")
    }));
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["path"] == "/admin/realms/test/users")
            .count(),
        0
    );
    assert_ne!(owner_session.session_id, member_session.session_id);
    Ok(())
}

#[tokio::test]
async fn organization_directory_is_teacher_visible_and_student_denied_without_lookup()
-> Result<(), Box<dyn Error>> {
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            container.get_host_port_ipv4(5432).await?
        ))
        .await?;
    support::apply_access_migrations(&pool).await?;
    let mut tasks = Tasks(Vec::new());
    let admin = Uuid::now_v7();
    let (state, requests) = Box::pin(state(pool, admin, &mut tasks)).await?;
    let teacher_session = session(
        &state.pool,
        &state.key_ring,
        Uuid::now_v7(),
        contracts::PlatformRole::Teacher,
    )
    .await?;
    let student_session = session(
        &state.pool,
        &state.key_ring,
        Uuid::now_v7(),
        contracts::PlatformRole::Student,
    )
    .await?;
    let actor_count_before: i64 = sqlx::query_scalar("SELECT count(*) FROM access.actors")
        .fetch_one(&state.pool)
        .await?;

    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let router = browser_router(state.clone());
    tasks.0.push(tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    }));
    let client = reqwest::Client::builder().no_proxy().build()?;
    let path = "/api/v1/directory/users?query=alice&page=1&pageSize=10";
    let teacher_cookie = format!(
        "{}={}",
        state.deployment.browser.session_cookie_name, teacher_session.session_id
    );
    let teacher_response = client
        .get(format!("{base}{path}"))
        .header(header::COOKIE, teacher_cookie)
        .send()
        .await?;
    let teacher_status = teacher_response.status();
    let teacher_body = teacher_response.text().await?;
    assert_eq!(teacher_status, StatusCode::OK, "{teacher_body}");
    let page = serde_json::from_str::<contracts::OrganizationUserPage>(&teacher_body)?;
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].username, "alice");
    assert_eq!(page.items[0].display_name, "Alice Researcher");
    assert!(page.items[0].enabled);
    let default_response = client
        .get(format!("{base}/api/v1/directory/users?query=alice"))
        .header(
            header::COOKIE,
            format!(
                "{}={}",
                state.deployment.browser.session_cookie_name, teacher_session.session_id
            ),
        )
        .send()
        .await?;
    assert_eq!(default_response.status(), StatusCode::OK);
    let default_page = default_response
        .json::<contracts::OrganizationUserPage>()
        .await?;
    assert_eq!((default_page.page, default_page.page_size), (1, 25));

    let student_cookie = format!(
        "{}={}",
        state.deployment.browser.session_cookie_name, student_session.session_id
    );
    let student_response = client
        .get(format!("{base}{path}"))
        .header(header::COOKIE, student_cookie)
        .send()
        .await?;
    assert_eq!(student_response.status(), StatusCode::FORBIDDEN);
    assert_problem_response(
        student_response,
        StatusCode::FORBIDDEN,
        "LW_AUTH_SCOPE_DENIED",
    )
    .await?;

    let actor_count_after: i64 = sqlx::query_scalar("SELECT count(*) FROM access.actors")
        .fetch_one(&state.pool)
        .await?;
    assert_eq!(actor_count_after, actor_count_before);
    let requests = requests.lock().await;
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["path"] == "/admin/realms/test/users")
            .count(),
        2
    );
    Ok(())
}

async fn state(
    pool: PgPool,
    admin: Uuid,
    tasks: &mut Tasks,
) -> Result<(Arc<AppState>, Arc<Mutex<Vec<Value>>>), Box<dyn Error>> {
    Box::pin(state_with_environment(pool, admin, tasks, None)).await
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn owned_environment_requests_use_current_control_actions_and_reject_removed_creator()
-> Result<(), Box<dyn Error>> {
    use contracts::environment::{
        DesiredEnvironmentState, EnvironmentInstance, EnvironmentOperation,
        EnvironmentOperationKind, ObservedEnvironmentState, OperationState,
    };
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            container.get_host_port_ipv4(5432).await?
        ))
        .await?;
    support::apply_access_migrations(&pool).await?;
    let actor = Uuid::now_v7();
    let project = contracts::ProjectId::new();
    let environment_id = contracts::EnvironmentId::new();
    let course = contracts::CourseId::new();
    let timestamp: contracts::UtcTimestamp = "2026-10-12T00:00:00.000Z".parse()?;
    let instance = EnvironmentInstance {
        runtime_authorization: None,
        id: environment_id,
        project_id: project,
        course_id: Some(course),
        owner_id: actor.to_string().parse()?,
        display_label: "synthetic owner boundary".to_owned(),
        class: contracts::authoring::EnvironmentClass::Experiment,
        runtime_kind: contracts::authoring::RuntimeKind::Container,
        release_id: contracts::ReleaseId::new(),
        release_version: 1,
        lease_id: None,
        capacity_binding: None,
        approved_resources: contracts::resource::WorkloadResources {
            cpu_millicores: 1000,
            memory_bytes: 1_073_741_824,
            storage_bytes: 2_147_483_648,
            gpu: None,
        },
        provider_binding: "container-primary-v1".to_owned(),
        desired_state: DesiredEnvironmentState::Running,
        observed_state: ObservedEnvironmentState::Requested,
        revision: contracts::Revision::new(1)?,
        generation: 1,
        observed_generation: 0,
        operation: EnvironmentOperation {
            id: contracts::OperationId::new(),
            kind: EnvironmentOperationKind::Create,
            state: OperationState::Accepted,
            accepted_revision: contracts::Revision::new(1)?,
            attempt: 1,
            provider_step: 1,
            max_attempts: 3,
            next_attempt_at: timestamp,
            actor_id: actor.to_string().parse()?,
            trace_id: "synthetic-199".to_owned(),
            accepted_at: timestamp,
            deadline_at: "2026-10-12T00:10:00.000Z".parse()?,
            cleanup_started_at: None,
            diagnostic_code: None,
            preserve_mutable_disk: false,
            access_revocation_revision: None,
            retry_from_phase: None,
            reset_target: None,
            lease_authorization: None,
        },
        gpu_allocation: None,
        resource_reservation_released: false,
        eligibility_expires_at: Some("2026-10-13T00:00:00.000Z".parse()?),
        endpoints: Vec::new(),
        last_diagnostic_code: None,
        failed_phase: None,
        cleanup_evidence: None,
    };
    instance.validate()?;
    let mut tasks = Tasks(Vec::new());
    let (state, requests) = Box::pin(state_with_environment(
        pool.clone(),
        actor,
        &mut tasks,
        Some(instance),
    ))
    .await?;
    let bff = session(
        &pool,
        &state.key_ring,
        actor,
        contracts::PlatformRole::Student,
    )
    .await?;
    sqlx::query("INSERT INTO control.project_memberships(project_id,actor_id,course_id,role,object_role,state,revision) VALUES($1,$2,$3,'student','viewer','active',1)")
        .bind(project.as_uuid()).bind(actor).bind(course.as_uuid()).execute(&pool).await?;
    sqlx::query("INSERT INTO control.course_memberships(course_id,actor_id,role,state,revision) VALUES($1,$2,'student','active',1)")
        .bind(course.as_uuid()).bind(actor).execute(&pool).await?;
    // A more permissive unrelated project and a forged query cannot replace the Environment's project.
    let unrelated = contracts::ProjectId::new();
    sqlx::query("INSERT INTO control.project_memberships(project_id,actor_id,role,object_role,state,revision) VALUES($1,$2,'student','owner','active',1)")
        .bind(unrelated.as_uuid()).bind(actor).execute(&pool).await?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let app = browser_router(state.clone());
    tasks.0.push(tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    }));
    let client = reqwest::Client::builder().no_proxy().build()?;
    let cookie = format!(
        "{}={}",
        state.deployment.browser.session_cookie_name, bff.session_id
    );
    let endpoint =
        format!("{base}/api/v1/environments/{environment_id}/stop?projectId={unrelated}");
    let stop = || {
        client
            .post(&endpoint)
            .header(header::COOKIE, &cookie)
            .header(header::ORIGIN, ORIGIN)
            .header("x-csrf-token", bff.csrf_token.expose())
            .header("idempotency-key", "synthetic-stop")
            .header(header::IF_MATCH, "\"1\"")
            .json(&json!({"expectedRevision":1}))
    };
    assert_eq!(stop().send().await?.status(), StatusCode::FORBIDDEN);
    assert!(
        requests.lock().await.iter().all(|r| r["method"] != "POST"),
        "a viewer must not forward a mutation to the Environment boundary"
    );
    sqlx::query("UPDATE control.project_memberships SET object_role='operator',revision=2 WHERE project_id=$1 AND actor_id=$2")
        .bind(project.as_uuid()).bind(actor).execute(&pool).await?;
    assert_eq!(stop().send().await?.status(), StatusCode::ACCEPTED);
    assert_eq!(
        requests
            .lock()
            .await
            .iter()
            .filter(|r| r["method"] == "POST")
            .count(),
        1
    );
    let start = client
        .post(format!("{base}/api/v1/environments/{environment_id}/start"))
        .header(header::COOKIE, &cookie)
        .header(header::ORIGIN, ORIGIN)
        .header("x-csrf-token", bff.csrf_token.expose())
        .header("idempotency-key", "synthetic-start")
        .header(header::IF_MATCH, "\"1\"")
        .json(&json!({"expectedRevision":1}))
        .send()
        .await?;
    assert_eq!(
        start.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "new admission requires a verified applied policy"
    );
    assert_eq!(
        requests
            .lock()
            .await
            .iter()
            .filter(|r| r["method"] == "POST")
            .count(),
        1,
        "policy denial must not reach Environment"
    );
    assert_eq!(
        stop().send().await?.status(),
        StatusCode::ACCEPTED,
        "stopping remains available while no applied policy is verified"
    );
    sqlx::query("UPDATE control.course_memberships SET state='revoked',revision=2 WHERE course_id=$1 AND actor_id=$2")
        .bind(course.as_uuid()).bind(actor).execute(&pool).await?;
    assert_eq!(
        stop().send().await?.status(),
        StatusCode::FORBIDDEN,
        "a project role cannot replace revoked course scope"
    );
    sqlx::query("UPDATE control.course_memberships SET state='active',revision=3 WHERE course_id=$1 AND actor_id=$2")
        .bind(course.as_uuid()).bind(actor).execute(&pool).await?;
    sqlx::query("UPDATE control.project_memberships SET state='revoked',revision=3 WHERE project_id=$1 AND actor_id=$2")
        .bind(project.as_uuid()).bind(actor).execute(&pool).await?;
    assert_eq!(
        stop().send().await?.status(),
        StatusCode::FORBIDDEN,
        "owning the old instance cannot survive Control member removal"
    );
    assert_eq!(
        requests
            .lock()
            .await
            .iter()
            .filter(|r| r["method"] == "POST")
            .count(),
        2
    );
    Ok(())
}
