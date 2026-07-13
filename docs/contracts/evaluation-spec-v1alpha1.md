# EvaluationSpec v1alpha1 Contract

Status: implemented in the current worktree, pending human contract and security review.

`EvaluationSpec` is the versioned YAML contract that describes how LabWeaver collects a submission,
runs deterministic evaluation steps, aggregates scores and presents advisory review. The Rust types
in `crates/evaluation-domain` are the source of truth; checked-in JSON Schema files are generated from
those types and protected by a drift test.

External crates consume the validated decomposition through immutable getters on `EvaluationSpec`,
`EvaluationBody`, step structs, aggregation and review policy. Runner and Checker enums are public for
type-safe dispatch, while root fields remain private so callers cannot mutate a validated document or
construct an unchecked root value. Public Serde `Deserialize` implementations use private wire types
and run the same semantic validation, so direct `serde_yaml`/`serde_json` calls cannot bypass the
validated root boundary.

## Supported P0 shapes

- OJ: `workspace_snapshot`, `file_assertion`, approved-profile `program`, `exact`, `token` and
  `exit_code`.
- Linux: `system_facts`, read-only approved-profile `ansible_probe`, `service_state` and
  `exit_code`.
- Advisory: `llm_review` only. Advisory steps and `goal-review/v1` structurally have no scoring
  fields and reject unknown fields.
- Aggregation: `deterministic_sum` over `role: score` steps. Gates must reference `role: gate`
  steps.
- Release policy: `teacherApprovalRequiredForRelease` is a required constant `true`; neither an
  Agent nor a teacher-authored candidate can disable the production approval gate.

The contract does not accept raw shell commands. Program execution binds an approved toolchain
profile, and Linux probes accept only the v1alpha1 Ansible module allowlist. These files describe
candidates only; they do not approve, publish or execute generated content.

## Fail-fast validation

| Diagnostic                           | Meaning                                                              |
|--------------------------------------|----------------------------------------------------------------------|
| `LW_EVAL_SPEC_DOCUMENT_INVALID`      | YAML shape, enum, type or unknown field is invalid                   |
| `LW_EVAL_STEP_DUPLICATE`             | Step IDs are not unique                                              |
| `LW_EVAL_DEPENDENCY_MISSING`         | A dependency does not exist                                          |
| `LW_EVAL_DAG_CYCLE`                  | Step dependencies contain a cycle                                    |
| `LW_EVAL_SUBMISSION_PATH_UNSAFE`     | A Collector, Runner or LLM-readable path escapes the submission root |
| `LW_EVAL_STEP_CONFIG_INVALID`        | Runner, score or allowlist configuration is invalid                  |
| `LW_EVAL_AGGREGATION_SCORE_MISMATCH` | Declared maximum differs from deterministic step totals              |
| `LW_EVAL_AGGREGATION_SCORE_OVERFLOW` | Summing deterministic step maxima exceeds the `u32` contract range   |
| `LW_EVAL_AGGREGATION_GATE_INVALID`   | An aggregation gate does not reference a Gate step                   |
| `LW_EVAL_TEACHER_APPROVAL_REQUIRED`  | Release policy attempts to disable mandatory teacher approval        |
| `LW_EVAL_LLM_REVIEW_INVALID`         | Advisory output is malformed or contains protected fields            |

## Artifacts and verification

- `schemas/evaluation/evaluation-spec.v1alpha1.schema.json`
- `schemas/evaluation/goal-review.v1.schema.json`
- `crates/evaluation-domain/tests/fixtures/oj/evaluation.yaml`
- `crates/evaluation-domain/tests/fixtures/linux/evaluation.yaml`

```sh
cargo run --locked -p evaluation-domain --example export_schema -- schemas/evaluation
cargo test --locked -p evaluation-domain
cargo clippy -p evaluation-domain --all-targets --all-features -- -D warnings
```

This is E1 contract evidence. It does not prove a Runner, Kubernetes Job, VM, database, message path,
approval path or production evaluation run.
