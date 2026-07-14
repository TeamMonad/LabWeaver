"""Contract evidence for the router-owned Ansible controller and safety policies."""

from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import sys
import unittest


ROOT = Path(__file__).resolve().parents[2]
SAFETY_PATH = ROOT / "deploy/ansible/roles/storage_nodes/files/storage_safety.py"
SPEC = importlib.util.spec_from_file_location("storage_safety", SAFETY_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("storage safety module could not be loaded")
SAFETY = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = SAFETY
SPEC.loader.exec_module(SAFETY)


class AnsibleFixtureTests(unittest.TestCase):
    def test_controller_execution_is_router_owned(self) -> None:
        docs = (ROOT / "docs/deployment/ansible.md").read_text(encoding="utf-8")
        self.assertIn("cargo xtask deploy --infra", docs)
        self.assertIn("ansible-rs", docs)
        self.assertNotIn("tools/ansible.py", docs)

    def test_deploy_starts_with_preflight(self) -> None:
        site = (ROOT / "deploy/ansible/playbooks/site.yml").read_text(encoding="utf-8")
        self.assertEqual(site.splitlines()[1], "- import_playbook: 00-preflight.yml")

    def test_verify_uses_manual_vm_lifecycle_and_fails_on_cleanup(self) -> None:
        verify = (ROOT / "deploy/ansible/roles/verify/tasks/main.yml").read_text(encoding="utf-8")
        self.assertIn("verify_runtime_namespace", verify)
        self.assertIn("virtctl -n {{ verify_runtime_namespace }} start kvm-probe", verify)
        self.assertIn("virtctl -n {{ verify_runtime_namespace }} stop kvm-probe", verify)
        self.assertNotIn("patch virtualmachine/kvm-probe", verify)
        self.assertIn("verify_cleanup_failed", verify)
        self.assertIn("CILIUM_CLEANUP_FAILED", verify)

    def test_baseline_testflight_defers_identity_governance(self) -> None:
        verify = (ROOT / "deploy/ansible/roles/verify/tasks/main.yml").read_text(encoding="utf-8")
        schema = json.loads(
            (ROOT / "schemas/infrastructure/infrastructure-testflight-report.v1.schema.json").read_text(
                encoding="utf-8"
            )
        )
        self.assertIn("adopted-cluster-baseline", verify)
        self.assertIn("deferred-to-issue-47", verify)
        self.assertIn("deferred-to-issue-2", verify)
        self.assertIn("deferred", schema["properties"]["checks"]["items"]["properties"]["status"]["enum"])

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
