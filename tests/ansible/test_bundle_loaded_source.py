"""Source facts come from the age-authenticated input snapshot, never ambient reports."""

import hashlib
import json
import os
import sys
import unittest
from unittest.mock import patch

import test_deployment_bundle as fixtures


class BundleLoadedSourceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.fixture = fixtures.DeploymentBundleTests(methodName="runTest")
        self.fixture.setUp()
        self.addCleanup(self.fixture.tearDown)

    def test_source_is_only_emitted_after_authenticated_load_and_hashes_same_input(
        self,
    ):
        fixture = self.fixture
        rendered = fixture.private / "derive-only"
        fixtures.bundle.derive_bundle(fixture.root, rendered)
        self.assertFalse((rendered / "platform-configuration-source.json").exists())
        recipient = fixture.key()
        fixtures.bundle.seal_bundle(
            fixture.root, fixture.archive, [recipient], fixture.runtime
        )
        expected = hashlib.sha256(fixture.archive.read_bytes()).hexdigest()
        decrypt = fixtures.bundle._age

        def replace_original_path(argv, **kwargs):
            fixture.archive.write_bytes(b"changed-original-path-after-capture")
            decrypt(argv, **kwargs)

        with patch.object(fixtures.bundle, "_age", side_effect=replace_original_path):
            with fixtures.bundle.opened_bundle(
                fixture.archive, fixture.identity, fixture.runtime
            ) as (_, loaded, metadata):
                source_file = loaded / "platform-configuration-source.json"
                source = json.loads(source_file.read_bytes())
                self.assertEqual(source_file.stat().st_mode & 0o777, 0o600)
                self.assertEqual(source["encryptedBundleSha256"], expected)
                self.assertEqual(metadata["encryptedBundleSha256"], expected)
                self.assertEqual(
                    source["configurationSha256"],
                    fixture.version["configurationSha256"],
                )
                self.assertEqual(source["desiredRevision"], fixture.version["revision"])
                self.assertEqual(source["installation"], fixture.model["target"])
                self.assertNotIn(fixture.secret, source_file.read_bytes())
        fixture.clean_runtime()

    def test_exec_replaces_ambient_source_and_initial_settings_do_not_claim_loaded_version(
        self,
    ):
        fixture = self.fixture
        recipient = fixture.key()
        fixtures.bundle.seal_bundle(
            fixture.root, fixture.archive, [recipient], fixture.runtime
        )
        marker = fixture.directory / "source-checked"
        command = (
            "import os,pathlib,json; "
            "p=pathlib.Path(os.environ['LABWEAVER_PLATFORM_CONFIGURATION_SOURCE_FILE']); "
            "assert p.parent == pathlib.Path(os.environ['LW_BUNDLE_RENDERED_ROOT']); "
            "assert json.loads(p.read_bytes())['desiredRevision']==7; "
            "pathlib.Path(os.environ['SYNTHETIC_MARKER']).write_text('checked')"
        )
        with patch.dict(
            os.environ,
            {
                "LABWEAVER_PLATFORM_CONFIGURATION_SOURCE_FILE": "/not-a-source",
                "SYNTHETIC_MARKER": str(marker),
            },
        ):
            result = fixture.cli(
                "exec",
                "--archive",
                str(fixture.archive),
                "--identity",
                str(fixture.identity),
                "--",
                sys.executable,
                "-c",
                command,
            )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(marker.read_text(), "checked")
        self.assertNotIn(fixture.secret.decode(), result.stdout + result.stderr)
        fixture.clean_runtime()

        fixture.archive.unlink()
        fixture.model.update(configurationMode="initial_settings", revision=1)
        (fixture.root / fixture.model["configurationFile"]).unlink()
        fixtures.bundle.initialize_settings(
            None, fixture.root / fixture.model["configurationFile"]
        )
        fixture.save_model()
        fixtures.bundle.seal_bundle(
            fixture.root, fixture.archive, [recipient], fixture.runtime
        )
        with fixtures.bundle.opened_bundle(
            fixture.archive, fixture.identity, fixture.runtime
        ) as (_, loaded, _):
            self.assertFalse((loaded / "platform-configuration-source.json").exists())
        with patch.dict(
            os.environ,
            {"LABWEAVER_PLATFORM_CONFIGURATION_SOURCE_FILE": "/stale-source"},
        ):
            result = fixture.cli(
                "exec",
                "--archive",
                str(fixture.archive),
                "--identity",
                str(fixture.identity),
                "--",
                sys.executable,
                "-c",
                "import os; assert 'LABWEAVER_PLATFORM_CONFIGURATION_SOURCE_FILE' not in os.environ",
            )
        self.assertEqual(result.returncode, 0, result.stderr)
        fixture.clean_runtime()

    def test_wrong_identity_never_emits_a_source(self):
        fixture = self.fixture
        recipient = fixture.key()
        fixtures.bundle.seal_bundle(
            fixture.root, fixture.archive, [recipient], fixture.runtime
        )
        wrong = fixture.directory / "wrong-identity"
        fixture.key(wrong)
        with self.assertRaisesRegex(
            fixtures.bundle.BundleError, "LW_BUNDLE_AGE_FAILED"
        ):
            with fixtures.bundle.opened_bundle(fixture.archive, wrong, fixture.runtime):
                self.fail("wrong identity must not load a version")
        fixture.clean_runtime()


if __name__ == "__main__":
    unittest.main()
