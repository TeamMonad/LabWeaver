//! mTLS-only browser terminal executor backed by Kubernetes exec PTY.

use std::{io::Cursor, net::SocketAddr, str::FromStr, sync::Arc, time::Duration};

use axum::{
    Router,
    extract::{
        Extension, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use contracts::{CourseId, EnvironmentId, authoring::TerminalSpec};
use futures_util::{SinkExt, StreamExt};
use k8s_openapi::api::core::v1::Pod;
use kube::{
    Api, Client, Config,
    api::{AttachParams, ListParams, TerminalSize},
};
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::{
    MtlsConfig, RuntimeExecutorConfiguration, VerifiedCallerIdentity, serve_owner_resolver_mtls,
};

const SUBPROTOCOL: &str = "labweaver.terminal.v1";
const ENVIRONMENT_SERVICE_SAN: &str = "spiffe://labweaver/environment-service";
const MAX_FRAME_BYTES: usize = 64 * 1024;

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
    let pods: Api<Pod> = Api::namespaced(state.client, &namespace);
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
    let params = AttachParams::interactive_tty()
        .container("runtime")
        .max_stdin_buf_size(MAX_FRAME_BYTES)
        .max_stdout_buf_size(MAX_FRAME_BYTES);
    let mut process = pods
        .exec(pod_name, command, &params)
        .await
        .map_err(|_| "LW_CONTAINER_TERMINAL_EXEC_FAILED")?;
    let mut stdin = process
        .stdin()
        .ok_or("LW_CONTAINER_TERMINAL_STDIN_UNAVAILABLE")?;
    let mut stdout = process
        .stdout()
        .ok_or("LW_CONTAINER_TERMINAL_STDOUT_UNAVAILABLE")?;
    let mut terminal_size = process
        .terminal_size()
        .ok_or("LW_CONTAINER_TERMINAL_RESIZE_UNAVAILABLE")?;
    terminal_size
        .send(TerminalSize {
            width: request.cols,
            height: request.rows,
        })
        .await
        .map_err(|_| "LW_CONTAINER_TERMINAL_RESIZE_FAILED")?;
    let result = async {
        socket
            .send(Message::Text(r#"{"type":"ready"}"#.into()))
            .await
            .map_err(|_| "LW_CONTAINER_TERMINAL_CLIENT_DISCONNECTED")?;
        let (mut sender, mut receiver) = socket.split();
        let mut output = vec![0_u8; 16 * 1024];
        loop {
            tokio::select! {
                read = stdout.read(&mut output) => {
                    let count = read.map_err(|_| "LW_CONTAINER_TERMINAL_STDOUT_FAILED")?;
                    if count == 0 {
                        sender.send(Message::Text(r#"{"type":"exit"}"#.into())).await
                            .map_err(|_| "LW_CONTAINER_TERMINAL_CLIENT_DISCONNECTED")?;
                        break;
                    }
                    sender.send(Message::Binary(output[..count].to_vec().into())).await
                        .map_err(|_| "LW_CONTAINER_TERMINAL_CLIENT_DISCONNECTED")?;
                }
                message = receiver.next() => {
                    let Some(message) = message else { break; };
                    match message.map_err(|_| "LW_CONTAINER_TERMINAL_CLIENT_DISCONNECTED")? {
                        Message::Binary(bytes) => {
                            stdin.write_all(&bytes).await
                                .map_err(|_| "LW_CONTAINER_TERMINAL_STDIN_FAILED")?;
                        }
                        Message::Text(text) => {
                            match serde_json::from_str::<TerminalControl>(&text)
                                .map_err(|_| "LW_CONTAINER_TERMINAL_CONTROL_INVALID")? {
                                TerminalControl::Open { cols, rows }
                                | TerminalControl::Resize { cols, rows } => {
                                    if cols == 0 || rows == 0 || cols > 500 || rows > 200 {
                                        return Err("LW_CONTAINER_TERMINAL_SIZE_INVALID");
                                    }
                                    terminal_size.send(TerminalSize { width: cols, height: rows }).await
                                        .map_err(|_| "LW_CONTAINER_TERMINAL_RESIZE_FAILED")?;
                                }
                            }
                        }
                        Message::Close(_) => break,
                        Message::Ping(value) => sender.send(Message::Pong(value)).await
                            .map_err(|_| "LW_CONTAINER_TERMINAL_CLIENT_DISCONNECTED")?,
                        Message::Pong(_) => {}
                    }
                }
            }
        }
        Ok(())
    }
    .await;
    process.abort();
    result
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
    use k8s_openapi::api::core::v1::Pod;

    use super::ready_runtime_pod;

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
