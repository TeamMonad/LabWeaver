use super::{Identity, TerminalBridgeError, authorize_console_connection};
use contracts::{
    AccessGrantId, ConsoleSessionId, LeaseId,
    access::ConsoleKind,
    authoring::{EnvironmentClass, RuntimeKind},
    environment::{DesiredEnvironmentState, EnvironmentInstance, ObservedEnvironmentState},
};

#[path = "../tests/support/mod.rs"]
mod support;
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
