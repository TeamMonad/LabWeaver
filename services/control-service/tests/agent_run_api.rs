//! Public project Agent-run routes read the Agent authority before exposing state or approving a
//! generated Work configuration plan.

use std::{collections::BTreeSet, io::Cursor, sync::Arc};

use artifact_store::{ImmutableObjectStore, ObjectStoreError, PresignedUpload, VerifiedObject};
use async_trait::async_trait;
use auth::{
    ServiceAuthConfig, ServiceTokenClient, ServiceTokenClientConfig, ServiceTokenVerifier,
    TransportSecurityMode, no_redirect_http_client,
};
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{Path, Query, State},
    http::{HeaderValue, Request, StatusCode},
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use contracts::authoring::{
    AgentAttempt, AgentAttemptState, AgentRun, AgentRunPurpose, AgentRunState, AgentTrack,
    AgentTrackKind, LlmUsage, ProblemPackage, RuntimeKind, WorkConfigurationPlan,
};
use contracts::http::{
    ApproveWorkConfigurationRequest, GeneratedArtifactKind, GeneratedArtifactQuery,
    GeneratedArtifactRecord, InternalApproveWorkConfigurationRequest,
};
use contracts::supply_chain::BuildNetworkPolicy;
use contracts::{
    ActorId, AgentRunId, ArtifactId, ArtifactRef, AuthenticatedActor, AuthorizationDecision,
    AuthorizationDecisionRequest, BffSessionId, CourseId, EnvironmentId, PlatformRole, PolicyId,
    ProblemPackageId, Project, ProjectId, ProjectState, RetentionClass, RetentionDisposition,
    RetentionSnapshot, Revision, UtcTimestamp, WorkConfigurationPlanId,
};
use control_service::api::{ApiState, GatewayPrincipal, router};
use control_service::clients::ServiceHttpClientConfig;
use control_service::{
    ContainerBuildPolicy, ControlConfig, ControlService, EvaluationRuntimePolicy,
    VirtualMachineBasePolicy,
};
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn::auto::Builder,
    service::TowerToHyperService,
};
use persistence_sqlx::{Domain, Sha256Digest};
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose,
};
use rustls::{ServerConfig, pki_types::PrivateKeyDer};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::postgres::PgPoolOptions;
use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;
use tokio::{net::TcpListener, sync::oneshot};
use tokio_rustls::TlsAcceptor;
use tower::ServiceExt;

const SERVICE_TOKEN: &str = "eyJhbGciOiJub25lIn0.eyJhdWQiOlsibGFid2VhdmVyLWFjY2VzcyIsImxhYndlYXZlci1hZ2VudCIsImxhYndlYXZlci1lbnZpcm9ubWVudCIsImxhYndlYXZlci1ldmFsdWF0aW9uIl19.signature";

#[derive(Clone)]
struct AccessState;

#[derive(Clone)]
struct AgentState {
    run: AgentRun,
    approved_run: AgentRun,
    artifact: GeneratedArtifactRecord,
}

#[derive(Deserialize)]
struct TokenRequest {
    grant_type: Option<String>,
}

struct AuthorityHandle {
    issuer: String,
    shutdown: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for AuthorityHandle {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        self.task.abort();
    }
}

struct TlsServiceHandle {
    base_url: reqwest::Url,
    shutdown: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for TlsServiceHandle {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        self.task.abort();
    }
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one route test covers the live state and all fences"
)]
async fn project_agent_run_routes_use_live_agent_state_and_exact_scope()
-> Result<(), Box<dyn std::error::Error>> {
    let postgres = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            postgres.get_host_port_ipv4(5432).await?
        ))
        .await?;
    apply_domain_migrations(&pool).await?;

    let now = "2026-07-16T08:00:00.000Z".parse::<UtcTimestamp>()?;
    let later = "2099-01-01T00:00:00.000Z".parse::<UtcTimestamp>()?;
    let actor_id = ActorId::new();
    let project_id = ProjectId::new();
    let other_project_id = ProjectId::new();
    let course_id = CourseId::new();
    let project_data = project(project_id, actor_id, Some(course_id), now)?;
    let other_project = project(other_project_id, actor_id, Some(course_id), now)?;
    insert_project(&pool, &project_data).await?;
    insert_project(&pool, &other_project).await?;

    let package = package(project_id, course_id, now)?;
    insert_package(&pool, &package).await?;

    let script_content = b"#!/bin/sh\nprintf 'configured\\n'\n".to_vec();
    let script_artifact = ArtifactRef {
        artifact_id: ArtifactId::new(),
        store_binding: "test-store".to_owned(),
        object_version: "version-1".to_owned(),
        size_bytes: script_content.len() as u64,
        media_type: "text/x-shellscript".to_owned(),
    };
    let run = work_run(
        project_id,
        course_id,
        actor_id,
        package.id,
        script_artifact.clone(),
        AgentRunState::AwaitingApproval,
        2,
    )?;
    let mut approved_run = run.clone();
    approved_run.state = AgentRunState::Running;
    approved_run.revision = Revision::new(3)?;
    approved_run.tracks[0].attempts[0].state = AgentAttemptState::Running;
    approved_run.validate()?;
    let mut projected = run.clone();
    projected.state = AgentRunState::Requested;
    projected.revision = Revision::new(1)?;
    projected.plan = None;
    projected.tracks[0].attempts.clear();
    projected.validate()?;
    insert_projection(&pool, &projected).await?;

    let artifact = GeneratedArtifactRecord {
        artifact: script_artifact.clone(),
        project_id,
        course_id: Some(course_id),
        package_id: package.id,
        package_revision: package.revision,
        kind: GeneratedArtifactKind::WorkScript,
        object_key: "generated/work-script.sh".to_owned(),
        content_sha256: Sha256Digest::of_bytes(&script_content).to_string(),
    };
    let objects = Arc::new(TestObjects {
        reference: script_artifact,
        bytes: script_content.clone(),
    });

    let (ca_pem, leaf_pem, leaf_key_pem, jwk) = tls_material()?;
    let temp_dir = tempfile::tempdir()?;
    let ca_path = temp_dir.path().join("control-api-test-ca.pem");
    std::fs::write(&ca_path, &ca_pem)?;
    let authority = spawn_authority(jwk).await?;
    let access_server = spawn_access_server(&leaf_pem, &leaf_key_pem).await?;
    let agent_server = spawn_agent_server(
        &leaf_pem,
        &leaf_key_pem,
        run.clone(),
        approved_run.clone(),
        artifact,
    )
    .await?;
    let service_token_client = Arc::new(service_token_client(&authority.issuer).await?);
    let access = control_service::clients::AccessClient::new_authenticated(
        service_config(&access_server.base_url, &ca_path),
        Arc::clone(&service_token_client),
    )?;
    let agent = control_service::clients::AgentClient::new_authenticated(
        service_config(&agent_server.base_url, &ca_path),
        Arc::clone(&service_token_client),
    )?;
    let environment = control_service::clients::EnvironmentClient::new_authenticated(
        service_config(&agent_server.base_url, &ca_path),
        Arc::clone(&service_token_client),
    )?;
    let evaluation = control_service::clients::EvaluationClient::new_authenticated(
        service_config(&agent_server.base_url, &ca_path),
        Arc::clone(&service_token_client),
    )?;
    let verifier = ServiceTokenVerifier::discover(
        ServiceAuthConfig::new(
            &authority.issuer,
            "labweaver-agent".to_owned(),
            BTreeSet::from(["control-service".to_owned()]),
            BTreeSet::new(),
            BTreeSet::from(["ES256".to_owned()]),
            3_600,
            1,
            TransportSecurityMode::InsecureTestOnly,
        )?,
        no_redirect_http_client(None, TransportSecurityMode::InsecureTestOnly)?,
    )
    .await?;
    let service = ControlService::new(pool, objects, config()?)?;
    let app = router(Arc::new(ApiState {
        control: service,
        access,
        agent,
        evaluation,
        environment,
        service_token_verifier: Arc::new(verifier),
    }))
    .layer(axum::Extension(GatewayPrincipal {
        client_id: "access-gateway".to_owned(),
    }));

    let get_response = app
        .clone()
        .oneshot(public_request(
            format!("/api/v1/projects/{project_id}/agent-runs/{}", run.id),
            actor_id,
            BffSessionId::new(),
            None,
        )?)
        .await?;
    assert_eq!(get_response.status(), StatusCode::OK);
    assert_eq!(
        get_response.headers().get("etag"),
        Some(&HeaderValue::from_static("\"rev-2\""))
    );
    let live: AgentRun =
        serde_json::from_slice(&to_bytes(get_response.into_body(), usize::MAX).await?)?;
    assert_eq!(live.state, AgentRunState::AwaitingApproval);
    assert_eq!(live.plan, run.plan);

    let plan_response = app
        .clone()
        .oneshot(public_request(
            format!(
                "/api/v1/projects/{}/agent-runs/{}/work-configuration/plan",
                project_id, run.id
            ),
            actor_id,
            BffSessionId::new(),
            None,
        )?)
        .await?;
    assert_eq!(plan_response.status(), StatusCode::OK);
    let plan_view: contracts::http::WorkConfigurationPlanView =
        serde_json::from_slice(&to_bytes(plan_response.into_body(), usize::MAX).await?)?;
    assert_eq!(plan_view.plan, run.plan.clone().ok_or("missing test plan")?);
    assert_eq!(plan_view.script_content, String::from_utf8(script_content)?);

    let approval_request = ApproveWorkConfigurationRequest {
        expected_run_revision: Revision::new(2)?,
        expected_plan_revision: Revision::new(1)?,
        environment_revision: Revision::new(1)?,
        expires_at: later,
        reason: "approved for this Work".to_owned(),
        restart_confirmed: false,
    };
    let approval_response = app
        .clone()
        .oneshot(public_request(
            format!(
                "/api/v1/projects/{}/agent-runs/{}/work-configuration/approve",
                project_id, run.id
            ),
            actor_id,
            BffSessionId::new(),
            Some(serde_json::to_vec(&approval_request)?),
        )?)
        .await?;
    assert_eq!(approval_response.status(), StatusCode::ACCEPTED);
    let approved: AgentRun =
        serde_json::from_slice(&to_bytes(approval_response.into_body(), usize::MAX).await?)?;
    assert_eq!(approved.state, AgentRunState::Running);
    assert_eq!(approved.revision, Revision::new(3)?);

    let mismatch_response = app
        .oneshot(public_request(
            format!(
                "/api/v1/projects/{}/agent-runs/{}/work-configuration/plan",
                other_project_id, run.id
            ),
            actor_id,
            BffSessionId::new(),
            None,
        )?)
        .await?;
    assert_eq!(mismatch_response.status(), StatusCode::BAD_GATEWAY);
    Ok(())
}

async fn apply_domain_migrations(pool: &sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    sqlx::query("CREATE SCHEMA control").execute(pool).await?;
    let migration_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
    let catalog = persistence_sqlx::MigrationCatalog::load(&migration_root.join("catalog.yaml"))?;
    let entry = catalog
        .domains
        .iter()
        .find(|entry| entry.name == Domain::Control)
        .ok_or("missing control migration catalog entry")?;
    let mut connection = pool.acquire().await?;
    sqlx::query("SET search_path = control, pg_catalog")
        .execute(&mut *connection)
        .await?;
    for migration in &entry.migrations {
        let sql =
            persistence_sqlx::MigrationCatalog::read_verified_sql(&migration_root, migration)?;
        sqlx::raw_sql(&sql).execute(&mut *connection).await?;
    }
    Ok(())
}

fn config() -> Result<ControlConfig, Box<dyn std::error::Error>> {
    Ok(ControlConfig {
        package_object_prefix: "problem-packages".to_owned(),
        upload_ttl_seconds: 900,
        completion_lease_seconds: 300,
        max_package_files: 100,
        max_package_bytes: 1_048_576,
        retention_policy_id: PolicyId::new(),
        retention_seconds: 86_400,
        sse_retention_seconds: 3_600,
        trust_revision: Revision::new(1)?,
        image_policy_id: PolicyId::new(),
        image_policy_revision: Revision::new(1)?,
        environment_schema_sha256: Sha256Digest::of_bytes(b"environment"),
        evaluation_schema_sha256: Sha256Digest::of_bytes(b"evaluation"),
        container_build: ContainerBuildPolicy {
            builder_binding: "buildkit-primary-v1".to_owned(),
            output_repository_prefix: "harbor.internal/labweaver-system".to_owned(),
            dockerfile_path: "Dockerfile".to_owned(),
            network: BuildNetworkPolicy::DenyAll,
            max_duration_milliseconds: 600_000,
            max_cpu_millicores: 2_000,
            max_memory_bytes: 2_147_483_648,
        },
        virtual_machine_base: VirtualMachineBasePolicy {
            provider_binding: "kubevirt-primary-v1".to_owned(),
            storage_class_binding: "vm-rwo-primary-v1".to_owned(),
            artifact_id: contracts::ImageArtifactId::new(),
            base_disk: contracts::supply_chain::VirtualMachineBaseDisk {
                binding: "ubuntu-24.04-v1".to_owned(),
                source_registry_digest: concat!(
                    "docker://quay.io/containerdisks/ubuntu@",
                    "sha256:d28194a16351320fa9a093e18233033508a745566eb8ba3b309c32924bf155a5"
                )
                .to_owned(),
                capacity_bytes: 10_737_418_240,
            },
            format: contracts::supply_chain::VirtualMachineDiskFormat::Qcow2,
        },
        evaluation_runtime: EvaluationRuntimePolicy {
            provider_binding: "evaluation-primary-v1".to_owned(),
            runner_image: format!("runner@sha256:{}", "a".repeat(64)),
        },
    })
}

fn project(
    id: ProjectId,
    owner_actor_id: ActorId,
    course_id: Option<CourseId>,
    now: UtcTimestamp,
) -> Result<Project, Box<dyn std::error::Error>> {
    Ok(Project {
        id,
        owner_actor_id,
        name: "API route test project".to_owned(),
        description: None,
        course_id,
        state: ProjectState::Active,
        revision: Revision::new(1)?,
        created_at: now,
        updated_at: now,
    })
}

async fn insert_project(pool: &sqlx::PgPool, project: &Project) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO control.projects
         (project_id,owner_actor_id,name,description,course_id,state,revision,created_at,updated_at,contract)
         VALUES ($1,$2,$3,$4,$5,'active',$6,$7,$8,$9)",
    )
    .bind(project.id.as_uuid())
    .bind(project.owner_actor_id.as_uuid())
    .bind(&project.name)
    .bind(&project.description)
    .bind(project.course_id.map(CourseId::as_uuid))
    .bind(i64::try_from(project.revision.get()).map_err(|_| sqlx::Error::Protocol("revision overflow".to_owned()))?)
    .bind(project.created_at.get())
    .bind(project.updated_at.get())
    .bind(serde_json::to_value(project).map_err(|error| sqlx::Error::Protocol(error.to_string()))?)
    .execute(pool)
    .await?;
    Ok(())
}

fn package(
    project_id: ProjectId,
    course_id: CourseId,
    completed_at: UtcTimestamp,
) -> Result<ProblemPackage, Box<dyn std::error::Error>> {
    Ok(ProblemPackage {
        id: ProblemPackageId::new(),
        project_id,
        course_id: Some(course_id),
        revision: Revision::new(1)?,
        files: vec![contracts::authoring::PackageFile {
            path: "README.md".to_owned(),
            object: ArtifactRef {
                artifact_id: ArtifactId::new(),
                store_binding: "test-store".to_owned(),
                object_version: "package-version".to_owned(),
                size_bytes: 1,
                media_type: "text/markdown".to_owned(),
            },
        }],
        retention: RetentionSnapshot {
            policy_id: PolicyId::new(),
            policy_revision: Revision::new(1)?,
            class: RetentionClass::CourseMaterial,
            retain_until: completed_at,
            disposition: RetentionDisposition::Delete,
        },
        completed_at,
    })
}

async fn insert_package(pool: &sqlx::PgPool, package: &ProblemPackage) -> Result<(), sqlx::Error> {
    let contract =
        serde_json::to_value(package).map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
    sqlx::query(
        "INSERT INTO control.problem_packages
         (package_id,project_id,course_id,revision,manifest_sha256,contract,completed_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7)",
    )
    .bind(package.id.as_uuid())
    .bind(package.project_id.as_uuid())
    .bind(package.course_id.map(CourseId::as_uuid))
    .bind(
        i64::try_from(package.revision.get())
            .map_err(|_| sqlx::Error::Protocol("revision overflow".to_owned()))?,
    )
    .bind(Sha256Digest::of_bytes(b"manifest").to_string())
    .bind(contract)
    .bind(package.completed_at.get())
    .execute(pool)
    .await?;
    Ok(())
}

async fn insert_projection(pool: &sqlx::PgPool, run: &AgentRun) -> Result<(), sqlx::Error> {
    let contract =
        serde_json::to_value(run).map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
    sqlx::query(
        "INSERT INTO control.agent_run_projections
         (run_id,project_id,course_id,revision,state,contract_sha256,contract,projected_event_id)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
    )
    .bind(run.id.as_uuid())
    .bind(run.project_id.as_uuid())
    .bind(run.course_id.map(CourseId::as_uuid))
    .bind(
        i64::try_from(run.revision.get())
            .map_err(|_| sqlx::Error::Protocol("revision overflow".to_owned()))?,
    )
    .bind("requested")
    .bind(
        Sha256Digest::of_canonical(&contract)
            .map_err(|error| sqlx::Error::Protocol(error.to_string()))?
            .to_string(),
    )
    .bind(contract)
    .bind(uuid::Uuid::now_v7())
    .execute(pool)
    .await?;
    Ok(())
}

fn work_run(
    project_id: ProjectId,
    course_id: CourseId,
    actor_id: ActorId,
    package_id: ProblemPackageId,
    script_artifact: ArtifactRef,
    state: AgentRunState,
    revision: u64,
) -> Result<AgentRun, Box<dyn std::error::Error>> {
    let plan = WorkConfigurationPlan {
        id: WorkConfigurationPlanId::new(),
        revision: Revision::new(1)?,
        script_artifact,
        verification_script_artifact: None,
        summary: "Apply the requested configuration".to_owned(),
        requires_restart: false,
        environment_id: EnvironmentId::new(),
        environment_revision: Revision::new(1)?,
    };
    let run = AgentRun {
        id: AgentRunId::new(),
        project_id,
        course_id: Some(course_id),
        package_id,
        policy_id: PolicyId::new(),
        policy_revision: Revision::new(1)?,
        purpose: AgentRunPurpose::WorkConfiguration {
            environment_id: plan.environment_id,
            environment_revision: plan.environment_revision,
            actor_id,
            runtime_kind: RuntimeKind::Container,
        },
        state,
        revision: Revision::new(revision)?,
        tracks: vec![AgentTrack {
            kind: AgentTrackKind::WorkConfiguration,
            attempts: vec![AgentAttempt {
                number: 1,
                state: AgentAttemptState::AwaitingApproval,
                checkpoint: None,
                usage: LlmUsage {
                    input_tokens: 3,
                    output_tokens: 2,
                    requests: 1,
                    cost_microusd: 4,
                },
                usage_observed: false,
                diagnostic_code: None,
            }],
            candidate_id: None,
        }],
        plan: Some(plan),
    };
    run.validate()?;
    Ok(run)
}

fn service_config(base_url: &reqwest::Url, ca_path: &std::path::Path) -> ServiceHttpClientConfig {
    ServiceHttpClientConfig {
        base_url: base_url.clone(),
        ca_certificate_file: ca_path.to_string_lossy().into_owned(),
        timeout_milliseconds: 3_000,
    }
}

fn public_request(
    uri: String,
    actor_id: ActorId,
    session_id: BffSessionId,
    body: Option<Vec<u8>>,
) -> Result<Request<Body>, axum::http::Error> {
    let mut builder = Request::builder()
        .method(if body.is_some() { "POST" } else { "GET" })
        .uri(uri)
        .header("x-labweaver-actor-id", actor_id.to_string())
        .header("x-labweaver-session-id", session_id.to_string());
    if body.is_some() {
        builder = builder
            .header("content-type", "application/json")
            .header("Idempotency-Key", "api-route-approval");
    }
    builder.body(Body::from(body.unwrap_or_default()))
}

async fn service_token_client(
    issuer: &str,
) -> Result<ServiceTokenClient, Box<dyn std::error::Error>> {
    let config = ServiceTokenClientConfig::new(
        issuer,
        "labweaver-control".to_owned(),
        "test-secret".to_owned(),
        "labweaver-control".to_owned(),
        BTreeSet::from(["access.control.forward".to_owned()]),
        30,
        TransportSecurityMode::InsecureTestOnly,
    )?;
    Ok(ServiceTokenClient::discover(
        config,
        no_redirect_http_client(None, TransportSecurityMode::InsecureTestOnly)?,
    )
    .await?)
}

async fn spawn_authority(jwk: Value) -> Result<AuthorityHandle, Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let issuer = format!("http://localhost:{}/realms/test", address.port());
    let state = (issuer.clone(), jwk);
    let router = Router::new()
        .route(
            "/realms/test/.well-known/openid-configuration",
            get(authority_discovery),
        )
        .route("/realms/test/jwks", get(authority_jwks))
        .route("/realms/test/token", post(authority_token))
        .with_state(state);
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        tokio::select! {
            result = axum::serve(listener, router) => {
                let _ = result;
            }
            _ = &mut shutdown_rx => {}
        }
    });
    Ok(AuthorityHandle {
        issuer,
        shutdown: Some(shutdown_tx),
        task,
    })
}

async fn authority_discovery(State((issuer, _)): State<(String, Value)>) -> Json<Value> {
    Json(json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{issuer}/authorize"),
        "token_endpoint": format!("{issuer}/token"),
        "jwks_uri": format!("{issuer}/jwks"),
        "response_types_supported": ["code"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["ES256"],
        "grant_types_supported": ["client_credentials"]
    }))
}

async fn authority_jwks(State((_, jwk)): State<(String, Value)>) -> Json<Value> {
    Json(json!({"keys": [jwk]}))
}

async fn authority_token(
    axum::extract::Form(request): axum::extract::Form<TokenRequest>,
) -> Result<Json<Value>, StatusCode> {
    if request.grant_type.as_deref() != Some("client_credentials") {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(Json(json!({
        "access_token": SERVICE_TOKEN,
        "token_type": "Bearer",
        "expires_in": 300
    })))
}

async fn spawn_access_server(
    certificate_pem: &str,
    private_key_pem: &str,
) -> Result<TlsServiceHandle, Box<dyn std::error::Error>> {
    spawn_tls_service(
        Router::new()
            .route("/internal/v1/auth/decision", post(access_decision))
            .with_state(AccessState),
        certificate_pem,
        private_key_pem,
    )
    .await
}

async fn access_decision(
    State(AccessState): State<AccessState>,
    Json(request): Json<AuthorizationDecisionRequest>,
) -> Result<Json<AuthorizationDecision>, StatusCode> {
    let valid_until = "2099-01-01T00:00:00.000Z"
        .parse()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(AuthorizationDecision {
        actor: AuthenticatedActor {
            actor_id: request.actor_id,
            roles: vec![PlatformRole::Teacher],
            expires_at: valid_until,
        },
        scope: request.scope,
        authorization_revision: Revision::new(1).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        scope_revision: Revision::new(1).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        valid_until,
        diagnostic_code: None,
    }))
}

async fn spawn_agent_server(
    certificate_pem: &str,
    private_key_pem: &str,
    run: AgentRun,
    approved_run: AgentRun,
    artifact: GeneratedArtifactRecord,
) -> Result<TlsServiceHandle, Box<dyn std::error::Error>> {
    let state = AgentState {
        run,
        approved_run,
        artifact,
    };
    spawn_tls_service(
        Router::new()
            .route("/internal/v1/agent-runs/{run_id}", get(agent_run))
            .route(
                "/internal/v1/agent-runs/{run_id}/work-configuration/approve",
                post(agent_approve),
            )
            .route(
                "/internal/v1/generated-artifacts/{artifact_id}",
                get(generated_artifact),
            )
            .with_state(state),
        certificate_pem,
        private_key_pem,
    )
    .await
}

async fn agent_run(
    State(state): State<AgentState>,
    Path(run_id): Path<AgentRunId>,
) -> Result<Json<AgentRun>, StatusCode> {
    if run_id != state.run.id {
        return Err(StatusCode::NOT_FOUND);
    }
    Ok(Json(state.run))
}

async fn agent_approve(
    State(state): State<AgentState>,
    Path(run_id): Path<AgentRunId>,
    Json(request): Json<InternalApproveWorkConfigurationRequest>,
) -> Result<Json<AgentRun>, StatusCode> {
    if run_id != state.run.id
        || request.project_id != state.run.project_id
        || request.course_id != state.run.course_id
        || request.expected_run_revision != state.run.revision
    {
        return Err(StatusCode::CONFLICT);
    }
    Ok(Json(state.approved_run))
}

async fn generated_artifact(
    State(state): State<AgentState>,
    Path(artifact_id): Path<ArtifactId>,
    Query(query): Query<GeneratedArtifactQuery>,
) -> Result<Json<GeneratedArtifactRecord>, StatusCode> {
    if artifact_id != state.artifact.artifact.artifact_id
        || query.project_id != state.artifact.project_id
        || query.course_id != state.artifact.course_id
        || query.package_id != state.artifact.package_id
        || query.package_revision != state.artifact.package_revision
    {
        return Err(StatusCode::NOT_FOUND);
    }
    Ok(Json(state.artifact))
}

async fn spawn_tls_service(
    router: Router,
    certificate_pem: &str,
    private_key_pem: &str,
) -> Result<TlsServiceHandle, Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let config = tls_config(certificate_pem, private_key_pem)?;
    let acceptor = TlsAcceptor::from(config);
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        loop {
            let accepted = tokio::select! {
                result = listener.accept() => result,
                _ = &mut shutdown_rx => return,
            };
            let Ok((stream, _)) = accepted else {
                return;
            };
            let acceptor = acceptor.clone();
            let router = router.clone();
            tokio::spawn(async move {
                let Ok(stream) = acceptor.accept(stream).await else {
                    return;
                };
                let service = TowerToHyperService::new(router);
                let connection = Builder::new(TokioExecutor::new())
                    .serve_connection_with_upgrades(TokioIo::new(stream), service)
                    .into_owned();
                let _ = connection.await;
            });
        }
    });
    Ok(TlsServiceHandle {
        base_url: format!("https://localhost:{}/", address.port()).parse()?,
        shutdown: Some(shutdown_tx),
        task,
    })
}

fn tls_config(
    certificate_pem: &str,
    private_key_pem: &str,
) -> Result<Arc<ServerConfig>, Box<dyn std::error::Error>> {
    let certificates = rustls_pemfile::certs(&mut Cursor::new(certificate_pem.as_bytes()))
        .collect::<Result<Vec<_>, _>>()?;
    let key: PrivateKeyDer<'static> =
        rustls_pemfile::private_key(&mut Cursor::new(private_key_pem.as_bytes()))?
            .ok_or("private key missing")?;
    let mut config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certificates, key)?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

fn tls_material() -> Result<(String, String, String, Value), Box<dyn std::error::Error>> {
    let ca_key = KeyPair::generate()?;
    let mut ca_parameters = CertificateParams::new(Vec::<String>::new())?;
    ca_parameters.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_parameters.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
    ];
    let ca = CertifiedIssuer::self_signed(ca_parameters, ca_key)?;
    let mut leaf_parameters = CertificateParams::new(vec!["localhost".to_owned()])?;
    leaf_parameters.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    leaf_parameters.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let leaf_key = KeyPair::generate()?;
    let leaf = leaf_parameters.signed_by(&leaf_key, &ca)?;
    let public = leaf_key.public_key_raw();
    let jwk = json!({
        "kty": "EC",
        "crv": "P-256",
        "x": URL_SAFE_NO_PAD.encode(&public[1..33]),
        "y": URL_SAFE_NO_PAD.encode(&public[33..65]),
        "use": "sig",
        "alg": "ES256",
        "kid": "test"
    });
    Ok((ca.pem(), leaf.pem(), leaf_key.serialize_pem(), jwk))
}

struct TestObjects {
    reference: ArtifactRef,
    bytes: Vec<u8>,
}

#[async_trait]
impl ImmutableObjectStore for TestObjects {
    fn binding(&self) -> &str {
        &self.reference.store_binding
    }

    async fn presign_upload(
        &self,
        _: &str,
        _: u64,
        _: &str,
        _: UtcTimestamp,
    ) -> Result<PresignedUpload, ObjectStoreError> {
        Err(ObjectStoreError::ObjectUnavailable)
    }

    async fn read_verified(
        &self,
        key: &str,
        expected: &ArtifactRef,
    ) -> Result<VerifiedObject, ObjectStoreError> {
        if key != "generated/work-script.sh" || expected != &self.reference {
            return Err(ObjectStoreError::ObjectIdentityMismatch);
        }
        Ok(VerifiedObject {
            reference: expected.clone(),
            bytes: self.bytes.clone(),
        })
    }

    async fn freeze_current(
        &self,
        _: &str,
        _: u64,
        _: &str,
    ) -> Result<VerifiedObject, ObjectStoreError> {
        Err(ObjectStoreError::ObjectUnavailable)
    }

    async fn delete_orphan(&self, _: &str, _: &str) -> Result<(), ObjectStoreError> {
        Err(ObjectStoreError::DeleteFailed)
    }
}
