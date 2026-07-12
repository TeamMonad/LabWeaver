# Ansible cluster deployment

The deployment entry points are `python tools/ansible.py preflight`,
`python tools/ansible.py deploy`, `python tools/ansible.py verify`, and
`python tools/ansible.py backup`.

Copy the inventory and vault examples to ignored private files, replace every
placeholder, encrypt `vault.yml`, and provide its password file outside Git.

The playbooks never configure PVE, NetworkManager connections, interface IPs,
WAN/LAN routing, or Tailnet. Preflight rejects missing interfaces, unresolved
variables, non-Enforcing SELinux, and unsupported node families.

The `deploy` action is idempotent for declared resources. Storage formatting is
blocked unless `storage_allow_format=true` is deliberately supplied. Verify runs
the real Cilium suite and cleans its temporary resources. No public DNAT is
created.

The deployment controller needs Ansible and the collections in
`deploy/ansible/requirements.yml`; it also needs the pinned Helm and Cilium CLI
already available on the control-plane host. Their absence is a deliberate
preflight failure, never an implicit version selection.
