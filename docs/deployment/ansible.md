# Ansible cluster deployment

The only deployment controller entry points are `cargo xtask preflight --infra
--env <environment>`, `cargo xtask deploy --infra --env <environment> --yes`,
`cargo xtask verify --infra --env <environment> --yes`, and `cargo xtask backup
--infra --env <environment> --yes`. Sprint 2 adopts retained infrastructure with
`cargo xtask sprint2-foundation --infra --env <environment> --yes`,
`cargo xtask sprint2-buildkit --infra --env <environment> --yes`, and bounded
commands such as `cargo xtask sprint2-harbor-route --infra --env <environment>
--yes` and `cargo xtask sprint2-application --infra --env <environment>
--package-manifest <manifest> --yes`. They run only on the approved Linux router
worktree through `ansible-rs`; Windows fails with a stable unsupported-platform
diagnostic. The removed Python launcher is not a deployment fallback.
The router invocation must export explicit lowercase `LABWEAVER_RUN_ID` and
`LABWEAVER_TESTFLIGHT_RUN_ID` bindings; the controller never invents them.
It must also export `LABWEAVER_SOURCE_COMMIT` with the verified
bundle commit, `LABWEAVER_ANSIBLE_DEPENDENCY_ROOT` with the router controller
directory containing the locked collections, and a root-owned
`LABWEAVER_CONTROLLER_IDENTITY_FILE`. The locator must bind the current
machine identity to `deploy/ansible/controller.lock.yml`; a copied worktree on
another Linux host is rejected before Ansible starts. Missing, malformed, or
unreadable identity/dependency inputs fail before Ansible starts; the bundle
does not infer a dependency directory from its temporary extraction path.

The retained data-service CiliumNetworkPolicy admits credentialed administration probes only from
Cilium's `host` and `remote-node` reserved identities, and only on PostgreSQL 5432, NATS 4222 and
MinIO 9000. Cilium represents node-originated traffic with those identities rather than a source
IP, so an `ipBlock` would be a non-working paper boundary. The rule does not admit Pod, LAN or
public identities; all three services still require their exact TLS credential.
集群角色、固定版本、存储、网络和证据边界见
[`cluster-internal-configuration.md`](cluster-internal-configuration.md)。

Copy the inventory and group-variable examples to ignored private files. The
private layout is `group_vars/all/main.yml`, encrypted
`group_vars/all/vault.yml`, and `.vault-password`; Ansible automatically loads
both group-variable files. Replace every placeholder before use.

The playbooks never configure PVE, NetworkManager connections, interface IPs,
WAN/LAN routing, or Tailnet. Preflight rejects missing interfaces, unresolved
variables, non-Enforcing SELinux, and unsupported node families.

The `deploy` action is idempotent for declared resources. Storage formatting is
blocked unless `storage_allow_format=true` is deliberately supplied together
with an exact WWN and capacity declaration. The role rejects root, mounted,
partitioned, stacked, held, or identity-mismatched devices before formatting.
Verify runs isolated runtime probes, records both failure diagnostics and
cleanup state, and never writes a passed report after a failed check. No public
DNAT is created.

The deployment controller needs Ansible and the collections in
`deploy/ansible/requirements.yml`; it also needs the pinned Helm and Cilium CLI
already available on the control-plane host. Their absence is a deliberate
preflight failure, never an implicit version selection.
The approved Ansible Python runtime must also contain exactly
`kubernetes==34.1.0`. `xtask` resolves the Python interpreter beside the
canonical `ansible-playbook` binary and verifies this package version before
starting any playbook; it never installs Python dependencies automatically.
Harbor also requires the verified local `harbor-1.19.1.tgz` archive declared by
`harbor_chart_archive`; its SHA-256 and every Harbor/TestFlight image digest
are locked in `deploy/versions.lock.yml`. A tag-only image, archive mismatch,
or remote repository fallback is rejected.

Full `deploy --infra` runs the backup role before Harbor reconciliation. The
run-specific backup evidence binds run ID, cluster UID, commit, inventory and
component-lock hashes to the `harbor-reconcile` target. When Harbor already
contains persistent data, the operator must additionally provide a protected
Harbor data-backup evidence locator through
`LABWEAVER_HARBOR_DATA_BACKUP_LOCATOR`; missing or identity-mismatched evidence
blocks reconciliation. Sprint 2 adoption does not use this broad entry point for
a route-only change. `sprint2-harbor-route` verifies that the namespace,
Gateway, nginx Service, ready EndpointSlice and existing HTTPRoute are managed
LabWeaver objects, then changes the existing Gateway listener to TLS passthrough
and applies one `TLSRoute` to the Harbor nginx HTTPS Service. The Harbor nginx
certificate and public CA remain the end-to-end registry identity. It verifies Gateway `Accepted`/`ResolvedRefs` conditions
and the authenticated Docker Registry `/v2/` response. It does not invoke Helm,
write a Secret, restart a Harbor Pod, or mutate Harbor database, registry, PVC,
project, or image state. A route previously owned by kubectl client-side apply
has its Gateway fields transferred to the dedicated `labweaver-sprint2-adoption`
field manager only after all managed-object checks pass. The retained HTTPRoute
is left in place but no longer attaches to the TLS-only listener. TestFlight temporary resources in `labweaver-demo` are
named and selected by its run ID, so cleanup cannot target another run.
The command also publishes the public nginx CA to the fixed root-controlled
operator locator. BuildKit and packaging inputs must consume that CA after the
route changes; it installs the same public CA in the control-plane, router
system and container-client trust stores. Retaining the previous Gateway-termination CA is
a blocking TLS identity mismatch.
The same role adds one marked, exact `harbor.lab.lan` binding to the router
controller hosts database so `buildx` can complete the Registry token exchange
against the retained VIP. It does not replace DNS or any unrelated host entry.
The same bounded adoption reads the existing private `labweaver-system` project
and aligns only its project metadata to automatic scanning without pull-time
vulnerability prevention. Pull prevention is deliberately disabled because it
also blocks builder-only layers; the package command's digest-bound Trivy scan
is the single blocking Gate for final runtime images. The adoption does not
alter Harbor's global policy, CVE allowlist, project identity, repository
contents, or scan reports.

`ansible-lint`, syntax checks, encrypted fictional-Vault loading, and storage
safety fixtures run on Linux CI. The approved router or A-owned WSL controller worktree additionally
provides the real deploy, backup, isolated VM/storage/Gateway/Cilium probes,
schema-validated TestFlight report, and second idempotent replay. The report
remains blocked until OIDC, Harbor policy/recovery, and Release Gate evidence
are completed.

## Sprint 2 retained-infrastructure adoption

The current Sprint 2 delivery does not run `cargo xtask demo reset`. It does not
uninstall Sigstore or Kyverno and does not delete namespaces, webhooks, CRDs,
PVCs, schemas, streams, consumers, buckets, Harbor projects/images, or Keycloak
realms/clients. The reset implementation remains an explicitly destructive
maintenance command outside this delivery and is not an installation
prerequisite or Release Gate step.

The foundation command reconciles the retained PostgreSQL, NATS JetStream and
MinIO service bodies in `labweaver-data` before application adoption. All images are digest
locked, all three workloads use TLS, persistent volumes, restricted Pod Security
and default-deny NetworkPolicy. Each StatefulSet Pod template binds the exact
private bundle SHA-256 so an identity or configuration rotation cannot be
mistaken for a completed rollout while an old process remains Ready. Its private bundle uses
`sprint2-foundation-bundle-manifest.json`; the same renderer and strict key
validation used for the workload bundle apply. The reset deliberately excludes
`labweaver-data` and `labweaver-build` from namespace deletion and clears only
their LabWeaver schemas, streams and buckets.

The same playbook first installs checksum-locked administration clients. NSC is
installed on the approved router only for private NATS operator/account/user
authoring; NATS CLI, PostgreSQL client, MinIO client, BuildKit client and the
Keycloak administration client are installed on the control plane for the
allowlisted application-adoption role. Downloads are versioned in `deploy/versions.lock.yml`;
the role neither discovers a latest release nor executes an arbitrary shell.

On the approved router, create a new root-owned private authoring directory and
render its exact Kubernetes input bundle without printing credential values:

```sh
python3 tools/prepare_sprint2_foundation.py \
  --output .private/sprint2-foundation-<run-id>
python3 tools/render_sprint2_bundle.py \
  --manifest deploy/config/sprint2-foundation-bundle-manifest.json \
  --input .private/sprint2-foundation-<run-id>/render-input \
  --output .private/sprint2-foundation-<run-id>/bundle.yml
```

The authoring command creates one infrastructure CA, separate server
certificates, eight workload-specific NATS users and mTLS clients, one distinct
`sprint2-admin` mTLS client for controller-side NATS administration, the static
operator/account resolver config, and random PostgreSQL/MinIO bootstrap
credentials. The `WORKLOADS` account enables JetStream with bounded 8 GiB disk,
256 MiB memory, 16-stream, 64-consumer and 4096-pending-ack limits; the NATS
server remains bounded by the same or stricter global storage limits. A distinct
Platform CA issues the exact Control, Access, Agent,
Environment and OpenSSH Gateway identities: Control and Access have the combined
server/client EKU required by the reviewed call graph, Agent and Environment are
server-only, and OpenSSH Gateway is client-only. It accepts only a new private
path and fixed non-world-writable OpenSSL/NSC binaries. The checked-in renderer
then enforces the exact foundation ConfigMap/Secret key set.

Application adoption must bind the retained NATS administrator credentials and
the dedicated TLS identity separately. Set
`sprint2_application_nats_credentials_file`,
`sprint2_application_nats_ca_file`,
`sprint2_application_nats_client_certificate_file`, and
`sprint2_application_nats_client_private_key_file` to root-owned mode-0600
files in ignored private storage. The role passes all four inputs to every NATS
CLI invocation and fails before mutation when any input is absent; a workload
client certificate must not be reused for administration.
The reviewed Keycloak realm remains controller-owned input; the application
role validates it locally, stages a root-owned mode-0600 copy in the bounded
remote run directory, and gives only that execution-host path to `kcadm`.
Because `kcadm config credentials` can return zero after an HTTP authentication
failure, the role rejects any 4xx/5xx response text and immediately performs a
bounded master-realm user query. A readable realm metadata endpoint alone is
not accepted as administrator authorization.

Application adoption installs the reviewed Harbor public CA into both the
system trust store and `/etc/containers/certs.d/<registry>/ca.crt` on every
inventory member of `k8s_cluster`, then refreshes only changed trust stores.
This is required before the digest-only workloads are created; an image pull
through an untrusted registry certificate remains a blocking rollout error.

The `labweaver-system` namespace enforces Pod Security `baseline` because the
OpenSSH Gateway must start as root and retain only `CHOWN`, `DAC_OVERRIDE`,
`FOWNER`, `SETGID`, `SETUID`, and `SYS_CHROOT` for the fixed account/session
boundary. The namespace continues to audit and warn at `restricted`; every
other Sprint 2 workload is explicitly non-root, drops all capabilities, and
uses a read-only root filesystem. The Gateway exception does not permit
`privileged`, HostPath, host networking, or a Kubernetes API token.

```sh
cargo xtask sprint2-foundation --infra --env demo --yes
```

BuildKit is reconciled separately in `labweaver-build`. Generate a distinct
mTLS authority and exact two-object private bundle. The bundle pins the public
CA of the existing Harbor endpoint so registry TLS remains strict; then provide
the reviewed Harbor endpoint CIDR in the ignored inventory:

```sh
python3 tools/prepare_sprint2_buildkit.py \
  --registry-host harbor.lab.lan \
  --dns-nameserver 10.96.0.10 \
  --registry-ca /var/lib/labweaver/.private/harbor-public/registry-ca.crt \
  --output .private/sprint2-buildkit-<run-id>
python3 tools/render_sprint2_bundle.py \
  --manifest deploy/config/sprint2-buildkit-bundle-manifest.json \
  --input .private/sprint2-buildkit-<run-id>/render-input \
  --output .private/sprint2-buildkit-<run-id>/bundle.yml
cargo xtask sprint2-buildkit --infra --env demo --yes
```

`--registry-ca` 必须指向当前 Harbor 公网入口证书的签发 CA，不能使用
Harbor 集群内部 CA。部署角色会在修改 BuildKit 前把 bundle 中的 CA 与
保留的 Harbor nginx TLS Secret 做精确比对；不一致时以
`SPRINT2_BUILDKIT_HARBOR_CA_MISMATCH` 阻断。

The BuildKit namespace is the sole approved Sprint 2 exception for
`Unconfined` seccomp/AppArmor, container-scoped SELinux `spc_t`, and
`--oci-worker-no-process-sandbox`. The SELinux exception permits rootless
BuildKit's inner `runc` to mount a new `devpts` instance and relabel snapshot
content on enforcing hosts; it is applied to the builder container rather than
installing a node-wide policy. The
workload remains non-root, non-privileged, without HostPath or hostNetwork, and
without Kubernetes API credentials. The container permits only the `SETUID` and
`SETGID` capabilities plus the setuid transition required by RootlessKit's
`newuidmap`/`newgidmap`; every other capability remains dropped. Its gRPC endpoint requires mTLS; the
generated `build-executor-client` material is injected only into the
`build-executor` Secret. The same authoring run installs a mode-0600 operator
client in the fixed control-plane packaging directory; an older client is
replaced whenever the BuildKit authority rotates. Default-deny NetworkPolicy admits that workload and
the reviewed Harbor CIDR only. For in-cluster builds, the role reads the
retained Harbor nginx Service identity and adds an exact, idempotent
`harbor.lab.lan -> ClusterIP` record to the retained CoreDNS Corefile. This is
required because registry resolution also occurs inside BuildKit worker
namespaces, where a Pod-only `hostAliases` entry is insufficient. The role
rejects an existing ambiguous record and does not replace other CoreDNS data.
This avoids unsupported same-cluster LoadBalancer hairpinning. Egress is additionally limited to the `harbor`
namespace's exact nginx labels and TLS target port.
Dependency-fetch steps use a separate Cilium policy bound only to the BuildKit
Pod. DNS is intercepted through the cluster `kube-dns` endpoint so Cilium can
materialize the reviewed FQDN identities; name resolution alone does not grant
network access. TCP/443 is limited to the exact Cargo sparse-index, crate
archive, npm registry, and Alpine package hostnames. Compilation remains
`--network=none`; no wildcard data-plane FQDN or arbitrary egress is permitted.

Prepare the ignored application configuration bundle, Keycloak realm, Access
seed and Helm values with the checked-in renderers. Then adopt the retained data
services and deploy the immutable profile:

```sh
export LABWEAVER_RUN_ID=sprint2-application-<run-id>
export LABWEAVER_TESTFLIGHT_RUN_ID=testflight-sprint2-<run-id>
cargo xtask sprint2-application \
  --infra \
  --env demo \
  --package-manifest artifacts/package/<package-run>/PlatformImagePackageManifest.json \
  --yes
```

应用部署会在 `labweaver-system` 中非破坏性地采用或首次导入锁定的
Ubuntu 24.04 VM base。Registry manifest digest 与磁盘 SHA-256 固定在
`deploy/versions.lock.yml`；已有 `DataVolume` 或 `DataSource` 只要来源、
StorageClass 或 hash 不一致就会阻断，不会被覆盖。导入成功后发布
`ubuntu-lab-base-v1`，供 KubeVirt executor 通过 CDI `sourceRef` 克隆。

门户与 OpenSSH Gateway 复用现有 public Gateway，但只做有界的增量采用：缺失时分别
增加 HTTPS/443 与 TCP/2222 listener，并创建独立 HTTPRoute/TCPRoute 和
ReferenceGrant；已存在但协议、端口、hostname 或路由范围不一致时立即阻断。门户 CA
仅写入保留路由器的系统 trust store，验证过程不使用 `-k` 或其他跳过 TLS 校验的参数。

This command is fail-closed and non-destructive. It applies a baseline only to
a domain with no existing business relations and an empty migration ledger;
otherwise it requires the exact catalog and migration hashes. Access seed rows
are inserted only when missing and conflicting identities abort the transaction.
Missing JetStream streams, the versioned MinIO bucket and the Keycloak realm may
be created, while an existing object must pass identity checks. The Harbor
project must already exist and is read-only. The application namespace and its
reviewed ConfigMap/Secret bundle are reconciled in place, followed by two atomic
Helm upgrades using exactly seven digest references. The command never invokes
`demo reset`, `DROP`, stream or bucket deletion, Harbor project/image deletion,
realm deletion, namespace deletion, trust-plane uninstall, CRD removal or PVC
removal. Its sanitized report conforms to
`schemas/results/sprint2-application-report.v1.schema.json`.

The following legacy reset description documents an out-of-scope maintenance
path and must not be followed for Sprint 2 adoption. `demo reset` runs only the
allowlisted `93-sprint2-reset.yml` playbook. It is a
pre-release destructive operation: there is no upgrade or restore guarantee for
the deleted LabWeaver business data. Before it changes the cluster it verifies
PostgreSQL, JetStream, MinIO, BuildKit, Harbor and Keycloak connectivity, then
inventories the cluster UID, Helm releases and all Kyverno policies. A missing
dependency fails before any namespace, webhook, realm, bucket or schema is
deleted. Any ClusterPolicy or any
Policy outside the exact LabWeaver reset namespaces stops the run with
`KYVERNO_EXTERNAL_DEPENDENCY_DETECTED`.

The ignored environment inventory must supply reviewed paths and credentials for
PostgreSQL (`PGSERVICEFILE`), NATS, MinIO, BuildKit, Harbor and Keycloak, the
Sprint 2 Helm values, and a separate rollback-probe values file whose only purpose
is to make readiness fail. It must also provide one reviewed multi-document
Kubernetes YAML bundle containing exactly the nine required ConfigMaps and nine
required Secrets in `labweaver-system`. The role rejects extra kinds, names or
namespaces, applies the bundle only after namespace recreation, and records only
its SHA-256. Secrets remain in Vault or root-owned controller files and are never
copied into the report. A private Access seed is derived from the reviewed
Keycloak realm so the OIDC `sub` hashes, durable Actor IDs and teacher/student
course memberships are bound before either browser session is created:

```sh
python3 tools/prepare_sprint2_access_seed.py \
  --realm-file .private/keycloak-realm.json \
  --issuer https://keycloak.example.invalid/realms/workloads \
  --course-id 00000000-0000-7000-8000-000000000301 \
  --teacher-username teacher \
  --student-username student \
  --output .private/sprint2-access-seed.json
```

The application inventory supplies that file as
`sprint2_application_access_seed_file`. The legacy reset inventory may retain
the old variable only for out-of-scope maintenance.
The role validates all identities before destruction, seeds only Control and
OpenSSH Gateway service identities, and checks the exact membership count after
the baseline migration. The operator must first read the target UID and set:

```sh
export LABWEAVER_RUN_ID=sprint2-reset-20260719
export LABWEAVER_SPRINT2_RESET_CONFIRMATION="destroy-pre-release-data:<cluster-uid>:${LABWEAVER_RUN_ID}"
cargo xtask demo reset --infra --env demo --yes
```

The role removes the historical Private Sigstore namespace and, only after the
dependency guard passes, Kyverno and its residual CRDs/webhooks. It then resets
the exact LabWeaver namespaces, six PostgreSQL schemas, declared NATS streams,
artifact bucket, Harbor project/images and Keycloak realm. It applies the single
Sprint 2 baseline migration for each domain, deploys the ten-workload profile
twice, exercises Helm atomic rollback with the reviewed failing values, verifies
all rollouts, and writes a sanitized report conforming to
`schemas/results/sprint2-reset-report.v1.schema.json`.

The reset report is deployment evidence, not Sprint 2 acceptance. Real Container,
KubeVirt, Gateway, Keycloak Playwright, freeze/cleanup and Release Gate checks
must still close under the same source/deployment identity.
