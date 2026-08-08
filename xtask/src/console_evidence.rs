use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use super::AppError;

const SCHEMA: &str = "schemas/results/connected-console-evidence.v1.schema.json";
const REQUIRED_CASES: [&str; 6] = [
    "positive",
    "revoke",
    "expiry",
    "stop",
    "delete",
    "control-channel-loss",
];

#[derive(Clone, Copy)]
pub(super) struct GateIdentity<'a> {
    pub source_commit: &'a str,
    pub run_id: uuid::Uuid,
    pub package_identity: &'a str,
    pub deployment_identity: &'a str,
    pub migration_catalog_sha256: &'a str,
    pub access_service_image: &'a str,
    pub environment_service_image: &'a str,
    pub runtime_artifact: &'a str,
    pub runtime_kind: &'a str,
    pub console_kind: &'a str,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ConnectedConsoleEvidence {
    schema_version: String,
    status: String,
    mode: String,
    provider_mode: String,
    source_commit: String,
    run_id: uuid::Uuid,
    package_identity: String,
    deployment_identity: String,
    migration_catalog_sha256: String,
    runtime_kind: String,
    console_kind: String,
    images: ImageIdentity,
    lifecycle_cases: Vec<LifecycleCase>,
    browser_evidence: BrowserEvidence,
    cleanup: CleanupEvidence,
    redaction: RedactionEvidence,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EnvironmentIdentity {
    id: uuid::Uuid,
    revision: u64,
    namespace: String,
    target_name: String,
    target_uid: uuid::Uuid,
    runtime_identity: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AccessIdentity {
    grant_id: uuid::Uuid,
    grant_revision: u64,
    lease_id: uuid::Uuid,
    lease_revision: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ImageIdentity {
    access_service: String,
    environment_service: String,
    runtime_artifact: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LifecycleCase {
    case: String,
    status: String,
    environment: EnvironmentIdentity,
    access: AccessIdentity,
    capability_id: uuid::Uuid,
    session_id: uuid::Uuid,
    duration_ms: u64,
    observed_diagnostic: String,
    stale_reconnect_denied: bool,
    audit_sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BrowserEvidence {
    browser: String,
    browser_version: String,
    viewport: Viewport,
    trace_sha256: String,
    screenshot_sha256: String,
    video_sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Viewport {
    width: u64,
    height: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CleanupEvidence {
    completed: bool,
    readback_completed: bool,
    temporary_fault_policy_removed: bool,
    pre_counts: ResourceCounts,
    post_counts: ResourceCounts,
    readback_sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ResourceCounts {
    capabilities: u64,
    sessions: u64,
    pods: u64,
    vmis: u64,
    pvcs: u64,
    fault_policies: u64,
}

impl ResourceCounts {
    const fn is_empty(&self) -> bool {
        self.capabilities == 0
            && self.sessions == 0
            && self.pods == 0
            && self.vmis == 0
            && self.pvcs == 0
            && self.fault_policies == 0
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "the report deliberately records each independent redaction audit result"
)]
struct RedactionEvidence {
    verified: bool,
    contains_secrets: bool,
    contains_raw_user_content: bool,
    contains_terminal_transcript: bool,
    contains_vnc_frames: bool,
    contains_absolute_paths: bool,
}

pub(super) fn validate_report(root: &Path, report: &Path) -> Result<(), AppError> {
    let (_, evidence) = read(root, report)?;
    validate_semantics(&evidence)
}

pub(super) fn validate_for_gate(
    root: &Path,
    report: &Path,
    expected: GateIdentity<'_>,
) -> Result<(), AppError> {
    let (_, evidence) = read(root, report)?;
    validate_semantics(&evidence)?;
    if evidence.source_commit != expected.source_commit
        || evidence.run_id != expected.run_id
        || evidence.package_identity != expected.package_identity
        || evidence.deployment_identity != expected.deployment_identity
        || evidence.migration_catalog_sha256 != expected.migration_catalog_sha256
        || evidence.images.access_service != expected.access_service_image
        || evidence.images.environment_service != expected.environment_service_image
        || evidence.images.runtime_artifact != expected.runtime_artifact
        || evidence.runtime_kind != expected.runtime_kind
        || evidence.console_kind != expected.console_kind
    {
        return Err(error(
            "LW_CONSOLE_EVIDENCE_IDENTITY_MISMATCH",
            "console evidence differs from the frozen Release Gate identity",
        ));
    }
    Ok(())
}

fn read(root: &Path, report: &Path) -> Result<(Value, ConnectedConsoleEvidence), AppError> {
    let report = secure_file(root, report)?;
    let bytes = fs::read(&report)
        .map_err(|cause| error("LW_CONSOLE_EVIDENCE_UNREADABLE", &cause.to_string()))?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|cause| error("LW_CONSOLE_EVIDENCE_SCHEMA_INVALID", &cause.to_string()))?;
    let schema: Value = serde_json::from_slice(
        &fs::read(root.join(SCHEMA))
            .map_err(|cause| error("LW_CONSOLE_EVIDENCE_SCHEMA_INVALID", &cause.to_string()))?,
    )
    .map_err(|cause| error("LW_CONSOLE_EVIDENCE_SCHEMA_INVALID", &cause.to_string()))?;
    let validator = jsonschema::validator_for(&schema)
        .map_err(|cause| error("LW_CONSOLE_EVIDENCE_SCHEMA_INVALID", &cause.to_string()))?;
    if let Some(cause) = validator.iter_errors(&value).next() {
        return Err(error(
            "LW_CONSOLE_EVIDENCE_SCHEMA_INVALID",
            &cause.to_string(),
        ));
    }
    if contains_forbidden_material(&value) {
        return Err(error(
            "LW_CONSOLE_EVIDENCE_REDACTION_FAILED",
            "console evidence contains a forbidden locator, credential, payload, or absolute path",
        ));
    }
    let evidence = serde_json::from_value(value.clone())
        .map_err(|cause| error("LW_CONSOLE_EVIDENCE_SCHEMA_INVALID", &cause.to_string()))?;
    Ok((value, evidence))
}

#[allow(
    clippy::too_many_lines,
    reason = "the console evidence boundary keeps all identity and lifecycle invariants visible"
)]
fn validate_semantics(evidence: &ConnectedConsoleEvidence) -> Result<(), AppError> {
    if evidence.schema_version != "connected-console-evidence.v1"
        || evidence.status != "passed"
        || evidence.mode != "connected"
        || evidence.provider_mode != "real"
    {
        return Err(error(
            "LW_CONSOLE_EVIDENCE_BOUNDARY_INVALID",
            "only passed connected evidence from a real provider is accepted",
        ));
    }
    let valid_pair = matches!(
        (
            evidence.runtime_kind.as_str(),
            evidence.console_kind.as_str()
        ),
        ("container", "xterm") | ("kubevirt", "novnc")
    );
    if !valid_pair {
        return Err(error(
            "LW_CONSOLE_EVIDENCE_RUNTIME_KIND_INVALID",
            "runtime and console kind are not an authoritative pair",
        ));
    }
    let actual_cases = evidence
        .lifecycle_cases
        .iter()
        .map(|item| item.case.as_str())
        .collect::<BTreeSet<_>>();
    if evidence.lifecycle_cases.len() != REQUIRED_CASES.len()
        || actual_cases != REQUIRED_CASES.into_iter().collect()
    {
        return Err(error(
            "LW_CONSOLE_EVIDENCE_CASE_SET_INVALID",
            "the exact positive, revoke, expiry, stop, delete, and control-channel-loss cases are required",
        ));
    }
    for case in &evidence.lifecycle_cases {
        let expected_diagnostic = match case.case.as_str() {
            "positive" => "LW_CONSOLE_SESSION_ACTIVE",
            "control-channel-loss" => "LW_CONSOLE_CONTROL_CHANNEL_LOST",
            "revoke" | "expiry" | "stop" | "delete" => "LW_CONSOLE_AUTHORIZATION_ENDED",
            _ => unreachable!("case set was validated"),
        };
        if case.status != "passed"
            || case.duration_ms > 60_000
            || !case.stale_reconnect_denied
            || case.observed_diagnostic != expected_diagnostic
            || case.access.grant_revision == 0
            || case.access.lease_revision == 0
            || case.environment.revision == 0
            || case.environment.id.is_nil()
            || case.environment.target_uid.is_nil()
            || case.access.grant_id.is_nil()
            || case.access.lease_id.is_nil()
            || case.capability_id.is_nil()
            || case.session_id.is_nil()
        {
            return Err(error(
                "LW_CONSOLE_EVIDENCE_CASE_FAILED",
                &format!(
                    "{} did not satisfy the connected fail-closed contract",
                    case.case
                ),
            ));
        }
    }
    let pre_target_present = match evidence.runtime_kind.as_str() {
        "container" => evidence.cleanup.pre_counts.pods > 0,
        "kubevirt" => evidence.cleanup.pre_counts.vmis > 0 && evidence.cleanup.pre_counts.pvcs > 0,
        _ => false,
    };
    if !evidence.cleanup.completed
        || !evidence.cleanup.readback_completed
        || !evidence.cleanup.temporary_fault_policy_removed
        || !pre_target_present
        || !evidence.cleanup.post_counts.is_empty()
    {
        return Err(error(
            "LW_CONSOLE_EVIDENCE_CLEANUP_INCOMPLETE",
            "console resources or the temporary control-channel fault policy remain after cleanup",
        ));
    }
    if !evidence.redaction.verified
        || evidence.redaction.contains_secrets
        || evidence.redaction.contains_raw_user_content
        || evidence.redaction.contains_terminal_transcript
        || evidence.redaction.contains_vnc_frames
        || evidence.redaction.contains_absolute_paths
    {
        return Err(error(
            "LW_CONSOLE_EVIDENCE_REDACTION_FAILED",
            "console evidence redaction is incomplete",
        ));
    }
    let identity_fields_present = evidence.lifecycle_cases.iter().all(|case| {
        !case.environment.id.is_nil()
            && case.environment.revision > 0
            && !case.environment.namespace.is_empty()
            && !case.environment.target_name.is_empty()
            && !case.environment.target_uid.is_nil()
            && !case.environment.runtime_identity.is_empty()
            && !case.access.grant_id.is_nil()
            && case.access.grant_revision > 0
            && !case.access.lease_id.is_nil()
            && case.access.lease_revision > 0
    }) && !evidence.browser_evidence.browser.is_empty()
        && !evidence.browser_evidence.browser_version.is_empty()
        && evidence.browser_evidence.viewport.width >= 320
        && evidence.browser_evidence.viewport.height >= 320;
    let all_hashes_present = evidence
        .lifecycle_cases
        .iter()
        .all(|case| digest(&case.audit_sha256))
        && [
            &evidence.browser_evidence.trace_sha256,
            &evidence.browser_evidence.screenshot_sha256,
            &evidence.browser_evidence.video_sha256,
            &evidence.cleanup.readback_sha256,
        ]
        .into_iter()
        .all(|value| digest(value));
    if !identity_fields_present || !all_hashes_present {
        return Err(error(
            "LW_CONSOLE_EVIDENCE_IDENTITY_MISSING",
            "console evidence identity or artifact hash is missing",
        ));
    }
    Ok(())
}

fn secure_file(root: &Path, report: &Path) -> Result<PathBuf, AppError> {
    if report.as_os_str().is_empty()
        || report.is_absolute()
        || report
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(error(
            "LW_CONSOLE_EVIDENCE_LOCATOR_INVALID",
            "report must be a repository-relative regular file",
        ));
    }
    let root = root
        .canonicalize()
        .map_err(|cause| error("LW_CONSOLE_EVIDENCE_LOCATOR_INVALID", &cause.to_string()))?;
    let report = root
        .join(report)
        .canonicalize()
        .map_err(|cause| error("LW_CONSOLE_EVIDENCE_LOCATOR_INVALID", &cause.to_string()))?;
    if !report.starts_with(&root)
        || !report.is_file()
        || fs::symlink_metadata(&report).is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(error(
            "LW_CONSOLE_EVIDENCE_LOCATOR_INVALID",
            "report must stay inside the repository and cannot be a symlink",
        ));
    }
    Ok(report)
}

fn contains_forbidden_material(value: &Value) -> bool {
    match value {
        Value::Array(items) => items.iter().any(contains_forbidden_material),
        Value::Object(items) => items.iter().any(|(key, value)| {
            matches!(
                key.as_str(),
                "connectionLocator"
                    | "cookie"
                    | "token"
                    | "password"
                    | "secret"
                    | "credential"
                    | "terminalTranscript"
                    | "vncFrame"
                    | "rawPayload"
            ) || contains_forbidden_material(value)
        }),
        Value::String(value) => {
            let lower = value.to_ascii_lowercase();
            value.starts_with('/')
                || value.starts_with('\\')
                || value.as_bytes().get(1) == Some(&b':')
                || lower.contains("bearer ")
                || lower.contains("set-cookie:")
                || lower.contains("begin private key")
                || lower.contains("/connect/console/")
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

fn digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

fn error(code: &'static str, detail: &str) -> AppError {
    AppError::ReleaseGate {
        code,
        detail: detail.to_owned(),
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "test fixtures use explicit panic messages for impossible malformed in-memory values"
)]
pub(crate) mod tests {
    use std::fs;
    use std::path::Path;

    use serde_json::{Value, json};
    use tempfile::tempdir;

    use super::{GateIdentity, validate_for_gate, validate_report};

    #[test]
    fn report_requires_real_same_identity_complete_lifecycle_and_redaction()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempdir()?;
        let root = temporary.path();
        fs::create_dir_all(root.join("schemas/results"))?;
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../schemas/results/connected-console-evidence.v1.schema.json"),
            root.join("schemas/results/connected-console-evidence.v1.schema.json"),
        )?;
        fs::create_dir_all(root.join("artifacts/evidence"))?;
        let report = valid_report("container", "xterm");
        fs::write(
            root.join("artifacts/evidence/console.json"),
            serde_json::to_vec_pretty(&report)?,
        )?;
        validate_report(root, Path::new("artifacts/evidence/console.json"))?;
        validate_for_gate(
            root,
            Path::new("artifacts/evidence/console.json"),
            GateIdentity {
                source_commit: "a000000000000000000000000000000000000000",
                run_id: uuid::Uuid::parse_str("01999999-9999-7999-8999-999999999999")?,
                package_identity: &hash('b'),
                deployment_identity: &hash('c'),
                migration_catalog_sha256: &hash('d'),
                access_service_image: &hash('e'),
                environment_service_image: &hash('f'),
                runtime_artifact: &hash('1'),
                runtime_kind: "container",
                console_kind: "xterm",
            },
        )?;

        let mut invalid = report;
        invalid["lifecycleCases"]
            .as_array_mut()
            .expect("array")
            .pop();
        fs::write(
            root.join("artifacts/evidence/console.json"),
            serde_json::to_vec_pretty(&invalid)?,
        )?;
        let error = validate_report(root, Path::new("artifacts/evidence/console.json"))
            .expect_err("incomplete lifecycle must fail");
        assert_eq!(
            error.diagnostic_code(),
            "LW_CONSOLE_EVIDENCE_SCHEMA_INVALID"
        );
        Ok(())
    }

    #[test]
    fn report_rejects_mock_identity_drift_cleanup_redaction_and_escaped_path()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempdir()?;
        let root = temporary.path();
        fs::create_dir_all(root.join("schemas/results"))?;
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../schemas/results/connected-console-evidence.v1.schema.json"),
            root.join("schemas/results/connected-console-evidence.v1.schema.json"),
        )?;
        fs::create_dir_all(root.join("artifacts/evidence"))?;
        let report_path = root.join("artifacts/evidence/console.json");
        for (pointer, value, expected) in [
            (
                "/providerMode",
                json!("mock"),
                "LW_CONSOLE_EVIDENCE_SCHEMA_INVALID",
            ),
            (
                "/cleanup/postCounts/pods",
                json!(1),
                "LW_CONSOLE_EVIDENCE_CLEANUP_INCOMPLETE",
            ),
            (
                "/redaction/containsSecrets",
                json!(true),
                "LW_CONSOLE_EVIDENCE_SCHEMA_INVALID",
            ),
            (
                "/lifecycleCases/0/observedDiagnostic",
                json!("LW_WRONG"),
                "LW_CONSOLE_EVIDENCE_CASE_FAILED",
            ),
        ] {
            let mut report = valid_report("container", "xterm");
            *report.pointer_mut(pointer).expect("fixture pointer") = value;
            fs::write(&report_path, serde_json::to_vec_pretty(&report)?)?;
            let error = validate_report(root, Path::new("artifacts/evidence/console.json"))
                .expect_err("invalid evidence must fail");
            assert_eq!(error.diagnostic_code(), expected);
        }

        let report = valid_report("container", "xterm");
        fs::write(&report_path, serde_json::to_vec_pretty(&report)?)?;
        let error = validate_for_gate(
            root,
            Path::new("artifacts/evidence/console.json"),
            GateIdentity {
                source_commit: "b000000000000000000000000000000000000000",
                run_id: uuid::Uuid::parse_str("01999999-9999-7999-8999-999999999999")?,
                package_identity: &hash('b'),
                deployment_identity: &hash('c'),
                migration_catalog_sha256: &hash('d'),
                access_service_image: &hash('e'),
                environment_service_image: &hash('f'),
                runtime_artifact: &hash('1'),
                runtime_kind: "container",
                console_kind: "xterm",
            },
        )
        .expect_err("cross-candidate evidence must fail");
        assert_eq!(
            error.diagnostic_code(),
            "LW_CONSOLE_EVIDENCE_IDENTITY_MISMATCH"
        );
        let error = validate_report(root, Path::new("../console.json"))
            .expect_err("escaped locator must fail");
        assert_eq!(
            error.diagnostic_code(),
            "LW_CONSOLE_EVIDENCE_LOCATOR_INVALID"
        );
        Ok(())
    }

    pub(crate) fn valid_report(runtime: &str, console: &str) -> Value {
        let (pods, vmis, pvcs, target_name) = if runtime == "container" {
            (6, 0, 0, "runtime-pod")
        } else {
            (0, 6, 6, "runtime")
        };
        let cases = [
            ("positive", "LW_CONSOLE_SESSION_ACTIVE"),
            ("revoke", "LW_CONSOLE_AUTHORIZATION_ENDED"),
            ("expiry", "LW_CONSOLE_AUTHORIZATION_ENDED"),
            ("stop", "LW_CONSOLE_AUTHORIZATION_ENDED"),
            ("delete", "LW_CONSOLE_AUTHORIZATION_ENDED"),
            ("control-channel-loss", "LW_CONSOLE_CONTROL_CHANNEL_LOST"),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (case, diagnostic))| json!({
            "case": case,
            "status": "passed",
            "environment": {"id":format!("01999999-9999-7999-8999-9999999997{index:02}"),"revision":index+1,"namespace":"labweaver-run","targetName":target_name,"targetUid":format!("01999999-9999-7999-8999-9999999996{index:02}"),"runtimeIdentity":"release-runtime-identity"},
            "access": {"grantId":format!("01999999-9999-7999-8999-9999999995{index:02}"),"grantRevision":index+1,"leaseId":format!("01999999-9999-7999-8999-9999999994{index:02}"),"leaseRevision":index+1},
            "capabilityId": format!("01999999-9999-7999-8999-9999999999{index:02}"),
            "sessionId": format!("01999999-9999-7999-8999-9999999998{index:02}"),
            "durationMs": 1000,
            "observedDiagnostic": diagnostic,
            "staleReconnectDenied": true,
            "auditSha256": hash('a')
        }))
        .collect::<Vec<_>>();
        json!({
            "schemaVersion": "connected-console-evidence.v1",
            "status": "passed",
            "mode": "connected",
            "providerMode": "real",
            "sourceCommit": "a000000000000000000000000000000000000000",
            "runId": "01999999-9999-7999-8999-999999999999",
            "packageIdentity": hash('b'),
            "deploymentIdentity": hash('c'),
            "migrationCatalogSha256": hash('d'),
            "runtimeKind": runtime,
            "consoleKind": console,
            "images": {"accessService":hash('e'),"environmentService":hash('f'),"runtimeArtifact":hash('1')},
            "lifecycleCases": cases,
            "browserEvidence": {"browser":"chromium","browserVersion":"1.61.1","viewport":{"width":1440,"height":900},"traceSha256":hash('2'),"screenshotSha256":hash('3'),"videoSha256":hash('4')},
            "cleanup": {"completed":true,"readbackCompleted":true,"temporaryFaultPolicyRemoved":true,"preCounts":{"capabilities":6,"sessions":6,"pods":pods,"vmis":vmis,"pvcs":pvcs,"faultPolicies":1},"postCounts":{"capabilities":0,"sessions":0,"pods":0,"vmis":0,"pvcs":0,"faultPolicies":0},"readbackSha256":hash('5')},
            "redaction": {"verified":true,"containsSecrets":false,"containsRawUserContent":false,"containsTerminalTranscript":false,"containsVncFrames":false,"containsAbsolutePaths":false}
        })
    }

    pub(crate) fn hash(character: char) -> String {
        format!("sha256:{}", character.to_string().repeat(64))
    }
}
