//! Public candidate builds retain Agent's state, revision and cancellation authority.

use contracts::http::{
    CancelCandidateBuildRequest, CandidateBuildTarget, CandidateBuildTask, IdempotencyKey,
    InternalAgentBuildCancellationRequest, InternalAgentBuildCancellationResult,
    InternalAgentBuildStatusQuery,
};
use contracts::{ActorId, BuildRequestId, CandidateId, CourseId, ProjectId, UtcTimestamp};
use persistence_sqlx::{Domain, IdempotencyDecision, IdempotencyStore, Sha256Digest};
use serde_json::json;
use sqlx::Row;

use crate::clients::{AgentClient, DownstreamError};
use crate::{ControlError, ControlService, canonical_hash, db};

const CANCEL: &str = "control_cancel_candidate_build_v1";

struct BuildBinding {
    project_id: ProjectId,
    course_id: Option<CourseId>,
    build_request_id: BuildRequestId,
    candidate_id: CandidateId,
    target: CandidateBuildTarget,
}

impl BuildBinding {
    fn task(
        &self,
        status: InternalAgentBuildCancellationResult,
    ) -> Result<CandidateBuildTask, DownstreamError> {
        if status.project_id != self.project_id
            || status.course_id != self.course_id
            || status.build_request_id != self.build_request_id
        {
            return Err(DownstreamError::IdentityMismatch);
        }
        Ok(CandidateBuildTask {
            candidate_id: self.candidate_id,
            target: self.target,
            status,
        })
    }
}

impl ControlService {
    async fn candidate_build_binding(
        &self,
        project_id: ProjectId,
        candidate_id: CandidateId,
        target: CandidateBuildTarget,
    ) -> Result<BuildBinding, ControlError> {
        let (target_name, kind) = match target {
            CandidateBuildTarget::Environment => ("environment", "environment"),
            CandidateBuildTarget::EvaluationRunner => ("evaluation_runner", "evaluation"),
        };
        let row = sqlx::query(
            "SELECT b.build_request_id,b.course_id FROM control.container_build_projections b \
             JOIN control.candidates c ON c.candidate_id=b.candidate_id AND c.revision=b.candidate_revision \
               AND c.project_id=b.project_id AND c.course_id IS NOT DISTINCT FROM b.course_id \
               AND c.content_sha256=b.candidate_sha256 \
             WHERE b.project_id=$1 AND b.candidate_id=$2 AND b.target=$3 AND c.candidate_kind=$4",
        ).bind(project_id.as_uuid()).bind(candidate_id.as_uuid()).bind(target_name).bind(kind)
            .fetch_optional(&self.pool).await.map_err(db)?.ok_or(ControlError::CandidateNotFound)?;
        Ok(BuildBinding {
            project_id,
            candidate_id,
            target,
            course_id: row
                .try_get::<Option<uuid::Uuid>, _>("course_id")
                .map_err(db)?
                .map(|id| id.to_string().parse())
                .transpose()
                .map_err(|_| ControlError::PersistenceIdentityMismatch)?,
            build_request_id: row
                .try_get::<uuid::Uuid, _>("build_request_id")
                .map_err(db)?
                .to_string()
                .parse()
                .map_err(|_| ControlError::PersistenceIdentityMismatch)?,
        })
    }

    pub(crate) async fn candidate_build(
        &self,
        agent: &AgentClient,
        project_id: ProjectId,
        candidate_id: CandidateId,
        target: CandidateBuildTarget,
    ) -> Result<CandidateBuildTask, crate::api::ApiError> {
        let binding = self
            .candidate_build_binding(project_id, candidate_id, target)
            .await?;
        let status = agent
            .get_build(
                binding.build_request_id,
                &InternalAgentBuildStatusQuery {
                    project_id,
                    course_id: binding.course_id,
                },
            )
            .await?;
        Ok(binding.task(status)?)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "one actor-attributed candidate cancellation boundary"
    )]
    pub(crate) async fn cancel_candidate_build(
        &self,
        agent: &AgentClient,
        project_id: ProjectId,
        candidate_id: CandidateId,
        target: CandidateBuildTarget,
        request: &CancelCandidateBuildRequest,
        actor_id: ActorId,
        key: &IdempotencyKey,
    ) -> Result<CandidateBuildTask, crate::api::ApiError> {
        let binding = self
            .candidate_build_binding(project_id, candidate_id, target)
            .await?;
        if binding.build_request_id != request.build_request_id {
            return Err(ControlError::RevisionConflict.into());
        }
        let hash = canonical_hash(&json!({"projectId":project_id,"candidateId":candidate_id,
            "target":target,"buildRequestId":binding.build_request_id,"actorId":actor_id,"request":request}))?;
        // Preserve the first server timestamp in the existing ledger so a lost RPC response can
        // retry the exact Agent command. Commit this short reservation before any external call.
        let mut transaction = self.pool.begin().await.map_err(db)?;
        match IdempotencyStore::reserve(
            &mut transaction,
            Domain::Control,
            CANCEL,
            key.as_str(),
            hash,
        )
        .await
        .map_err(|_| ControlError::PersistenceFailed)?
        {
            IdempotencyDecision::Replay(value) => {
                transaction.rollback().await.map_err(db)?;
                return serde_json::from_value(value)
                    .map_err(|_| ControlError::PersistenceIdentityMismatch.into());
            }
            IdempotencyDecision::Conflict => return Err(ControlError::IdempotencyConflict.into()),
            IdempotencyDecision::Reserved | IdempotencyDecision::InProgress => {}
        }
        let requested_at = UtcTimestamp::from_utc(
            sqlx::query_scalar(
                "SELECT date_trunc('milliseconds',created_at) FROM control.idempotency_ledger \
             WHERE operation=$1 AND idempotency_key=$2",
            )
            .bind(CANCEL)
            .bind(key.as_str())
            .fetch_one(&mut *transaction)
            .await
            .map_err(db)?,
        )
        .map_err(|_| ControlError::ContractInvalid)?;
        transaction.commit().await.map_err(db)?;
        let internal_key = IdempotencyKey::parse(&format!(
            "candidate-build:{}",
            Sha256Digest::of_bytes(key.as_str().as_bytes())
        ))
        .map_err(|_| ControlError::ContractInvalid)?;
        let status = agent
            .cancel_build(
                binding.build_request_id,
                &InternalAgentBuildCancellationRequest {
                    project_id,
                    course_id: binding.course_id,
                    build_request_id: binding.build_request_id,
                    expected_state: request.expected_state,
                    expected_revision: request.expected_revision,
                    actor_id,
                    requested_at,
                },
                &internal_key,
            )
            .await?;
        if !status.cancellation_requested
            || status.state != request.expected_state
            || status.revision.get()
                != request
                    .expected_revision
                    .get()
                    .checked_add(1)
                    .ok_or(ControlError::ContractInvalid)?
        {
            return Err(DownstreamError::IdentityMismatch.into());
        }
        let task = binding.task(status)?;
        let mut transaction = self.pool.begin().await.map_err(db)?;
        // Concurrent exact requests may both receive Agent's stored response; only one completes
        // the Control ledger. The other returns the same canonical receipt.
        match IdempotencyStore::reserve(
            &mut transaction,
            Domain::Control,
            CANCEL,
            key.as_str(),
            hash,
        )
        .await
        .map_err(|_| ControlError::PersistenceFailed)?
        {
            IdempotencyDecision::Replay(value) => {
                transaction.rollback().await.map_err(db)?;
                return serde_json::from_value(value)
                    .map_err(|_| ControlError::PersistenceIdentityMismatch.into());
            }
            IdempotencyDecision::InProgress => {}
            _ => return Err(ControlError::IdempotencyConflict.into()),
        }
        IdempotencyStore::complete(
            &mut transaction,
            Domain::Control,
            CANCEL,
            key.as_str(),
            &serde_json::to_value(&task).map_err(|_| ControlError::ContractInvalid)?,
        )
        .await
        .map_err(|_| ControlError::PersistenceFailed)?;
        transaction.commit().await.map_err(db)?;
        Ok(task)
    }
}
