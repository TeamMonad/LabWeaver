//! Real `PostgreSQL` plus mTLS owner-resolver integration coverage.
#![allow(
    clippy::too_many_lines,
    reason = "one end-to-end test keeps certificate, database, and outage evidence under one identity"
)]

mod support;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use contracts::environment::{
    EnvironmentInstance, EnvironmentOperationKind, EnvironmentOwnerResolutionRequest,
};
use contracts::{ActorId, CourseId};
use environment_service::{
    LifecycleCommand, MtlsConfig, MtlsServerError, OwnerResolver, OwnerResolverPolicy,
    PgEnvironmentStore, authorize_owner_resolution, owner_resolver_router, plan_command,
    serve_owner_resolver_mtls,
};
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose,
};
use reqwest::{Certificate, Client, Identity, StatusCode};
use sqlx::{PgPool, postgres::PgPoolOptions};
use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

const ALLOWED_CALLER_SAN: &str = "access-service.internal";

#[tokio::test]
async fn resolver_uses_real_postgres_and_verified_rotatable_mtls_identity()
-> Result<(), Box<dyn std::error::Error>> {
    let container = Postgres::default().with_tag("17.5-alpine").start().await?;
    let database_url = format!(
        "postgres://postgres:postgres@127.0.0.1:{}/postgres",
        container.get_host_port_ipv4(5432).await?
    );
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&database_url)
        .await?;
    migrate(&pool).await?;

    let mut authoritative = support::ready_instance();
    authoritative.eligibility_expires_at = support::timestamp("2030-07-15T00:00:00.000Z");
    insert_instance(&pool, &authoritative).await?;

    let ca = test_ca()?;
    let (server_certificate, server_key) = leaf_certificate(&ca, "localhost", false)?;
    let (client_certificate, client_key) = leaf_certificate(&ca, ALLOWED_CALLER_SAN, true)?;
    let (rotated_client_certificate, rotated_client_key) =
        leaf_certificate(&ca, ALLOWED_CALLER_SAN, true)?;
    let (unregistered_certificate, unregistered_key) =
        leaf_certificate(&ca, "unknown-service.internal", true)?;

    let store = PgEnvironmentStore::new(pool.clone());
    let resolver = OwnerResolver::new(
        store.clone(),
        OwnerResolverPolicy::new([ALLOWED_CALLER_SAN])?,
    );
    let (address, shutdown, server) = start_server(
        owner_resolver_router(resolver.clone()),
        &ca.pem(),
        &server_certificate,
        &server_key,
    )
    .await?;
    let allowed = mtls_client(&ca.pem(), &client_certificate, &client_key)?;
    let rotated = mtls_client(&ca.pem(), &rotated_client_certificate, &rotated_client_key)?;
    let unregistered = mtls_client(&ca.pem(), &unregistered_certificate, &unregistered_key)?;
    let no_identity = Client::builder()
        .add_root_certificate(Certificate::from_pem(ca.pem().as_bytes())?)
        .build()?;

    let original_request = request_for(&authoritative);
    let original_response = resolve(&allowed, address, &original_request).await?;
    assert_eq!(original_response.status(), StatusCode::OK);
    assert_eq!(
        original_response
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|value| value.to_str().ok()),
        Some("\"rev-2\"")
    );
    let mut slow_handshake = tokio::net::TcpStream::connect(address).await?;
    tokio::time::sleep(std::time::Duration::from_millis(5_100)).await;
    let mut byte = [0_u8; 1];
    let closed = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        slow_handshake.read(&mut byte),
    )
    .await?;
    assert!(matches!(closed, Ok(0) | Err(_)));
    assert_eq!(
        resolve(&allowed, address, &original_request)
            .await?
            .status(),
        StatusCode::OK
    );

    let mut wrong_course = original_request.clone();
    wrong_course.course_id = CourseId::new();
    assert_eq!(
        resolve(&allowed, address, &wrong_course).await?.status(),
        StatusCode::FORBIDDEN
    );
    let mut wrong_owner = original_request.clone();
    wrong_owner.owner_actor_id = ActorId::new();
    assert_eq!(
        resolve(&allowed, address, &wrong_owner).await?.status(),
        StatusCode::FORBIDDEN
    );
    let mut stale_revision = original_request.clone();
    stale_revision.expected_revision = support::revision(1);
    assert_eq!(
        resolve(&allowed, address, &stale_revision).await?.status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        resolve(&unregistered, address, &original_request)
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );
    assert!(
        resolve(&no_identity, address, &original_request)
            .await
            .is_err()
    );
    assert_eq!(
        resolve(&rotated, address, &original_request)
            .await?
            .status(),
        StatusCode::OK
    );

    let mut reassigned = authoritative.clone();
    reassigned.owner_id = ActorId::new();
    reassigned.revision = support::revision(3);
    for endpoint in &mut reassigned.endpoints {
        endpoint.revision = reassigned.revision;
    }
    update_instance(&pool, &reassigned).await?;
    assert_eq!(
        resolve(&allowed, address, &original_request)
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );
    let reassigned_request = request_for(&reassigned);
    assert_eq!(
        resolve(&allowed, address, &reassigned_request)
            .await?
            .status(),
        StatusCode::OK
    );

    let database_now = store.current_time().await?;
    let mut expired = reassigned.clone();
    expired.eligibility_expires_at = shift_minutes(database_now, -1)?;
    let lagged_process_clock = shift_minutes(database_now, -2)?;
    assert!(
        authorize_owner_resolution(&expired, &request_for(&expired), lagged_process_clock).is_ok()
    );
    update_instance(&pool, &expired).await?;
    assert_eq!(
        resolve(&allowed, address, &request_for(&expired))
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );

    let deleting = plan_command(
        &reassigned,
        &LifecycleCommand {
            environment_id: reassigned.id,
            kind: EnvironmentOperationKind::Delete,
            expected_revision: reassigned.revision,
            actor_id: ActorId::new(),
            trace_id: "trace-mtls-delete-0001".to_owned(),
            accepted_at: support::timestamp("2026-07-15T01:00:00.000Z"),
            deadline_at: support::timestamp("2026-07-15T01:10:00.000Z"),
            access_revocation_revision: Some(support::revision(7)),
            preserve_mutable_disk: false,
            max_attempts: 3,
            reset_target: None,
        },
        contracts::OperationId::new(),
    )?;
    update_instance(&pool, &deleting).await?;
    assert_eq!(
        resolve(&allowed, address, &request_for(&deleting))
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );

    update_instance(&pool, &reassigned).await?;
    drop(allowed);
    drop(rotated);
    drop(unregistered);
    drop(no_identity);
    shutdown
        .send(())
        .map_err(|()| "primary resolver shutdown receiver disappeared")?;
    server.await??;

    let (rotated_server_certificate, rotated_server_key) =
        leaf_certificate(&ca, "localhost", false)?;
    let (rotated_address, rotated_shutdown, rotated_server) = start_server(
        owner_resolver_router(resolver),
        &ca.pem(),
        &rotated_server_certificate,
        &rotated_server_key,
    )
    .await?;
    let rotated = mtls_client(&ca.pem(), &rotated_client_certificate, &rotated_client_key)?;
    assert_eq!(
        resolve(&rotated, rotated_address, &reassigned_request)
            .await?
            .status(),
        StatusCode::OK
    );

    pool.close().await;
    assert_eq!(
        resolve(&rotated, rotated_address, &reassigned_request)
            .await?
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );

    drop(rotated);
    rotated_shutdown
        .send(())
        .map_err(|()| "rotated resolver shutdown receiver disappeared")?;
    rotated_server.await??;
    let outage_client = mtls_client(&ca.pem(), &rotated_client_certificate, &rotated_client_key)?;
    assert!(
        resolve(&outage_client, rotated_address, &reassigned_request)
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn shutdown_future_failure_is_propagated_as_a_typed_server_error()
-> Result<(), Box<dyn std::error::Error>> {
    let ca = test_ca()?;
    let (server_certificate, server_key) = leaf_certificate(&ca, "localhost", false)?;
    let config = MtlsConfig::from_pem(
        ca.pem().as_bytes(),
        server_certificate.as_bytes(),
        server_key.as_bytes(),
    )?;
    let listener = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).await?;
    let result = serve_owner_resolver_mtls(listener, axum::Router::new(), config, async {
        Err(MtlsServerError::ShutdownSignal(std::io::Error::other(
            "injected signal registration failure",
        )))
    })
    .await;
    assert!(matches!(result, Err(MtlsServerError::ShutdownSignal(_))));
    Ok(())
}

async fn migrate(pool: &PgPool) -> Result<(), sqlx::Error> {
    let migration = format!(
        "CREATE SCHEMA environment; SET search_path TO environment;\n{}",
        include_str!("../../../migrations/environment/0001_sprint2_baseline.sql")
    );
    sqlx::raw_sql(&migration).execute(pool).await?;
    Ok(())
}

async fn insert_instance(
    pool: &PgPool,
    instance: &EnvironmentInstance,
) -> Result<(), Box<dyn std::error::Error>> {
    instance.validate()?;
    sqlx::query(
        "INSERT INTO environment.environment_instances \
         (environment_id, course_id, owner_actor_id, release_id, generation, observed_generation, desired_state, \
          observed_state, provider_binding, lease_id, revision, terminal_diagnostic, \
          eligibility_expires_at, contract) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)",
    )
    .bind(instance.id.as_uuid())
    .bind(instance.course_id.as_uuid())
    .bind(instance.owner_id.as_uuid())
    .bind(instance.release_id.as_uuid())
    .bind(i64::try_from(instance.generation)?)
    .bind(i64::try_from(instance.observed_generation)?)
    .bind(wire(&instance.desired_state)?)
    .bind(wire(&instance.observed_state)?)
    .bind(&instance.provider_binding)
    .bind(instance.lease_id.map(contracts::LeaseId::as_uuid))
    .bind(i64::try_from(instance.revision.get())?)
    .bind(&instance.last_diagnostic_code)
    .bind(instance.eligibility_expires_at.get())
    .bind(serde_json::to_value(instance)?)
    .execute(pool)
    .await?;
    Ok(())
}

async fn update_instance(
    pool: &PgPool,
    instance: &EnvironmentInstance,
) -> Result<(), Box<dyn std::error::Error>> {
    instance.validate()?;
    let result = sqlx::query(
        "UPDATE environment.environment_instances \
         SET generation=$2, observed_generation=$3, desired_state=$4, observed_state=$5, \
             revision=$6, eligibility_expires_at=$7, contract=$8, updated_at=now() \
         WHERE environment_id=$1",
    )
    .bind(instance.id.as_uuid())
    .bind(i64::try_from(instance.generation)?)
    .bind(i64::try_from(instance.observed_generation)?)
    .bind(wire(&instance.desired_state)?)
    .bind(wire(&instance.observed_state)?)
    .bind(i64::try_from(instance.revision.get())?)
    .bind(instance.eligibility_expires_at.get())
    .bind(serde_json::to_value(instance)?)
    .execute(pool)
    .await?;
    if result.rows_affected() != 1 {
        return Err("authoritative environment row disappeared".into());
    }
    Ok(())
}

fn wire<T: serde::Serialize>(value: &T) -> Result<String, Box<dyn std::error::Error>> {
    serde_json::to_value(value)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "wire enum did not serialize as a string".into())
}

fn request_for(instance: &EnvironmentInstance) -> EnvironmentOwnerResolutionRequest {
    EnvironmentOwnerResolutionRequest {
        environment_id: instance.id,
        course_id: instance.course_id,
        owner_actor_id: instance.owner_id,
        expected_revision: instance.revision,
    }
}

fn shift_minutes(
    timestamp: contracts::UtcTimestamp,
    minutes: i64,
) -> Result<contracts::UtcTimestamp, Box<dyn std::error::Error>> {
    let shifted = timestamp
        .get()
        .checked_add(time::Duration::minutes(minutes))
        .ok_or("timestamp shift overflow")?;
    Ok(contracts::UtcTimestamp::from_utc(shifted)?)
}

async fn resolve(
    client: &Client,
    address: SocketAddr,
    request: &EnvironmentOwnerResolutionRequest,
) -> Result<reqwest::Response, reqwest::Error> {
    client
        .post(format!(
            "https://localhost:{}/internal/v1/environments/{}/owner:resolve",
            address.port(),
            request.environment_id
        ))
        .json(request)
        .send()
        .await
}

fn mtls_client(
    ca_pem: &str,
    certificate_pem: &str,
    private_key_pem: &str,
) -> Result<Client, Box<dyn std::error::Error>> {
    let identity = format!("{certificate_pem}{private_key_pem}");
    Ok(Client::builder()
        .add_root_certificate(Certificate::from_pem(ca_pem.as_bytes())?)
        .identity(Identity::from_pem(identity.as_bytes())?)
        .build()?)
}

async fn start_server(
    router: axum::Router,
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
