//! Original Resource lease fences apply to either environment class.

use std::str::FromStr;

use contracts::{
    AccessGrantId, ActorId, ConsoleCapabilityId, EnvironmentId, LeaseId, ProjectId, ReleaseId,
    Revision, UtcTimestamp,
    access::{ConsoleCapability, ConsoleCapabilityAvailability, ConsoleKind, ConsoleLeaseFence},
    authoring::{EnvironmentClass, RuntimeKind},
    environment::{
        EnvironmentAccessSubjectKind, EnvironmentConsoleBinding, EnvironmentConsoleEligibility,
        EnvironmentConsoleEligibilityRequest,
    },
};

fn fixture() -> Result<
    (
        EnvironmentConsoleEligibilityRequest,
        EnvironmentConsoleEligibility,
        ConsoleCapabilityAvailability,
        ConsoleCapability,
        UtcTimestamp,
    ),
    Box<dyn std::error::Error>,
> {
    let now = UtcTimestamp::from_str("2026-10-13T00:00:00.000Z")?;
    let expires = UtcTimestamp::from_str("2026-10-13T00:10:00.000Z")?;
    let revision = Revision::new(3)?;
    let request = EnvironmentConsoleEligibilityRequest {
        environment_id: EnvironmentId::new(),
        project_id: ProjectId::new(),
        course_id: None,
        actor_id: ActorId::new(),
        subject_kind: EnvironmentAccessSubjectKind::Owner,
        expected_revision: revision,
    };
    let fence = ConsoleLeaseFence {
        lease_id: LeaseId::new(),
        lease_revision: Revision::new(4)?,
        expires_at: expires,
    };
    let eligibility = EnvironmentConsoleEligibility {
        environment_id: request.environment_id,
        project_id: request.project_id,
        course_id: None,
        owner_actor_id: request.actor_id,
        environment_class: EnvironmentClass::Experiment,
        runtime_kind: RuntimeKind::VirtualMachine,
        environment_revision: revision,
        release_id: ReleaseId::new(),
        release_version: 1,
        eligibility_expires_at: Some(expires),
        lease_fence: Some(fence.clone()),
        binding: EnvironmentConsoleBinding::Novnc,
    };
    let availability = ConsoleCapabilityAvailability {
        access_grant_id: AccessGrantId::new(),
        access_grant_revision: revision,
        project_id: request.project_id,
        course_id: None,
        environment_id: request.environment_id,
        environment_class: EnvironmentClass::Experiment,
        environment_revision: revision,
        expires_at: expires,
        lease_fence: Some(fence.clone()),
        kinds: vec![ConsoleKind::Novnc],
    };
    let capability = ConsoleCapability {
        id: ConsoleCapabilityId::new(),
        kind: ConsoleKind::Novnc,
        access_grant_id: availability.access_grant_id,
        access_grant_revision: revision,
        project_id: request.project_id,
        course_id: None,
        environment_id: request.environment_id,
        environment_class: EnvironmentClass::Experiment,
        environment_revision: revision,
        lease_fence: Some(fence),
        issued_at: now,
        expires_at: UtcTimestamp::from_str("2026-10-13T00:00:30.000Z")?,
        connection_locator: "/connect/console/synthetic-handoff".to_owned(),
        websocket_subprotocol: ConsoleKind::Novnc.websocket_subprotocol().to_owned(),
    };
    Ok((request, eligibility, availability, capability, now))
}

#[test]
fn funded_experiment_console_eligibility_retains_its_fence()
-> Result<(), Box<dyn std::error::Error>> {
    let (request, eligibility, _, _, now) = fixture()?;
    eligibility.validate_for(&request, now)?;
    assert!(eligibility.lease_fence.is_some());
    Ok(())
}

#[test]
fn funded_experiment_console_availability_is_valid() -> Result<(), Box<dyn std::error::Error>> {
    let (_, _, availability, _, _) = fixture()?;
    availability.validate()?;
    Ok(())
}

#[test]
fn funded_experiment_console_capability_keeps_the_exact_original_fence()
-> Result<(), Box<dyn std::error::Error>> {
    let (_, _, availability, capability, _) = fixture()?;
    capability.validate()?;
    capability.validate_against(&availability)?;
    Ok(())
}
