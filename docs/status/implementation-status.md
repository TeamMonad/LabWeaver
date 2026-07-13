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
| Identity, Tailnet, DirectAccessGrant and Guacamole trust boundary | A | documented, pending B/D review | ACCESS-01a formal architecture documentation and ADR 0001 | E0 | no OIDC, Headscale Grants, Router firewall, Guacamole extension, grant store, containment or VM-stop implementation/evidence |
| NATS Subject v1 and delivery contract | A | documented, pending A/B review | ADR 0003 and NATS event catalog | E0 | no NATS client, JetStream Stream/Consumer, Outbox publisher, quarantine path or integration evidence |
| EvaluationSpec v1alpha1 contract | B | implemented in current worktree, pending review | generated schemas, OJ/Linux fixtures and `evaluation-domain` contract tests | E1 | no Runner execution, persistence, messaging or production approval path |
| Agent state and Tool contract | B | partially implemented, blocked | `agent-core` state, capability binding, timeout/cancel/no-retry, idempotency identity propagation, output validation, diagnostic ownership, audit and negative tests | Partial E1 | role A must freeze Tool permissions and approval evidence; durable idempotency reservation/replay, service integration and AG-01b Fixture Backend remain unimplemented |
| Frontend, Agent runtime and Playwright work | C/B/D | planned | assigned Sprint Issues | E0 | explicitly outside role A implementation scope |
| Real KubeVirt path | B/D | blocked pending preflight | Issue #15 | E0 | no E3 evidence exists |

## Kubernetes infrastructure automation

State: implemented, pending review and real Ansible replay.

The Ansible playbooks encode the currently validated Rocky Kubernetes baseline:
Kubernetes/CRI-O, Cilium, MetalLB, Local Path, NFS CSI, cert-manager, KubeVirt,
CDI, Kyverno, internal Gateway, and etcd backup. The prior manual environment
provided E3 evidence; this branch does not claim E3 for the playbooks until a
fresh deploy and second idempotency run complete.

Linux CI now provides lint, syntax, fictional encrypted-Vault, preflight-chain,
and storage-safety fixture evidence. This is not E3 evidence. Blockers remain:
private inventory, encrypted Vault, a Linux Ansible controller, and a real
replay environment for first deploy, idempotency, storage, VM, Gateway, Cilium,
and etcd acceptance.
