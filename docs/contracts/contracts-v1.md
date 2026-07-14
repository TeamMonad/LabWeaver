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

Problem packages complete atomically only after path, size, per-file hash and manifest hash validation. Course LLM policy permits only an explicit OpenAI Responses API binding with fixed model, revision and budgets; it has no fallback. Secret, token, private key, PII and student content outside `llmReadable` are denied, while allowlisted paths still pass classification, content and size policy.

Agent runs contain independent Environment and Evaluation tracks. Retry creates a monotonic attempt under the same run and preserves prior checkpoint, usage, cost and diagnostics. Candidate decisions are append-only and bind exact candidate/dependency revisions and hashes; stale policy, schema or trust revisions require revalidation and approval.

EnvironmentSpec separates business class from a closed Container/VM union. EvaluationSpec and GoalReview use v1; deterministic points are integers and LLM output has no scoring or release authority. SubmissionManifest supports only `exactFile` and `directoryTree`, rejects path escape, symlink, duplicate and overlap, and produces an immutable FrozenSubmission identity.

Build, artifact, scan and release semantics follow ADR 0006. Environment state exposes desired/observed state, generation and operation identity. Restart preserves mutable disk through stop/start; reset revokes Grants before restoring the published baseline. Access uses accepted Ed25519, FIDO2 Ed25519 or RSA >=3072 keys, server aliases, healthy one-to-one EndpointGrants and a 60-second session termination bound after revoke/expiry.

## HTTP, SSE and Gateway

Public REST is under `/api/v1`; Internal Gateway routes are under `/internal/v1` and require deployment-controlled service identity with mTLS. Every mutation requires `Idempotency-Key`; request-hash mismatch returns `LW_IDEMPOTENCY_CONFLICT`. Existing-resource mutations also require strong `If-Match`; missing/stale values return 412 `LW_REVISION_CONFLICT`. Long-running work returns 202 with operation identity. Lists use opaque cursors and bounded limits.

SSE uses `GET /api/v1/events?courseId=...`. `Last-Event-ID` and `after` are equivalent and conflicting values are rejected. Expired cursors return 410 `LW_SSE_CURSOR_EXPIRED`; gaps return `LW_SSE_CURSOR_GAP` and require REST snapshot recovery. Events contain only sanitized identity, revision/hash and diagnostic fields.

SSH authorization accepts server alias and Gateway identity, never target host/port. The response is scoped to one key, ForceCommand token, endpoint identity and shortest validity. It grants no generic shell, forwarding, SCP or SFTP semantics.

## Generation and compatibility

```sh
cargo xtask contracts generate
cargo xtask contracts check
cargo xtask test --suite contract
```

Generated outputs are `schemas/contracts/v1/`, two files in `schemas/openapi/`, and `web/src/generated/contracts/`. Only Public OpenAPI feeds the Web SDK. v1 breaking changes require parallel v2 publication; existing subjects and fields cannot be reinterpreted.
