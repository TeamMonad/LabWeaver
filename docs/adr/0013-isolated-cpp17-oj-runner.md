# ADR 0013: Isolated C++17 OJ Runner

Status: proposed and implemented locally for Issue #140; requires A+B Runner,
Checker, Aggregator and security review plus D same-build Kubernetes Verify
before acceptance.

## Context

Issue #140 introduces the first deterministic execution path after immutable
submission freeze. It must compile an approved C++17 source, execute private
test cases, classify terminal outcomes, aggregate scores and retain immutable
evidence without executing a teacher- or student-provided command. A mutable
image tag, host execution, network egress, privileged container, incomplete
case set, forged case score or public private-test payload must fail closed.

Issue #123 owns the public EvaluationRun, StepRun, attempt, Outbox and
PostgreSQL lifecycle. That contract is not present on the current `develop`
baseline. This decision therefore defines the internal execution boundary that
#123 may call; it does not duplicate or silently replace #123's public API,
event or migration work.

## Decision

Evaluation accepts one strict internal `OjExecutionRequest`. It binds UUIDv7
Run, StepRun and attempt identities, the immutable submission and evaluator
identities, one digest-pinned `cpp17-approved-v1` worker image, bounded source
and case file hashes and sizes, the public Program phase, exact or
ASCII-whitespace token checking, resolved case weights, the Score-step maximum,
and compile/run resource limits. A compile phase accepts no checker, cases or
score; a test phase requires all three. Unknown fields, duplicate case IDs,
unsafe relative paths, phase/checker mismatch, unapproved profiles, mutable
images and out-of-range limits are rejected before resource construction.

Ansible installs and reads back a permanent namespace-wide ingress-and-egress
default-deny NetworkPolicy before any runner Job can exist. The executor refuses
to start when that exact policy is missing or permits any traffic; the dynamic
attempt policy is defense in depth and is never the first-line isolation
boundary. The executor then creates exactly one attempt-scoped immutable
ConfigMap, additional default-deny NetworkPolicy and Job. Full request SHA-256
identity is stored in annotations, while every label value remains within the
Kubernetes 63-character limit. The Job uses a fixed ServiceAccount and image
pull secret, disables service-account token mounting, sets `backoffLimit: 0` and
a calculated active deadline, runs as UID/GID 65532 with `RuntimeDefault`
seccomp, drops all capabilities, denies privilege escalation, uses a read-only
root filesystem and mounts the submission and private evaluator PVCs read-only.
Compile-phase Jobs omit the evaluator binding entirely; only test-phase Jobs may
mount it. Only attempt evidence and bounded `/work` storage are writable. CPU,
memory, ephemeral storage, output and wall-time limits are explicit. The pod
receives no shell command or network exception.

The worker reads files through the existing no-follow PVC capability, verifies
each declared size and SHA-256, and invokes only fixed absolute binaries. It
enters a fail-closed compiler Landlock domain before compiling with:

```text
/usr/bin/g++ -std=c++17 -O2 -pipe -fno-diagnostics-color -o /work/submission -- /work/submission.cpp
```

The compiler domain may read only the fixed toolchain/system paths and may
write only `/work`. It cannot read the command, submission or evaluator mounts,
so preprocessor includes, assembler `.incbin` directives and linker inputs
cannot exfiltrate private tests into the executable.

Each test process is entered through a fixed internal helper that applies
`RLIMIT_AS`, `RLIMIT_CPU`, `RLIMIT_FSIZE`, `RLIMIT_NPROC=64` and a zero core-dump
limit after verifying that the current cgroup v2 `pids.max` is finite and no
greater than 128. An absent, unlimited or broader Pod PID ceiling fails closed.
The helper then applies a fail-closed Landlock v3 filesystem policy and a
submission-only seccomp filter. The filter rejects `setsid`, `setpgid`,
`unshare`, `setns`, namespace-bearing `clone` calls and direct `clone3`, so
student descendants cannot leave the process group that the worker kills after
every case. The child may read only its binary, required runtime libraries and a
small fixed set of system data/device files. In particular it cannot read the
command ConfigMap, submission source, evaluator mount, evidence mount or
arbitrary `/work` files. A kernel without full Landlock, seccomp and bounded
cgroup PID enforcement rejects execution. The coordinator applies an
independent Job deadline and container resource ceiling. A measured wall
timeout or `SIGXCPU` is TLE; unattributed `SIGKILL` is RuntimeError, while Pod
`OOMKilled` is reported as the stable memory-limit diagnostic. Stdout and stderr
are drained concurrently under one combined byte ceiling. Runtime result
classification is deterministic: output limit, time limit, memory limit,
runtime error, wrong answer or accepted. Compile failure is a separate terminal
state. No LLM participates in checking, aggregation or scoring.

Aggregation requires exactly one evidence item for every requested case.
Duplicate, missing, unknown or point-forged evidence is rejected. A passing
case receives its contract points; every other case receives zero. The first
non-passing case in request order determines the aggregate terminal status,
making replay independent of map or event ordering. The checked sum of passed
case weights is normalized to the public Score-step maximum with
`floor(passedWeight * scoreMax / totalWeight)`; a fully passing step therefore
receives exactly `scoreMax`, while overflow or a zero denominator fails closed.
Issue #123 must resolve approved `TestGroup` sources to these immutable case
bindings without changing their reviewed total weight.

Full canonical evidence contains only identities, hashes, byte counts, bounded
resource observations, stable diagnostics and deterministic scores. It is
written with create-new semantics to the attempt evidence volume. The
termination message contains only a bounded receipt with the full-evidence
hash, status and score. The student projection contains only status, stable
diagnostic, score and case counts; it cannot expose private input, expected
output, the compile command or raw stdout/stderr.

Start is idempotent only when all three existing resources have the exact
attempt labels and full request-hash annotation. A partial bundle is removed
before recreation. Apply failure triggers exact cleanup. Cancel and terminal
cleanup delete only the attempt Job, NetworkPolicy and ConfigMap using
foreground propagation plus the observed UID and resourceVersion preconditions,
then verify absence. A replacement race returns an identity conflict instead of
deleting the new object. Cancellation returns `CleanupPending` until that
absence readback completes and only then returns `Cancelled` for #123 to persist.
Namespace and PVC deletion are never part of this executor. Ambiguous ownership,
multiple Pods, invalid receipts, cleanup residue, API failure, worker OOM or Job
deadline failure remains a blocking diagnostic.

## Alternatives considered

- Host compilation or a service-process child was rejected because it crosses
  the tenant and resource-isolation boundary.
- Shell scripts and teacher-provided compiler flags were rejected because they
  turn approved evaluation data into commands.
- A mutable GCC tag was rejected because an attempt could not prove or replay
  its toolchain identity.
- Network access with an allowlist was rejected because C++17 compile and test
  execution require no network dependency.
- Special, floating-point and LLM checkers were rejected as Issue #140
  non-goals.
- Best-effort partial scoring was rejected because missing or duplicate
  evidence would make retries and delivery order affect the result.

## Security and data implications

Private test bytes stay on the read-only evaluator volume and in bounded worker
memory. Neither full evidence, termination receipt, stable diagnostics nor the
student projection contains input, expected output, command text or raw log
payload. Hashes remain sensitive metadata and must use Evaluation-owned
authorization and retention policy when #123 publishes the attempt.

The dedicated base runtime is digest-pinned in `deploy/versions.lock.yml`; the
complete worker is built twice and scanned in the platform-image workflow. The
OJ image fails on any High or Critical vulnerability or secret, and its Trivy
JSON is retained as a CI artifact. The workflow result, not the base lock entry,
is the vulnerability gate. The executor requires the approved request
toolchain digest and actual promoted worker-image digest to match.

This boundary does not grant RBAC, create the runner namespace or provision
PVCs. Deployment must provide a least-privilege ServiceAccount that can read no
student resource and a policy-enforced namespace. Any request to add
privileged mode, HostPath, host networking, Kubernetes credentials or egress
requires a separate reviewed decision.

## Compatibility, evidence and rollback

This change adds no public REST API, NATS subject, JSON Schema, database
Migration or public `contracts` type. #123 must bind its authoritative attempt
and immutable artifact lifecycle to this internal request and receipt without
weakening their identity checks.

Local E1 evidence covers strict request validation, exact/token checking,
deterministic aggregation, all required terminal classifications, forged and
incomplete evidence rejection, fixed image/version/workflow identity, Job
security context, default-deny networking, resource ceilings and exact cleanup
scope. Local compilation and unit tests do not prove a real compiler image,
Kubernetes scheduling, cgroup OOM behavior, NetworkPolicy enforcement,
cancellation race, immutable evidence publication, Landlock denial or
residue-free process/resource cleanup. Those require D Verify with the same
image and source identity after #123 provides the authoritative attempt path.

Before connected use, rollback removes this internal module, dedicated image
workflow entry and version lock. After attempts exist, rollback disables new
starts, lets Evaluation retain full evidence under its approved policy, cancels
or cleans only the exact attempt resources, and never rewrites a terminal
score or deletes an immutable submission/evaluator artifact.
