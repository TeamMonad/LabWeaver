//! Actual process snapshots and original apply facts for two synthetic Control replicas.

use std::path::Path;

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use contracts::http::IdempotencyKey;
use contracts::platform_configuration::{
    ApplyPlatformConfigurationRequest,
    ControlConfigurationDeploymentObservation as DeploymentObservation,
    ControlConfigurationReplicaObservation as ReplicaObservation,
    PlatformConfigurationApplyStage as Stage, PlatformConfigurationDesired,
    PlatformConfigurationFailureReason, PlatformConfigurationStatus,
    RegisterPlatformConfigurationBundleRequest, ReportPlatformConfigurationApplyRequest,
    UpdatePlatformConfigurationRequest,
};
use contracts::{ActorId, Revision, UtcTimestamp};
use control_service::platform_configuration::{
    self as configuration, BundleCiphertext, LoadedConfiguration,
};
use persistence_sqlx::Sha256Digest;
use sqlx::{PgPool, postgres::PgPoolOptions};
use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;
use tower::ServiceExt;

#[allow(
    dead_code,
    reason = "shared fixtures serve separate configuration integration targets"
)]
#[path = "support/platform_configuration_common.rs"]
mod configuration_common;
#[allow(
    dead_code,
    reason = "shared HTTP fixture also exposes browser actor identities"
)]
#[path = "support/platform_configuration_http.rs"]
mod http_fixture;
mod support;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn now() -> TestResult<UtcTimestamp> {
    let value = time::OffsetDateTime::now_utc();
    Ok(UtcTimestamp::from_utc(value.replace_nanosecond(
        value.nanosecond() / 1_000_000 * 1_000_000,
    )?)?)
}
fn key(value: &str) -> TestResult<IdempotencyKey> {
    Ok(IdempotencyKey::parse(value)?)
}

fn loaded(root: &Path, name: &str, uid: &str) -> TestResult<LoadedConfiguration> {
    Ok(LoadedConfiguration::load(
        &root.join(".private/loaded/platform-configuration.json"),
        &root.join(".private/loaded/platform-configuration-source.json"),
    )?
    .on_replica(name, "labweaver-system", uid)?)
}

async fn observe(router: &Router, token: &str) -> TestResult<ReplicaObservation> {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/internal/v1/platform-configuration/loaded")
                .header("Authorization", format!("Bearer {token}"))
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("Cache-Control")
            .and_then(|v| v.to_str().ok()),
        Some("no-store")
    );
    Ok(serde_json::from_slice(
        &to_bytes(response.into_body(), 65536).await?,
    )?)
}

fn deployment(replicas: Vec<ReplicaObservation>) -> DeploymentObservation {
    DeploymentObservation {
        deployment_uid: uuid::Uuid::now_v7().to_string(),
        deployment_generation: 2,
        expected_replicas: 2,
        namespace: "labweaver-system".to_owned(),
        release_name: "labweaver".to_owned(),
        replicas,
    }
}

async fn begin(
    pool: &PgPool,
    state: PlatformConfigurationStatus,
    cipher: &[u8],
    prefix: &str,
) -> TestResult<PlatformConfigurationStatus> {
    let desired = state.desired.as_ref().ok_or("no desired")?;
    let state = configuration::register_bundle(
        pool,
        ActorId::new(),
        desired.revision,
        &RegisterPlatformConfigurationBundleRequest {
            expected_revision: state.revision,
            configuration_sha256: desired.configuration_sha256.clone(),
        },
        BundleCiphertext {
            digest: Sha256Digest::of_bytes(cipher),
            bytes: cipher.len().try_into()?,
        },
        &key(&format!("{prefix}-bundle"))?,
        now()?,
    )
    .await?;
    let mut state = configuration::request_apply(
        pool,
        ActorId::new(),
        &ApplyPlatformConfigurationRequest {
            expected_revision: state.revision,
            desired_revision: state.desired.as_ref().ok_or("no desired")?.revision,
        },
        &key(&format!("{prefix}-apply"))?,
        now()?,
    )
    .await?;
    for phase in [Stage::Validating, Stage::Applying, Stage::Verifying] {
        let attempt = state.latest_apply.as_ref().ok_or("no apply")?;
        state = configuration::report(
            pool,
            None,
            "synthetic-executor",
            attempt.id,
            &ReportPlatformConfigurationApplyRequest {
                expected_revision: attempt.revision,
                stage: phase,
                failure_reason: None,
                observed_configuration_sha256: None,
                observed_encrypted_bundle_sha256: None,
                control_replica_observations: None,
            },
            &key(&format!("{prefix}-{phase:?}"))?,
            now()?,
        )
        .await?;
    }
    Ok(state)
}

async fn succeed(
    router: &Router,
    token: &str,
    state: &PlatformConfigurationStatus,
    observed: Option<DeploymentObservation>,
    value: &str,
) -> TestResult<(StatusCode, Option<PlatformConfigurationStatus>)> {
    let attempt = state.latest_apply.as_ref().ok_or("no attempt")?;
    let request = ReportPlatformConfigurationApplyRequest {
        expected_revision: attempt.revision,
        stage: Stage::Succeeded,
        failure_reason: None,
        observed_configuration_sha256: Some(
            state
                .desired
                .as_ref()
                .ok_or("no desired")?
                .configuration_sha256
                .clone(),
        ),
        observed_encrypted_bundle_sha256: Some(attempt.encrypted_bundle_sha256.clone()),
        control_replica_observations: observed,
    };
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!(
                    "/internal/v1/platform-configuration/applies/{}",
                    attempt.id
                ))
                .header("Authorization", format!("Bearer {token}"))
                .header("Content-Type", "application/json")
                .header("If-Match", format!("\"rev-{}\"", attempt.revision.get()))
                .header("Idempotency-Key", value)
                .body(Body::from(serde_json::to_vec(&request)?))?,
        )
        .await?;
    let status = response.status();
    let body = to_bytes(response.into_body(), 1_048_576).await?;
    Ok((
        status,
        if status == StatusCode::OK {
            Some(serde_json::from_slice(&body)?)
        } else {
            None
        },
    ))
}

#[tokio::test]
#[ignore = "requires the actual age/Helm/Python Bundle producer"]
#[expect(
    clippy::too_many_lines,
    reason = "one real Bundle and database lifecycle retains the old effective version across replica mismatch, restart and retry"
)]
async fn all_control_replicas_must_report_actual_loaded_source_before_applied() -> TestResult {
    let container = Postgres::default().with_tag("17.6-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            container.get_host_port_ipv4(5432).await?
        ))
        .await?;
    support::apply_domain_migrations(&pool, persistence_sqlx::Domain::Control).await?;
    let initial = configuration::update(
        &pool,
        ActorId::new(),
        &UpdatePlatformConfigurationRequest {
            expected_revision: 0,
            configuration: PlatformConfigurationDesired::default(),
        },
        &key("replicas-first-edit")?,
        now()?,
    )
    .await?;
    let temporary = tempfile::tempdir()?;
    let first_root = temporary.path().join("first");
    let (cipher, _) = configuration_common::actual_loaded_bundle_replicas(
        initial.desired.as_ref().ok_or("no desired")?,
        &first_root,
        2,
    )?;
    let first_uid = uuid::Uuid::now_v7().to_string();
    let second_uid = uuid::Uuid::now_v7().to_string();
    let fixture = http_fixture::HttpFixture::with_loaded(
        pool.clone(),
        Some(loaded(&first_root, "control-a", &first_uid)?),
    )
    .await?;
    let second = fixture.replica(pool.clone(), loaded(&first_root, "control-b", &second_uid)?)?;
    let token = fixture.token("synthetic-executor", "control.platform_configuration.apply")?;
    let gateway = fixture.token("labweaver-access", "control.platform_configuration.apply")?;
    let rejected = fixture
        .router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/internal/v1/platform-configuration/loaded")
                .header("Authorization", format!("Bearer {gateway}"))
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(rejected.status(), StatusCode::FORBIDDEN);
    let state = begin(&pool, initial, &cipher, "first").await?;
    let first = observe(&fixture.router, &token).await?;
    let peer = observe(&second, &token).await?;
    assert_eq!(
        succeed(&fixture.router, &token, &state, None, "missing-roster")
            .await?
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        succeed(
            &fixture.router,
            &token,
            &state,
            Some(deployment(vec![first.clone()])),
            "missing-peer"
        )
        .await?
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        succeed(
            &fixture.router,
            &token,
            &state,
            Some(deployment(vec![first.clone(), first.clone()])),
            "duplicate-peer"
        )
        .await?
        .0,
        StatusCode::CONFLICT
    );
    let mut corrupt = peer.clone();
    corrupt.configuration_sha256 = "0".repeat(64);
    assert_eq!(
        succeed(
            &fixture.router,
            &token,
            &state,
            Some(deployment(vec![first.clone(), corrupt])),
            "wrong-digest"
        )
        .await?
        .0,
        StatusCode::CONFLICT
    );
    assert!(configuration::get(&pool).await?.applied.is_none());
    let (status, result) = succeed(
        &fixture.router,
        &token,
        &state,
        Some(deployment(vec![first.clone(), peer])),
        "all-first",
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    let effective = result.ok_or("no applied result")?;
    let old_effective = effective.applied.clone().ok_or("no effective")?;

    let mut changed = old_effective.configuration.clone();
    changed.lifecycle.detailed_log_retention_seconds = 60;
    let desired = configuration::update(
        &pool,
        ActorId::new(),
        &UpdatePlatformConfigurationRequest {
            expected_revision: effective.revision,
            configuration: changed,
        },
        &key("replicas-second-edit")?,
        now()?,
    )
    .await?;
    assert_eq!(desired.applied.as_ref(), Some(&old_effective));
    let next_root = temporary.path().join("next");
    let (next_cipher, _) = configuration_common::actual_loaded_bundle_replicas(
        desired.desired.as_ref().ok_or("no desired")?,
        &next_root,
        2,
    )?;
    let active = fixture.replica(pool.clone(), loaded(&next_root, "control-a", &first_uid)?)?;
    let next_peer = fixture.replica(pool.clone(), loaded(&next_root, "control-b", &second_uid)?)?;
    let state = begin(&pool, desired, &next_cipher, "next").await?;
    let updated = observe(&active, &token).await?;
    let old_peer = observe(&second, &token).await?;
    assert_eq!(
        succeed(
            &active,
            &token,
            &state,
            Some(deployment(vec![updated.clone(), old_peer])),
            "old-replica"
        )
        .await?
        .0,
        StatusCode::CONFLICT
    );
    let peer = observe(&next_peer, &token).await?;
    let restarted = fixture.replica(pool.clone(), loaded(&next_root, "control-a", &first_uid)?)?;
    assert_eq!(
        succeed(
            &restarted,
            &token,
            &state,
            Some(deployment(vec![updated, peer.clone()])),
            "old-boot-evidence"
        )
        .await?
        .0,
        StatusCode::CONFLICT
    );
    let attempt = state.latest_apply.as_ref().ok_or("no apply")?;
    let failed = configuration::report(
        &pool,
        None,
        "synthetic-executor",
        attempt.id,
        &ReportPlatformConfigurationApplyRequest {
            expected_revision: attempt.revision,
            stage: Stage::Failed,
            failure_reason: Some(PlatformConfigurationFailureReason::VerificationFailed),
            observed_configuration_sha256: None,
            observed_encrypted_bundle_sha256: None,
            control_replica_observations: None,
        },
        &key("replica-failed")?,
        now()?,
    )
    .await?;
    assert_eq!(failed.applied.as_ref(), Some(&old_effective));
    assert_eq!(
        failed.applied_state,
        contracts::platform_configuration::PlatformConfigurationAppliedState::Unverified
    );
    let failed_id = attempt.id;
    let retry = begin(&pool, failed, &next_cipher, "retry").await?;
    let fresh = observe(&restarted, &token).await?;
    let peer = observe(&next_peer, &token).await?;
    let (status, result) = succeed(
        &restarted,
        &token,
        &retry,
        Some(deployment(vec![fresh, peer])),
        "all-next",
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    let verified = result.ok_or("no verified result")?;
    assert_eq!(verified.applied, verified.desired);
    assert_eq!(
        configuration::get_apply(&pool, failed_id).await?.stage,
        Stage::Failed
    );
    assert_ne!(verified.applied.as_ref(), Some(&old_effective));
    assert_eq!(
        verified.applied.as_ref().ok_or("no applied")?.revision,
        Revision::new(
            verified
                .desired
                .as_ref()
                .ok_or("no desired")?
                .revision
                .get()
        )?
    );
    Ok(())
}
