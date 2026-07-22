# Active Blockers

This file contains only blockers for the current `release/sprint2` source
identity. Historical Issue evidence is not a current completion claim.

## Connected supply-chain replay

- Local fixed-command Build Executor, persistent fences and digest/Trivy
  contracts are implemented.
- The 3a9 package was built and validated against retained BuildKit/Harbor/Trivy;
  application adoption run `sprint2-application-3a9ac6c1-2315` published seven
  digest-bound images and reconciled the retained target without destructive
  reset.
- Blocker: a fresh course build using a newly uploaded context currently
  terminates with `LW_AGENT_BUILD_REJECTED` before publication. The retained
  older context object was removed by its normal retention lifecycle, so it is
  not a valid fallback. The executor currently redacts the provider's exact
  object/validation cause from the machine-readable status and needs a focused
  diagnostic fix before another replay.
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
  `3a9ac6c1a31040570a303b80309a23ce5ea635e1` then completed connected package
  validation and two non-destructive application reconciliations; all ten
  workloads were Ready with digest-only images.
- Resolved in source identity `3a9ac6c1`: the deployed Web/Keycloak path now
  reaches real teacher and student landing pages, and the teacher candidate
  approval Playwright flow passes against the adopted deployment.
- Current blocker: a current Container release cannot be published or started
  from retained candidate data because Trivy evidence is stale; a new course
  build currently fails closed with `LW_AGENT_BUILD_REJECTED` and must be
  diagnosed without bypassing the gate.
- Exit: repair the build/object diagnostic root cause, publish a fresh digest
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
