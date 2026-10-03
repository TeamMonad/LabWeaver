//! Durable recovery for ended authoring sandboxes.
//!
//! The recovery worker reuses the attempt identity and exact persisted Kubernetes references. A
//! fixed usage payload is stored before deletion when timing is available; Resource delivery then
//! retries independently after the owned objects are confirmed absent and the reservation is
//! released.

use artifact_store::S3ImmutableObjectStore;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use contracts::authoring::AgentTrackKind;
use contracts::execution::{ExecutionCleanupStatus, ExecutionObjectRef, TaskExecutionBinding};
use contracts::{AgentRunId, TaskRunId};
use serde_json::Value;
use sqlx::{PgPool, Row, postgres::PgRow};
use task_execution::kubernetes::{KubernetesApiClient, KubernetesJobObservation};
use task_execution::resource::ResourceClient;

use super::{
    PostgresAgentRunStore, SandboxAttemptIntent, SandboxAuthoringProcess,
    checkpoint_sandbox_usage_observation, checkpoint_usage_from_observation,
    cleanup_recovery_with_poll, deliver_usage_payload, load_usage_checkpoint,
    mark_usage_unavailable, release_after_confirmed_cleanup, reproducible_timing,
};

const RECONCILE_INTERVAL: Duration = Duration::from_secs(5);
const DEFERRED_RETRY: Duration = Duration::from_millis(250);
const USAGE_TIMING_UNAVAILABLE: &str = "LW_AGENT_SANDBOX_USAGE_TIMING_UNAVAILABLE";

/// Starts the durable recovery loop. The Agent service owns the returned handle and its shutdown.
pub(super) fn spawn(
    api: KubernetesApiClient,
    resources: ResourceClient,
    store: PostgresAgentRunStore,
    objects: Arc<S3ImmutableObjectStore>,
    configuration: super::SandboxProcessConfiguration,
) -> Option<tokio::task::JoinHandle<()>> {
    let handle = tokio::runtime::Handle::try_current().ok()?;
    Some(handle.spawn(async move {
        Worker {
            api,
            resources,
            store,
            objects,
            configuration,
        }
        .run()
        .await;
    }))
}

struct Worker {
    api: KubernetesApiClient,
    resources: ResourceClient,
    store: PostgresAgentRunStore,
    objects: Arc<S3ImmutableObjectStore>,
    configuration: super::SandboxProcessConfiguration,
}

#[derive(Debug)]
struct PendingAttempt {
    run_id: AgentRunId,
    track: AgentTrackKind,
    attempt: u32,
    task_run_id: uuid::Uuid,
    execution_generation: u64,
    namespace: String,
    workload_name: String,
    binding: Option<Value>,
    objects: Value,
    state: String,
    request_payload: Option<Value>,
    keys: [Option<String>; 3],
}

impl PendingAttempt {
    fn intent(&self) -> Result<SandboxAttemptIntent, crate::run_store::AgentRunStoreError> {
        Ok(SandboxAttemptIntent {
            run_id: self.run_id,
            track: self.track,
            attempt: self.attempt,
            task_run_id: self.task_run_id,
            execution_generation: self.execution_generation,
            namespace: self.namespace.clone(),
            workload_name: self.workload_name.clone(),
            binding: self
                .binding
                .clone()
                .ok_or(crate::run_store::AgentRunStoreError::InvalidContract)?,
        })
    }

    fn from_row(row: &PgRow) -> Result<Self, crate::run_store::AgentRunStoreError> {
        let run_id: uuid::Uuid = row
            .try_get("run_id")
            .map_err(|_| crate::run_store::AgentRunStoreError::PersistenceFailed)?;
        let track: String = row
            .try_get("track")
            .map_err(|_| crate::run_store::AgentRunStoreError::PersistenceFailed)?;
        let attempt_number: i64 = row
            .try_get("attempt_number")
            .map_err(|_| crate::run_store::AgentRunStoreError::PersistenceFailed)?;
        let execution_generation: i64 = row
            .try_get("execution_generation")
            .map_err(|_| crate::run_store::AgentRunStoreError::PersistenceFailed)?;
        let run_id = AgentRunId::from_str(&run_id.to_string())
            .map_err(|_| crate::run_store::AgentRunStoreError::InvalidContract)?;
        let track =
            parse_track(&track).ok_or(crate::run_store::AgentRunStoreError::InvalidContract)?;
        let attempt = u32::try_from(attempt_number)
            .map_err(|_| crate::run_store::AgentRunStoreError::InvalidContract)?;
        let execution_generation = u64::try_from(execution_generation)
            .map_err(|_| crate::run_store::AgentRunStoreError::InvalidContract)?;
        Ok(Self {
            run_id,
            track,
            attempt,
            task_run_id: row
                .try_get("task_run_id")
                .map_err(|_| crate::run_store::AgentRunStoreError::PersistenceFailed)?,
            execution_generation,
            namespace: row
                .try_get("namespace")
                .map_err(|_| crate::run_store::AgentRunStoreError::PersistenceFailed)?,
            workload_name: row
                .try_get("workload_name")
                .map_err(|_| crate::run_store::AgentRunStoreError::PersistenceFailed)?,
            binding: row
                .try_get("binding")
                .map_err(|_| crate::run_store::AgentRunStoreError::PersistenceFailed)?,
            objects: row
                .try_get("objects")
                .map_err(|_| crate::run_store::AgentRunStoreError::PersistenceFailed)?,
            state: row
                .try_get("state")
                .map_err(|_| crate::run_store::AgentRunStoreError::PersistenceFailed)?,
            request_payload: row
                .try_get("request_payload")
                .map_err(|_| crate::run_store::AgentRunStoreError::PersistenceFailed)?,
            keys: [
                row.try_get("result_object_key")
                    .map_err(|_| crate::run_store::AgentRunStoreError::PersistenceFailed)?,
                row.try_get("stderr_object_key")
                    .map_err(|_| crate::run_store::AgentRunStoreError::PersistenceFailed)?,
                row.try_get("export_object_key")
                    .map_err(|_| crate::run_store::AgentRunStoreError::PersistenceFailed)?,
            ],
        })
    }
}

impl Worker {
    async fn run(&self) {
        let mut interval = tokio::time::interval(RECONCILE_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            let attempts = match self.next_pending_batch().await {
                Ok(attempts) => attempts,
                Err(error) => {
                    tracing::error!(
                        event = "agent.authoring.sandbox.cleanup_recovery_scan_failed",
                        error_kind = ?error,
                        "authoring cleanup recovery scan failed",
                    );
                    continue;
                }
            };
            for attempt in attempts {
                let outcome = match self.reconcile(&attempt).await {
                    Ok(true) => continue,
                    Ok(false) => None,
                    Err(error) => {
                        tracing::error!(
                            event = "agent.authoring.sandbox.cleanup_recovery_deferred",
                            task_run_id = %attempt.task_run_id,
                            error_kind = ?error,
                            "authoring cleanup recovery failed and will be retried",
                        );
                        Some(error)
                    }
                };
                if let Err(error) = self.defer(&attempt).await {
                    tracing::error!(
                        event = "agent.authoring.sandbox.cleanup_recovery_defer_failed",
                        task_run_id = %attempt.task_run_id,
                        prior_error = ?outcome,
                        error_kind = ?error,
                        "authoring cleanup recovery could not advance its retry time",
                    );
                }
                tokio::time::sleep(DEFERRED_RETRY).await;
            }
        }
    }

    async fn next_pending_batch(
        &self,
    ) -> Result<Vec<PendingAttempt>, crate::run_store::AgentRunStoreError> {
        select_pending_batch(self.store.pool()).await
    }

    async fn defer(
        &self,
        pending: &PendingAttempt,
    ) -> Result<(), crate::run_store::AgentRunStoreError> {
        let affected = sqlx::query(
            "UPDATE agent.authoring_sandbox_attempts
             SET updated_at=now()
             WHERE run_id=$1 AND track=$2 AND attempt_number=$3 AND execution_generation=$4 AND task_run_id=$5
               AND state IN ('submitted','terminal','failed','cleanup_confirmed','released')",
        )
        .bind(pending.run_id.as_uuid())
        .bind(track_name(pending.track))
        .bind(i64::from(pending.attempt))
        .bind(i64::try_from(pending.execution_generation).map_err(|_|crate::run_store::AgentRunStoreError::InvalidContract)?)
        .bind(pending.task_run_id)
        .execute(self.store.pool())
        .await
        .map_err(|_| crate::run_store::AgentRunStoreError::PersistenceFailed)?
        .rows_affected();
        if affected != 1 {
            return Err(crate::run_store::AgentRunStoreError::StateConflict);
        }
        Ok(())
    }

    async fn reconcile(
        &self,
        pending: &PendingAttempt,
    ) -> Result<bool, crate::run_store::AgentRunStoreError> {
        let task_run_id = TaskRunId::from_str(&pending.task_run_id.to_string())
            .map_err(|_| crate::run_store::AgentRunStoreError::InvalidContract)?;
        if pending.binding.is_none() {
            return self.reconcile_creating(pending, task_run_id).await;
        }
        let binding: TaskExecutionBinding = serde_json::from_value(
            pending
                .binding
                .clone()
                .ok_or(crate::run_store::AgentRunStoreError::InvalidContract)?,
        )
        .map_err(|_| crate::run_store::AgentRunStoreError::InvalidContract)?;
        if binding.task_run_id != task_run_id
            || binding.execution_generation != pending.execution_generation
            || binding.namespace != pending.namespace
            || binding.workload_name != pending.workload_name
            || binding.validate().is_err()
        {
            tracing::error!(
                event = "agent.authoring.sandbox.cleanup_recovery_rejected",
                failure_stage = "checkpoint.identity",
                task_run_id = %task_run_id.as_uuid(),
                "durable sandbox identity does not match its execution binding",
            );
            return Ok(false);
        }
        let intent = pending.intent()?;
        let persisted_usage = load_usage_checkpoint(&self.store, &intent).await?;
        let persisted_observation = self.store.load_sandbox_usage_observation(&intent).await?;
        if pending.state == "released" {
            return self
                .reconcile_released_attempt(
                    &intent,
                    binding,
                    persisted_usage,
                    persisted_observation,
                )
                .await;
        }
        self.reconcile_pending_attempt(
            pending,
            task_run_id,
            binding,
            intent,
            persisted_usage,
            persisted_observation,
        )
        .await
    }

    async fn capture_receipt(
        &self,
        pending: &PendingAttempt,
        intent: &SandboxAttemptIntent,
        message: &str,
    ) -> Result<(), super::TerminalReceiptError> {
        use super::TerminalReceiptError::{Invalid, Unavailable};
        let saved = self
            .store
            .load_sandbox_attempt(
                pending.run_id,
                pending.track,
                pending.attempt,
                pending.execution_generation,
            )
            .await
            .map_err(|_| Unavailable)?
            .ok_or(Invalid)?;
        if saved.state == "failed" || saved.diagnostic_code.is_some() {
            return Ok(());
        }
        let keys = [
            pending.keys[0].as_deref().ok_or(Invalid)?,
            pending.keys[1].as_deref().ok_or(Invalid)?,
            pending.keys[2].as_deref().ok_or(Invalid)?,
        ];
        let version = self
            .store
            .sandbox_claude_version(pending.run_id)
            .await
            .map_err(|error| match error {
                crate::run_store::AgentRunStoreError::InvalidContract => Invalid,
                _ => Unavailable,
            })?;
        if let Some(value) = saved.terminal_receipt {
            let frozen: super::FrozenSandboxReceipt =
                serde_json::from_value(value).map_err(|_| Invalid)?;
            frozen
                .receipt
                .validate_version(
                    &version,
                    self.configuration.result_max_bytes,
                    self.configuration.stderr_max_bytes,
                    self.configuration.sandbox.workspace_bytes,
                )
                .map_err(|_| Invalid)?;
            if !frozen.matches_identity(pending.task_run_id, keys)
                || !frozen.valid_metadata(self.objects.binding())
            {
                return Err(Invalid);
            }
            frozen.assemble(self.objects.as_ref(), true).await?;
            return Ok(());
        }
        let receipt = super::parse_receipt(message).map_err(|_| Invalid)?;
        receipt
            .validate_version(
                &version,
                self.configuration.result_max_bytes,
                self.configuration.stderr_max_bytes,
                self.configuration.sandbox.workspace_bytes,
            )
            .map_err(|_| Invalid)?;
        for (key, name) in keys.iter().zip(["result.json", "stderr.log", "export.tar"]) {
            if *key
                != super::object_key(
                    &self.configuration.object_prefix,
                    TaskRunId::from_str(&pending.task_run_id.to_string()).map_err(|_| Invalid)?,
                    name,
                )
            {
                return Err(Invalid);
            }
        }
        let frozen = super::freeze_terminal_receipt(
            self.objects.as_ref(),
            pending.task_run_id,
            &receipt,
            keys,
        )
        .await?;
        self.store
            .checkpoint_sandbox_receipt(
                intent,
                self.objects.binding(),
                &serde_json::to_value(frozen).map_err(|_| Invalid)?,
            )
            .await
            .map_err(|error| match error {
                crate::run_store::AgentRunStoreError::InvalidContract
                | crate::run_store::AgentRunStoreError::IdentityMismatch => Invalid,
                _ => Unavailable,
            })?;
        self.store
            .complete_sandbox_attempt(
                intent,
                Some((keys[0], &receipt.result_sha256, receipt.result_size_bytes)),
                receipt.exit_code,
                None,
            )
            .await
            .map_err(|_| Unavailable)
    }

    async fn reconcile_creating(
        &self,
        pending: &PendingAttempt,
        task: TaskRunId,
    ) -> Result<bool, crate::run_store::AgentRunStoreError> {
        if pending.state != "creating" {
            return Err(crate::run_store::AgentRunStoreError::InvalidContract);
        }
        let request: super::SandboxResourceRequest = serde_json::from_value(
            pending
                .request_payload
                .clone()
                .ok_or(crate::run_store::AgentRunStoreError::InvalidContract)?,
        )
        .map_err(|_| crate::run_store::AgentRunStoreError::InvalidContract)?;
        let lifecycle = request
            .lifecycle(&self.resources, task)
            .map_err(|_| crate::run_store::AgentRunStoreError::StateConflict)?;
        // Settle the same immutable POST, including ambiguous accepted requests, then cancel it.
        lifecycle
            .create()
            .await
            .map_err(|_| crate::run_store::AgentRunStoreError::StateConflict)?;
        let identity = SandboxAuthoringProcess::attempt_identity(
            &pending.namespace,
            &pending.workload_name,
            super::attempt_ownership_from_parts(pending.run_id.as_uuid(), task, &request.trace_id),
            &request.trace_id,
        );
        let cleanup = self
            .api
            .cleanup_intent(
                &identity,
                &crate::sandbox::sandbox_cleanup_targets(&pending.namespace, task.as_uuid()),
            )
            .await
            .map_err(|_| crate::run_store::AgentRunStoreError::StateConflict)?;
        if cleanup != ExecutionCleanupStatus::Confirmed {
            return Ok(false);
        }
        lifecycle
            .cancel("authoring generation recovered before submission")
            .await
            .map_err(|_| crate::run_store::AgentRunStoreError::StateConflict)?;
        let request = self
            .resources
            .get_task_resource_request(task)
            .await
            .map_err(|_| crate::run_store::AgentRunStoreError::StateConflict)?;
        match request.state {
            contracts::resource::ResourceRequestState::Expired
            | contracts::resource::ResourceRequestState::Cancelled
            | contracts::resource::ResourceRequestState::Rejected => {}
            _ => {
                let status = self
                    .resources
                    .get_task_resource(task)
                    .await
                    .map_err(|_| crate::run_store::AgentRunStoreError::StateConflict)?;
                if !status.cleanup_confirmed {
                    return Ok(false);
                }
            }
        }
        self.store
            .release_creating_sandbox(
                pending.run_id,
                pending.track,
                pending.attempt,
                pending.task_run_id,
                pending.execution_generation,
            )
            .await?;
        Ok(true)
    }

    async fn reconcile_released_attempt(
        &self,
        intent: &SandboxAttemptIntent,
        binding: TaskExecutionBinding,
        persisted_usage: Option<super::SandboxUsageCheckpoint>,
        persisted_observation: Option<(
            contracts::execution::ExecutionObservation,
            contracts::UtcTimestamp,
        )>,
    ) -> Result<bool, crate::run_store::AgentRunStoreError> {
        let usage = match persisted_usage {
            Some(usage) => Some(usage),
            None => {
                checkpoint_usage_from_observation(
                    &self.store,
                    &self.resources,
                    intent,
                    &binding,
                    persisted_observation.as_ref(),
                )
                .await?
            }
        };
        if usage.is_none() && persisted_observation.is_some() {
            return Ok(false);
        }
        self.deliver_usage_checkpoint(intent, usage).await
    }

    // The ordering is safety-critical: save usage intent, prove exact ownership, delete, then
    // release independently from metering delivery.
    #[allow(clippy::too_many_lines)]
    async fn reconcile_pending_attempt(
        &self,
        pending: &PendingAttempt,
        task_run_id: TaskRunId,
        binding: TaskExecutionBinding,
        intent: SandboxAttemptIntent,
        persisted_usage: Option<super::SandboxUsageCheckpoint>,
        persisted_observation: Option<(
            contracts::execution::ExecutionObservation,
            contracts::UtcTimestamp,
        )>,
    ) -> Result<bool, crate::run_store::AgentRunStoreError> {
        let identity = SandboxAuthoringProcess::attempt_identity(
            &pending.namespace,
            &pending.workload_name,
            super::attempt_ownership_from_parts(
                pending.run_id.as_uuid(),
                task_run_id,
                &binding.trace_id,
            ),
            &binding.trace_id,
        );
        let objects: Vec<ExecutionObjectRef> = serde_json::from_value(pending.objects.clone())
            .map_err(|_| crate::run_store::AgentRunStoreError::InvalidContract)?;
        let observed = match self
            .api
            .observe(&identity, super::job_uid(&objects).as_deref())
            .await
        {
            Ok(KubernetesJobObservation::Completed {
                message,
                observation,
            }) => {
                let captured = self.capture_receipt(pending, &intent, &message).await;
                match captured {
                    Ok(()) => {}
                    Err(super::TerminalReceiptError::Unavailable) => return Ok(false),
                    Err(super::TerminalReceiptError::Invalid) => {
                        match self
                            .store
                            .complete_sandbox_attempt(
                                &intent,
                                None,
                                1,
                                Some("LW_AGENT_SANDBOX_RECEIPT_INVALID"),
                            )
                            .await
                        {
                            Ok(()) => {}
                            Err(crate::run_store::AgentRunStoreError::StateConflict) => {
                                let saved = self
                                    .store
                                    .load_sandbox_attempt(
                                        pending.run_id,
                                        pending.track,
                                        pending.attempt,
                                        pending.execution_generation,
                                    )
                                    .await?
                                    .ok_or(crate::run_store::AgentRunStoreError::StateConflict)?;
                                if saved.diagnostic_code.is_none() {
                                    return Err(
                                        crate::run_store::AgentRunStoreError::StateConflict,
                                    );
                                }
                            }
                            Err(error) => return Err(error),
                        }
                    }
                }
                Some(observation)
            }
            Ok(KubernetesJobObservation::Failed { observation, .. }) => Some(observation),
            Ok(KubernetesJobObservation::Missing) => None,
            Ok(KubernetesJobObservation::Running) => {
                tracing::warn!(
                    event = "agent.authoring.sandbox.cleanup_recovery_deferred",
                    failure_stage = "sandbox.observation",
                    task_run_id = %task_run_id.as_uuid(),
                    "authoring attempt is still running; cleanup will wait for terminal state",
                );
                return Ok(false);
            }
            Err(error) => {
                tracing::warn!(
                    event = "agent.authoring.sandbox.cleanup_recovery_deferred",
                    failure_stage = "sandbox.observation",
                    task_run_id = %task_run_id.as_uuid(),
                    error_kind = error.error_kind(),
                    "Kubernetes could not confirm an ended authoring attempt",
                );
                return Ok(false);
            }
        };
        let usage_observation = match persisted_observation {
            Some(observation) => Some(observation),
            None => match observed.as_ref() {
                Some(observation) if reproducible_timing(observation).is_some() => {
                    checkpoint_sandbox_usage_observation(&self.store, &intent, observation).await?
                }
                _ => None,
            },
        };
        let usage = match persisted_usage {
            Some(usage) => Some(usage),
            None if usage_observation.is_some() => {
                checkpoint_usage_from_observation(
                    &self.store,
                    &self.resources,
                    &intent,
                    &binding,
                    usage_observation.as_ref(),
                )
                .await?
            }
            None => mark_usage_unavailable(&self.store, &intent, USAGE_TIMING_UNAVAILABLE).await?,
        };
        let cleanup = if objects.is_empty() {
            self.api
                .cleanup_intent(
                    &identity,
                    &crate::sandbox::sandbox_cleanup_targets(
                        &pending.namespace,
                        task_run_id.as_uuid(),
                    ),
                )
                .await
                .map_err(|_| crate::run_store::AgentRunStoreError::StateConflict)?
        } else {
            cleanup_recovery_with_poll(&self.api, &identity, &objects).await
        };
        if cleanup != ExecutionCleanupStatus::Confirmed {
            return Ok(false);
        }
        let release = release_after_confirmed_cleanup(&self.store, &intent, &self.resources).await;
        let delivery = if usage.is_none() && usage_observation.is_some() {
            Ok(false)
        } else {
            self.deliver_usage_checkpoint(&intent, usage).await
        };
        if let Err(error) = &release {
            tracing::warn!(
                event = "agent.authoring.sandbox.cleanup_recovery_deferred",
                failure_stage = "sandbox.release",
                task_run_id = %task_run_id.as_uuid(),
                error_kind = ?error,
                "owned sandbox objects are absent but Resource release remains pending",
            );
        }
        if release.is_err() {
            return match delivery {
                Ok(_) => Ok(false),
                Err(error) => Err(error),
            };
        }
        delivery
    }

    async fn deliver_usage_checkpoint(
        &self,
        intent: &SandboxAttemptIntent,
        usage: Option<super::SandboxUsageCheckpoint>,
    ) -> Result<bool, crate::run_store::AgentRunStoreError> {
        let Some(usage) = usage else {
            return Ok(true);
        };
        if usage.delivered {
            return Ok(true);
        }
        let task_run_id = TaskRunId::from_str(&intent.task_run_id.to_string())
            .map_err(|_| crate::run_store::AgentRunStoreError::InvalidContract)?;
        if !deliver_usage_payload(&self.resources, &task_run_id, &usage.deliveries).await {
            return Ok(false);
        }
        self.store.mark_sandbox_usage_delivered(intent).await?;
        Ok(true)
    }
}

async fn select_pending_batch(
    pool: &PgPool,
) -> Result<Vec<PendingAttempt>, crate::run_store::AgentRunStoreError> {
    let rows = sqlx::query(
        "SELECT run_id,track,attempt_number,task_run_id,execution_generation,
                    namespace,workload_name,binding,objects,state,request_payload,result_object_key,stderr_object_key,export_object_key
             FROM agent.authoring_sandbox_attempts
             WHERE updated_at <= now() - interval '3 seconds'
               AND ((state IN ('creating','submitted','terminal','failed','cleanup_confirmed'))
                    OR (state='released' AND usage_payload IS NOT NULL
                        AND NOT usage_delivered)
                    OR (state='released' AND usage_observation IS NOT NULL
                        AND usage_payload IS NULL))
               AND (state NOT IN ('creating','submitted') OR NOT EXISTS (
                    SELECT 1 FROM agent.agent_track_work_items work
                    WHERE work.run_id=agent.authoring_sandbox_attempts.run_id
                      AND work.track=agent.authoring_sandbox_attempts.track
                      AND work.attempt_number=agent.authoring_sandbox_attempts.attempt_number
                      AND work.state='running'
                      AND work.lease_expires_at > clock_timestamp()
               ))
             ORDER BY updated_at,run_id,track,attempt_number,execution_generation
             LIMIT 32",
    )
    .fetch_all(pool)
    .await
    .map_err(|_| crate::run_store::AgentRunStoreError::PersistenceFailed)?;
    let mut attempts = Vec::with_capacity(rows.len());
    for row in rows {
        let run_id = row.try_get::<uuid::Uuid, _>("run_id");
        let track = row.try_get::<String, _>("track");
        let attempt = row.try_get::<i64, _>("attempt_number");
        let generation = row.try_get::<i64, _>("execution_generation");
        let task = row.try_get::<uuid::Uuid, _>("task_run_id");
        match PendingAttempt::from_row(&row) {
            Ok(attempt) => attempts.push(attempt),
            Err(error) => {
                tracing::error!(
                    event = "agent.authoring.sandbox.cleanup_recovery_row_invalid",
                    error_kind = ?error,
                    "authoring cleanup recovery row is invalid and will be deferred",
                );
                if let (Ok(run_id), Ok(track), Ok(attempt), Ok(generation), Ok(task)) =
                    (run_id, track, attempt, generation, task)
                {
                    let deferred = sqlx::query(
                        "UPDATE agent.authoring_sandbox_attempts SET updated_at=now()
                             WHERE run_id=$1 AND track=$2 AND attempt_number=$3 AND execution_generation=$4 AND task_run_id=$5",
                    )
                    .bind(run_id)
                    .bind(track)
                    .bind(attempt).bind(generation).bind(task)
                    .execute(pool)
                    .await
                    .map_err(|_| crate::run_store::AgentRunStoreError::PersistenceFailed)?
                    .rows_affected();
                    if deferred != 1 {
                        tracing::warn!(
                            event = "agent.authoring.sandbox.cleanup_recovery_row_defer_missed",
                            run_id = %run_id,
                            "invalid authoring cleanup row changed before it could be deferred",
                        );
                    }
                } else {
                    return Err(error);
                }
            }
        }
    }
    Ok(attempts)
}

fn parse_track(value: &str) -> Option<AgentTrackKind> {
    match value {
        "environment" => Some(AgentTrackKind::Environment),
        "evaluation" => Some(AgentTrackKind::Evaluation),
        "work_configuration" => Some(AgentTrackKind::WorkConfiguration),
        _ => None,
    }
}

fn track_name(track: AgentTrackKind) -> &'static str {
    match track {
        AgentTrackKind::Environment => "environment",
        AgentTrackKind::Evaluation => "evaluation",
        AgentTrackKind::WorkConfiguration => "work_configuration",
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        error::Error,
        io::Cursor,
        path::{Path, PathBuf},
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use auth::{
        ServiceTokenClient, ServiceTokenClientConfig, TransportSecurityMode,
        no_redirect_http_client,
    };
    use axum::{
        Json, Router,
        extract::{Form, Path as AxumPath, State},
        http::StatusCode,
        response::{IntoResponse, Response},
        routing::{get, post},
    };
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use contracts::{
        ActorId, AgentRunId, CapacityClaimId, CourseId, LeaseId, ProjectId, ResourceApprovalId,
        ResourceRequestId, TaskRunId, UsageRecordId, UtcTimestamp,
        authoring::AgentTrackKind,
        execution::{ExecutionObjectRef, ExecutionObservation, ExecutionWorkloadState},
        http::{RecordResourceUsageRequest, TaskResourceStatus},
    };
    use hyper_util::{
        rt::{TokioExecutor, TokioIo},
        server::conn::auto::Builder,
        service::TowerToHyperService,
    };
    use persistence_sqlx::{Domain, MigrationCatalog};
    use rcgen::generate_simple_self_signed;
    use reqwest::Url;
    use rustls::{ServerConfig, pki_types::PrivateKeyDer};
    use serde::Deserialize;
    use serde_json::{Value, json};
    use sqlx::postgres::PgPoolOptions;
    use task_execution::{
        kubernetes::KubernetesApiClient, resource::required_scopes_for_diagnostics,
    };
    use task_execution::{
        kubernetes::{KubernetesApiConfiguration, KubernetesJobIdentity, KubernetesOwnership},
        resource::{ResourceClient, ResourceClientConfiguration},
    };
    use testcontainers::{ImageExt, runners::AsyncRunner};
    use testcontainers_modules::postgres::Postgres;
    use tokio::{net::TcpListener, task::JoinHandle};
    use tokio_rustls::TlsAcceptor;

    use super::{Worker, parse_track, reproducible_timing, select_pending_batch};
    use crate::{
        run_store::{PostgresAgentRunStore, SandboxAttemptIntent},
        sandbox::{SANDBOX_EVENT_SCOPE, SANDBOX_MAIN_CONTAINER, SANDBOX_MANAGED_BY},
        sandbox_process::{SandboxAuthoringProcess, attempt_ownership_from_parts},
    };

    async fn test_objects() -> Result<Arc<artifact_store::S3ImmutableObjectStore>, Box<dyn Error>> {
        Ok(Arc::new(
            artifact_store::S3ImmutableObjectStore::new(
                artifact_store::S3StoreConfig {
                    binding: "test-objects".to_owned(),
                    endpoint: "https://objects.invalid".parse()?,
                    bucket: "test".to_owned(),
                    region: "test".to_owned(),
                    object_prefix: "authoring".to_owned(),
                    upload_ttl_seconds: 60,
                    max_object_bytes: 1024 * 1024,
                    force_path_style: true,
                    ca_bundle_file: None,
                },
                artifact_store::S3Credential {
                    access_key_id: "test".to_owned(),
                    secret_access_key: "test".to_owned(),
                    session_token: None,
                },
            )
            .await?,
        ))
    }
    fn test_configuration() -> super::super::SandboxProcessConfiguration {
        super::super::SandboxProcessConfiguration {
            sandbox: crate::sandbox::SandboxConfiguration {
                namespace: "runner".to_owned(),
                image: format!("test@sha256:{}", "a".repeat(64)),
                service_account_name: "runner".to_owned(),
                image_pull_secret_name: None,
                cpu_millicores: 100,
                memory_bytes: 1024 * 1024,
                workspace_bytes: 1024 * 1024,
                wall_time_seconds: 900,
                allowed_egress: std::collections::BTreeSet::from(["10.0.0.1/32:443".to_owned()]),
                buildkit_image: None,
                buildkit_config_map_name: None,
            },
            object_prefix: "authoring".to_owned(),
            result_max_bytes: 1024 * 1024,
            stderr_max_bytes: 1024 * 1024,
            kubernetes_api_server: "https://kubernetes.invalid".to_owned(),
            kubernetes_bearer_token_file: "unused".to_owned(),
            kubernetes_ca_file: "unused".to_owned(),
            request_timeout_milliseconds: 5000,
            object_store_ca_file: None,
            worker_environment: std::collections::BTreeMap::new(),
        }
    }

    async fn apply_agent_migrations(pool: &sqlx::PgPool) -> Result<(), Box<dyn Error>> {
        sqlx::query("CREATE SCHEMA agent").execute(pool).await?;
        let mut connection = pool.acquire().await?;
        sqlx::query("SET search_path = agent, pg_catalog")
            .execute(&mut *connection)
            .await?;
        let migration_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations");
        let catalog = MigrationCatalog::load(&migration_root.join("catalog.yaml"))?;
        let migrations = catalog
            .domains
            .iter()
            .find(|domain| domain.name == Domain::Agent)
            .ok_or_else(|| std::io::Error::other("migration catalog has no agent domain"))?;
        for migration in &migrations.migrations {
            let sql = MigrationCatalog::read_verified_sql(&migration_root, migration)?;
            sqlx::raw_sql(&sql).execute(&mut *connection).await?;
        }
        Ok(())
    }

    #[test]
    fn parses_only_persisted_authoring_tracks() {
        assert_eq!(
            parse_track("environment"),
            Some(AgentTrackKind::Environment)
        );
        assert_eq!(parse_track("evaluation"), Some(AgentTrackKind::Evaluation));
        assert_eq!(
            parse_track("work_configuration"),
            Some(AgentTrackKind::WorkConfiguration)
        );
        assert_eq!(parse_track("unknown"), None);
    }

    #[test]
    fn recovery_only_uses_a_reproducible_timing_interval() -> Result<(), Box<dyn std::error::Error>>
    {
        let started_at: UtcTimestamp = "2026-09-30T10:00:00.000Z".parse()?;
        let terminated_at: UtcTimestamp = "2026-09-30T10:00:01.000Z".parse()?;
        let known = ExecutionObservation {
            state: ExecutionWorkloadState::Succeeded,
            exit_code: Some(0),
            reason_code: None,
            pod_name: Some("sandbox".to_owned()),
            started_at: Some(started_at),
            terminated_at: Some(terminated_at),
        };
        assert!(reproducible_timing(&known).is_some());
        let missing = ExecutionObservation {
            started_at: None,
            terminated_at: None,
            ..known
        };
        assert!(reproducible_timing(&missing).is_none());
        Ok(())
    }

    #[tokio::test]
    async fn submitted_cleanup_waits_for_the_current_track_lease_to_expire()
    -> Result<(), Box<dyn Error>> {
        let postgres = Postgres::default().with_tag("17.5-alpine").start().await?;
        let database_url = format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            postgres.get_host_port_ipv4(5432).await?
        );
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect(&database_url)
            .await?;
        apply_agent_migrations(&pool).await?;

        let run_id = uuid::Uuid::new_v4();
        let task_run_id = uuid::Uuid::new_v4();
        let lease_token = uuid::Uuid::new_v4();
        sqlx::query(
            "INSERT INTO agent.agent_runs
             (run_id,project_id,course_id,problem_package_id,revision,state,provider_binding,
              input_sha256,policy_revision,purpose,contract)
             VALUES ($1,$2,$3,$4,1,'requested','claude-code-v1',repeat('a',64),1,
                     '{\"kind\":\"authoring\",\"environmentClass\":\"experiment\"}',
                     '{\"state\":\"requested\"}')",
        )
        .bind(run_id)
        .bind(uuid::Uuid::new_v4())
        .bind(uuid::Uuid::new_v4())
        .bind(uuid::Uuid::new_v4())
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO agent.agent_track_work_items
             (run_id,track,state,input_sha256,attempt_number,worker_id,lease_token,
              lease_expires_at,heartbeat_at)
             VALUES ($1,'environment','running',repeat('a',64),1,'cleanup-test',$2,
                     clock_timestamp()+interval '5 minutes',clock_timestamp())",
        )
        .bind(run_id)
        .bind(lease_token)
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO agent.authoring_sandbox_attempts
             (run_id,track,attempt_number,task_run_id,execution_generation,namespace,
              workload_name,binding,objects,state,updated_at)
             VALUES ($1,'environment',1,$2,1,'runner','sandbox-test','{}','[]','submitted',
                     clock_timestamp()-interval '10 seconds')",
        )
        .bind(run_id)
        .bind(task_run_id)
        .execute(&pool)
        .await?;

        assert!(select_pending_batch(&pool).await?.is_empty());
        sqlx::query(
            "UPDATE agent.agent_track_work_items
             SET lease_expires_at=clock_timestamp()-interval '1 second'
             WHERE run_id=$1 AND track='environment'",
        )
        .bind(run_id)
        .execute(&pool)
        .await?;
        let pending = select_pending_batch(&pool).await?;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].task_run_id, task_run_id);
        Ok(())
    }

    // Keep the PostgreSQL lease, fake service boundaries, cleanup, and retry sequence together.
    #[allow(clippy::too_many_lines)]
    #[tokio::test]
    async fn usage_outage_releases_after_owned_cleanup_and_restart_retries_the_fixed_payload()
    -> Result<(), Box<dyn Error>> {
        let postgres = Postgres::default().with_tag("17.5-alpine").start().await?;
        let database_url = format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            postgres.get_host_port_ipv4(5432).await?
        );
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect(&database_url)
            .await?;
        apply_agent_migrations(&pool).await?;
        let store = PostgresAgentRunStore::new(pool.clone());

        let mut oldest_deferred_run = None;
        for index in 0..32 {
            let deferred_run_id = AgentRunId::new();
            let deferred_task_run_id = uuid::Uuid::new_v4();
            if index == 0 {
                oldest_deferred_run = Some(deferred_run_id);
            }
            insert_agent_run(&pool, deferred_run_id).await?;
            let workload_name = format!(
                "deferred-{}",
                &deferred_task_run_id.simple().to_string()[..20]
            );
            sqlx::query(
                "INSERT INTO agent.authoring_sandbox_attempts
                 (run_id,track,attempt_number,task_run_id,execution_generation,namespace,
                  workload_name,binding,objects,state,updated_at)
                 VALUES ($1,'environment',1,$2,1,'runner',$3,'{}','[]','failed',
                         CASE WHEN $4 THEN now()-interval '1 hour'
                              ELSE now()-interval '20 seconds' END)",
            )
            .bind(deferred_run_id.as_uuid())
            .bind(deferred_task_run_id)
            .bind(workload_name)
            .bind(index == 0)
            .execute(&pool)
            .await?;
        }

        let run_id = AgentRunId::new();
        let task_run_id = TaskRunId::new();
        let workload_name = format!(
            "lw-auth-{}",
            &task_run_id.as_uuid().simple().to_string()[..20]
        );
        let resource_status = resource_status_value(task_run_id, false);
        let typed_status: TaskResourceStatus = serde_json::from_value(resource_status.clone())?;
        let binding = contracts::execution::TaskExecutionBinding::from_admitted_status(
            &typed_status,
            1,
            workload_name.clone(),
            "cleanup-restart-test",
        )?;
        let intent = SandboxAttemptIntent {
            run_id,
            track: AgentTrackKind::Environment,
            attempt: 1,
            task_run_id: task_run_id.as_uuid(),
            execution_generation: 1,
            namespace: "runner".to_owned(),
            workload_name: workload_name.clone(),
            binding: serde_json::to_value(binding)?,
        };
        insert_agent_run(&pool, run_id).await?;
        store.begin_sandbox_attempt(&intent).await?;
        let objects = vec![ExecutionObjectRef {
            api_version: "batch/v1".to_owned(),
            resource: "jobs".to_owned(),
            name: workload_name.clone(),
            uid: "job-uid-1".to_owned(),
        }];
        store
            .record_sandbox_objects(&intent, &serde_json::to_value(&objects)?)
            .await?;
        store
            .complete_sandbox_attempt(&intent, None, 1, Some("LW_TEST_TERMINAL"))
            .await?;
        sqlx::query(
            "UPDATE agent.authoring_sandbox_attempts
             SET updated_at=now()-interval '10 seconds'
             WHERE run_id=$1 AND track='environment' AND attempt_number=1",
        )
        .bind(run_id.as_uuid())
        .execute(&pool)
        .await?;

        let fake_resource = spawn_resource_client(resource_status, 100).await?;
        let ownership =
            attempt_ownership_from_parts(run_id.as_uuid(), task_run_id, "cleanup-restart-test");
        let identity = SandboxAuthoringProcess::attempt_identity(
            "runner",
            &workload_name,
            ownership.clone(),
            "cleanup-restart-test",
        );
        let fake_kube = spawn_kubernetes(identity.clone(), ownership, true).await?;
        let token_file = tempfile::NamedTempFile::new()?;
        std::fs::write(token_file.path(), b"test-kubernetes-token")?;
        let api = KubernetesApiClient::for_test(
            KubernetesApiConfiguration {
                kubernetes_api_server: Url::parse(&fake_kube.base_uri)?,
                kubernetes_bearer_token_file: token_file.path().to_owned(),
                kubernetes_ca_file: PathBuf::from("unused-test-ca"),
                runner_namespace: "runner".to_owned(),
                request_timeout_milliseconds: 5_000,
            },
            reqwest::Client::builder().no_proxy().build()?,
            "labweaver-agent-test",
            "agent-test",
            "LW_AGENT_",
            SANDBOX_MANAGED_BY,
            SANDBOX_EVENT_SCOPE,
        );
        let worker = Worker {
            api: api.clone(),
            resources: fake_resource.client.clone(),
            store: store.clone(),
            objects: test_objects().await?,
            configuration: test_configuration(),
        };

        let first_batch = select_pending_batch(&pool).await?;
        let oldest_deferred_run = oldest_deferred_run.ok_or("oldest row was not inserted")?;
        assert_eq!(first_batch.len(), 32);
        assert_eq!(first_batch[0].run_id, oldest_deferred_run);
        assert!(
            !first_batch
                .iter()
                .any(|attempt| attempt.task_run_id == task_run_id.as_uuid())
        );
        assert!(worker.reconcile(&first_batch[0]).await.is_err());
        worker.defer(&first_batch[0]).await?;

        let pending = select_pending_batch(&pool)
            .await?
            .into_iter()
            .find(|attempt| attempt.task_run_id == task_run_id.as_uuid())
            .ok_or("later sandbox attempt must enter the next batch")?;
        assert!(!worker.reconcile(&pending).await?);
        let (saved_payload, delivered) = store
            .load_sandbox_usage(&intent)
            .await?
            .ok_or("usage payload must be durable before owned cleanup")?;
        assert!(!delivered);
        let state: String = sqlx::query_scalar(
            "SELECT state FROM agent.authoring_sandbox_attempts
             WHERE run_id=$1 AND track='environment' AND attempt_number=1",
        )
        .bind(run_id.as_uuid())
        .fetch_one(&pool)
        .await?;
        assert_eq!(state, "released");
        assert!(!fake_kube.state.job_exists.load(Ordering::SeqCst));
        assert_eq!(fake_resource.state.release_count.load(Ordering::SeqCst), 1);
        let expected_payloads = saved_payload
            .as_array()
            .ok_or("persisted usage payload must be a delivery array")?;
        {
            let first_posts = fake_resource
                .state
                .usage_posts
                .lock()
                .map_err(|_| "resource state poisoned")?;
            assert_eq!(first_posts.len(), expected_payloads.len() * 3);
            for expected in expected_payloads {
                assert_eq!(
                    first_posts.iter().filter(|post| *post == expected).count(),
                    3
                );
            }
        }

        sqlx::query(
            "UPDATE agent.authoring_sandbox_attempts
             SET updated_at=now()-interval '10 seconds'
             WHERE run_id=$1 AND track='environment' AND attempt_number=1",
        )
        .bind(run_id.as_uuid())
        .execute(&pool)
        .await?;
        let recovered = select_pending_batch(&pool)
            .await?
            .into_iter()
            .find(|attempt| attempt.task_run_id == task_run_id.as_uuid())
            .ok_or("released undelivered usage must be selected after restart")?;
        let restarted = Worker {
            api,
            resources: fake_resource.client.clone(),
            store: store.clone(),
            objects: test_objects().await?,
            configuration: test_configuration(),
        };
        fake_resource
            .state
            .failures_remaining
            .store(0, Ordering::SeqCst);
        assert!(restarted.reconcile(&recovered).await?);
        let (_, delivered) = store
            .load_sandbox_usage(&intent)
            .await?
            .ok_or("usage payload remains durable after retry")?;
        assert!(delivered);
        let posts = fake_resource
            .state
            .usage_posts
            .lock()
            .map_err(|_| "resource state poisoned")?;
        assert_eq!(posts.len(), expected_payloads.len() * 4);
        for expected in expected_payloads {
            assert_eq!(posts.iter().filter(|post| *post == expected).count(), 4);
        }
        assert_eq!(fake_resource.state.release_count.load(Ordering::SeqCst), 1);
        assert!(!fake_kube.state.job_exists.load(Ordering::SeqCst));
        Ok(())
    }

    // Keep the legacy-state diagnostic and exact-owned release assertions in one flow.
    #[allow(clippy::too_many_lines)]
    #[tokio::test]
    async fn legacy_missing_attempt_records_unknown_timing_and_releases()
    -> Result<(), Box<dyn Error>> {
        let postgres = Postgres::default().with_tag("17.5-alpine").start().await?;
        let database_url = format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            postgres.get_host_port_ipv4(5432).await?
        );
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect(&database_url)
            .await?;
        apply_agent_migrations(&pool).await?;
        let store = PostgresAgentRunStore::new(pool.clone());

        let run_id = AgentRunId::new();
        let task_run_id = TaskRunId::new();
        let workload_name = format!(
            "lw-auth-{}",
            &task_run_id.as_uuid().simple().to_string()[..20]
        );
        let resource_status = resource_status_value(task_run_id, false);
        let typed_status: TaskResourceStatus = serde_json::from_value(resource_status.clone())?;
        let binding = contracts::execution::TaskExecutionBinding::from_admitted_status(
            &typed_status,
            1,
            workload_name.clone(),
            "legacy-missing-test",
        )?;
        let intent = SandboxAttemptIntent {
            run_id,
            track: AgentTrackKind::Environment,
            attempt: 1,
            task_run_id: task_run_id.as_uuid(),
            execution_generation: 1,
            namespace: "runner".to_owned(),
            workload_name: workload_name.clone(),
            binding: serde_json::to_value(binding)?,
        };
        insert_agent_run(&pool, run_id).await?;
        store.begin_sandbox_attempt(&intent).await?;
        let objects = vec![ExecutionObjectRef {
            api_version: "batch/v1".to_owned(),
            resource: "jobs".to_owned(),
            name: workload_name.clone(),
            uid: "job-uid-1".to_owned(),
        }];
        store
            .record_sandbox_objects(&intent, &serde_json::to_value(&objects)?)
            .await?;
        store
            .complete_sandbox_attempt(&intent, None, 1, Some("LW_TEST_LEGACY_MISSING"))
            .await?;
        sqlx::query(
            "UPDATE agent.authoring_sandbox_attempts
             SET updated_at=now()-interval '10 seconds'
             WHERE run_id=$1 AND track='environment' AND attempt_number=1",
        )
        .bind(run_id.as_uuid())
        .execute(&pool)
        .await?;
        let pending = select_pending_batch(&pool)
            .await?
            .into_iter()
            .find(|attempt| attempt.task_run_id == task_run_id.as_uuid())
            .ok_or("legacy sandbox attempt must be selected")?;

        let fake_resource = spawn_resource_client(resource_status, 0).await?;
        let ownership =
            attempt_ownership_from_parts(run_id.as_uuid(), task_run_id, "legacy-missing-test");
        let identity = SandboxAuthoringProcess::attempt_identity(
            "runner",
            &workload_name,
            ownership.clone(),
            "legacy-missing-test",
        );
        let fake_kube = spawn_kubernetes(identity.clone(), ownership, false).await?;
        let token_file = tempfile::NamedTempFile::new()?;
        std::fs::write(token_file.path(), b"test-kubernetes-token")?;
        let api = KubernetesApiClient::for_test(
            KubernetesApiConfiguration {
                kubernetes_api_server: Url::parse(&fake_kube.base_uri)?,
                kubernetes_bearer_token_file: token_file.path().to_owned(),
                kubernetes_ca_file: PathBuf::from("unused-test-ca"),
                runner_namespace: "runner".to_owned(),
                request_timeout_milliseconds: 5_000,
            },
            reqwest::Client::builder().no_proxy().build()?,
            "labweaver-agent-test",
            "agent-test",
            "LW_AGENT_",
            SANDBOX_MANAGED_BY,
            SANDBOX_EVENT_SCOPE,
        );
        let worker = Worker {
            api,
            resources: fake_resource.client.clone(),
            store: store.clone(),
            objects: test_objects().await?,
            configuration: test_configuration(),
        };

        assert!(worker.reconcile(&pending).await?);
        let (state, diagnostic_code, usage_diagnostic_code):
            (String, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT state,diagnostic_code,usage_diagnostic_code FROM agent.authoring_sandbox_attempts
             WHERE run_id=$1 AND track='environment' AND attempt_number=1",
        )
        .bind(run_id.as_uuid())
        .fetch_one(&pool)
        .await?;
        assert_eq!(state, "released");
        assert_eq!(diagnostic_code.as_deref(), Some("LW_TEST_LEGACY_MISSING"));
        assert_eq!(
            usage_diagnostic_code.as_deref(),
            Some("LW_AGENT_SANDBOX_USAGE_TIMING_UNAVAILABLE")
        );
        assert_eq!(fake_resource.state.release_count.load(Ordering::SeqCst), 1);
        assert!(
            fake_resource
                .state
                .usage_posts
                .lock()
                .map_err(|_| "resource state poisoned")?
                .is_empty()
        );
        assert!(!fake_kube.state.job_exists.load(Ordering::SeqCst));
        Ok(())
    }

    // The injected PostgreSQL write fault must leave the exact owned Job and Resource lease intact.
    #[allow(clippy::too_many_lines)]
    #[tokio::test]
    async fn observation_checkpoint_failure_keeps_owned_kubernetes_objects()
    -> Result<(), Box<dyn Error>> {
        let postgres = Postgres::default().with_tag("17.5-alpine").start().await?;
        let database_url = format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            postgres.get_host_port_ipv4(5432).await?
        );
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect(&database_url)
            .await?;
        apply_agent_migrations(&pool).await?;
        let store = PostgresAgentRunStore::new(pool.clone());

        let run_id = AgentRunId::new();
        let task_run_id = TaskRunId::new();
        let workload_name = format!(
            "lw-auth-{}",
            &task_run_id.as_uuid().simple().to_string()[..20]
        );
        let resource_status = resource_status_value(task_run_id, false);
        let typed_status: TaskResourceStatus = serde_json::from_value(resource_status.clone())?;
        let binding = contracts::execution::TaskExecutionBinding::from_admitted_status(
            &typed_status,
            1,
            workload_name.clone(),
            "checkpoint-failure-test",
        )?;
        let intent = SandboxAttemptIntent {
            run_id,
            track: AgentTrackKind::Environment,
            attempt: 1,
            task_run_id: task_run_id.as_uuid(),
            execution_generation: 1,
            namespace: "runner".to_owned(),
            workload_name: workload_name.clone(),
            binding: serde_json::to_value(binding)?,
        };
        insert_agent_run(&pool, run_id).await?;
        store.begin_sandbox_attempt(&intent).await?;
        let objects = vec![ExecutionObjectRef {
            api_version: "batch/v1".to_owned(),
            resource: "jobs".to_owned(),
            name: workload_name.clone(),
            uid: "job-uid-1".to_owned(),
        }];
        store
            .record_sandbox_objects(&intent, &serde_json::to_value(&objects)?)
            .await?;
        store
            .complete_sandbox_attempt(&intent, None, 1, Some("LW_TEST_TERMINAL"))
            .await?;
        sqlx::raw_sql(
            "CREATE FUNCTION agent.reject_sandbox_usage_observation() RETURNS trigger
             LANGUAGE plpgsql AS $$
             BEGIN
                 IF NEW.usage_observation IS NOT NULL THEN
                     RAISE EXCEPTION 'simulated observation persistence outage';
                 END IF;
                 RETURN NEW;
             END;
             $$;
             CREATE TRIGGER reject_sandbox_usage_observation
             BEFORE UPDATE OF usage_observation ON agent.authoring_sandbox_attempts
             FOR EACH ROW EXECUTE FUNCTION agent.reject_sandbox_usage_observation();",
        )
        .execute(&pool)
        .await?;
        sqlx::query(
            "UPDATE agent.authoring_sandbox_attempts
             SET updated_at=now()-interval '10 seconds'
             WHERE run_id=$1 AND track='environment' AND attempt_number=1",
        )
        .bind(run_id.as_uuid())
        .execute(&pool)
        .await?;
        let pending = select_pending_batch(&pool)
            .await?
            .into_iter()
            .find(|attempt| attempt.task_run_id == task_run_id.as_uuid())
            .ok_or("terminal sandbox attempt must be selected")?;

        let fake_resource = spawn_resource_client(resource_status, 0).await?;
        let ownership =
            attempt_ownership_from_parts(run_id.as_uuid(), task_run_id, "checkpoint-failure-test");
        let identity = SandboxAuthoringProcess::attempt_identity(
            "runner",
            &workload_name,
            ownership.clone(),
            "checkpoint-failure-test",
        );
        let fake_kube = spawn_kubernetes(identity.clone(), ownership, true).await?;
        let token_file = tempfile::NamedTempFile::new()?;
        std::fs::write(token_file.path(), b"test-kubernetes-token")?;
        let api = KubernetesApiClient::for_test(
            KubernetesApiConfiguration {
                kubernetes_api_server: Url::parse(&fake_kube.base_uri)?,
                kubernetes_bearer_token_file: token_file.path().to_owned(),
                kubernetes_ca_file: PathBuf::from("unused-test-ca"),
                runner_namespace: "runner".to_owned(),
                request_timeout_milliseconds: 5_000,
            },
            reqwest::Client::builder().no_proxy().build()?,
            "labweaver-agent-test",
            "agent-test",
            "LW_AGENT_",
            SANDBOX_MANAGED_BY,
            SANDBOX_EVENT_SCOPE,
        );
        let worker = Worker {
            api,
            resources: fake_resource.client.clone(),
            store: store.clone(),
            objects: test_objects().await?,
            configuration: test_configuration(),
        };

        assert!(worker.reconcile(&pending).await.is_err());
        let (state, usage_observation): (String, Option<Value>) = sqlx::query_as(
            "SELECT state,usage_observation FROM agent.authoring_sandbox_attempts
             WHERE run_id=$1 AND track='environment' AND attempt_number=1",
        )
        .bind(run_id.as_uuid())
        .fetch_one(&pool)
        .await?;
        assert_eq!(state, "failed");
        assert!(usage_observation.is_none());
        assert_eq!(fake_resource.state.release_count.load(Ordering::SeqCst), 0);
        assert!(
            fake_resource
                .state
                .usage_posts
                .lock()
                .map_err(|_| "resource state poisoned")?
                .is_empty()
        );
        assert!(fake_kube.state.job_exists.load(Ordering::SeqCst));
        Ok(())
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "keep NULL-binding recovery, exact owned deletion and Resource release assertions in one external-boundary scenario"
    )]
    async fn creating_recovery_settles_frozen_task_and_deletes_owned_objects_before_release()
    -> Result<(), Box<dyn Error>> {
        let postgres = Postgres::default().with_tag("17.5-alpine").start().await?;
        let url = format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            postgres.get_host_port_ipv4(5432).await?
        );
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await?;
        apply_agent_migrations(&pool).await?;
        let store = PostgresAgentRunStore::new(pool.clone());
        let run = AgentRunId::new();
        let task = TaskRunId::new();
        let workload = super::super::workload_name(task.as_uuid());
        let status = resource_status_value(task, false);
        let typed: TaskResourceStatus = serde_json::from_value(status.clone())?;
        let frozen = super::super::SandboxResourceRequest {
            project_id: typed.project_id,
            course_id: typed.request.course_id,
            actor_id: typed.owner_id,
            request_key: typed.request.request_key.clone(),
            trace_id: "creating-recovery-test".to_owned(),
            resources: typed.request.requested_resources.clone(),
            duration_seconds: typed.request.requested_duration_seconds,
        };
        insert_agent_run(&pool, run).await?;
        sqlx::query("UPDATE agent.agent_runs SET project_id=$2 WHERE run_id=$1")
            .bind(run.as_uuid())
            .bind(typed.project_id.as_uuid())
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO agent.authoring_sandbox_attempts (run_id,track,attempt_number,execution_generation,task_run_id,namespace,workload_name,binding,state,request_payload,updated_at) VALUES ($1,'environment',1,2,$2,'runner',$3,NULL,'creating',$4,clock_timestamp()-interval '10 seconds')")
            .bind(run.as_uuid()).bind(task.as_uuid()).bind(&workload).bind(serde_json::to_value(&frozen)?).execute(&pool).await?;
        let pending = select_pending_batch(&pool)
            .await?
            .into_iter()
            .find(|p| p.task_run_id == task.as_uuid())
            .ok_or("creating must be selected")?;
        assert!(pending.binding.is_none());
        let resource = spawn_resource_client(status, 0).await?;
        let owner = attempt_ownership_from_parts(run.as_uuid(), task, &frozen.trace_id);
        let identity = SandboxAuthoringProcess::attempt_identity(
            "runner",
            &workload,
            owner.clone(),
            &frozen.trace_id,
        );
        let kube = spawn_kubernetes(identity, owner, true).await?;
        let token = tempfile::NamedTempFile::new()?;
        std::fs::write(token.path(), b"test-kubernetes-token")?;
        let api = KubernetesApiClient::for_test(
            KubernetesApiConfiguration {
                kubernetes_api_server: Url::parse(&kube.base_uri)?,
                kubernetes_bearer_token_file: token.path().to_owned(),
                kubernetes_ca_file: PathBuf::from("unused-test-ca"),
                runner_namespace: "runner".to_owned(),
                request_timeout_milliseconds: 5000,
            },
            reqwest::Client::builder().no_proxy().build()?,
            "labweaver-agent-test",
            "agent-test",
            "LW_AGENT_",
            SANDBOX_MANAGED_BY,
            SANDBOX_EVENT_SCOPE,
        );
        let worker = Worker {
            api,
            resources: resource.client.clone(),
            store: store.clone(),
            objects: test_objects().await?,
            configuration: test_configuration(),
        };
        assert!(worker.reconcile(&pending).await?);
        let saved = store
            .load_sandbox_attempt(run, AgentTrackKind::Environment, 1, 2)
            .await?
            .ok_or("checkpoint missing")?;
        assert_eq!(saved.state, "released");
        assert!(saved.binding.is_none());
        assert_eq!(saved.request_payload, Some(serde_json::to_value(&frozen)?));
        assert!(!kube.state.job_exists.load(Ordering::SeqCst));
        assert_eq!(resource.state.release_count.load(Ordering::SeqCst), 1);
        assert!(
            resource
                .state
                .usage_posts
                .lock()
                .map_err(|_| "lock poisoned")?
                .is_empty()
        );
        Ok(())
    }

    async fn insert_agent_run(pool: &sqlx::PgPool, run_id: AgentRunId) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO agent.agent_runs
             (run_id,project_id,course_id,problem_package_id,revision,state,provider_binding,
              input_sha256,policy_revision,purpose,contract)
             VALUES ($1,$2,$3,$4,1,'requested','claude-code-v1',repeat('a',64),1,
                     '{\"kind\":\"authoring\",\"environmentClass\":\"experiment\"}',
                     '{\"state\":\"requested\"}')",
        )
        .bind(run_id.as_uuid())
        .bind(ProjectId::new().as_uuid())
        .bind(CourseId::new().as_uuid())
        .bind(contracts::ProblemPackageId::new().as_uuid())
        .execute(pool)
        .await?;
        Ok(())
    }

    fn resource_status_value(task_run_id: TaskRunId, cleanup_confirmed: bool) -> Value {
        let project_id = ProjectId::new();
        let course_id = CourseId::new();
        let owner_id = ActorId::new();
        let request_id = ResourceRequestId::new();
        let claim_id = CapacityClaimId::new();
        let approval_id = ResourceApprovalId::new();
        let lease_id = LeaseId::new();
        json!({
            "taskRunId": task_run_id,
            "projectId": project_id,
            "ownerId": owner_id,
            "executionNamespace": "runner",
            "claimRevision": 1,
            "leaseRevision": 1,
            "cleanupConfirmed": cleanup_confirmed,
            "request": {
                "id": request_id,
                "generation": 1,
                "requestKey": "authoring-cleanup-test-0001",
                "requesterId": owner_id,
                "projectId": project_id,
                "courseId": course_id,
                "target": {"kind":"task", "taskRunId":task_run_id},
                "requestedResources": {"cpuMillicores":100, "memoryBytes":1_048_576, "storageBytes":1_048_576},
                "requestedDurationSeconds": 60,
                "state": "active",
                "revision": 1,
                "createdAt": "2026-09-29T00:00:00.000Z",
                "updatedAt": "2026-09-29T00:00:00.000Z"
            },
            "claim": {
                "id": claim_id,
                "requestId": request_id,
                "approvalId": approval_id,
                "providerBinding": "test-provider-v1",
                "workloadResources": {"cpuMillicores":100, "memoryBytes":1_048_576, "storageBytes":1_048_576},
                "quotaResources": {"cpuMillicores":100, "memoryBytes":1_048_576, "storageBytes":1_048_576},
                "state": "handed_off",
                "revision": 1
            },
            "lease": {
                "id": lease_id,
                "requestId": request_id,
                "claimId": claim_id,
                "state": "active",
                "revision": 1,
                "activeFrom": "2026-09-29T00:00:00.000Z",
                "expiresAt": "2026-09-29T00:01:00.000Z",
                "createdAt": "2026-09-29T00:00:00.000Z",
                "updatedAt": "2026-09-29T00:00:00.000Z"
            }
        })
    }

    #[derive(Clone)]
    struct FakeResourceState {
        status: Arc<Mutex<Value>>,
        usage_posts: Arc<Mutex<Vec<Value>>>,
        failures_remaining: Arc<AtomicUsize>,
        release_count: Arc<AtomicUsize>,
    }

    struct FakeResource {
        client: ResourceClient,
        state: FakeResourceState,
        _authority: TestServer,
        _resource_server: TestServer,
    }

    #[derive(Clone)]
    struct FakeAuthority {
        issuer: String,
        access_token: String,
    }

    #[derive(Deserialize)]
    struct TokenRequest {
        grant_type: String,
    }

    #[allow(
        clippy::too_many_lines,
        reason = "keep the test-only token authority, Resource HTTP routes and client binding setup together"
    )]
    async fn spawn_resource_client(
        status: Value,
        fail_usage_count: usize,
    ) -> Result<FakeResource, Box<dyn Error>> {
        let token_payload = URL_SAFE_NO_PAD.encode(r#"{"aud":"labweaver-resource"}"#);
        let authority_listener = TcpListener::bind("127.0.0.1:0").await?;
        let authority_address = authority_listener.local_addr()?;
        let authority = FakeAuthority {
            issuer: format!("http://localhost:{}/realms/test", authority_address.port()),
            access_token: format!("eyJhbGciOiJub25lIn0.{token_payload}.test"),
        };
        let authority_router = Router::new()
            .route(
                "/realms/test/.well-known/openid-configuration",
                get(token_discovery),
            )
            .route("/realms/test/jwks", get(token_jwks))
            .route("/realms/test/token", post(token_exchange))
            .with_state(authority.clone());
        let authority_task = tokio::spawn(async move {
            let _ = axum::serve(authority_listener, authority_router).await;
        });
        let authority_server = TestServer {
            task: authority_task,
        };
        let scopes = required_scopes_for_diagnostics()
            .iter()
            .map(|scope| (*scope).to_owned())
            .collect::<BTreeSet<_>>();
        let token_http = no_redirect_http_client(None, TransportSecurityMode::InsecureTestOnly)?;
        let discovery_url = format!("{}/.well-known/openid-configuration", authority.issuer);
        let discovery_response = token_http.get(&discovery_url).send().await?;
        if !discovery_response.status().is_success() {
            return Err(std::io::Error::other(format!(
                "fake token authority discovery returned {}",
                discovery_response.status()
            ))
            .into());
        }
        let discovery_body = discovery_response.json::<Value>().await?;
        if discovery_body.get("issuer") != Some(&Value::String(authority.issuer.clone())) {
            return Err(std::io::Error::other(
                "fake token authority discovery issuer does not match configured issuer",
            )
            .into());
        }
        let token_client = ServiceTokenClient::discover(
            ServiceTokenClientConfig::new(
                &authority.issuer,
                "agent-service".to_owned(),
                "test-secret".to_owned(),
                "labweaver-resource".to_owned(),
                scopes.clone(),
                30,
                TransportSecurityMode::InsecureTestOnly,
            )?,
            token_http,
        )
        .await?;
        let state = FakeResourceState {
            status: Arc::new(Mutex::new(status)),
            usage_posts: Arc::new(Mutex::new(Vec::new())),
            failures_remaining: Arc::new(AtomicUsize::new(fail_usage_count)),
            release_count: Arc::new(AtomicUsize::new(0)),
        };
        let resource_router = Router::new()
            .route(
                "/internal/v1/task-resources/{task_run_id}/claim",
                post(resource_status),
            )
            .route("/internal/v1/task-resources", post(resource_create))
            .route(
                "/internal/v1/task-resources/{task_run_id}/request",
                get(resource_request),
            )
            .route(
                "/internal/v1/task-resources/{task_run_id}",
                get(resource_status),
            )
            .route(
                "/internal/v1/task-resources/{task_run_id}/release",
                post(resource_release),
            )
            .route("/internal/v1/resource/usage", post(resource_usage))
            .with_state(state.clone());
        let (base_uri, resource_server) = spawn_tls_server(resource_router).await?;
        let config = ResourceClientConfiguration {
            base_uri: Url::parse(&base_uri)?,
            ca_file: std::env::temp_dir().join("unused-sandbox-cleanup-ca.pem"),
            timeout_milliseconds: 5_000,
            max_request_bytes: 1024 * 1024,
            max_response_bytes: 1024 * 1024,
            audience: "labweaver-resource".to_owned(),
        };
        let client = reqwest::Client::builder()
            .no_proxy()
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .danger_accept_invalid_certs(true)
            .timeout(Duration::from_secs(5))
            .build()?;
        let client = ResourceClient::new(config, client, Arc::new(token_client), scopes)?;
        Ok(FakeResource {
            client,
            state,
            _authority: authority_server,
            _resource_server: resource_server,
        })
    }

    async fn resource_create(
        State(state): State<FakeResourceState>,
        Json(_request): Json<Value>,
    ) -> Json<Value> {
        Json(
            state
                .status
                .lock()
                .map_or(Value::Null, |status| status["request"].clone()),
        )
    }
    async fn resource_request(State(state): State<FakeResourceState>) -> Json<Value> {
        Json(
            state
                .status
                .lock()
                .map_or(Value::Null, |status| status["request"].clone()),
        )
    }

    async fn resource_status(
        State(state): State<FakeResourceState>,
        AxumPath(_task_run_id): AxumPath<String>,
    ) -> Json<Value> {
        Json(
            state
                .status
                .lock()
                .map_or(Value::Null, |status| status.clone()),
        )
    }

    async fn resource_release(
        State(state): State<FakeResourceState>,
        AxumPath(_task_run_id): AxumPath<String>,
        Json(_request): Json<Value>,
    ) -> Response {
        state.release_count.fetch_add(1, Ordering::SeqCst);
        let Ok(mut status) = state.status.lock() else {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        };
        status["cleanupConfirmed"] = Value::Bool(true);
        Json(status.clone()).into_response()
    }

    async fn resource_usage(
        State(state): State<FakeResourceState>,
        Json(request): Json<RecordResourceUsageRequest>,
    ) -> Response {
        let Ok(value) = serde_json::to_value(&request) else {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        };
        if let Ok(mut posts) = state.usage_posts.lock() {
            posts.push(value);
        } else {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        if state
            .failures_remaining
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                if remaining > 0 {
                    Some(remaining - 1)
                } else {
                    None
                }
            })
            .is_ok()
        {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        let record = contracts::resource::ResourceUsageRecord {
            id: UsageRecordId::new(),
            project_id: request.project_id,
            course_id: request.course_id,
            kind: request.kind,
            request_id: request.request_id,
            lease_id: request.lease_id,
            source_event_id: request.source_event_id,
            measured_from: request.measured_from,
            measured_until: request.measured_until,
            measurement: request.measurement,
            settlement: contracts::resource::UsageSettlementState::Pending,
            observed_at: request.measured_until,
        };
        Json(record).into_response()
    }

    async fn token_discovery(State(authority): State<FakeAuthority>) -> Json<Value> {
        Json(json!({
            "issuer": authority.issuer,
            "authorization_endpoint": format!("{}/authorize", authority.issuer),
            "token_endpoint": format!("{}/token", authority.issuer),
            "jwks_uri": format!("{}/jwks", authority.issuer),
            "response_types_supported": ["code"],
            "subject_types_supported": ["public"],
            "id_token_signing_alg_values_supported": ["ES256"],
            "grant_types_supported": ["client_credentials"]
        }))
    }

    async fn token_jwks() -> Json<Value> {
        Json(json!({"keys": []}))
    }

    async fn token_exchange(
        State(authority): State<FakeAuthority>,
        Form(request): Form<TokenRequest>,
    ) -> Response {
        if request.grant_type != "client_credentials" {
            return StatusCode::BAD_REQUEST.into_response();
        }
        Json(json!({
            "access_token": authority.access_token,
            "token_type": "Bearer",
            "expires_in": 300
        }))
        .into_response()
    }

    struct FakeKubernetesState {
        job_exists: AtomicBool,
        job: Value,
        pods: Value,
    }

    struct FakeKubernetes {
        base_uri: String,
        state: Arc<FakeKubernetesState>,
        _server: TestServer,
    }

    async fn spawn_kubernetes(
        identity: KubernetesJobIdentity,
        ownership: KubernetesOwnership,
        job_exists: bool,
    ) -> Result<FakeKubernetes, Box<dyn Error>> {
        let metadata = json!({
            "name": identity.job_name,
            "namespace": identity.namespace,
            "uid": "job-uid-1",
            "resourceVersion": "1",
            "labels": {
                "labweaver.io/managed-by": SANDBOX_MANAGED_BY,
                "labweaver.io/run-id": ownership.run_id.to_string(),
                "labweaver.io/step-run-id": ownership.step_run_id.to_string(),
                "labweaver.io/attempt-id": ownership.attempt_id.to_string(),
            },
            "annotations": {"labweaver.io/request-sha256": ownership.request_sha256},
        });
        let job = json!({
            "apiVersion": "batch/v1",
            "kind": "Job",
            "metadata": metadata,
            "spec": {},
            "status": {"succeeded": 1},
        });
        let pod = json!({
            "apiVersion": "v1",
            "kind": "Pod",
            "metadata": {
                "name": "sandbox-pod",
                "namespace": identity.namespace,
                "uid": "pod-uid-1",
                "labels": {
                    "labweaver.io/managed-by": SANDBOX_MANAGED_BY,
                    "labweaver.io/run-id": ownership.run_id.to_string(),
                    "labweaver.io/step-run-id": ownership.step_run_id.to_string(),
                    "labweaver.io/attempt-id": ownership.attempt_id.to_string(),
                },
                "annotations": {"labweaver.io/request-sha256": ownership.request_sha256},
            },
            "status": {"containerStatuses": [{
                "name": SANDBOX_MAIN_CONTAINER,
                "state": {"terminated": {
                    "exitCode": 0,
                    "reason": "Completed",
                    "message": "{}",
                    "startedAt": "2026-09-29T00:00:01Z",
                    "finishedAt": "2026-09-29T00:00:02Z"
                }}
            }]}
        });
        let state = Arc::new(FakeKubernetesState {
            job_exists: AtomicBool::new(job_exists),
            job,
            pods: if job_exists {
                json!({"items": [pod]})
            } else {
                json!({"items": []})
            },
        });
        let app = Router::new()
            .route(
                &format!(
                    "/apis/batch/v1/namespaces/runner/jobs/{}",
                    identity.job_name
                ),
                get(kubernetes_get_job).delete(kubernetes_delete_job),
            )
            .route("/api/v1/namespaces/runner/pods", get(kubernetes_list_pods))
            .with_state(state.clone());
        let (base_uri, server) = spawn_http_server(app).await?;
        Ok(FakeKubernetes {
            base_uri,
            state,
            _server: server,
        })
    }

    async fn kubernetes_get_job(State(state): State<Arc<FakeKubernetesState>>) -> Response {
        if state.job_exists.load(Ordering::SeqCst) {
            Json(state.job.clone()).into_response()
        } else {
            StatusCode::NOT_FOUND.into_response()
        }
    }

    async fn kubernetes_delete_job(State(state): State<Arc<FakeKubernetesState>>) -> StatusCode {
        state.job_exists.store(false, Ordering::SeqCst);
        StatusCode::OK
    }

    async fn kubernetes_list_pods(State(state): State<Arc<FakeKubernetesState>>) -> Json<Value> {
        Json(state.pods.clone())
    }

    struct TestServer {
        task: JoinHandle<()>,
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn spawn_http_server(router: Router) -> Result<(String, TestServer), Box<dyn Error>> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Ok((format!("http://{address}/"), TestServer { task }))
    }

    async fn spawn_tls_server(router: Router) -> Result<(String, TestServer), Box<dyn Error>> {
        let certified = generate_simple_self_signed(vec!["localhost".to_owned()])?;
        let certificates = rustls_pemfile::certs(&mut Cursor::new(certified.cert.pem().as_bytes()))
            .collect::<Result<Vec<_>, _>>()?;
        let key: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut Cursor::new(
            certified.signing_key.serialize_pem().as_bytes(),
        ))?
        .ok_or("fake Resource TLS key missing")?;
        let mut server_config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certificates, key)?;
        server_config.alpn_protocols = vec![b"http/1.1".to_vec()];
        let acceptor = TlsAcceptor::from(Arc::new(server_config));
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let acceptor = acceptor.clone();
                let router = router.clone();
                tokio::spawn(async move {
                    let Ok(stream) = acceptor.accept(stream).await else {
                        return;
                    };
                    let service = TowerToHyperService::new(router);
                    let connection = Builder::new(TokioExecutor::new())
                        .serve_connection_with_upgrades(TokioIo::new(stream), service)
                        .into_owned();
                    let _ = connection.await;
                });
            }
        });
        Ok((
            format!("https://localhost:{}/", address.port()),
            TestServer { task },
        ))
    }
}
