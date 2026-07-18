//! OpenSSH authorization and fixed-session helper for the Sprint 2 gateway.

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use contracts::UtcTimestamp;
use contracts::access::{
    CloseGatewaySessionRequest, CreateGatewaySessionRequest, GatewaySession, GatewaySessionState,
    HeartbeatGatewaySessionRequest, SshAuthorization, SshAuthorizationRequest,
};
use reqwest::{Certificate, Client, Identity, StatusCode};
use serde::Serialize;
use sha2::{Digest, Sha256};
use ssh_key::{HashAlg, PublicKey};
use time::OffsetDateTime;
use tokio::process::Command;
use tracing::{error, info};
use uuid::Uuid;

const AUTHORIZE_PATH: &str = "/internal/v1/ssh/authorize";
const SESSION_PATH: &str = "/internal/v1/sessions";

#[derive(Debug, thiserror::Error)]
enum GatewayError {
    #[error("gateway configuration is incomplete")]
    Configuration,
    #[error("gateway input is invalid")]
    InvalidInput,
    #[error("access authority rejected or failed the request")]
    Authority,
    #[error("target session failed")]
    Target,
}

#[derive(Clone)]
struct GatewayConfig {
    access_url: String,
    gateway_identity: String,
    client: Client,
}

impl GatewayConfig {
    fn load() -> Result<Self, GatewayError> {
        let access_url = required_env("LABWEAVER_ACCESS_URL")?;
        let gateway_identity = required_env("LABWEAVER_GATEWAY_IDENTITY")?;
        let cert_path = PathBuf::from(required_env("LABWEAVER_MTLS_CERT")?);
        let key_path = PathBuf::from(required_env("LABWEAVER_MTLS_KEY")?);
        let ca_path = PathBuf::from(required_env("LABWEAVER_MTLS_CA")?);
        let mut identity_pem = std::fs::read(cert_path).map_err(|_| GatewayError::Configuration)?;
        identity_pem.extend(std::fs::read(key_path).map_err(|_| GatewayError::Configuration)?);
        let identity =
            Identity::from_pem(&identity_pem).map_err(|_| GatewayError::Configuration)?;
        let ca = Certificate::from_pem(
            &std::fs::read(ca_path).map_err(|_| GatewayError::Configuration)?,
        )
        .map_err(|_| GatewayError::Configuration)?;
        let client = Client::builder()
            .identity(identity)
            .add_root_certificate(ca)
            .https_only(true)
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(|_| GatewayError::Configuration)?;
        Ok(Self {
            access_url: access_url.trim_end_matches('/').to_owned(),
            gateway_identity,
            client,
        })
    }

    async fn post<T: Serialize>(
        &self,
        path: &str,
        body: &T,
        idempotency_key: Option<&str>,
        revision: Option<contracts::Revision>,
    ) -> Result<reqwest::Response, GatewayError> {
        let mut request = self
            .client
            .post(format!("{}{path}", self.access_url))
            .header("x-request-id", Uuid::now_v7().to_string())
            .json(body);
        if let Some(key) = idempotency_key {
            request = request.header("Idempotency-Key", key);
        }
        if let Some(revision) = revision {
            request = request.header("If-Match", format!("\"rev-{}\"", revision.get()));
        }
        request.send().await.map_err(|_| GatewayError::Authority)
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt().json().with_target(false).init();
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            error!(event = "LW_GATEWAY_COMMAND_FAILED", reason = %error);
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), GatewayError> {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("authorized-keys") => {
            let alias = args.next().ok_or(GatewayError::InvalidInput)?;
            let key = args.next().ok_or(GatewayError::InvalidInput)?;
            let connection_id = connection_id()?;
            authorized_keys(&GatewayConfig::load()?, &alias, &key, &connection_id).await
        }
        Some("force-command") => {
            let authorization_id = args.next().ok_or(GatewayError::InvalidInput)?;
            let token = args.next().ok_or(GatewayError::InvalidInput)?;
            let alias = args.next().ok_or(GatewayError::InvalidInput)?;
            let connection_id = args.next().ok_or(GatewayError::InvalidInput)?;
            force_command(
                &GatewayConfig::load()?,
                &authorization_id,
                &token,
                &alias,
                &connection_id,
            )
            .await
        }
        _ => Err(GatewayError::InvalidInput),
    }
}

async fn authorized_keys(
    config: &GatewayConfig,
    alias: &str,
    presented_key: &str,
    connection_id: &str,
) -> Result<(), GatewayError> {
    validate_alias(alias)?;
    validate_connection_id(connection_id)?;
    let key = PublicKey::from_openssh(presented_key).map_err(|_| GatewayError::InvalidInput)?;
    let request = SshAuthorizationRequest {
        alias: alias.to_owned(),
        presented_key_fingerprint_sha256: key.fingerprint(HashAlg::Sha256).to_string(),
        gateway_identity: config.gateway_identity.clone(),
        connection_id: connection_id.to_owned(),
        source_address_hash: source_address_hash()?,
        requested_at: now()?,
    };
    let response = config.post(AUTHORIZE_PATH, &request, None, None).await?;
    if response.status() != StatusCode::OK {
        return Err(GatewayError::Authority);
    }
    let authorization = response
        .json::<SshAuthorization>()
        .await
        .map_err(|_| GatewayError::Authority)?;
    if authorization.normalized_authorized_key
        != key.to_openssh().map_err(|_| GatewayError::InvalidInput)?
    {
        return Err(GatewayError::Authority);
    }
    println!(
        "no-agent-forwarding,no-port-forwarding,no-X11-forwarding,no-user-rc,command=\"/usr/local/bin/labweaver-gateway force-command {} {} {} {}\" {}",
        shell_token(&authorization.authorization_id)?,
        shell_token(&authorization.force_command_token)?,
        alias,
        connection_id,
        authorization.normalized_authorized_key
    );
    Ok(())
}

async fn force_command(
    config: &GatewayConfig,
    authorization_id: &str,
    token: &str,
    alias: &str,
    connection_id: &str,
) -> Result<(), GatewayError> {
    validate_alias(alias)?;
    validate_connection_id(connection_id)?;
    if env::var("SSH_ORIGINAL_COMMAND").is_ok_and(|command| !command.trim().is_empty()) {
        return Err(GatewayError::InvalidInput);
    }
    let request = CreateGatewaySessionRequest {
        authorization_id: authorization_id.to_owned(),
        force_command_token: token.to_owned(),
        gateway_identity: config.gateway_identity.clone(),
        connection_id: connection_id.to_owned(),
        opened_at: now()?,
    };
    let idempotency_key = format!("gateway-session-{connection_id}");
    let response = config
        .post(SESSION_PATH, &request, Some(&idempotency_key), None)
        .await?;
    if response.status() != StatusCode::CREATED {
        return Err(GatewayError::Authority);
    }
    let mut session = response
        .json::<GatewaySession>()
        .await
        .map_err(|_| GatewayError::Authority)?;
    let mut child = Command::new("/usr/bin/ssh")
        .args([
            "-F",
            "/etc/labweaver/target-ssh.conf",
            &format!("lab@{alias}"),
        ])
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| GatewayError::Target)?;
    let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
    let result = loop {
        tokio::select! {
            status = child.wait() => break status.map_err(|_| GatewayError::Target).and_then(|status| status.success().then_some(()).ok_or(GatewayError::Target)),
            _ = heartbeat.tick() => {
                let body = HeartbeatGatewaySessionRequest {
                    gateway_identity: config.gateway_identity.clone(),
                    connection_id: connection_id.to_owned(),
                    expected_revision: session.revision,
                    observed_at: now()?,
                };
                let response = config
                    .post(
                        &format!("{SESSION_PATH}/{}/heartbeat", session.id),
                        &body,
                        None,
                        Some(session.revision),
                    )
                    .await?;
                if response.status() != StatusCode::OK {
                    child.kill().await.map_err(|_| GatewayError::Target)?;
                    break Err(GatewayError::Authority);
                }
                session = response.json::<GatewaySession>().await.map_err(|_| GatewayError::Authority)?;
                if session.state != GatewaySessionState::Active {
                    child.kill().await.map_err(|_| GatewayError::Target)?;
                    break Err(GatewayError::Authority);
                }
            }
        }
    };
    close_session(config, &session, connection_id, result.is_ok()).await?;
    result
}

async fn close_session(
    config: &GatewayConfig,
    session: &GatewaySession,
    connection_id: &str,
    clean: bool,
) -> Result<(), GatewayError> {
    let body = CloseGatewaySessionRequest {
        gateway_identity: config.gateway_identity.clone(),
        connection_id: connection_id.to_owned(),
        expected_revision: session.revision,
        closed_at: now()?,
        reason_code: if clean {
            "client_closed"
        } else {
            "target_failed"
        }
        .to_owned(),
    };
    let response = config
        .post(
            &format!("{SESSION_PATH}/{}/close", session.id),
            &body,
            None,
            Some(session.revision),
        )
        .await?;
    if response.status().is_success() {
        info!(event = "LW_GATEWAY_SESSION_CLOSED", session_id = %session.id);
        Ok(())
    } else {
        Err(GatewayError::Authority)
    }
}

fn required_env(name: &str) -> Result<String, GatewayError> {
    env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or(GatewayError::Configuration)
}

fn connection_id() -> Result<String, GatewayError> {
    let connection = required_env("SSH_CONNECTION")?;
    let mut hasher = Sha256::new();
    hasher.update(connection.as_bytes());
    Ok(format!("ssh-{:x}", hasher.finalize()))
}

fn source_address_hash() -> Result<String, GatewayError> {
    let source = required_env("SSH_CONNECTION")?
        .split_whitespace()
        .next()
        .ok_or(GatewayError::InvalidInput)?
        .to_owned();
    let mut hasher = Sha256::new();
    hasher.update(source.as_bytes());
    Ok(format!("{:x}", hasher.finalize()))
}

fn validate_alias(value: &str) -> Result<(), GatewayError> {
    let valid = value.len() == 23
        && value.starts_with("lw-")
        && value
            .bytes()
            .skip(3)
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
    valid.then_some(()).ok_or(GatewayError::InvalidInput)
}

fn validate_connection_id(value: &str) -> Result<(), GatewayError> {
    (value.starts_with("ssh-")
        && value.len() == 68
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-'))
    .then_some(())
    .ok_or(GatewayError::InvalidInput)
}

fn shell_token(value: &str) -> Result<&str, GatewayError> {
    (!value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')))
    .then_some(value)
    .ok_or(GatewayError::InvalidInput)
}

fn now() -> Result<UtcTimestamp, GatewayError> {
    UtcTimestamp::from_utc(OffsetDateTime::now_utc()).map_err(|_| GatewayError::Configuration)
}
