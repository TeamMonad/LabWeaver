# Container Supply Chain and Runtime v1

This document describes the supply-chain contract. Issue #180 tracks current
implementation and local validation; remote execution is outside this change.

## Scope and ownership

Control accepts an approved Container candidate and creates one immutable build
request. Agent owns the build command, terminal build state and `ImageArtifact`.
Environment owns the release projection and the lifecycle of the resulting
namespace. Deployment-owned executors perform only their fixed operations:

```text
build-executor:
  ensure private Harbor project
  -> buildctl build and push
  -> Trivy scan with pinned database
  -> read Harbor digest
  -> publish digest-bound result
  -> cleanup

container-executor:
  server-side apply
  -> observe
  -> scale, restart or delete
  -> cleanup readback
```

Neither executor accepts a shell command, `kubectl` text, mutable image tag or
Provider selected by registration order.

## Current v1 messages

| Subject | Producer to consumer | Durable behavior |
| --- | --- | --- |
| `labweaver.control.agent_build.requested.v1` | Control to Agent | Agent records the exact command hash and Inbox identity before ACK |
| `labweaver.agent.build.completed.v1` | Agent to Control | Control resolves the authoritative digest-bound artifact using a service-account JWT over TLS |
| `labweaver.agent.build.failed.v1` | Agent to Control | Stable terminal diagnostic, retryability and cleanup status |
| `labweaver.control.environment_template_release.published.v1` | Control to Environment | Immutable release and approved EnvironmentSpec projection |
| `labweaver.control.environment_template_release.withdrawn.v1` | Control to Environment | Ordered append-only withdrawal |

Repository consumers move together to the current contract. Empty databases
use the migration baseline; service startup never resets existing data.

## Artifact and publication gate

`ImageArtifact` binds exactly:

- the configured private Harbor repository;
- an OCI `sha256:` digest;
- Trivy scanner name and version;
- the pinned Trivy database digest;
- critical, high, medium and low vulnerability counts;
- the approved image policy revision and gate result.

Critical findings, a missing or mutable digest, a repository mismatch, stale
executor generation, missing approval or a failed cleanup block publication.
Control resolves the exact successful build projection and policy evaluation;
the public publication request contains only candidate, approval and runtime
identity and cannot supply its own artifact or scanner evidence.

## Manual Work publication

`GET /api/v1/projects/{projectId}/template-images` returns the active administrator-pinned
catalog entries at the current trust revision to authorized project readers. Image import and
repinning remain administrator operations; a teacher does not receive registry credentials.

`POST /api/v1/projects/{projectId}/environment-template-releases` is the Work publication command.
The project owner or manager supplies either the approved Agent candidate fields or `manual`,
never both. `manual` contains `platformImageId`, `platformImageGeneration`, `templateFamilyId`,
`description`, the full `spec`, a bounded review `reason`, and `confirmed: true`. The runtime
must be Work; a catalog Container spec has no `build_context`. The selected current catalog
entry must be active, have the exact repin generation, match the runtime and trust revision,
and bind the reviewed registry digest or VM disk/capacity/format. Missing confirmation,
mixed sources, changed catalog identity and insufficient project authority fail closed.

Before publication, `GET /api/v1/projects/{projectId}/manual-template-options` returns labelled
runtime/storage selections from Environment's loaded provider configuration and Control's actual
retention policy ID, revision and maximum duration. This read requires publication authority, not
merely template browsing. The publication transaction validates those identities and limits again
after its idempotency replay check. Unknown placement/storage, forged retention identities and
out-of-range dates return 422 `LW_MANUAL_TEMPLATE_OPTION_INVALID` with a readable explanation;
no release or Outbox event commits. Accepted command replay retains its original frozen decision.
`securityProfileBinding` remains a non-empty label and is not an executable security profile.
No selectable profile catalog is claimed for it.

For VM disks, Environment's current `baseDisks[]` are included in the runtime option projection
with their reviewed disk SHA-256, capacity, source digest, format and storage binding. Seeded
bindings take precedence over a dynamic catalog disk of the same binding; Control refuses a
same-name catalog image unless those facts match. An unseeded catalog disk remains subject to
Environment's runtime-base resolver.
After current caller/object authorization, a completed publication key is looked up in Control's
ledger before any Agent catalog or Environment options read. A lost successful response therefore
replays the original release decision even if either downstream is now unavailable. New keys still
require the current authoritative catalogs and the transaction's reservation/validation fences.

This one human command records `manualApproval` and publishes through the existing release
table, transactional idempotency ledger, Outbox and Environment projection. It does not create
an Agent run, candidate, build request or project model policy. Imported Container artifacts use
`kind: catalog_container`; built artifacts retain `kind: container`. VM artifacts retain their
exact catalog disk identity. Only one evidence source is valid in a release: candidate/run/
candidate approval or manual approval. Resource admission validates the same immutable release,
spec digest, project membership and applied platform runtime policy for either source.

Course Experiment publication still uses `CompleteAuthoringApprovalRequest`: its single teacher
approval binds the Environment/Evaluation authoring package. The Work endpoint rejects Experiment
specs and is not a second way to publish a course experiment.

Release views expose `name`, `description`, `templateFamilyId`, `purpose` and `capabilities`
(`terminal`, `desktop`, `ssh`, `applications`, `files`, `agent`). Capabilities describe template
support, not live endpoint health or an available model. The project Agent configuration endpoint
`GET /api/v1/projects/{projectId}/agent-capability` returns `configured`, `unconfigured`
(`LW_AGENT_MODEL_NOT_CONFIGURED`), or `unavailable` (`LW_AGENT_MODEL_BINDING_UNAVAILABLE`).
`configured` means the policy matches the deployment binding, not that a provider call or quota
admission has succeeded. Unreadable/corrupt configuration is HTTP 503
`LW_AGENT_CONFIGURATION_UNREADABLE`, never `unconfigured`.


## Build context resource boundary

Agent validates the immutable context's declared size before opening its object body. The
configured `maxUnpackedContextBytes` is a per-process semaphore budget for logical unpacked
file payload bytes, not a bound on overlapping temporary files or tar header overhead. Plain tar
reserves its entire declared object size atomically; gzip reserves the full configured cap before
the body read and never trusts the gzip ISIZE trailer. Unpacking counts actual expanded file
payload bytes against that reservation, rejecting an oversized archive. Concurrent builds wait
for permits; cancellation or the original deadline interrupts the wait without leaking permits.
The permit remains held until the compressed object temp file, unpacked workspace and BuildKit
upload spool/mmap `Bytes` have all been dropped after the build. The upload tar is spooled to disk
and mapped rather than copied into an in-memory archive. Disk exposure includes simultaneous
compressed object, unpacked workspace and re-packed BuildKit spool (plus tar headers/padding),
potentially across concurrent builds; `maxUnpackedContextBytes` does not cap that disk usage.
Temp-file owners clean up on success, rejection and cancellation. Archive paths, parent traversal
and links remain rejected.

Extraction runs in a blocking worker rather than on the async executor. Cancellation is
cooperative: the worker checks cancellation and the original deadline at each entry and read
boundary. A blocked OS read or write cannot be interrupted by those checks; it delays worker
exit and release of the permit and temporary files until the OS call returns. A stopped worker
removes its partial workspace and object file before returning the full reservation. Aborting
the workflow also signals that worker to stop, while the worker retains its cleanup guards
until it exits; cancellation does not guarantee a bounded exit time for blocking I/O.

## Build network posture

`BuildRequest.network` is an explicit per-request posture:

- `deny_all` is enforced for that build through the BuildKit frontend
  `force-network-mode=none`, so no build step can reach a network.
- `restricted` is not enforced per build. It is enforced at the BuildKit
  daemon level as a union allowlist shared by every build, so the
  request-scoped `allowed_registries` list does not narrow the daemon policy.

The current cluster deployment runs BuildKit with open egress. `deny_all`
still forces `network=none` for that build regardless of the deployment mode;
see [cluster internal configuration](../deployment/cluster-internal-configuration.md).

## Fencing, replay and failure behavior

Every executor request binds the command payload, generation or attempt,
deadline and request ID. PostgreSQL stores the highest accepted fence and a
permanent cleanup tombstone. Exact replay returns the stored response; payload
reuse, stale generations and operations after cleanup are rejected. A timeout,
cancel or failed stage enters bounded cleanup and cannot produce a publishable
artifact or Ready environment.

Build and OCI import wait within the original approved overall deadline; the
short control timeout does not shorten that execution budget. Cancellation
reaches the admitted executor before cleanup. BuildKit runs in an owned Tokio
runtime whose async solve and session connections close together. Shutdown bounds
the wait for blocking file or DNS work; thread termination alone is not remote
completion. Cleanup requires
the exact request, generation and stage-request labels in a completed BuildKit
history record; a disconnected client or elapsed deadline alone is insufficient.
Missing, ambiguous or unreachable completion remains unknown and blocks replay
and cleanup. OCI import stops its local publication workflow; any already sent
content-addressed blob or manifest may finish, but its immutable digest is never
deleted by candidate cleanup.

Cleanup has a separate bounded control wait and remains bound to the original
fence after the compute deadline expires. It checks the authoritative command's
repository and candidate tag even when no artifact row was written. A known
primary failure is preserved when cleanup fails, with `cleanupVerified=false`
and no automatic build retry. Terminal late-built commands can recover cleanup
from their exact canonical receipt and artifact, without changing failed or
cancelled state or rebuilding. The historical diagnostic is retained, including
an earlier cleanup failure, while the current cleanup status can become verified.

## Validation

Local tests cover contract validation, deterministic plans, fixed tool
invocation, replay and tombstone behavior. Real BuildKit, registry, scanner and
Kubernetes behavior requires separate execution against those dependencies.
Report the commands actually run and remaining limits in the PR.
