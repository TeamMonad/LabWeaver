# Implementation Status

This document records current repository and runtime facts. Design documents,
fixtures, generated schemas, health endpoints, and old deployment reports are
not runtime completion evidence.

Current source line: Draft PR #121 on `release/sprint2`; the exact reviewed head
is read from Git/PR metadata rather than duplicated as a stale status value.

## Sprint 3 runnable Demo contract

Issue #122 freezes the shared execution/evidence contract for Issues #123
through #126 in
[`runnable-environment-demo.md`](../architecture/runnable-environment-demo.md).
It defines the same-identity ledger, Container/KubeVirt resource readback,
delivery DAG and fail-closed Go/No-Go template without changing a public API,
event, Schema or Migration.

This is an implemented documentation contract, not connected runtime evidence.
#123 remains dependent on #63's real application/Gateway deployment; #124,
#125 and #126 remain dependent on their declared upstream implementation and
Verify work. No Sprint 3 Demo, Release Gate pass or `outcome:go` is claimed by
the presence of this document.

## Sprint 1 and Sprint 2

## Current connected slice (2026-07-28)

The current application deployment is bound to source
`24928e8f06e1bc9709c4c493b7d95b2b007f522c`. Package
`pkg-demo-sprint2-24928e8f-24928e8f06e1` completed with exit status zero and
the non-destructive Ansible application runs `deploy-24928e8f` and
`deploy-24928e8f-replay` completed with exit status zero. All ten declared
Deployments reported one ready replica and use Harbor digest references.
The adoption retained PostgreSQL, NATS, MinIO, Harbor, Keycloak, BuildKit,
Kubernetes and KubeVirt service bodies. It did not reset domain data or claim
the historical infrastructure deployment identity as current.

On the current identity, AgentRun
`019fa729-9ca5-7b73-96a1-8744cd18ab73` produced independent Environment and
Evaluation candidates and both received teacher approval. BuildKit built and
pushed the approved Container image to private Harbor, Trivy passed the
publication gate, and Control published release
`019fa72a-94ca-7cf2-8c75-024415ff19b6`. Container
`019fa72a-c932-7b13-9ab1-a10531d7b5b7` reached `ready`; its HTTP AccessGrant
returned 200. The non-root fixed initializer populated an empty PVC from
`/opt/labweaver/workspace-seed` without administrative mutation. Submission
`019fa72b-2b44-7b43-b917-c3feefbce4d6` froze one file with immutable object
SHA-256 `46332fd3cb4658ef82efdb62a3f7ff87feacc30d30b8b4ddffa6ee4ccbfde818`.
Stop, start, application-owned delete and cluster absence readback passed.

The immediately preceding `f4be2776` deployment completed a real KubeVirt
access, freeze and lifecycle replay. An external OpenSSH client passed
public-key authentication, grant authorization, target Service resolution and
exact target host-key validation. Submission
`019fa701-3801-75d1-8538-189df95b1055` froze one file with immutable object
SHA-256 `026de733a9c1cdc34e7e2fac7f541b854a031d39b80da0777b15498fa18953d5`.
Stop, start, delete and cluster absence readback passed. This proves the real
VM path on `f4be2776`, but it is not promoted to the final `24928e8f`
same-identity Release Gate.

On the older `ec6587cb` deployment, one production ECNU AgentRun completed
Environment and Evaluation candidate generation; the teacher approved both
candidates. BuildKit built and pushed the Container image to private Harbor,
Trivy 0.72.0 reported zero Critical findings, and Control published the
immutable digest. A real Container reached `ready`, served its HTTP endpoint
and produced an immutable frozen submission. Real Keycloak teacher/student
Playwright completed with three passed tests and one explicitly skipped VM
test. The older replay workspace file was seeded with an administrative
`kubectl exec`; that evidence remains a declared demo setup limitation. The
current Container resource plan instead initializes an empty persistent
workspace once from the approved image's fixed
`/opt/labweaver/workspace-seed` directory. The initializer is non-root, uses a
fixed command, fails closed when the seed is missing, and does not overwrite an
existing workspace. Connected verification of this corrected path is still
required for the current build identity.

Earlier provider-quota failures remain historical diagnostics only. The current
identity completed a fresh ECNU AgentRun and did not reuse an older candidate,
Fixture or release.

| Check | Result | Evidence / limitation |
| --- | --- | --- |
| Agent and approval | verified on current connected runtime | AgentRun `019fa729-9ca5-7b73-96a1-8744cd18ab73`; Environment candidate `019fa729-f4c2-71c0-a316-6f515e404307`; Evaluation candidate `019fa729-eb66-7cc2-b308-d3880b6c9277`; both have teacher approval. |
| Build and publication | verified on current connected runtime | BuildKit, Harbor and Trivy completed before immutable release `019fa72a-94ca-7cf2-8c75-024415ff19b6` was published. |
| Container create and access | verified on current connected runtime | Environment `019fa72a-c932-7b13-9ab1-a10531d7b5b7` reached authoritative `ready`; HTTP AccessGrant `019fa72b-6d17-7b12-99b6-73df9c46dec4` returned 200. |
| Container freeze and lifecycle | verified on current connected runtime | Automatic non-root PVC seeding produced submission `019fa72b-2b44-7b43-b917-c3feefbce4d6`; stop/start/delete and cluster absence readback passed. |
| Real browser flow | blocked on current identity | Real Keycloak API sessions completed teacher/student mutations, but two bounded current-identity Playwright attempts timed out without a report and are not counted as browser evidence. |
| KubeVirt | verified on immediate predecessor; blocked for final identity | Source `f4be2776` completed real VM Gateway access, immutable freeze, stop/start/delete and cleanup readback. The final `24928e8f` identity has not repeated the complete VM lifecycle and cannot close the terminal gate. |
| Infrastructure Verify | blocked by identity drift | Generic `cargo xtask verify --infra` correctly rejects the retained historical deployment manifest because its commit/inventory/component-lock identity differs from the current private inventory. Application-specific adoption and testflight passed. |
| Release Gate / Sprint 2 | blocked | Current-identity Container closure is complete. Same-identity VM, Access/Gateway negative matrix, rollback drill, current browser report, human review and the complete machine-readable Release Gate remain absent. |

| Capability | Owner | State | Current evidence | Blocker or limitation |
| --- | --- | --- | --- | --- |
| Six service and six PostgreSQL domain boundaries | A | implemented | Cargo workspace, service crates, deterministic catalog and one `0001_sprint2_baseline.sql` per domain; 3a9 non-destructive adoption verified the exact retained ledger across all six domains | Resource has no Sprint 2 production path |
| Contracts, OpenAPI and Web SDK | A/C | implemented locally | one ADR 0011 v1 Rust source with generated JSON Schema, Public/Internal OpenAPI and Web SDK; publication accepts only candidate/approval/runtime identity, while Control resolves Container build evidence or the deployment-locked VM base identity; contract/render and Web drift gates pass | connected clients and deployed schema identity remain pending |
| Keycloak/OIDC and course-scoped authorization | A | connected on ec6587 | Access BFF, bearer/mTLS checks, PostgreSQL tests and retained Keycloak reconciliation; real teacher/student sessions and teacher candidate approval pass through the adopted BFF session and CSRF transport | Access/Gateway protocol denial, expiry and revocation replay remains open |
| Control material, AgentRun, candidate approval and release | A/B | connected partially on 3a9 | PostgreSQL, MinIO, JetStream and mTLS integration tests; a real ECNU AgentRun completed both candidates and teacher approval was exercised on the adopted deployment | fresh BuildKit/Trivy evidence and release publication remain blocked |
| Claude Code Agent runtime | B | connected at 24928e8f | bounded process runtime, leases, cancellation, candidate validation and explicit ECNU Anthropic-compatible endpoint/Secret binding; current AgentRun produced both strict candidates | duplicate/reorder/cancel/deadline connected replay remains open |
| BuildKit/Harbor/Trivy supply chain | B | connected for Container | fixed-command `build-executor`, persistent fence and negative tests; the ec6587 package passed connected `package-validate`; a fresh course build reached BuildKit, Trivy, candidate cleanup and immutable digest publication | full duplicate/reorder/cancel/deadline connected replay and terminal Release Gate remain open |
| Environment lifecycle and owner resolver | B/A | implemented locally | lifecycle, PostgreSQL, JetStream, mTLS and typed executor tests | deployed owner/executor API path is pending |
| Container runtime | B | connected Container slice verified | deterministic plan, persistent fence, restricted Kubernetes SSA/observe/scale/restart/delete backend, finalizer-race handling and versioned cleanup evidence | KubeVirt replay and Gateway protocol replay remain pending; the retained artifact bucket does not provide Object Lock |
| KubeVirt runtime | B | connected on f9305c4a; final identity pending | deterministic VM/CDI plan, independent persistent fence, restricted API/subresource backend and executor-owned SSH host-key probe; `f9305c4a` completed real Gateway access and VM freeze after preserving workspace creation | the final `807a4c59` stop/recover/freeze/delete replay and dual-runtime gate remain blocked |
| AccessGrant and session authorization | A | connected on f9305c4a; negative matrix pending | external OpenSSH passed key authentication, grant authorization, target Service resolution and exact host-key identity; fixed local account, one-time token and session close remained enforced | final-identity denial, expiry, revocation and 60-second termination replay remain pending; Upgrade is explicitly outside Sprint 2 |
| Freeze-only Evaluation Service | B | connected Container freeze at 24928e8f and VM freeze at f4be2776 | Evaluation-owned public API, Access BFF authorization, PostgreSQL command/outbox, PVC/SFTP collection, immutable MinIO publication, restart recovery and cleanup readback | final-identity VM replay remains pending; Runner, Checker, Aggregator and scoring are excluded |
| Web teacher/student journeys | C | connected Container slice | component/SDK tests, live auth setup, teacher approval and student Container freeze evidence readback pass; deterministic Fixture freeze readback is aligned at PR head `bdedb772` | KubeVirt and the full same-identity lifecycle/negative matrix remain pending |
| Sprint 2 Helm deployment | A/D | 24928e8f adopted non-destructively twice | package and both application reconciles exited zero; ten Deployments are ready on digest references; retained infrastructure was not reset | generic infrastructure identity Verify, rollback drill and Release Gate remain pending |
| Sprint 2 data foundation | A/D | verified on adopted target | digest-locked PostgreSQL, NATS JetStream and MinIO StatefulSets; TLS, persistent storage, restricted Pod Security, default-deny NetworkPolicy, strict private bundle; a second reconciliation at source identity `4ced06d` changed nothing | this is retained-foundation evidence only, not application deployment or Sprint 2 E3 |
| Demo replay and Release Gate | A/D | implemented locally | exact connected check set, evidence rehash, clean-HEAD/deployment/catalog/image/Run binding, tamper test and stable missing-input diagnostics | Linux infrastructure Verify, real Keycloak Playwright and same-build passing report remain pending |
| Private Sigstore, Kyverno and Packer | A/D | removed from product and connected cluster | ADR 0006 is Superseded; active contracts, Chart and CI contain no trust-plane gate; the explicitly authorized 2026-07-24 cleanup removed the Private Sigstore Helm release, its namespace/PVC/routes, and Kyverno controllers/webhooks/CRDs/RBAC after a zero-policy dependency check; Packer was absent | deletion is irreversible and is maintenance evidence, not Sprint 2 acceptance |
| Sprint 2 retained-infrastructure adoption | A/D | 1122 verified | application run `sprint2-application-1122ef6e-0022` retained all named infrastructure and reconciled the application image identity without destructive reset | retained infrastructure is not dual-runtime E3 evidence |
| Sprint 2 destructive reset | A/D | deferred maintenance path | cluster-bound confirmation and destructive report remain isolated behind `demo reset` | explicitly excluded from this delivery and must not be used for Sprint 2 deployment |

## Accepted Sprint 2 security exceptions

- The rootless BuildKit namespace may use `Unconfined` seccomp/AppArmor,
  container-scoped SELinux `spc_t`, and `--oci-worker-no-process-sandbox`. It
  must remain non-privileged and may not
  use HostPath or hostNetwork. This exception is limited to the pinned BuildKit
  workload and is not a general Pod Security relaxation.
- Container and KubeVirt executor ClusterRoles retain broad namespace CRUD for
  Sprint 2. Separate ServiceAccounts and application ownership checks are
  compensating controls, not proof of least privilege. Production release is
  blocked until the RBAC is narrowed or enforced by a native admission
  boundary.
- Agent Service has unrestricted outbound network access for the Sprint 2
  course slice. A teacher-approved Container may independently request
  `network.mode=allow_all`; this exception does not apply to KubeVirt,
  BuildKit, Evaluation or other platform workloads. Identity, Secret,
  resource, ingress and approval checks remain fail closed.

## Release decision

Sprint 2 is **blocked**. It becomes verified only when one source identity closes
all of the following without Fixture or old-report substitution:

1. teacher Keycloak login, material upload, Claude Code AgentRun and independent
   candidate approval;
2. BuildKit build, private Harbor publication and digest-bound Trivy gate;
3. Container create/access/stop/recover/freeze/delete;
4. real KubeVirt VM create/SSH/stop/recover/freeze/delete;
5. AccessGrant denial, expiry and revocation;
6. cleanup readback, real Playwright journeys, idempotent Ansible replay and a
   passing machine-readable Release Gate.

Until then, local tests prove only their named local behavior.
