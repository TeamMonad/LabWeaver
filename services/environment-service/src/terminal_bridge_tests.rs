use super::{Identity, TerminalBridgeError, authorize_console_connection};
use contracts::{
    AccessGrantId, ConsoleSessionId, LeaseId,
    access::ConsoleKind,
    authoring::{EnvironmentClass, RuntimeKind},
    environment::{DesiredEnvironmentState, EnvironmentInstance, ObservedEnvironmentState},
};

use crate::test_support as support;
use support::{funded_ready_instance, ready_instance, revision, timestamp};

fn identity(instance: &EnvironmentInstance) -> Identity {
    Identity {
        session_id: ConsoleSessionId::new(),
        grant_id: AccessGrantId::new(),
        grant_revision: revision(1),
        environment_revision: instance.revision,
        lease: instance
            .operation
            .lease_authorization
            .as_ref()
            .map(|auth| (auth.lease_id, auth.lease_revision)),
    }
}

#[test]
fn funded_experiment_bridge_accepts_the_current_original_lease_for_both_runtimes() {
    let now = timestamp("2026-07-14T02:00:00.000Z");
    let mut instance = funded_ready_instance(EnvironmentClass::Experiment);
    for (runtime, kind) in [
        (RuntimeKind::Container, ConsoleKind::Xterm),
        (RuntimeKind::VirtualMachine, ConsoleKind::Novnc),
    ] {
        instance.runtime_kind = runtime;
        assert!(authorize_console_connection(&instance, kind, &identity(&instance), now).is_ok());
    }
}

#[test]
fn bridge_keeps_legacy_experiment_and_work_compatibility() {
    let now = timestamp("2026-07-14T02:00:00.000Z");
    for instance in [
        ready_instance(),
        funded_ready_instance(EnvironmentClass::Work),
    ] {
        assert!(
            authorize_console_connection(&instance, ConsoleKind::Xterm, &identity(&instance), now)
                .is_ok()
        );
    }
}

#[test]
fn bridge_rejects_expired_missing_and_mismatched_fences() {
    let now = timestamp("2026-07-14T02:00:00.000Z");
    for class in [EnvironmentClass::Experiment, EnvironmentClass::Work] {
        let instance = funded_ready_instance(class);
        for lease in [
            None,
            Some((LeaseId::new(), revision(1))),
            Some((instance.lease_id.unwrap_or_default(), revision(2))),
        ] {
            let mut headers = identity(&instance);
            headers.lease = lease;
            assert!(matches!(
                authorize_console_connection(&instance, ConsoleKind::Xterm, &headers, now),
                Err(TerminalBridgeError::ScopeDenied)
            ));
        }
        let mut expired = instance.clone();
        if let Some(auth) = &mut expired.operation.lease_authorization {
            auth.expires_at = now;
        }
        assert!(matches!(
            authorize_console_connection(&expired, ConsoleKind::Xterm, &identity(&expired), now),
            Err(TerminalBridgeError::ScopeDenied)
        ));
    }
    let legacy = ready_instance();
    let mut extra = identity(&legacy);
    extra.lease = Some((LeaseId::new(), revision(1)));
    assert!(matches!(
        authorize_console_connection(&legacy, ConsoleKind::Xterm, &extra, now),
        Err(TerminalBridgeError::ScopeDenied)
    ));
}

#[test]
fn bridge_rejects_stale_scope_generation_and_lifecycle() {
    let now = timestamp("2026-07-14T02:00:00.000Z");
    let original = funded_ready_instance(EnvironmentClass::Work);
    let headers = identity(&original);
    let mut cases = Vec::new();
    let mut changed = original.clone();
    changed.revision = revision(3);
    cases.push(changed);
    let mut changed = original.clone();
    changed.observed_generation = 0;
    cases.push(changed);
    let mut changed = original.clone();
    changed.desired_state = DesiredEnvironmentState::Stopped;
    cases.push(changed);
    let mut changed = original.clone();
    changed.observed_state = ObservedEnvironmentState::Deleting;
    cases.push(changed);
    let mut changed = original.clone();
    changed.eligibility_expires_at = Some(now);
    cases.push(changed);
    let mut changed = original;
    changed.runtime_kind = RuntimeKind::VirtualMachine;
    cases.push(changed);
    for instance in cases {
        assert!(matches!(
            authorize_console_connection(&instance, ConsoleKind::Xterm, &headers, now),
            Err(TerminalBridgeError::ScopeDenied)
        ));
    }
}

#[test]
fn bridge_rejects_not_yet_active_and_inconsistent_authorizations() {
    let now = timestamp("2026-07-14T02:00:00.000Z");
    for class in [EnvironmentClass::Experiment, EnvironmentClass::Work] {
        for mutation in 0..3 {
            let mut instance = funded_ready_instance(class);
            if mutation == 0 {
                instance.operation.lease_authorization = None;
            } else if let Some(auth) = &mut instance.operation.lease_authorization {
                if mutation == 1 {
                    auth.active_from = timestamp("2026-07-14T03:00:00.000Z");
                } else {
                    auth.environment_id = contracts::EnvironmentId::new();
                }
            }
            assert!(matches!(
                authorize_console_connection(
                    &instance,
                    ConsoleKind::Xterm,
                    &identity(&instance),
                    now
                ),
                Err(TerminalBridgeError::ScopeDenied)
            ));
        }
    }
}

#[test]
fn console_release_project_scope_uses_the_original_course_authorization() {
    let mut instance = funded_ready_instance(EnvironmentClass::Experiment);
    let mut projection = support::runtime_projection(&instance);
    projection.release.project_id = contracts::ProjectId::new();
    instance.runtime_authorization = Some(support::runtime_receipt(&instance, &projection));
    assert!(crate::resolver::console_release_project_matches(
        &instance,
        &projection
    ));
    let mut wrong = instance.clone();
    if let Some(receipt) = &mut wrong.runtime_authorization {
        receipt.authorization.configuration.authoring_project_id = contracts::ProjectId::new();
    }
    assert!(!crate::resolver::console_release_project_matches(
        &wrong,
        &projection
    ));
    let mut wrong = instance.clone();
    wrong.class = EnvironmentClass::Work;
    assert!(!crate::resolver::console_release_project_matches(
        &wrong,
        &projection
    ));
    let mut wrong = instance.clone();
    wrong.course_id = None;
    assert!(!crate::resolver::console_release_project_matches(
        &wrong,
        &projection
    ));
    let mut wrong = instance;
    wrong.lease_id = None;
    wrong.operation.lease_authorization = None;
    assert!(!crate::resolver::console_release_project_matches(
        &wrong,
        &projection
    ));
}
