# ADR 0011: Sprint 2 Forward Simplification

Status: Accepted

Date: 2026-07-19

## Context

Sprint 1 and Sprint 2 established the six service boundaries, PostgreSQL domain
ownership, the Control/Access/Agent/Environment contracts, two runtime plans,
and the Web journeys. The repository also accumulated parallel v1/v2 events,
an undeployed executor protocol, a Private Sigstore trust plane, Kyverno image
admission, and per-capability evidence processes before the dual-runtime product
path was runnable. The additional mechanisms block the course release without
closing the teacher-to-student journey.

This decision changes the pre-1.0 baseline. Existing business data and the old
wire contracts are intentionally not migration targets. Git history remains the
record of the previous design.

## Decision

### Product boundary

Sprint 2 ships Control, Access, Agent, Environment, Web, two owner-scoped
executor workers, and the OpenSSH Access Gateway. Evaluation and Resource keep
their service and schema ownership but are disabled in the Sprint 2 deployment.
Evaluation execution, scoring, WorkConfig, resource approval, Tailnet,
Guacamole, and additional Agent runtimes are out of scope.

The required journey is:

```text
ProblemPackage -> Claude Code AgentRun -> independent candidate approval
-> BuildKit -> Harbor -> Trivy -> immutable digest
-> Container or KubeVirt Environment -> AccessGrant
-> FrozenSubmission -> cleanup
```

### Contracts and persistence

- The pre-release public contract has one current `v1` representation.
- NATS is restricted to Agent, build, Environment lifecycle, Access expiry or
  revocation, and Submission freeze work that crosses an asynchronous boundary.
- Ordinary reads, authorization decisions, and synchronous validation use the
  owner service API.
- The public release model is `Draft -> PublishedRelease -> Run`. Candidate,
  validation, and approval records are children of the Draft rather than
  parallel public release lifecycles.
- A clean deployment applies one Sprint 2 baseline migration per domain that
  owns persisted state. No old database or event upgrade path is promised.

### Supply chain

Private Sigstore, Fulcio, Rekor, CT log, TUF, Kyverno, Packer, SBOM,
provenance, and attestation are removed from the current product contract and
deployment. A runnable image is authorized by all of the following:

1. it belongs to the configured private Harbor project;
2. it is referenced by an immutable `sha256` digest;
3. its Trivy report is bound to that digest and scanner database;
4. Critical findings fail the release, while High findings remain visible;
5. the teacher approved the corresponding release.

The absence of a signature or admission controller is not silently converted
into another trust claim.

### Executors

The existing NATS request/reply boundary remains because Kubernetes, KubeVirt,
BuildKit, Harbor, and Trivy credentials must not be held by the API processes.
It is completed by two deployments built from existing service images:

- `build-executor`, owned by Agent Service;
- separate `container-executor` and `kubevirt-executor` processes from the
  Environment Service image, each with its own ServiceAccount and RBAC.

They have separate identities and least-privilege credentials. They preserve
the existing generation, deadline, replay, cancellation, and cleanup fences.
No generic plugin loader, runtime discovery, provider ordering, or fallback is
introduced.

### Evidence and release

Unit, contract, integration, browser, deployment, and release reports remain.
E0-E4 labels are not maintained for every capability or Issue. The Release Gate
records only the evidence needed for its decision and binds it to one commit,
deployment manifest, migration catalog, image set, and run ID.

## Consequences

- Existing Sigstore and Kyverno code, deployment automation, schemas, and
  reports are deleted in a forward change.
- The adopted cluster is reset for LabWeaver state. Kubernetes, KubeVirt,
  PostgreSQL, NATS, MinIO, Harbor, and Keycloak services remain, while their
  LabWeaver-owned state is recreated.
- Kyverno itself is removed only after read-only inventory proves there are no
  non-LabWeaver consumers. Otherwise deployment stops with a blocking report.
- The Sprint 2 Release Gate requires both Container and real KubeVirt paths.
- ADR 0006 is superseded for signing, transparency, attestation, and admission
  policy. Its digest, scanning, and immutable publication rationale survives in
  this ADR.

## Rollback

Before deployment, rollback is a normal revert of this branch. After the
approved reset starts, old business data, signing data, and old wire versions
are not recoverable. Rollback redeploys the last verified application images
against a newly initialized database; it does not recreate the removed trust
plane.
