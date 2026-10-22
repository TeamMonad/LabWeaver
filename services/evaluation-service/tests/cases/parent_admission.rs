// The parent path uses Control's actual HTTPS client and Evaluation's real API/PG authority.
// Service identity is a test-layer fixture; provider output is the external process boundary.
use super::*;
use agent_service::claude_code::RunCancellation;
use agent_service::claude_code::{
    ClaudeCodeCommand, ClaudeCodeProcess, ClaudeCodeProcessError, ClaudeCodeProcessOutput,
    ClaudeCodeRuntime, ClaudeCodeRuntimeError, ExecutionScope, ModelInputAuthorization,
};
use contracts::http::CreateFrozenModelInputAuthorizationRequest;
use std::sync::atomic::{AtomicUsize, Ordering};

struct ParentServer {
    client: control_service::clients::EvaluationClient,
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}
impl ParentServer {
    async fn start(
        fixture: &ReviewFixture,
        runner: &KubernetesEvaluationRunner,
        authority: &AuthorityHandle,
        temp: &TempDir,
    ) -> Result<Self, Box<dyn Error>> {
        let ca = test_ca()?;
        let (cert, key) = leaf_certificate(&ca)?;
        let path = temp.path().join("parent-ca.pem");
        std::fs::write(&path, ca.pem())?;
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let base = Url::parse(&format!(
            "https://localhost:{}/",
            listener.local_addr()?.port()
        ))?;
        let state = crate::api::EvaluationApiState::new(
            crate::PgFreezeCommandStore::new(fixture.db.pool.clone()),
            PgFreezeStore::new(fixture.db.pool.clone()),
            fixture.db.store.clone(),
            Arc::new(runner.authoring.clone()),
        );
        // Production verifies the service token before this trusted extension. This test
        // isolates endpoint authorization and parent persistence, without claiming IdP acceptance.
        let principal = auth::ServiceIdentity {
            issuer: "https://test.invalid".to_owned(),
            subject: "control-test".to_owned(),
            client_id: "control-test".to_owned(),
            expires_at: time::OffsetDateTime::now_utc() + time::Duration::minutes(5),
            permissions: BTreeSet::from(["evaluation.control.invoke".to_owned()]),
        };
        let app = crate::api::evaluation_api_router(state).layer(axum::Extension(principal));
        let (tx, rx) = oneshot::channel();
        let tls = tls_config(&cert, &key)?;
        let task = tokio::spawn(serve_tls(listener, app, tls, rx));
        let tokens = Arc::new(
            ServiceTokenClient::discover(
                ServiceTokenClientConfig::new(
                    &authority.issuer,
                    "control-test".to_owned(),
                    "synthetic-test-secret".to_owned(),
                    "labweaver-evaluation".to_owned(),
                    BTreeSet::from(["evaluation.control.invoke".to_owned()]),
                    1,
                    TransportSecurityMode::InsecureTestOnly,
                )?,
                Client::builder().no_proxy().build()?,
            )
            .await?,
        );
        let client = control_service::clients::EvaluationClient::new_authenticated(
            control_service::clients::ServiceHttpClientConfig {
                base_url: base,
                ca_certificate_file: path.display().to_string(),
                timeout_milliseconds: 5_000,
            },
            tokens,
        )?;
        Ok(Self {
            client,
            stop: Some(tx),
            task,
        })
    }
    async fn stop(mut self) {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
        let _ = self.task.await;
    }
}

fn input(fixture: &ReviewFixture) -> Result<CreateFrozenModelInputAuthorizationRequest, Box<dyn Error>> {
    Ok(CreateFrozenModelInputAuthorizationRequest {
        input_authorization_context: fixture
            .command
            .input_authorization_context
            .clone()
            .ok_or("context")?,
        task_run_id: fixture.context.lease.task_run_id,
        parent_lease_sha256: contracts::http::model_input_parent_lease_sha256(
            fixture.context.lease.lease_token(),
        ),
        frozen_submission_id: fixture.frozen.id,
        evaluation_release_id: fixture.context.release.id,
        evaluation_release_revision: fixture.context.release.revision,
        selected_paths: vec!["answer.txt".to_owned()],
    })
}
async fn cancel(
    store: &PgEvaluationControlStore,
    run_id: contracts::EvaluationRunId,
) -> Result<(), Box<dyn Error>> {
    let run = store.load_run(run_id).await?;
    store
        .request_cancellation(
            run.id,
            &contracts::http::InternalEvaluationRunMutationRequest {
                project_id: run.project_id,
                course_id: run.course_id,
                actor_id: run.actor_id,
                expected_revision: run.revision,
            },
            &IdempotencyKey::parse("parent-authority-cancel")?,
            store.authority_now().await?,
            "parent-authority-cancel",
        )
        .await?;
    Ok(())
}

#[tokio::test]
async fn frozen_advisory_task_api_checks_scope_old_lease_and_fixed_deadline()
-> Result<(), Box<dyn Error>> {
    let mut cluster = spawn_cluster().await?;
    let authority = spawn_authority().await?;
    let temp = TempDir::new()?;
    let fixture = ReviewFixture::start(&cluster).await?;
    let runner = build_runner(&cluster, &authority, &temp, &fixture.db.pool).await?;
    let server = ParentServer::start(&fixture, &runner, &authority, &temp).await?;
    let request = input(&fixture)?;
    let facts = server.client.frozen_model_input_facts(&request).await?;
    assert_eq!(
        facts["inputAuthorizationContext"],
        serde_json::to_value(&request.input_authorization_context)?
    );
    for field in ["task", "owner", "release", "freeze", "lease"] {
        let mut other = request.clone();
        match field {
            "task" => other.task_run_id = contracts::TaskRunId::new(),
            "owner" => other.input_authorization_context.actor_id = ActorId::new(),
            "release" => other.evaluation_release_revision = Revision::new(99)?,
            "freeze" => other.frozen_submission_id = FrozenSubmissionId::new(),
            _ => other.parent_lease_sha256 = "a".repeat(64),
        }
        assert!(
            server
                .client
                .frozen_model_input_facts(&other)
                .await
                .is_err(),
            "{field}"
        );
    }
    assert_eq!(counts(&cluster).await, (0, 0));
    let first = fixture.db.store.authority_now().await?;
    let fixed = PgFreezeStore::new(fixture.db.pool.clone())
        .model_input_task_deadline(
            fixture.frozen.id,
            fixture.db.project_id,
            Some(fixture.db.course_id),
            fixture.db.actor_id,
            fixture.context.release.id,
            fixture.context.release.revision,
            request.task_run_id,
            &request.parent_lease_sha256,
        )
        .await?;
    assert!(fixed.get() <= first.get() + time::Duration::minutes(5));
    let token = Uuid::new_v4();
    sqlx::query(
        "UPDATE evaluation.evaluation_step_attempts SET lease_token=$2 WHERE task_run_id=$1",
    )
    .bind(request.task_run_id.as_uuid())
    .bind(token)
    .execute(&fixture.db.pool)
    .await?;
    assert!(
        server
            .client
            .frozen_model_input_facts(&request)
            .await
            .is_err()
    );
    let mut rotated = request.clone();
    rotated.parent_lease_sha256 = contracts::http::model_input_parent_lease_sha256(token);
    server.client.frozen_model_input_facts(&rotated).await?;
    let after = PgFreezeStore::new(fixture.db.pool.clone())
        .model_input_task_deadline(
            fixture.frozen.id,
            fixture.db.project_id,
            Some(fixture.db.course_id),
            fixture.db.actor_id,
            fixture.context.release.id,
            fixture.context.release.revision,
            request.task_run_id,
            &rotated.parent_lease_sha256,
        )
        .await?;
    assert_eq!(
        fixed, after,
        "lease rotation cannot refresh preparation deadline"
    );
    sqlx::query("UPDATE evaluation.evaluation_step_attempts SET lease_expires_at=clock_timestamp()-interval '1 millisecond' WHERE task_run_id=$1")
        .bind(request.task_run_id.as_uuid()).execute(&fixture.db.pool).await?;
    assert!(
        server
            .client
            .frozen_model_input_facts(&rotated)
            .await
            .is_err()
    );
    sqlx::query("UPDATE evaluation.evaluation_step_attempts SET lease_expires_at=clock_timestamp()+interval '1 minute',created_at=clock_timestamp()-interval '6 minutes' WHERE task_run_id=$1")
        .bind(request.task_run_id.as_uuid()).execute(&fixture.db.pool).await?;
    assert!(
        server
            .client
            .frozen_model_input_facts(&rotated)
            .await
            .is_err(),
        "no intent cannot mint another window"
    );
    assert_eq!(counts(&cluster).await, (0, 0));
    server.stop().await;
    cluster.stop().await;
    Ok(())
}

struct ParentGuard {
    client: control_service::clients::EvaluationClient,
    request: CreateFrozenModelInputAuthorizationRequest,
}

#[tokio::test]
async fn frozen_advisory_parent_recovery_does_not_refresh_saved_binding()
-> Result<(), Box<dyn Error>> {
    let mut cluster = spawn_cluster().await?;
    let authority = spawn_authority().await?;
    let temp = TempDir::new()?;
    let fixture = ReviewFixture::start(&cluster).await?;
    let runner = build_runner(&cluster, &authority, &temp, &fixture.db.pool).await?;
    let req = build(&fixture, &runner).await?;
    let intent = super::super::super::execution_resources(
        &fixture.context,
        NAMESPACE,
        EvaluationExecutionKind::LlmReview,
        None,
        &req,
        Vec::new(),
    )?;
    fixture.db.store
        .persist_execution_intent(&fixture.context.lease, &intent)
        .await?;
    let server = ParentServer::start(&fixture, &runner, &authority, &temp).await?;
    let original = input(&fixture)?;
    server.client.frozen_model_input_facts(&original).await?;
    expire_lease(&fixture.db.pool, fixture.context.lease.step_run_id).await?;
    let next =
        fixture.db.store
            .claim_next_step("review-recovery", Duration::from_mins(5))
            .await?
            .ok_or("parent must recover")?;
    assert_eq!(next.task_run_id, fixture.context.lease.task_run_id);
    assert_eq!(next.attempt, fixture.context.lease.attempt);
    assert_ne!(next.lease_token(), fixture.context.lease.lease_token());
    assert!(
        server
            .client
            .frozen_model_input_facts(&original)
            .await
            .is_err()
    );
    let mut refreshed = original;
    refreshed.parent_lease_sha256 =
        contracts::http::model_input_parent_lease_sha256(next.lease_token());
    assert!(
        server
            .client
            .frozen_model_input_facts(&refreshed)
            .await
            .is_err(),
        "new lease cannot refresh a saved binding"
    );
    let cp =
        fixture.db.store
            .load_execution_checkpoint(&next)
            .await?
            .ok_or("original intent missing")?;
    assert_eq!(
        cp.execution_resources
            .as_ref()
            .ok_or("intent missing")?
            .request,
        serde_json::to_value(&req)?
    );
    assert_eq!(req.input_authorization, fixture.binding);
    server.stop().await;
    cluster.stop().await;
    Ok(())
}
#[async_trait::async_trait]
impl ModelInputAuthorization for ParentGuard {
    async fn check(&self) -> Result<(), ClaudeCodeRuntimeError> {
        self.client
            .frozen_model_input_facts(&self.request)
            .await
            .map(|_| ())
            .map_err(|_| ClaudeCodeRuntimeError::InputAuthorizationDenied)
    }
}
struct CancelAfterFirstProvider {
    calls: AtomicUsize,
    store: PgEvaluationControlStore,
    run_id: contracts::EvaluationRunId,
}
#[async_trait::async_trait]
impl ClaudeCodeProcess for CancelAfterFirstProvider {
    async fn version(&self) -> Result<String, ClaudeCodeProcessError> {
        Ok("2.1.207".to_owned())
    }
    async fn recover_authoring_terminal(
        &self,
        _: &agent_service::claude_code::AuthoringAttemptScope,
        _: RunCancellation,
    ) -> Result<ClaudeCodeProcessOutput, ClaudeCodeProcessError> {
        Err(ClaudeCodeProcessError::TimedOut)
    }
    async fn execute(
        &self,
        _: &ExecutionScope,
        _: ClaudeCodeCommand,
        cancellation: RunCancellation,
    ) -> Result<ClaudeCodeProcessOutput, ClaudeCodeProcessError> {
        assert!(
            !cancellation.is_cancelled(),
            "parent cancellation RPC has not reached Agent"
        );
        self.calls.fetch_add(1, Ordering::SeqCst);
        cancel(&self.store, self.run_id)
            .await
            .map_err(|_| ClaudeCodeProcessError::Io)?;
        let session = Uuid::new_v4().to_string();
        let events = [
            json!({"type":"system","subtype":"init","session_id":session}),
            json!({"type":"assistant","session_id":session,"message":{"role":"assistant","content":[{"type":"text","text":"{}"}]}}),
            json!({"type":"result","subtype":"success","is_error":false,"session_id":session,"num_turns":1,"total_cost_usd":0.000_001,
                "usage":{"input_tokens":0,"output_tokens":0},"modelUsage":{"test-model":{"inputTokens":1,"outputTokens":1}},"permission_denials":[]}),
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
async fn frozen_advisory_parent_cancel_with_delayed_agent_rpc_blocks_initial_and_repair()
-> Result<(), Box<dyn Error>> {
    for before_initial in [true, false] {
        let mut cluster = spawn_cluster().await?;
        let authority = spawn_authority().await?;
        let temp = TempDir::new()?;
        let fixture = ReviewFixture::start(&cluster).await?;
        let runner = build_runner(&cluster, &authority, &temp, &fixture.db.pool).await?;
        let req = build(&fixture, &runner).await?;
        let intent = super::super::super::execution_resources(
            &fixture.context,
            NAMESPACE,
            EvaluationExecutionKind::LlmReview,
            None,
            &req,
            Vec::new(),
        )?;
        fixture.db.store
            .persist_execution_intent(&fixture.context.lease, &intent)
            .await?;
        let server = ParentServer::start(&fixture, &runner, &authority, &temp).await?;
        if before_initial {
            cancel(&fixture.db.store, fixture.context.run.id).await?;
        }
        fixture.reset(&cluster).await?;
        let process = Arc::new(CancelAfterFirstProvider {
            calls: AtomicUsize::new(0),
            store: fixture.db.store.clone(),
            run_id: fixture.context.run.id,
        });
        let runtime = ClaudeCodeRuntime::new(req.policy.clone(), process.clone())?
            .with_input_authorization(Arc::new(ParentGuard {
                client: server.client.clone(),
                request: input(&fixture)?,
            }));
        let cancellation = RunCancellation::new();
        let failure = runtime
            .review_with_usage(
                b"synthetic frozen contents".to_vec(),
                &["answer.txt".to_owned()],
                cancellation.clone(),
            )
            .await
            .err()
            .ok_or("parent cancellation must stop invocation")?;
        assert_eq!(
            failure.error.diagnostic_code(),
            "LW_MODEL_INPUT_AUTHORIZATION_DENIED"
        );
        assert!(
            !cancellation.is_cancelled(),
            "the delayed RPC was never delivered"
        );
        assert_eq!(
            process.calls.load(Ordering::SeqCst),
            usize::from(!before_initial)
        );
        if before_initial {
            assert!(failure.usage.is_none());
        } else {
            assert_eq!(failure.usage.ok_or("first usage must remain")?.requests, 1);
        }
        assert_eq!(counts(&cluster).await, (0, 0));
        server.stop().await;
        cluster.stop().await;
    }
    Ok(())
}
