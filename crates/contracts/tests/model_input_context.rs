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
