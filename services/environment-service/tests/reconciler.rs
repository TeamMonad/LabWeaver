//! Explicit Provider selection and bounded timeout coverage.

#![allow(clippy::expect_used)]

mod support;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use contracts::environment::{
    ActivateEnvironmentResourceReservationRequest, ActivateEnvironmentResourceReservationResponse,
    EnvironmentInstance, EnvironmentOperationKind, EnvironmentResourceReservationState,
    ObservedEnvironmentState, ReleaseEnvironmentResourceReservationRequest,
    ResolveEnvironmentResourceReservationRequest, SuspendEnvironmentResourceReservationRequest,
    SuspendEnvironmentResourceReservationResponse,
};
use contracts::resource::GpuAllocation;
use contracts::{ActorId, OperationId};
use environment_service::{
    EnvironmentProvider, LifecycleCommand, ProviderFailure, ProviderObservation, ProviderRegistry,
    ReconcileAction, ReconcileError, Reconciler, apply_provider_failure, next_action, plan_command,
};

use support::{ready_instance, requested_instance, revision, timestamp};

#[derive(Clone)]
struct RecordingAllocator {
    events: Arc<std::sync::Mutex<Vec<&'static str>>>,
}

fn test_allocator() -> RecordingAllocator {
    RecordingAllocator {
        events: Arc::new(std::sync::Mutex::new(Vec::new())),
    }
}

#[async_trait]
impl environment_service::ExperimentResourceAllocator for RecordingAllocator {
    async fn resolve_resource_reservation(
        &self,
        _request: &ResolveEnvironmentResourceReservationRequest,
    ) -> Result<Option<GpuAllocation>, environment_service::ResourceUsageClientError> {
        Err(environment_service::ResourceUsageClientError::Rejected { retryable: false })
    }

    async fn release_resource_reservation(
        &self,
        _request: &ReleaseEnvironmentResourceReservationRequest,
    ) -> Result<bool, environment_service::ResourceUsageClientError> {
        Ok(true)
    }

    async fn activate_resource_reservation(
        &self,
        request: &ActivateEnvironmentResourceReservationRequest,
    ) -> Result<
        ActivateEnvironmentResourceReservationResponse,
        environment_service::ResourceUsageClientError,
    > {
        self.events.lock().expect("event mutex").push("activate");
        Ok(ActivateEnvironmentResourceReservationResponse {
            version: 1,
            environment_id: request.environment_id,
            state: EnvironmentResourceReservationState::Reserved,
            reservation_generation: 2,
            environment_generation: request.environment_generation,
            allocation: request.expected_allocation.clone(),
            applied: true,
        })
    }

    async fn suspend_resource_reservation(
        &self,
        request: &SuspendEnvironmentResourceReservationRequest,
    ) -> Result<
        SuspendEnvironmentResourceReservationResponse,
        environment_service::ResourceUsageClientError,
    > {
        self.events.lock().expect("event mutex").push("suspend");
        Ok(SuspendEnvironmentResourceReservationResponse {
            version: 1,
            environment_id: request.environment_id,
            state: EnvironmentResourceReservationState::Suspended,
            reservation_generation: 3,
            environment_generation: request.environment_generation,
            allocation: None,
            applied: true,
        })
    }
}

struct RecordingProvider {
    events: Arc<std::sync::Mutex<Vec<&'static str>>>,
}

#[async_trait]
impl EnvironmentProvider for RecordingProvider {
    fn binding(&self) -> &'static str {
        "container-primary-v1"
    }

    async fn execute(
        &self,
        action: ReconcileAction,
        instance: &EnvironmentInstance,
    ) -> Result<environment_service::ProviderOutcome<ProviderObservation>, ProviderFailure> {
        self.events.lock().expect("event mutex").push(match action {
            ReconcileAction::Start | ReconcileAction::Restart => "provider_start",
            ReconcileAction::Stop => "provider_stop",
            _ => "provider_other",
        });
        let next_state = if action == ReconcileAction::Stop {
            ObservedEnvironmentState::Stopped
        } else {
            ObservedEnvironmentState::Ready
        };
        Ok(environment_service::ProviderOutcome::Completed(
            ProviderObservation {
                next_state,
                endpoints: instance.endpoints.clone(),
                cleanup_evidence: None,
                operation_complete: true,
            },
        ))
    }
}

struct FakeProvider {
    binding: &'static str,
    delay: Duration,
}

#[async_trait]
impl EnvironmentProvider for FakeProvider {
    fn binding(&self) -> &str {
        self.binding
    }

    async fn execute(
        &self,
        _action: ReconcileAction,
        instance: &EnvironmentInstance,
    ) -> Result<environment_service::ProviderOutcome<ProviderObservation>, ProviderFailure> {
        async {
            tokio::time::sleep(self.delay).await;
            Ok(ProviderObservation {
                next_state: contracts::environment::ObservedEnvironmentState::Ready,
                endpoints: instance.endpoints.clone(),
                cleanup_evidence: None,
                operation_complete: true,
            })
        }
        .await
        .map(environment_service::ProviderOutcome::Completed)
    }
}

fn planned_restart() -> Result<EnvironmentInstance, Box<dyn std::error::Error>> {
    let current = ready_instance();
    Ok(plan_command(
        &current,
        &LifecycleCommand {
            environment_id: current.id,
            kind: EnvironmentOperationKind::Restart,
            expected_revision: current.revision,
            actor_id: ActorId::new(),
            trace_id: "trace-restart-0001".to_owned(),
            accepted_at: timestamp("2026-07-14T01:00:00.000Z"),
            deadline_at: timestamp("2026-07-14T01:10:00.000Z"),
            access_revocation_revision: Some(revision(8)),
            preserve_mutable_disk: true,
            max_attempts: 3,
            reset_target: None,
        },
        OperationId::new(),
    )?)
}

#[tokio::test]
async fn exact_binding_executes_and_missing_binding_never_falls_back()
-> Result<(), Box<dyn std::error::Error>> {
    let instance = planned_restart()?;
    let mut exact = ProviderRegistry::default();
    exact.register(Arc::new(FakeProvider {
        binding: "container-primary-v1",
        delay: Duration::ZERO,
    }))?;
    let reconciler =
        Reconciler::new(exact, Duration::from_secs(1))?.with_resource_allocator(test_allocator());
    let observation = reconciler
        .execute_once(&instance, timestamp("2026-07-14T01:00:01.000Z"))
        .await?
        .completed()
        .ok_or("provider did not complete")?;
    assert!(observation.operation_complete);

    let mut wrong = ProviderRegistry::default();
    wrong.register(Arc::new(FakeProvider {
        binding: "different-provider-v1",
        delay: Duration::ZERO,
    }))?;
    let reconciler =
        Reconciler::new(wrong, Duration::from_secs(1))?.with_resource_allocator(test_allocator());
    assert!(matches!(
        reconciler
            .execute_once(&instance, timestamp("2026-07-14T01:00:01.000Z"))
            .await,
        Err(ReconcileError::ProviderUnavailable)
    ));
    Ok(())
}

#[tokio::test]
async fn duplicate_binding_and_provider_timeout_fail_closed()
-> Result<(), Box<dyn std::error::Error>> {
    let mut registry = ProviderRegistry::default();
    registry.register(Arc::new(FakeProvider {
        binding: "container-primary-v1",
        delay: Duration::from_millis(50),
    }))?;
    assert!(matches!(
        registry.register(Arc::new(FakeProvider {
            binding: "container-primary-v1",
            delay: Duration::ZERO,
        })),
        Err(ReconcileError::InvalidProviderRegistry)
    ));
    let reconciler = Reconciler::new(registry, Duration::from_millis(1))?
        .with_resource_allocator(test_allocator());
    assert!(matches!(
        reconciler
            .execute_once(&planned_restart()?, timestamp("2026-07-14T01:00:01.000Z"))
            .await,
        Err(ReconcileError::ProviderTimeout)
    ));
    Ok(())
}

#[tokio::test]
async fn experiment_resource_fence_wraps_provider_start_and_stop_in_order()
-> Result<(), Box<dyn std::error::Error>> {
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut registry = ProviderRegistry::default();
    registry.register(Arc::new(RecordingProvider {
        events: Arc::clone(&events),
    }))?;
    let reconciler = Reconciler::new(registry, Duration::from_secs(1))?.with_resource_allocator(
        RecordingAllocator {
            events: Arc::clone(&events),
        },
    );

    let mut start = ready_instance();
    start.observed_state = ObservedEnvironmentState::Stopped;
    start.operation.kind = EnvironmentOperationKind::Start;
    start.operation.state = contracts::environment::OperationState::Accepted;
    start.generation = 2;
    start.operation.id = OperationId::new();
    reconciler
        .execute_once(&start, timestamp("2026-07-14T01:00:01.000Z"))
        .await?;
    assert_eq!(
        *events.lock().expect("event mutex"),
        vec!["activate", "provider_start"]
    );

    events.lock().expect("event mutex").clear();
    let mut stop = ready_instance();
    stop.observed_state = ObservedEnvironmentState::Stopping;
    stop.desired_state = contracts::environment::DesiredEnvironmentState::Stopped;
    stop.operation.kind = EnvironmentOperationKind::Stop;
    stop.operation.state = contracts::environment::OperationState::Accepted;
    stop.generation = 3;
    stop.operation.id = OperationId::new();
    reconciler
        .execute_once(&stop, timestamp("2026-07-14T01:00:01.000Z"))
        .await?;
    assert_eq!(
        *events.lock().expect("event mutex"),
        vec!["provider_stop", "suspend"]
    );
    Ok(())
}

#[tokio::test]
async fn experiment_reset_and_recovered_provision_require_resource_activation()
-> Result<(), Box<dyn std::error::Error>> {
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut registry = ProviderRegistry::default();
    registry.register(Arc::new(RecordingProvider {
        events: Arc::clone(&events),
    }))?;
    let mut instance = ready_instance();
    instance.desired_state = contracts::environment::DesiredEnvironmentState::Stopped;
    instance.observed_state = ObservedEnvironmentState::Provisioning;
    instance.operation.kind = EnvironmentOperationKind::Reset;
    instance.operation.state = contracts::environment::OperationState::Accepted;
    instance.operation.id = OperationId::new();
    instance.generation = 2;

    let without_resource = Reconciler::new(registry, Duration::from_secs(1))?;
    assert!(matches!(
        without_resource
            .execute_once(&instance, timestamp("2026-07-14T01:00:01.000Z"))
            .await,
        Err(ReconcileError::ResourceUnavailable)
    ));

    let mut registry = ProviderRegistry::default();
    registry.register(Arc::new(RecordingProvider {
        events: Arc::clone(&events),
    }))?;
    let reconciler = Reconciler::new(registry, Duration::from_secs(1))?.with_resource_allocator(
        RecordingAllocator {
            events: Arc::clone(&events),
        },
    );
    reconciler
        .execute_once(&instance, timestamp("2026-07-14T01:00:01.000Z"))
        .await?;
    assert_eq!(
        *events.lock().expect("event mutex"),
        vec!["activate", "provider_other"]
    );

    events.lock().expect("event mutex").clear();
    instance.operation.kind = EnvironmentOperationKind::Retry;
    instance.operation.id = OperationId::new();
    reconciler
        .execute_once(&instance, timestamp("2026-07-14T01:00:01.000Z"))
        .await?;
    assert_eq!(
        *events.lock().expect("event mutex"),
        vec!["activate", "provider_other"]
    );

    events.lock().expect("event mutex").clear();
    instance.operation.kind = EnvironmentOperationKind::Create;
    instance.desired_state = contracts::environment::DesiredEnvironmentState::Running;
    instance.operation.id = OperationId::new();
    reconciler
        .execute_once(&instance, timestamp("2026-07-14T01:00:01.000Z"))
        .await?;
    assert_eq!(
        *events.lock().expect("event mutex"),
        vec!["activate", "provider_other"]
    );
    Ok(())
}

#[test]
fn reset_build_and_expire_cleanup_have_durable_next_actions()
-> Result<(), Box<dyn std::error::Error>> {
    let current = ready_instance();
    let mut reset = plan_command(
        &current,
        &LifecycleCommand {
            environment_id: current.id,
            kind: EnvironmentOperationKind::Reset,
            expected_revision: current.revision,
            actor_id: ActorId::new(),
            trace_id: "trace-reset-0001".to_owned(),
            accepted_at: timestamp("2026-07-14T01:00:00.000Z"),
            deadline_at: timestamp("2026-07-14T01:10:00.000Z"),
            access_revocation_revision: Some(revision(8)),
            preserve_mutable_disk: false,
            max_attempts: 3,
            reset_target: Some(
                contracts::environment::EnvironmentResetTarget::ExperimentBaseline {
                    release_id: current.release_id,
                    release_version: current.release_version,
                },
            ),
        },
        OperationId::new(),
    )?;
    reset.observed_state = ObservedEnvironmentState::Building;
    assert_eq!(
        next_action(&reset, timestamp("2026-07-14T01:00:01.000Z"))?,
        ReconcileAction::Build
    );

    let mut expire = plan_command(
        &current,
        &LifecycleCommand {
            environment_id: current.id,
            kind: EnvironmentOperationKind::Expire,
            expected_revision: current.revision,
            actor_id: ActorId::new(),
            trace_id: "trace-expire-0001".to_owned(),
            accepted_at: timestamp("2026-07-14T01:00:00.000Z"),
            deadline_at: timestamp("2026-07-14T01:10:00.000Z"),
            access_revocation_revision: Some(revision(8)),
            preserve_mutable_disk: false,
            max_attempts: 3,
            reset_target: None,
        },
        OperationId::new(),
    )?;
    expire.observed_state = ObservedEnvironmentState::Stopped;
    assert_eq!(
        next_action(&expire, timestamp("2026-07-14T01:00:01.000Z"))?,
        ReconcileAction::Cleanup
    );
    reset.observed_state = ObservedEnvironmentState::Stopping;
    reset.desired_state = contracts::environment::DesiredEnvironmentState::Stopped;
    assert_eq!(
        next_action(&reset, timestamp("2026-07-14T01:00:01.000Z"))?,
        ReconcileAction::Stop
    );
    Ok(())
}

#[test]
fn retry_and_recover_resume_the_persisted_failed_phase() -> Result<(), Box<dyn std::error::Error>> {
    use contracts::environment::DesiredEnvironmentState;

    for (failed_phase, resumed_phase, action) in [
        (
            ObservedEnvironmentState::Validating,
            ObservedEnvironmentState::Validating,
            ReconcileAction::Validate,
        ),
        (
            ObservedEnvironmentState::Building,
            ObservedEnvironmentState::Building,
            ReconcileAction::Build,
        ),
        (
            ObservedEnvironmentState::Provisioning,
            ObservedEnvironmentState::Provisioning,
            ReconcileAction::Provision,
        ),
        (
            ObservedEnvironmentState::Stopped,
            ObservedEnvironmentState::Provisioning,
            ReconcileAction::Start,
        ),
        (
            ObservedEnvironmentState::Stopping,
            ObservedEnvironmentState::Stopping,
            ReconcileAction::Stop,
        ),
        (
            ObservedEnvironmentState::Updating,
            ObservedEnvironmentState::Updating,
            ReconcileAction::Configure,
        ),
        (
            ObservedEnvironmentState::Expiring,
            ObservedEnvironmentState::Expiring,
            ReconcileAction::Stop,
        ),
        (
            ObservedEnvironmentState::Deleting,
            ObservedEnvironmentState::Deleting,
            ReconcileAction::Cleanup,
        ),
    ] {
        let mut active = requested_instance();
        active.observed_state = failed_phase;
        if matches!(
            failed_phase,
            ObservedEnvironmentState::Expiring | ObservedEnvironmentState::Deleting
        ) {
            active.desired_state = DesiredEnvironmentState::Deleted;
        }
        let failed = apply_provider_failure(
            &active,
            active.operation.id,
            "LW_ENVIRONMENT_PROVIDER_REJECTED",
        )?;
        for kind in [
            EnvironmentOperationKind::Retry,
            EnvironmentOperationKind::Recover,
        ] {
            let planned = plan_command(
                &failed,
                &LifecycleCommand {
                    environment_id: failed.id,
                    kind,
                    expected_revision: failed.revision,
                    actor_id: ActorId::new(),
                    trace_id: format!("trace-resume-{failed_phase:?}-{kind:?}"),
                    accepted_at: timestamp("2026-07-14T01:00:00.000Z"),
                    deadline_at: timestamp("2026-07-14T01:10:00.000Z"),
                    access_revocation_revision: None,
                    preserve_mutable_disk: false,
                    max_attempts: 3,
                    reset_target: None,
                },
                OperationId::new(),
            )?;
            assert_eq!(planned.operation.retry_from_phase, Some(failed_phase));
            assert_eq!(planned.observed_state, resumed_phase);
            assert_eq!(
                next_action(&planned, timestamp("2026-07-14T01:00:01.000Z"))?,
                action
            );
        }
    }
    Ok(())
}
