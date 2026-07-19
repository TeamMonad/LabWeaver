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
`Unconfined` seccomp/AppArmor and `--oci-worker-no-process-sandbox` exception.
Privileged containers, HostPath and hostNetwork remain prohibited. This removes
the design-decision blocker, but not the missing deployment or connected replay.

## Container and KubeVirt runtime replay

- Local restricted Kubernetes API executors, independent ServiceAccounts/RBAC,
  generation fences and cleanup tombstones are implemented.
- Blocker: no current same-build run proves Container and real hardware-KVM
  KubeVirt create, access, stop, recover, freeze and residue-free deletion.
- Exit: both runtimes pass lifecycle, identity drift, stale generation,
  namespace ownership, network isolation, disk preservation and cleanup tests.
- Owner: B implementation review; D connected Verify.

The current broad Container/KubeVirt namespace CRUD ClusterRoles are an
explicit Sprint 2 accepted risk. They are not described as least-privilege
evidence. Production use remains blocked until RBAC is narrowed or a native
admission boundary enforces the executor namespace/ServiceAccount fence.

## Freeze-only Evaluation deployment

- The owner selected Evaluation as the Sprint 2 `FrozenSubmission` owner; all
  execution and scoring remain excluded.
- The deployment topology is fixed in ADR 0010: a coordinator creates bounded
  same-image freeze Jobs in `labweaver-system`; Container collection uses an
  exact read-only NFS-backed PVC/PV binding, while VM collection uses a
  five-minute Environment-issued read-only SFTP certificate.
- Blocker: the coordinator, Environment certificate issuer, freeze-worker mode,
  namespace-local Job RBAC and connected PVC/SFTP cleanup replay are not yet
  implemented and reviewed.
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

## Destructive reset and idempotent deployment

- The repository now contains one Sprint 2 baseline migration per domain, a
  nine-workload Helm profile and a cluster/run-bound destructive reset role.
- Blocker: read-only target inventory and the retained-foundation replay confirm
  Kubernetes/KubeVirt, Harbor, Keycloak, PostgreSQL, NATS and MinIO, but no
  BuildKit service body or complete reviewed private workload-configuration
  bundle is available on the approved controller. The reset therefore remains
  intentionally unexecuted.
  The role probes all six dependencies before its first destructive task and
  rejects a missing or malformed eight-ConfigMap/eight-Secret bundle.
- Exit: the deployment owner supplies the missing service bodies and ignored
  reviewed inputs; the reset report binds cluster UID, source commit, migration
  catalog, configuration-bundle hash, image set and deletion hashes without
  Secret material after dependency guard, double deploy and rollback readback.
- Owner: A reset approval and execution; D independent Verify.

The repository now has a separate `sprint2-foundation` reconciliation for the
retained PostgreSQL, NATS and MinIO service bodies. Its Linux syntax/lint and
fixture evidence is static deployment evidence only. At source identity
`4ced06d`, the approved target also completed a real reconciliation and a second
zero-change replay: PostgreSQL, NATS and MinIO each reported one ready replica,
with bound persistent claims, digest-locked images, TLS and default-deny policy.
The private four-object bundle and seven currently deployed workload NATS
identities remain outside Git. The next foundation reconciliation must rotate
that private authority to the eight-identity contract that includes the
freeze-only Evaluation Service. The existing replay closes the retained service
body dependency, but it is not Sprint 2 E3 evidence and does not authorize
reset. `labweaver-data` and `labweaver-build` remain outside namespace reset;
LabWeaver data is cleared only after all reset dependency probes pass.

## Browser replay and Release Gate

- Web SDK and local browser fixtures exist.
- Blocker: real Keycloak teacher/student sessions, `demo replay` and the
  machine-readable `release-gate` are not closed under one deployment identity.
- Exit: Playwright records Trace, screenshot and video without fixed sleeps;
  the only passing Sprint 2 report validates against its schema and binds the
  same commit, deployment manifest, migration catalog, image digests and Run ID.
- Owner: C frontend review; D E3 Verify; A release judgment.
