//! Strict public Evaluation release and student-result projection contracts.

use contracts::http::{
    CreateEvaluationReleaseRequest, EvaluationReleaseListQuery, WithdrawEvaluationReleaseRequest,
};

#[test]
fn teacher_release_commands_reject_unknown_fields_and_invalid_identities() {
    let valid = serde_json::json!({
        "candidateId": uuid::Uuid::now_v7(),
        "candidateRevision": 1,
        "approvalId": uuid::Uuid::now_v7(),
    });
    assert!(serde_json::from_value::<CreateEvaluationReleaseRequest>(valid.clone()).is_ok());

    let mut unknown = valid.clone();
    unknown["runnerImage"] = serde_json::json!("registry.invalid/override:latest");
    assert!(serde_json::from_value::<CreateEvaluationReleaseRequest>(unknown).is_err());

    let mut zero_revision = valid.clone();
    zero_revision["candidateRevision"] = serde_json::json!(0);
    assert!(serde_json::from_value::<CreateEvaluationReleaseRequest>(zero_revision).is_err());

    assert!(
        serde_json::from_value::<WithdrawEvaluationReleaseRequest>(serde_json::json!({
            "expectedRevision": 1,
            "reasonCode": "withdrawn_without_registered_diagnostic"
        }))
        .is_err()
    );
}

#[test]
fn evaluation_list_cursor_is_bounded_and_injection_safe() {
    assert!(EvaluationReleaseListQuery::default().validate().is_ok());
    assert!(
        EvaluationReleaseListQuery {
            cursor: Some(uuid::Uuid::now_v7().to_string()),
            limit: Some(100),
        }
        .validate()
        .is_ok()
    );
    assert!(
        EvaluationReleaseListQuery {
            cursor: Some("../another-course".to_owned()),
            limit: Some(50),
        }
        .validate()
        .is_err()
    );
    assert!(
        EvaluationReleaseListQuery {
            cursor: None,
            limit: Some(0),
        }
        .validate()
        .is_err()
    );
}

#[test]
fn student_projection_schema_excludes_private_evaluation_fields()
-> Result<(), Box<dyn std::error::Error>> {
    let schema =
        include_str!("../../../schemas/contracts/v1/student-evaluation-result.schema.json");
    let document: serde_json::Value = serde_json::from_str(schema)?;
    for forbidden in [
        "stepId",
        "stepRunId",
        "dependsOn",
        "evidenceSha256",
        "runtimeIdentity",
        "evaluationSpec",
        "privateCase",
        "submissionContent",
        "rawLog",
    ] {
        assert!(
            !schema.contains(&format!("\"{forbidden}\"")),
            "student projection schema leaked {forbidden}"
        );
    }
    assert!(schema.contains("\"additionalProperties\": false"));
    assert_eq!(
        document["$defs"]["StudentEvaluationResultState"]["enum"],
        serde_json::json!(["succeeded", "failed", "cancelled"]),
        "student result wire contract must be terminal-only"
    );
    assert_eq!(
        document["$defs"]["StudentEvaluationStepResult"]["properties"]["position"]["minimum"], 1,
        "public step ordinals are one-based"
    );
    Ok(())
}

#[test]
fn generated_release_schema_enforces_revision_and_sha256_wire_constraints()
-> Result<(), Box<dyn std::error::Error>> {
    let document: serde_json::Value = serde_json::from_str(include_str!(
        "../../../schemas/contracts/v1/http/create-evaluation-release-request.schema.json"
    ))?;
    assert_eq!(document["$defs"]["Revision"]["minimum"], 1);
    // Sha256Digest has been removed in ARC-09 mono refactor
    assert!(document["$defs"].get("Sha256Digest").is_none());
    Ok(())
}
