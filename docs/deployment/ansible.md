# Ansible cluster deployment

The only deployment controller entry points are `cargo xtask preflight --infra
--env <environment>`, `cargo xtask deploy --infra --env <environment> --yes`,
`cargo xtask verify --infra --env <environment> --yes`, and `cargo xtask backup
--infra --env <environment> --yes`. The destructive Sprint 2 rebuild additionally
uses `cargo xtask demo reset --infra --env <environment> --yes`. They run only on the approved Linux router
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
copied into the report. The operator must first read the target UID and set:

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
