# ADR 0009: Fenced KubeVirt Runtime Provider and Private SSH Endpoint

Status: proposed and implemented locally for Issue #53; requires A+B security
review and D same-build Verify before acceptance.

## Context

An approved VM release is not executable merely because a `VirtualMachine`,
`DataVolume` or PVC exists. Environment must bind the immutable VM artifact,
storage class, KubeVirt objects, guest readiness, SSH host identity and Access
endpoint to one operation generation. Duplicate or late reconciliation must not
create a second VM or revive resources after deletion. Stop/start must preserve
the root disk and SSH host identity, while reset is the only operation allowed
to replace them.

The Environment service owns lifecycle state and the sanitized VM observation.
The deployment-owned KubeVirt executor owns Kubernetes/CDI API calls. Access
owns grants and the Gateway connection boundary. No component may infer another
owner's authoritative state from object existence or message order.

## Decision

`providerKind: "kubevirt"` selects one exact configured NATS subject, storage
binding, `StorageClass`, CDI `DataSource`, OpenSSH Gateway identity, locked guest
user, public user CA and active release policy/trust identity. Startup rejects
missing, mixed or wildcard bindings. A release is usable only when its VM
`ArtifactRef`, SHA-256, format, provider and storage binding match the approved
`EnvironmentSpec`; signature, policy, freshness and withdrawal checks are
reapplied before every operation that can create or expose a running VM.

The provider deterministically emits one namespace with a cleanup finalizer,
quota, default-deny policy, Gateway-only SSH ingress policy, fixed cloud-init
Secret, CDI `DataVolume`, KubeVirt `VirtualMachine` and ClusterIP SSH Service.
The plan carries the immutable base-disk `ArtifactRef`, expected SHA-256 and
plan SHA-256. It never emits NodePort, LoadBalancer, Ingress, VNC, a private key,
`authorized_keys`, user-supplied shell or arbitrary cloud-init content.

VM v1 admits exactly one approved `ssh:22` entry. Cloud-init is stored under
the KubeVirt-required Secret key `data.userdata`, not `stringData`. Guest CPU
and memory remain VM requests; the memory limit includes the reviewed VMI
overhead instead of equalling guest memory. The namespace quota adds explicit
CDI importer CPU/memory request and limit budgets plus a bounded scratch-PVC
budget, and allows the importer/runtime pod and target/scratch PVC pairs.
Startup rejects missing, zero or inverted budget bindings; plan creation rejects
a root disk larger than the reviewed CDI scratch budget.

Every executor request carries protocol version, environment and operation IDs,
provider step, environment generation, attempt, action, deterministic request
ID and deadline. The executor must persist the highest accepted fence per
environment, return the exact prior result for an exact request replay, reject
older or payload-conflicting requests, and persist a namespace deletion
tombstone before reporting cleanup. Server-side apply uses deterministic field
ownership and names; it must verify the CDI source/PVC corresponds to the exact
base-disk object version and SHA-256 rather than trusting a mutable name or
annotation.

`Ready` requires the current environment generation, a current observed VM
generation, non-empty VM/VMI/PVC UIDs, non-routable-private guest and Service
IPs, connected guest agent, successful SSH handshake and a non-empty SSH host
key SHA-256. Environment persists that sanitized identity in PostgreSQL before
publishing one stable healthy SSH endpoint. Stop requires VMI absence and
preserves VM UID, root-disk UID and host-key identity. Start and restart must
return those same identities. Reset may replace them under a newer generation.

Deletion is reached only after Access revocation has been recorded by the
existing lifecycle boundary. Cleanup remains callable when a release is
withdrawn or unavailable, deletes the entire deterministic namespace, proves
absence of VM, VMI, DataVolume, PVC, Secret, Service and NetworkPolicy, and
stores immutable cleanup evidence. The Environment tombstone wins over late
start, apply, observe or restart results.

## Alternatives considered

- Direct Kubernetes calls inside the lifecycle transaction were rejected
  because they couple database locks to an unbounded external API and cannot
  safely recover after a side effect but before commit.
- A mutable image URL or tag in cloud-init/CDI was rejected because it breaks
  the approved artifact identity chain.
- Per-user `authorized_keys` injection was rejected because AccessGrant expiry
  and revocation would no longer be authoritative at connection time.
- VM `Running` or a TCP probe alone was rejected because neither proves guest
  readiness, SSH host identity or endpoint authorization.
- Container or static-report substitutes were rejected because Issue #53 is a
  KubeVirt VM capability.

## Security and data implications

The only credential material in the resource plan is a public SSH user-CA key.
The guest account is non-root and password-locked; root login, interactive
passwords, forwarding, tunnelling and X11 are disabled. SSH is reachable only
through the configured Access Gateway pod selector and namespace. Raw guest
content, private keys, tokens, Kubernetes objects and executor credentials are
not stored in Environment state or logs.

The observation table contains only UUIDs, IP addresses, hashes, state and
sanitized cleanup evidence. Database errors are retryable; stale fences,
identity drift, invalid readiness and tombstoned operations fail closed without
a Ready endpoint.

## Compatibility and migration

Migration `environment/0005_kubevirt_runtime_observations.sql` is additive and
forward-only. It must be applied before enabling a KubeVirt provider binding.
Existing remote and Container bindings retain their behavior, and provider-
specific fields are rejected on the wrong kind.

Before publication, rollback is whole-PR reversion. After a VM has been
created, rollback first stops new admission, revokes grants, uses the same
cleanup protocol to remove owned namespaces, and disables the binding. The
observation table and deletion tombstones are retained for audit and late-
message fencing; they are not dropped. An old verified VM release is restored
by creating a new environment, never by mutating the failed instance or its
artifact identity.

## Evidence and replacement conditions

Local evidence includes deterministic resource-plan and negative security
tests, readiness/endpoint gating, duplicate request fencing, stop/start identity
preservation, cleanup behavior and a real PostgreSQL 17 migration/tombstone
test. This is E1 plus local E2 persistence evidence. It is not real VM evidence.

E3 requires a same-build run against the reviewed KubeVirt/CDI cluster and
executor: exact base-disk import, guest-agent readiness, SSH host-key handshake,
start-stop-start disk persistence, duplicate reconcile, network denial,
failure/cancel/recovery cleanup and proof that no AccessGrant remains usable.
This ADR is replaced only by another reviewed decision that preserves the same
immutable artifact, ownership, fencing, private-access and cleanup guarantees.
