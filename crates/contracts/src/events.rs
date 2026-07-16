//! Versioned NATS subjects and CloudEvents 1.0 wire contracts.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{
    AccessGrantId, ActorId, AgentRunId, BuildRequestId, CourseId, EnvironmentId, EventId,
    FrozenSubmissionId, ReleaseId, Revision, Sequence, Sha256Digest, UtcTimestamp,
};

pub const SPEC_VERSION: &str = "1.0";
pub const DATA_SCHEMA_BASE: &str = "https://schemas.labweaver.io/contracts/v1/events";

pub mod subjects {
    pub const AGENT_RUN_REQUESTED: &str = "labweaver.agent.run.requested.v1";
    pub const AGENT_RUN_COMPLETED: &str = "labweaver.agent.run.completed.v1";
    pub const AGENT_RUN_FAILED: &str = "labweaver.agent.run.failed.v1";
    pub const AGENT_BUILD_REQUESTED: &str = "labweaver.agent.build.requested.v1";
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
    pub const ACCESS_GRANT_REVOKED: &str = "labweaver.access.grant.revoked.v1";
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
        subject: subjects::ACCESS_GRANT_REVOKED,
        event_type: subjects::ACCESS_GRANT_REVOKED,
        schema_name: "access-grant-revoked",
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
            || !contract.event_type.ends_with(".v1")
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
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentBuildRequested {
    pub build_request_id: BuildRequestId,
    pub candidate_sha256: Sha256Digest,
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
pub struct SubmissionFrozen {
    pub frozen_submission_id: FrozenSubmissionId,
    pub environment_id: EnvironmentId,
    pub manifest_sha256: Sha256Digest,
    pub frozen_by: ActorId,
}
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReleasePublished {
    pub release_id: ReleaseId,
    pub version: u64,
    pub environment_spec_sha256: Sha256Digest,
    pub artifact_sha256: Sha256Digest,
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
    const PROTECTED: &[&str] = &[
        "secret",
        "token",
        "privateKey",
        "apiKey",
        "authorization",
        "password",
        "credential",
        "privateEndpoint",
        "targetHost",
        "targetPort",
        "submissionContent",
        "materialContent",
        "score",
        "pointsAwarded",
    ];
    match value {
        serde_json::Value::Object(object) => {
            for (key, nested) in object {
                if PROTECTED
                    .iter()
                    .any(|protected| key.eq_ignore_ascii_case(protected))
                {
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
    }

    #[test]
    fn registered_sources_are_owner_specific() {
        for contract in EVENT_CONTRACTS {
            assert!(contract.source().starts_with("urn:labweaver:"));
            assert!(contract.source().ends_with("-service"));
        }
    }
}
