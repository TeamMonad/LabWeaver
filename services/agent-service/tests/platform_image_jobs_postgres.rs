//! Durable import behavior against `PostgreSQL` and the real Agent HTTP routes.
use std::{
    collections::BTreeSet,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use agent_service::{
    api::{AgentApiState, CONTROL_PERMISSION, router},
    build_store::PgBuildStore,
    generated_artifacts::GeneratedArtifactStore,
    llm_review::LlmReviewStore,
    platform_image_jobs::{
        PlatformImageImportJobError, PlatformImageImportJobStore, PlatformImageImportWorker,
    },
    platform_images::{
        PgPlatformImageCatalog, PlatformImageRegistry, PlatformImageStoreError,
        RegisterPlatformImage,
    },
    run_store::PostgresAgentRunStore,
};
use artifact_store::{
    ImmutableObjectStore, ObjectStoreError, PresignedUpload, VerifiedObject, VerifiedObjectFile,
};
use async_trait::async_trait;
use contracts::http::{
    InternalPlatformImageImportEnqueueRequest, InternalPlatformImageImportJobStatus,
    InternalPlatformImageImportRequest, PlatformImageImportJobState, PlatformImageKind,
};
use contracts::{ActorId, ArtifactId, ArtifactRef, UploadSessionId, UtcTimestamp};
use sqlx::{PgPool, Row, postgres::PgPoolOptions};
use testcontainers::{ContainerAsync, ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;

mod support;
use support::{FakeRegistry, apply_agent_migrations};

async fn fixture() -> Result<(PgPool, ContainerAsync<Postgres>), Box<dyn std::error::Error>> {
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            container.get_host_port_ipv4(5432).await?
        ))
        .await?;
    apply_agent_migrations(&pool).await?;
    Ok((pool, container))
}

fn now() -> Result<UtcTimestamp, Box<dyn std::error::Error>> {
    let value = time::OffsetDateTime::now_utc();
    Ok(UtcTimestamp::from_utc(value.replace_nanosecond(
        value.nanosecond() / 1_000_000 * 1_000_000,
    )?)?)
}

fn request(binding: &str) -> InternalPlatformImageImportEnqueueRequest {
    InternalPlatformImageImportEnqueueRequest {
        upload_id: UploadSessionId::new(),
        request: InternalPlatformImageImportRequest {
            kind: PlatformImageKind::Container,
            binding: binding.to_owned(),
            target_reference: format!("registry.test/platform/{binding}:v1"),
            archive: ArtifactRef {
                artifact_id: ArtifactId::new(),
                store_binding: "test-object-store".to_owned(),
                object_version: "version-1".to_owned(),
                size_bytes: 5_000_000_000,
                media_type: contracts::http::PLATFORM_IMAGE_ARCHIVE_MEDIA_TYPE.to_owned(),
            },
            archive_object_key: "problem-packages/platform-image-uploads/archive.tar".to_owned(),
            disk_format: None,
            disk_path: None,
            capacity_bytes: None,
            trust_revision: 1,
            actor_id: ActorId::new(),
            reason: "administrator reviewed the import".to_owned(),
        },
    }
}

fn registration(
    request: &InternalPlatformImageImportEnqueueRequest,
) -> Result<RegisterPlatformImage, Box<dyn std::error::Error>> {
    Ok(RegisterPlatformImage {
        kind: request.request.kind,
        binding: request.request.binding.clone(),
        source_reference: request.request.target_reference.clone(),
        resolved_digest: format!("sha256:{}", "a".repeat(64)),
        media_type: "application/vnd.oci.image.manifest.v1+json".to_owned(),
        size_bytes: 32,
        capacity_bytes: None,
        disk_sha256: None,
        format: None,
        trust_revision: 1,
        actor_id: request.request.actor_id,
        reason: request.request.reason.clone(),
        now: now()?,
    })
}

async fn expire(pool: &PgPool, upload_id: UploadSessionId) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE agent.platform_image_import_jobs SET lease_expires_at=clock_timestamp()-interval '1 second' WHERE upload_id=$1")
        .bind(upload_id.as_uuid()).execute(pool).await?;
    Ok(())
}

#[tokio::test]
async fn immutable_enqueue_expired_lease_and_catalog_commit_survive_repository_restart()
-> Result<(), Box<dyn std::error::Error>> {
    let (pool, _postgres) = fixture().await?;
    let jobs = PlatformImageImportJobStore::new(pool.clone());
    let request = request("restart-import");
    let queued = jobs.enqueue(&request, now()?).await?;
    assert_eq!(queued.state, PlatformImageImportJobState::Queued);
    assert_eq!(jobs.enqueue(&request, now()?).await?, queued);
    let mut conflict = request.clone();
    conflict.request.actor_id = ActorId::new();
    assert!(matches!(
        jobs.enqueue(&conflict, now()?).await,
        Err(PlatformImageImportJobError::Conflict)
    ));
    let first = jobs
        .claim("first", Duration::from_secs(30), now()?)
        .await?
        .ok_or("job not claimed")?;
    let running = jobs.status(request.upload_id).await?;
    jobs.renew(
        first.upload_id,
        first.lease_token,
        Duration::from_secs(30),
        now()?,
    )
    .await?;
    assert_eq!(
        jobs.status(request.upload_id).await?.revision,
        running.revision
    );
    expire(&pool, request.upload_id).await?;
    assert!(matches!(
        jobs.renew(
            first.upload_id,
            first.lease_token,
            Duration::from_secs(30),
            now()?
        )
        .await,
        Err(PlatformImageImportJobError::Conflict)
    ));
    assert!(matches!(
        jobs.fail(first.upload_id, first.lease_token, "STALE", now()?)
            .await,
        Err(PlatformImageImportJobError::Conflict)
    ));
    let catalog = PgPlatformImageCatalog::new(pool.clone());
    assert!(matches!(
        catalog
            .register_import_job(first.upload_id, first.lease_token, &registration(&request)?)
            .await,
        Err(PlatformImageStoreError::Conflict)
    ));
    assert!(catalog.list(None).await?.is_empty());
    let restarted = PlatformImageImportJobStore::new(pool.clone());
    let current = restarted
        .claim("second", Duration::from_secs(30), now()?)
        .await?
        .ok_or("expired job not recovered")?;
    assert_ne!(current.lease_token, first.lease_token);
    assert_eq!(current.request, request.request);
    let entry = catalog
        .register_import_job(
            current.upload_id,
            current.lease_token,
            &registration(&request)?,
        )
        .await?;
    let succeeded = restarted.status(request.upload_id).await?;
    assert_eq!(succeeded.state, PlatformImageImportJobState::Succeeded);
    assert_eq!(succeeded.catalog_id, Some(entry.catalog_id));
    assert_eq!(restarted.enqueue(&request, now()?).await?, succeeded);
    assert_eq!(
        catalog
            .register_import_job(
                current.upload_id,
                current.lease_token,
                &registration(&request)?
            )
            .await?,
        entry
    );
    assert_eq!(catalog.list(None).await?.len(), 1);
    Ok(())
}

#[tokio::test]
async fn queued_running_and_expired_cancellation_never_publish_a_catalog_entry()
-> Result<(), Box<dyn std::error::Error>> {
    let (pool, _postgres) = fixture().await?;
    let jobs = PlatformImageImportJobStore::new(pool.clone());
    let catalog = PgPlatformImageCatalog::new(pool.clone());
    let queued = request("queued-cancel");
    jobs.enqueue(&queued, now()?).await?;
    let cancelled = jobs.cancel(queued.upload_id, now()?).await?;
    assert_eq!(cancelled.state, PlatformImageImportJobState::Cancelled);
    assert_eq!(jobs.cancel(queued.upload_id, now()?).await?, cancelled);
    assert!(
        jobs.claim("worker", Duration::from_secs(30), now()?)
            .await?
            .is_none()
    );
    let running = request("running-cancel");
    jobs.enqueue(&running, now()?).await?;
    let claim = jobs
        .claim("worker", Duration::from_secs(30), now()?)
        .await?
        .ok_or("job not claimed")?;
    assert_eq!(
        jobs.cancel(running.upload_id, now()?).await?.state,
        PlatformImageImportJobState::Running
    );
    assert!(
        jobs.cancellation_requested(claim.upload_id, claim.lease_token)
            .await?
    );
    assert!(matches!(
        catalog
            .register_import_job(claim.upload_id, claim.lease_token, &registration(&running)?)
            .await,
        Err(PlatformImageStoreError::Conflict)
    ));
    expire(&pool, running.upload_id).await?;
    assert!(
        PlatformImageImportJobStore::new(pool.clone())
            .claim("restarted", Duration::from_secs(30), now()?)
            .await?
            .is_none()
    );
    assert_eq!(
        jobs.status(running.upload_id).await?.state,
        PlatformImageImportJobState::Cancelled
    );
    assert!(catalog.list(None).await?.is_empty());
    Ok(())
}

#[tokio::test]
async fn malformed_expired_job_clears_the_lease_and_does_not_break_the_worker_queue()
-> Result<(), Box<dyn std::error::Error>> {
    let (pool, _postgres) = fixture().await?;
    let jobs = PlatformImageImportJobStore::new(pool.clone());
    let request = request("malformed");
    jobs.enqueue(&request, now()?).await?;
    jobs.claim("first", Duration::from_secs(30), now()?)
        .await?
        .ok_or("job not claimed")?;
    expire(&pool, request.upload_id).await?;
    sqlx::query(
        "UPDATE agent.platform_image_import_jobs SET request_json='{}'::jsonb WHERE upload_id=$1",
    )
    .bind(request.upload_id.as_uuid())
    .execute(&pool)
    .await?;
    assert!(
        jobs.claim("restart", Duration::from_secs(30), now()?)
            .await?
            .is_none()
    );
    assert_eq!(
        jobs.status(request.upload_id).await?.state,
        PlatformImageImportJobState::Failed
    );
    let row = sqlx::query("SELECT lease_token,lease_expires_at FROM agent.platform_image_import_jobs WHERE upload_id=$1").bind(request.upload_id.as_uuid()).fetch_one(&pool).await?;
    assert!(
        row.try_get::<Option<uuid::Uuid>, _>("lease_token")?
            .is_none()
    );
    assert!(
        row.try_get::<Option<time::OffsetDateTime>, _>("lease_expires_at")?
            .is_none()
    );
    Ok(())
}

struct BlockingObjects {
    started: tokio::sync::Notify,
    cancelled: Arc<AtomicUsize>,
}

struct ReadGuard(Arc<AtomicUsize>);
impl Drop for ReadGuard {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl ImmutableObjectStore for BlockingObjects {
    fn binding(&self) -> &'static str {
        "test-object-store"
    }
    async fn presign_upload(
        &self,
        _: &str,
        _: u64,
        _: &str,
        _: UtcTimestamp,
    ) -> Result<PresignedUpload, ObjectStoreError> {
        Err(ObjectStoreError::SigningFailed)
    }
    async fn read_verified(
        &self,
        _: &str,
        _: &ArtifactRef,
    ) -> Result<VerifiedObject, ObjectStoreError> {
        Err(ObjectStoreError::ObjectUnavailable)
    }
    async fn read_verified_file(
        &self,
        _: &str,
        _: &ArtifactRef,
    ) -> Result<VerifiedObjectFile, ObjectStoreError> {
        let _guard = ReadGuard(Arc::clone(&self.cancelled));
        self.started.notify_one();
        std::future::pending().await
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

#[tokio::test]
async fn lost_lease_cancels_the_archive_read_before_a_restarted_worker_recovers_it()
-> Result<(), Box<dyn std::error::Error>> {
    let (pool, _postgres) = fixture().await?;
    let objects = Arc::new(BlockingObjects {
        started: tokio::sync::Notify::new(),
        cancelled: Arc::new(AtomicUsize::new(0)),
    });
    let (_registry, base) = FakeRegistry::spawn().await?;
    let registry = PlatformImageRegistry::for_test(
        reqwest::Url::parse(&base)?,
        reqwest::Client::new(),
        agent_service::oci_registry::RegistryCredentials {
            username: "test".to_owned(),
            password: "test".to_owned(),
        },
    );
    let jobs = PlatformImageImportJobStore::new(pool.clone());
    let request = request("lease-lost-read");
    jobs.enqueue(&request, now()?).await?;
    let mut worker = PlatformImageImportWorker {
        jobs: jobs.clone(),
        catalog: PgPlatformImageCatalog::new(pool.clone()),
        registry: Some(registry),
        objects: objects.clone(),
        worker_id: "first".to_owned(),
        lease_duration: Duration::from_secs(3),
        poll_interval: Duration::from_mins(1),
    };
    let first = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(worker.clone().run()));
    tokio::time::timeout(Duration::from_secs(4), objects.started.notified()).await?;
    expire(&pool, request.upload_id).await?;
    tokio::time::timeout(Duration::from_secs(4), async {
        while objects.cancelled.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    assert_eq!(objects.cancelled.load(Ordering::SeqCst), 1);
    assert_eq!(
        jobs.status(request.upload_id).await?.state,
        PlatformImageImportJobState::Running
    );
    drop(first);
    worker.worker_id = "restarted".to_owned();
    worker.poll_interval = Duration::from_millis(10);
    let _restarted = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(worker.run()));
    tokio::time::timeout(Duration::from_secs(4), objects.started.notified()).await?;
    assert_eq!(
        jobs.enqueue(&request, now()?).await?.upload_id,
        request.upload_id
    );
    jobs.cancel(request.upload_id, now()?).await?;
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            if jobs.status(request.upload_id).await?.state == PlatformImageImportJobState::Cancelled
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Ok::<_, PlatformImageImportJobError>(())
    })
    .await??;
    assert_eq!(objects.cancelled.load(Ordering::SeqCst), 2);
    assert!(
        PgPlatformImageCatalog::new(pool)
            .list(None)
            .await?
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one HTTP job verifies the immediate acknowledgement and cooperative cancellation of its blocked read"
)]
async fn http_enqueue_returns_before_the_archive_read_and_running_cancel_unwinds_it()
-> Result<(), Box<dyn std::error::Error>> {
    let (pool, _postgres) = fixture().await?;
    let objects = Arc::new(BlockingObjects {
        started: tokio::sync::Notify::new(),
        cancelled: Arc::new(AtomicUsize::new(0)),
    });
    let (_registry, base) = FakeRegistry::spawn().await?;
    let registry = PlatformImageRegistry::for_test(
        reqwest::Url::parse(&base)?,
        reqwest::Client::new(),
        agent_service::oci_registry::RegistryCredentials {
            username: "test".to_owned(),
            password: "test".to_owned(),
        },
    );
    let jobs = PlatformImageImportJobStore::new(pool.clone());
    let state = Arc::new(AgentApiState {
        store: PostgresAgentRunStore::new(pool.clone()),
        build_store: PgBuildStore::new(pool.clone()),
        generated_artifacts: GeneratedArtifactStore::new(pool.clone()),
        llm_reviews: LlmReviewStore::new(pool.clone()),
        platform_images: PgPlatformImageCatalog::new(pool.clone()),
        platform_image_import_jobs: jobs.clone(),
        platform_registry: Some(registry.clone()),
        objects: objects.clone(),
    });
    let identity = auth::ServiceIdentity {
        issuer: "https://issuer.test".to_owned(),
        subject: "control".to_owned(),
        client_id: "labweaver-control".to_owned(),
        expires_at: time::OffsetDateTime::now_utc() + time::Duration::hours(1),
        permissions: BTreeSet::from([CONTROL_PERMISSION.to_owned()]),
    };
    let app = router(state).layer(axum::Extension(identity));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let api = format!("http://{}", listener.local_addr()?);
    let _server = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
        axum::serve(listener, app).await
    }));
    let worker = PlatformImageImportWorker {
        jobs: jobs.clone(),
        catalog: PgPlatformImageCatalog::new(pool.clone()),
        registry: Some(registry),
        objects: objects.clone(),
        worker_id: "http-import".to_owned(),
        lease_duration: Duration::from_secs(3),
        poll_interval: Duration::from_millis(10),
    };
    let _worker = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(worker.run()));
    let request = request("slow-large-archive");
    let client = reqwest::Client::new();
    let response = tokio::time::timeout(
        Duration::from_secs(1),
        client
            .post(format!("{api}/internal/v1/platform-images/import-jobs"))
            .header("Idempotency-Key", "fast-enqueue")
            .json(&request)
            .send(),
    )
    .await??;
    assert_eq!(response.status(), reqwest::StatusCode::ACCEPTED);
    let response: InternalPlatformImageImportJobStatus = response.json().await?;
    assert_eq!(response.upload_id, request.upload_id);
    tokio::time::timeout(Duration::from_secs(4), objects.started.notified()).await?;
    let replay = client
        .post(format!("{api}/internal/v1/platform-images/import-jobs"))
        .header("Idempotency-Key", "replay-enqueue")
        .json(&request)
        .send()
        .await?;
    assert_eq!(replay.status(), reqwest::StatusCode::ACCEPTED);
    let cancelled = client
        .post(format!(
            "{api}/internal/v1/platform-images/import-jobs/{}/cancel",
            request.upload_id
        ))
        .header("Idempotency-Key", "cancel-running")
        .json(&serde_json::json!({"uploadId": request.upload_id}))
        .send()
        .await?;
    assert_eq!(cancelled.status(), reqwest::StatusCode::ACCEPTED);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    loop {
        let status: InternalPlatformImageImportJobStatus = client
            .get(format!(
                "{api}/internal/v1/platform-images/import-jobs/{}",
                request.upload_id
            ))
            .send()
            .await?
            .json()
            .await?;
        if status.state == PlatformImageImportJobState::Cancelled {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "cancel did not complete: {status:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(objects.cancelled.load(Ordering::SeqCst), 1);
    assert!(
        PgPlatformImageCatalog::new(pool)
            .list(None)
            .await?
            .is_empty()
    );
    Ok(())
}
