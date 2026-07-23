# ADR 0012: Access-bound Container Browser Terminal

Status: Accepted for Issue #131 implementation; connected verification pending

Date: 2026-07-23

## Context

Issue #131 must demonstrate editing and freezing a real Container workspace from
the existing student console. The original Sprint 2 boundary supported bounded
HTTP forwarding only and rejected every Upgrade. A direct Pod route, terminal
sidecar, shell discovery fallback, or terminal transcript would bypass the
AccessGrant and Environment authorities or create another production path.

This ADR records the approved post-freeze exception. It does not change the
KubeVirt, Evaluation, BuildKit or Resource Service boundaries.

## Decision

An optional `TerminalSpec` is part of the immutable Container
`EnvironmentSpec`. It binds one normalized absolute executable, at most 32
direct arguments and `workingDirectory: /workspace`. There is no shell-string
interpolation, executable probing or fallback. Releases without this field do
not produce `browser_terminal`.

The connection path is:

```text
xterm.js
-> same-origin Access WebSocket
-> mTLS Environment terminal bridge
-> mTLS container-executor
-> Kubernetes exec PTY in the unique Ready runtime Pod
```

Access validates Origin, the `labweaver.terminal.v1` subprotocol, BFF session,
course membership, active AccessGrant, endpoint revision, capability and the
current Environment eligibility before Upgrade. PostgreSQL stores only bounded
session identity, state, heartbeat, expiry and diagnostic metadata. A
transactional advisory lock enforces the deployment-wide configured admission
limit, default 128. There is no idle timeout; grant expiry remains at most one
hour and the five-second heartbeat terminates revoked or expired authorization
well within the 60-second bound.

Environment revalidates the authoritative instance, release identity,
Container runtime, exact revisions, endpoint capability and `TerminalSpec`.
Only container-executor receives the Kubernetes credential. Its explicit
`kube` 0.99.0 client lists the environment namespace using the exact runtime,
environment and course labels; zero, multiple, terminating or non-Ready Pods
are rejected. The command runs only in container `runtime` with PTY, stdin,
stdout and bounded resize.

The Web UI pins `@xterm/xterm` 6.0.0 and `@xterm/addon-fit` 0.11.0. It exposes
connect, disconnect, manual reconnect, resize, full-screen state and stable
diagnostics. It never automatically retries authorization, protocol or
security failures.

## Security and data consequences

- Terminal bytes never enter PostgreSQL, NATS, ordinary logs, traces, reports
  or evidence. Browser scrollback is ephemeral UI state.
- `pods get/list/watch` and `pods/exec create` are granted only to
  container-executor. Access and Environment receive no Kubernetes token.
- Every internal hop requires a dedicated reviewed mTLS identity and fixed
  destination. The browser cannot select namespace, Pod, container,
  executable, arguments or working directory.
- Existing HTTP forwarding remains separate. Its generic Upgrade rejection is
  unchanged; only `/connect/{endpointGrantId}/terminal` accepts WebSocket.
- noVNC, VM terminal, Resource Service, Fixture fallback and public runtime
  routes remain excluded.

## Compatibility

`TerminalSpec`, endpoint capabilities and terminal URLs are additive for the
new reader. A missing terminal remains valid and disables the capability.
Because the v1 document uses strict unknown-field rejection, a release that
contains `terminal` cannot be read by an older binary. Deployment and rollback
therefore retain a schema-aware reader after the first such release is
published.

The Access migration is additive. Closed session metadata may be retained by
the existing operational retention policy, but contains no transcript.

## Alternatives rejected

- Direct Access-to-Kubernetes exec would give the Access domain Kubernetes
  credentials and bypass Environment release validation.
- NATS transport for terminal bytes would turn a control/event bus into an
  interactive data plane and risk persistence or replay.
- A terminal sidecar or per-environment public Service would add a second
  runtime path and enlarge the network and image surface.
- Guessing `/bin/sh`, using a shell command string or falling back to another
  executable would hide release defects and violate approval binding.

## Evidence and rollback

Local Contract, Rust, Web and Helm checks prove only implementation consistency.
Acceptance requires a same-identity ex3 deployment and real Container replay:
write in `/workspace`, disconnect/reconnect, stop/start, freeze, revoke,
cross-course denial and delete cleanup, without transcript evidence.

Rollback first disables `browser_terminal` and blocks new sessions, then closes
active sessions and removes terminal mTLS routing, and only then removes
`pods/exec`. The additive migration and schema-aware reader remain installed.
