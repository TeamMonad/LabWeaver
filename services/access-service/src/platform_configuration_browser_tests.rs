//! Full native OIDC navigation and BFF/Control configuration with disposable authorities.
use super::*;
use axum::body::Body;
use axum::extract::Request;
use control_service::api::{ApiState, authenticated_router};
use control_service::clients::{
    AccessClient, AgentClient, EnvironmentClient, EvaluationClient, ServiceHttpClientConfig,
};
use std::path::{Component, PathBuf};
use std::process::Stdio;

#[path = "../../control-service/tests/support/platform_configuration_common.rs"]
mod configuration_common;

#[derive(Clone)]
struct WebFiles(PathBuf);

async fn web_files(State(files): State<WebFiles>, request: Request) -> Response {
    let path = request.uri().path().trim_start_matches('/');
    if request.method() != Method::GET
        || path.starts_with("api/")
        || path.starts_with("auth/")
        || path.starts_with("internal/")
        || !std::path::Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    let mut file = files.0.join(path);
    if !file.is_file() && !path.contains('.') {
        file = files.0.join("index.html");
    }
    let content_type = match file.extension().and_then(|x| x.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "application/javascript",
        Some("css") => "text/css",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        _ => "application/octet-stream",
    };
    match tokio::fs::read(file).await {
        Ok(bytes) => ([(header::CONTENT_TYPE, content_type)], Body::from(bytes)).into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

fn realm_services(
    realm: &mut Value,
    access_secret: &str,
    control_secret: &str,
) -> Result<(), Box<dyn Error>> {
    let assignments = [
        (
            "labweaver-access",
            "labweaver-control",
            "access.control.forward",
            access_secret,
        ),
        (
            "labweaver-control",
            "labweaver-access",
            "access.authorization.decide",
            control_secret,
        ),
    ];
    for (caller, target, permission, secret) in assignments {
        realm["roles"]["client"][target] =
            json!([{"name":permission,"clientRole":true,"composite":false}]);
        realm["clientScopeMappings"][target] = json!([{"client":caller,"roles":[permission]}]);
        realm["clients"].as_array_mut().ok_or("realm clients missing")?.push(json!({
            "clientId":caller,"secret":secret,"enabled":true,"publicClient":false,
            "protocol":"openid-connect","serviceAccountsEnabled":true,"standardFlowEnabled":false,
            "directAccessGrantsEnabled":false,"fullScopeAllowed":false,
            "protocolMappers":[{"name":"service audiences","protocol":"openid-connect",
                "protocolMapper":"oidc-audience-resolve-mapper","consentRequired":false,
                "config":{"access.token.claim":"true","id.token.claim":"false"}}]
        }));
        realm["users"]
            .as_array_mut()
            .ok_or("realm users missing")?
            .push(json!({
                "username":format!("service-account-{caller}"),"enabled":true,
                "serviceAccountClientId":caller,"clientRoles":{target:[permission]}
            }));
    }
    realm["users"][0]["username"] = json!("admin");
    realm["users"][0]["realmRoles"] = json!(["platform-admin"]);
    let mut student = realm["users"][0].clone();
    student["username"] = json!("student");
    student["email"] = json!("student@example.invalid");
    student["realmRoles"] = json!(["student"]);
    realm["users"]
        .as_array_mut()
        .ok_or("realm users missing")?
        .push(student);
    Ok(())
}

#[expect(
    clippy::too_many_lines,
    reason = "one test assembly connects both real TLS authorities without a second production composition path"
)]
async fn connect_control(
    app: &mut AppState,
    provider_ca: &str,
    tasks: &mut Tasks,
    temporary: &std::path::Path,
    access_secret: &str,
    control_secret: &str,
) -> Result<(), Box<dyn Error>> {
    let access_listener = TcpListener::bind("127.0.0.1:0").await?;
    let control_listener = TcpListener::bind("127.0.0.1:0").await?;
    let access_base = format!("https://{}/", access_listener.local_addr()?);
    let control_base = format!("https://{}/", control_listener.local_addr()?);
    let tls = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_owned()])?;
    let ca = tls.cert.pem();
    let ca_path = temporary.join("service-ca.pem");
    std::fs::write(&ca_path, &ca)?;
    let server =
        http_transport::server_config(ca.as_bytes(), tls.signing_key.serialize_pem().as_bytes())?;
    let issuer = &app.deployment.oidc.issuer;
    let http =
        no_redirect_http_client(Some(provider_ca.as_bytes()), TransportSecurityMode::Strict)?;
    let verifier = |audience: &str, caller: &str| {
        ServiceAuthConfig::new(
            issuer,
            audience.to_owned(),
            BTreeSet::from([caller.to_owned()]),
            BTreeSet::new(),
            BTreeSet::from(["RS256".to_owned()]),
            3600,
            1,
            TransportSecurityMode::Strict,
        )
    };
    app.service_token_verifier = Arc::new(
        ServiceTokenVerifier::discover(
            verifier("labweaver-access", "labweaver-control")?,
            http.clone(),
        )
        .await?,
    );
    let access_tokens = Arc::new(
        ServiceTokenClient::discover(
            ServiceTokenClientConfig::new(
                issuer,
                "labweaver-access".to_owned(),
                access_secret.to_owned(),
                "labweaver-control".to_owned(),
                BTreeSet::from(["access.control.forward".to_owned()]),
                30,
                TransportSecurityMode::Strict,
            )?,
            http.clone(),
        )
        .await?,
    );
    app.deployment
        .control_gateway
        .base_uri
        .clone_from(&control_base);
    app.deployment.control_gateway.allowed_server_sans = vec!["127.0.0.1".to_owned()];
    app.control_proxy = proxy::ControlGatewayProxy::new(
        &app.deployment.control_gateway,
        ca.as_bytes(),
        TransportSecurityMode::Strict,
        access_tokens,
        ServiceTokenTarget {
            audience: "labweaver-control".to_owned(),
            scopes: BTreeSet::from(["access.control.forward".to_owned()]),
        },
    )?;
    let control_verifier = Arc::new(
        ServiceTokenVerifier::discover(
            verifier("labweaver-control", "labweaver-access")?,
            http.clone(),
        )
        .await?,
    );
    let control_tokens = Arc::new(
        ServiceTokenClient::discover(
            ServiceTokenClientConfig::new(
                issuer,
                "labweaver-control".to_owned(),
                control_secret.to_owned(),
                "labweaver-access".to_owned(),
                BTreeSet::from(["access.authorization.decide".to_owned()]),
                30,
                TransportSecurityMode::Strict,
            )?,
            http,
        )
        .await?,
    );
    let endpoint = ServiceHttpClientConfig {
        base_url: Url::parse(&access_base)?,
        ca_certificate_file: ca_path.to_string_lossy().into_owned(),
        timeout_milliseconds: 3000,
    };
    let api = Arc::new(ApiState {
        control: control_service::ControlService::new(
            app.pool.clone(),
            Arc::new(configuration_common::NoObjects),
            configuration_common::config()?,
        )?,
        access: AccessClient::new_authenticated(endpoint.clone(), control_tokens.clone())?,
        agent: AgentClient::new_authenticated(endpoint.clone(), control_tokens.clone())?,
        environment: EnvironmentClient::new_authenticated(
            endpoint.clone(),
            control_tokens.clone(),
        )?,
        evaluation: EvaluationClient::new_authenticated(endpoint, control_tokens)?,
        service_token_verifier: control_verifier.clone(),
    });
    let control = authenticated_router(&api, &control_verifier);
    let access = internal_router(Arc::new(app.clone()));
    let access_server = server.clone();
    tasks.0.push(tokio::spawn(async move {
        let _ = http_transport::serve_tls(access_listener, access, access_server).await;
    }));
    tasks.0.push(tokio::spawn(async move {
        let _ = http_transport::serve_tls(control_listener, control, server).await;
    }));
    Ok(())
}

#[tokio::test]
#[ignore = "requires installed Web dependencies, Chromium, Node, age and disposable Keycloak/Docker"]
async fn chromium_login_and_csrf_reach_real_platform_configuration() -> Result<(), Box<dyn Error>> {
    run_browser("sdk").await
}

#[tokio::test]
#[ignore = "requires composed Grok Web source and real BFF browser dependencies"]
async fn chromium_login_and_csrf_reach_platform_configuration_page() -> Result<(), Box<dyn Error>> {
    run_browser("app").await
}

async fn run_browser(surface: &str) -> Result<(), Box<dyn Error>> {
    let temporary = tempfile::tempdir()?;
    let access_secret = Uuid::now_v7().to_string();
    let control_secret = Uuid::now_v7().to_string();
    let web = temporary.path().join("web-dist");
    let routes = Router::new()
        .fallback(web_files)
        .with_state(WebFiles(web.clone()));
    let fixture = oidc_login_tests::real_oidc_fixture_with(
        "localhost",
        |realm| realm_services(realm, &access_secret, &control_secret),
        async |app, ca, tasks| {
            connect_control(
                app,
                ca,
                tasks,
                temporary.path(),
                &access_secret,
                &control_secret,
            )
            .await
        },
        routes,
    )
    .await?;
    let cipher = configuration_common::ciphertext()?;
    let input = temporary.path().join("browser-input.json");
    let contents = serde_json::to_vec(
        &json!({"base":fixture.base,"ciphertext":URL_SAFE_NO_PAD.encode(&cipher),
        "webDist":web,"evidenceDirectory":temporary.path(),"surface":surface}),
    )?;
    std::fs::write(&input, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&input, std::fs::Permissions::from_mode(0o600))?;
    }
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../web/e2e/backend/platform-configuration-login.mjs");
    let mut child = tokio::process::Command::new("node")
        .arg(script)
        .arg(&input)
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
            Some("web_ready") => {
                child
                    .stdin
                    .as_mut()
                    .ok_or("missing browser stdin")?
                    .write_all(b"{\"ok\":true}\n")
                    .await?;
            }
            Some("complete") => {
                completed = true;
                println!("Configuration BFF browser passed: {message}");
            }
            _ => return Err("unexpected browser protocol message".into()),
        }
    }
    assert!(
        child.wait().await?.success(),
        "configuration browser failed"
    );
    assert!(completed, "configuration browser did not complete");
    let state = control_service::platform_configuration::get(&fixture.app.pool).await?;
    assert_eq!(
        state
            .registered_bundle
            .as_ref()
            .ok_or("missing registered bundle")?
            .encrypted_bundle_sha256,
        persistence_sqlx::Sha256Digest::of_bytes(&cipher).to_string()
    );
    assert!(state.applied.is_none());
    assert_eq!(
        state
            .latest_apply
            .as_ref()
            .ok_or("missing requested apply")?
            .stage,
        contracts::platform_configuration::PlatformConfigurationApplyStage::Requested
    );
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM control.platform_configuration_bundles")
            .fetch_one(&fixture.app.pool)
            .await?;
    assert_eq!(count, 1);
    Ok(())
}
