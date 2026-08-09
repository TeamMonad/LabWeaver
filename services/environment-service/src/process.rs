use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_nats::connection::State as NatsConnectionState;
use contracts::environment::EnvironmentOperationKind;
use contracts::{ActorId, PolicyId, Revision, UtcTimestamp};
use serde::Deserialize;
use sqlx::postgres::PgPoolOptions;
use tokio::sync::watch;

use crate::{
    ContainerProvider, ContainerProviderConfiguration, ContainerReleasePolicy,
    EnvironmentStoreError, FreezeBindingConfiguration, FreezeBindingService,
    JetStreamCommandConsumer, JetStreamEventPublisher, JetStreamReleaseConsumer, KubeVirtProvider,
    KubeVirtProviderConfiguration, KubeVirtResourceBudget, KubeVirtSshBootstrap,
    KubeVirtStorageBinding, LifecycleCommand, NatsAccessRevoker, NatsContainerProviderBackend,
    NatsEnvironmentProvider, NatsKubeVirtProviderBackend, NatsMessagingError,
    NatsResourceLeaseVerifier, OutboxDispatchError, OutboxDispatcher, PgEnvironmentStore,
    PgKubeVirtObservationStore, PgReleaseProjectionStore, ProviderRegistry, ReconcileError,
    ReconcileWorker, ReconcileWorkerError, Reconciler, connect_nats_mtls,
};

const DATABASE_URL: &str = "LABWEAVER_DATABASE_URL";
const NATS_SERVER: &str = "LABWEAVER_NATS_SERVER";
const NATS_CA_PATH: &str = "LABWEAVER_NATS_CA_PATH";
const NATS_CLIENT_CERTIFICATE_PATH: &str = "LABWEAVER_NATS_CLIENT_CERT_PATH";
const NATS_CLIENT_PRIVATE_KEY_PATH: &str = "LABWEAVER_NATS_CLIENT_KEY_PATH";
const NATS_CREDENTIALS_PATH: &str = "LABWEAVER_NATS_CREDENTIALS_PATH";
const COMMAND_STREAM: &str = "LABWEAVER_ENVIRONMENT_COMMAND_STREAM";
const COMMAND_CONSUMER: &str = "LABWEAVER_ENVIRONMENT_COMMAND_CONSUMER";
const COMMAND_QUARANTINE_SUBJECT: &str = "LABWEAVER_ENVIRONMENT_COMMAND_QUARANTINE_SUBJECT";
const RELEASE_STREAM: &str = "LABWEAVER_ENVIRONMENT_RELEASE_STREAM";
const RELEASE_CONSUMER: &str = "LABWEAVER_ENVIRONMENT_RELEASE_CONSUMER";
const RELEASE_QUARANTINE_SUBJECT: &str = "LABWEAVER_ENVIRONMENT_RELEASE_QUARANTINE_SUBJECT";
const PROVIDER_BINDINGS_PATH: &str = "LABWEAVER_ENVIRONMENT_PROVIDER_BINDINGS_PATH";
const ACCESS_REVOCATION_SUBJECT: &str = "LABWEAVER_ACCESS_REVOCATION_SUBJECT";
const RESOURCE_LEASE_VERIFICATION_SUBJECT: &str = "LABWEAVER_RESOURCE_LEASE_VERIFICATION_SUBJECT";
const WORKER_ID: &str = "LABWEAVER_ENVIRONMENT_WORKER_ID";
const SYSTEM_ACTOR_ID: &str = "LABWEAVER_ENVIRONMENT_SYSTEM_ACTOR_ID";

const PROVIDER_TIMEOUT: Duration = Duration::from_secs(150);
const EXECUTOR_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const RECONCILE_LEASE: Duration = Duration::from_secs(180);
const RETRY_DELAY: Duration = Duration::from_secs(1);
const OUTBOX_TIMEOUT: Duration = Duration::from_secs(5);
const ACCESS_REVOCATION_TIMEOUT: Duration = Duration::from_secs(5);
const RESOURCE_LEASE_VERIFICATION_TIMEOUT: Duration = Duration::from_secs(5);
const WORKER_INTERVAL: Duration = Duration::from_millis(100);
const EXPIRY_INTERVAL: Duration = Duration::from_secs(1);
const READINESS_INTERVAL: Duration = Duration::from_secs(1);
const EXPIRY_OPERATION_BUDGET: time::Duration = time::Duration::minutes(5);

/// Production lifecycle runtime containing command consumption, reconcile, expiry, and Outbox loops.
pub struct EnvironmentProcessRuntime {
    store: PgEnvironmentStore,
    command_consumer: JetStreamCommandConsumer,
    release_store: PgReleaseProjectionStore,
    release_consumer: JetStreamReleaseConsumer,
    worker: ReconcileWorker,
    outbox: OutboxDispatcher<JetStreamEventPublisher>,
    access_revoker: NatsAccessRevoker,
    lease_verifier: NatsResourceLeaseVerifier,
    nats: async_nats::Client,
    worker_id: String,
    system_actor_id: ActorId,
    readiness: Arc<AtomicBool>,
    expiry_ready: Arc<AtomicBool>,
    freeze_bindings: FreezeBindingService,
}

impl EnvironmentProcessRuntime {
    /// Loads explicit `PostgreSQL`, mTLS NATS, consumer, provider, and worker configuration.
    #[allow(
        clippy::too_many_lines,
        reason = "startup keeps all fail-closed dependency bindings visible in one initialization boundary"
    )]
    pub async fn from_env() -> Result<Self, EnvironmentProcessRuntimeError> {
        let database_url = required(DATABASE_URL)?;
        let pool = PgPoolOptions::new()
            .max_connections(16)
            .connect(&database_url)
            .await?;
        require_schema(&pool).await?;
        let store = PgEnvironmentStore::new(pool.clone());

        let nats = connect_nats_mtls(
            &required(NATS_SERVER)?,
            required_path(NATS_CA_PATH)?,
            required_path(NATS_CLIENT_CERTIFICATE_PATH)?,
            required_path(NATS_CLIENT_PRIVATE_KEY_PATH)?,
            required_path(NATS_CREDENTIALS_PATH)?,
        )
        .await?;
        let command_consumer = JetStreamCommandConsumer::bind(
            nats.clone(),
            &required(COMMAND_STREAM)?,
            &required(COMMAND_CONSUMER)?,
            &required(COMMAND_QUARANTINE_SUBJECT)?,
        )
        .await?;
        let release_store = PgReleaseProjectionStore::new(pool.clone());
        let release_consumer = JetStreamReleaseConsumer::bind(
            nats.clone(),
            &required(RELEASE_STREAM)?,
            &required(RELEASE_CONSUMER)?,
            &required(RELEASE_QUARANTINE_SUBJECT)?,
        )
        .await?;

        let provider_bindings = load_provider_bindings(&required_path(PROVIDER_BINDINGS_PATH)?)?;
        let container_freeze_configuration =
            single_provider_configuration(&provider_bindings, "container")?;
        let vm_freeze_configuration =
            single_provider_configuration(&provider_bindings, "kubevirt")?;
        let freeze_bindings = FreezeBindingService::new(
            pool.clone(),
            release_store.clone(),
            FreezeBindingConfiguration {
                container_workspace_storage_class: container_freeze_configuration
                    .workspace_storage_class_name
                    .clone()
                    .ok_or(EnvironmentProcessRuntimeError::ConfigParse)?,
                vm_username: vm_freeze_configuration
                    .guest_user
                    .clone()
                    .ok_or(EnvironmentProcessRuntimeError::ConfigParse)?,
                vm_workspace_root: vm_freeze_configuration
                    .collector_workspace_root
                    .clone()
                    .ok_or(EnvironmentProcessRuntimeError::ConfigParse)?,
                ssh_user_ca_public_key: vm_freeze_configuration
                    .ssh_user_ca_public_key
                    .clone()
                    .ok_or(EnvironmentProcessRuntimeError::ConfigParse)?,
                ssh_user_ca_private_key_path: vm_freeze_configuration
                    .ssh_user_ca_private_key_path
                    .clone()
                    .ok_or(EnvironmentProcessRuntimeError::ConfigParse)?,
            },
        )?;
        let mut registry = ProviderRegistry::default();
        for configuration in provider_bindings {
            match configuration.provider_kind.as_deref().unwrap_or("remote") {
                "remote" => {
                    if configuration.has_provider_specific_fields() {
                        return Err(EnvironmentProcessRuntimeError::ConfigParse);
                    }
                    registry.register(Arc::new(NatsEnvironmentProvider::new(
                        configuration.binding,
                        configuration.subject,
                        nats.clone(),
                    )?))?;
                }
                "container" => {
                    if configuration.has_kubevirt_fields()
                        || !configuration.has_complete_container_fields()
                    {
                        return Err(EnvironmentProcessRuntimeError::ConfigParse);
                    }
                    let backend = Arc::new(NatsContainerProviderBackend::new(
                        nats.clone(),
                        configuration.subject.clone(),
                        EXECUTOR_REQUEST_TIMEOUT,
                    )?);
                    let provider = ContainerProvider::new(
                        configuration.binding.clone(),
                        backend,
                        Arc::new(release_store.clone()),
                        ContainerProviderConfiguration::new(
                            configuration.release_policy()?,
                            configuration
                                .image_repository_prefix
                                .clone()
                                .ok_or(EnvironmentProcessRuntimeError::ConfigParse)?,
                            configuration
                                .access_namespace
                                .clone()
                                .ok_or(EnvironmentProcessRuntimeError::ConfigParse)?,
                            configuration
                                .access_pod_label
                                .clone()
                                .ok_or(EnvironmentProcessRuntimeError::ConfigParse)?,
                            configuration
                                .image_pull_secret_name
                                .clone()
                                .ok_or(EnvironmentProcessRuntimeError::ConfigParse)?,
                            configuration
                                .workspace_storage_class_name
                                .clone()
                                .ok_or(EnvironmentProcessRuntimeError::ConfigParse)?,
                        )?,
                    )?;
                    registry.register(Arc::new(provider))?;
                }
                "kubevirt" => {
                    if configuration.has_container_only_fields()
                        || !configuration.has_complete_kubevirt_fields()
                    {
                        return Err(EnvironmentProcessRuntimeError::ConfigParse);
                    }
                    let backend = Arc::new(
                        NatsKubeVirtProviderBackend::new(
                            nats.clone(),
                            configuration.subject.clone(),
                            EXECUTOR_REQUEST_TIMEOUT,
                        )
                        .map_err(|_| EnvironmentProcessRuntimeError::ConfigParse)?,
                    );
                    let provider = KubeVirtProvider::new(
                        configuration.binding.clone(),
                        backend,
                        Arc::new(release_store.clone()),
                        Arc::new(PgKubeVirtObservationStore::new(pool.clone())),
                        KubeVirtProviderConfiguration::new(
                            configuration.trust_revision()?,
                            KubeVirtStorageBinding::new(
                                configuration
                                    .storage_class_binding
                                    .clone()
                                    .ok_or(EnvironmentProcessRuntimeError::ConfigParse)?,
                                configuration
                                    .storage_class_name
                                    .clone()
                                    .ok_or(EnvironmentProcessRuntimeError::ConfigParse)?,
                                configuration
                                    .data_source_namespace
                                    .clone()
                                    .ok_or(EnvironmentProcessRuntimeError::ConfigParse)?,
                                configuration
                                    .data_source_name
                                    .clone()
                                    .ok_or(EnvironmentProcessRuntimeError::ConfigParse)?,
                            )?,
                            KubeVirtSshBootstrap::new(
                                configuration
                                    .gateway_namespace
                                    .clone()
                                    .ok_or(EnvironmentProcessRuntimeError::ConfigParse)?,
                                configuration
                                    .gateway_pod_label
                                    .clone()
                                    .ok_or(EnvironmentProcessRuntimeError::ConfigParse)?,
                                configuration
                                    .collector_namespace
                                    .clone()
                                    .ok_or(EnvironmentProcessRuntimeError::ConfigParse)?,
                                configuration
                                    .collector_pod_label
                                    .clone()
                                    .ok_or(EnvironmentProcessRuntimeError::ConfigParse)?,
                                configuration
                                    .guest_user
                                    .clone()
                                    .ok_or(EnvironmentProcessRuntimeError::ConfigParse)?,
                                configuration
                                    .ssh_user_ca_public_key
                                    .as_deref()
                                    .ok_or(EnvironmentProcessRuntimeError::ConfigParse)?,
                            )?,
                            KubeVirtResourceBudget::new(
                                configuration
                                    .vmi_memory_overhead_bytes
                                    .ok_or(EnvironmentProcessRuntimeError::ConfigParse)?,
                                configuration
                                    .cdi_importer_cpu_request_millicores
                                    .ok_or(EnvironmentProcessRuntimeError::ConfigParse)?,
                                configuration
                                    .cdi_importer_cpu_limit_millicores
                                    .ok_or(EnvironmentProcessRuntimeError::ConfigParse)?,
                                configuration
                                    .cdi_importer_memory_request_bytes
                                    .ok_or(EnvironmentProcessRuntimeError::ConfigParse)?,
                                configuration
                                    .cdi_importer_memory_limit_bytes
                                    .ok_or(EnvironmentProcessRuntimeError::ConfigParse)?,
                                configuration
                                    .cdi_scratch_storage_bytes
                                    .ok_or(EnvironmentProcessRuntimeError::ConfigParse)?,
                            )?,
                        ),
                    )?;
                    registry.register(Arc::new(provider))?;
                }
                _ => return Err(EnvironmentProcessRuntimeError::ConfigParse),
            }
        }
        let worker = ReconcileWorker::new(
            store.clone(),
            Reconciler::new(registry, PROVIDER_TIMEOUT)?,
            RECONCILE_LEASE,
            RETRY_DELAY,
        )?;
        let outbox = OutboxDispatcher::new(
            pool,
            JetStreamEventPublisher::new(nats.clone()),
            OUTBOX_TIMEOUT,
        )?;
        let access_revoker = NatsAccessRevoker::new(
            required(ACCESS_REVOCATION_SUBJECT)?,
            nats.clone(),
            ACCESS_REVOCATION_TIMEOUT,
        )?;
        let lease_verifier = NatsResourceLeaseVerifier::new(
            required(RESOURCE_LEASE_VERIFICATION_SUBJECT)?,
            nats.clone(),
            RESOURCE_LEASE_VERIFICATION_TIMEOUT,
        )?;
        let worker_id = required(WORKER_ID)?;
        validate_worker_id(&worker_id)?;
        let system_actor_id = ActorId::from_str(&required(SYSTEM_ACTOR_ID)?)
            .map_err(|_| EnvironmentProcessRuntimeError::Configuration(SYSTEM_ACTOR_ID))?;
        Ok(Self {
            store,
            command_consumer,
            release_store,
            release_consumer,
            worker,
            outbox,
            access_revoker,
            lease_verifier,
            nats,
            worker_id,
            system_actor_id,
            readiness: Arc::new(AtomicBool::new(true)),
            expiry_ready: Arc::new(AtomicBool::new(true)),
            freeze_bindings,
        })
    }

    #[must_use]
    pub fn readiness(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.readiness)
    }

    /// Clones only the owner dependencies required by the authenticated public API.
    #[must_use]
    pub fn api_state(&self) -> crate::EnvironmentApiState {
        crate::EnvironmentApiState::new(
            self.store.clone(),
            self.release_store.clone(),
            self.access_revoker.clone(),
            self.lease_verifier.clone(),
            self.freeze_bindings.clone(),
        )
    }

    /// Runs all durable loops until SIGINT/SIGTERM; any unhandled loop failure stops the process.
    pub async fn serve(self) -> Result<(), EnvironmentProcessRuntimeError> {
        let Self {
            store,
            mut command_consumer,
            release_store,
            mut release_consumer,
            worker,
            outbox,
            access_revoker,
            lease_verifier,
            nats,
            worker_id,
            system_actor_id,
            readiness,
            expiry_ready,
            freeze_bindings: _,
        } = self;
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        tokio::try_join!(
            shutdown_task(shutdown_tx),
            command_loop(
                store.clone(),
                &mut command_consumer,
                lease_verifier,
                shutdown_rx.clone()
            ),
            release_loop(release_store, &mut release_consumer, shutdown_rx.clone()),
            reconcile_loop(store.clone(), worker, worker_id, shutdown_rx.clone()),
            outbox_loop(outbox, shutdown_rx.clone()),
            expiry_loop(
                store.clone(),
                access_revoker,
                system_actor_id,
                Arc::clone(&expiry_ready),
                shutdown_rx.clone()
            ),
            readiness_loop(
                store,
                nats.clone(),
                Arc::clone(&expiry_ready),
                Arc::clone(&readiness),
                shutdown_rx
            ),
        )?;
        nats.drain()
            .await
            .map_err(|_| EnvironmentProcessRuntimeError::NatsDrain)?;
        Ok(())
    }
}

async fn release_loop(
    store: PgReleaseProjectionStore,
    consumer: &mut JetStreamReleaseConsumer,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), EnvironmentProcessRuntimeError> {
    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                changed.map_err(|_| EnvironmentProcessRuntimeError::ShutdownChannel)?;
                return Ok(());
            }
            result = consumer.process_next(&store) => {
                let outcome = result?;
                tracing::info!(event = "environment.release.consumed", ?outcome);
            }
        }
    }
}

async fn command_loop(
    store: PgEnvironmentStore,
    consumer: &mut JetStreamCommandConsumer,
    lease_verifier: NatsResourceLeaseVerifier,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), EnvironmentProcessRuntimeError> {
    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                changed.map_err(|_| EnvironmentProcessRuntimeError::ShutdownChannel)?;
                return Ok(());
            }
            result = consumer.process_next(&store, &lease_verifier) => {
                let outcome = result?;
                tracing::info!(event = "environment.command.consumed", ?outcome);
            }
        }
    }
}

async fn reconcile_loop(
    store: PgEnvironmentStore,
    worker: ReconcileWorker,
    worker_id: String,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), EnvironmentProcessRuntimeError> {
    let mut interval = tokio::time::interval(WORKER_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                changed.map_err(|_| EnvironmentProcessRuntimeError::ShutdownChannel)?;
                return Ok(());
            }
            _ = interval.tick() => {
                let now = store.current_time().await?;
                let outcome = worker.run_once(&worker_id, now).await?;
                tracing::debug!(event = "environment.reconcile.completed", ?outcome);
            }
        }
    }
}

async fn outbox_loop(
    outbox: OutboxDispatcher<JetStreamEventPublisher>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), EnvironmentProcessRuntimeError> {
    let mut interval = tokio::time::interval(WORKER_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                changed.map_err(|_| EnvironmentProcessRuntimeError::ShutdownChannel)?;
                return Ok(());
            }
            _ = interval.tick() => {
                let outcome = outbox.dispatch_once().await?;
                tracing::debug!(event = "environment.outbox.completed", ?outcome);
            }
        }
    }
}

async fn expiry_loop(
    store: PgEnvironmentStore,
    access_revoker: NatsAccessRevoker,
    system_actor_id: ActorId,
    expiry_ready: Arc<AtomicBool>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), EnvironmentProcessRuntimeError> {
    let mut interval = tokio::time::interval(EXPIRY_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                changed.map_err(|_| EnvironmentProcessRuntimeError::ShutdownChannel)?;
                return Ok(());
            }
            _ = interval.tick() => {
                let now = store.current_time().await?;
                let mut scan_ready = true;
                for instance in store.find_expired(now, 32).await? {
                    let revocation_revision = match access_revoker.revoke_for_expiry(&instance).await {
                        Ok(revision) => revision,
                        Err(error) => {
                            scan_ready = false;
                            tracing::error!(
                                event = "environment.expiry.revocation_failed",
                                environment_id = %instance.id,
                                diagnostic_code = %error,
                            );
                            continue;
                        }
                    };
                    let deadline = add_time(now, EXPIRY_OPERATION_BUDGET)?;
                    let command = LifecycleCommand {
                        environment_id: instance.id,
                        kind: EnvironmentOperationKind::Expire,
                        expected_revision: instance.revision,
                        actor_id: system_actor_id,
                        trace_id: format!("expiry:{}:{}", instance.id, instance.revision.get()),
                        accepted_at: now,
                        deadline_at: deadline,
                        access_revocation_revision: Some(revocation_revision),
                        preserve_mutable_disk: false,
                        max_attempts: 3,
                        reset_target: None,
                    };
                    let idempotency_key = format!(
                        "expire-{}-r{}",
                        instance.id,
                        instance.revision.get()
                    );
                    match store.accept_command(&idempotency_key, &command).await {
                        Ok(_) => {
                            tracing::info!(
                                event = "environment.expiry.accepted",
                                environment_id = %instance.id,
                                revision = instance.revision.get(),
                            );
                        }
                        Err(EnvironmentStoreError::Lifecycle(crate::LifecycleError::RevisionConflict)
                            | EnvironmentStoreError::RevisionConflict) => {}
                        Err(error) => return Err(error.into()),
                    }
                }
                expiry_ready.store(scan_ready, Ordering::Release);
            }
        }
    }
}

async fn readiness_loop(
    store: PgEnvironmentStore,
    nats: async_nats::Client,
    expiry_ready: Arc<AtomicBool>,
    readiness: Arc<AtomicBool>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), EnvironmentProcessRuntimeError> {
    let mut interval = tokio::time::interval(READINESS_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                changed.map_err(|_| EnvironmentProcessRuntimeError::ShutdownChannel)?;
                readiness.store(false, Ordering::Release);
                return Ok(());
            }
            _ = interval.tick() => {
                let database_ready = store.current_time().await.is_ok();
                let nats_ready = nats.connection_state() == NatsConnectionState::Connected;
                readiness.store(
                    database_ready && nats_ready && expiry_ready.load(Ordering::Acquire),
                    Ordering::Release,
                );
            }
        }
    }
}

async fn shutdown_task(
    shutdown: watch::Sender<bool>,
) -> Result<(), EnvironmentProcessRuntimeError> {
    shutdown_signal().await?;
    shutdown
        .send(true)
        .map_err(|_| EnvironmentProcessRuntimeError::ShutdownChannel)
}

async fn shutdown_signal() -> Result<(), EnvironmentProcessRuntimeError> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .map_err(EnvironmentProcessRuntimeError::Signal)?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result.map_err(EnvironmentProcessRuntimeError::Signal),
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c()
            .await
            .map_err(EnvironmentProcessRuntimeError::Signal)
    }
}

async fn require_schema(pool: &sqlx::PgPool) -> Result<(), EnvironmentProcessRuntimeError> {
    let ready: bool = sqlx::query_scalar(
        "SELECT to_regclass('environment.environment_instances') IS NOT NULL \
         AND to_regclass('environment.environment_operations') IS NOT NULL \
         AND to_regclass('environment.outbox_events') IS NOT NULL \
         AND to_regclass('environment.inbox_events') IS NOT NULL \
         AND to_regclass('environment.release_projections') IS NOT NULL \
         AND to_regclass('environment.kubevirt_runtime_observations') IS NOT NULL",
    )
    .fetch_one(pool)
    .await?;
    if !ready {
        return Err(EnvironmentProcessRuntimeError::SchemaUnavailable);
    }
    Ok(())
}

fn required(name: &'static str) -> Result<String, EnvironmentProcessRuntimeError> {
    let value =
        std::env::var(name).map_err(|_| EnvironmentProcessRuntimeError::Configuration(name))?;
    let value = value.trim();
    if value.is_empty() {
        return Err(EnvironmentProcessRuntimeError::Configuration(name));
    }
    Ok(value.to_owned())
}

fn required_path(name: &'static str) -> Result<PathBuf, EnvironmentProcessRuntimeError> {
    let path = PathBuf::from(required(name)?);
    if !path.is_absolute() {
        return Err(EnvironmentProcessRuntimeError::Configuration(name));
    }
    Ok(path)
}

fn validate_worker_id(value: &str) -> Result<(), EnvironmentProcessRuntimeError> {
    if value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
    {
        return Err(EnvironmentProcessRuntimeError::Configuration(WORKER_ID));
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProviderBindingConfiguration {
    binding: String,
    subject: String,
    provider_kind: Option<String>,
    access_namespace: Option<String>,
    access_pod_label: Option<String>,
    gateway_namespace: Option<String>,
    gateway_name: Option<String>,
    gateway_section: Option<String>,
    image_pull_secret_name: Option<String>,
    image_repository_prefix: Option<String>,
    workspace_storage_class_name: Option<String>,
    active_image_policy_id: Option<String>,
    active_image_policy_revision: Option<u64>,
    active_trust_revision: Option<u64>,
    storage_class_binding: Option<String>,
    storage_class_name: Option<String>,
    data_source_namespace: Option<String>,
    data_source_name: Option<String>,
    gateway_pod_label: Option<String>,
    collector_namespace: Option<String>,
    collector_pod_label: Option<String>,
    guest_user: Option<String>,
    ssh_user_ca_public_key: Option<String>,
    ssh_user_ca_private_key_path: Option<PathBuf>,
    collector_workspace_root: Option<String>,
    vmi_memory_overhead_bytes: Option<u64>,
    cdi_importer_cpu_request_millicores: Option<u32>,
    cdi_importer_cpu_limit_millicores: Option<u32>,
    cdi_importer_memory_request_bytes: Option<u64>,
    cdi_importer_memory_limit_bytes: Option<u64>,
    cdi_scratch_storage_bytes: Option<u64>,
}

impl ProviderBindingConfiguration {
    fn trust_revision(&self) -> Result<Revision, EnvironmentProcessRuntimeError> {
        Revision::new(
            self.active_trust_revision
                .ok_or(EnvironmentProcessRuntimeError::ConfigParse)?,
        )
        .map_err(|_| EnvironmentProcessRuntimeError::ConfigParse)
    }

    fn release_policy(&self) -> Result<ContainerReleasePolicy, EnvironmentProcessRuntimeError> {
        ContainerReleasePolicy::new(
            PolicyId::from_str(
                self.active_image_policy_id
                    .as_deref()
                    .ok_or(EnvironmentProcessRuntimeError::ConfigParse)?,
            )
            .map_err(|_| EnvironmentProcessRuntimeError::ConfigParse)?,
            Revision::new(
                self.active_image_policy_revision
                    .ok_or(EnvironmentProcessRuntimeError::ConfigParse)?,
            )
            .map_err(|_| EnvironmentProcessRuntimeError::ConfigParse)?,
            Revision::new(
                self.active_trust_revision
                    .ok_or(EnvironmentProcessRuntimeError::ConfigParse)?,
            )
            .map_err(|_| EnvironmentProcessRuntimeError::ConfigParse)?,
        )
        .map_err(EnvironmentProcessRuntimeError::from)
    }

    fn has_container_only_fields(&self) -> bool {
        self.access_namespace.is_some()
            || self.access_pod_label.is_some()
            || self.gateway_name.is_some()
            || self.gateway_section.is_some()
            || self.image_pull_secret_name.is_some()
            || self.image_repository_prefix.is_some()
            || self.workspace_storage_class_name.is_some()
    }

    fn has_complete_container_fields(&self) -> bool {
        self.access_namespace.is_some()
            && self.access_pod_label.is_some()
            && self.gateway_name.is_none()
            && self.gateway_section.is_none()
            && self.image_pull_secret_name.is_some()
            && self.image_repository_prefix.is_some()
            && self.workspace_storage_class_name.is_some()
            && self.active_image_policy_id.is_some()
            && self.active_image_policy_revision.is_some()
            && self.active_trust_revision.is_some()
    }

    fn has_kubevirt_fields(&self) -> bool {
        self.storage_class_binding.is_some()
            || self.storage_class_name.is_some()
            || self.data_source_namespace.is_some()
            || self.data_source_name.is_some()
            || self.gateway_pod_label.is_some()
            || self.collector_namespace.is_some()
            || self.collector_pod_label.is_some()
            || self.guest_user.is_some()
            || self.ssh_user_ca_public_key.is_some()
            || self.ssh_user_ca_private_key_path.is_some()
            || self.collector_workspace_root.is_some()
            || self.vmi_memory_overhead_bytes.is_some()
            || self.cdi_importer_cpu_request_millicores.is_some()
            || self.cdi_importer_cpu_limit_millicores.is_some()
            || self.cdi_importer_memory_request_bytes.is_some()
            || self.cdi_importer_memory_limit_bytes.is_some()
            || self.cdi_scratch_storage_bytes.is_some()
    }

    fn has_complete_kubevirt_fields(&self) -> bool {
        self.gateway_namespace.is_some()
            && self.storage_class_binding.is_some()
            && self.storage_class_name.is_some()
            && self.data_source_namespace.is_some()
            && self.data_source_name.is_some()
            && self.gateway_pod_label.is_some()
            && self.collector_namespace.is_some()
            && self.collector_pod_label.is_some()
            && self.guest_user.is_some()
            && self.ssh_user_ca_public_key.is_some()
            && self.ssh_user_ca_private_key_path.is_some()
            && self.collector_workspace_root.is_some()
            && self.vmi_memory_overhead_bytes.is_some()
            && self.cdi_importer_cpu_request_millicores.is_some()
            && self.cdi_importer_cpu_limit_millicores.is_some()
            && self.cdi_importer_memory_request_bytes.is_some()
            && self.cdi_importer_memory_limit_bytes.is_some()
            && self.cdi_scratch_storage_bytes.is_some()
            && self.active_image_policy_id.is_none()
            && self.active_image_policy_revision.is_none()
            && self.active_trust_revision.is_some()
    }

    fn has_provider_specific_fields(&self) -> bool {
        self.access_namespace.is_some()
            || self.access_pod_label.is_some()
            || self.gateway_namespace.is_some()
            || self.has_container_only_fields()
            || self.has_kubevirt_fields()
            || self.active_image_policy_id.is_some()
            || self.active_image_policy_revision.is_some()
            || self.active_trust_revision.is_some()
    }
}

fn load_provider_bindings(
    path: &Path,
) -> Result<Vec<ProviderBindingConfiguration>, EnvironmentProcessRuntimeError> {
    let bytes = std::fs::read(path).map_err(|_| EnvironmentProcessRuntimeError::ConfigRead)?;
    let bindings: Vec<ProviderBindingConfiguration> =
        serde_json::from_slice(&bytes).map_err(|_| EnvironmentProcessRuntimeError::ConfigParse)?;
    if bindings.is_empty() {
        return Err(EnvironmentProcessRuntimeError::ConfigParse);
    }
    Ok(bindings)
}

fn single_provider_configuration<'a>(
    bindings: &'a [ProviderBindingConfiguration],
    provider_kind: &str,
) -> Result<&'a ProviderBindingConfiguration, EnvironmentProcessRuntimeError> {
    let matches = bindings
        .iter()
        .filter(|binding| binding.provider_kind.as_deref() == Some(provider_kind))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [configuration] => Ok(configuration),
        _ => Err(EnvironmentProcessRuntimeError::ConfigParse),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::case_sensitive_file_extension_comparisons)]
mod deployment_contract_tests {
    use super::ProviderBindingConfiguration;

    #[test]
    fn checked_in_platform_provider_example_has_no_legacy_contract() {
        let example = include_str!("../../../deploy/config/environment-providers.json.example");
        let bindings: Vec<ProviderBindingConfiguration> =
            serde_json::from_str(example).expect("provider example must deserialize");

        assert_eq!(bindings.len(), 2);
        assert!(
            bindings
                .iter()
                .all(|binding| binding.subject.ends_with(".v1"))
        );
        assert!(!example.contains(".v2"));
        assert!(!example.contains("activeTrustBundleSha256"));
    }
}

fn add_time(
    timestamp: UtcTimestamp,
    duration: time::Duration,
) -> Result<UtcTimestamp, EnvironmentProcessRuntimeError> {
    let value = timestamp
        .get()
        .checked_add(duration)
        .ok_or(EnvironmentProcessRuntimeError::ClockOverflow)?;
    UtcTimestamp::from_utc(value).map_err(|_| EnvironmentProcessRuntimeError::ClockOverflow)
}

/// Stable production process failures without configuration values, credentials, or payloads.
#[derive(Debug, thiserror::Error)]
pub enum EnvironmentProcessRuntimeError {
    #[error("LW_ENVIRONMENT_RUNTIME_CONFIGURATION_INVALID: {0}")]
    Configuration(&'static str),
    #[error("LW_ENVIRONMENT_RUNTIME_CONFIG_READ_FAILED")]
    ConfigRead,
    #[error("LW_ENVIRONMENT_RUNTIME_CONFIG_PARSE_FAILED")]
    ConfigParse,
    #[error("LW_ENVIRONMENT_RUNTIME_SCHEMA_UNAVAILABLE")]
    SchemaUnavailable,
    #[error("LW_ENVIRONMENT_RUNTIME_CLOCK_OVERFLOW")]
    ClockOverflow,
    #[error("LW_ENVIRONMENT_RUNTIME_SIGNAL_FAILED")]
    Signal(#[source] std::io::Error),
    #[error("LW_ENVIRONMENT_RUNTIME_SHUTDOWN_CHANNEL_FAILED")]
    ShutdownChannel,
    #[error("LW_ENVIRONMENT_RUNTIME_NATS_DRAIN_FAILED")]
    NatsDrain,
    #[error("LW_ENVIRONMENT_RUNTIME_DATABASE_FAILED")]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Store(#[from] EnvironmentStoreError),
    #[error(transparent)]
    Nats(#[from] NatsMessagingError),
    #[error(transparent)]
    Reconcile(#[from] ReconcileError),
    #[error(transparent)]
    Worker(#[from] ReconcileWorkerError),
    #[error(transparent)]
    Outbox(#[from] OutboxDispatchError),
    #[error(transparent)]
    ReleaseProjection(#[from] crate::ReleaseProjectionError),
    #[error(transparent)]
    FreezeBinding(#[from] crate::FreezeBindingError),
}
