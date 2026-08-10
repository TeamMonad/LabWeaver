# Implementation Status

This document records current repository and connected-runtime facts. Design
documents, fixtures, health endpoints and reports from another source identity
are not completion evidence.

## Sprint 3 Rust fault-localization logging (#165)

The internal `labweaver.log.v1` formatter, schema, strict HTTP correlation
boundary, downstream header propagation, CloudEvent/NATS correlation,
Container/KubeVirt provider fence trace identity, Freeze Worker stage events,
and Access Gateway authorization/session events are implemented on the #165
candidate. The formatter supplies the mandatory event envelope to all Rust
logs and redacts or omits non-allowlisted detail before serialization. Focused
schema, privacy, INFO/DEBUG, W3C and invalid-header tests are local merge
evidence.

This status does not claim a connected failure reconstruction or release
decision. Real HTTP-to-NATS-to-Worker-to-Provider evidence remains owned by
Sprint-end acceptance Issue #126 under its frozen identity and independent D
Verify.

## Release demo video (#128)

The versioned capture and edit pipeline is implemented in `tools/demo-video`.
It uses eight fixed Playwright scene IDs, repository-relative evidence
locators, strict capture/manifest Schemas, Remotion, FFprobe and external
`zh-CN`/`en-US` SRT files. The complete Fixture preview is visibly labelled
`Fixture preview / not release evidence` and remains `releaseEligible: false`.

Current formal state: **blocked**. A public final cannot be resolved until #126
provides all eight connected shots from one frozen source/package/configuration/
migration/deployment/image/runtime/Run identity, a passing Release Gate v3
report, D Verify and a completed frame-by-frame privacy review. #128 consumes
those outputs and does not open another package, deployment, Resource replay,
connected Playwright or Release Gate window. The existing PPT and its source
are unchanged and are not #128 deliverables.

The Docker Desktop Fixture preview has a separate non-release deployment path.
`tools/demo-video local-cluster` owns only the labelled
`labweaver-local-demo` namespace and `labweaver-fixture-demo` Helm release,
requires a clean commit and the exact Docker Desktop contexts, deploys one
non-root/no-token/read-only Web Pod, then records
`demo-video-local-cluster-report.v1`. Its `demo` action refreshes deterministic
role state, captures the eight Fixture scenes sequentially and renders with one
GPU-backed worker. This path remains `releaseEligible: false`; it does not
deploy application services or change the #126/KubeVirt final blocker.

## Local integration candidate gate (#150)

The local Docker-first integration entry point is implemented at
`cargo xtask test --suite integration`. `--scope changed` performs explicit
path selection; `--scope candidate` runs the full local Docker dependency and
BuildKit/Registry/Trivy canary gate, with optional fresh kind API validation.
Container lifecycle (network, create/start, labels, random host ports, env,
image pull, inspect, exec readiness, logs, cleanup) uses the Bollard API;
BuildKit, `docker push`, `docker save` and Trivy remain CLI entry points.
JetStream is probed through `async-nats` (unique stream, publish, ack, message
count).

Verified local evidence (working tree on `9b8d5abf`, dirty): run
`019fcdd980467d7094c65e1915e1ec38` passed the host contract probe, dependency
contract probe, BuildKit/Registry/Trivy supply chain and the fresh kind gate
with cleanup passed. The earlier Docker Hub pull timeout blocker cleared once
external registry connectivity recovered; pinned images are digest-verified.

Not claimed: real service binaries starting against the shared dependencies,
MinIO immutable-object semantics, Keycloak token issuance/JWT validation, CI
execution of the manual-dispatch workflow, connected deployment, KubeVirt or
Release Gate evidence. kind proves Kubernetes API and Helm rendering semantics
only. Reports remain ignored local evidence and do not satisfy Sprint-end
connected acceptance. Ordinary Issue/PR merge gates stop at this local evidence
boundary; cluster deployment and Release Gate remain owned by the dedicated
Sprint-end acceptance Issue.

## Sprint 3 ConsoleCapability contract

| Capability | State | Current evidence and boundary |
| --- | --- | --- |
| Container xterm ConsoleCapability (#131) | implemented; locally verified; connected blocked | Additive capability/session migration, strict BFF/Origin/CSRF/ETag/idempotency issuance, atomic redemption, metadata lifecycle events, cancellation registry, Environment-authoritative `TerminalSpec`/Lease validation, mTLS proxy chain, unique Ready Pod selection, fixed `runtime` PTY, binary I/O, bounded resize/output and Sprint 3 Web reuse are implemented. Contract generation, focused Rust/Web tests, static deployment checks and local integration are merge evidence. No shared-cluster PTY, connected revocation/control-loss or Release Gate evidence is claimed; #126 owns it. |
| KubeVirt noVNC ConsoleCapability (#124) | implemented; locally verified; connected blocked | Reuses the generic capability/session/proxy foundation and noVNC UI. Runtime-tagged Environment eligibility, kind-safe one-time consumption, per-connection revision/Lease/release checks, a dedicated mTLS KubeVirt console executor, fixed VMI namespace/name/UID/label fencing, bounded RFB relay and least-privilege deployment policy are implemented. Local contract, Rust, Web and deployment checks are merge evidence; real browser-to-VMI, connected revoke/expiry/stop/delete and Release Gate remain #126 scope. |
| Web console Fixture preview | implemented; Fixture-verified | `pnpm --dir web preview:console:fixture` builds and serves the EX3-derived deterministic browser preview without a backend. It is visibly marked Fixture, renders only Fixture state, and is documented in `docs/testing/fixture-console-preview.md`. It is layout/state-machine evidence only, not Access proxy, Container, KubeVirt, connected-runtime, or Release Gate evidence. |
| Connected console acceptance (#126) | implemented locally; connected blocked | Release Gate v3 requires 13 checks and parses same-identity `connected-console-evidence.v1` for real xterm and noVNC. Connected Playwright covers positive/manual reconnect, revoke, short Grant expiry, stop, delete, consumed-locator rejection and label/UID-fenced control-channel loss with cleanup readback. No cluster write or E4 claim has been made: the operation budget window, clean frozen candidate, new package/deployment identity, Resource signing source/credential registry, KubeVirt/CDI/KVM/storage/identity/network preflight and GPU/mdev capacity must all clear first; D retains independent Verify. |

## Current identity

- PR #147: Ready for review / release-blocked, branch
  `feature/142-resource-lease`, target `develop`
- Source identity: every candidate report and package must record the full Git
  `HEAD`; a dirty tree is never release evidence.
- Connected cluster identity: not present in this checkout
- Resource package/deployment/replay identity: not generated for this source
- Local Resource replay identity envelope: implemented; it records only
  repository-relative locators, SHA-256 hashes, immutable Resource image and
  configuration-bundle identity; no private values are emitted
- Formal Release Gate v3: owned by Sprint-end acceptance Issue #126; not a per-PR deployment gate

The Docker Desktop local validation profile is deliberately separate from the
formal gate. It records a `local-connected-non-release` preflight report and
must never be described as Kubernetes/KubeVirt connected evidence.

The native Docker Desktop preflight was run against context `docker-desktop`
and recorded under the ignored `artifacts/local-replay/` locator. It correctly
failed closed with four blockers: missing `nfs-rwx`, KubeVirt, CDI and
`ECNU_API_KEY`; the report is `releaseEligible: false` and is bound only to
the exact `sourceCommit` embedded in that report.

### Approved-environment read-only preflight (2026-08-05)

The approved cluster was inspected through the controlled SSH entry points only;
no Kubernetes, Ansible, package, credential or filesystem write was performed.
The three-node cluster is reachable and all nodes are `Ready`; KubeVirt is
`Deployed`, CDI is healthy, and both `nfs-rwx` (immediate) and `local-path`
storage classes are present. PostgreSQL, NATS, MinIO, BuildKit, Harbor and
Keycloak foundation workloads are ready. No LabWeaver application Deployment,
Resource Service Pod, environment Namespace or VirtualMachine is currently
running.

The only remote source checkout found for the earlier Resource validation is a
detached, stale identity and has no package or deployment manifest for the
current `feature/142-resource-lease` HEAD. Its latest replay log returns
`LW_EXECUTION_ATTEMPT_BUDGET_EXHAUSTED`; no replay process is active, and the
existing clean-validation lock and root-only logs are retained unchanged. The
connected operation budget therefore prevents starting another package,
application reconcile or replay cycle without an explicitly recorded Owner
validation window.

The Resource NATS issuance record is also fail-closed with
`LW_RESOURCE_NATS_SIGNING_SOURCE_UNAVAILABLE`: no operator/account signing key
store was found, although existing service-credential locators were detected.
No service credential, JWT, private key or signing material was read. Node
allocatable resources expose no `nvidia.com/gpu` device-plugin capacity or
mdev extension resource, so GPU capacity remains an explicit negative
capability for this environment. These observations are diagnostic only and
cannot satisfy the same-identity connected Resource replay or Release Gate v3.

### Development-cluster redeploy feasibility (2026-08-05)

The latest recorded read-only preflight shows that the LabWeaver application
surface is already absent while the shared foundation is retained. A second
"thorough clean" followed by deployment is therefore not currently eligible
from #142/#147: the connected operation budget is exhausted, the remote checkout
has no package/deployment identity for this source, the Resource NATS signing
source is unavailable, and the cluster publishes no GPU or mdev capacity.

No ledger deletion, ledger-root rotation, stale-package reuse or fourth
connected cycle is allowed. A future clean redeploy requires an Owner-approved
acceptance window under #126 (or the separate v1 deployment project #148), a
fresh source/package/deployment identity, a reviewed private bundle including
the NATS signing source, and a new read-only capacity/credential preflight.
Until those conditions are recorded, only local/CI and read-only preflight are
permitted; this assessment is not deployment or Release Gate evidence.

## Legacy Sprint 1 and Sprint 2 connected result (superseded)

The table below is retained only as historical context for the previous
connected validation. Its reports, Run IDs and source identities are legacy
evidence and cannot be reused for the current `feature/142-resource-lease`
candidate, local Docker report, or formal Resource Gate v2.

| Capability | State | Current evidence and boundary |
| --- | --- | --- |
| Six service and six PostgreSQL domain boundaries | verified | Baseline catalog and non-destructive adoption cover all six owned schemas. Resource remains outside the Sprint 2 runtime path. |
| Contract, OpenAPI, JSON Schema and Web SDK | verified | One Sprint 2 v1 semantic source; drift checks and CI pass. |
| Keycloak/OIDC and authorization | verified | Real teacher and student sessions, CSRF-protected BFF calls, course scoping and connected denial matrix pass. |
| Claude Code AgentRun | verified | Container AgentRun `019fa7ca-2be5-7540-8546-d55f721a4c27` and VM AgentRun `019fa7d7-0bec-7f11-811f-3c22deb44e75` each produced independent Environment and Evaluation candidates through the explicit ECNU Anthropic-compatible binding. |
| Teacher approval and publication | verified | Both candidate kinds were approved for each runtime. Container release `019fa7cb-d51e-7af3-8f89-995a509e37f3` and VM release `019fa7e1-cd5f-78f2-8e5e-68c2971c47e8` were published. |
| BuildKit, Harbor and Trivy | verified | Container artifact `sha256:e06d528deebc192569bcc337784d3bd8eed4ecd44755520137c32f98bd497547` was built, pushed, scanned by Trivy 0.72.0 and passed the digest-bound gate with zero Critical findings. |
| Container runtime | verified | Environment `019fa7cc-1b72-76e1-a039-cd6aa74f1c1e` completed create, HTTP access, freeze, stop, start, delete and absence readback. |
| KubeVirt runtime | verified | VM `019fa7e2-4bfd-7ee2-9eea-61314caa82d6` completed create, Gateway SSH workspace write/read, freeze, stop, start, delete and absence readback. |
| Freeze-only Evaluation | verified | Container submission `019fa7d0-d62a-7542-9e81-0bf5471d7b31` and VM submission `019fa7fe-0d0b-7343-91d0-61f492250c12` are immutable and retain runtime-artifact identity. Runner, Checker, Aggregator and scoring remain excluded. |
| Access and Gateway negative matrix | verified | Cross-course, revoked, expired, arbitrary shell, target injection, SCP/SFTP, forwarding and Access-service-outage cases all rejected. The temporary test VM was deleted afterward. |
| Real browser journeys | verified | Connected Playwright used real Keycloak teacher/student sessions. The Container replay passed teacher and student paths; the VM replay passed teacher/student setup and the KubeVirt freeze path. |
| Application adoption and idempotence | verified | Two non-destructive application reconciles retained the same package, configuration, migration and seven-image identities. |
| Rollback drill | verified | Helm rollback from revision 260 to 259 created revision 261; all workloads recovered. The current source deployment was then restored and verified at revision 265. |
| Cleanup readback | verified | No namespace labeled `labweaver.io/environment=true` remained after the final cleanup; all ten platform Deployments remained ready. |
| Machine-readable Release Gate | legacy | Earlier source identity produced a schema-valid report for Run `a2835d47-7f9b-48a3-b8a0-60d22f57d5e2`; it is invalid for current #142. Evidence remains private/ignored and contains no Secret. |
| Full `cargo xtask demo replay` | not run | The authoritative gate was run directly after connected checks. The wrapper's repeated infrastructure Verify and Playwright execution was intentionally skipped; this is not represented as a replay pass. |

## Issue #123 local implementation

The `codex/issue-123-control-plane` worktree adds the PostgreSQL-authoritative
EvaluationRelease, EvaluationRun and StepRun control plane. The contract now
binds source, package, configuration, migration catalog, digest-pinned runner
image, runtime artifact, frozen submission, release identity and trace ID.
Evaluation Service owns internal mTLS routes for release publication, run
creation, readback, cancellation, StepRun retry, StepRun cleanup verification
and fenced worker completion. The evaluation migration adds release, run,
step-run and attempt tables with idempotency, lease and pending-cleanup
constraints, and Outbox events remain payload-safe and hash-only.

Current evidence is local E2:

```sh
cargo test -p contracts --all-targets --all-features
DOCKER_HOST=unix:///Users/zeyi2/.colima/default/docker.sock \
  cargo test -p evaluation-service --test control_plane -- --nocapture
cargo xtask contracts check
```

This evidence proves the contract/schema surface and real disposable
PostgreSQL behavior for idempotent release/run creation, identity mismatch
rejection, worker lease fencing, cancel, retry, expired-lease recovery and
cleanup-boundary completion. It does not prove a real Control Service caller,
Kubernetes/OJ runner, runner image digest, provider binding, shared cluster,
Release Gate report or D connected Verify.

## Issue #125 / #160 / #161 release and result product surface

Status: **implemented, local verification pending completion**.

The #160 backend candidate adds explicit teacher EvaluationRelease
publish/list/detail/withdraw contracts and student owner-scoped terminal-result
list/detail contracts. Access authorizes every new route by session, role and
course scope; Control reconstructs runtime identity from authoritative
candidate, approval, course policy, ProblemPackage and deployment configuration;
Evaluation remains the sole release/run authority. Migration 3 records an
append-only actor-attributed withdrawal audit and adds newest-first release and
student terminal-result indexes. OpenAPI, JSON Schema and the generated Web SDK
are generated from the Rust contracts.

The #161 stacked Web candidate adds a teacher publish/list/detail/withdraw
surface to the existing approval page and student terminal-result list/detail
routes. Both use the generated SDK, existing role routing, `AsyncState`,
`DiagnosticBanner`, confirmation dialog and Fixture transport. Runtime identity
is read-only. Failed/cancelled runs omit partial scores, and the client neither
polls nor subscribes to running state.

No shared-cluster operation is part of #125/#160/#161. Contract, Rust,
disposable PostgreSQL, Web unit and Fixture browser checks remain PR evidence,
not connected evidence. Real Keycloak sessions, connected Playwright, actual
runtime/provider identity and Release Gate evidence remain exclusively owned by
Sprint-end acceptance #126.

## Issue #140 local implementation

The `feature/140-oj-cpp17-runner` worktree adds the internal C++17 OJ request,
checker, deterministic aggregator, evidence/receipt validation, isolated
Kubernetes resource plan and executor, shell-free worker modes, dedicated
digest-pinned image and focused tests. Review hardening keeps full request
digests in annotations, requires an Ansible-provisioned namespace default-deny
before Job creation, rejects process-group/namespace escape syscalls, limits
student processes, and binds deletion to UID/resourceVersion preconditions.

Its current evidence is **E1 only**. The implementation is not connected to a
merged public EvaluationRun or StepRun, and this #123 branch has not yet been
revalidated with the OJ runner branch. No real Kubernetes run has verified the
compiler image, cgroup outcomes, NetworkPolicy, cancellation or cleanup.
Accordingly Issue #140 is not `done`, is not a release-gate pass and does not
change the connected Sprint 2 identity above.

## Issue #141 read-only Ansible Probe local implementation

The `feature/141-linux-nginx-probe` worktree adds the internal read-only
Ansible probe request, bounded typed facts, deterministic assertion
evaluation, evidence/receipt validation, the Kubernetes resource plan and
executor, the pinned-profile worker mode, the dedicated digest-pinned image
(`ansible-core 2.18.6` plus the approved `linux-nginx-probe-v1` playbook) and
focused tests. The executor requires an Ansible-provisioned
`ansible-probe-default-deny` namespace policy before Job creation, allows
attempt egress only to the target VM on TCP/22, pins the SSH host key by
SHA-256 and binds deletion to UID/resourceVersion preconditions.

Its current evidence is **E1 only**. The implementation is not connected to a
public EvaluationRun or StepRun because Issue #123's authoritative lifecycle,
Outbox and persistence contract is not present on the current `develop`
baseline. No real runner image, Kubernetes scheduling, NetworkPolicy
enforcement, real KubeVirt VM path, certificate issuance or cleanup has been
verified yet; the positive and negative real-VM paths remain connected
evidence owed to D Verify. Accordingly Issue #141 is not `done`, is not a
release-gate pass and does not change the connected Sprint 2 identity above.

## Issue #142 Resource request and Lease authority

The `feature/142-resource-lease` worktree contains the Resource-owned request,
approval, capacity-claim and Lease contracts; versioned schemas; PostgreSQL
migrations; request fact Outbox dispatch; exact NATS Lease verification; and a
fenced Kubernetes ResourceQuota-shell provider. A ready shell activates only
after provider readback, then performs an mTLS Resource-to-Environment Work
handoff bound to request, claim, Lease, Release hash and Provider binding.
Handoff is idempotent at Environment and becomes `handed_off` only after its
acceptance; three failed handoffs become an auditable `blocked` claim.

The Work publication boundary and empty-deployment bootstrap are implemented
locally but not yet connected-verified. `POST /api/v1/courses/{courseId}/work-agent-runs`
uses a distinct request contract and persists `EnvironmentClass::Work` through
the Agent dispatch. An experiment, absent or conflicting Environment class is
rejected with `LW_LLM_ENVIRONMENT_CLASS_MISMATCH`; the existing AgentRun route
continues to bind `experiment`. A private Resource acceptance profile is
cross-checked against the Access seed and requires UUIDv7 course/actor IDs plus
exact teacher, student and platform-admin memberships. It cannot seed
candidates, approvals, releases, requests or leases directly.

Release Gate v3 requires a Resource deployment manifest, immutable
`resource-service` identity and `resource-lease` connected evidence. v1 reports
remain legacy and cannot close #142. The connected replay, real Container/VM
evidence and Release Gate are owned exclusively by Sprint-end acceptance Issue
#126; ordinary development PRs must stop at local and CI evidence. #142 remains
implemented locally and cannot be marked Done or release-ready until #126
supplies the connected evidence.

### Authorized destructive clean-validation reset (2026-08-04)

The user explicitly authorized destruction of LabWeaver application state and
legacy credentials so the next validation can start from a clean environment.
Run `resource142-clean-20260804T032949Z` completed on `edge-router`. Its
sanitized root-only manifest is retained at the controlled locator
`remote://edge-router/var/lib/labweaver/cleanup/resource142-clean/resource142-clean-20260804T032949Z/manifest.sha256`
with digest
`sha256:c8ae79fa2acf114726f06dbb097e0b5c7284800fde460f1fccbc0032abf4ea33`;
33 recorded files all verified `OK`.

The reset removed the LabWeaver application namespace and auxiliary runtime
namespaces, truncated all six business schemas while preserving migration
tables, removed NATS state and application credentials, removed the Workloads
Keycloak realm and the LabWeaver Harbor project, and removed the old private
bundle, credential registry, package kubeconfig and execution ledger. PostgreSQL,
MinIO, Harbor, Keycloak, Kubernetes/KubeVirt and their foundation storage were
retained. No Secret values, JWTs, private keys or user material are included in
the manifest. Previous connected reports and attempt-ledger conclusions are
therefore invalid for the next candidate; only evidence produced in the
Sprint-end acceptance window owned by #126, after the new package, deployment
and Resource replay, may close #142.

The retained Harbor Trivy deployment had a real NFS `root_squash`/kubelet
`applyFSGroup` blocker. The Harbor Ansible role now applies
`fsGroupChangePolicy: OnRootMismatch` with `fsGroup: 10000` before readiness;
the live StatefulSet was reconciled and read back `1/1 Ready`. This repair is
part of the source-of-truth role and is covered by the Ansible fixture test.
The sanitized repair readback is retained at
`remote://edge-router/var/lib/labweaver/cleanup/resource142-clean/resource142-trivy-fsgroup-repair-20260804T040500Z/manifest.sha256`
with readback digest
`sha256:23a10e75f622577d29a04872e869a397d56b5d2c7431d666046e28ffc8d821f`.

The source identity for each checkout is recorded by its package and report.
Earlier source identities and retained connected deployment evidence are legacy
and cannot be reused. The next connected candidate must be built from the clean
state above and produce a new package, deployment, Resource replay and Gate
identity.

The attempt counts and `LW_EXECUTION_OPERATION_BUDGET_EXHAUSTED` diagnostic
below are historical pre-reset observations only. The user explicitly
authorized their destruction as part of the clean-validation reset; they are
retained here as a bounded explanation of the prior failure, not as an active
ledger or permission to reuse old evidence.

Before the reset, `resource-application-repair --infra` had
consumed its 3/3 operation attempts, `resource replay repair` had consumed 2/3,
and `package-resource` had consumed 2/3. The build-gate repair is therefore
locally verified but intentionally has not been sent through a fourth
application cycle or an unpaired replay; doing so would bypass the shared
environment convergence policy.

`cargo xtask resource replay` is implemented locally as the only replay entry.
It accepts private profile/authentication/deployment/package locators, validates
their identity chain, performs the Work AgentRun and Resource public API flow,
and emits a root-only sanitized report. `cargo xtask demo replay` now requires
and invokes it before live Playwright and Gate evaluation. The clean reset
removed all previous private profile, authentication, Resource deployment
manifest, package locator and replay report inputs; a new controlled bundle
must be provisioned before connected execution. This path remains `implemented;
ready for human review`; connected verification is delegated to Sprint-end
acceptance Issue #126. No local or stale report is treated as connected
evidence.

The replay driver now waits on the public Control candidate view until a
Container candidate's authoritative build projection is `succeeded` and its
artifact/policy evidence is present before publishing the Work release. A
failed or cancelled projection returns its stable diagnostic, and a missing,
invalid or timed-out projection blocks without attempting release publication.
This removes the approval-to-build race observed by the previous connected
attempts; it has not yet been connected-verified under the remaining execution
ledger budget.

The Resource public HTTP surface is now implemented locally: owner-scoped list
and get, create, approve/resize-approve, cancel, reject, retry, Lease renew and
Lease revoke all use explicit actor/caller checks, idempotency keys and revision
fences backed by the PostgreSQL store. Migration 0006 adds a durable
Lease-revision acknowledgement fence. Renewal is synchronized over the
Resource mTLS boundary and may only extend the exact Work aggregate. The
Resource listener now requires a CA-verified client certificate with the exact
`spiffe://labweaver/access-service` URI SAN. Access supplies actor, session and
roles only through a short-lived signed delegation bound to the BFF session;
the Resource API does not trust caller, actor or role HTTP headers. Revocation
and database-clock expiry now share one persistent saga: Environment revokes
Access before deletion, Resource waits for the Environment tombstone and exact
namespace-absence readback, then and only then marks the capacity claim and
Lease terminal. Cleanup failures remain `expiring`/`releasing`, retain a stable
diagnostic and append a bounded attempt record instead of releasing capacity.
The Resource quota shell uses the same deterministic `lw-env-*` namespace that
Environment adopts, so the approved quota governs the actual workload. Namespace
deletion is fenced by the observed claim labels/annotations, UID and
`resourceVersion`; a same-name or conflicting object is rejected. Every request
terminal transition and Lease activation, renewal, revocation, expiry and
capacity-release transition enqueues its versioned v1 event in the same SQL
transaction as the state change. Rejected and cancelled requests use distinct
event subjects rather than the submitted subject.

### PR #147 review remediation (local, not connected-verified)

The current working tree contains the fixes requested by D's review: the
Resource mTLS/delegation boundary, claim-fenced namespace cleanup with
collision tests, lifecycle Outbox atomicity, and corrected request event
semantics. Local Rust, contract/schema, and Ansible fixture checks pass. A
same-identity package, connected replay, Release Gate v3 and B/D human review
remain outstanding and are not claimed by this local status.

The retained application deployment at source `46d22482` remains connected on
the demo cluster: Resource is digest-pinned and Ready, migrations 0001-0005
have an audited ledger, and a real create/approve flow produced a read-back
ResourceQuota shell and active Lease. Migration 0006 and the renewal/cleanup
saga are implemented and locally verified but require a new same-identity
package, deployment and connected replay before they are marked verified.
The current source identity cannot reuse this retained evidence. The
operation-wide connected ledger now stops additional blind candidates after
its budget; a new package/deployment/replay requires an explicitly audited
validation window rather than ledger deletion or a second controller root.

The former NATS operator seed was not recovered and is retired. A controlled
forward rotation preserved only the reviewed `WORKLOADS` account key, created a
new operator/SYS authority, and reissued all ten JWT/mTLS identities. Two
independent playbook runs retained seven streams and five consumers, rejected
the immediately preceding credentials, observed zero pending Resource Outbox
rows, and left all nine NATS-bearing Deployments Ready. The Environment identity
can publish `labweaver.resource.lease.verify.v1`; the bounded Resource
request/reply path returned the exact active Lease. Root-only authority,
deployment, connected-verification and rollback records use the controlled
locators documented in `AGENTS.md`; no credential value or secret hash is a
committed artifact.

Stale provisioning recovery preserves its authoritative failed phase, and the
Resource process exits when its background runtime fails so Kubernetes can
restart it. A deliberately invalid fixture handoff produced three auditable
`retry`, `retry`, `failed` attempt rows without taking Resource down.
The prior Environment handoff blocker remains historical evidence only: the
retained catalog at that identity contained no approved `work` release. The
current implementation must create an approved Work release through the
teacher/Agent/approval path; no database shortcut or invalid fixture counts as
positive handoff evidence.

Local evidence includes Resource unit tests covering mTLS/delegation identity,
claim-fenced cleanup and lifecycle fencing, sixteen Environment unit tests,
strict all-target/all-feature Clippy for affected crates, contract and generated
Web SDK drift checks, migration catalog validation, migration/store test
compilation, Ansible fixture tests, Web lint/typecheck, format and diff checks.
Docker-backed PostgreSQL execution is unavailable on the local Windows host; the
new PostgreSQL migration and saga still require connected execution.

## Accepted Sprint 2 security exceptions

- Rootless BuildKit may use the documented namespace-scoped
  `Unconfined`/`spc_t`/no-process-sandbox settings. It remains
  non-privileged and may not use HostPath or host networking.
- Container and KubeVirt executors retain broad namespace CRUD for Sprint 2.
  Separate ServiceAccounts and application ownership checks are compensating
  controls, not least-privilege proof.
- Agent Service has unrestricted outbound access. A teacher-approved
  Container may independently use `network.mode=allow_all`; the exception does
  not extend to KubeVirt, BuildKit, Evaluation or other workloads.
- `labweaver-system` uses the documented Pod Security baseline exception for
  the OpenSSH Gateway. Other workloads retain their restricted security
  context.

## Release decision

Issue #142 is **implemented locally; ready for human review**. Its ordinary
development gate is local integration, contract, Ansible/Helm render, targeted
tests and CI evidence. Shared-cluster deployment, Resource replay, real
Container/KubeVirt evidence, connected Playwright and Release Gate v3 are
delegated to Sprint-end acceptance Issue #126 and must not be duplicated from
PR #147. #142 cannot be marked Done or release-ready until #126 completes its
frozen-identity acceptance window.

PR #147 still requires the appropriate human core/security review and resolved
review threads; it is not a connected acceptance or release pass. The author
must not approve or merge the PR. No Tag, formal release or `main` merge is
included.
