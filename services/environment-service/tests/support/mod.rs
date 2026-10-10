#![allow(dead_code, clippy::panic)]

use std::{error::Error, path::Path, str::FromStr};

use async_trait::async_trait;
use contracts::authoring::{EnvironmentClass, RuntimeKind};
use contracts::environment::{
    ActivateEnvironmentResourceReservationRequest, ActivateEnvironmentResourceReservationResponse,
    DesiredEnvironmentState, EndpointHealth, EndpointProtocol, EnvironmentEndpoint,
    EnvironmentInstance, EnvironmentOperation, EnvironmentOperationKind,
    EnvironmentResourceReservationState, ObservedEnvironmentState, OperationState,
    ReleaseEnvironmentResourceReservationRequest, ResolveEnvironmentResourceReservationRequest,
    SuspendEnvironmentResourceReservationRequest, SuspendEnvironmentResourceReservationResponse,
};
use contracts::resource::{GpuAllocation, WorkloadResources};
use contracts::{
    ActorId, CourseId, EndpointId, EnvironmentId, OperationId, PolicyId, ProjectId, ReleaseId,
    RetentionClass, RetentionDisposition, RetentionSnapshot, Revision, UtcTimestamp,
};
use persistence_sqlx::{Domain, MigrationCatalog};
use sqlx::PgPool;

#[derive(Clone, Copy)]
pub struct TestResourceAllocator;

#[async_trait]
impl environment_service::ExperimentResourceAllocator for TestResourceAllocator {
    async fn resolve_resource_reservation(
        &self,
        request: &ResolveEnvironmentResourceReservationRequest,
    ) -> Result<Option<GpuAllocation>, environment_service::ResourceUsageClientError> {
        Ok(request
            .approved_resources
            .gpu
            .as_ref()
            .map(|gpu| GpuAllocation {
                entry_id: contracts::GpuCatalogEntryId::new(),
                class: gpu.class.clone(),
                count: gpu.count,
                mode: contracts::resource::GpuAllocationMode::Exclusive,
                provider_binding: request.provider_binding.clone(),
                allocation_binding: "test-resource".to_owned(),
                catalog_revision: Revision::new(1)
                    .unwrap_or_else(|error| unreachable!("test revision should parse: {error}")),
            }))
    }

    async fn release_resource_reservation(
        &self,
        _request: &ReleaseEnvironmentResourceReservationRequest,
    ) -> Result<bool, environment_service::ResourceUsageClientError> {
        Ok(true)
    }

    async fn activate_resource_reservation(
        &self,
        request: &ActivateEnvironmentResourceReservationRequest,
    ) -> Result<
        ActivateEnvironmentResourceReservationResponse,
        environment_service::ResourceUsageClientError,
    > {
        Ok(ActivateEnvironmentResourceReservationResponse {
            version: 1,
            environment_id: request.environment_id,
            state: EnvironmentResourceReservationState::Reserved,
            reservation_generation: 1,
            environment_generation: request.environment_generation,
            allocation: request.expected_allocation.clone(),
            applied: true,
        })
    }

    async fn suspend_resource_reservation(
        &self,
        request: &SuspendEnvironmentResourceReservationRequest,
    ) -> Result<
        SuspendEnvironmentResourceReservationResponse,
        environment_service::ResourceUsageClientError,
    > {
        Ok(SuspendEnvironmentResourceReservationResponse {
            version: 1,
            environment_id: request.environment_id,
            state: EnvironmentResourceReservationState::Suspended,
            reservation_generation: 1,
            environment_generation: request.environment_generation,
            allocation: None,
            applied: true,
        })
    }
}

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

pub fn finite_retention() -> RetentionSnapshot {
    RetentionSnapshot {
        policy_id: PolicyId::new(),
        policy_revision: Revision::new(1)
            .unwrap_or_else(|error| unreachable!("static revision should parse: {error}")),
        class: RetentionClass::CourseMaterial,
        retain_until: Some(timestamp("2027-07-15T00:00:00.000Z")),
        disposition: RetentionDisposition::Delete,
    }
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
        approved_resources: WorkloadResources {
            cpu_millicores: 1,
            memory_bytes: 1,
            storage_bytes: 1,
            gpu: None,
        },
        gpu_allocation: None,
        resource_reservation_released: false,
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
        eligibility_expires_at: Some(timestamp("2026-07-15T00:00:00.000Z")),
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
        approved_resources: WorkloadResources {
            cpu_millicores: 1,
            memory_bytes: 1,
            storage_bytes: 1,
            gpu: None,
        },
        gpu_allocation: None,
        resource_reservation_released: false,
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
        eligibility_expires_at: Some(timestamp("2026-07-15T00:00:00.000Z")),
        endpoints: Vec::new(),
        last_diagnostic_code: None,
        failed_phase: None,
        cleanup_evidence: None,
    }
}

pub fn revision(value: u64) -> Revision {
    Revision::new(value).unwrap_or_else(|error| panic!("invalid test revision: {error}"))
}
