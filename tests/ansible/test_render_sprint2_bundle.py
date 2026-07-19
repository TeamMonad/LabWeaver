import importlib.util
import json
import tempfile
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).parents[2] / "tools" / "render_sprint2_bundle.py"
SPEC = importlib.util.spec_from_file_location("render_sprint2_bundle", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class Sprint2BundleTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.manifest = self.root / "manifest.json"
        self.manifest.write_text(
            json.dumps(
                {
                    "apiVersion": MODULE.API_VERSION,
                    "namespace": "labweaver-system",
                    "configMaps": {"service-config": ["config.yaml"]},
                    "secrets": {"service-secrets": ["token"]},
                }
            ),
            encoding="utf-8",
        )
        config = self.root / "input" / "configmaps" / "service-config"
        secret = self.root / "input" / "secrets" / "service-secrets"
        config.mkdir(parents=True)
        secret.mkdir(parents=True)
        (config / "config.yaml").write_text("enabled: true\n", encoding="utf-8")
        (secret / "token").write_bytes(b"not-logged")

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def test_render_is_deterministic_and_base64_encodes_secrets(self) -> None:
        first = MODULE.render_bundle(self.manifest, self.root / "input")
        second = MODULE.render_bundle(self.manifest, self.root / "input")

        self.assertEqual(first, second)
        self.assertNotIn(b"not-logged", first)
        self.assertIn(b"bm90LWxvZ2dlZA==", first)
        self.assertEqual(first.count(b"---\n"), 2)

    def test_extra_file_is_rejected(self) -> None:
        extra = self.root / "input" / "secrets" / "service-secrets" / "unexpected"
        extra.write_text("value", encoding="utf-8")

        with self.assertRaisesRegex(MODULE.BundleError, "LW_SPRINT2_BUNDLE_INPUT_INCOMPLETE"):
            MODULE.render_bundle(self.manifest, self.root / "input")

    def test_output_is_exclusive(self) -> None:
        output = self.root / "bundle.yaml"
        MODULE._write_exclusive(output, b"first")

        with self.assertRaises(FileExistsError):
            MODULE._write_exclusive(output, b"second")
        self.assertEqual(output.read_bytes(), b"first")

    def test_checked_in_manifest_declares_the_reset_object_set(self) -> None:
        manifest_path = MODULE_PATH.parents[1] / "deploy" / "config" / "sprint2-bundle-manifest.json"
        manifest = MODULE._load_manifest(manifest_path)

        self.assertEqual(len(manifest["configMaps"]), 9)
        self.assertEqual(len(manifest["secrets"]), 9)
        self.assertIn("buildkit-client.crt", manifest["secrets"]["build-executor-secrets"])
        self.assertEqual(manifest["namespace"], "labweaver-system")

        foundation_path = (
            MODULE_PATH.parents[1]
            / "deploy"
            / "config"
            / "sprint2-foundation-bundle-manifest.json"
        )
        foundation = MODULE._load_manifest(foundation_path)
        self.assertEqual(set(foundation["configMaps"]), {"nats-config"})
        self.assertEqual(
            set(foundation["secrets"]),
            {"postgres-secrets", "nats-server-secrets", "minio-secrets"},
        )
        self.assertEqual(foundation["namespace"], "labweaver-data")

    def test_public_output_path_is_rejected(self) -> None:
        with self.assertRaisesRegex(MODULE.BundleError, "LW_SPRINT2_BUNDLE_PRIVATE_PATH_REQUIRED"):
            MODULE._require_private_path(self.root / "bundle.yaml")

        MODULE._require_private_path(self.root / ".private" / "bundle.yaml")


if __name__ == "__main__":
    unittest.main()
