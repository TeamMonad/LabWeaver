//! Read-only Environment transport over the existing Resource approval authority.
use super::{ResourceApiError, ResourceApiState, require_environment_service};
use axum::{Extension, Json, extract::State};
use contracts::environment::{
    EnvironmentRuntimeAuthorizationResolution, ResolveEnvironmentRuntimeAuthorizationRequest,
};

pub(super) async fn resolve(
    State(state): State<ResourceApiState>,
    Extension(identity): Extension<auth::ServiceIdentity>,
    Json(input): Json<ResolveEnvironmentRuntimeAuthorizationRequest>,
) -> Result<Json<EnvironmentRuntimeAuthorizationResolution>, ResourceApiError> {
    require_environment_service(&state, &identity)?;
    let current = crate::runtime_authorization::resolve_environment_authorization(
        &state.store.pool(),
        &input,
    )
    .await
    .map_err(crate::store::ResourceStoreError::from)?;
    Ok(Json(EnvironmentRuntimeAuthorizationResolution {
        valid_until: current.lease.expires_at.min(current.approval_valid_until),
        authorization: current.binding,
        owner_actor_id: current.lease.owner_actor_id,
        lease_authorization: current.lease,
        provider_binding: current.provider_binding,
    }))
}
