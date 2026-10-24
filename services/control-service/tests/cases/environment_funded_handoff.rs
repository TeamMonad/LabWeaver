//! Environment consumer journey after the normal Control publication and Resource funded lease.
//! Only service transport/auth and Kubernetes are external synthetic boundaries; the authorities,
//! original approval, hold, aggregate, operation ledger and meter use the real `PostgreSQL` stores.
#![allow(clippy::too_many_lines)]
use async_trait::async_trait;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use contracts::environment::{
    EnvironmentLeaseAuthorization, EnvironmentRuntimeAuthorizationResolution,
    ResolveEnvironmentRuntimeAuthorizationRequest, ResourceEnvironmentHandoff,
};
use contracts::events::{CloudEvent, EVENT_CONTRACTS, ReleasePublished, SPEC_VERSION, subjects};
use contracts::resource::environment_funding::EnvironmentCreditGrant;
use contracts::{EventId, Revision, Sequence, UtcTimestamp};
use environment_service::{
    EnvironmentRuntimeAdmission, EnvironmentRuntimeAuthority, PgEnvironmentStore,
    PgReleaseProjectionStore, ResourceUsageClientError,
};
use futures_util::StreamExt;
use resource_service::{
    runtime_authorization::CurrentEnvironmentAuthorization, store::PgResourceStore,
};
use sqlx::{PgPool, Row};
use std::{sync::Arc, time::Duration};
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::{GenericImage, runners::AsyncRunner};
use tower::ServiceExt;
#[path = "environment_funded_auth.rs"]
mod service_auth;
#[path = "environment_funded_upgrade.rs"]
mod upgrade_guards;
#[path = "environment_funded_vm.rs"]
mod vm_boundary;
type Result<T = (), E = Box<dyn std::error::Error + Send + Sync>> = std::result::Result<T, E>;
const OWNER: &str = "synthetic-environment";

#[derive(Clone)]
struct UnusedAdmission;
#[async_trait]
impl environment_service::WorkAdmissionResolver for UnusedAdmission {
    async fn resolve(
        &self,
        _id: contracts::AgentRunId,
        _query: &contracts::http::WorkConfigurationAdmissionQuery,
        _now: UtcTimestamp,
    ) -> std::result::Result<
        contracts::http::WorkConfigurationAdmissionBinding,
        environment_service::WorkAdmissionClientError,
    > {
        Err(environment_service::WorkAdmissionClientError::Configuration)
    }
}

#[derive(Clone)]
struct PgAuthority(PgResourceStore, PgPool);
#[async_trait]
impl EnvironmentRuntimeAuthority for PgAuthority {
    async fn resolve_runtime_authorization(
        &self,
        input: &ResolveEnvironmentRuntimeAuthorizationRequest,
    ) -> std::result::Result<EnvironmentRuntimeAuthorizationResolution, ResourceUsageClientError>
    {
        let value = resource_service::runtime_authorization::current_environment_authorization(
            &self.1,
            input.resource_request_id,
            input.expected_lease_id,
        )
        .await
        .map_err(|_| ResourceUsageClientError::InvalidResponse)?;
        Ok(EnvironmentRuntimeAuthorizationResolution {
            valid_until: value.lease.expires_at.min(value.approval_valid_until),
            provider_binding: value.provider_binding,
            owner_actor_id: value.lease.owner_actor_id,
            authorization: value.binding,
            lease_authorization: value.lease,
        })
    }
    async fn resolve_credit_grant(
        &self,
        lease: &EnvironmentLeaseAuthorization,
    ) -> std::result::Result<EnvironmentCreditGrant, ResourceUsageClientError> {
        self.0
            .environment_credit_grant(OWNER, lease.lease_id)
            .await
            .map_err(|_| ResourceUsageClientError::InvalidResponse)
    }
}

pub(super) async fn exercise(
    pool: &PgPool,
    resource: &PgResourceStore,
    original: &CurrentEnvironmentAuthorization,
) -> Result {
    super::super::apply_domain_migrations(pool, persistence_sqlx::Domain::Environment)
        .await
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    let row=sqlx::query("SELECT release_contract,candidate_contract FROM control.runtime_authorization_configurations WHERE release_id=$1")
        .bind(match original.binding.target {contracts::resource::ResourceTarget::Environment {release_id,..}=>release_id.as_uuid(),contracts::resource::ResourceTarget::Task { .. }=>return Err("environment target".into())}).fetch_one(pool).await?;
    let release: contracts::supply_chain::EnvironmentTemplateRelease =
        serde_json::from_value(row.try_get("release_contract")?)?;
    let candidate: contracts::authoring::EnvironmentCandidate =
        serde_json::from_value(row.try_get("candidate_contract")?)?;
    let projection = ReleasePublished {
        release,
        environment_spec: candidate.spec,
    };
    if projection.environment_spec.class == contracts::authoring::EnvironmentClass::Experiment {
        assert_ne!(projection.release.project_id, original.lease.project_id);
    } else {
        assert_eq!(projection.release.project_id, original.lease.project_id);
    }
    let releases = PgReleaseProjectionStore::new(pool.clone());
    let contract = EVENT_CONTRACTS
        .iter()
        .find(|v| v.subject == subjects::ENVIRONMENT_TEMPLATE_RELEASE_PUBLISHED)
        .ok_or("release event")?;
    releases
        .accept(
            "funded-env-release",
            &CloudEvent {
                specversion: SPEC_VERSION.into(),
                id: EventId::new(),
                source: contract.source().into(),
                event_type: contract.event_type.into(),
                subject: contract.subject.into(),
                time: projection.release.published_at,
                datacontenttype: "application/json".into(),
                dataschema: contract.data_schema(),
                project_id: projection.release.project_id,
                course_id: projection.release.course_id,
                aggregate_revision: Revision::new(1)?,
                aggregate_sequence: Sequence(1),
                trace_id: "funded-environment".into(),
                data: projection.clone(),
            },
        )
        .await?;
    let store = PgEnvironmentStore::new(pool.clone());
    let admission = EnvironmentRuntimeAdmission::new(
        releases.clone(),
        Arc::new(PgAuthority(resource.clone(), pool.clone())),
    );
    let grant = resource
        .environment_credit_grant(OWNER, original.lease.lease_id)
        .await?;
    assert!(grant.can_authorize_execution);
    let claim: contracts::resource::CapacityClaim = serde_json::from_value(
        sqlx::query_scalar::<_, serde_json::Value>(
            "SELECT contract FROM resource.capacity_claims WHERE claim_id=$1",
        )
        .bind(grant.claim_id.as_uuid())
        .fetch_one(pool)
        .await?,
    )?;
    let request = resource.load(grant.request_id).await?;
    let handoff = ResourceEnvironmentHandoff {
        version: 1,
        request_id: request.id,
        request_revision: request.revision,
        lease_id: original.lease.lease_id,
        lease_revision: original.lease.lease_revision,
        claim_id: claim.id,
        claim_revision: claim.revision,
        environment_id: original.lease.environment_id,
        project_id: request.project_id,
        course_id: request.course_id,
        owner_actor_id: request.requester_id,
        display_label: request.request_key.clone(),
        release_id: projection.release.id,
        release_version: projection.release.version,
        provider_binding: claim.provider_binding.clone(),
        capacity_binding: claim.id.to_string(),
        approved_resources: claim.workload_resources.clone(),
        gpu_allocation: claim.gpu_allocation.clone(),
        trace_id: "funded-original-handoff".into(),
    };
    assert_eq!(
        handoff.class_for_release(&projection)?,
        projection.environment_spec.class
    );
    let broker = GenericImage::new("nats", "2.11.8-alpine")
        .with_exposed_port(4222.tcp())
        .with_wait_for(WaitFor::message_on_stderr("Server is ready"))
        .start()
        .await?;
    let nats = async_nats::connect(format!(
        "nats://127.0.0.1:{}",
        broker.get_host_port_ipv4(4222).await?
    ))
    .await?;
    let mut subscription = nats.subscribe("funded-lease-verify").await?;
    let mut access_requests = nats.subscribe("unused-access-revoke").await?;
    let revocations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed_revocations = Arc::clone(&revocations);
    let access_probe = tokio::spawn(async move {
        while access_requests.next().await.is_some() {
            observed_revocations.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    });
    nats.flush().await?;
    let responder = nats.clone();
    let authority = resource.clone();
    let replies = tokio::spawn(async move {
        while let Some(message) = subscription.next().await {
            let Some(reply) = message.reply else { continue };
            let Ok(input) = serde_json::from_slice::<
                contracts::environment::EnvironmentLeaseVerificationRequest,
            >(&message.payload) else {
                continue;
            };
            let Ok(now) = authority.current_time().await else {
                continue;
            };
            let Ok(value) = authority.verify_environment_lease(&input, now).await else {
                continue;
            };
            let Ok(body) = serde_json::to_vec(&value) else {
                continue;
            };
            let _ = responder.publish(reply, body.into()).await;
        }
    });
    let freeze = environment_service::FreezeBindingService::new_with_admission_resolver(
        pool.clone(),
        releases.clone(),
        environment_service::FreezeBindingConfiguration {
            container_workspace_storage_class: "synthetic-storage".into(),
            vm: None,
        },
        UnusedAdmission,
    )?;
    let api = environment_service::environment_api_router(
        environment_service::EnvironmentApiState::new(
            store.clone(),
            releases,
            environment_service::NatsAccessRevoker::new(
                "unused-access-revoke".into(),
                nats.clone(),
                Duration::from_secs(2),
            )?,
            environment_service::NatsResourceLeaseVerifier::new(
                "funded-lease-verify".into(),
                nats,
                Duration::from_secs(2),
            )?,
            freeze,
        )
        .with_runtime_admission(admission.clone()),
    );
    let authority = service_auth::start().await?;
    let api = environment_service::with_service_auth(api, authority.verifier.clone());
    let rejected = api
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/v1/resource/work-handoffs")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&handoff)?))?,
        )
        .await?;
    assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);
    let send = |router: Router, body: serde_json::Value| {
        let token = authority.token.clone();
        Box::pin(async move {
            let mut req = Request::builder()
                .method("POST")
                .uri("/internal/v1/resource/work-handoffs")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::from(body.to_string()))?;
            req.extensions_mut()
                .insert(telemetry::RequestContext::generate());
            let response = router.oneshot(req).await?;
            let status = response.status();
            let body = to_bytes(response.into_body(), 1_048_576).await?;
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>((
                status,
                serde_json::from_slice::<serde_json::Value>(&body)?,
            ))
        })
    };
    let public = contracts::http::CreateEnvironmentRequest {
        resource_request_id: request.id,
        project_id: request.project_id,
        course_id: request.course_id,
        release_id: projection.release.id,
        release_version: projection.release.version,
        display_label: None,
    };
    let create = |router: Router, body: serde_json::Value, actor: contracts::ActorId| {
        let token = authority.access_token.clone();
        Box::pin(async move {
            let mut input = Request::builder()
                .method("POST")
                .uri("/api/v1/environments")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {token}"))
                .header("x-labweaver-actor-id", actor.to_string())
                .header("x-labweaver-session-id", uuid::Uuid::now_v7().to_string())
                .header("idempotency-key", "funded-browser-create")
                .body(Body::from(body.to_string()))?;
            input
                .extensions_mut()
                .insert(telemetry::RequestContext::generate());
            let response = router.oneshot(input).await?;
            let status = response.status();
            let bytes = to_bytes(response.into_body(), 1_048_576).await?;
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>((
                status,
                serde_json::from_slice::<serde_json::Value>(&bytes)?,
            ))
        })
    };
    let mut incomplete = serde_json::to_value(&public)?;
    incomplete
        .as_object_mut()
        .ok_or("create intent object")?
        .remove("resourceRequestId");
    assert_eq!(
        create(api.clone(), incomplete, request.requester_id)
            .await?
            .0,
        StatusCode::BAD_REQUEST
    );
    let pending = create(
        api.clone(),
        serde_json::to_value(&public)?,
        request.requester_id,
    )
    .await?;
    assert_eq!(pending.0, StatusCode::CONFLICT, "{}", pending.1);
    assert_eq!(
        pending.1["diagnosticCode"],
        "LW_ENVIRONMENT_ALLOCATION_PENDING"
    );
    let mut altered = handoff.clone();
    altered.approved_resources.cpu_millicores += 1;
    assert_eq!(
        send(api.clone(), serde_json::to_value(&altered)?).await?.0,
        StatusCode::PRECONDITION_FAILED
    );
    altered = handoff.clone();
    altered.claim_id = contracts::CapacityClaimId::new();
    assert_ne!(
        send(api.clone(), serde_json::to_value(&altered)?).await?.0,
        StatusCode::ACCEPTED
    );
    let (first, replay) = tokio::join!(
        send(api.clone(), serde_json::to_value(&handoff)?),
        send(api.clone(), serde_json::to_value(&handoff)?),
    );
    let first = first?;
    assert_eq!(first.0, StatusCode::ACCEPTED, "{}", first.1);
    assert_eq!(replay?, first);
    let receipt: contracts::environment::ResourceEnvironmentHandoffReceipt =
        serde_json::from_value(first.1.clone())?;
    assert_eq!(receipt.request_id, request.id);
    assert_eq!(receipt.accepted.environment_id, handoff.environment_id);
    assert_eq!(
        store.load_resource_handoff_receipt(request.id).await?,
        receipt
    );
    resource
        .mark_capacity_handed_off(
            claim.id,
            claim.revision,
            original.lease.lease_id,
            receipt.lease_revision,
        )
        .await?;
    let instance = store.load(handoff.environment_id).await?;
    assert_eq!(instance.class, projection.environment_spec.class);
    assert!(instance.has_resource_lease());
    assert_eq!(instance.generation, 1);
    assert_eq!(
        instance.operation.lease_authorization.as_ref(),
        Some(&original.lease)
    );
    let attached = create(
        api.clone(),
        serde_json::to_value(&public)?,
        request.requester_id,
    )
    .await?;
    assert_eq!(attached.0, StatusCode::ACCEPTED, "{}", attached.1);
    assert_eq!(attached.1["environmentId"], instance.id.to_string());
    assert_eq!(attached.1["operationId"], instance.operation.id.to_string());
    assert_eq!(
        create(
            api.clone(),
            serde_json::to_value(&public)?,
            request.requester_id
        )
        .await?,
        attached
    );
    let mut changed = public.clone();
    changed.resource_request_id = contracts::ResourceRequestId::new();
    assert_eq!(
        create(
            api.clone(),
            serde_json::to_value(&changed)?,
            request.requester_id
        )
        .await?
        .0,
        StatusCode::CONFLICT
    );
    assert_ne!(
        create(
            api.clone(),
            serde_json::to_value(&public)?,
            contracts::ActorId::new()
        )
        .await?
        .0,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        admission.for_instance(&instance).await?.lease_authorization,
        original.lease
    );
    sqlx::query("UPDATE control.course_memberships SET state='revoked',revision=revision+1 WHERE course_id=$1 AND actor_id=$2")
        .bind(request.course_id.ok_or("course scope")?.as_uuid()).bind(request.requester_id.as_uuid()).execute(pool).await?;
    assert!(matches!(
        resource_service::runtime_authorization::current_environment_authorization(
            pool,
            request.id,
            Some(original.lease.lease_id)
        )
        .await,
        Err(resource_service::runtime_authorization::RuntimeBindingError::ScopeDenied)
    ));
    assert!(admission.for_instance(&instance).await.is_err());
    let mut retry = handoff.clone();
    retry.request_revision = Revision::new(handoff.request_revision.get() + 1)?;
    retry.lease_revision = Revision::new(handoff.lease_revision.get() + 1)?;
    retry.claim_revision = Revision::new(handoff.claim_revision.get() + 1)?;
    retry.trace_id = "new-trace-after-lost-response".into();
    assert_eq!(
        send(api.clone(), serde_json::to_value(&retry)?).await?,
        first,
        "a known accepted handoff must replay without granting new execution after revocation"
    );
    sqlx::query("UPDATE control.course_memberships SET state='active',revision=revision+1 WHERE course_id=$1 AND actor_id=$2")
        .bind(request.course_id.ok_or("course scope")?.as_uuid()).bind(request.requester_id.as_uuid()).execute(pool).await?;
    assert_eq!(
        resource
            .environment_credit_grant(OWNER, original.lease.lease_id)
            .await?
            .hold
            .id,
        grant.hold.id
    );
    let counts:(i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM environment.environment_instances WHERE environment_id=$1),(SELECT count(*) FROM environment.environment_operations WHERE environment_id=$1),(SELECT count(*) FROM resource.credit_holds WHERE request_id=$2)").bind(instance.id.as_uuid()).bind(request.id.as_uuid()).fetch_one(pool).await?;
    assert_eq!(counts, (1, 1, 1));
    let old_reservations: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM resource.environment_resource_reservations WHERE environment_id=$1",
    )
    .bind(instance.id.as_uuid())
    .fetch_one(pool)
    .await?;
    assert_eq!(old_reservations, 0);
    let meter: serde_json::Value = sqlx::query_scalar(
        "SELECT contract FROM environment.resource_metering_state WHERE environment_id=$1",
    )
    .bind(instance.id.as_uuid())
    .fetch_one(pool)
    .await?;
    assert_eq!(meter["target"]["kind"], "resource_request");
    assert_eq!(meter["target"]["requestId"], request.id.to_string());
    let mut foreign = handoff.clone();
    foreign.environment_id = contracts::EnvironmentId::new();
    assert_ne!(
        send(api.clone(), serde_json::to_value(&foreign)?).await?.0,
        StatusCode::ACCEPTED
    );
    let http = LifecycleHttp {
        router: api,
        access_token: authority.access_token.clone(),
        usage_identity: authority.usage_identity.clone(),
        revocations,
    };
    Box::pin(exercise_execution(
        pool, resource, &store, &admission, &instance, &grant, &http,
    ))
    .await?;
    replies.abort();
    access_probe.abort();
    Ok(())
}

// Kubernetes effects are the only simulated provider boundary; lifecycle projection is production.
struct ClusterFixture {
    store: PgEnvironmentStore,
    calls: Arc<std::sync::atomic::AtomicUsize>,
    reject_next_restart: Arc<std::sync::atomic::AtomicBool>,
}
impl ClusterFixture {
    async fn observation(
        &self,
    ) -> std::result::Result<
        environment_service::ContainerApplyObservation,
        environment_service::ProviderFailure,
    > {
        Ok(environment_service::ContainerApplyObservation {
            ready: true,
            observed_at: self.store.current_time().await.map_err(|_| {
                environment_service::ProviderFailure {
                    code: environment_service::ProviderFailureCode::Unavailable,
                    retryable: true,
                }
            })?,
        })
    }
}
#[async_trait]
impl environment_service::ContainerProviderBackend for ClusterFixture {
    async fn apply(
        &self,
        _: &environment_service::ContainerBackendFence,
        _: &environment_service::ContainerResourcePlan,
    ) -> std::result::Result<
        environment_service::ContainerApplyObservation,
        environment_service::ProviderFailure,
    > {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.observation().await
    }
    async fn observe(
        &self,
        _: &environment_service::ContainerBackendFence,
        _: &environment_service::ContainerResourcePlan,
    ) -> std::result::Result<
        environment_service::ContainerApplyObservation,
        environment_service::ProviderFailure,
    > {
        self.observation().await
    }
    async fn scale(
        &self,
        _: &environment_service::ContainerBackendFence,
        _: &environment_service::ContainerResourcePlan,
        replicas: u32,
    ) -> std::result::Result<
        environment_service::ContainerApplyObservation,
        environment_service::ProviderFailure,
    > {
        if replicas > 0 {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        self.observation().await
    }
    async fn restart(
        &self,
        _: &environment_service::ContainerBackendFence,
        _: &environment_service::ContainerResourcePlan,
        _: Revision,
    ) -> std::result::Result<
        environment_service::ContainerApplyObservation,
        environment_service::ProviderFailure,
    > {
        if self
            .reject_next_restart
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(environment_service::ProviderFailure {
                code: environment_service::ProviderFailureCode::Rejected,
                retryable: false,
            });
        }
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.observation().await
    }
    async fn delete_namespace(
        &self,
        _: &environment_service::ContainerBackendFence,
        _: &environment_service::ContainerResourcePlan,
    ) -> std::result::Result<contracts::ArtifactRef, environment_service::ProviderFailure> {
        Ok(cleanup_evidence())
    }
}
fn assert_native_access_fence(
    instance: &contracts::environment::EnvironmentInstance,
    now: UtcTimestamp,
) -> Result {
    use contracts::environment::{
        EnvironmentAccessSubjectKind, EnvironmentEndpointEligibilityRequest,
        EnvironmentOwnerResolutionRequest,
    };
    let owner = EnvironmentOwnerResolutionRequest {
        environment_id: instance.id,
        project_id: instance.project_id,
        course_id: instance.course_id,
        owner_actor_id: instance.owner_id,
        expected_revision: instance.revision,
    };
    let lease = instance
        .operation
        .lease_authorization
        .as_ref()
        .ok_or("native access lease")?;
    let resolved = environment_service::authorize_owner_resolution(instance, &owner, now)?;
    assert_eq!(
        resolved
            .lease_fence
            .as_ref()
            .ok_or("owner lease fence")?
            .lease_id,
        lease.lease_id
    );
    let endpoints = EnvironmentEndpointEligibilityRequest {
        environment_id: instance.id,
        project_id: instance.project_id,
        course_id: instance.course_id,
        actor_id: instance.owner_id,
        subject_kind: EnvironmentAccessSubjectKind::Owner,
        expected_revision: instance.revision,
        endpoint_ids: instance.endpoints.iter().map(|entry| entry.id).collect(),
    };
    assert_eq!(
        environment_service::authorize_endpoint_eligibility(instance, &endpoints, now)?.lease_fence,
        resolved.lease_fence
    );
    let mut expired = instance.clone();
    expired
        .operation
        .lease_authorization
        .as_mut()
        .ok_or("native access lease")?
        .expires_at = now;
    assert!(environment_service::authorize_owner_resolution(&expired, &owner, now).is_err());
    assert!(
        environment_service::authorize_endpoint_eligibility(&expired, &endpoints, now).is_err()
    );
    let mut missing = instance.clone();
    missing.operation.lease_authorization = None;
    assert!(environment_service::authorize_owner_resolution(&missing, &owner, now).is_err());
    let mut stale = instance.clone();
    stale.observed_generation = 0;
    assert!(environment_service::authorize_owner_resolution(&stale, &owner, now).is_err());
    Ok(())
}

fn cleanup_evidence() -> contracts::ArtifactRef {
    contracts::ArtifactRef {
        artifact_id: contracts::ArtifactId::new(),
        store_binding: "synthetic-cleanup".into(),
        object_version: "original-native-lease".into(),
        size_bytes: 1,
        media_type: "application/json".into(),
    }
}
struct PhysicalClusterFixture(Arc<std::sync::atomic::AtomicUsize>, UtcTimestamp);
#[async_trait]
impl environment_service::ContainerExecutorBackend for PhysicalClusterFixture {
    async fn execute(
        &self,
        _: &environment_service::ContainerBackendFence,
        request: &environment_service::ContainerExecutorRequest,
    ) -> environment_service::ContainerExecutorResponse {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        match request {
            environment_service::ContainerExecutorRequest::DeleteNamespace { plan } => {
                environment_service::ContainerExecutorResponse::Deleted {
                    plan_sha256: plan.plan_sha256,
                    cleanup_evidence: cleanup_evidence(),
                }
            }
            environment_service::ContainerExecutorRequest::Apply { plan } => {
                environment_service::ContainerExecutorResponse::Observed {
                    plan_sha256: plan.plan_sha256,
                    observation: environment_service::ContainerApplyObservation {
                        ready: true,
                        observed_at: self.1,
                    },
                }
            }
            _ => environment_service::ContainerExecutorResponse::Failed {
                failure: environment_service::ProviderFailure {
                    code: environment_service::ProviderFailureCode::Rejected,
                    retryable: false,
                },
            },
        }
    }
}
fn physical_envelope(
    instance: &contracts::environment::EnvironmentInstance,
    step: u32,
    cleanup: bool,
    deadline: UtcTimestamp,
) -> Result<environment_service::ContainerExecutorRequestEnvelope> {
    use environment_service::{
        ContainerBackendFence, ContainerExecutorRequest, ContainerExecutorRequestEnvelope,
        ContainerResource, ContainerResourcePlan, ReconcileAction,
    };
    let namespace = format!("lw-env-{}", instance.id);
    let plan = ContainerResourcePlan {
        environment_id: instance.id,
        project_id: instance.project_id,
        namespace: namespace.clone(),
        namespace_uid: None,
        image: "synthetic/runtime".into(),
        plan_sha256: persistence_sqlx::Sha256Digest::of_bytes(b"funded-pvc"),
        resources: vec![ContainerResource {
            kind: "PersistentVolumeClaim".into(),
            namespace: Some(namespace),
            name: "workspace".into(),
            document: serde_json::json!({"kind":"PersistentVolumeClaim"}),
        }],
    };
    let action = if cleanup {
        ReconcileAction::Cleanup
    } else {
        ReconcileAction::Provision
    };
    let request = if cleanup {
        ContainerExecutorRequest::DeleteNamespace { plan }
    } else {
        ContainerExecutorRequest::Apply { plan }
    };
    let request_id = persistence_sqlx::Sha256Digest::of_canonical(
        &serde_json::json!({"protocolVersion":environment_service::CONTAINER_BACKEND_PROTOCOL_VERSION,"environmentId":instance.id,"operationId":instance.operation.id,"providerStep":step,"operationGeneration":instance.generation,"attempt":1,"action":action,"deadlineAt":deadline,"request":request}),
    )?;
    Ok(ContainerExecutorRequestEnvelope {
        fence: ContainerBackendFence {
            protocol_version: environment_service::CONTAINER_BACKEND_PROTOCOL_VERSION,
            environment_id: instance.id,
            operation_id: instance.operation.id,
            provider_step: step,
            operation_generation: instance.generation,
            attempt: 1,
            action,
            request_id,
            trace_id: "synthetic-native-funding".into(),
            deadline_at: deadline,
        },
        request,
    })
}
struct LifecycleHttp {
    router: Router,
    access_token: String,
    usage_identity: auth::ServiceIdentity,
    revocations: Arc<std::sync::atomic::AtomicUsize>,
}

impl LifecycleHttp {
    async fn send(
        &self,
        instance: &contracts::environment::EnvironmentInstance,
        action: &str,
    ) -> Result<(StatusCode, serde_json::Value)> {
        let mut input = Request::builder()
            .method("POST")
            .uri(format!("/api/v1/environments/{}/{action}", instance.id))
            .header("authorization", format!("Bearer {}", self.access_token))
            .header("x-labweaver-actor-id", instance.owner_id.to_string())
            .header("x-labweaver-session-id", uuid::Uuid::now_v7().to_string())
            .header(
                "if-match",
                contracts::http::StrongEtag::from_revision(instance.revision).header_value(),
            )
            .header(
                "idempotency-key",
                format!(
                    "funded-{action}-{}-{}",
                    instance.id,
                    instance.revision.get()
                ),
            )
            .body(Body::empty())?;
        input
            .extensions_mut()
            .insert(telemetry::RequestContext::generate());
        let response = self.router.clone().oneshot(input).await?;
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1_048_576).await?;
        Ok((status, serde_json::from_slice(&bytes)?))
    }

    async fn command(
        &self,
        instance: &contracts::environment::EnvironmentInstance,
        action: &str,
    ) -> Result {
        let first = self.send(instance, action).await?;
        assert_eq!(first.0, StatusCode::ACCEPTED, "{}", first.1);
        assert_eq!(
            self.send(instance, action).await?,
            first,
            "lost-response replay cannot create another operation"
        );
        Ok(())
    }
}

async fn lifecycle_command(
    store: &PgEnvironmentStore,
    instance: &contracts::environment::EnvironmentInstance,
    kind: contracts::environment::EnvironmentOperationKind,
    lease: Option<EnvironmentLeaseAuthorization>,
) -> Result {
    let now = store.current_time().await?;
    store
        .accept_api_command(
            &format!("native-{}-{kind:?}", instance.revision.get()),
            &environment_service::LifecycleCommand {
                environment_id: instance.id,
                kind,
                expected_revision: instance.revision,
                actor_id: instance.owner_id,
                trace_id: "synthetic-native-lifecycle".into(),
                accepted_at: now,
                deadline_at: UtcTimestamp::from_utc(now.get() + time::Duration::seconds(30))?,
                access_revocation_revision: matches!(
                    kind,
                    contracts::environment::EnvironmentOperationKind::Stop
                        | contracts::environment::EnvironmentOperationKind::Delete
                )
                .then_some(instance.revision)
                .or(instance.operation.access_revocation_revision),
                preserve_mutable_disk: true,
                max_attempts: 3,
                reset_target: None,
            },
            None,
            lease,
            instance.project_id,
            instance.course_id,
        )
        .await?;
    Ok(())
}
async fn exercise_execution(
    pool: &PgPool,
    resource: &PgResourceStore,
    store: &PgEnvironmentStore,
    admission: &EnvironmentRuntimeAdmission,
    instance: &contracts::environment::EnvironmentInstance,
    initial: &EnvironmentCreditGrant,
    http: &LifecycleHttp,
) -> Result {
    use contracts::environment::{
        EnvironmentOperationKind as Kind, ObservedEnvironmentState as State,
    };
    use environment_service::{
        ContainerReleaseResolver as _, ReconcileWorkerOutcome, RuntimeAdmissionError,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = Arc::new(AtomicUsize::new(0));
    let reject_next_restart = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut registry = environment_service::ProviderRegistry::default();
    let projections = PgReleaseProjectionStore::new(pool.clone());
    let resolved = projections
        .resolve(instance.release_id, instance.release_version)
        .await?;
    let projection = &resolved.projection;
    if let contracts::supply_chain::ImageArtifact::Container { repository, .. } =
        &projection.release.artifact
    {
        let prefix = repository
            .rsplit_once('/')
            .ok_or("image repository prefix")?
            .0;
        assert_eq!(instance.project_id, projection.release.project_id);
        assert_eq!(instance.release_id, projection.release.id);
        assert_eq!(instance.release_version, projection.release.version);
        let approval = projection
            .release
            .approval
            .as_ref()
            .ok_or("generated container approval")?;
        let configuration = environment_service::ContainerProviderConfiguration::new(
            environment_service::ContainerReleasePolicy::new(
                contracts::PolicyId::new(),
                approval.policy_revision,
                approval.trust_revision,
            )?,
            prefix.into(),
            "synthetic-access".into(),
            "synthetic-access".into(),
            "registry-pull".into(),
            "synthetic-storage".into(),
            "ReadWriteOnce",
        )?;
        let provider = environment_service::ContainerProvider::new(
            instance.provider_binding.clone(),
            Arc::new(ClusterFixture {
                store: store.clone(),
                calls: calls.clone(),
                reject_next_restart: reject_next_restart.clone(),
            }),
            Arc::new(projections),
            configuration,
        )?;
        provider.plan(
            instance,
            &resolved,
            environment_service::ReconcileAction::Validate,
        )?;
        registry.register(Arc::new(provider))?;
    } else {
        vm_boundary::register(
            &mut registry,
            pool,
            projection,
            calls.clone(),
            reject_next_restart.clone(),
        )?;
    }
    let worker = environment_service::ReconcileWorker::new(
        store.clone(),
        environment_service::Reconciler::new(registry, Duration::from_millis(100))?
            .with_runtime_admission(admission.clone()),
        Duration::from_millis(1100),
        Duration::from_millis(10),
    )?;
    for state in [
        State::Validating,
        State::Building,
        State::Provisioning,
        State::Ready,
    ] {
        let outcome = run_due(&worker, store, "native-create").await?;
        assert!(
            matches!(outcome, ReconcileWorkerOutcome::Advanced { state: actual, .. } if actual == state),
            "expected {state:?}, actual {outcome:?}"
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let ready = store.load(instance.id).await?;
    assert_native_access_fence(&ready, store.current_time().await?)?;
    assert_eq!(ready.lease_id, instance.lease_id);
    let physical_calls = Arc::new(AtomicUsize::new(0));
    let executor = environment_service::FencedContainerExecutor::new(
        environment_service::PgContainerExecutorFenceStore::new(pool.clone()),
        PhysicalClusterFixture(physical_calls.clone(), store.current_time().await?),
    )
    .with_runtime_admission(admission.clone());
    let deadline =
        UtcTimestamp::from_utc(store.current_time().await?.get() + time::Duration::seconds(30))?;
    let container = instance.runtime_kind == contracts::authoring::RuntimeKind::Container;
    let first = physical_envelope(&ready, 1, false, deadline)?;
    if !container {
        assert!(
            matches!(
                executor.execute(first.clone()).await,
                Err(
                    environment_service::ContainerExecutorFenceError::RuntimeAdmission(
                        RuntimeAdmissionError::Binding
                    )
                )
            ),
            "an original VM grant cannot authorize a Container executor effect"
        );
        assert_eq!(physical_calls.load(Ordering::SeqCst), 0);
    }
    if container {
        for foreign in [
            contracts::environment::EnvironmentInstance {
                project_id: contracts::ProjectId::new(),
                ..ready.clone()
            },
            contracts::environment::EnvironmentInstance {
                generation: ready.generation + 1,
                ..ready.clone()
            },
        ] {
            assert!(matches!(
                executor
                    .execute(physical_envelope(&foreign, 1, false, deadline)?)
                    .await,
                Err(
                    environment_service::ContainerExecutorFenceError::RuntimeAdmission(
                        RuntimeAdmissionError::Binding
                    )
                )
            ));
        }
        assert_eq!(physical_calls.load(Ordering::SeqCst), 0);
        executor.execute(first.clone()).await?;
        assert_eq!(physical_calls.load(Ordering::SeqCst), 1);
    }
    let new_until =
        UtcTimestamp::from_utc(initial.covered_until.get() + time::Duration::seconds(60))?;
    let renewed = resource
        .renew_lease(
            "native-funded-renewal",
            initial.lease_id,
            initial.lease_revision,
            new_until,
            "synthetic-renewal",
        )
        .await?;
    assert_eq!(
        resource
            .renew_lease(
                "native-funded-renewal",
                initial.lease_id,
                initial.lease_revision,
                new_until,
                "synthetic-renewal"
            )
            .await?,
        renewed
    );
    let grant = resource
        .environment_credit_grant(OWNER, initial.lease_id)
        .await?;
    assert_eq!(grant.hold.id, initial.hold.id);
    assert_eq!(grant.covered_from, initial.covered_from);
    assert_eq!(grant.covered_until, new_until);
    let current = admission.for_instance(&ready).await?;
    assert_eq!(current.lease_authorization.lease_revision, renewed.revision);
    let refreshed = store
        .refresh_work_lease(ready.id, current.lease_authorization.clone())
        .await?;
    resource
        .mark_lease_synced(initial.claim_id, current.lease_authorization.lease_revision)
        .await?;
    assert_eq!(refreshed.generation, ready.generation);
    assert_eq!(refreshed.owner_id, ready.owner_id);
    lifecycle_command(store, &refreshed, Kind::Stop, None).await?;
    assert!(matches!(
        run_due(&worker, store, "native-stop").await?,
        ReconcileWorkerOutcome::Advanced {
            state: State::Stopped,
            ..
        }
    ));
    let stopped = store.load(instance.id).await?;
    let retained = resource
        .next_active_environment_cleanup(None)
        .await?
        .ok_or("active native lease")?;
    let stop_status = store.load_cleanup_status(stopped.id).await?;
    assert!(!stop_status.cleanup_complete);
    assert!(matches!(
        resource
            .begin_deleted_environment_release(&retained, &stop_status, contracts::ActorId::new())
            .await,
        Err(resource_service::store::ResourceStoreError::CapacityReadbackInvalid)
    ));
    assert_eq!(
        resource.load_lease(initial.lease_id).await?.state,
        contracts::resource::ResourceLeaseState::Active
    );
    assert_eq!(
        resource
            .environment_credit_grant(OWNER, initial.lease_id)
            .await?
            .hold
            .state,
        contracts::resource::credits::CreditHoldState::Pending
    );
    http.command(&stopped, "start").await?;
    assert!(matches!(
        run_due(&worker, store, "native-restart").await?,
        ReconcileWorkerOutcome::Advanced {
            state: State::Ready,
            ..
        }
    ));
    let restarted = store.load(instance.id).await?;
    assert_eq!(restarted.generation, stopped.generation + 1);
    assert_eq!(restarted.lease_id, ready.lease_id);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    reject_next_restart.store(true, Ordering::SeqCst);
    lifecycle_command(
        store,
        &restarted,
        Kind::Restart,
        Some(current.lease_authorization.clone()),
    )
    .await?;
    assert!(matches!(
        run_due(&worker, store, "native-interrupted-restart").await?,
        ReconcileWorkerOutcome::Failed { .. }
    ));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "the failed cluster call created no replacement"
    );
    let interrupted = store.load(instance.id).await?;
    http.command(&interrupted, "recover").await?;
    assert!(matches!(
        run_due(&worker, store, "native-recovered-original-lease").await?,
        ReconcileWorkerOutcome::Advanced {
            state: State::Ready,
            ..
        }
    ));
    let restarted = store.load(instance.id).await?;
    assert_eq!(restarted.lease_id, initial.lease_id.into());
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        resource
            .environment_credit_grant(OWNER, initial.lease_id)
            .await?
            .hold
            .id,
        initial.hold.id
    );
    if container {
        let restarted = if instance.class == contracts::authoring::EnvironmentClass::Work {
            Box::pin(upgrade_guards::exercise(
                pool, store, admission, &worker, &restarted, http, &calls,
            ))
            .await?
        } else {
            restarted
        };
        lifecycle_command(store, &restarted, Kind::Delete, None).await?;
        assert!(matches!(
            run_due(&worker, store, "native-known-funded-delete").await?,
            ReconcileWorkerOutcome::Advanced {
                state: State::Deleted,
                ..
            }
        ));
        let deleted = store.load(instance.id).await?;
        executor
            .execute(physical_envelope(&deleted, 1, true, deadline)?)
            .await?;
        assert_eq!(physical_calls.load(Ordering::SeqCst), 2);
        assert_original_release(
            pool,
            resource,
            store,
            &deleted,
            initial,
            &http.usage_identity,
        )
        .await?;
        return Ok(());
    }
    let credits = resource_service::credits::PgCreditStore::new(pool.clone());
    let unknown = credits
        .resolve(
            "native-hold-unknown",
            OWNER,
            initial.hold.id,
            &contracts::resource::credits::CreditHoldResolutionRequest::Unknown {
                event_id: Some(contracts::EventId::new()),
                reason: "synthetic metering unavailable".into(),
            },
        )
        .await?;
    assert!(matches!(
        admission.for_instance(&restarted).await,
        Err(RuntimeAdmissionError::FundingInactive)
    ));
    if container {
        assert!(matches!(
            executor
                .execute(physical_envelope(&restarted, 1, false, deadline)?)
                .await,
            Err(
                environment_service::ContainerExecutorFenceError::RuntimeAdmission(
                    RuntimeAdmissionError::FundingInactive
                )
            )
        ));
    }
    // A restarted executor replays an already completed effect, despite loss of current funding.
    let recovered = environment_service::FencedContainerExecutor::new(
        environment_service::PgContainerExecutorFenceStore::new(pool.clone()),
        PhysicalClusterFixture(physical_calls.clone(), store.current_time().await?),
    )
    .with_runtime_admission(admission.clone());
    if container {
        recovered.execute(first).await?;
        assert_eq!(physical_calls.load(Ordering::SeqCst), 1);
    }
    lifecycle_command(store, &restarted, Kind::Stop, None).await?;
    assert!(matches!(
        run_due(&worker, store, "native-unfunded-stop").await?,
        ReconcileWorkerOutcome::Advanced {
            state: State::Stopped,
            ..
        }
    ));
    let stopped = store.load(instance.id).await?;
    lifecycle_command(
        store,
        &stopped,
        Kind::Restart,
        Some(current.lease_authorization),
    )
    .await?;
    assert!(
        matches!(run_due(&worker, store, "native-unfunded-restart").await?, ReconcileWorkerOutcome::Failed { ref diagnostic_code } if diagnostic_code == "LW_RUNTIME_FUNDING_INACTIVE")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    credits
        .expire_unknown_at(unknown.reconciliation_due_at.ok_or("unknown due")?)
        .await?;
    let released = resource
        .environment_credit_grant(OWNER, initial.lease_id)
        .await?;
    assert_eq!(
        released.hold.state,
        contracts::resource::credits::CreditHoldState::Released
    );
    let failed = store.load(instance.id).await?;
    assert!(matches!(
        admission.for_instance(&failed).await,
        Err(RuntimeAdmissionError::FundingInactive)
    ));
    let rejected = http.send(&failed, "recover").await?;
    assert_eq!(rejected.0, StatusCode::CONFLICT, "{}", rejected.1);
    assert_eq!(rejected.1["diagnosticCode"], "LW_RUNTIME_FUNDING_INACTIVE");
    assert_eq!(store.load(instance.id).await?.revision, failed.revision);
    if container {
        assert!(matches!(
            recovered
                .execute(physical_envelope(&failed, 1, false, deadline)?)
                .await,
            Err(
                environment_service::ContainerExecutorFenceError::RuntimeAdmission(
                    RuntimeAdmissionError::FundingInactive
                )
            )
        ));
    }
    lifecycle_command(store, &failed, Kind::Delete, None).await?;
    assert!(matches!(
        run_due(&worker, store, "native-unfunded-cleanup").await?,
        ReconcileWorkerOutcome::Advanced {
            state: State::Deleted,
            ..
        }
    ));
    let deleted = store.load(instance.id).await?;
    if container {
        recovered
            .execute(physical_envelope(&deleted, 1, true, deadline)?)
            .await?;
        assert_eq!(physical_calls.load(Ordering::SeqCst), 2);
    }
    assert_eq!(deleted.lease_id, initial.lease_id.into());
    assert_original_release(
        pool,
        resource,
        store,
        &deleted,
        initial,
        &http.usage_identity,
    )
    .await?;
    let legacy: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM resource.environment_resource_reservations WHERE environment_id=$1",
    )
    .bind(instance.id.as_uuid())
    .fetch_one(pool)
    .await?;
    assert_eq!(legacy, 0);
    let deliveries: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT request FROM environment.resource_meter_deliveries WHERE environment_id=$1",
    )
    .bind(instance.id.as_uuid())
    .fetch_all(pool)
    .await?;
    assert!(!deliveries.is_empty());
    assert!(
        deliveries
            .iter()
            .all(|v| v["target"]["kind"] == "resource_request"
                && v["target"]["requestId"] == initial.request_id.to_string())
    );
    Ok(())
}

async fn run_due(
    worker: &environment_service::ReconcileWorker,
    store: &PgEnvironmentStore,
    label: &str,
) -> Result<environment_service::ReconcileWorkerOutcome> {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let result = worker.run_once(label, store.current_time().await?).await?;
            if result != environment_service::ReconcileWorkerOutcome::Idle {
                return Ok::<_, Box<dyn std::error::Error + Send + Sync>>(result);
            }
            tokio::task::yield_now().await;
        }
    })
    .await?
}

async fn assert_original_release(
    pool: &PgPool,
    resource: &PgResourceStore,
    store: &PgEnvironmentStore,
    instance: &contracts::environment::EnvironmentInstance,
    initial: &EnvironmentCreditGrant,
    usage_identity: &auth::ServiceIdentity,
) -> Result {
    let item = resource
        .next_active_environment_cleanup(None)
        .await?
        .ok_or("active lease cleanup candidate")?;
    assert_eq!(item.lease.id, initial.lease_id);
    assert!(
        resource
            .next_active_environment_cleanup(Some(item.lease.id))
            .await?
            .is_none()
    );
    let status = store.load_cleanup_status(instance.id).await?;
    assert!(status.cleanup_complete);
    for changed in [
        contracts::environment::ResourceWorkCleanupStatus {
            capacity_binding: contracts::CapacityClaimId::new().to_string(),
            ..status.clone()
        },
        contracts::environment::ResourceWorkCleanupStatus {
            lease_revision: Revision::new(1)?,
            ..status.clone()
        },
    ] {
        assert!(matches!(
            resource
                .begin_deleted_environment_release(&item, &changed, contracts::ActorId::new())
                .await,
            Err(resource_service::store::ResourceStoreError::CapacityReadbackInvalid)
        ));
    }
    assert_usage_settlement_replay(pool, resource, store, instance, usage_identity).await?;
    let before = resource_service::credits::PgCreditStore::new(pool.clone())
        .account(instance.project_id)
        .await?;
    let releasing = resource
        .begin_deleted_environment_release(&item, &status, contracts::ActorId::new())
        .await?;
    let released = resource
        .complete_capacity_release(
            releasing.claim.id,
            releasing.claim.revision,
            releasing.lease.id,
            releasing.lease.revision,
            contracts::ActorId::new(),
            "native-delete-release",
        )
        .await?;
    assert!(matches!(
        released.state,
        contracts::resource::ResourceLeaseState::Expired
            | contracts::resource::ResourceLeaseState::Revoked
    ));
    let ledger: i64 =
        sqlx::query_scalar("SELECT count(*) FROM resource.credit_ledger_entries WHERE hold_id=$1")
            .bind(initial.hold.id)
            .fetch_one(pool)
            .await?;
    assert!(
        resource
            .complete_capacity_release(
                releasing.claim.id,
                releasing.claim.revision,
                releasing.lease.id,
                releasing.lease.revision,
                contracts::ActorId::new(),
                "native-delete-release-replay"
            )
            .await
            .is_err()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM resource.credit_ledger_entries WHERE hold_id=$1"
        )
        .bind(initial.hold.id)
        .fetch_one(pool)
        .await?,
        ledger
    );
    let after = resource_service::credits::PgCreditStore::new(pool.clone())
        .account(instance.project_id)
        .await?;
    assert_eq!(after.reserved, contracts::resource::FixedDecimal::zero());
    assert_eq!(after.spent, before.spent);
    assert!(
        resource
            .next_active_environment_cleanup(None)
            .await?
            .is_none()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM resource.credit_holds WHERE request_id=$1"
        )
        .bind(initial.request_id.as_uuid())
        .fetch_one(pool)
        .await?,
        1
    );
    Ok(())
}

async fn assert_usage_settlement_replay(
    pool: &PgPool,
    resource: &PgResourceStore,
    store: &PgEnvironmentStore,
    instance: &contracts::environment::EnvironmentInstance,
    caller: &auth::ServiceIdentity,
) -> Result {
    let requests: Vec<serde_json::Value> = sqlx::query_scalar("SELECT request FROM environment.resource_meter_deliveries WHERE environment_id=$1 ORDER BY measured_from,delivery_id")
        .bind(instance.id.as_uuid()).fetch_all(pool).await?;
    assert!(
        !requests.is_empty(),
        "physical lifecycle must produce real meter facts"
    );
    let credits = resource_service::credits::PgCreditStore::new(pool.clone());
    let mut nonzero_charge = false;
    for input in requests {
        let input: contracts::http::RecordResourceUsageRequest = serde_json::from_value(input)?;
        let observed_at = store.current_time().await?;
        let first = resource
            .record_usage_internal(
                &input,
                observed_at,
                caller,
                OWNER,
                &std::collections::BTreeSet::new(),
            )
            .await?;
        let charge = resource.settle_usage(first.id).await?;
        nonzero_charge |= charge
            .as_ref()
            .is_some_and(|value| value.total.amount.scaled() > 0);
        let account = credits.account(instance.project_id).await?;
        let ledger: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM resource.credit_ledger_entries WHERE project_id=$1",
        )
        .bind(instance.project_id.as_uuid())
        .fetch_one(pool)
        .await?;
        let replay = resource
            .record_usage_internal(
                &input,
                store.current_time().await?,
                caller,
                OWNER,
                &std::collections::BTreeSet::new(),
            )
            .await?;
        assert_eq!(replay.id, first.id);
        assert_eq!(
            resource
                .settle_usage(replay.id)
                .await?
                .map(|value| value.id),
            charge.map(|value| value.id)
        );
        let repeated = credits.account(instance.project_id).await?;
        assert_eq!(
            (
                repeated.balance,
                repeated.reserved,
                repeated.spent,
                repeated.platform_absorbed
            ),
            (
                account.balance,
                account.reserved,
                account.spent,
                account.platform_absorbed
            )
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM resource.credit_ledger_entries WHERE project_id=$1"
            )
            .bind(instance.project_id.as_uuid())
            .fetch_one(pool)
            .await?,
            ledger
        );
    }
    assert!(
        nonzero_charge,
        "the configured nonzero CPU rate must produce an actual charge even when the platform absorbs it"
    );
    Ok(())
}
