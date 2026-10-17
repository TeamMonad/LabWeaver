//! Credits handlers share the existing Access and workload authentication layers.
use super::{
    DiagnosticCode, Extension, HeaderMap, IntoResponse, Json, PageQuery, Path, ProblemDetails,
    ProjectId, Query, ResourceApiState, ResourceCallerPrincipal, Response, Router, State,
    StatusCode, authorize, get, header, post, require_admin, required_header,
};
use crate::credits::{CreditError, PgCreditStore};
use axum::extract::rejection::JsonRejection;
use contracts::resource::credits::{
    CreditAccount, CreditAdminOperation, CreditHold, CreditHoldRequest,
    CreditHoldResolutionRequest, CreditLedgerEntry, CreditLedgerPage,
};
use uuid::Uuid;

pub(super) fn public_routes() -> Router<ResourceApiState> {
    Router::new()
        .route("/api/v1/projects/{project_id}/credits/holds", get(holds))
        .route("/api/v1/projects/{project_id}/credits", get(account))
        .route("/api/v1/projects/{project_id}/credits/ledger", get(ledger))
        .route("/api/v1/resource/credits/operations", post(admin))
}
pub(super) fn internal_routes() -> Router<ResourceApiState> {
    Router::new()
        .route("/internal/v1/credits/holds", post(hold))
        .route(
            "/internal/v1/credits/holds/{hold_id}/resolve",
            post(resolve),
        )
}
async fn account(
    State(state): State<ResourceApiState>,
    Extension(principal): Extension<ResourceCallerPrincipal>,
    Path(project): Path<ProjectId>,
) -> Result<Json<CreditAccount>, CreditApiError> {
    authorize(&principal);
    Ok(Json(
        PgCreditStore::new(state.store.pool())
            .account(project)
            .await?,
    ))
}
async fn ledger(
    State(state): State<ResourceApiState>,
    Extension(principal): Extension<ResourceCallerPrincipal>,
    Path(project): Path<ProjectId>,
    Query(query): Query<PageQuery>,
) -> Result<Json<CreditLedgerPage>, CreditApiError> {
    authorize(&principal);
    let (page, size, offset) = query.normalized().map_err(|_| CreditError::Invalid)?;
    Ok(Json(
        PgCreditStore::new(state.store.pool())
            .ledger(project, page, size, offset)
            .await?,
    ))
}
async fn holds(
    State(state): State<ResourceApiState>,
    Extension(principal): Extension<ResourceCallerPrincipal>,
    Path(project): Path<ProjectId>,
    Query(query): Query<PageQuery>,
) -> Result<Json<contracts::resource::credits::CreditHoldPage>, CreditApiError> {
    authorize(&principal);
    let (page, size, offset) = query.normalized().map_err(|_| CreditError::Invalid)?;
    Ok(Json(
        PgCreditStore::new(state.store.pool())
            .holds(project, page, size, offset)
            .await?,
    ))
}
async fn admin(
    State(state): State<ResourceApiState>,
    Extension(principal): Extension<ResourceCallerPrincipal>,
    headers: HeaderMap,
    input: Result<Json<CreditAdminOperation>, JsonRejection>,
) -> Result<Json<Vec<CreditLedgerEntry>>, CreditApiError> {
    require_admin(&principal).map_err(|_| CreditError::Denied)?;
    let Json(input) = input.map_err(|_| CreditError::Invalid)?;
    let key = required_header(&headers, "idempotency-key").map_err(|_| CreditError::Invalid)?;
    Ok(Json(
        PgCreditStore::new(state.store.pool())
            .admin(&key, principal.actor_id, &principal.roles, &input)
            .await?,
    ))
}
fn owner(state: &ResourceApiState, identity: &auth::ServiceIdentity) -> Result<(), CreditError> {
    if state.environment_service_client_id.as_deref() == Some(identity.client_id.as_str())
        || state
            .task_service_client_ids
            .as_ref()
            .is_some_and(|ids| ids.contains(&identity.client_id))
    {
        Ok(())
    } else {
        Err(CreditError::Denied)
    }
}
async fn hold(
    State(state): State<ResourceApiState>,
    Extension(identity): Extension<auth::ServiceIdentity>,
    headers: HeaderMap,
    input: Result<Json<CreditHoldRequest>, JsonRejection>,
) -> Result<Json<CreditHold>, CreditApiError> {
    owner(&state, &identity)?;
    let Json(input) = input.map_err(|_| CreditError::Invalid)?;
    let request = state
        .store
        .load(input.request_id)
        .await
        .map_err(|_| CreditError::NotFound)?;
    match request.target {
        contracts::resource::ResourceTarget::Environment { .. } => {
            if state.environment_service_client_id.as_deref() != Some(identity.client_id.as_str()) {
                return Err(CreditError::Denied.into());
            }
        }
        contracts::resource::ResourceTarget::Task { .. } => {
            if !state
                .task_service_client_ids
                .as_ref()
                .is_some_and(|ids| ids.contains(&identity.client_id))
            {
                return Err(CreditError::Denied.into());
            }
        }
    }
    let key = required_header(&headers, "idempotency-key").map_err(|_| CreditError::Invalid)?;
    Ok(Json(
        PgCreditStore::new(state.store.pool())
            .hold(&key, &identity.client_id, &input)
            .await?,
    ))
}
async fn resolve(
    State(state): State<ResourceApiState>,
    Extension(identity): Extension<auth::ServiceIdentity>,
    headers: HeaderMap,
    Path(hold_id): Path<Uuid>,
    input: Result<Json<CreditHoldResolutionRequest>, JsonRejection>,
) -> Result<Json<CreditHold>, CreditApiError> {
    owner(&state, &identity)?;
    let Json(input) = input.map_err(|_| CreditError::Invalid)?;
    let key = required_header(&headers, "idempotency-key").map_err(|_| CreditError::Invalid)?;
    Ok(Json(
        PgCreditStore::new(state.store.pool())
            .resolve(&key, &identity.client_id, hold_id, &input)
            .await?,
    ))
}
#[derive(Debug)]
struct CreditApiError(CreditError);
impl From<CreditError> for CreditApiError {
    fn from(error: CreditError) -> Self {
        Self(error)
    }
}
impl IntoResponse for CreditApiError {
    fn into_response(self) -> Response {
        let code = self.0.to_string();
        let status = match &self.0 {
            CreditError::Denied => StatusCode::FORBIDDEN,
            CreditError::Invalid | CreditError::Precision(_) => StatusCode::BAD_REQUEST,
            CreditError::NotFound => StatusCode::NOT_FOUND,
            CreditError::Storage(_) | CreditError::Persistence(_) | CreditError::Wire(_) => {
                StatusCode::SERVICE_UNAVAILABLE
            }
            _ => StatusCode::CONFLICT,
        };
        let problem = ProblemDetails {
            problem_type: format!("urn:labweaver:problem:{code}"),
            title: "Credits 操作失败".into(),
            status: status.as_u16(),
            detail: code.clone(),
            instance: "/api/v1/resource/credits".into(),
            diagnostic_code: DiagnosticCode::parse(code)
                .unwrap_or_else(|_| DiagnosticCode::registered("LW_CREDITS_INVALID")),
            request_id: Uuid::now_v7().to_string(),
            trace_id: None,
            retryable: status == StatusCode::SERVICE_UNAVAILABLE,
            violations: Vec::new(),
        };
        (
            status,
            [(header::CONTENT_TYPE, "application/problem+json")],
            Json(problem),
        )
            .into_response()
    }
}
