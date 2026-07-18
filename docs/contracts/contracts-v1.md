# Contracts v1

## Authority and evidence

`crates/contracts` is the semantic single source of truth for public types, validation, lifecycle rules, REST/SSE, Gateway Internal API and NATS payloads. Generated JSON Schema, OpenAPI and the Public Axios SDK are projections and must be byte-identical to regeneration. This delivery is E1 only: it proves contract behavior, not Handler, PostgreSQL, Outbox, Provider, Gateway, Kubernetes or browser runtime availability.

## Stable wire rules

- Public identifiers are non-interchangeable UUIDv7 newtypes; persisted revisions are non-zero `u64`, and event/SSE sequence values are monotonic `u64`.
- Time is RFC 3339 UTC with literal `Z` and millisecond precision. Expiry, timeout and retry budgets are explicit.
- SHA-256 is lowercase 64-character hex. Structured documents hash RFC 8785 canonical JSON; files and binary artifacts hash raw bytes.
- Request documents and events reject unknown fields, duplicate keys, missing/empty/oversized values, non-finite numbers and unsupported versions. Responses may gain optional fields in v1.
- Every JSON ingress must decode through `contracts::parse_strict_json`; this rejects duplicate keys and trailing documents before typed decoding instead of losing conflicts in a default map.
- Errors use RFC 9457 `application/problem+json` with `diagnosticCode`, request/trace identity, retryability and bounded violations. Unknown `LW_*` diagnostics are blocking.

## Domain invariants

Problem packages complete atomically only after path, size, per-file hash and manifest hash validation. Course LLM policy permits only an explicit Claude Code worker binding with an opaque runtime profile, exact model, CLI version, worker image hash, runtime-configuration hash, bounded per-worker in-flight limit, revision and budgets; it has no SDK, provider or model fallback. Provider transport and authentication remain deployment-owned Claude Code configuration and are not public contract fields. Secret, token, private key, PII and student content outside `llmReadable` are denied, while allowlisted paths still pass classification, content and size policy.

Agent runs contain independent Environment and Evaluation tracks. Each due track has one PostgreSQL-authoritative worker lease; heartbeat extends that exact token, expiry permits a monotonic retry, durable cancellation is observed by the lease owner, and terminal completion immediately retains the track checkpoint before aggregate derivation. Claude Code input can be constructed only by the service-owned ProblemPackage gate after package/object size and SHA-256 verification, course-policy matching and revision-bound hard-deny classification. A track accepts output only after CLI-version verification, result-envelope validation, immutable budget enforcement, protected-field rejection, exact generated Schema, semantic validation and canonical output hashing all succeed. Retry creates a monotonic attempt under the same run and preserves prior checkpoint, usage, cost and diagnostics. Candidate decisions are append-only and bind exact candidate/dependency revisions and hashes; stale policy, schema or trust revisions require revalidation and approval.

EnvironmentSpec separates business class from a closed Container/VM union. EvaluationSpec and GoalReview use v1; deterministic points are integers and LLM output has no scoring or release authority. SubmissionManifest supports only `exactFile` and `directoryTree`, rejects path escape, symlink, duplicate and overlap, and produces an immutable FrozenSubmission identity.

Build, artifact, scan and release semantics follow ADR 0006. Environment state exposes desired/observed state, generation and operation identity. Restart preserves mutable disk through stop/start; reset revokes Grants before restoring the published baseline. Access uses accepted Ed25519, FIDO2 Ed25519 or RSA >=3072 keys, exact OpenSSH SHA-256 fingerprint matching, server aliases, healthy one-to-one `EndpointGrant`s and a 60-second session termination bound after revoke, expiry or key deletion. `AccessGrant` follows `requested -> active|denied|revoked` and `active -> expired|revoked`; renew uses `Idempotency-Key` plus strong `If-Match`, extends time only and creates a new revision.

## HTTP, SSE and Gateway

Public REST is under `/api/v1`; Internal Gateway routes are under `/internal/v1` and require deployment-controlled service identity with mTLS. Every mutation requires `Idempotency-Key`; request-hash mismatch returns `LW_IDEMPOTENCY_CONFLICT`. Existing-resource mutations also require strong `If-Match`; missing/stale values return 412 `LW_REVISION_CONFLICT`. Long-running Environment work returns 202 with both Environment and operation identity. Course-scoped inventory, operation history and actor-scoped AccessGrant discovery use opaque cursors, bounded limits and snapshot stream positions; malformed cursors return 400 and expired cursors return 410. Aggregate-local `Sequence` and public `StreamSequence` are distinct wire types. `StreamSequence` is a canonical unsigned decimal string across REST snapshots, event envelopes, SSE `id`, `Last-Event-ID`, and `after`, preserving the full `u64` range without JavaScript number coercion.

SSE uses `GET /api/v1/events?courseId=...`. `Last-Event-ID` and `after` are equivalent and conflicting values are rejected. Expired cursors return 410 `LW_SSE_CURSOR_EXPIRED`; gaps return `LW_SSE_CURSOR_GAP` and require REST snapshot recovery. Events contain only sanitized identity, revision/hash and diagnostic fields.

OpenSSH authenticates only the fixed local account `gateway`. `AuthorizedKeysCommand` accepts the presented key and Gateway identity but deliberately has no target field because OpenSSH resolves the local account before running the helper. The client selects one server-generated alias only through the exact forced-command grammar `connect lw-<id>`. Access Service revalidates that alias, key, actor, grant, membership and Environment endpoint before consuming the one-time token. Neither phase accepts a target host/port, generic shell, forwarding, SCP or SFTP semantics.

The Gateway creates, heartbeats and closes sessions through dedicated request types. A one-time opaque token is bound to Gateway identity, connection, key, grant revision and endpoint; only its SHA-256 digest is stored and consumption is atomic. A session records the key and grant revision. Revocation creates an explicit `terminating` deadline, and a missing close receipt becomes `terminationOverdue` rather than a successful close.

Environment exposes a read-only mTLS endpoint-eligibility decision bound to environment/course/subject/revision, eligibility/Lease expiry and the exact requested endpoint protocol, health and revision. The response never contains host, port, credentials or Provider internals, and Access never reads the Environment schema directly.

## Generation and compatibility

```sh
cargo xtask contracts generate
cargo xtask contracts check
cargo xtask test --suite contract
```

Generated outputs are `schemas/contracts/v1/`, two files in `schemas/openapi/`, and `web/src/generated/contracts/`. Only Public OpenAPI feeds the Web SDK. v1 breaking changes require parallel v2 publication; existing subjects and fields cannot be reinterpreted.
