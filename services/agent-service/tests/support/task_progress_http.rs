//! Real Agent HTTP handlers, `PostgreSQL` and signed service identities.
use super::*;
use agent_service::api::{AgentApiState, router};
use agent_service::run_store::PostgresAgentRunStore;
use contracts::authoring::{
    AgentAttempt, AgentAttemptState, AgentRun, AgentRunPurpose, AgentRunState, AgentTrack,
    AgentTrackKind, EnvironmentClass, LlmUsage,
};
use contracts::{ActorId, AgentRunId, CourseId, PolicyId, ProblemPackageId, ProjectId, Revision};
use persistence_sqlx::Sha256Digest;
use sqlx::postgres::PgPoolOptions;
use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn task_progress_http_rejects_cross_actor_run_tampering_and_offset()
-> Result<(), Box<dyn std::error::Error>> {
    let postgres = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            postgres.get_host_port_ipv4(5432).await?
        ))
        .await?;
    super::support::apply_agent_migrations(&pool).await?;
    let run_id = AgentRunId::new();
    let other_run = AgentRunId::new();
    let project = ProjectId::new();
    let course = CourseId::new();
    for id in [run_id, other_run] {
        let run = AgentRun {
            id,
            project_id: project,
            course_id: Some(course),
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
        sqlx::query("INSERT INTO agent.agent_runs(run_id,project_id,course_id,problem_package_id,revision,state,provider_binding,input_sha256,policy_revision,purpose,contract) VALUES($1,$2,$3,$4,1,'failed','claude-code-v1',$5,1,$6,$7)")
            .bind(id.as_uuid()).bind(project.as_uuid()).bind(course.as_uuid()).bind(run.package_id.as_uuid()).bind(Sha256Digest::of_bytes(b"input").to_string()).bind(serde_json::to_value(run.purpose)?).bind(serde_json::to_value(run)?).execute(&pool).await?;
    }
    let key = signing_material("progress")?;
    let authority = spawn_authority(&key).await?;
    let verifier = ServiceTokenVerifier::discover(
        ServiceAuthConfig::new(
            &authority.issuer,
            AUDIENCE.to_owned(),
            BTreeSet::from([CONTROL_CLIENT_ID.to_owned()]),
            BTreeSet::new(),
            BTreeSet::from(["ES256".to_owned()]),
            3600,
            1,
            TransportSecurityMode::InsecureTestOnly,
        )?,
        no_redirect_http_client(None, TransportSecurityMode::InsecureTestOnly)?,
    )
    .await?;
    let token = signed_token(
        &authority.key,
        &authority.issuer,
        CONTROL_CLIENT_ID,
        &[CONTROL_PERMISSION],
    )?;
    let objects = artifact_store::S3ImmutableObjectStore::new(
        artifact_store::S3StoreConfig {
            binding: "unused-in-progress-read".to_owned(),
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
    .await?;
    let state = Arc::new(AgentApiState {
        store: PostgresAgentRunStore::new(pool.clone()).with_cursor_signing_keys(vec![7; 32], None),
        resource: None,
        build_store: agent_service::build_store::PgBuildStore::new(pool.clone()),
        platform_images: agent_service::platform_images::PgPlatformImageCatalog::new(pool.clone()),
        generated_artifacts: agent_service::generated_artifacts::GeneratedArtifactStore::new(
            pool.clone(),
        ),
        llm_reviews: agent_service::llm_review::LlmReviewStore::new(pool.clone()),
        platform_image_import_jobs:
            agent_service::platform_image_jobs::PlatformImageImportJobStore::new(pool.clone()),
        platform_registry: None,
        objects: Arc::new(objects),
    });
    let app = with_service_auth(router(state), Arc::new(verifier));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}", listener.local_addr()?);
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let client = reqwest::Client::new();
    let actor = ActorId::new();
    let path = format!("{base}/internal/v1/agent-runs/{run_id}/task-progress");
    assert_eq!(
        client
            .get(&path)
            .header("x-labweaver-actor-id", actor.to_string())
            .send()
            .await?
            .status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    let response = client
        .get(&path)
        .bearer_auth(&token)
        .header("x-labweaver-actor-id", actor.to_string())
        .query(&[("limit", "1")])
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let page: Value = response.json().await?;
    assert!(page.get("page").is_none());
    assert!(page.get("pageSize").is_none());
    assert!(page.get("hasMore").is_none());
    let cursor = page["nextCursor"].as_str().ok_or("missing cursor")?;
    for foreign_course in [Some(CourseId::new()), None] {
        set_course(&pool, run_id, foreign_course).await?;
        let response = client
            .get(&path)
            .bearer_auth(&token)
            .header("x-labweaver-actor-id", actor.to_string())
            .query(&[("limit", "1"), ("cursor", cursor)])
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    }
    let without_course: Value = client
        .get(&path)
        .bearer_auth(&token)
        .header("x-labweaver-actor-id", actor.to_string())
        .query(&[("limit", "1")])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let no_course_cursor = without_course["nextCursor"]
        .as_str()
        .ok_or("missing unscoped continuation")?;
    set_course(&pool, run_id, Some(course)).await?;
    assert_eq!(
        client
            .get(&path)
            .bearer_auth(&token)
            .header("x-labweaver-actor-id", actor.to_string())
            .query(&[("cursor", no_course_cursor)])
            .send()
            .await?
            .status(),
        reqwest::StatusCode::BAD_REQUEST
    );
    let mut tampered = cursor.to_owned();
    let last = tampered.pop().ok_or("empty cursor")?;
    tampered.push(if last == 'A' { 'B' } else { 'A' });
    for (id, requester, query) in [
        (other_run, actor, vec![("cursor", cursor.to_owned())]),
        (run_id, ActorId::new(), vec![("cursor", cursor.to_owned())]),
        (run_id, actor, vec![("cursor", tampered)]),
        (run_id, actor, vec![("page", "2".to_owned())]),
        (run_id, actor, vec![("pageSize", "1".to_owned())]),
    ] {
        let response = client
            .get(format!("{base}/internal/v1/agent-runs/{id}/task-progress"))
            .bearer_auth(&token)
            .header("x-labweaver-actor-id", requester.to_string())
            .query(&query)
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    }
    server.abort();
    let _ = server.await;
    Ok(())
}

async fn set_course(
    pool: &sqlx::PgPool,
    run_id: AgentRunId,
    course: Option<CourseId>,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE agent.agent_runs SET course_id=$2,contract=jsonb_set(contract,'{courseId}',COALESCE(to_jsonb($2::uuid),'null'::jsonb)) WHERE run_id=$1")
        .bind(run_id.as_uuid()).bind(course.map(CourseId::as_uuid)).execute(pool).await?;
    Ok(())
}
