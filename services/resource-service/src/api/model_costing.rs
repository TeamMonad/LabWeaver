use super::{Extension, Json, Path, ResourceApiError, ResourceApiState, Router, State, post};
use contracts::resource::model_costing::{
    ModelCostAdmission, ModelCostAdmissionRequest, ModelCostSettlementRequest,
};
use uuid::Uuid;

pub(super) fn internal_routes() -> Router<ResourceApiState> {
    Router::new()
        .route("/internal/v1/credits/model/admit", post(admit))
        .route("/internal/v1/credits/model/{hold_id}/settle", post(settle))
}

async fn admit(
    State(state): State<ResourceApiState>,
    Extension(identity): Extension<auth::ServiceIdentity>,
    Json(input): Json<ModelCostAdmissionRequest>,
) -> Result<Json<ModelCostAdmission>, ResourceApiError> {
    super::credits::owner(&state, &identity).map_err(|e| ResourceApiError::Store(e.into()))?;
    Ok(Json(
        state
            .store
            .admit_model_cost(&input.call_id.to_string(), &identity.client_id, &input)
            .await?,
    ))
}

async fn settle(
    State(state): State<ResourceApiState>,
    Extension(identity): Extension<auth::ServiceIdentity>,
    Path(hold_id): Path<Uuid>,
    Json(input): Json<ModelCostSettlementRequest>,
) -> Result<Json<contracts::resource::credits::CreditHold>, ResourceApiError> {
    super::credits::owner(&state, &identity).map_err(|e| ResourceApiError::Store(e.into()))?;
    if input.hold_id != hold_id {
        return Err(ResourceApiError::Invalid);
    }
    Ok(Json(
        state
            .store
            .settle_model_cost(
                "model-settlement",
                &identity.client_id,
                hold_id,
                input.call_id,
                input.event_id,
                &input.model_binding,
                &input.measurement,
            )
            .await?,
    ))
}
