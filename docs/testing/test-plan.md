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

## PostgreSQL ownership and Migration planned gates

[ADR 0002](../adr/0002-postgresql-schema-and-migration-policy.md) and the
[Database Migration contract](../development/database-migrations.md) are E0
design evidence only. Before any persistence path is marked implemented, tests
must prove that runtime roles cannot execute DDL or cross-domain writes;
unknown, missing, ahead, behind and checksum-mismatched schema identities deny
readiness with stable diagnostics; and concurrent Migration Jobs serialize with
the domain advisory lock.

The integration suite must also prove repeat execution, immutable history,
build-identity report validation, partial-failure blocking, Expand/Contract
compatibility, reviewed forward repair, transaction-plus-domain-Outbox
atomicity, publisher retry, duplicate/replayed event handling, and
Control-owned `shared_audit` projection idempotency. A database container and
real Migration Job are required for this evidence; fixture-only checks cannot
promote it above E1.

The persistence suite must additionally prove that bootstrap revokes `PUBLIC`
and default privileges, each pool uses only its assigned role and fixed
`search_path`, and runtime history access is read-only. It must exercise the
global release lock, domain locks, crash-released lock recovery, same-release
retry, different-release blocking, transactional file failure and retained
partial-domain ledger evidence. It must prove process termination for
`DB_SCHEMA_MISSING`, `DB_SCHEMA_UNKNOWN`, `DB_SCHEMA_AHEAD`,
`DB_SCHEMA_INCOMPLETE` and `DB_SCHEMA_CHECKSUM_MISMATCH`, plus live/NotReady
503 behavior for `DB_SCHEMA_BEHIND` and `DB_SCHEMA_UNAVAILABLE`.

Audit tests must prove that a projection failure cannot change a committed
business transaction, that it records an unhealthy watermark, and that
idempotent replay/backfill plus dual-write watermark comparison supports a
future approved handoff to the audit-projection worker.
## Environment lifecycle v1alpha1 planned gates

The proposed `EnvironmentLifecycle v1alpha1` contract has no runtime test
evidence. Before implementation can be marked complete, its contract and
integration suites must prove the unified state transition matrix, invalid
transition rejection, revision conflicts, idempotency-key replay and payload
conflicts, bounded provider retry exhaustion, and explicit retry/reset paths.

They must also prove Experiment baseline-reset isolation, Work Active-Lease
requirements, reset acceptance only from `Ready`, `Stopped` and `Failed`, reset
target convergence, serialized Work configuration, configuration-failure
transition to `Failed`, access denial for any non-Ready or unhealthy endpoint,
grant revocation before reset/expiry/failure/delete cleanup, deletion
idempotency and sanitized `Deleted` tombstone evidence. Provider, Access,
Resource and KubeVirt evidence remains planned; this documentation is E0 only.
## Infrastructure automation

| Layer | Required evidence | Failure condition |
| --- | --- | --- |
| Static | `ansible-lint`, syntax check, YAML parsing | invalid task, unresolved template, unpinned component |
| Preflight | private inventory, Vault, interfaces, SELinux, KVM, NFS reachability | any missing prerequisite blocks deploy |
| Idempotency | two consecutive `cargo xtask deploy --infra --env <env> --yes` runs from the router worktree | second run has an unexpected change or error |
| Storage | Local Path RWO and NFS RWX cross-worker write/read | PVC is not Bound or data is not shared |
| Runtime | KubeVirt VM start, console, stop/start, cleanup | no hardware KVM-backed Running VMI |
| Network | Cilium connectivity and internal Gateway route | failed suite or unprogrammed Gateway |
| Recovery | etcd snapshot plus `etcdutl snapshot status` | snapshot cannot be validated |

### Harbor adopted-cluster gates (Issue #23)

The Harbor controller path is deliberately separate from `site.yml`. It must
refuse an unknown cluster, a changed existing Gateway, a conflicting VIP/DNS
record, a missing root-only secret locator, a missing storage class, an
unlocked chart/application version, or a configuration that would invoke a
bootstrap role. The controller must run only on the Linux router worktree and
must fail on Windows rather than selecting a different launcher or PATH fallback.

Required controller evidence is: router-side `cargo fmt --check`, Linux
`cargo clippy` and unit tests, `ansible-lint`, playbook syntax check, and a
check/diff before a real reconciliation. The real run must verify the dedicated
namespace/PVCs/Pods, Gateway `Programmed`, DNS, internal-CA TLS trust,
authenticated Harbor health and `labweaver-system` policy. A second run must
have no undeclared mutation. This remains E2 deployment evidence until the
declared push/pull/scan and recovery gates are separately performed.

TestFlight is emitted as a run-scoped, schema-validated
`InfrastructureTestFlightReport`; it binds the deployment manifest and cluster
UID, records cleanup, and remains `blocked` while Harbor OIDC is not configured.

OIDC is a non-goal for Issue #23. Its absence is a Release Gate blocker, not an
optional success path.

### VM-01a E3 run

Issue #15 has one current-run E3 artifact at source commit
`48cda8de9fef775f7578c90ca879356979df2706`:
`docs/testing/evidence/vm-01a-e3-20260713.md`. It proves only a run-scoped,
cleaned-up `local-path` RWO flow, cross-worker `nfs-rwx` RWX flow, fixed-digest
hardware-KVM VM start/console/stop/start flow, and an existing Cilium Gateway
request. A failed prerequisite, workload, lifecycle or cleanup makes the
verifier non-zero. This does not promote the Ansible deployment path, Access
path, application path or release readiness above their separately recorded
evidence levels.

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

## Playwright role-project configuration (Issue #9)

The requirements baseline is PR #36 at
`a9bc7a8ab013a35a846a4b428bad22ecc48eca1b`, merged into `develop` by
`0f80e4e9c4b2334d4a833d1fb6a2263ecc3dda9a`. The integration baseline for this
worktree is `develop` commit `8ec186599f82afeab7ff5bed346c844ce7f923d1`; it is
not a replacement for the PR #36 requirements baseline. The `web/` pnpm workspace defines exactly `setup`,
`teacher`, `student`, and `platform-admin` Playwright projects. `platform-admin`
maps to the current Web `admin` route naming. `researcher` is an independent
formal role and is deliberately outside Issue #9: it has no project, alias,
test match, or shared `student` authentication state here. A future approved
Issue must provide its project and auth state independently.

`pnpm test:e2e:gate` runs `verify`, `contract`, and configuration `list`, then
atomically writes the aggregate E1 report with every check's status and exit
code. The report is invalidated at gate start so a failed build cannot upload a
previous successful result. Requirements-baseline metadata and its comparison
logic are covered by the contract tests; CI does not currently supply an
externally pinned baseline-change input. `pnpm test:e2e` intentionally blocks with
`PW_AUTH_SETUP_NOT_IMPLEMENTED` and `PW_NO_RUNTIME_TESTS` until an approved
authentication contract, safe role storage states, and real runtime tests
exist; subprocess contract tests verify both specified blocked entrypoints. No
browser, login, Keycloak/OIDC, backend, role-isolation, E3, or E4 claim is made
by this configuration work.
