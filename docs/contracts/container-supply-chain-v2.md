# Container Supply Chain and Runtime v2

Status: implemented locally for Issue #52; A+B human review and D Verify are
required. Connected BuildKit, Harbor, Trivy, Private Sigstore and Kubernetes
evidence is not recorded, so this document does not claim E3 completion.

## Scope and ownership

Control turns an approved Container candidate into one immutable `BuildRequest`.
Agent owns build execution, terminal state, `ImageArtifact` and
`ImagePolicyEvaluation`. Environment owns the immutable release projection and
the lifecycle of the resulting Kubernetes namespace. Deployment-owned executors
perform restricted BuildKit/registry/signing and Kubernetes API operations; the
services select them only through exact configured NATS subjects and bindings.

The implementation does not add a microservice, Runner type, public endpoint,
scoring path or unapproved shell execution. A tag, provider registration order,
LLM output or executor response cannot replace an authoritative digest-bound
record.

## Versioned messages

| Subject | Producer to consumer | Durable behavior |
| --- | --- | --- |
| `labweaver.control.agent_build.requested.v2` | Control to Agent | Full approved `BuildRequest`, `CandidateApproval`, idempotency key and command hash. Agent records Inbox before ACK. |
| `labweaver.agent.build.completed.v2` | Agent to Control | Build/artifact identity plus canonical artifact and policy hashes. Control reads the full authoritative Agent artifact over mTLS before projection. |
| `labweaver.agent.build.failed.v2` | Agent to Control | Stable terminal diagnostic plus retryable and cleanup-verification flags. |
| `labweaver.control.environment_template_release.published.v2` | Control to Environment | Full immutable release, approved `EnvironmentSpec` and canonical projection hash. Environment records an immutable projection before ACK. |
| `labweaver.control.environment_template_release.withdrawn.v1` | Control to Environment | Append-only aggregate sequence 2 withdrawal. Environment records it against the exact projected release before ACK. |

Invalid messages are published to the configured private quarantine subject and
then acknowledged. Duplicate deliveries are acknowledged after durable Inbox
deduplication. Gaps and retryable persistence failures are negatively
acknowledged. Event type must equal the exact subject; v1 payloads are not
silently upgraded to v2.

## Build pipeline

The Agent derives one canonical build identity from the command and verifies it
at every stage:

1. ensure the exact `course-<course UUID>` Harbor project is private, has a
   non-zero quota and exposes only the expected per-project pull robot identity;
2. build the approved immutable context using the exact restricted BuildKit
   binding and fixed base digest;
3. require digest-bound SBOM and in-toto provenance object references;
4. scan the same digest with the configured Trivy version and database hash;
5. reject any Critical finding and retain High findings as warnings;
6. sign and verify with the exact private Fulcio issuer, workload subject, trust
   bundle, non-empty certificate/signature hashes, signed subject digest, Rekor
   inclusion and SCT evidence within the configured freshness window;
7. publish the same digest under a non-`latest` immutable tag;
8. atomically persist the artifact, evaluation and terminal Outbox event.

Stage timeout, request deadline and cancellation are bounded. Any failure after
admission invokes cleanup. A build can retry only when the failure is marked
retryable and cleanup was verified. Cleanup failure is terminal and prevents an
artifact or successful completion event.

The deployment-owned executor request/reply protocol is carried on the exact
`build.provider_subject` from
`deploy/config/agent-control-plane.yaml.example`. Every request carries protocol
version, BuildRequest ID, monotonically increasing database attempt, lease token,
stage, deterministic stage request ID and deadline. Each reply repeats the
protocol version, BuildRequest ID, attempt, stage and request ID as well as the
stage-specific build identity and digest; a mismatch is rejected. The executor
must durably reject an attempt lower than the highest generation already seen.
Cleanup records a generation-scoped tombstone that wins over every late stage
completion, while a newer generation cannot be removed by an older cleanup.
The executor must map approved
`ArtifactRef` inputs to restricted BuildKit, Harbor/Trivy and Private Sigstore;
the service does not accept raw paths, credentials or fallback providers.

## Container runtime projection

For a verified Container release, Environment deterministically produces:

- one namespace, ResourceQuota, LimitRange and ServiceAccount with token
  automount disabled and the exact same-namespace Harbor imagePullSecret; the
  namespace carries the controlled cleanup finalizer;
- one workspace PVC, default-deny NetworkPolicy and explicit protected Gateway
  ingress policy;
- one non-root Deployment using only `repository@sha256:digest`, read-only root
  filesystem, dropped Linux capabilities, `RuntimeDefault` seccomp and the PVC
  mounted at `/workspace`;
- one ClusterIP Service and HTTPRoute whose parent is the exact configured
  internal Gateway namespace, name and section. The route uses a deterministic
  environment-specific path and the ingress policy selects only that Gateway's
  data-plane pods.

No Ingress or public Service is generated. Public exposure in
`EnvironmentSpec` is rejected. Stop, start, restart, observe and namespace
cleanup use the same deterministic plan hash. Cleanup does not require the
release projection to remain available; it deletes the deterministic namespace
and requires immutable cleanup evidence before the lifecycle reaches `Deleted`.
The deployment-owned executor must first remove the HTTPRoute, Service and
workload, verify that no endpoint remains reachable, clear the controlled
namespace finalizer, and then verify namespace absence before returning that
evidence.

Every remote Kubernetes request also carries the exact reconcile action,
`operationId`, `providerStep`, environment generation, attempt, deterministic
request ID and deadline. The executor must persist the highest accepted
generation/operation/step tuple, make repeats of the same request ID idempotent,
and write a namespace tombstone before Delete returns. A late Apply, Scale,
Restart or Observe older than that tuple or tombstone is rejected without a side
effect.

The provider binding file is
`deploy/config/environment-providers.json.example`. `providerKind: "container"`
selects this implementation; the subject is an exact deployment-owned
Kubernetes executor subject. Gateway and imagePullSecret fields are mandatory
for this provider. The active policy revision, trust revision and trust-bundle
SHA-256 are also mandatory and are compared again on every runtime use. These
fields are forbidden for the legacy remote provider. The executor
materializes the named per-course pull Secret from its reviewed credential
locator; credentials never enter a release event, resource plan or log.

## Configuration and failure boundary

The Environment release consumer additionally requires:

```text
LABWEAVER_ENVIRONMENT_RELEASE_STREAM
LABWEAVER_ENVIRONMENT_RELEASE_CONSUMER
LABWEAVER_ENVIRONMENT_RELEASE_QUARANTINE_SUBJECT
```

All three name deployment-owned JetStream resources; startup does not create or
select a wildcard stream. The durable release consumer must use exactly two
filter subjects: published v2 and withdrawn v1.
`LABWEAVER_ENVIRONMENT_PROVIDER_BINDINGS_PATH` must be an absolute path to the
reviewed JSON binding file.

The release resolver reads the projection, withdrawal state and PostgreSQL
authority clock in one statement. Provision, Observe, Start, Restart, Reset,
Retry and Recover paths fail closed when evidence is expired, the release is
withdrawn, or the active policy/trust identity differs. Existing running
instances are not stopped implicitly; Stop and Cleanup remain available so the
owner can remove access and resources safely. A withdrawn or stale release can
never create a new Ready endpoint.

PostgreSQL migrations add the Control BuildRequest-to-candidate projection,
Agent build command state and Environment release projections. Records are
immutable or fenced by leases, and terminal artifact, policy and Outbox writes
are atomic. Control accepts a completed artifact only for the exact persisted
course, candidate revision, candidate hash, approval and BuildRequest identity;
the synchronous API readback fallback is forbidden. Rollback before publication
is whole-change reversion. Once an artifact or release exists, remediation is
cleanup or withdrawal followed by a new immutable build/release, never mutation
of the old identity.

## Evidence and remaining gate

Current local evidence covers deterministic identity, High/Critical policy,
issuer and publication-digest mismatch, deadline/cancellation cleanup,
PostgreSQL lease heartbeat and live cancellation with one terminal Outbox,
complete Migration application, deterministic Kubernetes plans, protected
routing, stable endpoint projection and cleanup. E3 still requires one
connected, same-build replay against the
reviewed BuildKit, Harbor, Trivy, Private Sigstore and Kubernetes executors,
including timeout/cancel/retry/cleanup and proof that no failed run leaves an
accessible namespace, route or image.
