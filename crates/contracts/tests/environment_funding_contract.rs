//! Purpose comes from the same trusted immutable release used by the Environment owner.
#![allow(
    clippy::expect_used,
    clippy::too_many_lines,
    reason = "valid full release fixture and explicit adversarial assertions"
)]
use contracts::authoring::{
    CandidateApproval, CandidateDecision, EnvironmentClass, EnvironmentSpec, RuntimeKind,
};
use contracts::environment::ResourceEnvironmentHandoff;
use contracts::events::ReleasePublished;
use contracts::supply_chain::{EnvironmentTemplateRelease, ImageArtifact};
use contracts::{
    ActorId, ApprovalId, ArtifactId, ArtifactRef, BuildRequestId, CandidateId, ImageArtifactId,
    PolicyId, ReleaseId, Revision,
};
use serde_json::json;
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
            "runAsUser":65534,
            "rootFilesystemPolicy":"read_only_required",
            "privilegeEscalationPolicy":"deny",
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
    let published_at = "2026-07-16T08:00:00.000Z".parse().expect("timestamp");
    let artifact_sha256 = "0123456789abcdef".repeat(4);
    let release = EnvironmentTemplateRelease {
        id: ReleaseId::new(),
        project_id,
        course_id,
        version: 1,
        candidate_id: Some(candidate_id),
        agent_run_id: Some(contracts::AgentRunId::new()),
        candidate_revision: Some(Revision::new(1).expect("revision")),
        runtime_kind: RuntimeKind::Container,
        approval: Some(CandidateApproval {
            id: ApprovalId::new(),
            candidate_id,
            candidate_revision: Revision::new(1).expect("revision"),
            policy_revision: Revision::new(7).expect("revision"),
            trust_revision: Revision::new(1).expect("revision"),
            actor_id: ActorId::new(),
            decision: CandidateDecision::Approved,
            reason: "reviewed".to_owned(),
            decided_at: published_at,
        }),
        manual_approval: None,
        artifact: ImageArtifact::Container {
            id: artifact_id,
            build_request_id: BuildRequestId::new(),
            repository: format!(
                "harbor.internal/labweaver-system/project-{project_id}-{candidate_id}"
            ),
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

#[test]
fn original_release_purpose_does_not_depend_on_course_or_caller_kind()
-> Result<(), Box<dyn std::error::Error>> {
    let mut published = projection();
    let handoff: ResourceEnvironmentHandoff = serde_json::from_value(json!({
        "version":1,"requestId":contracts::ResourceRequestId::new(),"requestRevision":3,
        "leaseId":contracts::LeaseId::new(),"leaseRevision":2,"claimId":contracts::CapacityClaimId::new(),"claimRevision":3,
        "environmentId":contracts::EnvironmentId::new(),"projectId":published.release.project_id,"courseId":published.release.course_id,
        "ownerActorId":ActorId::new(),"displayLabel":"synthetic environment","releaseId":published.release.id,
        "releaseVersion":published.release.version,"providerBinding":"container-primary-v1","capacityBinding":"original-claim",
        "approvedResources":{"cpuMillicores":1000,"memoryBytes":1_073_741_824,"storageBytes":1_073_741_824},"traceId":"synthetic-purpose",
    }))?;
    assert_eq!(
        handoff.class_for_release(&published)?,
        EnvironmentClass::Experiment
    );
    let mut course_handoff = handoff.clone();
    course_handoff.project_id = contracts::ProjectId::new();
    assert_eq!(
        course_handoff.class_for_release(&published)?,
        EnvironmentClass::Experiment
    );
    let mut no_course = published.clone();
    no_course.release.course_id = None;
    course_handoff.course_id = None;
    assert!(course_handoff.class_for_release(&no_course).is_err());
    published.environment_spec.class = EnvironmentClass::Work;
    assert_eq!(
        handoff.class_for_release(&published)?,
        EnvironmentClass::Work
    );
    let mut foreign = published.clone();
    foreign.release.project_id = contracts::ProjectId::new();
    assert!(handoff.class_for_release(&foreign).is_err());
    foreign = published.clone();
    foreign.release.id = ReleaseId::new();
    assert!(handoff.class_for_release(&foreign).is_err());
    foreign = published.clone();
    foreign.release.version += 1;
    assert!(handoff.class_for_release(&foreign).is_err());
    foreign = published.clone();
    foreign.release.course_id = None;
    assert!(handoff.class_for_release(&foreign).is_err());
    let mut wire = serde_json::to_value(&handoff)?;
    wire["class"] = json!("work");
    assert!(serde_json::from_value::<ResourceEnvironmentHandoff>(wire).is_err());
    Ok(())
}
