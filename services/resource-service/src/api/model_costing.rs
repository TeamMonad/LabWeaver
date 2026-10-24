use super::{Extension, Json, Path, ResourceApiError, ResourceApiState, Router, State, post};
use axum::http::HeaderMap;
use contracts::resource::model_costing::{
    ModelCostAdmission, ModelCostAdmissionRequest, ModelCostSettlementRequest,
};
use uuid::Uuid;

const ADMIT_PATH: &str = "/internal/v1/credits/model/admit";
const SETTLE_PATH: &str = "/internal/v1/credits/model/{hold_id}/settle";

pub(super) fn internal_routes() -> Router<ResourceApiState> {
    Router::new()
        .route(ADMIT_PATH, post(admit))
        .route(SETTLE_PATH, post(settle))
}

async fn admit(
    State(state): State<ResourceApiState>,
    Extension(identity): Extension<auth::ServiceIdentity>,
    headers: HeaderMap,
    Json(input): Json<ModelCostAdmissionRequest>,
) -> Result<Json<ModelCostAdmission>, ResourceApiError> {
    super::build_costing::owner(&state, &identity)?;
    let key = super::required_header(&headers, "idempotency-key")?;
    Ok(Json(
        state
            .store
            .admit_model_cost(&key, &identity.client_id, &input)
            .await?,
    ))
}

async fn settle(
    State(state): State<ResourceApiState>,
    Extension(identity): Extension<auth::ServiceIdentity>,
    Path(hold_id): Path<Uuid>,
    Json(input): Json<ModelCostSettlementRequest>,
) -> Result<Json<contracts::resource::credits::CreditHold>, ResourceApiError> {
    super::build_costing::owner(&state, &identity)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn model_routes_are_registered_with_the_same_auth_and_wire_contracts()
    -> Result<(), Box<dyn std::error::Error>> {
        let admit =
            contracts::http::operation_contract("admitModelCost").ok_or("admit registration")?;
        let settle =
            contracts::http::operation_contract("settleModelCost").ok_or("settle registration")?;
        assert_eq!(admit.path, ADMIT_PATH);
        assert_eq!(settle.path.replace("{holdId}", "{hold_id}"), SETTLE_PATH);
        assert_eq!(
            super::super::internal_route_permission(ADMIT_PATH),
            Some(admit.permission)
        );
        assert_eq!(
            super::super::internal_route_permission("/internal/v1/credits/model/example/settle"),
            Some(settle.permission)
        );
        assert_eq!(
            admit.mutation,
            contracts::http::MutationContract::IdempotentCreate
        );
        assert_eq!(admit.security, contracts::http::Security::ServiceJwt);
        assert_eq!(settle.security, contracts::http::Security::ServiceJwt);
        Ok(())
    }
}
