"""Use real age with synthetic keys and isolated application configuration."""

import base64
import copy
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tarfile
import tempfile
import time
import unittest
from unittest.mock import patch

import yaml

REPOSITORY = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(REPOSITORY / "tools"))
import deployment_bundle as bundle


def write(path: Path, payload: bytes) -> None:
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    for parent in path.parents:
        if parent.name == ".private":
            break
        if ".private" in parent.parts:
            parent.chmod(0o700)
    path.write_bytes(payload)
    path.chmod(0o600)


class DeploymentBundleTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.directory = Path(self.temporary.name)
        self.private = self.directory / ".private"
        self.private.mkdir(mode=0o700)
        self.root = self.private / "source"
        self.runtime = self.private / "runtime"
        self.archive = self.directory / "installation.age"
        self.identity = self.directory / "synthetic-key.txt"
        self.secret = b"synthetic-secret-never-log-0123456789"
        manifest = json.loads(bundle.DEFAULT_MANIFEST.read_bytes())
        configmaps = {}
        secrets = {}
        for name, keys in manifest["configMaps"].items():
            configmaps[name] = {}
            for key in keys:
                path = f"config/runtime/{name}/{key}"
                configmaps[name][key] = path
                write(self.root / path, b"fixture: true\n")
        jwt_payload = base64.urlsafe_b64encode(
            json.dumps({"nats": {"pub": {"allow": ["$JS.ACK.>"]}}}).encode()
        ).rstrip(b"=")
        credentials = (
            b"-----BEGIN NATS USER JWT-----\na."
            + jwt_payload
            + b".synthetic\n------END NATS USER JWT------\n"
        )
        for name, keys in manifest["secrets"].items():
            secrets[name] = {}
            for key in keys:
                # One credential reference can feed several application objects.
                path = (
                    "secrets/nats.creds"
                    if key == "nats.creds"
                    else "secrets/shared-token"
                )
                secrets[name][key] = path
                write(
                    self.root / path,
                    credentials if key == "nats.creds" else self.secret,
                )
        self.model = {
            "apiVersion": bundle.API_VERSION,
            "revision": 7,
            "target": {"namespace": "labweaver-system", "releaseName": "labweaver"},
            "versions": {
                "lockFile": "versions/lock.yaml",
                "packageFile": "versions/package.json",
            },
            "docs": {"operationsFile": "docs/operations.md"},
            "application": {
                "configMaps": configmaps,
                "secrets": secrets,
                "helmValues": {"hostAliases": []},
                "ansibleVariables": {
                    "platform_application_minio_password": {
                        "secret": "secrets/shared-token"
                    },
                    "platform_application_nats_credentials_file": {
                        "file": "secrets/nats.creds"
                    },
                },
            },
        }
        write(
            self.root / "versions/lock.yaml",
            b"schema_version: 1\nplatform_foundation: {}\n",
        )
        write(
            self.root / "versions/package.json",
            json.dumps(
                {
                    "images": [
                        {
                            "component": "control-service",
                            "reference": "example.invalid/control@sha256:" + "a" * 64,
                        }
                    ]
                }
            ).encode(),
        )
        write(
            self.root / "docs/operations.md",
            b"Synthetic installation. No remote deployment.\n",
        )
        self.save_model()

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def save_model(self) -> None:
        write(self.root / bundle.MODEL_FILE, yaml.safe_dump(self.model).encode())

    def key(self, path: Path | None = None) -> str:
        if shutil.which("age") is None or shutil.which("age-keygen") is None:
            self.fail(
                "age and age-keygen are required. The CI controller installs pinned age v1.3.2."
            )
        path = path or self.identity
        result = subprocess.run(
            ["age-keygen", "-o", str(path)], check=True, capture_output=True, text=True
        )
        path.chmod(0o600)
        self.assertNotIn("AGE-SECRET", result.stderr)
        return subprocess.run(
            ["age-keygen", "-y", str(path)], check=True, capture_output=True, text=True
        ).stdout.strip()

    def seal(self) -> None:
        recipient = self.key()
        self.assertEqual(
            bundle.seal_bundle(self.root, self.archive, [recipient], self.runtime)[
                "revision"
            ],
            7,
        )

    def clean_runtime(self) -> None:
        self.assertEqual(list(self.runtime.iterdir()), [])

    def cli(self, *arguments: str) -> subprocess.CompletedProcess:
        return subprocess.run(
            [
                sys.executable,
                str(REPOSITORY / "tools/deployment_bundle.py"),
                "--runtime",
                str(self.runtime),
                *arguments,
            ],
            capture_output=True,
            text=True,
        )

    def encrypted_tar(self, members: list[tuple[str, bytes, bytes]]) -> None:
        recipient = self.key()
        payload = io.BytesIO()
        with tarfile.open(fileobj=payload, mode="w") as archive:
            for name, value, kind in members:
                info = tarfile.TarInfo(name)
                info.type = kind
                info.size = len(value) if kind == tarfile.REGTYPE else 0
                info.linkname = "../../outside" if kind != tarfile.REGTYPE else ""
                archive.addfile(
                    info, io.BytesIO(value) if kind == tarfile.REGTYPE else None
                )
        self.archive.write_bytes(
            subprocess.run(
                ["age", "-r", recipient],
                input=payload.getvalue(),
                check=True,
                capture_output=True,
            ).stdout
        )

    def test_one_source_derives_all_inputs_and_rotates_shared_credential_once(
        self,
    ) -> None:
        rendered = self.private / "rendered"
        metadata = bundle.derive_bundle(self.root, rendered)
        documents = list(
            yaml.safe_load_all((rendered / "application.yaml").read_bytes())
        )
        self.assertEqual(len(documents), 20)
        self.assertEqual(
            {d["metadata"]["namespace"] for d in documents}, {"labweaver-system"}
        )
        values = yaml.safe_load((rendered / "helm-values.yaml").read_bytes())
        self.assertEqual(
            values["deploymentIdentity"]["configurationBundleSha256"],
            metadata["configurationSha256"],
        )
        self.assertEqual(
            values["images"]["control_service"],
            "example.invalid/control@sha256:" + "a" * 64,
        )
        self.assertFalse((rendered / "object-inputs").exists())
        previous = metadata["configurationSha256"]
        write(
            self.root / "secrets/shared-token",
            b"rotated-synthetic-credential-9876543210",
        )
        second = self.private / "second"
        changed = bundle.derive_bundle(self.root, second)
        self.assertNotEqual(changed["configurationSha256"], previous)
        for document in yaml.safe_load_all((second / "application.yaml").read_bytes()):
            if document["kind"] == "Secret":
                for key, payload in document["data"].items():
                    if key != "nats.creds":
                        self.assertEqual(
                            base64.b64decode(payload),
                            b"rotated-synthetic-credential-9876543210",
                        )

    def test_real_age_restores_exact_source_in_new_directory_and_cleans_success(
        self,
    ) -> None:
        expected = bundle.load_bundle(self.root)[1]
        self.seal()
        self.assertNotIn(self.secret, self.archive.read_bytes())
        self.assertNotIn(b"config/bundle.yaml", self.archive.read_bytes())
        shutil.rmtree(self.root)
        with bundle.opened_bundle(self.archive, self.identity, self.runtime) as (
            restored,
            rendered,
            metadata,
        ):
            self.assertEqual(bundle.load_bundle(restored)[1], expected)
            self.assertEqual(metadata["revision"], 7)
            self.assertEqual(restored.stat().st_mode & 0o777, 0o700)
            for path in restored.rglob("*"):
                if path.is_file():
                    self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            self.assertTrue((rendered / "application.yaml").exists())
        self.clean_runtime()

    def test_wrong_key_fails_and_cleans_partial_plaintext(self) -> None:
        self.seal()
        other_key = self.directory / "wrong-key.txt"
        self.key(other_key)
        with self.assertRaisesRegex(bundle.BundleError, "LW_BUNDLE_AGE_FAILED"):
            with bundle.opened_bundle(self.archive, other_key, self.runtime):
                self.fail("Wrong identity cannot open a Bundle")
        self.clean_runtime()

    def test_missing_key_and_wide_key_permissions_fail_without_printing_paths(
        self,
    ) -> None:
        self.seal()
        for identity in (self.directory / "missing.txt", self.identity):
            with self.subTest(identity=identity.name):
                if identity.exists():
                    identity.chmod(0o644)
                result = self.cli(
                    "exec", "--archive", str(self.archive), "--identity", str(identity)
                )
                self.assertEqual(result.returncode, 1)
                self.assertEqual(result.stderr.strip(), "LW_BUNDLE_IDENTITY_REQUIRED")
                self.assertNotIn(str(self.directory), result.stderr)
                self.clean_runtime()

    def test_tampered_ciphertext_fails_before_consumer_and_cleans(self) -> None:
        self.seal()
        payload = bytearray(self.archive.read_bytes())
        payload[-10] ^= 1
        self.archive.write_bytes(payload)
        result = self.cli(
            "exec", "--archive", str(self.archive), "--identity", str(self.identity)
        )
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stderr.strip(), "LW_BUNDLE_AGE_FAILED")
        self.clean_runtime()

    def test_consumer_error_cleans_without_secret_stdout_stderr(self) -> None:
        self.seal()
        command = "import os, pathlib, sys; p=pathlib.Path(os.environ['LW_BUNDLE_ROOT'])/'secrets/shared-token'; print(p.read_text()); print(p.read_text(), file=sys.stderr); sys.exit(9)"
        result = self.cli(
            "exec",
            "--archive",
            str(self.archive),
            "--identity",
            str(self.identity),
            "--",
            sys.executable,
            "-c",
            command,
        )
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stderr.strip(), "LW_BUNDLE_COMMAND_FAILED")
        self.assertNotIn(self.secret.decode(), result.stdout + result.stderr)
        self.clean_runtime()
        # The same encrypted source remains usable after the failed consumer.
        result = self.cli(
            "exec", "--archive", str(self.archive), "--identity", str(self.identity)
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.clean_runtime()

    def test_consumer_reads_derived_inputs_and_success_cleans(self) -> None:
        self.seal()
        command = "import os, pathlib; p=pathlib.Path(os.environ['LABWEAVER_APPLICATION_VARS_FILE']); assert p.is_file(); assert (pathlib.Path(os.environ['LW_BUNDLE_RENDERED_ROOT'])/'application.yaml').is_file()"
        result = self.cli(
            "exec",
            "--archive",
            str(self.archive),
            "--identity",
            str(self.identity),
            "--",
            sys.executable,
            "-c",
            command,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)["revision"], 7)
        self.clean_runtime()

    def test_consumer_exception_cleans(self) -> None:
        self.seal()
        with self.assertRaisesRegex(RuntimeError, "synthetic failure"):
            with bundle.opened_bundle(self.archive, self.identity, self.runtime):
                raise RuntimeError("synthetic failure")
        self.clean_runtime()

    def test_exited_consumer_cannot_leave_background_descendant_using_plaintext(
        self,
    ) -> None:
        self.seal()
        marker = self.directory / "background-finished"
        child = f"import pathlib,time; time.sleep(2); pathlib.Path({str(marker)!r}).write_text('finished')"
        command = (
            f"import subprocess,sys; subprocess.Popen([sys.executable,'-c',{child!r}])"
        )
        result = self.cli(
            "exec",
            "--archive",
            str(self.archive),
            "--identity",
            str(self.identity),
            "--",
            sys.executable,
            "-c",
            command,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.clean_runtime()
        time.sleep(2.2)
        self.assertFalse(marker.exists())

    def test_sigterm_cleans_and_stops_child(self) -> None:
        self.seal()
        marker = self.directory / "child-started"
        done = self.directory / "child-finished"
        command = f"import pathlib,time; pathlib.Path({str(marker)!r}).write_text('started'); time.sleep(5); pathlib.Path({str(done)!r}).write_text('finished')"
        process = subprocess.Popen(
            [
                sys.executable,
                str(REPOSITORY / "tools/deployment_bundle.py"),
                "--runtime",
                str(self.runtime),
                "exec",
                "--archive",
                str(self.archive),
                "--identity",
                str(self.identity),
                "--",
                sys.executable,
                "-c",
                command,
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        try:
            deadline = time.monotonic() + 10
            while not marker.exists() and time.monotonic() < deadline:
                time.sleep(0.02)
            self.assertTrue(marker.exists())
            process.send_signal(signal.SIGTERM)
            stdout, stderr = process.communicate(timeout=10)
            self.assertEqual(process.returncode, 1)
            self.assertEqual(stderr.strip(), b"LW_BUNDLE_INTERRUPTED")
            self.assertNotIn(self.secret, stdout + stderr)
            self.clean_runtime()
            time.sleep(5.2)
            self.assertFalse(
                done.exists(), "consumer must not outlive cleaned credentials"
            )
        finally:
            if process.poll() is None:
                process.kill()
                process.communicate()

    def test_age_failure_leaves_no_output_or_plaintext(self) -> None:
        recipient = self.key()
        with patch.object(
            bundle, "_age", side_effect=bundle.BundleError("LW_BUNDLE_AGE_FAILED")
        ):
            with self.assertRaisesRegex(bundle.BundleError, "LW_BUNDLE_AGE_FAILED"):
                bundle.seal_bundle(self.root, self.archive, [recipient], self.runtime)
        self.assertFalse(self.archive.exists())
        self.assertEqual(list(self.directory.glob(".bundle-*")), [])
        self.clean_runtime()

    def test_missing_age_fails_explicitly(self) -> None:
        with patch.object(
            bundle.subprocess, "run", side_effect=FileNotFoundError("secret marker")
        ):
            with self.assertRaisesRegex(bundle.BundleError, "^LW_BUNDLE_AGE_REQUIRED$"):
                bundle._age(["--version"], stdout=subprocess.DEVNULL)

    def test_existing_archive_is_preserved(self) -> None:
        self.seal()
        before = hashlib.sha256(self.archive.read_bytes()).digest()
        recipient = subprocess.run(
            ["age-keygen", "-y", str(self.identity)],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
        with self.assertRaisesRegex(bundle.BundleError, "LW_BUNDLE_OUTPUT_EXISTS"):
            bundle.seal_bundle(self.root, self.archive, [recipient], self.runtime)
        self.assertEqual(hashlib.sha256(self.archive.read_bytes()).digest(), before)

    def test_required_fields_duplicate_yaml_keys_and_aliases_fail(self) -> None:
        original = copy.deepcopy(self.model)
        for key in original:
            with self.subTest(key=key):
                self.model = {k: v for k, v in original.items() if k != key}
                self.save_model()
                with self.assertRaises(bundle.BundleError):
                    bundle.load_bundle(self.root)
        for document in (b"revision: 1\nrevision: 2\n", b"a: &a [1]\nb: *a\n"):
            write(self.root / bundle.MODEL_FILE, document)
            with self.assertRaisesRegex(
                bundle.BundleError, "LW_BUNDLE_DOCUMENT_INVALID"
            ):
                bundle.load_bundle(self.root)

    def test_unknown_version_target_and_wrong_revision_type_fail(self) -> None:
        for key, value in (
            ("apiVersion", "unknown"),
            ("revision", True),
            ("revision", 0),
            ("target", {"namespace": "../invalid", "releaseName": "labweaver"}),
        ):
            with self.subTest(key=key, value=value):
                original = self.model[key]
                self.model[key] = value
                self.save_model()
                with self.assertRaises(bundle.BundleError):
                    bundle.load_bundle(self.root)
                self.model[key] = original

    def test_missing_extra_and_empty_files_fail(self) -> None:
        path = self.root / "docs/operations.md"
        path.unlink()
        with self.assertRaisesRegex(bundle.BundleError, "LW_BUNDLE_INVENTORY_MISMATCH"):
            bundle.load_bundle(self.root)
        write(path, b"fixture\n")
        write(self.root / "secrets/unreferenced", b"must never be silently carried")
        with self.assertRaisesRegex(bundle.BundleError, "LW_BUNDLE_INVENTORY_MISMATCH"):
            bundle.load_bundle(self.root)
        (self.root / "secrets/unreferenced").unlink()
        write(path, b"")
        with self.assertRaisesRegex(bundle.BundleError, "LW_BUNDLE_FILE_INVALID"):
            bundle.load_bundle(self.root)

    def test_symlinks_and_wide_private_permissions_fail(self) -> None:
        secret = self.root / "secrets/shared-token"
        secret.unlink()
        secret.symlink_to(self.directory / "outside-secret")
        with self.assertRaisesRegex(bundle.BundleError, "LW_BUNDLE_PATH_INVALID"):
            bundle.load_bundle(self.root)
        secret.unlink()
        write(secret, self.secret)
        self.private.chmod(0o755)
        with self.assertRaisesRegex(
            bundle.BundleError, "LW_BUNDLE_PERMISSIONS_INVALID"
        ):
            bundle.load_bundle(self.root)

    def test_external_and_parent_secret_references_fail(self) -> None:
        bindings = self.model["application"]["secrets"]["access-service-secrets"]
        for reference in (
            "../secret",
            "/etc/secret",
            "secrets/../secret",
            "secrets\\secret",
            "config/bundle.yaml",
        ):
            with self.subTest(reference=reference):
                bindings["database-url"] = reference
                self.save_model()
                with self.assertRaisesRegex(
                    bundle.BundleError, "LW_BUNDLE_REFERENCE_INVALID"
                ):
                    bundle.load_bundle(self.root)

    def test_unknown_object_and_derived_overrides_fail(self) -> None:
        original = copy.deepcopy(self.model)
        self.model["application"]["secrets"]["extra-secret"] = {
            "token": "secrets/shared-token"
        }
        self.save_model()
        with self.assertRaisesRegex(bundle.BundleError, "LW_BUNDLE_MODEL_INVALID"):
            bundle.load_bundle(self.root)
        for key in bundle.RESERVED_VARIABLES:
            with self.subTest(key=key):
                self.model = copy.deepcopy(original)
                self.model["application"]["ansibleVariables"][key] = "override"
                self.save_model()
                with self.assertRaisesRegex(
                    bundle.BundleError, "LW_BUNDLE_DERIVED_OVERRIDE_FORBIDDEN"
                ):
                    bundle.load_bundle(self.root)

    def test_literal_credentials_templates_and_helm_secret_references_fail(
        self,
    ) -> None:
        variables = self.model["application"]["ansibleVariables"]
        for value in ("plaintext-password", {"nested": "plaintext-password"}):
            variables["platform_application_minio_password"] = value
            self.save_model()
            with self.assertRaisesRegex(
                bundle.BundleError, "LW_BUNDLE_SECRET_REFERENCE_REQUIRED"
            ):
                bundle.load_bundle(self.root)
        variables["platform_application_minio_password"] = {
            "secret": "secrets/shared-token"
        }
        variables["platform_application_hostname"] = "{{ unsafe }}"
        self.save_model()
        with self.assertRaisesRegex(bundle.BundleError, "LW_BUNDLE_TEMPLATE_FORBIDDEN"):
            bundle.load_bundle(self.root)
        del variables["platform_application_hostname"]
        self.model["application"]["helmValues"]["credential"] = {
            "secret": "secrets/shared-token"
        }
        self.save_model()
        with self.assertRaisesRegex(
            bundle.BundleError, "LW_BUNDLE_HELM_SECRET_FORBIDDEN"
        ):
            bundle.load_bundle(self.root)

    def test_secret_text_is_unsafe_scalar_not_an_ansible_template(self) -> None:
        write(
            self.root / "secrets/shared-token",
            b"synthetic-{{ lookup('env','PATH') }}-value",
        )
        rendered = self.private / "rendered"
        bundle.derive_bundle(self.root, rendered)
        from ansible.parsing.dataloader import DataLoader
        from ansible.template import Templar

        loaded = DataLoader().load((rendered / "ansible-vars.yaml").read_text())
        value = loaded["platform_application_minio_password"]
        self.assertEqual(Templar(loader=DataLoader()).template(value), value)
        self.assertIn("{{ lookup", value)

    def test_private_key_and_duplicate_secret_outside_secret_directory_fail(
        self,
    ) -> None:
        for payload in (b"-----BEGIN PRIVATE KEY-----\nsynthetic\n", self.secret):
            write(self.root / "docs/operations.md", payload)
            with self.assertRaisesRegex(
                bundle.BundleError, "LW_BUNDLE_SECRET_LOCATION_INVALID"
            ):
                bundle.load_bundle(self.root)

    def test_age_identity_cannot_be_carried_as_a_declared_application_secret(
        self,
    ) -> None:
        for payload in (
            b"AGE-SECRET-KEY-1SYNTHETIC",
            b"age-encryption.org/v1\nsynthetic encrypted identity",
        ):
            write(self.root / "secrets/shared-token", payload)
            with self.assertRaisesRegex(
                bundle.BundleError, "LW_BUNDLE_IDENTITY_MUST_BE_SEPARATE"
            ):
                bundle.load_bundle(self.root)

    def test_yaml_parser_errors_do_not_echo_secret_or_private_paths(self) -> None:
        write(self.root / bundle.MODEL_FILE, b"key: [" + self.secret + b"\n")
        result = self.cli("validate", "--bundle", str(self.root))
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stderr.strip(), "LW_BUNDLE_DOCUMENT_INVALID")
        self.assertNotIn(self.secret.decode(), result.stdout + result.stderr)
        self.assertNotIn(str(self.private), result.stdout + result.stderr)
        self.clean_runtime()

    def test_nats_permission_check_is_preserved_in_encrypted_export(self) -> None:
        write(self.root / "secrets/nats.creds", b"not a valid credential")
        recipient = self.key()
        with self.assertRaisesRegex(
            bundle.objects.BundleError, "LW_PLATFORM_NATS_CREDENTIALS_INVALID"
        ):
            bundle.seal_bundle(self.root, self.archive, [recipient], self.runtime)
        self.assertFalse(self.archive.exists())
        self.clean_runtime()

    def test_tar_parent_absolute_symlink_hardlink_and_duplicate_members_fail(
        self,
    ) -> None:
        for members in (
            [("../../outside", b"bad", tarfile.REGTYPE)],
            [("/outside", b"bad", tarfile.REGTYPE)],
            [("secrets/link", b"", tarfile.SYMTYPE)],
            [("secrets/link", b"", tarfile.LNKTYPE)],
            [
                ("config/bundle.yaml", b"one", tarfile.REGTYPE),
                ("config/bundle.yaml", b"two", tarfile.REGTYPE),
            ],
        ):
            with self.subTest(members=[m[0] for m in members]):
                self.identity.unlink(missing_ok=True)
                self.encrypted_tar(members)
                with self.assertRaises(bundle.BundleError):
                    with bundle.opened_bundle(
                        self.archive, self.identity, self.runtime
                    ):
                        self.fail("Invalid archive must not be consumed")
                self.clean_runtime()
                self.assertFalse((self.directory.parent / "outside").exists())

    def test_size_and_count_limits_fail_before_age(self) -> None:
        with patch.object(bundle, "MAX_TOTAL_BYTES", 20):
            with self.assertRaisesRegex(bundle.BundleError, "LW_BUNDLE_LIMIT_EXCEEDED"):
                bundle.load_bundle(self.root)
        with patch.object(bundle, "MAX_FILES", 2):
            with self.assertRaisesRegex(bundle.BundleError, "LW_BUNDLE_LIMIT_EXCEEDED"):
                bundle.load_bundle(self.root)
        write(
            self.root / "docs/operations.md", b"x" * (bundle.objects.MAX_FILE_BYTES + 1)
        )
        with self.assertRaisesRegex(bundle.BundleError, "LW_BUNDLE_FILE_INVALID"):
            bundle.load_bundle(self.root)


if __name__ == "__main__":
    unittest.main()
