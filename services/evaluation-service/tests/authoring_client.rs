//! HTTPS admission-boundary behavior used by the submission-frozen consumer.

use std::{collections::BTreeSet, error::Error, io::Cursor, sync::Arc};

use auth::{ServiceTokenClient, ServiceTokenClientConfig, TransportSecurityMode};
use axum::{
    Router,
    body::Body,
    extract::State,
    http::{Method, Request, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};
use contracts::{
    ApprovalId, CourseId, EvaluationReleaseId, ProjectId, ReleaseId, Revision,
    http::{AuthoringPublicationAdmissionBinding, EnvironmentPublicationAdmissionQuery},
};
use evaluation_service::authoring_client::{
    AuthoringAdmissionClient, AuthoringAdmissionClientConfiguration, AuthoringAdmissionClientError,
};
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn::auto::Builder,
    service::TowerToHyperService,
};
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose,
};
use reqwest::{Certificate, Client, Url};
use rustls::{ServerConfig, pki_types::PrivateKeyDer};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::{
    net::TcpListener,
    sync::{Mutex, oneshot},
    task::JoinHandle,
};
use tokio_rustls::TlsAcceptor;

const MOCK_TOKEN: &str = "eyJhbGciOiJub25lIn0.eyJhdWQiOiJjb250cm9sIn0.sig";

#[derive(Clone)]
struct AdmissionResponse {
    status: StatusCode,
    body: Value,
}

struct AdmissionServer {
    base_url: Url,
    ca_pem: String,
    response: Arc<Mutex<AdmissionResponse>>,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}

struct AuthorityServer {
    issuer: String,
    task: JoinHandle<()>,
}

impl AdmissionServer {
    async fn start() -> Result<Self, Box<dyn Error>> {
        let ca = test_ca()?;
        let (certificate_pem, private_key_pem) = leaf_certificate(&ca)?;
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        let base_url = Url::parse(&format!("https://localhost:{port}/"))?;
        let response = Arc::new(Mutex::new(AdmissionResponse {
            status: StatusCode::OK,
            body: Value::Null,
        }));
        let router = Router::new()
            .fallback(any(admission_request))
            .with_state(response.clone());
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let tls = tls_config(&certificate_pem, &private_key_pem)?;
        let task = tokio::spawn(serve_tls(listener, router, tls, shutdown_rx));
        Ok(Self {
            base_url,
            ca_pem: ca.pem(),
            response,
            shutdown: Some(shutdown_tx),
            task: Some(task),
        })
    }

    async fn set_response(&self, status: StatusCode, body: Value) {
        let mut response = self.response.lock().await;
        response.status = status;
        response.body = body;
    }

    async fn stop(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl AuthorityServer {
    async fn start() -> Result<Self, Box<dyn Error>> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        let issuer = format!("http://localhost:{port}/realms/test");
        let discovery_issuer = issuer.clone();
        let router = Router::new()
            .route(
                "/realms/test/.well-known/openid-configuration",
                axum::routing::get(move || {
                    let issuer = discovery_issuer.clone();
                    async move {
                        axum::Json(json!({
                            "issuer": issuer,
                            "authorization_endpoint": format!("{issuer}/authorize"),
                            "token_endpoint": format!("{issuer}/token"),
                            "jwks_uri": format!("{issuer}/jwks"),
                            "response_types_supported": ["code"],
                            "subject_types_supported": ["public"],
                            "id_token_signing_alg_values_supported": ["ES256"],
                            "grant_types_supported": ["authorization_code", "client_credentials"]
                        }))
                    }
                }),
            )
            .route(
                "/realms/test/token",
                axum::routing::post(|| async {
                    axum::Json(json!({
                        "access_token": MOCK_TOKEN,
                        "token_type": "Bearer",
                        "expires_in": 300
                    }))
                }),
            )
            .route(
                "/realms/test/jwks",
                axum::routing::get(|| async { axum::Json(json!({ "keys": [] })) }),
            );
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Ok(Self { issuer, task })
    }
}

async fn admission_request(
    State(response): State<Arc<Mutex<AdmissionResponse>>>,
    request: Request<Body>,
) -> Response {
    let response = response.lock().await.clone();
    if request.method() != Method::GET
        || !request
            .uri()
            .path()
            .starts_with("/internal/v1/environment-releases/")
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    (response.status, axum::Json(response.body)).into_response()
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
) -> Result<Arc<ServerConfig>, Box<dyn Error>> {
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

async fn build_client(
    admission: &AdmissionServer,
    authority: &AuthorityServer,
    temp: &TempDir,
) -> Result<AuthoringAdmissionClient, Box<dyn Error>> {
    let ca = Certificate::from_pem(admission.ca_pem.as_bytes())?;
    let client = Client::builder()
        .no_proxy()
        .add_root_certificate(ca)
        .build()?;
    let token_http = Client::builder().no_proxy().build()?;
    let token_config = ServiceTokenClientConfig::new(
        &authority.issuer,
        "evaluation-test-client".to_owned(),
        "evaluation-test-secret".to_owned(),
        "control".to_owned(),
        BTreeSet::from([
            "control.authoring.read".to_owned(),
            "control.llm_policy.read".to_owned(),
        ]),
        1,
        TransportSecurityMode::InsecureTestOnly,
    )?;
    let token_client = Arc::new(ServiceTokenClient::discover(token_config, token_http).await?);
    let ca_file = temp.path().join("admission-ca.pem");
    std::fs::write(&ca_file, &admission.ca_pem)?;
    Ok(AuthoringAdmissionClient::new(
        AuthoringAdmissionClientConfiguration {
            base_uri: admission.base_url.clone(),
            ca_file,
            timeout_milliseconds: 2_000,
            max_request_bytes: 64 * 1024,
            max_response_bytes: 64 * 1024,
            audience: "control".to_owned(),
        },
        client,
        token_client,
        BTreeSet::from([
            "control.authoring.read".to_owned(),
            "control.llm_policy.read".to_owned(),
        ]),
    )?)
}

#[tokio::test]
async fn environment_admission_requires_exact_pair_preserves_work_none_and_retries_503()
-> Result<(), Box<dyn Error>> {
    let admission_server = AdmissionServer::start().await?;
    let authority_server = AuthorityServer::start().await?;
    let temp = TempDir::new()?;
    let client = build_client(&admission_server, &authority_server, &temp).await?;
    let project_id = ProjectId::new();
    let course_id = CourseId::new();
    let environment_release_id = ReleaseId::new();
    let evaluation_release_id = EvaluationReleaseId::new();
    let query = EnvironmentPublicationAdmissionQuery {
        project_id,
        course_id: Some(course_id),
        environment_release_version: 7,
    };
    let mut binding = AuthoringPublicationAdmissionBinding {
        approval_id: ApprovalId::new(),
        approval_revision: Revision::new(3)?,
        project_id,
        course_id: Some(course_id),
        environment_release_id,
        environment_release_version: 8,
        evaluation_release_id,
        evaluation_release_revision: Revision::new(4)?,
    };

    admission_server
        .set_response(StatusCode::OK, serde_json::to_value(&binding)?)
        .await;
    assert!(matches!(
        client
            .resolve_environment(environment_release_id, &query)
            .await,
        Err(AuthoringAdmissionClientError::ResponseInvalid)
    ));

    binding.environment_release_version = query.environment_release_version;
    admission_server
        .set_response(StatusCode::OK, serde_json::to_value(&binding)?)
        .await;
    let resolved = client
        .resolve_environment(environment_release_id, &query)
        .await?
        .ok_or("approved exact pair must be returned")?;
    assert_eq!(resolved, binding);

    admission_server
        .set_response(StatusCode::OK, Value::Null)
        .await;
    assert_eq!(
        client
            .resolve_environment(environment_release_id, &query)
            .await?,
        None,
        "Control's explicit null admission is the Work outcome"
    );

    admission_server
        .set_response(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({ "error": "temporarily unavailable" }),
        )
        .await;
    assert!(matches!(
        client
            .resolve_environment(environment_release_id, &query)
            .await,
        Err(AuthoringAdmissionClientError::Unavailable)
    ));

    admission_server.stop().await;
    authority_server.task.abort();
    Ok(())
}
