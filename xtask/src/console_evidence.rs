use std::fs;
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use super::AppError;

const SCHEMA: &str = "schemas/results/connected-console-evidence.v1.schema.json";

#[derive(Clone, Copy)]
pub(super) struct GateIdentity<'a> {
    pub source_commit: &'a str,
    pub run_id: uuid::Uuid,
}

pub(super) fn validate_report(root: &Path, report: &Path) -> Result<(), AppError> {
    let value = read_and_validate_schema(root, report)?;
    validate_key_identity(&value)?;
    Ok(())
}

pub(super) fn validate_for_gate(
    root: &Path,
    report: &Path,
    expected: GateIdentity<'_>,
) -> Result<(), AppError> {
    let value = read_and_validate_schema(root, report)?;
    validate_key_identity(&value)?;
    let source_commit = value
        .get("sourceCommit")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let run_id = value
        .get("runId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if source_commit != expected.source_commit || run_id != expected.run_id.to_string() {
        return Err(error(
            "LW_CONSOLE_EVIDENCE_IDENTITY_MISMATCH",
            "console evidence differs from the frozen Release Gate identity",
        ));
    }
    Ok(())
}

fn read_and_validate_schema(root: &Path, report: &Path) -> Result<Value, AppError> {
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
    Ok(value)
}

fn validate_key_identity(value: &Value) -> Result<(), AppError> {
    let source = value
        .get("sourceCommit")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let run = value
        .get("runId")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let package = value
        .get("packageIdentity")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if source.is_empty() || run.is_empty() || package.is_empty() {
        return Err(error(
            "LW_CONSOLE_EVIDENCE_IDENTITY_MISSING",
            "console evidence key identity is missing",
        ));
    }
    if uuid::Uuid::parse_str(run).is_err() {
        return Err(error(
            "LW_CONSOLE_EVIDENCE_IDENTITY_MISSING",
            "runId is not a valid UUID",
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
    if let Err(cause) = contracts::foundation::validate_relative_path(&report.to_string_lossy()) {
        return Err(error(
            "LW_CONSOLE_EVIDENCE_LOCATOR_INVALID",
            &cause.to_string(),
        ));
    }
    let path = root.join(report);
    if !path.is_file() {
        return Err(error(
            "LW_CONSOLE_EVIDENCE_LOCATOR_INVALID",
            "report must stay inside the repository and cannot be a symlink",
        ));
    }
    Ok(path)
}

fn error(code: &'static str, detail: &str) -> AppError {
    AppError::ReleaseGate {
        code,
        detail: detail.to_owned(),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::fs;
    use std::path::Path;

    use serde_json::{Value, json};
    use tempfile::tempdir;

    use super::{GateIdentity, validate_for_gate, validate_report};

    #[test]
    fn report_requires_schema_and_key_identity() -> Result<(), Box<dyn std::error::Error>> {
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
            },
        )?;
        // missing identity fails (schema catches empty digest)
        let mut invalid = report.clone();
        invalid["packageIdentity"] = Value::String(String::new());
        fs::write(
            root.join("artifacts/evidence/console.json"),
            serde_json::to_vec_pretty(&invalid)?,
        )?;
        let error = validate_report(root, Path::new("artifacts/evidence/console.json"))
            .expect_err("missing identity must fail");
        assert_eq!(
            error.diagnostic_code(),
            "LW_CONSOLE_EVIDENCE_SCHEMA_INVALID"
        );
        Ok(())
    }

    #[test]
    fn report_rejects_identity_mismatch_and_escaped_path() -> Result<(), Box<dyn std::error::Error>>
    {
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
        let report_path = root.join("artifacts/evidence/console.json");
        fs::write(&report_path, serde_json::to_vec_pretty(&report)?)?;
        let error = validate_for_gate(
            root,
            Path::new("artifacts/evidence/console.json"),
            GateIdentity {
                source_commit: "b000000000000000000000000000000000000000",
                run_id: uuid::Uuid::parse_str("01999999-9999-7999-8999-999999999999")?,
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
            "lifecycleCases": [
                {"case":"positive","status":"passed","environment":{"id":"01999999-9999-7999-8999-999999999701","revision":1,"namespace":"labweaver-run","targetName":"runtime-pod","targetUid":"01999999-9999-7999-8999-999999999601","runtimeIdentity":"release-runtime-identity"},"access":{"grantId":"01999999-9999-7999-8999-999999999501","grantRevision":1,"leaseId":"01999999-9999-7999-8999-999999999401","leaseRevision":1},"capabilityId":"01999999-9999-7999-8999-999999999901","sessionId":"01999999-9999-7999-8999-999999999801","durationMs":1000,"observedDiagnostic":"LW_CONSOLE_SESSION_ACTIVE","staleReconnectDenied":true,"auditSha256":hash('a')},
                {"case":"revoke","status":"passed","environment":{"id":"01999999-9999-7999-8999-999999999702","revision":2,"namespace":"labweaver-run","targetName":"runtime-pod","targetUid":"01999999-9999-7999-8999-999999999602","runtimeIdentity":"release-runtime-identity"},"access":{"grantId":"01999999-9999-7999-8999-999999999502","grantRevision":2,"leaseId":"01999999-9999-7999-8999-999999999402","leaseRevision":2},"capabilityId":"01999999-9999-7999-8999-999999999902","sessionId":"01999999-9999-7999-8999-999999999802","durationMs":1000,"observedDiagnostic":"LW_CONSOLE_AUTHORIZATION_ENDED","staleReconnectDenied":true,"auditSha256":hash('a')},
                {"case":"expiry","status":"passed","environment":{"id":"01999999-9999-7999-8999-999999999703","revision":3,"namespace":"labweaver-run","targetName":"runtime-pod","targetUid":"01999999-9999-7999-8999-999999999603","runtimeIdentity":"release-runtime-identity"},"access":{"grantId":"01999999-9999-7999-8999-999999999503","grantRevision":3,"leaseId":"01999999-9999-7999-8999-999999999403","leaseRevision":3},"capabilityId":"01999999-9999-7999-8999-999999999903","sessionId":"01999999-9999-7999-8999-999999999803","durationMs":1000,"observedDiagnostic":"LW_CONSOLE_AUTHORIZATION_ENDED","staleReconnectDenied":true,"auditSha256":hash('a')},
                {"case":"stop","status":"passed","environment":{"id":"01999999-9999-7999-8999-999999999704","revision":4,"namespace":"labweaver-run","targetName":"runtime-pod","targetUid":"01999999-9999-7999-8999-999999999604","runtimeIdentity":"release-runtime-identity"},"access":{"grantId":"01999999-9999-7999-8999-999999999504","grantRevision":4,"leaseId":"01999999-9999-7999-8999-999999999404","leaseRevision":4},"capabilityId":"01999999-9999-7999-8999-999999999904","sessionId":"01999999-9999-7999-8999-999999999804","durationMs":1000,"observedDiagnostic":"LW_CONSOLE_AUTHORIZATION_ENDED","staleReconnectDenied":true,"auditSha256":hash('a')},
                {"case":"delete","status":"passed","environment":{"id":"01999999-9999-7999-8999-999999999705","revision":5,"namespace":"labweaver-run","targetName":"runtime-pod","targetUid":"01999999-9999-7999-8999-999999999605","runtimeIdentity":"release-runtime-identity"},"access":{"grantId":"01999999-9999-7999-8999-999999999505","grantRevision":5,"leaseId":"01999999-9999-7999-8999-999999999405","leaseRevision":5},"capabilityId":"01999999-9999-7999-8999-999999999905","sessionId":"01999999-9999-7999-8999-999999999805","durationMs":1000,"observedDiagnostic":"LW_CONSOLE_AUTHORIZATION_ENDED","staleReconnectDenied":true,"auditSha256":hash('a')},
                {"case":"control-channel-loss","status":"passed","environment":{"id":"01999999-9999-7999-8999-999999999706","revision":6,"namespace":"labweaver-run","targetName":"runtime-pod","targetUid":"01999999-9999-7999-8999-999999999606","runtimeIdentity":"release-runtime-identity"},"access":{"grantId":"01999999-9999-7999-8999-999999999506","grantRevision":6,"leaseId":"01999999-9999-7999-8999-999999999406","leaseRevision":6},"capabilityId":"01999999-9999-7999-8999-999999999906","sessionId":"01999999-9999-7999-8999-999999999806","durationMs":1000,"observedDiagnostic":"LW_CONSOLE_CONTROL_CHANNEL_LOST","staleReconnectDenied":true,"auditSha256":hash('a')}
            ],
            "browserEvidence": {"browser":"chromium","browserVersion":"1.61.1","viewport":{"width":1440,"height":900},"traceSha256":hash('2'),"screenshotSha256":hash('3'),"videoSha256":hash('4')},
            "cleanup": {"completed":true,"readbackCompleted":true,"temporaryFaultPolicyRemoved":true,"preCounts":{"capabilities":6,"sessions":6,"pods":6,"vmis":0,"pvcs":0,"faultPolicies":1},"postCounts":{"capabilities":0,"sessions":0,"pods":0,"vmis":0,"pvcs":0,"faultPolicies":0},"readbackSha256":hash('5')},
            "redaction": {"verified":true,"containsSecrets":false,"containsRawUserContent":false,"containsTerminalTranscript":false,"containsVncFrames":false,"containsAbsolutePaths":false}
        })
    }

    pub(crate) fn hash(character: char) -> String {
        format!("sha256:{}", character.to_string().repeat(64))
    }
}
