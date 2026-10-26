//! Real `PostgreSQL` keyset regression, including deletion of the continuation anchor.
use super::*;
use agent_service::task_progress::TaskProgressError;
use contracts::ActorId;
use contracts::authoring::{AgentAttempt, AgentAttemptState, LlmUsage};
use contracts::http::TaskProgressQuery;

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn attempts_cursor_survives_concurrent_generations_and_deleted_anchor()
-> Result<(), Box<dyn std::error::Error>> {
    let postgres = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            postgres.get_host_port_ipv4(5432).await?
        ))
        .await?;
    apply_agent_migrations(&pool).await?;
    let mut run = requested_run()?;
    run.state = AgentRunState::Failed;
    run.tracks[0].attempts = (1..=3).map(failed_attempt).collect();
    run.tracks[1].attempts = vec![failed_attempt(1)];
    run.validate()?;
    insert_run(&pool, &run).await?;
    let mut other = run.clone();
    other.id = AgentRunId::new();
    other.project_id = ProjectId::new();
    insert_run(&pool, &other).await?;
    for generation in 1..=3 {
        insert_sandbox(&pool, run.id, 2, generation).await?;
    }
    insert_sandbox(&pool, run.id, 1, 1).await?;
    insert_sandbox(&pool, other.id, 2, 100).await?;
    let actor = ActorId::new();
    let store =
        PostgresAgentRunStore::new(pool.clone()).with_cursor_signing_keys(vec![7; 32], None);
    let query = TaskProgressQuery {
        cursor: None,
        limit: Some(1),
    };
    let first = store.task_progress(run.id, actor, None, &query).await?;
    assert_eq!(first.history[0].progress.execution_generation, Some(3));
    assert_eq!(first.current[0].progress.attempt, 3);
    let continuation = TaskProgressQuery {
        cursor: first.next_cursor.clone(),
        ..query.clone()
    };
    assert!(continuation.cursor.is_some());
    for (run_id, actor_id) in [(other.id, actor), (run.id, ActorId::new())] {
        assert!(matches!(
            store
                .task_progress(run_id, actor_id, None, &continuation)
                .await,
            Err(TaskProgressError::PageInvalid)
        ));
    }
    let mut tampered = first.next_cursor.clone().ok_or("missing cursor")?;
    let last = tampered.pop().ok_or("empty cursor")?;
    tampered.push(if last == 'A' { 'B' } else { 'A' });
    assert!(matches!(
        store
            .task_progress(
                run.id,
                actor,
                None,
                &TaskProgressQuery {
                    cursor: Some(tampered),
                    ..query.clone()
                }
            )
            .await,
        Err(TaskProgressError::PageInvalid)
    ));
    // A separate writer inserts above the boundary and removes the anchor between HTTP pages.
    let mut writer = pool.acquire().await?;
    sqlx::query("DELETE FROM agent.authoring_sandbox_attempts WHERE run_id=$1 AND attempt_number=2 AND execution_generation=3").bind(run.id.as_uuid()).execute(&mut *writer).await?;
    insert_sandbox(&pool, run.id, 2, 4).await?;
    let mut next = continuation;
    let mut seen = Vec::new();
    loop {
        let page = store.task_progress(run.id, actor, None, &next).await?;
        assert_eq!(page.current, first.current);
        seen.extend(
            page.history
                .iter()
                .map(|entry| (entry.progress.attempt, entry.progress.execution_generation)),
        );
        match page.next_cursor {
            Some(value) => next.cursor = Some(value),
            None => break,
        }
    }
    assert_eq!(seen, vec![(2, Some(2)), (2, Some(1)), (1, Some(1))]);
    let rotated = PostgresAgentRunStore::new(pool.clone())
        .with_cursor_signing_keys(vec![8; 32], Some(vec![7; 32]));
    assert_eq!(
        rotated
            .task_progress(
                run.id,
                actor,
                None,
                &TaskProgressQuery {
                    cursor: first.next_cursor.clone(),
                    ..query.clone()
                }
            )
            .await?
            .history[0]
            .progress
            .execution_generation,
        Some(2)
    );
    let retired = PostgresAgentRunStore::new(pool).with_cursor_signing_keys(vec![8; 32], None);
    assert!(matches!(
        retired
            .task_progress(
                run.id,
                actor,
                None,
                &TaskProgressQuery {
                    cursor: first.next_cursor,
                    ..query
                }
            )
            .await,
        Err(TaskProgressError::PageInvalid)
    ));
    Ok(())
}

async fn insert_run(pool: &sqlx::PgPool, run: &AgentRun) -> Result<(), Box<dyn std::error::Error>> {
    sqlx::query("INSERT INTO agent.agent_runs(run_id,project_id,course_id,problem_package_id,revision,state,provider_binding,input_sha256,policy_revision,purpose,contract) VALUES($1,$2,$3,$4,1,'failed','claude-code-v1',$5,1,$6,$7)")
        .bind(run.id.as_uuid()).bind(run.project_id.as_uuid()).bind(run.course_id.map(CourseId::as_uuid)).bind(run.package_id.as_uuid()).bind(Sha256Digest::of_bytes(b"input").to_string()).bind(serde_json::to_value(run.purpose)?).bind(serde_json::to_value(run)?).execute(pool).await?;
    Ok(())
}

async fn insert_sandbox(
    pool: &sqlx::PgPool,
    run_id: AgentRunId,
    attempt: i64,
    generation: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO agent.authoring_sandbox_attempts(run_id,track,attempt_number,execution_generation,task_run_id,namespace,workload_name,state) VALUES($1,'environment',$2,$3,$4,'test','test','released')")
        .bind(run_id.as_uuid()).bind(attempt).bind(generation).bind(uuid::Uuid::now_v7()).execute(pool).await?;
    Ok(())
}

#[tokio::test]
async fn diagnostics_reports_truncation_and_excludes_expired_rows_before_bound()
-> Result<(), Box<dyn std::error::Error>> {
    let postgres = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            postgres.get_host_port_ipv4(5432).await?
        ))
        .await?;
    apply_agent_migrations(&pool).await?;
    let mut run = requested_run()?;
    run.state = AgentRunState::Failed;
    for track in &mut run.tracks {
        track.attempts = vec![failed_attempt(1)];
    }
    run.validate()?;
    insert_run(&pool, &run).await?;
    let now = time::OffsetDateTime::now_utc().replace_nanosecond(0)?;
    let detail = contracts::http::AgentRunDiagnostic {
        track: AgentTrackKind::Environment,
        preparation_failed: true,
        attempt: 1,
        diagnostic_code: "LW_AGENT_FAILED".to_owned(),
        reason: "准备失败".to_owned(),
        next_step: "检查任务后重试".to_owned(),
        model: "test-model".to_owned(),
        runtime_binding: "test-runtime".to_owned(),
        package_id: run.package_id,
        package_revision: Revision::new(1)?,
        policy_id: run.policy_id,
        policy_revision: run.policy_revision,
        input_sha256: None,
        output_sha256: None,
        usage: None,
        created_at: UtcTimestamp::from_utc(now)?,
        expires_at: UtcTimestamp::from_utc(now + time::Duration::days(30))?,
    };
    sqlx::query("INSERT INTO agent.model_diagnostics(run_id,track,attempt,detail,created_at,expires_at) SELECT $1,'environment',n,jsonb_set($2::jsonb,'{attempt}',to_jsonb(n)),$3,$4 FROM generate_series(1,101) n")
        .bind(run.id.as_uuid()).bind(serde_json::to_value(detail)?).bind(now).bind(now+time::Duration::days(30)).execute(&pool).await?;
    let store = PostgresAgentRunStore::new(pool.clone());
    let bounded = store.diagnostics(run.id).await?;
    assert!(bounded.truncated);
    assert_eq!(bounded.items.len(), 100);
    assert_eq!(bounded.items[0].attempt, 101);
    assert_eq!(bounded.items[99].attempt, 2);
    sqlx::query("UPDATE agent.model_diagnostics SET created_at=now()-interval '31 days',expires_at=now()-interval '1 day' WHERE run_id=$1 AND attempt>99")
        .bind(run.id.as_uuid()).execute(&pool).await?;
    let complete = store.diagnostics(run.id).await?;
    assert!(!complete.truncated);
    assert_eq!(complete.items.len(), 99);
    assert_eq!(complete.items[0].attempt, 99);
    Ok(())
}

fn failed_attempt(number: u32) -> AgentAttempt {
    AgentAttempt {
        number,
        state: AgentAttemptState::Failed,
        checkpoint: None,
        usage: LlmUsage {
            input_tokens: 0,
            output_tokens: 0,
            requests: 0,
            cost_microusd: 0,
        },
        usage_observed: false,
        diagnostic_code: Some("LW_AGENT_FAILED".to_owned()),
    }
}
