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

## Infrastructure automation

| Layer | Required evidence | Failure condition |
| --- | --- | --- |
| Static | `ansible-lint`, syntax check, YAML parsing | invalid task, unresolved template, unpinned component |
| Preflight | private inventory, Vault, interfaces, SELinux, KVM, NFS reachability | any missing prerequisite blocks deploy |
| Idempotency | two consecutive `ansible-deploy` runs | second run has an unexpected change or error |
| Storage | Local Path RWO and NFS RWX cross-worker write/read | PVC is not Bound or data is not shared |
| Runtime | KubeVirt VM start, console, stop/start, cleanup | no hardware KVM-backed Running VMI |
| Network | Cilium connectivity and internal Gateway route | failed suite or unprogrammed Gateway |
| Recovery | etcd snapshot plus `etcdutl snapshot status` | snapshot cannot be validated |

## Non-cluster CI evidence

Linux CI runs fixed Ansible dependencies, lint, syntax, fictional encrypted
Vault loading, mandatory-preflight chain checks, and storage safety fixtures.
These checks prove only E1/E2 controller behavior; they do not replace E3
acceptance against the target cluster.
## Access trust-boundary verification (planned)

ACCESS-01a documents the required test contract; it does not supply executable authorization evidence. The implementation suite must cover valid and invalid OIDC identity, enrollment eligibility, Active-device expansion and removal, cross-user endpoint denial, unsupported protocol, missing/expired/revoked parent or direct grant, endpoint IP reuse, short-lived VNC credential scope, handoff-token replay/expiry, and no partial authorization state after a failed decision.

Contract and integration tests must prove Router-first sequencing, default deny, exact device/IP/port scope, matching Headscale and Router receipts before activation, and no activation when either enforcement action fails or is stale. Revocation tests must prove that the Router blocks the affected flow and clears its connection state within 60 seconds while another valid grant to the same VM remains usable. Endpoint isolation and VM stop tests must prove that escalation is audited and VM stop is refused while any valid grant remains.

Deployed verification must prove direct native SSH/VNC, browser SSH/VNC through the Guacamole handoff path, absence of public or CIDR-wide exposure, and safe traces/audit diagnostics without credentials or session payloads. Kubernetes NetworkPolicy is not accepted as the direct-session containment proof because established-connection behavior is implementation-defined.
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
