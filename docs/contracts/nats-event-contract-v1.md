# NATS Event Contract v1

## Status and scope

This is the human-readable, normative v1 catalog for cross-service NATS
messages. It implements the design decision in
[ADR 0003](../adr/0003-nats-subject-and-delivery-contract.md) and resolves
Issue #18 at E0 and is now backed by Issue #45 Rust payloads and generated
schemas at E1. Issue #51 adds local E2 evidence for the Environment lifecycle
command and Outbox subjects plus controlled Resource-Lease and Provider
request/reply using real NATS JetStream 2.11; it does not upgrade the rest of
the catalog, implement the Resource/Provider owners or prove a deployed NATS
identity.

All public Subjects use `labweaver.<owner>.<aggregate>.<event>.v1`. The Owner
is an existing business service, never a worker process or a Provider. A
message crosses service boundaries only after its Owner commits the state
change, idempotency record and local Outbox row together. A catalog entry may
be consumed only by the stated handling purpose; a future additional purpose
requires a catalog change and its own durable Consumer.

## CloudEvents envelope

Each message is a CloudEvents 1.0 structured JSON document with
`datacontenttype: application/json`.

| Attribute | Rule |
| --- | --- |
| `id` | The immutable `event_id`; unique for the published fact or command. |
| `source` | `urn:labweaver:<owner>` for the catalogued state Owner. |
| `specversion` | Exactly `1.0`. |
| `type` | Exactly the NATS Subject receiving the message. |
| `subject` | `<aggregate_type>/<aggregate_id>`; no local or user-specific path. |
| `time` | RFC 3339 UTC occurrence time recorded by the Owner. |
| `dataschema` | The exact generated `schemas/contracts/v1/events/<name>.schema.json` identity; it identifies the matching catalogued v1 payload, not a machine-local file. |
| `lwaggregatetype` / `lwaggregateid` | Must equal the aggregate represented by `subject`. |
| `lwsequence` | Non-negative, strictly increasing sequence within the aggregate. |
| `lwcorrelationid` / `lwcausationid` | Required correlation and immediate predecessor identity; the initial command uses its own `id` as causation ID. |
| `traceparent` | W3C Trace Context supplied or continued by the Owner. |
| `data` | Subject-specific payload containing only validated domain IDs, immutable references, hashes and bounded summaries. |

Consumers reject a missing required attribute, mismatched Subject/`type`/
`dataschema`, invalid aggregate identity, unsupported version, malformed
payload or invalid sequence before a business write. `id` is the canonical
deduplication key. A repeated ID is idempotently acknowledged only after the
consumer proves that its prior business effect is durable; a stale sequence or
sequence gap produces a stable blocking diagnostic and no speculative state
transition.

`data` must never contain credentials, tokens, raw submissions, full private
logs, direct environment endpoints or arbitrary executable text. It uses an
immutable object reference plus SHA-256 where an external artifact is needed.

## Stream and consumer policy

| Stream | Message class | Retention and handling |
| --- | --- | --- |
| `COMMANDS` | Rows marked Command in the catalog | WorkQueue. Exactly one declared handling purpose receives a command. It remains until successful acknowledgement or quarantine. |
| `EVENTS` | Rows marked Event | Limits. Each declared handling purpose has an independent durable pull Consumer. Events are facts, not a request to choose the first available Provider. |
| `AUDIT` | Explicitly selected sanitized audit copies | Limits with longer, environment-controlled retention. It is not a business state store and does not receive raw payloads. |
| controlled quarantine/DLQ | Terminal failures | Private deployment-controlled stream/locator. It retains identity, metadata, diagnostic and bounded safe payload evidence for audited repair or replay. |

Each durable Consumer explicitly configures `AckWait`, a finite backoff array
and `MaxDeliver`. A handler acknowledges only after its transaction commits.
Transport failure, a retryable dependency failure or a lease loss is retried
under that configuration. Parse, contract, authorization and unsupported
version failures are quarantined immediately. A retry-budget exhaustion is
quarantined after its last permitted delivery and emits an alert. No consumer
may log and skip, silently replace a message, or treat quarantine as success.

## Subject catalog

`sequence` is per row's aggregate, and every Event payload includes the
aggregate's resulting state/revision where applicable. Command payloads carry
the immutable input reference, requested operation and caller/approval context
needed by the handling Owner; they never carry untrusted executable text.

| Subject | Class / stream | State Owner → handling purpose | Aggregate and payload contract |
| --- | --- | --- | --- |
| `labweaver.control.lab_package.created.v1` | Event / EVENTS | Control → Agent planning | `lab_package`; immutable package/version references, hashes and approval state. |
| `labweaver.control.lab_release.approved.v1` | Event / EVENTS | Control → Environment, Evaluation | `lab_release`; approved release ID, version, immutable spec hashes and actor audit reference. |
| `labweaver.control.environment_template_release.published.v1` | Event / EVENTS + AUDIT | Control → Environment | `environment_template_release`; exact spec hash, approved release version and verified Container/VM artifact identity. This is the only new Subject added by Issue #45. |
| `labweaver.control.course.closed.v1` | Event / EVENTS + AUDIT | Control → Environment, Evaluation, Resource purge planning | `course`; closure revision and approved purge policy reference; no user data. |
| `labweaver.agent.run.requested.v1` | Command / COMMANDS | Agent → Agent run executor | `agent_run`; run type, validated immutable inputs, approval reference and idempotency key. |
| `labweaver.agent.run.completed.v1` | Event / EVENTS + AUDIT | Agent → Control review | `agent_run`; result reference/hash, validation summary and terminal state. |
| `labweaver.agent.run.failed.v1` | Event / EVENTS + AUDIT | Agent → Control review | `agent_run`; stable diagnostic, safe failure summary and terminal state. |
| `labweaver.agent.build.requested.v1` | Command / COMMANDS | Agent → Agent Build Executor | `agent_build`; approved BuildRequest reference, fixed input/image digests and idempotency key. |
| `labweaver.agent.build.completed.v1` | Event / EVENTS + AUDIT | Agent → Control approval | `agent_build`; image digest, attestation/reference hashes and validation summary. |
| `labweaver.agent.build.failed.v1` | Event / EVENTS + AUDIT | Agent → Control review | `agent_build`; terminal diagnostic and bounded safe report reference. |
| `labweaver.access.grant.created.v1` | Event / EVENTS + AUDIT | Access → Access Gateway policy application | `access_grant`; grant revision, scoped endpoint IDs, expiry and policy reference. |
| `labweaver.access.grant.activated.v1` | Event / EVENTS + AUDIT | Access → Gateway observers | `access_grant`; activated revision, endpoint IDs and expiry. |
| `labweaver.access.grant.denied.v1` | Event / EVENTS + AUDIT | Access → audit observers | `access_grant`; terminal revision and stable diagnostic. |
| `labweaver.access.grant.expired.v1` | Event / EVENTS + AUDIT | Access → Gateway observers | `access_grant`; terminal revision, effective time and stable diagnostic. |
| `labweaver.access.grant.revoked.v1` | Event / EVENTS + AUDIT | Access → Access Gateway revocation | `access_grant`; revoked revision, reason code and effective time. |
| `labweaver.access.ssh_key.revoked.v1` | Event / EVENTS + AUDIT | Access → Gateway observers | `ssh_public_key`; key ID, actor ID, revision and effective time; no public-key body. |
| `labweaver.access.session.termination_requested.v1` | Event / EVENTS + AUDIT | Access → OpenSSH Gateway | `gateway_session`; session/key/grant/endpoint revisions and termination deadline. |
| `labweaver.access.session.closed.v1` | Event / EVENTS + AUDIT | Access → audit observers | `gateway_session`; close receipt revision, effective time and reason. |
| `labweaver.access.session.termination_overdue.v1` | Event / EVENTS + AUDIT | Access → release/audit observers | `gateway_session`; overdue revision, deadline and blocking diagnostic. |
| `labweaver.access.policy.publish.requested.v1` | Command / COMMANDS | Access → Access policy compiler | `policy_revision`; validated policy input reference, prior revision and approval context. |
| `labweaver.access.device.expired.v1` | Event / EVENTS + AUDIT | Access → Access cleanup | `device`; device ID, expiry revision and cleanup scope. |
| `labweaver.environment.instance.provision_requested.v1` | Command / COMMANDS | Environment → Environment reconciler | `environment_instance`; approved template/version, runtime binding and idempotency key. |
| `labweaver.environment.instance.ready.v1` | Event / EVENTS + AUDIT | Environment → Access, Evaluation | `environment_instance`; observed generation, endpoint IDs and immutable runtime identity. |
| `labweaver.environment.instance.failed.v1` | Event / EVENTS + AUDIT | Environment → Control, Access cleanup | `environment_instance`; stable diagnostic, observed generation and safe report reference. |
| `labweaver.environment.instance.delete_requested.v1` | Command / COMMANDS | Environment → Environment reconciler | `environment_instance`; deletion revision, cleanup policy and idempotency key. |
| `labweaver.environment.instance.operation_accepted.v1` | Event / EVENTS + AUDIT | Environment → lifecycle observers | `environment_instance`; accepted operation ID, revision, generation and state without Provider payload. |
| `labweaver.environment.instance.state_changed.v1` | Event / EVENTS + AUDIT | Environment → Access and lifecycle observers | `environment_instance`; resulting state, revision/generation and stable diagnostic without endpoint credentials. |
| `labweaver.environment.instance.lifecycle_requested.v1` | Command / COMMANDS | Environment command boundary → Environment lifecycle reconciler | `environment_instance`; revision-checked lifecycle command, bounded deadline/retry, revocation revision and idempotency key without Provider handles; create carries the complete versioned first-aggregate spec. |
| `labweaver.evaluation.submission.freeze_requested.v1` | Command / COMMANDS | Evaluation → Evaluation Collector | `submission`; approved SubmissionManifest reference, source identity and idempotency key. |
| `labweaver.evaluation.submission.frozen.v1` | Event / EVENTS + AUDIT | Evaluation → lifecycle observers | `submission`; complete FrozenSubmission with immutable object version/SHA-256 plus source identity; emitted atomically with the database contract. Sprint 2 does not schedule evaluation. |
| `labweaver.evaluation.run.requested.v1` | Command / COMMANDS | Evaluation → Evaluation scheduler | `evaluation_run`; immutable submission/spec/bundle identities, approved execution binding and idempotency key. |
| `labweaver.evaluation.step.ready.v1` | Event / EVENTS | Evaluation → Evaluation executor | `evaluation_step_run`; step revision, runner binding, immutable inputs and dependency facts. |
| `labweaver.evaluation.step.completed.v1` | Event / EVENTS + AUDIT | Evaluation → deterministic aggregator | `evaluation_step_run`; terminal verdict, bounded metrics and evidence references; no LLM numeric score. |
| `labweaver.evaluation.run.completed.v1` | Event / EVENTS + AUDIT | Evaluation → Control release review | `evaluation_run`; deterministic aggregate result, evidence hashes and terminal state. |
| `labweaver.resource.request.submitted.v1` | Event / EVENTS + AUDIT | Resource → Resource approval workflow | `resource_request`; scoped request, policy revision and actor reference. |
| `labweaver.resource.request.approved.v1` | Event / EVENTS + AUDIT | Resource → Environment capacity binding | `resource_request`; approval revision, bound capacity reference and expiry policy. |
| `labweaver.resource.lease.expired.v1` | Event / EVENTS + AUDIT | Resource → Environment cleanup | `resource_lease`; lease revision, effective time and cleanup scope. |

The catalog intentionally has no `labweaver.build.*` or
`labweaver.artifact.*` public domain. Build Executor and Collector are
controlled workers respectively owned by Agent and Evaluation; assigning them
their own public namespace would imply a service and authority boundary that
does not exist.

## Controlled request/reply boundaries

Provider and owner-service verification calls are not public domain events and
do not enter `EVENTS` or `AUDIT`. Deployments bind each call to one exact,
reviewed subject; wildcards and registry-order fallback are forbidden.

The Environment Service Resource-Lease verifier sends a version-1 request with
the exact Lease, Environment, course, owner and capacity identities. It accepts
only a version-1 `Active` response containing the same identities, an explicit
Lease revision, and an interval active at the Environment database time.
Timeout or Resource unavailability is retryable. Missing, inactive, expired or
mismatched authorization is terminal, is quarantined with bounded safe evidence
and causes no aggregate or Provider mutation. The concrete Resource Service
responder remains a separate owner implementation.

Each Environment Provider request carries
`(operationId, providerStep, action)`; the response must echo the operation and
step. A Provider uses this tuple as its idempotency identity. Environment
persists and advances the step only with the corresponding lifecycle state so
adjacent phases cannot alias even when their action enum is equal.

## Required runtime evidence

An implementation may not claim this contract is live based on type existence,
a mock or a generated document. The first E2 implementation must prove with
real PostgreSQL and JetStream that Outbox commit/publish failure has no partial
business result, valid messages publish and consume, duplicates/replays are
idempotent, stale and gap sequences block state mutation, Consumers recover
after restart, acknowledgement occurs only after durable mutation, and invalid
or exhausted messages reach quarantine with a stable diagnostic and alert.
