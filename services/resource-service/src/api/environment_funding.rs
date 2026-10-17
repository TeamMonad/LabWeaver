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
