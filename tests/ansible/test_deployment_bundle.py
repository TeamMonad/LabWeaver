"""Use real age with synthetic keys and isolated application configuration."""

import base64
import copy
from contextlib import redirect_stderr, redirect_stdout
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
            "configurationFile": "config/platform-configuration.json",
            "target": {"namespace": "labweaver-system", "releaseName": "labweaver"},
            "versions": {
                "lockFile": "versions/lock.yaml",
                "packageFile": "versions/package.json",
            },
            "docs": {"operationsFile": "docs/operations.md"},
            "application": {
                "configMaps": configmaps,
                "secrets": secrets,
                "helmValues": {
                    "hostAliases": [],
                    "network": {"kubernetesApiCidrs": ["192.0.2.10/32"]},
                },
                "ansibleVariables": {
                    "platform_application_minio_password": {
                        "secret": "secrets/shared-token"
                    },
                    "platform_application_nats_credentials_file": {
                        "file": "secrets/nats.creds"
                    },
                },
                "workloadInputs": {
                    name: {
                        "env": (
                            dict(configuration.get("env", {}))
                            if configuration["enabled"]
                            else {}
                        ),
                        "secretEnv": (
                            {
                                key: {
                                    "secret": reference.get(
                                        "secret", configuration["secret"]
                                    ),
                                    "key": reference["key"],
                                }
                                for key, reference in configuration.get(
                                    "secretEnv", {}
                                ).items()
                            }
                            if configuration["enabled"]
                            else {}
                        ),
                    }
                    for name, configuration in yaml.safe_load(
                        bundle.WORKLOADS_FILE.read_bytes()
                    )["workloads"].items()
                },
            },
        }
        self.configuration = {
            "lifecycle": {
                "readOnlyRetentionSeconds": 604800,
                "detailedLogRetentionSeconds": 2592000,
                "expiryReminderSeconds": [86400, 3600],
                "preemptionGraceSeconds": 60,
            },
            "runtimePolicy": {
                "defaultContainerUid": 0,
                "allowedCapabilities": [],
                "preauthorizations": [],
                "privilegedAllowed": False,
                "privilegedExecutionLocation": None,
            },
            "builderPolicy": {
                "capabilities": [],
                "allowPrivilegeEscalation": False,
                "entitlements": [],
            },
        }
        self.version = {
            "revision": 7,
            "configuration": self.configuration,
            "configurationSha256": hashlib.sha256(
                json.dumps(
                    self.configuration, sort_keys=True, separators=(",", ":")
                ).encode()
            ).hexdigest(),
            "createdBy": "00000000-0000-7000-8000-000000000001",
            "createdAt": "1970-01-01T00:00:00.000Z",
        }
        write(
            self.root / self.model["configurationFile"],
            json.dumps(self.version).encode(),
        )
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
                            "component": name.replace("_", "-"),
                            "reference": "example.invalid/"
                            + (
                                "control"
                                if name == "control_service"
                                else name.replace("_", "-")
                            )
                            + "@sha256:"
                            + "a" * 64,
                        }
                        for name in yaml.safe_load(bundle.WORKLOADS_FILE.read_bytes())[
                            "images"
                        ]
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

    def save_version(self) -> None:
        self.version["configurationSha256"] = hashlib.sha256(
            json.dumps(
                self.configuration, sort_keys=True, separators=(",", ":")
            ).encode()
        ).hexdigest()
        write(
            self.root / self.model["configurationFile"],
            json.dumps(self.version).encode(),
        )

    def test_settings_export_and_encrypted_import_keep_policy_and_prepare_one_update(
        self,
    ) -> None:
        self.configuration["runtimePolicy"].update(
            allowedCapabilities=["NET_ADMIN", "NET_RAW"],
            privilegedAllowed=True,
            privilegedExecutionLocation={
                "kind": "dedicated_node_pool",
                "nodeSelector": {"labweaver.io/pool": "research"},
                "taintKey": "labweaver.io/research",
                "taintValue": "reserved",
            },
        )
        self.save_version()
        response = self.private / "server-version.json"
        write(response, json.dumps(self.version).encode())
        exported = self.private / "exported/config/platform-configuration.json"
        bundle.export_settings(response, exported)
        self.assertEqual(json.loads(exported.read_bytes()), self.version)
        self.seal()
        shutil.rmtree(self.root)
        update_file = self.private / "restored/update.json"
        result = bundle.import_settings(
            self.archive, self.identity, self.runtime, 12, update_file
        )
        update = json.loads(update_file.read_bytes())
        self.assertEqual(set(update), {"expectedRevision", "configuration"})
        self.assertEqual(update["expectedRevision"], 12)
        self.assertEqual(update["configuration"], self.configuration)
        self.assertEqual(
            update["configuration"]["builderPolicy"],
            {"capabilities": [], "allowPrivilegeEscalation": False, "entitlements": []},
        )
        self.assertEqual(
            result["managedConfigurationSha256"], self.version["configurationSha256"]
        )
        self.assertEqual(update_file.stat().st_mode & 0o777, 0o600)
        self.assertNotIn(self.secret, update_file.read_bytes())
        self.clean_runtime()

    def test_managed_version_hash_revision_fields_and_executor_boundary_are_required(
        self,
    ) -> None:
        cases = []
        wrong_hash = copy.deepcopy(self.version)
        wrong_hash["configuration"]["lifecycle"]["preemptionGraceSeconds"] = 90
        cases.append((wrong_hash, "LW_BUNDLE_CONFIGURATION_DIGEST_MISMATCH"))
        wrong_revision = dict(self.version, revision=8)
        cases.append((wrong_revision, "LW_BUNDLE_CONFIGURATION_REVISION_MISMATCH"))
        for field, value in (
            ("allowedCapabilities", ["ALL"]),
            ("privilegedAllowed", True),
        ):
            invalid = copy.deepcopy(self.version)
            invalid["configuration"]["runtimePolicy"][field] = value
            cases.append((invalid, "LW_BUNDLE_CONFIGURATION_INVALID"))
        secret = copy.deepcopy(self.version)
        secret["configuration"]["token"] = self.secret.decode()
        cases.append((secret, "LW_BUNDLE_CONFIGURATION_INVALID"))
        for version, code in cases:
            with self.subTest(code=code):
                write(
                    self.root / self.model["configurationFile"],
                    json.dumps(version).encode(),
                )
                with self.assertRaisesRegex(bundle.BundleError, code):
                    bundle.load_bundle(self.root)
        write(
            self.root / self.model["configurationFile"],
            json.dumps(self.version).encode(),
        )
        del self.model["configurationFile"]
        self.save_model()
        with self.assertRaisesRegex(bundle.BundleError, "LW_BUNDLE_MODEL_INVALID"):
            bundle.load_bundle(self.root)

    def test_settings_import_failure_retains_existing_output_and_cleans_plaintext(
        self,
    ) -> None:
        self.seal()
        update_file = self.private / "update.json"
        write(update_file, b"keep-existing-request")
        with self.assertRaisesRegex(bundle.BundleError, "LW_BUNDLE_OUTPUT_EXISTS"):
            bundle.import_settings(
                self.archive, self.identity, self.runtime, 0, update_file
            )
        self.assertEqual(update_file.read_bytes(), b"keep-existing-request")
        self.clean_runtime()
        other_key = self.directory / "wrong-import-key.txt"
        self.key(other_key)
        missing = self.private / "never-created.json"
        with self.assertRaisesRegex(bundle.BundleError, "LW_BUNDLE_AGE_FAILED"):
            bundle.import_settings(self.archive, other_key, self.runtime, 0, missing)
        self.assertFalse(missing.exists())
        self.clean_runtime()

    def test_workload_inputs_are_complete_single_source_with_existing_secret_objects(
        self,
    ) -> None:
        inputs = self.model["application"]["workloadInputs"]
        inputs["control-service"]["env"]["LABWEAVER_SERVICE_SCOPES"] = "synthetic.scope"
        self.save_model()
        bundle.derive_bundle(self.root, self.private / "rendered-inputs")
        values = yaml.safe_load(
            (self.private / "rendered-inputs/helm-values.yaml").read_bytes()
        )
        self.assertEqual(values["workloadInputs"], inputs)
        self.assertNotIn("workloads", values)
        del inputs["control-service"]["env"]["LABWEAVER_SERVICE_SCOPES"]
        self.save_model()
        with self.assertRaisesRegex(
            bundle.BundleError, "LW_BUNDLE_WORKLOAD_INPUT_MISSING"
        ):
            bundle.load_bundle(self.root)

    def test_environment_secret_injection_and_helm_override_cannot_make_another_source(
        self,
    ) -> None:
        original = copy.deepcopy(self.model)
        for change, code in (
            (
                {"env": {"ANTHROPIC_AUTH_TOKEN": self.secret.decode()}},
                "LW_BUNDLE_SECRET_REFERENCE_REQUIRED",
            ),
            (
                {"secretEnv": {"TOKEN": {"secret": "not-in-bundle", "key": "token"}}},
                "LW_BUNDLE_SECRET_REFERENCE_INVALID",
            ),
        ):
            self.model = copy.deepcopy(original)
            for group, extra in change.items():
                self.model["application"]["workloadInputs"]["agent-service"][
                    group
                ].update(extra)
            self.save_model()
            with self.assertRaisesRegex(bundle.BundleError, code):
                bundle.load_bundle(self.root)
        self.model = copy.deepcopy(original)
        self.model["application"]["helmValues"]["workloads"] = {
            "control-service": {"env": {}}
        }
        self.save_model()
        with self.assertRaisesRegex(
            bundle.BundleError, "LW_BUNDLE_DERIVED_OVERRIDE_FORBIDDEN"
        ):
            bundle.load_bundle(self.root)

    def test_missing_contract_tool_fails_before_encryption(self) -> None:
        with patch.dict(
            os.environ,
            {
                "LABWEAVER_BUNDLE_CONFIGURATION_TOOL": str(
                    self.directory / "absent-validator"
                )
            },
        ):
            with self.assertRaisesRegex(
                bundle.BundleError, "LW_BUNDLE_CONFIGURATION_TOOL_REQUIRED"
            ):
                bundle.seal_bundle(self.root, self.archive, [self.key()], self.runtime)
        self.assertFalse(self.archive.exists())

    def test_failed_settings_publish_is_atomic_and_cli_does_not_echo_sensitive_io_error(
        self,
    ) -> None:
        output = self.private / "export-pending/version.json"
        arguments = [
            "deployment_bundle.py",
            "settings-export",
            "--version-file",
            str(self.root / self.model["configurationFile"]),
            "--output",
            str(output),
        ]
        stdout = io.StringIO()
        stderr = io.StringIO()
        with patch.object(sys, "argv", arguments), patch.object(
            bundle.os, "link", side_effect=OSError(self.secret.decode())
        ), redirect_stdout(stdout), redirect_stderr(stderr):
            self.assertEqual(bundle.main(), 1)
        self.assertFalse(output.exists())
        self.assertEqual(list(output.parent.iterdir()), [])
        self.assertEqual(stdout.getvalue(), "")
        self.assertEqual(stderr.getvalue().strip(), "LW_BUNDLE_IO_FAILED")
        self.assertNotIn(self.secret.decode(), stderr.getvalue())

    def test_real_helm_uses_bundle_environment_without_legacy_fallback_or_builder_escalation(
        self,
    ) -> None:
        helm = shutil.which("helm")
        self.assertIsNotNone(
            helm, "The pinned Helm renderer is required for Bundle input tests."
        )
        self.configuration["runtimePolicy"].update(
            allowedCapabilities=["SYS_ADMIN"],
            privilegedAllowed=True,
            privilegedExecutionLocation={
                "kind": "virtual_machine_guest",
                "providerBinding": "synthetic-vm",
            },
        )
        self.save_version()
        self.model["application"]["workloadInputs"]["control-service"]["env"][
            "LABWEAVER_SERVICE_SCOPES"
        ] = "synthetic.bundle.scope"
        self.save_model()
        rendered = self.private / "helm-rendered"
        bundle.derive_bundle(self.root, rendered)
        result = subprocess.run(
            [
                helm,
                "template",
                "labweaver",
                str(bundle.WORKLOADS_FILE.parent),
                "--namespace",
                "labweaver-system",
                "--values",
                str(rendered / "helm-values.yaml"),
                "--set-string",
                "workloads.control-service.env.LEGACY_ONLY=must-not-be-rendered",
            ],
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        deployments = {
            item["metadata"]["name"]: item
            for item in yaml.safe_load_all(result.stdout)
            if item and item.get("kind") == "Deployment"
        }
        control = deployments["control-service"]["spec"]["template"]["spec"][
            "containers"
        ][0]
        env = {item["name"]: item for item in control["env"]}
        self.assertEqual(
            env["LABWEAVER_SERVICE_SCOPES"]["value"], "synthetic.bundle.scope"
        )
        self.assertNotIn("LEGACY_ONLY", env)
        builder = deployments["build-executor"]["spec"]["template"]["spec"][
            "containers"
        ][0]
        self.assertFalse(builder["securityContext"]["allowPrivilegeEscalation"])
        self.assertEqual(builder["securityContext"]["capabilities"], {"drop": ["ALL"]})

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
            metadata["applicationObjectsSha256"],
        )
        self.assertEqual(
            values["images"]["control_service"],
            "example.invalid/control@sha256:" + "a" * 64,
        )
        self.assertFalse((rendered / "object-inputs").exists())
        previous = metadata["applicationObjectsSha256"]
        write(
            self.root / "secrets/shared-token",
            b"rotated-synthetic-credential-9876543210",
        )
        second = self.private / "second"
        changed = bundle.derive_bundle(self.root, second)
        self.assertNotEqual(changed["applicationObjectsSha256"], previous)
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

    def test_seal_reports_the_actual_ciphertext_digest_bound_to_the_managed_version(
        self,
    ) -> None:
        result = bundle.seal_bundle(self.root, self.archive, [self.key()], self.runtime)
        self.assertEqual(result["revision"], self.version["revision"])
        self.assertEqual(
            result["managedConfigurationSha256"], self.version["configurationSha256"]
        )
        self.assertEqual(
            result["encryptedBundleSha256"],
            hashlib.sha256(self.archive.read_bytes()).hexdigest(),
        )
        self.assertNotEqual(
            result["encryptedBundleSha256"], result["managedConfigurationSha256"]
        )
        self.assertNotIn(self.secret.decode(), json.dumps(result))
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

    def test_private_marker_cannot_be_bypassed_with_parent_components(self) -> None:
        with self.assertRaisesRegex(
            bundle.BundleError, "LW_BUNDLE_PRIVATE_DIRECTORY_REQUIRED"
        ):
            bundle._private_directory(
                self.private / ".." / "public-runtime", create=True
            )
        self.assertFalse((self.directory / "public-runtime").exists())

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
