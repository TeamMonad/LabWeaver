//! Service-token integration coverage against a disposable OIDC authority.
//!
//! The tests exercise the complete discovery, signature, registered-claim,
//! client-binding, permission, refresh, and private-CA paths without using a
//! fixture that bypasses the HTTP boundary.

use std::{collections::BTreeSet, io::Cursor, sync::Arc, time::Duration};

use auth::{
    ServiceAuthConfig, ServiceAuthError, ServiceIdentity, ServiceTokenClient,
    ServiceTokenClientConfig, ServiceTokenVerifier, TransportSecurityMode, no_redirect_http_client,
};
use axum::{
    Form, Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn::auto::Builder,
    service::TowerToHyperService,
};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose,
};
use rustls::{ServerConfig, pki_types::PrivateKeyDer};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{
    net::TcpListener,
    sync::{RwLock, oneshot},
};
use tokio_rustls::TlsAcceptor;

const AUDIENCE: &str = "labweaver-access";
const CLIENT_ID: &str = "access-gateway";
const CLIENT_SECRET: &str = "test-client-secret";
const REQUIRED_PERMISSION: &str = "access.read";

#[derive(Clone)]
struct AuthorityState {
    issuer: String,
    key: Arc<RwLock<Arc<SigningMaterial>>>,
}

struct SigningMaterial {
    kid: String,
    private_der: Vec<u8>,
    jwk: Value,
}

struct AuthorityHandle {
    issuer: String,
    state: AuthorityState,
    shutdown: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl AuthorityHandle {
    async fn rotate(&self, material: SigningMaterial) {
        *self.state.key.write().await = Arc::new(material);
    }
}

impl Drop for AuthorityHandle {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        self.task.abort();
    }
}

#[derive(Deserialize)]
struct TokenRequest {
    grant_type: Option<String>,
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn verifier_rejects_missing_forged_expired_wrong_binding_and_permission_tokens()
-> Result<(), Box<dyn std::error::Error>> {
    let key = signing_material("initial")?;
    let forged_key = signing_material("forged")?;
    let authority = spawn_authority(key).await?;
    let verifier = build_verifier(
        &authority.issuer,
        BTreeSet::from([REQUIRED_PERMISSION.to_owned()]),
    )
    .await?;

    assert_eq!(
        verifier.authenticate(&HeaderMap::new()).await,
        Err(ServiceAuthError::CredentialsMissing)
    );

    let valid = token(
        &authority.state,
        &authority.issuer,
        CLIENT_ID,
        AUDIENCE,
        [REQUIRED_PERMISSION],
        unix_now() + 300,
    )
    .await?;
    let identity = authenticate(&verifier, &valid).await?;
    assert_identity(&identity, &authority.issuer);

    let forged = signed_token(
        &forged_key,
        &authority.issuer,
        CLIENT_ID,
        AUDIENCE,
        [REQUIRED_PERMISSION],
        unix_now() + 300,
    )?;
    assert_eq!(
        authenticate(&verifier, &forged).await,
        Err(ServiceAuthError::TokenRejected)
    );

    let expired = token(
        &authority.state,
        &authority.issuer,
        CLIENT_ID,
        AUDIENCE,
        [REQUIRED_PERMISSION],
        unix_now() - 1,
    )
    .await?;
    assert_eq!(
        authenticate(&verifier, &expired).await,
        Err(ServiceAuthError::TokenRejected)
    );

    let current = authority.state.key.read().await.clone();
    let wrong_issuer = signed_token(
        &current,
        "https://other-issuer.invalid/realms/test",
        CLIENT_ID,
        AUDIENCE,
        [REQUIRED_PERMISSION],
        unix_now() + 300,
    )?;
    assert_eq!(
        authenticate(&verifier, &wrong_issuer).await,
        Err(ServiceAuthError::TokenRejected)
    );

    let wrong_audience = signed_token(
        &current,
        &authority.issuer,
        CLIENT_ID,
        "labweaver-control",
        [REQUIRED_PERMISSION],
        unix_now() + 300,
    )?;
    assert_eq!(
        authenticate(&verifier, &wrong_audience).await,
        Err(ServiceAuthError::TokenRejected)
    );

    let wrong_client = signed_token(
        &current,
        &authority.issuer,
        "unknown-service",
        AUDIENCE,
        [REQUIRED_PERMISSION],
        unix_now() + 300,
    )?;
    assert_eq!(
        authenticate(&verifier, &wrong_client).await,
        Err(ServiceAuthError::TokenRejected)
    );

    let valid_headers = headers(&valid)?;
    let permission_denied = verifier
        .authenticate_with_permission(&valid_headers, "access.write")
        .await;
    assert_eq!(permission_denied, Err(ServiceAuthError::PermissionDenied));

    let missing_permission = token(
        &authority.state,
        &authority.issuer,
        CLIENT_ID,
        AUDIENCE,
        ["access.write"],
        unix_now() + 300,
    )
    .await?;
    assert_eq!(
        authenticate(&verifier, &missing_permission).await,
        Err(ServiceAuthError::TokenRejected)
    );
    Ok(())
}

#[tokio::test]
async fn verifier_refreshes_jwks_when_keycloak_rotates_signing_key()
-> Result<(), Box<dyn std::error::Error>> {
    let initial = signing_material("initial")?;
    let rotated = signing_material("rotated")?;
    let authority = spawn_authority(initial).await?;
    let verifier = build_verifier(&authority.issuer, BTreeSet::new()).await?;

    let first = token(
        &authority.state,
        &authority.issuer,
        CLIENT_ID,
        AUDIENCE,
        [REQUIRED_PERMISSION],
        unix_now() + 300,
    )
    .await?;
    let first_identity = authenticate(&verifier, &first).await?;
    assert_eq!(first_identity.client_id, CLIENT_ID);

    let rotated_kid = rotated.kid.clone();
    authority.rotate(rotated).await;
    tokio::time::sleep(Duration::from_millis(1_100)).await;
    let second = token(
        &authority.state,
        &authority.issuer,
        CLIENT_ID,
        AUDIENCE,
        [REQUIRED_PERMISSION],
        unix_now() + 300,
    )
    .await?;
    assert_eq!(
        jsonwebtoken::decode_header(&second)?.kid.as_deref(),
        Some(rotated_kid.as_str())
    );
    let second_identity = authenticate(&verifier, &second).await?;
    assert_eq!(second_identity.subject, "service-account");
    Ok(())
}

#[tokio::test]
async fn client_discovers_and_exchanges_over_a_private_ca() -> Result<(), Box<dyn std::error::Error>>
{
    let key = signing_material("client")?;
    let ca = test_ca()?;
    let (server_certificate, server_key) = leaf_certificate(&ca)?;
    let authority = spawn_tls_authority(key, &server_certificate, &server_key).await?;
    let client_config = ServiceTokenClientConfig::new(
        &authority.issuer,
        CLIENT_ID.to_owned(),
        CLIENT_SECRET.to_owned(),
        AUDIENCE.to_owned(),
        BTreeSet::from([REQUIRED_PERMISSION.to_owned()]),
        30,
        TransportSecurityMode::Strict,
    )?;
    let http = no_redirect_http_client(Some(ca.pem().as_bytes()), TransportSecurityMode::Strict)?;
    let client = ServiceTokenClient::discover(client_config, http).await?;
    let token = client.access_token().await?;
    assert_eq!(
        jsonwebtoken::decode_header(&token)?.kid.as_deref(),
        Some("client")
    );
    Ok(())
}

async fn build_verifier(
    issuer: &str,
    required_permissions: BTreeSet<String>,
) -> Result<ServiceTokenVerifier, Box<dyn std::error::Error>> {
    let config = ServiceAuthConfig::new(
        issuer,
        AUDIENCE.to_owned(),
        BTreeSet::from([CLIENT_ID.to_owned()]),
        required_permissions,
        BTreeSet::from(["ES256".to_owned()]),
        3_600,
        1,
        TransportSecurityMode::InsecureTestOnly,
    )?;
    let http = no_redirect_http_client(None, TransportSecurityMode::InsecureTestOnly)?;
    Ok(ServiceTokenVerifier::discover(config, http).await?)
}

async fn authenticate(
    verifier: &ServiceTokenVerifier,
    token: &str,
) -> Result<ServiceIdentity, ServiceAuthError> {
    let headers = headers(token).map_err(|_| ServiceAuthError::CredentialsMissing)?;
    verifier.authenticate(&headers).await
}

fn headers(token: &str) -> Result<HeaderMap, http::header::InvalidHeaderValue> {
    let mut headers = HeaderMap::new();
    headers.insert(
        http::header::AUTHORIZATION,
        format!("Bearer {token}").parse()?,
    );
    Ok(headers)
}

fn assert_identity(identity: &ServiceIdentity, issuer: &str) {
    assert_eq!(identity.subject, "service-account");
    assert_eq!(identity.client_id, CLIENT_ID);
    assert_eq!(identity.issuer, issuer);
    assert!(identity.allows(REQUIRED_PERMISSION));
}

async fn token(
    state: &AuthorityState,
    issuer: &str,
    client_id: &str,
    audience: &str,
    permissions: [&str; 1],
    exp: i64,
) -> Result<String, Box<dyn std::error::Error>> {
    let material = state.key.read().await.clone();
    Ok(signed_token(
        &material,
        issuer,
        client_id,
        audience,
        permissions,
        exp,
    )?)
}

fn signed_token(
    material: &SigningMaterial,
    issuer: &str,
    client_id: &str,
    audience: &str,
    permissions: [&str; 1],
    exp: i64,
) -> Result<String, jsonwebtoken::errors::Error> {
    let mut header = Header::new(Algorithm::ES256);
    header.kid = Some(material.kid.clone());
    let claims = json!({
        "iss": issuer,
        "sub": "service-account",
        "azp": client_id,
        "aud": audience,
        "exp": exp,
        "nbf": unix_now() - 1,
        "resource_access": {
            audience: {"roles": permissions}
        }
    });
    encode(
        &header,
        &claims,
        &EncodingKey::from_ec_der(&material.private_der),
    )
}

fn unix_now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

fn signing_material(kid: &str) -> Result<SigningMaterial, Box<dyn std::error::Error>> {
    let key = KeyPair::generate()?;
    let public = key.public_key_raw();
    if public.len() != 65 || public[0] != 4 {
        return Err("test P-256 key did not use an uncompressed public point".into());
    }
    let encode_component = |component: &[u8]| URL_SAFE_NO_PAD.encode(component);
    Ok(SigningMaterial {
        kid: kid.to_owned(),
        private_der: key.serialize_der(),
        jwk: json!({
            "kty": "EC",
            "crv": "P-256",
            "x": encode_component(&public[1..33]),
            "y": encode_component(&public[33..65]),
            "use": "sig",
            "alg": "ES256",
            "kid": kid
        }),
    })
}

async fn spawn_authority(
    key: SigningMaterial,
) -> Result<AuthorityHandle, Box<dyn std::error::Error>> {
    spawn_authority_with_server(key, false, None).await
}

async fn spawn_tls_authority(
    key: SigningMaterial,
    certificate: &str,
    private_key: &str,
) -> Result<AuthorityHandle, Box<dyn std::error::Error>> {
    spawn_authority_with_server(
        key,
        true,
        Some((certificate.to_owned(), private_key.to_owned())),
    )
    .await
}

async fn spawn_authority_with_server(
    key: SigningMaterial,
    tls: bool,
    tls_material: Option<(String, String)>,
) -> Result<AuthorityHandle, Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let scheme = if tls { "https" } else { "http" };
    let issuer = format!("{scheme}://localhost:{}/realms/test", address.port());
    let state = AuthorityState {
        issuer: issuer.clone(),
        key: Arc::new(RwLock::new(Arc::new(key))),
    };
    let router = Router::new()
        .route(
            "/realms/test/.well-known/openid-configuration",
            get(discovery),
        )
        .route("/realms/test/jwks", get(jwks))
        .route("/realms/test/token", post(token_endpoint))
        .with_state(state.clone());
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let task = if tls {
        let (certificate, private_key) = tls_material.ok_or("TLS material missing")?;
        let config = tls_config(&certificate, &private_key)?;
        tokio::spawn(serve_tls(listener, router, config, shutdown_rx))
    } else {
        tokio::spawn(async move {
            tokio::select! {
                result = axum::serve(listener, router) => {
                    let _ = result;
                }
                _ = shutdown_rx => {}
            }
        })
    };
    Ok(AuthorityHandle {
        issuer,
        state,
        shutdown: Some(shutdown_tx),
        task,
    })
}

async fn discovery(State(state): State<AuthorityState>) -> Json<Value> {
    let issuer = state.issuer.clone();
    Json(json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{}/authorize", state.issuer),
        "token_endpoint": format!("{}/token", state.issuer),
        "jwks_uri": format!("{}/jwks", state.issuer),
        "response_types_supported": ["code"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["ES256"],
        "grant_types_supported": ["authorization_code", "client_credentials"]
    }))
}

async fn jwks(State(state): State<AuthorityState>) -> Json<Value> {
    let key = state.key.read().await;
    Json(json!({"keys": [key.jwk.clone()]}))
}

async fn token_endpoint(
    State(state): State<AuthorityState>,
    Form(request): Form<TokenRequest>,
) -> Result<Json<Value>, StatusCode> {
    if request.grant_type.as_deref() != Some("client_credentials") {
        return Err(StatusCode::BAD_REQUEST);
    }
    let material = state.key.read().await.clone();
    let token = signed_token(
        &material,
        &state.issuer,
        CLIENT_ID,
        AUDIENCE,
        [REQUIRED_PERMISSION],
        unix_now() + 300,
    )
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({
        "access_token": token,
        "token_type": "Bearer",
        "expires_in": 300
    })))
}

async fn serve_tls(
    listener: TcpListener,
    router: Router,
    config: Arc<ServerConfig>,
    mut shutdown: oneshot::Receiver<()>,
) {
    let acceptor = TlsAcceptor::from(config);
    loop {
        let accepted = tokio::select! {
            result = listener.accept() => result,
            _ = &mut shutdown => return,
        };
        let Ok((stream, _)) = accepted else {
            return;
        };
        let acceptor = acceptor.clone();
        let router = router.clone();
        tokio::spawn(async move {
            let Ok(stream) = acceptor.accept(stream).await else {
                return;
            };
            let service = TowerToHyperService::new(router);
            let connection = Builder::new(TokioExecutor::new())
                .serve_connection_with_upgrades(TokioIo::new(stream), service)
                .into_owned();
            let _ = connection.await;
        });
    }
}

fn tls_config(
    certificate_pem: &str,
    private_key_pem: &str,
) -> Result<Arc<ServerConfig>, Box<dyn std::error::Error>> {
    let certificates = rustls_pemfile::certs(&mut Cursor::new(certificate_pem.as_bytes()))
        .collect::<Result<Vec<_>, _>>()?;
    let key: PrivateKeyDer<'static> =
        rustls_pemfile::private_key(&mut Cursor::new(private_key_pem.as_bytes()))?
            .ok_or("private key missing")?;
    let mut config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certificates, key)?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

fn test_ca() -> Result<CertifiedIssuer<'static, KeyPair>, rcgen::Error> {
    let mut parameters = CertificateParams::new(Vec::<String>::new())?;
    parameters.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    parameters.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
    ];
    CertifiedIssuer::self_signed(parameters, KeyPair::generate()?)
}

fn leaf_certificate(
    ca: &CertifiedIssuer<'static, KeyPair>,
) -> Result<(String, String), rcgen::Error> {
    let mut parameters = CertificateParams::new(vec!["localhost".to_owned()])?;
    parameters.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    parameters.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let key = KeyPair::generate()?;
    let certificate = parameters.signed_by(&key, ca)?;
    Ok((certificate.pem(), key.serialize_pem()))
}
