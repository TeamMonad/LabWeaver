# EnvironmentLifecycle v1 Contract

Status: local E2 implementation in the current worktree; pending A review, D Verify, CI and merge.

`EnvironmentLifecycle v1` defines the authoritative vocabulary and
cross-service rules for an environment instance. It applies to the Cartesian
product of business class and runtime:

| Business class | Runtime |
| --- | --- |
| `Experiment` | `Container` or `VirtualMachine` |
| `Work` | `Container` or `VirtualMachine` |

An implementation must bind one explicit provider and immutable template
version. It must not infer a provider from registration order or silently fall
back to another runtime.

## Instance record and authority

The Environment Service record contains, at minimum:

| Field | Semantics |
| --- | --- |
| `environmentId` | Stable instance identifier. |
| `class` / `runtimeKind` | Independent, immutable-at-creation dimensions. |
| `templateVersion` / `providerBinding` | Immutable, versioned source and explicit provider binding. |
| `desiredState` / `observedState` | Requested lifecycle intent and reconciled platform state. |
| `revision` | Monotonic optimistic-concurrency revision. |
| `leaseRef` | Required only for Work; a controlled reference to the Resource-owned Lease. |
| `capacityBinding` | Required only for Work; exact Resource-approved capacity identity paired with `leaseRef`. |
| `eligibilityExpiresAt` | Earliest authoritative time after which owner resolution and new access fail closed. |
| `endpointRefs` | Environment-owned endpoint metadata; never access credentials. |
| `lastDiagnostic` / `failedPhase` | Stable failure code and the phase eligible for explicit retry. |
| `operationRef` | Accepted operation identity, bounded retry/deadline, idempotency binding and terminal result. |
| `cleanupEvidence` | Sanitized provider cleanup result, retention decision and audit correlation. |

Environment Service alone writes this record. Resource Service owns Lease state;
Access Service owns grants and revocation; Provider APIs only supply observed
facts. Cross-service references are versioned and may not be replaced by direct
table writes.

## Current implementation boundary

`services/environment-service` now implements revision-checked command planning,
transactional SQLx persistence, idempotency replay/conflict handling, full
CloudEvent Outbox insertion and bounded replay dispatch, transactional Inbox
ordering with the lifecycle mutation, recoverable `FOR UPDATE SKIP LOCKED`
reconciler leases, unique per-claim fencing tokens, bounded retry/timeout,
explicit Provider binding with no fallback, expiry selection, cleanup
operations and payload-free stable diagnostics. The production process binds
an existing durable JetStream command consumer, runs reconcile/expiry/Outbox
loops, and invokes exact provider bindings through versioned NATS request/reply.
The versioned create command carries the complete first-aggregate input. A
single database transaction owns Inbox sequence acceptance, create idempotency,
the initial aggregate and operation, and its Outbox fact; production does not
require a pre-seeded aggregate. Subsequent commands use the same Inbox and
lifecycle transaction boundary. Outbox publication waits for a JetStream
persistence acknowledgement and supplies the event ID as the NATS deduplication
identity.

For Work create, start, retry, recover and reset, the consumer calls the exact
configured Resource request/reply subject before the Environment transaction.
Only a version-1 `Active` response whose Lease, Environment, course, owner and
capacity identities exactly match is accepted. The accepted operation retains
the Resource authorization revision and interval. Resource outage is retryable;
missing, inactive, expired or mismatched authorization is terminal and creates
no Provider side effect. Eligibility and Lease intervals are evaluated against
the Environment database clock rather than a process-local clock.

Every Provider call carries the durable `(operationId, providerStep, action)`
identity and requires the response to echo `operationId` and `providerStep`.
`providerStep` advances only after a successful durable transition, so two
adjacent lifecycle phases that use the same action cannot collapse into one
Provider idempotency key. Failed operations persist their exact `failedPhase`;
retry/recover resume that phase, while reset persists the class-specific
immutable target and its authorization revision.

A timed-out operation enters a separately bounded cleanup phase, retains the
original timeout diagnostic, and finishes as failed even after cleanup evidence
is durably recorded. Existing endpoints require an Access revocation revision
before destructive or timeout cleanup. A superseding destructive command cannot
begin Provider cleanup until the older Provider call's fenced lease has
expired. The destructive Sprint 2 reset applies
`environment/0001_sprint2_baseline.sql`, which creates the durable
retry/deadline/reconcile-lease, Provider-step, capacity-binding and failed-phase
columns together with the due-operation index. Pre-reset development rows are
not upgraded or inferred; the reset removes them before this baseline is
applied.

The internal mTLS owner-resolver contract returns only `environmentId`,
`courseId`, `ownerActorId`, authoritative revision and earliest expiry. Its
policy rejects an unregistered caller SAN, missing/deleting/expired/non-Ready
environment, unhealthy or missing endpoint, and course/owner/revision mismatch
with 403. The success response carries a strong revision ETag and evaluates
expiry using the database time returned with the same owner-resolution query,
so host clock skew cannot extend authorization. Database or resolver outage is
retryable 503. The production TLS
acceptor requires a CA-verified client certificate, extracts DNS SANs from its
leaf certificate and injects the verified identity; it never trusts an HTTP
header for caller identity. Process startup loads only absolute certificate/key
locators, an explicit SAN allowlist and the Environment database, and fails
before binding the internal route when any prerequisite is absent or invalid.
The acceptor bounds pre-authentication concurrency, handshake duration and HTTP
connection duration, continuously reaps tasks, drains on shutdown, and handles
both SIGINT and SIGTERM. Signal registration or wait failure is propagated as a
typed process error and cannot be reported as a clean shutdown. Dependency-aware
readiness returns 503 when the database, NATS connection or required
expiry-revocation path is unavailable.

This slice supplies the production transport adapters but not a concrete
Container/KubeVirt Provider or the Access-owned revocation responder. Those
remain explicit adjacent/E3 dependencies rather than implicit fallback paths.

### Required production configuration

Startup is fail-fast. Paths below must be absolute Secret mounts; values and
credential contents are never logged.

| Variable | Meaning |
| --- | --- |
| `LABWEAVER_DATABASE_URL` | Environment-owned PostgreSQL connection. |
| `LABWEAVER_NATS_SERVER` | TLS-enabled NATS server authority. |
| `LABWEAVER_NATS_CA_PATH` | Private NATS CA bundle. |
| `LABWEAVER_NATS_CLIENT_CERT_PATH` / `LABWEAVER_NATS_CLIENT_KEY_PATH` | Environment Service NATS mTLS identity. |
| `LABWEAVER_NATS_CREDENTIALS_PATH` | NATS credentials file mounted from a Secret. |
| `LABWEAVER_ENVIRONMENT_COMMAND_STREAM` / `LABWEAVER_ENVIRONMENT_COMMAND_CONSUMER` | Existing deployment-owned COMMANDS stream and durable consumer. |
| `LABWEAVER_ENVIRONMENT_COMMAND_QUARANTINE_SUBJECT` | Private controlled quarantine subject; terminal invalid deliveries are acknowledged only after sanitized identity/hash evidence is persisted there. |
| `LABWEAVER_ENVIRONMENT_RELEASE_STREAM` / `LABWEAVER_ENVIRONMENT_RELEASE_CONSUMER` | Existing deployment-owned stream and durable consumer configured with exact multi-subject filters for release published v2 and withdrawn v1. |
| `LABWEAVER_ENVIRONMENT_RELEASE_QUARANTINE_SUBJECT` | Private controlled quarantine subject for invalid v2 release projections. |
| `LABWEAVER_RESOURCE_LEASE_VERIFICATION_SUBJECT` | Exact Resource-owned versioned request/reply subject used to verify Work Lease state and scope before command acceptance. |
| `LABWEAVER_ENVIRONMENT_PROVIDER_BINDINGS_PATH` | Reviewed JSON array of exact `{ "binding", "subject" }` provider mappings; empty, duplicate or wildcard mappings fail startup. |
| `LABWEAVER_ACCESS_REVOCATION_SUBJECT` | Exact Access revocation request/reply subject used before automatic expiry cleanup. |
| `LABWEAVER_ENVIRONMENT_WORKER_ID` / `LABWEAVER_ENVIRONMENT_SYSTEM_ACTOR_ID` | Portable worker identity and audited UUIDv7 system actor. |
| `LABWEAVER_OWNER_RESOLVER_BIND_ADDR` | Internal owner-resolver listener. |
| `LABWEAVER_OWNER_RESOLVER_CLIENT_CA_PATH` | CA bundle for caller certificate verification. |
| `LABWEAVER_OWNER_RESOLVER_SERVER_CERT_PATH` / `LABWEAVER_OWNER_RESOLVER_SERVER_KEY_PATH` | Resolver server identity. |
| `LABWEAVER_OWNER_RESOLVER_ALLOWED_CALLER_SANS` | Comma-separated exact DNS SAN allowlist; wildcards are rejected. |

The provider binding file contains exact identities and provider-specific
non-secret routing configuration. The formal Container Provider uses:

```json
[
  {
    "binding": "container-primary-v1",
    "subject": "labweaver.provider.kubernetes.container.v1",
    "providerKind": "container",
    "gatewayNamespace": "access-system",
    "gatewayName": "protected-gateway",
    "gatewaySection": "protected-https",
    "imagePullSecretName": "harbor-course-pull",
    "activeImagePolicyId": "01900000-0000-7000-8000-000000000001",
    "activeImagePolicyRevision": 1,
    "activeTrustRevision": 1,
    "activeTrustBundleSha256": "d58414fc98a5de1ad8c269290835b407ff258b3f567dab3399fbc2911454a981"
  }
]
```

The complete example is `deploy/config/environment-providers.json.example`.
Omitting `providerKind` selects the existing remote provider; remote entries
must not contain Gateway, image-pull or trust-policy fields. Container entries
require every Gateway field, the exact same-namespace Harbor pull Secret name,
and the active image-policy ID/revision, trust revision and trust-bundle SHA-256. They use
the immutable publication plus append-only withdrawal projection described in
`docs/contracts/container-supply-chain-v1.md`.

## Desired and observed state

`desiredState` is one of `Running`, `Stopped` or `Deleted`. The service accepts
a command only after validating its current revision and applicable class,
approval, template and Lease constraints. Acceptance is not completion.

`observedState` is one of:

```text
Requested → Validating → Building → Provisioning → Ready ↔ Stopped
                                  ↘ Updating
Ready / Stopped / Updating / Expiring / Deleting / lifecycle phase → Failed
Ready / Stopped / Failed → Expiring → Deleting → Deleted
```

`Building` prepares the explicitly bound immutable runtime asset; `Provisioning`
creates, starts or restores provider resources and registers endpoint metadata.
`Ready` requires successful provider observation and endpoint health. `Stopped`
has no active user workload. `Updating` applies a Work configuration revision.
`Expiring` is the revocation-and-quiesce phase. `Deleting` is external cleanup.
`Deleted` is an immutable audit tombstone.

The diagram is an allowed transition set, not permission to skip validation.
The normative transitions are:

| From | Allowed next state | Trigger and guard |
| --- | --- | --- |
| `Requested` | `Validating`, `Failed`, `Deleting` | create acceptance; validation failure; authorized cancellation. |
| `Validating` | `Building`, `Failed`, `Deleting` | validated immutable input; blocking diagnostic; cancellation. |
| `Building` | `Provisioning`, `Failed`, `Deleting` | bound asset produced and verified; failure; cancellation. |
| `Provisioning` | `Ready`, `Stopped`, `Failed`, `Deleting` | health observed; desired stop; bounded retries exhausted; cancellation. |
| `Ready` | `Provisioning`, `Stopping`, `Updating`, `Expiring`, `Deleting`, `Failed` | authorized reset after grant revocation and workload quiescence; explicit stop; approved Work configuration; Lease/course expiry; delete; unhealthy provider. |
| `Stopped` | `Provisioning`, `Expiring`, `Deleting`, `Failed` | authorized start or reset; expiry; delete; provider inconsistency. |
| `Updating` | `Ready`, `Failed`, `Deleting` | verified configuration run; any configuration failure; delete after cancellation is recorded. |
| `Failed` | `Validating`, `Building`, `Provisioning`, `Stopping`, `Updating`, `Expiring`, `Deleting` | explicit retry/recover from the recorded failed phase; authorized reset; expiry; delete. |
| `Expiring` | `Stopped`, `Deleting`, `Failed` | grants revoked and workload stopped; retention requires cleanup; revocation/stop failure. |
| `Deleting` | `Deleted`, `Failed` | cleanup evidence complete; cleanup failure. |
| `Deleted` | none | terminal immutable tombstone. |

Every other transition is rejected with a stable lifecycle diagnostic and
creates no provider side effect. A provider's raw state never advances the
domain state by itself; the Environment Service maps it through the current
operation, revision and reconciliation rules.

### Reset transition rules

`reset` is legal only from observed `Ready`, `Stopped` or `Failed`. It is not a
generic escape hatch: `Requested`, `Validating`, `Building`, `Provisioning`,
`Updating`, `Expiring`, `Deleting` and `Deleted` reject it with a stable
diagnostic and no provider side effect.

| Source state | Required sequence | Completion target |
| --- | --- | --- |
| `Ready` | Revoke all eligible endpoint grants, quiesce the old workload, then enter `Provisioning` with the class-specific reset target. | `Ready` after provider observation and endpoint health succeed. |
| `Stopped` | Enter `Provisioning` with the class-specific reset target; no grant may be issued during restore. | `Stopped` after restore validation, preserving the prior stopped intent. |
| `Failed` | Enter `Validating`, then `Building` and `Provisioning`; revalidate the immutable reset target instead of reusing the failed provider state. | `Ready` or `Stopped`, according to the persisted desired intent. |

Reset preserves the valid persisted desired intent rather than accepting an
implicit new target. A reset from `Ready` therefore converges to `Ready`; a
reset from `Stopped` converges to `Stopped`. The class-specific reset target is
the Experiment's published immutable baseline or the Work environment's
explicit authorized snapshot/configuration revision.

## Class invariants

### Experiment

- Creation requires a published immutable release/template version and
  course-level authorization, not a Work Lease.
- The participant cannot mutate the baseline. `reset` restores exactly the
  published baseline and its pinned artifact identity.
- Configuration requests that would alter the published baseline are rejected.
- Course closure or retention expiry follows `Expiring → Deleting → Deleted`.

### Work

- Creation and `start` require a Resource Service Lease that is `Active` for
  the requested class, capacity and interval. Lease expiry or revocation makes
  the environment ineligible to start or remain accessible.
- Each approved configuration request is a serialized, revisioned operation.
  It alone enters `Updating`; concurrent configuration and lifecycle commands
  are rejected, except that deletion records cancellation before proceeding.
- A configuration failure always enters `Failed`, revokes grants and requires
  explicit retry, reset or deletion. It must not silently report the prior
  workload as ready.
- `reset` requires an explicit authorized snapshot or configuration revision;
  it cannot be interpreted as an Experiment-baseline reset.

## Lifecycle operations

The following endpoints are part of the v1 interface contract. Issue #45 established the
lifecycle surface and Issue #81 closes inventory, operation and AccessGrant discovery; neither
contract delivery by itself proves runtime handlers:

```http
POST   /api/v1/environments
GET    /api/v1/environments?courseId={courseId}&projectId={projectId}
GET    /api/v1/environments/{id}
POST   /api/v1/environments/{id}/start
POST   /api/v1/environments/{id}/stop
POST   /api/v1/environments/{id}/reset
POST   /api/v1/environments/{id}/retry
DELETE /api/v1/environments/{id}
GET    /api/v1/environments/{id}/endpoints
GET    /api/v1/environments/{id}/operations
GET    /api/v1/environments/{id}/operations/{operationId}
GET    /api/v1/environments/{id}/access-grants
POST   /api/v1/environments/{id}/configuration-requests
```

Every mutating lifecycle request requires an `Idempotency-Key` and an
`expectedRevision`. A valid, newly accepted request returns `202 Accepted` with
an `environmentId`, `operationId`, status URL and accepted revision. Reuse of the
same idempotency key with a different payload is rejected; a stale expected
revision is rejected; duplicate equivalent delivery returns the original
operation rather than creating a second provider action. `GET` returns both
desired and observed state, current revision, current/last operation, stable
diagnostic and sanitized audit references.

Inventory requires an explicit authorized `courseId`; `projectId` only narrows that scope. Its
owner projection is relation-safe and omits globally enumerable actor identity. Inventory,
operation history and AccessGrant discovery use bounded opaque cursors tied to one
`snapshotSequence`. Malformed cursors return 400 and expired cursors return 410; clients then take a
fresh REST snapshot and resume the course event stream from its `StreamSequence`. Operation polling
exposes timeout and cleanup deadlines rather than requiring clients to infer them from logs.

`retry` is authorized only from `Failed`; it resumes the recorded failed phase
with the same immutable bindings. `reset` is authorized only from `Ready`,
`Stopped` or `Failed` and follows the reset transition rules above. No command
may use an implicit provider, template, Lease, endpoint or fallback target.

## Lease, endpoint and access ordering

| Condition | Required behavior |
| --- | --- |
| Work create/start/retry/recover/reset | Environment Service verifies a versioned Resource response is `Active`, exact-scope and unexpired using the database clock before accepting the command. |
| New or renewed grant | Access Service issues only when Environment reports observed `Ready`, endpoint registration and endpoint health for the same revision. |
| Lease expiry/revocation, environment failure, reset, expiry or delete | Access Service revokes relevant grants first; Environment waits for the recorded revocation disposition before stop, restore or cleanup. Failure is fail closed for new access. |
| Endpoint unhealthy or absent | No new/renewed grant; Environment records diagnosis and reconciles or fails according to the lifecycle rules. |
| `Deleted` | Endpoint metadata and grants are no longer usable; tombstone retains only sanitized cleanup/audit evidence. |

The event family is versioned and idempotent: lifecycle command
acceptance and observed transitions belong to Environment; Lease state changes
belong to Resource; grant issuance/revocation belongs to Access. Event payloads
must carry event ID, environment ID, revision, operation ID, actor/correlation
identifiers and stable diagnostic code without secrets or raw provider handles. The Public SSE
projection additionally carries course/project scope, event identity, effective time and a
stream-level cursor distinct from the aggregate sequence. Runtime publication and replay evidence
remain future E2 work.

## Failure, retry and audit requirements

Provider, capacity, policy, template, endpoint-health and revocation failures
must produce a stable blocking diagnostic, structured lifecycle event and audit
record. Reconciliation retries are bounded and record attempt count, backoff
policy identity and final source diagnostic. Exhaustion transitions to `Failed`;
it is never converted into `Ready` by a background fallback.

Deletion is idempotent. After provider resources and data are handled according
to the versioned retention policy, `Deleted` preserves the instance identity,
class/runtime, template version, reason, timestamps, authorization references,
revocation disposition and cleanup evidence. It omits secrets, user content,
credentials and raw provider payloads.

## Verification

The current local E2 suite uses Docker PostgreSQL 17, NATS JetStream 2.11 and a
real rustls mTLS server. It exhaustively checks all 144 observed-state pairs and
all 144 state/operation pairs. Integration coverage includes populated-v1
migration; strict initial-create invariants; complete command payload identity;
production first-aggregate creation with atomic Inbox/idempotency/operation/
CloudEvent Outbox persistence; real JetStream publish acknowledgement and
durable command consumption; exact Active Resource Lease verification and
expired-Lease rejection without an aggregate or Provider side effect;
transactional Inbox duplicate/stale/gap blocking; optimistic conflicts; lease
fencing; and recovery by a new worker after a provider side effect but before
state save, with the same `(operationId, providerStep, action)` identity
preventing a duplicate side effect across four create phases.

Persistent worker tests cover timeout through successful cleanup while retaining
the root diagnostic, Ready cancellation through a non-Ready Deleted tombstone,
cleanup failure, expiry selection and the normal `Expiring → Stopped → Deleting`
path. Resolver coverage includes owner/course/revision decisions,
deletion/expiry, CA-verified SAN allowlisting, client/server certificate
rotation, bounded slow handshakes, strong ETag emission, database-clock expiry
under simulated host-clock skew, typed shutdown-failure propagation, database
outage as retryable 503 and resolver network outage.

Human A review and D Verify remain required before Issue #51 is accepted. Issue
#52 now supplies a formal local Container Provider with deterministic protected
Gateway resources and cleanup evidence. Issue #53 adds a formal local KubeVirt
Provider with an immutable VM/CDI plan, operation-generation fencing, guest and
SSH readiness, durable VM/disk/host-key identity and namespace deletion
tombstones. Its contract is `docs/contracts/kubevirt-runtime-provider-v1.md` and
its decision record is ADR 0009. Connected E3 verification must still cover
both deployment-owned Kubernetes executors, a real KubeVirt/CDI guest and SSH
handshake, Resource- and Access-owned responders, deployed mTLS NATS and the
Access consumer under the same build identity.
