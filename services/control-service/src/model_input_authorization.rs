//! Fresh checks against existing Access session and Control membership facts.
use super::{ControlError, ControlService, db};
use contracts::http::{
    ModelInputAuthorizationResolution, ModelInputAuthorizationResolveRequest,
    validate_model_input_selection,
};
use contracts::{AuthenticatedActor, AuthorizationScope, UtcTimestamp};
use sqlx::Row;
impl ControlService {
    pub(super) async fn current_model_input_context(
        &self,
        context: &contracts::http::ModelInputAuthorizationContext,
    ) -> Result<auth::AuthorizationContext, ControlError> {
        context
            .validate()
            .map_err(|_| ControlError::ContractInvalid)?;
        let now = time::OffsetDateTime::now_utc();
        // Contract timestamps use milliseconds; floor only the projection, never the live expiry predicates.
        let row = sqlx::query("SELECT platform_roles, date_trunc('milliseconds',LEAST(expires_at,idle_expires_at)) AS valid_until FROM access.current_authorization_sessions WHERE session_sha256=$1 AND actor_id=$2 AND expires_at>clock_timestamp() AND idle_expires_at>clock_timestamp()")
            .bind(&context.session_sha256).bind(context.actor_id.as_uuid()).fetch_optional(&self.pool).await.map_err(db)?.ok_or(ControlError::ContractInvalid)?;
        let roles: Vec<String> = row.try_get("platform_roles").map_err(db)?;
        let roles = roles
            .into_iter()
            .map(|role| serde_json::from_value(serde_json::Value::String(role)))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| ControlError::ContractInvalid)?;
        let actor = AuthenticatedActor {
            actor_id: context.actor_id,
            roles,
            expires_at: UtcTimestamp::from_utc(row.try_get("valid_until").map_err(db)?)
                .map_err(|_| ControlError::ContractInvalid)?,
        };
        let memberships = auth::load_membership_snapshot(&self.pool, context.actor_id.as_uuid())
            .await
            .map_err(|_| ControlError::ContractInvalid)?;
        let operation = contracts::operation_contract(&context.operation_id)
            .ok_or(ControlError::ContractInvalid)?;
        let scope = if context.operation_id == "createAgentRun" {
            AuthorizationScope::Course {
                course_id: context.course_id.ok_or(ControlError::ContractInvalid)?,
            }
        } else {
            AuthorizationScope::Project {
                project_id: context.project_id,
            }
        };
        let authorization = auth::AuthorizationContext {
            actor,
            course_memberships: memberships.course_memberships,
            project_memberships: memberships.project_memberships,
            now,
        };
        auth::object_authorization::authorize_operation(&authorization, scope, operation)
            .map_err(|_| ControlError::ContractInvalid)?;
        Ok(authorization)
    }

    /// Rechecks the current session, roles, membership and exact object versions.
    pub async fn resolve_model_input_authorization(
        &self,
        request: &ModelInputAuthorizationResolveRequest,
    ) -> Result<ModelInputAuthorizationResolution, ControlError> {
        let binding = &request.binding;
        binding
            .validate()
            .map_err(|_| ControlError::ContractInvalid)?;
        let contracts::http::ModelInputAuthorizationSource::ProblemPackage {
            package_id,
            package_revision,
        } = binding.source
        else {
            // Frozen facts and rubric authorization are not connected yet. Never read objects.
            return Err(ControlError::ContractInvalid);
        };
        // valid_until is evidence for the original create decision, not this task's deadline.
        // Every invocation below reauthorizes the same original session against current facts.
        if !matches!(
            binding.operation_id.as_str(),
            "createProjectAgentRun" | "createProjectWorkConfigurationRun" | "createAgentRun"
        ) {
            return Err(ControlError::ContractInvalid);
        }
        self.current_model_input_context(&binding.context()).await?;
        let project = self.project(binding.project_id).await?;
        if project.state == contracts::ProjectState::Archived
            || project.course_id != binding.course_id
        {
            return Err(ControlError::ProjectMismatch);
        }
        let package = self.project_package(binding.project_id, package_id).await?;
        let policy = self.active_project_policy(binding.project_id).await?;
        if package.revision != package_revision
            || package.course_id != binding.course_id
            || policy.id != binding.policy_id
            || policy.revision != binding.policy_revision
            || contracts::http::model_input_policy_sha256(&policy)
                .map_err(|_| ControlError::ContractInvalid)?
                != binding.policy_sha256
        {
            return Err(ControlError::RevisionConflict);
        }
        let paths = validate_model_input_selection(&package, Some(&request.selected_paths))
            .map_err(|_| ControlError::ContractInvalid)?;
        if contracts::http::model_input_selection_sha256(&paths)
            .map_err(|_| ControlError::ContractInvalid)?
            != binding.selection_sha256
        {
            return Err(ControlError::ContractInvalid);
        }
        Ok(ModelInputAuthorizationResolution {
            binding: binding.clone(),
        })
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FrozenFacts {
    frozen_submission_id: contracts::FrozenSubmissionId,
    project_id: contracts::ProjectId,
    course_id: Option<contracts::CourseId>,
    owner_actor_id: contracts::ActorId,
    manifest_revision: contracts::Revision,
    manifest_sha256: String,
    manifest: contracts::submission::SubmissionManifest,
    archive: contracts::ArtifactRef,
    content_sha256: String,
    // Purpose-specific projection of #197's already published four-field FrozenFile JSON.
    files: Vec<serde_json::Value>,
    environment: contracts::submission::FrozenEnvironmentIdentity,
    evaluation_release: ReleaseFact,
    input_authorization_context: Option<contracts::http::ModelInputAuthorizationContext>,
}
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReleaseFact {
    id: contracts::EvaluationReleaseId,
    revision: contracts::Revision,
    state: contracts::evaluation::EvaluationReleaseState,
}

impl ControlService {
    /// Frozen object reads and model use require separate current actions and course membership.
    pub async fn authorize_frozen_model_context(
        &self,
        context: &contracts::http::ModelInputAuthorizationContext,
    ) -> Result<(), ControlError> {
        if context.operation_id != "freezeSubmission" {
            return Err(ControlError::ContractInvalid);
        }
        let authorization = self.current_model_input_context(context).await?;
        let course = context
            .course_id
            .ok_or(ControlError::CourseMembershipRequired)?;
        auth::authorize(
            &authorization,
            contracts::AuthorizationScope::Course { course_id: course },
            &std::collections::BTreeSet::from([
                contracts::PlatformRole::Teacher,
                contracts::PlatformRole::Student,
            ]),
        )
        .map_err(|_| ControlError::ContractInvalid)?;
        auth::object_authorization::authorize_operation(
            &authorization,
            contracts::AuthorizationScope::Project {
                project_id: context.project_id,
            },
            contracts::operation_contract("getFrozenSubmission")
                .ok_or(ControlError::ContractInvalid)?,
        )
        .map_err(|_| ControlError::ContractInvalid)?;
        auth::object_authorization::authorize_project_action(
            &authorization,
            context.project_id,
            contracts::object_authorization::ObjectAction::ConfigureAgent,
        )
        .map_err(|_| ControlError::ContractInvalid)?;
        let project = self.project(context.project_id).await?;
        if project.state == contracts::ProjectState::Archived
            || project.course_id != context.course_id
        {
            return Err(ControlError::ProjectMismatch);
        }
        Ok(())
    }

    /// Assembles a frozen binding from protected metadata and current Control authority only.
    #[allow(
        clippy::too_many_lines,
        reason = "one frozen binding compares all independent ledger and publication identities"
    )]
    pub async fn bind_frozen_model_input(
        &self,
        request: &contracts::http::CreateFrozenModelInputAuthorizationRequest,
        facts: serde_json::Value,
        release: &contracts::evaluation::EvaluationRelease,
    ) -> Result<contracts::http::ModelInputAuthorizationBinding, ControlError> {
        use contracts::http::{
            FrozenModelInputSource, ModelInputAuthorizationBinding, ModelInputAuthorizationSource,
            ModelInputRubricIdentity,
        };
        let context = &request.input_authorization_context;
        self.authorize_frozen_model_context(context).await?;
        let facts: FrozenFacts =
            serde_json::from_value(facts).map_err(|_| ControlError::ContractInvalid)?;
        let manifest_hash = persistence_sqlx::Sha256Digest::of_canonical(&facts.manifest)
            .map_err(|_| ControlError::ContractInvalid)?
            .to_string();
        if facts.input_authorization_context.as_ref() != Some(context)
            || facts.project_id != context.project_id
            || facts.course_id != context.course_id
            || facts.owner_actor_id != context.actor_id
            || facts.frozen_submission_id != request.frozen_submission_id
            || facts.evaluation_release.id != request.evaluation_release_id
            || facts.evaluation_release.revision != request.evaluation_release_revision
            || facts.evaluation_release.state
                != contracts::evaluation::EvaluationReleaseState::Active
            || release.id != facts.evaluation_release.id
            || release.revision != facts.evaluation_release.revision
            || release.state != contracts::evaluation::EvaluationReleaseState::Active
            || release.project_id != facts.project_id
            || release.course_id != facts.course_id
            || manifest_hash != facts.manifest_sha256
        {
            return Err(ControlError::ProjectMismatch);
        }
        let admission = self
            .environment_publication_admission(
                facts.environment.release_id,
                &contracts::http::EnvironmentPublicationAdmissionQuery {
                    project_id: release.project_id,
                    course_id: release.course_id,
                    environment_release_version: facts.environment.release_version,
                },
            )
            .await?
            .ok_or(ControlError::ReleaseEvidenceInvalid)?;
        if admission.evaluation_release_id != release.id
            || admission.evaluation_release_revision != release.revision
        {
            return Err(ControlError::ReleaseCandidateMismatch);
        }
        let mut selected = request.selected_paths.clone();
        selected.sort();
        if selected.is_empty() || selected.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(ControlError::ContractInvalid);
        }
        let matching_steps = release
            .evaluation_spec
            .body()
            .steps()
            .iter()
            .filter_map(|step| step.advisory_runner())
            .filter_map(|runner| match runner {
                contracts::evaluation::AdvisoryRunnerSpec::LlmReview {
                    include, rubric, ..
                } => {
                    let mut include = include.clone();
                    include.sort();
                    (include == selected).then_some(rubric.as_str())
                }
            })
            .collect::<Vec<_>>();
        if matching_steps.len() != 1 {
            return Err(ControlError::ContractInvalid);
        }
        let rubric_path = matching_steps[0]
            .strip_prefix("evaluator://")
            .unwrap_or(matching_steps[0]);
        let publication: serde_json::Value = sqlx::query_scalar("SELECT contract FROM control.authoring_approval_publications WHERE project_id=$1 AND evaluation_release_id=$2 AND evaluation_release_revision=$3 AND state='ready'")
            .bind(release.project_id.as_uuid()).bind(release.id.as_uuid()).bind(super::i64_revision(release.revision)?).fetch_optional(&self.pool).await.map_err(db)?.ok_or(ControlError::ReleaseNotFound)?;
        let publication: contracts::authoring::AuthoringApprovalPublicationStatus =
            serde_json::from_value(publication)
                .map_err(|_| ControlError::PersistenceIdentityMismatch)?;
        if publication.approval.project_id != release.project_id
            || publication.approval.course_id != release.course_id
            || publication.evaluation_release_id != Some(release.id)
            || publication.evaluation_release_revision != Some(release.revision)
        {
            return Err(ControlError::PersistenceIdentityMismatch);
        }
        let package: serde_json::Value = sqlx::query_scalar("SELECT contract FROM control.problem_packages WHERE package_id=$1 AND project_id=$2 AND revision=$3")
            .bind(publication.approval.package_id.as_uuid()).bind(release.project_id.as_uuid()).bind(super::i64_revision(publication.approval.package_revision)?).fetch_optional(&self.pool).await.map_err(db)?.ok_or(ControlError::NotFound)?;
        if package["id"] != serde_json::json!(publication.approval.package_id)
            || package["revision"] != serde_json::json!(publication.approval.package_revision)
            || package["projectId"] != serde_json::json!(release.project_id)
            || package["courseId"] != serde_json::json!(release.course_id)
        {
            return Err(ControlError::PersistenceIdentityMismatch);
        }
        let rubric_files = package["files"]
            .as_array()
            .ok_or(ControlError::ContractInvalid)?
            .iter()
            .filter(|file| file["path"] == rubric_path)
            .collect::<Vec<_>>();
        if rubric_files.len() != 1 {
            return Err(ControlError::ContractInvalid);
        }
        let rubric = ModelInputRubricIdentity {
            package_id: publication.approval.package_id,
            package_revision: publication.approval.package_revision,
            path: rubric_path.to_owned(),
            artifact: serde_json::from_value(rubric_files[0]["object"].clone())
                .map_err(|_| ControlError::ContractInvalid)?,
            sha256: rubric_files[0]["sha256"]
                .as_str()
                .ok_or(ControlError::ContractInvalid)?
                .to_owned(),
        };
        rubric
            .validate()
            .map_err(|_| ControlError::ContractInvalid)?;
        let policy = self.active_project_policy(context.project_id).await?;
        let permission = self
            .current_rubric_permission(
                release.project_id,
                context
                    .course_id
                    .ok_or(ControlError::CourseMembershipRequired)?,
                release.id,
                release.revision,
                &rubric,
                &policy,
            )
            .await?;
        let mut file_hashes = Vec::new();
        for path in &selected {
            contracts::validate_relative_path(path).map_err(|_| ControlError::ContractInvalid)?;
            if !manifest_rule_contains(&facts.manifest.include, path)
                || !manifest_rule_contains(&facts.manifest.llm_readable, path)
                || manifest_rule_contains(&facts.manifest.exclude, path)
            {
                return Err(ControlError::ContractInvalid);
            }
            let files = facts
                .files
                .iter()
                .filter(|file| file["path"] == *path)
                .collect::<Vec<_>>();
            if files.len() != 1 {
                return Err(ControlError::ContractInvalid);
            }
            let sha = files[0]["sha256"]
                .as_str()
                .ok_or(ControlError::ContractInvalid)?
                .to_owned();
            file_hashes.push((path.clone(), sha));
        }
        let source = FrozenModelInputSource {
            task_run_id: request.task_run_id,
            frozen_submission_id: facts.frozen_submission_id,
            manifest_revision: facts.manifest_revision,
            manifest_sha256: facts.manifest_sha256,
            archive: facts.archive,
            content_sha256: facts.content_sha256,
            evaluation_release_id: release.id,
            evaluation_release_revision: release.revision,
            environment: facts.environment,
            rubric,
            rubric_permission: Some(permission),
        };
        let selection_sha256 =
            contracts::http::frozen_model_input_selection_sha256(&source, &file_hashes)
                .map_err(|_| ControlError::ContractInvalid)?;
        let binding = ModelInputAuthorizationBinding {
            session_sha256: context.session_sha256.clone(),
            actor_id: context.actor_id,
            project_id: context.project_id,
            course_id: context.course_id,
            operation_id: context.operation_id.clone(),
            valid_until: context.valid_until,
            policy_id: policy.id,
            policy_revision: policy.revision,
            policy_sha256: contracts::http::model_input_policy_sha256(&policy)
                .map_err(|_| ControlError::ContractInvalid)?,
            source: ModelInputAuthorizationSource::FrozenSubmission(Box::new(source)),
            selection_sha256,
        };
        binding
            .validate()
            .map_err(|_| ControlError::ContractInvalid)?;
        Ok(binding)
    }
}
fn manifest_rule_contains(rules: &[contracts::PathRule], path: &str) -> bool {
    rules.iter().any(|rule| {
        rule.path() == path
            || rule.path().strip_suffix("/**").is_some_and(|prefix| {
                path.strip_prefix(prefix)
                    .is_some_and(|tail| tail.starts_with('/') && tail.len() > 1)
            })
    })
}
