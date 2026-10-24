//! Real `PostgreSQL` + Control JWT/HTTPS resolver + Agent proxy; only the provider is captured.
use super::*;
use agent_service::{
    claude_code::{
        EgressClassifier, PackageObjectReadError, ProblemPackageEgressGate, ProblemPackageReader,
    },
    model_input_authorization::ModelInputAuthorizationClient,
    model_input_proxy::{ModelInputProxy, attempt_credential},
    run_store::{AgentRunReservation, PostgresAgentRunStore},
};
use contracts::{
    TaskRunId,
    http::{
        IdempotencyKey, InternalAgentRunRequest, InternalCreateAgentRunRequest,
        ModelInputAuthorizationBinding, ModelInputAuthorizationSource,
    },
};
use sha2::Digest;
use std::collections::BTreeMap;

struct SelectedObject(ArtifactRef);
#[async_trait]
impl ProblemPackageReader for SelectedObject {
    async fn read(
        &self,
        reference: &ArtifactRef,
        limit: usize,
    ) -> Result<Vec<u8>, PackageObjectReadError> {
        if reference != &self.0 || limit == 0 {
            return Err(PackageObjectReadError);
        }
        Ok(b"x".to_vec())
    }
}

async fn captured_provider(
    State(captured): State<Arc<tokio::sync::Mutex<Vec<Value>>>>,
    headers: axum::http::HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>, StatusCode> {
    if headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        != Some("Bearer provider-secret-kept-in-agent")
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    captured.lock().await.push(body);
    Ok(Json(
        json!({"id":"provider-fixture","type":"message","content":[],"usage":{"input_tokens":1,"output_tokens":1}}),
    ))
}

async fn send_model(
    app: &Router,
    task: TaskRunId,
    capability: &str,
    content: &str,
) -> Result<axum::response::Response, Box<dyn std::error::Error>> {
    Ok(app.clone().oneshot(Request::builder().method("POST")
        .uri(format!("/internal/v1/model-input-attempts/{task}/v1/messages"))
        .header("authorization",format!("Bearer {capability}"))
        .header("content-type","application/json")
        .body(Body::from(serde_json::to_vec(&json!({"model":"fixture-provider-v1","max_tokens":20,"messages":[{"role":"user","content":content}]}))?))?).await?)
}
async fn send_tool(
    app: &Router,
    task: TaskRunId,
    capability: &str,
) -> Result<axum::response::Response, Box<dyn std::error::Error>> {
    Ok(app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/internal/v1/model-input-attempts/{task}/authorize"
                ))
                .header("authorization", format!("Bearer {capability}"))
                .body(Body::empty())?,
        )
        .await?)
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one real original-session/lease fixture proves all new actions use the same authority"
)]
async fn sandbox_tools_and_each_model_request_recheck_original_authority()
-> Result<(), Box<dyn std::error::Error>> {
    let postgres = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            postgres.get_host_port_ipv4(5432).await?
        ))
        .await?;
    sqlx::query("CREATE ROLE lw_control_runtime NOLOGIN")
        .execute(&pool)
        .await?;
    support::apply_domain_migrations(&pool, Domain::Access).await?;
    support::apply_domain_migrations(&pool, Domain::Control).await?;
    support::apply_domain_migrations(&pool, Domain::Agent).await?;
    let now = UtcTimestamp::from_utc(time::OffsetDateTime::now_utc().replace_nanosecond(0)?)?;
    let actor = ActorId::new();
    sqlx::query("INSERT INTO access.actors(actor_id,issuer,subject_sha256) VALUES($1,'https://user-issuer.invalid',$2)").bind(actor.as_uuid()).bind("a".repeat(64)).execute(&pool).await?;
    let keys = auth::KeyRing::parse(
        "test".to_owned(),
        &format!("test:{}", URL_SAFE_NO_PAD.encode([7u8; 32])),
    )?;
    let session = auth::create_bff_session(
        &pool,
        &keys,
        auth::CreateBffSession {
            actor_id: actor.as_uuid(),
            roles: vec![PlatformRole::Student],
            authorization_revision: 1,
            expires_at: now.get() + time::Duration::hours(1),
            idle_ttl: time::Duration::minutes(30),
            oidc_sid: None,
            logout_hint: "fixture".to_owned(),
        },
        now.get(),
    )
    .await?;
    let objects = Arc::new(TestObjects {
        reference: ArtifactRef {
            artifact_id: ArtifactId::new(),
            store_binding: "test-store".to_owned(),
            object_version: "v1".to_owned(),
            size_bytes: 1,
            media_type: "text/plain".to_owned(),
        },
        bytes: b"x".to_vec(),
    });
    let control = ControlService::new(pool.clone(), objects, config()?)?;
    let project = control
        .create_project(
            actor,
            PlatformRole::Student,
            &contracts::CreateProjectRequest {
                name: "Scoped input proxy".to_owned(),
                description: None,
                course_id: None,
            },
            &IdempotencyKey::parse("model-proxy-project")?,
            now,
        )
        .await?;
    let mut package = package(project.id, CourseId::new(), now)?;
    package.course_id = None;
    package.files[0].sha256 = Sha256Digest::of_bytes(b"x").to_string();
    insert_package(&pool, &package).await?;
    let policy: contracts::authoring::ProjectLlmEgressPolicy = serde_json::from_value(
        json!({"id":PolicyId::new(),"projectId":project.id,"courseId":null,"revision":1,"binding":{"runtimeBinding":"claude-code-test","model":"fixture-provider-v1","claudeCodeVersion":"2.1.207","maxInFlightPerWorker":2},"budget":{"maxInputTokens":100_000,"maxOutputTokens":20_000,"maxRequests":10,"maxCostMicrousd":2_000_000,"timeoutMilliseconds":120_000,"maxTransientRetries":2,"maxSchemaRepairs":2},"deniedDataClasses":["secret","token","private_key","personally_identifiable_information","unallowlisted_student_submission"],"studentContentMode":"manifest_allowlist_only","activatedAt":now}),
    )?;
    control
        .activate_project_policy(
            project.id,
            policy.clone(),
            &IdempotencyKey::parse("model-proxy-policy")?,
            None,
        )
        .await?;
    let selected = vec!["README.md".to_owned()];
    let command = InternalCreateAgentRunRequest {
        input_authorization: ModelInputAuthorizationBinding {
            session_sha256: format!(
                "{:x}",
                sha2::Sha256::digest(session.session_id.to_string().as_bytes())
            ),
            actor_id: actor,
            project_id: project.id,
            course_id: None,
            operation_id: "createProjectAgentRun".to_owned(),
            valid_until: UtcTimestamp::from_utc(now.get() + time::Duration::seconds(5))?,
            source: ModelInputAuthorizationSource::ProblemPackage {
                package_id: package.id,
                package_revision: package.revision,
            },
            policy_id: policy.id,
            policy_revision: policy.revision,
            policy_sha256: contracts::http::model_input_policy_sha256(&policy)?,
            selection_sha256: contracts::http::model_input_selection_sha256(&selected)?,
        },
        project_id: project.id,
        course_id: None,
        actor_id: actor,
        request: InternalAgentRunRequest::Authoring(contracts::http::CreateAgentRunRequest {
            selected_paths: Some(selected.clone()),
            project_id: project.id,
            course_id: None,
            package_id: package.id,
            package_revision: package.revision,
            policy_id: policy.id,
            policy_revision: policy.revision,
            environment_class: contracts::authoring::EnvironmentClass::Work,
        }),
        purpose: AgentRunPurpose::Authoring {
            environment_class: contracts::authoring::EnvironmentClass::Work,
        },
        package: package.clone(),
        object_locators: BTreeMap::from([(
            package.files[0].object.artifact_id,
            "fixture/object".to_owned(),
        )]),
        policy: policy.clone(),
        preauthorization: None,
    };
    let store = PostgresAgentRunStore::new(pool.clone());
    let AgentRunReservation::Created(run) = store
        .reserve_internal_dispatch(
            &command,
            &IdempotencyKey::parse("model-proxy-dispatch")?,
            now,
            "model-proxy-trace",
        )
        .await?
    else {
        return Err("expected a new real dispatch".into());
    };
    let classifier: Arc<dyn EgressClassifier> = Arc::new(
        agent_service::classifier::DeterministicEgressClassifier::new(
            "fixture-dlp".to_owned(),
            Revision::new(1)?,
        )?,
    );
    let prepared = ProblemPackageEgressGate::new(
        Arc::new(SelectedObject(package.files[0].object.clone())),
        classifier.clone(),
    )
    .prepare(&package, &policy, &selected)
    .await?;
    let dispatch = store
        .claim_dispatch(std::time::Duration::from_mins(2))
        .await?
        .ok_or("missing dispatch")?;
    store
        .bind_prepared_dispatch(&dispatch, prepared.sha256())
        .await?;
    let track = store
        .claim_track(
            run.id,
            AgentTrackKind::Environment,
            prepared.sha256(),
            "proxy-worker",
            std::time::Duration::from_mins(2),
        )
        .await?
        .ok_or("missing track")?;
    let lease: uuid::Uuid = sqlx::query_scalar("SELECT lease_token FROM agent.agent_track_work_items WHERE run_id=$1 AND track='environment'").bind(run.id.as_uuid()).fetch_one(&pool).await?;
    let task = TaskRunId::new();
    // This test consumes an existing sandbox ledger fixture, not a substitute Resource backend.
    // Resource admission itself remains covered by the shared task-execution integration suite.
    let sandbox_binding = contracts::execution::TaskExecutionBinding {
        task_run_id: task,
        execution_generation: 1,
        resource_request_id: contracts::ResourceRequestId::new(),
        capacity_claim_id: contracts::CapacityClaimId::new(),
        lease_id: contracts::LeaseId::new(),
        claim_revision: Revision::new(1)?,
        lease_revision: Revision::new(1)?,
        project_id: project.id,
        provider_binding: "fixture-provider".to_owned(),
        namespace: "labweaver-authoring".to_owned(),
        workload_name: "model-proxy-fixture".to_owned(),
        trace_id: "model-proxy-trace".to_owned(),
    };
    sandbox_binding.validate()?;
    sqlx::query("INSERT INTO agent.authoring_sandbox_attempts(run_id,track,attempt_number,task_run_id,execution_generation,namespace,workload_name,binding,state) VALUES($1,'environment',$2,$3,1,'labweaver-authoring','model-proxy-fixture',$4,'submitted')").bind(run.id.as_uuid()).bind(i64::from(track.attempt)).bind(task.as_uuid()).bind(serde_json::to_value(sandbox_binding)?).execute(&pool).await?;
    let capability = attempt_credential(task.as_uuid(), lease);
    let (ca, certificate, key, jwk) = tls_material()?;
    let files = tempfile::tempdir()?;
    let ca_path = files.path().join("ca.pem");
    std::fs::write(&ca_path, &ca)?;
    let issuer = spawn_creation_authority(jwk, &key).await?;
    let tokens = Arc::new(service_token_client(&issuer.issuer).await?);
    let verifier = Arc::new(
        ServiceTokenVerifier::discover(
            ServiceAuthConfig::new(
                &issuer.issuer,
                "labweaver-control".to_owned(),
                BTreeSet::from(["control-service".to_owned()]),
                BTreeSet::new(),
                BTreeSet::from(["ES256".to_owned()]),
                3600,
                1,
                TransportSecurityMode::InsecureTestOnly,
            )?,
            no_redirect_http_client(None, TransportSecurityMode::InsecureTestOnly)?,
        )
        .await?,
    );
    let captured = Arc::new(tokio::sync::Mutex::new(Vec::<Value>::new()));
    let provider = spawn_tls_service(
        Router::new()
            .route("/v1/messages", post(captured_provider))
            .with_state(captured.clone()),
        &certificate,
        &key,
    )
    .await?;
    // These unused downstream clients are real authenticated clients, not business substitutes.
    let cfg = service_config(&provider.base_url, &ca_path);
    let state = Arc::new(ApiState {
        control,
        access: control_service::clients::AccessClient::new_authenticated(
            cfg.clone(),
            tokens.clone(),
        )?,
        agent: control_service::clients::AgentClient::new_authenticated(
            cfg.clone(),
            tokens.clone(),
        )?,
        environment: control_service::clients::EnvironmentClient::new_authenticated(
            cfg.clone(),
            tokens.clone(),
        )?,
        evaluation: control_service::clients::EvaluationClient::new_authenticated(
            cfg,
            tokens.clone(),
        )?,
        service_token_verifier: verifier.clone(),
    });
    let control_api = spawn_tls_service(
        control_service::api::authenticated_router(&state, &verifier),
        &certificate,
        &key,
    )
    .await?;
    let authority = ModelInputAuthorizationClient::from_configuration(
        control_api.base_url.to_string(),
        &ca_path,
        "labweaver-control".to_owned(),
        5000,
        tokens,
    )?;
    let transport = reqwest::Client::builder()
        .no_proxy()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .add_root_certificate(reqwest::Certificate::from_pem(ca.as_bytes())?)
        .build()?;
    let app = ModelInputProxy::new(
        store.clone(),
        Some(authority),
        classifier,
        &BTreeMap::from([
            (
                "ANTHROPIC_BASE_URL".to_owned(),
                provider.base_url.to_string(),
            ),
            (
                "ANTHROPIC_AUTH_TOKEN".to_owned(),
                "provider-secret-kept-in-agent".to_owned(),
            ),
        ]),
        transport,
    )?
    .router();
    assert_eq!(
        send_tool(&app, task, &capability).await?.status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        send_model(&app, task, &capability, "SELECTED_FILE_MARKER")
            .await?
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        send_model(&app, task, &capability, "TOOL_RESULT_MARKER")
            .await?
            .status(),
        StatusCode::OK
    );
    assert_eq!(captured.lock().await.len(), 2);
    assert_eq!(
        send_model(
            &app,
            task,
            &capability,
            "-----BEGIN PRIVATE KEY-----\nsensitive-marker"
        )
        .await?
        .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send_model(&app, task, &capability, &capability)
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );
    let escaped_capability = format!("\\u{:04x}{}", capability.as_bytes()[0], &capability[1..]);
    for body in [
        format!(
            r#"{{"model":"fixture-provider-v1","messages":[{{"role":"user","content":"{escaped_capability}"}}]}}"#
        ),
        format!(
            r#"{{"model":"fixture-provider-v1","messages":[],"metadata":{{"{escaped_capability}":"must-not-leave"}}}}"#
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/internal/v1/model-input-attempts/{task}/v1/messages"
                    ))
                    .header("authorization", format!("Bearer {capability}"))
                    .header("content-type", "application/json")
                    .body(Body::from(body))?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    for secret in [
        "-----BEGIN PRIVATE KEY-----",
        "AKIAIOSFODNN7EXAMPLE",
        "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJzZWNyZXQifQ.signaturemarker",
    ] {
        let input = json!({(secret): "harmless-value"});
        let plain = serde_json::to_string(
            &json!({"model":"fixture-provider-v1", "messages":[{"role":"assistant", "content":[{"type":"tool_use", "id":"read-fixture", "name":"Read", "input":input}]}]}),
        )?;
        let encoded = serde_json::to_string(secret)?;
        let escaped = format!("\"\\u{:04x}{}", secret.as_bytes()[0], &encoded[2..]);
        let unicode = plain.replacen(&encoded, &escaped, 1);
        let nested = serde_json::to_string(
            &json!({"model":"fixture-provider-v1", "messages":[{"role":"assistant", "content":[{"type":"tool_use", "id":"read-fixture", "name":"Read", "input":{"nested":[{"deeper":{(secret): "harmless-value"}}]}}]}]}),
        )?;
        for body in [plain, unicode, nested] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!(
                            "/internal/v1/model-input-attempts/{task}/v1/messages"
                        ))
                        .header("authorization", format!("Bearer {capability}"))
                        .header("content-type", "application/json")
                        .body(Body::from(body))?,
                )
                .await?;
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
            assert_eq!(
                captured.lock().await.len(),
                2,
                "secret keys must not reach the provider"
            );
        }
    }
    assert_eq!(
        send_tool(&app, TaskRunId::new(), &capability)
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send_tool(&app, task, &"0".repeat(64)).await?.status(),
        StatusCode::FORBIDDEN
    );
    for mutation in [
        "UPDATE access.bff_sessions SET revoked_at=clock_timestamp() WHERE session_id=$1",
        "UPDATE access.bff_sessions SET idle_expires_at=clock_timestamp()-interval '1 second' WHERE session_id=$1",
    ] {
        sqlx::query(sqlx::AssertSqlSafe(mutation))
            .bind(session.session_id)
            .execute(&pool)
            .await?;
        assert_eq!(
            send_tool(&app, task, &capability).await?.status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            send_model(&app, task, &capability, "MUST_NOT_SEND")
                .await?
                .status(),
            StatusCode::FORBIDDEN
        );
        sqlx::query("UPDATE access.bff_sessions SET revoked_at=NULL,idle_expires_at=clock_timestamp()+interval '20 minutes' WHERE session_id=$1").bind(session.session_id).execute(&pool).await?;
    }
    sqlx::query("UPDATE control.project_memberships SET object_role='viewer' WHERE project_id=$1 AND actor_id=$2").bind(project.id.as_uuid()).bind(actor.as_uuid()).execute(&pool).await?;
    assert_eq!(
        send_tool(&app, task, &capability).await?.status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send_model(&app, task, &capability, "MUST_NOT_SEND")
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );
    sqlx::query("UPDATE control.project_memberships SET object_role='owner' WHERE project_id=$1 AND actor_id=$2").bind(project.id.as_uuid()).bind(actor.as_uuid()).execute(&pool).await?;
    for mutation in [
        "UPDATE agent.agent_track_work_items SET lease_expires_at=clock_timestamp()-interval '1 second' WHERE run_id=$1",
        "UPDATE agent.agent_track_work_items SET attempt_started_at=clock_timestamp()-interval '1 hour' WHERE run_id=$1",
        "UPDATE agent.agent_track_work_items SET lease_token=gen_random_uuid() WHERE run_id=$1",
    ] {
        sqlx::query(sqlx::AssertSqlSafe(mutation))
            .bind(run.id.as_uuid())
            .execute(&pool)
            .await?;
        assert_eq!(
            send_tool(&app, task, &capability).await?.status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            send_model(&app, task, &capability, "MUST_NOT_SEND")
                .await?
                .status(),
            StatusCode::FORBIDDEN
        );
        sqlx::query("UPDATE agent.agent_track_work_items SET lease_expires_at=clock_timestamp()+interval '1 minute',attempt_started_at=$2,lease_token=$3 WHERE run_id=$1").bind(run.id.as_uuid()).bind(now.get()).bind(lease).execute(&pool).await?;
    }
    sqlx::query(
        "UPDATE agent.agent_runs SET cancellation_requested_at=clock_timestamp() WHERE run_id=$1",
    )
    .bind(run.id.as_uuid())
    .execute(&pool)
    .await?;
    assert_eq!(
        send_tool(&app, task, &capability).await?.status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send_model(&app, task, &capability, "MUST_NOT_SEND")
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );
    let captured = captured.lock().await;
    assert_eq!(
        captured.len(),
        2,
        "denials must not reach even a free provider fixture"
    );
    let actual = serde_json::to_string(&*captured)?;
    assert!(actual.contains("SELECTED_FILE_MARKER"));
    assert!(actual.contains("TOOL_RESULT_MARKER"));
    for secret in [
        "MUST_NOT_SEND",
        "sensitive-marker",
        capability.as_str(),
        "provider-secret-kept-in-agent",
    ] {
        assert!(!actual.contains(secret));
    }
    assert_eq!(
        store.load(run.id).await?.tracks[0].attempts[0]
            .usage
            .requests,
        0,
        "proxy cannot fabricate completed usage"
    );
    Ok(())
}
