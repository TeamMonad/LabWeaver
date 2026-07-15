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
    ReconcileAction, ReconcileError, Reconciler, next_action, plan_command,
};

use support::{ready_instance, revision, timestamp};

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
