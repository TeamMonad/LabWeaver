# Test Plan

## Requirements-baseline traceability

`docs/requirements/acceptance-criteria.md` assigns AC-01 through AC-10-P1 to
US-01 through US-10. It is the testable requirements baseline, not evidence of
runtime completion. The matrix records the required target evidence and the
current `planned`, `blocked`, E0, or E1 state for each acceptance item.

P0 implementation issues must add the mapped requirement IDs to their contract,
integration, E2E, deployment, and release evidence. A test may close a
requirement only when its result is tied to the applicable build/deployment
identity at the evidence level named by the matrix. Fixture-only results do not
substitute for required real KubeVirt, Access, Resource, Evaluation, Ansible,
or Playwright proof.

## API-01a gates

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build --workspace --locked
cargo test --workspace --locked
```

The current test surface covers liveness, readiness, correlation headers, a stable not-found diagnostic and fail-fast binding configuration. It does not prove database, JetStream, OIDC, KubeVirt, MinIO, authorization or business behavior.

## EvaluationSpec v1alpha1 gates

```sh
cargo run --locked -p evaluation-domain --example export_schema -- schemas/evaluation
cargo test --locked -p evaluation-domain
cargo clippy -p evaluation-domain --all-targets --all-features -- -D warnings
```

The contract tests validate OJ and Linux examples against generated JSON Schema and semantic rules.
An external-crate integration test reads Metadata, Submission, Step, Runner, Checker, Aggregation and
Review values through the public immutable domain API. Direct public Serde deserialization is tested
against invalid `EvaluationSpec` and `GoalReview` documents to prove that wire APIs cannot bypass
semantic validation.
Negative coverage includes duplicate IDs, missing dependencies, DAG cycles, score mismatches, unsafe
Collector/Runner/LLM paths, protected LLM score fields, raw command fields, zero-value scores and Ansible modules outside
the allowlist. The JSON Schema and semantic reader both reject attempts to disable mandatory teacher
approval. Score aggregation overflow is exercised in both debug and release profiles. This is
E1 evidence and does not execute a Runner or production evaluation path.

## Governance verification

Read back Milestones, Labels, branch protection, Sprint parents, sub-issues and Project fields through GitHub APIs. The verified governance result is 20 Project items and 15 P0 items with `Workflow Status=Ready`, plus Owner, Review Role, Sprint, Area, Codex Mode, Risk, SP and Evidence metadata.
