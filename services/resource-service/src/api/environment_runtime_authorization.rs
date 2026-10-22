//! Environment-owned transport for the original Resource execution authorization.
use super::{ResourceApiError, ResourceApiState, require_environment_service};
use axum::{Extension, Json, extract::State};
use contracts::environment::{
    EnvironmentRuntimeAuthorizationResolution, ResolveEnvironmentRuntimeAuthorizationRequest,
};
use contracts::runtime_authorization::RuntimeAuthorizationError;

pub(super) async fn resolve(
    State(state): State<ResourceApiState>,
    Extension(identity): Extension<auth::ServiceIdentity>,
    Json(input): Json<ResolveEnvironmentRuntimeAuthorizationRequest>,
) -> Result<Json<EnvironmentRuntimeAuthorizationResolution>, ResourceApiError> {
    require_environment_service(&state, &identity)?;
    let current = crate::runtime_authorization::current_environment_authorization(
        &state.store.pool(),
        input.resource_request_id,
        input.expected_lease_id,
    )
    .await
    .map_err(crate::store::ResourceStoreError::from)?;
    let contracts::resource::ResourceTarget::Environment {
        environment_id,
        release_id,
        release_version,
    } = current.binding.target
    else {
        return Err(crate::store::ResourceStoreError::from(
            crate::runtime_authorization::RuntimeBindingError::from(
                RuntimeAuthorizationError::TargetNotEnvironment,
            ),
        )
        .into());
    };
    if current.lease.resource_request_id != input.resource_request_id
        || current.lease.project_id != input.project_id
        || current.lease.course_id != input.course_id
        || current.lease.owner_actor_id != input.owner_actor_id
        || current.lease.environment_id != environment_id
        || input
            .environment_id
            .is_some_and(|expected| expected != environment_id)
        || input.release_id != release_id
        || input.release_version != release_version
        || input
            .expected_lease_id
            .is_some_and(|id| id != current.lease.lease_id)
    {
        return Err(crate::store::ResourceStoreError::from(
            crate::runtime_authorization::RuntimeBindingError::from(
                RuntimeAuthorizationError::ConfigurationMismatch,
            ),
        )
        .into());
    }
    let policy = &current.applied_configuration.configuration.runtime_policy;
    let valid_until = current.lease.expires_at.min(current.approval_valid_until);
    Ok(Json(EnvironmentRuntimeAuthorizationResolution {
        authorization: current.binding,
        owner_actor_id: current.lease.owner_actor_id,
        lease_authorization: current.lease,
        provider_binding: current.provider_binding,
        valid_until,
        default_container_uid: policy.default_container_uid,
        privileged_execution_location: policy.privileged_execution_location.clone(),
    }))
}
