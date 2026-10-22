// Recovery composition only: real consumer, Evaluation/Agent PG stores and Agent HTTPS API.
// Control/OIDC/S3 and the free provider are external fixtures. Trusted service identity is
// injected at the Agent API test boundary; this does not repeat full authorization acceptance.
use super::*;
use agent_service::claude_code::{
    AuthoringAttemptScope, ClaudeCodeCommand, ClaudeCodeProcess, ClaudeCodeProcessError,
    ClaudeCodeProcessOutput, ClaudeCodeRuntimeError, ExecutionScope, RunCancellation,
};
use agent_service::llm_review::{
    LlmReviewLease, LlmReviewStore, LlmReviewWorker, ReviewInputAuthorization,
};
use contracts::http::{AgentLlmReviewQuery, AgentLlmReviewState};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct AgentCapture {
    pool: PgPool,
    task_id: contracts::TaskRunId,
    lose_ack: AtomicBool,
    fail_get: AtomicBool,
    gets: AtomicUsize,
    posts: AtomicUsize,
    accepted: AtomicUsize,
    intent_observed: AtomicBool,
}

async fn interrupt_agent(
    State(capture): State<Arc<AgentCapture>>,
    request: Request<Body>,
    next: axum::middleware::Next,
) -> Response {
    let create =
        request.method() == Method::POST && request.uri().path() == "/internal/v1/llm-reviews";
    let get = request.method() == Method::GET
        && request
            .uri()
            .path()
            .starts_with("/internal/v1/llm-reviews/");
    if create || get {
        let saved: bool = sqlx::query_scalar(
            "SELECT execution_resources IS NOT NULL FROM evaluation.evaluation_step_attempts WHERE task_run_id=$1",
        )
        .bind(capture.task_id.as_uuid())
        .fetch_one(&capture.pool)
        .await
        .expect("consumer intent must exist before any Agent request");
        assert!(
            saved,
            "the real consumer must persist intent before Agent HTTP"
        );
        capture.intent_observed.store(true, Ordering::SeqCst);
    }
    if get {
        capture.gets.fetch_add(1, Ordering::SeqCst);
        if capture.fail_get.swap(false, Ordering::SeqCst) {
            // Stop the first consumer after durable intent, before any possible create.
            return StatusCode::BAD_GATEWAY.into_response();
        }
    }
    if create {
        capture.posts.fetch_add(1, Ordering::SeqCst);
    }
    let response = next.run(request).await;
    if create && response.status() == StatusCode::ACCEPTED {
        capture.accepted.fetch_add(1, Ordering::SeqCst);
        if capture.lose_ack.swap(false, Ordering::SeqCst) {
            // The actual handler has already committed its PG row. Fail only its response body.
            let stream = futures_util::stream::once(async {
                Err::<axum::body::Bytes, std::io::Error>(std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "test response lost after Agent commit",
                ))
            });
            let mut lost = Response::new(Body::from_stream(stream));
            *lost.status_mut() = StatusCode::ACCEPTED;
            return lost;
        }
    }
    response
}

struct AgentServer {
    base: Url,
    ca_file: std::path::PathBuf,
    capture: Arc<AgentCapture>,
    store: LlmReviewStore,
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl AgentServer {
    async fn start(
        fixture: &ReviewFixture,
        runner: &KubernetesEvaluationRunner,
        temp: &TempDir,
        has_receipt: bool,
    ) -> Result<Self, Box<dyn Error>> {
        apply_agent_migrations(&fixture.db.pool).await?;
        let ca = test_ca()?;
        let (cert, key) = leaf_certificate(&ca)?;
        let ca_file = temp.path().join("recovery-agent-ca.pem");
        std::fs::write(&ca_file, ca.pem())?;
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let base = Url::parse(&format!(
            "https://localhost:{}/",
            listener.local_addr()?.port()
        ))?;
        let pool = fixture.db.pool.clone();
        let store = LlmReviewStore::new(pool.clone());
        let state = Arc::new(agent_service::api::AgentApiState {
            resource: None,
            store: agent_service::run_store::PostgresAgentRunStore::new(pool.clone()),
            build_store: agent_service::build_store::PgBuildStore::new(pool.clone()),
            generated_artifacts: agent_service::generated_artifacts::GeneratedArtifactStore::new(
                pool.clone(),
            ),
            llm_reviews: store.clone(),
            platform_images: agent_service::platform_images::PgPlatformImageCatalog::new(
                pool.clone(),
            ),
            platform_registry: None,
            objects: runner.objects.clone(),
            platform_image_import_jobs:
                agent_service::platform_image_jobs::PlatformImageImportJobStore::new(pool.clone()),
        });
        let capture = Arc::new(AgentCapture {
            pool,
            task_id: fixture.context.lease.task_run_id,
            lose_ack: AtomicBool::new(has_receipt),
            fail_get: AtomicBool::new(!has_receipt),
            gets: AtomicUsize::new(0),
            posts: AtomicUsize::new(0),
            accepted: AtomicUsize::new(0),
            intent_observed: AtomicBool::new(false),
        });
        let identity = auth::ServiceIdentity {
            issuer: "https://test.invalid".to_owned(),
            subject: "evaluation-recovery-test".to_owned(),
            client_id: "evaluation-recovery-test".to_owned(),
            expires_at: time::OffsetDateTime::now_utc() + time::Duration::minutes(5),
            permissions: BTreeSet::from([
                "agent.llm_review.create".to_owned(),
                "agent.llm_review.read".to_owned(),
                "agent.llm_review.cancel".to_owned(),
            ]),
        };
        let app = agent_service::api::router(state)
            .layer(axum::Extension(identity))
            .layer(axum::middleware::from_fn_with_state(
                capture.clone(),
                interrupt_agent,
            ));
        let (tx, rx) = oneshot::channel();
        let task = tokio::spawn(serve_tls(listener, app, tls_config(&cert, &key)?, rx));
        Ok(Self {
            base,
            ca_file,
            capture,
            store,
            stop: Some(tx),
            task,
        })
    }

    async fn client(&self, authority: &AuthorityHandle) -> Result<AgentClient, Box<dyn Error>> {
        let tokens = Arc::new(
            ServiceTokenClient::discover(
                ServiceTokenClientConfig::new(
                    &authority.issuer,
                    "evaluation-recovery-test".to_owned(),
                    "synthetic-recovery-secret".to_owned(),
                    "agent".to_owned(),
                    BTreeSet::from(["agent.llm_review.read".to_owned()]),
                    1,
                    TransportSecurityMode::InsecureTestOnly,
                )?,
                Client::builder().no_proxy().build()?,
            )
            .await?,
        );
        Ok(AgentClient::from_configuration(
            AgentClientConfiguration {
                base_uri: self.base.clone(),
                ca_file: self.ca_file.clone(),
                timeout_milliseconds: 5_000,
                max_request_bytes: 1024 * 1024,
                max_response_bytes: 1024 * 1024,
                audience: "agent".to_owned(),
            },
            tokens,
            scopes(&[
                "agent.llm_review.create",
                "agent.llm_review.read",
                "agent.llm_review.cancel",
            ]),
        )?)
    }

    async fn stop(mut self) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
        let _ = self.task.await;
    }
}

async fn apply_agent_migrations(pool: &PgPool) -> Result<(), Box<dyn Error>> {
    sqlx::query("CREATE SCHEMA agent").execute(pool).await?;
    let mut connection = pool.acquire().await?;
    sqlx::query("SET search_path=agent,pg_catalog")
        .execute(&mut *connection)
        .await?;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
    let catalog = persistence_sqlx::MigrationCatalog::load(&root.join("catalog.yaml"))?;
    let domain = catalog
        .domains
        .iter()
        .find(|d| d.name == persistence_sqlx::Domain::Agent)
        .ok_or("Agent migration catalog missing")?;
    for migration in &domain.migrations {
        let sql = persistence_sqlx::MigrationCatalog::read_verified_sql(&root, migration)?;
        sqlx::raw_sql(&sql).execute(&mut *connection).await?;
    }
    Ok(())
}

struct ParentAuthority(PgFreezeStore);
#[async_trait::async_trait]
impl ReviewInputAuthorization for ParentAuthority {
    async fn check(&self, lease: &LlmReviewLease) -> Result<(), ClaudeCodeRuntimeError> {
        let ModelInputAuthorizationSource::FrozenSubmission(source) =
            &lease.request.input_authorization.source
        else {
            return Err(ClaudeCodeRuntimeError::InputAuthorizationDenied);
        };
        self.0
            .model_input_facts_for_task(
                source.frozen_submission_id,
                lease.request.project_id,
                lease.request.course_id,
                lease.request.input_authorization.actor_id,
                source.evaluation_release_id,
                source.evaluation_release_revision,
                lease.task_run_id,
                &source.parent_lease_sha256,
            )
            .await
            .map(|_| ())
            .map_err(|_| ClaudeCodeRuntimeError::InputAuthorizationDenied)
    }
}

#[derive(Default)]
struct FreeProvider {
    calls: AtomicUsize,
}
#[async_trait::async_trait]
impl ClaudeCodeProcess for FreeProvider {
    async fn version(&self) -> Result<String, ClaudeCodeProcessError> {
        Ok("2.1.207".to_owned())
    }
    async fn recover_authoring_terminal(
        &self,
        _: &AuthoringAttemptScope,
        _: RunCancellation,
    ) -> Result<ClaudeCodeProcessOutput, ClaudeCodeProcessError> {
        Err(ClaudeCodeProcessError::TimedOut)
    }
    async fn execute(
        &self,
        _: &ExecutionScope,
        _: ClaudeCodeCommand,
        _: RunCancellation,
    ) -> Result<ClaudeCodeProcessOutput, ClaudeCodeProcessError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let review = json!({"schema_version":"goal-review/v1","assessment":"met","confidence":0.75,"findings":[{"criterion":"synthetic answer exists","result":"met","evidence":[{"path":"answer.txt","start_line":1,"end_line":1}],"suggestion":"keep the answer focused"}],"requires_teacher_attention":false});
        let session = Uuid::new_v4();
        let events = [
            json!({"type":"system","subtype":"init","session_id":session}),
            json!({"type":"assistant","session_id":session,"message":{"role":"assistant","content":[{"type":"text","text":review.to_string()}]}}),
            json!({"type":"result","subtype":"success","is_error":false,"session_id":session,"num_turns":1,"total_cost_usd":0.000_123,"usage":{"input_tokens":3,"output_tokens":2},"permission_denials":[]}),
        ];
        let mut bytes = Vec::new();
        for event in events {
            serde_json::to_writer(&mut bytes, &event).map_err(|_| ClaudeCodeProcessError::Io)?;
            bytes.push(b'\n');
        }
        Ok(ClaudeCodeProcessOutput::from_raw(Some(0), bytes, &[]))
    }
}

#[tokio::test]
async fn frozen_advisory_consumer_recovers_checkpoint_after_lease_takeover()
-> Result<(), Box<dyn Error>> {
    let mut evidence = Vec::new();
    for has_receipt in [true, false] {
        let mut cluster = spawn_cluster().await?;
        let authority = spawn_authority().await?;
        let temp = TempDir::new()?;
        let fixture = ReviewFixture::start(&cluster).await?;
        let mut runner = build_runner(&cluster, &authority, &temp, &fixture.db.pool).await?;
        let server = AgentServer::start(&fixture, &runner, &temp, has_receipt).await?;
        runner.agent = server.client(&authority).await?;
        assert!(
            fixture
                .db
                .store
                .load_execution_checkpoint(&fixture.context.lease)
                .await?
                .is_none()
        );
        // Only the production consumer constructs, persists and sends the first request.
        assert!(runner.execute(fixture.context.clone()).await.is_err());
        assert!(server.capture.intent_observed.load(Ordering::SeqCst));
        assert_eq!(server.capture.gets.load(Ordering::SeqCst), 1);
        assert_eq!(
            server.capture.posts.load(Ordering::SeqCst),
            usize::from(has_receipt)
        );
        assert_eq!(
            server.capture.accepted.load(Ordering::SeqCst),
            usize::from(has_receipt)
        );
        let original = fixture
            .db
            .store
            .load_execution_checkpoint(&fixture.context.lease)
            .await?
            .ok_or("consumer must persist intent")?;
        assert!(original.terminal_completion.is_none());
        let original_resources = original
            .execution_resources
            .as_ref()
            .ok_or("consumer resources")?;
        let request: contracts::http::InternalAgentLlmReviewRequest =
            serde_json::from_value(original_resources.request.clone())?;
        assert_eq!(request.input_authorization, fixture.binding);
        let request_hash = Sha256Digest::of_canonical(&request)?.to_string();
        let query = AgentLlmReviewQuery {
            project_id: request.project_id,
            course_id: request.course_id,
        };
        let process = Arc::new(FreeProvider::default());
        let worker = LlmReviewWorker {
            authorization: Some(Arc::new(ParentAuthority(PgFreezeStore::new(
                fixture.db.pool.clone(),
            )))),
            store: server.store.clone(),
            classifier: Arc::new(
                agent_service::classifier::DeterministicEgressClassifier::new(
                    "classifier-v1".to_owned(),
                    Revision::new(1)?,
                )?,
            ),
            process: process.clone(),
            worker_id: "consumer-recovery-agent".to_owned(),
            lease_duration: Duration::from_mins(2),
            poll_interval: Duration::from_millis(20),
        };
        let worker_task = tokio::spawn(worker.run());
        let prior_receipt = if has_receipt {
            let receipt = tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let receipt = server.store.get(request.task_run_id, &query).await?;
                    if matches!(
                        receipt.state,
                        AgentLlmReviewState::Succeeded | AgentLlmReviewState::Failed
                    ) {
                        break Ok::<_, agent_service::llm_review::LlmReviewStoreError>(receipt);
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await??;
            assert_eq!(receipt.state, AgentLlmReviewState::Succeeded);
            assert_eq!(receipt.request_sha256, request_hash);
            let usage = receipt.usage.as_ref().ok_or("historical usage")?;
            assert_eq!(
                (
                    usage.requests,
                    usage.input_tokens,
                    usage.output_tokens,
                    usage.cost_microusd
                ),
                (1, 3, 2, 123)
            );
            let saved_request: Value = sqlx::query_scalar(
                "SELECT request_json FROM agent.llm_review_runs WHERE task_run_id=$1",
            )
            .bind(request.task_run_id.as_uuid())
            .fetch_one(&fixture.db.pool)
            .await?;
            assert_eq!(saved_request, original_resources.request);
            Some(receipt)
        } else {
            None
        };
        let calls_before = process.calls.load(Ordering::SeqCst);
        assert_eq!(calls_before, usize::from(has_receipt));
        drop(runner);
        expire_lease(&fixture.db.pool, fixture.context.lease.step_run_id).await?;
        let lease = fixture
            .db
            .store
            .claim_next_step("recovered-consumer", Duration::from_mins(5))
            .await?
            .ok_or("expired parent must be claimable")?;
        assert_eq!(lease.task_run_id, fixture.context.lease.task_run_id);
        assert_eq!(lease.attempt, fixture.context.lease.attempt);
        assert_eq!(lease.run_id, fixture.context.lease.run_id);
        assert_eq!(lease.step_run_id, fixture.context.lease.step_run_id);
        assert_ne!(lease.worker_id, fixture.context.lease.worker_id);
        assert_ne!(lease.lease_token(), fixture.context.lease.lease_token());
        let ModelInputAuthorizationSource::FrozenSubmission(source) =
            &request.input_authorization.source
        else {
            return Err("frozen source missing".into());
        };
        assert_ne!(
            contracts::http::model_input_parent_lease_sha256(lease.lease_token()),
            source.parent_lease_sha256
        );
        let mut recovered = context(&fixture.db, &lease)?;
        recovered.run = fixture.db.store.load_run(lease.run_id).await?;
        assert_eq!(
            fixture
                .db
                .store
                .load_execution_checkpoint(&lease)
                .await?
                .ok_or("takeover checkpoint")?,
            original
        );
        fixture.reset(&cluster).await?;
        cluster.state.lock().await.model_denied = true;
        let fresh_temp = TempDir::new()?;
        let mut recovered_runner =
            build_runner(&cluster, &authority, &fresh_temp, &fixture.db.pool).await?;
        recovered_runner.agent = server.client(&authority).await?;
        let result = recovered_runner.execute(recovered.clone()).await;
        if has_receipt {
            let completion = result?;
            assert_eq!(completion.state, EvaluationStepRunState::Succeeded);
            assert_eq!(
                completion.review,
                prior_receipt.as_ref().and_then(|r| r.review.clone())
            );
            let saved = fixture
                .db
                .store
                .load_execution_checkpoint(&lease)
                .await?
                .ok_or("terminal consumer checkpoint")?;
            assert_eq!(saved.terminal_completion, Some(completion.clone()));
            assert_eq!(saved.execution_resources, original.execution_resources);
            let gets = server.capture.gets.load(Ordering::SeqCst);
            assert_eq!(gets, 2, "one receipt GET after restart");
            assert_eq!(
                recovered_runner.execute(recovered.clone()).await?,
                completion
            );
            assert_eq!(
                server.capture.gets.load(Ordering::SeqCst),
                gets,
                "saved terminal checkpoint must avoid even another receipt GET"
            );
            assert_eq!(
                server.store.get(request.task_run_id, &query).await?,
                prior_receipt.clone().ok_or("prior receipt")?
            );
        } else {
            assert!(
                matches!(
                    result,
                    Err(crate::execution::ExecutionError::IdentityMismatch)
                ),
                "old parent binding must be refused on missing receipt"
            );
            assert!(matches!(
                recovered_runner.execute(recovered).await,
                Err(crate::execution::ExecutionError::IdentityMismatch)
            ));
            assert_eq!(
                fixture
                    .db
                    .store
                    .load_execution_checkpoint(&lease)
                    .await?
                    .ok_or("unchanged intent")?,
                original
            );
            assert_eq!(server.capture.gets.load(Ordering::SeqCst), 3);
        }
        let final_checkpoint = fixture
            .db
            .store
            .load_execution_checkpoint(&lease)
            .await?
            .ok_or("final checkpoint")?;
        let final_request: contracts::http::InternalAgentLlmReviewRequest = serde_json::from_value(
            final_checkpoint
                .execution_resources
                .ok_or("final resources")?
                .request,
        )?;
        assert_eq!(final_request, request);
        assert_eq!(
            Sha256Digest::of_canonical(&final_request)?.to_string(),
            request_hash
        );
        assert_eq!(final_request.deadline_at, request.deadline_at);
        assert_eq!(
            final_request.input_authorization,
            request.input_authorization
        );
        assert_eq!(
            counts(&cluster).await,
            (0, 0),
            "no recovery object reads or mock Agent sends"
        );
        assert!(
            cluster.state.lock().await.model_requests.is_empty(),
            "no refreshed Control binding"
        );
        assert_eq!(
            server.capture.posts.load(Ordering::SeqCst),
            usize::from(has_receipt)
        );
        assert_eq!(
            server.capture.accepted.load(Ordering::SeqCst),
            usize::from(has_receipt)
        );
        assert_eq!(process.calls.load(Ordering::SeqCst), calls_before);
        let (rows, agent_attempt): (i64, Option<i64>) = sqlx::query_as(
            "SELECT count(*),max(attempt) FROM agent.llm_review_runs WHERE task_run_id=$1",
        )
        .bind(request.task_run_id.as_uuid())
        .fetch_one(&fixture.db.pool)
        .await?;
        assert_eq!(rows, i64::from(has_receipt));
        assert_eq!(agent_attempt, has_receipt.then_some(1));
        assert!(
            server
                .store
                .claim(
                    "verify-no-requeue",
                    Duration::from_secs(5),
                    fixture.db.store.authority_now().await?
                )
                .await?
                .is_none()
        );
        evidence.push(json!({
            "hasReceipt": has_receipt,
            "taskRunId": request.task_run_id,
            "attempt": lease.attempt,
            "leaseRotated": true,
            "consumerPersistedIntent": server.capture.intent_observed.load(Ordering::SeqCst),
            "freshConsumerRecoveredCheckpoint": true,
            "bindingUnchanged": true,
            "requestHashUnchanged": true,
            "deadlineUnchanged": true,
            "requestSha256": request_hash,
            "deadlineAt": request.deadline_at,
            "receipt": prior_receipt,
            "agentCreatePosts": server.capture.posts.load(Ordering::SeqCst),
            "recoveryCreatePosts": server.capture.posts.load(Ordering::SeqCst) - usize::from(has_receipt),
            "providerCallsBefore": calls_before,
            "providerCallsAfter": process.calls.load(Ordering::SeqCst),
            "repairCalls": process.calls.load(Ordering::SeqCst).saturating_sub(1),
            "agentRows": rows,
            "agentAttempt": agent_attempt,
            "recoveryObjectReads": counts(&cluster).await.0,
        }));
        worker_task.abort();
        let _ = worker_task.await;
        server.stop().await;
        cluster.stop().await;
    }
    if let Some(path) = std::env::var_os("LW197_RECOVERY_EVIDENCE") {
        std::fs::write(
            path,
            serde_json::to_vec_pretty(
                &json!({"cases":evidence,"passed":true,"boundary":"synthetic freeze/Control/OIDC/S3 and trusted service identity; free external provider; real consumer/PG/Agent API/worker"}),
            )?,
        )?;
    }
    Ok(())
}
