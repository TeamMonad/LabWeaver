//! Ephemeral-CA mTLS route coverage for Gateway-to-Control and Control-owned clients.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use artifact_store::{ImmutableObjectStore, ObjectStoreError, PresignedUpload, VerifiedObject};
use async_trait::async_trait;
use auth::{MtlsFileConfig, extract_mtls_principal, load_mtls_server_config};
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use contracts::authoring::{AgentRun, AgentRunState, AgentTrack, AgentTrackKind, RuntimeKind};
use contracts::http::{CreateProblemPackageUploadRequest, ProblemPackageUploadFile};
use contracts::supply_chain::BuildNetworkPolicy;
use contracts::{
    ActorId, AgentRunId, AuthenticatedActor, AuthorizationDecision, AuthorizationDecisionRequest,
    BffSessionId, CourseId, PlatformRole, PolicyId, ProblemDetails, ProblemPackageId, Revision,
    Sha256Digest, UtcTimestamp,
};
use control_service::api::{ApiState, router, serve_mtls};
use control_service::clients::{AccessClient, AgentClient, DownstreamError, MtlsClientFileConfig};
use control_service::{ContainerBuildPolicy, ControlConfig, ControlService};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder as HyperBuilder;
use hyper_util::service::TowerToHyperService;
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose, SanType,
};
use sqlx::postgres::PgPoolOptions;
use tempfile::TempDir;
use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;

const GATEWAY_URI: &str = "spiffe://labweaver/gateway";
const CONTROL_URI: &str = "spiffe://labweaver/control-service";

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one ephemeral CA identity proves the complete Gateway and Control client matrix"
)]
async fn mtls_sans_rotation_and_outage_fail_closed_on_control_routes()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let ca = test_ca()?;
    let ca_file = write(&directory, "ca.pem", &ca.pem())?;
    let (server_certificate, server_key) = dns_leaf(&ca, "localhost", false)?;
    let server_certificate_file = write(&directory, "server.pem", &server_certificate)?;
    let server_key_file = write(&directory, "server.key", &server_key)?;
    let gateway1 = client_identity(&directory, &ca, "gateway-1", GATEWAY_URI)?;
    let gateway2 = client_identity(&directory, &ca, "gateway-2", GATEWAY_URI)?;
    let control1 = client_identity(&directory, &ca, "control-1", CONTROL_URI)?;
    let control2 = client_identity(&directory, &ca, "control-2", CONTROL_URI)?;
    let wrong = client_identity(&directory, &ca, "wrong", "spiffe://labweaver/untrusted")?;

    let run = requested_run()?;
    let downstream_state = DownstreamState {
        unavailable: Arc::new(AtomicBool::new(false)),
        run: run.clone(),
    };
    let downstream_router = Router::new()
        .route("/internal/v1/auth/decision", post(authorize))
        .route("/internal/v1/agent-runs/{run_id}", get(get_run))
        .with_state(downstream_state.clone());
    let (downstream_address, downstream_task) = start_mtls(
        downstream_router,
        &server_certificate_file,
        &server_key_file,
        &ca_file,
        CONTROL_URI,
    )
    .await?;
    let downstream_url = format!("https://localhost:{}/", downstream_address.port());
    let access = AccessClient::new(client_config(&downstream_url, &ca_file, &control1, 1_000)?)?;
    let agent = AgentClient::new(client_config(&downstream_url, &ca_file, &control1, 1_000)?)?;

    let postgres = Postgres::default().with_tag("17.5-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            postgres.get_host_port_ipv4(5432).await?
        ))
        .await?;
    sqlx::raw_sql(&format!(
        "CREATE SCHEMA control; SET search_path TO control;\n{}\n{}\n{}",
        include_str!("../../../migrations/control/0001_initial.sql"),
        include_str!("../../../migrations/control/0002_control_plane.sql"),
        include_str!("../../../migrations/control/0003_container_build_projections.sql")
    ))
    .execute(&pool)
    .await?;
    let control = ControlService::new(pool, Arc::new(PresigningObjects), config()?)?;
    let state = Arc::new(ApiState {
        control,
        access,
        agent,
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let control_address = listener.local_addr()?;
    let control_mtls = load_mtls_server_config(&MtlsFileConfig {
        bind_addr: control_address.to_string(),
        server_certificate_file: path(&server_certificate_file),
        server_key_file: path(&server_key_file),
        client_ca_file: path(&ca_file),
        allowed_san_uris: BTreeSet::from([GATEWAY_URI.to_owned()]),
        required_eku: "clientAuth".to_owned(),
    })?;
    let control_task = tokio::spawn(serve_mtls(listener, router(state), control_mtls));
    let control_url = format!("https://localhost:{}/", control_address.port());
    let actor = ActorId::new();
    let session = BffSessionId::new();
    let course = CourseId::new();
    let retention_revision = Revision::new(1)?;
    for (index, gateway) in [&gateway1, &gateway2].into_iter().enumerate() {
        let response = upload_request(
            client(&ca_file, gateway)?,
            &control_url,
            course,
            actor,
            session,
            retention_revision,
            &format!("issue-48-gateway-rotation-{index}"),
        )
        .await?;
        let status = response.status();
        let body = response.text().await?;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
    assert!(
        upload_request(
            client(&ca_file, &wrong)?,
            &control_url,
            course,
            actor,
            session,
            retention_revision,
            "issue-48-wrong-gateway",
        )
        .await
        .is_err()
    );

    for control_identity in [&control1, &control2] {
        let client = AgentClient::new(client_config(
            &downstream_url,
            &ca_file,
            control_identity,
            1_000,
        )?)?;
        assert_eq!(client.get(run.id).await?, run);
    }
    let denied = AgentClient::new(client_config(&downstream_url, &ca_file, &wrong, 1_000)?)?;
    assert_eq!(denied.get(run.id).await, Err(DownstreamError::Unavailable));

    downstream_state.unavailable.store(true, Ordering::SeqCst);
    let response = upload_request(
        client(&ca_file, &gateway1)?,
        &control_url,
        course,
        actor,
        session,
        retention_revision,
        "issue-48-access-outage",
    )
    .await?;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let problem = response.json::<ProblemDetails>().await?;
    assert_eq!(
        problem.diagnostic_code.as_str(),
        "LW_CONTROL_DOWNSTREAM_UNAVAILABLE"
    );

    control_task.abort();
    downstream_task.abort();
    Ok(())
}

#[derive(Clone)]
struct DownstreamState {
    unavailable: Arc<AtomicBool>,
    run: AgentRun,
}

async fn authorize(
    State(state): State<DownstreamState>,
    Json(request): Json<AuthorizationDecisionRequest>,
) -> Result<Json<AuthorizationDecision>, StatusCode> {
    if state.unavailable.load(Ordering::SeqCst) {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    let valid_until = "2030-01-01T00:00:00.000Z"
        .parse::<UtcTimestamp>()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(AuthorizationDecision {
        actor: AuthenticatedActor {
            actor_id: request.actor_id,
            roles: vec![PlatformRole::Teacher],
            expires_at: valid_until,
        },
        scope: request.scope,
        authorization_revision: Revision::new(1).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        scope_revision: Revision::new(1).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        valid_until,
        diagnostic_code: None,
    }))
}

async fn get_run(State(state): State<DownstreamState>) -> Result<Json<AgentRun>, StatusCode> {
    if state.unavailable.load(Ordering::SeqCst) {
        Err(StatusCode::SERVICE_UNAVAILABLE)
    } else {
        Ok(Json(state.run))
    }
}

async fn upload_request(
    client: reqwest::Client,
    base_url: &str,
    course: CourseId,
    actor: ActorId,
    session: BffSessionId,
    retention_revision: Revision,
    key: &str,
) -> Result<reqwest::Response, reqwest::Error> {
    client
        .post(format!(
            "{base_url}api/v1/courses/{course}/problem-package-uploads"
        ))
        .header("X-LabWeaver-Actor-Id", actor.to_string())
        .header("X-LabWeaver-Session-Id", session.to_string())
        .header("Idempotency-Key", key)
        .json(&CreateProblemPackageUploadRequest {
            files: vec![ProblemPackageUploadFile {
                path: "statement.md".to_owned(),
                size_bytes: 9,
                sha256: Sha256Digest::of_bytes(b"statement"),
                media_type: "text/markdown".to_owned(),
            }],
            retention_policy_revision: retention_revision,
        })
        .send()
        .await
}

async fn start_mtls(
    router: Router,
    certificate: &Path,
    key: &Path,
    ca: &Path,
    allowed_uri: &str,
) -> Result<
    (
        std::net::SocketAddr,
        tokio::task::JoinHandle<Result<(), std::io::Error>>,
    ),
    Box<dyn std::error::Error>,
> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let mtls = load_mtls_server_config(&MtlsFileConfig {
        bind_addr: address.to_string(),
        server_certificate_file: path(certificate),
        server_key_file: path(key),
        client_ca_file: path(ca),
        allowed_san_uris: BTreeSet::from([allowed_uri.to_owned()]),
        required_eku: "clientAuth".to_owned(),
    })?;
    let task = tokio::spawn(async move {
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::clone(&mtls.server_config));
        loop {
            let (stream, _) = listener.accept().await?;
            let acceptor = acceptor.clone();
            let router = router.clone();
            let allowed = mtls.allowed_san_uris.clone();
            tokio::spawn(async move {
                let Ok(tls) = acceptor.accept(stream).await else {
                    return;
                };
                let Some(peer) = tls
                    .get_ref()
                    .1
                    .peer_certificates()
                    .and_then(|certificates| certificates.first())
                else {
                    return;
                };
                let Ok(principal) = extract_mtls_principal(peer, &allowed) else {
                    return;
                };
                let service = router.layer(Extension(principal));
                if HyperBuilder::new(TokioExecutor::new())
                    .serve_connection_with_upgrades(
                        TokioIo::new(tls),
                        TowerToHyperService::new(service),
                    )
                    .await
                    .is_err()
                {
                    tracing::debug!(event = "test.mtls.connection_closed");
                }
            });
        }
    });
    Ok((address, task))
}

fn client_config(
    base_url: &str,
    ca: &Path,
    identity: &(PathBuf, PathBuf),
    timeout_milliseconds: u64,
) -> Result<MtlsClientFileConfig, DownstreamError> {
    Ok(MtlsClientFileConfig {
        base_url: base_url
            .parse()
            .map_err(|_| DownstreamError::Configuration)?,
        ca_certificate_file: path(ca),
        client_certificate_file: path(&identity.0),
        client_private_key_file: path(&identity.1),
        timeout_milliseconds,
    })
}

fn client(ca: &Path, identity: &(PathBuf, PathBuf)) -> Result<reqwest::Client, DownstreamError> {
    client_config("https://localhost/", ca, identity, 1_000)?.build()
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

fn dns_leaf(
    ca: &CertifiedIssuer<'static, KeyPair>,
    san: &str,
    client: bool,
) -> Result<(String, String), rcgen::Error> {
    let mut parameters = CertificateParams::new(vec![san.to_owned()])?;
    parameters.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    parameters.extended_key_usages = vec![if client {
        ExtendedKeyUsagePurpose::ClientAuth
    } else {
        ExtendedKeyUsagePurpose::ServerAuth
    }];
    let key = KeyPair::generate()?;
    Ok((parameters.signed_by(&key, ca)?.pem(), key.serialize_pem()))
}

fn client_identity(
    directory: &TempDir,
    ca: &CertifiedIssuer<'static, KeyPair>,
    name: &str,
    uri: &str,
) -> Result<(PathBuf, PathBuf), Box<dyn std::error::Error>> {
    let mut parameters = CertificateParams::new(Vec::<String>::new())?;
    parameters.subject_alt_names = vec![SanType::URI(uri.try_into()?)];
    parameters.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    parameters.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    let key = KeyPair::generate()?;
    let certificate = parameters.signed_by(&key, ca)?.pem();
    Ok((
        write(directory, &format!("{name}.pem"), &certificate)?,
        write(directory, &format!("{name}.key"), &key.serialize_pem())?,
    ))
}

fn write(directory: &TempDir, name: &str, value: &str) -> Result<PathBuf, std::io::Error> {
    let path = directory.path().join(name);
    std::fs::write(&path, value)?;
    Ok(path)
}

fn path(value: &Path) -> String {
    value.to_string_lossy().into_owned()
}

fn requested_run() -> Result<AgentRun, Box<dyn std::error::Error>> {
    Ok(AgentRun {
        id: AgentRunId::new(),
        course_id: CourseId::new(),
        package_id: ProblemPackageId::new(),
        policy_id: PolicyId::new(),
        policy_revision: Revision::new(1)?,
        requested_runtime: RuntimeKind::Container,
        state: AgentRunState::Requested,
        revision: Revision::new(1)?,
        tracks: vec![
            AgentTrack {
                kind: AgentTrackKind::Environment,
                attempts: Vec::new(),
                candidate_id: None,
            },
            AgentTrack {
                kind: AgentTrackKind::Evaluation,
                attempts: Vec::new(),
                candidate_id: None,
            },
        ],
    })
}

struct PresigningObjects;

#[async_trait]
impl ImmutableObjectStore for PresigningObjects {
    async fn presign_upload(
        &self,
        _: &str,
        _: u64,
        _: Sha256Digest,
        _: &str,
        now: UtcTimestamp,
    ) -> Result<PresignedUpload, ObjectStoreError> {
        Ok(PresignedUpload {
            url: "https://minio.invalid/upload".to_owned(),
            required_headers: std::collections::BTreeMap::default(),
            expires_at: UtcTimestamp::from_utc(now.get() + time::Duration::seconds(900))
                .map_err(|_| ObjectStoreError::ConfigurationInvalid)?,
        })
    }

    async fn read_verified(
        &self,
        _: &str,
        _: &str,
        _: u64,
        _: Sha256Digest,
        _: &str,
    ) -> Result<VerifiedObject, ObjectStoreError> {
        Err(ObjectStoreError::ObjectUnavailable)
    }

    async fn freeze_current(
        &self,
        _: &str,
        _: u64,
        _: Sha256Digest,
        _: &str,
    ) -> Result<VerifiedObject, ObjectStoreError> {
        Err(ObjectStoreError::ObjectUnavailable)
    }

    async fn delete_orphan(&self, _: &str, _: &str) -> Result<(), ObjectStoreError> {
        Err(ObjectStoreError::DeleteFailed)
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
            output_repository_prefix: "harbor.internal".to_owned(),
            dockerfile_path: "Dockerfile".to_owned(),
            network: BuildNetworkPolicy::DenyAll,
            max_duration_milliseconds: 600_000,
            max_cpu_millicores: 2_000,
            max_memory_bytes: 2_147_483_648,
        },
    })
}
