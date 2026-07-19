# ADR 0010: Immutable PVC and Certificate-bound SSH Collector

Status: proposed and implemented locally for Issue #54; requires A+B contract
and security review plus D same-build Verify before acceptance.

## Context

A workspace is mutable and cannot be scored directly. Collection must bind one
approved `SubmissionManifest`, actor, Agent run, Environment revision, runtime
artifact, retention policy and source identity to immutable bytes. Container
workspaces are available through a read-only PVC mount. VM workspaces are
available only over the private SSH path. Neither path may follow links, escape
its root, expose arbitrary shell access, silently truncate output or publish a
partial result.

PostgreSQL and S3-compatible Object Lock cannot share one atomic transaction.
The design must therefore distinguish an inaccessible retained orphan from a
publishable `FrozenSubmission`, fence concurrent workers and preserve failed
attempts without weakening retention.

## Decision

Evaluation owns one runtime-neutral snapshot engine. A `SnapshotSource`
provides only no-follow metadata, bounded directory enumeration and bounded
regular-file reads. The PVC implementation opens the mounted workspace once as
a `cap-std` directory capability. The VM implementation accepts only a private
IP, port 22, pinned host-key SHA-256 and an OpenSSH user certificate whose
principal is `labweaver-collector`, validity is at most five minutes, and
critical `force-command` is exactly `internal-sftp -R`. It opens an SFTP
subsystem and exposes no shell, exec, PTY, forwarding or write operation.

The engine validates `include`, `exclude`, `required`, relative-path and
symlink rules before reading. Deployment-owned limits additionally cap raw
input at 64 MiB, canonical output at 96 MiB and file count at 10,000 even when
the approved manifest allows more. Preflight records the sorted file path,
size and SHA-256 list. Freeze rereads every selected file from the same source
identity and rejects any difference. The canonical v1 archive contains sorted
paths and base64 bytes; file identities and archive bytes use SHA-256.

Each idempotency key owns one stable `FrozenSubmissionId` and append-only
attempt rows. A database-clock lease fences the active worker. Expired attempts
become failed records before a new attempt starts. The object key includes the
stable ID and attempt. Evaluation uploads the exact archive with `If-None-Match:
*`, Governance mode and the frozen retention deadline, then requires a non-null
version and exact HEAD/read-back match for size, media type, SHA-256, retention
mode and deadline. Only after that verification does one PostgreSQL transaction
write `frozen_submissions`, complete the attempt and enqueue
`labweaver.evaluation.submission.frozen.v1` containing the same object version,
hash and full immutable contract.

### Sprint 2 deployment topology

The Evaluation Service runs a freeze-only coordinator in
`labweaver-system`. It consumes the durable v1 freeze command, reserves the
fenced attempt, and creates an allowlisted Kubernetes Job from the same
Evaluation Service image in `--mode freeze-worker`. The worker has no
Kubernetes API token, no shell command input and no Evaluation Runner, Checker,
Aggregator or scoring code path. Its command is a strict immutable ConfigMap;
its database, object-store and NATS credentials are mounted from the existing
Evaluation Secret in the same namespace. The coordinator owns bounded Job,
ConfigMap and one-time Secret cleanup and treats residue as a blocking failure.

For Container environments, Sprint 2 workspaces use the reviewed `nfs-rwx`
storage binding. The coordinator resolves the exact bound PVC and PV by
environment identity, accepts only the expected NFS CSI driver and server/path
shape, and mounts that same NFS export read-only into the worker. It never
mounts a HostPath and never copies Evaluation credentials into a student
namespace. The worker opens the mount as a `cap-std` capability before
collection.

For KubeVirt environments, the coordinator generates an ephemeral user key and
asks the Environment owner over mTLS to sign its public key for one exact
running environment. Environment verifies course, owner, revision, runtime
generation, current private guest address and pinned host key, then issues a
certificate valid for at most five minutes with principal
`labweaver-collector` and critical `force-command=internal-sftp -R`. The key and
certificate live only in a run-scoped Secret in `labweaver-system`; the worker
removes no credential itself and coordinator cleanup is mandatory.

The coordinator has cluster read-only access to the exact PVC/PV discovery
resources and namespace-local mutation access only to its Job, ConfigMap and
Secret resources. This is distinct from the accepted broad runtime-executor
ClusterRoles. No student-controlled field becomes a Kubernetes resource name,
NFS server/path, network destination or credential locator without an
Environment-owned identity resolution.

An upload error, ambiguous retained orphan, database failure or lost fence does
not create a publishable row or event. Failed attempts retain a payload-free
diagnostic and cleanup status. Governance-locked bytes are never mutated or
deleted early; a retry uses a new attempt key and object version.

## Alternatives considered

- Reading the live workspace during scoring was rejected because it breaks
  reproducibility and permits time-of-check/time-of-use changes.
- Tar through shell or SSH exec was rejected because it accepts command
  injection, shell configuration and unbounded output.
- Long-lived SSH keys or a collector certificate without read-only forced
  command were rejected because stolen credentials could open an interactive
  session or modify the submission.
- Upload followed by an un-fenced best-effort database write was rejected
  because duplicates and ambiguous commit failures can publish divergent
  identities.
- Deleting a Governance-locked orphan was rejected because bypassing retention
  would invalidate the security model; such objects remain inaccessible and
  explicitly non-publishable until policy disposal.

## Security and data implications

No diagnostic, log, event or attempt row contains student file contents,
private keys or certificate bytes. The database stores only hashes, object
identity, the immutable public contract and stable diagnostics. Canonical
archive bytes exist only in bounded worker memory and the retained object.
`llmReadable` remains an allowlist candidate and does not make the archive
eligible for LLM egress.

The VM cloud-init principal file now permits both `labweaver-gateway` and
`labweaver-collector`; the latter remains safe only when the issuer enforces
the short-lived read-only certificate conditions above. Principal enrollment
does not itself issue a credential. Deployment Secret lifecycle and a real
single-Environment issuer are mandatory before VM acceptance.

## Compatibility, rollback and evidence

The destructive reset applies `evaluation/0001_sprint2_baseline.sql`, including
the corrected freeze uniqueness rule that distinguishes independent approved
freezes with identical bytes. No pre-reset Evaluation data upgrade is
supported. Empty regular files are valid and remain hash-addressed.

Before publication, rollback is whole-PR reversion. After Object Lock upload,
rollback disables new collection, retains request/attempt rows and locked
objects through their policy deadline, and never rewrites a completed
`FrozenSubmission`.

Local evidence covers PVC adversarial paths and deterministic archives, SSH
configuration/credential guards, PostgreSQL idempotency and Outbox consistency,
and a MinIO Object Lock integration test. This is E1 plus test-defined E2; it is
not E3. E3 requires the same build against a real read-only PVC, real
KubeVirt VM, private network, real short-lived certificate issuer and MinIO
bucket, including credential expiry, SSH denial/timeout, retained-orphan and
cleanup evidence.
