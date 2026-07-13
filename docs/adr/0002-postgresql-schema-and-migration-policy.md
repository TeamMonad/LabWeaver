# ADR 0002: PostgreSQL Schema Ownership and Migration Policy

Status: proposed; requires Architecture Owner A and Agent/Backend Owner B review.

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

- A course deployment uses one PostgreSQL cluster with the domain schemas
  `control`, `access`, `environment`, `agent`, `evaluation` and `resource`.
  Each domain has separate schema-owner, Migration and runtime logins. Runtime
  logins receive only the least privileges required for their own schema and
  never receive `CREATE`, `ALTER`, `DROP`, ownership or cross-domain write
  privileges.
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

## Migration identity and evidence

An applied Migration record must retain its domain, monotonic ID, filename,
SHA-256, applied timestamp, result, executor identity and release build
identity. An already-applied file is never edited, renumbered or replaced.
The deployment manifest and Job report must allow the Release Gate to connect
the expected identity, database state and release build without relying on a
human log or mutable deployment label.

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

The first runtime implementation must prove domain-role isolation, advisory
lock behavior, immutable identity validation, rollback blocking, forward
repair, transaction-plus-Outbox atomicity, duplicate-event handling and audit
projection idempotency. The full required evidence is recorded in the test
plan and coverage matrix.

## Security, compatibility and rollback

Migration credentials are deployment-only secrets and must not be mounted into
service workloads, logs, artifacts or normal development examples. Runtime
diagnostics include domain, expected and observed identity, request/trace ID
and blocking code, but no connection string, SQL text containing sensitive
data, token or credential.

Expand releases must remain compatible with the preceding application and
event version until the approved Contract phase. A failed release rolls back
application/Helm revisions only when the recorded compatibility check permits
it; database repair is a new reviewed forward Migration. Replacing this policy
requires a superseding ADR and a compatibility plan for all persisted domain
state and Migration histories.

## Evidence

Current evidence level: E0. This ADR is a proposed design and has no SQLx,
database, Migration Job, Outbox, audit-projection or startup-validation
evidence.
