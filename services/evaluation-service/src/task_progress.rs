//! Owner-scoped task reads derived from existing Evaluation attempts and Resource facts.
#![allow(missing_docs, clippy::missing_errors_doc)]

use contracts::evaluation::{
    EvaluationRun, EvaluationRunTaskProgress, EvaluationStepRunState, EvaluationStepTaskProgress,
};
use contracts::execution::{TaskAttemptPhase, TaskAttemptProgress};
use contracts::http::TaskProgressQuery;
use contracts::resource::ResourceRequestState;
use contracts::{ActorId, EvaluationRunId, EvaluationStepRunId, ProjectId, TaskRunId};
use sqlx::Row;
use task_execution::resource::{ResourceClient, ResourceClientError};

use crate::control_plane::{EvaluationControlStoreError, PgEvaluationControlStore};

#[derive(Debug, thiserror::Error)]
pub enum EvaluationTaskProgressError {
    #[error(transparent)]
    Store(#[from] EvaluationControlStoreError),
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
struct AttemptProgress {
    step_run_id: EvaluationStepRunId,
    created_at: time::OffsetDateTime,
    attempt: u32,
    task_run_id: TaskRunId,
    state: String,
    diagnostic: Option<String>,
    cleanup_verified: bool,
    submitted: bool,
    has_intent: bool,
    request_id: Option<contracts::ResourceRequestId>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct HistoryPosition(time::OffsetDateTime, uuid::Uuid, i32);

impl PgEvaluationControlStore {
    /// Resolves the most recently created owned run after an asynchronous freeze dispatch.
    pub async fn task_progress_for_submission(
        &self,
        project_id: ProjectId,
        actor_id: ActorId,
        submission_id: contracts::FrozenSubmissionId,
        resource: Option<&ResourceClient>,
        query: &TaskProgressQuery,
    ) -> Result<contracts::evaluation::EvaluationRunTaskProgress, EvaluationTaskProgressError> {
        query
            .validate()
            .map_err(|_| EvaluationTaskProgressError::PageInvalid)?;
        let run_id: uuid::Uuid = sqlx::query_scalar(
            "SELECT run_id FROM evaluation.evaluation_runs
            WHERE project_id=$1 AND actor_id=$2 AND frozen_submission_id=$3
            ORDER BY created_at DESC,run_id DESC LIMIT 1",
        )
        .bind(project_id.as_uuid())
        .bind(actor_id.as_uuid())
        .bind(submission_id.as_uuid())
        .fetch_optional(self.pool())
        .await
        .map_err(EvaluationControlStoreError::from)?
        .ok_or(EvaluationControlStoreError::RunNotFound)?;
        self.task_progress_scoped(
            project_id,
            actor_id,
            run_id
                .to_string()
                .parse()
                .map_err(|_| EvaluationControlStoreError::ContractInvalid)?,
            resource,
            query,
            Some(submission_id),
        )
        .await
    }

    /// Applies actor/project ownership before looking up attempts or contacting Resource.
    pub async fn task_progress(
        &self,
        project_id: ProjectId,
        actor_id: ActorId,
        run_id: EvaluationRunId,
        resource: Option<&ResourceClient>,
        query: &TaskProgressQuery,
    ) -> Result<EvaluationRunTaskProgress, EvaluationTaskProgressError> {
        self.task_progress_scoped(project_id, actor_id, run_id, resource, query, None)
            .await
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    async fn task_progress_scoped(
        &self,
        project_id: ProjectId,
        actor_id: ActorId,
        run_id: EvaluationRunId,
        resource: Option<&ResourceClient>,
        query: &TaskProgressQuery,
        submission_id: Option<contracts::FrozenSubmissionId>,
    ) -> Result<EvaluationRunTaskProgress, EvaluationTaskProgressError> {
        query
            .validate()
            .map_err(|_| EvaluationTaskProgressError::PageInvalid)?;
        if self.cursor_signing_key_current.is_empty() {
            return Err(EvaluationTaskProgressError::Store(
                EvaluationControlStoreError::ContractInvalid,
            ));
        }
        // Read only the immutable course binding here so even a cross-actor cursor
        // is rejected as an invalid scope before the actor-owned attempt query.
        let course_id: Option<uuid::Uuid> =
            sqlx::query_scalar("SELECT course_id FROM evaluation.evaluation_runs WHERE run_id=$1")
                .bind(run_id.as_uuid())
                .fetch_optional(self.pool())
                .await
                .map_err(EvaluationControlStoreError::from)?
                .ok_or(EvaluationControlStoreError::RunNotFound)?;
        let scope = (
            "evaluation-task-progress",
            project_id,
            actor_id,
            run_id,
            course_id,
            submission_id,
        );
        let after: Option<HistoryPosition> = query
            .cursor
            .as_deref()
            .map(|value| {
                persistence_sqlx::decode_cursor(
                    "evaluation",
                    self.cursor_signing_key_current.as_slice(),
                    self.cursor_signing_key_previous
                        .as_deref()
                        .map(Vec::as_slice),
                    &scope,
                    value,
                )
                .map_err(|_| EvaluationTaskProgressError::PageInvalid)
            })
            .transpose()?;
        let limit = query.limit.unwrap_or(50);
        for _ in 0..3 {
            let run = self
                .owned_progress_run(project_id, actor_id, run_id)
                .await?;
            if run.course_id.map(contracts::CourseId::as_uuid) != course_id {
                return Err(EvaluationTaskProgressError::PageInvalid);
            }
            let release = self.load_release(run.release_id).await?;
            let attempts = self
                .progress_attempts(project_id, actor_id, run_id, true, None, limit)
                .await?;
            let past = self
                .progress_attempts(project_id, actor_id, run_id, false, after.as_ref(), limit)
                .await?;
            let mut current = Vec::new();
            for step in &run.steps {
                let saved = attempts.iter().find(|saved| saved.step_run_id == step.id);
                let native_phase = match step.state {
                    EvaluationStepRunState::Pending => TaskAttemptPhase::Queued,
                    EvaluationStepRunState::Running => TaskAttemptPhase::Preparing,
                    EvaluationStepRunState::Succeeded => TaskAttemptPhase::Succeeded,
                    EvaluationStepRunState::Retryable | EvaluationStepRunState::Failed => {
                        TaskAttemptPhase::Failed
                    }
                    EvaluationStepRunState::Cancelled => TaskAttemptPhase::Cancelled,
                    EvaluationStepRunState::Skipped => TaskAttemptPhase::Blocked,
                };
                let mut progress = saved.map_or_else(
                    || TaskAttemptProgress {
                        attempt: step.current_attempt,
                        execution_generation: None,
                        task_run_id: None,
                        phase: native_phase,
                        resource_request_id: None,
                        resource_state: None,
                        diagnostic_code: step
                            .diagnostic_code
                            .as_ref()
                            .map(|code| code.as_str().to_owned()),
                        next_steps: vec!["getOwnProjectEvaluationTaskProgress".to_owned()],
                    },
                    |saved| projection(saved).progress,
                );
                if let Some(saved) = saved.filter(|saved| saved.state == "running") {
                    if step_needs_resource(&release.evaluation_spec, &step.step_id)? {
                        apply_live_resource(&mut progress, saved, project_id, actor_id, resource)
                            .await?;
                    } else {
                        progress.phase = if saved.has_intent {
                            TaskAttemptPhase::Executing
                        } else {
                            TaskAttemptPhase::Preparing
                        };
                    }
                }
                current.push(EvaluationStepTaskProgress {
                    step_run_id: step.id,
                    progress,
                });
            }
            if self
                .owned_progress_run(project_id, actor_id, run_id)
                .await?
                .revision
                != run.revision
                || self
                    .progress_attempts(project_id, actor_id, run_id, true, None, limit)
                    .await?
                    != attempts
                || self
                    .progress_attempts(project_id, actor_id, run_id, false, after.as_ref(), limit)
                    .await?
                    != past
            {
                continue;
            }
            let next_cursor = if past.len() > usize::from(limit) {
                let last = &past[usize::from(limit) - 1];
                Some(
                    persistence_sqlx::encode_cursor(
                        "evaluation",
                        self.cursor_signing_key_current.as_slice(),
                        &scope,
                        HistoryPosition(
                            last.created_at,
                            last.step_run_id.as_uuid(),
                            i32::try_from(last.attempt)
                                .map_err(|_| EvaluationTaskProgressError::PageInvalid)?,
                        ),
                    )
                    .map_err(|_| EvaluationTaskProgressError::PageInvalid)?,
                )
            } else {
                None
            };
            let history = past
                .into_iter()
                .take(usize::from(limit))
                .map(|saved| projection(&saved))
                .collect();
            return Ok(EvaluationRunTaskProgress {
                run_id,
                project_id,
                revision: run.revision,
                state: run.state,
                current,
                history,
                next_cursor,
            });
        }
        Err(EvaluationTaskProgressError::Changed)
    }

    async fn owned_progress_run(
        &self,
        project_id: ProjectId,
        actor_id: ActorId,
        run_id: EvaluationRunId,
    ) -> Result<EvaluationRun, EvaluationControlStoreError> {
        let value: serde_json::Value = sqlx::query_scalar(
            "SELECT contract
            FROM evaluation.evaluation_runs WHERE run_id=$1 AND project_id=$2 AND actor_id=$3",
        )
        .bind(run_id.as_uuid())
        .bind(project_id.as_uuid())
        .bind(actor_id.as_uuid())
        .fetch_optional(self.pool())
        .await?
        .ok_or(EvaluationControlStoreError::RunNotFound)?;
        let run: EvaluationRun = serde_json::from_value(value)
            .map_err(|_| EvaluationControlStoreError::ContractInvalid)?;
        run.validate()?;
        if run.id != run_id || run.project_id != project_id || run.actor_id != actor_id {
            return Err(EvaluationControlStoreError::IdentityMismatch);
        }
        Ok(run)
    }

    #[allow(clippy::too_many_arguments)]
    async fn progress_attempts(
        &self,
        project_id: ProjectId,
        actor_id: ActorId,
        run_id: EvaluationRunId,
        current: bool,
        after: Option<&HistoryPosition>,
        limit: u16,
    ) -> Result<Vec<AttemptProgress>, EvaluationControlStoreError> {
        let rows = sqlx::query("SELECT a.step_run_id,a.attempt,a.task_run_id,a.state,a.created_at,
            a.cleanup_verified,a.diagnostic_code,a.execution_resources IS NOT NULL AS has_intent,
            COALESCE(jsonb_array_length(a.execution_resources->'objects')>0,false) AS submitted,
            (a.execution_resources->'admission'->>'resourceRequestId')::uuid AS resource_request_id
            FROM evaluation.evaluation_step_attempts a
            JOIN evaluation.evaluation_step_runs s ON s.step_run_id=a.step_run_id
            JOIN evaluation.evaluation_runs r ON r.run_id=s.run_id
            WHERE r.run_id=$1 AND r.project_id=$2 AND r.actor_id=$3
                AND (($4 AND a.attempt=s.current_attempt) OR (NOT $4 AND a.attempt<s.current_attempt))
                AND ($5::timestamptz IS NULL OR (a.created_at,a.step_run_id,a.attempt)<($5,$6,$7))
            ORDER BY a.created_at DESC,a.step_run_id DESC,a.attempt DESC LIMIT CASE WHEN $4 THEN NULL ELSE $8 END")
            .bind(run_id.as_uuid()).bind(project_id.as_uuid()).bind(actor_id.as_uuid()).bind(current)
            .bind(after.map(|key| key.0)).bind(after.map(|key| key.1)).bind(after.map(|key| key.2))
            .bind(i64::from(limit)+1).fetch_all(self.pool()).await?;
        rows.into_iter()
            .map(|row| {
                Ok(AttemptProgress {
                    created_at: row.try_get("created_at")?,
                    step_run_id: row
                        .try_get::<uuid::Uuid, _>("step_run_id")?
                        .to_string()
                        .parse()
                        .map_err(|_| EvaluationControlStoreError::ContractInvalid)?,
                    attempt: u32::try_from(row.try_get::<i32, _>("attempt")?)
                        .map_err(|_| EvaluationControlStoreError::ContractInvalid)?,
                    task_run_id: row
                        .try_get::<uuid::Uuid, _>("task_run_id")?
                        .to_string()
                        .parse()
                        .map_err(|_| EvaluationControlStoreError::ContractInvalid)?,
                    state: row.try_get("state")?,
                    diagnostic: row.try_get("diagnostic_code")?,
                    cleanup_verified: row.try_get("cleanup_verified")?,
                    submitted: row.try_get("submitted")?,
                    has_intent: row.try_get("has_intent")?,
                    request_id: row
                        .try_get::<Option<uuid::Uuid>, _>("resource_request_id")?
                        .map(|id| id.to_string().parse())
                        .transpose()
                        .map_err(|_| EvaluationControlStoreError::ContractInvalid)?,
                })
            })
            .collect()
    }
}

fn projection(saved: &AttemptProgress) -> EvaluationStepTaskProgress {
    let phase = match saved.state.as_str() {
        "running" => TaskAttemptPhase::Preparing,
        _ if !saved.cleanup_verified => TaskAttemptPhase::CleanupPending,
        "succeeded" => TaskAttemptPhase::Succeeded,
        "cancelled" => TaskAttemptPhase::Cancelled,
        _ => TaskAttemptPhase::Failed,
    };
    EvaluationStepTaskProgress {
        step_run_id: saved.step_run_id,
        progress: TaskAttemptProgress {
            attempt: saved.attempt,
            execution_generation: Some(u64::from(saved.attempt)),
            task_run_id: Some(saved.task_run_id),
            phase,
            resource_request_id: saved.request_id,
            resource_state: None,
            diagnostic_code: saved.diagnostic.clone(),
            next_steps: vec!["getOwnProjectEvaluationTaskProgress".to_owned()],
        },
    }
}

fn step_needs_resource(
    spec: &contracts::evaluation::EvaluationSpec,
    step_id: &str,
) -> Result<bool, EvaluationControlStoreError> {
    let declared = spec
        .body()
        .steps()
        .iter()
        .find(|step| step.id() == step_id)
        .ok_or(EvaluationControlStoreError::ContractInvalid)?;
    let plan = crate::execution::plan_deterministic_step(declared)
        .map_err(|_| EvaluationControlStoreError::ContractInvalid)?;
    Ok(matches!(
        plan,
        crate::execution::StepExecutionPlan::Program { .. }
            | crate::execution::StepExecutionPlan::AnsibleProbe { .. }
    ))
}

async fn apply_live_resource(
    progress: &mut TaskAttemptProgress,
    saved: &AttemptProgress,
    project_id: ProjectId,
    actor_id: ActorId,
    resource: Option<&ResourceClient>,
) -> Result<(), EvaluationTaskProgressError> {
    let resource = resource.ok_or(EvaluationTaskProgressError::ResourceUnavailable)?;
    let request = match resource.get_task_resource_request(saved.task_run_id).await {
        Ok(request) => request,
        Err(ResourceClientError::RequestMissing) if !saved.submitted => return Ok(()),
        Err(_) => return Err(EvaluationTaskProgressError::ResourceUnavailable),
    };
    if request.project_id != project_id || request.requester_id != actor_id {
        return Err(EvaluationTaskProgressError::IdentityMismatch);
    }
    progress.resource_request_id = Some(request.id);
    progress.resource_state = Some(request.state);
    if request.diagnostic_code.is_some() {
        progress.diagnostic_code = request.diagnostic_code;
    }
    progress.phase = match request.state {
        ResourceRequestState::Reviewing => TaskAttemptPhase::WaitingResourceApproval,
        ResourceRequestState::Active
            if progress.diagnostic_code.as_deref() == Some("LW_RESOURCE_TASK_LEASE_EXPIRED") =>
        {
            TaskAttemptPhase::CleanupPending
        }
        ResourceRequestState::Active if saved.submitted => TaskAttemptPhase::Executing,
        ResourceRequestState::Allocating | ResourceRequestState::Active => {
            TaskAttemptPhase::Allocating
        }
        ResourceRequestState::Expiring => TaskAttemptPhase::CleanupPending,
        _ => TaskAttemptPhase::Blocked,
    };
    Ok(())
}
