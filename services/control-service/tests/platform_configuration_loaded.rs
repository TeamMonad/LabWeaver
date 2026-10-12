//! Loaded process settings and the original apply observer share the actual Bundle source.

use std::path::Path;

use contracts::http::IdempotencyKey;
use contracts::platform_configuration::{
    ApplyPlatformConfigurationRequest, PlatformConfigurationApplyStage as Stage,
    PlatformConfigurationDesired, PlatformConfigurationFailureReason as Failure,
    PlatformConfigurationStatus, PlatformConfigurationVersion,
    RegisterPlatformConfigurationBundleRequest, ReportPlatformConfigurationApplyRequest,
    UpdatePlatformConfigurationRequest,
};
use contracts::{ActorId, Revision, UtcTimestamp};
use control_service::platform_configuration::{
    self as configuration, BundleCiphertext, ConfigurationError, LoadedConfiguration,
};
use persistence_sqlx::Sha256Digest;
use sqlx::{PgPool, postgres::PgPoolOptions};
use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;

#[allow(
    dead_code,
    reason = "this shared fixture also defines dependencies for other configuration test targets"
)]
#[path = "support/platform_configuration_common.rs"]
mod configuration_common;
#[allow(
    dead_code,
    reason = "the shared HTTP fixture also exposes browser administrator identities"
)]
#[path = "support/platform_configuration_http.rs"]
mod http_fixture;
mod support;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn write(path: &Path, payload: &[u8]) -> TestResult {
    std::fs::write(path, payload)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn version() -> TestResult<PlatformConfigurationVersion> {
    let configuration = PlatformConfigurationDesired::default();
    Ok(PlatformConfigurationVersion {
        revision: Revision::new(1)?,
        configuration_sha256: Sha256Digest::of_canonical(&configuration)?.to_string(),
        configuration,
        created_by: ActorId::new(),
        created_at: now()?,
    })
}

fn now() -> TestResult<UtcTimestamp> {
    Ok("2026-10-12T16:00:00.000Z".parse()?)
}
fn key(value: &str) -> TestResult<IdempotencyKey> {
    Ok(IdempotencyKey::parse(value)?)
}

#[test]
fn loaded_files_reject_wrong_binding_unknown_fields_and_secret_diagnostics() -> TestResult {
    let temporary = tempfile::tempdir()?;
    let version_file = temporary.path().join("version.json");
    let source_file = temporary.path().join("source.json");
    let version = version()?;
    write(&version_file, &serde_json::to_vec(&version)?)?;
    let source = serde_json::json!({
        "apiVersion":"deploy.labweaver.io/loaded-source/v1",
        "desiredRevision":1,"configurationSha256":version.configuration_sha256,
        "encryptedBundleSha256":"a".repeat(64),
        "installation":{"namespace":"synthetic-control", "releaseName":"synthetic"}
    });
    write(&source_file, &serde_json::to_vec(&source)?)?;
    let loaded = LoadedConfiguration::load(&version_file, &source_file)?;
    assert!(!format!("{loaded:?}").contains("runtimePolicy"));
    for (field, value) in [
        ("desiredRevision", serde_json::json!(2)),
        ("configurationSha256", serde_json::json!("b".repeat(64))),
        ("encryptedBundleSha256", serde_json::json!("copied-report")),
        ("apiVersion", serde_json::json!("wrong")),
        ("token", serde_json::json!("synthetic-secret-never-log")),
        (
            "installation",
            serde_json::json!({"namespace":"https://elsewhere", "releaseName":"synthetic"}),
        ),
    ] {
        let mut changed = source.clone();
        changed[field] = value;
        write(&source_file, &serde_json::to_vec(&changed)?)?;
        let error = LoadedConfiguration::load(&version_file, &source_file)
            .err()
            .ok_or("invalid source was loaded")?;
        assert!(!error.to_string().contains("synthetic-secret"));
    }
    write(&source_file, &serde_json::to_vec(&source)?)?;
    let mut changed = serde_json::to_value(&version)?;
    changed["configuration"]["lifecycle"]["preemptionGraceSeconds"] = serde_json::json!(90);
    write(&version_file, &serde_json::to_vec(&changed)?)?;
    assert!(LoadedConfiguration::load(&version_file, &source_file).is_err());
    write(&version_file, br#"{"revision":1,"revision":2}"#)?;
    assert!(LoadedConfiguration::load(&version_file, &source_file).is_err());
    Ok(())
}

#[cfg(unix)]
#[test]
fn loaded_files_reject_permissions_missing_files_and_symlinks() -> TestResult {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let temporary = tempfile::tempdir()?;
    let version_file = temporary.path().join("version.json");
    let source_file = temporary.path().join("source.json");
    write(&version_file, &serde_json::to_vec(&version()?)?)?;
    write(&source_file, b"{}")?;
    std::fs::set_permissions(&version_file, std::fs::Permissions::from_mode(0o644))?;
    assert!(LoadedConfiguration::load(&version_file, &source_file).is_err());
    std::fs::set_permissions(&version_file, std::fs::Permissions::from_mode(0o600))?;
    let link = temporary.path().join("link.json");
    symlink(&version_file, &link)?;
    assert!(LoadedConfiguration::load(&link, &source_file).is_err());
    assert!(LoadedConfiguration::load(&temporary.path().join("missing"), &source_file).is_err());
    write(&version_file, &vec![b' '; 1_048_577])?;
    assert!(LoadedConfiguration::load(&version_file, &source_file).is_err());
    Ok(())
}

async fn begin(
    pool: &PgPool,
    state: PlatformConfigurationStatus,
    ciphertext: &[u8],
    name: &str,
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
            digest: Sha256Digest::of_bytes(ciphertext),
            bytes: ciphertext.len().try_into()?,
        },
        &key(&format!("{name}-register"))?,
        now()?,
    )
    .await?;
    Ok(configuration::request_apply(
        pool,
        ActorId::new(),
        &ApplyPlatformConfigurationRequest {
            expected_revision: state.revision,
            desired_revision: state.desired.as_ref().ok_or("no desired")?.revision,
        },
        &key(&format!("{name}-apply"))?,
        now()?,
    )
    .await?)
}

fn report(
    state: &PlatformConfigurationStatus,
    phase: Stage,
) -> TestResult<ReportPlatformConfigurationApplyRequest> {
    let apply = state.latest_apply.as_ref().ok_or("no apply")?;
    let observed_configuration_sha256 = if phase == Stage::Succeeded {
        Some(
            state
                .desired
                .as_ref()
                .ok_or("no desired")?
                .configuration_sha256
                .clone(),
        )
    } else {
        None
    };
    Ok(ReportPlatformConfigurationApplyRequest {
        expected_revision: apply.revision,
        stage: phase,
        failure_reason: (phase == Stage::Failed).then_some(Failure::VerificationFailed),
        observed_configuration_sha256,
        observed_encrypted_bundle_sha256: (phase == Stage::Succeeded)
            .then(|| apply.encrypted_bundle_sha256.clone()),
    })
}

async fn progress(
    pool: &PgPool,
    mut state: PlatformConfigurationStatus,
    name: &str,
) -> TestResult<PlatformConfigurationStatus> {
    for stage in [Stage::Validating, Stage::Applying, Stage::Verifying] {
        state = configuration::report(
            pool,
            None,
            "synthetic-executor",
            state.latest_apply.as_ref().ok_or("no apply")?.id,
            &report(&state, stage)?,
            &key(&format!("{name}-{stage:?}"))?,
            now()?,
        )
        .await?;
    }
    Ok(state)
}

/// Executed explicitly in CI with age, Python/PyYAML, Helm and the built Rust validator.
#[tokio::test]
#[ignore = "requires the actual Bundle producer toolchain"]
#[expect(
    clippy::too_many_lines,
    reason = "one actual age source and database lifecycle must retain the prior effective snapshot across failure and retry"
)]
async fn actual_bundle_load_is_required_for_success_and_failure_keeps_effective() -> TestResult {
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;
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
        &key("loaded-first-edit")?,
        now()?,
    )
    .await?;
    let temporary = tempfile::tempdir()?;
    let (cipher, loaded) = configuration_common::actual_loaded_bundle(
        initial.desired.as_ref().ok_or("no desired")?,
        &temporary.path().join("first"),
    )?;
    let state = begin(&pool, initial, &cipher, "first").await?;
    let state = progress(&pool, state, "first").await?;
    let success = report(&state, Stage::Succeeded)?;
    let id = state.latest_apply.as_ref().ok_or("no apply")?.id;
    assert!(matches!(
        configuration::report(
            &pool,
            None,
            "synthetic-executor",
            id,
            &success,
            &key("copied-unloaded-success")?,
            now()?
        )
        .await,
        Err(ConfigurationError::ObservationMismatch)
    ));
    assert_eq!(configuration::get(&pool).await?, state);

    let (_other_cipher, wrong_source) = configuration_common::actual_loaded_bundle(
        state.desired.as_ref().ok_or("no desired")?,
        &temporary.path().join("other-cipher"),
    )?;
    assert!(matches!(
        configuration::report(
            &pool,
            Some(&wrong_source),
            "synthetic-executor",
            id,
            &success,
            &key("other-source-success")?,
            now()?
        )
        .await,
        Err(ConfigurationError::ObservationMismatch)
    ));
    let source_root = temporary.path().join("first/.private/loaded");
    let prior_process = LoadedConfiguration::load(
        &source_root.join("platform-configuration.json"),
        &source_root.join("platform-configuration-source.json"),
    )?;
    let fixture = http_fixture::HttpFixture::with_loaded(pool.clone(), Some(loaded)).await?;
    // Replacing the mounted input is not reloading this process. Its observer must
    // continue to read the snapshot used at construction, rather than the new file.
    write(
        &source_root.join("platform-configuration.json"),
        b"invalid-replacement-never-loaded",
    )?;
    assert!(
        LoadedConfiguration::load(
            &source_root.join("platform-configuration.json"),
            &source_root.join("platform-configuration-source.json")
        )
        .is_err()
    );
    let token = fixture.token("synthetic-executor", "control.platform_configuration.apply")?;
    let accepted = fixture
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/internal/v1/platform-configuration/applies/{id}"))
                .header("Authorization", format!("Bearer {token}"))
                .header("Content-Type", "application/json")
                .header(
                    "If-Match",
                    format!("\"rev-{}\"", success.expected_revision.get()),
                )
                .header("Idempotency-Key", "actual-loaded-success")
                .body(Body::from(serde_json::to_vec(&success)?))?,
        )
        .await?;
    assert_eq!(accepted.status(), StatusCode::OK);
    let mut state: PlatformConfigurationStatus =
        serde_json::from_slice(&to_bytes(accepted.into_body(), 1_048_576).await?)?;
    let effective = state.applied.clone().ok_or("not applied")?;
    let effective_bundle = state.applied_bundle_sha256.clone();
    let replay = configuration::report(
        &pool,
        None,
        "synthetic-executor",
        id,
        &success,
        &key("actual-loaded-success")?,
        now()?,
    )
    .await?;
    assert_eq!(replay, state);
    assert_eq!(
        effective.configuration_sha256,
        Sha256Digest::of_canonical(&effective.configuration)?.to_string()
    );

    let mut next = effective.configuration.clone();
    next.runtime_policy.default_container_uid = 1000;
    state = configuration::update(
        &pool,
        ActorId::new(),
        &UpdatePlatformConfigurationRequest {
            expected_revision: state.revision,
            configuration: next,
        },
        &key("loaded-next-edit")?,
        now()?,
    )
    .await?;
    assert_eq!(state.applied.as_ref(), Some(&effective));
    let (next_cipher, next_loaded) = configuration_common::actual_loaded_bundle(
        state.desired.as_ref().ok_or("no desired")?,
        &temporary.path().join("second"),
    )?;
    state = begin(&pool, state, &next_cipher, "second").await?;
    state = progress(&pool, state, "second").await?;
    let id = state.latest_apply.as_ref().ok_or("no apply")?.id;
    assert!(matches!(
        configuration::report(
            &pool,
            Some(&prior_process),
            "synthetic-executor",
            id,
            &report(&state, Stage::Succeeded)?,
            &key("old-process-success")?,
            now()?
        )
        .await,
        Err(ConfigurationError::ObservationMismatch)
    ));
    state = configuration::report(
        &pool,
        Some(&prior_process),
        "synthetic-executor",
        id,
        &report(&state, Stage::Failed)?,
        &key("loaded-failed")?,
        now()?,
    )
    .await?;
    assert_eq!(state.applied.as_ref(), Some(&effective));
    assert_eq!(state.applied_bundle_sha256, effective_bundle);
    assert_eq!(
        state.applied_state,
        contracts::platform_configuration::PlatformConfigurationAppliedState::Unverified
    );
    let failed_id = id;
    state = begin(&pool, state, &next_cipher, "retry").await?;
    state = progress(&pool, state, "retry").await?;
    let id = state.latest_apply.as_ref().ok_or("no retry")?.id;
    state = configuration::report(
        &pool,
        Some(&next_loaded),
        "synthetic-executor",
        id,
        &report(&state, Stage::Succeeded)?,
        &key("loaded-retry-success")?,
        now()?,
    )
    .await?;
    assert_ne!(state.applied.as_ref(), Some(&effective));
    assert_eq!(state.applied, state.desired);
    assert_eq!(
        configuration::get_apply(&pool, failed_id).await?.stage,
        Stage::Failed
    );
    Ok(())
}
