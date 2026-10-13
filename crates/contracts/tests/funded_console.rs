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

struct ConsoleFixture {
    request: EnvironmentConsoleEligibilityRequest,
    eligibility: EnvironmentConsoleEligibility,
    availability: ConsoleCapabilityAvailability,
    capability: ConsoleCapability,
    now: UtcTimestamp,
}

fn fixture() -> Result<ConsoleFixture, Box<dyn std::error::Error>> {
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
    Ok(ConsoleFixture {
        request,
        eligibility,
        availability,
        capability,
        now,
    })
}

#[test]
fn funded_experiment_console_eligibility_retains_its_fence()
-> Result<(), Box<dyn std::error::Error>> {
    let case = fixture()?;
    case.eligibility.validate_for(&case.request, case.now)?;
    assert!(case.eligibility.lease_fence.is_some());
    Ok(())
}

#[test]
fn funded_experiment_console_availability_is_valid() -> Result<(), Box<dyn std::error::Error>> {
    fixture()?.availability.validate()?;
    Ok(())
}

#[test]
fn funded_experiment_console_capability_keeps_the_exact_original_fence()
-> Result<(), Box<dyn std::error::Error>> {
    let case = fixture()?;
    case.capability.validate()?;
    case.capability.validate_against(&case.availability)?;
    Ok(())
}

#[test]
fn funded_console_cannot_drop_or_replace_the_original_fence()
-> Result<(), Box<dyn std::error::Error>> {
    let case = fixture()?;
    for mutation in 0..4 {
        let mut capability = case.capability.clone();
        if mutation == 0 {
            capability.lease_fence = None;
        } else if let Some(fence) = &mut capability.lease_fence {
            match mutation {
                1 => fence.lease_id = LeaseId::new(),
                2 => fence.lease_revision = Revision::new(5)?,
                _ => fence.expires_at = UtcTimestamp::from_str("2026-10-13T00:11:00.000Z")?,
            }
        }
        assert!(capability.validate_against(&case.availability).is_err());
    }
    Ok(())
}

#[test]
fn expired_lease_denies_console_eligibility_availability_and_capability()
-> Result<(), Box<dyn std::error::Error>> {
    let mut case = fixture()?;
    for fence in [
        &mut case.eligibility.lease_fence,
        &mut case.availability.lease_fence,
        &mut case.capability.lease_fence,
    ]
    .into_iter()
    .flatten()
    {
        fence.expires_at = case.now;
    }
    assert!(
        case.eligibility
            .validate_for(&case.request, case.now)
            .is_err()
    );
    assert!(case.availability.validate().is_err());
    assert!(case.capability.validate().is_err());
    Ok(())
}

#[test]
fn both_classes_keep_valid_fences_and_only_legacy_experiment_can_omit_one()
-> Result<(), Box<dyn std::error::Error>> {
    for class in [EnvironmentClass::Experiment, EnvironmentClass::Work] {
        let mut case = fixture()?;
        case.eligibility.environment_class = class;
        case.availability.environment_class = class;
        case.capability.environment_class = class;
        case.eligibility.validate_for(&case.request, case.now)?;
        case.capability.validate_against(&case.availability)?;
        case.eligibility.lease_fence = None;
        case.availability.lease_fence = None;
        case.capability.lease_fence = None;
        let legacy = class == EnvironmentClass::Experiment;
        assert_eq!(
            case.eligibility
                .validate_for(&case.request, case.now)
                .is_ok(),
            legacy
        );
        assert_eq!(case.availability.validate().is_ok(), legacy);
        assert_eq!(
            case.capability.validate_against(&case.availability).is_ok(),
            legacy
        );
    }
    Ok(())
}
