//! One production SSE loop for both Control scopes, with bounded current member checks.
use super::{ApiError, now};
use crate::{ControlError, ControlService, SseRecord};
use axum::response::{
    IntoResponse, Response,
    sse::{Event, KeepAlive, Sse},
};
use contracts::{AuthorizationDecision, AuthorizationScope};
use futures_util::stream;

struct EventStream {
    control: ControlService,
    scope: AuthorizationScope,
    cursor: u64,
    pending: std::collections::VecDeque<SseRecord>,
    live: auth::live_authorization::LiveAuthorization,
}

/// Builds the production scoped SSE after the HTTP boundary verifies the actor and scope.
/// Each event and each idle poll use current Control facts. Lookup errors and timeouts end the stream.
pub async fn response(
    control: ControlService,
    decision: AuthorizationDecision,
    session_id: contracts::BffSessionId,
    after: Option<u64>,
    limit: u32,
) -> Result<Response, ApiError> {
    let operation = match decision.scope {
        AuthorizationScope::Course { .. } => "streamCourseEvents",
        AuthorizationScope::Project { .. } => "streamProjectEvents",
        _ => return Err(ApiError::forbidden("LW_AUTH_SCOPE_DENIED")),
    };
    let live = auth::live_authorization::LiveAuthorization::new(
        control.pool.clone(),
        decision.clone(),
        operation,
        session_id,
    );
    if !live.active().await {
        return Err(ApiError::forbidden("LW_AUTH_SCOPE_DENIED"));
    }
    let records = fetch_page(&control, &decision.scope, after, limit).await?;
    let cursor = records
        .last()
        .map_or(after.unwrap_or(0), |record| record.sequence);
    let output = stream::unfold(
        EventStream {
            control,
            scope: decision.scope,
            cursor,
            pending: records.into(),
            live,
        },
        |mut state| async move {
            loop {
                if !state.live.active().await {
                    return None;
                }
                if let Some(record) = state.pending.pop_front() {
                    state.cursor = record.sequence;
                    let event = Event::default()
                        .id(record.sequence.to_string())
                        .event(record.event_type)
                        .data(record.payload.to_string());
                    return Some((Ok::<_, std::convert::Infallible>(event), state));
                }
                tokio::time::sleep(std::time::Duration::from_secs(
                    contracts::object_authorization::AUTHORIZATION_RECHECK_SECONDS,
                ))
                .await;
                match fetch_page(&state.control, &state.scope, Some(state.cursor), 256).await {
                    Ok(records) => state.pending = records.into(),
                    Err(error) => {
                        tracing::warn!(event="control.sse.authorization_or_poll_failed", diagnostic=%error.diagnostic, cursor=state.cursor);
                        return None;
                    }
                }
            }
        },
    );
    Ok(Sse::new(output)
        .keep_alive(KeepAlive::default())
        .into_response())
}

async fn fetch_page(
    control: &ControlService,
    scope: &AuthorizationScope,
    after: Option<u64>,
    limit: u32,
) -> Result<Vec<SseRecord>, ApiError> {
    let timestamp = now()?;
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(
            contracts::object_authorization::AUTHORIZATION_CHECK_TIMEOUT_SECONDS,
        ),
        async {
            match scope {
                AuthorizationScope::Course { course_id } => {
                    control.sse_page(*course_id, after, limit, timestamp).await
                }
                AuthorizationScope::Project { project_id } => {
                    control
                        .project_sse_page(*project_id, after, limit, timestamp)
                        .await
                }
                _ => Err(ControlError::ContractInvalid),
            }
        },
    )
    .await
    .map_err(|_| ApiError::internal("LW_AUTH_MEMBERSHIP_UNAVAILABLE"))?;
    result.map_err(ApiError::from)
}
