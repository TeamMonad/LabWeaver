# Active Blockers

## Cross-role Day 1 gate

- Frontend build and B/C/D first branches or PRs are not role A deliverables.
- Owner: B, C and D respectively.
- Exit condition: each owner supplies its own reviewed PR and required evidence.

## ACCESS-01a implementation evidence

- The formal trust-boundary design is documented, but no Keycloak integration, Headscale enrollment/policy compiler, Access Service grant persistence, Access Gateway, or session-revocation mechanism exists in the current evidence identity.
- Owner: A for Access contracts and policy boundary; B for Environment endpoint integration; D for deployed verification and replay evidence.
- Exit condition: reviewed contracts and implementations provide E1/E2 authorization tests, E3 deployed Gateway and policy evidence, and E4 multi-role expiry/revocation replay showing Gateway-only container and VM access.
## Resolved blockers

- GitHub Project write scope was restored and Issue #20 was closed as completed.
- All 20 governance Issues are present in `LabWeaver Delivery`; Issues #5–#19 were read back with `Workflow Status=Ready` and `Delivery Priority=P0`.
- GitHub exposes built-in status/priority/date fields as Issue-derived fields. Writable Scrum metadata therefore uses `Workflow Status` and `Delivery Priority`, while `Target date` is updated through the Issue field API.
