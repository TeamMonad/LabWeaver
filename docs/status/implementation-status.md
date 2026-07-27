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
`807a4c598465dfc0977c6b4660ec6be1f5f4eaec`. Package
`pkg-demo-sprint2-807a4c59-807a4c598465` completed with exit status zero and
the non-destructive Ansible application run `deploy-807a4c59` completed with
exit status zero. All ten declared Deployments reported one ready replica and
use Harbor digest references.
The adoption retained PostgreSQL, NATS, MinIO, Harbor, Keycloak, BuildKit,
Kubernetes and KubeVirt service bodies. It did not reset domain data or claim
the historical infrastructure deployment identity as current.

The immediately preceding `f9305c4a` deployment completed a real KubeVirt
access and freeze repair replay. An external OpenSSH client passed public-key
authentication, grant authorization, target Service resolution and exact
target host-key identity validation. The controlled session wrote one
`README.md`, then submission `019fa58b-a66c-7282-bdc2-6cacea5c070c` froze one
file with immutable object SHA-256
`9419467254237418d7c3019fec739161d336ef93cb225dcc2f955b4030f27830`.
Freeze-owned Job, ConfigMap, Secret and NetworkPolicy names were absent after
cleanup. This proves the repaired path on `f9305c4a`, but it is not promoted to
the final `807a4c59` same-identity Release Gate.

On the older `ec6587cb` deployment, one production ECNU AgentRun completed
Environment and Evaluation candidate generation; the teacher approved both
candidates. BuildKit built and pushed the Container image to private Harbor,
Trivy 0.72.0 reported zero Critical findings, and Control published the
immutable digest. A real Container reached `ready`, served its HTTP endpoint
and produced an immutable frozen submission. Real Keycloak teacher/student
Playwright completed with three passed tests and one explicitly skipped VM
test. The workspace file used by this replay was seeded with an administrative
`kubectl exec` because Sprint 2 has no student workspace writer; this is a demo
setup limitation, not a verified student editing path.

Two fresh AgentRuns on `f9305c4a` and `807a4c59` failed closed before producing
candidates. A direct, payload-free Anthropic compatibility probe returned HTTP
429 with the provider diagnostic that the application's monthly credits limit
was exceeded. Claude Code 2.1.x represented this provider failure as an empty
zero-token `success` stream containing a synthetic user event, so no prompt or
Schema change can restore generation while the provider quota remains
exhausted. No prior candidate, Fixture or old release is substituted: the old
Container release was independently rejected with
`LW_ENVIRONMENT_RELEASE_EVIDENCE_EXPIRED`.

| Check | Result | Evidence / limitation |
| --- | --- | --- |
| Agent and approval | verified on connected runtime | AgentRun `019f9161-8e50-7dd0-ac63-073e5f4f1733`; Environment candidate `019f9161-bb3f-7690-b42c-a162a6375c3b`; Evaluation candidate `019f9161-acbe-71c2-b103-bdd85d771d63`; both have teacher approval. |
| Build and publication | verified on connected runtime | Artifact `019f9162-9486-7412-a504-325a8df42254`; immutable digest `sha256:02f173cda04d3567bb7fb348e6b9fa3630a491697031ba5b4faad783f0abf7d2`; Trivy Critical count 0; release `019f916c-b4d8-7623-8bf6-9c893fc191e2`. High findings remain and are not described as zero vulnerabilities. |
| Container create and access | verified for demo | Environment `019f916d-169b-7061-b7b5-c8fce0e7ce7e` reached authoritative `ready` and its HTTP endpoint was healthy. Current same-identity protocol denial/expiry/revocation replay remains open. |
| Container freeze | verified on connected runtime | Submission `019f9171-b23b-7a43-86f4-8f914caf4ec7` contains one file and immutable object digest `1c730c71a98b9ceb985ee19e55498312787c8ed4f211aabac73df534889b097b`; the browser also read back Object Version and SHA evidence. |
| Real browser flow | verified for Container slice | Real Keycloak teacher and student sessions: 3 passed, 1 VM test explicitly skipped; no Fixture result is used as connected evidence. |
| KubeVirt | verified on immediate predecessor; blocked for final identity | Source `f9305c4a` completed real VM access through the OpenSSH Gateway and immutable freeze with cleanup readback. The final `807a4c59` identity has not repeated the complete VM lifecycle and cannot close the terminal gate. |
| Infrastructure Verify | blocked by identity drift | Generic `cargo xtask verify --infra` correctly rejects the retained historical deployment manifest because its commit/inventory/component-lock identity differs from the current private inventory. Application-specific adoption and testflight passed. |
| Release Gate / Sprint 2 | blocked | ECNU monthly credits are exhausted; a fresh final-identity candidate/build/release cannot be produced. Same-identity Container/VM lifecycle, Access/Gateway negative matrix, rollback drill and the complete machine-readable Release Gate remain absent. |

| Capability | Owner | State | Current evidence | Blocker or limitation |
| --- | --- | --- | --- | --- |
| Six service and six PostgreSQL domain boundaries | A | implemented | Cargo workspace, service crates, deterministic catalog and one `0001_sprint2_baseline.sql` per domain; 3a9 non-destructive adoption verified the exact retained ledger across all six domains | Resource has no Sprint 2 production path |
| Contracts, OpenAPI and Web SDK | A/C | implemented locally | one ADR 0011 v1 Rust source with generated JSON Schema, Public/Internal OpenAPI and Web SDK; publication accepts only candidate/approval/runtime identity, while Control resolves Container build evidence or the deployment-locked VM base identity; contract/render and Web drift gates pass | connected clients and deployed schema identity remain pending |
| Keycloak/OIDC and course-scoped authorization | A | connected on ec6587 | Access BFF, bearer/mTLS checks, PostgreSQL tests and retained Keycloak reconciliation; real teacher/student sessions and teacher candidate approval pass through the adopted BFF session and CSRF transport | Access/Gateway protocol denial, expiry and revocation replay remains open |
| Control material, AgentRun, candidate approval and release | A/B | connected partially on 3a9 | PostgreSQL, MinIO, JetStream and mTLS integration tests; a real ECNU AgentRun completed both candidates and teacher approval was exercised on the adopted deployment | fresh BuildKit/Trivy evidence and release publication remain blocked |
| Claude Code Agent runtime | B | implemented and deployed at 807a4c59; provider blocked | bounded process runtime, leases, cancellation, candidate validation and explicit ECNU Anthropic-compatible endpoint/Secret binding; local Runtime tests and CI pass; a direct provider probe confirms HTTP 429 monthly credits exhaustion | quota restoration or a replacement valid `ECNU_API_KEY` is required before a fresh dual-candidate replay; retained runs are diagnostic evidence only |
| BuildKit/Harbor/Trivy supply chain | B | connected for Container | fixed-command `build-executor`, persistent fence and negative tests; the ec6587 package passed connected `package-validate`; a fresh course build reached BuildKit, Trivy, candidate cleanup and immutable digest publication | full duplicate/reorder/cancel/deadline connected replay and terminal Release Gate remain open |
| Environment lifecycle and owner resolver | B/A | implemented locally | lifecycle, PostgreSQL, JetStream, mTLS and typed executor tests | deployed owner/executor API path is pending |
| Container runtime | B | connected Container slice verified | deterministic plan, persistent fence, restricted Kubernetes SSA/observe/scale/restart/delete backend, finalizer-race handling and versioned cleanup evidence | KubeVirt replay and Gateway protocol replay remain pending; the retained artifact bucket does not provide Object Lock |
| KubeVirt runtime | B | connected on f9305c4a; final identity pending | deterministic VM/CDI plan, independent persistent fence, restricted API/subresource backend and executor-owned SSH host-key probe; `f9305c4a` completed real Gateway access and VM freeze after preserving workspace creation | the final `807a4c59` stop/recover/freeze/delete replay and dual-runtime gate remain blocked |
| AccessGrant and session authorization | A | connected on f9305c4a; negative matrix pending | external OpenSSH passed key authentication, grant authorization, target Service resolution and exact host-key identity; fixed local account, one-time token and session close remained enforced | final-identity denial, expiry, revocation and 60-second termination replay remain pending; Upgrade is explicitly outside Sprint 2 |
| Freeze-only Evaluation Service | B | connected VM freeze on f9305c4a | Evaluation-owned public API, Access BFF authorization, PostgreSQL command/outbox, Environment-issued VM binding, bounded SSH collection, immutable MinIO object publication, restart recovery and cleanup readback | final-identity Container and VM replay remain pending; Runner, Checker, Aggregator and scoring are excluded |
| Web teacher/student journeys | C | connected Container slice | component/SDK tests, live auth setup, teacher approval and student Container freeze evidence readback pass; deterministic Fixture freeze readback is aligned at PR head `bdedb772` | KubeVirt and the full same-identity lifecycle/negative matrix remain pending |
| Sprint 2 Helm deployment | A/D | 807a4c59 adopted non-destructively | package and application reconciliation exited zero; ten Deployments are ready on digest references; retained infrastructure was not reset | generic infrastructure identity Verify, second idempotent replay, rollback drill and Release Gate remain pending |
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
