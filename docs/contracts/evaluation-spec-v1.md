# EvaluationSpec v1 Contract

Status: implemented in the current worktree, pending human contract and security review.

`EvaluationSpec` is the versioned YAML contract that describes how LabWeaver collects a submission,
runs deterministic evaluation steps, aggregates scores and presents advisory review. The Rust types
in `crates/contracts/src/evaluation` are the source of truth; checked-in JSON Schema files are generated from
those types and protected by a drift test.

External crates consume the validated decomposition through immutable getters on `EvaluationSpec`,
`EvaluationBody`, step structs, aggregation and review policy. Runner and Checker enums are public for
type-safe dispatch, while root fields remain private so callers cannot mutate a validated document or
construct an unchecked root value. Public Serde `Deserialize` implementations use private wire types
and run the same semantic validation, so direct `serde_yaml`/`serde_json` calls cannot bypass the
validated root boundary.

## Supported P0 shapes

- OJ: `workspace_snapshot`, `file_assertion`, approved-profile `program`, `exact`, `token`,
  `json_schema` and `exit_code`.
- Linux: `system_facts`, read-only approved-profile `ansible_probe`, `service_state`, `json_schema`
  and `exit_code`.
- Advisory: `llm_review` only. Advisory steps and `goal-review/v1` structurally have no scoring
  fields and reject unknown fields.
- Aggregation: `deterministic_sum` over `role: score` steps. Gates must reference `role: gate`
  steps.
- Release policy: `teacherApprovalRequiredForRelease` is a required constant `true`; neither an
  Agent nor a teacher-authored candidate can disable the production approval gate.

The contract does not accept raw shell commands. Program execution binds an approved toolchain
profile, and Linux probes accept only the v1 Ansible module allowlist. These files describe
candidates only; they do not approve, publish or execute generated content.

`submission.llmReadable` is a mandatory explicit allowlist. Every entry must be a normalized safe
relative path and an exact member of the configured `workspace_snapshot.include`; an entry excluded
directly or through an excluded parent path is rejected. Every `llm_review.include` must in turn be
a subset of `llmReadable`. The current v1 shape binds one Collector, so `system_facts`
requires an empty `llmReadable` and cannot be paired with file evidence for an LLM Review. A future
multi-Collector shape requires a separately reviewed contract change.

Collector include/exclude values in this v1 contract are literal normalized paths. An exclude
also excludes descendants by path-segment prefix. Glob metacharacters such as `*`, `?`, `[]` and
`{}` are rejected until the Collector contract defines and implements one portable matching
algorithm; draft examples using `build/**` do not describe accepted runtime input.

Runner and Checker combinations are fail-closed:

| Runner / phase       | Accepted Checker                              |
|----------------------|-----------------------------------------------|
| `file_assertion`     | `exit_code`                                   |
| `program` / compile  | `exit_code`                                   |
| `program` / test     | `exact`, `token`, `json_schema`               |
| `ansible_probe`      | `exit_code`, `json_schema`, `service_state`   |

All other combinations return `LW_EVAL_STEP_CONFIG_INVALID` before execution.

`GoalReview::validate()` enforces normalized safe evidence paths, one-based non-reversed line
ranges, non-empty criterion/suggestion/evidence, at most 64 findings, at most 16 evidence locations
per finding, and bounded UTF-8 byte lengths. A consumer handling an Advisory Step must use
`GoalReview::from_json_against` or `GoalReview::validate_against` with that step's validated
`include`; intrinsic deserialization alone cannot supply step context.

## Evaluation control plane

`EvaluationSpec` remains immutable input. Runtime state is represented by
`EvaluationRelease`, `EvaluationRun` and `EvaluationStepRun` contracts generated
from `crates/contracts/src/evaluation/control.rs`. A release binds the approved
spec to source, package, configuration, migration catalog, digest-pinned runner
image and runtime artifact identities. A run additionally binds the release
identity, immutable FrozenSubmission content hash, source identity and trace ID.

Evaluation Service owns the PostgreSQL-authoritative lifecycle. The internal
mTLS API accepts release publication, run creation, readback, cancellation,
StepRun retry, cleanup verification and worker completion. Worker completion is
fenced by `(stepRunId, attempt, workerId, leaseToken)` and accepts only hash-only
evidence. Failed or cancelled StepRuns may remain pending cleanup; the aggregate
Run is completed only after cleanup is explicitly verified.

Control exposes the teacher release lifecycle through the Access BFF:

- `POST /api/v1/courses/{courseId}/evaluation-releases` accepts only
  `candidateId`, `candidateRevision`, `evaluationSpecSha256` and `approvalId`,
  plus `Idempotency-Key`. Control reloads the authoritative candidate, approval,
  active course policy and ProblemPackage and constructs the deployment-owned
  `EvaluationRuntimeIdentity`; no browser field can select an image, Provider or
  runtime hash.
- `GET /api/v1/courses/{courseId}/evaluation-releases` and the corresponding
  `/{releaseId}` route return Evaluation-owned state. The list is newest-first
  and uses an opaque, scope-bound cursor.
- `POST /api/v1/courses/{courseId}/evaluation-releases/{releaseId}/withdraw`
  requires `Idempotency-Key`, `If-Match`, the same expected revision in the
  request, and a stable diagnostic `reasonCode`. Evaluation persists the actor,
  trace, request hash and revision in an append-only withdrawal audit row.

The Access BFF routes student reads directly to Evaluation after checking the
session, student role and course membership:

- `GET /api/v1/courses/{courseId}/me/evaluation-results`
- `GET /api/v1/courses/{courseId}/me/evaluation-results/{runId}`

Only terminal runs owned by the current actor are visible. Successful runs
expose the deterministic total and public step scores. Failed and cancelled
runs expose state and stable diagnostics without a partial total. The projection
omits step IDs, dependencies, evidence hashes, runtime identity, private cases,
submission content and raw logs. Withdrawal prevents new runs but does not erase
historical terminal results.

Outbox events are payload-safe projections. They carry release, run or step-run
identity, revision, state, diagnostics, evidence hashes and cleanup flags, but
not submissions, raw logs, private evaluator inputs, object locators or numeric
score payloads.

## Fail-fast validation

| Diagnostic                           | Meaning                                                              |
|--------------------------------------|----------------------------------------------------------------------|
| `LW_EVAL_SPEC_DOCUMENT_INVALID`      | YAML shape, enum, type or unknown field is invalid                   |
| `LW_EVAL_STEP_DUPLICATE`             | Step IDs are not unique                                              |
| `LW_EVAL_DEPENDENCY_MISSING`         | A dependency does not exist                                          |
| `LW_EVAL_DAG_CYCLE`                  | Step dependencies contain a cycle                                    |
| `LW_EVAL_SUBMISSION_PATH_UNSAFE`     | A Collector, Runner or LLM-readable path escapes the submission root |
| `LW_EVAL_LLM_READABLE_NOT_COLLECTED` | `llmReadable` names a path absent from the frozen workspace snapshot  |
| `LW_EVAL_LLM_INCLUDE_NOT_ALLOWED`    | An Advisory Step requests a path outside `llmReadable`                |
| `LW_EVAL_STEP_CONFIG_INVALID`        | Runner, score or allowlist configuration is invalid                  |
| `LW_EVAL_AGGREGATION_SCORE_MISMATCH` | Declared maximum differs from deterministic step totals              |
| `LW_EVAL_AGGREGATION_SCORE_OVERFLOW` | Summing deterministic step maxima exceeds the `u32` contract range   |
| `LW_EVAL_AGGREGATION_GATE_INVALID`   | An aggregation gate does not reference a Gate step                   |
| `LW_EVAL_TEACHER_APPROVAL_REQUIRED`  | Release policy attempts to disable mandatory teacher approval        |
| `LW_EVAL_LLM_REVIEW_INVALID`         | Advisory output is malformed or contains protected fields            |
| `LW_EVAL_LLM_FINDING_INVALID`        | A finding has empty criterion, suggestion or evidence                 |
| `LW_EVAL_LLM_LIMIT_EXCEEDED`         | A finding, evidence list or bounded field exceeds its limit           |
| `LW_EVAL_LLM_EVIDENCE_PATH_UNSAFE`   | An evidence path is not a normalized safe relative path               |
| `LW_EVAL_LLM_EVIDENCE_PATH_NOT_ALLOWED` | Evidence is outside the current Advisory Step include              |
| `LW_EVAL_LLM_EVIDENCE_RANGE_INVALID` | Evidence uses zero or reversed line bounds                            |

## Open P1 contract decisions

- The v1 Ansible module allowlist does not prove HTTP behavior for the Linux golden path.
  Adding modules such as `uri`/`wait_for` or defining offline-verified immutable playbook bundles
  requires an architecture and security decision; the Linux fixture currently claims neither.
- `TestGroup.weight` remains a relative input without a frozen normalization, rounding, remainder or
  partial-case formula. It must not be used for production scoring until A approves one deterministic
  definition (or replaces it with per-group `maxPoints`).

## Artifacts and verification

- `schemas/contracts/v1/evaluation-spec.schema.json`
- `schemas/contracts/v1/evaluation-release.schema.json`
- `schemas/contracts/v1/student-evaluation-result.schema.json`
- `schemas/evaluation/goal-review.v1.schema.json`
- `crates/contracts/tests/fixtures/evaluation/oj/evaluation.yaml`
- `crates/contracts/tests/fixtures/evaluation/linux/evaluation.yaml`

```sh
cargo xtask contracts generate
cargo xtask test --suite contract
cargo clippy -p contracts --all-targets --all-features -- -D warnings
```

This is E1 contract evidence. It does not prove a Runner, Kubernetes Job, VM, database, message path,
approval path or production evaluation run.
