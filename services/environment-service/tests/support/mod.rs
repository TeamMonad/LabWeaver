#![allow(dead_code, clippy::panic)]

use std::{error::Error, path::Path, str::FromStr};

use contracts::authoring::{EnvironmentClass, RuntimeKind};
use contracts::environment::{
    DesiredEnvironmentState, EndpointHealth, EndpointProtocol, EnvironmentEndpoint,
    EnvironmentInstance, EnvironmentOperation, EnvironmentOperationKind, ObservedEnvironmentState,
    OperationState,
};
use contracts::{
    ActorId, CourseId, EndpointId, EnvironmentId, OperationId, ProjectId, ReleaseId, Revision,
    UtcTimestamp,
};
use persistence_sqlx::{Domain, MigrationCatalog};
use sqlx::PgPool;

/// Creates the Environment schema from every migration declared in the repository catalog.
///
/// Integration tests must use the same ordered and hash-checked SQL as the service migration
/// coordinator so newly added tables cannot silently be absent from a test database.
pub async fn apply_environment_migrations(pool: &PgPool) -> Result<(), Box<dyn Error>> {
    sqlx::query("CREATE SCHEMA environment")
        .execute(pool)
        .await?;
    let mut connection = pool.acquire().await?;
    sqlx::query("SET search_path = environment, pg_catalog")
        .execute(&mut *connection)
        .await?;
    let migration_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
    let catalog = MigrationCatalog::load(&migration_root.join("catalog.yaml"))?;
    let environment_migrations = catalog
        .domains
        .iter()
        .find(|domain| domain.name == Domain::Environment)
        .ok_or_else(|| std::io::Error::other("migration catalog has no environment domain"))?;
    for migration in &environment_migrations.migrations {
        let sql = MigrationCatalog::read_verified_sql(&migration_root, migration)?;
        sqlx::raw_sql(&sql).execute(&mut *connection).await?;
    }
    Ok(())
}

pub fn timestamp(value: &str) -> UtcTimestamp {
    UtcTimestamp::from_str(value).unwrap_or_else(|error| panic!("invalid test timestamp: {error}"))
}

pub fn ready_instance() -> EnvironmentInstance {
    let accepted_at = timestamp("2026-07-14T00:00:00.000Z");
    EnvironmentInstance {
        id: EnvironmentId::new(),
        display_label: "Ready environment".to_owned(),
        project_id: ProjectId::new(),
        course_id: Some(CourseId::new()),
        owner_id: ActorId::new(),
        class: EnvironmentClass::Experiment,
        runtime_kind: RuntimeKind::Container,
        release_id: ReleaseId::new(),
        release_version: 1,
        lease_id: None,
        capacity_binding: None,
        provider_binding: "container-primary-v1".to_owned(),
        desired_state: DesiredEnvironmentState::Running,
        observed_state: ObservedEnvironmentState::Ready,
        revision: revision(2),
        generation: 1,
        observed_generation: 1,
        operation: EnvironmentOperation {
            id: OperationId::new(),
            kind: EnvironmentOperationKind::Create,
            state: OperationState::Succeeded,
            accepted_revision: revision(1),
            attempt: 1,
            provider_step: 1,
            max_attempts: 3,
            next_attempt_at: accepted_at,
            actor_id: ActorId::new(),
            trace_id: "11111111111111111111111111111111".to_owned(),
            accepted_at,
            deadline_at: timestamp("2026-07-14T00:10:00.000Z"),
            cleanup_started_at: None,
            diagnostic_code: None,
            preserve_mutable_disk: false,
            access_revocation_revision: None,
            retry_from_phase: None,
            reset_target: None,
            lease_authorization: None,
        },
        eligibility_expires_at: timestamp("2026-07-15T00:00:00.000Z"),
        endpoints: vec![EnvironmentEndpoint {
            id: EndpointId::new(),
            protocol: EndpointProtocol::Https,
            revision: revision(2),
            health: EndpointHealth::Healthy,
            observed_at: timestamp("2026-07-14T00:01:00.000Z"),
        }],
        last_diagnostic_code: None,
        failed_phase: None,
        cleanup_evidence: None,
    }
}

pub fn requested_instance() -> EnvironmentInstance {
    let accepted_at = timestamp("2026-07-14T00:00:00.000Z");
    EnvironmentInstance {
        id: EnvironmentId::new(),
        display_label: "Requested environment".to_owned(),
        project_id: ProjectId::new(),
        course_id: Some(CourseId::new()),
        owner_id: ActorId::new(),
        class: EnvironmentClass::Experiment,
        runtime_kind: RuntimeKind::Container,
        release_id: ReleaseId::new(),
        release_version: 1,
        lease_id: None,
        capacity_binding: None,
        provider_binding: "container-primary-v1".to_owned(),
        desired_state: DesiredEnvironmentState::Running,
        observed_state: ObservedEnvironmentState::Requested,
        revision: revision(1),
        generation: 1,
        observed_generation: 0,
        operation: EnvironmentOperation {
            id: OperationId::new(),
            kind: EnvironmentOperationKind::Create,
            state: OperationState::Accepted,
            accepted_revision: revision(1),
            attempt: 1,
            provider_step: 1,
            max_attempts: 3,
            next_attempt_at: accepted_at,
            actor_id: ActorId::new(),
            trace_id: "22222222222222222222222222222222".to_owned(),
            accepted_at,
            deadline_at: timestamp("2026-07-14T00:10:00.000Z"),
            cleanup_started_at: None,
            diagnostic_code: None,
            preserve_mutable_disk: false,
            access_revocation_revision: None,
            retry_from_phase: None,
            reset_target: None,
            lease_authorization: None,
        },
        eligibility_expires_at: timestamp("2026-07-15T00:00:00.000Z"),
        endpoints: Vec::new(),
        last_diagnostic_code: None,
        failed_phase: None,
        cleanup_evidence: None,
    }
}

pub fn revision(value: u64) -> Revision {
    Revision::new(value).unwrap_or_else(|error| panic!("invalid test revision: {error}"))
}
