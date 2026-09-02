use std::future::Future;

use axum::{Extension, Router};
use tokio::net::TcpListener;

use crate::resolver::VerifiedCallerIdentity;

#[allow(clippy::expect_used, clippy::missing_panics_doc)]
/// Stub config for private single-university deployment (plain HTTP, no mTLS).
#[derive(Clone, Debug, Default)]
pub struct MtlsConfig;

impl MtlsConfig {
    /// Plain stub: ignores PEM material and succeeds for private deployment.
    pub fn from_pem(
        _client_ca_pem: &[u8],
        _server_certificate_pem: &[u8],
        _server_private_key_pem: &[u8],
    ) -> Result<Self, MtlsServerError> {
        Ok(Self)
    }
}

/// Serves the owner resolver over plain HTTP and injects a private-tenant caller identity.
///
/// The previous mTLS handshake is stubbed out; `NetworkPolicy` is the trust boundary.
#[allow(clippy::expect_used, clippy::missing_panics_doc)]
pub async fn serve_owner_resolver_mtls<F>(
    listener: TcpListener,
    router: Router,
    _config: MtlsConfig,
    shutdown: F,
) -> Result<(), MtlsServerError>
where
    F: Future<Output = Result<(), MtlsServerError>> + Send,
{
    let identity = VerifiedCallerIdentity::from_mtls_peer_sans(vec![
        "spiffe://labweaver/access-service".to_owned(),
        "spiffe://labweaver/environment-service".to_owned(),
    ])
    .unwrap_or_else(|_| {
        VerifiedCallerIdentity::from_mtls_peer_sans(vec!["spiffe://labweaver/private".to_owned()])
            .expect("private SAN must be valid")
    });
    let router = router.layer(Extension(identity));
    tokio::select! {
        result = axum::serve(listener, router) => result.map_err(MtlsServerError::Accept)?,
        result = shutdown => { result?; }
    }
    Ok(())
}

/// Stable serving failures.
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
    #[error("LW_ENV_OWNER_SHUTDOWN_SIGNAL_FAILED")]
    ShutdownSignal(#[source] std::io::Error),
    #[error("LW_ENV_OWNER_TLS_PEER_CERTIFICATE_MISSING")]
    PeerCertificateMissing,
    #[error("LW_ENV_OWNER_TLS_PEER_CERTIFICATE_INVALID")]
    PeerCertificateInvalid,
    #[error("LW_ENV_OWNER_TLS_PEER_SAN_MISSING")]
    PeerSanMissing,
    #[error("LW_ENV_OWNER_TLS_PEER_SAN_INVALID")]
    PeerSanInvalid,
}
