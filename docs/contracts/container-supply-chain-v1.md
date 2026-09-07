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

## Fencing, replay and failure behavior

Every executor request binds the command payload, generation or attempt,
deadline and request ID. PostgreSQL stores the highest accepted fence and a
permanent cleanup tombstone. Exact replay returns the stored response; payload
reuse, stale generations and operations after cleanup are rejected. A timeout,
cancel or failed stage enters bounded cleanup and cannot produce a publishable
artifact or Ready environment.

## Validation

Local tests cover contract validation, deterministic plans, fixed tool
invocation, replay and tombstone behavior. Real BuildKit, registry, scanner and
Kubernetes behavior requires separate execution against those dependencies.
Report the commands actually run and remaining limits in the PR.
