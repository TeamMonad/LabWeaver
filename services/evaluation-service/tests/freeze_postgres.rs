//! Real `PostgreSQL` proof for freeze idempotency, failed-attempt retention, and Outbox atomicity.
#![allow(
    clippy::expect_used,
    reason = "the integration fixture uses fixed valid contract identities"
)]

use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use artifact_store::{ImmutableObjectStore, ObjectStoreError, PresignedUpload, VerifiedObject};
use async_trait::async_trait;
use contracts::authoring::RuntimeKind;
use contracts::submission::{FrozenEnvironmentIdentity, SubmissionManifest};
use contracts::{
    ActorId, AgentRunId, ArtifactId, ArtifactRef, BuildRequestId, CourseId, PolicyId, ReleaseId,
    RetentionClass, RetentionDisposition, RetentionSnapshot, Revision, Sha256Digest, UtcTimestamp,
    parse_strict_json,
};
use evaluation_service::{
    FreezeRequest, FreezeService, FreezeServiceError, PgFreezeStore, PvcSnapshotSource,
    SnapshotCollector,
};
use sqlx::postgres::PgPoolOptions;
use tempfile::tempdir;
use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;

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
    async fn presign_upload(
        &self,
        _key: &str,
        _size_bytes: u64,
        _sha256: Sha256Digest,
        _media_type: &str,
        _now: UtcTimestamp,
    ) -> Result<PresignedUpload, ObjectStoreError> {
        Err(ObjectStoreError::SigningFailed)
    }

    async fn read_verified(
        &self,
        _key: &str,
        _version: &str,
        _expected_size: u64,
        _expected_sha256: Sha256Digest,
        _media_type: &str,
    ) -> Result<VerifiedObject, ObjectStoreError> {
        Err(ObjectStoreError::ObjectUnavailable)
    }

    async fn freeze_current(
        &self,
        _key: &str,
        _expected_size: u64,
        _expected_sha256: Sha256Digest,
        _media_type: &str,
    ) -> Result<VerifiedObject, ObjectStoreError> {
        Err(ObjectStoreError::ObjectUnavailable)
    }

    async fn put_governance_locked(
        &self,
        _key: &str,
        bytes: &[u8],
        sha256: Sha256Digest,
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
                sha256,
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
    let payload: serde_json::Value =
        sqlx::query_scalar("SELECT payload FROM evaluation.outbox_events")
            .fetch_one(&fixture.pool)
            .await?;
    assert_eq!(
        payload["data"]["submission"]["object"]["objectVersion"],
        first.object.object_version
    );
    assert_eq!(
        payload["data"]["submission"]["object"]["sha256"],
        first.object.sha256.to_string()
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
        let migrations = format!(
            "CREATE SCHEMA evaluation; SET search_path TO evaluation;\n{}",
            include_str!("../../../migrations/evaluation/0001_sprint2_baseline.sql")
        );
        sqlx::raw_sql(&migrations).execute(&pool).await?;
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
            course_id,
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
                runtime_artifact_sha256: Sha256Digest::of_bytes(b"container-image"),
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
