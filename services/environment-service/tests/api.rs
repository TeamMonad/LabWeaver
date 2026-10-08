//! Public Environment HTTP behavior against the real `PostgreSQL` store.
#![allow(clippy::too_many_lines)]

mod support;

use std::{
    collections::BTreeSet,
    error::Error,
    str::FromStr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{HeaderValue, Method, Request, StatusCode, header},
};
use contracts::environment::{
    ActivateEnvironmentResourceReservationRequest, ActivateEnvironmentResourceReservationResponse,
    EndpointHealth, EndpointProtocol, EnvironmentEndpoint, EnvironmentInstance,
    EnvironmentOperationKind, EnvironmentResourceReservationState,
    ReleaseEnvironmentResourceReservationRequest, ResolveEnvironmentResourceReservationRequest,
    SuspendEnvironmentResourceReservationRequest, SuspendEnvironmentResourceReservationResponse,
};
use contracts::events::ReleasePublished;
use contracts::http::{SnapshotPage, StrongEtag};
use contracts::resource::{GpuAllocation, GpuAllocationMode};
use contracts::{
    ActorId, AgentRunId, ApprovalId, BuildRequestId, CandidateId, EndpointId, EnvironmentId,
    ImageArtifactId, OperationId, PolicyId, ProjectId, ReleaseId, Revision, UtcTimestamp,
};
use environment_service::{
    EnvironmentApiState, ExperimentResourceAllocator, FreezeBindingConfiguration,
    FreezeBindingService, NatsAccessRevoker, NatsResourceLeaseVerifier, PgEnvironmentStore,
    PgReleaseProjectionStore, ProviderObservation, ResourceUsageClientError,
    WorkAdmissionClientError, WorkAdmissionResolver, apply_provider_failure,
    apply_provider_observation, environment_api_router,
};
use futures_util::StreamExt;
use persistence_sqlx::Sha256Digest;
use serde_json::{Value, json};
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::{GenericImage, ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;
use time::OffsetDateTime;
use tokio::task::JoinHandle;
use tower::ServiceExt as _;

use support::{requested_instance, revision, timestamp};

const ACCESS_REVOKE_SUBJECT: &str = "test.access.environment.revoke.v1";
const LEASE_VERIFY_SUBJECT: &str = "test.resource.lease.verify.v1";

#[derive(Clone, Copy)]
struct UnusedAdmission;

#[async_trait]
impl WorkAdmissionResolver for UnusedAdmission {
    async fn resolve(
        &self,
        _run_id: AgentRunId,
        _query: &contracts::http::WorkConfigurationAdmissionQuery,
        _now: UtcTimestamp,
    ) -> Result<contracts::http::WorkConfigurationAdmissionBinding, WorkAdmissionClientError> {
        Err(WorkAdmissionClientError::Configuration)
    }
}

#[derive(Clone)]
struct CountingGpuAllocator {
    resolve_count: Arc<AtomicUsize>,
    release_count: Arc<AtomicUsize>,
    allocation: GpuAllocation,
}

#[async_trait]
impl ExperimentResourceAllocator for CountingGpuAllocator {
    async fn resolve_resource_reservation(
        &self,
        request: &ResolveEnvironmentResourceReservationRequest,
    ) -> Result<Option<GpuAllocation>, ResourceUsageClientError> {
        let count = self.resolve_count.fetch_add(1, Ordering::SeqCst) + 1;
        if count > 1 {
            return Err(ResourceUsageClientError::Rejected);
        }
        let gpu = request
            .gpu
            .as_ref()
            .ok_or(ResourceUsageClientError::InvalidResponse)?;
        if gpu.class != self.allocation.class
            || gpu.count != self.allocation.count
            || request.provider_binding != self.allocation.provider_binding
        {
            return Err(ResourceUsageClientError::InvalidResponse);
        }
        Ok(Some(self.allocation.clone()))
    }

    async fn release_resource_reservation(
        &self,
        request: &ReleaseEnvironmentResourceReservationRequest,
    ) -> Result<bool, ResourceUsageClientError> {
        self.release_count.fetch_add(1, Ordering::SeqCst);
        let _ = request;
        Ok(true)
    }

    async fn activate_resource_reservation(
        &self,
        request: &ActivateEnvironmentResourceReservationRequest,
    ) -> Result<ActivateEnvironmentResourceReservationResponse, ResourceUsageClientError> {
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
    ) -> Result<SuspendEnvironmentResourceReservationResponse, ResourceUsageClientError> {
        Ok(SuspendEnvironmentResourceReservationResponse {
            version: 1,
            environment_id: request.environment_id,
            state: EnvironmentResourceReservationState::Suspended,
            reservation_generation: 1,
            environment_generation: request.environment_generation,
            allocation: Some(self.allocation.clone()),
            applied: true,
        })
    }
}

#[tokio::test]
async fn public_create_replay_does_not_resolve_gpu_or_claim_capacity_twice()
-> Result<(), Box<dyn Error>> {
    let postgres = Postgres::default().with_tag("17.5-alpine").start().await?;
    let database_url = format!(
        "postgres://postgres:postgres@127.0.0.1:{}/postgres",
        postgres.get_host_port_ipv4(5432).await?
    );
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(6)
        .connect(&database_url)
        .await?;
    support::apply_environment_migrations(&pool).await?;

    let nats = GenericImage::new("nats", "2.11.8-alpine")
        .with_exposed_port(4222.tcp())
        .with_wait_for(WaitFor::message_on_stderr("Server is ready"))
        .start()
        .await?;
    let nats_url = format!("nats://127.0.0.1:{}", nats.get_host_port_ipv4(4222).await?);
    let nats_client = async_nats::connect(nats_url).await?;

    let project_id = ProjectId::new();
    let course_id = Some(contracts::CourseId::new());
    let projection = gpu_release_projection(project_id, course_id)?;
    sqlx::query(
        "INSERT INTO environment.release_projections
         (release_id,project_id,course_id,release_version,provider_binding,projection_sha256,contract,projected_event_id,aggregate_sequence)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,1)",
    )
    .bind(projection.release.id.as_uuid())
    .bind(project_id.as_uuid())
    .bind(course_id.map(contracts::CourseId::as_uuid))
    .bind(i64::try_from(projection.release.version)?)
    .bind("container-primary-v1")
    .bind(Sha256Digest::of_bytes(b"gpu-release-test").to_string())
    .bind(serde_json::to_value(&projection)?)
    .bind(uuid::Uuid::now_v7())
    .execute(&pool)
    .await?;

    let store = PgEnvironmentStore::new(pool.clone());
    let releases = PgReleaseProjectionStore::new(pool.clone());
    let resolve_count = Arc::new(AtomicUsize::new(0));
    let release_count = Arc::new(AtomicUsize::new(0));
    let allocator = CountingGpuAllocator {
        resolve_count: Arc::clone(&resolve_count),
        release_count: Arc::clone(&release_count),
        allocation: GpuAllocation {
            entry_id: contracts::GpuCatalogEntryId::new(),
            class: "a100".to_owned(),
            count: 1,
            mode: GpuAllocationMode::Exclusive,
            provider_binding: "container-primary-v1".to_owned(),
            allocation_binding: "capacity-a100-0".to_owned(),
            catalog_revision: revision(1),
        },
    };
    let api = environment_api_router(
        EnvironmentApiState::new(
            store,
            releases.clone(),
            NatsAccessRevoker::new(
                ACCESS_REVOKE_SUBJECT.to_owned(),
                nats_client.clone(),
                Duration::from_secs(2),
            )?,
            NatsResourceLeaseVerifier::new(
                LEASE_VERIFY_SUBJECT.to_owned(),
                nats_client,
                Duration::from_secs(2),
            )?,
            FreezeBindingService::new_with_admission_resolver(
                pool.clone(),
                releases.clone(),
                FreezeBindingConfiguration {
                    container_workspace_storage_class: "test-storage".to_owned(),
                    vm: None,
                },
                UnusedAdmission,
            )?,
        )
        .with_resource_reservations(allocator),
    );
    let identity = access_identity();
    let actor_id = ActorId::new();
    let session_id = uuid::Uuid::now_v7();
    let body = json!({
        "projectId": project_id,
        "courseId": course_id,
        "releaseId": projection.release.id,
        "releaseVersion": projection.release.version,
        "displayLabel": "GPU replay"
    });

    let first = send(
        &api,
        Method::POST,
        "/api/v1/environments".to_owned(),
        &identity,
        actor_id,
        session_id,
        None,
        Some(body.clone()),
        Some("api-create-gpu-replay"),
    )
    .await?;
    assert_eq!(first.0, StatusCode::ACCEPTED);
    let replay = send(
        &api,
        Method::POST,
        "/api/v1/environments".to_owned(),
        &identity,
        actor_id,
        uuid::Uuid::now_v7(),
        None,
        Some(body.clone()),
        Some("api-create-gpu-replay"),
    )
    .await?;
    assert_eq!(replay.0, StatusCode::ACCEPTED);
    assert_eq!(replay.1["operationId"], first.1["operationId"]);
    assert_eq!(replay.1["environmentId"], first.1["environmentId"]);
    assert_eq!(resolve_count.load(Ordering::SeqCst), 1);
    assert_eq!(release_count.load(Ordering::SeqCst), 0);

    let mut changed_label = body.clone();
    changed_label["displayLabel"] = json!("different label");
    let conflict = send(
        &api,
        Method::POST,
        "/api/v1/environments".to_owned(),
        &identity,
        actor_id,
        uuid::Uuid::now_v7(),
        None,
        Some(changed_label),
        Some("api-create-gpu-replay"),
    )
    .await?;
    assert_eq!(conflict.0, StatusCode::CONFLICT);
    assert_eq!(resolve_count.load(Ordering::SeqCst), 1);

    let other_actor = send(
        &api,
        Method::POST,
        "/api/v1/environments".to_owned(),
        &identity,
        ActorId::new(),
        uuid::Uuid::now_v7(),
        None,
        Some(body),
        Some("api-create-gpu-replay"),
    )
    .await?;
    assert_eq!(other_actor.0, StatusCode::CONFLICT);
    assert_eq!(resolve_count.load(Ordering::SeqCst), 1);
    Ok(())
}

#[tokio::test]
async fn public_operation_and_lifecycle_routes_preserve_scope_cursor_and_status_contracts()
-> Result<(), Box<dyn Error>> {
    let postgres = Postgres::default().with_tag("17.5-alpine").start().await?;
    let database_url = format!(
        "postgres://postgres:postgres@127.0.0.1:{}/postgres",
        postgres.get_host_port_ipv4(5432).await?
    );
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(6)
        .connect(&database_url)
        .await?;
    support::apply_environment_migrations(&pool).await?;

    let nats = GenericImage::new("nats", "2.11.8-alpine")
        .with_exposed_port(4222.tcp())
        .with_wait_for(WaitFor::message_on_stderr("Server is ready"))
        .start()
        .await?;
    let nats_url = format!("nats://127.0.0.1:{}", nats.get_host_port_ipv4(4222).await?);
    let nats_client = async_nats::connect(nats_url).await?;
    let (revocation_responder, revocation_count) =
        spawn_revocation_responder(nats_client.clone()).await?;

    let releases = PgReleaseProjectionStore::new(pool.clone());
    let api = environment_api_router(EnvironmentApiState::new(
        PgEnvironmentStore::new(pool.clone()),
        releases.clone(),
        NatsAccessRevoker::new(
            ACCESS_REVOKE_SUBJECT.to_owned(),
            nats_client.clone(),
            Duration::from_secs(2),
        )?,
        NatsResourceLeaseVerifier::new(
            LEASE_VERIFY_SUBJECT.to_owned(),
            nats_client,
            Duration::from_secs(2),
        )?,
        FreezeBindingService::new_with_admission_resolver(
            pool.clone(),
            releases,
            FreezeBindingConfiguration {
                container_workspace_storage_class: "test-storage".to_owned(),
                vm: None,
            },
            UnusedAdmission,
        )?,
    ));

    let identity = access_identity();
    let session_id = uuid::Uuid::now_v7();
    let store = PgEnvironmentStore::new(pool.clone());
    let mut first = requested_instance();
    first.project_id = ProjectId::new();
    first.course_id = None;
    first.owner_id = ActorId::new();
    first.operation.actor_id = first.owner_id;
    let mut second = first.clone();
    second.id = EnvironmentId::new();
    second.operation.id = OperationId::new();
    second.display_label = "Second public environment".to_owned();
    store.create("api-create-first", &first).await?;
    store.create("api-create-second", &second).await?;
    let first_ready = converge_to_ready(&store, first.id).await?;

    let stop_accepted = store
        .accept_command(
            "api-seed-stop",
            &contracts::environment::EnvironmentLifecycleCommand {
                environment_id: first.id,
                kind: EnvironmentOperationKind::Stop,
                expected_revision: first_ready.revision,
                actor_id: first.owner_id,
                trace_id: "api-seed-stop".to_owned(),
                accepted_at: timestamp("2026-07-14T00:01:00.000Z"),
                deadline_at: timestamp("2027-07-14T00:01:00.000Z"),
                access_revocation_revision: Some(revision(4)),
                preserve_mutable_disk: true,
                max_attempts: 3,
                reset_target: None,
            },
        )
        .await?;

    let operations_uri = format!("/api/v1/environments/{}/operations", first.id);
    let first_page = send(
        &api,
        Method::GET,
        format!("{operations_uri}?limit=1"),
        &identity,
        first.owner_id,
        session_id,
        None,
        None,
        None,
    )
    .await?;
    assert_eq!(first_page.0, StatusCode::OK);
    let first_page_body: SnapshotPage<Value> = serde_json::from_value(first_page.1.clone())?;
    assert_eq!(first_page_body.items.len(), 1);
    let next_cursor = first_page_body
        .next_cursor
        .clone()
        .ok_or("operation page should have a next cursor")?;
    let second_page = send(
        &api,
        Method::GET,
        format!("{operations_uri}?limit=1&cursor={next_cursor}"),
        &identity,
        first.owner_id,
        session_id,
        None,
        None,
        None,
    )
    .await?;
    assert_eq!(second_page.0, StatusCode::OK);
    let second_page_body: SnapshotPage<Value> = serde_json::from_value(second_page.1.clone())?;
    assert_eq!(second_page_body.items.len(), 1);
    let create_operation_id = first.operation.id.to_string();
    let stop_operation_id = stop_accepted.operation_id.to_string();
    let listed_ids = [
        first_page_body.items[0]["operationId"].as_str(),
        second_page_body.items[0]["operationId"].as_str(),
    ];
    assert!(listed_ids.contains(&Some(create_operation_id.as_str())));
    assert!(listed_ids.contains(&Some(stop_operation_id.as_str())));

    let filtered = send(
        &api,
        Method::GET,
        format!("{operations_uri}?kind=stop&state=accepted"),
        &identity,
        first.owner_id,
        session_id,
        None,
        None,
        None,
    )
    .await?;
    assert_eq!(filtered.0, StatusCode::OK);
    let filtered_body: SnapshotPage<Value> = serde_json::from_value(filtered.1)?;
    assert_eq!(filtered_body.items.len(), 1);
    assert_eq!(
        filtered_body.items[0]["operationId"],
        stop_accepted.operation_id.to_string()
    );

    let cursor_scope_mismatch = send(
        &api,
        Method::GET,
        format!("{operations_uri}?state=accepted&limit=1&cursor={next_cursor}"),
        &identity,
        first.owner_id,
        session_id,
        None,
        None,
        None,
    )
    .await?;
    assert_eq!(cursor_scope_mismatch.0, StatusCode::BAD_REQUEST);
    let malformed_cursor = send(
        &api,
        Method::GET,
        format!("{operations_uri}?cursor=bad%24"),
        &identity,
        first.owner_id,
        session_id,
        None,
        None,
        None,
    )
    .await?;
    assert_eq!(malformed_cursor.0, StatusCode::BAD_REQUEST);
    let invalid_limit = send(
        &api,
        Method::GET,
        format!("{operations_uri}?limit=101"),
        &identity,
        first.owner_id,
        session_id,
        None,
        None,
        None,
    )
    .await?;
    assert_eq!(invalid_limit.0, StatusCode::BAD_REQUEST);

    let create_get = send(
        &api,
        Method::GET,
        format!("{operations_uri}/{}", first.operation.id),
        &identity,
        first.owner_id,
        session_id,
        None,
        None,
        None,
    )
    .await?;
    assert_eq!(create_get.0, StatusCode::OK);
    assert_eq!(create_get.1["operationId"], first.operation.id.to_string());
    assert_eq!(create_get.1["kind"], "create");
    let unknown_operation = send(
        &api,
        Method::GET,
        format!("{operations_uri}/{}", OperationId::new()),
        &identity,
        first.owner_id,
        session_id,
        None,
        None,
        None,
    )
    .await?;
    assert_eq!(unknown_operation.0, StatusCode::NOT_FOUND);
    let wrong_actor = send(
        &api,
        Method::GET,
        operations_uri.clone(),
        &identity,
        ActorId::new(),
        session_id,
        None,
        None,
        None,
    )
    .await?;
    assert_eq!(wrong_actor.0, StatusCode::FORBIDDEN);

    let inventory_uri = format!(
        "/api/v1/environments?projectId={}&limit=1",
        first.project_id
    );
    let inventory = send(
        &api,
        Method::GET,
        inventory_uri.clone(),
        &identity,
        first.owner_id,
        session_id,
        None,
        None,
        None,
    )
    .await?;
    assert_eq!(inventory.0, StatusCode::OK);
    assert_eq!(inventory.1["items"].as_array().map(Vec::len), Some(1));
    let inventory_cursor = inventory.1["nextCursor"]
        .as_str()
        .ok_or("inventory page should have a next cursor")?;
    let inventory_second = send(
        &api,
        Method::GET,
        format!("{inventory_uri}&cursor={inventory_cursor}"),
        &identity,
        first.owner_id,
        session_id,
        None,
        None,
        None,
    )
    .await?;
    assert_eq!(inventory_second.0, StatusCode::OK);
    assert_eq!(
        inventory_second.1["items"].as_array().map(Vec::len),
        Some(1)
    );

    let cancel = send(
        &api,
        Method::POST,
        format!("/api/v1/environments/{}/cancel", first.id),
        &identity,
        first.owner_id,
        session_id,
        Some(stop_accepted.revision),
        None,
        Some("api-cancel-first"),
    )
    .await?;
    assert_eq!(cancel.0, StatusCode::ACCEPTED);
    let cancel_operation_id = OperationId::from_str(
        cancel.1["operationId"]
            .as_str()
            .ok_or("cancel operation id missing")?,
    )?;
    let cancel_status_url = cancel.1["statusUrl"]
        .as_str()
        .ok_or("cancel status URL missing")?;
    assert_eq!(
        cancel_status_url,
        format!(
            "/api/v1/environments/{}/operations/{}",
            first.id, cancel_operation_id
        )
    );
    let cancel_status = send(
        &api,
        Method::GET,
        cancel_status_url.to_owned(),
        &identity,
        first.owner_id,
        session_id,
        None,
        None,
        None,
    )
    .await?;
    assert_eq!(cancel_status.0, StatusCode::OK);
    assert_eq!(cancel_status.1["kind"], "cancel");

    let cancel_replay = send(
        &api,
        Method::POST,
        format!("/api/v1/environments/{}/cancel", first.id),
        &identity,
        first.owner_id,
        session_id,
        Some(stop_accepted.revision),
        None,
        Some("api-cancel-first"),
    )
    .await?;
    assert_eq!(cancel_replay.0, StatusCode::ACCEPTED);
    assert_eq!(cancel_replay.1["operationId"], cancel.1["operationId"]);
    assert_eq!(revocation_count.load(Ordering::SeqCst), 1);
    assert_eq!(cancel_status.1["state"], "cancelling");

    let recover_target = failed_instance(
        &store,
        &pool,
        first.project_id,
        first.owner_id,
        "api-recover",
    )
    .await?;
    let recover = send(
        &api,
        Method::POST,
        format!("/api/v1/environments/{}/recover", recover_target.id),
        &identity,
        recover_target.owner_id,
        session_id,
        Some(recover_target.revision),
        None,
        Some("api-recover-failed"),
    )
    .await?;
    assert_eq!(
        recover.0,
        StatusCode::ACCEPTED,
        "recover response: {:?}",
        recover.1
    );
    assert_eq!(
        recover.1["statusUrl"]
            .as_str()
            .map(|url| url.contains("/operations/")),
        Some(true)
    );
    let recover_operation_id = OperationId::from_str(
        recover.1["operationId"]
            .as_str()
            .ok_or("recover operation id missing")?,
    )?;
    let recover_status = send(
        &api,
        Method::GET,
        recover.1["statusUrl"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        &identity,
        recover_target.owner_id,
        session_id,
        None,
        None,
        None,
    )
    .await?;
    assert_eq!(recover_status.0, StatusCode::OK);
    assert_eq!(
        recover_status.1["operationId"],
        recover_operation_id.to_string()
    );
    assert_eq!(recover_status.1["kind"], "recover");

    let reset_target =
        failed_instance(&store, &pool, first.project_id, first.owner_id, "api-reset").await?;
    let reset_body = json!({
        "resetTarget": {
            "kind": "experiment_baseline",
            "releaseId": reset_target.release_id,
            "releaseVersion": reset_target.release_version
        }
    });
    let reset = send(
        &api,
        Method::POST,
        format!("/api/v1/environments/{}/reset", reset_target.id),
        &identity,
        reset_target.owner_id,
        session_id,
        Some(reset_target.revision),
        Some(reset_body),
        Some("api-reset-failed"),
    )
    .await?;
    assert_eq!(reset.0, StatusCode::ACCEPTED);
    let reset_status_url = reset.1["statusUrl"]
        .as_str()
        .ok_or("reset status URL missing")?;
    let reset_status = send(
        &api,
        Method::GET,
        reset_status_url.to_owned(),
        &identity,
        reset_target.owner_id,
        session_id,
        None,
        None,
        None,
    )
    .await?;
    assert_eq!(reset_status.0, StatusCode::OK);
    assert_eq!(reset_status.1["kind"], "reset");

    revocation_responder.abort();
    Ok(())
}

async fn failed_instance(
    store: &PgEnvironmentStore,
    pool: &sqlx::PgPool,
    project_id: ProjectId,
    owner_id: ActorId,
    key: &str,
) -> Result<contracts::environment::EnvironmentInstance, Box<dyn Error>> {
    let mut instance = requested_instance();
    instance.project_id = project_id;
    instance.course_id = None;
    instance.owner_id = owner_id;
    instance.eligibility_expires_at = Some(timestamp("2099-01-01T00:00:00.000Z"));
    instance.operation.actor_id = owner_id;
    store.create(key, &instance).await?;

    let mut held = Vec::new();
    let lease = loop {
        let candidate: Option<(uuid::Uuid, String, String)> = sqlx::query_as(
            "SELECT operation_id, state, contract->>'state' \
             FROM environment.environment_operations \
             WHERE state IN ('accepted','running','cancelling') \
               AND next_attempt_at <= now() \
               AND (lease_expires_at IS NULL OR lease_expires_at <= now()) \
             ORDER BY created_at, operation_id LIMIT 1",
        )
        .fetch_optional(pool)
        .await?;
        let lease = store
            .claim_due("api-failure-seeder", Duration::from_secs(30))
            .await?
            .ok_or("failed instance operation was not due")?;
        let (candidate_id, original_state, original_contract_state) =
            candidate.ok_or("claim_due selected an unobserved operation")?;
        if candidate_id != lease.instance.operation.id.as_uuid() {
            return Err("claim_due selected an unexpected operation".into());
        }
        if lease.instance.id == instance.id {
            break lease;
        }
        held.push((lease, original_state, original_contract_state));
    };
    let failed = apply_provider_failure(
        &lease.instance,
        lease.instance.operation.id,
        "LW_ENVIRONMENT_PROVIDER_TIMEOUT",
    )?;
    store.save_reconciled(&lease, &failed).await?;

    for (held_lease, original_state, original_contract_state) in held {
        let result = sqlx::query(
            "UPDATE environment.environment_operations \
             SET state=$3, \
                 contract=jsonb_set(contract, '{state}', to_jsonb($4::text), true), \
                 lease_owner=NULL, lease_token=NULL, lease_expires_at=NULL, heartbeat_at=NULL \
             WHERE operation_id=$1 AND lease_owner=$2",
        )
        .bind(held_lease.instance.operation.id.as_uuid())
        .bind(&held_lease.worker_id)
        .bind(original_state)
        .bind(original_contract_state)
        .execute(pool)
        .await?;
        if result.rows_affected() != 1 {
            return Err("failed instance helper lost a held operation lease".into());
        }
    }
    Ok(store.load(instance.id).await?)
}

fn gpu_release_projection(
    project_id: ProjectId,
    course_id: Option<contracts::CourseId>,
) -> Result<ReleasePublished, Box<dyn Error>> {
    let candidate_id = CandidateId::new();
    let candidate_revision = revision(1);
    let release_id = ReleaseId::new();
    let published_at = timestamp("2026-07-14T00:00:00.000Z");
    let digest = format!("sha256:{}", "0".repeat(64));
    let projection: ReleasePublished = serde_json::from_value(json!({
        "release": {
            "id": release_id,
            "projectId": project_id,
            "courseId": course_id,
            "version": 1,
            "candidateId": candidate_id,
            "agentRunId": AgentRunId::new(),
            "candidateRevision": candidate_revision,
            "runtimeKind": "container",
            "approval": {
                "id": ApprovalId::new(),
                "candidateId": candidate_id,
                "candidateRevision": candidate_revision,
                "policyRevision": 1,
                "trustRevision": 1,
                "actorId": ActorId::new(),
                "decision": "approved",
                "reason": "reviewed",
                "decidedAt": published_at
            },
            "artifact": {
                "kind": "container",
                "id": ImageArtifactId::new(),
                "build_request_id": BuildRequestId::new(),
                "repository": "harbor.internal/labweaver-test",
                "digest": digest
            },
            "publishedBy": ActorId::new(),
            "publishedAt": published_at
        },
        "environmentSpec": {
            "apiVersion": "environment.labweaver.io/v1",
            "kind": "EnvironmentSpec",
            "name": "gpu-replay",
            "class": "experiment",
            "resources": {
                "cpuMillicores": 1000,
                "memoryBytes": 1_073_741_824_u64,
                "storageBytes": 1_073_741_824_u64,
                "gpu": {"class": "a100", "count": 1}
            },
            "network": {"mode": "deny_all"},
            "entries": [{"name": "web", "protocol": "http", "servicePort": 8080}],
            "security": {
                "userPolicy": "non_root_required",
                "rootFilesystemPolicy": "read_only_required",
                "privilegeEscalationPolicy": "deny",
                "publicExposurePolicy": "deny",
                "securityProfileBinding": "restricted-v1"
            },
            "runtime": {
                "kind": "container",
                "provider_binding": "container-primary-v1",
                "build_context": {
                    "artifactId": contracts::ArtifactId::new(),
                    "storeBinding": "artifact-store-v1",
                    "objectVersion": "version-1",
                    "sizeBytes": 128,
                    "mediaType": "application/vnd.oci.image.layer.v1.tar+gzip"
                },
                "service_port": 8080
            },
            "retention": {
                "policyId": PolicyId::new(),
                "policyRevision": 1,
                "class": "run_evidence",
                "retainUntil": "2099-01-01T00:00:00.000Z",
                "disposition": "delete"
            }
        }
    }))?;
    projection.validate()?;
    Ok(projection)
}

async fn converge_to_ready(
    store: &PgEnvironmentStore,
    environment_id: EnvironmentId,
) -> Result<EnvironmentInstance, Box<dyn Error>> {
    let states = [
        (
            contracts::environment::ObservedEnvironmentState::Validating,
            false,
        ),
        (
            contracts::environment::ObservedEnvironmentState::Building,
            false,
        ),
        (
            contracts::environment::ObservedEnvironmentState::Provisioning,
            false,
        ),
        (
            contracts::environment::ObservedEnvironmentState::Ready,
            true,
        ),
    ];
    for (index, (next_state, operation_complete)) in states.into_iter().enumerate() {
        let lease = store
            .claim_due(
                &format!("api-ready-seeder-{index}"),
                Duration::from_secs(30),
            )
            .await?
            .ok_or("ready instance operation was not due")?;
        let endpoints = if operation_complete {
            vec![EnvironmentEndpoint {
                id: EndpointId::new(),
                protocol: EndpointProtocol::Https,
                revision: revision(lease.instance.revision.get() + 1),
                health: EndpointHealth::Healthy,
                observed_at: timestamp("2026-07-14T00:01:00.000Z"),
            }]
        } else {
            Vec::new()
        };
        let updated = apply_provider_observation(
            &lease.instance,
            lease.instance.operation.id,
            ProviderObservation {
                next_state,
                endpoints,
                cleanup_evidence: None,
                operation_complete,
            },
        )?;
        store.save_reconciled(&lease, &updated).await?;
    }
    Ok(store.load(environment_id).await?)
}

async fn spawn_revocation_responder(
    client: async_nats::Client,
) -> Result<(JoinHandle<()>, Arc<AtomicUsize>), Box<dyn Error>> {
    let mut requests = client.subscribe(ACCESS_REVOKE_SUBJECT).await?;
    let count = Arc::new(AtomicUsize::new(0));
    let responder_count = Arc::clone(&count);
    let handle = tokio::spawn(async move {
        while let Some(message) = requests.next().await {
            let Some(reply) = message.reply else {
                continue;
            };
            responder_count.fetch_add(1, Ordering::SeqCst);
            let request: Value = match serde_json::from_slice(&message.payload) {
                Ok(value) => value,
                Err(_) => continue,
            };
            let response = json!({
                "version": 1,
                "environmentId": request["environmentId"],
                "environmentRevision": request["environmentRevision"],
                "accessRevocationRevision": 9
            });
            if client
                .publish(
                    reply,
                    serde_json::to_vec(&response).unwrap_or_default().into(),
                )
                .await
                .is_err()
            {
                break;
            }
        }
    });
    Ok((handle, count))
}

fn access_identity() -> auth::ServiceIdentity {
    auth::ServiceIdentity {
        issuer: "test-issuer".to_owned(),
        subject: "test-subject".to_owned(),
        client_id: "test-access-bff".to_owned(),
        expires_at: OffsetDateTime::now_utc() + time::Duration::hours(1),
        permissions: BTreeSet::from(["access.environment.forward".to_owned()]),
    }
}

#[allow(clippy::too_many_arguments)]
async fn send(
    router: &Router,
    method: Method,
    uri: String,
    identity: &auth::ServiceIdentity,
    actor_id: ActorId,
    session_id: uuid::Uuid,
    revision: Option<Revision>,
    body: Option<Value>,
    idempotency_key: Option<&str>,
) -> Result<(StatusCode, Value), Box<dyn Error>> {
    let payload = body
        .map(|value| serde_json::to_vec(&value))
        .transpose()?
        .unwrap_or_default();
    let mut request = Request::new(Body::from(payload));
    *request.method_mut() = method;
    *request.uri_mut() = uri.parse()?;
    let headers = request.headers_mut();
    headers.insert(
        "x-labweaver-actor-id",
        HeaderValue::from_str(&actor_id.to_string())?,
    );
    headers.insert(
        "x-labweaver-session-id",
        HeaderValue::from_str(&session_id.to_string())?,
    );
    if let Some(revision) = revision {
        headers.insert(
            header::IF_MATCH,
            HeaderValue::from_str(&StrongEtag::from_revision(revision).header_value())?,
        );
    }
    if let Some(idempotency_key) = idempotency_key {
        headers.insert("idempotency-key", HeaderValue::from_str(idempotency_key)?);
    }
    request.extensions_mut().insert(identity.clone());
    request
        .extensions_mut()
        .insert(telemetry::RequestContext::generate());
    let response = router.clone().oneshot(request).await?;
    let status = response.status();
    let body = to_bytes(response.into_body(), 2 * 1024 * 1024).await?;
    let value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body)?
    };
    Ok((status, value))
}
