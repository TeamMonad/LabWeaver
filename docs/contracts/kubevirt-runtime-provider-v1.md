# KubeVirt Runtime Provider v1

Status: implemented locally for Issue #53; A+B security review, D Verify and a
connected KubeVirt/CDI E3 replay are required before acceptance.

## Scope and ownership

Environment owns release admission, lifecycle transitions, provider selection,
sanitized VM observations and endpoint eligibility. The deployment-owned
KubeVirt executor owns Kubernetes, CDI and KubeVirt API calls. Access owns
AccessGrant state and the OpenSSH Gateway connection decision.

This contract does not add a public endpoint, microservice, VNC path, Tailnet,
NodePort, LoadBalancer, arbitrary cloud-init shell or Container fallback.

## Release and provider binding

One VM operation is admitted only when all of these identities agree:

- `EnvironmentInstance` is `virtual_machine` and binds the exact configured
  provider;
- approved `EnvironmentSpec.runtime` binds that provider, the immutable base
  disk, reviewed storage binding and SSH port 22;
- `EnvironmentTemplateRelease` contains a VM artifact whose `ArtifactRef`,
  object version, SHA-256 and disk format match the spec and policy evaluation;
- the release is not withdrawn or expired and uses the active policy revision,
  trust revision and trust-bundle SHA-256;
- security requires a non-root guest, mutable VM root disk, denied privilege
  escalation and denied public exposure.

`Stop` and namespace cleanup remain available after withdrawal so access and
resources can fail closed. Provision, observe, start, restart, reset, retry and
recover cannot create a Ready endpoint from a withdrawn, expired or trust-
rotated release.

The deployment binding requires:

```json
{
  "providerKind": "kubevirt",
  "binding": "kubevirt-primary-v1",
  "subject": "labweaver.provider.kubevirt.vm.v1",
  "storageClassBinding": "vm-rwo-primary-v1",
  "storageClassName": "local-path",
  "dataSourceNamespace": "labweaver-system",
  "dataSourceName": "ubuntu-lab-base-v1",
  "gatewayNamespace": "access-system",
  "gatewayPodLabel": "openssh-gateway",
  "guestUser": "lab",
  "sshUserCaPublicKey": "ssh-ed25519 ...",
  "vmiMemoryOverheadBytes": 536870912,
  "cdiImporterCpuRequestMillicores": 1000,
  "cdiImporterCpuLimitMillicores": 4000,
  "cdiImporterMemoryRequestBytes": 262144000,
  "cdiImporterMemoryLimitBytes": 1073741824,
  "cdiScratchStorageBytes": 10737418240,
  "activeImagePolicyId": "01900000-0000-7000-8000-000000000001",
  "activeImagePolicyRevision": 1,
  "activeTrustRevision": 1,
  "activeTrustBundleSha256": "<64 lowercase hex>"
}
```

The path is deployment-owned and must be absolute. The six resource-budget
values must be non-zero, each CDI limit must be at least its request, and the
values must match or conservatively exceed the deployed KubeVirt VMI memory
overhead and CDI workload requests/limits. The scratch budget must be at least
the approved root-disk request. A wildcard subject, partial binding, private
key, invalid public key or Container-only field fails startup.

VM v1 accepts exactly one entry and it must be SSH port 22. Any additional
HTTP, HTTPS or SSH entry is rejected instead of being silently omitted from the
immutable approved spec.

## Deterministic resource plan

For environment `<id>`, the namespace is `lw-env-<id>`. The provider emits
exactly one of each owned runtime object unless noted:

| Object | Required behavior |
| --- | --- |
| Namespace | Deterministic name, Environment/course labels and controlled cleanup finalizer. |
| ResourceQuota | Guest resources plus explicit VMI-memory and CDI-importer request/limit budgets, one CDI scratch PVC, at most two PVCs and two transient/runtime pods. The VM memory limit is guest memory plus the reviewed VMI overhead, so KubeVirt's derived request cannot exceed the limit when the observed overhead remains within the binding. |
| NetworkPolicy | Default-deny ingress and egress; one additional SSH ingress rule from the exact Gateway namespace and pod label; optional reviewed restricted-egress rule. |
| Secret | Fixed base64 `data.userdata` cloud-init with public user CA only; locked non-root user; no password, root login, forwarding, tunnel, X11, private key or `authorized_keys`. |
| DataVolume | Exact configured CDI `DataSource` and `StorageClass`, RWO root disk, immutable release/object/hash annotations. |
| VirtualMachine | `runStrategy: Always`, hardware-KVM node selector, no graphics, virtio root PVC and cloud-init disk, pod network and SSH readiness probe. |
| Service | ClusterIP only, port 22, deterministic selector and access-controlled annotation. |

The guest principal file admits `labweaver-gateway` and
`labweaver-collector`. The Collector principal is usable only with a
single-Environment user certificate that expires within five minutes and has
critical `force-command = internal-sftp -R`; Evaluation pins the observed host
key and exposes only SFTP reads. Principal enrollment is not credential
issuance. A deployment-owned short-lived issuer and ephemeral Secret cleanup
remain mandatory for VM Collector E3.

The executor uses server-side apply with deterministic field ownership. Before
creating or starting the VM it resolves the private artifact reference and
verifies the CDI source and resulting PVC match the exact base-disk object
version and SHA-256. A mutable `DataSource` name or annotation alone is not
evidence. Exact reapplication does not create another VM, DataVolume or PVC.

## Backend protocol and fencing

Requests use NATS request/reply on the exact configured subject and carry:

```text
protocolVersion = 1
environmentId
operationId
providerStep
environmentGeneration
attempt
action
requestId = sha256(canonical fence identity)
deadlineAt
plan + planSha256
```

Replies repeat every fence field and the plan SHA-256. A mismatch, unknown
field, oversized response or action/result mismatch is rejected as an invalid
observation. Transport failure is retryable and never produces an endpoint.

The executor persists the highest accepted
`(environmentGeneration, attempt, providerStep)` tuple per environment. An
exact request ID returns the exact prior result. The same request ID with a
different payload, an older tuple or any non-cleanup request after a deletion
tombstone is rejected without a side effect. A newer generation cannot be
removed by an older cleanup.

## Readiness and SSH endpoint

An observation reaches `Ready` only when:

- observed environment generation equals the requested generation;
- KubeVirt observed generation is at least the VM resource generation;
- VM, current VMI and root PVC UIDs are non-empty;
- guest and ClusterIP addresses are non-unspecified, non-loopback and
  non-multicast;
- guest agent is connected;
- the executor completed an SSH handshake through the controlled path and
  returned a non-empty host-key SHA-256.

Environment transactionally records the observation before returning one
stable, healthy SSH endpoint ID. Object existence, VMI phase, a TCP port or a
Service IP by itself cannot produce an endpoint. Access still revalidates its
own Grant and endpoint revision for every new connection.

`Stop` proves VMI absence and records no endpoint while retaining VM UID,
root-disk UID and host-key SHA-256. `Start` and `Restart` must return the same VM,
root-disk and host-key identities. `Reset` is the only operation allowed to
replace them under a newer generation. Identity drift otherwise fails closed.

## Failure, recovery and cleanup

Lifecycle cancellation, timeout, failure, expiry and delete first use the
existing Environment-to-Access revocation fence. Namespace cleanup then removes
all owned objects, clears the controlled finalizer and verifies absence of the
Namespace, VM, VMI, DataVolume, PVC, Secret, Service and NetworkPolicy. Only an
immutable, non-empty `ArtifactRef` to sanitized cleanup evidence permits
`Deleted`; cleanup failure remains retryable and exposes no endpoint.

Environment stores the last accepted running/stopped observation and deletion
tombstone in `environment.kubevirt_runtime_observations`. Exact replay is
idempotent. Stale tuple, request-payload conflict, unexpected disk/VM/host-key
replacement or late work after deletion is rejected. `Observe` may recover an
unrecorded current VM only when every readiness and release identity check
succeeds.

## Evidence boundary

Local tests prove deterministic plans, private networking, safe cloud-init,
readiness gating, stable endpoint identity, duplicate request fencing,
stop-start identity preservation, cleanup evidence and PostgreSQL tombstones.
They do not prove KubeVirt, CDI, guest boot, SSH or network enforcement.

E3 must use the same commit and deployment identity to prove real CDI import,
VM/guest/SSH readiness, disk persistence across start-stop-start, no duplicate
VM/DataVolume, denied non-Gateway network access, failure/cancel/recovery/delete
cleanup and unusable grants after revocation.
