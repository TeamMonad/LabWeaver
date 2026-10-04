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

#[tokio::test]
async fn schema_invocation_generations_keep_task_usage_and_cleanup_independent() -> TestResult {
    let (pool, _container) = fixture().await?;
    let store = PostgresAgentRunStore::new(pool.clone());
    let first = intent()?;
    let mut second = intent()?;
    second.run_id = first.run_id;
    second.execution_generation = 2;
    second.binding["executionGeneration"] = serde_json::json!(2);
    terminal(&store, &first).await?;
    terminal(&store, &second).await?;
    let first_usage = usage(&first)?;
    let second_usage = usage(&second)?;
    store.checkpoint_sandbox_usage(&first, &first_usage).await?;
    store
        .checkpoint_sandbox_usage(&second, &second_usage)
        .await?;
    store.confirm_sandbox_cleanup(&first).await?;
    store.mark_sandbox_released(&first).await?;
    store.mark_sandbox_usage_delivered(&first).await?;
    let restarted = PostgresAgentRunStore::new(pool);
    let checkpoint = restarted
        .load_sandbox_attempt(first.run_id, first.track, 1, 2)
        .await?
        .ok_or("second generation missing")?;
    assert_eq!(checkpoint.task_run_id, second.task_run_id);
    assert_eq!(checkpoint.state, "failed");
    assert_eq!(
        restarted.load_sandbox_usage(&first).await?,
        Some((first_usage, true))
    );
    assert_eq!(
        restarted.load_sandbox_usage(&second).await?,
        Some((second_usage, false))
    );
    let mut wrong = second.clone();
    wrong.binding["executionGeneration"] = serde_json::json!(1);
    assert_eq!(
        restarted.confirm_sandbox_cleanup(&wrong).await,
        Err(AgentRunStoreError::IdentityMismatch)
    );
    Ok(())
}

fn frozen_receipt(intent: &SandboxAttemptIntent) -> serde_json::Value {
    let empty = persistence_sqlx::Sha256Digest::of_bytes(&[]).to_string();
    serde_json::json!({
        "taskRunId":intent.task_run_id,
        "receipt":{"resultSizeBytes":0,"resultSha256":empty,"stderrSizeBytes":0,"stderrSha256":empty,
            "exitCode":7,"claudeVersion":"2.1.215","exportSizeBytes":0,"exportSha256":empty},
        "keys":["own/result.json","own/stderr.log","own/export.tar"],"references":[null,null,null]
    })
}

#[tokio::test]
async fn terminal_receipt_first_winner_is_immutable_and_available_after_cleanup() -> TestResult {
    let (pool, _container) = fixture().await?;
    let store = PostgresAgentRunStore::new(pool.clone());
    let intent = intent()?;
    store.begin_sandbox_attempt(&intent).await?;
    sqlx::query("UPDATE agent.authoring_sandbox_attempts SET result_object_key='own/result.json',stderr_object_key='own/stderr.log',export_object_key='own/export.tar' WHERE task_run_id=$1")
        .bind(intent.task_run_id).execute(&pool).await?;
    store
        .record_sandbox_objects(&intent, &serde_json::json!([]))
        .await?;
    let proposed = frozen_receipt(&intent);
    assert_eq!(
        store
            .checkpoint_sandbox_receipt(&intent, "sandbox-results", &proposed)
            .await?,
        proposed
    );
    let digest = persistence_sqlx::Sha256Digest::of_bytes(&[]).to_string();
    store
        .complete_sandbox_attempt(&intent, Some(("own/result.json", &digest, 0)), 7, None)
        .await?;
    store
        .mark_sandbox_usage_unavailable(&intent, "timing_unavailable")
        .await?;
    store.confirm_sandbox_cleanup(&intent).await?;
    store.mark_sandbox_released(&intent).await?;
    let restarted = PostgresAgentRunStore::new(pool);
    assert_eq!(
        restarted
            .checkpoint_sandbox_receipt(&intent, "sandbox-results", &proposed)
            .await?,
        proposed
    );
    let current = restarted
        .load_sandbox_attempt(intent.run_id, intent.track, intent.attempt, 1)
        .await?
        .ok_or("checkpoint missing")?;
    assert_eq!(current.terminal_receipt, Some(proposed.clone()));
    assert_eq!(current.state, "released");
    let mut wrong = proposed.clone();
    wrong["taskRunId"] = serde_json::json!(uuid::Uuid::now_v7());
    assert_eq!(
        restarted
            .checkpoint_sandbox_receipt(&intent, "sandbox-results", &wrong)
            .await,
        Err(AgentRunStoreError::IdentityMismatch)
    );
    let mut changed = proposed.clone();
    changed["receipt"]["exitCode"] = serde_json::json!(0);
    assert_eq!(
        restarted
            .checkpoint_sandbox_receipt(&intent, "sandbox-results", &changed)
            .await,
        Err(AgentRunStoreError::IdentityMismatch)
    );
    let mut malformed = proposed.clone();
    malformed["receipt"]
        .as_object_mut()
        .ok_or("receipt missing")?
        .remove("exportSizeBytes");
    assert_eq!(
        restarted
            .checkpoint_sandbox_receipt(&intent, "sandbox-results", &malformed)
            .await,
        Err(AgentRunStoreError::InvalidContract)
    );
    restarted
        .complete_sandbox_attempt(&intent, None, 1, Some("LW_AGENT_SANDBOX_RECEIPT_INVALID"))
        .await?;
    assert_eq!(
        restarted
            .checkpoint_sandbox_receipt(&intent, "sandbox-results", &proposed)
            .await,
        Err(AgentRunStoreError::StateConflict)
    );
    assert_eq!(
        restarted
            .complete_sandbox_attempt(&intent, Some(("own/result.json", &digest, 0)), 7, None)
            .await,
        Err(AgentRunStoreError::StateConflict)
    );
    let failed = restarted
        .load_sandbox_attempt(intent.run_id, intent.track, intent.attempt, 1)
        .await?
        .ok_or("failed checkpoint missing")?;
    assert_eq!(failed.state, "released");
    assert_eq!(
        failed.diagnostic_code.as_deref(),
        Some("LW_AGENT_SANDBOX_RECEIPT_INVALID")
    );
    Ok(())
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "keep the creating checkpoint and lease/cancellation fences in one real PostgreSQL sequence"
)]
async fn creating_intent_freezes_request_and_fences_lease_cancellation_and_start() -> TestResult {
    use agent_service::claude_code::AuthoringAttemptScope;
    let (pool, _container) = fixture().await?;
    let store = PostgresAgentRunStore::new(pool.clone());
    let started: time::OffsetDateTime =
        sqlx::query_scalar("SELECT date_trunc('milliseconds',clock_timestamp())")
            .fetch_one(&pool)
            .await?;
    let scope = AuthoringAttemptScope {
        run_id: AgentRunId::new(),
        project_id: ProjectId::new(),
        course_id: None,
        actor_id: contracts::ActorId::new(),
        track: AgentTrackKind::Environment,
        attempt: 1,
        execution_generation: 1,
        started_at: UtcTimestamp::from_utc(started)?,
        worker_id: "generation-worker".to_owned(),
        lease_token: uuid::Uuid::now_v7(),
        trace_id: "generation-test".to_owned(),
        claude_code_version: "2.1.215".to_owned(),
    };
    // Only authoritative columns consumed by the generation fence are needed in this DB fixture.
    sqlx::query("INSERT INTO agent.agent_runs (run_id,project_id,problem_package_id,revision,state,provider_binding,input_sha256,policy_revision,contract,purpose) VALUES ($1,$2,$3,1,'running','claude-code-production',$4,1,'{}','{}')")
        .bind(scope.run_id.as_uuid()).bind(scope.project_id.as_uuid()).bind(uuid::Uuid::now_v7()).bind("a".repeat(64)).execute(&pool).await?;
    sqlx::query("INSERT INTO agent.agent_track_work_items (run_id,track,state,input_sha256,attempt_number,worker_id,lease_token,lease_expires_at,heartbeat_at,attempt_started_at) VALUES ($1,'environment','running',$2,1,$3,$4,clock_timestamp()+interval '1 minute',clock_timestamp(),$5)")
        .bind(scope.run_id.as_uuid()).bind("a".repeat(64)).bind(&scope.worker_id).bind(scope.lease_token).bind(started).execute(&pool).await?;
    let task = uuid::Uuid::now_v7();
    let keys = ["own/result.json", "own/stderr.log", "own/export.tar"];
    let frozen_request =
        serde_json::json!({"durationSeconds":900,"resources":{"cpuMillicores":100}});
    let saved = store
        .reserve_sandbox_generation(
            &scope,
            task,
            "agent-sandboxes",
            "sandbox-generation",
            keys,
            &frozen_request,
        )
        .await?;
    assert_eq!(saved.state, "creating");
    assert_eq!(saved.binding, None);
    assert_eq!(saved.request_payload, Some(frozen_request.clone()));
    let replay = store
        .reserve_sandbox_generation(
            &scope,
            uuid::Uuid::now_v7(),
            "agent-sandboxes",
            "sandbox-generation",
            keys,
            &serde_json::json!({"durationSeconds":1}),
        )
        .await?;
    assert_eq!(replay.task_run_id, task);
    assert_eq!(replay.request_payload, Some(frozen_request));
    for field in ["token", "worker", "attempt", "started", "project"] {
        let mut wrong = scope.clone();
        match field {
            "token" => wrong.lease_token = uuid::Uuid::now_v7(),
            "worker" => wrong.worker_id = "wrong-worker".to_owned(),
            "attempt" => wrong.attempt = 2,
            "started" => wrong.started_at = time_at(100)?,
            _ => wrong.project_id = ProjectId::new(),
        }
        assert_eq!(
            store.fence_sandbox_generation(&wrong).await,
            Err(AgentRunStoreError::LeaseLost)
        );
    }
    sqlx::query(
        "UPDATE agent.agent_runs SET cancellation_requested_at=clock_timestamp() WHERE run_id=$1",
    )
    .bind(scope.run_id.as_uuid())
    .execute(&pool)
    .await?;
    let mut repair = scope.clone();
    repair.execution_generation = 2;
    assert!(matches!(
        store
            .reserve_sandbox_generation(
                &repair,
                uuid::Uuid::now_v7(),
                "agent-sandboxes",
                "sandbox-repair",
                keys,
                &serde_json::json!({})
            )
            .await,
        Err(AgentRunStoreError::LeaseLost)
    ));
    assert!(
        store
            .load_sandbox_attempt(scope.run_id, scope.track, 1, 2)
            .await?
            .is_none()
    );
    sqlx::query("UPDATE agent.agent_runs SET cancellation_requested_at=NULL WHERE run_id=$1")
        .bind(scope.run_id.as_uuid())
        .execute(&pool)
        .await?;
    sqlx::query("UPDATE agent.agent_track_work_items SET lease_expires_at=clock_timestamp()-interval '1 second' WHERE run_id=$1").bind(scope.run_id.as_uuid()).execute(&pool).await?;
    assert_eq!(
        store.fence_sandbox_generation(&scope).await,
        Err(AgentRunStoreError::LeaseLost)
    );
    let stored_start: time::OffsetDateTime = sqlx::query_scalar(
        "SELECT attempt_started_at FROM agent.agent_track_work_items WHERE run_id=$1",
    )
    .bind(scope.run_id.as_uuid())
    .fetch_one(&pool)
    .await?;
    assert_eq!(stored_start, started);
    Ok(())
}
