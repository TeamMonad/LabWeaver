//! Real Access + Environment HTTP authorities, persisted sessions/grants, and an untrusted HTTP app.
use super::*;
use axum::body::Body;
use contracts::{EndpointGrantId, environment::EnvironmentInstance};
use environment_service::{OwnerResolver, PgEnvironmentStore, PgReleaseProjectionStore};
use futures_util::{SinkExt, Stream, StreamExt};
use reqwest::cookie::{CookieStore, Jar};
use rustls::{ClientConfig, RootCertStore};
use sqlx::Row;
use std::{
    pin::Pin,
    task::{Context, Poll},
};
use tokio_tungstenite::{
    Connector, client_async_tls_with_config,
    tungstenite::{self, Message as WsMessage},
};

type TestWebSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

struct RuntimeFixture {
    state: Arc<AppState>,
    client: reqwest::Client,
    portal: String,
    runtime: String,
    grant: EndpointGrantId,
    runtime_b: String,
    grant_b: EndpointGrantId,
    request_log: Arc<Mutex<Vec<Value>>>,
    bff: BffSession,
    upstream: Arc<Mutex<Vec<Value>>>,
    jar: Arc<Jar>,
    certificate_pem: String,
    private_key_pem: String,
    tasks: Tasks,
    _database: testcontainers::ContainerAsync<Postgres>,
}

struct IssuedHandoff {
    url: Url,
    token: String,
    binding: String,
}

impl IssuedHandoff {
    fn exchange(&self, client: &reqwest::Client) -> reqwest::RequestBuilder {
        client
            .post(self.url.as_str())
            .header(header::ORIGIN, self.url.origin().ascii_serialization())
            .form(&[("token", self.token.as_str())])
    }
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
        seed_runtime(&pool, &instance, grant, access_grant).await?;
        let mut second = instance.clone();
        second.id = contracts::EnvironmentId::new();
        second.project_id = contracts::ProjectId::new();
        second.endpoints[0].id = contracts::EndpointId::new();
        let grant_b = EndpointGrantId::new();
        seed_runtime(&pool, &second, grant_b, contracts::AccessGrantId::new()).await?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let portal = format!("https://portal.localhost:{}", address.port());
        let runtime_base = format!("https://runtime.localhost:{}", address.port());
        let runtime = format!(
            "https://g-{}.runtime.localhost:{}",
            grant.as_uuid().simple(),
            address.port()
        );
        let runtime_b = format!(
            "https://g-{}.runtime.localhost:{}",
            grant_b.as_uuid().simple(),
            address.port()
        );
        let certificate = rcgen::generate_simple_self_signed(vec![
            "portal.localhost".to_owned(),
            "*.runtime.localhost".to_owned(),
            "127.0.0.1".to_owned(),
        ])?;
        let ca = certificate.cert.pem();
        let private_key_pem = certificate.signing_key.serialize_pem();
        let tls = http_transport::server_config(ca.as_bytes(), private_key_pem.as_bytes())?;
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
        let upstream_listener = TcpListener::bind("127.0.0.1:0").await?;
        let upstream_address = upstream_listener.local_addr()?;
        let upstream = Arc::new(Mutex::new(Vec::new()));
        let app_router = Router::new()
            .route("/ws", get(upstream_ws))
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
            .runtime_base_origin
            .clone_from(&runtime_base);
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
        let mut runtime_config = mutable.deployment.environment_gateway.clone();
        runtime_config.timeout_milliseconds = 30_000;
        mutable.runtime_proxy.client = proxy::RuntimeGatewayProxy::client_builder(&runtime_config)
            .no_proxy()
            .build()?;
        mutable.runtime_proxy.max_response_bytes = usize::MAX;
        mutable.runtime_proxy.http_base = Some(Url::parse(&format!("http://{upstream_address}"))?);
        mutable.runtime_proxy.websocket_base =
            Some(Url::parse(&format!("ws://{upstream_address}"))?);
        let request_log = Arc::new(Mutex::new(Vec::new()));
        let router = browser_router(Arc::clone(&state)).layer(
            axum::middleware::from_fn_with_state(Arc::clone(&request_log), record_request_uri),
        );
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
            .cookie_provider(Arc::clone(&jar))
            .add_root_certificate(reqwest::Certificate::from_pem(ca.as_bytes())?)
            .resolve("portal.localhost", address)
            .resolve(
                Url::parse(&runtime)?
                    .host_str()
                    .ok_or("missing runtime host")?,
                address,
            )
            .resolve(
                Url::parse(&runtime_b)?
                    .host_str()
                    .ok_or("missing second runtime host")?,
                address,
            )
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            state,
            client,
            portal,
            runtime,
            grant,
            runtime_b,
            grant_b,
            request_log,
            bff,
            upstream,
            jar,
            certificate_pem: ca,
            private_key_pem,
            tasks,
            _database: database,
        })
    }

    fn websocket_request(
        &self,
        origin: &str,
        grant: EndpointGrantId,
        cookie: &str,
        protocols: &str,
    ) -> Result<tungstenite::handshake::client::Request, Box<dyn Error>> {
        let mut request = format!("{origin}/connect/{grant}/ws?session_id=a%2Fb&token=x+y")
            .replace("https://", "wss://")
            .into_client_request()?;
        request
            .headers_mut()
            .insert(header::ORIGIN, origin.parse()?);
        request
            .headers_mut()
            .insert(header::COOKIE, cookie.parse()?);
        request
            .headers_mut()
            .insert(header::SEC_WEBSOCKET_PROTOCOL, protocols.parse()?);
        Ok(request)
    }

    async fn websocket(
        &self,
        request: tungstenite::handshake::client::Request,
    ) -> Result<(TestWebSocket, tungstenite::handshake::client::Response), Box<dyn Error>> {
        let mut roots = RootCertStore::empty();
        for certificate in
            rustls_pemfile::certs(&mut std::io::Cursor::new(self.certificate_pem.as_bytes()))
        {
            roots.add(certificate?)?;
        }
        let tls = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        // Both positive and negative cases use the same socket, SNI and trusted root.
        let port = Url::parse(&self.portal)?
            .port()
            .ok_or("missing Access fixture port")?;
        let socket = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).await?;
        Ok(client_async_tls_with_config(
            request,
            socket,
            None,
            Some(Connector::Rustls(Arc::new(tls))),
        )
        .await?)
    }

    async fn assert_websocket_denied(
        &self,
        request: tungstenite::handshake::client::Request,
        status: StatusCode,
        diagnostic: &str,
    ) -> Result<(), Box<dyn Error>> {
        let before = self
            .upstream
            .lock()
            .await
            .iter()
            .filter(|entry| entry["websocket"] == true)
            .count();
        let result = self.websocket(request).await;
        let error = result.err().ok_or("unauthorized websocket connected")?;
        let Some(tungstenite::Error::Http(response)) = error.downcast_ref::<tungstenite::Error>()
        else {
            return Err(
                format!("expected HTTP denial, not TLS/DNS/transport failure: {error}").into(),
            );
        };
        assert_eq!(response.status(), status);
        let problem: contracts::ProblemDetails = serde_json::from_slice(
            response
                .body()
                .as_ref()
                .ok_or("missing websocket denial body")?,
        )?;
        assert_eq!(problem.diagnostic_code.as_str(), diagnostic);
        assert_eq!(
            self.upstream
                .lock()
                .await
                .iter()
                .filter(|entry| entry["websocket"] == true)
                .count(),
            before,
            "denied request reached upstream"
        );
        Ok(())
    }

    async fn handoff(&self, grant: EndpointGrantId) -> Result<IssuedHandoff, Box<dyn Error>> {
        let start = self
            .client
            .get(format!("{}/connect/{grant}/", self.portal))
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
        let return_url = Url::parse(bootstrap.headers()[header::LOCATION].to_str()?)?;
        let binding = return_url
            .fragment()
            .and_then(|fragment| fragment.strip_prefix("binding="))
            .ok_or("missing binding fragment")?;
        assert!(
            !return_url
                .query()
                .is_some_and(|query| query.contains("binding"))
        );
        let page = self.client.get(return_url.as_str()).send().await?;
        assert_eq!(page.status(), StatusCode::OK);
        let issued = self
            .client
            .post(format!("{}/connect/{grant}/", self.portal))
            .header(header::ORIGIN, &self.portal)
            .header("X-CSRF-Token", self.bff.csrf_token.expose())
            .json(&json!({"binding":binding}))
            .send()
            .await?;
        assert_eq!(issued.status(), StatusCode::OK, "{}", issued.text().await?);
        assert!(self.request_log.lock().await.iter().all(|entry| {
            entry["target"]
                .as_str()
                .is_some_and(|uri| !uri.contains(binding) && !uri.contains("binding="))
        }));
        let document: Value = issued.json().await?;
        let url = Url::parse(
            document["handoffUrl"]
                .as_str()
                .ok_or("missing handoff URL")?,
        )?;
        let token = url
            .fragment()
            .and_then(|fragment| fragment.strip_prefix("token="))
            .ok_or("missing handoff token fragment")?
            .to_owned();
        assert_eq!(url.path(), format!("/runtime-handoff/{grant}"));
        assert!(url.query().is_none());
        Ok(IssuedHandoff {
            url,
            token,
            binding: binding.to_owned(),
        })
    }

    fn runtime_cookie(&self, grant: EndpointGrantId) -> Result<String, Box<dyn Error>> {
        let origin = if grant == self.grant {
            &self.runtime
        } else if grant == self.grant_b {
            &self.runtime_b
        } else {
            return Err("unknown runtime grant".into());
        };
        let url = Url::parse(&format!("{origin}/connect/{grant}/"))?;
        Ok(self
            .jar
            .cookies(&url)
            .ok_or("missing runtime capability cookie")?
            .to_str()?
            .to_owned())
    }
}

async fn record_request_uri(
    State(log): State<Arc<Mutex<Vec<Value>>>>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    // Nginx's default $request/$request_uri sees exactly this request target, never fragments or POST bodies.
    let headers = request.headers();
    let cookies = headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let capability = cookies
        .split(';')
        .find_map(|item| item.trim().strip_prefix("__Secure-labweaver_runtime="));
    let authority = request
        .uri()
        .authority()
        .map(axum::http::uri::Authority::as_str)
        .or_else(|| {
            headers
                .get(header::HOST)
                .and_then(|value| value.to_str().ok())
        });
    let record = json!({"target": request.uri().to_string(),"path":request.uri().path(),"authority":authority,
        "site":headers.get("sec-fetch-site").and_then(|value|value.to_str().ok()),
        "origin":headers.get(header::ORIGIN).and_then(|value|value.to_str().ok()),
        "capabilitySha256":capability.map(|token|Sha256Digest::of_bytes(token.as_bytes()).to_string()),
        "hasBffCookie":cookies.contains("__Host-labweaver_session=")});
    log.lock().await.push(record);
    next.run(request).await
}

async fn seed_runtime(
    pool: &PgPool,
    instance: &EnvironmentInstance,
    grant: EndpointGrantId,
    access: contracts::AccessGrantId,
) -> Result<(), Box<dyn Error>> {
    let actor = instance.owner_id.as_uuid();
    sqlx::query("INSERT INTO environment.environment_instances(environment_id,project_id,course_id,owner_actor_id,release_id,generation,observed_generation,desired_state,observed_state,provider_binding,revision,contract) VALUES($1,$2,NULL,$3,$4,1,1,'running','ready','container-primary-v1',1,$5)")
        .bind(instance.id.as_uuid()).bind(instance.project_id.as_uuid()).bind(actor).bind(instance.release_id.as_uuid())
        .bind(serde_json::to_value(instance)?).execute(pool).await?;
    sqlx::query("INSERT INTO control.project_memberships(project_id,actor_id,role,object_role,state,revision) VALUES($1,$2,'student','editor','active',1)")
        .bind(instance.project_id.as_uuid()).bind(actor).execute(pool).await?;
    sqlx::query("INSERT INTO access.access_grants(grant_id,actor_id,project_id,environment_id,environment_revision,revision,state,not_before,expires_at,contract) VALUES($1,$2,$3,$4,1,1,'active',$5,$6,$7)")
        .bind(access.as_uuid()).bind(actor).bind(instance.project_id.as_uuid()).bind(instance.id.as_uuid())
        .bind(instance.operation.accepted_at.get()).bind(instance.operation.deadline_at.get()).bind(json!({"subjectKind":"owner"})).execute(pool).await?;
    sqlx::query("INSERT INTO access.endpoint_grants(endpoint_grant_id,grant_id,endpoint_id,endpoint_revision,protocol,action,health,expires_at,contract) VALUES($1,$2,$3,1,'http','connect','healthy',$4,'{}')")
        .bind(grant.as_uuid()).bind(access.as_uuid()).bind(instance.endpoints[0].id.as_uuid()).bind(instance.operation.deadline_at.get()).execute(pool).await?;
    Ok(())
}

async fn upstream_ws(
    State(requests): State<Arc<Mutex<Vec<Value>>>>,
    uri: Uri,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    requests.lock().await.push(json!({"path":uri.path(), "query":uri.query(), "cookie":headers.get(header::COOKIE).and_then(|value|value.to_str().ok()), "websocket":true}));
    let flood = uri.query() == Some("session_id=a%2Fb&token=x+y&flood=true");
    if !flood && uri.query() != Some("session_id=a%2Fb&token=x+y") {
        return StatusCode::BAD_REQUEST.into_response();
    }
    upgrade
        .protocols(["runtime.test.v1"])
        .on_upgrade(move |socket| async move {
            let (mut sender, mut receiver) = socket.split();
            if flood {
                let send = async {
                    loop {
                        if sender
                            .send(axum::extract::ws::Message::Binary(Bytes::from_static(
                                &[b'x'; 65_536],
                            )))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                };
                let receive = async { while let Some(Ok(_)) = receiver.next().await {} };
                tokio::select! { () = send => {}, () = receive => {} }
            } else {
                while let Some(Ok(message)) = receiver.next().await {
                    if matches!(message, axum::extract::ws::Message::Close(_)) {
                        break;
                    }
                    if sender.send(message).await.is_err() {
                        break;
                    }
                }
            }
            requests.lock().await.push(json!({"wsClosed":true}));
        })
        .into_response()
}

struct FloodProbe(Arc<Mutex<Vec<Value>>>);

impl Stream for FloodProbe {
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Ready(Some(Ok(Bytes::from_static(&[b'x'; 65_536]))))
    }
}

impl Drop for FloodProbe {
    fn drop(&mut self) {
        let requests = Arc::clone(&self.0);
        tokio::spawn(async move {
            requests.lock().await.push(json!({"floodCancelled":true}));
        });
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
    if uri.path() == "/sse" {
        let first = futures_util::stream::once(async {
            Ok::<_, std::io::Error>(Bytes::from_static(b"data: first\n\n"))
        });
        let never = futures_util::stream::pending::<Result<Bytes, std::io::Error>>();
        return Response::builder()
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from_stream(first.chain(never)))
            .unwrap_or_else(|_| Response::new(Body::empty()));
    }
    if uri.path() == "/download" {
        let first = futures_util::stream::once(async {
            Ok::<_, std::io::Error>(Bytes::from_static(b"first-chunk"))
        });
        let never = futures_util::stream::pending::<Result<Bytes, std::io::Error>>();
        return Response::builder()
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .body(Body::from_stream(first.chain(never)))
            .unwrap_or_else(|_| Response::new(Body::empty()));
    }
    if uri.path() == "/cancel" {
        return Response::builder()
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .body(Body::from_stream(CancelProbe {
                requests: Arc::clone(&requests),
                first: true,
            }))
            .unwrap_or_else(|_| Response::new(Body::empty()));
    }
    if uri.path() == "/flood" {
        return Body::from_stream(FloodProbe(Arc::clone(&requests))).into_response();
    }
    if uri.path() == "/cookie" {
        return Response::builder()
            .header(
                header::SET_COOKIE,
                "app=1; Domain \t= .runtime.localhost; Path=/; Secure; SameSite=Lax",
            )
            .body(Body::from(Bytes::from_static(b"cookie")))
            .unwrap_or_else(|_| Response::new(Body::empty()));
    }
    if uri.path() == "/dangerous" {
        return Response::builder()
            .status(StatusCode::FOUND)
            .header(header::LOCATION, "https://evil.example/steal")
            .body(Body::empty())
            .unwrap_or_else(|_| Response::new(Body::empty()));
    }
    if uri.path() == "/dangerous-relative" {
        return (
            StatusCode::FOUND,
            [(header::LOCATION, "//evil.example/steal")],
        )
            .into_response();
    }
    if uri.path() == "/cookie-api" {
        return (
            [(
                header::SET_COOKIE,
                "api_session=abc; Path=/api; Secure; SameSite=None",
            )],
            "cookie",
        )
            .into_response();
    }
    if uri.path() == "/cookie-host" {
        return (
            [(
                header::SET_COOKIE,
                "__Host-app=abc; Path=/; Secure; SameSite=Strict",
            )],
            "cookie",
        )
            .into_response();
    }
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

struct CancelProbe {
    requests: Arc<Mutex<Vec<Value>>>,
    first: bool,
}

impl Stream for CancelProbe {
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.first {
            self.first = false;
            Poll::Ready(Some(Ok(Bytes::from_static(b"first"))))
        } else {
            Poll::Pending
        }
    }
}

impl Drop for CancelProbe {
    fn drop(&mut self) {
        if let Ok(mut requests) = self.requests.try_lock() {
            requests.push(json!({"cancelled": true}));
        }
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn runtime_handoff_isolated_bound_one_time_and_live_revocation() -> Result<(), Box<dyn Error>>
{
    let fixture = Box::pin(RuntimeFixture::new()).await?;
    // Hold the store lock until issuance is demonstrably waiting inside PostgreSQL.
    // An application timestamp computed before that wait must not shorten the TTL.
    let mut store_lock = fixture.state.pool.begin().await?;
    sqlx::query("LOCK TABLE access.runtime_capabilities IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *store_lock)
        .await?;
    let issuing = fixture.handoff(fixture.grant);
    tokio::pin!(issuing);
    let await_store_wait = async {
        loop {
            let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT FROM pg_locks WHERE relation='access.runtime_capabilities'::regclass AND mode='RowExclusiveLock' AND NOT granted)")
                .fetch_one(&fixture.state.pool).await?;
            if waiting {
                return Ok::<(), Box<dyn Error>>(());
            }
            tokio::task::yield_now().await;
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::select! {
            result = &mut issuing => { let _ = result?; Err::<(), Box<dyn Error>>("issuance did not wait for PostgreSQL".into()) },
            result = await_store_wait => result,
        }
    }).await??;
    let database_release: OffsetDateTime =
        sqlx::query_scalar("SELECT date_trunc('milliseconds',clock_timestamp())")
            .fetch_one(&fixture.state.pool)
            .await?;
    store_lock.commit().await?;
    let handoff = issuing.await?;
    let runtime_url = Url::parse(&fixture.runtime)?;
    let address = std::net::SocketAddr::from((
        [127, 0, 0, 1],
        runtime_url.port().ok_or("missing runtime port")?,
    ));
    let other = reqwest::Client::builder()
        .no_proxy()
        .danger_accept_invalid_certs(true)
        .resolve(
            runtime_url.host_str().ok_or("missing runtime host")?,
            address,
        )
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let page = fixture.client.get(handoff.url.as_str()).send().await?;
    assert_eq!(page.status(), StatusCode::OK);
    assert_eq!(page.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(page.headers()["referrer-policy"], "no-referrer");
    let csp = page.headers()["content-security-policy"]
        .to_str()?
        .to_owned();
    assert!(
        csp.contains("script-src 'sha256-")
            && !csp.contains("nonce-")
            && !csp.contains("unsafe-inline")
    );
    let page_text = page.text().await?;
    assert!(!page_text.contains(&handoff.token) && !page_text.contains(&handoff.binding));
    assert_eq!(
        handoff.exchange(&other).send().await?.status(),
        StatusCode::FORBIDDEN
    );
    for origin in [
        None,
        Some(fixture.portal.as_str()),
        Some(fixture.runtime_b.as_str()),
    ] {
        let mut request = fixture
            .client
            .post(handoff.url.as_str())
            .form(&[("token", handoff.token.as_str())]);
        if let Some(origin) = origin {
            request = request.header(header::ORIGIN, origin);
        }
        assert_eq!(request.send().await?.status(), StatusCode::FORBIDDEN);
    }
    let mut crossed = handoff.url.clone();
    crossed.set_path(&format!("/runtime-handoff/{}", EndpointGrantId::new()));
    assert_eq!(
        fixture
            .client
            .post(crossed)
            .header(header::ORIGIN, &fixture.runtime)
            .form(&[("token", handoff.token.as_str())])
            .send()
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        fixture
            .client
            .get(format!(
                "{}/runtime-handoff/{}/removed-token-route",
                fixture.runtime, fixture.grant
            ))
            .send()
            .await?
            .status(),
        StatusCode::NOT_FOUND
    );
    let row = sqlx::query("SELECT c.issued_at,c.handoff_expires_at,c.expires_at,s.expires_at AS session_expires FROM access.runtime_capabilities c JOIN access.bff_sessions s ON s.session_id=c.bff_session_id")
        .fetch_one(&fixture.state.pool).await?;
    let issued_at: OffsetDateTime = row.get("issued_at");
    assert!(
        issued_at >= database_release,
        "issuance must use the database clock after the store lock, not an earlier application timestamp"
    );
    assert_eq!(
        row.get::<OffsetDateTime, _>("handoff_expires_at"),
        issued_at + Duration::seconds(30)
    );
    assert_eq!(
        row.get::<OffsetDateTime, _>("expires_at"),
        std::cmp::min(
            issued_at + Duration::seconds(300),
            row.get("session_expires")
        )
    );
    assert_eq!(issued_at.nanosecond() % 1_000_000, 0);
    let (first, second) = tokio::join!(
        handoff.exchange(&fixture.client).send(),
        handoff.exchange(&fixture.client).send()
    );
    let first = first?;
    let second = second?;
    let mut statuses = [first.status().as_u16(), second.status().as_u16()];
    statuses.sort_unstable();
    assert_eq!(
        statuses,
        [303, 403],
        "exactly one concurrent PostgreSQL redemption succeeds"
    );
    let consumed = if first.status() == StatusCode::SEE_OTHER {
        first
    } else {
        second
    };
    let cookie = consumed.headers()[header::SET_COOKIE].to_str()?.to_owned();
    assert!(cookie.contains("HttpOnly; Secure; SameSite=Lax"));
    assert!(cookie.contains(&format!("Path=/connect/{}/", fixture.grant)));
    assert!(!cookie.contains("Domain="));
    assert_eq!(
        handoff.exchange(&fixture.client).send().await?.status(),
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
    let handoff_b = fixture.handoff(fixture.grant_b).await?;
    let consumed_b = handoff_b.exchange(&fixture.client).send().await?;
    assert_eq!(consumed_b.status(), StatusCode::SEE_OTHER);
    let cookie_b = consumed_b.headers()[header::SET_COOKIE]
        .to_str()?
        .split(';')
        .next()
        .ok_or("missing capability B")?;
    assert_eq!(
        other
            .get(&target)
            .header(header::COOKIE, cookie_b)
            .send()
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );
    let target_b = format!(
        "{}/connect/{}/cross-read",
        fixture.runtime_b, fixture.grant_b
    );
    assert_eq!(
        fixture
            .client
            .get(&target_b)
            .header(header::ORIGIN, &fixture.runtime)
            .send()
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        fixture
            .client
            .post(&target_b)
            .header(header::ORIGIN, &fixture.runtime)
            .send()
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );
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
    let expired = fixture.handoff(fixture.grant).await?;
    sqlx::query("UPDATE access.runtime_capabilities SET issued_at=now()-interval '10 minutes',handoff_expires_at=now()-interval '9 minutes 30 seconds',expires_at=now()-interval '5 minutes' WHERE consumed_at IS NULL").execute(&fixture.state.pool).await?;
    assert_eq!(
        expired.exchange(&fixture.client).send().await?.status(),
        StatusCode::FORBIDDEN
    );
    sqlx::query("UPDATE access.runtime_capabilities SET issued_at=now()-interval '10 minutes',handoff_expires_at=now()-interval '9 minutes 30 seconds',expires_at=now()-interval '5 minutes' WHERE consumed_at IS NOT NULL").execute(&fixture.state.pool).await?;
    assert_eq!(
        fixture.client.get(&target).send().await?.status(),
        StatusCode::FORBIDDEN
    );
    let renewed = fixture.handoff(fixture.grant).await?;
    assert_eq!(
        renewed.exchange(&fixture.client).send().await?.status(),
        StatusCode::SEE_OTHER
    );
    let before_revoke = fixture.handoff(fixture.grant).await?;
    sqlx::query("UPDATE access.access_grants SET state='revoked',revoked_at=now(),reason_code='actor_requested'")
        .execute(&fixture.state.pool)
        .await?;
    assert_eq!(
        fixture.client.get(&target).send().await?.status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        before_revoke
            .exchange(&fixture.client)
            .send()
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );
    sqlx::query("UPDATE access.access_grants SET state='active',revoked_at=NULL,reason_code=NULL")
        .execute(&fixture.state.pool)
        .await?;
    let before_expiry = fixture.handoff(fixture.grant).await?;
    sqlx::query("UPDATE access.access_grants SET not_before=now()-interval '1 minute',expires_at=now()-interval '1 second'")
        .execute(&fixture.state.pool)
        .await?;
    assert_eq!(
        before_expiry
            .exchange(&fixture.client)
            .send()
            .await?
            .status(),
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
    let before_logout = fixture.handoff(fixture.grant).await?;
    let logout = fixture
        .client
        .post(format!("{}/auth/logout", fixture.portal))
        .header(header::ORIGIN, &fixture.portal)
        .header("X-CSRF-Token", fixture.bff.csrf_token.expose())
        .send()
        .await?;
    assert_eq!(logout.status(), StatusCode::OK);
    assert_eq!(
        before_logout
            .exchange(&fixture.client)
            .send()
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        fixture.client.get(&target).send().await?.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(fixture.upstream.lock().await.len(), requests);
    assert!(fixture.request_log.lock().await.iter().all(|entry| {
        entry["target"].as_str().is_some_and(|target| {
            !target.contains(&handoff.token) && !target.contains(&handoff.binding)
        })
    }));
    Ok(())
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn runtime_proxy_protocols_are_streamed_and_fenced() -> Result<(), Box<dyn Error>> {
    let fixture = Box::pin(RuntimeFixture::new()).await?;
    let handoff_a = fixture.handoff(fixture.grant).await?;
    assert_eq!(
        handoff_a.exchange(&fixture.client).send().await?.status(),
        StatusCode::SEE_OTHER
    );
    let handoff_b = fixture.handoff(fixture.grant_b).await?;
    assert_eq!(
        handoff_b.exchange(&fixture.client).send().await?.status(),
        StatusCode::SEE_OTHER
    );
    let cookie_a = fixture.runtime_cookie(fixture.grant)?;
    let target = |origin: &str, grant: EndpointGrantId, path: &str| {
        format!("{origin}/connect/{grant}{path}")
    };

    let plain = fixture
        .client
        .get(target(&fixture.runtime, fixture.grant, "/"))
        .send()
        .await?;
    assert_eq!(plain.status(), StatusCode::OK);
    let sse = fixture
        .client
        .get(target(&fixture.runtime, fixture.grant, "/sse"))
        .send()
        .await?;
    let mut sse_stream = sse.bytes_stream();
    let first = tokio::time::timeout(std::time::Duration::from_secs(2), sse_stream.next())
        .await?
        .ok_or("SSE ended before first event")??;
    assert_eq!(first, Bytes::from_static(b"data: first\n\n"));
    let download = fixture
        .client
        .get(target(&fixture.runtime, fixture.grant, "/download"))
        .send()
        .await?;
    let first_download = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        download.bytes_stream().next(),
    )
    .await?
    .ok_or("download ended before first chunk")??;
    assert_eq!(first_download, Bytes::from_static(b"first-chunk"));

    let cookie_response = fixture
        .client
        .get(target(&fixture.runtime, fixture.grant, "/cookie"))
        .send()
        .await?;
    let set_cookie = cookie_response
        .headers()
        .get(header::SET_COOKIE)
        .ok_or("runtime app cookie was not forwarded")?
        .to_str()?;
    assert!(!set_cookie.to_ascii_lowercase().contains("domain="));
    for path in ["/dangerous", "/dangerous-relative"] {
        let dangerous = fixture
            .client
            .get(target(&fixture.runtime, fixture.grant, path))
            .send()
            .await?;
        assert_problem_response(
            dangerous,
            StatusCode::SERVICE_UNAVAILABLE,
            "LW_ACCESS_RUNTIME_REDIRECT_REJECTED",
        )
        .await?;
    }
    for path in ["/cookie-api", "/cookie-host", "/api/refresh"] {
        assert_eq!(
            fixture
                .client
                .get(target(&fixture.runtime, fixture.grant, path))
                .send()
                .await?
                .status(),
            StatusCode::OK
        );
    }
    let refresh = fixture
        .upstream
        .lock()
        .await
        .iter()
        .find(|entry| entry["path"] == "/api/refresh")
        .cloned()
        .ok_or("missing application refresh")?;
    let app_cookies = refresh["cookie"]
        .as_str()
        .ok_or("application cookies not forwarded")?;
    assert!(app_cookies.contains("app=1"));
    assert!(app_cookies.contains("api_session=abc"));
    assert!(app_cookies.contains("__Host-app=abc"));
    assert!(!app_cookies.contains("labweaver"));
    assert!(
        fixture
            .jar
            .cookies(&Url::parse(&fixture.portal)?)
            .is_some_and(|cookies| !cookies.to_str().unwrap_or("").contains("app="))
    );
    let cross_http = fixture
        .client
        .get(target(&fixture.runtime_b, fixture.grant_b, "/"))
        .header(header::COOKIE, &cookie_a)
        .send()
        .await?;
    assert_eq!(cross_http.status(), StatusCode::FORBIDDEN);

    let ws_request = fixture.websocket_request(
        &fixture.runtime,
        fixture.grant,
        &fixture.runtime_cookie(fixture.grant)?,
        "unsupported.v0, runtime.test.v1",
    )?;
    let (mut ws, response) = fixture.websocket(ws_request).await?;
    assert_eq!(
        response
            .headers()
            .get(header::SEC_WEBSOCKET_PROTOCOL)
            .ok_or("missing selected protocol")?,
        "runtime.test.v1"
    );
    ws.send(WsMessage::Text("echo".into())).await?;
    assert_eq!(
        ws.next().await.ok_or("WS closed")??,
        WsMessage::Text("echo".into())
    );
    let ws_upstream = fixture
        .upstream
        .lock()
        .await
        .iter()
        .find(|entry| entry["websocket"] == true)
        .cloned()
        .ok_or("missing upstream WS")?;
    assert_eq!(ws_upstream["query"], "session_id=a%2Fb&token=x+y");
    assert!(
        ws_upstream["cookie"]
            .as_str()
            .is_some_and(|value| value.contains("app=1") && !value.contains("labweaver"))
    );

    let cross_ws = fixture.websocket_request(
        &fixture.runtime_b,
        fixture.grant_b,
        &cookie_a,
        "runtime.test.v1",
    )?;
    fixture
        .assert_websocket_denied(
            cross_ws,
            StatusCode::FORBIDDEN,
            "LW_ACCESS_RUNTIME_CAPABILITY_REJECTED",
        )
        .await?;
    let cancel = fixture
        .client
        .get(target(&fixture.runtime, fixture.grant, "/cancel"))
        .send()
        .await?;
    let mut cancel_stream = cancel.bytes_stream();
    assert_eq!(
        cancel_stream.next().await.ok_or("cancel probe ended")??,
        Bytes::from_static(b"first")
    );
    drop(cancel_stream);
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if fixture
                .upstream
                .lock()
                .await
                .iter()
                .any(|entry| entry["cancelled"] == true)
            {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await?;

    sqlx::query("UPDATE access.access_grants g SET state='revoked',revision=revision+1,revoked_at=now(),reason_code='fixture_revoke',updated_at=now() FROM access.endpoint_grants eg WHERE eg.grant_id=g.grant_id AND eg.endpoint_grant_id=$1")
        .bind(fixture.grant.as_uuid())
        .execute(&fixture.state.pool)
        .await?;
    let ws_closed = tokio::time::timeout(std::time::Duration::from_secs(7), ws.next()).await?;
    assert!(ws_closed.is_none_or(|result| result.is_err()));
    let sse_closed =
        tokio::time::timeout(std::time::Duration::from_secs(7), sse_stream.next()).await?;
    assert!(sse_closed.is_none());
    Ok(())
}

#[tokio::test]
async fn runtime_long_connections_recheck_capability_and_session() -> Result<(), Box<dyn Error>> {
    for change in ["capability", "session", "logout"] {
        let fixture = Box::pin(RuntimeFixture::new()).await?;
        let handoff = fixture.handoff(fixture.grant).await?;
        assert_eq!(
            handoff.exchange(&fixture.client).send().await?.status(),
            StatusCode::SEE_OTHER
        );
        let request = fixture.websocket_request(
            &fixture.runtime,
            fixture.grant,
            &fixture.runtime_cookie(fixture.grant)?,
            "runtime.test.v1",
        )?;
        let (mut ws, _) = fixture.websocket(request).await?;
        ws.send(WsMessage::Text("ready".into())).await?;
        assert_eq!(
            ws.next().await.ok_or("WS closed before revocation")??,
            WsMessage::Text("ready".into())
        );
        let sse = fixture
            .client
            .get(format!("{}/connect/{}/sse", fixture.runtime, fixture.grant))
            .send()
            .await?;
        let mut sse = sse.bytes_stream();
        assert_eq!(
            sse.next().await.ok_or("SSE closed before revocation")??,
            Bytes::from_static(b"data: first\n\n")
        );
        match change {
            "capability" => {
                sqlx::query("UPDATE access.runtime_capabilities SET issued_at=now()-interval '10 minutes',handoff_expires_at=now()-interval '9 minutes 30 seconds',expires_at=now()-interval '5 minutes' WHERE consumed_at IS NOT NULL").execute(&fixture.state.pool).await?;
            }
            "session" => {
                sqlx::query("UPDATE access.bff_sessions SET revoked_at=now() WHERE session_id=$1")
                    .bind(fixture.bff.session_id)
                    .execute(&fixture.state.pool)
                    .await?;
            }
            "logout" => {
                let response = fixture
                    .client
                    .post(format!("{}/auth/logout", fixture.portal))
                    .header(header::ORIGIN, &fixture.portal)
                    .header("X-CSRF-Token", fixture.bff.csrf_token.expose())
                    .send()
                    .await?;
                assert_eq!(response.status(), StatusCode::OK);
            }
            _ => unreachable!(),
        }
        let closed = tokio::time::timeout(std::time::Duration::from_secs(8), ws.next()).await?;
        assert!(
            closed.is_none_or(|message| message.is_err()),
            "WS survived {change}"
        );
        let closed = tokio::time::timeout(std::time::Duration::from_secs(8), sse.next()).await?;
        assert!(closed.is_none(), "SSE survived {change}");
    }
    Ok(())
}

#[tokio::test]
async fn runtime_websocket_cross_grant_denial_is_http_not_tls() -> Result<(), Box<dyn Error>> {
    let fixture = Box::pin(RuntimeFixture::new()).await?;
    let handoff = fixture.handoff(fixture.grant).await?;
    assert_eq!(
        handoff.exchange(&fixture.client).send().await?.status(),
        StatusCode::SEE_OTHER
    );
    let cookie = fixture.runtime_cookie(fixture.grant)?;
    let allowed =
        fixture.websocket_request(&fixture.runtime, fixture.grant, &cookie, "runtime.test.v1")?;
    let (mut ws, _) = fixture.websocket(allowed).await?;
    ws.send(WsMessage::Text("allowed".into())).await?;
    assert_eq!(
        ws.next().await.ok_or("positive WS failed")??,
        WsMessage::Text("allowed".into())
    );
    let denied = fixture.websocket_request(
        &fixture.runtime_b,
        fixture.grant_b,
        &cookie,
        "runtime.test.v1",
    )?;
    fixture
        .assert_websocket_denied(
            denied,
            StatusCode::FORBIDDEN,
            "LW_ACCESS_RUNTIME_CAPABILITY_REJECTED",
        )
        .await?;
    Ok(())
}

#[tokio::test]
async fn runtime_websocket_rejects_unsupported_protocol_before_upgrade()
-> Result<(), Box<dyn Error>> {
    let fixture = Box::pin(RuntimeFixture::new()).await?;
    let handoff = fixture.handoff(fixture.grant).await?;
    assert_eq!(
        handoff.exchange(&fixture.client).send().await?.status(),
        StatusCode::SEE_OTHER
    );
    let request = fixture.websocket_request(
        &fixture.runtime,
        fixture.grant,
        &fixture.runtime_cookie(fixture.grant)?,
        "unsupported.v1, unsupported.v2",
    )?;
    let error = fixture
        .websocket(request)
        .await
        .err()
        .ok_or("unsupported protocol upgraded")?;
    let Some(tungstenite::Error::Http(response)) = error.downcast_ref::<tungstenite::Error>()
    else {
        return Err(
            format!("unsupported protocol failed after downstream upgrade: {error}").into(),
        );
    };
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    Ok(())
}

#[tokio::test]
async fn runtime_websocket_requires_and_rechecks_edit() -> Result<(), Box<dyn Error>> {
    let fixture = Box::pin(RuntimeFixture::new()).await?;
    let handoff = fixture.handoff(fixture.grant).await?;
    assert_eq!(
        handoff.exchange(&fixture.client).send().await?.status(),
        StatusCode::SEE_OTHER
    );
    sqlx::query("UPDATE control.project_memberships SET object_role='viewer'")
        .execute(&fixture.state.pool)
        .await?;
    let request = fixture.websocket_request(
        &fixture.runtime,
        fixture.grant,
        &fixture.runtime_cookie(fixture.grant)?,
        "runtime.test.v1",
    )?;
    fixture
        .assert_websocket_denied(request, StatusCode::FORBIDDEN, "LW_AUTH_SCOPE_DENIED")
        .await?;
    sqlx::query("UPDATE control.project_memberships SET object_role='editor'")
        .execute(&fixture.state.pool)
        .await?;
    let request = fixture.websocket_request(
        &fixture.runtime,
        fixture.grant,
        &fixture.runtime_cookie(fixture.grant)?,
        "runtime.test.v1",
    )?;
    let (mut ws, _) = fixture.websocket(request).await?;
    ws.send(WsMessage::Text("write allowed".into())).await?;
    assert_eq!(
        ws.next().await.ok_or("WS closed before downgrade")??,
        WsMessage::Text("write allowed".into())
    );
    sqlx::query("UPDATE control.project_memberships SET object_role='viewer'")
        .execute(&fixture.state.pool)
        .await?;
    let closed = tokio::time::timeout(std::time::Duration::from_secs(8), ws.next()).await?;
    assert!(
        closed.is_none_or(|message| message.is_err()),
        "WS survived editor to viewer downgrade"
    );
    Ok(())
}

#[tokio::test]
async fn runtime_revocation_supervision_survives_backpressure_and_authority_stall()
-> Result<(), Box<dyn Error>> {
    for failure in ["revoked", "authority_stall"] {
        let fixture = Box::pin(RuntimeFixture::new()).await?;
        let handoff = fixture.handoff(fixture.grant).await?;
        assert_eq!(
            handoff.exchange(&fixture.client).send().await?.status(),
            StatusCode::SEE_OTHER
        );
        let cookie = fixture.runtime_cookie(fixture.grant)?;
        let mut headers = HeaderMap::new();
        headers.insert(
            header::HOST,
            Url::parse(&fixture.runtime)?.authority().parse()?,
        );
        headers.insert(header::COOKIE, cookie.parse()?);
        // Never poll this body: supervision must not depend on downstream demand.
        let body = proxy::forward_runtime(
            State(Arc::clone(&fixture.state)),
            Method::GET,
            format!("/connect/{}/flood", fixture.grant).parse()?,
            headers,
            Bytes::new(),
        )
        .await
        .map_err(|error| std::io::Error::other(error.diagnostic))?;
        assert_eq!(body.status(), StatusCode::OK);
        let mut request = fixture.websocket_request(
            &fixture.runtime,
            fixture.grant,
            &cookie,
            "runtime.test.v1",
        )?;
        *request.uri_mut() = format!("{}&flood=true", request.uri()).parse()?;
        let (ws, _) = fixture.websocket(request).await?;
        // Leave the browser unread until both forwarding directions encounter backpressure.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let mut lock = fixture.state.pool.begin().await?;
        if failure == "authority_stall" {
            sqlx::query("LOCK TABLE access.runtime_capabilities IN ACCESS EXCLUSIVE MODE")
                .execute(&mut *lock)
                .await?;
        } else {
            sqlx::query("UPDATE access.bff_sessions SET revoked_at=now() WHERE session_id=$1")
                .bind(fixture.bff.session_id)
                .execute(&fixture.state.pool)
                .await?;
        }
        tokio::time::timeout(std::time::Duration::from_secs(8), async {
            loop {
                let entries = fixture.upstream.lock().await;
                let http_closed = entries.iter().any(|entry| entry["floodCancelled"] == true);
                let ws_closed = entries.iter().any(|entry| entry["wsClosed"] == true);
                drop(entries);
                if http_closed && ws_closed {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .map_err(|error| {
            format!("{failure}: upstream remained open under backpressure: {error}")
        })?;
        lock.rollback().await?;
        drop(body);
        drop(ws);
    }
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
    input.write_all(format!("{}\n", json!({"portal":fixture.portal,"runtime":fixture.runtime,"grant":fixture.grant,"runtimeB":fixture.runtime_b,"grantB":fixture.grant_b,"session":fixture.bff.session_id})).as_bytes()).await?;
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
    assert!(fixture.upstream.lock().await.iter().all(|request| {
        !request["path"]
            .as_str()
            .is_some_and(|path| path.starts_with("/cross-"))
    }));
    let b_capability: String = sqlx::query_scalar("SELECT capability_sha256 FROM access.runtime_capabilities WHERE endpoint_grant_id=$1 AND consumed_at IS NOT NULL")
        .bind(fixture.grant_b.as_uuid()).fetch_one(&fixture.state.pool).await?;
    let wire = fixture.request_log.lock().await;
    assert!(wire.iter().all(|entry| {
        entry["target"]
            .as_str()
            .is_some_and(|target| !target.contains("binding="))
    }));
    assert!(
        wire.iter()
            .filter(|entry| entry["authority"]
                .as_str()
                .is_some_and(|authority| authority.contains(".runtime.localhost")))
            .all(|entry| entry["hasBffCookie"] == false)
    );
    for suffix in ["/cross-read", "/cross-write"] {
        let entry = wire
            .iter()
            .find(|entry| {
                entry["path"]
                    .as_str()
                    .is_some_and(|path| path.ends_with(suffix))
            })
            .ok_or("browser attack never reached Access")?;
        assert_eq!(entry["site"].as_str(), Some("same-site"));
        assert_eq!(entry["origin"].as_str(), Some(fixture.runtime.as_str()));
        assert!(
            entry["capabilitySha256"].as_str() == Some(b_capability.as_str()),
            "B's valid host-only Cookie must be automatically attached on the rejected request"
        );
    }
    Ok(())
}

async fn nginx_fault_proxy(
    fixture: &RuntimeFixture,
    upstream: &str,
) -> Result<testcontainers::ContainerAsync<testcontainers::GenericImage>, Box<dyn Error>> {
    use testcontainers::core::IntoContainerPort;
    // Exactly the configuration deployed by Kind; only external infrastructure
    // addresses and timeout duration are substituted for controlled TCP faults.
    let source = include_str!("../../../deploy/config/local-dev-portal.nginx.conf");
    assert!(source.contains("error_log /dev/stderr warn;"));
    assert!(source.contains("access_log /dev/stdout;"));
    let config = source
        .replace("kube-dns.kube-system.svc.cluster.local", "127.0.0.11")
        .replace(
            "access-service.labweaver-system.svc.cluster.local:8080",
            upstream,
        )
        .replace(
            "proxy_buffering off;",
            "proxy_buffering off;\n  proxy_connect_timeout 1s;\n  proxy_read_timeout 1s;",
        );
    let image = include_str!("../../../containers/Containerfile.web")
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("ARG WEB_RUNTIME=\""))
        .and_then(|line| line.strip_suffix('"'))
        .ok_or("missing pinned Kind Web/Nginx runtime")?;
    let (name, tag) = image
        .split_once(':')
        .ok_or("invalid pinned Nginx image reference")?;
    let edge = testcontainers::GenericImage::new(name, tag)
        .with_exposed_port(8443.tcp())
        .with_copy_to("/etc/nginx/nginx.conf", config.into_bytes())
        .with_copy_to(
            "/etc/labweaver/tls/tls.crt",
            fixture.certificate_pem.as_bytes().to_vec(),
        )
        .with_copy_to(
            "/etc/labweaver/tls/tls.key",
            fixture.private_key_pem.as_bytes().to_vec(),
        )
        .with_copy_to(
            "/etc/labweaver/trust/ca.crt",
            fixture.certificate_pem.as_bytes().to_vec(),
        )
        .with_cmd(["nginx", "-c", "/etc/nginx/nginx.conf", "-g", "daemon off;"])
        .start()
        .await?;
    let port = edge.get_host_port_ipv4(8443).await?;
    let ready = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if let Ok(response) = fixture
                .client
                .get(format!("https://127.0.0.1:{port}/health/ready"))
                .send()
                .await
                && response.status() == StatusCode::OK
            {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await;
    if ready.is_err() {
        return Err(format!(
            "real Nginx did not become ready: {}",
            String::from_utf8_lossy(&edge.stderr_to_vec().await?)
        )
        .into());
    }
    Ok(edge)
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "the security regression keeps real minting, pinned shared Nginx configuration, and both fault/log assertions together"
)]
async fn runtime_handoff_credentials_never_enter_real_nginx_fault_logs()
-> Result<(), Box<dyn Error>> {
    let mut fixture = Box::pin(RuntimeFixture::new()).await?;
    let handoff = fixture.handoff(fixture.grant).await?;
    let network = tokio::process::Command::new("docker")
        .args([
            "network",
            "inspect",
            "bridge",
            "--format",
            "{{(index .IPAM.Config 0).Gateway}}",
        ])
        .output()
        .await?;
    if !network.status.success() {
        return Err("cannot resolve the local Docker host gateway for the timeout fixture".into());
    }
    let gateway: std::net::IpAddr = std::str::from_utf8(&network.stdout)?.trim().parse()?;
    let blackhole = TcpListener::bind("0.0.0.0:0").await?;
    let timeout_upstream =
        std::net::SocketAddr::new(gateway, blackhole.local_addr()?.port()).to_string();
    // This is deliberate network fault injection, never a business authorization backend.
    fixture.tasks.0.push(tokio::spawn(async move {
        let mut connections = Vec::new();
        while let Ok((connection, _)) = blackhole.accept().await {
            connections.push(connection);
        }
    }));
    for (upstream, status, diagnostic) in [
        ("127.0.0.1:9", StatusCode::BAD_GATEWAY, "connect() failed"),
        (
            timeout_upstream.as_str(),
            StatusCode::GATEWAY_TIMEOUT,
            "upstream timed out",
        ),
    ] {
        let edge = nginx_fault_proxy(&fixture, upstream).await?;
        let port = edge.get_host_port_ipv4(8443).await?;
        let mut runtime_url = handoff.url.clone();
        runtime_url
            .set_port(Some(port))
            .map_err(|()| "invalid Nginx port")?;
        let page = fixture.client.get(runtime_url.as_str()).send().await?;
        assert_eq!(page.status(), status);
        let _ = page.bytes().await?;
        let exchange = fixture
            .client
            .post(runtime_url.as_str())
            .header(header::ORIGIN, runtime_url.origin().ascii_serialization())
            .form(&[("token", handoff.token.as_str())])
            .send()
            .await?;
        assert_eq!(exchange.status(), status);
        let _ = exchange.bytes().await?;
        let mut portal_url = Url::parse(&fixture.portal)?;
        portal_url
            .set_port(Some(port))
            .map_err(|()| "invalid Nginx port")?;
        portal_url.set_path(&format!("/connect/{}/", fixture.grant));
        portal_url.set_query(Some("handoff=true"));
        portal_url.set_fragment(Some(&format!("binding={}", handoff.binding)));
        let portal = fixture.client.get(portal_url.as_str()).send().await?;
        assert_eq!(portal.status(), status);
        let _ = portal.bytes().await?;
        let binding = fixture
            .client
            .post(portal_url.as_str())
            .header(header::ORIGIN, portal_url.origin().ascii_serialization())
            .header("X-CSRF-Token", fixture.bff.csrf_token.expose())
            .json(&json!({"binding":handoff.binding}))
            .send()
            .await?;
        assert_eq!(binding.status(), status);
        let _ = binding.bytes().await?;
        edge.stop().await?;
        let access = String::from_utf8(edge.stdout_to_vec().await?)?;
        let errors = String::from_utf8(edge.stderr_to_vec().await?)?;
        for log in [&access, &errors] {
            assert!(
                !log.contains(&handoff.token),
                "a real handoff token entered Nginx logs"
            );
            assert!(
                !log.contains(&handoff.binding),
                "a real browser binding entered Nginx logs"
            );
            assert!(
                log.contains(&fixture.grant.to_string()),
                "grant correlation must remain available"
            );
        }
        assert!(
            access.contains(&format!(" {} ", status.as_u16())),
            "access logs must retain the failure status"
        );
        assert!(
            errors.contains(diagnostic),
            "warn-level error logs must retain the upstream failure cause"
        );
    }
    assert!(fixture.upstream.lock().await.is_empty());
    let consumed: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM access.runtime_capabilities WHERE consumed_at IS NOT NULL",
    )
    .fetch_one(&fixture.state.pool)
    .await?;
    assert_eq!(consumed, 0);
    println!(
        "PASS: real Kind Nginx config retains warn/access diagnostics for 502 and 504; actual issued token and binding never enter either log"
    );
    Ok(())
}
