# Submission Collector v1

Status: implemented locally for Issue #54; proposed ADR 0010, A+B security
review, D Verify and connected E3 evidence remain required.

## Accepted input and identity

The internal freeze command must come from an authenticated, approved
`SubmissionManifest` projection and carries:

- course, actor, Agent run and non-zero manifest revision;
- canonical `submissionManifestSha256`;
- exact Environment ID/revision, release ID/version, runtime kind, runtime
  artifact SHA-256 and optional Container build request;
- StudentSubmission retention policy identity/revision/deadline;
- one idempotency key and trace ID;
- one Environment-owned source identity.

The current collector accepts `source: workspace`. It does not execute an
`EvaluationSpec`, Runner or Checker and cannot produce a score. Container
identity selects PVC; VirtualMachine identity selects SSH/SFTP. A cross-kind
binding is rejected.

## PVC source

The collector receives a deployment-mounted read-only workspace root and opens
it as a directory capability. All submitted paths remain normalized relative
paths. Metadata and reads do not intentionally follow the selected final
symlink. Symlinks, devices, sockets, FIFOs, invalid UTF-8 components and any
entry outside the capability fail closed.

## SSH source

The binding requires a private address, port 22, locked non-root guest user,
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
blocks VM E3.

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

## Storage, persistence and event

The object store binding must support bucket versioning and Governance Object
Lock. Upload uses one attempt-specific key, exact SHA-256 checksum,
`If-None-Match: *`, Governance mode and the frozen `retainUntil`. Success
requires an exact non-null version and verified HEAD plus byte read-back.

`evaluation.submission_freeze_requests` owns one stable ID per course and
idempotency key. `submission_freeze_attempts` retains every fenced attempt and
failure diagnostic. A completed attempt, authoritative `frozen_submissions`
row and v2 Outbox event are committed in one transaction. Exact replay returns
the stored contract; a different request under the same key conflicts.

The v2 event data includes the complete `FrozenSubmission` and
`sourceIdentitySha256`. Its object version, object SHA-256 and file manifest
must therefore equal the database JSON and Object Lock version.

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
