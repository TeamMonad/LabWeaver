//! First-request evidence validation without inferring object permissions.
use contracts::http::ModelInputAuthorizationContext;
use contracts::{ActorId, ProjectId};
use serde_json::{Value, json};

fn context_json() -> Value {
    json!({
        "sessionSha256": "a".repeat(64),
        "actorId": ActorId::new(),
        "projectId": ProjectId::new(),
        "courseId": null,
        "operationId": "freezeSubmission",
        "validUntil": "2026-10-14T12:00:00.000Z"
    })
}

#[test]
fn first_request_context_round_trips_without_refreshing_decision_evidence()
-> Result<(), Box<dyn std::error::Error>> {
    let original = context_json();
    let context: ModelInputAuthorizationContext = serde_json::from_value(original.clone())?;
    // A past decision timestamp is evidence, not permission or a new task deadline.
    context.validate()?;
    assert_eq!(serde_json::to_value(context)?, original);
    Ok(())
}

#[test]
fn missing_context_or_caller_permission_fields_cannot_be_recovered()
-> Result<(), Box<dyn std::error::Error>> {
    for field in [
        "sessionSha256",
        "actorId",
        "projectId",
        "operationId",
        "validUntil",
    ] {
        let mut value = context_json();
        value.as_object_mut().ok_or("not an object")?.remove(field);
        assert!(serde_json::from_value::<ModelInputAuthorizationContext>(value).is_err());
    }
    for field in ["roles", "allowEgress", "visibility", "permission"] {
        let mut value = context_json();
        value[field] = json!(true);
        assert!(serde_json::from_value::<ModelInputAuthorizationContext>(value).is_err());
    }
    Ok(())
}

#[test]
fn malformed_session_and_nil_actor_are_rejected() -> Result<(), Box<dyn std::error::Error>> {
    for session in ["", "token", &"A".repeat(64)] {
        let mut value = context_json();
        value["sessionSha256"] = json!(session);
        let context: ModelInputAuthorizationContext = serde_json::from_value(value)?;
        assert!(context.validate().is_err());
    }
    let mut value = context_json();
    value["actorId"] = json!("00000000-0000-0000-0000-000000000000");
    assert!(serde_json::from_value::<ModelInputAuthorizationContext>(value).is_err());
    Ok(())
}

fn frozen_binding_json() -> Value {
    let mut binding = context_json();
    binding["policyId"] = json!(contracts::PolicyId::new());
    binding["policyRevision"] = json!(1);
    binding["policySha256"] = json!("e".repeat(64));
    binding["selectionSha256"] = json!("c".repeat(64));
    let archive = json!({"artifactId": contracts::ArtifactId::new(), "storeBinding":"synthetic", "objectVersion":"archive-v1", "sizeBytes": 50, "mediaType":"application/gzip"});
    binding["source"] = json!({
        "kind":"frozen_submission", "taskRunId":contracts::TaskRunId::new(),
        "frozenSubmissionId":contracts::FrozenSubmissionId::new(), "manifestRevision":3,
        "manifestSha256":"a".repeat(64), "archive":archive,
        "contentSha256":"b".repeat(64), "evaluationReleaseId":contracts::EvaluationReleaseId::new(),
        "evaluationReleaseRevision":9,
        "environment":{"environmentId":contracts::EnvironmentId::new(), "environmentRevision":2,
            "releaseId":contracts::ReleaseId::new(), "releaseVersion":4, "runtimeKind":"container", "buildRequestId":null},
        "rubric":{"packageId":contracts::ProblemPackageId::new(), "packageRevision":7,
            "path":"rubric.md", "artifact":{"artifactId":contracts::ArtifactId::new(), "storeBinding":"synthetic", "objectVersion":"rubric-v2", "sizeBytes":20, "mediaType":"text/plain"}, "sha256":"d".repeat(64)},
        "rubricPermission":null
    });
    binding
}

#[test]
fn frozen_binding_preserves_separate_releases_and_exact_object_versions()
-> Result<(), Box<dyn std::error::Error>> {
    let original = frozen_binding_json();
    let binding: contracts::http::ModelInputAuthorizationBinding =
        serde_json::from_value(original.clone())?;
    binding.validate()?; // Structural validation grants no visibility or egress permission.
    assert_eq!(serde_json::to_value(binding)?, original);
    assert_eq!(original["source"]["evaluationReleaseRevision"], 9);
    assert_eq!(original["source"]["environment"]["releaseVersion"], 4);
    assert_eq!(original["source"]["archive"]["objectVersion"], "archive-v1");
    assert_eq!(
        original["source"]["rubric"]["artifact"]["objectVersion"],
        "rubric-v2"
    );
    Ok(())
}

#[test]
fn frozen_binding_rejects_untyped_identity_and_caller_permission_claims()
-> Result<(), Box<dyn std::error::Error>> {
    for field in ["allowEgress", "visibility", "permission"] {
        let mut value = frozen_binding_json();
        value["source"]["rubric"][field] = json!(true);
        assert!(
            serde_json::from_value::<contracts::http::ModelInputAuthorizationBinding>(value)
                .is_err()
        );
    }
    let mut value = frozen_binding_json();
    let source = value
        .as_object_mut()
        .ok_or("not an object")?
        .remove("source")
        .ok_or("missing source")?;
    value["packageId"] = source["frozenSubmissionId"].clone();
    value["packageRevision"] = json!(3);
    assert!(
        serde_json::from_value::<contracts::http::ModelInputAuthorizationBinding>(value).is_err()
    );
    for field in ["manifestSha256", "contentSha256"] {
        let mut value = frozen_binding_json();
        value["source"][field] = json!("unknown");
        let binding: contracts::http::ModelInputAuthorizationBinding =
            serde_json::from_value(value)?;
        assert!(binding.validate().is_err());
    }
    Ok(())
}
