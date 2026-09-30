//! Durable metering and cleanup checkpoints against the service's `PostgreSQL` schema.
use agent_service::run_store::{AgentRunStoreError, PostgresAgentRunStore, SandboxAttemptIntent};
use contracts::{
    AgentRunId, CapacityClaimId, EventId, LeaseId, ProjectId, ResourceRequestId, Revision,
    TaskRunId, UtcTimestamp,
    authoring::AgentTrackKind,
    execution::{ExecutionObservation, ExecutionWorkloadState, TaskExecutionBinding},
    http::RecordResourceUsageRequest,
    resource::{ResourceUsageKind, ResourceUsageQuantities, UsageMeasurement},
};
use sqlx::{PgPool, Row, postgres::PgPoolOptions};
use testcontainers::{ContainerAsync, ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;

mod support;
use support::apply_agent_migrations;

type TestResult = Result<(), Box<dyn std::error::Error>>;

async fn fixture() -> Result<(PgPool, ContainerAsync<Postgres>), Box<dyn std::error::Error>> {
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            container.get_host_port_ipv4(5432).await?
        ))
        .await?;
    apply_agent_migrations(&pool).await?;
    Ok((pool, container))
}

fn time_at(seconds: i64) -> Result<UtcTimestamp, Box<dyn std::error::Error>> {
    Ok(UtcTimestamp::from_utc(
        time::OffsetDateTime::from_unix_timestamp(seconds)?,
    )?)
}

fn intent() -> Result<SandboxAttemptIntent, Box<dyn std::error::Error>> {
    let binding = TaskExecutionBinding {
        task_run_id: TaskRunId::new(),
        execution_generation: 1,
        resource_request_id: ResourceRequestId::new(),
        capacity_claim_id: CapacityClaimId::new(),
        lease_id: LeaseId::new(),
        claim_revision: Revision::new(1)?,
        lease_revision: Revision::new(1)?,
        project_id: ProjectId::new(),
        provider_binding: "sandbox-pool".to_owned(),
        namespace: "agent-sandboxes".to_owned(),
        workload_name: "sandbox-metering".to_owned(),
        trace_id: "metering-recovery".to_owned(),
    };
    binding.validate()?;
    Ok(SandboxAttemptIntent {
        run_id: AgentRunId::new(),
        track: AgentTrackKind::Environment,
        attempt: 1,
        task_run_id: binding.task_run_id.as_uuid(),
        execution_generation: binding.execution_generation,
        namespace: binding.namespace.clone(),
        workload_name: binding.workload_name.clone(),
        binding: serde_json::to_value(binding)?,
    })
}

fn usage(intent: &SandboxAttemptIntent) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let binding: TaskExecutionBinding = serde_json::from_value(intent.binding.clone())?;
    Ok(serde_json::to_value([RecordResourceUsageRequest {
        project_id: binding.project_id,
        course_id: None,
        kind: ResourceUsageKind::Compute,
        request_id: binding.resource_request_id,
        lease_id: Some(binding.lease_id),
        source_event_id: EventId::new(),
        measured_from: time_at(100)?,
        measured_until: time_at(160)?,
        measurement: UsageMeasurement::Known {
            quantities: ResourceUsageQuantities {
                cpu_millicore_seconds: 60_000,
                memory_byte_seconds: 60 * 1024 * 1024,
                storage_byte_seconds: 0,
                gpu_unit_seconds: 0,
            },
        },
    }])?)
}

async fn terminal(store: &PostgresAgentRunStore, intent: &SandboxAttemptIntent) -> TestResult {
    store.begin_sandbox_attempt(intent).await?;
    store
        .record_sandbox_objects(intent, &serde_json::json!([]))
        .await?;
    store
        .complete_sandbox_attempt(intent, None, 1, Some("LW_PROVIDER_FAILED"))
        .await?;
    Ok(())
}

#[tokio::test]
async fn fixed_usage_replays_after_release_and_refuses_same_event_payload_changes() -> TestResult {
    let (pool, _container) = fixture().await?;
    let store = PostgresAgentRunStore::new(pool.clone());
    let intent = intent()?;
    terminal(&store, &intent).await?;
    let payload = usage(&intent)?;
    store.checkpoint_sandbox_usage(&intent, &payload).await?;
    store.checkpoint_sandbox_usage(&intent, &payload).await?;
    let mut changed = payload.clone();
    changed[0]["measurement"] = serde_json::to_value(UsageMeasurement::Unknown {
        reason: "timing_unavailable".to_owned(),
    })?;
    assert_eq!(
        store.checkpoint_sandbox_usage(&intent, &changed).await,
        Err(AgentRunStoreError::StateConflict)
    );
    assert_eq!(
        store
            .mark_sandbox_usage_unavailable(&intent, "timing_unavailable")
            .await,
        Err(AgentRunStoreError::StateConflict)
    );
    assert_eq!(
        store.mark_sandbox_released(&intent).await,
        Err(AgentRunStoreError::StateConflict)
    );
    store.confirm_sandbox_cleanup(&intent).await?;
    store.confirm_sandbox_cleanup(&intent).await?;
    store.mark_sandbox_released(&intent).await?;
    store.mark_sandbox_released(&intent).await?;
    let restarted = PostgresAgentRunStore::new(pool.clone());
    restarted.confirm_sandbox_cleanup(&intent).await?;
    assert_eq!(
        restarted.load_sandbox_usage(&intent).await?,
        Some((payload.clone(), false))
    );
    restarted
        .checkpoint_sandbox_usage(&intent, &payload)
        .await?;
    restarted.mark_sandbox_usage_delivered(&intent).await?;
    restarted.mark_sandbox_usage_delivered(&intent).await?;
    assert_eq!(
        restarted.load_sandbox_usage(&intent).await?,
        Some((payload, true))
    );
    let row = sqlx::query(
        "SELECT state,usage_diagnostic_code FROM agent.authoring_sandbox_attempts WHERE run_id=$1",
    )
    .bind(intent.run_id.as_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(row.try_get::<String, _>("state")?, "released");
    assert_eq!(
        row.try_get::<Option<String>, _>("usage_diagnostic_code")?,
        None
    );
    Ok(())
}

#[tokio::test]
async fn durable_terminal_observation_keeps_the_first_boundary_after_objects_are_gone() -> TestResult
{
    let (pool, _container) = fixture().await?;
    let store = PostgresAgentRunStore::new(pool.clone());
    let intent = intent()?;
    terminal(&store, &intent).await?;
    store
        .mark_sandbox_usage_unavailable(&intent, "timing_unavailable")
        .await?;
    store
        .mark_sandbox_usage_unavailable(&intent, "timing_unavailable")
        .await?;
    assert_eq!(
        store
            .mark_sandbox_usage_unavailable(&intent, "different_reason")
            .await,
        Err(AgentRunStoreError::StateConflict)
    );
    let observation = ExecutionObservation {
        state: ExecutionWorkloadState::Failed,
        exit_code: Some(1),
        reason_code: Some("Error".to_owned()),
        pod_name: Some("sandbox-metering-pod".to_owned()),
        started_at: Some(time_at(100)?),
        terminated_at: Some(time_at(160)?),
    };
    let fixed_until = time_at(170)?;
    assert_eq!(
        store
            .checkpoint_sandbox_usage_observation(&intent, &observation, fixed_until)
            .await?,
        (observation.clone(), fixed_until)
    );
    assert_eq!(
        store
            .checkpoint_sandbox_usage_observation(&intent, &observation, time_at(200)?)
            .await?,
        (observation.clone(), fixed_until)
    );
    let mut changed = observation.clone();
    changed.terminated_at = Some(time_at(161)?);
    assert_eq!(
        store
            .checkpoint_sandbox_usage_observation(&intent, &changed, time_at(200)?)
            .await,
        Err(AgentRunStoreError::StateConflict)
    );
    assert_eq!(
        store
            .mark_sandbox_usage_unavailable(&intent, "timing_unavailable")
            .await,
        Err(AgentRunStoreError::StateConflict)
    );
    store.confirm_sandbox_cleanup(&intent).await?;
    store.mark_sandbox_released(&intent).await?;
    let restarted = PostgresAgentRunStore::new(pool.clone());
    assert_eq!(
        restarted.load_sandbox_usage_observation(&intent).await?,
        Some((observation, fixed_until))
    );
    let payload = usage(&intent)?;
    restarted
        .checkpoint_sandbox_usage(&intent, &payload)
        .await?;
    assert_eq!(
        restarted.load_sandbox_usage(&intent).await?,
        Some((payload, false))
    );
    let diagnostic: Option<String> = sqlx::query_scalar(
        "SELECT usage_diagnostic_code FROM agent.authoring_sandbox_attempts WHERE run_id=$1",
    )
    .bind(intent.run_id.as_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(diagnostic, None);
    Ok(())
}

#[tokio::test]
async fn checkpoints_fence_the_entire_attempt_identity_and_preserve_legacy_review_state()
-> TestResult {
    let (pool, _container) = fixture().await?;
    let store = PostgresAgentRunStore::new(pool.clone());
    let intent = intent()?;
    store.begin_sandbox_attempt(&intent).await?;
    let payload = usage(&intent)?;
    assert_eq!(
        store.checkpoint_sandbox_usage(&intent, &payload).await,
        Err(AgentRunStoreError::StateConflict)
    );
    store
        .record_sandbox_objects(&intent, &serde_json::json!([]))
        .await?;
    store
        .complete_sandbox_attempt(&intent, None, 1, Some("LW_PROVIDER_FAILED"))
        .await?;
    for field in ["task", "generation", "namespace", "workload", "binding"] {
        let mut wrong = intent.clone();
        match field {
            "task" => wrong.task_run_id = uuid::Uuid::now_v7(),
            "generation" => wrong.execution_generation += 1,
            "namespace" => wrong.namespace = "another-namespace".to_owned(),
            "workload" => wrong.workload_name = "another-sandbox".to_owned(),
            _ => wrong.binding["traceId"] = serde_json::json!("another-binding"),
        }
        assert_eq!(
            store.begin_sandbox_attempt(&wrong).await,
            Err(AgentRunStoreError::IdentityMismatch)
        );
        assert_eq!(
            store.checkpoint_sandbox_usage(&wrong, &payload).await,
            Err(AgentRunStoreError::IdentityMismatch)
        );
        assert_eq!(
            store.load_sandbox_usage(&wrong).await,
            Err(AgentRunStoreError::IdentityMismatch)
        );
        assert_eq!(
            store.confirm_sandbox_cleanup(&wrong).await,
            Err(AgentRunStoreError::IdentityMismatch)
        );
        assert_eq!(
            store.mark_sandbox_released(&wrong).await,
            Err(AgentRunStoreError::IdentityMismatch)
        );
    }
    store
        .mark_sandbox_usage_unavailable(&intent, "timing_unavailable")
        .await?;
    store.confirm_sandbox_cleanup(&intent).await?;
    store.mark_sandbox_released(&intent).await?;
    assert_eq!(store.load_sandbox_usage(&intent).await?, None);
    assert_eq!(
        store.mark_sandbox_usage_delivered(&intent).await,
        Err(AgentRunStoreError::StateConflict)
    );
    let diagnostic: Option<String> = sqlx::query_scalar(
        "SELECT usage_diagnostic_code FROM agent.authoring_sandbox_attempts WHERE run_id=$1",
    )
    .bind(intent.run_id.as_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(diagnostic.as_deref(), Some("timing_unavailable"));
    Ok(())
}
