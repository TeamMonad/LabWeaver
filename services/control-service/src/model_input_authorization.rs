//! Fresh checks against existing Access session and Control membership facts.
use super::{ControlError, ControlService, db};
use contracts::http::{
    ModelInputAuthorizationResolution, ModelInputAuthorizationResolveRequest,
    validate_model_input_selection,
};
use contracts::{AuthenticatedActor, AuthorizationScope, UtcTimestamp};
use sqlx::Row;
impl ControlService {
    /// Rechecks the current session, roles, membership and exact object versions.
    pub async fn resolve_model_input_authorization(
        &self,
        request: &ModelInputAuthorizationResolveRequest,
    ) -> Result<ModelInputAuthorizationResolution, ControlError> {
        let binding = &request.binding;
        let now = time::OffsetDateTime::now_utc();
        if binding.valid_until.get() <= now
            || !matches!(
                binding.operation_id.as_str(),
                "createProjectAgentRun" | "createProjectWorkConfigurationRun" | "createAgentRun"
            )
        {
            return Err(ControlError::ContractInvalid);
        }
        let row = sqlx::query("SELECT platform_roles, LEAST(expires_at,idle_expires_at) AS valid_until FROM access.current_authorization_sessions WHERE session_sha256=$1 AND actor_id=$2 AND expires_at>clock_timestamp() AND idle_expires_at>clock_timestamp()")
            .bind(&binding.session_sha256).bind(binding.actor_id.as_uuid()).fetch_optional(&self.pool).await.map_err(db)?.ok_or(ControlError::ContractInvalid)?;
        let roles: Vec<String> = row.try_get("platform_roles").map_err(db)?;
        let roles = roles
            .into_iter()
            .map(|role| serde_json::from_value(serde_json::Value::String(role)))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| ControlError::ContractInvalid)?;
        let actor = AuthenticatedActor {
            actor_id: binding.actor_id,
            roles,
            expires_at: UtcTimestamp::from_utc(row.try_get("valid_until").map_err(db)?)
                .map_err(|_| ControlError::ContractInvalid)?,
        };
        let memberships = auth::load_membership_snapshot(&self.pool, binding.actor_id.as_uuid())
            .await
            .map_err(|_| ControlError::ContractInvalid)?;
        let operation = contracts::operation_contract(&binding.operation_id)
            .ok_or(ControlError::ContractInvalid)?;
        let scope = if binding.operation_id == "createAgentRun" {
            AuthorizationScope::Course {
                course_id: binding.course_id.ok_or(ControlError::ContractInvalid)?,
            }
        } else {
            AuthorizationScope::Project {
                project_id: binding.project_id,
            }
        };
        auth::object_authorization::authorize_operation(
            &auth::AuthorizationContext {
                actor,
                course_memberships: memberships.course_memberships,
                project_memberships: memberships.project_memberships,
                now,
            },
            scope,
            operation,
        )
        .map_err(|_| ControlError::ContractInvalid)?;
        let project = self.project(binding.project_id).await?;
        if project.state == contracts::ProjectState::Archived
            || project.course_id != binding.course_id
        {
            return Err(ControlError::ProjectMismatch);
        }
        let package = self
            .project_package(binding.project_id, binding.package_id)
            .await?;
        let policy = self.active_project_policy(binding.project_id).await?;
        if package.revision != binding.package_revision
            || package.course_id != binding.course_id
            || policy.id != binding.policy_id
            || policy.revision != binding.policy_revision
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
