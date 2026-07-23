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

## Current connected slice (2026-07-24)

The deployed source identity remains `e062d34fc136e5868b5a3484bd670f36abc0a9af` on
`release/sprint2`. Its seven-image package
`pkg-demo-sprint2-e062d34fc136` passed connected validation and was applied by
non-destructive run `sprint2-application-e062d34f-0001`; that adoption retained
the existing PostgreSQL, NATS, MinIO, Harbor, Keycloak, Kubernetes, KubeVirt,
Kyverno and Private Sigstore service bodies and deleted no infrastructure. On
2026-07-24, a separate explicitly authorized maintenance action inventoried
dependencies, found no Kyverno policies or active application references, and
removed the deprecated Private Sigstore and Kyverno installations. Packer was
absent. PostgreSQL, NATS, MinIO, Harbor, Keycloak, BuildKit, Kubernetes,
KubeVirt and the two retained diagnostic environments were not changed. The
adoption completed its application reconcile. The
subsequent connected replay proved material upload, ECNU AgentRun, independent
candidate generation/approval, BuildKit build, Trivy scan, digest publication
and candidate-tag cleanup after adding the exact Harbor `tag:delete`
permission. The same published digest then completed Container creation,
course-scoped AccessGrant creation, immutable workspace freeze, stop/start and
application-owned deletion. The live application reconcile also restored the
Access NATS reply inbox, added the adopted API endpoint port, and aligned the
freeze Worker with its PostgreSQL CA and Object-Lock bucket. These are
connected runtime facts for the deployed package plus named live configuration
reconciles; the pending source commit is not yet a same-identity Release Gate.

The live application is currently a deliberately recorded mixed-identity
diagnostic deployment: Access Service uses digest
`sha256:fd51b8e88f3045ab3eb841ea5a7877aed761c4ea831a6fd532fb8cb5fc85d250`
and Environment Service plus both runtime executors use digest
`sha256:599bf1a14109a3e10707c1ba01fea3d7fdb7f516324b61afc31b59a5db63bd7f`;
the remaining application workloads retain the `e062d34f` package images.
These hotfixes passed a Critical-only Trivy gate and rollout readback, but they
are not a same-source package and cannot upgrade the connected Container
evidence to a Release Gate pass. At the Owner's direction, the immediate
handoff prioritizes the Container slice and defers VM replay, full Demo replay
and the complete machine-readable Release Gate. This acceleration does not
change the final Sprint 2 gate below.

| Check | Result | Evidence / limitation |
| --- | --- | --- |
| Container environment reconcile | verified on connected runtime | AgentRun `019f8de6-9316-7ef0-a84f-d3b4284132f6` published release `019f8de7-417d-7902-a56c-94fc854f7ba6` at immutable digest `sha256:81299f24573365f8f1349902a76f2411b0eef4bf14ac2fa793641d5a359cce84`; environment `019f8df5-2010-7300-b5cf-3a926ba12d57` reached `ready`. |
| Freeze worker storage binding | verified prerequisite | `labweaver-frozen-submissions` is an additive Object Lock bucket with versioning; the live Evaluation ConfigMap points at it. The existing `labweaver-artifacts` bucket was not changed. |
| Freeze worker network path | verified prerequisite | The retained data NetworkPolicy now permits the labelled environment namespace to reach PostgreSQL/NATS/MinIO; TLS and TCP probes passed. |
| Freeze completion | verified on Container | Frozen submission `019f8e0e-e391-7fe3-a88e-0ed8f7ec452f` bound the environment/release/build identities, one workspace file, manifest hash `e9f2fb2ff7c0a0bd339f35e3b48fdd221bf010b1a461b2d175d2e3bf84feae15`, immutable object hash `9d09be32601d84b2aa5b4cf64dc9ffaeb22800aedc4211b1db3c646825587678` and Object Lock version `d2b2119c-5de2-43f6-a915-9fd592c5c485`. |
| Container access | verified prerequisite | A real student session created an AccessGrant for the fresh Container environment after endpoint discovery; the grant was course- and environment-scoped. Gateway protocol replay remains pending. |
| Container freeze | verified | Fresh environment `019f8b1a-f95f-7551-a5ce-33f4c26466fd` froze submission `019f8b1b-e2e7-7212-9272-a5b89160cc98` from the mounted workspace; the immutable object was materialized by the Evaluation worker and the freeze command completed. |
| Container stop/start | verified | Environment `019f8df5-2010-7300-b5cf-3a926ba12d57` reached `stopped` at revision 11 and returned to `ready` at revision 13 through the public student session. |
| Container delete/cleanup | verified | The same environment accepted application-owned deletion and namespace `lw-env-019f8df5-2010-7300-b5cf-3a926ba12d57` became absent. |
| KubeVirt | connected lifecycle repair in progress | A real VM reached Environment authority `ready` with a healthy SSH endpoint, then completed an API-driven stop. The replay exposed and fixed missing executor-to-guest NetworkPolicy rules, premature readiness observation, duplicate KubeVirt readiness authority, and an invalid Namespace finalizer path. A clean create/start/freeze/delete replay at the final source identity is still required before completion is claimed. |
| Release Gate / Sprint 2 | blocked | Gateway protocol negatives, idempotent deployment replay, the final clean KubeVirt lifecycle and a same-source machine-readable passing Release Gate remain absent. |

| Capability | Owner | State | Current evidence | Blocker or limitation |
| --- | --- | --- | --- | --- |
| Six service and six PostgreSQL domain boundaries | A | implemented | Cargo workspace, service crates, deterministic catalog and one `0001_sprint2_baseline.sql` per domain; 3a9 non-destructive adoption verified the exact retained ledger across all six domains | Resource has no Sprint 2 production path |
| Contracts, OpenAPI and Web SDK | A/C | implemented locally | one ADR 0011 v1 Rust source with generated JSON Schema, Public/Internal OpenAPI and Web SDK; publication accepts only candidate/approval/runtime identity, while Control resolves Container build evidence or the deployment-locked VM base identity; contract/render and Web drift gates pass | connected clients and deployed schema identity remain pending |
| Keycloak/OIDC and course-scoped authorization | A | connected on 3a9 | Access BFF, bearer/mTLS checks, PostgreSQL tests and retained Keycloak reconciliation; real teacher/student landing and teacher candidate-approval browser flows now pass through the adopted BFF session and CSRF transport | current Container publication remains blocked by stale build evidence |
| Control material, AgentRun, candidate approval and release | A/B | connected partially on 3a9 | PostgreSQL, MinIO, JetStream and mTLS integration tests; a real ECNU AgentRun completed both candidates and teacher approval was exercised on the adopted deployment | fresh BuildKit/Trivy evidence and release publication remain blocked |
| Claude Code Agent runtime | B | implemented; previous head deployed | bounded process runtime, leases, cancellation, candidate validation and explicit ECNU Anthropic-compatible endpoint/Secret binding; retained data proves two real ECNU runs completed both tracks, while their completion projection exhausted redelivery before producing Control candidates | the current BFF repair must be deployed before a fresh same-head dual-candidate replay; retained runs are diagnostic evidence only |
| BuildKit/Harbor/Trivy supply chain | B | package verified; cleanup replay blocked | fixed-command `build-executor`, persistent fence and negative tests; the e062 package passed connected `package-validate`; a fresh course build reached BuildKit, Trivy and digest publication | Harbor candidate-tag deletion returned `403`; the bounded robot permission and credential were reconciled, but the post-fix replay is blocked by loss of all connected control paths |
| Environment lifecycle and owner resolver | B/A | implemented locally | lifecycle, PostgreSQL, JetStream, mTLS and typed executor tests | deployed owner/executor API path is pending |
| Container runtime | B | connected Container slice verified | deterministic plan, persistent fence, restricted Kubernetes SSA/observe/scale/restart/delete backend, finalizer-race handling and versioned cleanup evidence | KubeVirt replay and Gateway protocol replay remain pending; the retained artifact bucket does not provide Object Lock |
| KubeVirt runtime | B | connected repair verified partially | deterministic VM/CDI plan, independent persistent fence, restricted API/subresource backend and executor-owned SSH host-key probe; a real VM reached authoritative `ready`, exposed a healthy SSH endpoint and stopped through the public API. The final image is locally Trivy-scanned with zero Critical findings. | clean start/freeze/delete replay at the final source identity remains pending; the earlier lifecycle is diagnostic evidence rather than a passing dual-runtime gate |
| AccessGrant and session authorization | A | implemented locally | fixed local OpenSSH account, post-auth alias redemption, one-time token and session contracts; HTTP(S) grants use same-origin `/connect` paths with per-request Access and Environment revalidation and no direct runtime route | built Gateway image, connected HTTP/SSH denial, expiry and revocation replay are pending; Upgrade is explicitly outside Sprint 2 |
| Freeze-only Evaluation Service | B | implemented locally | Evaluation-owned public API, Access BFF authorization, PostgreSQL command/outbox, Environment-issued PVC/299-second read-only VM binding, bounded Kubernetes Job/Secret/ConfigMap/NetworkPolicy reconciliation, immutable MinIO Object Lock, restart recovery and cleanup readback | connected deployment and real PVC/VM replay are pending; Runner, Checker, Aggregator and scoring are excluded |
| Web teacher/student journeys | C | connected partially on 3a9 | component/SDK tests and live auth setup pass; teacher candidate approval is connected, while student Container create/access remains blocked by release evidence freshness | KubeVirt and final same-identity Container replay remain pending; local visual baselines were not rewritten |
| Sprint 2 Helm deployment | A/D | implemented; e062 adopted non-destructively | source `e062d34f` passed connected package validation and non-destructive application adoption run `sprint2-application-e062d34f-0001`; retained infrastructure was not deleted or rebuilt | post-permission Container replay, Gateway protocol replay, KubeVirt replay and Release Gate remain pending |
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
