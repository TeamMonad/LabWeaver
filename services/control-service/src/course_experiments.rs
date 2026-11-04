#![allow(missing_docs)]
use super::{ControlError, ControlService, db, i64_revision};
use contracts::course_experiments::{
    CourseExperiment, CourseExperimentGroupView, CreateCourseExperimentRequest,
    CreateExperimentGroupRequest, ExperimentGroup, ExperimentPolicy,
};
use contracts::http::IdempotencyKey;
use contracts::{CourseExperimentId, CourseId, ExperimentGroupId, Revision};
use persistence_sqlx::{Domain, IdempotencyDecision, IdempotencyStore, Sha256Digest};
use sqlx::Row;
use uuid::Uuid;
const CREATE_EXPERIMENT: &str = "control_create_course_experiment_v1";
const CREATE_GROUP: &str = "control_create_experiment_group_v1";

fn decode_experiment(row: &sqlx::postgres::PgRow) -> Result<CourseExperiment, ControlError> {
    let value: CourseExperiment = serde_json::from_value(row.try_get("contract").map_err(db)?)
        .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
    if value.id.as_uuid() != row.try_get::<Uuid, _>("experiment_id").map_err(db)?
        || value.course_id.as_uuid() != row.try_get::<Uuid, _>("course_id").map_err(db)?
    {
        return Err(ControlError::PersistenceIdentityMismatch);
    }
    Ok(value)
}

impl ControlService {
    pub async fn create_course_experiment(
        &self,
        actor: &contracts::AuthenticatedActor,
        course_id: CourseId,
        request: &CreateCourseExperimentRequest,
        key: &IdempotencyKey,
    ) -> Result<CourseExperiment, ControlError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let hash = Sha256Digest::of_canonical(&(actor.actor_id, course_id, request))
            .map_err(|_| ControlError::ContractInvalid)?;
        match IdempotencyStore::reserve(
            &mut tx,
            Domain::Control,
            CREATE_EXPERIMENT,
            key.as_str(),
            hash,
        )
        .await
        .map_err(|_| ControlError::PersistenceFailed)?
        {
            IdempotencyDecision::Replay(value) => {
                return serde_json::from_value(value)
                    .map_err(|_| ControlError::PersistenceIdentityMismatch);
            }
            IdempotencyDecision::Conflict => return Err(ControlError::IdempotencyConflict),
            IdempotencyDecision::InProgress => return Err(ControlError::OperationInProgress),
            IdempotencyDecision::Reserved => {}
        }
        let staff: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM control.course_memberships WHERE course_id=$1 AND actor_id=$2 AND object_role='instructor' AND role='teacher' AND state='active' AND (expires_at IS NULL OR expires_at>clock_timestamp()))") .bind(course_id.as_uuid()).bind(actor.actor_id.as_uuid()).fetch_one(&mut *tx).await.map_err(db)?;
        if !staff
            && !actor
                .roles
                .contains(&contracts::PlatformRole::PlatformAdmin)
        {
            return Err(ControlError::CourseGovernanceDenied);
        }
        let now: time::OffsetDateTime =
            sqlx::query_scalar("SELECT date_trunc('milliseconds',clock_timestamp())")
                .fetch_one(&mut *tx)
                .await
                .map_err(db)?;
        let now = contracts::UtcTimestamp::from_utc(now)
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        let id = CourseExperimentId::new();
        let revision = Revision::new(1).map_err(|_| ControlError::ContractInvalid)?;
        let experiment = CourseExperiment {
            id,
            course_id,
            release_id: request.release_id,
            policy: request.policy,
            revision,
            created_at: now,
        };
        let contract =
            serde_json::to_value(&experiment).map_err(|_| ControlError::ContractInvalid)?;
        sqlx::query("INSERT INTO control.course_experiments(experiment_id,course_id,release_id,policy,revision,contract) VALUES($1,$2,$3,$4,$5,$6)").bind(id.as_uuid()).bind(course_id.as_uuid()).bind(request.release_id.as_uuid()).bind(match request.policy { ExperimentPolicy::Individual => "individual", ExperimentPolicy::Group => "group" }).bind(i64_revision(revision)?).bind(contract).execute(&mut *tx).await.map_err(db)?;
        IdempotencyStore::complete(
            &mut tx,
            Domain::Control,
            CREATE_EXPERIMENT,
            key.as_str(),
            &serde_json::to_value(&experiment).map_err(|_| ControlError::ContractInvalid)?,
        )
        .await
        .map_err(|_| ControlError::PersistenceFailed)?;
        tx.commit().await.map_err(db)?;
        Ok(experiment)
    }
    pub async fn course_experiment(
        &self,
        _actor: &contracts::AuthenticatedActor,
        course_id: CourseId,
        experiment_id: CourseExperimentId,
    ) -> Result<CourseExperiment, ControlError> {
        let row = sqlx::query("SELECT experiment_id,course_id,contract FROM control.course_experiments WHERE experiment_id=$1 AND course_id=$2").bind(experiment_id.as_uuid()).bind(course_id.as_uuid()).fetch_optional(&self.pool).await.map_err(db)?.ok_or(ControlError::NotFound)?;
        decode_experiment(&row)
    }
    pub async fn create_experiment_group(
        &self,
        actor: &contracts::AuthenticatedActor,
        course_id: CourseId,
        experiment_id: CourseExperimentId,
        request: &CreateExperimentGroupRequest,
        key: &IdempotencyKey,
    ) -> Result<CourseExperimentGroupView, ControlError> {
        request
            .validate()
            .map_err(|_| ControlError::ContractInvalid)?;
        let mut tx = self.pool.begin().await.map_err(db)?;
        let row = sqlx::query("SELECT experiment_id,course_id,contract,policy FROM control.course_experiments WHERE experiment_id=$1 AND course_id=$2 FOR UPDATE").bind(experiment_id.as_uuid()).bind(course_id.as_uuid()).fetch_optional(&mut *tx).await.map_err(db)?.ok_or(ControlError::NotFound)?;
        let experiment = decode_experiment(&row)?;
        if experiment.policy != ExperimentPolicy::Group {
            return Err(ControlError::ContractInvalid);
        }
        let hash = Sha256Digest::of_canonical(&(actor.actor_id, course_id, experiment_id, request))
            .map_err(|_| ControlError::ContractInvalid)?;
        match IdempotencyStore::reserve(&mut tx, Domain::Control, CREATE_GROUP, key.as_str(), hash)
            .await
            .map_err(|_| ControlError::PersistenceFailed)?
        {
            IdempotencyDecision::Replay(value) => {
                return serde_json::from_value(value)
                    .map_err(|_| ControlError::PersistenceIdentityMismatch);
            }
            IdempotencyDecision::Conflict => return Err(ControlError::IdempotencyConflict),
            IdempotencyDecision::InProgress => return Err(ControlError::OperationInProgress),
            IdempotencyDecision::Reserved => {}
        }
        let instructor: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM control.course_memberships WHERE course_id=$1 AND actor_id=$2 AND object_role='instructor' AND role='teacher' AND state='active' AND (expires_at IS NULL OR expires_at>clock_timestamp()))").bind(course_id.as_uuid()).bind(actor.actor_id.as_uuid()).fetch_one(&mut *tx).await.map_err(db)?;
        if !instructor
            && !actor
                .roles
                .contains(&contracts::PlatformRole::PlatformAdmin)
        {
            return Err(ControlError::CourseGovernanceDenied);
        }
        for member in &request.member_actor_ids {
            let ok: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM control.course_memberships WHERE course_id=$1 AND actor_id=$2 AND state='active')").bind(course_id.as_uuid()).bind(member.as_uuid()).fetch_one(&mut *tx).await.map_err(db)?;
            if !ok {
                return Err(ControlError::CourseGovernanceDenied);
            }
        }
        let now: time::OffsetDateTime =
            sqlx::query_scalar("SELECT date_trunc('milliseconds',clock_timestamp())")
                .fetch_one(&mut *tx)
                .await
                .map_err(db)?;
        let now = contracts::UtcTimestamp::from_utc(now)
            .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        let id = ExperimentGroupId::new();
        let revision = Revision::new(1).map_err(|_| ControlError::ContractInvalid)?;
        let group = ExperimentGroup {
            id,
            experiment_id,
            payer_project_id: request.payer_project_id,
            payer_actor_id: request.payer_actor_id,
            member_actor_ids: request.member_actor_ids.clone(),
            payer_consent: request.payer_consent,
            revision,
            created_at: now,
        };
        let contract = serde_json::to_value(&group).map_err(|_| ControlError::ContractInvalid)?;
        sqlx::query("INSERT INTO control.experiment_groups(group_id,experiment_id,payer_project_id,payer_actor_id,payer_consent,revision,contract) VALUES($1,$2,$3,$4,$5,$6,$7)").bind(id.as_uuid()).bind(experiment_id.as_uuid()).bind(request.payer_project_id.as_uuid()).bind(request.payer_actor_id.as_uuid()).bind(true).bind(i64_revision(revision)?).bind(contract).execute(&mut *tx).await.map_err(db)?;
        for member in &request.member_actor_ids {
            sqlx::query("INSERT INTO control.experiment_group_members(group_id,actor_id,revision,state) VALUES($1,$2,$3,'active')").bind(id.as_uuid()).bind(member.as_uuid()).bind(i64_revision(revision)?).execute(&mut *tx).await.map_err(db)?;
        }
        let output = CourseExperimentGroupView { experiment, group };
        IdempotencyStore::complete(
            &mut tx,
            Domain::Control,
            CREATE_GROUP,
            key.as_str(),
            &serde_json::to_value(&output).map_err(|_| ControlError::ContractInvalid)?,
        )
        .await
        .map_err(|_| ControlError::PersistenceFailed)?;
        tx.commit().await.map_err(db)?;
        Ok(output)
    }
    pub async fn experiment_groups(
        &self,
        actor: &contracts::AuthenticatedActor,
        course_id: CourseId,
        experiment_id: CourseExperimentId,
    ) -> Result<Vec<CourseExperimentGroupView>, ControlError> {
        let experiment = self
            .course_experiment(actor, course_id, experiment_id)
            .await?;
        let rows = sqlx::query("SELECT g.contract FROM control.experiment_groups g WHERE g.experiment_id=$1 ORDER BY g.group_id").bind(experiment_id.as_uuid()).fetch_all(&self.pool).await.map_err(db)?;
        rows.into_iter()
            .map(|row| {
                let group: ExperimentGroup =
                    serde_json::from_value(row.try_get("contract").map_err(db)?)
                        .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
                Ok(CourseExperimentGroupView {
                    experiment: experiment.clone(),
                    group,
                })
            })
            .collect()
    }
}
