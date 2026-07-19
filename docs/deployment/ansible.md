# Ansible cluster deployment

The only deployment controller entry points are `cargo xtask preflight --infra
--env <environment>`, `cargo xtask deploy --infra --env <environment> --yes`,
`cargo xtask verify --infra --env <environment> --yes`, and `cargo xtask backup
--infra --env <environment> --yes`. The destructive Sprint 2 rebuild additionally
uses `cargo xtask sprint2-foundation --infra --env <environment> --yes`,
`cargo xtask sprint2-buildkit --infra --env <environment> --yes`, and then
`cargo xtask demo reset --infra --env <environment> --yes`. They run only on the approved Linux router
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

Every `deploy --infra` runs the backup role before Harbor reconciliation. The
run-specific backup evidence binds run ID, cluster UID, commit, inventory and
component-lock hashes to the `harbor-reconcile` target. When Harbor already
contains persistent data, the operator must additionally provide a protected
Harbor data-backup evidence locator through
`LABWEAVER_HARBOR_DATA_BACKUP_LOCATOR`; missing or identity-mismatched evidence
blocks reconciliation. TestFlight temporary resources in `labweaver-demo` are
named and selected by its run ID, so cleanup cannot target another run.

`ansible-lint`, syntax checks, encrypted fictional-Vault loading, and storage
safety fixtures run on Linux CI. The approved router or A-owned WSL controller worktree additionally
provides the real deploy, backup, isolated VM/storage/Gateway/Cilium probes,
schema-validated TestFlight report, and second idempotent replay. The report
remains blocked until OIDC, Harbor policy/recovery, and Release Gate evidence
are completed.

## Sprint 2 destructive reset

The foundation command reconciles the retained PostgreSQL, NATS JetStream and
MinIO service bodies in `labweaver-data` before reset. All images are digest
locked, all three workloads use TLS, persistent volumes, restricted Pod Security
and default-deny NetworkPolicy. Its private bundle uses
`sprint2-foundation-bundle-manifest.json`; the same renderer and strict key
validation used for the workload bundle apply. The reset deliberately excludes
`labweaver-data` and `labweaver-build` from namespace deletion and clears only
their LabWeaver schemas, streams and buckets.

The same playbook first installs checksum-locked administration clients. NSC is
installed on the approved router only for private NATS operator/account/user
authoring; NATS CLI, PostgreSQL client, MinIO client, BuildKit client and the
Keycloak administration client are installed on the control plane for the
allowlisted reset role. Downloads are versioned in `deploy/versions.lock.yml`;
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

The authoring command creates one internal CA, separate server certificates,
eight workload-specific NATS users and mTLS clients, the static operator/account
resolver config, and random PostgreSQL/MinIO bootstrap credentials. It accepts
only a new private path and fixed non-world-writable OpenSSL/NSC binaries. The
checked-in renderer then enforces the exact foundation ConfigMap/Secret key set.

```sh
cargo xtask sprint2-foundation --infra --env demo --yes
```

BuildKit is reconciled separately in `labweaver-build`. Generate a distinct
mTLS authority and exact two-object private bundle, then provide the reviewed
Harbor endpoint CIDR in the ignored inventory:

```sh
python3 tools/prepare_sprint2_buildkit.py \
  --output .private/sprint2-buildkit-<run-id>
python3 tools/render_sprint2_bundle.py \
  --manifest deploy/config/sprint2-buildkit-bundle-manifest.json \
  --input .private/sprint2-buildkit-<run-id>/render-input \
  --output .private/sprint2-buildkit-<run-id>/bundle.yml
cargo xtask sprint2-buildkit --infra --env demo --yes
```

The BuildKit namespace is the sole approved Sprint 2 exception for
`Unconfined` seccomp/AppArmor and `--oci-worker-no-process-sandbox`. The
workload remains non-root, non-privileged, without HostPath or hostNetwork, and
without Kubernetes API credentials. Its gRPC endpoint requires mTLS; the
generated `build-executor-client` material is injected only into the
`build-executor` Secret. Default-deny NetworkPolicy admits that workload and
the reviewed Harbor CIDR only.

`demo reset` runs only the allowlisted `93-sprint2-reset.yml` playbook. It is a
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
Kubernetes YAML bundle containing exactly the eight required ConfigMaps and eight
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

The reset inventory supplies that file as `sprint2_reset_access_seed_file`.
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
Sprint 2 baseline migration for each domain, deploys the nine-workload profile
twice, exercises Helm atomic rollback with the reviewed failing values, verifies
all rollouts, and writes a sanitized report conforming to
`schemas/results/sprint2-reset-report.v1.schema.json`.

The reset report is deployment evidence, not Sprint 2 acceptance. Real Container,
KubeVirt, Gateway, Keycloak Playwright, freeze/cleanup and Release Gate checks
must still close under the same source/deployment identity.
