// Real PG ledger and real S3/HTTPS clients; Control/OIDC/S3 responses are external fixtures.
use super::*;
use contracts::http::{
    FrozenModelInputSource, IdempotencyKey, ModelInputAuthorizationBinding,
    ModelInputAuthorizationContext, ModelInputAuthorizationSource, ModelInputRubricIdentity,
    ModelInputRubricPermissionRef,
};

struct ReviewFixture {
    db: DbFixture,
    frozen: FrozenSubmission,
    context: EvaluationAttemptContext,
    command: crate::SubmissionFreezeCommand,
    binding: ModelInputAuthorizationBinding,
}

impl ReviewFixture {
    async fn start(cluster: &MockCluster) -> Result<Self, Box<dyn Error>> {
        let mut db = DbFixture::start().await?;
        let now = db.store.authority_now().await?;
        db.store
            .request_cancellation(
                db.run.id,
                &contracts::http::InternalEvaluationRunMutationRequest {
                    project_id: db.project_id,
                    course_id: Some(db.course_id),
                    actor_id: db.actor_id,
                    expected_revision: db.run.revision,
                },
                &IdempotencyKey::parse("cancel-unused-probe")?,
                now,
                "review-fixture",
            )
            .await?;
        let mut frozen = PgFreezeStore::new(db.pool.clone())
            .load_completed_for_project_actor(
                db.run.frozen_submission_id,
                db.project_id,
                db.actor_id,
            )
            .await?;
        let archive = serde_json::to_vec(
            &json!({"apiVersion":"evaluation.labweaver.io/frozen-submission-archive/v1","files":[{"path":"answer.txt","contentBase64":"eA=="}]}),
        )?;
        frozen.object.size_bytes = u64::try_from(archive.len())?;
        frozen.object.media_type = FROZEN_ARCHIVE_MEDIA_TYPE.to_owned();
        frozen.content_sha256 = Sha256Digest::of_bytes(&archive).to_string();
        let manifest: contracts::submission::SubmissionManifest = serde_json::from_value(json!({
            "apiVersion":"evaluation.labweaver.io/v1","kind":"SubmissionManifest","name":"review","source":"workspace",
            "include":[{"kind":"exactFile","path":"answer.txt"}],"exclude":[],"required":[],
            "llmReadable":[{"kind":"exactFile","path":"answer.txt"}],"maxTotalBytes":1024,"maxFiles":4,"followSymlinks":false,
        }))?;
        manifest.validate()?;
        let manifest_sha = Sha256Digest::of_canonical(&manifest)?.to_string();
        let original = ModelInputAuthorizationContext {
            session_sha256: Sha256Digest::of_bytes(b"original-review-session").to_string(),
            actor_id: db.actor_id,
            project_id: db.project_id,
            course_id: Some(db.course_id),
            operation_id: "freezeSubmission".to_owned(),
            valid_until: UtcTimestamp::from_utc(now.get() - time::Duration::minutes(1))?,
        };
        let command = crate::SubmissionFreezeCommand {
            input_authorization_context: Some(original.clone()),
            worker_dispatch: crate::FreezeWorkerDispatch::Pending,
            frozen_submission_id: frozen.id,
            operation_id: contracts::OperationId::new(),
            project_id: db.project_id,
            course_id: Some(db.course_id),
            environment_id: frozen.environment.environment_id,
            actor_id: db.actor_id,
            environment_revision: frozen.environment.environment_revision,
            manifest_revision: frozen.manifest_revision,
            manifest,
            idempotency_key: "review-first-freeze".to_owned(),
            trace_id: "review-fixture".to_owned(),
            requested_at: now,
        };
        crate::PgFreezeCommandStore::new(db.pool.clone())
            .accept(&command)
            .await?;
        // These immutable fixture rows isolate the consumer. Actual collection/commit is covered
        // by the unchanged freeze_postgres suite; no synthetic row is a user permission grant.
        sqlx::query("UPDATE evaluation.frozen_submissions SET contract=$2,manifest_sha256=$3,content_sha256=$4 WHERE frozen_submission_id=$1")
            .bind(frozen.id.as_uuid()).bind(serde_json::to_value(&frozen)?).bind(&manifest_sha).bind(&frozen.content_sha256).execute(&db.pool).await?;
        sqlx::query("INSERT INTO evaluation.submission_freeze_requests(frozen_submission_id,project_id,course_id,environment_id,idempotency_key,request_sha256,source_identity_sha256,state,current_attempt,contract,completed_at) VALUES($1,$2,$3,$4,$5,$6,$6,'completed',1,$7,clock_timestamp())")
            .bind(frozen.id.as_uuid()).bind(db.project_id.as_uuid()).bind(db.course_id.as_uuid()).bind(frozen.environment.environment_id.as_uuid())
            .bind(&command.idempotency_key).bind(Sha256Digest::of_bytes(b"fixture-freeze-identity").to_string()).bind(serde_json::to_value(&frozen)?).execute(&db.pool).await?;
        let mut publish = publish_request(db.project_id, db.course_id, db.actor_id)?;
        publish.evaluation_spec = advisory_spec()?;
        publish.execution_binding.package.files[0].path = "rubric.md".to_owned();
        db.release = match db
            .store
            .publish_release(
                &publish,
                &IdempotencyKey::parse("review-release")?,
                now,
                "review-fixture",
            )
            .await?
        {
            EvaluationReleaseReservation::Created(value)
            | EvaluationReleaseReservation::Replayed(value) => value,
        };
        db.run = match db
            .store
            .create_run(
                &InternalCreateEvaluationRunRequest {
                    project_id: db.project_id,
                    course_id: Some(db.course_id),
                    release_id: db.release.id,
                    release_revision: db.release.revision,
                    frozen_submission_id: frozen.id,
                    actor_id: db.actor_id,
                    identity: EvaluationRunIdentity {
                        runtime_identity: db.release.runtime_identity.clone(),
                        trace_id: "review-fixture".to_owned(),
                    },
                },
                &IdempotencyKey::parse("review-run")?,
                now,
                "review-fixture",
                &admission(&db.release, &frozen),
            )
            .await?
        {
            EvaluationRunReservation::Created(value)
            | EvaluationRunReservation::Replayed(value) => value,
        };
        let lease = db
            .store
            .claim_next_step("review-worker", Duration::from_mins(5))
            .await?
            .ok_or("review not claimable")?;
        let context = context(&db, &lease)?;
        let mut policy = super::super::tests::advisory_request(now).policy;
        policy.budget.max_schema_repairs = 1;
        policy.budget.timeout_milliseconds = 10_000;
        policy.project_id = db.project_id;
        policy.course_id = Some(db.course_id);
        let rubric = &publish.execution_binding.package.files[0];
        let source = FrozenModelInputSource {
            task_run_id: lease.task_run_id,
            parent_lease_sha256: contracts::http::model_input_parent_lease_sha256(
                lease.lease_token(),
            ),
            frozen_submission_id: frozen.id,
            manifest_revision: frozen.manifest_revision,
            manifest_sha256: manifest_sha,
            archive: frozen.object.clone(),
            content_sha256: frozen.content_sha256.clone(),
            evaluation_release_id: db.release.id,
            evaluation_release_revision: db.release.revision,
            environment: frozen.environment.clone(),
            rubric: ModelInputRubricIdentity {
                package_id: publish.execution_binding.package.id,
                package_revision: publish.execution_binding.package.revision,
                path: rubric.path.clone(),
                artifact: rubric.object.clone(),
                sha256: rubric.sha256.clone(),
            },
            rubric_permission: Some(ModelInputRubricPermissionRef {
                authoring_project_id: db.project_id,
                policy_id: PolicyId::new(),
                policy_revision: Revision::new(1)?,
                approved_by: ActorId::new(),
                approver_course_membership_revision: Revision::new(1)?,
            }),
        };
        let binding = ModelInputAuthorizationBinding {
            session_sha256: original.session_sha256,
            actor_id: original.actor_id,
            project_id: original.project_id,
            course_id: original.course_id,
            operation_id: original.operation_id,
            valid_until: original.valid_until,
            policy_id: policy.id,
            policy_revision: policy.revision,
            policy_sha256: contracts::http::model_input_policy_sha256(&policy)?,
            selection_sha256: contracts::http::frozen_model_input_selection_sha256(
                &source,
                &[("answer.txt".to_owned(), frozen.files[0].sha256.clone())],
            )?,
            source: ModelInputAuthorizationSource::FrozenSubmission(Box::new(source)),
        };
        {
            let mut state = cluster.state.lock().await;
            state.model_binding = Some(serde_json::to_value(&binding)?);
            state.objects.insert(
                format!("/internal/v1/projects/{}/llm-egress-policy", db.project_id),
                serde_json::to_value(&policy)?,
            );
            state.s3_objects.insert(
                format!("/test-bucket/evaluation-tests/frozen/{}", frozen.id),
                MockS3Object {
                    body: archive,
                    content_type: frozen.object.media_type.clone(),
                    version_id: frozen.object.object_version.clone(),
                },
            );
            state.s3_objects.insert(
                format!(
                    "/test-package-bucket/{}",
                    publish.execution_binding.object_locators[&rubric.object.artifact_id]
                ),
                MockS3Object {
                    body: b"x".to_vec(),
                    content_type: rubric.object.media_type.clone(),
                    version_id: rubric.object.object_version.clone(),
                },
            );
        }
        Ok(Self {
            db,
            frozen,
            context,
            command,
            binding,
        })
    }
    async fn reset(&self, cluster: &MockCluster) -> Result<(), Box<dyn Error>> {
        let mut s = cluster.state.lock().await;
        s.calls.clear();
        s.model_requests.clear();
        s.model_denied = false;
        s.model_rechecks = 0;
        s.model_deny_recheck_at = None;
        s.model_binding = Some(serde_json::to_value(&self.binding)?);
        Ok(())
    }
}

fn advisory_spec() -> Result<EvaluationSpec, contracts::evaluation::EvaluationSpecError> {
    EvaluationSpec::from_yaml(
        r#"apiVersion: evaluation.labweaver.io/v1
kind: EvaluationSpec
metadata:
  name: frozen-review
  version: "1.0.0"
spec:
  submission:
    collector:
      kind: workspace_snapshot
      include: [answer.txt]
      maxBytes: 1024
    llmReadable: [answer.txt]
  steps:
    - role: advisory
      id: review
      runner:
        kind: llm_review
        include: [answer.txt]
        rubric: evaluator://rubric.md
        outputMode: goal_assessment
      failurePolicy: continue_advisory
    - role: score
      id: deterministic
      runner:
        kind: file_assertion
        requiredFiles: [answer.txt]
      checker:
        kind: exit_code
        expected: 0
      score:
        max: 1
      failurePolicy: stop
  aggregation:
    kind: deterministic_sum
    maxScore: 1
    gates: []
  review:
    teacherApprovalRequiredForRelease: true
    forceManualWhen: []
"#,
    )
}

async fn build(
    fixture: &ReviewFixture,
    runner: &KubernetesEvaluationRunner,
) -> Result<contracts::http::InternalAgentLlmReviewRequest, crate::execution::ExecutionError> {
    runner.build_advisory_request(
        &fixture.context,
        &fixture.frozen,
        &["answer.txt".to_owned()],
        "evaluator://rubric.md",
        contracts::evaluation::AdvisoryOutputMode::GoalAssessment,
    )
    .await
}
async fn counts(cluster: &MockCluster) -> (usize, usize) {
    let calls = cluster.calls().await;
    (
        calls
            .iter()
            .filter(|(m, p)| {
                *m == Method::GET
                    && (p.starts_with("/test-bucket/") || p.starts_with("/test-package-bucket/"))
            })
            .count(),
        calls
            .iter()
            .filter(|(m, p)| *m == Method::POST && p == "/internal/v1/llm-reviews")
            .count(),
    )
}

#[tokio::test]
async fn frozen_advisory_refusals_and_substitutions_read_and_send_nothing()
-> Result<(), Box<dyn Error>> {
    let mut cluster = spawn_cluster().await?;
    let authority = spawn_authority().await?;
    let temp = TempDir::new()?;
    let fixture = ReviewFixture::start(&cluster).await?;
    let runner = build_runner(&cluster, &authority, &temp, &fixture.db.pool).await?;
    // Control is an external authority in this receiver test. These outcomes do not replace
    // native Control rubric/session permission tests or claim a real IdP/model chain.
    for reason in [
        "unknown rubric",
        "hidden rubric",
        "expired original session",
        "revoked original session",
    ] {
        fixture.reset(&cluster).await?;
        cluster.state.lock().await.model_denied = true;
        assert!(
            runner.execute_advisory(
                &fixture.context,
                &fixture.frozen,
                &["answer.txt".to_owned()],
                "evaluator://rubric.md",
                contracts::evaluation::AdvisoryOutputMode::GoalAssessment
            )
            .await
            .is_err(),
            "{reason}"
        );
        assert_eq!(counts(&cluster).await, (0, 0));
        assert_eq!(
            cluster.state.lock().await.model_requests.len(),
            1,
            "must reach authority: {reason}"
        );
    }
    for field in [
        "sessionSha256",
        "source.archive.objectVersion",
        "source.rubric.artifact.objectVersion",
        "source.rubric.packageRevision",
        "source.manifestSha256",
        "source.evaluationReleaseRevision",
        "policySha256",
        "source.rubricPermission",
    ] {
        fixture.reset(&cluster).await?;
        let mut v = serde_json::to_value(&fixture.binding)?;
        let key = format!("/{}", field.replace('.', "/"));
        *v.pointer_mut(&key).ok_or("missing field")? = if field.ends_with("Revision") {
            json!(2)
        } else if field.ends_with("rubricPermission") {
            Value::Null
        } else if field.ends_with("Sha256") {
            json!("d".repeat(64))
        } else {
            json!("changed-version")
        };
        cluster.state.lock().await.model_binding = Some(v);
        assert!(build(&fixture, &runner).await.is_err(), "{field}");
        assert_eq!(counts(&cluster).await, (0, 0));
        assert_eq!(cluster.state.lock().await.model_requests.len(), 1);
    }
    fixture.reset(&cluster).await?;
    sqlx::query("UPDATE evaluation.submission_freeze_commands SET contract=contract || jsonb_build_object('inputAuthorizationContext',NULL) WHERE frozen_submission_id=$1")
        .bind(fixture.frozen.id.as_uuid()).execute(&fixture.db.pool).await?;
    assert!(build(&fixture, &runner).await.is_err());
    assert_eq!(counts(&cluster).await, (0, 0));
    assert!(cluster.state.lock().await.model_requests.is_empty());
    cluster.stop().await;
    Ok(())
}

#[tokio::test]
async fn frozen_advisory_rechecks_reads_and_preserves_first_intent_after_lost_response()
-> Result<(), Box<dyn Error>> {
    let mut cluster = spawn_cluster().await?;
    let authority = spawn_authority().await?;
    let temp = TempDir::new()?;
    let fixture = ReviewFixture::start(&cluster).await?;
    let runner = build_runner(&cluster, &authority, &temp, &fixture.db.pool).await?;
    for (at, n) in [(1, 0), (2, 1), (3, 2)] {
        fixture.reset(&cluster).await?;
        cluster.state.lock().await.model_deny_recheck_at = Some(at);
        assert!(build(&fixture, &runner).await.is_err());
        assert_eq!(counts(&cluster).await, (n, 0));
    }
    fixture.reset(&cluster).await?;
    let mut replay = fixture.command.clone();
    replay.frozen_submission_id = FrozenSubmissionId::new();
    replay.operation_id = contracts::OperationId::new();
    replay
        .input_authorization_context
        .as_mut()
        .ok_or("context")?
        .session_sha256 = "e".repeat(64);
    assert!(
        crate::PgFreezeCommandStore::new(fixture.db.pool.clone())
            .accept(&replay)
            .await?
            .replay
    );
    let req = build(&fixture, &runner).await?;
    req.validate()?;
    assert_eq!(req.input_authorization, fixture.binding);
    assert_eq!(req.files[0].content, "x");
    assert_eq!(req.rubric.content, "x");
    assert_eq!(counts(&cluster).await, (2, 0));
    let intent = super::super::execution_resources(
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
    cluster.state.lock().await.model_denied = true;
    assert!(
        runner.load_or_enqueue_advisory(&fixture.context, &req, "review-first")
            .await
            .is_err()
    );
    assert_eq!(counts(&cluster).await, (2, 0));
    {
        let mut s = cluster.state.lock().await;
        s.model_denied = false;
        s.agent_lose_create_response = true;
    }
    assert!(
        runner.load_or_enqueue_advisory(&fixture.context, &req, "review-first")
            .await
            .is_err()
    );
    assert_eq!(counts(&cluster).await, (2, 1));
    cluster.state.lock().await.model_denied = true;
    let cp =
        fixture.db.store
            .load_execution_checkpoint(&fixture.context.lease)
            .await?
            .ok_or("checkpoint")?;
    let recovered = super::super::parse_advisory_recovery_request(
        cp.execution_resources.as_ref().ok_or("intent")?,
        &fixture.context,
    )?;
    assert_eq!(
        serde_json::to_value(&recovered)?,
        serde_json::to_value(&req)?
    );
    runner.load_or_enqueue_advisory(&fixture.context, &recovered, "review-first")
        .await?;
    assert_eq!(counts(&cluster).await, (2, 1));
    cluster.stop().await;
    Ok(())
}

#[tokio::test]
async fn frozen_advisory_cancel_deadline_and_expired_database_lease_block_sends()
-> Result<(), Box<dyn Error>> {
    let mut cluster = spawn_cluster().await?;
    let authority = spawn_authority().await?;
    let temp = TempDir::new()?;
    let fixture = ReviewFixture::start(&cluster).await?;
    let runner = build_runner(&cluster, &authority, &temp, &fixture.db.pool).await?;
    let expiry: time::OffsetDateTime = sqlx::query_scalar(
        "SELECT lease_expires_at FROM evaluation.evaluation_step_attempts WHERE task_run_id=$1",
    )
    .bind(fixture.context.lease.task_run_id.as_uuid())
    .fetch_one(&fixture.db.pool)
    .await?;
    sqlx::query("UPDATE evaluation.evaluation_step_attempts SET lease_expires_at=clock_timestamp()-interval '1 millisecond' WHERE task_run_id=$1").bind(fixture.context.lease.task_run_id.as_uuid()).execute(&fixture.db.pool).await?;
    assert!(!fixture.context.lease_lost.is_cancelled());
    assert!(build(&fixture, &runner).await.is_err());
    assert_eq!(counts(&cluster).await, (0, 0));
    assert!(cluster.state.lock().await.model_requests.is_empty());
    sqlx::query(
        "UPDATE evaluation.evaluation_step_attempts SET lease_expires_at=$2 WHERE task_run_id=$1",
    )
    .bind(fixture.context.lease.task_run_id.as_uuid())
    .bind(expiry)
    .execute(&fixture.db.pool)
    .await?;
    let mut wrong = fixture.context.clone();
    wrong.lease.task_run_id = contracts::TaskRunId::new();
    assert!(
        runner.build_advisory_request(
            &wrong,
            &fixture.frozen,
            &["answer.txt".to_owned()],
            "evaluator://rubric.md",
            contracts::evaluation::AdvisoryOutputMode::GoalAssessment
        )
        .await
        .is_err()
    );
    assert_eq!(counts(&cluster).await, (0, 0));
    let mut req = build(&fixture, &runner).await?;
    fixture.reset(&cluster).await?;
    req.deadline_at = UtcTimestamp::from_utc(
        fixture.db.store.authority_now().await?.get() - time::Duration::seconds(1),
    )?;
    assert!(
        runner.load_or_enqueue_advisory(&fixture.context, &req, "expired-review")
            .await
            .is_err()
    );
    assert_eq!(counts(&cluster).await, (0, 0));
    let run = fixture.db.store.load_run(fixture.context.run.id).await?;
    fixture.db.store
        .request_cancellation(
            run.id,
            &contracts::http::InternalEvaluationRunMutationRequest {
                project_id: run.project_id,
                course_id: run.course_id,
                actor_id: run.actor_id,
                expected_revision: run.revision,
            },
            &IdempotencyKey::parse("cancel-review")?,
            fixture.db.store.authority_now().await?,
            "cancel-review",
        )
        .await?;
    assert!(build(&fixture, &runner).await.is_err());
    assert_eq!(counts(&cluster).await, (0, 0));
    cluster.stop().await;
    Ok(())
}

mod parent_admission_tests {
    include!("parent_admission.rs");
}
