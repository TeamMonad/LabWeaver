use super::{ApiError, ApiState, GatewayPrincipal, authorize_global};
use crate::notifications::{self, NotificationError};
use axum::{
    Extension, Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, put},
};
use contracts::{EventId, notifications::NotificationListQuery};
use std::sync::Arc;

pub(super) fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        .route("/api/v1/notifications", get(list))
        .route("/api/v1/notifications/unread-count", get(unread))
        .route(
            "/api/v1/notifications/{notification_id}/read",
            put(mark_read),
        )
}
async fn list(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    headers: HeaderMap,
    Query(query): Query<NotificationListQuery>,
) -> Result<Response, ApiError> {
    let actor = authorize_global(&state, &principal, &headers, "listNotifications")
        .await?
        .actor
        .actor_id;
    Ok(Json(notifications::list(&state.control.pool, actor, &query).await?).into_response())
}
async fn unread(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let actor = authorize_global(&state, &principal, &headers, "getNotificationUnreadCount")
        .await?
        .actor
        .actor_id;
    Ok(Json(notifications::unread(&state.control.pool, actor).await?).into_response())
}
async fn mark_read(
    State(state): State<Arc<ApiState>>,
    Extension(principal): Extension<GatewayPrincipal>,
    headers: HeaderMap,
    Path(id): Path<EventId>,
) -> Result<Response, ApiError> {
    let actor = authorize_global(&state, &principal, &headers, "markNotificationRead")
        .await?
        .actor
        .actor_id;
    Ok(Json(notifications::mark_read(&state.control.pool, actor, id).await?).into_response())
}
impl From<NotificationError> for ApiError {
    fn from(error: NotificationError) -> Self {
        let status = match error {
            NotificationError::NotFound => StatusCode::NOT_FOUND,
            NotificationError::Invalid => StatusCode::BAD_REQUEST,
            NotificationError::EventConflict => StatusCode::CONFLICT,
            NotificationError::Persistence(_) => StatusCode::SERVICE_UNAVAILABLE,
        };
        Self::new(
            status,
            &error.to_string(),
            status == StatusCode::SERVICE_UNAVAILABLE,
        )
    }
}
