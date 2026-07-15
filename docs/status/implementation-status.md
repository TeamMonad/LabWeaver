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
| Environment management Public API (#81) | A | contract and authenticated SDK transport implemented in current worktree, pending A+B review and D Verify | generated inventory/operation/AccessGrant schemas and Public OpenAPI, Rust negative contract tests, Chromium bearer/RFC 9457/cancel transport tests | E1 | no authorized Handler, persistence query, Outbox/SSE runtime, console UI, deployment or E2/E3 multi-role evidence |
| Issue #48 Control plane | A with B-owned Agent boundary; pending A+B review and D Verify | production path implemented in current worktree | public Control and mTLS Control-to-Agent APIs; immutable ProblemPackage upload completion; append-only course LLM policy, decisions, releases and withdrawals; Agent PostgreSQL dispatch leases; acknowledged Agent Outbox publisher; transactional Control Inbox/projection/SSE consumer; durable SSE; additive Control/Agent Migrations; PostgreSQL 17 migration, completion fencing/recovery/orphan-cleanup and 20-writer per-course SSE sequence tests; real versioned MinIO presign/freeze/overwrite/exact-version/cleanup test; real JetStream publisher ACK failure/retry and Control duplicate/gap/restart/outcome-outage recovery tests; ephemeral-CA Gateway-to-Control and Control-to-Access/Agent SAN, certificate-rotation and outage tests | E2 for the local same-worktree PostgreSQL, MinIO, JetStream and mTLS control-plane composition | #52/#53 do not provide an authoritative artifact projection, therefore positive production Release creation remains fail closed; internal Agent route coverage is local rather than deployed; no Evaluation execution, scoring, image build, Environment creation or E3 claim |
| EvaluationSpec and GoalReview v1 | B/A | implemented as part of Contracts v1, pending review | generated schemas and OJ/Linux positive/negative contract tests under `contracts` | E1 | no Runner execution, persistence, messaging or production approval path |
| Environment lifecycle and owner resolver (#51) | B/A | local E2 implementation in current worktree, review feedback addressed; pending A re-review and D Verify | exhaustive lifecycle matrices; Docker PostgreSQL 17 atomic first-create, Migration, idempotency, Inbox, reconcile, timeout/cancel and Provider-step recovery tests; real JetStream 2.11 command/Outbox/Resource-Lease/provider-adapter test; real rustls mTLS SAN/rotation/ETag/database-clock/shutdown/outage test | E2 | Resource and Access responders are integration fixtures rather than their owner-service implementations; no formal Container/KubeVirt Provider or E3 deployed mTLS NATS/cleanup evidence; author cannot supply A approval or D Verify |
| Linux Nginx material contract | A | implemented in current worktree, pending review | public-safe example package, candidate manifests, Python material validator and reviewed contract | E1 | no approved VM image, SubmissionManifest Reader, full Probe capability, KubeVirt VM, or E3 evidence; B owns the blocking runtime contract |
| Agent state and Tool contract | B | partially implemented, blocked | `agent-core` state, capability binding, timeout/cancel/no-retry, idempotency identity propagation, output validation, diagnostic ownership, audit and negative tests | Partial E1 | role A must freeze Tool permissions and approval evidence; the generic Tool dispatcher is not wired into a service path and AG-01b Fixture Backend remains unimplemented; the separate Claude Code path intentionally exposes no Tools |
| Claude Code Agent process-level runtime boundary | B | implemented in current worktree, pending A+B review and D Verify | `ClaudeCodeBindingV1`, generated Schema, ProblemPackage hash/classification gate, pinned shell-free worker, exact-Schema JSON prompt, strict local typed/semantic validation, terminal `stream-json` parsing, explicit-environment per-invocation HOME/XDG/tmp/workdir, bounded worker semaphore, hash-only audit, per-track PostgreSQL lease/heartbeat/cancellation/checkpoint and Outbox implementation; PostgreSQL 17 migration/replay/10-identical-request and 20-distinct-run contention across 4 workers/lease-reclaim/cross-worker-cancel integration passed on 2026-07-15; the current isolated process path generated a real local Environment candidate with CLI `2.1.209` in 11.83 seconds at 35,321 microusd | E1 runtime/process boundary plus live local provider evidence; E2 persistence/recovery | Evaluation and dual-track real-provider calls are not yet verified; explicit local provider environment is not deployment Secret/Config evidence; no pinned worker image/config verification, HTTP handler, Control candidate projection, JetStream publisher/consumer, object-store checkpoint reference, isolated Kubernetes Job or deployed multi-container/E3 credential path |
| Playwright role-project configuration | D (implemented in `test/9-playwright-role-projects`, pending C/A review) | implemented configuration, pending review | exactly four projects, aggregate E1 gate, static/subprocess contracts and fail-fast entrypoints | E1 | requirements baseline PR #36 @ `a9bc7a8ab013a35a846a4b428bad22ecc48eca1b` merged by `0f80e4e9c4b2334d4a833d1fb6a2263ecc3dda9a`; integration baseline `develop` @ `8ec186599f82afeab7ff5bed346c844ce7f923d1`; researcher requires a separate approved Project/auth-state Issue; CI records baseline metadata but does not receive an externally pinned baseline-change input; no auth setup, browser runtime, role isolation, E3 or E4 evidence exists |
| Frontend, Agent runtime and Playwright work | C/B/D | planned | assigned Sprint Issues | E0 | real UI behavior, authentication, authorization and browser evidence remain unimplemented |
| Adopted-cluster infrastructure baseline TestFlight | B/D | implemented, pending human Verify | Issue #15, scoped `InfrastructureTestFlightReport` bound to the current deployment identity | E3 baseline | proves RWO/RWX, KVM VM lifecycle, internal Gateway, Cilium control-plane, etcd backup evidence and cleanup only; OIDC/governance is deferred to #47 and extended security/recovery remains under #2 |
| Private Sigstore trust plane (#61) | A implementation; requires A+B security review and D Verify | deployed on the adopted cluster; current merge identity requires replay | source identity `3509e14` completed deploy, backup-first lifecycle, exact OIDC subject keyless signing, Fulcio certificate issuance, CT SCT, Rekor inclusion, bundle verification, isolated restore/DR drills, plan-only rotation, cleanup and a second idempotent deploy; reports are schema-validated and identity-bound | E3 for the pre-merge source identity | evidence must be regenerated for the merge commit before D starts T0; self-signed private-lab CA and deferred Kyverno/Release Gate scope remain explicit limitations |
| Keycloak identity foundation for #61 | A, requires B security review and D Verify | deployed on the adopted cluster | fixed-digest PostgreSQL/Keycloak Ready, namespace CA/TLS Ready, Gateway `10.20.0.222` Programmed, exact issuer and immutable workload `sub`/`aud`/`azp` verified; independent read-only verifier evidence is recorded in `docs/testing/evidence/identity-foundation-verifier-20260715.md` | E3 identity baseline | self-signed private-lab CA is not a public PKI; PostgreSQL recovery and D Verify remain required |

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

Issue #47 has a complete local E2 implementation on its dedicated feature
branch: the Access Service has configuration-validated OIDC Discovery,
Authorization Code + PKCE state/nonce handling, AEAD-protected PostgreSQL BFF
transaction/session/logout-hint records, synchronizer-CSRF logout with
RP-Initiated Logout, bearer/back-channel JWKS validation, and a separate Rustls
mTLS listener. The listener requires a client-authenticated CA chain,
an allowlisted URI SAN, and a live registered `service_identities` row before
it accepts `/internal/v1/auth/decision`. The decision route binds the requested
actor to a live BFF session, reloads course/project memberships for every
decision, checks the generated operation role/scope catalog, and returns an
expiry-bounded decision. Environment scope additionally calls the merged #51
owner resolver over configured mTLS and binds actor, course, environment,
Environment revision, strong ETag and eligibility expiry. Generated OpenAPI now
includes the allowed roles and scope kind for every catalog operation.

Current E2 evidence includes the controlled SQLx PostgreSQL container path
(session rotation, encrypted CSRF/logout-hint restoration, SID/direct
revocation, authoritative membership reload and service identity), a real
ephemeral-CA Access-to-Environment mTLS handshake with denial/tamper/outage,
and a digest-pinned HTTPS Keycloak 26.3 run
(`sha256:08a31919cfcd814bf1b465142b1a716c4d1a8830f772bb5c9dffcbd96de3fba6`).
The Keycloak run completed HTML
login, code exchange, nonce/issuer/audience/`azp`/role checks, provider logout,
two RSA signing-key rotations, custom-CA unknown-`kid` refresh and empty-JWKS
fail-closed behavior.

The PR review TLS findings are addressed: strict Discovery rejects non-HTTPS
authorization/token/JWKS/logout endpoints, an OIDC private CA replaces system
roots, and the Owner Resolver always uses an exclusive configured CA. A
double-opt-in `insecure-test-only` mode exists only for disposable loopback
tests; configuration plus `LABWEAVER_ENABLE_INSECURE_AUTH_TEST_MODE=1` are both
required. Unit/integration coverage proves remote HTTP rejection, loopback HTTP
Discovery, invalid loopback test certificates, and the unchanged strict
private-CA Keycloak path.

Human A+B review, D same-build Verify, controlled client-certificate rotation,
deployed metrics validation and real Gateway/internal-DNS/TLS verification
remain incomplete. Back-channel token validation and SID revocation are covered
below the HTTP boundary, but real Keycloak back-channel HTTP delivery is still
an E3 dependency. No E3, E4, production deployment, or Issue closure claim is
made.

The Issue #47 design preference for `tower-sessions` SQLx storage is not
currently usable with this workspace: the available `tower-sessions` 0.15
store is compiled against Axum 0.7, while the services are on Axum 0.8. It
cannot satisfy the required `SessionStore` trait or provide a valid Axum 0.8
extractor. The service therefore keeps the controlled `access.bff_sessions`
store and its catalog migration for now; replacing it requires an approved
compatible upstream release or a reviewed framework migration, not an unsafe
dual-Axum workaround.
