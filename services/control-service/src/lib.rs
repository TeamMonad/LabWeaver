//! Production Control domain: immutable material, policy, approval, release and SSE state.
#![allow(
    clippy::missing_errors_doc,
    clippy::too_many_lines,
    reason = "transactional use cases keep their complete consistency boundary visible"
)]

pub mod api;
pub mod clients;
pub mod messaging;

use std::collections::BTreeSet;
use std::str::FromStr;
use std::sync::Arc;

use artifact_store::{ImmutableObjectStore, ObjectStoreError};
use contracts::authoring::{
    AgentTrackKind, CandidateApproval, CandidateDecision, CourseLlmEgressPolicy,
    EnvironmentCandidate, EvaluationCandidate, PackageFile, ProblemPackage, RuntimeKind,
};
use contracts::evaluation::EvaluationRuntimeIdentity;
use contracts::events::{
    AgentBuildFailed, AgentBuildRequested, AgentRunEvent, CloudEvent,
    ReleasePublished, ReleaseWithdrawn, SPEC_VERSION, subjects,
};
use contracts::http::{
    CandidateBuildState, CandidateBuildView, CandidateDecisionRequest,
    CreateEnvironmentTemplateReleaseRequest, CreateEvaluationReleaseRequest,
    CreateProblemPackageUploadRequest, EnvironmentCandidateView, EvaluationCandidateView,
    IdempotencyKey, InternalPublishEvaluationReleaseRequest, ProblemPackageUploadFile,
    ProblemPackageUploadSession, ProblemPackageUploadTarget,
};
use contracts::supply_chain::{
    BuildNetworkPolicy, BuildRequest, EnvironmentTemplateRelease, EnvironmentTemplateReleaseView,
    ImageArtifact, ReleaseWithdrawal, VirtualMachineBaseDisk, VirtualMachineDiskFormat,
};
use contracts::{
    ActorId, ApprovalId, BuildRequestId, CandidateId, CourseId, DiagnosticCode, EventId,
    ImageArtifactId, PolicyId, ProblemPackageId, ReleaseId, RetentionClass, RetentionDisposition,
    RetentionSnapshot, Revision, Sequence, UploadSessionId, UtcTimestamp,
};
use persistence_sqlx::Sha256Digest; // internal persistence hash, not contract hash
use persistence_sqlx::{
    Domain, IdempotencyDecision, IdempotencyStore, InboxDecision, InboxStore, OutboxStore,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Row, Transaction};
use thiserror::Error;
use time::Duration;
use uuid::Uuid;

const CREATE_UPLOAD: &str = "control_create_problem_package_upload_v1";
const COMPLETE_UPLOAD: &str = "control_complete_problem_package_upload_v1";
const CREATE_POLICY: &str = "control_create_llm_policy_v1";
const DECIDE_CANDIDATE: &str = "control_decide_candidate_v1";
const CREATE_RELEASE: &str = "control_create_environment_template_release_v1";
const WITHDRAW_RELEASE: &str = "control_withdraw_environment_template_release_v1";
const BUILD_REQUEST_SUBJECT: &str = subjects::AGENT_BUILD_REQUESTED;
const RELEASE_SUBJECT: &str = subjects::ENVIRONMENT_TEMPLATE_RELEASE_PUBLISHED;
const WITHDRAWAL_SUBJECT: &str = subjects::ENVIRONMENT_TEMPLATE_RELEASE_WITHDRAWN;

/// Non-secret Control behavior configuration.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ControlConfig {
    /// Object-key prefix already constrained by the object store binding.
    pub package_object_prefix: String,
    /// Short-lived upload session duration.
    pub upload_ttl_seconds: u64,
    /// Fencing lease for one completion worker; must exceed one object-store request timeout.
    pub completion_lease_seconds: u64,
    /// Maximum files in one package.
    pub max_package_files: usize,
    /// Maximum aggregate package size.
    pub max_package_bytes: u64,
    /// Retention policy identity frozen into completed packages.
    pub retention_policy_id: PolicyId,
    /// Course material retention duration.
    pub retention_seconds: u64,
    /// Durable SSE retention duration.
    pub sse_retention_seconds: u64,
    /// Active supply-chain trust policy revision required by new decisions.
    pub trust_revision: Revision,
    /// Exact image-policy identity accepted for publication.
    pub image_policy_id: PolicyId,
    /// Exact active image-policy revision accepted for publication.
    pub image_policy_revision: Revision,
    /// Frozen Environment candidate schema identity.
    pub environment_schema_sha256: Sha256Digest,
    /// Frozen Evaluation candidate schema identity.
    pub evaluation_schema_sha256: Sha256Digest,
    /// Exact build execution policy used to turn an approved Container candidate into a command.
    pub container_build: ContainerBuildPolicy,
    /// Exact deployment-owned `KubeVirt` base disk accepted for VM publication.
    pub virtual_machine_base: VirtualMachineBasePolicy,
    /// Single deployment-owned Evaluation runtime identity template.
    pub evaluation_runtime: EvaluationRuntimePolicy,
}

/// Non-secret immutable Evaluation runtime fields; package identity is derived per candidate.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvaluationRuntimePolicy {
    /// Clean source identity for the deployed Evaluation worker.
    pub source_sha256: Sha256Digest,
    /// Exact registered Evaluation provider.
    pub provider_binding: String,
    /// Sanitized effective runtime configuration identity.
    pub configuration_sha256: Sha256Digest,
    /// Checked-in Evaluation Migration catalog identity.
    pub migration_catalog_sha256: Sha256Digest,
    /// Digest-pinned Evaluation runner image.
    pub runner_image: String,
    /// Immutable worker/runtime artifact identity.
    pub runtime_artifact_sha256: Sha256Digest,
}

impl EvaluationRuntimePolicy {
    fn identity(&self) -> Result<EvaluationRuntimeIdentity, ControlError> {
        let identity = EvaluationRuntimeIdentity {
            provider_binding: self.provider_binding.clone(),
            runner_image: self.runner_image.clone(),
        };
        identity
            .validate()
            .map_err(|_| ControlError::ConfigurationInvalid)?;
        Ok(identity)
    }
}

/// Deployment-owned, non-secret limits and bindings for approved Container builds.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContainerBuildPolicy {
    /// Exact registered `BuildKit` provider binding.
    pub builder_binding: String,
    /// Harbor registry/project prefix; Control appends one course-bound repository name.
    pub output_repository_prefix: String,
    /// Candidate-context-relative Dockerfile path.
    pub dockerfile_path: String,
    /// Explicit build-time network posture.
    pub network: BuildNetworkPolicy,
    /// Hard end-to-end build deadline.
    pub max_duration_milliseconds: u64,
    /// `BuildKit` CPU ceiling in millicores.
    pub max_cpu_millicores: u32,
    /// `BuildKit` memory ceiling in bytes.
    pub max_memory_bytes: u64,
}

/// Deployment-owned fixed `KubeVirt` artifact and provider bindings.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VirtualMachineBasePolicy {
    /// Exact Environment provider binding accepted in the candidate.
    pub provider_binding: String,
    /// Exact reviewed storage binding accepted in the candidate.
    pub storage_class_binding: String,
    /// Stable release artifact identity assigned to this deployment-owned disk.
    pub artifact_id: ImageArtifactId,
    /// Immutable CDI source and imported disk identity.
    pub base_disk: VirtualMachineBaseDisk,
    /// Exact disk encoding exposed to the runtime provider.
    pub format: VirtualMachineDiskFormat,
}

impl ControlConfig {
    /// Rejects unsafe or unbounded configuration.
    pub fn validate(&self) -> Result<(), ControlError> {
        let package_prefix_valid = !self.package_object_prefix.trim_matches('/').is_empty();
        let upload_ttl_valid = (1..=3_600).contains(&self.upload_ttl_seconds);
        let completion_lease_valid = (30..=3_600).contains(&self.completion_lease_seconds);
        let package_files_valid = (1..=10_000).contains(&self.max_package_files);
        let package_bytes_valid = self.max_package_bytes != 0;
        let retention_valid = self.retention_seconds != 0 && self.sse_retention_seconds != 0;
        let container_build_valid = self.container_build.validate();
        let virtual_machine_base_valid = self.virtual_machine_base.validate();
        let evaluation_runtime_valid = self.evaluation_runtime.identity().is_ok();
        if !(package_prefix_valid
            && upload_ttl_valid
            && completion_lease_valid
            && package_files_valid
            && package_bytes_valid
            && retention_valid
            && container_build_valid
            && virtual_machine_base_valid
            && evaluation_runtime_valid)
        {
            tracing::error!(
                event = "control.configuration_invalid",
                package_prefix_valid,
                upload_ttl_valid,
                completion_lease_valid,
                package_files_valid,
                package_bytes_valid,
                retention_valid,
                container_build_valid,
                virtual_machine_base_valid,
                evaluation_runtime_valid,
                "deployment-owned Control policy failed validation"
            );
            return Err(ControlError::ConfigurationInvalid);
        }
        Ok(())
    }
}

impl ContainerBuildPolicy {
    fn validate(&self) -> bool {
        let prefix = self.output_repository_prefix.trim_end_matches('/');
        let repository_scope = prefix.split_once('/').filter(|(registry, project)| {
            !registry.is_empty()
                && !project.is_empty()
                && !project.contains('/')
                && project
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        });
        !self.builder_binding.trim().is_empty()
            && !self
                .builder_binding
                .bytes()
                .any(|byte| byte.is_ascii_whitespace())
            && !prefix.is_empty()
            && !prefix.contains("://")
            && !prefix.contains('@')
            && !prefix.contains("..")
            && repository_scope.is_some()
            && !self.dockerfile_path.trim().is_empty()
            && self.max_duration_milliseconds > 0
            && self.max_cpu_millicores > 0
            && self.max_memory_bytes > 0
            && match &self.network {
                BuildNetworkPolicy::DenyAll => true,
                BuildNetworkPolicy::Restricted { allowed_registries } => {
                    !allowed_registries.is_empty()
                        && allowed_registries.iter().all(|registry| {
                            !registry.trim().is_empty()
                                && !registry.contains("://")
                                && !registry.contains('*')
                        })
                }
            }
    }
}

impl VirtualMachineBasePolicy {
    fn validate(&self) -> bool {
        !self.provider_binding.trim().is_empty()
            && !self.storage_class_binding.trim().is_empty()
            && self.base_disk.validate().is_ok()
    }
}

/// Control-owned transactional service.
#[derive(Clone)]
pub struct ControlService {
    pool: PgPool,
    objects: Arc<dyn ImmutableObjectStore>,
    config: ControlConfig,
}

enum CompletionReservation {
    Replay(ProblemPackage),
    Claimed(Uuid),
}

impl std::fmt::Debug for ControlService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ControlService")
            .field("pool", &self.pool)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl ControlService {
    /// Creates a service from an explicit Control-role pool and immutable object binding.
    pub fn new(
        pool: PgPool,
        objects: Arc<dyn ImmutableObjectStore>,
        config: ControlConfig,
    ) -> Result<Self, ControlError> {
        config.validate()?;
        Ok(Self {
            pool,
            objects,
            config,
        })
    }

    /// Creates one idempotent upload authority without persisting credentials or original paths.
    pub async fn create_upload(
        &self,
        course_id: CourseId,
        request: &CreateProblemPackageUploadRequest,
        idempotency_key: &IdempotencyKey,
        now: UtcTimestamp,
    ) -> Result<ProblemPackageUploadSession, ControlError> {
        validate_upload_request(request, &self.config)?;
        let request_hash = canonical_hash(&json!({"courseId":course_id,"request":request}))?;
        let upload_id = UploadSessionId::new();
        let revision = Revision::new(1).map_err(|_| ControlError::ContractInvalid)?;
        let expires_at = add_seconds(now, self.config.upload_ttl_seconds)?;

        let mut files = request.files.clone();
        files.sort_by(|left, right| left.path.cmp(&right.path));
        let mut targets = Vec::with_capacity(files.len());
        let mut object_keys = Vec::with_capacity(files.len());
        for (ordinal, file) in files.iter().enumerate() {
            let key = format!(
                "{}/{}/{}/{ordinal:05}",
                self.config.package_object_prefix.trim_matches('/'),
                course_id,
                upload_id,
            );
            let signed = self
                .objects
                .presign_upload(&key, file.size_bytes, &file.media_type, now)
                .await?;
            if signed.expires_at != expires_at {
                return Err(ControlError::ObjectStoreIdentityMismatch);
            }
            object_keys.push(key);
            targets.push(ProblemPackageUploadTarget {
                path: file.path.clone(),
                upload_url: signed.url,
                required_headers: signed.required_headers,
                expires_at,
            });
        }

        let session = ProblemPackageUploadSession {
            id: upload_id,
            course_id,
            revision,
            files: files.clone(),
            upload_targets: targets,
            expires_at,
        };
        let result = serde_json::to_value(&session).map_err(|_| ControlError::ContractInvalid)?;
        let mut transaction = self.pool.begin().await.map_err(db)?;
        match IdempotencyStore::reserve(
            &mut transaction,
            Domain::Control,
            CREATE_UPLOAD,
            idempotency_key.as_str(),
            request_hash,
        )
        .await
        .map_err(|_| ControlError::PersistenceFailed)?
        {
            IdempotencyDecision::Replay(value) => {
                transaction.rollback().await.map_err(db)?;
                return serde_json::from_value(value)
                    .map_err(|_| ControlError::PersistenceIdentityMismatch);
            }
            IdempotencyDecision::Conflict => return Err(ControlError::IdempotencyConflict),
            IdempotencyDecision::InProgress => return Err(ControlError::OperationInProgress),
            IdempotencyDecision::Reserved => {}
        }
        sqlx::query(
            "INSERT INTO control.problem_package_upload_sessions \
             (upload_id,course_id,revision,state,retention_policy_revision,expires_at) \
             VALUES ($1,$2,1,'pending',$3,$4)",
        )
        .bind(upload_id.as_uuid())
        .bind(course_id.as_uuid())
        .bind(i64_revision(request.retention_policy_revision)?)
        .bind(expires_at.get())
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        for (ordinal, (file, key)) in files.iter().zip(object_keys).enumerate() {
            sqlx::query(
                "INSERT INTO control.problem_package_upload_files \
                 (upload_id,ordinal,path,object_key,size_bytes,sha256,media_type) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7)",
            )
            .bind(upload_id.as_uuid())
            .bind(i32::try_from(ordinal).map_err(|_| ControlError::ContractInvalid)?)
            .bind(&file.path)
            .bind(key)
            .bind(i64::try_from(file.size_bytes).map_err(|_| ControlError::ContractInvalid)?)
            .bind(Sha256Digest::of_bytes(file.path.as_bytes()).to_string())
            .bind(&file.media_type)
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
        }
        IdempotencyStore::complete(
            &mut transaction,
            Domain::Control,
            CREATE_UPLOAD,
            idempotency_key.as_str(),
            &result,
        )
        .await
        .map_err(|_| ControlError::PersistenceFailed)?;
        transaction.commit().await.map_err(db)?;
        Ok(session)
    }

    /// Completes a package after independently freezing every exact object version.
    pub async fn complete_upload(
        &self,
        course_id: CourseId,
        upload_id: UploadSessionId,
        expected_revision: Revision,
        idempotency_key: &IdempotencyKey,
        now: UtcTimestamp,
    ) -> Result<ProblemPackage, ControlError> {
        let request_hash = canonical_hash(&json!({
            "courseId": course_id,
            "uploadId": upload_id,
            "expectedRevision": expected_revision,
        }))?;
        let completion_lease = match self
            .reserve_completion(
                course_id,
                upload_id,
                expected_revision,
                idempotency_key,
                request_hash,
            )
            .await?
        {
            CompletionReservation::Replay(replay) => return Ok(replay),
            CompletionReservation::Claimed(lease) => lease,
        };
        let rows = sqlx::query(
            "SELECT path,object_key,size_bytes,sha256,media_type,object_version,artifact_id \
             FROM control.problem_package_upload_files WHERE upload_id=$1 ORDER BY ordinal",
        )
        .bind(upload_id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        if rows.is_empty() {
            return self
                .fail_upload(
                    upload_id,
                    completion_lease,
                    idempotency_key,
                    "LW_PACKAGE_FILES_MISSING",
                    &[],
                )
                .await;
        }
        let _declared_manifest = rows
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
        if false {
            return self
                .fail_upload(
                    upload_id,
                    completion_lease,
                    idempotency_key,
                    "LW_PACKAGE_HASH_MISMATCH",
                    &[],
                )
                .await;
        }
        let mut package_files = Vec::with_capacity(rows.len());
        let mut frozen_versions = Vec::with_capacity(rows.len());
        for row in rows {
            self.renew_completion_lease(upload_id, completion_lease)
                .await?;
            let path: String = row.try_get("path").map_err(db)?;
            let key: String = row.try_get("object_key").map_err(db)?;
            let size = u64::try_from(row.try_get::<i64, _>("size_bytes").map_err(db)?)
                .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
            let _sha256 = row
                .try_get::<String, _>("sha256")
                .map_err(db)?
                .parse::<Sha256Digest>()
                .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
            let media_type: String = row.try_get("media_type").map_err(db)?;
            let stored_version: Option<String> = row.try_get("object_version").map_err(db)?;
            let stored_artifact: Option<Uuid> = row.try_get("artifact_id").map_err(db)?;
            let verified = match (stored_version, stored_artifact) {
                (Some(version), Some(artifact_id)) => self
                    .objects
                    .read_verified(&key, &version, size, &media_type)
                    .await
                    .map(|mut object| {
                        object.reference.artifact_id = artifact_id_from_uuid(artifact_id)?;
                        Ok(object)
                    })
                    .map_err(ControlError::from)
                    .and_then(std::convert::identity),
                (None, None) => self
                    .objects
                    .freeze_current(&key, size, &media_type)
                    .await
                    .map_err(ControlError::from),
                _ => Err(ControlError::PersistenceIdentityMismatch),
            };
            let Ok(object) = verified else {
                tracing::warn!(
                    event = "control.problem_package.object_verification_failed",
                    component = "problem-package",
                    operation = "problem_package.object.verify",
                    outcome = "failed",
                    duration_ms = 0_u64,
                    diagnostic_code = "LW_PACKAGE_OBJECT_VERIFICATION_FAILED",
                    error_kind = "object_verification_failed",
                    failure_stage = "problem_package.object.verify",
                    retryable = false,
                    safe_detail = "object_verification_failed",
                    upload_id = %upload_id,
                );
                return self
                    .fail_upload(
                        upload_id,
                        completion_lease,
                        idempotency_key,
                        "LW_PACKAGE_OBJECT_VERIFICATION_FAILED",
                        &frozen_versions,
                    )
                    .await;
            };
            self.record_frozen_version(upload_id, completion_lease, &key, &object.reference, now)
                .await?;
            frozen_versions.push((
                key,
                object.reference.object_version.clone(),
                object.reference.artifact_id,
            ));
            package_files.push(PackageFile {
                path,
                object: object.reference,
            });
        }
        let _package_manifest_sha256 = canonical_hash(&package_files)?;
        let session = sqlx::query(
            "SELECT retention_policy_revision FROM control.problem_package_upload_sessions \
             WHERE upload_id=$1 AND course_id=$2 AND state='completing' AND completion_lease_token=$3 AND completion_lease_expires_at>now()",
        )
        .bind(upload_id.as_uuid())
        .bind(course_id.as_uuid())
        .bind(completion_lease)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?
        .ok_or(ControlError::UploadStateConflict)?;
        let policy_revision =
            revision_from_i64(session.try_get("retention_policy_revision").map_err(db)?)?;
        let package = ProblemPackage {
            id: ProblemPackageId::new(),
            course_id,
            revision: Revision::new(1).map_err(|_| ControlError::ContractInvalid)?,
            files: package_files,
            retention: RetentionSnapshot {
                policy_id: self.config.retention_policy_id,
                policy_revision,
                class: RetentionClass::CourseMaterial,
                retain_until: add_seconds(now, self.config.retention_seconds)?,
                disposition: RetentionDisposition::Delete,
            },
            completed_at: now,
        };
        package
            .validate()
            .map_err(|_| ControlError::ContractInvalid)?;
        self.commit_completed_package(
            upload_id,
            completion_lease,
            idempotency_key,
            &package,
            &frozen_versions,
        )
        .await?;
        Ok(package)
    }

    /// Activates one append-only course policy under a course-scoped lock.
    pub async fn activate_policy(
        &self,
        course_id: CourseId,
        mut policy: CourseLlmEgressPolicy,
        idempotency_key: &IdempotencyKey,
    ) -> Result<CourseLlmEgressPolicy, ControlError> {
        if policy.course_id != course_id {
            return Err(ControlError::CourseMismatch);
        }
        policy.validate().map_err(|_| ControlError::PolicyInvalid)?;
        let request_hash = canonical_hash(&policy)?;
        let mut transaction = self.pool.begin().await.map_err(db)?;
        advisory_course_lock(&mut transaction, course_id).await?;
        match IdempotencyStore::reserve(
            &mut transaction,
            Domain::Control,
            CREATE_POLICY,
            idempotency_key.as_str(),
            request_hash,
        )
        .await
        .map_err(|_| ControlError::PersistenceFailed)?
        {
            IdempotencyDecision::Replay(value) => {
                transaction.rollback().await.map_err(db)?;
                return serde_json::from_value(value)
                    .map_err(|_| ControlError::PersistenceIdentityMismatch);
            }
            IdempotencyDecision::Conflict => return Err(ControlError::IdempotencyConflict),
            IdempotencyDecision::InProgress => return Err(ControlError::OperationInProgress),
            IdempotencyDecision::Reserved => {}
        }
        let next = sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(MAX(revision),0)+1 FROM control.course_llm_policies WHERE course_id=$1",
        )
        .bind(course_id.as_uuid())
        .fetch_one(&mut *transaction)
        .await
        .map_err(db)?;
        policy.revision = revision_from_i64(next)?;
        sqlx::query(
            "UPDATE control.course_llm_policies SET superseded_at=$2 \
             WHERE course_id=$1 AND superseded_at IS NULL",
        )
        .bind(course_id.as_uuid())
        .bind(policy.activated_at.get())
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        let contract = serde_json::to_value(&policy).map_err(|_| ControlError::ContractInvalid)?;
        sqlx::query(
            "INSERT INTO control.course_llm_policies \
             (policy_id,course_id,revision,contract_sha256,contract,activated_at) \
             VALUES ($1,$2,$3,$4,$5,$6)",
        )
        .bind(policy.id.as_uuid())
        .bind(course_id.as_uuid())
        .bind(i64_revision(policy.revision)?)
        .bind(canonical_hash(&policy)?.to_string())
        .bind(&contract)
        .bind(policy.activated_at.get())
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        append_sse(
            &mut transaction,
            course_id,
            "course_llm_policy.activated.v1",
            policy.id.as_uuid(),
            policy.revision,
            json!({"policyId":policy.id,"revision":policy.revision}),
        )
        .await?;
        IdempotencyStore::complete(
            &mut transaction,
            Domain::Control,
            CREATE_POLICY,
            idempotency_key.as_str(),
            &contract,
        )
        .await
        .map_err(|_| ControlError::PersistenceFailed)?;
        transaction.commit().await.map_err(db)?;
        Ok(policy)
    }

    /// Returns the active course policy without fallback to historical revisions.
    pub async fn active_policy(
        &self,
        course_id: CourseId,
    ) -> Result<CourseLlmEgressPolicy, ControlError> {
        load_contract(
            &self.pool,
            "SELECT contract FROM control.course_llm_policies WHERE course_id=$1 AND superseded_at IS NULL",
            course_id.as_uuid(),
        )
        .await
        .map_err(|error| match error {
            ControlError::NotFound => ControlError::PolicyNotFound,
            other => other,
        })
    }

    /// Reads one completed immutable package in the exact course scope.
    pub async fn package(
        &self,
        course_id: CourseId,
        package_id: ProblemPackageId,
    ) -> Result<ProblemPackage, ControlError> {
        load_contract_two(
            &self.pool,
            "SELECT contract FROM control.problem_packages WHERE package_id=$1 AND course_id=$2",
            package_id.as_uuid(),
            course_id.as_uuid(),
        )
        .await
    }

    /// Resolves internal opaque object keys for an exact completed package.
    pub async fn package_object_locators(
        &self,
        course_id: CourseId,
        package: &ProblemPackage,
    ) -> Result<std::collections::BTreeMap<contracts::ArtifactId, String>, ControlError> {
        if package.course_id != course_id {
            return Err(ControlError::CourseMismatch);
        }
        let rows = sqlx::query(
            "SELECT files.artifact_id,files.object_key FROM control.problem_package_upload_files files \
             JOIN control.problem_package_upload_sessions sessions ON sessions.upload_id=files.upload_id \
             WHERE sessions.course_id=$1 AND sessions.completed_package_id=$2 AND sessions.state='completed'",
        )
        .bind(course_id.as_uuid())
        .bind(package.id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        let mut locators = std::collections::BTreeMap::new();
        for row in rows {
            let artifact_id = contracts::ArtifactId::from_str(
                &row.try_get::<Uuid, _>("artifact_id")
                    .map_err(db)?
                    .to_string(),
            )
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
            if locators
                .insert(artifact_id, row.try_get("object_key").map_err(db)?)
                .is_some()
            {
                return Err(ControlError::PersistenceIdentityMismatch);
            }
        }
        let expected = package
            .files
            .iter()
            .map(|file| file.object.artifact_id)
            .collect::<BTreeSet<_>>();
        if locators.keys().copied().collect::<BTreeSet<_>>() != expected {
            return Err(ControlError::PersistenceIdentityMismatch);
        }
        Ok(locators)
    }

    /// Reads the latest durable `AgentRun` projection without crossing into the Agent schema.
    pub async fn agent_run(
        &self,
        course_id: CourseId,
        run_id: contracts::AgentRunId,
    ) -> Result<contracts::authoring::AgentRun, ControlError> {
        load_contract_two(
            &self.pool,
            "SELECT contract FROM control.agent_run_projections WHERE run_id=$1 AND course_id=$2",
            run_id.as_uuid(),
            course_id.as_uuid(),
        )
        .await
    }

    /// Reads a validated Environment candidate projection.
    pub async fn environment_candidate(
        &self,
        course_id: CourseId,
        candidate_id: CandidateId,
    ) -> Result<EnvironmentCandidate, ControlError> {
        load_candidate_contract(&self.pool, course_id, candidate_id, "environment").await
    }

    /// Reads the Control-owned teacher view for one Environment candidate.
    pub async fn environment_candidate_view(
        &self,
        course_id: CourseId,
        candidate_id: CandidateId,
    ) -> Result<EnvironmentCandidateView, ControlError> {
        let candidate = self.environment_candidate(course_id, candidate_id).await?;
        let approvals = load_candidate_approvals(&self.pool, candidate_id).await?;
        let build = load_candidate_build(&self.pool, course_id, candidate_id).await?;
        Ok(EnvironmentCandidateView {
            candidate,
            approvals,
            build,
            trust_revision: self.config.trust_revision,
        })
    }

    /// Reads a validated Evaluation candidate projection.
    pub async fn evaluation_candidate(
        &self,
        course_id: CourseId,
        candidate_id: CandidateId,
    ) -> Result<EvaluationCandidate, ControlError> {
        load_candidate_contract(&self.pool, course_id, candidate_id, "evaluation").await
    }

    /// Reads the Control-owned teacher view for one Evaluation candidate.
    pub async fn evaluation_candidate_view(
        &self,
        course_id: CourseId,
        candidate_id: CandidateId,
    ) -> Result<EvaluationCandidateView, ControlError> {
        let candidate = self.evaluation_candidate(course_id, candidate_id).await?;
        let approvals = load_candidate_approvals(&self.pool, candidate_id).await?;
        Ok(EvaluationCandidateView {
            candidate,
            approvals,
            trust_revision: self.config.trust_revision,
        })
    }

    /// Builds the exact Control-authorized command; browser input cannot select runtime identity.
    pub async fn prepare_evaluation_release(
        &self,
        course_id: CourseId,
        request: &CreateEvaluationReleaseRequest,
        published_by: ActorId,
    ) -> Result<InternalPublishEvaluationReleaseRequest, ControlError> {
        let candidate = self
            .evaluation_candidate(course_id, request.candidate_id)
            .await?;
        if candidate.revision != request.candidate_revision {
            return Err(ControlError::ReleaseCandidateMismatch);
        }
        let approval: CandidateApproval = load_contract_two(
            &self.pool,
            "SELECT contract FROM control.candidate_approvals WHERE approval_id=$1 AND candidate_id=$2",
            request.approval_id.as_uuid(),
            request.candidate_id.as_uuid(),
        )
        .await?;
        let active_policy = sqlx::query_scalar::<_, i64>(
            "SELECT revision FROM control.course_llm_policies WHERE course_id=$1 AND superseded_at IS NULL",
        )
        .bind(course_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?
        .ok_or(ControlError::PolicyNotFound)?;
        if !approval.is_release_eligible(
            request.candidate_revision,
            revision_from_i64(active_policy)?,
            self.config.trust_revision,
        ) {
            return Err(ControlError::ReleaseCandidateMismatch);
        }
        let run = self.agent_run(course_id, candidate.run_id).await?;
        let package = self.package(course_id, run.package_id).await?;
        package
            .validate()
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        Ok(InternalPublishEvaluationReleaseRequest {
            course_id,
            candidate_id: candidate.id,
            candidate_revision: candidate.revision,
            approval_id: approval.id,
            approval_revision: Revision::new(1)
                .map_err(|_| ControlError::PersistenceIdentityMismatch)?,
            evaluation_spec: candidate.spec,
            runtime_identity: self.config.evaluation_runtime.identity()?,
            published_by,
        })
    }

    /// Reads one immutable Release together with its optional append-only withdrawal fact.
    pub async fn release(
        &self,
        course_id: CourseId,
        release_id: ReleaseId,
    ) -> Result<EnvironmentTemplateReleaseView, ControlError> {
        let row = sqlx::query(
            "SELECT releases.contract AS release_contract,withdrawals.contract AS withdrawal_contract \
             FROM control.environment_template_releases releases \
             LEFT JOIN control.release_withdrawals withdrawals ON withdrawals.release_id=releases.release_id \
             WHERE releases.release_id=$1 AND releases.course_id=$2",
        )
        .bind(release_id.as_uuid())
        .bind(course_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?
        .ok_or(ControlError::NotFound)?;
        release_view(&row)
    }

    /// Lists releases by immutable version with bounded pagination.
    pub async fn releases(
        &self,
        course_id: CourseId,
        after_version: u64,
        limit: u32,
    ) -> Result<Vec<EnvironmentTemplateReleaseView>, ControlError> {
        if limit == 0 || limit > 200 {
            return Err(ControlError::ContractInvalid);
        }
        let rows = sqlx::query(
            "SELECT releases.contract AS release_contract,withdrawals.contract AS withdrawal_contract \
             FROM control.environment_template_releases releases \
             LEFT JOIN control.release_withdrawals withdrawals ON withdrawals.release_id=releases.release_id \
             WHERE releases.course_id=$1 AND releases.version>$2 ORDER BY releases.version LIMIT $3",
        )
        .bind(course_id.as_uuid())
        .bind(i64::try_from(after_version).map_err(|_| ControlError::ContractInvalid)?)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        rows.into_iter().map(|row| release_view(&row)).collect()
    }

    /// Projects one Agent-owned candidate using its exact source event identity.
    pub async fn project_agent_run(
        &self,
        event_id: EventId,
        run: &contracts::authoring::AgentRun,
    ) -> Result<(), ControlError> {
        run.validate().map_err(|_| ControlError::ContractInvalid)?;
        let contract = serde_json::to_value(run).map_err(|_| ControlError::ContractInvalid)?;
        let hash = canonical_hash(run)?;
        let result = sqlx::query(
            "INSERT INTO control.agent_run_projections \
             (run_id,course_id,revision,state,contract_sha256,contract,projected_event_id) \
             VALUES ($1,$2,$3,$4,$5,$6,$7) \
             ON CONFLICT (run_id) DO UPDATE SET revision=EXCLUDED.revision,state=EXCLUDED.state, \
             contract_sha256=EXCLUDED.contract_sha256,contract=EXCLUDED.contract, \
             projected_event_id=EXCLUDED.projected_event_id,updated_at=now() \
             WHERE control.agent_run_projections.revision < EXCLUDED.revision",
        )
        .bind(run.id.as_uuid())
        .bind(run.course_id.as_uuid())
        .bind(i64_revision(run.revision)?)
        .bind(agent_run_state_name(run.state))
        .bind(hash.to_string())
        .bind(&contract)
        .bind(event_id.as_uuid())
        .execute(&self.pool)
        .await
        .map_err(db)?;
        if result.rows_affected() == 0 {
            let existing: Value = sqlx::query_scalar(
                "SELECT contract FROM control.agent_run_projections WHERE run_id=$1",
            )
            .bind(run.id.as_uuid())
            .fetch_one(&self.pool)
            .await
            .map_err(db)?;
            if existing != contract {
                return Err(ControlError::ProjectionConflict);
            }
        }
        Ok(())
    }

    /// Projects Agent-owned candidates using their exact source event identity.
    pub async fn project_candidates(
        &self,
        event_id: EventId,
        run: &contracts::authoring::AgentRun,
        environment: Option<&EnvironmentCandidate>,
        evaluation: Option<&EvaluationCandidate>,
    ) -> Result<(), ControlError> {
        run.validate().map_err(|_| ControlError::ContractInvalid)?;
        if environment.is_none() && evaluation.is_none() {
            return Err(ControlError::CandidateMissing);
        }
        let mut transaction = self.pool.begin().await.map_err(db)?;
        let run_contract = serde_json::to_value(run).map_err(|_| ControlError::ContractInvalid)?;
        sqlx::query(
            "INSERT INTO control.agent_run_projections \
             (run_id,course_id,revision,state,contract_sha256,contract,projected_event_id) \
             VALUES ($1,$2,$3,$4,$5,$6,$7) \
             ON CONFLICT (run_id) DO UPDATE SET revision=EXCLUDED.revision,state=EXCLUDED.state, \
             contract_sha256=EXCLUDED.contract_sha256,contract=EXCLUDED.contract, \
             projected_event_id=EXCLUDED.projected_event_id,updated_at=now() \
             WHERE control.agent_run_projections.revision < EXCLUDED.revision",
        )
        .bind(run.id.as_uuid())
        .bind(run.course_id.as_uuid())
        .bind(i64_revision(run.revision)?)
        .bind(agent_run_state_name(run.state))
        .bind(canonical_hash(run)?.to_string())
        .bind(run_contract)
        .bind(event_id.as_uuid())
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        if let Some(candidate) = environment {
            candidate
                .validate()
                .map_err(|_| ControlError::ContractInvalid)?;
            insert_candidate(
                &mut transaction,
                run.course_id,
                run.id,
                "environment",
                candidate.id,
                candidate.revision,
                canonical_hash(&candidate.spec)?,
                candidate.policy_revision,
                self.config.environment_schema_sha256,
                event_id,
                serde_json::to_value(candidate).map_err(|_| ControlError::ContractInvalid)?,
            )
            .await?;
        }
        if let Some(candidate) = evaluation {
            candidate
                .validate()
                .map_err(|_| ControlError::ContractInvalid)?;
            insert_candidate(
                &mut transaction,
                run.course_id,
                run.id,
                "evaluation",
                candidate.id,
                candidate.revision,
                canonical_hash(&candidate.spec)?,
                candidate.policy_revision,
                self.config.evaluation_schema_sha256,
                event_id,
                serde_json::to_value(candidate).map_err(|_| ControlError::ContractInvalid)?,
            )
            .await?;
        }
        transaction.commit().await.map_err(db)?;
        Ok(())
    }

    /// Consumes one sequenced Agent event and its authoritative readback in one transaction.
    pub async fn consume_agent_run_event(
        &self,
        event: &CloudEvent<AgentRunEvent>,
        run: &contracts::authoring::AgentRun,
        environment: Option<&EnvironmentCandidate>,
        evaluation: Option<&EvaluationCandidate>,
    ) -> Result<InboxDecision, ControlError> {
        let contract = contracts::events::EventContract::by_subject(&event.subject)
            .ok_or(ControlError::ContractInvalid)?;
        event
            .validate(contract)
            .map_err(|_| ControlError::ContractInvalid)?;
        run.validate().map_err(|_| ControlError::ContractInvalid)?;
        if event.data.run_id != run.id
            || event.course_id != run.course_id
            || event.aggregate_revision != run.revision
            || !event_matches_run(event, run, environment, evaluation)
        {
            return Err(ControlError::ProjectionConflict);
        }
        let payload = serde_json::to_value(event).map_err(|_| ControlError::ContractInvalid)?;
        let payload_hash = canonical_hash(&payload)?;
        let mut transaction = self.pool.begin().await.map_err(db)?;
        let decision = InboxStore::accept(
            &mut transaction,
            Domain::Control,
            "control_agent_run_projection_v1",
            event.id.as_uuid(),
            run.id.as_uuid(),
            event.aggregate_sequence.0,
            payload_hash,
        )
        .await
        .map_err(|_| ControlError::PersistenceFailed)?;
        match decision {
            InboxDecision::Duplicate | InboxDecision::Stale => {
                transaction.commit().await.map_err(db)?;
                return Ok(decision);
            }
            InboxDecision::Gap => {
                transaction.rollback().await.map_err(db)?;
                return Err(ControlError::EventSequenceGap);
            }
            InboxDecision::Accepted => {}
        }
        let run_contract = serde_json::to_value(run).map_err(|_| ControlError::ContractInvalid)?;
        let updated = sqlx::query(
            "INSERT INTO control.agent_run_projections \
             (run_id,course_id,revision,state,contract_sha256,contract,projected_event_id) \
             VALUES ($1,$2,$3,$4,$5,$6,$7) \
             ON CONFLICT (run_id) DO UPDATE SET revision=EXCLUDED.revision,state=EXCLUDED.state, \
             contract_sha256=EXCLUDED.contract_sha256,contract=EXCLUDED.contract, \
             projected_event_id=EXCLUDED.projected_event_id,updated_at=now() \
             WHERE control.agent_run_projections.revision < EXCLUDED.revision \
                OR (control.agent_run_projections.revision=EXCLUDED.revision \
                    AND control.agent_run_projections.contract_sha256=EXCLUDED.contract_sha256)",
        )
        .bind(run.id.as_uuid())
        .bind(run.course_id.as_uuid())
        .bind(i64_revision(run.revision)?)
        .bind(agent_run_state_name(run.state))
        .bind(canonical_hash(run)?.to_string())
        .bind(run_contract)
        .bind(event.id.as_uuid())
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        if updated.rows_affected() != 1 {
            let persisted_revision: i64 = sqlx::query_scalar(
                "SELECT revision FROM control.agent_run_projections WHERE run_id=$1",
            )
            .bind(run.id.as_uuid())
            .fetch_one(&mut *transaction)
            .await
            .map_err(db)?;
            if persisted_revision < i64_revision(event.aggregate_revision)? {
                return Err(ControlError::ProjectionConflict);
            }
        }
        if let Some(candidate) = environment {
            insert_candidate(
                &mut transaction,
                run.course_id,
                run.id,
                "environment",
                candidate.id,
                candidate.revision,
                canonical_hash(&candidate.spec)?,
                candidate.policy_revision,
                self.config.environment_schema_sha256,
                event.id,
                serde_json::to_value(candidate).map_err(|_| ControlError::ContractInvalid)?,
            )
            .await?;
        }
        if let Some(candidate) = evaluation {
            insert_candidate(
                &mut transaction,
                run.course_id,
                run.id,
                "evaluation",
                candidate.id,
                candidate.revision,
                canonical_hash(&candidate.spec)?,
                candidate.policy_revision,
                self.config.evaluation_schema_sha256,
                event.id,
                serde_json::to_value(candidate).map_err(|_| ControlError::ContractInvalid)?,
            )
            .await?;
        }
        append_sse(
            &mut transaction,
            run.course_id,
            "agent_run.state_changed.v1",
            run.id.as_uuid(),
            run.revision,
            json!({"runId":run.id,"revision":run.revision,"state":agent_run_state_name(run.state)}),
        )
        .await?;
        transaction.commit().await.map_err(db)?;
        Ok(decision)
    }

    /// Stores an Agent-owned artifact resolution for later exact release comparison.
    pub async fn project_artifact(
        &self,
        event_id: EventId,
        course_id: CourseId,
        artifact: &ImageArtifact,
    ) -> Result<(), ControlError> {
        artifact
            .validate()
            .map_err(|_| ControlError::ReleaseEvidenceInvalid)?;
        let artifact_id = image_artifact_id(artifact);
        let build_request_id =
            image_build_request_id(artifact).ok_or(ControlError::ArtifactMismatch)?;
        let runtime_kind = match artifact.runtime_kind() {
            RuntimeKind::Container => "container",
            RuntimeKind::VirtualMachine => "virtual_machine",
        };
        let artifact_json =
            serde_json::to_value(artifact).map_err(|_| ControlError::ContractInvalid)?;
        let evaluation_json = serde_json::json!({});
        let mut transaction = self.pool.begin().await.map_err(db)?;
        let build = sqlx::query(
            "SELECT course_id,state,image_artifact_id FROM control.container_build_projections \
             WHERE build_request_id=$1 FOR UPDATE",
        )
        .bind(build_request_id.as_uuid())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?
        .ok_or(ControlError::ArtifactNotAuthoritative)?;
        if build.try_get::<Uuid, _>("course_id").map_err(db)? != course_id.as_uuid() {
            return Err(ControlError::CourseMismatch);
        }
        let state: String = build.try_get("state").map_err(db)?;
        let existing_build_artifact: Option<Uuid> =
            build.try_get("image_artifact_id").map_err(db)?;
        if state != "requested"
            && !(state == "succeeded" && existing_build_artifact == Some(artifact_id.as_uuid()))
        {
            return Err(ControlError::ProjectionConflict);
        }
        let inserted = sqlx::query(
            "INSERT INTO control.image_artifact_projections \
             (image_artifact_id,runtime_kind,artifact_sha256,artifact,policy_evaluation,projected_event_id) \
             VALUES ($1,$2,$3,$4,$5,$6) \
             ON CONFLICT (image_artifact_id) DO NOTHING",
        )
        .bind(artifact_id.as_uuid())
        .bind(runtime_kind)
        .bind(canonical_hash(artifact)?.to_string())
        .bind(&artifact_json)
        .bind(&evaluation_json)
        .bind(event_id.as_uuid())
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        if inserted.rows_affected() == 0 {
            let existing = sqlx::query(
                "SELECT artifact \
                 FROM control.image_artifact_projections WHERE image_artifact_id=$1",
            )
            .bind(artifact_id.as_uuid())
            .fetch_one(&mut *transaction)
            .await
            .map_err(db)?;
            let existing_artifact: Value = existing.try_get("artifact").map_err(db)?;
            if existing_artifact != artifact_json {
                return Err(ControlError::ProjectionConflict);
            }
        }
        if state == "requested" {
            let updated = sqlx::query(
                "UPDATE control.container_build_projections \
                 SET state='succeeded',image_artifact_id=$2,terminal_event_id=$3, \
                     completed_at=clock_timestamp(),updated_at=clock_timestamp() \
                 WHERE build_request_id=$1 AND state='requested'",
            )
            .bind(build_request_id.as_uuid())
            .bind(artifact_id.as_uuid())
            .bind(event_id.as_uuid())
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
            if updated.rows_affected() != 1 {
                return Err(ControlError::ProjectionConflict);
            }
        }
        transaction.commit().await.map_err(db)
    }

    /// Persists one terminal Agent build failure without creating artifact authority.
    pub async fn project_build_failure(
        &self,
        event_id: EventId,
        course_id: CourseId,
        failure: &AgentBuildFailed,
    ) -> Result<(), ControlError> {
        failure
            .validate()
            .map_err(|_| ControlError::ContractInvalid)?;
        let terminal_state = if failure.diagnostic_code == "LW_AGENT_BUILD_CANCELLED" {
            "cancelled"
        } else {
            "failed"
        };
        let mut transaction = self.pool.begin().await.map_err(db)?;
        let row = sqlx::query(
            "SELECT course_id,command_sha256,state,terminal_diagnostic,cleanup_verified \
             FROM control.container_build_projections WHERE build_request_id=$1 FOR UPDATE",
        )
        .bind(failure.build_request_id.as_uuid())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?
        .ok_or(ControlError::ArtifactNotAuthoritative)?;
        let observed_course: Uuid = row.try_get("course_id").map_err(db)?;
        let _observed_command: String = row.try_get("command_sha256").map_err(db)?;
        let observed_state: String = row.try_get("state").map_err(db)?;
        if observed_course != course_id.as_uuid() {
            return Err(ControlError::CourseMismatch);
        }
        if observed_state == terminal_state {
            let diagnostic: Option<String> = row.try_get("terminal_diagnostic").map_err(db)?;
            let cleanup_verified: Option<bool> = row.try_get("cleanup_verified").map_err(db)?;
            if diagnostic.as_deref() != Some(&failure.diagnostic_code)
                || cleanup_verified != Some(failure.cleanup_verified)
            {
                return Err(ControlError::ProjectionConflict);
            }
            transaction.rollback().await.map_err(db)?;
            return Ok(());
        }
        if observed_state != "requested" {
            return Err(ControlError::ProjectionConflict);
        }
        let updated = sqlx::query(
            "UPDATE control.container_build_projections \
             SET state=$2,terminal_diagnostic=$3,cleanup_verified=$4,terminal_event_id=$5, \
                 completed_at=clock_timestamp(),updated_at=clock_timestamp() \
             WHERE build_request_id=$1 AND state='requested'",
        )
        .bind(failure.build_request_id.as_uuid())
        .bind(terminal_state)
        .bind(&failure.diagnostic_code)
        .bind(failure.cleanup_verified)
        .bind(event_id.as_uuid())
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        if updated.rows_affected() != 1 {
            return Err(ControlError::ProjectionConflict);
        }
        transaction.commit().await.map_err(db)
    }

    /// Appends the only decision allowed for an exact candidate revision.
    #[allow(clippy::too_many_arguments)]
    pub async fn decide_candidate(
        &self,
        course_id: CourseId,
        candidate_id: CandidateId,
        expected_kind: AgentTrackKind,
        request: &CandidateDecisionRequest,
        actor_id: ActorId,
        expected_revision: Revision,
        idempotency_key: &IdempotencyKey,
        now: UtcTimestamp,
    ) -> Result<CandidateApproval, ControlError> {
        if request.reason.trim().is_empty() || request.candidate_revision != expected_revision {
            return Err(ControlError::RevisionConflict);
        }
        let request_hash = canonical_hash(&json!({
            "courseId":course_id,"candidateId":candidate_id,"kind":expected_kind,
            "request":request,"actorId":actor_id
        }))?;
        let mut transaction = self.pool.begin().await.map_err(db)?;
        match IdempotencyStore::reserve(
            &mut transaction,
            Domain::Control,
            DECIDE_CANDIDATE,
            idempotency_key.as_str(),
            request_hash,
        )
        .await
        .map_err(|_| ControlError::PersistenceFailed)?
        {
            IdempotencyDecision::Replay(value) => {
                transaction.rollback().await.map_err(db)?;
                return serde_json::from_value(value)
                    .map_err(|_| ControlError::PersistenceIdentityMismatch);
            }
            IdempotencyDecision::Conflict => return Err(ControlError::IdempotencyConflict),
            IdempotencyDecision::InProgress => return Err(ControlError::OperationInProgress),
            IdempotencyDecision::Reserved => {}
        }
        let row = sqlx::query(
            "SELECT candidate_kind,revision,content_sha256,policy_revision,schema_sha256,contract \
             FROM control.candidates WHERE candidate_id=$1 AND course_id=$2 FOR UPDATE",
        )
        .bind(candidate_id.as_uuid())
        .bind(course_id.as_uuid())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?
        .ok_or(ControlError::CandidateNotFound)?;
        let observed_revision = revision_from_i64(row.try_get("revision").map_err(db)?)?;
        let observed_hash: Sha256Digest = row
            .try_get::<String, _>("content_sha256")
            .map_err(db)?
            .parse()
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        let observed_policy = revision_from_i64(row.try_get("policy_revision").map_err(db)?)?;
        let observed_schema: Sha256Digest = row
            .try_get::<String, _>("schema_sha256")
            .map_err(db)?
            .parse()
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        let candidate_contract: Value = row.try_get("contract").map_err(db)?;
        let kind: String = row.try_get("candidate_kind").map_err(db)?;
        let expected_kind_name = match expected_kind {
            AgentTrackKind::Environment => "environment",
            AgentTrackKind::Evaluation => "evaluation",
        };
        if kind != expected_kind_name {
            return Err(ControlError::CandidateKindMismatch);
        }
        let expected_schema = match kind.as_str() {
            "environment" => self.config.environment_schema_sha256,
            "evaluation" => self.config.evaluation_schema_sha256,
            _ => return Err(ControlError::PersistenceIdentityMismatch),
        };
        let active_policy_revision = sqlx::query_scalar::<_, i64>(
            "SELECT revision FROM control.course_llm_policies WHERE course_id=$1 AND superseded_at IS NULL",
        )
        .bind(course_id.as_uuid())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?
        .ok_or(ControlError::PolicyNotFound)?;
        if observed_revision != request.candidate_revision
            || observed_policy != request.policy_revision
            || observed_schema != expected_schema
            || revision_from_i64(active_policy_revision)? != request.policy_revision
            || request.trust_revision != self.config.trust_revision
        {
            return Err(ControlError::RevisionConflict);
        }
        let _ = observed_hash;
        let approval = CandidateApproval {
            id: ApprovalId::new(),
            candidate_id,
            candidate_revision: request.candidate_revision,
            policy_revision: request.policy_revision,
            trust_revision: request.trust_revision,
            actor_id,
            decision: request.decision,
            reason: request.reason.clone(),
            decided_at: now,
        };
        let contract =
            serde_json::to_value(&approval).map_err(|_| ControlError::ContractInvalid)?;
        sqlx::query(
            "INSERT INTO control.candidate_approvals \
             (approval_id,candidate_id,candidate_revision,decision,actor_id,decision_sha256,contract,decided_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
        )
        .bind(approval.id.as_uuid())
        .bind(candidate_id.as_uuid())
        .bind(i64_revision(approval.candidate_revision)?)
        .bind(decision_name(approval.decision))
        .bind(actor_id.as_uuid())
        .bind(canonical_hash(&approval)?.to_string())
        .bind(&contract)
        .bind(now.get())
        .execute(&mut *transaction)
        .await
        .map_err(|error| {
            if is_unique_violation(&error) {
                ControlError::DecisionConflict
            } else {
                db(error)
            }
        })?;
        if approval.decision == CandidateDecision::Approved
            && expected_kind == AgentTrackKind::Environment
        {
            let candidate: EnvironmentCandidate = serde_json::from_value(candidate_contract)
                .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
            candidate
                .validate()
                .map_err(|_| ControlError::ContractInvalid)?;
            if let contracts::authoring::EnvironmentRuntimeSpec::Container {
                build_context,
                base_image_digest,
                ..
            } = &candidate.spec.runtime
            {
                let context_object_key = sqlx::query_scalar::<_, String>(
                    "SELECT f.object_key \
                     FROM control.problem_package_upload_files f \
                     JOIN control.problem_package_upload_sessions s ON s.upload_id=f.upload_id \
                     WHERE f.artifact_id=$1 AND f.object_version=$2 AND s.course_id=$3 \
                       AND s.state='completed' AND f.verified_at IS NOT NULL",
                )
                .bind(build_context.artifact_id.as_uuid())
                .bind(&build_context.object_version)
                .bind(course_id.as_uuid())
                .fetch_optional(&mut *transaction)
                .await
                .map_err(db)?
                .ok_or(ControlError::PersistenceIdentityMismatch)?;
                let build_request = BuildRequest {
                    id: BuildRequestId::new(),
                    course_id,
                    candidate_id,
                    candidate_revision: candidate.revision,
                    approval_id: approval.id,
                    builder_binding: self.config.container_build.builder_binding.clone(),
                    context: build_context.clone(),
                    context_object_key,
                    dockerfile_path: self.config.container_build.dockerfile_path.clone(),
                    base_image_digest: base_image_digest.clone(),
                    output_repository: format!(
                        "{}/course-{course_id}-{candidate_id}",
                        self.config
                            .container_build
                            .output_repository_prefix
                            .trim_end_matches('/')
                    ),
                    network: self.config.container_build.network.clone(),
                    max_duration_milliseconds: self
                        .config
                        .container_build
                        .max_duration_milliseconds,
                    max_cpu_millicores: self.config.container_build.max_cpu_millicores,
                    max_memory_bytes: self.config.container_build.max_memory_bytes,
                    created_at: now,
                };
                build_request
                    .validate()
                    .map_err(|_| ControlError::ContractInvalid)?;
                let build_idempotency_key = format!("approval:{}", approval.id);
                let command = AgentBuildRequested {
                    request: build_request,
                    approval: approval.clone(),
                    idempotency_key: build_idempotency_key,
                };
                command
                    .validate()
                    .map_err(|_| ControlError::ContractInvalid)?;
                // candidate_sha256 and command_sha256 are internal persistence hashes (not contract hashes)
                // computed via canonical JSON to preserve idempotency without dummy values.
                let candidate_sha256 = canonical_hash(&candidate.spec)?;
                let command_sha256 = canonical_hash(&command)?;
                sqlx::query(
                    "INSERT INTO control.container_build_projections \
                     (build_request_id,course_id,candidate_id,candidate_revision,candidate_sha256, \
                      approval_id,command_sha256,state,contract,created_at) \
                     VALUES ($1,$2,$3,$4,$5,$6,$7,'requested',$8,$9)",
                )
                .bind(command.request.id.as_uuid())
                .bind(course_id.as_uuid())
                .bind(candidate_id.as_uuid())
                .bind(
                    i64::try_from(command.request.candidate_revision.get())
                        .map_err(|_| ControlError::ContractInvalid)?,
                )
                .bind(candidate_sha256.to_string())
                .bind(approval.id.as_uuid())
                .bind(command_sha256.to_string())
                .bind(serde_json::to_value(&command).map_err(|_| ControlError::ContractInvalid)?)
                .bind(now.get())
                .execute(&mut *transaction)
                .await
                .map_err(db)?;
                let event_id = EventId::new();
                let contract = contracts::events::EventContract::by_subject(BUILD_REQUEST_SUBJECT).ok_or(ControlError::ContractInvalid)?;
                let event = CloudEvent {
                    specversion: SPEC_VERSION.to_owned(),
                    id: event_id,
                    source: contract.source().to_owned(),
                    event_type: BUILD_REQUEST_SUBJECT.to_owned(),
                    subject: BUILD_REQUEST_SUBJECT.to_owned(),
                    time: now,
                    datacontenttype: "application/json".to_owned(),
                    dataschema: contract.data_schema(),
                    course_id,
                    aggregate_revision: Revision::new(1)
                        .map_err(|_| ControlError::ContractInvalid)?,
                    aggregate_sequence: Sequence(1),
                    trace_id: format!("build:{}", command.request.id),
                    data: command,
                };
                event
                    .validate(contract)
                    .map_err(|_| ControlError::ContractInvalid)?;
                let payload =
                    serde_json::to_value(&event).map_err(|_| ControlError::ContractInvalid)?;
                OutboxStore::enqueue(
                    &mut transaction,
                    Domain::Control,
                    event_id.as_uuid(),
                    BUILD_REQUEST_SUBJECT,
                    BUILD_REQUEST_SUBJECT,
                    event.data.request.id.as_uuid(),
                    1,
                    &payload,
                    canonical_hash(&payload)?,
                )
                .await
                .map_err(|_| ControlError::PersistenceFailed)?;
            }
        }
        append_sse(
            &mut transaction,
            course_id,
            &format!("{kind}_candidate.decision.v1"),
            candidate_id.as_uuid(),
            approval.candidate_revision,
            json!({
                "candidateId":candidate_id,"approvalId":approval.id,
                "revision":approval.candidate_revision,"decision":approval.decision
            }),
        )
        .await?;
        IdempotencyStore::complete(
            &mut transaction,
            Domain::Control,
            DECIDE_CANDIDATE,
            idempotency_key.as_str(),
            &contract,
        )
        .await
        .map_err(|_| ControlError::PersistenceFailed)?;
        transaction.commit().await.map_err(db)?;
        Ok(approval)
    }

    /// Publishes an immutable environment-first release from authoritative projections only.
    pub async fn create_release(
        &self,
        course_id: CourseId,
        request: &CreateEnvironmentTemplateReleaseRequest,
        actor_id: ActorId,
        idempotency_key: &IdempotencyKey,
        now: UtcTimestamp,
        trace_id: &str,
    ) -> Result<EnvironmentTemplateRelease, ControlError> {
        validate_trace_id(trace_id)?;
        let request_hash = canonical_hash(&json!({
            "courseId":course_id,"request":request,"actorId":actor_id
        }))?;
        let mut transaction = self.pool.begin().await.map_err(db)?;
        advisory_course_lock(&mut transaction, course_id).await?;
        match IdempotencyStore::reserve(
            &mut transaction,
            Domain::Control,
            CREATE_RELEASE,
            idempotency_key.as_str(),
            request_hash,
        )
        .await
        .map_err(|_| ControlError::PersistenceFailed)?
        {
            IdempotencyDecision::Replay(value) => {
                transaction.rollback().await.map_err(db)?;
                return serde_json::from_value(value)
                    .map_err(|_| ControlError::PersistenceIdentityMismatch);
            }
            IdempotencyDecision::Conflict => return Err(ControlError::IdempotencyConflict),
            IdempotencyDecision::InProgress => return Err(ControlError::OperationInProgress),
            IdempotencyDecision::Reserved => {}
        }
        let candidate = sqlx::query(
            "SELECT candidate_kind,revision,content_sha256,policy_revision,schema_sha256,contract FROM control.candidates \
             WHERE candidate_id=$1 AND course_id=$2 FOR SHARE",
        )
        .bind(request.candidate_id.as_uuid())
        .bind(course_id.as_uuid())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?
        .ok_or(ControlError::CandidateNotFound)?;
        if candidate
            .try_get::<String, _>("candidate_kind")
            .map_err(db)?
            != "environment"
            || revision_from_i64(candidate.try_get("revision").map_err(db)?)?
                != request.candidate_revision
        {
            return Err(ControlError::ReleaseCandidateMismatch);
        }
        let environment_candidate: EnvironmentCandidate =
            serde_json::from_value(candidate.try_get::<Value, _>("contract").map_err(db)?)
                .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        environment_candidate
            .validate()
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        if environment_candidate.spec.runtime.kind() != request.runtime_kind {
            return Err(ControlError::ReleaseCandidateMismatch);
        }
        let approval: CandidateApproval = load_contract_tx(
            &mut transaction,
            "SELECT contract FROM control.candidate_approvals WHERE approval_id=$1 AND candidate_id=$2",
            request.approval_id.as_uuid(),
            request.candidate_id.as_uuid(),
        )
        .await?;
        let candidate_policy =
            revision_from_i64(candidate.try_get("policy_revision").map_err(db)?)?;
        let candidate_schema: Sha256Digest = candidate
            .try_get::<String, _>("schema_sha256")
            .map_err(db)?
            .parse()
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        let spec_sha256 = candidate
            .try_get::<String, _>("content_sha256")
            .map_err(db)?;
        let active_policy = sqlx::query_scalar::<_, i64>("SELECT revision FROM control.course_llm_policies WHERE course_id=$1 AND superseded_at IS NULL")
            .bind(course_id.as_uuid()).fetch_optional(&mut *transaction).await.map_err(db)?.ok_or(ControlError::PolicyNotFound)?;
        if !approval.is_release_eligible(
            request.candidate_revision,
            revision_from_i64(active_policy)?,
            self.config.trust_revision,
        ) || candidate_policy != approval.policy_revision
        {
            return Err(ControlError::ReleaseCandidateMismatch);
        }
        let _ = candidate_schema;
        let artifact = match &environment_candidate.spec.runtime {
            contracts::authoring::EnvironmentRuntimeSpec::Container { .. } => {
                // candidate_sha256 is internal persistence hash (not contract hash)
                let candidate_sha256 = canonical_hash(&environment_candidate.spec)?;
                let projection = sqlx::query(
                    "SELECT artifacts.artifact \
                     FROM control.container_build_projections builds \
                     JOIN control.image_artifact_projections artifacts \
                       ON artifacts.image_artifact_id=builds.image_artifact_id \
                     WHERE builds.course_id=$1 AND builds.candidate_id=$2 \
                       AND builds.candidate_revision=$3 AND builds.candidate_sha256=$4 \
                       AND builds.approval_id=$5 AND builds.state='succeeded' FOR SHARE",
                )
                .bind(course_id.as_uuid())
                .bind(request.candidate_id.as_uuid())
                .bind(
                    i64::try_from(request.candidate_revision.get())
                        .map_err(|_| ControlError::ReleaseCandidateMismatch)?,
                )
                .bind(candidate_sha256.to_string())
                .bind(approval.id.as_uuid())
                .fetch_optional(&mut *transaction)
                .await
                .map_err(db)?
                .ok_or(ControlError::ArtifactNotAuthoritative)?;
                let artifact: ImageArtifact =
                    serde_json::from_value(projection.try_get("artifact").map_err(db)?)
                        .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
                if !matches!(artifact, ImageArtifact::Container { .. }) {
                    return Err(ControlError::ArtifactMismatch);
                }
                artifact
            }
            contracts::authoring::EnvironmentRuntimeSpec::VirtualMachine {
                provider_binding,
                base_disk,
                storage_class_binding,
                ..
            } => {
                let policy = &self.config.virtual_machine_base;
                if provider_binding != &policy.provider_binding
                    || storage_class_binding != &policy.storage_class_binding
                    || base_disk != &policy.base_disk
                {
                    return Err(ControlError::ArtifactMismatch);
                }
                ImageArtifact::VirtualMachine {
                    id: policy.artifact_id,
                    base_disk: policy.base_disk.clone(),
                    format: policy.format,
                }
            }
        };
        let artifact_id = artifact.id();
        let next_version = sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(MAX(version),0)+1 FROM control.environment_template_releases WHERE course_id=$1",
        )
        .bind(course_id.as_uuid())
        .fetch_one(&mut *transaction)
        .await
        .map_err(db)?;
        let release = EnvironmentTemplateRelease {
            id: ReleaseId::new(),
            course_id,
            version: u64::try_from(next_version)
                .map_err(|_| ControlError::PersistenceIdentityMismatch)?,
            candidate_id: request.candidate_id,
            agent_run_id: environment_candidate.run_id,
            candidate_revision: request.candidate_revision,
            runtime_kind: request.runtime_kind,
            approval,
            artifact,
            published_by: actor_id,
            published_at: now,
        };
        release
            .validate()
            .map_err(|_| ControlError::ReleaseEvidenceInvalid)?;
        let contract = serde_json::to_value(&release).map_err(|_| ControlError::ContractInvalid)?;
        sqlx::query(
            "INSERT INTO control.environment_template_releases \
             (release_id,course_id,version,environment_candidate_id,candidate_revision, \
              spec_sha256,image_artifact_id,contract,published_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)",
        )
        .bind(release.id.as_uuid())
        .bind(course_id.as_uuid())
        .bind(next_version)
        .bind(release.candidate_id.as_uuid())
        .bind(i64_revision(release.candidate_revision)?)
        .bind(&spec_sha256)
        .bind(artifact_id.as_uuid())
        .bind(&contract)
        .bind(now.get())
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        let event_id = EventId::new();
        let _projection_sha256 = canonical_hash(&json!({
            "release": release,
            "environmentSpec": environment_candidate.spec,
        }))?;
        let event = CloudEvent {
            specversion: SPEC_VERSION.to_owned(),
            id: event_id,
            source: "urn:labweaver:control-service".to_owned(),
            event_type: RELEASE_SUBJECT.to_owned(),
            subject: RELEASE_SUBJECT.to_owned(),
            time: now,
            datacontenttype: "application/json".to_owned(),
            dataschema: contracts::events::EventContract::by_subject(RELEASE_SUBJECT).ok_or(ControlError::ContractInvalid)?.data_schema(),
            course_id,
            aggregate_revision: Revision::new(release.version)
                .map_err(|_| ControlError::ContractInvalid)?,
            aggregate_sequence: Sequence(1),
            trace_id: trace_id.to_owned(),
            data: ReleasePublished {
                release: release.clone(),
                environment_spec: environment_candidate.spec,
            },
        };
        event
            .validate(contracts::events::EventContract::by_subject(RELEASE_SUBJECT).ok_or(ControlError::ContractInvalid)?)
            .map_err(|_| ControlError::ContractInvalid)?;
        let event_payload =
            serde_json::to_value(&event).map_err(|_| ControlError::ContractInvalid)?;
        OutboxStore::enqueue(
            &mut transaction,
            Domain::Control,
            event_id.as_uuid(),
            RELEASE_SUBJECT,
            RELEASE_SUBJECT,
            release.id.as_uuid(),
            1,
            &event_payload,
            canonical_hash(&event_payload)?,
        )
        .await
        .map_err(|_| ControlError::PersistenceFailed)?;
        append_sse(
            &mut transaction,
            course_id,
            RELEASE_SUBJECT,
            release.id.as_uuid(),
            Revision::new(release.version).map_err(|_| ControlError::ContractInvalid)?,
            json!({
                "releaseId":release.id,"version":release.version,
                "environmentSpecSha256":spec_sha256,
                "highSeverityWarnings":0
            }),
        )
        .await?;
        IdempotencyStore::complete(
            &mut transaction,
            Domain::Control,
            CREATE_RELEASE,
            idempotency_key.as_str(),
            &contract,
        )
        .await
        .map_err(|_| ControlError::PersistenceFailed)?;
        transaction.commit().await.map_err(db)?;
        Ok(release)
    }

    /// Appends withdrawal without changing the immutable release.
    #[allow(clippy::too_many_arguments)]
    pub async fn withdraw_release(
        &self,
        course_id: CourseId,
        release_id: ReleaseId,
        expected_version: u64,
        actor_id: ActorId,
        reason_code: &str,
        idempotency_key: &IdempotencyKey,
        now: UtcTimestamp,
        trace_id: &str,
    ) -> Result<ReleaseWithdrawal, ControlError> {
        if !valid_reason_code(reason_code) || expected_version == 0 {
            return Err(ControlError::ContractInvalid);
        }
        validate_trace_id(trace_id)?;
        let request_hash = canonical_hash(&json!({
            "courseId":course_id,"releaseId":release_id,"expectedVersion":expected_version,
            "actorId":actor_id,"reasonCode":reason_code
        }))?;
        let mut transaction = self.pool.begin().await.map_err(db)?;
        match IdempotencyStore::reserve(
            &mut transaction,
            Domain::Control,
            WITHDRAW_RELEASE,
            idempotency_key.as_str(),
            request_hash,
        )
        .await
        .map_err(|_| ControlError::PersistenceFailed)?
        {
            IdempotencyDecision::Replay(value) => {
                transaction.rollback().await.map_err(db)?;
                return serde_json::from_value(value)
                    .map_err(|_| ControlError::PersistenceIdentityMismatch);
            }
            IdempotencyDecision::Conflict => return Err(ControlError::IdempotencyConflict),
            IdempotencyDecision::InProgress => return Err(ControlError::OperationInProgress),
            IdempotencyDecision::Reserved => {}
        }
        let version = sqlx::query_scalar::<_, i64>(
            "SELECT version FROM control.environment_template_releases \
             WHERE release_id=$1 AND course_id=$2 FOR SHARE",
        )
        .bind(release_id.as_uuid())
        .bind(course_id.as_uuid())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?
        .ok_or(ControlError::ReleaseNotFound)?;
        if u64::try_from(version).ok() != Some(expected_version) {
            return Err(ControlError::RevisionConflict);
        }
        let withdrawal = ReleaseWithdrawal {
            release_id,
            release_version: expected_version,
            actor_id,
            reason_code: reason_code.to_owned(),
            withdrawn_at: now,
        };
        let contract =
            serde_json::to_value(&withdrawal).map_err(|_| ControlError::ContractInvalid)?;
        sqlx::query(
            "INSERT INTO control.release_withdrawals \
             (release_id,release_version,actor_id,reason_code,withdrawn_at,contract) \
             VALUES ($1,$2,$3,$4,$5,$6)",
        )
        .bind(release_id.as_uuid())
        .bind(version)
        .bind(actor_id.as_uuid())
        .bind(reason_code)
        .bind(now.get())
        .bind(&contract)
        .execute(&mut *transaction)
        .await
        .map_err(|error| {
            if is_unique_violation(&error) {
                ControlError::DecisionConflict
            } else {
                db(error)
            }
        })?;
        let event_id = EventId::new();
        let event = CloudEvent {
            specversion: SPEC_VERSION.to_owned(),
            id: event_id,
            source: "urn:labweaver:control-service".to_owned(),
            event_type: WITHDRAWAL_SUBJECT.to_owned(),
            subject: WITHDRAWAL_SUBJECT.to_owned(),
            time: now,
            datacontenttype: "application/json".to_owned(),
            dataschema: contracts::events::EventContract::by_subject(WITHDRAWAL_SUBJECT).ok_or(ControlError::ContractInvalid)?.data_schema(),
            course_id,
            aggregate_revision: Revision::new(expected_version)
                .map_err(|_| ControlError::ContractInvalid)?,
            aggregate_sequence: Sequence(2),
            trace_id: trace_id.to_owned(),
            data: ReleaseWithdrawn {
                release_id,
                version: expected_version,
                actor_id,
                reason_code: reason_code.to_owned(),
                withdrawn_at: now,
            },
        };
        event
            .validate(contracts::events::EventContract::by_subject(WITHDRAWAL_SUBJECT).ok_or(ControlError::ContractInvalid)?)
            .map_err(|_| ControlError::ContractInvalid)?;
        let event_payload =
            serde_json::to_value(&event).map_err(|_| ControlError::ContractInvalid)?;
        OutboxStore::enqueue(
            &mut transaction,
            Domain::Control,
            event_id.as_uuid(),
            WITHDRAWAL_SUBJECT,
            WITHDRAWAL_SUBJECT,
            release_id.as_uuid(),
            2,
            &event_payload,
            canonical_hash(&event_payload)?,
        )
        .await
        .map_err(|_| ControlError::PersistenceFailed)?;
        append_sse(
            &mut transaction,
            course_id,
            WITHDRAWAL_SUBJECT,
            release_id.as_uuid(),
            Revision::new(expected_version).map_err(|_| ControlError::ContractInvalid)?,
            json!({"releaseId":release_id,"version":expected_version,"reasonCode":reason_code}),
        )
        .await?;
        IdempotencyStore::complete(
            &mut transaction,
            Domain::Control,
            WITHDRAW_RELEASE,
            idempotency_key.as_str(),
            &contract,
        )
        .await
        .map_err(|_| ControlError::PersistenceFailed)?;
        transaction.commit().await.map_err(db)?;
        Ok(withdrawal)
    }

    /// Reads a bounded SSE page after validating retention and cursor continuity.
    pub async fn sse_page(
        &self,
        course_id: CourseId,
        after: Option<u64>,
        limit: u32,
        now: UtcTimestamp,
    ) -> Result<Vec<SseRecord>, ControlError> {
        if limit == 0 || limit > 1_000 {
            return Err(ControlError::ContractInvalid);
        }
        let cutoff = now.get()
            - Duration::seconds(
                i64::try_from(self.config.sse_retention_seconds)
                    .map_err(|_| ControlError::ConfigurationInvalid)?,
            );
        if let Some(cursor) = after {
            let last = sqlx::query_scalar::<_, i64>(
                "SELECT last_sequence FROM control.sse_course_cursors WHERE course_id=$1",
            )
            .bind(course_id.as_uuid())
            .fetch_optional(&self.pool)
            .await
            .map_err(db)?
            .unwrap_or(0);
            let cursor = i64::try_from(cursor).map_err(|_| ControlError::SseCursorGap)?;
            if cursor > last {
                return Err(ControlError::SseCursorGap);
            }
            let exists = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM control.sse_events \
                 WHERE course_id=$1 AND sequence=$2 AND created_at >= $3)",
            )
            .bind(course_id.as_uuid())
            .bind(cursor)
            .bind(cutoff)
            .fetch_one(&self.pool)
            .await
            .map_err(db)?;
            if !exists && cursor != 0 {
                return Err(ControlError::SseCursorExpired);
            }
        }
        let rows = sqlx::query(
            "SELECT sequence,event_type,aggregate_id,aggregate_revision,payload,created_at \
             FROM control.sse_events WHERE course_id=$1 AND sequence>$2 AND created_at >= $3 \
             ORDER BY sequence LIMIT $4",
        )
        .bind(course_id.as_uuid())
        .bind(i64::try_from(after.unwrap_or(0)).map_err(|_| ControlError::SseCursorGap)?)
        .bind(cutoff)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        rows.iter().map(sse_record).collect()
    }

    /// Deletes at most one orphaned exact object version under a database fence.
    pub async fn cleanup_one_object(
        &self,
        now: UtcTimestamp,
    ) -> Result<CleanupOutcome, ControlError> {
        let mut transaction = self.pool.begin().await.map_err(db)?;
        let row = sqlx::query(
            "SELECT object_key,object_version,attempts FROM control.object_cleanup_ledger \
             WHERE completed_at IS NULL AND next_attempt_at <= $1 \
             ORDER BY next_attempt_at,object_key FOR UPDATE SKIP LOCKED LIMIT 1",
        )
        .bind(now.get())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?;
        let Some(row) = row else {
            transaction.rollback().await.map_err(db)?;
            return Ok(CleanupOutcome::Idle);
        };
        let key: String = row.try_get("object_key").map_err(db)?;
        let version: String = row.try_get("object_version").map_err(db)?;
        let attempts: i32 = row.try_get("attempts").map_err(db)?;
        if self.objects.delete_orphan(&key, &version).await.is_ok() {
            let updated = sqlx::query(
                "UPDATE control.object_cleanup_ledger SET completed_at=$3,last_diagnostic=NULL \
                     WHERE object_key=$1 AND object_version=$2 AND completed_at IS NULL",
            )
            .bind(&key)
            .bind(&version)
            .bind(now.get())
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
            if updated.rows_affected() != 1 {
                return Err(ControlError::CleanupFenceLost);
            }
            transaction.commit().await.map_err(db)?;
            Ok(CleanupOutcome::Deleted)
        } else {
            let next_attempt = attempts
                .checked_add(1)
                .ok_or(ControlError::CleanupAttemptsExhausted)?;
            if next_attempt > 20 {
                return Err(ControlError::CleanupAttemptsExhausted);
            }
            let exponent = u32::try_from(next_attempt.min(10))
                .map_err(|_| ControlError::CleanupAttemptsExhausted)?;
            let delay = 1_i64
                .checked_shl(exponent)
                .ok_or(ControlError::CleanupAttemptsExhausted)?;
            sqlx::query(
                "UPDATE control.object_cleanup_ledger SET attempts=$3,next_attempt_at=$4, \
                     last_diagnostic='LW_OBJECT_CLEANUP_RETRY' \
                     WHERE object_key=$1 AND object_version=$2 AND completed_at IS NULL",
            )
            .bind(&key)
            .bind(&version)
            .bind(next_attempt)
            .bind(now.get() + Duration::seconds(delay))
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
            transaction.commit().await.map_err(db)?;
            Ok(CleanupOutcome::RetryScheduled {
                attempt: u32::try_from(next_attempt)
                    .map_err(|_| ControlError::CleanupAttemptsExhausted)?,
            })
        }
    }

    /// Purges only SSE facts older than the configured retention cutoff.
    pub async fn purge_expired_sse(&self, now: UtcTimestamp) -> Result<u64, ControlError> {
        let cutoff = now.get()
            - Duration::seconds(
                i64::try_from(self.config.sse_retention_seconds)
                    .map_err(|_| ControlError::ConfigurationInvalid)?,
            );
        Ok(
            sqlx::query("DELETE FROM control.sse_events WHERE created_at < $1")
                .bind(cutoff)
                .execute(&self.pool)
                .await
                .map_err(db)?
                .rows_affected(),
        )
    }

    async fn reserve_completion(
        &self,
        course_id: CourseId,
        upload_id: UploadSessionId,
        expected_revision: Revision,
        idempotency_key: &IdempotencyKey,
        request_hash: Sha256Digest,
    ) -> Result<CompletionReservation, ControlError> {
        let mut transaction = self.pool.begin().await.map_err(db)?;
        match IdempotencyStore::reserve(
            &mut transaction,
            Domain::Control,
            COMPLETE_UPLOAD,
            idempotency_key.as_str(),
            request_hash,
        )
        .await
        .map_err(|_| ControlError::PersistenceFailed)?
        {
            IdempotencyDecision::Replay(value) => {
                transaction.rollback().await.map_err(db)?;
                return serde_json::from_value(value)
                    .map(CompletionReservation::Replay)
                    .map_err(|_| ControlError::PersistenceIdentityMismatch);
            }
            IdempotencyDecision::Conflict => return Err(ControlError::IdempotencyConflict),
            IdempotencyDecision::InProgress => {
                let lease_token = Uuid::now_v7();
                let lease_seconds = i64::try_from(self.config.completion_lease_seconds)
                    .map_err(|_| ControlError::ConfigurationInvalid)?;
                let reclaimed = sqlx::query(
                    "UPDATE control.problem_package_upload_sessions \
                     SET completion_lease_token=$5,completion_lease_expires_at=date_trunc('milliseconds',clock_timestamp())+($6*interval '1 second'),updated_at=now() \
                     WHERE upload_id=$1 AND course_id=$2 AND state='completing' \
                       AND completion_idempotency_key=$3 AND completion_request_sha256=$4 \
                       AND completion_lease_expires_at<=date_trunc('milliseconds',clock_timestamp()) RETURNING upload_id",
                )
                .bind(upload_id.as_uuid())
                .bind(course_id.as_uuid())
                .bind(idempotency_key.as_str())
                .bind(request_hash.to_string())
                .bind(lease_token)
                .bind(lease_seconds)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(db)?;
                if reclaimed.is_none() {
                    return Err(ControlError::OperationInProgress);
                }
                transaction.commit().await.map_err(db)?;
                return Ok(CompletionReservation::Claimed(lease_token));
            }
            IdempotencyDecision::Reserved => {}
        }
        let lease_token = Uuid::now_v7();
        let lease_seconds = i64::try_from(self.config.completion_lease_seconds)
            .map_err(|_| ControlError::ConfigurationInvalid)?;
        let result = sqlx::query(
            "UPDATE control.problem_package_upload_sessions \
             SET state='completing',revision=revision+1,completion_idempotency_key=$4,completion_request_sha256=$5,completion_lease_token=$6,completion_lease_expires_at=date_trunc('milliseconds',clock_timestamp())+($7*interval '1 second'),updated_at=now() \
             WHERE upload_id=$1 AND course_id=$2 AND state='pending' AND expires_at>date_trunc('milliseconds',clock_timestamp()) AND revision=$3 \
             RETURNING upload_id",
        )
        .bind(upload_id.as_uuid())
        .bind(course_id.as_uuid())
        .bind(i64_revision(expected_revision)?)
        .bind(idempotency_key.as_str())
        .bind(request_hash.to_string())
        .bind(lease_token)
        .bind(lease_seconds)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?;
        if result.is_none() {
            return Err(ControlError::UploadStateConflict);
        }
        transaction.commit().await.map_err(db)?;
        Ok(CompletionReservation::Claimed(lease_token))
    }

    async fn renew_completion_lease(
        &self,
        upload_id: UploadSessionId,
        lease_token: Uuid,
    ) -> Result<(), ControlError> {
        let lease_seconds = i64::try_from(self.config.completion_lease_seconds)
            .map_err(|_| ControlError::ConfigurationInvalid)?;
        let updated = sqlx::query(
            "UPDATE control.problem_package_upload_sessions \
             SET completion_lease_expires_at=now()+($3*interval '1 second'),updated_at=now() \
             WHERE upload_id=$1 AND state='completing' AND completion_lease_token=$2 AND completion_lease_expires_at>now()",
        )
        .bind(upload_id.as_uuid())
        .bind(lease_token)
        .bind(lease_seconds)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        if updated.rows_affected() != 1 {
            return Err(ControlError::OperationLeaseLost);
        }
        Ok(())
    }

    async fn record_frozen_version(
        &self,
        upload_id: UploadSessionId,
        lease_token: Uuid,
        object_key: &str,
        reference: &contracts::ArtifactRef,
        now: UtcTimestamp,
    ) -> Result<(), ControlError> {
        let updated = sqlx::query(
            "UPDATE control.problem_package_upload_files files \
             SET object_version=$4,artifact_id=$5,verified_at=$6 \
             WHERE files.upload_id=$1 AND files.object_key=$2 \
               AND (files.object_version IS NULL OR (files.object_version=$4 AND files.artifact_id=$5)) \
               AND EXISTS (SELECT 1 FROM control.problem_package_upload_sessions sessions \
                           WHERE sessions.upload_id=files.upload_id AND sessions.state='completing' \
                             AND sessions.completion_lease_token=$3 AND sessions.completion_lease_expires_at>now())",
        )
        .bind(upload_id.as_uuid())
        .bind(object_key)
        .bind(lease_token)
        .bind(&reference.object_version)
        .bind(reference.artifact_id.as_uuid())
        .bind(now.get())
        .execute(&self.pool)
        .await
        .map_err(db)?;
        if updated.rows_affected() != 1 {
            return Err(ControlError::OperationLeaseLost);
        }
        Ok(())
    }

    async fn commit_completed_package(
        &self,
        upload_id: UploadSessionId,
        lease_token: Uuid,
        idempotency_key: &IdempotencyKey,
        package: &ProblemPackage,
        frozen_versions: &[(String, String, contracts::ArtifactId)],
    ) -> Result<(), ControlError> {
        let mut transaction = self.pool.begin().await.map_err(db)?;
        let locked = sqlx::query_scalar::<_, String>(
            "SELECT state FROM control.problem_package_upload_sessions \
             WHERE upload_id=$1 AND completion_lease_token=$2 AND completion_lease_expires_at>now() FOR UPDATE",
        )
        .bind(upload_id.as_uuid())
        .bind(lease_token)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?
        .ok_or(ControlError::UploadNotFound)?;
        if locked != "completing" {
            return Err(ControlError::UploadStateConflict);
        }
        let contract = serde_json::to_value(package).map_err(|_| ControlError::ContractInvalid)?;
        // manifest_sha256 is internal persistence hash (not contract hash) for the completed package
        let manifest_sha256 = canonical_hash(package)?;
        sqlx::query(
            "INSERT INTO control.problem_packages \
             (package_id,course_id,revision,manifest_sha256,contract,completed_at) \
             VALUES ($1,$2,$3,$4,$5,$6)",
        )
        .bind(package.id.as_uuid())
        .bind(package.course_id.as_uuid())
        .bind(i64_revision(package.revision)?)
        .bind(manifest_sha256.to_string())
        .bind(&contract)
        .bind(package.completed_at.get())
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        for (key, version, artifact_id) in frozen_versions {
            let matches = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM control.problem_package_upload_files \
                 WHERE upload_id=$1 AND object_key=$2 AND object_version=$3 AND artifact_id=$4)",
            )
            .bind(upload_id.as_uuid())
            .bind(key)
            .bind(version)
            .bind(artifact_id.as_uuid())
            .fetch_one(&mut *transaction)
            .await
            .map_err(db)?;
            if !matches {
                return Err(ControlError::PersistenceIdentityMismatch);
            }
        }
        let completed = sqlx::query(
            "UPDATE control.problem_package_upload_sessions \
             SET state='completed',completed_package_id=$2,revision=revision+1,completion_lease_token=NULL,completion_lease_expires_at=NULL,updated_at=now() \
             WHERE upload_id=$1 AND state='completing' AND completion_lease_token=$3",
        )
        .bind(upload_id.as_uuid())
        .bind(package.id.as_uuid())
        .bind(lease_token)
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        if completed.rows_affected() != 1 {
            return Err(ControlError::OperationLeaseLost);
        }
        append_sse(
            &mut transaction,
            package.course_id,
            "problem_package.completed.v1",
            package.id.as_uuid(),
            package.revision,
            json!({"packageId":package.id,"revision":package.revision,"manifestSha256":manifest_sha256}),
        )
        .await?;
        IdempotencyStore::complete(
            &mut transaction,
            Domain::Control,
            COMPLETE_UPLOAD,
            idempotency_key.as_str(),
            &contract,
        )
        .await
        .map_err(|_| ControlError::PersistenceFailed)?;
        transaction.commit().await.map_err(db)?;
        Ok(())
    }

    async fn fail_upload<T>(
        &self,
        upload_id: UploadSessionId,
        lease_token: Uuid,
        idempotency_key: &IdempotencyKey,
        diagnostic: &str,
        cleanup_versions: &[(String, String, contracts::ArtifactId)],
    ) -> Result<T, ControlError> {
        let mut transaction = self.pool.begin().await.map_err(db)?;
        let failed = sqlx::query(
            "UPDATE control.problem_package_upload_sessions \
             SET state='failed',terminal_diagnostic=$2,revision=revision+1,completion_lease_token=NULL,completion_lease_expires_at=NULL,updated_at=now() \
             WHERE upload_id=$1 AND state='completing' AND completion_lease_token=$3 AND completion_lease_expires_at>now()",
        )
        .bind(upload_id.as_uuid())
        .bind(diagnostic)
        .bind(lease_token)
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        if failed.rows_affected() != 1 {
            return Err(ControlError::OperationLeaseLost);
        }
        for (key, version, _) in cleanup_versions {
            sqlx::query(
                "INSERT INTO control.object_cleanup_ledger (object_key,object_version,upload_id) \
                 VALUES ($1,$2,$3) ON CONFLICT DO NOTHING",
            )
            .bind(key)
            .bind(version)
            .bind(upload_id.as_uuid())
            .execute(&mut *transaction)
            .await
            .map_err(db)?;
        }
        sqlx::query(
            "DELETE FROM control.idempotency_ledger \
             WHERE operation=$1 AND idempotency_key=$2 AND state='in_progress'",
        )
        .bind(COMPLETE_UPLOAD)
        .bind(idempotency_key.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        transaction.commit().await.map_err(db)?;
        Err(ControlError::PackageVerificationFailed(
            diagnostic.to_owned(),
        ))
    }
}

/// Sanitized persisted SSE record.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SseRecord {
    /// Course-local monotonically increasing cursor.
    pub sequence: u64,
    /// Stable event contract name.
    pub event_type: String,
    /// Safe aggregate identity.
    pub aggregate_id: Uuid,
    /// Exact aggregate revision represented by this fact.
    pub aggregate_revision: Revision,
    /// Payload already checked for forbidden sensitive fields.
    pub payload: Value,
    /// Durable event creation time.
    pub created_at: UtcTimestamp,
}

/// Result of one bounded orphan cleanup iteration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CleanupOutcome {
    /// No due object version exists.
    Idle,
    /// One exact object version was deleted and durably marked complete.
    Deleted,
    /// A sanitized failure scheduled an explicit bounded retry.
    RetryScheduled {
        /// One-based attempt number now persisted.
        attempt: u32,
    },
}

fn validate_upload_request(
    request: &CreateProblemPackageUploadRequest,
    config: &ControlConfig,
) -> Result<(), ControlError> {
    if request.files.is_empty() || request.files.len() > config.max_package_files {
        return Err(ControlError::PackageManifestInvalid);
    }
    let mut paths = BTreeSet::new();
    let mut total = 0_u64;
    for file in &request.files {
        contracts::validate_relative_path(&file.path)
            .map_err(|_| ControlError::PackageManifestInvalid)?;
        if !paths.insert(&file.path) || file.size_bytes == 0 || file.media_type.trim().is_empty() {
            return Err(ControlError::PackageManifestInvalid);
        }
        total = total
            .checked_add(file.size_bytes)
            .ok_or(ControlError::PackageManifestInvalid)?;
    }
    if total > config.max_package_bytes {
        return Err(ControlError::PackageTooLarge);
    }
    Ok(())
}

async fn advisory_course_lock(
    transaction: &mut Transaction<'_, Postgres>,
    course_id: CourseId,
) -> Result<(), ControlError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(course_id.to_string())
        .execute(&mut **transaction)
        .await
        .map_err(db)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn insert_candidate(
    transaction: &mut Transaction<'_, Postgres>,
    course_id: CourseId,
    run_id: contracts::AgentRunId,
    kind: &str,
    candidate_id: CandidateId,
    revision: Revision,
    content_sha256: Sha256Digest,
    policy_revision: Revision,
    schema_sha256: Sha256Digest,
    event_id: EventId,
    contract: Value,
) -> Result<(), ControlError> {
    let inserted = sqlx::query(
        "INSERT INTO control.candidates \
         (candidate_id,candidate_kind,course_id,revision,state,content_sha256,contract,run_id, \
          policy_revision,schema_sha256,projected_event_id) \
         VALUES ($1,$2,$3,$4,'validated',$5,$6,$7,$8,$9,$10) \
         ON CONFLICT (candidate_id) DO NOTHING",
    )
    .bind(candidate_id.as_uuid())
    .bind(kind)
    .bind(course_id.as_uuid())
    .bind(i64_revision(revision)?)
    .bind(content_sha256.to_string())
    .bind(&contract)
    .bind(run_id.as_uuid())
    .bind(i64_revision(policy_revision)?)
    .bind(schema_sha256.to_string())
    .bind(event_id.as_uuid())
    .execute(&mut **transaction)
    .await
    .map_err(db)?;
    if inserted.rows_affected() == 0 {
        let existing = sqlx::query(
            "SELECT candidate_kind,course_id,revision,content_sha256,contract,run_id, \
             policy_revision,schema_sha256,projected_event_id \
             FROM control.candidates WHERE candidate_id=$1",
        )
        .bind(candidate_id.as_uuid())
        .fetch_one(&mut **transaction)
        .await
        .map_err(db)?;
        let content_matches = existing
            .try_get::<String, _>("candidate_kind")
            .map_err(db)?
            == kind
            && existing.try_get::<Uuid, _>("course_id").map_err(db)? == course_id.as_uuid()
            && existing.try_get::<i64, _>("revision").map_err(db)? == i64_revision(revision)?
            && existing
                .try_get::<String, _>("content_sha256")
                .map_err(db)?
                == content_sha256.to_string()
            && existing.try_get::<Value, _>("contract").map_err(db)? == contract
            && existing.try_get::<Uuid, _>("run_id").map_err(db)? == run_id.as_uuid()
            && existing.try_get::<i64, _>("policy_revision").map_err(db)?
                == i64_revision(policy_revision)?
            && existing.try_get::<String, _>("schema_sha256").map_err(db)?
                == schema_sha256.to_string();
        if !content_matches {
            return Err(ControlError::ProjectionConflict);
        }
        let prior_event: Uuid = existing.try_get("projected_event_id").map_err(db)?;
        // A retried run publishes a fresh completion event for the same
        // immutable candidate; refresh the projection source instead of
        // treating the differing event identity as a conflict.
        if prior_event != event_id.as_uuid() {
            sqlx::query(
                "UPDATE control.candidates SET projected_event_id=$2 WHERE candidate_id=$1",
            )
            .bind(candidate_id.as_uuid())
            .bind(event_id.as_uuid())
            .execute(&mut **transaction)
            .await
            .map_err(db)?;
        }
        return Ok(());
    }
    append_sse(
        transaction,
        course_id,
        &format!("{kind}_candidate.validated.v1"),
        candidate_id.as_uuid(),
        revision,
        json!({"candidateId":candidate_id,"revision":revision,"contentSha256":content_sha256}),
    )
    .await
}

async fn append_sse(
    transaction: &mut Transaction<'_, Postgres>,
    course_id: CourseId,
    event_type: &str,
    aggregate_id: Uuid,
    aggregate_revision: Revision,
    payload: Value,
) -> Result<(), ControlError> {
    reject_sensitive_payload(&payload)?;
    let payload_hash = canonical_hash(&payload)?;
    let sequence = sqlx::query_scalar::<_, i64>(
        "INSERT INTO control.sse_course_cursors (course_id,last_sequence) VALUES ($1,1) \
         ON CONFLICT (course_id) DO UPDATE \
         SET last_sequence=control.sse_course_cursors.last_sequence+1 \
         RETURNING last_sequence",
    )
    .bind(course_id.as_uuid())
    .fetch_one(&mut **transaction)
    .await
    .map_err(db)?;
    sqlx::query(
        "INSERT INTO control.sse_events \
         (course_id,sequence,event_type,aggregate_id,aggregate_revision,payload,payload_sha256) \
         VALUES ($1,$2,$3,$4,$5,$6,$7)",
    )
    .bind(course_id.as_uuid())
    .bind(sequence)
    .bind(event_type)
    .bind(aggregate_id)
    .bind(i64_revision(aggregate_revision)?)
    .bind(payload)
    .bind(payload_hash.to_string())
    .execute(&mut **transaction)
    .await
    .map_err(db)?;
    Ok(())
}

fn reject_sensitive_payload(value: &Value) -> Result<(), ControlError> {
    const FORBIDDEN: &[&str] = &[
        "authorization",
        "cookie",
        "secret",
        "token",
        "privatekey",
        "uploadurl",
        "requiredheaders",
        "content",
    ];
    match value {
        Value::Object(map) => {
            for (key, nested) in map {
                let normalized = key
                    .chars()
                    .filter(char::is_ascii_alphanumeric)
                    .flat_map(char::to_lowercase)
                    .collect::<String>();
                if FORBIDDEN.contains(&normalized.as_str()) {
                    return Err(ControlError::SensitiveEventPayload);
                }
                reject_sensitive_payload(nested)?;
            }
        }
        Value::Array(values) => {
            for nested in values {
                reject_sensitive_payload(nested)?;
            }
        }
        _ => {}
    }
    Ok(())
}

async fn load_contract<T>(pool: &PgPool, query: &str, id: Uuid) -> Result<T, ControlError>
where
    T: serde::de::DeserializeOwned,
{
    let row = sqlx::query(query)
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(db)?
        .ok_or(ControlError::NotFound)?;
    serde_json::from_value(row.try_get("contract").map_err(db)?)
        .map_err(|_| ControlError::PersistenceIdentityMismatch)
}

async fn load_contract_two<T>(
    pool: &PgPool,
    query: &str,
    first: Uuid,
    second: Uuid,
) -> Result<T, ControlError>
where
    T: serde::de::DeserializeOwned,
{
    let row = sqlx::query(query)
        .bind(first)
        .bind(second)
        .fetch_optional(pool)
        .await
        .map_err(db)?
        .ok_or(ControlError::NotFound)?;
    serde_json::from_value(row.try_get("contract").map_err(db)?)
        .map_err(|_| ControlError::PersistenceIdentityMismatch)
}

async fn load_candidate_contract<T>(
    pool: &PgPool,
    course_id: CourseId,
    candidate_id: CandidateId,
    kind: &str,
) -> Result<T, ControlError>
where
    T: serde::de::DeserializeOwned,
{
    let row = sqlx::query(
        "SELECT contract FROM control.candidates \
         WHERE candidate_id=$1 AND course_id=$2 AND candidate_kind=$3 AND state='validated'",
    )
    .bind(candidate_id.as_uuid())
    .bind(course_id.as_uuid())
    .bind(kind)
    .fetch_optional(pool)
    .await
    .map_err(db)?
    .ok_or(ControlError::CandidateNotFound)?;
    serde_json::from_value(row.try_get("contract").map_err(db)?)
        .map_err(|_| ControlError::PersistenceIdentityMismatch)
}

async fn load_candidate_approvals(
    pool: &PgPool,
    candidate_id: CandidateId,
) -> Result<Vec<CandidateApproval>, ControlError> {
    let rows = sqlx::query(
        "SELECT contract FROM control.candidate_approvals \
         WHERE candidate_id=$1 ORDER BY decided_at,approval_id",
    )
    .bind(candidate_id.as_uuid())
    .fetch_all(pool)
    .await
    .map_err(db)?;
    rows.into_iter()
        .map(|row| {
            serde_json::from_value(row.try_get("contract").map_err(db)?)
                .map_err(|_| ControlError::PersistenceIdentityMismatch)
        })
        .collect()
}

async fn load_candidate_build(
    pool: &PgPool,
    course_id: CourseId,
    candidate_id: CandidateId,
) -> Result<Option<CandidateBuildView>, ControlError> {
    let row = sqlx::query(
        "SELECT builds.state,builds.terminal_diagnostic,builds.cleanup_verified, \
                artifacts.artifact \
         FROM control.container_build_projections builds \
         LEFT JOIN control.image_artifact_projections artifacts \
           ON artifacts.image_artifact_id=builds.image_artifact_id \
         WHERE builds.course_id=$1 AND builds.candidate_id=$2",
    )
    .bind(course_id.as_uuid())
    .bind(candidate_id.as_uuid())
    .fetch_optional(pool)
    .await
    .map_err(db)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let state = match row.try_get::<String, _>("state").map_err(db)?.as_str() {
        "requested" => CandidateBuildState::Requested,
        "succeeded" => CandidateBuildState::Succeeded,
        "failed" => CandidateBuildState::Failed,
        "cancelled" => CandidateBuildState::Cancelled,
        _ => return Err(ControlError::PersistenceIdentityMismatch),
    };
    let artifact = row
        .try_get::<Option<Value>, _>("artifact")
        .map_err(db)?
        .map(serde_json::from_value)
        .transpose()
        .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
    let diagnostic_code = row
        .try_get::<Option<String>, _>("terminal_diagnostic")
        .map_err(db)?
        .map(DiagnosticCode::parse)
        .transpose()
        .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
    let cleanup_verified: Option<bool> = row.try_get("cleanup_verified").map_err(db)?;
    let evidence_is_complete = artifact.is_some();
    if (state == CandidateBuildState::Succeeded) != evidence_is_complete
        || (state == CandidateBuildState::Requested
            && (diagnostic_code.is_some() || cleanup_verified.is_some()))
        || (matches!(
            state,
            CandidateBuildState::Failed | CandidateBuildState::Cancelled
        ) && diagnostic_code.is_none())
    {
        return Err(ControlError::PersistenceIdentityMismatch);
    }
    Ok(Some(CandidateBuildView {
        state,
        artifact,
        diagnostic_code,
        cleanup_verified,
    }))
}

async fn load_contract_tx<T>(
    transaction: &mut Transaction<'_, Postgres>,
    query: &str,
    first: Uuid,
    second: Uuid,
) -> Result<T, ControlError>
where
    T: serde::de::DeserializeOwned,
{
    let row = sqlx::query(query)
        .bind(first)
        .bind(second)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(db)?
        .ok_or(ControlError::NotFound)?;
    serde_json::from_value(row.try_get("contract").map_err(db)?)
        .map_err(|_| ControlError::PersistenceIdentityMismatch)
}

fn sse_record(row: &sqlx::postgres::PgRow) -> Result<SseRecord, ControlError> {
    let created_at = UtcTimestamp::from_utc(row.try_get("created_at").map_err(db)?)
        .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
    Ok(SseRecord {
        sequence: u64::try_from(row.try_get::<i64, _>("sequence").map_err(db)?)
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?,
        event_type: row.try_get("event_type").map_err(db)?,
        aggregate_id: row.try_get("aggregate_id").map_err(db)?,
        aggregate_revision: revision_from_i64(row.try_get("aggregate_revision").map_err(db)?)?,
        payload: row.try_get("payload").map_err(db)?,
        created_at,
    })
}

fn release_view(
    row: &sqlx::postgres::PgRow,
) -> Result<EnvironmentTemplateReleaseView, ControlError> {
    let release = serde_json::from_value(row.try_get("release_contract").map_err(db)?)
        .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
    let withdrawal = row
        .try_get::<Option<Value>, _>("withdrawal_contract")
        .map_err(db)?
        .map(serde_json::from_value)
        .transpose()
        .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
    Ok(EnvironmentTemplateReleaseView {
        release,
        withdrawal,
    })
}

fn validate_trace_id(trace_id: &str) -> Result<(), ControlError> {
    if trace_id.trim().is_empty() || trace_id.len() > 256 || trace_id.chars().any(char::is_control)
    {
        return Err(ControlError::ContractInvalid);
    }
    Ok(())
}

fn valid_reason_code(reason_code: &str) -> bool {
    let mut bytes = reason_code.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    first.is_ascii_uppercase()
        && reason_code.len() <= 64
        && bytes.all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

fn canonical_hash<T: Serialize>(value: &T) -> Result<Sha256Digest, ControlError> {
    Sha256Digest::of_canonical(value).map_err(|_| ControlError::ContractInvalid)
}

fn add_seconds(now: UtcTimestamp, seconds: u64) -> Result<UtcTimestamp, ControlError> {
    let duration =
        Duration::seconds(i64::try_from(seconds).map_err(|_| ControlError::ConfigurationInvalid)?);
    UtcTimestamp::from_utc(now.get() + duration).map_err(|_| ControlError::ContractInvalid)
}

fn revision_from_i64(value: i64) -> Result<Revision, ControlError> {
    Revision::new(u64::try_from(value).map_err(|_| ControlError::PersistenceIdentityMismatch)?)
        .map_err(|_| ControlError::PersistenceIdentityMismatch)
}

fn artifact_id_from_uuid(value: Uuid) -> Result<contracts::ArtifactId, ControlError> {
    contracts::ArtifactId::from_str(&value.to_string())
        .map_err(|_| ControlError::PersistenceIdentityMismatch)
}

fn i64_revision(value: Revision) -> Result<i64, ControlError> {
    i64::try_from(value.get()).map_err(|_| ControlError::ContractInvalid)
}

fn decision_name(decision: CandidateDecision) -> &'static str {
    match decision {
        CandidateDecision::Approved => "approved",
        CandidateDecision::Rejected => "rejected",
        CandidateDecision::Withdrawn => "withdrawn",
    }
}

fn agent_run_state_name(state: contracts::authoring::AgentRunState) -> &'static str {
    use contracts::authoring::AgentRunState;
    match state {
        AgentRunState::Requested => "requested",
        AgentRunState::Running => "running",
        AgentRunState::Cancelling => "cancelling",
        AgentRunState::PartiallySucceeded => "partially_succeeded",
        AgentRunState::Succeeded => "succeeded",
        AgentRunState::Failed => "failed",
        AgentRunState::Cancelled => "cancelled",
    }
}

fn event_matches_run(
    event: &CloudEvent<AgentRunEvent>,
    run: &contracts::authoring::AgentRun,
    environment: Option<&EnvironmentCandidate>,
    evaluation: Option<&EvaluationCandidate>,
) -> bool {
    use contracts::authoring::AgentRunState;

    match event.subject.as_str() {
        contracts::events::subjects::AGENT_RUN_REQUESTED => {
            run.state == AgentRunState::Requested
                && event.data.state == "requested"
                && event.data.diagnostic_code.is_none()
                && environment.is_none()
                && evaluation.is_none()
        }
        contracts::events::subjects::AGENT_RUN_COMPLETED => {
            matches!(
                run.state,
                AgentRunState::Succeeded | AgentRunState::PartiallySucceeded
            ) && event.data.state == agent_run_state_name(run.state)
                && event.data.diagnostic_code.is_none()
                && environment.is_some()
                    == run.tracks.iter().any(|track| {
                        track.kind == contracts::authoring::AgentTrackKind::Environment
                            && track.candidate_id.is_some()
                    })
                && evaluation.is_some()
                    == run.tracks.iter().any(|track| {
                        track.kind == contracts::authoring::AgentTrackKind::Evaluation
                            && track.candidate_id.is_some()
                    })
        }
        contracts::events::subjects::AGENT_RUN_FAILED => {
            matches!(run.state, AgentRunState::Failed | AgentRunState::Cancelled)
                && event.data.state == agent_run_state_name(run.state)
                && event
                    .data
                    .diagnostic_code
                    .as_deref()
                    .is_some_and(|diagnostic| contracts::DiagnosticCode::parse(diagnostic).is_ok())
                && environment.is_none()
                && evaluation.is_none()
        }
        _ => false,
    }
}

fn image_artifact_id(artifact: &ImageArtifact) -> ImageArtifactId {
    match artifact {
        ImageArtifact::Container { id, .. } | ImageArtifact::VirtualMachine { id, .. } => *id,
    }
}

fn image_build_request_id(artifact: &ImageArtifact) -> Option<BuildRequestId> {
    match artifact {
        ImageArtifact::Container {
            build_request_id, ..
        } => Some(*build_request_id),
        ImageArtifact::VirtualMachine { .. } => None,
    }
}

fn is_unique_violation(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .is_some_and(|code| code == "23505")
}

fn db(_: sqlx::Error) -> ControlError {
    ControlError::PersistenceFailed
}

/// Stable fail-fast Control diagnostics.
#[derive(Debug, Error)]
#[allow(missing_docs)]
pub enum ControlError {
    #[error("LW_CONTROL_CONFIG_INVALID")]
    ConfigurationInvalid,
    #[error("LW_CONTRACT_DOCUMENT_INVALID")]
    ContractInvalid,
    #[error("LW_IDEMPOTENCY_CONFLICT")]
    IdempotencyConflict,
    #[error("LW_OPERATION_IN_PROGRESS")]
    OperationInProgress,
    #[error("LW_OPERATION_LEASE_LOST")]
    OperationLeaseLost,
    #[error("LW_CONTROL_PERSISTENCE_FAILED")]
    PersistenceFailed,
    #[error("LW_CONTROL_PERSISTED_IDENTITY_MISMATCH")]
    PersistenceIdentityMismatch,
    #[error("LW_PACKAGE_MANIFEST_INVALID")]
    PackageManifestInvalid,
    #[error("LW_PACKAGE_TOO_LARGE")]
    PackageTooLarge,
    #[error("LW_UPLOAD_NOT_FOUND")]
    UploadNotFound,
    #[error("LW_UPLOAD_STATE_CONFLICT")]
    UploadStateConflict,
    #[error("{0}")]
    PackageVerificationFailed(String),
    #[error("LW_PACKAGE_OBJECT_VERIFICATION_FAILED: {0}")]
    ObjectVerificationFailed(String),
    #[error("LW_OBJECT_STORE_IDENTITY_MISMATCH")]
    ObjectStoreIdentityMismatch,
    #[error("LW_COURSE_SCOPE_MISMATCH")]
    CourseMismatch,
    #[error("LW_LLM_POLICY_INVALID")]
    PolicyInvalid,
    #[error("LW_LLM_POLICY_NOT_FOUND")]
    PolicyNotFound,
    #[error("LW_CANDIDATE_NOT_FOUND")]
    CandidateNotFound,
    #[error("LW_CANDIDATE_MISSING")]
    CandidateMissing,
    #[error("LW_AGENT_PROJECTION_CONFLICT")]
    ProjectionConflict,
    #[error("LW_REVISION_CONFLICT")]
    RevisionConflict,
    #[error("LW_CANDIDATE_DECISION_CONFLICT")]
    DecisionConflict,
    #[error("LW_CANDIDATE_KIND_MISMATCH")]
    CandidateKindMismatch,
    #[error("LW_RELEASE_CANDIDATE_MISMATCH")]
    ReleaseCandidateMismatch,
    #[error("LW_RELEASE_EVIDENCE_INVALID")]
    ReleaseEvidenceInvalid,
    #[error("LW_RELEASE_EVIDENCE_STALE")]
    ReleaseEvidenceStale,
    #[error("LW_RELEASE_ARTIFACT_NOT_AUTHORITATIVE")]
    ArtifactNotAuthoritative,
    #[error("LW_RELEASE_ARTIFACT_MISMATCH")]
    ArtifactMismatch,
    #[error("LW_RELEASE_NOT_FOUND")]
    ReleaseNotFound,
    #[error("LW_SSE_CURSOR_EXPIRED")]
    SseCursorExpired,
    #[error("LW_SSE_CURSOR_GAP")]
    SseCursorGap,
    #[error("LW_EVENT_SEQUENCE_GAP")]
    EventSequenceGap,
    #[error("LW_SENSITIVE_EVENT_PAYLOAD")]
    SensitiveEventPayload,
    #[error("LW_OBJECT_CLEANUP_FENCE_LOST")]
    CleanupFenceLost,
    #[error("LW_OBJECT_CLEANUP_ATTEMPTS_EXHAUSTED")]
    CleanupAttemptsExhausted,
    #[error("LW_CONTROL_NOT_FOUND")]
    NotFound,
    #[error(transparent)]
    ObjectStore(#[from] ObjectStoreError),
}

#[cfg(test)]
mod tests {
    use contracts::http::{CreateProblemPackageUploadRequest, ProblemPackageUploadFile};
    use contracts::{PolicyId, Revision};
    use persistence_sqlx::Sha256Digest;

    use contracts::supply_chain::BuildNetworkPolicy;

    use super::{
        ContainerBuildPolicy, ControlConfig, ControlError, EvaluationRuntimePolicy,
        VirtualMachineBasePolicy, reject_sensitive_payload, validate_upload_request,
    };

    fn config() -> Result<ControlConfig, Box<dyn std::error::Error>> {
        Ok(ControlConfig {
            package_object_prefix: "problem-packages".to_owned(),
            upload_ttl_seconds: 900,
            completion_lease_seconds: 300,
            max_package_files: 2,
            max_package_bytes: 128,
            retention_policy_id: PolicyId::new(),
            retention_seconds: 86_400,
            sse_retention_seconds: 3_600,
            trust_revision: Revision::new(1)?,
            image_policy_id: PolicyId::new(),
            image_policy_revision: Revision::new(1)?,
            environment_schema_sha256: Sha256Digest::of_bytes(b"environment-schema"),
            evaluation_schema_sha256: Sha256Digest::of_bytes(b"evaluation-schema"),
            container_build: ContainerBuildPolicy {
                builder_binding: "buildkit-primary-v1".to_owned(),
                output_repository_prefix: "harbor.internal/labweaver-system".to_owned(),
                dockerfile_path: "Dockerfile".to_owned(),
                network: BuildNetworkPolicy::DenyAll,
                max_duration_milliseconds: 600_000,
                max_cpu_millicores: 2_000,
                max_memory_bytes: 2_147_483_648,
            },
            virtual_machine_base: VirtualMachineBasePolicy {
                provider_binding: "kubevirt-primary-v1".to_owned(),
                storage_class_binding: "vm-rwo-primary-v1".to_owned(),
                artifact_id: contracts::ImageArtifactId::new(),
                base_disk: contracts::supply_chain::VirtualMachineBaseDisk {
                    binding: "ubuntu-24.04-v1".to_owned(),
                    source_registry_digest: concat!(
                        "docker://quay.io/containerdisks/ubuntu@",
                        "sha256:d28194a16351320fa9a093e18233033508a745566eb8ba3b309c32924bf155a5"
                    )
                    .to_owned(),
                    capacity_bytes: 10_737_418_240,
                },
                format: contracts::supply_chain::VirtualMachineDiskFormat::Qcow2,
            },
            evaluation_runtime: EvaluationRuntimePolicy {
                source_sha256: Sha256Digest::of_bytes(b"source"),
                provider_binding: "evaluation-primary-v1".to_owned(),
                configuration_sha256: Sha256Digest::of_bytes(b"configuration"),
                migration_catalog_sha256: Sha256Digest::of_bytes(b"migrations"),
                runner_image: format!("runner@sha256:{}", "a".repeat(64)),
                runtime_artifact_sha256: Sha256Digest::of_bytes(b"runtime"),
            },
        })
    }

    #[test]
    fn upload_manifest_rejects_duplicates_escape_and_aggregate_overflow()
    -> Result<(), Box<dyn std::error::Error>> {
        let file = ProblemPackageUploadFile {
            path: "statement.md".to_owned(),
            size_bytes: 64,
            media_type: "text/markdown".to_owned(),
        };
        let request = CreateProblemPackageUploadRequest {
            files: vec![file.clone()],
            retention_policy_revision: Revision::new(1)?,
        };
        validate_upload_request(&request, &config()?)?;
        let duplicate = CreateProblemPackageUploadRequest {
            files: vec![file.clone(), file],
            retention_policy_revision: Revision::new(1)?,
        };
        assert!(matches!(
            validate_upload_request(&duplicate, &config()?),
            Err(ControlError::PackageManifestInvalid)
        ));
        Ok(())
    }

    #[test]
    fn sse_payload_rejects_nested_secrets() {
        let payload = serde_json::json!({"safe":[{"private_key":"redacted"}]});
        assert!(matches!(
            reject_sensitive_payload(&payload),
            Err(ControlError::SensitiveEventPayload)
        ));
    }
}
