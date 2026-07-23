//! Browser terminal admission, durable session metadata, and mTLS forwarding.

use std::{io::Cursor, sync::Arc, time::Duration};

use auth::ControlGatewayFileConfig;
use axum::{
    extract::{
        Path, State,
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade},
    },
    http::{self, HeaderMap, header},
    response::Response,
};
use futures_util::{SinkExt, StreamExt};
use rustls::{ClientConfig, RootCertStore};
use time::OffsetDateTime;
use tokio_tungstenite::{
    Connector, connect_async_tls_with_config,
    tungstenite::{client::IntoClientRequest, protocol::Message as UpstreamMessage},
};
use uuid::Uuid;

use super::{ApiError, AppState, authenticated_session, proxy, require_browser_origin};

const SUBPROTOCOL: &str = "labweaver.terminal.v1";
const HEARTBEAT_SECONDS: u64 = 5;
const ORPHAN_AFTER_SECONDS: i64 = 15;
const MAX_FRAME_BYTES: usize = 64 * 1024;

#[derive(Clone)]
pub(super) struct TerminalGateway {
    base_uri: url::Url,
    connector: Connector,
}

impl TerminalGateway {
    pub(super) fn new(
        gateway: &ControlGatewayFileConfig,
        ca_pem: &[u8],
        certificate_pem: &[u8],
        key_pem: &[u8],
    ) -> Result<Self, ApiError> {
        let base_uri = url::Url::parse(&gateway.base_uri)
            .map_err(|_| ApiError::internal("LW_ACCESS_TERMINAL_CONFIG_INVALID"))?;
        let mut roots = RootCertStore::empty();
        for certificate in rustls_pemfile::certs(&mut Cursor::new(ca_pem)) {
            roots
                .add(
                    certificate.map_err(|_| {
                        ApiError::internal("LW_ACCESS_TERMINAL_CERTIFICATE_INVALID")
                    })?,
                )
                .map_err(|_| ApiError::internal("LW_ACCESS_TERMINAL_CERTIFICATE_INVALID"))?;
        }
        if roots.is_empty() {
            return Err(ApiError::internal("LW_ACCESS_TERMINAL_CERTIFICATE_INVALID"));
        }
        let certificates = rustls_pemfile::certs(&mut Cursor::new(certificate_pem))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| ApiError::internal("LW_ACCESS_TERMINAL_CERTIFICATE_INVALID"))?;
        let key = rustls_pemfile::private_key(&mut Cursor::new(key_pem))
            .map_err(|_| ApiError::internal("LW_ACCESS_TERMINAL_CERTIFICATE_INVALID"))?
            .ok_or_else(|| ApiError::internal("LW_ACCESS_TERMINAL_CERTIFICATE_INVALID"))?;
        let tls = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_client_auth_cert(certificates, key)
            .map_err(|_| ApiError::internal("LW_ACCESS_TERMINAL_CERTIFICATE_INVALID"))?;
        Ok(Self {
            base_uri,
            connector: Connector::Rustls(Arc::new(tls)),
        })
    }

    fn request(
        &self,
        session_id: Uuid,
        actor_id: Uuid,
        endpoint_grant_id: contracts::EndpointGrantId,
        target: &proxy::RuntimeTarget,
    ) -> Result<http::Request<()>, ApiError> {
        let mut url = self.base_uri.clone();
        url.set_scheme("wss")
            .map_err(|()| ApiError::internal("LW_ACCESS_TERMINAL_CONFIG_INVALID"))?;
        url.set_path(&format!(
            "/internal/v1/environments/{}/terminal",
            target.environment_id
        ));
        let mut request = url
            .as_str()
            .into_client_request()
            .map_err(|_| ApiError::internal("LW_ACCESS_TERMINAL_CONFIG_INVALID"))?;
        let headers = request.headers_mut();
        headers.insert(
            header::SEC_WEBSOCKET_PROTOCOL,
            SUBPROTOCOL
                .parse()
                .map_err(|_| ApiError::internal("LW_ACCESS_TERMINAL_CONFIG_INVALID"))?,
        );
        for (name, value) in [
            ("x-labweaver-terminal-session-id", session_id.to_string()),
            ("x-labweaver-actor-id", actor_id.to_string()),
            (
                "x-labweaver-endpoint-grant-id",
                endpoint_grant_id.to_string(),
            ),
            ("x-labweaver-endpoint-id", target.endpoint_id.to_string()),
            (
                "x-labweaver-access-grant-id",
                target.access_grant_id.to_string(),
            ),
            ("x-labweaver-course-id", target.course_id.to_string()),
            (
                "x-labweaver-environment-revision",
                target.environment_revision.get().to_string(),
            ),
            (
                "x-labweaver-endpoint-revision",
                target.endpoint_revision.get().to_string(),
            ),
        ] {
            headers.insert(
                http::HeaderName::from_bytes(name.as_bytes())
                    .map_err(|_| ApiError::internal("LW_ACCESS_TERMINAL_CONFIG_INVALID"))?,
                value
                    .parse()
                    .map_err(|_| ApiError::internal("LW_ACCESS_TERMINAL_CONFIG_INVALID"))?,
            );
        }
        Ok(request)
    }
}

pub(super) async fn connect_terminal(
    State(state): State<Arc<AppState>>,
    Path(endpoint_grant_id): Path<contracts::EndpointGrantId>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    require_browser_origin(&state, &headers)?;
    let requested_protocol = headers
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.split(',').any(|item| item.trim() == SUBPROTOCOL));
    if !requested_protocol {
        return Err(ApiError::bad_request(
            "LW_ACCESS_TERMINAL_SUBPROTOCOL_REQUIRED",
        ));
    }
    let session = authenticated_session(&state, &headers).await?;
    let target = proxy::authorize_runtime(&state, session.actor_id, endpoint_grant_id).await?;
    if !target
        .capabilities
        .contains(&contracts::environment::EndpointCapability::BrowserTerminal)
    {
        return Err(ApiError::forbidden("LW_ACCESS_TERMINAL_CAPABILITY_DENIED"));
    }
    let terminal_session_id =
        reserve_session(&state, session.actor_id, endpoint_grant_id, &target).await?;
    let terminal_gateway = state.terminal_gateway.clone();
    Ok(upgrade
        .protocols([SUBPROTOCOL])
        .max_frame_size(MAX_FRAME_BYTES)
        .on_upgrade(move |browser| async move {
            if let Err(code) = bridge(
                Arc::clone(&state),
                terminal_gateway,
                terminal_session_id,
                session.actor_id,
                endpoint_grant_id,
                target,
                browser,
            )
            .await
            {
                tracing::warn!(
                    event = "access.browser_terminal.closed",
                    diagnostic = code,
                    terminal_session_id = %terminal_session_id,
                    endpoint_grant_id = %endpoint_grant_id
                );
            }
        }))
}

async fn reserve_session(
    state: &AppState,
    actor_id: Uuid,
    endpoint_grant_id: contracts::EndpointGrantId,
    target: &proxy::RuntimeTarget,
) -> Result<Uuid, ApiError> {
    let now = OffsetDateTime::now_utc();
    let orphaned_before = now - time::Duration::seconds(ORPHAN_AFTER_SECONDS);
    let session_id = Uuid::now_v7();
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('labweaver:browser-terminal-capacity'))")
        .execute(&mut *tx)
        .await
        .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    sqlx::query(
        "UPDATE access.browser_terminal_sessions SET state='failed',closed_at=$1,\
         diagnostic_code='LW_ACCESS_TERMINAL_ORPHANED' WHERE state IN ('opening','active') \
         AND last_heartbeat_at<$2",
    )
    .bind(now)
    .bind(orphaned_before)
    .execute(&mut *tx)
    .await
    .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    let active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM access.browser_terminal_sessions \
         WHERE state IN ('opening','active','terminating') AND expires_at>$1",
    )
    .bind(now)
    .fetch_one(&mut *tx)
    .await
    .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    if active >= i64::from(state.deployment.grants.max_browser_terminal_sessions) {
        return Err(ApiError::unavailable(
            "LW_ACCESS_TERMINAL_CAPACITY_EXHAUSTED",
        ));
    }
    sqlx::query(
        "INSERT INTO access.browser_terminal_sessions \
         (session_id,endpoint_grant_id,access_grant_id,actor_id,course_id,environment_id,\
          environment_revision,endpoint_revision,state,opened_at,last_heartbeat_at,expires_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,'opening',$9,$9,$10)",
    )
    .bind(session_id)
    .bind(endpoint_grant_id.as_uuid())
    .bind(target.access_grant_id.as_uuid())
    .bind(actor_id)
    .bind(target.course_id.as_uuid())
    .bind(target.environment_id.as_uuid())
    .bind(
        i64::try_from(target.environment_revision.get())
            .map_err(|_| ApiError::internal("LW_ACCESS_TERMINAL_IDENTITY_INVALID"))?,
    )
    .bind(
        i64::try_from(target.endpoint_revision.get())
            .map_err(|_| ApiError::internal("LW_ACCESS_TERMINAL_IDENTITY_INVALID"))?,
    )
    .bind(now)
    .bind(target.expires_at)
    .execute(&mut *tx)
    .await
    .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    tx.commit()
        .await
        .map_err(|_| ApiError::unavailable("LW_ACCESS_STORE_UNAVAILABLE"))?;
    Ok(session_id)
}

#[allow(clippy::too_many_arguments)]
async fn bridge(
    state: Arc<AppState>,
    gateway: TerminalGateway,
    terminal_session_id: Uuid,
    actor_id: Uuid,
    endpoint_grant_id: contracts::EndpointGrantId,
    target: proxy::RuntimeTarget,
    browser: WebSocket,
) -> Result<(), &'static str> {
    let result = bridge_connection(
        &state,
        gateway,
        terminal_session_id,
        actor_id,
        endpoint_grant_id,
        target,
        browser,
    )
    .await;
    let (state_name, diagnostic) = match result {
        Ok(()) => ("closed", "LW_ACCESS_TERMINAL_CLOSED"),
        Err(code) => ("failed", code),
    };
    close_session(&state, terminal_session_id, state_name, diagnostic).await?;
    result
}

#[allow(clippy::too_many_arguments)]
async fn bridge_connection(
    state: &AppState,
    gateway: TerminalGateway,
    terminal_session_id: Uuid,
    actor_id: Uuid,
    endpoint_grant_id: contracts::EndpointGrantId,
    target: proxy::RuntimeTarget,
    browser: WebSocket,
) -> Result<(), &'static str> {
    let request = gateway
        .request(terminal_session_id, actor_id, endpoint_grant_id, &target)
        .map_err(|_| "LW_ACCESS_TERMINAL_UPSTREAM_INVALID")?;
    let (upstream, response) =
        connect_async_tls_with_config(request, None, true, Some(gateway.connector.clone()))
            .await
            .map_err(|_| "LW_ACCESS_TERMINAL_UPSTREAM_UNAVAILABLE")?;
    if response
        .headers()
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|value| value.to_str().ok())
        != Some(SUBPROTOCOL)
    {
        return Err("LW_ACCESS_TERMINAL_SUBPROTOCOL_MISMATCH");
    }
    sqlx::query(
        "UPDATE access.browser_terminal_sessions SET state='active',activated_at=$2,\
         last_heartbeat_at=$2 WHERE session_id=$1 AND state='opening'",
    )
    .bind(terminal_session_id)
    .bind(OffsetDateTime::now_utc())
    .execute(&state.pool)
    .await
    .map_err(|_| "LW_ACCESS_STORE_UNAVAILABLE")?;
    let (mut browser_tx, mut browser_rx) = browser.split();
    let (mut upstream_tx, mut upstream_rx) = upstream.split();
    let mut heartbeat = tokio::time::interval(Duration::from_secs(HEARTBEAT_SECONDS));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            message = browser_rx.next() => {
                let Some(message) = message else { break Ok(()); };
                let message = message.map_err(|_| "LW_ACCESS_TERMINAL_BROWSER_FAILED")?;
                let upstream_message = match message {
                    Message::Text(value) => UpstreamMessage::Text(value.to_string().into()),
                    Message::Binary(value) => UpstreamMessage::Binary(value),
                    Message::Ping(value) => UpstreamMessage::Ping(value),
                    Message::Pong(value) => UpstreamMessage::Pong(value),
                    Message::Close(frame) => UpstreamMessage::Close(frame.map(|frame| {
                        tokio_tungstenite::tungstenite::protocol::CloseFrame {
                            code: frame.code.into(),
                            reason: frame.reason.to_string().into(),
                        }
                    })),
                };
                upstream_tx.send(upstream_message).await
                    .map_err(|_| "LW_ACCESS_TERMINAL_UPSTREAM_FAILED")?;
            }
            message = upstream_rx.next() => {
                let Some(message) = message else { break Ok(()); };
                let message = message.map_err(|_| "LW_ACCESS_TERMINAL_UPSTREAM_FAILED")?;
                let browser_message = match message {
                    UpstreamMessage::Text(value) => Message::Text(value.to_string().into()),
                    UpstreamMessage::Binary(value) => Message::Binary(value),
                    UpstreamMessage::Ping(value) => Message::Ping(value),
                    UpstreamMessage::Pong(value) => Message::Pong(value),
                    UpstreamMessage::Close(frame) => Message::Close(frame.map(|frame| CloseFrame {
                        code: frame.code.into(),
                        reason: frame.reason.to_string().into(),
                    })),
                    UpstreamMessage::Frame(_) => continue,
                };
                browser_tx.send(browser_message).await
                    .map_err(|_| "LW_ACCESS_TERMINAL_BROWSER_FAILED")?;
            }
            _ = heartbeat.tick() => {
                if !heartbeat_session(state, terminal_session_id).await? {
                    let _ = browser_tx.send(Message::Close(Some(CloseFrame {
                        code: 1008,
                        reason: "grant expired or revoked".into(),
                    }))).await;
                    break Err("LW_ACCESS_TERMINAL_AUTHORIZATION_ENDED");
                }
            }
        }
    }
}

async fn heartbeat_session(state: &AppState, session_id: Uuid) -> Result<bool, &'static str> {
    let now = OffsetDateTime::now_utc();
    let rows = sqlx::query(
        "UPDATE access.browser_terminal_sessions s SET last_heartbeat_at=$2 \
         FROM access.access_grants g, access.endpoint_grants eg, access.course_memberships cm \
         WHERE s.session_id=$1 AND s.state='active' AND s.access_grant_id=g.grant_id \
         AND s.endpoint_grant_id=eg.endpoint_grant_id \
         AND cm.course_id=s.course_id AND cm.actor_id=s.actor_id \
         AND g.state='active' AND g.actor_id=s.actor_id AND g.course_id=s.course_id \
         AND g.environment_id=s.environment_id AND g.environment_revision=s.environment_revision \
         AND eg.grant_id=g.grant_id AND eg.endpoint_revision=s.endpoint_revision \
         AND eg.health='healthy' AND cm.state='active' \
         AND (cm.expires_at IS NULL OR cm.expires_at>$2) \
         AND g.expires_at>$2 AND eg.expires_at>$2 AND s.expires_at>$2",
    )
    .bind(session_id)
    .bind(now)
    .execute(&state.pool)
    .await
    .map_err(|_| "LW_ACCESS_STORE_UNAVAILABLE")?
    .rows_affected();
    Ok(rows == 1)
}

async fn close_session(
    state: &AppState,
    session_id: Uuid,
    terminal_state: &str,
    diagnostic: &str,
) -> Result<(), &'static str> {
    sqlx::query(
        "UPDATE access.browser_terminal_sessions SET state=$2,closed_at=$3,\
         diagnostic_code=$4 WHERE session_id=$1 AND state IN ('opening','active','terminating')",
    )
    .bind(session_id)
    .bind(terminal_state)
    .bind(OffsetDateTime::now_utc())
    .bind(diagnostic)
    .execute(&state.pool)
    .await
    .map_err(|_| "LW_ACCESS_STORE_UNAVAILABLE")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::SUBPROTOCOL;

    #[test]
    fn terminal_subprotocol_is_versioned() {
        assert_eq!(SUBPROTOCOL, "labweaver.terminal.v1");
    }
}
