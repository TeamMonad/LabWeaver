use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use super::AppError;

const MANIFEST_PATH: &str = "tests/fixtures/acceptance/sprint3-acceptance-assets.json";
const REPORT_ROOT: &str = "tests/fixtures/acceptance/reports";
const EXPECTATIONS_PATH: &str = "tests/fixtures/acceptance/fixture-expectations.json";
const ASSET_SCHEMA: &str = "sprint3-acceptance-assets.v1.schema.json";
const EVIDENCE_SCHEMA: &str = "sprint3-acceptance-evidence.v1.schema.json";
const FEATURE_COMPLETE_SCHEMA: &str = "sprint3-feature-complete.v1.schema.json";
const REQUIRED_SCENARIOS: [&str; 3] = [
    "oj-real-e4",
    "container-linux-clone-real-e4",
    "kubevirt-linux-clone-real-e4",
];
const REQUIRED_SAMPLE_KINDS: [&str; 6] = [
    "correct",
    "compile-error",
    "wrong-answer",
    "time-limit",
    "memory-limit",
    "output-limit",
];
const REQUIRED_NEGATIVE_CASES: [(&str, &[&str]); 8] = [
    (
        "ssrf",
        &[
            "http",
            "loopback",
            "rfc1918",
            "link-local",
            "cloud-metadata",
            "ipv6-loopback",
            "ipv6-link-local",
            "ipv6-private",
            "redirect-private",
            "dns-rebinding",
            "protocol-downgrade",
            "embedded-credentials",
            "unpinned-base-image",
            "unapproved-hostname",
            "redirect-limit",
        ],
    ),
    (
        "vulnerability",
        &[
            "high-visible",
            "critical-blocked",
            "database-unavailable",
            "scan-expired",
            "identity-mismatch",
        ],
    ),
    (
        "secret",
        &[
            "source",
            "build-context",
            "image",
            "evaluator-output",
            "log-evidence",
        ],
    ),
    (
        "malicious-file",
        &[
            "path-traversal",
            "absolute-path",
            "symlink-escape",
            "archive-bomb",
            "oversized-file",
            "file-count-limit",
            "invalid-encoding",
            "special-device",
            "overwrite-existing",
            "mime-extension-mismatch",
        ],
    ),
    (
        "license",
        &[
            "allowed",
            "denied",
            "unknown",
            "compound-expression",
            "metadata-missing",
            "scanner-unavailable",
            "policy-revision-mismatch",
        ],
    ),
    (
        "signature",
        &[
            "unsigned",
            "issuer-mismatch",
            "subject-mismatch",
            "digest-mismatch",
            "trust-revision-mismatch",
            "proof-missing",
            "registry-identity-mismatch",
            "tag-only",
            "latest",
            "rollback-unverified-digest",
            "fixture-claims-live",
        ],
    ),
    (
        "invalid-evaluator-output",
        &[
            "protected-score",
            "unknown-field",
            "unknown-case",
            "duplicate-case",
            "missing-case",
            "negative-score",
            "integer-overflow",
            "non-integer-score",
            "case-id-mismatch",
            "spec-hash-mismatch",
            "artifact-hash-mismatch",
            "run-identity-mismatch",
            "invalid-terminal-state",
            "oversized-output",
            "secret-leak",
            "invalid-utf8",
        ],
    ),
    (
        "cross-tenant-access",
        &[
            "foreign-tenant",
            "foreign-course",
            "foreign-release",
            "foreign-submission",
            "foreign-run",
            "foreign-environment",
            "foreign-endpoint",
            "foreign-access-grant",
            "cross-actor-idempotency",
            "revoked-membership",
            "owner-lease-change",
            "stale-revision",
        ],
    ),
];

pub(super) fn list() {
    for scenario in REQUIRED_SCENARIOS {
        println!("{scenario}");
    }
}

pub(super) fn validate(root: &Path) -> Result<(), AppError> {
    let manifest_path = root.join(MANIFEST_PATH);
    let document = read_schema_instance(
        root,
        ASSET_SCHEMA,
        &manifest_path,
        "LW_TEST_ASSET_SCHEMA_INVALID",
    )?;
    validate_manifest(root, &document)
}

pub(super) fn validate_report(root: &Path, report: &Path) -> Result<(), AppError> {
    let report = read_schema_instance(
        root,
        EVIDENCE_SCHEMA,
        report,
        "LW_TEST_ASSET_REPORT_SCHEMA_INVALID",
    )?;
    validate_report_value(&report)
}

pub(super) fn validate_feature_complete(root: &Path, report: &Path) -> Result<(), AppError> {
    let value = read_schema_instance(
        root,
        FEATURE_COMPLETE_SCHEMA,
        report,
        "LW_TEST_ASSET_FEATURE_COMPLETE_SCHEMA_INVALID",
    )?;
    validate_feature_complete_value(root, &value)
}

pub(super) fn validate_fixtures(root: &Path) -> Result<(), AppError> {
    validate(root)?;
    let expectations = load_expectations(root)?;
    let report_root = root.join(REPORT_ROOT);
    let mut discovered = BTreeSet::new();

    for path in json_files(&report_root.join("valid"))? {
        validate_report(root, &path)?;
        discovered.insert(relative_forward_slash(root, &path)?);
    }
    for path in json_files(&report_root.join("invalid"))? {
        validate_expected_failure(root, &path, &expectations, false)?;
        discovered.insert(relative_forward_slash(root, &path)?);
    }
    for path in json_files(&report_root.join("feature-complete/valid"))? {
        validate_feature_complete(root, &path)?;
        discovered.insert(relative_forward_slash(root, &path)?);
    }
    for path in json_files(&report_root.join("feature-complete/invalid"))? {
        validate_expected_failure(root, &path, &expectations, true)?;
        discovered.insert(relative_forward_slash(root, &path)?);
    }

    let expected_paths = expectations.keys().cloned().collect::<BTreeSet<_>>();
    let invalid_paths = discovered
        .iter()
        .filter(|path| path.contains("/invalid/"))
        .cloned()
        .collect::<BTreeSet<_>>();
    if expected_paths != invalid_paths {
        return Err(code(
            "LW_TEST_ASSET_FIXTURE_EXPECTATION_MISMATCH",
            "invalid fixture inventory and expected diagnostics differ",
        ));
    }
    if discovered.is_empty() {
        return Err(code(
            "LW_TEST_ASSET_FIXTURE_INVENTORY_EMPTY",
            "no acceptance report fixtures were discovered",
        ));
    }
    Ok(())
}

fn validate_manifest(root: &Path, manifest: &Value) -> Result<(), AppError> {
    let scenarios = array(manifest, "scenarios", "LW_TEST_ASSET_SCHEMA_INVALID")?;
    require_unique_ids(scenarios, "id", "LW_TEST_ASSET_DUPLICATE_SCENARIO")?;
    let scenario_ids = ids(scenarios, "id");
    if scenario_ids != REQUIRED_SCENARIOS.into_iter().collect() {
        return Err(code(
            "LW_TEST_ASSET_SCENARIO_INVENTORY_INVALID",
            "the exact three frozen golden scenarios are required",
        ));
    }
    for scenario in scenarios {
        if scenario["currentEvidenceLevel"] != "E1"
            || scenario["executionMode"] != "fixture"
            || scenario["providerMode"] != "mock"
            || scenario["isFixture"] != true
            || scenario["isMock"] != true
            || scenario["futureLiveE4Required"] != true
        {
            return Err(code(
                "LW_TEST_ASSET_SCENARIO_BOUNDARY_INVALID",
                "frozen scenario assets must remain explicit E1 fixtures pending live E4",
            ));
        }
        for field in [
            "productEntrypoint",
            "actor",
            "authenticationIdentity",
            "authorizationIdentity",
            "submissionIdentity",
            "labReleaseIdentity",
            "evaluationReleaseIdentity",
            "environmentTemplateReleaseIdentity",
            "imageDigest",
            "specHash",
            "artifactHash",
            "runId",
            "stepRunId",
            "attemptId",
            "resourceIdentity",
            "accessIdentity",
            "traceId",
            "preconditions",
            "actions",
            "expectedTransitions",
            "expectedDiagnostics",
            "expectedEvidence",
            "failureEvidence",
            "cleanup",
            "rollback",
            "e4SuccessConditions",
        ] {
            if scenario.get(field).is_none() {
                return Err(code(
                    "LW_TEST_ASSET_SCENARIO_CONTRACT_INCOMPLETE",
                    "golden scenario contract is incomplete",
                ));
            }
        }
    }

    validate_samples(
        root,
        array(manifest, "samples", "LW_TEST_ASSET_SCHEMA_INVALID")?,
    )?;
    validate_negative_matrix(array(
        manifest,
        "negativeCases",
        "LW_TEST_ASSET_SCHEMA_INVALID",
    )?)?;
    validate_frontend_inventory(array(
        manifest,
        "frontendAcceptance",
        "LW_TEST_ASSET_SCHEMA_INVALID",
    )?)?;

    let mock = manifest.get("mockBoundary").ok_or_else(|| {
        code(
            "LW_TEST_ASSET_MOCK_BOUNDARY_MISSING",
            "mock boundary is missing",
        )
    })?;
    let forbidden = array(mock, "cannotSatisfy", "LW_TEST_ASSET_MOCK_BOUNDARY_MISSING")?;
    for required in [
        "oj-real-e4",
        "container-linux-clone-real-e4",
        "kubevirt-linux-clone-real-e4",
        "real-resource",
        "real-access",
        "real-provider",
        "real-identity-chain",
        "real-signature",
        "real-admission",
        "real-cleanup",
        "feature-complete",
    ] {
        if !forbidden.iter().any(|item| item.as_str() == Some(required)) {
            return Err(code(
                "LW_TEST_ASSET_MOCK_BOUNDARY_INCOMPLETE",
                "mock exclusion inventory is incomplete",
            ));
        }
    }
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "keeping every sample boundary check in one auditable fail-closed path"
)]
fn validate_samples(root: &Path, samples: &[Value]) -> Result<(), AppError> {
    require_unique_ids(samples, "id", "LW_TEST_ASSET_DUPLICATE_SAMPLE")?;
    let kinds = ids(samples, "kind");
    if kinds != REQUIRED_SAMPLE_KINDS.into_iter().collect() {
        return Err(code(
            "LW_TEST_ASSET_SAMPLE_INVENTORY_INVALID",
            "the exact six C++17 sample kinds are required",
        ));
    }
    let fixture_root = root.join("tests/fixtures/acceptance/oj");
    let canonical_root = fs::canonicalize(&fixture_root).map_err(|_| {
        code(
            "LW_TEST_ASSET_SAMPLE_PATH_INVALID",
            "sample fixture root is unavailable",
        )
    })?;
    for sample in samples {
        if sample["language"] != "c++17"
            || sample["staticValidationOnly"] != true
            || sample["deterministic"] != true
            || sample["networkAllowed"] != false
            || sample["filesystemAllowed"] != false
            || sample["safeToExecuteOnHost"] != false
        {
            return Err(code(
                "LW_TEST_ASSET_SAMPLE_BOUNDARY_INVALID",
                "sample language or static safety boundary is invalid",
            ));
        }
        let relative = sample["path"].as_str().ok_or_else(|| {
            code(
                "LW_TEST_ASSET_SAMPLE_PATH_INVALID",
                "sample path is missing",
            )
        })?;
        let path =
            resolve_confined_existing(&fixture_root, &canonical_root, relative).map_err(|_| {
                code(
                    "LW_TEST_ASSET_SAMPLE_PATH_INVALID",
                    "sample path is missing or escapes the OJ fixture root",
                )
            })?;
        if path.extension().and_then(|value| value.to_str()) != Some("cpp") {
            return Err(code(
                "LW_TEST_ASSET_SAMPLE_TYPE_INVALID",
                "sample source must use the .cpp extension",
            ));
        }
        let bytes = fs::read(&path).map_err(|_| {
            code(
                "LW_TEST_ASSET_SAMPLE_PATH_INVALID",
                "sample source cannot be read",
            )
        })?;
        let actual_hash = format!("{:x}", Sha256::digest(bytes));
        if sample["sha256"].as_str() != Some(actual_hash.as_str()) {
            return Err(code(
                "LW_TEST_ASSET_SAMPLE_HASH_MISMATCH",
                "sample SHA-256 does not match checked-in bytes",
            ));
        }
        if sample["kind"] == "correct" || sample["kind"] == "wrong-answer" {
            validate_auxiliary_hash(
                &fixture_root,
                &canonical_root,
                sample,
                "inputPath",
                "inputSha256",
            )?;
            validate_auxiliary_hash(
                &fixture_root,
                &canonical_root,
                sample,
                "expectedOutputPath",
                "expectedOutputSha256",
            )?;
        }
        if sample["kind"] == "wrong-answer" {
            validate_auxiliary_hash(
                &fixture_root,
                &canonical_root,
                sample,
                "actualOutputPath",
                "actualOutputSha256",
            )?;
            if sample["actualOutputSha256"] == sample["expectedOutputSha256"] {
                return Err(code(
                    "LW_TEST_ASSET_SAMPLE_OUTCOME_INVALID",
                    "WrongAnswer actual and expected output hashes must differ",
                ));
            }
        }
        let limits = sample.get("limits").ok_or_else(|| {
            code(
                "LW_TEST_ASSET_SAMPLE_LIMITS_INVALID",
                "sample limits are missing",
            )
        })?;
        for key in [
            "compileWallMs",
            "runWallMs",
            "cpuMs",
            "memoryBytes",
            "outputBytes",
        ] {
            if limits
                .get(key)
                .and_then(Value::as_u64)
                .is_none_or(|value| value == 0)
            {
                return Err(code(
                    "LW_TEST_ASSET_SAMPLE_LIMITS_INVALID",
                    "all sample resource limits must be positive",
                ));
            }
        }
        if sample["kind"] != "correct"
            && sample
                .get("expectedDiagnostic")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
        {
            return Err(code(
                "LW_TEST_ASSET_SAMPLE_DIAGNOSTIC_MISSING",
                "negative sample diagnostic is missing",
            ));
        }
    }
    Ok(())
}

fn validate_auxiliary_hash(
    fixture_root: &Path,
    canonical_root: &Path,
    sample: &Value,
    path_key: &str,
    hash_key: &str,
) -> Result<(), AppError> {
    let reference = sample
        .get(path_key)
        .and_then(Value::as_str)
        .ok_or_else(|| {
            code(
                "LW_TEST_ASSET_SAMPLE_IO_MISSING",
                "sample input or expected output reference is missing",
            )
        })?;
    let path =
        resolve_confined_existing(fixture_root, canonical_root, reference).map_err(|_| {
            code(
                "LW_TEST_ASSET_SAMPLE_PATH_INVALID",
                "sample input or output path is invalid",
            )
        })?;
    let bytes = fs::read(path).map_err(|_| {
        code(
            "LW_TEST_ASSET_SAMPLE_PATH_INVALID",
            "sample input or output cannot be read",
        )
    })?;
    let actual = format!("{:x}", Sha256::digest(bytes));
    if sample.get(hash_key).and_then(Value::as_str) != Some(actual.as_str()) {
        return Err(code(
            "LW_TEST_ASSET_SAMPLE_HASH_MISMATCH",
            "sample input or output SHA-256 does not match checked-in bytes",
        ));
    }
    Ok(())
}

fn validate_negative_matrix(cases: &[Value]) -> Result<(), AppError> {
    require_unique_ids(cases, "id", "LW_TEST_ASSET_DUPLICATE_NEGATIVE_CASE")?;
    let mut actual: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for case in cases {
        let category = case["category"].as_str().unwrap_or_default();
        let vector = case["vector"].as_str().unwrap_or_default();
        actual.entry(category).or_default().insert(vector);
        if case["expectedResult"] != "rejected"
            || case["safeStaticFixture"] != true
            || case["performsNetworkRequest"] != false
            || case["persistsState"] != false
            || case["expectedDiagnostic"]
                .as_str()
                .is_none_or(str::is_empty)
        {
            return Err(code(
                "LW_TEST_ASSET_NEGATIVE_CASE_BOUNDARY_INVALID",
                "negative cases must be safe, static, deterministic rejections",
            ));
        }
        if category == "cross-tenant-access"
            && (case["objectExistenceDisclosed"] != false
                || case["createsIdempotencyFact"] != false
                || case["createsOutbox"] != false)
        {
            return Err(code(
                "LW_TEST_ASSET_CROSS_TENANT_BOUNDARY_INVALID",
                "cross-tenant cases must leave no observable or durable fact",
            ));
        }
    }
    if actual.len() != REQUIRED_NEGATIVE_CASES.len() {
        return Err(code(
            "LW_TEST_ASSET_NEGATIVE_MATRIX_INCOMPLETE",
            "negative matrix category inventory is incomplete",
        ));
    }
    for (category, required) in REQUIRED_NEGATIVE_CASES {
        let expected = required.iter().copied().collect::<BTreeSet<_>>();
        if actual.get(category) != Some(&expected) {
            return Err(code(
                "LW_TEST_ASSET_NEGATIVE_MATRIX_INCOMPLETE",
                "negative matrix vector inventory is incomplete or contains unknown vectors",
            ));
        }
    }
    Ok(())
}

fn validate_frontend_inventory(items: &[Value]) -> Result<(), AppError> {
    require_unique_ids(items, "id", "LW_TEST_ASSET_DUPLICATE_FRONTEND_CASE")?;
    let required = [
        "teacher-oj-release",
        "student-oj-submit",
        "student-container-clone",
        "student-kubevirt-clone",
        "teacher-evaluation-readback",
        "admin-audit-readback",
    ];
    if ids(items, "id") != required.into_iter().collect() {
        return Err(code(
            "LW_TEST_ASSET_FRONTEND_INVENTORY_INCOMPLETE",
            "frontend acceptance inventory is incomplete or contains unknown cases",
        ));
    }
    for item in items {
        if item["currentStatus"] != "planned"
            || item["futureEvidenceLevel"] != "E4"
            || item["fixtureMaySatisfy"] != false
            || item["requiresAuthentication"] != true
            || item["requiresAuthorization"] != true
            || item["requiresBackendReadback"] != true
            || item["requiresCleanup"] != true
            || item["route"]
                .as_str()
                .is_none_or(|route| !route.starts_with('/'))
        {
            return Err(code(
                "LW_TEST_ASSET_FRONTEND_BOUNDARY_INVALID",
                "frontend acceptance case lacks a fail-closed product boundary",
            ));
        }
    }
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "evidence escalation rules are intentionally ordered in one auditable path"
)]
fn validate_report_value(report: &Value) -> Result<(), AppError> {
    let scenario = report["scenarioId"].as_str().unwrap_or_default();
    if !REQUIRED_SCENARIOS.contains(&scenario) {
        return Err(code(
            "LW_TEST_ASSET_UNKNOWN_SCENARIO",
            "scenario is not allowlisted",
        ));
    }
    if report["gateId"] != "acceptance" {
        return Err(code(
            "LW_TEST_ASSET_UNKNOWN_GATE",
            "gate is not allowlisted",
        ));
    }
    let level = report["evidenceLevel"].as_str().unwrap_or_default();
    let mode = report["executionMode"].as_str().unwrap_or_default();
    if mode == "planned" && report["result"] == "passed" {
        return Err(code(
            "LW_TEST_ASSET_PLANNED_RESULT_INVALID",
            "planned evidence cannot pass a runtime gate",
        ));
    }
    if mode == "fixture" && report["result"] == "passed" {
        return Err(code(
            "LW_TEST_ASSET_FIXTURE_RESULT_INVALID",
            "fixture evidence cannot pass a runtime gate",
        ));
    }
    if mode == "local" && report["connected"] == true {
        return Err(code(
            "LW_TEST_ASSET_LOCAL_CONNECTED_INVALID",
            "local evidence cannot claim connected execution",
        ));
    }
    if mode == "ci" && report["liveRuntime"] == true {
        return Err(code(
            "LW_TEST_ASSET_CI_LIVE_RUNTIME_INVALID",
            "CI evidence cannot implicitly claim a live runtime",
        ));
    }
    if (level == "E3" || level == "E4") && report["isFixture"] == true {
        return Err(code(
            if level == "E3" {
                "LW_TEST_ASSET_E3_FIXTURE_FORBIDDEN"
            } else {
                "LW_TEST_ASSET_E4_FIXTURE_FORBIDDEN"
            },
            "fixture evidence cannot satisfy real E3 or E4",
        ));
    }
    if level == "E4" && (report["isMock"] == true || report["providerMode"] == "mock") {
        return Err(code(
            "LW_TEST_ASSET_E4_MOCK_FORBIDDEN",
            "mock evidence cannot satisfy E4",
        ));
    }
    if level == "E3" || level == "E4" {
        validate_real_identity(report, level)?;
    }
    if level == "E4" {
        if mode != "live-runtime"
            || report["connected"] != true
            || report["liveRuntime"] != true
            || report["productionIdentity"] != true
        {
            return Err(code(
                "LW_TEST_ASSET_E4_IDENTITY_MISSING",
                "E4 requires connected live-runtime production identity",
            ));
        }
        if report.pointer("/cleanup/completed") != Some(&Value::Bool(true))
            || report.pointer("/cleanup/readbackCompleted") != Some(&Value::Bool(true))
            || report.pointer("/cleanup/runId") != report.get("runId")
        {
            return Err(code(
                "LW_TEST_ASSET_E4_CLEANUP_REQUIRED",
                "E4 cleanup and readback must bind the evaluation run",
            ));
        }
        if report.pointer("/rollback/verified") != Some(&Value::Bool(true)) {
            return Err(code(
                "LW_TEST_ASSET_E4_ROLLBACK_REQUIRED",
                "E4 rollback verification is required",
            ));
        }
        if report["unresolvedBlockers"]
            .as_array()
            .is_none_or(|items| !items.is_empty())
            || report["knownLimitations"]
                .as_array()
                .is_none_or(|items| !items.is_empty())
        {
            return Err(code(
                "LW_TEST_ASSET_E4_BLOCKER_PRESENT",
                "E4 cannot pass with blockers or known limitations",
            ));
        }
        if report["requiredStepsSkipped"] == true {
            return Err(code(
                "LW_TEST_ASSET_E4_REQUIRED_STEP_SKIPPED",
                "E4 cannot skip a required step",
            ));
        }
        if report["result"] == "passed" && report["diagnosticCode"] != "LW_ACCEPTANCE_E4_PASS" {
            return Err(code(
                "LW_TEST_ASSET_E4_DIAGNOSTIC_INVALID",
                "passed E4 evidence requires the stable pass diagnostic",
            ));
        }
    }
    Ok(())
}

fn validate_real_identity(report: &Value, level: &str) -> Result<(), AppError> {
    if report["isMock"] == true
        || report["providerMode"] != "real"
        || report["connected"] != true
        || report["productionIdentity"] != true
    {
        return Err(code(
            if level == "E3" {
                "LW_TEST_ASSET_E3_IDENTITY_MISSING"
            } else {
                "LW_TEST_ASSET_E4_IDENTITY_MISSING"
            },
            "real evidence requires provider, connection, and production identity",
        ));
    }
    for key in [
        "productEntrypoint",
        "actor",
        "verifier",
        "tenantId",
        "courseId",
        "authenticationIdentity",
        "authorizationIdentity",
        "submissionIdentity",
        "labRelease",
        "evaluationRelease",
        "environmentTemplateRelease",
        "providerIdentity",
        "resourceIdentity",
        "accessIdentity",
        "runId",
        "stepRunId",
        "attemptId",
        "traceId",
        "buildIdentity",
        "sourceCommit",
        "baseCommit",
        "imageDigest",
        "artifactHash",
        "specHash",
        "policyRevision",
        "trustRevision",
        "environmentIdentity",
        "deploymentIdentity",
    ] {
        if report
            .get(key)
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
        {
            return Err(code(
                if level == "E3" {
                    "LW_TEST_ASSET_E3_IDENTITY_MISSING"
                } else {
                    "LW_TEST_ASSET_E4_IDENTITY_MISSING"
                },
                "real evidence identity or immutable digest is missing",
            ));
        }
    }
    let source = report["sourceCommit"].as_str();
    if report["buildSourceCommit"].as_str() != source
        || report["deploymentSourceCommit"].as_str() != source
    {
        return Err(code(
            "LW_TEST_ASSET_REPORT_IDENTITY_MISMATCH",
            "source, build, and deployment commit identities differ",
        ));
    }
    if report["runId"] != report["readbackRunId"] {
        return Err(code(
            "LW_TEST_ASSET_REPORT_IDENTITY_MISMATCH",
            "write and readback run identities differ",
        ));
    }
    let started = parse_timestamp(report, "startedAt", level)?;
    let completed = parse_timestamp(report, "completedAt", level)?;
    let expires = parse_timestamp(report, "expiresAt", level)?;
    if started > completed || completed >= expires || expires <= OffsetDateTime::now_utc() {
        return Err(code(
            "LW_TEST_ASSET_REPORT_TIME_INVALID",
            "real evidence timestamps are reversed or expired",
        ));
    }
    if report["durationMs"].as_u64().is_none()
        || report["dirty"] != false
        || report["toolVersions"]
            .as_object()
            .is_none_or(serde_json::Map::is_empty)
        || report.pointer("/redaction/verified") != Some(&Value::Bool(true))
    {
        return Err(code(
            if level == "E3" {
                "LW_TEST_ASSET_E3_IDENTITY_MISSING"
            } else {
                "LW_TEST_ASSET_E4_IDENTITY_MISSING"
            },
            "real evidence duration, clean build, tools, or redaction proof is missing",
        ));
    }
    Ok(())
}

fn parse_timestamp(report: &Value, key: &str, level: &str) -> Result<OffsetDateTime, AppError> {
    let value = report.get(key).and_then(Value::as_str).ok_or_else(|| {
        code(
            if level == "E3" {
                "LW_TEST_ASSET_E3_IDENTITY_MISSING"
            } else {
                "LW_TEST_ASSET_E4_IDENTITY_MISSING"
            },
            "real evidence timestamp is missing",
        )
    })?;
    OffsetDateTime::parse(value, &Rfc3339).map_err(|_| {
        code(
            "LW_TEST_ASSET_REPORT_TIME_INVALID",
            "real evidence timestamp is not RFC3339",
        )
    })
}

#[allow(
    clippy::too_many_lines,
    reason = "Feature Complete references and identity binding are reviewed as one atomic gate"
)]
fn validate_feature_complete_value(root: &Path, value: &Value) -> Result<(), AppError> {
    if value["result"] != "passed" {
        return Err(code(
            "LW_TEST_ASSET_FEATURE_COMPLETE_NOT_PASSED",
            "Feature Complete report must explicitly pass",
        ));
    }
    if value["unresolvedBlockers"]
        .as_array()
        .is_none_or(|items| !items.is_empty())
        || value["knownLimitations"]
            .as_array()
            .is_none_or(|items| !items.is_empty())
    {
        return Err(code(
            "LW_TEST_ASSET_FEATURE_COMPLETE_BLOCKER_PRESENT",
            "Feature Complete cannot contain blockers or limitations",
        ));
    }
    for (key, diagnostic) in [
        (
            "sprint2Issue64Evidence",
            "LW_TEST_ASSET_FEATURE_COMPLETE_64_MISSING",
        ),
        ("ojE4Report", "LW_TEST_ASSET_FEATURE_COMPLETE_OJ_E4_MISSING"),
        (
            "containerCloneE4Report",
            "LW_TEST_ASSET_FEATURE_COMPLETE_CONTAINER_E4_MISSING",
        ),
        (
            "kubevirtCloneE4Report",
            "LW_TEST_ASSET_FEATURE_COMPLETE_KUBEVIRT_E4_MISSING",
        ),
    ] {
        reference_path(value, key, diagnostic)?;
    }
    let issue64 = reference_path(
        value,
        "sprint2Issue64Evidence",
        "LW_TEST_ASSET_FEATURE_COMPLETE_64_MISSING",
    )?;
    let issue64_path = resolve_report_reference(root, issue64)?;
    let mut report_paths = Vec::new();
    for (key, scenario, missing) in [
        (
            "ojE4Report",
            "oj-real-e4",
            "LW_TEST_ASSET_FEATURE_COMPLETE_OJ_E4_MISSING",
        ),
        (
            "containerCloneE4Report",
            "container-linux-clone-real-e4",
            "LW_TEST_ASSET_FEATURE_COMPLETE_CONTAINER_E4_MISSING",
        ),
        (
            "kubevirtCloneE4Report",
            "kubevirt-linux-clone-real-e4",
            "LW_TEST_ASSET_FEATURE_COMPLETE_KUBEVIRT_E4_MISSING",
        ),
    ] {
        let reference = reference_path(value, key, missing)?;
        let path = resolve_report_reference(root, reference)?;
        report_paths.push((path, scenario));
    }

    let mut reports = Vec::new();
    for (path, scenario) in report_paths {
        let report = read_schema_instance(
            root,
            EVIDENCE_SCHEMA,
            &path,
            "LW_TEST_ASSET_FEATURE_COMPLETE_E4_INVALID",
        )?;
        validate_report_value(&report).map_err(|_| {
            code(
                "LW_TEST_ASSET_FEATURE_COMPLETE_E4_INVALID",
                "Feature Complete references invalid E4 evidence",
            )
        })?;
        if report["scenarioId"] != scenario
            || report["evidenceLevel"] != "E4"
            || report["result"] != "passed"
        {
            return Err(code(
                "LW_TEST_ASSET_FEATURE_COMPLETE_E4_INVALID",
                "Feature Complete references the wrong scenario or evidence level",
            ));
        }
        reports.push(report);
    }
    let issue64_value = read_json(&issue64_path, "LW_TEST_ASSET_FEATURE_COMPLETE_64_INVALID")?;
    if issue64_value["issue"] != 64
        || issue64_value["result"] != "passed"
        || issue64_value["evidenceLevel"] != "E4"
        || issue64_value["isFixture"] != false
        || issue64_value["isMock"] != false
    {
        return Err(code(
            "LW_TEST_ASSET_FEATURE_COMPLETE_64_INVALID",
            "#64 reference is not real passed E4 evidence",
        ));
    }
    for key in [
        "sourceCommit",
        "buildIdentity",
        "deploymentIdentity",
        "environmentIdentity",
        "tenantId",
        "courseId",
    ] {
        let expected = value.get(key);
        if expected.is_none() || reports.iter().any(|report| report.get(key) != expected) {
            return Err(code(
                "LW_TEST_ASSET_FEATURE_COMPLETE_IDENTITY_MISMATCH",
                "Feature Complete and referenced E4 reports do not share one identity",
            ));
        }
        if key != "tenantId" && key != "courseId" && issue64_value.get(key) != expected {
            return Err(code(
                "LW_TEST_ASSET_FEATURE_COMPLETE_IDENTITY_MISMATCH",
                "#64 and Sprint 3 reports do not share one build/deployment identity",
            ));
        }
    }
    Ok(())
}

fn resolve_report_reference(root: &Path, reference: &str) -> Result<PathBuf, AppError> {
    let reports = root.join(REPORT_ROOT);
    let canonical_root = fs::canonicalize(&reports).map_err(|_| {
        code(
            "LW_TEST_ASSET_REPORT_REFERENCE_INVALID",
            "report fixture root is unavailable",
        )
    })?;
    resolve_confined_existing(&reports, &canonical_root, reference)
        .map_err(|reason| code("LW_TEST_ASSET_REPORT_REFERENCE_INVALID", reason))
}

fn resolve_confined_existing(
    lexical_root: &Path,
    canonical_root: &Path,
    reference: &str,
) -> Result<PathBuf, &'static str> {
    if reference.is_empty()
        || reference.contains('\\')
        || reference.starts_with('/')
        || reference.starts_with("//")
        || reference.as_bytes().get(1) == Some(&b':')
    {
        return Err("reference must be a portable relative path");
    }
    let relative = Path::new(reference);
    if relative.extension().and_then(|value| value.to_str()) != Some("json")
        && lexical_root.ends_with("reports")
    {
        return Err("report reference must name a JSON file");
    }
    if relative.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err("reference contains traversal or an absolute prefix");
    }
    let path = lexical_root.join(relative);
    let canonical = fs::canonicalize(&path).map_err(|_| "referenced file does not exist")?;
    require_canonical_confinement(canonical_root, &canonical)?;
    if !canonical.is_file() {
        return Err("reference does not resolve to a file");
    }
    Ok(canonical)
}

fn require_canonical_confinement(
    canonical_root: &Path,
    canonical_path: &Path,
) -> Result<(), &'static str> {
    if canonical_path.starts_with(canonical_root) {
        Ok(())
    } else {
        Err("referenced file escapes its allowed root through a symlink")
    }
}

fn validate_expected_failure(
    root: &Path,
    path: &Path,
    expectations: &BTreeMap<String, String>,
    feature_complete: bool,
) -> Result<(), AppError> {
    let relative = relative_forward_slash(root, path)?;
    let expected = expectations.get(&relative).ok_or_else(|| {
        code(
            "LW_TEST_ASSET_FIXTURE_EXPECTATION_MISSING",
            "invalid fixture has no expected diagnostic",
        )
    })?;
    let result = if feature_complete {
        validate_feature_complete(root, path)
    } else {
        validate_report(root, path)
    };
    let actual = result
        .err()
        .ok_or_else(|| {
            code(
                "LW_TEST_ASSET_INVALID_FIXTURE_ACCEPTED",
                "invalid fixture was accepted",
            )
        })?
        .diagnostic_code();
    if actual != expected {
        return Err(code(
            "LW_TEST_ASSET_FIXTURE_DIAGNOSTIC_MISMATCH",
            "invalid fixture returned a different diagnostic",
        ));
    }
    Ok(())
}

fn load_expectations(root: &Path) -> Result<BTreeMap<String, String>, AppError> {
    let value = read_json(
        &root.join(EXPECTATIONS_PATH),
        "LW_TEST_ASSET_FIXTURE_EXPECTATION_MISSING",
    )?;
    let object = value.as_object().ok_or_else(|| {
        code(
            "LW_TEST_ASSET_FIXTURE_EXPECTATION_MISSING",
            "fixture expectation manifest must be an object",
        )
    })?;
    let mut result = BTreeMap::new();
    for (path, diagnostic) in object {
        let diagnostic = diagnostic.as_str().ok_or_else(|| {
            code(
                "LW_TEST_ASSET_FIXTURE_EXPECTATION_MISSING",
                "fixture diagnostic must be a string",
            )
        })?;
        if result.insert(path.clone(), diagnostic.to_owned()).is_some() {
            return Err(code(
                "LW_TEST_ASSET_FIXTURE_EXPECTATION_MISMATCH",
                "duplicate fixture expectation",
            ));
        }
    }
    Ok(result)
}

fn json_files(directory: &Path) -> Result<Vec<PathBuf>, AppError> {
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut pending = vec![directory.to_path_buf()];
    let mut result = Vec::new();
    while let Some(current) = pending.pop() {
        for entry in fs::read_dir(&current).map_err(|_| {
            code(
                "LW_TEST_ASSET_FIXTURE_INVENTORY_INVALID",
                "fixture directory cannot be read",
            )
        })? {
            let entry = entry.map_err(|_| {
                code(
                    "LW_TEST_ASSET_FIXTURE_INVENTORY_INVALID",
                    "fixture directory entry cannot be read",
                )
            })?;
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().and_then(|value| value.to_str()) == Some("json") {
                result.push(path);
            }
        }
    }
    result.sort();
    Ok(result)
}

fn read_schema_instance(
    root: &Path,
    schema_name: &str,
    path: &Path,
    diagnostic: &'static str,
) -> Result<Value, AppError> {
    let schema = read_json(&root.join("schemas/results").join(schema_name), diagnostic)?;
    let instance = read_json(path, diagnostic)?;
    let validator = jsonschema::validator_for(&schema)
        .map_err(|_| code(diagnostic, "acceptance JSON Schema cannot be compiled"))?;
    if !validator.is_valid(&instance) {
        return Err(code(
            diagnostic,
            "document does not satisfy its JSON Schema",
        ));
    }
    Ok(instance)
}

fn read_json(path: &Path, diagnostic: &'static str) -> Result<Value, AppError> {
    let text =
        fs::read_to_string(path).map_err(|_| code(diagnostic, "JSON document is missing"))?;
    serde_json::from_str(&text).map_err(|_| code(diagnostic, "JSON document is malformed"))
}

fn reference_path<'a>(
    value: &'a Value,
    key: &str,
    diagnostic: &'static str,
) -> Result<&'a str, AppError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            code(
                diagnostic,
                "required Feature Complete report reference is missing",
            )
        })
}

fn require_unique_ids(
    values: &[Value],
    field: &str,
    diagnostic: &'static str,
) -> Result<(), AppError> {
    let mut seen = BTreeSet::new();
    for value in values {
        let id = value.get(field).and_then(Value::as_str).unwrap_or_default();
        if id.is_empty() || !seen.insert(id) {
            return Err(code(diagnostic, "identifier is missing or duplicated"));
        }
    }
    Ok(())
}

fn ids<'a>(values: &'a [Value], field: &str) -> BTreeSet<&'a str> {
    values
        .iter()
        .filter_map(|value| value.get(field).and_then(Value::as_str))
        .collect()
}

fn array<'a>(
    value: &'a Value,
    key: &str,
    diagnostic: &'static str,
) -> Result<&'a [Value], AppError> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| code(diagnostic, "required array is missing"))
}

fn relative_forward_slash(root: &Path, path: &Path) -> Result<String, AppError> {
    path.strip_prefix(root)
        .map(|value| value.to_string_lossy().replace('\\', "/"))
        .map_err(|_| {
            code(
                "LW_TEST_ASSET_FIXTURE_INVENTORY_INVALID",
                "fixture path is outside the repository",
            )
        })
}

fn code(code: &'static str, detail: impl Into<String>) -> AppError {
    AppError::AcceptanceAsset {
        code,
        detail: detail.into(),
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test setup failures should panic at the exact fixture operation"
)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
    }

    fn valid_report(name: &str) -> Value {
        read_json(&root().join(REPORT_ROOT).join("valid").join(name), "TEST")
            .expect("valid fixture must parse")
    }

    fn diagnostic(result: Result<(), AppError>) -> &'static str {
        result.expect_err("validation must fail").diagnostic_code()
    }

    #[test]
    fn manifest_and_all_checked_in_fixtures_validate() {
        validate_fixtures(&root()).expect("checked-in fixture corpus must validate");
    }

    #[test]
    fn evidence_level_boundaries_fail_closed() {
        let mut report = valid_report("local-e2.json");
        report["executionMode"] = Value::String("planned".into());
        report["result"] = Value::String("passed".into());
        assert_eq!(
            diagnostic(validate_report_value(&report)),
            "LW_TEST_ASSET_PLANNED_RESULT_INVALID"
        );
        report["executionMode"] = Value::String("fixture".into());
        assert_eq!(
            diagnostic(validate_report_value(&report)),
            "LW_TEST_ASSET_FIXTURE_RESULT_INVALID"
        );
        report["executionMode"] = Value::String("local".into());
        report["connected"] = Value::Bool(true);
        assert_eq!(
            diagnostic(validate_report_value(&report)),
            "LW_TEST_ASSET_LOCAL_CONNECTED_INVALID"
        );
    }

    #[test]
    fn e3_and_e4_reject_fixture_mock_and_missing_identity() {
        let mut report = valid_report("local-e2.json");
        report["evidenceLevel"] = Value::String("E3".into());
        report["isFixture"] = Value::Bool(true);
        assert_eq!(
            diagnostic(validate_report_value(&report)),
            "LW_TEST_ASSET_E3_FIXTURE_FORBIDDEN"
        );

        report["evidenceLevel"] = Value::String("E4".into());
        assert_eq!(
            diagnostic(validate_report_value(&report)),
            "LW_TEST_ASSET_E4_FIXTURE_FORBIDDEN"
        );
        report["isFixture"] = Value::Bool(false);
        report["isMock"] = Value::Bool(true);
        report["providerMode"] = Value::String("mock".into());
        assert_eq!(
            diagnostic(validate_report_value(&report)),
            "LW_TEST_ASSET_E4_MOCK_FORBIDDEN"
        );
    }

    #[test]
    fn report_rejects_unknown_scenario_gate_and_identity_drift() {
        let mut report = valid_report("local-e2.json");
        report["scenarioId"] = Value::String("unknown".into());
        assert_eq!(
            diagnostic(validate_report_value(&report)),
            "LW_TEST_ASSET_UNKNOWN_SCENARIO"
        );
        report["scenarioId"] = Value::String("oj-real-e4".into());
        report["gateId"] = Value::String("unknown".into());
        assert_eq!(
            diagnostic(validate_report_value(&report)),
            "LW_TEST_ASSET_UNKNOWN_GATE"
        );
    }

    #[test]
    fn report_reference_rejects_traversal_absolute_drive_unc_and_missing() {
        for reference in [
            "../escape.json",
            "/absolute.json",
            "C:/drive.json",
            "//server/share.json",
            r"\\server\share.json",
            "missing.json",
            "valid/planned-e1.txt",
        ] {
            assert_eq!(
                resolve_report_reference(&root(), reference)
                    .expect_err("unsafe reference must fail")
                    .diagnostic_code(),
                "LW_TEST_ASSET_REPORT_REFERENCE_INVALID"
            );
        }
    }

    #[test]
    fn negative_matrix_and_frontend_inventory_are_exact() {
        let manifest = read_json(&root().join(MANIFEST_PATH), "TEST").unwrap();
        let mut cases = manifest["negativeCases"].as_array().unwrap().clone();
        cases.pop();
        assert_eq!(
            diagnostic(validate_negative_matrix(&cases)),
            "LW_TEST_ASSET_NEGATIVE_MATRIX_INCOMPLETE"
        );
        let mut frontend = manifest["frontendAcceptance"].as_array().unwrap().clone();
        frontend[0]["requiresAuthorization"] = Value::Bool(false);
        assert_eq!(
            diagnostic(validate_frontend_inventory(&frontend)),
            "LW_TEST_ASSET_FRONTEND_BOUNDARY_INVALID"
        );
        let mut duplicated = manifest.clone();
        duplicated["scenarios"][1]["id"] = duplicated["scenarios"][0]["id"].clone();
        assert_eq!(
            diagnostic(validate_manifest(&root(), &duplicated)),
            "LW_TEST_ASSET_DUPLICATE_SCENARIO"
        );
    }

    #[test]
    fn sample_hash_and_limits_fail_closed() {
        let manifest = read_json(&root().join(MANIFEST_PATH), "TEST").unwrap();
        let mut samples = manifest["samples"].as_array().unwrap().clone();
        samples[0]["sha256"] = Value::String("0".repeat(64));
        assert_eq!(
            diagnostic(validate_samples(&root(), &samples)),
            "LW_TEST_ASSET_SAMPLE_HASH_MISMATCH"
        );
        let mut samples = manifest["samples"].as_array().unwrap().clone();
        samples[0]["limits"]["memoryBytes"] = Value::Number(0.into());
        assert_eq!(
            diagnostic(validate_samples(&root(), &samples)),
            "LW_TEST_ASSET_SAMPLE_LIMITS_INVALID"
        );
    }

    #[test]
    fn invalid_fixture_diagnostic_must_match_exactly() {
        let expectations = load_expectations(&root()).unwrap();
        let path = root().join(REPORT_ROOT).join("invalid/planned-passed.json");
        validate_expected_failure(&root(), &path, &expectations, false).unwrap();
        let mut wrong = expectations;
        wrong.insert(
            relative_forward_slash(&root(), &path).unwrap(),
            "LW_WRONG".into(),
        );
        assert_eq!(
            diagnostic(validate_expected_failure(&root(), &path, &wrong, false)),
            "LW_TEST_ASSET_FIXTURE_DIAGNOSTIC_MISMATCH"
        );
    }

    #[test]
    fn canonical_confinement_rejects_symlink_targets_outside_root() {
        let temporary = tempfile::tempdir().unwrap();
        let allowed = temporary.path().join("allowed");
        let outside = temporary.path().join("outside.json");
        fs::create_dir(&allowed).unwrap();
        fs::write(&outside, "{}").unwrap();
        let allowed = fs::canonicalize(allowed).unwrap();
        let outside = fs::canonicalize(outside).unwrap();
        assert_eq!(
            require_canonical_confinement(&allowed, &outside),
            Err("referenced file escapes its allowed root through a symlink")
        );
    }

    #[test]
    fn e4_cleanup_rollback_blocker_skip_and_diagnostic_fail_closed() {
        let path = root()
            .join(REPORT_ROOT)
            .join("invalid/e4-diagnostic-invalid.json");
        let mut report = read_json(&path, "TEST").unwrap();
        report["diagnosticCode"] = Value::String("LW_ACCEPTANCE_E4_PASS".into());
        validate_report_value(&report).expect("synthetic baseline is internally complete");

        let mut candidate = report.clone();
        candidate["cleanup"]["completed"] = Value::Bool(false);
        assert_eq!(
            diagnostic(validate_report_value(&candidate)),
            "LW_TEST_ASSET_E4_CLEANUP_REQUIRED"
        );
        let mut candidate = report.clone();
        candidate["rollback"]["verified"] = Value::Bool(false);
        assert_eq!(
            diagnostic(validate_report_value(&candidate)),
            "LW_TEST_ASSET_E4_ROLLBACK_REQUIRED"
        );
        let mut candidate = report.clone();
        candidate["unresolvedBlockers"] = serde_json::json!(["synthetic"]);
        assert_eq!(
            diagnostic(validate_report_value(&candidate)),
            "LW_TEST_ASSET_E4_BLOCKER_PRESENT"
        );
        let mut candidate = report.clone();
        candidate["requiredStepsSkipped"] = Value::Bool(true);
        assert_eq!(
            diagnostic(validate_report_value(&candidate)),
            "LW_TEST_ASSET_E4_REQUIRED_STEP_SKIPPED"
        );
        let mut candidate = report;
        candidate["diagnosticCode"] = Value::String("LW_WRONG".into());
        assert_eq!(
            diagnostic(validate_report_value(&candidate)),
            "LW_TEST_ASSET_E4_DIAGNOSTIC_INVALID"
        );
    }

    #[test]
    fn feature_complete_rejects_cross_report_identity() {
        let temporary = tempfile::tempdir().unwrap();
        let temp_root = temporary.path();
        fs::create_dir_all(temp_root.join("schemas/results")).unwrap();
        fs::create_dir_all(temp_root.join(REPORT_ROOT).join("support")).unwrap();
        for schema in [EVIDENCE_SCHEMA, FEATURE_COMPLETE_SCHEMA] {
            fs::copy(
                root().join("schemas/results").join(schema),
                temp_root.join("schemas/results").join(schema),
            )
            .unwrap();
        }
        fs::write(
            temp_root.join(REPORT_ROOT).join("support/issue64.json"),
            serde_json::to_vec(&serde_json::json!({
                "issue": 64,
                "result": "passed",
                "evidenceLevel": "E4",
                "isFixture": false,
                "isMock": false,
                "sourceCommit": "1111111111111111111111111111111111111111",
                "buildIdentity": "build-synthetic",
                "deploymentIdentity": "deployment-synthetic",
                "environmentIdentity": "environment-synthetic"
            }))
            .unwrap(),
        )
        .unwrap();
        let mut evidence = read_json(
            &root()
                .join(REPORT_ROOT)
                .join("invalid/e4-diagnostic-invalid.json"),
            "TEST",
        )
        .unwrap();
        evidence["diagnosticCode"] = Value::String("LW_ACCEPTANCE_E4_PASS".into());
        for (file, scenario) in [
            ("oj.json", "oj-real-e4"),
            ("container.json", "container-linux-clone-real-e4"),
            ("kubevirt.json", "kubevirt-linux-clone-real-e4"),
        ] {
            evidence["scenarioId"] = Value::String(scenario.into());
            fs::write(
                temp_root.join(REPORT_ROOT).join("support").join(file),
                serde_json::to_vec(&evidence).unwrap(),
            )
            .unwrap();
        }
        let mut feature = serde_json::json!({
            "schemaVersion": "sprint3-feature-complete.v1",
            "result": "passed",
            "sourceCommit": "1111111111111111111111111111111111111111",
            "buildIdentity": "build-synthetic",
            "deploymentIdentity": "deployment-synthetic",
            "environmentIdentity": "environment-synthetic",
            "tenantId": "tenant-synthetic",
            "courseId": "course-synthetic",
            "sprint2Issue64Evidence": "support/issue64.json",
            "ojE4Report": "support/oj.json",
            "containerCloneE4Report": "support/container.json",
            "kubevirtCloneE4Report": "support/kubevirt.json",
            "unresolvedBlockers": [],
            "knownLimitations": []
        });
        validate_feature_complete_value(temp_root, &feature)
            .expect("same-identity synthetic contract must validate");
        let issue64_path = temp_root.join(REPORT_ROOT).join("support/issue64.json");
        let mut issue64 = read_json(&issue64_path, "TEST").unwrap();
        issue64["isFixture"] = Value::Bool(true);
        fs::write(&issue64_path, serde_json::to_vec(&issue64).unwrap()).unwrap();
        assert_eq!(
            diagnostic(validate_feature_complete_value(temp_root, &feature)),
            "LW_TEST_ASSET_FEATURE_COMPLETE_64_INVALID"
        );
        issue64["isFixture"] = Value::Bool(false);
        fs::write(&issue64_path, serde_json::to_vec(&issue64).unwrap()).unwrap();
        feature["sourceCommit"] = Value::String("2222222222222222222222222222222222222222".into());
        assert_eq!(
            diagnostic(validate_feature_complete_value(temp_root, &feature)),
            "LW_TEST_ASSET_FEATURE_COMPLETE_IDENTITY_MISMATCH"
        );
    }
}
