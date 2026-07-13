"""Cross-platform, fail-closed launcher for LabWeaver Ansible playbooks."""

from __future__ import annotations

import argparse
import os
from pathlib import Path
import shutil
import subprocess


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("action", choices=("preflight", "deploy", "verify", "backup"))
    parser.add_argument("--env", default="dev")
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()

    root = Path(__file__).resolve().parents[1]
    ansible_dir = root / "deploy" / "ansible"
    inventory_dir = ansible_dir / "inventories" / args.env
    inputs = {
        "inventory": inventory_dir / "hosts.yml",
        "group_vars": inventory_dir / "group_vars" / "all" / "main.yml",
        "vault": inventory_dir / "group_vars" / "all" / "vault.yml",
        "vault_password": inventory_dir / ".vault-password",
    }

    missing = [name for name, value in inputs.items() if not value.is_file()]
    if missing:
        parser.error("missing required private deployment input: " + ", ".join(missing))

    executable = shutil.which("ansible-playbook")
    if executable is None:
        parser.error("ansible-playbook is not installed or not on PATH")

    playbooks = {
        "preflight": "00-preflight.yml",
        "deploy": "site.yml",
        "verify": "90-verify.yml",
        "backup": "85-backup.yml",
    }
    command = [
        executable,
        "-i",
        str(inputs["inventory"]),
        "--vault-password-file",
        str(inputs["vault_password"]),
        str(ansible_dir / "playbooks" / playbooks[args.action]),
    ]
    if args.check:
        command.append("--check")
    environment = os.environ | {"ANSIBLE_CONFIG": str(ansible_dir / "ansible.cfg")}
    return subprocess.run(command, cwd=root, env=environment, check=False).returncode


if __name__ == "__main__":
    raise SystemExit(main())
