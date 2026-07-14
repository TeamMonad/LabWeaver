//! Environment instance lifecycle and operation semantics.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::authoring::{EnvironmentClass, RuntimeKind};
use crate::{ActorId, EndpointId, EnvironmentId, OperationId, ReleaseId, Revision, UtcTimestamp};

/// Requested steady state.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DesiredEnvironmentState {
    Running,
    Stopped,
    Deleted,
}

/// Authoritative observed lifecycle state.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservedEnvironmentState {
    Requested,
    Validating,
    Building,
    Provisioning,
    Ready,
    Stopped,
    Updating,
    Expiring,
    Deleting,
    Deleted,
    Failed,
}

/// Explicit operation kind; restart and destructive reset are never aliases.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentOperationKind {
    Create,
    Start,
    Stop,
    Restart,
    Reset,
    Retry,
    Cancel,
    Recover,
    Expire,
    Delete,
    Freeze,
}

/// Persistent operation state.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    Accepted,
    Running,
    Cancelling,
    Succeeded,
    Failed,
    Cancelled,
}

/// Idempotent accepted environment operation.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentOperation {
    pub id: OperationId,
    pub kind: EnvironmentOperationKind,
    pub state: OperationState,
    pub accepted_revision: Revision,
    pub attempt: u32,
    pub actor_id: ActorId,
    pub accepted_at: UtcTimestamp,
    pub deadline_at: UtcTimestamp,
    pub diagnostic_code: Option<String>,
    pub preserve_mutable_disk: bool,
    pub access_revocation_revision: Option<Revision>,
}

/// Sanitized Environment-owned endpoint metadata.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentEndpoint {
    pub id: EndpointId,
    pub protocol: EndpointProtocol,
    pub revision: Revision,
    pub health: EndpointHealth,
    pub observed_at: UtcTimestamp,
}

/// Supported controlled protocols.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointProtocol {
    Http,
    Https,
    Ssh,
}

/// Endpoint health gates new AccessGrants.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointHealth {
    Pending,
    Healthy,
    Unhealthy,
    Removed,
}

/// PostgreSQL-authoritative environment view.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentInstance {
    pub id: EnvironmentId,
    pub course_id: crate::CourseId,
    pub owner_id: ActorId,
    pub class: EnvironmentClass,
    pub runtime_kind: RuntimeKind,
    pub release_id: ReleaseId,
    pub release_version: u64,
    pub provider_binding: String,
    pub desired_state: DesiredEnvironmentState,
    pub observed_state: ObservedEnvironmentState,
    pub revision: Revision,
    pub generation: u64,
    pub observed_generation: u64,
    pub operation: EnvironmentOperation,
    pub endpoints: Vec<EnvironmentEndpoint>,
    pub last_diagnostic_code: Option<String>,
    pub cleanup_evidence: Option<crate::ArtifactRef>,
}

impl EnvironmentInstance {
    /// Validates aggregate invariants without consulting provider state.
    pub fn validate(&self) -> Result<(), EnvironmentError> {
        if self.release_version == 0
            || self.provider_binding.trim().is_empty()
            || self.generation == 0
            || self.observed_generation > self.generation
            || self.operation.attempt == 0
            || self.operation.deadline_at <= self.operation.accepted_at
        {
            return Err(EnvironmentError::InvalidAggregate);
        }
        match self.operation.kind {
            EnvironmentOperationKind::Restart if !self.operation.preserve_mutable_disk => {
                return Err(EnvironmentError::InvalidAggregate);
            }
            EnvironmentOperationKind::Reset
                if self.operation.preserve_mutable_disk
                    || self.operation.access_revocation_revision.is_none() =>
            {
                return Err(EnvironmentError::GrantRevocationRequired);
            }
            EnvironmentOperationKind::Delete
                if self.operation.access_revocation_revision.is_none() =>
            {
                return Err(EnvironmentError::GrantRevocationRequired);
            }
            _ => {}
        }
        if self.observed_state == ObservedEnvironmentState::Ready
            && (self.observed_generation != self.generation
                || self.endpoints.is_empty()
                || self
                    .endpoints
                    .iter()
                    .any(|endpoint| endpoint.health != EndpointHealth::Healthy))
        {
            return Err(EnvironmentError::ReadyWithoutHealthyEndpoint);
        }
        if self.observed_state == ObservedEnvironmentState::Deleted
            && self.cleanup_evidence.is_none()
        {
            return Err(EnvironmentError::CleanupEvidenceRequired);
        }
        Ok(())
    }

    /// Checks whether an observed state transition is part of the frozen lifecycle.
    pub fn ensure_transition(
        from: ObservedEnvironmentState,
        to: ObservedEnvironmentState,
    ) -> Result<(), EnvironmentError> {
        use ObservedEnvironmentState as State;
        let allowed = matches!(
            (from, to),
            (
                State::Requested,
                State::Validating | State::Failed | State::Deleting
            ) | (
                State::Validating,
                State::Building | State::Failed | State::Deleting
            ) | (
                State::Building,
                State::Provisioning | State::Failed | State::Deleting
            ) | (
                State::Provisioning,
                State::Ready | State::Stopped | State::Failed | State::Deleting
            ) | (
                State::Ready,
                State::Provisioning
                    | State::Stopped
                    | State::Updating
                    | State::Expiring
                    | State::Deleting
                    | State::Failed
            ) | (
                State::Stopped,
                State::Provisioning | State::Expiring | State::Deleting | State::Failed
            ) | (
                State::Updating,
                State::Ready | State::Failed | State::Deleting
            ) | (
                State::Failed,
                State::Validating | State::Provisioning | State::Expiring | State::Deleting
            ) | (
                State::Expiring,
                State::Stopped | State::Deleting | State::Failed
            ) | (State::Deleting, State::Deleted | State::Failed)
        );
        if allowed {
            Ok(())
        } else {
            Err(EnvironmentError::InvalidTransition { from, to })
        }
    }

    /// Validates an operation against the current observed state.
    pub fn ensure_operation_allowed(
        state: ObservedEnvironmentState,
        operation: EnvironmentOperationKind,
    ) -> Result<(), EnvironmentError> {
        use EnvironmentOperationKind as Operation;
        use ObservedEnvironmentState as State;
        let allowed = match operation {
            Operation::Create => state == State::Requested,
            Operation::Start => state == State::Stopped,
            Operation::Stop | Operation::Freeze => state == State::Ready,
            Operation::Restart | Operation::Reset => {
                matches!(state, State::Ready | State::Stopped | State::Failed)
            }
            Operation::Retry | Operation::Recover => state == State::Failed,
            Operation::Cancel => !matches!(state, State::Deleted | State::Deleting),
            Operation::Expire => matches!(state, State::Ready | State::Stopped | State::Failed),
            Operation::Delete => state != State::Deleted,
        };
        if allowed {
            Ok(())
        } else {
            Err(EnvironmentError::OperationNotAllowed { state, operation })
        }
    }
}

/// Environment lifecycle contract failure.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum EnvironmentError {
    #[error("EnvironmentInstance aggregate is internally inconsistent")]
    InvalidAggregate,
    #[error("Ready requires current generation and healthy endpoint identity")]
    ReadyWithoutHealthyEndpoint,
    #[error("Deleted requires immutable cleanup evidence")]
    CleanupEvidenceRequired,
    #[error("reset and delete require recorded AccessGrant revocation before provider mutation")]
    GrantRevocationRequired,
    #[error("illegal environment transition: {from:?} -> {to:?}")]
    InvalidTransition {
        from: ObservedEnvironmentState,
        to: ObservedEnvironmentState,
    },
    #[error("operation {operation:?} is not allowed from {state:?}")]
    OperationNotAllowed {
        state: ObservedEnvironmentState,
        operation: EnvironmentOperationKind,
    },
}

#[cfg(test)]
mod tests {
    use super::{EnvironmentInstance, EnvironmentOperationKind, ObservedEnvironmentState};

    #[test]
    fn deleted_is_terminal() {
        for state in [
            ObservedEnvironmentState::Requested,
            ObservedEnvironmentState::Ready,
            ObservedEnvironmentState::Failed,
        ] {
            assert!(
                EnvironmentInstance::ensure_transition(ObservedEnvironmentState::Deleted, state)
                    .is_err()
            );
        }
    }

    #[test]
    fn restart_and_reset_are_explicit_and_bounded() {
        for operation in [
            EnvironmentOperationKind::Restart,
            EnvironmentOperationKind::Reset,
        ] {
            assert!(
                EnvironmentInstance::ensure_operation_allowed(
                    ObservedEnvironmentState::Ready,
                    operation
                )
                .is_ok()
            );
            assert!(
                EnvironmentInstance::ensure_operation_allowed(
                    ObservedEnvironmentState::Provisioning,
                    operation
                )
                .is_err()
            );
        }
    }
}
