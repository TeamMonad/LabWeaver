//! LabWeaver's versioned, infrastructure-independent public contract source of truth.
//!
//! This crate owns wire types, validation, lifecycle semantics, REST/SSE descriptions,
//! CloudEvents payloads, and generated schemas. It deliberately contains no Axum router,
//! persistence implementation, provider integration, or fallback behavior.
#![allow(
    missing_docs,
    reason = "wire fields are normatively documented by generated schemas and formal contract documents"
)]
#![allow(
    clippy::case_sensitive_file_extension_comparisons,
    clippy::doc_markdown,
    clippy::missing_errors_doc,
    clippy::needless_pass_by_value,
    clippy::too_many_lines,
    reason = "contract catalog and generated-schema APIs favor stable wire terminology and exhaustive colocated declarations"
)]

pub mod access;
pub mod auth;
pub mod authoring;
pub mod diagnostic;
pub mod environment;
pub mod evaluation;
pub mod events;
pub mod foundation;
pub mod http;
pub mod schema;
pub mod submission;
pub mod supply_chain;

pub use auth::{
    AuthSession, AuthenticatedActor, AuthorizationDecision, AuthorizationDecisionRequest,
    AuthorizationScope, CourseMembership, CsrfTokenResponse, MembershipState, PlatformRole,
    ProjectMembership,
};
pub use diagnostic::{DiagnosticCode, ProblemDetails, Violation};
pub use foundation::*;
pub use http::{OperationScopeKind, operation_authorization};

/// Stable public REST major version.
pub const API_VERSION: &str = "v1";

/// Stable health response shared by all service runtime adapters.
#[derive(Clone, Debug, PartialEq, serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HealthResponse {
    /// Contract version.
    pub version: &'static str,
    /// Stable service identifier.
    pub service: &'static str,
    /// Current health state.
    pub status: &'static str,
}
