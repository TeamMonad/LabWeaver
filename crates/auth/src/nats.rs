//! Shared NATS mTLS connection primitive.
//!
//! All services should route their mTLS NATS connections through this function instead of
//! maintaining local ConnectOptions chains.

use std::path::PathBuf;
use std::time::Duration;

/// Connects to a NATS server with mandatory TLS, private CA, client certificate, and credentials.
pub async fn connect_mtls(
    server: &str,
    ca_path: PathBuf,
    certificate_path: PathBuf,
    key_path: PathBuf,
    credentials_path: PathBuf,
) -> Result<async_nats::Client, NatsMtlsError> {
    validate_inputs(
        server,
        &ca_path,
        &certificate_path,
        &key_path,
        &credentials_path,
    )?;

    let options = async_nats::ConnectOptions::new()
        .require_tls(true)
        .add_root_certificates(ca_path)
        .add_client_certificate(certificate_path, key_path);

    // credentials_file is an async method that returns Result<ConnectOptions, io::Error>
    let options = options
        .credentials_file(credentials_path)
        .await
        .map_err(|e| NatsMtlsError::Credentials(e.to_string()))?;

    options
        .connect(server)
        .await
        .map_err(|e| NatsMtlsError::Connect(e.to_string()))
}

/// Like [`connect_mtls`] but includes a connection timeout.
pub async fn connect_mtls_with_timeout(
    server: &str,
    ca_path: PathBuf,
    certificate_path: PathBuf,
    key_path: PathBuf,
    credentials_path: PathBuf,
    timeout: Duration,
) -> Result<async_nats::Client, NatsMtlsError> {
    validate_inputs(
        server,
        &ca_path,
        &certificate_path,
        &key_path,
        &credentials_path,
    )?;

    let options = async_nats::ConnectOptions::new()
        .require_tls(true)
        .add_root_certificates(ca_path)
        .add_client_certificate(certificate_path, key_path);

    // credentials_file is an async method that returns Result<ConnectOptions, io::Error>
    let options = options
        .credentials_file(credentials_path)
        .await
        .map_err(|e| NatsMtlsError::Credentials(e.to_string()))?;

    // Apply timeout and connect
    tokio::time::timeout(timeout, options.connect(server))
        .await
        .map_err(|_| NatsMtlsError::Connect("connection timed out".to_owned()))?
        .map_err(|e| NatsMtlsError::Connect(e.to_string()))
}

/// Shared connection failure.
#[derive(Debug, thiserror::Error)]
pub enum NatsMtlsError {
    /// Server or file paths are missing or invalid.
    #[error("NATS configuration is invalid")]
    Configuration,

    /// TLS credential setup failed.
    #[error("NATS TLS/credentials setup failed: {0}")]
    Credentials(String),

    /// Could not establish NATS connection.
    #[error("NATS connection refused: {0}")]
    Connect(String),
}

/// Validates server address and file paths before building a connection.
fn validate_inputs(
    server: &str,
    ca_path: &std::path::Path,
    certificate_path: &std::path::Path,
    key_path: &std::path::Path,
    credentials_path: &std::path::Path,
) -> Result<(), NatsMtlsError> {
    if server.trim().is_empty()
        || [ca_path, certificate_path, key_path, credentials_path]
            .iter()
            .any(|path| path.as_os_str().is_empty())
    {
        return Err(NatsMtlsError::Configuration);
    }
    Ok(())
}
