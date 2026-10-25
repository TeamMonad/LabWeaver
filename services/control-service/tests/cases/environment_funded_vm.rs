//! Real `KubeVirt` provider with deterministic observations only at its Kubernetes boundary.
use async_trait::async_trait;
use contracts::UtcTimestamp;
use environment_service::{
    KubeVirtBackendFence, KubeVirtCleanupPlan, KubeVirtResourcePlan, KubeVirtRunningObservation,
    KubeVirtStoppedObservation, ProviderFailure, ProviderFailureCode, ProviderOutcome,
};
use sqlx::PgPool;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

type Result<T, E = Box<dyn std::error::Error + Send + Sync>> = std::result::Result<T, E>;

struct Cluster {
    store: environment_service::PgEnvironmentStore,
    calls: Arc<AtomicUsize>,
    vm: uuid::Uuid,
    reject_next_restart: Arc<std::sync::atomic::AtomicBool>,
    disk: uuid::Uuid,
}
impl Cluster {
    async fn now(&self) -> Result<UtcTimestamp, ProviderFailure> {
        self.store
            .current_time()
            .await
            .map_err(|_| ProviderFailure {
                code: ProviderFailureCode::Unavailable,
                retryable: true,
            })
    }
    async fn running(
        &self,
        fence: &KubeVirtBackendFence,
    ) -> Result<ProviderOutcome<KubeVirtRunningObservation>, ProviderFailure> {
        Ok(ProviderOutcome::Completed(KubeVirtRunningObservation {
            observed_environment_generation: fence.environment_generation,
            vm_resource_generation: 1,
            observed_vm_resource_generation: 1,
            vm_uid: self.vm,
            vmi_uid: uuid::Uuid::new_v4(),
            root_disk_uid: self.disk,
            guest_ip: std::net::Ipv4Addr::new(10, 0, 0, 10).into(),
            service_cluster_ip: std::net::Ipv4Addr::new(10, 96, 0, 10).into(),
            ssh_host_key_sha256: Some(persistence_sqlx::Sha256Digest::of_bytes(
                b"synthetic-host-key",
            )),
            guest_agent_connected: false,
            observed_at: self.now().await?,
        }))
    }
}
#[async_trait]
impl environment_service::KubeVirtProviderBackend for Cluster {
    async fn apply(
        &self,
        fence: &KubeVirtBackendFence,
        _: &KubeVirtResourcePlan,
    ) -> Result<ProviderOutcome<KubeVirtRunningObservation>, ProviderFailure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.running(fence).await
    }
    async fn observe(
        &self,
        fence: &KubeVirtBackendFence,
        _: &KubeVirtResourcePlan,
    ) -> Result<ProviderOutcome<KubeVirtRunningObservation>, ProviderFailure> {
        self.running(fence).await
    }
    async fn start(
        &self,
        fence: &KubeVirtBackendFence,
        _: &KubeVirtResourcePlan,
    ) -> Result<ProviderOutcome<KubeVirtRunningObservation>, ProviderFailure> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.running(fence).await
    }
    async fn restart(
        &self,
        fence: &KubeVirtBackendFence,
        _: &KubeVirtResourcePlan,
    ) -> Result<ProviderOutcome<KubeVirtRunningObservation>, ProviderFailure> {
        if self.reject_next_restart.swap(false, Ordering::SeqCst) {
            return Err(ProviderFailure {
                code: ProviderFailureCode::Rejected,
                retryable: false,
            });
        }
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.running(fence).await
    }
    async fn stop(
        &self,
        fence: &KubeVirtBackendFence,
        _: &KubeVirtCleanupPlan,
    ) -> Result<ProviderOutcome<KubeVirtStoppedObservation>, ProviderFailure> {
        Ok(ProviderOutcome::Completed(KubeVirtStoppedObservation {
            observed_environment_generation: fence.environment_generation,
            vm_uid: self.vm,
            root_disk_uid: self.disk,
            vmi_absent: true,
            observed_at: self.now().await?,
        }))
    }
    async fn delete_namespace(
        &self,
        _: &KubeVirtBackendFence,
        _: &KubeVirtCleanupPlan,
        _: Option<&contracts::environment::EnvironmentCleanupTarget>,
    ) -> Result<ProviderOutcome<contracts::ArtifactRef>, ProviderFailure> {
        Ok(ProviderOutcome::Completed(super::cleanup_evidence()))
    }
}

pub(super) fn register(
    registry: &mut environment_service::ProviderRegistry,
    pool: &PgPool,
    projection: &contracts::events::ReleasePublished,
    calls: Arc<AtomicUsize>,
    reject_next_restart: Arc<std::sync::atomic::AtomicBool>,
) -> Result<()> {
    register_backend(
        registry,
        pool,
        projection,
        Arc::new(Cluster {
            store: environment_service::PgEnvironmentStore::new(pool.clone()),
            calls,
            vm: uuid::Uuid::new_v4(),
            disk: uuid::Uuid::new_v4(),
            reject_next_restart,
        }),
    )
}

pub(super) fn register_backend<B: environment_service::KubeVirtProviderBackend + 'static>(
    registry: &mut environment_service::ProviderRegistry,
    pool: &PgPool,
    projection: &contracts::events::ReleasePublished,
    backend: Arc<B>,
) -> Result<()> {
    let contracts::authoring::EnvironmentRuntimeSpec::VirtualMachine {
        provider_binding,
        base_disk,
        storage_class_binding,
        ..
    } = &projection.environment_spec.runtime
    else {
        return Err("VM release required".into());
    };
    let contracts::supply_chain::ImageArtifact::VirtualMachine { format, .. } =
        &projection.release.artifact
    else {
        return Err("VM artifact required".into());
    };
    let base = environment_service::KubeVirtBaseDiskBinding::new(
        base_disk.binding.clone(),
        base_disk.source_registry_digest.clone(),
        "1".repeat(64),
        base_disk.capacity_bytes,
        *format,
        storage_class_binding.clone(),
        "synthetic-storage".into(),
        "synthetic-bases".into(),
        "base-disk".into(),
        "labweaver".into(),
        22,
    )?;
    let ssh = environment_service::KubeVirtSshBootstrap::new(
        "synthetic-access".into(),
        "gateway".into(),
        "synthetic-collector".into(),
        "collector".into(),
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDIhz2GK/XCUj4i6Q5yQJNL1MKDXETe1aM1lHYMGt2SQ",
    )?;
    let budget = environment_service::KubeVirtResourceBudget::new(
        536_870_912,
        1000,
        4000,
        262_144_000,
        1_073_741_824,
        base_disk.capacity_bytes,
    )?;
    let configuration = environment_service::KubeVirtProviderConfiguration::new(
        projection
            .release
            .approval
            .as_ref()
            .ok_or("generated VM approval")?
            .trust_revision,
        vec![base],
        None,
        ssh,
        budget,
    )?;
    registry.register(Arc::new(environment_service::KubeVirtProvider::new(
        provider_binding.clone(),
        backend,
        Arc::new(environment_service::PgReleaseProjectionStore::new(
            pool.clone(),
        )),
        Arc::new(environment_service::PgKubeVirtObservationStore::new(
            pool.clone(),
        )),
        configuration,
    )?))?;
    Ok(())
}
