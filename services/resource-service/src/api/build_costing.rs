//! Build funding uses the configured Agent service identity, never browser delegation.
use super::{
    Extension, Json, Path, ProjectId, ResourceApiError, ResourceApiState, ResourceCallerPrincipal,
    Router, State, authorize, get, post,
};
use contracts::BuildRequestId;
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BuildLeaseQuery {
    lease_token: uuid::Uuid,
}
use contracts::resource::build_costing::{
    BuildCostAdmissionRequest, BuildCostEstimate, BuildCostEstimateRequest, BuildCostRecord,
    ObserveBuildCostRequest,
};

pub(super) fn public_routes() -> Router<ResourceApiState> {
    Router::new()
        .route(
            "/api/v1/projects/{project_id}/build-cost-estimate",
            post(estimate),
        )
        .route(
            "/api/v1/projects/{project_id}/build-costs/{build_request_id}/attempts/{attempt}",
            get(project_record),
        )
}
pub(super) fn internal_routes() -> Router<ResourceApiState> {
    Router::new()
        .route("/internal/v1/build-cost-admissions", post(admit))
        .route(
            "/internal/v1/build-costs/{build_request_id}/attempts/{attempt}",
            get(record),
        )
        .route("/internal/v1/build-cost-observations", post(observe))
        .route(
            "/internal/v1/build-cost-reconciliations",
            get(reconciliations),
        )
}
fn owner(
    state: &ResourceApiState,
    identity: &auth::ServiceIdentity,
) -> Result<(), ResourceApiError> {
    if state.build_service_client_id.as_deref() != Some(identity.client_id.as_str()) {
        return Err(ResourceApiError::ScopeDenied);
    }
    Ok(())
}
async fn estimate(
    State(state): State<ResourceApiState>,
    Extension(principal): Extension<ResourceCallerPrincipal>,
    Path(project): Path<ProjectId>,
    Json(input): Json<BuildCostEstimateRequest>,
) -> Result<Json<BuildCostEstimate>, ResourceApiError> {
    authorize(&principal);
    Ok(Json(
        state.store.estimate_build_cost(project, &input).await?,
    ))
}
async fn project_record(
    State(state): State<ResourceApiState>,
    Extension(principal): Extension<ResourceCallerPrincipal>,
    Path((project, build, attempt)): Path<(ProjectId, BuildRequestId, u32)>,
) -> Result<Json<BuildCostRecord>, ResourceApiError> {
    authorize(&principal);
    Ok(Json(
        state
            .store
            .get_project_build_cost(project, build, attempt)
            .await?,
    ))
}
async fn admit(
    State(state): State<ResourceApiState>,
    Extension(identity): Extension<auth::ServiceIdentity>,
    Json(input): Json<BuildCostAdmissionRequest>,
) -> Result<Json<BuildCostRecord>, ResourceApiError> {
    owner(&state, &identity)?;
    Ok(Json(
        state
            .store
            .admit_build_cost(&identity.client_id, &input)
            .await?,
    ))
}
async fn record(
    State(state): State<ResourceApiState>,
    Extension(identity): Extension<auth::ServiceIdentity>,
    Path((build, attempt)): Path<(BuildRequestId, u32)>,
    axum::extract::Query(query): axum::extract::Query<BuildLeaseQuery>,
) -> Result<Json<BuildCostRecord>, ResourceApiError> {
    owner(&state, &identity)?;
    Ok(Json(
        state
            .store
            .get_build_cost_with_fence(&identity.client_id, build, attempt, query.lease_token)
            .await?,
    ))
}
async fn observe(
    State(state): State<ResourceApiState>,
    Extension(identity): Extension<auth::ServiceIdentity>,
    Json(input): Json<ObserveBuildCostRequest>,
) -> Result<Json<BuildCostRecord>, ResourceApiError> {
    owner(&state, &identity)?;
    Ok(Json(
        state
            .store
            .observe_build_cost(&identity.client_id, &input)
            .await?,
    ))
}

async fn reconciliations(
    State(state): State<ResourceApiState>,
    Extension(identity): Extension<auth::ServiceIdentity>,
) -> Result<Json<Vec<contracts::resource::build_costing::BuildCostReconciliation>>, ResourceApiError>
{
    owner(&state, &identity)?;
    Ok(Json(
        state
            .store
            .claim_build_cost_reconciliations(&identity.client_id)
            .await?,
    ))
}
