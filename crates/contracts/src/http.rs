//! Infrastructure-neutral REST, SSE, concurrency, and security contract catalog.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    AccessGrantId, ActorId, AgentRunId, ApprovalId, ArtifactRef, BuildRequestId, CandidateId,
    CourseId, DiagnosticCode, EndpointId, EnvironmentId, EvaluationReleaseId, EvaluationRunId,
    EvaluationStepRunId, EventId, FrozenSubmissionId, ImageArtifactId, LeaseId, OperationId,
    PlatformRole, ProblemPackageId, ProjectId, ReleaseId, ResourceRequestId, Revision,
    StreamSequence, TaskRunId, UploadSessionId, UtcTimestamp,
};

pub const IDEMPOTENCY_KEY_HEADER: &str = "Idempotency-Key";
pub const IF_MATCH_HEADER: &str = "If-Match";
pub const ETAG_HEADER: &str = "ETag";
pub const LAST_EVENT_ID_HEADER: &str = "Last-Event-ID";

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationAccepted {
    pub operation_id: OperationId,
    pub revision: Revision,
    pub status_url: String,
}

/// Access-owned membership grant for one Project actor.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AddProjectMembershipRequest {
    pub actor_id: ActorId,
    pub role: PlatformRole,
    pub expires_at: Option<UtcTimestamp>,
}

/// Revision-fenced removal of one Project membership.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoveProjectMembershipRequest {
    pub expected_revision: Revision,
    pub reason: String,
}

/// Environment-specific accepted response. Existing OperationAccepted fields keep their v1
/// meaning while `environmentId` removes the need to parse an undeclared status URL.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentOperationAccepted {
    pub operation_id: OperationId,
    pub revision: Revision,
    pub status_url: String,
    pub environment_id: EnvironmentId,
}

/// Browser request for a new Project-scoped capacity reservation.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateResourceRequest {
    /// Required project scope. Work capacity is never allocated at an
    /// unscoped course-wide boundary.
    pub project_id: ProjectId,
    /// Optional teaching association. Independent research requests omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub course_id: Option<CourseId>,
    pub request_key: String,
    pub target: crate::resource::ResourceTarget,
    pub resources: crate::resource::WorkloadResources,
    pub duration_seconds: u64,
}

/// Explicit one-shot task identity allocated by the task owner before capacity is requested.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateTaskResourceTarget {
    pub task_run_id: TaskRunId,
}

/// Evaluation-owned request for a new one-shot task resource reservation.
///
/// Resource derives the requester and task target from this immutable command in one database
/// transaction. The route is service-authenticated and never accepts a browser delegation.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InternalCreateTaskResourceRequest {
    pub task_run_id: TaskRunId,
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub owner_id: ActorId,
    pub request_key: String,
    pub resources: crate::resource::WorkloadResources,
    pub duration_seconds: u64,
}

/// PostgreSQL-authoritative one-shot task resource state returned to Evaluation.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskResourceStatus {
    pub task_run_id: TaskRunId,
    pub project_id: ProjectId,
    pub owner_id: ActorId,
    /// Namespace selected by the trusted task scheduler at handoff time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_namespace: Option<String>,
    pub claim_revision: Revision,
    pub lease_revision: Revision,
    pub cleanup_confirmed: bool,
    pub request: crate::resource::ResourceRequest,
    pub claim: crate::resource::CapacityClaim,
    pub lease: crate::resource::ResourceLease,
}

/// Revision fence for acknowledging a task resource handoff.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AcknowledgeTaskResourceRequest {
    pub expected_claim_revision: Revision,
    pub expected_lease_revision: Revision,
    /// Exact provider namespace in which this task will execute.
    pub execution_namespace: String,
}

/// Revision fence for releasing a task resource reservation.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleaseTaskResourceRequest {
    pub expected_claim_revision: Revision,
    pub expected_lease_revision: Revision,
}

/// Provider usage submitted by a trusted Resource meter or reconciler.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecordResourceUsageRequest {
    pub project_id: ProjectId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub course_id: Option<CourseId>,
    pub kind: crate::resource::ResourceUsageKind,
    pub request_id: ResourceRequestId,
    pub lease_id: Option<LeaseId>,
    pub source_event_id: crate::EventId,
    pub measured_from: UtcTimestamp,
    pub measured_until: UtcTimestamp,
    pub measurement: crate::resource::UsageMeasurement,
}

/// Administrator-created immutable rate version.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateResourceRateRequest {
    pub unit: crate::resource::ResourceBillingUnit,
    /// Number of base usage units represented by `unit_price`.
    pub unit_quantity: u64,
    pub gpu_class: Option<String>,
    pub gpu_mode: Option<crate::resource::GpuAllocationMode>,
    pub unit_price: crate::resource::Money,
    pub effective_from: UtcTimestamp,
    pub effective_until: Option<UtcTimestamp>,
}

/// Project budget mutation. Amounts are decimal strings with six fractional digits.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpsertResourceBudgetRequest {
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub limit: crate::resource::Money,
    pub warning_at: crate::resource::Money,
}

/// Administrator adjustment appended to an existing calculated charge.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateResourceAdjustmentRequest {
    pub amount: crate::resource::Money,
    pub reason: String,
}

/// Administrator approval or resize decision. The selected Provider is always explicit.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApproveResourceRequest {
    pub expected_revision: Revision,
    pub provider_binding: String,
    pub resources: crate::resource::WorkloadResources,
    pub duration_seconds: u64,
    pub reason: String,
}

/// Revision-fenced reason-bearing Resource mutation.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceRequestMutation {
    pub expected_revision: Revision,
    pub reason: String,
}

/// Revision-fenced Lease renewal. Resources and Provider binding are immutable after approval.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RenewResourceLease {
    pub expected_revision: Revision,
    pub duration_seconds: u64,
    pub reason: String,
}

/// Stable response returned when Resource accepts an asynchronous allocation mutation.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceOperationAccepted {
    pub request_id: ResourceRequestId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_id: Option<LeaseId>,
    pub revision: Revision,
    pub status_url: String,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProblemPackageUploadFile {
    pub path: String,
    pub size_bytes: u64,
    pub media_type: String,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateProblemPackageUploadRequest {
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub files: Vec<ProblemPackageUploadFile>,
    pub retention_policy_revision: Revision,
}

/// Short-lived, per-object upload authority returned only by session creation.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProblemPackageUploadTarget {
    pub path: String,
    pub upload_url: String,
    pub required_headers: BTreeMap<String, String>,
    pub expires_at: UtcTimestamp,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProblemPackageUploadSession {
    pub id: UploadSessionId,
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub revision: Revision,
    pub files: Vec<ProblemPackageUploadFile>,
    pub upload_targets: Vec<ProblemPackageUploadTarget>,
    pub expires_at: UtcTimestamp,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompleteProblemPackageUploadRequest {}

/// One teacher command for approving an immutable Environment/Evaluation authoring package.
///
/// The selected artifact is checked against Control's authoritative build projection before the
/// command is persisted. The HTTP idempotency key remains the `Idempotency-Key` header.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompleteAuthoringApprovalRequest {
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub package_id: ProblemPackageId,
    pub package_revision: Revision,
    pub environment_candidate_id: CandidateId,
    pub environment_candidate_revision: Revision,
    pub evaluation_candidate_id: CandidateId,
    pub evaluation_candidate_revision: Revision,
    pub image_artifact: crate::supply_chain::ImageArtifact,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateAgentRunRequest {
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub package_id: ProblemPackageId,
    pub package_revision: Revision,
    pub policy_id: crate::PolicyId,
    pub policy_revision: Revision,
    pub environment_class: crate::authoring::EnvironmentClass,
}

/// Public request for a configuration run against one existing Resource-managed Work environment.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateWorkConfigurationRunRequest {
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub package_id: ProblemPackageId,
    pub package_revision: Revision,
    pub policy_id: crate::PolicyId,
    pub policy_revision: Revision,
    pub environment_id: EnvironmentId,
    pub environment_revision: Revision,
    pub preauthorization_id: Option<crate::WorkConfigurationPreauthorizationId>,
    pub preauthorization_revision: Option<Revision>,
}

/// Public teacher read model for reviewing one generated Work configuration plan.
///
/// Control resolves the exact script artifacts bound to the plan and returns their UTF-8
/// contents alongside the immutable plan metadata. Artifact references remain part of the plan
/// for identity checking, while the content fields make the approval decision reviewable without
/// exposing object-store credentials or URLs.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkConfigurationPlanView {
    pub plan: crate::authoring::WorkConfigurationPlan,
    pub script_content: String,
    pub verification_script_content: Option<String>,
}

/// Public command for approving one exact generated Work configuration plan.
///
/// Control resolves the actor, project, run, plan, and bound artifacts from its authoritative
/// state. The request therefore carries only the revision fences and the approval decision; it
/// cannot substitute a plan, artifact, environment, or caller identity.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApproveWorkConfigurationRequest {
    pub expected_run_revision: Revision,
    pub expected_plan_revision: Revision,
    pub environment_revision: Revision,
    pub expires_at: UtcTimestamp,
    pub reason: String,
    pub restart_confirmed: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CandidateDecisionRequest {
    pub candidate_revision: Revision,
    pub policy_revision: Revision,
    pub trust_revision: Revision,
    pub decision: crate::authoring::CandidateDecision,
    pub reason: String,
}

/// Public state of the deterministic Container build attached to one approved candidate.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateBuildState {
    Requested,
    Succeeded,
    Failed,
    Cancelled,
}

/// Control-owned build projection exposed for teacher review without leaking object keys or
/// executor internals.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CandidateBuildView {
    pub state: CandidateBuildState,
    pub artifact: Option<crate::supply_chain::ImageArtifact>,
    pub diagnostic_code: Option<DiagnosticCode>,
    pub cleanup_verified: Option<bool>,
}

/// Control-owned teacher read model for one immutable Environment candidate.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentCandidateView {
    pub candidate: crate::authoring::EnvironmentCandidate,
    pub approvals: Vec<crate::authoring::CandidateApproval>,
    pub build: Option<CandidateBuildView>,
    /// Runtime artifact resolved by Control from authoritative build or VM policy state.
    ///
    /// This is the exact artifact a teacher can approve. It remains null while a
    /// container build is incomplete or when a candidate does not match the
    /// deployment-owned VM base policy.
    pub image_artifact: Option<crate::supply_chain::ImageArtifact>,
    pub trust_revision: Revision,
}

/// Control-owned teacher read model for one immutable Evaluation candidate.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvaluationCandidateView {
    pub candidate: crate::authoring::EvaluationCandidate,
    pub approvals: Vec<crate::authoring::CandidateApproval>,
    pub trust_revision: Revision,
}

/// Public teacher command for publishing an exact approved Evaluation candidate.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateEvaluationReleaseRequest {
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub candidate_id: CandidateId,
    pub candidate_revision: Revision,
    pub approval_id: ApprovalId,
}

/// Revision-fenced append-only Evaluation release withdrawal.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WithdrawEvaluationReleaseRequest {
    pub expected_revision: Revision,
    pub reason_code: DiagnosticCode,
}

/// Bounded newest-first Evaluation release list query.
#[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvaluationReleaseListQuery {
    pub cursor: Option<String>,
    pub limit: Option<u16>,
}

impl EvaluationReleaseListQuery {
    pub fn validate(&self) -> Result<(), HttpContractError> {
        validate_cursor_page(self.cursor.as_deref(), self.limit)
    }
}

/// Control-to-Evaluation withdrawal command carried only over mTLS.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InternalWithdrawEvaluationReleaseRequest {
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub expected_revision: Revision,
    pub withdrawn_by: ActorId,
    pub reason_code: DiagnosticCode,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateEnvironmentTemplateReleaseRequest {
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub candidate_id: CandidateId,
    pub candidate_revision: Revision,
    pub runtime_kind: crate::authoring::RuntimeKind,
    pub approval_id: ApprovalId,
}

/// Append-only reason supplied when withdrawing an immutable release.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WithdrawEnvironmentTemplateReleaseRequest {
    /// Stable reviewed reason code; free-form sensitive text is not accepted.
    pub reason_code: String,
}

/// Control-to-Agent command carried only over an allowlisted mTLS service identity.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(
    tag = "kind",
    content = "request",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum InternalAgentRunRequest {
    Authoring(CreateAgentRunRequest),
    WorkConfiguration(CreateWorkConfigurationRunRequest),
}

/// Control-to-Agent command carried only over an allowlisted mTLS service identity.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InternalCreateAgentRunRequest {
    /// Authoritative Project ownership and optional teaching association.
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    /// Public immutable request whose idempotency key remains an HTTP header.
    pub request: InternalAgentRunRequest,
    /// Control-derived immutable purpose. Agent rejects a request whose purpose does not agree
    /// with its typed request variant and target identities.
    pub purpose: crate::authoring::AgentRunPurpose,
    /// Completed Control-owned package; Agent re-reads and re-hashes every object.
    pub package: crate::authoring::ProblemPackage,
    /// Internal artifact ID to opaque object-key mapping; keys never contain original paths.
    pub object_locators: BTreeMap<crate::ArtifactId, String>,
    /// Active immutable Project policy.
    pub policy: crate::authoring::ProjectLlmEgressPolicy,
    /// Optional concrete grant for reusing one exact approved Work configuration plan.
    pub preauthorization: Option<crate::authoring::WorkConfigurationPreauthorization>,
}

impl InternalCreateAgentRunRequest {
    /// Validates that every immutable input in the dispatch shares one project context.
    pub fn validate(&self) -> Result<(), HttpContractError> {
        let request_identity = match (&self.request, self.purpose) {
            (
                InternalAgentRunRequest::Authoring(request),
                crate::authoring::AgentRunPurpose::Authoring { environment_class },
            ) if request.project_id == self.project_id
                && request.course_id == self.course_id
                && request.environment_class == environment_class =>
            {
                (
                    request.project_id,
                    request.course_id,
                    request.package_id,
                    request.policy_id,
                    request.policy_revision,
                )
            }
            (
                InternalAgentRunRequest::WorkConfiguration(request),
                crate::authoring::AgentRunPurpose::WorkConfiguration {
                    environment_id,
                    environment_revision,
                    ..
                },
            ) if request.project_id == self.project_id
                && request.course_id == self.course_id
                && request.environment_id == environment_id
                && request.environment_revision == environment_revision
                && match (
                    request.preauthorization_id,
                    request.preauthorization_revision,
                    self.preauthorization.as_ref(),
                ) {
                    (Some(id), Some(revision), Some(grant)) => {
                        grant.id == id
                            && grant.revision == revision
                            && grant.project_id == self.project_id
                            && grant.environment_id == environment_id
                            && grant.environment_revision == environment_revision
                    }
                    (None, None, None) => true,
                    _ => false,
                } =>
            {
                (
                    request.project_id,
                    request.course_id,
                    request.package_id,
                    request.policy_id,
                    request.policy_revision,
                )
            }
            _ => return Err(HttpContractError::InvalidInternalIdentity),
        };
        if request_identity.0 != self.project_id
            || request_identity.1 != self.course_id
            || self.package.project_id != self.project_id
            || self.package.course_id != self.course_id
            || self.policy.project_id != self.project_id
            || self.policy.course_id != self.course_id
            || request_identity.2 != self.package.id
            || request_identity.4 != self.policy.revision
            || request_identity.3 != self.policy.id
        {
            return Err(HttpContractError::InvalidInternalIdentity);
        }
        self.package
            .validate()
            .map_err(|_| HttpContractError::InvalidInternalIdentity)?;
        self.policy
            .validate()
            .map_err(|_| HttpContractError::InvalidInternalIdentity)?;
        Ok(())
    }
}

/// Revision precondition for Control-to-Agent cancellation and retry mutations.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InternalAgentRunMutationRequest {
    /// Exact Project authority that owns the target run.
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    /// Exact Agent-owned run revision observed by Control.
    pub expected_revision: Revision,
}

/// One UTF-8 submission file included in a bounded internal advisory review request.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentLlmReviewFile {
    pub path: String,
    /// SHA-256 of the exact UTF-8 bytes in `content`, verified by Agent before provider egress.
    pub sha256: String,
    pub content: String,
}

/// The immutable rubric object and its exact UTF-8 content for one advisory review.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentLlmReviewRubric {
    pub artifact: ArtifactRef,
    pub path: String,
    /// SHA-256 of the exact UTF-8 bytes in `content`, verified by Agent before provider egress.
    pub sha256: String,
    pub content: String,
}

/// Evaluation-to-Agent request for one bounded, advisory-only LLM review.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InternalAgentLlmReviewRequest {
    pub task_run_id: TaskRunId,
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub frozen_submission_id: FrozenSubmissionId,
    pub submission_artifact: ArtifactRef,
    pub policy: crate::authoring::ProjectLlmEgressPolicy,
    pub files: Vec<AgentLlmReviewFile>,
    pub rubric: AgentLlmReviewRubric,
    pub deadline_at: UtcTimestamp,
}

impl InternalAgentLlmReviewRequest {
    /// Maximum number of submission files carried by one review request.
    pub const MAX_FILES: usize = 128;
    /// Maximum UTF-8 content bytes sent to the LLM provider in one request.
    pub const MAX_INPUT_BYTES: usize = 1024 * 1024;

    /// Validates transport-safe paths, bounded input, immutable artifact metadata, and policy
    /// ownership. Agent separately hashes the supplied UTF-8 bytes before any provider egress.
    pub fn validate(&self) -> Result<(), HttpContractError> {
        if self.files.is_empty() || self.files.len() > Self::MAX_FILES {
            return Err(HttpContractError::InvalidAgentLlmReview);
        }
        self.policy
            .validate()
            .map_err(|_| HttpContractError::InvalidAgentLlmReview)?;
        self.policy
            .validate_ownership(self.project_id, self.course_id)
            .map_err(|_| HttpContractError::InvalidAgentLlmReview)?;
        validate_artifact_ref(&self.submission_artifact)?;
        validate_review_path_and_hash(&self.rubric.path, &self.rubric.sha256)?;
        validate_artifact_ref(&self.rubric.artifact)?;

        let mut total_input_bytes = self.rubric.content.len();
        let mut paths = BTreeSet::new();
        for file in &self.files {
            validate_review_path_and_hash(&file.path, &file.sha256)?;
            if !paths.insert(file.path.as_str()) {
                return Err(HttpContractError::InvalidAgentLlmReview);
            }
            total_input_bytes = total_input_bytes.saturating_add(file.content.len());
        }
        if total_input_bytes > Self::MAX_INPUT_BYTES {
            return Err(HttpContractError::InvalidAgentLlmReview);
        }
        Ok(())
    }
}

/// Durable lifecycle of one internal advisory LLM review.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentLlmReviewState {
    Queued,
    Running,
    Cancelling,
    Succeeded,
    Failed,
    Cancelled,
}

/// Agent receipt for one advisory LLM review request.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InternalAgentLlmReviewReceipt {
    pub task_run_id: TaskRunId,
    pub request_sha256: String,
    pub state: AgentLlmReviewState,
    /// Advisory review output. It is present only for `succeeded` receipts and has no score.
    pub review: Option<crate::evaluation::GoalReview>,
    /// Provider usage is absent when the provider did not expose a trustworthy usage envelope.
    pub usage: Option<crate::authoring::LlmUsage>,
    pub diagnostic_code: Option<String>,
    pub started_at: Option<UtcTimestamp>,
    pub finished_at: Option<UtcTimestamp>,
}

/// Exact project scope used to read or cancel one Agent-owned advisory review.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentLlmReviewQuery {
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
}

impl InternalAgentLlmReviewReceipt {
    /// Validates hash shape and lifecycle-dependent review and finish-time invariants.
    pub fn validate(&self) -> Result<(), HttpContractError> {
        if !is_sha256(&self.request_sha256)
            || (self.state == AgentLlmReviewState::Succeeded) != self.review.is_some()
            || (self.state != AgentLlmReviewState::Succeeded && self.review.is_some())
            || (matches!(
                self.state,
                AgentLlmReviewState::Succeeded
                    | AgentLlmReviewState::Failed
                    | AgentLlmReviewState::Cancelled
            ) && self.finished_at.is_none())
        {
            return Err(HttpContractError::InvalidAgentLlmReview);
        }
        if let Some(review) = &self.review {
            review
                .validate()
                .map_err(|_| HttpContractError::InvalidAgentLlmReview)?;
        }
        Ok(())
    }
}

fn validate_review_path_and_hash(path: &str, sha256: &str) -> Result<(), HttpContractError> {
    crate::validate_relative_path(path).map_err(|_| HttpContractError::InvalidAgentLlmReview)?;
    if !is_sha256(sha256) {
        return Err(HttpContractError::InvalidAgentLlmReview);
    }
    Ok(())
}

fn validate_artifact_ref(reference: &ArtifactRef) -> Result<(), HttpContractError> {
    if reference.store_binding.trim().is_empty()
        || reference.object_version.trim().is_empty()
        || reference.media_type.trim().is_empty()
        || reference.size_bytes == 0
    {
        return Err(HttpContractError::InvalidAgentLlmReview);
    }
    Ok(())
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn valid_source_identity(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value.trim() == value
        && !value
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
}

/// Exact scope used when an internal caller resolves one Agent-owned generated artifact.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GeneratedArtifactQuery {
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub package_id: crate::ProblemPackageId,
    pub package_revision: Revision,
}

/// Agent-owned immutable generated-artifact metadata returned to a trusted service caller.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GeneratedArtifactRecord {
    pub artifact: crate::ArtifactRef,
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub package_id: crate::ProblemPackageId,
    pub package_revision: Revision,
    pub kind: GeneratedArtifactKind,
    pub object_key: String,
    pub content_sha256: String,
}

/// Immutable output class recorded by Agent for generated objects.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GeneratedArtifactKind {
    BuildContext,
    WorkScript,
    VerificationScript,
}

/// Control-to-Agent command that binds one exact approved Work configuration grant to an
/// AgentRun that is waiting for approval.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InternalApproveWorkConfigurationRequest {
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub expected_run_revision: Revision,
    pub preauthorization: crate::authoring::WorkConfigurationPreauthorization,
}

/// Agent-owned durable state used as an optimistic precondition for build cancellation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InternalAgentBuildState {
    Requested,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

/// Control-to-Agent build cancellation command authenticated by the service JWT boundary.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InternalAgentBuildCancellationRequest {
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub build_request_id: BuildRequestId,
    pub expected_state: InternalAgentBuildState,
    pub expected_revision: Revision,
    pub actor_id: ActorId,
    pub requested_at: UtcTimestamp,
}

/// Scope and immutable identity required to read one Agent-owned build status.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InternalAgentBuildStatusQuery {
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
}

/// Durable result returned for an accepted or exactly replayed build cancellation.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InternalAgentBuildCancellationResult {
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub build_request_id: BuildRequestId,
    pub state: InternalAgentBuildState,
    pub revision: Revision,
    pub cancellation_requested: bool,
}

/// Terminal or in-progress Agent result used to rebuild Control projections after replay.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InternalAgentRunOutcome {
    /// Authoritative aggregate.
    pub run: crate::authoring::AgentRun,
    /// Validated Environment candidate, if that track succeeded.
    pub environment_candidate: Option<crate::authoring::EnvironmentCandidate>,
    /// Validated Evaluation candidate, if that track succeeded.
    pub evaluation_candidate: Option<crate::authoring::EvaluationCandidate>,
    /// Generated Work configuration plan, if this run has the WorkConfiguration purpose.
    pub plan: Option<crate::authoring::WorkConfigurationPlan>,
}

impl InternalAgentRunOutcome {
    /// Validates parent identities.
    pub fn validate(&self) -> Result<(), HttpContractError> {
        if self
            .environment_candidate
            .as_ref()
            .is_some_and(|candidate| {
                candidate.run_id != self.run.id
                    || candidate.project_id != self.run.project_id
                    || candidate.course_id != self.run.course_id
            })
            || self.evaluation_candidate.as_ref().is_some_and(|candidate| {
                candidate.run_id != self.run.id
                    || candidate.project_id != self.run.project_id
                    || candidate.course_id != self.run.course_id
            })
            || self.plan != self.run.plan
        {
            return Err(HttpContractError::InvalidInternalIdentity);
        }
        if let Some(plan) = &self.plan {
            plan.validate()
                .map_err(|_| HttpContractError::InvalidInternalIdentity)?;
            let Some((environment_id, environment_revision, _)) =
                self.run.purpose.work_environment()
            else {
                return Err(HttpContractError::InvalidInternalIdentity);
            };
            if plan.environment_id != environment_id
                || plan.environment_revision != environment_revision
            {
                return Err(HttpContractError::InvalidInternalIdentity);
            }
        }
        Ok(())
    }
}

/// Agent-owned artifact and policy evaluation resolved for Control publication.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InternalImageArtifactResolution {
    pub artifact_id: ImageArtifactId,
    pub artifact: crate::supply_chain::ImageArtifact,
}

impl InternalImageArtifactResolution {
    /// Verifies exact artifact ID.
    pub fn validate(&self) -> Result<(), HttpContractError> {
        self.artifact
            .validate()
            .map_err(|_| HttpContractError::InvalidInternalIdentity)?;
        if self.artifact_id != self.artifact.id() {
            return Err(HttpContractError::InvalidInternalIdentity);
        }
        Ok(())
    }
}

/// Project identity and release revisions required before Evaluation can publish an
/// authoring-approved release.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthoringPublicationAdmissionBinding {
    pub approval_id: ApprovalId,
    pub approval_revision: Revision,
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub environment_release_id: ReleaseId,
    pub environment_release_version: u64,
    pub evaluation_release_id: EvaluationReleaseId,
    pub evaluation_release_revision: Revision,
}

/// Exact project and publication revisions used to read an Evaluation admission binding.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthoringPublicationAdmissionQuery {
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub approval_revision: Revision,
    pub evaluation_release_id: EvaluationReleaseId,
}

/// Exact Work environment and actor revisions used to resolve a Control admission binding.
///
/// The run revision is included so the returned binding cannot be silently substituted after the
/// caller observed a different immutable AgentRun.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkConfigurationAdmissionQuery {
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub environment_id: EnvironmentId,
    pub environment_revision: Revision,
    pub actor_id: ActorId,
    pub run_revision: Revision,
    /// `None` starts a new Environment execution; `Some` resumes this persisted VM execution.
    #[schemars(with = "Option<String>")]
    pub execution_id: Option<uuid::Uuid>,
}

/// Exact persisted VM execution identity returned with a Work admission binding during recovery.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkConfigurationRecoveryIdentity {
    #[schemars(with = "String")]
    pub execution_id: uuid::Uuid,
    pub plan_id: crate::WorkConfigurationPlanId,
    pub plan_revision: Revision,
    /// Persistent VM target UID that Environment must match before issuing a certificate.
    pub source_identity: String,
}

impl WorkConfigurationRecoveryIdentity {
    pub fn validate(&self) -> Result<(), HttpContractError> {
        if self.plan_revision.get() == 0 || !valid_source_identity(&self.source_identity) {
            return Err(HttpContractError::InvalidInternalIdentity);
        }
        Ok(())
    }
}

/// Control-owned Work configuration admission facts returned to a trusted downstream consumer.
///
/// The optional plan and preauthorization are retained as their complete immutable contracts so the
/// consumer can enforce exact artifact and expiry bindings without reconstructing Agent state.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkConfigurationAdmissionBinding {
    pub run_id: AgentRunId,
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub environment_id: EnvironmentId,
    pub environment_revision: Revision,
    pub actor_id: ActorId,
    pub run_revision: Revision,
    pub state: crate::authoring::AgentRunState,
    pub plan: Option<crate::authoring::WorkConfigurationPlan>,
    pub preauthorization: Option<crate::authoring::WorkConfigurationPreauthorization>,
    /// Present only when the admission is resuming an existing persisted VM execution.
    pub recovery: Option<WorkConfigurationRecoveryIdentity>,
    /// SHA-256 digest of the exact script bytes returned by the plan endpoint.
    pub script_sha256: String,
    /// SHA-256 digest of the optional verification script bytes.
    pub verification_script_sha256: Option<String>,
}

impl WorkConfigurationAdmissionBinding {
    /// Enforces the new-execution versus exact persisted-execution boundary.
    pub fn validate_recovery_for(
        &self,
        execution_id: Option<uuid::Uuid>,
    ) -> Result<(), HttpContractError> {
        match (execution_id, &self.recovery) {
            (None, None) => Ok(()),
            (Some(expected), Some(recovery)) if recovery.execution_id == expected => {
                recovery.validate()?;
                if self.plan.as_ref().is_some_and(|plan| {
                    plan.id != recovery.plan_id || plan.revision != recovery.plan_revision
                }) {
                    return Err(HttpContractError::InvalidInternalIdentity);
                }
                Ok(())
            }
            _ => Err(HttpContractError::InvalidInternalIdentity),
        }
    }
}

/// Lifecycle of one Environment-owned Work configuration execution.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainerWorkExecutionState {
    Running,
    Succeeded,
    Failed,
    Cancelling,
    Cancelled,
    CleanupPending,
    CleanupFailed,
}

/// Agent request for one bounded execution in an existing container Work environment.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContainerWorkExecutionRequest {
    pub run_id: AgentRunId,
    pub run_revision: Revision,
    pub plan_id: crate::WorkConfigurationPlanId,
    pub plan_revision: Revision,
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub environment_id: EnvironmentId,
    pub environment_revision: Revision,
    pub actor_id: ActorId,
    pub script_content: String,
    pub verification_script_content: Option<String>,
    pub deadline_at: UtcTimestamp,
}

impl ContainerWorkExecutionRequest {
    pub const MAX_SCRIPT_BYTES: usize = 512 * 1024;

    pub fn validate(&self) -> Result<(), HttpContractError> {
        if self.run_revision.get() == 0
            || self.plan_revision.get() == 0
            || self.environment_revision.get() == 0
            || self.script_content.is_empty()
            || self.script_content.len() > Self::MAX_SCRIPT_BYTES
            || self
                .verification_script_content
                .as_ref()
                .is_some_and(|value| value.is_empty() || value.len() > Self::MAX_SCRIPT_BYTES)
        {
            return Err(HttpContractError::InvalidContainerWorkExecution);
        }
        Ok(())
    }
}

/// Exact identity used to read an existing container Work execution.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContainerWorkExecutionQuery {
    pub project_id: ProjectId,
    pub environment_id: EnvironmentId,
    pub plan_id: crate::WorkConfigurationPlanId,
    pub plan_revision: Revision,
}

/// Read-only metadata for one Agent-owned persisted VM execution intent.
///
/// The contract intentionally excludes scripts, private keys, certificates, and provider
/// credentials. Agent remains the source of truth for the full private execution request.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentWorkExecutionIntentMetadata {
    #[schemars(with = "String")]
    pub execution_id: uuid::Uuid,
    pub run_id: AgentRunId,
    pub run_revision: Revision,
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub environment_id: EnvironmentId,
    pub environment_revision: Revision,
    pub actor_id: ActorId,
    pub plan_id: crate::WorkConfigurationPlanId,
    pub plan_revision: Revision,
    pub source_identity: String,
    pub script_sha256: String,
    pub verification_script_sha256: Option<String>,
}

impl AgentWorkExecutionIntentMetadata {
    pub fn validate(&self) -> Result<(), HttpContractError> {
        if self.run_revision.get() == 0
            || self.environment_revision.get() == 0
            || self.plan_revision.get() == 0
            || !valid_source_identity(&self.source_identity)
            || !is_sha256(&self.script_sha256)
            || self
                .verification_script_sha256
                .as_deref()
                .is_some_and(|value| !is_sha256(value))
        {
            return Err(HttpContractError::InvalidInternalIdentity);
        }
        Ok(())
    }
}

/// Scope query for the Agent-owned persisted VM execution intent endpoint.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentWorkExecutionIntentQuery {
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    #[schemars(with = "String")]
    pub execution_id: uuid::Uuid,
}

/// Environment receipt for one execution with bounded execution output.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContainerWorkExecutionReceipt {
    #[schemars(with = "String")]
    pub execution_id: uuid::Uuid,
    pub run_id: AgentRunId,
    pub plan_id: crate::WorkConfigurationPlanId,
    pub plan_revision: Revision,
    pub environment_id: EnvironmentId,
    pub environment_revision: Revision,
    pub target_pod_uid: String,
    pub state: ContainerWorkExecutionState,
    pub exit_code: Option<i32>,
    pub verification_exit_code: Option<i32>,
    pub output: String,
    pub output_truncated: bool,
    pub diagnostic_code: Option<DiagnosticCode>,
    pub started_at: Option<UtcTimestamp>,
    pub finished_at: Option<UtcTimestamp>,
}

impl ContainerWorkExecutionReceipt {
    pub const MAX_OUTPUT_BYTES: usize = 64 * 1024;

    pub fn validate(&self) -> Result<(), HttpContractError> {
        if self.target_pod_uid.trim().is_empty()
            || self.target_pod_uid.len() > 256
            || self.output.len() > Self::MAX_OUTPUT_BYTES
            || (matches!(
                self.state,
                ContainerWorkExecutionState::Succeeded
                    | ContainerWorkExecutionState::Failed
                    | ContainerWorkExecutionState::Cancelled
                    | ContainerWorkExecutionState::CleanupFailed
            ) && self.finished_at.is_none())
        {
            return Err(HttpContractError::InvalidContainerWorkExecution);
        }
        Ok(())
    }
}

/// Control-to-Evaluation command that publishes one approved immutable `EvaluationSpec`.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InternalPublishEvaluationReleaseRequest {
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub candidate_id: CandidateId,
    pub candidate_revision: Revision,
    pub approval_id: ApprovalId,
    pub approval_revision: Revision,
    pub evaluation_spec: crate::evaluation::EvaluationSpec,
    pub execution_binding: crate::evaluation::EvaluationExecutionBinding,
    pub runtime_identity: crate::evaluation::EvaluationRuntimeIdentity,
    pub published_by: ActorId,
}

impl InternalPublishEvaluationReleaseRequest {
    /// Validates the immutable release command before persistence.
    pub fn validate(&self) -> Result<(), HttpContractError> {
        self.evaluation_spec
            .validate()
            .map_err(|_| HttpContractError::InvalidEvaluationControl)?;
        self.execution_binding
            .validate()
            .map_err(|_| HttpContractError::InvalidEvaluationControl)?;
        self.execution_binding
            .validate_ownership(self.project_id, self.course_id)
            .map_err(|_| HttpContractError::InvalidInternalIdentity)?;
        self.runtime_identity
            .validate()
            .map_err(|_| HttpContractError::InvalidEvaluationControl)
    }
}

/// Control-to-Evaluation command that starts one run from a release and frozen submission.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InternalCreateEvaluationRunRequest {
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub release_id: EvaluationReleaseId,
    pub release_revision: Revision,
    pub frozen_submission_id: crate::FrozenSubmissionId,
    pub actor_id: ActorId,
    pub identity: crate::evaluation::EvaluationRunIdentity,
}

impl InternalCreateEvaluationRunRequest {
    /// Validates the immutable run command before persistence.
    pub fn validate(&self) -> Result<(), HttpContractError> {
        self.identity
            .validate()
            .map_err(|_| HttpContractError::InvalidEvaluationControl)
    }
}

/// Revision precondition for Control-to-Evaluation run mutations.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InternalEvaluationRunMutationRequest {
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub expected_revision: Revision,
    pub actor_id: ActorId,
}

/// Worker-to-Evaluation terminal result fenced by a StepRun attempt.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InternalCompleteEvaluationStepRequest {
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub run_id: EvaluationRunId,
    pub step_run_id: EvaluationStepRunId,
    pub attempt: u32,
    pub worker_id: String,
    pub runtime_identity: crate::evaluation::EvaluationRuntimeIdentity,
    pub lease_token: String,
    pub completion: crate::evaluation::EvaluationStepCompletion,
}

impl InternalCompleteEvaluationStepRequest {
    /// Validates the worker-supplied runtime binding before persistence fencing.
    pub fn validate(&self) -> Result<(), HttpContractError> {
        self.runtime_identity
            .validate()
            .map_err(|_| HttpContractError::InvalidEvaluationControl)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateEnvironmentRequest {
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub release_id: ReleaseId,
    pub release_version: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_label: Option<String>,
}

/// Browser request for one explicitly selected, immutable Environment reset target.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResetEnvironmentRequest {
    pub reset_target: crate::environment::EnvironmentResetTarget,
}

impl ResetEnvironmentRequest {
    pub fn validate(&self) -> Result<(), HttpContractError> {
        let valid = match &self.reset_target {
            crate::environment::EnvironmentResetTarget::ExperimentBaseline {
                release_version,
                ..
            } => *release_version > 0,
            crate::environment::EnvironmentResetTarget::WorkSnapshot { snapshot, .. } => {
                !snapshot.store_binding.trim().is_empty()
                    && !snapshot.object_version.trim().is_empty()
                    && snapshot.size_bytes > 0
                    && !snapshot.media_type.trim().is_empty()
            }
            crate::environment::EnvironmentResetTarget::WorkConfiguration { .. } => true,
        };
        valid
            .then_some(())
            .ok_or(HttpContractError::InvalidEnvironmentQuery)
    }
}

impl CreateEnvironmentRequest {
    pub fn validate(&self) -> Result<(), HttpContractError> {
        if self.release_version == 0
            || self.display_label.as_ref().is_some_and(|value| {
                value.trim().is_empty()
                    || value.chars().count() > 120
                    || value.chars().any(char::is_control)
            })
        {
            return Err(HttpContractError::InvalidEnvironmentQuery);
        }
        Ok(())
    }
}

pub use crate::environment::{
    EnvironmentOwnerResolution, EnvironmentOwnerResolutionRequest,
    EnvironmentWorkConfigurationTarget, EnvironmentWorkConfigurationTargetQuery,
};

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FreezeSubmissionRequest {
    /// Optional teaching association. Independent project work omits this field.
    pub course_id: Option<CourseId>,
    pub manifest: crate::submission::SubmissionManifest,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateSshPublicKeyRequest {
    pub public_key_openssh: String,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateAccessGrantRequest {
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub environment_id: EnvironmentId,
    pub environment_revision: Revision,
    pub endpoint_ids: Vec<EndpointId>,
    pub expires_at: Option<UtcTimestamp>,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RevokeAccessGrantRequest {
    pub grant_id: AccessGrantId,
    pub reason_code: String,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RenewAccessGrantRequest {
    pub grant_id: AccessGrantId,
    pub expires_at: UtcTimestamp,
}

/// Revision-fenced request to issue a single browser console capability.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IssueConsoleCapabilityRequest {
    pub kind: crate::access::ConsoleKind,
    pub expected_access_grant_revision: Revision,
    pub expected_environment_revision: Revision,
    pub expected_lease_fence: Option<crate::access::ConsoleLeaseFence>,
}

impl IssueConsoleCapabilityRequest {
    pub fn validate_against(
        &self,
        availability: &crate::access::ConsoleCapabilityAvailability,
    ) -> Result<(), HttpContractError> {
        availability
            .validate()
            .map_err(|_| HttpContractError::InvalidConsoleCapability)?;
        if self.expected_access_grant_revision != availability.access_grant_revision
            || self.expected_environment_revision != availability.environment_revision
            || self.expected_lease_fence != availability.lease_fence
            || !availability.kinds.contains(&self.kind)
        {
            return Err(HttpContractError::InvalidConsoleCapability);
        }
        Ok(())
    }

    /// `If-Match` is the AccessGrant revision fence, duplicated in the body only
    /// because the body is persisted as the idempotency fingerprint.
    pub fn validate_if_match(&self, if_match: &StrongEtag) -> Result<(), HttpContractError> {
        if if_match.revision() == self.expected_access_grant_revision {
            Ok(())
        } else {
            Err(HttpContractError::InvalidConsoleCapability)
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CursorPage<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
}

/// Cursor page bound to one consistent REST/SSE snapshot.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SnapshotPage<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
    pub snapshot_sequence: StreamSequence,
    pub snapshot_at: UtcTimestamp,
}

pub const DEFAULT_PAGE_LIMIT: u16 = 50;
pub const MAX_PAGE_LIMIT: u16 = 100;
pub const MAX_CURSOR_LENGTH: usize = 512;

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentInventoryQuery {
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub runtime_kind: Option<crate::authoring::RuntimeKind>,
    pub class: Option<crate::authoring::EnvironmentClass>,
    pub desired_state: Option<crate::environment::DesiredEnvironmentState>,
    pub observed_state: Option<crate::environment::ObservedEnvironmentState>,
    pub release_id: Option<ReleaseId>,
    pub cursor: Option<String>,
    pub limit: Option<u16>,
}

impl EnvironmentInventoryQuery {
    pub fn validate(&self) -> Result<(), HttpContractError> {
        validate_cursor_page(self.cursor.as_deref(), self.limit)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentOperationListQuery {
    pub kind: Option<crate::environment::EnvironmentOperationKind>,
    pub state: Option<crate::environment::OperationState>,
    pub cursor: Option<String>,
    pub limit: Option<u16>,
}

impl EnvironmentOperationListQuery {
    pub fn validate(&self) -> Result<(), HttpContractError> {
        validate_cursor_page(self.cursor.as_deref(), self.limit)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentAccessGrantListQuery {
    pub state: Option<crate::access::AccessGrantState>,
    pub endpoint_id: Option<EndpointId>,
    #[serde(default)]
    pub include_terminal: bool,
    pub cursor: Option<String>,
    pub limit: Option<u16>,
}

impl EnvironmentAccessGrantListQuery {
    pub fn validate(&self) -> Result<(), HttpContractError> {
        validate_cursor_page(self.cursor.as_deref(), self.limit)
    }
}

fn validate_cursor_page(cursor: Option<&str>, limit: Option<u16>) -> Result<(), HttpContractError> {
    if cursor.is_some_and(|value| {
        value.is_empty()
            || value.len() > MAX_CURSOR_LENGTH
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte))
    }) || limit.is_some_and(|value| value == 0 || value > MAX_PAGE_LIMIT)
    {
        return Err(HttpContractError::InvalidCursorPage);
    }
    Ok(())
}

/// Environment-management subset carried by the course event stream.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum EnvironmentManagementEvent {
    EnvironmentChanged {
        environment_id: EnvironmentId,
        revision: Revision,
        observed_state: crate::environment::ObservedEnvironmentState,
        operation_id: Option<OperationId>,
    },
    OperationChanged {
        environment_id: EnvironmentId,
        operation_id: OperationId,
        revision: Revision,
        state: crate::environment::OperationState,
    },
    AccessGrantChanged {
        environment_id: EnvironmentId,
        access_grant_id: AccessGrantId,
        revision: Revision,
        state: crate::access::AccessGrantState,
    },
}

/// Public event envelope used for course-scoped inventory synchronization.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentManagementStreamEvent {
    pub event_id: EventId,
    pub stream_sequence: StreamSequence,
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub effective_at: UtcTimestamp,
    pub data: EnvironmentManagementEvent,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IdempotencyKey(String);
impl IdempotencyKey {
    pub fn parse(value: &str) -> Result<Self, HttpContractError> {
        if !(8..=128).contains(&value.len())
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
        {
            return Err(HttpContractError::InvalidIdempotencyKey);
        }
        Ok(Self(value.to_owned()))
    }
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StrongEtag(Revision);
impl StrongEtag {
    #[must_use]
    pub fn from_revision(revision: Revision) -> Self {
        Self(revision)
    }
    #[must_use]
    pub fn header_value(&self) -> String {
        format!("\"rev-{}\"", self.0.get())
    }
    /// Returns the exact revision carried by this strong validator.
    #[must_use]
    pub const fn revision(&self) -> Revision {
        self.0
    }
    pub fn parse(value: &str) -> Result<Self, HttpContractError> {
        if value.starts_with("W/") {
            return Err(HttpContractError::WeakEtag);
        }
        let raw = value
            .strip_prefix("\"rev-")
            .and_then(|v| v.strip_suffix('"'))
            .ok_or(HttpContractError::InvalidEtag)?;
        let revision = raw
            .parse::<u64>()
            .map_err(|_| HttpContractError::InvalidEtag)?;
        Ok(Self(
            Revision::new(revision).map_err(|_| HttpContractError::InvalidEtag)?,
        ))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SseEvent<T> {
    pub cursor: StreamSequence,
    pub event: String,
    pub data: T,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SseResume {
    Beginning,
    After(StreamSequence),
}
pub fn resolve_sse_resume(
    last_event_id: Option<StreamSequence>,
    after: Option<StreamSequence>,
) -> Result<SseResume, HttpContractError> {
    match (last_event_id, after) {
        (None, None) => Ok(SseResume::Beginning),
        (Some(value), None) | (None, Some(value)) => Ok(SseResume::After(value)),
        (Some(header), Some(query)) if header == query => Ok(SseResume::After(header)),
        (Some(_), Some(_)) => Err(HttpContractError::ConflictingSseCursor),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApiSurface {
    Public,
    GatewayInternal,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Method {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MutationContract {
    None,
    IdempotentCreate,
    IdempotentRevisioned,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Security {
    Oidc,
    BffSession,
    ServiceJwt,
}

/// Scope input that an operation requires from the authorization boundary.
/// Resource ownership for environment scopes is resolved by the owning service
/// before it asks Access for a decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationScopeKind {
    /// Platform-global actor scope.
    Global,
    /// Course membership scope.
    Course,
    /// Project membership scope.
    Project,
    /// Environment scope supplied by its owning service.
    Environment,
    /// Registered internal service identity scope.
    Service,
}

/// Explicit authorization policy for a catalog operation is now co-located with
/// the transport contract so there is a single `OPERATIONS` table.
const TEACHER: &[PlatformRole] = &[PlatformRole::Teacher];
const TEACHER_OR_STUDENT: &[PlatformRole] = &[PlatformRole::Teacher, PlatformRole::Student];
const TEACHER_OR_ADMIN: &[PlatformRole] = &[PlatformRole::Teacher, PlatformRole::PlatformAdmin];
const PLATFORM_ADMIN: &[PlatformRole] = &[PlatformRole::PlatformAdmin];
const ALL_ROLES: &[PlatformRole] = &[
    PlatformRole::Teacher,
    PlatformRole::Student,
    PlatformRole::PlatformAdmin,
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OperationContract {
    pub surface: ApiSurface,
    pub method: Method,
    pub path: &'static str,
    pub operation_id: &'static str,
    pub permission: &'static str,
    pub security: Security,
    pub mutation: MutationContract,
    pub success_status: u16,
    pub timeout_milliseconds: u64,
    pub cancellable: bool,
    pub retryable: bool,
    pub allowed_roles: &'static [PlatformRole],
    pub scope: OperationScopeKind,
}

/// Looks up the merged transport + authorization contract for a stable operation id.
#[must_use]
pub fn operation_contract(operation_id: &str) -> Option<&'static OperationContract> {
    OPERATIONS.iter().find(|op| op.operation_id == operation_id)
}

macro_rules! op {
    ($surface:ident,$method:ident,$path:literal,$id:literal,$permission:literal,$security:ident,$mutation:ident,$status:literal,$cancel:literal,$retry:literal,$roles:expr,$scope:ident) => {
        OperationContract {
            surface: ApiSurface::$surface,
            method: Method::$method,
            path: $path,
            operation_id: $id,
            permission: $permission,
            security: Security::$security,
            mutation: MutationContract::$mutation,
            success_status: $status,
            timeout_milliseconds: 30_000,
            cancellable: $cancel,
            retryable: $retry,
            allowed_roles: $roles,
            scope: OperationScopeKind::$scope,
        }
    };
}

pub const OPERATIONS: &[OperationContract] = &[
    // Project is the cross-domain ownership root. The list and create operations
    // are actor-scoped by Access even though they do not carry a project path.
    op!(
        Public,
        Get,
        "/api/v1/projects",
        "listProjects",
        "project:read",
        Oidc,
        None,
        200,
        false,
        true,
        ALL_ROLES,
        Global
    ),
    op!(
        Public,
        Post,
        "/api/v1/projects",
        "createProject",
        "project:write",
        Oidc,
        IdempotentCreate,
        201,
        false,
        true,
        ALL_ROLES,
        Global
    ),
    op!(
        Public,
        Get,
        "/api/v1/projects/{projectId}",
        "getProject",
        "project:read",
        Oidc,
        None,
        200,
        false,
        true,
        ALL_ROLES,
        Project
    ),
    op!(
        Public,
        Patch,
        "/api/v1/projects/{projectId}",
        "updateProject",
        "project:write",
        Oidc,
        IdempotentRevisioned,
        200,
        false,
        true,
        TEACHER_OR_STUDENT,
        Project
    ),
    op!(
        Public,
        Post,
        "/api/v1/projects/{projectId}/archive",
        "archiveProject",
        "project:archive",
        Oidc,
        IdempotentRevisioned,
        200,
        false,
        true,
        TEACHER_OR_STUDENT,
        Project
    ),
    op!(
        Public,
        Get,
        "/api/v1/projects/{projectId}/members",
        "listProjectMemberships",
        "project_membership:read",
        Oidc,
        None,
        200,
        false,
        true,
        TEACHER_OR_STUDENT,
        Project
    ),
    op!(
        Public,
        Post,
        "/api/v1/projects/{projectId}/members",
        "addProjectMembership",
        "project_membership:write",
        Oidc,
        IdempotentCreate,
        201,
        false,
        true,
        TEACHER_OR_STUDENT,
        Project
    ),
    op!(
        Public,
        Delete,
        "/api/v1/projects/{projectId}/members/{actorId}",
        "removeProjectMembership",
        "project_membership:write",
        Oidc,
        IdempotentRevisioned,
        200,
        false,
        true,
        TEACHER_OR_STUDENT,
        Project
    ),
    op!(
        Public,
        Post,
        "/api/v1/projects/{projectId}/problem-package-uploads",
        "createProjectProblemPackageUpload",
        "problem_package:write",
        Oidc,
        IdempotentCreate,
        201,
        false,
        true,
        ALL_ROLES,
        Project
    ),
    op!(
        Public,
        Post,
        "/api/v1/projects/{projectId}/problem-package-uploads/{uploadId}/complete",
        "completeProjectProblemPackageUpload",
        "problem_package:write",
        Oidc,
        IdempotentRevisioned,
        201,
        false,
        true,
        ALL_ROLES,
        Project
    ),
    op!(
        Public,
        Get,
        "/api/v1/projects/{projectId}/problem-packages/{packageId}",
        "getProjectProblemPackage",
        "problem_package:read",
        Oidc,
        None,
        200,
        false,
        true,
        ALL_ROLES,
        Project
    ),
    op!(
        Public,
        Post,
        "/api/v1/projects/{projectId}/llm-egress-policies",
        "createProjectLlmPolicy",
        "llm_policy:write",
        Oidc,
        IdempotentCreate,
        201,
        false,
        true,
        ALL_ROLES,
        Project
    ),
    op!(
        Public,
        Get,
        "/api/v1/projects/{projectId}/llm-egress-policies/active",
        "getActiveProjectLlmPolicy",
        "llm_policy:read",
        Oidc,
        None,
        200,
        false,
        true,
        ALL_ROLES,
        Project
    ),
    op!(
        GatewayInternal,
        Get,
        "/internal/v1/projects/{projectId}/llm-egress-policy",
        "getInternalProjectLlmEgressPolicy",
        "control.llm_policy.read",
        ServiceJwt,
        None,
        200,
        false,
        true,
        PLATFORM_ADMIN,
        Service
    ),
    op!(
        Public,
        Post,
        "/api/v1/projects/{projectId}/agent-runs",
        "createProjectAgentRun",
        "agent_run:write",
        Oidc,
        IdempotentCreate,
        202,
        true,
        true,
        ALL_ROLES,
        Project
    ),
    op!(
        Public,
        Post,
        "/api/v1/projects/{projectId}/work-configuration-runs",
        "createProjectWorkConfigurationRun",
        "agent_run:write",
        Oidc,
        IdempotentCreate,
        202,
        true,
        true,
        ALL_ROLES,
        Project
    ),
    op!(
        Public,
        Post,
        "/api/v1/projects/{projectId}/agent-runs/{runId}/work-configuration/approve",
        "approveProjectWorkConfigurationRun",
        "agent_run:write",
        Oidc,
        IdempotentRevisioned,
        202,
        true,
        true,
        ALL_ROLES,
        Project
    ),
    op!(
        Public,
        Get,
        "/api/v1/projects/{projectId}/agent-runs/{runId}",
        "getProjectAgentRun",
        "agent_run:read",
        Oidc,
        None,
        200,
        false,
        true,
        ALL_ROLES,
        Project
    ),
    op!(
        Public,
        Get,
        "/api/v1/projects/{projectId}/agent-runs/{runId}/work-configuration/plan",
        "getProjectWorkConfigurationPlan",
        "agent_run:read",
        Oidc,
        None,
        200,
        false,
        true,
        ALL_ROLES,
        Project
    ),
    op!(
        Public,
        Post,
        "/api/v1/projects/{projectId}/agent-runs/{runId}/cancel",
        "cancelProjectAgentRun",
        "agent_run:write",
        Oidc,
        IdempotentRevisioned,
        202,
        false,
        true,
        ALL_ROLES,
        Project
    ),
    op!(
        Public,
        Post,
        "/api/v1/projects/{projectId}/agent-runs/{runId}/tracks/{track}/retry",
        "retryProjectAgentRunTrack",
        "agent_run:write",
        Oidc,
        IdempotentRevisioned,
        202,
        true,
        true,
        ALL_ROLES,
        Project
    ),
    op!(
        Public,
        Get,
        "/api/v1/projects/{projectId}/environment-candidates/{candidateId}",
        "getProjectEnvironmentCandidate",
        "candidate:read",
        Oidc,
        None,
        200,
        false,
        true,
        ALL_ROLES,
        Project
    ),
    op!(
        Public,
        Post,
        "/api/v1/projects/{projectId}/environment-candidates/{candidateId}/decisions",
        "appendProjectEnvironmentCandidateDecision",
        "candidate:approve",
        Oidc,
        IdempotentRevisioned,
        201,
        false,
        false,
        ALL_ROLES,
        Project
    ),
    op!(
        Public,
        Get,
        "/api/v1/projects/{projectId}/evaluation-candidates/{candidateId}",
        "getProjectEvaluationCandidate",
        "candidate:read",
        Oidc,
        None,
        200,
        false,
        true,
        ALL_ROLES,
        Project
    ),
    op!(
        Public,
        Post,
        "/api/v1/projects/{projectId}/evaluation-candidates/{candidateId}/decisions",
        "appendProjectEvaluationCandidateDecision",
        "candidate:approve",
        Oidc,
        IdempotentRevisioned,
        201,
        false,
        false,
        ALL_ROLES,
        Project
    ),
    op!(
        Public,
        Get,
        "/api/v1/projects/{projectId}/events",
        "streamProjectEvents",
        "events:read",
        Oidc,
        None,
        200,
        false,
        true,
        ALL_ROLES,
        Project
    ),
    op!(
        Public,
        Post,
        "/api/v1/projects/{projectId}/authoring-approvals",
        "completeProjectAuthoringApproval",
        "authoring:approve",
        Oidc,
        IdempotentCreate,
        201,
        false,
        false,
        TEACHER_OR_ADMIN,
        Project
    ),
    op!(
        Public,
        Get,
        "/api/v1/projects/{projectId}/authoring-approvals/{approvalId}",
        "getProjectAuthoringApproval",
        "authoring:read",
        Oidc,
        None,
        200,
        false,
        true,
        TEACHER_OR_ADMIN,
        Project
    ),
    op!(
        Public,
        Post,
        "/api/v1/resource-requests",
        "createResourceRequest",
        "resource_request:write",
        BffSession,
        IdempotentCreate,
        202,
        false,
        true,
        TEACHER_OR_STUDENT,
        Project
    ),
    op!(
        Public,
        Post,
        "/api/v1/projects/{projectId}/resource-requests",
        "createProjectResourceRequest",
        "resource_request:write",
        BffSession,
        IdempotentCreate,
        202,
        false,
        true,
        TEACHER_OR_STUDENT,
        Project
    ),
    op!(
        Public,
        Post,
        "/api/v1/resource-requests/{requestId}/resize-and-approve",
        "resizeAndApproveResourceRequest",
        "resource_request:approve",
        BffSession,
        IdempotentRevisioned,
        202,
        false,
        true,
        PLATFORM_ADMIN,
        Project
    ),
    op!(
        Public,
        Get,
        "/api/v1/resource-requests",
        "listResourceRequests",
        "resource_request:read",
        BffSession,
        None,
        200,
        false,
        true,
        PLATFORM_ADMIN,
        Global
    ),
    op!(
        Public,
        Get,
        "/api/v1/projects/{projectId}/resource-requests",
        "listProjectResourceRequests",
        "resource_request:read",
        BffSession,
        None,
        200,
        false,
        true,
        ALL_ROLES,
        Project
    ),
    op!(
        Public,
        Get,
        "/api/v1/resource-requests/{requestId}",
        "getResourceRequest",
        "resource_request:read",
        BffSession,
        None,
        200,
        false,
        true,
        ALL_ROLES,
        Project
    ),
    op!(
        Public,
        Post,
        "/api/v1/resource-requests/{requestId}/cancel",
        "cancelResourceRequest",
        "resource_request:cancel",
        BffSession,
        IdempotentRevisioned,
        202,
        false,
        true,
        TEACHER_OR_STUDENT,
        Project
    ),
    op!(
        Public,
        Post,
        "/api/v1/resource-requests/{requestId}/approve",
        "approveResourceRequest",
        "resource_request:approve",
        BffSession,
        IdempotentRevisioned,
        202,
        false,
        true,
        PLATFORM_ADMIN,
        Project
    ),
    op!(
        Public,
        Post,
        "/api/v1/resource-requests/{requestId}/reject",
        "rejectResourceRequest",
        "resource_request:approve",
        BffSession,
        IdempotentRevisioned,
        202,
        false,
        true,
        PLATFORM_ADMIN,
        Project
    ),
    op!(
        Public,
        Post,
        "/api/v1/resource-requests/{requestId}/retry",
        "retryResourceRequest",
        "resource_request:retry",
        BffSession,
        IdempotentRevisioned,
        202,
        false,
        true,
        PLATFORM_ADMIN,
        Project
    ),
    op!(
        Public,
        Get,
        "/api/v1/resource-leases",
        "listResourceLeases",
        "resource_lease:read",
        BffSession,
        None,
        200,
        false,
        true,
        PLATFORM_ADMIN,
        Global
    ),
    op!(
        Public,
        Get,
        "/api/v1/projects/{projectId}/resource-leases",
        "listProjectResourceLeases",
        "resource_lease:read",
        BffSession,
        None,
        200,
        false,
        true,
        ALL_ROLES,
        Project
    ),
    op!(
        Public,
        Get,
        "/api/v1/resource-leases/{leaseId}",
        "getResourceLease",
        "resource_lease:read",
        BffSession,
        None,
        200,
        false,
        true,
        ALL_ROLES,
        Project
    ),
    op!(
        Public,
        Post,
        "/api/v1/resource-leases/{leaseId}/renew",
        "renewResourceLease",
        "resource_lease:renew",
        BffSession,
        IdempotentRevisioned,
        200,
        false,
        true,
        TEACHER_OR_STUDENT,
        Project
    ),
    op!(
        Public,
        Post,
        "/api/v1/resource-leases/{leaseId}/revoke",
        "revokeResourceLease",
        "resource_lease:revoke",
        BffSession,
        IdempotentRevisioned,
        200,
        false,
        true,
        TEACHER_OR_STUDENT,
        Project
    ),
    op!(
        Public,
        Get,
        "/api/v1/resource/gpu-catalog",
        "listResourceGpuCatalog",
        "resource_gpu_catalog:read",
        BffSession,
        None,
        200,
        false,
        true,
        ALL_ROLES,
        Global
    ),
    op!(
        Public,
        Post,
        "/api/v1/resource/gpu-catalog",
        "createResourceGpuCatalogEntry",
        "resource_gpu_catalog:write",
        BffSession,
        IdempotentCreate,
        201,
        false,
        true,
        PLATFORM_ADMIN,
        Global
    ),
    op!(
        Public,
        Get,
        "/api/v1/resource/rates",
        "listResourceRates",
        "resource_rate:read",
        BffSession,
        None,
        200,
        false,
        true,
        ALL_ROLES,
        Global
    ),
    op!(
        Public,
        Post,
        "/api/v1/resource/rates",
        "createResourceRate",
        "resource_rate:write",
        BffSession,
        IdempotentCreate,
        201,
        false,
        true,
        PLATFORM_ADMIN,
        Global
    ),
    op!(
        Public,
        Get,
        "/api/v1/projects/{projectId}/resource-budget",
        "getProjectResourceBudget",
        "resource_budget:read",
        BffSession,
        None,
        200,
        false,
        true,
        PLATFORM_ADMIN,
        Project
    ),
    op!(
        Public,
        Put,
        "/api/v1/projects/{projectId}/resource-budget",
        "upsertProjectResourceBudget",
        "resource_budget:write",
        BffSession,
        IdempotentCreate,
        200,
        false,
        true,
        PLATFORM_ADMIN,
        Project
    ),
    op!(
        Public,
        Get,
        "/api/v1/projects/{projectId}/charges",
        "listProjectResourceCharges",
        "resource_charge:read",
        BffSession,
        None,
        200,
        false,
        true,
        PLATFORM_ADMIN,
        Project
    ),
    op!(
        Public,
        Post,
        "/api/v1/projects/{projectId}/charges/{chargeId}/adjustments",
        "createProjectResourceChargeAdjustment",
        "resource_charge:adjust",
        BffSession,
        IdempotentCreate,
        201,
        false,
        true,
        PLATFORM_ADMIN,
        Project
    ),
    op!(
        Public,
        Get,
        "/api/v1/access-grants/{grantId}/console-capabilities",
        "listConsoleCapabilities",
        "console_capability:read",
        Oidc,
        None,
        200,
        false,
        true,
        TEACHER_OR_STUDENT,
        Environment
    ),
    op!(
        Public,
        Post,
        "/api/v1/access-grants/{grantId}/console-capabilities",
        "issueConsoleCapability",
        "console_capability:issue",
        BffSession,
        IdempotentRevisioned,
        201,
        false,
        false,
        TEACHER_OR_STUDENT,
        Environment
    ),
    op!(
        Public,
        Post,
        "/api/v1/courses/{courseId}/evaluation-releases",
        "createEvaluationRelease",
        "evaluation_release:publish",
        BffSession,
        IdempotentCreate,
        201,
        false,
        true,
        TEACHER,
        Course
    ),
    op!(
        Public,
        Get,
        "/api/v1/courses/{courseId}/evaluation-releases",
        "listEvaluationReleases",
        "evaluation_release:read",
        Oidc,
        None,
        200,
        false,
        true,
        TEACHER,
        Course
    ),
    op!(
        Public,
        Get,
        "/api/v1/courses/{courseId}/evaluation-releases/{releaseId}",
        "getEvaluationRelease",
        "evaluation_release:read",
        Oidc,
        None,
        200,
        false,
        true,
        TEACHER,
        Course
    ),
    op!(
        Public,
        Post,
        "/api/v1/courses/{courseId}/evaluation-releases/{releaseId}/withdraw",
        "withdrawEvaluationRelease",
        "evaluation_release:withdraw",
        BffSession,
        IdempotentRevisioned,
        200,
        false,
        false,
        TEACHER,
        Course
    ),
    op!(
        Public,
        Get,
        "/api/v1/courses/{courseId}/me/evaluation-results",
        "listOwnEvaluationResults",
        "evaluation_result:read_own",
        Oidc,
        None,
        200,
        false,
        true,
        &[PlatformRole::Student],
        Course
    ),
    op!(
        Public,
        Get,
        "/api/v1/courses/{courseId}/me/evaluation-results/{runId}",
        "getOwnEvaluationResult",
        "evaluation_result:read_own",
        Oidc,
        None,
        200,
        false,
        true,
        &[PlatformRole::Student],
        Course
    ),
    op!(
        Public,
        Get,
        "/api/v1/projects/{projectId}/me/evaluation-results",
        "listOwnProjectEvaluationResults",
        "evaluation_result:read_own",
        Oidc,
        None,
        200,
        false,
        true,
        &[PlatformRole::Student],
        Project
    ),
    op!(
        Public,
        Get,
        "/api/v1/projects/{projectId}/me/evaluation-results/{runId}",
        "getOwnProjectEvaluationResult",
        "evaluation_result:read_own",
        Oidc,
        None,
        200,
        false,
        true,
        &[PlatformRole::Student],
        Project
    ),
    op!(
        Public,
        Get,
        "/api/v1/projects/{projectId}/environment-template-releases",
        "listEnvironmentTemplateReleases",
        "release:read",
        Oidc,
        None,
        200,
        false,
        true,
        ALL_ROLES,
        Project
    ),
    op!(
        Public,
        Post,
        "/api/v1/projects/{projectId}/environment-template-releases",
        "createEnvironmentTemplateRelease",
        "release:publish",
        Oidc,
        IdempotentCreate,
        202,
        true,
        true,
        ALL_ROLES,
        Project
    ),
    op!(
        Public,
        Get,
        "/api/v1/projects/{projectId}/environment-template-releases/{releaseId}",
        "getEnvironmentTemplateRelease",
        "release:read",
        Oidc,
        None,
        200,
        false,
        true,
        ALL_ROLES,
        Project
    ),
    op!(
        Public,
        Post,
        "/api/v1/projects/{projectId}/environment-template-releases/{releaseId}/withdraw",
        "withdrawEnvironmentTemplateRelease",
        "release:withdraw",
        Oidc,
        IdempotentRevisioned,
        201,
        false,
        false,
        ALL_ROLES,
        Project
    ),
    op!(
        Public,
        Get,
        "/api/v1/environments",
        "listEnvironments",
        "environment:read",
        Oidc,
        None,
        200,
        false,
        true,
        TEACHER_OR_STUDENT,
        Project
    ),
    op!(
        Public,
        Post,
        "/api/v1/environments",
        "createEnvironment",
        "environment:write",
        Oidc,
        IdempotentCreate,
        202,
        true,
        true,
        TEACHER_OR_STUDENT,
        Project
    ),
    op!(
        Public,
        Get,
        "/api/v1/environments/{environmentId}",
        "getEnvironment",
        "environment:read",
        Oidc,
        None,
        200,
        false,
        true,
        TEACHER_OR_STUDENT,
        Environment
    ),
    op!(
        Public,
        Get,
        "/api/v1/environments/{environmentId}/operations/{operationId}",
        "getEnvironmentOperation",
        "environment:read",
        Oidc,
        None,
        200,
        false,
        true,
        TEACHER_OR_STUDENT,
        Environment
    ),
    op!(
        Public,
        Get,
        "/api/v1/environments/{environmentId}/operations",
        "listEnvironmentOperations",
        "environment:read",
        Oidc,
        None,
        200,
        false,
        true,
        TEACHER_OR_STUDENT,
        Environment
    ),
    op!(
        Public,
        Post,
        "/api/v1/environments/{environmentId}/start",
        "startEnvironment",
        "environment:write",
        Oidc,
        IdempotentRevisioned,
        202,
        true,
        true,
        TEACHER_OR_STUDENT,
        Environment
    ),
    op!(
        Public,
        Post,
        "/api/v1/environments/{environmentId}/stop",
        "stopEnvironment",
        "environment:write",
        Oidc,
        IdempotentRevisioned,
        202,
        true,
        true,
        TEACHER_OR_STUDENT,
        Environment
    ),
    op!(
        Public,
        Post,
        "/api/v1/environments/{environmentId}/restart",
        "restartEnvironment",
        "environment:write",
        Oidc,
        IdempotentRevisioned,
        202,
        true,
        true,
        TEACHER_OR_STUDENT,
        Environment
    ),
    op!(
        Public,
        Post,
        "/api/v1/environments/{environmentId}/reset",
        "resetEnvironment",
        "environment:write",
        Oidc,
        IdempotentRevisioned,
        202,
        true,
        true,
        TEACHER_OR_STUDENT,
        Environment
    ),
    op!(
        Public,
        Post,
        "/api/v1/environments/{environmentId}/retry",
        "retryEnvironment",
        "environment:write",
        Oidc,
        IdempotentRevisioned,
        202,
        true,
        true,
        TEACHER_OR_STUDENT,
        Environment
    ),
    op!(
        Public,
        Post,
        "/api/v1/environments/{environmentId}/cancel",
        "cancelEnvironmentOperation",
        "environment:write",
        Oidc,
        IdempotentRevisioned,
        202,
        false,
        true,
        TEACHER_OR_STUDENT,
        Environment
    ),
    op!(
        Public,
        Post,
        "/api/v1/environments/{environmentId}/recover",
        "recoverEnvironment",
        "environment:write",
        Oidc,
        IdempotentRevisioned,
        202,
        true,
        true,
        TEACHER_OR_STUDENT,
        Environment
    ),
    op!(
        Public,
        Delete,
        "/api/v1/environments/{environmentId}",
        "deleteEnvironment",
        "environment:delete",
        Oidc,
        IdempotentRevisioned,
        202,
        true,
        true,
        TEACHER_OR_STUDENT,
        Environment
    ),
    op!(
        Public,
        Get,
        "/api/v1/environments/{environmentId}/endpoints",
        "listEnvironmentEndpoints",
        "environment:read",
        Oidc,
        None,
        200,
        false,
        true,
        TEACHER_OR_STUDENT,
        Environment
    ),
    op!(
        Public,
        Post,
        "/api/v1/environments/{environmentId}/freeze",
        "freezeSubmission",
        "submission:freeze",
        Oidc,
        IdempotentRevisioned,
        202,
        true,
        true,
        TEACHER_OR_STUDENT,
        Environment
    ),
    op!(
        Public,
        Get,
        "/api/v1/frozen-submissions/{submissionId}",
        "getFrozenSubmission",
        "submission:read",
        Oidc,
        None,
        200,
        false,
        true,
        TEACHER_OR_STUDENT,
        Environment
    ),
    op!(
        Public,
        Post,
        "/api/v1/me/ssh-public-keys",
        "createSshPublicKey",
        "ssh_key:write",
        Oidc,
        IdempotentCreate,
        201,
        false,
        false,
        TEACHER_OR_STUDENT,
        Global
    ),
    op!(
        Public,
        Get,
        "/api/v1/me/ssh-public-keys",
        "listSshPublicKeys",
        "ssh_key:read",
        Oidc,
        None,
        200,
        false,
        true,
        TEACHER_OR_STUDENT,
        Global
    ),
    op!(
        Public,
        Delete,
        "/api/v1/me/ssh-public-keys/{keyId}",
        "deleteSshPublicKey",
        "ssh_key:write",
        Oidc,
        IdempotentRevisioned,
        204,
        false,
        true,
        TEACHER_OR_STUDENT,
        Global
    ),
    op!(
        Public,
        Get,
        "/api/v1/environments/{environmentId}/access-grants",
        "listEnvironmentAccessGrants",
        "access_grant:read",
        Oidc,
        None,
        200,
        false,
        true,
        TEACHER_OR_STUDENT,
        Environment
    ),
    op!(
        Public,
        Post,
        "/api/v1/environments/{environmentId}/access-grants",
        "createAccessGrant",
        "access_grant:write",
        Oidc,
        IdempotentCreate,
        201,
        false,
        true,
        TEACHER_OR_STUDENT,
        Environment
    ),
    op!(
        Public,
        Get,
        "/api/v1/access-grants/{grantId}",
        "getAccessGrant",
        "access_grant:read",
        Oidc,
        None,
        200,
        false,
        true,
        TEACHER_OR_STUDENT,
        Environment
    ),
    op!(
        Public,
        Post,
        "/api/v1/access-grants/{grantId}/revoke",
        "revokeAccessGrant",
        "access_grant:revoke",
        Oidc,
        IdempotentRevisioned,
        202,
        false,
        true,
        TEACHER_OR_STUDENT,
        Environment
    ),
    op!(
        Public,
        Post,
        "/api/v1/access-grants/{grantId}/renew",
        "renewAccessGrant",
        "access_grant:write",
        Oidc,
        IdempotentRevisioned,
        200,
        false,
        true,
        TEACHER_OR_STUDENT,
        Environment
    ),
    op!(
        GatewayInternal,
        Post,
        "/internal/v1/environments/{environmentId}/owner:resolve",
        "resolveEnvironmentOwner",
        "environment:resolve_owner",
        ServiceJwt,
        None,
        200,
        false,
        true,
        PLATFORM_ADMIN,
        Service
    ),
    op!(
        GatewayInternal,
        Post,
        "/internal/v1/environments/{environmentId}/endpoint-eligibility:resolve",
        "resolveEndpointEligibility",
        "environment:resolve_endpoint_eligibility",
        ServiceJwt,
        None,
        200,
        false,
        true,
        PLATFORM_ADMIN,
        Service
    ),
    op!(
        GatewayInternal,
        Get,
        "/internal/v1/environments/{environmentId}/work-configuration-target",
        "resolveEnvironmentWorkConfigurationTarget",
        "environment.work.read",
        ServiceJwt,
        None,
        200,
        false,
        true,
        PLATFORM_ADMIN,
        Service
    ),
    op!(
        GatewayInternal,
        Post,
        "/internal/v1/environments/{environmentId}/execution-binding/evaluation",
        "resolveEnvironmentEvaluationExecutionBinding",
        "environment:resolve_evaluation_execution_binding",
        ServiceJwt,
        None,
        200,
        false,
        true,
        PLATFORM_ADMIN,
        Service
    ),
    op!(
        GatewayInternal,
        Post,
        "/internal/v1/environments/{environmentId}/execution-binding/work",
        "resolveEnvironmentWorkExecutionBinding",
        "environment:resolve_work_execution_binding",
        ServiceJwt,
        None,
        200,
        false,
        true,
        PLATFORM_ADMIN,
        Service
    ),
    op!(
        GatewayInternal,
        Post,
        "/internal/v1/ssh/authorize",
        "authorizeSsh",
        "gateway:ssh_authorize",
        ServiceJwt,
        None,
        200,
        false,
        false,
        PLATFORM_ADMIN,
        Service
    ),
    op!(
        GatewayInternal,
        Post,
        "/internal/v1/sessions",
        "createGatewaySession",
        "gateway:session_write",
        ServiceJwt,
        IdempotentCreate,
        201,
        false,
        true,
        PLATFORM_ADMIN,
        Service
    ),
    op!(
        GatewayInternal,
        Post,
        "/internal/v1/sessions/{sessionId}/heartbeat",
        "heartbeatGatewaySession",
        "gateway:session_write",
        ServiceJwt,
        IdempotentRevisioned,
        204,
        false,
        true,
        PLATFORM_ADMIN,
        Service
    ),
    op!(
        GatewayInternal,
        Post,
        "/internal/v1/sessions/{sessionId}/close",
        "closeGatewaySession",
        "gateway:session_write",
        ServiceJwt,
        IdempotentRevisioned,
        204,
        false,
        true,
        PLATFORM_ADMIN,
        Service
    ),
    op!(
        Public,
        Post,
        "/api/v1/resource/usage",
        "recordResourceUsage",
        "resource:usage_record",
        BffSession,
        None,
        200,
        false,
        true,
        PLATFORM_ADMIN,
        Project
    ),
    op!(
        GatewayInternal,
        Post,
        "/internal/v1/llm-reviews",
        "createInternalAgentLlmReview",
        "agent.llm_review.create",
        ServiceJwt,
        IdempotentCreate,
        202,
        true,
        true,
        PLATFORM_ADMIN,
        Service
    ),
    op!(
        GatewayInternal,
        Get,
        "/internal/v1/llm-reviews/{taskRunId}",
        "getInternalAgentLlmReview",
        "agent.llm_review.read",
        ServiceJwt,
        None,
        200,
        false,
        true,
        PLATFORM_ADMIN,
        Service
    ),
    op!(
        GatewayInternal,
        Post,
        "/internal/v1/llm-reviews/{taskRunId}/cancel",
        "cancelInternalAgentLlmReview",
        "agent.llm_review.cancel",
        ServiceJwt,
        IdempotentCreate,
        200,
        false,
        true,
        PLATFORM_ADMIN,
        Service
    ),
    op!(
        GatewayInternal,
        Get,
        "/internal/v1/agent-runs/{runId}/work-execution-intent",
        "getInternalAgentWorkExecutionIntent",
        "agent.control.invoke",
        ServiceJwt,
        None,
        200,
        false,
        true,
        PLATFORM_ADMIN,
        Service
    ),
    op!(
        GatewayInternal,
        Get,
        "/internal/v1/authoring-publications/{approvalId}/admission",
        "getInternalAuthoringPublicationAdmission",
        "control.authoring.read",
        ServiceJwt,
        None,
        200,
        false,
        true,
        PLATFORM_ADMIN,
        Service
    ),
    op!(
        GatewayInternal,
        Get,
        "/internal/v1/generated-artifacts/{artifactId}",
        "getInternalGeneratedArtifact",
        "agent.control.invoke",
        ServiceJwt,
        None,
        200,
        false,
        true,
        PLATFORM_ADMIN,
        Service
    ),
    op!(
        GatewayInternal,
        Post,
        "/internal/v1/task-resources",
        "createTaskResourceRequest",
        "resource.task.create",
        ServiceJwt,
        IdempotentCreate,
        201,
        false,
        true,
        PLATFORM_ADMIN,
        Service
    ),
    op!(
        GatewayInternal,
        Get,
        "/internal/v1/task-resources/{taskRunId}/request",
        "getTaskResourceRequest",
        "resource.task.read",
        ServiceJwt,
        None,
        200,
        false,
        true,
        PLATFORM_ADMIN,
        Service
    ),
    op!(
        GatewayInternal,
        Post,
        "/internal/v1/task-resources/{taskRunId}/claim",
        "claimTaskResource",
        "resource.task.claim",
        ServiceJwt,
        None,
        200,
        false,
        true,
        PLATFORM_ADMIN,
        Service
    ),
    op!(
        GatewayInternal,
        Post,
        "/internal/v1/task-resources/{taskRunId}/ack",
        "acknowledgeTaskResource",
        "resource.task.ack",
        ServiceJwt,
        None,
        200,
        false,
        true,
        PLATFORM_ADMIN,
        Service
    ),
    op!(
        GatewayInternal,
        Get,
        "/internal/v1/task-resources/{taskRunId}",
        "getTaskResource",
        "resource.task.read",
        ServiceJwt,
        None,
        200,
        false,
        true,
        PLATFORM_ADMIN,
        Service
    ),
    op!(
        GatewayInternal,
        Post,
        "/internal/v1/task-resources/{taskRunId}/release",
        "releaseTaskResource",
        "resource.task.release",
        ServiceJwt,
        None,
        200,
        false,
        true,
        PLATFORM_ADMIN,
        Service
    ),
    op!(
        GatewayInternal,
        Post,
        "/internal/v1/task-resources/{taskRunId}/cancel",
        "cancelTaskResource",
        "resource.task.cancel",
        ServiceJwt,
        IdempotentCreate,
        200,
        false,
        true,
        PLATFORM_ADMIN,
        Service
    ),
    op!(
        GatewayInternal,
        Post,
        "/internal/v1/resource/usage",
        "recordInternalResourceUsage",
        "resource.usage.record",
        ServiceJwt,
        None,
        200,
        false,
        true,
        PLATFORM_ADMIN,
        Service
    ),
];

pub fn validate_operation_catalog() -> Result<(), HttpContractError> {
    let mut ids = BTreeSet::new();
    for operation in OPERATIONS {
        if !ids.insert(operation.operation_id)
            || operation.permission.is_empty()
            || operation.timeout_milliseconds == 0
        {
            return Err(HttpContractError::InvalidOperationCatalog);
        }
        if operation.surface == ApiSurface::GatewayInternal
            && operation.security != Security::ServiceJwt
        {
            return Err(HttpContractError::PublicInternalLeak);
        }
        if operation.mutation != MutationContract::None && operation.method == Method::Get {
            return Err(HttpContractError::InvalidOperationCatalog);
        }
        if operation.allowed_roles.is_empty()
            || (operation.security == Security::ServiceJwt
                && operation.scope != OperationScopeKind::Service)
        {
            return Err(HttpContractError::InvalidOperationCatalog);
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventStreamQuery {
    pub course_id: CourseId,
    pub after: Option<StreamSequence>,
}

#[derive(Debug, thiserror::Error)]
pub enum HttpContractError {
    #[error("invalid Idempotency-Key")]
    InvalidIdempotencyKey,
    #[error("weak ETag is not accepted")]
    WeakEtag,
    #[error("invalid strong ETag")]
    InvalidEtag,
    #[error("Last-Event-ID and after disagree")]
    ConflictingSseCursor,
    #[error("operation catalog is incomplete or ambiguous")]
    InvalidOperationCatalog,
    #[error("invalid container Work execution request or receipt")]
    InvalidContainerWorkExecution,
    #[error("public and internal security surfaces are mixed")]
    PublicInternalLeak,
    #[error("cursor or page limit is invalid")]
    InvalidCursorPage,
    #[error("environment request or query is invalid")]
    InvalidEnvironmentQuery,
    #[error("internal service identity or canonical response is invalid")]
    InvalidInternalIdentity,
    #[error("console capability request or availability is invalid")]
    InvalidConsoleCapability,
    #[error("evaluation control request is invalid")]
    InvalidEvaluationControl,
    #[error("internal Agent LLM review request or receipt is invalid")]
    InvalidAgentLlmReview,
}

#[must_use]
pub fn sse_cursor_expired() -> DiagnosticCode {
    DiagnosticCode::registered("LW_SSE_CURSOR_EXPIRED")
}
#[must_use]
pub fn sse_cursor_gap() -> DiagnosticCode {
    DiagnosticCode::registered("LW_SSE_CURSOR_GAP")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn operation_ids_and_surfaces_are_sound() -> Result<(), HttpContractError> {
        validate_operation_catalog()
    }
    #[test]
    fn weak_etag_is_rejected() {
        assert!(matches!(
            StrongEtag::parse("W/\"rev-1\""),
            Err(HttpContractError::WeakEtag)
        ));
    }
    #[test]
    fn sse_cursor_sources_must_agree() {
        let above_javascript_safe_integer = StreamSequence(9_007_199_254_740_992);
        let adjacent_cursor = StreamSequence(9_007_199_254_740_993);
        assert!(matches!(
            resolve_sse_resume(
                Some(above_javascript_safe_integer),
                Some(above_javascript_safe_integer)
            ),
            Ok(SseResume::After(value)) if value == above_javascript_safe_integer
        ));
        assert!(matches!(
            resolve_sse_resume(Some(above_javascript_safe_integer), Some(adjacent_cursor)),
            Err(HttpContractError::ConflictingSseCursor)
        ));
    }

    #[test]
    fn console_capability_request_requires_the_exact_discovered_fences() {
        let timestamp = |value| {
            serde_json::from_str(&format!("\"{value}\""))
                .unwrap_or_else(|error| unreachable!("static timestamp must parse: {error}"))
        };
        let revision = |value| {
            Revision::new(value)
                .unwrap_or_else(|error| unreachable!("static revision must be non-zero: {error}"))
        };
        let availability = crate::access::ConsoleCapabilityAvailability {
            access_grant_id: AccessGrantId::new(),
            access_grant_revision: revision(2),
            project_id: ProjectId::new(),
            course_id: Some(CourseId::new()),
            environment_id: EnvironmentId::new(),
            environment_class: crate::authoring::EnvironmentClass::Experiment,
            environment_revision: revision(3),
            expires_at: timestamp("2026-07-29T00:01:00.000Z"),
            lease_fence: None,
            kinds: vec![crate::access::ConsoleKind::Xterm],
        };
        let request = IssueConsoleCapabilityRequest {
            kind: crate::access::ConsoleKind::Xterm,
            expected_access_grant_revision: revision(2),
            expected_environment_revision: revision(3),
            expected_lease_fence: None,
        };
        assert!(request.validate_against(&availability).is_ok());
        assert!(
            request
                .validate_if_match(&StrongEtag::from_revision(revision(2)))
                .is_ok()
        );
        assert!(matches!(
            request.validate_if_match(&StrongEtag::from_revision(revision(3))),
            Err(HttpContractError::InvalidConsoleCapability)
        ));

        let stale = IssueConsoleCapabilityRequest {
            expected_environment_revision: revision(4),
            ..request.clone()
        };
        assert!(matches!(
            stale.validate_against(&availability),
            Err(HttpContractError::InvalidConsoleCapability)
        ));

        let lease = crate::access::ConsoleLeaseFence {
            lease_id: crate::LeaseId::new(),
            lease_revision: revision(5),
            expires_at: timestamp("2026-07-29T00:01:00.000Z"),
        };
        let work = crate::access::ConsoleCapabilityAvailability {
            environment_class: crate::authoring::EnvironmentClass::Work,
            lease_fence: Some(lease.clone()),
            ..availability.clone()
        };
        let work_request = IssueConsoleCapabilityRequest {
            expected_lease_fence: Some(lease),
            ..request
        };
        assert!(work_request.validate_against(&work).is_ok());
        assert!(matches!(
            request.validate_against(&work),
            Err(HttpContractError::InvalidConsoleCapability)
        ));

        let invalid_experiment = IssueConsoleCapabilityRequest {
            expected_lease_fence: Some(crate::access::ConsoleLeaseFence {
                lease_id: crate::LeaseId::new(),
                lease_revision: revision(6),
                expires_at: timestamp("2026-07-29T00:01:00.000Z"),
            }),
            ..work_request
        };
        assert!(matches!(
            invalid_experiment.validate_against(&availability),
            Err(HttpContractError::InvalidConsoleCapability)
        ));
    }
}
