# Test Plan

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

## Linux Nginx material-contract gates

```sh
python examples/linux-nginx/verify_material_contract.py --self-test
cargo test --locked -p evaluation-domain
```

The Python validator proves only public material-package integrity: SHA-256
records, required HTML markers, candidate submission constraints, controlled
material boundary, the normal target mapping, and the two declared negative
mappings. It also exercises missing public material, altered template,
restricted content, and oversized report failures. The `submission.yaml` and
`LW_LINUX_LAB_*` identifiers are candidate/planned contract material, not a
runtime schema or implemented diagnostic. The Rust test retains the existing
EvaluationSpec contract and does not validate HTTP, TCP/80, VM, SSH, Ansible, or
real Probe behavior. Those remain blocked pending B's approved profile and a
real KubeVirt E3 run.
## Agent Core gates

```sh
cargo test --locked -p agent-core
cargo clippy -p agent-core --all-targets --all-features -- -D warnings
```

State tests cover the valid path, atomic illegal-transition rejection, cancellation, bounded repair,
execution approval, final release approval and fail-fast entry to `Failed` from every non-terminal
state while preserving the root-cause diagnostic. Registry tests cover explicit dispatch, missing and
duplicate bindings, missing implementations, version/risk/capability mismatch, input Schema
rejection, masked untrusted payloads, approval, Tool failure and incompatible output version/Schema.
Success and every dispatch rejection/failure assert payload-free audit identity, hashes, outcome and
diagnostic. Elevated/high-risk dispatch is fail-closed until the approval evidence contract is
reviewed. A cross-module regression test propagates `LW_AGENT_TOOL_EXECUTION_FAILED` from Tool
dispatch into the Agent transition record. Pending Tool tests prove bound wall timeout, cooperative
cancellation and exactly one attempt per Registry call. Implementation failures accept only a closed
failure-code enum; a regression test passes a secret marker as Tool input and proves neither public
error Display nor its source chain contains the marker. All implementation failures normalize to
`LW_AGENT_TOOL_EXECUTION_FAILED`. This is partial E1 evidence; filesystem/network/runtime
permissions, approved dispatch and durable idempotency reservation/replay are not implemented.
Fixture Backend belongs to AG-01b. No Agent Service, database, NATS, LLM, build, Environment or
production execution path is exercised.

## Governance verification

Read back Milestones, Labels, branch protection, Sprint parents, sub-issues and Project fields through GitHub APIs. The verified governance result is 20 Project items and 15 P0 items with `Workflow Status=Ready`, plus Owner, Review Role, Sprint, Area, Codex Mode, Risk, SP and Evidence metadata.

## NATS Subject and delivery planned gates

[ADR 0003](../adr/0003-nats-subject-and-delivery-contract.md) and the
[NATS Event Contract v1](../contracts/nats-event-contract-v1.md) are E0 design
evidence only. Before a messaging path is marked implemented, tests must prove
that every deployed Subject has one catalogued state Owner and handling
purpose; malformed CloudEvents and Subject/type/dataschema mismatches are
rejected before mutation; and Owner state, idempotency record and local Outbox
row commit atomically.

The E2 integration suite must use real PostgreSQL and JetStream to prove
publish retry, duplicate/replay idempotency, stale and gap sequence blocking,
durable pull-consumer recovery, acknowledgement after durable state mutation,
declared finite retry/backoff behaviour and quarantine with a stable
diagnostic/alert after invalid input or retry exhaustion. Fixture-only tests do
not promote this evidence above E1.
