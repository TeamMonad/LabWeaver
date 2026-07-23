//! Environment-authoritative terminal validation and mTLS executor forwarding.

use std::{io::Cursor, sync::Arc};

use axum::{
    Router,
    extract::{
        Extension, Path, State,
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use contracts::{
    ActorId, CourseId, EndpointGrantId, EndpointId, EnvironmentId, Revision,
    authoring::{EnvironmentRuntimeSpec, RuntimeKind},
    environment::{
        DesiredEnvironmentState, EndpointCapability, EndpointHealth, ObservedEnvironmentState,
    },
};
use futures_util::{SinkExt, StreamExt};
use rustls::{ClientConfig, RootCertStore};
use serde::{Deserialize, Serialize};
use tokio_tungstenite::{
    Connector, connect_async_tls_with_config,
    tungstenite::{client::IntoClientRequest, protocol::Message as UpstreamMessage},
};

use crate::{
    ContainerReleaseResolver, EnvironmentApiState, EnvironmentStoreError, VerifiedCallerIdentity,
};

const ACCESS_SERVICE_SAN: &str = "spiffe://labweaver/access-service";
const SUBPROTOCOL: &str = "labweaver.terminal.v1";
const MAX_FRAME_BYTES: usize = 64 * 1024;

#[derive(Clone)]
pub(crate) struct TerminalExecutorGateway {
    uri: url::Url,
    connector: Connector,
}

#[derive(Clone)]
struct TerminalBridgeState {
    authority: EnvironmentApiState,
    gateway: TerminalExecutorGateway,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum BrowserOpen {
    Open { cols: u16, rows: u16 },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExecutorRequest {
    environment_id: EnvironmentId,
    course_id: CourseId,
    terminal: contracts::authoring::TerminalSpec,
    cols: u16,
    rows: u16,
}

struct TerminalIdentity {
    course_id: CourseId,
    environment_revision: Revision,
    endpoint_id: EndpointId,
    endpoint_revision: Revision,
}

impl TerminalExecutorGateway {
    pub(crate) fn from_env() -> Result<Self, TerminalBridgeError> {
        let uri = required_url("LABWEAVER_CONTAINER_TERMINAL_EXECUTOR_URI")?;
        if uri.scheme() != "wss"
            || uri.path() != "/internal/v1/container-terminal"
            || uri.query().is_some()
            || uri.fragment().is_some()
        {
            return Err(TerminalBridgeError::Configuration);
        }
        let ca = std::fs::read(required("LABWEAVER_CONTAINER_TERMINAL_EXECUTOR_CA_PATH")?)?;
        let certificate =
            std::fs::read(required("LABWEAVER_CONTAINER_TERMINAL_EXECUTOR_CERT_PATH")?)?;
        let key = std::fs::read(required("LABWEAVER_CONTAINER_TERMINAL_EXECUTOR_KEY_PATH")?)?;
        let mut roots = RootCertStore::empty();
        for certificate in rustls_pemfile::certs(&mut Cursor::new(ca)) {
            roots
                .add(certificate.map_err(|_| TerminalBridgeError::Certificate)?)
                .map_err(|_| TerminalBridgeError::Certificate)?;
        }
        if roots.is_empty() {
            return Err(TerminalBridgeError::Certificate);
        }
        let certificates = rustls_pemfile::certs(&mut Cursor::new(certificate))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| TerminalBridgeError::Certificate)?;
        let key = rustls_pemfile::private_key(&mut Cursor::new(key))
            .map_err(|_| TerminalBridgeError::Certificate)?
            .ok_or(TerminalBridgeError::Certificate)?;
        let tls = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_client_auth_cert(certificates, key)
            .map_err(|_| TerminalBridgeError::Certificate)?;
        Ok(Self {
            uri,
            connector: Connector::Rustls(Arc::new(tls)),
        })
    }
}

pub(crate) fn terminal_bridge_router(
    authority: EnvironmentApiState,
    gateway: TerminalExecutorGateway,
) -> Router {
    Router::new()
        .route(
            "/internal/v1/environments/{environment_id}/terminal",
            get(upgrade_terminal),
        )
        .with_state(TerminalBridgeState { authority, gateway })
}

async fn upgrade_terminal(
    State(state): State<TerminalBridgeState>,
    Extension(caller): Extension<VerifiedCallerIdentity>,
    Path(environment_id): Path<EnvironmentId>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Result<Response, TerminalBridgeError> {
    if !caller.contains_san(ACCESS_SERVICE_SAN) {
        return Err(TerminalBridgeError::CallerDenied);
    }
    if !headers
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.split(',').any(|part| part.trim() == SUBPROTOCOL))
    {
        return Err(TerminalBridgeError::SubprotocolRequired);
    }
    let identity = terminal_identity(&headers)?;
    let (instance, now) = state
        .authority
        .store
        .load_for_owner_resolution(environment_id)
        .await
        .map_err(|error| match error {
            EnvironmentStoreError::EnvironmentNotFound => TerminalBridgeError::ScopeDenied,
            _ => TerminalBridgeError::AuthorityUnavailable,
        })?;
    if instance.id != environment_id
        || instance.course_id != identity.course_id
        || instance.revision != identity.environment_revision
        || instance.runtime_kind != RuntimeKind::Container
        || instance.desired_state != DesiredEnvironmentState::Running
        || instance.observed_state != ObservedEnvironmentState::Ready
        || instance.eligibility_expires_at <= now
        || !instance.endpoints.iter().any(|endpoint| {
            endpoint.id == identity.endpoint_id
                && endpoint.revision == identity.endpoint_revision
                && endpoint.health == EndpointHealth::Healthy
                && endpoint
                    .capabilities
                    .contains(&EndpointCapability::BrowserTerminal)
        })
    {
        return Err(TerminalBridgeError::ScopeDenied);
    }
    let release = state
        .authority
        .releases
        .resolve(instance.release_id, instance.release_version)
        .await
        .map_err(|_| TerminalBridgeError::AuthorityUnavailable)?;
    if release.withdrawn_at.is_some()
        || release.projection.release.course_id != instance.course_id
        || release.projection.release.runtime_kind != RuntimeKind::Container
    {
        return Err(TerminalBridgeError::ScopeDenied);
    }
    let EnvironmentRuntimeSpec::Container {
        terminal: Some(terminal),
        ..
    } = &release.projection.environment_spec.runtime
    else {
        return Err(TerminalBridgeError::CapabilityDenied);
    };
    terminal
        .validate()
        .map_err(|_| TerminalBridgeError::ContractInvalid)?;
    let terminal = terminal.clone();
    Ok(upgrade
        .protocols([SUBPROTOCOL])
        .max_frame_size(MAX_FRAME_BYTES)
        .on_upgrade(move |browser| async move {
            if let Err(code) =
                bridge(state.gateway, environment_id, identity, terminal, browser).await
            {
                tracing::warn!(
                    event = "environment.browser_terminal.closed",
                    diagnostic = code,
                    environment_id = %environment_id
                );
            }
        }))
}

async fn bridge(
    gateway: TerminalExecutorGateway,
    environment_id: EnvironmentId,
    identity: TerminalIdentity,
    terminal: contracts::authoring::TerminalSpec,
    mut browser: WebSocket,
) -> Result<(), &'static str> {
    let (cols, rows) = match browser.next().await {
        Some(Ok(Message::Text(value))) => {
            match serde_json::from_str::<BrowserOpen>(&value)
                .map_err(|_| "LW_ENV_TERMINAL_OPEN_INVALID")?
            {
                BrowserOpen::Open { cols, rows } => (cols, rows),
            }
        }
        _ => return Err("LW_ENV_TERMINAL_OPEN_REQUIRED"),
    };
    valid_size(cols, rows)?;
    let mut request = gateway
        .uri
        .as_str()
        .into_client_request()
        .map_err(|_| "LW_ENV_TERMINAL_EXECUTOR_CONFIG_INVALID")?;
    request.headers_mut().insert(
        header::SEC_WEBSOCKET_PROTOCOL,
        SUBPROTOCOL
            .parse()
            .map_err(|_| "LW_ENV_TERMINAL_EXECUTOR_CONFIG_INVALID")?,
    );
    let (upstream, response) =
        connect_async_tls_with_config(request, None, true, Some(gateway.connector))
            .await
            .map_err(|_| "LW_ENV_TERMINAL_EXECUTOR_UNAVAILABLE")?;
    if response
        .headers()
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|value| value.to_str().ok())
        != Some(SUBPROTOCOL)
    {
        return Err("LW_ENV_TERMINAL_EXECUTOR_PROTOCOL_INVALID");
    }
    let (mut upstream_tx, mut upstream_rx) = upstream.split();
    upstream_tx
        .send(UpstreamMessage::Text(
            serde_json::to_string(&ExecutorRequest {
                environment_id,
                course_id: identity.course_id,
                terminal,
                cols,
                rows,
            })
            .map_err(|_| "LW_ENV_TERMINAL_EXECUTOR_REQUEST_INVALID")?
            .into(),
        ))
        .await
        .map_err(|_| "LW_ENV_TERMINAL_EXECUTOR_UNAVAILABLE")?;
    let (mut browser_tx, mut browser_rx) = browser.split();
    loop {
        tokio::select! {
            message = browser_rx.next() => {
                let Some(message) = message else { break; };
                let message = message.map_err(|_| "LW_ENV_TERMINAL_BROWSER_FAILED")?;
                let upstream = match message {
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
                upstream_tx.send(upstream).await
                    .map_err(|_| "LW_ENV_TERMINAL_EXECUTOR_FAILED")?;
            }
            message = upstream_rx.next() => {
                let Some(message) = message else { break; };
                let message = message.map_err(|_| "LW_ENV_TERMINAL_EXECUTOR_FAILED")?;
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
                    .map_err(|_| "LW_ENV_TERMINAL_BROWSER_FAILED")?;
            }
        }
    }
    Ok(())
}

fn terminal_identity(headers: &HeaderMap) -> Result<TerminalIdentity, TerminalBridgeError> {
    let _: ActorId = header_value(headers, "x-labweaver-actor-id")?;
    let _: EndpointGrantId = header_value(headers, "x-labweaver-endpoint-grant-id")?;
    Ok(TerminalIdentity {
        course_id: header_value(headers, "x-labweaver-course-id")?,
        environment_revision: revision_header(headers, "x-labweaver-environment-revision")?,
        endpoint_id: header_value(headers, "x-labweaver-endpoint-id")?,
        endpoint_revision: revision_header(headers, "x-labweaver-endpoint-revision")?,
    })
}

fn revision_header(
    headers: &HeaderMap,
    name: &'static str,
) -> Result<Revision, TerminalBridgeError> {
    let value = header_value::<u64>(headers, name)?;
    Revision::new(value).map_err(|_| TerminalBridgeError::IdentityInvalid)
}

fn header_value<T: std::str::FromStr>(
    headers: &HeaderMap,
    name: &'static str,
) -> Result<T, TerminalBridgeError> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
        .ok_or(TerminalBridgeError::IdentityInvalid)
}

fn valid_size(cols: u16, rows: u16) -> Result<(), &'static str> {
    if cols == 0 || rows == 0 || cols > 500 || rows > 200 {
        return Err("LW_ENV_TERMINAL_SIZE_INVALID");
    }
    Ok(())
}

fn required(name: &'static str) -> Result<String, TerminalBridgeError> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or(TerminalBridgeError::Configuration)
}

fn required_url(name: &'static str) -> Result<url::Url, TerminalBridgeError> {
    url::Url::parse(&required(name)?).map_err(|_| TerminalBridgeError::Configuration)
}

#[derive(Debug, thiserror::Error)]
pub enum TerminalBridgeError {
    #[error("LW_ENV_TERMINAL_CONFIGURATION_INVALID")]
    Configuration,
    #[error("LW_ENV_TERMINAL_CERTIFICATE_INVALID")]
    Certificate,
    #[error("LW_ENV_TERMINAL_CALLER_DENIED")]
    CallerDenied,
    #[error("LW_ENV_TERMINAL_SUBPROTOCOL_REQUIRED")]
    SubprotocolRequired,
    #[error("LW_ENV_TERMINAL_IDENTITY_INVALID")]
    IdentityInvalid,
    #[error("LW_ENV_TERMINAL_SCOPE_DENIED")]
    ScopeDenied,
    #[error("LW_ENV_TERMINAL_CAPABILITY_DENIED")]
    CapabilityDenied,
    #[error("LW_ENV_TERMINAL_CONTRACT_INVALID")]
    ContractInvalid,
    #[error("LW_ENV_TERMINAL_AUTHORITY_UNAVAILABLE")]
    AuthorityUnavailable,
    #[error("LW_ENV_TERMINAL_IO_FAILED")]
    Io(#[from] std::io::Error),
}

impl IntoResponse for TerminalBridgeError {
    fn into_response(self) -> Response {
        let status = match self {
            Self::CallerDenied | Self::ScopeDenied | Self::CapabilityDenied => {
                StatusCode::FORBIDDEN
            }
            Self::SubprotocolRequired | Self::IdentityInvalid | Self::ContractInvalid => {
                StatusCode::BAD_REQUEST
            }
            _ => StatusCode::SERVICE_UNAVAILABLE,
        };
        (status, self.to_string()).into_response()
    }
}
