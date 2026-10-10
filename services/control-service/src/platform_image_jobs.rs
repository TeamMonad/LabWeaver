//! Durable Control-side orchestration for platform image imports.
//!
//! Control owns the upload session and its public state.  This worker only keeps the
//! short database lease, freezes the exact uploaded object version, and coordinates the
//! Agent-owned import job.  It never holds a database transaction while calling either
//! the object store or Agent.
#![allow(missing_docs)]

use std::str::FromStr;
use std::time::Duration;

use artifact_store::{ObjectStoreError, PlatformImageMultipartPartInput};
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
    pub multipart_upload_id: Option<String>,
    pub multipart_part_size_bytes: Option<u64>,
    pub multipart_part_count: Option<u32>,
    pub multipart_complete_started: bool,
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
                      completion_idempotency_key,expires_at,multipart_upload_id,
                      multipart_part_size_bytes,multipart_part_count,multipart_complete_started
               FROM control.platform_image_upload_sessions
              WHERE (state='queued' AND multipart_upload_id IS NOT NULL)
                 OR (state IN ('freezing','importing','cancelling')
                     AND multipart_upload_id IS NOT NULL
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

    /// Reads the client-supplied multipart manifest under the current worker fence.
    pub async fn platform_image_multipart_completion_parts(
        &self,
        claim: &PlatformImageImportClaim,
    ) -> Result<Vec<PlatformImageMultipartPartInput>, ControlError> {
        let rows = sqlx::query(
            "SELECT part_number,requested_etag \
               FROM control.platform_image_upload_parts WHERE upload_id=$1 ORDER BY part_number",
        )
        .bind(claim.upload_id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        let expected_count = usize::try_from(
            claim
                .multipart_part_count
                .ok_or(ControlError::PersistenceIdentityMismatch)?,
        )
        .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        if rows.len() != expected_count {
            return Err(ControlError::PersistenceIdentityMismatch);
        }
        rows.into_iter()
            .enumerate()
            .map(|(index, row)| {
                let part_number = u32::try_from(row.try_get::<i32, _>("part_number").map_err(db)?)
                    .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
                if part_number != u32::try_from(index + 1).unwrap_or_default() {
                    return Err(ControlError::PersistenceIdentityMismatch);
                }
                Ok(PlatformImageMultipartPartInput {
                    part_number,
                    etag: row
                        .try_get::<Option<String>, _>("requested_etag")
                        .map_err(db)?
                        .ok_or(ControlError::PersistenceIdentityMismatch)?,
                })
            })
            .collect()
    }

    /// Fences the transition from queued manifest validation to S3 completion.
    pub async fn mark_platform_image_multipart_complete_started(
        &self,
        claim: &PlatformImageImportClaim,
        now: UtcTimestamp,
    ) -> Result<(), ControlError> {
        if claim.multipart_complete_started {
            return Ok(());
        }
        let updated = sqlx::query(
            "UPDATE control.platform_image_upload_sessions \
                SET multipart_complete_started=true,revision=revision+1,updated_at=$3 \
              WHERE upload_id=$1 AND completion_lease_token=$2 \
                AND state IN ('freezing','cancelling') \
                AND completion_lease_expires_at>date_trunc('milliseconds',clock_timestamp()) \
                AND multipart_complete_started=false",
        )
        .bind(claim.upload_id.as_uuid())
        .bind(claim.lease_token)
        .bind(now.get())
        .execute(&self.pool)
        .await
        .map_err(db)?;
        if updated.rows_affected() != 1 {
            let current = sqlx::query_scalar::<_, bool>(
                "SELECT multipart_complete_started FROM control.platform_image_upload_sessions \
                  WHERE upload_id=$1 AND completion_lease_token=$2",
            )
            .bind(claim.upload_id.as_uuid())
            .bind(claim.lease_token)
            .fetch_optional(&self.pool)
            .await
            .map_err(db)?;
            if current != Some(true) {
                return Err(ControlError::OperationLeaseLost);
            }
        }
        Ok(())
    }

    /// Persists the exact S3 part facts before `CompleteMultipartUpload` is attempted.
    pub async fn record_platform_image_multipart_parts(
        &self,
        claim: &PlatformImageImportClaim,
        parts: &[artifact_store::PlatformImageMultipartPart],
        now: UtcTimestamp,
    ) -> Result<(), ControlError> {
        let lease_valid = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM control.platform_image_upload_sessions
               WHERE upload_id=$1 AND completion_lease_token=$2
                 AND state IN ('freezing','cancelling')
                 AND completion_lease_expires_at>date_trunc('milliseconds',clock_timestamp()))",
        )
        .bind(claim.upload_id.as_uuid())
        .bind(claim.lease_token)
        .fetch_one(&self.pool)
        .await
        .map_err(db)?;
        if !lease_valid {
            return Err(ControlError::OperationLeaseLost);
        }
        for part in parts {
            let updated = sqlx::query(
                "UPDATE control.platform_image_upload_parts \
                    SET etag=$3,observed_size_bytes=$4 \
                  WHERE upload_id=$1 AND part_number=$2 AND requested_etag=$3",
            )
            .bind(claim.upload_id.as_uuid())
            .bind(i32::try_from(part.part_number).map_err(|_| ControlError::ContractInvalid)?)
            .bind(&part.etag)
            .bind(i64::try_from(part.size_bytes).map_err(|_| ControlError::ContractInvalid)?)
            .execute(&self.pool)
            .await
            .map_err(db)?;
            if updated.rows_affected() != 1 {
                return Err(ControlError::ObjectStoreIdentityMismatch);
            }
        }
        self.renew_platform_image_import(claim, now).await
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
        // An uncompleted multipart upload is cancelled through the worker so S3 receives an
        // abort for this exact upload identity. A completion accepted before expiry continues
        // independently of the browser URL lifetime.
        self.control
            .recover_pending_platform_image_upload_creation()
            .await?;
        let mut expiration_transaction = self.control.pool.begin().await.map_err(db)?;
        // Pending creation rows without a persisted multipart ID are recovered above.  Their
        // exact object key is the only safe scope for discovery and abort, so never terminalize
        // them with a bulk SQL update that would lose an accepted S3 multipart upload.
        sqlx::query(
            r"UPDATE control.platform_image_upload_sessions
                  SET state='queued',cancel_requested=true,revision=revision+1,updated_at=$1
                WHERE state='pending' AND multipart_upload_id IS NOT NULL AND expires_at<=$1",
        )
        .bind(now.get())
        .execute(&mut *expiration_transaction)
        .await
        .map_err(db)?;
        expiration_transaction.commit().await.map_err(db)?;
        self.record_expired_upload_versions(now).await?;
        let Some(mut claim) = self.control.claim_platform_image_import(now).await? else {
            return Ok(());
        };
        let had_archive_reference = claim.archive.is_some();
        if claim.archive.is_none() {
            let multipart_upload_id = claim
                .multipart_upload_id
                .clone()
                .ok_or(PlatformImageImportWorkerError::Invalid)?;
            if claim.cancel_requested && !claim.multipart_complete_started {
                match self
                    .control
                    .objects
                    .abort_platform_image_multipart_upload(
                        &claim.archive_object_key,
                        &multipart_upload_id,
                    )
                    .await
                {
                    Ok(()) | Err(ObjectStoreError::ObjectNotFound) => {
                        self.control
                            .cancel_platform_image_import_fenced(
                                &claim,
                                timestamp()
                                    .map_err(|()| PlatformImageImportWorkerError::Invalid)?,
                            )
                            .await?;
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
            let parts = self
                .control
                .platform_image_multipart_completion_parts(&claim)
                .await?;
            let completion_started_before = claim.multipart_complete_started;
            if !claim.multipart_complete_started {
                self.control
                    .mark_platform_image_multipart_complete_started(
                        &claim,
                        timestamp().map_err(|()| PlatformImageImportWorkerError::Invalid)?,
                    )
                    .await?;
                claim.multipart_complete_started = true;
            }
            let observed = match self
                .control
                .objects
                .list_platform_image_multipart_parts(
                    &claim.archive_object_key,
                    &multipart_upload_id,
                )
                .await
            {
                Ok(parts) => parts,
                Err(ObjectStoreError::ObjectNotFound) if claim.multipart_complete_started => {
                    Vec::new()
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
            if observed.is_empty() && !completion_started_before {
                self.control
                    .fail_platform_image_import_fenced(
                        &claim,
                        ObjectStoreError::ObjectIdentityMismatch.diagnostic_code(),
                        timestamp().map_err(|()| PlatformImageImportWorkerError::Invalid)?,
                    )
                    .await?;
                return Ok(());
            }
            if !observed.is_empty()
                && let Err(error) = self
                    .control
                    .record_platform_image_multipart_parts(
                        &claim,
                        &observed,
                        timestamp().map_err(|()| PlatformImageImportWorkerError::Invalid)?,
                    )
                    .await
            {
                if matches!(error, ControlError::ObjectStoreIdentityMismatch) {
                    self.control
                        .fail_platform_image_import_fenced(
                            &claim,
                            ObjectStoreError::ObjectIdentityMismatch.diagnostic_code(),
                            timestamp().map_err(|()| PlatformImageImportWorkerError::Invalid)?,
                        )
                        .await?;
                    return Ok(());
                }
                return Err(error.into());
            }
            if !observed.is_empty() {
                self.control
                    .objects
                    .complete_platform_image_multipart_upload(
                        &claim.archive_object_key,
                        &multipart_upload_id,
                        claim.archive_size,
                        &parts,
                    )
                    .await
                    .map_err(|error| match error {
                        ObjectStoreError::ObjectUnavailable => {
                            PlatformImageImportWorkerError::Objects(error)
                        }
                        error => PlatformImageImportWorkerError::Objects(error),
                    })?;
            }
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
                Err(ObjectStoreError::ObjectNotFound | ObjectStoreError::ObjectUnavailable) => {
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
    let multipart_upload_id = row
        .try_get::<Option<String>, _>("multipart_upload_id")
        .map_err(db)?;
    let multipart_part_size_bytes = row
        .try_get::<Option<i64>, _>("multipart_part_size_bytes")
        .map_err(db)?
        .map(|value| u64::try_from(value).map_err(|_| ControlError::PersistenceIdentityMismatch))
        .transpose()?;
    let multipart_part_count = row
        .try_get::<Option<i32>, _>("multipart_part_count")
        .map_err(db)?
        .map(|value| u32::try_from(value).map_err(|_| ControlError::PersistenceIdentityMismatch))
        .transpose()?;
    let multipart_complete_started = row.try_get("multipart_complete_started").map_err(db)?;
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
        multipart_upload_id,
        multipart_part_size_bytes,
        multipart_part_count,
        multipart_complete_started,
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
