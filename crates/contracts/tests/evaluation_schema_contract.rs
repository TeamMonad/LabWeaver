//! Contract and negative tests for `EvaluationSpec` v1.

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use contracts::evaluation::{
    AggregationKind, CheckerSpec, CollectorSpec, DeterministicRunnerSpec, EvaluationSpec,
    EvaluationSpecError, EvaluationStep, GoalReview, ProgramPhase, evaluation_spec_schema,
    goal_review_schema,
};

const OJ_FIXTURE: &str = include_str!("fixtures/evaluation/oj/evaluation.yaml");
const LINUX_FIXTURE: &str = include_str!("fixtures/evaluation/linux/evaluation.yaml");

fn oj_fixture() -> String {
    OJ_FIXTURE.replace("\r\n", "\n")
}

fn linux_fixture() -> String {
    LINUX_FIXTURE.replace("\r\n", "\n")
}

#[test]
fn oj_and_linux_examples_pass_schema_and_semantic_validation() -> Result<(), Box<dyn Error>> {
    let schema = evaluation_spec_schema()?;
    let validator = jsonschema::validator_for(&schema)?;

    for yaml in [oj_fixture(), linux_fixture()] {
        let yaml_value: serde_yaml::Value = serde_yaml::from_str(&yaml)?;
        let json_value = serde_json::to_value(yaml_value)?;
        if !validator.is_valid(&json_value) {
            return Err("fixture failed generated JSON Schema validation".into());
        }
        EvaluationSpec::from_yaml(&yaml)?;
    }
    Ok(())
}

#[test]
fn external_consumers_can_read_the_domain_decomposition() -> Result<(), Box<dyn Error>> {
    let spec = EvaluationSpec::from_yaml(&oj_fixture())?;
    assert_eq!(spec.metadata().name(), "shortest-path-v1");
    assert_eq!(spec.metadata().version(), "1.0.0");

    let CollectorSpec::WorkspaceSnapshot {
        include, max_bytes, ..
    } = spec.body().submission().collector()
    else {
        return Err("expected workspace collector".into());
    };
    assert!(include.iter().any(|path| path == "src/main.cpp"));
    assert_eq!(*max_bytes, 20_971_520);
    assert_eq!(
        spec.body().submission().llm_readable(),
        &["src/main.cpp".to_owned(), "report.md".to_owned()]
    );

    let compile = spec
        .body()
        .steps()
        .iter()
        .find(|step| step.id() == "compile")
        .ok_or("compile step is missing")?;
    let EvaluationStep::Gate(compile) = compile else {
        return Err("compile must be a Gate step".into());
    };
    let DeterministicRunnerSpec::Program {
        toolchain_profile,
        phase,
        input,
        limits,
        ..
    } = compile.runner()
    else {
        return Err("compile must use the program Runner".into());
    };
    assert_eq!(toolchain_profile, "cpp17-approved-v1");
    assert_eq!(*phase, ProgramPhase::Compile);
    assert_eq!(input, "src/main.cpp");
    assert_eq!(limits.wall_time_seconds(), 30);
    assert!(matches!(
        compile.checker(),
        CheckerSpec::ExitCode { expected: 0 }
    ));

    assert_eq!(
        spec.body().aggregation().kind(),
        AggregationKind::DeterministicSum
    );
    assert_eq!(spec.body().aggregation().max_score(), 80);
    assert!(spec.body().review().teacher_approval_required_for_release());
    Ok(())
}

#[test]
fn public_deserialize_cannot_bypass_evaluation_semantics() -> Result<(), Box<dyn Error>> {
    let approval_bypass = oj_fixture().replacen(
        "teacherApprovalRequiredForRelease: true",
        "teacherApprovalRequiredForRelease: false",
        1,
    );
    let unsafe_runner_path = oj_fixture().replacen(
        "requiredFiles:\n          - src/main.cpp",
        "requiredFiles:\n          - ../secret",
        1,
    );

    for invalid in [&approval_bypass, &unsafe_runner_path] {
        if serde_yaml::from_str::<EvaluationSpec>(invalid).is_ok() {
            return Err("public Deserialize bypassed EvaluationSpec semantics".into());
        }
    }
    Ok(())
}

#[test]
fn checked_in_schemas_match_rust_types() -> Result<(), Box<dyn Error>> {
    let schema_directory = workspace_root().join("schemas/contracts/v1");
    assert_schema_matches(
        &schema_directory.join("evaluation-spec.schema.json"),
        &evaluation_spec_schema()?,
    )?;
    assert_schema_matches(
        &schema_directory.join("goal-review.schema.json"),
        &goal_review_schema()?,
    )
}

#[test]
fn duplicate_step_ids_fail_fast() -> Result<(), Box<dyn Error>> {
    let invalid = oj_fixture().replacen("id: compile", "id: preflight", 1);
    let error = spec_error(&invalid)?;
    assert_eq!(error.diagnostic_code(), "LW_EVAL_STEP_DUPLICATE");
    Ok(())
}

#[test]
fn missing_dependencies_fail_fast() -> Result<(), Box<dyn Error>> {
    let invalid = oj_fixture().replacen("dependsOn: [compile]", "dependsOn: [missing]", 1);
    let error = spec_error(&invalid)?;
    assert_eq!(error.diagnostic_code(), "LW_EVAL_DEPENDENCY_MISSING");
    Ok(())
}

#[test]
fn dependency_cycles_fail_fast() -> Result<(), Box<dyn Error>> {
    let invalid = oj_fixture().replacen(
        "id: preflight\n      runner:",
        "id: preflight\n      dependsOn: [correctness]\n      runner:",
        1,
    );
    let error = spec_error(&invalid)?;
    assert_eq!(error.diagnostic_code(), "LW_EVAL_DAG_CYCLE");
    Ok(())
}

#[test]
fn aggregation_mismatch_fails_fast() -> Result<(), Box<dyn Error>> {
    let invalid = oj_fixture().replacen("maxScore: 80", "maxScore: 79", 1);
    let error = spec_error(&invalid)?;
    assert_eq!(
        error.diagnostic_code(),
        "LW_EVAL_AGGREGATION_SCORE_MISMATCH"
    );
    Ok(())
}

#[test]
fn deterministic_score_overflow_fails_with_a_stable_diagnostic() -> Result<(), Box<dyn Error>> {
    let invalid = r#"
apiVersion: evaluation.labweaver.io/v1
kind: EvaluationSpec
metadata:
  name: score-overflow
  version: "1.0.0"
spec:
  submission:
    collector:
      kind: workspace_snapshot
      include: [src/main.cpp]
      maxBytes: 1024
    llmReadable: []
  steps:
    - role: score
      id: score-a
      runner:
        kind: file_assertion
        requiredFiles: [src/main.cpp]
      checker:
        kind: exit_code
        expected: 0
      score:
        max: 4294967295
      failurePolicy: continue
    - role: score
      id: score-b
      runner:
        kind: file_assertion
        requiredFiles: [src/main.cpp]
      checker:
        kind: exit_code
        expected: 0
      score:
        max: 4294967295
      failurePolicy: continue
  aggregation:
    kind: deterministic_sum
    maxScore: 4294967294
    gates: []
  review:
    teacherApprovalRequiredForRelease: true
    forceManualWhen: []
"#;
    let error = spec_error(invalid)?;
    assert_eq!(
        error.diagnostic_code(),
        "LW_EVAL_AGGREGATION_SCORE_OVERFLOW"
    );
    Ok(())
}

#[test]
fn zero_value_score_steps_are_rejected() -> Result<(), Box<dyn Error>> {
    let invalid =
        oj_fixture()
            .replacen("max: 80", "max: 0", 1)
            .replacen("maxScore: 80", "maxScore: 0", 1);
    let error = spec_error(&invalid)?;
    assert_eq!(error.diagnostic_code(), "LW_EVAL_STEP_CONFIG_INVALID");
    Ok(())
}

#[test]
fn advisory_steps_reject_protected_score_fields() -> Result<(), Box<dyn Error>> {
    let invalid = oj_fixture().replacen(
        "      failurePolicy: continue_advisory",
        "      score:\n        max: 100\n      failurePolicy: continue_advisory",
        1,
    );
    let error = spec_error(&invalid)?;
    assert_eq!(error.diagnostic_code(), "LW_EVAL_SPEC_DOCUMENT_INVALID");
    Ok(())
}

#[test]
fn release_policy_cannot_disable_teacher_approval() -> Result<(), Box<dyn Error>> {
    let invalid = oj_fixture().replacen(
        "teacherApprovalRequiredForRelease: true",
        "teacherApprovalRequiredForRelease: false",
        1,
    );

    let schema = evaluation_spec_schema()?;
    let validator = jsonschema::validator_for(&schema)?;
    let yaml_value: serde_yaml::Value = serde_yaml::from_str(&invalid)?;
    let json_value = serde_json::to_value(yaml_value)?;
    if validator.is_valid(&json_value) {
        return Err("JSON Schema unexpectedly allowed teacher approval bypass".into());
    }

    let error = spec_error(&invalid)?;
    assert_eq!(error.diagnostic_code(), "LW_EVAL_TEACHER_APPROVAL_REQUIRED");
    Ok(())
}

#[test]
fn runner_contract_rejects_unknown_command_fields() -> Result<(), Box<dyn Error>> {
    let invalid = oj_fixture().replacen(
        "        input: src/main.cpp\n        limits:",
        "        input: src/main.cpp\n        command: [sh, -c, unsafe]\n        limits:",
        1,
    );
    let error = spec_error(&invalid)?;
    assert_eq!(error.diagnostic_code(), "LW_EVAL_SPEC_DOCUMENT_INVALID");
    Ok(())
}

#[test]
fn ansible_probe_rejects_modules_outside_the_allowlist() -> Result<(), Box<dyn Error>> {
    let invalid = linux_fixture().replacen("ansible.builtin.stat", "ansible.builtin.shell", 1);
    let error = spec_error(&invalid)?;
    assert_eq!(error.diagnostic_code(), "LW_EVAL_STEP_CONFIG_INVALID");
    Ok(())
}

#[test]
fn llm_readable_paths_must_be_frozen_by_the_collector() -> Result<(), Box<dyn Error>> {
    let invalid = oj_fixture().replacen(
        "    llmReadable:\n      - src/main.cpp\n      - report.md",
        "    llmReadable:\n      - src/main.cpp\n      - hidden.md",
        1,
    );
    let error = spec_error(&invalid)?;
    assert_eq!(
        error.diagnostic_code(),
        "LW_EVAL_LLM_READABLE_NOT_COLLECTED"
    );

    for excluded in ["report.md", "src"] {
        let invalid = oj_fixture().replacen("        - build", &format!("        - {excluded}"), 1);
        let error = spec_error(&invalid)?;
        assert_eq!(
            error.diagnostic_code(),
            "LW_EVAL_LLM_READABLE_NOT_COLLECTED",
            "LLM-readable path under excluded {excluded} was accepted"
        );
    }

    let system_facts_with_file =
        linux_fixture().replacen("    llmReadable: []", "    llmReadable: [report.md]", 1);
    let error = spec_error(&system_facts_with_file)?;
    assert_eq!(
        error.diagnostic_code(),
        "LW_EVAL_LLM_READABLE_NOT_COLLECTED"
    );
    Ok(())
}

#[test]
fn llm_review_include_must_be_a_subset_of_llm_readable() -> Result<(), Box<dyn Error>> {
    let invalid = oj_fixture().replacen(
        "        include:\n          - report.md\n          - src/main.cpp",
        "        include:\n          - hidden.md\n          - src/main.cpp",
        1,
    );
    let error = spec_error(&invalid)?;
    assert_eq!(error.diagnostic_code(), "LW_EVAL_LLM_INCLUDE_NOT_ALLOWED");
    Ok(())
}

#[test]
fn runner_checker_compatibility_matrix_is_fail_closed() -> Result<(), Box<dyn Error>> {
    let runners = [
        (
            "file_assertion",
            "        kind: file_assertion\n        requiredFiles: [src/main.cpp]",
            &["exit_code"][..],
        ),
        (
            "program_compile",
            "        kind: program\n        toolchainProfile: cpp17-approved-v1\n        phase: compile\n        input: src/main.cpp\n        limits:\n          wallTimeSeconds: 30\n          memoryBytes: 1024\n          outputBytes: 1024",
            &["exit_code"][..],
        ),
        (
            "program_test",
            "        kind: program\n        toolchainProfile: cpp17-approved-v1\n        phase: test\n        input: src/main.cpp\n        testGroups:\n          - name: basic\n            source: evaluator://tests/basic\n            maxPoints: 1\n        limits:\n          wallTimeSeconds: 30\n          memoryBytes: 1024\n          outputBytes: 1024",
            &["exact", "token", "json_schema"][..],
        ),
        (
            "ansible_probe",
            "        kind: ansible_probe\n        playbookProfile: linux-probe-v1\n        moduleAllowlist: [ansible.builtin.stat]\n        readOnly: true\n        assertions:\n          - fact: host.reachable\n            expected: true",
            &["exit_code", "json_schema", "service_state"][..],
        ),
    ];
    let checkers = [
        ("exact", "        kind: exact"),
        ("token", "        kind: token"),
        ("exit_code", "        kind: exit_code\n        expected: 0"),
        (
            "json_schema",
            "        kind: json_schema\n        schemaRef: evaluator://schemas/result-v1.json",
        ),
        (
            "service_state",
            "        kind: service_state\n        service: nginx\n        expected: active",
        ),
    ];

    for (runner_name, runner, allowed_checkers) in runners {
        for (checker_name, checker) in checkers {
            let result = EvaluationSpec::from_yaml(&compatibility_spec(runner, checker));
            if allowed_checkers.contains(&checker_name) {
                assert!(
                    result.is_ok(),
                    "{runner_name} + {checker_name} should be accepted: {result:?}"
                );
            } else {
                let Err(error) = result else {
                    return Err(format!(
                        "unsupported {runner_name} + {checker_name} pair was accepted"
                    )
                    .into());
                };
                assert_eq!(
                    error.diagnostic_code(),
                    "LW_EVAL_STEP_CONFIG_INVALID",
                    "unexpected diagnostic for {runner_name} + {checker_name}"
                );
            }
        }
    }
    Ok(())
}

#[test]
fn llm_review_output_rejects_protected_score_fields() -> Result<(), Box<dyn Error>> {
    let review = r#"{
        "schema_version": "goal-review/v1",
        "assessment": "met",
        "confidence": 0.9,
        "findings": [],
        "requires_teacher_attention": false,
        "score": 100
    }"#;
    let Err(error) = GoalReview::from_json(review) else {
        return Err("LLM review unexpectedly accepted score".into());
    };
    assert_eq!(error.diagnostic_code(), "LW_EVAL_LLM_REVIEW_INVALID");
    Ok(())
}

#[test]
fn public_deserialize_cannot_bypass_goal_review_semantics() -> Result<(), Box<dyn Error>> {
    let review = r#"{
        "schema_version": "goal-review/v1",
        "assessment": "met",
        "confidence": 2.0,
        "findings": [],
        "requires_teacher_attention": false
    }"#;

    let Err(error) = GoalReview::from_json(review) else {
        return Err("GoalReview::from_json accepted invalid confidence".into());
    };
    assert_eq!(error.diagnostic_code(), "LW_EVAL_LLM_CONFIDENCE_INVALID");

    if serde_json::from_str::<GoalReview>(review).is_ok() {
        return Err("public Deserialize bypassed GoalReview semantics".into());
    }

    let schema = goal_review_schema()?;
    let validator = jsonschema::validator_for(&schema)?;
    let value: serde_json::Value = serde_json::from_str(review)?;
    if validator.is_valid(&value) {
        return Err("GoalReview JSON Schema accepted invalid confidence".into());
    }
    Ok(())
}

#[test]
fn goal_review_evidence_is_bounded_and_validated_against_the_step() -> Result<(), Box<dyn Error>> {
    let valid = goal_review_json("report.md", 1, 3);
    GoalReview::from_json_against(&valid, &["report.md"])?;

    let Err(disallowed) = GoalReview::from_json_against(&valid, &["src/main.cpp"]) else {
        return Err("evidence outside the advisory include was accepted".into());
    };
    assert_eq!(
        disallowed.diagnostic_code(),
        "LW_EVAL_LLM_EVIDENCE_PATH_NOT_ALLOWED"
    );
    let mut empty_evidence: serde_json::Value = serde_json::from_str(&valid)?;
    empty_evidence["findings"][0]["evidence"] = serde_json::json!([]);

    for (review, diagnostic) in [
        (
            goal_review_json("../../secret", 1, 1),
            "LW_EVAL_LLM_EVIDENCE_PATH_UNSAFE",
        ),
        (
            goal_review_json("report.md", 0, 1),
            "LW_EVAL_LLM_EVIDENCE_RANGE_INVALID",
        ),
        (
            goal_review_json("report.md", 3, 2),
            "LW_EVAL_LLM_EVIDENCE_RANGE_INVALID",
        ),
        (
            valid.replacen("criterion-a", " ", 1),
            "LW_EVAL_LLM_FINDING_INVALID",
        ),
        (
            valid.replacen("suggestion-a", " ", 1),
            "LW_EVAL_LLM_FINDING_INVALID",
        ),
        (empty_evidence.to_string(), "LW_EVAL_LLM_FINDING_INVALID"),
    ] {
        let Err(error) = GoalReview::from_json(&review) else {
            return Err("invalid GoalReview evidence unexpectedly passed".into());
        };
        assert_eq!(error.diagnostic_code(), diagnostic);
    }
    Ok(())
}

#[test]
fn goal_review_counts_and_field_lengths_are_bounded() -> Result<(), Box<dyn Error>> {
    let evidence = serde_json::json!({
        "path": "report.md",
        "start_line": 1,
        "end_line": 1
    });
    let finding = serde_json::json!({
        "criterion": "criterion-a",
        "result": "met",
        "evidence": [evidence.clone()],
        "suggestion": "suggestion-a"
    });
    let base = serde_json::json!({
        "schema_version": "goal-review/v1",
        "assessment": "met",
        "confidence": 0.9,
        "findings": [finding.clone()],
        "requires_teacher_attention": false
    });

    let mut too_many_findings = base.clone();
    too_many_findings["findings"] = serde_json::Value::Array(vec![finding.clone(); 65]);
    let mut too_many_evidence = base.clone();
    too_many_evidence["findings"][0]["evidence"] = serde_json::Value::Array(vec![evidence; 17]);
    let mut long_criterion = base;
    long_criterion["findings"][0]["criterion"] = "x".repeat(1_025).into();

    for review in [too_many_findings, too_many_evidence, long_criterion] {
        let Err(error) = GoalReview::from_json(&review.to_string()) else {
            return Err("oversized GoalReview unexpectedly passed".into());
        };
        assert_eq!(error.diagnostic_code(), "LW_EVAL_LLM_LIMIT_EXCEEDED");
    }
    Ok(())
}

#[test]
fn submission_paths_cannot_escape_the_snapshot_root() -> Result<(), Box<dyn Error>> {
    let invalid = oj_fixture().replacen("src/main.cpp", "../secret", 1);
    let error = spec_error(&invalid)?;
    assert_eq!(error.diagnostic_code(), "LW_EVAL_SUBMISSION_PATH_UNSAFE");
    Ok(())
}

#[test]
fn collector_paths_reject_unimplemented_glob_semantics() -> Result<(), Box<dyn Error>> {
    let invalid = oj_fixture().replacen("        - build", "        - build/**", 1);
    let error = spec_error(&invalid)?;
    assert_eq!(error.diagnostic_code(), "LW_EVAL_SUBMISSION_PATH_UNSAFE");
    Ok(())
}

#[test]
fn file_assertion_paths_cannot_escape_the_submission_root() -> Result<(), Box<dyn Error>> {
    let invalid = oj_fixture().replacen(
        "requiredFiles:\n          - src/main.cpp",
        "requiredFiles:\n          - ../secret",
        1,
    );
    let error = spec_error(&invalid)?;
    assert_eq!(error.diagnostic_code(), "LW_EVAL_SUBMISSION_PATH_UNSAFE");
    Ok(())
}

#[test]
fn program_inputs_cannot_escape_the_submission_root() -> Result<(), Box<dyn Error>> {
    let invalid = oj_fixture().replacen("input: src/main.cpp", "input: ../secret", 1);
    let error = spec_error(&invalid)?;
    assert_eq!(error.diagnostic_code(), "LW_EVAL_SUBMISSION_PATH_UNSAFE");
    Ok(())
}

fn compatibility_spec(runner: &str, checker: &str) -> String {
    format!(
        r#"apiVersion: evaluation.labweaver.io/v1
kind: EvaluationSpec
metadata:
  name: compatibility-matrix
  version: "1.0.0"
spec:
  submission:
    collector:
      kind: workspace_snapshot
      include: [src/main.cpp]
      maxBytes: 1024
    llmReadable: []
  steps:
    - role: gate
      id: compatibility
      runner:
{runner}
      checker:
{checker}
      failurePolicy: stop
  aggregation:
    kind: deterministic_sum
    maxScore: 0
    gates: []
  review:
    teacherApprovalRequiredForRelease: true
    forceManualWhen: []
"#
    )
}

fn goal_review_json(path: &str, start_line: u32, end_line: u32) -> String {
    serde_json::json!({
        "schema_version": "goal-review/v1",
        "assessment": "met",
        "confidence": 0.9,
        "findings": [{
            "criterion": "criterion-a",
            "result": "met",
            "evidence": [{
                "path": path,
                "start_line": start_line,
                "end_line": end_line
            }],
            "suggestion": "suggestion-a"
        }],
        "requires_teacher_attention": false
    })
    .to_string()
}

fn spec_error(input: &str) -> Result<EvaluationSpecError, Box<dyn Error>> {
    match EvaluationSpec::from_yaml(input) {
        Ok(_) => Err("invalid EvaluationSpec unexpectedly passed".into()),
        Err(error) => Ok(error),
    }
}

fn assert_schema_matches(path: &Path, expected: &serde_json::Value) -> Result<(), Box<dyn Error>> {
    let actual: serde_json::Value = serde_json::from_str(&fs::read_to_string(path)?)?;
    if actual != *expected {
        return Err(format!("generated schema differs from {}", path.display()).into());
    }
    Ok(())
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}
