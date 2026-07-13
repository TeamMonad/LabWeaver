# Active Blockers

## Cross-role Day 1 gate

- Frontend build and B/C/D first branches or PRs are not role A deliverables.
- Owner: B, C and D respectively.
- Exit condition: each owner supplies its own reviewed PR and required evidence.

## PostgreSQL persistence implementation

- Issue #17 freezes only the schema ownership and Migration design. No SQLx
  persistence, database roles, Migration files, controlled Migration Job,
  release/domain locks, Outbox publisher, audit projection or readiness
  validation exists. It remains open after this documentation PR.
- Owner: A for the persistence/release boundary; B must complete the required
  high-risk Migration review and approve ADR 0002 with A before it can become
  accepted.
- Exit condition: a separately scoped implementation issue provides current
  PostgreSQL integration evidence for bootstrap/default-privilege enforcement,
  role and connection identity isolation, all startup/readiness diagnostics,
  release/domain advisory locks, immutable manifest/history/report identity,
  forward repair, Outbox atomicity and idempotent audit projection.

## Resolved blockers

- GitHub Project write scope was restored and Issue #20 was closed as completed.
- All 20 governance Issues are present in `LabWeaver Delivery`; Issues #5–#19 were read back with `Workflow Status=Ready` and `Delivery Priority=P0`.
- GitHub exposes built-in status/priority/date fields as Issue-derived fields. Writable Scrum metadata therefore uses `Workflow Status` and `Delivery Priority`, while `Target date` is updated through the Issue field API.
