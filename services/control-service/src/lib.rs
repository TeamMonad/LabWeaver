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
    AgentRun, AgentRunPurpose, AgentRunState, AgentTrackKind, AuthoringApproval,
    AuthoringApprovalPublicationStatus, AuthoringPublicationState, CandidateApproval,
    CandidateDecision, EnvironmentCandidate, EnvironmentClass, EvaluationCandidate, PackageFile,
    ProblemPackage, ProjectLlmEgressPolicy, RuntimeKind,
};
use contracts::evaluation::{EvaluationExecutionBinding, EvaluationRuntimeIdentity};
use contracts::events::{
    AgentBuildFailed, AgentBuildRequested, AgentRunEvent, AuthoringApprovalCompleted, CloudEvent,
    EVENT_CONTRACTS, ReleasePublished, ReleaseWithdrawn, SPEC_VERSION, subjects,
};
use contracts::http::{
    AddProjectMembershipRequest, AgentWorkExecutionIntentMetadata, ApproveWorkConfigurationRequest,
    AuthoringPublicationAdmissionBinding, AuthoringPublicationAdmissionQuery, CandidateBuildState,
    CandidateBuildView, CandidateDecisionRequest, CompleteAuthoringApprovalRequest,
    CreateEnvironmentTemplateReleaseRequest, CreateEvaluationReleaseRequest,
    CreateProblemPackageUploadRequest, EnvironmentCandidateView, EvaluationCandidateView,
    GeneratedArtifactRecord, IdempotencyKey, InternalPublishEvaluationReleaseRequest,
    ProblemPackageUploadFile, ProblemPackageUploadSession, ProblemPackageUploadTarget,
    RemoveProjectMembershipRequest, WorkConfigurationAdmissionBinding,
    WorkConfigurationAdmissionQuery, WorkConfigurationRecoveryIdentity,
};
use contracts::supply_chain::{
    BuildNetworkPolicy, BuildRequest, EnvironmentTemplateRelease, EnvironmentTemplateReleaseView,
    ImageArtifact, ReleaseWithdrawal, VirtualMachineBaseDisk, VirtualMachineDiskFormat,
};
use contracts::{
    ActorId, ApprovalId, BuildRequestId, CandidateId, CourseId, DiagnosticCode, EventId,
    ImageArtifactId, MembershipState, PlatformRole, PolicyId, ProblemPackageId, Project, ProjectId,
    ProjectMembership, ProjectState, ReleaseId, RetentionClass, RetentionDisposition,
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
const CREATE_WORK_RELEASE: &str = "control_create_work_environment_template_release_v1";
const WITHDRAW_PROJECT_RELEASE: &str = "control_withdraw_project_environment_template_release_v1";
const CREATE_PROJECT: &str = "control_create_project_v1";
const UPDATE_PROJECT: &str = "control_update_project_v1";
const ARCHIVE_PROJECT: &str = "control_archive_project_v1";
const ADD_PROJECT_MEMBERSHIP: &str = "control_add_project_membership_v1";
const REMOVE_PROJECT_MEMBERSHIP: &str = "control_remove_project_membership_v1";
const COMPLETE_AUTHORING_APPROVAL: &str = "control_complete_authoring_approval_v1";
const APPROVE_WORK_CONFIGURATION: &str = "control_approve_work_configuration_v1";
const BUILD_REQUEST_SUBJECT: &str = subjects::AGENT_BUILD_REQUESTED;
const RELEASE_SUBJECT: &str = subjects::ENVIRONMENT_TEMPLATE_RELEASE_PUBLISHED;
const WITHDRAWAL_SUBJECT: &str = subjects::ENVIRONMENT_TEMPLATE_RELEASE_WITHDRAWN;
const AUTHORING_APPROVAL_SUBJECT: &str = subjects::AUTHORING_APPROVAL_COMPLETED;

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
    /// Exact registered Evaluation provider.
    pub provider_binding: String,
    /// Digest-pinned Evaluation runner image.
    pub runner_image: String,
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

/// Result of claiming one authoring publication trigger.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuthoringPublicationClaim {
    /// The durable projection was moved to `publishing` and downstream work may run.
    Claimed(AuthoringApprovalPublicationStatus),
    /// The projection is already complete; the trigger can be acknowledged.
    AlreadyReady(AuthoringApprovalPublicationStatus),
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

    /// Creates a project and its initial Access-owned owner membership in one
    /// transaction. A course association is accepted only for an active
    /// membership with the owner's asserted platform role.
    pub async fn create_project(
        &self,
        owner_actor_id: ActorId,
        owner_role: PlatformRole,
        request: &contracts::CreateProjectRequest,
        idempotency_key: &IdempotencyKey,
        now: UtcTimestamp,
    ) -> Result<Project, ControlError> {
        validate_project_fields(&request.name, request.description.as_deref())?;
        let request_hash = canonical_hash(&json!({
            "ownerActorId": owner_actor_id,
            "ownerRole": owner_role,
            "request": request,
        }))?;
        let project = Project {
            id: ProjectId::new(),
            owner_actor_id,
            name: request.name.trim().to_owned(),
            description: request
                .description
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
            course_id: request.course_id,
            state: ProjectState::Active,
            revision: Revision::new(1).map_err(|_| ControlError::ContractInvalid)?,
            created_at: now,
            updated_at: now,
        };
        project
            .validate()
            .map_err(|_| ControlError::ContractInvalid)?;
        let contract = serde_json::to_value(&project).map_err(|_| ControlError::ContractInvalid)?;
        let mut transaction = self.pool.begin().await.map_err(db)?;
        match IdempotencyStore::reserve(
            &mut transaction,
            Domain::Control,
            CREATE_PROJECT,
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
        if let Some(course_id) = project.course_id {
            auth::require_course_membership(
                &mut transaction,
                course_id,
                owner_actor_id,
                owner_role,
                now.get(),
            )
            .await
            .map_err(|_| ControlError::CourseMembershipRequired)?;
        }
        sqlx::query(
            "INSERT INTO control.projects \
             (project_id,owner_actor_id,name,description,course_id,state,revision,created_at,updated_at,contract) \
             VALUES ($1,$2,$3,$4,$5,'active',$6,$7,$8,$9)",
        )
        .bind(project.id.as_uuid())
        .bind(owner_actor_id.as_uuid())
        .bind(&project.name)
        .bind(&project.description)
        .bind(project.course_id.map(CourseId::as_uuid))
        .bind(i64_revision(project.revision)?)
        .bind(project.created_at.get())
        .bind(project.updated_at.get())
        .bind(&contract)
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        auth::insert_project_owner_membership(
            &mut transaction,
            project.id,
            owner_actor_id,
            project.course_id,
            owner_role,
        )
        .await
        .map_err(|_| ControlError::PersistenceFailed)?;
        IdempotencyStore::complete(
            &mut transaction,
            Domain::Control,
            CREATE_PROJECT,
            idempotency_key.as_str(),
            &contract,
        )
        .await
        .map_err(|_| ControlError::PersistenceFailed)?;
        transaction.commit().await.map_err(db)?;
        Ok(project)
    }

    /// Lists active and archived projects visible through Access memberships.
    pub async fn list_projects(
        &self,
        actor_id: ActorId,
        platform_admin: bool,
    ) -> Result<Vec<Project>, ControlError> {
        let rows = sqlx::query(
            "SELECT p.contract FROM control.projects p \
             WHERE $2 OR p.owner_actor_id=$1 OR EXISTS( \
                 SELECT 1 FROM access.project_memberships m \
                  WHERE m.project_id=p.project_id AND m.actor_id=$1 AND m.state='active' \
                    AND (m.expires_at IS NULL OR m.expires_at > clock_timestamp())) \
             ORDER BY p.updated_at DESC,p.project_id",
        )
        .bind(actor_id.as_uuid())
        .bind(platform_admin)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        rows.into_iter()
            .map(|row| project_from_value(row.try_get("contract").map_err(db)?))
            .collect()
    }

    /// Reads one project. Scope authorization is performed by Access at the
    /// HTTP boundary; this method still fences the identity in persisted data.
    pub async fn project(&self, project_id: ProjectId) -> Result<Project, ControlError> {
        let row = sqlx::query("SELECT contract FROM control.projects WHERE project_id=$1")
            .bind(project_id.as_uuid())
            .fetch_optional(&self.pool)
            .await
            .map_err(db)?
            .ok_or(ControlError::ProjectNotFound)?;
        project_from_value(row.try_get("contract").map_err(db)?)
    }

    /// Updates project metadata under an owner-or-platform-admin governance check.
    pub async fn update_project(
        &self,
        project_id: ProjectId,
        actor_id: ActorId,
        platform_admin: bool,
        request: &contracts::UpdateProjectRequest,
        idempotency_key: &IdempotencyKey,
        now: UtcTimestamp,
    ) -> Result<Project, ControlError> {
        validate_project_fields(&request.name, request.description.as_deref())?;
        let request_hash = canonical_hash(&json!({
            "projectId": project_id,
            "actorId": actor_id,
            "request": request,
        }))?;
        let mut transaction = self.pool.begin().await.map_err(db)?;
        match IdempotencyStore::reserve(
            &mut transaction,
            Domain::Control,
            UPDATE_PROJECT,
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
            "SELECT owner_actor_id,name,description,course_id,state,revision,created_at,updated_at \
             FROM control.projects WHERE project_id=$1 FOR UPDATE",
        )
        .bind(project_id.as_uuid())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?
        .ok_or(ControlError::ProjectNotFound)?;
        let owner: Uuid = row.try_get("owner_actor_id").map_err(db)?;
        if !platform_admin && owner != actor_id.as_uuid() {
            return Err(ControlError::ProjectGovernanceDenied);
        }
        let revision = revision_from_i64(row.try_get("revision").map_err(db)?)?;
        if revision != request.expected_revision {
            return Err(ControlError::RevisionConflict);
        }
        let created_at = UtcTimestamp::from_utc(row.try_get("created_at").map_err(db)?)
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        let updated_at = now;
        let project = Project {
            id: project_id,
            owner_actor_id: ActorId::from_str(&owner.to_string())
                .map_err(|_| ControlError::PersistenceIdentityMismatch)?,
            name: request.name.trim().to_owned(),
            description: request
                .description
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
            course_id: row
                .try_get::<Option<Uuid>, _>("course_id")
                .map_err(db)?
                .map(|value| CourseId::from_str(&value.to_string()))
                .transpose()
                .map_err(|_| ControlError::PersistenceIdentityMismatch)?,
            state: project_state_from_db(&row.try_get::<String, _>("state").map_err(db)?)?,
            revision: Revision::new(
                revision
                    .get()
                    .checked_add(1)
                    .ok_or(ControlError::ContractInvalid)?,
            )
            .map_err(|_| ControlError::ContractInvalid)?,
            created_at,
            updated_at,
        };
        if project.state == ProjectState::Archived {
            return Err(ControlError::ProjectArchived);
        }
        project
            .validate()
            .map_err(|_| ControlError::ContractInvalid)?;
        let contract = serde_json::to_value(&project).map_err(|_| ControlError::ContractInvalid)?;
        sqlx::query(
            "UPDATE control.projects SET name=$2,description=$3,revision=$4,updated_at=$5,contract=$6 \
             WHERE project_id=$1 AND revision=$7",
        )
        .bind(project_id.as_uuid())
        .bind(&project.name)
        .bind(&project.description)
        .bind(i64_revision(project.revision)?)
        .bind(project.updated_at.get())
        .bind(&contract)
        .bind(i64_revision(request.expected_revision)?)
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        IdempotencyStore::complete(
            &mut transaction,
            Domain::Control,
            UPDATE_PROJECT,
            idempotency_key.as_str(),
            &contract,
        )
        .await
        .map_err(|_| ControlError::PersistenceFailed)?;
        transaction.commit().await.map_err(db)?;
        Ok(project)
    }

    /// Archives a project without deleting its immutable history or memberships.
    pub async fn archive_project(
        &self,
        project_id: ProjectId,
        actor_id: ActorId,
        platform_admin: bool,
        expected_revision: Revision,
        idempotency_key: &IdempotencyKey,
        now: UtcTimestamp,
    ) -> Result<Project, ControlError> {
        let request_hash = canonical_hash(&json!({
            "projectId": project_id,
            "actorId": actor_id,
            "expectedRevision": expected_revision,
        }))?;
        let mut transaction = self.pool.begin().await.map_err(db)?;
        match IdempotencyStore::reserve(
            &mut transaction,
            Domain::Control,
            ARCHIVE_PROJECT,
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
            "SELECT contract FROM control.projects WHERE project_id=$1 AND revision=$2 FOR UPDATE",
        )
        .bind(project_id.as_uuid())
        .bind(i64_revision(expected_revision)?)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?
        .ok_or(ControlError::RevisionConflict)?;
        let current = project_from_value(row.try_get("contract").map_err(db)?)?;
        if !platform_admin && current.owner_actor_id != actor_id {
            return Err(ControlError::ProjectGovernanceDenied);
        }
        if current.state == ProjectState::Archived {
            return Err(ControlError::ProjectArchived);
        }
        let project = Project {
            revision: Revision::new(
                expected_revision
                    .get()
                    .checked_add(1)
                    .ok_or(ControlError::ContractInvalid)?,
            )
            .map_err(|_| ControlError::ContractInvalid)?,
            state: ProjectState::Archived,
            updated_at: now,
            ..current
        };
        let contract = serde_json::to_value(&project).map_err(|_| ControlError::ContractInvalid)?;
        sqlx::query(
            "UPDATE control.projects SET state='archived',revision=$2,updated_at=$3,contract=$4 \
             WHERE project_id=$1 AND revision=$5",
        )
        .bind(project_id.as_uuid())
        .bind(i64_revision(project.revision)?)
        .bind(project.updated_at.get())
        .bind(&contract)
        .bind(i64_revision(expected_revision)?)
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        IdempotencyStore::complete(
            &mut transaction,
            Domain::Control,
            ARCHIVE_PROJECT,
            idempotency_key.as_str(),
            &contract,
        )
        .await
        .map_err(|_| ControlError::PersistenceFailed)?;
        transaction.commit().await.map_err(db)?;
        Ok(project)
    }

    /// Reads Access-owned memberships for a project in revision order.
    pub async fn project_memberships(
        &self,
        project_id: ProjectId,
    ) -> Result<Vec<ProjectMembership>, ControlError> {
        let rows = sqlx::query(
            "SELECT course_id,project_id,actor_id,role,state,revision,expires_at \
             FROM access.project_memberships WHERE project_id=$1 ORDER BY actor_id,role,revision DESC",
        )
        .bind(project_id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        rows.iter().map(project_membership_from_row).collect()
    }

    /// Adds one teacher or student membership under owner/admin governance.
    pub async fn add_project_membership(
        &self,
        project_id: ProjectId,
        actor_id: ActorId,
        platform_admin: bool,
        request: &AddProjectMembershipRequest,
        idempotency_key: &IdempotencyKey,
        now: UtcTimestamp,
    ) -> Result<ProjectMembership, ControlError> {
        if request.role == PlatformRole::PlatformAdmin || request.actor_id == actor_id {
            return Err(ControlError::ProjectGovernanceDenied);
        }
        if request
            .expires_at
            .is_some_and(|expires_at| expires_at <= now)
        {
            return Err(ControlError::ContractInvalid);
        }
        let request_hash = canonical_hash(&json!({
            "projectId": project_id,
            "actorId": actor_id,
            "request": request,
        }))?;
        let mut transaction = self.pool.begin().await.map_err(db)?;
        match IdempotencyStore::reserve(
            &mut transaction,
            Domain::Control,
            ADD_PROJECT_MEMBERSHIP,
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
        let project = project_from_tx(&mut transaction, project_id).await?;
        if !platform_admin && project.owner_actor_id != actor_id {
            return Err(ControlError::ProjectGovernanceDenied);
        }
        if project.state == ProjectState::Archived {
            return Err(ControlError::ProjectArchived);
        }
        let row = sqlx::query(
            "INSERT INTO access.project_memberships \
             (course_id,project_id,actor_id,role,state,revision,expires_at,updated_at) \
             VALUES ($1,$2,$3,$4,'active',1,$5,$6) \
             ON CONFLICT (project_id,actor_id) DO UPDATE SET \
                 course_id=EXCLUDED.course_id, \
                 role=EXCLUDED.role, \
                 state='active', \
                 revision=access.project_memberships.revision+1, \
                 expires_at=EXCLUDED.expires_at, \
                 updated_at=EXCLUDED.updated_at \
             RETURNING course_id,project_id,actor_id,role,state,revision,expires_at",
        )
        .bind(project.course_id.map(CourseId::as_uuid))
        .bind(project_id.as_uuid())
        .bind(request.actor_id.as_uuid())
        .bind(platform_role_name(request.role))
        .bind(request.expires_at.map(UtcTimestamp::get))
        .bind(now.get())
        .fetch_one(&mut *transaction)
        .await
        .map_err(db)?;
        let membership = project_membership_from_row(&row)?;
        let contract =
            serde_json::to_value(&membership).map_err(|_| ControlError::ContractInvalid)?;
        IdempotencyStore::complete(
            &mut transaction,
            Domain::Control,
            ADD_PROJECT_MEMBERSHIP,
            idempotency_key.as_str(),
            &contract,
        )
        .await
        .map_err(|_| ControlError::PersistenceFailed)?;
        transaction.commit().await.map_err(db)?;
        Ok(membership)
    }

    /// Revokes one membership with an exact revision fence; the project owner
    /// remains protected as the initial governance principal.
    #[allow(
        clippy::too_many_arguments,
        reason = "membership mutation keeps the target and acting identity fences adjacent"
    )]
    pub async fn remove_project_membership(
        &self,
        project_id: ProjectId,
        target_actor_id: ActorId,
        actor_id: ActorId,
        platform_admin: bool,
        request: &RemoveProjectMembershipRequest,
        idempotency_key: &IdempotencyKey,
        now: UtcTimestamp,
    ) -> Result<ProjectMembership, ControlError> {
        if request.reason.trim().is_empty() {
            return Err(ControlError::ContractInvalid);
        }
        let request_hash = canonical_hash(&json!({
            "projectId": project_id,
            "targetActorId": target_actor_id,
            "actorId": actor_id,
            "request": request,
        }))?;
        let mut transaction = self.pool.begin().await.map_err(db)?;
        match IdempotencyStore::reserve(
            &mut transaction,
            Domain::Control,
            REMOVE_PROJECT_MEMBERSHIP,
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
        let project = project_from_tx(&mut transaction, project_id).await?;
        if !platform_admin && project.owner_actor_id != actor_id {
            return Err(ControlError::ProjectGovernanceDenied);
        }
        if project.owner_actor_id == target_actor_id {
            return Err(ControlError::OwnerMembershipProtected);
        }
        let row = sqlx::query(
            "SELECT course_id,project_id,actor_id,role,state,revision,expires_at \
             FROM access.project_memberships WHERE project_id=$1 AND actor_id=$2 AND state='active' FOR UPDATE",
        )
        .bind(project_id.as_uuid())
        .bind(target_actor_id.as_uuid())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?
        .ok_or(ControlError::MembershipNotFound)?;
        let membership = project_membership_from_row(&row)?;
        if membership.revision != request.expected_revision {
            return Err(ControlError::RevisionConflict);
        }
        let updated_revision = Revision::new(
            membership
                .revision
                .get()
                .checked_add(1)
                .ok_or(ControlError::ContractInvalid)?,
        )
        .map_err(|_| ControlError::ContractInvalid)?;
        let updated = sqlx::query(
            "UPDATE access.project_memberships SET state='revoked',revision=$3,updated_at=$4 \
             WHERE project_id=$1 AND actor_id=$2 AND revision=$5",
        )
        .bind(project_id.as_uuid())
        .bind(target_actor_id.as_uuid())
        .bind(i64_revision(updated_revision)?)
        .bind(now.get())
        .bind(i64_revision(request.expected_revision)?)
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        if updated.rows_affected() != 1 {
            return Err(ControlError::PersistenceIdentityMismatch);
        }
        let membership = ProjectMembership {
            revision: updated_revision,
            state: MembershipState::Revoked,
            ..membership
        };
        let contract =
            serde_json::to_value(&membership).map_err(|_| ControlError::ContractInvalid)?;
        IdempotencyStore::complete(
            &mut transaction,
            Domain::Control,
            REMOVE_PROJECT_MEMBERSHIP,
            idempotency_key.as_str(),
            &contract,
        )
        .await
        .map_err(|_| ControlError::PersistenceFailed)?;
        transaction.commit().await.map_err(db)?;
        Ok(membership)
    }

    /// Creates one idempotent upload authority without persisting credentials or original paths.
    pub async fn create_upload(
        &self,
        course_id: CourseId,
        request: &CreateProblemPackageUploadRequest,
        idempotency_key: &IdempotencyKey,
        now: UtcTimestamp,
    ) -> Result<ProblemPackageUploadSession, ControlError> {
        self.create_upload_in_scope(
            request.project_id,
            Some(course_id),
            request,
            idempotency_key,
            now,
        )
        .await
    }

    /// Creates an upload authority for a project, including projects without a course.
    pub async fn create_project_upload(
        &self,
        project_id: ProjectId,
        request: &CreateProblemPackageUploadRequest,
        idempotency_key: &IdempotencyKey,
        now: UtcTimestamp,
    ) -> Result<ProblemPackageUploadSession, ControlError> {
        let project = self.project(project_id).await?;
        if project.state == ProjectState::Archived {
            return Err(ControlError::ProjectArchived);
        }
        self.create_upload_in_scope(project_id, project.course_id, request, idempotency_key, now)
            .await
    }

    async fn create_upload_in_scope(
        &self,
        project_id: ProjectId,
        course_id: Option<CourseId>,
        request: &CreateProblemPackageUploadRequest,
        idempotency_key: &IdempotencyKey,
        now: UtcTimestamp,
    ) -> Result<ProblemPackageUploadSession, ControlError> {
        validate_upload_request(request, &self.config)?;
        if request.project_id != project_id || request.course_id != course_id {
            return Err(ControlError::ProjectMismatch);
        }
        let request_hash = canonical_hash(&json!({
            "projectId":project_id,
            "courseId":course_id,
            "request":request
        }))?;
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
                project_id,
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
            project_id: request.project_id,
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
             (upload_id,project_id,course_id,revision,state,retention_policy_revision,expires_at) \
             VALUES ($1,$2,$3,1,'pending',$4,$5)",
        )
        .bind(upload_id.as_uuid())
        .bind(request.project_id.as_uuid())
        .bind(request.course_id.map(CourseId::as_uuid))
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
        self.complete_upload_in_scope(
            None,
            Some(course_id),
            upload_id,
            expected_revision,
            idempotency_key,
            now,
        )
        .await
    }

    /// Completes a package in a project scope, including projects without a course.
    pub async fn complete_project_upload(
        &self,
        project_id: ProjectId,
        upload_id: UploadSessionId,
        expected_revision: Revision,
        idempotency_key: &IdempotencyKey,
        now: UtcTimestamp,
    ) -> Result<ProblemPackage, ControlError> {
        let project = self.project(project_id).await?;
        if project.state == ProjectState::Archived {
            return Err(ControlError::ProjectArchived);
        }
        self.complete_upload_in_scope(
            Some(project_id),
            project.course_id,
            upload_id,
            expected_revision,
            idempotency_key,
            now,
        )
        .await
    }

    async fn complete_upload_in_scope(
        &self,
        project_id: Option<ProjectId>,
        course_id: Option<CourseId>,
        upload_id: UploadSessionId,
        expected_revision: Revision,
        idempotency_key: &IdempotencyKey,
        now: UtcTimestamp,
    ) -> Result<ProblemPackage, ControlError> {
        let request_hash = canonical_hash(&json!({
            "projectId": project_id,
            "courseId": course_id,
            "uploadId": upload_id,
            "expectedRevision": expected_revision,
        }))?;
        let completion_lease = match self
            .reserve_completion(
                project_id,
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
                (Some(version), Some(artifact_id)) => {
                    let expected = contracts::ArtifactRef {
                        artifact_id: artifact_id_from_uuid(artifact_id)?,
                        store_binding: self.objects.binding().to_owned(),
                        object_version: version,
                        size_bytes: size,
                        media_type,
                    };
                    self.objects
                        .read_verified(&key, &expected)
                        .await
                        .map_err(ControlError::from)
                }
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
            "SELECT project_id,course_id,retention_policy_revision FROM control.problem_package_upload_sessions \
             WHERE upload_id=$1 AND ($2::uuid IS NULL OR project_id=$2) AND course_id IS NOT DISTINCT FROM $3 \
               AND state='completing' AND completion_lease_token=$4 AND completion_lease_expires_at>now()",
        )
        .bind(upload_id.as_uuid())
        .bind(project_id.map(ProjectId::as_uuid))
        .bind(course_id.map(CourseId::as_uuid))
        .bind(completion_lease)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?
        .ok_or(ControlError::UploadStateConflict)?;
        let stored_project_id = ProjectId::from_str(
            &session
                .try_get::<Uuid, _>("project_id")
                .map_err(db)?
                .to_string(),
        )
        .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        let stored_course_id = session
            .try_get::<Option<Uuid>, _>("course_id")
            .map_err(db)?
            .map(|id| CourseId::from_str(&id.to_string()))
            .transpose()
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        if stored_course_id != course_id {
            return Err(ControlError::CourseMismatch);
        }
        if project_id.is_some_and(|expected| expected != stored_project_id) {
            return Err(ControlError::ProjectMismatch);
        }
        let policy_revision =
            revision_from_i64(session.try_get("retention_policy_revision").map_err(db)?)?;
        let package = ProblemPackage {
            id: ProblemPackageId::new(),
            project_id: stored_project_id,
            course_id: stored_course_id,
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
        policy: ProjectLlmEgressPolicy,
        idempotency_key: &IdempotencyKey,
    ) -> Result<ProjectLlmEgressPolicy, ControlError> {
        self.activate_policy_in_scope(policy.project_id, Some(course_id), policy, idempotency_key)
            .await
    }

    /// Activates one append-only project policy, including independent Work projects.
    pub async fn activate_project_policy(
        &self,
        project_id: ProjectId,
        policy: ProjectLlmEgressPolicy,
        idempotency_key: &IdempotencyKey,
    ) -> Result<ProjectLlmEgressPolicy, ControlError> {
        let project = self.project(project_id).await?;
        if project.state == ProjectState::Archived {
            return Err(ControlError::ProjectArchived);
        }
        self.activate_policy_in_scope(project_id, project.course_id, policy, idempotency_key)
            .await
    }

    async fn activate_policy_in_scope(
        &self,
        project_id: ProjectId,
        course_id: Option<CourseId>,
        mut policy: ProjectLlmEgressPolicy,
        idempotency_key: &IdempotencyKey,
    ) -> Result<ProjectLlmEgressPolicy, ControlError> {
        if policy.project_id != project_id || policy.course_id != course_id {
            return Err(ControlError::ProjectMismatch);
        }
        policy.validate().map_err(|_| ControlError::PolicyInvalid)?;
        let request_hash = canonical_hash(&policy)?;
        let mut transaction = self.pool.begin().await.map_err(db)?;
        advisory_project_lock(&mut transaction, project_id).await?;
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
            "SELECT COALESCE(MAX(revision),0)+1 FROM control.project_llm_policies WHERE project_id=$1",
        )
        .bind(policy.project_id.as_uuid())
        .fetch_one(&mut *transaction)
        .await
        .map_err(db)?;
        policy.revision = revision_from_i64(next)?;
        sqlx::query(
            "UPDATE control.project_llm_policies SET superseded_at=$2 \
             WHERE project_id=$1 AND superseded_at IS NULL",
        )
        .bind(policy.project_id.as_uuid())
        .bind(policy.activated_at.get())
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        let contract = serde_json::to_value(&policy).map_err(|_| ControlError::ContractInvalid)?;
        sqlx::query(
            "INSERT INTO control.project_llm_policies \
             (policy_id,project_id,course_id,revision,contract_sha256,contract,activated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7)",
        )
        .bind(policy.id.as_uuid())
        .bind(policy.project_id.as_uuid())
        .bind(course_id.map(CourseId::as_uuid))
        .bind(i64_revision(policy.revision)?)
        .bind(canonical_hash(&policy)?.to_string())
        .bind(&contract)
        .bind(policy.activated_at.get())
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        if let Some(course_id) = course_id {
            append_sse(
                &mut transaction,
                course_id,
                "course_llm_policy.activated.v1",
                policy.id.as_uuid(),
                policy.revision,
                json!({"policyId":policy.id,"revision":policy.revision}),
            )
            .await?;
        } else {
            append_project_sse(
                &mut transaction,
                project_id,
                "project_llm_policy.activated.v1",
                policy.id.as_uuid(),
                policy.revision,
                json!({"policyId":policy.id,"revision":policy.revision}),
            )
            .await?;
        }
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
    ) -> Result<ProjectLlmEgressPolicy, ControlError> {
        load_contract(
            &self.pool,
            "SELECT contract FROM control.project_llm_policies WHERE course_id=$1 AND superseded_at IS NULL",
            course_id.as_uuid(),
        )
        .await
        .map_err(|error| match error {
            ControlError::NotFound => ControlError::PolicyNotFound,
            other => other,
        })
    }

    /// Returns the active policy for the exact project, without a course fallback.
    pub async fn active_project_policy(
        &self,
        project_id: ProjectId,
    ) -> Result<ProjectLlmEgressPolicy, ControlError> {
        load_contract(
            &self.pool,
            "SELECT contract FROM control.project_llm_policies \
             WHERE project_id=$1 AND superseded_at IS NULL",
            project_id.as_uuid(),
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

    /// Reads one completed immutable package in the exact project scope.
    pub async fn project_package(
        &self,
        project_id: ProjectId,
        package_id: ProblemPackageId,
    ) -> Result<ProblemPackage, ControlError> {
        load_contract_two(
            &self.pool,
            "SELECT contract FROM control.problem_packages WHERE package_id=$1 AND project_id=$2",
            package_id.as_uuid(),
            project_id.as_uuid(),
        )
        .await
    }

    /// Resolves internal opaque object keys for an exact completed package.
    pub async fn package_object_locators(
        &self,
        course_id: CourseId,
        package: &ProblemPackage,
    ) -> Result<std::collections::BTreeMap<contracts::ArtifactId, String>, ControlError> {
        if package.course_id != Some(course_id) {
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

    /// Resolves opaque object keys for a completed package in the exact project scope.
    pub async fn project_package_object_locators(
        &self,
        project_id: ProjectId,
        package: &ProblemPackage,
    ) -> Result<std::collections::BTreeMap<contracts::ArtifactId, String>, ControlError> {
        if package.project_id != project_id {
            return Err(ControlError::ProjectMismatch);
        }
        let rows = sqlx::query(
            "SELECT files.artifact_id,files.object_key FROM control.problem_package_upload_files files \
             JOIN control.problem_package_upload_sessions sessions ON sessions.upload_id=files.upload_id \
             WHERE sessions.project_id=$1 AND sessions.completed_package_id=$2 AND sessions.state='completed'",
        )
        .bind(project_id.as_uuid())
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

    /// Reads the latest durable `AgentRun` projection in the exact project scope.
    pub async fn projected_agent_run(
        &self,
        project_id: ProjectId,
        run_id: contracts::AgentRunId,
    ) -> Result<contracts::authoring::AgentRun, ControlError> {
        load_contract_two(
            &self.pool,
            "SELECT contract FROM control.agent_run_projections WHERE run_id=$1 AND project_id=$2",
            run_id.as_uuid(),
            project_id.as_uuid(),
        )
        .await
    }

    /// Reads and verifies one Agent-owned generated object through the shared immutable store.
    pub async fn read_generated_artifact(
        &self,
        record: &GeneratedArtifactRecord,
    ) -> Result<Vec<u8>, ControlError> {
        let object = self
            .objects
            .read_verified(&record.object_key, &record.artifact)
            .await?;
        if object.reference != record.artifact
            || Sha256Digest::of_bytes(&object.bytes).to_string() != record.content_sha256
        {
            return Err(ControlError::ObjectStoreIdentityMismatch);
        }
        Ok(object.bytes)
    }

    /// Loads one exact Work configuration preauthorization revision.
    pub async fn work_configuration_preauthorization(
        &self,
        project_id: ProjectId,
        preauthorization_id: contracts::WorkConfigurationPreauthorizationId,
        revision: Revision,
    ) -> Result<contracts::authoring::WorkConfigurationPreauthorization, ControlError> {
        let row = sqlx::query(
            "SELECT contract FROM control.work_configuration_preauthorizations \
             WHERE preauthorization_id=$1 AND project_id=$2 AND revision=$3",
        )
        .bind(preauthorization_id.as_uuid())
        .bind(project_id.as_uuid())
        .bind(i64_revision(revision)?)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?
        .ok_or(ControlError::NotFound)?;
        let grant: contracts::authoring::WorkConfigurationPreauthorization =
            serde_json::from_value(row.try_get("contract").map_err(db)?)
                .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        if grant.id != preauthorization_id
            || grant.project_id != project_id
            || grant.revision != revision
        {
            return Err(ControlError::PersistenceIdentityMismatch);
        }
        Ok(grant)
    }

    /// Persists one exact approved Work plan and returns its concrete grant.
    #[allow(
        clippy::too_many_arguments,
        reason = "approval persists one exact run, actor, request, and time fence"
    )]
    pub async fn prepare_work_configuration_approval(
        &self,
        project_id: ProjectId,
        run_id: contracts::AgentRunId,
        run: &AgentRun,
        actor_id: ActorId,
        request: &ApproveWorkConfigurationRequest,
        idempotency_key: &IdempotencyKey,
        now: UtcTimestamp,
    ) -> Result<contracts::authoring::WorkConfigurationPreauthorization, ControlError> {
        let request_hash = canonical_hash(&json!({
            "projectId": project_id,
            "runId": run_id,
            "actorId": actor_id,
            "request": request,
        }))?;
        let mut transaction = self.pool.begin().await.map_err(db)?;
        advisory_project_lock(&mut transaction, project_id).await?;
        match IdempotencyStore::reserve(
            &mut transaction,
            Domain::Control,
            APPROVE_WORK_CONFIGURATION,
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
        run.validate()
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        if run.id != run_id
            || run.project_id != project_id
            || run.revision != request.expected_run_revision
            || run.state != AgentRunState::AwaitingApproval
            || request.reason.trim().is_empty()
            || request.reason.chars().count() > 500
            || request.expires_at <= now
        {
            return Err(ControlError::RevisionConflict);
        }
        let AgentRunPurpose::WorkConfiguration {
            environment_id,
            environment_revision,
            actor_id: run_actor,
            ..
        } = run.purpose
        else {
            return Err(ControlError::CandidateKindMismatch);
        };
        let plan = run.plan.as_ref().ok_or(ControlError::NotFound)?;
        if run_actor != actor_id
            || request.expected_plan_revision != plan.revision
            || request.environment_revision != environment_revision
            || plan.environment_id != environment_id
            || plan.environment_revision != environment_revision
            || (plan.requires_restart && !request.restart_confirmed)
        {
            return Err(ControlError::RevisionConflict);
        }
        let grant = contracts::authoring::WorkConfigurationPreauthorization {
            id: contracts::WorkConfigurationPreauthorizationId::new(),
            project_id,
            environment_id,
            environment_revision,
            actor_id,
            plan_id: plan.id,
            plan_revision: plan.revision,
            script_artifact: plan.script_artifact.clone(),
            verification_script_artifact: plan.verification_script_artifact.clone(),
            expires_at: request.expires_at,
            revision: Revision::new(1).map_err(|_| ControlError::ContractInvalid)?,
        };
        grant
            .validate_against_plan(plan)
            .map_err(|_| ControlError::ContractInvalid)?;
        let contract = serde_json::to_value(&grant).map_err(|_| ControlError::ContractInvalid)?;
        sqlx::query(
            "INSERT INTO control.work_configuration_preauthorizations \
             (preauthorization_id,project_id,environment_id,environment_revision,actor_id,plan_id,plan_revision,script_artifact,verification_script_artifact,expires_at,revision,contract) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)",
        )
        .bind(grant.id.as_uuid())
        .bind(grant.project_id.as_uuid())
        .bind(grant.environment_id.as_uuid())
        .bind(i64_revision(grant.environment_revision)?)
        .bind(grant.actor_id.as_uuid())
        .bind(grant.plan_id.as_uuid())
        .bind(i64_revision(grant.plan_revision)?)
        .bind(serde_json::to_value(&grant.script_artifact).map_err(|_| ControlError::ContractInvalid)?)
        .bind(grant.verification_script_artifact.as_ref().map(serde_json::to_value).transpose().map_err(|_| ControlError::ContractInvalid)?)
        .bind(grant.expires_at.get())
        .bind(i64_revision(grant.revision)?)
        .bind(&contract)
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        IdempotencyStore::complete(
            &mut transaction,
            Domain::Control,
            APPROVE_WORK_CONFIGURATION,
            idempotency_key.as_str(),
            &contract,
        )
        .await
        .map_err(|_| ControlError::PersistenceFailed)?;
        transaction.commit().await.map_err(db)?;
        Ok(grant)
    }

    /// Resolves Work execution admission from the current Agent-owned run aggregate.
    ///
    /// Agent state is projected into Control asynchronously.  Callers at the execution boundary
    /// must supply the current Agent response so a stale projection cannot authorize an old run
    /// revision or hide a cancellation.  Membership and preauthorization remain Control-owned
    /// checks and are read in the transaction-independent Control database boundary.
    pub async fn work_configuration_admission_with_run(
        &self,
        run_id: contracts::AgentRunId,
        query: &WorkConfigurationAdmissionQuery,
        run: &AgentRun,
        recovery: Option<&AgentWorkExecutionIntentMetadata>,
    ) -> Result<WorkConfigurationAdmissionBinding, ControlError> {
        let recovery_requested = query.execution_id.is_some();
        if recovery_requested != recovery.is_some() {
            return Err(ControlError::PersistenceIdentityMismatch);
        }
        run.validate()
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        if run.id != run_id
            || run.project_id != query.project_id
            || run.course_id != query.course_id
        {
            return Err(ControlError::RevisionConflict);
        }
        if recovery_requested {
            // Recovery is a reconciliation path for an already persisted VM side effect.  It
            // must remain usable after membership/preauthorization revocation, while still
            // requiring the exact immutable intent that Agent stored before the side effect.
            let intent = recovery.ok_or(ControlError::PersistenceIdentityMismatch)?;
            intent
                .validate()
                .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
            if intent.run_id != run_id
                || intent.project_id != query.project_id
                || intent.course_id != query.course_id
                || intent.run_revision != query.run_revision
                || intent.execution_id != query.execution_id.unwrap_or_default()
            {
                return Err(ControlError::RevisionConflict);
            }
            if !matches!(
                run.state,
                AgentRunState::Running | AgentRunState::Cancelling
            ) {
                return Err(ControlError::RevisionConflict);
            }
        } else {
            // The admission is a new authorization decision at execution time.  A run and its
            // preauthorization retain the actor who requested approval, but neither is a
            // substitute for the current Project ownership boundary.
            self.require_active_work_actor(query.project_id, query.actor_id, query.course_id)
                .await?;
            if run.revision != query.run_revision || run.state != AgentRunState::Running {
                return Err(ControlError::RevisionConflict);
            }
        }
        let AgentRunPurpose::WorkConfiguration {
            environment_id,
            environment_revision,
            actor_id,
            ..
        } = run.purpose
        else {
            return Err(ControlError::CandidateKindMismatch);
        };
        if environment_id != query.environment_id
            || environment_revision != query.environment_revision
            || actor_id != query.actor_id
        {
            return Err(ControlError::ProjectMismatch);
        }
        let plan = run.plan.clone();
        if let Some(intent) = recovery
            && plan.as_ref().is_none_or(|plan| {
                plan.id != intent.plan_id || plan.revision != intent.plan_revision
            })
        {
            return Err(ControlError::PersistenceIdentityMismatch);
        }
        let preauthorization = if recovery_requested {
            None
        } else if let Some(plan) = &plan {
            let row = sqlx::query(
                "SELECT contract FROM control.work_configuration_preauthorizations \
                 WHERE project_id=$1 AND environment_id=$2 AND environment_revision=$3 \
                   AND actor_id=$4 AND plan_id=$5 AND plan_revision=$6 \
                   AND expires_at>clock_timestamp() ORDER BY created_at DESC LIMIT 1",
            )
            .bind(query.project_id.as_uuid())
            .bind(query.environment_id.as_uuid())
            .bind(i64_revision(query.environment_revision)?)
            .bind(query.actor_id.as_uuid())
            .bind(plan.id.as_uuid())
            .bind(i64_revision(plan.revision)?)
            .fetch_optional(&self.pool)
            .await
            .map_err(db)?;
            row.map(|row| -> Result<_, ControlError> {
                let grant: contracts::authoring::WorkConfigurationPreauthorization =
                    serde_json::from_value(row.try_get("contract").map_err(db)?)
                        .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
                grant
                    .validate_against_plan(plan)
                    .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
                Ok(grant)
            })
            .transpose()?
        } else {
            None
        };
        Ok(WorkConfigurationAdmissionBinding {
            run_id,
            project_id: query.project_id,
            course_id: query.course_id,
            environment_id,
            environment_revision,
            actor_id,
            run_revision: run.revision,
            state: run.state,
            plan,
            preauthorization,
            recovery: recovery.map(|intent| WorkConfigurationRecoveryIdentity {
                execution_id: intent.execution_id,
                plan_id: intent.plan_id,
                plan_revision: intent.plan_revision,
                source_identity: intent.source_identity.clone(),
            }),
            script_sha256: String::new(),
            verification_script_sha256: None,
        })
    }

    /// Rechecks the live Project and Access membership before a Work execution admission.
    ///
    /// The check intentionally reads the Access-owned membership table directly through the
    /// constrained Control database grant.  This mirrors Access's project-scope rule, including
    /// the matching course membership required by a teaching-associated Project, while keeping
    /// cleanup/query of an already persisted Environment execution independent of this gate.
    async fn require_active_work_actor(
        &self,
        project_id: ProjectId,
        actor_id: ActorId,
        course_id: Option<CourseId>,
    ) -> Result<(), ControlError> {
        let project = self.project(project_id).await?;
        if project.state == ProjectState::Archived || project.course_id != course_id {
            return Err(ControlError::ProjectArchived);
        }
        let active = sqlx::query_scalar::<_, bool>(
            r"SELECT EXISTS (
                SELECT 1
                FROM access.project_memberships pm
                WHERE pm.project_id = $1
                  AND pm.actor_id = $2
                  AND pm.state = 'active'
                  AND (pm.expires_at IS NULL OR pm.expires_at > clock_timestamp())
                  AND (
                      $3::uuid IS NULL
                      OR EXISTS (
                          SELECT 1
                          FROM access.course_memberships cm
                          WHERE cm.course_id = $3
                            AND cm.actor_id = $2
                            AND cm.role = pm.role
                            AND cm.state = 'active'
                            AND (
                                cm.expires_at IS NULL
                                OR cm.expires_at > clock_timestamp()
                            )
                      )
                  )
            )",
        )
        .bind(project_id.as_uuid())
        .bind(actor_id.as_uuid())
        .bind(course_id.map(CourseId::as_uuid))
        .fetch_one(&self.pool)
        .await
        .map_err(db)?;
        if !active {
            return Err(ControlError::ProjectMismatch);
        }
        Ok(())
    }

    /// Reads a validated Environment candidate projection.
    pub async fn environment_candidate(
        &self,
        course_id: CourseId,
        candidate_id: CandidateId,
    ) -> Result<EnvironmentCandidate, ControlError> {
        load_candidate_contract(&self.pool, course_id, candidate_id, "environment").await
    }

    /// Reads a validated Environment candidate in the exact project scope.
    pub async fn project_environment_candidate(
        &self,
        project_id: ProjectId,
        candidate_id: CandidateId,
    ) -> Result<EnvironmentCandidate, ControlError> {
        load_candidate_project_contract(&self.pool, project_id, candidate_id, "environment").await
    }

    /// Reads the Control-owned teacher view for one Environment candidate.
    pub async fn environment_candidate_view(
        &self,
        course_id: CourseId,
        candidate_id: CandidateId,
    ) -> Result<EnvironmentCandidateView, ControlError> {
        let candidate = self.environment_candidate(course_id, candidate_id).await?;
        let approvals = load_candidate_approvals(&self.pool, candidate_id).await?;
        let build = load_candidate_build(&self.pool, course_id, &candidate).await?;
        let image_artifact = resolve_candidate_image_artifact(
            &candidate,
            build.as_ref(),
            &self.config.virtual_machine_base,
        );
        Ok(EnvironmentCandidateView {
            candidate,
            approvals,
            build,
            image_artifact: image_artifact?,
            trust_revision: self.config.trust_revision,
        })
    }

    /// Reads the teacher view for an Environment candidate in a project scope.
    pub async fn project_environment_candidate_view(
        &self,
        project_id: ProjectId,
        candidate_id: CandidateId,
    ) -> Result<EnvironmentCandidateView, ControlError> {
        let candidate = self
            .project_environment_candidate(project_id, candidate_id)
            .await?;
        let approvals = load_candidate_approvals(&self.pool, candidate_id).await?;
        let build = load_candidate_build_project(&self.pool, project_id, &candidate).await?;
        let image_artifact = resolve_candidate_image_artifact(
            &candidate,
            build.as_ref(),
            &self.config.virtual_machine_base,
        );
        Ok(EnvironmentCandidateView {
            candidate,
            approvals,
            build,
            image_artifact: image_artifact?,
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

    /// Reads a validated Evaluation candidate in the exact project scope.
    pub async fn project_evaluation_candidate(
        &self,
        project_id: ProjectId,
        candidate_id: CandidateId,
    ) -> Result<EvaluationCandidate, ControlError> {
        load_candidate_project_contract(&self.pool, project_id, candidate_id, "evaluation").await
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

    /// Reads the teacher view for an Evaluation candidate in a project scope.
    pub async fn project_evaluation_candidate_view(
        &self,
        project_id: ProjectId,
        candidate_id: CandidateId,
    ) -> Result<EvaluationCandidateView, ControlError> {
        let candidate = self
            .project_evaluation_candidate(project_id, candidate_id)
            .await?;
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
            "SELECT revision FROM control.project_llm_policies WHERE course_id=$1 AND superseded_at IS NULL",
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
        let object_locators = self.package_object_locators(course_id, &package).await?;
        Ok(InternalPublishEvaluationReleaseRequest {
            project_id: candidate.project_id,
            course_id: candidate.course_id,
            candidate_id: candidate.id,
            candidate_revision: candidate.revision,
            approval_id: approval.id,
            approval_revision: Revision::new(1)
                .map_err(|_| ControlError::PersistenceIdentityMismatch)?,
            evaluation_spec: candidate.spec,
            execution_binding: EvaluationExecutionBinding {
                package,
                object_locators,
            },
            runtime_identity: self.config.evaluation_runtime.identity()?,
            published_by,
        })
    }

    /// Reads one immutable release in the exact project scope. The optional
    /// course context is an additional filter; omitting it returns the
    /// project's release regardless of whether it carries teaching context.
    pub async fn project_release(
        &self,
        project_id: ProjectId,
        release_id: ReleaseId,
        course_id: Option<CourseId>,
        actor_id: ActorId,
    ) -> Result<EnvironmentTemplateReleaseView, ControlError> {
        let row = sqlx::query(
            "SELECT releases.release_id,releases.project_id AS release_project_id,releases.course_id AS release_course_id,\
                    releases.version AS release_version,\
                    releases.environment_candidate_id AS release_candidate_id,\
                    releases.candidate_revision AS release_candidate_revision,\
                    releases.contract AS release_contract,withdrawals.contract AS withdrawal_contract,\
                    projects.owner_actor_id,\
                    candidates.project_id AS candidate_project_id,candidates.course_id AS candidate_course_id,\
                    candidates.revision AS candidate_revision,candidates.contract AS candidate_contract,\
                    publications.contract AS publication_contract \
             FROM control.environment_template_releases releases \
             LEFT JOIN control.release_withdrawals withdrawals ON withdrawals.release_id=releases.release_id \
             JOIN control.projects projects ON projects.project_id=releases.project_id \
             JOIN control.candidates candidates ON candidates.candidate_id=releases.environment_candidate_id \
               AND candidates.project_id=releases.project_id \
               AND candidates.course_id IS NOT DISTINCT FROM releases.course_id \
               AND candidates.candidate_kind='environment' \
               AND candidates.state='validated' \
               AND candidates.revision=releases.candidate_revision \
             LEFT JOIN control.authoring_approval_publications publications \
               ON publications.project_id=releases.project_id \
               AND publications.environment_release_id=releases.release_id \
               AND publications.state='ready' \
             WHERE releases.release_id=$1 AND releases.project_id=$2 \
               AND ($3::uuid IS NULL OR releases.course_id IS NOT DISTINCT FROM $3) \
               AND ((candidates.contract->'spec'->>'class'='work' AND projects.owner_actor_id=$4) \
                    OR (candidates.contract->'spec'->>'class'='experiment' \
                        AND publications.environment_release_id=releases.release_id))",
        )
        .bind(release_id.as_uuid())
        .bind(project_id.as_uuid())
        .bind(course_id.map(CourseId::as_uuid))
        .bind(actor_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?
        .ok_or(ControlError::NotFound)?;
        project_release_view(&row, project_id, actor_id)
    }

    /// Lists immutable releases in one project with an optional course filter.
    pub async fn project_releases(
        &self,
        project_id: ProjectId,
        course_id: Option<CourseId>,
        after_version: u64,
        limit: u32,
        actor_id: ActorId,
    ) -> Result<Vec<EnvironmentTemplateReleaseView>, ControlError> {
        if limit == 0 || limit > 100 {
            return Err(ControlError::ContractInvalid);
        }
        let rows = sqlx::query(
            "SELECT releases.release_id,releases.project_id AS release_project_id,releases.course_id AS release_course_id,\
                    releases.version AS release_version,\
                    releases.environment_candidate_id AS release_candidate_id,\
                    releases.candidate_revision AS release_candidate_revision,\
                    releases.contract AS release_contract,withdrawals.contract AS withdrawal_contract,\
                    projects.owner_actor_id,\
                    candidates.project_id AS candidate_project_id,candidates.course_id AS candidate_course_id,\
                    candidates.revision AS candidate_revision,candidates.contract AS candidate_contract,\
                    publications.contract AS publication_contract \
             FROM control.environment_template_releases releases \
             LEFT JOIN control.release_withdrawals withdrawals ON withdrawals.release_id=releases.release_id \
             JOIN control.projects projects ON projects.project_id=releases.project_id \
             JOIN control.candidates candidates ON candidates.candidate_id=releases.environment_candidate_id \
               AND candidates.project_id=releases.project_id \
               AND candidates.course_id IS NOT DISTINCT FROM releases.course_id \
               AND candidates.candidate_kind='environment' \
               AND candidates.state='validated' \
               AND candidates.revision=releases.candidate_revision \
             LEFT JOIN control.authoring_approval_publications publications \
               ON publications.project_id=releases.project_id \
               AND publications.environment_release_id=releases.release_id \
               AND publications.state='ready' \
             WHERE releases.project_id=$1 \
               AND ($2::uuid IS NULL OR releases.course_id IS NOT DISTINCT FROM $2) \
               AND releases.version>$3 \
               AND ((candidates.contract->'spec'->>'class'='work' AND projects.owner_actor_id=$5) \
                    OR (candidates.contract->'spec'->>'class'='experiment' \
                        AND publications.environment_release_id=releases.release_id)) \
             ORDER BY releases.version,releases.release_id LIMIT $4",
        )
        .bind(project_id.as_uuid())
        .bind(course_id.map(CourseId::as_uuid))
        .bind(i64::try_from(after_version).map_err(|_| ControlError::ContractInvalid)?)
        .bind(i64::from(limit))
        .bind(actor_id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        rows.into_iter()
            .map(|row| project_release_view(&row, project_id, actor_id))
            .collect()
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
             (run_id,project_id,course_id,revision,state,contract_sha256,contract,projected_event_id) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8) \
             ON CONFLICT (run_id) DO UPDATE SET revision=EXCLUDED.revision,state=EXCLUDED.state, \
             contract_sha256=EXCLUDED.contract_sha256,contract=EXCLUDED.contract, \
             projected_event_id=EXCLUDED.projected_event_id,updated_at=now() \
             WHERE control.agent_run_projections.revision < EXCLUDED.revision",
        )
        .bind(run.id.as_uuid())
        .bind(run.project_id.as_uuid())
        .bind(run.course_id.map(CourseId::as_uuid))
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
        generated_context: Option<&GeneratedArtifactRecord>,
    ) -> Result<(), ControlError> {
        run.validate().map_err(|_| ControlError::ContractInvalid)?;
        if environment.is_none() && evaluation.is_none() {
            return Err(ControlError::CandidateMissing);
        }
        if generated_context.is_some() && environment.is_none() {
            return Err(ControlError::ContractInvalid);
        }
        if let Some(candidate) = environment {
            candidate
                .validate_against_run(run)
                .map_err(|_| ControlError::ProjectionConflict)?;
        }
        if let Some(candidate) = evaluation {
            candidate
                .validate_against_run(run)
                .map_err(|_| ControlError::ProjectionConflict)?;
        }
        let mut transaction = self.pool.begin().await.map_err(db)?;
        let run_contract = serde_json::to_value(run).map_err(|_| ControlError::ContractInvalid)?;
        sqlx::query(
            "INSERT INTO control.agent_run_projections \
             (run_id,project_id,course_id,revision,state,contract_sha256,contract,projected_event_id) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8) \
             ON CONFLICT (run_id) DO UPDATE SET revision=EXCLUDED.revision,state=EXCLUDED.state, \
             contract_sha256=EXCLUDED.contract_sha256,contract=EXCLUDED.contract, \
             projected_event_id=EXCLUDED.projected_event_id,updated_at=now() \
             WHERE control.agent_run_projections.revision < EXCLUDED.revision",
        )
        .bind(run.id.as_uuid())
        .bind(run.project_id.as_uuid())
        .bind(run.course_id.map(CourseId::as_uuid))
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
                run.project_id,
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
            enqueue_container_build(
                &mut transaction,
                &self.config,
                run.project_id,
                run.course_id,
                run.package_id,
                candidate,
                generated_context,
                candidate.created_at,
            )
            .await?;
        }
        if let Some(candidate) = evaluation {
            candidate
                .validate()
                .map_err(|_| ControlError::ContractInvalid)?;
            insert_candidate(
                &mut transaction,
                run.project_id,
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
        generated_context: Option<&GeneratedArtifactRecord>,
    ) -> Result<InboxDecision, ControlError> {
        let contract = EVENT_CONTRACTS
            .iter()
            .copied()
            .find(|contract| contract.subject == event.subject)
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
        if generated_context.is_some() && environment.is_none() {
            return Err(ControlError::ContractInvalid);
        }
        if let Some(candidate) = environment {
            candidate
                .validate_against_run(run)
                .map_err(|_| ControlError::ProjectionConflict)?;
        }
        if let Some(candidate) = evaluation {
            candidate
                .validate_against_run(run)
                .map_err(|_| ControlError::ProjectionConflict)?;
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
             (run_id,project_id,course_id,revision,state,contract_sha256,contract,projected_event_id) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8) \
             ON CONFLICT (run_id) DO UPDATE SET revision=EXCLUDED.revision,state=EXCLUDED.state, \
             contract_sha256=EXCLUDED.contract_sha256,contract=EXCLUDED.contract, \
             projected_event_id=EXCLUDED.projected_event_id,updated_at=now() \
             WHERE control.agent_run_projections.revision < EXCLUDED.revision \
                OR (control.agent_run_projections.revision=EXCLUDED.revision \
                    AND control.agent_run_projections.contract_sha256=EXCLUDED.contract_sha256)",
        )
        .bind(run.id.as_uuid())
        .bind(run.project_id.as_uuid())
        .bind(run.course_id.map(CourseId::as_uuid))
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
                run.project_id,
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
            enqueue_container_build(
                &mut transaction,
                &self.config,
                run.project_id,
                run.course_id,
                run.package_id,
                candidate,
                generated_context,
                event.time,
            )
            .await?;
        }
        if let Some(candidate) = evaluation {
            insert_candidate(
                &mut transaction,
                run.project_id,
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
        let event_payload =
            json!({"runId":run.id,"revision":run.revision,"state":agent_run_state_name(run.state)});
        if let Some(course_id) = run.course_id {
            append_sse(
                &mut transaction,
                course_id,
                "agent_run.state_changed.v1",
                run.id.as_uuid(),
                run.revision,
                event_payload,
            )
            .await?;
        } else {
            append_project_sse(
                &mut transaction,
                run.project_id,
                "agent_run.state_changed.v1",
                run.id.as_uuid(),
                run.revision,
                event_payload,
            )
            .await?;
        }
        transaction.commit().await.map_err(db)?;
        Ok(decision)
    }

    /// Stores an Agent-owned artifact resolution for later exact release comparison.
    pub async fn project_artifact(
        &self,
        event_id: EventId,
        project_id: ProjectId,
        course_id: Option<CourseId>,
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
            "SELECT project_id,course_id,state,image_artifact_id
             FROM control.container_build_projections
             WHERE build_request_id=$1 FOR UPDATE",
        )
        .bind(build_request_id.as_uuid())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?
        .ok_or(ControlError::ArtifactNotAuthoritative)?;
        if build.try_get::<Uuid, _>("project_id").map_err(db)? != project_id.as_uuid()
            || build.try_get::<Option<Uuid>, _>("course_id").map_err(db)?
                != course_id.map(CourseId::as_uuid)
        {
            return Err(ControlError::ProjectMismatch);
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
        project_id: ProjectId,
        course_id: Option<CourseId>,
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
            "SELECT project_id,course_id,command_sha256,state,terminal_diagnostic,cleanup_verified
             FROM control.container_build_projections WHERE build_request_id=$1 FOR UPDATE",
        )
        .bind(failure.build_request_id.as_uuid())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?
        .ok_or(ControlError::ArtifactNotAuthoritative)?;
        let observed_project: Uuid = row.try_get("project_id").map_err(db)?;
        let observed_course: Option<Uuid> = row.try_get("course_id").map_err(db)?;
        let _observed_command: String = row.try_get("command_sha256").map_err(db)?;
        let observed_state: String = row.try_get("state").map_err(db)?;
        if observed_project != project_id.as_uuid()
            || observed_course != course_id.map(CourseId::as_uuid)
        {
            return Err(ControlError::ProjectMismatch);
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

    /// Appends the only decision allowed for an exact project candidate revision.
    #[allow(clippy::too_many_arguments)]
    pub async fn decide_project_candidate(
        &self,
        project_id: ProjectId,
        candidate_id: CandidateId,
        expected_kind: AgentTrackKind,
        request: &CandidateDecisionRequest,
        actor_id: ActorId,
        expected_revision: Revision,
        idempotency_key: &IdempotencyKey,
        now: UtcTimestamp,
    ) -> Result<CandidateApproval, ControlError> {
        let project = self.project(project_id).await?;
        if project.state == ProjectState::Archived {
            return Err(ControlError::ProjectArchived);
        }
        self.decide_candidate_in_scope(
            project_id,
            project.course_id,
            candidate_id,
            expected_kind,
            request,
            actor_id,
            expected_revision,
            idempotency_key,
            now,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn decide_candidate_in_scope(
        &self,
        project_id: ProjectId,
        course_id: Option<CourseId>,
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
            "projectId":project_id,"courseId":course_id,"candidateId":candidate_id,"kind":expected_kind,
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
            "SELECT candidate_kind,project_id,course_id,revision,content_sha256,policy_revision,schema_sha256,contract \
             FROM control.candidates \
             WHERE candidate_id=$1 AND project_id=$2 \
               AND course_id IS NOT DISTINCT FROM $3 FOR UPDATE",
        )
        .bind(candidate_id.as_uuid())
        .bind(project_id.as_uuid())
        .bind(course_id.map(CourseId::as_uuid))
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?
        .ok_or(ControlError::CandidateNotFound)?;
        let observed_project_id = ProjectId::from_str(
            &row.try_get::<Uuid, _>("project_id")
                .map_err(db)?
                .to_string(),
        )
        .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        let observed_course_id = row
            .try_get::<Option<Uuid>, _>("course_id")
            .map_err(db)?
            .map(|value| CourseId::from_str(&value.to_string()))
            .transpose()
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        if observed_project_id != project_id || observed_course_id != course_id {
            return Err(ControlError::ProjectMismatch);
        }
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
            AgentTrackKind::WorkConfiguration => return Err(ControlError::CandidateKindMismatch),
        };
        if kind != expected_kind_name {
            return Err(ControlError::CandidateKindMismatch);
        }
        if request.decision == CandidateDecision::Approved {
            // Shared teaching material is completed through the single
            // authoring approval boundary. A candidate decision cannot act
            // as a second approval path. Independent Work may confirm only
            // its Environment candidate, and only its project owner may do so.
            if expected_kind != AgentTrackKind::Environment {
                return Err(ControlError::DecisionConflict);
            }
            let candidate: EnvironmentCandidate =
                serde_json::from_value(candidate_contract.clone())
                    .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
            candidate
                .validate()
                .map_err(|_| ControlError::ContractInvalid)?;
            if candidate.spec.class != contracts::authoring::EnvironmentClass::Work {
                return Err(ControlError::DecisionConflict);
            }
            let owner: Uuid = sqlx::query_scalar(
                "SELECT owner_actor_id FROM control.projects WHERE project_id=$1",
            )
            .bind(observed_project_id.as_uuid())
            .fetch_optional(&mut *transaction)
            .await
            .map_err(db)?
            .ok_or(ControlError::ProjectNotFound)?;
            if owner != actor_id.as_uuid() {
                return Err(ControlError::ProjectGovernanceDenied);
            }
        }
        let expected_schema = match kind.as_str() {
            "environment" => self.config.environment_schema_sha256,
            "evaluation" => self.config.evaluation_schema_sha256,
            _ => return Err(ControlError::PersistenceIdentityMismatch),
        };
        let active_policy_revision = sqlx::query_scalar::<_, i64>(
            "SELECT revision FROM control.project_llm_policies \
             WHERE project_id=$1 AND course_id IS NOT DISTINCT FROM $2 AND superseded_at IS NULL",
        )
        .bind(observed_project_id.as_uuid())
        .bind(course_id.map(CourseId::as_uuid))
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
            if candidate.id != candidate_id
                || candidate.project_id != observed_project_id
                || candidate.course_id != course_id
                || candidate.revision != observed_revision
                || candidate.policy_revision != observed_policy
            {
                return Err(ControlError::PersistenceIdentityMismatch);
            }
        }
        let event_payload = json!({
            "candidateId":candidate_id,"approvalId":approval.id,
            "revision":approval.candidate_revision,"decision":approval.decision
        });
        append_project_sse(
            &mut transaction,
            observed_project_id,
            &format!("{kind}_candidate.decision.v1"),
            candidate_id.as_uuid(),
            approval.candidate_revision,
            event_payload,
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

    /// Completes the single teacher approval for one immutable project package.
    ///
    /// All inputs are read from Control-owned projections while the project lock is held. The
    /// approval, project event, and durable outbox publication intent are committed together;
    /// this method never waits for Environment or Evaluation to acknowledge the event.
    #[allow(clippy::too_many_arguments)]
    pub async fn complete_authoring_approval(
        &self,
        project_id: ProjectId,
        request: &CompleteAuthoringApprovalRequest,
        actor_id: ActorId,
        idempotency_key: &IdempotencyKey,
        now: UtcTimestamp,
        trace_id: &str,
    ) -> Result<AuthoringApproval, ControlError> {
        if request.project_id != project_id
            || request.reason.trim().is_empty()
            || request.reason.chars().count() > 500
        {
            return Err(ControlError::ContractInvalid);
        }
        request
            .image_artifact
            .validate()
            .map_err(|_| ControlError::ReleaseEvidenceInvalid)?;
        validate_trace_id(trace_id)?;
        let request_hash = canonical_hash(&json!({
            "projectId": project_id,
            "request": request,
            "actorId": actor_id,
        }))?;
        let mut transaction = self.pool.begin().await.map_err(db)?;
        advisory_project_lock(&mut transaction, project_id).await?;
        match IdempotencyStore::reserve(
            &mut transaction,
            Domain::Control,
            COMPLETE_AUTHORING_APPROVAL,
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

        let project = project_from_tx(&mut transaction, project_id).await?;
        if project.state == ProjectState::Archived {
            return Err(ControlError::ProjectArchived);
        }
        if request.course_id != project.course_id {
            return Err(ControlError::ProjectMismatch);
        }

        let package_row = sqlx::query(
            "SELECT revision,contract FROM control.problem_packages \
             WHERE package_id=$1 AND project_id=$2 AND course_id IS NOT DISTINCT FROM $3 FOR SHARE",
        )
        .bind(request.package_id.as_uuid())
        .bind(project_id.as_uuid())
        .bind(project.course_id.map(CourseId::as_uuid))
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?
        .ok_or(ControlError::NotFound)?;
        let package_revision = revision_from_i64(package_row.try_get("revision").map_err(db)?)?;
        if package_revision != request.package_revision {
            return Err(ControlError::RevisionConflict);
        }
        let package: ProblemPackage =
            serde_json::from_value(package_row.try_get("contract").map_err(db)?)
                .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        package
            .validate()
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        package
            .validate_ownership(project_id, project.course_id)
            .map_err(|_| ControlError::ProjectMismatch)?;
        if package.id != request.package_id || package.revision != package_revision {
            return Err(ControlError::PersistenceIdentityMismatch);
        }

        let policy_row = sqlx::query(
            "SELECT contract FROM control.project_llm_policies \
             WHERE project_id=$1 AND course_id IS NOT DISTINCT FROM $2 \
               AND superseded_at IS NULL FOR SHARE",
        )
        .bind(project_id.as_uuid())
        .bind(project.course_id.map(CourseId::as_uuid))
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?
        .ok_or(ControlError::PolicyNotFound)?;
        let policy: ProjectLlmEgressPolicy =
            serde_json::from_value(policy_row.try_get("contract").map_err(db)?)
                .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        policy
            .validate()
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        policy
            .validate_ownership(project_id, project.course_id)
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;

        let environment_row = sqlx::query(
            "SELECT candidate_kind,project_id,course_id,run_id,revision,content_sha256, \
                    policy_revision,schema_sha256,contract \
             FROM control.candidates \
             WHERE candidate_id=$1 AND project_id=$2 \
               AND course_id IS NOT DISTINCT FROM $3 AND candidate_kind='environment' \
               AND state='validated' FOR SHARE",
        )
        .bind(request.environment_candidate_id.as_uuid())
        .bind(project_id.as_uuid())
        .bind(project.course_id.map(CourseId::as_uuid))
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?
        .ok_or(ControlError::CandidateNotFound)?;
        let environment_revision =
            revision_from_i64(environment_row.try_get("revision").map_err(db)?)?;
        if environment_revision != request.environment_candidate_revision {
            return Err(ControlError::RevisionConflict);
        }
        let environment_project = ProjectId::from_str(
            &environment_row
                .try_get::<Uuid, _>("project_id")
                .map_err(db)?
                .to_string(),
        )
        .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        let environment_course = environment_row
            .try_get::<Option<Uuid>, _>("course_id")
            .map_err(db)?
            .map(|value| CourseId::from_str(&value.to_string()))
            .transpose()
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        let environment_run_id = environment_row
            .try_get::<Option<Uuid>, _>("run_id")
            .map_err(db)?
            .ok_or(ControlError::PersistenceIdentityMismatch)?;
        let environment_policy_revision =
            revision_from_i64(environment_row.try_get("policy_revision").map_err(db)?)?;
        let environment_schema: Sha256Digest = environment_row
            .try_get::<String, _>("schema_sha256")
            .map_err(db)?
            .parse()
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        let environment_content: Sha256Digest = environment_row
            .try_get::<String, _>("content_sha256")
            .map_err(db)?
            .parse()
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        let environment: EnvironmentCandidate =
            serde_json::from_value(environment_row.try_get("contract").map_err(db)?)
                .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        environment
            .validate()
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        if environment_project != project_id
            || environment_course != project.course_id
            || environment.id != request.environment_candidate_id
            || environment.revision != environment_revision
            || environment.policy_revision != environment_policy_revision
            || environment_schema != self.config.environment_schema_sha256
            || environment_content != canonical_hash(&environment.spec)?
        {
            return Err(ControlError::PersistenceIdentityMismatch);
        }
        if environment.spec.class != EnvironmentClass::Experiment {
            return Err(ControlError::DecisionConflict);
        }

        let evaluation_row = sqlx::query(
            "SELECT candidate_kind,project_id,course_id,run_id,revision,content_sha256, \
                    policy_revision,schema_sha256,contract \
             FROM control.candidates \
             WHERE candidate_id=$1 AND project_id=$2 \
               AND course_id IS NOT DISTINCT FROM $3 AND candidate_kind='evaluation' \
               AND state='validated' FOR SHARE",
        )
        .bind(request.evaluation_candidate_id.as_uuid())
        .bind(project_id.as_uuid())
        .bind(project.course_id.map(CourseId::as_uuid))
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?
        .ok_or(ControlError::CandidateNotFound)?;
        let evaluation_revision =
            revision_from_i64(evaluation_row.try_get("revision").map_err(db)?)?;
        if evaluation_revision != request.evaluation_candidate_revision {
            return Err(ControlError::RevisionConflict);
        }
        let evaluation_project = ProjectId::from_str(
            &evaluation_row
                .try_get::<Uuid, _>("project_id")
                .map_err(db)?
                .to_string(),
        )
        .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        let evaluation_course = evaluation_row
            .try_get::<Option<Uuid>, _>("course_id")
            .map_err(db)?
            .map(|value| CourseId::from_str(&value.to_string()))
            .transpose()
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        let evaluation_run_id = evaluation_row
            .try_get::<Option<Uuid>, _>("run_id")
            .map_err(db)?
            .ok_or(ControlError::PersistenceIdentityMismatch)?;
        let evaluation_policy_revision =
            revision_from_i64(evaluation_row.try_get("policy_revision").map_err(db)?)?;
        let evaluation_schema: Sha256Digest = evaluation_row
            .try_get::<String, _>("schema_sha256")
            .map_err(db)?
            .parse()
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        let evaluation_content: Sha256Digest = evaluation_row
            .try_get::<String, _>("content_sha256")
            .map_err(db)?
            .parse()
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        let evaluation: EvaluationCandidate =
            serde_json::from_value(evaluation_row.try_get("contract").map_err(db)?)
                .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        evaluation
            .validate()
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        if evaluation_project != project_id
            || evaluation_course != project.course_id
            || evaluation.id != request.evaluation_candidate_id
            || evaluation.revision != evaluation_revision
            || evaluation.policy_revision != evaluation_policy_revision
            || evaluation_schema != self.config.evaluation_schema_sha256
            || evaluation_content != canonical_hash(&evaluation.spec)?
        {
            return Err(ControlError::PersistenceIdentityMismatch);
        }
        if environment_run_id != evaluation_run_id {
            return Err(ControlError::CandidateMissing);
        }

        let run_id = contracts::AgentRunId::from_str(&environment_run_id.to_string())
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        let run_row = sqlx::query(
            "SELECT project_id,course_id,revision,state,contract \
             FROM control.agent_run_projections \
             WHERE run_id=$1 AND project_id=$2 AND course_id IS NOT DISTINCT FROM $3 FOR SHARE",
        )
        .bind(run_id.as_uuid())
        .bind(project_id.as_uuid())
        .bind(project.course_id.map(CourseId::as_uuid))
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?
        .ok_or(ControlError::CandidateMissing)?;
        let run: contracts::authoring::AgentRun =
            serde_json::from_value(run_row.try_get("contract").map_err(db)?)
                .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        run.validate()
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        if run.id != run_id
            || run.project_id != project_id
            || run.course_id != project.course_id
            || run.package_id != package.id
            || run.policy_id != policy.id
            || run.policy_revision != policy.revision
            || run.state != AgentRunState::Succeeded
        {
            return Err(ControlError::CandidateMissing);
        }
        environment
            .validate_against_run(&run)
            .map_err(|_| ControlError::CandidateMissing)?;
        evaluation
            .validate_against_run(&run)
            .map_err(|_| ControlError::CandidateMissing)?;
        let environment_track = run
            .tracks
            .iter()
            .find(|track| track.kind == AgentTrackKind::Environment)
            .and_then(|track| track.candidate_id);
        let evaluation_track = run
            .tracks
            .iter()
            .find(|track| track.kind == AgentTrackKind::Evaluation)
            .and_then(|track| track.candidate_id);
        if environment_track != Some(environment.id) || evaluation_track != Some(evaluation.id) {
            return Err(ControlError::CandidateMissing);
        }

        validate_authoring_artifact(
            &mut transaction,
            project_id,
            project.course_id,
            &environment,
            &request.image_artifact,
            &self.config,
        )
        .await?;

        let next_revision = sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(MAX(revision),0)+1 FROM control.authoring_approvals WHERE project_id=$1",
        )
        .bind(project_id.as_uuid())
        .fetch_one(&mut *transaction)
        .await
        .map_err(db)?;
        let approval = AuthoringApproval {
            id: ApprovalId::new(),
            project_id,
            course_id: project.course_id,
            revision: Revision::new(
                u64::try_from(next_revision).map_err(|_| ControlError::ContractInvalid)?,
            )
            .map_err(|_| ControlError::ContractInvalid)?,
            package_id: package.id,
            package_revision: package.revision,
            environment_candidate_id: environment.id,
            environment_candidate_revision: environment.revision,
            evaluation_candidate_id: evaluation.id,
            evaluation_candidate_revision: evaluation.revision,
            evaluation_runtime_identity: self.config.evaluation_runtime.identity()?,
            image_artifact: request.image_artifact.clone(),
            actor_id,
            reason: request.reason.trim().to_owned(),
            approved_at: now,
        };
        approval
            .validate()
            .map_err(|_| ControlError::ContractInvalid)?;
        let approval_contract =
            serde_json::to_value(&approval).map_err(|_| ControlError::ContractInvalid)?;
        sqlx::query(
            "INSERT INTO control.authoring_approvals \
             (approval_id,project_id,course_id,revision,package_id,package_revision, \
              environment_candidate_id,environment_candidate_revision,evaluation_candidate_id, \
              evaluation_candidate_revision,image_artifact_id,actor_id,reason,contract,approved_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)",
        )
        .bind(approval.id.as_uuid())
        .bind(project_id.as_uuid())
        .bind(project.course_id.map(CourseId::as_uuid))
        .bind(i64_revision(approval.revision)?)
        .bind(approval.package_id.as_uuid())
        .bind(i64_revision(approval.package_revision)?)
        .bind(approval.environment_candidate_id.as_uuid())
        .bind(i64_revision(approval.environment_candidate_revision)?)
        .bind(approval.evaluation_candidate_id.as_uuid())
        .bind(i64_revision(approval.evaluation_candidate_revision)?)
        .bind(approval.image_artifact.id().as_uuid())
        .bind(actor_id.as_uuid())
        .bind(&approval.reason)
        .bind(&approval_contract)
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

        let publication = AuthoringApprovalPublicationStatus {
            approval: approval.clone(),
            status: AuthoringPublicationState::Pending,
            environment_release_id: None,
            evaluation_release_id: None,
            evaluation_release_revision: None,
            diagnostic_code: None,
            updated_at: now,
            revision: Revision::new(1).map_err(|_| ControlError::ContractInvalid)?,
        };
        let publication_contract = publication_contract(&publication)?;
        sqlx::query(
            "INSERT INTO control.authoring_approval_publications
             (approval_id,project_id,course_id,state,environment_release_id,evaluation_release_id,
              evaluation_release_revision,diagnostic_code,updated_at,revision,contract)
             VALUES ($1,$2,$3,'pending',NULL,NULL,NULL,NULL,$4,$5,$6)",
        )
        .bind(approval.id.as_uuid())
        .bind(project_id.as_uuid())
        .bind(project.course_id.map(CourseId::as_uuid))
        .bind(now.get())
        .bind(i64_revision(publication.revision)?)
        .bind(&publication_contract)
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
        let event_contract = event_contract(AUTHORING_APPROVAL_SUBJECT)?;
        let event = CloudEvent {
            specversion: SPEC_VERSION.to_owned(),
            id: event_id,
            source: event_contract.source().to_owned(),
            event_type: AUTHORING_APPROVAL_SUBJECT.to_owned(),
            subject: AUTHORING_APPROVAL_SUBJECT.to_owned(),
            time: now,
            datacontenttype: "application/json".to_owned(),
            dataschema: event_contract.data_schema(),
            project_id,
            course_id: project.course_id,
            aggregate_revision: approval.revision,
            aggregate_sequence: Sequence(1),
            trace_id: trace_id.to_owned(),
            data: AuthoringApprovalCompleted {
                approval: approval.clone(),
            },
        };
        event
            .data
            .validate()
            .map_err(|_| ControlError::ContractInvalid)?;
        event
            .validate(event_contract)
            .map_err(|_| ControlError::ContractInvalid)?;
        let event_payload =
            serde_json::to_value(&event).map_err(|_| ControlError::ContractInvalid)?;
        OutboxStore::enqueue(
            &mut transaction,
            Domain::Control,
            event_id.as_uuid(),
            AUTHORING_APPROVAL_SUBJECT,
            AUTHORING_APPROVAL_SUBJECT,
            approval.id.as_uuid(),
            1,
            &event_payload,
            canonical_hash(&event_payload)?,
        )
        .await
        .map_err(|_| ControlError::PersistenceFailed)?;
        append_project_sse(
            &mut transaction,
            project_id,
            AUTHORING_APPROVAL_SUBJECT,
            approval.id.as_uuid(),
            approval.revision,
            json!({
                "approvalId": approval.id,
                "revision": approval.revision,
                "packageId": approval.package_id,
                "environmentCandidateId": approval.environment_candidate_id,
                "evaluationCandidateId": approval.evaluation_candidate_id,
                "imageArtifactId": approval.image_artifact.id(),
            }),
        )
        .await?;
        IdempotencyStore::complete(
            &mut transaction,
            Domain::Control,
            COMPLETE_AUTHORING_APPROVAL,
            idempotency_key.as_str(),
            &approval_contract,
        )
        .await
        .map_err(|_| ControlError::PersistenceFailed)?;
        transaction.commit().await.map_err(db)?;
        Ok(approval)
    }

    /// Reads the durable downstream publication projection for one project approval.
    pub async fn authoring_approval_publication_status(
        &self,
        project_id: ProjectId,
        approval_id: ApprovalId,
    ) -> Result<AuthoringApprovalPublicationStatus, ControlError> {
        let row = sqlx::query(
            "SELECT state,project_id,course_id,environment_release_id,evaluation_release_id,
                    evaluation_release_revision,contract
             FROM control.authoring_approval_publications
             WHERE approval_id=$1 AND project_id=$2",
        )
        .bind(approval_id.as_uuid())
        .bind(project_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?
        .ok_or(ControlError::NotFound)?;
        let status = authoring_publication_status_from_row(&row)?;
        status
            .validate_ownership(project_id, status.approval.course_id)
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        if status.approval.id != approval_id {
            return Err(ControlError::PersistenceIdentityMismatch);
        }
        Ok(status)
    }

    /// Claims one durable authoring publication trigger before doing downstream work.
    ///
    /// The claim transaction is deliberately short.  It only fences the mutable projection;
    /// Environment release construction and the Evaluation HTTP request happen after the
    /// transaction has committed.
    pub async fn claim_authoring_publication(
        &self,
        approval: &AuthoringApproval,
        now: UtcTimestamp,
    ) -> Result<AuthoringPublicationClaim, ControlError> {
        approval
            .validate()
            .map_err(|_| ControlError::ContractInvalid)?;
        let mut transaction = self.pool.begin().await.map_err(db)?;
        let row = sqlx::query(
            "SELECT state,project_id,course_id,environment_release_id,evaluation_release_id,
                    evaluation_release_revision,contract
             FROM control.authoring_approval_publications
             WHERE approval_id=$1 AND project_id=$2 FOR UPDATE",
        )
        .bind(approval.id.as_uuid())
        .bind(approval.project_id.as_uuid())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?
        .ok_or(ControlError::NotFound)?;
        let current = authoring_publication_status_from_row(&row)?;
        if current.approval != *approval
            || current.approval.project_id != approval.project_id
            || current.approval.course_id != approval.course_id
        {
            return Err(ControlError::PersistenceIdentityMismatch);
        }
        if current.status == AuthoringPublicationState::Ready {
            transaction.commit().await.map_err(db)?;
            return Ok(AuthoringPublicationClaim::AlreadyReady(current));
        }
        let updated = AuthoringApprovalPublicationStatus {
            approval: current.approval,
            status: AuthoringPublicationState::Publishing,
            environment_release_id: current.environment_release_id,
            evaluation_release_id: current.evaluation_release_id,
            evaluation_release_revision: current.evaluation_release_revision,
            diagnostic_code: None,
            updated_at: now,
            revision: next_revision(current.revision)?,
        };
        let contract = publication_contract(&updated)?;
        let result = sqlx::query(
            "UPDATE control.authoring_approval_publications
             SET state='publishing',diagnostic_code=NULL,updated_at=$3,revision=$4,contract=$5
             WHERE approval_id=$1 AND project_id=$2 AND revision=$6",
        )
        .bind(approval.id.as_uuid())
        .bind(approval.project_id.as_uuid())
        .bind(now.get())
        .bind(i64_revision(updated.revision)?)
        .bind(&contract)
        .bind(i64_revision(current.revision)?)
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        if result.rows_affected() != 1 {
            return Err(ControlError::OperationLeaseLost);
        }
        transaction.commit().await.map_err(db)?;
        Ok(AuthoringPublicationClaim::Claimed(updated))
    }

    /// Builds the exact Evaluation command for an authoring approval.
    ///
    /// This method performs only bounded Control reads.  The caller must invoke the returned
    /// command after this method has returned so no database transaction spans the downstream
    /// request.
    pub async fn prepare_authoring_evaluation_release(
        &self,
        approval: &AuthoringApproval,
    ) -> Result<InternalPublishEvaluationReleaseRequest, ControlError> {
        approval
            .validate()
            .map_err(|_| ControlError::ContractInvalid)?;
        let candidate = self
            .project_evaluation_candidate(approval.project_id, approval.evaluation_candidate_id)
            .await?;
        if candidate.course_id != approval.course_id
            || candidate.revision != approval.evaluation_candidate_revision
        {
            return Err(ControlError::ReleaseCandidateMismatch);
        }
        let package = self
            .project_package(approval.project_id, approval.package_id)
            .await?;
        if package.course_id != approval.course_id || package.revision != approval.package_revision
        {
            return Err(ControlError::RevisionConflict);
        }
        package
            .validate()
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        package
            .validate_ownership(approval.project_id, approval.course_id)
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        let object_locators = self
            .project_package_object_locators(approval.project_id, &package)
            .await?;
        let command = InternalPublishEvaluationReleaseRequest {
            project_id: approval.project_id,
            course_id: approval.course_id,
            candidate_id: candidate.id,
            candidate_revision: candidate.revision,
            approval_id: approval.id,
            approval_revision: approval.revision,
            evaluation_spec: candidate.spec,
            execution_binding: EvaluationExecutionBinding {
                package,
                object_locators,
            },
            runtime_identity: approval.evaluation_runtime_identity.clone(),
            published_by: approval.actor_id,
        };
        command
            .validate()
            .map_err(|_| ControlError::ContractInvalid)?;
        Ok(command)
    }

    /// Publishes the Environment side of an authoring approval in the Control transaction.
    ///
    /// A stable synthetic `CandidateApproval` carries the authoring approval ID.  Looking up that
    /// ID before allocating a version makes a crash between the insert and the status update
    /// replay-safe while the project advisory lock prevents concurrent version allocation.
    pub async fn publish_authoring_environment_release(
        &self,
        approval: &AuthoringApproval,
        now: UtcTimestamp,
        trace_id: &str,
    ) -> Result<EnvironmentTemplateRelease, ControlError> {
        approval
            .validate()
            .map_err(|_| ControlError::ContractInvalid)?;
        validate_trace_id(trace_id)?;
        let mut transaction = self.pool.begin().await.map_err(db)?;
        advisory_project_lock(&mut transaction, approval.project_id).await?;

        let approval_row = sqlx::query(
            "SELECT contract FROM control.authoring_approvals
             WHERE approval_id=$1 AND project_id=$2
               AND course_id IS NOT DISTINCT FROM $3 FOR SHARE",
        )
        .bind(approval.id.as_uuid())
        .bind(approval.project_id.as_uuid())
        .bind(approval.course_id.map(CourseId::as_uuid))
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?
        .ok_or(ControlError::NotFound)?;
        let persisted_approval: AuthoringApproval =
            serde_json::from_value(approval_row.try_get("contract").map_err(db)?)
                .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        persisted_approval
            .validate()
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        if persisted_approval != *approval {
            return Err(ControlError::PersistenceIdentityMismatch);
        }

        let project = project_from_tx(&mut transaction, approval.project_id).await?;
        if project.state == ProjectState::Archived || project.course_id != approval.course_id {
            return Err(ControlError::ProjectMismatch);
        }

        let package_row = sqlx::query(
            "SELECT revision,contract FROM control.problem_packages
             WHERE package_id=$1 AND project_id=$2
               AND course_id IS NOT DISTINCT FROM $3 FOR SHARE",
        )
        .bind(approval.package_id.as_uuid())
        .bind(approval.project_id.as_uuid())
        .bind(approval.course_id.map(CourseId::as_uuid))
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?
        .ok_or(ControlError::NotFound)?;
        let package_revision = revision_from_i64(package_row.try_get("revision").map_err(db)?)?;
        if package_revision != approval.package_revision {
            return Err(ControlError::RevisionConflict);
        }
        let package: ProblemPackage =
            serde_json::from_value(package_row.try_get("contract").map_err(db)?)
                .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        package
            .validate()
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        package
            .validate_ownership(approval.project_id, approval.course_id)
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;

        let environment_row = sqlx::query(
            "SELECT project_id,course_id,run_id,revision,content_sha256,policy_revision,
                    schema_sha256,contract
             FROM control.candidates
             WHERE candidate_id=$1 AND project_id=$2
               AND course_id IS NOT DISTINCT FROM $3 AND candidate_kind='environment'
               AND state='validated' FOR SHARE",
        )
        .bind(approval.environment_candidate_id.as_uuid())
        .bind(approval.project_id.as_uuid())
        .bind(approval.course_id.map(CourseId::as_uuid))
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?
        .ok_or(ControlError::CandidateNotFound)?;
        let environment_revision =
            revision_from_i64(environment_row.try_get("revision").map_err(db)?)?;
        if environment_revision != approval.environment_candidate_revision {
            return Err(ControlError::RevisionConflict);
        }
        let environment: EnvironmentCandidate =
            serde_json::from_value(environment_row.try_get("contract").map_err(db)?)
                .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        environment
            .validate()
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        if environment.id != approval.environment_candidate_id
            || environment.project_id != approval.project_id
            || environment.course_id != approval.course_id
            || environment.revision != environment_revision
            || environment.spec.class != EnvironmentClass::Experiment
            || environment.spec.runtime.kind() != approval.image_artifact.runtime_kind()
        {
            return Err(ControlError::ReleaseCandidateMismatch);
        }
        let environment_policy_revision =
            revision_from_i64(environment_row.try_get("policy_revision").map_err(db)?)?;
        let environment_schema: Sha256Digest = environment_row
            .try_get::<String, _>("schema_sha256")
            .map_err(db)?
            .parse()
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        let environment_content: Sha256Digest = environment_row
            .try_get::<String, _>("content_sha256")
            .map_err(db)?
            .parse()
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        if environment_schema != self.config.environment_schema_sha256
            || environment_content != canonical_hash(&environment.spec)?
        {
            return Err(ControlError::PersistenceIdentityMismatch);
        }
        let run_id = environment.run_id;
        let run_row = sqlx::query(
            "SELECT contract FROM control.agent_run_projections
             WHERE run_id=$1 AND project_id=$2
               AND course_id IS NOT DISTINCT FROM $3 FOR SHARE",
        )
        .bind(run_id.as_uuid())
        .bind(approval.project_id.as_uuid())
        .bind(approval.course_id.map(CourseId::as_uuid))
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?
        .ok_or(ControlError::CandidateMissing)?;
        let run: contracts::authoring::AgentRun =
            serde_json::from_value(run_row.try_get("contract").map_err(db)?)
                .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        run.validate()
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        if run.id != run_id
            || run.project_id != approval.project_id
            || run.course_id != approval.course_id
            || run.package_id != package.id
            || run.state != AgentRunState::Succeeded
        {
            return Err(ControlError::CandidateMissing);
        }
        environment
            .validate_against_run(&run)
            .map_err(|_| ControlError::CandidateMissing)?;
        let evaluation_row = sqlx::query(
            "SELECT run_id,revision,contract FROM control.candidates
             WHERE candidate_id=$1 AND project_id=$2
               AND course_id IS NOT DISTINCT FROM $3 AND candidate_kind='evaluation'
               AND state='validated' FOR SHARE",
        )
        .bind(approval.evaluation_candidate_id.as_uuid())
        .bind(approval.project_id.as_uuid())
        .bind(approval.course_id.map(CourseId::as_uuid))
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?
        .ok_or(ControlError::CandidateNotFound)?;
        let evaluation_revision =
            revision_from_i64(evaluation_row.try_get("revision").map_err(db)?)?;
        let evaluation: EvaluationCandidate =
            serde_json::from_value(evaluation_row.try_get("contract").map_err(db)?)
                .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        evaluation
            .validate()
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        if evaluation.id != approval.evaluation_candidate_id
            || evaluation.revision != evaluation_revision
            || evaluation.revision != approval.evaluation_candidate_revision
            || evaluation.run_id != run.id
            || evaluation.course_id != approval.course_id
        {
            return Err(ControlError::CandidateMissing);
        }
        evaluation
            .validate_against_run(&run)
            .map_err(|_| ControlError::CandidateMissing)?;
        validate_authoring_artifact(
            &mut transaction,
            approval.project_id,
            approval.course_id,
            &environment,
            &approval.image_artifact,
            &self.config,
        )
        .await?;

        let synthetic_approval = CandidateApproval {
            id: approval.id,
            candidate_id: environment.id,
            candidate_revision: environment.revision,
            policy_revision: environment_policy_revision,
            trust_revision: self.config.trust_revision,
            actor_id: approval.actor_id,
            decision: CandidateDecision::Approved,
            reason: approval.reason.clone(),
            decided_at: approval.approved_at,
        };
        let existing = sqlx::query(
            "SELECT contract FROM control.environment_template_releases
             WHERE project_id=$1 AND course_id IS NOT DISTINCT FROM $2
               AND contract->'approval'->>'id'=$3 FOR UPDATE",
        )
        .bind(approval.project_id.as_uuid())
        .bind(approval.course_id.map(CourseId::as_uuid))
        .bind(approval.id.to_string())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?;
        if let Some(row) = existing {
            let release: EnvironmentTemplateRelease =
                serde_json::from_value(row.try_get("contract").map_err(db)?)
                    .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
            release
                .validate()
                .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
            release
                .validate_ownership(approval.project_id, approval.course_id)
                .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
            release
                .validate_against_candidate(&environment)
                .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
            if release.approval != synthetic_approval || release.artifact != approval.image_artifact
            {
                return Err(ControlError::PersistenceIdentityMismatch);
            }
            transaction.commit().await.map_err(db)?;
            return Ok(release);
        }

        let next_version = sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(MAX(version),0)+1
             FROM control.environment_template_releases WHERE project_id=$1",
        )
        .bind(approval.project_id.as_uuid())
        .fetch_one(&mut *transaction)
        .await
        .map_err(db)?;
        let release = EnvironmentTemplateRelease {
            id: ReleaseId::new(),
            project_id: approval.project_id,
            course_id: approval.course_id,
            version: u64::try_from(next_version)
                .map_err(|_| ControlError::PersistenceIdentityMismatch)?,
            candidate_id: environment.id,
            agent_run_id: environment.run_id,
            candidate_revision: environment.revision,
            runtime_kind: environment.spec.runtime.kind(),
            approval: synthetic_approval,
            artifact: approval.image_artifact.clone(),
            published_by: approval.actor_id,
            published_at: now,
        };
        release
            .validate()
            .map_err(|_| ControlError::ReleaseEvidenceInvalid)?;
        let contract = serde_json::to_value(&release).map_err(|_| ControlError::ContractInvalid)?;
        let spec_sha256 = canonical_hash(&environment.spec)?;
        sqlx::query(
            "INSERT INTO control.environment_template_releases
             (release_id,project_id,course_id,version,environment_candidate_id,candidate_revision,
              spec_sha256,image_artifact_id,contract,published_at)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
        )
        .bind(release.id.as_uuid())
        .bind(release.project_id.as_uuid())
        .bind(release.course_id.map(CourseId::as_uuid))
        .bind(next_version)
        .bind(release.candidate_id.as_uuid())
        .bind(i64_revision(release.candidate_revision)?)
        .bind(spec_sha256.to_string())
        .bind(release.artifact.id().as_uuid())
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
        let event_id = EventId::new();
        let event_contract = event_contract(RELEASE_SUBJECT)?;
        let event = CloudEvent {
            specversion: SPEC_VERSION.to_owned(),
            id: event_id,
            source: event_contract.source().to_owned(),
            event_type: RELEASE_SUBJECT.to_owned(),
            subject: RELEASE_SUBJECT.to_owned(),
            time: now,
            datacontenttype: "application/json".to_owned(),
            dataschema: event_contract.data_schema(),
            project_id: release.project_id,
            course_id: release.course_id,
            aggregate_revision: Revision::new(release.version)
                .map_err(|_| ControlError::ContractInvalid)?,
            aggregate_sequence: Sequence(1),
            trace_id: trace_id.to_owned(),
            data: ReleasePublished {
                release: release.clone(),
                environment_spec: environment.spec,
            },
        };
        event
            .data
            .validate()
            .map_err(|_| ControlError::ContractInvalid)?;
        event
            .validate(event_contract)
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
        append_project_sse(
            &mut transaction,
            approval.project_id,
            RELEASE_SUBJECT,
            release.id.as_uuid(),
            Revision::new(release.version).map_err(|_| ControlError::ContractInvalid)?,
            json!({"releaseId":release.id,"version":release.version,"authoringApprovalId":approval.id}),
        )
        .await?;
        if let Some(course_id) = approval.course_id {
            append_sse(
                &mut transaction,
                course_id,
                RELEASE_SUBJECT,
                release.id.as_uuid(),
                Revision::new(release.version).map_err(|_| ControlError::ContractInvalid)?,
                json!({"releaseId":release.id,"version":release.version}),
            )
            .await?;
        }
        transaction.commit().await.map_err(db)?;
        Ok(release)
    }

    /// Records the terminal downstream publication result using a compare-and-swap revision.
    pub async fn complete_authoring_publication(
        &self,
        approval_id: ApprovalId,
        project_id: ProjectId,
        environment_release: &EnvironmentTemplateRelease,
        evaluation_release: &contracts::evaluation::EvaluationRelease,
        now: UtcTimestamp,
    ) -> Result<(), ControlError> {
        environment_release
            .validate()
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        evaluation_release
            .validate()
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        let mut transaction = self.pool.begin().await.map_err(db)?;
        let row = sqlx::query(
            "SELECT state,project_id,course_id,environment_release_id,evaluation_release_id,
                    evaluation_release_revision,contract
             FROM control.authoring_approval_publications
             WHERE approval_id=$1 AND project_id=$2 FOR UPDATE",
        )
        .bind(approval_id.as_uuid())
        .bind(project_id.as_uuid())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?
        .ok_or(ControlError::NotFound)?;
        let current = authoring_publication_status_from_row(&row)?;
        if current.approval.id != approval_id {
            return Err(ControlError::PersistenceIdentityMismatch);
        }
        environment_release
            .validate_ownership(project_id, current.approval.course_id)
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        evaluation_release
            .validate_ownership(project_id, current.approval.course_id)
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        if environment_release.approval.id != approval_id
            || environment_release.candidate_id != current.approval.environment_candidate_id
            || environment_release.candidate_revision
                != current.approval.environment_candidate_revision
            || evaluation_release.approval_id != approval_id
            || evaluation_release.approval_revision != current.approval.revision
            || evaluation_release.candidate_id != current.approval.evaluation_candidate_id
            || evaluation_release.candidate_revision
                != current.approval.evaluation_candidate_revision
            || evaluation_release.state != contracts::evaluation::EvaluationReleaseState::Active
        {
            return Err(ControlError::PersistenceIdentityMismatch);
        }
        if current.status == AuthoringPublicationState::Ready {
            if current.environment_release_id == Some(environment_release.id)
                && current.evaluation_release_id == Some(evaluation_release.id)
                && current.evaluation_release_revision == Some(evaluation_release.revision)
            {
                transaction.commit().await.map_err(db)?;
                return Ok(());
            }
            return Err(ControlError::PersistenceIdentityMismatch);
        }
        if current.status != AuthoringPublicationState::Publishing {
            return Err(ControlError::OperationLeaseLost);
        }
        let updated = AuthoringApprovalPublicationStatus {
            approval: current.approval,
            status: AuthoringPublicationState::Ready,
            environment_release_id: Some(environment_release.id),
            evaluation_release_id: Some(evaluation_release.id),
            evaluation_release_revision: Some(evaluation_release.revision),
            diagnostic_code: None,
            updated_at: now,
            revision: next_revision(current.revision)?,
        };
        let contract = publication_contract(&updated)?;
        let result = sqlx::query(
            "UPDATE control.authoring_approval_publications
             SET state='ready',environment_release_id=$3,evaluation_release_id=$4,
                 evaluation_release_revision=$5,diagnostic_code=NULL,updated_at=$6,
                 revision=$7,contract=$8
             WHERE approval_id=$1 AND project_id=$2 AND revision=$9
               AND state='publishing'",
        )
        .bind(approval_id.as_uuid())
        .bind(project_id.as_uuid())
        .bind(environment_release.id.as_uuid())
        .bind(evaluation_release.id.as_uuid())
        .bind(i64_revision(evaluation_release.revision)?)
        .bind(now.get())
        .bind(i64_revision(updated.revision)?)
        .bind(&contract)
        .bind(i64_revision(current.revision)?)
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        if result.rows_affected() != 1 {
            return Err(ControlError::OperationLeaseLost);
        }
        append_project_sse(
            &mut transaction,
            project_id,
            "authoring_approval.publication.ready.v1",
            approval_id.as_uuid(),
            updated.revision,
            json!({"approvalId":approval_id,"state":"ready","environmentReleaseId":environment_release.id,"evaluationReleaseId":evaluation_release.id}),
        )
        .await?;
        transaction.commit().await.map_err(db)?;
        Ok(())
    }

    /// Persists a terminal authoring publication failure using a compare-and-swap revision.
    pub async fn fail_authoring_publication(
        &self,
        approval_id: ApprovalId,
        project_id: ProjectId,
        diagnostic_code: DiagnosticCode,
        now: UtcTimestamp,
    ) -> Result<(), ControlError> {
        if diagnostic_code.as_str().trim().is_empty() {
            return Err(ControlError::ContractInvalid);
        }
        let mut transaction = self.pool.begin().await.map_err(db)?;
        let row = sqlx::query(
            "SELECT state,project_id,course_id,environment_release_id,evaluation_release_id,
                    evaluation_release_revision,contract
             FROM control.authoring_approval_publications
             WHERE approval_id=$1 AND project_id=$2 FOR UPDATE",
        )
        .bind(approval_id.as_uuid())
        .bind(project_id.as_uuid())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?
        .ok_or(ControlError::NotFound)?;
        let current = authoring_publication_status_from_row(&row)?;
        if current.approval.id != approval_id {
            return Err(ControlError::PersistenceIdentityMismatch);
        }
        if current.status == AuthoringPublicationState::Ready {
            transaction.commit().await.map_err(db)?;
            return Ok(());
        }
        let updated = AuthoringApprovalPublicationStatus {
            approval: current.approval,
            status: AuthoringPublicationState::Failed,
            environment_release_id: current.environment_release_id,
            evaluation_release_id: current.evaluation_release_id,
            evaluation_release_revision: current.evaluation_release_revision,
            diagnostic_code: Some(diagnostic_code),
            updated_at: now,
            revision: next_revision(current.revision)?,
        };
        let contract = publication_contract(&updated)?;
        let result = sqlx::query(
            "UPDATE control.authoring_approval_publications
             SET state='failed',diagnostic_code=$3,updated_at=$4,revision=$5,contract=$6
             WHERE approval_id=$1 AND project_id=$2 AND revision=$7
               AND state <> 'ready'",
        )
        .bind(approval_id.as_uuid())
        .bind(project_id.as_uuid())
        .bind(updated.diagnostic_code.as_ref().map(DiagnosticCode::as_str))
        .bind(now.get())
        .bind(i64_revision(updated.revision)?)
        .bind(&contract)
        .bind(i64_revision(current.revision)?)
        .execute(&mut *transaction)
        .await
        .map_err(db)?;
        if result.rows_affected() != 1 {
            return Err(ControlError::OperationLeaseLost);
        }
        append_project_sse(
            &mut transaction,
            project_id,
            "authoring_approval.publication.failed.v1",
            approval_id.as_uuid(),
            updated.revision,
            json!({"approvalId":approval_id,"state":"failed","diagnosticCode":updated.diagnostic_code}),
        )
        .await?;
        transaction.commit().await.map_err(db)?;
        Ok(())
    }

    /// Returns an Evaluation admission binding only after both downstream releases are ready.
    pub async fn authoring_publication_admission(
        &self,
        approval_id: ApprovalId,
        query: &AuthoringPublicationAdmissionQuery,
    ) -> Result<AuthoringPublicationAdmissionBinding, ControlError> {
        let row = sqlx::query(
            "SELECT state,project_id,course_id,environment_release_id,evaluation_release_id,
                    evaluation_release_revision,contract
             FROM control.authoring_approval_publications
             WHERE approval_id=$1 AND project_id=$2
               AND course_id IS NOT DISTINCT FROM $3",
        )
        .bind(approval_id.as_uuid())
        .bind(query.project_id.as_uuid())
        .bind(query.course_id.map(CourseId::as_uuid))
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?
        .ok_or(ControlError::NotFound)?;
        let status = authoring_publication_status_from_row(&row)?;
        if status.approval.id != approval_id
            || status.approval.revision != query.approval_revision
            || status.approval.project_id != query.project_id
            || status.approval.course_id != query.course_id
        {
            return Err(ControlError::ProjectMismatch);
        }
        match status.status {
            AuthoringPublicationState::Pending | AuthoringPublicationState::Publishing => {
                return Err(ControlError::OperationInProgress);
            }
            AuthoringPublicationState::Failed => return Err(ControlError::ReleaseEvidenceInvalid),
            AuthoringPublicationState::Ready => {}
        }
        let environment_release_id = status
            .environment_release_id
            .ok_or(ControlError::PersistenceIdentityMismatch)?;
        let evaluation_release_id = status
            .evaluation_release_id
            .ok_or(ControlError::PersistenceIdentityMismatch)?;
        let evaluation_release_revision = status
            .evaluation_release_revision
            .ok_or(ControlError::PersistenceIdentityMismatch)?;
        if query.evaluation_release_id != evaluation_release_id {
            return Err(ControlError::ReleaseCandidateMismatch);
        }
        let release_row = sqlx::query(
            "SELECT project_id,course_id,version,contract
             FROM control.environment_template_releases
             WHERE release_id=$1 AND project_id=$2
               AND course_id IS NOT DISTINCT FROM $3",
        )
        .bind(environment_release_id.as_uuid())
        .bind(query.project_id.as_uuid())
        .bind(query.course_id.map(CourseId::as_uuid))
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?
        .ok_or(ControlError::ReleaseNotFound)?;
        let environment_release: EnvironmentTemplateRelease =
            serde_json::from_value(release_row.try_get("contract").map_err(db)?)
                .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        environment_release
            .validate()
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        if environment_release.id != environment_release_id
            || environment_release.project_id != query.project_id
            || environment_release.course_id != query.course_id
            || environment_release.approval.id != approval_id
            || environment_release.candidate_id != status.approval.environment_candidate_id
            || environment_release.candidate_revision
                != status.approval.environment_candidate_revision
        {
            return Err(ControlError::PersistenceIdentityMismatch);
        }
        Ok(AuthoringPublicationAdmissionBinding {
            approval_id,
            approval_revision: status.approval.revision,
            project_id: query.project_id,
            course_id: query.course_id,
            environment_release_id,
            environment_release_version: environment_release.version,
            evaluation_release_id,
            evaluation_release_revision,
        })
    }

    /// Publishes an immutable Work release from authoritative project projections only.
    ///
    /// Experiment releases are published by the authoring-approval consumer after the complete
    /// approval has been durably recorded. This entry point therefore accepts only a validated
    /// Work candidate and never treats a course association as the ownership boundary.
    #[allow(
        clippy::too_many_arguments,
        reason = "release creation binds the project, actor, revision, event, and trace fences"
    )]
    pub async fn create_project_work_release(
        &self,
        project_id: ProjectId,
        request: &CreateEnvironmentTemplateReleaseRequest,
        actor_id: ActorId,
        platform_admin: bool,
        idempotency_key: &IdempotencyKey,
        now: UtcTimestamp,
        trace_id: &str,
    ) -> Result<EnvironmentTemplateRelease, ControlError> {
        if request.project_id != project_id {
            return Err(ControlError::ProjectMismatch);
        }
        validate_trace_id(trace_id)?;
        let request_hash = canonical_hash(&json!({
            "projectId":project_id,"request":request,"actorId":actor_id
        }))?;
        let mut transaction = self.pool.begin().await.map_err(db)?;
        advisory_project_lock(&mut transaction, project_id).await?;
        let project = project_from_tx(&mut transaction, project_id).await?;
        if project.state == ProjectState::Archived {
            return Err(ControlError::ProjectArchived);
        }
        if !platform_admin && project.owner_actor_id != actor_id {
            return Err(ControlError::ProjectGovernanceDenied);
        }
        if project.course_id != request.course_id {
            return Err(ControlError::ProjectMismatch);
        }
        let course_id = request.course_id;
        match IdempotencyStore::reserve(
            &mut transaction,
            Domain::Control,
            CREATE_WORK_RELEASE,
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
            "SELECT candidate_kind,project_id,course_id,revision,content_sha256,policy_revision,schema_sha256,contract FROM control.candidates \
             WHERE candidate_id=$1 AND project_id=$2 AND course_id IS NOT DISTINCT FROM $3 \
               AND candidate_kind='environment' AND state='validated' FOR SHARE",
        )
        .bind(request.candidate_id.as_uuid())
        .bind(project_id.as_uuid())
        .bind(course_id.map(CourseId::as_uuid))
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
        if environment_candidate.project_id != project_id
            || environment_candidate.course_id != course_id
            || environment_candidate.spec.class != EnvironmentClass::Work
            || environment_candidate.spec.runtime.kind() != request.runtime_kind
        {
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
        let active_policy = sqlx::query_scalar::<_, i64>(
            "SELECT revision FROM control.project_llm_policies
             WHERE project_id=$1 AND course_id IS NOT DISTINCT FROM $2 AND superseded_at IS NULL",
        )
        .bind(project_id.as_uuid())
        .bind(course_id.map(CourseId::as_uuid))
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?
        .ok_or(ControlError::PolicyNotFound)?;
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
                      WHERE builds.project_id=$1 AND builds.course_id IS NOT DISTINCT FROM $2 \
                        AND builds.candidate_id=$3 AND builds.candidate_revision=$4 \
                        AND builds.candidate_sha256=$5 AND builds.state='succeeded' FOR SHARE",
                )
                .bind(project_id.as_uuid())
                .bind(course_id.map(CourseId::as_uuid))
                .bind(request.candidate_id.as_uuid())
                .bind(
                    i64::try_from(request.candidate_revision.get())
                        .map_err(|_| ControlError::ReleaseCandidateMismatch)?,
                )
                .bind(candidate_sha256.to_string())
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
            "SELECT COALESCE(MAX(version),0)+1
             FROM control.environment_template_releases WHERE project_id=$1",
        )
        .bind(project_id.as_uuid())
        .fetch_one(&mut *transaction)
        .await
        .map_err(db)?;
        let release = EnvironmentTemplateRelease {
            id: ReleaseId::new(),
            project_id,
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
             (release_id,project_id,course_id,version,environment_candidate_id,candidate_revision, \
              spec_sha256,image_artifact_id,contract,published_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
        )
        .bind(release.id.as_uuid())
        .bind(release.project_id.as_uuid())
        .bind(course_id.map(CourseId::as_uuid))
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
            dataschema: event_contract(RELEASE_SUBJECT)?.data_schema(),
            project_id: release.project_id,
            course_id: release.course_id,
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
            .validate(event_contract(RELEASE_SUBJECT)?)
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
        append_project_sse(
            &mut transaction,
            project_id,
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
        if let Some(course_id) = course_id {
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
        }
        IdempotencyStore::complete(
            &mut transaction,
            Domain::Control,
            CREATE_WORK_RELEASE,
            idempotency_key.as_str(),
            &contract,
        )
        .await
        .map_err(|_| ControlError::PersistenceFailed)?;
        transaction.commit().await.map_err(db)?;
        Ok(release)
    }

    /// Appends a project-scoped withdrawal without changing the immutable release.
    #[allow(clippy::too_many_arguments)]
    pub async fn withdraw_project_release(
        &self,
        project_id: ProjectId,
        release_id: ReleaseId,
        expected_version: u64,
        actor_id: ActorId,
        platform_admin: bool,
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
            "projectId":project_id,"releaseId":release_id,"expectedVersion":expected_version,
            "actorId":actor_id,"reasonCode":reason_code
        }))?;
        let mut transaction = self.pool.begin().await.map_err(db)?;
        advisory_project_lock(&mut transaction, project_id).await?;
        let project = project_from_tx(&mut transaction, project_id).await?;
        if !platform_admin && project.owner_actor_id != actor_id {
            return Err(ControlError::ProjectGovernanceDenied);
        }
        match IdempotencyStore::reserve(
            &mut transaction,
            Domain::Control,
            WITHDRAW_PROJECT_RELEASE,
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
        let release_row = sqlx::query(
            "SELECT version,project_id,course_id,environment_candidate_id,candidate_revision,contract
             FROM control.environment_template_releases
             WHERE release_id=$1 AND project_id=$2 FOR SHARE",
        )
        .bind(release_id.as_uuid())
        .bind(project_id.as_uuid())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?
        .ok_or(ControlError::ReleaseNotFound)?;
        let version: i64 = release_row.try_get("version").map_err(db)?;
        if u64::try_from(version).ok() != Some(expected_version) {
            return Err(ControlError::RevisionConflict);
        }
        let release_project_id = ProjectId::from_str(
            &release_row
                .try_get::<Uuid, _>("project_id")
                .map_err(db)?
                .to_string(),
        )
        .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        if release_project_id != project_id {
            return Err(ControlError::PersistenceIdentityMismatch);
        }
        let release_course_id = release_row
            .try_get::<Option<Uuid>, _>("course_id")
            .map_err(db)?
            .map(|id| CourseId::from_str(&id.to_string()))
            .transpose()
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        if release_course_id != project.course_id {
            return Err(ControlError::PersistenceIdentityMismatch);
        }
        let release: EnvironmentTemplateRelease =
            serde_json::from_value(release_row.try_get("contract").map_err(db)?)
                .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        release
            .validate()
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        release
            .validate_ownership(project_id, release_course_id)
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        if release.id != release_id || release.version != expected_version {
            return Err(ControlError::PersistenceIdentityMismatch);
        }
        let candidate_id = release_row
            .try_get::<Uuid, _>("environment_candidate_id")
            .map_err(db)?;
        let candidate_revision = revision_from_i64(
            release_row
                .try_get::<i64, _>("candidate_revision")
                .map_err(db)?,
        )?;
        if release.candidate_id.as_uuid() != candidate_id
            || release.candidate_revision != candidate_revision
        {
            return Err(ControlError::PersistenceIdentityMismatch);
        }
        let candidate_class = sqlx::query_scalar::<_, String>(
            "SELECT contract->'spec'->>'class'
             FROM control.candidates
             WHERE candidate_id=$1 AND project_id=$2
               AND course_id IS NOT DISTINCT FROM $3
               AND candidate_kind='environment' AND revision=$4 AND state='validated'",
        )
        .bind(candidate_id)
        .bind(project_id.as_uuid())
        .bind(release_course_id.map(CourseId::as_uuid))
        .bind(i64_revision(candidate_revision)?)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(db)?
        .ok_or(ControlError::ReleaseCandidateMismatch)?;
        if candidate_class != "work" && candidate_class != "experiment" {
            return Err(ControlError::ReleaseCandidateMismatch);
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
             (release_id,project_id,course_id,release_version,actor_id,reason_code,withdrawn_at,contract) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
        )
        .bind(release_id.as_uuid())
        .bind(release_project_id.as_uuid())
        .bind(release_course_id.map(CourseId::as_uuid))
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
            dataschema: event_contract(WITHDRAWAL_SUBJECT)?.data_schema(),
            project_id: release_project_id,
            course_id: release_course_id,
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
            .validate(event_contract(WITHDRAWAL_SUBJECT)?)
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
        append_project_sse(
            &mut transaction,
            project_id,
            WITHDRAWAL_SUBJECT,
            release_id.as_uuid(),
            Revision::new(expected_version).map_err(|_| ControlError::ContractInvalid)?,
            json!({"releaseId":release_id,"version":expected_version,"reasonCode":reason_code}),
        )
        .await?;
        if let Some(course_id) = release_course_id {
            append_sse(
                &mut transaction,
                course_id,
                WITHDRAWAL_SUBJECT,
                release_id.as_uuid(),
                Revision::new(expected_version).map_err(|_| ControlError::ContractInvalid)?,
                json!({"releaseId":release_id,"version":expected_version,"reasonCode":reason_code}),
            )
            .await?;
        }
        IdempotencyStore::complete(
            &mut transaction,
            Domain::Control,
            WITHDRAW_PROJECT_RELEASE,
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

    /// Reads a bounded SSE page for one exact project scope.
    pub async fn project_sse_page(
        &self,
        project_id: ProjectId,
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
                "SELECT last_sequence FROM control.sse_project_cursors WHERE project_id=$1",
            )
            .bind(project_id.as_uuid())
            .fetch_optional(&self.pool)
            .await
            .map_err(db)?
            .unwrap_or(0);
            let cursor = i64::try_from(cursor).map_err(|_| ControlError::SseCursorGap)?;
            if cursor > last {
                return Err(ControlError::SseCursorGap);
            }
            let exists = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM control.sse_project_events \
                 WHERE project_id=$1 AND sequence=$2 AND created_at >= $3)",
            )
            .bind(project_id.as_uuid())
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
             FROM control.sse_project_events WHERE project_id=$1 AND sequence>$2 AND created_at >= $3 \
             ORDER BY sequence LIMIT $4",
        )
        .bind(project_id.as_uuid())
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
        let course_rows = sqlx::query("DELETE FROM control.sse_events WHERE created_at < $1")
            .bind(cutoff)
            .execute(&self.pool)
            .await
            .map_err(db)?
            .rows_affected();
        let project_rows =
            sqlx::query("DELETE FROM control.sse_project_events WHERE created_at < $1")
                .bind(cutoff)
                .execute(&self.pool)
                .await
                .map_err(db)?
                .rows_affected();
        Ok(course_rows + project_rows)
    }

    async fn reserve_completion(
        &self,
        project_id: Option<ProjectId>,
        course_id: Option<CourseId>,
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
                     SET completion_lease_token=$6,completion_lease_expires_at=date_trunc('milliseconds',clock_timestamp())+($7*interval '1 second'),updated_at=now() \
                     WHERE upload_id=$1 AND ($2::uuid IS NULL OR project_id=$2) \
                       AND course_id IS NOT DISTINCT FROM $3 AND state='completing' \
                       AND completion_idempotency_key=$4 AND completion_request_sha256=$5 \
                       AND completion_lease_expires_at<=date_trunc('milliseconds',clock_timestamp()) RETURNING upload_id",
                )
                .bind(upload_id.as_uuid())
                .bind(project_id.map(ProjectId::as_uuid))
                .bind(course_id.map(CourseId::as_uuid))
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
             SET state='completing',revision=revision+1,completion_idempotency_key=$5,completion_request_sha256=$6,completion_lease_token=$7,completion_lease_expires_at=date_trunc('milliseconds',clock_timestamp())+($8*interval '1 second'),updated_at=now() \
             WHERE upload_id=$1 AND ($2::uuid IS NULL OR project_id=$2) AND course_id IS NOT DISTINCT FROM $3 \
               AND state='pending' AND expires_at>date_trunc('milliseconds',clock_timestamp()) AND revision=$4 \
             RETURNING upload_id",
        )
        .bind(upload_id.as_uuid())
        .bind(project_id.map(ProjectId::as_uuid))
        .bind(course_id.map(CourseId::as_uuid))
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
             (package_id,project_id,course_id,revision,manifest_sha256,contract,completed_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7)",
        )
        .bind(package.id.as_uuid())
        .bind(package.project_id.as_uuid())
        .bind(package.course_id.map(CourseId::as_uuid))
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
        let event_payload = json!({"packageId":package.id,"revision":package.revision,"manifestSha256":manifest_sha256});
        if let Some(course_id) = package.course_id {
            append_sse(
                &mut transaction,
                course_id,
                "problem_package.completed.v1",
                package.id.as_uuid(),
                package.revision,
                event_payload,
            )
            .await?;
        } else {
            append_project_sse(
                &mut transaction,
                package.project_id,
                "problem_package.completed.v1",
                package.id.as_uuid(),
                package.revision,
                event_payload,
            )
            .await?;
        }
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

async fn advisory_project_lock(
    transaction: &mut Transaction<'_, Postgres>,
    project_id: ProjectId,
) -> Result<(), ControlError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!("project:{project_id}"))
        .execute(&mut **transaction)
        .await
        .map_err(db)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn insert_candidate(
    transaction: &mut Transaction<'_, Postgres>,
    project_id: ProjectId,
    course_id: Option<CourseId>,
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
         (candidate_id,candidate_kind,project_id,course_id,revision,state,content_sha256,contract,run_id, \
          policy_revision,schema_sha256,projected_event_id) \
         VALUES ($1,$2,$3,$4,$5,'validated',$6,$7,$8,$9,$10,$11) \
         ON CONFLICT (candidate_id) DO NOTHING",
    )
    .bind(candidate_id.as_uuid())
    .bind(kind)
    .bind(project_id.as_uuid())
    .bind(course_id.map(CourseId::as_uuid))
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
            "SELECT candidate_kind,project_id,course_id,revision,content_sha256,contract,run_id, \
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
            && existing.try_get::<Uuid, _>("project_id").map_err(db)? == project_id.as_uuid()
            && existing
                .try_get::<Option<Uuid>, _>("course_id")
                .map_err(db)?
                == course_id.map(CourseId::as_uuid)
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
    let event_payload =
        json!({"candidateId":candidate_id,"revision":revision,"contentSha256":content_sha256});
    if let Some(course_id) = course_id {
        append_sse(
            transaction,
            course_id,
            &format!("{kind}_candidate.validated.v1"),
            candidate_id.as_uuid(),
            revision,
            event_payload,
        )
        .await
    } else {
        append_project_sse(
            transaction,
            project_id,
            &format!("{kind}_candidate.validated.v1"),
            candidate_id.as_uuid(),
            revision,
            event_payload,
        )
        .await
    }
}

#[allow(clippy::too_many_arguments)]
async fn enqueue_container_build(
    transaction: &mut Transaction<'_, Postgres>,
    config: &ControlConfig,
    project_id: ProjectId,
    course_id: Option<CourseId>,
    package_id: ProblemPackageId,
    candidate: &EnvironmentCandidate,
    generated_context: Option<&GeneratedArtifactRecord>,
    created_at: UtcTimestamp,
) -> Result<(), ControlError> {
    candidate
        .validate()
        .map_err(|_| ControlError::ContractInvalid)?;
    if candidate.project_id != project_id {
        return Err(ControlError::ProjectMismatch);
    }
    if candidate.course_id != course_id {
        return Err(ControlError::CourseMismatch);
    }
    let contracts::authoring::EnvironmentRuntimeSpec::Container { build_context, .. } =
        &candidate.spec.runtime
    else {
        if generated_context.is_some() {
            return Err(ControlError::PersistenceIdentityMismatch);
        }
        return Ok(());
    };

    let context_object_key = resolve_container_context_object_key(
        transaction,
        project_id,
        course_id,
        package_id,
        build_context,
        generated_context,
    )
    .await?;

    let existing = sqlx::query(
        "SELECT build_request_id,project_id,course_id,candidate_id,candidate_revision, \
                candidate_sha256,command_sha256,state,contract \
         FROM control.container_build_projections \
         WHERE candidate_id=$1 AND candidate_revision=$2 FOR UPDATE",
    )
    .bind(candidate.id.as_uuid())
    .bind(i64_revision(candidate.revision)?)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(db)?;
    if let Some(row) = existing {
        validate_existing_container_build_projection(
            &row,
            project_id,
            course_id,
            candidate,
            &context_object_key,
        )?;
        return Ok(());
    }

    let build_request = BuildRequest {
        id: BuildRequestId::new(),
        project_id,
        course_id,
        candidate_id: candidate.id,
        candidate_revision: candidate.revision,
        builder_binding: config.container_build.builder_binding.clone(),
        context: build_context.clone(),
        context_object_key: context_object_key.clone(),
        dockerfile_path: config.container_build.dockerfile_path.clone(),
        output_repository: container_build_output_repository(
            config,
            project_id,
            course_id,
            candidate.id,
        ),
        network: config.container_build.network.clone(),
        max_duration_milliseconds: config.container_build.max_duration_milliseconds,
        max_cpu_millicores: config.container_build.max_cpu_millicores,
        max_memory_bytes: config.container_build.max_memory_bytes,
        created_at,
    };
    build_request
        .validate()
        .map_err(|_| ControlError::ContractInvalid)?;
    let command = AgentBuildRequested {
        idempotency_key: format!("build:{}", build_request.id),
        request: build_request,
    };
    command
        .validate()
        .map_err(|_| ControlError::ContractInvalid)?;
    let candidate_sha256 = canonical_hash(&candidate.spec)?;
    let command_sha256 = canonical_hash(&command)?;
    let command_contract =
        serde_json::to_value(&command).map_err(|_| ControlError::ContractInvalid)?;
    let inserted = sqlx::query(
        "INSERT INTO control.container_build_projections \
         (build_request_id,project_id,course_id,candidate_id,candidate_revision,candidate_sha256, \
          command_sha256,state,contract,created_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,'requested',$8,$9) \
         ON CONFLICT (candidate_id,candidate_revision) DO NOTHING",
    )
    .bind(command.request.id.as_uuid())
    .bind(project_id.as_uuid())
    .bind(course_id.map(CourseId::as_uuid))
    .bind(candidate.id.as_uuid())
    .bind(i64_revision(candidate.revision)?)
    .bind(candidate_sha256.to_string())
    .bind(command_sha256.to_string())
    .bind(&command_contract)
    .bind(created_at.get())
    .execute(&mut **transaction)
    .await
    .map_err(|error| {
        if is_unique_violation(&error) {
            ControlError::ProjectionConflict
        } else {
            db(error)
        }
    })?;
    if inserted.rows_affected() == 0 {
        let row = sqlx::query(
            "SELECT build_request_id,project_id,course_id,candidate_id,candidate_revision, \
                    candidate_sha256,command_sha256,state,contract \
             FROM control.container_build_projections \
             WHERE candidate_id=$1 AND candidate_revision=$2 FOR UPDATE",
        )
        .bind(candidate.id.as_uuid())
        .bind(i64_revision(candidate.revision)?)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(db)?
        .ok_or(ControlError::PersistenceIdentityMismatch)?;
        validate_existing_container_build_projection(
            &row,
            project_id,
            course_id,
            candidate,
            &context_object_key,
        )?;
        return Ok(());
    }

    let event_id = EventId::new();
    let contract = event_contract(BUILD_REQUEST_SUBJECT)?;
    let event = CloudEvent {
        specversion: SPEC_VERSION.to_owned(),
        id: event_id,
        source: contract.source().to_owned(),
        event_type: BUILD_REQUEST_SUBJECT.to_owned(),
        subject: BUILD_REQUEST_SUBJECT.to_owned(),
        time: created_at,
        datacontenttype: "application/json".to_owned(),
        dataschema: contract.data_schema(),
        project_id,
        course_id,
        aggregate_revision: Revision::new(1).map_err(|_| ControlError::ContractInvalid)?,
        aggregate_sequence: Sequence(1),
        trace_id: format!("build:{}", command.request.id),
        data: command,
    };
    event
        .validate(contract)
        .map_err(|_| ControlError::ContractInvalid)?;
    let payload = serde_json::to_value(&event).map_err(|_| ControlError::ContractInvalid)?;
    OutboxStore::enqueue(
        transaction,
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
    tracing::info!(
        event = "control.container_build.requested",
        project_id = %project_id,
        course_id = ?course_id,
        run_id = %candidate.run_id,
        candidate_id = %candidate.id,
        candidate_revision = candidate.revision.get(),
        build_request_id = %event.data.request.id,
        "container build projection enqueued",
    );
    Ok(())
}

fn validate_existing_container_build_projection(
    row: &sqlx::postgres::PgRow,
    project_id: ProjectId,
    course_id: Option<CourseId>,
    candidate: &EnvironmentCandidate,
    context_object_key: &str,
) -> Result<(), ControlError> {
    let build_request_id: Uuid = row.try_get("build_request_id").map_err(db)?;
    let persisted_project_id: Uuid = row.try_get("project_id").map_err(db)?;
    let persisted_course_id: Option<Uuid> = row.try_get("course_id").map_err(db)?;
    let persisted_candidate_id: Uuid = row.try_get("candidate_id").map_err(db)?;
    let persisted_candidate_revision: i64 = row.try_get("candidate_revision").map_err(db)?;
    let persisted_candidate_sha256: Sha256Digest = row
        .try_get::<String, _>("candidate_sha256")
        .map_err(db)?
        .parse()
        .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
    let state: String = row.try_get("state").map_err(db)?;
    if !matches!(
        state.as_str(),
        "requested" | "succeeded" | "failed" | "cancelled"
    ) {
        return Err(ControlError::PersistenceIdentityMismatch);
    }
    let command: AgentBuildRequested = serde_json::from_value(row.try_get("contract").map_err(db)?)
        .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
    command
        .validate()
        .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
    let command_sha256: Sha256Digest = row
        .try_get::<String, _>("command_sha256")
        .map_err(db)?
        .parse()
        .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
    let expected_candidate_sha256 = canonical_hash(&candidate.spec)?;
    let expected_revision = i64_revision(candidate.revision)?;
    let expected_course_id = course_id.map(CourseId::as_uuid);
    let contracts::authoring::EnvironmentRuntimeSpec::Container { build_context, .. } =
        &candidate.spec.runtime
    else {
        return Err(ControlError::PersistenceIdentityMismatch);
    };
    let request_matches = persisted_project_id == project_id.as_uuid()
        && persisted_course_id == expected_course_id
        && persisted_candidate_id == candidate.id.as_uuid()
        && persisted_candidate_revision == expected_revision
        && persisted_candidate_sha256 == expected_candidate_sha256
        && build_request_id == command.request.id.as_uuid()
        && command.request.project_id == project_id
        && command.request.course_id == course_id
        && command.request.candidate_id == candidate.id
        && command.request.candidate_revision == candidate.revision
        && command.request.context == *build_context
        && command.request.context_object_key == context_object_key
        && command.idempotency_key == format!("build:{}", command.request.id)
        && command_sha256 == canonical_hash(&command)?;
    if !request_matches {
        return Err(ControlError::ProjectionConflict);
    }
    Ok(())
}

async fn resolve_container_context_object_key(
    transaction: &mut Transaction<'_, Postgres>,
    project_id: ProjectId,
    course_id: Option<CourseId>,
    package_id: ProblemPackageId,
    build_context: &contracts::ArtifactRef,
    generated_context: Option<&GeneratedArtifactRecord>,
) -> Result<String, ControlError> {
    let package_row = sqlx::query(
        "SELECT project_id,course_id,revision,contract \
         FROM control.problem_packages \
         WHERE package_id=$1 AND project_id=$2 FOR SHARE",
    )
    .bind(package_id.as_uuid())
    .bind(project_id.as_uuid())
    .fetch_optional(&mut **transaction)
    .await
    .map_err(db)?
    .ok_or(ControlError::PersistenceIdentityMismatch)?;
    let persisted_project_id: Uuid = package_row.try_get("project_id").map_err(db)?;
    let persisted_course_id: Option<Uuid> = package_row.try_get("course_id").map_err(db)?;
    let persisted_revision: i64 = package_row.try_get("revision").map_err(db)?;
    let package: ProblemPackage =
        serde_json::from_value(package_row.try_get("contract").map_err(db)?)
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
    package
        .validate()
        .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
    if persisted_project_id != project_id.as_uuid()
        || persisted_course_id != course_id.map(CourseId::as_uuid)
        || package.id != package_id
        || package.project_id != project_id
        || package.course_id != course_id
        || i64_revision(package.revision)? != persisted_revision
    {
        return Err(ControlError::PersistenceIdentityMismatch);
    }

    let uploaded_file = package
        .files
        .iter()
        .find(|file| file.object == *build_context);
    if uploaded_file.is_some() {
        if generated_context.is_some() {
            return Err(ControlError::PersistenceIdentityMismatch);
        }
        return sqlx::query_scalar::<_, String>(
            "SELECT f.object_key \
             FROM control.problem_package_upload_files f \
             JOIN control.problem_package_upload_sessions s ON s.upload_id=f.upload_id \
             WHERE f.artifact_id=$1 AND f.object_version=$2 AND f.size_bytes=$3 \
               AND f.media_type=$4 AND s.project_id=$5 \
               AND s.course_id IS NOT DISTINCT FROM $6 AND s.completed_package_id=$7 \
               AND s.state='completed' AND f.verified_at IS NOT NULL",
        )
        .bind(build_context.artifact_id.as_uuid())
        .bind(&build_context.object_version)
        .bind(i64::try_from(build_context.size_bytes).map_err(|_| ControlError::ContractInvalid)?)
        .bind(&build_context.media_type)
        .bind(project_id.as_uuid())
        .bind(course_id.map(CourseId::as_uuid))
        .bind(package_id.as_uuid())
        .fetch_optional(&mut **transaction)
        .await
        .map_err(db)?
        .ok_or(ControlError::PersistenceIdentityMismatch);
    }

    let generated = generated_context.ok_or(ControlError::PersistenceIdentityMismatch)?;
    if generated.kind != contracts::http::GeneratedArtifactKind::BuildContext
        || generated.artifact != *build_context
        || generated.project_id != project_id
        || generated.course_id != course_id
        || generated.package_id != package_id
        || generated.package_revision != package.revision
        || !valid_generated_object_key(&generated.object_key)
    {
        return Err(ControlError::PersistenceIdentityMismatch);
    }
    Ok(generated.object_key.clone())
}

fn valid_generated_object_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 2 * 1024
        && !value.contains("..")
        && !value.bytes().any(|byte| byte.is_ascii_control())
}

fn container_build_output_repository(
    config: &ControlConfig,
    project_id: ProjectId,
    course_id: Option<CourseId>,
    candidate_id: CandidateId,
) -> String {
    format!(
        "{}/{}-{candidate_id}",
        config
            .container_build
            .output_repository_prefix
            .trim_end_matches('/'),
        course_id.map_or_else(
            || format!("project-{project_id}"),
            |course_id| format!("course-{course_id}"),
        )
    )
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

async fn append_project_sse(
    transaction: &mut Transaction<'_, Postgres>,
    project_id: ProjectId,
    event_type: &str,
    aggregate_id: Uuid,
    aggregate_revision: Revision,
    payload: Value,
) -> Result<(), ControlError> {
    reject_sensitive_payload(&payload)?;
    let payload_hash = canonical_hash(&payload)?;
    let sequence = sqlx::query_scalar::<_, i64>(
        "INSERT INTO control.sse_project_cursors (project_id,last_sequence) VALUES ($1,1) \
         ON CONFLICT (project_id) DO UPDATE \
         SET last_sequence=control.sse_project_cursors.last_sequence+1 \
         RETURNING last_sequence",
    )
    .bind(project_id.as_uuid())
    .fetch_one(&mut **transaction)
    .await
    .map_err(db)?;
    sqlx::query(
        "INSERT INTO control.sse_project_events \
         (project_id,sequence,event_type,aggregate_id,aggregate_revision,payload,payload_sha256) \
         VALUES ($1,$2,$3,$4,$5,$6,$7)",
    )
    .bind(project_id.as_uuid())
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

/// Verifies that a requested authoring artifact is the exact artifact produced for the selected
/// Environment candidate. VM artifacts are deployment-owned and therefore compared with the
/// reviewed fixed base policy; container artifacts must have a succeeded build projection.
async fn validate_authoring_artifact(
    transaction: &mut Transaction<'_, Postgres>,
    project_id: ProjectId,
    course_id: Option<CourseId>,
    environment: &EnvironmentCandidate,
    artifact: &ImageArtifact,
    config: &ControlConfig,
) -> Result<(), ControlError> {
    if artifact.runtime_kind() != environment.spec.runtime.kind() {
        return Err(ControlError::ArtifactMismatch);
    }
    match (&environment.spec.runtime, artifact) {
        (
            contracts::authoring::EnvironmentRuntimeSpec::Container { .. },
            ImageArtifact::Container {
                build_request_id, ..
            },
        ) => {
            let row = sqlx::query(
                "SELECT builds.build_request_id,builds.candidate_sha256,artifacts.artifact \
                 FROM control.container_build_projections builds \
                 JOIN control.image_artifact_projections artifacts \
                   ON artifacts.image_artifact_id=builds.image_artifact_id \
                 WHERE builds.build_request_id=$1 AND builds.project_id=$2 \
                   AND builds.course_id IS NOT DISTINCT FROM $3 \
                   AND builds.candidate_id=$4 AND builds.candidate_revision=$5 \
                   AND builds.state='succeeded' FOR SHARE",
            )
            .bind(build_request_id.as_uuid())
            .bind(project_id.as_uuid())
            .bind(course_id.map(CourseId::as_uuid))
            .bind(environment.id.as_uuid())
            .bind(i64_revision(environment.revision)?)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(db)?
            .ok_or(ControlError::ArtifactNotAuthoritative)?;
            let persisted_build_request: Uuid = row.try_get("build_request_id").map_err(db)?;
            if persisted_build_request != build_request_id.as_uuid() {
                return Err(ControlError::PersistenceIdentityMismatch);
            }
            let persisted_candidate_hash: Sha256Digest = row
                .try_get::<String, _>("candidate_sha256")
                .map_err(db)?
                .parse()
                .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
            if persisted_candidate_hash != canonical_hash(&environment.spec)? {
                return Err(ControlError::ArtifactMismatch);
            }
            let persisted_artifact: ImageArtifact =
                serde_json::from_value(row.try_get("artifact").map_err(db)?)
                    .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
            if persisted_artifact != *artifact {
                return Err(ControlError::ArtifactMismatch);
            }
            Ok(())
        }
        (
            contracts::authoring::EnvironmentRuntimeSpec::VirtualMachine {
                provider_binding,
                base_disk,
                storage_class_binding,
                ..
            },
            ImageArtifact::VirtualMachine { .. },
        ) => {
            let policy = &config.virtual_machine_base;
            if provider_binding != &policy.provider_binding
                || storage_class_binding != &policy.storage_class_binding
                || base_disk != &policy.base_disk
            {
                return Err(ControlError::ArtifactMismatch);
            }
            let expected = ImageArtifact::VirtualMachine {
                id: policy.artifact_id,
                base_disk: policy.base_disk.clone(),
                format: policy.format,
            };
            if *artifact != expected {
                return Err(ControlError::ArtifactMismatch);
            }
            Ok(())
        }
        _ => Err(ControlError::ArtifactMismatch),
    }
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

fn validate_project_fields(name: &str, description: Option<&str>) -> Result<(), ControlError> {
    if name.trim().is_empty()
        || name.chars().count() > 120
        || name.chars().any(char::is_control)
        || description.is_some_and(|value| {
            value.chars().count() > 2_000 || value.chars().any(char::is_control)
        })
    {
        return Err(ControlError::ProjectInvalid);
    }
    Ok(())
}

fn project_from_value(value: Value) -> Result<Project, ControlError> {
    let project: Project =
        serde_json::from_value(value).map_err(|_| ControlError::PersistenceIdentityMismatch)?;
    project
        .validate()
        .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
    Ok(project)
}

fn next_revision(current: Revision) -> Result<Revision, ControlError> {
    Revision::new(
        current
            .get()
            .checked_add(1)
            .ok_or(ControlError::PersistenceIdentityMismatch)?,
    )
    .map_err(|_| ControlError::PersistenceIdentityMismatch)
}

fn publication_state_from_db(value: &str) -> Result<AuthoringPublicationState, ControlError> {
    match value {
        "pending" => Ok(AuthoringPublicationState::Pending),
        "publishing" => Ok(AuthoringPublicationState::Publishing),
        "ready" => Ok(AuthoringPublicationState::Ready),
        "failed" => Ok(AuthoringPublicationState::Failed),
        _ => Err(ControlError::PersistenceIdentityMismatch),
    }
}

fn publication_contract(
    status: &AuthoringApprovalPublicationStatus,
) -> Result<Value, ControlError> {
    status
        .validate()
        .map_err(|_| ControlError::ContractInvalid)?;
    status
        .validate_ownership(status.approval.project_id, status.approval.course_id)
        .map_err(|_| ControlError::ContractInvalid)?;
    if status.revision.get() == 0
        || (status.status == AuthoringPublicationState::Ready
            && (status.environment_release_id.is_none() || status.evaluation_release_id.is_none()))
        || (status.status == AuthoringPublicationState::Failed && status.diagnostic_code.is_none())
    {
        return Err(ControlError::ContractInvalid);
    }
    serde_json::to_value(status).map_err(|_| ControlError::ContractInvalid)
}

fn authoring_publication_status_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<AuthoringApprovalPublicationStatus, ControlError> {
    let status: AuthoringApprovalPublicationStatus =
        serde_json::from_value(row.try_get("contract").map_err(db)?)
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
    status
        .validate()
        .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
    let state = publication_state_from_db(&row.try_get::<String, _>("state").map_err(db)?)?;
    if status.status != state {
        return Err(ControlError::PersistenceIdentityMismatch);
    }
    let project_id = ProjectId::from_str(
        &row.try_get::<Uuid, _>("project_id")
            .map_err(db)?
            .to_string(),
    )
    .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
    let course_id = row
        .try_get::<Option<Uuid>, _>("course_id")
        .map_err(db)?
        .map(|value| CourseId::from_str(&value.to_string()))
        .transpose()
        .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
    if status.approval.project_id != project_id || status.approval.course_id != course_id {
        return Err(ControlError::PersistenceIdentityMismatch);
    }
    let environment_release_id = row
        .try_get::<Option<Uuid>, _>("environment_release_id")
        .map_err(db)?
        .map(|value| ReleaseId::from_str(&value.to_string()))
        .transpose()
        .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
    let evaluation_release_id = row
        .try_get::<Option<Uuid>, _>("evaluation_release_id")
        .map_err(db)?
        .map(|value| contracts::EvaluationReleaseId::from_str(&value.to_string()))
        .transpose()
        .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
    let evaluation_release_revision = row
        .try_get::<Option<i64>, _>("evaluation_release_revision")
        .map_err(db)?
        .map(revision_from_i64)
        .transpose()?;
    if status.environment_release_id != environment_release_id
        || status.evaluation_release_id != evaluation_release_id
        || status.evaluation_release_revision != evaluation_release_revision
    {
        return Err(ControlError::PersistenceIdentityMismatch);
    }
    Ok(status)
}

async fn project_from_tx(
    transaction: &mut Transaction<'_, Postgres>,
    project_id: ProjectId,
) -> Result<Project, ControlError> {
    let row = sqlx::query("SELECT contract FROM control.projects WHERE project_id=$1 FOR UPDATE")
        .bind(project_id.as_uuid())
        .fetch_optional(&mut **transaction)
        .await
        .map_err(db)?
        .ok_or(ControlError::ProjectNotFound)?;
    let project = project_from_value(row.try_get("contract").map_err(db)?)?;
    if project.id != project_id {
        return Err(ControlError::PersistenceIdentityMismatch);
    }
    Ok(project)
}

fn project_state_from_db(value: &str) -> Result<ProjectState, ControlError> {
    match value {
        "active" => Ok(ProjectState::Active),
        "archived" => Ok(ProjectState::Archived),
        _ => Err(ControlError::PersistenceIdentityMismatch),
    }
}

fn platform_role_name(role: PlatformRole) -> &'static str {
    match role {
        PlatformRole::Teacher => "teacher",
        PlatformRole::Student => "student",
        PlatformRole::PlatformAdmin => "platform_admin",
    }
}

fn project_membership_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<ProjectMembership, ControlError> {
    let project_id = ProjectId::from_str(
        &row.try_get::<Uuid, _>("project_id")
            .map_err(db)?
            .to_string(),
    )
    .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
    let actor_id = ActorId::from_str(&row.try_get::<Uuid, _>("actor_id").map_err(db)?.to_string())
        .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
    let course_id = row
        .try_get::<Option<Uuid>, _>("course_id")
        .map_err(db)?
        .map(|value| CourseId::from_str(&value.to_string()))
        .transpose()
        .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
    let role = match row.try_get::<String, _>("role").map_err(db)?.as_str() {
        "teacher" => PlatformRole::Teacher,
        "student" => PlatformRole::Student,
        "platform_admin" => PlatformRole::PlatformAdmin,
        _ => return Err(ControlError::PersistenceIdentityMismatch),
    };
    let state = match row.try_get::<String, _>("state").map_err(db)?.as_str() {
        "active" => MembershipState::Active,
        "suspended" => MembershipState::Suspended,
        "revoked" => MembershipState::Revoked,
        _ => return Err(ControlError::PersistenceIdentityMismatch),
    };
    Ok(ProjectMembership {
        course_id,
        project_id,
        actor_id,
        role,
        state,
        revision: revision_from_i64(row.try_get("revision").map_err(db)?)?,
        expires_at: row
            .try_get::<Option<time::OffsetDateTime>, _>("expires_at")
            .map_err(db)?
            .map(UtcTimestamp::from_utc)
            .transpose()
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?,
    })
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

async fn load_candidate_project_contract<T>(
    pool: &PgPool,
    project_id: ProjectId,
    candidate_id: CandidateId,
    kind: &str,
) -> Result<T, ControlError>
where
    T: serde::de::DeserializeOwned,
{
    let row = sqlx::query(
        "SELECT contract FROM control.candidates \
         WHERE candidate_id=$1 AND project_id=$2 AND candidate_kind=$3 AND state='validated'",
    )
    .bind(candidate_id.as_uuid())
    .bind(project_id.as_uuid())
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

/// Resolve the artifact that is safe to present as the approval input.
///
/// Container candidates require a succeeded, Control-projected build. VM
/// candidates have no build projection; their artifact is the deployment-owned
/// base only when every reviewed binding still matches the active policy.
fn resolve_candidate_image_artifact(
    candidate: &EnvironmentCandidate,
    build: Option<&CandidateBuildView>,
    virtual_machine_base: &VirtualMachineBasePolicy,
) -> Result<Option<ImageArtifact>, ControlError> {
    let artifact = match &candidate.spec.runtime {
        contracts::authoring::EnvironmentRuntimeSpec::Container { .. } => build
            .filter(|view| view.state == CandidateBuildState::Succeeded)
            .and_then(|view| view.artifact.clone()),
        contracts::authoring::EnvironmentRuntimeSpec::VirtualMachine {
            provider_binding,
            base_disk,
            storage_class_binding,
            ..
        } if provider_binding == &virtual_machine_base.provider_binding
            && base_disk == &virtual_machine_base.base_disk
            && storage_class_binding == &virtual_machine_base.storage_class_binding =>
        {
            Some(ImageArtifact::VirtualMachine {
                id: virtual_machine_base.artifact_id,
                base_disk: virtual_machine_base.base_disk.clone(),
                format: virtual_machine_base.format,
            })
        }
        contracts::authoring::EnvironmentRuntimeSpec::VirtualMachine { .. } => None,
    };

    if let Some(artifact) = &artifact
        && (artifact.runtime_kind() != candidate.spec.runtime.kind()
            || artifact.validate().is_err())
    {
        return Err(ControlError::PersistenceIdentityMismatch);
    }
    Ok(artifact)
}

async fn load_candidate_build(
    pool: &PgPool,
    course_id: CourseId,
    candidate: &EnvironmentCandidate,
) -> Result<Option<CandidateBuildView>, ControlError> {
    let candidate_sha256 = canonical_hash(&candidate.spec)?;
    let row = sqlx::query(
        "SELECT builds.state,builds.terminal_diagnostic,builds.cleanup_verified, \
                artifacts.artifact \
         FROM control.container_build_projections builds \
         JOIN control.candidates candidates \
           ON candidates.candidate_id=builds.candidate_id \
          AND candidates.revision=builds.candidate_revision \
          AND candidates.content_sha256=builds.candidate_sha256 \
         LEFT JOIN control.image_artifact_projections artifacts \
           ON artifacts.image_artifact_id=builds.image_artifact_id \
         WHERE builds.course_id=$1 AND builds.candidate_id=$2 \
           AND candidates.course_id=$1 AND candidates.candidate_kind='environment' \
           AND candidates.state='validated' AND candidates.revision=$3 \
           AND candidates.content_sha256=$4",
    )
    .bind(course_id.as_uuid())
    .bind(candidate.id.as_uuid())
    .bind(i64_revision(candidate.revision)?)
    .bind(candidate_sha256.to_string())
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

async fn load_candidate_build_project(
    pool: &PgPool,
    project_id: ProjectId,
    candidate: &EnvironmentCandidate,
) -> Result<Option<CandidateBuildView>, ControlError> {
    let candidate_sha256 = canonical_hash(&candidate.spec)?;
    let row = sqlx::query(
        "SELECT builds.state,builds.terminal_diagnostic,builds.cleanup_verified, \
                artifacts.artifact \
         FROM control.container_build_projections builds \
         JOIN control.candidates candidates \
           ON candidates.candidate_id=builds.candidate_id \
          AND candidates.revision=builds.candidate_revision \
          AND candidates.content_sha256=builds.candidate_sha256 \
         LEFT JOIN control.image_artifact_projections artifacts \
           ON artifacts.image_artifact_id=builds.image_artifact_id \
         WHERE builds.project_id=$1 AND builds.candidate_id=$2 \
           AND candidates.project_id=$1 AND candidates.candidate_kind='environment' \
           AND candidates.state='validated' AND candidates.revision=$3 \
           AND candidates.content_sha256=$4",
    )
    .bind(project_id.as_uuid())
    .bind(candidate.id.as_uuid())
    .bind(i64_revision(candidate.revision)?)
    .bind(candidate_sha256.to_string())
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
        .map(|value| {
            serde_json::from_value(value).map_err(|_| ControlError::PersistenceIdentityMismatch)
        })
        .transpose()?;
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
        diagnostic_code,
        cleanup_verified,
        artifact,
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

fn project_release_view(
    row: &sqlx::postgres::PgRow,
    project_id: ProjectId,
    actor_id: ActorId,
) -> Result<EnvironmentTemplateReleaseView, ControlError> {
    let view = release_view(row)?;
    let release = &view.release;
    release
        .validate()
        .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
    let persisted_project_id = ProjectId::from_str(
        &row.try_get::<Uuid, _>("release_project_id")
            .map_err(db)?
            .to_string(),
    )
    .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
    let persisted_release_id = ReleaseId::from_str(
        &row.try_get::<Uuid, _>("release_id")
            .map_err(db)?
            .to_string(),
    )
    .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
    let persisted_course_id = row
        .try_get::<Option<Uuid>, _>("release_course_id")
        .map_err(db)?
        .map(|value| CourseId::from_str(&value.to_string()))
        .transpose()
        .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
    let persisted_version = u64::try_from(row.try_get::<i64, _>("release_version").map_err(db)?)
        .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
    let persisted_candidate_id = CandidateId::from_str(
        &row.try_get::<Uuid, _>("release_candidate_id")
            .map_err(db)?
            .to_string(),
    )
    .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
    let persisted_candidate_revision = revision_from_i64(
        row.try_get::<i64, _>("release_candidate_revision")
            .map_err(db)?,
    )?;
    if persisted_project_id != project_id
        || release.id != persisted_release_id
        || release.project_id != persisted_project_id
        || release.course_id != persisted_course_id
        || release.version != persisted_version
        || release.candidate_id != persisted_candidate_id
        || release.candidate_revision != persisted_candidate_revision
    {
        return Err(ControlError::PersistenceIdentityMismatch);
    }
    let owner_actor_id = ActorId::from_str(
        &row.try_get::<Uuid, _>("owner_actor_id")
            .map_err(db)?
            .to_string(),
    )
    .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
    let candidate_project_id = ProjectId::from_str(
        &row.try_get::<Uuid, _>("candidate_project_id")
            .map_err(db)?
            .to_string(),
    )
    .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
    let candidate_course_id = row
        .try_get::<Option<Uuid>, _>("candidate_course_id")
        .map_err(db)?
        .map(|value| CourseId::from_str(&value.to_string()))
        .transpose()
        .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
    let candidate_revision =
        revision_from_i64(row.try_get::<i64, _>("candidate_revision").map_err(db)?)?;
    let candidate: EnvironmentCandidate =
        serde_json::from_value(row.try_get::<Value, _>("candidate_contract").map_err(db)?)
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
    candidate
        .validate()
        .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
    if candidate.project_id != candidate_project_id
        || candidate.course_id != candidate_course_id
        || candidate.revision != candidate_revision
    {
        return Err(ControlError::PersistenceIdentityMismatch);
    }
    release
        .validate_against_candidate(&candidate)
        .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
    match candidate.spec.class {
        EnvironmentClass::Work => {
            if owner_actor_id != actor_id {
                return Err(ControlError::NotFound);
            }
            if row
                .try_get::<Option<Value>, _>("publication_contract")
                .map_err(db)?
                .is_some()
            {
                return Err(ControlError::PersistenceIdentityMismatch);
            }
        }
        EnvironmentClass::Experiment => {
            let publication = row
                .try_get::<Option<Value>, _>("publication_contract")
                .map_err(db)?
                .ok_or(ControlError::NotFound)
                .and_then(|value| {
                    serde_json::from_value::<AuthoringApprovalPublicationStatus>(value)
                        .map_err(|_| ControlError::PersistenceIdentityMismatch)
                })?;
            publication
                .validate()
                .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
            publication
                .validate_ownership(project_id, release.course_id)
                .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
            if publication.status != AuthoringPublicationState::Ready
                || publication.approval.id != release.approval.id
                || publication.environment_release_id != Some(release.id)
            {
                return Err(ControlError::NotFound);
            }
        }
    }
    Ok(view)
}

fn event_contract(subject: &str) -> Result<contracts::events::EventContract, ControlError> {
    EVENT_CONTRACTS
        .iter()
        .copied()
        .find(|contract| contract.subject == subject)
        .ok_or(ControlError::ContractInvalid)
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
        AgentRunState::AwaitingApproval => "awaiting_approval",
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

#[allow(
    clippy::needless_pass_by_value,
    reason = "database errors are consumed to preserve structured error context in the log"
)]
fn db(error: sqlx::Error) -> ControlError {
    tracing::error!(
        error = ?error,
        diagnostic_code = "LW_CONTROL_PERSISTENCE_FAILED",
        "control database operation failed",
    );
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
    #[error("LW_PROJECT_SCOPE_MISMATCH")]
    ProjectMismatch,
    #[error("LW_PROJECT_INVALID")]
    ProjectInvalid,
    #[error("LW_PROJECT_NOT_FOUND")]
    ProjectNotFound,
    #[error("LW_PROJECT_ARCHIVED")]
    ProjectArchived,
    #[error("LW_PROJECT_GOVERNANCE_DENIED")]
    ProjectGovernanceDenied,
    #[error("LW_PROJECT_COURSE_MEMBERSHIP_REQUIRED")]
    CourseMembershipRequired,
    #[error("LW_PROJECT_MEMBERSHIP_CONFLICT")]
    MembershipConflict,
    #[error("LW_PROJECT_MEMBERSHIP_NOT_FOUND")]
    MembershipNotFound,
    #[error("LW_PROJECT_OWNER_MEMBERSHIP_PROTECTED")]
    OwnerMembershipProtected,
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
    use contracts::AgentRunId;
    use contracts::authoring::{EnvironmentCandidate, EnvironmentRuntimeSpec};
    use contracts::http::{CreateProblemPackageUploadRequest, ProblemPackageUploadFile};
    use contracts::supply_chain::{ImageArtifact, VirtualMachineBaseDisk};
    use contracts::{CourseId, PolicyId, ProjectId, Revision};
    use persistence_sqlx::Sha256Digest;

    use contracts::supply_chain::BuildNetworkPolicy;

    use super::{
        ContainerBuildPolicy, ControlConfig, ControlError, EvaluationRuntimePolicy,
        VirtualMachineBasePolicy, reject_sensitive_payload, resolve_candidate_image_artifact,
        validate_upload_request,
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
                provider_binding: "evaluation-primary-v1".to_owned(),
                runner_image: format!("runner@sha256:{}", "a".repeat(64)),
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
            project_id: ProjectId::new(),
            course_id: Some(CourseId::new()),
            files: vec![file.clone()],
            retention_policy_revision: Revision::new(1)?,
        };
        validate_upload_request(&request, &config()?)?;
        let duplicate = CreateProblemPackageUploadRequest {
            project_id: request.project_id,
            course_id: request.course_id,
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

    #[test]
    fn candidate_view_image_artifact_requires_exact_vm_policy_bindings()
    -> Result<(), Box<dyn std::error::Error>> {
        let config = config()?;
        let candidate = vm_candidate(
            &config.virtual_machine_base.provider_binding,
            &config.virtual_machine_base.storage_class_binding,
            &config.virtual_machine_base.base_disk,
        )?;
        let expected = ImageArtifact::VirtualMachine {
            id: config.virtual_machine_base.artifact_id,
            base_disk: config.virtual_machine_base.base_disk.clone(),
            format: config.virtual_machine_base.format,
        };
        assert_eq!(
            resolve_candidate_image_artifact(&candidate, None, &config.virtual_machine_base,)?,
            Some(expected.clone())
        );

        let mut provider_mismatch = candidate.clone();
        if let EnvironmentRuntimeSpec::VirtualMachine {
            provider_binding, ..
        } = &mut provider_mismatch.spec.runtime
        {
            *provider_binding = "other-provider".to_owned();
        }
        assert_eq!(
            resolve_candidate_image_artifact(
                &provider_mismatch,
                None,
                &config.virtual_machine_base,
            )?,
            None
        );

        let mut storage_mismatch = candidate.clone();
        if let EnvironmentRuntimeSpec::VirtualMachine {
            storage_class_binding,
            ..
        } = &mut storage_mismatch.spec.runtime
        {
            *storage_class_binding = "other-storage".to_owned();
        }
        assert_eq!(
            resolve_candidate_image_artifact(
                &storage_mismatch,
                None,
                &config.virtual_machine_base,
            )?,
            None
        );

        let mut disk_mismatch = candidate;
        if let EnvironmentRuntimeSpec::VirtualMachine { base_disk, .. } =
            &mut disk_mismatch.spec.runtime
        {
            base_disk.source_registry_digest = format!(
                "docker://quay.io/containerdisks/ubuntu@sha256:{}",
                "a".repeat(64)
            );
        }
        assert_eq!(
            resolve_candidate_image_artifact(&disk_mismatch, None, &config.virtual_machine_base)?,
            None
        );
        Ok(())
    }

    fn vm_candidate(
        provider_binding: &str,
        storage_class_binding: &str,
        base_disk: &VirtualMachineBaseDisk,
    ) -> Result<EnvironmentCandidate, Box<dyn std::error::Error>> {
        let candidate: EnvironmentCandidate = serde_json::from_value(serde_json::json!({
            "id": contracts::CandidateId::new(),
            "runId": AgentRunId::new(),
            "projectId": ProjectId::new(),
            "courseId": CourseId::new(),
            "revision": 1,
            "spec": {
                "apiVersion": "environment.labweaver.io/v1",
                "kind": "EnvironmentSpec",
                "name": "test-vm",
                "class": "experiment",
                "resources": {
                    "cpuMillicores": 1000,
                    "memoryBytes": 2_147_483_648_u64,
                    "storageBytes": 10_737_418_240_u64
                },
                "network": {"mode": "deny_all"},
                "entries": [{"name": "ssh", "protocol": "ssh", "servicePort": 22}],
                "security": {
                    "userPolicy": "non_root_required",
                    "rootFilesystemPolicy": "mutable_required",
                    "privilegeEscalationPolicy": "deny",
                    "publicExposurePolicy": "deny",
                    "securityProfileBinding": "restricted-v1"
                },
                "runtime": {
                    "kind": "virtual_machine",
                    "provider_binding": provider_binding,
                    "base_disk": base_disk,
                    "storage_class_binding": storage_class_binding,
                    "ssh_port": 22
                },
                "retention": {
                    "policyId": PolicyId::new(),
                    "policyRevision": 1,
                    "class": "run_evidence",
                    "retainUntil": "2099-01-01T00:00:00.000Z",
                    "disposition": "delete"
                }
            },
            "policyRevision": 1,
            "model": "test-model",
            "createdAt": "2026-09-08T00:00:00.000Z"
        }))?;
        candidate.validate()?;
        Ok(candidate)
    }
}
