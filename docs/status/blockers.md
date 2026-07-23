# Active Blockers

This file contains only blockers for the current `release/sprint2` source
identity. Historical Issue evidence is not a current completion claim.

## Issue #131 local single-node-kind deployment

- The repository now contains the initial `ex3` profile, pinned kind node
  configuration, local inventory template, and bootstrap role.
- The base connected layer is now deployed manually from Alpine WSL: the
  digest-pinned kind cluster, Experimental Gateway API, Cilium, local-path,
  PostgreSQL, NATS and MinIO are Ready/Bound, and the Sprint 2 foundation
  reconcile completed successfully.
- Source `e9b875a0368a3aa34f554a0c1a3aa88812c3b511` reconciled Harbor,
  Trivy, Keycloak and the rootless BuildKit Service through the local
  Gateway/NodePort path. Foundation and identity verification pass, and the
  worktree used to establish this baseline is clean.
- The application layer is blocked before deployment. A real package attempt
  reaches rootless BuildKit but inner `runc` fails with
  `no cgroup mount found in mountinfo` on the Docker Desktop/kind node. The
  seven-image package manifest therefore does not exist. The rejected
  `hostUsers=false` experiment was rolled back; privileged, HostPath and #130
  Fixture images are not permitted substitutes.
- A deliberately separate mixed-source demonstration deployed the Issue #131
  terminal path and passed real browser login, PTY write, reconnect, full-screen
  and revoke-denial behavior. It is not the application-layer exit: six images
  predate Environment/container-executor source `96d9ca3d`, no seven-image
  package manifest exists, and infrastructure Secret material entered
  transient local diagnostic output. Recycle the local cluster and regenerate
  its private bundle before using it for security, D Verify or Release evidence.
- Exit: repair the kind/rootless OCI execution boundary without widening the
  accepted BuildKit security exception, package all seven images from one clean
  source identity, then run application and verify with one Run ID and capture
  the second-reconcile and fail-closed Container evidence.

## Connected supply-chain replay

- Local fixed-command Build Executor, persistent fences and digest/Trivy
  contracts are implemented.
- The `e062d34f` seven-image package was built and validated against retained
  BuildKit/Harbor/Trivy; application adoption run
  `sprint2-application-e062d34f-0001` reconciled the retained target without
  destructive reset.
- Resolved causes: the build worker now receives the combined MinIO/Harbor CA
  bundle through both Rustls AWS SDK and process TLS bindings; the retained
  Cilium policy has separate valid host and platform-client ingress rules; the
  pinned Trivy DB exists in the worker-readable repository.
- Last exact application failure: a fresh course build completed BuildKit,
  Trivy and digest publication, then Harbor rejected candidate-tag deletion
  with `403 Forbidden`. The worker robot was granted only
  `artifact:delete`, its Secret was reconciled, and the worker rolled out.
- Current blocker: the required post-permission replay cannot be observed.
  Direct Kubernetes API access, router SSH and `https://demo.lab.lan` all time
  out. Per fail-fast policy no further mutation or completion claim is made
  until one control path is restored. The bounded diagnostic Pod
  `harbor/harbor-admin-probe` may remain in terminal state and must be removed
  before final residue verification.
- Exit: duplicate/reordered command, cancel, deadline, scanner rejection,
  cleanup and private-pull negatives pass in the adopted environment.
- Owner: B implementation review; D connected Verify; A release judgment.

The owner approved rootless BuildKit for Sprint 2 with the narrowly scoped
`Unconfined` seccomp/AppArmor, container-scoped SELinux `spc_t`, and
`--oci-worker-no-process-sandbox` exception.
Privileged containers, HostPath and hostNetwork remain prohibited. This removes
the design-decision blocker, but not the missing deployment or connected replay.

## Container and KubeVirt runtime replay

- Local restricted Kubernetes API executors, independent ServiceAccounts/RBAC,
  generation fences and cleanup tombstones are implemented.
- Container create/readiness, student AccessGrant creation, workspace freeze,
  stop, and start now pass on a fresh published Container release. The
  Environment worker fix is `8c08fe4e`; it accepts the provider's immediate
  `Stopped -> Ready` observation instead of converting it to a failure.
- Delete and cleanup now pass on the same Container identity after fixing the
  cleanup-plan validator, failed-operation fence tombstone, KubeSphere finalizer
  race, MinIO object-prefix binding and versioned evidence write. The namespace
  and runtime resources are absent after the application-owned cleanup request.
  The retained `labweaver-artifacts` bucket has versioning but no Object Lock;
  cleanup evidence therefore uses conditional versioned immutable storage and
  does not claim Governance Lock.
- KubeVirt is intentionally skipped in this round and remains unverified. The
  full Sprint 2 exit still additionally requires the real KubeVirt replay.
- Owner: B implementation review; D connected Verify.

The current broad Container/KubeVirt namespace CRUD ClusterRoles are an
explicit Sprint 2 accepted risk. They are not described as least-privilege
evidence. Production use remains blocked until RBAC is narrowed or a native
admission boundary enforces the executor namespace/ServiceAccount fence.

## Freeze-only Evaluation deployment

- The owner selected Evaluation as the Sprint 2 `FrozenSubmission` owner; all
  execution and scoring remain excluded.
- The deployment topology is fixed in ADR 0010: a coordinator creates bounded
  same-image freeze Jobs; Container collection runs in the Environment
  namespace with an exact read-only NFS-backed PVC binding, while VM collection
  runs in `labweaver-evaluation` with a five-minute Environment-issued
  read-only SFTP certificate.
- The coordinator, namespace-local immutable inputs, dedicated tokenless Worker
  ServiceAccount, NetworkPolicy and cleanup readback are implemented locally.
- The first connected Container freeze attempts reached the worker and exposed
  a source defect: the request and completion events reused aggregate sequence
  1, so PostgreSQL rejected the completion outbox insert and the attempt became
  `LW_COLLECT_DATABASE_FAILED`. Commit `12069bbb` advances the completion event
  to sequence/revision 2. It is not yet packaged or replayed against the live
  cluster.
- Container PVC freeze is verified on the retained prior worker build; a fresh
  3a9 same-identity freeze replay remains pending. VM/SFTP replay is
  explicitly deferred this round.
- Exit: duplicate/reordered commands, expired leases/certificates, PV identity
  mismatch, host-key mismatch, partial upload, worker restart and all residue
  checks fail closed under B review and D connected Verify.

## Access Gateway deployment

- Local key authentication, post-auth alias redemption, one-time session token
  and fixed target command are implemented.
- Blocker: the Gateway image and Secret permission handoff have not run against
  the adopted Access Service and real runtime endpoints.
- Exit: invalid key, cross-course alias, target injection, SCP/SFTP/forwarding,
  Access outage, expiry and revocation all fail closed; revoked sessions end
  within 60 seconds.
- Owner: A authorization semantics; B security review; D Verify.

## Non-destructive adoption and idempotent deployment

- The repository contains one Sprint 2 baseline migration per domain, a
  Sprint 2 Helm profile and an allowlisted non-destructive application adoption
  path. The legacy reset playbook is explicitly outside this delivery.
- Retained PostgreSQL, NATS, MinIO, Harbor, Keycloak, Kubernetes, KubeVirt and
  BuildKit service bodies have been inventoried. Source
  `d85855e9a236ba605411921b8575d1f785acfde5` has a connected seven-image
  package whose Harbor digests and pinned Trivy database passed connected
  validation.
- The reviewed ECNU Anthropic-compatible endpoint and `ecnu-plus` binding were
  verified from the operator workstation with the supplied credential. The
  credential is present only in ignored private input as
  `agent-service-secrets/anthropic-auth-token`; it is not repository or release
  evidence until the application adoption reads it back through the mounted
  Secret file.
- The current Control NATS user was forward-rotated to permit only the two
  configured Agent quarantine subjects in addition to its retained grants. Its
  public claims passed the checked-in fail-closed credential validator; no
  retained stream or account was reset.
- Resolved prerequisite: `k8s-cp1` and kube-apiserver recovered. Source
  `1122ef6e` completed connected package validation and a non-destructive
  application reconciliation; retained infrastructure was not deleted or
  rebuilt.
- Resolved in source identity `3a9ac6c1`: the deployed Web/Keycloak path now
  reaches real teacher and student landing pages, and the teacher candidate
  approval Playwright flow passes against the adopted deployment.
- Current blocker: a current Container release cannot be published or started
  until the S3 dispatch failure is repaired without bypassing immutable object
  verification.
- Exit: repair the build/object transport root cause, publish a fresh digest
  with valid Trivy evidence, then complete the Container replay without reset
  or deleting retained infrastructure.
- Owner: A BFF/adoption execution; C frontend review; D independent Verify.

The repository now has a separate `sprint2-foundation` reconciliation for the
retained PostgreSQL, NATS and MinIO service bodies. Its Linux syntax/lint and
fixture evidence is static deployment evidence only. At source identity
`4ced06d`, the approved target also completed a real reconciliation and a second
zero-change replay: PostgreSQL, NATS and MinIO each reported one ready replica,
with bound persistent claims, digest-locked images, TLS and default-deny policy.
The private four-object bundle and deployed workload NATS identities remain
outside Git. The next foundation reconciliation must rotate that private
authority to the eight-identity contract that includes the freeze-only
Evaluation Service. The existing replay closes the retained service-body
dependency, but it is not Sprint 2 E3 evidence and does not authorize deletion.

## Browser replay and Release Gate

- Web SDK, local browser fixtures, real Keycloak auth setup and live
  teacher/student specifications exist. Passwords are read only from private
  files; live and Fixture specifications are mutually excluded.
- Teacher Keycloak setup and candidate approval now pass on the adopted 3a9
  deployment. Student Container create/access is still blocked downstream by
  stale image evidence and failed runtime publication, so the browser and
  Release Gate remain open.
- Operational blocker: the cluster and application endpoint are reachable, but
  real login currently stops at the deployed Web BFF mismatch described above.
  Retained Agent runs cannot substitute because their completion event did not
  produce Control candidates.
- Exit: Playwright records Trace, screenshot and video without fixed sleeps;
  the only passing Sprint 2 report validates against its schema and binds the
  same commit, deployment manifest, migration catalog, image digests and Run ID.
- Owner: C frontend review; D E3 Verify; A release judgment.
