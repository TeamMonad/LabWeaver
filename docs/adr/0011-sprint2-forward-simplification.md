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

Sprint 2 ships Control, Access, Agent, Environment, Web, a freeze-only
Evaluation Service, two owner-scoped executor workers, and the OpenSSH Access
Gateway. Evaluation retains ownership of `FrozenSubmission` and consumes only
the Submission freeze lifecycle; it does not run EvaluationSpec steps, a
Runner, a Checker, an Aggregator, or scoring. Resource keeps its service and
schema ownership but is disabled in the Sprint 2 deployment. Evaluation
execution, scoring, WorkConfig, resource approval, Tailnet, Guacamole, and
additional Agent runtimes are out of scope.

The required journey is:

```text
ProblemPackage -> Claude Code AgentRun -> independent candidate approval
-> BuildKit -> Harbor -> Trivy -> immutable digest
-> Container or KubeVirt Environment -> AccessGrant
-> FrozenSubmission -> cleanup
```

Browser uploads use the portal HTTPS origin. The bucket path is routed to the
existing Web Nginx workload, which preserves the SigV4 `Host` and path and
proxies to the retained MinIO Service over verified TLS. The MinIO CA and the
allowlisted proxy location are mounted from a release-owned `ConfigMap`.
Control signs the public authority; Agent, build, runtime and freeze workers
continue to use the cluster-internal MinIO endpoint. This application-layer
bridge is required because the adopted Cilium Gateway controller does not
reconcile `BackendTLSPolicy`; it neither reconfigures nor replaces MinIO and
does not add another workload or service boundary.

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

They have separate identities. The BuildKit daemon runs rootless in the
dedicated `labweaver-build` namespace. Sprint 2 explicitly permits only the
rootless BuildKit requirements `Unconfined` seccomp/AppArmor and
container-scoped SELinux `spc_t` with `--oci-worker-no-process-sandbox`;
privileged containers, HostPath and
hostNetwork remain prohibited. BuildKit uses mTLS, default-deny network policy,
a dedicated ServiceAccount without Kubernetes API access, and no shared API
process credentials.

The Container and KubeVirt executors retain their current broad namespace CRUD
ClusterRoles for Sprint 2. This is an owner-accepted, time-bounded risk, not a
least-privilege or production-security verification. Compensating controls are
separate ServiceAccounts, fixed executor binaries without arbitrary shell or
`kubectl`, managed-namespace ownership checks, NetworkPolicy, generation
fences, and structured audit events. The roles must be narrowed or protected by
a native admission boundary before a production release.

The executors preserve the existing generation, deadline, replay,
cancellation, and cleanup fences. No generic plugin loader, runtime discovery,
provider ordering, or fallback is introduced.

### Evidence and release

Unit, contract, integration, browser, deployment, and release reports remain.
E0-E4 labels are not maintained for every capability or Issue. The Release Gate
records only the evidence needed for its decision and binds it to one commit,
deployment manifest, migration catalog, image set, and run ID.

## Consequences

- Sigstore, Kyverno, and Packer are absent from the Sprint 2 product profile and
  Release Gate. Existing infrastructure installations are not deleted or
  reconciled by this delivery.
- The adopted cluster is consumed in place. Kubernetes, KubeVirt, PostgreSQL,
  NATS, MinIO, Harbor, Keycloak, namespaces, volumes, realms, projects, streams,
  buckets, and schemas are retained. Adoption may create a missing LabWeaver
  object or apply a reviewed forward change, but it never drops or recreates an
  existing object.
- A narrow adoption command owns each required infrastructure change. For
  example, `sprint2-harbor-route` validates the existing managed Harbor
  installation and changes only its `HTTPRoute`; it does not run the Harbor
  Helm reconciliation or touch persistent Harbor state.
- `sprint2-application` creates only missing application-owned state, verifies
  exact migration and service identities when state already exists, and deploys
  the seven-image profile without invoking the destructive reset path.
- The Sprint 2 Release Gate requires both Container and real KubeVirt paths.
- Release evidence must expose the accepted broad-runtime-RBAC risk and must
  not report least-privilege verification while the exception remains.
- ADR 0006 is superseded for signing, transparency, attestation, and admission
  policy. Its digest, scanning, and immutable publication rationale survives in
  this ADR.

## Rollback

Rollback is a normal revert of this branch plus a server-side apply of the last
reviewed application and route manifests. Because this delivery does not delete
infrastructure or business data, rollback does not depend on reconstructing
Harbor, Keycloak, PostgreSQL, NATS, MinIO, Sigstore, or Kyverno state.
