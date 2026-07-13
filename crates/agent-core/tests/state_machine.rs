//! Agent state-machine transition and failure tests.

use std::error::Error;

use agent_core::{AgentEvent, AgentRun, AgentState, TransitionOutcome};

#[test]
fn valid_run_reaches_approval_handoff_without_publishing() -> Result<(), Box<dyn Error>> {
    let mut run = AgentRun::new("run-001")?;
    for event in [
        AgentEvent::Parsed,
        AgentEvent::CapabilitiesRetrieved,
        AgentEvent::PlanCreated,
        AgentEvent::CandidateGenerated,
        AgentEvent::SchemaValid,
        AgentEvent::PolicyAllowed,
        AgentEvent::ValidationExecuted,
        AgentEvent::VerificationPassed,
    ] {
        run.apply(event)?;
    }
    assert_eq!(run.state(), AgentState::AwaitingReleaseApproval);

    let record = run.apply(AgentEvent::TeacherApproved)?;
    assert_eq!(record.outcome(), TransitionOutcome::ApprovalHandedOff);
    assert_eq!(run.state(), AgentState::Completed);
    assert!(run.state().is_terminal());
    Ok(())
}

#[test]
fn execution_approval_cannot_bypass_deterministic_verification() -> Result<(), Box<dyn Error>> {
    let mut run = AgentRun::new("run-elevated")?;
    for event in [
        AgentEvent::Parsed,
        AgentEvent::CapabilitiesRetrieved,
        AgentEvent::PlanCreated,
        AgentEvent::CandidateGenerated,
        AgentEvent::SchemaValid,
        AgentEvent::ApprovalRequired,
    ] {
        run.apply(event)?;
    }
    assert_eq!(run.state(), AgentState::AwaitingExecutionApproval);

    let record = run.apply(AgentEvent::TeacherApproved)?;
    assert_eq!(record.outcome(), TransitionOutcome::Advanced);
    assert_eq!(run.state(), AgentState::ExecuteValidation);
    assert!(!run.state().is_terminal());

    run.apply(AgentEvent::ValidationExecuted)?;
    run.apply(AgentEvent::VerificationPassed)?;
    assert_eq!(run.state(), AgentState::AwaitingReleaseApproval);
    Ok(())
}

#[test]
fn illegal_transition_is_fail_fast_and_atomic() -> Result<(), Box<dyn Error>> {
    let mut run = AgentRun::new("run-invalid")?;
    let Err(error) = run.apply(AgentEvent::CandidateGenerated) else {
        return Err("illegal transition unexpectedly succeeded".into());
    };
    assert_eq!(error.diagnostic_code(), "LW_AGENT_TRANSITION_INVALID");
    assert_eq!(run.state(), AgentState::Parse);
    assert_eq!(run.automatic_repairs(), 0);
    assert!(run.history().is_empty());
    Ok(())
}

#[test]
fn automatic_repair_budget_is_bounded_and_observable() -> Result<(), Box<dyn Error>> {
    let mut run = AgentRun::new("run-repair")?;
    drive_to_schema_validation(&mut run)?;

    for expected_repairs in 1..=2 {
        run.apply(AgentEvent::SchemaInvalid)?;
        run.apply(AgentEvent::RepairReady)?;
        assert_eq!(run.automatic_repairs(), expected_repairs);
        run.apply(AgentEvent::CandidateGenerated)?;
    }

    run.apply(AgentEvent::SchemaInvalid)?;
    let record = run.apply(AgentEvent::RepairReady)?;
    assert_eq!(record.outcome(), TransitionOutcome::RepairBudgetExhausted);
    assert_eq!(
        record.outcome().diagnostic_code(),
        Some("LW_AGENT_REPAIR_BUDGET_EXHAUSTED")
    );
    assert_eq!(
        record.diagnostic_code(),
        Some("LW_AGENT_REPAIR_BUDGET_EXHAUSTED")
    );
    assert_eq!(run.state(), AgentState::Failed);
    assert_eq!(run.automatic_repairs(), 2);
    Ok(())
}

#[test]
fn cancellation_is_terminal_and_audited() -> Result<(), Box<dyn Error>> {
    let mut run = AgentRun::new("run-cancel")?;
    run.apply(AgentEvent::Parsed)?;
    let record = run.apply(AgentEvent::Cancel)?;
    assert_eq!(record.to(), AgentState::Cancelled);
    assert_eq!(record.outcome(), TransitionOutcome::Cancelled);
    assert_eq!(record.diagnostic_code(), Some("LW_AGENT_RUN_CANCELLED"));
    assert_eq!(record.sequence(), 2);

    let Err(error) = run.apply(AgentEvent::CapabilitiesRetrieved) else {
        return Err("terminal run accepted another transition".into());
    };
    assert_eq!(error.diagnostic_code(), "LW_AGENT_TRANSITION_INVALID");
    assert_eq!(run.history().len(), 2);
    Ok(())
}

#[test]
fn every_active_stage_can_fail_fast_with_its_root_cause() -> Result<(), Box<dyn Error>> {
    let mut run = AgentRun::new("run-failure")?;
    for event in [
        AgentEvent::Parsed,
        AgentEvent::CapabilitiesRetrieved,
        AgentEvent::PlanCreated,
        AgentEvent::CandidateGenerated,
        AgentEvent::SchemaValid,
        AgentEvent::PolicyAllowed,
        AgentEvent::ValidationExecuted,
        AgentEvent::VerificationPassed,
    ] {
        assert_fails_fast(run.clone())?;
        if run.state() == AgentState::SchemaValidate {
            let mut repair = run.clone();
            repair.apply(AgentEvent::SchemaInvalid)?;
            assert_fails_fast(repair)?;
        }
        if run.state() == AgentState::PolicyValidate {
            let mut approval = run.clone();
            approval.apply(AgentEvent::ApprovalRequired)?;
            assert_fails_fast(approval)?;
        }
        run.apply(event)?;
    }
    assert_eq!(run.state(), AgentState::AwaitingReleaseApproval);
    assert_fails_fast(run)?;
    Ok(())
}

fn assert_fails_fast(mut run: AgentRun) -> Result<(), Box<dyn Error>> {
    let expected_state = run.state();
    let record = run.apply_failure("LW_TEST_STAGE_FAILED")?;
    assert_eq!(record.from(), expected_state);
    assert_eq!(record.event(), AgentEvent::FailureRecorded);
    assert_eq!(record.to(), AgentState::Failed);
    assert_eq!(record.outcome(), TransitionOutcome::FailedFast);
    assert_eq!(record.diagnostic_code(), Some("LW_TEST_STAGE_FAILED"));
    assert_eq!(run.state(), AgentState::Failed);
    Ok(())
}

#[test]
fn invalid_failure_diagnostic_is_rejected_atomically() -> Result<(), Box<dyn Error>> {
    let mut run = AgentRun::new("run-invalid-diagnostic")?;
    let Err(error) = run.apply_failure("tool failed") else {
        return Err("invalid failure diagnostic unexpectedly passed".into());
    };
    assert_eq!(
        error.diagnostic_code(),
        "LW_AGENT_FAILURE_DIAGNOSTIC_INVALID"
    );
    assert_eq!(run.state(), AgentState::Parse);
    assert!(run.history().is_empty());
    Ok(())
}

#[test]
fn run_identity_is_required() -> Result<(), Box<dyn Error>> {
    let Err(error) = AgentRun::new("  ") else {
        return Err("empty run identity unexpectedly passed".into());
    };
    assert_eq!(error.diagnostic_code(), "LW_AGENT_RUN_ID_INVALID");
    Ok(())
}

fn drive_to_schema_validation(run: &mut AgentRun) -> Result<(), Box<dyn Error>> {
    for event in [
        AgentEvent::Parsed,
        AgentEvent::CapabilitiesRetrieved,
        AgentEvent::PlanCreated,
        AgentEvent::CandidateGenerated,
    ] {
        run.apply(event)?;
    }
    Ok(())
}
