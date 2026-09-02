use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};

/// Stable machine-readable diagnostic code.
///
/// Consumers must treat an unknown `LW_*` code as blocking. The newtype is intentionally open so
/// additive diagnostics do not force a wire-version change.
#[derive(Clone, Debug, Eq, Hash, JsonSchema, Ord, PartialEq, PartialOrd, Serialize)]
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

impl<'de> Deserialize<'de> for DiagnosticCode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::parse(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
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

        /// Coarse-grained registry for the private single-university deployment.
        /// Each code is a stable coarse class; `detail` carries the per-domain
        /// context. The newtype remains open so unknown `LW_*` codes stay blocking.
        pub const REGISTERED_DIAGNOSTICS: &[&str] = &[$($value),+];
    };
}

diagnostic_codes! {
    CONTRACT_DOCUMENT_INVALID => "LW_CONTRACT_DOCUMENT_INVALID",
    IDEMPOTENCY_CONFLICT => "LW_IDEMPOTENCY_CONFLICT",
    REVISION_CONFLICT => "LW_REVISION_CONFLICT",
    INVALID_REQUEST => "LW_INVALID_REQUEST",
    NOT_FOUND => "LW_NOT_FOUND",
    CONFLICT => "LW_CONFLICT",
    UNAUTHORIZED => "LW_UNAUTHORIZED",
    ACCESS_DENIED => "LW_ACCESS_DENIED",
    RATE_LIMITED => "LW_RATE_LIMITED",
    PROVIDER_UNAVAILABLE => "LW_PROVIDER_UNAVAILABLE",
    PROVIDER_TIMEOUT => "LW_PROVIDER_TIMEOUT",
    PROVIDER_REJECTED => "LW_PROVIDER_REJECTED",
    OUTBOX_PUBLISH_FAILED => "LW_OUTBOX_PUBLISH_FAILED",
    OUTBOX_PUBLISH_TIMEOUT => "LW_OUTBOX_PUBLISH_TIMEOUT",
    OUTBOX_FENCE_LOST => "LW_OUTBOX_FENCE_LOST",
    DATABASE_FAILED => "LW_DATABASE_FAILED",
    HASH_MISMATCH => "LW_HASH_MISMATCH",
    EVIDENCE_INVALID => "LW_EVIDENCE_INVALID",
    EVIDENCE_STALE => "LW_EVIDENCE_STALE",
    INTERNAL_ERROR => "LW_INTERNAL_ERROR",
    RESOURCE_EXHAUSTED => "LW_RESOURCE_EXHAUSTED",
    SSE_CURSOR_INVALID => "LW_SSE_CURSOR_INVALID",
    SSE_CURSOR_EXPIRED => "LW_SSE_CURSOR_EXPIRED",
    SSE_CURSOR_CONFLICT => "LW_SSE_CURSOR_CONFLICT",
    IMAGE_VERIFICATION_FAILED => "LW_IMAGE_VERIFICATION_FAILED",
    CONSOLE_UNAVAILABLE => "LW_CONSOLE_UNAVAILABLE",
    SUBMISSION_INVALID => "LW_SUBMISSION_INVALID",
    EVALUATION_INVALID => "LW_EVALUATION_INVALID",
    ENVIRONMENT_UNAVAILABLE => "LW_ENVIRONMENT_UNAVAILABLE",
    OBJECT_UNAVAILABLE => "LW_OBJECT_UNAVAILABLE",
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
