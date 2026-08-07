//! Real `PostgreSQL` proof for `EvaluationRelease`, `EvaluationRun` and `StepRun` fencing.
#![allow(
    clippy::expect_used,
    reason = "the integration fixture uses fixed valid contract identities"
)]

use std::time::Duration;

use contracts::{
    ActorId, ApprovalId, CandidateId, CourseId, DiagnosticCode, EnvironmentId, EvaluationRunId,
    EvaluationStepRunId, FrozenSubmissionId, Revision, Sha256Digest, UtcTimestamp,
    evaluation::{
        EvaluationRunIdentity, EvaluationRunState, EvaluationRuntimeIdentity, EvaluationSpec,
        EvaluationStepCompletion, EvaluationStepRole, EvaluationStepRunState,
    },
    http::{
        IdempotencyKey, InternalCreateEvaluationRunRequest, InternalEvaluationRunMutationRequest,
        InternalPublishEvaluationReleaseRequest,
    },
};
use evaluation_service::{
    EvaluationControlStoreError, EvaluationReleaseReservation, EvaluationRunReservation,
    EvaluationStepLease, PgEvaluationControlStore,
};
use sqlx::Row;
use sqlx::postgres::PgPoolOptions;
use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;

#[tokio::test]
async fn release_and_run_are_idempotent_and_close_identity()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestContext::start(oj_spec()?).await?;
    let request = fixture.publish_request.clone();
    let release_key = idempotency("release-idem-01")?;
    let first = fixture
        .store
        .publish_release(
            &request,
            &release_key,
            fixture.now().await?,
            "trace-release-1",
        )
        .await?;
    let replay = fixture
        .store
        .publish_release(
            &request,
            &release_key,
            fixture.now().await?,
            "trace-release-2",
        )
        .await?;
    let release = match (first, replay) {
        (
            EvaluationReleaseReservation::Created(release),
            EvaluationReleaseReservation::Replayed(replay),
        ) if release.id == replay.id => release,
        other => return Err(format!("unexpected release reservation: {other:?}").into()),
    };
    assert_eq!(
        count(&fixture.pool, "evaluation.evaluation_releases").await?,
        1
    );
    assert_eq!(count(&fixture.pool, "evaluation.outbox_events").await?, 1);

    fixture.seed_frozen_submission().await?;
    let run_request = fixture.create_run_request(&release, "trace-run-1")?;
    let run_key = idempotency("run-idem-0001")?;
    let first_run = fixture
        .store
        .create_run(&run_request, &run_key, fixture.now().await?, "trace-run-1")
        .await?;
    let replayed_run = fixture
        .store
        .create_run(&run_request, &run_key, fixture.now().await?, "trace-run-1")
        .await?;
    let run = match (first_run, replayed_run) {
        (EvaluationRunReservation::Created(run), EvaluationRunReservation::Replayed(replay))
            if run.id == replay.id =>
        {
            run
        }
        other => return Err(format!("unexpected run reservation: {other:?}").into()),
    };
    assert_eq!(run.state, EvaluationRunState::Queued);
    assert!(run.steps.iter().all(|step| step.run_id == run.id));
    assert_eq!(
        count(&fixture.pool, "evaluation.evaluation_step_runs").await?,
        i64::try_from(run.steps.len())?
    );
    assert_eq!(count(&fixture.pool, "evaluation.evaluation_runs").await?, 1);

    let mut wrong_identity = run_request.clone();
    wrong_identity
        .identity
        .runtime_identity
        .runtime_artifact_sha256 = Sha256Digest::of_bytes(b"different-runtime");
    let error = fixture
        .store
        .create_run(
            &wrong_identity,
            &idempotency("run-idem-0002")?,
            fixture.now().await?,
            "trace-run-1",
        )
        .await
        .expect_err("runtime identity mismatch must fail closed");
    assert!(matches!(
        error,
        EvaluationControlStoreError::IdentityMismatch
    ));
    Ok(())
}

#[tokio::test]
async fn worker_lease_fencing_retry_and_cleanup_are_authoritative()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestContext::start(single_score_spec()?).await?;
    let run = fixture.create_seeded_run("trace-retry-1").await?;
    let first = fixture
        .store
        .claim_next_step("worker-a", Duration::from_secs(30))
        .await?
        .expect("step must be claimable");
    assert_eq!(first.attempt, 1);
    assert!(
        fixture
            .store
            .claim_next_step("worker-b", Duration::from_secs(30))
            .await?
            .is_none()
    );

    assert_completion_attempt_fences(&fixture, run.id, &first, "trace-retry-1").await?;

    let failed = complete_leased_step(
        &fixture,
        run.id,
        &first,
        &failed_completion(true),
        "trace-retry-1",
    )
    .await?;
    assert_eq!(failed.state, EvaluationRunState::Failed);
    assert!(failed.cleanup_verified);

    let retried = fixture
        .store
        .retry_step(
            run.id,
            first.step_run_id,
            &mutation(fixture.course_id, failed.revision, fixture.actor_id),
            &idempotency("retry-step-01")?,
            fixture.now().await?,
            "trace-retry-1",
        )
        .await?;
    assert_eq!(retried.state, EvaluationRunState::Running);
    let second = fixture
        .store
        .claim_next_step("worker-b", Duration::from_secs(30))
        .await?
        .expect("retried step must be claimable");
    assert_eq!(second.step_run_id, first.step_run_id);
    assert_eq!(second.attempt, 2);

    let succeeded = complete_leased_step(
        &fixture,
        run.id,
        &second,
        &success_completion(7),
        "trace-retry-1",
    )
    .await?;
    assert_eq!(succeeded.state, EvaluationRunState::Succeeded);
    assert_eq!(succeeded.awarded_score, 7);
    assert!(succeeded.cleanup_verified);
    Ok(())
}

#[tokio::test]
async fn cancellation_and_expired_lease_recovery_preserve_cleanup_boundary()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestContext::start(single_score_spec()?).await?;
    let cancelled = fixture.create_seeded_run("trace-cancel-1").await?;
    let cancelled = fixture
        .store
        .request_cancellation(
            cancelled.id,
            &mutation(fixture.course_id, cancelled.revision, fixture.actor_id),
            &idempotency("cancel-run-01")?,
            fixture.now().await?,
            "trace-cancel-1",
        )
        .await?;
    assert_eq!(cancelled.state, EvaluationRunState::Cancelled);
    assert!(cancelled.cleanup_verified);
    assert!(cancelled.completed_at.is_some());

    let recoverable = fixture.create_seeded_run("trace-recover-1").await?;
    let lease = fixture
        .store
        .claim_next_step("worker-recover", Duration::from_secs(30))
        .await?
        .expect("recovery step must be claimable");
    expire_lease(&fixture.pool, lease.step_run_id).await?;
    assert_eq!(fixture.store.recover_expired_step_attempts(4).await?, 1);
    let failed = fixture.store.load_run(recoverable.id).await?;
    assert_eq!(failed.state, EvaluationRunState::Failed);
    assert!(!failed.cleanup_verified);
    assert!(failed.completed_at.is_none());
    assert_eq!(
        failed.diagnostic_code.as_ref().map(DiagnosticCode::as_str),
        Some("LW_EVALUATION_STEP_LEASE_EXPIRED")
    );

    let cleaned = fixture
        .store
        .verify_step_cleanup(
            failed.id,
            lease.step_run_id,
            &mutation(fixture.course_id, failed.revision, fixture.actor_id),
            &idempotency("cleanup-step-1")?,
            fixture.now().await?,
            "trace-recover-1",
        )
        .await?;
    assert_eq!(cleaned.state, EvaluationRunState::Failed);
    assert!(cleaned.cleanup_verified);
    assert!(cleaned.completed_at.is_some());
    assert!(attempt_cleanup_verified(&fixture.pool, lease.step_run_id, lease.attempt).await?);
    assert!(step_cleanup_verified(&fixture.pool, lease.step_run_id).await?);
    assert!(
        operator_event_count(&fixture.pool, fixture.actor_id).await? >= 2,
        "cancel and cleanup events should carry the mutation actor"
    );
    Ok(())
}

#[test]
fn completion_contract_enforces_score_diagnostic_cleanup_matrix()
-> Result<(), Box<dyn std::error::Error>> {
    success_completion(7).validate(EvaluationStepRole::Score, 7)?;
    non_score_success_completion().validate(EvaluationStepRole::Gate, 0)?;
    non_score_success_completion().validate(EvaluationStepRole::Advisory, 0)?;
    failed_completion(false).validate(EvaluationStepRole::Score, 7)?;

    let mut missing_score = success_completion(7);
    missing_score.awarded_score = None;
    assert!(
        missing_score
            .validate(EvaluationStepRole::Score, 7)
            .is_err()
    );

    let mut scored_gate = non_score_success_completion();
    scored_gate.awarded_score = Some(0);
    assert!(scored_gate.validate(EvaluationStepRole::Gate, 0).is_err());

    let mut failed_with_score = failed_completion(false);
    failed_with_score.awarded_score = Some(1);
    assert!(
        failed_with_score
            .validate(EvaluationStepRole::Score, 7)
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn forked_dag_failure_continues_independent_branch_and_retry_restores_successors()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestContext::start(forked_dag_spec()?).await?;
    let run = fixture.create_seeded_run("trace-dag-1").await?;
    let gate = fixture
        .store
        .claim_next_step("worker-gate", Duration::from_secs(30))
        .await?
        .expect("gate must be claimable first");
    assert_eq!(gate.step_id, "preflight");

    let after_gate_failure = complete_leased_step(
        &fixture,
        run.id,
        &gate,
        &failed_completion(true),
        "trace-dag-1",
    )
    .await?;
    assert_eq!(after_gate_failure.state, EvaluationRunState::Running);
    assert_eq!(
        step_by_id(&after_gate_failure, "dependent-score").state,
        EvaluationStepRunState::Skipped
    );
    assert_eq!(
        step_by_id(&after_gate_failure, "independent-score").state,
        EvaluationStepRunState::Pending
    );

    let independent = fixture
        .store
        .claim_next_step("worker-independent", Duration::from_secs(30))
        .await?
        .expect("independent branch should still run");
    assert_eq!(independent.step_id, "independent-score");
    let after_independent = complete_leased_step(
        &fixture,
        run.id,
        &independent,
        &success_completion(3),
        "trace-dag-1",
    )
    .await?;
    assert_eq!(after_independent.state, EvaluationRunState::Failed);
    assert_eq!(after_independent.awarded_score, 3);

    let retried = fixture
        .store
        .retry_step(
            run.id,
            gate.step_run_id,
            &mutation(
                fixture.course_id,
                after_independent.revision,
                fixture.actor_id,
            ),
            &idempotency("retry-gate-dag")?,
            fixture.now().await?,
            "trace-dag-1",
        )
        .await?;
    assert_eq!(retried.state, EvaluationRunState::Running);
    assert_eq!(
        step_by_id(&retried, "preflight").state,
        EvaluationStepRunState::Retryable
    );
    assert_eq!(
        step_by_id(&retried, "dependent-score").state,
        EvaluationStepRunState::Pending
    );

    let retried_gate = fixture
        .store
        .claim_next_step("worker-gate-2", Duration::from_secs(30))
        .await?
        .expect("retried gate should be claimable");
    assert_eq!(retried_gate.step_id, "preflight");
    let after_gate_success = complete_leased_step(
        &fixture,
        run.id,
        &retried_gate,
        &non_score_success_completion(),
        "trace-dag-1",
    )
    .await?;
    assert_eq!(after_gate_success.state, EvaluationRunState::Running);

    let dependent = fixture
        .store
        .claim_next_step("worker-dependent", Duration::from_secs(30))
        .await?
        .expect("restored dependent score should run");
    assert_eq!(dependent.step_id, "dependent-score");
    let completed = complete_leased_step(
        &fixture,
        run.id,
        &dependent,
        &success_completion(5),
        "trace-dag-1",
    )
    .await?;
    assert_eq!(completed.state, EvaluationRunState::Succeeded);
    assert_eq!(completed.awarded_score, 8);
    assert_eq!(duplicate_step_revision_events(&fixture.pool).await?, 0);
    Ok(())
}

#[tokio::test]
async fn score_continue_failure_allows_dependent_successor_and_does_not_fail_run()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestContext::start(score_continue_spec()?).await?;
    let run = fixture.create_seeded_run("trace-score-continue").await?;
    let flaky = fixture
        .store
        .claim_next_step("worker-flaky", Duration::from_secs(30))
        .await?
        .expect("first score must be claimable");
    assert_eq!(flaky.step_id, "flaky-score");
    let after_failure = complete_leased_step(
        &fixture,
        run.id,
        &flaky,
        &failed_completion(true),
        "trace-score-continue",
    )
    .await?;
    assert_eq!(after_failure.state, EvaluationRunState::Running);
    assert_eq!(
        step_by_id(&after_failure, "independent-score").state,
        EvaluationStepRunState::Pending
    );
    assert_eq!(
        step_by_id(&after_failure, "dependent-after-flaky").state,
        EvaluationStepRunState::Pending
    );

    let dependent = fixture
        .store
        .claim_next_step("worker-dependent-score", Duration::from_secs(30))
        .await?
        .expect("failurePolicy continue must allow dependent scores to run");
    assert_eq!(dependent.step_id, "dependent-after-flaky");
    let after_dependent = complete_leased_step(
        &fixture,
        run.id,
        &dependent,
        &success_completion(2),
        "trace-score-continue",
    )
    .await?;
    assert_eq!(after_dependent.state, EvaluationRunState::Running);
    assert_eq!(after_dependent.awarded_score, 2);

    let independent = fixture
        .store
        .claim_next_step("worker-independent-score", Duration::from_secs(30))
        .await?
        .expect("failurePolicy continue must leave independent scores runnable");
    assert_eq!(independent.step_id, "independent-score");
    let completed = complete_leased_step(
        &fixture,
        run.id,
        &independent,
        &success_completion(4),
        "trace-score-continue",
    )
    .await?;
    assert_eq!(completed.state, EvaluationRunState::Succeeded);
    assert_eq!(completed.awarded_score, 6);
    Ok(())
}

#[tokio::test]
async fn advisory_failure_follows_declared_cleanup_semantics()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestContext::start(advisory_then_score_spec()?).await?;
    let run = fixture.create_seeded_run("trace-advisory").await?;
    let advisory = fixture
        .store
        .claim_next_step("worker-advisory", Duration::from_secs(30))
        .await?
        .expect("advisory must be first");
    assert_eq!(advisory.step_id, "lint-advisory");
    let after_advisory = complete_leased_step(
        &fixture,
        run.id,
        &advisory,
        &failed_completion(false),
        "trace-advisory",
    )
    .await?;
    assert_eq!(after_advisory.state, EvaluationRunState::Running);
    assert!(after_advisory.diagnostic_code.is_none());

    let score = fixture
        .store
        .claim_next_step("worker-score", Duration::from_secs(30))
        .await?
        .expect("deterministic score must continue after advisory failure");
    assert_eq!(score.step_id, "deterministic-score");
    let completed = complete_leased_step(
        &fixture,
        run.id,
        &score,
        &success_completion(6),
        "trace-advisory",
    )
    .await?;
    assert_eq!(completed.state, EvaluationRunState::Running);
    assert_eq!(completed.awarded_score, 6);

    let cleaned = fixture
        .store
        .verify_step_cleanup(
            run.id,
            advisory.step_run_id,
            &mutation(fixture.course_id, completed.revision, fixture.actor_id),
            &idempotency("cleanup-advisory")?,
            fixture.now().await?,
            "trace-advisory",
        )
        .await?;
    assert_eq!(cleaned.state, EvaluationRunState::Succeeded);
    assert_eq!(cleaned.awarded_score, 6);
    Ok(())
}

#[tokio::test]
async fn claim_and_complete_advance_run_revision_and_reject_stale_mutations()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestContext::start(single_score_spec()?).await?;
    let run = fixture.create_seeded_run("trace-revision").await?;
    let original_revision = run.revision;
    let lease = fixture
        .store
        .claim_next_step("worker-revision", Duration::from_secs(30))
        .await?
        .expect("single step must be claimable");
    let claimed = fixture.store.load_run(run.id).await?;
    assert!(claimed.revision.get() > original_revision.get());
    let stale = fixture
        .store
        .request_cancellation(
            run.id,
            &mutation(fixture.course_id, original_revision, fixture.actor_id),
            &idempotency("stale-cancel-after-claim")?,
            fixture.now().await?,
            "trace-revision",
        )
        .await
        .expect_err("claim must invalidate stale expectedRevision");
    assert!(matches!(stale, EvaluationControlStoreError::StateConflict));

    let completed = fixture
        .store
        .complete_step(
            fixture.course_id,
            run.id,
            lease.step_run_id,
            lease.attempt,
            &lease.worker_id,
            &lease.worker_san_uri,
            &lease.runtime_identity,
            lease.lease_token(),
            &success_completion(7),
            "trace-revision",
        )
        .await?;
    assert!(completed.revision.get() > claimed.revision.get());
    assert_eq!(completed.state, EvaluationRunState::Succeeded);
    Ok(())
}

#[tokio::test]
async fn expired_completion_loses_to_database_clock_and_recovery_is_single_writer()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestContext::start(single_score_spec()?).await?;
    let run = fixture.create_seeded_run("trace-expired-boundary").await?;
    let lease = fixture
        .store
        .claim_next_step("worker-expired", Duration::from_secs(30))
        .await?
        .expect("single step must be claimable");
    expire_lease(&fixture.pool, lease.step_run_id).await?;

    let completion = fixture
        .store
        .complete_step(
            fixture.course_id,
            run.id,
            lease.step_run_id,
            lease.attempt,
            &lease.worker_id,
            &lease.worker_san_uri,
            &lease.runtime_identity,
            lease.lease_token(),
            &success_completion(7),
            "trace-expired-boundary",
        )
        .await
        .expect_err("DB clock must reject completion after expiry");
    assert!(matches!(completion, EvaluationControlStoreError::LeaseLost));

    assert_eq!(fixture.store.recover_expired_step_attempts(4).await?, 1);
    assert_eq!(fixture.store.recover_expired_step_attempts(4).await?, 0);
    let failed = fixture.store.load_run(run.id).await?;
    assert_eq!(failed.state, EvaluationRunState::Failed);
    Ok(())
}

struct TestContext {
    _container: testcontainers::ContainerAsync<Postgres>,
    pool: sqlx::PgPool,
    store: PgEvaluationControlStore,
    course_id: CourseId,
    actor_id: ActorId,
    frozen_submission_id: FrozenSubmissionId,
    frozen_submission_sha256: Sha256Digest,
    source_identity_sha256: Sha256Digest,
    publish_request: InternalPublishEvaluationReleaseRequest,
}

impl TestContext {
    async fn start(spec: EvaluationSpec) -> Result<Self, Box<dyn std::error::Error>> {
        let container = Postgres::default().with_tag("17.5-alpine").start().await?;
        let database_url = format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            container.get_host_port_ipv4(5432).await?
        );
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect(&database_url)
            .await?;
        let migrations = format!(
            "CREATE SCHEMA evaluation; SET search_path TO evaluation;\n{}\n{}",
            include_str!("../../../migrations/evaluation/0001_sprint2_baseline.sql"),
            include_str!("../../../migrations/evaluation/0002_evaluation_control_plane.sql")
        );
        sqlx::raw_sql(&migrations).execute(&pool).await?;
        let store = PgEvaluationControlStore::new(pool.clone());
        let course_id = CourseId::new();
        let actor_id = ActorId::new();
        let publish_request = InternalPublishEvaluationReleaseRequest {
            course_id,
            candidate_id: CandidateId::new(),
            candidate_revision: Revision::new(2)?,
            candidate_sha256: Sha256Digest::of_bytes(b"evaluation-candidate"),
            approval_id: ApprovalId::new(),
            approval_revision: Revision::new(3)?,
            approval_sha256: Sha256Digest::of_bytes(b"evaluation-approval"),
            evaluation_spec: spec,
            runtime_identity: runtime_identity(),
            published_by: actor_id,
        };
        Ok(Self {
            _container: container,
            pool,
            store,
            course_id,
            actor_id,
            frozen_submission_id: FrozenSubmissionId::new(),
            frozen_submission_sha256: Sha256Digest::of_bytes(b"frozen-submission-content"),
            source_identity_sha256: Sha256Digest::of_bytes(b"source-identity"),
            publish_request,
        })
    }

    async fn now(&self) -> Result<UtcTimestamp, EvaluationControlStoreError> {
        self.store.authority_now().await
    }

    async fn seed_frozen_submission(&self) -> Result<(), Box<dyn std::error::Error>> {
        sqlx::query(
            "INSERT INTO evaluation.frozen_submissions \
             (frozen_submission_id,course_id,environment_id,manifest_sha256,content_sha256,\
              schema_version,tool_version,contract,frozen_at,idempotency_key,\
              source_identity_sha256,object_key,object_version) \
             VALUES ($1,$2,$3,$4,$5,'submission.freeze/v1','control-plane-test',$6,$7,$8,$9,$10,$11) \
             ON CONFLICT (frozen_submission_id) DO NOTHING",
        )
        .bind(self.frozen_submission_id.as_uuid())
        .bind(self.course_id.as_uuid())
        .bind(EnvironmentId::new().as_uuid())
        .bind(Sha256Digest::of_bytes(b"submission-manifest").to_string())
        .bind(self.frozen_submission_sha256.to_string())
        .bind(serde_json::json!({
            "frozenSubmissionId": self.frozen_submission_id,
            "courseId": self.course_id,
            "contentSha256": self.frozen_submission_sha256,
        }))
        .bind(self.now().await?.get())
        .bind(format!("freeze:{}", self.frozen_submission_id))
        .bind(self.source_identity_sha256.to_string())
        .bind(format!("frozen/{}", self.frozen_submission_id))
        .bind("v1")
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    fn create_run_request(
        &self,
        release: &contracts::evaluation::EvaluationRelease,
        trace_id: &str,
    ) -> Result<InternalCreateEvaluationRunRequest, Box<dyn std::error::Error>> {
        Ok(InternalCreateEvaluationRunRequest {
            course_id: self.course_id,
            release_id: release.id,
            release_revision: release.revision,
            frozen_submission_id: self.frozen_submission_id,
            actor_id: self.actor_id,
            identity: EvaluationRunIdentity {
                release_identity_sha256: release.release_identity_sha256()?,
                evaluation_spec_sha256: release.evaluation_spec_sha256,
                frozen_submission_sha256: self.frozen_submission_sha256,
                source_identity_sha256: self.source_identity_sha256,
                runtime_identity: release.runtime_identity.clone(),
                trace_id: trace_id.to_owned(),
            },
        })
    }

    async fn create_seeded_run(
        &self,
        trace_id: &str,
    ) -> Result<contracts::evaluation::EvaluationRun, Box<dyn std::error::Error>> {
        let release = match self
            .store
            .publish_release(
                &self.publish_request,
                &idempotency("release-seeded")?,
                self.now().await?,
                trace_id,
            )
            .await?
        {
            EvaluationReleaseReservation::Created(release)
            | EvaluationReleaseReservation::Replayed(release) => release,
        };
        self.seed_frozen_submission().await?;
        match self
            .store
            .create_run(
                &self.create_run_request(&release, trace_id)?,
                &idempotency(&format!("run-{trace_id}"))?,
                self.now().await?,
                trace_id,
            )
            .await?
        {
            EvaluationRunReservation::Created(run) | EvaluationRunReservation::Replayed(run) => {
                Ok(run)
            }
        }
    }
}

fn idempotency(value: &str) -> Result<IdempotencyKey, contracts::http::HttpContractError> {
    IdempotencyKey::parse(value)
}

fn mutation(
    course_id: CourseId,
    expected_revision: Revision,
    actor_id: ActorId,
) -> InternalEvaluationRunMutationRequest {
    InternalEvaluationRunMutationRequest {
        course_id,
        expected_revision,
        actor_id,
    }
}

async fn complete_leased_step(
    fixture: &TestContext,
    run_id: EvaluationRunId,
    lease: &EvaluationStepLease,
    completion: &EvaluationStepCompletion,
    trace_id: &str,
) -> Result<contracts::evaluation::EvaluationRun, EvaluationControlStoreError> {
    fixture
        .store
        .complete_step(
            fixture.course_id,
            run_id,
            lease.step_run_id,
            lease.attempt,
            &lease.worker_id,
            &lease.worker_san_uri,
            &lease.runtime_identity,
            lease.lease_token(),
            completion,
            trace_id,
        )
        .await
}

fn runtime_identity() -> EvaluationRuntimeIdentity {
    EvaluationRuntimeIdentity {
        source_sha256: Sha256Digest::of_bytes(b"source-tree"),
        provider_binding: "kubernetes/oj-runner".to_owned(),
        package_sha256: Sha256Digest::of_bytes(b"problem-package"),
        configuration_sha256: Sha256Digest::of_bytes(b"runtime-config"),
        migration_catalog_sha256: Sha256Digest::of_bytes(b"migration-catalog"),
        runner_image: format!(
            "registry.example/labweaver/evaluation-worker@sha256:{}",
            Sha256Digest::of_bytes(b"runner-image")
        ),
        runtime_artifact_sha256: Sha256Digest::of_bytes(b"runtime-artifact"),
    }
}

fn success_completion(score: u32) -> EvaluationStepCompletion {
    EvaluationStepCompletion {
        state: EvaluationStepRunState::Succeeded,
        awarded_score: Some(score),
        diagnostic_code: None,
        evidence_sha256: Sha256Digest::of_bytes(format!("success-{score}").as_bytes()),
        cleanup_verified: true,
    }
}

fn non_score_success_completion() -> EvaluationStepCompletion {
    EvaluationStepCompletion {
        state: EvaluationStepRunState::Succeeded,
        awarded_score: None,
        diagnostic_code: None,
        evidence_sha256: Sha256Digest::of_bytes(b"success-non-score"),
        cleanup_verified: true,
    }
}

fn failed_completion(cleanup_verified: bool) -> EvaluationStepCompletion {
    EvaluationStepCompletion {
        state: EvaluationStepRunState::Failed,
        awarded_score: None,
        diagnostic_code: Some(DiagnosticCode::registered(
            "LW_EVALUATION_DEPENDENCY_FAILED",
        )),
        evidence_sha256: Sha256Digest::of_bytes(b"failed-step"),
        cleanup_verified,
    }
}

fn step_by_id<'a>(
    run: &'a contracts::evaluation::EvaluationRun,
    step_id: &str,
) -> &'a contracts::evaluation::EvaluationStepRun {
    run.steps
        .iter()
        .find(|step| step.step_id == step_id)
        .expect("step id must exist in test run")
}

struct AttemptRuntimeIdentity {
    provider_binding: String,
    runner_image: String,
    runtime_artifact_sha256: String,
    runtime_identity_sha256: String,
}

async fn attempt_runtime_identity(
    pool: &sqlx::PgPool,
    step_run_id: EvaluationStepRunId,
    attempt: u32,
) -> Result<Option<AttemptRuntimeIdentity>, Box<dyn std::error::Error>> {
    let attempt = i32::try_from(attempt)?;
    let row = sqlx::query(
        "
        SELECT provider_binding, runner_image, runtime_artifact_sha256, runtime_identity_sha256
        FROM evaluation.evaluation_step_attempts
        WHERE step_run_id = $1 AND attempt = $2
        ",
    )
    .bind(step_run_id.as_uuid())
    .bind(attempt)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|row| AttemptRuntimeIdentity {
        provider_binding: row.get("provider_binding"),
        runner_image: row.get("runner_image"),
        runtime_artifact_sha256: row.get("runtime_artifact_sha256"),
        runtime_identity_sha256: row.get("runtime_identity_sha256"),
    }))
}

async fn assert_completion_attempt_fences(
    fixture: &TestContext,
    run_id: EvaluationRunId,
    lease: &EvaluationStepLease,
    trace_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let fenced = fixture
        .store
        .complete_step(
            fixture.course_id,
            run_id,
            lease.step_run_id,
            lease.attempt,
            &lease.worker_id,
            &lease.worker_san_uri,
            &lease.runtime_identity,
            uuid::Uuid::now_v7(),
            &success_completion(lease.max_score),
            trace_id,
        )
        .await
        .expect_err("wrong lease token must fail closed");
    assert!(matches!(fenced, EvaluationControlStoreError::LeaseLost));

    let mut wrong_runtime_identity = lease.runtime_identity.clone();
    wrong_runtime_identity.runtime_artifact_sha256 =
        Sha256Digest::of_bytes(b"wrong-runtime-artifact");
    let identity_mismatch = fixture
        .store
        .complete_step(
            fixture.course_id,
            run_id,
            lease.step_run_id,
            lease.attempt,
            &lease.worker_id,
            &lease.worker_san_uri,
            &wrong_runtime_identity,
            lease.lease_token(),
            &success_completion(lease.max_score),
            trace_id,
        )
        .await
        .expect_err("runtime identity mismatch must fail closed before completion");
    assert!(matches!(
        identity_mismatch,
        EvaluationControlStoreError::IdentityMismatch
    ));

    let attempt_identity =
        attempt_runtime_identity(&fixture.pool, lease.step_run_id, lease.attempt)
            .await?
            .expect("running attempt must store runtime identity fence");
    assert_eq!(
        attempt_identity.provider_binding,
        lease.runtime_identity.provider_binding
    );
    assert_eq!(
        attempt_identity.runner_image,
        lease.runtime_identity.runner_image
    );
    assert_eq!(
        attempt_identity.runtime_artifact_sha256,
        lease.runtime_identity.runtime_artifact_sha256.to_string()
    );
    assert_eq!(
        attempt_identity.runtime_identity_sha256,
        Sha256Digest::of_canonical(&lease.runtime_identity)?.to_string()
    );
    Ok(())
}

fn single_score_spec() -> Result<EvaluationSpec, contracts::evaluation::EvaluationSpecError> {
    EvaluationSpec::from_yaml(
        r#"apiVersion: evaluation.labweaver.io/v1
kind: EvaluationSpec
metadata:
  name: single-score-v1
  version: "1.0.0"
spec:
  submission:
    collector:
      kind: workspace_snapshot
      include: [answer.txt]
      maxBytes: 1024
    llmReadable: [answer.txt]
  steps:
    - role: score
      id: score-answer
      runner:
        kind: file_assertion
        requiredFiles: [answer.txt]
      checker:
        kind: exit_code
        expected: 0
      score:
        max: 7
      failurePolicy: stop
  aggregation:
    kind: deterministic_sum
    maxScore: 7
    gates: []
  review:
    teacherApprovalRequiredForRelease: true
    forceManualWhen: []
"#,
    )
}

fn forked_dag_spec() -> Result<EvaluationSpec, contracts::evaluation::EvaluationSpecError> {
    EvaluationSpec::from_yaml(
        r#"apiVersion: evaluation.labweaver.io/v1
kind: EvaluationSpec
metadata:
  name: forked-dag-v1
  version: "1.0.0"
spec:
  submission:
    collector:
      kind: workspace_snapshot
      include: [answer.txt]
      maxBytes: 1024
    llmReadable: [answer.txt]
  steps:
    - role: gate
      id: preflight
      runner:
        kind: file_assertion
        requiredFiles: [answer.txt]
      checker:
        kind: exit_code
        expected: 0
      failurePolicy: stop
    - role: score
      id: dependent-score
      dependsOn: [preflight]
      runner:
        kind: file_assertion
        requiredFiles: [answer.txt]
      checker:
        kind: exit_code
        expected: 0
      score:
        max: 5
      failurePolicy: stop
    - role: score
      id: independent-score
      runner:
        kind: file_assertion
        requiredFiles: [answer.txt]
      checker:
        kind: exit_code
        expected: 0
      score:
        max: 3
      failurePolicy: continue
  aggregation:
    kind: deterministic_sum
    maxScore: 8
    gates:
      - step: preflight
        requiredStatus: passed
  review:
    teacherApprovalRequiredForRelease: true
    forceManualWhen: []
"#,
    )
}

fn score_continue_spec() -> Result<EvaluationSpec, contracts::evaluation::EvaluationSpecError> {
    EvaluationSpec::from_yaml(
        r#"apiVersion: evaluation.labweaver.io/v1
kind: EvaluationSpec
metadata:
  name: score-continue-v1
  version: "1.0.0"
spec:
  submission:
    collector:
      kind: workspace_snapshot
      include: [answer.txt]
      maxBytes: 1024
    llmReadable: []
  steps:
    - role: score
      id: flaky-score
      runner:
        kind: file_assertion
        requiredFiles: [answer.txt]
      checker:
        kind: exit_code
        expected: 0
      score:
        max: 6
      failurePolicy: continue
    - role: score
      id: dependent-after-flaky
      dependsOn: [flaky-score]
      runner:
        kind: file_assertion
        requiredFiles: [answer.txt]
      checker:
        kind: exit_code
        expected: 0
      score:
        max: 2
      failurePolicy: stop
    - role: score
      id: independent-score
      runner:
        kind: file_assertion
        requiredFiles: [answer.txt]
      checker:
        kind: exit_code
        expected: 0
      score:
        max: 4
      failurePolicy: stop
  aggregation:
    kind: deterministic_sum
    maxScore: 12
    gates: []
  review:
    teacherApprovalRequiredForRelease: true
    forceManualWhen: []
"#,
    )
}

fn advisory_then_score_spec() -> Result<EvaluationSpec, contracts::evaluation::EvaluationSpecError>
{
    EvaluationSpec::from_yaml(
        r#"apiVersion: evaluation.labweaver.io/v1
kind: EvaluationSpec
metadata:
  name: advisory-then-score-v1
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
      id: lint-advisory
      runner:
        kind: llm_review
        include: [answer.txt]
        rubric: evaluator://rubrics/lint.yaml
        outputMode: goal_assessment
      failurePolicy: continue_advisory
    - role: score
      id: deterministic-score
      runner:
        kind: file_assertion
        requiredFiles: [answer.txt]
      checker:
        kind: exit_code
        expected: 0
      score:
        max: 6
      failurePolicy: stop
  aggregation:
    kind: deterministic_sum
    maxScore: 6
    gates: []
  review:
    teacherApprovalRequiredForRelease: true
    forceManualWhen: []
"#,
    )
}

fn oj_spec() -> Result<EvaluationSpec, contracts::evaluation::EvaluationSpecError> {
    EvaluationSpec::from_yaml(include_str!(
        "../../../crates/contracts/tests/fixtures/evaluation/oj/evaluation.yaml"
    ))
}

async fn expire_lease(
    pool: &sqlx::PgPool,
    step_run_id: EvaluationStepRunId,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE evaluation.evaluation_step_attempts \
         SET lease_expires_at=clock_timestamp() - interval '1 second' \
         WHERE step_run_id=$1",
    )
    .bind(step_run_id.as_uuid())
    .execute(pool)
    .await?;
    Ok(())
}

async fn attempt_cleanup_verified(
    pool: &sqlx::PgPool,
    step_run_id: EvaluationStepRunId,
    attempt: u32,
) -> Result<bool, Box<dyn std::error::Error>> {
    Ok(sqlx::query_scalar(
        "SELECT cleanup_verified FROM evaluation.evaluation_step_attempts \
         WHERE step_run_id=$1 AND attempt=$2",
    )
    .bind(step_run_id.as_uuid())
    .bind(i32::try_from(attempt)?)
    .fetch_one(pool)
    .await?)
}

async fn step_cleanup_verified(
    pool: &sqlx::PgPool,
    step_run_id: EvaluationStepRunId,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT cleanup_verified FROM evaluation.evaluation_step_runs WHERE step_run_id=$1",
    )
    .bind(step_run_id.as_uuid())
    .fetch_one(pool)
    .await
}

async fn operator_event_count(pool: &sqlx::PgPool, actor_id: ActorId) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*) FROM evaluation.outbox_events \
         WHERE payload #>> '{data,operatorActorId}' = $1",
    )
    .bind(actor_id.to_string())
    .fetch_one(pool)
    .await
}

async fn duplicate_step_revision_events(pool: &sqlx::PgPool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT COALESCE(sum(extra),0)::bigint FROM ( \
             SELECT count(*) - 1 AS extra \
             FROM evaluation.outbox_events \
             WHERE subject='labweaver.evaluation.step_run.state_changed.v1' \
             GROUP BY payload #>> '{data,stepRunId}', payload #>> '{data,revision}' \
             HAVING count(*) > 1 \
         ) duplicates",
    )
    .fetch_one(pool)
    .await
}

async fn count(pool: &sqlx::PgPool, table: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
        .fetch_one(pool)
        .await
}
