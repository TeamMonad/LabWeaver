//! OpenSSH authorization and fixed-session helper for the Sprint 2 gateway.

use std::env;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

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
use tracing::{error, info, warn};

const AUTHORIZE_PATH: &str = "/internal/v1/ssh/authorize";
const SESSION_PATH: &str = "/internal/v1/sessions";

#[derive(Debug, thiserror::Error)]
enum GatewayError {
    #[error("gateway configuration is incomplete")]
    Configuration,
    #[error("gateway input is invalid")]
    InvalidInput,
    #[error("gateway input is invalid at {0}")]
    InputStage(&'static str),
    #[error("access authority rejected or failed the request")]
    Authority,
    #[error("target session failed")]
    Target,
}

impl GatewayError {
    const fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::Configuration => "LW_GATEWAY_CONFIGURATION_INVALID",
            Self::InvalidInput | Self::InputStage(_) => "LW_GATEWAY_INPUT_INVALID",
            Self::Authority => "LW_GATEWAY_AUTHORITY_FAILED",
            Self::Target => "LW_GATEWAY_TARGET_SESSION_FAILED",
        }
    }

    const fn error_kind(&self) -> &'static str {
        match self {
            Self::Configuration => "configuration_invalid",
            Self::InvalidInput | Self::InputStage(_) => "input_rejected",
            Self::Authority => "access_authority_failed",
            Self::Target => "target_session_failed",
        }
    }

    const fn failure_stage(&self) -> &'static str {
        match self {
            Self::InputStage(stage) => stage,
            Self::Configuration => "gateway.configuration",
            Self::InvalidInput => "gateway.input",
            Self::Authority => "gateway.access_authority",
            Self::Target => "gateway.target_session",
        }
    }

    const fn retryable(&self) -> bool {
        matches!(self, Self::Authority | Self::Target)
    }
}

#[derive(Clone)]
struct GatewayConfig {
    access_url: String,
    gateway_identity: String,
    client: Client,
    context: telemetry::RequestContext,
}

impl GatewayConfig {
    fn load(context: telemetry::RequestContext) -> Result<Self, GatewayError> {
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
            context,
        })
    }

    async fn post<T: Serialize>(
        &self,
        path: &str,
        body: &T,
        idempotency_key: Option<&str>,
        revision: Option<contracts::Revision>,
    ) -> Result<reqwest::Response, GatewayError> {
        let started = Instant::now();
        let mut request = self
            .client
            .post(format!("{}{path}", self.access_url))
            .json(body);
        let mut headers = reqwest::header::HeaderMap::new();
        self.context
            .inject_headers(&mut headers)
            .map_err(|_| GatewayError::Configuration)?;
        request = request.headers(headers);
        if let Some(key) = idempotency_key {
            request = request.header("Idempotency-Key", key);
        }
        if let Some(revision) = revision {
            request = request.header("If-Match", format!("\"rev-{}\"", revision.get()));
        }
        let operation = if path == AUTHORIZE_PATH {
            "gateway.authorize"
        } else if path.ends_with("/heartbeat") {
            "gateway.session.heartbeat"
        } else if path.ends_with("/close") {
            "gateway.session.close"
        } else {
            "gateway.session.create"
        };
        let response = request.send().await.map_err(|_| GatewayError::Authority)?;
        let outcome = if response.status().is_success() {
            "succeeded"
        } else {
            "rejected"
        };
        info!(
            schema = telemetry::LOG_SCHEMA,
            event = "gateway.authority.completed",
            service = "access-gateway",
            component = "access-client",
            operation,
            outcome,
            duration_ms = elapsed_millis(started),
            request_id = self.context.request_id(),
            trace_id = self.context.trace_id(),
            http_status = response.status().as_u16(),
        );
        Ok(response)
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    if telemetry::init("access-gateway").is_err() {
        eprintln!(
            "{{\"timestamp_unix_ms\":0,\"level\":\"ERROR\",\"schema\":\"labweaver.log.v1\",\"event\":\"gateway.telemetry.failed\",\"service\":\"access-gateway\",\"component\":\"process\",\"operation\":\"telemetry.initialize\",\"outcome\":\"failed\",\"duration_ms\":0,\"diagnostic_code\":\"LW_TELEMETRY_INIT_FAILED\",\"error_kind\":\"telemetry_initialization_failed\",\"failure_stage\":\"gateway.telemetry.initialize\",\"retryable\":false,\"safe_detail\":\"redacted_unclassified\"}}"
        );
        return ExitCode::FAILURE;
    }
    let context = telemetry::RequestContext::generate();
    match run(&context).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let level_is_warn = error.retryable()
                || matches!(
                    error,
                    GatewayError::InvalidInput | GatewayError::InputStage(_)
                );
            if level_is_warn {
                warn!(
                    schema = telemetry::LOG_SCHEMA,
                    event = "gateway.command.failed",
                    service = "access-gateway",
                    component = "command",
                    operation = "gateway.command",
                    outcome = "failed",
                    duration_ms = 0_u64,
                    request_id = context.request_id(),
                    trace_id = context.trace_id(),
                    diagnostic_code = error.diagnostic_code(),
                    error_kind = error.error_kind(),
                    failure_stage = error.failure_stage(),
                    retryable = error.retryable(),
                    safe_detail = "redacted_unclassified",
                );
            } else {
                error!(
                    schema = telemetry::LOG_SCHEMA,
                    event = "gateway.command.failed",
                    service = "access-gateway",
                    component = "command",
                    operation = "gateway.command",
                    outcome = "failed",
                    duration_ms = 0_u64,
                    request_id = context.request_id(),
                    trace_id = context.trace_id(),
                    diagnostic_code = error.diagnostic_code(),
                    error_kind = error.error_kind(),
                    failure_stage = error.failure_stage(),
                    retryable = false,
                    safe_detail = "redacted_unclassified",
                );
            }
            ExitCode::FAILURE
        }
    }
}

async fn run(context: &telemetry::RequestContext) -> Result<(), GatewayError> {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("authorized-keys") => {
            let local_user = args
                .next()
                .ok_or(GatewayError::InputStage("authorized_keys.local_user"))?;
            let key = args
                .next()
                .ok_or(GatewayError::InputStage("authorized_keys.presented_key"))?;
            let connection = args
                .next()
                .ok_or(GatewayError::InputStage("authorized_keys.connection"))?;
            if args.next().is_some() {
                return Err(GatewayError::InputStage("authorized_keys.extra_argument"));
            }
            let connection_id = connection_id(&connection)
                .map_err(|_| GatewayError::InputStage("authorized_keys.connection"))?;
            let source_address = connection
                .split_ascii_whitespace()
                .next()
                .ok_or(GatewayError::InputStage("authorized_keys.source_address"))?;
            authorized_keys(
                &GatewayConfig::load(context.clone())?,
                &local_user,
                &key,
                &connection_id,
                source_address,
            )
            .await
        }
        Some("force-command") => {
            let authorization_id = args.next().ok_or(GatewayError::InvalidInput)?;
            let token = args.next().ok_or(GatewayError::InvalidInput)?;
            let connection_id = args.next().ok_or(GatewayError::InvalidInput)?;
            if args.next().is_some() {
                return Err(GatewayError::InvalidInput);
            }
            force_command(
                &GatewayConfig::load(context.clone())?,
                &authorization_id,
                &token,
                &connection_id,
            )
            .await
        }
        Some("known-host") => {
            let invocation = args.next().ok_or(GatewayError::InvalidInput)?;
            let host = args.next().ok_or(GatewayError::InvalidInput)?;
            let fingerprint = args.next().ok_or(GatewayError::InvalidInput)?;
            let encoded_key = args.next().ok_or(GatewayError::InvalidInput)?;
            if args.next().is_some() {
                return Err(GatewayError::InvalidInput);
            }
            known_host(&invocation, &host, &fingerprint, &encoded_key)
        }
        _ => Err(GatewayError::InvalidInput),
    }
}

async fn authorized_keys(
    config: &GatewayConfig,
    local_user: &str,
    presented_key: &str,
    connection_id: &str,
    source_address: &str,
) -> Result<(), GatewayError> {
    if local_user != "gateway" {
        return Err(GatewayError::InputStage("authorized_keys.local_user"));
    }
    validate_connection_id(connection_id)
        .map_err(|_| GatewayError::InputStage("authorized_keys.connection_id"))?;
    let key = PublicKey::from_openssh(presented_key)
        .map_err(|_| GatewayError::InputStage("authorized_keys.key_parse"))?;
    let request = SshAuthorizationRequest {
        presented_key_fingerprint_sha256: key.fingerprint(HashAlg::Sha256).to_string(),
        gateway_identity: config.gateway_identity.clone(),
        connection_id: connection_id.to_owned(),
        source_address_hash: source_address_hash(source_address)
            .map_err(|_| GatewayError::InputStage("authorized_keys.source_address"))?,
        requested_at: now().map_err(|_| GatewayError::InputStage("authorized_keys.timestamp"))?,
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
        != key
            .to_openssh()
            .map_err(|_| GatewayError::InputStage("authorized_keys.key_serialize"))?
    {
        return Err(GatewayError::Authority);
    }
    info!(
        schema = telemetry::LOG_SCHEMA,
        event = "gateway.authorization.succeeded",
        service = "access-gateway",
        component = "ssh-authorization",
        operation = "gateway.authorize",
        outcome = "succeeded",
        duration_ms = 0_u64,
        request_id = config.context.request_id(),
        trace_id = config.context.trace_id(),
        connection_id,
    );
    println!(
        "restrict,command=\"/usr/local/bin/labweaver-gateway-command force-command {} {} {}\" {}",
        shell_token(&authorization.authorization_id)
            .map_err(|_| GatewayError::InputStage("authorized_keys.authorization_id"))?,
        shell_token(&authorization.force_command_token)
            .map_err(|_| GatewayError::InputStage("authorized_keys.force_command_token"))?,
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
    info!(
        schema = telemetry::LOG_SCHEMA,
        event = "gateway.session.started",
        service = "access-gateway",
        component = "ssh-session",
        operation = "gateway.session",
        outcome = "started",
        duration_ms = 0_u64,
        request_id = config.context.request_id(),
        trace_id = config.context.trace_id(),
        session_id = %session.id,
        connection_id,
        revision = session.revision.get(),
    );
    let mut child = Command::new("/usr/bin/ssh")
        .args([
            "-F",
            "/etc/labweaver/target-ssh.conf",
            "-o",
            &format!("HostName={}", session.target_host),
            "-o",
            &format!("HostKeyAlias={alias}"),
            &format!("lab@{alias}"),
        ])
        .env("LABWEAVER_TARGET_ALIAS", &session.target_alias)
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

fn known_host(
    invocation: &str,
    host: &str,
    fingerprint: &str,
    encoded_key: &str,
) -> Result<(), GatewayError> {
    let expected_host = required_env("LABWEAVER_TARGET_ALIAS")?;
    if invocation == "ORDER" {
        validate_known_host_invocation(invocation, host, &expected_host)?;
        return Ok(());
    }
    if invocation == "HOSTNAME" {
        validate_known_host_invocation(invocation, host, &expected_host)?;
    } else if invocation == "ADDRESS" {
        let address = host
            .parse::<std::net::IpAddr>()
            .map_err(|_| GatewayError::InvalidInput)?;
        if !private_ip(address) {
            return Err(GatewayError::Authority);
        }
    } else {
        return Err(GatewayError::InvalidInput);
    }
    let line = if invocation == "HOSTNAME" {
        verified_known_host_line(host, fingerprint, encoded_key, &expected_host)?
    } else {
        verified_host_key_line(host, fingerprint, encoded_key)?
    };
    println!("{line}");
    Ok(())
}

fn validate_known_host_invocation(
    invocation: &str,
    host: &str,
    expected_host: &str,
) -> Result<(), GatewayError> {
    validate_alias(host)?;
    if expected_host != host {
        return Err(GatewayError::Authority);
    }
    matches!(invocation, "ORDER" | "HOSTNAME" | "ADDRESS")
        .then_some(())
        .ok_or(GatewayError::InvalidInput)
}

fn verified_known_host_line(
    host: &str,
    fingerprint: &str,
    encoded_key: &str,
    expected_host: &str,
) -> Result<String, GatewayError> {
    validate_alias(host)?;
    if expected_host != host {
        return Err(GatewayError::Authority);
    }
    verified_host_key_line(host, fingerprint, encoded_key)
}

fn verified_host_key_line(
    host: &str,
    fingerprint: &str,
    encoded_key: &str,
) -> Result<String, GatewayError> {
    let key = PublicKey::from_openssh(&format!("ssh-ed25519 {encoded_key}"))
        .map_err(|_| GatewayError::InvalidInput)?;
    let observed_fingerprint = key.fingerprint(HashAlg::Sha256).to_string();
    if observed_fingerprint != fingerprint {
        return Err(GatewayError::Authority);
    }
    Ok(format!("{host} ssh-ed25519 {encoded_key}"))
}

const fn private_ip(address: std::net::IpAddr) -> bool {
    match address {
        std::net::IpAddr::V4(address) => {
            address.is_private() && !address.is_unspecified() && !address.is_broadcast()
        }
        std::net::IpAddr::V6(address) => {
            (address.is_unique_local() || address.is_unicast_link_local())
                && !address.is_unspecified()
        }
    }
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
        info!(
            schema = telemetry::LOG_SCHEMA,
            event = "gateway.session.closed",
            service = "access-gateway",
            component = "ssh-session",
            operation = "gateway.session.close",
            outcome = "succeeded",
            duration_ms = 0_u64,
            request_id = config.context.request_id(),
            trace_id = config.context.trace_id(),
            session_id = %session.id,
            connection_id,
            revision = session.revision.get(),
        );
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

fn connection_id(connection: &str) -> Result<String, GatewayError> {
    if connection.trim().is_empty()
        || connection.len() > 512
        || connection.chars().any(char::is_control)
    {
        return Err(GatewayError::InvalidInput);
    }
    let mut hasher = Sha256::new();
    hasher.update(connection.as_bytes());
    Ok(format!("ssh-{:x}", hasher.finalize()))
}

fn source_address_hash(source: &str) -> Result<String, GatewayError> {
    source
        .parse::<std::net::IpAddr>()
        .map_err(|_| GatewayError::InvalidInput)?;
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
        && value.bytes().skip(4).all(|byte| byte.is_ascii_hexdigit()))
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
    let value = OffsetDateTime::now_utc();
    let normalized = value
        .replace_nanosecond((value.nanosecond() / 1_000_000) * 1_000_000)
        .map_err(|_| GatewayError::Configuration)?;
    UtcTimestamp::from_utc(normalized).map_err(|_| GatewayError::Configuration)
}

fn elapsed_millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
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
    fn connection_id_requires_the_fixed_prefix_and_sha256_hex() {
        let valid = format!("ssh-{}", "a5".repeat(32));
        assert!(validate_connection_id(&valid).is_ok());
        assert!(validate_connection_id(&format!("ssh-{}", "z5".repeat(32))).is_err());
        assert!(validate_connection_id(&format!("http-{}", "a5".repeat(32))).is_err());
        assert!(validate_connection_id("ssh-deadbeef").is_err());
        assert!(connection_id("10.20.0.1 52790 10.244.1.233 2222").is_ok());
        assert!(connection_id("").is_err());
        assert!(source_address_hash("10.20.0.1").is_ok());
        assert!(source_address_hash("not-an-ip").is_err());
    }

    #[test]
    fn gateway_timestamp_is_normalized_to_contract_milliseconds() -> Result<(), GatewayError> {
        let observed = now()?;
        assert_eq!(observed.get().nanosecond() % 1_000_000, 0);
        Ok(())
    }

    #[test]
    fn known_host_requires_the_authoritative_alias_and_fingerprint_identity()
    -> Result<(), ssh_key::Error> {
        let key = PublicKey::from_openssh(
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFuGX5eSWJQm3kb+Jv4H0jHnI9I8FvkCcP9p3u3Cz5yz",
        )?;
        let fingerprint = key.fingerprint(HashAlg::Sha256).to_string();
        assert!(
            verified_known_host_line(
                "lw-abcdefghijklmnopqrst",
                &fingerprint,
                "AAAAC3NzaC1lZDI1NTE5AAAAIFuGX5eSWJQm3kb+Jv4H0jHnI9I8FvkCcP9p3u3Cz5yz",
                "lw-abcdefghijklmnopqrst",
            )
            .is_ok()
        );
        assert!(
            verified_known_host_line(
                "lw-bbcdefghijklmnopqrst",
                &fingerprint,
                "AAAAC3NzaC1lZDI1NTE5AAAAIFuGX5eSWJQm3kb+Jv4H0jHnI9I8FvkCcP9p3u3Cz5yz",
                "lw-abcdefghijklmnopqrst",
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn known_host_order_probe_accepts_only_the_authoritative_alias() {
        let alias = "lw-abcdefghijklmnopqrst";
        assert!(validate_known_host_invocation("ORDER", alias, alias).is_ok());
        assert!(validate_known_host_invocation("ORDER", "lw-bbcdefghijklmnopqrst", alias).is_err());
        assert!(validate_known_host_invocation("UNKNOWN", alias, alias).is_err());
    }

    #[test]
    fn known_host_address_accepts_only_private_target_addresses() {
        assert!(private_ip(std::net::IpAddr::V4(std::net::Ipv4Addr::new(
            10, 101, 251, 15
        ))));
        assert!(private_ip(std::net::IpAddr::V6(std::net::Ipv6Addr::new(
            0xfd00, 0, 0, 0, 0, 0, 0, 0x15
        ))));
        assert!(!private_ip(std::net::IpAddr::V4(std::net::Ipv4Addr::new(
            8, 8, 8, 8
        ))));
        assert!(!private_ip(std::net::IpAddr::V6(
            std::net::Ipv6Addr::UNSPECIFIED
        )));
    }

    #[test]
    fn known_host_address_preserves_the_authoritative_key_identity() -> Result<(), ssh_key::Error> {
        let encoded_key = "AAAAC3NzaC1lZDI1NTE5AAAAIFuGX5eSWJQm3kb+Jv4H0jHnI9I8FvkCcP9p3u3Cz5yz";
        let key = PublicKey::from_openssh(&format!("ssh-ed25519 {encoded_key}"))?;
        let fingerprint = key.fingerprint(HashAlg::Sha256).to_string();
        let line = verified_host_key_line("10.101.251.15", &fingerprint, encoded_key);
        assert_eq!(
            line.as_deref().ok(),
            Some(format!("10.101.251.15 ssh-ed25519 {encoded_key}").as_str())
        );
        assert!(verified_host_key_line("10.101.251.15", "SHA256:wrong", encoded_key).is_err());
        Ok(())
    }
}
