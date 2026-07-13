# Implementation Status

Status is proven only by the identified commit/worktree and current evidence. `docs/draft/` is not completion evidence.

| Capability | Owner | State | Evidence | Level | Limitation / blocker |
| --- | --- | --- | --- | --- | --- |
| Rust Workspace and foundation crates | A | implemented | `cargo xtask check` passed after PR #21 merge | E1 | no business domain implementation |
| Six Axum service shells | A | implemented | `cargo xtask check` passed; Control live/ready smoke passed | E1 | health-only; no persistence, messaging or providers |
| GitHub Milestones, Labels and Sprint Issues | A | configured | GitHub API read-back | E0 | remote metadata only |
| GitHub Project fields and Ready assignments | A | configured | API read-back: 15 P0 items have `Workflow Status=Ready` | E0 | Issue #20 resolved; remote metadata does not prove product behavior |
| Branch protection | A | configured | GitHub API read-back for `main` and `develop` | E0 | required `rust-gate` starts with API-01a PR |
| C4, service boundaries and data ownership | A | documented, pending review | current documentation PR | E0 | design evidence only |
| PostgreSQL schema ownership and Migration policy | A | proposed, pending A/B approval and merge | ADR 0002, data ownership and Migration contract | E0 | Issue #17 remains open: no SQLx, database roles, Migration files/Job, locks, Outbox, audit projection or readiness validation |
| NATS Subject v1 and delivery contract | A | documented, pending A/B review | ADR 0003 and NATS event catalog | E0 | no NATS client, JetStream Stream/Consumer, Outbox publisher, quarantine path or integration evidence |
| EvaluationSpec v1alpha1 contract | B | implemented in current worktree, pending review | generated schemas, OJ/Linux fixtures and `evaluation-domain` contract tests | E1 | no Runner execution, persistence, messaging or production approval path |
| Frontend, Agent runtime and Playwright work | C/B/D | planned | assigned Sprint Issues | E0 | explicitly outside role A implementation scope |
| Real KubeVirt path | B/D | blocked pending preflight | Issue #15 | E0 | no E3 evidence exists |
