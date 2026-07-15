//! Environment instance lifecycle and operation semantics.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::authoring::{EnvironmentClass, RuntimeKind};
use crate::{
    ActorId, CourseId, EndpointId, EnvironmentId, LeaseId, OperationId, ReleaseId, Revision,
    UtcTimestamp,
};

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
    Stopping,
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
    Cleanup,
    Freeze,
}

/// Revision-checked lifecycle intent consumed by the Environment state owner.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentLifecycleCommand {
    pub environment_id: EnvironmentId,
    pub kind: EnvironmentOperationKind,
    pub expected_revision: Revision,
    pub actor_id: ActorId,
    pub trace_id: String,
    pub accepted_at: UtcTimestamp,
    pub deadline_at: UtcTimestamp,
    pub access_revocation_revision: Option<Revision>,
    pub preserve_mutable_disk: bool,
    pub max_attempts: u32,
}

/// Command-specific data carried inside the catalogued lifecycle CloudEvent.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentLifecycleCommandData {
    pub idempotency_key: String,
    pub command: EnvironmentLifecycleCommand,
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
    pub max_attempts: u32,
    pub next_attempt_at: UtcTimestamp,
    pub actor_id: ActorId,
    pub trace_id: String,
    pub accepted_at: UtcTimestamp,
    pub deadline_at: UtcTimestamp,
    pub cleanup_started_at: Option<UtcTimestamp>,
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
    pub lease_id: Option<LeaseId>,
    pub provider_binding: String,
    pub desired_state: DesiredEnvironmentState,
    pub observed_state: ObservedEnvironmentState,
    pub revision: Revision,
    pub generation: u64,
    pub observed_generation: u64,
    pub operation: EnvironmentOperation,
    pub eligibility_expires_at: UtcTimestamp,
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
            || self.operation.max_attempts < self.operation.attempt
            || self.operation.deadline_at <= self.operation.accepted_at
            || self.operation.next_attempt_at < self.operation.accepted_at
            || self.operation.next_attempt_at > self.operation.deadline_at
            || self.operation.cleanup_started_at.is_some_and(|started_at| {
                self.desired_state != DesiredEnvironmentState::Deleted
                    || started_at < self.operation.accepted_at
                    || started_at > self.operation.deadline_at
            })
            || self.operation.trace_id.is_empty()
            || self.operation.trace_id.len() > 128
            || !self
                .operation
                .trace_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
        {
            return Err(EnvironmentError::InvalidAggregate);
        }
        for code in [
            self.last_diagnostic_code.as_deref(),
            self.operation.diagnostic_code.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            crate::DiagnosticCode::parse(code)
                .map_err(|_| EnvironmentError::InvalidDiagnosticCode)?;
        }
        match self.class {
            EnvironmentClass::Experiment if self.lease_id.is_some() => {
                return Err(EnvironmentError::InvalidAggregate);
            }
            EnvironmentClass::Work if self.lease_id.is_none() => {
                return Err(EnvironmentError::LeaseRequired);
            }
            _ => {}
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
            EnvironmentOperationKind::Stop
            | EnvironmentOperationKind::Cancel
            | EnvironmentOperationKind::Expire
            | EnvironmentOperationKind::Delete
            | EnvironmentOperationKind::Cleanup
                if self.operation.access_revocation_revision.is_none() =>
            {
                return Err(EnvironmentError::GrantRevocationRequired);
            }
            _ => {}
        }
        if self.observed_state == ObservedEnvironmentState::Ready
            && (self.observed_generation != self.generation
                || self.endpoints.is_empty()
                || self.endpoints.iter().any(|endpoint| {
                    endpoint.health != EndpointHealth::Healthy || endpoint.revision != self.revision
                }))
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
                    | State::Stopping
                    | State::Updating
                    | State::Expiring
                    | State::Deleting
                    | State::Failed
            ) | (
                State::Stopping | State::Expiring,
                State::Stopped | State::Failed | State::Deleting
            ) | (
                State::Stopped,
                State::Provisioning | State::Expiring | State::Deleting | State::Failed
            ) | (
                State::Updating,
                State::Ready | State::Failed | State::Deleting
            ) | (
                State::Failed,
                State::Validating | State::Provisioning | State::Expiring | State::Deleting
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
            Operation::Cleanup => {
                matches!(state, State::Expiring | State::Deleting | State::Failed)
            }
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
    #[error(
        "stop, reset, expiry, cancellation and cleanup require recorded AccessGrant revocation before provider mutation"
    )]
    GrantRevocationRequired,
    #[error("Work environments require a Resource-owned Lease reference")]
    LeaseRequired,
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
    #[error("owner resolver configuration is unsafe or incomplete")]
    InvalidResolverConfiguration,
    #[error("environment diagnostic code is not a stable LW_* identity")]
    InvalidDiagnosticCode,
}

/// Fail-closed internal request used by Access Service to verify Environment ownership.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentOwnerResolutionRequest {
    pub environment_id: EnvironmentId,
    pub course_id: CourseId,
    pub owner_actor_id: ActorId,
    pub expected_revision: Revision,
}

/// Minimal Environment-authoritative ownership result; it never contains endpoints or credentials.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentOwnerResolution {
    pub environment_id: EnvironmentId,
    pub course_id: CourseId,
    pub owner_actor_id: ActorId,
    pub environment_revision: Revision,
    pub eligibility_expires_at: UtcTimestamp,
}

/// Deployment-supplied client settings for the controlled mTLS owner-resolver call.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentOwnerResolverClientConfig {
    pub resolver_uri: String,
    pub ca_certificate_locator: String,
    pub client_certificate_locator: String,
    pub client_private_key_locator: String,
    pub allowed_server_sans: Vec<String>,
    pub timeout_milliseconds: u64,
    pub max_retries: u8,
}

impl EnvironmentOwnerResolverClientConfig {
    /// Rejects implicit endpoints, inline key material, unbounded calls, and empty SAN policy.
    pub fn validate(&self) -> Result<(), EnvironmentError> {
        let locators = [
            self.ca_certificate_locator.as_str(),
            self.client_certificate_locator.as_str(),
            self.client_private_key_locator.as_str(),
        ];
        let resolver_authority = self.resolver_uri.strip_prefix("https://");
        if resolver_authority.is_none_or(|authority| {
            authority.is_empty()
                || authority.contains('@')
                || authority.contains('/')
                || authority.contains('?')
                || authority.contains('#')
                || authority.chars().any(char::is_whitespace)
        }) || locators.iter().any(|locator| {
            !locator.starts_with("secret://")
                || locator.len() <= "secret://".len()
                || locator.contains("-----BEGIN")
                || locator.contains('\n')
        }) || self.allowed_server_sans.is_empty()
            || self.allowed_server_sans.iter().any(|san| {
                san.trim().is_empty()
                    || san.contains('*')
                    || san.contains('@')
                    || san.contains('/')
                    || san.chars().any(char::is_whitespace)
            })
            || !(1..=30_000).contains(&self.timeout_milliseconds)
            || self.max_retries > 3
        {
            return Err(EnvironmentError::InvalidResolverConfiguration);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        EnvironmentInstance, EnvironmentOperationKind, EnvironmentOwnerResolverClientConfig,
        ObservedEnvironmentState,
    };

    const STATES: [ObservedEnvironmentState; 12] = [
        ObservedEnvironmentState::Requested,
        ObservedEnvironmentState::Validating,
        ObservedEnvironmentState::Building,
        ObservedEnvironmentState::Provisioning,
        ObservedEnvironmentState::Ready,
        ObservedEnvironmentState::Stopping,
        ObservedEnvironmentState::Stopped,
        ObservedEnvironmentState::Updating,
        ObservedEnvironmentState::Expiring,
        ObservedEnvironmentState::Deleting,
        ObservedEnvironmentState::Deleted,
        ObservedEnvironmentState::Failed,
    ];
    const OPERATIONS: [EnvironmentOperationKind; 12] = [
        EnvironmentOperationKind::Create,
        EnvironmentOperationKind::Start,
        EnvironmentOperationKind::Stop,
        EnvironmentOperationKind::Restart,
        EnvironmentOperationKind::Reset,
        EnvironmentOperationKind::Retry,
        EnvironmentOperationKind::Cancel,
        EnvironmentOperationKind::Recover,
        EnvironmentOperationKind::Expire,
        EnvironmentOperationKind::Delete,
        EnvironmentOperationKind::Cleanup,
        EnvironmentOperationKind::Freeze,
    ];

    #[test]
    fn transition_matrix_is_exhaustive_for_every_state_pair() {
        use ObservedEnvironmentState as State;
        for from in STATES {
            for to in STATES {
                let expected = matches!(
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
                            | State::Stopping
                            | State::Updating
                            | State::Expiring
                            | State::Deleting
                            | State::Failed
                    ) | (
                        State::Stopping | State::Expiring,
                        State::Stopped | State::Failed | State::Deleting
                    ) | (
                        State::Stopped,
                        State::Provisioning | State::Expiring | State::Deleting | State::Failed
                    ) | (
                        State::Updating,
                        State::Ready | State::Failed | State::Deleting
                    ) | (
                        State::Failed,
                        State::Validating | State::Provisioning | State::Expiring | State::Deleting
                    ) | (State::Deleting, State::Deleted | State::Failed)
                );
                assert_eq!(
                    EnvironmentInstance::ensure_transition(from, to).is_ok(),
                    expected,
                    "unexpected transition result for {from:?} -> {to:?}"
                );
            }
        }
    }

    #[test]
    fn operation_matrix_is_exhaustive_for_every_state_and_operation() {
        use EnvironmentOperationKind as Operation;
        use ObservedEnvironmentState as State;
        for state in STATES {
            for operation in OPERATIONS {
                let expected = match operation {
                    Operation::Create => state == State::Requested,
                    Operation::Start => state == State::Stopped,
                    Operation::Stop | Operation::Freeze => state == State::Ready,
                    Operation::Restart | Operation::Reset => {
                        matches!(state, State::Ready | State::Stopped | State::Failed)
                    }
                    Operation::Retry | Operation::Recover => state == State::Failed,
                    Operation::Cancel => !matches!(state, State::Deleted | State::Deleting),
                    Operation::Expire => {
                        matches!(state, State::Ready | State::Stopped | State::Failed)
                    }
                    Operation::Delete => state != State::Deleted,
                    Operation::Cleanup => {
                        matches!(state, State::Expiring | State::Deleting | State::Failed)
                    }
                };
                assert_eq!(
                    EnvironmentInstance::ensure_operation_allowed(state, operation).is_ok(),
                    expected,
                    "unexpected operation result for {operation:?} from {state:?}"
                );
            }
        }
    }

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

    #[test]
    fn resolver_client_requires_https_locators_sans_and_bounded_retry() {
        let mut config = EnvironmentOwnerResolverClientConfig {
            resolver_uri: "https://environment-service.internal".to_owned(),
            ca_certificate_locator: "secret://environment-resolver/ca".to_owned(),
            client_certificate_locator: "secret://access-service/tls-cert".to_owned(),
            client_private_key_locator: "secret://access-service/tls-key".to_owned(),
            allowed_server_sans: vec!["environment-service.internal".to_owned()],
            timeout_milliseconds: 2_000,
            max_retries: 2,
        };
        assert!(config.validate().is_ok());
        config.client_private_key_locator = "-----BEGIN PRIVATE KEY-----".to_owned();
        assert!(config.validate().is_err());
        config.client_private_key_locator = "secret://access-service/tls-key".to_owned();
        config.resolver_uri = "https://user@environment-service.internal".to_owned();
        assert!(config.validate().is_err());
        config.resolver_uri = "https://environment-service.internal".to_owned();
        config.allowed_server_sans = vec!["*.internal".to_owned()];
        assert!(config.validate().is_err());
    }
}
