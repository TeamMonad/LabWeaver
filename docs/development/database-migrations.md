# Database Migrations

## Status and scope

This document is the formal implementation contract for GitHub Issue #17. It
defines the required production behavior; it does not claim that the described
directories, roles, Migration Job or runtime validation currently exist.
Current evidence is E0. Issue #17 remains open until a separately scoped
implementation provides the required runtime evidence. The implementation must
follow [ADR 0002](../adr/0002-postgresql-schema-and-migration-policy.md).

## Ownership and credentials

Course deployments use one PostgreSQL cluster with deployment schema
`platform_meta` and the domain schemas listed in
[Data Ownership](../architecture/data-ownership.md). A short-lived Ansible DB
provisioner is the only identity that may create schemas, roles and grants. It
creates all identities, revokes its own access after bootstrap, and is never
available to application or connection-pool workloads.

Every domain has three separate identities:

| Identity | Permitted responsibility | Prohibited responsibility |
| --- | --- | --- |
| NOLOGIN schema owner | owns domain objects and owner-specific default grants | direct connection, service runtime and cross-domain writes |
| Migration login | owns the domain Migration history; uses restricted `SET ROLE` only for its own schema DDL | service runtime, other domains, direct ad-hoc production SQL and history substitution |
| runtime login | own-schema DML and read-only schema/history identity validation | DDL, history writes, ownership changes, `SET ROLE` and cross-domain writes |

The provisioner revokes all `PUBLIC` privileges on the `public` schema, every
LabWeaver schema and all existing objects. Before the first Migration, it sets
owner-specific default privileges so new tables, sequences and functions do
not grant `PUBLIC`; it grants only the domain runtime DML and read-only history
access required by the contract. A runtime login cannot read another domain's
history. The release coordinator alone writes `platform_meta`; it is not a
business service identity.

Each pool has a distinct secret and role: one runtime pool per service, one
single-domain Migration pool, one release-coordinator pool and one audit
projection pool. Password, connection URL, pool or role fallback between these
identities is forbidden. The provisioner sets and every checkout verifies an
exact database-level `search_path`: `<domain>, pg_catalog` for domain roles and
`platform_meta, pg_catalog` for the release coordinator. Pools reset that
value before reuse, and application SQL cannot append untrusted schemas.

The `shared_audit` schema is append-only and has no service-owned business
tables. Control Service temporarily owns its projection consumer and uses a
separate restricted projection login. Other services do not write this schema.

Cross-domain foreign keys, cascading actions, triggers and functions are not
permitted. Domain records use stable IDs plus immutable version/hash references
and coordinate through the versioned API/event contracts.

## Layout, identity and execution

The future repository layout is:

```text
migrations/
  manifest.yaml
  control/
  access/
  environment/
  agent/
  evaluation/
  resource/
```

`manifest.yaml` is canonically serialized before calculating its SHA-256. It
is a reviewed, version-controlled deployment input that binds the release ID,
Git commit, immutable build/image digest, Migration-tool version, fixed domain
order, and every Migration ID, filename and SHA-256. The Job verifies the
manifest identity against its own immutable build metadata and checked-out
files before it locks or changes a schema.

Domain directories use monotonically increasing IDs. Each domain owns a
schema-local Migration history and obtains a domain-specific PostgreSQL
advisory lock before comparing or applying files. The Migration login owns and
writes that history; runtime may only read it for identity validation.

Each history record includes domain, monotonic ID, filename, SHA-256, apply
timestamp, outcome, executor identity and release identity. Applied files are
immutable: editing, renumbering, removing or substituting one is a blocking
identity mismatch, not an opportunity to silently repair history.

`platform_meta` holds the release ledger. Before a domain lock, the
release-coordinator obtains a session-level global advisory lock whose two
signed `int4` values are the first 64 bits of
`SHA-256(cluster_uuid || "labweaver:migration-release")`, split in network-byte
order. All releases for one cluster therefore contend on one lock. The ledger
records release ID, manifest SHA-256, Git commit, build/image digest, Job and
attempt identity, timestamps, current domain, state, report SHA-256 and stable
failure diagnostic.

The sole production execution boundary is the dedicated Migration Job,
orchestrated by Ansible through the typed command:

```sh
cargo xtask migrate --yes
```

At the PR's verified current Head, this command exits non-zero and emits
`[XTASK_NOT_IMPLEMENTED] migrate is declared in the design but has no
implementation in this checkout`. The PR body records that exact commit and
verification output. This is a documented blocking diagnostic, not Migration
evidence. Direct SQL and service-start migration are not supported production
paths. Environment selection is not implemented in the current `migrate`
command; adding it requires a reviewed CLI contract and must not infer a target
from local configuration.

## Startup, failure and recovery

Before a service accepts business traffic, runtime validation compares the
expected release identity with the domain's observed schema and Migration
history. `DB_SCHEMA_MISSING`, `DB_SCHEMA_UNKNOWN`, `DB_SCHEMA_AHEAD`,
`DB_SCHEMA_INCOMPLETE` and `DB_SCHEMA_CHECKSUM_MISMATCH` log one structured
error at the owning boundary and terminate the process with non-zero status.
`DB_SCHEMA_BEHIND` and `DB_SCHEMA_UNAVAILABLE` keep the process live so the
orchestrator does not restart-loop during a controlled Migration or transient
database outage; `/health/live` remains successful, `/health/ready` returns
503, and all business traffic is rejected. Runtime validation never applies,
repairs or skips a Migration.

Production migration uses Expand/Contract:

1. Back up the affected data and validate release compatibility before the Job.
2. Expand with backward-compatible schema and application changes.
3. Observe the declared compatibility window and confirm no old reader,
   writer or event consumer still depends on the retired shape.
4. Run the separately approved destructive Contract Migration.

Every initial Migration file is transactional. A failed file commits neither
its domain DDL/DML nor history row. Earlier committed files remain applied; the
release ledger records the failed domain and diagnostic, and no later domain is
attempted. PostgreSQL releases the session lock after a Job crash; the next Job
marks an abandoned `running` attempt failed before proceeding. Only an exactly
matching release/manifest/build identity may retry after applied-history
validation. A different release is blocked until a reviewed forward repair
records the predecessor resolved.

Automatic down migrations are prohibited. If the application release can no
longer run, roll back only to a compatibility-approved application revision;
repair database state with a new, reviewed forward Migration.

## Transactional Outbox and audit projection

A domain transaction writes only its business records, domain idempotency
records and domain-local Outbox row. The Outbox publisher emits the versioned
event after commit. Consumers treat delivery as at-least-once and must reject
or idempotently handle duplicate, stale, unsupported and replayed events.

Control Service's temporary audit projection consumes those events and writes
sanitized, append-only records to `shared_audit`. Projection failure never
blocks, rolls back or changes a business transaction; it only marks the
projection unhealthy, records a watermark/diagnostic and blocks Release Gate
claims requiring audit completeness. Replay/backfill reads retained domain
Outbox events and is idempotent by event identity. Audit data is not a source
for business state, scoring, authorization or replay; it must not contain
credentials, raw submissions, full sensitive payloads or unredacted connection
details.

The successor is a separately approved non-business audit-projection worker.
Control remains owner until a scope-freeze exception, superseding ownership
ADR, A/B review, E2 replay/backfill, dual-write/watermark comparison and
verified cutover are complete.

## Required implementation evidence

The implementation issue must provide current-build evidence for all of the
following before this contract is upgraded from E0:

- provisioner revocation/default privilege enforcement and connection identity
  isolation;
- all declared schema diagnostics, process termination, liveness and readiness
  behavior;
- global and domain advisory-lock contention, crash retry, different-release
  blocking and partial-domain failure recording;
- immutable manifest/history identity and machine-readable build/report chain;
- transaction-plus-Outbox atomicity, publisher retry and consumer replay;
- audit projection idempotency and append-only permission enforcement;
- Expand/Contract compatibility and reviewed forward-repair path.
