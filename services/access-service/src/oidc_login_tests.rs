//! Real Keycloak, HTTPS cookie stores, and `PostgreSQL` through the production BFF router.

use std::time::Duration as StdDuration;
use std::{path::Path, process::Stdio};

use scraper::{Html, Selector};
use testcontainers::{
    GenericImage,
    core::{IntoContainerPort, WaitFor},
};

use super::*;

#[tokio::test]
#[ignore = "requires installed Playwright Chromium, Node, Docker, real Keycloak and PostgreSQL"]
async fn chromium_oidc_navigation_enforces_browser_binding_and_session_lifecycle()
-> Result<(), Box<dyn Error>> {
    let fixture = real_oidc_fixture("localhost").await?;
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/oidc_browser.mjs");
    let mut child = tokio::process::Command::new("node")
        .arg(script)
        .arg(&fixture.base)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()?;
    let mut lines = BufReader::new(child.stdout.take().ok_or("missing browser stdout")?).lines();
    let mut completed = false;
    while let Some(line) = lines.next_line().await? {
        let message: Value = serde_json::from_str(&line)?;
        match message["op"].as_str() {
            Some("expire") => {
                let hash = message["stateSha256"]
                    .as_str()
                    .ok_or("missing state hash")?;
                assert!(hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()));
                let updated = sqlx::query("UPDATE access.oidc_transactions SET created_at=now()-interval '10 minutes',expires_at=now()-interval '1 second' WHERE state_sha256=$1 AND consumed_at IS NULL")
                    .bind(hash).execute(&fixture.app.pool).await?.rows_affected();
                assert_eq!(updated, 1);
                child
                    .stdin
                    .as_mut()
                    .ok_or("missing browser stdin")?
                    .write_all(b"{\"ok\":true}\n")
                    .await?;
            }
            Some("complete") => {
                completed = true;
                println!("Chromium OIDC navigation passed: {message}");
            }
            _ => return Err("unexpected browser probe message".into()),
        }
    }
    assert!(child.wait().await?.success(), "Chromium OIDC probe failed");
    assert!(completed, "browser probe never completed its assertions");
    let sessions: i64 = sqlx::query_scalar("SELECT count(*) FROM access.bff_sessions")
        .fetch_one(&fixture.app.pool)
        .await?;
    assert_eq!(sessions, 2, "wrong browser or replay created a session");
    let transactions: i64 = sqlx::query_scalar("SELECT count(*) FROM access.oidc_transactions")
        .fetch_one(&fixture.app.pool)
        .await?;
    assert_eq!(
        transactions, 3,
        "invalid return target created a transaction"
    );
    Ok(())
}

async fn provider_callback(
    client: &reqwest::Client,
    authorization: &str,
    callback_uri: &str,
) -> Result<Url, Box<dyn Error>> {
    let response = client.get(authorization).send().await?;
    let mut response = if response.status().is_redirection() {
        response
    } else {
        let page = response.error_for_status()?.text().await?;
        let action = {
            let document = Html::parse_document(&page);
            let selector =
                Selector::parse("form#kc-form-login").map_err(|_| "invalid login selector")?;
            document
                .select(&selector)
                .next()
                .and_then(|form| form.value().attr("action"))
                .ok_or("Keycloak login form missing")?
                .to_owned()
        };
        client
            .post(action)
            .form(&[
                ("username", "teacher"),
                ("password", "test-only-password"),
                ("credentialId", ""),
            ])
            .send()
            .await?
    };
    let issuer_origin = Url::parse(authorization)?.origin();
    for _ in 0..8 {
        assert!(
            response.status().is_redirection(),
            "Keycloak did not issue a redirect"
        );
        let location = Url::parse(
            response
                .headers()
                .get(header::LOCATION)
                .ok_or("Keycloak redirect missing")?
                .to_str()?,
        )?;
        if location.as_str().starts_with(&format!("{callback_uri}?")) {
            return Ok(location);
        }
        assert_eq!(location.origin(), issuer_origin);
        response = client.get(location).send().await?;
    }
    Err("Keycloak exceeded redirect limit".into())
}

fn browser_client(ca: &str, provider_ca: &str) -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .no_proxy()
        .cookie_store(true)
        .add_root_certificate(reqwest::Certificate::from_pem(ca.as_bytes())?)
        .add_root_certificate(reqwest::Certificate::from_pem(provider_ca.as_bytes())?)
        .redirect(reqwest::redirect::Policy::none())
        .build()
}

pub(super) struct OidcFixture {
    _tasks: Tasks,
    pub(super) app: Arc<AppState>,
    pub(super) base: String,
    callback_uri: String,
    portal_ca: String,
    provider_ca: String,
    _keycloak: testcontainers::ContainerAsync<GenericImage>,
    _postgres: testcontainers::ContainerAsync<Postgres>,
}

async fn navigation_destination() -> axum::response::Html<&'static str> {
    axum::response::Html(
        "<!doctype html><html lang=\"en\"><head><title>OIDC navigation probe</title></head><body><main><h1>OIDC navigation probe</h1><p>This test landing page contains no product UI or business state.</p></main></body></html>",
    )
}

#[allow(
    clippy::too_many_lines,
    reason = "reuse real OIDC infrastructure for protocol and browser engine tests"
)]
async fn real_oidc_fixture(portal_host: &str) -> Result<OidcFixture, Box<dyn Error>> {
    real_oidc_fixture_with(
        portal_host,
        |_| Ok(()),
        async |_, _, _| Ok(()),
        Router::new(),
    )
    .await
}

#[allow(
    clippy::too_many_lines,
    reason = "one fixture owns real Keycloak and browser TLS setup"
)]
pub(super) async fn real_oidc_fixture_with(
    portal_host: &str,
    configure_realm: impl FnOnce(&mut Value) -> Result<(), Box<dyn Error>>,
    configure_app: impl AsyncFnOnce(&mut AppState, &str, &mut Tasks) -> Result<(), Box<dyn Error>>,
    extra_routes: Router,
) -> Result<OidcFixture, Box<dyn Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("https://{portal_host}:{}", listener.local_addr()?.port());
    let callback_uri = format!("{base}/auth/callback");
    let browser_tls =
        rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_owned(), "localhost".to_owned()])?;
    let portal_ca = browser_tls.cert.pem();
    let browser_server = http_transport::server_config(
        portal_ca.as_bytes(),
        browser_tls.signing_key.serialize_pem().as_bytes(),
    )?;
    let provider_tls = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_owned()])?;
    let provider_ca = provider_tls.cert.pem();
    let mut realm: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/keycloak/labweaver-test-realm.json"
    ))?;
    realm["clients"][0]["redirectUris"] = json!([callback_uri]);
    realm["clients"][0]["webOrigins"] = json!([base]);
    realm["clients"][0]["attributes"]["post.logout.redirect.uris"] = json!(format!("{base}/"));
    configure_realm(&mut realm)?;
    // Pinned to the same real authority release as deploy/versions.lock.yml.
    let keycloak = GenericImage::new(
        "docker.io/keycloak/keycloak",
        "26.7.0@sha256:1362a9d9f13ab325231ea133610cc905e12805804abc7acbef552dd613720aa6",
    )
    .with_exposed_port(8443.tcp())
    .with_wait_for(WaitFor::message_on_stdout("Listening on:"))
    .with_env_var("JAVA_OPTS_KC_HEAP", "-Xms128m -Xmx512m")
    .with_copy_to(
        "/opt/keycloak/conf/test.crt",
        provider_ca.as_bytes().to_vec(),
    )
    .with_copy_to(
        "/opt/keycloak/conf/test.key",
        provider_tls.signing_key.serialize_pem().into_bytes(),
    )
    .with_copy_to(
        "/opt/keycloak/data/import/labweaver-test-realm.json",
        serde_json::to_vec(&realm)?,
    )
    .with_cmd([
        "start-dev",
        "--http-enabled=false",
        "--https-port=8443",
        "--hostname-strict=false",
        "--https-certificate-file=/opt/keycloak/conf/test.crt",
        "--https-certificate-key-file=/opt/keycloak/conf/test.key",
        "--import-realm",
    ])
    .with_startup_timeout(StdDuration::from_mins(2))
    .start()
    .await?;
    let issuer = format!(
        "https://127.0.0.1:{}/realms/labweaver-test",
        keycloak.get_host_port_ipv4(8443).await?
    );
    let postgres = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            postgres.get_host_port_ipv4(5432).await?
        ))
        .await?;
    support::apply_access_migrations(&pool).await?;
    let mut tasks = Tasks(Vec::new());
    let (fixture, _) = Box::pin(state(pool, Uuid::now_v7(), &mut tasks)).await?;
    let mut app = (*fixture).clone();
    app.config = AuthConfig::new(
        &issuer,
        "labweaver-web".to_owned(),
        &callback_uri,
        &format!("{base}/"),
        "labweaver-api".to_owned(),
        BTreeSet::from([base.clone()]),
        900,
    )?;
    app.deployment.oidc.issuer = issuer;
    app.deployment.oidc.redirect_uri.clone_from(&callback_uri);
    app.deployment.oidc.post_logout_redirect_uri = format!("{base}/");
    app.deployment.oidc.jwt_algorithms = BTreeSet::from(["RS256".to_owned()]);
    app.oidc_http =
        no_redirect_http_client(Some(provider_ca.as_bytes()), TransportSecurityMode::Strict)?;
    app.provider =
        OidcProvider::discover_with_http_client(&app.config, app.oidc_http.clone()).await?;
    app.bearer_authorizer = Arc::new(
        build_bearer_authorizer(&app.config, &app.deployment.oidc, app.oidc_http.clone()).await?,
    );
    app.backchannel_logout_authorizer = Arc::new(
        build_backchannel_logout_authorizer(
            &app.config,
            &app.deployment.oidc,
            app.oidc_http.clone(),
        )
        .await?,
    );
    configure_app(&mut app, &provider_ca, &mut tasks).await?;
    let app = Arc::new(app);
    let router = browser_router(app.clone())
        .route("/", get(navigation_destination))
        .route("/oidc-probe", get(navigation_destination))
        .route("/favicon.ico", get(|| async { StatusCode::NO_CONTENT }))
        .merge(extra_routes);
    tasks.0.push(tokio::spawn(async move {
        let _ = http_transport::serve_tls(listener, router, browser_server).await;
    }));
    Ok(OidcFixture {
        _tasks: tasks,
        app,
        base,
        callback_uri,
        portal_ca,
        provider_ca,
        _keycloak: keycloak,
        _postgres: postgres,
    })
}

#[tokio::test]
#[ignore = "starts disposable real Keycloak and PostgreSQL; run explicitly for OIDC integration"]
#[allow(
    clippy::too_many_lines,
    reason = "one real OIDC journey protects browser binding and session lifecycle"
)]
async fn real_keycloak_binds_two_browsers_and_preserves_tabs_replay_expiry_and_logout()
-> Result<(), Box<dyn Error>> {
    let fixture = real_oidc_fixture("127.0.0.1").await?;
    let OidcFixture {
        app,
        base,
        callback_uri,
        portal_ca,
        provider_ca,
        _tasks,
        _keycloak,
        _postgres,
    } = fixture;
    let browser_a = browser_client(&portal_ca, &provider_ca)?;
    let browser_b = browser_client(&portal_ca, &provider_ca)?;
    let mut callbacks = Vec::new();
    let targets = [
        "/researcher/workspaces?project=first#active",
        "/teacher/labs?project=second",
    ];
    // Begin both tabs before completing either; independent cookies must coexist.
    for target in targets {
        let login = browser_a
            .get(format!("{base}/auth/login"))
            .query(&[("return_to", target)])
            .send()
            .await?;
        assert_eq!(login.status(), StatusCode::TEMPORARY_REDIRECT);
        let authorization = login
            .headers()
            .get(header::LOCATION)
            .ok_or("missing authorization URL")?
            .to_str()?;
        callbacks.push(provider_callback(&browser_a, authorization, &callback_uri).await?);
    }
    let rejected = browser_b.get(callbacks[0].clone()).send().await?;
    assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        rejected.json::<Value>().await?,
        json!({"diagnosticCode": "LW_AUTH_OIDC_BROWSER_REJECTED"})
    );
    let sessions: i64 = sqlx::query_scalar("SELECT count(*) FROM access.bff_sessions")
        .fetch_one(&app.pool)
        .await?;
    assert_eq!(sessions, 0);
    for (callback, target) in callbacks.iter().zip(targets) {
        let success = browser_a.get(callback.clone()).send().await?;
        assert_eq!(success.status(), StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(
            success
                .headers()
                .get(header::LOCATION)
                .ok_or("missing portal return")?
                .to_str()?,
            format!("{base}{target}")
        );
        assert_eq!(
            success.headers().get_all(header::SET_COOKIE).iter().count(),
            2
        );
        let replay = browser_a.get(callback.clone()).send().await?;
        assert_eq!(replay.status(), StatusCode::UNAUTHORIZED);
    }
    let sessions: i64 = sqlx::query_scalar("SELECT count(*) FROM access.bff_sessions")
        .fetch_one(&app.pool)
        .await?;
    assert_eq!(sessions, 2);
    let refreshed = browser_a
        .get(format!("{base}/api/v1/auth/session"))
        .send()
        .await?;
    assert_eq!(refreshed.status(), StatusCode::OK);
    let session_body = refreshed.json::<Value>().await?;
    assert!(
        session_body["actor"]["roles"]
            .as_array()
            .is_some_and(|roles| roles.iter().any(|role| role == "teacher"))
    );
    let b_session = browser_b
        .get(format!("{base}/api/v1/auth/session"))
        .send()
        .await?;
    assert_eq!(b_session.status(), StatusCode::UNAUTHORIZED);
    let expired = browser_a.get(format!("{base}/auth/login")).send().await?;
    let authorization = expired
        .headers()
        .get(header::LOCATION)
        .ok_or("missing authorization URL")?
        .to_str()?;
    let expired_callback = provider_callback(&browser_a, authorization, &callback_uri).await?;
    let expired_state = expired_callback
        .query_pairs()
        .find(|(name, _)| name == "state")
        .ok_or("missing state")?
        .1
        .into_owned();
    // Expire at the trusted callback clock without waiting for the full login TTL.
    sqlx::query("UPDATE access.oidc_transactions SET created_at=now()-interval '10 minutes',expires_at=now()-interval '1 second' WHERE state_sha256=$1")
        .bind(Sha256Digest::of_bytes(expired_state.as_bytes()).to_string()).execute(&app.pool).await?;
    let rejected = browser_a.get(expired_callback).send().await?;
    assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        rejected.json::<Value>().await?,
        json!({"diagnosticCode": "LW_AUTH_OIDC_STATE_REJECTED"})
    );
    let csrf: Value = browser_a
        .get(format!("{base}/api/v1/auth/csrf"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let logout = browser_a
        .post(format!("{base}/auth/logout"))
        .header(header::ORIGIN, &base)
        .header(
            "x-csrf-token",
            csrf["csrfToken"].as_str().ok_or("missing CSRF token")?,
        )
        .send()
        .await?;
    assert_eq!(logout.status(), StatusCode::OK);
    let logout_body: Value = logout.json().await?;
    let provider_logout = browser_a
        .get(
            logout_body["logoutUrl"]
                .as_str()
                .ok_or("missing provider logout URL")?,
        )
        .send()
        .await?;
    assert!(provider_logout.status().is_redirection());
    assert_eq!(
        provider_logout
            .headers()
            .get(header::LOCATION)
            .ok_or("missing logout redirect")?
            .to_str()?,
        format!("{base}/")
    );
    assert_eq!(
        browser_a
            .get(format!("{base}/api/v1/auth/session"))
            .send()
            .await?
            .status(),
        StatusCode::UNAUTHORIZED
    );
    Ok(())
}
