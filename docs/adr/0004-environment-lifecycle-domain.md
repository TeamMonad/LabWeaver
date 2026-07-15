# ADR 0004: Environment Lifecycle Domain

Status: proposed; requires human contract and security review.

## Context

LabWeaver supports two independent dimensions for environments: business class
(`Experiment` or `Work`) and runtime (`Container` or `VirtualMachine`). The design
baseline describes both dimensions and a lifecycle, but it does not define a
cross-service authority boundary, desired-versus-observed state model, command
concurrency contract, or terminal deletion evidence. Without those decisions,
the Environment, Resource and Access services can make incompatible lifecycle
decisions and leave an expired or failed environment reachable.

This ADR originally resolved GitHub Issue #16 at the design level. Issue #51
now supplies a local E2 runtime core with Docker PostgreSQL, NATS JetStream and
real mTLS integration. It includes production command/reconcile/expiry/Outbox
wiring, an exact Resource Lease verifier, and exact remote Provider and
Access-revocation adapters, but does not provide a concrete
Container/KubeVirt Provider, Resource/Access-owned responder, CRD or deployed
access path.

## Decision

- `Experiment` / `Work` and `Container` / `VirtualMachine` are orthogonal. A
  runtime does not infer a business class, and a business class does not infer a
  runtime.
- Environment Service is authoritative for an environment instance, its
  lifecycle intent, desired state, observed state, endpoint metadata and
  provider reconciliation. Resource Service is authoritative for Work Lease
  validity. Access Service is authoritative for AccessGrant and EndpointGrant
  issuance and revocation.
- Lifecycle commands are asynchronous desired-state operations. They require an
  idempotency key and expected revision, persist an operation record, and return
  an operation reference rather than treating a provider request as completion.
- A create command carries the complete versioned first-aggregate input. Inbox
  ordering, create idempotency, the initial aggregate and operation, and its
  Outbox fact commit atomically; production never depends on test seeding.
- Only an observed `Ready` environment with healthy registered endpoints is
  eligible for a new or renewed access grant. Lease expiry, failure, expiry and
  deletion revoke grants before the environment is stopped or cleaned up.
- An Experiment can be created from a published immutable release without a
  Work Lease and can reset only to that release baseline. A Work environment
  requires an Active Lease to create or start; its reset target is an explicitly
  selected, authorized snapshot or configuration revision. Reset is legal only
  from `Ready`, `Stopped` or `Failed`; a reset from `Ready` revokes grants before
  restore, while a reset from `Stopped` preserves stopped intent.
- Work create, start, retry, recover and reset require an exact Active Resource
  Lease response evaluated with the Environment database clock. The accepted
  operation records the authorization snapshot; inactive, expired or
  mismatched responses fail closed before Provider execution.
- Work configuration runs are serialized through `Updating`. Any configuration
  failure transitions the instance to `Failed`; it never silently resumes as
  `Ready`.
- Provider reconciliation has bounded, observable retries. Exhaustion leaves
  the instance in `Failed` with a stable diagnostic. Recovery requires an
  explicit authorized retry or reset operation. Each call uses a durable
  `(operationId, providerStep, action)` identity, and retry/recover resumes the
  persisted failed phase rather than inferring one from a generic Failed state.
- Deletion cleans external resources and data according to the applicable
  retention policy, then leaves an immutable `Deleted` audit tombstone. A
  tombstone is not recoverable.

The complete vocabulary, transition rules and interface requirements are in
[`EnvironmentLifecycle v1`](../contracts/environment-lifecycle-v1.md).

## Alternatives considered

| Alternative | Rejected because |
| --- | --- |
| Separate full state machines for Experiment and Work | Duplicates common provider, access and audit semantics and makes cross-class tooling inconsistent. |
| A single status field with no desired/observed split | Cannot distinguish an accepted command from provider convergence or diagnose reconciliation failures. |
| Environment Service owns Work Lease or grants | Violates the existing service authority boundary and makes revocation ambiguous. |
| Automatic unbounded recovery | Can hide persistent configuration, policy or capacity failures and accidentally restore access. |
| Hard-delete completed environment records | Removes the evidence needed to prove grant revocation and cleanup. |

## Consequences

Environment Service implementations must persist revisioned desired and
observed state, operation and Provider-step identity, transition reason,
failed phase, reset target, Resource authorization, provider binding and
cleanup evidence. Resource and Access integrations use versioned events or
controlled APIs; no service may mutate another service's tables. Any future
wire change requires compatibility review.

The first implementation provides idempotency, optimistic concurrency, retry
limits, structured lifecycle events and fail-closed owner checks. This ADR is
design evidence; only the linked same-build integration results qualify as
runtime evidence.

## Security, data and rollback

State changes that remove eligibility for access must revoke grants before
provider stop/delete actions. Provider failures and unhealthy endpoints deny
new access. Reset and deletion must validate immutable template, snapshot and
retention bindings; raw provider handles, credentials and submitted data do not
belong in normal logs or tombstone fields.

Replacing this decision requires a superseding ADR and a compatibility plan for
existing instances, operations, leases, grants and audit history. Rollback of
Issue #51 is a forward Migration plus removal of the Environment process wiring;
the applied Migration and persisted runtime rows must not be silently deleted.

## Evidence

Current evidence level: local E2 for Rust types, validation, state transitions,
idempotent transactional repository code, real JetStream command/Outbox
delivery, new-worker reconciliation recovery, persistent timeout/cancel cleanup
and fail-closed owner resolution over a real rustls mTLS server backed by Docker
PostgreSQL 17. It also proves production first-aggregate creation, exact Active
Resource Lease verification/rejection, durable Provider-step identity,
failed-phase/reset-target persistence, database-clock owner expiry, strong ETag
emission and typed shutdown failure. The artifacts remain pending A review and
D Verify and do not prove a formal Provider, KubeVirt VM, Resource/Access-owned
responder or deployed E3 cleanup path.
