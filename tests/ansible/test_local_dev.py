"""Local Kind bundle generation tests."""

from __future__ import annotations

import base64
import json
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import yaml


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))
import local_dev  # noqa: E402
import local_dev_e2e  # noqa: E402


class LocalDevBundleTests(unittest.TestCase):
    def test_local_kind_workspace_configuration_requires_explicit_supported_mode(self) -> None:
        with patch.object(
            local_dev,
            "load_yaml",
            return_value={
                "localValidation": {
                    "mode": "kind",
                    "storageClassName": "standard",
                    "accessMode": "ReadWriteOnce",
                }
            },
        ):
            self.assertEqual(
                local_dev.local_kind_workspace_configuration(),
                ("standard", "ReadWriteOnce"),
            )

        with patch.object(
            local_dev,
            "load_yaml",
            return_value={
                "localValidation": {
                    "mode": "kind",
                    "storageClassName": "standard",
                    "accessMode": "ReadWriteOncePod",
                }
            },
        ):
            with self.assertRaisesRegex(
                local_dev.LocalDevError,
                "valid localValidation workspace configuration",
            ):
                local_dev.local_kind_workspace_configuration()

    def test_playwright_summary_keeps_all_terminal_result_counts(self) -> None:
        logs = """
        Running 5 tests using 2 workers

          2 failed
            [teacher] › e2e/teacher/authoring.live.spec.mjs:90:1 › teacher authors
            [student] › e2e/student/sprint2-flow.live.spec.mjs:283:1 › student provisions
          1 skipped
          1 interrupted
          1 flaky
          3 passed (3.3m)
        """

        self.assertEqual(
            local_dev_e2e.playwright_summary(logs),
            "2 failed, 1 skipped, 1 interrupted, 1 flaky, 3 passed, (3.3m)",
        )

    def test_playwright_summary_ignores_nonterminal_count_text(self) -> None:
        logs = """
        1 failed assertion appears in diagnostic text

          2 failed
          3 passed (3.3m)
        """

        self.assertEqual(local_dev_e2e.playwright_summary(logs), "2 failed, 3 passed, (3.3m)")

    def test_bounded_diagnostic_keeps_head_and_tail_and_redacts_secrets(self) -> None:
        secret = "teacher-password"
        logs = (
            "teacher failure: authoring request returned 403\n"
            "  at teacher.spec.mjs:42:7\n"
            f"authorization: Bearer {secret}\n"
            + ("middle stack frame\n" * 1000)
            + "student failure: provisioning request returned 500\n"
            + "  at student.spec.mjs:84:9\n"
        )

        detail = local_dev_e2e._redact_values(logs, [secret])

        self.assertLessEqual(len(detail), local_dev_e2e.DETAIL_LIMIT)
        self.assertIn("teacher failure: authoring request returned 403", detail)
        self.assertIn("student failure: provisioning request returned 500", detail)
        self.assertIn("teacher-password", logs)
        self.assertNotIn(secret, detail)
        self.assertIn("<redacted>", detail)
        self.assertIn("output truncated; showing beginning and end", detail)

    def test_run_command_decodes_utf8_process_output(self) -> None:
        result = local_dev_e2e.run_command(
            [
                sys.executable,
                "-c",
                "import sys; sys.stdout.buffer.write('中文诊断'.encode('utf-8'))",
            ],
            capture=True,
        )

        self.assertEqual(result.stdout, "中文诊断")

    def test_run_command_reports_invalid_utf8_process_output(self) -> None:
        with self.assertRaisesRegex(
            local_dev_e2e.HarnessError,
            r"command output is not valid UTF-8: " + re.escape(sys.executable),
        ):
            local_dev_e2e.run_command(
                [
                    sys.executable,
                    "-c",
                    "import sys; sys.stdout.buffer.write(bytes([0x93]))",
                ],
                capture=True,
            )

    def test_preload_foundation_images_pulls_digests_in_each_kind_node(self) -> None:
        commands: list[list[str]] = []

        def capture_run(argv: list[str], **_kwargs: object) -> subprocess.CompletedProcess[str]:
            commands.append(argv)
            if argv[:4] == ["kind", "get", "nodes", "--name"]:
                return subprocess.CompletedProcess(argv, 0, stdout="labweaver-local-test-control-plane\n")
            return subprocess.CompletedProcess(argv, 0)

        with patch.object(local_dev, "run", side_effect=capture_run), patch.object(
            local_dev, "CLUSTER", "labweaver-local-test"
        ):
            local_dev.preload_foundation_images()

        expected_images = [
            local_dev.POSTGRES_IMAGE,
            local_dev.NATS_IMAGE,
            local_dev.MINIO_IMAGE,
            local_dev.KEYCLOAK_IMAGE,
            local_dev.NATS_BOX_IMAGE,
        ]
        self.assertEqual(
            commands,
            [["kind", "get", "nodes", "--name", "labweaver-local-test"]]
            + [
                ["docker", "exec", "labweaver-local-test-control-plane", "crictl", "pull", image]
                for image in expected_images
            ],
        )

    def test_cert_creates_nested_output_and_verifies_chain(self) -> None:
        openssl = shutil.which("openssl")
        if openssl is None:
            self.skipTest("openssl is required by the local Kind stack")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            authority = root / "authority"
            authority.mkdir()
            ca_key = authority / "ca.key"
            ca_crt = authority / "ca.crt"
            subprocess.run(
                [
                    openssl,
                    "req",
                    "-x509",
                    "-newkey",
                    "rsa:2048",
                    "-nodes",
                    "-keyout",
                    str(ca_key),
                    "-out",
                    str(ca_crt),
                    "-subj",
                    "/CN=LabWeaver local test CA",
                    "-days",
                    "1",
                ],
                check=True,
                capture_output=True,
                text=True,
            )
            output = root / "generated" / "portal"
            key, certificate = local_dev.cert(
                output,
                ca_key,
                ca_crt,
                "portal",
                ["DNS:portal.local"],
            )
            self.assertTrue(key.is_file())
            self.assertGreater(key.stat().st_size, 0)
            self.assertTrue(certificate.is_file())
            self.assertGreater(certificate.stat().st_size, 0)
            self.assertFalse((output / "portal.csr").exists())
            self.assertFalse((output / "portal.ext").exists())
            subprocess.run(
                [openssl, "verify", "-CAfile", str(ca_crt), str(certificate)],
                check=True,
                capture_output=True,
                text=True,
            )

    def test_local_registry_fixture_manifest_uses_valid_labels_and_endpoint_slice(self) -> None:
        objects = local_dev.local_registry_objects("172.18.0.2")

        self.assertEqual([document["kind"] for document in objects], ["Service", "EndpointSlice"])
        endpoint_slice = objects[1]
        self.assertEqual(endpoint_slice["apiVersion"], "discovery.k8s.io/v1")
        self.assertEqual(endpoint_slice["metadata"]["labels"]["kubernetes.io/service-name"], "local-registry")
        self.assertEqual(endpoint_slice["endpoints"][0]["addresses"], ["172.18.0.2"])
        local_dev.validate_kubernetes_label_maps(objects)

    def test_kubernetes_label_validation_covers_portal_and_oidc_probe(self) -> None:
        applied: list[list[dict[str, object]]] = []

        def capture_apply(_kubeconfig: Path, objects: list[dict[str, object]]) -> None:
            applied.append(objects)
            local_dev.validate_kubernetes_label_maps(objects)

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            portal_key = root / "portal.key"
            portal_crt = root / "portal.crt"
            portal_ca = root / "portal-ca.crt"
            probe_foundation = root / "foundation"
            (probe_foundation / "authority").mkdir(parents=True)
            (probe_foundation / "authority" / "ca.crt").write_bytes(b"ca")
            portal_key.write_bytes(b"key")
            portal_crt.write_bytes(b"certificate")
            portal_ca.write_bytes(b"ca")
            with (
                patch.object(local_dev, "apply", side_effect=capture_apply),
                patch.object(local_dev, "wait_rollout"),
                patch.object(local_dev, "start_admin_pod"),
                patch.object(local_dev, "wait_command"),
                patch.object(local_dev, "delete_admin_pod"),
                patch.object(
                    local_dev,
                    "kubectl",
                    return_value=local_dev.subprocess.CompletedProcess([], 0),
                ),
            ):
                local_dev.deploy_local_portal(
                    root,
                    root,
                    {"web": "localhost:5001/labweaver/web@sha256:" + "a" * 64},
                    portal_key,
                    portal_crt,
                    portal_ca,
                )
                local_dev.wait_local_oidc_issuer(Path("kubeconfig"), 38080, probe_foundation)

        self.assertEqual(len(applied), 3)
        portal_objects, probe_secret, probe_objects = applied
        self.assertEqual(probe_secret[0]["kind"], "Secret")
        self.assertEqual(probe_secret[0]["metadata"]["name"], "local-dev-oidc-probe-bootstrap")
        self.assertEqual(probe_secret[0]["data"], {"keycloak-ca.pem": base64.b64encode(b"ca").decode()})
        portal_deployment = next(
            document for document in portal_objects if document["kind"] == "Deployment"
        )
        portal_policies = [
            document
            for document in portal_objects
            if document["kind"] == "NetworkPolicy"
        ]
        self.assertEqual(
            {document["metadata"]["name"] for document in portal_policies},
            {"local-dev-portal", "local-dev-oidc-egress"},
        )
        portal_policy = next(
            document for document in portal_policies
            if document["metadata"]["name"] == "local-dev-portal"
        )
        oidc_policy = next(
            document for document in portal_policies
            if document["metadata"]["name"] == "local-dev-oidc-egress"
        )
        self.assertEqual(
            portal_deployment["metadata"]["labels"]["labweaver.local-dev.owner"],
            local_dev.KUBERNETES_OWNER_LABEL_VALUE,
        )
        self.assertEqual(
            portal_policy["spec"]["ingress"][0]["from"][1]["podSelector"]["matchLabels"][
                "labweaver.local-dev.owner"
            ],
            local_dev.KUBERNETES_OWNER_LABEL_VALUE,
        )
        self.assertEqual(
            oidc_policy["spec"]["podSelector"]["matchExpressions"],
            [{
                "key": "app.kubernetes.io/name",
                "operator": "In",
                "values": list(local_dev.LOCAL_OIDC_CALLER_WORKLOADS),
            }],
        )
        self.assertEqual(
            oidc_policy["spec"]["egress"],
            [{
                "to": [{"podSelector": {"matchLabels": {"app": "local-dev-portal"}}}],
                "ports": [{"protocol": "TCP", "port": 8443}],
            }],
        )
        probe_policy = probe_objects[0]
        self.assertEqual(
            probe_policy["spec"]["podSelector"]["matchLabels"]["labweaver.local-dev.owner"],
            local_dev.KUBERNETES_OWNER_LABEL_VALUE,
        )

    def test_oidc_probe_cleans_its_resources_when_pod_start_fails(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            foundation = root / "foundation"
            (foundation / "authority").mkdir(parents=True)
            (foundation / "authority" / "ca.crt").write_bytes(b"ca")
            with (
                patch.object(local_dev, "apply"),
                patch.object(local_dev, "start_admin_pod", side_effect=RuntimeError("pod start failed")),
                patch.object(local_dev, "wait_command") as wait_command,
                patch.object(local_dev, "delete_admin_pod") as delete_admin_pod,
                patch.object(
                    local_dev,
                    "kubectl",
                    return_value=local_dev.subprocess.CompletedProcess([], 0),
                ) as kubectl,
            ):
                with self.assertRaisesRegex(RuntimeError, "pod start failed"):
                    local_dev.wait_local_oidc_issuer(Path("kubeconfig"), 38080, foundation)

            wait_command.assert_not_called()
            delete_admin_pod.assert_called_once_with(
                Path("kubeconfig"), "local-dev-oidc-probe-bootstrap"
            )
            delete_args = [call.args[1] for call in kubectl.call_args_list]
            self.assertIn(
                [
                    "-n", local_dev.DATA_NAMESPACE, "delete", "secret",
                    "local-dev-oidc-probe-bootstrap", "--ignore-not-found",
                ],
                delete_args,
            )
            self.assertIn(
                [
                    "-n", local_dev.DATA_NAMESPACE, "delete", "networkpolicy",
                    "local-dev-oidc-probe-bootstrap-egress", "--ignore-not-found",
                ],
                delete_args,
            )

    def test_minio_bootstrap_creates_object_lock_bucket_and_verifies_versioning(self) -> None:
        calls: list[tuple[list[str], bool]] = []
        bucket = "labweaver-artifacts"
        target = f"local/{bucket}"

        def minio(command: list[str], *, check: bool = True) -> subprocess.CompletedProcess[str]:
            calls.append((command, check))
            if command == ["ls", "--json", "local"]:
                return subprocess.CompletedProcess(command, 0, stdout="")
            if command == ["mb", "--with-lock", "--ignore-existing", target]:
                return subprocess.CompletedProcess(command, 0)
            if command == ["retention", "info", "--default", target, "--json"]:
                return subprocess.CompletedProcess(
                    command,
                    0,
                    stdout=json.dumps({
                        "op": "info",
                        "enabled": "Enabled",
                        "mode": "",
                        "validity": "0",
                        "status": "success",
                    }),
                )
            if command == ["version", "enable", target]:
                return subprocess.CompletedProcess(command, 0)
            if command == ["version", "info", target, "--json"]:
                return subprocess.CompletedProcess(
                    command,
                    0,
                    stdout=json.dumps({
                        "status": "success",
                        "versioning": {"status": "Enabled"},
                    }),
                )
            raise AssertionError(f"unexpected MinIO command: {command}")

        local_dev.bootstrap_minio_bucket(minio, bucket)

        self.assertEqual(
            [command for command, _check in calls],
            [
                ["ls", "--json", "local"],
                ["mb", "--with-lock", "--ignore-existing", target],
                ["retention", "info", "--default", target, "--json"],
                ["version", "enable", target],
                ["version", "info", target, "--json"],
            ],
        )
        self.assertTrue(all(not check for _command, check in calls))

    def test_minio_bootstrap_rejects_non_enabled_or_invalid_version_status(self) -> None:
        bucket = "labweaver-artifacts"
        target = f"local/{bucket}"
        version_responses = (
            json.dumps({
                "status": "success",
                "versioning": {"status": "Suspended"},
            }),
            "not-json",
        )

        for version_stdout in version_responses:
            with self.subTest(version_stdout=version_stdout):
                calls: list[list[str]] = []

                def minio(
                    command: list[str], *, check: bool = True
                ) -> subprocess.CompletedProcess[str]:
                    calls.append(command)
                    if command == ["ls", "--json", "local"]:
                        return subprocess.CompletedProcess(
                            command,
                            0,
                            stdout=json.dumps({
                                "status": "success",
                                "type": "folder",
                                "key": f"{bucket}/",
                            }),
                        )
                    if command == ["retention", "info", "--default", target, "--json"]:
                        return subprocess.CompletedProcess(
                            command,
                            0,
                            stdout=json.dumps({
                                "op": "info",
                                "enabled": "Enabled",
                                "mode": "",
                                "validity": "0",
                                "status": "success",
                            }),
                        )
                    if command == ["version", "enable", target]:
                        return subprocess.CompletedProcess(command, 0)
                    if command == ["version", "info", target, "--json"]:
                        return subprocess.CompletedProcess(command, 0, stdout=version_stdout)
                    raise AssertionError(f"unexpected MinIO command: {command}")

                with self.assertRaises(local_dev.LocalDevError):
                    local_dev.bootstrap_minio_bucket(minio, bucket)
                self.assertEqual(calls[-1], ["version", "info", target, "--json"])

    def test_minio_bootstrap_fails_existing_bucket_without_object_lock_before_mutation(self) -> None:
        calls: list[list[str]] = []
        bucket = "labweaver-artifacts"
        target = f"local/{bucket}"

        def minio(command: list[str], *, check: bool = True) -> subprocess.CompletedProcess[str]:
            calls.append(command)
            if command == ["ls", "--json", "local"]:
                return subprocess.CompletedProcess(
                    command,
                    0,
                    stdout=json.dumps({
                        "status": "success",
                        "type": "folder",
                        "key": f"{bucket}/",
                    }),
                )
            if command == ["retention", "info", "--default", target, "--json"]:
                return subprocess.CompletedProcess(
                    command,
                    1,
                    stdout=json.dumps({
                        "status": "error",
                        "error": {"message": "Remote bucket does not support locking"},
                    }),
                    stderr="command terminated with exit code 1\n",
                )
            raise AssertionError(f"unexpected MinIO command: {command}")

        with self.assertRaisesRegex(local_dev.LocalDevError, "Object Lock is not enabled"):
            local_dev.bootstrap_minio_bucket(minio, bucket)

        self.assertEqual(
            calls,
            [
                ["ls", "--json", "local"],
                ["retention", "info", "--default", target, "--json"],
            ],
        )

    def test_kubernetes_label_validation_rejects_path_owner_value(self) -> None:
        invalid_objects = [
            {"metadata": {"labels": {"labweaver.local-dev.owner": "tools/local_dev.py"}}}
        ]
        with self.assertRaises(local_dev.LocalDevError):
            local_dev.validate_kubernetes_label_maps(invalid_objects)
        with patch.object(local_dev, "run") as run_mock:
            with self.assertRaises(local_dev.LocalDevError):
                local_dev.apply(Path("kubeconfig"), invalid_objects)
            run_mock.assert_not_called()

    def test_agent_image_build_carries_claude_pins_in_both_profiles(self) -> None:
        captured: list[list[str]] = []

        def fake_run(argv: list[str], **_kwargs: object) -> local_dev.subprocess.CompletedProcess[str]:
            captured.append(argv)
            if argv[:2] == ["git", "rev-parse"]:
                return local_dev.subprocess.CompletedProcess(argv, 0, stdout="fixture-commit\n")
            if argv[:3] == ["git", "show", "-s"]:
                return local_dev.subprocess.CompletedProcess(argv, 0, stdout="1700000000\n")
            if argv[:3] == ["docker", "inspect", "--format"]:
                return local_dev.subprocess.CompletedProcess(
                    argv,
                    0,
                    stdout="localhost:5001/labweaver/local/image@sha256:" + "a" * 64 + "\n",
                )
            return local_dev.subprocess.CompletedProcess(argv, 0, stdout="")

        for fixture, target in ((False, "agent-runtime"), (True, "agent-runtime-fixture")):
            captured.clear()
            with patch.object(local_dev, "run", side_effect=fake_run):
                local_dev.build_images(external_fixtures=fixture)
            agent_build = next(
                command
                for command in captured
                if "--build-arg" in command
                and "SERVICE=agent-service" in command
            )
            self.assertIn("--target", agent_build)
            self.assertEqual(agent_build[agent_build.index("--target") + 1], target)
            self.assertIn(f"CLAUDE_CODE_VERSION={local_dev.CLAUDE_CODE_VERSION}", agent_build)
            self.assertIn(
                f"CLAUDE_CODE_LINUX_X64_SHA512={local_dev.CLAUDE_CODE_LINUX_X64_SHA512}",
                agent_build,
            )

    def test_evaluation_runner_image_build_uses_combined_runtime_target(self) -> None:
        captured: list[list[str]] = []

        def fake_run(argv: list[str], **_kwargs: object) -> local_dev.subprocess.CompletedProcess[str]:
            captured.append(argv)
            if argv[:2] == ["git", "rev-parse"]:
                return local_dev.subprocess.CompletedProcess(argv, 0, stdout="fixture-commit\n")
            if argv[:3] == ["git", "show", "-s"]:
                return local_dev.subprocess.CompletedProcess(argv, 0, stdout="1700000000\n")
            if argv[:3] == ["docker", "inspect", "--format"]:
                return local_dev.subprocess.CompletedProcess(
                    argv,
                    0,
                    stdout="localhost:5001/labweaver/local/image@sha256:" + "a" * 64 + "\n",
                )
            return local_dev.subprocess.CompletedProcess(argv, 0, stdout="")

        with patch.object(local_dev, "run", side_effect=fake_run):
            images = local_dev.build_images(external_fixtures=False)

        self.assertRegex(images["evaluation_runner"], r"evaluation-runner@sha256:[0-9a-f]{64}$")
        build = next(
            command
            for command in captured
            if command[:3] == ["docker", "buildx", "build"]
            and "containers/Containerfile.ansible-probe" in command
        )
        self.assertEqual(build[build.index("--target") + 1], "evaluation-runtime")

    def test_work_runtime_fixture_build_uses_seed_runtime_target(self) -> None:
        captured: list[list[str]] = []

        def fake_run(argv: list[str], **_kwargs: object) -> local_dev.subprocess.CompletedProcess[str]:
            captured.append(argv)
            if argv[:2] == ["git", "rev-parse"]:
                return local_dev.subprocess.CompletedProcess(argv, 0, stdout="fixture-commit\n")
            if argv[:3] == ["git", "show", "-s"]:
                return local_dev.subprocess.CompletedProcess(argv, 0, stdout="1700000000\n")
            if argv[:3] == ["docker", "inspect", "--format"]:
                return local_dev.subprocess.CompletedProcess(
                    argv,
                    0,
                    stdout="localhost:5001/labweaver/local/image@sha256:" + "a" * 64 + "\n",
                )
            return local_dev.subprocess.CompletedProcess(argv, 0, stdout="")

        with patch.object(local_dev, "run", side_effect=fake_run):
            image = local_dev.build_work_runtime_fixture()

        expected_tag = "localhost:5001/labweaver/local/work-runtime-fixture:fixture-commit"
        self.assertEqual(
            image,
            "localhost:5001/labweaver/local/work-runtime-fixture@sha256:" + "a" * 64,
        )
        build = next(
            command
            for command in captured
            if command[:3] == ["docker", "buildx", "build"]
        )
        self.assertEqual(build[build.index("--target") + 1], "work-runtime-fixture")
        self.assertEqual(build[build.index("--tag") + 1], expected_tag)
        self.assertEqual(build[build.index("--file") + 1], "containers/Containerfile.web")
        self.assertIn(["docker", "push", expected_tag], captured)

    def test_build_executor_fixture_uses_explicit_source_image(self) -> None:
        source_image = "localhost:5001/labweaver/local/work-runtime-fixture@sha256:" + "a" * 64
        with tempfile.TemporaryDirectory() as directory:
            foundation = Path(directory) / "foundation"
            nats_client = foundation / "nats-clients" / "build-executor"
            nats_client.mkdir(parents=True)
            for name in ("nats.creds", "nats-ca.pem", "nats-client.crt", "nats-client.key"):
                (nats_client / name).write_bytes(name.encode())
            applied: list[list[dict[str, object]]] = []
            def capture_apply(_kubeconfig: Path, objects: list[dict[str, object]]) -> None:
                applied.append(objects)

            with patch.object(local_dev, "apply", side_effect=capture_apply), patch.object(
                local_dev, "wait_rollout"
            ):
                local_dev.start_build_executor_fixture(Path("kubeconfig"), foundation, source_image)

        deployment = next(document for document in applied[0] if document["kind"] == "Deployment")
        env = deployment["spec"]["template"]["spec"]["containers"][0]["env"]
        self.assertIn({"name": "FIXTURE_SOURCE_IMAGE", "value": source_image}, env)

    def test_web_containerfile_keeps_seed_fixture_separate_from_production(self) -> None:
        containerfile = (ROOT / "containers" / "Containerfile.web").read_text(encoding="utf-8")
        self.assertIn("FROM ${WEB_RUNTIME} AS runtime-base", containerfile)
        self.assertIn("FROM runtime-base AS runtime", containerfile)
        fixture = containerfile.split("FROM runtime-base AS work-runtime-fixture", maxsplit=1)[1]
        for required in (
            "USER root",
            "/opt/labweaver/workspace-seed",
            "chmod 0777 /workspace",
            "chmod 1777 /tmp",
            "USER 65534:65534",
            "WORKDIR /workspace",
        ):
            self.assertIn(required, fixture)

    def test_local_kind_bundle_covers_enabled_workloads_and_identity_bindings(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            work = root / "work"
            foundation = root / "foundation"
            work.mkdir()

            client_names = ("web", "access", "agent", "control", "environment", "evaluation", "resource")
            (work / "client-secrets.json").write_text(
                json.dumps({name: f"{name}-secret" for name in client_names}),
                encoding="utf-8",
            )
            database_names = (
                "control-service",
                "access-service",
                "agent-service",
                "environment-service",
                "evaluation-service",
                "resource-service",
            )
            (work / "database-passwords.json").write_text(
                json.dumps({name: f"{name}-password" for name in database_names}),
                encoding="utf-8",
            )
            self._write_foundation_fixture(foundation)
            for name in ("ssh_host_ed25519_key", "target_key", "target_key.pub", "target_key-cert.pub"):
                (work / name).write_bytes(name.encode())

            images = {
                "evaluation_service": (
                    f"localhost:{local_dev.REGISTRY_PORT}/labweaver/local/"
                    "evaluation-service@sha256:" + "a" * 64
                ),
                "evaluation_runner": (
                    f"localhost:{local_dev.REGISTRY_PORT}/labweaver/local/"
                    "evaluation-runner@sha256:" + "b" * 64
                ),
            }
            bundle, resource_bundle, _ = local_dev.make_app_input(work, foundation, images)

            platform_documents = list(yaml.safe_load_all(bundle.read_text(encoding="utf-8")))
            platform_names = {
                (document["kind"], document["metadata"]["name"])
                for document in platform_documents
            }
            self.assertNotIn(("ConfigMap", "build-executor-config"), platform_names)
            self.assertNotIn(("Secret", "build-executor-secrets"), platform_names)
            self.assertNotIn(("ConfigMap", "kubevirt-executor-config"), platform_names)
            self.assertNotIn(("Secret", "kubevirt-executor-secrets"), platform_names)
            self.assertNotIn(("ConfigMap", "kubevirt-console-executor-config"), platform_names)
            self.assertNotIn(("Secret", "kubevirt-console-executor-secrets"), platform_names)
            self.assertIn(("Secret", "openssh-gateway-secrets"), platform_names)

            gateway = next(
                document
                for document in platform_documents
                if document["kind"] == "Secret" and document["metadata"]["name"] == "openssh-gateway-secrets"
            )
            gateway_client_secret = base64.b64decode(gateway["data"]["service-client-secret"])
            self.assertEqual(gateway_client_secret, b"access-secret")

            control_config = next(
                document
                for document in platform_documents
                if document["kind"] == "ConfigMap"
                and document["metadata"]["name"] == "control-service-config"
            )
            environment_config = next(
                document
                for document in platform_documents
                if document["kind"] == "ConfigMap"
                and document["metadata"]["name"] == "environment-service-config"
            )
            control_values = yaml.safe_load(control_config["data"]["config.yaml"])
            self.assertEqual(
                control_values["control"]["evaluationRuntime"]["runnerImage"],
                images["evaluation_runner"],
            )
            evaluation_config = next(
                document
                for document in platform_documents
                if document["kind"] == "ConfigMap"
                and document["metadata"]["name"] == "evaluation-service-config"
            )
            evaluation_values = yaml.safe_load(evaluation_config["data"]["config.yaml"])
            self.assertEqual(
                evaluation_values["coordinator"]["workerImage"],
                images["evaluation_service"],
            )
            providers = json.loads(environment_config["data"]["providers.json"])
            container_provider = next(
                provider for provider in providers if provider["providerKind"] == "container"
            )
            self.assertEqual(
                container_provider["imageRepositoryPrefix"],
                control_values["control"]["containerBuild"]["outputRepositoryPrefix"],
            )
            self.assertEqual(
                container_provider["imageRepositoryPrefix"],
                f"localhost:{local_dev.REGISTRY_PORT}/labweaver-system",
            )
            self.assertEqual(container_provider["imagePullSecretName"], "harbor-course-pull")
            self.assertEqual(container_provider["workspaceStorageClassName"], "standard")
            self.assertEqual(container_provider["workspaceAccessMode"], "ReadWriteOnce")
            pull_secret = next(
                document
                for document in platform_documents
                if document["kind"] == "Secret"
                and document["metadata"]["name"] == "container-executor-secrets"
            )
            pull_config = json.loads(
                base64.b64decode(pull_secret["data"]["registry-pull-config.json"])
            )
            self.assertIn(f"localhost:{local_dev.REGISTRY_PORT}", pull_config["auths"])

            resource_documents = list(yaml.safe_load_all(resource_bundle.read_text(encoding="utf-8")))
            resource_names = {
                (document["kind"], document["metadata"]["name"])
                for document in resource_documents
            }
            self.assertIn(("ConfigMap", "resource-service-config"), resource_names)
            self.assertIn(("Secret", "resource-service-secrets"), resource_names)

            app_input = work / "app-input"
            self.assertFalse((app_input / "configmaps" / "kubevirt-executor-config").exists())
            self.assertFalse((app_input / "configmaps" / "kubevirt-console-executor-config").exists())
            self.assertFalse((app_input / "secrets" / "kubevirt-executor-secrets").exists())
            self.assertFalse((app_input / "secrets" / "kubevirt-console-executor-secrets").exists())

    @staticmethod
    def _write_foundation_fixture(foundation: Path) -> None:
        (foundation / "authority").mkdir(parents=True)
        (foundation / "platform-authority").mkdir(parents=True)
        (foundation / "render-input" / "secrets" / "minio-secrets").mkdir(parents=True)
        (foundation / "ssh-authority").mkdir(parents=True)
        (foundation / "authority" / "ca.crt").write_bytes(b"ca")
        (foundation / "platform-authority" / "ca.crt").write_bytes(b"platform-ca")
        (foundation / "render-input" / "secrets" / "minio-secrets" / "root-password").write_bytes(
            b"minio-password"
        )
        (foundation / "ssh-authority" / "collector-ca").write_bytes(b"collector-ca")

        identity_names = (
            "control-service",
            "access-service",
            "agent-service",
            "environment-service",
            "evaluation-service",
            "container-executor",
            "resource-service",
            "openssh-gateway",
        )
        for name in identity_names:
            identity = foundation / "platform-identities" / name
            identity.mkdir(parents=True)
            (identity / "certificate.pem").write_bytes(f"{name}-certificate".encode())
            (identity / "key.pem").write_bytes(f"{name}-key".encode())

        payload = {
            "iss": "fixture-issuer",
            "nats": {
                "pub": {
                    "allow": [
                        "$JS.ACK.>",
                        "$JS.API.>",
                        "labweaver.resource.request.approved.v1",
                        "labweaver.resource.request.submitted.v1",
                        "labweaver.resource.request.rejected.v1",
                        "labweaver.resource.request.cancelled.v1",
                        "labweaver.resource.request.state_changed.v1",
                        "labweaver.resource.lease.activated.v1",
                        "labweaver.resource.lease.renewed.v1",
                        "labweaver.resource.lease.revoked.v1",
                        "labweaver.resource.lease.expiring.v1",
                        "labweaver.resource.lease.expired.v1",
                    ]
                },
                "sub": {"allow": ["_INBOX.>", "labweaver.resource.lease.verify.v1"]},
                "resp": {"max": 1},
            },
        }
        encoded = base64.urlsafe_b64encode(json.dumps(payload).encode()).rstrip(b"=")
        credentials = (
            b"-----BEGIN NATS USER JWT-----\n"
            + b"fixture."
            + encoded
            + b".signature\n------END NATS USER JWT------\n"
        )
        for name in (
            "control-service",
            "access-service",
            "agent-service",
            "environment-service",
            "evaluation-service",
            "resource-service",
        ):
            nats_client = foundation / "nats-clients" / name
            nats_client.mkdir(parents=True)
            (nats_client / "nats.creds").write_bytes(credentials)
            (nats_client / "nats-client.crt").write_bytes(b"nats-certificate")
            (nats_client / "nats-client.key").write_bytes(b"nats-key")


if __name__ == "__main__":
    unittest.main()
