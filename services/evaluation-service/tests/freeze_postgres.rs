//! Real `PostgreSQL` proof for freeze idempotency, failed-attempt retention, and Outbox atomicity.
#![allow(
    clippy::expect_used,
    reason = "the integration fixture uses fixed valid contract identities"
)]

mod support;

use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use artifact_store::{ImmutableObjectStore, ObjectStoreError, PresignedUpload, VerifiedObject};
use async_trait::async_trait;
use contracts::authoring::RuntimeKind;
use contracts::submission::{FrozenEnvironmentIdentity, SubmissionManifest};
use contracts::{
    ActorId, AgentRunId, ArtifactId, ArtifactRef, BuildRequestId, CourseId, PolicyId, ProjectId,
    ReleaseId, RetentionClass, RetentionDisposition, RetentionSnapshot, Revision, UtcTimestamp,
    parse_strict_json,
};
use evaluation_service::{
    FreezeRequest, FreezeService, FreezeServiceError, PgFreezeCommandStore, PgFreezeStore,
    PvcSnapshotSource, SnapshotCollector, SubmissionFreezeCommand,
};
use persistence_sqlx::Sha256Digest;
use sqlx::postgres::PgPoolOptions;
use tempfile::tempdir;
use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;

#[tokio::test]
async fn public_acceptance_is_atomic_idempotent_and_enqueues_one_command()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestContext::start(0).await?;
    let store = PgFreezeCommandStore::new(fixture.pool.clone());
    let command = SubmissionFreezeCommand {
        frozen_submission_id: contracts::FrozenSubmissionId::new(),
        operation_id: contracts::OperationId::new(),
        project_id: fixture.request.project_id,
        course_id: fixture.request.course_id,
        environment_id: fixture.request.environment.environment_id,
        actor_id: fixture.request.actor_id,
        environment_revision: fixture.request.environment.environment_revision,
        manifest_revision: fixture.request.manifest_revision,
        manifest: fixture.request.manifest.clone(),
        idempotency_key: "browser-freeze-1".to_owned(),
        trace_id: "browser-freeze-trace-1".to_owned(),
        requested_at: PgFreezeStore::new(fixture.pool.clone())
            .authority_now()
            .await?,
    };
    let first = store.accept(&command).await?;
    let mut retry = command.clone();
    retry.frozen_submission_id = contracts::FrozenSubmissionId::new();
    retry.operation_id = contracts::OperationId::new();
    retry.trace_id = "browser-freeze-trace-2".to_owned();
    let replay = store.accept(&retry).await?;
    assert!(!first.replay);
    assert!(replay.replay);
    assert_eq!(first.frozen_submission_id, replay.frozen_submission_id);
    assert_eq!(first.accepted.operation_id, replay.accepted.operation_id);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM evaluation.submission_freeze_commands")
            .fetch_one(&fixture.pool)
            .await?,
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM evaluation.outbox_events")
            .fetch_one(&fixture.pool)
            .await?,
        1
    );
    let claimed = store.claim_next().await?.expect("queued command");
    assert_eq!(claimed.frozen_submission_id, command.frozen_submission_id);
    assert!(store.claim_next().await?.is_none());
    assert_eq!(store.running(32).await?, vec![command.clone()]);
    store
        .mark_failed_pending_cleanup(
            first.frozen_submission_id,
            "LW_COLLECT_SSH_CREDENTIAL_INVALID",
        )
        .await?;
    assert!(store.running(32).await?.is_empty());
    assert_eq!(store.cleanup_pending(32).await?, vec![command.clone()]);
    assert_eq!(
        sqlx::query_as::<_, (String, bool)>(
            "SELECT diagnostic_code,cleanup_verified \
             FROM evaluation.submission_freeze_commands WHERE frozen_submission_id=$1",
        )
        .bind(first.frozen_submission_id.as_uuid())
        .fetch_one(&fixture.pool)
        .await?,
        ("LW_COLLECT_SSH_CREDENTIAL_INVALID".to_owned(), false)
    );
    store
        .mark_cleanup_verified(first.frozen_submission_id)
        .await?;
    assert!(store.cleanup_pending(32).await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn project_idempotency_identity_supports_independent_work_and_rejects_content_dedupe()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestContext::start(0).await?;
    let store = PgFreezeStore::new(fixture.pool.clone());
    let now = store.authority_now().await?;
    let request_sha256 = Sha256Digest::of_bytes(b"independent-request");
    let source_identity_sha256 = Sha256Digest::of_bytes(b"independent-source");
    let first_id = contracts::FrozenSubmissionId::new();
    let first = store
        .begin(
            first_id,
            fixture.request.project_id,
            None,
            fixture.request.environment.environment_id,
            "independent-freeze-1",
            request_sha256,
            source_identity_sha256,
            "independent-worker",
            std::time::Duration::from_mins(1),
        )
        .await?;
    assert!(matches!(
        first,
        evaluation_service::BeginFreeze::Acquired(_)
    ));

    let duplicate_request = sqlx::query(
        "INSERT INTO evaluation.submission_freeze_requests \
         (frozen_submission_id,project_id,course_id,environment_id,idempotency_key,request_sha256,source_identity_sha256,state,current_attempt) \
         VALUES ($1,$2,NULL,$3,$4,$5,$6,'active',1)",
    )
    .bind(contracts::FrozenSubmissionId::new().as_uuid())
    .bind(fixture.request.project_id.as_uuid())
    .bind(fixture.request.environment.environment_id.as_uuid())
    .bind("independent-freeze-1")
    .bind(request_sha256.to_string())
    .bind(source_identity_sha256.to_string())
    .execute(&fixture.pool)
    .await;
    assert!(matches!(
        duplicate_request,
        Err(sqlx::Error::Database(error)) if error.code().as_deref() == Some("23505")
    ));

    let content_sha256 = Sha256Digest::of_bytes(b"same-content").to_string();
    for (submission_id, idempotency_key, object_key) in [
        (
            contracts::FrozenSubmissionId::new(),
            "independent-submission-1",
            "frozen/independent-1",
        ),
        (
            contracts::FrozenSubmissionId::new(),
            "independent-submission-2",
            "frozen/independent-2",
        ),
    ] {
        sqlx::query(
            "INSERT INTO evaluation.frozen_submissions \
             (frozen_submission_id,project_id,course_id,environment_id,manifest_sha256,content_sha256,schema_version,tool_version,contract,frozen_at, \
              idempotency_key,source_identity_sha256,object_key,object_version) \
             VALUES ($1,$2,NULL,$3,$4,$5,$6,$7,'{}'::jsonb,$8,$9,$10,$11,$12)",
        )
        .bind(submission_id.as_uuid())
        .bind(fixture.request.project_id.as_uuid())
        .bind(fixture.request.environment.environment_id.as_uuid())
        .bind(Sha256Digest::of_bytes(b"same-manifest").to_string())
        .bind(&content_sha256)
        .bind("evaluation.labweaver.io/frozen-submission/v1")
        .bind("test")
        .bind(now.get())
        .bind(idempotency_key)
        .bind(source_identity_sha256.to_string())
        .bind(object_key)
        .bind("version")
        .execute(&fixture.pool)
        .await?;
    }
    Ok(())
}

#[derive(Debug)]
struct LockedStore {
    failures_remaining: AtomicUsize,
    puts: AtomicUsize,
}

impl LockedStore {
    fn new(failures: usize) -> Self {
        Self {
            failures_remaining: AtomicUsize::new(failures),
            puts: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl ImmutableObjectStore for LockedStore {
    fn binding(&self) -> &'static str {
        "minio-submissions-v1"
    }

    async fn presign_upload(
        &self,
        _key: &str,
        _size_bytes: u64,
        _media_type: &str,
        _now: UtcTimestamp,
    ) -> Result<PresignedUpload, ObjectStoreError> {
        Err(ObjectStoreError::SigningFailed)
    }

    async fn read_verified(
        &self,
        _key: &str,
        _expected: &ArtifactRef,
    ) -> Result<VerifiedObject, ObjectStoreError> {
        Err(ObjectStoreError::ObjectUnavailable)
    }

    async fn freeze_current(
        &self,
        _key: &str,
        _expected_size: u64,
        _media_type: &str,
    ) -> Result<VerifiedObject, ObjectStoreError> {
        Err(ObjectStoreError::ObjectUnavailable)
    }

    async fn put_governance_locked(
        &self,
        _key: &str,
        bytes: &[u8],
        media_type: &str,
        _now: UtcTimestamp,
        _retain_until: UtcTimestamp,
    ) -> Result<VerifiedObject, ObjectStoreError> {
        let put = self.puts.fetch_add(1, Ordering::AcqRel) + 1;
        if self
            .failures_remaining
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
        {
            return Err(ObjectStoreError::UploadFailed);
        }
        Ok(VerifiedObject {
            reference: ArtifactRef {
                artifact_id: ArtifactId::new(),
                store_binding: "minio-submissions-v1".to_owned(),
                object_version: format!("locked-version-{put}"),
                size_bytes: u64::try_from(bytes.len())
                    .map_err(|_| ObjectStoreError::ObjectTooLarge)?,
                media_type: media_type.to_owned(),
            },
            bytes: bytes.to_vec(),
        })
    }

    async fn delete_orphan(&self, _key: &str, _version: &str) -> Result<(), ObjectStoreError> {
        Err(ObjectStoreError::ObjectLockRequired)
    }
}

#[tokio::test]
async fn repeated_request_replays_one_database_object_and_event_identity()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestContext::start(0).await?;
    let first = fixture
        .service
        .freeze(&fixture.request, &fixture.source)
        .await?;
    let replay = fixture
        .service
        .freeze(&fixture.request, &fixture.source)
        .await?;
    assert_eq!(first, replay);
    assert_eq!(fixture.object_store.puts.load(Ordering::Acquire), 1);
    assert_eq!(
        count(&fixture.pool, "evaluation.frozen_submissions").await?,
        1
    );
    assert_eq!(count(&fixture.pool, "evaluation.outbox_events").await?, 1);
    let persisted_manifest_sha256: String = sqlx::query_scalar(
        "SELECT manifest_sha256 FROM evaluation.frozen_submissions WHERE frozen_submission_id=$1",
    )
    .bind(first.id.as_uuid())
    .fetch_one(&fixture.pool)
    .await?;
    assert_eq!(
        persisted_manifest_sha256,
        Sha256Digest::of_canonical(&fixture.request.manifest)?.to_string()
    );
    let persisted_content_sha256: String = sqlx::query_scalar(
        "SELECT content_sha256 FROM evaluation.frozen_submissions WHERE frozen_submission_id=$1",
    )
    .bind(first.id.as_uuid())
    .fetch_one(&fixture.pool)
    .await?;
    assert_eq!(persisted_content_sha256, first.content_sha256);
    let payload: serde_json::Value =
        sqlx::query_scalar("SELECT payload FROM evaluation.outbox_events")
            .fetch_one(&fixture.pool)
            .await?;
    assert_eq!(
        payload["data"]["submission"]["object"]["objectVersion"],
        first.object.object_version
    );
    let mut conflicting = fixture.request.clone();
    conflicting.actor_id = ActorId::new();
    assert!(matches!(
        fixture.service.freeze(&conflicting, &fixture.source).await,
        Err(FreezeServiceError::IdempotencyConflict)
    ));
    Ok(())
}

#[tokio::test]
async fn upload_failure_is_not_publishable_and_retry_retains_both_attempts()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = TestContext::start(1).await?;
    assert!(matches!(
        fixture
            .service
            .freeze(&fixture.request, &fixture.source)
            .await,
        Err(FreezeServiceError::ObjectStore(
            ObjectStoreError::UploadFailed
        ))
    ));
    assert_eq!(
        count(&fixture.pool, "evaluation.frozen_submissions").await?,
        0
    );
    assert_eq!(count(&fixture.pool, "evaluation.outbox_events").await?, 0);
    let frozen = fixture
        .service
        .freeze(&fixture.request, &fixture.source)
        .await?;
    assert_eq!(frozen.attempt, 2);
    let attempts: Vec<(i32, String, bool)> = sqlx::query_as(
        "SELECT attempt,state,cleanup_verified FROM evaluation.submission_freeze_attempts ORDER BY attempt",
    )
    .fetch_all(&fixture.pool)
    .await?;
    assert_eq!(
        attempts,
        vec![
            (1, "failed".to_owned(), false),
            (2, "completed".to_owned(), true)
        ]
    );
    assert_eq!(
        count(&fixture.pool, "evaluation.frozen_submissions").await?,
        1
    );
    assert_eq!(count(&fixture.pool, "evaluation.outbox_events").await?, 1);
    Ok(())
}

struct TestContext {
    _container: testcontainers::ContainerAsync<Postgres>,
    pool: sqlx::PgPool,
    object_store: Arc<LockedStore>,
    service: FreezeService,
    request: FreezeRequest,
    source: PvcSnapshotSource,
    _workspace: tempfile::TempDir,
}

impl TestContext {
    async fn start(failures: usize) -> Result<Self, Box<dyn std::error::Error>> {
        let container = Postgres::default().with_tag("17.5-alpine").start().await?;
        let database_url = format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            container.get_host_port_ipv4(5432).await?
        );
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect(&database_url)
            .await?;
        support::apply_evaluation_migrations(&pool).await?;
        let store = PgFreezeStore::new(pool.clone());
        let now = store.authority_now().await?;
        let object_store = Arc::new(LockedStore::new(failures));
        let service = FreezeService::new(
            store,
            object_store.clone(),
            SnapshotCollector::default(),
            "frozen-submissions",
            "collector-test-worker",
        )?;
        let workspace = tempdir()?;
        fs::create_dir_all(workspace.path().join("src"))?;
        fs::write(workspace.path().join("src/main.rs"), b"fn main() {}\n")?;
        let source_identity = Sha256Digest::of_bytes(b"pvc:environment:revision:1");
        let source = PvcSnapshotSource::open(workspace.path(), source_identity)?;
        let course_id = CourseId::new();
        let environment_id = contracts::EnvironmentId::new();
        let request = FreezeRequest {
            frozen_submission_id: contracts::FrozenSubmissionId::new(),
            project_id: ProjectId::new(),
            course_id: Some(course_id),
            actor_id: ActorId::new(),
            agent_run_id: AgentRunId::new(),
            manifest_revision: Revision::new(1)?,
            manifest: manifest()?,
            environment: FrozenEnvironmentIdentity {
                environment_id,
                environment_revision: Revision::new(3)?,
                release_id: ReleaseId::new(),
                release_version: 7,
                runtime_kind: RuntimeKind::Container,
                build_request_id: Some(BuildRequestId::new()),
            },
            retention: RetentionSnapshot {
                policy_id: PolicyId::new(),
                policy_revision: Revision::new(2)?,
                class: RetentionClass::StudentSubmission,
                retain_until: UtcTimestamp::from_utc(now.get() + time::Duration::days(1))?,
                disposition: RetentionDisposition::Delete,
            },
            idempotency_key: format!("freeze:{environment_id}:1"),
            trace_id: format!("collector:{course_id}:{environment_id}"),
        };
        Ok(Self {
            _container: container,
            pool,
            object_store,
            service,
            request,
            source,
            _workspace: workspace,
        })
    }
}

fn manifest() -> Result<SubmissionManifest, Box<dyn std::error::Error>> {
    Ok(parse_strict_json(
        br#"{
          "apiVersion":"evaluation.labweaver.io/v1",
          "kind":"SubmissionManifest",
          "name":"workspace",
          "source":"workspace",
          "include":[{"kind":"directoryTree","path":"src"}],
          "exclude":[],
          "required":[{"kind":"exactFile","path":"src/main.rs"}],
          "llmReadable":[],
          "maxTotalBytes":1048576,
          "maxFiles":100,
          "followSymlinks":false
        }"#,
    )?)
}

async fn count(pool: &sqlx::PgPool, table: &str) -> Result<i64, sqlx::Error> {
    let query = format!("SELECT count(*) FROM {table}");
    sqlx::query_scalar(&query).fetch_one(pool).await
}
