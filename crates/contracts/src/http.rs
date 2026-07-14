//! Infrastructure-neutral REST, SSE, concurrency, and security contract catalog.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    AccessGrantId, ApprovalId, CandidateId, CourseId, DiagnosticCode, EndpointId, EnvironmentId,
    OperationId, ProblemPackageId, ReleaseId, Revision, Sequence, Sha256Digest, UploadSessionId,
    UtcTimestamp,
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

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateEnvironmentTemplateReleaseRequest {
    pub candidate_id: CandidateId,
    pub candidate_revision: Revision,
    pub environment_spec_sha256: Sha256Digest,
    pub runtime_kind: crate::authoring::RuntimeKind,
    pub approval_id: ApprovalId,
    pub artifact: crate::supply_chain::ImageArtifact,
    pub image_policy_evaluation: crate::supply_chain::ImagePolicyEvaluation,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateEnvironmentRequest {
    pub course_id: CourseId,
    pub release_id: ReleaseId,
    pub release_version: u64,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FreezeSubmissionRequest {
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
    pub environment_id: EnvironmentId,
    pub environment_revision: Revision,
    pub endpoint_ids: Vec<EndpointId>,
    pub expires_at: UtcTimestamp,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RevokeAccessGrantRequest {
    pub grant_id: AccessGrantId,
    pub reason_code: String,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CursorPage<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
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
    pub cursor: Sequence,
    pub event: String,
    pub data: T,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SseResume {
    Beginning,
    After(Sequence),
}
pub fn resolve_sse_resume(
    last_event_id: Option<Sequence>,
    after: Option<Sequence>,
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
    }
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventStreamQuery {
    pub course_id: CourseId,
    pub after: Option<Sequence>,
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
        assert!(matches!(
            resolve_sse_resume(Some(Sequence(7)), Some(Sequence(7))),
            Ok(SseResume::After(Sequence(7)))
        ));
        assert!(matches!(
            resolve_sse_resume(Some(Sequence(7)), Some(Sequence(8))),
            Err(HttpContractError::ConflictingSseCursor)
        ));
    }
}
