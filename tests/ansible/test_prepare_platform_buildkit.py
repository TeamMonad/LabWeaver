"""Safety contracts for private Sprint 2 BuildKit authoring."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
AUTHORING_CA_PATH = "/etc/buildkit/registry-ca.crt"
SCRIPT = ROOT / "tools/prepare_platform_buildkit.py"
SPEC = importlib.util.spec_from_file_location("prepare_platform_buildkit", SCRIPT)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("BuildKit authoring module could not be loaded")
BUILDKIT = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = BUILDKIT
SPEC.loader.exec_module(BUILDKIT)


class BuildkitAuthoringTests(unittest.TestCase):
    def test_output_must_be_new_and_private(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            with self.assertRaisesRegex(
                BUILDKIT.BuildkitAuthoringError,
                "LW_PLATFORM_BUILDKIT_PRIVATE_PATH_REQUIRED",
            ):
                BUILDKIT._private_output(root / "buildkit")

            private = root / ".private"
            private.mkdir()
            output = private / "buildkit"
            self.assertEqual(BUILDKIT._private_output(output), output.resolve())
            output.mkdir()
            with self.assertRaisesRegex(
                BUILDKIT.BuildkitAuthoringError,
                "LW_PLATFORM_BUILDKIT_OUTPUT_EXISTS",
            ):
                BUILDKIT._private_output(output)

    def test_validity_is_bounded_before_tool_execution(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for name in ("home", "authority", "render-input", "build-executor-client"):
                (root / name).mkdir()
            with self.assertRaisesRegex(
                BUILDKIT.BuildkitAuthoringError,
                "LW_PLATFORM_BUILDKIT_VALIDITY_INVALID",
            ):
                BUILDKIT.prepare(
                    root,
                    Path("/not/invoked"),
                    1,
                    "harbor.lab.lan",
                    "10.96.0.10",
                    Path("/not/invoked"),
                )

    def test_registry_identity_is_validated_before_authoring(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            with self.assertRaisesRegex(
                BUILDKIT.BuildkitAuthoringError,
                "LW_PLATFORM_BUILDKIT_REGISTRY_HOST_INVALID",
            ):
                BUILDKIT.prepare(
                    root,
                    Path("/not/invoked"),
                    365,
                    "https://harbor.lab.lan",
                    "10.96.0.10",
                    Path("/not/invoked"),
                )

    def test_worker_never_installs_bridge_networking_in_the_attempt_namespace(self) -> None:
        # The authoring sidecar shares the attempt pod network namespace. A worker that installs
        # bridge networking there breaks the attempt's own connections, including the result
        # upload, so the reviewed configuration has to pin host networking explicitly.
        standalone = BUILDKIT.buildkitd_configuration(
            "harbor.lab.lan", "10.96.0.10", True, "/etc/buildkit/registry-ca.crt"
        )
        sidecar = BUILDKIT.buildkitd_configuration(
            "harbor.lab.lan", None, False, AUTHORING_CA_PATH
        )
        for configuration in (standalone, sidecar):
            self.assertIn('[worker.oci]', configuration)
            self.assertIn('networkMode = "host"', configuration)
            self.assertIn("noProcessSandbox = true", configuration)
            self.assertNotIn("networkMode = \"auto\"", configuration)

    def test_dns_nameserver_is_an_explicit_ip_address(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            with self.assertRaisesRegex(
                BUILDKIT.BuildkitAuthoringError,
                "LW_PLATFORM_BUILDKIT_DNS_NAMESERVER_INVALID",
            ):
                BUILDKIT.prepare(
                    root,
                    Path("/not/invoked"),
                    365,
                    "harbor.lab.lan",
                    "kube-dns.kube-system.svc",
                    Path("/not/invoked"),
                )


if __name__ == "__main__":
    unittest.main()
