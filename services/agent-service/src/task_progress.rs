//! Read projections from Agent attempts and Resource facts, without a second task lifecycle.
#![allow(missing_docs, clippy::missing_errors_doc)]

use contracts::authoring::{
    AgentAttempt, AgentAttemptState, AgentRunTaskProgress, AgentTrackKind, AgentTrackTaskProgress,
};
use contracts::execution::{TaskAttemptPhase, TaskAttemptProgress};
use contracts::http::TaskProgressQuery;
use contracts::resource::{ResourceRequest, ResourceRequestState};
use contracts::{ActorId, AgentRunId, TaskRunId};
use sqlx::Row;
use task_execution::resource::{ResourceClient, ResourceClientError};

use crate::run_store::{AgentRunStoreError, PostgresAgentRunStore};

#[derive(Debug, thiserror::Error)]
pub enum TaskProgressError {
    #[error(transparent)]
    Store(#[from] AgentRunStoreError),
    #[error("LW_TASK_PROGRESS_PAGE_INVALID")]
    PageInvalid,
    #[error("LW_TASK_PROGRESS_RESOURCE_UNAVAILABLE")]
    ResourceUnavailable,
    #[error("LW_TASK_PROGRESS_IDENTITY_MISMATCH")]
    IdentityMismatch,
    #[error("LW_TASK_PROGRESS_CHANGED")]
    Changed,
}

#[derive(Clone, Debug, PartialEq)]
struct SandboxProgress {
    track: AgentTrackKind,
    attempt: u32,
    generation: u64,
    task_run_id: TaskRunId,
    state: String,
    diagnostic: Option<String>,
    submitted: bool,
    request_id: Option<contracts::ResourceRequestId>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct HistoryPosition(u32, u64, AgentTrackKind);

#[derive(PartialEq)]
struct ProgressEntry {
    track: AgentTrackKind,
    attempt: AgentAttempt,
    sandbox: Option<SandboxProgress>,
}

impl PostgresAgentRunStore {
    /// Keeps the latest business attempt on every page; history never replaces it.
    #[allow(
        clippy::too_many_lines,
        reason = "owner, sandbox and Work execution reads share one bounded consistency check"
    )]
    pub async fn task_progress(
        &self,
        run_id: AgentRunId,
        actor_id: ActorId,
        resource: Option<&ResourceClient>,
        query: &TaskProgressQuery,
    ) -> Result<AgentRunTaskProgress, TaskProgressError> {
        query
            .validate()
            .map_err(|_| TaskProgressError::PageInvalid)?;
        let limit = query.limit.unwrap_or(50);
        for _ in 0..3 {
            let run = self.load(run_id).await?;
            let scope = ("agent-task-progress", actor_id, run.project_id, run_id);
            let after: Option<HistoryPosition> = query
                .cursor
                .as_deref()
                .map(|value| {
                    persistence_sqlx::decode_cursor(
                        "agent",
                        self.cursor_signing_key_current.as_slice(),
                        self.cursor_signing_key_previous
                            .as_deref()
                            .map(Vec::as_slice),
                        &scope,
                        value,
                    )
                    .map_err(|_| TaskProgressError::PageInvalid)
                })
                .transpose()?;
            let executing_work = self.executing_work(run_id, run.plan.as_ref()).await?;
            let current_rows = self
                .progress_entries(run_id, run.project_id, executing_work, true, None, limit)
                .await?;
            let past = self
                .progress_entries(
                    run_id,
                    run.project_id,
                    executing_work,
                    false,
                    after.as_ref(),
                    limit,
                )
                .await?;
            let actor: Option<uuid::Uuid> = sqlx::query_scalar(
                "SELECT actor_id FROM agent.agent_run_dispatches WHERE run_id=$1",
            )
            .bind(run_id.as_uuid())
            .fetch_optional(self.pool())
            .await
            .map_err(|_| AgentRunStoreError::PersistenceFailed)?
            .flatten();
            let mut current = Vec::new();
            for entry in &current_rows {
                let mut projected = projection(entry.track, &entry.attempt, entry.sandbox.as_ref());
                if let Some(saved) = entry
                    .sandbox
                    .as_ref()
                    .filter(|saved| saved.state != "released")
                    && let Some(request) =
                        current_resource(resource, run.project_id, actor, saved).await?
                {
                    apply_resource(&mut projected.progress, &request, saved);
                }
                current.push(projected);
            }
            if executing_work
                && let Some(attempt) = run
                    .tracks
                    .iter()
                    .find(|track| track.kind == AgentTrackKind::WorkConfiguration)
                    .and_then(|track| track.attempts.last())
                    .filter(|attempt| attempt.state == AgentAttemptState::Running)
            {
                let mut entry = projection(AgentTrackKind::WorkConfiguration, attempt, None);
                entry.progress.phase = TaskAttemptPhase::Executing;
                entry
                    .progress
                    .next_steps
                    .push("getProjectWorkConfigurationPlan".to_owned());
                current.push(entry);
            }
            if self.load(run_id).await?.revision != run.revision
                || self.executing_work(run_id, run.plan.as_ref()).await? != executing_work
                || self
                    .progress_entries(run_id, run.project_id, executing_work, true, None, limit)
                    .await?
                    != current_rows
                || self
                    .progress_entries(
                        run_id,
                        run.project_id,
                        executing_work,
                        false,
                        after.as_ref(),
                        limit,
                    )
                    .await?
                    != past
            {
                continue;
            }
            let next_cursor = if past.len() > usize::from(limit) {
                let last = &past[usize::from(limit) - 1];
                Some(
                    persistence_sqlx::encode_cursor(
                        "agent",
                        self.cursor_signing_key_current.as_slice(),
                        &scope,
                        HistoryPosition(
                            last.attempt.number,
                            last.sandbox.as_ref().map_or(0, |saved| saved.generation),
                            last.track,
                        ),
                    )
                    .map_err(|_| TaskProgressError::PageInvalid)?,
                )
            } else {
                None
            };
            let history = past
                .into_iter()
                .take(usize::from(limit))
                .map(|entry| {
                    let mut projected =
                        projection(entry.track, &entry.attempt, entry.sandbox.as_ref());
                    if let Some(saved) = entry
                        .sandbox
                        .as_ref()
                        .filter(|saved| saved.state == "released")
                    {
                        projected.progress.phase = if saved.diagnostic.is_some() {
                            TaskAttemptPhase::Failed
                        } else {
                            TaskAttemptPhase::Succeeded
                        };
                    }
                    projected.progress.next_steps.clear();
                    projected
                })
                .collect();
            return Ok(AgentRunTaskProgress {
                run_id,
                project_id: run.project_id,
                revision: run.revision,
                state: run.state,
                current,
                history,
                next_cursor,
            });
        }
        Err(TaskProgressError::Changed)
    }

    #[allow(clippy::too_many_arguments)]
    async fn progress_entries(
        &self,
        run_id: AgentRunId,
        project_id: contracts::ProjectId,
        executing_work: bool,
        current: bool,
        after: Option<&HistoryPosition>,
        limit: u16,
    ) -> Result<Vec<ProgressEntry>, AgentRunStoreError> {
        let rows = sqlx::query(
            "WITH entries AS (
                SELECT t->>'kind' AS track, a AS attempt_contract,
                    (a->>'number')::bigint AS attempt_number,
                    CASE t->>'kind' WHEN 'environment' THEN 0 WHEN 'evaluation' THEN 1 ELSE 2 END AS track_order,
                    s.execution_generation,s.task_run_id,s.state,s.diagnostic_code,
                    s.state IN ('submitted','terminal') AS submitted,
                    (s.binding->>'resourceRequestId')::uuid AS resource_request_id,
                    $3 AND t->>'kind'='work_configuration' AND a->>'state'='running' AS executing,
                    row_number() OVER (PARTITION BY t->>'kind' ORDER BY (a->>'number')::bigint DESC,s.execution_generation DESC NULLS LAST) AS rank
                FROM agent.agent_runs r
                CROSS JOIN LATERAL jsonb_array_elements(r.contract->'tracks') t
                CROSS JOIN LATERAL jsonb_array_elements(t->'attempts') a
                LEFT JOIN agent.authoring_sandbox_attempts s ON s.run_id=r.run_id AND s.track=t->>'kind' AND s.attempt_number=(a->>'number')::bigint
                WHERE r.run_id=$1 AND r.project_id=$2
            ) SELECT * FROM entries
            WHERE CASE WHEN $4 THEN rank=1 AND NOT executing ELSE (rank<>1 OR executing) AND (NOT executing OR task_run_id IS NOT NULL) END
                AND ($5::bigint IS NULL OR (attempt_number,COALESCE(execution_generation,0),track_order)<($5,$6,$7))
            ORDER BY CASE WHEN $4 THEN track_order END ASC,attempt_number DESC,COALESCE(execution_generation,0) DESC,track_order DESC
            LIMIT CASE WHEN $4 THEN NULL ELSE $8 END",
        ).bind(run_id.as_uuid()).bind(project_id.as_uuid()).bind(executing_work).bind(current)
            .bind(after.map(|key| i64::from(key.0)))
            .bind(after.map(|key| i64::try_from(key.1)).transpose().map_err(|_| AgentRunStoreError::InvalidContract)?)
            .bind(after.map(|key| match key.2 { AgentTrackKind::Environment => 0_i32, AgentTrackKind::Evaluation => 1, AgentTrackKind::WorkConfiguration => 2 }))
            .bind(i64::from(limit)+1).fetch_all(self.pool()).await.map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        rows.into_iter()
            .map(|row| {
                let track = serde_json::from_value(serde_json::Value::String(
                    row.try_get("track")
                        .map_err(|_| AgentRunStoreError::PersistenceFailed)?,
                ))
                .map_err(|_| AgentRunStoreError::InvalidContract)?;
                let attempt: AgentAttempt = serde_json::from_value(
                    row.try_get("attempt_contract")
                        .map_err(|_| AgentRunStoreError::PersistenceFailed)?,
                )
                .map_err(|_| AgentRunStoreError::InvalidContract)?;
                let task: Option<uuid::Uuid> = row
                    .try_get("task_run_id")
                    .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
                let sandbox = task
                    .map(|task| -> Result<SandboxProgress, AgentRunStoreError> {
                        Ok(SandboxProgress {
                            track,
                            attempt: attempt.number,
                            generation: u64::try_from(
                                row.try_get::<i64, _>("execution_generation")
                                    .map_err(|_| AgentRunStoreError::PersistenceFailed)?,
                            )
                            .map_err(|_| AgentRunStoreError::InvalidContract)?,
                            task_run_id: task
                                .to_string()
                                .parse()
                                .map_err(|_| AgentRunStoreError::InvalidContract)?,
                            state: row
                                .try_get("state")
                                .map_err(|_| AgentRunStoreError::PersistenceFailed)?,
                            diagnostic: row
                                .try_get("diagnostic_code")
                                .map_err(|_| AgentRunStoreError::PersistenceFailed)?,
                            submitted: row
                                .try_get("submitted")
                                .map_err(|_| AgentRunStoreError::PersistenceFailed)?,
                            request_id: row
                                .try_get::<Option<uuid::Uuid>, _>("resource_request_id")
                                .map_err(|_| AgentRunStoreError::PersistenceFailed)?
                                .map(|id| id.to_string().parse())
                                .transpose()
                                .map_err(|_| AgentRunStoreError::InvalidContract)?,
                        })
                    })
                    .transpose()?;
                Ok(ProgressEntry {
                    track,
                    attempt,
                    sandbox,
                })
            })
            .collect()
    }

    async fn executing_work(
        &self,
        run_id: AgentRunId,
        plan: Option<&contracts::authoring::WorkConfigurationPlan>,
    ) -> Result<bool, AgentRunStoreError> {
        let Some(plan) = plan else {
            return Ok(false);
        };
        sqlx::query_scalar(
            "SELECT COALESCE(execution_request->>'planId'=$2,false) AND state='running'
            FROM agent.agent_track_work_items WHERE run_id=$1 AND track='work_configuration'",
        )
        .bind(run_id.as_uuid())
        .bind(plan.id.to_string())
        .fetch_optional(self.pool())
        .await
        .map(|value| value.unwrap_or(false))
        .map_err(|_| AgentRunStoreError::PersistenceFailed)
    }
}

fn projection(
    track: AgentTrackKind,
    attempt: &AgentAttempt,
    sandbox: Option<&SandboxProgress>,
) -> AgentTrackTaskProgress {
    let phase = match attempt.state {
        AgentAttemptState::Pending => TaskAttemptPhase::Queued,
        AgentAttemptState::Running | AgentAttemptState::Repairing => TaskAttemptPhase::Preparing,
        AgentAttemptState::AwaitingApproval => TaskAttemptPhase::AwaitingReview,
        AgentAttemptState::Succeeded => TaskAttemptPhase::Succeeded,
        AgentAttemptState::Failed => TaskAttemptPhase::Failed,
        AgentAttemptState::Cancelled => TaskAttemptPhase::Cancelled,
    };
    let phase =
        if sandbox.is_some_and(|saved| saved.state != "released" && saved.state != "creating") {
            TaskAttemptPhase::CleanupPending
        } else {
            phase
        };
    let next_steps = match phase {
        TaskAttemptPhase::Failed | TaskAttemptPhase::Cancelled => {
            vec!["getProjectAgentRun", "retryProjectAgentRunTrack"]
        }
        TaskAttemptPhase::Succeeded | TaskAttemptPhase::CleanupPending => {
            vec!["getProjectAgentRun"]
        }
        _ => vec!["getProjectAgentRun", "cancelProjectAgentRun"],
    }
    .into_iter()
    .map(str::to_owned)
    .collect();
    AgentTrackTaskProgress {
        track,
        progress: TaskAttemptProgress {
            attempt: attempt.number,
            execution_generation: sandbox.map(|saved| saved.generation),
            task_run_id: sandbox.map(|saved| saved.task_run_id),
            phase,
            resource_request_id: sandbox.and_then(|saved| saved.request_id),
            resource_state: None,
            diagnostic_code: sandbox
                .and_then(|saved| saved.diagnostic.clone())
                .or_else(|| attempt.diagnostic_code.clone()),
            next_steps,
        },
    }
}

fn apply_resource(
    progress: &mut TaskAttemptProgress,
    request: &ResourceRequest,
    sandbox: &SandboxProgress,
) {
    progress.resource_request_id = Some(request.id);
    progress.resource_state = Some(request.state);
    if request.diagnostic_code.is_some() {
        progress
            .diagnostic_code
            .clone_from(&request.diagnostic_code);
    }
    progress.phase = match request.state {
        ResourceRequestState::Reviewing => TaskAttemptPhase::WaitingResourceApproval,
        ResourceRequestState::Active
            if sandbox.diagnostic.is_some()
                || request.diagnostic_code.as_deref() == Some("LW_RESOURCE_TASK_LEASE_EXPIRED") =>
        {
            TaskAttemptPhase::CleanupPending
        }
        ResourceRequestState::Active if sandbox.submitted => TaskAttemptPhase::Executing,
        ResourceRequestState::Allocating | ResourceRequestState::Active => {
            TaskAttemptPhase::Allocating
        }
        ResourceRequestState::Expiring => TaskAttemptPhase::CleanupPending,
        _ if sandbox.submitted => TaskAttemptPhase::CleanupPending,
        _ => TaskAttemptPhase::Blocked,
    };
    if matches!(
        progress.phase,
        TaskAttemptPhase::CleanupPending | TaskAttemptPhase::Blocked
    ) {
        progress.next_steps = vec!["getProjectAgentRun".to_owned()];
    }
}

async fn current_resource(
    resource: Option<&ResourceClient>,
    project: contracts::ProjectId,
    actor: Option<uuid::Uuid>,
    saved: &SandboxProgress,
) -> Result<Option<ResourceRequest>, TaskProgressError> {
    let client = resource.ok_or(TaskProgressError::ResourceUnavailable)?;
    match client.get_task_resource_request(saved.task_run_id).await {
        Ok(request) => {
            if request.project_id != project
                || actor.is_none_or(|id| request.requester_id.as_uuid() != id)
            {
                return Err(TaskProgressError::IdentityMismatch);
            }
            Ok(Some(request))
        }
        Err(ResourceClientError::RequestMissing) if saved.state == "creating" => Ok(None),
        Err(_) => Err(TaskProgressError::ResourceUnavailable),
    }
}
