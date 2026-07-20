//! Internal mutual-TLS server configuration and peer-principal extraction.

use std::{collections::BTreeSet, fs::File, io::BufReader, sync::Arc};

use rustls::{
    RootCertStore, ServerConfig,
    crypto::CryptoProvider,
    pki_types::{CertificateDer, PrivateKeyDer},
    server::WebPkiClientVerifier,
};
use x509_parser::{
    extensions::GeneralName,
    prelude::{FromDer, X509Certificate},
};

use crate::config::MtlsFileConfig;

/// Validated mTLS listener material.
#[derive(Clone)]
pub struct MtlsServerConfig {
    /// Rustls server configuration which requires a trusted client certificate.
    pub server_config: Arc<ServerConfig>,
    /// Allowlisted service principal URIs extracted from leaf SANs.
    pub allowed_san_uris: BTreeSet<String>,
}

/// Reads server credentials and builds a fail-closed client-auth verifier.
pub fn load_mtls_server_config(config: &MtlsFileConfig) -> Result<MtlsServerConfig, MtlsError> {
    install_crypto_provider()?;
    let certificates = read_certificates(&config.server_certificate_file)?;
    let private_key = read_private_key(&config.server_key_file)?;
    let client_ca = read_certificates(&config.client_ca_file)?;
    let mut roots = RootCertStore::empty();
    for certificate in client_ca {
        roots.add(certificate).map_err(|_| MtlsError::ClientCa)?;
    }
    let verifier = WebPkiClientVerifier::builder(Arc::new(roots))
        .build()
        .map_err(|_| MtlsError::ClientCa)?;
    let mut server_config = ServerConfig::builder()
        .with_client_cert_verifier(verifier)
        .with_single_cert(certificates, private_key)
        .map_err(|_| MtlsError::ServerCredentials)?;
    server_config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(MtlsServerConfig {
        server_config: Arc::new(server_config),
        allowed_san_uris: config.allowed_san_uris.clone(),
    })
}

fn install_crypto_provider() -> Result<(), MtlsError> {
    if CryptoProvider::get_default().is_none()
        && rustls::crypto::ring::default_provider()
            .install_default()
            .is_err()
        && CryptoProvider::get_default().is_none()
    {
        return Err(MtlsError::CryptoProvider);
    }
    Ok(())
}

/// Extracts exactly one allowlisted URI SAN from a Rustls-verified client leaf.
pub fn extract_mtls_principal(
    leaf: &CertificateDer<'_>,
    allowed_san_uris: &BTreeSet<String>,
) -> Result<String, MtlsError> {
    let (_, certificate) =
        X509Certificate::from_der(leaf.as_ref()).map_err(|_| MtlsError::PeerCertificate)?;
    let sans = certificate
        .subject_alternative_name()
        .map_err(|_| MtlsError::PeerCertificate)?
        .ok_or(MtlsError::PeerSanMissing)?;
    let principals = sans
        .value
        .general_names
        .iter()
        .filter_map(|name| match name {
            GeneralName::URI(uri) => {
                let uri = *uri;
                allowed_san_uris.contains(uri).then(|| uri.to_owned())
            }
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    if principals.len() != 1 {
        return Err(MtlsError::PeerSanDenied);
    }
    principals
        .into_iter()
        .next()
        .ok_or(MtlsError::PeerSanDenied)
}

fn read_certificates(path: &str) -> Result<Vec<CertificateDer<'static>>, MtlsError> {
    let file = File::open(path).map_err(|_| MtlsError::ServerCredentials)?;
    rustls_pemfile::certs(&mut BufReader::new(file))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| MtlsError::ServerCredentials)
}

fn read_private_key(path: &str) -> Result<PrivateKeyDer<'static>, MtlsError> {
    let file = File::open(path).map_err(|_| MtlsError::ServerCredentials)?;
    rustls_pemfile::private_key(&mut BufReader::new(file))
        .map_err(|_| MtlsError::ServerCredentials)?
        .ok_or(MtlsError::ServerCredentials)
}

/// mTLS failures never authenticate a caller.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum MtlsError {
    /// The process could not select the configured Rustls crypto provider.
    #[error("LW_AUTH_STARTUP_FAILED")]
    CryptoProvider,
    /// Server certificate or private key could not be loaded safely.
    #[error("LW_AUTH_CONFIG_BINDING_MISSING")]
    ServerCredentials,
    /// Client CA was malformed or unusable.
    #[error("LW_AUTH_CONFIG_BINDING_MISSING")]
    ClientCa,
    /// Peer certificate was malformed after the TLS handshake.
    #[error("LW_AUTH_SERVICE_IDENTITY_DENIED")]
    PeerCertificate,
    /// Peer certificate had no URI SAN.
    #[error("LW_AUTH_SERVICE_IDENTITY_DENIED")]
    PeerSanMissing,
    /// Peer certificate did not present exactly one allowlisted URI SAN.
    #[error("LW_AUTH_SERVICE_IDENTITY_DENIED")]
    PeerSanDenied,
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, fs};

    use rcgen::{
        BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
        KeyUsagePurpose, SanType, generate_simple_self_signed,
    };
    use tempfile::tempdir;

    use crate::config::MtlsFileConfig;

    use super::{extract_mtls_principal, load_mtls_server_config};

    #[test]
    fn loads_ca_bound_mtls_material_and_accepts_only_allowlisted_uri_san()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempdir()?;
        let rcgen::CertifiedKey {
            cert: server_certificate,
            signing_key: server_key,
        } = generate_simple_self_signed(vec!["localhost".to_owned()])?;
        let server_certificate_file = directory.path().join("server.pem");
        let server_key_file = directory.path().join("server.key");
        fs::write(&server_certificate_file, server_certificate.pem())?;
        fs::write(&server_key_file, server_key.serialize_pem())?;

        let mut ca_params = CertificateParams::new(Vec::new())?;
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::DigitalSignature,
        ];
        let ca_key = KeyPair::generate()?;
        let ca_certificate = ca_params.self_signed(&ca_key)?;
        let issuer = Issuer::new(ca_params, ca_key);
        let client_ca_file = directory.path().join("client-ca.pem");
        fs::write(&client_ca_file, ca_certificate.pem())?;

        let allowed_uri = "spiffe://labweaver/gateway";
        let client_leaf = client_certificate(allowed_uri, &issuer)?;
        let config = MtlsFileConfig {
            bind_addr: "127.0.0.1:9443".to_owned(),
            server_certificate_file: server_certificate_file.to_string_lossy().into_owned(),
            server_key_file: server_key_file.to_string_lossy().into_owned(),
            client_ca_file: client_ca_file.to_string_lossy().into_owned(),
            allowed_san_uris: BTreeSet::from([allowed_uri.to_owned()]),
            required_eku: "clientAuth".to_owned(),
        };
        let loaded = load_mtls_server_config(&config)?;
        assert_eq!(
            extract_mtls_principal(client_leaf.der(), &loaded.allowed_san_uris)?,
            allowed_uri
        );

        let wrong_certificate = client_certificate("spiffe://labweaver/other", &issuer)?;
        assert!(extract_mtls_principal(wrong_certificate.der(), &loaded.allowed_san_uris).is_err());
        Ok(())
    }

    fn client_certificate(
        uri: &str,
        issuer: &Issuer<'static, KeyPair>,
    ) -> Result<rcgen::Certificate, rcgen::Error> {
        let mut params = CertificateParams::new(Vec::new())?;
        params.subject_alt_names = vec![SanType::URI(uri.try_into()?)];
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        let key = KeyPair::generate()?;
        params.signed_by(&key, issuer)
    }
}
