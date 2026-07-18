# Test Plan

## TEST-03a Sprint 3 acceptance asset gate

Issue #94 adds a static E1 gate that freezes three future E4 golden paths, six deterministic C++17
samples, the complete security/error matrix, frontend acceptance inventory, evidence schema and
Feature Complete reference contract:

```sh
cargo test -p xtask --locked
cargo xtask acceptance-assets validate
cargo xtask acceptance-assets list
cargo xtask acceptance-assets validate-fixtures
cargo xtask acceptance-assets validate-report --report tests/fixtures/acceptance/reports/valid/planned-e1.json
cargo xtask acceptance-assets validate-report --report tests/fixtures/acceptance/reports/valid/local-e2.json
```

Negative fixtures must exit non-zero with the exact diagnostic recorded in
`tests/fixtures/acceptance/fixture-expectations.json`. The suite covers evidence-level escalation,
Mock/fixture E3/E4 claims, missing and cross-bound identity, cleanup, rollback, blockers, skipped
steps, unknown scenario/gate, all report-reference path classes, incomplete negative/frontend/sample
inventories and Feature Complete prerequisites. C++ timeout, memory and output samples are never
executed by this static gate. See `docs/testing/sprint3-acceptance-assets.md` for the evidence
boundary and future E4 prerequisites.

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

`cargo xtask test --suite contract` is the executable aggregate contract gate.
It first rejects Rust-generated Schema/OpenAPI drift, then runs the complete
`contracts` crate target set and finally rejects generated Web SDK drift through
the checked-in `pnpm contracts:check` entry point. Missing Cargo or pnpm tooling,
test failure, or drift fails the aggregate command.

## Issue #48 Control-plane gates

The Control plane is tested as an asynchronous, authority-separated path. Control may read
Agent outcomes and artifact evaluations only through the internal mTLS API and may advance its
projections only with an Inbox-protected Agent event. It must never read or write the `agent`
schema. Every mutation test supplies an `Idempotency-Key`; mutations of existing resources also
supply an exact strong `If-Match` value.

The checked-in PostgreSQL integration entry point is:

```sh
cargo test -p control-service --test postgres -- --nocapture
```

It applies the released Control and Agent Migration chains to PostgreSQL 17, proves that a
completion state cannot exist without its fencing lease, rejects a candidate through the wrong
kind-specific decision route without writing approval/idempotency state, verifies withdrawal in
GET/list read models, and races twenty writers against one course SSE cursor while requiring the
exact sequence set `1..=20`. The Agent course-scope test proves a foreign course mutation leaves
both run and idempotency state unchanged. Contract drift, unit and static gates are:

```sh
cargo xtask contracts check
cargo test -p artifact-store -p control-service -p agent-service --lib
cargo clippy -p artifact-store -p control-service -p agent-service --all-targets -- -D warnings
```

Aggregate E2 is recorded only when the same worktree passes real MinIO versioned-object tests,
JetStream publish-ACK/duplicate/gap/restart tests, and ephemeral-CA Gateway-to-Control,
Control-to-Access and Control-to-Agent SAN/rotation/outage tests. Issue #48's local suite now
supplies that composition; it remains distinct from deployed owner-service or Kubernetes evidence.
Issue #52 now provides the local authoritative Container artifact/evaluation projection. Issue #53
adds the VM release consumer path and exact KubeVirt/CDI artifact, storage and SSH readiness
binding. The positive release path is expected to return a stable blocking diagnostic until the
v2 build completion has been durably projected. Connected E3 requires the deployment-owned
BuildKit/Harbor/Trivy/Private Sigstore and Kubernetes/KubeVirt executors.

Issue #52 local regression gates additionally run the ten-stage Agent pipeline suite, six
Container Provider tests and the Agent/Environment PostgreSQL tests. They reject empty
certificate/signature hashes, signature subject-digest drift, expired or withdrawn releases and
active policy/trust rotation. The PostgreSQL cases prove append-only withdrawal sequence 2 and
that a provider delay longer than retry delay still schedules from the post-provider database
clock. Remote executor requests carry their durable fence identity, but executor-side
highest-generation and cleanup/delete tombstones remain connected E3 evidence.

The current worktree records real versioned MinIO presign/freeze/overwrite/exact-version/cleanup
coverage and real JetStream Agent Outbox missing-stream failure, retry and persisted-ACK ordering.
The Control Outbox test applies the same ACK fence to both release publication and withdrawal and
proves both immutable CloudEvents reach the configured stream exactly once.
The Control consumer suite separately proves duplicate suppression, gap rejection, durable restart,
outcome-fetch outage, redelivery, and atomic Inbox/projection/SSE commit. The ephemeral-CA suite
proves configured URI-SAN boundaries, leaf-certificate rotation and downstream outage mapping.

## Issue #47 controlled Keycloak verification

The ignored `auth` Keycloak integration test is an explicit E2 entry point. It
requires a caller-owned disposable Keycloak 26.3 HTTPS container, its controlled
CA file and disposable bootstrap-admin credentials; it has no public endpoint
or fixture-only fallback. It drives the HTML login form through Authorization
Code + PKCE, exchanges the code, validates nonce/issuer/audience/`azp` and role
claims, and executes RP-Initiated Logout. It then creates two new Keycloak RSA
providers, proves unknown-`kid` refresh through the same private-CA client, and
removes the authoritative key providers to prove fail-closed JWKS outage. The
realm fixture and admin credentials are test-only and must never be used by a
deployment.

The recorded local run used
`quay.io/keycloak/keycloak@sha256:08a31919cfcd814bf1b465142b1a716c4d1a8830f772bb5c9dffcbd96de3fba6`.
After starting that disposable HTTPS container with the checked-in realm fixture,
run the gate with caller-controlled values:

```sh
LABWEAVER_KEYCLOAK_TEST_ISSUER='https://localhost:18443/realms/labweaver-test' \
LABWEAVER_KEYCLOAK_TEST_CA_FILE='<controlled-ca-file>' \
LABWEAVER_KEYCLOAK_TEST_ADMIN_USERNAME='<disposable-admin>' \
LABWEAVER_KEYCLOAK_TEST_ADMIN_PASSWORD='<disposable-password>' \
cargo test -p auth --test keycloak_discovery -- --ignored --nocapture
```

The test mutates and removes signing-key providers, so the Keycloak instance
must be disposable and must be destroyed after the run.

Tests that cannot provision TLS may set `transport_security:
insecure-test-only` and `LABWEAVER_ENABLE_INSECURE_AUTH_TEST_MODE=1`. The mode
is deliberately restricted to loopback addresses and does not relax token,
claim, PKCE, state, nonce, CSRF, role or scope validation. Production and shared
test deployments must use `strict` with a controlled CA instead.

The controlled PostgreSQL integration entry point also applies the immutable
migration catalog, verifies the runtime cannot perform schema DDL, and covers
encrypted BFF-session restoration, CSRF verification, direct/SID revocation,
authoritative membership reload and registered service identity checks. The
Rustls mTLS tests generate ephemeral CA, server and `clientAuth` certificates.
They cover allowlisted URI SAN extraction and a real Access-to-Environment
handshake, exact owner/course/environment/revision and strong-ETag binding,
denial, response tamper and bounded outage. A deployed Gateway decision call
and controlled certificate rotation remain E3 work.

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

# Docker/Colima-backed PostgreSQL executor fencing, restart replay and cancellation.
cargo test --locked -p agent-service --test build_store_postgres
cargo test --locked -p environment-service --test postgres \
  container_executor_persists_generation_and_permanent_delete_tombstone

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

### Environment management API E1 gates (#81)

The contract slice is verified with:

```sh
cargo test -p contracts --test environment_management_api_contract
cargo xtask contracts check
pnpm --dir web typecheck
pnpm --dir web test:e2e:sdk
```

Rust tests prove bounded opaque pagination, required course scope, distinct stream cursors,
terminal timeout/cleanup facts, generated Public OpenAPI coverage, Environment-specific 202
identity and per-operation error status sets. Real Chromium transport tests prove BFF cookie/CSRF
separation, direct bearer injection, absence of a duplicated `/api/v1` prefix, typed RFC 9457
decoding, missing-credential failure, timeout/cancellation distinction and no silent retry. These
are E1 contract and browser-transport
evidence only; they do not prove Handler authorization, PostgreSQL snapshots, Outbox/SSE replay,
Access ownership, UI behavior or deployment.

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
the transport and state-owner boundaries. #52 and #53 now add formal local
Container and KubeVirt Providers; the KubeVirt suite verifies deterministic
VM/CDI resources, default-deny plus Gateway-only SSH ingress, safe fixed
base64 `data.userdata` cloud-init, strict one-entry `ssh:22` projection,
VMI/CDI/scratch-aware quota without unsafe equal guest limits, guest/SSH
readiness gating, duplicate fence identity, stop-start identity preservation
and cleanup. A real PostgreSQL 17 test applies Migration
0004 and proves exact replay, stale-fence rejection, disk/host-key preservation
and deletion tombstones. Access-owned revocation responder, Resource-owned Lease
responder and connected E3 deployment evidence remain required. #47 now
consumes the merged #51 resolver contract through mTLS, but the combined build
identity still requires A+B review and D Verify before Issue closure.

### KubeVirt RuntimeProvider local and E3 gates (#53)

The local regression entry points are:

```sh
cargo test -p environment-service --test kubevirt_provider
cargo test -p environment-service --test postgres \
  kubevirt_observation_identity_is_durable_fenced_and_tombstoned
```

The first command is E1 Provider/fake-executor evidence; the second is local E2
PostgreSQL evidence and uses the caller's configured Docker-compatible runtime.

E3 uses the reviewed deployment binding and same commit to import the exact VM
artifact through CDI, wait for current KubeVirt observed generation, guest agent
and SSH host-key handshake, then record one private SSH endpoint. It repeats the
same reconcile and proves one VM/DataVolume/PVC; writes a marker to the guest
disk and proves it survives start-stop-start; attempts non-Gateway network
access and proves denial; and injects apply, readiness, cancellation, restart,
observation and delete failures. Every terminal cleanup case must show Access
revocation plus absence of Namespace, VM, VMI, DataVolume, PVC, Secret, Service
and NetworkPolicy. The same report records the actual VMI memory overhead and
CDI default pod requests and proves they do not exceed the deployment binding's
explicit budgets. A Fixture result or the pre-existing infrastructure VM
TestFlight cannot satisfy this gate.

### Dual-runtime Submission Collector gates (#54)

Local deterministic and contract entry points are:

```sh
cargo test -p evaluation-service --lib --test collector
cargo test -p evaluation-service --test freeze_postgres
cargo test -p artifact-store minio_versioning_object_lock_and_cleanup_are_fail_closed
cargo xtask contracts check
cargo test -p persistence-sqlx
```

The first command proves bounded PVC selection, empty-file identity,
preflight/freeze consistency, source mutation, excludes, required paths,
symlink rejection and raw/file/output limits. The SSH unit guard rejects public
targets, unsafe roots and credentials outside the five-minute binding. The
PostgreSQL test proves exact idempotent replay, request conflict, retained
failed attempt, retry attempt, one authoritative row and one matching v2
Outbox event. The MinIO test must use a versioned Object-Lock-enabled bucket and
prove Governance mode, deadline, non-null version, metadata, checksum and exact
read-back. Docker absence is a blocker, not a skipped pass.

E3 mounts a real Container PVC read-only and freezes an approved manifest
through the production source resolver. It then issues a single-Environment VM
certificate with principal `labweaver-collector`, maximum five-minute validity
and critical `force-command = internal-sftp -R`, connects over the private
path with a pinned host key and freezes the same manifest shape. Negative runs
cover missing and changed files, symlink/traversal, all three limits, wrong host
key, certificate expiry, shell/write denial, SSH refusal/timeout, MinIO outage,
version/retention mismatch and database failure after upload. The report must
bind commit, deployment, Environment/runtime/source, manifest, actor, file
manifest, object version/hash, database contract and Outbox payload. It must
also identify any retained but non-publishable orphan and prove no failed run
produced a `FrozenSubmission` or event.

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

### Platform image supply-chain gates (Issue #62)

PR/static gates run without GHCR write or signing credentials. They require
`cargo fmt --all -- --check`, strict `cargo clippy -p xtask`, `cargo test -p
xtask`, static package-manifest validation, seven two-pass image builds,
secret/SBOM/Trivy checks, Helm lint/template, Kyverno tests and
`git diff --check`. The component set is exact. Missing, duplicate, external,
tag-only or conflicting digest entries fail. Critical vulnerabilities and
secrets fail; every High finding remains in the report.

Only a `develop` push whose complete static/build/scan matrix passed may enter
the Actions GHCR publication job. That job uses the repository-scoped
`GITHUB_TOKEN` with job-local `packages:write`, publishes an immutable
`git-<source-commit>` tag for each exact component, reads back the subject
digest, and never publishes `latest` or a package manifest. Pull requests and
failed matrices must never execute it. The unsigned digest remains
non-deployable until the controlled router completes private signing and proof
verification.

The controlled Linux router is the only connected execution authority.
`package-validate --mode connected` must reread the GHCR subject manifest digest,
BuildKit SBOM/provenance attestations, Trivy DB identity, private Sigstore
certificate, SCT, Rekor inclusion and current or explicitly previous trust
revision. A connected prerequisite failure never invokes static validation as
a fallback, and a partial run never publishes the canonical package manifest.

E3 begins with a read-only concurrency and infrastructure baseline. A possible
impact on another deployment stops the run. The run-scoped namespace must then
prove all seven real binaries and dependencies reach readiness, accepted exact
identity/digest admission, rejection of unsigned, external, tag-only, wrong
identity/trust and tampered-digest inputs, rollback to the previous still-valid
verified manifest, and cleanup. The report binds one source commit, cluster UID,
trust revision, package/deployment manifest pair and seven digests. Missing
Issue #61 post-merge identity replay or production Config/Secret locators keeps
the result blocked rather than fixture-backed.

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
## AccessGrant and OpenSSH authorization verification

Issue #49 verifies the Access-owned portion of `ssh alias@gateway`. Contract tests cover supported, weak and malformed keys; exact fingerprint binding; Grant and session state matrices; alias injection; stale revisions; token replay; and TTL bounds. PostgreSQL integration uses the forward-only Access Migration and proves global fingerprint uniqueness, one nonterminal actor/environment Grant, actor/Gateway-scoped idempotency, digest-only token storage, expired activation-lease reclamation, stale-worker fencing and immediate authorization denial after membership revocation. The ephemeral-CA mTLS suite proves endpoint eligibility rejects owner/Lease denial, revision drift, unhealthy endpoints, tampered responses and resolver outage.

The remaining E2 gate must run two Access instances against real PostgreSQL, TLS JetStream and the real Environment mTLS eligibility handler, including concurrent lease recovery, publish acknowledgement/replay, SAN/certificate failures, revoke/expiry/key-delete atomicity and the 60-second close/overdue result. Controlled Kubernetes verification may claim only the deployed Access/Environment API and mTLS boundary. Native SSH-to-VM requires both #53 and #63 and must not be inferred from fixture, contract or report generation.

## Deferred direct-access verification

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
