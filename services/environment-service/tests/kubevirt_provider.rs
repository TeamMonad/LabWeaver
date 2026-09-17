//! `KubeVirt` release, fencing, resource-plan, readiness and cleanup contract tests.
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures use explicit assertion messages for invalid setup"
)]

mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use contracts::authoring::{
    CandidateApproval, CandidateDecision, EnvironmentClass, EnvironmentEntrySpec,
    EnvironmentRuntimeSpec, EnvironmentSpec, RuntimeKind,
};
use contracts::environment::{
    DesiredEnvironmentState, EndpointProtocol, EnvironmentLeaseAuthorization,
    EnvironmentOperationKind, ObservedEnvironmentState,
};
use contracts::events::ReleasePublished;
use contracts::resource::{GpuAllocation, GpuAllocationMode, GpuRequest, WorkloadResources};
use contracts::supply_chain::{
    EnvironmentTemplateRelease, ImageArtifact, VirtualMachineBaseDisk, VirtualMachineDiskFormat,
};
use contracts::{
    ActorId, ApprovalId, ArtifactId, ArtifactRef, CandidateId, GpuCatalogEntryId, ImageArtifactId,
    LeaseId, PolicyId, ReleaseId, ResourceRequestId, Revision, UtcTimestamp,
};
use environment_service::{
    CdiImportClient, CdiImportError, ContainerReleaseResolver, EnvironmentProvider,
    KUBEVIRT_BACKEND_PROTOCOL_VERSION, KubeVirtBackendFence, KubeVirtBaseDiskBinding,
    KubeVirtBaseDiskIdentity, KubeVirtBaseDiskImport, KubeVirtCleanupPlan,
    KubeVirtObservationStore, KubeVirtObservationStoreError, KubeVirtProvider,
    KubeVirtProviderBackend, KubeVirtProviderConfiguration, KubeVirtResourceBudget,
    KubeVirtResourcePlan, KubeVirtRunningObservation, KubeVirtSshBootstrap,
    KubeVirtStoppedObservation, ProviderFailure, ReconcileAction, ReleaseProjectionError,
    ResolvedContainerRelease, RuntimeVmBasePolicy, ensure_base_disk,
};
use persistence_sqlx::Sha256Digest;
use serde_json::json;
use uuid::Uuid;

const VM_UID: Uuid = Uuid::from_u128(1);
const VMI_UID: Uuid = Uuid::from_u128(2);
const ROOT_DISK_UID: Uuid = Uuid::from_u128(3);

#[derive(Clone)]
struct FixtureResolver {
    projection: ReleasePublished,
    authority_now: UtcTimestamp,
    withdrawn_at: Option<UtcTimestamp>,
}

#[async_trait]
impl ContainerReleaseResolver for FixtureResolver {
    async fn resolve(
        &self,
        release_id: ReleaseId,
        release_version: u64,
    ) -> Result<ResolvedContainerRelease, ReleaseProjectionError> {
        if self.projection.release.id != release_id
            || self.projection.release.version != release_version
        {
            return Err(ReleaseProjectionError::NotFound);
        }
        Ok(ResolvedContainerRelease {
            projection: self.projection.clone(),
            authority_now: self.authority_now,
            withdrawn_at: self.withdrawn_at,
        })
    }
}

#[derive(Default)]
struct FixtureBackend {
    operations: Mutex<Vec<String>>,
    fences: Mutex<Vec<KubeVirtBackendFence>>,
    objects: Mutex<BTreeSet<(String, String, String)>>,
    incomplete_readiness: bool,
    guest_agent_disconnected: bool,
    public_route: bool,
}

#[derive(Default)]
struct FixtureObservationStore {
    states: Mutex<Vec<String>>,
}

#[async_trait]
impl KubeVirtObservationStore for FixtureObservationStore {
    async fn record_running(
        &self,
        _fence: &KubeVirtBackendFence,
        _plan: &KubeVirtResourcePlan,
        _observation: &KubeVirtRunningObservation,
    ) -> Result<(), KubeVirtObservationStoreError> {
        self.states
            .lock()
            .expect("states lock")
            .push("running".to_owned());
        Ok(())
    }

    async fn record_stopped(
        &self,
        _fence: &KubeVirtBackendFence,
        _plan: &KubeVirtResourcePlan,
        _observation: &KubeVirtStoppedObservation,
    ) -> Result<(), KubeVirtObservationStoreError> {
        self.states
            .lock()
            .expect("states lock")
            .push("stopped".to_owned());
        Ok(())
    }

    async fn record_deleted(
        &self,
        _fence: &KubeVirtBackendFence,
        _plan: &KubeVirtCleanupPlan,
        _cleanup_evidence: &ArtifactRef,
    ) -> Result<(), KubeVirtObservationStoreError> {
        self.states
            .lock()
            .expect("states lock")
            .push("deleted".to_owned());
        Ok(())
    }
}

impl FixtureBackend {
    fn record(&self, operation: &str, fence: &KubeVirtBackendFence) {
        self.operations
            .lock()
            .expect("operations lock")
            .push(operation.to_owned());
        self.fences.lock().expect("fences lock").push(fence.clone());
    }

    fn running(&self, fence: &KubeVirtBackendFence) -> KubeVirtRunningObservation {
        KubeVirtRunningObservation {
            observed_environment_generation: fence.environment_generation,
            vm_resource_generation: 4,
            observed_vm_resource_generation: 4,
            vm_uid: VM_UID,
            vmi_uid: VMI_UID,
            root_disk_uid: ROOT_DISK_UID,
            guest_ip: if self.public_route {
                "198.51.100.17".parse().expect("public guest IP")
            } else {
                "10.42.0.17".parse().expect("guest IP")
            },
            service_cluster_ip: if self.public_route {
                "203.0.113.17".parse().expect("public service IP")
            } else {
                "10.96.0.17".parse().expect("service IP")
            },
            ssh_host_key_sha256: Sha256Digest::of_bytes(b"vm-host-key"),
            guest_agent_connected: !self.guest_agent_disconnected,
            ssh_ready: !self.incomplete_readiness,
            observed_at: timestamp("2026-07-16T08:01:00.000Z"),
        }
    }

    fn apply_objects(&self, plan: &KubeVirtResourcePlan) {
        let mut objects = self.objects.lock().expect("objects lock");
        for resource in &plan.resources {
            objects.insert((
                resource.kind.clone(),
                resource.namespace.clone().unwrap_or_default(),
                resource.name.clone(),
            ));
        }
    }

    fn count_kind(&self, kind: &str) -> usize {
        self.objects
            .lock()
            .expect("objects lock")
            .iter()
            .filter(|(object_kind, _, _)| object_kind == kind)
            .count()
    }
}

#[async_trait]
impl KubeVirtProviderBackend for FixtureBackend {
    async fn apply(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtResourcePlan,
    ) -> Result<KubeVirtRunningObservation, ProviderFailure> {
        self.record("apply", fence);
        self.apply_objects(plan);
        Ok(self.running(fence))
    }

    async fn observe(
        &self,
        fence: &KubeVirtBackendFence,
        _plan: &KubeVirtResourcePlan,
    ) -> Result<KubeVirtRunningObservation, ProviderFailure> {
        self.record("observe", fence);
        Ok(self.running(fence))
    }

    async fn start(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtResourcePlan,
    ) -> Result<KubeVirtRunningObservation, ProviderFailure> {
        self.record("start", fence);
        self.apply_objects(plan);
        Ok(self.running(fence))
    }

    async fn stop(
        &self,
        fence: &KubeVirtBackendFence,
        _plan: &KubeVirtResourcePlan,
    ) -> Result<KubeVirtStoppedObservation, ProviderFailure> {
        self.record("stop", fence);
        Ok(KubeVirtStoppedObservation {
            observed_environment_generation: fence.environment_generation,
            vm_uid: VM_UID,
            root_disk_uid: ROOT_DISK_UID,
            vmi_absent: true,
            observed_at: timestamp("2026-07-16T08:02:00.000Z"),
        })
    }

    async fn restart(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtResourcePlan,
    ) -> Result<KubeVirtRunningObservation, ProviderFailure> {
        self.record("restart", fence);
        self.apply_objects(plan);
        Ok(self.running(fence))
    }

    async fn delete_namespace(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtCleanupPlan,
    ) -> Result<ArtifactRef, ProviderFailure> {
        self.record("delete", fence);
        self.objects
            .lock()
            .expect("objects lock")
            .retain(|(kind, namespace, name)| {
                namespace != &plan.namespace && !(kind == "Namespace" && name == &plan.namespace)
            });
        Ok(ArtifactRef {
            artifact_id: ArtifactId::new(),
            store_binding: "environment-cleanup-evidence-v1".to_owned(),
            object_version: plan.plan_sha256.to_string(),
            size_bytes: 1,
            media_type: "application/json".to_owned(),
        })
    }
}

#[test]
fn plan_rejects_unlisted_bindings_and_reviewed_base_drift() {
    for mutate in [
        |base_disk: &mut VirtualMachineBaseDisk| base_disk.binding = "cirros-0.6-v1".to_owned(),
        |base_disk: &mut VirtualMachineBaseDisk| {
            base_disk.source_registry_digest = format!(
                "docker://quay.io/containerdisks/ubuntu@sha256:{}",
                "a".repeat(64)
            );
        },
        |base_disk: &mut VirtualMachineBaseDisk| base_disk.capacity_bytes = 20_000_000_000,
    ] {
        let mut projection = projection();
        let ImageArtifact::VirtualMachine { base_disk, .. } = &mut projection.release.artifact
        else {
            panic!("VM fixture artifact");
        };
        mutate(base_disk);
        let instance = instance_for(&projection);
        let provider = provider(projection.clone(), Arc::new(FixtureBackend::default()));
        let resolved = resolved(projection);
        assert!(matches!(
            provider.plan(&instance, &resolved, ReconcileAction::Provision),
            Err(ReleaseProjectionError::SecurityPostureInvalid)
        ));
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the resource-plan test audits the complete security bundle in one place"
)]
fn plan_is_deterministic_private_and_digest_bound() {
    let projection = projection();
    let instance = instance_for(&projection);
    let provider = provider(projection.clone(), Arc::new(FixtureBackend::default()));
    let resolved = resolved(projection);
    let first = provider
        .plan(&instance, &resolved, ReconcileAction::Provision)
        .expect("valid VM plan");
    let second = provider
        .plan(&instance, &resolved, ReconcileAction::Provision)
        .expect("same input plans deterministically");

    assert_eq!(first.plan_sha256, second.plan_sha256);
    let EnvironmentRuntimeSpec::VirtualMachine { base_disk, .. } =
        &resolved.projection.environment_spec.runtime
    else {
        panic!("VM fixture runtime");
    };
    assert_eq!(&first.base_disk, base_disk);
    assert_eq!(first.base_disk_format, VirtualMachineDiskFormat::Qcow2);
    assert_eq!(first.storage_class_name, "local-path");
    assert_eq!(count_resource(&first, "DataVolume"), 1);
    assert_eq!(count_resource(&first, "VirtualMachine"), 1);
    assert_eq!(count_resource(&first, "Service"), 1);
    assert_eq!(
        resource(&first, "Namespace")
            .document
            .pointer("/metadata/labels/labweaver.io~1environment"),
        Some(&json!("true"))
    );

    let data_volume = resource(&first, "DataVolume");
    assert_eq!(
        data_volume.document.pointer("/spec/sourceRef/kind"),
        Some(&json!("DataSource"))
    );
    assert_eq!(
        data_volume
            .document
            .pointer("/spec/storage/storageClassName"),
        Some(&json!("local-path"))
    );
    assert_eq!(
        data_volume
            .document
            .pointer("/metadata/annotations/labweaver.io~1base-disk-sha256"),
        Some(&json!(
            "ffe6203da54deeb6db5d2a98a83f9ec8e55f149d3f7ba622e1abe5fa966ee3d6"
        ))
    );
    assert_eq!(
        data_volume
            .document
            .pointer("/metadata/annotations/labweaver.io~1base-disk-source-registry"),
        Some(&json!(first.base_disk.source_registry_digest))
    );

    let virtual_machine = resource(&first, "VirtualMachine");
    assert_eq!(
        virtual_machine.document.pointer("/spec/runStrategy"),
        Some(&json!("Always"))
    );
    assert_eq!(
        virtual_machine
            .document
            .pointer("/spec/template/spec/nodeSelector/labweaver.io~1kubevirt"),
        Some(&json!("true"))
    );
    assert_eq!(
        virtual_machine
            .document
            .pointer("/spec/template/spec/domain/devices/autoattachGraphicsDevice"),
        Some(&json!(true))
    );
    for label in [
        "labweaver.io~1environment-id",
        "labweaver.io~1course-id",
        "labweaver.io~1release-id",
        "labweaver.io~1release-version",
    ] {
        assert!(
            virtual_machine
                .document
                .pointer(&format!("/spec/template/metadata/labels/{label}"))
                .is_some(),
            "missing authoritative VMI identity label {label}"
        );
    }
    assert_eq!(
        virtual_machine
            .document
            .pointer("/spec/template/spec/volumes/0/persistentVolumeClaim/claimName"),
        Some(&json!("rootdisk"))
    );
    assert_eq!(
        virtual_machine
            .document
            .pointer("/spec/template/spec/domain/resources/requests/memory"),
        Some(&json!("2147483648"))
    );
    assert_eq!(
        virtual_machine
            .document
            .pointer("/spec/template/spec/domain/resources/limits/cpu"),
        Some(&json!("2000m"))
    );
    assert_eq!(
        virtual_machine
            .document
            .pointer("/spec/template/spec/domain/resources/limits/memory"),
        Some(&json!("2684354560"))
    );

    let quota = resource(&first, "ResourceQuota");
    assert_eq!(
        quota.document.pointer("/spec/hard/requests.cpu"),
        Some(&json!("3000m"))
    );
    assert_eq!(
        quota.document.pointer("/spec/hard/limits.cpu"),
        Some(&json!("6000m"))
    );
    assert_eq!(
        quota.document.pointer("/spec/hard/requests.memory"),
        Some(&json!("2946498560"))
    );
    assert_eq!(
        quota.document.pointer("/spec/hard/limits.memory"),
        Some(&json!("3758096384"))
    );
    assert_eq!(
        quota.document.pointer("/spec/hard/requests.storage"),
        Some(&json!("21474836480"))
    );
    assert_eq!(
        quota.document.pointer("/spec/hard/persistentvolumeclaims"),
        Some(&json!("2"))
    );
    assert_eq!(quota.document.pointer("/spec/hard/pods"), Some(&json!("2")));
    assert_eq!(
        quota
            .document
            .pointer("/metadata/annotations/labweaver.io~1vmi-memory-overhead-bytes"),
        Some(&json!("536870912"))
    );
    assert_eq!(
        quota
            .document
            .pointer("/metadata/annotations/labweaver.io~1cdi-scratch-storage-bytes"),
        Some(&json!("10737418240"))
    );

    let cloud_init_secret = resource(&first, "Secret");
    assert!(cloud_init_secret.document.pointer("/stringData").is_none());
    let cloud_init_encoded = cloud_init_secret
        .document
        .pointer("/data/userdata")
        .and_then(serde_json::Value::as_str)
        .expect("cloud-init data.userdata");
    let cloud_init_bytes = BASE64_STANDARD
        .decode(cloud_init_encoded)
        .expect("base64 cloud-init userdata");
    let cloud_init = std::str::from_utf8(&cloud_init_bytes).expect("UTF-8 cloud-init userdata");
    assert!(cloud_init.contains("TrustedUserCAKeys /etc/ssh/labweaver_user_ca.pub"));
    assert!(cloud_init.contains("labweaver-gateway\n      labweaver-collector"));
    assert!(
        cloud_init.contains("[install, -d, -o, lab, -g, lab, -m, '0700', /home/lab/workspace]")
    );
    assert!(cloud_init.contains("AuthorizedKeysFile none"));
    assert!(cloud_init.contains("AuthenticationMethods publickey"));
    assert!(cloud_init.contains("AllowUsers lab"));
    assert!(cloud_init.contains("PasswordAuthentication no"));
    assert!(cloud_init.contains("AllowAgentForwarding no"));
    assert!(!cloud_init.contains("PermitAgentForwarding"));
    assert!(cloud_init.contains("- [sshd, -t]"));
    assert!(cloud_init.contains("- [systemctl, enable, --now, ssh.service]"));
    assert!(!cloud_init.contains("ssh_authorized_keys"));
    assert!(!cloud_init.contains("PRIVATE KEY"));
    let network_data_encoded = cloud_init_secret
        .document
        .pointer("/data/networkdata")
        .and_then(serde_json::Value::as_str)
        .expect("cloud-init data.networkdata");
    let network_data = BASE64_STANDARD
        .decode(network_data_encoded)
        .expect("base64 cloud-init networkdata");
    assert_eq!(
        std::str::from_utf8(&network_data).expect("UTF-8 cloud-init networkdata"),
        "version: 2\nethernets:\n  default:\n    match:\n      name: \"en*\"\n    dhcp4: true\n"
    );
    assert_eq!(
        resource(&first, "VirtualMachine")
            .document
            .pointer("/spec/template/spec/volumes/1/cloudInitNoCloud/networkDataSecretRef/name"),
        Some(&json!("cloud-init"))
    );

    assert_eq!(
        resource(&first, "Service").document.pointer("/spec/type"),
        Some(&json!("ClusterIP"))
    );
    assert_eq!(
        resource(&first, "VirtualMachine")
            .document
            .pointer("/spec/template/spec/readinessProbe"),
        None,
        "executor-owned SSH verification is the single readiness authority"
    );
    assert!(first.resources.iter().all(|resource| {
        resource.kind != "Ingress"
            && resource.document.pointer("/spec/type") != Some(&json!("NodePort"))
            && resource.document.pointer("/spec/type") != Some(&json!("LoadBalancer"))
    }));
    let ingress = named_resource(&first, "NetworkPolicy", "openssh-gateway-ingress");
    assert_eq!(
        ingress
            .document
            .pointer("/spec/ingress/0/from/0/podSelector/matchLabels/app.kubernetes.io~1name"),
        Some(&json!("openssh-gateway"))
    );
    assert_eq!(
        ingress.document.pointer(
            "/spec/ingress/0/from/0/namespaceSelector/matchLabels/kubernetes.io~1metadata.name"
        ),
        Some(&json!("access-system"))
    );
    assert_eq!(
        ingress
            .document
            .pointer("/spec/ingress/0/from/1/podSelector/matchLabels/app.kubernetes.io~1name"),
        Some(&json!("evaluation-freeze-worker"))
    );
    assert_eq!(
        ingress.document.pointer(
            "/spec/ingress/0/from/1/namespaceSelector/matchLabels/kubernetes.io~1metadata.name"
        ),
        Some(&json!("labweaver-evaluation"))
    );
    assert_eq!(
        ingress
            .document
            .pointer("/spec/ingress/0/from/2/podSelector/matchLabels/app.kubernetes.io~1name"),
        Some(&json!("kubevirt-executor"))
    );
    assert_eq!(
        ingress.document.pointer(
            "/spec/ingress/0/from/2/namespaceSelector/matchLabels/kubernetes.io~1metadata.name"
        ),
        Some(&json!("labweaver-system"))
    );
    let cdi_ingress = named_resource(&first, "NetworkPolicy", "cdi-clone-ingress");
    assert_eq!(
        cdi_ingress
            .document
            .pointer("/spec/podSelector/matchLabels/cdi.kubevirt.io"),
        Some(&json!("cdi-upload-server"))
    );
    assert_eq!(
        cdi_ingress.document.pointer(
            "/spec/ingress/0/from/0/namespaceSelector/matchLabels/kubernetes.io~1metadata.name"
        ),
        Some(&json!("labweaver-system"))
    );
    assert_eq!(
        cdi_ingress.document.pointer("/spec/ingress/0/ports/0/port"),
        Some(&json!(8443))
    );
}

#[test]
fn plan_renders_each_approved_vm_vgpu_and_resource_quantity() {
    let projection = projection();
    let mut instance = instance_for(&projection);
    let lease_id = LeaseId::new();
    let capacity_binding = "vm-vgpu-capacity-1".to_owned();
    let gpu_class = "nvidia-vgpu".to_owned();
    let allocation_binding = "nvidia.com/grid-t4-4c".to_owned();
    let gpu_allocation = GpuAllocation {
        entry_id: GpuCatalogEntryId::new(),
        class: gpu_class.clone(),
        count: 2,
        mode: GpuAllocationMode::VmVgpu,
        provider_binding: "kubevirt-primary-v1".to_owned(),
        allocation_binding: allocation_binding.clone(),
        catalog_revision: revision(1),
    };
    instance.class = EnvironmentClass::Work;
    instance.lease_id = Some(lease_id);
    instance.capacity_binding = Some(capacity_binding.clone());
    instance.operation.lease_authorization = Some(EnvironmentLeaseAuthorization {
        resource_request_id: ResourceRequestId::new(),
        lease_id,
        lease_revision: revision(1),
        environment_id: instance.id,
        project_id: instance.project_id,
        course_id: instance.course_id,
        owner_actor_id: instance.owner_id,
        capacity_binding,
        approved_resources: WorkloadResources {
            cpu_millicores: 2_000,
            memory_bytes: 2_147_483_648,
            storage_bytes: 10_737_418_240,
            gpu: Some(GpuRequest {
                class: gpu_class,
                count: 2,
            }),
        },
        gpu_allocation: Some(gpu_allocation),
        active_from: timestamp("2026-07-16T08:00:00.000Z"),
        expires_at: timestamp("2026-07-16T09:00:00.000Z"),
    });
    let provider = provider(projection.clone(), Arc::new(FixtureBackend::default()));
    let plan = provider
        .plan(&instance, &resolved(projection), ReconcileAction::Provision)
        .expect("valid VM vGPU plan");

    let virtual_machine = resource(&plan, "VirtualMachine");
    let gpus = virtual_machine
        .document
        .pointer("/spec/template/spec/domain/devices/gpus")
        .and_then(serde_json::Value::as_array)
        .expect("VM GPU devices");
    assert_eq!(gpus.len(), 2);
    for (index, gpu) in gpus.iter().enumerate() {
        assert_eq!(
            gpu.pointer("/name"),
            Some(&json!(format!("runtime-gpu-{index}")))
        );
        assert_eq!(
            gpu.pointer("/deviceName"),
            Some(&json!("nvidia.com/grid-t4-4c"))
        );
    }
    assert_eq!(
        virtual_machine
            .document
            .pointer("/spec/template/spec/domain/resources/limits/nvidia.com~1grid-t4-4c"),
        Some(&json!("2"))
    );
    let quota = resource(&plan, "ResourceQuota");
    assert_eq!(
        quota
            .document
            .pointer("/spec/hard/limits.nvidia.com~1grid-t4-4c"),
        Some(&json!("2"))
    );
}

#[test]
fn experiment_vm_gpu_allocation_renders_vgpu_devices_and_limits() {
    let mut projection = projection();
    projection.environment_spec.resources.gpu = Some(GpuRequest {
        class: "t4-vgpu".to_owned(),
        count: 1,
    });
    projection.validate().expect("GPU VM projection");
    let mut instance = instance_for(&projection);
    instance.gpu_allocation = Some(GpuAllocation {
        entry_id: GpuCatalogEntryId::new(),
        class: "t4-vgpu".to_owned(),
        count: 1,
        mode: GpuAllocationMode::VmVgpu,
        provider_binding: "kubevirt-primary-v1".to_owned(),
        allocation_binding: "nvidia.com/grid-t4-4c".to_owned(),
        catalog_revision: revision(1),
    });
    let provider = provider(projection.clone(), Arc::new(FixtureBackend::default()));

    let plan = provider
        .plan(&instance, &resolved(projection), ReconcileAction::Provision)
        .expect("resolved Experiment VM vGPU allocation is rendered");
    let virtual_machine = resource(&plan, "VirtualMachine");
    let gpus = virtual_machine
        .document
        .pointer("/spec/template/spec/domain/devices/gpus")
        .and_then(serde_json::Value::as_array)
        .expect("VM GPU devices");
    assert_eq!(gpus.len(), 1);
    assert_eq!(
        gpus[0].pointer("/deviceName"),
        Some(&json!("nvidia.com/grid-t4-4c"))
    );
    assert_eq!(
        virtual_machine
            .document
            .pointer("/spec/template/spec/domain/resources/limits/nvidia.com~1grid-t4-4c"),
        Some(&json!("1"))
    );
}

#[test]
fn experiment_vm_gpu_without_a_durable_allocation_fails_closed() {
    let mut projection = projection();
    projection.environment_spec.resources.gpu = Some(GpuRequest {
        class: "t4-vgpu".to_owned(),
        count: 1,
    });
    projection.validate().expect("GPU VM projection");
    let instance = instance_for(&projection);
    assert!(instance.gpu_allocation.is_none());
    let provider = provider(projection.clone(), Arc::new(FixtureBackend::default()));

    assert!(matches!(
        provider.plan(&instance, &resolved(projection), ReconcileAction::Provision),
        Err(ReleaseProjectionError::SecurityPostureInvalid)
    ));
}

#[tokio::test]
async fn readiness_requires_vm_ssh_and_current_generation() {
    let release_projection = projection();
    let mut instance = instance_for(&release_projection);
    instance.observed_state = ObservedEnvironmentState::Provisioning;
    let incomplete = Arc::new(FixtureBackend {
        incomplete_readiness: true,
        ..FixtureBackend::default()
    });
    let incomplete_provider = provider(release_projection.clone(), incomplete);

    let observation = incomplete_provider
        .execute(ReconcileAction::Provision, &instance)
        .await
        .expect("incomplete readiness is retryable progress");
    assert_eq!(
        observation.next_state,
        ObservedEnvironmentState::Provisioning
    );
    assert!(!observation.operation_complete);
    assert!(observation.endpoints.is_empty());

    let public_route = Arc::new(FixtureBackend {
        public_route: true,
        ..FixtureBackend::default()
    });
    let public_route_provider = provider(release_projection, public_route);
    let observation = public_route_provider
        .execute(ReconcileAction::Provision, &instance)
        .await
        .expect("public addresses remain incomplete readiness");
    assert_eq!(
        observation.next_state,
        ObservedEnvironmentState::Provisioning
    );
    assert!(observation.endpoints.is_empty());
}

#[tokio::test]
async fn readiness_accepts_ssh_proof_without_guest_agent() {
    let release_projection = projection();
    let mut instance = instance_for(&release_projection);
    instance.observed_state = ObservedEnvironmentState::Provisioning;
    let backend = Arc::new(FixtureBackend {
        guest_agent_disconnected: true,
        ..FixtureBackend::default()
    });
    let provider = provider(release_projection, backend);

    let observation = provider
        .execute(ReconcileAction::Provision, &instance)
        .await
        .expect("SSH readiness is authoritative without a guest agent");
    assert_eq!(observation.next_state, ObservedEnvironmentState::Ready);
    assert!(observation.operation_complete);
    assert_eq!(observation.endpoints.len(), 1);
}

#[tokio::test]
async fn duplicate_reconcile_is_idempotent_and_fenced() {
    let projection = projection();
    let mut instance = instance_for(&projection);
    instance.observed_state = ObservedEnvironmentState::Provisioning;
    let backend = Arc::new(FixtureBackend::default());
    let provider = provider(projection, backend.clone());

    let first = provider
        .execute(ReconcileAction::Provision, &instance)
        .await
        .expect("provision succeeds");
    let replay = provider
        .execute(ReconcileAction::Provision, &instance)
        .await
        .expect("same reconcile is idempotent");

    assert_eq!(first.endpoints, replay.endpoints);
    assert_eq!(first.endpoints.len(), 1);
    assert_eq!(first.endpoints[0].protocol, EndpointProtocol::Ssh);
    let fences = backend.fences.lock().expect("fences lock");
    assert_eq!(fences.len(), 2);
    assert_eq!(fences[0].request_id, fences[1].request_id);
    assert!(fences.iter().all(|fence| {
        fence.protocol_version == KUBEVIRT_BACKEND_PROTOCOL_VERSION
            && fence.environment_id == instance.id
            && fence.operation_id == instance.operation.id
            && fence.trace_id == instance.operation.trace_id
            && fence.provider_step == instance.operation.provider_step
            && fence.environment_generation == instance.generation
            && fence.attempt == instance.operation.attempt
            && fence.deadline_at == instance.operation.deadline_at
    }));
    drop(fences);
    assert_eq!(backend.count_kind("VirtualMachine"), 1);
    assert_eq!(backend.count_kind("DataVolume"), 1);
    assert_eq!(backend.count_kind("Service"), 1);
}

#[tokio::test]
async fn start_stop_start_preserves_vm_disk_host_key_and_endpoint_identity() {
    let projection = projection();
    let backend = Arc::new(FixtureBackend::default());
    let provider = provider(projection.clone(), backend.clone());

    let mut provision = instance_for(&projection);
    provision.observed_state = ObservedEnvironmentState::Provisioning;
    let first = provider
        .execute(ReconcileAction::Provision, &provision)
        .await
        .expect("initial provision");

    let mut stop = provision.clone();
    stop.observed_state = ObservedEnvironmentState::Stopping;
    stop.desired_state = DesiredEnvironmentState::Stopped;
    stop.generation = 2;
    stop.operation.id = contracts::OperationId::new();
    let stopped = provider
        .execute(ReconcileAction::Stop, &stop)
        .await
        .expect("stop preserves disk");
    assert_eq!(stopped.next_state, ObservedEnvironmentState::Stopped);
    assert!(stopped.endpoints.is_empty());

    let mut start = provision;
    start.observed_state = ObservedEnvironmentState::Stopped;
    start.generation = 3;
    start.operation.id = contracts::OperationId::new();
    let second = provider
        .execute(ReconcileAction::Start, &start)
        .await
        .expect("start reuses VM disk");

    assert_eq!(second.next_state, ObservedEnvironmentState::Ready);
    assert_eq!(first.endpoints[0].id, second.endpoints[0].id);
    assert_eq!(VM_UID, Uuid::from_u128(1));
    assert_eq!(ROOT_DISK_UID, Uuid::from_u128(3));
    assert_eq!(
        backend
            .operations
            .lock()
            .expect("operations lock")
            .as_slice(),
        ["apply", "stop", "start"]
    );
    let fences = backend.fences.lock().expect("fences lock");
    assert_ne!(fences[0].request_id, fences[1].request_id);
    assert_ne!(fences[1].request_id, fences[2].request_id);
    drop(fences);
    assert_eq!(backend.count_kind("VirtualMachine"), 1);
    assert_eq!(backend.count_kind("DataVolume"), 1);
}

#[tokio::test]
async fn cleanup_deletes_the_owned_namespace_and_requires_evidence() {
    let projection = projection();
    let backend = Arc::new(FixtureBackend::default());
    let provider = provider(projection.clone(), backend.clone());

    let mut provision = instance_for(&projection);
    provision.observed_state = ObservedEnvironmentState::Provisioning;
    provider
        .execute(ReconcileAction::Provision, &provision)
        .await
        .expect("fixture materializes owned resources");
    assert_eq!(backend.count_kind("VirtualMachine"), 1);
    assert_eq!(backend.count_kind("DataVolume"), 1);

    let mut instance = provision;
    instance.observed_state = ObservedEnvironmentState::Stopped;
    instance.desired_state = DesiredEnvironmentState::Deleted;
    instance.generation = 2;
    instance.operation.id = contracts::OperationId::new();
    instance.operation.kind = EnvironmentOperationKind::Expire;

    let checkpoint = provider
        .execute(ReconcileAction::Cleanup, &instance)
        .await
        .expect("cleanup enters deleting state");
    assert_eq!(checkpoint.next_state, ObservedEnvironmentState::Deleting);
    assert!(!checkpoint.operation_complete);
    assert!(checkpoint.cleanup_evidence.is_none());

    instance.observed_state = ObservedEnvironmentState::Deleting;

    let observation = provider
        .execute(ReconcileAction::Cleanup, &instance)
        .await
        .expect("cleanup succeeds");
    assert_eq!(observation.next_state, ObservedEnvironmentState::Deleted);
    assert!(observation.operation_complete);
    assert!(observation.endpoints.is_empty());
    assert!(observation.cleanup_evidence.is_some());
    assert_eq!(
        backend
            .operations
            .lock()
            .expect("operations lock")
            .as_slice(),
        ["apply", "delete"]
    );
    assert!(backend.objects.lock().expect("objects lock").is_empty());
}

#[tokio::test]
async fn expire_stop_returns_a_non_terminal_checkpoint_for_cleanup() {
    let projection = projection();
    let mut instance = instance_for(&projection);
    instance.observed_state = ObservedEnvironmentState::Expiring;
    instance.desired_state = DesiredEnvironmentState::Deleted;
    instance.operation.kind = EnvironmentOperationKind::Expire;
    let backend = Arc::new(FixtureBackend::default());
    let provider = provider(projection, backend.clone());

    let observation = provider
        .execute(ReconcileAction::Stop, &instance)
        .await
        .expect("expire stop succeeds");

    assert_eq!(observation.next_state, ObservedEnvironmentState::Stopped);
    assert!(!observation.operation_complete);
    assert!(observation.endpoints.is_empty());
    assert_eq!(
        backend
            .operations
            .lock()
            .expect("operations lock")
            .as_slice(),
        ["stop"]
    );
}

#[test]
fn invalid_release_storage_or_ssh_bootstrap_fails_closed() {
    let projection = projection();
    let instance = instance_for(&projection);
    let backend = Arc::new(FixtureBackend::default());
    let vm_provider = provider(projection.clone(), backend);

    let mut withdrawn = resolved(projection.clone());
    withdrawn.withdrawn_at = Some(timestamp("2026-07-16T08:20:00.000Z"));
    assert!(matches!(
        vm_provider.plan(&instance, &withdrawn, ReconcileAction::Provision),
        Err(ReleaseProjectionError::Withdrawn)
    ));

    assert!(ubuntu_base_disk("INVALID".to_owned()).is_err());
    assert!(
        KubeVirtSshBootstrap::new(
            "access-system".to_owned(),
            "openssh-gateway".to_owned(),
            "labweaver-evaluation".to_owned(),
            "evaluation-freeze-worker".to_owned(),
            "not-a-public-key",
        )
        .is_err()
    );

    for extra_entry in [
        EnvironmentEntrySpec {
            name: "web".to_owned(),
            protocol: EndpointProtocol::Http,
            service_port: 80,
        },
        EnvironmentEntrySpec {
            name: "secure-web".to_owned(),
            protocol: EndpointProtocol::Https,
            service_port: 443,
        },
        EnvironmentEntrySpec {
            name: "admin-ssh".to_owned(),
            protocol: EndpointProtocol::Ssh,
            service_port: 22,
        },
    ] {
        let mut partial_projection = projection.clone();
        partial_projection
            .environment_spec
            .entries
            .push(extra_entry);
        rebind_projection(&mut partial_projection);
        let partial_instance = instance_for(&partial_projection);
        let partial_provider = provider(
            partial_projection.clone(),
            Arc::new(FixtureBackend::default()),
        );
        assert!(matches!(
            partial_provider.plan(
                &partial_instance,
                &resolved(partial_projection),
                ReconcileAction::Provision,
            ),
            Err(ReleaseProjectionError::SecurityPostureInvalid)
        ));
    }

    assert!(
        KubeVirtResourceBudget::new(0, 1_000, 4_000, 262_144_000, 1_073_741_824, 10_737_418_240)
            .is_err()
    );
    let insufficient_scratch = provider_with_budget(
        projection.clone(),
        Arc::new(FixtureBackend::default()),
        KubeVirtResourceBudget::new(536_870_912, 1_000, 4_000, 262_144_000, 1_073_741_824, 1)
            .expect("non-zero resource budget"),
    );
    assert!(matches!(
        insufficient_scratch.plan(&instance, &resolved(projection), ReconcileAction::Provision,),
        Err(ReleaseProjectionError::SecurityPostureInvalid)
    ));
}

fn provider(
    projection: ReleasePublished,
    backend: Arc<FixtureBackend>,
) -> KubeVirtProvider<FixtureBackend, FixtureResolver, FixtureObservationStore> {
    provider_with_budget(
        projection,
        backend,
        KubeVirtResourceBudget::new(
            536_870_912,
            1_000,
            4_000,
            262_144_000,
            1_073_741_824,
            10_737_418_240,
        )
        .expect("KubeVirt resource budget"),
    )
}

fn provider_with_budget(
    projection: ReleasePublished,
    backend: Arc<FixtureBackend>,
    resource_budget: KubeVirtResourceBudget,
) -> KubeVirtProvider<FixtureBackend, FixtureResolver, FixtureObservationStore> {
    KubeVirtProvider::new(
        "kubevirt-primary-v1".to_owned(),
        backend,
        Arc::new(FixtureResolver {
            projection,
            authority_now: timestamp("2026-07-16T08:30:00.000Z"),
            withdrawn_at: None,
        }),
        Arc::new(FixtureObservationStore::default()),
        KubeVirtProviderConfiguration::new(
            revision(1),
            vec![ubuntu_base_disk("local-path".to_owned()).expect("base disk binding")],
            None,
            KubeVirtSshBootstrap::new(
                "access-system".to_owned(),
                "openssh-gateway".to_owned(),
                "labweaver-evaluation".to_owned(),
                "evaluation-freeze-worker".to_owned(),
                "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDIhz2GK/XCUj4i6Q5yQJNL1MKDXETe1aM1lHYMGt2SQ",
            )
            .expect("SSH bootstrap"),
            resource_budget,
        )
        .expect("provider configuration"),
    )
    .expect("provider configuration")
}

fn ubuntu_base_disk(
    storage_class_name: String,
) -> Result<KubeVirtBaseDiskBinding, ReleaseProjectionError> {
    KubeVirtBaseDiskBinding::new(
        "ubuntu-24.04-v1".to_owned(),
        concat!(
            "docker://quay.io/containerdisks/ubuntu@",
            "sha256:d28194a16351320fa9a093e18233033508a745566eb8ba3b309c32924bf155a5"
        )
        .to_owned(),
        "ffe6203da54deeb6db5d2a98a83f9ec8e55f149d3f7ba622e1abe5fa966ee3d6".to_owned(),
        10_737_418_240,
        contracts::supply_chain::VirtualMachineDiskFormat::Qcow2,
        "vm-rwo-primary-v1".to_owned(),
        storage_class_name,
        "labweaver-system".to_owned(),
        "ubuntu-lab-base-v1".to_owned(),
        "lab".to_owned(),
        22,
    )
}

fn resolved(projection: ReleasePublished) -> ResolvedContainerRelease {
    ResolvedContainerRelease {
        projection,
        authority_now: timestamp("2026-07-16T08:30:00.000Z"),
        withdrawn_at: None,
    }
}

fn resource<'a>(
    plan: &'a KubeVirtResourcePlan,
    kind: &str,
) -> &'a environment_service::KubeVirtResource {
    plan.resources
        .iter()
        .find(|resource| resource.kind == kind)
        .unwrap_or_else(|| panic!("missing {kind}"))
}

fn named_resource<'a>(
    plan: &'a KubeVirtResourcePlan,
    kind: &str,
    name: &str,
) -> &'a environment_service::KubeVirtResource {
    plan.resources
        .iter()
        .find(|resource| resource.kind == kind && resource.name == name)
        .unwrap_or_else(|| panic!("missing {kind}/{name}"))
}

fn count_resource(plan: &KubeVirtResourcePlan, kind: &str) -> usize {
    plan.resources
        .iter()
        .filter(|resource| resource.kind == kind)
        .count()
}

fn instance_for(projection: &ReleasePublished) -> contracts::environment::EnvironmentInstance {
    let mut instance = support::requested_instance();
    instance.project_id = projection.release.project_id;
    instance.course_id = projection.release.course_id;
    instance.release_id = projection.release.id;
    instance.release_version = projection.release.version;
    instance.runtime_kind = RuntimeKind::VirtualMachine;
    "kubevirt-primary-v1".clone_into(&mut instance.provider_binding);
    instance
}

#[allow(
    clippy::too_many_lines,
    reason = "the fixture deliberately constructs the complete immutable VM release identity"
)]
fn projection() -> ReleasePublished {
    let base_disk = VirtualMachineBaseDisk {
        binding: "ubuntu-24.04-v1".to_owned(),
        source_registry_digest: concat!(
            "docker://quay.io/containerdisks/ubuntu@",
            "sha256:d28194a16351320fa9a093e18233033508a745566eb8ba3b309c32924bf155a5"
        )
        .to_owned(),
        capacity_bytes: 10_737_418_240,
    };
    let environment_spec: EnvironmentSpec = serde_json::from_value(json!({
        "apiVersion":"environment.labweaver.io/v1",
        "kind":"EnvironmentSpec",
        "name":"vm-lab",
        "class":"experiment",
        "resources":{"cpuMillicores":2000,"memoryBytes":2_147_483_648_u64,"storageBytes":10_737_418_240_u64},
        "network":{"mode":"deny_all"},
        "entries":[{"name":"ssh","protocol":"ssh","servicePort":22}],
        "security":{
            "userPolicy":"non_root_required",
            "rootFilesystemPolicy":"mutable_required",
            "privilegeEscalationPolicy":"deny",
            "publicExposurePolicy":"deny",
            "securityProfileBinding":"kubevirt-restricted-v1"
        },
        "runtime":{
            "kind":"virtual_machine",
            "provider_binding":"kubevirt-primary-v1",
            "base_disk":base_disk,
            "storage_class_binding":"vm-rwo-primary-v1",
            "ssh_port":22
        },
        "retention":{
            "policyId":PolicyId::new(),"policyRevision":1,"class":"run_evidence",
            "retainUntil":"2026-08-16T08:00:00.000Z","disposition":"delete"
        }
    }))
    .expect("valid EnvironmentSpec");
    let artifact_id = ImageArtifactId::new();
    let project_id = contracts::ProjectId::new();
    let course_id = Some(contracts::CourseId::new());
    let candidate_id = CandidateId::new();
    let published_at = timestamp("2026-07-16T08:00:00.000Z");
    let release = EnvironmentTemplateRelease {
        id: ReleaseId::new(),
        project_id,
        course_id,
        version: 1,
        candidate_id,
        agent_run_id: contracts::AgentRunId::new(),
        candidate_revision: revision(1),
        runtime_kind: RuntimeKind::VirtualMachine,
        approval: CandidateApproval {
            id: ApprovalId::new(),
            candidate_id,
            candidate_revision: revision(1),
            policy_revision: revision(1),
            trust_revision: revision(1),
            actor_id: ActorId::new(),
            decision: CandidateDecision::Approved,
            reason: "reviewed".to_owned(),
            decided_at: published_at,
        },
        artifact: ImageArtifact::VirtualMachine {
            id: artifact_id,
            base_disk: base_disk.clone(),
            format: VirtualMachineDiskFormat::Qcow2,
        },
        published_by: ActorId::new(),
        published_at,
    };
    let projection = ReleasePublished {
        release,
        environment_spec,
    };
    projection.validate().expect("valid projection");
    projection
}

fn rebind_projection(projection: &mut ReleasePublished) {
    projection.validate().expect("valid rebound projection");
}

fn revision(value: u64) -> Revision {
    Revision::new(value).expect("positive revision")
}

fn timestamp(value: &str) -> UtcTimestamp {
    UtcTimestamp::from_str(value).expect("valid timestamp")
}

const RUNTIME_MANIFEST_HEX: &str =
    "1111111111111111111111111111111111111111111111111111111111111111";
const RUNTIME_DIGEST: &str = concat!(
    "docker://quay.io/containerdisks/debian@sha256:",
    "1111111111111111111111111111111111111111111111111111111111111111"
);
const SECOND_RUNTIME_MANIFEST_HEX: &str =
    "2222222222222222222222222222222222222222222222222222222222222222";
const SECOND_RUNTIME_DIGEST: &str = concat!(
    "docker://quay.io/containerdisks/debian@sha256:",
    "2222222222222222222222222222222222222222222222222222222222222222"
);

#[derive(Clone, Copy, Eq, PartialEq)]
enum FixtureImportOutcome {
    Succeeds,
    Fails,
}

struct FixtureCdiImport {
    data_sources: Mutex<BTreeMap<(String, String), BTreeMap<String, String>>>,
    applied_data_volumes: Mutex<Vec<String>>,
    published_data_sources: Mutex<Vec<String>>,
    outcome: FixtureImportOutcome,
}

impl FixtureCdiImport {
    fn new(outcome: FixtureImportOutcome) -> Self {
        Self {
            data_sources: Mutex::new(BTreeMap::new()),
            applied_data_volumes: Mutex::new(Vec::new()),
            published_data_sources: Mutex::new(Vec::new()),
            outcome,
        }
    }

    fn seed_data_source(&self, namespace: &str, name: &str, annotations: BTreeMap<String, String>) {
        self.data_sources
            .lock()
            .expect("data sources lock")
            .insert((namespace.to_owned(), name.to_owned()), annotations);
    }
}

/// Mirrors the reviewed annotation contract the real importer writes so the reuse path is
/// exercised without a cluster.
fn recorded_identity_annotations(import: &KubeVirtBaseDiskImport) -> BTreeMap<String, String> {
    let mut annotations = BTreeMap::from([
        (
            "labweaver.io/source-registry".to_owned(),
            import.source_registry_digest.clone(),
        ),
        (
            "labweaver.io/base-disk-identity".to_owned(),
            import.identity.as_str().to_owned(),
        ),
        (
            "labweaver.io/base-disk-capacity-bytes".to_owned(),
            import.capacity_bytes.to_string(),
        ),
    ]);
    if import.identity == KubeVirtBaseDiskIdentity::ReviewedDiskSha256 {
        annotations.insert(
            "labweaver.io/disk-sha256".to_owned(),
            import.disk_sha256.clone(),
        );
    }
    annotations
}

#[async_trait]
impl CdiImportClient for FixtureCdiImport {
    async fn read_base_data_source(
        &self,
        namespace: &str,
        name: &str,
    ) -> Result<Option<BTreeMap<String, String>>, CdiImportError> {
        Ok(self
            .data_sources
            .lock()
            .expect("data sources lock")
            .get(&(namespace.to_owned(), name.to_owned()))
            .cloned())
    }

    async fn import_base_data_volume(
        &self,
        import: &KubeVirtBaseDiskImport,
    ) -> Result<String, CdiImportError> {
        self.applied_data_volumes
            .lock()
            .expect("applied lock")
            .push(import.data_volume_name());
        if self.outcome == FixtureImportOutcome::Fails {
            return Err(CdiImportError::ImportFailed);
        }
        Ok("11111111-1111-4111-8111-111111111111".to_owned())
    }

    async fn publish_base_data_source(
        &self,
        import: &KubeVirtBaseDiskImport,
        data_volume_uid: &str,
    ) -> Result<(), CdiImportError> {
        self.published_data_sources
            .lock()
            .expect("published lock")
            .push(format!("{}:{data_volume_uid}", import.data_source_name));
        self.seed_data_source(
            &import.data_source_namespace,
            &import.data_source_name,
            recorded_identity_annotations(import),
        );
        Ok(())
    }
}

fn import_request(binding: &KubeVirtBaseDiskBinding) -> KubeVirtBaseDiskImport {
    KubeVirtBaseDiskImport {
        data_source_namespace: binding.data_source_namespace.clone(),
        data_source_name: binding.data_source_name.clone(),
        source_registry_digest: binding.source_registry_digest.clone(),
        disk_sha256: binding.disk_sha256.clone(),
        identity: KubeVirtBaseDiskIdentity::ReviewedDiskSha256,
        storage_class_name: binding.storage_class_name.clone(),
        capacity_bytes: binding.capacity_bytes,
    }
}

#[tokio::test]
async fn base_disk_import_creates_then_reuses_without_recreating() {
    let binding = ubuntu_base_disk("local-path".to_owned()).expect("base disk binding");
    let import = import_request(&binding);
    let client = FixtureCdiImport::new(FixtureImportOutcome::Succeeds);

    let created = ensure_base_disk(&client, &import)
        .await
        .expect("first use imports the base disk");
    assert_eq!(created.data_source_namespace, "labweaver-system");
    assert_eq!(created.data_source_name, "ubuntu-lab-base-v1");
    assert_eq!(
        client
            .applied_data_volumes
            .lock()
            .expect("applied lock")
            .as_slice(),
        ["ubuntu-lab-base-v1-seed"]
    );
    assert_eq!(
        client
            .published_data_sources
            .lock()
            .expect("published lock")
            .len(),
        1
    );

    ensure_base_disk(&client, &import)
        .await
        .expect("second use reuses the published DataSource");
    assert_eq!(
        client
            .applied_data_volumes
            .lock()
            .expect("applied lock")
            .len(),
        1
    );
    assert_eq!(
        client
            .published_data_sources
            .lock()
            .expect("published lock")
            .len(),
        1
    );
}

#[tokio::test]
async fn base_disk_import_rejects_recorded_identity_drift() {
    let binding = ubuntu_base_disk("local-path".to_owned()).expect("base disk binding");
    let import = import_request(&binding);
    let client = FixtureCdiImport::new(FixtureImportOutcome::Succeeds);

    let mut registry_drift = recorded_identity_annotations(&import);
    registry_drift.insert(
        "labweaver.io/source-registry".to_owned(),
        RUNTIME_DIGEST.to_owned(),
    );
    client.seed_data_source("labweaver-system", "ubuntu-lab-base-v1", registry_drift);
    assert!(matches!(
        ensure_base_disk(&client, &import).await,
        Err(CdiImportError::IdentityMismatch)
    ));

    let mut disk_drift = recorded_identity_annotations(&import);
    disk_drift.insert(
        "labweaver.io/disk-sha256".to_owned(),
        SECOND_RUNTIME_MANIFEST_HEX.to_owned(),
    );
    client.seed_data_source("labweaver-system", "ubuntu-lab-base-v1", disk_drift);
    assert!(matches!(
        ensure_base_disk(&client, &import).await,
        Err(CdiImportError::IdentityMismatch)
    ));

    assert!(
        client
            .applied_data_volumes
            .lock()
            .expect("applied lock")
            .is_empty()
    );
}

#[tokio::test]
async fn base_disk_import_rejects_over_capacity_reuse() {
    let binding = ubuntu_base_disk("local-path".to_owned()).expect("base disk binding");
    let import = import_request(&binding);
    let client = FixtureCdiImport::new(FixtureImportOutcome::Succeeds);

    let mut over_capacity = recorded_identity_annotations(&import);
    over_capacity.insert(
        "labweaver.io/base-disk-capacity-bytes".to_owned(),
        (import.capacity_bytes + 1).to_string(),
    );
    client.seed_data_source("labweaver-system", "ubuntu-lab-base-v1", over_capacity);

    assert!(matches!(
        ensure_base_disk(&client, &import).await,
        Err(CdiImportError::CapacityExceeded)
    ));
    assert!(
        client
            .applied_data_volumes
            .lock()
            .expect("applied lock")
            .is_empty()
    );
}

#[tokio::test]
async fn base_disk_import_failure_never_publishes_a_data_source() {
    let binding = ubuntu_base_disk("local-path".to_owned()).expect("base disk binding");
    let import = import_request(&binding);
    let client = FixtureCdiImport::new(FixtureImportOutcome::Fails);

    for _ in 0..2 {
        assert!(matches!(
            ensure_base_disk(&client, &import).await,
            Err(CdiImportError::ImportFailed)
        ));
    }
    assert_eq!(
        client
            .applied_data_volumes
            .lock()
            .expect("applied lock")
            .len(),
        2
    );
    assert!(
        client
            .published_data_sources
            .lock()
            .expect("published lock")
            .is_empty()
    );
}

fn runtime_policy(max_bases: u32, max_capacity_bytes: u64) -> RuntimeVmBasePolicy {
    RuntimeVmBasePolicy::new(
        "vm-rwo-primary-v1".to_owned(),
        "local-path".to_owned(),
        "labweaver-system".to_owned(),
        "lab".to_owned(),
        22,
        max_bases,
        max_capacity_bytes,
    )
    .expect("runtime vm base policy")
}

#[test]
fn runtime_policy_rejects_invalid_fields() {
    let base = (
        "vm-rwo-primary-v1".to_owned(),
        "local-path".to_owned(),
        "labweaver-system".to_owned(),
        "lab".to_owned(),
    );
    assert!(
        RuntimeVmBasePolicy::new(
            String::new(),
            base.1.clone(),
            base.2.clone(),
            base.3.clone(),
            22,
            8,
            1
        )
        .is_err()
    );
    assert!(
        RuntimeVmBasePolicy::new(
            base.0.clone(),
            base.1.clone(),
            base.2.clone(),
            "Lab".to_owned(),
            22,
            8,
            1
        )
        .is_err()
    );
    assert!(
        RuntimeVmBasePolicy::new(
            base.0.clone(),
            base.1.clone(),
            base.2.clone(),
            base.3.clone(),
            0,
            8,
            1
        )
        .is_err()
    );
    assert!(
        RuntimeVmBasePolicy::new(
            base.0.clone(),
            base.1.clone(),
            base.2.clone(),
            base.3.clone(),
            22,
            0,
            1
        )
        .is_err()
    );
    assert!(RuntimeVmBasePolicy::new(base.0, base.1, base.2, base.3, 22, 8, 0).is_err());
}

fn remap_runtime_base(
    mut projection: ReleasePublished,
    binding: &str,
    source_registry_digest: &str,
    capacity_bytes: u64,
) -> ReleasePublished {
    let base_disk = VirtualMachineBaseDisk {
        binding: binding.to_owned(),
        source_registry_digest: source_registry_digest.to_owned(),
        capacity_bytes,
    };
    if let ImageArtifact::VirtualMachine {
        base_disk: artifact_base,
        ..
    } = &mut projection.release.artifact
    {
        *artifact_base = base_disk.clone();
    }
    if let EnvironmentRuntimeSpec::VirtualMachine {
        base_disk: spec_base,
        ..
    } = &mut projection.environment_spec.runtime
    {
        *spec_base = base_disk;
    }
    rebind_projection(&mut projection);
    projection
}

fn provider_with_runtime_policy(
    projection: ReleasePublished,
    backend: Arc<FixtureBackend>,
    policy: RuntimeVmBasePolicy,
) -> KubeVirtProvider<FixtureBackend, FixtureResolver, FixtureObservationStore> {
    KubeVirtProvider::new(
        "kubevirt-primary-v1".to_owned(),
        backend,
        Arc::new(FixtureResolver {
            projection,
            authority_now: timestamp("2026-07-16T08:30:00.000Z"),
            withdrawn_at: None,
        }),
        Arc::new(FixtureObservationStore::default()),
        KubeVirtProviderConfiguration::new(
            revision(1),
            vec![ubuntu_base_disk("local-path".to_owned()).expect("base disk binding")],
            Some(policy),
            KubeVirtSshBootstrap::new(
                "access-system".to_owned(),
                "openssh-gateway".to_owned(),
                "labweaver-evaluation".to_owned(),
                "evaluation-freeze-worker".to_owned(),
                "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDIhz2GK/XCUj4i6Q5yQJNL1MKDXETe1aM1lHYMGt2SQ",
            )
            .expect("SSH bootstrap"),
            KubeVirtResourceBudget::new(
                536_870_912,
                1_000,
                4_000,
                262_144_000,
                1_073_741_824,
                10_737_418_240,
            )
            .expect("KubeVirt resource budget"),
        )
        .expect("provider configuration"),
    )
    .expect("provider configuration")
}

#[test]
fn runtime_registered_base_resolves_by_declared_digest() {
    let projection =
        remap_runtime_base(projection(), "debian-13-v1", RUNTIME_DIGEST, 2_147_483_648);
    let provider = provider_with_runtime_policy(
        projection.clone(),
        Arc::new(FixtureBackend::default()),
        runtime_policy(8, 137_438_953_472),
    );

    let plan = provider
        .plan(
            &instance_for(&projection),
            &resolved(projection),
            ReconcileAction::Provision,
        )
        .expect("runtime base resolves through the policy");
    assert_eq!(
        plan.base_disk_identity,
        KubeVirtBaseDiskIdentity::RuntimeRegistryDigest
    );
    assert_eq!(plan.base_disk_disk_sha256, RUNTIME_MANIFEST_HEX);
    assert_eq!(
        plan.base_disk_data_source_name,
        format!("vm-base-{}", &RUNTIME_MANIFEST_HEX[..32])
    );
    assert_eq!(plan.base_disk_data_source_namespace, "labweaver-system");

    let data_volume = resource(&plan, "DataVolume");
    let annotations = &data_volume.document["metadata"]["annotations"];
    assert_eq!(
        annotations["labweaver.io/base-disk-identity"],
        "registry-digest"
    );
    assert_eq!(
        annotations["labweaver.io/base-disk-source-registry"],
        RUNTIME_DIGEST
    );
    assert!(annotations.get("labweaver.io/base-disk-sha256").is_none());
    assert_eq!(
        data_volume.document["spec"]["sourceRef"]["name"],
        plan.base_disk_data_source_name
    );
    assert_eq!(
        data_volume.document["spec"]["sourceRef"]["namespace"],
        "labweaver-system"
    );
}

#[test]
fn seeded_base_keeps_reviewed_disk_identity_annotation() {
    let projection = projection();
    let provider = provider_with_runtime_policy(
        projection.clone(),
        Arc::new(FixtureBackend::default()),
        runtime_policy(8, 137_438_953_472),
    );
    let plan = provider
        .plan(
            &instance_for(&projection),
            &resolved(projection),
            ReconcileAction::Provision,
        )
        .expect("seeded base resolves");
    assert_eq!(
        plan.base_disk_identity,
        KubeVirtBaseDiskIdentity::ReviewedDiskSha256
    );
    let annotations = &resource(&plan, "DataVolume").document["metadata"]["annotations"];
    assert_eq!(
        annotations["labweaver.io/base-disk-identity"],
        "disk-sha256"
    );
    assert_eq!(
        annotations["labweaver.io/base-disk-sha256"],
        "ffe6203da54deeb6db5d2a98a83f9ec8e55f149d3f7ba622e1abe5fa966ee3d6"
    );
}

#[test]
fn runtime_base_capacity_and_count_bounds_fail_closed() {
    let oversized = remap_runtime_base(projection(), "debian-13-v1", RUNTIME_DIGEST, 4_294_967_296);
    let provider = provider_with_runtime_policy(
        oversized.clone(),
        Arc::new(FixtureBackend::default()),
        runtime_policy(8, 2_147_483_648),
    );
    assert!(matches!(
        provider.plan(
            &instance_for(&oversized),
            &resolved(oversized),
            ReconcileAction::Provision
        ),
        Err(ReleaseProjectionError::VmBaseCapacityExceeded)
    ));

    let bounded = provider_with_runtime_policy(
        projection(),
        Arc::new(FixtureBackend::default()),
        runtime_policy(1, 137_438_953_472),
    );
    let first = remap_runtime_base(projection(), "debian-13-v1", RUNTIME_DIGEST, 1_073_741_824);
    assert!(
        bounded
            .plan(
                &instance_for(&first),
                &resolved(first),
                ReconcileAction::Provision
            )
            .is_ok()
    );
    let second = remap_runtime_base(
        projection(),
        "debian-12-v1",
        SECOND_RUNTIME_DIGEST,
        1_073_741_824,
    );
    assert!(matches!(
        bounded.plan(
            &instance_for(&second),
            &resolved(second),
            ReconcileAction::Provision
        ),
        Err(ReleaseProjectionError::VmBaseImportFailed)
    ));
    let replay = remap_runtime_base(projection(), "debian-13-v1", RUNTIME_DIGEST, 1_073_741_824);
    assert!(
        bounded
            .plan(
                &instance_for(&replay),
                &resolved(replay),
                ReconcileAction::Provision
            )
            .is_ok()
    );
}

#[test]
fn unseeded_base_without_runtime_policy_fails_closed() {
    let projection =
        remap_runtime_base(projection(), "debian-13-v1", RUNTIME_DIGEST, 1_073_741_824);
    let provider = provider(projection.clone(), Arc::new(FixtureBackend::default()));
    assert!(matches!(
        provider.plan(
            &instance_for(&projection),
            &resolved(projection),
            ReconcileAction::Provision
        ),
        Err(ReleaseProjectionError::SecurityPostureInvalid)
    ));
}
