//! Real native Provider -> NATS executor -> worker diagnostics, with real Resource authorities.
use super::super::super::ExecutorFailure as Fault;
use super::super::{LifecycleHttp, PgAuthority, run_due, vm_boundary};
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
    ContainerProvider, ContainerProviderConfiguration, ContainerReleasePolicy,
    EnvironmentRuntimeAdmission, EnvironmentRuntimeAuthority, FencedContainerExecutor,
    FencedKubeVirtExecutor, KubeVirtExecutionInstance, NatsContainerExecutorServer,
    NatsContainerProviderBackend, NatsKubeVirtExecutorServer, NatsKubeVirtProviderBackend,
    PgContainerExecutorFenceStore, PgEnvironmentStore, PgKubeVirtExecutorFenceStore,
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
                Fault::ResourceUnavailable => {}
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
    http.command(initial, "start").await?;
    let starting = store.load(initial.id).await?;
    assert_eq!(starting.generation, initial.generation + 1);
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
    let providers = registry(pool, &client, &subject, &starting).await?;
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
    loop {
        attempts += 1;
        let outcome = run_due(&worker, &store, "executor-transport-denial").await?;
        let persisted = store.load(initial.id).await?;
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
            FencedContainerExecutor::new(PgContainerExecutorFenceStore::new(pool.clone()), cluster)
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
) -> Result<ProviderRegistry> {
    use environment_service::ContainerReleaseResolver as _;
    let releases = PgReleaseProjectionStore::new(pool.clone());
    let resolved = releases
        .resolve(instance.release_id, instance.release_version)
        .await?;
    let mut registry = ProviderRegistry::default();
    if let contracts::supply_chain::ImageArtifact::Container { repository, .. } =
        &resolved.projection.release.artifact
    {
        let approval = resolved
            .projection
            .release
            .approval
            .as_ref()
            .ok_or("release approval")?;
        let config = ContainerProviderConfiguration::new(
            ContainerReleasePolicy::new(
                contracts::PolicyId::new(),
                approval.policy_revision,
                approval.trust_revision,
            )?,
            repository.rsplit_once('/').ok_or("repository")?.0.into(),
            "synthetic-access".into(),
            "synthetic-access".into(),
            "registry-pull".into(),
            "synthetic-storage".into(),
            "ReadWriteOnce",
        )?;
        registry.register(Arc::new(ContainerProvider::new(
            instance.provider_binding.clone(),
            Arc::new(NatsContainerProviderBackend::new(
                client.clone(),
                subject.into(),
                Duration::from_secs(2),
            )?),
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
