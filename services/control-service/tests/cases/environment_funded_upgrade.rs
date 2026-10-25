//! Real Control publication, funded Resource chain, HTTP/JWT and `PostgreSQL` upgrade regression.
//! Only object storage, service transport and Kubernetes use the existing external fixtures.
#![allow(clippy::too_many_lines)]
use super::{LifecycleHttp, Result};
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use contracts::authoring::{EnvironmentRuntimeSpec, EnvironmentSpec, RuntimeKind};
use contracts::environment::{EnvironmentInstance, ObservedEnvironmentState};
use contracts::events::{CloudEvent, EVENT_CONTRACTS, SPEC_VERSION, subjects};
use contracts::http::{
    CreateEnvironmentTemplateReleaseRequest, IdempotencyKey, ManualWorkTemplateRequest,
    PlatformImageEntry, PlatformImageKind, PlatformImageStatus, StrongEtag,
    UpgradeEnvironmentTemplateRequest,
};
use contracts::{EventId, ReleaseId, Revision, Sequence};
use environment_service::{
    ContainerReleaseResolver, EnvironmentRuntimeAdmission, PgEnvironmentStore,
    PgReleaseProjectionStore, ReconcileWorker, ReconcileWorkerOutcome,
};
use sqlx::PgPool;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

#[allow(clippy::too_many_arguments)]
pub(super) async fn exercise(
    pool: &PgPool,
    store: &PgEnvironmentStore,
    admission: &EnvironmentRuntimeAdmission,
    worker: &ReconcileWorker,
    instance: &EnvironmentInstance,
    http: &LifecycleHttp,
    calls: &AtomicUsize,
    reject_next_restart: &AtomicBool,
    provider: &environment_service::ContainerProvider<
        super::ClusterFixture,
        PgReleaseProjectionStore,
    >,
) -> Result<EnvironmentInstance> {
    let releases = PgReleaseProjectionStore::new(pool.clone());
    let source = releases
        .resolve(instance.release_id, instance.release_version)
        .await?
        .projection;
    let family = source.release.template_family_id().ok_or("source family")?;
    let target = publish(
        pool,
        store,
        instance,
        &source.environment_spec,
        &family,
        "compatible",
    )
    .await?;
    project(&releases, &target).await?;
    let request = intent(&target);
    store
        .prepare_work_template_upgrade(instance, &request)
        .await?;
    let original_lease = admission.for_instance(instance).await?.lease_authorization;
    let funding = funding_snapshot(pool).await?;

    // Every denied request is checked before Access revocation, intent or provider writes.
    for (label, spec, target_family, expected) in [
        (
            "foreign-family",
            source.environment_spec.clone(),
            "foreign-family".to_owned(),
            "LW_WORK_TEMPLATE_UPGRADE_FAMILY_MISMATCH",
        ),
        (
            "excess-cpu",
            {
                let mut spec = source.environment_spec.clone();
                spec.resources.cpu_millicores += 1;
                spec
            },
            family.clone(),
            "LW_WORK_TEMPLATE_UPGRADE_REQUIRES_RESOURCE_APPROVAL",
        ),
        (
            "network",
            {
                let mut spec = source.environment_spec.clone();
                spec.network = contracts::authoring::NetworkPolicySpec::AllowAll;
                spec
            },
            family.clone(),
            "LW_WORK_TEMPLATE_UPGRADE_REQUIRES_RESOURCE_APPROVAL",
        ),
    ] {
        let changed = publish(pool, store, instance, &spec, &target_family, label).await?;
        project(&releases, &changed).await?;
        assert_denied(
            pool,
            store,
            http,
            instance,
            label,
            &intent(&changed),
            expected,
            calls,
        )
        .await?;
        match admission
            .for_release(instance, changed.release.id, changed.release.version)
            .await
        {
            Err(error) => assert_eq!(
                error.diagnostic_code(),
                "LW_RUNTIME_CONFIGURATION_BINDING_MISMATCH",
                "{label}"
            ),
            Ok(_) => return Err(format!("Resource authorized incompatible target: {label}").into()),
        }
    }
    let unpublished = UpgradeEnvironmentTemplateRequest {
        target_release_id: ReleaseId::new(),
        target_release_version: target.release.version + 10,
        confirmed: true,
    };
    assert_denied(
        pool,
        store,
        http,
        instance,
        "unpublished",
        &unpublished,
        "LW_WORK_TEMPLATE_UPGRADE_RELEASE_UNAVAILABLE",
        calls,
    )
    .await?;

    // Control is authoritative even if Environment has not consumed the withdrawal event yet.
    let withdrawn = publish(
        pool,
        store,
        instance,
        &source.environment_spec,
        &family,
        "withdrawn",
    )
    .await?;
    project(&releases, &withdrawn).await?;
    let (control, owner) = control(pool, instance.project_id).await?;
    control
        .withdraw_project_release(
            instance.project_id,
            withdrawn.release.id,
            withdrawn.release.version,
            owner,
            false,
            "UPGRADE_WITHDRAWN",
            &IdempotencyKey::parse("upgrade-withdrawn")?,
            store.current_time().await?,
            "upgrade-withdrawn",
        )
        .await?;
    assert_denied(
        pool,
        store,
        http,
        instance,
        "withdrawn",
        &intent(&withdrawn),
        "LW_RUNTIME_SCOPE_DENIED",
        calls,
    )
    .await?;

    // Old immutable approval JSON stays old: missing projection must not panic or mint rights.
    let approval: (uuid::Uuid, serde_json::Value) = sqlx::query_as("SELECT a.approval_id,a.contract FROM resource.resource_approvals a JOIN resource.capacity_claims c ON c.approval_id=a.approval_id JOIN resource.resource_leases l ON l.claim_id=c.claim_id WHERE l.lease_id=$1")
        .bind(original_lease.lease_id.as_uuid()).fetch_one(pool).await?;
    let mut legacy = approval.1.clone();
    legacy["runtimeAuthorization"]["configuration"]
        .as_object_mut()
        .ok_or("configuration")?
        .remove("permissionProjectionSha256");
    sqlx::query("UPDATE resource.resource_approvals SET contract=$2 WHERE approval_id=$1")
        .bind(approval.0)
        .bind(&legacy)
        .execute(pool)
        .await?;
    assert_denied(
        pool,
        store,
        http,
        instance,
        "legacy-approval",
        &request,
        "LW_RUNTIME_AUTHORIZATION_REAPPROVAL_REQUIRED",
        calls,
    )
    .await?;
    match admission.for_instance(instance).await {
        Err(error) => assert_eq!(
            error.diagnostic_code(),
            "LW_RUNTIME_AUTHORIZATION_REAPPROVAL_REQUIRED"
        ),
        Ok(_) => return Err("legacy approval authorized new compute".into()),
    }
    sqlx::query("UPDATE resource.resource_approvals SET contract=$2 WHERE approval_id=$1")
        .bind(approval.0)
        .bind(&approval.1)
        .execute(pool)
        .await?;

    sqlx::query("UPDATE access.actors SET disabled_at=clock_timestamp() WHERE actor_id=$1")
        .bind(instance.owner_id.as_uuid())
        .execute(pool)
        .await?;
    assert_denied(
        pool,
        store,
        http,
        instance,
        "revoked-owner",
        &request,
        "LW_RUNTIME_SCOPE_DENIED",
        calls,
    )
    .await?;
    sqlx::query("UPDATE access.actors SET disabled_at=NULL WHERE actor_id=$1")
        .bind(instance.owner_id.as_uuid())
        .execute(pool)
        .await?;

    for (mode, code) in [
        (1, "LW_ENVIRONMENT_ACCESS_REVOCATION_RESPONSE_INVALID"),
        (2, "LW_ENVIRONMENT_ACCESS_REVOCATION_TIMEOUT"),
    ] {
        http.revocation_mode.store(mode, Ordering::SeqCst);
        let counts = operation_counts(pool).await?;
        let before = calls.load(Ordering::SeqCst);
        let (status, problem) =
            send_upgrade(http, instance, "compatible-upgrade", &request).await?;
        assert!(!status.is_success(), "{problem}");
        assert_eq!(problem["diagnosticCode"], code);
        assert_eq!(
            store.load(instance.id).await?,
            *instance,
            "revocation failure retains queryable original state"
        );
        assert_eq!(operation_counts(pool).await?, counts);
        assert_eq!(funding_snapshot(pool).await?, funding);
        assert_eq!(calls.load(Ordering::SeqCst), before);
    }
    http.revocation_mode.store(0, Ordering::SeqCst);

    let before = calls.load(Ordering::SeqCst);
    reject_next_restart.store(true, Ordering::SeqCst);
    let (first, second) = tokio::join!(
        send_upgrade(http, instance, "compatible-upgrade", &request),
        send_upgrade(http, instance, "compatible-upgrade", &request)
    );
    let first = first?;
    let second = second?;
    assert_eq!(first.0, StatusCode::ACCEPTED, "{}", first.1);
    assert_eq!(second.0, StatusCode::ACCEPTED, "{}", second.1);
    assert_eq!(
        first.1, second.1,
        "same-key concurrency returns the same operation"
    );
    assert!(matches!(
        super::run_due(worker, store, "upgrade-restart-failure").await?,
        ReconcileWorkerOutcome::Failed { .. }
    ));
    let failed = store.load(instance.id).await?;
    assert_eq!(failed.observed_state, ObservedEnvironmentState::Failed);
    assert_eq!(failed.release_id, target.release.id);
    assert!(failed.operation.template_upgrade.is_some());
    assert_eq!(failed.lease_id, instance.lease_id);
    let counts = operation_counts(pool).await?;
    assert_eq!(
        send_upgrade(http, instance, "compatible-upgrade", &request).await?,
        first
    );
    assert_eq!(
        operation_counts(pool).await?,
        counts,
        "retrying the accepted request adds no operation"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        before,
        "failed cluster call installed no replacement"
    );
    http.command(&failed, "recover").await?;
    for _ in 0..8 {
        super::run_due(worker, store, "upgrade-recovery").await?;
        if store.load(instance.id).await?.observed_state == ObservedEnvironmentState::Ready {
            break;
        }
    }
    let ready = store.load(instance.id).await?;
    assert_eq!(ready.observed_state, ObservedEnvironmentState::Ready);
    assert_eq!(ready.release_id, target.release.id);
    assert_eq!(ready.lease_id, instance.lease_id);
    assert_eq!(ready.capacity_binding, instance.capacity_binding);
    assert_eq!(calls.load(Ordering::SeqCst), before + 1);
    assert_eq!(
        admission.for_instance(&ready).await?.lease_authorization,
        original_lease
    );
    assert_eq!(
        funding_snapshot(pool).await?,
        funding,
        "upgrade and recovery cannot create approval/claim/lease/hold or ledger charge"
    );
    // Provider plans continue to address exactly the same workspace volume across release cutover.
    let old_plan = provider.plan(
        instance,
        &releases
            .resolve(instance.release_id, instance.release_version)
            .await?,
        environment_service::ReconcileAction::Restart,
    )?;
    let new_plan = provider.plan(
        &ready,
        &releases
            .resolve(ready.release_id, ready.release_version)
            .await?,
        environment_service::ReconcileAction::Restart,
    )?;
    let workspace = |plan: &environment_service::ContainerResourcePlan| {
        plan.resources
            .iter()
            .filter(|resource| resource.kind == "PersistentVolumeClaim")
            .map(|resource| {
                (
                    resource.namespace.clone(),
                    resource.name.clone(),
                    resource.document.clone(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert!(
        !workspace(&old_plan).is_empty(),
        "original workspace PVC is present"
    );
    assert_eq!(workspace(&old_plan), workspace(&new_plan));
    Ok(ready)
}

fn intent(target: &contracts::events::ReleasePublished) -> UpgradeEnvironmentTemplateRequest {
    UpgradeEnvironmentTemplateRequest {
        target_release_id: target.release.id,
        target_release_version: target.release.version,
        confirmed: true,
    }
}

async fn control(
    pool: &PgPool,
    project: contracts::ProjectId,
) -> Result<(control_service::ControlService, contracts::ActorId)> {
    let config = super::super::super::control_config()
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    let service = control_service::ControlService::new(
        pool.clone(),
        Arc::new(super::super::super::FixtureObjects { fail_second: false }),
        config,
    )?;
    let owner: uuid::Uuid =
        sqlx::query_scalar("SELECT owner_actor_id FROM control.projects WHERE project_id=$1")
            .bind(project.as_uuid())
            .fetch_one(pool)
            .await?;
    Ok((service, owner.to_string().parse()?))
}

async fn publish(
    pool: &PgPool,
    store: &PgEnvironmentStore,
    instance: &EnvironmentInstance,
    spec: &EnvironmentSpec,
    family: &str,
    label: &str,
) -> Result<contracts::events::ReleasePublished> {
    let project = instance.project_id;
    let (control, owner) = control(pool, project).await?;
    let now = store.current_time().await?;
    let mut spec = spec.clone();
    if let EnvironmentRuntimeSpec::Container { build_context, .. } = &mut spec.runtime {
        *build_context = None;
    }
    let config = super::super::super::control_config()
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    let image = PlatformImageEntry {
        catalog_id: contracts::PlatformImageId::new(),
        kind: PlatformImageKind::Container,
        binding: "upgrade-image".into(),
        source_reference: format!(
            "{}/upgrade:288",
            config.container_build.output_repository_prefix
        ),
        resolved_digest: format!("sha256:{}", "c".repeat(64)),
        media_type: "application/vnd.oci.image.manifest.v1+json".into(),
        size_bytes: 1024,
        status: PlatformImageStatus::Active,
        trust_revision: config.trust_revision.get(),
        repin_generation: 1,
        capacity_bytes: None,
        disk_sha256: None,
        format: None,
        pinned_at: now,
        updated_at: now,
    };
    let request = CreateEnvironmentTemplateReleaseRequest {
        project_id: project,
        course_id: instance.course_id,
        candidate_id: None,
        candidate_revision: None,
        approval_id: None,
        runtime_kind: RuntimeKind::Container,
        manual: Some(Box::new(ManualWorkTemplateRequest {
            platform_image_id: image.catalog_id,
            platform_image_generation: 1,
            template_family_id: family.into(),
            description: label.into(),
            spec: spec.clone(),
            reason: "reviewed compatible upgrade".into(),
            confirmed: true,
        })),
    };
    let release = control
        .create_project_work_release(
            project,
            &request,
            owner,
            false,
            &IdempotencyKey::parse(&format!("upgrade-publish-{label}"))?,
            now,
            "upgrade-publish",
            &[image],
        )
        .await?;
    Ok(contracts::events::ReleasePublished {
        release,
        environment_spec: spec,
    })
}

async fn project(
    store: &PgReleaseProjectionStore,
    target: &contracts::events::ReleasePublished,
) -> Result {
    let event = EVENT_CONTRACTS
        .iter()
        .find(|event| event.subject == subjects::ENVIRONMENT_TEMPLATE_RELEASE_PUBLISHED)
        .ok_or("release event")?;
    store
        .accept(
            &format!("upgrade-projection-{}", target.release.id),
            &CloudEvent {
                specversion: SPEC_VERSION.into(),
                id: EventId::new(),
                source: event.source().into(),
                event_type: event.event_type.into(),
                subject: event.subject.into(),
                time: target.release.published_at,
                datacontenttype: "application/json".into(),
                dataschema: event.data_schema(),
                project_id: target.release.project_id,
                course_id: target.release.course_id,
                aggregate_revision: Revision::new(target.release.version)?,
                aggregate_sequence: Sequence(1),
                trace_id: "upgrade-publication".into(),
                data: target.clone(),
            },
        )
        .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn assert_denied(
    pool: &PgPool,
    store: &PgEnvironmentStore,
    http: &LifecycleHttp,
    instance: &EnvironmentInstance,
    key: &str,
    request: &UpgradeEnvironmentTemplateRequest,
    expected: &str,
    calls: &AtomicUsize,
) -> Result {
    let counts = operation_counts(pool).await?;
    let funding = funding_snapshot(pool).await?;
    let provider_calls = calls.load(Ordering::SeqCst);
    let revocations = http.revocations.load(Ordering::SeqCst);
    let (status, problem) =
        send_upgrade(http, instance, &format!("upgrade-denied-{key}"), request).await?;
    assert!(!status.is_success(), "{key}: {problem}");
    assert_eq!(problem["diagnosticCode"], expected, "{key}: {problem}");
    if expected == "LW_RUNTIME_AUTHORIZATION_REAPPROVAL_REQUIRED" {
        assert!(
            problem["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("重新提交资源申请"))
        );
    }
    assert_eq!(store.load(instance.id).await?, *instance, "{key}");
    assert_eq!(operation_counts(pool).await?, counts, "{key}");
    assert_eq!(funding_snapshot(pool).await?, funding, "{key}");
    assert_eq!(calls.load(Ordering::SeqCst), provider_calls, "{key}");
    assert_eq!(
        http.revocations.load(Ordering::SeqCst),
        revocations,
        "{key}"
    );
    Ok(())
}

async fn funding_snapshot(pool: &PgPool) -> Result<serde_json::Value> {
    Ok(sqlx::query_scalar("SELECT jsonb_build_object('approvals',(SELECT count(*) FROM resource.resource_approvals),'claims',(SELECT count(*) FROM resource.capacity_claims),'leases',(SELECT count(*) FROM resource.resource_leases),'holds',(SELECT jsonb_agg(to_jsonb(h) ORDER BY h.hold_id) FROM resource.credit_holds h),'ledger',(SELECT count(*) FROM resource.credit_ledger_entries))").fetch_one(pool).await?)
}
async fn operation_counts(pool: &PgPool) -> Result<(i64, i64)> {
    Ok(sqlx::query_as("SELECT (SELECT count(*) FROM environment.environment_operations), (SELECT count(*) FROM environment.outbox_events)").fetch_one(pool).await?)
}
async fn send_upgrade(
    http: &LifecycleHttp,
    instance: &EnvironmentInstance,
    key: &str,
    request: &UpgradeEnvironmentTemplateRequest,
) -> Result<(StatusCode, serde_json::Value)> {
    let mut input = Request::builder()
        .method("POST")
        .uri(format!(
            "/api/v1/environments/{}/template-upgrade",
            instance.id
        ))
        .header("authorization", format!("Bearer {}", http.access_token))
        .header("x-labweaver-actor-id", instance.owner_id.to_string())
        .header("x-labweaver-session-id", uuid::Uuid::now_v7().to_string())
        .header(
            "if-match",
            StrongEtag::from_revision(instance.revision).header_value(),
        )
        .header("idempotency-key", key)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(request)?))?;
    input
        .extensions_mut()
        .insert(telemetry::RequestContext::generate());
    let response = http.router.clone().oneshot(input).await?;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1_048_576).await?;
    Ok((status, serde_json::from_slice(&bytes)?))
}
use tower::ServiceExt;
