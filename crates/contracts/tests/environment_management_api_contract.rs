//! Contract and generated-OpenAPI conformance for Issue #81.

use std::str::FromStr;

use contracts::environment::{
    EnvironmentOperationKind, EnvironmentOperationSnapshot, EnvironmentOperationStatus,
    PublicEnvironmentOperationPhase,
};
use contracts::http::{
    CreateEnvironmentRequest, EnvironmentInventoryQuery, EnvironmentOperationListQuery,
    EventStreamQuery, MAX_PAGE_LIMIT,
};
use contracts::{
    CourseId, EnvironmentId, OperationId, ReleaseId, Revision, StreamSequence, UtcTimestamp,
};
use serde_json::Value;

#[test]
fn stream_sequence_is_lossless_and_canonical_on_every_public_resume_surface()
-> Result<(), Box<dyn std::error::Error>> {
    let course_id = CourseId::from_str("01900000-0000-7000-8000-000000000001")?;
    let cursor = StreamSequence(u64::MAX);

    assert_eq!(serde_json::to_string(&cursor)?, format!("\"{}\"", u64::MAX));
    assert_eq!(
        serde_json::from_str::<StreamSequence>(&format!("\"{}\"", u64::MAX))?,
        cursor
    );
    for invalid in ["18446744073709551616", "01", "+1", " 1", "1 "] {
        assert!(
            StreamSequence::from_str(invalid).is_err(),
            "accepted {invalid}"
        );
    }
    assert!(serde_json::from_str::<StreamSequence>("9007199254740993").is_err());

    let cursor_schema = serde_json::to_value(schemars::schema_for!(StreamSequence))?;
    let cursor_validator = jsonschema::validator_for(&cursor_schema)?;
    assert!(cursor_validator.is_valid(&serde_json::json!(u64::MAX.to_string())));
    for invalid in [
        serde_json::json!("18446744073709551616"),
        serde_json::json!("01"),
        serde_json::json!(9_007_199_254_740_993_u64),
    ] {
        assert!(
            !cursor_validator.is_valid(&invalid),
            "schema accepted {invalid}"
        );
    }

    let query: EventStreamQuery = serde_json::from_value(serde_json::json!({
        "courseId": course_id,
        "after": "9007199254740993"
    }))?;
    assert_eq!(query.after, Some(StreamSequence(9_007_199_254_740_993)));
    assert!(
        serde_json::from_value::<EventStreamQuery>(serde_json::json!({
            "courseId": course_id,
            "after": 9_007_199_254_740_993_u64
        }))
        .is_err()
    );
    Ok(())
}

#[test]
fn inventory_and_operation_queries_enforce_bounded_opaque_pagination()
-> Result<(), Box<dyn std::error::Error>> {
    let course_id = CourseId::from_str("01900000-0000-7000-8000-000000000001")?;
    let query = EnvironmentInventoryQuery {
        course_id,
        project_id: None,
        runtime_kind: None,
        class: None,
        desired_state: None,
        observed_state: None,
        release_id: None,
        cursor: Some("opaque.cursor_1~snapshot".to_owned()),
        limit: Some(MAX_PAGE_LIMIT),
    };
    assert!(query.validate().is_ok());

    let invalid_limit = EnvironmentOperationListQuery {
        kind: None,
        state: None,
        cursor: None,
        limit: Some(MAX_PAGE_LIMIT + 1),
    };
    assert!(invalid_limit.validate().is_err());

    let invalid_cursor = EnvironmentOperationListQuery {
        kind: None,
        state: None,
        cursor: Some("not/opaque".to_owned()),
        limit: None,
    };
    assert!(invalid_cursor.validate().is_err());

    let create = CreateEnvironmentRequest {
        course_id,
        release_id: ReleaseId::from_str("01900000-0000-7000-8000-000000000002")?,
        release_version: 1,
        display_label: Some("Linux systems lab".to_owned()),
    };
    assert!(create.validate().is_ok());
    Ok(())
}

#[test]
fn operation_snapshot_requires_consistent_terminal_facts() -> Result<(), Box<dyn std::error::Error>>
{
    let accepted_at = UtcTimestamp::from_str("2026-07-15T12:00:00.000Z")?;
    let terminal_at = UtcTimestamp::from_str("2026-07-15T12:05:00.000Z")?;
    let mut snapshot = EnvironmentOperationSnapshot {
        environment_id: EnvironmentId::from_str("01900000-0000-7000-8000-000000000001")?,
        operation_id: OperationId::from_str("01900000-0000-7000-8000-000000000002")?,
        kind: EnvironmentOperationKind::Start,
        state: EnvironmentOperationStatus::TimedOut,
        accepted_revision: Revision::new(4)?,
        current_revision: Revision::new(5)?,
        accepted_at,
        started_at: Some(accepted_at),
        updated_at: terminal_at,
        terminal_at: Some(terminal_at),
        deadline_at: terminal_at,
        timed_out_at: Some(terminal_at),
        cleanup_started_at: None,
        cleanup_deadline_at: None,
        provider_phase: Some(PublicEnvironmentOperationPhase::Provisioning),
        attempt: 3,
        max_attempts: 3,
        retry_eligible: false,
        cancel_eligible: false,
        diagnostic_code: Some(contracts::DiagnosticCode::parse(
            "LW_ENVIRONMENT_PROVIDER_TIMEOUT",
        )?),
        request_id: "request-81".to_owned(),
        trace_id: "trace-81".to_owned(),
        last_changed_stream_sequence: StreamSequence(9),
    };
    assert!(snapshot.validate().is_ok());
    snapshot.retry_eligible = true;
    assert!(snapshot.validate().is_ok());
    snapshot.cancel_eligible = true;
    assert!(snapshot.validate().is_err());
    Ok(())
}

#[test]
fn generated_openapi_closes_inventory_operation_and_grant_discovery_gaps()
-> Result<(), Box<dyn std::error::Error>> {
    let generated = contracts::schema::generate_all()?;
    let artifact = generated
        .iter()
        .find(|item| item.relative_path.ends_with("labweaver-public.v1.json"))
        .ok_or("Public OpenAPI artifact missing")?;
    let openapi: Value = serde_json::from_slice(&artifact.bytes)?;
    let paths = openapi
        .get("paths")
        .and_then(Value::as_object)
        .ok_or("OpenAPI paths missing")?;

    for path in [
        "/api/v1/environments",
        "/api/v1/environments/{environmentId}/operations/{operationId}",
        "/api/v1/environments/{environmentId}/operations",
        "/api/v1/environments/{environmentId}/access-grants",
    ] {
        assert!(paths.contains_key(path), "missing Public API path: {path}");
    }

    let inventory = &paths["/api/v1/environments"]["get"];
    let parameters = inventory["parameters"]
        .as_array()
        .ok_or("inventory parameters missing")?;
    assert!(
        parameters
            .iter()
            .any(|parameter| { parameter["name"] == "courseId" && parameter["required"] == true })
    );
    assert!(inventory["responses"].get("410").is_some());
    assert!(inventory["responses"].get("409").is_some());
    assert!(inventory["responses"].get("412").is_none());
    for diagnostic in [
        "LW_HTTP_UNAUTHENTICATED",
        "LW_ACCESS_DENIED",
        "LW_ENVIRONMENT_CURSOR_INVALID",
        "LW_ENVIRONMENT_CURSOR_EXPIRED",
        "LW_HTTP_RATE_LIMITED",
        "LW_HTTP_SERVICE_UNAVAILABLE",
        "LW_HTTP_INTERNAL",
    ] {
        assert!(
            inventory["x-labweaver-errors"]
                .as_array()
                .is_some_and(|codes| codes.iter().any(|code| code == diagnostic))
        );
    }

    let create_schema = &paths["/api/v1/environments"]["post"]["responses"]["202"]["content"]["application/json"]
        ["schema"]["$ref"];
    assert_eq!(
        create_schema,
        "../contracts/v1/http/environment-operation-accepted.schema.json"
    );

    let stream = &paths["/api/v1/events"]["get"];
    assert!(
        stream["responses"]["200"]["content"]
            .get("text/event-stream")
            .is_some()
    );
    assert!(
        stream["x-labweaver-errors"]
            .as_array()
            .is_some_and(|codes| codes.iter().any(|code| code == "LW_SSE_CURSOR_CONFLICT"))
    );
    for parameter_name in ["after", "Last-Event-ID"] {
        let parameter = stream["parameters"]
            .as_array()
            .and_then(|parameters| {
                parameters
                    .iter()
                    .find(|parameter| parameter["name"] == parameter_name)
            })
            .ok_or("SSE cursor parameter missing")?;
        assert_eq!(parameter["schema"]["type"], "string");
    }
    Ok(())
}
