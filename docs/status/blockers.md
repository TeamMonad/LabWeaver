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
  same-image freeze Jobs; Container collection runs in the Environment
  namespace with an exact read-only NFS-backed PVC binding, while VM collection
  runs in `labweaver-evaluation` with a five-minute Environment-issued
  read-only SFTP certificate.
- The coordinator, namespace-local immutable inputs, dedicated tokenless Worker
  ServiceAccount, NetworkPolicy and cleanup readback are implemented locally.
- Blocker: B review and connected PVC/SFTP Job, restart and cleanup replay have
  not run against the adopted cluster.
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
  BuildKit service bodies have been inventoried. The previous Draft PR head has
  a connected seven-image package; the ECNU provider-binding change requires a
  new package for the next exact source identity.
- Blocker: edge-router command execution is restored. The reviewed ECNU
  Anthropic-compatible endpoint is now explicit, but the authorized
  `ECNU_API_KEY` is not present in the current process environment or the
  prepared deployment host private input, so the Secret file cannot yet be
  rendered without inventing or exposing a credential.
- Exit: make the supplied `ECNU_API_KEY` available through a private file or the
  deployment process environment, render it only as
  `agent-service-secrets/anthropic-auth-token`, package the current head, then
  complete two atomic application adoptions and rollback readback without
  invoking reset or deleting retained infrastructure.
- Owner: A adoption execution; D independent Verify.

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
- Operational blocker: the application endpoint remains reachable, but SSH and
  Kubernetes management connections from the authorized workstation time out.
  Current-head packaging and non-destructive Ansible reconcile cannot proceed
  until that management path is restored.
- Exit: Playwright records Trace, screenshot and video without fixed sleeps;
  the only passing Sprint 2 report validates against its schema and binds the
  same commit, deployment manifest, migration catalog, image digests and Run ID.
- Owner: C frontend review; D E3 Verify; A release judgment.
