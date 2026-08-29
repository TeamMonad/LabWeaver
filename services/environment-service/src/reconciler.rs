use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use contracts::UtcTimestamp;
use contracts::environment::{
    EnvironmentInstance, EnvironmentOperationKind, ObservedEnvironmentState, OperationState,
};
use serde::{Deserialize, Serialize};
use tokio::time::timeout;

use crate::{
    EnvironmentStoreError, PgEnvironmentStore, apply_provider_failure, apply_provider_observation,
    apply_retry, begin_timeout_cleanup,
};

pub type ProviderObservation = crate::lifecycle::AppliedProviderObservation;

/// One explicit, bounded Provider side effect selected from durable state.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconcileAction {
    Validate,
    Build,
    Provision,
    Observe,
    Start,
    Stop,
    Restart,
    Reset,
    Configure,
    Cleanup,
}

/// Payload-free Provider failure safe to persist and expose in diagnostics.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderFailure {
    pub code: ProviderFailureCode,
    pub retryable: bool,
}

impl ProviderFailure {
    #[must_use]
    pub const fn diagnostic_code(self) -> &'static str {
        self.code.diagnostic_code()
    }
}

/// Closed Provider failure family; raw Provider messages are never persisted.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderFailureCode {
    Unavailable,
    Transient,
    Rejected,
    ObservationInvalid,
    CleanupFailed,
}

impl ProviderFailureCode {
    #[must_use]
    pub const fn diagnostic_code(self) -> &'static str {
        match self {
            Self::Unavailable => "LW_ENVIRONMENT_PROVIDER_UNAVAILABLE",
            Self::Transient => "LW_ENVIRONMENT_PROVIDER_TRANSIENT",
            Self::Rejected => "LW_ENVIRONMENT_PROVIDER_REJECTED",
            Self::ObservationInvalid => "LW_ENVIRONMENT_PROVIDER_OBSERVATION_INVALID",
            Self::CleanupFailed => "LW_ENVIRONMENT_PROVIDER_CLEANUP_FAILED",
        }
    }
}

/// Explicit Environment Provider contract. Implementations may not choose a fallback Provider.
#[async_trait]
pub trait EnvironmentProvider: Send + Sync {
    fn binding(&self) -> &str;

    async fn execute(
        &self,
        action: ReconcileAction,
        instance: &EnvironmentInstance,
    ) -> Result<ProviderObservation, ProviderFailure>;
}

/// Exact-name Provider registry. Duplicate and empty bindings are rejected.
#[derive(Default)]
pub struct ProviderRegistry {
    providers: BTreeMap<String, Arc<dyn EnvironmentProvider>>,
}

impl ProviderRegistry {
    pub fn register(
        &mut self,
        provider: Arc<dyn EnvironmentProvider>,
    ) -> Result<(), ReconcileError> {
        let binding = provider.binding();
        if binding.trim().is_empty() || self.providers.contains_key(binding) {
            return Err(ReconcileError::InvalidProviderRegistry);
        }
        self.providers.insert(binding.to_owned(), provider);
        Ok(())
    }

    fn resolve(&self, binding: &str) -> Result<Arc<dyn EnvironmentProvider>, ReconcileError> {
        self.providers
            .get(binding)
            .cloned()
            .ok_or(ReconcileError::ProviderUnavailable)
    }
}

/// One reconciliation executor with a hard per-call timeout.
pub struct Reconciler {
    registry: ProviderRegistry,
    provider_timeout: Duration,
}

/// Durable one-item reconciler worker. A caller may drive this in a bounded loop.
pub struct ReconcileWorker {
    store: PgEnvironmentStore,
    reconciler: Reconciler,
    lease_duration: Duration,
    retry_delay: Duration,
}

impl ReconcileWorker {
    pub fn new(
        store: PgEnvironmentStore,
        reconciler: Reconciler,
        lease_duration: Duration,
        retry_delay: Duration,
    ) -> Result<Self, ReconcileWorkerError> {
        let minimum_lease = reconciler
            .provider_timeout
            .checked_add(Duration::from_secs(1))
            .ok_or(ReconcileWorkerError::InvalidConfiguration)?;
        if lease_duration.is_zero()
            || lease_duration > Duration::from_mins(5)
            || retry_delay.is_zero()
            || retry_delay > Duration::from_mins(5)
            || lease_duration < minimum_lease
            || !is_millisecond_duration(lease_duration)
            || !is_millisecond_duration(retry_delay)
            || !is_millisecond_duration(reconciler.provider_timeout)
        {
            return Err(ReconcileWorkerError::InvalidConfiguration);
        }
        Ok(Self {
            store,
            reconciler,
            lease_duration,
            retry_delay,
        })
    }

    /// Claims and processes at most one due operation.
    pub async fn run_once(
        &self,
        worker_id: &str,
        now: UtcTimestamp,
    ) -> Result<ReconcileWorkerOutcome, ReconcileWorkerError> {
        let Some(lease) = self.store.claim_due(worker_id, self.lease_duration).await? else {
            return Ok(ReconcileWorkerOutcome::Idle);
        };
        self.store.heartbeat(&lease, self.lease_duration).await?;
        if now > lease.instance.operation.deadline_at
            && lease.instance.operation.cleanup_started_at.is_none()
        {
            let cleanup_deadline =
                self.cleanup_deadline(now, lease.instance.operation.max_attempts)?;
            let updated = begin_timeout_cleanup(&lease.instance, now, cleanup_deadline)?;
            self.store.save_reconciled(&lease, &updated).await?;
            return Ok(ReconcileWorkerOutcome::Advanced {
                state: updated.observed_state,
                terminal: false,
            });
        }
        match self.reconciler.execute_once(&lease.instance, now).await {
            Ok(observation) => {
                let updated = match apply_provider_observation(
                    &lease.instance,
                    lease.instance.operation.id,
                    observation,
                ) {
                    Ok(updated) => updated,
                    Err(crate::LifecycleError::ProviderObservationInvalid) => {
                        let updated = apply_provider_failure(
                            &lease.instance,
                            lease.instance.operation.id,
                            "LW_ENVIRONMENT_PROVIDER_OBSERVATION_INVALID",
                        )?;
                        self.store.save_reconciled(&lease, &updated).await?;
                        return Ok(ReconcileWorkerOutcome::Failed {
                            diagnostic_code: "LW_ENVIRONMENT_PROVIDER_OBSERVATION_INVALID",
                        });
                    }
                    Err(error) => return Err(error.into()),
                };
                // A non-terminal observation must not be immediately claimed again.  The
                // container and KubeVirt providers both return `operation_complete = false`
                // while a resource is still converging; leaving `next_attempt_at` at the
                // operation acceptance time creates a hot loop that repeatedly persists the
                // same observation, inflates the public revision, and starves the cluster.
                let updated =
                    Self::defer_non_terminal_observation(&updated, now, self.retry_delay)?;
                self.store.save_reconciled(&lease, &updated).await?;
                Ok(ReconcileWorkerOutcome::Advanced {
                    state: updated.observed_state,
                    terminal: matches!(
                        updated.operation.state,
                        OperationState::Succeeded
                            | OperationState::Failed
                            | OperationState::Cancelled
                    ),
                })
            }
            Err(error) => {
                let diagnostic_code = error.diagnostic_code();
                if error.retryable()
                    && lease.instance.operation.attempt < lease.instance.operation.max_attempts
                {
                    let retry_at = add_duration(now, self.retry_delay)?;
                    if retry_at <= lease.instance.operation.deadline_at {
                        let updated = apply_retry(
                            &lease.instance,
                            lease.instance.operation.id,
                            diagnostic_code,
                            retry_at,
                        )?;
                        self.store.save_reconciled(&lease, &updated).await?;
                        return Ok(ReconcileWorkerOutcome::RetryScheduled {
                            attempt: updated.operation.attempt,
                        });
                    }
                }
                let updated = apply_provider_failure(
                    &lease.instance,
                    lease.instance.operation.id,
                    diagnostic_code,
                )?;
                self.store.save_reconciled(&lease, &updated).await?;
                Ok(ReconcileWorkerOutcome::Failed { diagnostic_code })
            }
        }
    }

    fn defer_non_terminal_observation(
        instance: &EnvironmentInstance,
        now: UtcTimestamp,
        retry_delay: Duration,
    ) -> Result<EnvironmentInstance, ReconcileWorkerError> {
        if matches!(
            instance.operation.state,
            OperationState::Succeeded | OperationState::Failed | OperationState::Cancelled
        ) {
            return Ok(instance.clone());
        }
        let mut updated = instance.clone();
        let next_attempt_at = add_duration(now, retry_delay)?;
        updated.operation.next_attempt_at = next_attempt_at.min(updated.operation.deadline_at);
        updated.validate().map_err(|_| {
            ReconcileWorkerError::Lifecycle(crate::LifecycleError::ProviderObservationInvalid)
        })?;
        Ok(updated)
    }

    fn cleanup_deadline(
        &self,
        now: UtcTimestamp,
        max_attempts: u32,
    ) -> Result<UtcTimestamp, ReconcileWorkerError> {
        let provider_budget = self
            .reconciler
            .provider_timeout
            .checked_mul(max_attempts)
            .ok_or(ReconcileWorkerError::ClockOverflow)?;
        let retry_budget = self
            .retry_delay
            .checked_mul(max_attempts.saturating_sub(1))
            .ok_or(ReconcileWorkerError::ClockOverflow)?;
        let cleanup_budget = provider_budget
            .checked_add(retry_budget)
            .and_then(|duration| duration.checked_add(Duration::from_secs(1)))
            .ok_or(ReconcileWorkerError::ClockOverflow)?;
        add_duration(now, cleanup_budget)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReconcileWorkerOutcome {
    Idle,
    Advanced {
        state: ObservedEnvironmentState,
        terminal: bool,
    },
    RetryScheduled {
        attempt: u32,
    },
    Failed {
        diagnostic_code: &'static str,
    },
}

impl Reconciler {
    pub fn new(
        registry: ProviderRegistry,
        provider_timeout: Duration,
    ) -> Result<Self, ReconcileError> {
        if provider_timeout.is_zero() || provider_timeout > Duration::from_mins(5) {
            return Err(ReconcileError::InvalidTimeout);
        }
        Ok(Self {
            registry,
            provider_timeout,
        })
    }

    pub async fn execute_once(
        &self,
        instance: &EnvironmentInstance,
        now: UtcTimestamp,
    ) -> Result<ProviderObservation, ReconcileError> {
        let action = next_action(instance, now)?;
        let provider = self.registry.resolve(&instance.provider_binding)?;
        let result = timeout(self.provider_timeout, provider.execute(action, instance))
            .await
            .map_err(|_| ReconcileError::ProviderTimeout)?;
        result.map_err(ReconcileError::Provider)
    }
}

/// Selects the next action solely from the persisted operation and lifecycle state.
pub fn next_action(
    instance: &EnvironmentInstance,
    now: UtcTimestamp,
) -> Result<ReconcileAction, ReconcileError> {
    use EnvironmentOperationKind as Operation;
    use ObservedEnvironmentState as State;

    if matches!(
        instance.operation.state,
        OperationState::Succeeded | OperationState::Failed | OperationState::Cancelled
    ) {
        return Err(ReconcileError::OperationTerminal);
    }
    if now > instance.operation.deadline_at
        && instance.observed_state == ObservedEnvironmentState::Deleting
    {
        return Ok(ReconcileAction::Cleanup);
    }
    match (instance.operation.kind, instance.observed_state) {
        (
            Operation::Create | Operation::Retry | Operation::Recover,
            State::Requested | State::Validating,
        )
        | (Operation::Reset, State::Validating) => Ok(ReconcileAction::Validate),
        (
            Operation::Create | Operation::Retry | Operation::Recover | Operation::Reset,
            State::Building,
        ) => Ok(ReconcileAction::Build),
        (Operation::Retry | Operation::Recover, State::Provisioning)
            if instance.operation.retry_from_phase == Some(State::Stopped)
                && instance.desired_state
                    == contracts::environment::DesiredEnvironmentState::Running =>
        {
            Ok(ReconcileAction::Start)
        }
        (Operation::Create | Operation::Retry | Operation::Recover, State::Provisioning) => {
            Ok(ReconcileAction::Provision)
        }
        (Operation::Start, State::Stopped) => Ok(ReconcileAction::Start),
        (Operation::Stop | Operation::Retry | Operation::Recover, State::Stopping)
        | (Operation::Expire | Operation::Retry | Operation::Recover, State::Expiring) => {
            Ok(ReconcileAction::Stop)
        }
        (Operation::Restart, State::Provisioning) => Ok(ReconcileAction::Restart),
        (Operation::Reset, State::Provisioning) => Ok(ReconcileAction::Reset),
        (Operation::Retry | Operation::Recover, State::Updating) => Ok(ReconcileAction::Configure),
        (Operation::Expire, State::Stopped) | (_, State::Deleting) => Ok(ReconcileAction::Cleanup),
        (_, State::Provisioning | State::Updating) => Ok(ReconcileAction::Observe),
        _ => Err(ReconcileError::NoAction),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ReconcileError {
    #[error("LW_ENVIRONMENT_PROVIDER_REGISTRY_INVALID")]
    InvalidProviderRegistry,
    #[error("LW_ENVIRONMENT_PROVIDER_UNAVAILABLE")]
    ProviderUnavailable,
    #[error("LW_ENVIRONMENT_PROVIDER_TIMEOUT_INVALID")]
    InvalidTimeout,
    #[error("LW_ENVIRONMENT_PROVIDER_TIMEOUT")]
    ProviderTimeout,
    #[error("LW_ENVIRONMENT_OPERATION_TERMINAL")]
    OperationTerminal,
    #[error("LW_ENVIRONMENT_RECONCILE_ACTION_INVALID")]
    NoAction,
    #[error("{0:?}")]
    Provider(ProviderFailure),
}

impl ReconcileError {
    #[must_use]
    pub const fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::InvalidProviderRegistry => "LW_ENVIRONMENT_PROVIDER_REGISTRY_INVALID",
            Self::ProviderUnavailable => "LW_ENVIRONMENT_PROVIDER_UNAVAILABLE",
            Self::InvalidTimeout => "LW_ENVIRONMENT_PROVIDER_TIMEOUT_INVALID",
            Self::ProviderTimeout => "LW_ENVIRONMENT_PROVIDER_TIMEOUT",
            Self::OperationTerminal => "LW_ENVIRONMENT_OPERATION_TERMINAL",
            Self::NoAction => "LW_ENVIRONMENT_RECONCILE_ACTION_INVALID",
            Self::Provider(failure) => failure.code.diagnostic_code(),
        }
    }

    #[must_use]
    pub const fn retryable(&self) -> bool {
        match self {
            Self::ProviderUnavailable | Self::ProviderTimeout => true,
            Self::Provider(failure) => failure.retryable,
            Self::InvalidProviderRegistry
            | Self::InvalidTimeout
            | Self::OperationTerminal
            | Self::NoAction => false,
        }
    }
}

fn add_duration(
    timestamp: UtcTimestamp,
    duration: Duration,
) -> Result<UtcTimestamp, ReconcileWorkerError> {
    let duration =
        time::Duration::try_from(duration).map_err(|_| ReconcileWorkerError::ClockOverflow)?;
    let value = timestamp
        .get()
        .checked_add(duration)
        .ok_or(ReconcileWorkerError::ClockOverflow)?;
    UtcTimestamp::from_utc(value).map_err(|_| ReconcileWorkerError::ClockOverflow)
}

const fn is_millisecond_duration(duration: Duration) -> bool {
    duration.subsec_nanos().is_multiple_of(1_000_000)
}

#[derive(Debug, thiserror::Error)]
pub enum ReconcileWorkerError {
    #[error("LW_ENVIRONMENT_RECONCILE_CONFIGURATION_INVALID")]
    InvalidConfiguration,
    #[error("LW_ENVIRONMENT_RECONCILE_CLOCK_OVERFLOW")]
    ClockOverflow,
    #[error("LW_ENVIRONMENT_RECONCILE_STORE_FAILED: {0}")]
    Store(#[from] EnvironmentStoreError),
    #[error("LW_ENVIRONMENT_RECONCILE_LIFECYCLE_FAILED: {0}")]
    Lifecycle(#[from] crate::LifecycleError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use contracts::authoring::{EnvironmentClass, RuntimeKind};
    use contracts::environment::{
        DesiredEnvironmentState, EnvironmentOperation, EnvironmentOperationKind,
        ObservedEnvironmentState, OperationState,
    };
    use contracts::{
        ActorId, CourseId, EnvironmentId, OperationId, ReleaseId, Revision, UtcTimestamp};
    use std::str::FromStr;

    fn timestamp(value: &str) -> UtcTimestamp {
        UtcTimestamp::from_str(value).unwrap_or_else(|error| {
            eprintln!("test timestamp must be valid: {error}");
            std::process::abort();
        })
    }

    fn provisioning_instance() -> EnvironmentInstance {
        let accepted_at = timestamp("2026-07-22T00:00:00.000Z");
        EnvironmentInstance {
            id: EnvironmentId::new(),
            display_label: "reconcile test".to_owned(),
            course_id: CourseId::new(),
            owner_id: ActorId::new(),
            class: EnvironmentClass::Experiment,
            runtime_kind: RuntimeKind::Container,
            release_id: ReleaseId::new(),
            release_version: 1,
            lease_id: None,
            capacity_binding: None,
            provider_binding: "container-primary-v1".to_owned(),
            desired_state: DesiredEnvironmentState::Running,
            observed_state: ObservedEnvironmentState::Provisioning,
            revision: Revision::new(2).unwrap_or_else(|error| {
                eprintln!("revision must be valid: {error}");
                std::process::abort();
            }),
            generation: 1,
            observed_generation: 0,
            operation: EnvironmentOperation {
                id: OperationId::new(),
                kind: EnvironmentOperationKind::Create,
                state: OperationState::Running,
                accepted_revision: Revision::new(1).unwrap_or_else(|error| {
                    eprintln!("revision must be valid: {error}");
                    std::process::abort();
                }),
                attempt: 1,
                provider_step: 2,
                max_attempts: 3,
                next_attempt_at: accepted_at,
                actor_id: ActorId::new(),
                trace_id: "trace-reconcile-test".to_owned(),
                accepted_at,
                deadline_at: timestamp("2026-07-22T00:10:00.000Z"),
                cleanup_started_at: None,
                diagnostic_code: None,
                preserve_mutable_disk: false,
                access_revocation_revision: None,
                retry_from_phase: None,
                reset_target: None,
                lease_authorization: None,
            },
            eligibility_expires_at: timestamp("2026-07-23T00:00:00.000Z"),
            endpoints: Vec::new(),
            last_diagnostic_code: None,
            failed_phase: None,
            cleanup_evidence: None,
        }
    }

    #[test]
    fn non_terminal_observation_is_deferred_until_retry_delay() {
        let current = provisioning_instance();
        let now = timestamp("2026-07-22T00:01:00.000Z");
        let deferred =
            ReconcileWorker::defer_non_terminal_observation(&current, now, Duration::from_secs(1))
                .unwrap_or_else(|error| {
                    eprintln!("valid deferred observation: {error}");
                    std::process::abort();
                });

        assert_eq!(
            deferred.operation.next_attempt_at,
            timestamp("2026-07-22T00:01:01.000Z")
        );
        assert_eq!(
            deferred.operation.provider_step,
            current.operation.provider_step
        );
        assert_eq!(deferred.revision, current.revision);
    }
}
