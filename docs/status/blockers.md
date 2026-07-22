# Active Blockers

This file contains only blockers for the current `release/sprint2` source
identity. Historical Issue evidence is not a current completion claim.

## Connected supply-chain replay

- Local fixed-command Build Executor, persistent fences and digest/Trivy
  contracts are implemented.
- Blocker: no current connected run has bound BuildKit, the private Harbor
  project, pinned Trivy database and published digest to the same source and
  deployment manifest.
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
- Container create/readiness and same-build PVC freeze now pass on the current
  retained cluster. Stop also reached `stopped`. The remaining Container replay
  blocker is access, start/recover after release evidence refresh, and
  residue-free delete. The current start attempt failed closed with
  `LW_ENVIRONMENT_RELEASE_EVIDENCE_EXPIRED`; this is a real release-evidence
  expiry, not a runtime fallback. KubeVirt is intentionally skipped in this
  round and remains unverified.
- Exit for the current Container slice: refresh/publish a valid Container
  release, then create, access, stop, recover, freeze, delete and cleanup
  readback under one package/deployment identity. The full Sprint 2 exit still
  additionally requires the real KubeVirt replay.
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
- Container PVC freeze is now verified with the `c14c5eda` worker build and the
  new Object Lock bucket. VM/SFTP replay is explicitly deferred this round.
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
  `da498a2643a83e32b3ab6cab3465771a019d1882` then completed connected package
  validation and two non-destructive application reconciliations; all ten
  workloads were Ready with digest-only images.
- Current blocker: the deployed Web image redirected protected routes to
  `AUTH-NOT-CONFIGURED` because it incorrectly required browser-side OIDC build
  variables while the approved deployment uses the Access BFF. Its generated
  SDK transport also forced bearer mode, so browser mutations could not carry
  the BFF cookie and synchronizer token.
- Exit: package and reconcile the BFF repair, then complete fresh teacher and
  student sessions plus the same-build Container/VM evidence without invoking
  reset or deleting retained infrastructure.
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
- Blocker: real Keycloak teacher/student sessions, `demo replay` and the
  machine-readable `release-gate` are not closed under one deployment identity.
- Operational blocker: the cluster and application endpoint are reachable, but
  real login currently stops at the deployed Web BFF mismatch described above.
  Retained Agent runs cannot substitute because their completion event did not
  produce Control candidates.
- Exit: Playwright records Trace, screenshot and video without fixed sleeps;
  the only passing Sprint 2 report validates against its schema and binds the
  same commit, deployment manifest, migration catalog, image digests and Run ID.
- Owner: C frontend review; D E3 Verify; A release judgment.
