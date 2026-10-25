//! Container release projection and protected Gateway resource-plan tests.
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures use explicit assertion messages for invalid setup"
)]

mod support;

use std::str::FromStr;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use contracts::authoring::{
    CandidateApproval, CandidateDecision, EnvironmentSpec, NetworkPolicySpec, RuntimeKind,
};
use contracts::environment::{
    DesiredEnvironmentState, EndpointProtocol, EnvironmentOperationKind, ObservedEnvironmentState,
};
use contracts::events::ReleasePublished;
use contracts::supply_chain::{EnvironmentTemplateRelease, ImageArtifact};
use contracts::{
    ActorId, ApprovalId, ArtifactId, ArtifactRef, BuildRequestId, CandidateId, ImageArtifactId,
    OperationId, PolicyId, ReleaseId, Revision, UtcTimestamp,
};
use environment_service::{
    CONTAINER_BACKEND_PROTOCOL_VERSION, ContainerApplyObservation, ContainerBackendFence,
    ContainerProvider, ContainerProviderBackend, ContainerProviderConfiguration,
    ContainerReleasePolicy, ContainerReleaseResolver, ContainerResourcePlan, EnvironmentProvider,
    ProviderFailure, ReconcileAction, ReleaseProjectionError, ResolvedContainerRelease,
};
use persistence_sqlx::Sha256Digest;
use serde_json::json;

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
    fences: Mutex<Vec<ContainerBackendFence>>,
    restart_revisions: Mutex<Vec<Revision>>,
}

impl FixtureBackend {
    fn record(&self, operation: &str, fence: &ContainerBackendFence) {
        self.operations
            .lock()
            .expect("operations lock")
            .push(operation.to_owned());
        self.fences.lock().expect("fences lock").push(fence.clone());
    }
}

#[async_trait]
impl ContainerProviderBackend for FixtureBackend {
    async fn apply(
        &self,
        fence: &ContainerBackendFence,
        _plan: &ContainerResourcePlan,
    ) -> Result<ContainerApplyObservation, ProviderFailure> {
        self.record("apply", fence);
        Ok(ready())
    }

    async fn observe(
        &self,
        fence: &ContainerBackendFence,
        _plan: &ContainerResourcePlan,
    ) -> Result<ContainerApplyObservation, ProviderFailure> {
        self.record("observe", fence);
        Ok(ready())
    }

    async fn scale(
        &self,
        fence: &ContainerBackendFence,
        _plan: &ContainerResourcePlan,
        replicas: u32,
    ) -> Result<ContainerApplyObservation, ProviderFailure> {
        self.record(&format!("scale:{replicas}"), fence);
        Ok(ready())
    }

    async fn restart(
        &self,
        fence: &ContainerBackendFence,
        _plan: &ContainerResourcePlan,
        operation_revision: Revision,
    ) -> Result<ContainerApplyObservation, ProviderFailure> {
        self.record("restart", fence);
        self.restart_revisions
            .lock()
            .expect("restart revisions lock")
            .push(operation_revision);
        Ok(ready())
    }

    async fn delete_namespace(
        &self,
        fence: &ContainerBackendFence,
        plan: &ContainerResourcePlan,
    ) -> Result<ArtifactRef, ProviderFailure> {
        self.record("delete", fence);
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
fn plan_uses_digest_only_image_and_only_the_access_proxy() {
    let projection = projection();
    let instance = instance_for(&projection);
    let provider = provider(projection.clone(), Arc::new(FixtureBackend::default()));
    let resolved = resolved(projection.clone());
    let first = provider.plan(&instance, &resolved, ReconcileAction::Provision);
    let first = first.expect("plan is valid");
    let second = provider
        .plan(&instance, &resolved, ReconcileAction::Provision)
        .expect("same release plans deterministically");

    assert_eq!(first.plan_sha256, second.plan_sha256);
    assert!(first.image.contains("@sha256:"));
    assert!(!first.image.ends_with(":latest"));

    assert_eq!(
        resource(&first, "Namespace")
            .document
            .pointer("/metadata/finalizers/0"),
        Some(&json!("labweaver.io/environment-cleanup"))
    );
    assert_eq!(
        resource(&first, "Namespace")
            .document
            .pointer("/metadata/labels/labweaver.io~1environment"),
        Some(&json!("true"))
    );

    let deployment = resource(&first, "Deployment");
    assert_eq!(
        deployment
            .document
            .pointer("/spec/template/spec/containers/0/image"),
        Some(&json!(first.image))
    );
    assert_eq!(
        deployment.document.pointer("/spec/strategy/type"),
        Some(&json!("Recreate"))
    );
    assert_workspace_seed(&deployment.document, &first.image);
    assert_runtime_security_and_tmp(&deployment.document);
    assert_eq!(
        resource(&first, "Service").document.pointer("/spec/type"),
        Some(&json!("ClusterIP"))
    );
    assert_eq!(
        resource(&first, "ServiceAccount")
            .document
            .pointer("/imagePullSecrets/0/name"),
        Some(&json!("harbor-course-pull"))
    );
    let workspace = resource(&first, "PersistentVolumeClaim");
    assert_eq!(
        workspace.document.pointer("/spec/storageClassName"),
        Some(&json!("nfs-rwx"))
    );
    assert_eq!(
        workspace.document.pointer("/spec/accessModes/0"),
        Some(&json!("ReadWriteMany"))
    );
    assert_freeze_quota(resource(&first, "ResourceQuota"));
    assert!(
        first
            .resources
            .iter()
            .all(|resource| resource.kind != "Ingress")
    );

    assert!(
        first
            .resources
            .iter()
            .all(|resource| resource.kind != "HTTPRoute")
    );
    assert_eq!(
        resource(&first, "Service")
            .document
            .pointer("/spec/ports/0/port"),
        Some(&json!(8080))
    );
    let gateway_policy = first
        .resources
        .iter()
        .find(|resource| {
            resource.kind == "NetworkPolicy" && resource.name == "access-service-ingress"
        })
        .expect("Access Service ingress policy exists");
    assert_eq!(
        gateway_policy
            .document
            .pointer("/spec/ingress/0/from/0/podSelector/matchLabels/app.kubernetes.io~1name"),
        Some(&json!("access-service"))
    );
}

#[test]
fn plan_uses_configured_workspace_access_mode() {
    let projection = projection();
    let instance = instance_for(&projection);
    let provider = provider_with_access_mode(
        projection.clone(),
        Arc::new(FixtureBackend::default()),
        "ReadWriteOnce",
    );

    let plan = provider
        .plan(&instance, &resolved(projection), ReconcileAction::Provision)
        .expect("RWO workspace configuration is valid");

    assert_eq!(
        resource(&plan, "PersistentVolumeClaim")
            .document
            .pointer("/spec/accessModes/0"),
        Some(&json!("ReadWriteOnce"))
    );
}

fn assert_workspace_seed(document: &serde_json::Value, image: &str) {
    assert_eq!(
        document.pointer("/spec/template/spec/initContainers/0/name"),
        Some(&json!("workspace-seed"))
    );
    assert_eq!(
        document.pointer("/spec/template/spec/initContainers/0/image"),
        Some(&json!(image))
    );
    assert_eq!(
        document.pointer("/spec/template/spec/initContainers/0/command/0"),
        Some(&json!("/bin/sh"))
    );
    let command = document
        .pointer("/spec/template/spec/initContainers/0/command/2")
        .and_then(serde_json::Value::as_str)
        .expect("workspace seed command");
    assert!(command.contains("/opt/labweaver/workspace-seed"));
    assert!(command.contains("LW_ENVIRONMENT_WORKSPACE_SEED_MISSING"));
    assert!(command.contains("find /workspace"));
    assert_eq!(
        document.pointer("/spec/template/spec/initContainers/0/securityContext/runAsNonRoot"),
        Some(&json!(false))
    );
    assert_eq!(
        document
            .pointer("/spec/template/spec/initContainers/0/securityContext/readOnlyRootFilesystem"),
        Some(&json!(false))
    );
    assert_eq!(
        document
            .pointer("/spec/template/spec/initContainers/0/securityContext/capabilities/drop/0"),
        Some(&json!("ALL"))
    );
    assert_eq!(
        document.pointer("/spec/template/spec/initContainers/0/volumeMounts/0/mountPath"),
        Some(&json!("/workspace"))
    );
}

fn assert_freeze_quota(quota: &environment_service::ContainerResource) {
    assert_eq!(
        quota.document.pointer("/spec/hard/requests.cpu"),
        Some(&json!("1100m"))
    );
    assert_eq!(
        quota.document.pointer("/spec/hard/limits.cpu"),
        Some(&json!("2000m"))
    );
    assert_eq!(
        quota.document.pointer("/spec/hard/requests.memory"),
        Some(&json!(1_207_959_552_u64.to_string()))
    );
    assert_eq!(
        quota.document.pointer("/spec/hard/limits.memory"),
        Some(&json!(2_147_483_648_u64.to_string()))
    );
    assert_eq!(quota.document.pointer("/spec/hard/pods"), Some(&json!("2")));
    assert_eq!(
        quota
            .document
            .pointer("/metadata/annotations/labweaver.io~1freeze-request-cpu-millicores"),
        Some(&json!("100"))
    );
}

fn assert_runtime_security_and_tmp(document: &serde_json::Value) {
    assert_eq!(
        document
            .pointer("/spec/template/spec/containers/0/securityContext/readOnlyRootFilesystem",),
        Some(&json!(false))
    );
    assert_eq!(
        document
            .pointer("/spec/template/spec/containers/0/securityContext/allowPrivilegeEscalation",),
        Some(&json!(false))
    );
    assert_eq!(
        document.pointer("/spec/template/spec/containers/0/securityContext/capabilities/drop/0"),
        Some(&json!("ALL"))
    );
    assert_eq!(
        document.pointer("/spec/template/spec/containers/0/volumeMounts/1/mountPath"),
        Some(&json!("/tmp"))
    );
    assert_eq!(
        document.pointer("/spec/template/spec/volumes/1/emptyDir/sizeLimit"),
        Some(&json!("64Mi"))
    );
}

#[test]
fn deny_all_container_network_keeps_ingress_and_egress_isolation() {
    let projection = projection();
    let instance = instance_for(&projection);
    let provider = provider(projection.clone(), Arc::new(FixtureBackend::default()));
    let plan = provider
        .plan(&instance, &resolved(projection), ReconcileAction::Provision)
        .expect("deny_all plan");

    assert_eq!(
        resource(&plan, "NetworkPolicy").name,
        "default-deny-ingress"
    );
    assert!(plan.resources.iter().any(|resource| {
        resource.kind == "NetworkPolicy" && resource.name == "deny-all-egress"
    }));
}

#[test]
fn allow_all_container_network_leaves_egress_unisolated() {
    let mut projection = projection();
    projection.environment_spec.network = NetworkPolicySpec::AllowAll;
    projection
        .validate()
        .expect("allow_all container projection");

    let instance = instance_for(&projection);
    let provider = provider(projection.clone(), Arc::new(FixtureBackend::default()));
    let plan = provider
        .plan(&instance, &resolved(projection), ReconcileAction::Provision)
        .expect("allow_all plan");

    assert!(plan.resources.iter().all(|resource| {
        resource.kind != "NetworkPolicy"
            || !matches!(
                resource.name.as_str(),
                "deny-all-egress" | "restricted-egress"
            )
    }));
    let ingress = plan
        .resources
        .iter()
        .find(|resource| {
            resource.kind == "NetworkPolicy" && resource.name == "default-deny-ingress"
        })
        .expect("ingress isolation remains");
    assert_eq!(
        ingress.document.pointer("/spec/policyTypes"),
        Some(&json!(["Ingress"]))
    );
}

#[test]
fn non_http_container_entry_cannot_be_projected_as_a_gateway_endpoint() {
    let mut projection = projection();
    projection.environment_spec.entries[0].protocol = EndpointProtocol::Ssh;
    projection.validate().expect("internally valid projection");
    let instance = instance_for(&projection);
    let provider = provider(projection.clone(), Arc::new(FixtureBackend::default()));

    assert!(matches!(
        provider.plan(&instance, &resolved(projection), ReconcileAction::Provision),
        Err(ReleaseProjectionError::SecurityPostureInvalid)
    ));
}

#[test]
fn release_from_a_different_harbor_prefix_is_rejected() {
    let mut projection = projection();
    let ImageArtifact::Container { repository, .. } = &mut projection.release.artifact else {
        panic!("fixture must use a container artifact");
    };
    *repository = repository.replacen("labweaver-system", "unreviewed-project", 1);
    projection.validate().expect("internally valid projection");
    let instance = instance_for(&projection);
    let provider = provider(projection.clone(), Arc::new(FixtureBackend::default()));

    assert!(matches!(
        provider.plan(&instance, &resolved(projection), ReconcileAction::Provision),
        Err(ReleaseProjectionError::IdentityMismatch)
    ));
}

#[tokio::test]
async fn provision_returns_one_stable_healthy_endpoint() {
    let projection = projection();
    let mut instance = instance_for(&projection);
    instance.observed_state = ObservedEnvironmentState::Provisioning;
    let backend = Arc::new(FixtureBackend::default());
    let provider = provider(projection, backend.clone());

    let first = provider
        .execute(ReconcileAction::Provision, &instance)
        .await
        .expect("provision succeeds")
        .completed()
        .expect("provider completed");
    let second = provider
        .execute(ReconcileAction::Observe, &instance)
        .await
        .expect("observe succeeds")
        .completed()
        .expect("provider completed");

    assert_eq!(first.next_state, ObservedEnvironmentState::Ready);
    assert!(first.operation_complete);
    assert_eq!(first.endpoints.len(), 1);
    assert_eq!(first.endpoints[0].protocol, EndpointProtocol::Http);
    assert_eq!(first.endpoints[0].id, second.endpoints[0].id);
    assert_eq!(
        backend
            .operations
            .lock()
            .expect("operations lock")
            .as_slice(),
        ["apply", "observe"]
    );
    let fences = backend.fences.lock().expect("fences lock");
    assert_eq!(fences.len(), 2);
    assert!(fences.iter().all(|fence| {
        fence.protocol_version == CONTAINER_BACKEND_PROTOCOL_VERSION
            && fence.environment_id == instance.id
            && fence.operation_id == instance.operation.id
            && fence.trace_id == instance.operation.trace_id
            && fence.provider_step == instance.operation.provider_step
            && fence.operation_generation == instance.generation
            && fence.attempt == instance.operation.attempt
            && fence.deadline_at == instance.operation.deadline_at
    }));
    assert_ne!(fences[0].request_id, fences[1].request_id);
}

#[tokio::test]
async fn restart_reuses_operation_acceptance_revision_across_observations() {
    let projection = projection();
    let backend = Arc::new(FixtureBackend::default());
    let provider = provider(projection.clone(), backend.clone());
    let mut instance = instance_for(&projection);
    instance.observed_state = ObservedEnvironmentState::Provisioning;
    instance.operation.kind = EnvironmentOperationKind::Restart;
    instance.operation.accepted_revision = revision(7);
    instance.revision = revision(12);

    provider
        .execute(ReconcileAction::Restart, &instance)
        .await
        .expect("first restart reconciliation succeeds")
        .completed()
        .expect("provider completed");

    instance.revision = revision(13);
    provider
        .execute(ReconcileAction::Restart, &instance)
        .await
        .expect("retrying the same restart succeeds")
        .completed()
        .expect("provider completed");

    instance.operation.id = OperationId::new();
    instance.operation.accepted_revision = revision(8);
    instance.revision = revision(14);
    provider
        .execute(ReconcileAction::Restart, &instance)
        .await
        .expect("a distinct restart succeeds")
        .completed()
        .expect("provider completed");

    assert_eq!(
        backend
            .restart_revisions
            .lock()
            .expect("restart revisions lock")
            .as_slice(),
        [revision(7), revision(7), revision(8)]
    );
}

#[tokio::test]
async fn cleanup_deletes_the_namespace_and_requires_evidence() {
    let projection = projection();
    let mut instance = instance_for(&projection);
    instance.observed_state = ObservedEnvironmentState::Stopped;
    instance.desired_state = DesiredEnvironmentState::Deleted;
    instance.operation.kind = EnvironmentOperationKind::Expire;
    let backend = Arc::new(FixtureBackend::default());
    let provider = provider(projection, backend.clone());

    let checkpoint = provider
        .execute(ReconcileAction::Cleanup, &instance)
        .await
        .expect("cleanup enters deleting state")
        .completed()
        .expect("provider completed");
    assert_eq!(checkpoint.next_state, ObservedEnvironmentState::Deleting);
    assert!(!checkpoint.operation_complete);
    assert!(checkpoint.cleanup_evidence.is_none());

    instance.observed_state = ObservedEnvironmentState::Deleting;
    let observation = provider
        .execute(ReconcileAction::Cleanup, &instance)
        .await
        .expect("cleanup succeeds")
        .completed()
        .expect("provider completed");

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
        ["delete"]
    );
}

#[test]
fn withdrawn_expired_or_rotated_release_is_rejected_before_apply() {
    let projection = projection();
    let instance = instance_for(&projection);
    let backend = Arc::new(FixtureBackend::default());
    let provider = provider(projection.clone(), backend.clone());

    let mut withdrawn = resolved(projection.clone());
    withdrawn.withdrawn_at = Some(timestamp("2026-07-16T08:20:00.000Z"));
    assert!(matches!(
        provider.plan(&instance, &withdrawn, ReconcileAction::Provision),
        Err(ReleaseProjectionError::Withdrawn)
    ));

    let rotated = provider_with_state(
        projection.clone(),
        backend,
        timestamp("2026-07-16T08:30:00.000Z"),
        None,
        PolicyId::new(),
        revision(2),
        revision(2),
    );
    assert!(matches!(
        rotated.plan(&instance, &resolved(projection), ReconcileAction::Provision),
        Err(ReleaseProjectionError::TrustRevisionMismatch)
    ));
}

#[test]
fn course_approval_policy_revision_is_independent_from_image_policy_identity() {
    let projection = projection();
    let instance = instance_for(&projection);
    let provider = provider(projection.clone(), Arc::new(FixtureBackend::default()));

    provider
        .plan(&instance, &resolved(projection), ReconcileAction::Provision)
        .expect("the unrelated course approval policy revision is not an image policy identity");
}

#[tokio::test]
async fn withdrawal_blocks_new_use_but_still_allows_stop() {
    let projection = projection();
    let mut instance = instance_for(&projection);
    instance.observed_state = ObservedEnvironmentState::Stopping;
    instance.desired_state = DesiredEnvironmentState::Stopped;
    let backend = Arc::new(FixtureBackend::default());
    let provider = provider_with_state(
        projection.clone(),
        backend.clone(),
        timestamp("2026-07-16T10:00:00.000Z"),
        Some(timestamp("2026-07-16T09:30:00.000Z")),
        PolicyId::new(),
        revision(2),
        projection
            .release
            .trust_revision()
            .expect("approved release"),
    );

    instance.release_id = ReleaseId::new(); // Cleanup must not need a resolvable release.
    let observation = provider
        .execute(ReconcileAction::Stop, &instance)
        .await
        .expect("withdrawal must not prevent fail-closed stop")
        .completed()
        .expect("provider completed");
    assert_eq!(observation.next_state, ObservedEnvironmentState::Stopped);
    assert_eq!(
        backend
            .operations
            .lock()
            .expect("operations lock")
            .as_slice(),
        ["scale:0"]
    );
}

#[tokio::test]
async fn expire_stop_returns_a_non_terminal_checkpoint_for_cleanup() {
    let projection = projection();
    let mut instance = instance_for(&projection);
    instance.observed_state = ObservedEnvironmentState::Expiring;
    instance.desired_state = DesiredEnvironmentState::Deleted;
    instance.operation.kind = EnvironmentOperationKind::Expire;
    instance.operation.access_revocation_revision = Some(instance.revision);
    instance.release_id = ReleaseId::new();
    let backend = Arc::new(FixtureBackend::default());
    let provider = provider(projection, backend.clone());

    let observation = provider
        .execute(ReconcileAction::Stop, &instance)
        .await
        .expect("expire stop succeeds")
        .completed()
        .expect("provider completed");

    assert_eq!(observation.next_state, ObservedEnvironmentState::Deleting);
    let deleting = environment_service::apply_provider_observation(
        &instance,
        instance.operation.id,
        observation.clone(),
    )
    .expect("expire advances through the real deletion state");
    assert_eq!(deleting.observed_state, ObservedEnvironmentState::Deleting);
    assert!(!observation.operation_complete);
    assert!(observation.endpoints.is_empty());
    assert_eq!(
        backend
            .operations
            .lock()
            .expect("operations lock")
            .as_slice(),
        [] as [&str; 0]
    );
}

fn provider(
    projection: ReleasePublished,
    backend: Arc<FixtureBackend>,
) -> ContainerProvider<FixtureBackend, FixtureResolver> {
    let trust_revision = projection
        .release
        .trust_revision()
        .expect("approved release");
    provider_with_state(
        projection,
        backend,
        timestamp("2026-07-16T08:30:00.000Z"),
        None,
        PolicyId::new(),
        revision(2),
        trust_revision,
    )
}

fn provider_with_access_mode(
    projection: ReleasePublished,
    backend: Arc<FixtureBackend>,
    workspace_access_mode: &str,
) -> ContainerProvider<FixtureBackend, FixtureResolver> {
    let trust_revision = projection
        .release
        .trust_revision()
        .expect("approved release");
    provider_with_state_and_access_mode(
        projection,
        backend,
        timestamp("2026-07-16T08:30:00.000Z"),
        None,
        PolicyId::new(),
        revision(2),
        trust_revision,
        workspace_access_mode,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "negative tests vary each trust authority independently"
)]
fn provider_with_state(
    projection: ReleasePublished,
    backend: Arc<FixtureBackend>,
    authority_now: UtcTimestamp,
    withdrawn_at: Option<UtcTimestamp>,
    image_policy_id: PolicyId,
    image_policy_revision: Revision,
    trust_revision: Revision,
) -> ContainerProvider<FixtureBackend, FixtureResolver> {
    provider_with_state_and_access_mode(
        projection,
        backend,
        authority_now,
        withdrawn_at,
        image_policy_id,
        image_policy_revision,
        trust_revision,
        "ReadWriteMany",
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "negative tests vary each trust authority independently"
)]
fn provider_with_state_and_access_mode(
    projection: ReleasePublished,
    backend: Arc<FixtureBackend>,
    authority_now: UtcTimestamp,
    withdrawn_at: Option<UtcTimestamp>,
    image_policy_id: PolicyId,
    image_policy_revision: Revision,
    trust_revision: Revision,
    workspace_access_mode: &str,
) -> ContainerProvider<FixtureBackend, FixtureResolver> {
    ContainerProvider::new(
        "container-primary-v1".to_owned(),
        backend,
        Arc::new(FixtureResolver {
            projection,
            authority_now,
            withdrawn_at,
        }),
        ContainerProviderConfiguration::new(
            ContainerReleasePolicy::new(image_policy_id, image_policy_revision, trust_revision)
                .expect("release policy"),
            "harbor.internal/labweaver-system".to_owned(),
            "labweaver-platform".to_owned(),
            "access-service".to_owned(),
            "harbor-course-pull".to_owned(),
            "nfs-rwx".to_owned(),
            workspace_access_mode,
        )
        .expect("container configuration"),
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
    plan: &'a ContainerResourcePlan,
    kind: &str,
) -> &'a environment_service::ContainerResource {
    plan.resources
        .iter()
        .find(|resource| resource.kind == kind)
        .unwrap_or_else(|| panic!("missing {kind}"))
}

fn instance_for(projection: &ReleasePublished) -> contracts::environment::EnvironmentInstance {
    let mut instance = support::requested_instance();
    instance.project_id = projection.release.project_id;
    instance.course_id = projection.release.course_id;
    instance.release_id = projection.release.id;
    instance.release_version = projection.release.version;
    "container-primary-v1".clone_into(&mut instance.provider_binding);
    instance.runtime_authorization = Some(support::runtime_receipt(&instance, projection));
    instance
}

fn configure_work(
    instance: &mut contracts::environment::EnvironmentInstance,
    projection: &ReleasePublished,
) {
    instance.class = contracts::authoring::EnvironmentClass::Work;
    instance.eligibility_expires_at = Some(timestamp("2027-07-15T00:00:00.000Z"));
    let lease_id = contracts::LeaseId::new();
    instance.lease_id = Some(lease_id);
    instance.capacity_binding = Some("synthetic-capacity".to_owned());
    instance.operation.lease_authorization =
        Some(contracts::environment::EnvironmentLeaseAuthorization {
            resource_request_id: contracts::ResourceRequestId::new(),
            lease_id,
            lease_revision: revision(1),
            environment_id: instance.id,
            project_id: instance.project_id,
            course_id: instance.course_id,
            owner_actor_id: instance.owner_id,
            capacity_binding: "synthetic-capacity".to_owned(),
            approved_resources: instance.approved_resources.clone(),
            gpu_allocation: None,
            active_from: timestamp("2026-07-14T00:00:00.000Z"),
            expires_at: timestamp("2027-07-15T00:00:00.000Z"),
        });
    instance.runtime_authorization = Some(support::runtime_receipt(instance, projection));
}

#[derive(Clone, Copy)]
struct UnusedWorkAdmission;
#[async_trait]
impl environment_service::WorkAdmissionResolver for UnusedWorkAdmission {
    async fn resolve(
        &self,
        _id: contracts::AgentRunId,
        _query: &contracts::http::WorkConfigurationAdmissionQuery,
        _now: UtcTimestamp,
    ) -> Result<
        contracts::http::WorkConfigurationAdmissionBinding,
        environment_service::WorkAdmissionClientError,
    > {
        Err(environment_service::WorkAdmissionClientError::Configuration)
    }
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one synthetic database follows Work creation through freeze and endpoint admission with the same identity"
)]
async fn existing_work_freeze_and_http_target_follow_pinned_placement_and_keep_identity_fences()
-> Result<(), Box<dyn std::error::Error>> {
    use contracts::events::{CloudEvent, EVENT_CONTRACTS, SPEC_VERSION, subjects};
    use environment_service::{
        FreezeBindingConfiguration, FreezeBindingService, OwnerResolver, PgEnvironmentStore,
        PgReleaseProjectionStore,
    };
    use testcontainers::{ImageExt, runners::AsyncRunner};
    let pg = testcontainers_modules::postgres::Postgres::default()
        .with_tag("17.5-alpine")
        .start()
        .await?;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(3)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            pg.get_host_port_ipv4(5432).await?
        ))
        .await?;
    support::apply_environment_migrations(&pool).await?;
    let mut projection = projection();
    projection.environment_spec.class = contracts::authoring::EnvironmentClass::Work;
    let mut instance = instance_for(&projection);
    configure_work(&mut instance, &projection);
    let event_contract = EVENT_CONTRACTS
        .iter()
        .copied()
        .find(|c| c.subject == subjects::ENVIRONMENT_TEMPLATE_RELEASE_PUBLISHED)
        .ok_or("event contract")?;
    let releases = PgReleaseProjectionStore::new(pool.clone());
    releases
        .accept(
            "synthetic-release",
            &CloudEvent {
                specversion: SPEC_VERSION.to_owned(),
                id: contracts::EventId::new(),
                source: event_contract.source().to_owned(),
                event_type: event_contract.event_type.to_owned(),
                subject: event_contract.subject.to_owned(),
                time: projection.release.published_at,
                datacontenttype: "application/json".to_owned(),
                dataschema: event_contract.data_schema(),
                project_id: instance.project_id,
                course_id: instance.course_id,
                aggregate_revision: revision(1),
                aggregate_sequence: contracts::Sequence(1),
                trace_id: "synthetic-placement-freeze".to_owned(),
                data: projection.clone(),
            },
        )
        .await?;
    let store = PgEnvironmentStore::new(pool.clone());
    store.create("synthetic-work-create", &instance).await?;
    let profile = environment_service::ContainerNamespaceProfile::Existing {
        namespace: "synthetic-work-tests".to_owned(),
        namespace_uid: uuid::Uuid::from_u128(207),
        project_id: instance.project_id,
    };
    let provider = provider(projection.clone(), Arc::new(FixtureBackend::default()))
        .with_namespace_profile(profile)?;
    let plan = provider.plan(&instance, &resolved(projection), ReconcileAction::Provision)?;
    let mut transaction = pool.begin().await?;
    plan.placement().pin(&mut transaction).await?;
    transaction.commit().await?;
    for (index, action) in [
        ReconcileAction::Validate,
        ReconcileAction::Validate,
        ReconcileAction::Build,
        ReconcileAction::Provision,
    ]
    .into_iter()
    .enumerate()
    {
        let lease = store
            .claim_due(
                &format!("synthetic-worker-{index}"),
                std::time::Duration::from_secs(30),
            )
            .await?
            .ok_or("reconcile claim")?;
        let environment_service::ProviderOutcome::Completed(observation) = provider
            .execute(action, &lease.instance)
            .await
            .map_err(|e| format!("{e:?}"))?
        else {
            return Err("pending fixture".into());
        };
        let updated = environment_service::apply_provider_observation(
            &lease.instance,
            lease.instance.operation.id,
            observation,
        )?;
        store.save_reconciled(&lease, &updated).await?;
    }
    let ready = store.load(instance.id).await?;
    assert_eq!(ready.observed_state, ObservedEnvironmentState::Ready);
    assert_eq!(ready.observed_generation, ready.generation);
    let freeze = FreezeBindingService::new_with_admission_resolver(
        pool.clone(),
        releases.clone(),
        FreezeBindingConfiguration {
            container_workspace_storage_class: "synthetic-storage".to_owned(),
            vm: None,
        },
        UnusedWorkAdmission,
    )?;
    let request = contracts::submission::EnvironmentFreezeBindingRequest {
        project_id: ready.project_id,
        course_id: ready.course_id,
        actor_id: ready.owner_id,
        expected_revision: ready.revision,
        collector_public_key_openssh: None,
    };
    let binding = freeze.resolve(instance.id, &request).await?;
    let contracts::submission::EnvironmentFreezeSourceBinding::Container {
        namespace,
        persistent_volume_claim,
        ..
    } = binding.source
    else {
        return Err("container source".into());
    };
    assert_eq!(namespace, plan.namespace);
    assert_eq!(
        persistent_volume_claim,
        plan.placement().resource_name("workspace")
    );
    let mut foreign = request.clone();
    foreign.project_id = contracts::ProjectId::new();
    assert!(freeze.resolve(instance.id, &foreign).await.is_err());
    foreign = request.clone();
    foreign.actor_id = contracts::ActorId::new();
    assert!(freeze.resolve(instance.id, &foreign).await.is_err());
    foreign = request.clone();
    foreign.expected_revision = instance.revision;
    assert!(freeze.resolve(instance.id, &foreign).await.is_err());
    let now = time::OffsetDateTime::now_utc();
    let caller = auth::ServiceIdentity {
        issuer: "https://synthetic.example".to_owned(),
        subject: "access".to_owned(),
        client_id: "access".to_owned(),
        expires_at: now + time::Duration::minutes(1),
        permissions: ["environment.endpoint.resolve".to_owned()]
            .into_iter()
            .collect(),
    };
    let resolver = OwnerResolver::new(store, releases);
    let eligibility_request = contracts::environment::EnvironmentEndpointEligibilityRequest {
        environment_id: ready.id,
        project_id: ready.project_id,
        course_id: ready.course_id,
        actor_id: ready.owner_id,
        subject_kind: contracts::environment::EnvironmentAccessSubjectKind::Owner,
        expected_revision: ready.revision,
        endpoint_ids: ready.endpoints.iter().map(|e| e.id).collect(),
    };
    let target = resolver
        .resolve_endpoint_eligibility(&caller, &eligibility_request)
        .await?
        .runtime_target
        .ok_or("runtime target")?;
    assert_eq!(target.namespace, plan.namespace);
    assert_eq!(
        target.service_name,
        plan.placement().resource_name("runtime")
    );
    let mut stale = ready.clone();
    stale.observed_generation -= 1;
    assert!(
        environment_service::authorize_endpoint_eligibility(
            &stale,
            &eligibility_request,
            UtcTimestamp::from_utc(now.replace_nanosecond(0)?)?
        )
        .is_err()
    );
    stale = ready;
    stale
        .operation
        .lease_authorization
        .as_mut()
        .ok_or("lease")?
        .expires_at = timestamp("2026-07-14T00:01:00.000Z");
    assert!(
        environment_service::authorize_endpoint_eligibility(
            &stale,
            &eligibility_request,
            UtcTimestamp::from_utc(now.replace_nanosecond(0)?)?
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn existing_namespace_plan_scopes_work_objects_and_preserves_operator_policies() {
    let mut projection = projection();
    projection.environment_spec.class = contracts::authoring::EnvironmentClass::Work;
    let mut instance = instance_for(&projection);
    configure_work(&mut instance, &projection);
    let profile = environment_service::ContainerNamespaceProfile::Existing {
        namespace: "synthetic-work-tests".to_owned(),
        namespace_uid: uuid::Uuid::from_u128(207),
        project_id: instance.project_id,
    };
    let provider = provider(projection.clone(), Arc::new(FixtureBackend::default()))
        .with_namespace_profile(profile)
        .expect("profile valid");
    let plan = provider
        .plan(
            &instance,
            &resolved(projection.clone()),
            ReconcileAction::Provision,
        )
        .expect("Work plan");
    assert_eq!(plan.namespace, "synthetic-work-tests");
    assert_eq!(plan.namespace_uid, Some(uuid::Uuid::from_u128(207)));
    assert_eq!(plan.resources.len(), 4);
    for object in &plan.resources {
        assert!(matches!(
            object.kind.as_str(),
            "Deployment" | "Service" | "ServiceAccount" | "PersistentVolumeClaim"
        ));
        assert!(object.name.starts_with(&format!("lw-{}-", instance.id)));
        assert_eq!(object.namespace.as_deref(), Some("synthetic-work-tests"));
    }
    let placement = plan.placement();
    let deployment = resource(&plan, "Deployment");
    assert_eq!(
        deployment
            .document
            .pointer("/spec/template/spec/serviceAccountName"),
        Some(&json!(placement.resource_name("runtime")))
    );
    assert_eq!(
        deployment
            .document
            .pointer("/spec/template/spec/volumes/0/persistentVolumeClaim/claimName"),
        Some(&json!(placement.resource_name("workspace")))
    );
    assert_eq!(
        deployment
            .document
            .pointer("/spec/selector/matchLabels/app"),
        Some(&json!(placement.resource_name("runtime")))
    );
    assert_eq!(
        resource(&plan, "ServiceAccount")
            .document
            .pointer("/imagePullSecrets/0/name"),
        Some(&json!(placement.resource_name("registry-pull")))
    );
    let mut second = instance.clone();
    second.id = contracts::EnvironmentId::new();
    configure_work(&mut second, &projection);
    let other = provider
        .plan(&second, &resolved(projection), ReconcileAction::Provision)
        .expect("second Work");
    assert!(plan.resources.iter().all(|a| {
        other
            .resources
            .iter()
            .all(|b| a.kind != b.kind || a.name != b.name)
    }));
}

#[test]
fn existing_namespace_profile_rejects_wrong_project_experiment_and_network_modes() {
    let mut projection = projection();
    let mut instance = instance_for(&projection);
    let profile = environment_service::ContainerNamespaceProfile::Existing {
        namespace: "synthetic-work-tests".to_owned(),
        namespace_uid: uuid::Uuid::from_u128(207),
        project_id: instance.project_id,
    };
    let provider = provider(projection.clone(), Arc::new(FixtureBackend::default()))
        .with_namespace_profile(profile)
        .expect("profile");
    assert!(
        provider
            .plan(
                &instance,
                &resolved(projection.clone()),
                ReconcileAction::Provision
            )
            .is_err()
    );
    configure_work(&mut instance, &projection);
    projection.environment_spec.class = contracts::authoring::EnvironmentClass::Work;
    projection.environment_spec.network = NetworkPolicySpec::AllowAll;
    assert!(
        provider
            .plan(
                &instance,
                &resolved(projection.clone()),
                ReconcileAction::Provision
            )
            .is_err()
    );
    projection.environment_spec.network = NetworkPolicySpec::DenyAll;
    instance.project_id = contracts::ProjectId::new();
    assert!(
        provider
            .plan(&instance, &resolved(projection), ReconcileAction::Provision)
            .is_err()
    );
}

#[test]
fn experiment_gpu_allocation_renders_workload_limits_and_request_quota_for_modes() {
    for (mode, allocation_binding) in [
        (
            contracts::resource::GpuAllocationMode::Exclusive,
            "nvidia.com/gpu",
        ),
        (
            contracts::resource::GpuAllocationMode::ContainerTimeSlice,
            "nvidia.com/gpu.shared",
        ),
    ] {
        let projection = gpu_projection();
        let mut instance = instance_for(&projection);
        let mut allocation = gpu_allocation("a100-exclusive", 1);
        allocation.mode = mode;
        allocation.allocation_binding = allocation_binding.to_owned();
        instance.gpu_allocation = Some(allocation);
        let provider = provider(projection.clone(), Arc::new(FixtureBackend::default()));

        let plan = provider
            .plan(&instance, &resolved(projection), ReconcileAction::Provision)
            .expect("resolved Experiment GPU allocation is rendered");
        let deployment = resource(&plan, "Deployment");
        let escaped_binding = allocation_binding.replace('~', "~0").replace('/', "~1");
        assert_eq!(
            deployment.document.pointer(&format!(
                "/spec/template/spec/containers/0/resources/requests/{escaped_binding}"
            )),
            Some(&json!("1"))
        );
        assert_eq!(
            deployment.document.pointer(&format!(
                "/spec/template/spec/containers/0/resources/limits/{escaped_binding}"
            )),
            Some(&json!("1"))
        );
        let quota = resource(&plan, "ResourceQuota");
        assert_eq!(
            quota
                .document
                .pointer(&format!("/spec/hard/requests.{escaped_binding}")),
            Some(&json!("1"))
        );
        assert_eq!(
            quota
                .document
                .pointer(&format!("/spec/hard/limits.{escaped_binding}")),
            None
        );
    }
}

#[test]
fn experiment_gpu_release_without_a_durable_allocation_fails_closed() {
    let projection = gpu_projection();
    let instance = instance_for(&projection);
    assert!(instance.gpu_allocation.is_none());
    let provider = provider(projection.clone(), Arc::new(FixtureBackend::default()));

    assert!(matches!(
        provider.plan(&instance, &resolved(projection), ReconcileAction::Provision),
        Err(ReleaseProjectionError::SecurityPostureInvalid)
    ));
}

#[test]
fn experiment_gpu_allocation_must_match_the_declared_class_and_count() {
    let projection = gpu_projection();
    let mut instance = instance_for(&projection);
    instance.gpu_allocation = Some(gpu_allocation("a100-exclusive", 2));
    let provider = provider(projection.clone(), Arc::new(FixtureBackend::default()));

    assert!(matches!(
        provider.plan(&instance, &resolved(projection), ReconcileAction::Provision),
        Err(ReleaseProjectionError::SecurityPostureInvalid)
    ));
}

fn gpu_projection() -> ReleasePublished {
    let mut projection = projection();
    projection.environment_spec.resources.gpu = Some(contracts::resource::GpuRequest {
        class: "a100-exclusive".to_owned(),
        count: 1,
    });
    projection.validate().expect("GPU experiment projection");
    projection
}

fn gpu_allocation(class: &str, count: u32) -> contracts::resource::GpuAllocation {
    contracts::resource::GpuAllocation {
        entry_id: contracts::GpuCatalogEntryId::new(),
        class: class.to_owned(),
        count,
        mode: contracts::resource::GpuAllocationMode::Exclusive,
        provider_binding: "container-primary-v1".to_owned(),
        allocation_binding: "nvidia.com/gpu".to_owned(),
        catalog_revision: revision(1),
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the fixture deliberately constructs the complete immutable release identity"
)]
fn projection() -> ReleasePublished {
    let environment_spec: EnvironmentSpec = serde_json::from_value(json!({
        "apiVersion":"environment.labweaver.io/v1",
        "kind":"EnvironmentSpec",
        "name":"container-lab",
        "class":"experiment",
        "resources":{"cpuMillicores":1000,"memoryBytes":1_073_741_824_u64,"storageBytes":1_073_741_824_u64},
        "network":{"mode":"deny_all"},
        "entries":[{"name":"web","protocol":"http","servicePort":8080}],
        "security":{
            "runAsUser":null,
            "readOnlyRootFilesystem":false,
            "allowPrivilegeEscalation":false,
            "publicExposurePolicy":"deny",
            "securityProfileBinding":"restricted-v1"
        },
        "runtime":{
            "kind":"container",
            "provider_binding":"container-primary-v1",
            "build_context":artifact_ref("application/vnd.oci.image.layer.v1.tar+gzip"),
            "service_port":8080
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
    let artifact_sha256 = Sha256Digest::of_bytes(b"container-image");
    let release = EnvironmentTemplateRelease {
        id: ReleaseId::new(),
        project_id,
        course_id,
        version: 1,
        candidate_id: Some(candidate_id),
        agent_run_id: Some(contracts::AgentRunId::new()),
        candidate_revision: Some(revision(1)),
        runtime_kind: RuntimeKind::Container,
        approval: Some(CandidateApproval {
            id: ApprovalId::new(),
            candidate_id,
            candidate_revision: revision(1),
            policy_revision: revision(7),
            trust_revision: revision(1),
            actor_id: ActorId::new(),
            decision: CandidateDecision::Approved,
            reason: "reviewed".to_owned(),
            decided_at: published_at,
        }),
        manual_approval: None,
        artifact: ImageArtifact::Container {
            id: artifact_id,
            build_request_id: BuildRequestId::new(),
            repository: match course_id {
                Some(course_id) => {
                    format!("harbor.internal/labweaver-system/course-{course_id}-{candidate_id}")
                }
                None => {
                    format!("harbor.internal/labweaver-system/project-{project_id}-{candidate_id}")
                }
            },
            digest: format!("sha256:{artifact_sha256}"),
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

fn artifact_ref(media_type: &str) -> ArtifactRef {
    ArtifactRef {
        artifact_id: ArtifactId::new(),
        store_binding: "artifact-store-v1".to_owned(),
        object_version: "version-1".to_owned(),
        size_bytes: 128,
        media_type: media_type.to_owned(),
    }
}

fn ready() -> ContainerApplyObservation {
    ContainerApplyObservation {
        ready: true,
        observed_at: timestamp("2026-07-16T08:01:00.000Z"),
    }
}

fn revision(value: u64) -> Revision {
    Revision::new(value).expect("positive revision")
}

fn timestamp(value: &str) -> UtcTimestamp {
    UtcTimestamp::from_str(value).expect("valid timestamp")
}

#[test]
fn ordinary_container_projects_only_the_approved_uid_capabilities_and_root_filesystem() {
    use contracts::platform_configuration::LinuxCapability;
    for run_as_user in [None, Some(0), Some(1234)] {
        let mut projection = projection();
        projection.environment_spec.security.run_as_user = run_as_user;
        projection
            .environment_spec
            .security
            .capabilities
            .insert(LinuxCapability::NetAdmin);
        projection
            .environment_spec
            .security
            .read_only_root_filesystem = true;
        let mut instance = instance_for(&projection);
        instance
            .runtime_authorization
            .as_mut()
            .expect("receipt")
            .default_container_uid = 1000;
        let provider = provider(projection.clone(), Arc::new(FixtureBackend::default()));
        let plan = provider
            .plan(
                &instance,
                &resolved(projection.clone()),
                ReconcileAction::Provision,
            )
            .expect("approved capability plan");
        let document = &resource(&plan, "Deployment").document;
        let uid = run_as_user.unwrap_or(1000);
        let expected = json!({"runAsNonRoot":uid!=0,"runAsUser":uid,"privileged":false,
            "allowPrivilegeEscalation":false,"readOnlyRootFilesystem":true,
            "capabilities":{"drop":["ALL"],"add":["NET_ADMIN"]},"seccompProfile":{"type":"RuntimeDefault"}});
        assert_eq!(
            document.pointer("/spec/template/spec/containers/0/securityContext"),
            Some(&expected)
        );
        assert_eq!(
            document.pointer("/spec/template/spec/initContainers/0/securityContext"),
            Some(&expected)
        );
        assert_eq!(
            resource(&plan, "Namespace")
                .document
                .pointer("/metadata/labels/pod-security.kubernetes.io~1enforce"),
            Some(&json!("privileged"))
        );
        instance
            .runtime_authorization
            .as_mut()
            .expect("receipt")
            .authorization
            .permissions
            .capabilities
            .clear();
        assert!(
            provider
                .plan(&instance, &resolved(projection), ReconcileAction::Provision)
                .is_err(),
            "adding a capability cannot reuse the original approval"
        );
    }
}
