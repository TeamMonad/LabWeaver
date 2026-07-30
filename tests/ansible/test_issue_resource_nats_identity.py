"""Safety tests for Resource NATS identity issuance."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import sys
import tempfile
import unittest

import yaml


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "tools/issue_resource_nats_identity.py"
SPEC = importlib.util.spec_from_file_location("issue_resource_nats_identity", SCRIPT)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("identity issuance module could not be loaded")
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)


class ResourceIdentityIssuanceTests(unittest.TestCase):
    def test_ansible_entrypoint_is_explicit_and_secret_silent(self) -> None:
        playbook = yaml.safe_load(
            (ROOT / "deploy/ansible/playbooks/94-resource-identity.yml").read_text(
                encoding="utf-8"
            )
        )
        tasks = playbook[0]["tasks"]
        command = next(task for task in tasks if "Issue Resource identity" in task["name"])
        self.assertTrue(command["no_log"])
        self.assertIn("--store", command["ansible.builtin.command"]["argv"])
        self.assertIn("--nsc", command["ansible.builtin.command"]["argv"])

    def test_output_must_be_private(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            public = root / "output"
            public.mkdir()
            with self.assertRaisesRegex(MODULE.IssuanceError, "PRIVATE_PATH_REQUIRED"):
                MODULE.private_path(public / "identity")

    def test_existing_output_is_rejected_before_nsc(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            private = Path(temporary) / ".private"
            private.mkdir()
            store = private / "store"
            store.mkdir()
            output = private / "identity"
            output.mkdir()
            with self.assertRaisesRegex(MODULE.IssuanceError, "OUTPUT_EXISTS"):
                MODULE.issue(store, Path("/does/not/exist"), output, 365)

    def test_permissions_are_bounded(self) -> None:
        self.assertEqual(MODULE.IDENTITY, "resource-service")
        self.assertEqual(MODULE.SUBJECT, "labweaver.resource.lease.verify.v1")


if __name__ == "__main__":
    unittest.main()
