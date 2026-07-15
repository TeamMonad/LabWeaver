//! Lifecycle command, retry, and failure regression coverage.
#![allow(clippy::panic)]

mod support;

use contracts::authoring::EnvironmentClass;
use contracts::environment::{
    DesiredEnvironmentState, EndpointHealth, EnvironmentOperationKind, ObservedEnvironmentState,
    OperationState,
};
use contracts::{ActorId, ArtifactId, ArtifactRef, LeaseId, OperationId, Sha256Digest};
use environment_service::{
    LifecycleCommand, LifecycleError, ProviderObservation, apply_provider_failure,
    apply_provider_observation, apply_retry, begin_timeout_cleanup, plan_command,
};

use support::{ready_instance, requested_instance, revision, timestamp};

fn command(
    instance: &contracts::environment::EnvironmentInstance,
    kind: EnvironmentOperationKind,
) -> LifecycleCommand {
    LifecycleCommand {
        environment_id: instance.id,
        kind,
        expected_revision: instance.revision,
        actor_id: ActorId::new(),
        trace_id: "trace-command-0001".to_owned(),
        accepted_at: timestamp("2026-07-14T01:00:00.000Z"),
        deadline_at: timestamp("2026-07-14T01:10:00.000Z"),
        access_revocation_revision: (!matches!(
            kind,
            EnvironmentOperationKind::Create | EnvironmentOperationKind::Start
        ))
        .then(|| revision(9)),
        preserve_mutable_disk: kind == EnvironmentOperationKind::Restart,
        max_attempts: 3,
    }
}

#[test]
fn stop_is_accepted_without_claiming_provider_convergence() -> Result<(), Box<dyn std::error::Error>>
{
    let current = ready_instance();
    let operation_id = OperationId::new();
    let planned = plan_command(
        &current,
        &command(&current, EnvironmentOperationKind::Stop),
        operation_id,
    )?;
    assert_eq!(planned.desired_state, DesiredEnvironmentState::Stopped);
    assert_eq!(planned.observed_state, ObservedEnvironmentState::Stopping);
    assert_eq!(planned.generation, current.generation + 1);
    assert_eq!(planned.operation.state, OperationState::Accepted);

    let stopped = apply_provider_observation(
        &planned,
        operation_id,
        ProviderObservation {
            next_state: ObservedEnvironmentState::Stopped,
            endpoints: Vec::new(),
            cleanup_evidence: None,
            operation_complete: true,
        },
    )?;
    assert_eq!(stopped.observed_state, ObservedEnvironmentState::Stopped);
    assert_eq!(stopped.generation, planned.generation);
    assert_eq!(stopped.observed_generation, stopped.generation);
    Ok(())
}

#[test]
fn start_and_recover_converge_through_validated_provider_states()
-> Result<(), Box<dyn std::error::Error>> {
    let current = ready_instance();
    let stopped_plan = plan_command(
        &current,
        &command(&current, EnvironmentOperationKind::Stop),
        OperationId::new(),
    )?;
    let stopped = apply_provider_observation(
        &stopped_plan,
        stopped_plan.operation.id,
        ProviderObservation {
            next_state: ObservedEnvironmentState::Stopped,
            endpoints: Vec::new(),
            cleanup_evidence: None,
            operation_complete: true,
        },
    )?;
    let started_plan = plan_command(
        &stopped,
        &command(&stopped, EnvironmentOperationKind::Start),
        OperationId::new(),
    )?;
    assert_eq!(
        started_plan.observed_state,
        ObservedEnvironmentState::Stopped
    );
    let start_provisioning = apply_provider_observation(
        &started_plan,
        started_plan.operation.id,
        ProviderObservation {
            next_state: ObservedEnvironmentState::Provisioning,
            endpoints: Vec::new(),
            cleanup_evidence: None,
            operation_complete: false,
        },
    )?;
    let mut started_endpoints = current.endpoints.clone();
    for endpoint in &mut started_endpoints {
        endpoint.revision = revision(start_provisioning.revision.get() + 1);
    }
    let started = apply_provider_observation(
        &start_provisioning,
        start_provisioning.operation.id,
        ProviderObservation {
            next_state: ObservedEnvironmentState::Ready,
            endpoints: started_endpoints,
            cleanup_evidence: None,
            operation_complete: true,
        },
    )?;
    assert_eq!(started.operation.state, OperationState::Succeeded);

    let requested = requested_instance();
    let failed = apply_provider_failure(
        &requested,
        requested.operation.id,
        "LW_ENVIRONMENT_PROVIDER_REJECTED",
    )?;
    let recovered_plan = plan_command(
        &failed,
        &command(&failed, EnvironmentOperationKind::Recover),
        OperationId::new(),
    )?;
    let building = apply_provider_observation(
        &recovered_plan,
        recovered_plan.operation.id,
        ProviderObservation {
            next_state: ObservedEnvironmentState::Building,
            endpoints: Vec::new(),
            cleanup_evidence: None,
            operation_complete: false,
        },
    )?;
    let provisioning = apply_provider_observation(
        &building,
        building.operation.id,
        ProviderObservation {
            next_state: ObservedEnvironmentState::Provisioning,
            endpoints: Vec::new(),
            cleanup_evidence: None,
            operation_complete: false,
        },
    )?;
    let mut recovered_endpoints = current.endpoints;
    for endpoint in &mut recovered_endpoints {
        endpoint.revision = revision(provisioning.revision.get() + 1);
    }
    let recovered = apply_provider_observation(
        &provisioning,
        provisioning.operation.id,
        ProviderObservation {
            next_state: ObservedEnvironmentState::Ready,
            endpoints: recovered_endpoints,
            cleanup_evidence: None,
            operation_complete: true,
        },
    )?;
    assert_eq!(recovered.observed_state, ObservedEnvironmentState::Ready);
    assert_eq!(recovered.operation.state, OperationState::Succeeded);
    Ok(())
}

#[test]
fn provider_cannot_complete_an_intermediate_or_wrong_steady_state()
-> Result<(), Box<dyn std::error::Error>> {
    let requested = requested_instance();
    assert!(matches!(
        apply_provider_observation(
            &requested,
            requested.operation.id,
            ProviderObservation {
                next_state: ObservedEnvironmentState::Validating,
                endpoints: Vec::new(),
                cleanup_evidence: None,
                operation_complete: true,
            },
        ),
        Err(LifecycleError::ProviderObservationInvalid)
    ));

    let current = ready_instance();
    let operation_id = OperationId::new();
    let planned = plan_command(
        &current,
        &command(&current, EnvironmentOperationKind::Restart),
        operation_id,
    )?;
    assert!(matches!(
        apply_provider_observation(
            &planned,
            operation_id,
            ProviderObservation {
                next_state: ObservedEnvironmentState::Stopped,
                endpoints: Vec::new(),
                cleanup_evidence: None,
                operation_complete: false,
            },
        ),
        Err(LifecycleError::ProviderObservationInvalid)
    ));

    let cleanup = begin_timeout_cleanup(
        &planned,
        timestamp("2026-07-14T01:10:01.000Z"),
        timestamp("2026-07-14T01:20:01.000Z"),
    )?;
    assert!(matches!(
        apply_provider_observation(
            &cleanup,
            operation_id,
            ProviderObservation {
                next_state: ObservedEnvironmentState::Deleted,
                endpoints: Vec::new(),
                cleanup_evidence: None,
                operation_complete: true,
            },
        ),
        Err(LifecycleError::ProviderObservationInvalid)
    ));
    Ok(())
}

#[test]
fn expired_provider_call_is_fenced_into_cleanup() -> Result<(), Box<dyn std::error::Error>> {
    let current = ready_instance();
    let planned = plan_command(
        &current,
        &command(&current, EnvironmentOperationKind::Restart),
        OperationId::new(),
    )?;
    let cleanup = begin_timeout_cleanup(
        &planned,
        timestamp("2026-07-14T01:10:01.000Z"),
        timestamp("2026-07-14T01:20:01.000Z"),
    )?;
    assert_eq!(cleanup.desired_state, DesiredEnvironmentState::Deleted);
    assert_eq!(cleanup.observed_state, ObservedEnvironmentState::Deleting);
    assert_eq!(cleanup.operation.state, OperationState::Cancelling);
    assert_eq!(
        cleanup.last_diagnostic_code.as_deref(),
        Some("LW_ENVIRONMENT_PROVIDER_TIMEOUT")
    );
    let deleted = apply_provider_observation(
        &cleanup,
        cleanup.operation.id,
        ProviderObservation {
            next_state: ObservedEnvironmentState::Deleted,
            endpoints: Vec::new(),
            cleanup_evidence: Some(cleanup_evidence()),
            operation_complete: true,
        },
    )?;
    assert_eq!(deleted.observed_state, ObservedEnvironmentState::Deleted);
    assert_eq!(deleted.operation.state, OperationState::Failed);
    assert_eq!(
        deleted.operation.diagnostic_code.as_deref(),
        Some("LW_ENVIRONMENT_PROVIDER_TIMEOUT")
    );
    assert_eq!(
        deleted.last_diagnostic_code.as_deref(),
        Some("LW_ENVIRONMENT_PROVIDER_TIMEOUT")
    );
    Ok(())
}

#[test]
fn expire_stop_checkpoint_is_followed_by_cleanup() -> Result<(), Box<dyn std::error::Error>> {
    let current = ready_instance();
    let planned = plan_command(
        &current,
        &command(&current, EnvironmentOperationKind::Expire),
        OperationId::new(),
    )?;
    let stopped = apply_provider_observation(
        &planned,
        planned.operation.id,
        ProviderObservation {
            next_state: ObservedEnvironmentState::Stopped,
            endpoints: Vec::new(),
            cleanup_evidence: None,
            operation_complete: false,
        },
    )?;
    assert_eq!(stopped.observed_state, ObservedEnvironmentState::Stopped);
    assert_eq!(stopped.operation.state, OperationState::Running);
    let deleting = apply_provider_observation(
        &stopped,
        stopped.operation.id,
        ProviderObservation {
            next_state: ObservedEnvironmentState::Deleting,
            endpoints: Vec::new(),
            cleanup_evidence: None,
            operation_complete: false,
        },
    )?;
    let deleted = apply_provider_observation(
        &deleting,
        deleting.operation.id,
        ProviderObservation {
            next_state: ObservedEnvironmentState::Deleted,
            endpoints: Vec::new(),
            cleanup_evidence: Some(cleanup_evidence()),
            operation_complete: true,
        },
    )?;
    assert_eq!(deleted.observed_state, ObservedEnvironmentState::Deleted);
    assert_eq!(deleted.operation.state, OperationState::Succeeded);
    Ok(())
}

#[test]
fn timeout_cleanup_requires_recorded_revocation_when_endpoints_existed() {
    let current = ready_instance();
    let mut restart = command(&current, EnvironmentOperationKind::Restart);
    restart.access_revocation_revision = None;
    assert!(matches!(
        plan_command(&current, &restart, OperationId::new()),
        Err(LifecycleError::Contract(
            contracts::environment::EnvironmentError::GrantRevocationRequired
        ))
    ));
}

#[test]
fn freeze_reobserves_endpoints_without_changing_generation()
-> Result<(), Box<dyn std::error::Error>> {
    let current = ready_instance();
    let operation_id = OperationId::new();
    let planned = plan_command(
        &current,
        &command(&current, EnvironmentOperationKind::Freeze),
        operation_id,
    )?;
    assert_eq!(planned.observed_state, ObservedEnvironmentState::Updating);
    assert_eq!(planned.generation, current.generation);
    assert!(
        planned
            .endpoints
            .iter()
            .all(|endpoint| endpoint.health == EndpointHealth::Unhealthy)
    );
    let mut endpoints = planned.endpoints.clone();
    for endpoint in &mut endpoints {
        endpoint.health = EndpointHealth::Healthy;
        endpoint.revision = revision(planned.revision.get() + 1);
    }
    let frozen = apply_provider_observation(
        &planned,
        operation_id,
        ProviderObservation {
            next_state: ObservedEnvironmentState::Ready,
            endpoints,
            cleanup_evidence: None,
            operation_complete: true,
        },
    )?;
    assert_eq!(frozen.generation, current.generation);
    assert_eq!(frozen.observed_state, ObservedEnvironmentState::Ready);
    Ok(())
}

#[test]
fn work_requires_a_resource_lease_and_experiment_rejects_one() {
    let mut work = ready_instance();
    work.class = EnvironmentClass::Work;
    assert!(matches!(
        work.validate(),
        Err(contracts::environment::EnvironmentError::LeaseRequired)
    ));

    let mut experiment = ready_instance();
    experiment.lease_id = Some(LeaseId::new());
    assert!(matches!(
        experiment.validate(),
        Err(contracts::environment::EnvironmentError::InvalidAggregate)
    ));
}

#[test]
fn delete_requires_revocation_and_immediately_removes_endpoint_eligibility() {
    let current = ready_instance();
    let mut missing_revocation = command(&current, EnvironmentOperationKind::Delete);
    missing_revocation.access_revocation_revision = None;
    let missing = plan_command(&current, &missing_revocation, OperationId::new());
    assert!(matches!(
        missing,
        Err(LifecycleError::Contract(
            contracts::environment::EnvironmentError::GrantRevocationRequired
        ))
    ));

    let mut delete = command(&current, EnvironmentOperationKind::Delete);
    delete.access_revocation_revision = Some(revision(9));
    let planned = plan_command(&current, &delete, OperationId::new())
        .unwrap_or_else(|error| panic!("delete plan failed: {error}"));
    assert_eq!(planned.desired_state, DesiredEnvironmentState::Deleted);
    assert_eq!(planned.observed_state, ObservedEnvironmentState::Deleting);
    assert!(
        planned
            .endpoints
            .iter()
            .all(|endpoint| endpoint.health == EndpointHealth::Removed)
    );
}

#[test]
fn stale_revision_and_overlapping_non_destructive_command_are_rejected() {
    let current = ready_instance();
    let mut stale = command(&current, EnvironmentOperationKind::Stop);
    stale.expected_revision = revision(1);
    assert!(matches!(
        plan_command(&current, &stale, OperationId::new()),
        Err(LifecycleError::RevisionConflict)
    ));

    let first = plan_command(
        &current,
        &command(&current, EnvironmentOperationKind::Stop),
        OperationId::new(),
    )
    .unwrap_or_else(|error| panic!("first plan failed: {error}"));
    assert!(matches!(
        plan_command(
            &first,
            &command(&first, EnvironmentOperationKind::Restart),
            OperationId::new()
        ),
        Err(LifecycleError::OperationActive)
    ));
}

#[test]
fn retry_is_bounded_and_terminal_failure_never_keeps_a_healthy_endpoint()
-> Result<(), Box<dyn std::error::Error>> {
    let current = ready_instance();
    let operation_id = OperationId::new();
    let planned = plan_command(
        &current,
        &command(&current, EnvironmentOperationKind::Restart),
        operation_id,
    )?;
    let retried = apply_retry(
        &planned,
        operation_id,
        "LW_ENVIRONMENT_PROVIDER_TRANSIENT",
        timestamp("2026-07-14T01:00:01.000Z"),
    )?;
    assert_eq!(retried.operation.attempt, 2);
    let failed = apply_provider_failure(
        &retried,
        operation_id,
        "LW_ENVIRONMENT_PROVIDER_RETRY_EXHAUSTED",
    )?;
    assert_eq!(failed.observed_state, ObservedEnvironmentState::Failed);
    assert_eq!(failed.operation.state, OperationState::Failed);
    assert!(
        failed
            .endpoints
            .iter()
            .all(|endpoint| endpoint.health != EndpointHealth::Healthy)
    );
    Ok(())
}

fn cleanup_evidence() -> ArtifactRef {
    ArtifactRef {
        artifact_id: ArtifactId::new(),
        store_binding: "environment-cleanup-evidence-v1".to_owned(),
        object_version: "version-0001".to_owned(),
        sha256: Sha256Digest::of_bytes(b"cleanup evidence"),
        size_bytes: 16,
        media_type: "application/json".to_owned(),
    }
}
