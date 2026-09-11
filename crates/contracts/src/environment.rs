//! Environment instance lifecycle and operation semantics.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::access::{ConsoleKind, ConsoleLeaseFence, validate_ssh_public_key};
use crate::authoring::{EnvironmentClass, RuntimeKind, TerminalSpec};
use crate::resource::{GpuAllocation, WorkloadResources};
use crate::{
    ActorId, AgentRunId, CapacityClaimId, CourseId, DiagnosticCode, EndpointId, EnvironmentId,
    EvaluationRunId, EvaluationStepRunId, LeaseId, OperationId, ProjectId, ReleaseId,
    ResourceRequestId, Revision, StreamSequence, UtcTimestamp,
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
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
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

/// The consumer and immutable purpose of one short-lived VM execution binding.
///
/// Evaluation probes and private Work configuration use the same Environment-owned
/// issuer, but they are separate purposes so an issued credential cannot be replayed
/// across service boundaries.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum EnvironmentExecutionPurpose {
    /// Run one approved evaluation probe attempt.
    EvaluationProbe {
        run_id: EvaluationRunId,
        step_run_id: EvaluationStepRunId,
        attempt: u32,
    },
    /// Configure one private Work environment for an Agent run.
    WorkConfiguration {
        agent_run_id: AgentRunId,
        run_revision: Revision,
    },
    /// Resume one persisted VM Work execution after an Agent or Environment restart.
    WorkConfigurationRecovery {
        agent_run_id: AgentRunId,
        run_revision: Revision,
        #[schemars(with = "String")]
        execution_id: uuid::Uuid,
        plan_id: crate::WorkConfigurationPlanId,
        plan_revision: Revision,
        /// Persistent VM target UID from the Agent execution intent.
        source_identity: String,
    },
}

impl EnvironmentExecutionPurpose {
    fn validate(&self) -> Result<(), EnvironmentError> {
        if matches!(self, Self::EvaluationProbe { attempt: 0, .. })
            || matches!(self, Self::WorkConfiguration { run_revision, .. } if run_revision.get() == 0)
            || matches!(
                self,
                Self::WorkConfigurationRecovery {
                    run_revision,
                    plan_revision,
                    source_identity,
                    ..
                } if run_revision.get() == 0
                    || plan_revision.get() == 0
                    || !valid_token(source_identity, 256)
            )
        {
            return Err(EnvironmentError::ExecutionBindingInvalid);
        }
        Ok(())
    }
}

/// Evaluation or Agent request for one fresh VM execution credential.
///
/// The public key is generated for this request and is never persisted as a
/// reusable actor key. `runtime_kind` is repeated in the request to make a
/// container/VM mix-up fail before credential issuance.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentExecutionBindingRequest {
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub actor_id: ActorId,
    pub expected_revision: Revision,
    pub runtime_kind: RuntimeKind,
    pub purpose: EnvironmentExecutionPurpose,
    pub public_key_openssh: String,
}

impl EnvironmentExecutionBindingRequest {
    /// Validates the request before the Environment resolves any VM identity.
    pub fn validate(&self) -> Result<(), EnvironmentError> {
        if self.runtime_kind != RuntimeKind::VirtualMachine
            || self.public_key_openssh.trim() != self.public_key_openssh
            || validate_ssh_public_key(&self.public_key_openssh).is_err()
        {
            return Err(EnvironmentError::ExecutionBindingInvalid);
        }
        self.purpose.validate()
    }
}

/// Exact VM target selected by Environment for one execution binding.
///
/// The host, port, account, workspace root, host-key fingerprint and opaque
/// source identity are all resolved from the current Environment instance. A
/// consumer must reject a target whose source identity changes while it is
/// executing.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum EnvironmentExecutionSourceBinding {
    VirtualMachine {
        namespace: String,
        host: String,
        port: u16,
        username: String,
        workspace_root: String,
        expected_host_key_sha256: String,
        source_identity: String,
        execution_certificate_openssh: String,
        expires_at: UtcTimestamp,
    },
}

impl EnvironmentExecutionSourceBinding {
    fn validate(&self) -> Result<(), EnvironmentError> {
        let Self::VirtualMachine {
            namespace,
            host,
            port,
            username,
            workspace_root,
            expected_host_key_sha256,
            source_identity,
            execution_certificate_openssh,
            ..
        } = self;
        if valid_token(namespace, 63)
            && valid_private_ip(host)
            && *port == 22
            && valid_vm_username(username)
            && valid_workspace_root(workspace_root)
            && valid_token(expected_host_key_sha256, 256)
            && valid_token(source_identity, 256)
            && !execution_certificate_openssh.is_empty()
            && execution_certificate_openssh.len() <= 16_384
            && !execution_certificate_openssh.contains('\n')
            && !execution_certificate_openssh.contains('\r')
        {
            Ok(())
        } else {
            Err(EnvironmentError::ExecutionBindingInvalid)
        }
    }
}

/// Environment-owned response containing one frozen identity and a fresh VM
/// execution credential. The certificate has no collector/SFTP semantics.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentExecutionBinding {
    pub environment: crate::submission::FrozenEnvironmentIdentity,
    pub source: EnvironmentExecutionSourceBinding,
}

impl EnvironmentExecutionBinding {
    /// Validates that the response matches the request and is still usable.
    pub fn validate_for(
        &self,
        environment_id: EnvironmentId,
        request: &EnvironmentExecutionBindingRequest,
        now: UtcTimestamp,
    ) -> Result<(), EnvironmentError> {
        request.validate()?;
        if self.environment.environment_id != environment_id
            || self.environment.environment_revision != request.expected_revision
            || self.environment.runtime_kind != request.runtime_kind
        {
            return Err(EnvironmentError::ExecutionBindingInvalid);
        }
        self.source.validate()?;
        if let (
            EnvironmentExecutionPurpose::WorkConfigurationRecovery {
                source_identity: expected,
                ..
            },
            EnvironmentExecutionSourceBinding::VirtualMachine {
                source_identity: actual,
                ..
            },
        ) = (&request.purpose, &self.source)
            && expected != actual
        {
            return Err(EnvironmentError::ExecutionBindingInvalid);
        }
        let expires_at = match &self.source {
            EnvironmentExecutionSourceBinding::VirtualMachine { expires_at, .. } => *expires_at,
        };
        if expires_at <= now {
            return Err(EnvironmentError::ExecutionBindingInvalid);
        }
        Ok(())
    }
}

/// Resource-authoritative Active Lease snapshot retained with the accepted operation.
/// Private single-university deployment simplifies this to a TTL/PVC binding:
/// `capacity_binding` maps directly to the Work PVC name and `expires_at` is
/// the authoritative TTL; no separate capacity-shell hash is required.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentLeaseAuthorization {
    /// The Resource request that owns this lease. Environment meters use this
    /// identity when delivering usage back to Resource.
    pub resource_request_id: ResourceRequestId,
    pub lease_id: LeaseId,
    pub lease_revision: Revision,
    pub environment_id: EnvironmentId,
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub owner_actor_id: ActorId,
    pub capacity_binding: String,
    /// The exact Resource-approved limits Environment must apply to its workload.
    pub approved_resources: WorkloadResources,
    /// Catalog resolution captured by Resource when a GPU was approved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_allocation: Option<GpuAllocation>,
    pub active_from: UtcTimestamp,
    pub expires_at: UtcTimestamp,
}

impl EnvironmentLeaseAuthorization {
    /// Validates the complete Resource-approved allocation fence consumed by Environment.
    pub fn validate(&self) -> Result<(), EnvironmentError> {
        if self.capacity_binding.trim().is_empty() || self.active_from >= self.expires_at {
            return Err(EnvironmentError::InvalidResourceHandoff);
        }
        self.approved_resources
            .validate()
            .map_err(|_| EnvironmentError::InvalidResourceHandoff)?;
        validate_gpu_allocation(&self.approved_resources, self.gpu_allocation.as_ref())
    }
}

/// Versioned Resource request for exact Lease scope and current state.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentLeaseVerificationRequest {
    pub version: u8,
    pub lease_id: LeaseId,
    pub environment_id: EnvironmentId,
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
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

/// Resource-to-Environment command to create one Work aggregate from an already-approved
/// Lease and capacity shell. Environment resolves the release projection locally.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceWorkHandoff {
    pub version: u8,
    pub request_id: ResourceRequestId,
    pub request_revision: Revision,
    pub lease_id: LeaseId,
    pub lease_revision: Revision,
    pub claim_id: CapacityClaimId,
    pub claim_revision: Revision,
    pub environment_id: EnvironmentId,
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub owner_actor_id: ActorId,
    pub display_label: String,
    pub release_id: ReleaseId,
    pub release_version: u64,
    pub provider_binding: String,
    pub capacity_binding: String,
    /// Exact approved limits and catalog resolution consumed by Environment.
    pub approved_resources: WorkloadResources,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_allocation: Option<GpuAllocation>,
    pub trace_id: String,
}

impl ResourceWorkHandoff {
    pub fn validate(&self) -> Result<(), EnvironmentError> {
        if self.version != 1
            || self.request_revision.get() == 0
            || self.lease_revision.get() == 0
            || self.claim_revision.get() == 0
            || self.release_version == 0
            || self.display_label.trim().is_empty()
            || self.display_label.chars().count() > 120
            || self.provider_binding.is_empty()
            || self.provider_binding.len() > 120
            || self.capacity_binding.is_empty()
            || self.capacity_binding.len() > 120
            || self.trace_id.is_empty()
            || self.trace_id.len() > 128
            || self.trace_id.chars().any(char::is_control)
        {
            return Err(EnvironmentError::InvalidResourceHandoff);
        }
        self.approved_resources
            .validate()
            .map_err(|_| EnvironmentError::InvalidResourceHandoff)?;
        validate_gpu_allocation(&self.approved_resources, self.gpu_allocation.as_ref())?;
        Ok(())
    }
}

fn validate_gpu_allocation(
    resources: &WorkloadResources,
    allocation: Option<&GpuAllocation>,
) -> Result<(), EnvironmentError> {
    match (&resources.gpu, allocation) {
        (None, None) => Ok(()),
        (Some(request), Some(allocation))
            if request.class == allocation.class && request.count == allocation.count =>
        {
            allocation
                .validate()
                .map_err(|_| EnvironmentError::InvalidResourceHandoff)
        }
        _ => Err(EnvironmentError::InvalidResourceHandoff),
    }
}

/// Resource-authoritative Lease update for an existing Work aggregate.
///
/// Environment accepts this only from the authenticated Resource service after
/// independently resolving the exact active Lease. It extends eligibility but
/// cannot change the immutable release, Provider, owner, course, or capacity
/// binding selected by the original handoff.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceWorkLeaseUpdate {
    pub version: u8,
    pub lease_id: LeaseId,
    pub lease_revision: Revision,
    pub environment_id: EnvironmentId,
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub owner_actor_id: ActorId,
    pub capacity_binding: String,
    pub expires_at: UtcTimestamp,
    pub trace_id: String,
}

impl ResourceWorkLeaseUpdate {
    pub fn validate(&self) -> Result<(), EnvironmentError> {
        if self.version != 1
            || self.lease_revision.get() == 0
            || self.capacity_binding.is_empty()
            || self.capacity_binding.len() > 120
            || self.trace_id.is_empty()
            || self.trace_id.len() > 128
            || self.trace_id.chars().any(char::is_control)
        {
            return Err(EnvironmentError::InvalidResourceHandoff);
        }
        Ok(())
    }
}

/// Resource-authoritative request to revoke access and delete one Work
/// environment before releasing its exact capacity claim.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceWorkCleanup {
    pub version: u8,
    pub lease_id: LeaseId,
    pub lease_revision: Revision,
    pub environment_id: EnvironmentId,
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub owner_actor_id: ActorId,
    pub capacity_binding: String,
    pub reason_code: String,
    pub trace_id: String,
}

impl ResourceWorkCleanup {
    pub fn validate(&self) -> Result<(), EnvironmentError> {
        if self.version != 1
            || self.lease_revision.get() == 0
            || self.capacity_binding.is_empty()
            || self.capacity_binding.len() > 120
            || crate::DiagnosticCode::parse(&self.reason_code).is_err()
            || self.trace_id.is_empty()
            || self.trace_id.len() > 128
            || self.trace_id.chars().any(char::is_control)
        {
            return Err(EnvironmentError::InvalidResourceHandoff);
        }
        Ok(())
    }
}

/// Minimal Resource-visible cleanup readback. Provider handles, endpoints and
/// user content never cross this boundary.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceWorkCleanupStatus {
    pub version: u8,
    pub environment_id: EnvironmentId,
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub owner_actor_id: ActorId,
    pub lease_id: LeaseId,
    pub lease_revision: Revision,
    pub capacity_binding: String,
    pub revision: Revision,
    pub observed_state: ObservedEnvironmentState,
    pub cleanup_complete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic_code: Option<String>,
}

impl ResourceWorkCleanupStatus {
    /// Validates the identity fence returned by Environment before Resource releases a claim.
    pub fn validate(&self) -> Result<(), EnvironmentError> {
        if self.version != 1
            || self.lease_revision.get() == 0
            || self.revision.get() == 0
            || self.capacity_binding.trim().is_empty()
            || self.capacity_binding.len() > 120
        {
            return Err(EnvironmentError::InvalidResourceHandoff);
        }
        Ok(())
    }
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

impl OperationState {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

/// Safe operation representation for Public REST and SSE consumers.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentOperationSnapshot {
    pub environment_id: EnvironmentId,
    pub operation_id: OperationId,
    pub kind: EnvironmentOperationKind,
    pub state: OperationState,
    pub accepted_revision: Revision,
    pub accepted_at: UtcTimestamp,
    pub deadline_at: UtcTimestamp,
    pub cleanup_started_at: Option<UtcTimestamp>,
    pub terminal_at: Option<UtcTimestamp>,
    pub attempt: u32,
    pub max_attempts: u32,
    pub retry_eligible: bool,
    pub cancel_eligible: bool,
    pub diagnostic_code: Option<DiagnosticCode>,
    pub trace_id: String,
}

impl EnvironmentOperationSnapshot {
    pub fn validate(&self) -> Result<(), EnvironmentError> {
        let terminal = self.state.is_terminal();
        if self.attempt == 0
            || self.attempt > self.max_attempts
            || self.deadline_at <= self.accepted_at
            || terminal != self.terminal_at.is_some()
            || self
                .terminal_at
                .is_some_and(|value| value < self.accepted_at)
            || self.cleanup_started_at.is_some_and(|value| {
                value < self.accepted_at
                    || self
                        .terminal_at
                        .is_some_and(|terminal_at| value > terminal_at)
            })
            || (terminal && self.cancel_eligible)
            || (self.retry_eligible
                && (!matches!(self.state, OperationState::Failed)
                    || self.attempt >= self.max_attempts))
            || self.trace_id.trim().is_empty()
            || self.trace_id.len() > 128
            || !self
                .trace_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
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
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
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
            if operation.environment_id != self.id || operation.accepted_revision > self.revision {
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
    pub project_id: ProjectId,
    pub course_id: Option<crate::CourseId>,
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
        if let Some(authorization) = &self.operation.lease_authorization
            && (self.class != EnvironmentClass::Work
                || Some(authorization.lease_id) != self.lease_id
                || authorization.environment_id != self.id
                || authorization.project_id != self.project_id
                || authorization.course_id != self.course_id
                || authorization.owner_actor_id != self.owner_id
                || Some(authorization.capacity_binding.as_str())
                    != self.capacity_binding.as_deref()
                || authorization.active_from >= authorization.expires_at)
        {
            return Err(EnvironmentError::LeaseAuthorizationInvalid);
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
                State::Provisioning
                    | State::Ready
                    | State::Expiring
                    | State::Deleting
                    | State::Failed
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
    #[error("Resource Work handoff is incomplete or unsafe")]
    InvalidResourceHandoff,
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
    #[error("console eligibility response is incomplete, stale, or scope-mismatched")]
    ConsoleEligibilityInvalid,
    #[error("VM execution binding is incomplete, stale, or scope-mismatched")]
    ExecutionBindingInvalid,
    #[error("Work configuration target is incomplete, stale, or scope-mismatched")]
    WorkConfigurationTargetInvalid,
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

fn valid_token(value: &str, max_len: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_len
        && value.trim() == value
        && !value
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
}

fn valid_private_ip(value: &str) -> bool {
    let Ok(address) = value.parse::<std::net::IpAddr>() else {
        return false;
    };
    match address {
        std::net::IpAddr::V4(address) => {
            address.is_private() || address.is_loopback() || address.is_link_local()
        }
        std::net::IpAddr::V6(address) => {
            address.is_unique_local() || address.is_loopback() || address.is_unicast_link_local()
        }
    }
}

fn valid_vm_username(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn valid_workspace_root(value: &str) -> bool {
    value.starts_with('/')
        && value.len() <= 1_024
        && value != "/"
        && !value.ends_with('/')
        && !value.contains("//")
        && value.split('/').skip(1).all(|component| {
            !component.is_empty()
                && component != "."
                && component != ".."
                && !component.contains('/')
                && !component.contains('\\')
                && !component.chars().any(char::is_control)
        })
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
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub owner_actor_id: ActorId,
    pub expected_revision: Revision,
}

/// Minimal Environment-authoritative ownership result; it never contains endpoints or credentials.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentOwnerResolution {
    pub environment_id: EnvironmentId,
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub owner_actor_id: ActorId,
    pub environment_revision: Revision,
    pub eligibility_expires_at: UtcTimestamp,
}

/// Control-to-Environment request for the authoritative Work execution target.
///
/// Environment resolves the runtime from the exact Work aggregate and its Resource lease. The
/// caller cannot choose a runtime kind or replace the actor/project scope.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentWorkConfigurationTargetQuery {
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub actor_id: ActorId,
    pub expected_revision: Revision,
}

/// Environment-authoritative runtime target for one Work configuration run.
///
/// The response contains only immutable routing and ownership facts. Readiness and lease
/// eligibility are checked by Environment before this response is issued.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentWorkConfigurationTarget {
    pub environment_id: EnvironmentId,
    pub environment_revision: Revision,
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub actor_id: ActorId,
    pub runtime_kind: RuntimeKind,
}

impl EnvironmentWorkConfigurationTarget {
    /// Verifies that a returned target belongs to the exact requested Work scope and revision.
    pub fn validate_for(
        &self,
        environment_id: EnvironmentId,
        request: &EnvironmentWorkConfigurationTargetQuery,
    ) -> Result<(), EnvironmentError> {
        if self.environment_id != environment_id
            || self.environment_revision != request.expected_revision
            || self.project_id != request.project_id
            || self.course_id != request.course_id
            || self.actor_id != request.actor_id
        {
            return Err(EnvironmentError::WorkConfigurationTargetInvalid);
        }
        Ok(())
    }
}

/// Fail-closed Access Service request for exact endpoint eligibility.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentEndpointEligibilityRequest {
    pub environment_id: EnvironmentId,
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub actor_id: ActorId,
    pub subject_kind: EnvironmentAccessSubjectKind,
    pub expected_revision: Revision,
    pub endpoint_ids: Vec<EndpointId>,
}

/// Access-to-Environment request for one browser-console admission snapshot.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentConsoleEligibilityRequest {
    pub environment_id: EnvironmentId,
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub actor_id: ActorId,
    pub subject_kind: EnvironmentAccessSubjectKind,
    pub expected_revision: Revision,
}

/// Runtime-specific console binding selected by Environment Service.
///
/// The KubeVirt binding intentionally carries no VMI locator, Kubernetes
/// credential, or guest password. The executor resolves the exact authoritative
/// VMI again when the one-time capability is consumed.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum EnvironmentConsoleBinding {
    Xterm { terminal: TerminalSpec },
    Novnc,
}

impl EnvironmentConsoleBinding {
    #[must_use]
    pub const fn kind(&self) -> ConsoleKind {
        match self {
            Self::Xterm { .. } => ConsoleKind::Xterm,
            Self::Novnc => ConsoleKind::Novnc,
        }
    }
}

/// Environment-authoritative, credential-free console admission facts.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentConsoleEligibility {
    pub environment_id: EnvironmentId,
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
    pub owner_actor_id: ActorId,
    pub environment_class: EnvironmentClass,
    pub runtime_kind: RuntimeKind,
    pub environment_revision: Revision,
    pub release_id: ReleaseId,
    pub release_version: u64,
    pub eligibility_expires_at: UtcTimestamp,
    pub lease_fence: Option<ConsoleLeaseFence>,
    pub binding: EnvironmentConsoleBinding,
}

impl EnvironmentConsoleEligibility {
    pub fn validate_for(
        &self,
        request: &EnvironmentConsoleEligibilityRequest,
        now: UtcTimestamp,
    ) -> Result<(), EnvironmentError> {
        let lease_valid = match (self.environment_class, &self.lease_fence) {
            (EnvironmentClass::Experiment, None) => true,
            (EnvironmentClass::Work, Some(fence)) => {
                fence.expires_at >= self.eligibility_expires_at
            }
            _ => false,
        };
        let binding_valid = match (&self.binding, self.runtime_kind) {
            (EnvironmentConsoleBinding::Xterm { terminal }, RuntimeKind::Container) => {
                terminal.validate().is_ok()
            }
            (EnvironmentConsoleBinding::Novnc, RuntimeKind::VirtualMachine) => true,
            _ => false,
        };
        if !binding_valid
            || self.environment_id != request.environment_id
            || self.project_id != request.project_id
            || self.course_id != request.course_id
            || (request.subject_kind == EnvironmentAccessSubjectKind::Owner
                && self.owner_actor_id != request.actor_id)
            || self.environment_revision != request.expected_revision
            || self.release_version == 0
            || self.eligibility_expires_at <= now
            || !lease_valid
        {
            return Err(EnvironmentError::ConsoleEligibilityInvalid);
        }
        Ok(())
    }
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
    pub project_id: ProjectId,
    pub course_id: Option<CourseId>,
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
            || self.project_id != request.project_id
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

/// Deployment-supplied client settings for the controlled service-JWT owner-resolver call.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentOwnerResolverClientConfig {
    pub resolver_uri: String,
    pub ca_certificate_locator: String,
    pub timeout_milliseconds: u64,
    pub max_retries: u8,
}

impl EnvironmentOwnerResolverClientConfig {
    /// Rejects implicit endpoints, inline CA material, and unbounded calls.
    pub fn validate(&self) -> Result<(), EnvironmentError> {
        let resolver_authority = self
            .resolver_uri
            .strip_prefix("https://")
            .or_else(|| self.resolver_uri.strip_prefix("http://"));
        if resolver_authority.is_none_or(|authority| {
            authority.is_empty()
                || authority.contains('@')
                || authority.contains('/')
                || authority.contains('?')
                || authority.contains('#')
                || authority.chars().any(char::is_whitespace)
        }) || {
            let locator = self.ca_certificate_locator.as_str();
            !locator.starts_with("secret://")
                || locator.len() <= "secret://".len()
                || locator.contains("-----BEGIN")
                || locator.contains('\n')
        } || !(1..=30_000).contains(&self.timeout_milliseconds)
            || self.max_retries > 3
        {
            return Err(EnvironmentError::InvalidResolverConfiguration);
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::str::FromStr;

    use super::{
        EnvironmentAccessSubjectKind, EnvironmentConsoleBinding, EnvironmentConsoleEligibility,
        EnvironmentConsoleEligibilityRequest, EnvironmentInstance, EnvironmentOperationKind,
        EnvironmentOwnerResolverClientConfig, EnvironmentWorkConfigurationTarget,
        EnvironmentWorkConfigurationTargetQuery, ObservedEnvironmentState, ResourceWorkCleanup,
        ResourceWorkHandoff, ResourceWorkLeaseUpdate,
    };
    use crate::authoring::{EnvironmentClass, RuntimeKind, TerminalSpec};
    use crate::{
        ActorId, CapacityClaimId, CourseId, EnvironmentId, LeaseId, ProjectId, ReleaseId,
        ResourceRequestId, Revision, UtcTimestamp,
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

    #[test]
    fn work_configuration_target_is_fenced_to_the_exact_environment_scope() {
        let environment_id = EnvironmentId::new();
        let project_id = ProjectId::new();
        let actor_id = ActorId::new();
        let revision = Revision::new(4).expect("revision");
        let request = EnvironmentWorkConfigurationTargetQuery {
            project_id,
            course_id: None,
            actor_id,
            expected_revision: revision,
        };
        let target = EnvironmentWorkConfigurationTarget {
            environment_id,
            environment_revision: revision,
            project_id,
            course_id: None,
            actor_id,
            runtime_kind: RuntimeKind::VirtualMachine,
        };

        assert!(target.validate_for(environment_id, &request).is_ok());

        let mut wrong_scope = request;
        wrong_scope.actor_id = ActorId::new();
        assert!(target.validate_for(environment_id, &wrong_scope).is_err());
    }

    #[test]
    fn console_binding_matches_the_authoritative_runtime_without_exposing_a_vmi_locator() {
        let environment_id = EnvironmentId::new();
        let project_id = ProjectId::new();
        let course_id = CourseId::new();
        let actor_id = ActorId::new();
        let now = UtcTimestamp::from_str("2026-08-08T00:00:00.000Z").expect("timestamp");
        let expires = UtcTimestamp::from_str("2026-08-08T00:10:00.000Z").expect("timestamp");
        let request = EnvironmentConsoleEligibilityRequest {
            environment_id,
            project_id,
            course_id: Some(course_id),
            actor_id,
            subject_kind: EnvironmentAccessSubjectKind::Owner,
            expected_revision: Revision::new(3).expect("revision"),
        };
        let mut eligibility = EnvironmentConsoleEligibility {
            environment_id,
            project_id,
            course_id: Some(course_id),
            owner_actor_id: actor_id,
            environment_class: EnvironmentClass::Experiment,
            runtime_kind: RuntimeKind::VirtualMachine,
            environment_revision: Revision::new(3).expect("revision"),
            release_id: ReleaseId::new(),
            release_version: 1,
            eligibility_expires_at: expires,
            lease_fence: None,
            binding: EnvironmentConsoleBinding::Novnc,
        };
        assert!(eligibility.validate_for(&request, now).is_ok());
        assert_eq!(
            eligibility.binding.kind(),
            crate::access::ConsoleKind::Novnc
        );
        eligibility.runtime_kind = RuntimeKind::Container;
        assert!(eligibility.validate_for(&request, now).is_err());
        eligibility.binding = EnvironmentConsoleBinding::Xterm {
            terminal: TerminalSpec {
                executable: "/bin/sh".to_owned(),
                args: Vec::new(),
                working_directory: "/workspace".to_owned(),
            },
        };
        assert!(eligibility.validate_for(&request, now).is_ok());
    }
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
                        State::Provisioning
                            | State::Ready
                            | State::Expiring
                            | State::Deleting
                            | State::Failed
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
    fn resolver_client_requires_https_ca_locator_and_bounded_retry() {
        let mut config = EnvironmentOwnerResolverClientConfig {
            resolver_uri: "https://environment-service.internal".to_owned(),
            ca_certificate_locator: "secret://environment-resolver/ca".to_owned(),
            timeout_milliseconds: 2_000,
            max_retries: 2,
        };
        assert!(config.validate().is_ok());
        config.ca_certificate_locator = "-----BEGIN CERTIFICATE-----".to_owned();
        assert!(config.validate().is_err());
        config.ca_certificate_locator = "secret://environment-resolver/ca".to_owned();
        config.resolver_uri = "https://user@environment-service.internal".to_owned();
        assert!(config.validate().is_err());
    }

    #[test]
    fn resource_work_handoff_requires_all_fences_and_a_safe_display_label() {
        let mut handoff = ResourceWorkHandoff {
            version: 1,
            request_id: ResourceRequestId::new(),
            request_revision: Revision::new(2).expect("revision"),
            lease_id: LeaseId::new(),
            lease_revision: Revision::new(2).expect("revision"),
            claim_id: CapacityClaimId::new(),
            claim_revision: Revision::new(3).expect("revision"),
            environment_id: EnvironmentId::new(),
            project_id: ProjectId::new(),
            course_id: Some(CourseId::new()),
            owner_actor_id: ActorId::new(),
            display_label: "workbench".to_owned(),
            release_id: ReleaseId::new(),
            release_version: 1,
            provider_binding: "kubernetes-standard".to_owned(),
            capacity_binding: "claim-123".to_owned(),
            approved_resources: super::WorkloadResources {
                cpu_millicores: 1,
                memory_bytes: 1,
                storage_bytes: 1,
                gpu: None,
            },
            gpu_allocation: None,
            trace_id: "resource-handoff-123".to_owned(),
        };
        assert!(handoff.validate().is_ok());
        handoff.trace_id = "\n".to_owned();
        assert!(handoff.validate().is_err());
    }

    #[test]
    fn resource_work_lease_update_and_cleanup_are_exact_and_bounded() {
        let lease_id = LeaseId::new();
        let environment_id = EnvironmentId::new();
        let project_id = ProjectId::new();
        let course_id = CourseId::new();
        let owner_actor_id = ActorId::new();
        let update = ResourceWorkLeaseUpdate {
            version: 1,
            lease_id,
            lease_revision: Revision::new(4).expect("revision"),
            environment_id,
            project_id,
            course_id: Some(course_id),
            owner_actor_id,
            capacity_binding: "capacity-claim".into(),
            expires_at: UtcTimestamp::from_str("2026-08-01T00:00:00.000Z").expect("timestamp"),
            trace_id: "resource-lease-sync".into(),
        };
        assert!(update.validate().is_ok());
        let mut cleanup = ResourceWorkCleanup {
            version: 1,
            lease_id,
            lease_revision: update.lease_revision,
            environment_id,
            project_id,
            course_id: Some(course_id),
            owner_actor_id,
            capacity_binding: update.capacity_binding,
            reason_code: "LW_RESOURCE_LEASE_EXPIRED".into(),
            trace_id: "resource-work-cleanup".into(),
        };
        assert!(cleanup.validate().is_ok());
        cleanup.reason_code = "expired".into();
        assert!(cleanup.validate().is_err());
    }
}
