//! Durable Agent-owned platform image import jobs.
//!
//! Control submits an immutable archive reference and the upload id. The upload id is the single
//! idempotency identity, while this table's lease token fences worker copies. Registry publication
//! is performed before `PgPlatformImageCatalog::register_import_job`, which locks this row and
//! commits the catalog entry with the terminal job state.

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use artifact_store::ImmutableObjectStore;
use contracts::http::{
    InternalPlatformImageImportEnqueueRequest, InternalPlatformImageImportJobStatus,
    InternalPlatformImageImportRequest, PlatformImageImportJobState,
};
use contracts::{PlatformImageId, UploadSessionId, UtcTimestamp};
use persistence_sqlx::Sha256Digest;
use serde_json::Value;
use sqlx::{PgPool, Row};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::platform_image_import;
use crate::platform_images::{
    PgPlatformImageCatalog, PlatformImageRegistry, PlatformImageStoreError, RegisterPlatformImage,
};

const CANCELLED_DIAGNOSTIC: &str = "LW_PLATFORM_IMAGE_IMPORT_CANCELLED";
const INVALID_REQUEST_DIAGNOSTIC: &str = "LW_PLATFORM_IMAGE_IMPORT_REQUEST_INVALID";

/// Durable job-store failures returned by the Agent internal API and worker.
#[derive(Debug, Error)]
pub enum PlatformImageImportJobError {
    #[error("LW_AGENT_PERSISTENCE_FAILED")]
    Persistence(#[from] sqlx::Error),
    #[error("LW_PLATFORM_IMAGE_IMPORT_NOT_FOUND")]
    NotFound,
    #[error("LW_PLATFORM_IMAGE_IMPORT_STATE_CONFLICT")]
    Conflict,
    #[error("LW_PLATFORM_IMAGE_IMPORT_REQUEST_INVALID")]
    InvalidRequest,
}

impl PlatformImageImportJobError {
    /// Stable diagnostic code for API mapping.
    #[must_use]
    pub const fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::Persistence(_) => "LW_AGENT_PERSISTENCE_FAILED",
            Self::NotFound => "LW_PLATFORM_IMAGE_IMPORT_NOT_FOUND",
            Self::Conflict => "LW_PLATFORM_IMAGE_IMPORT_STATE_CONFLICT",
            Self::InvalidRequest => INVALID_REQUEST_DIAGNOSTIC,
        }
    }
}

/// Agent-owned durable import job repository.
#[derive(Clone, Debug)]
pub struct PlatformImageImportJobStore {
    pool: PgPool,
}

impl PlatformImageImportJobStore {
    /// Creates a repository over the Agent role pool.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Inserts one job or replays the existing immutable request for the upload id.
    pub async fn enqueue(
        &self,
        request: &InternalPlatformImageImportEnqueueRequest,
        now: UtcTimestamp,
    ) -> Result<InternalPlatformImageImportJobStatus, PlatformImageImportJobError> {
        let request_json = serde_json::to_value(&request.request)
            .map_err(|_| PlatformImageImportJobError::InvalidRequest)?;
        let request_sha256 = Sha256Digest::of_canonical(&request.request_json_for_hash())
            .map_err(|_| PlatformImageImportJobError::InvalidRequest)?;
        let inserted = sqlx::query(
            "INSERT INTO agent.platform_image_import_jobs \
             (upload_id,request_json,request_sha256,state,revision,created_at,updated_at) \
             VALUES ($1,$2,$3,'queued',1,$4,$4) \
             ON CONFLICT (upload_id) DO NOTHING",
        )
        .bind(request.upload_id.as_uuid())
        .bind(request_json)
        .bind(request_sha256.to_string())
        .bind(now.get())
        .execute(&self.pool)
        .await?;
        if inserted.rows_affected() == 1 {
            return self.status(request.upload_id).await;
        }
        let row = sqlx::query(
            "SELECT request_sha256 FROM agent.platform_image_import_jobs WHERE upload_id=$1",
        )
        .bind(request.upload_id.as_uuid())
        .fetch_optional(&self.pool)
        .await?
        .ok_or(PlatformImageImportJobError::NotFound)?;
        let stored_hash: String = row.try_get("request_sha256")?;
        if stored_hash != request_sha256.to_string() {
            return Err(PlatformImageImportJobError::Conflict);
        }
        self.status(request.upload_id).await
    }

    /// Reads one job status without exposing its private request or lease token.
    pub async fn status(
        &self,
        upload_id: UploadSessionId,
    ) -> Result<InternalPlatformImageImportJobStatus, PlatformImageImportJobError> {
        let row = sqlx::query(
            "SELECT upload_id,state,revision,diagnostic,catalog_id \
             FROM agent.platform_image_import_jobs WHERE upload_id=$1",
        )
        .bind(upload_id.as_uuid())
        .fetch_optional(&self.pool)
        .await?
        .ok_or(PlatformImageImportJobError::NotFound)?;
        status_from_row(&row)
    }

    /// Requests cancellation. Running work is allowed to finish its blocking cleanup, but its
    /// terminal catalog transaction observes this flag and refuses publication.
    pub async fn cancel(
        &self,
        upload_id: UploadSessionId,
        now: UtcTimestamp,
    ) -> Result<InternalPlatformImageImportJobStatus, PlatformImageImportJobError> {
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query(
            "SELECT state,cancellation_requested FROM agent.platform_image_import_jobs \
             WHERE upload_id=$1 FOR UPDATE",
        )
        .bind(upload_id.as_uuid())
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(PlatformImageImportJobError::NotFound)?;
        let state: String = row.try_get("state")?;
        let cancellation_requested: bool = row.try_get("cancellation_requested")?;
        if state == "queued" {
            sqlx::query(
                "UPDATE agent.platform_image_import_jobs \
                 SET state='cancelled',cancellation_requested=true,revision=revision+1, \
                     diagnostic=$2,updated_at=$3,completed_at=$3 \
                 WHERE upload_id=$1 AND state='queued'",
            )
            .bind(upload_id.as_uuid())
            .bind(CANCELLED_DIAGNOSTIC)
            .bind(now.get())
            .execute(&mut *transaction)
            .await?;
        } else if state == "running" && !cancellation_requested {
            sqlx::query(
                "UPDATE agent.platform_image_import_jobs \
                 SET cancellation_requested=true,revision=revision+1,updated_at=$2 \
                 WHERE upload_id=$1 AND state='running'",
            )
            .bind(upload_id.as_uuid())
            .bind(now.get())
            .execute(&mut *transaction)
            .await?;
        }
        let updated = sqlx::query(
            "SELECT upload_id,state,revision,diagnostic,catalog_id \
             FROM agent.platform_image_import_jobs WHERE upload_id=$1",
        )
        .bind(upload_id.as_uuid())
        .fetch_one(&mut *transaction)
        .await?;
        let status = status_from_row(&updated)?;
        transaction.commit().await?;
        Ok(status)
    }

    /// Claims one queued or expired running job with a fresh fencing token.
    pub async fn claim(
        &self,
        worker_id: &str,
        lease_duration: Duration,
        now: UtcTimestamp,
    ) -> Result<Option<ClaimedPlatformImageImport>, PlatformImageImportJobError> {
        let lease_seconds = i64::try_from(lease_duration.as_secs())
            .map_err(|_| PlatformImageImportJobError::InvalidRequest)?;
        if lease_seconds == 0 {
            return Err(PlatformImageImportJobError::InvalidRequest);
        }
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query(
            "SELECT upload_id,request_json,cancellation_requested \
             FROM agent.platform_image_import_jobs \
             WHERE state='queued' \
                OR (state='running' AND lease_expires_at<=date_trunc('milliseconds',clock_timestamp())) \
             ORDER BY updated_at,upload_id \
             LIMIT 1 FOR UPDATE SKIP LOCKED",
        )
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(row) = row else {
            transaction.commit().await?;
            return Ok(None);
        };
        let upload_id =
            UploadSessionId::from_str(&row.try_get::<Uuid, _>("upload_id")?.to_string())
                .map_err(|_| PlatformImageImportJobError::InvalidRequest)?;
        let cancellation_requested: bool = row.try_get("cancellation_requested")?;
        if cancellation_requested {
            sqlx::query(
                "UPDATE agent.platform_image_import_jobs \
                 SET state='cancelled',diagnostic=$2,lease_token=NULL,lease_expires_at=NULL, \
                     revision=revision+1,updated_at=$3,completed_at=$3 \
                 WHERE upload_id=$1 AND state='running'",
            )
            .bind(upload_id.as_uuid())
            .bind(CANCELLED_DIAGNOSTIC)
            .bind(now.get())
            .execute(&mut *transaction)
            .await?;
            transaction.commit().await?;
            return Ok(None);
        }
        let request_json: Value = row.try_get("request_json")?;
        let request: InternalPlatformImageImportRequest =
            if let Ok(request) = serde_json::from_value(request_json) {
                request
            } else {
                sqlx::query(
                    "UPDATE agent.platform_image_import_jobs \
                     SET state='failed',diagnostic=$2,lease_token=NULL,lease_expires_at=NULL, \
                         revision=revision+1,updated_at=$3, \
                         completed_at=$3 \
                     WHERE upload_id=$1",
                )
                .bind(upload_id.as_uuid())
                .bind(INVALID_REQUEST_DIAGNOSTIC)
                .bind(now.get())
                .execute(&mut *transaction)
                .await?;
                transaction.commit().await?;
                return Ok(None);
            };
        let lease_token = Uuid::now_v7();
        sqlx::query(
            "UPDATE agent.platform_image_import_jobs \
             SET state='running',worker_id=$2,lease_token=$3, \
                 lease_expires_at=date_trunc('milliseconds',clock_timestamp())+($4*interval '1 second'), \
                 revision=revision+1,updated_at=$5 \
             WHERE upload_id=$1 AND state IN ('queued','running')",
        )
        .bind(upload_id.as_uuid())
        .bind(worker_id)
        .bind(lease_token)
        .bind(lease_seconds)
        .bind(now.get())
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(Some(ClaimedPlatformImageImport {
            upload_id,
            lease_token,
            request,
        }))
    }

    /// Marks a running job failed only when the worker still owns its lease.
    pub async fn fail(
        &self,
        upload_id: UploadSessionId,
        lease_token: Uuid,
        diagnostic: &str,
        now: UtcTimestamp,
    ) -> Result<(), PlatformImageImportJobError> {
        let updated = sqlx::query(
            "UPDATE agent.platform_image_import_jobs \
             SET state=CASE WHEN cancellation_requested THEN 'cancelled' ELSE 'failed' END, \
                 diagnostic=CASE WHEN cancellation_requested \
                     THEN 'LW_PLATFORM_IMAGE_IMPORT_CANCELLED' ELSE $3 END, \
                 lease_token=NULL,lease_expires_at=NULL, \
                 revision=revision+1,updated_at=$4,completed_at=$4 \
             WHERE upload_id=$1 AND state='running' AND lease_token=$2 \
               AND lease_expires_at>date_trunc('milliseconds',clock_timestamp())",
        )
        .bind(upload_id.as_uuid())
        .bind(lease_token)
        .bind(diagnostic)
        .bind(now.get())
        .execute(&self.pool)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(PlatformImageImportJobError::Conflict);
        }
        Ok(())
    }

    /// Marks a running cancelled job after its blocking import has fully unwound.
    pub async fn cancelled(
        &self,
        upload_id: UploadSessionId,
        lease_token: Uuid,
        now: UtcTimestamp,
    ) -> Result<(), PlatformImageImportJobError> {
        let updated = sqlx::query(
            "UPDATE agent.platform_image_import_jobs \
             SET state='cancelled',diagnostic=$3,lease_token=NULL,lease_expires_at=NULL, \
                 revision=revision+1,updated_at=$4,completed_at=$4 \
             WHERE upload_id=$1 AND state='running' AND lease_token=$2 \
               AND lease_expires_at>date_trunc('milliseconds',clock_timestamp())",
        )
        .bind(upload_id.as_uuid())
        .bind(lease_token)
        .bind(CANCELLED_DIAGNOSTIC)
        .bind(now.get())
        .execute(&self.pool)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(PlatformImageImportJobError::Conflict);
        }
        Ok(())
    }

    /// Reports whether the current worker's job has been cancelled.
    pub async fn cancellation_requested(
        &self,
        upload_id: UploadSessionId,
        lease_token: Uuid,
    ) -> Result<bool, PlatformImageImportJobError> {
        let row = sqlx::query(
            "SELECT cancellation_requested FROM agent.platform_image_import_jobs \
              WHERE upload_id=$1 AND state='running' AND lease_token=$2 \
                AND lease_expires_at>date_trunc('milliseconds',clock_timestamp())",
        )
        .bind(upload_id.as_uuid())
        .bind(lease_token)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(PlatformImageImportJobError::Conflict)?;
        Ok(row.try_get("cancellation_requested")?)
    }

    /// Extends the current worker lease without changing the public job revision.
    pub async fn renew(
        &self,
        upload_id: UploadSessionId,
        lease_token: Uuid,
        lease_duration: Duration,
        now: UtcTimestamp,
    ) -> Result<(), PlatformImageImportJobError> {
        let lease_seconds = i64::try_from(lease_duration.as_secs())
            .map_err(|_| PlatformImageImportJobError::InvalidRequest)?;
        if lease_seconds == 0 {
            return Err(PlatformImageImportJobError::InvalidRequest);
        }
        let updated = sqlx::query(
            "UPDATE agent.platform_image_import_jobs \
             SET lease_expires_at=date_trunc('milliseconds',clock_timestamp())+($3*interval '1 second'), \
                 updated_at=$4 \
             WHERE upload_id=$1 AND state='running' AND lease_token=$2 \
               AND lease_expires_at>date_trunc('milliseconds',clock_timestamp())",
        )
        .bind(upload_id.as_uuid())
        .bind(lease_token)
        .bind(lease_seconds)
        .bind(now.get())
        .execute(&self.pool)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(PlatformImageImportJobError::Conflict);
        }
        Ok(())
    }
}

/// Claimed Agent import payload and its fencing token.
#[derive(Clone, Debug)]
pub struct ClaimedPlatformImageImport {
    pub upload_id: UploadSessionId,
    pub lease_token: Uuid,
    pub request: InternalPlatformImageImportRequest,
}

/// Background worker for durable platform image imports.
#[derive(Clone)]
pub struct PlatformImageImportWorker {
    pub jobs: PlatformImageImportJobStore,
    pub catalog: PgPlatformImageCatalog,
    pub registry: Option<PlatformImageRegistry>,
    pub objects: Arc<dyn ImmutableObjectStore>,
    pub worker_id: String,
    pub lease_duration: Duration,
    pub poll_interval: Duration,
}

impl PlatformImageImportWorker {
    /// Runs the bounded import loop until the service is stopped.
    pub async fn run(self) -> Result<(), PlatformImageImportJobError> {
        let mut ticker = tokio::time::interval(self.poll_interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            if let Err(error) = self.tick().await {
                tracing::error!(
                    event = "agent.platform_image_import.worker_error",
                    diagnostic = %error,
                    "platform image import worker tick failed; lease recovery will retry"
                );
                tokio::time::sleep(self.poll_interval).await;
            }
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one lease owner must join cancellation cleanup before committing the import result"
    )]
    async fn tick(&self) -> Result<(), PlatformImageImportJobError> {
        let now = timestamp().map_err(|()| PlatformImageImportJobError::InvalidRequest)?;
        let Some(job) = self
            .jobs
            .claim(&self.worker_id, self.lease_duration, now)
            .await?
        else {
            return Ok(());
        };
        let Some(registry) = self.registry.as_ref() else {
            self.jobs
                .fail(
                    job.upload_id,
                    job.lease_token,
                    "LW_PLATFORM_IMAGE_REGISTRY_NOT_CONFIGURED",
                    timestamp().map_err(|()| PlatformImageImportJobError::InvalidRequest)?,
                )
                .await?;
            return Ok(());
        };
        let renew_period = Duration::from_secs((self.lease_duration.as_secs() / 3).max(1));
        let registry = registry.clone();
        let objects = Arc::clone(&self.objects);
        let request = job.request.clone();
        let cancellation = CancellationToken::new();
        let _cancel_on_drop = cancellation.clone().drop_guard();
        let publish_cancellation = cancellation.clone();
        let mut publish_task = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
            platform_image_import::publish_platform_image_with_cancellation(
                &registry,
                objects.as_ref(),
                &request,
                &publish_cancellation,
            )
            .await
        }));
        let mut lease_lost = false;
        let mut cancellation_requested = false;
        let mut lease_ticker = tokio::time::interval(renew_period);
        lease_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let published = loop {
            tokio::select! {
                result = &mut publish_task => break Some(result),
                _ = lease_ticker.tick() => {
                    let Ok(now) = timestamp() else {
                            lease_lost = true;
                            break None;
                    };
                    match self.jobs.cancellation_requested(job.upload_id, job.lease_token).await {
                        Ok(true) => {
                            cancellation_requested = true;
                            cancellation.cancel();
                        }
                        Ok(false) => {}
                        Err(error) => {
                            tracing::warn!(
                                event = "agent.platform_image_import.lease_read_failed",
                                upload_id = %job.upload_id,
                                diagnostic = %error,
                                "stopping publication until the job lease can be recovered"
                            );
                            lease_lost = true;
                            break None;
                        }
                    }
                    if let Err(error) = self.jobs.renew(
                        job.upload_id,
                        job.lease_token,
                        self.lease_duration,
                        now,
                    ).await {
                        tracing::warn!(
                            event = "agent.platform_image_import.lease_renew_failed",
                            upload_id = %job.upload_id,
                            diagnostic = %error,
                            "stopping publication until the job lease can be recovered"
                        );
                        lease_lost = true;
                        break None;
                    }
                }
            }
        };
        if lease_lost {
            cancellation.cancel();
            let _ = publish_task.await;
            return Ok(());
        }
        let Some(published) = published else {
            return Ok(());
        };
        let published = match published {
            Ok(Ok(published)) => published,
            Ok(Err(platform_image_import::PlatformImageImportError::Cancelled)) => {
                self.jobs
                    .cancelled(
                        job.upload_id,
                        job.lease_token,
                        timestamp().map_err(|()| PlatformImageImportJobError::InvalidRequest)?,
                    )
                    .await?;
                return Ok(());
            }
            Ok(Err(error)) => {
                let diagnostic = error.diagnostic_code();
                if matches!(
                    error,
                    platform_image_import::PlatformImageImportError::ObjectStore(
                        artifact_store::ObjectStoreError::ObjectUnavailable
                    ) | platform_image_import::PlatformImageImportError::Registry(
                        crate::platform_images::PlatformImageRegistryError::RegistryUnavailable
                    ) | platform_image_import::PlatformImageImportError::Catalog(
                        PlatformImageStoreError::Persistence
                    )
                ) {
                    tracing::warn!(event = "agent.platform_image_import.transport_retry", upload_id = %job.upload_id, diagnostic,
                        "temporary import failure; the immutable job will be recovered after lease expiry");
                    return Ok(());
                }
                self.jobs
                    .fail(
                        job.upload_id,
                        job.lease_token,
                        diagnostic,
                        timestamp().map_err(|()| PlatformImageImportJobError::InvalidRequest)?,
                    )
                    .await?;
                return Ok(());
            }
            Err(_) => {
                self.jobs
                    .fail(
                        job.upload_id,
                        job.lease_token,
                        "LW_PLATFORM_IMAGE_IMPORT_WORKER_FAILED",
                        timestamp().map_err(|()| PlatformImageImportJobError::InvalidRequest)?,
                    )
                    .await?;
                return Ok(());
            }
        };
        if cancellation_requested
            || self
                .jobs
                .cancellation_requested(job.upload_id, job.lease_token)
                .await?
        {
            self.jobs
                .cancelled(
                    job.upload_id,
                    job.lease_token,
                    timestamp().map_err(|()| PlatformImageImportJobError::InvalidRequest)?,
                )
                .await?;
            return Ok(());
        }
        let register = RegisterPlatformImage {
            kind: job.request.kind,
            binding: job.request.binding.clone(),
            source_reference: job.request.target_reference.clone(),
            resolved_digest: published.resolved_digest,
            media_type: published.media_type,
            size_bytes: published.size_bytes,
            capacity_bytes: published.capacity_bytes,
            disk_sha256: published.disk_sha256,
            format: published.format,
            trust_revision: job.request.trust_revision,
            actor_id: job.request.actor_id,
            reason: job.request.reason.clone(),
            now: timestamp().map_err(|()| PlatformImageImportJobError::InvalidRequest)?,
        };
        match self
            .catalog
            .register_import_job(job.upload_id, job.lease_token, &register)
            .await
        {
            Ok(_) => Ok(()),
            Err(PlatformImageStoreError::Conflict) => {
                if self
                    .jobs
                    .cancellation_requested(job.upload_id, job.lease_token)
                    .await?
                {
                    self.jobs
                        .cancelled(
                            job.upload_id,
                            job.lease_token,
                            timestamp()
                                .map_err(|()| PlatformImageImportJobError::InvalidRequest)?,
                        )
                        .await?;
                } else {
                    self.jobs
                        .fail(
                            job.upload_id,
                            job.lease_token,
                            "LW_PLATFORM_IMAGE_STATE_CONFLICT",
                            timestamp()
                                .map_err(|()| PlatformImageImportJobError::InvalidRequest)?,
                        )
                        .await?;
                }
                Ok(())
            }
            Err(PlatformImageStoreError::InvalidRequest) => {
                self.jobs
                    .fail(
                        job.upload_id,
                        job.lease_token,
                        INVALID_REQUEST_DIAGNOSTIC,
                        timestamp().map_err(|()| PlatformImageImportJobError::InvalidRequest)?,
                    )
                    .await?;
                Ok(())
            }
            Err(PlatformImageStoreError::NotFound | PlatformImageStoreError::Persistence) => {
                Err(PlatformImageImportJobError::Conflict)
            }
        }
    }
}

fn status_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<InternalPlatformImageImportJobStatus, PlatformImageImportJobError> {
    let upload_id = UploadSessionId::from_str(&row.try_get::<Uuid, _>("upload_id")?.to_string())
        .map_err(|_| PlatformImageImportJobError::InvalidRequest)?;
    let state = match row.try_get::<String, _>("state")?.as_str() {
        "queued" => PlatformImageImportJobState::Queued,
        "running" => PlatformImageImportJobState::Running,
        "succeeded" => PlatformImageImportJobState::Succeeded,
        "failed" => PlatformImageImportJobState::Failed,
        "cancelled" => PlatformImageImportJobState::Cancelled,
        _ => return Err(PlatformImageImportJobError::InvalidRequest),
    };
    let revision = contracts::Revision::new(
        u64::try_from(row.try_get::<i64, _>("revision")?)
            .map_err(|_| PlatformImageImportJobError::InvalidRequest)?,
    )
    .map_err(|_| PlatformImageImportJobError::InvalidRequest)?;
    let catalog_id = row
        .try_get::<Option<Uuid>, _>("catalog_id")?
        .map(|id| {
            PlatformImageId::from_str(&id.to_string())
                .map_err(|_| PlatformImageImportJobError::InvalidRequest)
        })
        .transpose()?;
    Ok(InternalPlatformImageImportJobStatus {
        upload_id,
        state,
        revision,
        diagnostic: row.try_get("diagnostic")?,
        catalog_id,
    })
}

fn timestamp() -> Result<UtcTimestamp, ()> {
    let value = time::OffsetDateTime::now_utc();
    let value = value
        .replace_nanosecond((value.nanosecond() / 1_000_000) * 1_000_000)
        .map_err(|_| ())?;
    UtcTimestamp::from_utc(value).map_err(|_| ())
}

trait EnqueueHash {
    fn request_json_for_hash(&self) -> Value;
}

impl EnqueueHash for InternalPlatformImageImportEnqueueRequest {
    fn request_json_for_hash(&self) -> Value {
        serde_json::json!({"uploadId": self.upload_id, "request": self.request})
    }
}
