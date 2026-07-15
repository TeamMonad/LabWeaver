# ADR 0008: Environment Resource Management API

## Status

Proposed for A+B review under Issue #81.

## Context

The v1 contract could create and mutate one Environment, but a production console could not
enumerate a course inventory, inspect asynchronous operations, discover the current actor's
AccessGrants, or resume a course event stream from a cursor. The generated Web client also exposed
transport details without one fail-closed authentication and RFC 9457 error boundary.

Returning internal aggregates would disclose actor identity, provider state, endpoint routing, or
authorization policy. Reusing aggregate-local sequence values as SSE cursors would also make replay
and REST-to-SSE resynchronization ambiguous.

## Decision

- `GET /api/v1/environments` is course-scoped. `courseId` is required and `projectId` is an optional
  narrowing filter; authorization is evaluated before inventory projection.
- Inventory, operation, and AccessGrant discovery return dedicated public snapshots. They do not
  expose actor IDs, provider names or payloads, endpoint host/port, credentials, or policy internals.
- Environment mutations return `EnvironmentOperationAccepted`, including `environmentId`,
  `operationId`, accepted revision, and a status URL.
- Operation snapshots expose stable lifecycle states, including `timed_out`, deadlines, cleanup
  facts, retry/cancel eligibility, stable diagnostics, and request/trace correlation.
- `GET /api/v1/environments/{environmentId}/operations/{operationId}` is the authoritative polling
  resource. Operation history and actor-scoped AccessGrant history are cursor-paged subresources.
- Public pages bind an opaque cursor to a `snapshotSequence` and `snapshotAt`. Expired cursors return
  410; malformed cursors return 400. A client must restart from a fresh snapshot after 410.
- `StreamSequence` is distinct from aggregate-local `Sequence`. The course SSE envelope contains an
  event identity, course/project scope, effective time, and monotonic stream sequence. REST snapshot
  plus SSE resume is the console synchronization path.
- The Web SDK is created through one factory. It explicitly selects either the portal BFF session
  (credentialed cookie plus synchronizer token on mutations) or an approved direct OIDC bearer
  client. Missing credentials fail closed; cancellation and timeout remain distinct; valid RFC 9457
  responses become typed errors. The generated paths own `/api/v1`; the configured base URL is an
  origin or deployment base, never another API prefix.

## Consequences

This is an additive v1 contract. Runtime handlers, persistence queries, authorization checks,
Outbox publication, and UI consumption remain separate implementation work. Until those paths have
E2/E3 evidence, Issue #81 provides E1 contract and browser-transport evidence only.

Any runtime implementation must keep cursor encoding opaque, enforce course/project authorization,
publish stream positions transactionally with state changes, and fail closed on stale snapshots,
replay gaps, missing identity, or unavailable Access ownership.

## Rollback

Before runtime adoption, revert the added contract projections and regenerate artifacts. After a
consumer ships, preserve v1 fields and operations and supersede incompatible semantics with a new
API version; do not reinterpret existing cursors or states.
