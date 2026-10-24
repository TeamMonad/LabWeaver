//! Real `PostgreSQL` upgrade lifecycle; only the runtime provider is an external-boundary fixture.
use super::*;
use contracts::authoring::{EnvironmentClass, RuntimeKind};
use contracts::events::ReleasePublished;
use contracts::http::UpgradeEnvironmentTemplateRequest;
use environment_service::TemplateUpgradeError;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

struct CurrentClockRuntimeProvider;

#[async_trait]
impl EnvironmentProvider for CurrentClockRuntimeProvider {
    fn binding(&self) -> &'static str {
        "container-primary-v1"
    }

    async fn execute(
        &self,
        action: ReconcileAction,
        instance: &contracts::environment::EnvironmentInstance,
    ) -> Result<environment_service::ProviderOutcome<ProviderObservation>, ProviderFailure> {
        let outcome = LifecycleSuccessProvider {
            restart_ready_observed_at: None,
        }
        .execute(action, instance)
        .await?;
        match outcome {
            environment_service::ProviderOutcome::Completed(mut observation) => {
                let observed = time::OffsetDateTime::now_utc();
                let observed = observed
                    .replace_nanosecond(u32::from(observed.millisecond()) * 1_000_000)
                    .map_err(|_| ProviderFailure {
                        code: ProviderFailureCode::Rejected,
                        retryable: false,
                    })?;
                let now = UtcTimestamp::from_utc(observed).map_err(|_| ProviderFailure {
                    code: ProviderFailureCode::Rejected,
                    retryable: false,
                })?;
                for endpoint in &mut observation.endpoints {
                    endpoint.observed_at = now;
                }
                Ok(environment_service::ProviderOutcome::Completed(observation))
            }
            environment_service::ProviderOutcome::Pending => {
                Ok(environment_service::ProviderOutcome::Pending)
            }
        }
    }
}

fn worker(store: PgEnvironmentStore, fail: bool) -> TestResult<ReconcileWorker> {
    let mut registry = ProviderRegistry::default();
    if fail {
        registry.register(Arc::new(CleanupFailureProvider))?;
    } else {
        registry.register(Arc::new(CurrentClockRuntimeProvider))?;
    }
    Ok(ReconcileWorker::new(
        store,
        Reconciler::new(registry, Duration::from_millis(100))?,
        Duration::from_millis(1_100),
        Duration::from_millis(100),
    )?)
}

async fn ready_work(
    store: &PgEnvironmentStore,
) -> TestResult<contracts::environment::EnvironmentInstance> {
    let now = store.current_time().await?;
    let mut instance = requested_instance();
    instance.class = EnvironmentClass::Work;
    instance.lease_id = Some(LeaseId::new());
    instance.capacity_binding = Some("upgrade-workspace".to_owned());
    instance.eligibility_expires_at = Some(UtcTimestamp::from_utc(
        now.get() + time::Duration::hours(1),
    )?);
    instance.operation.actor_id = instance.owner_id;
    instance.operation.accepted_at = now;
    instance.operation.next_attempt_at = now;
    instance.operation.deadline_at =
        UtcTimestamp::from_utc(now.get() + time::Duration::minutes(10))?;
    instance.operation.lease_authorization = Some(EnvironmentLeaseAuthorization {
        resource_request_id: ResourceRequestId::new(),
        lease_id: instance.lease_id.ok_or("lease")?,
        lease_revision: revision(1),
        environment_id: instance.id,
        project_id: instance.project_id,
        course_id: instance.course_id,
        owner_actor_id: instance.owner_id,
        capacity_binding: "upgrade-workspace".to_owned(),
        approved_resources: instance.approved_resources.clone(),
        gpu_allocation: None,
        active_from: UtcTimestamp::from_utc(now.get() - time::Duration::seconds(1))?,
        expires_at: instance.eligibility_expires_at.ok_or("expiry")?,
    });
    store.create("upgrade-create-work", &instance).await?;
    let runtime = worker(store.clone(), false)?;
    for _ in 0..4 {
        runtime
            .run_once("upgrade-create-worker", store.current_time().await?)
            .await?;
        tokio::time::sleep(Duration::from_millis(110)).await;
    }
    let ready = store.load(instance.id).await?;
    assert_eq!(ready.observed_state, ObservedEnvironmentState::Ready);
    Ok(ready)
}

fn projection(
    instance: &contracts::environment::EnvironmentInstance,
) -> TestResult<ReleasePublished> {
    let image_id = contracts::PlatformImageId::new();
    let digest = format!("sha256:{}", "a".repeat(64));
    let spec = serde_json::json!({
        "apiVersion":"environment.labweaver.io/v1","kind":"EnvironmentSpec","name":"manual-work","class":"work",
        "resources":instance.approved_resources,"network":{"mode":"deny_all"},
        "entries":[{"name":"app","protocol":"https","servicePort":8080}],
        "security":{"userPolicy":"non_root_required","rootFilesystemPolicy":"read_only_required","privilegeEscalationPolicy":"deny","publicExposurePolicy":"deny","securityProfileBinding":"restricted-v1"},
        "runtime":{"kind":"container","provider_binding":instance.provider_binding,"service_port":8080},
        "retention":{"policyId":contracts::PolicyId::new(),"policyRevision":1,"class":"run_evidence","retainUntil":instance.eligibility_expires_at,"disposition":"delete"}
    });
    Ok(serde_json::from_value(
        serde_json::json!({"environmentSpec":spec,"release":{
            "id":instance.release_id,"projectId":instance.project_id,"courseId":instance.course_id,"version":instance.release_version,
            "candidateId":null,"candidateRevision":null,"agentRunId":null,"approval":null,"runtimeKind":"container",
            "manualApproval":{"templateFamilyId":"manual-upgrade-202","description":"upgrade fixture","spec":spec,"reason":"human reviewed",
                "approvedBy":instance.owner_id,"approvedAt":instance.operation.accepted_at,"trustRevision":1,
                "platformImage":{"catalogId":image_id,"kind":"container","binding":"manual-work","sourceReference":"harbor.internal/labweaver-system/manual:202",
                    "resolvedDigest":digest,"mediaType":"application/vnd.oci.image.manifest.v1+json","sizeBytes":1,"status":"active","trustRevision":1,"repinGeneration":1,
                    "pinnedAt":instance.operation.accepted_at,"updatedAt":instance.operation.accepted_at}},
            "artifact":{"kind":"catalog_container","id":image_id,"catalog_id":image_id,"repin_generation":1,"trust_revision":1,"repository":"harbor.internal/labweaver-system/manual","digest":digest},
            "publishedBy":instance.owner_id,"publishedAt":instance.operation.accepted_at
        }}),
    )?)
}

async fn insert_projection(pool: &sqlx::PgPool, projection: &ReleasePublished) -> TestResult {
    projection.validate()?;
    sqlx::query("INSERT INTO environment.release_projections(release_id,release_version,project_id,course_id,provider_binding,projection_sha256,contract,projected_event_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8)")
        .bind(projection.release.id.as_uuid()).bind(i64::try_from(projection.release.version)?).bind(projection.release.project_id.as_uuid())
        .bind(projection.release.course_id.map(CourseId::as_uuid)).bind("container-primary-v1")
        .bind(Sha256Digest::of_canonical(projection)?.to_string()).bind(serde_json::to_value(projection)?).bind(EventId::new().as_uuid()).execute(pool).await?;
    Ok(())
}

fn request(target: &ReleasePublished) -> UpgradeEnvironmentTemplateRequest {
    UpgradeEnvironmentTemplateRequest {
        target_release_id: target.release.id,
        target_release_version: target.release.version,
        confirmed: true,
    }
}

async fn command(
    store: &PgEnvironmentStore,
    instance: &contracts::environment::EnvironmentInstance,
) -> TestResult<LifecycleCommand> {
    let now = store.current_time().await?;
    Ok(LifecycleCommand {
        environment_id: instance.id,
        kind: EnvironmentOperationKind::Restart,
        expected_revision: instance.revision,
        actor_id: instance.owner_id,
        trace_id: "upgrade-confirmed".to_owned(),
        accepted_at: now,
        deadline_at: UtcTimestamp::from_utc(now.get() + time::Duration::minutes(10))?,
        access_revocation_revision: Some(revision(10)),
        preserve_mutable_disk: true,
        max_attempts: 1,
        reset_target: None,
    })
}

#[tokio::test]
async fn template_upgrade_preserves_workspace_and_exposes_failure_without_completion() -> TestResult
{
    let pg = Postgres::default().with_tag("17.5-alpine").start().await?;
    let url = format!(
        "postgres://postgres:postgres@127.0.0.1:{}/postgres",
        pg.get_host_port_ipv4(5432).await?
    );
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await?;
    support::apply_environment_migrations(&pool).await?;
    let store = PgEnvironmentStore::new(pool.clone());
    let original = ready_work(&store).await?;
    let source = projection(&original)?;
    insert_projection(&pool, &source).await?;
    let mut target = source.clone();
    target.release.id = ReleaseId::new();
    target.release.version += 1;
    insert_projection(&pool, &target).await?;
    let mut unconfirmed = request(&target);
    unconfirmed.confirmed = false;
    assert!(matches!(
        store
            .prepare_work_template_upgrade(&original, &unconfirmed)
            .await,
        Err(EnvironmentStoreError::TemplateUpgrade(
            TemplateUpgradeError::ConfirmationRequired
        ))
    ));
    let mut vm = original.clone();
    vm.runtime_kind = RuntimeKind::VirtualMachine;
    assert!(matches!(
        store
            .prepare_work_template_upgrade(&vm, &request(&target))
            .await,
        Err(EnvironmentStoreError::TemplateUpgrade(
            TemplateUpgradeError::UnsupportedForVm
        ))
    ));
    let mut intent = command(&store, &original).await?;
    let authorization = original
        .operation
        .lease_authorization
        .clone()
        .ok_or("lease authorization")?;
    intent.actor_id = ActorId::new();
    assert!(matches!(
        store
            .accept_work_template_upgrade(
                "upgrade-denied-202",
                &intent,
                &request(&target),
                authorization.clone()
            )
            .await,
        Err(EnvironmentStoreError::TemplateUpgrade(
            TemplateUpgradeError::ScopeDenied
        ))
    ));
    intent.actor_id = original.owner_id;
    let accepted = store
        .accept_work_template_upgrade(
            "upgrade-confirmed-202",
            &intent,
            &request(&target),
            authorization.clone(),
        )
        .await?;
    assert_eq!(
        store
            .accept_work_template_upgrade(
                "upgrade-confirmed-202",
                &intent,
                &request(&target),
                authorization
            )
            .await?,
        accepted
    );
    assert!(matches!(
        store
            .accept_work_template_upgrade(
                "upgrade-confirmed-202",
                &intent,
                &unconfirmed,
                original
                    .operation
                    .lease_authorization
                    .clone()
                    .ok_or("lease")?
            )
            .await,
        Err(EnvironmentStoreError::IdempotencyConflict)
    ));
    let pending = store
        .get_operation(original.id, original.owner_id, accepted.operation_id)
        .await?
        .snapshot;
    assert_eq!(pending.state, OperationState::Accepted);
    assert_eq!(
        pending
            .template_upgrade
            .ok_or("upgrade metadata")?
            .target_release_id,
        target.release.id
    );
    worker(store.clone(), true)?
        .run_once("upgrade-failing-worker", store.current_time().await?)
        .await?;
    let failed = store.load(original.id).await?;
    assert_eq!(failed.observed_state, ObservedEnvironmentState::Failed);
    assert_eq!(failed.desired_state, DesiredEnvironmentState::Running);
    assert_eq!(failed.lease_id, original.lease_id);
    assert_eq!(failed.capacity_binding, original.capacity_binding);
    assert_eq!(failed.approved_resources, original.approved_resources);
    assert!(failed.cleanup_evidence.is_none());
    let status = store
        .get_operation(original.id, original.owner_id, accepted.operation_id)
        .await?
        .snapshot;
    assert_eq!(status.state, OperationState::Failed);
    assert!(status.diagnostic_code.is_some());
    assert!(
        store
            .get_operation(original.id, ActorId::new(), accepted.operation_id)
            .await
            .is_err()
    );
    assert_upgrade_timeout_and_cancel_preserve_data(&failed)?;
    Ok(())
}

fn assert_upgrade_timeout_and_cancel_preserve_data(
    failed: &contracts::environment::EnvironmentInstance,
) -> TestResult {
    let mut pending = failed.clone();
    pending.observed_state = ObservedEnvironmentState::Provisioning;
    pending.failed_phase = None;
    pending.operation.state = OperationState::Running;
    let timed_out = environment_service::begin_timeout_cleanup(
        &pending,
        pending.operation.deadline_at,
        UtcTimestamp::from_utc(pending.operation.deadline_at.get() + time::Duration::minutes(1))?,
    )?;
    assert_eq!(timed_out.operation.state, OperationState::Failed);
    assert_eq!(timed_out.desired_state, DesiredEnvironmentState::Running);
    assert!(timed_out.operation.cleanup_started_at.is_none());
    assert_eq!(
        timed_out.last_diagnostic_code.as_deref(),
        Some("LW_WORK_TEMPLATE_UPGRADE_TIMEOUT")
    );
    let cancel = LifecycleCommand {
        environment_id: pending.id,
        kind: EnvironmentOperationKind::Cancel,
        expected_revision: pending.revision,
        actor_id: pending.owner_id,
        trace_id: "cancel-upgrade".to_owned(),
        accepted_at: pending.operation.accepted_at,
        deadline_at: pending.operation.deadline_at,
        access_revocation_revision: Some(revision(11)),
        preserve_mutable_disk: false,
        max_attempts: 1,
        reset_target: None,
    };
    let cancelled = environment_service::plan_command_authorized(
        &pending,
        &cancel,
        OperationId::new(),
        None,
        pending.operation.accepted_at,
    )?;
    assert_eq!(cancelled.desired_state, DesiredEnvironmentState::Stopped);
    assert_eq!(cancelled.observed_state, ObservedEnvironmentState::Stopping);
    assert!(cancelled.operation.preserve_mutable_disk);
    assert_eq!(
        environment_service::next_action(&cancelled, cancelled.operation.accepted_at)?,
        ReconcileAction::Stop
    );
    Ok(())
}
