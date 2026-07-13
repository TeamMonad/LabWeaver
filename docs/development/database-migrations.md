# Database Migrations

## Status and scope

This document is the formal implementation contract for GitHub Issue #17. It
defines the required production behavior; it does not claim that the described
directories, roles, Migration Job or runtime validation currently exist.
Current evidence is E0. The runtime implementation must follow
[ADR 0002](../adr/0002-postgresql-schema-and-migration-policy.md).

## Ownership and credentials

Course deployments use one PostgreSQL cluster with the domain schemas listed
in [Data Ownership](../architecture/data-ownership.md). Every domain has three
separate identities:

| Identity | Permitted responsibility | Prohibited responsibility |
| --- | --- | --- |
| schema owner | bootstrap and grant the domain schema | application runtime and cross-domain business writes |
| Migration login | execute approved files for exactly one domain during the Migration Job | service runtime, other schemas and ad-hoc SQL |
| runtime login | perform least-privilege queries and writes in its own domain schema | DDL, Migration history writes, ownership changes and cross-domain writes |

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

`manifest.yaml` is a reviewed, version-controlled deployment input. It fixes
the domain execution order and the expected Migration IDs/checksums for the
release. Domain directories use monotonically increasing IDs. Each domain owns
its schema-local version history and obtains a domain-specific PostgreSQL
advisory lock before comparing or applying files.

Each history record and Job report includes domain, monotonic ID, filename,
SHA-256, apply timestamp, outcome, executor identity and release build
identity. Applied files are immutable: editing, renumbering, removing or
substituting one is a blocking identity mismatch, not an opportunity to
silently repair history.

The sole production execution boundary is the dedicated Migration Job,
orchestrated by Ansible through the typed command:

```sh
cargo xtask migrate --yes
```

Until that command and its controlled Job are implemented, the command must
return `XTASK_NOT_IMPLEMENTED`; it must not report success or perform a hidden
fallback. Direct SQL and service-start migration are not supported production
paths. Environment selection is not implemented in the current `migrate`
command; adding it requires a reviewed CLI contract and must not infer a target
from local configuration.

## Startup, failure and recovery

Before a service becomes Ready, its runtime validation must compare the
expected release identity with the domain's observed schema and Migration
history. Missing schema, unknown schema, an unexpected version, checksum
mismatch, incomplete Migration or unavailable database produces a stable
blocking diagnostic and prevents readiness. Runtime validation never applies,
repairs or skips a Migration.

Production migration uses Expand/Contract:

1. Back up the affected data and validate release compatibility before the Job.
2. Expand with backward-compatible schema and application changes.
3. Observe the declared compatibility window and confirm no old reader,
   writer or event consumer still depends on the retired shape.
4. Run the separately approved destructive Contract Migration.

Any Migration failure stops the deployment and preserves the failure report.
Automatic down migrations are prohibited. If the application release can no
longer run, roll back only to a compatibility-approved application revision;
repair database state with a new, reviewed forward Migration.

## Transactional Outbox and audit projection

A domain transaction writes only its business records, domain idempotency
records and domain-local Outbox row. The Outbox publisher emits the versioned
event after commit. Consumers treat delivery as at-least-once and must reject
or idempotently handle duplicate, stale, unsupported and replayed events.

Control Service's audit projection consumes those events and writes sanitized,
append-only records to `shared_audit`. Projection progress is idempotent. Audit
data is not a source for business state, scoring, authorization or replay; it
must not contain credentials, raw submissions, full sensitive payloads or
unredacted connection details.

## Required implementation evidence

The implementation issue must provide current-build evidence for all of the
following before this contract is upgraded from E0:

- database role rejection for DDL and cross-domain writes;
- unknown/missing/version/checksum schema identity diagnostics and readiness
  denial;
- advisory-lock contention, repeated Job execution and partial-failure
  blocking;
- immutable Migration history and build-identity report verification;
- transaction-plus-Outbox atomicity, publisher retry and consumer replay;
- audit projection idempotency and append-only permission enforcement;
- Expand/Contract compatibility and reviewed forward-repair path.
