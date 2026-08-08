# ADR 0012: Unified Console Capability and noVNC Boundary

Status: Accepted; Container xterm and KubeVirt noVNC implemented locally, connected verification pending

Date: 2026-07-29

## Context

The historical browser SSH/VNC proposal depended on Guacamole and an independent
extension boundary. It is superseded for browser consoles. Container xterm and
KubeVirt noVNC need one Access-owned admission model without exposing a runtime
endpoint, VNC password, Kubernetes credential, terminal bytes, or VNC frames to
the browser or ordinary observability systems.

Issue #122 froze the public contract. Issue #131 added the Container xterm
implementation, and #124 adds KubeVirt noVNC while preserving that wire shape.
#126 owns connected verification and Release Gate evidence.

## Decision

`ConsoleCapability` is an AccessGrant-scoped, one-time browser handoff for
`xterm` or `novnc`. Discovery and issuance are separate:

- `GET /api/v1/access-grants/{grantId}/console-capabilities` returns eligible
  kinds and identity fences only.
- `POST /api/v1/access-grants/{grantId}/console-capabilities` requires BFF,
  Origin, CSRF, `Idempotency-Key`, expected AccessGrant/Environment revisions,
  and an exact Work Lease fence.
- The `If-Match` strong ETag is the same AccessGrant revision as
  `expectedAccessGrantRevision`; the server rejects a mismatch. The body copy
  is retained only as part of the idempotency fingerprint.
- A successful issuance returns an opaque same-origin relative locator and
  versioned WebSocket subprotocol. It expires exactly 30 seconds after issue.
  The handoff secret is sent only through a path-scoped `Secure`, `HttpOnly`,
  `SameSite=Strict` cookie with `Max-Age=30` and a Path equal to the returned
  locator; it is never part of the response body, URL, SDK, log or Debug
  output. The locator is exactly one non-empty opaque segment after
  `/connect/console/`.

Work environments require a matching Lease ID/revision/expiry fence. Experiment
environments explicitly have no Lease fence. The locator is consumed once;
manual reconnect issues a fresh capability and automatic security retries are
forbidden.

Availability and an issued capability must not outlive the Work Lease. If fewer
than 30 seconds remain in the authoritative AccessGrant, environment or Lease
boundary, issuance is rejected with `LW_CONSOLE_CAPABILITY_EXPIRED`; the server
does not shorten a capability or mint a partial-lifetime handoff.

The intended stream is:

```text
Web xterm/noVNC -> Access WebSocket proxy -> Environment mTLS bridge
-> runtime executor -> Container exec or KubeVirt VMI /vnc
```

KubeVirt VNC is connected by a service-side least-privilege identity to the VMI
`/vnc` WebSocket subresource. There is no guest VNC password, public
websockify/noVNC endpoint, Guacamole, new microservice, or browser-visible
Kubernetes credential.

Environment returns an internal tagged `EnvironmentConsoleBinding`: Container
binds `xterm` plus the immutable `TerminalSpec`, while KubeVirt binds `novnc`
without a browser-visible VMI locator. The Environment bridge revalidates the
running instance, revision, Work Lease and release/runtime binding for every
connection. Its `kubevirt-console-executor` process mode then locks the accepted
running observation to canonical namespace `lw-env-{environmentId}`, fixed VMI
name `runtime`, VMI UID and identity labels before opening the official VNC
subresource with `plain.kubevirt.io`.

The executor receives a dedicated ServiceAccount. Its only Kubernetes API
permissions are `get` on the fixed-name `runtime` VMI and `get` on
`virtualmachineinstances/vnc`; it cannot read Secrets, mutate VM lifecycle or
access Pods. Network policy permits only Environment mTLS ingress and
Kubernetes API egress. VNC binary, ping, pong and close frames are relayed with
a bounded frame size and backpressure and are never parsed, cached or logged.

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

Migration `access/0002_console_capabilities_and_sessions.sql` adds additive,
metadata-only capability/session persistence. It stores an AEAD-encrypted
handoff secret until atomic redemption and then scrubs the ciphertext. It does
not store terminal input/output, transcripts, cookies, Kubernetes targets or
credentials. #124 implements noVNC/KubeVirt; #126 provides connected evidence.

Environment Service, Container executor and KubeVirt console executor receive distinct platform mTLS
identities from the unified authority workflow. Rotation replaces their
`mtls-ca.pem`, `tls.crt` and `tls.key` bundle entries together with NATS
credentials, and the credential registry binds the resulting application
bundle hash without exposing certificate material or controller locators.

Before consumers exist, rollback reverts the contract release. After consumers
exist, disable new issuance, terminate sessions, then roll back proxy/runtime
and RBAC while retaining compatible readers and additive persistence.

## Evidence

Issue #131 locally verifies the Access capability/session transaction,
Environment-authoritative eligibility, mTLS bridge, fixed Container PTY exec,
binary/control framing, bounded resize/output and Web reconnect behavior.
#124 locally verifies runtime-tagged discovery and issuance, kind-safe atomic
consumption, authoritative VMI identity fencing, bounded RFB relay and the
dedicated deployment security boundary.
Fixture, historical PR #138 and mixed-source demonstrations remain
non-connected evidence; #126 must close the shared-cluster obligation.
