#![allow(dead_code, clippy::panic)]

use std::str::FromStr;

use contracts::authoring::{EnvironmentClass, RuntimeKind};
use contracts::environment::{
    DesiredEnvironmentState, EndpointHealth, EndpointProtocol, EnvironmentEndpoint,
    EnvironmentInstance, EnvironmentOperation, EnvironmentOperationKind, ObservedEnvironmentState,
    OperationState,
};
use contracts::{
    ActorId, CourseId, EndpointId, EnvironmentId, OperationId, ReleaseId, Revision, UtcTimestamp,
};

pub fn timestamp(value: &str) -> UtcTimestamp {
    UtcTimestamp::from_str(value).unwrap_or_else(|error| panic!("invalid test timestamp: {error}"))
}

pub fn ready_instance() -> EnvironmentInstance {
    let accepted_at = timestamp("2026-07-14T00:00:00.000Z");
    EnvironmentInstance {
        id: EnvironmentId::new(),
        display_label: "Ready environment".to_owned(),
        course_id: CourseId::new(),
        owner_id: ActorId::new(),
        class: EnvironmentClass::Experiment,
        runtime_kind: RuntimeKind::Container,
        release_id: ReleaseId::new(),
        release_version: 1,
        lease_id: None,
        capacity_binding: None,
        provider_binding: "container-primary-v1".to_owned(),
        desired_state: DesiredEnvironmentState::Running,
        observed_state: ObservedEnvironmentState::Ready,
        revision: revision(2),
        generation: 1,
        observed_generation: 1,
        operation: EnvironmentOperation {
            id: OperationId::new(),
            kind: EnvironmentOperationKind::Create,
            state: OperationState::Succeeded,
            accepted_revision: revision(1),
            attempt: 1,
            provider_step: 1,
            max_attempts: 3,
            next_attempt_at: accepted_at,
            actor_id: ActorId::new(),
            trace_id: "11111111111111111111111111111111".to_owned(),
            accepted_at,
            deadline_at: timestamp("2026-07-14T00:10:00.000Z"),
            cleanup_started_at: None,
            diagnostic_code: None,
            preserve_mutable_disk: false,
            access_revocation_revision: None,
            retry_from_phase: None,
            reset_target: None,
            lease_authorization: None,
        },
        eligibility_expires_at: timestamp("2026-07-15T00:00:00.000Z"),
        endpoints: vec![EnvironmentEndpoint {
            id: EndpointId::new(),
            protocol: EndpointProtocol::Https,
            revision: revision(2),
            health: EndpointHealth::Healthy,
            ssh_host_key_identity_sha256: None,
            observed_at: timestamp("2026-07-14T00:01:00.000Z"),
        }],
        last_diagnostic_code: None,
        failed_phase: None,
        cleanup_evidence: None,
    }
}

pub fn requested_instance() -> EnvironmentInstance {
    let accepted_at = timestamp("2026-07-14T00:00:00.000Z");
    EnvironmentInstance {
        id: EnvironmentId::new(),
        display_label: "Requested environment".to_owned(),
        course_id: CourseId::new(),
        owner_id: ActorId::new(),
        class: EnvironmentClass::Experiment,
        runtime_kind: RuntimeKind::Container,
        release_id: ReleaseId::new(),
        release_version: 1,
        lease_id: None,
        capacity_binding: None,
        provider_binding: "container-primary-v1".to_owned(),
        desired_state: DesiredEnvironmentState::Running,
        observed_state: ObservedEnvironmentState::Requested,
        revision: revision(1),
        generation: 1,
        observed_generation: 0,
        operation: EnvironmentOperation {
            id: OperationId::new(),
            kind: EnvironmentOperationKind::Create,
            state: OperationState::Accepted,
            accepted_revision: revision(1),
            attempt: 1,
            provider_step: 1,
            max_attempts: 3,
            next_attempt_at: accepted_at,
            actor_id: ActorId::new(),
            trace_id: "22222222222222222222222222222222".to_owned(),
            accepted_at,
            deadline_at: timestamp("2026-07-14T00:10:00.000Z"),
            cleanup_started_at: None,
            diagnostic_code: None,
            preserve_mutable_disk: false,
            access_revocation_revision: None,
            retry_from_phase: None,
            reset_target: None,
            lease_authorization: None,
        },
        eligibility_expires_at: timestamp("2026-07-15T00:00:00.000Z"),
        endpoints: Vec::new(),
        last_diagnostic_code: None,
        failed_phase: None,
        cleanup_evidence: None,
    }
}

pub fn revision(value: u64) -> Revision {
    Revision::new(value).unwrap_or_else(|error| panic!("invalid test revision: {error}"))
}
