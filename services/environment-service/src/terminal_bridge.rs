//! Environment-authoritative console validation and mTLS executor forwarding.

use crate::{
    ContainerReleaseResolver, EnvironmentApiState, EnvironmentStoreError, VerifiedCallerIdentity,
};
use axum::{
    Router,
    extract::{
        Extension, Path, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use contracts::{
    AccessGrantId, ConsoleSessionId, EnvironmentId, LeaseId, ReleaseId, Revision,
    access::{ConsoleClientControl, ConsoleKind},
    authoring::{EnvironmentClass, EnvironmentRuntimeSpec, RuntimeKind},
    environment::{DesiredEnvironmentState, ObservedEnvironmentState},
};
use futures_util::{SinkExt, StreamExt};
use rustls::{ClientConfig, RootCertStore};
use serde::Serialize;
use std::{io::Cursor, sync::Arc};
use tokio_tungstenite::{
    Connector, connect_async_tls_with_config,
    tungstenite::{client::IntoClientRequest, protocol::Message as UpstreamMessage},
};

const ACCESS_SERVICE_SAN: &str = "spiffe://labweaver/access-service";
const MAX_FRAME_BYTES: usize = 64 * 1024;

#[derive(Clone)]
pub struct TerminalExecutorGateway {
    uri: url::Url,
    connector: Connector,
}
#[derive(Clone)]
struct BridgeState {
    authority: EnvironmentApiState,
    gateway: TerminalExecutorGateway,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ExecutorRequest {
    environment_id: EnvironmentId,
    course_id: contracts::CourseId,
    release_id: ReleaseId,
    release_version: u64,
    terminal: contracts::authoring::TerminalSpec,
    cols: u16,
    rows: u16,
}
struct Identity {
    session_id: ConsoleSessionId,
    grant_id: AccessGrantId,
    grant_revision: Revision,
    environment_revision: Revision,
    lease: Option<(LeaseId, Revision)>,
}

impl TerminalExecutorGateway {
    pub fn from_env() -> Result<Self, TerminalBridgeError> {
        let uri = url::Url::parse(&required("LABWEAVER_CONTAINER_TERMINAL_EXECUTOR_URI")?)
            .map_err(|_| TerminalBridgeError::Configuration)?;
        if uri.scheme() != "wss"
            || uri.path() != "/internal/v1/container-terminal"
            || uri.query().is_some()
            || uri.fragment().is_some()
        {
            return Err(TerminalBridgeError::Configuration);
        }
        let ca = std::fs::read(required("LABWEAVER_CONTAINER_TERMINAL_EXECUTOR_CA_PATH")?)?;
        let cert = std::fs::read(required("LABWEAVER_CONTAINER_TERMINAL_EXECUTOR_CERT_PATH")?)?;
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
        let certificates = rustls_pemfile::certs(&mut Cursor::new(cert))
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

pub fn terminal_bridge_router(
    authority: EnvironmentApiState,
    gateway: TerminalExecutorGateway,
) -> Router {
    Router::new()
        .route(
            "/internal/v1/environments/{environment_id}/console:connect",
            get(upgrade),
        )
        .with_state(BridgeState { authority, gateway })
}

async fn upgrade(
    State(state): State<BridgeState>,
    caller: Option<Extension<VerifiedCallerIdentity>>,
    Path(environment_id): Path<EnvironmentId>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Result<Response, TerminalBridgeError> {
    if !caller.is_some_and(|Extension(identity)| identity.contains_san(ACCESS_SERVICE_SAN)) {
        return Err(TerminalBridgeError::CallerDenied);
    }
    let protocol = ConsoleKind::Xterm.websocket_subprotocol();
    if !headers
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|p| p.trim() == protocol))
    {
        return Err(TerminalBridgeError::SubprotocolRequired);
    }
    let identity = identity(&headers)?;
    let (instance, now) = state
        .authority
        .store
        .load_for_owner_resolution(environment_id)
        .await
        .map_err(|error| match error {
            EnvironmentStoreError::EnvironmentNotFound => TerminalBridgeError::ScopeDenied,
            _ => TerminalBridgeError::AuthorityUnavailable,
        })?;
    if instance.revision != identity.environment_revision
        || instance.runtime_kind != RuntimeKind::Container
        || instance.desired_state != DesiredEnvironmentState::Running
        || instance.observed_state != ObservedEnvironmentState::Ready
        || instance.eligibility_expires_at <= now
    {
        return Err(TerminalBridgeError::ScopeDenied);
    }
    match (
        instance.class,
        &instance.operation.lease_authorization,
        identity.lease,
    ) {
        (EnvironmentClass::Experiment, None, None) => {}
        (EnvironmentClass::Work, Some(auth), Some((id, revision)))
            if auth.lease_id == id && auth.lease_revision == revision && auth.expires_at > now => {}
        _ => return Err(TerminalBridgeError::ScopeDenied),
    }
    let release = state
        .authority
        .releases
        .resolve(instance.release_id, instance.release_version)
        .await
        .map_err(|_| TerminalBridgeError::AuthorityUnavailable)?;
    if release.withdrawn_at.is_some() || release.projection.release.course_id != instance.course_id
    {
        return Err(TerminalBridgeError::ScopeDenied);
    }
    let EnvironmentRuntimeSpec::Container {
        terminal: Some(terminal),
        ..
    } = release.projection.environment_spec.runtime
    else {
        return Err(TerminalBridgeError::CapabilityDenied);
    };
    terminal
        .validate()
        .map_err(|_| TerminalBridgeError::ContractInvalid)?;
    let terminal = *terminal;
    Ok(upgrade.protocols([protocol]).max_frame_size(MAX_FRAME_BYTES).on_upgrade(move |browser| async move {
        if let Err(code) = bridge(state.gateway, environment_id, instance.course_id, instance.release_id, instance.release_version, terminal, browser).await { tracing::warn!(event="environment.console.closed", diagnostic=code, console_session_id=%identity.session_id, access_grant_id=%identity.grant_id, access_grant_revision=identity.grant_revision.get(), environment_id=%environment_id); }
    }))
}

async fn bridge(
    gateway: TerminalExecutorGateway,
    environment_id: EnvironmentId,
    course_id: contracts::CourseId,
    release_id: ReleaseId,
    release_version: u64,
    terminal: contracts::authoring::TerminalSpec,
    browser: WebSocket,
) -> Result<(), &'static str> {
    let mut request = gateway
        .uri
        .as_str()
        .into_client_request()
        .map_err(|_| "LW_ENV_CONSOLE_EXECUTOR_CONFIG_INVALID")?;
    request.headers_mut().insert(
        header::SEC_WEBSOCKET_PROTOCOL,
        ConsoleKind::Xterm
            .websocket_subprotocol()
            .parse()
            .map_err(|_| "LW_ENV_CONSOLE_EXECUTOR_CONFIG_INVALID")?,
    );
    let (upstream, response) =
        connect_async_tls_with_config(request, None, true, Some(gateway.connector))
            .await
            .map_err(|_| "LW_ENV_CONSOLE_EXECUTOR_UNAVAILABLE")?;
    if response
        .headers()
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|v| v.to_str().ok())
        != Some(ConsoleKind::Xterm.websocket_subprotocol())
    {
        return Err("LW_ENV_CONSOLE_EXECUTOR_PROTOCOL_INVALID");
    }
    let (mut upstream_tx, mut upstream_rx) = upstream.split();
    upstream_tx
        .send(UpstreamMessage::Text(
            serde_json::to_string(&ExecutorRequest {
                environment_id,
                course_id,
                release_id,
                release_version,
                terminal,
                cols: 80,
                rows: 24,
            })
            .map_err(|_| "LW_ENV_CONSOLE_EXECUTOR_REQUEST_INVALID")?
            .into(),
        ))
        .await
        .map_err(|_| "LW_ENV_CONSOLE_EXECUTOR_UNAVAILABLE")?;
    let (mut browser_tx, mut browser_rx) = browser.split();
    loop {
        tokio::select! {
            message=browser_rx.next()=>{let Some(message)=message else{return Ok(())}; let message=message.map_err(|_|"LW_ENV_CONSOLE_BROWSER_FAILED")?; let forwarded=match message { Message::Binary(v) if v.len()<=MAX_FRAME_BYTES=>UpstreamMessage::Binary(v), Message::Text(v) if v.len()<=1024=>{let control:ConsoleClientControl=contracts::parse_strict_json(v.as_bytes()).map_err(|_|"LW_ENV_CONSOLE_CONTROL_INVALID")?;control.validate().map_err(|_|"LW_ENV_CONSOLE_CONTROL_INVALID")?;UpstreamMessage::Text(v.to_string().into())}, Message::Ping(v)=>UpstreamMessage::Ping(v),Message::Pong(v)=>UpstreamMessage::Pong(v),Message::Close(_)=>return Ok(()),_=>return Err("LW_ENV_CONSOLE_FRAME_INVALID")};upstream_tx.send(forwarded).await.map_err(|_|"LW_ENV_CONSOLE_EXECUTOR_FAILED")?;}
            message=upstream_rx.next()=>{let Some(message)=message else{return Ok(())};let message=message.map_err(|_|"LW_ENV_CONSOLE_EXECUTOR_FAILED")?;let forwarded=match message{UpstreamMessage::Binary(v) if v.len()<=MAX_FRAME_BYTES=>Message::Binary(v),UpstreamMessage::Ping(v)=>Message::Ping(v),UpstreamMessage::Pong(v)=>Message::Pong(v),UpstreamMessage::Close(_)=>return Ok(()),_=>return Err("LW_ENV_CONSOLE_EXECUTOR_FRAME_INVALID")};browser_tx.send(forwarded).await.map_err(|_|"LW_ENV_CONSOLE_BROWSER_FAILED")?;}
        }
    }
}

fn identity(headers: &HeaderMap) -> Result<Identity, TerminalBridgeError> {
    let lease_id = headers
        .get("x-labweaver-lease-id")
        .and_then(|v| v.to_str().ok());
    let lease_revision = headers
        .get("x-labweaver-lease-revision")
        .and_then(|v| v.to_str().ok());
    let lease = match (lease_id, lease_revision) {
        (Some(id), Some(rev)) => Some((
            id.parse()
                .map_err(|_| TerminalBridgeError::IdentityInvalid)?,
            Revision::new(
                rev.parse()
                    .map_err(|_| TerminalBridgeError::IdentityInvalid)?,
            )
            .map_err(|_| TerminalBridgeError::IdentityInvalid)?,
        )),
        (None, None) => None,
        _ => return Err(TerminalBridgeError::IdentityInvalid),
    };
    Ok(Identity {
        session_id: header(headers, "x-labweaver-console-session-id")?,
        grant_id: header(headers, "x-labweaver-access-grant-id")?,
        grant_revision: revision_header(headers, "x-labweaver-access-grant-revision")?,
        environment_revision: revision_header(headers, "x-labweaver-environment-revision")?,
        lease,
    })
}
fn header<T: std::str::FromStr>(headers: &HeaderMap, name: &str) -> Result<T, TerminalBridgeError> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .ok_or(TerminalBridgeError::IdentityInvalid)
}
fn revision_header(headers: &HeaderMap, name: &str) -> Result<Revision, TerminalBridgeError> {
    Revision::new(header(headers, name)?).map_err(|_| TerminalBridgeError::IdentityInvalid)
}
fn required(name: &'static str) -> Result<String, TerminalBridgeError> {
    std::env::var(name)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .ok_or(TerminalBridgeError::Configuration)
}

#[derive(Debug, thiserror::Error)]
pub enum TerminalBridgeError {
    #[error("LW_ENV_CONSOLE_CONFIGURATION_INVALID")]
    Configuration,
    #[error("LW_ENV_CONSOLE_CERTIFICATE_INVALID")]
    Certificate,
    #[error("LW_ENV_CONSOLE_CALLER_DENIED")]
    CallerDenied,
    #[error("LW_ENV_CONSOLE_SUBPROTOCOL_REQUIRED")]
    SubprotocolRequired,
    #[error("LW_ENV_CONSOLE_IDENTITY_INVALID")]
    IdentityInvalid,
    #[error("LW_ENV_CONSOLE_SCOPE_DENIED")]
    ScopeDenied,
    #[error("LW_ENV_CONSOLE_CAPABILITY_DENIED")]
    CapabilityDenied,
    #[error("LW_ENV_CONSOLE_CONTRACT_INVALID")]
    ContractInvalid,
    #[error("LW_ENV_CONSOLE_AUTHORITY_UNAVAILABLE")]
    AuthorityUnavailable,
    #[error("LW_ENV_CONSOLE_IO_FAILED")]
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
