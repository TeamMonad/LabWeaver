# Issue #48 Control-plane deployment

## Configuration boundary

Control and Agent read one reviewed YAML document from
`LABWEAVER_CONTROL_CONFIG_FILE` and `LABWEAVER_AGENT_CONFIG_FILE`. The repository examples are
`deploy/config/control-plane.yaml.example` and
`deploy/config/agent-control-plane.yaml.example`. Database passwords, object-store credentials,
NATS credentials and private keys are file locators; secret values do not belong in YAML, logs,
reports or Git.

The Claude Code worker has one deployment owned provider binding that uses only the
three generic Anthropic fields. Reviewed ConfigMap files supply the base URL from
`agent-service-config/anthropic-base-url` and the model from
`agent-service-config/anthropic-model`; the operator-provided auth token is mounted as
`agent-service-secrets/anthropic-auth-token`. The env-cleared child receives exactly
`ANTHROPIC_BASE_URL`, `ANTHROPIC_AUTH_TOKEN` and `ANTHROPIC_MODEL`. Missing, empty or extra
input blocks startup; operator-specific variable names are not read, and the worker does not
inherit an ambient provider credential or have a fallback endpoint.

The deployment must provision these identities before either service starts:

- Gateway client CA and exact URI SAN accepted by Control;
- Control client certificate accepted by Access and Agent;
- Agent and Control server certificates whose DNS SAN matches each configured HTTPS base URL;
- versioned MinIO bucket with an allowlist restricted to the configured object prefix;
- JetStream stream and durable consumers named by configuration;
- Control and Agent runtime database roles with only their own schema privileges.

The reviewed Control configuration pins the active image-policy ID and revision.
Publication accepts only an approved private Harbor repository and immutable digest
under that image-policy identity. The current deployment contract does not carry a
scanner/database identity or vulnerability gate result. There is no signing trust-plane
configuration in the deployment contract.

Control publishes both Release publication and withdrawal facts from its PostgreSQL Outbox. The
publisher uses the configured bounded ACK timeout and poll interval and marks `published_at` only
after JetStream persistence acknowledgement. A restart therefore retries an unacknowledged fact
with the same CloudEvent ID. Consumers must process aggregate sequence `1` (publication) before
sequence `2` (withdrawal), and must reject new Environment creation from a withdrawn release.

For ProblemPackage completion, the client manifest hash is the canonical hash of the sorted
original upload declaration. Control separately computes the immutable completed-package manifest
hash after freezing every exact MinIO object version. This keeps the client-verifiable upload
contract distinct from the server-owned object-version identity.

After the non-destructive retained-infrastructure inventory confirms that each
domain has no business relations and an empty migration ledger, the application
profile applies the baseline catalog before starting the new processes. Verify
the generated contracts remain current:

```sh
cargo xtask contracts check
```

Service startup verifies required tables and exits with a stable diagnostic when schema,
certificate, secret locator, provider binding or durable consumer configuration is absent.
Startup never repairs an unknown schema.

## One-shot workload runtime prerequisites

Every platform one-shot workload — admitted Agent authoring attempts, OJ runs and Ansible probes —
runs as a Kubernetes Job with `runtimeClassName: labweaver-sandbox`. Untrusted experiment programs,
probe scripts and model-generated code all execute inside that runtime, so the isolation boundary is
identical for all three roles and is not chosen per service.

Before any of these services dispatches work, every node eligible for those workloads must expose a
`RuntimeClass` with handler `labweaver-sandbox` mapped to gVisor, with the `runsc` sentry sidecar
tree installed next to the runtime binary. The handler is a container-runtime handler, so its
spelling depends on the node's CRI: a containerd node registers
`runtime_type = "io.containerd.runsc.v1"` in the `io.containerd.grpc.v1.cri` runtime table, while a
CRI-O node registers a CRI-O runtime table named exactly like the handler:

```toml
[crio.runtime.runtimes.labweaver-sandbox]
runtime_path = "/usr/local/libexec/labweaver-sandbox/runsc"
runtime_type = "oci"
monitor_path = "/usr/libexec/crio/conmon"
```

`deploy/ansible/roles/sandbox_runtime` installs that table (and the reviewed gVisor release from
`deploy/versions.lock.yml`) on the CRI-O worker nodes and publishes the `RuntimeClass`. The handler
must not set a `base_runtime_spec`: `runsc` refuses to start a container from a base spec that
carries no `mounts` array, so the process bound is not expressed in the OCI spec.

The process bound is enforced on the pod cgroup instead. Eligible nodes set the kubelet
`podPidsLimit` to a reviewed finite value, so a fork bomb inside a sandbox is terminated instead of
exhausting the node's pid space. Under gVisor every guest process is a sandbox host thread, so this
node-level bound covers the whole one-shot Pod — every container of the Job — rather than one
container, and a gVisor release that ignores `linux.resources.pids` leaves a per-container OCI cap
unenforceable. The bound therefore has to leave room for every co-located workload on that node: a
deployment that shares nodes with JVM services must review a larger value, while a dedicated
one-shot node pool can review a small one. The v1 deployment shares both workers with Keycloak,
PostgreSQL, NATS, MinIO, Harbor and user environments, and carries `podPidsLimit: 16384`
(`deploy/ansible/roles/sandbox_runtime/defaults/main.yml`); the single-node local stack carries
4096 (`tools/local_dev.py`). Recording which value the node class carries is a deployment
prerequisite, not a reason to drop the sandbox. If the handler is unavailable, scheduling must fail
closed instead of falling back to the node default runtime.

Keep the existing seccomp, no-new-privileges, dropped-capability, read-only-root-filesystem and
non-root controls unchanged; do not weaken them to make the runtime available. When the authoring
BuildKit sidecar is enabled, the attempt Pod keeps the same RuntimeClass; a node class that cannot
run the rootless BuildKit sidecar inside gVisor is a deployment prerequisite to record, not a reason
to drop the sandbox control.

## Authoring sandbox prerequisites

Admitted authoring attempts run one Kubernetes Job per attempt in the fixed `labweaver-authoring`
namespace. Ansible reconciles the namespace, the `authoring-default-deny` namespace-wide
NetworkPolicy, the tokenless `authoring-runner` ServiceAccount and the platform registry pull
secret; the Agent executor verifies the default-deny policy before it applies any attempt bundle.
The attempt Pod runs the digest-pinned sandbox image built from the `authoring-sandbox` target of
`containers/Containerfile.rust` (Claude Code CLI plus bash, python3, git, curl and buildctl), as a
non-root user with a read-only root filesystem, no service-account token and only the configured
proxy/registry/model egress CIDRs, and it carries `runtimeClassName: labweaver-sandbox` (see the
shared runtime prerequisites above). Agent holds a namespaced Role limited to Jobs, Secrets, Pods and
NetworkPolicies in that single namespace; it never receives cluster-wide permissions.

## Current production blocker

The local v1 build and release path is implemented. Connected BuildKit, Harbor,
Container and KubeVirt replay under one deployment identity is still
required before the deployment can claim verified operation. Fixtures and static
reports are not production publication evidence.
