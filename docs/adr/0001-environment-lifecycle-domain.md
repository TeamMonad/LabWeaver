# ADR 0001: Environment Lifecycle Domain

Status: proposed; requires human contract and security review.

## Context

LabWeaver supports two independent dimensions for environments: business class
(`Experiment` or `Work`) and runtime (`Container` or `VirtualMachine`). The design
baseline describes both dimensions and a lifecycle, but it does not define a
cross-service authority boundary, desired-versus-observed state model, command
concurrency contract, or terminal deletion evidence. Without those decisions,
the Environment, Resource and Access services can make incompatible lifecycle
decisions and leave an expired or failed environment reachable.

This ADR resolves GitHub Issue #16 at the design level only. It neither creates
providers nor authorizes a service, database schema, CRD, REST handler, event
publisher or access path.

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
- Only an observed `Ready` environment with healthy registered endpoints is
  eligible for a new or renewed access grant. Lease expiry, failure, expiry and
  deletion revoke grants before the environment is stopped or cleaned up.
- An Experiment can be created from a published immutable release without a
  Work Lease and can reset only to that release baseline. A Work environment
  requires an Active Lease to create or start; its reset target is an explicitly
  selected, authorized snapshot or configuration revision. Reset is legal only
  from `Ready`, `Stopped` or `Failed`; a reset from `Ready` revokes grants before
  restore, while a reset from `Stopped` preserves stopped intent.
- Work configuration runs are serialized through `Updating`. Any configuration
  failure transitions the instance to `Failed`; it never silently resumes as
  `Ready`.
- Provider reconciliation has bounded, observable retries. Exhaustion leaves
  the instance in `Failed` with a stable diagnostic. Recovery requires an
  explicit authorized retry or reset operation.
- Deletion cleans external resources and data according to the applicable
  retention policy, then leaves an immutable `Deleted` audit tombstone. A
  tombstone is not recoverable.

The complete vocabulary, transition rules and interface requirements are in
[`EnvironmentLifecycle v1alpha1`](../contracts/environment-lifecycle-v1alpha1.md).

## Alternatives considered

| Alternative | Rejected because |
| --- | --- |
| Separate full state machines for Experiment and Work | Duplicates common provider, access and audit semantics and makes cross-class tooling inconsistent. |
| A single status field with no desired/observed split | Cannot distinguish an accepted command from provider convergence or diagnose reconciliation failures. |
| Environment Service owns Work Lease or grants | Violates the existing service authority boundary and makes revocation ambiguous. |
| Automatic unbounded recovery | Can hide persistent configuration, policy or capacity failures and accidentally restore access. |
| Hard-delete completed environment records | Removes the evidence needed to prove grant revocation and cleanup. |

## Consequences

Future Environment Service implementation must persist revisioned desired and
observed state, operation identity, transition reason, provider binding and
cleanup evidence. Resource and Access integrations must use versioned events or
controlled APIs; no service may mutate another service's tables. The public
contract requires a future compatibility review before any wire implementation.

The first implementation must provide idempotency, optimistic concurrency,
retry limits, structured lifecycle audit events and fail-closed access checks.
It must not report this ADR as runtime evidence.

## Security, data and rollback

State changes that remove eligibility for access must revoke grants before
provider stop/delete actions. Provider failures and unhealthy endpoints deny
new access. Reset and deletion must validate immutable template, snapshot and
retention bindings; raw provider handles, credentials and submitted data do not
belong in normal logs or tombstone fields.

Replacing this decision requires a superseding ADR and a compatibility plan for
existing instances, operations, leases, grants and audit history. Until an
implementation exists, rollback is removal of this proposed design document and
its documentation references; no runtime state exists to migrate.

## Evidence

Current evidence level: E0. This ADR and its companion contract are design
artifacts pending human review. They do not prove a real provider, KubeVirt VM,
Lease, AccessGrant, endpoint or cleanup path.
