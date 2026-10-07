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
    DesiredEnvironmentState, EndpointHealth, EndpointProtocol, EnvironmentCreateSpec,
    EnvironmentEndpoint, EnvironmentLeaseAuthorization, EnvironmentOperationKind,
    ObservedEnvironmentState, OperationState,
};
use contracts::events::{CloudEvent, EVENT_CONTRACTS, ReleaseWithdrawn, SPEC_VERSION, subjects};
use contracts::http::RecordResourceUsageRequest;
use contracts::resource::{
    GpuAllocation, GpuAllocationMode, GpuRequest, ResourceUsageKind, UsageMeasurement,
    WorkloadResources,
};
use contracts::supply_chain::{VirtualMachineBaseDisk, VirtualMachineDiskFormat};
use contracts::{
    ActorId, ArtifactId, ArtifactRef, CourseId, EndpointId, EnvironmentId, EventId, LeaseId,
    OperationId, ProjectId, ReleaseId, ResourceRequestId, Revision, Sequence, UtcTimestamp,
};
use environment_service::{
    CONTAINER_BACKEND_PROTOCOL_VERSION, ContainerApplyObservation, ContainerBackendFence,
    ContainerExecutorBackend, ContainerExecutorFenceError, ContainerExecutorRequest,
    ContainerExecutorRequestEnvelope, ContainerExecutorResponse, ContainerResourcePlan,
    EnvironmentEventPublisher, EnvironmentInventoryFilter, EnvironmentProvider,
    EnvironmentStoreError, FencedContainerExecutor, FencedKubeVirtExecutor, InboundCommandDecision,
    InboundLifecycleCommand, KUBEVIRT_BACKEND_PROTOCOL_VERSION, KubeVirtBackendFence,
    KubeVirtBaseDiskIdentity, KubeVirtCleanupPlan, KubeVirtExecutionInstance,
    KubeVirtExecutionPermit, KubeVirtExecutorBackend, KubeVirtExecutorFenceError,
    KubeVirtExecutorRequest, KubeVirtExecutorRequestEnvelope, KubeVirtExecutorResponse,
    KubeVirtObservationStore, KubeVirtObservationStoreError, KubeVirtResourcePlan,
    KubeVirtRunningObservation, KubeVirtStoppedObservation, LifecycleCommand, LifecycleError,
    NatsKubeVirtExecutorServer, OutboxDispatchError, OutboxDispatchOutcome, OutboxDispatcher,
    PgContainerExecutorFenceStore, PgEnvironmentStore, PgKubeVirtExecutorFenceStore,
    PgKubeVirtObservationStore, PgReleaseProjectionStore, ProviderFailure, ProviderFailureCode,
    ProviderObservation, ProviderRegistry, PublishFailure, ReconcileAction, ReconcileWorker,
    ReconcileWorkerOutcome, Reconciler, ReleaseProjectionDecision, apply_provider_observation,
};
use persistence_sqlx::Sha256Digest;
use serde_json::Value;
use sqlx::postgres::PgPoolOptions;
use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;
use tokio::sync::Notify;

use support::{requested_instance, revision, timestamp};

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
    support::apply_environment_migrations(&pool).await?;

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
    let mut retry_context = instance.clone();
    retry_context.operation.trace_id = "trace-create-retry".to_owned();
    retry_context.operation.accepted_at = timestamp("2026-07-14T00:00:01.000Z");
    retry_context.operation.next_attempt_at = retry_context.operation.accepted_at;
    retry_context.operation.deadline_at = timestamp("2027-01-14T00:00:01.000Z");
    assert!(retry_context.validate().is_ok());
    assert_eq!(
        store.create("create-key-0001", &retry_context).await?,
        accepted
    );

    sqlx::query(
        "UPDATE environment.environment_instances \
         SET created_at='2026-07-24T00:00:00.123456Z'::timestamptz, \
             updated_at='2026-07-24T00:00:01.654321Z'::timestamptz \
         WHERE environment_id=$1",
    )
    .bind(instance.id.as_uuid())
    .execute(&pool)
    .await?;
    let inventory = store
        .list_owned(
            EnvironmentInventoryFilter {
                project_id: instance.project_id,
                course_id: instance.course_id,
                owner_actor_id: instance.owner_id,
                runtime_kind: None,
                class: None,
                desired_state: None,
                observed_state: None,
                release_id: None,
            },
            None,
            100,
        )
        .await?;
    let listed = inventory
        .records
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
    let mut conflicting_label = instance.clone();
    conflicting_label.display_label = "different-label".to_owned();
    assert!(matches!(
        store.create("create-key-0001", &conflicting_label).await,
        Err(EnvironmentStoreError::IdempotencyConflict)
    ));
    let mut conflicting_actor = instance.clone();
    conflicting_actor.owner_id = ActorId::new();
    conflicting_actor.operation.actor_id = conflicting_actor.owner_id;
    assert!(matches!(
        store.create("create-key-0001", &conflicting_actor).await,
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
    assert_eq!(envelope["projectId"], instance.project_id.to_string());
    assert_eq!(
        envelope["courseId"],
        serde_json::to_value(instance.course_id)?
    );
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
    let claimed_operation = store
        .get_operation(instance.id, instance.owner_id, instance.operation.id)
        .await?;
    assert_eq!(claimed_operation.snapshot.state, OperationState::Running);
    let claimed_contract_state: String = sqlx::query_scalar(
        "SELECT contract->>'state' FROM environment.environment_operations WHERE operation_id=$1",
    )
    .bind(instance.operation.id.as_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(claimed_contract_state, "running");
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
        project_id: inbox_target.project_id,
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
    assert_eq!(
        store
            .accept_command("delete-key-command-identity", &changed_deadline)
            .await?,
        store
            .accept_command("delete-key-command-identity", &identity_command)
            .await?
    );
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
async fn stale_reconcile_lease_is_reported_without_failing_the_worker()
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
    support::apply_environment_migrations(&pool).await?;
    let store = PgEnvironmentStore::new(pool);
    let instance = requested_instance();
    store
        .create("create-key-stale-reconcile", &instance)
        .await?;

    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let mut registry = ProviderRegistry::default();
    registry.register(Arc::new(BlockingProvider {
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    }))?;
    let worker = ReconcileWorker::new(
        store.clone(),
        Reconciler::new(registry, Duration::from_secs(1))?,
        Duration::from_secs(2),
        Duration::from_secs(1),
    )?;
    let worker_task = tokio::spawn(async move {
        worker
            .run_once(
                "environment-worker-stale-reconcile",
                timestamp("2026-07-14T00:01:00.000Z"),
            )
            .await
    });
    entered.notified().await;
    let accepted_at = store.current_time().await?;
    let deadline_at = UtcTimestamp::from_utc(
        accepted_at
            .get()
            .checked_add(time::Duration::minutes(10))
            .ok_or("stale reconcile deadline overflow")?,
    )?;

    let accepted = store
        .accept_command(
            "delete-key-stale-reconcile",
            &LifecycleCommand {
                environment_id: instance.id,
                kind: EnvironmentOperationKind::Delete,
                expected_revision: instance.revision,
                actor_id: ActorId::new(),
                trace_id: "trace-delete-stale-reconcile".to_owned(),
                accepted_at,
                deadline_at,
                access_revocation_revision: Some(revision(9)),
                preserve_mutable_disk: false,
                max_attempts: 3,
                reset_target: None,
            },
        )
        .await?;
    release.notify_one();

    assert_eq!(worker_task.await??, ReconcileWorkerOutcome::LeaseLost);
    let current = store.load(instance.id).await?;
    assert_eq!(current.revision, revision(2));
    assert_eq!(current.operation.id, accepted.operation_id);
    Ok(())
}

#[tokio::test]
async fn api_work_handoff_persists_verified_lease_authorization_and_replays()
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
    support::apply_environment_migrations(&pool).await?;
    let store = PgEnvironmentStore::new(pool);

    let mut handoff = requested_instance();
    handoff.class = contracts::authoring::EnvironmentClass::Work;
    handoff.lease_id = Some(LeaseId::new());
    handoff.capacity_binding = Some("work-capacity-regression".to_owned());
    handoff.eligibility_expires_at = timestamp("2027-07-15T00:00:00.000Z");
    let lease_id = handoff.lease_id.ok_or("lease id missing")?;
    let capacity_binding = handoff
        .capacity_binding
        .clone()
        .ok_or("capacity binding missing")?;
    let authorization = EnvironmentLeaseAuthorization {
        resource_request_id: ResourceRequestId::new(),
        lease_id,
        lease_revision: revision(4),
        environment_id: handoff.id,
        project_id: handoff.project_id,
        course_id: handoff.course_id,
        owner_actor_id: handoff.owner_id,
        capacity_binding: capacity_binding.clone(),
        approved_resources: WorkloadResources {
            cpu_millicores: 500,
            memory_bytes: 512 * 1024 * 1024,
            storage_bytes: 2 * 1024 * 1024 * 1024,
            gpu: None,
        },
        gpu_allocation: None,
        active_from: timestamp("2026-07-14T00:00:00.000Z"),
        expires_at: timestamp("2027-07-15T00:00:00.000Z"),
    };
    let accepted_at = timestamp("2026-07-14T00:00:00.000Z");
    let command = LifecycleCommand {
        environment_id: handoff.id,
        kind: EnvironmentOperationKind::Create,
        expected_revision: revision(1),
        actor_id: handoff.owner_id,
        trace_id: "trace-api-work-handoff-regression".to_owned(),
        accepted_at,
        deadline_at: timestamp("2027-01-14T00:00:00.000Z"),
        access_revocation_revision: None,
        preserve_mutable_disk: false,
        max_attempts: 3,
        reset_target: None,
    };
    let create = EnvironmentCreateSpec {
        project_id: handoff.project_id,
        course_id: handoff.course_id,
        owner_actor_id: handoff.owner_id,
        display_label: handoff.display_label.clone(),
        class: handoff.class,
        runtime_kind: handoff.runtime_kind,
        release_id: handoff.release_id,
        release_version: handoff.release_version,
        provider_binding: handoff.provider_binding.clone(),
        lease_id: handoff.lease_id,
        capacity_binding: handoff.capacity_binding.clone(),
        approved_resources: handoff.approved_resources.clone(),
        gpu_allocation: None,
        eligibility_expires_at: handoff.eligibility_expires_at,
    };

    let accepted = store
        .accept_api_command(
            "resource-work-handoff-regression",
            &command,
            Some(&create),
            Some(authorization.clone()),
            handoff.project_id,
            handoff.course_id,
        )
        .await?;
    assert_eq!(accepted.environment_id, handoff.id);
    let loaded = store.load(handoff.id).await?;
    assert_eq!(
        loaded.operation.lease_authorization,
        Some(authorization.clone())
    );
    assert_eq!(loaded.lease_id, handoff.lease_id);
    assert_eq!(
        loaded.capacity_binding.as_deref(),
        Some(capacity_binding.as_str())
    );

    let replay = store
        .accept_api_command(
            "resource-work-handoff-regression",
            &command,
            Some(&create),
            Some(authorization),
            handoff.project_id,
            handoff.course_id,
        )
        .await?;
    assert_eq!(replay, accepted);
    assert_eq!(store.load(handoff.id).await?.revision, revision(1));
    Ok(())
}

#[tokio::test]
async fn legacy_environment_metering_migration_closes_old_ready_stopped_deleted_boundaries()
-> Result<(), Box<dyn std::error::Error>> {
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            container.get_host_port_ipv4(5432).await?
        ))
        .await?;
    let old_schema = format!(
        "CREATE SCHEMA environment; SET search_path TO environment;\n{}\n{}\n{}\n{}\n{}",
        include_str!("../../../migrations/environment/0001_platform_baseline.sql"),
        include_str!("../../../migrations/environment/0002_project_ownership.sql"),
        include_str!("../../../migrations/environment/0003_resource_usage_deliveries.sql"),
        include_str!("../../../migrations/environment/0004_work_configuration_executions.sql"),
        include_str!("../../../migrations/environment/0005_kubevirt_execution_owner.sql")
    );
    sqlx::raw_sql(&old_schema).execute(&pool).await?;

    let resources = WorkloadResources {
        cpu_millicores: 250,
        memory_bytes: 512 * 1024 * 1024,
        storage_bytes: 2 * 1024 * 1024 * 1024,
        gpu: None,
    };
    let legacy_gpu_resources = WorkloadResources {
        gpu: Some(GpuRequest {
            class: "a10".to_owned(),
            count: 1,
        }),
        ..resources.clone()
    };
    let legacy_gpu_allocation = GpuAllocation {
        entry_id: contracts::GpuCatalogEntryId::new(),
        class: "a10".to_owned(),
        count: 1,
        mode: GpuAllocationMode::Exclusive,
        provider_binding: "provider".to_owned(),
        allocation_binding: "allocation".to_owned(),
        catalog_revision: revision(1),
    };
    let fixtures = [
        (
            ObservedEnvironmentState::Ready,
            DesiredEnvironmentState::Running,
            EnvironmentOperationKind::Create,
            timestamp("2026-07-14T00:00:00.000Z"),
        ),
        (
            ObservedEnvironmentState::Stopped,
            DesiredEnvironmentState::Stopped,
            EnvironmentOperationKind::Stop,
            timestamp("2026-07-14T00:01:00.000Z"),
        ),
        (
            ObservedEnvironmentState::Deleted,
            DesiredEnvironmentState::Deleted,
            EnvironmentOperationKind::Delete,
            timestamp("2026-07-14T00:02:00.000Z"),
        ),
    ];
    let mut instances = Vec::with_capacity(fixtures.len());
    for (observed_state, desired_state, operation_kind, accepted_at) in fixtures {
        let mut instance = support::ready_instance();
        let instance_resources = if observed_state == ObservedEnvironmentState::Ready {
            resources.clone()
        } else {
            legacy_gpu_resources.clone()
        };
        instance.approved_resources = instance_resources.clone();
        if observed_state != ObservedEnvironmentState::Ready {
            instance.gpu_allocation = Some(legacy_gpu_allocation.clone());
        }
        instance.observed_state = observed_state;
        instance.desired_state = desired_state;
        instance.operation.kind = operation_kind;
        instance.operation.state = OperationState::Succeeded;
        instance.operation.accepted_revision = instance.revision;
        instance.operation.accepted_at = accepted_at;
        instance.operation.next_attempt_at = accepted_at;
        instance.operation.deadline_at = timestamp("2026-07-14T00:10:00.000Z");
        instance.operation.access_revocation_revision = match operation_kind {
            EnvironmentOperationKind::Stop | EnvironmentOperationKind::Delete => Some(revision(3)),
            _ => None,
        };
        if observed_state != ObservedEnvironmentState::Ready {
            instance.endpoints.clear();
        }
        if observed_state == ObservedEnvironmentState::Deleted {
            instance.cleanup_evidence = Some(ArtifactRef {
                artifact_id: ArtifactId::new(),
                store_binding: "environment-cleanup-evidence-v1".to_owned(),
                object_version: instance.operation.id.to_string(),
                size_bytes: 1,
                media_type: "application/json".to_owned(),
            });
            instance.operation.cleanup_started_at = Some(accepted_at);
        }
        let mut legacy_contract = serde_json::to_value(&instance)?;
        legacy_contract
            .as_object_mut()
            .ok_or("legacy Environment contract must be an object")?
            .remove("approvedResources");
        let projection_contract = serde_json::json!({
            "environmentSpec": {
                "resources": {
                    "cpuMillicores": instance_resources.cpu_millicores,
                    "memoryBytes": instance_resources.memory_bytes,
                    "storageBytes": instance_resources.storage_bytes,
                    "gpu": &instance_resources.gpu
                }
            }
        });
        sqlx::query(
            "INSERT INTO environment.release_projections \
             (release_id,project_id,course_id,release_version,provider_binding,projection_sha256,contract,projected_event_id) \
             VALUES ($1,$2,$3,1,$4,$5,$6,$7)",
        )
        .bind(instance.release_id.as_uuid())
        .bind(instance.project_id.as_uuid())
        .bind(instance.course_id.map(CourseId::as_uuid))
        .bind(&instance.provider_binding)
        .bind("a".repeat(64))
        .bind(projection_contract)
        .bind(EventId::new().as_uuid())
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO environment.environment_instances \
             (environment_id,project_id,course_id,owner_actor_id,release_id,generation,observed_generation,\
              desired_state,observed_state,provider_binding,lease_id,capacity_binding,revision,terminal_diagnostic,\
              failed_phase,eligibility_expires_at,contract) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17)",
        )
        .bind(instance.id.as_uuid())
        .bind(instance.project_id.as_uuid())
        .bind(instance.course_id.map(CourseId::as_uuid))
        .bind(instance.owner_id.as_uuid())
        .bind(instance.release_id.as_uuid())
        .bind(i64::try_from(instance.generation)?)
        .bind(i64::try_from(instance.observed_generation)?)
        .bind(match desired_state {
            DesiredEnvironmentState::Running => "running",
            DesiredEnvironmentState::Stopped => "stopped",
            DesiredEnvironmentState::Deleted => "deleted",
        })
        .bind(match observed_state {
            ObservedEnvironmentState::Ready => "ready",
            ObservedEnvironmentState::Stopped => "stopped",
            ObservedEnvironmentState::Deleted => "deleted",
            _ => unreachable!("fixture state is constrained above"),
        })
        .bind(&instance.provider_binding)
        .bind(instance.lease_id.map(LeaseId::as_uuid))
        .bind(instance.capacity_binding.as_deref())
        .bind(i64::try_from(instance.revision.get())?)
        .bind(instance.last_diagnostic_code.as_deref())
        .bind(Option::<&str>::None)
        .bind(instance.eligibility_expires_at.get())
        .bind(legacy_contract)
        .execute(&pool)
        .await?;
        if matches!(
            observed_state,
            ObservedEnvironmentState::Stopped | ObservedEnvironmentState::Deleted
        ) {
            let legacy_compute_started_at = if observed_state == ObservedEnvironmentState::Deleted {
                Value::Null
            } else {
                serde_json::json!("2026-07-14T00:00:30.000Z")
            };
            let legacy_meter = serde_json::json!({
                "version": 1,
                "environmentId": instance.id,
                "projectId": instance.project_id,
                "courseId": instance.course_id,
                "ownerActorId": instance.owner_id,
                "gpuAllocation": &legacy_gpu_allocation,
                "computeStartedAt": legacy_compute_started_at,
                "pendingGpuUnitSeconds": 0,
                "settlementPending": true
            });
            sqlx::query(
                "INSERT INTO environment.resource_metering_state (environment_id, contract) \
                 VALUES ($1, $2)",
            )
            .bind(instance.id.as_uuid())
            .bind(legacy_meter)
            .execute(&pool)
            .await?;
        }
        instances.push(instance);
    }

    let source_event_id = EventId::new();
    let legacy_delivery = serde_json::json!({
        "kind": "compute",
        "projectId": instances[0].project_id,
        "courseId": instances[0].course_id,
        "requestId": ResourceRequestId::new(),
        "leaseId": null,
        "sourceEventId": source_event_id,
        "measuredFrom": "2026-07-14T00:10:00.000Z",
        "measuredUntil": "2026-07-14T00:11:00.000Z",
        "measurement": {
            "state": "known",
            "quantities": {
                "cpuMillicoreSeconds": 15000,
                "memoryByteSeconds": 31_457_280_000_u64,
                "storageByteSeconds": 0,
                "gpuUnitSeconds": 0
            }
        }
    });
    sqlx::query(
        "INSERT INTO environment.resource_meter_deliveries \
         (delivery_id,environment_id,source_event_id,kind,measured_from,measured_until,request,state,attempts,next_attempt_at) \
         VALUES ($1,$2,$3,'compute',$4,$5,$6,'pending',0,$7)",
    )
    .bind(EventId::new().as_uuid())
    .bind(instances[0].id.as_uuid())
    .bind(source_event_id.as_uuid())
    .bind(timestamp("2026-07-14T00:10:00.000Z").get())
    .bind(timestamp("2026-07-14T00:11:00.000Z").get())
    .bind(legacy_delivery)
    .bind(timestamp("2026-07-14T00:12:00.000Z").get())
    .execute(&pool)
    .await?;

    let migration =
        include_str!("../../../migrations/environment/0006_unified_environment_metering.sql");
    sqlx::raw_sql(migration).execute(&pool).await?;
    sqlx::raw_sql(migration).execute(&pool).await?;

    let store = PgEnvironmentStore::new(pool.clone());
    for instance in &instances {
        let expected_resources = if instance.gpu_allocation.is_some() {
            &legacy_gpu_resources
        } else {
            &resources
        };
        assert_eq!(
            store.load(instance.id).await?.approved_resources,
            expected_resources.clone()
        );
    }
    for instance in &instances {
        let expected_resources = if instance.gpu_allocation.is_some() {
            &legacy_gpu_resources
        } else {
            &resources
        };
        let contract: Value = sqlx::query_scalar(
            "SELECT contract FROM environment.resource_metering_state WHERE environment_id=$1",
        )
        .bind(instance.id.as_uuid())
        .fetch_one(&pool)
        .await?;
        assert_eq!(
            contract["approvedResources"],
            serde_json::to_value(expected_resources)?
        );
        assert!(matches!(
            serde_json::from_value::<contracts::resource::ResourceUsageTarget>(
                contract["target"].clone()
            )?,
            contracts::resource::ResourceUsageTarget::ExperimentEnvironment { environment_id }
                if environment_id == instance.id
        ));
        match instance.observed_state {
            ObservedEnvironmentState::Ready => {
                assert!(contract["computeUnknownStartedAt"].is_string());
                assert!(contract["storageStartedAt"].is_string());
                assert_eq!(contract["storageKnown"], false);
            }
            ObservedEnvironmentState::Stopped => {
                assert!(contract["computeStartedAt"].is_null());
                assert!(contract["computeUnknownStartedAt"].is_null());
                assert!(contract["storageStartedAt"].is_string());
                assert_eq!(contract["storageKnown"], false);
                assert!(contract.get("pendingGpuUnitSeconds").is_none());
                assert!(contract.get("settlementPending").is_none());
                assert_eq!(
                    contract["gpuAllocation"],
                    serde_json::to_value(&legacy_gpu_allocation)?
                );
            }
            ObservedEnvironmentState::Deleted => {
                assert!(contract["computeStartedAt"].is_null());
                assert!(contract["computeUnknownStartedAt"].is_null());
                assert!(contract["storageStartedAt"].is_null());
                assert_eq!(contract["storageKnown"], false);
                assert!(contract.get("pendingGpuUnitSeconds").is_none());
                assert!(contract.get("settlementPending").is_none());
            }
            _ => unreachable!(),
        }
    }
    let migrated_request: Value = sqlx::query_scalar(
        "SELECT request FROM environment.resource_meter_deliveries WHERE source_event_id=$1",
    )
    .bind(source_event_id.as_uuid())
    .fetch_one(&pool)
    .await?;
    let decoded: RecordResourceUsageRequest = serde_json::from_value(migrated_request.clone())?;
    assert!(matches!(
        decoded.target,
        contracts::resource::ResourceUsageTarget::ExperimentEnvironment { environment_id }
            if environment_id == instances[0].id
    ));
    assert!(migrated_request.get("projectId").is_none());
    assert!(migrated_request.get("requestId").is_none());
    let delivery_state: String = sqlx::query_scalar(
        "SELECT state FROM environment.resource_meter_deliveries WHERE source_event_id=$1",
    )
    .bind(source_event_id.as_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(delivery_state, "pending");

    // Exercise the migrated Ready row through the real store and worker.  The initial
    // migration marker is unknown compute/storage; Stop closes only compute, Restart
    // recovers storage and starts a new known compute interval, and Delete closes both.
    let migrated_ready = store.load(instances[0].id).await?;
    let worker = success_worker(store.clone())?;
    store
        .accept_command(
            "legacy-meter-stop",
            &LifecycleCommand {
                environment_id: migrated_ready.id,
                kind: EnvironmentOperationKind::Stop,
                expected_revision: migrated_ready.revision,
                actor_id: migrated_ready.owner_id,
                trace_id: "trace-legacy-meter-stop".to_owned(),
                accepted_at: timestamp("2026-07-14T00:01:30.000Z"),
                deadline_at: timestamp("2027-01-14T00:06:00.000Z"),
                access_revocation_revision: Some(revision(4)),
                preserve_mutable_disk: true,
                max_attempts: 3,
                reset_target: None,
            },
        )
        .await?;
    assert!(matches!(
        worker
            .run_once(
                "legacy-meter-stop-worker",
                timestamp("2026-07-14T00:01:30.000Z")
            )
            .await?,
        ReconcileWorkerOutcome::Advanced {
            state: ObservedEnvironmentState::Stopped,
            terminal: true
        }
    ));
    let stopped_meter: Value = sqlx::query_scalar(
        "SELECT contract FROM environment.resource_metering_state WHERE environment_id=$1",
    )
    .bind(migrated_ready.id.as_uuid())
    .fetch_one(&pool)
    .await?;
    assert!(stopped_meter["computeStartedAt"].is_null());
    assert!(stopped_meter["computeUnknownStartedAt"].is_null());
    assert!(stopped_meter["storageStartedAt"].is_string());
    assert_eq!(stopped_meter["storageKnown"], false);

    let stopped = store.load(migrated_ready.id).await?;
    store
        .accept_command(
            "legacy-meter-restart",
            &LifecycleCommand {
                environment_id: stopped.id,
                kind: EnvironmentOperationKind::Restart,
                expected_revision: stopped.revision,
                actor_id: stopped.owner_id,
                trace_id: "trace-legacy-meter-restart".to_owned(),
                accepted_at: timestamp("2026-07-14T00:02:00.000Z"),
                deadline_at: timestamp("2027-01-15T00:07:00.000Z"),
                access_revocation_revision: None,
                preserve_mutable_disk: true,
                max_attempts: 3,
                reset_target: None,
            },
        )
        .await?;
    assert!(matches!(
        worker
            .run_once(
                "legacy-meter-restart-worker",
                timestamp("2026-07-14T00:03:00.000Z")
            )
            .await?,
        ReconcileWorkerOutcome::Advanced {
            state: ObservedEnvironmentState::Ready,
            terminal: true
        }
    ));
    let restarted_meter: Value = sqlx::query_scalar(
        "SELECT contract FROM environment.resource_metering_state WHERE environment_id=$1",
    )
    .bind(migrated_ready.id.as_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(
        restarted_meter["computeStartedAt"],
        serde_json::json!("2026-07-14T00:03:00.000Z")
    );
    assert_eq!(
        restarted_meter["storageStartedAt"],
        serde_json::json!("2026-07-14T00:03:00.000Z")
    );
    assert_eq!(restarted_meter["storageKnown"], true);

    let restarted = store.load(migrated_ready.id).await?;
    store
        .accept_command(
            "legacy-meter-delete",
            &LifecycleCommand {
                environment_id: restarted.id,
                kind: EnvironmentOperationKind::Delete,
                expected_revision: restarted.revision,
                actor_id: restarted.owner_id,
                trace_id: "trace-legacy-meter-delete".to_owned(),
                accepted_at: timestamp("2026-07-14T00:04:00.000Z"),
                deadline_at: timestamp("2027-01-16T00:08:00.000Z"),
                access_revocation_revision: Some(revision(5)),
                preserve_mutable_disk: false,
                max_attempts: 3,
                reset_target: None,
            },
        )
        .await?;
    assert!(matches!(
        worker
            .run_once(
                "legacy-meter-delete-worker",
                timestamp("2026-07-14T00:04:30.000Z")
            )
            .await?,
        ReconcileWorkerOutcome::Advanced {
            state: ObservedEnvironmentState::Deleted,
            terminal: true
        }
    ));
    let deleted_meter: Value = sqlx::query_scalar(
        "SELECT contract FROM environment.resource_metering_state WHERE environment_id=$1",
    )
    .bind(migrated_ready.id.as_uuid())
    .fetch_one(&pool)
    .await?;
    assert!(deleted_meter["computeStartedAt"].is_null());
    assert!(deleted_meter["computeUnknownStartedAt"].is_null());
    assert!(deleted_meter["storageStartedAt"].is_null());
    assert_eq!(deleted_meter["storageKnown"], false);

    let deliveries: Vec<Value> = sqlx::query_scalar(
        "SELECT request FROM environment.resource_meter_deliveries \
         WHERE environment_id=$1 ORDER BY measured_from, delivery_id",
    )
    .bind(migrated_ready.id.as_uuid())
    .fetch_all(&pool)
    .await?;
    let deliveries: Vec<RecordResourceUsageRequest> = deliveries
        .into_iter()
        .map(serde_json::from_value)
        .collect::<Result<_, _>>()?;
    assert_eq!(deliveries.len(), 5);
    for request in &deliveries {
        assert!(matches!(
            request.target,
            contracts::resource::ResourceUsageTarget::ExperimentEnvironment { environment_id }
                if environment_id == migrated_ready.id
        ));
    }
    let find_delivery = |kind: ResourceUsageKind, from: &str, until: &str| {
        deliveries.iter().find(|request| {
            request.kind == kind
                && request.measured_from == timestamp(from)
                && request.measured_until == timestamp(until)
        })
    };
    assert!(matches!(
        find_delivery(
            ResourceUsageKind::Compute,
            "2026-07-14T00:00:00.000Z",
            "2026-07-14T00:01:30.000Z"
        )
        .ok_or("migrated unknown compute delivery missing")?
        .measurement,
        UsageMeasurement::Unknown { .. }
    ));
    assert!(matches!(
        find_delivery(
            ResourceUsageKind::Storage,
            "2026-07-14T00:00:00.000Z",
            "2026-07-14T00:03:00.000Z"
        )
        .ok_or("migrated unknown storage delivery missing")?
        .measurement,
        UsageMeasurement::Unknown { .. }
    ));
    assert!(matches!(
        find_delivery(
            ResourceUsageKind::Compute,
            "2026-07-14T00:03:00.000Z",
            "2026-07-14T00:04:30.000Z"
        )
        .ok_or("migrated known compute delivery missing")?
        .measurement,
        UsageMeasurement::Known { .. }
    ));
    assert!(matches!(
        find_delivery(
            ResourceUsageKind::Storage,
            "2026-07-14T00:03:00.000Z",
            "2026-07-14T00:04:30.000Z"
        )
        .ok_or("migrated known storage delivery missing")?
        .measurement,
        UsageMeasurement::Known { .. }
    ));
    Ok(())
}

#[tokio::test]
#[allow(clippy::expect_used)]
async fn work_lease_refresh_rebinds_ready_endpoints_and_fences_cleanup()
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
    support::apply_environment_migrations(&pool).await?;

    let store = PgEnvironmentStore::new(pool.clone());
    let mut work = requested_instance();
    work.class = contracts::authoring::EnvironmentClass::Work;
    work.lease_id = Some(LeaseId::new());
    work.capacity_binding = Some("workspace-v1".to_owned());
    let initial_expiry = timestamp("2027-07-15T00:00:00.000Z");
    work.eligibility_expires_at = initial_expiry;
    work.operation.lease_authorization = Some(EnvironmentLeaseAuthorization {
        resource_request_id: ResourceRequestId::new(),
        lease_id: work.lease_id.expect("lease set above"),
        lease_revision: support::revision(1),
        environment_id: work.id,
        project_id: work.project_id,
        course_id: work.course_id,
        owner_actor_id: work.owner_id,
        capacity_binding: work
            .capacity_binding
            .clone()
            .expect("capacity binding set above"),
        approved_resources: WorkloadResources {
            cpu_millicores: 500,
            memory_bytes: 512 * 1024 * 1024,
            storage_bytes: 2 * 1024 * 1024 * 1024,
            gpu: None,
        },
        gpu_allocation: None,
        active_from: timestamp("2026-07-14T00:00:00.000Z"),
        expires_at: initial_expiry,
    });
    work.approved_resources = work
        .operation
        .lease_authorization
        .as_ref()
        .ok_or("expected initial Work lease authorization")?
        .approved_resources
        .clone();
    store.create("create-key-lease-refresh", &work).await?;

    let worker = success_worker(store.clone())?;
    for index in 0..4 {
        assert!(matches!(
            worker
                .run_once(
                    &format!("environment-worker-lease-refresh-{index}"),
                    timestamp("2026-07-14T00:01:00.000Z")
                )
                .await?,
            ReconcileWorkerOutcome::Advanced { .. }
        ));
    }
    let ready = store.load(work.id).await?;
    assert_eq!(ready.observed_state, ObservedEnvironmentState::Ready);
    let current_authorization = ready
        .operation
        .lease_authorization
        .clone()
        .ok_or("expected persisted lease authorization")?;

    let mut renewed_authorization = current_authorization.clone();
    renewed_authorization.lease_revision = support::revision(2);
    renewed_authorization.expires_at = timestamp("2027-07-16T00:00:00.000Z");
    let refreshed = store
        .refresh_work_lease(work.id, renewed_authorization.clone())
        .await?;
    assert_eq!(refreshed.revision, Revision::new(ready.revision.get() + 1)?);
    assert!(
        refreshed
            .endpoints
            .iter()
            .all(|endpoint| endpoint.revision == refreshed.revision)
    );
    assert_eq!(
        refreshed.operation.lease_authorization,
        Some(renewed_authorization.clone())
    );
    let refreshed_operation = store
        .get_operation(work.id, work.owner_id, refreshed.operation.id)
        .await?;
    assert_eq!(
        refreshed_operation.snapshot.operation_id,
        refreshed.operation.id
    );
    assert_eq!(
        refreshed_operation.snapshot.state,
        refreshed.operation.state
    );

    let mut wrong_scope = renewed_authorization.clone();
    wrong_scope.project_id = ProjectId::new();
    assert!(matches!(
        store.refresh_work_lease(work.id, wrong_scope).await,
        Err(EnvironmentStoreError::LeaseAuthorizationInvalid)
    ));
    assert_eq!(store.load(work.id).await?.revision, refreshed.revision);

    store
        .accept_command(
            "stop-key-after-lease-refresh",
            &LifecycleCommand {
                environment_id: work.id,
                kind: EnvironmentOperationKind::Stop,
                expected_revision: refreshed.revision,
                actor_id: ActorId::new(),
                trace_id: "trace-stop-after-lease-refresh".to_owned(),
                accepted_at: timestamp("2026-07-14T00:02:00.000Z"),
                deadline_at: timestamp("2026-07-14T00:07:00.000Z"),
                access_revocation_revision: Some(support::revision(6)),
                preserve_mutable_disk: false,
                max_attempts: 3,
                reset_target: None,
            },
        )
        .await?;
    assert!(matches!(
        worker
            .run_once(
                "environment-worker-stop-after-lease-refresh",
                timestamp("2026-07-14T00:02:00.000Z")
            )
            .await?,
        ReconcileWorkerOutcome::Advanced {
            state: ObservedEnvironmentState::Stopped,
            terminal: true
        }
    ));
    let stopped = store.load(work.id).await?;
    assert_eq!(stopped.operation.kind, EnvironmentOperationKind::Stop);
    assert_eq!(stopped.operation.state, OperationState::Succeeded);
    let mut stop_refreshed_authorization = stopped
        .operation
        .lease_authorization
        .clone()
        .ok_or("expected lease authorization after stop")?;
    stop_refreshed_authorization.lease_revision = support::revision(3);
    stop_refreshed_authorization.expires_at = timestamp("2027-07-17T00:00:00.000Z");
    let stopped_refreshed = store
        .refresh_work_lease(work.id, stop_refreshed_authorization)
        .await?;
    let stopped_operation = store
        .get_operation(work.id, work.owner_id, stopped.operation.id)
        .await?;
    assert_eq!(
        stopped_operation.snapshot.operation_id,
        stopped.operation.id
    );
    assert_eq!(stopped_operation.snapshot.state, OperationState::Succeeded);

    store
        .accept_command(
            "restart-key-after-lease-refresh",
            &LifecycleCommand {
                environment_id: work.id,
                kind: EnvironmentOperationKind::Restart,
                expected_revision: stopped_refreshed.revision,
                actor_id: ActorId::new(),
                trace_id: "trace-restart-after-lease-refresh".to_owned(),
                accepted_at: timestamp("2026-07-14T00:03:00.000Z"),
                deadline_at: timestamp("2026-07-14T00:08:00.000Z"),
                access_revocation_revision: None,
                preserve_mutable_disk: true,
                max_attempts: 3,
                reset_target: None,
            },
        )
        .await?;
    let restarting = store.load(work.id).await?;
    assert_eq!(restarting.operation.kind, EnvironmentOperationKind::Restart);
    assert_eq!(
        restarting.observed_state,
        ObservedEnvironmentState::Provisioning
    );
    assert!(matches!(
        worker
            .run_once(
                "environment-worker-restart-after-lease-refresh",
                timestamp("2026-07-14T00:03:00.000Z")
            )
            .await?,
        ReconcileWorkerOutcome::Advanced {
            state: ObservedEnvironmentState::Ready,
            terminal: true
        }
    ));
    let restarted = store.load(work.id).await?;
    assert_eq!(restarted.observed_state, ObservedEnvironmentState::Ready);

    store
        .accept_command(
            "delete-key-after-lease-refresh",
            &LifecycleCommand {
                environment_id: work.id,
                kind: EnvironmentOperationKind::Delete,
                expected_revision: restarted.revision,
                actor_id: ActorId::new(),
                trace_id: "trace-delete-after-lease-refresh".to_owned(),
                accepted_at: timestamp("2026-07-14T00:04:00.000Z"),
                deadline_at: timestamp("2026-07-14T00:09:00.000Z"),
                access_revocation_revision: Some(support::revision(7)),
                preserve_mutable_disk: false,
                max_attempts: 3,
                reset_target: None,
            },
        )
        .await?;
    let mut refresh_during_delete = renewed_authorization.clone();
    refresh_during_delete.lease_revision = support::revision(3);
    refresh_during_delete.expires_at = timestamp("2027-07-17T00:00:00.000Z");
    assert!(matches!(
        store
            .refresh_work_lease(work.id, refresh_during_delete)
            .await,
        Err(EnvironmentStoreError::LeaseAuthorizationInvalid)
    ));
    assert!(matches!(
        worker
            .run_once(
                "environment-worker-delete-after-lease-refresh",
                timestamp("2026-07-14T00:04:00.000Z")
            )
            .await?,
        ReconcileWorkerOutcome::Advanced {
            state: ObservedEnvironmentState::Deleted,
            terminal: true
        }
    ));
    let deleted = store.load(work.id).await?;
    assert_eq!(deleted.observed_state, ObservedEnvironmentState::Deleted);
    let meter: Value = sqlx::query_scalar(
        "SELECT contract FROM environment.resource_metering_state WHERE environment_id=$1",
    )
    .bind(work.id.as_uuid())
    .fetch_one(&pool)
    .await?;
    assert!(meter["computeStartedAt"].is_null());
    assert!(meter["computeUnknownStartedAt"].is_null());
    assert!(meter["storageStartedAt"].is_null());
    assert_eq!(meter["storageKnown"], false);
    let deliveries: Vec<Value> = sqlx::query_scalar(
        "SELECT request FROM environment.resource_meter_deliveries \
         WHERE environment_id=$1 ORDER BY measured_from, delivery_id",
    )
    .bind(work.id.as_uuid())
    .fetch_all(&pool)
    .await?;
    assert_eq!(deliveries.len(), 3);
    let resource_request_id = renewed_authorization.resource_request_id;
    let lease_id = work.lease_id.ok_or("expected Work lease id")?;
    for delivery in deliveries {
        let request: RecordResourceUsageRequest = serde_json::from_value(delivery)?;
        assert!(matches!(
            request.target,
            contracts::resource::ResourceUsageTarget::ResourceRequest {
                request_id,
                lease_id: Some(target_lease_id),
            } if request_id == resource_request_id && target_lease_id == lease_id
        ));
    }
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
        "CREATE SCHEMA environment; SET search_path TO environment;\n{}\n{}\n{}",
        include_str!("../../../migrations/environment/0001_platform_baseline.sql"),
        include_str!("../../../migrations/environment/0002_project_ownership.sql"),
        include_str!("../../../migrations/environment/0003_resource_usage_deliveries.sql")
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
        "CREATE SCHEMA environment; SET search_path TO environment;\n{}\n{}\n{}",
        include_str!("../../../migrations/environment/0001_platform_baseline.sql"),
        include_str!("../../../migrations/environment/0002_project_ownership.sql"),
        include_str!("../../../migrations/environment/0003_resource_usage_deliveries.sql")
    );
    sqlx::raw_sql(&migrations).execute(&pool).await?;

    let consumer = "environment-release-v1";
    let release_id = ReleaseId::new();
    let project_id = ProjectId::new();
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
         (release_id,project_id,course_id,release_version,provider_binding,projection_sha256,contract,projected_event_id) \
         VALUES ($1,$2,$3,1,'container-primary-v1',$4,'{}'::jsonb,$5)",
    )
    .bind(release_id.as_uuid())
    .bind(project_id.as_uuid())
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
        project_id,
        course_id: Some(course_id),
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
        "CREATE SCHEMA environment; SET search_path TO environment;\n{}\n{}\n{}",
        include_str!("../../../migrations/environment/0001_platform_baseline.sql"),
        include_str!("../../../migrations/environment/0002_project_ownership.sql"),
        include_str!("../../../migrations/environment/0003_resource_usage_deliveries.sql")
    );
    sqlx::raw_sql(&migrations).execute(&pool).await?;
    let authority_now = container_database_now(&pool).await?;
    let deadline = container_add_time(authority_now, time::Duration::minutes(1))?;
    let environment_id = EnvironmentId::new();
    let plan = ContainerResourcePlan {
        environment_id,
        project_id: contracts::ProjectId::new(),
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
        project_id: contracts::ProjectId::new(),
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
            trace_id: "01900000000070008000000000000001".to_owned(),
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
        _: &KubeVirtExecutionPermit,
    ) -> KubeVirtExecutorResponse {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match request {
            KubeVirtExecutorRequest::Stop { plan } => KubeVirtExecutorResponse::Stopped {
                plan_sha256: plan.plan_sha256,
                observation: KubeVirtStoppedObservation {
                    observed_environment_generation: fence.environment_generation,
                    vm_uid: uuid::Uuid::new_v4(),
                    root_disk_uid: uuid::Uuid::new_v4(),
                    vmi_absent: true,
                    observed_at: self.observed_at,
                },
            },
            KubeVirtExecutorRequest::DeleteNamespace { plan } => {
                KubeVirtExecutorResponse::Deleted {
                    plan_sha256: plan.plan_sha256,
                    cleanup_evidence: ArtifactRef {
                        artifact_id: ArtifactId::new(),
                        store_binding: "environment-cleanup-evidence-v1".to_owned(),
                        object_version: plan.plan_sha256.to_string(),
                        size_bytes: 1,
                        media_type: "application/json".to_owned(),
                    },
                }
            }
            KubeVirtExecutorRequest::Apply { plan }
            | KubeVirtExecutorRequest::Observe { plan }
            | KubeVirtExecutorRequest::Start { plan }
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
    support::apply_environment_migrations(&pool).await?;
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
    let mut authority = requested_instance();
    authority.id = environment_id;
    authority.operation.id = operation_id;
    authority.operation.accepted_at = observed_at;
    authority.operation.next_attempt_at = observed_at;
    authority.operation.deadline_at = deadline;
    authority.eligibility_expires_at = deadline;
    PgEnvironmentStore::new(pool.clone())
        .create("kubevirt-replay-authority", &authority)
        .await?;
    let instance = executor_instance();
    for _ in 0..2 {
        FencedKubeVirtExecutor::new(
            PgKubeVirtExecutorFenceStore::new(pool.clone()),
            CountingKubeVirtExecutor {
                calls: Arc::clone(&calls),
                observed_at,
            },
            instance.clone(),
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
        instance,
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

fn executor_instance() -> KubeVirtExecutionInstance {
    KubeVirtExecutionInstance {
        namespace: "labweaver-system".to_owned(),
        pod_name: "executor-fixture".to_owned(),
        pod_uid: uuid::Uuid::new_v4(),
        container_name: "kubevirt-executor".to_owned(),
        boot_token: uuid::Uuid::new_v4(),
    }
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
            capacity_bytes: 10_737_418_240,
        },
        base_disk_format: VirtualMachineDiskFormat::Qcow2,
        base_disk_identity: KubeVirtBaseDiskIdentity::ReviewedDiskSha256,
        base_disk_data_source_namespace: "labweaver-system".to_owned(),
        base_disk_data_source_name: "ubuntu-lab-base-v1".to_owned(),
        base_disk_disk_sha256: "ffe6203da54deeb6db5d2a98a83f9ec8e55f149d3f7ba622e1abe5fa966ee3d6"
            .to_owned(),
        storage_class_name: "local-path".to_owned(),
        vm_vgpu_licensing: None,
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
                project_id: contracts::ProjectId::new(),
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
            trace_id: "01900000000070008000000000000002".to_owned(),
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
        | KubeVirtExecutorRequest::Restart { plan } => plan.environment_id,
        KubeVirtExecutorRequest::Stop { plan }
        | KubeVirtExecutorRequest::DeleteNamespace { plan } => plan.environment_id,
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
        "CREATE SCHEMA environment; SET search_path TO environment;\n{}\n{}\n{}",
        include_str!("../../../migrations/environment/0001_platform_baseline.sql"),
        include_str!("../../../migrations/environment/0002_project_ownership.sql"),
        include_str!("../../../migrations/environment/0003_resource_usage_deliveries.sql")
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
            capacity_bytes: 10_737_418_240,
        },
        base_disk_format: VirtualMachineDiskFormat::Qcow2,
        base_disk_identity: KubeVirtBaseDiskIdentity::ReviewedDiskSha256,
        base_disk_data_source_namespace: "labweaver-system".to_owned(),
        base_disk_data_source_name: "ubuntu-lab-base-v1".to_owned(),
        base_disk_disk_sha256: "ffe6203da54deeb6db5d2a98a83f9ec8e55f149d3f7ba622e1abe5fa966ee3d6"
            .to_owned(),
        storage_class_name: "local-path".to_owned(),
        vm_vgpu_licensing: None,
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
    let stop_plan = KubeVirtCleanupPlan {
        environment_id,
        project_id: contracts::ProjectId::new(),
        namespace: plan.namespace.clone(),
        virtual_machine_name: plan.virtual_machine_name.clone(),
        plan_sha256: Sha256Digest::of_bytes(b"stop-plan"),
    };
    store.record_stopped(&stop, &stop_plan, &stopped).await?;
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
        project_id: contracts::ProjectId::new(),
        namespace: plan.namespace.clone(),
        virtual_machine_name: plan.virtual_machine_name.clone(),
        plan_sha256: Sha256Digest::of_bytes(b"cleanup-plan"),
    };
    let cleanup_evidence = ArtifactRef {
        artifact_id: ArtifactId::new(),
        store_binding: "environment-cleanup-evidence-v1".to_owned(),
        object_version: "cleanup-1".to_owned(),
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
        trace_id: "01900000000070008000000000000003".to_owned(),
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

struct BlockingProvider {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl EnvironmentProvider for BlockingProvider {
    fn binding(&self) -> &'static str {
        "container-primary-v1"
    }

    async fn execute(
        &self,
        _action: ReconcileAction,
        _instance: &contracts::environment::EnvironmentInstance,
    ) -> Result<environment_service::ProviderOutcome<ProviderObservation>, ProviderFailure> {
        async {
            self.entered.notify_one();
            self.release.notified().await;
            Ok(ProviderObservation {
                next_state: ObservedEnvironmentState::Validating,
                endpoints: Vec::new(),
                cleanup_evidence: None,
                operation_complete: false,
            })
        }
        .await
        .map(environment_service::ProviderOutcome::Completed)
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
    ) -> Result<environment_service::ProviderOutcome<ProviderObservation>, ProviderFailure> {
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
    ) -> Result<environment_service::ProviderOutcome<ProviderObservation>, ProviderFailure> {
        async {
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
                (ReconcileAction::Build, ObservedEnvironmentState::Building) => {
                    ProviderObservation {
                        next_state: ObservedEnvironmentState::Provisioning,
                        endpoints: Vec::new(),
                        cleanup_evidence: None,
                        operation_complete: false,
                    }
                }
                (ReconcileAction::Provision, ObservedEnvironmentState::Provisioning) => {
                    ProviderObservation {
                        next_state: ObservedEnvironmentState::Ready,
                        endpoints: vec![EnvironmentEndpoint {
                            id: EndpointId::new(),
                            protocol: EndpointProtocol::Https,
                            revision: next_revision,
                            health: EndpointHealth::Healthy,
                            observed_at: timestamp("2026-07-14T00:01:00.000Z"),
                        }],
                        cleanup_evidence: None,
                        operation_complete: true,
                    }
                }
                (ReconcileAction::Stop, ObservedEnvironmentState::Stopping) => {
                    ProviderObservation {
                        next_state: ObservedEnvironmentState::Stopped,
                        endpoints: Vec::new(),
                        cleanup_evidence: None,
                        operation_complete: true,
                    }
                }
                (ReconcileAction::Restart, ObservedEnvironmentState::Provisioning) => {
                    ProviderObservation {
                        next_state: ObservedEnvironmentState::Ready,
                        endpoints: vec![EnvironmentEndpoint {
                            id: EndpointId::new(),
                            protocol: EndpointProtocol::Https,
                            revision: next_revision,
                            health: EndpointHealth::Healthy,
                            observed_at: timestamp("2026-07-14T00:03:00.000Z"),
                        }],
                        cleanup_evidence: None,
                        operation_complete: true,
                    }
                }
                (ReconcileAction::Cleanup, ObservedEnvironmentState::Deleting) => {
                    ProviderObservation {
                        next_state: ObservedEnvironmentState::Deleted,
                        endpoints: Vec::new(),
                        cleanup_evidence: Some(ArtifactRef {
                            artifact_id: ArtifactId::new(),
                            store_binding: "environment-cleanup-evidence-v1".to_owned(),
                            object_version: instance.operation.id.to_string(),
                            size_bytes: 1,
                            media_type: "application/json".to_owned(),
                        }),
                        operation_complete: true,
                    }
                }
                _ => {
                    return Err(ProviderFailure {
                        code: ProviderFailureCode::Rejected,
                        retryable: false,
                    });
                }
            };
            Ok(observation)
        }
        .await
        .map(environment_service::ProviderOutcome::Completed)
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
    ) -> Result<environment_service::ProviderOutcome<ProviderObservation>, ProviderFailure> {
        async {
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
        .await
        .map(environment_service::ProviderOutcome::Completed)
    }
}

struct BlockingKubeVirtExecutor {
    calls: Arc<AtomicUsize>,
    entered: Arc<Notify>,
    dropped: Arc<AtomicBool>,
    observed_at: UtcTimestamp,
}
struct ExecutionDrop(Arc<AtomicBool>);
impl Drop for ExecutionDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
#[async_trait]
impl KubeVirtExecutorBackend for BlockingKubeVirtExecutor {
    async fn execute(
        &self,
        fence: &KubeVirtBackendFence,
        request: &KubeVirtExecutorRequest,
        permit: &KubeVirtExecutionPermit,
    ) -> KubeVirtExecutorResponse {
        if matches!(request, KubeVirtExecutorRequest::Apply { .. }) {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let _drop = ExecutionDrop(Arc::clone(&self.dropped));
            self.entered.notify_one();
            std::future::pending::<()>().await;
        }
        CountingKubeVirtExecutor {
            calls: Arc::clone(&self.calls),
            observed_at: self.observed_at,
        }
        .execute(fence, request, permit)
        .await
    }
}

#[tokio::test]
async fn kubevirt_pending_cancellation_and_timeout_preserve_exact_execution()
-> Result<(), Box<dyn std::error::Error>> {
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(6)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            container.get_host_port_ipv4(5432).await?
        ))
        .await?;
    support::apply_environment_migrations(&pool).await?;
    let store = PgEnvironmentStore::new(pool.clone());
    let now = store.current_time().await?;
    let mut authority = requested_instance();
    authority.operation.accepted_at = now;
    authority.operation.next_attempt_at = now;
    authority.operation.deadline_at = container_add_time(now, time::Duration::seconds(5))?;
    authority.eligibility_expires_at = container_add_time(now, time::Duration::minutes(2))?;
    store.create("pending-authority", &authority).await?;
    let calls = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(Notify::new());
    let dropped = Arc::new(AtomicBool::new(false));
    let instance = executor_instance();
    let executor = Arc::new(FencedKubeVirtExecutor::new(
        PgKubeVirtExecutorFenceStore::new(pool.clone()),
        BlockingKubeVirtExecutor {
            calls: Arc::clone(&calls),
            entered: Arc::clone(&entered),
            dropped: Arc::clone(&dropped),
            observed_at: now,
        },
        instance.clone(),
    ));
    let plan = kubevirt_executor_plan(authority.id);
    let first = kubevirt_executor_envelope(
        plan.clone(),
        authority.operation.id,
        1,
        1,
        ReconcileAction::Provision,
        authority.operation.deadline_at,
    )?;
    let task = {
        let executor = Arc::clone(&executor);
        let first = first.clone();
        tokio::spawn(async move { executor.execute(first).await })
    };
    entered.notified().await;
    assert!(matches!(
        executor.execute(first.clone()).await?.response,
        KubeVirtExecutorResponse::Pending
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let unchanged = store.load(authority.id).await?;
    assert_eq!(unchanged, authority);
    let accepted_at = store.current_time().await?;
    let accepted = store
        .accept_command(
            "pending-delete",
            &LifecycleCommand {
                environment_id: authority.id,
                kind: EnvironmentOperationKind::Delete,
                expected_revision: authority.revision,
                actor_id: authority.owner_id,
                trace_id: "pending-delete".to_owned(),
                accepted_at,
                deadline_at: container_add_time(accepted_at, time::Duration::seconds(5))?,
                access_revocation_revision: Some(revision(2)),
                preserve_mutable_disk: false,
                max_attempts: 3,
                reset_target: None,
            },
        )
        .await?;
    let current = store.load(authority.id).await?;
    assert_eq!(current.operation.id, accepted.operation_id);
    let cleanup = kubevirt_executor_envelope(
        plan,
        accepted.operation_id,
        current.generation,
        1,
        ReconcileAction::Cleanup,
        current.operation.deadline_at,
    )?;
    assert!(matches!(
        executor.execute(cleanup.clone()).await?.response,
        KubeVirtExecutorResponse::Pending
    ));
    let response = tokio::time::timeout(Duration::from_secs(2), task).await???;
    assert!(matches!(
        response.response,
        KubeVirtExecutorResponse::Failed {
            failure: ProviderFailure {
                code: ProviderFailureCode::Cancelled,
                ..
            }
        }
    ));
    assert!(dropped.load(Ordering::SeqCst));
    let old_terminal: serde_json::Value = sqlx::query_scalar(
        "SELECT last_response FROM environment.kubevirt_executor_fences WHERE environment_id=$1",
    )
    .bind(authority.id.as_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(old_terminal["failure"]["code"], "cancelled");
    assert!(matches!(
        executor.execute(cleanup).await?.response,
        KubeVirtExecutorResponse::Deleted { .. }
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    let mut timed = requested_instance();
    let now = store.current_time().await?;
    timed.operation.accepted_at = now;
    timed.operation.next_attempt_at = now;
    timed.operation.deadline_at = container_add_time(now, time::Duration::milliseconds(350))?;
    timed.eligibility_expires_at = container_add_time(now, time::Duration::minutes(1))?;
    store.create("timeout-authority", &timed).await?;
    dropped.store(false, Ordering::SeqCst);
    let timeout_request = kubevirt_executor_envelope(
        kubevirt_executor_plan(timed.id),
        timed.operation.id,
        1,
        1,
        ReconcileAction::Provision,
        timed.operation.deadline_at,
    )?;
    let terminal = executor.execute(timeout_request.clone()).await?;
    assert!(matches!(
        terminal.response,
        KubeVirtExecutorResponse::Failed {
            failure: ProviderFailure {
                code: ProviderFailureCode::Timeout,
                ..
            }
        }
    ));
    assert!(dropped.load(Ordering::SeqCst));
    let count = calls.load(Ordering::SeqCst);
    assert_eq!(
        serde_json::to_value(executor.execute(timeout_request).await?.response)?,
        serde_json::to_value(terminal.response)?
    );
    assert_eq!(calls.load(Ordering::SeqCst), count);
    Ok(())
}

struct PendingProvider;
#[async_trait]
impl EnvironmentProvider for PendingProvider {
    fn binding(&self) -> &'static str {
        "container-primary-v1"
    }
    async fn execute(
        &self,
        _: ReconcileAction,
        _: &contracts::environment::EnvironmentInstance,
    ) -> Result<environment_service::ProviderOutcome<ProviderObservation>, ProviderFailure> {
        Ok(environment_service::ProviderOutcome::Pending)
    }
}
#[tokio::test]
async fn pending_reconcile_changes_only_schedule_without_retry_event_or_usage()
-> Result<(), Box<dyn std::error::Error>> {
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            container.get_host_port_ipv4(5432).await?
        ))
        .await?;
    support::apply_environment_migrations(&pool).await?;
    let store = PgEnvironmentStore::new(pool.clone());
    let now = store.current_time().await?;
    let mut instance = requested_instance();
    instance.operation.accepted_at = now;
    instance.operation.next_attempt_at = now;
    instance.operation.deadline_at = container_add_time(now, time::Duration::minutes(1))?;
    instance.eligibility_expires_at = instance.operation.deadline_at;
    store.create("pending-reconcile", &instance).await?;
    let before_events: i64 = sqlx::query_scalar("SELECT count(*) FROM environment.outbox_events")
        .fetch_one(&pool)
        .await?;
    let mut registry = ProviderRegistry::default();
    registry.register(Arc::new(PendingProvider))?;
    let worker = ReconcileWorker::new(
        store.clone(),
        Reconciler::new(registry, Duration::from_secs(1))?,
        Duration::from_secs(2),
        Duration::from_millis(10),
    )?;
    for _ in 0..2 {
        assert_eq!(
            worker
                .run_once("pending-worker", store.current_time().await?)
                .await?,
            ReconcileWorkerOutcome::Pending
        );
        let current = store.load(instance.id).await?;
        let mut expected = instance.clone();
        expected.operation.next_attempt_at = current.operation.next_attempt_at;
        assert_eq!(current, expected);
        assert!(current.operation.next_attempt_at <= instance.operation.deadline_at);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM environment.outbox_events")
            .fetch_one(&pool)
            .await?,
        before_events
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM environment.resource_meter_deliveries")
            .fetch_one(&pool)
            .await?,
        0
    );
    Ok(())
}

#[tokio::test]
async fn kubevirt_incarnation_recovery_and_terminal_commit_retry_do_not_repeat_effects()
-> Result<(), Box<dyn std::error::Error>> {
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(6)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            container.get_host_port_ipv4(5432).await?
        ))
        .await?;
    support::apply_environment_migrations(&pool).await?;
    let store = PgEnvironmentStore::new(pool.clone());
    let now = store.current_time().await?;
    let mut authority = requested_instance();
    authority.operation.accepted_at = now;
    authority.operation.next_attempt_at = now;
    authority.operation.deadline_at = container_add_time(now, time::Duration::minutes(1))?;
    authority.eligibility_expires_at = authority.operation.deadline_at;
    store.create("incarnation-recovery", &authority).await?;
    let calls = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(Notify::new());
    let dropped = Arc::new(AtomicBool::new(false));
    let instance = executor_instance();
    let backend = || BlockingKubeVirtExecutor {
        calls: Arc::clone(&calls),
        entered: Arc::clone(&entered),
        dropped: Arc::clone(&dropped),
        observed_at: now,
    };
    let original = Arc::new(FencedKubeVirtExecutor::new(
        PgKubeVirtExecutorFenceStore::new(pool.clone()),
        backend(),
        instance.clone(),
    ));
    let request = kubevirt_executor_envelope(
        kubevirt_executor_plan(authority.id),
        authority.operation.id,
        1,
        1,
        ReconcileAction::Provision,
        authority.operation.deadline_at,
    )?;
    let task = {
        let original = Arc::clone(&original);
        let request = request.clone();
        tokio::spawn(async move { original.execute(request).await })
    };
    entered.notified().await;
    task.abort();
    let _ = task.await;
    assert!(dropped.load(Ordering::SeqCst));
    drop(original);
    // A different Pod, even when the old Pod is no longer observable, cannot prove termination.
    let mut foreign = instance.clone();
    foreign.pod_uid = uuid::Uuid::new_v4();
    foreign.boot_token = uuid::Uuid::new_v4();
    let foreign = FencedKubeVirtExecutor::new(
        PgKubeVirtExecutorFenceStore::new(pool.clone()),
        backend(),
        foreign,
    );
    foreign.prepare_startup().await?;
    assert!(matches!(
        foreign.execute(request.clone()).await?.response,
        KubeVirtExecutorResponse::Pending
    ));
    assert_eq!(
        sqlx::query_scalar::<_, Option<serde_json::Value>>(
            "SELECT last_response FROM environment.kubevirt_executor_fences WHERE environment_id=$1"
        )
        .bind(authority.id.as_uuid())
        .fetch_one(&pool)
        .await?,
        None
    );
    // This fixture models the next isolated PID1 boot of that same container, not Pod disappearance.
    let mut replacement = instance.clone();
    replacement.boot_token = uuid::Uuid::new_v4();
    let replacement = FencedKubeVirtExecutor::new(
        PgKubeVirtExecutorFenceStore::new(pool.clone()),
        backend(),
        replacement,
    );
    replacement.prepare_startup().await?;
    assert!(matches!(
        replacement.execute(request).await?.response,
        KubeVirtExecutorResponse::Failed {
            failure: ProviderFailure {
                code: ProviderFailureCode::Cancelled,
                ..
            }
        }
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let mut second = requested_instance();
    second.operation.accepted_at = now;
    second.operation.next_attempt_at = now;
    second.operation.deadline_at = authority.operation.deadline_at;
    second.eligibility_expires_at = authority.operation.deadline_at;
    store.create("terminal-commit-retry", &second).await?;
    let executor = FencedKubeVirtExecutor::new(
        PgKubeVirtExecutorFenceStore::new(pool.clone()),
        CountingKubeVirtExecutor {
            calls: Arc::clone(&calls),
            observed_at: now,
        },
        instance,
    );
    sqlx::raw_sql("CREATE FUNCTION environment.reject_terminal_once() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'terminal persistence unavailable'; END $$; CREATE TRIGGER reject_terminal BEFORE UPDATE ON environment.kubevirt_executor_fences FOR EACH ROW WHEN (NEW.last_response IS NOT NULL) EXECUTE FUNCTION environment.reject_terminal_once();").execute(&pool).await?;
    let request = kubevirt_executor_envelope(
        kubevirt_executor_plan(second.id),
        second.operation.id,
        1,
        1,
        ReconcileAction::Provision,
        second.operation.deadline_at,
    )?;
    assert!(matches!(
        executor.execute(request.clone()).await?.response,
        KubeVirtExecutorResponse::Pending
    ));
    let count = calls.load(Ordering::SeqCst);
    assert_eq!(
        sqlx::query_scalar::<_, Option<serde_json::Value>>(
            "SELECT last_response FROM environment.kubevirt_executor_fences WHERE environment_id=$1"
        )
        .bind(second.id.as_uuid())
        .fetch_one(&pool)
        .await?,
        None
    );
    sqlx::query("DROP TRIGGER reject_terminal ON environment.kubevirt_executor_fences")
        .execute(&pool)
        .await?;
    assert!(matches!(
        executor.execute(request.clone()).await?.response,
        KubeVirtExecutorResponse::Running { .. }
    ));
    assert_eq!(calls.load(Ordering::SeqCst), count);
    // Admission deadline is re-read after the database row lock, never before it.
    let mut transaction = pool.begin().await?;
    sqlx::query("SELECT environment_id FROM environment.kubevirt_executor_fences WHERE environment_id=$1 FOR UPDATE").bind(second.id.as_uuid()).fetch_one(&mut *transaction).await?;
    let deadline = container_add_time(
        store.current_time().await?,
        time::Duration::milliseconds(100),
    )?;
    let next = kubevirt_executor_envelope(
        kubevirt_executor_plan(second.id),
        second.operation.id,
        1,
        2,
        ReconcileAction::Provision,
        deadline,
    )?;
    let executor = Arc::new(executor);
    let task = {
        let executor = Arc::clone(&executor);
        tokio::spawn(async move { executor.execute(next).await })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;
    transaction.commit().await?;
    assert!(matches!(
        task.await?,
        Err(KubeVirtExecutorFenceError::DeadlineExceeded)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), count);
    Ok(())
}

#[tokio::test]
async fn kubevirt_server_shutdown_drops_and_commits_accepted_backend_before_exit()
-> Result<(), Box<dyn std::error::Error>> {
    use testcontainers::{
        GenericImage,
        core::{IntoContainerPort, WaitFor},
    };
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            container.get_host_port_ipv4(5432).await?
        ))
        .await?;
    support::apply_environment_migrations(&pool).await?;
    let store = PgEnvironmentStore::new(pool.clone());
    let now = store.current_time().await?;
    let mut authority = requested_instance();
    authority.operation.accepted_at = now;
    authority.operation.next_attempt_at = now;
    authority.operation.deadline_at = container_add_time(now, time::Duration::minutes(1))?;
    authority.eligibility_expires_at = authority.operation.deadline_at;
    store.create("graceful-drain", &authority).await?;
    let entered = Arc::new(Notify::new());
    let dropped = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(AtomicUsize::new(0));
    let executor = FencedKubeVirtExecutor::new(
        PgKubeVirtExecutorFenceStore::new(pool.clone()),
        BlockingKubeVirtExecutor {
            calls: Arc::clone(&calls),
            entered: Arc::clone(&entered),
            dropped: Arc::clone(&dropped),
            observed_at: now,
        },
        executor_instance(),
    );
    executor.prepare_startup().await?;
    let nats = GenericImage::new("nats", "2.11.8-alpine")
        .with_exposed_port(4222.tcp())
        .with_wait_for(WaitFor::message_on_stderr("Server is ready"))
        .start()
        .await?;
    let client = async_nats::connect(format!(
        "nats://127.0.0.1:{}",
        nats.get_host_port_ipv4(4222).await?
    ))
    .await?;
    let server = NatsKubeVirtExecutorServer::new(
        client.clone(),
        "fixture.kubevirt.drain".to_owned(),
        executor,
    )?;
    let (shutdown, receiver) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(async move { server.serve(receiver).await });
    // Subscription readiness is observed through NATS, not an arbitrary sleep.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let response = client
                .send_request(
                    "fixture.kubevirt.drain",
                    async_nats::Request::new()
                        .timeout(Some(Duration::from_millis(50)))
                        .payload(b"invalid-contract".to_vec().into()),
                )
                .await;
            if !response.is_err_and(|error| {
                error.kind() == async_nats::client::RequestErrorKind::NoResponders
            }) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await?;
    let request = kubevirt_executor_envelope(
        kubevirt_executor_plan(authority.id),
        authority.operation.id,
        1,
        1,
        ReconcileAction::Provision,
        authority.operation.deadline_at,
    )?;
    let payload = serde_json::to_vec(&request)?;
    let request_task = tokio::spawn(async move {
        client
            .request("fixture.kubevirt.drain", payload.into())
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), entered.notified()).await?;
    shutdown.send_replace(true);
    let response = request_task.await??;
    let response: environment_service::KubeVirtExecutorResponseEnvelope =
        serde_json::from_slice(&response.payload)?;
    assert!(matches!(
        response.response,
        KubeVirtExecutorResponse::Failed {
            failure: ProviderFailure {
                code: ProviderFailureCode::Cancelled,
                ..
            }
        }
    ));
    tokio::time::timeout(Duration::from_secs(2), task).await???;
    assert!(dropped.load(Ordering::SeqCst));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let terminal: serde_json::Value = sqlx::query_scalar(
        "SELECT last_response FROM environment.kubevirt_executor_fences WHERE environment_id=$1",
    )
    .bind(authority.id.as_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(terminal["failure"]["code"], "cancelled");
    Ok(())
}
