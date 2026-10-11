//! Periodic reconciliation of orphaned Evaluation Kubernetes workloads.
//!
//! The reconciler never guesses ownership. A live workload is removed automatically only when the
//! durable attempt record proves it belongs to a terminal attempt whose cleanup was not verified
//! and the live object identity matches the persisted checkpoint. Every other case keeps a stable
//! diagnostic and leaves the object untouched.
#![allow(
    missing_docs,
    clippy::missing_errors_doc,
    clippy::too_many_lines,
    reason = "the orphan classification matrix is intentionally colocated for review"
)]

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use contracts::execution::ExecutionCleanupStatus;
use contracts::{EvaluationRunId, EvaluationStepRunId, TaskRunId};
use serde_json::Value;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{
    ansible_probe_executor::AnsibleProbeKubernetesExecutor,
    control_plane::{EvaluationExecutionKind, EvaluationOrphanAttempt, PgEvaluationControlStore},
    kubernetes_job::{KubernetesApiClient, KubernetesApiConfiguration, KubernetesJobError},
    oj_executor::OjKubernetesExecutor,
};

/// Exact label selector that identifies Evaluation-managed one-shot workloads.
pub const ORPHAN_JOB_LABEL_SELECTOR: &str = "labweaver.io/managed-by=evaluation-service";

const FIELD_MANAGER: &str = "labweaver-orphan-reconciler";
const MANAGED_BY: &str = "evaluation-service";
const EVENT_SCOPE: &str = "evaluation";
const LOG_SCOPE: &str = "evaluation.orphan";
const DIAGNOSTIC_PREFIX: &str = "LW_EVALUATION_";

/// Failure returned by one orphan reconciliation pass.
#[derive(Debug, Error)]
pub enum OrphanReconcileError {
    #[error("orphan reconcile configuration is invalid")]
    ConfigurationInvalid,
    #[error("orphan reconcile Kubernetes API failed")]
    Kubernetes(#[from] KubernetesJobError),
    #[error("orphan reconcile control-plane lookup failed")]
    Control,
}

/// Durable attempt lookup used by the reconciler.
#[async_trait]
pub trait OrphanAttemptLookup: Send + Sync + 'static {
    async fn lookup_attempt(
        &self,
        step_run_id: EvaluationStepRunId,
        task_run_id: TaskRunId,
    ) -> Result<Option<EvaluationOrphanAttempt>, OrphanReconcileError>;
}

#[async_trait]
impl OrphanAttemptLookup for PgEvaluationControlStore {
    async fn lookup_attempt(
        &self,
        step_run_id: EvaluationStepRunId,
        task_run_id: TaskRunId,
    ) -> Result<Option<EvaluationOrphanAttempt>, OrphanReconcileError> {
        self.load_orphan_attempt(step_run_id, task_run_id)
            .await
            .map_err(|_| OrphanReconcileError::Control)
    }
}

/// Classification of one live Evaluation-managed Job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrphanDecision {
    /// No durable terminal attempt proves ownership; the object is reported only.
    ReportUnverified,
    /// A running attempt still owns the object; the scheduler recovers it.
    ReportRunning,
    /// The durable record says cleanup was verified, so the live object is inconsistent.
    ReportInconsistent,
    /// A terminal attempt with unverified cleanup owns the object and it may be removed.
    Cleanup,
}

/// Classifies one live Job against its durable attempt projection.
#[must_use]
pub fn classify_orphan(
    job_name: &str,
    job_uid: Option<&str>,
    attempt: Option<&EvaluationOrphanAttempt>,
) -> OrphanDecision {
    let Some(attempt) = attempt else {
        return OrphanDecision::ReportUnverified;
    };
    if !attempt.state.is_terminal() {
        return OrphanDecision::ReportRunning;
    }
    let Some(resources) = attempt.execution_resources.as_ref() else {
        return OrphanDecision::ReportUnverified;
    };
    if resources.kind == EvaluationExecutionKind::LlmReview {
        return OrphanDecision::ReportUnverified;
    }
    let expected_name = match resources.kind {
        EvaluationExecutionKind::Program => format!(
            "lw-oj-{}",
            &resources.task_run_id.as_uuid().simple().to_string()[..20]
        ),
        EvaluationExecutionKind::AnsibleProbe => format!(
            "lw-ap-{}",
            &resources.task_run_id.as_uuid().simple().to_string()[..20]
        ),
        EvaluationExecutionKind::LlmReview => return OrphanDecision::ReportUnverified,
    };
    if job_name != expected_name {
        return OrphanDecision::ReportUnverified;
    }
    if let Some(job) = resources
        .objects
        .iter()
        .find(|object| object.resource == "jobs")
        && job_uid != Some(job.uid.as_str())
    {
        return OrphanDecision::ReportUnverified;
    }
    if attempt.cleanup_verified {
        return OrphanDecision::ReportInconsistent;
    }
    OrphanDecision::Cleanup
}

/// One reconciliation pass result used for tests and diagnostics.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct OrphanReport {
    pub scanned: usize,
    pub cleaned: usize,
    pub running: usize,
    pub unverified: usize,
    pub inconsistent: usize,
    pub pending: usize,
}

/// Periodic orphan workload reconciler for one runner namespace.
pub struct OrphanReconciler {
    api: KubernetesApiClient,
    oj: OjKubernetesExecutor,
    ansible_probe: AnsibleProbeKubernetesExecutor,
    lookup: Arc<dyn OrphanAttemptLookup>,
    namespace: String,
    poll_interval: Duration,
    detected: AtomicU64,
    cleaned: AtomicU64,
}

impl OrphanReconciler {
    /// Builds the reconciler from the validated execution configuration.
    ///
    /// # Errors
    ///
    /// Returns a stable configuration error when the Kubernetes bindings are invalid.
    pub fn new(
        configuration: &crate::kubernetes_runner::EvaluationExecutionConfiguration,
        poll_interval_seconds: u64,
        lookup: Arc<dyn OrphanAttemptLookup>,
    ) -> Result<Self, OrphanReconcileError> {
        if !(1..=3_600).contains(&poll_interval_seconds) {
            return Err(OrphanReconcileError::ConfigurationInvalid);
        }
        let api = KubernetesApiClient::new(
            KubernetesApiConfiguration {
                kubernetes_api_server: configuration.oj.kubernetes_api_server.clone(),
                kubernetes_bearer_token_file: configuration.oj.kubernetes_bearer_token_file.clone(),
                kubernetes_ca_file: configuration.oj.kubernetes_ca_file.clone(),
                runner_namespace: configuration.runner_namespace.clone(),
                request_timeout_milliseconds: configuration.oj.request_timeout_milliseconds,
            },
            FIELD_MANAGER,
            LOG_SCOPE,
            DIAGNOSTIC_PREFIX,
            MANAGED_BY,
            EVENT_SCOPE,
        )
        .map_err(|_| OrphanReconcileError::ConfigurationInvalid)?;
        let oj = OjKubernetesExecutor::new(configuration.oj.clone())
            .map_err(|_| OrphanReconcileError::ConfigurationInvalid)?;
        let ansible_probe =
            AnsibleProbeKubernetesExecutor::new(configuration.ansible_probe_executor.clone())
                .map_err(|_| OrphanReconcileError::ConfigurationInvalid)?;
        Ok(Self {
            api,
            oj,
            ansible_probe,
            lookup,
            namespace: configuration.runner_namespace.clone(),
            poll_interval: Duration::from_secs(poll_interval_seconds),
            detected: AtomicU64::new(0),
            cleaned: AtomicU64::new(0),
        })
    }

    /// Returns how many managed Jobs were examined.
    #[must_use]
    pub fn detected(&self) -> u64 {
        self.detected.load(Ordering::Relaxed)
    }

    /// Returns how many orphan bundles reached confirmed cleanup.
    #[must_use]
    pub fn cleaned(&self) -> u64 {
        self.cleaned.load(Ordering::Relaxed)
    }

    /// Runs one reconciliation pass over the managed Jobs.
    ///
    /// # Errors
    ///
    /// Returns a stable error when the Kubernetes list or durable lookup fails. Individual
    /// objects that cannot be proven or removed keep their diagnostic and do not fail the pass.
    pub async fn reconcile_once(&self) -> Result<OrphanReport, OrphanReconcileError> {
        let jobs = self
            .api
            .list(
                self.namespace.as_str(),
                "batch/v1",
                "jobs",
                ORPHAN_JOB_LABEL_SELECTOR,
            )
            .await?;
        let mut report = OrphanReport::default();
        for job in jobs {
            report.scanned = report.scanned.saturating_add(1);
            self.detected.fetch_add(1, Ordering::Relaxed);
            let Some(job_name) = job.pointer("/metadata/name").and_then(Value::as_str) else {
                report.unverified = report.unverified.saturating_add(1);
                continue;
            };
            let job_uid = job.pointer("/metadata/uid").and_then(Value::as_str);
            let labels = job.pointer("/metadata/labels").and_then(Value::as_object);
            let identity = labels.and_then(|labels| {
                let step_run_id = labels
                    .get("labweaver.io/step-run-id")
                    .and_then(Value::as_str)?
                    .parse::<EvaluationStepRunId>()
                    .ok()?;
                let task_run_id = labels
                    .get("labweaver.io/attempt-id")
                    .and_then(Value::as_str)?
                    .parse::<TaskRunId>()
                    .ok()?;
                let run_id = labels
                    .get("labweaver.io/run-id")
                    .and_then(Value::as_str)?
                    .parse::<EvaluationRunId>()
                    .ok()?;
                Some((step_run_id, task_run_id, run_id))
            });
            let Some((step_run_id, task_run_id, run_id)) = identity else {
                tracing::warn!(
                    event = "evaluation.orphan.unverified_identity",
                    job_name = job_name,
                    diagnostic_code = "LW_EVALUATION_ORPHAN_IDENTITY_UNVERIFIED",
                    "orphan Job labels cannot be parsed; leaving the object untouched",
                );
                report.unverified = report.unverified.saturating_add(1);
                continue;
            };
            let attempt = self.lookup.lookup_attempt(step_run_id, task_run_id).await?;
            if let Some(resources) = attempt
                .as_ref()
                .and_then(|attempt| attempt.execution_resources.as_ref())
                && (resources.run_id != run_id
                    || resources.step_run_id != step_run_id
                    || resources.task_run_id != task_run_id)
            {
                tracing::warn!(
                    event = "evaluation.orphan.identity_conflict",
                    job_name = job_name,
                    diagnostic_code = "LW_EVALUATION_ORPHAN_IDENTITY_CONFLICT",
                    "orphan Job labels disagree with the durable attempt; leaving it untouched",
                );
                report.unverified = report.unverified.saturating_add(1);
                continue;
            }
            match classify_orphan(job_name, job_uid, attempt.as_ref()) {
                OrphanDecision::ReportRunning => {
                    report.running = report.running.saturating_add(1);
                }
                OrphanDecision::ReportUnverified => {
                    tracing::warn!(
                        event = "evaluation.orphan.unverified",
                        job_name = job_name,
                        diagnostic_code = "LW_EVALUATION_ORPHAN_UNVERIFIED",
                        "orphan Job ownership cannot be proven; leaving it untouched",
                    );
                    report.unverified = report.unverified.saturating_add(1);
                }
                OrphanDecision::ReportInconsistent => {
                    tracing::warn!(
                        event = "evaluation.orphan.inconsistent",
                        job_name = job_name,
                        diagnostic_code = "LW_EVALUATION_ORPHAN_INCONSISTENT",
                        "cleanup was already verified but a matching Job still exists",
                    );
                    report.inconsistent = report.inconsistent.saturating_add(1);
                }
                OrphanDecision::Cleanup => {
                    let resources = attempt
                        .and_then(|attempt| attempt.execution_resources)
                        .ok_or(OrphanReconcileError::Control)?;
                    let status = match resources.kind {
                        EvaluationExecutionKind::Program => self
                            .oj
                            .cleanup_recovery(&resources)
                            .await
                            .map_err(|_| OrphanReconcileError::Control)?,
                        EvaluationExecutionKind::AnsibleProbe => self
                            .ansible_probe
                            .cleanup_recovery(&resources)
                            .await
                            .map_err(|_| OrphanReconcileError::Control)?,
                        EvaluationExecutionKind::LlmReview => ExecutionCleanupStatus::Confirmed,
                    };
                    match status {
                        ExecutionCleanupStatus::Confirmed => {
                            self.cleaned.fetch_add(1, Ordering::Relaxed);
                            report.cleaned = report.cleaned.saturating_add(1);
                            tracing::info!(
                                event = "evaluation.orphan.cleaned",
                                job_name = job_name,
                                diagnostic_code = "LW_EVALUATION_ORPHAN_CLEANED",
                                "terminal attempt orphan workload was cleaned and verified",
                            );
                        }
                        ExecutionCleanupStatus::Pending { remaining_objects } => {
                            report.pending = report.pending.saturating_add(1);
                            tracing::warn!(
                                event = "evaluation.orphan.cleanup_pending",
                                job_name = job_name,
                                remaining = remaining_objects.len(),
                                diagnostic_code = "LW_EVALUATION_ORPHAN_CLEANUP_PENDING",
                                "orphan cleanup is still pending; the next pass continues",
                            );
                        }
                        ExecutionCleanupStatus::Unknown { diagnostic } => {
                            report.unverified = report.unverified.saturating_add(1);
                            tracing::warn!(
                                event = "evaluation.orphan.cleanup_unknown",
                                job_name = job_name,
                                diagnostic_code = diagnostic.as_str(),
                                "orphan cleanup is unknown; Resource is not released",
                            );
                        }
                    }
                }
            }
        }
        Ok(report)
    }

    /// Runs the reconciler until the cancellation token fires.
    pub async fn run(&self, cancellation: CancellationToken) {
        let mut interval = tokio::time::interval(self.poll_interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = cancellation.cancelled() => return,
                _ = interval.tick() => {}
            }
            if let Err(error) = self.reconcile_once().await {
                // `error` is a protected field name, so every pass reported the same redacted text
                // and the failing stage was unrecoverable from the log. The variant discriminates
                // configuration from the Kubernetes API from the control-plane lookup, which is
                // what an operator needs to route the failure.
                let error_kind = match error {
                    OrphanReconcileError::ConfigurationInvalid => "configuration_invalid",
                    OrphanReconcileError::Kubernetes(_) => "kubernetes",
                    OrphanReconcileError::Control => "control_lookup",
                };
                tracing::error!(
                    event = "evaluation.orphan.reconcile_failed",
                    diagnostic_code = "LW_EVALUATION_ORPHAN_RECONCILE_FAILED",
                    error_kind = error_kind,
                    error = %error,
                    "orphan reconcile pass failed; the next pass retries",
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use contracts::execution::TaskExecutionBinding;
    use contracts::{CapacityClaimId, LeaseId, ProjectId, ResourceRequestId, Revision, TaskRunId};
    use serde_json::json;

    use super::{OrphanDecision, classify_orphan};
    use crate::control_plane::{
        EvaluationAttemptState, EvaluationExecutionKind, EvaluationExecutionResources,
        EvaluationOrphanAttempt,
    };

    fn revision() -> Revision {
        Revision::new(1).unwrap_or_else(|_| unreachable!("fixture revision must be non-zero"))
    }

    fn resources(
        kind: EvaluationExecutionKind,
        objects: Vec<contracts::execution::ExecutionObjectRef>,
    ) -> EvaluationExecutionResources {
        let run_id = contracts::EvaluationRunId::new();
        let step_run_id = contracts::EvaluationStepRunId::new();
        let task_run_id = TaskRunId::new();
        EvaluationExecutionResources {
            schema_version: crate::EVALUATION_EXECUTION_RESOURCES_SCHEMA_VERSION.to_owned(),
            run_id,
            step_run_id,
            task_run_id,
            namespace: "labweaver-evaluation".to_owned(),
            kind,
            admission: Some(TaskExecutionBinding {
                task_run_id,
                execution_generation: 1,
                resource_request_id: ResourceRequestId::new(),
                capacity_claim_id: CapacityClaimId::new(),
                lease_id: LeaseId::new(),
                claim_revision: revision(),
                lease_revision: revision(),
                project_id: ProjectId::new(),
                provider_binding: "kubernetes-job".to_owned(),
                namespace: "labweaver-evaluation".to_owned(),
                workload_name: "lw-oj-00000000000000000000".to_owned(),
                trace_id: "trace".to_owned(),
            }),
            request: json!({}),
            objects,
        }
    }

    fn attempt(
        state: EvaluationAttemptState,
        cleanup_verified: bool,
        resources: Option<EvaluationExecutionResources>,
    ) -> EvaluationOrphanAttempt {
        EvaluationOrphanAttempt {
            state,
            cleanup_verified,
            execution_resources: resources,
        }
    }

    #[test]
    fn missing_attempt_is_never_cleaned() {
        assert_eq!(
            classify_orphan("lw-oj-attempt", Some("uid"), None),
            OrphanDecision::ReportUnverified
        );
    }

    #[test]
    fn running_attempt_is_left_to_the_scheduler() {
        let attempt = attempt(EvaluationAttemptState::Running, false, None);
        assert_eq!(
            classify_orphan("lw-oj-attempt", Some("uid"), Some(&attempt)),
            OrphanDecision::ReportRunning
        );
    }

    #[test]
    fn terminal_attempt_without_checkpoint_is_unverified() {
        let attempt = attempt(EvaluationAttemptState::Failed, false, None);
        assert_eq!(
            classify_orphan("lw-oj-attempt", Some("uid"), Some(&attempt)),
            OrphanDecision::ReportUnverified
        );
    }

    #[test]
    fn verified_cleanup_with_live_job_is_inconsistent() {
        let resources = resources(EvaluationExecutionKind::Program, Vec::new());
        let name = format!(
            "lw-oj-{}",
            &resources.task_run_id.as_uuid().simple().to_string()[..20]
        );
        let attempt = attempt(EvaluationAttemptState::Failed, true, Some(resources));
        assert_eq!(
            classify_orphan(&name, Some("uid"), Some(&attempt)),
            OrphanDecision::ReportInconsistent
        );
    }

    #[test]
    fn uid_replacement_is_unverified() {
        let mut resources = resources(EvaluationExecutionKind::Program, Vec::new());
        let name = format!(
            "lw-oj-{}",
            &resources.task_run_id.as_uuid().simple().to_string()[..20]
        );
        resources.objects = vec![contracts::execution::ExecutionObjectRef {
            api_version: "batch/v1".to_owned(),
            resource: "jobs".to_owned(),
            name: name.clone(),
            uid: "original-uid".to_owned(),
        }];
        let attempt = attempt(EvaluationAttemptState::Failed, false, Some(resources));
        assert_eq!(
            classify_orphan(&name, Some("replacement-uid"), Some(&attempt)),
            OrphanDecision::ReportUnverified
        );
        assert_eq!(
            classify_orphan(&name, Some("original-uid"), Some(&attempt)),
            OrphanDecision::Cleanup
        );
    }

    #[test]
    fn terminal_attempt_without_uids_requires_the_deterministic_name() {
        let resources = resources(EvaluationExecutionKind::Program, Vec::new());
        let attempt = attempt(
            EvaluationAttemptState::Failed,
            false,
            Some(resources.clone()),
        );
        let expected = format!(
            "lw-oj-{}",
            &resources.task_run_id.as_uuid().simple().to_string()[..20]
        );
        assert_eq!(
            classify_orphan(&expected, Some("uid"), Some(&attempt)),
            OrphanDecision::Cleanup
        );
        assert_eq!(
            classify_orphan("lw-oj-unrelated", Some("uid"), Some(&attempt)),
            OrphanDecision::ReportUnverified
        );
    }

    #[test]
    fn llm_review_checkpoints_are_never_cleaned() {
        let attempt = attempt(
            EvaluationAttemptState::Failed,
            false,
            Some(resources(EvaluationExecutionKind::LlmReview, Vec::new())),
        );
        assert_eq!(
            classify_orphan("lw-oj-attempt", Some("uid"), Some(&attempt)),
            OrphanDecision::ReportUnverified
        );
    }

    #[test]
    fn terminal_ansible_probe_attempt_uses_the_probe_name() {
        let resources = resources(EvaluationExecutionKind::AnsibleProbe, Vec::new());
        let attempt = attempt(
            EvaluationAttemptState::Cancelled,
            false,
            Some(resources.clone()),
        );
        let expected = format!(
            "lw-ap-{}",
            &resources.task_run_id.as_uuid().simple().to_string()[..20]
        );
        assert_eq!(
            classify_orphan(&expected, Some("uid"), Some(&attempt)),
            OrphanDecision::Cleanup
        );
    }
}
