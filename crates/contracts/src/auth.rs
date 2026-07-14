//! OIDC actor, membership, and authorization-decision wire contracts.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    ActorId, BffSessionId, CourseId, DiagnosticCode, EnvironmentId, ProjectId, Revision,
    UtcTimestamp,
};

/// Base role asserted by the configured OIDC issuer.
#[derive(
    Clone, Copy, Debug, Deserialize, Eq, JsonSchema, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum PlatformRole {
    /// Course teacher with an active course membership.
    Teacher,
    /// Course student with an active course membership.
    Student,
    /// Platform operator; course scope is still required for course data.
    PlatformAdmin,
}

/// Safe actor identity derived from a verified OIDC token.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthenticatedActor {
    /// Local durable actor identity.
    pub actor_id: ActorId,
    /// Verified base roles.
    pub roles: Vec<PlatformRole>,
    /// Verified identity expiration.
    pub expires_at: UtcTimestamp,
}

/// Safe browser-session representation. OIDC subjects, tokens, and provider
/// session identifiers are intentionally excluded from this public DTO.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthSession {
    /// Authenticated durable actor.
    pub actor: AuthenticatedActor,
    /// Current authorization revision for the returned effective scopes.
    pub authorization_revision: Revision,
    /// Effective scopes known by the Access authority at response time.
    pub scopes: Vec<AuthorizationScope>,
    /// Earliest expiry that invalidates this session representation.
    pub expires_at: UtcTimestamp,
}

/// Synchronizer token returned only to an authenticated browser session.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CsrfTokenResponse {
    /// Opaque token to send in the `X-CSRF-Token` request header.
    pub csrf_token: String,
    /// Session expiry bound to this token.
    pub expires_at: UtcTimestamp,
}

/// Authoritative membership state owned by Access Service.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MembershipState {
    /// Membership may authorize matching operations.
    Active,
    /// Membership is temporarily disabled and must deny access.
    Suspended,
    /// Membership was permanently revoked and must deny access.
    Revoked,
}

/// Course-local role grant with revision and expiry.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CourseMembership {
    /// Course scope.
    pub course_id: CourseId,
    /// Actor scope.
    pub actor_id: ActorId,
    /// Role permitted by this course.
    pub role: PlatformRole,
    /// Current lifecycle state.
    pub state: MembershipState,
    /// Monotonic authorization revision.
    pub revision: Revision,
    /// Optional hard expiry.
    pub expires_at: Option<UtcTimestamp>,
}

/// Project-local scope nested under a course membership.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectMembership {
    /// Owning course.
    pub course_id: CourseId,
    /// Project scope.
    pub project_id: ProjectId,
    /// Actor scope.
    pub actor_id: ActorId,
    /// Role permitted by this project.
    pub role: PlatformRole,
    /// Current lifecycle state.
    pub state: MembershipState,
    /// Monotonic authorization revision.
    pub revision: Revision,
    /// Optional hard expiry.
    pub expires_at: Option<UtcTimestamp>,
}

/// Resource scope evaluated by the authorization boundary.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AuthorizationScope {
    /// Platform-wide endpoint with no course resource.
    Global,
    /// Course-scoped endpoint.
    Course { course_id: CourseId },
    /// Project-scoped endpoint.
    Project {
        course_id: CourseId,
        project_id: ProjectId,
    },
    /// Environment owner resolution is supplied by the owning service.
    Environment {
        course_id: CourseId,
        environment_id: EnvironmentId,
        /// Exact Environment-authoritative revision resolved for this request.
        environment_revision: Revision,
    },
    /// Internal service endpoint authenticated by mTLS.
    Service { service_id: String },
}

/// Safe result returned to a trusted internal caller after scope evaluation.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthorizationDecision {
    /// Authenticated actor.
    pub actor: AuthenticatedActor,
    /// Requested resource scope.
    pub scope: AuthorizationScope,
    /// Authorization revision which invalidates derived Gateway decisions.
    pub authorization_revision: Revision,
    /// Scope-owner revision. For Environment scope this is the exact
    /// Environment revision returned by the owner resolver.
    pub scope_revision: Revision,
    /// Earliest identity or membership expiry; callers must not cache past this point.
    pub valid_until: UtcTimestamp,
    /// Stable denial diagnostic, absent for permits.
    pub diagnostic_code: Option<DiagnosticCode>,
}

/// Trusted-Gateway authorization request. The caller identity is taken from
/// mTLS and is never accepted from a browser-controlled header.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthorizationDecisionRequest {
    /// Stable operation catalog identifier.
    pub operation_id: String,
    /// Actor whose session or bearer token was already authenticated.
    pub actor_id: ActorId,
    /// Opaque BFF session identifier presented only by the trusted Gateway.
    /// Access verifies that the live session belongs to `actor_id`; it is never
    /// accepted from a browser header.
    pub session_id: BffSessionId,
    /// Scope requiring evaluation.
    pub scope: AuthorizationScope,
    /// Revision observed by the caller, if it holds a prior decision.
    pub authorization_revision: Option<Revision>,
    /// Scope revision observed by the caller, if it holds a prior decision.
    pub scope_revision: Option<Revision>,
}
