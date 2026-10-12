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
use std::{collections::BTreeSet, sync::Arc, time::Duration};
use testcontainers::core::{IntoContainerPort, WaitFor};
use testcontainers::{GenericImage, runners::AsyncRunner};
use tower::ServiceExt;
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
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
        let policy = &value.applied_configuration.configuration.runtime_policy;
        Ok(EnvironmentRuntimeAuthorizationResolution {
            valid_until: value.lease.expires_at.min(value.approval_valid_until),
            provider_binding: value.provider_binding,
            owner_actor_id: value.lease.owner_actor_id,
            default_container_uid: policy.default_container_uid,
            privileged_execution_location: policy.privileged_execution_location.clone(),
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
    super::super::apply_domain_migrations(pool, persistence_sqlx::Domain::Environment).await?;
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
    assert_ne!(
        projection.release.project_id, original.lease.project_id,
        "the normal classroom payer differs from the teacher's authoring project"
    );
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
        contracts::authoring::EnvironmentClass::Experiment
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
    let caller = auth::ServiceIdentity {
        issuer: "synthetic".into(),
        subject: "synthetic-resource".into(),
        client_id: "synthetic-resource".into(),
        expires_at: time::OffsetDateTime::now_utc() + time::Duration::hours(1),
        permissions: BTreeSet::from(["resource.environment.manage".into()]),
    };
    let send = |router: Router, body: serde_json::Value| {
        let caller = caller.clone();
        async move {
            let mut req = Request::builder()
                .method("POST")
                .uri("/internal/v1/resource/work-handoffs")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))?;
            req.extensions_mut().insert(caller);
            req.extensions_mut()
                .insert(telemetry::RequestContext::generate());
            let response = router.oneshot(req).await?;
            let status = response.status();
            let body = to_bytes(response.into_body(), 1_048_576).await?;
            Ok::<_, Box<dyn std::error::Error>>((
                status,
                serde_json::from_slice::<serde_json::Value>(&body)?,
            ))
        }
    };
    let first = send(api.clone(), serde_json::to_value(&handoff)?).await?;
    assert_eq!(first.0, StatusCode::ACCEPTED, "{}", first.1);
    let replay = send(api.clone(), serde_json::to_value(&handoff)?).await?;
    assert_eq!(replay, first);
    resource
        .mark_capacity_handed_off(
            claim.id,
            claim.revision,
            original.lease.lease_id,
            original.lease.lease_revision,
        )
        .await?;
    let instance = store.load(handoff.environment_id).await?;
    assert_eq!(
        instance.class,
        contracts::authoring::EnvironmentClass::Experiment
    );
    assert!(instance.has_resource_lease());
    assert_eq!(instance.generation, 1);
    assert_eq!(
        instance.operation.lease_authorization.as_ref(),
        Some(&original.lease)
    );
    let public = contracts::http::CreateEnvironmentRequest {
        resource_request_id: request.id,
        project_id: request.project_id,
        course_id: request.course_id,
        release_id: projection.release.id,
        release_version: projection.release.version,
        display_label: None,
    };
    let attached = store
        .bind_funded_api_create(
            "funded-browser-create",
            &public,
            request.requester_id,
            instance.id,
        )
        .await?;
    assert_eq!(attached.environment_id, instance.id);
    assert_eq!(attached.operation_id, instance.operation.id);
    assert_eq!(
        store
            .bind_funded_api_create(
                "funded-browser-create",
                &public,
                request.requester_id,
                instance.id
            )
            .await?,
        attached
    );
    assert_eq!(
        admission.for_instance(&instance).await?.lease_authorization,
        original.lease
    );
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
        send(api, serde_json::to_value(&foreign)?).await?.0,
        StatusCode::ACCEPTED
    );
    Box::pin(exercise_execution(
        pool, resource, &store, &admission, &instance, &grant,
    ))
    .await?;
    replies.abort();
    Ok(())
}

// Real lifecycle/ledger; only the cluster observation boundary is synthetic.
struct ClusterFixture {
    store: PgEnvironmentStore,
    binding: String,
    calls: Arc<std::sync::atomic::AtomicUsize>,
}
#[async_trait]
impl environment_service::EnvironmentProvider for ClusterFixture {
    fn binding(&self) -> &str {
        &self.binding
    }
    async fn execute(
        &self,
        action: environment_service::ReconcileAction,
        instance: &contracts::environment::EnvironmentInstance,
    ) -> std::result::Result<
        environment_service::ProviderOutcome<environment_service::ProviderObservation>,
        environment_service::ProviderFailure,
    > {
        use contracts::environment::ObservedEnvironmentState as State;
        use environment_service::ReconcileAction as Action;
        let next = match (action, instance.observed_state) {
            (Action::Validate, State::Requested) => State::Validating,
            (Action::Validate, State::Validating) => State::Building,
            (Action::Build, State::Building) => State::Provisioning,
            (Action::Provision | Action::Restart | Action::Start | Action::Reset, _) => {
                self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                State::Ready
            }
            (Action::Stop, _) => State::Stopped,
            (Action::Cleanup, _) => State::Deleted,
            _ => {
                return Err(environment_service::ProviderFailure {
                    code: environment_service::ProviderFailureCode::Rejected,
                    retryable: false,
                });
            }
        };
        let endpoints = if next == State::Ready {
            vec![contracts::environment::EnvironmentEndpoint {
                id: contracts::EndpointId::new(),
                protocol: contracts::environment::EndpointProtocol::Https,
                revision: Revision::new(instance.revision.get() + 1).map_err(|_| {
                    environment_service::ProviderFailure {
                        code: environment_service::ProviderFailureCode::Rejected,
                        retryable: false,
                    }
                })?,
                health: contracts::environment::EndpointHealth::Healthy,
                observed_at: self.store.current_time().await.map_err(|_| {
                    environment_service::ProviderFailure {
                        code: environment_service::ProviderFailureCode::Rejected,
                        retryable: false,
                    }
                })?,
            }]
        } else {
            vec![]
        };
        Ok(environment_service::ProviderOutcome::Completed(
            environment_service::ProviderObservation {
                next_state: next,
                endpoints,
                cleanup_evidence: (next == State::Deleted).then(cleanup_evidence),
                operation_complete: matches!(next, State::Ready | State::Stopped | State::Deleted),
            },
        ))
    }
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
                .then_some(instance.revision),
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
) -> Result {
    use contracts::environment::{
        EnvironmentOperationKind as Kind, ObservedEnvironmentState as State,
    };
    use environment_service::{ReconcileWorkerOutcome, RuntimeAdmissionError};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = Arc::new(AtomicUsize::new(0));
    let mut registry = environment_service::ProviderRegistry::default();
    registry.register(Arc::new(ClusterFixture {
        store: store.clone(),
        binding: instance.provider_binding.clone(),
        calls: calls.clone(),
    }))?;
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
    assert_eq!(ready.lease_id, instance.lease_id);
    let physical_calls = Arc::new(AtomicUsize::new(0));
    let executor = environment_service::FencedContainerExecutor::new(
        environment_service::PgContainerExecutorFenceStore::new(pool.clone()),
        PhysicalClusterFixture(physical_calls.clone(), store.current_time().await?),
    )
    .with_runtime_admission(admission.clone());
    let deadline =
        UtcTimestamp::from_utc(store.current_time().await?.get() + time::Duration::seconds(30))?;
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
    let first = physical_envelope(&ready, 1, false, deadline)?;
    executor.execute(first.clone()).await?;
    assert_eq!(physical_calls.load(Ordering::SeqCst), 1);
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
    lifecycle_command(
        store,
        &stopped,
        Kind::Restart,
        Some(current.lease_authorization.clone()),
    )
    .await?;
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
    let credits = resource_service::credits::PgCreditStore::new(pool.clone());
    let unknown = credits
        .resolve(
            "native-hold-unknown",
            OWNER,
            initial.hold.id,
            &contracts::resource::credits::CreditHoldResolutionRequest::Unknown {
                reason: "synthetic metering unavailable".into(),
            },
        )
        .await?;
    assert!(matches!(
        admission.for_instance(&restarted).await,
        Err(RuntimeAdmissionError::FundingInactive)
    ));
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
    // A restarted executor replays an already completed effect, despite loss of current funding.
    let recovered = environment_service::FencedContainerExecutor::new(
        environment_service::PgContainerExecutorFenceStore::new(pool.clone()),
        PhysicalClusterFixture(physical_calls.clone(), store.current_time().await?),
    )
    .with_runtime_admission(admission.clone());
    recovered.execute(first).await?;
    assert_eq!(physical_calls.load(Ordering::SeqCst), 1);
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
    assert_eq!(calls.load(Ordering::SeqCst), 2);
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
    lifecycle_command(store, &failed, Kind::Delete, None).await?;
    assert!(matches!(
        run_due(&worker, store, "native-unfunded-cleanup").await?,
        ReconcileWorkerOutcome::Advanced {
            state: State::Deleted,
            ..
        }
    ));
    let deleted = store.load(instance.id).await?;
    recovered
        .execute(physical_envelope(&deleted, 1, true, deadline)?)
        .await?;
    assert_eq!(physical_calls.load(Ordering::SeqCst), 2);
    assert_eq!(deleted.lease_id, initial.lease_id.into());
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
                return Ok::<_, Box<dyn std::error::Error>>(result);
            }
            tokio::task::yield_now().await;
        }
    })
    .await?
}
