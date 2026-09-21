"""Local Kind bundle generation tests."""

from __future__ import annotations

import base64
import hashlib
import io
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import yaml


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))
import local_dev  # noqa: E402


def make_app_input_with_local_cidrs(*args: object, **kwargs: object) -> tuple[Path, Path, str]:
    """Render the local app input with the CIDRs the owned cluster reports.

    The renderer resolves the Service CIDR, the pod CIDR and the Kind network from the owned
    cluster; these tests exercise the rendering, not the cluster readback.
    """

    with (
        patch.object(local_dev, "local_service_cidr", return_value="10.201.0.0/16"),
        patch.object(local_dev, "local_kind_pod_cidr", return_value="10.202.0.0/16"),
        patch.object(local_dev, "local_kind_network_cidr", return_value="172.18.0.0/16"),
    ):
        return local_dev.make_app_input(*args, **kwargs)
import local_dev_e2e  # noqa: E402


class _FakeDownloadResponse:
    """Context manager standing in for a urlopen response body."""

    def __init__(self, payload: bytes) -> None:
        self._payload = payload

    def __enter__(self) -> "_FakeDownloadResponse":
        return self

    def __exit__(self, *_exc: object) -> bool:
        return False

    def read(self, size: int = -1) -> bytes:
        if size < 0:
            block, self._payload = self._payload, b""
            return block
        block, self._payload = self._payload[:size], self._payload[size:]
        return block


class LocalDevBundleTests(unittest.TestCase):
    def test_run_passes_raw_bytes_without_text_transcoding(self) -> None:
        captured: dict[str, object] = {}

        def capture_subprocess_run(argv: list[str], **kwargs: object) -> subprocess.CompletedProcess[bytes]:
            captured["argv"] = argv
            captured.update(kwargs)
            return subprocess.CompletedProcess(argv, 0)

        payload = b"#!/bin/sh\nexit 0\n"
        with patch.object(local_dev.subprocess, "run", side_effect=capture_subprocess_run):
            local_dev.run(["docker", "exec", "-i", "node", "sh", "-c", "cat > /tmp/script"], input_bytes=payload)

        self.assertEqual(captured["input"], payload)
        self.assertFalse(captured["text"])

    def test_provider_environment_loads_only_the_three_standard_fields(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "provider.env"
            path.write_text(
                "ANTHROPIC_BASE_URL=https://provider.example.test/anthropic\n"
                "ANTHROPIC_AUTH_TOKEN=file-token\n"
                "ANTHROPIC_MODEL=file-model\n",
                encoding="utf-8",
            )
            with patch.dict(os.environ, {"ANTHROPIC_AUTH_TOKEN": "ambient-token"}):
                self.assertEqual(
                    local_dev.load_provider_environment(path),
                    {
                        "ANTHROPIC_BASE_URL": "https://provider.example.test/anthropic",
                        "ANTHROPIC_AUTH_TOKEN": "file-token",
                        "ANTHROPIC_MODEL": "file-model",
                    },
                )

    def test_provider_environment_rejects_missing_or_invalid_values_without_echoing_secret(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            missing_token = root / "missing-token.env"
            missing_token.write_text(
                "ANTHROPIC_BASE_URL=https://provider.example.test/anthropic\n"
                "ANTHROPIC_MODEL=file-model\n",
                encoding="utf-8",
            )
            with self.assertRaisesRegex(local_dev.LocalDevError, "ANTHROPIC_AUTH_TOKEN") as missing:
                local_dev.load_provider_environment(missing_token)
            self.assertNotIn("ambient-token", str(missing.exception))

            invalid_port = root / "invalid-port.env"
            invalid_port.write_text(
                "ANTHROPIC_BASE_URL=https://provider.example.test:not-a-port/anthropic\n"
                "ANTHROPIC_AUTH_TOKEN=private-token\n"
                "ANTHROPIC_MODEL=file-model\n",
                encoding="utf-8",
            )
            with self.assertRaisesRegex(local_dev.LocalDevError, "absolute HTTPS URL") as invalid:
                local_dev.load_provider_environment(invalid_port)
            self.assertNotIn("private-token", str(invalid.exception))

    def test_provider_environment_accepts_plain_http_only_for_local_endpoints(self) -> None:
        def provider(base_url: str) -> dict[str, str]:
            return local_dev.validate_provider_environment(
                {
                    "ANTHROPIC_BASE_URL": base_url,
                    "ANTHROPIC_AUTH_TOKEN": "local-token",
                    "ANTHROPIC_MODEL": "local-model",
                }
            )

        for base_url in (
            "http://127.0.0.1:11434",
            "http://localhost:11434/v1",
            "http://10.201.0.1:11434",
            "http://172.18.0.1:11434",
            "http://192.168.56.10:11434",
            "http://[::1]:11434",
            "http://169.254.10.10:11434",
            "https://provider.example.test/anthropic",
        ):
            with self.subTest(base_url=base_url):
                self.assertEqual(provider(base_url)["ANTHROPIC_BASE_URL"], base_url)

        for base_url in (
            "http://provider.example.test/anthropic",
            "http://8.8.8.8:11434",
            "ftp://127.0.0.1:11434",
        ):
            with self.subTest(base_url=base_url):
                with self.assertRaises(local_dev.LocalDevError) as context:
                    provider(base_url)
                self.assertIn("absolute HTTPS URL", str(context.exception))

    def test_local_kind_network_cidr_selects_the_ipv4_subnet(self) -> None:
        configurations = json.dumps(
            [
                {"Subnet": "fc00:f853:ccd:e793::/64", "Gateway": "fc00:f853:ccd:e793::1"},
                {"Subnet": "172.18.0.0/16", "Gateway": "172.18.0.1"},
            ]
        )

        def capture_run(argv: list[str], **_kwargs: object) -> subprocess.CompletedProcess[str]:
            return subprocess.CompletedProcess(argv, 0, stdout=configurations)

        with patch.object(local_dev, "run", side_effect=capture_run):
            self.assertEqual(local_dev.local_kind_network_cidr(), "172.18.0.0/16")

        with patch.object(
            local_dev,
            "run",
            side_effect=lambda argv, **kwargs: subprocess.CompletedProcess(
                argv, 0, stdout=json.dumps([{"Subnet": "fc00::/64"}])
            ),
        ):
            with self.assertRaises(local_dev.LocalDevError) as context:
                local_dev.local_kind_network_cidr()
            self.assertIn("no IPv4 subnet", str(context.exception))

    def test_local_service_cidr_reads_the_apiserver_range(self) -> None:
        manifest = (
            "apiVersion: v1\n"
            "kind: Pod\n"
            "spec:\n"
            "  containers:\n"
            "  - command:\n"
            "    - kube-apiserver\n"
            "    - --service-cluster-ip-range=10.201.0.0/16\n"
        )

        def capture_run(argv: list[str], **_kwargs: object) -> subprocess.CompletedProcess[str]:
            return subprocess.CompletedProcess(argv, 0, stdout=manifest)

        with (
            patch.object(local_dev, "kind_nodes", return_value=["kind-control-plane"]),
            patch.object(local_dev, "run", side_effect=capture_run),
        ):
            self.assertEqual(local_dev.local_service_cidr(), "10.201.0.0/16")

        with (
            patch.object(local_dev, "kind_nodes", return_value=["kind-control-plane"]),
            patch.object(
                local_dev,
                "run",
                side_effect=lambda argv, **kwargs: subprocess.CompletedProcess(argv, 0, stdout=""),
            ),
        ):
            with self.assertRaises(local_dev.LocalDevError) as context:
                local_dev.local_service_cidr()
            self.assertIn("service-cluster-ip-range", str(context.exception))

    def test_provider_environment_rejects_extra_fields_and_path_is_not_exposed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = Path(directory) / "provider-with-extra.env"
            path.write_text(
                "ANTHROPIC_BASE_URL=https://provider.example.test/anthropic\n"
                "ANTHROPIC_AUTH_TOKEN=private-token\n"
                "ANTHROPIC_MODEL=file-model\n"
                "ANTHROPIC_LEGACY_KEY=must-reject\n",
                encoding="utf-8",
            )
            with self.assertRaisesRegex(local_dev.LocalDevError, "unsupported fields") as error:
                local_dev.load_provider_environment(path)
            self.assertNotIn("private-token", str(error.exception))
            self.assertNotIn(str(path), str(error.exception))

            malformed = root / "provider-malformed.env"
            malformed.write_text(
                "ANTHROPIC_BASE_URL=https://provider.example.test/anthropic\n"
                "ANTHROPIC_AUTH_TOKEN=private-token\n"
                "ANTHROPIC_MODEL=file-model\n"
                "this is not a dotenv assignment\n",
                encoding="utf-8",
            )
            with self.assertRaisesRegex(local_dev.LocalDevError, "invalid dotenv entry"):
                local_dev.load_provider_environment(malformed)

    def test_external_fixture_provider_does_not_load_dotenv(self) -> None:
        with patch.object(
            local_dev,
            "load_provider_environment",
            side_effect=AssertionError("fixture must not load a provider file"),
        ):
            self.assertEqual(
                local_dev.resolve_provider_environment(external_fixtures=True, provider_env=None),
                {
                    "ANTHROPIC_BASE_URL": "https://local-claude-fixture.invalid/anthropic",
                    "ANTHROPIC_AUTH_TOKEN": "local-claude-fixture-token",
                    "ANTHROPIC_MODEL": "local-claude-fixture",
                },
            )
        with self.assertRaisesRegex(local_dev.LocalDevError, "cannot be combined"):
            local_dev.resolve_provider_environment(
                external_fixtures=True,
                provider_env=Path("provider.env"),
            )

    def test_playwright_job_reads_provider_model_from_agent_configmap(self) -> None:
        stack = local_dev_e2e.Stack(
            state_file=Path(".tmp/local-dev/state.json"),
            state={},
            kubeconfig=Path(".private/local-dev/kubeconfig"),
            run_root=Path(".private/local-dev"),
            run_id="a" * 12,
            namespace="labweaver-system",
            identity_namespace="keycloak-system",
            registry="labweaver-local-registry-" + "a" * 12,
            registry_port=5001,
            portal_port=38080,
        )
        job = local_dev_e2e.make_job(
            stack,
            job_name="local-dev-e2e-test",
            image="localhost:5001/labweaver/local/playwright-e2e:test",
            credentials_secret="credentials",
            ca_secret="ca",
            projects=["teacher"],
            grep=None,
            grep_invert=None,
            extra_environment={},
            skip_vm=False,
            timeout_seconds=120,
        )
        main_env = job["spec"]["template"]["spec"]["containers"][0]["env"]
        provider_model = next(item for item in main_env if item["name"] == "LABWEAVER_E2E_PROVIDER_MODEL")
        self.assertEqual(
            provider_model["valueFrom"]["configMapKeyRef"],
            {"name": "agent-service-config", "key": "anthropic-model", "optional": False},
        )
        command = job["spec"]["template"]["spec"]["containers"][0]["args"]
        self.assertEqual(command.count("--retries=0"), 1)

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

    def test_kindnet_resources_use_owned_kubeconfig_and_preserve_daemonset_fields(self) -> None:
        calls: list[tuple[Path, list[str]]] = []

        def capture_kubectl(
            kubeconfig: Path,
            args: list[str],
            **_kwargs: object,
        ) -> subprocess.CompletedProcess[str]:
            calls.append((kubeconfig, args))
            return subprocess.CompletedProcess(args, 0, "", "")

        with (
            patch.object(local_dev, "kubectl", side_effect=capture_kubectl),
            patch.object(local_dev, "wait_rollout") as wait,
        ):
            kubeconfig = Path(".tmp/local-dev/owned-kubeconfig")
            local_dev.configure_kindnet_resources(kubeconfig)

        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0][0], kubeconfig)
        args = calls[0][1]
        self.assertEqual(
            args[:6],
            [
                "-n",
                "kube-system",
                "patch",
                "daemonset/kindnet",
                "--type=strategic",
                "--patch",
            ],
        )
        patch_document = json.loads(args[6])
        self.assertEqual(
            patch_document,
            {
                "spec": {
                    "template": {
                        "spec": {
                            "containers": [
                                {
                                    "name": "kindnet-cni",
                                    "resources": {
                                        "limits": {"cpu": "500m", "memory": "128Mi"},
                                        "requests": {"cpu": "500m", "memory": "128Mi"},
                                    },
                                }
                            ]
                        }
                    }
                }
            },
        )
        wait.assert_called_once_with(kubeconfig, "daemonset", "kindnet", "kube-system")

    def test_kind_cluster_config_applies_the_reviewed_pod_process_limit(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            state = Path(directory)
            calls: list[list[str]] = []

            def capture_run(argv: list[str], **_kwargs: object) -> subprocess.CompletedProcess[str]:
                calls.append(argv)
                return subprocess.CompletedProcess(argv, 0, stdout="")

            with (
                patch.object(local_dev, "STATE_DIR", state),
                patch.object(local_dev, "CLUSTER", "kind-config-cluster"),
                patch.object(local_dev, "REGISTRY", "kind-config-registry"),
                patch.object(local_dev, "REGISTRY_PORT", 5001),
                patch.object(local_dev, "RUN_ID", "kindconfig"),
                patch.object(local_dev, "run", side_effect=capture_run),
                patch.object(local_dev, "configure_kind_sandbox_runtime"),
                patch.object(local_dev, "configure_kindnet_resources"),
                patch.object(local_dev, "kind_nodes", return_value=[]),
            ):
                local_dev.create_cluster(state / "kubeconfig", expose_registry=False)

            kind_config = yaml.safe_load((state / "kind-config.yaml").read_text(encoding="utf-8"))
        self.assertEqual(kind_config["name"], "kind-config-cluster")
        self.assertEqual(
            kind_config["kubeadmConfigPatches"],
            [
                "kind: KubeletConfiguration\n"
                f"podPidsLimit: {local_dev.SANDBOX_PIDS_LIMIT}\n"
            ],
        )
        self.assertEqual(
            kind_config["nodes"],
            [{"role": "control-plane", "image": local_dev.KIND_IMAGE}],
        )
        self.assertEqual(
            calls[0][:3], ["kind", "create", "cluster"]
        )

    def test_kind_sandbox_runtime_installs_gvisor_handler_without_a_base_spec(self) -> None:
        containerd_config = (
            '[plugins."io.containerd.grpc.v1.cri".containerd.runtimes.runc]\n'
            '  runtime_type = "io.containerd.runc.v2"\n'
        )
        calls: list[tuple[list[str], bytes | None]] = []
        applied: list[tuple[Path, list[dict[str, object]]]] = []

        def capture_run(
            argv: list[str],
            *,
            input_bytes: bytes | None = None,
            capture: bool = False,
            check: bool = True,
        ) -> subprocess.CompletedProcess[str]:
            del capture, check
            calls.append((argv, input_bytes))
            if argv == [
                "docker",
                "exec",
                "kind-control-plane",
                "cat",
                local_dev.KIND_CONTAINERD_CONFIG,
            ]:
                return subprocess.CompletedProcess(argv, 0, stdout=containerd_config)
            return subprocess.CompletedProcess(argv, 0, stdout="")

        def capture_apply(kubeconfig: Path, objects: list[dict[str, object]]) -> None:
            applied.append((kubeconfig, objects))

        with tempfile.TemporaryDirectory() as directory:
            cache = Path(directory)
            binaries: dict[str, Path] = {}
            for name in (
                "runsc",
                "containerd-shim-runsc-v1",
                "gvisor-bin/gvisor_sentry",
            ):
                (cache / name).parent.mkdir(parents=True, exist_ok=True)
                (cache / name).write_bytes(f"{name} bytes".encode())
                binaries[name] = cache / name
            with (
                patch.object(local_dev, "kind_nodes", return_value=["kind-control-plane"]),
                patch.object(local_dev, "run", side_effect=capture_run),
                patch.object(local_dev, "apply", side_effect=capture_apply),
                patch.object(
                    local_dev,
                    "_sandbox_runsc_archive",
                    return_value=cache / "gvisor.tar.bz2",
                ),
                patch.object(local_dev, "_extract_sandbox_runsc", return_value=binaries),
            ):
                kubeconfig = Path(".tmp/local-dev/owned-kubeconfig")
                local_dev.configure_kind_sandbox_runtime(kubeconfig)

        config_writes = [
            payload
            for argv, payload in calls
            if payload is not None and argv[-1] == f"cat > {local_dev.KIND_CONTAINERD_CONFIG}"
        ]
        self.assertEqual(len(config_writes), 1)
        written_config = config_writes[0].decode("utf-8")
        self.assertTrue(written_config.startswith(containerd_config))
        self.assertIn(
            '[plugins."io.containerd.grpc.v1.cri".containerd.runtimes.labweaver-sandbox]\n'
            '  runtime_type = "io.containerd.runsc.v1"\n',
            written_config,
        )
        # gVisor's containerd runtime plugin rejects unknown option keys, and a
        # base OCI spec without a mounts array makes runsc refuse to start a
        # container, so the installed table carries neither.
        self.assertNotIn(".options]", written_config)
        self.assertNotIn("base_runtime_spec", written_config)
        self.assertEqual(
            [argv for argv, _payload in calls if argv[:2] == ["docker", "cp"]],
            [
                [
                    "docker",
                    "cp",
                    str(binaries[name]),
                    f"kind-control-plane:{local_dev.SANDBOX_RUNSC_INSTALL_DIR}/{name}",
                ]
                for name in sorted(binaries)
            ],
        )
        self.assertEqual(
            [
                argv
                for argv, _payload in calls
                if argv[:3] == ["docker", "exec", "kind-control-plane"] and "mkdir" in argv
            ],
            [
                [
                    "docker",
                    "exec",
                    "kind-control-plane",
                    "mkdir",
                    "-p",
                    f"{local_dev.SANDBOX_RUNSC_INSTALL_DIR}/gvisor-bin",
                ]
            ],
        )
        self.assertEqual(
            [
                argv
                for argv, _payload in calls
                if argv[:3] == ["docker", "exec", "kind-control-plane"] and "chmod" in argv
            ],
            [
                [
                    "docker",
                    "exec",
                    "kind-control-plane",
                    "chmod",
                    "0755",
                    f"{local_dev.SANDBOX_RUNSC_INSTALL_DIR}/{name}",
                ]
                for name in local_dev.SANDBOX_RUNSC_REQUIRED_MEMBERS
            ],
        )
        self.assertEqual(
            [argv for argv, _payload in calls if argv[-3:] == ["systemctl", "restart", "containerd"]],
            [["docker", "exec", "kind-control-plane", "systemctl", "restart", "containerd"]],
        )
        self.assertEqual(
            applied,
            [
                (
                    Path(".tmp/local-dev/owned-kubeconfig"),
                    [
                        {
                            "apiVersion": "node.k8s.io/v1",
                            "kind": "RuntimeClass",
                            "metadata": {
                                "name": "labweaver-sandbox",
                                "labels": {
                                    "labweaver.local-dev.owner": (
                                        local_dev.KUBERNETES_OWNER_LABEL_VALUE
                                    )
                                },
                            },
                            "handler": "labweaver-sandbox",
                        }
                    ],
                )
            ],
        )

    def test_kind_sandbox_runtime_accepts_existing_handler_without_duplicate_config(self) -> None:
        containerd_config = (
            '[plugins."io.containerd.grpc.v1.cri".containerd.runtimes.runc]\n'
            '  runtime_type = "io.containerd.runc.v2"\n'
            '[plugins."io.containerd.grpc.v1.cri".containerd.runtimes.labweaver-sandbox]\n'
            '  runtime_type = "io.containerd.runsc.v1"\n'
        )
        calls: list[tuple[list[str], bytes | None]] = []

        def capture_run(
            argv: list[str],
            *,
            input_bytes: bytes | None = None,
            capture: bool = False,
            check: bool = True,
        ) -> subprocess.CompletedProcess[str]:
            del capture, check
            calls.append((argv, input_bytes))
            if argv[-2:] == ["cat", local_dev.KIND_CONTAINERD_CONFIG]:
                return subprocess.CompletedProcess(argv, 0, stdout=containerd_config)
            return subprocess.CompletedProcess(argv, 0, stdout="")

        with (
            patch.object(local_dev, "kind_nodes", return_value=["kind-control-plane"]),
            patch.object(local_dev, "run", side_effect=capture_run),
            patch.object(local_dev, "apply"),
            patch.object(local_dev, "_sandbox_runsc_archive", return_value=Path("gvisor.tar.bz2")),
            patch.object(
                local_dev,
                "_extract_sandbox_runsc",
                return_value={
                    "runsc": Path("/cache/runsc"),
                    "containerd-shim-runsc-v1": Path("/cache/containerd-shim-runsc-v1"),
                    "gvisor-bin/gvisor_sentry": Path("/cache/gvisor-bin/gvisor_sentry"),
                },
            ),
        ):
            local_dev.configure_kind_sandbox_runtime(Path(".tmp/local-dev/owned-kubeconfig"))

        self.assertFalse(
            any(
                payload is not None and argv[-1] == f"cat > {local_dev.KIND_CONTAINERD_CONFIG}"
                for argv, payload in calls
            )
        )
        self.assertEqual(
            sum(argv[-3:] == ["systemctl", "restart", "containerd"] for argv, _payload in calls),
            1,
        )

    def test_kind_sandbox_runtime_rejects_unexpected_existing_handler(self) -> None:
        containerd_config = (
            '[plugins."io.containerd.grpc.v1.cri".containerd.runtimes.labweaver-sandbox]\n'
            '  runtime_type = "io.containerd.runsc.v1"\n'
            '[plugins."io.containerd.grpc.v1.cri".containerd.runtimes.labweaver-sandbox.options]\n'
            '  SystemdCgroup = true\n'
        )

        def capture_run(
            argv: list[str],
            *,
            input_bytes: bytes | None = None,
            capture: bool = False,
            check: bool = True,
        ) -> subprocess.CompletedProcess[str]:
            del input_bytes, capture, check
            if argv[-2:] == ["cat", local_dev.KIND_CONTAINERD_CONFIG]:
                return subprocess.CompletedProcess(argv, 0, stdout=containerd_config)
            return subprocess.CompletedProcess(argv, 0, stdout="")

        with (
            patch.object(local_dev, "kind_nodes", return_value=["kind-control-plane"]),
            patch.object(local_dev, "run", side_effect=capture_run),
            patch.object(local_dev, "apply"),
            patch.object(local_dev, "_sandbox_runsc_archive", return_value=Path("gvisor.tar.bz2")),
            patch.object(
                local_dev,
                "_extract_sandbox_runsc",
                return_value={
                    "runsc": Path("/cache/runsc"),
                    "containerd-shim-runsc-v1": Path("/cache/containerd-shim-runsc-v1"),
                    "gvisor-bin/gvisor_sentry": Path("/cache/gvisor-bin/gvisor_sentry"),
                },
            ),
        ):
            with self.assertRaises(local_dev.LocalDevError) as context:
                local_dev.configure_kind_sandbox_runtime(Path(".tmp/local-dev/owned-kubeconfig"))
        self.assertIn("unexpected settings", str(context.exception))

    def test_sandbox_runtime_lock_requires_every_gvisor_key(self) -> None:
        complete = {
            "gvisor": {
                "version": "release-20260914.0",
                "linux_amd64_url": "https://example.invalid/gvisor.tar.bz2",
                "linux_amd64_sha512": "a" * 128,
            }
        }
        with patch.object(local_dev, "load_yaml", return_value=complete):
            self.assertEqual(
                local_dev._sandbox_runtime_lock()["version"], "release-20260914.0"
            )
        for key in ("version", "linux_amd64_url", "linux_amd64_sha512"):
            incomplete = {"gvisor": {k: v for k, v in complete["gvisor"].items() if k != key}}
            with patch.object(local_dev, "load_yaml", return_value=incomplete):
                with self.assertRaises(local_dev.LocalDevError) as context:
                    local_dev._sandbox_runtime_lock()
            self.assertIn(f"gvisor.{key}", str(context.exception))
        empty = {"gvisor": {**complete["gvisor"], "linux_amd64_sha512": "  "}}
        with patch.object(local_dev, "load_yaml", return_value=empty):
            with self.assertRaises(local_dev.LocalDevError):
                local_dev._sandbox_runtime_lock()
        with patch.object(local_dev, "load_yaml", return_value={"gvisor": None}):
            with self.assertRaises(local_dev.LocalDevError):
                local_dev._sandbox_runtime_lock()

    def test_sandbox_runsc_archive_verifies_the_locked_sha512(self) -> None:
        payload = b"locked gvisor release payload"
        correct = hashlib.sha512(payload).hexdigest()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / "gvisor.tar.bz2"
            archive.write_bytes(payload)
            wrong = {
                "version": "release-test",
                "linux_amd64_url": "https://example.invalid/gvisor.tar.bz2",
                "linux_amd64_sha512": "0" * 128,
            }
            with patch.object(local_dev, "SANDBOX_RUNSC_ARCHIVE", archive):
                with self.assertRaises(local_dev.LocalDevError) as context:
                    local_dev._sandbox_runsc_archive(wrong)
                self.assertIn("sha512", str(context.exception))
                cached = local_dev._sandbox_runsc_archive(
                    {**wrong, "linux_amd64_sha512": correct}
                )
            self.assertEqual(cached, archive)

            fresh = root / "fresh.tar.bz2"

            def fake_urlopen(_request: object, **_kwargs: object) -> _FakeDownloadResponse:
                return _FakeDownloadResponse(payload)

            with (
                patch.object(local_dev, "SANDBOX_RUNSC_ARCHIVE", fresh),
                patch.object(local_dev, "urlopen", side_effect=fake_urlopen),
            ):
                with self.assertRaises(local_dev.LocalDevError):
                    local_dev._sandbox_runsc_archive(wrong)
            self.assertFalse(fresh.exists())
            self.assertFalse(fresh.with_name(fresh.name + ".part").exists())

            with (
                patch.object(local_dev, "SANDBOX_RUNSC_ARCHIVE", fresh),
                patch.object(local_dev, "urlopen", side_effect=fake_urlopen),
            ):
                downloaded = local_dev._sandbox_runsc_archive(
                    {**wrong, "linux_amd64_sha512": correct}
                )
            self.assertEqual(downloaded.read_bytes(), payload)

    def test_sandbox_runsc_extraction_requires_the_reviewed_members(self) -> None:
        runsc = b"runsc binary"
        shim = b"shim binary"
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / "gvisor.tar.bz2"
            self._write_tarball(archive, {"runsc": runsc})
            cache = root / "extracted"
            with patch.object(local_dev, "SANDBOX_RUNSC_CACHE_DIR", cache):
                with self.assertRaises(local_dev.LocalDevError) as context:
                    local_dev._extract_sandbox_runsc(archive)
                self.assertIn("containerd-shim-runsc-v1", str(context.exception))

            self._write_tarball(
                archive, {"runsc": runsc, "containerd-shim-runsc-v1": shim}
            )
            with patch.object(local_dev, "SANDBOX_RUNSC_CACHE_DIR", cache):
                extracted = local_dev._extract_sandbox_runsc(archive)
            self.assertEqual(extracted["runsc"].read_bytes(), runsc)
            self.assertEqual(extracted["containerd-shim-runsc-v1"].read_bytes(), shim)
            for target in extracted.values():
                self.assertEqual(stat.S_IMODE(target.stat().st_mode), 0o755)

            sentry = b"sentry sidecar"
            self._write_tarball(
                archive,
                {
                    "runsc": runsc,
                    "containerd-shim-runsc-v1": shim,
                    "gvisor-bin/gvisor_sentry": sentry,
                },
            )
            with patch.object(local_dev, "SANDBOX_RUNSC_CACHE_DIR", cache):
                extracted = local_dev._extract_sandbox_runsc(archive)
            self.assertEqual(
                extracted["gvisor-bin/gvisor_sentry"].read_bytes(), sentry
            )
            self.assertEqual(
                stat.S_IMODE(extracted["gvisor-bin/gvisor_sentry"].stat().st_mode),
                0o755,
            )

            self._write_tarball(
                archive,
                {
                    "runsc": runsc,
                    "containerd-shim-runsc-v1": shim,
                    "../escape": b"escape",
                },
            )
            with patch.object(local_dev, "SANDBOX_RUNSC_CACHE_DIR", cache):
                with self.assertRaises(local_dev.LocalDevError) as context:
                    local_dev._extract_sandbox_runsc(archive)
            self.assertIn("plain path", str(context.exception))
            self.assertFalse((root / "escape").exists())

    def _write_tarball(self, path: Path, members: dict[str, bytes]) -> None:
        with tarfile.open(path, "w:bz2") as bundle:
            for name, payload in members.items():
                info = tarfile.TarInfo(name)
                info.size = len(payload)
                bundle.addfile(info, io.BytesIO(payload))

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

    def test_keycloak_foundation_uses_persistent_single_writer_storage(self) -> None:
        applied: list[list[dict[str, object]]] = []

        def capture_apply(_kubeconfig: Path, objects: list[dict[str, object]]) -> None:
            applied.append(objects)
            local_dev.validate_kubernetes_label_maps(objects)

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            foundation = root / "foundation"
            work = root / "work"
            foundation.mkdir()
            work.mkdir()
            key = root / "keycloak.key"
            crt = root / "keycloak.crt"
            key.write_bytes(b"key")
            crt.write_bytes(b"certificate")
            with (
                patch.object(local_dev, "apply", side_effect=capture_apply),
                patch.object(local_dev, "foundation_objects", return_value=[]),
                patch.object(local_dev, "local_kind_workspace_configuration", return_value=("standard", "ReadWriteOnce")),
                patch.object(local_dev, "cert", return_value=(key, crt)),
                patch.object(local_dev, "wait_rollout"),
                patch.object(local_dev, "wait_postgres_ready"),
                patch.object(local_dev, "bootstrap_nats_and_minio"),
                patch.object(
                    local_dev,
                    "kubectl",
                    return_value=local_dev.subprocess.CompletedProcess([], 0),
                ),
            ):
                local_dev.start_foundation(Path("kubeconfig"), foundation, work)

        keycloak_objects = next(
            objects
            for objects in applied
            if any(document.get("metadata", {}).get("name") == "keycloak" for document in objects)
        )
        self.assertEqual(
            [document["kind"] for document in keycloak_objects],
            ["PersistentVolumeClaim", "Deployment", "Service"],
        )
        pvc = keycloak_objects[0]
        self.assertEqual(pvc["metadata"], {"name": "keycloak-data", "namespace": "keycloak-system"})
        self.assertEqual(
            pvc["spec"],
            {
                "accessModes": ["ReadWriteOnce"],
                "storageClassName": "standard",
                "resources": {"requests": {"storage": "1Gi"}},
            },
        )

        deployment = keycloak_objects[1]
        self.assertEqual(deployment["spec"]["strategy"], {"type": "Recreate"})
        pod_spec = deployment["spec"]["template"]["spec"]
        self.assertEqual(
            pod_spec["securityContext"],
            {
                "runAsNonRoot": True,
                "fsGroup": 1000,
                "fsGroupChangePolicy": "OnRootMismatch",
                "seccompProfile": {"type": "RuntimeDefault"},
            },
        )
        container = pod_spec["containers"][0]
        self.assertEqual(
            container["securityContext"],
            {
                "runAsUser": 1000,
                "allowPrivilegeEscalation": False,
                "capabilities": {"drop": ["ALL"]},
            },
        )
        self.assertNotIn("runAsGroup", container["securityContext"])
        self.assertIn("--import-realm", container["args"])
        data_mount = next(mount for mount in container["volumeMounts"] if mount["name"] == "data")
        self.assertEqual(data_mount["mountPath"], "/opt/keycloak/data")
        self.assertFalse(data_mount.get("readOnly", False))
        realm_mount = next(mount for mount in container["volumeMounts"] if mount["name"] == "realm")
        self.assertEqual(realm_mount["mountPath"], "/opt/keycloak/data/import/workloads-realm.json")
        self.assertTrue(realm_mount["readOnly"])
        data_volume = next(volume for volume in pod_spec["volumes"] if volume["name"] == "data")
        self.assertEqual(data_volume["persistentVolumeClaim"], {"claimName": "keycloak-data"})

    def test_kind_harbor_trust_installs_idempotent_restart_restore_unit(self) -> None:
        calls: list[tuple[list[str], bytes | None]] = []

        def capture_run(
            argv: list[str],
            *,
            input_bytes: bytes | None = None,
            capture: bool = False,
            check: bool = True,
        ) -> subprocess.CompletedProcess[str]:
            del capture, check
            calls.append((argv, input_bytes))
            return subprocess.CompletedProcess(argv, 0, "", "")

        with tempfile.TemporaryDirectory() as directory:
            certificate = Path(directory) / "harbor-ca.crt"
            certificate.write_bytes(b"harbor-ca")
            provider = SimpleNamespace(
                registry_host="harbor.lab.lan",
                registry_service_ip="10.96.0.42",
                harbor_ca_file=certificate,
            )
            with (
                patch.object(local_dev, "kind_nodes", return_value=["kind-control-plane", "kind-worker"]),
                patch.object(local_dev, "run", side_effect=capture_run),
            ):
                local_dev.configure_kind_harbor_trust(provider)
                local_dev.configure_kind_harbor_trust(provider)

        script_payloads = [
            payload
            for _argv, payload in calls
            if payload is not None and payload.startswith(b"#!/bin/sh")
        ]
        unit_payloads = [
            payload
            for _argv, payload in calls
            if payload is not None and payload.startswith(b"[Unit]")
        ]
        self.assertEqual(len(script_payloads), 4)
        self.assertEqual(len(unit_payloads), 4)
        self.assertEqual(len(set(script_payloads)), 1)
        self.assertEqual(len(set(unit_payloads)), 1)

        self.assertTrue(all(b"\r\n" not in payload for _argv, payload in calls if payload is not None))
        script = script_payloads[0].decode("utf-8")
        self.assertIn("registry_ip='10.96.0.42'", script)
        self.assertIn("registry_host='harbor.lab.lan'", script)
        self.assertIn('substr($i, 1, 1) == "#"', script)
        self.assertIn("kept_aliases++", script)
        self.assertIn('cat "$temporary_file" > /etc/hosts', script)
        self.assertNotIn(">> /etc/hosts", script)
        self.assertNotIn("mv ", script)

        unit = unit_payloads[0].decode("utf-8")
        self.assertIn("Before=containerd.service kubelet.service", unit)
        self.assertIn("ExecStart=/usr/local/sbin/labweaver-restore-harbor-host", unit)
        self.assertIn("WantedBy=multi-user.target", unit)
        self.assertNotIn("RemainAfterExit", unit)

        enable_now = [
            argv
            for argv, _payload in calls
            if argv[-4:] == ["systemctl", "enable", "--now", "labweaver-harbor-hosts.service"]
        ]
        self.assertEqual(len(enable_now), 4)

    def test_kind_harbor_trust_surfaces_restore_unit_start_failure(self) -> None:
        def fail_unit_start(
            argv: list[str],
            *,
            input_bytes: bytes | None = None,
            capture: bool = False,
            check: bool = True,
        ) -> subprocess.CompletedProcess[str]:
            del input_bytes, capture, check
            if argv[-4:] == ["systemctl", "enable", "--now", "labweaver-harbor-hosts.service"]:
                raise local_dev.LocalDevError("command failed (1): systemctl enable --now")
            return subprocess.CompletedProcess(argv, 0, "", "")

        with tempfile.TemporaryDirectory() as directory:
            certificate = Path(directory) / "harbor-ca.crt"
            certificate.write_bytes(b"harbor-ca")
            provider = SimpleNamespace(
                registry_host="harbor.lab.lan",
                registry_service_ip="10.96.0.42",
                harbor_ca_file=certificate,
            )
            with (
                patch.object(local_dev, "kind_nodes", return_value=["kind-control-plane"]),
                patch.object(local_dev, "run", side_effect=fail_unit_start),
            ):
                with self.assertRaisesRegex(local_dev.LocalDevError, "systemctl enable --now"):
                    local_dev.configure_kind_harbor_trust(provider)

    def test_evaluation_runner_bootstrap_uses_final_execution_and_private_pull_config(self) -> None:
        applied: list[list[dict[str, object]]] = []

        def capture_apply(_kubeconfig: Path, objects: list[dict[str, object]]) -> None:
            applied.append(objects)
            local_dev.validate_kubernetes_label_maps(objects)

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            app_input = root / "app-input"
            config_path = app_input / "configmaps/evaluation-service-config/config.yaml"
            config_path.parent.mkdir(parents=True)
            config_path.write_text(
                yaml.safe_dump(
                    {
                        "execution": {
                            "runnerNamespace": "evaluation-runners",
                            "ojServiceAccountName": "oj-runner-v2",
                            "ansibleProbeServiceAccountName": "ansible-probe-v2",
                            "imagePullSecretName": "harbor-course-pull-v2",
                        }
                    },
                    sort_keys=False,
                ),
                encoding="utf-8",
            )
            private_pull_config = root / "registry-pull-config.json"
            private_payload = b'{"auths":{"harbor.lab.lan":{"auth":"runtime"}}}\n'
            private_pull_config.write_bytes(private_payload)

            with patch.object(local_dev, "apply", side_effect=capture_apply):
                local_dev.bootstrap_evaluation_runner_resources(
                    Path("kubeconfig"), app_input, private_pull_config
                )

        self.assertEqual(len(applied), 1)
        objects = applied[0]
        self.assertEqual(
            [(document["kind"], document["metadata"]["name"]) for document in objects],
            [
                ("Namespace", "evaluation-runners"),
                ("NetworkPolicy", "oj-runner-default-deny"),
                ("NetworkPolicy", "ansible-probe-default-deny"),
                ("ServiceAccount", "oj-runner-v2"),
                ("ServiceAccount", "ansible-probe-v2"),
                ("Secret", "harbor-course-pull-v2"),
            ],
        )
        namespace = objects[0]
        self.assertEqual(
            namespace["metadata"]["labels"],
            {
                "app.kubernetes.io/part-of": "labweaver",
                "labweaver.io/managed": "true",
                "pod-security.kubernetes.io/enforce": "restricted",
                "pod-security.kubernetes.io/audit": "restricted",
                "pod-security.kubernetes.io/warn": "restricted",
            },
        )
        for policy in objects[1:3]:
            self.assertEqual(policy["metadata"]["namespace"], "evaluation-runners")
            self.assertEqual(
                policy["spec"],
                {
                    "podSelector": {},
                    "policyTypes": ["Ingress", "Egress"],
                    "ingress": [],
                    "egress": [],
                },
            )
        for service_account in objects[3:5]:
            self.assertEqual(service_account["metadata"]["namespace"], "evaluation-runners")
            self.assertFalse(service_account["automountServiceAccountToken"])
        pull_secret = objects[5]
        self.assertEqual(pull_secret["type"], "kubernetes.io/dockerconfigjson")
        self.assertEqual(
            base64.b64decode(pull_secret["data"][".dockerconfigjson"]),
            private_payload,
        )

    def test_evaluation_runner_bootstrap_rejects_missing_private_pull_config(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            config_path = root / "app-input/configmaps/evaluation-service-config/config.yaml"
            config_path.parent.mkdir(parents=True)
            config_path.write_text(
                "execution:\n"
                "  runnerNamespace: labweaver-evaluation\n"
                "  ojServiceAccountName: oj-runner\n"
                "  ansibleProbeServiceAccountName: ansible-probe\n"
                "  imagePullSecretName: harbor-course-pull\n",
                encoding="utf-8",
            )
            with self.assertRaisesRegex(local_dev.LocalDevError, "unavailable"):
                local_dev.bootstrap_evaluation_runner_resources(
                    Path("kubeconfig"), root / "app-input", root / "missing.json"
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

    def test_local_minio_buckets_include_all_declared_unique_buckets(self) -> None:
        self.assertEqual(
            local_dev.local_minio_buckets(),
            ("labweaver-artifacts", "labweaver-frozen-submissions"),
        )

    def test_minio_bootstrap_runs_immutable_verifier_for_each_configured_bucket(self) -> None:
        verifier = patch.object(local_dev, "bootstrap_minio_bucket")
        with patch.object(
            local_dev,
            "local_minio_buckets",
            return_value=("labweaver-artifacts", "labweaver-frozen-submissions"),
        ), verifier as bootstrap:
            minio = object()
            local_dev.bootstrap_minio_buckets(minio)

        self.assertEqual(
            [call.args[1] for call in bootstrap.call_args_list],
            ["labweaver-artifacts", "labweaver-frozen-submissions"],
        )

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
        # The Agent platform registry is part of the reviewed bundle contract, so
        # only the profile that provisions the run-owned Harbor and BuildKit
        # provider can render every declared secret; the fixture profile fails
        # closed in test_local_fixture_profile_requires_the_real_build_provider.
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
            build_nats = foundation / "nats-clients" / "build-executor"
            build_nats.mkdir(parents=True)
            for name in ("nats.creds", "nats-client.crt", "nats-client.key"):
                (build_nats / name).write_bytes(f"build-{name}".encode())
            for name in ("ssh_host_ed25519_key", "target_key", "target_key.pub", "target_key-cert.pub"):
                (work / name).write_bytes(name.encode())

            provider = self._write_real_build_provider(root)
            images = {
                "evaluation_service": (
                    f"localhost:{local_dev.REGISTRY_PORT}/labweaver/local/"
                    "evaluation-service@sha256:" + "a" * 64
                ),
                "evaluation_runner": (
                    f"localhost:{local_dev.REGISTRY_PORT}/labweaver/local/"
                    "evaluation-runner@sha256:" + "b" * 64
                ),
                "authoring_sandbox": (
                    f"localhost:{local_dev.REGISTRY_PORT}/labweaver/local/"
                    "authoring-sandbox@sha256:" + "c" * 64
                ),
            }
            bundle, resource_bundle, _ = make_app_input_with_local_cidrs(
                work,
                foundation,
                images,
                {
                    "ANTHROPIC_BASE_URL": "https://provider.example.test/anthropic",
                    "ANTHROPIC_AUTH_TOKEN": "test-provider-token",
                    "ANTHROPIC_MODEL": "test-model",
                },
                provider,
            )

            platform_documents = list(yaml.safe_load_all(bundle.read_text(encoding="utf-8")))
            platform_names = {
                (document["kind"], document["metadata"]["name"])
                for document in platform_documents
            }
            self.assertIn(("ConfigMap", "build-executor-config"), platform_names)
            self.assertIn(("Secret", "build-executor-secrets"), platform_names)
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
            agent_config = next(
                document
                for document in platform_documents
                if document["kind"] == "ConfigMap"
                and document["metadata"]["name"] == "agent-service-config"
            )
            self.assertEqual(
                agent_config["data"]["anthropic-base-url"],
                "https://provider.example.test/anthropic\n",
            )
            self.assertEqual(agent_config["data"]["anthropic-model"], "test-model\n")
            agent_secret = next(
                document
                for document in platform_documents
                if document["kind"] == "Secret"
                and document["metadata"]["name"] == "agent-service-secrets"
            )
            self.assertEqual(
                base64.b64decode(agent_secret["data"]["anthropic-auth-token"]),
                b"test-provider-token",
            )
            self.assertNotIn("test-provider-token", bundle.read_text(encoding="utf-8"))
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
            self.assertEqual(
                evaluation_values["objectStore"]["binding"],
                "minio-submissions-v1",
            )
            self.assertEqual(
                evaluation_values["packageObjectStore"]["binding"],
                "problem-package-minio-v1",
            )
            self.assertNotEqual(
                evaluation_values["objectStore"]["binding"],
                evaluation_values["packageObjectStore"]["binding"],
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
                f"{provider.registry_host}/labweaver-system",
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
            self.assertIn(provider.registry_host, pull_config["auths"])

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

    def test_local_fixture_profile_binds_fixture_platform_registry_credentials(self) -> None:
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

            bundle, _resource_bundle, _ = make_app_input_with_local_cidrs(
                work,
                foundation,
                {
                    "evaluation_service": (
                        f"localhost:{local_dev.REGISTRY_PORT}/labweaver/local/"
                        "evaluation-service@sha256:" + "a" * 64
                    ),
                    "evaluation_runner": (
                        f"localhost:{local_dev.REGISTRY_PORT}/labweaver/local/"
                        "evaluation-runner@sha256:" + "b" * 64
                    ),
                    "authoring_sandbox": (
                        f"localhost:{local_dev.REGISTRY_PORT}/labweaver/local/"
                        "authoring-sandbox@sha256:" + "c" * 64
                    ),
                },
                {
                    "ANTHROPIC_BASE_URL": "https://provider.example.test/anthropic",
                    "ANTHROPIC_AUTH_TOKEN": "test-provider-token",
                    "ANTHROPIC_MODEL": "test-model",
                },
            )

            documents = list(yaml.safe_load_all(bundle.read_text(encoding="utf-8")))
            agent_config = next(
                document
                for document in documents
                if document["kind"] == "ConfigMap"
                and document["metadata"]["name"] == "agent-service-config"
            )
            platform_registry = yaml.safe_load(agent_config["data"]["config.yaml"])[
                "platform_registry"
            ]
            self.assertEqual(
                platform_registry["registry"], f"localhost:{local_dev.REGISTRY_PORT}"
            )
            self.assertEqual(
                platform_registry["ca_file"], "/etc/labweaver/secrets/harbor-ca.crt"
            )
            agent_secret = next(
                document
                for document in documents
                if document["kind"] == "Secret"
                and document["metadata"]["name"] == "agent-service-secrets"
            )
            self.assertEqual(
                base64.b64decode(agent_secret["data"]["harbor-ca.crt"]),
                (foundation / "authority" / "ca.crt").read_bytes(),
            )
            self.assertEqual(
                base64.b64decode(agent_secret["data"]["harbor-username"]), b"local-dev"
            )
            self.assertEqual(
                base64.b64decode(agent_secret["data"]["harbor-password"]), b"local-dev"
            )
            build_executor_secret = next(
                (
                    document
                    for document in documents
                    if document["kind"] == "Secret"
                    and document["metadata"]["name"] == "build-executor-secrets"
                ),
                None,
            )
            self.assertIsNone(build_executor_secret)

    @staticmethod
    def _write_real_build_provider(root: Path) -> local_dev.local_dev_build.RealBuildProvider:
        """Write the private Harbor and BuildKit provider files for one bundle render."""

        provider_root = root / "provider"
        provider_root.mkdir()
        files = {
            "harbor-ca.crt": b"harbor-ca",
            "builder-username": b"robot$labweaver-system+platform-build-executor\n",
            "builder-password": b"builder-token\n",
            "runtime-username": b"robot$labweaver-system+runtime-puller\n",
            "runtime-password": b"runtime-token\n",
            "registry-pull-config.json": b'{"auths": {"harbor.lab.lan": {"auth": "runtime"}}}\n',
            "buildkit-ca.crt": b"buildkit-ca",
            "buildkit-client.crt": b"buildkit-client-crt",
            "buildkit-client.key": b"buildkit-client-key",
            "chart.tgz": b"chart",
        }
        paths = {}
        for name, value in files.items():
            path = provider_root / name
            path.write_bytes(value)
            paths[name] = path
        return local_dev.local_dev_build.RealBuildProvider(
            registry_host="harbor.lab.lan",
            registry_service_ip="10.96.0.42",
            harbor_api="https://harbor.lab.lan/",
            buildkit_address="tcp://buildkit.labweaver-build.svc:1234",
            harbor_ca_file=paths["harbor-ca.crt"],
            builder_username_file=paths["builder-username"],
            builder_password_file=paths["builder-password"],
            runtime_username_file=paths["runtime-username"],
            runtime_password_file=paths["runtime-password"],
            registry_pull_config_file=paths["registry-pull-config.json"],
            buildkit_ca_file=paths["buildkit-ca.crt"],
            buildkit_client_certificate_file=paths["buildkit-client.crt"],
            buildkit_client_private_key_file=paths["buildkit-client.key"],
            chart_archive=paths["chart.tgz"],
            project_storage_quota_bytes=4 * 1024 * 1024 * 1024,
            buildkit_network_policy_mode="kindnet-network-policy-unenforced;cilium-unavailable",
        )

    def test_real_provider_bundle_uses_harbor_buildkit_inputs_and_quota(self) -> None:
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
            build_nats = foundation / "nats-clients" / "build-executor"
            build_nats.mkdir(parents=True)
            for name in ("nats.creds", "nats-client.crt", "nats-client.key"):
                (build_nats / name).write_bytes(f"build-{name}".encode())
            for name in ("ssh_host_ed25519_key", "target_key", "target_key.pub", "target_key-cert.pub"):
                (work / name).write_bytes(name.encode())

            provider = self._write_real_build_provider(root)
            images = {
                "evaluation_service": "localhost:5001/labweaver/local/evaluation-service@sha256:" + "a" * 64,
                "evaluation_runner": "localhost:5001/labweaver/local/evaluation-runner@sha256:" + "b" * 64,
                "authoring_sandbox": "localhost:5001/labweaver/local/authoring-sandbox@sha256:" + "c" * 64,
            }
            bundle, _resource_bundle, _ = make_app_input_with_local_cidrs(
                work,
                foundation,
                images,
                {
                    "ANTHROPIC_BASE_URL": "https://provider.example.test/anthropic",
                    "ANTHROPIC_AUTH_TOKEN": "test-provider-token",
                    "ANTHROPIC_MODEL": "test-model",
                },
                provider,
            )

            documents = list(yaml.safe_load_all(bundle.read_text(encoding="utf-8")))
            build_config = next(
                document["data"]["config.yaml"]
                for document in documents
                if document["kind"] == "ConfigMap"
                and document["metadata"]["name"] == "build-executor-config"
            )
            build_values = yaml.safe_load(build_config)
            self.assertEqual(build_values["executor"]["harborRegistry"], "harbor.lab.lan")
            self.assertEqual(build_values["executor"]["harborApi"], "https://harbor.lab.lan/")
            self.assertEqual(
                build_values["executor"]["projectStorageQuotaBytes"],
                provider.project_storage_quota_bytes,
            )
            build_secret = next(
                document
                for document in documents
                if document["kind"] == "Secret"
                and document["metadata"]["name"] == "build-executor-secrets"
            )
            self.assertEqual(
                base64.b64decode(build_secret["data"]["harbor-ca.crt"]),
                b"harbor-ca",
            )
            self.assertEqual(
                base64.b64decode(build_secret["data"]["buildkit-client.key"]),
                b"buildkit-client-key",
            )
            self.assertEqual(
                local_dev.real_build_helm_values(provider),
                {
                    "workloads": {
                        "build-executor": {
                            "enabled": True,
                            "hostAliases": [
                                {"ip": "10.96.0.42", "hostnames": ["harbor.lab.lan"]}
                            ],
                        }
                    }
                },
            )

    def test_authoring_buildkit_sidecar_activates_only_the_two_reviewed_keys(self) -> None:
        example = ROOT / "deploy/config/agent-control-plane.yaml.example"
        original = example.read_text(encoding="utf-8")
        self.assertIn('  # buildkit_image: "', original)
        self.assertIn('  # buildkit_config_map_name: "authoring-buildkit-config"', original)
        image = "docker.io/moby/buildkit:v0.31.1-rootless@sha256:" + "d" * 64

        rendered = local_dev.render_authoring_buildkit_sidecar(original, image)

        original_lines = original.splitlines()
        rendered_lines = rendered.splitlines()
        self.assertEqual(len(original_lines), len(rendered_lines))
        changed = [
            (before, after)
            for before, after in zip(original_lines, rendered_lines)
            if before != after
        ]
        self.assertEqual(len(changed), 2)
        self.assertTrue(changed[0][0].startswith("  # buildkit_image: "))
        self.assertEqual(changed[0][1], f'  buildkit_image: "{image}"')
        self.assertEqual(
            changed[1],
            (
                '  # buildkit_config_map_name: "authoring-buildkit-config"',
                '  buildkit_config_map_name: "authoring-buildkit-config"',
            ),
        )
        sandbox = yaml.safe_load(rendered)["sandbox"]
        self.assertEqual(sandbox["buildkit_image"], image)
        self.assertEqual(sandbox["buildkit_config_map_name"], "authoring-buildkit-config")

    def test_authoring_buildkit_sidecar_requires_both_commented_anchors(self) -> None:
        image = "docker.io/moby/buildkit:v0.31.1-rootless@sha256:" + "d" * 64
        with self.assertRaisesRegex(
            local_dev.LocalDevError,
            r"no commented sandbox\.buildkit_image anchor",
        ):
            local_dev.render_authoring_buildkit_sidecar('sandbox:\n  image: "pinned"\n', image)
        with self.assertRaisesRegex(
            local_dev.LocalDevError,
            r"no commented sandbox\.buildkit_config_map_name anchor",
        ):
            local_dev.render_authoring_buildkit_sidecar(
                'sandbox:\n  # buildkit_image: "old"\n', image
            )

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
