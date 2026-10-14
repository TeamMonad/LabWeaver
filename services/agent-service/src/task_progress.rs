//! Read projections from Agent attempts and Resource facts, without a second task lifecycle.
#![allow(missing_docs, clippy::missing_errors_doc)]

use contracts::authoring::{
    AgentAttempt, AgentAttemptState, AgentRunTaskProgress, AgentTrackKind, AgentTrackTaskProgress,
};
use contracts::execution::{TaskAttemptPhase, TaskAttemptProgress};
use contracts::http::PageQuery;
use contracts::resource::{ResourceRequest, ResourceRequestState};
use contracts::{AgentRunId, TaskRunId};
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

impl PostgresAgentRunStore {
    /// Keeps the latest business attempt on every page; history never replaces it.
    #[allow(
        clippy::too_many_lines,
        reason = "owner, sandbox and Work execution reads share one bounded consistency check"
    )]
    pub async fn task_progress(
        &self,
        run_id: AgentRunId,
        resource: Option<&ResourceClient>,
        query: &PageQuery,
    ) -> Result<AgentRunTaskProgress, TaskProgressError> {
        let (page, page_size, offset) = query
            .normalized()
            .map_err(|_| TaskProgressError::PageInvalid)?;
        for _ in 0..3 {
            let run = self.load(run_id).await?;
            let executing_work = self.executing_work(run_id, run.plan.as_ref()).await?;
            let sandboxes = self.sandbox_progress(run_id).await?;
            let actor: Option<uuid::Uuid> = sqlx::query_scalar(
                "SELECT actor_id FROM agent.agent_run_dispatches WHERE run_id=$1",
            )
            .bind(run_id.as_uuid())
            .fetch_optional(self.pool())
            .await
            .map_err(|_| AgentRunStoreError::PersistenceFailed)?
            .flatten();
            let mut current = Vec::new();
            let mut history = Vec::new();
            for track in &run.tracks {
                for attempt in &track.attempts {
                    let generations: Vec<_> = sandboxes
                        .iter()
                        .filter(|saved| {
                            saved.track == track.kind && saved.attempt == attempt.number
                        })
                        .collect();
                    let mut is_current = track
                        .attempts
                        .last()
                        .is_some_and(|last| last.number == attempt.number);
                    if is_current
                        && executing_work
                        && track.kind == AgentTrackKind::WorkConfiguration
                        && attempt.state == AgentAttemptState::Running
                    {
                        let mut entry = projection(track.kind, attempt, None);
                        entry.progress.phase = TaskAttemptPhase::Executing;
                        entry
                            .progress
                            .next_steps
                            .push("getProjectWorkConfigurationPlan".to_owned());
                        current.push(entry);
                        if generations.is_empty() {
                            continue;
                        }
                        is_current = false;
                    }
                    if generations.is_empty() {
                        let mut entry = projection(track.kind, attempt, None);
                        if is_current {
                            current.push(entry);
                        } else {
                            entry.progress.next_steps.clear();
                            history.push(entry);
                        }
                    } else {
                        for (position, saved) in generations.iter().enumerate() {
                            let mut entry = projection(track.kind, attempt, Some(saved));
                            if is_current && position == 0 {
                                if saved.state != "released"
                                    && let Some(request) =
                                        current_resource(resource, run.project_id, actor, saved)
                                            .await?
                                {
                                    apply_resource(&mut entry.progress, &request, saved);
                                }
                                current.push(entry);
                            } else {
                                // Earlier invocations of the same business attempt have their own
                                // durable outcome. A later invocation cannot overwrite it.
                                if saved.state == "released" {
                                    entry.progress.phase = if saved.diagnostic.is_some() {
                                        TaskAttemptPhase::Failed
                                    } else {
                                        TaskAttemptPhase::Succeeded
                                    };
                                }
                                entry.progress.next_steps.clear();
                                history.push(entry);
                            }
                        }
                    }
                }
            }
            if self.load(run_id).await?.revision != run.revision
                || self.sandbox_progress(run_id).await? != sandboxes
                || self.executing_work(run_id, run.plan.as_ref()).await? != executing_work
            {
                continue;
            }
            history.sort_by_key(|entry| {
                std::cmp::Reverse((
                    entry.progress.attempt,
                    entry.progress.execution_generation.unwrap_or(0),
                    entry.track,
                ))
            });
            let offset = usize::try_from(offset).map_err(|_| TaskProgressError::PageInvalid)?;
            let has_more = history.len() > offset.saturating_add(usize::from(page_size));
            let history = history
                .into_iter()
                .skip(offset)
                .take(usize::from(page_size))
                .collect();
            return Ok(AgentRunTaskProgress {
                run_id,
                project_id: run.project_id,
                revision: run.revision,
                state: run.state,
                current,
                history,
                page,
                page_size,
                has_more,
            });
        }
        Err(TaskProgressError::Changed)
    }

    async fn sandbox_progress(
        &self,
        run_id: AgentRunId,
    ) -> Result<Vec<SandboxProgress>, AgentRunStoreError> {
        let rows = sqlx::query(
            "SELECT track,attempt_number,execution_generation,task_run_id,
            state,diagnostic_code,state IN ('submitted','terminal') AS submitted,
            (binding->>'resourceRequestId')::uuid AS resource_request_id
            FROM agent.authoring_sandbox_attempts WHERE run_id=$1
            ORDER BY track,attempt_number DESC,execution_generation DESC",
        )
        .bind(run_id.as_uuid())
        .fetch_all(self.pool())
        .await
        .map_err(|_| AgentRunStoreError::PersistenceFailed)?;
        rows.into_iter()
            .map(|row| -> Result<SandboxProgress, sqlx::Error> {
                Ok(SandboxProgress {
                    track: serde_json::from_value(serde_json::Value::String(row.try_get("track")?))
                        .map_err(|error| sqlx::Error::Decode(Box::new(error)))?,
                    attempt: u32::try_from(row.try_get::<i64, _>("attempt_number")?)
                        .map_err(|error| sqlx::Error::Decode(Box::new(error)))?,
                    generation: u64::try_from(row.try_get::<i64, _>("execution_generation")?)
                        .map_err(|error| sqlx::Error::Decode(Box::new(error)))?,
                    task_run_id: row
                        .try_get::<uuid::Uuid, _>("task_run_id")?
                        .to_string()
                        .parse()
                        .map_err(|error| sqlx::Error::Decode(Box::new(error)))?,
                    state: row.try_get("state")?,
                    diagnostic: row.try_get("diagnostic_code")?,
                    submitted: row.try_get("submitted")?,
                    request_id: row
                        .try_get::<Option<uuid::Uuid>, _>("resource_request_id")?
                        .map(|id| id.to_string().parse())
                        .transpose()
                        .map_err(|error| sqlx::Error::Decode(Box::new(error)))?,
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| AgentRunStoreError::PersistenceFailed)
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
