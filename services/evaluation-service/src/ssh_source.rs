//! SSH Collector transport restricted to the SFTP subsystem.

use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use contracts::{Sha256Digest, UtcTimestamp};
use russh::client;
use russh::keys::{load_openssh_certificate, load_secret_key};
use russh_sftp::client::SftpSession;
use russh_sftp::client::error::Error as SftpError;
use russh_sftp::protocol::{FileAttributes, StatusCode};
use tokio::io::AsyncReadExt as _;

use crate::collector::{
    CollectError, SnapshotSource, SnapshotTransport, SourceEntry, SourceKind, SourceMetadata,
};

const MAX_CREDENTIAL_TTL_SECONDS: i64 = 300;

/// One short-lived, single-environment, certificate-authenticated SFTP binding.
#[derive(Clone)]
pub struct SshSnapshotConfig {
    /// Private runtime address; public targets are rejected.
    pub host: IpAddr,
    /// P0 SSH port, fixed to 22.
    pub port: u16,
    /// Locked non-root VM account.
    pub username: String,
    /// Absolute remote workspace root.
    pub workspace_root: String,
    /// Ephemeral private-key file mounted from a short-lived Secret.
    pub private_key_path: PathBuf,
    /// OpenSSH user certificate bound to the same key and environment.
    pub certificate_path: PathBuf,
    /// Exact observed SSH host-key blob digest from Environment Service.
    pub expected_host_key_sha256: Sha256Digest,
    /// Opaque Environment-owned source identity.
    pub source_identity: Sha256Digest,
    /// Credential expiry; must be no more than five minutes after connection time.
    pub expires_at: UtcTimestamp,
    /// TCP, key exchange, and authentication deadline.
    pub connect_timeout: Duration,
    /// Per-SFTP-operation deadline.
    pub operation_timeout: Duration,
}

impl std::fmt::Debug for SshSnapshotConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SshSnapshotConfig")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("workspace_root", &self.workspace_root)
            .field("expected_host_key_sha256", &self.expected_host_key_sha256)
            .field("source_identity", &self.source_identity)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

impl SshSnapshotConfig {
    fn validate(&self, now: UtcTimestamp) -> Result<(), CollectError> {
        let ttl = (self.expires_at.get() - now.get()).whole_seconds();
        if !private_ip(self.host)
            || self.port != 22
            || self.username.trim().is_empty()
            || self.username.len() > 64
            || !self
                .username
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            || !safe_remote_root(&self.workspace_root)
            || !safe_secret_path(&self.private_key_path)
            || !safe_secret_path(&self.certificate_path)
            || ttl <= 0
            || ttl > MAX_CREDENTIAL_TTL_SECONDS
            || self.connect_timeout.is_zero()
            || self.operation_timeout.is_zero()
        {
            return Err(CollectError::SshCredentialInvalid);
        }
        Ok(())
    }
}

/// Connected SFTP-only source. No shell, exec, PTY, forwarding, or write API is exposed.
pub struct SshSnapshotSource {
    config: SshSnapshotConfig,
    _session: client::Handle<HostKeyVerifier>,
    sftp: SftpSession,
}

impl std::fmt::Debug for SshSnapshotSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SshSnapshotSource")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl SshSnapshotSource {
    /// Connects, pins the host key, authenticates the short-lived certificate, and opens SFTP.
    ///
    /// # Errors
    ///
    /// Returns a stable credential, host-key, timeout, path, or source diagnostic.
    #[allow(
        clippy::too_many_lines,
        reason = "the SSH handshake keeps every security check before source construction"
    )]
    pub async fn connect(
        config: SshSnapshotConfig,
        now: UtcTimestamp,
    ) -> Result<Self, CollectError> {
        config.validate(now)?;
        let private_key = load_secret_key(&config.private_key_path, None)
            .map_err(|_| CollectError::SshCredentialInvalid)?;
        let certificate = load_openssh_certificate(&config.certificate_path)
            .map_err(|_| CollectError::SshCredentialInvalid)?;
        validate_certificate(&certificate, &config, now)?;
        let client_config = Arc::new(client::Config {
            inactivity_timeout: Some(config.operation_timeout),
            ..client::Config::default()
        });
        let verifier = HostKeyVerifier {
            expected: config.expected_host_key_sha256,
        };
        let address = (config.host, config.port);
        let mut session = match tokio::time::timeout(
            config.connect_timeout,
            client::connect(client_config, address, verifier),
        )
        .await
        {
            Err(_) => return Err(CollectError::SshTimeout),
            Ok(Err(russh::Error::UnknownKey)) => {
                return Err(CollectError::SshHostKeyMismatch);
            }
            Ok(Err(_)) => return Err(CollectError::SourceUnavailable),
            Ok(Ok(session)) => session,
        };
        let authentication = tokio::time::timeout(
            config.connect_timeout,
            session.authenticate_openssh_cert(
                config.username.clone(),
                Arc::new(private_key),
                certificate,
            ),
        )
        .await
        .map_err(|_| CollectError::SshTimeout)?
        .map_err(|_| CollectError::SourceUnavailable)?;
        if !authentication.success() {
            return Err(CollectError::SshCredentialInvalid);
        }
        let channel =
            tokio::time::timeout(config.operation_timeout, session.channel_open_session())
                .await
                .map_err(|_| CollectError::SshTimeout)?
                .map_err(|_| CollectError::SourceUnavailable)?;
        tokio::time::timeout(
            config.operation_timeout,
            channel.request_subsystem(true, "sftp"),
        )
        .await
        .map_err(|_| CollectError::SshTimeout)?
        .map_err(|_| CollectError::SourceUnavailable)?;
        let sftp = tokio::time::timeout(
            config.operation_timeout,
            SftpSession::new(channel.into_stream()),
        )
        .await
        .map_err(|_| CollectError::SshTimeout)?
        .map_err(|_| CollectError::SourceUnavailable)?;
        let canonical = tokio::time::timeout(
            config.operation_timeout,
            sftp.canonicalize(config.workspace_root.clone()),
        )
        .await
        .map_err(|_| CollectError::SshTimeout)?
        .map_err(|_| CollectError::SourceUnavailable)?;
        if canonical.trim_end_matches('/') != config.workspace_root.trim_end_matches('/') {
            return Err(CollectError::UnsafePath);
        }
        let root_metadata = tokio::time::timeout(
            config.operation_timeout,
            sftp.symlink_metadata(config.workspace_root.clone()),
        )
        .await
        .map_err(|_| CollectError::SshTimeout)?
        .map_err(|_| CollectError::SourceUnavailable)?;
        if !root_metadata.is_dir() || root_metadata.is_symlink() {
            return Err(CollectError::UnsafePath);
        }
        Ok(Self {
            config,
            _session: session,
            sftp,
        })
    }

    fn remote_path(&self, path: &str) -> Result<String, CollectError> {
        contracts::validate_relative_path(path).map_err(|_| CollectError::UnsafePath)?;
        Ok(format!(
            "{}/{}",
            self.config.workspace_root.trim_end_matches('/'),
            path
        ))
    }

    async fn remote_metadata(&self, path: &str) -> Result<Option<SourceMetadata>, CollectError> {
        let remote = self.remote_path(path)?;
        let result = tokio::time::timeout(
            self.config.operation_timeout,
            self.sftp.symlink_metadata(remote),
        )
        .await
        .map_err(|_| CollectError::SshTimeout)?;
        match result {
            Ok(metadata) => Ok(Some(sftp_metadata(&metadata))),
            Err(SftpError::Status(status)) if status.status_code == StatusCode::NoSuchFile => {
                Ok(None)
            }
            Err(SftpError::Timeout) => Err(CollectError::SshTimeout),
            Err(_) => Err(CollectError::SourceUnavailable),
        }
    }

    async fn validate_remote_path(&self, path: &str) -> Result<(), CollectError> {
        contracts::validate_relative_path(path).map_err(|_| CollectError::UnsafePath)?;
        let mut prefix = String::new();
        let components = path.split('/').collect::<Vec<_>>();
        for (index, component) in components.iter().enumerate() {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(component);
            let Some(metadata) = self.remote_metadata(&prefix).await? else {
                return Ok(());
            };
            if metadata.kind == SourceKind::Symlink {
                return Err(CollectError::SymlinkRejected);
            }
            if index + 1 != components.len() && metadata.kind != SourceKind::Directory {
                return Err(CollectError::UnsupportedEntry);
            }
        }
        let remote = self.remote_path(path)?;
        let canonical = tokio::time::timeout(
            self.config.operation_timeout,
            self.sftp.canonicalize(remote.clone()),
        )
        .await
        .map_err(|_| CollectError::SshTimeout)?
        .map_err(|error| map_sftp_error(&error))?;
        if canonical != remote {
            return Err(CollectError::SymlinkRejected);
        }
        Ok(())
    }
}

fn validate_certificate(
    certificate: &russh::keys::ssh_key::Certificate,
    config: &SshSnapshotConfig,
    now: UtcTimestamp,
) -> Result<(), CollectError> {
    let authority_seconds = u64::try_from(now.get().unix_timestamp())
        .map_err(|_| CollectError::SshCredentialInvalid)?;
    let configured_expiry = u64::try_from(config.expires_at.get().unix_timestamp())
        .map_err(|_| CollectError::SshCredentialInvalid)?;
    if certificate.cert_type() != russh::keys::ssh_key::certificate::CertType::User
        || certificate.valid_after() > authority_seconds
        || certificate.valid_before() <= authority_seconds
        || certificate.valid_before() > configured_expiry
        || certificate
            .valid_principals()
            .iter()
            .all(|principal| principal != "labweaver-collector")
        || certificate
            .critical_options()
            .get("force-command")
            .is_none_or(|command| command != "internal-sftp -R")
    {
        return Err(CollectError::SshCredentialInvalid);
    }
    Ok(())
}

#[async_trait]
impl SnapshotSource for SshSnapshotSource {
    fn transport(&self) -> SnapshotTransport {
        SnapshotTransport::Ssh
    }

    fn identity(&self) -> Sha256Digest {
        self.config.source_identity
    }

    async fn validate_path(&self, path: &str) -> Result<(), CollectError> {
        self.validate_remote_path(path).await
    }

    async fn metadata(&self, path: &str) -> Result<Option<SourceMetadata>, CollectError> {
        self.remote_metadata(path).await
    }

    async fn read_dir(&self, path: &str) -> Result<Vec<SourceEntry>, CollectError> {
        self.validate_remote_path(path).await?;
        let remote = self.remote_path(path)?;
        let entries =
            tokio::time::timeout(self.config.operation_timeout, self.sftp.read_dir(remote))
                .await
                .map_err(|_| CollectError::SshTimeout)?
                .map_err(|error| map_sftp_error(&error))?;
        let mut result = Vec::new();
        for entry in entries {
            let name = entry.file_name();
            if !safe_component(&name) {
                return Err(CollectError::UnsafePath);
            }
            result.push(SourceEntry {
                name,
                metadata: sftp_metadata(&entry.metadata()),
            });
        }
        result.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(result)
    }

    async fn read_file(&self, path: &str, max_bytes: u64) -> Result<Vec<u8>, CollectError> {
        self.validate_remote_path(path).await?;
        let before = self
            .remote_metadata(path)
            .await?
            .ok_or(CollectError::RequiredPathMissing)?;
        if before.kind == SourceKind::Symlink {
            return Err(CollectError::SymlinkRejected);
        }
        if before.kind != SourceKind::File {
            return Err(CollectError::UnsupportedEntry);
        }
        if before.size_bytes > max_bytes {
            return Err(CollectError::ByteLimitExceeded);
        }
        let remote = self.remote_path(path)?;
        let file = tokio::time::timeout(self.config.operation_timeout, self.sftp.open(remote))
            .await
            .map_err(|_| CollectError::SshTimeout)?
            .map_err(|error| map_sftp_error(&error))?;
        let mut bytes = Vec::new();
        tokio::time::timeout(
            self.config.operation_timeout,
            file.take(max_bytes.saturating_add(1))
                .read_to_end(&mut bytes),
        )
        .await
        .map_err(|_| CollectError::SshTimeout)?
        .map_err(|_| CollectError::SourceUnavailable)?;
        if u64::try_from(bytes.len()).map_err(|_| CollectError::ByteLimitExceeded)? > max_bytes {
            return Err(CollectError::ByteLimitExceeded);
        }
        let after = self
            .remote_metadata(path)
            .await?
            .ok_or(CollectError::SourceChanged)?;
        self.validate_remote_path(path).await?;
        if before != after || u64::try_from(bytes.len()).ok() != Some(after.size_bytes) {
            return Err(CollectError::SourceChanged);
        }
        Ok(bytes)
    }
}

#[derive(Clone, Copy, Debug)]
struct HostKeyVerifier {
    expected: Sha256Digest,
}

impl client::Handler for HostKeyVerifier {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        let observed = host_key_identity(server_public_key);
        let matches = observed == self.expected;
        if !matches {
            tracing::warn!(
                event = "evaluation.collector.ssh_host_key_mismatch",
                expected_host_key_sha256 = %self.expected,
                observed_host_key_sha256 = %observed,
            );
        }
        Ok(matches)
    }
}

fn host_key_identity(server_public_key: &russh::keys::ssh_key::PublicKey) -> Sha256Digest {
    let fingerprint = server_public_key
        .fingerprint(russh::keys::HashAlg::Sha256)
        .to_string();
    Sha256Digest::of_bytes(fingerprint.as_bytes())
}

fn sftp_metadata(metadata: &FileAttributes) -> SourceMetadata {
    let kind = if metadata.is_symlink() {
        SourceKind::Symlink
    } else if metadata.is_regular() {
        SourceKind::File
    } else if metadata.is_dir() {
        SourceKind::Directory
    } else {
        SourceKind::Other
    };
    SourceMetadata {
        kind,
        size_bytes: metadata.len(),
    }
}

fn map_sftp_error(error: &SftpError) -> CollectError {
    match error {
        SftpError::Timeout => CollectError::SshTimeout,
        _ => CollectError::SourceUnavailable,
    }
}

fn private_ip(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(value) => value.is_private() || value.is_loopback() || value.is_link_local(),
        IpAddr::V6(value) => {
            value.is_unique_local() || value.is_loopback() || value.is_unicast_link_local()
        }
    }
}

fn safe_remote_root(value: &str) -> bool {
    value.starts_with('/')
        && value.len() <= 1_024
        && value != "/"
        && !value.ends_with('/')
        && !value.contains("//")
        && value.split('/').skip(1).all(safe_component)
}

fn safe_secret_path(path: &std::path::Path) -> bool {
    let value = path.to_string_lossy();
    value.starts_with('/')
        && !value.contains("//")
        && !value.contains('\\')
        && value.split('/').skip(1).all(safe_component)
}

fn safe_component(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && !value.contains('/')
        && !value.contains('\\')
        && !value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::{
        SshSnapshotConfig, host_key_identity, private_ip, safe_remote_root, validate_certificate,
    };
    use contracts::{Sha256Digest, UtcTimestamp};
    use russh::keys::ssh_key::{PrivateKey, certificate, private::Ed25519Keypair};
    use std::net::{IpAddr, Ipv4Addr};
    use std::path::PathBuf;
    use std::time::Duration;

    fn config(expires_at: &str) -> Result<SshSnapshotConfig, Box<dyn std::error::Error>> {
        Ok(SshSnapshotConfig {
            host: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 8)),
            port: 22,
            username: "labweaver".to_owned(),
            workspace_root: "/home/labweaver/workspace".to_owned(),
            private_key_path: PathBuf::from("/run/secrets/collector/key"),
            certificate_path: PathBuf::from("/run/secrets/collector/key-cert.pub"),
            expected_host_key_sha256: Sha256Digest::of_bytes(b"host"),
            source_identity: Sha256Digest::of_bytes(b"source"),
            expires_at: expires_at.parse()?,
            connect_timeout: Duration::from_secs(5),
            operation_timeout: Duration::from_secs(5),
        })
    }

    #[test]
    fn ssh_binding_rejects_public_targets_escape_and_long_lived_credentials()
    -> Result<(), Box<dyn std::error::Error>> {
        let now: UtcTimestamp = "2026-07-18T08:00:00.000Z".parse()?;
        assert!(config("2026-07-18T08:04:59.000Z")?.validate(now).is_ok());
        assert!(config("2026-07-18T08:05:01.000Z")?.validate(now).is_err());
        let mut public = config("2026-07-18T08:04:59.000Z")?;
        public.host = "203.0.113.8".parse()?;
        assert!(public.validate(now).is_err());
        let mut escaped = config("2026-07-18T08:04:59.000Z")?;
        escaped.workspace_root = "/home/labweaver/../root".to_owned();
        assert!(escaped.validate(now).is_err());
        assert!(private_ip("fd00::8".parse()?));
        assert!(safe_remote_root("/srv/workspace"));
        Ok(())
    }

    #[test]
    fn host_key_identity_matches_the_runtime_executor_fingerprint_contract() {
        let key = PrivateKey::from(Ed25519Keypair::from_seed(&[7_u8; 32]));
        let public_key = key.public_key();
        let fingerprint = public_key
            .fingerprint(russh::keys::HashAlg::Sha256)
            .to_string();

        assert_eq!(
            host_key_identity(public_key),
            Sha256Digest::of_bytes(fingerprint.as_bytes())
        );
    }

    #[test]
    fn ssh_certificate_requires_collector_principal_read_only_command_and_bound_expiry()
    -> Result<(), Box<dyn std::error::Error>> {
        let now: UtcTimestamp = "2026-07-18T08:00:00.000Z".parse()?;
        let config = config("2026-07-18T08:04:59.000Z")?;
        let authority_seconds = u64::try_from(now.get().unix_timestamp())?;
        let valid = certificate(
            authority_seconds,
            authority_seconds + 299,
            "labweaver-collector",
            Some("internal-sftp -R"),
        )?;
        assert!(validate_certificate(&valid, &config, now).is_ok());

        let wrong_principal = certificate(
            authority_seconds,
            authority_seconds + 299,
            "labweaver-gateway",
            Some("internal-sftp -R"),
        )?;
        assert!(validate_certificate(&wrong_principal, &config, now).is_err());
        let shell_capable = certificate(
            authority_seconds,
            authority_seconds + 299,
            "labweaver-collector",
            None,
        )?;
        assert!(validate_certificate(&shell_capable, &config, now).is_err());
        let overlong = certificate(
            authority_seconds,
            authority_seconds + 300,
            "labweaver-collector",
            Some("internal-sftp -R"),
        )?;
        assert!(validate_certificate(&overlong, &config, now).is_err());
        Ok(())
    }

    fn certificate(
        valid_after: u64,
        valid_before: u64,
        principal: &str,
        force_command: Option<&str>,
    ) -> Result<russh::keys::ssh_key::Certificate, Box<dyn std::error::Error>> {
        let ca_key = PrivateKey::from(Ed25519Keypair::from_seed(&[0x11; 32]));
        let subject_key = PrivateKey::from(Ed25519Keypair::from_seed(&[0x22; 32]));
        let mut builder = certificate::Builder::new(
            vec![0x33; certificate::Builder::RECOMMENDED_NONCE_SIZE],
            subject_key.public_key(),
            valid_after,
            valid_before,
        )?;
        builder.cert_type(certificate::CertType::User)?;
        builder.valid_principal(principal)?;
        if let Some(command) = force_command {
            builder.critical_option("force-command", command)?;
        }
        Ok(builder.sign(&ca_key)?)
    }
}
