# Rust runtime logging contract v1

`labweaver.log.v1` is the internal structured-log contract for Rust production
processes. Its machine-readable schema is
[`schemas/contracts/v1/internal/labweaver-log-v1.schema.json`](../../schemas/contracts/v1/internal/labweaver-log-v1.schema.json).
Logs are diagnostic evidence only: they do not participate in business hashes,
replay decisions, scoring, or release conclusions.

## Required envelope

Every emitted JSON event has `event`, `service`, `component`, `operation`,
`outcome`, and `duration_ms`, plus `schema=labweaver.log.v1`, `level`, and
`timestamp_unix_ms`. The shared formatter supplies conservative values for a
legacy callsite that has not declared every field; new boundary events must set
the six semantic fields explicitly.

Use the following contextual fields when they exist at that boundary:

- `request_id` and the 32-lowercase-hex W3C `trace_id`;
- stable actor, course, project, run, environment, resource, lease, session,
  operation, event, and message identities;
- `revision`, `attempt`, `delivery_attempt`, `binding`, `provider`, `stream`,
  `consumer`, and catalogued `subject`;
- `diagnostic_code`, `error_kind`, `failure_stage`, `retryable`, and an audited
  token-valued `safe_detail` for failures.

HTTP logs use the Axum route template. Raw URI, query, endpoint, and URL values
are forbidden.

## Correlation

The HTTP boundary validates `x-request-id` and W3C `traceparent` before invoking
a handler. A missing request ID is a UUIDv7. A missing trace context is a valid
locally generated W3C context. Invalid supplied values return RFC 9457 JSON with
HTTP 400 and a stable diagnostic; they are never silently replaced.

The validated headers are copied to downstream HTTP requests. The CloudEvent
envelope carries the trace identity through Outbox and NATS. JetStream events
also record the stream, durable consumer, catalogued subject, message identity,
and delivery attempt. Worker and provider fences preserve the same trace
identity through claim, stage, retry, fence, terminal, and cleanup decisions.
No tracing exporter is enabled by this contract.

## Levels and ownership

- `INFO`: boundary completion, state transition, provider selection, and
  successful cleanup.
- `DEBUG`: safe stage detail, timings, and idle polling. Idle polling must not
  appear at `INFO`.
- `WARN`: rejection, retryable failure, timeout, recovery, and retry scheduling.
- `ERROR`: terminal non-retryable failure, emitted once by the boundary that
  owns the root cause or final disposition.

Intermediate layers return typed errors with their source chain intact. They do
not log the raw chain. A retry does not emit a terminal `ERROR`; exhaustion does.

## Privacy boundary

The shared formatter uses an allowlist. Values carried in `error`, `reason`,
`detail`, `path`, `url`, `endpoint`, `object_key`, `locator`, `token`, `secret`,
`payload`, `body`, `request`, `response`, `command`, `transcript`, address, and
peer-address fields are replaced with `redacted_unclassified`. Unknown fields
are omitted. `safe_detail` accepts only a bounded lowercase token; other values
become `redacted_unclassified`.

Never log secrets, credentials, public keys, temporary certificates, original
submissions, terminal content, full request/response bodies, object-store
locators, native handles, or complete `Debug` objects. Add a reviewed stable
field or diagnostic when more fault-localization information is required.

## Acceptance boundary

Local tests validate schema conformance, INFO/DEBUG separation, W3C extraction,
header propagation, invalid-header rejection, UUIDv7 request IDs, provider and
worker trace continuity, and protected-value absence. Connected evidence is
owned by Sprint 3 acceptance Issue #126 and is not inferred from local logs.
