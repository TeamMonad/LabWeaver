//! Infrastructure-neutral REST, SSE, concurrency, and security contract catalog.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    AccessGrantId, ActorId, ApprovalId, BuildRequestId, CandidateId, CourseId, DiagnosticCode,
    EndpointId, EnvironmentId, EventId, ImageArtifactId, OperationId, PlatformRole,
    ProblemPackageId, ProjectId, ReleaseId, Revision, Sha256Digest, StreamSequence,
    UploadSessionId, UtcTimestamp,
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

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProblemPackageUploadFile {
    pub path: String,
    pub size_bytes: u64,
    pub sha256: Sha256Digest,
    pub media_type: String,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateProblemPackageUploadRequest {
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
    pub course_id: CourseId,
    pub revision: Revision,
    pub files: Vec<ProblemPackageUploadFile>,
    pub upload_targets: Vec<ProblemPackageUploadTarget>,
    pub expires_at: UtcTimestamp,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompleteProblemPackageUploadRequest {
    pub manifest_sha256: Sha256Digest,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateAgentRunRequest {
    pub package_id: ProblemPackageId,
    pub package_revision: Revision,
    pub package_sha256: Sha256Digest,
    pub policy_id: crate::PolicyId,
    pub policy_revision: Revision,
    pub requested_runtime: crate::authoring::RuntimeKind,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CandidateDecisionRequest {
    pub candidate_revision: Revision,
    pub candidate_sha256: Sha256Digest,
    pub policy_revision: Revision,
    pub schema_sha256: Sha256Digest,
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
    pub image_policy_evaluation: Option<crate::supply_chain::ImagePolicyEvaluation>,
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

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateEnvironmentTemplateReleaseRequest {
    pub candidate_id: CandidateId,
    pub candidate_revision: Revision,
    pub environment_spec_sha256: Sha256Digest,
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InternalCreateAgentRunRequest {
    /// Authoritative route course.
    pub course_id: CourseId,
    /// Public immutable request whose idempotency key remains an HTTP header.
    pub request: CreateAgentRunRequest,
    /// Completed Control-owned package; Agent re-reads and re-hashes every object.
    pub package: crate::authoring::ProblemPackage,
    /// Internal artifact ID to opaque object-key mapping; keys never contain original paths.
    pub object_locators: BTreeMap<crate::ArtifactId, String>,
    /// Active immutable course policy.
    pub policy: crate::authoring::CourseLlmEgressPolicy,
}

/// Revision precondition for Control-to-Agent cancellation and retry mutations.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InternalAgentRunMutationRequest {
    /// Exact course authority that owns the target run.
    pub course_id: CourseId,
    /// Exact Agent-owned run revision observed by Control.
    pub expected_revision: Revision,
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

/// Control-to-Agent build cancellation command carried only over allowlisted mTLS.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InternalAgentBuildCancellationRequest {
    pub course_id: CourseId,
    pub build_request_id: BuildRequestId,
    pub command_sha256: Sha256Digest,
    pub expected_state: InternalAgentBuildState,
    pub expected_revision: Revision,
    pub actor_id: ActorId,
    /// Exact verified Control URI SAN; the Agent compares it with the mTLS peer principal.
    pub authority_san_uri: String,
    pub requested_at: UtcTimestamp,
}

/// Scope and immutable identity required to read one Agent-owned build status.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InternalAgentBuildStatusQuery {
    pub course_id: CourseId,
    pub command_sha256: Sha256Digest,
}

/// Durable result returned for an accepted or exactly replayed build cancellation.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InternalAgentBuildCancellationResult {
    pub course_id: CourseId,
    pub build_request_id: BuildRequestId,
    pub command_sha256: Sha256Digest,
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
    /// Canonical hash over this response, excluding this field.
    pub outcome_sha256: Sha256Digest,
}

impl InternalAgentRunOutcome {
    /// Validates parent identities and canonical response identity.
    pub fn validate(&self) -> Result<(), HttpContractError> {
        if self
            .environment_candidate
            .as_ref()
            .is_some_and(|candidate| candidate.run_id != self.run.id)
            || self
                .evaluation_candidate
                .as_ref()
                .is_some_and(|candidate| candidate.run_id != self.run.id)
        {
            return Err(HttpContractError::InvalidInternalIdentity);
        }
        let canonical = Sha256Digest::of_canonical(&serde_json::json!({
            "run": self.run,
            "environmentCandidate": self.environment_candidate,
            "evaluationCandidate": self.evaluation_candidate,
        }))
        .map_err(|_| HttpContractError::InvalidInternalIdentity)?;
        if canonical != self.outcome_sha256 {
            return Err(HttpContractError::InvalidInternalIdentity);
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
    pub policy_evaluation: crate::supply_chain::ImagePolicyEvaluation,
    pub resolution_sha256: Sha256Digest,
}

impl InternalImageArtifactResolution {
    /// Verifies exact artifact ID and canonical response identity.
    pub fn validate(&self) -> Result<(), HttpContractError> {
        if self.policy_evaluation.artifact_id != self.artifact_id {
            return Err(HttpContractError::InvalidInternalIdentity);
        }
        let canonical = Sha256Digest::of_canonical(&serde_json::json!({
            "artifactId": self.artifact_id,
            "artifact": self.artifact,
            "policyEvaluation": self.policy_evaluation,
        }))
        .map_err(|_| HttpContractError::InvalidInternalIdentity)?;
        if canonical != self.resolution_sha256 {
            return Err(HttpContractError::InvalidInternalIdentity);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateEnvironmentRequest {
    pub course_id: CourseId,
    pub release_id: ReleaseId,
    pub release_version: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_label: Option<String>,
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

pub use crate::environment::{EnvironmentOwnerResolution, EnvironmentOwnerResolutionRequest};

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FreezeSubmissionRequest {
    pub course_id: CourseId,
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
    pub course_id: CourseId,
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
    pub course_id: CourseId,
    pub project_id: Option<ProjectId>,
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
    pub state: Option<crate::environment::EnvironmentOperationStatus>,
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
        state: crate::environment::EnvironmentOperationStatus,
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
    pub course_id: CourseId,
    pub project_id: Option<ProjectId>,
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
    ServiceMtls,
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

/// Explicit authorization policy for a catalog operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OperationAuthorization {
    /// Stable operation identifier.
    pub operation_id: &'static str,
    /// Base OIDC roles permitted to request the operation.
    pub allowed_roles: &'static [PlatformRole],
    /// Required resource scope kind.
    pub scope: OperationScopeKind,
}

const TEACHER: &[PlatformRole] = &[PlatformRole::Teacher];
const TEACHER_OR_STUDENT: &[PlatformRole] = &[PlatformRole::Teacher, PlatformRole::Student];
const PLATFORM_ADMIN: &[PlatformRole] = &[PlatformRole::PlatformAdmin];

/// Authorization policy for every public and gateway operation. This table is
/// intentionally separate from route implementations so generated contracts,
/// Gateway requests, and service middleware share one semantic source.
pub const OPERATION_AUTHORIZATIONS: &[OperationAuthorization] = &[
    OperationAuthorization {
        operation_id: "createProblemPackageUpload",
        allowed_roles: TEACHER,
        scope: OperationScopeKind::Course,
    },
    OperationAuthorization {
        operation_id: "completeProblemPackageUpload",
        allowed_roles: TEACHER,
        scope: OperationScopeKind::Course,
    },
    OperationAuthorization {
        operation_id: "getProblemPackage",
        allowed_roles: TEACHER_OR_STUDENT,
        scope: OperationScopeKind::Course,
    },
    OperationAuthorization {
        operation_id: "createCourseLlmPolicy",
        allowed_roles: TEACHER,
        scope: OperationScopeKind::Course,
    },
    OperationAuthorization {
        operation_id: "getActiveCourseLlmPolicy",
        allowed_roles: TEACHER,
        scope: OperationScopeKind::Course,
    },
    OperationAuthorization {
        operation_id: "createAgentRun",
        allowed_roles: TEACHER,
        scope: OperationScopeKind::Course,
    },
    OperationAuthorization {
        operation_id: "getAgentRun",
        allowed_roles: TEACHER,
        scope: OperationScopeKind::Course,
    },
    OperationAuthorization {
        operation_id: "cancelAgentRun",
        allowed_roles: TEACHER,
        scope: OperationScopeKind::Course,
    },
    OperationAuthorization {
        operation_id: "retryAgentRunTrack",
        allowed_roles: TEACHER,
        scope: OperationScopeKind::Course,
    },
    OperationAuthorization {
        operation_id: "getEnvironmentCandidate",
        allowed_roles: TEACHER,
        scope: OperationScopeKind::Course,
    },
    OperationAuthorization {
        operation_id: "appendEnvironmentCandidateDecision",
        allowed_roles: TEACHER,
        scope: OperationScopeKind::Course,
    },
    OperationAuthorization {
        operation_id: "getEvaluationCandidate",
        allowed_roles: TEACHER,
        scope: OperationScopeKind::Course,
    },
    OperationAuthorization {
        operation_id: "appendEvaluationCandidateDecision",
        allowed_roles: TEACHER,
        scope: OperationScopeKind::Course,
    },
    OperationAuthorization {
        operation_id: "createEnvironmentTemplateRelease",
        allowed_roles: TEACHER,
        scope: OperationScopeKind::Course,
    },
    OperationAuthorization {
        operation_id: "listEnvironmentTemplateReleases",
        allowed_roles: TEACHER_OR_STUDENT,
        scope: OperationScopeKind::Course,
    },
    OperationAuthorization {
        operation_id: "getEnvironmentTemplateRelease",
        allowed_roles: TEACHER_OR_STUDENT,
        scope: OperationScopeKind::Course,
    },
    OperationAuthorization {
        operation_id: "withdrawEnvironmentTemplateRelease",
        allowed_roles: TEACHER,
        scope: OperationScopeKind::Course,
    },
    OperationAuthorization {
        operation_id: "createEnvironment",
        allowed_roles: TEACHER_OR_STUDENT,
        scope: OperationScopeKind::Course,
    },
    OperationAuthorization {
        operation_id: "listEnvironments",
        allowed_roles: TEACHER_OR_STUDENT,
        scope: OperationScopeKind::Course,
    },
    OperationAuthorization {
        operation_id: "getEnvironment",
        allowed_roles: TEACHER_OR_STUDENT,
        scope: OperationScopeKind::Environment,
    },
    OperationAuthorization {
        operation_id: "getEnvironmentOperation",
        allowed_roles: TEACHER_OR_STUDENT,
        scope: OperationScopeKind::Environment,
    },
    OperationAuthorization {
        operation_id: "listEnvironmentOperations",
        allowed_roles: TEACHER_OR_STUDENT,
        scope: OperationScopeKind::Environment,
    },
    OperationAuthorization {
        operation_id: "listEnvironmentAccessGrants",
        allowed_roles: TEACHER_OR_STUDENT,
        scope: OperationScopeKind::Environment,
    },
    OperationAuthorization {
        operation_id: "startEnvironment",
        allowed_roles: TEACHER_OR_STUDENT,
        scope: OperationScopeKind::Environment,
    },
    OperationAuthorization {
        operation_id: "stopEnvironment",
        allowed_roles: TEACHER_OR_STUDENT,
        scope: OperationScopeKind::Environment,
    },
    OperationAuthorization {
        operation_id: "restartEnvironment",
        allowed_roles: TEACHER_OR_STUDENT,
        scope: OperationScopeKind::Environment,
    },
    OperationAuthorization {
        operation_id: "resetEnvironment",
        allowed_roles: TEACHER_OR_STUDENT,
        scope: OperationScopeKind::Environment,
    },
    OperationAuthorization {
        operation_id: "retryEnvironment",
        allowed_roles: TEACHER_OR_STUDENT,
        scope: OperationScopeKind::Environment,
    },
    OperationAuthorization {
        operation_id: "cancelEnvironmentOperation",
        allowed_roles: TEACHER_OR_STUDENT,
        scope: OperationScopeKind::Environment,
    },
    OperationAuthorization {
        operation_id: "recoverEnvironment",
        allowed_roles: TEACHER_OR_STUDENT,
        scope: OperationScopeKind::Environment,
    },
    OperationAuthorization {
        operation_id: "deleteEnvironment",
        allowed_roles: TEACHER_OR_STUDENT,
        scope: OperationScopeKind::Environment,
    },
    OperationAuthorization {
        operation_id: "listEnvironmentEndpoints",
        allowed_roles: TEACHER_OR_STUDENT,
        scope: OperationScopeKind::Environment,
    },
    OperationAuthorization {
        operation_id: "freezeSubmission",
        allowed_roles: TEACHER_OR_STUDENT,
        scope: OperationScopeKind::Environment,
    },
    OperationAuthorization {
        operation_id: "getFrozenSubmission",
        allowed_roles: TEACHER_OR_STUDENT,
        scope: OperationScopeKind::Environment,
    },
    OperationAuthorization {
        operation_id: "createSshPublicKey",
        allowed_roles: TEACHER_OR_STUDENT,
        scope: OperationScopeKind::Global,
    },
    OperationAuthorization {
        operation_id: "listSshPublicKeys",
        allowed_roles: TEACHER_OR_STUDENT,
        scope: OperationScopeKind::Global,
    },
    OperationAuthorization {
        operation_id: "deleteSshPublicKey",
        allowed_roles: TEACHER_OR_STUDENT,
        scope: OperationScopeKind::Global,
    },
    OperationAuthorization {
        operation_id: "createAccessGrant",
        allowed_roles: TEACHER_OR_STUDENT,
        scope: OperationScopeKind::Environment,
    },
    OperationAuthorization {
        operation_id: "getAccessGrant",
        allowed_roles: TEACHER_OR_STUDENT,
        scope: OperationScopeKind::Environment,
    },
    OperationAuthorization {
        operation_id: "revokeAccessGrant",
        allowed_roles: TEACHER_OR_STUDENT,
        scope: OperationScopeKind::Environment,
    },
    OperationAuthorization {
        operation_id: "renewAccessGrant",
        allowed_roles: TEACHER_OR_STUDENT,
        scope: OperationScopeKind::Environment,
    },
    OperationAuthorization {
        operation_id: "streamCourseEvents",
        allowed_roles: TEACHER_OR_STUDENT,
        scope: OperationScopeKind::Course,
    },
    OperationAuthorization {
        operation_id: "authorizeSsh",
        allowed_roles: PLATFORM_ADMIN,
        scope: OperationScopeKind::Service,
    },
    OperationAuthorization {
        operation_id: "createGatewaySession",
        allowed_roles: PLATFORM_ADMIN,
        scope: OperationScopeKind::Service,
    },
    OperationAuthorization {
        operation_id: "heartbeatGatewaySession",
        allowed_roles: PLATFORM_ADMIN,
        scope: OperationScopeKind::Service,
    },
    OperationAuthorization {
        operation_id: "closeGatewaySession",
        allowed_roles: PLATFORM_ADMIN,
        scope: OperationScopeKind::Service,
    },
    OperationAuthorization {
        operation_id: "resolveEnvironmentOwner",
        allowed_roles: PLATFORM_ADMIN,
        scope: OperationScopeKind::Service,
    },
    OperationAuthorization {
        operation_id: "resolveEndpointEligibility",
        allowed_roles: PLATFORM_ADMIN,
        scope: OperationScopeKind::Service,
    },
];

/// Looks up mandatory role and scope metadata for a stable operation id.
#[must_use]
pub fn operation_authorization(operation_id: &str) -> Option<&'static OperationAuthorization> {
    OPERATION_AUTHORIZATIONS
        .iter()
        .find(|authorization| authorization.operation_id == operation_id)
}

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
}

macro_rules! op {
    ($surface:ident,$method:ident,$path:literal,$id:literal,$permission:literal,$security:ident,$mutation:ident,$status:literal,$cancel:literal,$retry:literal) => {
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
        }
    };
}

pub const OPERATIONS: &[OperationContract] = &[
    op!(
        Public,
        Post,
        "/api/v1/courses/{courseId}/problem-package-uploads",
        "createProblemPackageUpload",
        "problem_package:write",
        Oidc,
        IdempotentCreate,
        201,
        false,
        true
    ),
    op!(
        Public,
        Post,
        "/api/v1/courses/{courseId}/problem-package-uploads/{uploadId}/complete",
        "completeProblemPackageUpload",
        "problem_package:write",
        Oidc,
        IdempotentRevisioned,
        201,
        false,
        true
    ),
    op!(
        Public,
        Get,
        "/api/v1/courses/{courseId}/problem-packages/{packageId}",
        "getProblemPackage",
        "problem_package:read",
        Oidc,
        None,
        200,
        false,
        true
    ),
    op!(
        Public,
        Post,
        "/api/v1/courses/{courseId}/llm-egress-policies",
        "createCourseLlmPolicy",
        "llm_policy:write",
        Oidc,
        IdempotentCreate,
        201,
        false,
        true
    ),
    op!(
        Public,
        Get,
        "/api/v1/courses/{courseId}/llm-egress-policies/active",
        "getActiveCourseLlmPolicy",
        "llm_policy:read",
        Oidc,
        None,
        200,
        false,
        true
    ),
    op!(
        Public,
        Post,
        "/api/v1/courses/{courseId}/agent-runs",
        "createAgentRun",
        "agent_run:write",
        Oidc,
        IdempotentCreate,
        202,
        true,
        true
    ),
    op!(
        Public,
        Get,
        "/api/v1/courses/{courseId}/agent-runs/{runId}",
        "getAgentRun",
        "agent_run:read",
        Oidc,
        None,
        200,
        false,
        true
    ),
    op!(
        Public,
        Post,
        "/api/v1/courses/{courseId}/agent-runs/{runId}/cancel",
        "cancelAgentRun",
        "agent_run:write",
        Oidc,
        IdempotentRevisioned,
        202,
        false,
        true
    ),
    op!(
        Public,
        Post,
        "/api/v1/courses/{courseId}/agent-runs/{runId}/tracks/{track}/retry",
        "retryAgentRunTrack",
        "agent_run:write",
        Oidc,
        IdempotentRevisioned,
        202,
        true,
        true
    ),
    op!(
        Public,
        Get,
        "/api/v1/courses/{courseId}/environment-candidates/{candidateId}",
        "getEnvironmentCandidate",
        "candidate:read",
        Oidc,
        None,
        200,
        false,
        true
    ),
    op!(
        Public,
        Post,
        "/api/v1/courses/{courseId}/environment-candidates/{candidateId}/decisions",
        "appendEnvironmentCandidateDecision",
        "candidate:approve",
        Oidc,
        IdempotentRevisioned,
        201,
        false,
        false
    ),
    op!(
        Public,
        Get,
        "/api/v1/courses/{courseId}/evaluation-candidates/{candidateId}",
        "getEvaluationCandidate",
        "candidate:read",
        Oidc,
        None,
        200,
        false,
        true
    ),
    op!(
        Public,
        Post,
        "/api/v1/courses/{courseId}/evaluation-candidates/{candidateId}/decisions",
        "appendEvaluationCandidateDecision",
        "candidate:approve",
        Oidc,
        IdempotentRevisioned,
        201,
        false,
        false
    ),
    op!(
        Public,
        Post,
        "/api/v1/courses/{courseId}/environment-template-releases",
        "createEnvironmentTemplateRelease",
        "release:publish",
        Oidc,
        IdempotentCreate,
        202,
        true,
        true
    ),
    op!(
        Public,
        Get,
        "/api/v1/courses/{courseId}/environment-template-releases",
        "listEnvironmentTemplateReleases",
        "release:read",
        Oidc,
        None,
        200,
        false,
        true
    ),
    op!(
        Public,
        Get,
        "/api/v1/courses/{courseId}/environment-template-releases/{releaseId}",
        "getEnvironmentTemplateRelease",
        "release:read",
        Oidc,
        None,
        200,
        false,
        true
    ),
    op!(
        Public,
        Post,
        "/api/v1/courses/{courseId}/environment-template-releases/{releaseId}/withdraw",
        "withdrawEnvironmentTemplateRelease",
        "release:withdraw",
        Oidc,
        IdempotentRevisioned,
        201,
        false,
        false
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
        true
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
        true
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
        true
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
        true
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
        true
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
        true
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
        true
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
        true
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
        true
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
        true
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
        true
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
        true
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
        true
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
        true
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
        true
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
        true
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
        false
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
        true
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
        true
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
        true
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
        true
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
        true
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
        true
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
        true
    ),
    op!(
        Public,
        Get,
        "/api/v1/events",
        "streamCourseEvents",
        "events:read",
        Oidc,
        None,
        200,
        false,
        true
    ),
    op!(
        GatewayInternal,
        Post,
        "/internal/v1/environments/{environmentId}/owner:resolve",
        "resolveEnvironmentOwner",
        "environment:resolve_owner",
        ServiceMtls,
        None,
        200,
        false,
        true
    ),
    op!(
        GatewayInternal,
        Post,
        "/internal/v1/environments/{environmentId}/endpoint-eligibility:resolve",
        "resolveEndpointEligibility",
        "environment:resolve_endpoint_eligibility",
        ServiceMtls,
        None,
        200,
        false,
        true
    ),
    op!(
        GatewayInternal,
        Post,
        "/internal/v1/ssh/authorize",
        "authorizeSsh",
        "gateway:ssh_authorize",
        ServiceMtls,
        None,
        200,
        false,
        false
    ),
    op!(
        GatewayInternal,
        Post,
        "/internal/v1/sessions",
        "createGatewaySession",
        "gateway:session_write",
        ServiceMtls,
        IdempotentCreate,
        201,
        false,
        true
    ),
    op!(
        GatewayInternal,
        Post,
        "/internal/v1/sessions/{sessionId}/heartbeat",
        "heartbeatGatewaySession",
        "gateway:session_write",
        ServiceMtls,
        IdempotentRevisioned,
        204,
        false,
        true
    ),
    op!(
        GatewayInternal,
        Post,
        "/internal/v1/sessions/{sessionId}/close",
        "closeGatewaySession",
        "gateway:session_write",
        ServiceMtls,
        IdempotentRevisioned,
        204,
        false,
        true
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
            && operation.security != Security::ServiceMtls
        {
            return Err(HttpContractError::PublicInternalLeak);
        }
        if operation.mutation != MutationContract::None && operation.method == Method::Get {
            return Err(HttpContractError::InvalidOperationCatalog);
        }
        let authorization = operation_authorization(operation.operation_id)
            .ok_or(HttpContractError::InvalidOperationCatalog)?;
        if authorization.allowed_roles.is_empty()
            || (operation.security == Security::ServiceMtls
                && authorization.scope != OperationScopeKind::Service)
        {
            return Err(HttpContractError::InvalidOperationCatalog);
        }
    }
    if OPERATION_AUTHORIZATIONS.len() != OPERATIONS.len() {
        return Err(HttpContractError::InvalidOperationCatalog);
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
    #[error("public and internal security surfaces are mixed")]
    PublicInternalLeak,
    #[error("cursor or page limit is invalid")]
    InvalidCursorPage,
    #[error("environment request or query is invalid")]
    InvalidEnvironmentQuery,
    #[error("internal service identity or canonical response is invalid")]
    InvalidInternalIdentity,
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
}
