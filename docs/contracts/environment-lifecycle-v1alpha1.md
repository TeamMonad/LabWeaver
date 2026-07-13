# EnvironmentLifecycle v1alpha1 Contract

Status: proposed design contract; no API, event, persistence or provider implementation exists.

`EnvironmentLifecycle v1alpha1` defines the authoritative vocabulary and
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

The future Environment Service record must contain, at minimum:

| Field | Semantics |
| --- | --- |
| `environmentId` | Stable instance identifier. |
| `class` / `runtimeKind` | Independent, immutable-at-creation dimensions. |
| `templateVersion` / `providerBinding` | Immutable, versioned source and explicit provider binding. |
| `desiredState` / `observedState` | Requested lifecycle intent and reconciled platform state. |
| `revision` | Monotonic optimistic-concurrency revision. |
| `leaseRef` | Required only for Work; a controlled reference to the Resource-owned Lease. |
| `endpointRefs` | Environment-owned endpoint metadata; never access credentials. |
| `lastDiagnostic` / `failedPhase` | Stable failure code and the phase eligible for explicit retry. |
| `operationRef` | Accepted operation identity, idempotency binding and terminal result. |
| `cleanupEvidence` | Sanitized provider cleanup result, retention decision and audit correlation. |

Environment Service alone writes this record. Resource Service owns Lease state;
Access Service owns grants and revocation; Provider APIs only supply observed
facts. Cross-service references are versioned and may not be replaced by direct
table writes.

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
| `Ready` | `Stopped`, `Updating`, `Expiring`, `Deleting`, `Failed` | explicit stop; approved Work configuration; Lease/course expiry; delete; unhealthy provider. |
| `Stopped` | `Provisioning`, `Expiring`, `Deleting`, `Failed` | authorized start; expiry; delete; provider inconsistency. |
| `Updating` | `Ready`, `Failed`, `Deleting` | verified configuration run; any configuration failure; delete after cancellation is recorded. |
| `Failed` | `Validating`, `Provisioning`, `Expiring`, `Deleting` | explicit retry from recorded failed phase; authorized reset; expiry; delete. |
| `Expiring` | `Stopped`, `Deleting`, `Failed` | grants revoked and workload stopped; retention requires cleanup; revocation/stop failure. |
| `Deleting` | `Deleted`, `Failed` | cleanup evidence complete; cleanup failure. |
| `Deleted` | none | terminal immutable tombstone. |

Every other transition is rejected with a stable lifecycle diagnostic and
creates no provider side effect. A provider's raw state never advances the
domain state by itself; the Environment Service maps it through the current
operation, revision and reconciliation rules.

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

The following future endpoints are part of the v1alpha1 interface contract:

```http
POST   /api/v1/environments
GET    /api/v1/environments/{id}
POST   /api/v1/environments/{id}:start
POST   /api/v1/environments/{id}:stop
POST   /api/v1/environments/{id}:reset
POST   /api/v1/environments/{id}:retry
DELETE /api/v1/environments/{id}
GET    /api/v1/environments/{id}/endpoints
POST   /api/v1/environments/{id}/configuration-requests
```

Every mutating lifecycle request requires an `Idempotency-Key` and an
`expectedRevision`. A valid, newly accepted request returns `202 Accepted` with
an `operationRef`, target desired state and accepted revision. Reuse of the
same idempotency key with a different payload is rejected; a stale expected
revision is rejected; duplicate equivalent delivery returns the original
operation rather than creating a second provider action. `GET` returns both
desired and observed state, current revision, current/last operation, stable
diagnostic and sanitized audit references.

`retry` is authorized only from `Failed`; it resumes the recorded failed phase
with the same immutable bindings. `reset` is a separately authorized recovery
operation with the class-specific target above. No command may use an implicit
provider, template, Lease, endpoint or fallback target.

## Lease, endpoint and access ordering

| Condition | Required behavior |
| --- | --- |
| Work create/start | Environment Service verifies referenced Lease is Active before accepting the command. |
| New or renewed grant | Access Service issues only when Environment reports observed `Ready`, endpoint registration and endpoint health for the same revision. |
| Lease expiry/revocation, environment failure, expiry or delete | Access Service revokes relevant grants first; Environment waits for the recorded revocation disposition before stop or cleanup. Failure is fail closed for new access. |
| Endpoint unhealthy or absent | No new/renewed grant; Environment records diagnosis and reconciles or fails according to the lifecycle rules. |
| `Deleted` | Endpoint metadata and grants are no longer usable; tombstone retains only sanitized cleanup/audit evidence. |

The required future event family is versioned and idempotent: lifecycle command
acceptance and observed transitions belong to Environment; Lease state changes
belong to Resource; grant issuance/revocation belongs to Access. Event payloads
must carry event ID, environment ID, revision, operation ID, actor/correlation
identifiers and stable diagnostic code without secrets or raw provider handles.

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

## Planned verification

This contract has E0 design evidence only. A future implementation must add
contract and integration coverage for normal transitions, duplicate commands,
idempotency-key payload conflicts, revision conflicts, illegal transitions,
provider failure and retry exhaustion, class invariants, Lease expiry,
revocation ordering, unhealthy endpoints, configuration failure, deletion
idempotency and tombstone evidence.
