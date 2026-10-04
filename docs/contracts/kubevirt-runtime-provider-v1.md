# KubeVirt Runtime Provider v1

This document describes the Environment-owned KubeVirt implementation. Changes follow the
normal owner review process; local tests do not establish behavior on a real KubeVirt/CDI cluster.

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
- `EnvironmentTemplateRelease` contains a VM artifact whose deployment binding,
  immutable OCI source digest, disk SHA-256, capacity and disk format match the
  approved spec and reviewed deployment lock;
- the release is not withdrawn and uses the active approval trust revision;
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
  "activeTrustRevision": 1
}
```

The path is deployment-owned and must be absolute. The six resource-budget
values must be non-zero, each CDI limit must be at least its request, and the
CDI budgets must match or conservatively exceed the deployed importer workload
requests/limits. The VMI memory headroom is added once to the guest compute
limit; it does not replace KubeVirt launcher overhead. `cdiScratchStorageBytes` and the
approved root-disk storage request are logical disk requirements; the scratch
budget must be at least the approved root-disk request. A wildcard subject,
partial binding, private key, invalid public key or Container-only field fails
startup.

VM v1 accepts exactly one entry and it must be SSH port 22. Any additional
HTTP, HTTPS or SSH entry is rejected instead of being silently omitted from the
immutable approved spec.

The executor derives physical root-plus-scratch quota from the current
`CDIConfig.status.filesystemOverhead`, selecting the StorageClass override
before `global`, and the effective `scratchSpaceStorageClass`. It follows
[CDI 1.65 sizing](https://github.com/kubevirt/containerized-data-importer/blob/v1.65.0/pkg/util/util.go):
root bytes align up to 1 MiB before applying overhead and rounding up; scratch
uses the corresponding usable size rounded down to 1 MiB, applies the scratch
class overhead, then aligns up to 1 MiB. Only the applied quota uses physical
bytes; the approved plan, `planSha256` and billing storage remain logical.

Applied CPU and memory quota follows the installed [KubeVirt 1.8.4 KVM
calculation](https://github.com/kubevirt/kubevirt/blob/v1.8.4/pkg/hypervisor/kvm/hypervisorbackend.go)
and [native serial-console resources](https://github.com/kubevirt/kubevirt/blob/v1.8.4/pkg/virt-controller/services/serialconsolelog.go).
The executor lists at most two KubeVirt instances and requires exactly one,
without pagination, with observed and target version both `v1.8.4`. Its
configuration is the sole authority for console enablement and resource policy.

For the managed amd64 projection, launcher memory includes Kilo-rounded page
tables, fixed KVM processes, vCPU and IO-thread tables, optional graphics, and
one VFIO allowance when GPU or host devices are present. It adds that overhead
to both compute request and limit. An enabled native console adds its own
request and limit; CDI importer budgets cover concurrent import work. Existing
quota must match all derived CPU, memory and storage quantities exactly; drift
does not authorize widening. Guest resources, plan hash and billing remain unchanged.

This calculation supports the default overhead ratio (1) and default native
console request/limit resources. Different effective overrides, unknown
versions or compute-affecting shapes outside the managed projection fail
closed. Console disablement and per-VM console flags follow KubeVirt precedence.
The explicit compute memory limit means KubeVirt automatic memory-limit
generation does not apply.

Root and seed DataVolumes explicitly request `Filesystem` on an existing
managed StorageClass. Missing or null StorageProfile `claimPropertySets` does
not make this explicit mode unsupported. A profile's
`cdi.kubevirt.io/minimumSupportedPvcSize` must be absent or zero; nonzero or
malformed overrides are rejected. Existing Bound PVCs must match the exact
derived physical size and DataVolume controller UID chain. Drift is rejected
without automatic expansion. A matching Pending PVC remains subject to
bounded readiness checks and does not establish Ready.

## Deterministic resource plan

For environment `<id>`, the namespace is `lw-env-<id>`. The provider emits
exactly one of each owned runtime object unless noted:

| Object | Required behavior |
| --- | --- |
| Namespace | Deterministic name, Environment/course labels and controlled cleanup finalizer. |
| ResourceQuota | The intent retains guest resources, compute-limit headroom and CDI budgets. Applied quota additionally covers KVM launcher overhead and native console resources, physical root-plus-scratch storage, at most two PVCs and two transient/runtime pods. |
| NetworkPolicy | Default-deny ingress and egress; one additional SSH ingress rule from the exact Gateway, freeze collector, and (when configured) Evaluation runner namespace and pod selectors; optional reviewed restricted-egress rule. |
| Secret | Fixed base64 `data.userdata` cloud-init with public user CA only; locked non-root user; no password, root login, forwarding, tunnel, X11, private key or `authorized_keys`. |
| DataVolume | Exact configured CDI `DataSource` and `StorageClass`, RWO Filesystem root disk with the approved logical storage request, immutable release/object/hash annotations. |
| VirtualMachine | `runStrategy: Always`, explicit amd64 architecture, hardware-KVM node selector, graphics and serial console, virtio root PVC and cloud-init disk, pod network and SSH access. |
| Service | ClusterIP only, port 22, deterministic selector and access-controlled annotation. |

The guest principal file admits `labweaver-gateway`, `labweaver-collector`,
`labweaver-evaluation`, and `labweaver-agent`. The Collector principal is
usable only with a single-Environment user certificate that expires within
five minutes and has critical `force-command = internal-sftp -R`; Evaluation
and Agent execution bindings use their own short-lived user certificates
without a force command. Evaluation pins the observed host key and source
identity before connecting. Principal enrollment is not credential issuance.
A deployment-owned short-lived issuer and ephemeral Secret cleanup remain
mandatory for VM Collector and execution flows.

The executor uses server-side apply with deterministic field ownership. Before
creating or starting the VM it verifies that the CDI source and resulting PVC
match the exact deployment-locked base-disk source digest and disk SHA-256. A
mutable `DataSource` name or annotation alone is not
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

## Testing

Local tests cover deterministic plans, private networking configuration, cloud-init generation,
readiness gating, stable endpoint identity, duplicate request fencing,
stop-start identity preservation, cleanup evidence and PostgreSQL tombstones.
They do not prove KubeVirt, CDI, guest boot, SSH or network enforcement.

Cluster integration tests must exercise CDI import, VM/guest/SSH readiness, disk persistence
across start-stop-start, duplicate request handling, denied non-Gateway network access,
failure/cancel/recovery/delete cleanup and rejected connections after grant revocation.
