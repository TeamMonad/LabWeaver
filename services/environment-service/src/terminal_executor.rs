//! mTLS-only Kubernetes exec PTY for the fixed `runtime` container.

use crate::{
    MtlsConfig, RuntimeExecutorConfiguration, VerifiedCallerIdentity, serve_owner_resolver_mtls,
};
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
use contracts::{
    CourseId, EnvironmentId, ReleaseId,
    access::{ConsoleClientControl, ConsoleKind},
    authoring::TerminalSpec,
};
use futures_util::{SinkExt, StreamExt};
use k8s_openapi::api::core::v1::Pod;
use kube::{
    Api, Client, Config,
    api::{AttachParams, ListParams, TerminalSize},
};
use serde::Deserialize;
use std::{io::Cursor, net::SocketAddr, str::FromStr, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const ENVIRONMENT_SERVICE_SAN: &str = "spiffe://labweaver/environment-service";
const MAX_FRAME_BYTES: usize = 64 * 1024;
const MAX_SESSION_OUTPUT_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalExecutorServerConfig {
    pub bind_addr: String,
    pub client_ca_file: String,
    pub server_certificate_file: String,
    pub server_private_key_file: String,
    pub allowed_caller_san: String,
}
pub struct TerminalExecutorServer {
    listener: tokio::net::TcpListener,
    router: Router,
    tls: MtlsConfig,
}
#[derive(Clone)]
struct ExecutorState {
    client: Client,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ExecRequest {
    environment_id: EnvironmentId,
    course_id: CourseId,
    release_id: ReleaseId,
    release_version: u64,
    terminal: TerminalSpec,
    cols: u16,
    rows: u16,
}

impl TerminalExecutorServer {
    pub async fn new(
        server: &TerminalExecutorServerConfig,
        kubernetes: &RuntimeExecutorConfiguration,
    ) -> Result<Self, TerminalExecutorServerError> {
        if server.allowed_caller_san != ENVIRONMENT_SERVICE_SAN {
            return Err(TerminalExecutorServerError::Configuration);
        }
        let listener = tokio::net::TcpListener::bind(
            SocketAddr::from_str(&server.bind_addr)
                .map_err(|_| TerminalExecutorServerError::Configuration)?,
        )
        .await?;
        let tls = MtlsConfig::from_pem(
            &std::fs::read(&server.client_ca_file)?,
            &std::fs::read(&server.server_certificate_file)?,
            &std::fs::read(&server.server_private_key_file)?,
        )?;
        let state = ExecutorState {
            client: explicit_kube_client(kubernetes)?,
        };
        let router = Router::new()
            .route("/internal/v1/container-terminal", get(upgrade))
            .with_state(state);
        Ok(Self {
            listener,
            router,
            tls,
        })
    }
    pub async fn serve(self) -> Result<(), TerminalExecutorServerError> {
        serve_owner_resolver_mtls(
            self.listener,
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
    let mut config = Config::new(
        configuration
            .api_server
            .as_str()
            .parse()
            .map_err(|_| TerminalExecutorServerError::Configuration)?,
    );
    let roots = rustls_pemfile::certs(&mut Cursor::new(std::fs::read(
        &configuration.cluster_ca_file,
    )?))
    .map(|c| c.map(|c| c.as_ref().to_vec()))
    .collect::<Result<Vec<_>, _>>()?;
    if roots.is_empty() {
        return Err(TerminalExecutorServerError::Configuration);
    }
    config.root_cert = Some(roots);
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
async fn upgrade(
    State(state): State<ExecutorState>,
    caller: Option<Extension<VerifiedCallerIdentity>>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Result<Response, TerminalExecutorServerError> {
    if !caller.is_some_and(|Extension(identity)| identity.contains_san(ENVIRONMENT_SERVICE_SAN)) {
        return Err(TerminalExecutorServerError::CallerDenied);
    }
    let protocol = ConsoleKind::Xterm.websocket_subprotocol();
    if !headers
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|p| p.trim() == protocol))
    {
        return Err(TerminalExecutorServerError::SubprotocolRequired);
    }
    Ok(upgrade
        .protocols([protocol])
        .max_frame_size(MAX_FRAME_BYTES)
        .on_upgrade(move |socket| async move {
            if let Err(code) = execute(state, socket).await {
                tracing::warn!(
                    event = "environment.container_console.closed",
                    diagnostic = code
                );
            }
        }))
}
async fn execute(state: ExecutorState, mut socket: WebSocket) -> Result<(), &'static str> {
    let request = match socket.next().await {
        Some(Ok(Message::Text(value))) => {
            contracts::parse_strict_json::<ExecRequest>(value.as_bytes())
                .map_err(|_| "LW_CONTAINER_CONSOLE_REQUEST_INVALID")?
        }
        _ => return Err("LW_CONTAINER_CONSOLE_REQUEST_REQUIRED"),
    };
    request
        .terminal
        .validate()
        .map_err(|_| "LW_CONTAINER_CONSOLE_SPEC_INVALID")?;
    valid_size(request.cols, request.rows)?;
    let namespace = format!("lw-env-{}", request.environment_id);
    let pods: Api<Pod> = Api::namespaced(state.client, &namespace);
    if request.release_version == 0 {
        return Err("LW_CONTAINER_CONSOLE_REQUEST_INVALID");
    }
    let labels = format!(
        "app=runtime,labweaver.io/environment-id={},labweaver.io/course-id={},labweaver.io/release-id={},labweaver.io/release-version={}",
        request.environment_id, request.course_id, request.release_id, request.release_version
    );
    let candidates = pods
        .list(&ListParams::default().labels(&labels))
        .await
        .map_err(|_| "LW_CONTAINER_CONSOLE_POD_LIST_FAILED")?;
    let pod_name = select_runtime_pod_name(&candidates.items)?;
    let command = terminal_command(request.terminal);
    let params = AttachParams::interactive_tty()
        .container("runtime")
        .max_stdin_buf_size(MAX_FRAME_BYTES)
        .max_stdout_buf_size(MAX_FRAME_BYTES);
    let mut process = pods
        .exec(pod_name, command, &params)
        .await
        .map_err(|_| "LW_CONTAINER_CONSOLE_EXEC_FAILED")?;
    let mut stdin = process
        .stdin()
        .ok_or("LW_CONTAINER_CONSOLE_STDIN_UNAVAILABLE")?;
    let mut stdout = process
        .stdout()
        .ok_or("LW_CONTAINER_CONSOLE_STDOUT_UNAVAILABLE")?;
    let mut size = process
        .terminal_size()
        .ok_or("LW_CONTAINER_CONSOLE_RESIZE_UNAVAILABLE")?;
    size.send(TerminalSize {
        width: request.cols,
        height: request.rows,
    })
    .await
    .map_err(|_| "LW_CONTAINER_CONSOLE_RESIZE_FAILED")?;
    let result=async{let(mut sender,mut receiver)=socket.split();let mut output=vec![0_u8;16*1024];let mut total=0_u64;loop{tokio::select!{
        read=stdout.read(&mut output)=>{let count=read.map_err(|_|"LW_CONTAINER_CONSOLE_STDOUT_FAILED")?;if count==0{return Ok(())}total=total.saturating_add(count as u64);if total>MAX_SESSION_OUTPUT_BYTES{return Err("LW_CONTAINER_CONSOLE_OUTPUT_LIMIT_EXCEEDED")}sender.send(Message::Binary(output[..count].to_vec().into())).await.map_err(|_|"LW_CONTAINER_CONSOLE_CLIENT_DISCONNECTED")?;}
        message=receiver.next()=>{let Some(message)=message else{return Ok(())};match message.map_err(|_|"LW_CONTAINER_CONSOLE_CLIENT_DISCONNECTED")?{Message::Binary(bytes) if bytes.len()<=MAX_FRAME_BYTES=>stdin.write_all(&bytes).await.map_err(|_|"LW_CONTAINER_CONSOLE_STDIN_FAILED")?,Message::Text(text) if text.len()<=1024=>{let control:ConsoleClientControl=contracts::parse_strict_json(text.as_bytes()).map_err(|_|"LW_CONTAINER_CONSOLE_CONTROL_INVALID")?;control.validate().map_err(|_|"LW_CONTAINER_CONSOLE_SIZE_INVALID")?;let ConsoleClientControl::Resize{cols,rows}=control;size.send(TerminalSize{width:cols,height:rows}).await.map_err(|_|"LW_CONTAINER_CONSOLE_RESIZE_FAILED")?},Message::Ping(v)=>sender.send(Message::Pong(v)).await.map_err(|_|"LW_CONTAINER_CONSOLE_CLIENT_DISCONNECTED")?,Message::Pong(_)=>{},Message::Close(_)=>return Ok(()),_=>return Err("LW_CONTAINER_CONSOLE_FRAME_INVALID")}}
    }}}.await;
    process.abort();
    result
}
fn valid_size(cols: u16, rows: u16) -> Result<(), &'static str> {
    ConsoleClientControl::Resize { cols, rows }
        .validate()
        .map_err(|_| "LW_CONTAINER_CONSOLE_SIZE_INVALID")
}
fn terminal_command(terminal: TerminalSpec) -> Vec<String> {
    let mut command = Vec::with_capacity(terminal.args.len() + 6);
    command.extend([
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        "cd -- \"$1\" && shift && exec \"$@\"".to_owned(),
        "labweaver-console".to_owned(),
        terminal.working_directory,
        terminal.executable,
    ]);
    command.extend(terminal.args);
    command
}
fn ready_runtime_pod(pod: &Pod) -> bool {
    pod.metadata.deletion_timestamp.is_none()
        && pod.status.as_ref().is_some_and(|status| {
            status.phase.as_deref() == Some("Running")
                && status.conditions.as_ref().is_some_and(|conditions| {
                    conditions
                        .iter()
                        .any(|c| c.type_ == "Ready" && c.status == "True")
                })
                && status
                    .container_statuses
                    .as_ref()
                    .is_some_and(|statuses| statuses.iter().any(|c| c.name == "runtime" && c.ready))
        })
}

fn select_runtime_pod_name(pods: &[Pod]) -> Result<&str, &'static str> {
    let mut candidates = pods.iter().filter(|pod| ready_runtime_pod(pod));
    let candidate = candidates
        .next()
        .ok_or("LW_CONTAINER_CONSOLE_POD_NOT_READY")?;
    if candidates.next().is_some() {
        return Err("LW_CONTAINER_CONSOLE_POD_AMBIGUOUS");
    }
    candidate
        .metadata
        .name
        .as_deref()
        .ok_or("LW_CONTAINER_CONSOLE_POD_IDENTITY_INVALID")
}

#[derive(Debug, thiserror::Error)]
pub enum TerminalExecutorServerError {
    #[error("LW_CONTAINER_CONSOLE_CONFIGURATION_INVALID")]
    Configuration,
    #[error("LW_CONTAINER_CONSOLE_CALLER_DENIED")]
    CallerDenied,
    #[error("LW_CONTAINER_CONSOLE_SUBPROTOCOL_REQUIRED")]
    SubprotocolRequired,
    #[error("LW_CONTAINER_CONSOLE_KUBERNETES_CLIENT_FAILED")]
    KubernetesClient(#[source] kube::Error),
    #[error("LW_CONTAINER_CONSOLE_IO_FAILED")]
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
mod tests {
    use super::*;
    #[test]
    fn only_running_ready_runtime_container_is_eligible() -> Result<(), serde_json::Error> {
        let ready: Pod = serde_json::from_value(
            serde_json::json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"runtime-1"},"status":{"phase":"Running","conditions":[{"type":"Ready","status":"True"}],"containerStatuses":[{"name":"runtime","ready":true,"restartCount":0,"image":"example.invalid/runtime","imageID":"sha256:test"}]}}),
        )?;
        assert!(ready_runtime_pod(&ready));
        assert_eq!(
            select_runtime_pod_name(std::slice::from_ref(&ready)),
            Ok("runtime-1")
        );
        let mut pending = ready.clone();
        if let Some(status) = &mut pending.status {
            status.phase = Some("Pending".to_owned());
        }
        assert_eq!(
            select_runtime_pod_name(&[pending]),
            Err("LW_CONTAINER_CONSOLE_POD_NOT_READY")
        );
        assert_eq!(
            select_runtime_pod_name(&[ready.clone(), ready]),
            Err("LW_CONTAINER_CONSOLE_POD_AMBIGUOUS")
        );
        Ok(())
    }

    #[test]
    fn terminal_command_uses_positional_arguments_without_shell_interpolation() {
        let command = terminal_command(TerminalSpec {
            executable: "/bin/bash".to_owned(),
            args: vec!["--noprofile".to_owned()],
            working_directory: "/workspace".to_owned(),
        });
        assert_eq!(
            command,
            [
                "/bin/sh",
                "-c",
                "cd -- \"$1\" && shift && exec \"$@\"",
                "labweaver-console",
                "/workspace",
                "/bin/bash",
                "--noprofile"
            ]
        );
    }

    #[test]
    fn terminal_size_rejects_zero_and_out_of_contract_bounds() {
        assert!(valid_size(1, 1).is_ok());
        assert!(valid_size(500, 200).is_ok());
        assert!(valid_size(0, 24).is_err());
        assert!(valid_size(80, 0).is_err());
        assert!(valid_size(501, 24).is_err());
        assert!(valid_size(80, 201).is_err());
    }
}
