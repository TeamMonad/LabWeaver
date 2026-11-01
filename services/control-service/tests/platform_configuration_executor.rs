//! Production Python executor reports through real Control HTTPS and PostgreSQL.
use axum::{body::Body, http::{Request, StatusCode}};
use contracts::http::IdempotencyKey;
use contracts::platform_configuration::{
    ApplyPlatformConfigurationRequest, PlatformConfigurationAppliedState as AppliedState,
    PlatformConfigurationApplyStage as Stage, PlatformConfigurationDesired,
    PlatformConfigurationFailureReason as Failure, RegisterPlatformConfigurationBundleRequest,
    UpdatePlatformConfigurationRequest,
};
use contracts::{ActorId, UtcTimestamp};
use control_service::platform_configuration::{self as configuration, BundleCiphertext, LoadedConfiguration};
use persistence_sqlx::Sha256Digest;
use sqlx::postgres::PgPoolOptions;
use testcontainers::{ImageExt, runners::AsyncRunner};
use testcontainers_modules::postgres::Postgres;
use tower::ServiceExt;

#[allow(dead_code, reason = "shared configuration dependencies serve several integration targets")]
#[path = "support/platform_configuration_common.rs"]
mod configuration_common;
#[allow(dead_code, reason = "shared HTTP fixture includes browser identities")]
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[expect(clippy::too_many_lines, reason = "one real bundle and PG lifecycle tests success and two failure reports")]
async fn production_executor_verifies_real_control_and_fails_closed() -> TestResult {
    let container = Postgres::default().with_tag("17.6-alpine").start().await?;
    let pool = PgPoolOptions::new().max_connections(8).connect(&format!(
        "postgres://postgres:postgres@127.0.0.1:{}/postgres",
        container.get_host_port_ipv4(5432).await?
    )).await?;
    support::apply_domain_migrations(&pool, persistence_sqlx::Domain::Control).await?;
    let actor = ActorId::new();
    let state = configuration::update(&pool, actor, &UpdatePlatformConfigurationRequest {
        expected_revision: 0, configuration: PlatformConfigurationDesired::default(),
    }, &IdempotencyKey::parse("executor-first-desired")?, now()?).await?;
    let temporary = tempfile::tempdir()?;
    let root = temporary.path().join("bundle");
    let (cipher, _) = configuration_common::actual_loaded_bundle(
        state.desired.as_ref().ok_or("missing desired")?, &root,
    )?;
    let pod_uid = uuid::Uuid::now_v7().to_string();
    let loaded = LoadedConfiguration::load(
        &root.join(".private/loaded/platform-configuration.json"),
        &root.join(".private/loaded/platform-configuration-source.json"),
    )?.on_replica("control-a", "labweaver-system", &pod_uid)?;
    let fixture = http_fixture::HttpFixture::with_loaded(pool.clone(), Some(loaded)).await?;
    let (server, ca_file) = fixture.serve_executor().await?;
    let source: serde_json::Value = serde_json::from_slice(&std::fs::read(
        root.join(".private/loaded/platform-configuration-source.json"),
    )?)?;
    let deployment_uid = uuid::Uuid::now_v7().to_string();
    let rs_uid = uuid::Uuid::now_v7().to_string();
    let roster = serde_json::json!([
        {"metadata":{"name":"control-service","namespace":"labweaver-system","uid":deployment_uid,"generation":1},
         "spec":{"replicas":1,"selector":{"matchLabels":{"app":"control-service"}}},"status":{"observedGeneration":1}},
        {"items":[{"metadata":{"namespace":"labweaver-system","uid":rs_uid,"ownerReferences":[{"controller":true,"kind":"Deployment","uid":deployment_uid}]}}]},
        {"items":[{"metadata":{"name":"control-a","namespace":"labweaver-system","uid":pod_uid,"labels":{"app":"control-service"},"ownerReferences":[{"controller":true,"kind":"ReplicaSet","uid":rs_uid}]},
         "status":{"phase":"Running","podIP":"127.0.0.1","conditions":[{"type":"Ready","status":"True"}],"containerStatuses":[{"restartCount":0}]}}]}
    ]);
    let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for case in ["success", "mismatch", "unreachable"] {
        let state = configuration::get(&pool).await?;
        let desired = state.desired.as_ref().ok_or("missing desired")?;
        let registered = configuration::register_bundle(&pool, actor, desired.revision,
            &RegisterPlatformConfigurationBundleRequest { expected_revision: state.revision,
                configuration_sha256: desired.configuration_sha256.clone() },
            BundleCiphertext { digest: Sha256Digest::of_bytes(&cipher), bytes: cipher.len().try_into()? },
            &IdempotencyKey::parse(&format!("executor-register-{case}"))?, now()?).await?;
        let pending = configuration::request_apply(&pool, actor, &ApplyPlatformConfigurationRequest {
            expected_revision: registered.revision, desired_revision: desired.revision,
        }, &IdempotencyKey::parse(&format!("executor-apply-{case}"))?, now()?).await?;
        if case == "success" {
            let attempt_id = pending.latest_apply.as_ref().ok_or("missing pending attempt")?.id;
            for (client, permission, path, expected) in [
                ("labweaver-access", "control.platform_configuration.apply", format!("/internal/v1/platform-configuration/applies/{attempt_id}"), StatusCode::FORBIDDEN),
                ("labweaver-deployment-executor", "access.control.forward", format!("/internal/v1/platform-configuration/applies/{attempt_id}"), StatusCode::FORBIDDEN),
                ("labweaver-deployment-executor", "control.platform_configuration.apply", "/api/v1/admin/platform-configuration".to_owned(), StatusCode::FORBIDDEN),
                ("labweaver-deployment-executor", "control.platform_configuration.apply", format!("/internal/v1/platform-configuration/applies/{}", contracts::OperationId::new()), StatusCode::CONFLICT),
            ] {
                let response = fixture.router.clone().oneshot(Request::builder().uri(path)
                    .header("Authorization", format!("Bearer {}", fixture.token(client, permission)?))
                    .body(Body::empty())?).await?;
                assert_eq!(response.status(), expected);
            }
        }
        let input = temporary.path().join(format!("{case}.json"));
        std::fs::write(&input, serde_json::to_vec(&serde_json::json!({
            "source":source,"endpoint":server.base_url,"caFile":ca_file,"roster":roster,
            "applyId":pending.latest_apply.as_ref().ok_or("missing pending attempt")?.id,
            "token":fixture.token("labweaver-deployment-executor", "control.platform_configuration.apply")?,"case":case,
        }))?)?;
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&input, std::fs::Permissions::from_mode(0o600))?;
        }
        let output = tokio::process::Command::new("python3")
            .arg(repository.join("tests/ansible/run_verified_apply_control.py"))
            .arg(input).kill_on_drop(true).output().await?;
        assert!(output.status.success(), "real executor driver failed: {}", String::from_utf8_lossy(&output.stderr));
        let state = configuration::get(&pool).await?;
        let attempt = state.latest_apply.as_ref().ok_or("missing attempt")?;
        assert_eq!(attempt.executor_client_id.as_deref(), Some("labweaver-deployment-executor"));
        assert_eq!(attempt.revision.get(), 5);
        if case == "success" {
            assert!(String::from_utf8(output.stdout)?.contains("VERIFIED_APPLY_CONTROL_OK"));
            assert_eq!(attempt.stage, Stage::Succeeded);
            assert_eq!(state.applied_state, AppliedState::Verified);
        } else {
            assert!(String::from_utf8(output.stdout)?.contains("VERIFIED_APPLY_FAIL_CLOSED"));
            assert_eq!(attempt.stage, Stage::Failed);
            assert_eq!(attempt.failed_stage, Some(Stage::Verifying));
            assert_eq!(attempt.failure_reason, Some(Failure::VerificationFailed));
            assert_eq!(state.applied_state, AppliedState::Unverified);
            assert_eq!(state.applied.as_ref(), state.desired.as_ref());
        }
    }
    Ok(())
}
