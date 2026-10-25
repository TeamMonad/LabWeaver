//! Legacy active cleanup rows must never acquire a replacement UID after an upgrade.
use super::*;
use crate::{
    KubeVirtBackendFence, KubeVirtCleanupPlan, KubeVirtExecutorRequest as VmRequest,
    KubeVirtExecutorResponse as VmResponse, KubeVirtResourcePlan, KubeVirtRunningObservation,
    KubeVirtStoppedObservation,
};
use contracts::authoring::RuntimeKind;

// This is only an in-process transport: the native VM admission/permit/executor and the
// production response decoders remain active, and HTTPS Kubernetes is the external fixture.
struct DirectVmKubernetes(Arc<crate::FencedKubeVirtExecutor<KubernetesContainerExecutor>>);
impl DirectVmKubernetes {
    async fn invoke(
        &self,
        fence: &KubeVirtBackendFence,
        request: VmRequest,
    ) -> Result<VmResponse, ProviderFailure> {
        let fence =
            crate::kubevirt_provider::bind_kubevirt_executor_request(fence.clone(), &request)?;
        self.0
            .execute(crate::KubeVirtExecutorRequestEnvelope { fence, request })
            .await
            .map(|envelope| envelope.response)
            .map_err(|error| crate::kubevirt_provider::kubevirt_executor_failure(&error))
    }
}
#[async_trait]
impl crate::KubeVirtProviderBackend for DirectVmKubernetes {
    async fn apply(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtResourcePlan,
    ) -> Result<ProviderOutcome<KubeVirtRunningObservation>, ProviderFailure> {
        crate::kubevirt_provider::running_response(
            self.invoke(fence, VmRequest::Apply { plan: plan.clone() })
                .await?,
            plan,
        )
    }
    async fn observe(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtResourcePlan,
    ) -> Result<ProviderOutcome<KubeVirtRunningObservation>, ProviderFailure> {
        crate::kubevirt_provider::running_response(
            self.invoke(fence, VmRequest::Observe { plan: plan.clone() })
                .await?,
            plan,
        )
    }
    async fn start(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtResourcePlan,
    ) -> Result<ProviderOutcome<KubeVirtRunningObservation>, ProviderFailure> {
        crate::kubevirt_provider::running_response(
            self.invoke(fence, VmRequest::Start { plan: plan.clone() })
                .await?,
            plan,
        )
    }
    async fn stop(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtCleanupPlan,
    ) -> Result<ProviderOutcome<KubeVirtStoppedObservation>, ProviderFailure> {
        crate::kubevirt_provider::stopped_response(
            self.invoke(fence, VmRequest::Stop { plan: plan.clone() })
                .await?,
            plan,
        )
    }
    async fn restart(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtResourcePlan,
    ) -> Result<ProviderOutcome<KubeVirtRunningObservation>, ProviderFailure> {
        crate::kubevirt_provider::running_response(
            self.invoke(fence, VmRequest::Restart { plan: plan.clone() })
                .await?,
            plan,
        )
    }
    async fn delete_namespace(
        &self,
        fence: &KubeVirtBackendFence,
        plan: &KubeVirtCleanupPlan,
        target: Option<&EnvironmentCleanupTarget>,
    ) -> Result<ProviderOutcome<ArtifactRef>, ProviderFailure> {
        crate::kubevirt_provider::cleanup_response(
            self.invoke(
                fence,
                VmRequest::DeleteNamespace {
                    plan: plan.clone(),
                    target: target.cloned(),
                },
            )
            .await?,
            plan,
        )
    }
}

impl Context {
    fn vm_worker(&self) -> TestResult<crate::ReconcileWorker> {
        let executor = crate::FencedKubeVirtExecutor::new(
            crate::PgKubeVirtExecutorFenceStore::new(self.pool.clone()),
            self.executor.clone(),
            crate::KubeVirtExecutionInstance {
                namespace: "labweaver-system".to_owned(),
                pod_name: "executor-fixture".to_owned(),
                pod_uid: uuid::Uuid::now_v7(),
                container_name: "kubevirt-executor".to_owned(),
                boot_token: uuid::Uuid::now_v7(),
            },
        );
        let base = crate::KubeVirtBaseDiskBinding::new(
            "vm-base-v1".to_owned(),
            format!("docker://registry.example/base@sha256:{}", "a".repeat(64)),
            "b".repeat(64),
            1_073_741_824,
            contracts::supply_chain::VirtualMachineDiskFormat::Qcow2,
            "local-path".to_owned(),
            "local-path".to_owned(),
            "labweaver-system".to_owned(),
            "base".to_owned(),
            "labweaver".to_owned(),
            22,
        )?;
        let ssh = crate::KubeVirtSshBootstrap::new(
            "labweaver-system".to_owned(),
            "openssh-gateway".to_owned(),
            "labweaver-evaluation".to_owned(),
            "evaluation-freeze-worker".to_owned(),
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDIhz2GK/XCUj4i6Q5yQJNL1MKDXETe1aM1lHYMGt2SQ",
        )?;
        let configuration = crate::KubeVirtProviderConfiguration::new(
            Revision::new(1)?,
            vec![base],
            None,
            ssh,
            crate::KubeVirtResourceBudget::new(1, 1, 1, 1, 1, 1)?,
        )?;
        let provider = crate::KubeVirtProvider::new(
            self.initial.provider_binding.clone(),
            Arc::new(DirectVmKubernetes(Arc::new(executor))),
            Arc::new(crate::PgReleaseProjectionStore::new(self.pool.clone())),
            Arc::new(crate::PgKubeVirtObservationStore::new(self.pool.clone())),
            configuration,
        )?;
        let mut registry = crate::ProviderRegistry::default();
        registry.register(Arc::new(provider))?;
        Ok(crate::ReconcileWorker::new(
            self.store.clone(),
            crate::Reconciler::new(registry, Duration::from_secs(5))?,
            Duration::from_secs(10),
            Duration::from_millis(10),
        )?)
    }

    pub(super) async fn seed_legacy_active(&self, state: OperationState) -> TestResult {
        let mut current = self.store.load(self.initial.id).await?;
        current.observed_state = contracts::environment::ObservedEnvironmentState::Deleting;
        current.failed_phase = None;
        current.last_diagnostic_code = None;
        current.operation.state = state;
        current.operation.diagnostic_code = None;
        current.operation.cleanup_target = None;
        current.operation.cleanup_protocol_version = None;
        current.operation.attempt = if state == OperationState::Accepted {
            2
        } else {
            1
        };
        current.operation.max_attempts = 3;
        current.operation.cleanup_started_at =
            (state == OperationState::Cancelling).then_some(current.operation.accepted_at);
        current.validate()?;
        let aggregate = serde_json::to_value(&current)?;
        let operation = serde_json::to_value(&current.operation)?;
        assert!(operation.get("cleanupProtocolVersion").is_none());
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE environment.environment_instances SET contract=$2,observed_state='deleting',failed_phase=NULL,terminal_diagnostic=NULL WHERE environment_id=$1")
            .bind(current.id.as_uuid()).bind(aggregate).execute(&mut *tx).await?;
        let old_lease = state != OperationState::Accepted;
        sqlx::query("UPDATE environment.environment_operations SET contract=$2,state=$3,retry_count=$4,max_attempts=3,lease_owner=$5,lease_token=$6,lease_expires_at=$7,heartbeat_at=CASE WHEN $5::text IS NULL THEN NULL ELSE $7::timestamptz-interval '1 second' END,finished_at=NULL,diagnostic=NULL WHERE operation_id=$1")
            .bind(current.operation.id.as_uuid()).bind(operation)
            .bind(serde_json::to_value(state)?.as_str().ok_or("state")?)
            .bind(i32::try_from(current.operation.attempt - 1)?)
            .bind(old_lease.then_some("pre-upgrade-worker"))
            .bind(old_lease.then(uuid::Uuid::now_v7))
            .bind(old_lease.then_some(self.store.current_time().await?.get() - time::Duration::seconds(1)))
            .execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn pre_upgrade_delete_conflict(&self) -> TestResult {
        self.mock
            .lifecycle
            .as_ref()
            .ok_or("lifecycle")?
            .lock()
            .await
            .deletion = LifecycleDeletion::ReplaceNamespaceConflict;
        // Model the old protocol's already-issued destructive call, including a real HTTPS
        // DELETE A and 409/replacement. Its old durable operation has no UID checkpoint.
        assert!(
            self.executor
                .remove_owned_namespace(
                    self.initial.id,
                    self.initial.project_id,
                    &format!("lw-env-{}", self.initial.id),
                    self.store
                        .load(self.initial.id)
                        .await?
                        .operation
                        .deadline_at,
                    uuid::Uuid::from_u128(1),
                    None
                )
                .await
                .is_err()
        );
        self.assert_only_original_delete(1).await
    }
}

async fn legacy_takeover(runtime: RuntimeKind) -> TestResult {
    let context = Context::start_runtime(runtime).await?;
    context.pre_upgrade_delete_conflict().await?;
    for state in [
        OperationState::Accepted,
        OperationState::Running,
        OperationState::Cancelling,
    ] {
        context.seed_legacy_active(state).await?;
        let requests_before = context.mock.events.lock().await.len();
        let worker = if runtime == RuntimeKind::Container {
            context.worker(context.executor.clone())?
        } else {
            context.vm_worker()?
        };
        let outcome = worker
            .run_once("post-upgrade-worker", context.store.current_time().await?)
            .await?;
        assert!(
            matches!(outcome, crate::ReconcileWorkerOutcome::Failed { diagnostic_code }
            if diagnostic_code == "LW_ENVIRONMENT_PROVIDER_OBSERVATION_INVALID")
        );
        let failed = context.store.load(context.initial.id).await?;
        assert_eq!(failed.operation.state, OperationState::Failed);
        assert!(failed.operation.cleanup_target.is_none());
        assert!(failed.operation.cleanup_protocol_version.is_none());
        assert_eq!(
            context.mock.events.lock().await.len(),
            requests_before,
            "legacy worker must not even start a fresh Kubernetes capture"
        );
        context.assert_only_original_delete(1).await?;
    }
    Ok(())
}

#[tokio::test]
async fn container_legacy_active_and_scheduled_retry_never_rebind_replacement() -> TestResult {
    Box::pin(legacy_takeover(RuntimeKind::Container)).await
}

#[tokio::test]
async fn kubevirt_legacy_active_and_scheduled_retry_never_rebind_replacement() -> TestResult {
    Box::pin(legacy_takeover(RuntimeKind::VirtualMachine)).await
}

#[tokio::test]
async fn marked_new_kubevirt_operation_captures_through_native_executor_before_delete() -> TestResult
{
    let context = Context::start_runtime(RuntimeKind::VirtualMachine).await?;
    let outcome = context
        .vm_worker()?
        .run_once("new-protocol-vm", context.store.current_time().await?)
        .await?;
    assert!(
        matches!(&outcome, crate::ReconcileWorkerOutcome::Pending),
        "native VM checkpoint outcome={outcome:?}, diagnostic={:?}, Kubernetes={:?}",
        context
            .store
            .load(context.initial.id)
            .await?
            .operation
            .diagnostic_code,
        context.mock.events.lock().await
    );
    let current = context.store.load(context.initial.id).await?;
    assert_eq!(
        current.operation.cleanup_protocol_version,
        Some(contracts::environment::ENVIRONMENT_CLEANUP_PROTOCOL_VERSION)
    );
    assert_eq!(
        current
            .operation
            .cleanup_target
            .as_ref()
            .map(|target| target.uid),
        Some(uuid::Uuid::from_u128(1))
    );
    assert!(
        context
            .mock
            .lifecycle
            .as_ref()
            .ok_or("lifecycle")?
            .lock()
            .await
            .delete_uids
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn legacy_lease_cannot_mint_protocol_or_target_via_cas() -> TestResult {
    let context = Context::start().await?;
    context.pre_upgrade_delete_conflict().await?;
    context.seed_legacy_active(OperationState::Running).await?;
    let lease = context
        .store
        .claim_due("legacy-cas", Duration::from_secs(10))
        .await?
        .ok_or("lease")?;
    let target = EnvironmentCleanupTarget {
        namespace: format!("lw-env-{}", context.initial.id),
        uid: uuid::Uuid::from_u128(999),
        objects: Vec::new(),
    };
    assert!(matches!(
        crate::lifecycle::pin_cleanup_target(&lease.instance, target.clone()),
        Err(crate::LifecycleError::ProviderObservationInvalid)
    ));
    let mut forged = lease.instance.clone();
    forged.revision = Revision::new(forged.revision.get() + 1)?;
    forged.operation.provider_step += 1;
    forged.operation.cleanup_target = Some(target);
    assert!(matches!(
        context.store.save_reconciled(&lease, &forged).await,
        Err(crate::EnvironmentStoreError::InvalidOperationRecord)
    ));
    forged.operation.cleanup_protocol_version =
        Some(contracts::environment::ENVIRONMENT_CLEANUP_PROTOCOL_VERSION);
    assert!(matches!(
        context.store.save_reconciled(&lease, &forged).await,
        Err(crate::EnvironmentStoreError::InvalidOperationRecord)
    ));
    context.assert_only_original_delete(1).await
}

#[tokio::test]
async fn marked_failure_before_first_capture_can_recover_without_deleting_early() -> TestResult {
    let context = Context::start().await?;
    context.fail_current().await?;
    let current = context.store.load(context.initial.id).await?;
    assert_eq!(
        current.operation.cleanup_protocol_version,
        Some(contracts::environment::ENVIRONMENT_CLEANUP_PROTOCOL_VERSION)
    );
    assert!(current.operation.cleanup_target.is_none());
    let now = context.store.current_time().await?;
    context
        .store
        .accept_command(
            "marked-first-capture-recover",
            &crate::LifecycleCommand {
                environment_id: current.id,
                kind: EnvironmentOperationKind::Recover,
                expected_revision: current.revision,
                actor_id: current.owner_id,
                trace_id: "marked-recover".to_owned(),
                accepted_at: now,
                deadline_at: UtcTimestamp::from_utc(now.get() + time::Duration::minutes(10))?,
                access_revocation_revision: current.operation.access_revocation_revision,
                preserve_mutable_disk: false,
                max_attempts: 1,
                reset_target: None,
            },
        )
        .await?;
    let outcome = context
        .worker(context.executor.clone())?
        .run_once(
            "marked-recovered-worker",
            context.store.current_time().await?,
        )
        .await?;
    assert!(matches!(outcome, crate::ReconcileWorkerOutcome::Pending));
    assert_eq!(
        context
            .store
            .load(context.initial.id)
            .await?
            .operation
            .cleanup_target
            .map(|target| target.uid),
        Some(uuid::Uuid::from_u128(1))
    );
    assert!(
        context
            .mock
            .lifecycle
            .as_ref()
            .ok_or("lifecycle")?
            .lock()
            .await
            .delete_uids
            .is_empty()
    );
    Ok(())
}
