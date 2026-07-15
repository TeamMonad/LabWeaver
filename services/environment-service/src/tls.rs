use std::future::Future;
use std::io::{BufReader, Cursor};
use std::sync::Arc;

use axum::{Extension, Router};
use hyper::server::conn::http1;
use hyper_util::rt::TokioIo;
use hyper_util::service::TowerToHyperService;
use rustls::server::WebPkiClientVerifier;
use rustls::{RootCertStore, ServerConfig};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio::time::{Duration, timeout};
use tokio_rustls::TlsAcceptor;
use x509_parser::extensions::GeneralName;
use x509_parser::prelude::{FromDer, X509Certificate};

use crate::resolver::VerifiedCallerIdentity;

const MAX_IN_FLIGHT_CONNECTIONS: usize = 128;
const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const HTTP_CONNECTION_TIMEOUT: Duration = Duration::from_secs(30);
const SHUTDOWN_DRAIN_TIMEOUT: Duration = Duration::from_secs(10);

/// Fully parsed TLS server configuration that requires a CA-verified client certificate.
#[derive(Clone)]
pub struct MtlsConfig {
    server: Arc<ServerConfig>,
}

impl MtlsConfig {
    /// Parses a trusted client CA bundle, server certificate chain, and server private key.
    pub fn from_pem(
        client_ca_pem: &[u8],
        server_certificate_pem: &[u8],
        server_private_key_pem: &[u8],
    ) -> Result<Self, MtlsServerError> {
        let mut ca_reader = BufReader::new(Cursor::new(client_ca_pem));
        let ca_certificates = rustls_pemfile::certs(&mut ca_reader)
            .collect::<Result<Vec<_>, _>>()
            .map_err(MtlsServerError::CertificateRead)?;
        let mut roots = RootCertStore::empty();
        let (accepted, rejected) = roots.add_parsable_certificates(ca_certificates);
        if accepted == 0 || rejected != 0 {
            return Err(MtlsServerError::ClientCaInvalid);
        }

        let mut certificate_reader = BufReader::new(Cursor::new(server_certificate_pem));
        let certificate_chain = rustls_pemfile::certs(&mut certificate_reader)
            .collect::<Result<Vec<_>, _>>()
            .map_err(MtlsServerError::CertificateRead)?;
        if certificate_chain.is_empty() {
            return Err(MtlsServerError::ServerCertificateMissing);
        }
        let mut key_reader = BufReader::new(Cursor::new(server_private_key_pem));
        let private_key = rustls_pemfile::private_key(&mut key_reader)
            .map_err(MtlsServerError::CertificateRead)?
            .ok_or(MtlsServerError::ServerPrivateKeyMissing)?;

        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let verifier =
            WebPkiClientVerifier::builder_with_provider(Arc::new(roots), Arc::clone(&provider))
                .build()
                .map_err(|error| MtlsServerError::ClientVerifier(error.to_string()))?;
        let server = ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|error| MtlsServerError::TlsConfiguration(error.to_string()))?
            .with_client_cert_verifier(verifier)
            .with_single_cert(certificate_chain, private_key)
            .map_err(|error| MtlsServerError::TlsConfiguration(error.to_string()))?;
        Ok(Self {
            server: Arc::new(server),
        })
    }
}

/// Serves the owner resolver over TLS and injects only CA-verified certificate SAN identities.
#[allow(
    clippy::too_many_lines,
    reason = "the accept loop keeps connection bounds, authentication, serving, reaping, and drain behavior visible together"
)]
pub async fn serve_owner_resolver_mtls<F>(
    listener: TcpListener,
    router: Router,
    config: MtlsConfig,
    shutdown: F,
) -> Result<(), MtlsServerError>
where
    F: Future<Output = ()> + Send,
{
    let acceptor = TlsAcceptor::from(config.server);
    let mut connections = JoinSet::new();
    let connection_permits = Arc::new(Semaphore::new(MAX_IN_FLIGHT_CONNECTIONS));
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            () = &mut shutdown => break,
            completed = connections.join_next(), if !connections.is_empty() => {
                if let Some(Err(error)) = completed {
                    tracing::error!(
                        event = "environment.owner_resolver.connection_task_failed",
                        diagnostic_code = "LW_ENV_OWNER_CONNECTION_FAILED",
                        error = %error,
                    );
                }
            }
            accepted = listener.accept() => {
                let (stream, peer_address) = accepted.map_err(MtlsServerError::Accept)?;
                let Ok(connection_permit) = Arc::clone(&connection_permits).try_acquire_owned() else {
                    tracing::warn!(
                        event = "environment.owner_resolver.connection_rejected",
                        %peer_address,
                        diagnostic_code = "LW_ENV_OWNER_CONNECTION_LIMIT_EXCEEDED",
                    );
                    continue;
                };
                let acceptor = acceptor.clone();
                let router = router.clone();
                connections.spawn(async move {
                    let _connection_permit = connection_permit;
                    let tls_stream = match timeout(TLS_HANDSHAKE_TIMEOUT, acceptor.accept(stream)).await {
                        Ok(Ok(stream)) => stream,
                        Err(_) => {
                            tracing::warn!(
                                event = "environment.owner_resolver.tls_timeout",
                                %peer_address,
                                diagnostic_code = "LW_ENV_OWNER_TLS_HANDSHAKE_TIMEOUT",
                            );
                            return;
                        }
                        Ok(Err(error)) => {
                            tracing::warn!(
                                event = "environment.owner_resolver.tls_rejected",
                                %peer_address,
                                diagnostic_code = "LW_ENV_OWNER_CALLER_UNTRUSTED",
                                error = %error,
                            );
                            return;
                        }
                    };
                    let identity = match verified_identity(&tls_stream) {
                        Ok(identity) => identity,
                        Err(error) => {
                            tracing::warn!(
                                event = "environment.owner_resolver.san_rejected",
                                %peer_address,
                                diagnostic_code = "LW_ENV_OWNER_CALLER_UNTRUSTED",
                                error = %error,
                            );
                            return;
                        }
                    };
                    let service = TowerToHyperService::new(router.layer(Extension(identity)));
                    match timeout(
                        HTTP_CONNECTION_TIMEOUT,
                        http1::Builder::new()
                            .serve_connection(TokioIo::new(tls_stream), service),
                    )
                    .await
                    {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => {
                            tracing::warn!(
                                event = "environment.owner_resolver.connection_failed",
                                %peer_address,
                                diagnostic_code = "LW_ENV_OWNER_CONNECTION_FAILED",
                                error = %error,
                            );
                        }
                        Err(_) => {
                            tracing::warn!(
                                event = "environment.owner_resolver.connection_timeout",
                                %peer_address,
                                diagnostic_code = "LW_ENV_OWNER_CONNECTION_TIMEOUT",
                            );
                        }
                    }
                });
            }
        }
    }
    if timeout(SHUTDOWN_DRAIN_TIMEOUT, async {
        while connections.join_next().await.is_some() {}
    })
    .await
    .is_err()
    {
        tracing::warn!(
            event = "environment.owner_resolver.shutdown_drain_timeout",
            diagnostic_code = "LW_ENV_OWNER_SHUTDOWN_DRAIN_TIMEOUT",
        );
        connections.abort_all();
        while connections.join_next().await.is_some() {}
    }
    Ok(())
}

fn verified_identity(
    stream: &tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
) -> Result<VerifiedCallerIdentity, MtlsServerError> {
    let certificates = stream
        .get_ref()
        .1
        .peer_certificates()
        .ok_or(MtlsServerError::PeerCertificateMissing)?;
    let leaf = certificates
        .first()
        .ok_or(MtlsServerError::PeerCertificateMissing)?;
    let (_, certificate) = X509Certificate::from_der(leaf.as_ref())
        .map_err(|_| MtlsServerError::PeerCertificateInvalid)?;
    let alternative_names = certificate
        .subject_alternative_name()
        .map_err(|_| MtlsServerError::PeerCertificateInvalid)?
        .ok_or(MtlsServerError::PeerSanMissing)?;
    let sans = alternative_names
        .value
        .general_names
        .iter()
        .filter_map(|name| match name {
            GeneralName::DNSName(value) => Some((*value).to_owned()),
            _ => None,
        });
    VerifiedCallerIdentity::from_mtls_peer_sans(sans).map_err(|_| MtlsServerError::PeerSanInvalid)
}

/// Stable mTLS setup and serving failures. Certificate contents are never included.
#[derive(Debug, thiserror::Error)]
pub enum MtlsServerError {
    #[error("LW_ENV_OWNER_TLS_CERTIFICATE_READ_FAILED")]
    CertificateRead(#[source] std::io::Error),
    #[error("LW_ENV_OWNER_TLS_CLIENT_CA_INVALID")]
    ClientCaInvalid,
    #[error("LW_ENV_OWNER_TLS_SERVER_CERTIFICATE_MISSING")]
    ServerCertificateMissing,
    #[error("LW_ENV_OWNER_TLS_SERVER_KEY_MISSING")]
    ServerPrivateKeyMissing,
    #[error("LW_ENV_OWNER_TLS_CLIENT_VERIFIER_INVALID")]
    ClientVerifier(String),
    #[error("LW_ENV_OWNER_TLS_CONFIGURATION_INVALID")]
    TlsConfiguration(String),
    #[error("LW_ENV_OWNER_TLS_ACCEPT_FAILED")]
    Accept(#[source] std::io::Error),
    #[error("LW_ENV_OWNER_TLS_PEER_CERTIFICATE_MISSING")]
    PeerCertificateMissing,
    #[error("LW_ENV_OWNER_TLS_PEER_CERTIFICATE_INVALID")]
    PeerCertificateInvalid,
    #[error("LW_ENV_OWNER_TLS_PEER_SAN_MISSING")]
    PeerSanMissing,
    #[error("LW_ENV_OWNER_TLS_PEER_SAN_INVALID")]
    PeerSanInvalid,
}
