//! Real `PostgreSQL` coverage for idempotency, Outbox atomicity, and reconciler leases.
#![allow(
    clippy::too_many_lines,
    reason = "one container lifecycle keeps migration and lease-race evidence in a shared database"
)]

mod support;

use std::collections::HashSet;
use std::time::Duration;
use std::{
    sync::Arc,
    sync::Mutex,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use async_trait::async_trait;
use contracts::environment::{
    EndpointHealth, EndpointProtocol, EnvironmentEndpoint, EnvironmentOperationKind,
    ObservedEnvironmentState, OperationState,
};
use contracts::events::{CloudEvent, EVENT_CONTRACTS, ReleaseWithdrawn, SPEC_VERSION, subjects};
use contracts::supply_chain::{VirtualMachineBaseDisk, VirtualMachineDiskFormat};
use contracts::{
    ActorId, ArtifactId, ArtifactRef, CourseId, EndpointId, EnvironmentId, EventId, OperationId,
    ReleaseId, Revision, Sequence, Sha256Digest, UtcTimestamp,
};
use environment_service::{
    CONTAINER_BACKEND_PROTOCOL_VERSION, ContainerApplyObservation, ContainerBackendFence,
    ContainerExecutorBackend, ContainerExecutorFenceError, ContainerExecutorRequest,
    ContainerExecutorRequestEnvelope, ContainerExecutorResponse, ContainerResourcePlan,
    EnvironmentEventPublisher, EnvironmentProvider, EnvironmentStoreError, FencedContainerExecutor,
    FencedKubeVirtExecutor, InboundCommandDecision, InboundLifecycleCommand,
    KUBEVIRT_BACKEND_PROTOCOL_VERSION, KubeVirtBackendFence, KubeVirtCleanupPlan,
    KubeVirtExecutorBackend, KubeVirtExecutorFenceError, KubeVirtExecutorRequest,
    KubeVirtExecutorRequestEnvelope, KubeVirtExecutorResponse, KubeVirtObservationStore,
    KubeVirtObservationStoreError, KubeVirtResourcePlan, KubeVirtRunningObservation,
    KubeVirtStoppedObservation, LifecycleCommand, LifecycleError, OutboxDispatchError,
    OutboxDispatchOutcome, OutboxDispatcher, PgContainerExecutorFenceStore, PgEnvironmentStore,
    PgKubeVirtExecutorFenceStore, PgKubeVirtObservationStore, PgReleaseProjectionStore,
    ProviderFailure, ProviderFailureCode, ProviderObservation, ProviderRegistry, PublishFailure,
    ReconcileAction, ReconcileWorker, ReconcileWorkerOutcome, Reconciler,
    ReleaseProjectionDecision, apply_provider_observation,
};
use sqlx::postgres::PgPoolOptions;
use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;

use support::{requested_instance, timestamp};

#[tokio::test]
async fn durable_command_and_lease_path_is_atomic_and_recoverable()
-> Result<(), Box<dyn std::error::Error>> {
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let url = format!(
        "postgres://postgres:postgres@127.0.0.1:{}/postgres",
        container.get_host_port_ipv4(5432).await?
    );
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await?;
    let baseline = format!(
        "CREATE SCHEMA environment; SET search_path TO environment;\n{}",
        include_str!("../../../migrations/environment/0001_sprint2_baseline.sql")
    );
    sqlx::raw_sql(&baseline).execute(&pool).await?;

    let store = PgEnvironmentStore::new(pool.clone());

    let mut invalid_ready_create = support::ready_instance();
    invalid_ready_create.operation.state = OperationState::Accepted;
    assert!(invalid_ready_create.validate().is_ok());
    assert!(matches!(
        store
            .create("create-key-invalid-ready", &invalid_ready_create)
            .await,
        Err(EnvironmentStoreError::InvalidCreateAggregate)
    ));
    let invalid_ready_count: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint FROM environment.environment_instances WHERE environment_id=$1",
    )
    .bind(invalid_ready_create.id.as_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(invalid_ready_count, 0);

    let instance = requested_instance();
    let accepted = store.create("create-key-0001", &instance).await?;
    assert_eq!(accepted.environment_id, instance.id);
    assert_eq!(
        serde_json::to_value(&accepted)?["environmentId"],
        instance.id.to_string()
    );
    let replay = store.create("create-key-0001", &instance).await?;
    assert_eq!(accepted, replay);

    sqlx::query(
        "UPDATE environment.environment_instances \
         SET created_at='2026-07-24T00:00:00.123456Z'::timestamptz, \
             updated_at='2026-07-24T00:00:01.654321Z'::timestamptz \
         WHERE environment_id=$1",
    )
    .bind(instance.id.as_uuid())
    .execute(&pool)
    .await?;
    let (inventory, _) = store
        .list_owned(instance.course_id, instance.owner_id, 100)
        .await?;
    let listed = inventory
        .iter()
        .find(|record| record.instance.id == instance.id)
        .ok_or("expected the created environment in owned inventory")?;
    assert_eq!(listed.created_at.to_string(), "2026-07-24T00:00:00.123Z");
    assert_eq!(listed.updated_at.to_string(), "2026-07-24T00:00:01.654Z");

    let mut conflicting = instance.clone();
    conflicting.release_version += 1;
    assert!(matches!(
        store.create("create-key-0001", &conflicting).await,
        Err(EnvironmentStoreError::IdempotencyConflict)
    ));
    let outbox_count: i64 =
        sqlx::query_scalar("SELECT count(*)::bigint FROM environment.outbox_events")
            .fetch_one(&pool)
            .await?;
    assert_eq!(outbox_count, 1);
    let envelope: serde_json::Value =
        sqlx::query_scalar("SELECT payload FROM environment.outbox_events WHERE aggregate_id=$1")
            .bind(instance.id.as_uuid())
            .fetch_one(&pool)
            .await?;
    assert_eq!(envelope["specversion"], "1.0");
    assert_eq!(envelope["courseId"], instance.course_id.to_string());
    assert_eq!(envelope["traceId"], instance.operation.trace_id);
    assert_eq!(envelope["data"]["environmentId"], instance.id.to_string());

    let publisher = RecordingPublisher::fail_first();
    let dispatcher =
        OutboxDispatcher::new(pool.clone(), publisher.clone(), Duration::from_secs(2))?;
    assert!(matches!(
        dispatcher.dispatch_once().await,
        Err(OutboxDispatchError::Publish(PublishFailure::Unavailable))
    ));
    let published_at: Option<time::OffsetDateTime> = sqlx::query_scalar(
        "SELECT published_at FROM environment.outbox_events WHERE aggregate_id=$1",
    )
    .bind(instance.id.as_uuid())
    .fetch_one(&pool)
    .await?;
    assert!(published_at.is_none());
    let dispatch_outcome = dispatcher.dispatch_once().await?;
    assert!(matches!(
        dispatch_outcome,
        OutboxDispatchOutcome::Published { .. }
    ));
    let deliveries = publisher.deliveries()?;
    assert_eq!(deliveries.len(), 2);
    assert_eq!(deliveries[0], deliveries[1]);
    let published_at: Option<time::OffsetDateTime> = sqlx::query_scalar(
        "SELECT published_at FROM environment.outbox_events WHERE aggregate_id=$1",
    )
    .bind(instance.id.as_uuid())
    .fetch_one(&pool)
    .await?;
    assert!(published_at.is_some());

    let lease = store
        .claim_due("environment-worker-a", Duration::from_secs(30))
        .await?
        .ok_or("expected a due create operation")?;
    assert!(
        store
            .claim_due("environment-worker-b", Duration::from_secs(30))
            .await?
            .is_none()
    );
    assert!(
        store
            .claim_due("environment-worker-a", Duration::from_secs(30))
            .await?
            .is_none()
    );
    store.heartbeat(&lease, Duration::from_secs(30)).await?;

    let validating = apply_provider_observation(
        &lease.instance,
        lease.instance.operation.id,
        ProviderObservation {
            next_state: ObservedEnvironmentState::Validating,
            endpoints: Vec::new(),
            cleanup_evidence: None,
            operation_complete: false,
        },
    )?;
    store.save_reconciled(&lease, &validating).await?;
    let renewed = store
        .claim_due("environment-worker-a", Duration::from_secs(30))
        .await?
        .ok_or("expected the next reconcile step")?;
    assert!(matches!(
        store.heartbeat(&lease, Duration::from_secs(30)).await,
        Err(EnvironmentStoreError::LeaseLost)
    ));
    store
        .heartbeat(&renewed, Duration::from_millis(1_500))
        .await?;
    let loaded = store.load(instance.id).await?;
    assert_eq!(loaded.revision, validating.revision);
    assert_eq!(loaded.observed_state, ObservedEnvironmentState::Validating);

    let outbox_count: i64 =
        sqlx::query_scalar("SELECT count(*)::bigint FROM environment.outbox_events")
            .fetch_one(&pool)
            .await?;
    assert_eq!(outbox_count, 2);
    let expired = store
        .find_expired(timestamp("2026-07-16T00:00:00.000Z"), 10)
        .await?;
    assert_eq!(expired.len(), 1);

    let superseded = requested_instance();
    store.create("create-key-superseded", &superseded).await?;
    let old_lease = store
        .claim_due("environment-worker-old", Duration::from_secs(30))
        .await?
        .ok_or("expected the operation that will be superseded")?;
    let accepted_at_value: time::OffsetDateTime =
        sqlx::query_scalar("SELECT date_trunc('milliseconds', clock_timestamp())")
            .fetch_one(&pool)
            .await?;
    let accepted_at = UtcTimestamp::from_utc(accepted_at_value)?;
    let deadline_at = UtcTimestamp::from_utc(
        accepted_at_value
            .checked_add(time::Duration::minutes(10))
            .ok_or("deadline overflow")?,
    )?;
    let destructive = LifecycleCommand {
        environment_id: superseded.id,
        kind: EnvironmentOperationKind::Delete,
        expected_revision: superseded.revision,
        actor_id: ActorId::new(),
        trace_id: "trace-delete-superseded".to_owned(),
        accepted_at,
        deadline_at,
        access_revocation_revision: Some(support::revision(9)),
        preserve_mutable_disk: false,
        max_attempts: 3,
        reset_target: None,
    };
    let cleanup = store
        .accept_command("delete-key-superseded", &destructive)
        .await?;
    let (old_state, old_lease_expires_at, old_token_present): (
        String,
        Option<time::OffsetDateTime>,
        bool,
    ) = sqlx::query_as(
        "SELECT state, lease_expires_at, lease_token IS NOT NULL \
         FROM environment.environment_operations WHERE operation_id=$1",
    )
    .bind(old_lease.instance.operation.id.as_uuid())
    .fetch_one(&pool)
    .await?;
    let cleanup_next_attempt_at: time::OffsetDateTime = sqlx::query_scalar(
        "SELECT next_attempt_at FROM environment.environment_operations WHERE operation_id=$1",
    )
    .bind(cleanup.operation_id.as_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(old_state, "cancelled");
    assert!(old_token_present);
    assert!(cleanup_next_attempt_at >= old_lease_expires_at.ok_or("old lease disappeared")?);
    assert!(
        store
            .claim_due("environment-worker-cleanup", Duration::from_secs(30))
            .await?
            .is_none()
    );

    let inbox_target = requested_instance();
    store.create("create-key-inbox", &inbox_target).await?;
    let inbound = InboundLifecycleCommand {
        consumer: "environment-lifecycle-v1".to_owned(),
        event_id: EventId::new(),
        course_id: inbox_target.course_id,
        aggregate_revision: inbox_target.revision,
        aggregate_sequence: Sequence(1),
        idempotency_key: "delete-key-inbox".to_owned(),
        command: LifecycleCommand {
            environment_id: inbox_target.id,
            kind: EnvironmentOperationKind::Delete,
            expected_revision: inbox_target.revision,
            actor_id: ActorId::new(),
            trace_id: "trace-delete-inbox".to_owned(),
            accepted_at,
            deadline_at,
            access_revocation_revision: Some(support::revision(10)),
            preserve_mutable_disk: false,
            max_attempts: 3,
            reset_target: None,
        },
        create: None,
        lease_authorization: None,
    };
    assert!(matches!(
        store.accept_inbound_command(&inbound).await?,
        InboundCommandDecision::Applied(_)
    ));
    let applied = store.load(inbox_target.id).await?;
    assert_eq!(applied.revision, support::revision(2));
    assert_eq!(
        store.accept_inbound_command(&inbound).await?,
        InboundCommandDecision::Duplicate
    );

    let mut conflicting_event = inbound.clone();
    conflicting_event.command.trace_id = "trace-delete-inbox-conflict".to_owned();
    assert!(matches!(
        store.accept_inbound_command(&conflicting_event).await,
        Err(EnvironmentStoreError::Persistence(_))
    ));
    let mut stale_event = inbound.clone();
    stale_event.event_id = EventId::new();
    assert_eq!(
        store.accept_inbound_command(&stale_event).await?,
        InboundCommandDecision::Stale
    );
    let mut gap_event = inbound;
    gap_event.event_id = EventId::new();
    gap_event.aggregate_sequence = Sequence(3);
    assert_eq!(
        store.accept_inbound_command(&gap_event).await?,
        InboundCommandDecision::Gap
    );
    assert_eq!(
        store.load(inbox_target.id).await?.revision,
        applied.revision
    );
    let inbox_count: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint FROM environment.inbox_events \
         WHERE consumer='environment-lifecycle-v1'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(inbox_count, 1);

    let idempotency_target = requested_instance();
    store
        .create("create-key-command-identity", &idempotency_target)
        .await?;
    let identity_command = LifecycleCommand {
        environment_id: idempotency_target.id,
        kind: EnvironmentOperationKind::Delete,
        expected_revision: idempotency_target.revision,
        actor_id: ActorId::new(),
        trace_id: "trace-delete-command-identity".to_owned(),
        accepted_at,
        deadline_at,
        access_revocation_revision: Some(support::revision(12)),
        preserve_mutable_disk: false,
        max_attempts: 3,
        reset_target: None,
    };
    store
        .accept_command("delete-key-command-identity", &identity_command)
        .await?;
    let mut changed_deadline = identity_command.clone();
    changed_deadline.deadline_at = UtcTimestamp::from_utc(
        deadline_at
            .get()
            .checked_add(time::Duration::minutes(1))
            .ok_or("deadline overflow")?,
    )?;
    assert!(matches!(
        store
            .accept_command("delete-key-command-identity", &changed_deadline)
            .await,
        Err(EnvironmentStoreError::IdempotencyConflict)
    ));
    let mut changed_retry_limit = identity_command;
    changed_retry_limit.max_attempts = 4;
    assert!(matches!(
        store
            .accept_command("delete-key-command-identity", &changed_retry_limit)
            .await,
        Err(EnvironmentStoreError::IdempotencyConflict)
    ));
    assert_eq!(
        store.load(idempotency_target.id).await?.revision,
        support::revision(2)
    );

    let mut registry = ProviderRegistry::default();
    registry.register(Arc::new(CleanupFailureProvider))?;
    let worker = ReconcileWorker::new(
        store.clone(),
        Reconciler::new(registry, Duration::from_secs(1))?,
        Duration::from_secs(2),
        Duration::from_secs(1),
    )?;
    assert!(matches!(
        worker
            .run_once("environment-worker-cleanup-failure", accepted_at)
            .await?,
        ReconcileWorkerOutcome::Failed {
            diagnostic_code: "LW_ENVIRONMENT_PROVIDER_CLEANUP_FAILED"
        }
    ));
    let cleanup_failed = store.load(inbox_target.id).await?;
    assert_eq!(
        cleanup_failed.observed_state,
        ObservedEnvironmentState::Failed
    );
    assert!(cleanup_failed.endpoints.is_empty());
    assert_eq!(
        cleanup_failed.last_diagnostic_code.as_deref(),
        Some("LW_ENVIRONMENT_PROVIDER_CLEANUP_FAILED")
    );
    assert_eq!(
        cleanup_failed.failed_phase,
        Some(ObservedEnvironmentState::Deleting)
    );
    let persisted_failed_phase: Option<String> = sqlx::query_scalar(
        "SELECT failed_phase FROM environment.environment_instances WHERE environment_id=$1",
    )
    .bind(cleanup_failed.id.as_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(persisted_failed_phase.as_deref(), Some("deleting"));
    assert!(matches!(
        worker
            .run_once("environment-worker-command-identity-cleanup", accepted_at)
            .await?,
        ReconcileWorkerOutcome::Failed {
            diagnostic_code: "LW_ENVIRONMENT_PROVIDER_CLEANUP_FAILED"
        }
    ));
    assert_eq!(
        store.load(idempotency_target.id).await?.observed_state,
        ObservedEnvironmentState::Failed
    );

    let mut crash_target = requested_instance();
    crash_target.eligibility_expires_at = timestamp("2027-07-15T00:00:00.000Z");
    store
        .create("create-key-crash-recovery", &crash_target)
        .await?;
    let abandoned = store
        .claim_due("environment-worker-crashed", Duration::from_millis(20))
        .await?
        .ok_or("expected an operation for crash recovery")?;
    assert_eq!(abandoned.instance.id, crash_target.id);
    let crash_provider = Arc::new(IdempotentCrashProvider::default());
    crash_provider
        .execute(ReconcileAction::Validate, &abandoned.instance)
        .await
        .map_err(|_| "provider side effect simulation failed")?;
    let mut crash_registry = ProviderRegistry::default();
    crash_registry.register(crash_provider.clone())?;
    let restarted_worker = ReconcileWorker::new(
        store.clone(),
        Reconciler::new(crash_registry, Duration::from_millis(100))?,
        Duration::from_millis(1_100),
        Duration::from_millis(100),
    )?;
    let recovered_outcome = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let outcome = restarted_worker
                .run_once(
                    "environment-worker-restarted",
                    timestamp("2026-07-14T00:01:00.000Z"),
                )
                .await?;
            if outcome == ReconcileWorkerOutcome::Idle {
                tokio::task::yield_now().await;
            } else {
                break Ok::<_, environment_service::ReconcileWorkerError>(outcome);
            }
        }
    })
    .await??;
    assert!(matches!(
        recovered_outcome,
        ReconcileWorkerOutcome::Advanced {
            state: ObservedEnvironmentState::Validating,
            ..
        }
    ));
    assert_eq!(crash_provider.calls.load(Ordering::SeqCst), 2);
    assert_eq!(crash_provider.side_effects.load(Ordering::SeqCst), 1);
    assert_eq!(
        store.load(crash_target.id).await?.observed_state,
        ObservedEnvironmentState::Validating
    );
    assert!(matches!(
        store.heartbeat(&abandoned, Duration::from_secs(30)).await,
        Err(EnvironmentStoreError::LeaseLost)
    ));
    for expected_state in [
        ObservedEnvironmentState::Building,
        ObservedEnvironmentState::Provisioning,
        ObservedEnvironmentState::Ready,
    ] {
        assert!(matches!(
            restarted_worker
                .run_once(
                    "environment-worker-restarted",
                    timestamp("2026-07-14T00:01:00.000Z"),
                )
                .await?,
            ReconcileWorkerOutcome::Advanced { state, .. } if state == expected_state
        ));
    }
    let completed_create = store.load(crash_target.id).await?;
    assert_eq!(completed_create.operation.provider_step, 4);
    assert_eq!(crash_provider.calls.load(Ordering::SeqCst), 5);
    assert_eq!(crash_provider.side_effects.load(Ordering::SeqCst), 4);
    let persisted_provider_step: i64 = sqlx::query_scalar(
        "SELECT provider_step FROM environment.environment_operations WHERE operation_id=$1",
    )
    .bind(completed_create.operation.id.as_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(persisted_provider_step, 4);
    let reset_target = contracts::environment::EnvironmentResetTarget::ExperimentBaseline {
        release_id: completed_create.release_id,
        release_version: completed_create.release_version,
    };
    store
        .accept_command(
            "reset-key-persisted-target",
            &LifecycleCommand {
                environment_id: completed_create.id,
                kind: EnvironmentOperationKind::Reset,
                expected_revision: completed_create.revision,
                actor_id: ActorId::new(),
                trace_id: "trace-reset-persisted-target".to_owned(),
                accepted_at,
                deadline_at,
                access_revocation_revision: Some(support::revision(22)),
                preserve_mutable_disk: false,
                max_attempts: 3,
                reset_target: Some(reset_target.clone()),
            },
        )
        .await?;
    assert_eq!(
        store
            .load(completed_create.id)
            .await?
            .operation
            .reset_target,
        Some(reset_target)
    );

    let race_target = requested_instance();
    store
        .create("create-key-optimistic-race", &race_target)
        .await?;
    let delete_command = LifecycleCommand {
        environment_id: race_target.id,
        kind: EnvironmentOperationKind::Delete,
        expected_revision: race_target.revision,
        actor_id: ActorId::new(),
        trace_id: "trace-delete-race".to_owned(),
        accepted_at,
        deadline_at,
        access_revocation_revision: Some(support::revision(11)),
        preserve_mutable_disk: false,
        max_attempts: 3,
        reset_target: None,
    };
    let mut cancel_command = delete_command.clone();
    cancel_command.kind = EnvironmentOperationKind::Cancel;
    cancel_command.trace_id = "trace-cancel-race".to_owned();
    let (delete_result, cancel_result) = tokio::join!(
        store.accept_command("delete-key-race", &delete_command),
        store.accept_command("cancel-key-race", &cancel_command)
    );
    let results = [delete_result, cancel_result];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(
                result,
                Err(EnvironmentStoreError::Lifecycle(
                    LifecycleError::RevisionConflict
                ))
            ))
            .count(),
        1
    );
    assert_eq!(
        store.load(race_target.id).await?.revision,
        support::revision(2)
    );
    Ok(())
}

#[tokio::test]
async fn persistent_timeout_and_ready_cancel_cleanup_are_bounded()
-> Result<(), Box<dyn std::error::Error>> {
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let url = format!(
        "postgres://postgres:postgres@127.0.0.1:{}/postgres",
        container.get_host_port_ipv4(5432).await?
    );
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await?;
    let migrations = format!(
        "CREATE SCHEMA environment; SET search_path TO environment;\n{}",
        include_str!("../../../migrations/environment/0001_sprint2_baseline.sql")
    );
    sqlx::raw_sql(&migrations).execute(&pool).await?;
    let store = PgEnvironmentStore::new(pool);

    let timed_out = requested_instance();
    store
        .create("create-key-timeout-worker", &timed_out)
        .await?;
    let timeout_now = timestamp("2026-07-14T00:10:01.000Z");
    let worker = success_worker(store.clone())?;
    assert!(matches!(
        worker
            .run_once("environment-worker-timeout", timeout_now)
            .await?,
        ReconcileWorkerOutcome::Advanced {
            state: ObservedEnvironmentState::Deleting,
            terminal: false
        }
    ));
    assert!(matches!(
        worker
            .run_once(
                "environment-worker-timeout-cleanup",
                timestamp("2026-07-14T00:10:02.000Z")
            )
            .await?,
        ReconcileWorkerOutcome::Advanced {
            state: ObservedEnvironmentState::Deleted,
            terminal: true
        }
    ));
    let timeout_deleted = store.load(timed_out.id).await?;
    assert_eq!(timeout_deleted.operation.state, OperationState::Failed);
    assert_eq!(
        timeout_deleted.last_diagnostic_code.as_deref(),
        Some("LW_ENVIRONMENT_PROVIDER_TIMEOUT")
    );
    assert!(timeout_deleted.endpoints.is_empty());
    assert!(timeout_deleted.cleanup_evidence.is_some());

    let ready_target = requested_instance();
    store
        .create("create-key-ready-cancel", &ready_target)
        .await?;
    for index in 0..4 {
        assert!(matches!(
            worker
                .run_once(
                    &format!("environment-worker-converge-{index}"),
                    timestamp("2026-07-14T00:01:00.000Z")
                )
                .await?,
            ReconcileWorkerOutcome::Advanced { .. }
        ));
    }
    let ready = store.load(ready_target.id).await?;
    assert_eq!(ready.observed_state, ObservedEnvironmentState::Ready);
    assert!(
        ready
            .endpoints
            .iter()
            .all(|endpoint| endpoint.health == EndpointHealth::Healthy)
    );
    let accepted_at = timestamp("2026-07-14T00:02:00.000Z");
    store
        .accept_command(
            "cancel-key-ready-environment",
            &LifecycleCommand {
                environment_id: ready.id,
                kind: EnvironmentOperationKind::Cancel,
                expected_revision: ready.revision,
                actor_id: ActorId::new(),
                trace_id: "trace-cancel-ready-environment".to_owned(),
                accepted_at,
                deadline_at: timestamp("2026-07-14T00:07:00.000Z"),
                access_revocation_revision: Some(support::revision(20)),
                preserve_mutable_disk: false,
                max_attempts: 3,
                reset_target: None,
            },
        )
        .await?;
    assert!(matches!(
        worker
            .run_once("environment-worker-cancel-cleanup", accepted_at)
            .await?,
        ReconcileWorkerOutcome::Advanced {
            state: ObservedEnvironmentState::Deleted,
            terminal: true
        }
    ));
    let cancelled = store.load(ready.id).await?;
    assert_eq!(cancelled.operation.state, OperationState::Cancelled);
    assert_eq!(cancelled.observed_state, ObservedEnvironmentState::Deleted);
    assert!(
        cancelled
            .endpoints
            .iter()
            .all(|endpoint| endpoint.health != EndpointHealth::Healthy)
    );
    assert!(cancelled.cleanup_evidence.is_some());
    Ok(())
}

#[tokio::test]
async fn release_withdrawal_is_projected_in_aggregate_order()
-> Result<(), Box<dyn std::error::Error>> {
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let url = format!(
        "postgres://postgres:postgres@127.0.0.1:{}/postgres",
        container.get_host_port_ipv4(5432).await?
    );
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await?;
    let migrations = format!(
        "CREATE SCHEMA environment; SET search_path TO environment;\n{}",
        include_str!("../../../migrations/environment/0001_sprint2_baseline.sql")
    );
    sqlx::raw_sql(&migrations).execute(&pool).await?;

    let consumer = "environment-release-v1";
    let release_id = ReleaseId::new();
    let course_id = CourseId::new();
    let publication_event_id = EventId::new();
    sqlx::query(
        "INSERT INTO environment.inbox_events \
         (consumer,event_id,aggregate_id,aggregate_sequence,payload_sha256) VALUES ($1,$2,$3,1,$4)",
    )
    .bind(consumer)
    .bind(publication_event_id.as_uuid())
    .bind(release_id.as_uuid())
    .bind(Sha256Digest::of_bytes(b"publication").to_string())
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO environment.inbox_watermarks (consumer,aggregate_id,last_sequence) VALUES ($1,$2,1)",
    )
    .bind(consumer)
    .bind(release_id.as_uuid())
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO environment.release_projections \
         (release_id,course_id,release_version,provider_binding,projection_sha256,contract,projected_event_id) \
         VALUES ($1,$2,1,'container-primary-v1',$3,'{}'::jsonb,$4)",
    )
    .bind(release_id.as_uuid())
    .bind(course_id.as_uuid())
    .bind(Sha256Digest::of_bytes(b"projection").to_string())
    .bind(publication_event_id.as_uuid())
    .execute(&pool)
    .await?;

    let withdrawn_at = timestamp("2026-07-16T09:00:00.000Z");
    let contract = EVENT_CONTRACTS
        .iter()
        .copied()
        .find(|contract| contract.subject == subjects::ENVIRONMENT_TEMPLATE_RELEASE_WITHDRAWN)
        .ok_or("withdrawal contract missing")?;
    let event = CloudEvent {
        specversion: SPEC_VERSION.to_owned(),
        id: EventId::new(),
        source: contract.source().to_owned(),
        event_type: contract.event_type.to_owned(),
        subject: contract.subject.to_owned(),
        time: withdrawn_at,
        datacontenttype: "application/json".to_owned(),
        dataschema: contract.data_schema(),
        course_id,
        aggregate_revision: Revision::new(1)?,
        aggregate_sequence: Sequence(2),
        trace_id: "release-withdrawal-test".to_owned(),
        data: ReleaseWithdrawn {
            release_id,
            version: 1,
            actor_id: ActorId::new(),
            reason_code: "SECURITY_REVOKED".to_owned(),
            withdrawn_at,
        },
    };
    let store = PgReleaseProjectionStore::new(pool.clone());
    assert_eq!(
        store.accept_withdrawal(consumer, &event).await?,
        ReleaseProjectionDecision::Applied
    );
    assert_eq!(
        store.accept_withdrawal(consumer, &event).await?,
        ReleaseProjectionDecision::Duplicate
    );
    let (sequence, persisted_withdrawn_at, reason): (i64, time::OffsetDateTime, String) =
        sqlx::query_as(
            "SELECT aggregate_sequence,withdrawn_at,withdrawal_reason_code \
             FROM environment.release_projections WHERE release_id=$1",
        )
        .bind(release_id.as_uuid())
        .fetch_one(&pool)
        .await?;
    assert_eq!(sequence, 2);
    assert_eq!(
        UtcTimestamp::from_utc(persisted_withdrawn_at)?,
        withdrawn_at
    );
    assert_eq!(reason, "SECURITY_REVOKED");

    let missing_release_id = ReleaseId::new();
    let mut gap_event = event;
    gap_event.id = EventId::new();
    gap_event.data.release_id = missing_release_id;
    assert_eq!(
        store.accept_withdrawal(consumer, &gap_event).await?,
        ReleaseProjectionDecision::Gap
    );
    Ok(())
}

#[derive(Clone)]
struct CountingContainerExecutor {
    calls: Arc<AtomicUsize>,
    observed_at: UtcTimestamp,
}

#[async_trait]
impl ContainerExecutorBackend for CountingContainerExecutor {
    async fn execute(
        &self,
        _fence: &ContainerBackendFence,
        request: &ContainerExecutorRequest,
    ) -> ContainerExecutorResponse {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match request {
            ContainerExecutorRequest::DeleteNamespace { plan } => {
                ContainerExecutorResponse::Deleted {
                    plan_sha256: plan.plan_sha256,
                    cleanup_evidence: ArtifactRef {
                        artifact_id: ArtifactId::new(),
                        store_binding: "environment-cleanup-evidence-v1".to_owned(),
                        object_version: plan.plan_sha256.to_string(),
                        sha256: Sha256Digest::of_bytes(plan.namespace.as_bytes()),
                        size_bytes: 1,
                        media_type: "application/json".to_owned(),
                    },
                }
            }
            request => ContainerExecutorResponse::Observed {
                plan_sha256: container_request_plan(request).plan_sha256,
                observation: ContainerApplyObservation {
                    ready: true,
                    observed_at: self.observed_at,
                },
            },
        }
    }
}

#[tokio::test]
async fn container_executor_persists_generation_and_permanent_delete_tombstone()
-> Result<(), Box<dyn std::error::Error>> {
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let url = format!(
        "postgres://postgres:postgres@127.0.0.1:{}/postgres",
        container.get_host_port_ipv4(5432).await?
    );
    let pool = PgPoolOptions::new()
        .max_connections(3)
        .connect(&url)
        .await?;
    let migrations = format!(
        "CREATE SCHEMA environment; SET search_path TO environment;\n{}",
        include_str!("../../../migrations/environment/0001_sprint2_baseline.sql")
    );
    sqlx::raw_sql(&migrations).execute(&pool).await?;
    let authority_now = container_database_now(&pool).await?;
    let deadline = container_add_time(authority_now, time::Duration::minutes(1))?;
    let environment_id = EnvironmentId::new();
    let plan = ContainerResourcePlan {
        environment_id,
        namespace: format!("lw-env-{environment_id}"),
        image: format!("harbor.internal/course/image@sha256:{}", "a".repeat(64)),
        resources: Vec::new(),
        plan_sha256: Sha256Digest::of_bytes(b"container-plan"),
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let operation_one = OperationId::new();
    let first = container_executor_envelope(
        plan.clone(),
        operation_one,
        1,
        1,
        1,
        ReconcileAction::Provision,
        deadline,
    )?;
    FencedContainerExecutor::new(
        PgContainerExecutorFenceStore::new(pool.clone()),
        CountingContainerExecutor {
            calls: calls.clone(),
            observed_at: authority_now,
        },
    )
    .execute(first.clone())
    .await?;
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // Reconstructing the executor simulates restart; exact delivery replays the stored result.
    FencedContainerExecutor::new(
        PgContainerExecutorFenceStore::new(pool.clone()),
        CountingContainerExecutor {
            calls: calls.clone(),
            observed_at: authority_now,
        },
    )
    .execute(first)
    .await?;
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let operation_two = OperationId::new();
    let executor = FencedContainerExecutor::new(
        PgContainerExecutorFenceStore::new(pool.clone()),
        CountingContainerExecutor {
            calls: calls.clone(),
            observed_at: authority_now,
        },
    );
    executor
        .execute(container_executor_envelope(
            plan.clone(),
            operation_two,
            2,
            1,
            1,
            ReconcileAction::Provision,
            deadline,
        )?)
        .await?;
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    assert!(matches!(
        executor
            .execute(container_executor_envelope(
                plan.clone(),
                operation_one,
                1,
                2,
                1,
                ReconcileAction::Cleanup,
                deadline,
            )?)
            .await,
        Err(ContainerExecutorFenceError::StaleGeneration)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    executor
        .execute(container_executor_envelope(
            plan.clone(),
            operation_two,
            2,
            2,
            1,
            ReconcileAction::Cleanup,
            deadline,
        )?)
        .await?;
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert!(matches!(
        executor
            .execute(container_executor_envelope(
                plan,
                OperationId::new(),
                3,
                1,
                1,
                ReconcileAction::Provision,
                deadline,
            )?)
            .await,
        Err(ContainerExecutorFenceError::Tombstoned)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 3);

    let expired_environment_id = EnvironmentId::new();
    let expired_plan = ContainerResourcePlan {
        environment_id: expired_environment_id,
        namespace: format!("lw-env-{expired_environment_id}"),
        image: String::new(),
        resources: Vec::new(),
        plan_sha256: Sha256Digest::of_bytes(b"expired-plan"),
    };
    assert!(matches!(
        executor
            .execute(container_executor_envelope(
                expired_plan,
                OperationId::new(),
                1,
                1,
                1,
                ReconcileAction::Provision,
                container_add_time(
                    container_database_now(&pool).await?,
                    time::Duration::seconds(-1)
                )?,
            )?)
            .await,
        Err(ContainerExecutorFenceError::DeadlineExceeded)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    Ok(())
}

fn container_executor_envelope(
    plan: ContainerResourcePlan,
    operation_id: OperationId,
    operation_generation: u64,
    provider_step: u32,
    attempt: u32,
    action: ReconcileAction,
    deadline_at: UtcTimestamp,
) -> Result<ContainerExecutorRequestEnvelope, Box<dyn std::error::Error>> {
    let request = match action {
        ReconcileAction::Provision | ReconcileAction::Reset => {
            ContainerExecutorRequest::Apply { plan }
        }
        ReconcileAction::Cleanup => ContainerExecutorRequest::DeleteNamespace { plan },
        _ => return Err("unsupported executor fixture action".into()),
    };
    let request_id = Sha256Digest::of_canonical(&serde_json::json!({
        "protocolVersion": CONTAINER_BACKEND_PROTOCOL_VERSION,
        "environmentId": container_request_plan(&request).environment_id,
        "operationId": operation_id,
        "providerStep": provider_step,
        "operationGeneration": operation_generation,
        "attempt": attempt,
        "action": action,
        "deadlineAt": deadline_at,
        "request": &request,
    }))?;
    Ok(ContainerExecutorRequestEnvelope {
        fence: ContainerBackendFence {
            protocol_version: CONTAINER_BACKEND_PROTOCOL_VERSION,
            environment_id: container_request_plan(&request).environment_id,
            operation_id,
            provider_step,
            operation_generation,
            attempt,
            action,
            request_id,
            deadline_at,
        },
        request,
    })
}

const fn container_request_plan(request: &ContainerExecutorRequest) -> &ContainerResourcePlan {
    match request {
        ContainerExecutorRequest::Apply { plan }
        | ContainerExecutorRequest::Observe { plan }
        | ContainerExecutorRequest::Scale { plan, .. }
        | ContainerExecutorRequest::Restart { plan, .. }
        | ContainerExecutorRequest::DeleteNamespace { plan } => plan,
    }
}

async fn container_database_now(
    pool: &sqlx::PgPool,
) -> Result<UtcTimestamp, Box<dyn std::error::Error>> {
    let value: time::OffsetDateTime =
        sqlx::query_scalar("SELECT date_trunc('milliseconds',clock_timestamp())")
            .fetch_one(pool)
            .await?;
    Ok(UtcTimestamp::from_utc(value)?)
}

fn container_add_time(
    timestamp: UtcTimestamp,
    duration: time::Duration,
) -> Result<UtcTimestamp, Box<dyn std::error::Error>> {
    Ok(UtcTimestamp::from_utc(timestamp.get() + duration)?)
}

struct CountingKubeVirtExecutor {
    calls: Arc<AtomicUsize>,
    observed_at: UtcTimestamp,
}

#[async_trait]
impl KubeVirtExecutorBackend for CountingKubeVirtExecutor {
    async fn execute(
        &self,
        fence: &KubeVirtBackendFence,
        request: &KubeVirtExecutorRequest,
    ) -> KubeVirtExecutorResponse {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match request {
            KubeVirtExecutorRequest::DeleteNamespace { plan } => {
                KubeVirtExecutorResponse::Deleted {
                    plan_sha256: plan.plan_sha256,
                    cleanup_evidence: ArtifactRef {
                        artifact_id: ArtifactId::new(),
                        store_binding: "environment-cleanup-evidence-v1".to_owned(),
                        object_version: plan.plan_sha256.to_string(),
                        sha256: Sha256Digest::of_bytes(plan.namespace.as_bytes()),
                        size_bytes: 1,
                        media_type: "application/json".to_owned(),
                    },
                }
            }
            KubeVirtExecutorRequest::Apply { plan }
            | KubeVirtExecutorRequest::Observe { plan }
            | KubeVirtExecutorRequest::Start { plan }
            | KubeVirtExecutorRequest::Stop { plan }
            | KubeVirtExecutorRequest::Restart { plan } => KubeVirtExecutorResponse::Running {
                plan_sha256: plan.plan_sha256,
                observation: KubeVirtRunningObservation {
                    observed_environment_generation: fence.environment_generation,
                    vm_resource_generation: 1,
                    observed_vm_resource_generation: 1,
                    vm_uid: uuid::Uuid::new_v4(),
                    vmi_uid: uuid::Uuid::new_v4(),
                    root_disk_uid: uuid::Uuid::new_v4(),
                    guest_ip: std::net::IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 2)),
                    service_cluster_ip: std::net::IpAddr::V4(std::net::Ipv4Addr::new(10, 96, 0, 2)),
                    ssh_host_key_sha256: Sha256Digest::of_bytes(b"host-key"),
                    guest_agent_connected: true,
                    ssh_ready: true,
                    observed_at: self.observed_at,
                },
            },
        }
    }
}

#[tokio::test]
async fn kubevirt_executor_replays_and_permanently_tombstones_cleanup()
-> Result<(), Box<dyn std::error::Error>> {
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let url = format!(
        "postgres://postgres:postgres@127.0.0.1:{}/postgres",
        container.get_host_port_ipv4(5432).await?
    );
    let pool = PgPoolOptions::new()
        .max_connections(3)
        .connect(&url)
        .await?;
    sqlx::raw_sql(&format!(
        "CREATE SCHEMA environment; SET search_path TO environment;\n{}",
        include_str!("../../../migrations/environment/0001_sprint2_baseline.sql")
    ))
    .execute(&pool)
    .await?;
    let observed_at = container_database_now(&pool).await?;
    let deadline = container_add_time(observed_at, time::Duration::minutes(1))?;
    let environment_id = EnvironmentId::new();
    let plan = kubevirt_executor_plan(environment_id);
    let calls = Arc::new(AtomicUsize::new(0));
    let operation_id = OperationId::new();
    let first = kubevirt_executor_envelope(
        plan.clone(),
        operation_id,
        1,
        1,
        ReconcileAction::Provision,
        deadline,
    )?;
    for _ in 0..2 {
        FencedKubeVirtExecutor::new(
            PgKubeVirtExecutorFenceStore::new(pool.clone()),
            CountingKubeVirtExecutor {
                calls: Arc::clone(&calls),
                observed_at,
            },
        )
        .execute(first.clone())
        .await?;
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let executor = FencedKubeVirtExecutor::new(
        PgKubeVirtExecutorFenceStore::new(pool),
        CountingKubeVirtExecutor {
            calls: Arc::clone(&calls),
            observed_at,
        },
    );
    executor
        .execute(kubevirt_executor_envelope(
            plan.clone(),
            operation_id,
            1,
            2,
            ReconcileAction::Cleanup,
            deadline,
        )?)
        .await?;
    assert!(matches!(
        executor
            .execute(kubevirt_executor_envelope(
                plan,
                OperationId::new(),
                2,
                1,
                ReconcileAction::Provision,
                deadline,
            )?)
            .await,
        Err(KubeVirtExecutorFenceError::Tombstoned)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    Ok(())
}

fn kubevirt_executor_plan(environment_id: EnvironmentId) -> KubeVirtResourcePlan {
    KubeVirtResourcePlan {
        environment_id,
        namespace: format!("lw-env-{environment_id}"),
        virtual_machine_name: "runtime".to_owned(),
        data_volume_name: "rootdisk".to_owned(),
        base_disk: VirtualMachineBaseDisk {
            binding: "ubuntu-24.04-v1".to_owned(),
            source_registry_digest: concat!(
                "docker://quay.io/containerdisks/ubuntu@",
                "sha256:d28194a16351320fa9a093e18233033508a745566eb8ba3b309c32924bf155a5"
            )
            .to_owned(),
            disk_sha256: Sha256Digest::of_bytes(b"vm-base-disk"),
            capacity_bytes: 10_737_418_240,
        },
        base_disk_format: VirtualMachineDiskFormat::Qcow2,
        storage_class_name: "local-path".to_owned(),
        resources: Vec::new(),
        plan_sha256: Sha256Digest::of_bytes(b"vm-plan"),
    }
}

fn kubevirt_executor_envelope(
    plan: KubeVirtResourcePlan,
    operation_id: OperationId,
    generation: u64,
    provider_step: u32,
    action: ReconcileAction,
    deadline_at: UtcTimestamp,
) -> Result<KubeVirtExecutorRequestEnvelope, Box<dyn std::error::Error>> {
    let request = match action {
        ReconcileAction::Provision => KubeVirtExecutorRequest::Apply { plan },
        ReconcileAction::Cleanup => KubeVirtExecutorRequest::DeleteNamespace {
            plan: KubeVirtCleanupPlan {
                environment_id: plan.environment_id,
                namespace: plan.namespace,
                virtual_machine_name: plan.virtual_machine_name,
                plan_sha256: plan.plan_sha256,
            },
        },
        _ => return Err("unsupported executor fixture action".into()),
    };
    let request_id = Sha256Digest::of_canonical(&serde_json::json!({
        "protocolVersion": KUBEVIRT_BACKEND_PROTOCOL_VERSION,
        "environmentId": environment_id_for_kubevirt_request(&request),
        "operationId": operation_id,
        "providerStep": provider_step,
        "environmentGeneration": generation,
        "attempt": 1,
        "action": action,
        "deadlineAt": deadline_at,
        "request": &request,
    }))?;
    Ok(KubeVirtExecutorRequestEnvelope {
        fence: KubeVirtBackendFence {
            protocol_version: KUBEVIRT_BACKEND_PROTOCOL_VERSION,
            environment_id: environment_id_for_kubevirt_request(&request),
            operation_id,
            provider_step,
            environment_generation: generation,
            attempt: 1,
            action,
            request_id,
            deadline_at,
        },
        request,
    })
}

const fn environment_id_for_kubevirt_request(request: &KubeVirtExecutorRequest) -> EnvironmentId {
    match request {
        KubeVirtExecutorRequest::Apply { plan }
        | KubeVirtExecutorRequest::Observe { plan }
        | KubeVirtExecutorRequest::Start { plan }
        | KubeVirtExecutorRequest::Stop { plan }
        | KubeVirtExecutorRequest::Restart { plan } => plan.environment_id,
        KubeVirtExecutorRequest::DeleteNamespace { plan } => plan.environment_id,
    }
}

#[tokio::test]
async fn kubevirt_observation_identity_is_durable_fenced_and_tombstoned()
-> Result<(), Box<dyn std::error::Error>> {
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let url = format!(
        "postgres://postgres:postgres@127.0.0.1:{}/postgres",
        container.get_host_port_ipv4(5432).await?
    );
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await?;
    let migration = format!(
        "CREATE SCHEMA environment; SET search_path TO environment;\n{}",
        include_str!("../../../migrations/environment/0001_sprint2_baseline.sql")
    );
    sqlx::raw_sql(&migration).execute(&pool).await?;
    let store = PgKubeVirtObservationStore::new(pool.clone());
    let environment_id = EnvironmentId::new();
    let plan = KubeVirtResourcePlan {
        environment_id,
        namespace: format!("lw-env-{environment_id}"),
        virtual_machine_name: "runtime".to_owned(),
        data_volume_name: "rootdisk".to_owned(),
        base_disk: VirtualMachineBaseDisk {
            binding: "ubuntu-24.04-v1".to_owned(),
            source_registry_digest: concat!(
                "docker://quay.io/containerdisks/ubuntu@",
                "sha256:d28194a16351320fa9a093e18233033508a745566eb8ba3b309c32924bf155a5"
            )
            .to_owned(),
            disk_sha256: Sha256Digest::of_bytes(b"vm-base-disk"),
            capacity_bytes: 10_737_418_240,
        },
        base_disk_format: VirtualMachineDiskFormat::Qcow2,
        storage_class_name: "local-path".to_owned(),
        resources: Vec::new(),
        plan_sha256: Sha256Digest::of_bytes(b"vm-plan"),
    };
    let vm_uid = uuid::Uuid::new_v4();
    let root_disk_uid = uuid::Uuid::new_v4();
    let running = KubeVirtRunningObservation {
        observed_environment_generation: 1,
        vm_resource_generation: 2,
        observed_vm_resource_generation: 2,
        vm_uid,
        vmi_uid: uuid::Uuid::new_v4(),
        root_disk_uid,
        guest_ip: "10.42.0.10".parse()?,
        service_cluster_ip: "10.96.0.10".parse()?,
        ssh_host_key_sha256: Sha256Digest::of_bytes(b"stable-host-key"),
        guest_agent_connected: true,
        ssh_ready: true,
        observed_at: timestamp("2026-07-16T08:00:00.000Z"),
    };
    let provision = kubevirt_fence(environment_id, 1, ReconcileAction::Provision);
    store.record_running(&provision, &plan, &running).await?;
    store.record_running(&provision, &plan, &running).await?;

    let mut stale = provision;
    stale.request_id = Sha256Digest::of_bytes(b"stale-replay");
    assert!(matches!(
        store.record_running(&stale, &plan, &running).await,
        Err(KubeVirtObservationStoreError::StaleFence)
    ));

    let stop = kubevirt_fence(environment_id, 2, ReconcileAction::Stop);
    let stopped = KubeVirtStoppedObservation {
        observed_environment_generation: 2,
        vm_uid,
        root_disk_uid,
        vmi_absent: true,
        observed_at: timestamp("2026-07-16T08:05:00.000Z"),
    };
    store.record_stopped(&stop, &plan, &stopped).await?;
    let persisted_host_key: String = sqlx::query_scalar(
        "SELECT ssh_host_key_sha256 FROM environment.kubevirt_runtime_observations \
         WHERE environment_id=$1",
    )
    .bind(environment_id.as_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(persisted_host_key, running.ssh_host_key_sha256.to_string());

    let start = kubevirt_fence(environment_id, 3, ReconcileAction::Start);
    let mut restarted = running;
    restarted.observed_environment_generation = 3;
    restarted.vmi_uid = uuid::Uuid::new_v4();
    restarted.guest_ip = "10.42.0.11".parse()?;
    store.record_running(&start, &plan, &restarted).await?;

    let changed_identity = kubevirt_fence(environment_id, 4, ReconcileAction::Start);
    let mut changed = restarted;
    changed.observed_environment_generation = 4;
    changed.root_disk_uid = uuid::Uuid::new_v4();
    assert!(matches!(
        store
            .record_running(&changed_identity, &plan, &changed)
            .await,
        Err(KubeVirtObservationStoreError::IdentityMismatch)
    ));

    let cleanup = kubevirt_fence(environment_id, 4, ReconcileAction::Cleanup);
    let cleanup_plan = KubeVirtCleanupPlan {
        environment_id,
        namespace: plan.namespace.clone(),
        virtual_machine_name: plan.virtual_machine_name.clone(),
        plan_sha256: Sha256Digest::of_bytes(b"cleanup-plan"),
    };
    let cleanup_evidence = ArtifactRef {
        artifact_id: ArtifactId::new(),
        store_binding: "environment-cleanup-evidence-v1".to_owned(),
        object_version: "cleanup-1".to_owned(),
        sha256: Sha256Digest::of_bytes(b"cleanup-evidence"),
        size_bytes: 1,
        media_type: "application/json".to_owned(),
    };
    store
        .record_deleted(&cleanup, &cleanup_plan, &cleanup_evidence)
        .await?;
    let late_start = kubevirt_fence(environment_id, 5, ReconcileAction::Start);
    let mut late_observation = restarted;
    late_observation.observed_environment_generation = 5;
    assert!(matches!(
        store
            .record_running(&late_start, &plan, &late_observation)
            .await,
        Err(KubeVirtObservationStoreError::Tombstoned)
    ));

    let raced_environment_id = EnvironmentId::new();
    let mut raced_plan = plan.clone();
    raced_plan.environment_id = raced_environment_id;
    raced_plan.namespace = format!("lw-env-{raced_environment_id}");
    let older_fence = kubevirt_fence(raced_environment_id, 1, ReconcileAction::Provision);
    let newer_fence = kubevirt_fence(raced_environment_id, 2, ReconcileAction::Provision);
    let mut older_observation = running;
    older_observation.observed_environment_generation = 1;
    let mut newer_observation = running;
    newer_observation.observed_environment_generation = 2;
    let (older_result, newer_result) = tokio::join!(
        store.record_running(&older_fence, &raced_plan, &older_observation),
        store.record_running(&newer_fence, &raced_plan, &newer_observation),
    );
    assert!(
        older_result.is_ok()
            || matches!(older_result, Err(KubeVirtObservationStoreError::StaleFence))
    );
    newer_result?;
    let persisted_generation: i64 = sqlx::query_scalar(
        "SELECT environment_generation FROM environment.kubevirt_runtime_observations \
         WHERE environment_id=$1",
    )
    .bind(raced_environment_id.as_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(persisted_generation, 2);
    Ok(())
}

fn kubevirt_fence(
    environment_id: EnvironmentId,
    generation: u64,
    action: ReconcileAction,
) -> KubeVirtBackendFence {
    KubeVirtBackendFence {
        protocol_version: 1,
        environment_id,
        operation_id: OperationId::new(),
        provider_step: 1,
        environment_generation: generation,
        attempt: 1,
        action,
        request_id: Sha256Digest::of_bytes(
            format!("{environment_id}:{generation}:{action:?}").as_bytes(),
        ),
        deadline_at: timestamp("2026-07-16T09:00:00.000Z"),
    }
}

fn success_worker(
    store: PgEnvironmentStore,
) -> Result<ReconcileWorker, Box<dyn std::error::Error>> {
    let mut registry = ProviderRegistry::default();
    registry.register(Arc::new(LifecycleSuccessProvider))?;
    Ok(ReconcileWorker::new(
        store,
        Reconciler::new(registry, Duration::from_millis(100))?,
        Duration::from_millis(1_100),
        Duration::from_millis(100),
    )?)
}

#[derive(Clone)]
struct RecordingPublisher {
    fail_next: Arc<AtomicBool>,
    deliveries: Arc<Mutex<Vec<EventId>>>,
}

impl RecordingPublisher {
    fn fail_first() -> Self {
        Self {
            fail_next: Arc::new(AtomicBool::new(true)),
            deliveries: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn deliveries(&self) -> Result<Vec<EventId>, Box<dyn std::error::Error>> {
        self.deliveries
            .lock()
            .map(|deliveries| deliveries.clone())
            .map_err(|_| "recording publisher mutex was poisoned".into())
    }
}

#[async_trait]
impl EnvironmentEventPublisher for RecordingPublisher {
    async fn publish(
        &self,
        _subject: &str,
        event: &CloudEvent<serde_json::Value>,
    ) -> Result<(), PublishFailure> {
        self.deliveries
            .lock()
            .map_err(|_| PublishFailure::Rejected)?
            .push(event.id);
        if self.fail_next.swap(false, Ordering::SeqCst) {
            return Err(PublishFailure::Unavailable);
        }
        Ok(())
    }
}

struct CleanupFailureProvider;

struct LifecycleSuccessProvider;

#[derive(Default)]
struct IdempotentCrashProvider {
    calls: AtomicUsize,
    side_effects: AtomicUsize,
    completed: Mutex<HashSet<(OperationId, u32, ReconcileAction)>>,
}

#[async_trait]
impl EnvironmentProvider for IdempotentCrashProvider {
    fn binding(&self) -> &'static str {
        "container-primary-v1"
    }

    async fn execute(
        &self,
        action: ReconcileAction,
        instance: &contracts::environment::EnvironmentInstance,
    ) -> Result<ProviderObservation, ProviderFailure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self
            .completed
            .lock()
            .map_err(|_| ProviderFailure {
                code: ProviderFailureCode::Transient,
                retryable: true,
            })?
            .insert((
                instance.operation.id,
                instance.operation.provider_step,
                action,
            ))
        {
            self.side_effects.fetch_add(1, Ordering::SeqCst);
        }
        LifecycleSuccessProvider.execute(action, instance).await
    }
}

#[async_trait]
impl EnvironmentProvider for LifecycleSuccessProvider {
    fn binding(&self) -> &'static str {
        "container-primary-v1"
    }

    async fn execute(
        &self,
        action: ReconcileAction,
        instance: &contracts::environment::EnvironmentInstance,
    ) -> Result<ProviderObservation, ProviderFailure> {
        let next_revision = support::revision(instance.revision.get() + 1);
        let observation = match (action, instance.observed_state) {
            (ReconcileAction::Validate, ObservedEnvironmentState::Requested) => {
                ProviderObservation {
                    next_state: ObservedEnvironmentState::Validating,
                    endpoints: Vec::new(),
                    cleanup_evidence: None,
                    operation_complete: false,
                }
            }
            (ReconcileAction::Validate, ObservedEnvironmentState::Validating) => {
                ProviderObservation {
                    next_state: ObservedEnvironmentState::Building,
                    endpoints: Vec::new(),
                    cleanup_evidence: None,
                    operation_complete: false,
                }
            }
            (ReconcileAction::Build, ObservedEnvironmentState::Building) => ProviderObservation {
                next_state: ObservedEnvironmentState::Provisioning,
                endpoints: Vec::new(),
                cleanup_evidence: None,
                operation_complete: false,
            },
            (ReconcileAction::Provision, ObservedEnvironmentState::Provisioning) => {
                ProviderObservation {
                    next_state: ObservedEnvironmentState::Ready,
                    endpoints: vec![EnvironmentEndpoint {
                        id: EndpointId::new(),
                        protocol: EndpointProtocol::Https,
                        revision: next_revision,
                        health: EndpointHealth::Healthy,
                        ssh_host_key_identity_sha256: None,
                        observed_at: timestamp("2026-07-14T00:01:00.000Z"),
                    }],
                    cleanup_evidence: None,
                    operation_complete: true,
                }
            }
            (ReconcileAction::Cleanup, ObservedEnvironmentState::Deleting) => ProviderObservation {
                next_state: ObservedEnvironmentState::Deleted,
                endpoints: Vec::new(),
                cleanup_evidence: Some(ArtifactRef {
                    artifact_id: ArtifactId::new(),
                    store_binding: "environment-cleanup-evidence-v1".to_owned(),
                    object_version: instance.operation.id.to_string(),
                    sha256: Sha256Digest::of_bytes(instance.operation.id.to_string().as_bytes()),
                    size_bytes: 1,
                    media_type: "application/json".to_owned(),
                }),
                operation_complete: true,
            },
            _ => {
                return Err(ProviderFailure {
                    code: ProviderFailureCode::Rejected,
                    retryable: false,
                });
            }
        };
        Ok(observation)
    }
}

#[async_trait]
impl EnvironmentProvider for CleanupFailureProvider {
    fn binding(&self) -> &'static str {
        "container-primary-v1"
    }

    async fn execute(
        &self,
        action: ReconcileAction,
        _instance: &contracts::environment::EnvironmentInstance,
    ) -> Result<ProviderObservation, ProviderFailure> {
        if action != ReconcileAction::Cleanup {
            return Err(ProviderFailure {
                code: ProviderFailureCode::Rejected,
                retryable: false,
            });
        }
        Err(ProviderFailure {
            code: ProviderFailureCode::CleanupFailed,
            retryable: false,
        })
    }
}
