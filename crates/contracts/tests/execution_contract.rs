//! Regression coverage for the one-shot task execution admission contract.

use contracts::execution::{
    ExecutionCleanupStatus, ExecutionContractError, ExecutionObjectRef, ExecutionObservation,
    ExecutionWorkloadState, TaskExecutionBinding,
};
use contracts::http::TaskResourceStatus;
use contracts::resource::{
    CapacityClaim, CapacityClaimState, ResourceLease, ResourceLeaseState, ResourceRequest,
    ResourceRequestState, ResourceTarget, WorkloadResources,
};
use contracts::{
    ActorId, CapacityClaimId, DiagnosticCode, LeaseId, ProjectId, ResourceApprovalId,
    ResourceRequestId, Revision, TaskRunId, UtcTimestamp,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const NAMESPACE: &str = "labweaver-evaluation";

fn timestamp(value: &str) -> Result<UtcTimestamp, Box<dyn std::error::Error>> {
    Ok(value.parse()?)
}

fn workload_resources() -> WorkloadResources {
    WorkloadResources {
        cpu_millicores: 1_000,
        memory_bytes: 1 << 30,
        storage_bytes: 1 << 30,
        gpu: None,
    }
}

fn admitted_status(
    task_run_id: TaskRunId,
) -> Result<TaskResourceStatus, Box<dyn std::error::Error>> {
    let request_id = ResourceRequestId::new();
    let claim_id = CapacityClaimId::new();
    let lease_id = LeaseId::new();
    let project_id = ProjectId::new();
    let owner_id = ActorId::new();
    let created_at = timestamp("2026-09-19T07:00:00.000Z")?;
    let active_from = timestamp("2026-09-19T07:00:00.000Z")?;
    let expires_at = timestamp("2026-09-19T08:00:00.000Z")?;
    let revision = Revision::new(2)?;
    Ok(TaskResourceStatus {
        task_run_id,
        project_id,
        owner_id,
        execution_namespace: Some(NAMESPACE.to_owned()),
        claim_revision: revision,
        lease_revision: revision,
        cleanup_confirmed: false,
        request: ResourceRequest {
            id: request_id,
            generation: 1,
            request_key: format!("evaluation-{task_run_id}"),
            requester_id: owner_id,
            project_id,
            course_id: None,
            target: ResourceTarget::Task { task_run_id },
            requested_resources: workload_resources(),
            requested_duration_seconds: 600,
            state: ResourceRequestState::Active,
            revision,
            created_at,
            updated_at: created_at,
            diagnostic_code: None,
        },
        claim: CapacityClaim {
            id: claim_id,
            request_id,
            approval_id: ResourceApprovalId::new(),
            provider_binding: "kubernetes-job".to_owned(),
            workload_resources: workload_resources(),
            quota_resources: workload_resources(),
            gpu_allocation: None,
            state: CapacityClaimState::HandedOff,
            revision,
        },
        lease: ResourceLease {
            id: lease_id,
            request_id,
            claim_id,
            state: ResourceLeaseState::Active,
            revision,
            active_from: Some(active_from),
            expires_at: Some(expires_at),
            revoke_reason_code: None,
            created_at,
            updated_at: created_at,
        },
    })
}

fn admitted_binding()
-> Result<(TaskExecutionBinding, TaskResourceStatus), Box<dyn std::error::Error>> {
    let status = admitted_status(TaskRunId::new())?;
    let binding = TaskExecutionBinding::from_admitted_status(
        &status,
        1,
        "lw-oj-0123456789abcdef0123",
        "trace-res-04",
    )?;
    Ok((binding, status))
}

#[test]
fn admitted_status_binds_the_exact_reservation() -> TestResult {
    let (binding, status) = admitted_binding()?;
    assert_eq!(binding.task_run_id, status.task_run_id);
    assert_eq!(binding.execution_generation, 1);
    assert_eq!(binding.resource_request_id, status.request.id);
    assert_eq!(binding.capacity_claim_id, status.claim.id);
    assert_eq!(binding.lease_id, status.lease.id);
    assert_eq!(binding.claim_revision, status.claim_revision);
    assert_eq!(binding.lease_revision, status.lease_revision);
    assert_eq!(binding.namespace, NAMESPACE);
    assert_eq!(binding.provider_binding, "kubernetes-job");
    assert!(binding.same_reservation(&status));
    assert!(binding.matches_admitted_status(&status));

    let encoded = serde_json::to_value(&binding)?;
    assert_eq!(encoded["executionGeneration"], 1);
    assert_eq!(encoded["namespace"], NAMESPACE);
    assert_eq!(encoded["taskRunId"], binding.task_run_id.to_string());
    let decoded: TaskExecutionBinding = serde_json::from_value(encoded.clone())?;
    assert_eq!(decoded, binding);

    let mut unknown = encoded;
    unknown["ownerId"] = serde_json::json!(ActorId::new().to_string());
    assert!(serde_json::from_value::<TaskExecutionBinding>(unknown).is_err());
    Ok(())
}

#[test]
fn reviewing_request_is_not_admitted() -> TestResult {
    let mut status = admitted_status(TaskRunId::new())?;
    status.request.state = ResourceRequestState::Reviewing;
    assert!(matches!(
        TaskExecutionBinding::from_admitted_status(&status, 1, "lw-oj-1", "trace"),
        Err(ExecutionContractError::AdmissionStateMismatch)
    ));
    Ok(())
}

#[test]
fn blocked_claim_is_not_admitted() -> TestResult {
    let mut status = admitted_status(TaskRunId::new())?;
    status.claim.state = CapacityClaimState::Blocked;
    assert!(matches!(
        TaskExecutionBinding::from_admitted_status(&status, 1, "lw-oj-1", "trace"),
        Err(ExecutionContractError::AdmissionStateMismatch)
    ));
    Ok(())
}

#[test]
fn revoked_lease_is_not_admitted() -> TestResult {
    let mut status = admitted_status(TaskRunId::new())?;
    status.lease.state = ResourceLeaseState::Revoked;
    status.lease.revoke_reason_code = Some("administrator_revoke".to_owned());
    assert!(matches!(
        TaskExecutionBinding::from_admitted_status(&status, 1, "lw-oj-1", "trace"),
        Err(ExecutionContractError::AdmissionStateMismatch)
    ));
    Ok(())
}

#[test]
fn missing_execution_namespace_is_not_admitted() -> TestResult {
    let mut status = admitted_status(TaskRunId::new())?;
    status.execution_namespace = None;
    assert!(matches!(
        TaskExecutionBinding::from_admitted_status(&status, 1, "lw-oj-1", "trace"),
        Err(ExecutionContractError::AdmissionStateMismatch)
    ));
    Ok(())
}

#[test]
fn lease_revision_mismatch_is_not_admitted() -> TestResult {
    let mut status = admitted_status(TaskRunId::new())?;
    status.lease.revision = Revision::new(3)?;
    assert!(matches!(
        TaskExecutionBinding::from_admitted_status(&status, 1, "lw-oj-1", "trace"),
        Err(ExecutionContractError::AdmissionStateMismatch)
    ));
    Ok(())
}

#[test]
fn lease_renewal_keeps_reservation_but_requires_rebinding() -> TestResult {
    let (binding, mut status) = admitted_binding()?;
    status.lease.revision = Revision::new(7)?;
    status.lease_revision = Revision::new(7)?;
    assert!(binding.same_reservation(&status));
    assert!(!binding.matches_admitted_status(&status));
    Ok(())
}

#[test]
fn binding_rejects_zero_generation_and_malformed_identity() -> TestResult {
    let (mut binding, _) = admitted_binding()?;
    binding.execution_generation = 0;
    assert!(matches!(
        binding.validate(),
        Err(ExecutionContractError::InvalidBinding)
    ));

    let (mut binding, _) = admitted_binding()?;
    binding.namespace = "LabWeaver_Evaluation".to_owned();
    assert!(matches!(
        binding.validate(),
        Err(ExecutionContractError::InvalidBinding)
    ));

    let (mut binding, _) = admitted_binding()?;
    binding.workload_name = "-leading-dash".to_owned();
    assert!(matches!(
        binding.validate(),
        Err(ExecutionContractError::InvalidBinding)
    ));

    let (mut binding, _) = admitted_binding()?;
    binding.trace_id = String::new();
    assert!(matches!(
        binding.validate(),
        Err(ExecutionContractError::InvalidBinding)
    ));
    Ok(())
}

#[test]
fn observation_requires_consistent_terminal_state() -> TestResult {
    let started_at = timestamp("2026-09-19T07:00:01.000Z")?;
    let terminated_at = timestamp("2026-09-19T07:00:02.000Z")?;
    let succeeded = ExecutionObservation {
        state: ExecutionWorkloadState::Succeeded,
        exit_code: Some(0),
        reason_code: Some("Completed".to_owned()),
        pod_name: Some("lw-oj-0123456789abcdef0123-abcde".to_owned()),
        started_at: Some(started_at),
        terminated_at: Some(terminated_at),
    };
    succeeded.validate()?;
    assert!(succeeded.state.is_terminal());

    let mut failed_without_exit = succeeded.clone();
    failed_without_exit.state = ExecutionWorkloadState::Failed;
    failed_without_exit.exit_code = None;
    assert!(matches!(
        failed_without_exit.validate(),
        Err(ExecutionContractError::InvalidObservation)
    ));

    let mut running_with_exit = succeeded.clone();
    running_with_exit.state = ExecutionWorkloadState::Running;
    assert!(matches!(
        running_with_exit.validate(),
        Err(ExecutionContractError::InvalidObservation)
    ));

    let mut missing_with_exit = succeeded.clone();
    missing_with_exit.state = ExecutionWorkloadState::Missing;
    assert!(matches!(
        missing_with_exit.validate(),
        Err(ExecutionContractError::InvalidObservation)
    ));

    let mut malformed_reason = succeeded.clone();
    malformed_reason.reason_code = Some("completed successfully".to_owned());
    assert!(matches!(
        malformed_reason.validate(),
        Err(ExecutionContractError::InvalidObservation)
    ));

    let mut unpaired_timing = succeeded;
    unpaired_timing.terminated_at = None;
    assert!(matches!(
        unpaired_timing.validate(),
        Err(ExecutionContractError::InvalidObservation)
    ));
    Ok(())
}

#[test]
fn cleanup_status_only_confirms_verified_deletion() -> TestResult {
    let confirmed = ExecutionCleanupStatus::Confirmed;
    confirmed.validate()?;
    assert!(confirmed.is_confirmed());
    assert_eq!(
        serde_json::to_value(&confirmed)?,
        serde_json::json!({"status": "confirmed"})
    );

    let object = ExecutionObjectRef {
        api_version: "batch/v1".to_owned(),
        resource: "jobs".to_owned(),
        name: "lw-oj-0123456789abcdef0123".to_owned(),
        uid: "8f0e1f0e-2f7a-4c1b-9d3a-2b3c4d5e6f70".to_owned(),
    };
    object.validate()?;
    let pending = ExecutionCleanupStatus::Pending {
        remaining_objects: vec![object],
    };
    pending.validate()?;
    assert!(!pending.is_confirmed());

    let empty_pending = ExecutionCleanupStatus::Pending {
        remaining_objects: Vec::new(),
    };
    assert!(matches!(
        empty_pending.validate(),
        Err(ExecutionContractError::InvalidCleanupStatus)
    ));

    let unknown = ExecutionCleanupStatus::Unknown {
        diagnostic: DiagnosticCode::parse("LW_EXECUTION_CLEANUP_UNKNOWN")?,
    };
    unknown.validate()?;
    assert!(!unknown.is_confirmed());
    let encoded = serde_json::to_value(&unknown)?;
    assert_eq!(encoded["status"], "unknown");
    assert_eq!(encoded["diagnostic"], "LW_EXECUTION_CLEANUP_UNKNOWN");
    Ok(())
}
