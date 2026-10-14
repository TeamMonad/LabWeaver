//! Synthetic issuer and TLS authorization boundary shared only by configuration tests.
#![allow(
    clippy::missing_errors_doc,
    clippy::too_many_lines,
    reason = "test fixtures keep transport setup explicit"
)]
use artifact_store::{ImmutableObjectStore, ObjectStoreError, PresignedUpload, VerifiedObject};
use async_trait::async_trait;
use auth::{
    ServiceAuthConfig, ServiceTokenClient, ServiceTokenClientConfig, ServiceTokenVerifier,
    TransportSecurityMode, no_redirect_http_client,
};
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use contracts::supply_chain::BuildNetworkPolicy;
use contracts::{
    ActorId, ArtifactRef, AuthenticatedActor, AuthorizationDecision, AuthorizationDecisionRequest,
    PlatformRole, PolicyId, Revision, UtcTimestamp,
};
use control_service::api::{ApiState, authenticated_router};
use control_service::clients::{
    AccessClient, AgentClient, EnvironmentClient, EvaluationClient, ServiceHttpClientConfig,
};
use control_service::{
    ContainerBuildPolicy, ControlConfig, ControlService, EvaluationRuntimePolicy,
};
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn::auto::Builder,
    service::TowerToHyperService,
};
use persistence_sqlx::Sha256Digest;
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose,
};
use rustls::{ServerConfig, pki_types::PrivateKeyDer};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::{collections::BTreeSet, io::Cursor, sync::Arc};
use tokio::{net::TcpListener, sync::oneshot};
use tokio_rustls::TlsAcceptor;

const SERVICE_TOKEN: &str = "eyJhbGciOiJub25lIn0.eyJhdWQiOlsibGFid2VhdmVyLWFjY2VzcyIsImxhYndlYXZlci1hZ2VudCIsImxhYndlYXZlci1lbnZpcm9ubWVudCIsImxhYndlYXZlci1ldmFsdWF0aW9uIl19.signature";
#[derive(Deserialize)]
struct TokenRequest {
    grant_type: Option<String>,
}
struct AuthorityHandle {
    issuer: String,
    shutdown: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for AuthorityHandle {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        self.task.abort();
    }
}
struct TlsServiceHandle {
    base_url: reqwest::Url,
    shutdown: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for TlsServiceHandle {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        self.task.abort();
    }
}

pub(super) struct HttpFixture {
    pub router: Router,
    pub admin: ActorId,
    pub other_actor: ActorId,
    issuer: AuthorityHandle,
    _access: TlsServiceHandle,
    _temporary: tempfile::TempDir,
    key: String,
}
impl HttpFixture {
    pub async fn new(pool: PgPool) -> Result<Self, Box<dyn std::error::Error>> {
        let admin = ActorId::new();
        let other_actor = ActorId::new();
        let (ca, leaf, key, jwk) = tls_material()?;
        let temporary = tempfile::tempdir()?;
        let ca_path = temporary.path().join("synthetic-ca.pem");
        std::fs::write(&ca_path, ca)?;
        let issuer = spawn_authority(jwk).await?;
        let access = spawn_tls_service(
            Router::new()
                .route("/internal/v1/auth/decision", post(access_decision))
                .with_state(admin),
            &leaf,
            &key,
        )
        .await?;
        let tokens = Arc::new(service_token_client(&issuer.issuer).await?);
        let endpoint = service_config(&access.base_url, &ca_path);
        let verifier = Arc::new(
            ServiceTokenVerifier::discover(
                ServiceAuthConfig::new(
                    &issuer.issuer,
                    "labweaver-control".to_owned(),
                    BTreeSet::from([
                        "labweaver-access".to_owned(),
                        "synthetic-executor".to_owned(),
                    ]),
                    BTreeSet::new(),
                    BTreeSet::from(["ES256".to_owned()]),
                    3600,
                    1,
                    TransportSecurityMode::InsecureTestOnly,
                )?,
                no_redirect_http_client(None, TransportSecurityMode::InsecureTestOnly)?,
            )
            .await?,
        );
        let state = Arc::new(ApiState {
            control: ControlService::new(pool, Arc::new(NoObjects), config()?)?,
            access: AccessClient::new_authenticated(endpoint.clone(), Arc::clone(&tokens))?,
            agent: AgentClient::new_authenticated(endpoint.clone(), Arc::clone(&tokens))?,
            environment: EnvironmentClient::new_authenticated(
                endpoint.clone(),
                Arc::clone(&tokens),
            )?,
            evaluation: EvaluationClient::new_authenticated(endpoint, tokens)?,
            service_token_verifier: Arc::clone(&verifier),
        });
        let router = authenticated_router(&state, &verifier);
        Ok(Self {
            router,
            admin,
            other_actor,
            issuer,
            _access: access,
            _temporary: temporary,
            key,
        })
    }
    pub fn token(
        &self,
        client: &str,
        permission: &str,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::ES256);
        header.kid = Some("test".to_owned());
        Ok(jsonwebtoken::encode(
            &header,
            &json!({"iss":self.issuer.issuer,"aud":["labweaver-control"],"exp":time::OffsetDateTime::now_utc().unix_timestamp()+300,"iat":time::OffsetDateTime::now_utc().unix_timestamp(),"azp":client,"sub":format!("service-account-{client}"),"resource_access":{"labweaver-control":{"roles":[permission]}}}),
            &jsonwebtoken::EncodingKey::from_ec_pem(self.key.as_bytes())?,
        )?)
    }
}
async fn access_decision(
    State(admin): State<ActorId>,
    Json(request): Json<AuthorizationDecisionRequest>,
) -> Result<Json<AuthorizationDecision>, StatusCode> {
    let valid_until = "2099-01-01T00:00:00.000Z"
        .parse()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(AuthorizationDecision {
        actor: AuthenticatedActor {
            actor_id: request.actor_id,
            roles: vec![if request.actor_id == admin {
                PlatformRole::PlatformAdmin
            } else {
                PlatformRole::Teacher
            }],
            expires_at: valid_until,
        },
        scope: request.scope,
        authorization_revision: Revision::new(1).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        scope_revision: Revision::new(1).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        valid_until,
        diagnostic_code: None,
    }))
}
struct NoObjects;
#[async_trait]
impl ImmutableObjectStore for NoObjects {
    async fn delete_orphan(&self, _: &str, _: &str) -> Result<(), ObjectStoreError> {
        Err(ObjectStoreError::ObjectUnavailable)
    }
    fn binding(&self) -> &'static str {
        "unused-fixture"
    }
    async fn presign_upload(
        &self,
        _: &str,
        _: u64,
        _: &str,
        _: UtcTimestamp,
    ) -> Result<PresignedUpload, ObjectStoreError> {
        Err(ObjectStoreError::ObjectUnavailable)
    }
    async fn read_verified(
        &self,
        _: &str,
        _: &ArtifactRef,
    ) -> Result<VerifiedObject, ObjectStoreError> {
        Err(ObjectStoreError::ObjectUnavailable)
    }
    async fn freeze_current(
        &self,
        _: &str,
        _: u64,
        _: &str,
    ) -> Result<VerifiedObject, ObjectStoreError> {
        Err(ObjectStoreError::ObjectUnavailable)
    }
}

fn config() -> Result<ControlConfig, Box<dyn std::error::Error>> {
    Ok(ControlConfig {
        package_object_prefix: "problem-packages".to_owned(),
        upload_ttl_seconds: 900,
        completion_lease_seconds: 300,
        max_package_files: 100,
        max_package_bytes: 1_048_576,
        retention_policy_id: PolicyId::new(),
        retention_seconds: 86_400,
        sse_retention_seconds: 3_600,
        trust_revision: Revision::new(1)?,
        image_policy_id: PolicyId::new(),
        image_policy_revision: Revision::new(1)?,
        environment_schema_sha256: Sha256Digest::of_bytes(b"environment"),
        evaluation_schema_sha256: Sha256Digest::of_bytes(b"evaluation"),
        container_build: ContainerBuildPolicy {
            builder_binding: "buildkit-primary-v1".to_owned(),
            output_repository_prefix: "harbor.internal/labweaver-system".to_owned(),
            dockerfile_path: "Dockerfile".to_owned(),
            runner_dockerfile_path: "evaluation/Dockerfile".to_owned(),
            network: BuildNetworkPolicy::DenyAll,
            max_duration_milliseconds: 600_000,
            max_cpu_millicores: 2_000,
            max_memory_bytes: 2_147_483_648,
        },
        virtual_machine_bases: control_service::VirtualMachineBaseCatalog {
            provider_binding: "kubevirt-primary-v1".to_owned(),
            storage_class_binding: "vm-rwo-primary-v1".to_owned(),
            max_bases: 8,
            max_capacity_bytes: 137_438_953_472,
            bases: vec![control_service::VirtualMachineBasePolicy {
                artifact_id: contracts::ImageArtifactId::new(),
                base_disk: contracts::supply_chain::VirtualMachineBaseDisk {
                    binding: "ubuntu-24.04-v1".to_owned(),
                    source_registry_digest: concat!(
                        "docker://quay.io/containerdisks/ubuntu@",
                        "sha256:d28194a16351320fa9a093e18233033508a745566eb8ba3b309c32924bf155a5"
                    )
                    .to_owned(),
                    capacity_bytes: 10_737_418_240,
                },
                format: contracts::supply_chain::VirtualMachineDiskFormat::Qcow2,
            }],
        },
        evaluation_runtime: EvaluationRuntimePolicy {
            provider_binding: "evaluation-primary-v1".to_owned(),
            runner_image: format!("runner@sha256:{}", "a".repeat(64)),
        },
        llm_policy_options: contracts::authoring::ProjectLlmPolicyOptions {
            models: vec![contracts::authoring::ProjectLlmPolicyModelOption {
                model: "fixture-provider-v1".to_owned(),
                label: "Fixture model".to_owned(),
            }],
            default_model: "fixture-provider-v1".to_owned(),
            runtime_binding: "claude-code-test".to_owned(),
            claude_code_version: "2.1.207".to_owned(),
            max_in_flight_per_worker: 2,
        },
    })
}

fn service_config(base_url: &reqwest::Url, ca_path: &std::path::Path) -> ServiceHttpClientConfig {
    ServiceHttpClientConfig {
        base_url: base_url.clone(),
        ca_certificate_file: ca_path.to_string_lossy().into_owned(),
        timeout_milliseconds: 3_000,
    }
}

async fn service_token_client(
    issuer: &str,
) -> Result<ServiceTokenClient, Box<dyn std::error::Error>> {
    let config = ServiceTokenClientConfig::new(
        issuer,
        "labweaver-control".to_owned(),
        "test-secret".to_owned(),
        "labweaver-control".to_owned(),
        BTreeSet::from(["access.control.forward".to_owned()]),
        30,
        TransportSecurityMode::InsecureTestOnly,
    )?;
    Ok(ServiceTokenClient::discover(
        config,
        no_redirect_http_client(None, TransportSecurityMode::InsecureTestOnly)?,
    )
    .await?)
}

async fn spawn_authority(jwk: Value) -> Result<AuthorityHandle, Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let issuer = format!("http://localhost:{}/realms/test", address.port());
    let state = (issuer.clone(), jwk);
    let router = Router::new()
        .route(
            "/realms/test/.well-known/openid-configuration",
            get(authority_discovery),
        )
        .route("/realms/test/jwks", get(authority_jwks))
        .route("/realms/test/token", post(authority_token))
        .with_state(state);
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        tokio::select! {
            result = axum::serve(listener, router) => {
                let _ = result;
            }
            _ = &mut shutdown_rx => {}
        }
    });
    Ok(AuthorityHandle {
        issuer,
        shutdown: Some(shutdown_tx),
        task,
    })
}

async fn authority_discovery(State((issuer, _)): State<(String, Value)>) -> Json<Value> {
    Json(json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{issuer}/authorize"),
        "token_endpoint": format!("{issuer}/token"),
        "jwks_uri": format!("{issuer}/jwks"),
        "response_types_supported": ["code"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["ES256"],
        "grant_types_supported": ["client_credentials"]
    }))
}

async fn authority_jwks(State((_, jwk)): State<(String, Value)>) -> Json<Value> {
    Json(json!({"keys": [jwk]}))
}

async fn authority_token(
    axum::extract::Form(request): axum::extract::Form<TokenRequest>,
) -> Result<Json<Value>, StatusCode> {
    if request.grant_type.as_deref() != Some("client_credentials") {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(Json(json!({
        "access_token": SERVICE_TOKEN,
        "token_type": "Bearer",
        "expires_in": 300
    })))
}

async fn spawn_tls_service(
    router: Router,
    certificate_pem: &str,
    private_key_pem: &str,
) -> Result<TlsServiceHandle, Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let config = tls_config(certificate_pem, private_key_pem)?;
    let acceptor = TlsAcceptor::from(config);
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        loop {
            let accepted = tokio::select! {
                result = listener.accept() => result,
                _ = &mut shutdown_rx => return,
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
    });
    Ok(TlsServiceHandle {
        base_url: format!("https://localhost:{}/", address.port()).parse()?,
        shutdown: Some(shutdown_tx),
        task,
    })
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

fn tls_material() -> Result<(String, String, String, Value), Box<dyn std::error::Error>> {
    let ca_key = KeyPair::generate()?;
    let mut ca_parameters = CertificateParams::new(Vec::<String>::new())?;
    ca_parameters.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_parameters.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
    ];
    let ca = CertifiedIssuer::self_signed(ca_parameters, ca_key)?;
    let mut leaf_parameters = CertificateParams::new(vec!["localhost".to_owned()])?;
    leaf_parameters.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    leaf_parameters.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let leaf_key = KeyPair::generate()?;
    let leaf = leaf_parameters.signed_by(&leaf_key, &ca)?;
    let public = leaf_key.public_key_raw();
    let jwk = json!({
        "kty": "EC",
        "crv": "P-256",
        "x": URL_SAFE_NO_PAD.encode(&public[1..33]),
        "y": URL_SAFE_NO_PAD.encode(&public[33..65]),
        "use": "sig",
        "alg": "ES256",
        "kid": "test"
    });
    Ok((ca.pem(), leaf.pem(), leaf_key.serialize_pem(), jwk))
}
