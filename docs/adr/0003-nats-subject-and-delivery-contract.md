# ADR 0003: NATS Subject and Delivery Contract

- Status: proposed
- Issue: #18
- Owners: A (architecture and messaging boundary), B review required
- Evidence level: E0 design decision

## Context

LabWeaver crosses service boundaries through versioned REST contracts,
immutable artifact references, controlled service calls and NATS JetStream.
The v2.1 draft lists example Subjects and states that delivery is
at-least-once, but it does not define a canonical event envelope, publishing
boundary, ordering scope, consumer failure behaviour or a complete ownership
catalog. Without those rules, a worker could become an accidental state owner,
duplicate delivery could create duplicate business effects, and an invalid
message could disappear without an actionable diagnostic.

This ADR resolves GitHub Issue #18 at the documentation level only. It does
not add a NATS client, Stream, Consumer, Outbox publisher, schema generator or
runtime validation.

## Decision

- Public v1 Subjects use
  `labweaver.<owner>.<aggregate>.<event>.v1`. `owner` is the service that owns
  the authoritative aggregate state. `aggregate` and `event` use lowercase
  snake_case. Each catalog entry has exactly one state owner and one or more
  explicitly declared handling purposes; a Command has exactly one.
- A breaking payload or semantic change publishes a parallel `.v2` Subject.
  A v1 Subject is never reinterpreted in place. Compatible producers and
  consumers remain available for the catalogued transition window; retiring a
  version requires an explicitly approved compatibility plan.
- Every cross-service message is a CloudEvents 1.0 structured JSON document.
  Its CloudEvent `type` is exactly the complete NATS Subject. Consumers reject
  a mismatch between NATS Subject, `type` and `dataschema` before any business
  mutation.
- The CloudEvent `id` is LabWeaver's `event_id`. Every message also records
  the authoritative aggregate type and ID, a non-negative sequence that is
  strictly monotonic within that aggregate, occurrence time, correlation ID,
  causation ID and trace context. Ordering is guaranteed only per aggregate;
  no consumer may infer a global or cross-aggregate order.
- The state owner writes its business state change, idempotency record and
  domain-local Outbox row in one transaction. Only after commit may the Owner
  publish the public event. A Build Executor or Collector is a controlled
  worker of Agent or Evaluation respectively, not an independent service or
  state owner; it returns results to its Owner, which records state and emits
  the authoritative event.
- Commands use `COMMANDS` with WorkQueue retention and one declared handling
  purpose. Domain events use `EVENTS` with Limits retention and independent
  durable consumers for each declared purpose. `AUDIT` receives only the
  explicitly selected, sanitized audit events. Retention, replicas and
  storage are versioned environment configuration, not hard-coded by this
  contract. Commands remain until acknowledged or quarantined.
- Consumers are durable pull consumers with explicit acknowledgement. Their
  deployment configuration declares `AckWait`, a finite backoff sequence and
  `MaxDeliver`. Duplicate delivery is expected. Consumers deduplicate by
  `event_id` and apply business idempotency using the catalogued aggregate and
  operation key; they reject stale sequences and surface a sequence gap rather
  than silently advancing state.
- A malformed CloudEvent, unsupported schema/version, authorization failure
  or exhausted retry budget must enter a controlled quarantine/DLQ record with
  the original identity, consumer, diagnostic, delivery count and trace
  context. It must trigger an operator-visible blocking alert. It must not be
  acknowledged as a successful business operation, discarded, or rewritten.

The authoritative v1 catalog and payload rules are in
[NATS Event Contract v1](../contracts/nats-event-contract-v1.md).

## Consequences

The first runtime implementation must provide an explicit manifest binding for
every Stream and durable Consumer. It must validate the CloudEvent envelope
before handler dispatch, preserve source error context in stable diagnostics,
and make quarantine inspection and replay an audited operator operation.

No message contains credentials, tokens, raw student submissions, unredacted
endpoint details or arbitrary log payloads. Payloads carry immutable object
references, hashes, bounded summaries and domain IDs instead. Audit retention
and course purge remain subject to the approved data lifecycle policy; this ADR
does not create an indefinite message archive.

## Alternatives rejected

| Alternative | Rejected because |
| --- | --- |
| Keep the draft Subject list without an envelope or Owner catalog | It cannot prevent conflicting producers, incompatible payloads or silent consumer behaviour. |
| Let workers publish authoritative completion events directly | It bypasses the Owner's transaction and Outbox boundary. |
| Exactly-once delivery | JetStream delivery is at least once; claiming otherwise would hide duplicate and replay handling requirements. |
| Global ordering | It couples unrelated aggregates, blocks horizontal scaling and is not needed for domain correctness. |
| A shared retry loop inside each handler | It makes delivery budget, observability and terminal failure semantics inconsistent. |
| Acknowledge malformed or exhausted messages after logging | It loses a required recovery path and violates fail-fast delivery semantics. |

## Compatibility, rollback and evidence

This is E0 documentation evidence. It changes no deployed Subject, Stream,
schema or persisted state. Before implementation is marked complete, an E2
suite using real PostgreSQL and JetStream must prove Outbox atomicity,
publish/retry, duplicate and replay idempotency, per-aggregate stale/gap
handling, durable-consumer recovery, acknowledgement behaviour and quarantine
delivery. A later revision replaces this ADR only through a superseding ADR
with a subject-version migration and compatibility plan.
