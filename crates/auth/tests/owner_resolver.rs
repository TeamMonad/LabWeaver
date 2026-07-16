//! Real mTLS coverage for the Access-to-Environment ownership boundary.

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};

use auth::{EnvironmentOwnerResolverClient, OwnerResolverClientError, TransportSecurityMode};
use axum::{
    Json, Router,
    extract::State,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use contracts::environment::{
    EndpointHealth, EndpointProtocol, EnvironmentAccessSubjectKind, EnvironmentEndpoint,
    EnvironmentEndpointEligibility, EnvironmentEndpointEligibilityRequest,
    EnvironmentOwnerResolution, EnvironmentOwnerResolutionRequest,
    EnvironmentOwnerResolverClientConfig,
};
use contracts::http::StrongEtag;
use contracts::{ActorId, CourseId, EndpointId, EnvironmentId, Revision, UtcTimestamp};
use environment_service::{MtlsConfig, serve_owner_resolver_mtls};
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose,
};
use tokio::{net::TcpListener, sync::oneshot};

#[derive(Clone)]
struct ResolverState {
    mode: Arc<AtomicU8>,
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one ephemeral-CA scenario preserves the full mTLS, tamper, outage and SAN lifecycle"
)]
async fn client_enforces_mtls_identity_response_binding_and_bounded_outage()
-> Result<(), Box<dyn std::error::Error>> {
    let ca = test_ca()?;
    let (server_certificate, server_key) = leaf_certificate(&ca, "localhost", false)?;
    let (client_certificate, client_key) = leaf_certificate(&ca, "access-service.internal", true)?;
    let mode = Arc::new(AtomicU8::new(0));
    let router = Router::new()
        .route(
            "/internal/v1/environments/{environment_id}/owner:resolve",
            post(resolve_owner),
        )
        .route(
            "/internal/v1/environments/{environment_id}/endpoint-eligibility:resolve",
            post(resolve_endpoint_eligibility),
        )
        .with_state(ResolverState {
            mode: Arc::clone(&mode),
        });
    let (address, shutdown, server) =
        start_server(router, &ca.pem(), &server_certificate, &server_key).await?;
    let config = EnvironmentOwnerResolverClientConfig {
        resolver_uri: format!("https://localhost:{}", address.port()),
        ca_certificate_locator: "secret://access/owner-resolver-ca".to_owned(),
        client_certificate_locator: "secret://access/owner-resolver-client-certificate".to_owned(),
        client_private_key_locator: "secret://access/owner-resolver-client-private-key".to_owned(),
        allowed_server_sans: vec!["localhost".to_owned()],
        timeout_milliseconds: 1_000,
        max_retries: 1,
    };
    let client = EnvironmentOwnerResolverClient::new(
        &config,
        ca.pem().as_bytes(),
        client_certificate.as_bytes(),
        client_key.as_bytes(),
        Duration::from_millis(5),
        TransportSecurityMode::Strict,
    )?;
    let request = EnvironmentOwnerResolutionRequest {
        environment_id: EnvironmentId::new(),
        course_id: CourseId::new(),
        owner_actor_id: ActorId::new(),
        expected_revision: Revision::new(7)?,
    };
    let now = "2026-07-15T00:00:00.000Z".parse::<UtcTimestamp>()?;

    let resolution = client.resolve(&request, now).await?;
    assert_eq!(resolution.environment_id, request.environment_id);
    assert_eq!(resolution.environment_revision, request.expected_revision);

    let endpoint_request = EnvironmentEndpointEligibilityRequest {
        environment_id: request.environment_id,
        course_id: request.course_id,
        actor_id: request.owner_actor_id,
        subject_kind: EnvironmentAccessSubjectKind::Owner,
        expected_revision: request.expected_revision,
        endpoint_ids: vec![EndpointId::new()],
    };
    let endpoint_resolution = client
        .resolve_endpoint_eligibility(&endpoint_request, now)
        .await?;
    assert_eq!(endpoint_resolution.endpoints.len(), 1);
    assert_eq!(
        endpoint_resolution.endpoints[0].id,
        endpoint_request.endpoint_ids[0]
    );

    mode.store(1, Ordering::SeqCst);
    assert_eq!(
        client.resolve(&request, now).await,
        Err(OwnerResolverClientError::ScopeDenied)
    );

    mode.store(2, Ordering::SeqCst);
    assert_eq!(
        client.resolve(&request, now).await,
        Err(OwnerResolverClientError::ResponseInvalid)
    );

    mode.store(0, Ordering::SeqCst);
    let unrelated_ca = test_ca()?;
    let insecure_client = EnvironmentOwnerResolverClient::new(
        &config,
        unrelated_ca.pem().as_bytes(),
        client_certificate.as_bytes(),
        client_key.as_bytes(),
        Duration::from_millis(5),
        TransportSecurityMode::InsecureTestOnly,
    )?;
    let insecure_resolution = insecure_client.resolve(&request, now).await?;
    assert_eq!(insecure_resolution.environment_id, request.environment_id);

    shutdown
        .send(())
        .map_err(|()| "owner resolver shutdown receiver disappeared")?;
    server.await??;
    assert_eq!(
        client.resolve(&request, now).await,
        Err(OwnerResolverClientError::Unavailable)
    );

    let mut disallowed = config;
    disallowed.allowed_server_sans = vec!["different.internal".to_owned()];
    assert!(matches!(
        EnvironmentOwnerResolverClient::new(
            &disallowed,
            ca.pem().as_bytes(),
            client_certificate.as_bytes(),
            client_key.as_bytes(),
            Duration::from_millis(5),
            TransportSecurityMode::Strict,
        ),
        Err(OwnerResolverClientError::Configuration)
    ));
    Ok(())
}

async fn resolve_owner(
    State(state): State<ResolverState>,
    Json(request): Json<EnvironmentOwnerResolutionRequest>,
) -> Result<Response, StatusCode> {
    if state.mode.load(Ordering::SeqCst) == 1 {
        return Err(StatusCode::FORBIDDEN);
    }
    let environment_id = if state.mode.load(Ordering::SeqCst) == 2 {
        EnvironmentId::new()
    } else {
        request.environment_id
    };
    let resolution = EnvironmentOwnerResolution {
        environment_id,
        course_id: request.course_id,
        owner_actor_id: request.owner_actor_id,
        environment_revision: request.expected_revision,
        eligibility_expires_at: "2030-07-15T00:00:00.000Z"
            .parse()
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
    };
    let etag = StrongEtag::from_revision(resolution.environment_revision).header_value();
    Ok(([(header::ETAG, etag)], Json(resolution)).into_response())
}

async fn resolve_endpoint_eligibility(
    State(state): State<ResolverState>,
    Json(request): Json<EnvironmentEndpointEligibilityRequest>,
) -> Result<Response, StatusCode> {
    if state.mode.load(Ordering::SeqCst) == 1 {
        return Err(StatusCode::FORBIDDEN);
    }
    let environment_id = if state.mode.load(Ordering::SeqCst) == 2 {
        EnvironmentId::new()
    } else {
        request.environment_id
    };
    let resolution = EnvironmentEndpointEligibility {
        environment_id,
        course_id: request.course_id,
        owner_actor_id: request.actor_id,
        environment_revision: request.expected_revision,
        eligibility_expires_at: "2030-07-15T00:00:00.000Z"
            .parse()
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        endpoints: vec![EnvironmentEndpoint {
            id: request.endpoint_ids[0],
            protocol: EndpointProtocol::Ssh,
            revision: request.expected_revision,
            health: EndpointHealth::Healthy,
            observed_at: "2026-07-15T00:00:00.000Z"
                .parse()
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        }],
    };
    let etag = StrongEtag::from_revision(resolution.environment_revision).header_value();
    Ok(([(header::ETAG, etag)], Json(resolution)).into_response())
}

async fn start_server(
    router: Router,
    ca_pem: &str,
    server_certificate_pem: &str,
    server_private_key_pem: &str,
) -> Result<
    (
        SocketAddr,
        oneshot::Sender<()>,
        tokio::task::JoinHandle<Result<(), environment_service::MtlsServerError>>,
    ),
    Box<dyn std::error::Error>,
> {
    let listener = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).await?;
    let address = listener.local_addr()?;
    let config = MtlsConfig::from_pem(
        ca_pem.as_bytes(),
        server_certificate_pem.as_bytes(),
        server_private_key_pem.as_bytes(),
    )?;
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let server = tokio::spawn(serve_owner_resolver_mtls(
        listener,
        router,
        config,
        async move {
            let _ = shutdown_rx.await;
            Ok(())
        },
    ));
    Ok((address, shutdown_tx, server))
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
    let certificate = parameters.signed_by(&key, ca)?;
    Ok((certificate.pem(), key.serialize_pem()))
}
