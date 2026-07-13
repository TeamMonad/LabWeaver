# ADR 0001: VM Direct and Guacamole Access Paths

Status: proposed, pending B security review and D verification review.

## Context

LabWeaver needs lower-latency native VM access without making Tailnet reachability an authorization bypass. The prior Gateway-only design did not distinguish native VM access from browser-mediated SSH/VNC and did not define a reliable containment boundary for established direct sessions.

## Decision

- Native SSH and VNC use an explicit `DirectAccessGrant` through Headscale Grants and a designated Subnet Router firewall. The grant is materialized for every Active device owned by its subject and only for the exact VM address, protocol and port.
- Router enforcement is applied before Headscale Grants, but the grant is not usable until both services acknowledge the same revision.
- Browser SSH/VNC uses a custom Guacamole extension. The portal completes Keycloak Authorization Code plus PKCE; Access Service issues a one-time handoff token, and the extension validates it through an internal mutually authenticated Access Service call. Guacamole has no independent OIDC login, business authorization store or connection database.
- An ordinary revoke isolates the affected device-to-endpoint flow within 60 seconds by Router filtering and connection-state clearing. Endpoint isolation and VM stop are controlled escalation actions; VM stop requires no remaining active grants.

## Alternatives rejected

- Gateway-only access: retained for HTTP endpoints but does not provide native SSH/VNC direct access.
- Headscale Grants alone: rejected because Access Service remains the authorization truth and established-session containment needs an observed Router action.
- Kubernetes NetworkPolicy as the session terminator: rejected because existing-connection behavior is implementation-defined.
- Guacamole built-in OIDC or JDBC authorization: rejected because it would introduce a second login/session model or a competing authorization data owner.
- Unconditional VM stop on every revoke: rejected because it disrupts unrelated valid grants.

## Consequences and compatibility

This adds planned Access Service, Policy Compiler, Router, Environment Service and Guacamole extension contracts but no current API, schema, Migration or runtime implementation. Existing Gateway paths remain valid for HTTP endpoints. Future implementations require dual review because they change authorization, endpoint exposure, credentials and VM lifecycle behavior.

## Security, evidence and replacement

Default-deny policy, exact endpoint scope, device-aware revisions, one-time handoff tokens, scoped credentials, Router receipts and safe audit fields are mandatory. The decision is supported only by E0 documentation until E1/E2 contract tests, E3 deployed enforcement evidence and E4 multi-role replay exist. Replace this decision if the required Router isolation receipt or Guacamole extension boundary cannot be verified; revert to browser-only Gateway access rather than silently weakening containment.
