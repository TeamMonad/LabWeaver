//! Real `PostgreSQL` owner-resolver integration coverage.
#![allow(
    clippy::too_many_lines,
    reason = "one end-to-end test keeps database and lifecycle evidence under one identity"
)]

mod support;

use std::net::SocketAddr;

use auth::ServiceIdentity;
use axum::Extension;
use contracts::environment::{
    EnvironmentInstance, EnvironmentOperationKind, EnvironmentOwnerResolutionRequest,
};
use contracts::{ActorId, CourseId};
use environment_service::{
    LifecycleCommand, OwnerResolver, PgEnvironmentStore, PgReleaseProjectionStore,
    authorize_owner_resolution, owner_resolver_router, plan_command,
};
use reqwest::{Client, StatusCode};
use sqlx::{PgPool, postgres::PgPoolOptions};
use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;
use time::OffsetDateTime;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

const OWNER_RESOLUTION_PERMISSION: &str = "environment.owner.resolve";

#[tokio::test]
async fn resolver_uses_real_postgres_and_owner_resolution_logic()
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
    support::apply_environment_migrations(&pool).await?;

    let mut authoritative = support::ready_instance();
    authoritative.eligibility_expires_at = support::timestamp("2030-07-15T00:00:00.000Z");
    insert_instance(&pool, &authoritative).await?;

    let store = PgEnvironmentStore::new(pool.clone());
    let resolver = OwnerResolver::new(store.clone(), PgReleaseProjectionStore::new(pool.clone()));
    let caller = ServiceIdentity {
        issuer: "https://keycloak.example.test/realms/workloads".to_owned(),
        subject: "service-account-access".to_owned(),
        client_id: "labweaver-access".to_owned(),
        expires_at: OffsetDateTime::now_utc() + time::Duration::hours(1),
        permissions: [OWNER_RESOLUTION_PERMISSION.to_owned()]
            .into_iter()
            .collect(),
    };
    let (address, shutdown, server) = start_server(owner_resolver_router(resolver), caller).await?;
    let client = reqwest::Client::new();

    let original_request = request_for(&authoritative);
    let original_response = resolve(&client, address, &original_request).await?;
    assert_eq!(original_response.status(), StatusCode::OK);
    assert_eq!(
        original_response
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|value| value.to_str().ok()),
        Some("\"rev-2\"")
    );

    let mut wrong_course = original_request.clone();
    wrong_course.course_id = Some(CourseId::new());
    assert_eq!(
        resolve(&client, address, &wrong_course).await?.status(),
        StatusCode::FORBIDDEN
    );
    let mut wrong_owner = original_request.clone();
    wrong_owner.owner_actor_id = ActorId::new();
    assert_eq!(
        resolve(&client, address, &wrong_owner).await?.status(),
        StatusCode::FORBIDDEN
    );
    let mut stale_revision = original_request.clone();
    stale_revision.expected_revision = support::revision(1);
    assert_eq!(
        resolve(&client, address, &stale_revision).await?.status(),
        StatusCode::FORBIDDEN
    );

    let mut reassigned = authoritative.clone();
    reassigned.owner_id = ActorId::new();
    reassigned.revision = support::revision(3);
    for endpoint in &mut reassigned.endpoints {
        endpoint.revision = reassigned.revision;
    }
    update_instance(&pool, &reassigned).await?;
    assert_eq!(
        resolve(&client, address, &original_request).await?.status(),
        StatusCode::FORBIDDEN
    );
    let reassigned_request = request_for(&reassigned);
    assert_eq!(
        resolve(&client, address, &reassigned_request)
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
        resolve(&client, address, &request_for(&expired))
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
        resolve(&client, address, &request_for(&deleting))
            .await?
            .status(),
        StatusCode::FORBIDDEN
    );

    // Verify that closing the database pool causes SERVICE_UNAVAILABLE.
    update_instance(&pool, &reassigned).await?;
    pool.close().await;
    assert_eq!(
        resolve(&client, address, &reassigned_request)
            .await?
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );

    shutdown
        .send(())
        .map_err(|()| "primary resolver shutdown receiver disappeared")?;
    server.await??;
    Ok(())
}

async fn insert_instance(
    pool: &PgPool,
    instance: &EnvironmentInstance,
) -> Result<(), Box<dyn std::error::Error>> {
    instance.validate()?;
    sqlx::query(
        "INSERT INTO environment.environment_instances \
         (environment_id, project_id, course_id, owner_actor_id, release_id, generation, observed_generation, desired_state, \
          observed_state, provider_binding, lease_id, revision, terminal_diagnostic, \
          eligibility_expires_at, contract) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)",
    )
    .bind(instance.id.as_uuid())
    .bind(instance.project_id.as_uuid())
    .bind(instance.course_id.map(contracts::CourseId::as_uuid))
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
        project_id: instance.project_id,
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
            "http://localhost:{}/internal/v1/environments/{}/owner:resolve",
            address.port(),
            request.environment_id
        ))
        .json(request)
        .send()
        .await
}

async fn start_server(
    router: axum::Router,
    caller: ServiceIdentity,
) -> Result<
    (
        SocketAddr,
        oneshot::Sender<()>,
        tokio::task::JoinHandle<Result<(), std::io::Error>>,
    ),
    Box<dyn std::error::Error>,
> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let router = router.layer(Extension(caller));
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await
    });
    Ok((address, shutdown_tx, server))
}
