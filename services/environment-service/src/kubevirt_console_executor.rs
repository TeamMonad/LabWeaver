//! mTLS-only adapter from an authoritative Environment identity to `KubeVirt` VMI VNC.

use crate::{MtlsConfig, VerifiedCallerIdentity, serve_owner_resolver_mtls};
use axum::{
    Router,
    extract::{
        Extension, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, Request, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use contracts::{CourseId, EnvironmentId, ReleaseId, Revision, access::ConsoleKind};
use futures_util::{SinkExt, StreamExt};
use hyper_util::rt::TokioIo;
use kube::{Client, Config, client::Body};
use serde::Deserialize;
use serde_json::Value;
use sqlx::{PgPool, Row};
use std::{io::Cursor, net::SocketAddr, path::PathBuf, str::FromStr, time::Duration};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        self,
        protocol::{Message as UpstreamMessage, Role},
    },
};
use uuid::Uuid;

const ENVIRONMENT_SERVICE_SAN: &str = "spiffe://labweaver/environment-service";
const KUBEVIRT_PLAIN_PROTOCOL: &str = "plain.kubevirt.io";
const MAX_FRAME_BYTES: usize = 64 * 1024;
const VMI_NAME: &str = "runtime";

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KubeVirtConsoleExecutorServerConfig {
    pub bind_addr: String,
    pub client_ca_file: String,
    pub server_certificate_file: String,
    pub server_private_key_file: String,
    pub allowed_caller_san: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct KubeVirtConsoleKubernetesConfiguration {
    pub api_server: url::Url,
    pub bearer_token_file: PathBuf,
    pub cluster_ca_file: PathBuf,
    pub request_timeout_milliseconds: u64,
}

pub struct KubeVirtConsoleExecutorServer {
    listener: tokio::net::TcpListener,
    router: Router,
    tls: MtlsConfig,
}

#[derive(Clone)]
struct ExecutorState {
    pool: PgPool,
    client: Client,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VncRequest {
    environment_id: EnvironmentId,
    course_id: CourseId,
    release_id: ReleaseId,
    release_version: u64,
    environment_revision: Revision,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct VncTarget {
    namespace: String,
    name: String,
    vmi_uid: Uuid,
}

impl KubeVirtConsoleExecutorServer {
    pub async fn new(
        server: &KubeVirtConsoleExecutorServerConfig,
        kubernetes: &KubeVirtConsoleKubernetesConfiguration,
        pool: PgPool,
    ) -> Result<Self, KubeVirtConsoleExecutorServerError> {
        if server.allowed_caller_san != ENVIRONMENT_SERVICE_SAN {
            return Err(KubeVirtConsoleExecutorServerError::Configuration);
        }
        let listener = tokio::net::TcpListener::bind(
            SocketAddr::from_str(&server.bind_addr)
                .map_err(|_| KubeVirtConsoleExecutorServerError::Configuration)?,
        )
        .await?;
        let tls = MtlsConfig::from_pem(
            &std::fs::read(&server.client_ca_file)?,
            &std::fs::read(&server.server_certificate_file)?,
            &std::fs::read(&server.server_private_key_file)?,
        )?;
        let state = ExecutorState {
            pool,
            client: explicit_kube_client(kubernetes)?,
        };
        let router = Router::new()
            .route("/internal/v1/kubevirt-vnc", get(upgrade))
            .with_state(state);
        Ok(Self {
            listener,
            router,
            tls,
        })
    }

    pub async fn serve(self) -> Result<(), KubeVirtConsoleExecutorServerError> {
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
    configuration: &KubeVirtConsoleKubernetesConfiguration,
) -> Result<Client, KubeVirtConsoleExecutorServerError> {
    if configuration.api_server.scheme() != "https"
        || configuration.api_server.host_str().is_none()
        || !configuration.bearer_token_file.is_absolute()
        || !configuration.cluster_ca_file.is_absolute()
        || configuration.request_timeout_milliseconds == 0
        || configuration.request_timeout_milliseconds > 30_000
    {
        return Err(KubeVirtConsoleExecutorServerError::Configuration);
    }
    let mut config = Config::new(
        configuration
            .api_server
            .as_str()
            .parse()
            .map_err(|_| KubeVirtConsoleExecutorServerError::Configuration)?,
    );
    let roots = rustls_pemfile::certs(&mut Cursor::new(std::fs::read(
        &configuration.cluster_ca_file,
    )?))
    .map(|certificate| certificate.map(|certificate| certificate.as_ref().to_vec()))
    .collect::<Result<Vec<_>, _>>()?;
    if roots.is_empty() {
        return Err(KubeVirtConsoleExecutorServerError::Configuration);
    }
    config.root_cert = Some(roots);
    config.auth_info.token_file = Some(
        configuration
            .bearer_token_file
            .to_str()
            .ok_or(KubeVirtConsoleExecutorServerError::Configuration)?
            .to_owned(),
    );
    let timeout = Duration::from_millis(configuration.request_timeout_milliseconds);
    config.connect_timeout = Some(timeout);
    config.read_timeout = Some(timeout);
    config.write_timeout = Some(timeout);
    Client::try_from(config).map_err(KubeVirtConsoleExecutorServerError::KubernetesClient)
}

async fn upgrade(
    State(state): State<ExecutorState>,
    caller: Option<Extension<VerifiedCallerIdentity>>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Result<Response, KubeVirtConsoleExecutorServerError> {
    if !caller.is_some_and(|Extension(identity)| identity.contains_san(ENVIRONMENT_SERVICE_SAN)) {
        return Err(KubeVirtConsoleExecutorServerError::CallerDenied);
    }
    let protocol = ConsoleKind::Novnc.websocket_subprotocol();
    if !headers
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.split(',').any(|item| item.trim() == protocol))
    {
        return Err(KubeVirtConsoleExecutorServerError::SubprotocolRequired);
    }
    Ok(upgrade
        .protocols([protocol])
        .max_frame_size(MAX_FRAME_BYTES)
        .max_message_size(MAX_FRAME_BYTES)
        .on_upgrade(move |socket| async move {
            if let Err(code) = execute(state, socket).await {
                tracing::warn!(
                    event = "environment.kubevirt_console.closed",
                    diagnostic = code
                );
            }
        }))
}

async fn execute(state: ExecutorState, mut browser: WebSocket) -> Result<(), &'static str> {
    let request = match browser.next().await {
        Some(Ok(Message::Text(value))) => {
            contracts::parse_strict_json::<VncRequest>(value.as_bytes())
                .map_err(|_| "LW_KUBEVIRT_CONSOLE_REQUEST_INVALID")?
        }
        _ => return Err("LW_KUBEVIRT_CONSOLE_REQUEST_REQUIRED"),
    };
    let target = resolve_target(&state.pool, &request).await?;
    validate_vmi(&state.client, &request, &target).await?;
    let upstream = open_vnc(&state.client, &target).await?;
    validate_vmi(&state.client, &request, &target).await?;
    relay(browser, upstream).await
}

async fn resolve_target(pool: &PgPool, request: &VncRequest) -> Result<VncTarget, &'static str> {
    let row = sqlx::query(
        "SELECT i.course_id,i.release_id,i.revision,i.generation,i.observed_generation,\
                i.desired_state,i.observed_state,(i.contract->>'releaseVersion')::bigint AS release_version,\
                o.state,o.environment_generation,o.namespace,o.virtual_machine_name,o.vmi_uid \
         FROM environment.environment_instances i \
         JOIN environment.kubevirt_runtime_observations o ON o.environment_id=i.environment_id \
         WHERE i.environment_id=$1",
    )
    .bind(request.environment_id.as_uuid())
    .fetch_optional(pool)
    .await
    .map_err(|_| "LW_KUBEVIRT_CONSOLE_AUTHORITY_UNAVAILABLE")?
    .ok_or("LW_KUBEVIRT_CONSOLE_TARGET_NOT_READY")?;
    let generation: i64 = row.get("generation");
    let expected_namespace = format!("lw-env-{}", request.environment_id);
    let namespace: String = row.get("namespace");
    let name: String = row.get("virtual_machine_name");
    let revision = i64::try_from(request.environment_revision.get())
        .map_err(|_| "LW_KUBEVIRT_CONSOLE_REQUEST_INVALID")?;
    let release_version = i64::try_from(request.release_version)
        .map_err(|_| "LW_KUBEVIRT_CONSOLE_REQUEST_INVALID")?;
    if row.get::<Uuid, _>("course_id") != request.course_id.as_uuid()
        || row.get::<Uuid, _>("release_id") != request.release_id.as_uuid()
        || row.get::<i64, _>("revision") != revision
        || row.get::<i64, _>("release_version") != release_version
        || row.get::<i64, _>("observed_generation") != generation
        || row.get::<String, _>("desired_state") != "running"
        || row.get::<String, _>("observed_state") != "ready"
        || row.get::<String, _>("state") != "running"
        || row.get::<i64, _>("environment_generation") != generation
        || namespace != expected_namespace
        || name != VMI_NAME
    {
        return Err("LW_KUBEVIRT_CONSOLE_TARGET_STALE");
    }
    let vmi_uid = row
        .get::<Option<Uuid>, _>("vmi_uid")
        .ok_or("LW_KUBEVIRT_CONSOLE_TARGET_NOT_READY")?;
    Ok(VncTarget {
        namespace,
        name,
        vmi_uid,
    })
}

async fn validate_vmi(
    client: &Client,
    request: &VncRequest,
    target: &VncTarget,
) -> Result<(), &'static str> {
    let path = format!(
        "/apis/kubevirt.io/v1/namespaces/{}/virtualmachineinstances/{}",
        target.namespace, target.name
    );
    let document: Value = client
        .request(
            Request::get(path)
                .body(Vec::new())
                .map_err(|_| "LW_KUBEVIRT_CONSOLE_REQUEST_INVALID")?,
        )
        .await
        .map_err(|_| "LW_KUBEVIRT_CONSOLE_TARGET_UNAVAILABLE")?;
    validate_vmi_document(&document, request, target)
}

fn validate_vmi_document(
    document: &Value,
    request: &VncRequest,
    target: &VncTarget,
) -> Result<(), &'static str> {
    let label = |name: &str| {
        document
            .pointer(&format!("/metadata/labels/{name}"))
            .and_then(Value::as_str)
    };
    let ready = document
        .pointer("/status/conditions")
        .and_then(Value::as_array)
        .is_some_and(|conditions| {
            conditions.iter().any(|condition| {
                condition.get("type").and_then(Value::as_str) == Some("Ready")
                    && condition.get("status").and_then(Value::as_str) == Some("True")
            })
        });
    let vmi_uid = target.vmi_uid.to_string();
    let environment_id = request.environment_id.to_string();
    let course_id = request.course_id.to_string();
    let release_id = request.release_id.to_string();
    let release_version = request.release_version.to_string();
    if document.pointer("/metadata/name").and_then(Value::as_str) != Some(target.name.as_str())
        || document
            .pointer("/metadata/namespace")
            .and_then(Value::as_str)
            != Some(target.namespace.as_str())
        || document.pointer("/metadata/uid").and_then(Value::as_str) != Some(vmi_uid.as_str())
        || document.pointer("/status/phase").and_then(Value::as_str) != Some("Running")
        || !ready
        || label("labweaver.io~1environment-id") != Some(environment_id.as_str())
        || label("labweaver.io~1course-id") != Some(course_id.as_str())
        || label("labweaver.io~1release-id") != Some(release_id.as_str())
        || label("labweaver.io~1release-version") != Some(release_version.as_str())
    {
        return Err("LW_KUBEVIRT_CONSOLE_TARGET_STALE");
    }
    Ok(())
}

async fn open_vnc(
    client: &Client,
    target: &VncTarget,
) -> Result<WebSocketStream<TokioIo<hyper::upgrade::Upgraded>>, &'static str> {
    let key = tungstenite::handshake::client::generate_key();
    let path = format!(
        "/apis/subresources.kubevirt.io/v1/namespaces/{}/virtualmachineinstances/{}/vnc",
        target.namespace, target.name
    );
    let request = Request::get(path)
        .header(header::CONNECTION, "Upgrade")
        .header(header::UPGRADE, "websocket")
        .header(header::SEC_WEBSOCKET_VERSION, "13")
        .header(header::SEC_WEBSOCKET_KEY, &key)
        .header(header::SEC_WEBSOCKET_PROTOCOL, KUBEVIRT_PLAIN_PROTOCOL)
        .body(Body::empty())
        .map_err(|_| "LW_KUBEVIRT_CONSOLE_REQUEST_INVALID")?;
    let response = client
        .send(request)
        .await
        .map_err(|_| "LW_KUBEVIRT_CONSOLE_UPSTREAM_UNAVAILABLE")?;
    if response.status() != StatusCode::SWITCHING_PROTOCOLS
        || response
            .headers()
            .get(header::UPGRADE)
            .and_then(|value| value.to_str().ok())
            .is_none_or(|value| !value.eq_ignore_ascii_case("websocket"))
        || response
            .headers()
            .get(header::CONNECTION)
            .and_then(|value| value.to_str().ok())
            .is_none_or(|value| !header_contains_token(value, "upgrade"))
        || response
            .headers()
            .get(header::SEC_WEBSOCKET_PROTOCOL)
            .and_then(|value| value.to_str().ok())
            != Some(KUBEVIRT_PLAIN_PROTOCOL)
        || response
            .headers()
            .get(header::SEC_WEBSOCKET_ACCEPT)
            .and_then(|value| value.to_str().ok())
            != Some(tungstenite::handshake::derive_accept_key(key.as_bytes()).as_str())
    {
        return Err("LW_KUBEVIRT_CONSOLE_UPSTREAM_PROTOCOL_INVALID");
    }
    let upgraded = hyper::upgrade::on(response)
        .await
        .map_err(|_| "LW_KUBEVIRT_CONSOLE_UPSTREAM_UNAVAILABLE")?;
    let configuration = tungstenite::protocol::WebSocketConfig::default()
        .max_frame_size(Some(MAX_FRAME_BYTES))
        .max_message_size(Some(MAX_FRAME_BYTES));
    Ok(
        WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Client, Some(configuration))
            .await,
    )
}

fn header_contains_token(value: &str, expected: &str) -> bool {
    value
        .split(',')
        .any(|token| token.trim().eq_ignore_ascii_case(expected))
}

async fn relay(
    browser: WebSocket,
    upstream: WebSocketStream<TokioIo<hyper::upgrade::Upgraded>>,
) -> Result<(), &'static str> {
    let (mut browser_tx, mut browser_rx) = browser.split();
    let (mut upstream_tx, mut upstream_rx) = upstream.split();
    loop {
        tokio::select! {
            message = browser_rx.next() => {
                let Some(message) = message else {
                    let _ = upstream_tx.send(UpstreamMessage::Close(None)).await;
                    return Ok(());
                };
                let forwarded = match message.map_err(|_| "LW_KUBEVIRT_CONSOLE_BROWSER_FAILED")? {
                    Message::Binary(value) if value.len() <= MAX_FRAME_BYTES => UpstreamMessage::Binary(value),
                    Message::Ping(value) => UpstreamMessage::Ping(value),
                    Message::Pong(value) => UpstreamMessage::Pong(value),
                    Message::Close(_) => {
                        let _ = upstream_tx.send(UpstreamMessage::Close(None)).await;
                        return Ok(());
                    },
                    Message::Text(_) | Message::Binary(_) => {
                        return Err("LW_KUBEVIRT_CONSOLE_FRAME_INVALID");
                    }
                };
                upstream_tx.send(forwarded).await.map_err(|_| "LW_KUBEVIRT_CONSOLE_UPSTREAM_FAILED")?;
            }
            message = upstream_rx.next() => {
                let Some(message) = message else {
                    let _ = browser_tx.send(Message::Close(None)).await;
                    return Ok(());
                };
                let forwarded = match message.map_err(|_| "LW_KUBEVIRT_CONSOLE_UPSTREAM_FAILED")? {
                    UpstreamMessage::Binary(value) if value.len() <= MAX_FRAME_BYTES => Message::Binary(value),
                    UpstreamMessage::Ping(value) => Message::Ping(value),
                    UpstreamMessage::Pong(value) => Message::Pong(value),
                    UpstreamMessage::Close(_) => {
                        let _ = browser_tx.send(Message::Close(None)).await;
                        return Ok(());
                    },
                    _ => return Err("LW_KUBEVIRT_CONSOLE_UPSTREAM_FRAME_INVALID"),
                };
                browser_tx.send(forwarded).await.map_err(|_| "LW_KUBEVIRT_CONSOLE_BROWSER_FAILED")?;
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum KubeVirtConsoleExecutorServerError {
    #[error("LW_KUBEVIRT_CONSOLE_CONFIGURATION_INVALID")]
    Configuration,
    #[error("LW_KUBEVIRT_CONSOLE_CALLER_DENIED")]
    CallerDenied,
    #[error("LW_KUBEVIRT_CONSOLE_SUBPROTOCOL_REQUIRED")]
    SubprotocolRequired,
    #[error("LW_KUBEVIRT_CONSOLE_KUBERNETES_CLIENT_FAILED")]
    KubernetesClient(#[source] kube::Error),
    #[error("LW_KUBEVIRT_CONSOLE_IO_FAILED")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Tls(#[from] crate::MtlsServerError),
}

impl IntoResponse for KubeVirtConsoleExecutorServerError {
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
    use serde_json::json;

    fn request() -> Result<VncRequest, Box<dyn std::error::Error>> {
        Ok(VncRequest {
            environment_id: EnvironmentId::new(),
            course_id: CourseId::new(),
            release_id: ReleaseId::new(),
            release_version: 7,
            environment_revision: Revision::new(4)?,
        })
    }

    fn target(request: &VncRequest) -> VncTarget {
        VncTarget {
            namespace: format!("lw-env-{}", request.environment_id),
            name: VMI_NAME.to_owned(),
            vmi_uid: Uuid::now_v7(),
        }
    }

    fn ready_vmi(request: &VncRequest, target: &VncTarget) -> Value {
        json!({
            "metadata": {
                "name": target.name,
                "namespace": target.namespace,
                "uid": target.vmi_uid,
                "labels": {
                    "labweaver.io/environment-id": request.environment_id,
                    "labweaver.io/course-id": request.course_id,
                    "labweaver.io/release-id": request.release_id,
                    "labweaver.io/release-version": request.release_version.to_string()
                }
            },
            "status": {
                "phase": "Running",
                "conditions": [{"type": "Ready", "status": "True"}]
            }
        })
    }

    #[test]
    fn executor_contract_fixes_the_vmi_name_and_plain_kubevirt_protocol() {
        assert_eq!(VMI_NAME, "runtime");
        assert_eq!(KUBEVIRT_PLAIN_PROTOCOL, "plain.kubevirt.io");
        assert_eq!(MAX_FRAME_BYTES, 64 * 1024);
        assert!(header_contains_token("keep-alive, Upgrade", "upgrade"));
    }

    #[test]
    fn exact_running_vmi_identity_is_required() -> Result<(), Box<dyn std::error::Error>> {
        let request = request()?;
        let target = target(&request);
        let mut document = ready_vmi(&request, &target);
        assert!(validate_vmi_document(&document, &request, &target).is_ok());
        document["metadata"]["uid"] = json!(Uuid::nil());
        assert_eq!(
            validate_vmi_document(&document, &request, &target),
            Err("LW_KUBEVIRT_CONSOLE_TARGET_STALE")
        );
        Ok(())
    }

    #[test]
    fn stopped_or_mislabeled_vmi_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let request = request()?;
        let target = target(&request);
        let mut document = ready_vmi(&request, &target);
        document["status"]["phase"] = json!("Succeeded");
        assert!(validate_vmi_document(&document, &request, &target).is_err());
        let mut document = ready_vmi(&request, &target);
        document["metadata"]["labels"]["labweaver.io/course-id"] = json!(Uuid::nil());
        assert!(validate_vmi_document(&document, &request, &target).is_err());
        Ok(())
    }
}
