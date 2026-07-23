//! mTLS-only browser terminal executor backed by Kubernetes exec PTY.

use std::{io::Cursor, net::SocketAddr, str::FromStr, sync::Arc, time::Duration};

use axum::{
    Router,
    extract::{
        Extension, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{self, HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use contracts::{CourseId, EnvironmentId, authoring::TerminalSpec};
use futures_util::{SinkExt, StreamExt};
use k8s_openapi::api::core::v1::Pod;
use kube::{
    Api, Client, Config,
    api::{ListParams, TerminalSize},
};
use kube_tokio_tungstenite::tungstenite::protocol::Message as KubernetesMessage;
use serde::Deserialize;

use crate::{
    MtlsConfig, RuntimeExecutorConfiguration, VerifiedCallerIdentity, serve_owner_resolver_mtls,
};

const SUBPROTOCOL: &str = "labweaver.terminal.v1";
const ENVIRONMENT_SERVICE_SAN: &str = "spiffe://labweaver/environment-service";
const MAX_FRAME_BYTES: usize = 64 * 1024;
const STDIN_CHANNEL: u8 = 0;
const STDOUT_CHANNEL: u8 = 1;
const STATUS_CHANNEL: u8 = 3;
const RESIZE_CHANNEL: u8 = 4;
const CLOSE_CHANNEL: u8 = 255;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalExecutorServerConfig {
    pub bind_addr: String,
    pub client_ca_file: String,
    pub server_certificate_file: String,
    pub server_private_key_file: String,
    pub allowed_caller_san: String,
}

#[derive(Clone)]
pub struct TerminalExecutorServer {
    listener: Arc<tokio::net::TcpListener>,
    router: Router,
    tls: MtlsConfig,
}

#[derive(Clone)]
struct TerminalExecutorState {
    client: Client,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TerminalExecRequest {
    environment_id: EnvironmentId,
    course_id: CourseId,
    terminal: TerminalSpec,
    cols: u16,
    rows: u16,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum TerminalControl {
    Open { cols: u16, rows: u16 },
    Resize { cols: u16, rows: u16 },
}

impl TerminalExecutorServer {
    pub async fn new(
        server: &TerminalExecutorServerConfig,
        kubernetes: &RuntimeExecutorConfiguration,
    ) -> Result<Self, TerminalExecutorServerError> {
        if server.allowed_caller_san != ENVIRONMENT_SERVICE_SAN {
            return Err(TerminalExecutorServerError::Configuration);
        }
        let address = SocketAddr::from_str(&server.bind_addr)
            .map_err(|_| TerminalExecutorServerError::Configuration)?;
        let listener = tokio::net::TcpListener::bind(address).await?;
        let tls = MtlsConfig::from_pem(
            &std::fs::read(&server.client_ca_file)?,
            &std::fs::read(&server.server_certificate_file)?,
            &std::fs::read(&server.server_private_key_file)?,
        )?;
        let client = explicit_kube_client(kubernetes)?;
        let state = TerminalExecutorState { client };
        let router = Router::new()
            .route("/internal/v1/container-terminal", get(upgrade_terminal))
            .with_state(state);
        Ok(Self {
            listener: Arc::new(listener),
            router,
            tls,
        })
    }

    pub async fn serve(self) -> Result<(), TerminalExecutorServerError> {
        let listener = Arc::try_unwrap(self.listener)
            .map_err(|_| TerminalExecutorServerError::Configuration)?;
        serve_owner_resolver_mtls(
            listener,
            self.router,
            self.tls,
            std::future::pending::<Result<(), crate::MtlsServerError>>(),
        )
        .await?;
        Ok(())
    }
}

fn explicit_kube_client(
    configuration: &RuntimeExecutorConfiguration,
) -> Result<Client, TerminalExecutorServerError> {
    let cluster_url = configuration
        .api_server
        .as_str()
        .parse()
        .map_err(|_| TerminalExecutorServerError::Configuration)?;
    let mut config = Config::new(cluster_url);
    let ca = std::fs::read(&configuration.cluster_ca_file)?;
    let root_cert = rustls_pemfile::certs(&mut Cursor::new(ca))
        .map(|certificate| certificate.map(|certificate| certificate.as_ref().to_vec()))
        .collect::<Result<Vec<_>, _>>()?;
    if root_cert.is_empty() {
        return Err(TerminalExecutorServerError::Configuration);
    }
    config.root_cert = Some(root_cert);
    config.auth_info.token_file = Some(
        configuration
            .bearer_token_file
            .to_str()
            .ok_or(TerminalExecutorServerError::Configuration)?
            .to_owned(),
    );
    let timeout = Duration::from_millis(configuration.request_timeout_milliseconds);
    config.connect_timeout = Some(timeout);
    config.read_timeout = Some(timeout);
    config.write_timeout = Some(timeout);
    Client::try_from(config).map_err(TerminalExecutorServerError::KubernetesClient)
}

async fn upgrade_terminal(
    State(state): State<TerminalExecutorState>,
    caller: Option<Extension<VerifiedCallerIdentity>>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Result<Response, TerminalExecutorServerError> {
    if !caller.is_some_and(|Extension(identity)| identity.contains_san(ENVIRONMENT_SERVICE_SAN)) {
        return Err(TerminalExecutorServerError::CallerDenied);
    }
    if !headers
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.split(',').any(|part| part.trim() == SUBPROTOCOL))
    {
        return Err(TerminalExecutorServerError::SubprotocolRequired);
    }
    Ok(upgrade
        .protocols([SUBPROTOCOL])
        .max_frame_size(MAX_FRAME_BYTES)
        .on_upgrade(move |socket| async move {
            if let Err(code) = execute_terminal(state, socket).await {
                tracing::warn!(
                    event = "environment.container_terminal.closed",
                    diagnostic = code
                );
            }
        }))
}

#[allow(
    clippy::too_many_lines,
    reason = "the PTY session keeps bounded admission, exec, resize and cleanup in one auditable lifetime"
)]
async fn execute_terminal(
    state: TerminalExecutorState,
    mut socket: WebSocket,
) -> Result<(), &'static str> {
    let request = match socket.next().await {
        Some(Ok(Message::Text(value))) => serde_json::from_str::<TerminalExecRequest>(&value)
            .map_err(|_| "LW_CONTAINER_TERMINAL_REQUEST_INVALID")?,
        _ => return Err("LW_CONTAINER_TERMINAL_REQUEST_REQUIRED"),
    };
    request
        .terminal
        .validate()
        .map_err(|_| "LW_CONTAINER_TERMINAL_SPEC_INVALID")?;
    if request.cols == 0 || request.rows == 0 || request.cols > 500 || request.rows > 200 {
        return Err("LW_CONTAINER_TERMINAL_SIZE_INVALID");
    }
    let namespace = format!("lw-env-{}", request.environment_id);
    let pods: Api<Pod> = Api::namespaced(state.client.clone(), &namespace);
    let labels = format!(
        "app=runtime,labweaver.io/environment-id={},labweaver.io/course-id={}",
        request.environment_id, request.course_id
    );
    let candidates = pods
        .list(&ListParams::default().labels(&labels))
        .await
        .map_err(|_| "LW_CONTAINER_TERMINAL_POD_LIST_FAILED")?
        .into_iter()
        .filter(ready_runtime_pod)
        .collect::<Vec<_>>();
    if candidates.len() != 1 {
        return Err(if candidates.is_empty() {
            "LW_CONTAINER_TERMINAL_POD_NOT_READY"
        } else {
            "LW_CONTAINER_TERMINAL_POD_AMBIGUOUS"
        });
    }
    let pod_name = candidates[0]
        .metadata
        .name
        .as_deref()
        .ok_or("LW_CONTAINER_TERMINAL_POD_IDENTITY_INVALID")?;
    let mut command = Vec::with_capacity(request.terminal.args.len() + 1);
    command.push(request.terminal.executable.clone());
    command.extend(request.terminal.args);
    let mut exec_request = kubernetes_exec_request(&namespace, pod_name, &command)
        .map_err(|_| "LW_CONTAINER_TERMINAL_EXEC_REQUEST_INVALID")?;
    exec_request.extensions_mut().insert("exec");
    let connection = state.client.connect(exec_request).await.map_err(|error| {
        tracing::warn!(
            event = "environment.container_terminal.exec_failed",
            diagnostic = "LW_CONTAINER_TERMINAL_EXEC_FAILED",
            error = %error
        );
        "LW_CONTAINER_TERMINAL_EXEC_FAILED"
    })?;
    let supports_stream_close = connection.supports_stream_close();
    let (mut kubernetes_tx, mut kubernetes_rx) = connection.into_stream().split();
    send_terminal_size(&mut kubernetes_tx, request.cols, request.rows).await?;
    socket
        .send(Message::Text(r#"{"type":"ready"}"#.into()))
        .await
        .map_err(|_| "LW_CONTAINER_TERMINAL_CLIENT_DISCONNECTED")?;
    let (mut browser_tx, mut browser_rx) = socket.split();
    loop {
        tokio::select! {
            message = kubernetes_rx.next() => {
                let Some(message) = message else {
                    browser_tx.send(Message::Text(r#"{"type":"exit"}"#.into())).await
                        .map_err(|_| "LW_CONTAINER_TERMINAL_CLIENT_DISCONNECTED")?;
                    break;
                };
                match message.map_err(|_| "LW_CONTAINER_TERMINAL_EXEC_STREAM_FAILED")? {
                    KubernetesMessage::Binary(frame) if frame.first() == Some(&STDOUT_CHANNEL) => {
                        browser_tx.send(Message::Binary(frame[1..].to_vec().into())).await
                            .map_err(|_| "LW_CONTAINER_TERMINAL_CLIENT_DISCONNECTED")?;
                    }
                    KubernetesMessage::Binary(frame) if frame.first() == Some(&STATUS_CHANNEL) => {
                        browser_tx.send(Message::Text(r#"{"type":"exit"}"#.into())).await
                            .map_err(|_| "LW_CONTAINER_TERMINAL_CLIENT_DISCONNECTED")?;
                        break;
                    }
                    KubernetesMessage::Ping(value) => {
                        kubernetes_tx.send(KubernetesMessage::Pong(value)).await
                            .map_err(|_| "LW_CONTAINER_TERMINAL_EXEC_STREAM_FAILED")?;
                    }
                    KubernetesMessage::Close(_) => break,
                    _ => {}
                }
            }
            message = browser_rx.next() => {
                let Some(message) = message else { break; };
                match message.map_err(|_| "LW_CONTAINER_TERMINAL_CLIENT_DISCONNECTED")? {
                    Message::Binary(bytes) => {
                        kubernetes_tx.send(KubernetesMessage::Binary(
                            channel_frame(STDIN_CHANNEL, &bytes).into()
                        )).await.map_err(|_| "LW_CONTAINER_TERMINAL_STDIN_FAILED")?;
                    }
                    Message::Text(text) => {
                        match serde_json::from_str::<TerminalControl>(&text)
                            .map_err(|_| "LW_CONTAINER_TERMINAL_CONTROL_INVALID")? {
                            TerminalControl::Open { cols, rows }
                            | TerminalControl::Resize { cols, rows } => {
                                if cols == 0 || rows == 0 || cols > 500 || rows > 200 {
                                    return Err("LW_CONTAINER_TERMINAL_SIZE_INVALID");
                                }
                                send_terminal_size(&mut kubernetes_tx, cols, rows).await?;
                            }
                        }
                    }
                    Message::Close(_) => {
                        if supports_stream_close {
                            kubernetes_tx.send(KubernetesMessage::Binary(
                                vec![CLOSE_CHANNEL, STDIN_CHANNEL].into()
                            )).await.map_err(|_| "LW_CONTAINER_TERMINAL_EXEC_STREAM_FAILED")?;
                        }
                        break;
                    }
                    Message::Ping(value) => browser_tx.send(Message::Pong(value)).await
                        .map_err(|_| "LW_CONTAINER_TERMINAL_CLIENT_DISCONNECTED")?,
                    Message::Pong(_) => {}
                }
            }
        }
    }
    let _ = kubernetes_tx.close().await;
    Ok(())
}

fn kubernetes_exec_request(
    namespace: &str,
    pod_name: &str,
    command: &[String],
) -> Result<http::Request<Vec<u8>>, http::Error> {
    let target = format!("/api/v1/namespaces/{namespace}/pods/{pod_name}/exec?");
    let mut query = url::form_urlencoded::Serializer::new(target);
    for (name, value) in [
        ("stdin", "true"),
        ("stdout", "true"),
        ("stderr", "false"),
        ("tty", "true"),
        ("container", "runtime"),
    ] {
        query.append_pair(name, value);
    }
    for argument in command {
        query.append_pair("command", argument);
    }
    http::Request::post(query.finish()).body(Vec::new())
}

fn channel_frame(channel: u8, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(payload.len() + 1);
    frame.push(channel);
    frame.extend_from_slice(payload);
    frame
}

async fn send_terminal_size<S>(sink: &mut S, cols: u16, rows: u16) -> Result<(), &'static str>
where
    S: futures_util::Sink<KubernetesMessage> + Unpin,
{
    let size = serde_json::to_vec(&TerminalSize {
        width: cols,
        height: rows,
    })
    .map_err(|_| "LW_CONTAINER_TERMINAL_RESIZE_FAILED")?;
    sink.send(KubernetesMessage::Binary(
        channel_frame(RESIZE_CHANNEL, &size).into(),
    ))
    .await
    .map_err(|_| "LW_CONTAINER_TERMINAL_RESIZE_FAILED")
}

fn ready_runtime_pod(pod: &Pod) -> bool {
    pod.metadata.deletion_timestamp.is_none()
        && pod.status.as_ref().is_some_and(|status| {
            status.phase.as_deref() == Some("Running")
                && status.conditions.as_ref().is_some_and(|conditions| {
                    conditions
                        .iter()
                        .any(|condition| condition.type_ == "Ready" && condition.status == "True")
                })
                && status.container_statuses.as_ref().is_some_and(|statuses| {
                    statuses
                        .iter()
                        .any(|container| container.name == "runtime" && container.ready)
                })
        })
}

#[derive(Debug, thiserror::Error)]
pub enum TerminalExecutorServerError {
    #[error("LW_CONTAINER_TERMINAL_CONFIGURATION_INVALID")]
    Configuration,
    #[error("LW_CONTAINER_TERMINAL_CALLER_DENIED")]
    CallerDenied,
    #[error("LW_CONTAINER_TERMINAL_SUBPROTOCOL_REQUIRED")]
    SubprotocolRequired,
    #[error("LW_CONTAINER_TERMINAL_KUBERNETES_CLIENT_FAILED")]
    KubernetesClient(#[source] kube::Error),
    #[error("LW_CONTAINER_TERMINAL_IO_FAILED")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Tls(#[from] crate::MtlsServerError),
}

impl IntoResponse for TerminalExecutorServerError {
    fn into_response(self) -> Response {
        let status = match self {
            Self::CallerDenied => StatusCode::FORBIDDEN,
            Self::SubprotocolRequired => StatusCode::BAD_REQUEST,
            _ => StatusCode::SERVICE_UNAVAILABLE,
        };
        (status, self.to_string()).into_response()
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use axum::http::Method;
    use k8s_openapi::api::core::v1::Pod;

    use super::{STDIN_CHANNEL, channel_frame, kubernetes_exec_request, ready_runtime_pod};

    #[test]
    fn exec_uses_post_and_direct_channel_framing() {
        let request = kubernetes_exec_request(
            "lw-env-00000000-0000-7000-8000-000000000401",
            "runtime",
            &["/bin/sh".to_owned(), "-l".to_owned()],
        )
        .expect("exec request");
        assert_eq!(request.method(), Method::POST);
        let uri = request.uri().to_string();
        assert!(uri.contains("/pods/runtime/exec?"));
        assert!(uri.contains("container=runtime"));
        assert!(uri.contains("command=%2Fbin%2Fsh"));
        assert_eq!(channel_frame(STDIN_CHANNEL, b"pwd\n"), b"\0pwd\n");
    }

    #[test]
    fn only_a_running_ready_runtime_container_is_eligible() {
        let ready: Pod = serde_json::from_value(serde_json::json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {"name": "runtime-1"},
            "status": {
                "phase": "Running",
                "conditions": [{"type": "Ready", "status": "True"}],
                "containerStatuses": [{
                    "name": "runtime",
                    "ready": true,
                    "restartCount": 0,
                    "image": "example.invalid/runtime",
                    "imageID": "sha256:test"
                }]
            }
        }))
        .expect("pod fixture must deserialize");
        assert!(ready_runtime_pod(&ready));

        let mut not_ready = ready;
        not_ready.status.as_mut().expect("status").phase = Some("Pending".to_owned());
        assert!(!ready_runtime_pod(&not_ready));
    }
}
