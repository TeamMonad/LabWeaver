from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).parents[2] / "tools" / "update_credential_registry_current.py"
SPEC = importlib.util.spec_from_file_location("update_credential_registry_current", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


COMMIT = "61973471e19c010042967c36012e8d9e0495b611"


class CurrentCredentialRegistryTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.private = self.root / ".private"
        self.private.mkdir(mode=0o700)
        self.targets = {
            "nats-authority-source": self.private / "nats-authority",
            "deployment-bundle": self.private / "configuration-bundle.yml",
            "resource-profile": self.private / "resource-profile.json",
            "replay-authentication": self.private / "resource-replay-auth.json",
            "controller-identity": self.private / "controller-identity.yml",
        }
        source = self.targets["nats-authority-source"]
        source.mkdir(mode=0o700)
        nested = source / "authority"
        nested.mkdir(mode=0o700)
        (nested / "seed.nk").write_text("secret-value\n", encoding="utf-8")
        (nested / "seed.nk").chmod(0o600)
        (nested / "ca.crt").write_text("public-certificate\n", encoding="utf-8")
        (nested / "ca.crt").chmod(0o644)
        for name, path in self.targets.items():
            if name != "nats-authority-source":
                path.write_text(f"{name}\n", encoding="utf-8")
                path.chmod(0o600)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def test_adopt_is_atomic_and_manifest_is_locator_only(self) -> None:
        result = MODULE.adopt(
            root=self.root / "credential-registry",
            run_id="issue126-v21-a1",
            source_commit=COMMIT,
            targets=self.targets,
            require_root=False,
        )

        current = self.root / "credential-registry" / "current"
        manifest = self.root / "credential-registry" / "current.sha256"
        self.assertTrue(current.is_symlink())
        self.assertEqual(result["entry_count"], 5)
        self.assertEqual(MODULE._verify_current(self.root / "credential-registry", require_root=False)["run_id"], "issue126-v21-a1")
        payload = manifest.read_text(encoding="utf-8")
        self.assertNotIn("secret-value", payload)
        self.assertNotIn(str(self.private), payload)
        self.assertEqual({line.split()[0] for line in payload.splitlines()}, {f"current/{name}" for name in MODULE.NAMES})
        self.assertEqual(manifest.stat().st_mode & 0o777, 0o600)

    def test_second_adoption_preserves_previous_version(self) -> None:
        registry = self.root / "credential-registry"
        MODULE.adopt(
            root=registry,
            run_id="issue126-v21-a1",
            source_commit=COMMIT,
            targets=self.targets,
            require_root=False,
        )
        MODULE.adopt(
            root=registry,
            run_id="issue126-v21-a2",
            source_commit=COMMIT,
            targets=self.targets,
            require_root=False,
        )

        self.assertEqual(MODULE._verify_current(registry, require_root=False)["run_id"], "issue126-v21-a2")
        self.assertTrue((registry / "versions" / "issue126-v21-a1" / "current").is_dir())
        self.assertTrue((registry / "versions" / "issue126-v21-a2" / "current").is_dir())

    def test_wider_target_permissions_are_rejected(self) -> None:
        self.targets["resource-profile"].chmod(0o644)
        with self.assertRaisesRegex(MODULE.RegistryError, "LW_CREDENTIAL_REGISTRY_TARGET_PERMISSIONS_INVALID"):
            MODULE.adopt(
                root=self.root / "credential-registry",
                run_id="issue126-v21-a1",
                source_commit=COMMIT,
                targets=self.targets,
                require_root=False,
            )

    def test_public_certificate_permissions_are_allowed_in_authority_source(self) -> None:
        result = MODULE.adopt(
            root=self.root / "credential-registry",
            run_id="issue126-v21-public-cert",
            source_commit=COMMIT,
            targets=self.targets,
            require_root=False,
        )
        self.assertEqual(result["entry_count"], 5)


if __name__ == "__main__":
    unittest.main()
