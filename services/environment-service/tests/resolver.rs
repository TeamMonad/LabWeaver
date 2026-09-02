//! Fail-closed owner resolution regression coverage.

mod support;

use axum::http::header;
use axum::response::IntoResponse;
use contracts::{
    ActorId,
    environment::{
        EnvironmentAccessSubjectKind, EnvironmentEndpointEligibilityRequest,
        EnvironmentOwnerResolutionRequest, ObservedEnvironmentState,
    },
};
use environment_service::{
    OwnerResolverError, authorize_endpoint_eligibility, authorize_owner_resolution,
};

use support::{ready_instance, revision, timestamp};

#[test]
fn exact_authoritative_tuple_resolves_without_endpoint_or_credential_data()
-> Result<(), Box<dyn std::error::Error>> {
    let instance = ready_instance();
    let request = EnvironmentOwnerResolutionRequest {
        environment_id: instance.id,
        course_id: instance.course_id,
        owner_actor_id: instance.owner_id,
        expected_revision: instance.revision,
    };
    let resolution =
        authorize_owner_resolution(&instance, &request, timestamp("2026-07-14T02:00:00.000Z"))?;
    let value = serde_json::to_value(resolution)?;
    assert!(value.get("environmentRevision").is_some());
    assert!(value.get("endpoint").is_none());
    assert!(value.get("credential").is_none());
    Ok(())
}

#[test]
fn endpoint_eligibility_returns_only_exact_requested_safe_metadata()
-> Result<(), Box<dyn std::error::Error>> {
    let instance = ready_instance();
    let endpoint_id = instance.endpoints[0].id;
    let request = EnvironmentEndpointEligibilityRequest {
        environment_id: instance.id,
        course_id: instance.course_id,
        actor_id: instance.owner_id,
        subject_kind: EnvironmentAccessSubjectKind::Owner,
        expected_revision: instance.revision,
        endpoint_ids: vec![endpoint_id],
    };
    let result =
        authorize_endpoint_eligibility(&instance, &request, timestamp("2026-07-14T02:00:00.000Z"))?;
    assert_eq!(result.endpoints.len(), 1);
    let value = serde_json::to_value(result)?;
    assert!(value.get("host").is_none());
    assert!(value.get("port").is_none());
    assert!(value.get("credential").is_none());
    Ok(())
}

#[test]
fn endpoint_eligibility_enforces_owner_but_accepts_access_verified_teacher_scope() {
    let instance = ready_instance();
    let other_actor = ActorId::new();
    let mut request = EnvironmentEndpointEligibilityRequest {
        environment_id: instance.id,
        course_id: instance.course_id,
        actor_id: other_actor,
        subject_kind: EnvironmentAccessSubjectKind::Owner,
        expected_revision: instance.revision,
        endpoint_ids: vec![instance.endpoints[0].id],
    };
    assert!(matches!(
        authorize_endpoint_eligibility(&instance, &request, timestamp("2026-07-14T02:00:00.000Z")),
        Err(OwnerResolverError::ScopeMismatch)
    ));
    request.subject_kind = EnvironmentAccessSubjectKind::CourseTeacher;
    assert!(
        authorize_endpoint_eligibility(&instance, &request, timestamp("2026-07-14T02:00:00.000Z"))
            .is_ok()
    );
}

#[test]
fn resolver_failures_use_problem_details_content_type() {
    let response = OwnerResolverError::RequestInvalid.into_response();
    assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE),
        Some(&axum::http::HeaderValue::from_static(
            "application/problem+json"
        ))
    );
}

#[test]
fn scope_revision_lifecycle_and_expiry_mismatches_fail_closed() {
    let instance = ready_instance();
    let request = EnvironmentOwnerResolutionRequest {
        environment_id: instance.id,
        course_id: instance.course_id,
        owner_actor_id: instance.owner_id,
        expected_revision: revision(instance.revision.get() + 1),
    };
    assert!(matches!(
        authorize_owner_resolution(&instance, &request, timestamp("2026-07-14T02:00:00.000Z")),
        Err(OwnerResolverError::ScopeMismatch)
    ));

    let exact = EnvironmentOwnerResolutionRequest {
        expected_revision: instance.revision,
        ..request
    };
    let mut deleting = instance.clone();
    deleting.observed_state = ObservedEnvironmentState::Deleting;
    assert!(matches!(
        authorize_owner_resolution(&deleting, &exact, timestamp("2026-07-14T02:00:00.000Z")),
        Err(OwnerResolverError::EnvironmentUnavailable)
    ));
    assert!(matches!(
        authorize_owner_resolution(&instance, &exact, timestamp("2026-07-16T00:00:00.000Z")),
        Err(OwnerResolverError::EnvironmentUnavailable)
    ));

    let mut stale_endpoint = instance.clone();
    stale_endpoint.endpoints[0].revision = revision(instance.revision.get() - 1);
    assert!(matches!(
        authorize_owner_resolution(
            &stale_endpoint,
            &exact,
            timestamp("2026-07-14T02:00:00.000Z")
        ),
        Err(OwnerResolverError::EnvironmentUnavailable)
    ));
}
