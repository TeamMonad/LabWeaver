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
use contracts::authoring::{CandidateApproval, CandidateDecision, EnvironmentSpec, RuntimeKind};
use contracts::environment::{DesiredEnvironmentState, EndpointProtocol, ObservedEnvironmentState};
use contracts::events::ReleasePublishedV2;
use contracts::supply_chain::{
    EnvironmentTemplateRelease, ImageArtifact, ImagePolicyEvaluation, SigstoreEvidence,
    VirtualMachineDiskFormat, VulnerabilitySummary,
};
use contracts::{
    ActorId, ApprovalId, ArtifactId, ArtifactRef, CandidateId, ImageArtifactId, PolicyId,
    ReleaseId, Revision, Sha256Digest, UtcTimestamp,
};
use environment_service::{
    ContainerReleasePolicy, ContainerReleaseResolver, EnvironmentProvider,
    KUBEVIRT_BACKEND_PROTOCOL_VERSION, KubeVirtBackendFence, KubeVirtCleanupPlan,
    KubeVirtObservationStore, KubeVirtObservationStoreError, KubeVirtProvider,
    KubeVirtProviderBackend, KubeVirtProviderConfiguration, KubeVirtResourcePlan,
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
    projection: ReleasePublishedV2,
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
            guest_agent_connected: !self.incomplete_readiness,
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
    assert_eq!(
        first.base_disk.sha256,
        resolved
            .projection
            .release
            .image_policy_evaluation
            .artifact_sha256
    );
    assert_eq!(first.base_disk_format, VirtualMachineDiskFormat::Qcow2);
    assert_eq!(first.storage_class_name, "local-path");
    assert_eq!(count_resource(&first, "DataVolume"), 1);
    assert_eq!(count_resource(&first, "VirtualMachine"), 1);
    assert_eq!(count_resource(&first, "Service"), 1);

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
        Some(&json!(first.base_disk.sha256.to_string()))
    );
    assert_eq!(
        data_volume
            .document
            .pointer("/metadata/annotations/labweaver.io~1base-disk-object-version"),
        Some(&json!(first.base_disk.object_version))
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

    let cloud_init = resource(&first, "Secret")
        .document
        .pointer("/stringData/userData")
        .and_then(serde_json::Value::as_str)
        .expect("cloud-init userData");
    assert!(cloud_init.contains("TrustedUserCAKeys /etc/ssh/labweaver_user_ca.pub"));
    assert!(cloud_init.contains("AuthorizedKeysFile none"));
    assert!(cloud_init.contains("AuthenticationMethods publickey"));
    assert!(cloud_init.contains("AllowUsers lab"));
    assert!(cloud_init.contains("PasswordAuthentication no"));
    assert!(cloud_init.contains("- [systemctl, reload, ssh]"));
    assert!(!cloud_init.contains("ssh_authorized_keys"));
    assert!(!cloud_init.contains("PRIVATE KEY"));

    assert_eq!(
        resource(&first, "Service").document.pointer("/spec/type"),
        Some(&json!("ClusterIP"))
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
}

#[tokio::test]
async fn readiness_requires_vm_guest_agent_ssh_and_current_generation() {
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
    let provider = provider(projection.clone(), backend);

    let mut expired = resolved(projection.clone());
    expired.authority_now = projection.release.image_policy_evaluation.valid_until;
    assert!(matches!(
        provider.plan(&instance, &expired, ReconcileAction::Provision),
        Err(ReleaseProjectionError::EvidenceExpired)
    ));
    let mut withdrawn = resolved(projection.clone());
    withdrawn.withdrawn_at = Some(timestamp("2026-07-16T08:20:00.000Z"));
    assert!(matches!(
        provider.plan(&instance, &withdrawn, ReconcileAction::Provision),
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
}

fn provider(
    projection: ReleasePublishedV2,
    backend: Arc<FixtureBackend>,
) -> KubeVirtProvider<FixtureBackend, FixtureResolver, FixtureObservationStore> {
    let image_policy_id = projection.release.image_policy_evaluation.policy_id;
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
            ContainerReleasePolicy::new(
                image_policy_id,
                revision(1),
                revision(1),
                Sha256Digest::of_bytes(b"trust-bundle"),
            )
            .expect("release policy"),
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
        ),
    )
    .expect("provider configuration")
}

fn resolved(projection: ReleasePublishedV2) -> ResolvedContainerRelease {
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

fn instance_for(projection: &ReleasePublishedV2) -> contracts::environment::EnvironmentInstance {
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
fn projection() -> ReleasePublishedV2 {
    let base_disk = artifact_ref("application/x-qemu-disk");
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
    let artifact_sha256 = base_disk.sha256;
    let trust_bundle_sha256 = Sha256Digest::of_bytes(b"trust-bundle");
    let artifact_id = ImageArtifactId::new();
    let course_id = contracts::CourseId::new();
    let candidate_id = CandidateId::new();
    let published_at = timestamp("2026-07-16T08:00:00.000Z");
    let release = EnvironmentTemplateRelease {
        id: ReleaseId::new(),
        course_id,
        version: 1,
        candidate_id,
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
            sbom: artifact_ref("application/spdx+json"),
            provenance: artifact_ref("application/vnd.in-toto+json"),
            signature: SigstoreEvidence {
                trust_bundle_sha256,
                fulcio_issuer: "https://fulcio.internal".to_owned(),
                certificate_subject: "spiffe://labweaver/vm-image-builder".to_owned(),
                subject_digest: artifact_sha256,
                certificate_sha256: Sha256Digest::of_bytes(b"certificate"),
                signature_sha256: Sha256Digest::of_bytes(b"signature"),
                rekor_log_id: "rekor-private-v1".to_owned(),
                rekor_log_index: 1,
                rekor_inclusion_proof_sha256: Sha256Digest::of_bytes(b"rekor-proof"),
                ct_log_id: "ct-private-v1".to_owned(),
                sct_sha256: Sha256Digest::of_bytes(b"sct"),
                verified_at: published_at,
            },
        },
        image_policy_evaluation: ImagePolicyEvaluation {
            artifact_id,
            artifact_sha256,
            policy_id: PolicyId::new(),
            policy_revision: revision(1),
            scanner_name: "trivy".to_owned(),
            scanner_version: "0.58.0".to_owned(),
            scanner_database_sha256: Sha256Digest::of_bytes(b"trivy-db"),
            vulnerabilities: VulnerabilitySummary {
                unknown: 0,
                low: 0,
                medium: 0,
                high: 1,
                critical: 0,
            },
            trust_bundle_sha256,
            expected_fulcio_issuer: "https://fulcio.internal".to_owned(),
            expected_certificate_subject: "spiffe://labweaver/vm-image-builder".to_owned(),
            require_rekor_inclusion: true,
            require_ct_sct: true,
            evaluated_at: published_at,
            max_evidence_age_milliseconds: 3_600_000,
            valid_until: timestamp("2026-07-16T09:00:00.000Z"),
            passed: true,
        },
        published_by: ActorId::new(),
        published_at,
    };
    let projection_sha256 = Sha256Digest::of_canonical(&json!({
        "release": &release,
        "environmentSpec": &environment_spec,
    }))
    .expect("projection hash");
    let projection = ReleasePublishedV2 {
        release,
        environment_spec,
        projection_sha256,
    };
    projection.validate().expect("valid projection");
    projection
}

fn artifact_ref(media_type: &str) -> ArtifactRef {
    ArtifactRef {
        artifact_id: ArtifactId::new(),
        store_binding: "artifact-store-v1".to_owned(),
        object_version: "version-1".to_owned(),
        sha256: Sha256Digest::of_bytes(media_type.as_bytes()),
        size_bytes: 128,
        media_type: media_type.to_owned(),
    }
}

fn revision(value: u64) -> Revision {
    Revision::new(value).expect("positive revision")
}

fn timestamp(value: &str) -> UtcTimestamp {
    UtcTimestamp::from_str(value).expect("valid timestamp")
}
