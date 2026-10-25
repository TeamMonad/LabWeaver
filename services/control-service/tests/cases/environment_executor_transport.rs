//! Real native Provider -> NATS executor -> worker diagnostics, with real Resource authorities.
use super::super::super::ExecutorFailure as Fault;
use super::super::{LifecycleHttp, PgAuthority, run_due, vm_boundary};
use super::super::super::ContainerSecurityTampering as Tamper;
use super::{Cluster, Result};
use async_trait::async_trait;
use contracts::{
    authoring::RuntimeKind,
    environment::{
        EnvironmentInstance, EnvironmentLeaseAuthorization,
        EnvironmentRuntimeAuthorizationResolution, ResolveEnvironmentRuntimeAuthorizationRequest,
    },
};
use environment_service::{
    ContainerProvider, ContainerProviderBackend,
    EnvironmentRuntimeAdmission, EnvironmentRuntimeAuthority, FencedContainerExecutor,
    FencedKubeVirtExecutor, KubeVirtExecutionInstance, NatsContainerExecutorServer,
    NatsContainerProviderBackend, NatsKubeVirtExecutorServer, NatsKubeVirtProviderBackend,
    PgEnvironmentStore, PgKubeVirtExecutorFenceStore,
    PgReleaseProjectionStore, ProviderRegistry, ReconcileWorker, ReconcileWorkerOutcome,
    Reconciler, ResourceUsageClientError,
};
use resource_service::store::PgResourceStore;
use sqlx::PgPool;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use testcontainers::{
    GenericImage,
    core::{IntoContainerPort, WaitFor},
    runners::AsyncRunner,
};

impl Fault {
    const fn code(self) -> &'static str {
        match self {
            Self::Lease => "LW_RUNTIME_LEASE_NOT_ACTIVE",
            Self::Funding => "LW_RUNTIME_FUNDING_INACTIVE",
            Self::ResourceUnavailable => "LW_RUNTIME_AUTHORIZATION_INPUT_UNAVAILABLE",
            Self::Security(_) => "LW_ENVIRONMENT_CONTAINER_EXECUTOR_IDENTITY_MISMATCH",
        }
    }
}

// Return the real owner's read unchanged, then change a real authoritative fact before the NATS
// executor's independent read. This deterministically exercises the inter-process recheck window.
struct Interleave {
    authority: PgAuthority,
    pool: PgPool,
    fault: Fault,
    fired: AtomicBool,
}
#[async_trait]
impl EnvironmentRuntimeAuthority for Interleave {
    async fn resolve_runtime_authorization(
        &self,
        request: &ResolveEnvironmentRuntimeAuthorizationRequest,
    ) -> std::result::Result<EnvironmentRuntimeAuthorizationResolution, ResourceUsageClientError>
    {
        self.authority.resolve_runtime_authorization(request).await
    }
    async fn resolve_credit_grant(
        &self,
        lease: &EnvironmentLeaseAuthorization,
    ) -> std::result::Result<
        contracts::resource::environment_funding::EnvironmentCreditGrant,
        ResourceUsageClientError,
    > {
        let grant = self.authority.resolve_credit_grant(lease).await?;
        if !self.fired.swap(true, Ordering::SeqCst) {
            match self.fault {
                Fault::Lease => {
                    sqlx::query("UPDATE resource.resource_leases SET state='revoked',contract=jsonb_set(contract,'{state}','\"revoked\"') WHERE lease_id=$1")
                        .bind(lease.lease_id.as_uuid()).execute(&self.pool).await.map_err(|_| ResourceUsageClientError::InvalidResponse)?;
                }
                Fault::Funding => {
                    sqlx::query("UPDATE resource.credit_holds SET measurement_unknown=true WHERE hold_id=$1")
                        .bind(grant.hold.id).execute(&self.pool).await.map_err(|_| ResourceUsageClientError::InvalidResponse)?;
                }
                Fault::ResourceUnavailable | Fault::Security(_) => {}
            }
        }
        Ok(grant)
    }
}

pub(in super::super) async fn exercise(
    pool: &PgPool,
    resource: &PgResourceStore,
    admission: &EnvironmentRuntimeAdmission,
    initial: &EnvironmentInstance,
    http: &LifecycleHttp,
    fault: Fault,
) -> Result {
    let broker = GenericImage::new("nats", "2.11.8-alpine")
        .with_exposed_port(4222.tcp())
        .with_wait_for(WaitFor::message_on_stderr("Server is ready"))
        .start()
        .await?;
    let client = async_nats::connect(format!(
        "nats://127.0.0.1:{}",
        broker.get_host_port_ipv4(4222).await?
    ))
    .await?;
    let store = PgEnvironmentStore::new(pool.clone());
    let initial_revocations = http.revocations.load(Ordering::SeqCst);
    assert!(
        initial.endpoints.is_empty(),
        "stopped fixture must have no live endpoints"
    );
    admission.for_instance(initial).await?;
    let starting = if matches!(fault, Fault::Security(_)) {
        initial.clone()
    } else {
        http.command(initial, "start").await?;
        let starting = store.load(initial.id).await?;
        assert_eq!(starting.generation, initial.generation + 1);
        starting
    };
    let closed =
        sqlx::postgres::PgPoolOptions::new().connect_lazy_with((*pool.connect_options()).clone());
    closed.close().await;
    let executor_authority = if matches!(fault, Fault::ResourceUnavailable) {
        PgAuthority(PgResourceStore::new(closed.clone()), closed)
    } else {
        PgAuthority(resource.clone(), pool.clone())
    };
    let executor_admission = EnvironmentRuntimeAdmission::new(
        PgReleaseProjectionStore::new(pool.clone()),
        Arc::new(executor_authority),
    );
    let owner_admission = EnvironmentRuntimeAdmission::new(
        PgReleaseProjectionStore::new(pool.clone()),
        Arc::new(Interleave {
            authority: PgAuthority(resource.clone(), pool.clone()),
            pool: pool.clone(),
            fault,
            fired: AtomicBool::new(false),
        }),
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let subject = format!("synthetic.executor.gate.{}", uuid::Uuid::new_v4());
    let server = start_server(
        pool,
        &client,
        &subject,
        &starting,
        executor_admission,
        calls.clone(),
    )
    .await?;
    let providers = registry(pool, &client, &subject, &starting, fault).await?;
    let worker = ReconcileWorker::new(
        store.clone(),
        Reconciler::new(providers, Duration::from_secs(3))?.with_runtime_admission(owner_admission),
        Duration::from_secs(5),
        Duration::from_millis(10),
    )?;
    // Setup performs real release reads before flushing the shared NATS client.
    tokio::task::yield_now().await;
    client.flush().await?;
    let mut attempts = 0;
    let mut transitions = 0;
    loop {
        transitions += 1;
        assert!(transitions <= 12, "native denial must complete within bounded lifecycle steps");
        let outcome = run_due(&worker, &store, "executor-transport-denial").await?;
        let persisted = store.load(initial.id).await?;
        if matches!(fault, Fault::Security(_)) && matches!(outcome, ReconcileWorkerOutcome::Advanced { terminal: false, .. }) {
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert!(persisted.last_diagnostic_code.is_none());
            continue;
        }
        attempts += 1;
        assert_eq!(
            persisted.last_diagnostic_code.as_deref(),
            Some(fault.code())
        );
        assert_eq!(
            persisted.operation.diagnostic_code.as_deref(),
            Some(fault.code())
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "denied NATS effect never reaches Kubernetes"
        );
        assert!(
            attempts <= starting.operation.max_attempts,
            "retry budget must be bounded"
        );
        match outcome {
            ReconcileWorkerOutcome::RetryScheduled { .. } => {
                assert!(matches!(fault, Fault::ResourceUnavailable));
            }
            ReconcileWorkerOutcome::Failed { diagnostic_code } => {
                assert_eq!(diagnostic_code, fault.code());
                assert!(persisted.operation.state.is_terminal());
                break;
            }
            other => return Err(format!("unexpected transport denial outcome: {other:?}").into()),
        }
    }
    if matches!(fault, Fault::ResourceUnavailable) {
        assert_eq!(attempts, starting.operation.max_attempts);
    } else {
        assert_eq!(attempts, 1);
    }
    assert_eq!(
        http.revocations.load(Ordering::SeqCst),
        initial_revocations,
        "stopped Start must not require Access revocation"
    );
    server.abort();
    let _ = server.await;
    Ok(())
}

async fn start_server(
    pool: &PgPool,
    client: &async_nats::Client,
    subject: &str,
    instance: &EnvironmentInstance,
    admission: EnvironmentRuntimeAdmission,
    calls: Arc<AtomicUsize>,
) -> Result<tokio::task::JoinHandle<()>> {
    let cluster = Cluster(
        calls,
        PgEnvironmentStore::new(pool.clone()).current_time().await?,
    );
    if instance.runtime_kind == RuntimeKind::Container {
        let server = NatsContainerExecutorServer::new(
            client.clone(),
            subject.into(),
            FencedContainerExecutor::new(super::super::container_fence_store(pool.clone(), instance).await?, cluster)
                .with_runtime_admission(admission),
        )?;
        Ok(tokio::spawn(async move {
            let _ = server.serve().await;
        }))
    } else {
        let server = NatsKubeVirtExecutorServer::new(
            client.clone(),
            subject.into(),
            FencedKubeVirtExecutor::new(
                PgKubeVirtExecutorFenceStore::new(pool.clone()),
                cluster,
                KubeVirtExecutionInstance {
                    namespace: "synthetic-executors".into(),
                    pod_name: "executor".into(),
                    pod_uid: uuid::Uuid::new_v4(),
                    container_name: "executor".into(),
                    boot_token: uuid::Uuid::new_v4(),
                },
            )
            .with_runtime_admission(admission),
        )?;
        Ok(tokio::spawn(async move {
            let (_shutdown, receiver) = tokio::sync::watch::channel(false);
            let _ = server.serve(receiver).await;
        }))
    }
}

async fn registry(
    pool: &PgPool,
    client: &async_nats::Client,
    subject: &str,
    instance: &EnvironmentInstance,
    fault: Fault,
) -> Result<ProviderRegistry> {
    use environment_service::ContainerReleaseResolver as _;
    let releases = PgReleaseProjectionStore::new(pool.clone());
    let resolved = releases
        .resolve(instance.release_id, instance.release_version)
        .await?;
    let mut registry = ProviderRegistry::default();
    if matches!(&resolved.projection.release.artifact, contracts::supply_chain::ImageArtifact::Container { .. })
    {
        let config = super::super::projection_configuration(&resolved.projection)?;
        registry.register(Arc::new(ContainerProvider::new(
            instance.provider_binding.clone(),
            Arc::new(TamperingContainerTransport {
                transport: NatsContainerProviderBackend::new(client.clone(), subject.into(), Duration::from_secs(2))?,
                tampering: if let Fault::Security(tamper) = fault { Some(tamper) } else { None },
            }),
            Arc::new(releases),
            config,
        )?))?;
    } else {
        vm_boundary::register_backend(
            &mut registry,
            pool,
            &resolved.projection,
            Arc::new(
                NatsKubeVirtProviderBackend::new(
                    client.clone(),
                    subject.into(),
                    Duration::from_secs(2),
                )
                .map_err(|failure| std::io::Error::other(failure.diagnostic_code()))?,
            ),
        )?;
    }
    Ok(registry)
}

// Only the external transport boundary is altered. The native client binds each changed Apply
// into a fresh request digest; Control, Resource, Provider, executor and worker remain real.
struct TamperingContainerTransport {
    transport: NatsContainerProviderBackend,
    tampering: Option<Tamper>,
}

impl Tamper {
    fn apply(self, plan: &mut environment_service::ContainerResourcePlan) -> Result {
        use serde_json::json;
        if self == Self::NamespacePsa {
            let namespace = plan.resources.iter_mut().find(|r| r.kind == "Namespace").ok_or("namespace")?;
            assert_eq!(namespace.document["metadata"]["labels"]["pod-security.kubernetes.io/enforce"], "baseline");
            namespace.document["metadata"]["labels"]["pod-security.kubernetes.io/enforce"] = json!("privileged");
            return Ok(());
        }
        let deployment = plan.resources.iter_mut().find(|r| r.kind == "Deployment").ok_or("deployment")?;
        let pod = deployment.document.pointer_mut("/spec/template/spec").ok_or("pod")?;
        match self {
            Self::HostNetwork => pod["hostNetwork"] = json!(true),
            Self::HostPid => pod["hostPID"] = json!(true),
            Self::HostIpc => pod["hostIPC"] = json!(true),
            Self::HostPath => {
                pod["volumes"].as_array_mut().ok_or("volumes")?.push(json!({"name":"host-root","hostPath":{"path":"/","type":"Directory"}}));
                pod["containers"][0]["volumeMounts"].as_array_mut().ok_or("mounts")?.push(json!({"name":"host-root","mountPath":"/host"}));
            }
            Self::ExtraContainer => {
                let mut extra = pod["containers"][0].clone();
                extra["name"] = json!("extra-runtime");
                pod["containers"].as_array_mut().ok_or("containers")?.push(extra);
            }
            Self::ServiceAccount => pod["serviceAccountName"] = json!("default"),
            Self::ServiceAccountToken => {
                pod["automountServiceAccountToken"] = json!(true);
                pod["volumes"].as_array_mut().ok_or("volumes")?.push(json!({"name":"api-token","projected":{"sources":[{"serviceAccountToken":{"path":"token","expirationSeconds":600}}]}}));
                pod["containers"][0]["volumeMounts"].as_array_mut().ok_or("mounts")?.push(json!({"name":"api-token","mountPath":"/var/run/api-token","readOnly":true}));
            }
            Self::PrivilegeEscalation => {
                pod["containers"][0]["securityContext"]["allowPrivilegeEscalation"] = json!(true);
                pod["initContainers"][0]["securityContext"]["allowPrivilegeEscalation"] = json!(true);
            }
            Self::NamespacePsa => return Err("namespace branch must be handled first".into()),
        }
        Ok(())
    }
}

#[async_trait]
impl environment_service::ContainerProviderBackend for TamperingContainerTransport {
    async fn apply(&self, fence: &environment_service::ContainerBackendFence, plan: &environment_service::ContainerResourcePlan)
    -> std::result::Result<environment_service::ContainerApplyObservation, environment_service::ProviderFailure> {
        if let Some(tampering) = self.tampering {
            let mut changed = plan.clone();
            tampering.apply(&mut changed).map_err(|_| environment_service::ProviderFailure {
                code: environment_service::ProviderFailureCode::Rejected, retryable: false,
            })?;
            self.transport.apply(fence, &changed).await
        } else { self.transport.apply(fence, plan).await }
    }

    async fn observe(&self, fence: &environment_service::ContainerBackendFence, plan: &environment_service::ContainerResourcePlan)
    -> std::result::Result<environment_service::ContainerApplyObservation, environment_service::ProviderFailure> {
        self.transport.observe(fence, plan).await
    }

    async fn scale(&self, fence: &environment_service::ContainerBackendFence, plan: &environment_service::ContainerResourcePlan, replicas: u32)
    -> std::result::Result<environment_service::ContainerApplyObservation, environment_service::ProviderFailure> {
        self.transport.scale(fence, plan, replicas).await
    }

    async fn restart(&self, fence: &environment_service::ContainerBackendFence, plan: &environment_service::ContainerResourcePlan, revision: contracts::Revision)
    -> std::result::Result<environment_service::ContainerApplyObservation, environment_service::ProviderFailure> {
        self.transport.restart(fence, plan, revision).await
    }

    async fn delete_namespace(&self, fence: &environment_service::ContainerBackendFence, plan: &environment_service::ContainerResourcePlan)
    -> std::result::Result<contracts::ArtifactRef, environment_service::ProviderFailure> {
        self.transport.delete_namespace(fence, plan).await
    }
}
