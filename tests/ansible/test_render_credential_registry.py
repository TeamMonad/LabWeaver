import importlib.util
import json
import os
import tempfile
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).parents[2] / "tools" / "render_credential_registry.py"
SPEC = importlib.util.spec_from_file_location("render_credential_registry", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)

RUN_ID = "issue152-clean-redeploy-a1"
COMMIT = "5142a02009ae7e021d6fdaa1c345093864ecdcb0"
FIXED_TIMESTAMP = "2026-08-05T00:00:00Z"


class CredentialRegistryTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.input = self.root / ".private" / "rotation"
        secrets = self.input / "secrets" / "postgres-secrets"
        secrets.mkdir(parents=True)
        (secrets / "postgres-password").write_bytes(b"never-logged-value")
        (self.input / "ca.crt").write_bytes(b"public-certificate")

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def test_registry_is_deterministic_and_sanitized(self) -> None:
        first = MODULE.render_registry(self.input, RUN_ID, COMMIT, FIXED_TIMESTAMP)
        second = MODULE.render_registry(self.input, RUN_ID, COMMIT, FIXED_TIMESTAMP)

        self.assertEqual(first, second)
        payload = json.dumps(first).encode()
        self.assertNotIn(b"never-logged-value", payload)
        self.assertEqual(first["schema_version"], MODULE.SCHEMA_VERSION)
        self.assertEqual(first["run_id"], RUN_ID)
        self.assertEqual(first["source_commit"], COMMIT)
        self.assertEqual(first["entry_count"], 2)
        self.assertEqual(
            sorted(entry["path"] for entry in first["entries"]),
            ["ca.crt", "secrets/postgres-secrets/postgres-password"],
        )
        for entry in first["entries"]:
            self.assertRegex(entry["sha256"], r"^[0-9a-f]{64}$")
            self.assertEqual(set(entry), {"path", "size", "mode", "sha256"})

    def test_registry_records_expected_hash(self) -> None:
        registry = MODULE.render_registry(self.input, RUN_ID, COMMIT, FIXED_TIMESTAMP)
        entries = {entry["path"]: entry for entry in registry["entries"]}
        import hashlib

        expected = hashlib.sha256(b"public-certificate").hexdigest()
        self.assertEqual(entries["ca.crt"]["sha256"], expected)

    def test_invalid_run_id_is_rejected(self) -> None:
        with self.assertRaisesRegex(MODULE.RegistryError, "LW_CREDENTIAL_REGISTRY_RUN_ID_INVALID"):
            MODULE.render_registry(self.input, "Bad_Run", COMMIT)

    def test_invalid_commit_is_rejected(self) -> None:
        with self.assertRaisesRegex(MODULE.RegistryError, "LW_CREDENTIAL_REGISTRY_COMMIT_INVALID"):
            MODULE.render_registry(self.input, RUN_ID, "not-a-commit")

    def test_empty_input_is_rejected(self) -> None:
        empty = self.root / ".private" / "empty"
        empty.mkdir(parents=True)
        with self.assertRaisesRegex(MODULE.RegistryError, "LW_CREDENTIAL_REGISTRY_INPUT_INVALID"):
            MODULE.render_registry(empty, RUN_ID, COMMIT)

    def test_hidden_file_is_rejected(self) -> None:
        (self.input / ".leftover").write_bytes(b"stray")
        with self.assertRaisesRegex(MODULE.RegistryError, "LW_CREDENTIAL_REGISTRY_INPUT_INVALID"):
            MODULE.render_registry(self.input, RUN_ID, COMMIT)

    def test_empty_file_is_rejected(self) -> None:
        (self.input / "ca.crt").write_bytes(b"")
        with self.assertRaisesRegex(MODULE.RegistryError, "LW_CREDENTIAL_REGISTRY_INPUT_INVALID"):
            MODULE.render_registry(self.input, RUN_ID, COMMIT)

    def test_public_path_is_rejected(self) -> None:
        with self.assertRaisesRegex(
            MODULE.RegistryError, "LW_CREDENTIAL_REGISTRY_PRIVATE_PATH_REQUIRED"
        ):
            MODULE._require_private_path(self.root / "registry.json")

        MODULE._require_private_path(self.root / ".private" / "registry.json")

    def test_output_is_exclusive(self) -> None:
        output = self.root / ".private" / "registry.json"
        MODULE._write_exclusive(output, b"first")

        with self.assertRaises(FileExistsError):
            MODULE._write_exclusive(output, b"second")
        self.assertEqual(output.read_bytes(), b"first")
        if os.name == "posix":
            self.assertEqual(output.stat().st_mode & 0o777, 0o600)


if __name__ == "__main__":
    unittest.main()
