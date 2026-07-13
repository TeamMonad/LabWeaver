//! Contract and negative tests for `EvaluationSpec` v1alpha1.

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

use evaluation_domain::{
    AggregationKind, CheckerSpec, CollectorSpec, DeterministicRunnerSpec, EvaluationSpec,
    EvaluationSpecError, EvaluationStep, GoalReview, ProgramPhase, evaluation_spec_schema,
    goal_review_schema,
};

const OJ_FIXTURE: &str = include_str!("fixtures/oj/evaluation.yaml");
const LINUX_FIXTURE: &str = include_str!("fixtures/linux/evaluation.yaml");

#[test]
fn oj_and_linux_examples_pass_schema_and_semantic_validation() -> Result<(), Box<dyn Error>> {
    let schema = evaluation_spec_schema()?;
    let validator = jsonschema::validator_for(&schema)?;

    for yaml in [OJ_FIXTURE, LINUX_FIXTURE] {
        let yaml_value: serde_yaml::Value = serde_yaml::from_str(yaml)?;
        let json_value = serde_json::to_value(yaml_value)?;
        if !validator.is_valid(&json_value) {
            return Err("fixture failed generated JSON Schema validation".into());
        }
        EvaluationSpec::from_yaml(yaml)?;
    }
    Ok(())
}

#[test]
fn external_consumers_can_read_the_domain_decomposition() -> Result<(), Box<dyn Error>> {
    let spec = EvaluationSpec::from_yaml(OJ_FIXTURE)?;
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
    let approval_bypass = OJ_FIXTURE.replacen(
        "teacherApprovalRequiredForRelease: true",
        "teacherApprovalRequiredForRelease: false",
        1,
    );
    let unsafe_runner_path = OJ_FIXTURE.replacen(
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
    let schema_directory = workspace_root().join("schemas/evaluation");
    assert_schema_matches(
        &schema_directory.join("evaluation-spec.v1alpha1.schema.json"),
        &evaluation_spec_schema()?,
    )?;
    assert_schema_matches(
        &schema_directory.join("goal-review.v1.schema.json"),
        &goal_review_schema()?,
    )
}

#[test]
fn duplicate_step_ids_fail_fast() -> Result<(), Box<dyn Error>> {
    let invalid = OJ_FIXTURE.replacen("id: compile", "id: preflight", 1);
    let error = spec_error(&invalid)?;
    assert_eq!(error.diagnostic_code(), "LW_EVAL_STEP_DUPLICATE");
    Ok(())
}

#[test]
fn missing_dependencies_fail_fast() -> Result<(), Box<dyn Error>> {
    let invalid = OJ_FIXTURE.replacen("dependsOn: [compile]", "dependsOn: [missing]", 1);
    let error = spec_error(&invalid)?;
    assert_eq!(error.diagnostic_code(), "LW_EVAL_DEPENDENCY_MISSING");
    Ok(())
}

#[test]
fn dependency_cycles_fail_fast() -> Result<(), Box<dyn Error>> {
    let invalid = OJ_FIXTURE.replacen(
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
    let invalid = OJ_FIXTURE.replacen("maxScore: 80", "maxScore: 79", 1);
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
apiVersion: evaluation.labweaver.io/v1alpha1
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
  steps:
    - role: score
      id: score-a
      runner:
        kind: file_assertion
        requiredFiles: [src/main.cpp]
      checker:
        kind: exact
      score:
        max: 4294967295
      failurePolicy: continue
    - role: score
      id: score-b
      runner:
        kind: file_assertion
        requiredFiles: [src/main.cpp]
      checker:
        kind: exact
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
        OJ_FIXTURE
            .replacen("max: 80", "max: 0", 1)
            .replacen("maxScore: 80", "maxScore: 0", 1);
    let error = spec_error(&invalid)?;
    assert_eq!(error.diagnostic_code(), "LW_EVAL_STEP_CONFIG_INVALID");
    Ok(())
}

#[test]
fn advisory_steps_reject_protected_score_fields() -> Result<(), Box<dyn Error>> {
    let invalid = OJ_FIXTURE.replacen(
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
    let invalid = OJ_FIXTURE.replacen(
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
    let invalid = OJ_FIXTURE.replacen(
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
    let invalid = LINUX_FIXTURE.replacen("ansible.builtin.stat", "ansible.builtin.shell", 1);
    let error = spec_error(&invalid)?;
    assert_eq!(error.diagnostic_code(), "LW_EVAL_STEP_CONFIG_INVALID");
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
fn submission_paths_cannot_escape_the_snapshot_root() -> Result<(), Box<dyn Error>> {
    let invalid = OJ_FIXTURE.replacen("src/main.cpp", "../secret", 1);
    let error = spec_error(&invalid)?;
    assert_eq!(error.diagnostic_code(), "LW_EVAL_SUBMISSION_PATH_UNSAFE");
    Ok(())
}

#[test]
fn file_assertion_paths_cannot_escape_the_submission_root() -> Result<(), Box<dyn Error>> {
    let invalid = OJ_FIXTURE.replacen(
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
    let invalid = OJ_FIXTURE.replacen("input: src/main.cpp", "input: ../secret", 1);
    let error = spec_error(&invalid)?;
    assert_eq!(error.diagnostic_code(), "LW_EVAL_SUBMISSION_PATH_UNSAFE");
    Ok(())
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
