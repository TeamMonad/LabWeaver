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
