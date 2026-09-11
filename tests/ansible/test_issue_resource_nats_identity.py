"""Safety tests for Resource NATS identity issuance."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

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
        self.assertEqual(
            MODULE.PUBLISH_SUBJECTS,
            (
                "$JS.API.>",
                "$JS.ACK.>",
                "labweaver.resource.request.submitted.v1",
                "labweaver.resource.request.approved.v1",
                "labweaver.resource.request.rejected.v1",
                "labweaver.resource.request.cancelled.v1",
                "labweaver.resource.request.state_changed.v1",
                "labweaver.resource.lease.activated.v1",
                "labweaver.resource.lease.renewed.v1",
                "labweaver.resource.lease.revoked.v1",
                "labweaver.resource.lease.expiring.v1",
                "labweaver.resource.lease.expired.v1",
            ),
        )
        self.assertEqual(
            MODULE.SUBSCRIBE_SUBJECTS,
            ("_INBOX.>", "labweaver.resource.lease.verify.v1"),
        )
        self.assertTrue(MODULE.RESPONSE_PERMISSION)

    def test_response_inbox_is_restored_before_credentials_are_generated(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            private = Path(temporary) / ".private"
            private.mkdir()
            store = private / "store"
            store.mkdir()
            output = private / "identity"
            calls: list[list[str]] = []

            def fake_run_nsc(_nsc: Path, _store: Path, arguments: list[str], home: Path) -> None:
                calls.append(arguments)
                if arguments[:2] == ["generate", "creds"]:
                    (home.parent / "resource-service.nats.creds").write_text(
                        "test-only", encoding="utf-8"
                    )

            with patch.object(MODULE, "run_nsc", side_effect=fake_run_nsc):
                MODULE.issue(store, Path("nsc"), output, 365)

            self.assertEqual(calls[0][:2], ["add", "user"])
            self.assertIn(
                ["--allow-pub", "labweaver.resource.lease.expired.v1"],
                [calls[0][index : index + 2] for index in range(len(calls[0]) - 1)],
            )
            self.assertEqual(
                calls[1],
                [
                    "edit",
                    "user",
                    "--account",
                    "WORKLOADS",
                    "--name",
                    "resource-service",
                    "--allow-sub",
                    "_INBOX.>",
                ],
            )
            self.assertEqual(calls[2][:3], ["generate", "creds", "--account"])


if __name__ == "__main__":
    unittest.main()
