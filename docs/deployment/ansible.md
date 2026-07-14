# Ansible cluster deployment

The only deployment controller entry points are `cargo xtask preflight --infra
--env <environment>`, `cargo xtask deploy --infra --env <environment> --yes`,
`cargo xtask verify --infra --env <environment> --yes`, and `cargo xtask backup
--infra --env <environment> --yes`. They run only on the approved Linux router
worktree through `ansible-rs`; Windows fails with a stable unsupported-platform
diagnostic. The removed Python launcher is not a deployment fallback.
The router invocation must export `LABWEAVER_SOURCE_COMMIT` with the verified
bundle commit and `LABWEAVER_ANSIBLE_DEPENDENCY_ROOT` with the absolute router
controller directory that contains the locked collections. Missing, malformed,
or unreadable identity/dependency inputs fail before Ansible starts; the bundle
does not infer a dependency directory from its temporary extraction path.
集群角色、固定版本、存储、网络和证据边界见
[`cluster-internal-configuration.md`](cluster-internal-configuration.md)。

The deployment entry points are `python tools/ansible.py preflight`,
`python tools/ansible.py deploy`, `python tools/ansible.py verify`, and
`python tools/ansible.py backup`.

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
Harbor also requires the verified local `harbor-1.19.1.tgz` archive declared by
`harbor_chart_archive`; a remote repository fallback is not allowed.

`ansible-lint`, syntax checks, encrypted fictional-Vault loading, and storage
safety fixtures run on Linux CI. The approved router worktree additionally
provides the real deploy, backup, isolated VM/storage/Gateway/Cilium probes,
schema-validated TestFlight report, and second idempotent replay. The report
remains blocked until OIDC, Harbor policy/recovery, and Release Gate evidence
are completed.
