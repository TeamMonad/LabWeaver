# ADR 0001: VM Direct and Guacamole Access Paths

Status: deferred future proposal. It is not the selected P0 path for Issues #49/#53/#63.

## Context

LabWeaver may later need lower-latency native VM access without making Tailnet reachability an authorization bypass. The production P0 selected for the current course slice is `ssh alias@gateway`: Access Service owns authorization, Issue #63 owns the OpenSSH Gateway, and Issue #53 owns the real VM endpoint. This ADR preserves a possible later direction only.

## Decision

- `DirectAccessGrant`, Headscale/Router direct access and Guacamole are deferred and must not be implemented or reported as part of #49/#53/#63.
- Any future adoption requires a new scope decision, reviewed contracts and evidence independent of the OpenSSH Gateway path.
- Router enforcement is applied before Headscale Grants, but the grant is not usable until both services acknowledge the same revision.
- Browser SSH/VNC uses a custom Guacamole extension. The portal completes Keycloak Authorization Code plus PKCE; Access Service issues a one-time handoff token, and the extension validates it through an internal mutually authenticated Access Service call. Guacamole has no independent OIDC login, business authorization store or connection database.
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

Default-deny policy, exact endpoint scope, device-aware revisions, one-time handoff tokens, scoped credentials, Router receipts and safe audit fields are mandatory. The decision is supported only by E0 documentation until E1/E2 contract tests, E3 deployed enforcement evidence and E4 multi-role replay exist. Replace this decision if the required Router isolation receipt or Guacamole extension boundary cannot be verified; revert to browser-only Gateway access rather than silently weakening containment.
