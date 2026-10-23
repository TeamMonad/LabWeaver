# Issue #48 Control-plane deployment

Material upload recovery and migration requirements are documented in
[Material uploads](../contracts/material-uploads.md). Keep the original upload deadline
when reissuing authorities. Budget temporary storage for two concurrent material
verifications and retain exact-version cleanup permission within the owned prefix.

## Configuration boundary

Control and Agent read one reviewed YAML document from
`LABWEAVER_CONTROL_CONFIG_FILE` and `LABWEAVER_AGENT_CONFIG_FILE`. The repository examples are
`deploy/config/control-plane.yaml.example` and
`deploy/config/agent-control-plane.yaml.example`. Database passwords, object-store credentials,
NATS credentials and private keys are file locators; secret values do not belong in YAML, logs,
reports or Git.

The Agent service owns the model gateway. Its upstream URL, upstream key and HMAC signing key are
mounted from `model-gateway-upstream-url`, `model-gateway-upstream-key` and
`model-gateway-signing-key` Secret files. Workers receive only `ANTHROPIC_MODEL` statically and
per-attempt `ANTHROPIC_BASE_URL`/`ANTHROPIC_API_KEY` values. Missing, empty or extra worker
environment input blocks startup; no ambient provider credential or fallback route exists.

## Agent model gateway

Agent 启动时必须从 Secret 文件读取 `model-gateway-upstream-url`、
`model-gateway-upstream-key` 和 `model-gateway-signing-key`；任一缺失、为空或不是绝对路径都
导致 fail-closed。网关监听 Agent Service 的内部 `9460` 端口，Worker 只获得静态模型名和
每次 run/attempt 签发的短期 `ANTHROPIC_API_KEY`，不能配置提供商 URL 或长期 key。

沙箱命名空间的 Cilium NetworkPolicy 只允许到 `agent-service:9460`（以及其已批准的对象存储），
不允许 host、FQDN 或提供商 CIDR。更新 upstream 时只替换 Secret 并重启 Agent；不把 key 写入
YAML、命令参数、日志或 bundle 报告。

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
tree installed next to the runtime binary.

The isolation boundary has to cover a Pod's sandbox container as well as its other containers, so
the node needs a container runtime that applies the Pod's runtime class to both. containerd does:
`sandboxer = "podsandbox"` on a runtime table makes the runtime own the Pod sandbox, and the CRI
writes the standard `io.kubernetes.cri.*` annotations that gVisor reads to tell a sandbox container
from an ordinary one. CRI-O does not — it creates a Pod's sandbox with the node's *default* runtime
regardless of the Pod's `runtimeClassName`, so `runsc` never owns a sandbox for such a Pod and every
Job container fails with `cannot load sandbox: …_sandbox:….state: no such file or directory`.
Sandbox-capable workers therefore run containerd, while the control-plane node keeps the CRI the
cluster was installed with:

```toml
[plugins.'io.containerd.cri.v1.runtime'.containerd.runtimes.labweaver-sandbox]
  runtime_type = 'io.containerd.runsc.v1'
  sandboxer = 'podsandbox'
```

`deploy/ansible/roles/sandbox_runtime` installs the reviewed gVisor release from
`deploy/versions.lock.yml` into `/usr/local/bin` (the whole tree, because `runsc` resolves its
`gvisor-bin/` sentry relative to its own directory and containerd finds the shim by name on `PATH`),
writes that runtime table into `/etc/containerd/config.toml`, points the kubelet's
`containerRuntimeEndpoint` at containerd, retires the previous container runtime on the node and
publishes the `RuntimeClass`. The runtime table sets no options and no `base_runtime_spec`: gVisor's
shim rejects unknown option keys, and `runsc` refuses to start a container from a base spec that
carries no `mounts` array, so the process bound is not expressed in the OCI spec. Every runtime
table keeps `sandboxer = 'podsandbox'` so the sandbox container of a Pod is created by the runtime
that Pod selected; a GPU node additionally keeps the `nvidia` device runtime for the workloads that
need it.

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

## BuildKit Credits admission

The build-executor configuration requires the `resource` block from
`deploy/config/build-executor.yaml.example`: HTTPS URI, trusted Resource CA,
request/response size bounds, timeout, and `labweaver-resource` audience.
Its existing Agent service account requests `resource.credits.hold` and
`resource.usage.record`. Mount the OIDC CA, Resource CA and service-client-secret;
configure `LABWEAVER_SERVICE_OIDC_ISSUER`, `LABWEAVER_SERVICE_OIDC_CA`,
`LABWEAVER_SERVICE_CLIENT_ID`, `LABWEAVER_SERVICE_CLIENT_SECRET_FILE`,
`LABWEAVER_SERVICE_SCOPES` and token refresh skew. Resource separately sets
`LABWEAVER_AGENT_SERVICE_CLIENT_ID` to the authorized client. Missing configuration
rejects BuildKit execution; it does not select a free or anonymous path.

Apply Resource migration 0019 and Agent migration 0021. Agent 0021 grants Resource
only the specific read columns needed to verify the authoritative build command;
Agent has no wallet write access. Bundle/profile configuration must provide the
same Resource client block and secret mounts before connected deployment validation.
Configure an explicit `CREDITS` price with `build_second`, `builderBinding` and
`unitQuantity` before admitting builds. Existing USD compute/storage rates do not
price this operation and are not automatically changed.

### Original Environment lease funding

Apply Resource migration 0020 with the verified migration catalog before starting
Resource. It adds immutable funding windows and seeds no operating rates or
balances. The existing LABWEAVER_ENVIRONMENT_SERVICE_CLIENT_ID binds the original
hold owner and the grant reader; service JWT/TLS and resource.environment.resolve
remain required. Configure complete CREDITS rates and project allowances before
activating a lease. A missing rate, insufficient funds or an expired original
approval leaves activation uncommitted.

Renewal reuses the same hold and funds only a cumulative shortfall. The accounting
loop also handles expired financial coverage and unactivated holds whose original
approval expired. Financial release does not confirm physical cleanup.
The Environment owner must consume the original grant and runtime receipt before
new compute, while continuing stop/cleanup without new funds. The #207 lifecycle
integration and cluster validation remain separate delivery work; see
[original lease coverage](../contracts/project-credits.md#原-environment-lease-的资金覆盖).
