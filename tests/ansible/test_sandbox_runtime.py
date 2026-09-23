"""Behavioural tests for the sandbox container-runtime configuration.

The sandbox `RuntimeClass` only works when the container runtime applies the
gVisor runtime to a Pod *and* to its sandbox container. These tests render the
configuration exactly as Ansible does, parse it as TOML, and assert the runtime
table the runtime will actually consume.
"""

from __future__ import annotations

import tomllib
import unittest
from pathlib import Path

from jinja2 import Environment, StrictUndefined


ROOT = Path(__file__).resolve().parents[2]
TEMPLATE = (
    ROOT
    / "deploy"
    / "ansible"
    / "roles"
    / "sandbox_runtime"
    / "templates"
    / "containerd-config.toml.j2"
)
CRI_RUNTIME = "io.containerd.cri.v1.runtime"
CRI_IMAGES = "io.containerd.cri.v1.images"
HANDLER = "labweaver-sandbox"
PAUSE_IMAGE = "registry.example.test/pause:3.10.1"


def render(nvidia_available: bool) -> dict:
    environment = Environment(undefined=StrictUndefined, keep_trailing_newline=True)
    # Ansible provides these filters; plain Jinja2 does not.
    environment.filters["bool"] = lambda value: bool(value)
    rendered = environment.from_string(TEMPLATE.read_text(encoding="utf-8")).render(
        sandbox_runtime_name=HANDLER,
        sandbox_runtime_runtime_type="io.containerd.runsc.v1",
        sandbox_runtime_default_runtime_name="runc",
        sandbox_runtime_nvidia_runtime_name="nvidia",
        sandbox_runtime_nvidia_runtime_binary="/usr/bin/nvidia-container-runtime",
        sandbox_runtime_nvidia_available=nvidia_available,
        sandbox_runtime_lock={"containerd": {"sandbox_image": PAUSE_IMAGE}},
    )
    return tomllib.loads(rendered)


class SandboxContainerRuntimeConfigTest(unittest.TestCase):
    def runtime_table(self, config: dict) -> dict:
        return config["plugins"][CRI_RUNTIME]["containerd"]["runtimes"]

    def test_the_sandbox_handler_is_a_gvisor_runtime(self) -> None:
        runtimes = self.runtime_table(render(nvidia_available=False))
        self.assertIn(HANDLER, runtimes)
        self.assertEqual(runtimes[HANDLER]["runtime_type"], "io.containerd.runsc.v1")
        # gVisor's shim rejects unknown option keys, so the table carries none.
        self.assertNotIn("options", runtimes[HANDLER])

    def test_every_runtime_creates_the_pod_sandbox_itself(self) -> None:
        runtimes = self.runtime_table(render(nvidia_available=True))
        for name, table in runtimes.items():
            self.assertEqual(
                table["sandboxer"],
                "podsandbox",
                f"runtime {name} must own the Pod sandbox container",
            )

    def test_the_default_runtime_stays_the_node_runtime(self) -> None:
        config = render(nvidia_available=False)
        containerd = config["plugins"][CRI_RUNTIME]["containerd"]
        self.assertEqual(containerd["default_runtime_name"], "runc")
        self.assertEqual(containerd["runtimes"]["runc"]["runtime_type"], "io.containerd.runc.v2")
        self.assertIs(containerd["runtimes"]["runc"]["options"]["SystemdCgroup"], True)
        self.assertNotIn("nvidia", containerd["runtimes"])

    def test_a_gpu_node_keeps_the_device_runtime(self) -> None:
        runtimes = self.runtime_table(render(nvidia_available=True))
        self.assertEqual(
            runtimes["nvidia"]["options"]["BinaryName"], "/usr/bin/nvidia-container-runtime"
        )
        self.assertIs(runtimes["nvidia"]["options"]["SystemdCgroup"], True)

    def test_the_pod_sandbox_image_is_the_locked_one(self) -> None:
        config = render(nvidia_available=False)
        self.assertEqual(
            config["plugins"][CRI_IMAGES]["pinned_images"]["sandbox"], PAUSE_IMAGE
        )

    def test_slow_proxied_pulls_are_not_aborted_early(self) -> None:
        images = render(nvidia_available=False)["plugins"][CRI_IMAGES]
        self.assertEqual(images["image_pull_progress_timeout"], "30m")

    def test_the_cri_uses_the_platform_cni_configuration(self) -> None:
        cni = render(nvidia_available=False)["plugins"][CRI_RUNTIME]["cni"]
        self.assertEqual(cni["conf_dir"], "/etc/cni/net.d")
        self.assertIn("/opt/cni/bin", cni["bin_dirs"])


if __name__ == "__main__":
    unittest.main()
