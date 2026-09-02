//! Pure AgentRun orchestration helpers extracted from run_store.rs.
//!
//! Validation, checkpoint construction, and attempt tracking helpers that have no SQL dependency.

use std::time::Duration;

use contracts::authoring::{
    AgentAttempt, AgentAttemptState, AgentRun, AgentRunState, AgentTrack, AgentTrackKind,
    EnvironmentCandidate, EvaluationCandidate,
};
use contracts::diagnostic;
use contracts::events::subjects;
use contracts::http::CreateAgentRunRequest;
use contracts::{AgentRunId, CandidateId, CourseId, Revision, UtcTimestamp};

use persistence_sqlx::Sha256Digest;

use crate::claude_code::{
    CandidateDocument, ClaudeCodeExecution, ClaudeCodeFailure, RuntimeAuditOutcome,
};

use super::run_store::{
    AgentRunStoreError, ExecuteAgentRun, ReserveAgentRun, AgentTrackCheckpoint, StoredCandidate, TrackClaim, zero_usage,
};

/// Append a newly claimed attempt to the AgentRun record.
pub(crate) fn append_claimed_attempt(
    run: &mut AgentRun,
    track_kind: AgentTrackKind,
    _input_sha256: Sha256Digest,
    claim: &TrackClaim,
) -> Result<u32, AgentRunStoreError> {
    let track = run
        .tracks
        .iter_mut()
        .find(|track| track.kind == track_kind)
        .ok_or(AgentRunStoreError::InvalidContract)?;
    if claim.state == "running" {
        let previous = track
            .attempts
            .last_mut()
            .filter(|attempt| attempt.state == AgentAttemptState::Running)
            .ok_or(AgentRunStoreError::InvalidContract)?;
        previous.state = AgentAttemptState::Failed;
        previous.diagnostic_code = Some(diagnostic::PROVIDER_UNAVAILABLE.to_owned());
    }
    let attempt = u32::try_from(track.attempts.len().saturating_add(1))
        .map_err(|_| AgentRunStoreError::InvalidContract)?;
    if i64::from(attempt) != claim.attempt_number.saturating_add(1) {
        return Err(AgentRunStoreError::InvalidContract);
    }
    track.attempts.push(AgentAttempt {
        number: attempt,
        state: AgentAttemptState::Running,
        checkpoint: None,
        usage: zero_usage(),
        usage_observed: false,
        diagnostic_code: None,
    });
    let derived = run
        .derived_state()
        .map_err(|_| AgentRunStoreError::InvalidContract)?;
    run.state = if claim.cancellation_requested && derived == AgentRunState::Running {
        AgentRunState::Cancelling
    } else {
        derived
    };
    run.revision = run
        .revision
        .next()
        .ok_or(AgentRunStoreError::InvalidContract)?;
    run.validate()
        .map_err(|_| AgentRunStoreError::InvalidContract)?;
    Ok(attempt)
}

/// Validate a reservation input before creating an idempotent AgentRun.
pub(crate) fn validate_reservation(command: &ReserveAgentRun<'_>) -> Result<(), AgentRunStoreError> {
    if command.trace_id.trim().is_empty()
        || command.course_id != command.input.course_id()
        || command.request.package_id != command.input.package_id()
        || command.request.package_revision != command.input.package_revision()
        || command.request.policy_id != command.policy.id
        || command.request.policy_revision != command.policy.revision
        || command.input.policy_id() != command.policy.id
        || command.input.policy_revision() != command.policy.revision
        || command.policy.course_id != command.course_id
    {
        return Err(AgentRunStoreError::IdentityMismatch);
    }
    command
        .policy
        .validate()
        .map_err(|_| AgentRunStoreError::IdentityMismatch)
}

/// Validate that the reserved run matches the execution command.
pub(crate) fn validate_reserved_run(
    command: &ExecuteAgentRun<'_>,
    run: &AgentRun,
) -> Result<(), AgentRunStoreError> {
    if run.course_id != command.course_id
        || run.package_id != command.request.package_id
        || run.policy_id != command.request.policy_id
        || run.state == AgentRunState::Failed
        || run.state == AgentRunState::Cancelled
    {
        return Err(AgentRunStoreError::IdentityMismatch);
    }
    if command.trace_id.trim().is_empty()
        || command.input.course_id() != command.course_id
        || command.input.package_id() != command.request.package_id
        || command.input.package_revision() != command.request.package_revision
    {
        return Err(AgentRunStoreError::IdentityMismatch);
    }
    Ok(())
}

/// Construct the initial AgentRun from a create request.
pub(crate) fn requested_run(
    request: &CreateAgentRunRequest,
    course_id: CourseId,
) -> Result<AgentRun, AgentRunStoreError> {
    let run = AgentRun {
        id: AgentRunId::new(),
        course_id,
        package_id: request.package_id,
        policy_id: request.policy_id,
        policy_revision: request.policy_revision,
        requested_runtime: request.requested_runtime,
        state: AgentRunState::Requested,
        revision: Revision::new(1).map_err(|_| AgentRunStoreError::InvalidContract)?,
        tracks: vec![
            AgentTrack {
                kind: AgentTrackKind::Environment,
                attempts: Vec::new(),
                candidate_id: None,
            },
            AgentTrack {
                kind: AgentTrackKind::Evaluation,
                attempts: Vec::new(),
                candidate_id: None,
            },
        ],
    };
    run.validate()
        .map_err(|_| AgentRunStoreError::InvalidContract)?;
    Ok(run)
}

/// Derive an environment checkpoint from execution result.
pub(crate) fn environment_checkpoint(
    run: &AgentRun,
    attempt: u32,
    result: Result<ClaudeCodeExecution, ClaudeCodeFailure>,
    now: UtcTimestamp,
) -> Result<AgentTrackCheckpoint, AgentRunStoreError> {
    match result {
        Ok(execution) => {
            let CandidateDocument::Environment(spec) = execution.document else {
                return Err(AgentRunStoreError::InvalidContract);
            };
            if spec.runtime.kind() != run.requested_runtime {
                let mut audit = execution.audit;
                audit.outcome = RuntimeAuditOutcome::Failed;
                audit.diagnostic_code = Some(diagnostic::EVIDENCE_INVALID.to_owned());
                return Ok(AgentTrackCheckpoint {
                    run_id: run.id,
                    sequence: checkpoint_sequence(AgentTrackKind::Environment, attempt)?,
                    track: AgentTrackKind::Environment,
                    attempt,
                    audit,
                    candidate: None,
                });
            }
            let candidate = EnvironmentCandidate {
                id: CandidateId::new(),
                run_id: run.id,
                revision: Revision::new(1).map_err(|_| AgentRunStoreError::InvalidContract)?,
                spec,
                policy_revision: run.policy_revision,
                model: execution.audit.model.clone(),
                created_at: now,
            };
            candidate
                .validate()
                .map_err(|_| AgentRunStoreError::InvalidContract)?;
            Ok(AgentTrackCheckpoint {
                run_id: run.id,
                sequence: checkpoint_sequence(AgentTrackKind::Environment, attempt)?,
                track: AgentTrackKind::Environment,
                attempt,
                audit: execution.audit,
                candidate: Some(StoredCandidate::Environment(candidate)),
            })
        }
        Err(failure) => Ok(AgentTrackCheckpoint {
            run_id: run.id,
            sequence: checkpoint_sequence(AgentTrackKind::Environment, attempt)?,
            track: AgentTrackKind::Environment,
            attempt,
            audit: failure.audit().clone(),
            candidate: None,
        }),
    }
}

/// Derive an evaluation checkpoint from execution result.
pub(crate) fn evaluation_checkpoint(
    run: &AgentRun,
    attempt: u32,
    result: Result<ClaudeCodeExecution, ClaudeCodeFailure>,
    now: UtcTimestamp,
) -> Result<AgentTrackCheckpoint, AgentRunStoreError> {
    match result {
        Ok(execution) => {
            let CandidateDocument::Evaluation(spec) = execution.document else {
                return Err(AgentRunStoreError::InvalidContract);
            };
            let candidate = EvaluationCandidate {
                id: CandidateId::new(),
                run_id: run.id,
                revision: Revision::new(1).map_err(|_| AgentRunStoreError::InvalidContract)?,
                spec,
                policy_revision: run.policy_revision,
                model: execution.audit.model.clone(),
                created_at: now,
            };
            candidate
                .validate()
                .map_err(|_| AgentRunStoreError::InvalidContract)?;
            Ok(AgentTrackCheckpoint {
                run_id: run.id,
                sequence: checkpoint_sequence(AgentTrackKind::Evaluation, attempt)?,
                track: AgentTrackKind::Evaluation,
                attempt,
                audit: execution.audit,
                candidate: Some(StoredCandidate::Evaluation(candidate)),
            })
        }
        Err(failure) => Ok(AgentTrackCheckpoint {
            run_id: run.id,
            sequence: checkpoint_sequence(AgentTrackKind::Evaluation, attempt)?,
            track: AgentTrackKind::Evaluation,
            attempt,
            audit: failure.audit().clone(),
            candidate: None,
        }),
    }
}

/// Apply a committed checkpoint to the in-memory AgentRun record.
pub(crate) fn apply_checkpoint(
    run: &mut AgentRun,
    checkpoint: &AgentTrackCheckpoint,
) -> Result<(), AgentRunStoreError> {
    let track = run
        .tracks
        .iter_mut()
        .find(|track| track.kind == checkpoint.track)
        .ok_or(AgentRunStoreError::InvalidContract)?;
    let attempt = track
        .attempts
        .last_mut()
        .ok_or(AgentRunStoreError::InvalidContract)?;
    if attempt.number != checkpoint.attempt
        || checkpoint.audit.track != checkpoint.track
        || checkpoint.audit.course_id != run.course_id
        || checkpoint.audit.package_id != run.package_id
        || checkpoint.audit.policy_id != run.policy_id
        || checkpoint.audit.policy_revision != run.policy_revision
    {
        return Err(AgentRunStoreError::IdentityMismatch);
    }
    attempt.usage = checkpoint.audit.usage;
    attempt.usage_observed = checkpoint.audit.usage_observed;
    if let Some(candidate) = &checkpoint.candidate {
        attempt.state = AgentAttemptState::Succeeded;
        attempt.diagnostic_code = None;
        track.candidate_id = Some(match candidate {
            StoredCandidate::Environment(candidate) => candidate.id,
            StoredCandidate::Evaluation(candidate) => candidate.id,
        });
    } else {
        attempt.state = if checkpoint.audit.outcome == RuntimeAuditOutcome::Cancelled {
            AgentAttemptState::Cancelled
        } else {
            AgentAttemptState::Failed
        };
        attempt
            .diagnostic_code
            .clone_from(&checkpoint.audit.diagnostic_code);
    }
    Ok(())
}

/// Serialize checkpoint state for logging and events.
#[must_use]
pub(crate) fn checkpoint_state(checkpoint: &AgentTrackCheckpoint) -> &'static str {
    if checkpoint.candidate.is_some() {
        "succeeded"
    } else if checkpoint.audit.outcome == RuntimeAuditOutcome::Cancelled {
        "cancelled"
    } else {
        "failed"
    }
}

/// Derive the terminal event subject and diagnostic from the run state.
#[must_use]
pub(crate) fn terminal_event(run: &AgentRun) -> (&'static str, Option<&str>) {
    if matches!(
        run.state,
        AgentRunState::Succeeded | AgentRunState::PartiallySucceeded
    ) {
        (subjects::AGENT_RUN_COMPLETED, None)
    } else {
        (
            subjects::AGENT_RUN_FAILED,
            run.tracks
                .iter()
                .filter_map(|track| track.attempts.last())
                .find_map(|attempt| attempt.diagnostic_code.as_deref()),
        )
    }
}

/// Compute the outbox sequence number for a checkpoint event.
pub(crate) fn checkpoint_sequence(
    track: AgentTrackKind,
    attempt: u32,
) -> Result<u64, AgentRunStoreError> {
    let base = u64::from(
        attempt
            .checked_sub(1)
            .ok_or(AgentRunStoreError::InvalidContract)?,
    )
    .checked_mul(2)
    .ok_or(AgentRunStoreError::InvalidContract)?;
    base.checked_add(match track {
        AgentTrackKind::Environment => 1,
        AgentTrackKind::Evaluation => 2,
    })
    .ok_or(AgentRunStoreError::InvalidContract)
}

/// Validate the worker identity string and lease duration.
pub(crate) fn validate_worker(
    worker_id: &str,
    lease_duration: Duration,
) -> Result<(), AgentRunStoreError> {
    if worker_id.is_empty()
        || worker_id.len() > 256
        || !worker_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
        || lease_duration.is_zero()
        || lease_duration > Duration::from_hours(1)
    {
        return Err(AgentRunStoreError::WorkerIdentityInvalid);
    }
    Ok(())
}

/// Convert lease duration to PostgreSQL-compatible milliseconds.
pub(crate) fn lease_milliseconds(lease_duration: Duration) -> Result<i64, AgentRunStoreError> {
    i64::try_from(lease_duration.as_millis()).map_err(|_| AgentRunStoreError::WorkerIdentityInvalid)
}
