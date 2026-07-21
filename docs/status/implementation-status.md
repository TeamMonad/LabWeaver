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

| Capability | Owner | State | Current evidence | Blocker or limitation |
| --- | --- | --- | --- | --- |
| Six service and six PostgreSQL domain boundaries | A | implemented | Cargo workspace, service crates, deterministic catalog and one `0001_sprint2_baseline.sql` per domain; non-destructive adoption applies only to an empty domain or verifies the exact existing ledger; an earlier Draft head completed connected adoption | current-head adoption is pending; Resource has no Sprint 2 production path |
| Contracts, OpenAPI and Web SDK | A/C | implemented locally | one ADR 0011 v1 Rust source with generated JSON Schema, Public/Internal OpenAPI and Web SDK; publication accepts only candidate/approval/runtime identity, while Control resolves Container build evidence or the deployment-locked VM base identity; contract/render and Web drift gates pass | connected clients and deployed schema identity remain pending |
| Keycloak/OIDC and course-scoped authorization | A | implemented locally; previous head deployed | Access BFF, bearer/mTLS checks, PostgreSQL tests and retained Keycloak reconciliation; live probing of deployed source `da498a26` exposed that Web still required browser-side OIDC build variables and its generated SDK transport forced bearer mode, so the current source now derives identity from the BFF session and uses BFF cookies plus CSRF for SDK mutations | the BFF Web repair must be packaged, deployed and exercised with real teacher/student sessions |
| Control material, AgentRun, candidate approval and release | A/B | implemented locally; older head deployed | PostgreSQL, MinIO, JetStream and mTLS integration tests; Control-owned candidate views reload approval history and fail closed on incomplete build evidence instead of relying on Web-only fields; the prior deployed head reached the material upload path | current-head deployment and complete dual-candidate replay are pending |
| Claude Code Agent runtime | B | implemented; previous head deployed | bounded process runtime, leases, cancellation, candidate validation and explicit ECNU Anthropic-compatible endpoint/Secret binding; retained data proves two real ECNU runs completed both tracks, while their completion projection exhausted redelivery before producing Control candidates | the current BFF repair must be deployed before a fresh same-head dual-candidate replay; retained runs are diagnostic evidence only |
| BuildKit/Harbor/Trivy supply chain | B | implemented; previous head packaged | fixed-command `build-executor`, persistent fence and negative tests; source `da498a2643a83e32b3ab6cab3465771a019d1882` produced seven digest-bound Harbor images and passed connected `package-validate` against the locked Trivy database | the BFF repair changes the source identity and therefore requires a new package; a real course build/publication remains pending |
| Environment lifecycle and owner resolver | B/A | implemented locally | lifecycle, PostgreSQL, JetStream, mTLS and typed executor tests | deployed owner/executor API path is pending |
| Container runtime | B | implemented locally | deterministic plan, persistent fence and restricted Kubernetes SSA/observe/scale/restart/delete backend | connected Kubernetes apply, object-lock cleanup and access replay are pending |
| KubeVirt runtime | B | implemented locally; retained platform recovered | deterministic VM/CDI plan, independent persistent fence, restricted API/subresource backend and SSH host-key probe; KubeVirt is `Deployed`, current API/controller/operator/handler replicas and CDI deployments are Ready after the control-plane recovery | real base-disk/guest-agent/SSH/cleanup replay is pending; readiness alone is not VM evidence |
| AccessGrant and session authorization | A | implemented locally | fixed local OpenSSH account, post-auth alias redemption, one-time token and session contracts; HTTP(S) grants use same-origin `/connect` paths with per-request Access and Environment revalidation and no direct runtime route | built Gateway image, connected HTTP/SSH denial, expiry and revocation replay are pending; Upgrade is explicitly outside Sprint 2 |
| Freeze-only Evaluation Service | B | implemented locally | Evaluation-owned public API, Access BFF authorization, PostgreSQL command/outbox, Environment-issued PVC/299-second read-only VM binding, bounded Kubernetes Job/Secret/ConfigMap/NetworkPolicy reconciliation, immutable MinIO Object Lock, restart recovery and cleanup readback | connected deployment and real PVC/VM replay are pending; Runner, Checker, Aggregator and scoring are excluded |
| Web teacher/student journeys | C | live path implemented; fixture verified | component/SDK tests and the Linux teacher/student Fixture matrix pass at the Draft PR head; live setup performs real Keycloak login from password files and live specs read approved Agent candidates plus frozen Container/VM evidence | current-head connected Playwright remains pending; local visual baselines were not rewritten for workstation-specific pixel drift |
| Sprint 2 Helm deployment | A/D | implemented; previous head deployed | source `da498a2643a83e32b3ab6cab3465771a019d1882` passed connected package validation and two non-destructive reconciliations with identical package, migration and configuration hashes; live readback showed all ten workloads Ready, seven Harbor digest images, OpenSSH on the reviewed shared VIP and no TCPRoute | the BFF repair changes the source identity and must be repackaged and reconciled; real runtime verification remains pending |
| Sprint 2 data foundation | A/D | verified on adopted target | digest-locked PostgreSQL, NATS JetStream and MinIO StatefulSets; TLS, persistent storage, restricted Pod Security, default-deny NetworkPolicy, strict private bundle; a second reconciliation at source identity `4ced06d` changed nothing | this is retained-foundation evidence only, not application deployment or Sprint 2 E3 |
| Demo replay and Release Gate | A/D | implemented locally | exact connected check set, evidence rehash, clean-HEAD/deployment/catalog/image/Run binding, tamper test and stable missing-input diagnostics | Linux infrastructure Verify, real Keycloak Playwright and same-build passing report remain pending |
| Private Sigstore, Kyverno and Packer | A/D | removed from active product source | ADR 0006 is Superseded; active contracts, Chart and CI contain no trust-plane gate | retained installations are deliberately untouched and are not Sprint 2 evidence |
| Sprint 2 retained-infrastructure adoption | A/D | previous head verified | `sprint2-buildkit` adopted retained BuildKit and exact mTLS Buildx; application runs `sprint2-application-da498a26-a2` and `sprint2-application-da498a26-b` retained all named infrastructure, reported `destructive_reset=false`, and reconciled the same ten-workload identity twice | the current source must repeat these checks after the BFF repair; retained infrastructure is not dual-runtime E3 evidence |
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
