//! Only the configured Environment owner may read the original financial grant.
use super::{Extension, Json, Path, ResourceApiError, ResourceApiState, State};
pub(super) async fn record(
    State(state): State<ResourceApiState>,
    Extension(identity): Extension<auth::ServiceIdentity>,
    Path(lease): Path<contracts::LeaseId>,
) -> Result<Json<contracts::resource::environment_funding::EnvironmentCreditGrant>, ResourceApiError>
{
    super::require_environment_service(&state, &identity)?;
    Ok(Json(
        state
            .store
            .environment_credit_grant(&identity.client_id, lease)
            .await?,
    ))
}

pub(super) async fn project_record(
    State(state): State<ResourceApiState>,
    Extension(principal): Extension<super::ResourceCallerPrincipal>,
    Path((project, lease)): Path<(contracts::ProjectId, contracts::LeaseId)>,
) -> Result<Json<contracts::resource::environment_funding::EnvironmentCreditGrant>, ResourceApiError>
{
    super::authorize_project_access(&principal, project)?;
    let lease = state.store.load_lease(lease).await?;
    let request = state.store.load(lease.request_id).await?;
    if request.project_id != project {
        return Err(ResourceApiError::ScopeDenied);
    }
    let owner = state
        .environment_service_client_id
        .as_deref()
        .ok_or(ResourceApiError::ServiceConfiguration)?;
    Ok(Json(
        state
            .store
            .environment_credit_grant(owner, lease.id)
            .await?,
    ))
}

pub(super) async fn restore(
    State(state): State<ResourceApiState>,
    Extension(identity): Extension<auth::ServiceIdentity>,
    headers: super::HeaderMap,
    Json(input): Json<contracts::resource::environment_funding::RestoreEnvironmentFundingRequest>,
) -> Result<Json<contracts::resource::environment_funding::EnvironmentCreditGrant>, ResourceApiError>
{
    super::require_environment_service(&state, &identity)?;
    let key = super::required_header(&headers, "idempotency-key")?;
    Ok(Json(
        state
            .store
            .restore_environment_funding(&key, &identity.client_id, &input)
            .await?,
    ))
}
