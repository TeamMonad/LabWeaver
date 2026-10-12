//! Estimates preserve the existing delegated project and service-owner boundaries.
use super::{
    Extension, HeaderMap, Json, Path, ProjectId, ResourceApiError, ResourceApiState,
    ResourceCallerPrincipal, Router, State, authorize, post, required_header,
};
use axum::extract::rejection::JsonRejection;
use contracts::resource::costing::{
    EstimatedCreditHold, EstimatedCreditHoldRequest, ResourceCostEstimate,
    ResourceCostEstimateRequest,
};
pub(super) fn public_routes() -> Router<ResourceApiState> {
    Router::new().route(
        "/api/v1/projects/{project_id}/resource-cost-estimate",
        post(estimate),
    )
}
pub(super) fn internal_routes() -> Router<ResourceApiState> {
    Router::new().route("/internal/v1/credits/estimated-holds", post(hold))
}
async fn estimate(
    State(state): State<ResourceApiState>,
    Extension(principal): Extension<ResourceCallerPrincipal>,
    Path(project): Path<ProjectId>,
    input: Result<Json<ResourceCostEstimateRequest>, JsonRejection>,
) -> Result<Json<ResourceCostEstimate>, ResourceApiError> {
    authorize(&principal);
    let Json(input) = input.map_err(|_| ResourceApiError::Invalid)?;
    input.validate().map_err(|_| ResourceApiError::Invalid)?;
    Ok(Json(state.store.estimate_cost(project, &input).await?))
}
async fn hold(
    State(state): State<ResourceApiState>,
    Extension(identity): Extension<auth::ServiceIdentity>,
    headers: HeaderMap,
    input: Result<Json<EstimatedCreditHoldRequest>, JsonRejection>,
) -> Result<Json<EstimatedCreditHold>, ResourceApiError> {
    super::credits::owner(&state, &identity).map_err(|_| ResourceApiError::ScopeDenied)?;
    let Json(input) = input.map_err(|_| ResourceApiError::Invalid)?;
    input
        .expected_estimate
        .input
        .validate()
        .map_err(|_| ResourceApiError::Invalid)?;
    let key = required_header(&headers, "idempotency-key")?;
    Ok(Json(
        state
            .store
            .hold_estimated_cost(
                &key,
                &identity.client_id,
                state.environment_service_client_id.as_deref() == Some(identity.client_id.as_str()),
                &input,
            )
            .await?,
    ))
}
