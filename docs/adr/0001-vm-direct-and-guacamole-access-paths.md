# ADR 0001: VM Direct and Guacamole Access Paths (Superseded for Browser Consoles)

Status: deferred future direct-access proposal. Browser-console guidance is superseded by ADR 0012.

## Context

LabWeaver may later need lower-latency native VM access without making reachability an authorization bypass. The production P0 selected for the current course slice is the fixed account flow `ssh gateway@gateway connect <server-alias>`: Access Service owns authorization, Issue #63 owns the OpenSSH Gateway, and Issue #53 owns the real VM endpoint. This ADR preserves a possible later direction only.

## Decision

- `DirectAccessGrant` and Headscale/Router direct access are deferred and must not be implemented or reported as part of #49/#53/#63.
- Any future adoption requires a new scope decision, reviewed contracts and evidence independent of the OpenSSH Gateway path.
- Router enforcement is applied before Headscale Grants, but the grant is not usable until both services acknowledge the same revision.
- The historical Guacamole extension proposal is superseded. Browser xterm/noVNC follows ADR 0012's AccessGrant-scoped ConsoleCapability contract.
- An ordinary revoke isolates the affected device-to-endpoint flow within 60 seconds by Router filtering and connection-state clearing. Endpoint isolation and VM stop are controlled escalation actions; VM stop requires no remaining active grants.

## Alternatives rejected

- OpenSSH Gateway access: selected for the current P0 because it keeps the authorization and session-termination boundary explicit without exposing VM addresses to clients.
- Headscale Grants alone: rejected because Access Service remains the authorization truth and established-session containment needs an observed Router action.
- Kubernetes NetworkPolicy as the session terminator: rejected because existing-connection behavior is implementation-defined.
- Guacamole built-in OIDC or JDBC authorization: rejected because it would introduce a second login/session model or a competing authorization data owner.
- Unconditional VM stop on every revoke: rejected because it disrupts unrelated valid grants.

## Consequences and compatibility

This ADR adds no current API, schema, Migration or runtime requirement. The current implementation source of truth is the versioned Access/Environment/OpenSSH Gateway contract. Future direct-access work requires a separate Issue and dual review because it changes authorization, endpoint exposure, credentials and VM lifecycle behavior.

## Security, evidence and replacement

Default-deny policy, exact endpoint scope, device-aware revisions, scoped credentials, Router receipts and safe audit fields remain mandatory for any future direct-access proposal. This ADR is not browser-console implementation guidance.
