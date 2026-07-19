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
            let local_user = args.next().ok_or(GatewayError::InvalidInput)?;
            let key = args.next().ok_or(GatewayError::InvalidInput)?;
            let connection_id = connection_id()?;
            authorized_keys(&GatewayConfig::load()?, &local_user, &key, &connection_id).await
        }
        Some("force-command") => {
            let authorization_id = args.next().ok_or(GatewayError::InvalidInput)?;
            let token = args.next().ok_or(GatewayError::InvalidInput)?;
            let connection_id = args.next().ok_or(GatewayError::InvalidInput)?;
            if args.next().is_some() {
                return Err(GatewayError::InvalidInput);
            }
            force_command(
                &GatewayConfig::load()?,
                &authorization_id,
                &token,
                &connection_id,
            )
            .await
        }
        Some("known-host") => {
            let host = args.next().ok_or(GatewayError::InvalidInput)?;
            let fingerprint = args.next().ok_or(GatewayError::InvalidInput)?;
            let encoded_key = args.next().ok_or(GatewayError::InvalidInput)?;
            if args.next().is_some() {
                return Err(GatewayError::InvalidInput);
            }
            known_host(&host, &fingerprint, &encoded_key)
        }
        _ => Err(GatewayError::InvalidInput),
    }
}

async fn authorized_keys(
    config: &GatewayConfig,
    local_user: &str,
    presented_key: &str,
    connection_id: &str,
) -> Result<(), GatewayError> {
    if local_user != "gateway" {
        return Err(GatewayError::InvalidInput);
    }
    validate_connection_id(connection_id)?;
    let key = PublicKey::from_openssh(presented_key).map_err(|_| GatewayError::InvalidInput)?;
    let request = SshAuthorizationRequest {
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
        "restrict,command=\"/usr/local/bin/labweaver-gateway force-command {} {} {}\" {}",
        shell_token(&authorization.authorization_id)?,
        shell_token(&authorization.force_command_token)?,
        connection_id,
        authorization.normalized_authorized_key
    );
    Ok(())
}

async fn force_command(
    config: &GatewayConfig,
    authorization_id: &str,
    token: &str,
    connection_id: &str,
) -> Result<(), GatewayError> {
    validate_connection_id(connection_id)?;
    let original_command = required_env("SSH_ORIGINAL_COMMAND")?;
    let alias = parse_connect_command(&original_command)?;
    let request = CreateGatewaySessionRequest {
        authorization_id: authorization_id.to_owned(),
        force_command_token: token.to_owned(),
        alias: alias.to_owned(),
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
        .env("LABWEAVER_TARGET_ALIAS", &session.target_alias)
        .env(
            "LABWEAVER_TARGET_HOST_KEY_IDENTITY_SHA256",
            session.target_ssh_host_key_identity_sha256.to_string(),
        )
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

fn known_host(host: &str, fingerprint: &str, encoded_key: &str) -> Result<(), GatewayError> {
    let expected_host = required_env("LABWEAVER_TARGET_ALIAS")?;
    let expected_identity = required_env("LABWEAVER_TARGET_HOST_KEY_IDENTITY_SHA256")?;
    let line = verified_known_host_line(
        host,
        fingerprint,
        encoded_key,
        &expected_host,
        &expected_identity,
    )?;
    println!("{line}");
    Ok(())
}

fn verified_known_host_line(
    host: &str,
    fingerprint: &str,
    encoded_key: &str,
    expected_host: &str,
    expected_identity: &str,
) -> Result<String, GatewayError> {
    validate_alias(host)?;
    if expected_host != host {
        return Err(GatewayError::Authority);
    }
    let key = PublicKey::from_openssh(&format!("ssh-ed25519 {encoded_key}"))
        .map_err(|_| GatewayError::InvalidInput)?;
    let observed_fingerprint = key.fingerprint(HashAlg::Sha256).to_string();
    if observed_fingerprint != fingerprint {
        return Err(GatewayError::Authority);
    }
    let observed_identity = format!("{:x}", Sha256::digest(fingerprint.as_bytes()));
    if observed_identity != expected_identity {
        return Err(GatewayError::Authority);
    }
    Ok(format!("{host} ssh-ed25519 {encoded_key}"))
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
            .all(|byte| byte.is_ascii_lowercase() || matches!(byte, b'2'..=b'7'));
    valid.then_some(()).ok_or(GatewayError::InvalidInput)
}

fn parse_connect_command(value: &str) -> Result<&str, GatewayError> {
    let mut tokens = value.split_ascii_whitespace();
    if tokens.next() != Some("connect") {
        return Err(GatewayError::InvalidInput);
    }
    let alias = tokens.next().ok_or(GatewayError::InvalidInput)?;
    if tokens.next().is_some() {
        return Err(GatewayError::InvalidInput);
    }
    validate_alias(alias)?;
    Ok(alias)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_command_accepts_only_one_server_alias() {
        assert!(matches!(
            parse_connect_command("connect lw-abcdefghijklmnopqrst"),
            Ok("lw-abcdefghijklmnopqrst")
        ));
        for invalid in [
            "",
            "connect",
            "connect lw-abcdefghijklmnopqrst extra",
            "ssh lw-abcdefghijklmnopqrst",
            "connect lw-abcdefghijklmnopqrs;id",
            "scp file lw-abcdefghijklmnopqrst:/tmp",
        ] {
            assert!(parse_connect_command(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn known_host_requires_the_authoritative_alias_and_fingerprint_identity() {
        let key = PublicKey::from_openssh(
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFuGX5eSWJQm3kb+Jv4H0jHnI9I8FvkCcP9p3u3Cz5yz",
        )
        .expect("key");
        let fingerprint = key.fingerprint(HashAlg::Sha256).to_string();
        let identity = format!("{:x}", Sha256::digest(fingerprint.as_bytes()));
        assert!(
            verified_known_host_line(
                "lw-abcdefghijklmnopqrst",
                &fingerprint,
                "AAAAC3NzaC1lZDI1NTE5AAAAIFuGX5eSWJQm3kb+Jv4H0jHnI9I8FvkCcP9p3u3Cz5yz",
                "lw-abcdefghijklmnopqrst",
                &identity,
            )
            .is_ok()
        );
        assert!(
            verified_known_host_line(
                "lw-bbcdefghijklmnopqrst",
                &fingerprint,
                "AAAAC3NzaC1lZDI1NTE5AAAAIFuGX5eSWJQm3kb+Jv4H0jHnI9I8FvkCcP9p3u3Cz5yz",
                "lw-abcdefghijklmnopqrst",
                &identity,
            )
            .is_err()
        );
    }
}
