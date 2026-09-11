//! TLS transport shared by the independently deployed HTTP services.

use std::{io::Cursor, path::Path, sync::Arc, time::Duration};

use axum::Router;
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn::auto::Builder,
    service::TowerToHyperService,
};
use rustls::{ServerConfig, pki_types::PrivateKeyDer};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// TLS server configuration and serving failures.
#[derive(Debug, thiserror::Error)]
pub enum HttpTransportError {
    /// The configured server certificate or key could not be read.
    #[error("LW_HTTP_TLS_MATERIAL_READ_FAILED")]
    MaterialRead(#[source] std::io::Error),
    /// The configured PEM material did not contain a usable certificate or key.
    #[error("LW_HTTP_TLS_MATERIAL_INVALID")]
    MaterialInvalid,
    /// Rustls rejected the server certificate and key combination.
    #[error("LW_HTTP_TLS_CONFIGURATION_INVALID")]
    Configuration(#[source] rustls::Error),
    /// The listener could not accept a new connection.
    #[error("LW_HTTP_TLS_ACCEPT_FAILED")]
    Accept(#[source] std::io::Error),
}

/// Loads a one-way TLS server configuration from deployment-owned PEM files.
///
/// Internal service identity is carried by a JWT.  This transport only
/// protects the token and request in transit; it does not derive an identity
/// from a client certificate or a request header.
///
/// # Errors
///
/// Returns [`HttpTransportError::MaterialRead`] when either file cannot be
/// read, or one of the configuration errors when the PEM material is invalid.
pub fn load_server_config(
    certificate_path: impl AsRef<Path>,
    private_key_path: impl AsRef<Path>,
) -> Result<Arc<ServerConfig>, HttpTransportError> {
    let certificate_pem =
        std::fs::read(certificate_path).map_err(HttpTransportError::MaterialRead)?;
    let private_key_pem =
        std::fs::read(private_key_path).map_err(HttpTransportError::MaterialRead)?;
    server_config(&certificate_pem, &private_key_pem)
}

/// Builds a one-way TLS server configuration from PEM material.
///
/// # Errors
///
/// Returns [`HttpTransportError::MaterialInvalid`] when the PEM input does not
/// contain a certificate and private key, or
/// [`HttpTransportError::Configuration`] when they cannot be used together.
pub fn server_config(
    certificate_pem: &[u8],
    private_key_pem: &[u8],
) -> Result<Arc<ServerConfig>, HttpTransportError> {
    let certificates = rustls_pemfile::certs(&mut Cursor::new(certificate_pem))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| HttpTransportError::MaterialInvalid)?;
    if certificates.is_empty() {
        return Err(HttpTransportError::MaterialInvalid);
    }
    let key: PrivateKeyDer<'static> =
        rustls_pemfile::private_key(&mut Cursor::new(private_key_pem))
            .map_err(|_| HttpTransportError::MaterialInvalid)?
            .ok_or(HttpTransportError::MaterialInvalid)?;
    let mut config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certificates, key)
        .map_err(HttpTransportError::Configuration)?;
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

/// Serves an Axum router over one-way TLS until the listener fails.
///
/// # Errors
///
/// Returns [`HttpTransportError::Accept`] when accepting a TCP connection
/// fails. Individual handshake and connection failures are logged and do not
/// stop the accept loop.
pub async fn serve_tls(
    listener: TcpListener,
    router: Router,
    config: Arc<ServerConfig>,
) -> Result<(), HttpTransportError> {
    let acceptor = TlsAcceptor::from(config);
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(connection) => connection,
            Err(error) => {
                tracing::error!(
                    schema = telemetry::LOG_SCHEMA,
                    event = "http.tls.accept_failed",
                    component = "http-transport",
                    operation = "tcp.accept",
                    outcome = "failed",
                    error_kind = "transport",
                    retryable = false,
                    error = %error,
                );
                return Err(HttpTransportError::Accept(error));
            }
        };
        let acceptor = acceptor.clone();
        let router = router.clone();
        tokio::spawn(async move {
            let stream =
                match tokio::time::timeout(TLS_HANDSHAKE_TIMEOUT, acceptor.accept(stream)).await {
                    Ok(Ok(stream)) => stream,
                    Ok(Err(error)) => {
                        tracing::warn!(
                            schema = telemetry::LOG_SCHEMA,
                            event = "http.tls.handshake_failed",
                            component = "http-transport",
                            operation = "tls.handshake",
                            outcome = "failed",
                            peer = %peer,
                            error = %error,
                        );
                        return;
                    }
                    Err(_) => {
                        tracing::warn!(
                            schema = telemetry::LOG_SCHEMA,
                            event = "http.tls.handshake_timeout",
                            component = "http-transport",
                            operation = "tls.handshake",
                            outcome = "timed_out",
                            peer = %peer,
                            timeout_seconds = TLS_HANDSHAKE_TIMEOUT.as_secs(),
                        );
                        return;
                    }
                };
            let io = TokioIo::new(stream);
            let service = TowerToHyperService::new(router);
            let connection = Builder::new(TokioExecutor::new())
                .serve_connection_with_upgrades(io, service)
                .into_owned();
            if let Err(error) = connection.await {
                tracing::warn!(
                    schema = telemetry::LOG_SCHEMA,
                    event = "http.tls.connection_failed",
                    component = "http-transport",
                    operation = "http.connection",
                    outcome = "failed",
                    peer = %peer,
                    error = %error,
                );
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::server_config;

    #[test]
    fn invalid_server_material_fails_closed() {
        assert!(server_config(b"not-a-certificate", b"not-a-key").is_err());
    }
}
