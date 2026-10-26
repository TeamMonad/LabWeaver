//! Real Access, Control, Agent and Resource routers, `PostgreSQL`, TLS and signed service JWTs.
//! Only the external OIDC issuer is a fixture. No business handler or principal is substituted.
#![allow(clippy::large_futures)]

use super::*;
use contracts::{ActorId, AgentRunId, ProjectId, Revision};
use control_service::clients::{
    AccessClient, AgentClient, EnvironmentClient, EvaluationClient, ServiceHttpClientConfig,
};
use persistence_sqlx::{Domain, MigrationCatalog};

const CLIENT_ID: &str = "access-test";
const DELEGATION_KEY: [u8; 32] = [7; 32];

struct ListFixture {
    _tasks: Tasks,
    _postgres: testcontainers::ContainerAsync<Postgres>,
    _runtime: tempfile::TempDir,
    http: reqwest::Client,
    app: Arc<AppState>,
    tokens: Arc<ServiceTokenClient>,
    access: String,
    control: String,
    agent: String,
    resource: String,
    project: ProjectId,
    sessions: Vec<BffSession>,
    agent_queries: Arc<Mutex<Vec<String>>>,
}

// The same external-authority fixture pattern as notification_browser_tests; roles are issued
// for the requested audience/scopes and every owner verifies the actual JWT signature.
async fn list_token(
    State(authority): State<Authority>,
    headers: HeaderMap,
    Form(form): Form<std::collections::HashMap<String, String>>,
) -> Response {
    let credentials = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode("access-test:fixture-secret")
    );
    if headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        != Some(credentials.as_str())
        || form.get("grant_type").map(String::as_str) != Some("client_credentials")
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let mut header = Header::new(Algorithm::ES256);
    header.kid = Some("fixture".to_owned());
    let claims = json!({"iss":authority.issuer,"aud":["labweaver-access","labweaver-control","labweaver-agent","labweaver-resource"],"azp":CLIENT_ID,"sub":"service-account-access","iat":now,"nbf":now-1,"exp":now+300,"resource_access":{"labweaver-access":{"roles":["access.authorization.decide"]},"labweaver-control":{"roles":["access.control.forward"]},"labweaver-agent":{"roles":["agent.control.invoke"]},"labweaver-resource":{"roles":["resource.api.invoke"]}}});
    match jsonwebtoken::encode(&header, &claims, &authority.signing_key) {
        Ok(token) => Json(json!({"access_token":token,"token_type":"Bearer","expires_in":300}))
            .into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn verifier(
    issuer: &str,
    audience: &str,
    http: &reqwest::Client,
) -> Result<Arc<ServiceTokenVerifier>, Box<dyn Error>> {
    Ok(Arc::new(
        ServiceTokenVerifier::discover(
            ServiceAuthConfig::new(
                issuer,
                audience.to_owned(),
                BTreeSet::from([CLIENT_ID.to_owned()]),
                BTreeSet::new(),
                BTreeSet::from(["ES256".to_owned()]),
                600,
                1,
                TransportSecurityMode::Strict,
            )?,
            http.clone(),
        )
        .await?,
    ))
}

async fn domain_migrations(pool: &PgPool, domain: Domain) -> Result<(), Box<dyn Error>> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
    let catalog = MigrationCatalog::load(&root.join("catalog.yaml"))?;
    let migrations = catalog
        .domains
        .iter()
        .find(|entry| entry.name == domain)
        .ok_or("domain missing")?;
    let mut connection = pool.acquire().await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "CREATE SCHEMA {}; SET search_path = {},pg_catalog",
        domain.schema(),
        domain.schema()
    )))
    .execute(&mut *connection)
    .await?;
    for migration in &migrations.migrations {
        sqlx::raw_sql(sqlx::AssertSqlSafe(MigrationCatalog::read_verified_sql(
            &root, migration,
        )?))
        .execute(&mut *connection)
        .await?;
    }
    Ok(())
}
fn serve(tasks: &mut Tasks, listener: TcpListener, router: Router, tls: Arc<rustls::ServerConfig>) {
    tasks.0.push(tokio::spawn(async move {
        let _ = http_transport::serve_tls(listener, router, tls).await;
    }));
}

impl ListFixture {
    #[allow(
        clippy::too_many_lines,
        reason = "assemble real authenticated owner routers once for the four boundary tests"
    )]
    async fn new() -> Result<Self, Box<dyn Error>> {
        let postgres = Postgres::default().with_tag("17.5-alpine").start().await?;
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect(&format!(
                "postgres://postgres:postgres@127.0.0.1:{}/postgres",
                postgres.get_host_port_ipv4(5432).await?
            ))
            .await?;
        support::apply_access_migrations(&pool).await?;
        domain_migrations(&pool, Domain::Agent).await?;
        domain_migrations(&pool, Domain::Resource).await?;
        let mut tasks = Tasks(Vec::new());
        let (app, _) = Box::pin(state(pool.clone(), Uuid::now_v7(), &mut tasks)).await?;
        let mut app = Arc::unwrap_or_clone(app);
        let tls = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_owned()])?;
        let ca = tls.cert.pem();
        let server_tls = http_transport::server_config(
            ca.as_bytes(),
            tls.signing_key.serialize_pem().as_bytes(),
        )?;
        let runtime = tempfile::tempdir()?;
        let ca_path = runtime.path().join("query-service-ca.pem");
        std::fs::write(&ca_path, &ca)?;
        let issuer_listener = TcpListener::bind("127.0.0.1:0").await?;
        let issuer = format!("https://{}/realms/test", issuer_listener.local_addr()?);
        let key = KeyPair::generate()?;
        let public = key.public_key_raw();
        let x = URL_SAFE_NO_PAD.encode(&public[1..33]);
        let y = URL_SAFE_NO_PAD.encode(&public[33..65]);
        let authority = Authority {
            issuer: issuer.clone(),
            admin: Uuid::now_v7(),
            requests: Arc::new(Mutex::new(Vec::new())),
            signing_key: Arc::new(EncodingKey::from_ec_der(&key.serialize_der())),
            decoding_key: DecodingKey::from_ec_components(&x, &y)?,
            jwk: json!({"kty":"EC","crv":"P-256","x":x,"y":y,"use":"sig","alg":"ES256","kid":"fixture"}),
            upstream: "control",
            environment: None,
        };
        let issuer_router = Router::new()
            .route(
                "/realms/test/.well-known/openid-configuration",
                get(discovery),
            )
            .route("/realms/test/jwks", get(jwks))
            .route("/realms/test/token", post(list_token))
            .with_state(authority);
        serve(
            &mut tasks,
            issuer_listener,
            issuer_router,
            Arc::clone(&server_tls),
        );
        let http = no_redirect_http_client(Some(ca.as_bytes()), TransportSecurityMode::Strict)?;
        let tokens = Arc::new(
            ServiceTokenClient::discover(
                ServiceTokenClientConfig::new(
                    &issuer,
                    CLIENT_ID.to_owned(),
                    "fixture-secret".to_owned(),
                    "labweaver-control".to_owned(),
                    BTreeSet::from(["access.control.forward".to_owned()]),
                    30,
                    TransportSecurityMode::Strict,
                )?,
                http.clone(),
            )
            .await?,
        );
        app.service_token_verifier = verifier(&issuer, "labweaver-access", &http).await?;
        let access_internal_listener = TcpListener::bind("127.0.0.1:0").await?;
        let access_internal = format!("https://{}/", access_internal_listener.local_addr()?);
        serve(
            &mut tasks,
            access_internal_listener,
            internal_router(Arc::new(app.clone())),
            Arc::clone(&server_tls),
        );

        // List handlers never touch objects; install the normal S3 client, not an alternate
        // business implementation. The external endpoint is deliberately unused by this suite.
        let objects = Arc::new(
            artifact_store::S3ImmutableObjectStore::new_for_test(
                artifact_store::S3StoreConfig {
                    binding: "query-test".to_owned(),
                    endpoint: "http://127.0.0.1:9".parse()?,
                    bucket: "query-test".to_owned(),
                    region: "us-east-1".to_owned(),
                    object_prefix: "materials".to_owned(),
                    upload_ttl_seconds: 300,
                    max_object_bytes: 1024,
                    force_path_style: true,
                    ca_bundle_file: None,
                },
                artifact_store::S3Credential {
                    access_key_id: "query-test".to_owned(),
                    secret_access_key: "query-test-secret".to_owned(),
                    session_token: None,
                },
            )
            .await?,
        );
        let agent_listener = TcpListener::bind("127.0.0.1:0").await?;
        let agent = format!("https://{}/", agent_listener.local_addr()?);
        let agent_state = Arc::new(agent_service::api::AgentApiState {
            resource: None,
            store: agent_service::run_store::PostgresAgentRunStore::new(pool.clone()),
            build_store: agent_service::build_store::PgBuildStore::new(pool.clone()),
            generated_artifacts: agent_service::generated_artifacts::GeneratedArtifactStore::new(
                pool.clone(),
            ),
            llm_reviews: agent_service::llm_review::LlmReviewStore::new(pool.clone()),
            platform_images: agent_service::platform_images::PgPlatformImageCatalog::new(
                pool.clone(),
            ),
            platform_registry: None,
            objects: objects.clone(),
            platform_image_import_jobs:
                agent_service::platform_image_jobs::PlatformImageImportJobStore::new(pool.clone()),
        });
        let agent_queries = Arc::new(Mutex::new(Vec::new()));
        let capture = Arc::clone(&agent_queries);
        let agent_router = agent_service::api::with_service_auth(
            agent_service::api::router(agent_state),
            verifier(&issuer, "labweaver-agent", &http).await?,
        )
        .layer(axum::middleware::from_fn(
            move |request: axum::extract::Request, next: axum::middleware::Next| {
                let capture = Arc::clone(&capture);
                async move {
                    capture
                        .lock()
                        .await
                        .push(request.uri().query().unwrap_or_default().to_owned());
                    next.run(request).await
                }
            },
        ));
        serve(
            &mut tasks,
            agent_listener,
            agent_router,
            Arc::clone(&server_tls),
        );

        let resource_listener = TcpListener::bind("127.0.0.1:0").await?;
        let resource = format!("https://{}/", resource_listener.local_addr()?);
        let resource_store = resource_service::store::PgResourceStore::new(pool.clone());
        let resource_state = resource_service::api::ResourceApiState::new(resource_store.clone())
            .with_service_verifier(verifier(&issuer, "labweaver-resource", &http).await?)
            .with_access_service_client_id(CLIENT_ID.to_owned());
        let resource_router = resource_service::api::with_delegation(
            resource_service::api::resource_api_router(resource_state),
            Arc::new(DELEGATION_KEY.to_vec()),
        );
        serve(
            &mut tasks,
            resource_listener,
            resource_router,
            Arc::clone(&server_tls),
        );

        let control_listener = TcpListener::bind("127.0.0.1:0").await?;
        let control = format!("https://{}/", control_listener.local_addr()?);
        let client_config = |base: &str| -> Result<ServiceHttpClientConfig, Box<dyn Error>> {
            Ok(ServiceHttpClientConfig {
                base_url: base.parse()?,
                ca_certificate_file: ca_path.to_string_lossy().into_owned(),
                timeout_milliseconds: 5000,
            })
        };
        let config: serde_yaml::Value = serde_yaml::from_str(include_str!(
            "../../../deploy/config/control-plane.yaml.example"
        ))?;
        let control_service = control_service::ControlService::new(
            pool.clone(),
            objects,
            serde_yaml::from_value(config["control"].clone())?,
        )?;
        let control_verifier = verifier(&issuer, "labweaver-control", &http).await?;
        let control_state = Arc::new(control_service::api::ApiState {
            control: control_service,
            access: AccessClient::new_authenticated(
                client_config(&access_internal)?,
                Arc::clone(&tokens),
            )?,
            agent: AgentClient::new_authenticated(client_config(&agent)?, Arc::clone(&tokens))?,
            environment: EnvironmentClient::new_authenticated(
                client_config(&control)?,
                Arc::clone(&tokens),
            )?,
            evaluation: EvaluationClient::new_authenticated(
                client_config(&control)?,
                Arc::clone(&tokens),
            )?,
            service_token_verifier: Arc::clone(&control_verifier),
        });
        serve(
            &mut tasks,
            control_listener,
            control_service::api::authenticated_router(&control_state, &control_verifier),
            Arc::clone(&server_tls),
        );
        let mut control_config = app.deployment.control_gateway.clone();
        control_config.base_uri.clone_from(&control);
        control_config.allowed_server_sans = vec!["127.0.0.1".to_owned()];
        app.control_proxy = proxy::ControlGatewayProxy::new(
            &control_config,
            ca.as_bytes(),
            TransportSecurityMode::Strict,
            Arc::clone(&tokens),
            ServiceTokenTarget {
                audience: "labweaver-control".to_owned(),
                scopes: BTreeSet::from(["access.control.forward".to_owned()]),
            },
        )?;
        let mut resource_config = app.deployment.resource_gateway.clone();
        resource_config.base_uri.clone_from(&resource);
        resource_config.allowed_server_sans = vec!["127.0.0.1".to_owned()];
        app.resource_proxy = proxy::ResourceGatewayProxy::new(
            &resource_config,
            ca.as_bytes(),
            &DELEGATION_KEY,
            TransportSecurityMode::Strict,
            Arc::clone(&tokens),
            ServiceTokenTarget {
                audience: "labweaver-resource".to_owned(),
                scopes: BTreeSet::from(["resource.api.invoke".to_owned()]),
            },
        )?;
        let mut sessions = Vec::new();
        for role in [
            contracts::PlatformRole::PlatformAdmin,
            contracts::PlatformRole::Teacher,
            contracts::PlatformRole::Student,
        ] {
            sessions.push(session(&pool, &app.key_ring, Uuid::now_v7(), role).await?);
        }
        let project = seed_project_and_histories(&pool, &resource_store, &sessions).await?;
        let access_listener = TcpListener::bind("127.0.0.1:0").await?;
        let access = format!("https://{}/", access_listener.local_addr()?);
        app.config
            .allowed_origins
            .insert(access.trim_end_matches('/').to_owned());
        app.deployment.browser.allowed_origins = app.config.allowed_origins.clone();
        let app = Arc::new(app);
        serve(
            &mut tasks,
            access_listener,
            browser_router(Arc::clone(&app)),
            server_tls,
        );
        Ok(Self {
            _tasks: tasks,
            _postgres: postgres,
            _runtime: runtime,
            http,
            app,
            tokens,
            access,
            control,
            agent,
            resource,
            project,
            sessions,
            agent_queries,
        })
    }

    async fn service_get(
        &self,
        base: &str,
        path: &str,
        audience: &str,
        scope: &str,
    ) -> Result<reqwest::RequestBuilder, Box<dyn Error>> {
        let token = self
            .tokens
            .access_token_for(audience, &BTreeSet::from([scope.to_owned()]))
            .await?;
        Ok(self.http.get(format!("{base}{path}")).bearer_auth(token))
    }

    fn browser_get(&self, path: &str, session: &BffSession) -> reqwest::RequestBuilder {
        self.http
            .get(format!("{}{path}", self.access))
            .header(
                header::COOKIE,
                format!(
                    "{}={}",
                    self.app.deployment.browser.session_cookie_name, session.session_id
                ),
            )
            .header(header::ORIGIN, self.access.trim_end_matches('/'))
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "persist coherent domain histories and actual membership/session identities for router tests"
)]
async fn seed_project_and_histories(
    pool: &PgPool,
    resource: &resource_service::store::PgResourceStore,
    sessions: &[BffSession],
) -> Result<ProjectId, Box<dyn Error>> {
    use contracts::authoring::{
        AgentAttempt, AgentAttemptState, AgentRun, AgentRunPurpose, AgentRunState, AgentTrack,
        AgentTrackKind, EnvironmentClass, LlmUsage,
    };
    use contracts::resource::{
        ResourceRequest, ResourceRequestState, ResourceTarget, WorkloadResources,
    };
    let project_id = ProjectId::new();
    let owner_actor_id: ActorId = sessions[1].actor_id.to_string().parse()?;
    let now = resource.current_time().await?;
    let project = contracts::Project {
        id: project_id,
        owner_actor_id,
        name: "List query regression".to_owned(),
        description: None,
        course_id: None,
        state: contracts::ProjectState::Active,
        revision: Revision::new(1)?,
        created_at: now,
        updated_at: now,
    };
    sqlx::query("INSERT INTO control.projects(project_id,owner_actor_id,name,course_id,state,revision,created_at,updated_at,contract) VALUES($1,$2,$3,NULL,'active',1,$4,$4,$5)")
        .bind(project_id.as_uuid()).bind(project.owner_actor_id.as_uuid()).bind(&project.name).bind(now.get()).bind(serde_json::to_value(project)?).execute(pool).await?;
    for session in &sessions[1..] {
        let role = if session.roles.contains(&contracts::PlatformRole::Teacher) {
            "teacher"
        } else {
            "student"
        };
        sqlx::query("INSERT INTO control.project_memberships(project_id,actor_id,role,object_role,state,revision) VALUES($1,$2,$3,'viewer','active',1)").bind(project_id.as_uuid()).bind(session.actor_id).bind(role).execute(pool).await?;
    }
    // The global admin intentionally has no project membership.
    for state in [
        AgentRunState::Requested,
        AgentRunState::Running,
        AgentRunState::Failed,
    ] {
        let tracks = [AgentTrackKind::Environment, AgentTrackKind::Evaluation]
            .into_iter()
            .map(|kind| AgentTrack {
                kind,
                candidate_id: None,
                attempts: if state == AgentRunState::Requested {
                    Vec::new()
                } else {
                    vec![AgentAttempt {
                        number: 1,
                        state: if state == AgentRunState::Running {
                            AgentAttemptState::Running
                        } else {
                            AgentAttemptState::Failed
                        },
                        checkpoint: None,
                        usage: LlmUsage {
                            input_tokens: 0,
                            output_tokens: 0,
                            requests: 0,
                            cost_microusd: 0,
                        },
                        usage_observed: false,
                        diagnostic_code: (state == AgentRunState::Failed)
                            .then(|| "LW_AGENT_PROVIDER_FAILED".to_owned()),
                    }]
                },
            })
            .collect();
        let run = AgentRun {
            id: AgentRunId::new(),
            project_id,
            course_id: None,
            package_id: contracts::ProblemPackageId::new(),
            policy_id: contracts::PolicyId::new(),
            policy_revision: Revision::new(1)?,
            purpose: AgentRunPurpose::Authoring {
                environment_class: EnvironmentClass::Experiment,
            },
            state,
            revision: Revision::new(1)?,
            tracks,
            plan: None,
        };
        run.validate()?;
        sqlx::query("INSERT INTO agent.agent_runs(run_id,project_id,course_id,problem_package_id,revision,state,provider_binding,input_sha256,policy_revision,purpose,contract) VALUES($1,$2,NULL,$3,1,$4,'claude-code-test',$5,1,$6,$7)")
            .bind(run.id.as_uuid()).bind(project_id.as_uuid()).bind(run.package_id.as_uuid()).bind(serde_json::to_value(state)?.as_str().ok_or("state string missing")?).bind(Sha256Digest::of_bytes(run.id.to_string().as_bytes()).to_string()).bind(serde_json::to_value(run.purpose)?).bind(serde_json::to_value(run)?).execute(pool).await?;
    }
    for session in sessions {
        let requester_id: ActorId = session.actor_id.to_string().parse()?;
        for cancel in [false, true] {
            let request = ResourceRequest {
                id: contracts::ResourceRequestId::new(),
                generation: 1,
                request_key: format!("query-{}-{cancel}", session.actor_id),
                requester_id,
                project_id,
                course_id: None,
                target: ResourceTarget::Environment {
                    environment_id: contracts::EnvironmentId::new(),
                    release_id: contracts::ReleaseId::new(),
                    release_version: 1,
                },
                requested_resources: WorkloadResources {
                    cpu_millicores: 100,
                    memory_bytes: 1024,
                    storage_bytes: 1024,
                    gpu: None,
                },
                requested_duration_seconds: 300,
                state: ResourceRequestState::Reviewing,
                revision: Revision::new(1)?,
                created_at: now,
                updated_at: now,
                diagnostic_code: None,
            };
            resource
                .create(&request.request_key, &request, "query-seed")
                .await?;
            if cancel {
                resource
                    .reject_or_cancel(
                        &format!("cancel-{}", request.id),
                        request.id,
                        request.revision,
                        ResourceRequestState::Cancelled,
                        requester_id,
                        "query-seed",
                    )
                    .await?;
            }
        }
    }
    Ok(project_id)
}

fn agent_cases() -> [(&'static str, &'static [&'static str]); 5] {
    [
        (
            "state=requested&state=running&limit=50",
            &["requested", "running"],
        ),
        ("state=requested&limit=50", &["requested"]),
        ("limit=50", &["failed", "requested", "running"]),
        ("state=&limit=50", &["failed", "requested", "running"]),
        ("state=&state=running&limit=50", &["running"]),
    ]
}

fn resource_cases() -> [(&'static str, &'static [&'static str]); 5] {
    [
        ("state=reviewing&state=allocating&limit=50", &["reviewing"]),
        ("state=reviewing&limit=50", &["reviewing"]),
        ("limit=50", &["cancelled", "reviewing"]),
        ("state=&limit=50", &["cancelled", "reviewing"]),
        ("state=&state=reviewing&limit=50", &["reviewing"]),
    ]
}

async fn assert_states(
    response: reqwest::Response,
    expected: &[&str],
) -> Result<Value, Box<dyn Error>> {
    let status = response.status();
    let body = response.text().await?;
    let page: Value = serde_json::from_str(&body)?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let items = page["items"].as_array().ok_or("items missing")?;
    let matched_states = items
        .iter()
        .map(|item| item["state"].as_str().ok_or("state missing"))
        .collect::<Result<BTreeSet<_>, _>>()?;
    assert_eq!(matched_states, expected.iter().copied().collect());
    assert!(
        !items.is_empty(),
        "seeded history must be read through the real owner"
    );
    Ok(page)
}

async fn assert_diagnostic(
    response: reqwest::Response,
    status: StatusCode,
    diagnostic: &str,
) -> Result<(), Box<dyn Error>> {
    assert_eq!(response.status(), status);
    let body: Value = response.json().await?;
    assert_eq!(body["diagnosticCode"], diagnostic);
    Ok(())
}

#[tokio::test]
async fn agent_owner_exploded_list_queries() -> Result<(), Box<dyn Error>> {
    let fixture = ListFixture::new().await?;
    let path = format!("internal/v1/projects/{}/agent-runs", fixture.project);
    for (raw, expected) in agent_cases() {
        let response = fixture
            .service_get(
                &fixture.agent,
                &format!("{path}?{raw}"),
                "labweaver-agent",
                "agent.control.invoke",
            )
            .await?
            .send()
            .await?;
        assert_states(response, expected).await?;
    }
    for raw in [
        "state=unknown",
        "state=requested&state=unknown",
        "limit=50&limit=50",
        "cursor=a&cursor=b",
        "cursor=&cursor=b",
        "unknown=field",
    ] {
        let response = fixture
            .service_get(
                &fixture.agent,
                &format!("{path}?{raw}"),
                "labweaver-agent",
                "agent.control.invoke",
            )
            .await?
            .send()
            .await?;
        assert_diagnostic(
            response,
            StatusCode::BAD_REQUEST,
            "LW_CONTRACT_DOCUMENT_INVALID",
        )
        .await?;
    }
    Ok(())
}

#[tokio::test]
async fn control_owner_exploded_list_queries_preserve_forwarded_states()
-> Result<(), Box<dyn Error>> {
    let fixture = ListFixture::new().await?;
    let session = &fixture.sessions[0];
    let path = format!("api/v1/projects/{}/agent-runs", fixture.project);
    for (raw, expected) in agent_cases() {
        let response = fixture
            .service_get(
                &fixture.control,
                &format!("{path}?{raw}"),
                "labweaver-control",
                "access.control.forward",
            )
            .await?
            .header("x-labweaver-actor-id", session.actor_id.to_string())
            .header("x-labweaver-session-id", session.session_id.to_string())
            .send()
            .await?;
        assert_states(response, expected).await?;
    }
    assert_eq!(
        fixture.agent_queries.lock().await[0],
        "state=requested&state=running&limit=50"
    );
    let before = fixture.agent_queries.lock().await.len();
    for raw in [
        "state=unknown",
        "state=requested&state=unknown",
        "limit=50&limit=50",
        "cursor=a&cursor=b",
        "cursor=&cursor=b",
        "unknown=field",
    ] {
        let response = fixture
            .service_get(
                &fixture.control,
                &format!("{path}?{raw}"),
                "labweaver-control",
                "access.control.forward",
            )
            .await?
            .header("x-labweaver-actor-id", session.actor_id.to_string())
            .header("x-labweaver-session-id", session.session_id.to_string())
            .send()
            .await?;
        assert_problem_response(
            response,
            StatusCode::BAD_REQUEST,
            "LW_CONTRACT_DOCUMENT_INVALID",
        )
        .await?;
    }
    assert_eq!(
        fixture.agent_queries.lock().await.len(),
        before,
        "invalid queries cannot reach Agent"
    );
    Ok(())
}

#[tokio::test]
async fn resource_owner_exploded_list_queries() -> Result<(), Box<dyn Error>> {
    let fixture = ListFixture::new().await?;
    let path = format!("api/v1/projects/{}/resource-requests", fixture.project);
    let delegation = auth::encode_resource_delegation(
        &DELEGATION_KEY,
        &fixture.sessions[0],
        Some(fixture.project),
        OffsetDateTime::now_utc(),
    )?;
    for (raw, expected) in resource_cases() {
        let response = fixture
            .service_get(
                &fixture.resource,
                &format!("{path}?{raw}"),
                "labweaver-resource",
                "resource.api.invoke",
            )
            .await?
            .header("x-labweaver-resource-delegation", &delegation)
            .send()
            .await?;
        assert_states(response, expected).await?;
    }
    for raw in [
        "state=unknown",
        "state=reviewing&state=unknown",
        "limit=50&limit=50",
        "cursor=a&cursor=b",
        "cursor=&cursor=b",
        "unknown=field",
    ] {
        let response = fixture
            .service_get(
                &fixture.resource,
                &format!("{path}?{raw}"),
                "labweaver-resource",
                "resource.api.invoke",
            )
            .await?
            .header("x-labweaver-resource-delegation", &delegation)
            .send()
            .await?;
        assert_diagnostic(
            response,
            StatusCode::BAD_REQUEST,
            "LW_CONTRACT_DOCUMENT_INVALID",
        )
        .await?;
    }
    Ok(())
}

#[tokio::test]
async fn access_boundary_exploded_list_queries_for_every_role() -> Result<(), Box<dyn Error>> {
    let fixture = ListFixture::new().await?;
    for session in &fixture.sessions {
        for (collection, cases) in [
            ("agent-runs", agent_cases()),
            ("resource-requests", resource_cases()),
        ] {
            let path = format!("api/v1/projects/{}/{collection}", fixture.project);
            for (raw, expected) in cases {
                let page = assert_states(
                    fixture
                        .browser_get(&format!("{path}?{raw}"), session)
                        .send()
                        .await?,
                    expected,
                )
                .await?;
                if collection == "resource-requests"
                    && !session
                        .roles
                        .contains(&contracts::PlatformRole::PlatformAdmin)
                {
                    for request in page["items"].as_array().ok_or("items missing")? {
                        assert_eq!(request["requesterId"], session.actor_id.to_string());
                    }
                }
            }
            for raw in [
                "state=unknown",
                "limit=50&limit=50",
                "cursor=a&cursor=b",
                "unknown=field",
            ] {
                assert_problem_response(
                    fixture
                        .browser_get(&format!("{path}?{raw}"), session)
                        .send()
                        .await?,
                    StatusCode::BAD_REQUEST,
                    "LW_CONTRACT_DOCUMENT_INVALID",
                )
                .await?;
            }
            // Follow the real opaque cursor with the same exploded filter; no typed-query bypass.
            let filters = if collection == "agent-runs" {
                "state=requested&state=running"
            } else {
                "state=reviewing&state=cancelled"
            };
            let response = fixture
                .browser_get(&format!("{path}?{filters}&limit=1"), session)
                .send()
                .await?
                .error_for_status()?;
            let first: Value = response.json().await?;
            let cursor = first["nextCursor"]
                .as_str()
                .ok_or("first page cursor missing")?;
            let response = fixture
                .browser_get(
                    &format!("{path}?{filters}&limit=1&cursor={cursor}"),
                    session,
                )
                .send()
                .await?;
            assert_eq!(response.status(), StatusCode::OK);
            let second: Value = response.json().await?;
            assert_eq!(
                second["items"]
                    .as_array()
                    .ok_or("second items missing")?
                    .len(),
                1
            );
            assert_ne!(first["items"][0]["id"], second["items"][0]["id"]);
        }
    }
    assert!(
        fixture
            .agent_queries
            .lock()
            .await
            .iter()
            .any(|raw| raw == "state=requested&state=running&limit=50")
    );
    Ok(())
}
