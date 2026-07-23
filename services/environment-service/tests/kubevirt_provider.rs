//! `KubeVirt` release, fencing, resource-plan, readiness and cleanup contract tests.
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures use explicit assertion messages for invalid setup"
)]

mod support;

use std::collections::BTreeSet;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use contracts::authoring::{
    CandidateApproval, CandidateDecision, EnvironmentEntrySpec, EnvironmentRuntimeSpec,
    EnvironmentSpec, RuntimeKind,
};
use contracts::environment::{DesiredEnvironmentState, EndpointProtocol, ObservedEnvironmentState};
use contracts::events::ReleasePublished;
use contracts::supply_chain::{
    EnvironmentTemplateRelease, ImageArtifact, VirtualMachineBaseDisk, VirtualMachineDiskFormat,
};
use contracts::{
    ActorId, ApprovalId, ArtifactId, ArtifactRef, CandidateId, ImageArtifactId, PolicyId,
    ReleaseId, Revision, Sha256Digest, UtcTimestamp,
};
use environment_service::{
    ContainerReleaseResolver, EnvironmentProvider, KUBEVIRT_BACKEND_PROTOCOL_VERSION,
    KubeVirtBackendFence, KubeVirtCleanupPlan, KubeVirtObservationStore,
    KubeVirtObservationStoreError, KubeVirtProvider, KubeVirtProviderBackend,
    KubeVirtProviderConfiguration, KubeVirtResourceBudget, KubeVirtResourcePlan,
    KubeVirtRunningObservation, KubeVirtSshBootstrap, KubeVirtStoppedObservation,
    KubeVirtStorageBinding, ProviderFailure, ReconcileAction, ReleaseProjectionError,
    ResolvedContainerRelease,
};
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
        self.fences.lock().expect("fences lock").push(*fence);
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
            sha256: Sha256Digest::of_bytes(plan.namespace.as_bytes()),
            size_bytes: 1,
            media_type: "application/json".to_owned(),
        })
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
        Some(&json!(first.base_disk.disk_sha256.to_string()))
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
        Some(&json!(false))
    );
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
    assert!(cloud_init.contains("AuthorizedKeysFile none"));
    assert!(cloud_init.contains("AuthenticationMethods publickey"));
    assert!(cloud_init.contains("AllowUsers lab"));
    assert!(cloud_init.contains("PasswordAuthentication no"));
    assert!(cloud_init.contains("AllowAgentForwarding no"));
    assert!(!cloud_init.contains("PermitAgentForwarding"));
    assert!(cloud_init.contains("- [sshd, -t]"));
    assert!(cloud_init.contains("- [systemctl, enable, --now, ssh.socket]"));
    assert!(!cloud_init.contains("ssh_authorized_keys"));
    assert!(!cloud_init.contains("PRIVATE KEY"));

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
        Some(&json!("kubevirt-executor"))
    );
    assert_eq!(
        ingress.document.pointer(
            "/spec/ingress/0/from/1/namespaceSelector/matchLabels/kubernetes.io~1metadata.name"
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
    instance.observed_state = ObservedEnvironmentState::Deleting;
    instance.desired_state = DesiredEnvironmentState::Deleted;
    instance.generation = 2;
    instance.operation.id = contracts::OperationId::new();

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

    assert!(
        KubeVirtStorageBinding::new(
            "vm-rwo-primary-v1".to_owned(),
            "INVALID".to_owned(),
            "labweaver-system".to_owned(),
            "ubuntu-lab-base-v1".to_owned(),
        )
        .is_err()
    );
    assert!(
        KubeVirtSshBootstrap::new(
            "access-system".to_owned(),
            "openssh-gateway".to_owned(),
            "lab".to_owned(),
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
            KubeVirtStorageBinding::new(
                "vm-rwo-primary-v1".to_owned(),
                "local-path".to_owned(),
                "labweaver-system".to_owned(),
                "ubuntu-lab-base-v1".to_owned(),
            )
            .expect("storage binding"),
            KubeVirtSshBootstrap::new(
                "access-system".to_owned(),
                "openssh-gateway".to_owned(),
                "lab".to_owned(),
                "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDIhz2GK/XCUj4i6Q5yQJNL1MKDXETe1aM1lHYMGt2SQ",
            )
            .expect("SSH bootstrap"),
            resource_budget,
        ),
    )
    .expect("provider configuration")
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
        disk_sha256: Sha256Digest::of_bytes(b"vm-base-disk"),
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
    let environment_spec_sha256 = Sha256Digest::of_canonical(&environment_spec).expect("spec hash");
    let artifact_id = ImageArtifactId::new();
    let course_id = contracts::CourseId::new();
    let candidate_id = CandidateId::new();
    let published_at = timestamp("2026-07-16T08:00:00.000Z");
    let release = EnvironmentTemplateRelease {
        id: ReleaseId::new(),
        course_id,
        version: 1,
        candidate_id,
        agent_run_id: contracts::AgentRunId::new(),
        candidate_revision: revision(1),
        environment_spec_sha256,
        runtime_kind: RuntimeKind::VirtualMachine,
        approval: CandidateApproval {
            id: ApprovalId::new(),
            candidate_id,
            candidate_revision: revision(1),
            candidate_sha256: environment_spec_sha256,
            policy_revision: revision(1),
            schema_sha256: Sha256Digest::of_bytes(b"schema"),
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
        image_policy_evaluation: None,
        published_by: ActorId::new(),
        published_at,
    };
    let projection_sha256 = Sha256Digest::of_canonical(&json!({
        "release": &release,
        "environmentSpec": &environment_spec,
    }))
    .expect("projection hash");
    let projection = ReleasePublished {
        release,
        environment_spec,
        projection_sha256,
    };
    projection.validate().expect("valid projection");
    projection
}

fn rebind_projection(projection: &mut ReleasePublished) {
    let environment_spec_sha256 =
        Sha256Digest::of_canonical(&projection.environment_spec).expect("environment spec hash");
    projection.release.environment_spec_sha256 = environment_spec_sha256;
    projection.release.approval.candidate_sha256 = environment_spec_sha256;
    projection.projection_sha256 = Sha256Digest::of_canonical(&json!({
        "release": &projection.release,
        "environmentSpec": &projection.environment_spec,
    }))
    .expect("release projection hash");
    projection.validate().expect("valid rebound projection");
}

fn revision(value: u64) -> Revision {
    Revision::new(value).expect("positive revision")
}

fn timestamp(value: &str) -> UtcTimestamp {
    UtcTimestamp::from_str(value).expect("valid timestamp")
}
