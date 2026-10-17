# Submission Collector v1

This document describes the collector contract. Current implementation and test
status are maintained in the related pull request.

## Accepted input and identity

The internal freeze command must come from an authenticated, approved
`SubmissionManifest` projection and carries:

- required project, optional course, actor, Agent run and non-zero manifest revision;
- canonical `submissionManifestSha256`;
- exact Environment ID/revision, release ID/version, runtime kind, runtime
  artifact SHA-256 and optional Container build request;
- StudentSubmission retention policy identity/revision/deadline;
- one idempotency key and trace ID;
- one Environment-owned source identity.

The project is the authorization and idempotency scope even when no course is
associated. A nullable course must not weaken the database uniqueness constraint.
Distinct freeze requests may legitimately produce identical content hashes;
content identity does not replace request identity.

The current collector accepts `source: workspace`. It does not execute an
`EvaluationSpec`, Runner or Checker and cannot produce a score. Container
identity selects PVC; VirtualMachine identity selects SSH/SFTP. A cross-kind
binding is rejected.

The same Evaluation image exposes only the fixed `--mode freeze-worker`
process for a Job. It reads one deployment-owned configuration file and one
strict immutable command file from absolute mounted paths. PVC collection is
hard-bound to the read-only `/workspace` mount; VM collection accepts only the
bounded SFTP fields below. This mode contains no shell runner, Kubernetes
client, Evaluation Runner, Checker, Aggregator, or scoring entry point.

## PVC source

The collector receives a deployment-mounted read-only workspace root and opens
it as a directory capability. All submitted paths remain normalized relative
paths. Metadata and reads do not intentionally follow the selected final
symlink. Symlinks, devices, sockets, FIFOs, invalid UTF-8 components and any
entry outside the capability fail closed.

## SSH source

The binding requires a private address, port 22, password-locked guest account resolved from the approved runtime UID,
normalized absolute workspace root, pinned host-key blob SHA-256 and absolute
paths to an ephemeral private key and OpenSSH user certificate. At connection
time the certificate must:

- be currently valid and expire no later than the binding deadline;
- have no more than five minutes remaining;
- be a user certificate with principal `labweaver-collector`;
- carry critical option `force-command = internal-sftp -R`.

Only a session channel and SFTP subsystem are opened. Every connect and SFTP
operation has a deadline. No API exposes shell, exec, PTY, forwarding, write,
rename or delete. Host-key mismatch, authentication rejection, expiration and
timeout have distinct stable diagnostics.

The VM image accepts the collector principal through the reviewed public user
CA. The short-lived certificate issuer and ephemeral Secret cleanup are
deployment dependencies, not static repository credentials. Their absence
blocks VM collection.

Environment Service owns the internal binding endpoint protected by a
service-account JWT over TLS. It requires an authorized Evaluation caller,
the current owner, project, optional course and Environment revision, and rejects anything except a running,
current-generation, eligible Environment with a healthy endpoint. VM bindings
are derived from the persisted running KubeVirt observation and receive a
299-second certificate with principal `labweaver-collector` and critical
`force-command=internal-sftp -R`; the configured CA private key must match the
public CA embedded in the reviewed VM provider configuration.

Evaluation Service owns the browser-facing freeze command after Access BFF
authentication and project authorization. The coordinator atomically changes a
queued command to `running`, uses only fixed Kubernetes API operations, and
creates a digest-pinned same-image Job. Container Jobs run in the Environment
namespace with the exact PVC mounted read-only; VM Jobs run in the dedicated
Evaluation namespace with only the short-lived private key/certificate pair.
Both receive an immutable command/config bundle, bounded resources, no Service
Account token, default-deny ingress and explicit infrastructure/DNS/VM egress.
Job, Secret, ConfigMap and NetworkPolicy deletion must read back absent before
the command becomes `completed` or `failed`.

## Selection and limits

`SubmissionManifest.validate` is authoritative for normalized `include`,
`exclude`, `required`, `llmReadable`, manifest byte/file limits and
`followSymlinks: false`. The collector additionally enforces:

| Limit | Default hard bound |
| --- | ---: |
| raw selected bytes | 64 MiB |
| canonical archive bytes | 96 MiB |
| regular files | 10,000 |
| SSH certificate remaining TTL | 300 seconds |

Exact excludes win over includes. A required file or directory must exist in
the selected set. Empty regular files are valid. No limit produces truncation
or partial success.

Preflight and freeze each read and hash the complete selected set. Freeze must
match preflight source identity, sorted file list, total bytes and every file
SHA-256. The output media type is
`application/vnd.labweaver.frozen-submission.v1+json` and the canonical JCS
archive holds sorted path/base64 entries.

Each `FrozenFile.sha256` contains the hash of the bytes from that collection read.
Equal file sizes do not establish equal content. A same-size rewrite invalidates the
previous preflight with `LW_COLLECT_SOURCE_CHANGED`. A completed frozen object remains
unchanged when the user later edits the Work. The file hashes bind the manifest to the
same submission attempt and exact archive object version.

Evaluation consumers also compare the downloaded archive with `contentSha256` and
each decoded file with its pinned `FrozenFile.sha256`. A valid whole-object hash
does not replace the file identity check. Package object reads compare actual bytes
with the pinned `PackageFile.sha256`; equal paths, sizes or media types are insufficient.

## Storage, persistence and event

The object store binding must support bucket versioning and Governance Object
Lock. Upload uses one attempt-specific key, exact SHA-256 checksum,
`If-None-Match: *`, Governance mode and the frozen `retainUntil`. Success
requires an exact non-null version and verified HEAD plus byte read-back.

`evaluation.submission_freeze_requests` owns one stable ID per project and
idempotency key. Course is optional teaching context and does not change that
identity. `submission_freeze_attempts` retains every fenced attempt and failure
diagnostic. A completed attempt, authoritative `frozen_submissions`
row and v2 Outbox event are committed in one transaction. Exact replay returns
the stored contract; a different request under the same key conflicts.

The v2 event data includes the complete `FrozenSubmission` and
`sourceIdentitySha256`. Its object version, object SHA-256 and file manifest
must therefore equal the database JSON and Object Lock version.

## Internal model-input facts

Control may read ledger metadata through
`GET /internal/v1/evaluation-releases/{releaseId}/frozen-submissions/{submissionId}/model-input-facts`.
The route uses the existing `evaluation.control.invoke` service permission. It requires
`x-labweaver-project-id`, `x-labweaver-actor-id` (the frozen owner selector), optional
`x-labweaver-course-id`, and `If-Match` for the exact Evaluation release revision.
The owner selector does not establish the requesting user's object permissions.

The response contains `frozenSubmissionId`, `projectId`, `courseId`, `ownerActorId`,
`manifestRevision`, canonical `manifestSha256`, `manifest`, the exact `archive` reference,
`contentSha256`, `files`, `environment`, `evaluationRelease` (`id`, `revision`, `state`),
and `inputAuthorizationContext`. It omits object keys, download URLs, worker placement,
system-fact values, file content and rubric content. The read performs no object-store or
Control request. A missing, incomplete, differently scoped or inconsistent ledger result
is rejected. An active Evaluation release in the same scope is metadata; Control must still
verify the original publication admission linking that release to the Environment release.

`inputAuthorizationContext` consumes the complete, unchanged #216 Context from
`592ac41681b54df19d1651468c444c5965c2961d`: original session digest, actor, project/course,
operation and original decision `validUntil` evidence. Access must construct and overwrite
`x-labweaver-model-input-authorization-context` from its real authorization decision.
Evaluation checks the bounded JSON against the trusted forwarded session and scope, and
stores the first context in the existing command JSON. Replay cannot replace it. Legacy
commands or requests without that upstream evidence return a null context; model consumers
must reject it rather than derive a session or deadline from the actor or service token.

Access supplies this header from the verified original request and authorization decision.
Evaluation persists the first value; it cannot replace it when another session replays the request.
Facts and context are not visibility or egress grants. Hidden or unknown rubric permission
must be rejected before object reads; fresh user/object authorization remains required.

### Frozen advisory and parent task admission

Before archive or rubric reads, Evaluation obtains the original context from its frozen ledger
and calls Control's `POST /internal/v1/authoring/frozen-model-input-authorizations` with the
existing `control.authoring.read` permission. It compares the returned binding with the exact
freeze, manifest, archive/file hashes, Environment and Evaluation releases, separate rubric
package/object/hash and permission reference, policy and selection. It forwards that unchanged
binding in required `InternalAgentLlmReviewRequest.inputAuthorization`.

Control uses the task-limited metadata route
`GET /internal/v1/evaluation-releases/{releaseId}/frozen-submissions/{submissionId}/model-input-facts/tasks/{taskRunId}`.
This route requires the existing `evaluation.control.invoke`, the original scope/owner headers
and If-Match, plus one `x-labweaver-evaluation-lease-sha256` header. The response has exactly the
original fact fields. No raw lease token, worker credentials or content is returned. Evaluation
checks the actual run/step/current attempt, owner/scope/freeze/release, durable cancellation,
current database lease digest and expiry, and original deadline. Unknown or inactive parents
are refused. The ordinary metadata route cannot substitute for task admission.

`parentLeaseSha256` uses the shared hash function on the actual fenced Evaluation lease token.
The first request and selection digest retain it. Worker lease rotation invalidates the old
binding even when taskRunId is unchanged. Recovery can reconcile an existing Agent receipt
and its observed usage; it cannot refresh the old binding to permit another send.

Before a full review request exists, immutable attempt creation time plus the existing five-minute
window fixes the deadline. After the intent is stored, its original deadline is reused. Rechecks
do not extend it. The current parent is checked before each content read, after preparation and
before a missing Agent receipt permits sending. Initial refusal causes zero reads and sends;
revocation during preparation prevents subsequent actions but cannot undo completed reads.
Agent's Control rechecks before its first model call and every repair use the same parent path,
so they do not depend on the cancellation RPC arriving first. Original content, context and
deadline remain in the existing execution-intent JSON; response loss does not cause a new read
or a duplicate create when the original receipt already exists.

## Stable failure diagnostics

The implementation uses payload-free codes including:

- `LW_COLLECT_PATH_UNSAFE`, `LW_COLLECT_SYMLINK_REJECTED`,
  `LW_COLLECT_REQUIRED_PATH_MISSING`;
- `LW_COLLECT_FILE_LIMIT_EXCEEDED`, `LW_COLLECT_BYTE_LIMIT_EXCEEDED`,
  `LW_COLLECT_OUTPUT_LIMIT_EXCEEDED`;
- `LW_COLLECT_SOURCE_CHANGED`, `LW_COLLECT_SOURCE_IDENTITY_MISMATCH`;
- `LW_COLLECT_SSH_CREDENTIAL_INVALID`, `LW_COLLECT_SSH_HOST_KEY_MISMATCH`,
  `LW_COLLECT_SSH_TIMEOUT`;
- `LW_OBJECT_UPLOAD_FAILED`, `LW_OBJECT_VERSIONING_REQUIRED`,
  `LW_OBJECT_LOCK_REQUIRED`, `LW_OBJECT_LOCK_IDENTITY_MISMATCH`;
- `LW_COLLECT_IDEMPOTENCY_CONFLICT`, `LW_COLLECT_FENCE_LOST`,
  `LW_COLLECT_DATABASE_FAILED`.

Failures create neither a `FrozenSubmission` nor a publication event. An
ambiguous locked orphan is retained but is not referenced by a publishable
contract.
