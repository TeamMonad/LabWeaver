//! Real service-JWT coverage for the Access-to-Environment ownership boundary.

use std::{
    collections::BTreeSet,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};

use auth::{
    EnvironmentOwnerResolverClient, OwnerResolverClientError, ServiceTokenClient,
    ServiceTokenClientConfig, TransportSecurityMode, no_redirect_http_client,
};
use axum::{
    Json, Router,
    extract::{Form, State},
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
use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, IsCa, KeyPair, KeyUsagePurpose};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{net::TcpListener, sync::oneshot};

const SERVICE_TOKEN: &str = "eyJhbGciOiJub25lIn0.eyJhdWQiOiJsYWJ3ZWF2ZXItZW52aXJvbm1lbnQifQ.sig";

#[derive(Clone)]
struct ResolverState {
    mode: Arc<AtomicU8>,
    issuer: String,
    service_token: String,
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one local authority scenario preserves token, tamper, outage, and retry lifecycle"
)]
async fn client_enforces_jwt_identity_response_binding_and_bounded_outage()
-> Result<(), Box<dyn std::error::Error>> {
    let ca = test_ca()?;
    let mode = Arc::new(AtomicU8::new(0));
    let (address, shutdown, server) = start_server(Arc::clone(&mode)).await?;
    let issuer = format!("http://localhost:{}/realms/test", address.port());
    let config = EnvironmentOwnerResolverClientConfig {
        resolver_uri: format!("http://localhost:{}", address.port()),
        ca_certificate_locator: "secret://access/owner-resolver-ca".to_owned(),
        timeout_milliseconds: 1_000,
        max_retries: 1,
    };
    let token_config = ServiceTokenClientConfig::new(
        &issuer,
        "access-service".to_owned(),
        "test-secret".to_owned(),
        "labweaver-environment".to_owned(),
        BTreeSet::from(["environment.owner.resolve".to_owned()]),
        30,
        TransportSecurityMode::InsecureTestOnly,
    )?;
    let token_http = no_redirect_http_client(None, TransportSecurityMode::InsecureTestOnly)?;
    let service_token_client = ServiceTokenClient::discover(token_config, token_http).await?;
    let client = EnvironmentOwnerResolverClient::new(
        &config,
        ca.pem().as_bytes(),
        service_token_client,
        "labweaver-environment".to_owned(),
        BTreeSet::from([
            "environment.console.resolve".to_owned(),
            "environment.endpoint.resolve".to_owned(),
            "environment.owner.resolve".to_owned(),
        ]),
        Duration::from_millis(5),
        TransportSecurityMode::InsecureTestOnly,
    )?;
    let request = EnvironmentOwnerResolutionRequest {
        environment_id: EnvironmentId::new(),
        project_id: contracts::ProjectId::new(),
        course_id: Some(CourseId::new()),
        owner_actor_id: ActorId::new(),
        expected_revision: Revision::new(7)?,
    };
    let now = "2026-07-15T00:00:00.000Z".parse::<UtcTimestamp>()?;

    let resolution = client.resolve(&request, now).await?;
    assert_eq!(resolution.environment_id, request.environment_id);
    assert_eq!(resolution.environment_revision, request.expected_revision);

    let endpoint_request = EnvironmentEndpointEligibilityRequest {
        environment_id: request.environment_id,
        project_id: request.project_id,
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
    assert_eq!(
        client
            .resolve_endpoint_eligibility(&endpoint_request, now)
            .await,
        Err(OwnerResolverClientError::ScopeDenied)
    );

    mode.store(2, Ordering::SeqCst);
    assert_eq!(
        client.resolve(&request, now).await,
        Err(OwnerResolverClientError::ResponseInvalid)
    );
    assert_eq!(
        client
            .resolve_endpoint_eligibility(&endpoint_request, now)
            .await,
        Err(OwnerResolverClientError::ResponseInvalid)
    );

    mode.store(3, Ordering::SeqCst);
    assert_eq!(
        client
            .resolve_endpoint_eligibility(&endpoint_request, now)
            .await,
        Err(OwnerResolverClientError::ResponseInvalid)
    );

    mode.store(4, Ordering::SeqCst);
    assert_eq!(
        client
            .resolve_endpoint_eligibility(&endpoint_request, now)
            .await,
        Err(OwnerResolverClientError::ResponseInvalid)
    );

    mode.store(0, Ordering::SeqCst);
    let unrelated_ca = test_ca()?;
    let token_config = ServiceTokenClientConfig::new(
        &issuer,
        "access-service".to_owned(),
        "test-secret".to_owned(),
        "labweaver-environment".to_owned(),
        BTreeSet::from(["environment.owner.resolve".to_owned()]),
        30,
        TransportSecurityMode::InsecureTestOnly,
    )?;
    let token_http = no_redirect_http_client(None, TransportSecurityMode::InsecureTestOnly)?;
    let service_token_client = ServiceTokenClient::discover(token_config, token_http).await?;
    let insecure_client = EnvironmentOwnerResolverClient::new(
        &config,
        unrelated_ca.pem().as_bytes(),
        service_token_client,
        "labweaver-environment".to_owned(),
        BTreeSet::from([
            "environment.console.resolve".to_owned(),
            "environment.endpoint.resolve".to_owned(),
            "environment.owner.resolve".to_owned(),
        ]),
        Duration::from_millis(5),
        TransportSecurityMode::InsecureTestOnly,
    )?;
    let insecure_resolution = insecure_client.resolve(&request, now).await?;
    assert_eq!(insecure_resolution.environment_id, request.environment_id);

    let mut disallowed = config.clone();
    disallowed.resolver_uri = "http://resolver.internal:1234".to_owned();
    let token_config = ServiceTokenClientConfig::new(
        &issuer,
        "access-service".to_owned(),
        "test-secret".to_owned(),
        "labweaver-environment".to_owned(),
        BTreeSet::from(["environment.owner.resolve".to_owned()]),
        30,
        TransportSecurityMode::InsecureTestOnly,
    )?;
    let token_http = no_redirect_http_client(None, TransportSecurityMode::InsecureTestOnly)?;
    let service_token_client = ServiceTokenClient::discover(token_config, token_http).await?;
    assert!(matches!(
        EnvironmentOwnerResolverClient::new(
            &disallowed,
            ca.pem().as_bytes(),
            service_token_client,
            "labweaver-environment".to_owned(),
            BTreeSet::from([
                "environment.console.resolve".to_owned(),
                "environment.endpoint.resolve".to_owned(),
                "environment.owner.resolve".to_owned(),
            ]),
            Duration::from_millis(5),
            TransportSecurityMode::InsecureTestOnly,
        ),
        Err(OwnerResolverClientError::Configuration)
    ));

    shutdown
        .send(())
        .map_err(|()| "owner resolver shutdown receiver disappeared")?;
    server.await?;
    assert_eq!(
        client.resolve(&request, now).await,
        Err(OwnerResolverClientError::Unavailable)
    );
    assert_eq!(
        client
            .resolve_endpoint_eligibility(&endpoint_request, now)
            .await,
        Err(OwnerResolverClientError::Unavailable)
    );

    Ok(())
}

#[derive(Deserialize)]
struct TokenRequest {
    grant_type: Option<String>,
}

async fn discovery(State(state): State<ResolverState>) -> Json<Value> {
    Json(json!({
        "issuer": state.issuer,
        "authorization_endpoint": format!("{}/authorize", state.issuer),
        "token_endpoint": format!("{}/token", state.issuer),
        "jwks_uri": format!("{}/jwks", state.issuer),
        "response_types_supported": ["code"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["ES256"],
        "grant_types_supported": ["client_credentials"]
    }))
}

async fn token_endpoint(
    State(state): State<ResolverState>,
    Form(request): Form<TokenRequest>,
) -> Result<Json<Value>, StatusCode> {
    if request.grant_type.as_deref() != Some("client_credentials") {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(Json(json!({
        "access_token": state.service_token,
        "token_type": "Bearer",
        "expires_in": 300
    })))
}

async fn jwks() -> Json<Value> {
    Json(json!({"keys": []}))
}

async fn resolve_owner(
    State(state): State<ResolverState>,
    headers: axum::http::HeaderMap,
    Json(request): Json<EnvironmentOwnerResolutionRequest>,
) -> Result<Response, StatusCode> {
    require_service_token(&headers, &state)?;
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
        project_id: request.project_id,
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
    headers: axum::http::HeaderMap,
    Json(request): Json<EnvironmentEndpointEligibilityRequest>,
) -> Result<Response, StatusCode> {
    require_service_token(&headers, &state)?;
    if state.mode.load(Ordering::SeqCst) == 1 {
        return Err(StatusCode::FORBIDDEN);
    }
    let environment_id = if state.mode.load(Ordering::SeqCst) == 2 {
        EnvironmentId::new()
    } else {
        request.environment_id
    };
    let environment_revision = if state.mode.load(Ordering::SeqCst) == 3 {
        Revision::new(request.expected_revision.get() + 1).map_err(|_| StatusCode::BAD_REQUEST)?
    } else {
        request.expected_revision
    };
    let health = if state.mode.load(Ordering::SeqCst) == 4 {
        EndpointHealth::Unhealthy
    } else {
        EndpointHealth::Healthy
    };
    let resolution = EnvironmentEndpointEligibility {
        environment_id,
        project_id: request.project_id,
        course_id: request.course_id,
        owner_actor_id: request.actor_id,
        environment_revision,
        eligibility_expires_at: "2030-07-15T00:00:00.000Z"
            .parse()
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        endpoints: vec![EnvironmentEndpoint {
            id: request.endpoint_ids[0],
            protocol: EndpointProtocol::Ssh,
            revision: request.expected_revision,
            health,
            observed_at: "2026-07-15T00:00:00.000Z"
                .parse()
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        }],
    };
    let etag = StrongEtag::from_revision(resolution.environment_revision).header_value();
    Ok(([(header::ETAG, etag)], Json(resolution)).into_response())
}

fn require_service_token(
    headers: &axum::http::HeaderMap,
    state: &ResolverState,
) -> Result<(), StatusCode> {
    let expected = format!("Bearer {}", state.service_token);
    if headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        != Some(expected.as_str())
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(())
}

async fn start_server(
    mode: Arc<AtomicU8>,
) -> Result<
    (SocketAddr, oneshot::Sender<()>, tokio::task::JoinHandle<()>),
    Box<dyn std::error::Error>,
> {
    let listener = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).await?;
    let address = listener.local_addr()?;
    let issuer = format!("http://localhost:{}/realms/test", address.port());
    let router = Router::new()
        .route(
            "/realms/test/.well-known/openid-configuration",
            axum::routing::get(discovery),
        )
        .route("/realms/test/token", post(token_endpoint))
        .route("/realms/test/jwks", axum::routing::get(jwks))
        .route(
            "/internal/v1/environments/{environment_id}/owner:resolve",
            post(resolve_owner),
        )
        .route(
            "/internal/v1/environments/{environment_id}/endpoint-eligibility:resolve",
            post(resolve_endpoint_eligibility),
        )
        .with_state(ResolverState {
            mode,
            issuer,
            service_token: SERVICE_TOKEN.to_owned(),
        });
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        tokio::select! {
            result = axum::serve(listener, router) => {
                let _ = result;
            }
            _ = shutdown_rx => {}
        }
    });
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
