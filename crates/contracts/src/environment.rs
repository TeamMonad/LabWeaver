//! Environment instance lifecycle and operation semantics.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::authoring::{EnvironmentClass, RuntimeKind};
use crate::{
    ActorId, CourseId, DiagnosticCode, EndpointId, EnvironmentId, LeaseId, OperationId, ProjectId,
    ReleaseId, Revision, StreamSequence, UtcTimestamp,
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

/// Authoritative fields required to create the first Environment aggregate.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentCreateSpec {
    pub course_id: CourseId,
    pub owner_actor_id: ActorId,
    pub display_label: String,
    pub class: EnvironmentClass,
    pub runtime_kind: RuntimeKind,
    pub release_id: ReleaseId,
    pub release_version: u64,
    pub provider_binding: String,
    pub lease_id: Option<LeaseId>,
    pub capacity_binding: Option<String>,
    pub eligibility_expires_at: UtcTimestamp,
}

/// Explicit immutable target selected for one reset operation.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum EnvironmentResetTarget {
    ExperimentBaseline {
        release_id: ReleaseId,
        release_version: u64,
    },
    WorkSnapshot {
        snapshot: crate::ArtifactRef,
        authorization_revision: Revision,
    },
    WorkConfiguration {
        configuration_revision: Revision,
        authorization_revision: Revision,
    },
}

/// Resource-authoritative Active Lease snapshot retained with the accepted operation.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentLeaseAuthorization {
    pub lease_id: LeaseId,
    pub lease_revision: Revision,
    pub environment_id: EnvironmentId,
    pub course_id: CourseId,
    pub owner_actor_id: ActorId,
    pub capacity_binding: String,
    pub active_from: UtcTimestamp,
    pub expires_at: UtcTimestamp,
}

/// Versioned Resource request for exact Lease scope and current state.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentLeaseVerificationRequest {
    pub version: u8,
    pub lease_id: LeaseId,
    pub environment_id: EnvironmentId,
    pub course_id: CourseId,
    pub owner_actor_id: ActorId,
    pub capacity_binding: String,
}

/// Closed Resource Lease states understood by the Environment verifier.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentLeaseState {
    Active,
    Expiring,
    Expired,
    Revoked,
}

/// Versioned Resource response. Only `Active` with an exact authorization is accepted.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentLeaseVerificationResponse {
    pub version: u8,
    pub state: EnvironmentLeaseState,
    pub authorization: Option<EnvironmentLeaseAuthorization>,
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
    pub reset_target: Option<EnvironmentResetTarget>,
}

/// Command-specific data carried inside the catalogued lifecycle CloudEvent.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentLifecycleCommandData {
    pub idempotency_key: String,
    pub command: EnvironmentLifecycleCommand,
    pub create: Option<EnvironmentCreateSpec>,
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

/// Public operation state used by REST snapshots and SSE projections.
///
/// The runtime aggregate retains its historical `OperationState`; timeout is exposed as a
/// distinct terminal fact instead of forcing clients to infer it from a generic failure code.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentOperationStatus {
    Accepted,
    Running,
    Cancelling,
    Succeeded,
    Failed,
    Cancelled,
    TimedOut,
}

/// Provider-safe progress phase. Provider names, node identities, and raw payloads stay private.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicEnvironmentOperationPhase {
    Validating,
    Building,
    Provisioning,
    Stopping,
    RevokingAccess,
    CleaningUp,
    Finalizing,
}

impl EnvironmentOperationStatus {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Cancelled | Self::TimedOut
        )
    }
}

/// Safe operation representation for Public REST and SSE consumers.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentOperationSnapshot {
    pub environment_id: EnvironmentId,
    pub operation_id: OperationId,
    pub kind: EnvironmentOperationKind,
    pub state: EnvironmentOperationStatus,
    pub accepted_revision: Revision,
    pub current_revision: Revision,
    pub accepted_at: UtcTimestamp,
    pub started_at: Option<UtcTimestamp>,
    pub updated_at: UtcTimestamp,
    pub terminal_at: Option<UtcTimestamp>,
    pub deadline_at: UtcTimestamp,
    pub timed_out_at: Option<UtcTimestamp>,
    pub cleanup_started_at: Option<UtcTimestamp>,
    pub cleanup_deadline_at: Option<UtcTimestamp>,
    pub provider_phase: Option<PublicEnvironmentOperationPhase>,
    pub attempt: u32,
    pub max_attempts: u32,
    pub retry_eligible: bool,
    pub cancel_eligible: bool,
    pub diagnostic_code: Option<DiagnosticCode>,
    pub request_id: String,
    pub trace_id: String,
    pub last_changed_stream_sequence: StreamSequence,
}

impl EnvironmentOperationSnapshot {
    pub fn validate(&self) -> Result<(), EnvironmentError> {
        let terminal = self.state.is_terminal();
        if self.attempt == 0
            || self.attempt > self.max_attempts
            || self.updated_at < self.accepted_at
            || self.deadline_at <= self.accepted_at
            || self
                .started_at
                .is_some_and(|value| value < self.accepted_at || value > self.updated_at)
            || terminal != self.terminal_at.is_some()
            || self
                .terminal_at
                .is_some_and(|value| value < self.accepted_at || value > self.updated_at)
            || (self.state == EnvironmentOperationStatus::TimedOut) != self.timed_out_at.is_some()
            || self.timed_out_at.is_some_and(|value| {
                value < self.accepted_at || value > self.updated_at || value < self.deadline_at
            })
            || (self.state != EnvironmentOperationStatus::TimedOut
                && self
                    .terminal_at
                    .is_some_and(|value| value > self.deadline_at))
            || self.cleanup_deadline_at.is_some() != self.cleanup_started_at.is_some()
            || self
                .cleanup_started_at
                .is_some_and(|value| value < self.accepted_at || value > self.updated_at)
            || self.cleanup_deadline_at.is_some_and(|deadline| {
                self.cleanup_started_at
                    .is_none_or(|started| deadline <= started)
            })
            || (terminal && self.cancel_eligible)
            || (self.retry_eligible
                && !matches!(
                    self.state,
                    EnvironmentOperationStatus::Failed | EnvironmentOperationStatus::TimedOut
                ))
            || self.request_id.trim().is_empty()
            || self.trace_id.trim().is_empty()
            || self.last_changed_stream_sequence.0 == 0
        {
            return Err(EnvironmentError::InvalidOperationSnapshot);
        }
        Ok(())
    }
}

/// Actor-safe relationship to the Environment owner.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentOwnerRelation {
    SelfOwned,
    Managed,
}

/// Actor-safe owner label. It deliberately carries no globally enumerable ActorId.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentOwnerSummary {
    pub relation: EnvironmentOwnerRelation,
    pub display_label: Option<String>,
}

/// Access readiness projected without endpoint routes, credentials, or policy material.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentAccessEligibilityState {
    Eligible,
    Ineligible,
    ActiveGrant,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentAccessEligibilitySummary {
    pub state: EnvironmentAccessEligibilityState,
    pub reason_code: Option<DiagnosticCode>,
    pub healthy_endpoint_count: u32,
    pub active_grant_count: u32,
}

/// Minimal Environment inventory item suitable for a GCP-style resource console.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentSummary {
    pub id: EnvironmentId,
    pub display_label: String,
    pub course_id: CourseId,
    pub project_id: Option<ProjectId>,
    pub owner: EnvironmentOwnerSummary,
    pub class: EnvironmentClass,
    pub runtime_kind: RuntimeKind,
    pub release_id: ReleaseId,
    pub release_version: u64,
    pub desired_state: DesiredEnvironmentState,
    pub observed_state: ObservedEnvironmentState,
    pub revision: Revision,
    pub eligibility_expires_at: UtcTimestamp,
    pub created_at: UtcTimestamp,
    pub updated_at: UtcTimestamp,
    pub last_changed_stream_sequence: StreamSequence,
    pub current_operation: Option<EnvironmentOperationSnapshot>,
    pub access: EnvironmentAccessEligibilitySummary,
}

impl EnvironmentSummary {
    pub fn validate(&self) -> Result<(), EnvironmentError> {
        if self.display_label.trim().is_empty()
            || self.display_label.chars().count() > 120
            || self.display_label.chars().any(char::is_control)
            || self.owner.display_label.as_ref().is_some_and(|value| {
                value.trim().is_empty()
                    || value.chars().count() > 120
                    || value.chars().any(char::is_control)
            })
            || self.release_version == 0
            || self.updated_at < self.created_at
            || self.last_changed_stream_sequence.0 == 0
        {
            return Err(EnvironmentError::InvalidInventorySummary);
        }
        if let Some(operation) = &self.current_operation {
            operation.validate()?;
            if operation.environment_id != self.id || operation.current_revision != self.revision {
                return Err(EnvironmentError::InvalidInventorySummary);
            }
        }
        Ok(())
    }
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
    pub provider_step: u32,
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
    pub retry_from_phase: Option<ObservedEnvironmentState>,
    pub reset_target: Option<EnvironmentResetTarget>,
    pub lease_authorization: Option<EnvironmentLeaseAuthorization>,
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
    pub display_label: String,
    pub course_id: crate::CourseId,
    pub owner_id: ActorId,
    pub class: EnvironmentClass,
    pub runtime_kind: RuntimeKind,
    pub release_id: ReleaseId,
    pub release_version: u64,
    pub lease_id: Option<LeaseId>,
    pub capacity_binding: Option<String>,
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
    pub failed_phase: Option<ObservedEnvironmentState>,
    pub cleanup_evidence: Option<crate::ArtifactRef>,
}

impl EnvironmentInstance {
    /// Validates aggregate invariants without consulting provider state.
    pub fn validate(&self) -> Result<(), EnvironmentError> {
        if self.display_label.trim().is_empty()
            || self.display_label.chars().count() > 120
            || self.display_label.chars().any(char::is_control)
            || self.release_version == 0
            || self.provider_binding.trim().is_empty()
            || self.generation == 0
            || self.observed_generation > self.generation
            || self.operation.attempt == 0
            || self.operation.provider_step == 0
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
            EnvironmentClass::Experiment
                if self.lease_id.is_some() || self.capacity_binding.is_some() =>
            {
                return Err(EnvironmentError::InvalidAggregate);
            }
            EnvironmentClass::Work
                if self.lease_id.is_none()
                    || self
                        .capacity_binding
                        .as_deref()
                        .is_none_or(|binding| !valid_binding(binding)) =>
            {
                return Err(EnvironmentError::LeaseRequired);
            }
            _ => {}
        }
        if (self.observed_state == ObservedEnvironmentState::Failed) != self.failed_phase.is_some()
        {
            return Err(EnvironmentError::FailedPhaseRequired);
        }
        let retry_operation = matches!(
            self.operation.kind,
            EnvironmentOperationKind::Retry | EnvironmentOperationKind::Recover
        );
        if retry_operation != self.operation.retry_from_phase.is_some() {
            return Err(EnvironmentError::FailedPhaseRequired);
        }
        if (self.operation.kind == EnvironmentOperationKind::Reset)
            != self.operation.reset_target.is_some()
        {
            return Err(EnvironmentError::ResetTargetRequired);
        }
        if let Some(target) = &self.operation.reset_target {
            validate_reset_target(self, target)?;
        }
        if let Some(authorization) = &self.operation.lease_authorization {
            if self.class != EnvironmentClass::Work
                || Some(authorization.lease_id) != self.lease_id
                || authorization.environment_id != self.id
                || authorization.course_id != self.course_id
                || authorization.owner_actor_id != self.owner_id
                || Some(authorization.capacity_binding.as_str()) != self.capacity_binding.as_deref()
                || authorization.active_from >= authorization.expires_at
            {
                return Err(EnvironmentError::LeaseAuthorizationInvalid);
            }
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
                State::Validating
                    | State::Building
                    | State::Provisioning
                    | State::Stopping
                    | State::Updating
                    | State::Expiring
                    | State::Deleting
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
    #[error("Failed state and retry/recover require a persisted failed phase")]
    FailedPhaseRequired,
    #[error("reset requires an explicit class-specific immutable target")]
    ResetTargetRequired,
    #[error("reset target does not match the Environment class or immutable binding")]
    ResetTargetInvalid,
    #[error("Work create/start/retry/recover/reset requires an Active Lease authorization")]
    LeaseAuthorizationRequired,
    #[error("Resource Lease authorization does not match the Environment scope")]
    LeaseAuthorizationInvalid,
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
    #[error("endpoint eligibility response is incomplete, stale, unhealthy, or scope-mismatched")]
    EndpointEligibilityInvalid,
    #[error("environment diagnostic code is not a stable LW_* identity")]
    InvalidDiagnosticCode,
    #[error("public environment operation snapshot is internally inconsistent")]
    InvalidOperationSnapshot,
    #[error("public environment inventory summary is internally inconsistent")]
    InvalidInventorySummary,
}

fn valid_binding(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
}

fn validate_reset_target(
    instance: &EnvironmentInstance,
    target: &EnvironmentResetTarget,
) -> Result<(), EnvironmentError> {
    let valid = match (instance.class, target) {
        (
            EnvironmentClass::Experiment,
            EnvironmentResetTarget::ExperimentBaseline {
                release_id,
                release_version,
            },
        ) => *release_id == instance.release_id && *release_version == instance.release_version,
        (EnvironmentClass::Work, EnvironmentResetTarget::WorkSnapshot { snapshot, .. }) => {
            !snapshot.store_binding.trim().is_empty()
                && !snapshot.object_version.trim().is_empty()
                && snapshot.size_bytes > 0
                && !snapshot.media_type.trim().is_empty()
        }
        (EnvironmentClass::Work, EnvironmentResetTarget::WorkConfiguration { .. }) => true,
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(EnvironmentError::ResetTargetInvalid)
    }
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

/// Fail-closed Access Service request for exact endpoint eligibility.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentEndpointEligibilityRequest {
    pub environment_id: EnvironmentId,
    pub course_id: CourseId,
    pub actor_id: ActorId,
    pub subject_kind: EnvironmentAccessSubjectKind,
    pub expected_revision: Revision,
    pub endpoint_ids: Vec<EndpointId>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentAccessSubjectKind {
    Owner,
    CourseTeacher,
}

/// Environment-authoritative endpoint facts safe for Access Service.
///
/// Host, port, credential, provider and network-policy data are deliberately absent.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentEndpointEligibility {
    pub environment_id: EnvironmentId,
    pub course_id: CourseId,
    pub owner_actor_id: ActorId,
    pub environment_revision: Revision,
    pub eligibility_expires_at: UtcTimestamp,
    pub endpoints: Vec<EnvironmentEndpoint>,
}

impl EnvironmentEndpointEligibility {
    pub fn validate_for(
        &self,
        request: &EnvironmentEndpointEligibilityRequest,
        now: UtcTimestamp,
    ) -> Result<(), EnvironmentError> {
        let requested = request
            .endpoint_ids
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        let returned = self
            .endpoints
            .iter()
            .map(|endpoint| endpoint.id)
            .collect::<BTreeSet<_>>();
        if request.endpoint_ids.is_empty()
            || requested.len() != request.endpoint_ids.len()
            || self.environment_id != request.environment_id
            || self.course_id != request.course_id
            || (request.subject_kind == EnvironmentAccessSubjectKind::Owner
                && self.owner_actor_id != request.actor_id)
            || self.environment_revision != request.expected_revision
            || self.eligibility_expires_at <= now
            || requested != returned
            || self
                .endpoints
                .iter()
                .any(|endpoint| endpoint.health != EndpointHealth::Healthy)
        {
            return Err(EnvironmentError::EndpointEligibilityInvalid);
        }
        Ok(())
    }
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
                        State::Validating
                            | State::Building
                            | State::Provisioning
                            | State::Stopping
                            | State::Updating
                            | State::Expiring
                            | State::Deleting
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
