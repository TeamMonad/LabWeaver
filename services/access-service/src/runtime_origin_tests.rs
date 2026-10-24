//! Real Access + Environment HTTP authorities, persisted sessions/grants, and an untrusted HTTP app.
use super::*;
use contracts::{EndpointGrantId, environment::EnvironmentInstance};
use environment_service::{OwnerResolver, PgEnvironmentStore, PgReleaseProjectionStore};
use reqwest::cookie::Jar;

struct RuntimeFixture {
    state: Arc<AppState>,
    client: reqwest::Client,
    portal: String,
    runtime: String,
    grant: EndpointGrantId,
    bff: BffSession,
    upstream: Arc<Mutex<Vec<Value>>>,
    _tasks: Tasks,
    _database: testcontainers::ContainerAsync<Postgres>,
}

impl RuntimeFixture {
    #[allow(clippy::too_many_lines)]
    async fn new() -> Result<Self, Box<dyn Error>> {
        let database = Postgres::default().with_tag("17.5-alpine").start().await?;
        let pool = PgPoolOptions::new()
            .max_connections(6)
            .connect(&format!(
                "postgres://postgres:postgres@127.0.0.1:{}/postgres",
                database.get_host_port_ipv4(5432).await?
            ))
            .await?;
        support::apply_access_migrations(&pool).await?;
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
        let catalog = persistence_sqlx::MigrationCatalog::load(&root.join("catalog.yaml"))?;
        sqlx::query("CREATE SCHEMA environment")
            .execute(&pool)
            .await?;
        let mut connection = pool.acquire().await?;
        sqlx::query("SET search_path=environment,pg_catalog")
            .execute(&mut *connection)
            .await?;
        let domain = catalog
            .domains
            .iter()
            .find(|d| d.name == persistence_sqlx::Domain::Environment)
            .ok_or("missing environment domain")?;
        for migration in &domain.migrations {
            let sql = persistence_sqlx::MigrationCatalog::read_verified_sql(&root, migration)?;
            sqlx::raw_sql(sqlx::AssertSqlSafe(&*sql))
                .execute(&mut *connection)
                .await?;
        }
        drop(connection);
        let mut tasks = Tasks(Vec::new());
        let actor = Uuid::now_v7();
        let (mut state, _) = Box::pin(state(pool.clone(), actor, &mut tasks)).await?;
        let bff = session(
            &pool,
            &state.key_ring,
            actor,
            contracts::PlatformRole::Student,
        )
        .await?;
        let authority_now: OffsetDateTime =
            sqlx::query_scalar("SELECT date_trunc('milliseconds',clock_timestamp())")
                .fetch_one(&pool)
                .await?;
        let now = contracts::UtcTimestamp::from_utc(authority_now)?;
        let deadline = contracts::UtcTimestamp::from_utc(authority_now + Duration::minutes(30))?;
        let environment = contracts::EnvironmentId::new();
        let project = contracts::ProjectId::new();
        let endpoint = contracts::EndpointId::new();
        let grant = EndpointGrantId::new();
        let access_grant = contracts::AccessGrantId::new();
        let instance: EnvironmentInstance = serde_json::from_value(json!({
            "id": environment, "projectId": project, "courseId": null, "ownerId": actor,
            "displayLabel": "Runtime isolation HTTP fixture", "class": "experiment", "runtimeKind": "container",
            "releaseId": contracts::ReleaseId::new(), "releaseVersion": 1, "leaseId": null, "capacityBinding": null,
            "approvedResources": {"cpuMillicores": 1000,"memoryBytes":1_073_741_824,"storageBytes":2_147_483_648_u64,"gpu":null},
            "providerBinding":"container-primary-v1", "desiredState":"running", "observedState":"ready",
            "revision":1,"generation":1,"observedGeneration":1,
            "operation": {"id":contracts::OperationId::new(),"kind":"create","state":"succeeded","acceptedRevision":1,
                "attempt":1,"providerStep":1,"maxAttempts":3,"nextAttemptAt":now,"actorId":actor,
                "traceId":"11111111111111111111111111111111","acceptedAt":now,"deadlineAt":deadline,
                "cleanupStartedAt":null,"diagnosticCode":null,"preserveMutableDisk":false,
                "accessRevocationRevision":null,"retryFromPhase":null,"resetTarget":null,"leaseAuthorization":null,"templateUpgrade":null},
            "gpuAllocation":null,"resourceReservationReleased":false,"runtimeAuthorization":null,
            "eligibilityExpiresAt":deadline,
            "endpoints":[{"id":endpoint,"protocol":"http","revision":1,"health":"healthy","observedAt":now}],
            "lastDiagnosticCode":null,"failedPhase":null,"cleanupEvidence":null
        }))?;
        instance.validate()?;
        sqlx::query("INSERT INTO environment.environment_instances(environment_id,project_id,course_id,owner_actor_id,release_id,generation,observed_generation,desired_state,observed_state,provider_binding,revision,contract) VALUES($1,$2,NULL,$3,$4,1,1,'running','ready','container-primary-v1',1,$5)")
            .bind(environment.as_uuid()).bind(project.as_uuid()).bind(actor).bind(instance.release_id.as_uuid())
            .bind(serde_json::to_value(&instance)?).execute(&pool).await?;
        sqlx::query("INSERT INTO control.project_memberships(project_id,actor_id,role,object_role,state,revision) VALUES($1,$2,'student','editor','active',1)")
            .bind(project.as_uuid()).bind(actor).execute(&pool).await?;
        sqlx::query("INSERT INTO access.access_grants(grant_id,actor_id,project_id,environment_id,environment_revision,revision,state,not_before,expires_at,contract) VALUES($1,$2,$3,$4,1,1,'active',$5,$6,$7)")
            .bind(access_grant.as_uuid()).bind(actor).bind(project.as_uuid()).bind(environment.as_uuid())
            .bind(now.get()).bind(deadline.get()).bind(json!({"subjectKind":"owner"})).execute(&pool).await?;
        sqlx::query("INSERT INTO access.endpoint_grants(endpoint_grant_id,grant_id,endpoint_id,endpoint_revision,protocol,action,health,expires_at,contract) VALUES($1,$2,$3,1,'http','connect','healthy',$4,'{}')")
            .bind(grant.as_uuid()).bind(access_grant.as_uuid()).bind(endpoint.as_uuid()).bind(deadline.get()).execute(&pool).await?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let portal = format!("https://localhost:{}", address.port());
        let runtime = format!("https://127.0.0.1:{}", address.port());
        let certificate = rcgen::generate_simple_self_signed(vec![
            "localhost".to_owned(),
            "127.0.0.1".to_owned(),
        ])?;
        let ca = certificate.cert.pem();
        let tls = http_transport::server_config(
            ca.as_bytes(),
            certificate.signing_key.serialize_pem().as_bytes(),
        )?;
        let resolver_listener = TcpListener::bind("127.0.0.1:0").await?;
        let resolver_uri = format!("https://{}", resolver_listener.local_addr()?);
        let resolver = OwnerResolver::new(
            PgEnvironmentStore::new(pool.clone()),
            PgReleaseProjectionStore::new(pool),
        );
        let resolver_router = environment_service::with_service_auth(
            environment_service::owner_resolver_router(resolver),
            Arc::clone(&state.service_token_verifier),
        );
        let resolver_tls = Arc::clone(&tls);
        tasks.0.push(tokio::spawn(async move {
            let _ =
                http_transport::serve_tls(resolver_listener, resolver_router, resolver_tls).await;
        }));
        let scopes = BTreeSet::from(["environment.endpoint.resolve".to_owned()]);
        let token_client = ServiceTokenClient::discover(
            ServiceTokenClientConfig::new(
                &state.deployment.oidc.issuer,
                "access-test".to_owned(),
                "fixture-secret".to_owned(),
                AUDIENCE.to_owned(),
                scopes.clone(),
                30,
                TransportSecurityMode::Strict,
            )?,
            state.oidc_http.clone(),
        )
        .await?;
        let upstream_listener = TcpListener::bind("127.0.0.2:8080").await?;
        let upstream_address = upstream_listener.local_addr()?;
        let upstream = Arc::new(Mutex::new(Vec::new()));
        let app_router = Router::new()
            .fallback(any(untrusted_app))
            .with_state(Arc::clone(&upstream));
        tasks.0.push(tokio::spawn(async move {
            let _ = axum::serve(upstream_listener, app_router).await;
        }));
        let mutable = Arc::get_mut(&mut state).ok_or("state unexpectedly shared")?;
        mutable.config.allowed_origins = BTreeSet::from([portal.clone()]);
        mutable.deployment.browser.allowed_origins = mutable.config.allowed_origins.clone();
        mutable
            .deployment
            .browser
            .runtime_origin
            .clone_from(&runtime);
        let mut config = mutable.deployment.environment_owner_resolver.clone();
        config.resolver_uri = resolver_uri;
        mutable.owner_resolver = EnvironmentOwnerResolverClient::new(
            &config.contract(),
            ca.as_bytes(),
            token_client,
            AUDIENCE.to_owned(),
            scopes,
            std::time::Duration::from_millis(5),
            TransportSecurityMode::Strict,
        )?;
        mutable.runtime_proxy.client = reqwest::Client::builder()
            .no_proxy()
            .resolve(
                &format!("runtime.lw-env-{environment}.svc.cluster.local"),
                upstream_address,
            )
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let router = browser_router(Arc::clone(&state));
        tasks.0.push(tokio::spawn(async move {
            let _ = http_transport::serve_tls(listener, router, tls).await;
        }));
        let jar = Arc::new(Jar::default());
        jar.add_cookie_str(
            &format!(
                "__Host-labweaver_session={}; Path=/; Secure; HttpOnly; SameSite=Lax",
                bff.session_id
            ),
            &Url::parse(&portal)?,
        );
        let client = reqwest::Client::builder()
            .no_proxy()
            .cookie_provider(jar)
            .add_root_certificate(reqwest::Certificate::from_pem(ca.as_bytes())?)
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            state,
            client,
            portal,
            runtime,
            grant,
            bff,
            upstream,
            _tasks: tasks,
            _database: database,
        })
    }

    async fn handoff(&self) -> Result<String, Box<dyn Error>> {
        let start = self
            .client
            .get(format!("{}/connect/{}/", self.portal, self.grant))
            .send()
            .await?;
        assert_eq!(
            start.status(),
            StatusCode::SEE_OTHER,
            "{}",
            start.text().await?
        );
        let bootstrap = self
            .client
            .get(start.headers()[header::LOCATION].to_str()?)
            .send()
            .await?;
        assert_eq!(bootstrap.status(), StatusCode::SEE_OTHER);
        let issued = self
            .client
            .get(bootstrap.headers()[header::LOCATION].to_str()?)
            .send()
            .await?;
        assert_eq!(issued.status(), StatusCode::SEE_OTHER);
        Ok(issued.headers()[header::LOCATION].to_str()?.to_owned())
    }
}

async fn untrusted_app(
    State(requests): State<Arc<Mutex<Vec<Value>>>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    requests.lock().await.push(json!({"method":method.as_str(),"path":uri.path(),"cookie":headers.get(header::COOKIE).and_then(|h|h.to_str().ok()),"authorization":headers.get(header::AUTHORIZATION).and_then(|h|h.to_str().ok())}));
    if uri.path() == "/redirect" {
        return (
            StatusCode::FOUND,
            [(header::LOCATION, "/landing?from=redirect")],
        )
            .into_response();
    }
    if method == Method::POST {
        return body.into_response();
    }
    ([(header::CONTENT_TYPE,"text/html")],"<!doctype html><title>Untrusted runtime</title><script>window.runtimeScriptRan = true</script>").into_response()
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn runtime_handoff_isolated_bound_one_time_and_live_revocation() -> Result<(), Box<dyn Error>>
{
    let fixture = Box::pin(RuntimeFixture::new()).await?;
    let handoff = fixture.handoff().await?;
    let other = reqwest::Client::builder()
        .no_proxy()
        .danger_accept_invalid_certs(true)
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    assert_eq!(
        other.get(&handoff).send().await?.status(),
        StatusCode::FORBIDDEN
    );
    let crossed = handoff.replace(
        &fixture.grant.to_string(),
        &EndpointGrantId::new().to_string(),
    );
    assert_eq!(
        fixture.client.get(crossed).send().await?.status(),
        StatusCode::FORBIDDEN
    );
    let consumed = fixture.client.get(&handoff).send().await?;
    assert_eq!(consumed.status(), StatusCode::SEE_OTHER);
    let cookie = consumed.headers()[header::SET_COOKIE].to_str()?.to_owned();
    assert!(cookie.contains("HttpOnly; Secure; SameSite=Lax"));
    assert!(cookie.contains(&format!("Path=/connect/{}/", fixture.grant)));
    assert!(!cookie.contains("Domain="));
    assert_eq!(
        fixture.client.get(&handoff).send().await?.status(),
        StatusCode::FORBIDDEN
    );
    assert!(fixture.upstream.lock().await.is_empty());
    let target = format!("{}/connect/{}/", fixture.runtime, fixture.grant);
    let read = fixture.client.get(&target).send().await?;
    assert_eq!(read.status(), StatusCode::OK, "{}", read.text().await?);
    assert_eq!(
        fixture
            .client
            .post(&target)
            .header(header::ORIGIN, &fixture.runtime)
            .body("payload")
            .send()
            .await?
            .text()
            .await?,
        "payload"
    );
    assert_eq!(
        fixture
            .client
            .post(&target)
            .header(header::ORIGIN, &fixture.portal)
            .send()
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );
    let redirect = fixture
        .client
        .get(format!("{target}redirect"))
        .send()
        .await?;
    assert_eq!(redirect.status(), StatusCode::FOUND);
    assert_eq!(
        redirect.headers()[header::LOCATION],
        format!("/connect/{}/landing?from=redirect", fixture.grant)
    );
    for path in ["/api/v1/auth/csrf", "/api/v1/auth/session", "/auth/login"] {
        assert_eq!(
            fixture
                .client
                .get(format!("{}{path}", fixture.runtime))
                .send()
                .await?
                .status(),
            StatusCode::FORBIDDEN
        );
        let attack = fixture
            .client
            .get(format!("{}{path}", fixture.portal))
            .header(header::ORIGIN, &fixture.runtime)
            .send()
            .await?;
        assert_eq!(attack.status(), StatusCode::FORBIDDEN);
        assert!(
            !attack
                .headers()
                .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN)
        );
    }
    for request in fixture.upstream.lock().await.iter() {
        assert!(request["cookie"].is_null());
        assert!(request["authorization"].is_null());
    }
    let requests = fixture.upstream.lock().await.len();
    let cap = cookie.split(';').next().ok_or("missing capability")?;
    assert_eq!(
        other
            .get(format!(
                "{}/connect/{}/",
                fixture.runtime,
                EndpointGrantId::new()
            ))
            .header(header::COOKIE, cap)
            .send()
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );
    // Actor binding is also checked independently of the stored BFF session ID.
    let different = session(
        &fixture.state.pool,
        &fixture.state.key_ring,
        Uuid::now_v7(),
        contracts::PlatformRole::Student,
    )
    .await?;
    sqlx::query(
        "UPDATE access.runtime_capabilities SET bff_session_id=$1 WHERE consumed_at IS NOT NULL",
    )
    .bind(different.session_id)
    .execute(&fixture.state.pool)
    .await?;
    assert_eq!(
        fixture.client.get(&target).send().await?.status(),
        StatusCode::FORBIDDEN
    );
    sqlx::query(
        "UPDATE access.runtime_capabilities SET bff_session_id=$1 WHERE consumed_at IS NOT NULL",
    )
    .bind(fixture.bff.session_id)
    .execute(&fixture.state.pool)
    .await?;
    let expired = fixture.handoff().await?;
    sqlx::query("UPDATE access.runtime_capabilities SET handoff_expires_at=now()-interval '1 second' WHERE consumed_at IS NULL").execute(&fixture.state.pool).await?;
    assert_eq!(
        fixture.client.get(expired).send().await?.status(),
        StatusCode::FORBIDDEN
    );
    sqlx::query("UPDATE access.runtime_capabilities SET expires_at=now()-interval '1 second' WHERE consumed_at IS NOT NULL").execute(&fixture.state.pool).await?;
    assert_eq!(
        fixture.client.get(&target).send().await?.status(),
        StatusCode::FORBIDDEN
    );
    let renewed = fixture.handoff().await?;
    assert_eq!(
        fixture.client.get(renewed).send().await?.status(),
        StatusCode::SEE_OTHER
    );
    let before_revoke = fixture.handoff().await?;
    sqlx::query("UPDATE access.access_grants SET state='revoked',revoked_at=now(),reason_code='actor_requested'")
        .execute(&fixture.state.pool)
        .await?;
    assert_eq!(
        fixture.client.get(&target).send().await?.status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        fixture.client.get(before_revoke).send().await?.status(),
        StatusCode::FORBIDDEN
    );
    sqlx::query("UPDATE access.access_grants SET state='active',revoked_at=NULL,reason_code=NULL")
        .execute(&fixture.state.pool)
        .await?;
    let before_expiry = fixture.handoff().await?;
    sqlx::query("UPDATE access.access_grants SET not_before=now()-interval '1 minute',expires_at=now()-interval '1 second'")
        .execute(&fixture.state.pool)
        .await?;
    assert_eq!(
        fixture.client.get(before_expiry).send().await?.status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        fixture.client.get(&target).send().await?.status(),
        StatusCode::FORBIDDEN
    );
    sqlx::query("UPDATE access.access_grants SET expires_at=now()+interval '30 minutes'")
        .execute(&fixture.state.pool)
        .await?;
    sqlx::query("UPDATE control.project_memberships SET object_role='viewer'")
        .execute(&fixture.state.pool)
        .await?;
    assert_eq!(
        fixture
            .client
            .post(&target)
            .header(header::ORIGIN, &fixture.runtime)
            .send()
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );
    sqlx::query("UPDATE control.project_memberships SET object_role='editor'")
        .execute(&fixture.state.pool)
        .await?;
    let before_logout = fixture.handoff().await?;
    let logout = fixture
        .client
        .post(format!("{}/auth/logout", fixture.portal))
        .header(header::ORIGIN, &fixture.portal)
        .header("X-CSRF-Token", fixture.bff.csrf_token.expose())
        .send()
        .await?;
    assert_eq!(logout.status(), StatusCode::OK);
    assert_eq!(
        fixture.client.get(before_logout).send().await?.status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        fixture.client.get(&target).send().await?.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(fixture.upstream.lock().await.len(), requests);
    Ok(())
}

#[tokio::test]
#[ignore = "requires installed Playwright Chromium, Node and PostgreSQL Docker image"]
async fn chromium_runtime_script_cannot_use_portal_session() -> Result<(), Box<dyn Error>> {
    let fixture = Box::pin(RuntimeFixture::new()).await?;
    let script =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/runtime_origin_browser.mjs");
    let mut browser = tokio::process::Command::new("node")
        .arg(script)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let mut input = browser.stdin.take().ok_or("browser stdin unavailable")?;
    input.write_all(format!("{}\n", json!({"portal":fixture.portal,"runtime":fixture.runtime,"grant":fixture.grant,"session":fixture.bff.session_id})).as_bytes()).await?;
    drop(input);
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(90),
        browser.wait_with_output(),
    )
    .await??;
    assert!(
        output.status.success(),
        "browser stdout: {}\nbrowser stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    println!("{}", String::from_utf8_lossy(&output.stdout));
    assert!(!fixture.upstream.lock().await.is_empty());
    for request in fixture.upstream.lock().await.iter() {
        assert!(request["cookie"].is_null());
        assert!(request["authorization"].is_null());
    }
    Ok(())
}
