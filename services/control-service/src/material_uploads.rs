//! Recovery and cleanup of material uploads under their original authority deadline.

use std::str::FromStr;

use artifact_store::ObjectStoreError;
use contracts::http::{
    IdempotencyKey, ProblemPackageUploadFile, ProblemPackageUploadSession,
    ProblemPackageUploadState, ProblemPackageUploadTarget,
};
use contracts::{CourseId, ProblemPackageId, ProjectId, Revision, UploadSessionId, UtcTimestamp};
use persistence_sqlx::{Domain, IdempotencyDecision, IdempotencyStore};
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

use super::{ControlError, ControlService, canonical_hash, db, i64_revision, revision_from_i64};

const CANCEL_UPLOAD: &str = "control_cancel_problem_package_upload_v1";

impl ControlService {
    /// Queries one upload in its exact project and reissues only missing file authorities.
    pub async fn project_upload(
        &self,
        project_id: ProjectId,
        upload_id: UploadSessionId,
        now: UtcTimestamp,
    ) -> Result<ProblemPackageUploadSession, ControlError> {
        sqlx::query(
            "UPDATE control.problem_package_upload_sessions SET state='expired',revision=revision+1,\
             terminal_diagnostic='LW_PACKAGE_UPLOAD_EXPIRED',completion_lease_token=NULL,completion_lease_expires_at=NULL,updated_at=now() \
             WHERE upload_id=$1 AND project_id=$2 AND expires_at<=now() \
             AND (state='pending' OR (state='completing' AND completion_lease_expires_at<=now()))",
        )
        .bind(upload_id.as_uuid()).bind(project_id.as_uuid())
        .execute(&self.pool).await.map_err(db)?;
        let row = sqlx::query(
            "SELECT course_id,revision,state,expires_at,completed_package_id,terminal_diagnostic,\
             completion_idempotency_key,completion_expected_revision \
             FROM control.problem_package_upload_sessions WHERE upload_id=$1 AND project_id=$2",
        )
        .bind(upload_id.as_uuid())
        .bind(project_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?
        .ok_or(ControlError::UploadNotFound)?;
        let state = match row.try_get::<String, _>("state").map_err(db)?.as_str() {
            "pending" => ProblemPackageUploadState::Pending,
            "completing" => ProblemPackageUploadState::Completing,
            "completed" => ProblemPackageUploadState::Completed,
            "failed" => ProblemPackageUploadState::Failed,
            "cancelled" => ProblemPackageUploadState::Cancelled,
            "expired" => ProblemPackageUploadState::Expired,
            _ => return Err(ControlError::PersistenceIdentityMismatch),
        };
        let expires_at = UtcTimestamp::from_utc(row.try_get("expires_at").map_err(db)?)
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        let rows = sqlx::query(
            "SELECT path,object_key,size_bytes,media_type FROM control.problem_package_upload_files \
             WHERE upload_id=$1 ORDER BY ordinal",
        ).bind(upload_id.as_uuid()).fetch_all(&self.pool).await.map_err(db)?;
        let mut files = Vec::with_capacity(rows.len());
        let mut upload_targets = Vec::new();
        let mut uploaded_paths = Vec::new();
        for file_row in rows {
            let file = ProblemPackageUploadFile {
                path: file_row.try_get("path").map_err(db)?,
                size_bytes: u64::try_from(file_row.try_get::<i64, _>("size_bytes").map_err(db)?)
                    .map_err(|_| ControlError::PersistenceIdentityMismatch)?,
                media_type: file_row.try_get("media_type").map_err(db)?,
            };
            if state == ProblemPackageUploadState::Pending && expires_at.get() > now.get() {
                let key: String = file_row.try_get("object_key").map_err(db)?;
                match self
                    .objects
                    .freeze_current_reference(&key, file.size_bytes, &file.media_type)
                    .await
                {
                    Ok(_) => uploaded_paths.push(file.path.clone()),
                    Err(ObjectStoreError::ObjectNotFound) => {
                        let signed = self
                            .objects
                            .presign_upload_exact_until(
                                &key,
                                file.size_bytes,
                                &file.media_type,
                                now,
                                expires_at,
                            )
                            .await?;
                        upload_targets.push(ProblemPackageUploadTarget {
                            path: file.path.clone(),
                            upload_url: signed.url,
                            required_headers: signed.required_headers,
                            expires_at: signed.expires_at,
                        });
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            files.push(file);
        }
        let completed_package = row
            .try_get::<Option<Uuid>, _>("completed_package_id")
            .map_err(db)?
            .map(|id| ProblemPackageId::from_str(&id.to_string()))
            .transpose()
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        let completed_package = if let Some(package_id) = completed_package {
            Some(self.project_package(project_id, package_id).await?)
        } else {
            None
        };
        Ok(ProblemPackageUploadSession {
            id: upload_id,
            project_id,
            course_id: row
                .try_get::<Option<Uuid>, _>("course_id")
                .map_err(db)?
                .map(|id| CourseId::from_str(&id.to_string()))
                .transpose()
                .map_err(|_| ControlError::PersistenceIdentityMismatch)?,
            revision: revision_from_i64(row.try_get("revision").map_err(db)?)?,
            files,
            upload_targets,
            expires_at,
            state,
            uploaded_paths,
            completed_package,
            terminal_diagnostic: row.try_get("terminal_diagnostic").map_err(db)?,
            completion_idempotency_key: row.try_get("completion_idempotency_key").map_err(db)?,
            completion_expected_revision: row
                .try_get::<Option<i64>, _>("completion_expected_revision")
                .map_err(db)?
                .map(revision_from_i64)
                .transpose()?,
        })
    }

    /// Cancels pending or completing work with revision fencing and a durable replay result.
    pub async fn cancel_project_upload(
        &self,
        project_id: ProjectId,
        upload_id: UploadSessionId,
        expected_revision: Revision,
        idempotency_key: &IdempotencyKey,
        now: UtcTimestamp,
    ) -> Result<ProblemPackageUploadSession, ControlError> {
        let request_hash = canonical_hash(&json!({
            "projectId":project_id,"uploadId":upload_id,"expectedRevision":expected_revision,
        }))?;
        let mut transaction = self.pool.begin().await.map_err(db)?;
        match IdempotencyStore::reserve(
            &mut transaction,
            Domain::Control,
            CANCEL_UPLOAD,
            idempotency_key.as_str(),
            request_hash,
        )
        .await
        .map_err(|_| ControlError::PersistenceFailed)?
        {
            IdempotencyDecision::Replay(value) => {
                return serde_json::from_value(value)
                    .map_err(|_| ControlError::PersistenceIdentityMismatch);
            }
            IdempotencyDecision::Conflict => return Err(ControlError::IdempotencyConflict),
            IdempotencyDecision::InProgress => return Err(ControlError::OperationInProgress),
            IdempotencyDecision::Reserved => {}
        }
        let updated = sqlx::query(
            "UPDATE control.problem_package_upload_sessions \
             SET state='cancelled',revision=revision+1,terminal_diagnostic='LW_PACKAGE_UPLOAD_CANCELLED',\
                 completion_lease_token=NULL,completion_lease_expires_at=NULL,updated_at=now() \
             WHERE upload_id=$1 AND project_id=$2 AND revision=$3 AND state IN ('pending','completing')",
        ).bind(upload_id.as_uuid()).bind(project_id.as_uuid()).bind(i64_revision(expected_revision)?)
        .execute(&mut *transaction).await.map_err(db)?;
        if updated.rows_affected() != 1 {
            return Err(ControlError::UploadStateConflict);
        }
        // No authority is returned for this state. Read through this transaction before commit.
        let row = sqlx::query(
            "SELECT course_id,revision,expires_at FROM control.problem_package_upload_sessions WHERE upload_id=$1",
        ).bind(upload_id.as_uuid()).fetch_one(&mut *transaction).await.map_err(db)?;
        let file_rows = sqlx::query(
            "SELECT path,size_bytes,media_type FROM control.problem_package_upload_files WHERE upload_id=$1 ORDER BY ordinal",
        ).bind(upload_id.as_uuid()).fetch_all(&mut *transaction).await.map_err(db)?;
        let files = file_rows
            .iter()
            .map(|row| {
                Ok(ProblemPackageUploadFile {
                    path: row.try_get("path").map_err(db)?,
                    size_bytes: u64::try_from(row.try_get::<i64, _>("size_bytes").map_err(db)?)
                        .map_err(|_| ControlError::PersistenceIdentityMismatch)?,
                    media_type: row.try_get("media_type").map_err(db)?,
                })
            })
            .collect::<Result<Vec<_>, ControlError>>()?;
        let session = ProblemPackageUploadSession {
            id: upload_id,
            project_id,
            course_id: row
                .try_get::<Option<Uuid>, _>("course_id")
                .map_err(db)?
                .map(|id| CourseId::from_str(&id.to_string()))
                .transpose()
                .map_err(|_| ControlError::PersistenceIdentityMismatch)?,
            revision: revision_from_i64(row.try_get("revision").map_err(db)?)?,
            files,
            upload_targets: Vec::new(),
            uploaded_paths: Vec::new(),
            expires_at: UtcTimestamp::from_utc(row.try_get("expires_at").map_err(db)?)
                .map_err(|_| ControlError::PersistenceIdentityMismatch)?,
            state: ProblemPackageUploadState::Cancelled,
            completed_package: None,
            terminal_diagnostic: Some("LW_PACKAGE_UPLOAD_CANCELLED".to_owned()),
            completion_idempotency_key: None,
            completion_expected_revision: None,
        };
        IdempotencyStore::complete(
            &mut transaction,
            Domain::Control,
            CANCEL_UPLOAD,
            idempotency_key.as_str(),
            &serde_json::to_value(&session).map_err(|_| ControlError::ContractInvalid)?,
        )
        .await
        .map_err(|_| ControlError::PersistenceFailed)?;
        transaction.commit().await.map_err(db)?;
        tracing::info!(event="control.problem_package.cancelled", %upload_id, %project_id, at=%now);
        Ok(session)
    }

    /// Expires abandoned uploads and schedules exact orphan versions after upload authority ends.
    pub async fn reconcile_material_uploads(&self, now: UtcTimestamp) -> Result<(), ControlError> {
        sqlx::query(
            "UPDATE control.problem_package_upload_sessions SET state='expired',revision=revision+1,\
             terminal_diagnostic='LW_PACKAGE_UPLOAD_EXPIRED',completion_lease_token=NULL,completion_lease_expires_at=NULL,updated_at=now() \
             WHERE expires_at<=$1 AND (state='pending' OR (state='completing' AND completion_lease_expires_at<=$1))",
        ).bind(now.get()).execute(&self.pool).await.map_err(db)?;
        let upload_id = sqlx::query_scalar::<_,Uuid>(
            "UPDATE control.problem_package_upload_sessions SET cleanup_versions_next_attempt_at=$1+interval '5 minutes' \
             WHERE upload_id=(SELECT upload_id FROM control.problem_package_upload_sessions \
                 WHERE state IN ('failed','expired','cancelled') AND expires_at<=$1 \
                 AND cleanup_versions_next_attempt_at<=$1 ORDER BY cleanup_versions_next_attempt_at \
                 LIMIT 1 FOR UPDATE SKIP LOCKED) RETURNING upload_id",
        ).bind(now.get()).fetch_optional(&self.pool).await.map_err(db)?;
        let Some(upload_id) = upload_id else {
            return Ok(());
        };
        let keys = sqlx::query_scalar::<_,String>(
            "SELECT object_key FROM control.problem_package_upload_files WHERE upload_id=$1 ORDER BY ordinal",
        ).bind(upload_id).fetch_all(&self.pool).await.map_err(db)?;
        for key in keys {
            let versions = match self.objects.list_key_versions(&key).await {
                Ok(versions) => versions,
                Err(error) => {
                    tracing::warn!(event="control.problem_package.cleanup_retry", %upload_id,
                        diagnostic_code=%error);
                    continue;
                }
            };
            for version in versions {
                sqlx::query(
                    "INSERT INTO control.object_cleanup_ledger(object_key,object_version,upload_id) \
                     VALUES ($1,$2,$3) ON CONFLICT DO NOTHING",
                ).bind(&key).bind(version).bind(upload_id).execute(&self.pool).await.map_err(db)?;
            }
        }
        Ok(())
    }
}
