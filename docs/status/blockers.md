# Active Blockers

## Cross-role Day 1 gate

- Frontend build and B/C/D first branches or PRs are not role A deliverables.
- Owner: B, C and D respectively.
- Exit condition: each owner supplies its own reviewed PR and required evidence.

## ACCESS-01a implementation evidence

- The dual-path trust boundary is documented, but no Keycloak handoff, Access Service grant persistence, Headscale Grants compiler, Router firewall controller, Guacamole extension, scoped SSH/VNC credential issuer, containment receipt or VM-stop escalation exists in the current evidence identity.
- Owner: A for Access contracts, policy boundary and Guacamole handoff; B for Environment endpoint and scoped-credential integration; D for deployed verification and replay evidence.
- Exit condition: reviewed contracts and implementations provide E1/E2 dual-revision, device-scope and credential tests; E3 deployed Headscale, Router and Guacamole evidence; and E4 multi-device/multi-role replay proving direct and browser paths, 60-second Router isolation, unaffected valid grants and escalation behavior.
## NATS runtime implementation

- Issue #18 freezes only the public Subject, CloudEvents, Outbox, ordering,
  consumer and quarantine design. No NATS client, Stream, durable Consumer,
  Outbox publisher, delivery manifest, quarantine path or runtime envelope
  validation exists.
- Owner: A for the contract and message boundary; B must complete the required
  high-risk messaging review before implementation.
- Exit condition: a separately scoped implementation issue provides E2
  PostgreSQL and JetStream evidence for atomic Outbox publication, duplicate
  and replay idempotency, stale/gap sequence blocking, durable-consumer
  recovery, acknowledgement behaviour and terminal quarantine diagnostics.

## Agent Tool permission and approval contract

- Tool bindings do not yet model or enforce filesystem, network or runtime permissions.
- Elevated/high-risk Tools fail closed because no reviewed approval evidence contract exists.
- Owner: A freezes permission vocabulary and approval identity, revision, expiry and candidate/input
  binding; B implements the accepted contract.
- Exit condition: reviewed contract plus negative tests for permission escalation, stale/mismatched
  approval and repeated or changed-input dispatch.
- Impact: Issue #13 remains partially implemented and must not be submitted as complete.

## AG-01b Fixture Backend

- `environment.yaml` generation is blocked because the Environment domain vocabulary and Schema in
  Issue #16 are not frozen or implemented.
- Evaluation/LLM fixture generation is also deferred to AG-01b and is not evidence for AG-01a.
- Owner: A for the Environment domain decision; B consumes the accepted contract in AG-01b.
- Exit condition: reviewed Environment type/Schema with candidate and teacher-approval semantics.
- Impact: Issue #13 does not claim a Fixture Backend or Environment + Evaluation generation path.

## Resolved blockers

- GitHub Project write scope was restored and Issue #20 was closed as completed.
- All 20 governance Issues are present in `LabWeaver Delivery`; Issues #5–#19 were read back with `Workflow Status=Ready` and `Delivery Priority=P0`.
- GitHub exposes built-in status/priority/date fields as Issue-derived fields. Writable Scrum metadata therefore uses `Workflow Status` and `Delivery Priority`, while `Target date` is updated through the Issue field API.
