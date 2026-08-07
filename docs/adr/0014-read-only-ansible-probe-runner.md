# ADR 0014: Read-only Ansible Probe Runner for Linux Nginx

Status: proposed and implemented locally for Issue #141; requires A+B Runner,
security and scoring review plus D same-build Kubernetes Verify before
acceptance.

## Context

Issue #141 introduces the deterministic Linux system-lab path: execute the
approved `linux-nginx-probe-v1` profile against a real KubeVirt VM through a
read-only, allowlisted Ansible probe and produce bounded typed evidence with a
deterministic pass/fail result. VM mutation, arbitrary shell, SSH credential
disclosure, container substitution and static-report evidence are explicit
non-goals.

The public contract already freezes
`contracts::evaluation::DeterministicRunnerSpec::AnsibleProbe`: a
`playbook_profile`, a module allowlist drawn from the frozen v1 set
(`ansible.builtin.package_facts`, `ansible.builtin.service_facts`,
`ansible.builtin.stat`), `read_only = true`, and a list of `FactAssertion`.
The Linux material contract reserves the observation surface: host
reachability, Nginx installation and observed version, default-site
configuration and document root, and systemd state. TCP/80 and HTTP response
checks are outside the v1 allowlist and remain a documented blocker, not a
reason to widen the set in this change.

Issue #123 owns the public EvaluationRun, StepRun, attempt, Outbox and
PostgreSQL lifecycle. That contract is not present on the current `develop`
baseline. This decision therefore defines the internal execution boundary that
#123 may call; it does not duplicate or silently replace #123's public API,
event or migration work.

## Decision

Evaluation accepts one strict internal `AnsibleProbeExecutionRequest`
(schema `evaluation.labweaver.io/ansible-probe-execution/v1`). It binds UUIDv7
Run, StepRun and attempt identities, one digest-pinned runner image, the
approved playbook profile, the requested module allowlist, `read_only = true`,
the ordered assertions, one probe target and the SSH identity. The target must
be a private IPv4 address on TCP/22 with a locked non-root username. The SSH
identity references a private-key Secret, a short-lived user-certificate
Secret and the expected host-key SHA-256 observed by the environment domain.
Limits bound wall time (never beyond the 300-second certificate TTL), facts
bytes (≤ 4 MiB), output bytes and assertion count. Unknown fields, mutable
images, modules outside the frozen set, duplicate or out-of-range assertions,
non-private targets, port or username violations and out-of-range limits are
rejected before any resource is constructed.

Observed facts form a bounded typed model, not free-form JSON:
`host.reachable`, `service.<name>.active|.state`,
`package.<name>.installed|.version` and
`file.<absolute-path>.exists|.sha256|.mode`. Fact count, string length and
path length are capped; duplicates, unknown families, type mismatches,
malformed checksums and traversal-bearing paths fail as
`LW_AP_FACTS_MALFORMED`. `evaluate_assertions` is a pure function: an unknown
fact or a value whose JSON type differs from the expectation never passes, so
observation gaps degrade to closed failures instead of optimistic passes.
Full evidence and the payload-free termination receipt both re-validate
against the request; a terminal status produced without trustworthy execution
must carry empty facts and all-unknown assertion results, otherwise the
evidence is rejected as forged.

Ansible installs and reads back a permanent namespace-wide ingress-and-egress
default-deny NetworkPolicy (`ansible-probe-default-deny`) before any probe
Job can exist. The executor refuses to start when that exact policy is
missing or permits any traffic. The attempt-scoped policy is defense in
depth: it denies everything and allows egress only to the target VM address
on TCP/22. The executor then creates exactly one attempt-scoped immutable
ConfigMap holding the validated request, the attempt NetworkPolicy and the
Job. Full request SHA-256 identity is stored in annotations for idempotent
start. The Job uses `backoffLimit: 0`, disables service-account token
mounting, runs as UID/GID 65532 with a read-only root filesystem,
`RuntimeDefault` seccomp and all capabilities dropped, mounts the SSH private
key and user certificate read-only from their Secrets, and mounts writable
`/work` and `/evidence` emptyDir volumes with explicit size limits.

The worker runs only the pinned profile playbook at
`/opt/labweaver/probe/linux-nginx-probe-v1/playbook.yml`; any other profile
name fails closed. The playbook runs only the three allowlisted modules
against the VM and stats only the approved material paths
(`/etc/nginx/nginx.conf`, `/etc/nginx/sites-available/default`,
`/srv/labweaver-nginx-lab` and its `index.html`). No user-controlled value is
ever concatenated into a command line. The worker invokes `ansible-playbook`
with the JSON stdout callback, maps bounded module results into the typed
fact model in Rust, evaluates assertions deterministically, writes canonical
evidence with create-new semantics and leaves only the bounded receipt in the
termination message. Certificate expiry, unreachable host, host-key mismatch,
wall timeout, output overflow and malformed facts map to stable
`LW_AP_*` terminal statuses. A non-Linux worker fails closed instead of
executing.

Start is idempotent only when existing resources carry the exact attempt
labels and request-hash annotation; a partial bundle is removed before
recreation. Cancel and terminal cleanup delete only the attempt Job,
NetworkPolicy and ConfigMap with foreground propagation plus UID and
resourceVersion preconditions, then verify absence. Namespace deletion is
never part of this executor.

## Alternatives considered

- An in-process russh exec channel instead of Ansible was rejected because it
  re-implements the approved module semantics by hand and blurs the frozen
  allowlist boundary; the existing russh client stays the SFTP-only collector
  path.
- Parameterizing the playbook with caller-supplied extra-vars was rejected
  because any dynamic input weakens the approved-profile binding; observation
  requests outside the profile surface fail closed as unknown facts.
- Routing probe SSH through the access gateway was rejected because
  evaluation traffic must stay on the attempt-scoped network path directly to
  the VM, identical to the freeze collector.
- Widening the v1 allowlist for TCP/80 or HTTP checks was rejected as a
  separate reviewed decision reserved by the material contract.

## Security and data implications

Facts are bounded typed metadata: booleans, version and mode strings, and
SHA-256 digests. No file contents, command output, raw logs or student
payloads enter facts, evidence, the receipt or diagnostics. The SSH private
key and short-lived certificate exist only as read-only Secret mounts inside
the attempt pod; a stale certificate fails closed. The host key is pinned by
the SHA-256 observed and persisted by the environment domain. The dedicated
runtime image is digest-pinned, built twice and scanned in the platform-image
workflow with its Trivy JSON retained as a CI artifact.

This boundary grants no RBAC, creates no namespace and provisions no VM.
Deployment must provide the least-privilege executor identity and the
policy-enforced namespace. Any request for privileged mode, HostPath, host
networking, Kubernetes credentials or additional egress requires a separate
reviewed decision.

## Compatibility, evidence and rollback

This change adds no public REST API, NATS subject, JSON Schema, database
Migration or public `contracts` type. #123 must bind its authoritative
attempt and immutable artifact lifecycle to this internal request and receipt
without weakening their identity checks.

Local E1 evidence covers strict request validation, deterministic assertion
evaluation, evidence and receipt closure, NetworkPolicy and Job planning,
idempotent-start conflict handling, worker fact mapping, and timeout,
overflow, malformed-fact and stale-identity negatives. Local tests do not
prove a real runner image, Kubernetes scheduling, NetworkPolicy enforcement,
real VM reachability, certificate issuance or residue-free cleanup. The
positive Nginx path and the stopped-service and site-mismatch negative paths
require D Verify with the same image and source identity on a connected
cluster.

Before connected use, rollback removes this internal module, the dedicated
image workflow entry, the version-lock entry and the namespace policy
reconcile block. After attempts exist, rollback disables new starts, retains
immutable evidence under the approved policy, cancels or cleans only exact
attempt resources, and never rewrites a terminal result or deletes a shared
resource.
