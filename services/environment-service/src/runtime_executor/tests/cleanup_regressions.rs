//! Real `PostgreSQL` owner/worker recovery with only the HTTPS Kubernetes boundary simulated.
use super::*;
use contracts::environment::{EnvironmentInstance, EnvironmentOperationKind, OperationState};
use testcontainers::{ContainerAsync, ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;
use tokio::sync::watch;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

mod legacy_protocol;

// In-process transport at the existing Kubernetes seam, forwarding every operation to the
// production executor. The lifecycle, worker CAS and PostgreSQL store are not replaced.
struct DirectKubernetes(KubernetesContainerExecutor);
#[async_trait]
impl crate::ContainerProviderBackend for DirectKubernetes {
    async fn apply(
        &self,
        fence: &ContainerBackendFence,
        plan: &ContainerResourcePlan,
    ) -> Result<ContainerApplyObservation, ProviderFailure> {
        self.0.apply_plan(fence, plan).await
    }
    async fn observe(
        &self,
        _: &ContainerBackendFence,
        plan: &ContainerResourcePlan,
    ) -> Result<ContainerApplyObservation, ProviderFailure> {
        self.0.observe_plan(plan).await
    }
    async fn scale(
        &self,
        fence: &ContainerBackendFence,
        plan: &ContainerResourcePlan,
        replicas: u32,
    ) -> Result<ContainerApplyObservation, ProviderFailure> {
        self.0.scale(fence, plan, replicas).await
    }
    async fn restart(
        &self,
        _: &ContainerBackendFence,
        plan: &ContainerResourcePlan,
        revision: Revision,
    ) -> Result<ContainerApplyObservation, ProviderFailure> {
        self.0.restart(plan, revision).await
    }
    async fn delete_namespace(
        &self,
        fence: &ContainerBackendFence,
        plan: &ContainerResourcePlan,
        target: Option<&EnvironmentCleanupTarget>,
    ) -> Result<ProviderOutcome<ArtifactRef>, ProviderFailure> {
        self.0.delete_namespace(fence, plan, target).await
    }
}

struct Context {
    _postgres: ContainerAsync<Postgres>,
    pool: sqlx::PgPool,
    store: crate::PgEnvironmentStore,
    mock: MockKubernetes,
    _files: tempfile::TempDir,
    executor: KubernetesContainerExecutor,
    initial: EnvironmentInstance,
}

impl Context {
    async fn start() -> TestResult<Self> {
        Self::start_runtime(contracts::authoring::RuntimeKind::Container).await
    }

    async fn start_runtime(runtime: contracts::authoring::RuntimeKind) -> TestResult<Self> {
        let postgres = Postgres::default().with_tag("17.5-alpine").start().await?;
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(4)
            .connect(&format!(
                "postgres://postgres:postgres@127.0.0.1:{}/postgres",
                postgres.get_host_port_ipv4(5432).await?
            ))
            .await?;
        crate::test_support::apply_environment_migrations(&pool).await?;
        let store = crate::PgEnvironmentStore::new(pool.clone());
        let mut initial = crate::test_support::requested_instance();
        initial.runtime_kind = runtime;
        if runtime == contracts::authoring::RuntimeKind::VirtualMachine {
            initial.provider_binding = "kubevirt-primary-v1".to_owned();
        }
        store.create("cleanup-checkpoint-create", &initial).await?;
        let now = timestamp().map_err(|error| format!("{error:?}"))?;
        store
            .accept_command(
                "cleanup-checkpoint-delete",
                &crate::LifecycleCommand {
                    environment_id: initial.id,
                    kind: EnvironmentOperationKind::Delete,
                    expected_revision: initial.revision,
                    actor_id: initial.owner_id,
                    trace_id: "cleanup-checkpoint".to_owned(),
                    accepted_at: now,
                    deadline_at: UtcTimestamp::from_utc(now.get() + time::Duration::minutes(10))?,
                    // This consumer fixture starts with a persisted upstream Access revocation receipt.
                    access_revocation_revision: Some(initial.revision),
                    preserve_mutable_disk: false,
                    max_attempts: 1,
                    reset_target: None,
                },
            )
            .await?;
        let mock = spawn_lifecycle_kubernetes(Some(LifecycleKubernetes::owned(
            initial.id,
            initial.project_id,
        )))
        .await?;
        let (files, executor) = test_kubevirt_executor(&mock).await?;
        Ok(Self {
            _postgres: postgres,
            pool,
            store,
            mock,
            _files: files,
            executor,
            initial,
        })
    }

    fn worker(&self, executor: KubernetesContainerExecutor) -> TestResult<crate::ReconcileWorker> {
        let revision = Revision::new(1)?;
        let configuration = crate::ContainerProviderConfiguration::new(
            crate::ContainerReleasePolicy::new(contracts::PolicyId::new(), revision, revision)?,
            "registry.example/labweaver".to_owned(),
            "labweaver-platform".to_owned(),
            "access-service".to_owned(),
            "registry-pull".to_owned(),
            "local-path".to_owned(),
            "ReadWriteOnce",
        )?;
        let provider = crate::ContainerProvider::new(
            self.initial.provider_binding.clone(),
            Arc::new(DirectKubernetes(executor)),
            Arc::new(crate::PgReleaseProjectionStore::new(self.pool.clone())),
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

    async fn pin(&self) -> TestResult<EnvironmentCleanupTarget> {
        let outcome = self
            .worker(self.executor.clone())?
            .run_once("pin-worker", self.store.current_time().await?)
            .await?;
        assert!(matches!(outcome, crate::ReconcileWorkerOutcome::Pending));
        let current = self.store.load(self.initial.id).await?;
        let target = current
            .operation
            .cleanup_target
            .ok_or("worker did not persist target")?;
        assert_eq!(target.uid, uuid::Uuid::from_u128(1));
        let operation_target: Value = sqlx::query_scalar("SELECT contract->'cleanupTarget' FROM environment.environment_operations WHERE operation_id=$1")
            .bind(current.operation.id.as_uuid()).fetch_one(&self.pool).await?;
        assert_eq!(operation_target, serde_json::to_value(&target)?);
        assert!(
            self.mock
                .lifecycle
                .as_ref()
                .ok_or("lifecycle")?
                .lock()
                .await
                .delete_uids
                .is_empty()
        );
        Ok(target)
    }

    async fn fail_current(&self) -> TestResult {
        let lease = self
            .store
            .claim_due("failed-worker", Duration::from_secs(10))
            .await?
            .ok_or("cleanup lease")?;
        let failed = crate::apply_provider_failure(
            &lease.instance,
            lease.instance.operation.id,
            "LW_ENVIRONMENT_PROVIDER_REJECTED",
        )?;
        self.store.save_reconciled(&lease, &failed).await?;
        Ok(())
    }

    async fn namespace(&self) -> TestResult<Value> {
        Ok(self
            .mock
            .lifecycle
            .as_ref()
            .ok_or("lifecycle")?
            .lock()
            .await
            .objects
            .get(&format!("/api/v1/namespaces/lw-env-{}", self.initial.id))
            .ok_or("namespace")?
            .clone())
    }

    async fn replace_namespace(&self) -> TestResult {
        self.mock
            .lifecycle
            .as_ref()
            .ok_or("lifecycle")?
            .lock()
            .await
            .objects
            .get_mut(&format!("/api/v1/namespaces/lw-env-{}", self.initial.id))
            .ok_or("namespace")?["metadata"]["uid"] = json!(uuid::Uuid::from_u128(999));
        Ok(())
    }

    async fn scan_snapshot(&self, namespace: Value) -> TestResult<EnvironmentOrphanReport> {
        self.mock
            .inventory_pages
            .lock()
            .await
            .push_back(InventoryPage {
                continuation: None,
                status: StatusCode::OK,
                body: json!({"items":[namespace]}),
            });
        let (report, _) = self
            .executor
            .reconcile_orphans(
                &self.pool,
                contracts::authoring::RuntimeKind::Container,
                None,
            )
            .await
            .map_err(|error| format!("scan: {error:?}"))?;
        Ok(report)
    }

    async fn assert_only_original_delete(&self, expected: usize) -> TestResult {
        let fixture = self
            .mock
            .lifecycle
            .as_ref()
            .ok_or("lifecycle")?
            .lock()
            .await;
        assert_eq!(fixture.delete_uids.len(), expected);
        assert!(
            fixture
                .delete_uids
                .iter()
                .all(|uid| *uid == uuid::Uuid::from_u128(1))
        );
        assert_eq!(
            fixture
                .objects
                .get(&format!("/api/v1/namespaces/lw-env-{}", self.initial.id))
                .and_then(|object| object.pointer("/metadata/uid")),
            Some(&json!(uuid::Uuid::from_u128(999)))
        );
        Ok(())
    }
}

#[tokio::test]
async fn delete_conflict_replacement_persisted_failure_rescan_and_worker_recovery_never_delete_b()
-> TestResult {
    let context = Context::start().await?;
    let target = Box::pin(context.pin()).await?;
    let snapshot_a = context.namespace().await?;
    context
        .mock
        .lifecycle
        .as_ref()
        .ok_or("lifecycle")?
        .lock()
        .await
        .deletion = LifecycleDeletion::ReplaceNamespaceConflict;
    assert!(matches!(
        context
            .worker(context.executor.clone())?
            .run_once("delete-a", context.store.current_time().await?)
            .await?,
        crate::ReconcileWorkerOutcome::Failed { .. }
    ));
    let failed = context.store.load(context.initial.id).await?;
    assert_eq!(failed.operation.state, OperationState::Failed);
    assert_eq!(failed.operation.cleanup_target.as_ref(), Some(&target));
    // A fresh snapshot reports B as unverified. Even an older list snapshot still containing A
    // may enqueue only A's saved identity, never the B currently returned by GET.
    let fresh = context.scan_snapshot(context.namespace().await?).await?;
    assert_eq!(fresh.recovering, 0);
    assert_eq!(fresh.unverified, 1);
    let old_snapshot = context.scan_snapshot(snapshot_a).await?;
    assert_eq!(old_snapshot.recovering, 1);
    assert_eq!(
        context
            .store
            .load(context.initial.id)
            .await?
            .operation
            .cleanup_target
            .as_ref(),
        Some(&target)
    );
    assert!(matches!(
        context
            .worker(context.executor.clone())?
            .run_once("recovered-worker", context.store.current_time().await?)
            .await?,
        crate::ReconcileWorkerOutcome::Failed { .. }
    ));
    context.assert_only_original_delete(1).await
}

#[tokio::test]
async fn namespace_replacement_after_scan_enqueue_before_worker_execution_is_not_adopted()
-> TestResult {
    let context = Context::start().await?;
    let target = Box::pin(context.pin()).await?;
    context.fail_current().await?;
    assert_eq!(
        context
            .scan_snapshot(context.namespace().await?)
            .await?
            .recovering,
        1
    );
    context.replace_namespace().await?;
    assert!(matches!(
        context
            .worker(context.executor.clone())?
            .run_once("queued-worker", context.store.current_time().await?)
            .await?,
        crate::ReconcileWorkerOutcome::Failed { .. }
    ));
    assert_eq!(
        context
            .store
            .load(context.initial.id)
            .await?
            .operation
            .cleanup_target
            .as_ref(),
        Some(&target)
    );
    context.assert_only_original_delete(0).await
}

#[tokio::test]
async fn worker_restart_uses_original_durable_uid_not_the_replacement() -> TestResult {
    let context = Context::start().await?;
    let target = Box::pin(context.pin()).await?;
    context.replace_namespace().await?;
    // A new client/provider/worker has no in-memory admitted_uid from the first process.
    let (_files, restarted_executor) = test_kubevirt_executor(&context.mock).await?;
    assert!(matches!(
        context
            .worker(restarted_executor)?
            .run_once("restarted-worker", context.store.current_time().await?)
            .await?,
        crate::ReconcileWorkerOutcome::Failed { .. }
    ));
    assert_eq!(
        context
            .store
            .load(context.initial.id)
            .await?
            .operation
            .cleanup_target
            .as_ref(),
        Some(&target)
    );
    context.assert_only_original_delete(0).await
}

#[tokio::test]
async fn lost_worker_lease_cannot_commit_checkpoint_or_send_delete() -> TestResult {
    let context = Context::start().await?;
    let gate = Arc::new(KubernetesReadGate {
        path: format!("/api/v1/namespaces/lw-env-{}", context.initial.id),
        entered: tokio::sync::Notify::new(),
        resume: tokio::sync::Notify::new(),
    });
    *context.mock.read_gate.lock().await = Some(Arc::clone(&gate));
    let worker = context.worker(context.executor.clone())?;
    let now = context.store.current_time().await?;
    let old_worker = tokio::spawn(async move { worker.run_once("old-worker", now).await });
    tokio::time::timeout(Duration::from_secs(5), gate.entered.notified()).await?;
    sqlx::query("UPDATE environment.environment_operations SET lease_expires_at=clock_timestamp()-interval '1 second' WHERE environment_id=$1 AND lease_token IS NOT NULL")
        .bind(context.initial.id.as_uuid()).execute(&context.pool).await?;
    let takeover = context
        .store
        .claim_due("new-worker", Duration::from_secs(10))
        .await?
        .ok_or("takeover lease")?;
    gate.resume.notify_one();
    assert_eq!(old_worker.await??, crate::ReconcileWorkerOutcome::LeaseLost);
    assert_eq!(
        context
            .store
            .load(context.initial.id)
            .await?
            .operation
            .cleanup_target,
        None
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
    // The current lease can commit the same read-only observation through the real CAS.
    let target = context
        .executor
        .capture_cleanup_target(
            context.initial.id,
            context.initial.project_id,
            &format!("lw-env-{}", context.initial.id),
            None,
        )
        .await
        .map_err(|error| format!("{error:?}"))?
        .ok_or("target")?;
    let pinned = crate::lifecycle::pin_cleanup_target(&takeover.instance, target)?;
    context.store.save_reconciled(&takeover, &pinned).await?;
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
async fn failed_legacy_cleanup_without_original_uid_is_diagnostic_only() -> TestResult {
    let context = Context::start().await?;
    context.seed_legacy_active(OperationState::Accepted).await?;
    context.fail_current().await?;
    let report = context.scan_snapshot(context.namespace().await?).await?;
    assert_eq!(report.recovering, 0);
    assert_eq!(report.unverified, 1);
    let current = context.store.load(context.initial.id).await?;
    let now = context.store.current_time().await?;
    let recovery = context
        .store
        .accept_command(
            "legacy-cleanup-retry",
            &crate::LifecycleCommand {
                environment_id: current.id,
                kind: EnvironmentOperationKind::Cleanup,
                expected_revision: current.revision,
                actor_id: current.owner_id,
                trace_id: "legacy-cleanup".to_owned(),
                accepted_at: now,
                deadline_at: UtcTimestamp::from_utc(now.get() + time::Duration::minutes(10))?,
                access_revocation_revision: current.operation.access_revocation_revision,
                preserve_mutable_disk: false,
                max_attempts: 1,
                reset_target: None,
            },
        )
        .await;
    assert!(matches!(
        recovery,
        Err(crate::EnvironmentStoreError::Lifecycle(
            crate::LifecycleError::ProviderObservationInvalid
        ))
    ));
    assert_eq!(
        context
            .store
            .load(context.initial.id)
            .await?
            .operation
            .state,
        OperationState::Failed
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
async fn shared_namespace_object_checkpoint_rejects_replacement_on_later_call() -> TestResult {
    let (fixture, plan, profile) =
        existing_work_fixture(contracts::EnvironmentId::new(), contracts::ProjectId::new());
    let mock = spawn_lifecycle_kubernetes(Some(fixture)).await?;
    let (_files, executor) = test_kubevirt_executor(&mock).await?;
    let executor = executor.with_namespace_profile(profile);
    let target = observed_work_target(&executor, &plan).await?;
    let deployment = format!(
        "/apis/apps/v1/namespaces/{}/deployments/{}",
        plan.namespace,
        plan.placement().resource_name("runtime")
    );
    mock.lifecycle
        .as_ref()
        .ok_or("lifecycle")?
        .lock()
        .await
        .objects
        .get_mut(&deployment)
        .ok_or("deployment")?["metadata"]["uid"] = json!(uuid::Uuid::from_u128(999));
    assert!(
        executor
            .delete_work_objects(&container_test_fence(plan.environment_id), &plan, &target)
            .await
            .is_err()
    );
    let fixture = mock.lifecycle.as_ref().ok_or("lifecycle")?.lock().await;
    assert!(fixture.delete_uids.is_empty());
    assert_eq!(
        fixture
            .objects
            .get(&deployment)
            .and_then(|object| object.pointer("/metadata/uid")),
        Some(&json!(uuid::Uuid::from_u128(999)))
    );
    Ok(())
}

async fn run_inventory_pages(pages: Vec<InventoryPage>) -> TestResult {
    let context = Context::start().await?;
    context.mock.inventory_pages.lock().await.extend(pages);
    let (stop, shutdown) = watch::channel(false);
    let scanning = context.executor.clone().run_orphan_reconciler(
        context.pool.clone(),
        contracts::authoring::RuntimeKind::Container,
        shutdown,
    );
    tokio::pin!(scanning);
    // No clock advancement: a one-minute-per-page implementation cannot reach the tail here.
    let tail_seen = async {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if context.mock.inventory_pages.lock().await.is_empty() {
                    break;
                }
                context.mock.inventory_progress.notified().await;
            }
        })
        .await?;
        stop.send_replace(true);
        TestResult::Ok(())
    };
    tokio::try_join!(
        async { scanning.await.map_err(|error| format!("{error:?}").into()) },
        tail_seen
    )?;
    Ok(())
}

#[tokio::test]
async fn inventory_continuations_advance_without_waiting_for_the_periodic_tick() -> TestResult {
    run_inventory_pages(vec![
        InventoryPage {
            continuation: None,
            status: StatusCode::OK,
            body: json!({"metadata":{"continue":"page-2"},"items":[]}),
        },
        InventoryPage {
            continuation: Some("page-2".into()),
            status: StatusCode::OK,
            body: json!({"metadata":{"continue":"tail"},"items":[]}),
        },
        InventoryPage {
            continuation: Some("tail".into()),
            status: StatusCode::OK,
            body: json!({"items":[]}),
        },
    ])
    .await
}

#[tokio::test]
async fn expired_inventory_token_uses_replacement_and_still_reaches_tail() -> TestResult {
    run_inventory_pages(vec![
        InventoryPage {
            continuation: None,
            status: StatusCode::OK,
            body: json!({"metadata":{"continue":"expired"},"items":[]}),
        },
        InventoryPage {
            continuation: Some("expired".into()),
            status: StatusCode::GONE,
            body: json!({"kind":"Status","metadata":{"continue":"replacement"},"reason":"Expired"}),
        },
        InventoryPage {
            continuation: Some("replacement".into()),
            status: StatusCode::OK,
            body: json!({"metadata":{"continue":"tail"},"items":[]}),
        },
        InventoryPage {
            continuation: Some("tail".into()),
            status: StatusCode::OK,
            body: json!({"items":[]}),
        },
    ])
    .await
}
