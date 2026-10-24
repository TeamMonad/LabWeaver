//! C-stage regression using the parent's real Control publication, funded Resource lease,
//! JWT verifier and `PostgreSQL` repositories. Only the Kubernetes boundary is simulated.
use super::{LifecycleHttp, Result};
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use contracts::environment::{
    EnvironmentInstance, EnvironmentOperationKind, ObservedEnvironmentState,
};
use contracts::events::{CloudEvent, EVENT_CONTRACTS, SPEC_VERSION, subjects};
use contracts::http::{StrongEtag, UpgradeEnvironmentTemplateRequest};
use contracts::{EventId, ReleaseId, Revision, Sequence, UtcTimestamp};
use environment_service::{
    ContainerReleaseResolver, EnvironmentRuntimeAdmission, LifecycleCommand, PgEnvironmentStore,
    PgReleaseProjectionStore, ReconcileWorker, ReconcileWorkerOutcome,
};
use sqlx::PgPool;
use std::sync::atomic::{AtomicUsize, Ordering};
use tower::ServiceExt;

pub(super) async fn exercise(
    pool: &PgPool,
    store: &PgEnvironmentStore,
    admission: &EnvironmentRuntimeAdmission,
    worker: &ReconcileWorker,
    instance: &EnvironmentInstance,
    http: &LifecycleHttp,
    calls: &AtomicUsize,
) -> Result<EnvironmentInstance> {
    let releases = PgReleaseProjectionStore::new(pool.clone());
    let mut target = releases
        .resolve(instance.release_id, instance.release_version)
        .await?
        .projection;
    target.release.id = ReleaseId::new();
    target.release.version += 1;
    target.release.published_at = store.current_time().await?;
    if let contracts::supply_chain::ImageArtifact::Container {
        id,
        build_request_id,
        digest,
        ..
    } = &mut target.release.artifact
    {
        *id = contracts::ImageArtifactId::new();
        *build_request_id = contracts::BuildRequestId::new();
        *digest = format!("sha256:{}", "c".repeat(64));
    }
    let event_contract = EVENT_CONTRACTS
        .iter()
        .find(|event| event.subject == subjects::ENVIRONMENT_TEMPLATE_RELEASE_PUBLISHED)
        .ok_or("release event")?;
    // The target is an immutable publication fixture, not a fabricated Resource grant.
    // The actual original Resource authorization continues to name the original release.
    releases
        .accept(
            "native-upgrade-target",
            &CloudEvent {
                specversion: SPEC_VERSION.into(),
                id: EventId::new(),
                source: event_contract.source().into(),
                event_type: event_contract.event_type.into(),
                subject: event_contract.subject.into(),
                time: target.release.published_at,
                datacontenttype: "application/json".into(),
                dataschema: event_contract.data_schema(),
                project_id: target.release.project_id,
                course_id: target.release.course_id,
                aggregate_revision: Revision::new(1)?,
                aggregate_sequence: Sequence(1),
                trace_id: "native-upgrade-target".into(),
                data: target.clone(),
            },
        )
        .await?;
    let request = UpgradeEnvironmentTemplateRequest {
        target_release_id: target.release.id,
        target_release_version: target.release.version,
        confirmed: true,
    };
    store
        .prepare_work_template_upgrade(instance, &request)
        .await?;
    let counts = ledger_counts(pool).await?;
    let provider_calls = calls.load(Ordering::SeqCst);
    for key in [
        "native-upgrade-c",
        "native-upgrade-c",
        "native-upgrade-c-new-key",
    ] {
        let (status, problem) = send_upgrade(http, instance, key, &request).await?;
        assert_eq!(status, StatusCode::CONFLICT, "{problem}");
        assert_eq!(
            problem["diagnosticCode"],
            "LW_WORK_TEMPLATE_UPGRADE_UNAVAILABLE"
        );
        assert!(
            problem["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains("新建 Work") && detail.contains("迁移"))
        );
        assert_eq!(
            store.load(instance.id).await?,
            *instance,
            "C-stage rejection must preserve the complete running aggregate"
        );
        assert_eq!(
            ledger_counts(pool).await?,
            counts,
            "no operation or Outbox may be created"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            provider_calls,
            "no provider call"
        );
        assert_eq!(
            http.revocations.load(Ordering::SeqCst),
            0,
            "no Access revocation request"
        );
    }
    assert_direct_store_guard(store, admission, worker, instance, &request, calls).await
}

async fn ledger_counts(pool: &PgPool) -> Result<(i64, i64)> {
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

async fn assert_direct_store_guard(
    store: &PgEnvironmentStore,
    admission: &EnvironmentRuntimeAdmission,
    worker: &ReconcileWorker,
    instance: &EnvironmentInstance,
    request: &UpgradeEnvironmentTemplateRequest,
    calls: &AtomicUsize,
) -> Result<EnvironmentInstance> {
    // Store-level acceptance is intentionally retained for A', but is unreachable
    // through the public C-stage entry point. Fresh authority must still fence it.
    let current = admission.for_instance(instance).await?;
    let now = store.current_time().await?;
    store
        .accept_work_template_upgrade(
            "native-upgrade-store-bypass",
            &LifecycleCommand {
                environment_id: instance.id,
                kind: EnvironmentOperationKind::Restart,
                expected_revision: instance.revision,
                actor_id: instance.owner_id,
                trace_id: "native-upgrade-store-bypass".into(),
                accepted_at: now,
                deadline_at: UtcTimestamp::from_utc(now.get() + time::Duration::seconds(30))?,
                access_revocation_revision: Some(instance.revision),
                preserve_mutable_disk: true,
                max_attempts: 3,
                reset_target: None,
            },
            request,
            current.lease_authorization,
        )
        .await?;
    let before = calls.load(Ordering::SeqCst);
    assert!(
        matches!(super::run_due(worker, store, "native-upgrade-store-bypass").await?,
        ReconcileWorkerOutcome::Failed { ref diagnostic_code } if diagnostic_code == "LW_RUNTIME_CONFIGURATION_BINDING_MISMATCH")
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        before,
        "fresh configuration binding must reject before the provider"
    );
    let failed = store.load(instance.id).await?;
    assert_eq!(failed.observed_state, ObservedEnvironmentState::Failed);
    assert_eq!(failed.lease_id, instance.lease_id);
    assert_eq!(failed.capacity_binding, instance.capacity_binding);
    Ok(failed)
}
