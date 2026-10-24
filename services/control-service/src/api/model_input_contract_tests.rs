//! The production HTTP validators and RFC 9457 serializer must agree with generated operation errors.
use super::*;
use contracts::authoring::{PackageFile, ProblemPackage, ProjectLlmEgressPolicy};
use contracts::{
    AuthenticatedActor, AuthorizationDecision, AuthorizationScope, RetentionClass,
    RetentionDisposition, RetentionSnapshot,
};
use serde_json::{Value, json};

type Result<T, E = Box<dyn std::error::Error>> = std::result::Result<T, E>;

fn fixture() -> Result<(
    AuthorizationDecision,
    HeaderMap,
    ProblemPackage,
    ProjectLlmEgressPolicy,
)> {
    let now: UtcTimestamp = "2026-10-24T00:00:00.000Z".parse()?;
    let until: UtcTimestamp = "2099-01-01T00:00:00.000Z".parse()?;
    let project_id = ProjectId::new();
    let package = ProblemPackage {
        id: ProblemPackageId::new(),
        project_id,
        course_id: None,
        revision: Revision::new(1)?,
        files: vec![PackageFile {
            path: "README.md".to_owned(),
            sha256: "a".repeat(64),
            object: contracts::ArtifactRef {
                artifact_id: contracts::ArtifactId::new(),
                store_binding: "fixture".to_owned(),
                object_version: "v1".to_owned(),
                size_bytes: 1,
                media_type: "text/plain".to_owned(),
            },
        }],
        retention: RetentionSnapshot {
            policy_id: contracts::PolicyId::new(),
            policy_revision: Revision::new(1)?,
            class: RetentionClass::CourseMaterial,
            retain_until: Some(until),
            disposition: RetentionDisposition::Delete,
        },
        completed_at: now,
    };
    let policy = serde_json::from_value(
        json!({"id":contracts::PolicyId::new(),"projectId":project_id,"courseId":null,"revision":1,"binding":{"runtimeBinding":"fixture-runtime","model":"fixture-model","claudeCodeVersion":"2.1.207","maxInFlightPerWorker":1},"budget":{"maxInputTokens":1000,"maxOutputTokens":1000,"maxRequests":1,"maxCostMicrousd":1000,"timeoutMilliseconds":1000,"maxTransientRetries":0,"maxSchemaRepairs":0},"deniedDataClasses":["secret","token","private_key","personally_identifiable_information","unallowlisted_student_submission"],"studentContentMode":"manifest_allowlist_only","activatedAt":now}),
    )?;
    let decision = AuthorizationDecision {
        actor: AuthenticatedActor {
            actor_id: ActorId::new(),
            roles: vec![PlatformRole::Student],
            expires_at: until,
        },
        scope: AuthorizationScope::Project { project_id },
        authorization_revision: Revision::new(1)?,
        scope_revision: Revision::new(1)?,
        valid_until: until,
        diagnostic_code: None,
    };
    let mut headers = HeaderMap::new();
    headers.insert(SESSION_HEADER, BffSessionId::new().to_string().parse()?);
    Ok((decision, headers, package, policy))
}

fn declaration(operation: &str) -> Result<Vec<String>> {
    let artifacts = contracts::schema::generate_all()?;
    for artifact in artifacts
        .iter()
        .filter(|artifact| artifact.relative_path.starts_with("schemas/openapi/"))
    {
        let document: Value = serde_json::from_slice(&artifact.bytes)?;
        let paths = document["paths"].as_object().ok_or("paths missing")?;
        for path in paths.values().filter_map(Value::as_object) {
            for method in path.values() {
                if method["operationId"] == operation {
                    return method["x-labweaver-errors"]
                        .as_array()
                        .ok_or("HTTP errors missing")?
                        .iter()
                        .map(|code| {
                            code.as_str()
                                .map(str::to_owned)
                                .ok_or_else(|| "error must be a string".into())
                        })
                        .collect();
                }
            }
        }
    }
    Err(format!("operation not generated: {operation}").into())
}

async fn assert_declared(operation: &str, response: Response, expected: &str) -> Result<()> {
    assert!(response.status().is_client_error());
    assert_eq!(
        response
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("application/problem+json")
    );
    let body = axum::body::to_bytes(response.into_body(), 64 * 1024).await?;
    let actual: contracts::ProblemDetails = serde_json::from_slice(&body)?;
    assert_eq!(actual.diagnostic_code.as_str(), expected);
    let errors = declaration(operation)?;
    assert!(
        errors
            .iter()
            .any(|code| code == actual.diagnostic_code.as_str()),
        "{operation} must declare its actual HTTP failure"
    );
    for task_only in [
        "LW_MODEL_INPUT_AUTHORIZATION_REQUIRED",
        "LW_MODEL_INPUT_AUTHORIZATION_UNAVAILABLE",
    ] {
        assert!(!errors.iter().any(|code| code == task_only));
    }
    Ok(())
}

#[tokio::test]
async fn synchronous_model_input_validator_responses_are_declared() -> Result<()> {
    let (decision, headers, package, policy) = fixture()?;
    for operation in ["createProjectAgentRun", "createProjectWorkConfigurationRun"] {
        let response =
            match model_input_binding(&decision, &headers, operation, &package, &policy, None) {
                Ok(_) => return Err("missing selection was accepted".into()),
                Err(error) => error.into_response(),
            };
        assert_declared(operation, response, "LW_MODEL_INPUT_SELECTION_REQUIRED").await?;
        // The production create helper refuses malformed internal authorization context rather
        // than manufacturing a valid binding. This is a validator/HTTP-wire test, not a principal.
        let response = match model_input_binding(
            &decision,
            &headers,
            "invalid-operation",
            &package,
            &policy,
            Some(&["README.md".to_owned()]),
        ) {
            Ok(_) => return Err("invalid context was accepted".into()),
            Err(error) => error.into_response(),
        };
        assert_declared(operation, response, "LW_MODEL_INPUT_AUTHORIZATION_DENIED").await?;
    }
    let file = &package.files[0];
    let invalid = contracts::http::ModelInputRubricIdentity {
        package_id: package.id,
        package_revision: package.revision,
        path: "../hidden-rubric".to_owned(),
        artifact: file.object.clone(),
        sha256: file.sha256.clone(),
    };
    let response = match validate_model_input_rubric(&invalid) {
        Ok(()) => return Err("invalid rubric identity was accepted".into()),
        Err(error) => error.into_response(),
    };
    assert_declared(
        "maintainProjectRubricModelInputRule",
        response,
        "LW_MODEL_INPUT_AUTHORIZATION_INVALID",
    )
    .await?;
    Ok(())
}
