//! Explicit Provider selection and bounded timeout coverage.

mod support;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use contracts::environment::{
    EnvironmentInstance, EnvironmentOperationKind, ObservedEnvironmentState,
};
use contracts::{ActorId, OperationId};
use environment_service::{
    EnvironmentProvider, LifecycleCommand, ProviderFailure, ProviderObservation, ProviderRegistry,
    ReconcileAction, ReconcileError, Reconciler, apply_provider_failure, next_action, plan_command,
};

use support::{ready_instance, requested_instance, revision, timestamp};

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
    ) -> Result<ProviderObservation, ProviderFailure> {
        tokio::time::sleep(self.delay).await;
        Ok(ProviderObservation {
            next_state: contracts::environment::ObservedEnvironmentState::Ready,
            endpoints: instance.endpoints.clone(),
            cleanup_evidence: None,
            operation_complete: true,
        })
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
    let reconciler = Reconciler::new(exact, Duration::from_secs(1))?;
    let observation = reconciler
        .execute_once(&instance, timestamp("2026-07-14T01:00:01.000Z"))
        .await?;
    assert!(observation.operation_complete);

    let mut wrong = ProviderRegistry::default();
    wrong.register(Arc::new(FakeProvider {
        binding: "different-provider-v1",
        delay: Duration::ZERO,
    }))?;
    let reconciler = Reconciler::new(wrong, Duration::from_secs(1))?;
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
    let reconciler = Reconciler::new(registry, Duration::from_millis(1))?;
    assert!(matches!(
        reconciler
            .execute_once(&planned_restart()?, timestamp("2026-07-14T01:00:01.000Z"))
            .await,
        Err(ReconcileError::ProviderTimeout)
    ));
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
