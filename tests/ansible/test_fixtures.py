"""Linux-controller evidence for the Ansible launcher and safety policies."""

from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SAFETY_PATH = ROOT / "deploy/ansible/roles/storage_nodes/files/storage_safety.py"
SPEC = importlib.util.spec_from_file_location("storage_safety", SAFETY_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("storage safety module could not be loaded")
SAFETY = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = SAFETY
SPEC.loader.exec_module(SAFETY)


def run(*command: str, cwd: Path | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(command, cwd=cwd, text=True, capture_output=True, check=False)


class AnsibleFixtureTests(unittest.TestCase):
    @unittest.skipUnless(
        shutil.which("ansible-vault") and shutil.which("ansible-playbook"),
        "requires a Linux/Unix Ansible controller; enforced by the Linux CI gate",
    )
    def test_encrypted_group_vault_is_loaded_by_inventory_and_playbook(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            group_vars = root / "group_vars/all"
            group_vars.mkdir(parents=True)
            password = root / ".vault-password"
            password.write_text("fictional-fixture-password\n", encoding="utf-8")
            (root / "hosts.yml").write_text(
                "all:\n  hosts:\n    fixture:\n      ansible_connection: local\n      ansible_user: '{{ vault_fixture_user }}'\n",
                encoding="utf-8",
            )
            (group_vars / "main.yml").write_text("fixture_public_value: visible\n", encoding="utf-8")
            encrypted = run(
                "ansible-vault", "encrypt_string", "--vault-password-file", str(password),
                "--name", "vault_fixture_user", "fictional-fixture-user",
            )
            self.assertEqual(encrypted.returncode, 0, encrypted.stderr)
            (group_vars / "vault.yml").write_text(encrypted.stdout, encoding="utf-8")
            playbook = root / "assert.yml"
            playbook.write_text(
                "- hosts: fixture\n  gather_facts: false\n  tasks:\n    - ansible.builtin.assert:\n        that:\n          - ansible_user == 'fictional-fixture-user'\n          - fixture_public_value == 'visible'\n",
                encoding="utf-8",
            )
            result = run("ansible-playbook", "-i", "hosts.yml", "--vault-password-file", ".vault-password", "assert.yml", cwd=root)
            self.assertEqual(result.returncode, 0, result.stderr + result.stdout)

    def test_deploy_starts_with_preflight(self) -> None:
        site = (ROOT / "deploy/ansible/playbooks/site.yml").read_text(encoding="utf-8")
        self.assertEqual(site.splitlines()[1], "- import_playbook: 00-preflight.yml")

    def test_verify_uses_manual_vm_lifecycle_and_fails_on_cleanup(self) -> None:
        verify = (ROOT / "deploy/ansible/roles/verify/tasks/main.yml").read_text(encoding="utf-8")
        self.assertIn("virtctl -n labweaver-verify start kvm-probe", verify)
        self.assertIn("virtctl -n labweaver-verify stop kvm-probe", verify)
        self.assertNotIn("patch virtualmachine/kvm-probe", verify)
        self.assertIn("verify_cleanup_failed", verify)
        self.assertIn("CILIUM_CLEANUP_FAILED", verify)

    def test_storage_safety_rejects_dangerous_devices(self) -> None:
        safe = {"path": "/dev/test", "type": "disk", "fstype": None, "wwn": "fixture-wwn", "size": 1073741824, "pkname": None, "mountpoints": [None]}
        result = SAFETY.validate([safe], "/dev/test", "fixture-wwn", 1073741824, "/dev/root", [])
        self.assertTrue(result["safe_to_format"])
        for changed in (
            {**safe, "type": "part"}, {**safe, "wwn": "wrong"}, {**safe, "size": 1},
            {**safe, "mountpoints": ["/data"]}, {**safe, "children": [{"path": "/dev/test1"}], "pkname": None},
        ):
            with self.assertRaises(SAFETY.UnsafeStorage):
                SAFETY.validate([changed], "/dev/test", "fixture-wwn", 1073741824, "/dev/root", [])
        with self.assertRaises(SAFETY.UnsafeStorage):
            SAFETY.validate([safe], "/dev/test", "fixture-wwn", 1073741824, "/dev/root", ["holder"])


if __name__ == "__main__":
    unittest.main()
