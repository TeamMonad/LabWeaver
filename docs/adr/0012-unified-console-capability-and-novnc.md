# ADR 0012: Unified Console Capability and noVNC Boundary

Status: Accepted for contract implementation; runtime verification pending

Date: 2026-07-29

## Context

The historical browser SSH/VNC proposal depended on Guacamole and an independent
extension boundary. It is superseded for browser consoles. Container xterm and
KubeVirt noVNC need one Access-owned admission model without exposing a runtime
endpoint, VNC password, Kubernetes credential, terminal bytes, or VNC frames to
the browser or ordinary observability systems.

Issue #122 freezes the public and internal contract only. It does not implement
the Access proxy, Environment bridge, runtime executor stream, database tables,
deployment configuration, or browser UI.

## Decision

`ConsoleCapability` is an AccessGrant-scoped, one-time browser handoff for
`xterm` or `novnc`. Discovery and issuance are separate:

- `GET /api/v1/access-grants/{grantId}/console-capabilities` returns eligible
  kinds and identity fences only.
- `POST /api/v1/access-grants/{grantId}/console-capabilities` requires BFF,
  Origin, CSRF, `Idempotency-Key`, expected AccessGrant/Environment revisions,
  and an exact Work Lease fence.
- A successful issuance returns an opaque same-origin relative locator and
  versioned WebSocket subprotocol. It expires exactly 30 seconds after issue.
  The handoff secret is sent only through a path-scoped `Secure`, `HttpOnly`,
  `SameSite=Strict` cookie and is never part of the response body or URL.

Work environments require a matching Lease ID/revision/expiry fence. Experiment
environments explicitly have no Lease fence. The locator is consumed once;
manual reconnect issues a fresh capability and automatic security retries are
forbidden.

The intended stream is:

```text
Web xterm/noVNC -> Access WebSocket proxy -> Environment mTLS bridge
-> runtime executor -> Container exec or KubeVirt VMI /vnc
```

KubeVirt VNC is connected by a service-side least-privilege identity to the VMI
`/vnc` WebSocket subresource. There is no guest VNC password, public
websockify/noVNC endpoint, Guacamole, new microservice, or browser-visible
Kubernetes credential.

## Session and Failure Semantics

An established session uses its known expiry as a local deadline and receives
revoke, logout, membership, Lease, lifecycle, and revision changes through the
existing transactional Outbox and durable JetStream state stream. It does not
poll the database for authorization. Loss of the durable control subscription,
session ownership, NATS connectivity, or upstream mTLS closes all sessions
owned by that proxy process. A failed termination follows the existing
60-second overdue boundary and reports a safe diagnostic.

The stable error family covers denied, expired, consumed, revision conflict,
Lease invalid, environment not ready, subprotocol mismatch, upstream
unavailable, control-channel loss, and authorization ended. Logs, traces,
reports, and events retain only identities, revisions, timestamps, states and
safe diagnostics.

## Compatibility and Rollback

The new AccessGrant-level resource is additive. Existing `EndpointProtocol`,
`EndpointGrantSnapshot`, and Environment endpoint wire shapes are unchanged.
The shared-cluster Run ID belongs only in evidence envelopes that bind source,
package, configuration, migration, image, runtime, session, trace and report
identities; it is not a browser API field.

Issue #122 adds no migration. The re-opened #131 implementation will introduce
the unified capability/session persistence after this contract merges. #124
implements noVNC/KubeVirt; #126 provides E4 and Release Gate evidence.

Before consumers exist, rollback reverts the contract release. After consumers
exist, disable new issuance, terminate sessions, then roll back proxy/runtime
and RBAC while retaining compatible readers and additive persistence.

## Evidence

Issue #122 completes at E2 through generated Schema/OpenAPI/Web SDK checks,
cross-consumer compilation and negative contract tests. Fixture, historical
PR #138, mixed-source demonstrations and connected runtime evidence do not
close this ADR's downstream E3/E4 obligations.
