//! Real `PostgreSQL` configuration fences, history and independently supplied executor facts.

use contracts::http::IdempotencyKey;
use contracts::platform_configuration::{
    ApplyPlatformConfigurationRequest, BuilderEntitlement, LinuxCapability,
    PlatformConfigurationApplyStage as Stage, PlatformConfigurationDesired,
    PlatformConfigurationFailureReason as Failure, PlatformConfigurationNextStep as Next,
    PlatformConfigurationStatus, PrivilegedExecutionLocation,
    RegisterPlatformConfigurationBundleRequest, ReportPlatformConfigurationApplyRequest,
    RuntimePreauthorization, RuntimePreauthorizationScope, UpdatePlatformConfigurationRequest,
};
use contracts::{ActorId, OperationId, ProblemDetails, ProjectId, Revision, UtcTimestamp};
use control_service::platform_configuration::{self as configuration, ConfigurationError};
use sqlx::{PgPool, postgres::PgPoolOptions};
use testcontainers::{ContainerAsync, ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;

#[path = "support/platform_configuration_common.rs"]
mod configuration_common;
#[path = "support/platform_configuration_http.rs"]
mod http_fixture;
use configuration_common::ciphertext;
mod support;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

async fn database() -> TestResult<(ContainerAsync<Postgres>, PgPool)> {
    let container = Postgres::default().with_tag("17.6-alpine").start().await?;
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&format!(
            "postgres://postgres:postgres@127.0.0.1:{}/postgres",
            container.get_host_port_ipv4(5432).await?
        ))
        .await?;
    support::apply_domain_migrations(&pool, persistence_sqlx::Domain::Control).await?;
    Ok((container, pool))
}

fn now() -> TestResult<UtcTimestamp> {
    Ok("2026-10-12T08:00:00.000Z".parse()?)
}
fn key(value: &str) -> TestResult<IdempotencyKey> {
    Ok(IdempotencyKey::parse(value)?)
}

async fn edit(
    pool: &PgPool,
    expected: u64,
    configuration: PlatformConfigurationDesired,
    value: &str,
) -> TestResult<PlatformConfigurationStatus> {
    Ok(configuration::update(
        pool,
        ActorId::new(),
        &UpdatePlatformConfigurationRequest {
            expected_revision: expected,
            configuration,
        },
        &key(value)?,
        now()?,
    )
    .await?)
}

async fn begin(
    pool: &PgPool,
    state: &PlatformConfigurationStatus,
    value: &str,
) -> TestResult<PlatformConfigurationStatus> {
    let state = register(pool, state, &format!("{value}-bundle")).await?;
    Ok(configuration::request_apply(
        pool,
        ActorId::new(),
        &ApplyPlatformConfigurationRequest {
            expected_revision: state.revision,
            desired_revision: state
                .desired
                .as_ref()
                .ok_or("no desired snapshot")?
                .revision,
        },
        &key(value)?,
        now()?,
    )
    .await?)
}

/// Run explicitly with the Web dependencies and a Playwright Chromium installed.
/// This uses real Control routes and `PostgreSQL`. The issuer and Access decision
/// service remain synthetic external dependencies; this is not a BFF/UI test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Node, installed Web dependencies and Playwright Chromium"]
async fn browser_sdk_registers_real_ciphertext_and_reads_durable_failure() -> TestResult {
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    let (_container, pool) = database().await?;
    let fixture = http_fixture::HttpFixture::new(pool.clone()).await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let router = fixture.router.clone();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = stopped.await;
            })
            .await
    });
    let temporary = tempfile::tempdir()?;
    let cipher = ciphertext()?;
    let input = temporary.path().join("synthetic-browser-input.json");
    let contents = serde_json::to_vec(&serde_json::json!({
        "controlUrl": format!("http://{address}"),
        "gatewayToken": fixture.token("labweaver-access", "access.control.forward")?,
        "executorToken": fixture.token("synthetic-executor", "control.platform_configuration.apply")?,
        "admin": fixture.admin,
        "otherActor": fixture.other_actor,
        "session": contracts::BffSessionId::new(),
        "ciphertext": STANDARD.encode(&cipher),
        "evidenceDirectory": temporary.path(),
    }))?;
    std::fs::write(&input, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&input, std::fs::Permissions::from_mode(0o600))?;
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = tokio::time::timeout(
        std::time::Duration::from_mins(3),
        tokio::process::Command::new("node")
            .arg("e2e/backend/platform-configuration.mjs")
            .arg(&input)
            .current_dir(root.join("web"))
            .kill_on_drop(true)
            .output(),
    )
    .await;
    let _ = stop.send(());
    server.await??;
    let output = output??;
    // The script emits bounded diagnostics, never fixture tokens or request bodies.
    if !output.status.success() {
        return Err(format!(
            "browser SDK test failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    assert!(String::from_utf8(output.stdout)?.contains("PLATFORM_CONFIGURATION_BROWSER_OK"));
    let state = configuration::get(&pool).await?;
    assert!(state.registered_bundle.is_none());
    assert!(state.applied.is_none());
    assert_eq!(
        state.applied_state,
        contracts::platform_configuration::PlatformConfigurationAppliedState::Unverified
    );
    let retry = state.latest_apply.as_ref().ok_or("no browser retry")?;
    let failed =
        configuration::get_apply(&pool, retry.previous_apply_id.ok_or("no failed attempt")?)
            .await?;
    assert_eq!(failed.stage, Stage::Failed);
    assert_eq!(failed.failed_stage, Some(Stage::Applying));
    assert_eq!(
        failed.encrypted_bundle_sha256,
        persistence_sqlx::Sha256Digest::of_bytes(&cipher).to_string()
    );
    let registrations: i64 =
        sqlx::query_scalar("SELECT count(*) FROM control.platform_configuration_bundles")
            .fetch_one(&pool)
            .await?;
    assert_eq!(registrations, 1);
    Ok(())
}

async fn register(
    pool: &PgPool,
    state: &PlatformConfigurationStatus,
    value: &str,
) -> TestResult<PlatformConfigurationStatus> {
    let desired = state.desired.as_ref().ok_or("no desired")?;
    let cipher = ciphertext()?;
    Ok(configuration::register_bundle(
        pool,
        ActorId::new(),
        desired.revision,
        &RegisterPlatformConfigurationBundleRequest {
            expected_revision: state.revision,
            configuration_sha256: desired.configuration_sha256.clone(),
        },
        configuration::BundleCiphertext {
            digest: persistence_sqlx::Sha256Digest::of_bytes(&cipher),
            bytes: cipher.len() as u64,
        },
        &key(value)?,
        now()?,
    )
    .await?)
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one PostgreSQL journey checks registration races and in-flight binding isolation"
)]
async fn bundle_registration_is_fenced_replayable_and_preserves_inflight_binding() -> TestResult {
    let (_container, pool) = database().await?;
    let state = edit(
        &pool,
        0,
        PlatformConfigurationDesired::default(),
        "bundle-settings",
    )
    .await?;
    let desired = state.desired.as_ref().ok_or("no desired")?.clone();
    let actor = ActorId::new();
    let unregistered = ApplyPlatformConfigurationRequest {
        expected_revision: state.revision,
        desired_revision: desired.revision,
    };
    assert!(matches!(
        configuration::request_apply(
            &pool,
            actor,
            &unregistered,
            &key("unregistered-apply")?,
            now()?
        )
        .await,
        Err(ConfigurationError::BundleRequired)
    ));
    assert_eq!(configuration::get(&pool).await?, state);
    assert!(matches!(
        configuration::get_bundle(&pool, desired.revision).await,
        Err(ConfigurationError::NotFound)
    ));
    let first_cipher = ciphertext()?;
    let second_cipher = ciphertext()?;
    let facts = |cipher: &[u8]| configuration::BundleCiphertext {
        digest: persistence_sqlx::Sha256Digest::of_bytes(cipher),
        bytes: cipher.len() as u64,
    };
    let request = RegisterPlatformConfigurationBundleRequest {
        expected_revision: state.revision,
        configuration_sha256: desired.configuration_sha256.clone(),
    };
    let mut wrong = request.clone();
    wrong.configuration_sha256 = "b".repeat(64);
    assert!(matches!(
        configuration::register_bundle(
            &pool,
            actor,
            desired.revision,
            &wrong,
            facts(&first_cipher),
            &key("wrong-settings-binding")?,
            now()?
        )
        .await,
        Err(ConfigurationError::RevisionConflict)
    ));
    assert!(matches!(
        configuration::register_bundle(
            &pool,
            actor,
            Revision::new(999)?,
            &request,
            facts(&first_cipher),
            &key("wrong-revision-binding")?,
            now()?
        )
        .await,
        Err(ConfigurationError::RevisionConflict)
    ));
    let first_key = key("bundle-concurrent-first")?;
    let second_key = key("bundle-concurrent-second")?;
    let time = now()?;
    let (first, second) = tokio::join!(
        configuration::register_bundle(
            &pool,
            actor,
            desired.revision,
            &request,
            facts(&first_cipher),
            &first_key,
            time
        ),
        configuration::register_bundle(
            &pool,
            actor,
            desired.revision,
            &request,
            facts(&second_cipher),
            &second_key,
            time
        ),
    );
    let (state, winning_key, winning_cipher, other_cipher) = match (first, second) {
        (Ok(state), Err(ConfigurationError::RevisionConflict)) => {
            (state, first_key, &first_cipher, &second_cipher)
        }
        (Err(ConfigurationError::RevisionConflict), Ok(state)) => {
            (state, second_key, &second_cipher, &first_cipher)
        }
        _ => return Err("unexpected registration race outcome".into()),
    };
    assert!(state.applied.is_none());
    assert!(state.latest_apply.is_none());
    let replay = configuration::register_bundle(
        &pool,
        actor,
        desired.revision,
        &request,
        facts(winning_cipher),
        &winning_key,
        time,
    )
    .await?;
    assert_eq!(replay, state);
    assert!(matches!(
        configuration::register_bundle(
            &pool,
            actor,
            desired.revision,
            &request,
            facts(other_cipher),
            &winning_key,
            time
        )
        .await,
        Err(ConfigurationError::IdempotencyConflict)
    ));
    let registered = configuration::get_bundle(&pool, desired.revision).await?;
    assert_eq!(
        registered.encrypted_bundle_sha256,
        persistence_sqlx::Sha256Digest::of_bytes(winning_cipher).to_string()
    );
    assert_eq!(registered.registered_by, actor);
    let apply = configuration::request_apply(
        &pool,
        actor,
        &ApplyPlatformConfigurationRequest {
            expected_revision: state.revision,
            desired_revision: desired.revision,
        },
        &key("registered-apply")?,
        time,
    )
    .await?;
    let old_digest = apply
        .latest_apply
        .as_ref()
        .ok_or("no apply")?
        .encrypted_bundle_sha256
        .clone();
    let state = configuration::register_bundle(
        &pool,
        actor,
        desired.revision,
        &RegisterPlatformConfigurationBundleRequest {
            expected_revision: apply.revision,
            configuration_sha256: desired.configuration_sha256.clone(),
        },
        facts(other_cipher),
        &key("register-during-apply")?,
        time,
    )
    .await?;
    assert_eq!(
        state
            .latest_apply
            .as_ref()
            .ok_or("no apply")?
            .encrypted_bundle_sha256,
        old_digest
    );
    assert_ne!(
        state
            .registered_bundle
            .as_ref()
            .ok_or("no registration")?
            .encrypted_bundle_sha256,
        old_digest
    );
    let state = edit(
        &pool,
        state.revision,
        desired.configuration,
        "new-settings-clear-registration",
    )
    .await?;
    assert!(state.registered_bundle.is_none());
    let mut state = state;
    for (index, stage) in [
        Stage::Validating,
        Stage::Applying,
        Stage::Verifying,
        Stage::Succeeded,
    ]
    .into_iter()
    .enumerate()
    {
        state = progress(
            &pool,
            &state,
            stage,
            &format!("registered-old-apply-{index}"),
        )
        .await?;
    }
    assert_eq!(state.applied_bundle_sha256, Some(old_digest));
    assert_eq!(
        state.applied.as_ref().ok_or("no applied")?.revision,
        desired.revision
    );
    let current = state.desired.as_ref().ok_or("no desired")?;
    assert_ne!(current.revision, desired.revision);
    assert!(matches!(
        configuration::get_bundle(&pool, current.revision).await,
        Err(ConfigurationError::NotFound)
    ));
    assert_eq!(
        configuration::get_bundle(&pool, desired.revision)
            .await?
            .encrypted_bundle_sha256,
        persistence_sqlx::Sha256Digest::of_bytes(other_cipher).to_string()
    );
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM control.platform_configuration_bundles")
            .fetch_one(&pool)
            .await?;
    assert_eq!(count, 2);
    Ok(())
}

/// Synthetic deployment-input boundary for state-machine tests. The separate loaded
/// integration target uses the actual age/Bundle producer rather than this receipt fixture.
fn synthetic_loaded(
    version: &contracts::platform_configuration::PlatformConfigurationVersion,
    encrypted_bundle_sha256: &str,
) -> Result<control_service::platform_configuration::LoadedConfiguration, Box<dyn std::error::Error>>
{
    let directory = tempfile::tempdir()?;
    let version_file = directory.path().join("version.json");
    let source_file = directory.path().join("source.json");
    std::fs::write(&version_file, serde_json::to_vec(version)?)?;
    std::fs::write(
        &source_file,
        serde_json::to_vec(&serde_json::json!({
            "apiVersion":"deploy.labweaver.io/loaded-source/v1",
            "desiredRevision":version.revision,
            "configurationSha256":version.configuration_sha256,
            "encryptedBundleSha256":encrypted_bundle_sha256,
            "installation":{"namespace":"synthetic-control", "releaseName":"synthetic"}
        }))?,
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for file in [&version_file, &source_file] {
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600))?;
        }
    }
    Ok(
        control_service::platform_configuration::LoadedConfiguration::load(
            &version_file,
            &source_file,
        )?,
    )
}

async fn progress(
    pool: &PgPool,
    state: &PlatformConfigurationStatus,
    phase: Stage,
    value: &str,
) -> TestResult<PlatformConfigurationStatus> {
    let apply = state.latest_apply.as_ref().ok_or("no attempt")?;
    let desired = configuration::get_version(pool, apply.desired_revision).await?;
    let loaded = synthetic_loaded(&desired, &apply.encrypted_bundle_sha256)?;
    Ok(configuration::report(
        pool,
        Some(&loaded),
        "synthetic-deployment-executor",
        apply.id,
        &ReportPlatformConfigurationApplyRequest {
            expected_revision: apply.revision,
            stage: phase,
            failure_reason: (phase == Stage::Failed).then_some(Failure::ExecutionFailed),
            observed_configuration_sha256: (phase == Stage::Succeeded)
                .then_some(desired.configuration_sha256),
            observed_encrypted_bundle_sha256: (phase == Stage::Succeeded)
                .then_some(apply.encrypted_bundle_sha256.clone()),
            control_replica_observations: None,
        },
        &key(value)?,
        now()?,
    )
    .await?)
}

async fn verified(
    pool: &PgPool,
    mut state: PlatformConfigurationStatus,
    prefix: &str,
) -> TestResult<PlatformConfigurationStatus> {
    state = begin(pool, &state, &format!("{prefix}-apply")).await?;
    for (i, stage) in [
        Stage::Validating,
        Stage::Applying,
        Stage::Verifying,
        Stage::Succeeded,
    ]
    .into_iter()
    .enumerate()
    {
        state = progress(pool, &state, stage, &format!("{prefix}-phase-{i}")).await?;
    }
    Ok(state)
}

#[tokio::test]
async fn concurrent_edits_cannot_replace_a_newer_revision_and_replay_is_exact() -> TestResult {
    let (_container, pool) = database().await?;
    assert_eq!(configuration::get(&pool).await?.revision, 0);
    let actor = ActorId::new();
    let first_key = key("concurrent-edit-first")?;
    let second_key = key("concurrent-edit-second")?;
    let request = UpdatePlatformConfigurationRequest {
        expected_revision: 0,
        configuration: PlatformConfigurationDesired::default(),
    };
    let (first, second) = tokio::join!(
        configuration::update(&pool, actor, &request, &first_key, now()?),
        configuration::update(&pool, actor, &request, &second_key, now()?),
    );
    let (winner, winning_key) = match (first, second) {
        (Ok(value), Err(ConfigurationError::RevisionConflict)) => (value, "concurrent-edit-first"),
        (Err(ConfigurationError::RevisionConflict), Ok(value)) => (value, "concurrent-edit-second"),
        other => return Err(format!("unexpected edit outcomes: {other:?}").into()),
    };
    assert_eq!(winner.revision, 1);
    assert!(winner.applied.is_none());
    let replay = configuration::update(&pool, actor, &request, &key(winning_key)?, now()?).await?;
    assert_eq!(winner, replay);
    let mut changed = request;
    changed.configuration.lifecycle.read_only_retention_seconds = 120;
    assert!(matches!(
        configuration::update(&pool, actor, &changed, &key(winning_key)?, now()?).await,
        Err(ConfigurationError::IdempotencyConflict)
    ));
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM control.platform_configuration_versions")
            .fetch_one(&pool)
            .await?;
    assert_eq!(count, 1);
    Ok(())
}

#[tokio::test]
async fn applying_requires_ordered_matching_observations_and_one_executor() -> TestResult {
    let (_container, pool) = database().await?;
    let state = edit(
        &pool,
        0,
        PlatformConfigurationDesired::default(),
        "observation-edit",
    )
    .await?;
    let state = begin(&pool, &state, "observation-apply").await?;
    let apply = state.latest_apply.as_ref().ok_or("no apply")?;
    assert_eq!(apply.stage, Stage::Requested);
    assert!(state.applied.is_none());
    let report = ReportPlatformConfigurationApplyRequest {
        expected_revision: apply.revision,
        stage: Stage::Succeeded,
        failure_reason: None,
        observed_configuration_sha256: Some("b".repeat(64)),
        observed_encrypted_bundle_sha256: Some("a".repeat(64)),
        control_replica_observations: None,
    };
    assert!(matches!(
        configuration::report(
            &pool,
            None,
            "executor-one",
            apply.id,
            &report,
            &key("out-of-order-report")?,
            now()?
        )
        .await,
        Err(ConfigurationError::ApplyConflict)
    ));
    let mut state = progress(&pool, &state, Stage::Validating, "observation-validate").await?;
    let apply = state.latest_apply.as_ref().ok_or("no apply")?;
    let wrong_owner = ReportPlatformConfigurationApplyRequest {
        expected_revision: apply.revision,
        stage: Stage::Applying,
        observed_configuration_sha256: None,
        observed_encrypted_bundle_sha256: None,
        control_replica_observations: None,
        failure_reason: None,
    };
    assert!(matches!(
        configuration::report(
            &pool,
            None,
            "different-executor",
            apply.id,
            &wrong_owner,
            &key("wrong-executor-report")?,
            now()?
        )
        .await,
        Err(ConfigurationError::ApplyConflict)
    ));
    state = progress(&pool, &state, Stage::Applying, "observation-applying").await?;
    state = progress(&pool, &state, Stage::Verifying, "observation-verifying").await?;
    let apply = state.latest_apply.as_ref().ok_or("no apply")?;
    let mut mismatched = report;
    mismatched.expected_revision = apply.revision;
    assert!(matches!(
        configuration::report(
            &pool,
            None,
            "synthetic-deployment-executor",
            apply.id,
            &mismatched,
            &key("mismatched-observation")?,
            now()?
        )
        .await,
        Err(ConfigurationError::ObservationMismatch)
    ));
    assert!(configuration::get(&pool).await?.applied.is_none());
    state = progress(&pool, &state, Stage::Succeeded, "observation-succeeded").await?;
    assert_eq!(
        state.applied.as_ref().ok_or("no applied")?.revision,
        Revision::new(1)?
    );
    assert_eq!(state.applied_at, Some(now()?));
    assert_eq!(
        state.applied_bundle_sha256.as_ref(),
        state
            .latest_apply
            .as_ref()
            .map(|apply| &apply.encrypted_bundle_sha256)
    );
    assert!(!state.impact.runtime_policy_changed);
    Ok(())
}

#[tokio::test]
async fn failed_apply_keeps_verified_version_and_history_and_allows_a_new_attempt() -> TestResult {
    let (_container, pool) = database().await?;
    let state = edit(
        &pool,
        0,
        PlatformConfigurationDesired::default(),
        "history-first-edit",
    )
    .await?;
    let state = verified(&pool, state, "history-first").await?;
    let initial = state.applied.clone();
    let successful_id = state.latest_apply.as_ref().ok_or("no attempt")?.id;
    let mut desired = PlatformConfigurationDesired::default();
    desired.lifecycle.read_only_retention_seconds = 120;
    let state = edit(&pool, state.revision, desired, "history-second-edit").await?;
    assert!(state.impact.lifecycle_policy_changed);
    assert!(!state.impact.builder_policy_changed);
    let state = begin(&pool, &state, "history-second-apply").await?;
    let state = progress(&pool, &state, Stage::Validating, "history-second-validate").await?;
    let state = progress(&pool, &state, Stage::Applying, "history-second-applying").await?;
    let state = progress(&pool, &state, Stage::Failed, "history-second-failed").await?;
    let failed_id = state.latest_apply.as_ref().ok_or("no failed attempt")?.id;
    assert_eq!(state.applied, initial);
    assert_eq!(
        state.applied_state,
        contracts::platform_configuration::PlatformConfigurationAppliedState::Unverified
    );
    assert_eq!(
        state
            .latest_apply
            .as_ref()
            .ok_or("no failed attempt")?
            .failed_stage,
        Some(Stage::Applying)
    );
    assert_eq!(
        state
            .latest_apply
            .as_ref()
            .ok_or("no failed attempt")?
            .failure_reason,
        Some(Failure::ExecutionFailed)
    );
    assert_eq!(
        state
            .latest_apply
            .as_ref()
            .ok_or("no failed attempt")?
            .next_step,
        Next::ReconcileExternalResult
    );
    let retry = begin(&pool, &state, "history-retry-apply").await?;
    assert_eq!(
        retry
            .latest_apply
            .as_ref()
            .ok_or("no retry")?
            .previous_apply_id,
        Some(failed_id)
    );
    assert_eq!(
        retry.latest_apply.as_ref().ok_or("no retry")?.next_step,
        Next::ReconcileExternalResult
    );
    assert_eq!(
        retry.applied_state,
        contracts::platform_configuration::PlatformConfigurationAppliedState::Unverified
    );
    assert_ne!(retry.latest_apply.as_ref().ok_or("no retry")?.id, failed_id);
    assert_eq!(
        configuration::get_apply(&pool, failed_id).await?.stage,
        Stage::Failed
    );
    assert_eq!(
        configuration::get_apply(&pool, successful_id).await?.stage,
        Stage::Succeeded
    );
    assert_eq!(
        configuration::get_version(&pool, Revision::new(1)?)
            .await?
            .configuration,
        PlatformConfigurationDesired::default()
    );
    assert!(matches!(
        configuration::get_apply(&pool, OperationId::new()).await,
        Err(ConfigurationError::NotFound)
    ));
    Ok(())
}

#[tokio::test]
async fn newer_desired_during_execution_does_not_change_the_applied_snapshot() -> TestResult {
    let (_container, pool) = database().await?;
    let state = edit(
        &pool,
        0,
        PlatformConfigurationDesired::default(),
        "inflight-first-edit",
    )
    .await?;
    let state = begin(&pool, &state, "inflight-apply").await?;
    let state = progress(&pool, &state, Stage::Validating, "inflight-validate").await?;
    let mut desired = PlatformConfigurationDesired::default();
    desired.lifecycle.detailed_log_retention_seconds = 60;
    let mut state = edit(&pool, state.revision, desired, "inflight-second-edit").await?;
    let current_desired = state.desired.as_ref().ok_or("no desired")?.revision;
    for (i, stage) in [Stage::Applying, Stage::Verifying, Stage::Succeeded]
        .into_iter()
        .enumerate()
    {
        state = progress(&pool, &state, stage, &format!("inflight-phase-{i}")).await?;
    }
    assert_eq!(
        state.applied.as_ref().ok_or("no applied")?.revision,
        Revision::new(1)?
    );
    assert_eq!(
        state.desired.as_ref().ok_or("no desired")?.revision,
        current_desired
    );
    assert!(state.impact.lifecycle_policy_changed);
    // Reconnecting through a new pool reads the same durable facts.
    let reconnect = PgPoolOptions::new()
        .connect_with(pool.connect_options().as_ref().clone())
        .await?;
    let persisted = configuration::get(&reconnect).await?;
    assert_eq!(persisted, state);
    Ok(())
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one HTTP matrix covers every unsupported policy field"
)]
async fn http_policy_unsupported_fields_are_rejected_with_field_detail() -> TestResult {
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    let (_container, pool) = database().await?;
    let fixture = http_fixture::HttpFixture::new(pool).await?;
    let gateway = fixture.token("labweaver-access", "access.control.forward")?;
    let session = contracts::BffSessionId::new();
    let request = |configuration: PlatformConfigurationDesired,
                   key: &str|
     -> Result<Request<Body>, Box<dyn std::error::Error>> {
        Ok(Request::builder()
            .method("PATCH")
            .uri("/api/v1/admin/platform-configuration")
            .header("Authorization", format!("Bearer {gateway}"))
            .header("x-labweaver-actor-id", fixture.admin.to_string())
            .header("x-labweaver-session-id", session.to_string())
            .header("Content-Type", "application/json")
            .header("If-Match", "\"rev-0\"")
            .header("Idempotency-Key", key)
            .body(Body::from(serde_json::to_vec(
                &UpdatePlatformConfigurationRequest {
                    expected_revision: 0,
                    configuration,
                },
            )?))?)
    };
    let mut uid = PlatformConfigurationDesired::default();
    uid.runtime_policy.default_container_uid = u32::MAX;
    let mut capabilities = PlatformConfigurationDesired::default();
    capabilities
        .runtime_policy
        .allowed_capabilities
        .insert(LinuxCapability::SysAdmin);
    let mut preauthorization = PlatformConfigurationDesired::default();
    preauthorization.runtime_policy.privileged_allowed = true;
    preauthorization
        .runtime_policy
        .privileged_execution_location = Some(PrivilegedExecutionLocation::DedicatedNodePool {
        node_selector: std::collections::BTreeMap::from([(
            "pool".to_owned(),
            "research".to_owned(),
        )]),
        taint_key: "labweaver.io/research".to_owned(),
        taint_value: "true".to_owned(),
    });
    preauthorization
        .runtime_policy
        .preauthorizations
        .push(RuntimePreauthorization {
            scope: RuntimePreauthorizationScope::Project {
                project_id: ProjectId::new(),
            },
            capabilities: std::collections::BTreeSet::new(),
            privileged: true,
        });
    let mut guest = PlatformConfigurationDesired::default();
    guest.runtime_policy.privileged_allowed = true;
    guest.runtime_policy.privileged_execution_location =
        Some(PrivilegedExecutionLocation::VirtualMachineGuest {
            provider_binding: "kubevirt-test".to_owned(),
        });
    let mut builder_capabilities = PlatformConfigurationDesired::default();
    builder_capabilities
        .builder_policy
        .capabilities
        .insert(LinuxCapability::NetAdmin);
    let mut escalation = PlatformConfigurationDesired::default();
    escalation.builder_policy.allow_privilege_escalation = true;
    let mut entitlements = PlatformConfigurationDesired::default();
    entitlements
        .builder_policy
        .entitlements
        .insert(BuilderEntitlement::NetworkHost);
    let cases = [
        ("runtimePolicy.defaultContainerUid", uid),
        ("runtimePolicy.allowedCapabilities", capabilities),
        ("runtimePolicy.preauthorizations", preauthorization),
        ("runtimePolicy.privilegedExecutionLocation", guest),
        ("builderPolicy.capabilities", builder_capabilities),
        ("builderPolicy.allowPrivilegeEscalation", escalation),
        ("builderPolicy.entitlements", entitlements),
    ];
    for (index, (field, configuration)) in cases.into_iter().enumerate() {
        let response = fixture
            .router
            .clone()
            .oneshot(request(configuration, &format!("policy-{index}"))?)
            .await?;
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{field}"
        );
        let problem: ProblemDetails =
            serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await?)?;
        assert_eq!(
            problem.diagnostic_code.as_str(),
            "LW_PLATFORM_CONFIGURATION_POLICY_UNSUPPORTED",
            "{field}: {}",
            problem.detail
        );
        assert!(
            problem.detail.contains(field),
            "{field}: {}",
            problem.detail
        );
    }
    let response = fixture
        .router
        .clone()
        .oneshot(request(
            PlatformConfigurationDesired::default(),
            "policy-valid",
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    Ok(())
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one HTTP journey checks the exact public and signed executor boundaries"
)]
async fn http_json_yaml_share_revision_and_only_signed_executor_can_report() -> TestResult {
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    let (_container, pool) = database().await?;
    let fixture = http_fixture::HttpFixture::new(pool.clone()).await?;
    let gateway = fixture.token("labweaver-access", "access.control.forward")?;
    let session = contracts::BffSessionId::new();
    let root = "/api/v1/admin/platform-configuration";
    let request = |method: &str,
                   path: &str,
                   actor: ActorId,
                   body: Vec<u8>,
                   content: &str,
                   revision: u64,
                   idempotency: &str|
     -> Result<Request<Body>, axum::http::Error> {
        Request::builder()
            .method(method)
            .uri(path)
            .header("Authorization", format!("Bearer {gateway}"))
            .header("x-labweaver-actor-id", actor.to_string())
            .header("x-labweaver-session-id", session.to_string())
            .header("Content-Type", content)
            .header("If-Match", format!("\"rev-{revision}\""))
            .header("Idempotency-Key", idempotency)
            .body(Body::from(body))
    };
    let denied = fixture
        .router
        .clone()
        .oneshot(request(
            "GET",
            root,
            fixture.other_actor,
            vec![],
            "application/json",
            0,
            "http-denied-request",
        )?)
        .await?;
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    let initial = fixture
        .router
        .clone()
        .oneshot(request(
            "GET",
            root,
            fixture.admin,
            vec![],
            "application/json",
            0,
            "http-initial-request",
        )?)
        .await?;
    assert_eq!(initial.status(), StatusCode::OK);
    assert_eq!(initial.headers()["ETag"], "\"rev-0\"");
    let update = UpdatePlatformConfigurationRequest {
        expected_revision: 0,
        configuration: PlatformConfigurationDesired::default(),
    };
    let response = fixture
        .router
        .clone()
        .oneshot(request(
            "PATCH",
            root,
            fixture.admin,
            serde_json::to_vec(&update)?,
            "application/json",
            0,
            "http-json-update",
        )?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let state: PlatformConfigurationStatus =
        serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await?)?;
    assert_eq!(state.revision, 1);
    assert!(state.applied.is_none());
    let replay = fixture
        .router
        .clone()
        .oneshot(request(
            "PATCH",
            root,
            fixture.admin,
            serde_json::to_vec(&update)?,
            "application/json",
            0,
            "http-json-update",
        )?)
        .await?;
    assert_eq!(replay.status(), StatusCode::OK);
    let replayed: PlatformConfigurationStatus =
        serde_json::from_slice(&to_bytes(replay.into_body(), 1024 * 1024).await?)?;
    assert_eq!(replayed.revision, 1);
    let stale_yaml = fixture
        .router
        .clone()
        .oneshot(request(
            "PATCH",
            root,
            fixture.admin,
            serde_yaml::to_string(&update)?.into_bytes(),
            "application/yaml",
            0,
            "http-stale-yaml",
        )?)
        .await?;
    assert_eq!(stale_yaml.status(), StatusCode::CONFLICT);
    let mut current = update;
    current.expected_revision = 1;
    current.configuration.lifecycle.preemption_grace_seconds = 90;
    let yaml = fixture
        .router
        .clone()
        .oneshot(request(
            "PATCH",
            root,
            fixture.admin,
            serde_yaml::to_string(&current)?.into_bytes(),
            "application/yaml",
            1,
            "http-current-yaml",
        )?)
        .await?;
    assert_eq!(yaml.status(), StatusCode::OK);
    let state: PlatformConfigurationStatus =
        serde_json::from_slice(&to_bytes(yaml.into_body(), 1024 * 1024).await?)?;
    assert_eq!(
        state
            .desired
            .as_ref()
            .ok_or("no desired")?
            .configuration
            .lifecycle
            .preemption_grace_seconds,
        90
    );
    // Bundle export reads this immutable version. Its import uses the existing
    // complete update request and the destination fence, never source apply facts.
    let source_revision = state.desired.as_ref().ok_or("no desired")?.revision;
    let exported = fixture
        .router
        .clone()
        .oneshot(request(
            "GET",
            &format!("{root}/versions/{}", source_revision.get()),
            fixture.admin,
            vec![],
            "application/json",
            state.revision,
            "http-export-version",
        )?)
        .await?;
    assert_eq!(exported.status(), StatusCode::OK);
    let exported: contracts::platform_configuration::PlatformConfigurationVersion =
        serde_json::from_slice(&to_bytes(exported.into_body(), 1024 * 1024).await?)?;
    let imported = UpdatePlatformConfigurationRequest {
        expected_revision: state.revision,
        configuration: exported.configuration.clone(),
    };
    let imported_response = fixture
        .router
        .clone()
        .oneshot(request(
            "PATCH",
            root,
            fixture.admin,
            serde_json::to_vec(&imported)?,
            "application/json",
            imported.expected_revision,
            "http-import-settings",
        )?)
        .await?;
    assert_eq!(imported_response.status(), StatusCode::OK);
    let state: PlatformConfigurationStatus =
        serde_json::from_slice(&to_bytes(imported_response.into_body(), 1024 * 1024).await?)?;
    let desired = state.desired.as_ref().ok_or("no imported desired")?;
    assert_eq!(desired.configuration, exported.configuration);
    assert_eq!(desired.configuration_sha256, exported.configuration_sha256);
    assert_ne!(desired.revision, source_revision);
    assert!(state.applied.is_none());
    assert!(state.latest_apply.is_none());
    let stale_import = fixture
        .router
        .clone()
        .oneshot(request(
            "PATCH",
            root,
            fixture.admin,
            serde_json::to_vec(&imported)?,
            "application/json",
            imported.expected_revision,
            "http-stale-import-settings",
        )?)
        .await?;
    assert_eq!(stale_import.status(), StatusCode::CONFLICT);
    let invalid = fixture
        .router
        .clone()
        .oneshot(request(
            "PATCH",
            root,
            fixture.admin,
            br#"{"expectedRevision":"synthetic-secret-do-not-echo","configuration":{}}"#.to_vec(),
            "application/json",
            2,
            "http-malformed-body",
        )?)
        .await?;
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    assert!(
        !String::from_utf8(to_bytes(invalid.into_body(), 1024 * 1024).await?.to_vec())?
            .contains("synthetic-secret-do-not-echo")
    );

    let desired = state.desired.as_ref().ok_or("no desired")?;
    let cipher = ciphertext()?;
    let registration_uri = format!(
        "{root}/versions/{}/bundle?expectedRevision={}&configurationSha256={}",
        desired.revision.get(),
        state.revision,
        desired.configuration_sha256
    );
    for (actor, body, expected_status, operation) in [
        (
            fixture.other_actor,
            cipher.clone(),
            StatusCode::FORBIDDEN,
            "http-register-non-admin",
        ),
        (
            fixture.admin,
            b"synthetic-secret-not-ciphertext".to_vec(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "http-register-plaintext",
        ),
    ] {
        let denied = fixture
            .router
            .clone()
            .oneshot(request(
                "POST",
                &registration_uri,
                actor,
                body,
                "application/octet-stream",
                state.revision,
                operation,
            )?)
            .await?;
        assert_eq!(denied.status(), expected_status);
        assert!(
            !String::from_utf8(to_bytes(denied.into_body(), 1024 * 1024).await?.to_vec())?
                .contains("synthetic-secret-not-ciphertext")
        );
    }
    let registered = fixture
        .router
        .clone()
        .oneshot(request(
            "POST",
            &format!(
                "{root}/versions/{}/bundle?expectedRevision={}&configurationSha256={}",
                desired.revision.get(),
                state.revision,
                desired.configuration_sha256
            ),
            fixture.admin,
            cipher.clone(),
            "application/octet-stream",
            state.revision,
            "http-register-bundle",
        )?)
        .await?;
    assert_eq!(registered.status(), StatusCode::OK);
    let state: PlatformConfigurationStatus =
        serde_json::from_slice(&to_bytes(registered.into_body(), 1024 * 1024).await?)?;
    assert_eq!(
        state
            .registered_bundle
            .as_ref()
            .ok_or("no registered bundle")?
            .encrypted_bundle_sha256,
        persistence_sqlx::Sha256Digest::of_bytes(&cipher).to_string()
    );
    let queried = fixture
        .router
        .clone()
        .oneshot(request(
            "GET",
            &format!("{root}/versions/{}/bundle", desired.revision.get()),
            fixture.admin,
            Vec::new(),
            "application/json",
            state.revision,
            "http-read-bundle",
        )?)
        .await?;
    assert_eq!(queried.status(), StatusCode::OK);
    let binding: contracts::platform_configuration::PlatformConfigurationBundle =
        serde_json::from_slice(&to_bytes(queried.into_body(), 1024 * 1024).await?)?;
    assert_eq!(
        binding,
        state.registered_bundle.clone().ok_or("no registration")?
    );
    let apply = ApplyPlatformConfigurationRequest {
        expected_revision: state.revision,
        desired_revision: state.desired.as_ref().ok_or("no desired")?.revision,
    };
    let accepted = fixture
        .router
        .clone()
        .oneshot(request(
            "POST",
            &format!("{root}/applies"),
            fixture.admin,
            serde_json::to_vec(&apply)?,
            "application/json",
            state.revision,
            "http-apply-request",
        )?)
        .await?;
    assert_eq!(accepted.status(), StatusCode::ACCEPTED);
    let state: PlatformConfigurationStatus =
        serde_json::from_slice(&to_bytes(accepted.into_body(), 1024 * 1024).await?)?;
    assert!(state.applied.is_none());
    let attempt = state.latest_apply.as_ref().ok_or("no attempt")?;
    let report = ReportPlatformConfigurationApplyRequest {
        expected_revision: attempt.revision,
        stage: Stage::Validating,
        failure_reason: None,
        observed_configuration_sha256: None,
        observed_encrypted_bundle_sha256: None,
        control_replica_observations: None,
    };
    let internal = format!("/internal/v1/platform-configuration/applies/{}", attempt.id);
    for (client, permission) in [
        ("labweaver-access", "access.control.forward"),
        ("synthetic-executor", "access.control.forward"),
        ("labweaver-access", "control.platform_configuration.apply"),
    ] {
        let token = fixture.token(client, permission)?;
        let report_request = Request::builder()
            .method("PUT")
            .uri(&internal)
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .header("If-Match", "\"rev-1\"")
            .header(
                "Idempotency-Key",
                format!("http-report-denied-{client}-{permission}"),
            )
            .body(Body::from(serde_json::to_vec(&report)?))?;
        let denied = fixture.router.clone().oneshot(report_request).await?;
        assert!(matches!(
            denied.status(),
            StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED
        ));
    }
    let token = fixture.token("synthetic-executor", "control.platform_configuration.apply")?;
    let accepted = fixture
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(&internal)
                .header("Authorization", format!("Bearer {token}"))
                .header("Content-Type", "application/json")
                .header("If-Match", "\"rev-1\"")
                .header("Idempotency-Key", "http-report-accepted")
                .body(Body::from(serde_json::to_vec(&report)?))?,
        )
        .await?;
    assert_eq!(accepted.status(), StatusCode::OK);
    let state: PlatformConfigurationStatus =
        serde_json::from_slice(&to_bytes(accepted.into_body(), 1024 * 1024).await?)?;
    assert_eq!(
        state.latest_apply.as_ref().ok_or("no attempt")?.stage,
        Stage::Validating
    );
    assert!(state.applied.is_none());
    let missing = fixture
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(&internal)
                .header("Content-Type", "application/json")
                .body(Body::from(serde_json::to_vec(&report)?))?,
        )
        .await?;
    assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
    Ok(())
}
