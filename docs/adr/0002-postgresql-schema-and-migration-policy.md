# ADR 0002: PostgreSQL Schema Ownership and Migration Policy

Status: proposed. It becomes `accepted` only after Architecture Owner A and
Agent/Backend Owner B both approve this ADR, its privilege matrix and failure
semantics in the associated PR, and that PR is merged into `develop`. Runtime
implementation or E2 evidence is not a prerequisite for accepting this design;
neither a comment nor a single approval changes this status.

## Context

LabWeaver uses PostgreSQL as the authority for durable business state. The
service-boundary decision forbids one service from mutating another service's
data, but a shared course deployment still needs an enforceable schema, role,
Migration, Outbox and audit-projection model. Without it, a shared database
login, service-start migration, cross-schema foreign key, or shared Outbox can
silently couple deployments and make partial failures unrecoverable.

This ADR resolves GitHub Issue #17 at the design level only. It does not add a
SQLx crate, SQL files, database roles, a Migration Job, an Outbox publisher, or
runtime schema validation.

## Decision

- A course deployment uses one PostgreSQL cluster with deployment metadata
  schema `platform_meta` and the domain schemas `control`, `access`,
  `environment`, `agent`, `evaluation` and `resource`. `platform_meta` is not
  a business domain and is inaccessible to every runtime service login.
- A short-lived Ansible database provisioner is the only identity permitted to
  create those schemas and role grants. It creates one NOLOGIN schema-owner,
  one Migration login and one runtime login per domain, then creates the
  restricted release-coordinator and audit-projection logins. Its high
  privilege credential is removed from the Job and never mounted into service
  workloads. Long-lived bootstrap/superuser credentials are prohibited.
- The provisioner revokes all `PUBLIC` privileges on `public`, every
  LabWeaver schema, existing objects and owner-specific default privileges.
  It sets owner-specific default privileges before any Migration can create an
  object. Runtime logins receive only own-schema DML and `SELECT` on their
  domain Migration history; they never receive `CREATE`, `ALTER`, `DROP`,
  ownership, role-membership or cross-domain write privileges.
- A domain Migration login owns its domain's Migration-history table. It may
  use a restricted `SET ROLE` grant to the corresponding NOLOGIN schema-owner
  only while creating or altering domain objects, then resets the role before
  writing history. It cannot assume another domain owner or access another
  domain history. Every application, Migration, coordinator and projection
  connection pool uses a distinct login and secret; credential reuse and
  runtime fallback to a Migration login are prohibited.
- The provisioner sets each role's database-level `search_path` to exactly
  `<owned_schema>, pg_catalog`. Connections verify that value at checkout and
  reset it before reuse; application SQL must not add untrusted schemas. The
  release coordinator uses only `platform_meta, pg_catalog` and each Migration
  pool uses only its single domain schema plus `pg_catalog`.
- Every domain owns its business tables, local idempotency records and local
  Outbox. A business write and its local Outbox row commit in one transaction;
  no business transaction writes `shared_audit` or another domain schema.
- `shared_audit` is an append-only audit-projection boundary, not a shared
  business schema or Outbox. Until a separately approved worker exists,
  Control Service owns the projection consumer. It consumes versioned events,
  records projection progress idempotently, and writes only `shared_audit`.
  It may not mutate another domain's business tables.
- Cross-domain records store stable identifiers and, where relevant, immutable
  version or hash references. Cross-schema foreign keys, cascades, triggers
  and database functions that read or write another domain's business schema
  are prohibited. Versioned REST contracts, controlled service calls and NATS
  events carry cross-domain coordination.
- Migrations are immutable, domain-scoped files in `migrations/<domain>/`.
  Each domain has a schema-local version history and an advisory lock. A
  version-controlled deployment manifest declares the domain order and the
  expected Migration identity. No service chooses a migration by discovery
  order or silently skips an unknown schema.
- `platform_meta` persists the release ledger and is owned by the
  release-coordinator login. Before any domain lock, the Migration Job holds a
  session-level global advisory lock. Its two signed `int4` keys are the first
  64 bits of `SHA-256(cluster_uuid || "labweaver:migration-release")`, split
  in network-byte order. Therefore every release for the same cluster
  contends for the same lock; a domain order cannot let two releases migrate
  different domains concurrently.
- The ledger binds the lock holder to release ID, canonical manifest SHA-256,
  Git commit, build/image digest, Job identity, attempt ID, timestamps, state,
  current domain and stable failure diagnostic. A lock holder creates a
  `running` attempt before acquiring a domain lock. PostgreSQL releases the
  session lock on crash; the next attempt marks the abandoned `running` record
  as failed before retrying. Only the same release identity may retry after
  validating the manifest and applied history. A different release encountering
  a failed or incomplete predecessor must stop until a reviewed forward repair
  records resolution.
- The only production executor is a dedicated Migration Job orchestrated by
  Ansible through the typed `cargo xtask migrate` interface. The Job uses the
  relevant Migration login and emits a machine-readable result. Services only
  verify the expected schema identity during startup/readiness; absent,
  unknown, ahead, behind or checksum-mismatched schema state fails closed with
  a stable diagnostic. Services never run migrations automatically.
- Production migrations use Expand/Contract and forward repair. Automatic
  down migration is prohibited. A deployment performs backup and compatibility
  checks before migration; any failed Migration blocks the release and leaves
  the failure as evidence. Destructive contraction requires a completed
  compatibility window and a separately reviewed approval.
- Every initial Migration file is transactional. A failed file rolls back its
  own schema and history transaction; earlier committed files remain applied.
  The ledger then records the failed domain and diagnostic, and no later domain
  is attempted. Non-transactional Migration is unsupported until a superseding
  ADR defines its atomicity and recovery evidence.
- Services classify schema identity before accepting business traffic.
  `DB_SCHEMA_MISSING`, `DB_SCHEMA_UNKNOWN`, `DB_SCHEMA_AHEAD`,
  `DB_SCHEMA_INCOMPLETE` and `DB_SCHEMA_CHECKSUM_MISMATCH` emit one structured
  error and terminate with non-zero status. `DB_SCHEMA_BEHIND` and
  `DB_SCHEMA_UNAVAILABLE` keep `/health/live` successful but return 503 from
  `/health/ready` and reject business traffic; they never trigger a service
  side Migration or repair.
- Control Service's temporary audit projection is strictly asynchronous:
  projection failure never blocks, rolls back or alters a business transaction.
  It marks only the projection unhealthy, records its watermark/diagnostic and
  blocks Release Gate evidence that requires audit completeness. Its successor
  is a separately approved non-business audit-projection worker. Transfer
  requires a scope-freeze exception, superseding ownership ADR, A/B review,
  E2 idempotent replay/backfill evidence, dual-write/watermark comparison and
  a verified cutover; until then Control remains owner.

## Manifest, history and report identity

The release manifest is canonically serialized before SHA-256 calculation and
lists release ID, Git commit, build/image digest, Migration tool version,
ordered domains, and every Migration ID/filename/SHA-256. The Job refuses an
identity mismatch between its immutable image/build metadata, supplied manifest
and checked-out Migration files. Each history row retains the domain, monotonic
ID, filename, SHA-256, applied timestamp, outcome, executor identity and
release identity. An already-applied file is never edited, renumbered or
replaced.

The machine-readable Job report repeats the release/manifest/build/Job
identity, lock attempt, per-domain outcomes, applied-history identities,
failure diagnostic and report SHA-256. The ledger records the report hash.
Together, manifest, Job, files, history, ledger and report form the required
identity chain; human logs and mutable deployment labels are not evidence.

## Alternatives considered

| Alternative | Rejected because |
| --- | --- |
| One shared runtime database login | Database permissions cannot enforce the service boundary. |
| One global Outbox table written by every service | It creates an unreviewed cross-domain write exception and couples all business transactions. |
| Service-start automatic migration | Concurrent rollout, partial readiness and retry behavior can hide an incomplete schema transition. |
| Cross-schema foreign keys | They couple domain deployment, retention and deletion semantics. |
| Automatic down scripts | Data-changing migrations cannot generally be reversed safely or proven to preserve evidence. |

## Consequences

Future persistence work must supply explicit bindings for database endpoint,
schema, runtime login, Migration login and expected schema identity. It must
not select a schema, role or Migration source by registration order or a
fallback. The release workflow must create and validate the Migration Job
before workloads are eligible to become Ready.

The first runtime implementation must prove provisioner revocation/default
privilege behavior, connection identity isolation, global and domain lock
behavior, crash/retry recording, immutable identity validation, the declared
startup/readiness diagnostics, rollback blocking, forward repair,
transaction-plus-Outbox atomicity, duplicate-event handling and audit
projection idempotency. The full required evidence is recorded in the test
plan and coverage matrix.

## Security, compatibility and rollback

Provisioner, coordinator and Migration credentials are deployment-only secrets
and must not be mounted into service workloads, logs, artifacts or normal
development examples. Runtime diagnostics include domain, expected and
observed identity, request/trace ID and blocking code, but no connection
string, SQL text containing sensitive data, token or credential.

Expand releases must remain compatible with the preceding application and
event version until the approved Contract phase. A failed release rolls back
application/Helm revisions only when the recorded compatibility check permits
it; database repair is a new reviewed forward Migration. Replacing this policy
requires a superseding ADR and a compatibility plan for all persisted domain
state and Migration histories.

## Evidence

Current evidence level: E0. This proposed ADR has no SQLx, database role,
Migration Job, lock, Outbox, audit-projection or startup-validation evidence.
Issue #17 remains open until the required implementation and runtime evidence
exists; this ADR does not close it.
