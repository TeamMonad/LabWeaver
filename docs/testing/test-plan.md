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

## Contracts v1 gates

```sh
cargo xtask contracts check
cargo xtask test --suite contract
cargo clippy -p contracts --all-targets --all-features -- -D warnings
pnpm --dir web typecheck
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
cargo xtask test --suite contract
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

## Claude Code Agent runtime gates

```sh
cargo test --locked -p contracts --test authoring_contract
cargo test --locked -p agent-service
cargo clippy -p agent-service --all-targets --all-features -- -D warnings
cargo xtask contracts check
pnpm --dir web contracts:check

# Requires a disposable PostgreSQL server with CREATEDB, supplied by
# LABWEAVER_TEST_DATABASE_URL, or a Docker-backed PostgreSQL container.
cargo test --locked -p agent-service --test claude_code_runtime \
  postgres_run_is_atomic_and_exact_replay_is_not_billed_twice -- --ignored --exact

# Billable: provider environment must already be exported by the caller.
# The test and production worker never source ~/.zshrc or another startup file.
LABWEAVER_LIVE_CLAUDE_MODEL='<exact-model-id>' \
  cargo test --locked -p agent-service --test claude_code_runtime \
  live_claude_code_generates_environment_candidate -- --ignored --exact --nocapture
```

Contract tests prove that the only accepted binding is a provider-opaque
`ClaudeCodeBindingV1` with an explicit runtime profile, exact non-alias model,
CLI version, worker-image hash, runtime-configuration hash and a bounded
per-worker in-flight limit; the retired
OpenAI-shaped binding is rejected. Package-gate tests cover manifest/object
identity, revision-bound classifier identity, hard-denied data and prompt
injection retained only as untrusted structured content. Worker tests prove CLI
version matching, direct process invocation without a shell, immutable stdin
identity, bounded timeout/request/token/cost configuration, no session
persistence, no Chrome, no built-in subagents, no MCP discovery and an empty
Tool set. Process tests additionally prove inherited-environment clearing,
explicit deployment environment injection, unique HOME/XDG/tmp/workdir and
semaphore backpressure.

Result tests cover supported `stream-json` envelopes, progress-event handling,
terminal-result completion without waiting for process EOF, usage accounting,
exact typed EnvironmentSpec validation, protected authority fields, budget overflow,
provider failure classification, payload-free stderr handling and independent
dual-track partial success. The explicit PostgreSQL integration test additionally
checks migration `agent/0002`, atomic idempotency reservation, requested/terminal
Outbox rows, immediate one-track checkpoint retention, live double-claim
rejection, expired-lease attempt-2 recovery, durable cancellation observed by a
different worker, ten concurrent identical requests across four worker
identities with exactly two process invocations, and twenty distinct runs across
the same four workers. It passed on 2026-07-15 against
an ephemeral PostgreSQL 17 instance, using an isolated per-run database, and
supplies E2 persistence/recovery evidence. It does not
prove the deployment credential path, real provider compatibility, container
isolation, HTTP/worker handoff, Kubernetes Job execution, Control candidate
projection, object-store checkpoint references or JetStream delivery; those
runtime paths remain at E1 or unimplemented as listed in the status matrix.
On 2026-07-14, the provider's internal StructuredOutput path failed for the
complete Schema, so the worker protocol was reduced to the intended single
operation: reproduce the complete authoritative Schema in the prompt, require
one bare JSON object, and validate it locally without a weaker Schema or model
fallback. With Claude Code `2.1.209`, the current explicit-environment and
isolated-directory Environment candidate test passed on 2026-07-15 in 11.83
seconds and audited 35,321 microusd. The caller received provider variables
exported by login zsh before injecting them explicitly into the cleared child
environment; this does not prove Secret/Config injection in the worker
container. Evaluation, the real dual-track call and the
container/Kubernetes path remain explicit E3 blockers. Timeout tests also
require `usageObserved=false`: absence of a terminal result envelope must not
be reported as confirmed zero provider usage.

The live debug session isolated the Schema feature boundary: a nested `$ref`
dispatched Claude Code's internal `StructuredOutput` Tool, while a nested
`oneOf` did not. Deployment Verify therefore targets the actual v1 protocol:
the complete official Schema in the prompt, one JSON candidate, terminal
`stream-json` result, and successful local typed/semantic validation. The
worker remains NotReady if any of those checks fail; replacing the official
Schema with a weaker provider Schema is not an accepted fallback.

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
## Environment lifecycle v1 runtime gates

`EnvironmentLifecycle v1` now has local E2 state-transition, repository,
messaging, reconciler and owner-resolution evidence. Deterministic tests
exhaust all 144 observed-state pairs and all 144 state/operation pairs. Docker
PostgreSQL 17 tests cover populated-v1 migration, strict initial-create
invariants, production first-aggregate creation, complete idempotency request
identity, atomic Inbox/create/operation/full-CloudEvent Outbox insertion/replay,
transactional Inbox duplicate/stale/gap blocking, concurrent optimistic
locking, lease exclusion/token fencing, failed-phase and reset-target
persistence, recovery by a new worker after Provider side effect and before
save across distinct durable Provider steps, persistent timeout/cancel cleanup,
expiry selection and cleanup failure.

A Docker NATS JetStream 2.11 test proves acknowledged Outbox publication,
catalogued lifecycle CloudEvent consumption, sanitized terminal quarantine for
invalid payloads, exact-scope Active Resource Lease verification, expired-Lease
rejection without aggregate or Provider mutation, and
`(operationId, providerStep, action)`-bound Provider RPC.
A real rustls mTLS server test covers SAN allowlisting, bounded slow handshake,
client/server certificate rotation, owner/course/revision changes,
deletion/expiry, strong revision ETag, database-authoritative expiry under
simulated host-clock skew, retryable database/network outage and typed shutdown
failure propagation. Production wiring runs command, reconcile, expiry, Outbox
and readiness loops and handles SIGINT/SIGTERM. The exact commands and source
identity must be recorded in the PR; A review and D Verify are still mandatory.

They must also prove Experiment baseline-reset isolation, Work Active-Lease
requirements, reset acceptance only from `Ready`, `Stopped` and `Failed`, reset
target convergence, serialized Work configuration, configuration-failure
transition to `Failed`, access denial for any non-Ready or unhealthy endpoint,
grant revocation before reset/expiry/failure/delete cleanup, deletion
idempotency and sanitized `Deleted` tombstone evidence. The current slice proves
the transport and state-owner boundaries; concrete Container/KubeVirt Provider,
Access-owned revocation responder, Resource-owned Lease responder and E3
deployment evidence remain planned. #47 must not consume the resolver result until the
current PostgreSQL+JetStream+mTLS evidence and build identity receive the
required A review and D Verify.
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
unlocked chart/application version, mismatched chart archive, tag-only image,
missing bound backup evidence, an unapproved Linux controller, or a
configuration that would invoke a bootstrap role. The controller must run only
on the approved Linux router worktree and must fail on Windows rather than
selecting a different launcher or PATH fallback.

Required controller evidence is: router-side `cargo fmt --check`, Linux
`cargo clippy` and unit tests, `ansible-lint`, playbook syntax check, and a
check/diff before a real reconciliation. The real run must verify the dedicated
namespace/PVCs/Pods, Gateway `Programmed`, DNS, internal-CA TLS trust,
authenticated Harbor health and `labweaver-system` policy. A second run must
have no undeclared mutation. This remains E2 deployment evidence until the
declared push/pull/scan and recovery gates are separately performed.

TestFlight is emitted as a run-scoped, schema-validated
`InfrastructureTestFlightReport`; it binds commit, inventory, component lock,
Harbor policy manifest, deployment-manifest hash/locator, and cluster UID,
records cleanup, and passes only the declared `adopted-cluster-baseline`
scope. OIDC/governance checks are explicitly recorded as deferred to #47; they
cannot be read as a successful identity or release-gate claim.

The security/recovery expansion remains under the existing Sprint 2 parent #2.
Neither deferred set is part of the baseline close condition for #23 or #15.

### Private Sigstore gates (Issue #61)

The steady-state identity prerequisite is reconciled only through
`cargo xtask identity-foundation --infra --env <env> --action <deploy|verify>
--yes`. It must reject an unapproved controller, missing or changed root-owned
secret locator, mutable image, conflicting VIP/DNS, unexpected issuer, direct
access grant or interactive flow. E3 evidence requires an exact discovery
issuer, service-account token with the approved audience and
`preferred_username`, two Ready Keycloak replicas, persistent PostgreSQL, a
Programmed internal Gateway and trusted private CA.

The initial adopted-cluster bootstrap may use an A-owned, time-limited
TokenRequest kubeconfig from an approved Linux workstation. Such a run must use
the same digest-pinned manifests, emit a unique run ID, deny D access to Secret
objects and cluster mutation, and be followed by `identity-foundation --infra`
reconciliation before closure. A direct bootstrap is runtime evidence, not a
replacement deployment entry.

`cargo xtask private-sigstore --infra --env <env> --action <action> --yes` is the
only lifecycle entry; `<action>` is a Rust enum, not a path. Contract tests reject public/wildcard/human workload identities, stale or
tampered bundles, TUF rollback and reports that hide a required failure. Linux
controller checks must additionally prove the official chart hash, fixed image
digests, missing locator failure, backup-before-existing-state, ClusterIP-only
component services, TLS Gateway, default-deny policy and restricted pod context.

The real TestFlight must bind commit, run, cluster UID, inventory, deployment
manifest, component lock, trust bundle, workload policy and backup. Each of
identity, schema, chart/image identity, backup, deploy, second deploy,
sign/verify, restore, rotation, disaster recovery, cleanup, outage fail-closed,
TLS, NetworkPolicy, OIDC, SCT, Rekor inclusion and TUF has an independent status and diagnostic. Any non-passed
required check prevents overall `passed`. Until Keycloak, offline root and the
private cluster are available, those E3 checks remain `blocked`/`not_run`.

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

[ADR 0003](../adr/0003-nats-subject-and-delivery-contract.md) remains design evidence. The
[NATS Event Contract v1](../contracts/nats-event-contract-v1.md) now has E1 typed payload,
registry, schema, sequence and protected-payload evidence from `contracts`; it is not messaging
runtime evidence. Before a messaging path is marked implemented, E2 tests must prove
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
