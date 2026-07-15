use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Stable machine-readable diagnostic code.
///
/// Consumers must treat an unknown `LW_*` code as blocking. The newtype is intentionally open so
/// additive diagnostics do not force a wire-version change.
#[derive(
    Clone, Debug, Deserialize, Eq, Hash, JsonSchema, Ord, PartialEq, PartialOrd, Serialize,
)]
#[serde(transparent)]
pub struct DiagnosticCode(String);

impl DiagnosticCode {
    /// Creates a registered or forward-compatible LabWeaver diagnostic code.
    ///
    /// # Errors
    ///
    /// Returns an error unless the value starts with `LW_` and contains only ASCII uppercase
    /// letters, digits, and underscores.
    pub fn parse(value: impl Into<String>) -> Result<Self, DiagnosticCodeError> {
        let value = value.into();
        if value.len() < 4
            || value.len() > 96
            || !value.starts_with("LW_")
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
        {
            return Err(DiagnosticCodeError(value));
        }
        Ok(Self(value))
    }

    /// Returns the wire value.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Creates a compile-time registered code.
    #[must_use]
    pub fn registered(value: &'static str) -> Self {
        debug_assert!(value.starts_with("LW_"));
        Self(value.to_owned())
    }
}

/// Failure returned for malformed diagnostic codes.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("invalid LabWeaver diagnostic code: {0}")]
pub struct DiagnosticCodeError(String);

/// One safe field-level contract violation.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Violation {
    /// JSON Pointer or stable logical field path.
    pub field: String,
    /// Stable diagnostic for this field.
    pub code: DiagnosticCode,
    /// Bounded, client-safe explanation.
    pub message: String,
}

/// RFC 9457 Problem Details with LabWeaver correlation and retry semantics.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProblemDetails {
    /// Stable problem type URI.
    #[serde(rename = "type")]
    pub problem_type: String,
    /// Short safe title.
    pub title: String,
    /// HTTP status represented by the response.
    pub status: u16,
    /// Safe detail; never includes secrets, submissions, or raw provider payloads.
    pub detail: String,
    /// Request-local problem instance URI.
    pub instance: String,
    /// Stable LabWeaver diagnostic.
    pub diagnostic_code: DiagnosticCode,
    /// Request correlation identity.
    pub request_id: String,
    /// Distributed trace identity, when available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    /// Whether the same operation may be retried without changing its input.
    pub retryable: bool,
    /// Bounded field violations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub violations: Vec<Violation>,
}

macro_rules! diagnostic_codes {
    ($($name:ident => $value:literal),+ $(,)?) => {
        $(pub const $name: &str = $value;)+

        /// Complete registry frozen by Issue #45.
        pub const REGISTERED_DIAGNOSTICS: &[&str] = &[$($value),+];
    };
}

diagnostic_codes! {
    CONTRACT_DOCUMENT_INVALID => "LW_CONTRACT_DOCUMENT_INVALID",
    IDEMPOTENCY_CONFLICT => "LW_IDEMPOTENCY_CONFLICT",
    REVISION_CONFLICT => "LW_REVISION_CONFLICT",
    HTTP_ROUTE_NOT_FOUND => "LW_HTTP_ROUTE_NOT_FOUND",
    HTTP_REQUEST_ID_INVALID => "LW_HTTP_REQUEST_ID_INVALID",
    LLM_PROVIDER_BINDING_REQUIRED => "LW_LLM_PROVIDER_BINDING_REQUIRED",
    LLM_MODEL_REQUIRED => "LW_LLM_MODEL_REQUIRED",
    LLM_POLICY_REVISION_MISMATCH => "LW_LLM_POLICY_REVISION_MISMATCH",
    LLM_EGRESS_DENIED => "LW_LLM_EGRESS_DENIED",
    LLM_REFUSED => "LW_LLM_REFUSED",
    LLM_RATE_LIMITED => "LW_LLM_RATE_LIMITED",
    LLM_UPSTREAM_UNAVAILABLE => "LW_LLM_UPSTREAM_UNAVAILABLE",
    LLM_TIMEOUT => "LW_LLM_TIMEOUT",
    LLM_CANCELLED => "LW_LLM_CANCELLED",
    LLM_SCHEMA_INVALID => "LW_LLM_SCHEMA_INVALID",
    LLM_PROTECTED_FIELD => "LW_LLM_PROTECTED_FIELD",
    AGENT_RUNTIME_BINDING_REQUIRED => "LW_AGENT_RUNTIME_BINDING_REQUIRED",
    AGENT_RUNTIME_IDENTITY_INVALID => "LW_AGENT_RUNTIME_IDENTITY_INVALID",
    AGENT_RUNTIME_UNAVAILABLE => "LW_AGENT_RUNTIME_UNAVAILABLE",
    AGENT_RUNTIME_PROTOCOL_INVALID => "LW_AGENT_RUNTIME_PROTOCOL_INVALID",
    AGENT_RUNTIME_FAILED => "LW_AGENT_RUNTIME_FAILED",
    AGENT_RUNTIME_LIMIT_EXCEEDED => "LW_AGENT_RUNTIME_LIMIT_EXCEEDED",
    AGENT_RUNTIME_OUTPUT_LIMIT_EXCEEDED => "LW_AGENT_RUNTIME_OUTPUT_LIMIT_EXCEEDED",
    AGENT_RUN_STATE_CONFLICT => "LW_AGENT_RUN_STATE_CONFLICT",
    AGENT_PERSISTENCE_FAILED => "LW_AGENT_PERSISTENCE_FAILED",
    CANDIDATE_DEPENDENCY_STALE => "LW_CANDIDATE_DEPENDENCY_STALE",
    CANDIDATE_APPROVAL_REQUIRED => "LW_CANDIDATE_APPROVAL_REQUIRED",
    RELEASE_BINDING_INCOMPLETE => "LW_RELEASE_BINDING_INCOMPLETE",
    RELEASE_WITHDRAWN => "LW_RELEASE_WITHDRAWN",
    IMAGE_DIGEST_MISMATCH => "LW_IMAGE_DIGEST_MISMATCH",
    IMAGE_CRITICAL_VULNERABILITY => "LW_IMAGE_CRITICAL_VULNERABILITY",
    IMAGE_EVIDENCE_STALE => "LW_IMAGE_EVIDENCE_STALE",
    IMAGE_SIGNATURE_INVALID => "LW_IMAGE_SIGNATURE_INVALID",
    IMAGE_TRUST_UNAVAILABLE => "LW_IMAGE_TRUST_UNAVAILABLE",
    ENV_TRANSITION_INVALID => "LW_ENV_TRANSITION_INVALID",
    ENV_PROVIDER_BINDING_REQUIRED => "LW_ENV_PROVIDER_BINDING_REQUIRED",
    ENV_CLEANUP_INCOMPLETE => "LW_ENV_CLEANUP_INCOMPLETE",
    ENV_OWNER_CALLER_UNTRUSTED => "LW_ENV_OWNER_CALLER_UNTRUSTED",
    ENV_OWNER_POLICY_INVALID => "LW_ENV_OWNER_POLICY_INVALID",
    ENV_OWNER_SCOPE_MISMATCH => "LW_ENV_OWNER_SCOPE_MISMATCH",
    ENV_OWNER_UNAVAILABLE => "LW_ENV_OWNER_UNAVAILABLE",
    ENV_OWNER_RESOLVER_UNAVAILABLE => "LW_ENV_OWNER_RESOLVER_UNAVAILABLE",
    ENV_OWNER_RESPONSE_INVALID => "LW_ENV_OWNER_RESPONSE_INVALID",
    ENV_OWNER_SHUTDOWN_SIGNAL_FAILED => "LW_ENV_OWNER_SHUTDOWN_SIGNAL_FAILED",
    ENVIRONMENT_ELIGIBILITY_EXPIRED => "LW_ENVIRONMENT_ELIGIBILITY_EXPIRED",
    ENVIRONMENT_LEASE_AUTHORIZATION_REQUIRED => "LW_ENVIRONMENT_LEASE_AUTHORIZATION_REQUIRED",
    ENVIRONMENT_LEASE_AUTHORIZATION_INVALID => "LW_ENVIRONMENT_LEASE_AUTHORIZATION_INVALID",
    ENVIRONMENT_LEASE_VERIFICATION_REJECTED => "LW_ENVIRONMENT_LEASE_VERIFICATION_REJECTED",
    ENVIRONMENT_PROVIDER_STEP_OVERFLOW => "LW_ENVIRONMENT_PROVIDER_STEP_OVERFLOW",
    ENVIRONMENT_PROVIDER_UNAVAILABLE => "LW_ENVIRONMENT_PROVIDER_UNAVAILABLE",
    ENVIRONMENT_PROVIDER_TRANSIENT => "LW_ENVIRONMENT_PROVIDER_TRANSIENT",
    ENVIRONMENT_PROVIDER_REJECTED => "LW_ENVIRONMENT_PROVIDER_REJECTED",
    ENVIRONMENT_PROVIDER_OBSERVATION_INVALID => "LW_ENVIRONMENT_PROVIDER_OBSERVATION_INVALID",
    ENVIRONMENT_PROVIDER_CLEANUP_FAILED => "LW_ENVIRONMENT_PROVIDER_CLEANUP_FAILED",
    ENVIRONMENT_PROVIDER_TIMEOUT => "LW_ENVIRONMENT_PROVIDER_TIMEOUT",
    ENVIRONMENT_MIGRATION_RECONCILE_REQUIRED => "LW_ENVIRONMENT_MIGRATION_RECONCILE_REQUIRED",
    ACCESS_DENIED => "LW_ACCESS_DENIED",
    ACCESS_GRANT_EXPIRED => "LW_ACCESS_GRANT_EXPIRED",
    ACCESS_GRANT_REVOKED => "LW_ACCESS_GRANT_REVOKED",
    ACCESS_KEY_INVALID => "LW_ACCESS_KEY_INVALID",
    ACCESS_KEY_DUPLICATE => "LW_ACCESS_KEY_DUPLICATE",
    ACCESS_ENDPOINT_UNHEALTHY => "LW_ACCESS_ENDPOINT_UNHEALTHY",
    AUTH_CONFIG_URL_INVALID => "LW_AUTH_CONFIG_URL_INVALID",
    AUTH_CONFIG_BINDING_MISSING => "LW_AUTH_CONFIG_BINDING_MISSING",
    AUTH_CONFIG_SESSION_TTL_INVALID => "LW_AUTH_CONFIG_SESSION_TTL_INVALID",
    AUTH_CONFIG_ORIGIN_INVALID => "LW_AUTH_CONFIG_ORIGIN_INVALID",
    AUTH_REQUIRED => "LW_AUTH_REQUIRED",
    AUTH_SESSION_REJECTED => "LW_AUTH_SESSION_REJECTED",
    AUTH_SESSION_REVOKED => "LW_AUTH_SESSION_REVOKED",
    AUTH_TOKEN_INVALID => "LW_AUTH_TOKEN_INVALID",
    AUTH_TOKEN_EXPIRED => "LW_AUTH_TOKEN_EXPIRED",
    AUTH_ROLE_DENIED => "LW_AUTH_ROLE_DENIED",
    AUTH_COURSE_SCOPE_DENIED => "LW_AUTH_COURSE_SCOPE_DENIED",
    AUTH_PROJECT_SCOPE_DENIED => "LW_AUTH_PROJECT_SCOPE_DENIED",
    AUTH_CSRF_REQUIRED => "LW_AUTH_CSRF_REQUIRED",
    AUTH_CSRF_REJECTED => "LW_AUTH_CSRF_REJECTED",
    AUTH_OIDC_STATE_REQUIRED => "LW_AUTH_OIDC_STATE_REQUIRED",
    AUTH_OIDC_STATE_REJECTED => "LW_AUTH_OIDC_STATE_REJECTED",
    AUTH_JWKS_UNAVAILABLE => "LW_AUTH_JWKS_UNAVAILABLE",
    AUTH_MEMBERSHIP_UNAVAILABLE => "LW_AUTH_MEMBERSHIP_UNAVAILABLE",
    AUTH_SERVICE_IDENTITY_DENIED => "LW_AUTH_SERVICE_IDENTITY_DENIED",
    AUTH_ENVIRONMENT_SCOPE_DENIED => "LW_AUTH_ENVIRONMENT_SCOPE_DENIED",
    AUTH_OWNER_RESOLVER_UNAVAILABLE => "LW_AUTH_OWNER_RESOLVER_UNAVAILABLE",
    AUTH_OWNER_RESPONSE_INVALID => "LW_AUTH_OWNER_RESPONSE_INVALID",
    AUTH_TIMESTAMP_INVALID => "LW_AUTH_TIMESTAMP_INVALID",
    AUTH_LOGOUT_TOKEN_REPLAYED => "LW_AUTH_LOGOUT_TOKEN_REPLAYED",
    AUTH_KEYRING_ACTIVE_KEY_MISSING => "LW_AUTH_KEYRING_ACTIVE_KEY_MISSING",
    AUTH_KEYRING_MATERIAL_INVALID => "LW_AUTH_KEYRING_MATERIAL_INVALID",
    AUTH_KEYRING_KEY_UNKNOWN => "LW_AUTH_KEYRING_KEY_UNKNOWN",
    AUTH_KEYRING_CIPHERTEXT_INVALID => "LW_AUTH_KEYRING_CIPHERTEXT_INVALID",
    AUTH_KEYRING_ENCRYPTION_FAILED => "LW_AUTH_KEYRING_ENCRYPTION_FAILED",
    AUTH_KEYRING_AUTHENTICATION_FAILED => "LW_AUTH_KEYRING_AUTHENTICATION_FAILED",
    SUBMISSION_PATH_UNSAFE => "LW_SUBMISSION_PATH_UNSAFE",
    SUBMISSION_PATH_CONFLICT => "LW_SUBMISSION_PATH_CONFLICT",
    SUBMISSION_LIMIT_EXCEEDED => "LW_SUBMISSION_LIMIT_EXCEEDED",
    SUBMISSION_HASH_MISMATCH => "LW_SUBMISSION_HASH_MISMATCH",
    SUBMISSION_FREEZE_INCOMPLETE => "LW_SUBMISSION_FREEZE_INCOMPLETE",
    EVENT_ENVELOPE_INVALID => "LW_EVENT_ENVELOPE_INVALID",
    EVENT_SUBJECT_MISMATCH => "LW_EVENT_SUBJECT_MISMATCH",
    EVENT_SEQUENCE_STALE => "LW_EVENT_SEQUENCE_STALE",
    EVENT_SEQUENCE_GAP => "LW_EVENT_SEQUENCE_GAP",
    SSE_CURSOR_EXPIRED => "LW_SSE_CURSOR_EXPIRED",
    SSE_CURSOR_GAP => "LW_SSE_CURSOR_GAP",
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{DiagnosticCode, REGISTERED_DIAGNOSTICS};

    #[test]
    fn diagnostic_registry_is_unique_and_valid() {
        let unique = REGISTERED_DIAGNOSTICS
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        assert_eq!(unique.len(), REGISTERED_DIAGNOSTICS.len());
        for code in REGISTERED_DIAGNOSTICS {
            assert!(DiagnosticCode::parse(*code).is_ok());
        }
    }
}
