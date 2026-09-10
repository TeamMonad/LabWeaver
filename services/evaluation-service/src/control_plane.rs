//! PostgreSQL-authoritative Evaluation release, run and `StepRun` control plane.
#![allow(
    clippy::needless_pass_by_value,
    clippy::useless_conversion,
    clippy::all,
    dead_code,
    unused,
    unused_imports,
    missing_docs,
    clippy::missing_errors_doc,
    clippy::too_many_lines,
    reason = "the transaction fences and state derivation are intentionally colocated"
)]

use persistence_sqlx::Sha256Digest;
use std::{collections::BTreeSet, str::FromStr, time::Duration}; // internal persistence hash, not contract hash

use contracts::{
    ActorId, CourseId, DiagnosticCode, EvaluationReleaseId, EvaluationRunId, EvaluationStepRunId,
    EventId, ProjectId, Revision, Sequence, TaskRunId, UtcTimestamp,
    evaluation::{
        EVALUATION_RELEASE_SCHEMA_VERSION, EVALUATION_RUN_SCHEMA_VERSION,
        EvaluationExecutionBinding, EvaluationRelease, EvaluationReleaseState, EvaluationRun,
        EvaluationRunState, EvaluationRuntimeIdentity, EvaluationStepCompletion,
        EvaluationStepFailurePolicy, EvaluationStepRole, EvaluationStepRun, EvaluationStepRunState,
        StudentEvaluationResult,
    },
    events::{
        CloudEvent, EVENT_CONTRACTS, EvaluationReleasePublished, EvaluationRunEvent,
        EvaluationStepRunEvent, EventContract, SPEC_VERSION, subjects,
    },
    http::{
        AuthoringPublicationAdmissionBinding, CursorPage, IdempotencyKey,
        InternalCreateEvaluationRunRequest, InternalEvaluationRunMutationRequest,
        InternalPublishEvaluationReleaseRequest, InternalWithdrawEvaluationReleaseRequest,
        RecordResourceUsageRequest,
    },
};
use persistence_sqlx::{Domain, IdempotencyDecision, IdempotencyStore, OutboxStore};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

const PUBLISH_RELEASE_OPERATION: &str = "publish_evaluation_release_v1";
const WITHDRAW_RELEASE_OPERATION: &str = "withdraw_evaluation_release_v1";
const CREATE_RUN_OPERATION: &str = "create_evaluation_run_v1";
const CANCEL_RUN_OPERATION: &str = "cancel_evaluation_run_v1";
const RETRY_STEP_OPERATION: &str = "retry_evaluation_step_v1";
const VERIFY_STEP_CLEANUP_OPERATION: &str = "verify_evaluation_step_cleanup_v1";

/// Result of publishing a release through the idempotency ledger.
#[derive(Clone, Debug, PartialEq)]
pub enum EvaluationReleaseReservation {
    Created(EvaluationRelease),
    Replayed(EvaluationRelease),
}

/// Result of creating a run through the idempotency ledger.
#[derive(Clone, Debug, PartialEq)]
pub enum EvaluationRunReservation {
    Created(EvaluationRun),
    Replayed(EvaluationRun),
}

/// Fenced `StepRun` lease owned by exactly one worker attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EvaluationStepLease {
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub run_id: EvaluationRunId,
    pub step_run_id: EvaluationStepRunId,
    /// Durable one-shot Resource task identity for this exact step attempt.
    pub task_run_id: TaskRunId,
    pub step_id: String,
    pub role: EvaluationStepRole,
    pub max_score: u32,
    pub attempt: u32,
    pub worker_id: String,
    /// Revision of the running `StepRun` held by this lease.
    pub revision: Revision,
    pub runtime_identity: EvaluationRuntimeIdentity,
    pub trace_id: String,
    lease_token: Uuid,
}

impl EvaluationStepLease {
    #[must_use]
    pub const fn lease_token(&self) -> Uuid {
        self.lease_token
    }
}

/// Versioned, payload-free description of the Kubernetes objects owned by one execution attempt.
///
/// This is deliberately separate from the rendered Job bundle.  The latter contains command
/// `ConfigMaps` and short-lived signed materializer data and must never be persisted as recovery
/// state.  Recovery uses these references to read the live object, checks its UID and ownership
/// labels, and only then performs a deletion.
pub const EVALUATION_EXECUTION_RESOURCES_SCHEMA_VERSION: &str =
    "evaluation.labweaver.io/execution-resources/v1";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationExecutionKind {
    Program,
    AnsibleProbe,
    LlmReview,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvaluationExecutionObjectRef {
    pub api_version: String,
    /// Kubernetes collection name, such as `jobs` or `configmaps`.
    pub resource: String,
    pub name: String,
    pub uid: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvaluationExecutionResources {
    pub schema_version: String,
    pub run_id: EvaluationRunId,
    pub step_run_id: EvaluationStepRunId,
    pub task_run_id: TaskRunId,
    pub namespace: String,
    pub kind: EvaluationExecutionKind,
    /// Immutable, payload-free request consumed by the concrete executor.
    /// Materializer URLs and SSH private/signed credentials are intentionally
    /// kept out of this checkpoint and remain Kubernetes Secret data.
    pub request: serde_json::Value,
    pub objects: Vec<EvaluationExecutionObjectRef>,
}

impl EvaluationExecutionResources {
    /// Validates the bounded recovery references against one leased attempt.
    pub fn validate_for(
        &self,
        run_id: EvaluationRunId,
        step_run_id: EvaluationStepRunId,
        task_run_id: TaskRunId,
    ) -> Result<(), EvaluationControlStoreError> {
        if self.schema_version != EVALUATION_EXECUTION_RESOURCES_SCHEMA_VERSION
            || self.run_id != run_id
            || self.step_run_id != step_run_id
            || self.task_run_id != task_run_id
            || !valid_execution_namespace_value(&self.namespace)
            || !self.request.is_object()
            // An empty object list is the durable pre-start intent.  It is
            // upgraded to the complete UID set immediately after the
            // executor applies the immutable bundle.
            || self.objects.len() > 8
        {
            return Err(EvaluationControlStoreError::ContractInvalid);
        }
        let mut identities = BTreeSet::new();
        for object in &self.objects {
            let expected_api_version = match object.resource.as_str() {
                "jobs" => "batch/v1",
                "networkpolicies" => "networking.k8s.io/v1",
                "configmaps" | "secrets" => "v1",
                _ => return Err(EvaluationControlStoreError::ContractInvalid),
            };
            if object.api_version != expected_api_version
                || !valid_kubernetes_name(&object.name)
                || object.uid.is_empty()
                || object.uid.len() > 128
                || object.uid.chars().any(char::is_control)
                || !identities.insert((object.resource.as_str(), object.name.as_str()))
            {
                return Err(EvaluationControlStoreError::ContractInvalid);
            }
        }
        let encoded =
            serde_json::to_vec(self).map_err(|_| EvaluationControlStoreError::ContractInvalid)?;
        if encoded.len() > 64 * 1024 {
            return Err(EvaluationControlStoreError::ContractInvalid);
        }
        Ok(())
    }
}

/// Durable execution checkpoint used to resume observation, billing delivery, and cleanup after
/// a worker restart.
#[derive(Clone, Debug, PartialEq)]
pub struct EvaluationExecutionCheckpoint {
    pub execution_started_at: Option<UtcTimestamp>,
    pub execution_terminated_at: Option<UtcTimestamp>,
    pub terminal_completion: Option<EvaluationStepCompletion>,
    pub execution_resources: Option<EvaluationExecutionResources>,
}

fn valid_execution_namespace_value(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !value.starts_with('-')
        && !value.ends_with('-')
}

fn valid_kubernetes_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 253
        && value.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
                && label
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
        })
}

/// Evaluation-owned `PostgreSQL` repository.
#[derive(Clone, Debug)]
pub struct PgEvaluationControlStore {
    pool: PgPool,
}

impl PgEvaluationControlStore {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn authority_now(&self) -> Result<UtcTimestamp, EvaluationControlStoreError> {
        let value: time::OffsetDateTime =
            sqlx::query_scalar("SELECT date_trunc('milliseconds', clock_timestamp())")
                .fetch_one(&self.pool)
                .await?;
        UtcTimestamp::from_utc(value).map_err(|_| EvaluationControlStoreError::ClockInvalid)
    }

    pub async fn publish_release(
        &self,
        request: &InternalPublishEvaluationReleaseRequest,
        idempotency_key: &IdempotencyKey,
        now: UtcTimestamp,
        trace_id: &str,
    ) -> Result<EvaluationReleaseReservation, EvaluationControlStoreError> {
        request
            .validate()
            .map_err(|_| EvaluationControlStoreError::ContractInvalid)?;
        validate_trace(trace_id)?;
        let request_sha256 = Sha256Digest::of_canonical(request)
            .map_err(|_| EvaluationControlStoreError::ContractInvalid)?;
        let spec_sha256 = Sha256Digest::of_canonical(&request.evaluation_spec)
            .map_err(|_| EvaluationControlStoreError::ContractInvalid)?;
        let runtime_identity_sha256 = Sha256Digest::of_canonical(&request.runtime_identity)
            .map_err(|_| EvaluationControlStoreError::ContractInvalid)?;
        let execution_binding = serde_json::to_value(&request.execution_binding)
            .map_err(|_| EvaluationControlStoreError::ContractInvalid)?;
        let mut transaction = self.pool.begin().await?;
        match IdempotencyStore::reserve(
            &mut transaction,
            Domain::Evaluation,
            PUBLISH_RELEASE_OPERATION,
            idempotency_key.as_str(),
            request_sha256,
        )
        .await?
        {
            IdempotencyDecision::Replay(value) => {
                let release = decode_release(value)?;
                transaction.rollback().await?;
                tracing::info!(
                    event = "evaluation.release.publish_replayed",
                    course_id = ?release.course_id,
                    candidate_id = %release.candidate_id,
                    approval_id = %release.approval_id,
                    release_id = %release.id,
                    revision = release.revision.get(),
                    idempotency_replay = true,
                    trace_id,
                );
                return Ok(EvaluationReleaseReservation::Replayed(release));
            }
            IdempotencyDecision::Conflict => {
                transaction.rollback().await?;
                return Err(EvaluationControlStoreError::IdempotencyConflict);
            }
            IdempotencyDecision::InProgress => {
                transaction.rollback().await?;
                return Err(EvaluationControlStoreError::RequestInProgress);
            }
            IdempotencyDecision::Reserved => {}
        }
        let release = EvaluationRelease {
            schema_version: EVALUATION_RELEASE_SCHEMA_VERSION.to_owned(),
            id: EvaluationReleaseId::new(),
            project_id: request.project_id,
            course_id: request.course_id,
            candidate_id: request.candidate_id,
            candidate_revision: request.candidate_revision,
            approval_id: request.approval_id,
            approval_revision: request.approval_revision,
            evaluation_spec: request.evaluation_spec.clone(),
            runtime_identity: request.runtime_identity.clone(),
            state: EvaluationReleaseState::Active,
            revision: Revision::new(1).map_err(|_| EvaluationControlStoreError::ContractInvalid)?,
            published_by: request.published_by,
            published_at: now,
            withdrawn_at: None,
            withdrawal_diagnostic_code: None,
        };
        let release_identity_sha256 =
            Sha256Digest::of_canonical(&(&release, &request.execution_binding))
                .map_err(|_| EvaluationControlStoreError::ContractInvalid)?;
        let contract = serde_json::to_value(&release)
            .map_err(|_| EvaluationControlStoreError::ContractInvalid)?;
        sqlx::query(
            "INSERT INTO evaluation.evaluation_releases \
            (release_id,project_id,course_id,candidate_id,candidate_revision,approval_id,\
              approval_revision,evaluation_spec_sha256,runtime_identity_sha256,\
              release_identity_sha256,execution_binding,state,revision,contract,published_by,published_at,updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,'active',$12,$13,$14,$15,$15)",
        )
        .bind(release.id.as_uuid())
        .bind(release.project_id.as_uuid())
        .bind(release.course_id.map(CourseId::as_uuid))
        .bind(release.candidate_id.as_uuid())
        .bind(revision_i64(release.candidate_revision)?)
        .bind(release.approval_id.as_uuid())
        .bind(revision_i64(release.approval_revision)?)
        .bind(spec_sha256.to_string())
        .bind(runtime_identity_sha256.to_string())
        .bind(release_identity_sha256.to_string())
        .bind(&execution_binding)
        .bind(revision_i64(release.revision)?)
        .bind(&contract)
        .bind(release.published_by.as_uuid())
        .bind(now.get())
        .execute(&mut *transaction)
        .await
        .map_err(map_unique)?;
        enqueue_release_published(&mut transaction, &release, now, trace_id).await?;
        IdempotencyStore::complete(
            &mut transaction,
            Domain::Evaluation,
            PUBLISH_RELEASE_OPERATION,
            idempotency_key.as_str(),
            &contract,
        )
        .await?;
        transaction.commit().await?;
        release.validate()?;
        tracing::info!(
            event = "evaluation.release.published",
            course_id = ?release.course_id,
            candidate_id = %release.candidate_id,
            approval_id = %release.approval_id,
            release_id = %release.id,
            revision = release.revision.get(),
            idempotency_replay = false,
            trace_id,
        );
        Ok(EvaluationReleaseReservation::Created(release))
    }

    pub async fn load_release(
        &self,
        release_id: EvaluationReleaseId,
    ) -> Result<EvaluationRelease, EvaluationControlStoreError> {
        let value: Value = sqlx::query_scalar(
            "SELECT contract FROM evaluation.evaluation_releases WHERE release_id=$1",
        )
        .bind(release_id.as_uuid())
        .fetch_optional(&self.pool)
        .await?
        .ok_or(EvaluationControlStoreError::ReleaseNotFound)?;
        decode_release(value)
    }

    /// Loads the private package and object locator binding for an immutable release.
    ///
    /// The binding is intentionally unavailable through the public release projection, but
    /// Evaluation-owned materializers must read this persisted value rather than resolving a
    /// mutable package or object-store listing.
    pub async fn load_release_execution_binding(
        &self,
        release_id: EvaluationReleaseId,
    ) -> Result<EvaluationExecutionBinding, EvaluationControlStoreError> {
        let value: Value = sqlx::query_scalar(
            "SELECT execution_binding FROM evaluation.evaluation_releases WHERE release_id=$1",
        )
        .bind(release_id.as_uuid())
        .fetch_optional(&self.pool)
        .await?
        .ok_or(EvaluationControlStoreError::ReleaseNotFound)?;
        let binding: EvaluationExecutionBinding = serde_json::from_value(value)
            .map_err(|_| EvaluationControlStoreError::ContractInvalid)?;
        binding
            .validate()
            .map_err(|_| EvaluationControlStoreError::ContractInvalid)?;
        Ok(binding)
    }

    pub async fn list_releases(
        &self,
        project_id: ProjectId,
        course_id: Option<CourseId>,
        cursor: Option<EvaluationReleaseId>,
        limit: u16,
    ) -> Result<CursorPage<EvaluationRelease>, EvaluationControlStoreError> {
        if !(1..=100).contains(&limit) {
            return Err(EvaluationControlStoreError::ContractInvalid);
        }
        if let Some(cursor) = cursor {
            let exists = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM evaluation.evaluation_releases \
                 WHERE project_id=$1 AND ($2::uuid IS NULL OR course_id=$2) \
                   AND state='active' AND release_id=$3)",
            )
            .bind(project_id.as_uuid())
            .bind(course_id.map(CourseId::as_uuid))
            .bind(cursor.as_uuid())
            .fetch_one(&self.pool)
            .await?;
            if !exists {
                return Err(EvaluationControlStoreError::ContractInvalid);
            }
        }
        let values = sqlx::query_scalar::<_, Value>(
            "SELECT contract FROM evaluation.evaluation_releases \
             WHERE project_id=$1 AND ($2::uuid IS NULL OR course_id=$2) AND state='active' \
               AND ($3::uuid IS NULL OR (published_at,release_id) < \
               (SELECT published_at,release_id FROM evaluation.evaluation_releases \
                WHERE project_id=$1 AND ($2::uuid IS NULL OR course_id=$2) \
                  AND state='active' AND release_id=$3)) \
             ORDER BY published_at DESC,release_id DESC LIMIT $4",
        )
        .bind(project_id.as_uuid())
        .bind(course_id.map(CourseId::as_uuid))
        .bind(cursor.map(contracts::EvaluationReleaseId::as_uuid))
        .bind(i64::from(limit) + 1)
        .fetch_all(&self.pool)
        .await?;
        let mut items = values
            .into_iter()
            .map(decode_release)
            .collect::<Result<Vec<_>, _>>()?;
        let has_more = items.len() > usize::from(limit);
        items.truncate(usize::from(limit));
        let next_cursor = if has_more {
            items.last().map(|release| release.id.to_string())
        } else {
            None
        };
        Ok(CursorPage { items, next_cursor })
    }

    pub async fn withdraw_release(
        &self,
        release_id: EvaluationReleaseId,
        request: &InternalWithdrawEvaluationReleaseRequest,
        idempotency_key: &IdempotencyKey,
        now: UtcTimestamp,
        trace_id: &str,
    ) -> Result<EvaluationRelease, EvaluationControlStoreError> {
        validate_trace(trace_id)?;
        let request_sha256 = Sha256Digest::of_canonical(&serde_json::json!({
            "releaseId": release_id,
            "request": request,
        }))
        .map_err(|_| EvaluationControlStoreError::ContractInvalid)?;
        let mut transaction = self.pool.begin().await?;
        match IdempotencyStore::reserve(
            &mut transaction,
            Domain::Evaluation,
            WITHDRAW_RELEASE_OPERATION,
            idempotency_key.as_str(),
            request_sha256,
        )
        .await?
        {
            IdempotencyDecision::Replay(value) => {
                let release = decode_release(value)?;
                transaction.rollback().await?;
                tracing::info!(
                    event = "evaluation.release.withdraw_replayed",
                    project_id = %release.project_id,
                    course_id = ?release.course_id,
                    release_id = %release.id,
                    revision = release.revision.get(),
                    actor_id = %request.withdrawn_by,
                    diagnostic = request.reason_code.as_str(),
                    idempotency_replay = true,
                    trace_id,
                );
                return Ok(release);
            }
            IdempotencyDecision::Conflict => {
                return Err(EvaluationControlStoreError::IdempotencyConflict);
            }
            IdempotencyDecision::InProgress => {
                return Err(EvaluationControlStoreError::RequestInProgress);
            }
            IdempotencyDecision::Reserved => {}
        }
        let mut release = load_release_for_update(&mut transaction, release_id).await?;
        if release.project_id != request.project_id || release.course_id != request.course_id {
            return Err(EvaluationControlStoreError::CourseMismatch);
        }
        if release.revision != request.expected_revision {
            return Err(EvaluationControlStoreError::RevisionConflict);
        }
        if release.state != EvaluationReleaseState::Active {
            return Err(EvaluationControlStoreError::StateConflict);
        }
        release.state = EvaluationReleaseState::Withdrawn;
        release.revision = next_revision(release.revision)?;
        release.withdrawn_at = Some(now);
        release.withdrawal_diagnostic_code = Some(request.reason_code.clone());
        release.validate()?;
        let contract = serde_json::to_value(&release)
            .map_err(|_| EvaluationControlStoreError::ContractInvalid)?;
        sqlx::query(
            "UPDATE evaluation.evaluation_releases SET state='withdrawn',revision=$2,contract=$3,\
             withdrawn_at=$4,withdrawal_diagnostic_code=$5,updated_at=$4 WHERE release_id=$1",
        )
        .bind(release.id.as_uuid())
        .bind(revision_i64(release.revision)?)
        .bind(&contract)
        .bind(now.get())
        .bind(request.reason_code.as_str())
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "INSERT INTO evaluation.evaluation_release_withdrawals \
            (release_id,project_id,course_id,release_revision,withdrawn_by,reason_code,idempotency_key,\
              request_sha256,trace_id,withdrawn_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
        )
        .bind(release.id.as_uuid())
        .bind(release.project_id.as_uuid())
        .bind(release.course_id.map(CourseId::as_uuid))
        .bind(revision_i64(release.revision)?)
        .bind(request.withdrawn_by.as_uuid())
        .bind(request.reason_code.as_str())
        .bind(idempotency_key.as_str())
        .bind(request_sha256.to_string())
        .bind(trace_id)
        .bind(now.get())
        .execute(&mut *transaction)
        .await?;
        IdempotencyStore::complete(
            &mut transaction,
            Domain::Evaluation,
            WITHDRAW_RELEASE_OPERATION,
            idempotency_key.as_str(),
            &contract,
        )
        .await?;
        transaction.commit().await?;
        tracing::info!(
            event = "evaluation.release.withdrawn",
            project_id = %request.project_id,
            course_id = ?request.course_id,
            release_id = %release.id,
            revision = release.revision.get(),
            actor_id = %request.withdrawn_by,
            diagnostic = request.reason_code.as_str(),
            idempotency_replay = false,
            trace_id,
        );
        Ok(release)
    }

    pub async fn create_run(
        &self,
        request: &InternalCreateEvaluationRunRequest,
        idempotency_key: &IdempotencyKey,
        now: UtcTimestamp,
        trace_id: &str,
        admission: &AuthoringPublicationAdmissionBinding,
    ) -> Result<EvaluationRunReservation, EvaluationControlStoreError> {
        self.create_run_inner(request, idempotency_key, now, trace_id, admission)
            .await
    }

    async fn create_run_inner(
        &self,
        request: &InternalCreateEvaluationRunRequest,
        idempotency_key: &IdempotencyKey,
        now: UtcTimestamp,
        trace_id: &str,
        admission: &AuthoringPublicationAdmissionBinding,
    ) -> Result<EvaluationRunReservation, EvaluationControlStoreError> {
        request
            .validate()
            .map_err(|_| EvaluationControlStoreError::ContractInvalid)?;
        validate_trace(trace_id)?;
        if request.identity.trace_id != trace_id {
            return Err(EvaluationControlStoreError::IdentityMismatch);
        }
        let request_sha256 = Sha256Digest::of_canonical(request)
            .map_err(|_| EvaluationControlStoreError::ContractInvalid)?;
        let run_identity_sha256 = Sha256Digest::of_canonical(&request.identity)
            .map_err(|_| EvaluationControlStoreError::ContractInvalid)?;
        let mut transaction = self.pool.begin().await?;
        match IdempotencyStore::reserve(
            &mut transaction,
            Domain::Evaluation,
            CREATE_RUN_OPERATION,
            idempotency_key.as_str(),
            request_sha256,
        )
        .await?
        {
            IdempotencyDecision::Replay(value) => {
                let run = decode_run(value)?;
                transaction.rollback().await?;
                return Ok(EvaluationRunReservation::Replayed(run));
            }
            IdempotencyDecision::Conflict => {
                transaction.rollback().await?;
                return Err(EvaluationControlStoreError::IdempotencyConflict);
            }
            IdempotencyDecision::InProgress => {
                transaction.rollback().await?;
                return Err(EvaluationControlStoreError::RequestInProgress);
            }
            IdempotencyDecision::Reserved => {}
        }
        let release = load_release_for_update(&mut transaction, request.release_id).await?;
        if release.project_id != request.project_id
            || release.course_id != request.course_id
            || release.revision != request.release_revision
            || release.state != EvaluationReleaseState::Active
        {
            transaction.rollback().await?;
            return if release.state == EvaluationReleaseState::Active {
                Err(EvaluationControlStoreError::IdentityMismatch)
            } else {
                Err(EvaluationControlStoreError::ReleaseWithdrawn)
            };
        }
        verify_authoring_admission(&release, request, admission)?;
        if release.runtime_identity != request.identity.runtime_identity {
            transaction.rollback().await?;
            return Err(EvaluationControlStoreError::IdentityMismatch);
        }
        verify_frozen_submission(&mut transaction, request).await?;
        let run_id = EvaluationRunId::new();
        let steps = step_runs_for(&release, run_id)?;
        let max_score = release.evaluation_spec.body().aggregation().max_score();
        let run = EvaluationRun {
            schema_version: EVALUATION_RUN_SCHEMA_VERSION.to_owned(),
            id: run_id,
            project_id: request.project_id,
            course_id: request.course_id,
            release_id: release.id,
            release_revision: release.revision,
            frozen_submission_id: request.frozen_submission_id,
            actor_id: request.actor_id,
            state: EvaluationRunState::Queued,
            revision: Revision::new(1).map_err(|_| EvaluationControlStoreError::ContractInvalid)?,
            identity: request.identity.clone(),
            max_score,
            awarded_score: 0,
            steps,
            diagnostic_code: None,
            cancellation_requested: false,
            cleanup_verified: false,
            created_at: now,
            updated_at: now,
            completed_at: None,
        };
        run.validate()?;
        let contract =
            serde_json::to_value(&run).map_err(|_| EvaluationControlStoreError::ContractInvalid)?;
        sqlx::query(
            "INSERT INTO evaluation.evaluation_runs \
             (run_id,project_id,course_id,release_id,release_revision,frozen_submission_id,actor_id,\
              idempotency_key,request_sha256,run_identity_sha256,state,revision,max_score,\
              awarded_score,cleanup_verified,contract,created_at,updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,'queued',$11,$12,0,false,$13,$14,$14)",
        )
        .bind(run.id.as_uuid())
        .bind(run.project_id.as_uuid())
        .bind(run.course_id.map(CourseId::as_uuid))
        .bind(run.release_id.as_uuid())
        .bind(revision_i64(run.release_revision)?)
        .bind(run.frozen_submission_id.as_uuid())
        .bind(run.actor_id.as_uuid())
        .bind(idempotency_key.as_str())
        .bind(request_sha256.to_string())
        .bind(run_identity_sha256.to_string())
        .bind(revision_i64(run.revision)?)
        .bind(i32::try_from(run.max_score).map_err(|_| EvaluationControlStoreError::ScoreInvalid)?)
        .bind(&contract)
        .bind(now.get())
        .execute(&mut *transaction)
        .await
        .map_err(map_unique)?;
        for step in &run.steps {
            save_new_step(&mut transaction, step, now).await?;
        }
        enqueue_run_event(
            &mut transaction,
            &run,
            subjects::EVALUATION_RUN_REQUESTED,
            now,
            trace_id,
            None,
        )
        .await?;
        IdempotencyStore::complete(
            &mut transaction,
            Domain::Evaluation,
            CREATE_RUN_OPERATION,
            idempotency_key.as_str(),
            &contract,
        )
        .await?;
        transaction.commit().await?;
        Ok(EvaluationRunReservation::Created(run))
    }

    pub async fn load_run(
        &self,
        run_id: EvaluationRunId,
    ) -> Result<EvaluationRun, EvaluationControlStoreError> {
        let value: Value =
            sqlx::query_scalar("SELECT contract FROM evaluation.evaluation_runs WHERE run_id=$1")
                .bind(run_id.as_uuid())
                .fetch_optional(&self.pool)
                .await?
                .ok_or(EvaluationControlStoreError::RunNotFound)?;
        decode_run(value)
    }

    /// Loads the payload-free execution checkpoint for one exact attempt.
    pub async fn load_execution_checkpoint(
        &self,
        lease: &EvaluationStepLease,
    ) -> Result<Option<EvaluationExecutionCheckpoint>, EvaluationControlStoreError> {
        let row = sqlx::query(
            "SELECT attempt.execution_started_at,attempt.execution_terminated_at,attempt.terminal_completion,
                    attempt.execution_resources
             FROM evaluation.evaluation_step_attempts AS attempt
             JOIN evaluation.evaluation_step_runs AS step
               ON step.step_run_id=attempt.step_run_id
             WHERE attempt.step_run_id=$1 AND attempt.attempt=$2 AND attempt.task_run_id=$3
               AND attempt.worker_id=$4 AND attempt.lease_token=$5
               AND step.revision=$6",
        )
        .bind(lease.step_run_id.as_uuid())
        .bind(
            i32::try_from(lease.attempt)
                .map_err(|_| EvaluationControlStoreError::AttemptOverflow)?,
        )
        .bind(lease.task_run_id.as_uuid())
        .bind(&lease.worker_id)
        .bind(lease.lease_token())
        .bind(revision_i64(lease.revision)?)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(EvaluationControlStoreError::LeaseLost)?;
        let started_at: Option<time::OffsetDateTime> = row.try_get("execution_started_at")?;
        let terminated_at: Option<time::OffsetDateTime> = row.try_get("execution_terminated_at")?;
        let terminal_value: Option<Value> = row.try_get("terminal_completion")?;
        let resources_value: Option<Value> = row.try_get("execution_resources")?;
        if started_at.is_none()
            && terminated_at.is_none()
            && terminal_value.is_none()
            && resources_value.is_none()
        {
            return Ok(None);
        }
        let execution_resources = resources_value
            .map(|value| {
                serde_json::from_value::<EvaluationExecutionResources>(value)
                    .map_err(|_| EvaluationControlStoreError::ContractInvalid)
            })
            .transpose()?;
        if let Some(resources) = &execution_resources {
            resources.validate_for(lease.run_id, lease.step_run_id, lease.task_run_id)?;
        }
        let terminal_completion = terminal_value
            .map(|value| {
                serde_json::from_value::<EvaluationStepCompletion>(value)
                    .map_err(|_| EvaluationControlStoreError::ContractInvalid)
            })
            .transpose()?;
        if started_at.is_some() != terminated_at.is_some()
            || (started_at.is_some()
                && execution_resources.as_ref().is_some_and(|resources| {
                    resources.objects.is_empty()
                        && resources.kind != EvaluationExecutionKind::LlmReview
                }))
            || (terminal_completion.is_some() && execution_resources.is_none())
        {
            return Err(EvaluationControlStoreError::ContractInvalid);
        }
        let execution_started_at = started_at
            .map(UtcTimestamp::from_utc)
            .transpose()
            .map_err(|_| EvaluationControlStoreError::ClockInvalid)?;
        let execution_terminated_at = terminated_at
            .map(UtcTimestamp::from_utc)
            .transpose()
            .map_err(|_| EvaluationControlStoreError::ClockInvalid)?;
        if let Some((started, terminated)) = execution_started_at.zip(execution_terminated_at)
            && terminated <= started
        {
            return Err(EvaluationControlStoreError::ContractInvalid);
        }
        Ok(Some(EvaluationExecutionCheckpoint {
            execution_started_at,
            execution_terminated_at,
            terminal_completion,
            execution_resources,
        }))
    }

    /// Persists the immutable execution intent before the executor can apply
    /// any external object.  The request carries the deterministic attempt
    /// identity; object UIDs are filled by `mark_execution_started` after the
    /// bundle has been applied.
    pub async fn persist_execution_intent(
        &self,
        lease: &EvaluationStepLease,
        resources: &EvaluationExecutionResources,
    ) -> Result<(), EvaluationControlStoreError> {
        if !resources.objects.is_empty() {
            return Err(EvaluationControlStoreError::ContractInvalid);
        }
        resources.validate_for(lease.run_id, lease.step_run_id, lease.task_run_id)?;
        let resources_value = serde_json::to_value(resources)
            .map_err(|_| EvaluationControlStoreError::ContractInvalid)?;
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query(
            "SELECT attempt.execution_started_at,attempt.execution_terminated_at,attempt.terminal_completion,
                    attempt.execution_resources
             FROM evaluation.evaluation_step_attempts AS attempt
             JOIN evaluation.evaluation_step_runs AS step
               ON step.step_run_id=attempt.step_run_id
             WHERE attempt.step_run_id=$1 AND attempt.attempt=$2 AND attempt.task_run_id=$3
               AND attempt.worker_id=$4 AND attempt.lease_token=$5
               AND step.revision=$6
               AND attempt.state='running' AND attempt.lease_expires_at > clock_timestamp()
             FOR UPDATE",
        )
        .bind(lease.step_run_id.as_uuid())
        .bind(
            i32::try_from(lease.attempt)
                .map_err(|_| EvaluationControlStoreError::AttemptOverflow)?,
        )
        .bind(lease.task_run_id.as_uuid())
        .bind(&lease.worker_id)
        .bind(lease.lease_token())
        .bind(revision_i64(lease.revision)?)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(EvaluationControlStoreError::LeaseLost)?;
        let existing_started: Option<time::OffsetDateTime> = row.try_get("execution_started_at")?;
        let existing_terminated: Option<time::OffsetDateTime> =
            row.try_get("execution_terminated_at")?;
        let existing_terminal: Option<Value> = row.try_get("terminal_completion")?;
        let existing_resources: Option<Value> = row.try_get("execution_resources")?;
        if existing_started.is_some()
            || existing_terminated.is_some()
            || existing_terminal.is_some()
        {
            return Err(EvaluationControlStoreError::IdentityMismatch);
        }
        if let Some(existing) = existing_resources {
            let existing: EvaluationExecutionResources = serde_json::from_value(existing)
                .map_err(|_| EvaluationControlStoreError::ContractInvalid)?;
            existing.validate_for(lease.run_id, lease.step_run_id, lease.task_run_id)?;
            if existing != *resources {
                return Err(EvaluationControlStoreError::IdentityMismatch);
            }
            transaction.commit().await?;
            return Ok(());
        }
        let updated = sqlx::query(
            "UPDATE evaluation.evaluation_step_attempts AS attempt
             SET execution_resources=$7,updated_at=clock_timestamp()
             FROM evaluation.evaluation_step_runs AS step
             WHERE attempt.step_run_id=$1 AND attempt.attempt=$2 AND attempt.task_run_id=$3
               AND attempt.worker_id=$4 AND attempt.lease_token=$5
               AND step.step_run_id=attempt.step_run_id AND step.revision=$6
               AND attempt.state='running' AND attempt.lease_expires_at > clock_timestamp()",
        )
        .bind(lease.step_run_id.as_uuid())
        .bind(
            i32::try_from(lease.attempt)
                .map_err(|_| EvaluationControlStoreError::AttemptOverflow)?,
        )
        .bind(lease.task_run_id.as_uuid())
        .bind(&lease.worker_id)
        .bind(lease.lease_token())
        .bind(revision_i64(lease.revision)?)
        .bind(resources_value)
        .execute(&mut *transaction)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(EvaluationControlStoreError::LeaseLost);
        }
        transaction.commit().await?;
        Ok(())
    }

    /// Persists the object references immediately after the Kubernetes bundle is applied.
    ///
    /// Only the exact live lease owner can create this checkpoint.  The rendered Job documents,
    /// including signed materializer URLs and command data, never cross this boundary.
    pub async fn mark_execution_started(
        &self,
        lease: &EvaluationStepLease,
        started_at: Option<UtcTimestamp>,
        resources: &EvaluationExecutionResources,
    ) -> Result<(), EvaluationControlStoreError> {
        resources.validate_for(lease.run_id, lease.step_run_id, lease.task_run_id)?;
        let resources_value = serde_json::to_value(resources)
            .map_err(|_| EvaluationControlStoreError::ContractInvalid)?;
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query(
            "SELECT attempt.execution_started_at,attempt.execution_resources
             FROM evaluation.evaluation_step_attempts AS attempt
             JOIN evaluation.evaluation_step_runs AS step
               ON step.step_run_id=attempt.step_run_id
             WHERE attempt.step_run_id=$1 AND attempt.attempt=$2 AND attempt.task_run_id=$3
               AND attempt.worker_id=$4 AND attempt.lease_token=$5
               AND step.revision=$6
               AND attempt.state='running' AND attempt.lease_expires_at > clock_timestamp()
             FOR UPDATE",
        )
        .bind(lease.step_run_id.as_uuid())
        .bind(
            i32::try_from(lease.attempt)
                .map_err(|_| EvaluationControlStoreError::AttemptOverflow)?,
        )
        .bind(lease.task_run_id.as_uuid())
        .bind(&lease.worker_id)
        .bind(lease.lease_token())
        .bind(revision_i64(lease.revision)?)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(EvaluationControlStoreError::LeaseLost)?;
        let existing_started: Option<time::OffsetDateTime> = row.try_get("execution_started_at")?;
        let existing_resources: Option<Value> = row.try_get("execution_resources")?;
        if let Some(existing) = existing_resources {
            let existing: EvaluationExecutionResources = serde_json::from_value(existing)
                .map_err(|_| EvaluationControlStoreError::ContractInvalid)?;
            existing.validate_for(lease.run_id, lease.step_run_id, lease.task_run_id)?;
            // A pre-start intent has no UIDs yet.  It may be upgraded once,
            // but its immutable request and attempt identity must match.
            let intent_matches = existing.objects.is_empty()
                && existing.schema_version == resources.schema_version
                && existing.run_id == resources.run_id
                && existing.step_run_id == resources.step_run_id
                && existing.task_run_id == resources.task_run_id
                && existing.namespace == resources.namespace
                && existing.kind == resources.kind
                && existing.request == resources.request;
            if (existing.objects.is_empty() && !intent_matches)
                || (!existing.objects.is_empty() && existing != *resources)
                || existing_started
                    .map(|value| UtcTimestamp::from_utc(value))
                    .transpose()
                    .map_err(|_| EvaluationControlStoreError::ClockInvalid)?
                    != started_at
            {
                return Err(EvaluationControlStoreError::IdentityMismatch);
            }
            if existing.objects.is_empty() {
                sqlx::query(
                    "UPDATE evaluation.evaluation_step_attempts AS attempt
                     SET execution_started_at=$7,execution_resources=$8,updated_at=clock_timestamp()
                     FROM evaluation.evaluation_step_runs AS step
                     WHERE attempt.step_run_id=$1 AND attempt.attempt=$2 AND attempt.task_run_id=$3
                       AND attempt.worker_id=$4 AND attempt.lease_token=$5
                       AND step.step_run_id=attempt.step_run_id AND step.revision=$6
                       AND attempt.state='running' AND attempt.lease_expires_at > clock_timestamp()",
                )
                .bind(lease.step_run_id.as_uuid())
                .bind(
                    i32::try_from(lease.attempt)
                        .map_err(|_| EvaluationControlStoreError::AttemptOverflow)?,
                )
                .bind(lease.task_run_id.as_uuid())
                .bind(&lease.worker_id)
                .bind(lease.lease_token())
                .bind(revision_i64(lease.revision)?)
                .bind(started_at.map(UtcTimestamp::get))
                .bind(resources_value)
                .execute(&mut *transaction)
                .await?;
            }
            transaction.commit().await?;
            return Ok(());
        }
        if existing_started.is_some() {
            return Err(EvaluationControlStoreError::ContractInvalid);
        }
        sqlx::query(
            "UPDATE evaluation.evaluation_step_attempts AS attempt
             SET execution_started_at=$7,execution_resources=$8,updated_at=clock_timestamp()
             FROM evaluation.evaluation_step_runs AS step
             WHERE attempt.step_run_id=$1 AND attempt.attempt=$2 AND attempt.task_run_id=$3
               AND attempt.worker_id=$4 AND attempt.lease_token=$5
               AND step.step_run_id=attempt.step_run_id AND step.revision=$6
               AND attempt.state='running' AND attempt.lease_expires_at > clock_timestamp()",
        )
        .bind(lease.step_run_id.as_uuid())
        .bind(
            i32::try_from(lease.attempt)
                .map_err(|_| EvaluationControlStoreError::AttemptOverflow)?,
        )
        .bind(lease.task_run_id.as_uuid())
        .bind(&lease.worker_id)
        .bind(lease.lease_token())
        .bind(revision_i64(lease.revision)?)
        .bind(started_at.map(UtcTimestamp::get))
        .bind(resources_value)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
        Ok(())
    }

    /// Stores the deterministic terminal result and its usage delivery intents atomically.
    ///
    /// This is the commit point before any executor cleanup or Resource release.  A retry with
    /// the same checkpoint is idempotent; a different result or usage intent is rejected.
    pub async fn checkpoint_execution_terminal(
        &self,
        lease: &EvaluationStepLease,
        execution_started_at: Option<UtcTimestamp>,
        execution_terminated_at: Option<UtcTimestamp>,
        completion: &EvaluationStepCompletion,
        deliveries: &[RecordResourceUsageRequest],
    ) -> Result<(), EvaluationControlStoreError> {
        if execution_started_at.is_some() != execution_terminated_at.is_some()
            || execution_started_at
                .zip(execution_terminated_at)
                .is_some_and(|(started, terminated)| terminated <= started)
        {
            return Err(EvaluationControlStoreError::ContractInvalid);
        }
        if deliveries.len() > 2 {
            return Err(EvaluationControlStoreError::ContractInvalid);
        }
        let completion_value = serde_json::to_value(completion)
            .map_err(|_| EvaluationControlStoreError::ContractInvalid)?;
        if serde_json::to_vec(&completion_value)
            .map_err(|_| EvaluationControlStoreError::ContractInvalid)?
            .len()
            > 16 * 1024
        {
            return Err(EvaluationControlStoreError::ContractInvalid);
        }
        let mut transaction = self.pool.begin().await?;
        let step = load_step_for_update(&mut transaction, lease.step_run_id).await?;
        if step.run_id != lease.run_id
            || step.current_attempt != lease.attempt
            || step.state != EvaluationStepRunState::Running
        {
            transaction.rollback().await?;
            return Err(EvaluationControlStoreError::LeaseLost);
        }
        completion.validate(step.role, step.max_score)?;
        let row = sqlx::query(
            "SELECT attempt.execution_started_at,attempt.execution_terminated_at,attempt.terminal_completion,
                    attempt.execution_resources
             FROM evaluation.evaluation_step_attempts AS attempt
             JOIN evaluation.evaluation_step_runs AS step
               ON step.step_run_id=attempt.step_run_id
             WHERE attempt.step_run_id=$1 AND attempt.attempt=$2 AND attempt.task_run_id=$3
               AND attempt.worker_id=$4 AND attempt.lease_token=$5
               AND step.revision=$6
               AND attempt.state='running' AND attempt.lease_expires_at > clock_timestamp()
             FOR UPDATE",
        )
        .bind(lease.step_run_id.as_uuid())
        .bind(
            i32::try_from(lease.attempt)
                .map_err(|_| EvaluationControlStoreError::AttemptOverflow)?,
        )
        .bind(lease.task_run_id.as_uuid())
        .bind(&lease.worker_id)
        .bind(lease.lease_token())
        .bind(revision_i64(lease.revision)?)
        .fetch_optional(&mut *transaction)
        .await?
        .ok_or(EvaluationControlStoreError::LeaseLost)?;
        let existing_started: Option<time::OffsetDateTime> = row.try_get("execution_started_at")?;
        let existing_terminated: Option<time::OffsetDateTime> =
            row.try_get("execution_terminated_at")?;
        let existing_completion: Option<Value> = row.try_get("terminal_completion")?;
        let resources_value: Option<Value> = row.try_get("execution_resources")?;
        let resources_value =
            resources_value.ok_or(EvaluationControlStoreError::ContractInvalid)?;
        let resources: EvaluationExecutionResources = serde_json::from_value(resources_value)
            .map_err(|_| EvaluationControlStoreError::ContractInvalid)?;
        resources.validate_for(lease.run_id, lease.step_run_id, lease.task_run_id)?;
        // A failed intent with no created objects is a known pre-start
        // outcome and needs no UID set.  A successful completion still
        // requires at least one persisted object identity.
        if resources.objects.is_empty()
            && completion.state == EvaluationStepRunState::Succeeded
            && resources.kind != EvaluationExecutionKind::LlmReview
        {
            return Err(EvaluationControlStoreError::ContractInvalid);
        }
        if let Some(existing_completion) = existing_completion {
            let existing_completion: EvaluationStepCompletion =
                serde_json::from_value(existing_completion)
                    .map_err(|_| EvaluationControlStoreError::ContractInvalid)?;
            let existing_started = existing_started
                .map(UtcTimestamp::from_utc)
                .transpose()
                .map_err(|_| EvaluationControlStoreError::ClockInvalid)?;
            let existing_terminated = existing_terminated
                .map(UtcTimestamp::from_utc)
                .transpose()
                .map_err(|_| EvaluationControlStoreError::ClockInvalid)?;
            if existing_started != execution_started_at
                || existing_terminated != execution_terminated_at
                || existing_completion != *completion
            {
                return Err(EvaluationControlStoreError::IdentityMismatch);
            }
        } else {
            if existing_started.is_some() != existing_terminated.is_some() {
                return Err(EvaluationControlStoreError::ContractInvalid);
            }
            let updated = sqlx::query(
                "UPDATE evaluation.evaluation_step_attempts AS attempt
                 SET execution_started_at=$7,execution_terminated_at=$8,
                     terminal_completion=$9,updated_at=clock_timestamp()
                 FROM evaluation.evaluation_step_runs AS step
                 WHERE attempt.step_run_id=$1 AND attempt.attempt=$2 AND attempt.task_run_id=$3
                   AND attempt.worker_id=$4 AND attempt.lease_token=$5
                   AND step.step_run_id=attempt.step_run_id AND step.revision=$6
                   AND attempt.state='running' AND attempt.lease_expires_at > clock_timestamp()",
            )
            .bind(lease.step_run_id.as_uuid())
            .bind(
                i32::try_from(lease.attempt)
                    .map_err(|_| EvaluationControlStoreError::AttemptOverflow)?,
            )
            .bind(lease.task_run_id.as_uuid())
            .bind(&lease.worker_id)
            .bind(lease.lease_token())
            .bind(revision_i64(lease.revision)?)
            .bind(execution_started_at.map(UtcTimestamp::get))
            .bind(execution_terminated_at.map(UtcTimestamp::get))
            .bind(&completion_value)
            .execute(&mut *transaction)
            .await?;
            if updated.rows_affected() != 1 {
                transaction.rollback().await?;
                return Err(EvaluationControlStoreError::LeaseLost);
            }
        }
        for request in deliveries {
            enqueue_resource_delivery(&mut transaction, lease, request, execution_terminated_at)
                .await?;
        }
        transaction.commit().await?;
        Ok(())
    }

    /// Claims one due usage intent. The attempt counter is advanced before network I/O so a
    /// process restart leaves a retryable row behind without issuing two HTTP calls in one loop.
    pub async fn claim_resource_meter_delivery(
        &self,
    ) -> Result<Option<PendingResourceMeterDelivery>, EvaluationControlStoreError> {
        let mut transaction = self.pool.begin().await?;
        let row = sqlx::query(
            "SELECT delivery_id,step_run_id,attempt,task_run_id,source_event_id,request,attempts
             FROM evaluation.resource_meter_deliveries
             WHERE state='pending' AND next_attempt_at <= clock_timestamp()
             ORDER BY next_attempt_at,delivery_id
             FOR UPDATE SKIP LOCKED LIMIT 1",
        )
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(row) = row else {
            transaction.commit().await?;
            return Ok(None);
        };
        let delivery_id: Uuid = row.try_get("delivery_id")?;
        let step_run_id = parse_id::<EvaluationStepRunId>(row.try_get("step_run_id")?)?;
        let attempt_i32: i32 = row.try_get("attempt")?;
        let attempt =
            u32::try_from(attempt_i32).map_err(|_| EvaluationControlStoreError::AttemptOverflow)?;
        let task_run_id = parse_id::<TaskRunId>(row.try_get("task_run_id")?)?;
        let source_event_id = parse_id::<EventId>(row.try_get("source_event_id")?)?;
        let request: RecordResourceUsageRequest =
            serde_json::from_value(row.try_get("request")?)
                .map_err(|_| EvaluationControlStoreError::ContractInvalid)?;
        let attempts: i32 = row.try_get("attempts")?;
        if attempts < 0 || request.source_event_id != source_event_id {
            return Err(EvaluationControlStoreError::ContractInvalid);
        }
        let updated = sqlx::query(
            "UPDATE evaluation.resource_meter_deliveries
             SET attempts=attempts+1,
                 next_attempt_at=clock_timestamp()+interval '5 seconds'
             WHERE delivery_id=$1 AND state='pending'",
        )
        .bind(delivery_id)
        .execute(&mut *transaction)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(EvaluationControlStoreError::StateConflict);
        }
        transaction.commit().await?;
        Ok(Some(PendingResourceMeterDelivery {
            delivery_id,
            step_run_id,
            attempt,
            task_run_id,
            source_event_id,
            request,
            attempts,
        }))
    }

    /// Marks one exact usage intent delivered after Resource acknowledged its source event.
    pub async fn mark_resource_meter_delivery_delivered(
        &self,
        delivery_id: Uuid,
        source_event_id: EventId,
    ) -> Result<(), EvaluationControlStoreError> {
        let updated = sqlx::query(
            "UPDATE evaluation.resource_meter_deliveries
             SET state='delivered',delivered_at=clock_timestamp(),last_diagnostic_code=NULL
             WHERE delivery_id=$1 AND source_event_id=$2 AND state='pending'",
        )
        .bind(delivery_id)
        .bind(source_event_id.as_uuid())
        .execute(&self.pool)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(EvaluationControlStoreError::StateConflict);
        }
        Ok(())
    }

    /// Records a payload-free delivery diagnostic while preserving the durable retry row.
    pub async fn mark_resource_meter_delivery_failed(
        &self,
        delivery_id: Uuid,
        source_event_id: EventId,
        diagnostic_code: &str,
    ) -> Result<(), EvaluationControlStoreError> {
        if diagnostic_code.is_empty()
            || diagnostic_code.len() > 128
            || diagnostic_code.chars().any(char::is_control)
        {
            return Err(EvaluationControlStoreError::ContractInvalid);
        }
        let updated = sqlx::query(
            "UPDATE evaluation.resource_meter_deliveries
             SET last_diagnostic_code=$3
             WHERE delivery_id=$1 AND source_event_id=$2 AND state='pending'",
        )
        .bind(delivery_id)
        .bind(source_event_id.as_uuid())
        .bind(diagnostic_code)
        .execute(&self.pool)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(EvaluationControlStoreError::StateConflict);
        }
        Ok(())
    }

    pub async fn student_results(
        &self,
        project_id: ProjectId,
        course_id: Option<CourseId>,
        actor_id: ActorId,
        cursor: Option<EvaluationRunId>,
        limit: u16,
    ) -> Result<CursorPage<StudentEvaluationResult>, EvaluationControlStoreError> {
        if !(1..=100).contains(&limit) {
            return Err(EvaluationControlStoreError::ContractInvalid);
        }
        if let Some(cursor) = cursor {
            let exists = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM evaluation.evaluation_runs \
                 WHERE project_id=$1 AND ($2::uuid IS NULL OR course_id=$2) AND actor_id=$3 AND run_id=$4 \
                   AND state IN ('succeeded','failed','cancelled'))",
            )
            .bind(project_id.as_uuid())
            .bind(course_id.map(CourseId::as_uuid))
            .bind(actor_id.as_uuid())
            .bind(cursor.as_uuid())
            .fetch_one(&self.pool)
            .await?;
            if !exists {
                return Err(EvaluationControlStoreError::ContractInvalid);
            }
        }
        let values = sqlx::query_scalar::<_, Value>(
            "SELECT runs.contract FROM evaluation.evaluation_runs runs \
             JOIN evaluation.evaluation_releases releases ON releases.release_id=runs.release_id \
             WHERE runs.project_id=$1 AND ($2::uuid IS NULL OR runs.course_id=$2) AND runs.actor_id=$3 \
               AND runs.state IN ('succeeded','failed','cancelled') \
               AND ($4::uuid IS NULL OR (runs.updated_at,runs.run_id) < \
                 (SELECT updated_at,run_id FROM evaluation.evaluation_runs \
                  WHERE project_id=$1 AND ($2::uuid IS NULL OR course_id=$2) AND actor_id=$3 AND run_id=$4 \
                    AND state IN ('succeeded','failed','cancelled'))) \
             ORDER BY runs.updated_at DESC,runs.run_id DESC LIMIT $5",
        )
        .bind(project_id.as_uuid())
        .bind(course_id.map(CourseId::as_uuid))
        .bind(actor_id.as_uuid())
        .bind(cursor.map(EvaluationRunId::as_uuid))
        .bind(i64::from(limit) + 1)
        .fetch_all(&self.pool)
        .await?;
        let mut items = values
            .into_iter()
            .map(decode_run)
            .map(|run| {
                run.and_then(|value| {
                    StudentEvaluationResult::from_terminal(&value).map_err(Into::into)
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let has_more = items.len() > usize::from(limit);
        items.truncate(usize::from(limit));
        let next_cursor = if has_more {
            items.last().map(|result| result.run_id.to_string())
        } else {
            None
        };
        Ok(CursorPage { items, next_cursor })
    }

    pub async fn student_result(
        &self,
        project_id: ProjectId,
        course_id: Option<CourseId>,
        actor_id: ActorId,
        run_id: EvaluationRunId,
    ) -> Result<StudentEvaluationResult, EvaluationControlStoreError> {
        let value = sqlx::query_scalar::<_, Value>(
            "SELECT runs.contract FROM evaluation.evaluation_runs runs \
             JOIN evaluation.evaluation_releases releases ON releases.release_id=runs.release_id \
             WHERE runs.run_id=$1 AND runs.project_id=$2 AND ($3::uuid IS NULL OR runs.course_id=$3) AND runs.actor_id=$4 \
               AND runs.state IN ('succeeded','failed','cancelled')",
        )
        .bind(run_id.as_uuid())
        .bind(project_id.as_uuid())
        .bind(course_id.map(CourseId::as_uuid))
        .bind(actor_id.as_uuid())
        .fetch_optional(&self.pool)
        .await?
        .ok_or(EvaluationControlStoreError::RunNotFound)?;
        StudentEvaluationResult::from_terminal(&decode_run(value)?).map_err(Into::into)
    }

    pub async fn request_cancellation(
        &self,
        run_id: EvaluationRunId,
        request: &InternalEvaluationRunMutationRequest,
        idempotency_key: &IdempotencyKey,
        now: UtcTimestamp,
        trace_id: &str,
    ) -> Result<EvaluationRun, EvaluationControlStoreError> {
        self.revisioned_mutation(
            CANCEL_RUN_OPERATION,
            run_id,
            None,
            request,
            idempotency_key,
            now,
            trace_id,
            MutationKind::Cancel,
        )
        .await
    }

    pub async fn retry_step(
        &self,
        run_id: EvaluationRunId,
        step_run_id: EvaluationStepRunId,
        request: &InternalEvaluationRunMutationRequest,
        idempotency_key: &IdempotencyKey,
        now: UtcTimestamp,
        trace_id: &str,
    ) -> Result<EvaluationRun, EvaluationControlStoreError> {
        self.revisioned_mutation(
            RETRY_STEP_OPERATION,
            run_id,
            Some(step_run_id),
            request,
            idempotency_key,
            now,
            trace_id,
            MutationKind::RetryStep,
        )
        .await
    }

    pub async fn verify_step_cleanup(
        &self,
        run_id: EvaluationRunId,
        step_run_id: EvaluationStepRunId,
        request: &InternalEvaluationRunMutationRequest,
        idempotency_key: &IdempotencyKey,
        now: UtcTimestamp,
        trace_id: &str,
    ) -> Result<EvaluationRun, EvaluationControlStoreError> {
        self.revisioned_mutation(
            VERIFY_STEP_CLEANUP_OPERATION,
            run_id,
            Some(step_run_id),
            request,
            idempotency_key,
            now,
            trace_id,
            MutationKind::VerifyStepCleanup,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn revisioned_mutation(
        &self,
        operation: &'static str,
        run_id: EvaluationRunId,
        step_run_id: Option<EvaluationStepRunId>,
        request: &InternalEvaluationRunMutationRequest,
        idempotency_key: &IdempotencyKey,
        now: UtcTimestamp,
        trace_id: &str,
        kind: MutationKind,
    ) -> Result<EvaluationRun, EvaluationControlStoreError> {
        validate_trace(trace_id)?;
        let request_sha256 = Sha256Digest::of_canonical(&serde_json::json!({
            "runId": run_id,
            "stepRunId": step_run_id,
            "request": request,
        }))
        .map_err(|_| EvaluationControlStoreError::ContractInvalid)?;
        let mut transaction = self.pool.begin().await?;
        match IdempotencyStore::reserve(
            &mut transaction,
            Domain::Evaluation,
            operation,
            idempotency_key.as_str(),
            request_sha256,
        )
        .await?
        {
            IdempotencyDecision::Replay(value) => {
                let run = decode_run(value)?;
                transaction.rollback().await?;
                return Ok(run);
            }
            IdempotencyDecision::Conflict => {
                transaction.rollback().await?;
                return Err(EvaluationControlStoreError::IdempotencyConflict);
            }
            IdempotencyDecision::InProgress => {
                transaction.rollback().await?;
                return Err(EvaluationControlStoreError::RequestInProgress);
            }
            IdempotencyDecision::Reserved => {}
        }
        let mut run = load_run_for_update(&mut transaction, run_id).await?;
        if run.project_id != request.project_id
            || run.course_id != request.course_id
            || run.revision != request.expected_revision
        {
            transaction.rollback().await?;
            return Err(EvaluationControlStoreError::StateConflict);
        }
        let mut changed_step_ids = Vec::new();
        match kind {
            MutationKind::Cancel => {
                if is_terminal_run(run.state) {
                    transaction.rollback().await?;
                    return Err(EvaluationControlStoreError::StateConflict);
                }
                run.cancellation_requested = true;
                run.state = if run
                    .steps
                    .iter()
                    .any(|step| step.state == EvaluationStepRunState::Running)
                {
                    EvaluationRunState::Cancelling
                } else {
                    EvaluationRunState::Cancelled
                };
                run.revision = next_revision(run.revision)?;
                run.updated_at = now;
                changed_step_ids = cancel_unstarted_steps(&mut transaction, &mut run, now).await?;
                if run.state == EvaluationRunState::Cancelled {
                    run.diagnostic_code =
                        Some(DiagnosticCode::registered("LW_EVALUATION_CANCELLED"));
                    run.cleanup_verified = true;
                    run.completed_at = Some(now);
                }
            }
            MutationKind::RetryStep => {
                let step_run_id =
                    step_run_id.ok_or(EvaluationControlStoreError::ContractInvalid)?;
                let mut step = run
                    .steps
                    .iter()
                    .find(|step| step.id == step_run_id)
                    .cloned()
                    .ok_or(EvaluationControlStoreError::StepNotFound)?;
                if run.cancellation_requested
                    || step.state != EvaluationStepRunState::Failed
                    || !step.cleanup_verified
                {
                    transaction.rollback().await?;
                    return Err(EvaluationControlStoreError::StateConflict);
                }
                step.state = EvaluationStepRunState::Retryable;
                step.revision = next_revision(step.revision)?;
                step.awarded_score = None;
                step.diagnostic_code = None;
                step.started_at = None;
                step.completed_at = None;
                save_step(&mut transaction, &step).await?;
                push_step_change(&mut changed_step_ids, step.id);
                changed_step_ids.extend(
                    restore_dependency_skipped_successors(
                        &mut transaction,
                        run_id,
                        step.step_id.as_str(),
                    )
                    .await?,
                );
                run.state = EvaluationRunState::Running;
                run.cancellation_requested = false;
                run.diagnostic_code = None;
                run.completed_at = None;
                run.cleanup_verified = false;
                run.revision = next_revision(run.revision)?;
                run.updated_at = now;
            }
            MutationKind::VerifyStepCleanup => {
                let step_run_id =
                    step_run_id.ok_or(EvaluationControlStoreError::ContractInvalid)?;
                let mut step = run
                    .steps
                    .iter()
                    .find(|step| step.id == step_run_id)
                    .cloned()
                    .ok_or(EvaluationControlStoreError::StepNotFound)?;
                if !matches!(
                    step.state,
                    EvaluationStepRunState::Failed | EvaluationStepRunState::Cancelled
                ) || step.cleanup_verified
                {
                    transaction.rollback().await?;
                    return Err(EvaluationControlStoreError::StateConflict);
                }
                step.cleanup_verified = true;
                step.revision = next_revision(step.revision)?;
                mark_attempt_cleanup_verified(&mut transaction, &step, now).await?;
                save_step(&mut transaction, &step).await?;
                push_step_change(&mut changed_step_ids, step.id);
                run.revision = next_revision(run.revision)?;
                run.updated_at = now;
            }
        }
        refresh_and_save_run(
            &mut transaction,
            &mut run,
            now,
            &changed_step_ids,
            trace_id,
            Some(request.actor_id),
        )
        .await?;
        let contract =
            serde_json::to_value(&run).map_err(|_| EvaluationControlStoreError::ContractInvalid)?;
        IdempotencyStore::complete(
            &mut transaction,
            Domain::Evaluation,
            operation,
            idempotency_key.as_str(),
            &contract,
        )
        .await?;
        transaction.commit().await?;
        Ok(run)
    }

    pub async fn claim_next_step(
        &self,
        worker_id: &str,
        lease_duration: Duration,
    ) -> Result<Option<EvaluationStepLease>, EvaluationControlStoreError> {
        validate_worker(worker_id, lease_duration)?;
        let lease_milliseconds = i64::try_from(lease_duration.as_millis())
            .map_err(|_| EvaluationControlStoreError::WorkerIdentityInvalid)?;
        let mut transaction = self.pool.begin().await?;
        // A scheduler may restart after the Resource request has been created but before the
        // attempt completes.  Resume the worker's still-valid attempt and its durable TaskRunId
        // instead of allocating a second attempt (and a second Resource reservation).
        let active = sqlx::query(
            "SELECT attempt.step_run_id,step.run_id,attempt.attempt,attempt.task_run_id,
                    attempt.lease_token
             FROM evaluation.evaluation_step_attempts attempt
             JOIN evaluation.evaluation_step_runs step ON step.step_run_id=attempt.step_run_id
             JOIN evaluation.evaluation_runs run ON run.run_id=step.run_id
             WHERE attempt.worker_id=$1 AND attempt.state='running'
               AND attempt.lease_expires_at > clock_timestamp()
               AND (
                   (run.state IN ('queued','running') AND run.cancellation_requested=false)
                   OR
                   (run.state IN ('cancelling','cancelled') AND run.cancellation_requested=true)
               )
             ORDER BY attempt.created_at,attempt.step_run_id
             FOR UPDATE OF run,step,attempt SKIP LOCKED LIMIT 1",
        )
        .bind(worker_id)
        .fetch_optional(&mut *transaction)
        .await?;
        if let Some(row) = active {
            let step_run_id = parse_id::<EvaluationStepRunId>(row.try_get("step_run_id")?)?;
            let run_id = parse_id::<EvaluationRunId>(row.try_get("run_id")?)?;
            let task_run_id = parse_id::<TaskRunId>(row.try_get("task_run_id")?)?;
            let attempt = u32::try_from(row.try_get::<i32, _>("attempt")?)
                .map_err(|_| EvaluationControlStoreError::AttemptOverflow)?;
            let lease_token: Uuid = row
                .try_get("lease_token")
                .map_err(|_| EvaluationControlStoreError::LeaseLost)?;
            let run = load_run_for_update(&mut transaction, run_id).await?;
            let step = load_step_for_update(&mut transaction, step_run_id).await?;
            if step.run_id != run_id
                || step.current_attempt != attempt
                || step.state != EvaluationStepRunState::Running
            {
                transaction.rollback().await?;
                return Err(EvaluationControlStoreError::LeaseLost);
            }
            let lease = EvaluationStepLease {
                project_id: run.project_id,
                course_id: run.course_id,
                run_id,
                step_run_id,
                task_run_id,
                step_id: step.step_id,
                role: step.role,
                max_score: step.max_score,
                attempt,
                worker_id: worker_id.to_owned(),
                revision: step.revision,
                runtime_identity: run.identity.runtime_identity.clone(),
                trace_id: run.identity.trace_id,
                lease_token,
            };
            transaction.commit().await?;
            return Ok(Some(lease));
        }
        // A worker may have expired at any point after the attempt row was committed: before
        // Resource creation, after a Reviewing/Allocating reservation, after the Resource
        // handoff, or after the Kubernetes bundle was applied.  Reassign that exact attempt and
        // TaskRunId in every case.  The runner's idempotent Resource and Job reconciliation then
        // closes the phase-specific crash window without creating a second reservation or Job.
        let recovery = sqlx::query(
            "SELECT attempt.step_run_id,step.run_id,attempt.attempt,attempt.task_run_id
             FROM evaluation.evaluation_step_attempts attempt
             JOIN evaluation.evaluation_step_runs step ON step.step_run_id=attempt.step_run_id
             JOIN evaluation.evaluation_runs run ON run.run_id=step.run_id
             WHERE attempt.state='running'
               AND attempt.lease_expires_at <= clock_timestamp()
               AND (
                   (run.state IN ('queued','running') AND run.cancellation_requested=false)
                   OR
                   (run.state IN ('cancelling','cancelled') AND run.cancellation_requested=true)
               )
               AND step.state='running'
             ORDER BY attempt.lease_expires_at,attempt.step_run_id
             FOR UPDATE OF run,step,attempt SKIP LOCKED LIMIT 1",
        )
        .fetch_optional(&mut *transaction)
        .await?;
        if let Some(row) = recovery {
            let step_run_id = parse_id::<EvaluationStepRunId>(row.try_get("step_run_id")?)?;
            let run_id = parse_id::<EvaluationRunId>(row.try_get("run_id")?)?;
            let task_run_id = parse_id::<TaskRunId>(row.try_get("task_run_id")?)?;
            let attempt = u32::try_from(row.try_get::<i32, _>("attempt")?)
                .map_err(|_| EvaluationControlStoreError::AttemptOverflow)?;
            let now = authority_now(&mut transaction).await?;
            let lease_expires_at = now.get() + time::Duration::milliseconds(lease_milliseconds);
            let lease_token = Uuid::now_v7();
            sqlx::query(
                "UPDATE evaluation.evaluation_step_attempts
                 SET worker_id=$4,lease_token=$5,
                     lease_expires_at=$6,updated_at=$6
                 WHERE step_run_id=$1 AND attempt=$2 AND task_run_id=$3
                   AND state='running'",
            )
            .bind(step_run_id.as_uuid())
            .bind(i32::try_from(attempt).map_err(|_| EvaluationControlStoreError::AttemptOverflow)?)
            .bind(task_run_id.as_uuid())
            .bind(worker_id)
            .bind(lease_token)
            .bind(lease_expires_at)
            .execute(&mut *transaction)
            .await?;
            let run = load_run_for_update(&mut transaction, run_id).await?;
            let step = load_step_for_update(&mut transaction, step_run_id).await?;
            if step.run_id != run_id
                || step.current_attempt != attempt
                || step.state != EvaluationStepRunState::Running
            {
                transaction.rollback().await?;
                return Err(EvaluationControlStoreError::LeaseLost);
            }
            let lease = EvaluationStepLease {
                project_id: run.project_id,
                course_id: run.course_id,
                run_id,
                step_run_id,
                task_run_id,
                step_id: step.step_id,
                role: step.role,
                max_score: step.max_score,
                attempt,
                worker_id: worker_id.to_owned(),
                revision: step.revision,
                runtime_identity: run.identity.runtime_identity.clone(),
                trace_id: run.identity.trace_id,
                lease_token,
            };
            transaction.commit().await?;
            return Ok(Some(lease));
        }
        let row = sqlx::query(
            "SELECT step.step_run_id,step.run_id \
             FROM evaluation.evaluation_step_runs step \
             JOIN evaluation.evaluation_runs run ON run.run_id=step.run_id \
             WHERE run.state IN ('queued','running') \
               AND run.cancellation_requested=false \
               AND step.state IN ('pending','retryable') \
               AND (step.state='pending' OR step.cleanup_verified=true) \
               AND NOT EXISTS ( \
                   SELECT 1 FROM evaluation.evaluation_step_runs dep \
                   WHERE dep.run_id=step.run_id \
                     AND dep.step_id=ANY(step.depends_on) \
                     AND NOT ( \
                         dep.state='succeeded' \
                         OR \
                         (dep.role='score' AND dep.failure_policy='continue' \
                          AND dep.state IN ('failed','cancelled')) \
                         OR \
                         (dep.role='advisory' AND dep.failure_policy='continue_advisory' \
                          AND dep.state IN ('failed','cancelled')) \
                     )) \
             ORDER BY run.created_at,step.position \
             FOR UPDATE SKIP LOCKED LIMIT 1",
        )
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(row) = row else {
            transaction.rollback().await?;
            return Ok(None);
        };
        let step_run_id = parse_id::<EvaluationStepRunId>(row.try_get("step_run_id")?)?;
        let run_id = parse_id::<EvaluationRunId>(row.try_get("run_id")?)?;
        let mut run = load_run_for_update(&mut transaction, run_id).await?;
        let mut step = load_step_for_update(&mut transaction, step_run_id).await?;
        let attempt = step
            .current_attempt
            .checked_add(1)
            .ok_or(EvaluationControlStoreError::AttemptOverflow)?;
        let lease_token = Uuid::now_v7();
        let task_run_id = TaskRunId::new();
        let now = authority_now(&mut transaction).await?;
        let lease_expires_at = now.get() + time::Duration::milliseconds(lease_milliseconds);
        let runtime_identity_sha256 = runtime_identity_sha256(&run.identity.runtime_identity)?;
        let runtime_artifact_sha256 = runtime_artifact_sha256(&run.identity.runtime_identity)?;
        step.state = EvaluationStepRunState::Running;
        step.revision = next_revision(step.revision)?;
        step.current_attempt = attempt;
        step.awarded_score = None;
        step.diagnostic_code = None;
        step.cleanup_verified = false;
        step.started_at = Some(now);
        step.completed_at = None;
        save_step(&mut transaction, &step).await?;
        sqlx::query(
            "INSERT INTO evaluation.evaluation_step_attempts \
             (task_run_id,step_run_id,attempt,state,worker_id,provider_binding,runner_image,\
              runtime_artifact_sha256,runtime_identity_sha256,lease_token,lease_expires_at,\
              created_at,updated_at) \
             VALUES ($1,$2,$3,'running',$4,$5,$6,$7,$8,$9,$10,$11,$11)",
        )
        .bind(task_run_id.as_uuid())
        .bind(step_run_id.as_uuid())
        .bind(i32::try_from(attempt).map_err(|_| EvaluationControlStoreError::AttemptOverflow)?)
        .bind(worker_id)
        .bind(&run.identity.runtime_identity.provider_binding)
        .bind(&run.identity.runtime_identity.runner_image)
        .bind(runtime_artifact_sha256.to_string())
        .bind(runtime_identity_sha256.to_string())
        .bind(lease_token)
        .bind(lease_expires_at)
        .bind(now.get())
        .execute(&mut *transaction)
        .await?;
        if run.state == EvaluationRunState::Queued {
            run.state = EvaluationRunState::Running;
        }
        run.revision = next_revision(run.revision)?;
        run.updated_at = now;
        let trace_id = run.identity.trace_id.clone();
        refresh_and_save_run(&mut transaction, &mut run, now, &[step.id], &trace_id, None).await?;
        transaction.commit().await?;
        Ok(Some(EvaluationStepLease {
            project_id: run.project_id,
            course_id: run.course_id,
            run_id,
            step_run_id,
            task_run_id,
            step_id: step.step_id,
            role: step.role,
            max_score: step.max_score,
            attempt,
            worker_id: worker_id.to_owned(),
            revision: step.revision,
            runtime_identity: run.identity.runtime_identity.clone(),
            trace_id: run.identity.trace_id,
            lease_token,
        }))
    }

    /// Extends one still-owned step attempt using the database clock.
    ///
    /// The update is conditional on every durable identity fence and on the old
    /// expiry still being in the future.  A worker that loses this compare-and-
    /// swap fence must stop the external execution and cannot complete the step.
    pub async fn renew_step_lease(
        &self,
        lease: &EvaluationStepLease,
        lease_duration: Duration,
    ) -> Result<bool, EvaluationControlStoreError> {
        validate_worker(&lease.worker_id, lease_duration)?;
        let lease_milliseconds = i64::try_from(lease_duration.as_millis())
            .map_err(|_| EvaluationControlStoreError::WorkerIdentityInvalid)?;
        let cancellation_requested: Option<bool> = sqlx::query_scalar(
            r"
            UPDATE evaluation.evaluation_step_attempts AS attempt
            SET lease_expires_at = clock_timestamp() + ($7::bigint * interval '1 millisecond'),
                updated_at = clock_timestamp()
            FROM evaluation.evaluation_step_runs AS step
            JOIN evaluation.evaluation_runs AS run ON run.run_id = step.run_id
            WHERE attempt.step_run_id = $1
              AND attempt.attempt = $2
              AND attempt.state = 'running'
              AND attempt.task_run_id = $3
              AND attempt.worker_id = $4
              AND attempt.lease_token = $5
              AND attempt.lease_expires_at > clock_timestamp()
              AND step.step_run_id = attempt.step_run_id
              AND step.current_attempt = $2
              AND step.revision = $6
              AND step.state = 'running'
            RETURNING (
                SELECT current_run.cancellation_requested
                FROM evaluation.evaluation_step_runs AS current_step
                JOIN evaluation.evaluation_runs AS current_run
                  ON current_run.run_id = current_step.run_id
                WHERE current_step.step_run_id = attempt.step_run_id
            )
            ",
        )
        .bind(lease.step_run_id.as_uuid())
        .bind(
            i32::try_from(lease.attempt)
                .map_err(|_| EvaluationControlStoreError::AttemptOverflow)?,
        )
        .bind(lease.task_run_id.as_uuid())
        .bind(&lease.worker_id)
        .bind(lease.lease_token())
        .bind(revision_i64(lease.revision)?)
        .bind(lease_milliseconds)
        .fetch_optional(&self.pool)
        .await?;
        let Some(_) = cancellation_requested else {
            return Err(EvaluationControlStoreError::LeaseLost);
        };
        // The update intentionally continues while cancellation is requested so the worker keeps
        // ownership of the cleanup fence. A later heartbeat catches a cancellation that races
        // the read below.
        let cancellation_requested: bool = sqlx::query_scalar(
            "SELECT cancellation_requested FROM evaluation.evaluation_runs WHERE run_id=$1",
        )
        .bind(lease.run_id.as_uuid())
        .fetch_one(&self.pool)
        .await?;
        Ok(cancellation_requested)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn complete_step(
        &self,
        project_id: ProjectId,
        course_id: Option<CourseId>,
        run_id: EvaluationRunId,
        step_run_id: EvaluationStepRunId,
        attempt: u32,
        worker_id: &str,
        runtime_identity: &EvaluationRuntimeIdentity,
        lease_token: Uuid,
        completion: &EvaluationStepCompletion,
        trace_id: &str,
    ) -> Result<EvaluationRun, EvaluationControlStoreError> {
        validate_worker_id(worker_id)?;
        validate_trace(trace_id)?;
        if attempt == 0 {
            return Err(EvaluationControlStoreError::ContractInvalid);
        }
        let mut transaction = self.pool.begin().await?;
        let mut run = load_run_for_update(&mut transaction, run_id).await?;
        if run.project_id != project_id || run.course_id != course_id {
            transaction.rollback().await?;
            return Err(EvaluationControlStoreError::CourseMismatch);
        }
        runtime_identity.validate()?;
        if runtime_identity != &run.identity.runtime_identity {
            transaction.rollback().await?;
            return Err(EvaluationControlStoreError::IdentityMismatch);
        }
        let runtime_identity_sha256 = runtime_identity_sha256(runtime_identity)?;
        let runtime_artifact_sha256 = runtime_artifact_sha256(runtime_identity)?;
        let mut step = load_step_for_update(&mut transaction, step_run_id).await?;
        if step.run_id != run_id || step.current_attempt != attempt {
            transaction.rollback().await?;
            return Err(EvaluationControlStoreError::IdentityMismatch);
        }
        completion.validate(step.role, step.max_score)?;
        let attempt_i32 =
            i32::try_from(attempt).map_err(|_| EvaluationControlStoreError::AttemptOverflow)?;
        let terminal_state = step_state_name(completion.state);
        let completed_at: Option<time::OffsetDateTime> = sqlx::query_scalar(
            "WITH authority AS ( \
                  SELECT date_trunc('milliseconds', clock_timestamp()) AS completed_at \
              ) \
              UPDATE evaluation.evaluation_step_attempts AS attempt \
             SET state=$5,lease_token=NULL,lease_expires_at=NULL,\
                 diagnostic_code=$6,cleanup_verified=$7,\
                 completed_at=authority.completed_at,updated_at=authority.completed_at \
             FROM authority,evaluation.evaluation_step_runs AS step \
             WHERE attempt.step_run_id=$1 AND attempt.attempt=$2 \
               AND attempt.worker_id=$3 AND attempt.lease_token=$4 AND attempt.state='running' \
               AND step.step_run_id=attempt.step_run_id AND step.revision=$12 \
               AND attempt.provider_binding=$8 AND attempt.runner_image=$9 \
               AND attempt.runtime_artifact_sha256=$10 AND attempt.runtime_identity_sha256=$11 \
               AND attempt.lease_expires_at > authority.completed_at \
             RETURNING attempt.completed_at",
        )
        .bind(step_run_id.as_uuid())
        .bind(attempt_i32)
        .bind(worker_id)
        .bind(lease_token)
        .bind(terminal_state)
        .bind(
            completion
                .diagnostic_code
                .as_ref()
                .map(DiagnosticCode::as_str),
        )
        .bind(completion.cleanup_verified)
        .bind(&runtime_identity.provider_binding)
        .bind(&runtime_identity.runner_image)
        .bind(runtime_artifact_sha256.to_string())
        .bind(runtime_identity_sha256.to_string())
        .bind(revision_i64(step.revision)?)
        .fetch_optional(&mut *transaction)
        .await?;
        let Some(completed_at) = completed_at else {
            transaction.rollback().await?;
            return Err(EvaluationControlStoreError::LeaseLost);
        };
        let completed_at = UtcTimestamp::from_utc(completed_at)
            .map_err(|_| EvaluationControlStoreError::ClockInvalid)?;
        step.state = completion.state;
        step.revision = next_revision(step.revision)?;
        step.awarded_score = completion.awarded_score;
        step.review = completion.review.clone();
        step.diagnostic_code.clone_from(&completion.diagnostic_code);
        step.cleanup_verified = completion.cleanup_verified;
        step.completed_at = Some(completed_at);
        save_step(&mut transaction, &step).await?;
        let mut changed_step_ids = vec![step.id];
        if matches!(
            step.state,
            EvaluationStepRunState::Failed | EvaluationStepRunState::Cancelled
        ) && failure_stops_dependency_successors(&step)
        {
            changed_step_ids.extend(
                skip_dependency_successors(
                    &mut transaction,
                    run_id,
                    step.step_id.as_str(),
                    completed_at,
                )
                .await?,
            );
        }
        run.revision = next_revision(run.revision)?;
        run.updated_at = completed_at;
        refresh_and_save_run(
            &mut transaction,
            &mut run,
            completed_at,
            &changed_step_ids,
            trace_id,
            None,
        )
        .await?;
        transaction.commit().await?;
        Ok(run)
    }
}

#[derive(Clone, Copy)]
enum MutationKind {
    Cancel,
    RetryStep,
    VerifyStepCleanup,
}

/// One pending usage request claimed by the Evaluation delivery loop.
#[derive(Clone, Debug)]
pub struct PendingResourceMeterDelivery {
    pub delivery_id: Uuid,
    pub step_run_id: EvaluationStepRunId,
    pub attempt: u32,
    pub task_run_id: TaskRunId,
    pub source_event_id: EventId,
    pub request: RecordResourceUsageRequest,
    pub attempts: i32,
}

async fn enqueue_resource_delivery(
    transaction: &mut Transaction<'_, Postgres>,
    lease: &EvaluationStepLease,
    request: &RecordResourceUsageRequest,
    execution_terminated_at: Option<UtcTimestamp>,
) -> Result<(), EvaluationControlStoreError> {
    if request.measured_until <= request.measured_from
        || execution_terminated_at.is_some_and(|until| request.measured_until != until)
        || request.measurement.validate().is_err()
    {
        return Err(EvaluationControlStoreError::ContractInvalid);
    }
    let kind = match request.kind {
        contracts::resource::ResourceUsageKind::Compute => "compute",
        contracts::resource::ResourceUsageKind::Storage => "storage",
    };
    let request_value =
        serde_json::to_value(request).map_err(|_| EvaluationControlStoreError::ContractInvalid)?;
    let attempt =
        i32::try_from(lease.attempt).map_err(|_| EvaluationControlStoreError::AttemptOverflow)?;
    let delivery_id = Uuid::now_v7();
    let inserted = sqlx::query(
        "INSERT INTO evaluation.resource_meter_deliveries
         (delivery_id,step_run_id,attempt,task_run_id,source_event_id,kind,
          measured_from,measured_until,request,state,attempts,next_attempt_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,'pending',0,clock_timestamp())
         ON CONFLICT (task_run_id,kind) DO NOTHING",
    )
    .bind(delivery_id)
    .bind(lease.step_run_id.as_uuid())
    .bind(attempt)
    .bind(lease.task_run_id.as_uuid())
    .bind(request.source_event_id.as_uuid())
    .bind(kind)
    .bind(request.measured_from.get())
    .bind(request.measured_until.get())
    .bind(&request_value)
    .execute(&mut **transaction)
    .await
    .map_err(map_unique)?;
    if inserted.rows_affected() == 1 {
        return Ok(());
    }
    let row = sqlx::query(
        "SELECT step_run_id,attempt,task_run_id,source_event_id,request
         FROM evaluation.resource_meter_deliveries
         WHERE task_run_id=$1 AND kind=$2 FOR UPDATE",
    )
    .bind(lease.task_run_id.as_uuid())
    .bind(kind)
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or(EvaluationControlStoreError::ContractInvalid)?;
    let existing_step_run_id: Uuid = row.try_get("step_run_id")?;
    let existing_attempt: i32 = row.try_get("attempt")?;
    let existing_task_run_id: Uuid = row.try_get("task_run_id")?;
    let existing_source_event_id: Uuid = row.try_get("source_event_id")?;
    let existing_request: RecordResourceUsageRequest =
        serde_json::from_value(row.try_get("request")?)
            .map_err(|_| EvaluationControlStoreError::ContractInvalid)?;
    if existing_step_run_id != lease.step_run_id.as_uuid()
        || existing_attempt != attempt
        || existing_task_run_id != lease.task_run_id.as_uuid()
        || existing_source_event_id != request.source_event_id.as_uuid()
        || existing_request != *request
    {
        return Err(EvaluationControlStoreError::IdentityMismatch);
    }
    Ok(())
}

async fn verify_frozen_submission(
    transaction: &mut Transaction<'_, Postgres>,
    request: &InternalCreateEvaluationRunRequest,
) -> Result<(), EvaluationControlStoreError> {
    let row = sqlx::query(
        "SELECT course_id,content_sha256,source_identity_sha256 \
         FROM evaluation.frozen_submissions WHERE frozen_submission_id=$1",
    )
    .bind(request.frozen_submission_id.as_uuid())
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or(EvaluationControlStoreError::FrozenSubmissionNotFound)?;
    let course_id = request
        .course_id
        .ok_or(EvaluationControlStoreError::IdentityMismatch)?;
    if row.try_get::<Uuid, _>("course_id")? != course_id.as_uuid() {
        return Err(EvaluationControlStoreError::IdentityMismatch);
    }
    Ok(())
}

fn step_runs_for(
    release: &EvaluationRelease,
    run_id: EvaluationRunId,
) -> Result<Vec<EvaluationStepRun>, EvaluationControlStoreError> {
    release
        .evaluation_spec
        .body()
        .steps()
        .iter()
        .enumerate()
        .map(|(index, step)| {
            let role = match step {
                contracts::evaluation::EvaluationStep::Gate(_) => EvaluationStepRole::Gate,
                contracts::evaluation::EvaluationStep::Score(_) => EvaluationStepRole::Score,
                contracts::evaluation::EvaluationStep::Advisory(_) => EvaluationStepRole::Advisory,
            };
            let position = u32::try_from(index)
                .ok()
                .and_then(|value| value.checked_add(1))
                .ok_or(EvaluationControlStoreError::ContractInvalid)?;
            let step = EvaluationStepRun {
                id: EvaluationStepRunId::new(),
                run_id,
                step_id: step.id().to_owned(),
                position,
                role,
                failure_policy: step.failure_policy(),
                depends_on: step.dependencies().to_vec(),
                state: EvaluationStepRunState::Pending,
                revision: Revision::new(1)
                    .map_err(|_| EvaluationControlStoreError::ContractInvalid)?,
                current_attempt: 0,
                max_score: step.score().unwrap_or(0),
                awarded_score: None,
                review: None,
                diagnostic_code: None,
                cleanup_verified: false,
                started_at: None,
                completed_at: None,
            };
            Ok(step)
        })
        .collect::<Result<Vec<_>, _>>()
}

async fn save_new_step(
    transaction: &mut Transaction<'_, Postgres>,
    step: &EvaluationStepRun,
    now: UtcTimestamp,
) -> Result<(), EvaluationControlStoreError> {
    step.validate(step.run_id)?;
    let contract =
        serde_json::to_value(step).map_err(|_| EvaluationControlStoreError::ContractInvalid)?;
    let review_json = step
        .review
        .as_ref()
        .map(serde_json::to_value)
        .transpose()
        .map_err(|_| EvaluationControlStoreError::ContractInvalid)?;
    sqlx::query(
        "INSERT INTO evaluation.evaluation_step_runs \
         (step_run_id,run_id,position,step_id,role,failure_policy,depends_on,state,revision,current_attempt,\
         max_score,cleanup_verified,review_json,contract,created_at,updated_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,'pending',$8,0,$9,false,$10,$11,$12,$12)",
    )
    .bind(step.id.as_uuid())
    .bind(step.run_id.as_uuid())
    .bind(i32::try_from(step.position).map_err(|_| EvaluationControlStoreError::ContractInvalid)?)
    .bind(&step.step_id)
    .bind(step_role_name(step.role))
    .bind(step_failure_policy_name(step.failure_policy))
    .bind(&step.depends_on)
    .bind(revision_i64(step.revision)?)
    .bind(i32::try_from(step.max_score).map_err(|_| EvaluationControlStoreError::ScoreInvalid)?)
    .bind(review_json)
    .bind(&contract)
    .bind(now.get())
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

async fn load_release_for_update(
    transaction: &mut Transaction<'_, Postgres>,
    release_id: EvaluationReleaseId,
) -> Result<EvaluationRelease, EvaluationControlStoreError> {
    let value: Value = sqlx::query_scalar(
        "SELECT contract FROM evaluation.evaluation_releases WHERE release_id=$1 FOR UPDATE",
    )
    .bind(release_id.as_uuid())
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or(EvaluationControlStoreError::ReleaseNotFound)?;
    decode_release(value)
}

async fn load_run_for_update(
    transaction: &mut Transaction<'_, Postgres>,
    run_id: EvaluationRunId,
) -> Result<EvaluationRun, EvaluationControlStoreError> {
    let value: Value = sqlx::query_scalar(
        "SELECT contract FROM evaluation.evaluation_runs WHERE run_id=$1 FOR UPDATE",
    )
    .bind(run_id.as_uuid())
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or(EvaluationControlStoreError::RunNotFound)?;
    decode_run(value)
}

async fn load_step_for_update(
    transaction: &mut Transaction<'_, Postgres>,
    step_run_id: EvaluationStepRunId,
) -> Result<EvaluationStepRun, EvaluationControlStoreError> {
    let value: Value = sqlx::query_scalar(
        "SELECT contract FROM evaluation.evaluation_step_runs WHERE step_run_id=$1 FOR UPDATE",
    )
    .bind(step_run_id.as_uuid())
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or(EvaluationControlStoreError::StepNotFound)?;
    decode_step(value)
}

async fn load_steps(
    transaction: &mut Transaction<'_, Postgres>,
    run_id: EvaluationRunId,
) -> Result<Vec<EvaluationStepRun>, EvaluationControlStoreError> {
    let rows = sqlx::query(
        "SELECT contract FROM evaluation.evaluation_step_runs WHERE run_id=$1 ORDER BY position",
    )
    .bind(run_id.as_uuid())
    .fetch_all(&mut **transaction)
    .await?;
    rows.into_iter()
        .map(|row| decode_step(row.try_get("contract")?))
        .collect()
}

async fn save_step(
    transaction: &mut Transaction<'_, Postgres>,
    step: &EvaluationStepRun,
) -> Result<(), EvaluationControlStoreError> {
    step.validate(step.run_id)?;
    let contract =
        serde_json::to_value(step).map_err(|_| EvaluationControlStoreError::ContractInvalid)?;
    let awarded_score = step
        .awarded_score
        .map(i32::try_from)
        .transpose()
        .map_err(|_| EvaluationControlStoreError::ScoreInvalid)?;
    let review_json = step
        .review
        .as_ref()
        .map(serde_json::to_value)
        .transpose()
        .map_err(|_| EvaluationControlStoreError::ContractInvalid)?;
    let updated = sqlx::query(
        "UPDATE evaluation.evaluation_step_runs \
         SET state=$2,revision=$3,current_attempt=$4,awarded_score=$5,diagnostic_code=$6,\
             cleanup_verified=$7,contract=$8,updated_at=clock_timestamp(),\
             started_at=$9,completed_at=$10,review_json=$11 \
         WHERE step_run_id=$1",
    )
    .bind(step.id.as_uuid())
    .bind(step_state_name(step.state))
    .bind(revision_i64(step.revision)?)
    .bind(
        i32::try_from(step.current_attempt)
            .map_err(|_| EvaluationControlStoreError::AttemptOverflow)?,
    )
    .bind(awarded_score)
    .bind(step.diagnostic_code.as_ref().map(DiagnosticCode::as_str))
    .bind(step.cleanup_verified)
    .bind(&contract)
    .bind(step.started_at.map(UtcTimestamp::get))
    .bind(step.completed_at.map(UtcTimestamp::get))
    .bind(review_json)
    .execute(&mut **transaction)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(EvaluationControlStoreError::StateConflict);
    }
    Ok(())
}

async fn refresh_and_save_run(
    transaction: &mut Transaction<'_, Postgres>,
    run: &mut EvaluationRun,
    now: UtcTimestamp,
    changed_step_ids: &[EvaluationStepRunId],
    trace_id: &str,
    operator_actor_id: Option<ActorId>,
) -> Result<(), EvaluationControlStoreError> {
    run.steps = load_steps(transaction, run.id).await?;
    run.awarded_score = run
        .steps
        .iter()
        .filter(|step| step.role == EvaluationStepRole::Score)
        .map(|step| step.awarded_score.unwrap_or(0))
        .try_fold(0_u32, u32::checked_add)
        .ok_or(EvaluationControlStoreError::ScoreInvalid)?;
    let non_terminal = run.steps.iter().any(|step| {
        matches!(
            step.state,
            EvaluationStepRunState::Pending
                | EvaluationStepRunState::Running
                | EvaluationStepRunState::Retryable
        )
    });
    let deterministic_failure = run
        .steps
        .iter()
        .find(|step| failure_breaks_run(step, run.cancellation_requested));
    if let Some(step) = deterministic_failure {
        run.diagnostic_code.clone_from(&step.diagnostic_code);
        if non_terminal {
            run.state = if run.cancellation_requested {
                EvaluationRunState::Cancelling
            } else {
                EvaluationRunState::Running
            };
            run.cleanup_verified = false;
            run.completed_at = None;
        } else {
            run.state = EvaluationRunState::Failed;
            run.cleanup_verified = run
                .steps
                .iter()
                .filter(|step| !matches!(step.state, EvaluationStepRunState::Skipped))
                .all(|step| step.cleanup_verified);
            run.completed_at = if run.cleanup_verified {
                Some(now)
            } else {
                None
            };
        }
    } else if run.cancellation_requested && !non_terminal {
        run.state = EvaluationRunState::Cancelled;
        run.diagnostic_code = Some(DiagnosticCode::registered("LW_EVALUATION_CANCELLED"));
        run.cleanup_verified = run
            .steps
            .iter()
            .filter(|step| !matches!(step.state, EvaluationStepRunState::Skipped))
            .all(|step| step.cleanup_verified);
        run.completed_at = if run.cleanup_verified {
            Some(now)
        } else {
            None
        };
    } else if !non_terminal {
        let cleanup_verified = run
            .steps
            .iter()
            .filter(|step| !matches!(step.state, EvaluationStepRunState::Skipped))
            .all(|step| step.cleanup_verified);
        if cleanup_verified {
            run.state = EvaluationRunState::Succeeded;
            run.diagnostic_code = None;
            run.cleanup_verified = true;
            run.completed_at = Some(now);
        } else {
            run.state = EvaluationRunState::Running;
            run.diagnostic_code = None;
            run.cleanup_verified = false;
            run.completed_at = None;
        }
    } else if run.cancellation_requested {
        run.state = EvaluationRunState::Cancelling;
        run.cleanup_verified = false;
        run.completed_at = None;
    } else if run
        .steps
        .iter()
        .any(|step| step.state == EvaluationStepRunState::Running)
    {
        run.state = EvaluationRunState::Running;
        run.diagnostic_code = None;
        run.cleanup_verified = false;
        run.completed_at = None;
    }
    run.updated_at = now;
    run.validate()?;
    let contract =
        serde_json::to_value(&run).map_err(|_| EvaluationControlStoreError::ContractInvalid)?;
    let updated = sqlx::query(
        "UPDATE evaluation.evaluation_runs \
         SET state=$2,revision=$3,awarded_score=$4,diagnostic_code=$5,cancellation_requested=$6,\
             cleanup_verified=$7,contract=$8,updated_at=$9,completed_at=$10 \
         WHERE run_id=$1",
    )
    .bind(run.id.as_uuid())
    .bind(run_state_name(run.state))
    .bind(revision_i64(run.revision)?)
    .bind(i32::try_from(run.awarded_score).map_err(|_| EvaluationControlStoreError::ScoreInvalid)?)
    .bind(run.diagnostic_code.as_ref().map(DiagnosticCode::as_str))
    .bind(run.cancellation_requested)
    .bind(run.cleanup_verified)
    .bind(&contract)
    .bind(now.get())
    .bind(run.completed_at.map(UtcTimestamp::get))
    .execute(&mut **transaction)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(EvaluationControlStoreError::StateConflict);
    }
    enqueue_run_event(
        transaction,
        run,
        subjects::EVALUATION_RUN_STATE_CHANGED,
        now,
        trace_id,
        operator_actor_id,
    )
    .await?;
    for step_run_id in changed_step_ids {
        let step = run
            .steps
            .iter()
            .find(|step| step.id == *step_run_id)
            .ok_or(EvaluationControlStoreError::StepNotFound)?;
        enqueue_step_event(transaction, run, step, now, trace_id, operator_actor_id).await?;
    }
    Ok(())
}

async fn cancel_unstarted_steps(
    transaction: &mut Transaction<'_, Postgres>,
    run: &mut EvaluationRun,
    now: UtcTimestamp,
) -> Result<Vec<EvaluationStepRunId>, EvaluationControlStoreError> {
    let mut changed_step_ids = Vec::new();
    for step in &mut run.steps {
        if matches!(
            step.state,
            EvaluationStepRunState::Pending | EvaluationStepRunState::Retryable
        ) {
            step.state = EvaluationStepRunState::Cancelled;
            step.revision = next_revision(step.revision)?;
            step.diagnostic_code = Some(DiagnosticCode::registered("LW_EVALUATION_CANCELLED"));
            step.cleanup_verified = true;
            step.completed_at = Some(now);
            save_step(transaction, step).await?;
            push_step_change(&mut changed_step_ids, step.id);
        }
    }
    Ok(changed_step_ids)
}

async fn skip_dependency_successors(
    transaction: &mut Transaction<'_, Postgres>,
    run_id: EvaluationRunId,
    failed_step_id: &str,
    now: UtcTimestamp,
) -> Result<Vec<EvaluationStepRunId>, EvaluationControlStoreError> {
    let mut steps = load_steps(transaction, run_id).await?;
    let mut blocked = BTreeSet::from([failed_step_id.to_owned()]);
    let mut changed_step_ids = Vec::new();
    let mut progressed = true;
    while progressed {
        progressed = false;
        for step in &mut steps {
            if !step
                .depends_on
                .iter()
                .any(|dependency| blocked.contains(dependency))
            {
                continue;
            }
            match step.state {
                EvaluationStepRunState::Pending | EvaluationStepRunState::Retryable => {
                    step.state = EvaluationStepRunState::Skipped;
                    step.revision = next_revision(step.revision)?;
                    step.awarded_score = None;
                    step.diagnostic_code = Some(DiagnosticCode::registered(
                        "LW_EVALUATION_DEPENDENCY_FAILED",
                    ));
                    step.cleanup_verified = true;
                    step.completed_at = Some(now);
                    save_step(transaction, step).await?;
                    push_step_change(&mut changed_step_ids, step.id);
                    progressed |= blocked.insert(step.step_id.clone());
                }
                EvaluationStepRunState::Skipped
                    if step.diagnostic_code.as_ref().is_some_and(|diagnostic| {
                        diagnostic.as_str() == "LW_EVALUATION_DEPENDENCY_FAILED"
                    }) =>
                {
                    progressed |= blocked.insert(step.step_id.clone());
                }
                _ => {}
            }
        }
    }
    Ok(changed_step_ids)
}

async fn restore_dependency_skipped_successors(
    transaction: &mut Transaction<'_, Postgres>,
    run_id: EvaluationRunId,
    retried_step_id: &str,
) -> Result<Vec<EvaluationStepRunId>, EvaluationControlStoreError> {
    let mut steps = load_steps(transaction, run_id).await?;
    let mut restored = BTreeSet::from([retried_step_id.to_owned()]);
    let mut changed_step_ids = Vec::new();
    let mut progressed = true;
    while progressed {
        progressed = false;
        for step in &mut steps {
            if step.state != EvaluationStepRunState::Skipped
                || !step.diagnostic_code.as_ref().is_some_and(|diagnostic| {
                    diagnostic.as_str() == "LW_EVALUATION_DEPENDENCY_FAILED"
                })
                || !step
                    .depends_on
                    .iter()
                    .any(|dependency| restored.contains(dependency))
            {
                continue;
            }
            step.state = EvaluationStepRunState::Pending;
            step.revision = next_revision(step.revision)?;
            step.awarded_score = None;
            step.diagnostic_code = None;
            step.cleanup_verified = false;
            step.started_at = None;
            step.completed_at = None;
            save_step(transaction, step).await?;
            push_step_change(&mut changed_step_ids, step.id);
            progressed |= restored.insert(step.step_id.clone());
        }
    }
    Ok(changed_step_ids)
}

async fn mark_attempt_cleanup_verified(
    transaction: &mut Transaction<'_, Postgres>,
    step: &EvaluationStepRun,
    now: UtcTimestamp,
) -> Result<(), EvaluationControlStoreError> {
    let updated = sqlx::query(
        "UPDATE evaluation.evaluation_step_attempts \
         SET cleanup_verified=true,updated_at=$3 \
         WHERE step_run_id=$1 AND attempt=$2 \
           AND state IN ('failed','cancelled') AND cleanup_verified=false",
    )
    .bind(step.id.as_uuid())
    .bind(
        i32::try_from(step.current_attempt)
            .map_err(|_| EvaluationControlStoreError::AttemptOverflow)?,
    )
    .bind(now.get())
    .execute(&mut **transaction)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(EvaluationControlStoreError::StateConflict);
    }
    Ok(())
}

fn push_step_change(
    changed_step_ids: &mut Vec<EvaluationStepRunId>,
    step_run_id: EvaluationStepRunId,
) {
    if !changed_step_ids.contains(&step_run_id) {
        changed_step_ids.push(step_run_id);
    }
}

fn push_run_step_changes(
    affected_runs: &mut Vec<(EvaluationRunId, Vec<EvaluationStepRunId>)>,
    run_id: EvaluationRunId,
    changed_step_ids: Vec<EvaluationStepRunId>,
) {
    if let Some((_, existing)) = affected_runs
        .iter_mut()
        .find(|(affected_run_id, _)| *affected_run_id == run_id)
    {
        for step_run_id in changed_step_ids {
            push_step_change(existing, step_run_id);
        }
    } else {
        affected_runs.push((run_id, changed_step_ids));
    }
}

async fn enqueue_release_published(
    transaction: &mut Transaction<'_, Postgres>,
    release: &EvaluationRelease,
    now: UtcTimestamp,
    trace_id: &str,
) -> Result<(), EvaluationControlStoreError> {
    let contract = event_contract(subjects::EVALUATION_RELEASE_PUBLISHED)?;
    let data = EvaluationReleasePublished {
        release_id: release.id,
        revision: release.revision,

        published_by: release.published_by,
    };
    let event = event_envelope(
        contract,
        release.project_id,
        release.course_id,
        release.id.as_uuid(),
        release.revision,
        1,
        now,
        trace_id,
        data,
    )?;
    enqueue_event(transaction, contract, release.id.as_uuid(), 1, &event).await
}

async fn enqueue_run_event(
    transaction: &mut Transaction<'_, Postgres>,
    run: &EvaluationRun,
    subject: &'static str,
    now: UtcTimestamp,
    trace_id: &str,
    operator_actor_id: Option<ActorId>,
) -> Result<(), EvaluationControlStoreError> {
    let contract = event_contract(subject)?;
    let sequence = next_outbox_sequence(transaction, run.id.as_uuid()).await?;
    let data = EvaluationRunEvent {
        run_id: run.id,
        release_id: run.release_id,
        revision: run.revision,
        state: run_state_name(run.state).to_owned(),
        diagnostic_code: run
            .diagnostic_code
            .as_ref()
            .map(|diagnostic| diagnostic.as_str().to_owned()),
        operator_actor_id,
    };
    data.validate()?;
    let event = event_envelope(
        contract,
        run.project_id,
        run.course_id,
        run.id.as_uuid(),
        run.revision,
        sequence,
        now,
        trace_id,
        data,
    )?;
    enqueue_event(transaction, contract, run.id.as_uuid(), sequence, &event).await
}

async fn enqueue_step_event(
    transaction: &mut Transaction<'_, Postgres>,
    run: &EvaluationRun,
    step: &EvaluationStepRun,
    now: UtcTimestamp,
    trace_id: &str,
    operator_actor_id: Option<ActorId>,
) -> Result<(), EvaluationControlStoreError> {
    let contract = event_contract(subjects::EVALUATION_STEP_RUN_STATE_CHANGED)?;
    let sequence = next_outbox_sequence(transaction, step.id.as_uuid()).await?;
    let data = EvaluationStepRunEvent {
        run_id: run.id,
        step_run_id: step.id,
        step_id: step.step_id.clone(),
        revision: step.revision,
        state: step_state_name(step.state).to_owned(),
        attempt: step.current_attempt,
        diagnostic_code: step
            .diagnostic_code
            .as_ref()
            .map(|diagnostic| diagnostic.as_str().to_owned()),
        cleanup_verified: Some(step.cleanup_verified),
        operator_actor_id,
    };
    data.validate()?;
    let event = event_envelope(
        contract,
        run.project_id,
        run.course_id,
        step.id.as_uuid(),
        step.revision,
        sequence,
        now,
        trace_id,
        data,
    )?;
    enqueue_event(transaction, contract, step.id.as_uuid(), sequence, &event).await
}

#[allow(clippy::too_many_arguments)]
fn event_envelope<T: serde::Serialize>(
    contract: EventContract,
    project_id: ProjectId,
    course_id: Option<CourseId>,
    aggregate_id: Uuid,
    revision: Revision,
    sequence: u64,
    now: UtcTimestamp,
    trace_id: &str,
    data: T,
) -> Result<CloudEvent<T>, EvaluationControlStoreError> {
    let event = CloudEvent {
        specversion: SPEC_VERSION.to_owned(),
        id: EventId::new(),
        source: contract.source().to_owned(),
        event_type: contract.event_type.to_owned(),
        subject: contract.subject.to_owned(),
        time: now,
        datacontenttype: "application/json".to_owned(),
        dataschema: contract.data_schema(),
        project_id,
        course_id,
        aggregate_revision: revision,
        aggregate_sequence: Sequence(sequence),
        trace_id: trace_id.to_owned(),
        data,
    };
    let _ = aggregate_id;
    event.validate(contract)?;
    Ok(event)
}

async fn enqueue_event<T: serde::Serialize>(
    transaction: &mut Transaction<'_, Postgres>,
    contract: EventContract,
    aggregate_id: Uuid,
    sequence: u64,
    event: &CloudEvent<T>,
) -> Result<(), EvaluationControlStoreError> {
    let payload =
        serde_json::to_value(event).map_err(|_| EvaluationControlStoreError::ContractInvalid)?;
    let payload_sha256 = Sha256Digest::of_canonical(&payload)
        .map_err(|_| EvaluationControlStoreError::ContractInvalid)?;
    OutboxStore::enqueue(
        transaction,
        Domain::Evaluation,
        event.id.as_uuid(),
        contract.subject,
        contract.event_type,
        aggregate_id,
        sequence,
        &payload,
        payload_sha256,
    )
    .await?;
    Ok(())
}

async fn next_outbox_sequence(
    transaction: &mut Transaction<'_, Postgres>,
    aggregate_id: Uuid,
) -> Result<u64, EvaluationControlStoreError> {
    let next: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(aggregate_sequence),0)+1 \
         FROM evaluation.outbox_events WHERE aggregate_id=$1",
    )
    .bind(aggregate_id)
    .fetch_one(&mut **transaction)
    .await?;
    u64::try_from(next).map_err(|_| EvaluationControlStoreError::ContractInvalid)
}

async fn authority_now(
    transaction: &mut Transaction<'_, Postgres>,
) -> Result<UtcTimestamp, EvaluationControlStoreError> {
    let value: time::OffsetDateTime =
        sqlx::query_scalar("SELECT date_trunc('milliseconds', clock_timestamp())")
            .fetch_one(&mut **transaction)
            .await?;
    UtcTimestamp::from_utc(value).map_err(|_| EvaluationControlStoreError::ClockInvalid)
}

fn decode_release(value: Value) -> Result<EvaluationRelease, EvaluationControlStoreError> {
    let release: EvaluationRelease =
        serde_json::from_value(value).map_err(|_| EvaluationControlStoreError::ContractInvalid)?;
    release.validate()?;
    Ok(release)
}

fn decode_run(value: Value) -> Result<EvaluationRun, EvaluationControlStoreError> {
    let run: EvaluationRun =
        serde_json::from_value(value).map_err(|_| EvaluationControlStoreError::ContractInvalid)?;
    run.validate()?;
    Ok(run)
}

fn decode_step(value: Value) -> Result<EvaluationStepRun, EvaluationControlStoreError> {
    let step: EvaluationStepRun =
        serde_json::from_value(value).map_err(|_| EvaluationControlStoreError::ContractInvalid)?;
    step.validate(step.run_id)?;
    Ok(step)
}

fn event_contract(subject: &str) -> Result<EventContract, EvaluationControlStoreError> {
    EVENT_CONTRACTS
        .iter()
        .copied()
        .find(|contract| contract.subject == subject)
        .ok_or(EvaluationControlStoreError::ContractInvalid)
}

fn parse_id<T: FromStr<Err = uuid::Error>>(value: Uuid) -> Result<T, EvaluationControlStoreError> {
    value
        .to_string()
        .parse()
        .map_err(|_| EvaluationControlStoreError::IdentityMismatch)
}

fn next_revision(revision: Revision) -> Result<Revision, EvaluationControlStoreError> {
    Revision::new(
        revision
            .get()
            .checked_add(1)
            .ok_or(EvaluationControlStoreError::ContractInvalid)?,
    )
    .map_err(|_| EvaluationControlStoreError::ContractInvalid)
}

fn revision_i64(revision: Revision) -> Result<i64, EvaluationControlStoreError> {
    i64::try_from(revision.get()).map_err(|_| EvaluationControlStoreError::ContractInvalid)
}

const fn is_terminal_run(state: EvaluationRunState) -> bool {
    matches!(
        state,
        EvaluationRunState::Succeeded | EvaluationRunState::Failed | EvaluationRunState::Cancelled
    )
}

fn failure_stops_dependency_successors(step: &EvaluationStepRun) -> bool {
    matches!(
        step.state,
        EvaluationStepRunState::Failed | EvaluationStepRunState::Cancelled
    ) && step.failure_policy == EvaluationStepFailurePolicy::Stop
}

fn failure_breaks_run(step: &EvaluationStepRun, cancellation_requested: bool) -> bool {
    match step.state {
        // A failed deterministic step is always a failed evaluation.  The
        // score `Continue` policy only allows other runnable branches to
        // finish collecting their results; it does not turn an execution
        // failure into a successful run.  Advisory failures remain
        // informational when their declared policy is `ContinueAdvisory`.
        EvaluationStepRunState::Failed => step.role != EvaluationStepRole::Advisory,
        EvaluationStepRunState::Cancelled => {
            !cancellation_requested && step.role != EvaluationStepRole::Advisory
        }
        _ => false,
    }
}

fn runtime_identity_sha256(
    runtime_identity: &EvaluationRuntimeIdentity,
) -> Result<Sha256Digest, EvaluationControlStoreError> {
    Sha256Digest::of_canonical(runtime_identity)
        .map_err(|_| EvaluationControlStoreError::IdentityMismatch)
}

/// Returns the digest that is already part of the approved runner image.
///
/// The attempt table calls this value `runtime_artifact_sha256`; keeping the
/// value equal to the release's digest-pinned image makes the database fence
/// meaningful and prevents a worker from completing against a synthetic or
/// process-local placeholder artifact identity.
fn runtime_artifact_sha256(
    runtime_identity: &EvaluationRuntimeIdentity,
) -> Result<Sha256Digest, EvaluationControlStoreError> {
    let (_, digest) = runtime_identity
        .runner_image
        .rsplit_once("@sha256:")
        .ok_or(EvaluationControlStoreError::IdentityMismatch)?;
    digest
        .parse()
        .map_err(|_| EvaluationControlStoreError::IdentityMismatch)
}

const fn run_state_name(state: EvaluationRunState) -> &'static str {
    match state {
        EvaluationRunState::Queued => "queued",
        EvaluationRunState::Running => "running",
        EvaluationRunState::Cancelling => "cancelling",
        EvaluationRunState::Succeeded => "succeeded",
        EvaluationRunState::Failed => "failed",
        EvaluationRunState::Cancelled => "cancelled",
    }
}

const fn step_state_name(state: EvaluationStepRunState) -> &'static str {
    match state {
        EvaluationStepRunState::Pending => "pending",
        EvaluationStepRunState::Running => "running",
        EvaluationStepRunState::Retryable => "retryable",
        EvaluationStepRunState::Succeeded => "succeeded",
        EvaluationStepRunState::Failed => "failed",
        EvaluationStepRunState::Cancelled => "cancelled",
        EvaluationStepRunState::Skipped => "skipped",
    }
}

const fn step_role_name(role: EvaluationStepRole) -> &'static str {
    match role {
        EvaluationStepRole::Gate => "gate",
        EvaluationStepRole::Score => "score",
        EvaluationStepRole::Advisory => "advisory",
    }
}

const fn step_failure_policy_name(policy: EvaluationStepFailurePolicy) -> &'static str {
    match policy {
        EvaluationStepFailurePolicy::Stop => "stop",
        EvaluationStepFailurePolicy::Continue => "continue",
        EvaluationStepFailurePolicy::ContinueAdvisory => "continue_advisory",
    }
}

fn validate_trace(trace_id: &str) -> Result<(), EvaluationControlStoreError> {
    if trace_id.trim().is_empty() || trace_id.len() > 128 || trace_id.chars().any(char::is_control)
    {
        return Err(EvaluationControlStoreError::ContractInvalid);
    }
    Ok(())
}

fn verify_authoring_admission(
    release: &EvaluationRelease,
    request: &InternalCreateEvaluationRunRequest,
    admission: &AuthoringPublicationAdmissionBinding,
) -> Result<(), EvaluationControlStoreError> {
    if admission.approval_id != release.approval_id
        || admission.approval_revision != release.approval_revision
        || admission.project_id != request.project_id
        || admission.course_id != request.course_id
        || admission.evaluation_release_id != request.release_id
        || admission.evaluation_release_revision != release.revision
        || admission.environment_release_id.as_uuid().is_nil()
        || admission.environment_release_version == 0
    {
        return Err(EvaluationControlStoreError::IdentityMismatch);
    }
    Ok(())
}

fn validate_worker(
    worker_id: &str,
    lease_duration: Duration,
) -> Result<(), EvaluationControlStoreError> {
    validate_worker_id(worker_id)?;
    let millis = lease_duration.as_millis();
    if !(30_000..=1_800_000).contains(&millis) {
        return Err(EvaluationControlStoreError::WorkerIdentityInvalid);
    }
    Ok(())
}

fn validate_worker_id(worker_id: &str) -> Result<(), EvaluationControlStoreError> {
    if worker_id.trim().is_empty()
        || worker_id.len() > 96
        || !worker_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
    {
        return Err(EvaluationControlStoreError::WorkerIdentityInvalid);
    }
    Ok(())
}

fn map_unique(error: sqlx::Error) -> EvaluationControlStoreError {
    if error
        .as_database_error()
        .is_some_and(sqlx::error::DatabaseError::is_unique_violation)
    {
        EvaluationControlStoreError::IdentityMismatch
    } else {
        EvaluationControlStoreError::Database(error)
    }
}

/// Stable payload-free control-plane failures.
#[derive(Debug, thiserror::Error)]
pub enum EvaluationControlStoreError {
    #[error("LW_AUTH_COURSE_SCOPE_DENIED")]
    CourseMismatch,
    #[error("LW_EVALUATION_SPEC_INVALID")]
    ContractInvalid,
    #[error("LW_EVALUATION_IDENTITY_MISMATCH")]
    IdentityMismatch,
    #[error("LW_EVALUATION_SCORE_INVALID")]
    ScoreInvalid,
    #[error("LW_EVALUATION_RELEASE_NOT_FOUND")]
    ReleaseNotFound,
    #[error("LW_EVALUATION_RELEASE_WITHDRAWN")]
    ReleaseWithdrawn,
    #[error("LW_EVALUATION_RUN_NOT_FOUND")]
    RunNotFound,
    #[error("LW_EVALUATION_RUN_NOT_FOUND")]
    StepNotFound,
    #[error("LW_COLLECT_SUBMISSION_NOT_FOUND")]
    FrozenSubmissionNotFound,
    #[error("LW_IDEMPOTENCY_CONFLICT")]
    IdempotencyConflict,
    #[error("LW_OPERATION_IN_PROGRESS")]
    RequestInProgress,
    #[error("LW_EVALUATION_STATE_CONFLICT")]
    StateConflict,
    #[error("LW_EVALUATION_REVISION_CONFLICT")]
    RevisionConflict,
    #[error("LW_EVALUATION_STEP_LEASE_LOST")]
    LeaseLost,
    #[error("LW_EVALUATION_CLOCK_INVALID")]
    ClockInvalid,
    #[error("LW_EVALUATION_WORKER_IDENTITY_INVALID")]
    WorkerIdentityInvalid,
    #[error("LW_EVALUATION_ATTEMPT_OVERFLOW")]
    AttemptOverflow,
    #[error("LW_EVALUATION_DATABASE_FAILED")]
    Database(#[from] sqlx::Error),
    #[error("LW_EVALUATION_OUTBOX_IDENTITY_INVALID")]
    Event(#[from] contracts::events::EventError),
    #[error("LW_EVALUATION_DATABASE_FAILED")]
    Persistence(#[from] persistence_sqlx::PersistenceError),
    #[error("LW_EVALUATION_SPEC_INVALID")]
    Contract(#[from] contracts::evaluation::EvaluationControlContractError),
}

impl EvaluationControlStoreError {
    #[must_use]
    pub const fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::CourseMismatch => "LW_AUTH_COURSE_SCOPE_DENIED",
            Self::ContractInvalid | Self::Contract(_) => "LW_EVALUATION_SPEC_INVALID",
            Self::IdentityMismatch => "LW_EVALUATION_IDENTITY_MISMATCH",
            Self::ScoreInvalid => "LW_EVALUATION_SCORE_INVALID",
            Self::ReleaseNotFound => "LW_EVALUATION_RELEASE_NOT_FOUND",
            Self::ReleaseWithdrawn => "LW_EVALUATION_RELEASE_WITHDRAWN",
            Self::RunNotFound | Self::StepNotFound => "LW_EVALUATION_RUN_NOT_FOUND",
            Self::FrozenSubmissionNotFound => "LW_COLLECT_SUBMISSION_NOT_FOUND",
            Self::IdempotencyConflict => "LW_IDEMPOTENCY_CONFLICT",
            Self::RequestInProgress => "LW_OPERATION_IN_PROGRESS",
            Self::StateConflict => "LW_EVALUATION_STATE_CONFLICT",
            Self::RevisionConflict => "LW_EVALUATION_REVISION_CONFLICT",
            Self::LeaseLost => "LW_EVALUATION_STEP_LEASE_LOST",
            Self::ClockInvalid => "LW_EVALUATION_CLOCK_INVALID",
            Self::WorkerIdentityInvalid => "LW_EVALUATION_WORKER_IDENTITY_INVALID",
            Self::AttemptOverflow => "LW_EVALUATION_ATTEMPT_OVERFLOW",
            Self::Database(_) | Self::Persistence(_) => "LW_EVALUATION_DATABASE_FAILED",
            Self::Event(_) => "LW_EVALUATION_OUTBOX_IDENTITY_INVALID",
        }
    }
}
