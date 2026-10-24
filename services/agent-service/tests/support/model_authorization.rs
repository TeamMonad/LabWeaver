//! Real Control/Access/Agent PG and real JWT/TLS Control handler; no injected principal.
#![allow(
    clippy::too_many_lines,
    reason = "one native fixture owns its complete identity and transport setup"
)]
#[path = "../../../control-service/tests/support/platform_configuration_common.rs"]
#[allow(
    dead_code,
    reason = "reuse Control configuration defaults rather than duplicate its fixture"
)]
mod control_common;

use super::build_billing::{BuildBillingFixture, TlsListener};
use agent_service::{
    classifier::DeterministicEgressClassifier,
    claude_code::{PackageObjectReadError, ProblemPackageEgressGate, ProblemPackageReader},
    model_gateway::ModelGatewayScope,
    model_input_authorization::{GatewayInputAuthorization, ModelInputAuthorizationClient},
    run_store::{AgentRunReservation, PostgresAgentRunStore},
};
use async_trait::async_trait;
use auth::{ServiceAuthConfig, ServiceTokenVerifier, TransportSecurityMode};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use contracts::{
    ActorId, ArtifactId, ArtifactRef, PolicyId, ProjectId, Revision, UtcTimestamp,
    authoring::{
        AgentRunPurpose, AgentTrackKind, EnvironmentClass, ProblemPackage, ProjectLlmEgressPolicy,
    },
    http::{
        CreateAgentRunRequest, IdempotencyKey, InternalAgentRunRequest,
        InternalCreateAgentRunRequest, ModelInputAuthorizationBinding,
        ModelInputAuthorizationSource,
    },
};
use control_service::{ControlService, api::ApiState, clients::ServiceHttpClientConfig};
use persistence_sqlx::{Domain, MigrationCatalog, Sha256Digest};
use serde_json::json;
use sqlx::PgPool;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};
type Result<T, E = Box<dyn std::error::Error>> = std::result::Result<T, E>;
struct PackageBytes(ArtifactRef);
#[async_trait]
impl ProblemPackageReader for PackageBytes {
    async fn read(
        &self,
        reference: &ArtifactRef,
        limit: usize,
    ) -> std::result::Result<Vec<u8>, PackageObjectReadError> {
        if reference != &self.0 || limit == 0 {
            return Err(PackageObjectReadError);
        }
        Ok(b"x".to_vec())
    }
}

#[allow(
    dead_code,
    reason = "these fixture fields are exercised by different current integration targets"
)]
pub struct ModelAuthorizationFixture {
    pub authorization: GatewayInputAuthorization,
    pub project: ProjectId,
    pub run_id: contracts::AgentRunId,
    pub actor: ActorId,
    pub session: uuid::Uuid,
    pub attempt: u32,
    pub scope: ModelGatewayScope,
    pub now: UtcTimestamp,
    pub lease: uuid::Uuid,
    pub control_base_uri: reqwest::Url,
    pub control_ca_file: std::path::PathBuf,
    _directory: tempfile::TempDir,
    control: tokio::task::JoinHandle<()>,
}
impl Drop for ModelAuthorizationFixture {
    fn drop(&mut self) {
        self.control.abort();
    }
}
impl ModelAuthorizationFixture {
    pub async fn start(pool: PgPool, billing: &BuildBillingFixture) -> Result<Self> {
        sqlx::query("CREATE ROLE lw_control_runtime NOLOGIN")
            .execute(&pool)
            .await?;
        apply(&pool, Domain::Control).await?;
        apply(&pool, Domain::Access).await?;
        let now = UtcTimestamp::from_utc(
            sqlx::query_scalar("SELECT date_trunc('milliseconds',clock_timestamp())")
                .fetch_one(&pool)
                .await?,
        )?;
        let actor = ActorId::new();
        sqlx::query("INSERT INTO access.actors(actor_id,issuer,subject_sha256) VALUES($1,'https://fixture-user-issuer.invalid',$2)")
            .bind(actor.as_uuid()).bind(Sha256Digest::of_bytes(actor.as_uuid().as_bytes()).to_string()).execute(&pool).await?;
        let keys = auth::KeyRing::parse(
            "fixture".to_owned(),
            &format!("fixture:{}", URL_SAFE_NO_PAD.encode([7u8; 32])),
        )?;
        let session = auth::create_bff_session(
            &pool,
            &keys,
            auth::CreateBffSession {
                actor_id: actor.as_uuid(),
                roles: vec![contracts::PlatformRole::Student],
                authorization_revision: 1,
                expires_at: now.get() + time::Duration::hours(1),
                idle_ttl: time::Duration::minutes(30),
                oidc_sid: None,
                logout_hint: "fixture-logout".to_owned(),
            },
            now.get(),
        )
        .await?;
        let mut config = control_common::config()?;
        "fixture-model".clone_into(&mut config.llm_policy_options.models[0].model);
        "fixture-model".clone_into(&mut config.llm_policy_options.default_model);
        let service =
            ControlService::new(pool.clone(), Arc::new(control_common::NoObjects), config)?;
        let project = service
            .create_project(
                actor,
                contracts::PlatformRole::Student,
                &contracts::CreateProjectRequest {
                    name: "metered authorized model".to_owned(),
                    description: None,
                    course_id: None,
                },
                &IdempotencyKey::parse("gateway-authorized-project")?,
                now,
            )
            .await?;
        let package: ProblemPackage = serde_json::from_value(
            json!({"id":contracts::ProblemPackageId::new(),"projectId":project.id,"courseId":null,"revision":1,"files":[{"path":"README.md","sha256":Sha256Digest::of_bytes(b"x").to_string(),"object":{"artifactId":ArtifactId::new(),"storeBinding":"fixture","objectVersion":"v1","sizeBytes":1,"mediaType":"text/plain"}}],"retention":{"policyId":PolicyId::new(),"policyRevision":1,"class":"course_material","retainUntil":"2099-01-01T00:00:00.000Z","disposition":"delete"},"completedAt":now}),
        )?;
        sqlx::query("INSERT INTO control.problem_packages(package_id,project_id,course_id,revision,manifest_sha256,contract,completed_at) VALUES($1,$2,NULL,1,$3,$4,$5)")
            .bind(package.id.as_uuid()).bind(project.id.as_uuid()).bind(Sha256Digest::of_canonical(&package.files)?.to_string()).bind(serde_json::to_value(&package)?).bind(now.get()).execute(&pool).await?;
        let policy: ProjectLlmEgressPolicy = serde_json::from_value(
            json!({"id":PolicyId::new(),"projectId":project.id,"courseId":null,"revision":1,"binding":{"runtimeBinding":"claude-code-test","model":"fixture-model","claudeCodeVersion":"2.1.207","maxInFlightPerWorker":2},"budget":{"maxInputTokens":100_000,"maxOutputTokens":20_000,"maxRequests":10,"maxCostMicrousd":2_000_000,"timeoutMilliseconds":120_000,"maxTransientRetries":2,"maxSchemaRepairs":2},"deniedDataClasses":["secret","token","private_key","personally_identifiable_information","unallowlisted_student_submission"],"studentContentMode":"manifest_allowlist_only","activatedAt":now}),
        )?;
        service
            .activate_project_policy(
                project.id,
                policy.clone(),
                &IdempotencyKey::parse("gateway-authorized-policy")?,
                None,
            )
            .await?;
        let selected = vec!["README.md".to_owned()];
        let command = InternalCreateAgentRunRequest {
            input_authorization: ModelInputAuthorizationBinding {
                session_sha256: Sha256Digest::of_bytes(session.session_id.to_string().as_bytes())
                    .to_string(),
                actor_id: actor,
                project_id: project.id,
                course_id: None,
                operation_id: "createProjectAgentRun".to_owned(),
                valid_until: now,
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
            request: InternalAgentRunRequest::Authoring(CreateAgentRunRequest {
                selected_paths: Some(selected.clone()),
                project_id: project.id,
                course_id: None,
                package_id: package.id,
                package_revision: package.revision,
                policy_id: policy.id,
                policy_revision: policy.revision,
                environment_class: EnvironmentClass::Work,
            }),
            purpose: AgentRunPurpose::Authoring {
                environment_class: EnvironmentClass::Work,
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
                &IdempotencyKey::parse("gateway-authorized-run")?,
                now,
                "gateway-auth-fixture",
            )
            .await?
        else {
            return Err("expected new run".into());
        };
        let classifier = Arc::new(DeterministicEgressClassifier::new(
            "fixture-dlp".to_owned(),
            Revision::new(1)?,
        )?);
        let prepared = ProblemPackageEgressGate::new(
            Arc::new(PackageBytes(package.files[0].object.clone())),
            classifier.clone(),
        )
        .prepare(&package, &policy, &selected)
        .await?;
        let dispatch = store
            .claim_dispatch(Duration::from_mins(2))
            .await?
            .ok_or("dispatch missing")?;
        store
            .bind_prepared_dispatch(&dispatch, prepared.sha256())
            .await?;
        let track = store
            .claim_track(
                run.id,
                AgentTrackKind::Environment,
                prepared.sha256(),
                "gateway-auth-worker",
                Duration::from_mins(2),
            )
            .await?
            .ok_or("track missing")?;
        let lease: uuid::Uuid = sqlx::query_scalar("SELECT lease_token FROM agent.agent_track_work_items WHERE run_id=$1 AND track='environment'").bind(run.id.as_uuid()).fetch_one(&pool).await?;
        // A typed existing sandbox ledger fixture, not a replacement Resource compute backend.
        let task_run_id = contracts::TaskRunId::new();
        let sandbox = contracts::execution::TaskExecutionBinding {
            task_run_id,
            execution_generation: 1,
            resource_request_id: contracts::ResourceRequestId::new(),
            capacity_claim_id: contracts::CapacityClaimId::new(),
            lease_id: contracts::LeaseId::new(),
            claim_revision: Revision::new(1)?,
            lease_revision: Revision::new(1)?,
            project_id: project.id,
            provider_binding: "fixture-provider".to_owned(),
            namespace: "labweaver-authoring".to_owned(),
            workload_name: "model-gateway-fixture".to_owned(),
            trace_id: "gateway-auth-fixture".to_owned(),
        };
        sandbox.validate()?;
        sqlx::query("INSERT INTO agent.authoring_sandbox_attempts(run_id,track,attempt_number,task_run_id,execution_generation,namespace,workload_name,binding,state) VALUES($1,'environment',$2,$3,1,'labweaver-authoring','model-gateway-fixture',$4,'submitted')")
            .bind(run.id.as_uuid()).bind(i64::from(track.attempt)).bind(task_run_id.as_uuid()).bind(serde_json::to_value(sandbox)?).execute(&pool).await?;
        let scope = ModelGatewayScope::Authoring {
            track: AgentTrackKind::Environment,
            task_run_id,
            lease_sha256: contracts::http::model_input_parent_lease_sha256(lease),
        };
        let tls = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()])?;
        let tls_config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![tls.cert.der().clone()],
                rustls::pki_types::PrivateKeyDer::Pkcs8(tls.signing_key.serialize_der().into()),
            )?;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let base: reqwest::Url =
            format!("https://localhost:{}/", listener.local_addr()?.port()).parse()?;
        let directory = tempfile::tempdir()?;
        let ca = directory.path().join("control-ca.pem");
        std::fs::write(&ca, tls.cert.pem())?;
        let verifier = Arc::new(
            ServiceTokenVerifier::discover(
                ServiceAuthConfig::new(
                    &billing.issuer,
                    "labweaver-control".to_owned(),
                    BTreeSet::from(["synthetic-build-agent".to_owned()]),
                    BTreeSet::new(),
                    BTreeSet::from(["ES256".to_owned()]),
                    3600,
                    1,
                    TransportSecurityMode::InsecureTestOnly,
                )?,
                auth::no_redirect_http_client(None, TransportSecurityMode::InsecureTestOnly)?,
            )
            .await?,
        );
        let cfg = ServiceHttpClientConfig {
            base_url: base.clone(),
            ca_certificate_file: ca.to_string_lossy().into_owned(),
            timeout_milliseconds: 5000,
        };
        let token = &billing.token_client;
        let state = Arc::new(ApiState {
            control: service,
            access: control_service::clients::AccessClient::new_authenticated(
                cfg.clone(),
                token.clone(),
            )?,
            agent: control_service::clients::AgentClient::new_authenticated(
                cfg.clone(),
                token.clone(),
            )?,
            evaluation: control_service::clients::EvaluationClient::new_authenticated(
                cfg.clone(),
                token.clone(),
            )?,
            environment: control_service::clients::EnvironmentClient::new_authenticated(
                cfg,
                token.clone(),
            )?,
            service_token_verifier: verifier.clone(),
        });
        let app = control_service::api::authenticated_router(&state, &verifier);
        let control = tokio::spawn(async move {
            let _ = axum::serve(
                TlsListener {
                    listener,
                    acceptor: tokio_rustls::TlsAcceptor::from(Arc::new(tls_config)),
                },
                app,
            )
            .await;
        });
        let client = ModelInputAuthorizationClient::from_configuration(
            base.to_string(),
            &ca,
            "labweaver-control".to_owned(),
            5000,
            token.clone(),
        )?;
        Ok(Self {
            authorization: GatewayInputAuthorization {
                client: Some(client),
                classifier,
            },
            project: project.id,
            run_id: run.id,
            actor,
            session: session.session_id,
            attempt: track.attempt,
            scope,
            now,
            lease,
            control_base_uri: base,
            control_ca_file: ca,
            _directory: directory,
            control,
        })
    }
}

pub async fn apply(pool: &PgPool, domain: Domain) -> Result<()> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
    let catalog = MigrationCatalog::load(&root.join("catalog.yaml"))?;
    let mut connection = pool.acquire().await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "CREATE SCHEMA {}; SET search_path TO {},pg_catalog",
        domain.schema(),
        domain.schema()
    )))
    .execute(&mut *connection)
    .await?;
    for migration in &catalog
        .domains
        .iter()
        .find(|value| value.name == domain)
        .ok_or("domain missing")?
        .migrations
    {
        sqlx::raw_sql(sqlx::AssertSqlSafe(MigrationCatalog::read_verified_sql(
            &root, migration,
        )?))
        .execute(&mut *connection)
        .await?;
    }
    sqlx::query("RESET search_path")
        .execute(&mut *connection)
        .await?;
    Ok(())
}
