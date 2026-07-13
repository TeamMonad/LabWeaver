# Ansible cluster deployment

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

`ansible-lint`, syntax checks, encrypted fictional-Vault loading, and storage
safety fixtures run on Linux CI. They are static or fixture evidence only; a
real deployment, idempotent replay, VM, storage, Gateway, Cilium, and etcd
acceptance run remain required E3 evidence.
