# Implementation Status

Status is proven only by the identified commit/worktree and current evidence. `docs/draft/` is not completion evidence.

| Capability | Owner | State | Evidence | Level | Limitation / blocker |
| --- | --- | --- | --- | --- | --- |
| Rust Workspace and foundation crates | A | implemented | `cargo xtask check` passed after PR #21 merge | E1 | no business domain implementation |
| Six Axum service shells | A | implemented | `cargo xtask check` passed; Control live/ready smoke passed | E1 | health-only; no persistence, messaging or providers |
| GitHub Milestones, Labels and Sprint Issues | A | configured | GitHub API read-back | E0 | remote metadata only |
| GitHub Project fields and Ready assignments | A | configured | API read-back: 15 P0 items have `Workflow Status=Ready` | E0 | Issue #20 resolved; remote metadata does not prove product behavior |
| Testable requirements baseline | D (A temporarily implementing Issue #8) | documented, pending review | `docs/requirements/` impact map, journeys, 3C stories and acceptance matrix | E0 | requirements are targets only; no runtime capability or P0 release evidence is added |
| Branch protection | A | configured | GitHub API read-back for `main` and `develop` | E0 | required `rust-gate` starts with API-01a PR |
| C4, service boundaries and data ownership | A | documented, pending review | current documentation PR | E0 | design evidence only |
| PostgreSQL schema ownership and Migration policy | A | implemented, pending A/B review and D Verify | `persistence-sqlx`, immutable catalog, controlled `cargo xtask migrate`, Docker PostgreSQL integration test and migration report schema | E2 | no service startup/readiness wiring, JetStream publisher/consumer or audit-projection worker |
| Identity, Tailnet, DirectAccessGrant and Guacamole trust boundary | A | documented, pending B/D review | ACCESS-01a formal architecture documentation and ADR 0001 | E0 | no OIDC, Headscale Grants, Router firewall, Guacamole extension, grant store, containment or VM-stop implementation/evidence |
| NATS Subject v1 and delivery contract | A | Environment slice implemented locally, broader catalog pending A/B review | ADR 0003, NATS event catalog, Environment real JetStream command/Outbox integration test | Partial E2 | Environment path has client, durable consumer and acknowledged Outbox publisher; shared quarantine, other domains and deployment evidence remain unimplemented |
| Contracts v1 semantic source of truth | A | implemented in current worktree, pending A+B review and D Verify | `contracts` tests, generated JSON Schema, dual OpenAPI, Axios SDK and byte-drift gate | E1 | no Handler, persistence, Outbox/JetStream, Provider, Gateway, UI, deployment or EvaluationRun runtime evidence |
| EvaluationSpec and GoalReview v1 | B/A | implemented as part of Contracts v1, pending review | generated schemas and OJ/Linux positive/negative contract tests under `contracts` | E1 | no Runner execution, persistence, messaging or production approval path |
| Environment lifecycle and owner resolver (#51) | B/A | local E2 implementation in current worktree, review feedback addressed; pending A re-review and D Verify | exhaustive lifecycle matrices; Docker PostgreSQL 17 atomic first-create, Migration, idempotency, Inbox, reconcile, timeout/cancel and Provider-step recovery tests; real JetStream 2.11 command/Outbox/Resource-Lease/provider-adapter test; real rustls mTLS SAN/rotation/ETag/database-clock/shutdown/outage test | E2 | Resource and Access responders are integration fixtures rather than their owner-service implementations; no formal Container/KubeVirt Provider or E3 deployed mTLS NATS/cleanup evidence; author cannot supply A approval or D Verify |
| Linux Nginx material contract | A | implemented in current worktree, pending review | public-safe example package, candidate manifests, Python material validator and reviewed contract | E1 | no approved VM image, SubmissionManifest Reader, full Probe capability, KubeVirt VM, or E3 evidence; B owns the blocking runtime contract |
| Agent state and Tool contract | B | partially implemented, blocked | `agent-core` state, capability binding, timeout/cancel/no-retry, idempotency identity propagation, output validation, diagnostic ownership, audit and negative tests | Partial E1 | role A must freeze Tool permissions and approval evidence; the generic Tool dispatcher is not wired into a service path and AG-01b Fixture Backend remains unimplemented; the separate Claude Code path intentionally exposes no Tools |
| Claude Code Agent process-level runtime boundary | B | implemented in current worktree, pending A+B review and D Verify | `ClaudeCodeBindingV1`, generated Schema, ProblemPackage hash/classification gate, pinned shell-free worker, exact-Schema JSON prompt, strict local typed/semantic validation, terminal `stream-json` parsing, explicit-environment per-invocation HOME/XDG/tmp/workdir, bounded worker semaphore, hash-only audit, per-track PostgreSQL lease/heartbeat/cancellation/checkpoint and Outbox implementation; PostgreSQL 17 migration/replay/10-identical-request and 20-distinct-run contention across 4 workers/lease-reclaim/cross-worker-cancel integration passed on 2026-07-15; the current isolated process path generated a real local Environment candidate with CLI `2.1.209` in 11.83 seconds at 35,321 microusd | E1 runtime/process boundary plus live local provider evidence; E2 persistence/recovery | Evaluation and dual-track real-provider calls are not yet verified; explicit local provider environment is not deployment Secret/Config evidence; no pinned worker image/config verification, HTTP handler, Control candidate projection, JetStream publisher/consumer, object-store checkpoint reference, isolated Kubernetes Job or deployed multi-container/E3 credential path |
| Playwright role-project configuration | D (implemented in `test/9-playwright-role-projects`, pending C/A review) | implemented configuration, pending review | exactly four projects, aggregate E1 gate, static/subprocess contracts and fail-fast entrypoints | E1 | requirements baseline PR #36 @ `a9bc7a8ab013a35a846a4b428bad22ecc48eca1b` merged by `0f80e4e9c4b2334d4a833d1fb6a2263ecc3dda9a`; integration baseline `develop` @ `8ec186599f82afeab7ff5bed346c844ce7f923d1`; researcher requires a separate approved Project/auth-state Issue; CI records baseline metadata but does not receive an externally pinned baseline-change input; no auth setup, browser runtime, role isolation, E3 or E4 evidence exists |
| Frontend, Agent runtime and Playwright work | C/B/D | planned | assigned Sprint Issues | E0 | real UI behavior, authentication, authorization and browser evidence remain unimplemented |
| Adopted-cluster infrastructure baseline TestFlight | B/D | implemented, pending human Verify | Issue #15, scoped `InfrastructureTestFlightReport` bound to the current deployment identity | E3 baseline | proves RWO/RWX, KVM VM lifecycle, internal Gateway, Cilium control-plane, etcd backup evidence and cleanup only; OIDC/governance is deferred to #47 and extended security/recovery remains under #2 |

## Kubernetes infrastructure automation

State: implemented, pending review; the router replay is complete for the
currently scoped baseline.

The Ansible playbooks encode the currently validated Rocky Kubernetes baseline:
Kubernetes/CRI-O, Cilium, MetalLB, Local Path, NFS CSI, cert-manager, KubeVirt,
CDI, Kyverno, internal Gateway, and etcd backup. The prior manual environment
provided E3 evidence. The router worktree has now completed a guarded deploy,
backup, TestFlight verification, and a second idempotent deploy under the same
source identity.

Linux CI continues to provide lint, syntax, fictional encrypted-Vault,
preflight-chain, and storage-safety fixture evidence. The router is now the
only Linux execution authority for the real controller path; its evidence is
recorded below. Bootstrap remains intentionally out of scope for an adopted
cluster, and the existing baseline is read-only validated before Harbor
reconciliation.

## Harbor infrastructure (Issue #23)

State: manual adopted-cluster deployment established; guarded controller
reconciliation, router-side ansible-rs entrypoint, and sanitized manifest schemas
are implemented in this worktree. The next router rerun is blocked until the
new controller-identity locator and identity-bound backup evidence are present;
the earlier replay must not be treated as evidence for those new controls.

The manual deployment uses chart `1.19.1` and Harbor `2.15.1`, a dedicated
namespace, internal CA/TLS, a separate Cilium Gateway/HTTPRoute, a dedicated
router DNS fragment, the `local-path`/`nfs-rwx` single-instance storage split,
and the private `labweaver-system` project. The Trivy volume ownership repair
was scoped to the newly-created Harbor PVC after the NFS CSI provisioner created
it with anonymous ownership. It is deployment evidence only, not a replacement
for the future reconciler's first-run and replay evidence.

The TestFlight report is schema-validated for the `adopted-cluster-baseline`
scope. Keycloak OIDC is intentionally deferred to #47; registry push/pull/scan
policy replay, immutable-tag and retention enforcement, recovery drills, a
reviewed Cilium policy for host-network Gateway traffic, and Release Gate
evidence remain Sprint 2/#2 work. They do not make #23 or the baseline #15
release-ready, but are not blockers for their bounded close conditions.
