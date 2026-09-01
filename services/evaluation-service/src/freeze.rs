//! Immutable preflight, freeze, Object Lock, and durable publication orchestration.
#![allow(
    missing_docs,
    reason = "the versioned contracts and stable diagnostics define the external surface"
)]

use persistence_sqlx::Sha256Digest; // internal persistence hash, not contract hash
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use artifact_store::{ImmutableObjectStore, ObjectStoreError};
use contracts::authoring::RuntimeKind;
use contracts::submission::{
    FrozenEnvironmentIdentity, FrozenSubmission, SubmissionManifest, SubmissionSource,
};
use contracts::{
    ActorId, AgentRunId, CourseId, FrozenSubmissionId, RetentionClass, RetentionSnapshot, Revision,
};
use serde::{Deserialize, Serialize};

use crate::collector::{CollectError, SnapshotCollector, SnapshotSource, SnapshotTransport};
use crate::freeze_store::{BeginFreeze, FreezeStoreError, PgFreezeStore};

const DEFAULT_LEASE_TTL: Duration = Duration::from_mins(15);

/// Internal authenticated command produced from an approved `SubmissionManifest` projection.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FreezeRequest {
    pub frozen_submission_id: FrozenSubmissionId,
    pub course_id: CourseId,
    pub actor_id: ActorId,
    pub agent_run_id: AgentRunId,
    pub manifest_revision: Revision,
    pub manifest: SubmissionManifest,
    pub environment: FrozenEnvironmentIdentity,
    pub retention: RetentionSnapshot,
    pub idempotency_key: String,
    pub trace_id: String,
}

/// Production orchestration boundary shared by PVC and SSH/SFTP sources.
#[derive(Clone)]
pub struct FreezeService {
    store: PgFreezeStore,
    object_store: Arc<dyn ImmutableObjectStore>,
    collector: SnapshotCollector,
    object_prefix: String,
    worker_id: String,
    lease_ttl: Duration,
}

impl std::fmt::Debug for FreezeService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FreezeService")
            .field("object_prefix", &self.object_prefix)
            .field("worker_id", &self.worker_id)
            .field("lease_ttl", &self.lease_ttl)
            .finish_non_exhaustive()
    }
}

impl FreezeService {
    /// Creates the fail-closed service with one explicit object-store binding and worker identity.
    ///
    /// # Errors
    ///
    /// Returns `LW_COLLECT_CONFIG_INVALID` for an unsafe prefix or worker identity.
    pub fn new(
        store: PgFreezeStore,
        object_store: Arc<dyn ImmutableObjectStore>,
        collector: SnapshotCollector,
        object_prefix: &str,
        worker_id: &str,
    ) -> Result<Self, FreezeServiceError> {
        let object_prefix = object_prefix.trim_matches('/');
        if object_prefix.is_empty()
            || object_prefix.len() > 256
            || object_prefix.contains("..")
            || object_prefix.chars().any(char::is_control)
            || worker_id.trim().is_empty()
            || worker_id.len() > 128
            || worker_id.chars().any(char::is_control)
        {
            return Err(FreezeServiceError::ConfigurationInvalid);
        }
        Ok(Self {
            store,
            object_store,
            collector,
            object_prefix: object_prefix.to_owned(),
            worker_id: worker_id.to_owned(),
            lease_ttl: DEFAULT_LEASE_TTL,
        })
    }

    /// Freezes one exact source or returns the durable result for the same idempotency identity.
    ///
    /// # Errors
    ///
    /// Returns a stable [`FreezeServiceError`] and never publishes a partial result.
    #[allow(
        clippy::too_many_lines,
        reason = "the linear freeze sequence makes every failure-to-attempt transition auditable"
    )]
    pub async fn freeze(
        &self,
        request: &FreezeRequest,
        source: &dyn SnapshotSource,
    ) -> Result<FrozenSubmission, FreezeServiceError> {
        let started = Instant::now();
        validate_request(request, source)?;
        let submission_manifest_sha256 = Sha256Digest::of_canonical(&request.manifest)
            .map_err(|_| FreezeServiceError::ContractInvalid)?;
        let request_sha256 = request_hash(request, source, submission_manifest_sha256)?;
        let lease = match self
            .store
            .begin(
                request.frozen_submission_id,
                request.course_id,
                request.environment.environment_id,
                &request.idempotency_key,
                request_sha256,
                source.identity(),
                &self.worker_id,
                self.lease_ttl,
            )
            .await?
        {
            BeginFreeze::Acquired(lease) => {
                tracing::info!(
                    event = "evaluation.freeze.claimed",
                    component = "freeze-worker",
                    operation = "submission.freeze",
                    outcome = "claimed",
                    duration_ms = elapsed_millis(started),
                    trace_id = request.trace_id,
                    run_id = %request.agent_run_id,
                    environment_id = %request.environment.environment_id,
                    course_id = %request.course_id,
                    resource_id = %request.frozen_submission_id,
                    attempt = lease.attempt,
                    worker = self.worker_id,
                );
                lease
            }
            BeginFreeze::Replay(submission) => {
                tracing::info!(
                    event = "evaluation.freeze.idempotency_replayed",
                    component = "freeze-worker",
                    operation = "submission.freeze",
                    outcome = "replayed",
                    duration_ms = elapsed_millis(started),
                    trace_id = request.trace_id,
                    run_id = %request.agent_run_id,
                    environment_id = %request.environment.environment_id,
                    course_id = %request.course_id,
                    resource_id = %request.frozen_submission_id,
                );
                return Ok(*submission);
            }
            BeginFreeze::Conflict => return Err(FreezeServiceError::IdempotencyConflict),
            BeginFreeze::InProgress => return Err(FreezeServiceError::InProgress),
        };
        if request.retention.retain_until.get() <= lease.authority_now.get() {
            self.fail_attempt(&lease, "LW_COLLECT_CONTRACT_INVALID", true)
                .await?;
            return Err(FreezeServiceError::ContractInvalid);
        }
        self.store.mark_preflighting(&lease).await?;
        tracing::debug!(
            event = "evaluation.freeze.stage_started",
            component = "freeze-worker",
            operation = "submission.freeze.preflight",
            outcome = "started",
            duration_ms = elapsed_millis(started),
            trace_id = request.trace_id,
            resource_id = %request.frozen_submission_id,
            attempt = lease.attempt,
        );
        let preflight = match self.collector.preflight(source, &request.manifest).await {
            Ok(preflight) => preflight,
            Err(error) => {
                tracing::warn!(
                    event = "evaluation.freeze.stage_failed",
                    component = "freeze-worker",
                    operation = "submission.freeze.preflight",
                    outcome = "failed",
                    duration_ms = elapsed_millis(started),
                    trace_id = request.trace_id,
                    resource_id = %request.frozen_submission_id,
                    attempt = lease.attempt,
                    diagnostic_code = error.diagnostic_code(),
                    error_kind = "collector_preflight_failed",
                    failure_stage = "submission.freeze.preflight",
                    retryable = true,
                    safe_detail = "collector_preflight_rejected",
                );
                self.fail_attempt(&lease, error.diagnostic_code(), true)
                    .await?;
                return Err(FreezeServiceError::Collect(error));
            }
        };
        let archive = match self
            .collector
            .freeze(source, &request.manifest, &preflight)
            .await
        {
            Ok(archive) => archive,
            Err(error) => {
                tracing::warn!(
                    event = "evaluation.freeze.stage_failed",
                    component = "freeze-worker",
                    operation = "submission.freeze.collect",
                    outcome = "failed",
                    duration_ms = elapsed_millis(started),
                    trace_id = request.trace_id,
                    resource_id = %request.frozen_submission_id,
                    attempt = lease.attempt,
                    diagnostic_code = error.diagnostic_code(),
                    error_kind = "collector_freeze_failed",
                    failure_stage = "submission.freeze.collect",
                    retryable = true,
                    safe_detail = "collector_freeze_rejected",
                );
                self.fail_attempt(&lease, error.diagnostic_code(), true)
                    .await?;
                return Err(FreezeServiceError::Collect(error));
            }
        };
        let object_key = format!(
            "{}/{}/attempt-{}.json",
            self.object_prefix, lease.frozen_submission_id, lease.attempt
        );
        self.store.mark_uploading(&lease, &object_key).await?;
        let verified = match self
            .object_store
            .put_governance_locked(
                &object_key,
                &archive.bytes,
                archive.media_type,
                lease.authority_now,
                request.retention.retain_until,
            )
            .await
        {
            Ok(verified) => verified,
            Err(error) => {
                tracing::error!(
                    event = "evaluation.freeze.object_store_failed",
                    component = "freeze-worker",
                    operation = "submission.freeze.upload",
                    outcome = "failed",
                    duration_ms = elapsed_millis(started),
                    trace_id = request.trace_id,
                    resource_id = %lease.frozen_submission_id,
                    attempt = lease.attempt,
                    diagnostic_code = error.diagnostic_code(),
                    error_kind = "object_store_failed",
                    failure_stage = "submission.freeze.upload",
                    retryable = false,
                    safe_detail = "object_store_write_failed",
                );
                self.fail_attempt(&lease, error.diagnostic_code(), false)
                    .await?;
                return Err(FreezeServiceError::ObjectStore(error));
            }
        };
        if verified.bytes != archive.bytes
            || verified.reference.size_bytes
                != u64::try_from(archive.bytes.len()).unwrap_or(u64::MAX)
            || verified.reference.media_type != archive.media_type
            || verified.reference.object_version.trim().is_empty()
        {
            self.fail_attempt(&lease, "LW_OBJECT_LOCK_IDENTITY_MISMATCH", false)
                .await?;
            return Err(FreezeServiceError::ObjectIdentityMismatch);
        }
        let frozen_at = self.store.authority_now().await?;
        let submission = FrozenSubmission {
            id: lease.frozen_submission_id,
            course_id: request.course_id,
            actor_id: request.actor_id,
            agent_run_id: request.agent_run_id,
            attempt: lease.attempt,
            manifest_revision: request.manifest_revision,
            files: archive.files,
            object: verified.reference,
            environment: request.environment.clone(),
            retention: request.retention.clone(),
            system_facts: BTreeMap::new(),
            frozen_at,
            derived_archive: None,
        };
        if submission.validate().is_err() {
            self.fail_attempt(&lease, "LW_COLLECT_CONTRACT_INVALID", false)
                .await?;
            return Err(FreezeServiceError::ContractInvalid);
        }
        if let Err(error) = self
            .store
            .complete(&lease, &object_key, &submission, &request.trace_id)
            .await
        {
            tracing::error!(
                event = "evaluation.freeze.persistence_failed",
                component = "freeze-worker",
                operation = "submission.freeze.commit",
                outcome = "failed",
                duration_ms = elapsed_millis(started),
                trace_id = request.trace_id,
                resource_id = %lease.frozen_submission_id,
                attempt = lease.attempt,
                diagnostic_code = error.diagnostic_code(),
                error_kind = "persistence_failed",
                failure_stage = "submission.freeze.commit",
                retryable = false,
                safe_detail = "freeze_commit_failed",
            );
            self.fail_attempt(&lease, error.diagnostic_code(), false)
                .await?;
            return Err(FreezeServiceError::Store(error));
        }
        tracing::info!(
            event = "evaluation.freeze.completed",
            component = "freeze-worker",
            operation = "submission.freeze",
            outcome = "succeeded",
            duration_ms = elapsed_millis(started),
            trace_id = request.trace_id,
            run_id = %request.agent_run_id,
            environment_id = %request.environment.environment_id,
            course_id = %request.course_id,
            resource_id = %request.frozen_submission_id,
            attempt = lease.attempt,
        );
        Ok(submission)
    }

    async fn fail_attempt(
        &self,
        lease: &crate::freeze_store::FreezeLease,
        diagnostic_code: &'static str,
        cleanup_verified: bool,
    ) -> Result<(), FreezeServiceError> {
        self.store
            .fail(lease, diagnostic_code, cleanup_verified)
            .await
            .map_err(|_| {
                tracing::error!(
                    event = "evaluation.freeze.failure_persistence_failed",
                    component = "freeze-worker",
                    operation = "submission.freeze.failure_commit",
                    outcome = "failed",
                    duration_ms = 0_u64,
                    resource_id = %lease.frozen_submission_id,
                    attempt = lease.attempt,
                    diagnostic_code,
                    error_kind = "failure_persistence_failed",
                    failure_stage = "submission.freeze.failure_commit",
                    retryable = false,
                    safe_detail = "failure_persistence_failed",
                );
                FreezeServiceError::FailurePersistence
            })
    }
}

fn elapsed_millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn validate_request(
    request: &FreezeRequest,
    source: &dyn SnapshotSource,
) -> Result<(), FreezeServiceError> {
    request
        .manifest
        .validate()
        .map_err(|_| FreezeServiceError::ContractInvalid)?;
    let transport_matches = matches!(
        (request.environment.runtime_kind, source.transport()),
        (RuntimeKind::Container, SnapshotTransport::Pvc)
            | (RuntimeKind::VirtualMachine, SnapshotTransport::Ssh)
    );
    if request.manifest.source != SubmissionSource::Workspace
        || request.retention.class != RetentionClass::StudentSubmission
        || request.idempotency_key.trim().is_empty()
        || request.idempotency_key.len() > 512
        || request.idempotency_key.chars().any(char::is_control)
        || request.trace_id.trim().is_empty()
        || request.trace_id.len() > 128
        || request.trace_id.chars().any(char::is_control)
        || !transport_matches
    {
        return Err(FreezeServiceError::ContractInvalid);
    }
    Ok(())
}

fn request_hash(
    request: &FreezeRequest,
    source: &dyn SnapshotSource,
    submission_manifest_sha256: Sha256Digest,
) -> Result<Sha256Digest, FreezeServiceError> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct RequestIdentity<'a> {
        frozen_submission_id: FrozenSubmissionId,
        course_id: CourseId,
        actor_id: ActorId,
        agent_run_id: AgentRunId,
        manifest_revision: Revision,
        submission_manifest_sha256: Sha256Digest,
        environment: &'a FrozenEnvironmentIdentity,
        retention: &'a RetentionSnapshot,
        source_identity_sha256: Sha256Digest,
        transport: &'static str,
    }
    let transport = match source.transport() {
        SnapshotTransport::Pvc => "pvc",
        SnapshotTransport::Ssh => "ssh_sftp",
    };
    Sha256Digest::of_canonical(&RequestIdentity {
        frozen_submission_id: request.frozen_submission_id,
        course_id: request.course_id,
        actor_id: request.actor_id,
        agent_run_id: request.agent_run_id,
        manifest_revision: request.manifest_revision,
        submission_manifest_sha256,
        environment: &request.environment,
        retention: &request.retention,
        source_identity_sha256: source.identity(),
        transport,
    })
    .map_err(|_| FreezeServiceError::ContractInvalid)
}

/// Stable orchestration failures with no student paths or payloads.
#[derive(Debug, thiserror::Error)]
pub enum FreezeServiceError {
    #[error("LW_COLLECT_CONFIG_INVALID")]
    ConfigurationInvalid,
    #[error("LW_COLLECT_CONTRACT_INVALID")]
    ContractInvalid,
    #[error("LW_COLLECT_IDEMPOTENCY_CONFLICT")]
    IdempotencyConflict,
    #[error("LW_COLLECT_IN_PROGRESS")]
    InProgress,
    #[error(transparent)]
    Collect(#[from] CollectError),
    #[error(transparent)]
    ObjectStore(#[from] ObjectStoreError),
    #[error(transparent)]
    Store(#[from] FreezeStoreError),
    #[error("LW_OBJECT_LOCK_IDENTITY_MISMATCH")]
    ObjectIdentityMismatch,
    #[error("LW_COLLECT_FAILURE_PERSIST_FAILED")]
    FailurePersistence,
}

impl FreezeServiceError {
    /// Returns the stable diagnostic without exposing source paths or provider payloads.
    #[must_use]
    pub const fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::ConfigurationInvalid => "LW_COLLECT_CONFIG_INVALID",
            Self::ContractInvalid => "LW_COLLECT_CONTRACT_INVALID",
            Self::IdempotencyConflict => "LW_COLLECT_IDEMPOTENCY_CONFLICT",
            Self::InProgress => "LW_COLLECT_IN_PROGRESS",
            Self::Collect(error) => error.diagnostic_code(),
            Self::ObjectStore(error) => error.diagnostic_code(),
            Self::Store(error) => error.diagnostic_code(),
            Self::ObjectIdentityMismatch => "LW_OBJECT_LOCK_IDENTITY_MISMATCH",
            Self::FailurePersistence => "LW_COLLECT_FAILURE_PERSIST_FAILED",
        }
    }
}
