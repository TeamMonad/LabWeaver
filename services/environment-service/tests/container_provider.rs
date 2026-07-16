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
use contracts::authoring::{CandidateApproval, CandidateDecision, EnvironmentSpec, RuntimeKind};
use contracts::environment::{DesiredEnvironmentState, EndpointProtocol, ObservedEnvironmentState};
use contracts::events::ReleasePublishedV2;
use contracts::supply_chain::{
    EnvironmentTemplateRelease, ImageArtifact, ImagePolicyEvaluation, SigstoreEvidence,
    VulnerabilitySummary,
};
use contracts::{
    ActorId, ApprovalId, ArtifactId, ArtifactRef, BuildRequestId, CandidateId, ImageArtifactId,
    PolicyId, ReleaseId, Revision, Sha256Digest, UtcTimestamp,
};
use environment_service::{
    ContainerApplyObservation, ContainerProvider, ContainerProviderBackend,
    ContainerReleaseResolver, ContainerResourcePlan, EnvironmentProvider, ProviderFailure,
    ReconcileAction, ReleaseProjectionError,
};
use serde_json::json;

#[derive(Clone)]
struct FixtureResolver {
    projection: ReleasePublishedV2,
}

#[async_trait]
impl ContainerReleaseResolver for FixtureResolver {
    async fn resolve(
        &self,
        release_id: ReleaseId,
        release_version: u64,
    ) -> Result<ReleasePublishedV2, ReleaseProjectionError> {
        if self.projection.release.id != release_id
            || self.projection.release.version != release_version
        {
            return Err(ReleaseProjectionError::NotFound);
        }
        Ok(self.projection.clone())
    }
}

#[derive(Default)]
struct FixtureBackend {
    operations: Mutex<Vec<String>>,
}

impl FixtureBackend {
    fn record(&self, operation: &str) {
        self.operations
            .lock()
            .expect("operations lock")
            .push(operation.to_owned());
    }
}

#[async_trait]
impl ContainerProviderBackend for FixtureBackend {
    async fn apply(
        &self,
        _plan: &ContainerResourcePlan,
    ) -> Result<ContainerApplyObservation, ProviderFailure> {
        self.record("apply");
        Ok(ready())
    }

    async fn observe(
        &self,
        _plan: &ContainerResourcePlan,
    ) -> Result<ContainerApplyObservation, ProviderFailure> {
        self.record("observe");
        Ok(ready())
    }

    async fn scale(
        &self,
        _plan: &ContainerResourcePlan,
        replicas: u32,
    ) -> Result<ContainerApplyObservation, ProviderFailure> {
        self.record(&format!("scale:{replicas}"));
        Ok(ready())
    }

    async fn restart(
        &self,
        _plan: &ContainerResourcePlan,
        _operation_revision: Revision,
    ) -> Result<ContainerApplyObservation, ProviderFailure> {
        self.record("restart");
        Ok(ready())
    }

    async fn delete_namespace(
        &self,
        plan: &ContainerResourcePlan,
    ) -> Result<ArtifactRef, ProviderFailure> {
        self.record("delete");
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
fn plan_uses_digest_only_image_and_only_the_protected_gateway() {
    let projection = projection();
    let instance = instance_for(&projection);
    let provider = provider(projection.clone(), Arc::new(FixtureBackend::default()));
    let first = provider.plan(&instance, &projection);
    let first = first.expect("plan is valid");
    let second = provider
        .plan(&instance, &projection)
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

    let deployment = resource(&first, "Deployment");
    assert_eq!(
        deployment
            .document
            .pointer("/spec/template/spec/containers/0/image"),
        Some(&json!(first.image))
    );
    assert_eq!(
        deployment
            .document
            .pointer("/spec/template/spec/containers/0/securityContext/readOnlyRootFilesystem"),
        Some(&json!(true))
    );
    assert_eq!(
        deployment
            .document
            .pointer("/spec/template/spec/containers/0/securityContext/allowPrivilegeEscalation"),
        Some(&json!(false))
    );
    assert_eq!(
        deployment
            .document
            .pointer("/spec/template/spec/containers/0/securityContext/capabilities/drop/0"),
        Some(&json!("ALL"))
    );
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
    assert!(
        first
            .resources
            .iter()
            .all(|resource| resource.kind != "Ingress")
    );

    let route = resource(&first, "HTTPRoute");
    assert_eq!(
        route
            .document
            .pointer("/metadata/annotations/labweaver.io~1access-controlled"),
        Some(&json!("true"))
    );
    assert_eq!(
        route.document.pointer("/spec/parentRefs/0/namespace"),
        Some(&json!("access-system"))
    );
    assert_eq!(
        route.document.pointer("/spec/parentRefs/0/name"),
        Some(&json!("protected-gateway"))
    );
    assert_eq!(
        route.document.pointer("/spec/rules/0/matches/0/path/value"),
        Some(&json!(format!("/environments/{}/", instance.id)))
    );
    let gateway_policy = first
        .resources
        .iter()
        .find(|resource| {
            resource.kind == "NetworkPolicy" && resource.name == "protected-gateway-ingress"
        })
        .expect("protected Gateway policy exists");
    assert_eq!(
        gateway_policy.document.pointer(
            "/spec/ingress/0/from/0/podSelector/matchLabels/gateway.networking.k8s.io~1gateway-name"
        ),
        Some(&json!("protected-gateway"))
    );
}

#[test]
fn non_https_container_entry_cannot_be_projected_as_a_gateway_endpoint() {
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
        provider.plan(&instance, &projection),
        Err(ReleaseProjectionError::SecurityPostureInvalid)
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
    assert_eq!(first.endpoints[0].id, second.endpoints[0].id);
    assert_eq!(
        backend
            .operations
            .lock()
            .expect("operations lock")
            .as_slice(),
        ["apply", "observe"]
    );
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

fn provider(
    projection: ReleasePublishedV2,
    backend: Arc<FixtureBackend>,
) -> ContainerProvider<FixtureBackend, FixtureResolver> {
    ContainerProvider::new(
        "container-primary-v1".to_owned(),
        backend,
        Arc::new(FixtureResolver { projection }),
        "access-system".to_owned(),
        "protected-gateway".to_owned(),
        "protected-https".to_owned(),
        "harbor-course-pull".to_owned(),
    )
    .expect("provider configuration")
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

fn instance_for(projection: &ReleasePublishedV2) -> contracts::environment::EnvironmentInstance {
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
fn projection() -> ReleasePublishedV2 {
    let environment_spec: EnvironmentSpec = serde_json::from_value(json!({
        "apiVersion":"environment.labweaver.io/v1",
        "kind":"EnvironmentSpec",
        "name":"container-lab",
        "class":"experiment",
        "resources":{"cpuMillicores":1000,"memoryBytes":1_073_741_824_u64,"storageBytes":1_073_741_824_u64},
        "network":{"mode":"deny_all"},
        "entries":[{"name":"web","protocol":"https","servicePort":8080}],
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
        runtime_kind: RuntimeKind::Container,
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
        artifact: ImageArtifact::Container {
            id: artifact_id,
            build_request_id: BuildRequestId::new(),
            repository: format!("harbor.internal/course-{course_id}/{candidate_id}"),
            immutable_tag: "build-aabbccdd".to_owned(),
            digest: format!("sha256:{artifact_sha256}"),
            sbom: artifact_ref("application/spdx+json"),
            provenance: artifact_ref("application/vnd.in-toto+json"),
            signature: SigstoreEvidence {
                trust_bundle_sha256,
                fulcio_issuer: "https://fulcio.internal".to_owned(),
                certificate_subject: "spiffe://labweaver/image-builder".to_owned(),
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
            expected_certificate_subject: "spiffe://labweaver/image-builder".to_owned(),
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
