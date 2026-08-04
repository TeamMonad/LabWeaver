# Implementation Status

This document records current repository and connected-runtime facts. Design
documents, fixtures, health endpoints and reports from another source identity
are not completion evidence.

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
| Unified xterm/noVNC ConsoleCapability | contract implemented; runtime blocked | ADR 0012 and the AccessGrant-level discovery/issuance contract bind BFF, Origin, CSRF, idempotency, revisions, conditional Work Lease fences, 30-second one-time locator and safe observability. Generated Schema/OpenAPI/Web SDK and cross-consumer validation are required E2 evidence. #131 owns the Container xterm port, #124 owns KubeVirt/noVNC, and #126 owns shared-cluster E4/Release Gate. No Migration, proxy, UI, runtime or connected evidence is claimed here. |
| Web console Fixture preview | implemented; Fixture-verified | `pnpm --dir web preview:console:fixture` builds and serves the EX3-derived deterministic browser preview without a backend. It is visibly marked Fixture, renders only Fixture state, and is documented in `docs/testing/fixture-console-preview.md`. It is layout/state-machine evidence only, not Access proxy, Container, KubeVirt, connected-runtime, or Release Gate evidence. |

## Current identity

- Draft PR: #121, branch `release/sprint2`, target `develop`
- Source commit: `748c2470ad0f3fba848761f0113853a6870576d6`
- Cluster UID: `171e3e6b-1e8b-4666-9936-b5f8a514132e`
- Package manifest:
  `sha256:6fa824800ac83e51c826242128687b4734622bdcf348969fed8dae4c89cc63d9`
- Configuration bundle:
  `sha256:564cf33c34b8d851c349f8e45bf10da34665ea9ad05d21e4fe49199292fe6518`
- Migration catalog:
  `sha256:0c9d5dbac9c9f7855147f35f21ea492a8ec1a5f7d4a1fff49e9b413c1e0ef1c3`
- Non-destructive application runs: `deploy-748c2470` and
  `deploy-748c2470-repeat`
- Release Gate Run:
  `a2835d47-7f9b-48a3-b8a0-60d22f57d5e2`

The second application reconcile completed at Helm revision 265. All ten
declared Deployments read back one ready replica and use immutable Harbor
digests. Kubernetes, KubeVirt, PostgreSQL, NATS, MinIO, Harbor and Keycloak
were retained; no infrastructure reset was used.

## Sprint 1 and Sprint 2 connected result

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
| Machine-readable Release Gate | verified | `cargo xtask release-gate` produced a schema-valid passing report for Run `a2835d47-7f9b-48a3-b8a0-60d22f57d5e2`. Evidence remains private/ignored and contains no Secret. |
| Full `cargo xtask demo replay` | not run | The authoritative gate was run directly after connected checks. The wrapper's repeated infrastructure Verify and Playwright execution was intentionally skipped; this is not represented as a replay pass. |

## Issue #140 local implementation

The `feature/140-oj-cpp17-runner` worktree adds the internal C++17 OJ request,
checker, deterministic aggregator, evidence/receipt validation, isolated
Kubernetes resource plan and executor, shell-free worker modes, dedicated
digest-pinned image and focused tests. Review hardening keeps full request
digests in annotations, requires an Ansible-provisioned namespace default-deny
before Job creation, rejects process-group/namespace escape syscalls, limits
student processes, and binds deletion to UID/resourceVersion preconditions.

Its current evidence is **E1 only**. The implementation is not connected to a
public EvaluationRun or StepRun because Issue #123's authoritative lifecycle,
Outbox and persistence contract is not present on the current `develop`
baseline. No local container daemon or real Kubernetes run has verified the
compiler image, cgroup outcomes, NetworkPolicy, cancellation or cleanup.
Accordingly Issue #140 is not `done`, is not a release-gate pass and does not
change the connected Sprint 2 identity above.

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

Release Gate v2 requires a Resource deployment manifest, immutable
`resource-service` identity and `resource-lease` connected evidence. v1 reports
remain legacy and cannot close #142. A same-identity Resource replay on a clean
validation environment remains required before this is marked verified.

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
therefore invalid for the next candidate; only evidence produced after the new
package, deployment and Resource replay may close #142.

The retained Harbor Trivy deployment had a real NFS `root_squash`/kubelet
`applyFSGroup` blocker. The Harbor Ansible role now applies
`fsGroupChangePolicy: OnRootMismatch` with `fsGroup: 10000` before readiness;
the live StatefulSet was reconciled and read back `1/1 Ready`. This repair is
part of the source-of-truth role and is covered by the Ansible fixture test.
The sanitized repair readback is retained at
`remote://edge-router/var/lib/labweaver/cleanup/resource142-clean/resource142-trivy-fsgroup-repair-20260804T040500Z/manifest.sha256`
with readback digest
`sha256:23a10e75f622577d29a04872e869a397d56b5d2c7431d666046e28ffc8d821f`.

The current source identity is `40318900`. Local `xtask` tests (38 passed),
strict `xtask` Clippy, format and diff checks pass; the Resource replay input
tests (13 passed) also cover the asynchronous Container build gate. Retained connected
deployment evidence is bound to the earlier `611013a2` source and cannot be
reused for this source identity. The next connected candidate must be built
from the clean state above and produce a new package, deployment, replay and
Gate identity.

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
must be provisioned before connected execution. This path remains
`implemented; connected verification blocked`; no local or stale report is
treated as connected evidence.

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
Resource mTLS boundary and may only extend the exact Work aggregate. Revocation
and database-clock expiry now share one persistent saga: Environment revokes
Access before deletion, Resource waits for the Environment tombstone and exact
namespace-absence readback, then and only then marks the capacity claim and
Lease terminal. Cleanup failures remain `expiring`/`releasing`, retain a stable
diagnostic and append a bounded attempt record instead of releasing capacity.
The Resource quota shell uses the same deterministic `lw-env-*` namespace that
Environment adopts, so the approved quota governs the actual workload.

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

Local evidence includes six Resource unit tests, sixteen Environment unit
tests, strict all-target/all-feature Clippy for affected crates, contract and
generated Web SDK drift checks, migration catalog validation, migration/store
test compilation, 53 Ansible fixture tests, Web lint/typecheck, format and diff
checks. Docker-backed PostgreSQL execution is unavailable on the local Windows
host; the new PostgreSQL migration and saga still require connected execution.

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

The implementation and connected technical gate for Sprint 1+2 are verified
for the identity above. PR #121 remains **blocked from merge** because it is a
Draft high-risk PR and still requires:

1. B human review and approval for core/security changes;
2. C human review for Web changes;
3. D connected Verify and acceptance;
4. resolution of any resulting review threads and a final green CI readback.

The author must not approve or merge the PR. No Tag, formal release or `main`
merge is included.
