//! Regression coverage for teacher authoring and Claude Code runtime bindings.

use contracts::authoring::{AuthoringError, CourseLlmEgressPolicy};
use contracts::{CourseId, PolicyId};
use serde_json::{Value, json};

fn valid_policy_json() -> Value {
    json!({
        "id": PolicyId::new(),
        "courseId": CourseId::new(),
        "revision": 1,
        "binding": {
            "runtimeBinding": "claude-code-production",
            "model": "claude-sonnet-4-6-20260601",
            "claudeCodeVersion": "2.1.207",
            "workerImageSha256": "11".repeat(32),
            "runtimeConfigSha256": "22".repeat(32),
            "maxInFlightPerWorker": 2
        },
        "budget": {
            "maxInputTokens": 100_000,
            "maxOutputTokens": 16_000,
            "maxRequests": 8,
            "maxCostMicrousd": 2_000_000,
            "timeoutMilliseconds": 120_000,
            "maxTransientRetries": 2,
            "maxSchemaRepairs": 2
        },
        "deniedDataClasses": [
            "secret",
            "token",
            "private_key",
            "personally_identifiable_information",
            "unallowlisted_student_submission"
        ],
        "studentContentMode": "manifest_allowlist_only",
        "activatedAt": "2026-07-14T08:00:00.000Z"
    })
}

#[test]
fn claude_code_binding_is_explicit_and_provider_opaque() -> Result<(), Box<dyn std::error::Error>> {
    let policy: CourseLlmEgressPolicy = serde_json::from_value(valid_policy_json())?;
    policy.validate()?;

    assert_eq!(policy.binding.runtime_binding, "claude-code-production");
    assert_eq!(policy.binding.model, "claude-sonnet-4-6-20260601");
    assert_eq!(policy.binding.claude_code_version, "2.1.207");
    Ok(())
}

#[test]
fn legacy_openai_binding_is_rejected() {
    let mut value = valid_policy_json();
    value["binding"] = json!({
        "providerBinding": "openai-production",
        "model": "gpt-5",
        "strictStructuredOutputs": true
    });

    assert!(serde_json::from_value::<CourseLlmEgressPolicy>(value).is_err());
}

#[test]
fn runtime_binding_and_immutable_worker_identity_are_required()
-> Result<(), Box<dyn std::error::Error>> {
    for field in ["runtimeBinding", "claudeCodeVersion"] {
        let mut value = valid_policy_json();
        value["binding"][field] = json!("");
        let policy: CourseLlmEgressPolicy = serde_json::from_value(value)?;
        let error = match policy.validate() {
            Ok(()) => return Err("empty runtime identity accepted".into()),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            AuthoringError::RuntimeBindingRequired | AuthoringError::RuntimeIdentityInvalid
        ));
    }
    for field in ["workerImageSha256", "runtimeConfigSha256"] {
        let mut value = valid_policy_json();
        value["binding"][field] = json!("");
        assert!(serde_json::from_value::<CourseLlmEgressPolicy>(value).is_err());
    }

    for value in [0, 65] {
        let mut invalid = valid_policy_json();
        invalid["binding"]["maxInFlightPerWorker"] = json!(value);
        assert!(
            serde_json::from_value::<CourseLlmEgressPolicy>(invalid)?
                .validate()
                .is_err()
        );
    }
    Ok(())
}

#[test]
fn moving_claude_model_aliases_are_not_immutable_bindings() -> Result<(), Box<dyn std::error::Error>>
{
    for alias in ["default", "sonnet", "Sonnet", "opus", "haiku", "opusplan"] {
        let mut value = valid_policy_json();
        value["binding"]["model"] = json!(alias);
        let policy: CourseLlmEgressPolicy = serde_json::from_value(value)?;
        assert_eq!(policy.validate(), Err(AuthoringError::ModelRequired));
    }
    Ok(())
}

#[test]
fn claude_code_version_and_retry_bounds_are_fail_closed() -> Result<(), Box<dyn std::error::Error>>
{
    for version in ["latest", "2.1", "2.01.3", "2.1.3-beta"] {
        let mut value = valid_policy_json();
        value["binding"]["claudeCodeVersion"] = json!(version);
        let policy: CourseLlmEgressPolicy = serde_json::from_value(value)?;
        assert_eq!(
            policy.validate(),
            Err(AuthoringError::RuntimeIdentityInvalid)
        );
    }
    let mut value = valid_policy_json();
    value["budget"]["maxTransientRetries"] = json!(3);
    let policy: CourseLlmEgressPolicy = serde_json::from_value(value)?;
    assert_eq!(policy.validate(), Err(AuthoringError::InvalidBudget));
    Ok(())
}
