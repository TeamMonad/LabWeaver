# Implementation Status

This document records current repository and runtime facts. Design documents,
fixtures, generated schemas, health endpoints, and old deployment reports are
not runtime completion evidence.

Current source line: Draft PR #121 on `release/sprint2`; the exact reviewed head
is read from Git/PR metadata rather than duplicated as a stale status value.

## Issue #131 local single-node-kind profile

The `feature/131-single-node-kind` worktree contains an `ex3` deployment profile
with a digest-pinned kind configuration, local inventory, fail-fast Docker/kind
preflight, and local bootstrap branches. Alpine WSL manually ran the base,
identity, Harbor and BuildKit entrypoints against `labweaver-ex3`; the connected
base services, Harbor and rootless BuildKit were Ready on the local cluster, and
the identity resources were created but their current Keycloak rollout still
needs Cilium endpoint convergence. Remote-cluster MetalLB, NFS, Rocky, KubeVirt
and CDI prerequisites are not applied. The application and final verify layers remain blocked on private
operator inputs and a clean package-producing commit. Issue #130 Fixture assets
are intentionally not used.

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

## EX3 Fixture Demo (#130)

The EX3 fallback demonstration is implemented as a separate Fixture Web image
and the pre-existing Host Fixture path. Both use the same deterministic browser
Fixture, visible `FIXTURE MODE` banner, Playwright projects and source identity;
they do not introduce a mock backend, new product API, schema, migration or
Release Gate input. The Docker image is loopback-only and labelled
`org.labweaver.mode=fixture` plus `org.labweaver.ex3=true`.

The documented Playwright replay is a visible, sequential execution of the
existing teacher material/AgentRun, dual Candidate approval, student lifecycle
and grant/revoke, and Container HTTPS-entry tests. It is an interactive
presentation path only; the complete existing Fixture suite remains the
browser-regression evidence source. External-server mode is explicit through
`LABWEAVER_EXTERNAL_WEB_SERVER=true`; an unavailable Docker fixture or occupied
port fails rather than starting a Host preview.

This capability may satisfy only the EX3 `demo_ready` Fixture demonstration
condition after its current-branch Host and Docker evidence is recorded. It
does not change `local_container`, connected verification, KubeVirt status, or
the Sprint 2 Release decision, which remains blocked.

## Sprint 1 and Sprint 2

## Current connected slice (2026-07-23)

The current source identity is `e062d34fc136e5868b5a3484bd670f36abc0a9af` on
`release/sprint2`. Its seven-image package
`pkg-demo-sprint2-e062d34fc136` passed connected validation and was applied by
non-destructive run `sprint2-application-e062d34f-0001`; it retained the existing PostgreSQL,
NATS, MinIO, Harbor, Keycloak, Kubernetes, KubeVirt, Kyverno and Private
Sigstore service bodies. No namespace, schema, stream, bucket, image or CRD was
deleted or rebuilt. The adoption completed its application reconcile. The
subsequent connected replay proved material upload, ECNU AgentRun, independent
candidate generation/approval, BuildKit build, Trivy scan and digest
publication. Harbor then rejected the candidate-tag cleanup with `403
Forbidden`. The robot was granted the bounded `artifact:delete` permission and
the worker credential was reconciled, but the required replay could not be
verified because the Kubernetes API, router SSH and demo HTTPS entry all timed
out. This is a network blocker, not a passing Container result.

| Check | Result | Evidence / limitation |
| --- | --- | --- |
| Container environment reconcile | blocked on final replay | MinIO CA/policy and Trivy DB transport defects were repaired; a fresh connected build reached digest publication. Candidate-tag cleanup then failed closed with Harbor `403`; bounded delete permission was reconciled, but the post-fix replay is unavailable because all current control paths time out. |
| Freeze worker storage binding | verified prerequisite | `labweaver-frozen-submissions` is an additive Object Lock bucket with versioning; the live Evaluation ConfigMap points at it. The existing `labweaver-artifacts` bucket was not changed. |
| Freeze worker network path | verified prerequisite | The retained data NetworkPolicy now permits the labelled environment namespace to reach PostgreSQL/NATS/MinIO; TLS and TCP probes passed. |
| Freeze completion | verified on Container | Same-build Evaluation worker image `harbor.lab.lan/labweaver-system/evaluation-service@sha256:14ff7de5934660aced3efd0cb4c3443bdf7ab2bb4b4bbb8bd3780f17d55bdea3` completed frozen submission `019f8ae9-774d-7583-baa7-f1bc0756aad6`; Object Lock version `9f3fae6d-dbd2-4e84-957a-2a3459cccd03`, `cleanup_verified=true`, and the command reached `completed`. |
| Container access | verified prerequisite | A real student session created an AccessGrant for the fresh Container environment after endpoint discovery; the grant was course- and environment-scoped. Gateway protocol replay remains pending. |
| Container freeze | verified | Fresh environment `019f8b1a-f95f-7551-a5ce-33f4c26466fd` froze submission `019f8b1b-e2e7-7212-9272-a5b89160cc98` from the mounted workspace; the immutable object was materialized by the Evaluation worker and the freeze command completed. |
| Container stop/start | verified | The same fresh environment accepted stop (`stopped`, revision 10), then start (`ready`, revision 12) after the lifecycle fix allowing an immediate `Stopped → Ready` provider observation. |
| Container delete/cleanup | verified | After the cleanup-plan, fence replay, finalizer-race and MinIO-prefix fixes, environment `019f8b1a-f95f-7551-a5ce-33f4c26466fd` reached `deleted` (revision 42) with cleanup artifact version `d7cfd573-6d27-48d9-8ea2-1e6407cd3da0`; the namespace and its runtime resources are absent. The retained `labweaver-artifacts` bucket has versioning but no Object Lock, so this evidence uses conditional versioned immutable storage rather than Governance Lock. |
| KubeVirt | intentionally skipped | The user limited this round to Container runtime verification; no VM completion claim is made. |
| Release Gate / Sprint 2 | blocked | KubeVirt is intentionally skipped this round; the post-Harbor-permission Container replay and machine-readable passing Release Gate do not exist because connected verification is unavailable. |

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
| KubeVirt runtime | B | implemented locally; retained platform recovered | deterministic VM/CDI plan, independent persistent fence, restricted API/subresource backend and SSH host-key probe; KubeVirt is `Deployed`, current API/controller/operator/handler replicas and CDI deployments are Ready after the control-plane recovery | real base-disk/guest-agent/SSH/cleanup replay is pending; readiness alone is not VM evidence |
| AccessGrant and session authorization | A | implemented locally | fixed local OpenSSH account, post-auth alias redemption, one-time token and session contracts; HTTP(S) grants use same-origin `/connect` paths with per-request Access and Environment revalidation and no direct runtime route | built Gateway image, connected HTTP/SSH denial, expiry and revocation replay are pending; Upgrade is explicitly outside Sprint 2 |
| Freeze-only Evaluation Service | B | implemented locally | Evaluation-owned public API, Access BFF authorization, PostgreSQL command/outbox, Environment-issued PVC/299-second read-only VM binding, bounded Kubernetes Job/Secret/ConfigMap/NetworkPolicy reconciliation, immutable MinIO Object Lock, restart recovery and cleanup readback | connected deployment and real PVC/VM replay are pending; Runner, Checker, Aggregator and scoring are excluded |
| Web teacher/student journeys | C | connected partially on 3a9 | component/SDK tests and live auth setup pass; teacher candidate approval is connected, while student Container create/access remains blocked by release evidence freshness | KubeVirt and final same-identity Container replay remain pending; local visual baselines were not rewritten |
| Sprint 2 Helm deployment | A/D | implemented; e062 adopted non-destructively | source `e062d34f` passed connected package validation and non-destructive application adoption run `sprint2-application-e062d34f-0001`; retained infrastructure was not deleted or rebuilt | post-permission Container replay, Gateway protocol replay, KubeVirt replay and Release Gate remain pending |
| Sprint 2 data foundation | A/D | verified on adopted target | digest-locked PostgreSQL, NATS JetStream and MinIO StatefulSets; TLS, persistent storage, restricted Pod Security, default-deny NetworkPolicy, strict private bundle; a second reconciliation at source identity `4ced06d` changed nothing | this is retained-foundation evidence only, not application deployment or Sprint 2 E3 |
| Demo replay and Release Gate | A/D | implemented locally | exact connected check set, evidence rehash, clean-HEAD/deployment/catalog/image/Run binding, tamper test and stable missing-input diagnostics | Linux infrastructure Verify, real Keycloak Playwright and same-build passing report remain pending |
| Private Sigstore, Kyverno and Packer | A/D | removed from active product source | ADR 0006 is Superseded; active contracts, Chart and CI contain no trust-plane gate | retained installations are deliberately untouched and are not Sprint 2 evidence |
| Sprint 2 retained-infrastructure adoption | A/D | 1122 verified | application run `sprint2-application-1122ef6e-0022` retained all named infrastructure and reconciled the application image identity without destructive reset | retained infrastructure is not dual-runtime E3 evidence |
| Issue #131 single-node-kind base | A/D | connected foundation verified locally | Alpine WSL manual Ansible runs created the digest-pinned `labweaver-ex3` kind cluster and reconciled Experimental Gateway API, Cilium, local-path, PostgreSQL, NATS, MinIO, Keycloak, Harbor and rootless BuildKit; the foundation and identity readbacks completed successfully | Application package/configuration inputs and final Container/verify evidence remain blocked; this is not #130 Fixture evidence |
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
