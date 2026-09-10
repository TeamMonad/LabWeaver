//! `PostgreSQL` coverage for durable Work execution recovery boundaries.

mod support;

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use contracts::authoring::{
    AgentRunState, WorkConfigurationPlan, WorkConfigurationPreauthorization,
};
use contracts::environment::EnvironmentLeaseAuthorization;
use contracts::http::{
    ContainerWorkExecutionReceipt, ContainerWorkExecutionRequest, ContainerWorkExecutionState,
    WorkConfigurationAdmissionBinding, WorkConfigurationAdmissionQuery,
};
use contracts::resource::WorkloadResources;
use contracts::{
    AgentRunId, ArtifactId, ArtifactRef, LeaseId, ResourceRequestId, Revision, UtcTimestamp,
    WorkConfigurationPlanId, WorkConfigurationPreauthorizationId,
};
use environment_service::{
    ContainerWorkExecutionBackend, ContainerWorkExecutionService, ContainerWorkExecutionTarget,
    PgEnvironmentStore, WorkAdmissionClientError, WorkAdmissionResolver, WorkExecutionError,
    WorkExecutionOutcome,
};
use sqlx::postgres::PgPoolOptions;
use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;
use tokio::sync::Notify;
use uuid::Uuid;

const TEST_TARGET_POD_UID: &str = "pod-uid";

#[tokio::test]
async fn start_checks_admission_and_durably_fences_before_backend_execution()
-> Result<(), Box<dyn std::error::Error>> {
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let database_url = format!(
        "postgres://postgres:postgres@127.0.0.1:{}/postgres",
        container.get_host_port_ipv4(5432).await?
    );
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&database_url)
        .await?;
    support::apply_environment_migrations(&pool).await?;

    let instance = create_eligible_environment(&pool, "work-execution-start").await?;
    let request = request_for(&instance);
    let admission = StaticAdmission::new(admission_for(&request));
    let admission_calls = admission.calls.clone();
    let backend = Arc::new(TestBackend::new(
        pool.clone(),
        ExecuteBehavior::Blocked,
        ObserveBehavior::None,
    ));
    let service = ContainerWorkExecutionService::new_with_admission_resolver(
        PgEnvironmentStore::new(pool.clone()),
        pool.clone(),
        admission,
        backend.clone(),
    );

    let accepted = service.start(request.clone()).await?;
    assert_eq!(accepted.state, ContainerWorkExecutionState::Running);
    assert_eq!(backend.execute_calls.load(Ordering::SeqCst), 0);

    let persisted_request: serde_json::Value = sqlx::query_scalar(
        "SELECT request_json
         FROM environment.work_configuration_executions
         WHERE run_id=$1",
    )
    .bind(request.run_id.as_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(persisted_request, serde_json::to_value(&request)?);

    // An idempotent retry reads the durable intent and does not ask Control for
    // a second admission while the original worker is still in flight.
    let replay = service.start(request.clone()).await?;
    assert_eq!(replay.execution_id, accepted.execution_id);
    assert_eq!(admission_calls.load(Ordering::SeqCst), 1);
    assert_eq!(backend.execute_calls.load(Ordering::SeqCst), 0);

    backend.execute_started.notified().await;
    assert!(backend.durable_before_execute.load(Ordering::SeqCst));
    assert_eq!(backend.execute_calls.load(Ordering::SeqCst), 1);

    backend.release_execute.notify_one();
    let terminal = wait_for_state(
        &service,
        request.run_id,
        &query_for(&request),
        ContainerWorkExecutionState::Succeeded,
    )
    .await?;
    assert_eq!(terminal.exit_code, Some(0));
    Ok(())
}

#[tokio::test]
async fn prestart_runner_failure_is_terminalized_after_cancel_confirmation()
-> Result<(), Box<dyn std::error::Error>> {
    let (pool, _container) = test_database().await?;
    let instance = create_eligible_environment(&pool, "work-execution-prestart-failure").await?;
    let request = request_for(&instance);
    let admission = StaticAdmission::new(admission_for(&request));
    let backend = Arc::new(
        TestBackend::new(
            pool.clone(),
            ExecuteBehavior::Error(BackendError::RunnerFailed),
            ObserveBehavior::Error(BackendError::TargetIdentityChanged),
        )
        .with_prestart_diagnostic("LW_ENVIRONMENT_WORK_EXECUTION_TOOL_MISSING"),
    );
    let service = ContainerWorkExecutionService::new_with_admission_resolver(
        PgEnvironmentStore::new(pool.clone()),
        pool.clone(),
        admission,
        backend.clone(),
    );

    let accepted = service.start(request.clone()).await?;
    assert_eq!(accepted.state, ContainerWorkExecutionState::Running);
    let failed = wait_for_state(
        &service,
        request.run_id,
        &query_for(&request),
        ContainerWorkExecutionState::Failed,
    )
    .await?;
    assert_eq!(
        failed
            .diagnostic_code
            .as_ref()
            .map(contracts::DiagnosticCode::as_str),
        Some("LW_ENVIRONMENT_WORK_EXECUTION_TOOL_MISSING")
    );
    assert!(failed.started_at.is_some());
    assert!(failed.finished_at.is_some());
    assert_eq!(backend.cancel_calls.load(Ordering::SeqCst), 1);
    assert_eq!(backend.observe_calls.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn prestart_failure_does_not_terminalize_when_cancel_confirmation_fails()
-> Result<(), Box<dyn std::error::Error>> {
    let (pool, _container) = test_database().await?;
    let instance =
        create_eligible_environment(&pool, "work-execution-prestart-cancel-fails").await?;
    let request = request_for(&instance);
    let admission = StaticAdmission::new(admission_for(&request));
    let backend = Arc::new(
        TestBackend::new(
            pool.clone(),
            ExecuteBehavior::Error(BackendError::RunnerFailed),
            ObserveBehavior::Outcome(WorkExecutionOutcome {
                exit_code: 0,
                verification_exit_code: None,
                output: "must not observe after failed cancel".to_owned(),
                output_truncated: false,
            }),
        )
        .with_cancel_error(BackendError::RunnerFailed)
        .with_prestart_diagnostic("LW_ENVIRONMENT_WORK_EXECUTION_TOOL_MISSING"),
    );
    let service = ContainerWorkExecutionService::new_with_admission_resolver(
        PgEnvironmentStore::new(pool.clone()),
        pool.clone(),
        admission,
        backend.clone(),
    );

    let accepted = service.start(request.clone()).await?;
    assert_eq!(accepted.state, ContainerWorkExecutionState::Running);
    let pending = wait_for_state(
        &service,
        request.run_id,
        &query_for(&request),
        ContainerWorkExecutionState::CleanupPending,
    )
    .await?;
    assert_eq!(
        pending
            .diagnostic_code
            .as_ref()
            .map(contracts::DiagnosticCode::as_str),
        Some("LW_ENVIRONMENT_WORK_EXECUTION_RUNNER_FAILED")
    );
    assert!(pending.started_at.is_some());
    assert!(pending.finished_at.is_none());
    assert_eq!(backend.cancel_calls.load(Ordering::SeqCst), 1);
    assert_eq!(backend.observe_calls.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn cleanup_pending_prestart_failure_is_terminalized_after_cancel_confirmation()
-> Result<(), Box<dyn std::error::Error>> {
    let (pool, _container) = test_database().await?;
    let instance = create_eligible_environment(&pool, "work-execution-cleanup-prestart").await?;
    let request = request_for(&instance);
    let target = target();
    let receipt = receipt_for(
        &request,
        &target,
        ContainerWorkExecutionState::CleanupPending,
        true,
    );
    insert_execution(&pool, &request, &target, &receipt, 1).await?;

    let backend = Arc::new(
        TestBackend::new(
            pool.clone(),
            ExecuteBehavior::Error(BackendError::RunnerFailed),
            ObserveBehavior::Error(BackendError::TargetIdentityChanged),
        )
        .with_prestart_diagnostic("LW_ENVIRONMENT_WORK_EXECUTION_TOOL_MISSING"),
    );
    let service = ContainerWorkExecutionService::new_with_admission_resolver(
        PgEnvironmentStore::new(pool.clone()),
        pool.clone(),
        UnusedAdmission,
        backend.clone(),
    );

    let failed = service.recover_once(request.run_id).await?;
    assert_eq!(failed.state, ContainerWorkExecutionState::Failed);
    assert_eq!(
        failed
            .diagnostic_code
            .as_ref()
            .map(contracts::DiagnosticCode::as_str),
        Some("LW_ENVIRONMENT_WORK_EXECUTION_TOOL_MISSING")
    );
    assert!(failed.finished_at.is_some());
    assert_eq!(backend.cancel_calls.load(Ordering::SeqCst), 1);
    assert_eq!(backend.observe_calls.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn stale_environment_revision_is_rejected_before_backend_execution()
-> Result<(), Box<dyn std::error::Error>> {
    let (pool, _container) = test_database().await?;
    let instance =
        create_eligible_environment(&pool, "work-execution-stale-environment-revision").await?;
    let mut request = request_for(&instance);
    request.environment_revision = Revision::new(1)?;
    let admission = StaticAdmission::new(admission_for(&request));
    let admission_calls = admission.calls.clone();
    let backend = Arc::new(TestBackend::new(
        pool.clone(),
        ExecuteBehavior::Error(BackendError::RunnerFailed),
        ObserveBehavior::None,
    ));
    let service = ContainerWorkExecutionService::new_with_admission_resolver(
        PgEnvironmentStore::new(pool.clone()),
        pool,
        admission,
        backend.clone(),
    );

    let result = service.start(request).await;
    assert!(matches!(
        result,
        Err(WorkExecutionError::EnvironmentNotEligible)
    ));
    assert_eq!(admission_calls.load(Ordering::SeqCst), 0);
    assert_eq!(backend.execute_calls.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn cancellation_before_start_fence_never_calls_backend_process_operations()
-> Result<(), Box<dyn std::error::Error>> {
    let (pool, _container) = test_database().await?;
    let instance =
        create_eligible_environment(&pool, "work-execution-cancel-before-worker").await?;
    let request = request_for(&instance);
    let target = target();
    let receipt = receipt_for(
        &request,
        &target,
        ContainerWorkExecutionState::Running,
        false,
    );
    insert_execution(&pool, &request, &target, &receipt, 1).await?;

    let backend = Arc::new(TestBackend::new(
        pool.clone(),
        ExecuteBehavior::Error(BackendError::RunnerFailed),
        ObserveBehavior::None,
    ));
    let service = ContainerWorkExecutionService::new_with_admission_resolver(
        PgEnvironmentStore::new(pool.clone()),
        pool.clone(),
        UnusedAdmission,
        backend.clone(),
    );

    let cancelled = service.cancel(request.run_id, &query_for(&request)).await?;
    assert_eq!(cancelled.state, ContainerWorkExecutionState::Cancelled);
    assert!(cancelled.finished_at.is_some());
    assert_eq!(backend.execute_calls.load(Ordering::SeqCst), 0);
    assert_eq!(backend.cancel_calls.load(Ordering::SeqCst), 0);
    let persisted = service.query(request.run_id, &query_for(&request)).await?;
    assert_eq!(persisted.state, ContainerWorkExecutionState::Cancelled);
    Ok(())
}

#[tokio::test]
async fn restart_reconciles_persisted_execution_by_observing_without_reexecuting()
-> Result<(), Box<dyn std::error::Error>> {
    let (pool, _container) = test_database().await?;
    let instance = create_eligible_environment(&pool, "work-execution-restart-observe").await?;
    let request = request_for(&instance);
    let target = target();
    let receipt = receipt_for(
        &request,
        &target,
        ContainerWorkExecutionState::Running,
        true,
    );
    insert_execution(&pool, &request, &target, &receipt, 1).await?;

    let backend = Arc::new(TestBackend::new(
        pool.clone(),
        ExecuteBehavior::Error(BackendError::RunnerFailed),
        ObserveBehavior::Outcome(WorkExecutionOutcome {
            exit_code: 0,
            verification_exit_code: None,
            output: "recovered".to_owned(),
            output_truncated: false,
        }),
    ));
    let service = ContainerWorkExecutionService::new_with_admission_resolver(
        PgEnvironmentStore::new(pool.clone()),
        pool.clone(),
        UnusedAdmission,
        backend.clone(),
    );

    let recovered = service.recover_once(request.run_id).await?;
    assert_eq!(recovered.state, ContainerWorkExecutionState::Succeeded);
    assert_eq!(recovered.output, "recovered");
    assert_eq!(backend.execute_calls.load(Ordering::SeqCst), 0);
    assert_eq!(backend.cancel_calls.load(Ordering::SeqCst), 0);
    let persisted = service.query(request.run_id, &query_for(&request)).await?;
    assert_eq!(persisted.state, ContainerWorkExecutionState::Succeeded);
    Ok(())
}

#[tokio::test]
async fn stale_recovery_cas_does_not_overwrite_a_concurrent_cancellation_fence()
-> Result<(), Box<dyn std::error::Error>> {
    let (pool, _container) = test_database().await?;
    let instance = create_eligible_environment(&pool, "work-execution-stale-cas").await?;
    let request = request_for(&instance);
    let target = target();
    let receipt = receipt_for(
        &request,
        &target,
        ContainerWorkExecutionState::Running,
        true,
    );
    insert_execution(&pool, &request, &target, &receipt, 1).await?;

    let backend = Arc::new(TestBackend::new(
        pool.clone(),
        ExecuteBehavior::Error(BackendError::RunnerFailed),
        ObserveBehavior::Blocked(WorkExecutionOutcome {
            exit_code: 0,
            verification_exit_code: None,
            output: "late observation".to_owned(),
            output_truncated: false,
        }),
    ));
    let service = ContainerWorkExecutionService::new_with_admission_resolver(
        PgEnvironmentStore::new(pool.clone()),
        pool.clone(),
        UnusedAdmission,
        backend.clone(),
    );

    let recovery = {
        let service = service.clone();
        tokio::spawn(async move { service.recover_once(request.run_id).await })
    };
    backend.observe_started.notified().await;

    let mut cancelling = receipt.clone();
    cancelling.state = ContainerWorkExecutionState::Cancelling;
    sqlx::query(
        "UPDATE environment.work_configuration_executions
         SET receipt_json=$2, revision=revision+1
         WHERE run_id=$1 AND revision=1",
    )
    .bind(request.run_id.as_uuid())
    .bind(serde_json::to_value(&cancelling)?)
    .execute(&pool)
    .await?;

    backend.release_observe.notify_one();
    let result = recovery.await?;
    assert!(matches!(
        result,
        Err(WorkExecutionError::ConcurrentMutation)
    ));
    let persisted = service.query(request.run_id, &query_for(&request)).await?;
    assert_eq!(persisted.state, ContainerWorkExecutionState::Cancelling);
    assert_eq!(persisted.output, "");
    assert_eq!(backend.execute_calls.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn replaced_pod_identity_fails_recovery_without_reexecuting()
-> Result<(), Box<dyn std::error::Error>> {
    let (pool, _container) = test_database().await?;
    let instance = create_eligible_environment(&pool, "work-execution-pod-replaced").await?;
    let request = request_for(&instance);
    let target = target();
    let receipt = receipt_for(
        &request,
        &target,
        ContainerWorkExecutionState::Running,
        true,
    );
    insert_execution(&pool, &request, &target, &receipt, 1).await?;

    let backend = Arc::new(TestBackend::new(
        pool.clone(),
        ExecuteBehavior::Error(BackendError::RunnerFailed),
        ObserveBehavior::Error(BackendError::TargetIdentityChanged),
    ));
    let service = ContainerWorkExecutionService::new_with_admission_resolver(
        PgEnvironmentStore::new(pool.clone()),
        pool.clone(),
        UnusedAdmission,
        backend.clone(),
    );

    let result = service.recover_once(request.run_id).await;
    assert!(matches!(
        result,
        Err(WorkExecutionError::TargetIdentityChanged)
    ));
    let persisted = service.query(request.run_id, &query_for(&request)).await?;
    assert_eq!(persisted.state, ContainerWorkExecutionState::Failed);
    assert_eq!(
        persisted
            .diagnostic_code
            .as_ref()
            .map(contracts::DiagnosticCode::as_str),
        Some("LW_ENVIRONMENT_WORK_EXECUTION_TARGET_IDENTITY_CHANGED")
    );
    assert!(persisted.finished_at.is_some());
    assert_eq!(backend.execute_calls.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn malformed_observation_retains_cleanup_fence_for_started_execution()
-> Result<(), Box<dyn std::error::Error>> {
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let database_url = format!(
        "postgres://postgres:postgres@127.0.0.1:{}/postgres",
        container.get_host_port_ipv4(5432).await?
    );
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&database_url)
        .await?;
    support::apply_environment_migrations(&pool).await?;

    let instance = support::requested_instance();
    PgEnvironmentStore::new(pool.clone())
        .create("work-execution-environment", &instance)
        .await?;

    let run_id = AgentRunId::new();
    let plan_id = WorkConfigurationPlanId::new();
    let execution_id = Uuid::now_v7();
    let request = ContainerWorkExecutionRequest {
        run_id,
        run_revision: Revision::new(1)?,
        plan_id,
        plan_revision: Revision::new(1)?,
        project_id: instance.project_id,
        course_id: instance.course_id,
        environment_id: instance.id,
        environment_revision: instance.revision,
        actor_id: instance.owner_id,
        script_content: "true".to_owned(),
        verification_script_content: None,
        deadline_at: timestamp("2099-01-01T00:00:00.000Z"),
    };
    let target = ContainerWorkExecutionTarget {
        namespace: "test".to_owned(),
        pod_name: "runtime".to_owned(),
        pod_uid: "pod-uid".to_owned(),
        container: "runtime".to_owned(),
        workdir: "/workspace".to_owned(),
    };
    let receipt = ContainerWorkExecutionReceipt {
        execution_id,
        run_id,
        plan_id,
        plan_revision: Revision::new(1)?,
        environment_id: instance.id,
        environment_revision: instance.revision,
        target_pod_uid: target.pod_uid.clone(),
        state: ContainerWorkExecutionState::Running,
        exit_code: None,
        verification_exit_code: None,
        output: String::new(),
        output_truncated: false,
        diagnostic_code: None,
        started_at: Some(timestamp("2026-09-08T00:00:00.000Z")),
        finished_at: None,
    };
    sqlx::query(
        "INSERT INTO environment.work_configuration_executions
         (run_id,execution_id,project_id,environment_id,plan_id,plan_revision,
          request_json,target_json,receipt_json,revision)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,1)",
    )
    .bind(run_id.as_uuid())
    .bind(execution_id)
    .bind(instance.project_id.as_uuid())
    .bind(instance.id.as_uuid())
    .bind(plan_id.as_uuid())
    .bind(i64::try_from(receipt.plan_revision.get())?)
    .bind(serde_json::to_value(&request)?)
    .bind(serde_json::to_value(&target)?)
    .bind(serde_json::to_value(&receipt)?)
    .execute(&pool)
    .await?;

    let backend = Arc::new(MalformedObservationBackend::default());
    let service = ContainerWorkExecutionService::new_with_admission_resolver(
        PgEnvironmentStore::new(pool.clone()),
        pool.clone(),
        UnusedAdmission,
        backend.clone(),
    );

    let result = service.recover_once(run_id).await;
    assert!(matches!(
        result,
        Err(WorkExecutionError::ObservationInvalid)
    ));

    let persisted: serde_json::Value = sqlx::query_scalar(
        "SELECT receipt_json
         FROM environment.work_configuration_executions
         WHERE run_id=$1",
    )
    .bind(run_id.as_uuid())
    .fetch_one(&pool)
    .await?;
    let persisted: ContainerWorkExecutionReceipt = serde_json::from_value(persisted)?;
    assert_eq!(persisted.state, ContainerWorkExecutionState::CleanupPending);
    assert_eq!(
        persisted
            .diagnostic_code
            .as_ref()
            .map(contracts::DiagnosticCode::as_str),
        Some("LW_ENVIRONMENT_WORK_EXECUTION_RECEIPT_INVALID")
    );
    assert!(persisted.started_at.is_some());
    assert!(persisted.finished_at.is_none());
    assert_eq!(
        backend.cancel_calls.load(Ordering::SeqCst),
        0,
        "malformed observation leaves the still-live process behind the cleanup fence"
    );
    Ok(())
}

async fn test_database()
-> Result<(sqlx::PgPool, testcontainers::ContainerAsync<Postgres>), Box<dyn std::error::Error>> {
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let database_url = format!(
        "postgres://postgres:postgres@127.0.0.1:{}/postgres",
        container.get_host_port_ipv4(5432).await?
    );
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&database_url)
        .await?;
    support::apply_environment_migrations(&pool).await?;
    Ok((pool, container))
}

#[allow(clippy::expect_used)]
fn eligible_work_instance() -> contracts::environment::EnvironmentInstance {
    let mut instance = support::ready_instance();
    let lease_id = LeaseId::new();
    instance.class = contracts::authoring::EnvironmentClass::Work;
    instance.lease_id = Some(lease_id);
    instance.capacity_binding = Some("cpu-standard-v1".to_owned());
    instance.eligibility_expires_at = timestamp("2099-01-01T00:00:00.000Z");
    instance.operation.lease_authorization = Some(EnvironmentLeaseAuthorization {
        resource_request_id: ResourceRequestId::new(),
        lease_id,
        lease_revision: Revision::new(1).expect("valid lease revision"),
        environment_id: instance.id,
        project_id: instance.project_id,
        course_id: instance.course_id,
        owner_actor_id: instance.owner_id,
        capacity_binding: "cpu-standard-v1".to_owned(),
        approved_resources: WorkloadResources {
            cpu_millicores: 1_000,
            memory_bytes: 1_073_741_824,
            storage_bytes: 1_073_741_824,
            gpu: None,
        },
        gpu_allocation: None,
        active_from: timestamp("2026-01-01T00:00:00.000Z"),
        expires_at: timestamp("2099-01-01T00:00:00.000Z"),
    });
    instance
}

#[allow(clippy::expect_used)]
async fn create_eligible_environment(
    pool: &sqlx::PgPool,
    idempotency_key: &str,
) -> Result<contracts::environment::EnvironmentInstance, Box<dyn std::error::Error>> {
    let ready = eligible_work_instance();
    let mut requested = ready.clone();
    requested.observed_state = contracts::environment::ObservedEnvironmentState::Requested;
    requested.observed_generation = 0;
    requested.revision = Revision::new(1).expect("valid requested revision");
    requested.operation.state = contracts::environment::OperationState::Accepted;
    requested.operation.accepted_revision = requested.revision;
    requested.operation.provider_step = 1;
    requested.operation.attempt = 1;
    requested.endpoints.clear();
    PgEnvironmentStore::new(pool.clone())
        .create(idempotency_key, &requested)
        .await?;

    sqlx::query(
        "UPDATE environment.environment_instances
         SET observed_state='ready', observed_generation=1, revision=2, contract=$2
         WHERE environment_id=$1",
    )
    .bind(ready.id.as_uuid())
    .bind(serde_json::to_value(&ready)?)
    .execute(pool)
    .await?;
    Ok(ready)
}

#[allow(clippy::expect_used)]
fn request_for(
    instance: &contracts::environment::EnvironmentInstance,
) -> ContainerWorkExecutionRequest {
    ContainerWorkExecutionRequest {
        run_id: AgentRunId::new(),
        run_revision: Revision::new(1).expect("valid run revision"),
        plan_id: WorkConfigurationPlanId::new(),
        plan_revision: Revision::new(1).expect("valid plan revision"),
        project_id: instance.project_id,
        course_id: instance.course_id,
        environment_id: instance.id,
        environment_revision: instance.revision,
        actor_id: instance.owner_id,
        script_content: "printf execution".to_owned(),
        verification_script_content: None,
        deadline_at: timestamp("2099-01-01T00:00:00.000Z"),
    }
}

#[allow(clippy::expect_used)]
fn admission_for(request: &ContainerWorkExecutionRequest) -> WorkConfigurationAdmissionBinding {
    let script_artifact = ArtifactRef {
        artifact_id: ArtifactId::new(),
        store_binding: "test-artifacts".to_owned(),
        object_version: "script-v1".to_owned(),
        size_bytes: request.script_content.len() as u64,
        media_type: "text/plain".to_owned(),
    };
    let plan = WorkConfigurationPlan {
        id: request.plan_id,
        revision: request.plan_revision,
        script_artifact: script_artifact.clone(),
        verification_script_artifact: None,
        summary: "test plan".to_owned(),
        requires_restart: false,
        environment_id: request.environment_id,
        environment_revision: request.environment_revision,
    };
    let preauthorization = WorkConfigurationPreauthorization {
        id: WorkConfigurationPreauthorizationId::new(),
        project_id: request.project_id,
        environment_id: request.environment_id,
        environment_revision: request.environment_revision,
        actor_id: request.actor_id,
        plan_id: request.plan_id,
        plan_revision: request.plan_revision,
        script_artifact,
        verification_script_artifact: None,
        expires_at: timestamp("2099-01-01T00:00:00.000Z"),
        revision: Revision::new(1).expect("valid preauthorization revision"),
    };
    WorkConfigurationAdmissionBinding {
        run_id: request.run_id,
        project_id: request.project_id,
        course_id: request.course_id,
        environment_id: request.environment_id,
        environment_revision: request.environment_revision,
        actor_id: request.actor_id,
        run_revision: request.run_revision,
        state: AgentRunState::Running,
        plan: Some(plan),
        preauthorization: Some(preauthorization),
        recovery: None,
        script_sha256: persistence_sqlx::Sha256Digest::of_bytes(request.script_content.as_bytes())
            .to_string(),
        verification_script_sha256: None,
    }
}

fn target() -> ContainerWorkExecutionTarget {
    ContainerWorkExecutionTarget {
        namespace: "test".to_owned(),
        pod_name: "runtime".to_owned(),
        pod_uid: TEST_TARGET_POD_UID.to_owned(),
        container: "runtime".to_owned(),
        workdir: "/workspace".to_owned(),
    }
}

fn receipt_for(
    request: &ContainerWorkExecutionRequest,
    target: &ContainerWorkExecutionTarget,
    state: ContainerWorkExecutionState,
    started: bool,
) -> ContainerWorkExecutionReceipt {
    ContainerWorkExecutionReceipt {
        execution_id: Uuid::now_v7(),
        run_id: request.run_id,
        plan_id: request.plan_id,
        plan_revision: request.plan_revision,
        environment_id: request.environment_id,
        environment_revision: request.environment_revision,
        target_pod_uid: target.pod_uid.clone(),
        state,
        exit_code: None,
        verification_exit_code: None,
        output: String::new(),
        output_truncated: false,
        diagnostic_code: None,
        started_at: started.then(|| timestamp("2026-09-08T00:00:00.000Z")),
        finished_at: None,
    }
}

fn query_for(
    request: &ContainerWorkExecutionRequest,
) -> contracts::http::ContainerWorkExecutionQuery {
    contracts::http::ContainerWorkExecutionQuery {
        project_id: request.project_id,
        environment_id: request.environment_id,
        plan_id: request.plan_id,
        plan_revision: request.plan_revision,
    }
}

async fn insert_execution(
    pool: &sqlx::PgPool,
    request: &ContainerWorkExecutionRequest,
    target: &ContainerWorkExecutionTarget,
    receipt: &ContainerWorkExecutionReceipt,
    revision: i64,
) -> Result<(), Box<dyn std::error::Error>> {
    sqlx::query(
        "INSERT INTO environment.work_configuration_executions
         (run_id,execution_id,project_id,environment_id,plan_id,plan_revision,
          request_json,target_json,receipt_json,revision)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
    )
    .bind(request.run_id.as_uuid())
    .bind(receipt.execution_id)
    .bind(request.project_id.as_uuid())
    .bind(request.environment_id.as_uuid())
    .bind(request.plan_id.as_uuid())
    .bind(i64::try_from(request.plan_revision.get())?)
    .bind(serde_json::to_value(request)?)
    .bind(serde_json::to_value(target)?)
    .bind(serde_json::to_value(receipt)?)
    .bind(revision)
    .execute(pool)
    .await?;
    Ok(())
}

async fn wait_for_state(
    service: &ContainerWorkExecutionService,
    run_id: AgentRunId,
    query: &contracts::http::ContainerWorkExecutionQuery,
    expected: ContainerWorkExecutionState,
) -> Result<ContainerWorkExecutionReceipt, Box<dyn std::error::Error>> {
    for _ in 0..100 {
        let receipt = service.query(run_id, query).await?;
        if receipt.state == expected {
            return Ok(receipt);
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    Err(std::io::Error::other(format!("execution {run_id} did not reach {expected:?}")).into())
}

#[allow(clippy::expect_used)]
fn timestamp(value: &str) -> UtcTimestamp {
    value.parse().expect("valid test timestamp")
}

struct StaticAdmission {
    binding: WorkConfigurationAdmissionBinding,
    calls: Arc<AtomicUsize>,
}

impl StaticAdmission {
    fn new(binding: WorkConfigurationAdmissionBinding) -> Self {
        Self {
            binding,
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }
}

#[async_trait]
impl WorkAdmissionResolver for StaticAdmission {
    async fn resolve(
        &self,
        run_id: AgentRunId,
        _query: &WorkConfigurationAdmissionQuery,
        _now: UtcTimestamp,
    ) -> Result<WorkConfigurationAdmissionBinding, WorkAdmissionClientError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.binding.run_id != run_id {
            return Err(WorkAdmissionClientError::ResponseInvalid);
        }
        Ok(self.binding.clone())
    }
}

#[derive(Clone, Copy)]
enum BackendError {
    RunnerFailed,
    TargetIdentityChanged,
}

impl BackendError {
    fn into_error(self) -> WorkExecutionError {
        match self {
            Self::RunnerFailed => WorkExecutionError::RunnerFailed,
            Self::TargetIdentityChanged => WorkExecutionError::TargetIdentityChanged,
        }
    }
}

enum ExecuteBehavior {
    Blocked,
    Error(BackendError),
}

enum ObserveBehavior {
    None,
    Error(BackendError),
    Outcome(WorkExecutionOutcome),
    Blocked(WorkExecutionOutcome),
}

struct TestBackend {
    pool: sqlx::PgPool,
    target: ContainerWorkExecutionTarget,
    execute_behavior: ExecuteBehavior,
    observe_behavior: ObserveBehavior,
    cancel_behavior: Option<BackendError>,
    prestart_diagnostic: Option<String>,
    execute_started: Notify,
    release_execute: Notify,
    observe_started: Notify,
    release_observe: Notify,
    durable_before_execute: std::sync::atomic::AtomicBool,
    execute_calls: AtomicUsize,
    cancel_calls: AtomicUsize,
    observe_calls: AtomicUsize,
}

impl TestBackend {
    fn new(
        pool: sqlx::PgPool,
        execute_behavior: ExecuteBehavior,
        observe_behavior: ObserveBehavior,
    ) -> Self {
        Self {
            pool,
            target: target(),
            execute_behavior,
            observe_behavior,
            cancel_behavior: None,
            prestart_diagnostic: None,
            execute_started: Notify::new(),
            release_execute: Notify::new(),
            observe_started: Notify::new(),
            release_observe: Notify::new(),
            durable_before_execute: std::sync::atomic::AtomicBool::new(false),
            execute_calls: AtomicUsize::new(0),
            cancel_calls: AtomicUsize::new(0),
            observe_calls: AtomicUsize::new(0),
        }
    }

    fn with_cancel_error(mut self, error: BackendError) -> Self {
        self.cancel_behavior = Some(error);
        self
    }

    fn with_prestart_diagnostic(mut self, diagnostic: &str) -> Self {
        self.prestart_diagnostic = Some(diagnostic.to_owned());
        self
    }
}

#[async_trait]
impl ContainerWorkExecutionBackend for TestBackend {
    async fn resolve_target(
        &self,
        _request: &ContainerWorkExecutionRequest,
    ) -> Result<ContainerWorkExecutionTarget, WorkExecutionError> {
        Ok(self.target.clone())
    }

    async fn execute(
        &self,
        _target: &ContainerWorkExecutionTarget,
        execution_id: Uuid,
        _script: &str,
        _verification_script: Option<&str>,
        _deadline_at: UtcTimestamp,
    ) -> Result<WorkExecutionOutcome, WorkExecutionError> {
        self.execute_calls.fetch_add(1, Ordering::SeqCst);
        let durable: Option<Uuid> = sqlx::query_scalar(
            "SELECT execution_id
             FROM environment.work_configuration_executions
             WHERE execution_id=$1",
        )
        .bind(execution_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(WorkExecutionError::Database)?;
        self.durable_before_execute
            .store(durable == Some(execution_id), Ordering::SeqCst);
        self.execute_started.notify_one();
        if matches!(self.execute_behavior, ExecuteBehavior::Blocked) {
            self.release_execute.notified().await;
        }
        match &self.execute_behavior {
            ExecuteBehavior::Blocked => Ok(WorkExecutionOutcome {
                exit_code: 0,
                verification_exit_code: None,
                output: "executed".to_owned(),
                output_truncated: false,
            }),
            ExecuteBehavior::Error(error) => Err((*error).into_error()),
        }
    }

    async fn cancel(
        &self,
        _target: &ContainerWorkExecutionTarget,
        _execution_id: Uuid,
        _deadline_at: UtcTimestamp,
    ) -> Result<(), WorkExecutionError> {
        self.cancel_calls.fetch_add(1, Ordering::SeqCst);
        if let Some(error) = self.cancel_behavior {
            return Err(error.into_error());
        }
        Ok(())
    }

    async fn prestart_failure(
        &self,
        _target: &ContainerWorkExecutionTarget,
        _execution_id: Uuid,
    ) -> Result<Option<String>, WorkExecutionError> {
        Ok(self.prestart_diagnostic.clone())
    }

    async fn observe(
        &self,
        _target: &ContainerWorkExecutionTarget,
        _execution_id: Uuid,
        _verification_required: bool,
    ) -> Result<Option<WorkExecutionOutcome>, WorkExecutionError> {
        self.observe_calls.fetch_add(1, Ordering::SeqCst);
        self.observe_started.notify_one();
        match &self.observe_behavior {
            ObserveBehavior::None => Ok(None),
            ObserveBehavior::Error(error) => Err((*error).into_error()),
            ObserveBehavior::Outcome(outcome) => Ok(Some(outcome.clone())),
            ObserveBehavior::Blocked(outcome) => {
                self.release_observe.notified().await;
                Ok(Some(outcome.clone()))
            }
        }
    }
}

struct UnusedAdmission;

#[async_trait]
impl WorkAdmissionResolver for UnusedAdmission {
    async fn resolve(
        &self,
        _run_id: AgentRunId,
        _query: &WorkConfigurationAdmissionQuery,
        _now: UtcTimestamp,
    ) -> Result<WorkConfigurationAdmissionBinding, WorkAdmissionClientError> {
        Err(WorkAdmissionClientError::Unavailable)
    }
}

#[derive(Default)]
struct MalformedObservationBackend {
    cancel_calls: AtomicUsize,
}

#[async_trait]
impl ContainerWorkExecutionBackend for MalformedObservationBackend {
    async fn resolve_target(
        &self,
        _request: &ContainerWorkExecutionRequest,
    ) -> Result<ContainerWorkExecutionTarget, WorkExecutionError> {
        Err(WorkExecutionError::TargetUnavailable)
    }

    async fn execute(
        &self,
        _target: &ContainerWorkExecutionTarget,
        _execution_id: Uuid,
        _script: &str,
        _verification_script: Option<&str>,
        _deadline_at: UtcTimestamp,
    ) -> Result<WorkExecutionOutcome, WorkExecutionError> {
        Err(WorkExecutionError::RunnerFailed)
    }

    async fn cancel(
        &self,
        _target: &ContainerWorkExecutionTarget,
        _execution_id: Uuid,
        _deadline_at: UtcTimestamp,
    ) -> Result<(), WorkExecutionError> {
        self.cancel_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn observe(
        &self,
        _target: &ContainerWorkExecutionTarget,
        _execution_id: Uuid,
        _verification_required: bool,
    ) -> Result<Option<WorkExecutionOutcome>, WorkExecutionError> {
        Err(WorkExecutionError::ObservationInvalid)
    }
}
