//! Rubric visibility and model-use facts in the existing versioned policy JSON.
use super::{
    ControlError, ControlService, advisory_project_lock, db, i64_revision, runtime_authorization,
};
use contracts::authoring::ProjectLlmEgressPolicy;
use contracts::http::{
    IdempotencyKey, MaintainRubricModelInputRuleRequest, ProjectRubricModelInputRules,
    RubricModelInputApproval, RubricModelInputRule, RubricModelInputRuleAction,
    RubricModelInputVisibility,
};
use contracts::{AuthenticatedActor, ProjectId, Revision, UtcTimestamp};
use persistence_sqlx::{Domain, IdempotencyDecision, IdempotencyStore, Sha256Digest};
use serde_json::Value;
use sqlx::Row;

pub(super) const RULES_FIELD: &str = "rubricModelInputRules";
const OPERATION: &str = "maintain_project_rubric_model_input_rule";

pub(super) fn decode_policy(
    mut value: Value,
) -> Result<(ProjectLlmEgressPolicy, Vec<RubricModelInputRule>), ControlError> {
    let rules = value
        .as_object_mut()
        .ok_or(ControlError::PolicyInvalid)?
        .remove(RULES_FIELD)
        .map(serde_json::from_value)
        .transpose()
        .map_err(|_| ControlError::PolicyInvalid)?
        .unwrap_or_default();
    let policy: ProjectLlmEgressPolicy =
        serde_json::from_value(value).map_err(|_| ControlError::PolicyInvalid)?;
    policy.validate().map_err(|_| ControlError::PolicyInvalid)?;
    let rules: Vec<RubricModelInputRule> = rules;
    if rules.len() > 128 {
        return Err(ControlError::PolicyInvalid);
    }
    let mut keys = std::collections::BTreeSet::new();
    for rule in &rules {
        if !keys.insert(rule_key(rule)?) {
            return Err(ControlError::PolicyInvalid);
        }
        rule.rubric
            .validate()
            .map_err(|_| ControlError::PolicyInvalid)?;
    }
    Ok((policy, rules))
}

fn rule_key(rule: &RubricModelInputRule) -> Result<String, ControlError> {
    Ok(Sha256Digest::of_canonical(&(
        rule.course_id,
        rule.evaluation_release_id,
        rule.evaluation_release_revision,
        &rule.rubric,
    ))
    .map_err(|_| ControlError::ContractInvalid)?
    .to_string())
}

pub(super) fn encode_policy(
    policy: &ProjectLlmEgressPolicy,
    rules: &[RubricModelInputRule],
) -> Result<Value, ControlError> {
    let mut value = serde_json::to_value(policy).map_err(|_| ControlError::ContractInvalid)?;
    if !rules.is_empty() {
        value[RULES_FIELD] =
            serde_json::to_value(rules).map_err(|_| ControlError::ContractInvalid)?;
    }
    Ok(value)
}

impl ControlService {
    /// Changes only the exact rubric rule. Verified actor evidence never comes from the request body.
    #[allow(
        clippy::too_many_lines,
        reason = "teacher, content, policy revision and idempotency commit in one transaction"
    )]
    pub async fn maintain_rubric_model_input_rule(
        &self,
        project_id: ProjectId,
        actor: &AuthenticatedActor,
        request: &MaintainRubricModelInputRuleRequest,
        expected_revision: Revision,
        key: &IdempotencyKey,
        now: UtcTimestamp,
    ) -> Result<ProjectRubricModelInputRules, ControlError> {
        request
            .rubric
            .validate()
            .map_err(|_| ControlError::ContractInvalid)?;
        let request_hash =
            Sha256Digest::of_canonical(&(project_id, actor.actor_id, expected_revision, request))
                .map_err(|_| ControlError::ContractInvalid)?;
        let mut tx = self.pool.begin().await.map_err(db)?;
        advisory_project_lock(&mut tx, project_id).await?;
        let project_row =
            sqlx::query("SELECT contract FROM control.projects WHERE project_id=$1 FOR SHARE")
                .bind(project_id.as_uuid())
                .fetch_optional(&mut *tx)
                .await
                .map_err(db)?
                .ok_or(ControlError::NotFound)?;
        let project: contracts::Project =
            serde_json::from_value(project_row.try_get("contract").map_err(db)?)
                .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        if project.id != project_id || project.state == contracts::ProjectState::Archived {
            return Err(ControlError::ProjectMismatch);
        }
        let course_id = project
            .course_id
            .ok_or(ControlError::TeacherApprovalDenied)?;
        let teacher = runtime_authorization::teacher_scope(&mut tx, actor, Some(course_id), now)
            .await?
            .ok_or(ControlError::TeacherApprovalDenied)?;
        if !super::object_authorization::permits_in_transaction(
            &mut tx,
            project_id,
            actor.actor_id,
            contracts::object_authorization::ObjectAction::Edit,
        )
        .await?
        {
            return Err(ControlError::TeacherApprovalDenied);
        }
        match IdempotencyStore::reserve(
            &mut tx,
            Domain::Control,
            OPERATION,
            key.as_str(),
            request_hash,
        )
        .await
        .map_err(|_| ControlError::PersistenceFailed)?
        {
            IdempotencyDecision::Replay(value) => {
                tx.rollback().await.map_err(db)?;
                return serde_json::from_value(value)
                    .map_err(|_| ControlError::PersistenceIdentityMismatch);
            }
            IdempotencyDecision::Conflict => return Err(ControlError::IdempotencyConflict),
            IdempotencyDecision::InProgress => return Err(ControlError::OperationInProgress),
            IdempotencyDecision::Reserved => {}
        }
        let publication: Value = sqlx::query_scalar("SELECT contract FROM control.authoring_approval_publications WHERE project_id=$1 AND course_id=$2 AND evaluation_release_id=$3 AND evaluation_release_revision=$4 AND state='ready' FOR SHARE")
            .bind(project_id.as_uuid()).bind(course_id.as_uuid()).bind(request.evaluation_release_id.as_uuid()).bind(i64_revision(request.evaluation_release_revision)?)
            .fetch_optional(&mut *tx).await.map_err(db)?.ok_or(ControlError::ReleaseNotFound)?;
        let publication: contracts::authoring::AuthoringApprovalPublicationStatus =
            serde_json::from_value(publication)
                .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        if publication.approval.project_id != project_id
            || publication.approval.course_id != Some(course_id)
            || publication.approval.package_id != request.rubric.package_id
            || publication.approval.package_revision != request.rubric.package_revision
            || publication.evaluation_release_id != Some(request.evaluation_release_id)
            || publication.evaluation_release_revision != Some(request.evaluation_release_revision)
        {
            return Err(ControlError::ProjectMismatch);
        }
        let package: Value = sqlx::query_scalar("SELECT contract FROM control.problem_packages WHERE project_id=$1 AND course_id=$2 AND package_id=$3 AND revision=$4 FOR SHARE")
            .bind(project_id.as_uuid()).bind(course_id.as_uuid()).bind(request.rubric.package_id.as_uuid()).bind(i64_revision(request.rubric.package_revision)?)
            .fetch_optional(&mut *tx).await.map_err(db)?.ok_or(ControlError::NotFound)?;
        if package["id"] != serde_json::json!(request.rubric.package_id)
            || package["revision"] != serde_json::json!(request.rubric.package_revision)
            || package["projectId"] != serde_json::json!(project_id)
            || package["courseId"] != serde_json::json!(Some(course_id))
        {
            return Err(ControlError::PersistenceIdentityMismatch);
        }
        let files = package["files"]
            .as_array()
            .ok_or(ControlError::PersistenceIdentityMismatch)?;
        let matching = files
            .iter()
            .filter(|file| file["path"] == request.rubric.path)
            .collect::<Vec<_>>();
        if matching.len() != 1
            || matching[0]["sha256"] != request.rubric.sha256
            || matching[0]["object"]
                != serde_json::to_value(&request.rubric.artifact)
                    .map_err(|_| ControlError::ContractInvalid)?
        {
            return Err(ControlError::ArtifactMismatch);
        }
        let row = sqlx::query("SELECT contract,revision FROM control.project_llm_policies WHERE project_id=$1 AND superseded_at IS NULL FOR UPDATE")
            .bind(project_id.as_uuid()).fetch_optional(&mut *tx).await.map_err(db)?.ok_or(ControlError::PolicyNotFound)?;
        let (mut policy, mut rules) = decode_policy(row.try_get("contract").map_err(db)?)?;
        if policy.project_id != project_id
            || policy.course_id != Some(course_id)
            || policy.revision != expected_revision
            || row.try_get::<i64, _>("revision").map_err(db)? != i64_revision(expected_revision)?
        {
            return Err(ControlError::RevisionConflict);
        }
        let position = rules.iter().position(|rule| {
            rule.course_id == course_id
                && rule.evaluation_release_id == request.evaluation_release_id
                && rule.evaluation_release_revision == request.evaluation_release_revision
                && rule.rubric == request.rubric
        });
        let mut rule = position
            .map(|index| rules[index].clone())
            .unwrap_or(RubricModelInputRule {
                course_id,
                evaluation_release_id: request.evaluation_release_id,
                evaluation_release_revision: request.evaluation_release_revision,
                rubric: request.rubric.clone(),
                visibility: RubricModelInputVisibility::Hidden,
                approval: None,
            });
        match &request.action {
            RubricModelInputRuleAction::SetVisibility { visibility } => {
                rule.visibility = *visibility;
                rule.approval = None;
            }
            RubricModelInputRuleAction::ApproveModelUse {
                runtime_binding,
                model,
            } => {
                if position.is_none() || rule.visibility != RubricModelInputVisibility::CoursePublic
                {
                    return Err(ControlError::TeacherApprovalDenied);
                }
                let mut target = policy.binding.clone();
                target.runtime_binding.clone_from(runtime_binding);
                target.model.clone_from(model);
                self.config
                    .llm_policy_options
                    .validate_policy_binding(&target)
                    .map_err(|_| ControlError::PolicyInvalid)?;
                rule.approval = Some(RubricModelInputApproval {
                    approved_by: teacher.actor_id,
                    approver_course_membership_revision: teacher.membership_revision,
                    runtime_binding: runtime_binding.clone(),
                    model: model.clone(),
                });
            }
            RubricModelInputRuleAction::RevokeModelUse => {
                if position.is_none() {
                    return Err(ControlError::NotFound);
                }
                rule.approval = None;
            }
        }
        if let Some(index) = position {
            rules[index] = rule;
        } else {
            if rules.len() >= 128 {
                return Err(ControlError::PolicyInvalid);
            }
            rules.push(rule);
        }
        let next: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(revision),0)+1 FROM control.project_llm_policies WHERE project_id=$1").bind(project_id.as_uuid()).fetch_one(&mut *tx).await.map_err(db)?;
        // policy_id is the immutable version row's primary key, not a reusable project identity.
        policy.id = contracts::PolicyId::new();
        policy.revision = super::revision_from_i64(next)?;
        policy.activated_at = now;
        let contract = encode_policy(&policy, &rules)?;
        sqlx::query("UPDATE control.project_llm_policies SET superseded_at=$2 WHERE project_id=$1 AND superseded_at IS NULL").bind(project_id.as_uuid()).bind(now.get()).execute(&mut *tx).await.map_err(db)?;
        sqlx::query("INSERT INTO control.project_llm_policies (policy_id,project_id,course_id,revision,contract_sha256,contract,activated_at) VALUES($1,$2,$3,$4,$5,$6,$7)")
            .bind(policy.id.as_uuid()).bind(project_id.as_uuid()).bind(course_id.as_uuid()).bind(next).bind(Sha256Digest::of_canonical(&contract).map_err(|_| ControlError::ContractInvalid)?.to_string()).bind(contract).bind(now.get()).execute(&mut *tx).await.map_err(db)?;
        let result = ProjectRubricModelInputRules {
            project_id,
            course_id,
            policy_id: policy.id,
            policy_revision: policy.revision,
            rules,
        };
        let value = serde_json::to_value(&result).map_err(|_| ControlError::ContractInvalid)?;
        IdempotencyStore::complete(&mut tx, Domain::Control, OPERATION, key.as_str(), &value)
            .await
            .map_err(|_| ControlError::PersistenceFailed)?;
        tx.commit().await.map_err(db)?;
        Ok(result)
    }
}

impl ControlService {
    /// Resolves metadata-only model permission from the current rubric policy and instructor.
    pub async fn current_rubric_permission(
        &self,
        project_id: ProjectId,
        course_id: contracts::CourseId,
        release_id: contracts::EvaluationReleaseId,
        release_revision: Revision,
        rubric: &contracts::http::ModelInputRubricIdentity,
        model_policy: &ProjectLlmEgressPolicy,
    ) -> Result<contracts::http::ModelInputRubricPermissionRef, ControlError> {
        let row = sqlx::query("SELECT contract FROM control.project_llm_policies WHERE project_id=$1 AND course_id=$2 AND superseded_at IS NULL")
            .bind(project_id.as_uuid()).bind(course_id.as_uuid()).fetch_optional(&self.pool).await.map_err(db)?.ok_or(ControlError::PolicyNotFound)?;
        let (policy, rules) = decode_policy(row.try_get("contract").map_err(db)?)?;
        if policy.project_id != project_id || policy.course_id != Some(course_id) {
            return Err(ControlError::ProjectMismatch);
        }
        let rule = rules
            .iter()
            .find(|rule| {
                rule.course_id == course_id
                    && rule.evaluation_release_id == release_id
                    && rule.evaluation_release_revision == release_revision
                    && rule.rubric == *rubric
            })
            .ok_or(ControlError::TeacherApprovalDenied)?;
        if rule.visibility != RubricModelInputVisibility::CoursePublic {
            return Err(ControlError::TeacherApprovalDenied);
        }
        let approval = rule
            .approval
            .as_ref()
            .ok_or(ControlError::TeacherApprovalDenied)?;
        if approval.model != model_policy.binding.model
            || approval.runtime_binding != model_policy.binding.runtime_binding
        {
            return Err(ControlError::TeacherApprovalDenied);
        }
        // Recheck that this exact policy is still active in the same statement as instructor
        // membership. A revoke committed during metadata decoding cannot leave the old rule valid.
        let current: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM control.course_memberships WHERE course_id=$1 AND actor_id=$2 AND revision=$3 AND role='teacher' AND object_role='instructor' AND state='active' AND (expires_at IS NULL OR expires_at>clock_timestamp())) AND EXISTS(SELECT 1 FROM control.project_llm_policies WHERE policy_id=$4 AND revision=$5 AND project_id=$6 AND course_id=$1 AND superseded_at IS NULL)")
            .bind(course_id.as_uuid()).bind(approval.approved_by.as_uuid()).bind(i64_revision(approval.approver_course_membership_revision)?)
            .bind(policy.id.as_uuid()).bind(i64_revision(policy.revision)?).bind(project_id.as_uuid())
            .fetch_one(&self.pool).await.map_err(db)?;
        if !current {
            return Err(ControlError::TeacherApprovalDenied);
        }
        Ok(contracts::http::ModelInputRubricPermissionRef {
            authoring_project_id: project_id,
            policy_id: policy.id,
            policy_revision: policy.revision,
            approved_by: approval.approved_by,
            approver_course_membership_revision: approval.approver_course_membership_revision,
        })
    }
}

pub(super) async fn load_policy(
    pool: &sqlx::PgPool,
    query: &str,
    id: uuid::Uuid,
) -> Result<ProjectLlmEgressPolicy, ControlError> {
    let value: Value = sqlx::query_scalar(query)
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(db)?
        .ok_or(ControlError::PolicyNotFound)?;
    Ok(decode_policy(value)?.0)
}

impl ControlService {
    /// Instructor-only metadata view; it does not expose rubric bytes.
    pub async fn project_rubric_model_input_rules(
        &self,
        project_id: ProjectId,
        actor: &AuthenticatedActor,
        now: UtcTimestamp,
    ) -> Result<ProjectRubricModelInputRules, ControlError> {
        let project = self.project(project_id).await?;
        let course_id = project
            .course_id
            .ok_or(ControlError::TeacherApprovalDenied)?;
        let mut tx = self.pool.begin().await.map_err(db)?;
        runtime_authorization::teacher_scope(&mut tx, actor, Some(course_id), now)
            .await?
            .ok_or(ControlError::TeacherApprovalDenied)?;
        if !super::object_authorization::permits_in_transaction(
            &mut tx,
            project_id,
            actor.actor_id,
            contracts::object_authorization::ObjectAction::Browse,
        )
        .await?
        {
            return Err(ControlError::TeacherApprovalDenied);
        }
        let value: Value = sqlx::query_scalar("SELECT contract FROM control.project_llm_policies WHERE project_id=$1 AND course_id=$2 AND superseded_at IS NULL")
            .bind(project_id.as_uuid()).bind(course_id.as_uuid()).fetch_optional(&mut *tx).await.map_err(db)?.ok_or(ControlError::PolicyNotFound)?;
        let (policy, rules) = decode_policy(value)?;
        tx.rollback().await.map_err(db)?;
        Ok(ProjectRubricModelInputRules {
            project_id,
            course_id,
            policy_id: policy.id,
            policy_revision: policy.revision,
            rules,
        })
    }
}
