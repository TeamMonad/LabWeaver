//! Browser-router coverage with persisted sessions and external service boundaries.

use std::{
    collections::BTreeSet,
    error::Error,
    sync::{Arc, LazyLock},
};

use auth::{CreateBffSession, ServiceTokenClient, ServiceTokenClientConfig};
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

use super::*;

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

#[derive(Clone)]
struct Authority {
    issuer: String,
    signing_key: Arc<EncodingKey>,
    decoding_key: DecodingKey,
    jwk: Value,
    admin: Uuid,
    requests: Arc<Mutex<Vec<Value>>>,
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
    let actor = headers
        .get("x-labweaver-actor-id")
        .and_then(|v| v.to_str().ok());
    let record = json!({
        "method": method.as_str(), "path": uri.path(), "actor": actor,
        "session": headers.get("x-labweaver-session-id").and_then(|v| v.to_str().ok()),
        "ifMatch": headers.get(header::IF_MATCH).and_then(|v| v.to_str().ok()),
        "idempotencyKey": headers.get("idempotency-key").and_then(|v| v.to_str().ok()),
        "body": serde_json::from_slice::<Value>(&body).ok(),
        "browserCookie": headers.contains_key(header::COOKIE),
        "browserCsrf": headers.contains_key("x-csrf-token")
    });
    state.requests.lock().await.push(record);
    if actor != Some(state.admin.to_string().as_str()) {
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
            logout_hint: "fixture-logout".to_owned(),
        },
        now,
    )
    .await?)
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
async fn state(
    pool: PgPool,
    admin: Uuid,
    tasks: &mut Tasks,
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
        .with_state(authority.clone());
    tasks.0.push(tokio::spawn(async move {
        let _ = http_transport::serve_tls(listener, router, server_config).await;
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
    deployment.environment_owner_resolver.resolver_uri = base.trim_end_matches('/').to_owned();
    deployment.resource_gateway.base_uri.clone_from(&base);
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
    let environment_proxy = control_proxy.clone();
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
        token_client,
        target,
    )?;
    let runtime_proxy = proxy::RuntimeGatewayProxy::new(&deployment.environment_gateway)?;
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
