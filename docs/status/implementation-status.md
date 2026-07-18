# Implementation Status

This document records current repository and runtime facts. Design documents,
fixtures, generated schemas, health endpoints, and old deployment reports are
not runtime completion evidence.

Current source baseline: `release/sprint2` created from
`c8c87f217390ce9b58f2f808118d960fc150d935`.

## Sprint 1 and Sprint 2

| Capability | Owner | State | Current evidence | Blocker or limitation |
| --- | --- | --- | --- | --- |
| Six service and six PostgreSQL domain boundaries | A | implemented | Cargo workspace, service crates, migration catalog | Evaluation and Resource have no Sprint 2 production path and are disabled by the target deployment |
| Contracts, OpenAPI and Web SDK | A/C | implemented, simplifying | Rust contract tests and generated projections | v1/v2 event duplication and removed trust fields must be collapsed to the ADR 0011 v1 contract |
| Keycloak/OIDC and course-scoped authorization | A | implemented locally | Access BFF, bearer/mTLS checks, PostgreSQL tests | same-build deployed Keycloak/browser/Gateway verification is pending |
| Control material, AgentRun, candidate approval and release | A/B | implemented locally | PostgreSQL, MinIO, JetStream and mTLS integration tests | deployment configuration and connected executors are pending |
| Claude Code Agent runtime | B | implemented locally | bounded process runtime, leases, cancellation and candidate validation tests | deployment Secret/config injection and dual-candidate live replay are pending |
| BuildKit/Harbor/Trivy supply chain | B | in progress | durable pipeline and executor protocol tests | real `build-executor` side effects do not yet exist; Sigstore stages are being removed |
| Environment lifecycle and owner resolver | B/A | implemented locally | lifecycle, PostgreSQL, JetStream and mTLS tests | real Container/KubeVirt executor and deployed API path are pending |
| Container runtime | B | in progress | deterministic resource plan and fence tests | real Kubernetes apply/observe/cleanup worker is pending |
| KubeVirt runtime | B | in progress | deterministic VM/CDI plan, persistence and fence tests | real KubeVirt/CDI/SSH/cleanup worker replay is pending |
| AccessGrant and session authorization | A | implemented locally | Access contract, PostgreSQL and mTLS tests | OpenSSH Gateway image/helper and connected revocation replay are pending |
| Dual-runtime submission freeze | B | implemented locally | bounded PVC/SFTP collection and immutable MinIO tests | deployment certificate issuer and real PVC/VM replay are pending |
| Web teacher/student journeys | C | implemented against SDK and fixture modes | component, SDK and fixture Playwright tests | real Keycloak and same-build backend Playwright remain pending |
| Sprint 2 Helm deployment | A/D | blocked | generic chart exists | current chart does not match per-process config, ports, identity or worker topology |
| Demo replay and Release Gate | A/D | blocked | schemas and partial validators exist | `demo replay` and `release-gate` still return `XTASK_NOT_IMPLEMENTED` |
| Private Sigstore, Kyverno and Packer | A/D | removing | historical implementation and deployment evidence | ADR 0011 removes these from source and the adopted cluster |

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
