//! Real `PostgreSQL` proof for `EvaluationRelease`, `EvaluationRun` and `StepRun` fencing.
#![allow(
    clippy::expect_used,
    reason = "the integration fixture uses fixed valid contract identities"
)]

use std::time::Duration;

use contracts::{
    ActorId, ApprovalId, CandidateId, CourseId, DiagnosticCode, EnvironmentId, EvaluationStepRunId,
    FrozenSubmissionId, Revision, Sha256Digest, UtcTimestamp,
    evaluation::{
        EvaluationRunIdentity, EvaluationRunState, EvaluationRuntimeIdentity, EvaluationSpec,
        EvaluationStepCompletion, EvaluationStepRunState,
    },
    http::{
        IdempotencyKey, InternalCreateEvaluationRunRequest, InternalEvaluationRunMutationRequest,
        InternalPublishEvaluationReleaseRequest,
    },
};
use evaluation_service::{
    EvaluationControlStoreError, EvaluationReleaseReservation, EvaluationRunReservation,
    PgEvaluationControlStore,
};
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

    let fenced = fixture
        .store
        .complete_step(
            fixture.course_id,
            run.id,
            first.step_run_id,
            first.attempt,
            "worker-a",
            uuid::Uuid::now_v7(),
            &success_completion(7),
            fixture.now().await?,
            "trace-retry-1",
        )
        .await
        .expect_err("wrong lease token must fail closed");
    assert!(matches!(fenced, EvaluationControlStoreError::LeaseLost));

    let failed = fixture
        .store
        .complete_step(
            fixture.course_id,
            run.id,
            first.step_run_id,
            first.attempt,
            "worker-a",
            first.lease_token(),
            &failed_completion(true),
            fixture.now().await?,
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

    let succeeded = fixture
        .store
        .complete_step(
            fixture.course_id,
            run.id,
            second.step_run_id,
            second.attempt,
            "worker-b",
            second.lease_token(),
            &success_completion(7),
            fixture.now().await?,
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

fn runtime_identity() -> EvaluationRuntimeIdentity {
    EvaluationRuntimeIdentity {
        source_sha256: Sha256Digest::of_bytes(b"source-tree"),
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
    llmReadable: []
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
      failurePolicy: continue
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

async fn count(pool: &sqlx::PgPool, table: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
        .fetch_one(pool)
        .await
}
