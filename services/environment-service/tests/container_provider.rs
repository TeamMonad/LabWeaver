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
use contracts::environment::{DesiredEnvironmentState, EndpointProtocol, ObservedEnvironmentState};
use contracts::events::ReleasePublished;
use contracts::supply_chain::{
    EnvironmentTemplateRelease, ImageArtifact, ImagePolicyEvaluation, VulnerabilitySummary,
};
use contracts::{
    ActorId, ApprovalId, ArtifactId, ArtifactRef, BuildRequestId, CandidateId, ImageArtifactId,
    PolicyId, ReleaseId, Revision, Sha256Digest, UtcTimestamp,
};
use environment_service::{
    CONTAINER_BACKEND_PROTOCOL_VERSION, ContainerApplyObservation, ContainerBackendFence,
    ContainerProvider, ContainerProviderBackend, ContainerProviderConfiguration,
    ContainerReleasePolicy, ContainerReleaseResolver, ContainerResourcePlan, EnvironmentProvider,
    ProviderFailure, ReconcileAction, ReleaseProjectionError, ResolvedContainerRelease,
};
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
}

impl FixtureBackend {
    fn record(&self, operation: &str, fence: &ContainerBackendFence) {
        self.operations
            .lock()
            .expect("operations lock")
            .push(operation.to_owned());
        self.fences.lock().expect("fences lock").push(*fence);
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
        _operation_revision: Revision,
    ) -> Result<ContainerApplyObservation, ProviderFailure> {
        self.record("restart", fence);
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
            sha256: Sha256Digest::of_bytes(plan.namespace.as_bytes()),
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
        Some(&json!(true))
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
    let spec_sha256 = Sha256Digest::of_canonical(&projection.environment_spec).expect("spec hash");
    projection.release.environment_spec_sha256 = spec_sha256;
    projection.release.approval.candidate_sha256 = spec_sha256;
    projection.projection_sha256 = Sha256Digest::of_canonical(&json!({
        "release": &projection.release,
        "environmentSpec": &projection.environment_spec,
    }))
    .expect("projection hash");
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
    let spec_sha256 = Sha256Digest::of_canonical(&projection.environment_spec).expect("spec hash");
    projection.release.environment_spec_sha256 = spec_sha256;
    projection.release.approval.candidate_sha256 = spec_sha256;
    projection.projection_sha256 = Sha256Digest::of_canonical(&json!({
        "release": &projection.release,
        "environmentSpec": &projection.environment_spec,
    }))
    .expect("projection hash");
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
    projection.projection_sha256 = Sha256Digest::of_canonical(&json!({
        "release": &projection.release,
        "environmentSpec": &projection.environment_spec,
    }))
    .expect("projection hash");
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
        .expect("provision succeeds");
    let second = provider
        .execute(ReconcileAction::Observe, &instance)
        .await
        .expect("observe succeeds");

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
            && fence.provider_step == instance.operation.provider_step
            && fence.operation_generation == instance.generation
            && fence.attempt == instance.operation.attempt
            && fence.deadline_at == instance.operation.deadline_at
    }));
    assert_ne!(fences[0].request_id, fences[1].request_id);
}

#[tokio::test]
async fn cleanup_deletes_the_namespace_and_requires_evidence() {
    let projection = projection();
    let mut instance = instance_for(&projection);
    instance.observed_state = ObservedEnvironmentState::Deleting;
    instance.desired_state = DesiredEnvironmentState::Deleted;
    let backend = Arc::new(FixtureBackend::default());
    let provider = provider(projection, backend.clone());

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
        ["delete"]
    );
}

#[test]
fn withdrawn_expired_or_rotated_release_is_rejected_before_apply() {
    let projection = projection();
    let instance = instance_for(&projection);
    let backend = Arc::new(FixtureBackend::default());
    let provider = provider(projection.clone(), backend.clone());

    let mut expired = resolved(projection.clone());
    expired.authority_now = projection
        .release
        .image_policy_evaluation
        .as_ref()
        .expect("container release evidence")
        .valid_until;
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
    assert_ne!(
        projection.release.approval.policy_revision,
        projection
            .release
            .image_policy_evaluation
            .as_ref()
            .expect("container release evidence")
            .policy_revision
    );
    let instance = instance_for(&projection);
    let provider = provider(projection.clone(), Arc::new(FixtureBackend::default()));

    provider
        .plan(&instance, &resolved(projection), ReconcileAction::Provision)
        .expect("the unrelated course approval policy revision is not an image policy identity");
}

#[test]
fn same_revision_different_image_policy_id_is_rejected() {
    let projection = projection();
    let instance = instance_for(&projection);
    let provider = provider_with_state(
        projection.clone(),
        Arc::new(FixtureBackend::default()),
        timestamp("2026-07-16T08:30:00.000Z"),
        None,
        PolicyId::new(),
        projection
            .release
            .image_policy_evaluation
            .as_ref()
            .expect("container release evidence")
            .policy_revision,
        projection.release.approval.trust_revision,
    );

    assert!(matches!(
        provider.plan(&instance, &resolved(projection), ReconcileAction::Provision),
        Err(ReleaseProjectionError::TrustRevisionMismatch)
    ));
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
        projection
            .release
            .image_policy_evaluation
            .as_ref()
            .expect("container release evidence")
            .policy_id,
        projection
            .release
            .image_policy_evaluation
            .as_ref()
            .expect("container release evidence")
            .policy_revision,
        projection.release.approval.trust_revision,
    );

    let observation = provider
        .execute(ReconcileAction::Stop, &instance)
        .await
        .expect("withdrawal must not prevent fail-closed stop");
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

fn provider(
    projection: ReleasePublished,
    backend: Arc<FixtureBackend>,
) -> ContainerProvider<FixtureBackend, FixtureResolver> {
    let evaluation = projection
        .release
        .image_policy_evaluation
        .as_ref()
        .expect("container release evidence");
    let image_policy_id = evaluation.policy_id;
    let image_policy_revision = evaluation.policy_revision;
    let trust_revision = projection.release.approval.trust_revision;
    provider_with_state(
        projection,
        backend,
        timestamp("2026-07-16T08:30:00.000Z"),
        None,
        image_policy_id,
        image_policy_revision,
        trust_revision,
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
            "labweaver-sprint2".to_owned(),
            "access-service".to_owned(),
            "harbor-course-pull".to_owned(),
            "nfs-rwx".to_owned(),
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
    instance.course_id = projection.release.course_id;
    instance.release_id = projection.release.id;
    instance.release_version = projection.release.version;
    "container-primary-v1".clone_into(&mut instance.provider_binding);
    instance
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
            "userPolicy":"non_root_required",
            "rootFilesystemPolicy":"read_only_required",
            "privilegeEscalationPolicy":"deny",
            "publicExposurePolicy":"deny",
            "securityProfileBinding":"restricted-v1"
        },
        "runtime":{
            "kind":"container",
            "provider_binding":"container-primary-v1",
            "build_context":artifact_ref("application/vnd.oci.image.layer.v1.tar+gzip"),
            "base_image_digest":format!("sha256:{}", "b".repeat(64)),
            "service_port":8080
        },
        "retention":{
            "policyId":PolicyId::new(),"policyRevision":1,"class":"run_evidence",
            "retainUntil":"2026-08-16T08:00:00.000Z","disposition":"delete"
        }
    }))
    .expect("valid EnvironmentSpec");
    let environment_spec_sha256 = Sha256Digest::of_canonical(&environment_spec).expect("spec hash");
    let artifact_sha256 = Sha256Digest::of_bytes(b"container-image");
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
        runtime_kind: RuntimeKind::Container,
        approval: CandidateApproval {
            id: ApprovalId::new(),
            candidate_id,
            candidate_revision: revision(1),
            candidate_sha256: environment_spec_sha256,
            policy_revision: revision(7),
            schema_sha256: Sha256Digest::of_bytes(b"schema"),
            trust_revision: revision(1),
            actor_id: ActorId::new(),
            decision: CandidateDecision::Approved,
            reason: "reviewed".to_owned(),
            decided_at: published_at,
        },
        artifact: ImageArtifact::Container {
            id: artifact_id,
            build_request_id: BuildRequestId::new(),
            repository: format!(
                "harbor.internal/labweaver-system/course-{course_id}-{candidate_id}"
            ),
            digest: format!("sha256:{artifact_sha256}"),
        },
        image_policy_evaluation: Some(ImagePolicyEvaluation {
            artifact_id,
            artifact_sha256,
            policy_id: PolicyId::new(),
            policy_revision: revision(2),
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
            evaluated_at: published_at,
            max_evidence_age_milliseconds: 3_600_000,
            valid_until: timestamp("2026-07-16T09:00:00.000Z"),
            passed: true,
        }),
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
