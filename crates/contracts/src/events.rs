//! Versioned NATS subjects and CloudEvents 1.0 wire contracts.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::authoring::{CandidateApproval, CandidateDecision, EnvironmentSpec};
use crate::submission::FrozenSubmission;
use crate::supply_chain::{BuildRequest, EnvironmentTemplateRelease};
use crate::{
    AccessGrantId, ActorId, AgentRunId, BuildRequestId, CourseId, EnvironmentId, EventId,
    FrozenSubmissionId, GatewaySessionId, ReleaseId, Revision, Sequence, Sha256Digest,
    SshPublicKeyId, UtcTimestamp,
};

pub const SPEC_VERSION: &str = "1.0";
pub const DATA_SCHEMA_BASE: &str = "https://schemas.labweaver.io/contracts/v1/events";

pub mod subjects {
    pub const AGENT_RUN_REQUESTED: &str = "labweaver.agent.run.requested.v1";
    pub const AGENT_RUN_COMPLETED: &str = "labweaver.agent.run.completed.v1";
    pub const AGENT_RUN_FAILED: &str = "labweaver.agent.run.failed.v1";
    pub const AGENT_BUILD_REQUESTED: &str = "labweaver.control.agent_build.requested.v1";
    pub const AGENT_BUILD_COMPLETED: &str = "labweaver.agent.build.completed.v1";
    pub const AGENT_BUILD_FAILED: &str = "labweaver.agent.build.failed.v1";
    pub const ENVIRONMENT_PROVISION_REQUESTED: &str =
        "labweaver.environment.instance.provision_requested.v1";
    pub const ENVIRONMENT_READY: &str = "labweaver.environment.instance.ready.v1";
    pub const ENVIRONMENT_FAILED: &str = "labweaver.environment.instance.failed.v1";
    pub const ENVIRONMENT_DELETE_REQUESTED: &str =
        "labweaver.environment.instance.delete_requested.v1";
    pub const ENVIRONMENT_OPERATION_ACCEPTED: &str =
        "labweaver.environment.instance.operation_accepted.v1";
    pub const ENVIRONMENT_STATE_CHANGED: &str = "labweaver.environment.instance.state_changed.v1";
    pub const ENVIRONMENT_LIFECYCLE_REQUESTED: &str =
        "labweaver.environment.instance.lifecycle_requested.v1";
    pub const ACCESS_GRANT_CREATED: &str = "labweaver.access.grant.created.v1";
    pub const ACCESS_GRANT_ACTIVATED: &str = "labweaver.access.grant.activated.v1";
    pub const ACCESS_GRANT_DENIED: &str = "labweaver.access.grant.denied.v1";
    pub const ACCESS_GRANT_EXPIRED: &str = "labweaver.access.grant.expired.v1";
    pub const ACCESS_GRANT_REVOKED: &str = "labweaver.access.grant.revoked.v1";
    pub const ACCESS_SSH_KEY_REVOKED: &str = "labweaver.access.ssh_key.revoked.v1";
    pub const ACCESS_SESSION_TERMINATION_REQUESTED: &str =
        "labweaver.access.session.termination_requested.v1";
    pub const ACCESS_SESSION_CLOSED: &str = "labweaver.access.session.closed.v1";
    pub const ACCESS_SESSION_TERMINATION_OVERDUE: &str =
        "labweaver.access.session.termination_overdue.v1";
    pub const SUBMISSION_FREEZE_REQUESTED: &str =
        "labweaver.evaluation.submission.freeze_requested.v1";
    pub const SUBMISSION_FROZEN: &str = "labweaver.evaluation.submission.frozen.v1";
    pub const LAB_RELEASE_APPROVED: &str = "labweaver.control.lab_release.approved.v1";
    pub const ENVIRONMENT_TEMPLATE_RELEASE_PUBLISHED: &str =
        "labweaver.control.environment_template_release.published.v1";
    pub const ENVIRONMENT_TEMPLATE_RELEASE_WITHDRAWN: &str =
        "labweaver.control.environment_template_release.withdrawn.v1";
}

/// Strict CloudEvents 1.0 envelope carried as structured JSON.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CloudEvent<T> {
    pub specversion: String,
    pub id: EventId,
    pub source: String,
    #[serde(rename = "type")]
    pub event_type: String,
    pub subject: String,
    pub time: UtcTimestamp,
    pub datacontenttype: String,
    pub dataschema: String,
    pub course_id: CourseId,
    pub aggregate_revision: Revision,
    pub aggregate_sequence: Sequence,
    pub trace_id: String,
    pub data: T,
}

impl<T: Serialize> CloudEvent<T> {
    pub fn validate(&self, contract: EventContract) -> Result<(), EventError> {
        if self.specversion != SPEC_VERSION
            || self.datacontenttype != "application/json"
            || self.event_type != contract.event_type
            || self.subject != contract.subject
            || self.dataschema != contract.data_schema()
            || self.source != contract.source()
            || self.aggregate_sequence.0 == 0
            || self.trace_id.trim().is_empty()
        {
            return Err(EventError::EnvelopeMismatch);
        }
        let value = serde_json::to_value(&self.data)
            .map_err(|error| EventError::Serialization(error.to_string()))?;
        reject_protected_payload(&value)?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EventContract {
    pub subject: &'static str,
    pub event_type: &'static str,
    pub schema_name: &'static str,
}

impl EventContract {
    #[must_use]
    pub fn data_schema(self) -> String {
        format!("{DATA_SCHEMA_BASE}/{}.schema.json", self.schema_name)
    }

    /// Returns the authoritative owner identity encoded by the registered subject.
    #[must_use]
    pub fn source(self) -> &'static str {
        if self.subject.starts_with("labweaver.agent.") {
            "urn:labweaver:agent-service"
        } else if self.subject.starts_with("labweaver.environment.") {
            "urn:labweaver:environment-service"
        } else if self.subject.starts_with("labweaver.access.") {
            "urn:labweaver:access-service"
        } else if self.subject.starts_with("labweaver.evaluation.") {
            "urn:labweaver:evaluation-service"
        } else {
            "urn:labweaver:control-service"
        }
    }
}

pub const EVENT_CONTRACTS: &[EventContract] = &[
    EventContract {
        subject: subjects::AGENT_RUN_REQUESTED,
        event_type: subjects::AGENT_RUN_REQUESTED,
        schema_name: "agent-run-requested",
    },
    EventContract {
        subject: subjects::AGENT_RUN_COMPLETED,
        event_type: subjects::AGENT_RUN_COMPLETED,
        schema_name: "agent-run-completed",
    },
    EventContract {
        subject: subjects::AGENT_RUN_FAILED,
        event_type: subjects::AGENT_RUN_FAILED,
        schema_name: "agent-run-failed",
    },
    EventContract {
        subject: subjects::AGENT_BUILD_REQUESTED,
        event_type: subjects::AGENT_BUILD_REQUESTED,
        schema_name: "agent-build-requested",
    },
    EventContract {
        subject: subjects::AGENT_BUILD_COMPLETED,
        event_type: subjects::AGENT_BUILD_COMPLETED,
        schema_name: "agent-build-completed",
    },
    EventContract {
        subject: subjects::AGENT_BUILD_FAILED,
        event_type: subjects::AGENT_BUILD_FAILED,
        schema_name: "agent-build-failed",
    },
    EventContract {
        subject: subjects::ENVIRONMENT_PROVISION_REQUESTED,
        event_type: subjects::ENVIRONMENT_PROVISION_REQUESTED,
        schema_name: "environment-provision-requested",
    },
    EventContract {
        subject: subjects::ENVIRONMENT_READY,
        event_type: subjects::ENVIRONMENT_READY,
        schema_name: "environment-ready",
    },
    EventContract {
        subject: subjects::ENVIRONMENT_FAILED,
        event_type: subjects::ENVIRONMENT_FAILED,
        schema_name: "environment-failed",
    },
    EventContract {
        subject: subjects::ENVIRONMENT_DELETE_REQUESTED,
        event_type: subjects::ENVIRONMENT_DELETE_REQUESTED,
        schema_name: "environment-delete-requested",
    },
    EventContract {
        subject: subjects::ENVIRONMENT_OPERATION_ACCEPTED,
        event_type: subjects::ENVIRONMENT_OPERATION_ACCEPTED,
        schema_name: "environment-operation-accepted",
    },
    EventContract {
        subject: subjects::ENVIRONMENT_STATE_CHANGED,
        event_type: subjects::ENVIRONMENT_STATE_CHANGED,
        schema_name: "environment-state-changed",
    },
    EventContract {
        subject: subjects::ENVIRONMENT_LIFECYCLE_REQUESTED,
        event_type: subjects::ENVIRONMENT_LIFECYCLE_REQUESTED,
        schema_name: "environment-lifecycle-requested",
    },
    EventContract {
        subject: subjects::ACCESS_GRANT_CREATED,
        event_type: subjects::ACCESS_GRANT_CREATED,
        schema_name: "access-grant-created",
    },
    EventContract {
        subject: subjects::ACCESS_GRANT_ACTIVATED,
        event_type: subjects::ACCESS_GRANT_ACTIVATED,
        schema_name: "access-grant-activated",
    },
    EventContract {
        subject: subjects::ACCESS_GRANT_DENIED,
        event_type: subjects::ACCESS_GRANT_DENIED,
        schema_name: "access-grant-denied",
    },
    EventContract {
        subject: subjects::ACCESS_GRANT_EXPIRED,
        event_type: subjects::ACCESS_GRANT_EXPIRED,
        schema_name: "access-grant-expired",
    },
    EventContract {
        subject: subjects::ACCESS_GRANT_REVOKED,
        event_type: subjects::ACCESS_GRANT_REVOKED,
        schema_name: "access-grant-revoked",
    },
    EventContract {
        subject: subjects::ACCESS_SSH_KEY_REVOKED,
        event_type: subjects::ACCESS_SSH_KEY_REVOKED,
        schema_name: "access-ssh-key-revoked",
    },
    EventContract {
        subject: subjects::ACCESS_SESSION_TERMINATION_REQUESTED,
        event_type: subjects::ACCESS_SESSION_TERMINATION_REQUESTED,
        schema_name: "access-session-termination-requested",
    },
    EventContract {
        subject: subjects::ACCESS_SESSION_CLOSED,
        event_type: subjects::ACCESS_SESSION_CLOSED,
        schema_name: "access-session-closed",
    },
    EventContract {
        subject: subjects::ACCESS_SESSION_TERMINATION_OVERDUE,
        event_type: subjects::ACCESS_SESSION_TERMINATION_OVERDUE,
        schema_name: "access-session-termination-overdue",
    },
    EventContract {
        subject: subjects::SUBMISSION_FREEZE_REQUESTED,
        event_type: subjects::SUBMISSION_FREEZE_REQUESTED,
        schema_name: "submission-freeze-requested",
    },
    EventContract {
        subject: subjects::SUBMISSION_FROZEN,
        event_type: subjects::SUBMISSION_FROZEN,
        schema_name: "submission-frozen",
    },
    EventContract {
        subject: subjects::LAB_RELEASE_APPROVED,
        event_type: subjects::LAB_RELEASE_APPROVED,
        schema_name: "lab-release-approved",
    },
    EventContract {
        subject: subjects::ENVIRONMENT_TEMPLATE_RELEASE_PUBLISHED,
        event_type: subjects::ENVIRONMENT_TEMPLATE_RELEASE_PUBLISHED,
        schema_name: "environment-template-release-published",
    },
    EventContract {
        subject: subjects::ENVIRONMENT_TEMPLATE_RELEASE_WITHDRAWN,
        event_type: subjects::ENVIRONMENT_TEMPLATE_RELEASE_WITHDRAWN,
        schema_name: "environment-template-release-withdrawn",
    },
];

pub fn validate_registry() -> Result<(), EventError> {
    let mut subjects = BTreeSet::new();
    let mut event_types = BTreeSet::new();
    for contract in EVENT_CONTRACTS {
        if !contract.subject.ends_with(".v1")
            || contract.event_type != contract.subject
            || !subjects.insert(contract.subject)
            || !event_types.insert(contract.event_type)
        {
            return Err(EventError::RegistryConflict);
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentRunEvent {
    pub run_id: AgentRunId,
    pub attempt: u64,
    pub state: String,
    pub diagnostic_code: Option<String>,
}
/// Complete, approved, immutable command consumed by the Agent build executor.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentBuildRequested {
    pub request: BuildRequest,
    pub approval: CandidateApproval,
    pub idempotency_key: String,
    pub command_sha256: Sha256Digest,
}

impl AgentBuildRequested {
    /// Verifies approval, build-input and canonical command identities before execution.
    pub fn validate(&self) -> Result<(), EventError> {
        self.request
            .validate()
            .map_err(|_| EventError::PayloadIdentityMismatch)?;
        if self.approval.decision != CandidateDecision::Approved
            || self.approval.id != self.request.approval_id
            || self.approval.candidate_id != self.request.candidate_id
            || self.approval.candidate_revision != self.request.candidate_revision
            || self.approval.candidate_sha256 != self.request.candidate_sha256
            || self.idempotency_key.len() < 16
            || self.idempotency_key.len() > 128
            || !self
                .idempotency_key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
        {
            return Err(EventError::PayloadIdentityMismatch);
        }
        let command_sha256 = Sha256Digest::of_canonical(&serde_json::json!({
            "request": self.request,
            "approval": self.approval,
            "idempotencyKey": self.idempotency_key,
        }))
        .map_err(|error| EventError::Serialization(error.to_string()))?;
        if command_sha256 != self.command_sha256 {
            return Err(EventError::PayloadIdentityMismatch);
        }
        Ok(())
    }
}

/// Safe terminal identity emitted after artifact and policy evidence commit atomically.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentBuildCompleted {
    pub build_request_id: BuildRequestId,
    pub artifact_id: crate::ImageArtifactId,
    pub artifact_sha256: Sha256Digest,
    pub policy_evaluation_sha256: Sha256Digest,
}

/// Safe terminal failure emitted only after candidate-resource cleanup was attempted.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentBuildFailed {
    pub build_request_id: BuildRequestId,
    pub command_sha256: Sha256Digest,
    pub diagnostic_code: String,
    pub retryable: bool,
    pub cleanup_verified: bool,
}

impl AgentBuildFailed {
    pub fn validate(&self) -> Result<(), EventError> {
        crate::DiagnosticCode::parse(&self.diagnostic_code)
            .map_err(|_| EventError::PayloadIdentityMismatch)?;
        let cleanup_failed = self.diagnostic_code == "LW_AGENT_BUILD_CLEANUP_FAILED";
        if self.cleanup_verified == cleanup_failed || (self.retryable && !self.cleanup_verified) {
            return Err(EventError::PayloadIdentityMismatch);
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentEvent {
    pub environment_id: EnvironmentId,
    pub generation: u64,
    pub state: String,
    pub operation_id: Option<crate::OperationId>,
    pub diagnostic_code: Option<String>,
}
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccessGrantChanged {
    pub access_grant_id: AccessGrantId,
    pub revision: Revision,
    pub state: String,
    pub effective_at: UtcTimestamp,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SshPublicKeyRevoked {
    pub ssh_public_key_id: SshPublicKeyId,
    pub actor_id: ActorId,
    pub effective_at: UtcTimestamp,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GatewaySessionChanged {
    pub gateway_session_id: GatewaySessionId,
    pub access_grant_id: AccessGrantId,
    pub access_grant_revision: Revision,
    pub state: String,
    pub effective_at: UtcTimestamp,
    pub terminate_by: Option<UtcTimestamp>,
    pub reason_code: String,
}
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubmissionFreezeRequested {
    pub frozen_submission_id: FrozenSubmissionId,
    pub environment_id: EnvironmentId,
    pub manifest_sha256: Sha256Digest,
    pub frozen_by: ActorId,
}

/// Complete immutable submission identity emitted only after database and Object Lock verification.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubmissionFrozen {
    /// Authoritative frozen submission contract, including object version and digest.
    pub submission: FrozenSubmission,
    /// Environment-owned PVC or VM source identity used for both preflight and freeze.
    pub source_identity_sha256: Sha256Digest,
}

impl SubmissionFrozen {
    /// Verifies the embedded immutable contract before publication.
    pub fn validate(&self) -> Result<(), EventError> {
        self.submission
            .validate()
            .map_err(|_| EventError::PayloadIdentityMismatch)
    }
}
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LabReleaseApproved {
    pub release_id: ReleaseId,
    pub version: u64,
    pub environment_spec_sha256: Sha256Digest,
    pub artifact_sha256: Sha256Digest,
}

/// Complete immutable runtime projection consumed by the Environment state owner.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleasePublished {
    pub release: EnvironmentTemplateRelease,
    pub environment_spec: EnvironmentSpec,
    pub projection_sha256: Sha256Digest,
}

impl ReleasePublished {
    /// Verifies the exact approved spec, artifact and projection identity.
    pub fn validate(&self) -> Result<(), EventError> {
        self.release
            .validate()
            .map_err(|_| EventError::PayloadIdentityMismatch)?;
        self.environment_spec
            .validate()
            .map_err(|_| EventError::PayloadIdentityMismatch)?;
        let spec_sha256 = Sha256Digest::of_canonical(&self.environment_spec)
            .map_err(|error| EventError::Serialization(error.to_string()))?;
        if self.environment_spec.runtime.kind() != self.release.runtime_kind
            || spec_sha256 != self.release.environment_spec_sha256
        {
            return Err(EventError::PayloadIdentityMismatch);
        }
        let projection_sha256 = Sha256Digest::of_canonical(&serde_json::json!({
            "release": self.release,
            "environmentSpec": self.environment_spec,
        }))
        .map_err(|error| EventError::Serialization(error.to_string()))?;
        if projection_sha256 != self.projection_sha256 {
            return Err(EventError::PayloadIdentityMismatch);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleaseWithdrawn {
    pub release_id: ReleaseId,
    pub version: u64,
    pub actor_id: ActorId,
    pub reason_code: String,
    pub withdrawn_at: UtcTimestamp,
}

fn reject_protected_payload(value: &serde_json::Value) -> Result<(), EventError> {
    match value {
        serde_json::Value::Object(object) => {
            for (key, nested) in object {
                if protected_event_key(key) {
                    return Err(EventError::ProtectedPayload);
                }
                reject_protected_payload(nested)?;
            }
        }
        serde_json::Value::Array(values) => {
            for nested in values {
                reject_protected_payload(nested)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn protected_event_key(key: &str) -> bool {
    let normalized = key
        .bytes()
        .filter(u8::is_ascii_alphanumeric)
        .map(|byte| byte.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let normalized = String::from_utf8_lossy(&normalized);
    normalized.contains("secret")
        || normalized.contains("token")
        || normalized.contains("privatekey")
        || normalized.contains("apikey")
        || normalized.contains("password")
        || normalized.contains("credential")
        || normalized.contains("privateendpoint")
        || normalized.contains("targethost")
        || normalized.contains("targetport")
        || normalized.contains("submissioncontent")
        || normalized.contains("materialcontent")
        || normalized.contains("normalizedauthorizedkey")
        || normalized.contains("publickeyopenssh")
        || matches!(
            normalized.as_ref(),
            "authorization" | "score" | "pointsawarded"
        )
}

pub fn validate_delivery(previous: Option<Sequence>, current: Sequence) -> Result<(), EventError> {
    if let Some(previous) = previous {
        if current.0 <= previous.0 {
            return Err(EventError::DuplicateOrStale);
        }
        if current.0 != previous.0 + 1 {
            return Err(EventError::SequenceGap);
        }
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum EventError {
    #[error(
        "CloudEvent subject, type, dataschema, or required attribute does not match its registered contract"
    )]
    EnvelopeMismatch,
    #[error("event registry contains a duplicate or unversioned identity")]
    RegistryConflict,
    #[error("event payload contains protected content")]
    ProtectedPayload,
    #[error("event payload identity, approval, or immutable hash does not match")]
    PayloadIdentityMismatch,
    #[error("event aggregate sequence is duplicate or stale")]
    DuplicateOrStale,
    #[error("event aggregate sequence contains a gap")]
    SequenceGap,
    #[error("event payload serialization failed: {0}")]
    Serialization(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn registry_is_unique_and_versioned() -> Result<(), EventError> {
        validate_registry()
    }
    #[test]
    fn delivery_rejects_stale_and_gap() {
        assert!(matches!(
            validate_delivery(Some(Sequence(2)), Sequence(2)),
            Err(EventError::DuplicateOrStale)
        ));
        assert!(matches!(
            validate_delivery(Some(Sequence(2)), Sequence(4)),
            Err(EventError::SequenceGap)
        ));
    }

    #[test]
    fn protected_payload_scan_is_recursive_and_case_insensitive() {
        let payload = serde_json::json!({"safe": [{"Authorization": "Bearer redacted"}]});
        assert!(matches!(
            reject_protected_payload(&payload),
            Err(EventError::ProtectedPayload)
        ));
        for key in [
            "forceCommandToken",
            "normalizedAuthorizedKey",
            "token_sha256",
        ] {
            let payload = serde_json::json!({key: "redacted"});
            assert!(matches!(
                reject_protected_payload(&payload),
                Err(EventError::ProtectedPayload)
            ));
        }
    }

    #[test]
    fn registered_sources_are_owner_specific() {
        for contract in EVENT_CONTRACTS {
            assert!(contract.source().starts_with("urn:labweaver:"));
            assert!(contract.source().ends_with("-service"));
        }
    }

    #[test]
    fn build_failure_retry_requires_verified_cleanup() {
        let build_request_id = BuildRequestId::new();
        let command_sha256 = Sha256Digest::of_bytes(b"command");
        let unsafe_retry = AgentBuildFailed {
            build_request_id,
            command_sha256,
            diagnostic_code: "LW_AGENT_BUILD_CLEANUP_FAILED".to_owned(),
            retryable: true,
            cleanup_verified: false,
        };
        assert!(matches!(
            unsafe_retry.validate(),
            Err(EventError::PayloadIdentityMismatch)
        ));
        let cleaned_failure = AgentBuildFailed {
            build_request_id,
            command_sha256,
            diagnostic_code: "LW_AGENT_BUILD_TIMEOUT".to_owned(),
            retryable: true,
            cleanup_verified: true,
        };
        assert!(cleaned_failure.validate().is_ok());
    }
}
