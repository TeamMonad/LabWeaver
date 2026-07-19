# Implementation Status

This document records current repository and runtime facts. Design documents,
fixtures, generated schemas, health endpoints, and old deployment reports are
not runtime completion evidence.

Current source line: Draft PR #121 on `release/sprint2`; the exact reviewed head
is read from Git/PR metadata rather than duplicated as a stale status value.

## Sprint 1 and Sprint 2

| Capability | Owner | State | Current evidence | Blocker or limitation |
| --- | --- | --- | --- | --- |
| Six service and six PostgreSQL domain boundaries | A | implemented | Cargo workspace, service crates, deterministic catalog and one `0001_sprint2_baseline.sql` per domain; non-destructive adoption applies only to an empty domain or verifies the exact existing ledger | connected application adoption is pending; Resource has no Sprint 2 production path |
| Contracts, OpenAPI and Web SDK | A/C | implemented locally | one ADR 0011 v1 Rust source with generated JSON Schema, Public/Internal OpenAPI and Web SDK; drift checks pass | connected clients and deployed schema identity remain pending |
| Keycloak/OIDC and course-scoped authorization | A | implemented locally | Access BFF, bearer/mTLS checks, PostgreSQL tests | same-build deployed Keycloak/browser/Gateway verification is pending |
| Control material, AgentRun, candidate approval and release | A/B | implemented locally | PostgreSQL, MinIO, JetStream and mTLS integration tests | deployment configuration and connected executors are pending |
| Claude Code Agent runtime | B | implemented locally | bounded process runtime, leases, cancellation and candidate validation tests | private application bundle lacks the Claude Code runtime credential; dual-candidate live replay is pending |
| BuildKit/Harbor/Trivy supply chain | B | partially verified on adopted target | fixed-command `build-executor`, persistent fence, negative tests; source `8db85997` completed real BuildKit/Harbor/Trivy packaging | current PR head requires a new package after edge-router command execution is restored |
| Environment lifecycle and owner resolver | B/A | implemented locally | lifecycle, PostgreSQL, JetStream, mTLS and typed executor tests | deployed owner/executor API path is pending |
| Container runtime | B | implemented locally | deterministic plan, persistent fence and restricted Kubernetes SSA/observe/scale/restart/delete backend | connected Kubernetes apply, object-lock cleanup and access replay are pending |
| KubeVirt runtime | B | implemented locally | deterministic VM/CDI plan, independent persistent fence, restricted API/subresource backend and SSH host-key probe | real KubeVirt/CDI/guest-agent/SSH/cleanup replay is pending |
| AccessGrant and session authorization | A | implemented locally | fixed local OpenSSH account, post-auth alias redemption, one-time token and session contracts; HTTP(S) grants use same-origin `/connect` paths with per-request Access and Environment revalidation and no direct runtime route | built Gateway image, connected HTTP/SSH denial, expiry and revocation replay are pending; Upgrade is explicitly outside Sprint 2 |
| Freeze-only Evaluation Service | B | implemented locally | Evaluation-owned public API, Access BFF authorization, PostgreSQL command/outbox, Environment-issued PVC/299-second read-only VM binding, bounded Kubernetes Job/Secret/ConfigMap/NetworkPolicy reconciliation, immutable MinIO Object Lock, restart recovery and cleanup readback | connected deployment and real PVC/VM replay are pending; Runner, Checker, Aggregator and scoring are excluded |
| Web teacher/student journeys | C | implemented against SDK and fixture modes | component, SDK and fixture Playwright tests | real Keycloak and same-build backend Playwright remain pending |
| Sprint 2 Helm deployment | A/D | implemented locally | ten-workload profile, seven-image package contract, per-process config/Secret mounts, health probes, independent Container/KubeVirt/Evaluation RBAC, non-destructive application adoption and fail-closed API CIDR test | connected render/rollout, RBAC denial checks and Gateway startup remain pending |
| Sprint 2 data foundation | A/D | verified on adopted target | digest-locked PostgreSQL, NATS JetStream and MinIO StatefulSets; TLS, persistent storage, restricted Pod Security, default-deny NetworkPolicy, strict private bundle; a second reconciliation at source identity `4ced06d` changed nothing | this is retained-foundation evidence only, not application deployment or Sprint 2 E3 |
| Demo replay and Release Gate | A/D | implemented locally | exact connected check set, evidence rehash, clean-HEAD/deployment/catalog/image/Run binding, tamper test and stable missing-input diagnostics | Linux infrastructure Verify, real Keycloak Playwright and same-build passing report remain pending |
| Private Sigstore, Kyverno and Packer | A/D | removed from active product source | ADR 0006 is Superseded; active contracts, Chart and CI contain no trust-plane gate | retained installations are deliberately untouched and are not Sprint 2 evidence |
| Sprint 2 retained-infrastructure adoption | A/D | implemented locally | `sprint2-application` validates the seven-image package, adopts empty or exact-ledger schemas, creates only missing streams/bucket/realm, reconciles reviewed configuration and deploys twice without delete operations | edge-router SSH command channels time out and the private Agent credential is missing; connected execution remains pending |
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
