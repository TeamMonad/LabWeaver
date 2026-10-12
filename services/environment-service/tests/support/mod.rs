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
    let mut instance = EnvironmentInstance {
        id: EnvironmentId::new(),
        display_label: "Ready environment".to_owned(),
        project_id: ProjectId::new(),
        course_id: Some(CourseId::new()),
        owner_id: ActorId::new(),
        class: EnvironmentClass::Experiment,
        runtime_authorization: None,
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
    };
    instance.runtime_authorization =
        Some(runtime_receipt(&instance, &runtime_projection(&instance)));
    instance
}

pub fn requested_instance() -> EnvironmentInstance {
    let accepted_at = timestamp("2026-07-14T00:00:00.000Z");
    let mut instance = EnvironmentInstance {
        id: EnvironmentId::new(),
        display_label: "Requested environment".to_owned(),
        project_id: ProjectId::new(),
        course_id: Some(CourseId::new()),
        owner_id: ActorId::new(),
        class: EnvironmentClass::Experiment,
        runtime_authorization: None,
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
    };
    instance.runtime_authorization =
        Some(runtime_receipt(&instance, &runtime_projection(&instance)));
    instance
}

pub fn revision(value: u64) -> Revision {
    Revision::new(value).unwrap_or_else(|error| panic!("invalid test revision: {error}"))
}

/// A synthetic immutable release for lifecycle tests whose provider is an external-boundary stub.
pub fn runtime_projection(instance: &EnvironmentInstance) -> contracts::events::ReleasePublished {
    use contracts::authoring::{CandidateApproval, CandidateDecision};
    use contracts::supply_chain::EnvironmentTemplateRelease;
    use contracts::supply_chain::ImageArtifact;
    let spec = serde_json::from_value(serde_json::json!({
        "apiVersion":"environment.labweaver.io/v1","kind":"EnvironmentSpec","name":"synthetic-runtime",
        "class":"experiment","resources":{"cpuMillicores":1,"memoryBytes":1,"storageBytes":1},
        "network":{"mode":"deny_all"},"entries":[{"name":"web","protocol":"http","servicePort":8080}],
        "security":{"publicExposurePolicy":"deny","securityProfileBinding":"synthetic-v1"},
        "runtime":{"kind":"container","provider_binding":"container-primary-v1","service_port":8080,
            "build_context":{"artifactId":"00000000-0000-7000-8000-000000000001","storeBinding":"synthetic","objectVersion":"one","sizeBytes":1,"mediaType":"application/octet-stream"}},
        "retention":{"policyId":"00000000-0000-7000-8000-000000000002","policyRevision":1,"class":"run_evidence","retainUntil":"2027-07-15T00:00:00.000Z","disposition":"delete"}
    })).unwrap_or_else(|e| panic!("synthetic spec: {e}"));
    let candidate_id = contracts::CandidateId::new();
    contracts::events::ReleasePublished {
        release: EnvironmentTemplateRelease {
            id: instance.release_id,
            project_id: instance.project_id,
            course_id: instance.course_id,
            version: instance.release_version,
            candidate_id,
            agent_run_id: contracts::AgentRunId::new(),
            candidate_revision: revision(1),
            runtime_kind: instance.runtime_kind,
            approval: CandidateApproval {
                id: contracts::ApprovalId::new(),
                candidate_id,
                candidate_revision: revision(1),
                policy_revision: revision(1),
                trust_revision: revision(1),
                actor_id: instance.owner_id,
                decision: CandidateDecision::Approved,
                reason: "synthetic".to_owned(),
                decided_at: instance.operation.accepted_at,
            },
            artifact: ImageArtifact::Container {
                id: contracts::ImageArtifactId::new(),
                build_request_id: contracts::BuildRequestId::new(),
                repository: "synthetic/runtime".to_owned(),
                digest: format!("sha256:{}", "0".repeat(64)),
            },
            published_by: instance.owner_id,
            published_at: instance.operation.accepted_at,
        },
        environment_spec: spec,
    }
}

pub fn runtime_receipt(
    instance: &EnvironmentInstance,
    projection: &contracts::events::ReleasePublished,
) -> contracts::environment::EnvironmentRuntimeAuthorizationResolution {
    use contracts::{
        platform_configuration::{RuntimePreauthorization, RuntimePreauthorizationScope},
        runtime_authorization::{
            RuntimeAdmissionBasis, RuntimeAuthorizationBinding, RuntimeConfigurationIdentity,
        },
    };
    let lease = instance
        .operation
        .lease_authorization
        .clone()
        .unwrap_or_else(|| contracts::environment::EnvironmentLeaseAuthorization {
            resource_request_id: contracts::ResourceRequestId::new(),
            lease_id: instance.lease_id.unwrap_or_else(contracts::LeaseId::new),
            lease_revision: revision(1),
            environment_id: instance.id,
            project_id: instance.project_id,
            course_id: instance.course_id,
            owner_actor_id: instance.owner_id,
            capacity_binding: instance.id.to_string(),
            approved_resources: instance.approved_resources.clone(),
            gpu_allocation: instance.gpu_allocation.clone(),
            active_from: timestamp("2026-07-14T00:00:00.000Z"),
            expires_at: timestamp("2027-07-15T00:00:00.000Z"),
        });
    contracts::environment::EnvironmentRuntimeAuthorizationResolution {
        provider_binding: instance.provider_binding.clone(),
        valid_until: lease.expires_at,
        authorization: RuntimeAuthorizationBinding {
            request_id: lease.resource_request_id,
            project_id: instance.project_id,
            course_id: instance.course_id,
            request_generation: 1,
            target: contracts::resource::ResourceTarget::Environment {
                environment_id: instance.id,
                release_id: instance.release_id,
                release_version: instance.release_version,
            },
            configuration: RuntimeConfigurationIdentity {
                authoring_project_id: projection.release.project_id,
                course_id: projection.release.course_id,
                candidate_id: projection.release.candidate_id,
                candidate_revision: projection.release.candidate_revision,
                configuration_sha256: persistence_sqlx::Sha256Digest::of_canonical(
                    &projection.environment_spec,
                )
                .unwrap_or_else(|e| panic!("synthetic SHA: {e}"))
                .to_string(),
                authoring_approval_id: None,
                teacher_scope: None,
            },
            platform_configuration_revision: revision(1),
            platform_configuration_sha256: "0".repeat(64),
            permissions: RuntimePreauthorization {
                scope: RuntimePreauthorizationScope::Project {
                    project_id: instance.project_id,
                },
                capabilities: projection.environment_spec.security.capabilities.clone(),
                privileged: projection.environment_spec.security.privileged,
            },
            basis: RuntimeAdmissionBasis::RestrictedBaseline,
        },
        lease_authorization: lease,
        owner_actor_id: instance.owner_id,
        default_container_uid: 0,
        privileged_execution_location: None,
    }
}

#[derive(Clone)]
pub struct RuntimeFixtureReleases(pub EnvironmentInstance);
#[async_trait]
impl environment_service::ContainerReleaseResolver for RuntimeFixtureReleases {
    async fn resolve(
        &self,
        id: contracts::ReleaseId,
        version: u64,
    ) -> Result<
        environment_service::ResolvedContainerRelease,
        environment_service::ReleaseProjectionError,
    > {
        let mut instance = self.0.clone();
        instance.release_id = id;
        instance.release_version = version;
        Ok(environment_service::ResolvedContainerRelease {
            projection: runtime_projection(&instance),
            authority_now: instance.operation.accepted_at,
            withdrawn_at: None,
        })
    }
}

#[derive(Clone)]
pub struct RuntimeStoredReleases(pub PgPool);
#[async_trait]
impl environment_service::ContainerReleaseResolver for RuntimeStoredReleases {
    async fn resolve(
        &self,
        id: contracts::ReleaseId,
        version: u64,
    ) -> Result<
        environment_service::ResolvedContainerRelease,
        environment_service::ReleaseProjectionError,
    > {
        let contract:serde_json::Value=sqlx::query_scalar("SELECT contract FROM environment.environment_instances WHERE contract->>'releaseId'=$1 AND (contract->>'releaseVersion')::bigint=$2 LIMIT 1").bind(id.to_string()).bind(i64::try_from(version).map_err(|_| environment_service::ReleaseProjectionError::IdentityMismatch)?).fetch_one(&self.0).await?;
        let instance: EnvironmentInstance = serde_json::from_value(contract)
            .map_err(|_| environment_service::ReleaseProjectionError::ContractInvalid)?;
        RuntimeFixtureReleases(instance).resolve(id, version).await
    }
}

/// Resource is the mocked external boundary; real approval/claim validation belongs to Resource integration tests.
#[derive(Clone)]
pub struct RuntimeFixtureAuthority;
#[async_trait]
impl environment_service::EnvironmentRuntimeAuthority for RuntimeFixtureAuthority {
    async fn resolve_runtime_authorization(
        &self,
        request: &contracts::environment::ResolveEnvironmentRuntimeAuthorizationRequest,
    ) -> Result<
        contracts::environment::EnvironmentRuntimeAuthorizationResolution,
        environment_service::ResourceUsageClientError,
    > {
        let mut instance = requested_instance();
        instance.id = request
            .environment_id
            .ok_or(environment_service::ResourceUsageClientError::InvalidResponse)?;
        instance.project_id = request.project_id;
        instance.course_id = request.course_id;
        instance.owner_id = request.owner_actor_id;
        instance.release_id = request.release_id;
        instance.release_version = request.release_version;
        instance.operation.lease_authorization = None;
        instance.lease_id = request.expected_lease_id;
        let mut receipt = runtime_receipt(&instance, &runtime_projection(&instance));
        receipt.authorization.request_id = request.resource_request_id;
        receipt.lease_authorization.resource_request_id = request.resource_request_id;
        if let Some(id) = request.expected_lease_id {
            receipt.lease_authorization.lease_id = id;
        }
        Ok(receipt)
    }
    async fn resolve_credit_grant(
        &self,
        lease: &contracts::environment::EnvironmentLeaseAuthorization,
    ) -> Result<
        contracts::resource::environment_funding::EnvironmentCreditGrant,
        environment_service::ResourceUsageClientError,
    > {
        credit_grant_for_lease(lease)
    }
}

/// Explicit zero-price external Resource fixture; financial behavior is exercised with the real store separately.
pub fn credit_grant_for_lease(
    lease: &contracts::environment::EnvironmentLeaseAuthorization,
) -> Result<
    contracts::resource::environment_funding::EnvironmentCreditGrant,
    environment_service::ResourceUsageClientError,
> {
    use contracts::resource::{
        FixedDecimal, Money,
        costing::{ResourceCostEstimate, ResourceCostEstimateRequest},
        credits::{CreditHold, CreditHoldState},
    };
    Ok(
        contracts::resource::environment_funding::EnvironmentCreditGrant {
            request_id: lease.resource_request_id,
            lease_id: lease.lease_id,
            lease_revision: lease.lease_revision,
            claim_id: lease
                .capacity_binding
                .parse()
                .map_err(|_| environment_service::ResourceUsageClientError::InvalidResponse)?,
            approval_id: contracts::ResourceApprovalId::new(),
            covered_from: lease.active_from,
            covered_until: lease.expires_at,
            can_authorize_execution: true,
            hold: CreditHold {
                id: uuid::Uuid::new_v4(),
                project_id: lease.project_id,
                request_id: Some(lease.resource_request_id),
                build_request_id: None,
                build_attempt: None,
                business_key: "synthetic-zero-price".into(),
                owner_client_id: "synthetic-environment".into(),
                amount: FixedDecimal::zero(),
                remaining: FixedDecimal::zero(),
                state: CreditHoldState::Pending,
                measurement_unknown: false,
                reason: None,
                created_at: lease.active_from,
                unknown_since: None,
                reconciliation_due_at: None,
                expired_unknown_at: None,
            },
            estimate: ResourceCostEstimate {
                project_id: lease.project_id,
                input: ResourceCostEstimateRequest {
                    resources: lease.approved_resources.clone(),
                    compute_from: lease.active_from,
                    compute_until: lease.expires_at,
                    storage_from: lease.active_from,
                    storage_until: lease.expires_at,
                },
                lines: vec![],
                total: Money {
                    amount: FixedDecimal::zero(),
                    currency: "CREDITS".into(),
                },
            },
        },
    )
}

pub fn funded_instance(mut instance: EnvironmentInstance) -> EnvironmentInstance {
    instance.operation.lease_authorization = None;
    let receipt = runtime_receipt(&instance, &runtime_projection(&instance));
    instance.lease_id = Some(receipt.lease_authorization.lease_id);
    instance.capacity_binding = Some(receipt.lease_authorization.capacity_binding.clone());
    instance.operation.lease_authorization = Some(receipt.lease_authorization.clone());
    instance.runtime_authorization = Some(receipt);
    instance
}

pub fn runtime_admission(
    instance: &EnvironmentInstance,
) -> environment_service::EnvironmentRuntimeAdmission {
    environment_service::EnvironmentRuntimeAdmission::new(
        RuntimeFixtureReleases(instance.clone()),
        std::sync::Arc::new(RuntimeFixtureAuthority),
    )
}

pub fn stored_runtime_admission(pool: PgPool) -> environment_service::EnvironmentRuntimeAdmission {
    environment_service::EnvironmentRuntimeAdmission::new(
        RuntimeStoredReleases(pool),
        std::sync::Arc::new(RuntimeFixtureAuthority),
    )
}
