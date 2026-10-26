//! Public continuation requests use real Access, Control and Agent business handlers.
use super::*;
use contracts::authoring::{
    AgentAttempt, AgentAttemptState, AgentRun, AgentRunPurpose, AgentRunState, AgentTrack,
    AgentTrackKind, EnvironmentClass, LlmUsage,
};
use contracts::{AgentRunId, PolicyId, ProblemPackageId, ProjectId, Revision};
use persistence_sqlx::{Domain, MigrationCatalog};

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn revoked_actor_cannot_continue_agent_history_with_an_existing_signed_cursor()
-> Result<(), Box<dyn Error>> {
    let postgres = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            postgres.get_host_port_ipv4(5432).await?
        ))
        .await?;
    support::apply_access_migrations(&pool).await?;
    apply_agent(&pool).await?;
    let mut tasks = Tasks(Vec::new());
    let actor = Uuid::now_v7();
    let (state, unused_upstreams) = Box::pin(state(pool.clone(), actor, &mut tasks)).await?;
    let mut state = Arc::unwrap_or_clone(state);
    let browser_session = session(
        &pool,
        &state.key_ring,
        actor,
        contracts::PlatformRole::Student,
    )
    .await?;
    let issuer = state
        .config
        .issuer
        .as_str()
        .replace("/realms/test", "/realms/cursor");
    let tokens = Arc::new(
        ServiceTokenClient::discover(
            ServiceTokenClientConfig::new(
                &issuer,
                "access-test".to_owned(),
                "fixture-secret".to_owned(),
                "labweaver-control".to_owned(),
                BTreeSet::from(["access.control.forward".to_owned()]),
                30,
                TransportSecurityMode::Strict,
            )?,
            state.oidc_http.clone(),
        )
        .await?,
    );
    let access_verifier = verifier(&state, &issuer, "labweaver-access").await?;
    let agent_verifier = verifier(&state, &issuer, "labweaver-agent").await?;
    let control_verifier = verifier(&state, &issuer, "labweaver-control").await?;
    state.service_token_verifier = access_verifier;
    let tls = rcgen::generate_simple_self_signed(vec!["127.0.0.1".to_owned()])?;
    let ca = tls.cert.pem();
    let server_config =
        http_transport::server_config(ca.as_bytes(), tls.signing_key.serialize_pem().as_bytes())?;
    let temporary = tempfile::tempdir()?;
    let ca_path = temporary.path().join("ca.pem");
    std::fs::write(&ca_path, &ca)?;
    // The internal authorization server reads the same current persisted BFF session/membership.
    let access_url = serve(
        &mut tasks,
        internal_router(Arc::new(state.clone())),
        Arc::clone(&server_config),
    )
    .await?;
    let objects: Arc<dyn artifact_store::ImmutableObjectStore> = Arc::new(
        artifact_store::S3ImmutableObjectStore::new(
            artifact_store::S3StoreConfig {
                binding: "unused-progress-objects".to_owned(),
                endpoint: "https://objects.invalid".parse()?,
                bucket: "test-bucket".to_owned(),
                region: "test".to_owned(),
                object_prefix: "test".to_owned(),
                upload_ttl_seconds: 60,
                max_object_bytes: 1024,
                force_path_style: true,
                ca_bundle_file: None,
            },
            artifact_store::S3Credential {
                access_key_id: "test".to_owned(),
                secret_access_key: "test".to_owned(),
                session_token: None,
            },
        )
        .await?,
    );
    let agent = agent_service::api::AgentApiState {
        resource: None,
        store: agent_service::run_store::PostgresAgentRunStore::new(pool.clone())
            .with_cursor_signing_keys(vec![7; 32], None),
        build_store: agent_service::build_store::PgBuildStore::new(pool.clone()),
        generated_artifacts: agent_service::generated_artifacts::GeneratedArtifactStore::new(
            pool.clone(),
        ),
        llm_reviews: agent_service::llm_review::LlmReviewStore::new(pool.clone()),
        platform_images: agent_service::platform_images::PgPlatformImageCatalog::new(pool.clone()),
        platform_registry: None,
        objects: Arc::clone(&objects),
        platform_image_import_jobs:
            agent_service::platform_image_jobs::PlatformImageImportJobStore::new(pool.clone()),
    };
    let agent_url = serve(
        &mut tasks,
        agent_service::api::with_service_auth(
            agent_service::api::router(Arc::new(agent)),
            agent_verifier,
        ),
        Arc::clone(&server_config),
    )
    .await?;
    let configuration: serde_yaml::Value = serde_yaml::from_str(include_str!(
        "../../../deploy/config/control-plane.yaml.example"
    ))?;
    let config: control_service::ControlConfig =
        serde_yaml::from_value(configuration["control"].clone())?;
    let control = control_service::ControlService::new(pool.clone(), objects, config)?;
    let owner = Uuid::now_v7();
    let _owner_session = session(
        &pool,
        &state.key_ring,
        owner,
        contracts::PlatformRole::Student,
    )
    .await?;
    let owner_id = owner.to_string().parse::<contracts::ActorId>()?;
    let actor_id = actor.to_string().parse::<contracts::ActorId>()?;
    let now = contracts::UtcTimestamp::from_utc(OffsetDateTime::now_utc().replace_nanosecond(0)?)?;
    let project = control
        .create_project(
            owner_id,
            contracts::PlatformRole::Student,
            &contracts::CreateProjectRequest {
                name: "cursor revocation".to_owned(),
                description: None,
                course_id: None,
            },
            &contracts::http::IdempotencyKey::parse("cursor-project")?,
            now,
        )
        .await?
        .id;
    control
        .add_project_membership(
            project,
            owner_id,
            false,
            &contracts::http::ResolvedProjectMembershipRequest {
                actor_id,
                role: contracts::PlatformRole::Student,
                object_role: contracts::object_authorization::ProjectObjectRole::Viewer,
                expires_at: None,
            },
            &contracts::http::IdempotencyKey::parse("cursor-member")?,
            now,
        )
        .await?;
    let run = seed_run(&pool, project).await?;
    let endpoint =
        |url: &str| -> Result<control_service::clients::ServiceHttpClientConfig, Box<dyn Error>> {
            Ok(control_service::clients::ServiceHttpClientConfig {
                base_url: url.parse()?,
                ca_certificate_file: ca_path.display().to_string(),
                timeout_milliseconds: 5000,
            })
        };
    let control_state = Arc::new(control_service::api::ApiState {
        control,
        access: control_service::clients::AccessClient::new_authenticated(
            endpoint(&access_url)?,
            Arc::clone(&tokens),
        )?,
        agent: control_service::clients::AgentClient::new_authenticated(
            endpoint(&agent_url)?,
            Arc::clone(&tokens),
        )?,
        evaluation: control_service::clients::EvaluationClient::new_authenticated(
            endpoint(&access_url)?,
            Arc::clone(&tokens),
        )?,
        environment: control_service::clients::EnvironmentClient::new_authenticated(
            endpoint(&access_url)?,
            Arc::clone(&tokens),
        )?,
        service_token_verifier: Arc::clone(&control_verifier),
    });
    let control_url = serve(
        &mut tasks,
        control_service::api::authenticated_router(&control_state, &control_verifier),
        server_config,
    )
    .await?;
    let mut gateway = state.deployment.control_gateway.clone();
    gateway.base_uri = control_url;
    gateway.allowed_server_sans = vec!["127.0.0.1".to_owned()];
    state.control_proxy = proxy::ControlGatewayProxy::new(
        &gateway,
        ca.as_bytes(),
        TransportSecurityMode::Strict,
        tokens,
        ServiceTokenTarget {
            audience: "labweaver-control".to_owned(),
            scopes: BTreeSet::from(["access.control.forward".to_owned()]),
        },
    )?;
    let state = Arc::new(state);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let app = browser_router_on(Arc::clone(&state), listener.local_addr()?);
    tasks.0.push(tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    }));
    let client = reqwest::Client::builder().no_proxy().build()?;
    let cookie = format!(
        "{}={}",
        state.deployment.browser.session_cookie_name, browser_session.session_id
    );
    let url = format!("{base}/api/v1/projects/{project}/agent-runs/{run}/task-progress");
    let first = client
        .get(&url)
        .header(header::COOKIE, &cookie)
        .query(&[("limit", "1")])
        .send()
        .await?;
    let first_status = first.status();
    let first_body: Value = first.json().await?;
    assert_eq!(first_status, StatusCode::OK, "{first_body}");
    let cursor = first_body["nextCursor"]
        .as_str()
        .ok_or("missing continuation")?;
    let second = client
        .get(&url)
        .header(header::COOKIE, &cookie)
        .query(&[("limit", "1"), ("cursor", cursor)])
        .send()
        .await?;
    assert_eq!(second.status(), StatusCode::OK);
    sqlx::query("UPDATE control.project_memberships SET state='revoked',revision=2 WHERE project_id=$1 AND actor_id=$2")
        .bind(project.as_uuid()).bind(actor).execute(&pool).await?;
    let denied = client
        .get(&url)
        .header(header::COOKIE, &cookie)
        .query(&[("limit", "1"), ("cursor", cursor)])
        .send()
        .await?;
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    let body: Value = denied.json().await?;
    assert!(body.get("history").is_none());
    assert!(body.get("current").is_none());
    for suffix in [
        format!("me/evaluation-tasks/{run}"),
        format!(
            "me/evaluation-tasks/submissions/{}",
            contracts::FrozenSubmissionId::new()
        ),
    ] {
        let denied = client
            .get(format!("{base}/api/v1/projects/{project}/{suffix}"))
            .header(header::COOKIE, &cookie)
            .query(&[("limit", "1"), ("cursor", cursor)])
            .send()
            .await?;
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
        let body: Value = denied.json().await?;
        assert!(body.get("history").is_none());
    }
    assert!(
        unused_upstreams.lock().await.is_empty(),
        "no mocked business endpoint is used"
    );
    Ok(())
}

async fn verifier(
    state: &AppState,
    issuer: &str,
    audience: &str,
) -> Result<Arc<ServiceTokenVerifier>, Box<dyn Error>> {
    Ok(Arc::new(
        ServiceTokenVerifier::discover(
            ServiceAuthConfig::new(
                issuer,
                audience.to_owned(),
                BTreeSet::from(["access-test".to_owned()]),
                BTreeSet::new(),
                BTreeSet::from(["ES256".to_owned()]),
                600,
                1,
                TransportSecurityMode::Strict,
            )?,
            state.oidc_http.clone(),
        )
        .await?,
    ))
}

async fn serve(
    tasks: &mut Tasks,
    app: Router,
    configuration: Arc<rustls::ServerConfig>,
) -> Result<String, Box<dyn Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("https://{}/", listener.local_addr()?);
    tasks.0.push(tokio::spawn(async move {
        let _ = http_transport::serve_tls(listener, app, configuration).await;
    }));
    Ok(url)
}

async fn apply_agent(pool: &PgPool) -> Result<(), Box<dyn Error>> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
    let catalog = MigrationCatalog::load(&root.join("catalog.yaml"))?;
    sqlx::query("CREATE SCHEMA agent").execute(pool).await?;
    let mut connection = pool.acquire().await?;
    sqlx::query("SET search_path=agent,pg_catalog")
        .execute(&mut *connection)
        .await?;
    for migration in &catalog
        .domains
        .iter()
        .find(|domain| domain.name == Domain::Agent)
        .ok_or("missing Agent catalog")?
        .migrations
    {
        sqlx::raw_sql(sqlx::AssertSqlSafe(&*MigrationCatalog::read_verified_sql(
            &root, migration,
        )?))
        .execute(&mut *connection)
        .await?;
    }
    Ok(())
}

async fn seed_run(pool: &PgPool, project_id: ProjectId) -> Result<AgentRunId, Box<dyn Error>> {
    let run = AgentRun {
        id: AgentRunId::new(),
        project_id,
        course_id: None,
        package_id: ProblemPackageId::new(),
        policy_id: PolicyId::new(),
        policy_revision: Revision::new(1)?,
        purpose: AgentRunPurpose::Authoring {
            environment_class: EnvironmentClass::Experiment,
        },
        state: AgentRunState::Failed,
        revision: Revision::new(1)?,
        tracks: [AgentTrackKind::Environment, AgentTrackKind::Evaluation]
            .into_iter()
            .map(|kind| AgentTrack {
                kind,
                candidate_id: None,
                attempts: (1..=3)
                    .map(|number| AgentAttempt {
                        number,
                        state: AgentAttemptState::Failed,
                        checkpoint: None,
                        usage: LlmUsage {
                            input_tokens: 0,
                            output_tokens: 0,
                            requests: 0,
                            cost_microusd: 0,
                        },
                        usage_observed: false,
                        diagnostic_code: Some("LW_AGENT_FAILED".to_owned()),
                    })
                    .collect(),
            })
            .collect(),
        plan: None,
    };
    run.validate()?;
    sqlx::query("INSERT INTO agent.agent_runs(run_id,project_id,course_id,problem_package_id,revision,state,provider_binding,input_sha256,policy_revision,purpose,contract) VALUES($1,$2,NULL,$3,1,'failed','claude-code-v1',$4,1,$5,$6)")
        .bind(run.id.as_uuid()).bind(project_id.as_uuid()).bind(run.package_id.as_uuid()).bind(persistence_sqlx::Sha256Digest::of_bytes(b"input").to_string()).bind(serde_json::to_value(run.purpose)?).bind(serde_json::to_value(&run)?).execute(pool).await?;
    Ok(run.id)
}

pub(super) async fn token(
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
    let mut header = Header::new(Algorithm::ES256);
    header.kid = Some("fixture".to_owned());
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let claims = json!({"iss":state.issuer,"aud":["labweaver-control","labweaver-access","labweaver-agent"],"azp":"access-test","sub":"service-account-access",
        "iat":now,"nbf":now-1,"exp":now+300,"resource_access":{
            "labweaver-control":{"roles":["access.control.forward"]},
            "labweaver-access":{"roles":["access.authorization.decide"]},
            "labweaver-agent":{"roles":["agent.control.invoke"]}}});
    match jsonwebtoken::encode(&header, &claims, &state.signing_key) {
        Ok(token) => Json(json!({"access_token":token,"token_type":"Bearer","expires_in":300}))
            .into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
