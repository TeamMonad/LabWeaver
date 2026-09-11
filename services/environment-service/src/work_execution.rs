//! Environment owned execution of bounded configuration scripts in existing Work Pods.
//!
//! The execution row is the durable fence.  A request which already has a row is
//! observed and returned; it is never replayed after a process restart.

use std::{fmt::Write as _, str::FromStr, sync::Arc, time::Duration};

use async_trait::async_trait;
use contracts::http::{
    ContainerWorkExecutionQuery, ContainerWorkExecutionReceipt, ContainerWorkExecutionRequest,
    ContainerWorkExecutionState, WorkConfigurationAdmissionBinding,
    WorkConfigurationAdmissionQuery,
};
use contracts::{ActorId, AgentRunId, ProjectId, Revision, UtcTimestamp};
use k8s_openapi::api::core::v1::Pod;
use kube::{
    Api, Client,
    api::{AttachParams, ListParams},
};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

use crate::{PgEnvironmentStore, WorkAdmissionClient, WorkAdmissionResolver};

const RUNTIME_CONTAINER: &str = "runtime";
const WORKSPACE_ROOT: &str = "/workspace";
const EXECUTION_ROOT: &str = "/tmp/labweaver-work-executions";
const CANCEL_TIMEOUT: Duration = Duration::from_secs(8);
const RUNNER_SCRIPT: &[u8] = include_bytes!("../../work-configuration-runner.sh");

/// Target selected from the current Work Pod immediately before a new execution.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContainerWorkExecutionTarget {
    pub namespace: String,
    pub pod_name: String,
    pub pod_uid: String,
    pub container: String,
    pub workdir: String,
}

/// Result of one fixed command executed in a Work Pod.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkExecutionOutcome {
    pub exit_code: i32,
    pub verification_exit_code: Option<i32>,
    pub output: String,
    pub output_truncated: bool,
}

/// Backend seam kept small so durable execution semantics can be tested without Kubernetes.
#[async_trait]
pub trait ContainerWorkExecutionBackend: Send + Sync {
    async fn resolve_target(
        &self,
        request: &ContainerWorkExecutionRequest,
    ) -> Result<ContainerWorkExecutionTarget, WorkExecutionError>;

    async fn execute(
        &self,
        target: &ContainerWorkExecutionTarget,
        execution_id: Uuid,
        script: &str,
        verification_script: Option<&str>,
        deadline_at: UtcTimestamp,
    ) -> Result<WorkExecutionOutcome, WorkExecutionError>;

    async fn cancel(
        &self,
        target: &ContainerWorkExecutionTarget,
        execution_id: Uuid,
        deadline_at: UtcTimestamp,
    ) -> Result<(), WorkExecutionError>;

    async fn observe(
        &self,
        target: &ContainerWorkExecutionTarget,
        execution_id: Uuid,
        verification_required: bool,
    ) -> Result<Option<WorkExecutionOutcome>, WorkExecutionError>;

    /// Reads the runner's fixed prestart-failure marker after a successful
    /// cancellation handshake.
    ///
    /// The marker is deliberately not trusted on its own: callers must first
    /// complete `cancel`, which serializes with the runner's launch decision.
    /// Backends that do not expose the marker keep the conservative `None`
    /// behavior and continue through normal observation/cleanup recovery.
    async fn prestart_failure(
        &self,
        _target: &ContainerWorkExecutionTarget,
        _execution_id: Uuid,
    ) -> Result<Option<String>, WorkExecutionError> {
        Ok(None)
    }
}

/// Durable owner for existing Work configuration execution.
#[derive(Clone)]
pub struct ContainerWorkExecutionService {
    store: PgEnvironmentStore,
    pool: PgPool,
    admission: Arc<dyn WorkAdmissionResolver>,
    backend: Arc<dyn ContainerWorkExecutionBackend>,
}

impl ContainerWorkExecutionService {
    pub fn new(
        store: PgEnvironmentStore,
        pool: PgPool,
        admission: WorkAdmissionClient,
        backend: Arc<dyn ContainerWorkExecutionBackend>,
    ) -> Self {
        Self::new_with_admission_resolver(store, pool, admission, backend)
    }

    /// Builds an execution service with an explicit admission resolver.
    ///
    /// Production passes [`WorkAdmissionClient`]. Tests and alternate local
    /// transports can provide the same Control contract without changing the
    /// durable execution state machine.
    pub fn new_with_admission_resolver<A>(
        store: PgEnvironmentStore,
        pool: PgPool,
        admission: A,
        backend: Arc<dyn ContainerWorkExecutionBackend>,
    ) -> Self
    where
        A: WorkAdmissionResolver + 'static,
    {
        Self {
            store,
            pool,
            admission: Arc::new(admission),
            backend,
        }
    }

    /// Accepts one request.  The durable row is committed before the backend is called.
    pub async fn start(
        &self,
        request: ContainerWorkExecutionRequest,
    ) -> Result<ContainerWorkExecutionReceipt, WorkExecutionError> {
        request
            .validate()
            .map_err(|_| WorkExecutionError::RequestInvalid)?;
        // An exact replay is answered from the durable row before checking
        // live admission or resolving a Pod.  Admission may have expired (or
        // been revoked) while the original execution is still running; that
        // must not turn an idempotent retry into a second admission attempt.
        if let Some(existing) = load_execution_optional(&self.pool, request.run_id).await? {
            if existing.request != request
                || existing.receipt.run_id != request.run_id
                || existing.receipt.validate().is_err()
            {
                return Err(WorkExecutionError::IdentityMismatch);
            }
            return Ok(existing.receipt);
        }
        let now = current_time(&self.pool).await?;
        let instance = self.store.load(request.environment_id).await?;
        validate_work_environment(
            &instance,
            request.project_id,
            request.course_id,
            request.actor_id,
            request.environment_revision,
            now,
        )?;
        if instance.runtime_kind != contracts::authoring::RuntimeKind::Container {
            return Err(WorkExecutionError::EnvironmentNotEligible);
        }
        let admission = self
            .admission
            .resolve(
                request.run_id,
                &WorkConfigurationAdmissionQuery {
                    project_id: request.project_id,
                    course_id: request.course_id,
                    environment_id: request.environment_id,
                    environment_revision: request.environment_revision,
                    actor_id: request.actor_id,
                    run_revision: request.run_revision,
                    execution_id: None,
                },
                now,
            )
            .await
            .map_err(WorkExecutionError::Admission)?;
        verify_admission(&request, &admission)?;
        if request.deadline_at <= now {
            return Err(WorkExecutionError::DeadlineExceeded);
        }

        let target = self.backend.resolve_target(&request).await?;
        let execution_id = Uuid::now_v7();
        let receipt = ContainerWorkExecutionReceipt {
            execution_id,
            run_id: request.run_id,
            plan_id: request.plan_id,
            plan_revision: request.plan_revision,
            environment_id: request.environment_id,
            environment_revision: request.environment_revision,
            target_pod_uid: target.pod_uid.clone(),
            state: ContainerWorkExecutionState::Running,
            exit_code: None,
            verification_exit_code: None,
            output: String::new(),
            output_truncated: false,
            diagnostic_code: None,
            started_at: None,
            finished_at: None,
        };
        let inserted = insert_execution(&self.pool, &request, &target, &receipt).await?;
        if let Some(existing) = inserted {
            return Ok(existing);
        }
        let service = self.clone();
        let background_receipt = receipt.clone();
        tokio::spawn(async move {
            if let Err(error) = service
                .run_backend(request, target, background_receipt)
                .await
            {
                tracing::error!(
                    event = "environment.work_execution.failed",
                    error = %error,
                    error_kind = "execution",
                    retryable = false
                );
            }
        });
        Ok(receipt)
    }

    /// Reads a durable receipt after checking its full project/environment/plan identity.
    pub async fn query(
        &self,
        run_id: AgentRunId,
        query: &ContainerWorkExecutionQuery,
    ) -> Result<ContainerWorkExecutionReceipt, WorkExecutionError> {
        let record = load_execution(&self.pool, run_id).await?;
        if !record_matches_query(&record, run_id, query) {
            return Err(WorkExecutionError::IdentityMismatch);
        }
        Ok(record.receipt)
    }

    /// Requests process-group termination and records the terminal observation.
    pub async fn cancel(
        &self,
        run_id: AgentRunId,
        query: &ContainerWorkExecutionQuery,
    ) -> Result<ContainerWorkExecutionReceipt, WorkExecutionError> {
        let mut record = load_execution(&self.pool, run_id).await?;
        if !record_matches_query(&record, run_id, query) {
            return Err(WorkExecutionError::IdentityMismatch);
        }

        // Retry the state transition after a concurrent start fence.  Returning
        // the stale Running receipt would acknowledge cancellation without
        // ever sending a signal to the process group.
        loop {
            match record.receipt.state {
                ContainerWorkExecutionState::Succeeded
                | ContainerWorkExecutionState::Failed
                | ContainerWorkExecutionState::Cancelled
                | ContainerWorkExecutionState::CleanupFailed => return Ok(record.receipt),
                ContainerWorkExecutionState::Running
                | ContainerWorkExecutionState::Cancelling
                | ContainerWorkExecutionState::CleanupPending => {}
            }

            let deadline_expired = record.request.deadline_at <= current_time(&self.pool).await?;
            if record.receipt.state == ContainerWorkExecutionState::Running {
                let mut cancelling = record.receipt.clone();
                cancelling.state = ContainerWorkExecutionState::Cancelling;
                cancelling.finished_at = None;
                if !update_receipt(&self.pool, run_id, record.revision, &cancelling).await? {
                    record = load_execution(&self.pool, run_id).await?;
                    if !record_matches_query(&record, run_id, query) {
                        return Err(WorkExecutionError::IdentityMismatch);
                    }
                    continue;
                }
                record.receipt = cancelling;
                record.revision += 1;
            }

            return self.finish_cancellation(record, deadline_expired).await;
        }
    }

    /// Reconciles one persisted Running/Cancelling execution.  It never invokes the script.
    pub async fn recover_once(
        &self,
        run_id: AgentRunId,
    ) -> Result<ContainerWorkExecutionReceipt, WorkExecutionError> {
        let mut record = load_execution(&self.pool, run_id).await?;
        let deadline_expired = record.request.deadline_at <= current_time(&self.pool).await?;
        match record.receipt.state {
            ContainerWorkExecutionState::Running => {
                if deadline_expired {
                    if record.receipt.started_at.is_none() {
                        return update_failure(
                            &self.pool,
                            run_id,
                            record.revision,
                            record.receipt,
                            "LW_ENVIRONMENT_WORK_EXECUTION_DEADLINE_EXCEEDED",
                        )
                        .await;
                    }
                    let mut cancelling = record.receipt;
                    cancelling.state = ContainerWorkExecutionState::Cancelling;
                    cancelling.finished_at = None;
                    if !update_receipt(&self.pool, run_id, record.revision, &cancelling).await? {
                        return Ok(load_execution(&self.pool, run_id).await?.receipt);
                    }
                    record = ExecutionRecord {
                        request: record.request,
                        target: record.target,
                        receipt: cancelling,
                        revision: record.revision + 1,
                    };
                    return self.finish_cancellation(record, true).await;
                }
                match self
                    .backend
                    .observe(
                        &record.target,
                        record.receipt.execution_id,
                        record.request.verification_script_content.is_some(),
                    )
                    .await
                {
                    Err(error) if terminal_recovery_error(&error) => {
                        self.record_backend_failure(run_id, record.receipt.execution_id, &error)
                            .await?;
                        return Err(error);
                    }
                    Err(error) => return Err(error),
                    Ok(Some(outcome)) => {
                        return update_outcome(
                            &self.pool,
                            run_id,
                            record.revision,
                            record.receipt,
                            outcome,
                            false,
                        )
                        .await;
                    }
                    Ok(None) => {}
                }
            }
            ContainerWorkExecutionState::Cancelling
            | ContainerWorkExecutionState::CleanupPending => {
                // A cancellation request may have been persisted immediately
                // before a process restart.  Recovery must retry the actual
                // process-group kill, not merely poll the old receipt.
                return self.finish_cancellation(record, deadline_expired).await;
            }
            ContainerWorkExecutionState::Succeeded
            | ContainerWorkExecutionState::Failed
            | ContainerWorkExecutionState::Cancelled
            | ContainerWorkExecutionState::CleanupFailed => {}
        }
        Ok(record.receipt)
    }

    /// Lists only non-terminal executions for restart reconciliation.
    pub async fn running_ids(&self) -> Result<Vec<AgentRunId>, WorkExecutionError> {
        let rows = sqlx::query("SELECT run_id FROM environment.work_configuration_executions WHERE receipt_json->>'state' IN ('running','cancelling','cleanup_pending') ORDER BY created_at LIMIT 128")
            .fetch_all(&self.pool).await?;
        rows.into_iter()
            .map(|row| {
                let value: Uuid = row.try_get("run_id")?;
                AgentRunId::from_str(&value.to_string())
                    .map_err(|_| WorkExecutionError::IdentityMismatch)
            })
            .collect()
    }

    #[allow(clippy::too_many_lines)]
    async fn run_backend(
        &self,
        request: ContainerWorkExecutionRequest,
        target: ContainerWorkExecutionTarget,
        receipt: ContainerWorkExecutionReceipt,
    ) -> Result<(), WorkExecutionError> {
        let run_id = request.run_id;
        let execution_id = receipt.execution_id;
        let result = async {
            let started = current_time(&self.pool).await?;
            let current = load_execution(&self.pool, run_id).await?;
            if current.receipt.execution_id != execution_id {
                return Err(WorkExecutionError::ConcurrentMutation);
            }
            if current.receipt.state != ContainerWorkExecutionState::Running {
                // Cancellation won before this worker acquired the start fence.
                return Ok(());
            }
            let receipt =
                update_started(&self.pool, run_id, current.revision, receipt, started).await?;
            let Some(receipt) = receipt else {
                return Ok(());
            };
            let expected_revision = current.revision + 1;
            let outcome = match tokio::time::timeout_at(
                deadline_instant(request.deadline_at)?,
                self.backend.execute(
                    &target,
                    receipt.execution_id,
                    &request.script_content,
                    request.verification_script_content.as_deref(),
                    request.deadline_at,
                ),
            )
            .await
            {
                Ok(Ok(outcome)) => outcome,
                Ok(Err(error)) => return Err(error),
                Err(_) => {
                    // The durable deadline owns the timeout.  Kill the exact
                    // process group and consume its receipt when possible.
                    self.backend
                        .cancel(&target, receipt.execution_id, request.deadline_at)
                        .await?;
                    if let Some(outcome) = self
                        .backend
                        .observe(
                            &target,
                            receipt.execution_id,
                            request.verification_script_content.is_some(),
                        )
                        .await?
                    {
                        let mut terminal = update_outcome(
                            &self.pool,
                            run_id,
                            expected_revision,
                            receipt,
                            outcome,
                            false,
                        )
                        .await?;
                        terminal.diagnostic_code = Some(contracts::DiagnosticCode::registered(
                            "LW_ENVIRONMENT_WORK_EXECUTION_DEADLINE_EXCEEDED",
                        ));
                        // Persist the diagnostic with a second CAS.  This
                        // cannot overwrite a concurrent cancellation.
                        let _ =
                            update_receipt(&self.pool, run_id, expected_revision + 1, &terminal)
                                .await?;
                        return Ok(());
                    }
                    let _ = mark_cleanup_pending_with_code(
                        &self.pool,
                        run_id,
                        expected_revision,
                        receipt,
                        "LW_ENVIRONMENT_WORK_EXECUTION_DEADLINE_EXCEEDED",
                    )
                    .await?;
                    return Ok(());
                }
            };
            update_outcome(
                &self.pool,
                run_id,
                expected_revision,
                receipt,
                outcome,
                false,
            )
            .await?;
            Ok(())
        }
        .await;
        if let Err(error) = result {
            // A backend error must never leave a durable Running row.  The
            // recovery loop may later observe a still-live process only when
            // the backend explicitly reports cancellation pending.
            if let Err(record_error) = self
                .record_backend_failure(run_id, execution_id, &error)
                .await
            {
                tracing::error!(
                    event = "environment.work_execution.failure_record_failed",
                    run_id = %run_id,
                    error = %record_error,
                    original_error = %error,
                );
            }
            return Err(error);
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    async fn record_backend_failure(
        &self,
        run_id: AgentRunId,
        execution_id: Uuid,
        error: &WorkExecutionError,
    ) -> Result<(), WorkExecutionError> {
        if matches!(error, WorkExecutionError::ConcurrentMutation) {
            return Ok(());
        }
        let record = load_execution(&self.pool, run_id).await?;
        if record.receipt.execution_id != execution_id
            || matches!(
                record.receipt.state,
                ContainerWorkExecutionState::Succeeded
                    | ContainerWorkExecutionState::Failed
                    | ContainerWorkExecutionState::Cancelled
                    | ContainerWorkExecutionState::CleanupFailed
            )
        {
            return Ok(());
        }
        let diagnostic = backend_diagnostic(error);
        if record.receipt.started_at.is_none() || target_is_gone(error) {
            // No backend process can exist before the durable start fence.  A
            // target whose exact persisted Pod identity is gone is equally
            // terminal: the original process cannot still be running there.
            update_failure(
                &self.pool,
                run_id,
                record.revision,
                record.receipt,
                &diagnostic,
            )
            .await?;
            return Ok(());
        }

        // A runner can reject the request before creating its durable process
        // lock (for example, when the runtime lacks GNU `timeout`).  The
        // start fence is already persisted by this point, so the initial
        // RunnerFailed alone cannot prove that no process exists.  Confirm the
        // cancellation protocol first; only then may the backend's fixed
        // prestart marker turn this into a terminal failure.
        if runner_failure(error) {
            match self
                .backend
                .cancel(&record.target, execution_id, record.request.deadline_at)
                .await
            {
                Ok(()) => match self
                    .backend
                    .prestart_failure(&record.target, execution_id)
                    .await
                {
                    Ok(Some(prestart_diagnostic)) => {
                        update_prestart_failure(
                            &self.pool,
                            run_id,
                            record.revision,
                            record.receipt,
                            &prestart_diagnostic,
                        )
                        .await?;
                        return Ok(());
                    }
                    Ok(None) => {}
                    Err(probe_error) => {
                        tracing::warn!(
                            event = "environment.work_execution.prestart_failure_probe_failed",
                            run_id = %run_id,
                            error = %probe_error,
                            diagnostic_code = %diagnostic,
                            retryable = true,
                        );
                        mark_cleanup_pending_with_code(
                            &self.pool,
                            run_id,
                            record.revision,
                            record.receipt,
                            &diagnostic,
                        )
                        .await?;
                        return Ok(());
                    }
                },
                Err(cancel_error) if target_is_gone(&cancel_error) => {
                    update_failure(
                        &self.pool,
                        run_id,
                        record.revision,
                        record.receipt,
                        &backend_diagnostic(&cancel_error),
                    )
                    .await?;
                    return Ok(());
                }
                Err(cancel_error) => {
                    tracing::warn!(
                        event = "environment.work_execution.prestart_failure_cancel_failed",
                        run_id = %run_id,
                        error = %cancel_error,
                        diagnostic_code = %diagnostic,
                        retryable = true,
                    );
                    mark_cleanup_pending_with_code(
                        &self.pool,
                        run_id,
                        record.revision,
                        record.receipt,
                        &diagnostic,
                    )
                    .await?;
                    return Ok(());
                }
            }
        }

        // Once the start fence is durable, a malformed receipt, an unready Pod,
        // or a runner diagnostic does not prove that the process stopped.  A
        // read-only observation may establish termination; otherwise retain a
        // cleanup fence and let recovery retry cancel/observe.
        match self
            .backend
            .observe(
                &record.target,
                execution_id,
                record.request.verification_script_content.is_some(),
            )
            .await
        {
            Ok(Some(outcome)) => {
                update_outcome(
                    &self.pool,
                    run_id,
                    record.revision,
                    record.receipt,
                    outcome,
                    false,
                )
                .await?;
            }
            Ok(None) | Err(_) => {
                mark_cleanup_pending_with_code(
                    &self.pool,
                    run_id,
                    record.revision,
                    record.receipt,
                    &diagnostic,
                )
                .await?;
            }
        }
        Ok(())
    }

    /// Completes (or retries) a persisted cancellation fence.  The caller has
    /// already moved a Running receipt to Cancelling, or loaded a receipt that
    /// was left in Cancelling/CleanupPending by an earlier worker.
    #[allow(clippy::too_many_lines)]
    async fn finish_cancellation(
        &self,
        record: ExecutionRecord,
        deadline_expired: bool,
    ) -> Result<ContainerWorkExecutionReceipt, WorkExecutionError> {
        let run_id = record.request.run_id;
        let execution_id = record.receipt.execution_id;
        let verification_required = record.request.verification_script_content.is_some();

        // No process can have been started before the durable start fence.  A
        // late worker will see the Cancelling state and leave without calling
        // the backend, so this path must never try to read a PID file.
        if record.receipt.started_at.is_none() {
            if deadline_expired {
                return update_failure(
                    &self.pool,
                    run_id,
                    record.revision,
                    record.receipt,
                    "LW_ENVIRONMENT_WORK_EXECUTION_DEADLINE_EXCEEDED",
                )
                .await;
            }
            return mark_cancelled(&self.pool, run_id, record.revision, record.receipt).await;
        }

        if let Err(error) = self
            .backend
            .cancel(&record.target, execution_id, record.request.deadline_at)
            .await
        {
            if terminal_recovery_error(&error) {
                let diagnostic = backend_diagnostic(&error);
                let failed = if target_is_gone(&error) {
                    update_failure(
                        &self.pool,
                        run_id,
                        record.revision,
                        record.receipt,
                        &diagnostic,
                    )
                    .await?
                } else {
                    mark_cleanup_pending_with_code(
                        &self.pool,
                        run_id,
                        record.revision,
                        record.receipt,
                        &diagnostic,
                    )
                    .await?
                };
                tracing::error!(
                    event = "environment.work_execution.cancel_failed",
                    run_id = %run_id,
                    error = %error,
                    diagnostic_code = %diagnostic,
                    retryable = false,
                );
                return Ok(failed);
            }
            // A transport/API timeout can be transient.  Leave the durable
            // cancellation fence in place so the recovery loop retries it.
            return Err(error);
        }

        // `cancel` serializes with the runner's process-launch decision.  A
        // successful handshake therefore allows a backend to check its fixed
        // prestart marker and close a stale receipt left by an earlier worker,
        // including receipts already in CleanupPending.
        match self
            .backend
            .prestart_failure(&record.target, execution_id)
            .await
        {
            Ok(Some(prestart_diagnostic)) => {
                return update_prestart_failure(
                    &self.pool,
                    run_id,
                    record.revision,
                    record.receipt,
                    &prestart_diagnostic,
                )
                .await;
            }
            Ok(None) => {}
            Err(error) => return Err(error),
        }

        match self
            .backend
            .observe(&record.target, execution_id, verification_required)
            .await
        {
            Ok(Some(outcome)) => {
                let terminal = update_outcome(
                    &self.pool,
                    run_id,
                    record.revision,
                    record.receipt,
                    outcome,
                    !deadline_expired,
                )
                .await?;
                if deadline_expired {
                    return update_diagnostic(
                        &self.pool,
                        run_id,
                        record.revision + 1,
                        terminal,
                        "LW_ENVIRONMENT_WORK_EXECUTION_DEADLINE_EXCEEDED",
                    )
                    .await;
                }
                Ok(terminal)
            }
            Ok(None) => {
                // Do not repeatedly rewrite an already pending receipt during
                // recovery.  The process-group signal was sent successfully;
                // a later pass will consume the terminal runner receipt.
                if record.receipt.state == ContainerWorkExecutionState::CleanupPending
                    && !deadline_expired
                {
                    return Ok(record.receipt);
                }
                let diagnostic = if deadline_expired {
                    "LW_ENVIRONMENT_WORK_EXECUTION_DEADLINE_EXCEEDED"
                } else {
                    "LW_ENVIRONMENT_WORK_EXECUTION_CANCELLATION_PENDING"
                };
                mark_cleanup_pending_with_code(
                    &self.pool,
                    run_id,
                    record.revision,
                    record.receipt,
                    diagnostic,
                )
                .await
            }
            Err(error) if terminal_recovery_error(&error) => {
                let diagnostic = backend_diagnostic(&error);
                let failed = if target_is_gone(&error) {
                    update_failure(
                        &self.pool,
                        run_id,
                        record.revision,
                        record.receipt,
                        &diagnostic,
                    )
                    .await?
                } else {
                    mark_cleanup_pending_with_code(
                        &self.pool,
                        run_id,
                        record.revision,
                        record.receipt,
                        &diagnostic,
                    )
                    .await?
                };
                tracing::error!(
                    event = "environment.work_execution.cancel_observe_failed",
                    run_id = %run_id,
                    error = %error,
                    diagnostic_code = %diagnostic,
                    retryable = false,
                );
                Ok(failed)
            }
            Err(error) => Err(error),
        }
    }
}

/// Validates the shared Environment-side Work execution fence.
///
/// Callers add their backend-specific runtime checks after this function. Keeping
/// scope, readiness, expiry, and the Resource lease fence here prevents the
/// target read and the container execution path from drifting apart.
pub(crate) fn validate_work_environment(
    instance: &contracts::environment::EnvironmentInstance,
    project_id: ProjectId,
    course_id: Option<contracts::CourseId>,
    actor_id: ActorId,
    expected_revision: Revision,
    now: UtcTimestamp,
) -> Result<(), WorkExecutionError> {
    let lease_valid = match (
        instance.lease_id,
        instance.capacity_binding.as_deref(),
        instance.operation.lease_authorization.as_ref(),
    ) {
        (Some(lease_id), Some(capacity_binding), Some(authorization)) => {
            authorization.lease_id == lease_id
                && authorization.environment_id == instance.id
                && authorization.project_id == instance.project_id
                && authorization.course_id == instance.course_id
                && authorization.owner_actor_id == instance.owner_id
                && authorization.capacity_binding == capacity_binding
                && authorization.active_from <= now
                && authorization.expires_at > now
                && authorization.validate().is_ok()
        }
        _ => false,
    };
    if instance.class != contracts::authoring::EnvironmentClass::Work
        || instance.project_id != project_id
        || instance.course_id != course_id
        || instance.owner_id != actor_id
        || instance.revision != expected_revision
        || instance.desired_state != contracts::environment::DesiredEnvironmentState::Running
        || instance.observed_state != contracts::environment::ObservedEnvironmentState::Ready
        || instance.observed_generation != instance.generation
        || instance.eligibility_expires_at <= now
        || instance.endpoints.is_empty()
        || !instance.endpoints.iter().all(|endpoint| {
            endpoint.health == contracts::environment::EndpointHealth::Healthy
                && endpoint.revision == instance.revision
        })
        || !lease_valid
    {
        return Err(WorkExecutionError::EnvironmentNotEligible);
    }
    Ok(())
}

fn verify_admission(
    request: &ContainerWorkExecutionRequest,
    admission: &WorkConfigurationAdmissionBinding,
) -> Result<(), WorkExecutionError> {
    if admission.project_id != request.project_id
        || admission.course_id != request.course_id
        || admission.environment_id != request.environment_id
        || admission.environment_revision != request.environment_revision
        || admission.actor_id != request.actor_id
        || admission.run_revision != request.run_revision
        || admission.state != contracts::authoring::AgentRunState::Running
        || admission
            .plan
            .as_ref()
            .is_none_or(|plan| plan.id != request.plan_id || plan.revision != request.plan_revision)
        || admission.preauthorization.as_ref().is_none_or(|grant| {
            grant.plan_id != request.plan_id || grant.plan_revision != request.plan_revision
        })
        || persistence_sqlx::Sha256Digest::of_bytes(request.script_content.as_bytes()).to_string()
            != admission.script_sha256
        || admission.verification_script_sha256.as_deref()
            != request
                .verification_script_content
                .as_deref()
                .map(|script| {
                    persistence_sqlx::Sha256Digest::of_bytes(script.as_bytes()).to_string()
                })
                .as_deref()
    {
        return Err(WorkExecutionError::AdmissionMismatch);
    }
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ExecutionRecord {
    request: ContainerWorkExecutionRequest,
    target: ContainerWorkExecutionTarget,
    receipt: ContainerWorkExecutionReceipt,
    revision: i64,
}

fn receipt_matches(
    receipt: &ContainerWorkExecutionReceipt,
    query: &ContainerWorkExecutionQuery,
) -> bool {
    receipt.environment_id == query.environment_id
        && receipt.plan_id == query.plan_id
        && receipt.plan_revision == query.plan_revision
}

fn record_matches_query(
    record: &ExecutionRecord,
    run_id: AgentRunId,
    query: &ContainerWorkExecutionQuery,
) -> bool {
    record.request.run_id == run_id
        && record.receipt.run_id == run_id
        && record.request.project_id == query.project_id
        && record.request.environment_id == query.environment_id
        && record.request.plan_id == query.plan_id
        && record.request.plan_revision == query.plan_revision
        && receipt_matches(&record.receipt, query)
        && record.receipt.validate().is_ok()
}

async fn insert_execution(
    pool: &PgPool,
    request: &ContainerWorkExecutionRequest,
    target: &ContainerWorkExecutionTarget,
    receipt: &ContainerWorkExecutionReceipt,
) -> Result<Option<ContainerWorkExecutionReceipt>, WorkExecutionError> {
    let mut tx = pool.begin().await?;
    let inserted = sqlx::query(
        "INSERT INTO environment.work_configuration_executions (run_id,execution_id,project_id,environment_id,plan_id,plan_revision,request_json,target_json,receipt_json,revision) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,1) ON CONFLICT (run_id) DO NOTHING RETURNING run_id",
    )
    .bind(request.run_id.as_uuid())
    .bind(receipt.execution_id)
    .bind(request.project_id.as_uuid())
    .bind(request.environment_id.as_uuid())
    .bind(request.plan_id.as_uuid())
    .bind(i64::try_from(request.plan_revision.get()).map_err(|_| WorkExecutionError::IdentityMismatch)?)
    .bind(serde_json::to_value(request)?)
    .bind(serde_json::to_value(target)?)
    .bind(serde_json::to_value(receipt)?)
    .fetch_optional(&mut *tx)
    .await?;
    if inserted.is_some() {
        tx.commit().await?;
        return Ok(None);
    }
    let row = sqlx::query("SELECT request_json,receipt_json FROM environment.work_configuration_executions WHERE run_id=$1 FOR UPDATE")
        .bind(request.run_id.as_uuid()).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    let existing_request: ContainerWorkExecutionRequest =
        serde_json::from_value(row.try_get("request_json")?)?;
    if existing_request != *request {
        return Err(WorkExecutionError::IdentityMismatch);
    }
    let existing: ContainerWorkExecutionReceipt =
        serde_json::from_value(row.try_get("receipt_json")?)?;
    if existing.plan_id != request.plan_id
        || existing.plan_revision != request.plan_revision
        || existing.environment_id != request.environment_id
        || existing.run_id != request.run_id
        || existing.environment_revision != request.environment_revision
    {
        return Err(WorkExecutionError::IdentityMismatch);
    }
    Ok(Some(existing))
}

async fn load_execution(
    pool: &PgPool,
    run_id: AgentRunId,
) -> Result<ExecutionRecord, WorkExecutionError> {
    load_execution_optional(pool, run_id)
        .await?
        .ok_or(WorkExecutionError::NotFound)
}

async fn load_execution_optional(
    pool: &PgPool,
    run_id: AgentRunId,
) -> Result<Option<ExecutionRecord>, WorkExecutionError> {
    let row = sqlx::query("SELECT request_json,target_json,receipt_json,revision FROM environment.work_configuration_executions WHERE run_id=$1")
        .bind(run_id.as_uuid())
        .fetch_optional(pool)
        .await?;
    row.map(|row| {
        Ok(ExecutionRecord {
            request: serde_json::from_value(row.try_get("request_json")?)?,
            target: serde_json::from_value(row.try_get("target_json")?)?,
            receipt: serde_json::from_value(row.try_get("receipt_json")?)?,
            revision: row.try_get("revision")?,
        })
    })
    .transpose()
}

async fn update_started(
    pool: &PgPool,
    run_id: AgentRunId,
    expected_revision: i64,
    mut receipt: ContainerWorkExecutionReceipt,
    started: UtcTimestamp,
) -> Result<Option<ContainerWorkExecutionReceipt>, WorkExecutionError> {
    receipt.started_at = Some(started);
    Ok(update_receipt(pool, run_id, expected_revision, &receipt)
        .await?
        .then_some(receipt))
}

async fn update_failure(
    pool: &PgPool,
    run_id: AgentRunId,
    expected_revision: i64,
    mut receipt: ContainerWorkExecutionReceipt,
    code: &str,
) -> Result<ContainerWorkExecutionReceipt, WorkExecutionError> {
    receipt.state = if matches!(
        receipt.state,
        ContainerWorkExecutionState::Cancelling | ContainerWorkExecutionState::CleanupPending
    ) {
        ContainerWorkExecutionState::CleanupFailed
    } else {
        ContainerWorkExecutionState::Failed
    };
    update_failure_with_state(pool, run_id, expected_revision, receipt, code).await
}

async fn update_prestart_failure(
    pool: &PgPool,
    run_id: AgentRunId,
    expected_revision: i64,
    mut receipt: ContainerWorkExecutionReceipt,
    code: &str,
) -> Result<ContainerWorkExecutionReceipt, WorkExecutionError> {
    // The successful cancellation handshake and fixed marker together identify
    // a rejected launch, including when the durable receipt had already
    // reached CleanupPending. Keep this as Failed rather than CleanupFailed so
    // callers can distinguish it from an unresolved cleanup fence.
    receipt.state = ContainerWorkExecutionState::Failed;
    update_failure_with_state(pool, run_id, expected_revision, receipt, code).await
}

async fn update_failure_with_state(
    pool: &PgPool,
    run_id: AgentRunId,
    expected_revision: i64,
    mut receipt: ContainerWorkExecutionReceipt,
    code: &str,
) -> Result<ContainerWorkExecutionReceipt, WorkExecutionError> {
    receipt.diagnostic_code = Some(
        contracts::DiagnosticCode::parse(code.to_owned())
            .map_err(|_| WorkExecutionError::ReceiptInvalid)?,
    );
    receipt.finished_at = Some(current_time(pool).await?);
    if !update_receipt(pool, run_id, expected_revision, &receipt).await? {
        return Err(WorkExecutionError::ConcurrentMutation);
    }
    Ok(receipt)
}

async fn mark_cancelled(
    pool: &PgPool,
    run_id: AgentRunId,
    expected_revision: i64,
    mut receipt: ContainerWorkExecutionReceipt,
) -> Result<ContainerWorkExecutionReceipt, WorkExecutionError> {
    receipt.state = ContainerWorkExecutionState::Cancelled;
    receipt.diagnostic_code = None;
    receipt.finished_at = Some(current_time(pool).await?);
    if !update_receipt(pool, run_id, expected_revision, &receipt).await? {
        return Err(WorkExecutionError::ConcurrentMutation);
    }
    Ok(receipt)
}

async fn mark_cleanup_pending_with_code(
    pool: &PgPool,
    run_id: AgentRunId,
    expected_revision: i64,
    mut receipt: ContainerWorkExecutionReceipt,
    diagnostic: &str,
) -> Result<ContainerWorkExecutionReceipt, WorkExecutionError> {
    receipt.state = ContainerWorkExecutionState::CleanupPending;
    receipt.diagnostic_code = Some(
        contracts::DiagnosticCode::parse(diagnostic.to_owned())
            .map_err(|_| WorkExecutionError::ReceiptInvalid)?,
    );
    receipt.finished_at = None;
    if !update_receipt(pool, run_id, expected_revision, &receipt).await? {
        return Err(WorkExecutionError::ConcurrentMutation);
    }
    Ok(receipt)
}

async fn update_diagnostic(
    pool: &PgPool,
    run_id: AgentRunId,
    expected_revision: i64,
    mut receipt: ContainerWorkExecutionReceipt,
    diagnostic: &'static str,
) -> Result<ContainerWorkExecutionReceipt, WorkExecutionError> {
    receipt.diagnostic_code = Some(contracts::DiagnosticCode::registered(diagnostic));
    if !update_receipt(pool, run_id, expected_revision, &receipt).await? {
        return Err(WorkExecutionError::ConcurrentMutation);
    }
    Ok(receipt)
}

async fn update_outcome(
    pool: &PgPool,
    run_id: AgentRunId,
    expected_revision: i64,
    mut receipt: ContainerWorkExecutionReceipt,
    outcome: WorkExecutionOutcome,
    cancelling: bool,
) -> Result<ContainerWorkExecutionReceipt, WorkExecutionError> {
    receipt.exit_code = Some(outcome.exit_code);
    receipt.verification_exit_code = outcome.verification_exit_code;
    receipt.output = outcome.output;
    receipt.output_truncated = outcome.output_truncated;
    let succeeded =
        outcome.exit_code == 0 && outcome.verification_exit_code.is_none_or(|code| code == 0);
    receipt.state = if cancelling {
        ContainerWorkExecutionState::Cancelled
    } else if succeeded {
        ContainerWorkExecutionState::Succeeded
    } else {
        ContainerWorkExecutionState::Failed
    };
    receipt.finished_at = Some(current_time(pool).await?);
    if !update_receipt(pool, run_id, expected_revision, &receipt).await? {
        return Err(WorkExecutionError::ConcurrentMutation);
    }
    Ok(receipt)
}

async fn update_receipt(
    pool: &PgPool,
    run_id: AgentRunId,
    expected_revision: i64,
    receipt: &ContainerWorkExecutionReceipt,
) -> Result<bool, WorkExecutionError> {
    receipt
        .validate()
        .map_err(|_| WorkExecutionError::ReceiptInvalid)?;
    let result = sqlx::query("UPDATE environment.work_configuration_executions SET receipt_json=$2,revision=revision+1,updated_at=clock_timestamp() WHERE run_id=$1 AND revision=$3")
        .bind(run_id.as_uuid()).bind(serde_json::to_value(receipt)?).bind(expected_revision).execute(pool).await?;
    Ok(result.rows_affected() == 1)
}

fn backend_diagnostic(error: &WorkExecutionError) -> String {
    match error {
        WorkExecutionError::DeadlineExceeded => {
            "LW_ENVIRONMENT_WORK_EXECUTION_DEADLINE_EXCEEDED".to_owned()
        }
        WorkExecutionError::TargetIdentityChanged => {
            "LW_ENVIRONMENT_WORK_EXECUTION_TARGET_IDENTITY_CHANGED".to_owned()
        }
        WorkExecutionError::TargetUnavailable => {
            "LW_ENVIRONMENT_WORK_EXECUTION_TARGET_UNAVAILABLE".to_owned()
        }
        WorkExecutionError::TargetAmbiguous => {
            "LW_ENVIRONMENT_WORK_EXECUTION_TARGET_AMBIGUOUS".to_owned()
        }
        WorkExecutionError::ObservationInvalid => {
            "LW_ENVIRONMENT_WORK_EXECUTION_RECEIPT_INVALID".to_owned()
        }
        WorkExecutionError::RunnerFailed => {
            "LW_ENVIRONMENT_WORK_EXECUTION_RUNNER_FAILED".to_owned()
        }
        WorkExecutionError::RunnerFailedWithDiagnostic(code) => code.clone(),
        WorkExecutionError::CancellationTimeout => {
            "LW_ENVIRONMENT_WORK_EXECUTION_CANCELLATION_TIMEOUT".to_owned()
        }
        WorkExecutionError::Clock => "LW_ENVIRONMENT_WORK_EXECUTION_CLOCK_FAILED".to_owned(),
        _ => "LW_ENVIRONMENT_WORK_EXECUTION_BACKEND_FAILED".to_owned(),
    }
}

fn runner_failure(error: &WorkExecutionError) -> bool {
    matches!(
        error,
        WorkExecutionError::RunnerFailed | WorkExecutionError::RunnerFailedWithDiagnostic(_)
    )
}

fn terminal_recovery_error(error: &WorkExecutionError) -> bool {
    matches!(
        error,
        WorkExecutionError::TargetIdentityChanged
            | WorkExecutionError::TargetUnavailable
            | WorkExecutionError::ObservationInvalid
            | WorkExecutionError::RunnerFailed
            | WorkExecutionError::RunnerFailedWithDiagnostic(_)
    )
}

fn target_is_gone(error: &WorkExecutionError) -> bool {
    matches!(error, WorkExecutionError::TargetIdentityChanged)
}

async fn current_time(pool: &PgPool) -> Result<UtcTimestamp, WorkExecutionError> {
    let value: time::OffsetDateTime =
        sqlx::query_scalar("SELECT date_trunc('milliseconds',clock_timestamp())")
            .fetch_one(pool)
            .await?;
    UtcTimestamp::from_utc(value).map_err(|_| WorkExecutionError::Clock)
}

fn deadline_instant(deadline: UtcTimestamp) -> Result<tokio::time::Instant, WorkExecutionError> {
    let now = time::OffsetDateTime::now_utc();
    let delta = deadline.get() - now;
    let duration = delta
        .try_into()
        .map_err(|_| WorkExecutionError::DeadlineExceeded)?;
    Ok(tokio::time::Instant::now() + duration)
}

fn deadline_epoch_seconds(deadline: UtcTimestamp) -> Result<u64, WorkExecutionError> {
    let value = deadline.get();
    let seconds = value.unix_timestamp();
    let rounded = seconds
        .checked_add(i64::from(value.nanosecond() != 0))
        .ok_or(WorkExecutionError::DeadlineExceeded)?;
    u64::try_from(rounded).map_err(|_| WorkExecutionError::DeadlineExceeded)
}

/// Kubernetes implementation using only the fixed runtime container and positional arguments.
#[derive(Clone)]
pub struct KubernetesWorkExecutionBackend {
    client: Client,
}

impl KubernetesWorkExecutionBackend {
    pub async fn from_default() -> Result<Self, WorkExecutionError> {
        Ok(Self {
            client: Client::try_default().await.map_err(|error| {
                tracing::error!(
                    event = "environment.work_execution.kubernetes_client_init_failed",
                    operation = "client_init",
                    error_context = %bounded_context(&error),
                );
                WorkExecutionError::Kubernetes
            })?,
        })
    }

    fn pods(&self, target: &ContainerWorkExecutionTarget) -> Api<Pod> {
        Api::namespaced(self.client.clone(), &target.namespace)
    }

    async fn verify_target(
        &self,
        target: &ContainerWorkExecutionTarget,
    ) -> Result<(), WorkExecutionError> {
        let pod = self
            .pods(target)
            .get(&target.pod_name)
            .await
            .map_err(|error| {
                let identity_changed = matches!(
                    &error,
                    kube::Error::Api(response) if response.code == 404
                );
                tracing::warn!(
                    event = "environment.work_execution.kubernetes_target_get_failed",
                    operation = "target_get",
                    namespace = %target.namespace,
                    pod_name = %target.pod_name,
                    pod_uid = %target.pod_uid,
                    outcome = if identity_changed { "identity_changed" } else { "unavailable" },
                    error_context = %bounded_context(&error),
                );
                if identity_changed {
                    WorkExecutionError::TargetIdentityChanged
                } else {
                    WorkExecutionError::TargetUnavailable
                }
            })?;
        if pod.metadata.uid.as_deref() != Some(target.pod_uid.as_str())
            || target.container != RUNTIME_CONTAINER
            || target.workdir != WORKSPACE_ROOT
        {
            return Err(WorkExecutionError::TargetIdentityChanged);
        }
        if !ready_runtime_pod(&pod) {
            return Err(WorkExecutionError::TargetUnavailable);
        }
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    async fn exec_fixed(
        &self,
        target: &ContainerWorkExecutionTarget,
        command: Vec<String>,
        input: Option<&[u8]>,
    ) -> Result<(Vec<u8>, Vec<u8>), WorkExecutionError> {
        self.verify_target(target).await?;
        let mut process = self
            .pods(target)
            .exec(
                &target.pod_name,
                command,
                &AttachParams::default()
                    .container(&target.container)
                    .stdin(input.is_some())
                    .stdout(true)
                    .stderr(true),
            )
            .await
            .map_err(|error| {
                tracing::error!(
                    event = "environment.work_execution.kubernetes_exec_start_failed",
                    operation = "pod_exec_start",
                    namespace = %target.namespace,
                    pod_name = %target.pod_name,
                    pod_uid = %target.pod_uid,
                    container = %target.container,
                    error_context = %bounded_context(&error),
                );
                WorkExecutionError::Kubernetes
            })?;
        if let Some(input) = input {
            let mut stdin = process.stdin().ok_or_else(|| {
                tracing::error!(
                    event = "environment.work_execution.kubernetes_exec_stdin_missing",
                    operation = "pod_exec_stdin",
                    namespace = %target.namespace,
                    pod_name = %target.pod_name,
                    pod_uid = %target.pod_uid,
                    container = %target.container,
                );
                WorkExecutionError::Kubernetes
            })?;
            stdin.write_all(input).await.map_err(|error| {
                tracing::error!(
                    event = "environment.work_execution.kubernetes_exec_stdin_write_failed",
                    operation = "pod_exec_stdin_write",
                    namespace = %target.namespace,
                    pod_name = %target.pod_name,
                    pod_uid = %target.pod_uid,
                    container = %target.container,
                    input_bytes = input.len(),
                    error_context = %bounded_context(&error),
                );
                WorkExecutionError::Kubernetes
            })?;
            stdin.shutdown().await.map_err(|error| {
                tracing::error!(
                    event = "environment.work_execution.kubernetes_exec_stdin_shutdown_failed",
                    operation = "pod_exec_stdin_shutdown",
                    namespace = %target.namespace,
                    pod_name = %target.pod_name,
                    pod_uid = %target.pod_uid,
                    container = %target.container,
                    error_context = %bounded_context(&error),
                );
                WorkExecutionError::Kubernetes
            })?;
        }
        let mut stdout = process.stdout().ok_or_else(|| {
            tracing::error!(
                event = "environment.work_execution.kubernetes_exec_stdout_missing",
                operation = "pod_exec_stdout",
                namespace = %target.namespace,
                pod_name = %target.pod_name,
                pod_uid = %target.pod_uid,
                container = %target.container,
            );
            WorkExecutionError::Kubernetes
        })?;
        let mut stderr = process.stderr().ok_or_else(|| {
            tracing::error!(
                event = "environment.work_execution.kubernetes_exec_stderr_missing",
                operation = "pod_exec_stderr",
                namespace = %target.namespace,
                pod_name = %target.pod_name,
                pod_uid = %target.pod_uid,
                container = %target.container,
            );
            WorkExecutionError::Kubernetes
        })?;
        let (mut out, mut err) = (Vec::new(), Vec::new());
        tokio::try_join!(stdout.read_to_end(&mut out), stderr.read_to_end(&mut err)).map_err(
            |error| {
                tracing::error!(
                    event = "environment.work_execution.kubernetes_exec_output_read_failed",
                    operation = "pod_exec_output_read",
                    namespace = %target.namespace,
                    pod_name = %target.pod_name,
                    pod_uid = %target.pod_uid,
                    container = %target.container,
                    error_context = %bounded_context(&error),
                );
                WorkExecutionError::Kubernetes
            },
        )?;
        let status = process.take_status().ok_or_else(|| {
            tracing::error!(
                event = "environment.work_execution.kubernetes_exec_status_missing",
                operation = "pod_exec_status",
                namespace = %target.namespace,
                pod_name = %target.pod_name,
                pod_uid = %target.pod_uid,
                container = %target.container,
            );
            WorkExecutionError::Kubernetes
        })?;
        process.join().await.map_err(|error| {
            tracing::error!(
                event = "environment.work_execution.kubernetes_exec_join_failed",
                operation = "pod_exec_join",
                namespace = %target.namespace,
                pod_name = %target.pod_name,
                pod_uid = %target.pod_uid,
                container = %target.container,
                error_context = %bounded_context(&error),
            );
            WorkExecutionError::Kubernetes
        })?;
        let status = status.await.ok_or_else(|| {
            tracing::error!(
                event = "environment.work_execution.kubernetes_exec_status_channel_failed",
                operation = "pod_exec_status_channel",
                namespace = %target.namespace,
                pod_name = %target.pod_name,
                pod_uid = %target.pod_uid,
                container = %target.container,
            );
            WorkExecutionError::Kubernetes
        })?;
        if status.status.as_deref() != Some("Success") {
            let error = remote_command_error(&err);
            tracing::warn!(
                event = "environment.work_execution.kubernetes_exec_command_failed",
                operation = "pod_exec_command",
                namespace = %target.namespace,
                pod_name = %target.pod_name,
                pod_uid = %target.pod_uid,
                container = %target.container,
                status = ?status.status,
                stderr_bytes = err.len(),
                stderr_context = %bounded_context(String::from_utf8_lossy(&err)),
                diagnostic_code = %backend_diagnostic(&error),
            );
            return Err(error);
        }
        Ok((out, err))
    }

    async fn upload_file(
        &self,
        target: &ContainerWorkExecutionTarget,
        path: &str,
        contents: &[u8],
    ) -> Result<(), WorkExecutionError> {
        let command = vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            "set -eu; cat > \"$1\"; chmod 700 -- \"$1\"".to_owned(),
            "labweaver-upload".to_owned(),
            path.to_owned(),
        ];
        let (_, stderr) = self.exec_fixed(target, command, Some(contents)).await?;
        if !stderr.is_empty() {
            tracing::warn!(
                event = "environment.work_execution.kubernetes_upload_stderr",
                operation = "upload_file",
                namespace = %target.namespace,
                pod_name = %target.pod_name,
                pod_uid = %target.pod_uid,
                container = %target.container,
                path = %path,
                stderr_bytes = stderr.len(),
                stderr_context = %bounded_context(String::from_utf8_lossy(&stderr)),
            );
            return Err(WorkExecutionError::Kubernetes);
        }
        Ok(())
    }
}

#[async_trait]
impl ContainerWorkExecutionBackend for KubernetesWorkExecutionBackend {
    async fn resolve_target(
        &self,
        request: &ContainerWorkExecutionRequest,
    ) -> Result<ContainerWorkExecutionTarget, WorkExecutionError> {
        let namespace = format!("lw-env-{}", request.environment_id);
        let mut selector = format!(
            "app=runtime,labweaver.io/environment-id={},labweaver.io/project-id={}",
            request.environment_id, request.project_id
        );
        if let Some(course_id) = request.course_id {
            let _ = write!(selector, ",labweaver.io/course-id={course_id}");
        }
        let pods: Api<Pod> = Api::namespaced(self.client.clone(), &namespace);
        let list = pods
            .list(&ListParams::default().labels(&selector))
            .await
            .map_err(|error| {
                tracing::error!(
                    event = "environment.work_execution.kubernetes_target_list_failed",
                    operation = "target_list",
                    namespace = %namespace,
                    error_context = %bounded_context(&error),
                );
                WorkExecutionError::Kubernetes
            })?;
        let mut matches = list.items.into_iter().filter(ready_runtime_pod);
        let pod = matches
            .next()
            .ok_or(WorkExecutionError::TargetUnavailable)?;
        if matches.next().is_some() {
            return Err(WorkExecutionError::TargetAmbiguous);
        }
        let pod_name = pod
            .metadata
            .name
            .ok_or(WorkExecutionError::TargetUnavailable)?;
        let pod_uid = pod
            .metadata
            .uid
            .ok_or(WorkExecutionError::TargetUnavailable)?;
        Ok(ContainerWorkExecutionTarget {
            namespace,
            pod_name,
            pod_uid,
            container: RUNTIME_CONTAINER.to_owned(),
            workdir: WORKSPACE_ROOT.to_owned(),
        })
    }

    async fn execute(
        &self,
        target: &ContainerWorkExecutionTarget,
        execution_id: Uuid,
        script: &str,
        verification_script: Option<&str>,
        deadline_at: UtcTimestamp,
    ) -> Result<WorkExecutionOutcome, WorkExecutionError> {
        let dir = format!("{EXECUTION_ROOT}/{execution_id}");
        let deadline_seconds = deadline_epoch_seconds(deadline_at)?;
        self.exec_fixed(
            target,
            vec!["/bin/mkdir".to_owned(), "-p".to_owned(), dir.clone()],
            None,
        )
        .await?;
        self.upload_file(target, &format!("{dir}/runner.sh"), RUNNER_SCRIPT)
            .await?;
        self.upload_file(target, &format!("{dir}/primary.sh"), script.as_bytes())
            .await?;
        if let Some(script) = verification_script {
            self.upload_file(target, &format!("{dir}/verification.sh"), script.as_bytes())
                .await?;
        }
        let command = vec![
            "/bin/sh".to_owned(),
            format!("{dir}/runner.sh"),
            dir.clone(),
            target.workdir.clone(),
            if verification_script.is_some() {
                "1"
            } else {
                "0"
            }
            .to_owned(),
            deadline_seconds.to_string(),
        ];
        let _ = self.exec_fixed(target, command, None).await?;
        self.observe(target, execution_id, verification_script.is_some())
            .await?
            .ok_or(WorkExecutionError::ObservationInvalid)
    }

    async fn cancel(
        &self,
        target: &ContainerWorkExecutionTarget,
        execution_id: Uuid,
        _deadline_at: UtcTimestamp,
    ) -> Result<(), WorkExecutionError> {
        let dir = format!("{EXECUTION_ROOT}/{execution_id}");
        let command = vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            "set -eu; mkdir -p -- \"$1\"; : > \"$1/cancel\"; if [ -x \"$1/runner.sh\" ]; then /bin/sh \"$1/runner.sh\" --cancel \"$1\"; fi".to_owned(),
            "labweaver-cancel".to_owned(),
            dir,
        ];
        tokio::time::timeout(CANCEL_TIMEOUT, self.exec_fixed(target, command, None))
            .await
            .map_err(|_| WorkExecutionError::CancellationTimeout)??;
        Ok(())
    }

    async fn prestart_failure(
        &self,
        target: &ContainerWorkExecutionTarget,
        execution_id: Uuid,
    ) -> Result<Option<String>, WorkExecutionError> {
        self.verify_target(target).await?;
        let dir = format!("{EXECUTION_ROOT}/{execution_id}");
        let command = vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            // The cancel marker is part of the same fixed protocol as the
            // runner lock.  Requiring it keeps this read from becoming an
            // authorization decision based on user-writable files alone.
            "set -eu; if [ -f \"$1/phase\" ] && [ \"$(cat -- \"$1/phase\")\" = failed ] && [ ! -e \"$1/.lock\" ] && [ -f \"$1/cancel\" ] && [ -s \"$1/runner_error\" ]; then printf '1\\n'; cat -- \"$1/runner_error\"; else printf '0\\n'; fi".to_owned(),
            "labweaver-prestart-failure".to_owned(),
            dir,
        ];
        let (stdout, _) = self.exec_fixed(target, command, None).await?;
        parse_prestart_failure_probe(&stdout)
    }

    async fn observe(
        &self,
        target: &ContainerWorkExecutionTarget,
        execution_id: Uuid,
        verification_required: bool,
    ) -> Result<Option<WorkExecutionOutcome>, WorkExecutionError> {
        self.verify_target(target).await?;
        let dir = format!("{EXECUTION_ROOT}/{execution_id}");
        let command = vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            "set +e; \"$1\" --observe \"$2\" \"$3\"; status=$?; if [ \"$status\" -eq 3 ]; then exit 0; fi; exit \"$status\"".to_owned(),
            "labweaver-observe".to_owned(),
            format!("{dir}/runner.sh"),
            dir,
            if verification_required { "1" } else { "0" }.to_owned(),
        ];
        let (stdout, _) = self.exec_fixed(target, command, None).await?;
        if stdout.is_empty() {
            return Ok(None);
        }
        let mut lines = stdout.splitn(4, |byte| *byte == b'\n');
        let exit_code =
            std::str::from_utf8(lines.next().ok_or(WorkExecutionError::ObservationInvalid)?)
                .map_err(|_| WorkExecutionError::ObservationInvalid)?
                .trim()
                .parse()
                .map_err(|_| WorkExecutionError::ObservationInvalid)?;
        let verification = lines.next().ok_or(WorkExecutionError::ObservationInvalid)?;
        let verification_exit_code = if verification == b"-" {
            None
        } else {
            Some(
                std::str::from_utf8(verification)
                    .map_err(|_| WorkExecutionError::ObservationInvalid)?
                    .trim()
                    .parse()
                    .map_err(|_| WorkExecutionError::ObservationInvalid)?,
            )
        };
        let output_truncated = match lines.next().ok_or(WorkExecutionError::ObservationInvalid)? {
            b"0" => false,
            b"1" => true,
            _ => return Err(WorkExecutionError::ObservationInvalid),
        };
        let (output, bounded_truncated) = bound_output(lines.next().unwrap_or_default());
        Ok(Some(WorkExecutionOutcome {
            exit_code,
            verification_exit_code,
            output,
            output_truncated: output_truncated || bounded_truncated,
        }))
    }
}

const MAX_BACKEND_ERROR_CONTEXT_CHARS: usize = 512;

fn bounded_context(value: impl std::fmt::Display) -> String {
    let value = value.to_string();
    let mut characters = value.chars();
    let mut bounded = characters
        .by_ref()
        .take(MAX_BACKEND_ERROR_CONTEXT_CHARS)
        .collect::<String>();
    if characters.next().is_some() {
        bounded.push_str("...");
    }
    bounded
}

fn remote_command_error(stderr: &[u8]) -> WorkExecutionError {
    let stderr = String::from_utf8_lossy(stderr);
    let code = stderr.lines().find_map(|line| {
        let code = line.trim();
        let valid = code.starts_with("LW_ENVIRONMENT_WORK_EXECUTION_")
            && code.len() <= 96
            && code
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_');
        valid.then_some(code.to_owned())
    });
    match code.as_deref() {
        Some("LW_ENVIRONMENT_WORK_EXECUTION_RUNNER_FAILED") => WorkExecutionError::RunnerFailed,
        Some("LW_ENVIRONMENT_WORK_EXECUTION_TARGET_IDENTITY_CHANGED") => {
            WorkExecutionError::TargetIdentityChanged
        }
        Some(
            "LW_ENVIRONMENT_WORK_EXECUTION_VERIFICATION_MISSING"
            | "LW_ENVIRONMENT_WORK_EXECUTION_OBSERVATION_INVALID",
        ) => WorkExecutionError::ObservationInvalid,
        Some(code) => WorkExecutionError::RunnerFailedWithDiagnostic(code.to_owned()),
        None => WorkExecutionError::Kubernetes,
    }
}

fn parse_prestart_failure_probe(stdout: &[u8]) -> Result<Option<String>, WorkExecutionError> {
    let mut lines = stdout.splitn(2, |byte| *byte == b'\n');
    match lines.next().ok_or(WorkExecutionError::ObservationInvalid)? {
        b"0" if lines.next().is_none_or(<[u8]>::is_empty) => Ok(None),
        b"1" => {
            let marker = lines
                .next()
                .and_then(|rest| rest.strip_suffix(b"\n"))
                .ok_or(WorkExecutionError::ObservationInvalid)?;
            if marker.contains(&b'\n') {
                return Err(WorkExecutionError::ObservationInvalid);
            }
            let code = std::str::from_utf8(marker)
                .map_err(|_| WorkExecutionError::ObservationInvalid)?
                .to_owned();
            if !code.starts_with("LW_ENVIRONMENT_WORK_EXECUTION_") {
                return Err(WorkExecutionError::ObservationInvalid);
            }
            contracts::DiagnosticCode::parse(code.clone())
                .map_err(|_| WorkExecutionError::ObservationInvalid)?;
            Ok(Some(code))
        }
        _ => Err(WorkExecutionError::ObservationInvalid),
    }
}

fn bound_output(bytes: &[u8]) -> (String, bool) {
    let raw_truncated =
        bytes.len() > contracts::http::ContainerWorkExecutionReceipt::MAX_OUTPUT_BYTES;
    let bounded = &bytes[..bytes
        .len()
        .min(contracts::http::ContainerWorkExecutionReceipt::MAX_OUTPUT_BYTES)];
    let lossy = String::from_utf8_lossy(bounded);
    let max = contracts::http::ContainerWorkExecutionReceipt::MAX_OUTPUT_BYTES;
    if lossy.len() <= max {
        return (lossy.into_owned(), raw_truncated);
    }
    let end = lossy
        .char_indices()
        .take_while(|(index, character)| index + character.len_utf8() <= max)
        .map(|(index, character)| index + character.len_utf8())
        .last()
        .unwrap_or(0);
    (lossy[..end].to_owned(), true)
}

fn ready_runtime_pod(pod: &Pod) -> bool {
    pod.metadata.deletion_timestamp.is_none()
        && pod.status.as_ref().is_some_and(|status| {
            status.phase.as_deref() == Some("Running")
                && status.conditions.as_ref().is_some_and(|conditions| {
                    conditions
                        .iter()
                        .any(|condition| condition.type_ == "Ready" && condition.status == "True")
                })
                && status.container_statuses.as_ref().is_some_and(|statuses| {
                    statuses
                        .iter()
                        .any(|status| status.name == RUNTIME_CONTAINER && status.ready)
                })
        })
}

#[derive(Debug, thiserror::Error)]
pub enum WorkExecutionError {
    #[error("LW_ENVIRONMENT_WORK_EXECUTION_REQUEST_INVALID")]
    RequestInvalid,
    #[error("LW_ENVIRONMENT_WORK_EXECUTION_NOT_FOUND")]
    NotFound,
    #[error("LW_ENVIRONMENT_WORK_EXECUTION_IDENTITY_MISMATCH")]
    IdentityMismatch,
    #[error("LW_ENVIRONMENT_WORK_EXECUTION_ADMISSION_MISMATCH")]
    AdmissionMismatch,
    #[error("LW_ENVIRONMENT_WORK_EXECUTION_ENVIRONMENT_NOT_ELIGIBLE")]
    EnvironmentNotEligible,
    #[error("LW_ENVIRONMENT_WORK_EXECUTION_DEADLINE_EXCEEDED")]
    DeadlineExceeded,
    #[error("LW_ENVIRONMENT_WORK_EXECUTION_TARGET_UNAVAILABLE")]
    TargetUnavailable,
    #[error("LW_ENVIRONMENT_WORK_EXECUTION_TARGET_AMBIGUOUS")]
    TargetAmbiguous,
    #[error("LW_ENVIRONMENT_WORK_EXECUTION_TARGET_IDENTITY_CHANGED")]
    TargetIdentityChanged,
    #[error("LW_ENVIRONMENT_WORK_EXECUTION_CONCURRENT_MUTATION")]
    ConcurrentMutation,
    #[error("LW_ENVIRONMENT_WORK_EXECUTION_RECEIPT_INVALID")]
    ReceiptInvalid,
    #[error("LW_ENVIRONMENT_WORK_EXECUTION_OBSERVATION_INVALID")]
    ObservationInvalid,
    #[error("LW_ENVIRONMENT_WORK_EXECUTION_CANCELLATION_TIMEOUT")]
    CancellationTimeout,
    #[error("LW_ENVIRONMENT_WORK_EXECUTION_CLOCK_FAILED")]
    Clock,
    #[error("LW_ENVIRONMENT_WORK_EXECUTION_KUBERNETES_FAILED")]
    Kubernetes,
    #[error("LW_ENVIRONMENT_WORK_EXECUTION_RUNNER_FAILED")]
    RunnerFailed,
    #[error("{0}")]
    RunnerFailedWithDiagnostic(String),
    #[error(transparent)]
    Admission(#[from] crate::WorkAdmissionClientError),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Serialization(#[from] serde_json::Error),
    #[error(transparent)]
    Store(#[from] crate::EnvironmentStoreError),
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_BACKEND_ERROR_CONTEXT_CHARS, WorkExecutionError, backend_diagnostic, bound_output,
        bounded_context, remote_command_error,
    };
    use contracts::http::ContainerWorkExecutionReceipt;

    #[test]
    fn output_bound_caps_lossy_utf8_expansion() {
        let bytes = vec![0xff; ContainerWorkExecutionReceipt::MAX_OUTPUT_BYTES];
        let (output, truncated) = bound_output(&bytes);
        assert!(truncated);
        assert!(output.len() <= ContainerWorkExecutionReceipt::MAX_OUTPUT_BYTES);
        assert!(output.chars().all(|character| character == '\u{fffd}'));
    }

    #[test]
    fn output_bound_preserves_utf8_and_marks_raw_overflow() {
        let bytes = vec![b'a'; ContainerWorkExecutionReceipt::MAX_OUTPUT_BYTES + 1];
        let (output, truncated) = bound_output(&bytes);
        assert!(truncated);
        assert_eq!(
            output.len(),
            ContainerWorkExecutionReceipt::MAX_OUTPUT_BYTES
        );
        assert!(output.chars().all(|character| character == 'a'));
    }

    #[test]
    fn backend_failure_has_stable_diagnostic() {
        assert_eq!(
            backend_diagnostic(&WorkExecutionError::TargetIdentityChanged),
            "LW_ENVIRONMENT_WORK_EXECUTION_TARGET_IDENTITY_CHANGED"
        );
        assert_eq!(
            backend_diagnostic(&WorkExecutionError::Kubernetes),
            "LW_ENVIRONMENT_WORK_EXECUTION_BACKEND_FAILED"
        );
        assert_eq!(
            backend_diagnostic(&WorkExecutionError::RunnerFailedWithDiagnostic(
                "LW_ENVIRONMENT_WORK_EXECUTION_OUTPUT_COLLECTOR_TIMEOUT".to_owned(),
            )),
            "LW_ENVIRONMENT_WORK_EXECUTION_OUTPUT_COLLECTOR_TIMEOUT"
        );
    }

    #[test]
    fn remote_runner_diagnostic_is_preserved_without_accepting_arbitrary_stderr() {
        let preserved =
            remote_command_error(b"LW_ENVIRONMENT_WORK_EXECUTION_OUTPUT_COLLECTOR_TIMEOUT");
        assert!(matches!(
            preserved,
            WorkExecutionError::RunnerFailedWithDiagnostic(code)
                if code == "LW_ENVIRONMENT_WORK_EXECUTION_OUTPUT_COLLECTOR_TIMEOUT"
        ));
        assert!(matches!(
            remote_command_error(b"user output"),
            WorkExecutionError::Kubernetes
        ));
    }

    #[test]
    fn backend_error_context_is_bounded() {
        let context = bounded_context("x".repeat(MAX_BACKEND_ERROR_CONTEXT_CHARS + 100));
        assert_eq!(context.chars().count(), MAX_BACKEND_ERROR_CONTEXT_CHARS + 3);
        assert!(context.ends_with("..."));
    }
}
