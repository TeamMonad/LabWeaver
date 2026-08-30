//! Deterministic OJ checker, aggregation, and evidence-boundary tests.

use std::str::FromStr as _;

use persistence_sqlx::Sha256Digest;
use evaluation_service::oj::{
    OJ_EVIDENCE_SCHEMA_VERSION, OjCaseBinding, OjCaseEvidence, OjCaseStatus, OjCheckerKind,
    OjError, OjExecutionEvidence, OjExecutionLimits, OjExecutionPhase, OjExecutionRequest,
    OjFileBinding, OjProcessEvidence, OjTerminalStatus, aggregate_case_evidence, check_output,
};
use uuid::Uuid;

fn digest(marker: &[u8]) -> Sha256Digest {
    Sha256Digest::of_bytes(marker)
}

fn file(path: &str, bytes: &[u8]) -> OjFileBinding {
    OjFileBinding {
        path: path.to_owned(),
        sha256: digest(bytes),
        size_bytes: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
    }
}

fn request(checker: OjCheckerKind) -> OjExecutionRequest {
    OjExecutionRequest {
        schema_version: "evaluation.labweaver.io/oj-execution/v1".to_owned(),
        run_id: Uuid::now_v7(),
        step_run_id: Uuid::now_v7(),
        attempt_id: Uuid::now_v7(),
        trace_id: "trace-oj-test".to_owned(),
        toolchain_profile: "cpp17-approved-v1".to_owned(),
        toolchain_image_digest: format!("sha256:{}", "1".repeat(64)),
        submission_identity: digest(b"submission"),
        evaluator_identity: Some(digest(b"evaluator")),
        source: file("src/main.cpp", b"int main() {}"),
        phase: OjExecutionPhase::Test,
        checker: Some(checker),
        cases: vec![
            OjCaseBinding {
                id: "basic".to_owned(),
                input: file("cases/basic.in", b"1\n"),
                expected: file("cases/basic.out", b"1\n"),
                max_points: 40,
            },
            OjCaseBinding {
                id: "edge".to_owned(),
                input: file("cases/edge.in", b"2\n"),
                expected: file("cases/edge.out", b"2\n"),
                max_points: 60,
            },
        ],
        score_max_points: 80,
        limits: OjExecutionLimits {
            compile_wall_milliseconds: 10_000,
            run_wall_milliseconds: 1_000,
            cpu_milliseconds: 500,
            memory_bytes: 32 * 1024 * 1024,
            output_bytes: 1024,
        },
    }
}

fn evidence(id: &str, status: OjCaseStatus, awarded_points: u32) -> OjCaseEvidence {
    OjCaseEvidence {
        case_id: id.to_owned(),
        status,
        actual_output_sha256: digest(format!("actual-{id}").as_bytes()),
        stdout_bytes: 2,
        stderr_sha256: digest(b""),
        stderr_bytes: 0,
        duration_milliseconds: 1,
        peak_memory_bytes: Some(4096),
        awarded_points,
        diagnostic_code: status.diagnostic_code().to_owned(),
    }
}

fn process_evidence(exit_code: i32) -> OjProcessEvidence {
    OjProcessEvidence {
        exit_code: Some(exit_code),
        signal: None,
        stdout_sha256: digest(b""),
        stdout_bytes: 0,
        stderr_sha256: digest(b""),
        stderr_bytes: 0,
        duration_milliseconds: 1,
        peak_memory_bytes: None,
        timed_out: false,
        output_exceeded: false,
    }
}

fn error_diagnostic<T>(
    result: Result<T, OjError>,
) -> Result<&'static str, Box<dyn std::error::Error>> {
    match result {
        Ok(_) => Err("expected OJ validation failure".into()),
        Err(error) => Ok(error.diagnostic_code()),
    }
}

#[test]
fn exact_checker_is_byte_oriented_and_token_checker_only_normalizes_ascii_whitespace() {
    assert!(check_output(OjCheckerKind::Exact, b"1  2\n", b"1  2\n"));
    assert!(!check_output(OjCheckerKind::Exact, b"1 2\n", b"1  2\n"));

    assert!(check_output(
        OjCheckerKind::Token,
        b"alpha\tbeta\r\n42",
        b" alpha beta 42 \n"
    ));
    assert!(!check_output(
        OjCheckerKind::Token,
        "a\u{00a0}b".as_bytes(),
        b"a b"
    ));
}

#[test]
fn request_rejects_unapproved_profiles_unpinned_images_unsafe_paths_and_zero_limits()
-> Result<(), Box<dyn std::error::Error>> {
    let mut value = request(OjCheckerKind::Exact);
    assert!(value.validate().is_ok());

    value.toolchain_profile = "teacher-selected".to_owned();
    assert_eq!(
        error_diagnostic(value.validate())?,
        "LW_OJ_TOOLCHAIN_UNAPPROVED"
    );

    let mut value = request(OjCheckerKind::Exact);
    value.toolchain_image_digest = "gcc:latest".to_owned();
    assert_eq!(
        error_diagnostic(value.validate())?,
        "LW_OJ_IMAGE_IDENTITY_INVALID"
    );

    let mut value = request(OjCheckerKind::Exact);
    value.source.path = "../private.cpp".to_owned();
    assert_eq!(error_diagnostic(value.validate())?, "LW_OJ_PATH_UNSAFE");

    let mut value = request(OjCheckerKind::Exact);
    value.limits.output_bytes = 0;
    assert_eq!(error_diagnostic(value.validate())?, "LW_OJ_LIMIT_INVALID");

    let mut compile = request(OjCheckerKind::Exact);
    compile.phase = OjExecutionPhase::Compile;
    compile.checker = None;
    compile.cases.clear();
    compile.evaluator_identity = None;
    compile.score_max_points = 0;
    assert!(compile.validate().is_ok());

    compile.checker = Some(OjCheckerKind::Exact);
    assert_eq!(
        error_diagnostic(compile.validate())?,
        "LW_OJ_EXECUTION_PLAN_INVALID"
    );
    Ok(())
}

#[test]
fn deterministic_aggregation_covers_six_required_terminal_states()
-> Result<(), Box<dyn std::error::Error>> {
    let value = request(OjCheckerKind::Exact);
    let accepted = aggregate_case_evidence(
        &value,
        &[
            evidence("basic", OjCaseStatus::Accepted, 40),
            evidence("edge", OjCaseStatus::Accepted, 60),
        ],
    )?;
    assert_eq!(accepted.status, OjTerminalStatus::Accepted);
    assert_eq!((accepted.awarded_points, accepted.max_points), (80, 80));

    for (case_status, terminal) in [
        (OjCaseStatus::WrongAnswer, OjTerminalStatus::WrongAnswer),
        (
            OjCaseStatus::TimeLimitExceeded,
            OjTerminalStatus::TimeLimitExceeded,
        ),
        (
            OjCaseStatus::MemoryLimitExceeded,
            OjTerminalStatus::MemoryLimitExceeded,
        ),
        (
            OjCaseStatus::OutputLimitExceeded,
            OjTerminalStatus::OutputLimitExceeded,
        ),
    ] {
        let aggregate = aggregate_case_evidence(
            &value,
            &[
                evidence("basic", case_status, 0),
                evidence("edge", OjCaseStatus::Accepted, 60),
            ],
        )?;
        assert_eq!(aggregate.status, terminal);
        assert_eq!(aggregate.awarded_points, 48);
        assert_eq!(aggregate.max_points, 80);
    }

    let compile_error = OjTerminalStatus::CompileError;
    assert_eq!(compile_error.diagnostic_code(), "LW_OJ_COMPILE_ERROR");
    Ok(())
}

#[test]
fn compile_step_evidence_accepts_only_a_matching_process_outcome()
-> Result<(), Box<dyn std::error::Error>> {
    let mut request = request(OjCheckerKind::Exact);
    request.phase = OjExecutionPhase::Compile;
    request.checker = None;
    request.cases.clear();
    request.evaluator_identity = None;
    request.score_max_points = 0;
    let aggregate = evaluation_service::oj::OjAggregate {
        status: OjTerminalStatus::Accepted,
        awarded_points: 0,
        max_points: 0,
        passed_cases: 0,
        total_cases: 0,
        diagnostic_code: "LW_OJ_ACCEPTED".to_owned(),
    };
    let mut execution = OjExecutionEvidence {
        schema_version: OJ_EVIDENCE_SCHEMA_VERSION.to_owned(),
        run_id: request.run_id,
        step_run_id: request.step_run_id,
        attempt_id: request.attempt_id,
        trace_id: request.trace_id.clone(),
        request_sha256: request.request_sha256()?,
        submission_identity: request.submission_identity,
        evaluator_identity: request.evaluator_identity,
        toolchain_profile: request.toolchain_profile.clone(),
        toolchain_image_digest: request.toolchain_image_digest.clone(),
        terminal_status: OjTerminalStatus::Accepted,
        diagnostic_code: "LW_OJ_ACCEPTED".to_owned(),
        compile: process_evidence(0),
        cases: Vec::new(),
        aggregate,
    };
    execution.validate_for(&request)?;

    execution.compile = process_evidence(1);
    assert_eq!(
        error_diagnostic(execution.validate_for(&request))?,
        "LW_OJ_EVIDENCE_INVALID"
    );

    execution.terminal_status = OjTerminalStatus::CompileError;
    execution.diagnostic_code = "LW_OJ_COMPILE_ERROR".to_owned();
    execution.aggregate.status = OjTerminalStatus::CompileError;
    execution.aggregate.diagnostic_code = "LW_OJ_COMPILE_ERROR".to_owned();
    execution.validate_for(&request)?;
    Ok(())
}

#[test]
fn aggregation_rejects_duplicate_missing_unknown_and_forged_score_evidence()
-> Result<(), Box<dyn std::error::Error>> {
    let value = request(OjCheckerKind::Token);

    for evidence in [
        vec![
            evidence("basic", OjCaseStatus::Accepted, 40),
            evidence("basic", OjCaseStatus::Accepted, 40),
        ],
        vec![evidence("basic", OjCaseStatus::Accepted, 40)],
        vec![
            evidence("basic", OjCaseStatus::Accepted, 40),
            evidence("unknown", OjCaseStatus::Accepted, 60),
        ],
        vec![
            evidence("basic", OjCaseStatus::WrongAnswer, 40),
            evidence("edge", OjCaseStatus::Accepted, 60),
        ],
    ] {
        assert_eq!(
            error_diagnostic(aggregate_case_evidence(&value, &evidence))?,
            "LW_OJ_EVIDENCE_INVALID"
        );
    }
    Ok(())
}

#[test]
fn strict_json_rejects_unknown_fields_and_non_v7_run_identity()
-> Result<(), Box<dyn std::error::Error>> {
    let value = request(OjCheckerKind::Exact);
    let mut json = serde_json::to_value(value)?;
    json["unexpected"] = serde_json::json!(true);
    assert!(serde_json::from_value::<OjExecutionRequest>(json).is_err());

    let mut json = serde_json::to_value(request(OjCheckerKind::Exact))?;
    json["runId"] = serde_json::json!(Uuid::nil());
    let parsed: OjExecutionRequest = serde_json::from_value(json)?;
    assert_eq!(
        error_diagnostic(parsed.validate())?,
        "LW_OJ_IDENTITY_INVALID"
    );

    assert!(Sha256Digest::from_str(&"a".repeat(64)).is_ok());
    Ok(())
}

#[test]
fn student_projection_excludes_private_cases_commands_logs_and_evidence_identity()
-> Result<(), Box<dyn std::error::Error>> {
    let request = request(OjCheckerKind::Exact);
    let cases = vec![
        evidence("basic", OjCaseStatus::Accepted, 40),
        evidence("edge", OjCaseStatus::WrongAnswer, 0),
    ];
    let aggregate = aggregate_case_evidence(&request, &cases)?;
    let execution = OjExecutionEvidence {
        schema_version: OJ_EVIDENCE_SCHEMA_VERSION.to_owned(),
        run_id: request.run_id,
        step_run_id: request.step_run_id,
        attempt_id: request.attempt_id,
        trace_id: request.trace_id.clone(),
        request_sha256: request.request_sha256()?,
        submission_identity: request.submission_identity,
        evaluator_identity: request.evaluator_identity,
        toolchain_profile: request.toolchain_profile.clone(),
        toolchain_image_digest: request.toolchain_image_digest.clone(),
        terminal_status: aggregate.status,
        diagnostic_code: aggregate.diagnostic_code.clone(),
        compile: OjProcessEvidence {
            exit_code: Some(0),
            signal: None,
            stdout_sha256: digest(b"compiler stdout"),
            stdout_bytes: 15,
            stderr_sha256: digest(b"compiler stderr"),
            stderr_bytes: 15,
            duration_milliseconds: 10,
            peak_memory_bytes: None,
            timed_out: false,
            output_exceeded: false,
        },
        cases,
        aggregate,
    };
    execution.validate_for(&request)?;

    let public = serde_json::to_string(&execution.student_result())?;
    for private_marker in [
        "basic",
        "edge",
        "cases/",
        "main.cpp",
        "compiler",
        "toolchain",
        "sha256",
        "runId",
        "attemptId",
        "traceId",
    ] {
        assert!(!public.contains(private_marker));
    }
    assert!(public.contains("wrong_answer"));
    assert!(public.contains("\"awardedPoints\":32"));
    Ok(())
}
