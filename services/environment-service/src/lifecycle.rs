use contracts::environment::{
    DesiredEnvironmentState, EndpointHealth, EnvironmentEndpoint, EnvironmentError,
    EnvironmentInstance, EnvironmentLeaseAuthorization, EnvironmentLifecycleCommand,
    EnvironmentOperation, EnvironmentOperationKind, ObservedEnvironmentState, OperationState,
};
use contracts::{ArtifactRef, OperationId, Revision, UtcTimestamp};
use serde::{Deserialize, Serialize};

/// Repository-facing alias for the contracts-owned lifecycle command.
pub type LifecycleCommand = EnvironmentLifecycleCommand;

/// Sanitized result returned by an explicitly bound Provider.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AppliedProviderObservation {
    pub next_state: ObservedEnvironmentState,
    pub endpoints: Vec<EnvironmentEndpoint>,
    pub cleanup_evidence: Option<ArtifactRef>,
    pub operation_complete: bool,
}

/// Stable lifecycle implementation failures.
#[derive(Debug, thiserror::Error)]
pub enum LifecycleError {
    #[error("LW_ENVIRONMENT_REVISION_CONFLICT")]
    RevisionConflict,
    #[error("LW_ENVIRONMENT_REVISION_OVERFLOW")]
    RevisionOverflow,
    #[error("LW_ENVIRONMENT_GENERATION_OVERFLOW")]
    GenerationOverflow,
    #[error("LW_ENVIRONMENT_PROVIDER_STEP_OVERFLOW")]
    ProviderStepOverflow,
    #[error("LW_ENVIRONMENT_COMMAND_INVALID")]
    CommandInvalid,
    #[error("LW_ENVIRONMENT_OPERATION_ACTIVE")]
    OperationActive,
    #[error("LW_ENVIRONMENT_OPERATION_MISMATCH")]
    OperationMismatch,
    #[error("LW_ENVIRONMENT_RETRY_EXHAUSTED")]
    RetryExhausted,
    #[error("LW_ENVIRONMENT_RETRY_TIME_INVALID")]
    RetryTimeInvalid,
    #[error("LW_ENVIRONMENT_DIAGNOSTIC_INVALID")]
    DiagnosticInvalid,
    #[error("LW_ENVIRONMENT_PROVIDER_OBSERVATION_INVALID")]
    ProviderObservationInvalid,
    #[error("LW_ENVIRONMENT_ELIGIBILITY_EXPIRED")]
    EligibilityExpired,
    #[error("LW_ENVIRONMENT_LIFECYCLE_INVALID: {0}")]
    Contract(#[from] EnvironmentError),
}

/// Accepts a command without performing Provider I/O.
pub fn plan_command(
    current: &EnvironmentInstance,
    command: &LifecycleCommand,
    operation_id: OperationId,
) -> Result<EnvironmentInstance, LifecycleError> {
    plan_command_authorized(current, command, operation_id, None, command.accepted_at)
}

/// Accepts a command with a Resource-authoritative Lease snapshot and database clock.
#[allow(
    clippy::too_many_lines,
    reason = "command acceptance keeps all authority, concurrency, and transition guards in one auditable boundary"
)]
pub fn plan_command_authorized(
    current: &EnvironmentInstance,
    command: &LifecycleCommand,
    operation_id: OperationId,
    lease_authorization: Option<EnvironmentLeaseAuthorization>,
    authority_now: UtcTimestamp,
) -> Result<EnvironmentInstance, LifecycleError> {
    current.validate()?;
    if current.id != command.environment_id || current.revision != command.expected_revision {
        return Err(LifecycleError::RevisionConflict);
    }
    if !(1..=100).contains(&command.max_attempts)
        || command.deadline_at <= command.accepted_at
        || command.trace_id.is_empty()
        || command.trace_id.len() > 128
        || !command
            .trace_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
        || (command.kind == EnvironmentOperationKind::Reset) != command.reset_target.is_some()
    {
        return Err(LifecycleError::CommandInvalid);
    }
    if matches!(
        command.kind,
        EnvironmentOperationKind::Start
            | EnvironmentOperationKind::Retry
            | EnvironmentOperationKind::Recover
            | EnvironmentOperationKind::Reset
    ) && current.eligibility_expires_at <= authority_now
    {
        return Err(LifecycleError::EligibilityExpired);
    }
    let destructive = matches!(
        command.kind,
        EnvironmentOperationKind::Cancel
            | EnvironmentOperationKind::Expire
            | EnvironmentOperationKind::Delete
            | EnvironmentOperationKind::Cleanup
    );
    if matches!(
        current.operation.state,
        OperationState::Accepted | OperationState::Running | OperationState::Cancelling
    ) && !destructive
    {
        return Err(LifecycleError::OperationActive);
    }
    if current.desired_state == DesiredEnvironmentState::Deleted
        && !matches!(
            command.kind,
            EnvironmentOperationKind::Delete
                | EnvironmentOperationKind::Cleanup
                | EnvironmentOperationKind::Retry
                | EnvironmentOperationKind::Recover
        )
    {
        return Err(LifecycleError::CommandInvalid);
    }
    EnvironmentInstance::ensure_operation_allowed(current.observed_state, command.kind)?;
    if !current.endpoints.is_empty()
        && !matches!(
            command.kind,
            EnvironmentOperationKind::Create | EnvironmentOperationKind::Start
        )
        && command.access_revocation_revision.is_none()
    {
        return Err(LifecycleError::Contract(
            EnvironmentError::GrantRevocationRequired,
        ));
    }

    let retry_from_phase = if matches!(
        command.kind,
        EnvironmentOperationKind::Retry | EnvironmentOperationKind::Recover
    ) {
        Some(current.failed_phase.ok_or(LifecycleError::CommandInvalid)?)
    } else {
        None
    };
    if current.class == contracts::authoring::EnvironmentClass::Work
        && matches!(
            command.kind,
            EnvironmentOperationKind::Start
                | EnvironmentOperationKind::Retry
                | EnvironmentOperationKind::Recover
                | EnvironmentOperationKind::Reset
        )
    {
        validate_lease_authorization(current, lease_authorization.as_ref(), authority_now)?;
    } else if lease_authorization.is_some() {
        return Err(LifecycleError::CommandInvalid);
    }

    let mut planned = current.clone();
    planned.revision = next_revision(current.revision)?;
    planned.desired_state = desired_state(current, command.kind);
    planned.observed_state = accepted_observed_state(current, command.kind)?;
    if increments_generation(command.kind) {
        planned.generation = current
            .generation
            .checked_add(1)
            .ok_or(LifecycleError::GenerationOverflow)?;
    }
    if invalidates_endpoints(command.kind) {
        for endpoint in &mut planned.endpoints {
            endpoint.health = if destructive {
                EndpointHealth::Removed
            } else {
                EndpointHealth::Unhealthy
            };
        }
    }
    planned.last_diagnostic_code = None;
    planned.failed_phase = None;
    planned.operation = EnvironmentOperation {
        id: operation_id,
        kind: command.kind,
        state: if command.kind == EnvironmentOperationKind::Cancel {
            OperationState::Cancelling
        } else {
            OperationState::Accepted
        },
        accepted_revision: planned.revision,
        attempt: 1,
        provider_step: 1,
        max_attempts: command.max_attempts,
        next_attempt_at: command.accepted_at,
        actor_id: command.actor_id,
        trace_id: command.trace_id.clone(),
        accepted_at: command.accepted_at,
        deadline_at: command.deadline_at,
        cleanup_started_at: None,
        diagnostic_code: None,
        preserve_mutable_disk: command.preserve_mutable_disk,
        access_revocation_revision: command.access_revocation_revision,
        retry_from_phase,
        reset_target: command.reset_target.clone(),
        lease_authorization,
    };
    planned.validate()?;
    Ok(planned)
}

/// Applies one successful Provider observation while preserving transition validation.
pub fn apply_provider_observation(
    current: &EnvironmentInstance,
    operation_id: OperationId,
    observation: AppliedProviderObservation,
) -> Result<EnvironmentInstance, LifecycleError> {
    current.validate()?;
    ensure_current_operation(current, operation_id)?;
    validate_provider_observation(current, &observation)?;
    if current.observed_state != observation.next_state {
        EnvironmentInstance::ensure_transition(current.observed_state, observation.next_state)?;
    }
    let mut updated = current.clone();
    updated.revision = next_revision(current.revision)?;
    updated.observed_state = observation.next_state;
    updated.endpoints = observation.endpoints;
    updated.cleanup_evidence = observation.cleanup_evidence;
    let preserving_cleanup_failure = observation.operation_complete
        && observation.next_state == ObservedEnvironmentState::Deleted
        && current.operation.cleanup_started_at.is_some()
        && current.operation.diagnostic_code.is_some();
    if !preserving_cleanup_failure {
        updated.last_diagnostic_code = None;
        updated.operation.diagnostic_code = None;
    }
    updated.operation.state = if preserving_cleanup_failure {
        OperationState::Failed
    } else if observation.operation_complete
        && current.operation.kind == EnvironmentOperationKind::Cancel
    {
        OperationState::Cancelled
    } else if observation.operation_complete {
        OperationState::Succeeded
    } else {
        OperationState::Running
    };
    if observation.operation_complete {
        updated.observed_generation = updated.generation;
    } else {
        updated.operation.provider_step = updated
            .operation
            .provider_step
            .checked_add(1)
            .ok_or(LifecycleError::ProviderStepOverflow)?;
    }
    updated
        .validate()
        .map_err(|_| LifecycleError::ProviderObservationInvalid)?;
    Ok(updated)
}

/// Moves an expired operation into fenced cleanup before any Provider cleanup call.
pub fn begin_timeout_cleanup(
    current: &EnvironmentInstance,
    cleanup_started_at: UtcTimestamp,
    cleanup_deadline_at: UtcTimestamp,
) -> Result<EnvironmentInstance, LifecycleError> {
    ensure_current_operation(current, current.operation.id)?;
    if cleanup_started_at < current.operation.accepted_at
        || cleanup_deadline_at <= cleanup_started_at
    {
        return Err(LifecycleError::RetryTimeInvalid);
    }
    if !current.endpoints.is_empty() && current.operation.access_revocation_revision.is_none() {
        return Err(LifecycleError::Contract(
            EnvironmentError::GrantRevocationRequired,
        ));
    }
    if current.observed_state != ObservedEnvironmentState::Deleting {
        EnvironmentInstance::ensure_transition(
            current.observed_state,
            ObservedEnvironmentState::Deleting,
        )?;
    }
    let mut updated = current.clone();
    updated.revision = next_revision(current.revision)?;
    if updated.desired_state != DesiredEnvironmentState::Deleted {
        updated.generation = current
            .generation
            .checked_add(1)
            .ok_or(LifecycleError::GenerationOverflow)?;
    }
    updated.desired_state = DesiredEnvironmentState::Deleted;
    updated.observed_state = ObservedEnvironmentState::Deleting;
    updated.operation.state = OperationState::Cancelling;
    updated.operation.attempt = 1;
    updated.operation.provider_step = updated
        .operation
        .provider_step
        .checked_add(1)
        .ok_or(LifecycleError::ProviderStepOverflow)?;
    updated.operation.next_attempt_at = cleanup_started_at;
    updated.operation.deadline_at = cleanup_deadline_at;
    updated.operation.cleanup_started_at = Some(cleanup_started_at);
    updated.operation.diagnostic_code = Some("LW_ENVIRONMENT_PROVIDER_TIMEOUT".to_owned());
    updated.last_diagnostic_code = Some("LW_ENVIRONMENT_PROVIDER_TIMEOUT".to_owned());
    for endpoint in &mut updated.endpoints {
        endpoint.health = EndpointHealth::Removed;
    }
    updated.validate()?;
    Ok(updated)
}

/// Records a bounded retry without changing observed lifecycle state.
pub fn apply_retry(
    current: &EnvironmentInstance,
    operation_id: OperationId,
    diagnostic_code: &str,
    next_attempt_at: UtcTimestamp,
) -> Result<EnvironmentInstance, LifecycleError> {
    ensure_current_operation(current, operation_id)?;
    validate_diagnostic_code(diagnostic_code)?;
    if current.operation.attempt >= current.operation.max_attempts {
        return Err(LifecycleError::RetryExhausted);
    }
    if next_attempt_at <= current.operation.next_attempt_at
        || next_attempt_at > current.operation.deadline_at
    {
        return Err(LifecycleError::RetryTimeInvalid);
    }
    let mut updated = current.clone();
    updated.revision = next_revision(current.revision)?;
    updated.operation.attempt += 1;
    updated.operation.next_attempt_at = next_attempt_at;
    updated.operation.state = OperationState::Accepted;
    updated.operation.diagnostic_code = Some(diagnostic_code.to_owned());
    updated.last_diagnostic_code = Some(diagnostic_code.to_owned());
    updated.validate()?;
    Ok(updated)
}

/// Exhausts an operation into `Failed`, removing endpoint eligibility.
pub fn apply_provider_failure(
    current: &EnvironmentInstance,
    operation_id: OperationId,
    diagnostic_code: &str,
) -> Result<EnvironmentInstance, LifecycleError> {
    ensure_current_operation(current, operation_id)?;
    validate_diagnostic_code(diagnostic_code)?;
    if current.observed_state != ObservedEnvironmentState::Failed {
        EnvironmentInstance::ensure_transition(
            current.observed_state,
            ObservedEnvironmentState::Failed,
        )?;
    }
    let mut updated = current.clone();
    updated.revision = next_revision(current.revision)?;
    updated.observed_state = ObservedEnvironmentState::Failed;
    updated.failed_phase = Some(current.observed_state);
    updated.operation.state = OperationState::Failed;
    updated.operation.diagnostic_code = Some(diagnostic_code.to_owned());
    updated.last_diagnostic_code = Some(diagnostic_code.to_owned());
    for endpoint in &mut updated.endpoints {
        endpoint.health = EndpointHealth::Unhealthy;
    }
    updated.validate()?;
    Ok(updated)
}

fn ensure_current_operation(
    current: &EnvironmentInstance,
    operation_id: OperationId,
) -> Result<(), LifecycleError> {
    if current.operation.id != operation_id
        || matches!(
            current.operation.state,
            OperationState::Succeeded | OperationState::Failed | OperationState::Cancelled
        )
    {
        return Err(LifecycleError::OperationMismatch);
    }
    Ok(())
}

fn validate_diagnostic_code(code: &str) -> Result<(), LifecycleError> {
    contracts::DiagnosticCode::parse(code)
        .map(|_| ())
        .map_err(|_| LifecycleError::DiagnosticInvalid)
}

fn validate_provider_observation(
    current: &EnvironmentInstance,
    observation: &AppliedProviderObservation,
) -> Result<(), LifecycleError> {
    use EnvironmentOperationKind as Operation;
    use ObservedEnvironmentState as State;

    let transition_matches_operation = matches!(
        (
            current.operation.kind,
            current.observed_state,
            observation.next_state,
        ),
        (
            Operation::Create | Operation::Retry | Operation::Recover,
            State::Requested,
            State::Validating
        ) | (
            Operation::Create | Operation::Retry | Operation::Recover | Operation::Reset,
            State::Validating,
            State::Building
        ) | (
            Operation::Create | Operation::Retry | Operation::Recover | Operation::Reset,
            State::Building,
            State::Provisioning
        ) | (
            Operation::Create
                | Operation::Retry
                | Operation::Recover
                | Operation::Start
                | Operation::Restart
                | Operation::Reset,
            State::Provisioning,
            State::Provisioning | State::Ready | State::Stopped,
        ) | (Operation::Start, State::Stopped, State::Provisioning)
            | (Operation::Stop, State::Stopping, State::Stopped)
            | (
                Operation::Freeze | Operation::Retry | Operation::Recover,
                State::Updating,
                State::Ready
            )
            | (
                Operation::Expire,
                State::Expiring,
                State::Stopped | State::Deleting
            )
            | (Operation::Expire, State::Stopped, State::Deleting)
            | (
                Operation::Retry | Operation::Recover,
                State::Stopping | State::Expiring,
                State::Stopped | State::Deleting
            )
            | (_, State::Deleting, State::Deleted)
    );
    if !transition_matches_operation {
        return Err(LifecycleError::ProviderObservationInvalid);
    }

    let is_target = match current.desired_state {
        DesiredEnvironmentState::Running => observation.next_state == State::Ready,
        DesiredEnvironmentState::Stopped => observation.next_state == State::Stopped,
        DesiredEnvironmentState::Deleted => observation.next_state == State::Deleted,
    };
    let is_expire_cleanup_checkpoint = current.operation.kind == Operation::Expire
        && current.observed_state == State::Expiring
        && observation.next_state == State::Stopped
        && !observation.operation_complete;
    let is_steady_state = matches!(
        observation.next_state,
        State::Ready | State::Stopped | State::Deleted
    );
    if (is_steady_state && !is_target && !is_expire_cleanup_checkpoint)
        || observation.operation_complete != is_target
    {
        return Err(LifecycleError::ProviderObservationInvalid);
    }
    Ok(())
}

fn next_revision(current: Revision) -> Result<Revision, LifecycleError> {
    let value = current
        .get()
        .checked_add(1)
        .ok_or(LifecycleError::RevisionOverflow)?;
    Revision::new(value).map_err(|_| LifecycleError::RevisionOverflow)
}

const fn increments_generation(kind: EnvironmentOperationKind) -> bool {
    !matches!(kind, EnvironmentOperationKind::Freeze)
}

const fn invalidates_endpoints(kind: EnvironmentOperationKind) -> bool {
    matches!(
        kind,
        EnvironmentOperationKind::Stop
            | EnvironmentOperationKind::Restart
            | EnvironmentOperationKind::Reset
            | EnvironmentOperationKind::Cancel
            | EnvironmentOperationKind::Expire
            | EnvironmentOperationKind::Delete
            | EnvironmentOperationKind::Cleanup
            | EnvironmentOperationKind::Freeze
    )
}

const fn desired_state(
    current: &EnvironmentInstance,
    kind: EnvironmentOperationKind,
) -> DesiredEnvironmentState {
    match kind {
        EnvironmentOperationKind::Start | EnvironmentOperationKind::Restart => {
            DesiredEnvironmentState::Running
        }
        EnvironmentOperationKind::Stop => DesiredEnvironmentState::Stopped,
        EnvironmentOperationKind::Cancel
        | EnvironmentOperationKind::Expire
        | EnvironmentOperationKind::Delete
        | EnvironmentOperationKind::Cleanup => DesiredEnvironmentState::Deleted,
        EnvironmentOperationKind::Reset
        | EnvironmentOperationKind::Retry
        | EnvironmentOperationKind::Recover
        | EnvironmentOperationKind::Create
        | EnvironmentOperationKind::Freeze => current.desired_state,
    }
}

fn accepted_observed_state(
    current: &EnvironmentInstance,
    kind: EnvironmentOperationKind,
) -> Result<ObservedEnvironmentState, LifecycleError> {
    let current_state = current.observed_state;
    let next = match kind {
        EnvironmentOperationKind::Stop => ObservedEnvironmentState::Stopping,
        EnvironmentOperationKind::Reset if current_state == ObservedEnvironmentState::Failed => {
            ObservedEnvironmentState::Validating
        }
        EnvironmentOperationKind::Restart | EnvironmentOperationKind::Reset => {
            ObservedEnvironmentState::Provisioning
        }
        EnvironmentOperationKind::Retry | EnvironmentOperationKind::Recover => {
            retry_resume_state(current.failed_phase.ok_or(LifecycleError::CommandInvalid)?)?
        }
        EnvironmentOperationKind::Cancel | EnvironmentOperationKind::Delete => {
            ObservedEnvironmentState::Deleting
        }
        EnvironmentOperationKind::Expire => ObservedEnvironmentState::Expiring,
        EnvironmentOperationKind::Cleanup
            if current_state != ObservedEnvironmentState::Deleting =>
        {
            ObservedEnvironmentState::Deleting
        }
        EnvironmentOperationKind::Freeze => ObservedEnvironmentState::Updating,
        _ => current_state,
    };
    if next != current_state {
        EnvironmentInstance::ensure_transition(current_state, next)?;
    }
    Ok(next)
}

fn retry_resume_state(
    failed_phase: ObservedEnvironmentState,
) -> Result<ObservedEnvironmentState, LifecycleError> {
    use ObservedEnvironmentState as State;
    match failed_phase {
        State::Requested | State::Validating => Ok(State::Validating),
        State::Building => Ok(State::Building),
        State::Provisioning | State::Ready | State::Stopped => Ok(State::Provisioning),
        State::Stopping => Ok(State::Stopping),
        State::Updating => Ok(State::Updating),
        State::Expiring => Ok(State::Expiring),
        State::Deleting => Ok(State::Deleting),
        State::Deleted | State::Failed => Err(LifecycleError::CommandInvalid),
    }
}

fn validate_lease_authorization(
    current: &EnvironmentInstance,
    authorization: Option<&EnvironmentLeaseAuthorization>,
    authority_now: UtcTimestamp,
) -> Result<(), LifecycleError> {
    let authorization = authorization.ok_or(LifecycleError::Contract(
        EnvironmentError::LeaseAuthorizationRequired,
    ))?;
    if Some(authorization.lease_id) != current.lease_id
        || authorization.environment_id != current.id
        || authorization.course_id != current.course_id
        || authorization.owner_actor_id != current.owner_id
        || Some(authorization.capacity_binding.as_str()) != current.capacity_binding.as_deref()
        || authorization.active_from > authority_now
        || authorization.expires_at <= authority_now
    {
        return Err(LifecycleError::Contract(
            EnvironmentError::LeaseAuthorizationInvalid,
        ));
    }
    Ok(())
}
