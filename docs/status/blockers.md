# Active Blockers

## Platform image trusted supply chain (#62)

- The seven-image build, static validation, Helm/Kyverno policy and connected
  package/deploy/rollback implementation exist. All seven local images have
  reproducible subject digests, SBOM/provenance and runtime filesystem checks;
  Trivy reports zero Critical and zero secrets, with all 16 Web High findings
  retained for review. Local Docker/BuildKit evidence does not replace the
  gated `develop` Actions publication or controlled connected verification.
- Issue #61 must replay its private Sigstore evidence against the merged source
  identity. GHCR, Kyverno and the controlled Linux router require a read-only
  baseline before any connected #62 action.
- Owner: A for implementation and release judgment; B for high-risk review; D
  for independent Verify and Draft PR creation.
- Exit condition: production Config/Secret locators are available; the read-only
  baseline shows no conflicting deployment; one controlled run binds the source
  commit, cluster UID, trust revision, seven reproducible digests, SBOM,
  provenance, Trivy DB/report, certificate/SCT/Rekor proof, rollout/readiness,
  positive and negative admission, verified rollback and cleanup. Any conflict
  with another deployment stops the run.
- Impact: Issue #62 remains `Blocked`; local and CI evidence must not be described
  as a signed GHCR publication or E3 deployment.

## Cross-role Day 1 gate

- Frontend build and B/C/D first branches or PRs are not role A deliverables.
- Owner: B, C and D respectively.
- Exit condition: each owner supplies its own reviewed PR and required evidence.

## PostgreSQL persistence implementation

- Issue #46 implements SQLx persistence, database roles, Migration files,
  controlled Migration CLI, release/domain locks, local Outbox/Inbox and Docker
  PostgreSQL integration coverage. JetStream publishing, audit projection and
  service startup/readiness wiring remain out of scope.
- Owner: A for the persistence/release boundary; B must complete the required
  high-risk Migration review and approve ADR 0002 with A before it can become
  accepted.
- Exit condition: A+B approve the high-risk Migration boundary and D verifies
  the current Docker evidence; subsequent scoped issues add service readiness,
  JetStream publication, replay/backfill and audited forward repair.
## ACCESS-01a implementation evidence

- The dual-path trust boundary is documented, but no Keycloak handoff, Access Service grant persistence, Headscale Grants compiler, Router firewall controller, Guacamole extension, scoped SSH/VNC credential issuer, containment receipt or VM-stop escalation exists in the current evidence identity.
- Owner: A for Access contracts, policy boundary and Guacamole handoff; B for Environment endpoint and scoped-credential integration; D for deployed verification and replay evidence.
- Exit condition: reviewed contracts and implementations provide E1/E2 dual-revision, device-scope and credential tests; E3 deployed Headscale, Router and Guacamole evidence; and E4 multi-device/multi-role replay proving direct and browser paths, 60-second Router isolation, unaffected valid grants and escalation behavior.
## NATS runtime implementation

- Issue #18 freezes the public Subject, CloudEvents, Outbox, ordering, consumer
  and quarantine design. Issue #51 supplies the Environment-owned client,
  durable Consumer binding, Outbox publisher, quarantine and runtime envelope
  path at local E2; other domains, shared audit projection, deployment manifests
  and deployed NATS identity remain unimplemented.
- Owner: A for the contract and message boundary; B must complete the required
  high-risk messaging review before implementation.
- Exit condition: a separately scoped implementation issue provides E2
  PostgreSQL and JetStream evidence for atomic Outbox publication, duplicate
  and replay idempotency, stale/gap sequence blocking, durable-consumer
  recovery, acknowledgement behaviour and terminal quarantine diagnostics.

## ENV-02a E2 and deployment boundary

- Issue #51 now has local E2 lifecycle/repository/messaging/reconciler and
  owner-resolver evidence from Docker PostgreSQL 17, NATS JetStream 2.11 and a
  real rustls mTLS server. It includes exhaustive matrices, Inbox ordering,
  acknowledged Outbox replay, optimistic conflict, idempotent new-worker
  recovery after Provider side effect with durable Provider-step identity,
  production first-aggregate creation, exact Active Resource Lease gating,
  failed-phase/reset-target persistence, timeout/cancel cleanup, SAN bounds,
  strong ETag, database-clock expiry, certificate rotation, typed shutdown
  failure and outage coverage.
- Owner: B for Environment implementation; A for the reviewed Access/mTLS
  boundary; D for PostgreSQL, certificate-rotation, outage and deployed Verify.
- Exit condition for Issue #51 acceptance: A approves the high-risk contract,
  Migration and mTLS boundary, D verifies the current E2 commands and build
  identity, CI passes, and the PR is merged. The Access-owned revocation
  responder, Resource-owned Lease responder, connected verification of the #52
  Container Provider, a formal KubeVirt Provider and deployed mTLS NATS path
  remain explicit adjacent/E3 dependencies and may not be replaced with
  integration fixtures or fallback evidence.
- Impact: #47 now binds Environment-scope authorization to the merged resolver
  through real mTLS E2 coverage. A+B review, D same-build Verify and deployed
  Gateway/internal-DNS evidence remain blockers for Issue closure and E3.

## Agent Tool permission and approval contract

- Tool bindings do not yet model or enforce filesystem, network or runtime permissions.
- Elevated/high-risk Tools fail closed because no reviewed approval evidence contract exists.
- Owner: A freezes permission vocabulary and approval identity, revision, expiry and candidate/input
  binding; B implements the accepted contract.
- Exit condition: reviewed contract plus negative tests for permission escalation, stale/mismatched
  approval and repeated or changed-input dispatch.
- Impact: Issue #13 remains partially implemented and must not be submitted as complete.

## IMG-02a connected Container evidence (#52)

- The v2 Control, Agent and Environment paths have local E1 and PostgreSQL E2
  evidence, but the deployment-owned BuildKit/Harbor/Trivy/Private Sigstore and
  Kubernetes executor subjects have no connected same-build implementation or
  replay in this worktree.
- Owner: B for the executor integration and Container behavior; A for the
  high-risk Contract/security review; D for independent connected Verify.
- Exit condition: one reviewed build identity proves the per-course private
  Harbor Project/quota/robot, immutable context and base digest, SBOM/in-toto,
  Trivy policy, Fulcio/SCT/Rekor proof, immutable publication, private pull,
  code-server readiness through only the protected Gateway, and
  timeout/cancel/retry/finalizer cleanup with no reachable residual resource.
- Impact: Issue #52 does not meet its E3 acceptance criteria and must not be
  marked done or presented as a real Container runtime deployment.

## AG-01b Fixture Backend

- `environment.yaml` generation is blocked because the Environment domain vocabulary and Schema in
  Issue #16 are not frozen or implemented.
- Evaluation/LLM fixture generation is also deferred to AG-01b and is not evidence for AG-01a.
- Owner: A for the Environment domain decision; B consumes the accepted contract in AG-01b.
- Exit condition: reviewed Environment type/Schema with candidate and teacher-approval semantics.
- Impact: Issue #13 does not claim a Fixture Backend or Environment + Evaluation generation path.

## Resolved blockers

- GitHub Project write scope was restored and Issue #20 was closed as completed.
- All 20 governance Issues are present in `LabWeaver Delivery`; Issues #5–#19 were read back with `Workflow Status=Ready` and `Delivery Priority=P0`.
- GitHub exposes built-in status/priority/date fields as Issue-derived fields. Writable Scrum metadata therefore uses `Workflow Status` and `Delivery Priority`, while `Target date` is updated through the Issue field API.
