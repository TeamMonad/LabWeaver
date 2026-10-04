//! Durable Control-side orchestration for platform image imports.
//!
//! Control owns the upload session and its public state.  This worker only keeps the
//! short database lease, freezes the exact uploaded object version, and coordinates the
//! Agent-owned import job.  It never holds a database transaction while calling either
//! the object store or Agent.
#![allow(missing_docs)]

use std::str::FromStr;
use std::time::Duration;

use artifact_store::ObjectStoreError;
use contracts::http::{
    IdempotencyKey, InternalPlatformImageImportEnqueueRequest, InternalPlatformImageImportRequest,
    PlatformImageImportJobState, PlatformImageKind,
};
use contracts::supply_chain::VirtualMachineDiskFormat;
use contracts::{ActorId, ArtifactRef, PlatformImageId, UploadSessionId, UtcTimestamp};
use reqwest::header::HeaderMap;
use sqlx::Row;
use thiserror::Error;
use uuid::Uuid;

use crate::clients::AgentClient;
use crate::{
    ControlError, ControlService, db, platform_image_kind_from_str, schedule_staged_archive_cleanup,
};

const CANCELLED_DIAGNOSTIC: &str = "LW_PLATFORM_IMAGE_IMPORT_CANCELLED";
const AGENT_REQUEST_ATTEMPTS: usize = 3;
const TERMINAL_UPLOAD_RECONCILE_SECONDS: i64 = 300;
const TERMINAL_UPLOAD_RETRY_SECONDS: i64 = 30;

/// Errors that stop one worker tick.  A downstream error is kept separate from a database
/// failure so the service logs the real boundary without fabricating an import result.
#[derive(Debug, Error)]
pub enum PlatformImageImportWorkerError {
    #[error(transparent)]
    Control(#[from] ControlError),
    #[error(transparent)]
    Objects(#[from] ObjectStoreError),
    #[error("LW_CONTROL_PLATFORM_IMAGE_IMPORT_INVALID")]
    Invalid,
}

/// One Control upload claim and its lease fencing token.
#[derive(Clone, Debug)]
pub struct PlatformImageImportClaim {
    pub upload_id: UploadSessionId,
    pub lease_token: Uuid,
    pub actor_id: ActorId,
    pub kind: PlatformImageKind,
    pub binding: String,
    pub target_reference: String,
    pub archive_object_key: String,
    pub archive_size: u64,
    pub archive_media_type: String,
    pub disk_format: Option<VirtualMachineDiskFormat>,
    pub disk_path: Option<String>,
    pub capacity_bytes: Option<u64>,
    pub trust_revision: u64,
    pub reason: String,
    pub completion_key: Option<IdempotencyKey>,
    pub archive: Option<ArtifactRef>,
    pub cancel_requested: bool,
    pub expires_at: UtcTimestamp,
}

impl ControlService {
    /// Claims one queued or expired import without holding the transaction during I/O.
    pub async fn claim_platform_image_import(
        &self,
        now: UtcTimestamp,
    ) -> Result<Option<PlatformImageImportClaim>, ControlError> {
        let lease_seconds = i64::try_from(self.config.completion_lease_seconds)
            .map_err(|_| ControlError::ConfigurationInvalid)?;
        if lease_seconds == 0 {
            return Err(ControlError::ConfigurationInvalid);
        }
        let mut transaction = self.pool.begin().await.map_err(db)?;
        let row = sqlx::query(
            r"SELECT upload_id,created_by,kind,binding,target_reference,trust_revision,reason,
                      archive_bytes,archive_media_type,object_key,object_version,artifact_id,
                      disk_format,disk_path,capacity_bytes,state,cancel_requested,
                      completion_idempotency_key,expires_at
               FROM control.platform_image_upload_sessions
              WHERE state='queued'
                 OR (state IN ('freezing','importing','cancelling')
                     AND completion_lease_expires_at<=date_trunc('milliseconds',clock_timestamp()))
              ORDER BY updated_at,upload_id
              LIMIT 1
              FOR UPDATE SKIP LOCKED",
        )
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?;
        let Some(row) = row else {
            transaction.commit().await.map_err(db)?;
            return Ok(None);
        };

        let upload_id = parse_upload_id(&row)?;
        let cancel_requested: bool = row.try_get("cancel_requested").map_err(db)?;
        let lease_token = Uuid::now_v7();
        let next_state = if cancel_requested {
            "cancelling"
        } else if row
            .try_get::<Option<Uuid>, _>("artifact_id")
            .map_err(db)?
            .is_some()
        {
            "importing"
        } else {
            "freezing"
        };
        let updated = sqlx::query(
            r"UPDATE control.platform_image_upload_sessions
                  SET state=$2,completion_lease_token=$3,
                      completion_lease_expires_at=date_trunc('milliseconds',clock_timestamp())
                          +($4*interval '1 second'),
                      revision=revision+1,updated_at=$5
                WHERE upload_id=$1
                  AND (state='queued' OR state IN ('freezing','importing','cancelling'))
            RETURNING upload_id",
        )
        .bind(upload_id.as_uuid())
        .bind(next_state)
        .bind(lease_token)
        .bind(lease_seconds)
        .bind(now.get())
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        if updated.rows_affected() != 1 {
            transaction.rollback().await.map_err(db)?;
            return Ok(None);
        }

        let claim = claim_from_row(
            &row,
            upload_id,
            lease_token,
            cancel_requested,
            self.objects.binding(),
        )?;
        transaction.commit().await.map_err(db)?;
        Ok(Some(claim))
    }

    /// Records the exact object version frozen by the worker and moves it to Agent import.
    pub async fn record_platform_image_import_reference(
        &self,
        claim: &PlatformImageImportClaim,
        archive: &ArtifactRef,
        now: UtcTimestamp,
    ) -> Result<(), ControlError> {
        let updated = sqlx::query(
            r"UPDATE control.platform_image_upload_sessions
                  SET artifact_id=$3,object_version=$4,
                      state=CASE WHEN cancel_requested THEN 'cancelling' ELSE 'importing' END,
                      revision=revision+1,updated_at=$5
                WHERE upload_id=$1 AND completion_lease_token=$2
                  AND state IN ('freezing','cancelling')
                  AND completion_lease_expires_at>date_trunc('milliseconds',clock_timestamp())
                  AND (artifact_id IS NULL OR (artifact_id=$3 AND object_version=$4))",
        )
        .bind(claim.upload_id.as_uuid())
        .bind(claim.lease_token)
        .bind(archive.artifact_id.as_uuid())
        .bind(&archive.object_version)
        .bind(now.get())
        .execute(&self.pool)
        .await
        .map_err(db)?;
        if updated.rows_affected() != 1 {
            return Err(ControlError::OperationLeaseLost);
        }
        Ok(())
    }

    /// Extends one Control lease while Agent performs its bounded but potentially large import.
    pub async fn renew_platform_image_import(
        &self,
        claim: &PlatformImageImportClaim,
        now: UtcTimestamp,
    ) -> Result<(), ControlError> {
        let lease_seconds = i64::try_from(self.config.completion_lease_seconds)
            .map_err(|_| ControlError::ConfigurationInvalid)?;
        let updated = sqlx::query(
            r"UPDATE control.platform_image_upload_sessions
                  SET completion_lease_expires_at=date_trunc('milliseconds',clock_timestamp())
                      +($3*interval '1 second'),updated_at=$4
                WHERE upload_id=$1 AND completion_lease_token=$2
                  AND state IN ('freezing','importing','cancelling')
                  AND completion_lease_expires_at>date_trunc('milliseconds',clock_timestamp())",
        )
        .bind(claim.upload_id.as_uuid())
        .bind(claim.lease_token)
        .bind(lease_seconds)
        .bind(now.get())
        .execute(&self.pool)
        .await
        .map_err(db)?;
        if updated.rows_affected() != 1 {
            return Err(ControlError::OperationLeaseLost);
        }
        Ok(())
    }

    /// Reads the cancellation flag fenced to the current Control worker.
    pub async fn platform_image_import_cancel_requested(
        &self,
        claim: &PlatformImageImportClaim,
    ) -> Result<bool, ControlError> {
        let row = sqlx::query(
            r"SELECT cancel_requested
                 FROM control.platform_image_upload_sessions
                WHERE upload_id=$1 AND completion_lease_token=$2
                  AND state IN ('freezing','importing','cancelling')
                  AND completion_lease_expires_at>date_trunc('milliseconds',clock_timestamp())",
        )
        .bind(claim.upload_id.as_uuid())
        .bind(claim.lease_token)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?
        .ok_or(ControlError::OperationLeaseLost)?;
        row.try_get("cancel_requested").map_err(db)
    }

    /// Finalizes a successful Agent catalog publication under the Control lease fence.
    pub async fn finish_platform_image_import_fenced(
        &self,
        claim: &PlatformImageImportClaim,
        catalog_id: PlatformImageId,
        now: UtcTimestamp,
    ) -> Result<(), ControlError> {
        let mut transaction = self.pool.begin().await.map_err(db)?;
        let row = sqlx::query(
            r"UPDATE control.platform_image_upload_sessions
                  SET state='imported',imported_catalog_id=$3,terminal_diagnostic=NULL,
                      completion_lease_token=NULL,completion_lease_expires_at=NULL,
                      revision=revision+1,updated_at=$4
                WHERE upload_id=$1 AND completion_lease_token=$2
                  AND state IN ('importing','cancelling')
                  AND completion_lease_expires_at>date_trunc('milliseconds',clock_timestamp())
            RETURNING upload_id,state,revision,cancel_requested,terminal_diagnostic,imported_catalog_id,
                      object_key,object_version,completion_idempotency_key",
        )
        .bind(claim.upload_id.as_uuid())
        .bind(claim.lease_token)
        .bind(catalog_id.as_uuid())
        .bind(now.get())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?
        .ok_or(ControlError::OperationLeaseLost)?;
        let object_key: String = row.try_get("object_key").map_err(db)?;
        let object_version: Option<String> = row.try_get("object_version").map_err(db)?;
        schedule_staged_archive_cleanup(
            &mut transaction,
            claim.upload_id,
            &object_key,
            object_version.as_deref(),
        )
        .await?;
        transaction.commit().await.map_err(db)
    }

    /// Finalizes a failed Agent import, preserving the diagnostic and exact cleanup version.
    pub async fn fail_platform_image_import_fenced(
        &self,
        claim: &PlatformImageImportClaim,
        diagnostic: &str,
        now: UtcTimestamp,
    ) -> Result<(), ControlError> {
        let mut transaction = self.pool.begin().await.map_err(db)?;
        let row = sqlx::query(
            r"UPDATE control.platform_image_upload_sessions
                  SET state=CASE WHEN cancel_requested THEN 'cancelled' ELSE 'failed' END,
                      terminal_diagnostic=CASE WHEN cancel_requested
                          THEN 'LW_PLATFORM_IMAGE_IMPORT_CANCELLED' ELSE $3 END,
                      completion_lease_token=NULL,completion_lease_expires_at=NULL,
                      revision=revision+1,updated_at=$4
                WHERE upload_id=$1 AND completion_lease_token=$2
                  AND state IN ('freezing','importing','cancelling')
                  AND completion_lease_expires_at>date_trunc('milliseconds',clock_timestamp())
            RETURNING upload_id,state,revision,cancel_requested,terminal_diagnostic,imported_catalog_id,
                      object_key,object_version,completion_idempotency_key",
        )
        .bind(claim.upload_id.as_uuid())
        .bind(claim.lease_token)
        .bind(diagnostic)
        .bind(now.get())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?
        .ok_or(ControlError::OperationLeaseLost)?;
        let object_key: String = row.try_get("object_key").map_err(db)?;
        let object_version: Option<String> = row.try_get("object_version").map_err(db)?;
        schedule_staged_archive_cleanup(
            &mut transaction,
            claim.upload_id,
            &object_key,
            object_version.as_deref(),
        )
        .await?;
        transaction.commit().await.map_err(db)
    }

    /// Marks cancellation after Agent has stopped and released its temporary import resources.
    pub async fn cancel_platform_image_import_fenced(
        &self,
        claim: &PlatformImageImportClaim,
        now: UtcTimestamp,
    ) -> Result<(), ControlError> {
        self.fail_platform_image_import_fenced(claim, CANCELLED_DIAGNOSTIC, now)
            .await
    }
}

/// Background Control worker that coordinates one durable Agent job at a time.
#[derive(Clone)]
pub struct PlatformImageImportWorker {
    pub control: ControlService,
    pub agent: AgentClient,
    pub poll_interval: Duration,
}

impl PlatformImageImportWorker {
    /// Runs until the process is stopped.
    pub async fn run(self) -> Result<(), PlatformImageImportWorkerError> {
        let mut ticker = tokio::time::interval(self.poll_interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            if let Err(error) = self.tick().await {
                tracing::error!(
                    event = "control.platform_image_import.worker_error",
                    diagnostic = %error,
                    "platform image import worker tick failed; lease recovery will retry"
                );
                tokio::time::sleep(self.poll_interval).await;
            }
        }
    }

    /// Processes one leased import through its current boundary. An unresolved downstream
    /// response leaves the immutable job recoverable under the next lease.
    #[allow(
        clippy::too_many_lines,
        reason = "one leased import keeps freeze, enqueue, cancellation and terminal commit ordering visible"
    )]
    pub async fn tick(&self) -> Result<(), PlatformImageImportWorkerError> {
        let now = timestamp().map_err(|()| PlatformImageImportWorkerError::Invalid)?;
        // Only sessions that never accepted completion expire here. A queued or running
        // immutable Agent job continues independently of the signed PUT lifetime.
        sqlx::query(
            r"UPDATE control.platform_image_upload_sessions
                  SET state='failed',terminal_diagnostic='LW_PLATFORM_IMAGE_UPLOAD_EXPIRED',
                      revision=revision+1,updated_at=$1
                WHERE state='pending' AND expires_at<=$1",
        )
        .bind(now.get())
        .execute(&self.control.pool)
        .await
        .map_err(db)?;
        self.record_expired_upload_versions(now).await?;
        let Some(mut claim) = self.control.claim_platform_image_import(now).await? else {
            return Ok(());
        };
        let had_archive_reference = claim.archive.is_some();
        if claim.archive.is_none() {
            let archive = match self
                .control
                .objects
                .freeze_current_reference(
                    &claim.archive_object_key,
                    claim.archive_size,
                    &claim.archive_media_type,
                )
                .await
            {
                Ok(archive) => archive,
                Err(ObjectStoreError::ObjectNotFound) => {
                    claim.cancel_requested = self
                        .control
                        .platform_image_import_cancel_requested(&claim)
                        .await?;
                    if now.get() < claim.expires_at.get() {
                        return Ok(());
                    }
                    // The presigned URL normally expires with the session, but a late upload
                    // can race the worker's first HEAD. Resolve once more after expiry so any
                    // cleanup still records the exact immutable version; never delete the key
                    // without a version.
                    match self
                        .control
                        .objects
                        .freeze_current_reference(
                            &claim.archive_object_key,
                            claim.archive_size,
                            &claim.archive_media_type,
                        )
                        .await
                    {
                        Ok(archive) => archive,
                        Err(ObjectStoreError::ObjectNotFound) => {
                            if claim.cancel_requested {
                                self.control
                                    .cancel_platform_image_import_fenced(
                                        &claim,
                                        timestamp().map_err(|()| {
                                            PlatformImageImportWorkerError::Invalid
                                        })?,
                                    )
                                    .await?;
                            } else {
                                self.control
                                    .fail_platform_image_import_fenced(
                                        &claim,
                                        "LW_OBJECT_UNAVAILABLE",
                                        timestamp().map_err(|()| {
                                            PlatformImageImportWorkerError::Invalid
                                        })?,
                                    )
                                    .await?;
                            }
                            return Ok(());
                        }
                        Err(ObjectStoreError::ObjectUnavailable) => {
                            return Err(ObjectStoreError::ObjectUnavailable.into());
                        }
                        Err(error) => {
                            self.control
                                .fail_platform_image_import_fenced(
                                    &claim,
                                    error.diagnostic_code(),
                                    timestamp()
                                        .map_err(|()| PlatformImageImportWorkerError::Invalid)?,
                                )
                                .await?;
                            return Ok(());
                        }
                    }
                }
                Err(ObjectStoreError::ObjectUnavailable) => {
                    return Err(ObjectStoreError::ObjectUnavailable.into());
                }
                Err(error) => {
                    self.control
                        .fail_platform_image_import_fenced(
                            &claim,
                            error.diagnostic_code(),
                            timestamp().map_err(|()| PlatformImageImportWorkerError::Invalid)?,
                        )
                        .await?;
                    return Ok(());
                }
            };
            self.control
                .record_platform_image_import_reference(
                    &claim,
                    &archive,
                    timestamp().map_err(|()| PlatformImageImportWorkerError::Invalid)?,
                )
                .await?;
            claim.archive = Some(archive);
        }

        claim.cancel_requested = self
            .control
            .platform_image_import_cancel_requested(&claim)
            .await?;
        if claim.cancel_requested && !had_archive_reference {
            self.control
                .cancel_platform_image_import_fenced(
                    &claim,
                    timestamp().map_err(|()| PlatformImageImportWorkerError::Invalid)?,
                )
                .await?;
            return Ok(());
        }

        let request = InternalPlatformImageImportEnqueueRequest {
            upload_id: claim.upload_id,
            request: claim.import_request()?,
        };
        let empty_headers = HeaderMap::new();
        let agent_key = claim
            .completion_key
            .clone()
            .ok_or(PlatformImageImportWorkerError::Invalid)?;
        let mut enqueued = false;
        for attempt in 0..AGENT_REQUEST_ATTEMPTS {
            match self
                .agent
                .enqueue_platform_image_import(&request, &agent_key, &empty_headers)
                .await
            {
                Ok(_) => {
                    enqueued = true;
                    break;
                }
                Err(error) if attempt + 1 < AGENT_REQUEST_ATTEMPTS => {
                    tracing::warn!(event = "control.platform_image_import.enqueue_retry", upload_id = %claim.upload_id,
                        diagnostic = %error, "Agent enqueue response is unresolved; retrying the same job");
                    tokio::time::sleep(self.poll_interval).await;
                }
                Err(error) => {
                    tracing::warn!(event = "control.platform_image_import.enqueue_unresolved", upload_id = %claim.upload_id,
                        diagnostic = %error, "Agent enqueue response is unresolved; lease recovery will retry the same job");
                }
            }
        }
        if !enqueued {
            // The stable upload id makes a retry safe, but a lost response must not become a
            // terminal failure while Agent may already be publishing the job.
            return Ok(());
        }

        loop {
            self.control
                .renew_platform_image_import(
                    &claim,
                    timestamp().map_err(|()| PlatformImageImportWorkerError::Invalid)?,
                )
                .await?;
            let cancel_requested = self
                .control
                .platform_image_import_cancel_requested(&claim)
                .await?;
            let status = if cancel_requested {
                let cancel_key =
                    IdempotencyKey::parse(&format!("platform-image-cancel:{}", claim.upload_id))
                        .map_err(|_| PlatformImageImportWorkerError::Invalid)?;
                match self
                    .agent
                    .cancel_platform_image_import(claim.upload_id, &cancel_key, &empty_headers)
                    .await
                {
                    Ok(cancel_status) => cancel_status,
                    Err(error) => {
                        tracing::warn!(event = "control.platform_image_import.cancel_unresolved", upload_id = %claim.upload_id,
                            diagnostic = %error, "Agent cancellation response is unresolved; lease recovery will retry");
                        return Ok(());
                    }
                }
            } else {
                match self
                    .agent
                    .get_platform_image_import(claim.upload_id, &empty_headers)
                    .await
                {
                    Ok(status) => status,
                    Err(error) => {
                        tracing::warn!(event = "control.platform_image_import.status_unresolved", upload_id = %claim.upload_id,
                            diagnostic = %error, "Agent status is unavailable; lease recovery will retry");
                        return Ok(());
                    }
                }
            };

            match status.state {
                PlatformImageImportJobState::Queued | PlatformImageImportJobState::Running => {
                    tokio::time::sleep(self.poll_interval).await;
                }
                PlatformImageImportJobState::Succeeded => {
                    let Some(catalog_id) = status.catalog_id else {
                        self.control
                            .fail_platform_image_import_fenced(
                                &claim,
                                "LW_CONTROL_PLATFORM_IMAGE_IMPORT_RESULT_INVALID",
                                timestamp()
                                    .map_err(|()| PlatformImageImportWorkerError::Invalid)?,
                            )
                            .await?;
                        return Ok(());
                    };
                    self.control
                        .finish_platform_image_import_fenced(
                            &claim,
                            catalog_id,
                            timestamp().map_err(|()| PlatformImageImportWorkerError::Invalid)?,
                        )
                        .await?;
                    return Ok(());
                }
                PlatformImageImportJobState::Failed => {
                    self.control
                        .fail_platform_image_import_fenced(
                            &claim,
                            status
                                .diagnostic
                                .as_deref()
                                .unwrap_or("LW_PLATFORM_IMAGE_IMPORT_FAILED"),
                            timestamp().map_err(|()| PlatformImageImportWorkerError::Invalid)?,
                        )
                        .await?;
                    return Ok(());
                }
                PlatformImageImportJobState::Cancelled => {
                    self.control
                        .cancel_platform_image_import_fenced(
                            &claim,
                            timestamp().map_err(|()| PlatformImageImportWorkerError::Invalid)?,
                        )
                        .await?;
                    return Ok(());
                }
            }
        }
    }

    async fn record_expired_upload_versions(
        &self,
        now: UtcTimestamp,
    ) -> Result<(), PlatformImageImportWorkerError> {
        let row = sqlx::query(
            r"SELECT upload_id,object_key
                 FROM control.platform_image_upload_sessions
                WHERE state IN ('imported','failed','cancelled')
                  AND expires_at<=$1
                  AND cleanup_versions_next_attempt_at<=$1
                ORDER BY cleanup_versions_next_attempt_at,expires_at,upload_id LIMIT 1",
        )
        .bind(now.get())
        .fetch_optional(&self.control.pool)
        .await
        .map_err(db)?;
        let Some(row) = row else {
            return Ok(());
        };
        let upload_id = parse_upload_id(&row)?;
        let key: String = row.try_get("object_key").map_err(db)?;
        // No transaction or row lock crosses S3. A duplicate scan is safe because the ledger
        // is keyed by exact object key and immutable version, including overwritten versions.
        let versions = match self.control.objects.list_key_versions(&key).await {
            Ok(versions) => versions,
            Err(error) => {
                sqlx::query(
                    r"UPDATE control.platform_image_upload_sessions
                          SET cleanup_versions_next_attempt_at=$2+($3*interval '1 second')
                        WHERE upload_id=$1",
                )
                .bind(upload_id.as_uuid())
                .bind(now.get())
                .bind(TERMINAL_UPLOAD_RETRY_SECONDS)
                .execute(&self.control.pool)
                .await
                .map_err(db)?;
                return Err(error.into());
            }
        };
        let mut transaction = self.control.pool.begin().await.map_err(db)?;
        for version in versions {
            if version.is_empty() || version == "null" {
                return Err(PlatformImageImportWorkerError::Invalid);
            }
            schedule_staged_archive_cleanup(&mut transaction, upload_id, &key, Some(&version))
                .await?;
        }
        sqlx::query(
            r"UPDATE control.platform_image_upload_sessions
                  SET cleanup_versions_next_attempt_at=$3+($4*interval '1 second')
                WHERE upload_id=$1 AND object_key=$2 AND expires_at<=$3
                  AND state IN ('imported','failed','cancelled')",
        )
        .bind(upload_id.as_uuid())
        .bind(&key)
        .bind(now.get())
        .bind(TERMINAL_UPLOAD_RECONCILE_SECONDS)
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        transaction.commit().await.map_err(db)?;
        Ok(())
    }
}

fn claim_from_row(
    row: &sqlx::postgres::PgRow,
    upload_id: UploadSessionId,
    lease_token: Uuid,
    cancel_requested: bool,
    store_binding: &str,
) -> Result<PlatformImageImportClaim, ControlError> {
    let actor_id = ActorId::from_str(
        &row.try_get::<Uuid, _>("created_by")
            .map_err(db)?
            .to_string(),
    )
    .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
    let archive_size = u64::try_from(row.try_get::<i64, _>("archive_bytes").map_err(db)?)
        .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
    let archive = match (
        row.try_get::<Option<Uuid>, _>("artifact_id").map_err(db)?,
        row.try_get::<Option<String>, _>("object_version")
            .map_err(db)?,
    ) {
        (Some(artifact_id), Some(object_version)) => Some(ArtifactRef {
            artifact_id: super::artifact_id_from_uuid(artifact_id)?,
            store_binding: store_binding.to_owned(),
            object_version,
            size_bytes: archive_size,
            media_type: row.try_get("archive_media_type").map_err(db)?,
        }),
        (None, None) => None,
        _ => return Err(ControlError::PersistenceIdentityMismatch),
    };
    let completion_key = row
        .try_get::<Option<String>, _>("completion_idempotency_key")
        .map_err(db)?
        .map(|value| IdempotencyKey::parse(&value))
        .transpose()
        .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
    let kind = platform_image_kind_from_str(&row.try_get::<String, _>("kind").map_err(db)?)?;
    let binding = row.try_get("binding").map_err(db)?;
    let target_reference = row.try_get("target_reference").map_err(db)?;
    let archive_object_key = row.try_get("object_key").map_err(db)?;
    let archive_media_type = row.try_get("archive_media_type").map_err(db)?;
    let disk_format = row
        .try_get::<Option<String>, _>("disk_format")
        .map_err(db)?
        .map(|value| super::parse_disk_format(&value))
        .transpose()?;
    let disk_path = row.try_get("disk_path").map_err(db)?;
    let capacity_bytes = row
        .try_get::<Option<i64>, _>("capacity_bytes")
        .map_err(db)?
        .map(|value| u64::try_from(value).map_err(|_| ControlError::PersistenceIdentityMismatch))
        .transpose()?;
    let trust_revision = u64::try_from(row.try_get::<i64, _>("trust_revision").map_err(db)?)
        .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
    let reason = row.try_get("reason").map_err(db)?;
    let expires_at = UtcTimestamp::from_utc(row.try_get("expires_at").map_err(db)?)
        .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
    Ok(PlatformImageImportClaim {
        upload_id,
        lease_token,
        actor_id,
        kind,
        binding,
        target_reference,
        archive_object_key,
        archive_size,
        archive_media_type,
        disk_format,
        disk_path,
        capacity_bytes,
        trust_revision,
        reason,
        completion_key,
        archive,
        cancel_requested,
        expires_at,
    })
}

impl PlatformImageImportClaim {
    fn import_request(&self) -> Result<InternalPlatformImageImportRequest, ControlError> {
        Ok(InternalPlatformImageImportRequest {
            kind: self.kind,
            binding: self.binding.clone(),
            target_reference: self.target_reference.clone(),
            archive: self
                .archive
                .clone()
                .ok_or(ControlError::OperationLeaseLost)?,
            archive_object_key: self.archive_object_key.clone(),
            disk_format: self.disk_format,
            disk_path: self.disk_path.clone(),
            capacity_bytes: self.capacity_bytes,
            trust_revision: self.trust_revision,
            actor_id: self.actor_id,
            reason: self.reason.clone(),
        })
    }
}

fn parse_upload_id(row: &sqlx::postgres::PgRow) -> Result<UploadSessionId, ControlError> {
    UploadSessionId::from_str(&row.try_get::<Uuid, _>("upload_id").map_err(db)?.to_string())
        .map_err(|_| ControlError::PersistenceIdentityMismatch)
}

fn timestamp() -> Result<UtcTimestamp, ()> {
    let value = time::OffsetDateTime::now_utc();
    let value = value
        .replace_nanosecond((value.nanosecond() / 1_000_000) * 1_000_000)
        .map_err(|_| ())?;
    UtcTimestamp::from_utc(value).map_err(|_| ())
}
