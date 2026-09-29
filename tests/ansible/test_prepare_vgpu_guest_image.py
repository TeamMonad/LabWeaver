"""Focused checks for the Docker-only vGPU guest image preparation entrypoint."""

from __future__ import annotations

import hashlib
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "prepare_vgpu_guest_image", ROOT / "tools/prepare_vgpu_guest_image.py"
)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class PrepareVgpuGuestImageTests(unittest.TestCase):
    def test_builder_image_keeps_libguestfs_network_dependencies(self) -> None:
        containerfile = (ROOT / "containers/Containerfile.vgpu-guest-builder").read_text(encoding="utf-8")

        self.assertIn("apt-get -o APT::Update::Error-Mode=any update", containerfile)
        for package in ("dhcpcd-base", "iproute2", "isc-dhcp-client"):
            self.assertIn(f"        {package} \\", containerfile)

    def test_builder_script_has_guest_install_and_no_license_material_input(self) -> None:
        script = MODULE._builder_script(
            base="ubuntu-vgpu.qcow2",
            driver="nvidia-driver.deb",
            patcher="gridd-unlock-patcher",
            stage=".build",
            output="guest.containerdisk.tar",
            prepared_disk="guest.qcow2",
            capacity_info=".capacity.json",
            base_sha256="a" * 64,
            driver_sha256="b" * 64,
            patcher_sha256="c" * 64,
        )

        self.assertIn("qemu-img create", script)
        self.assertIn("qemu-img info --output=json", script)
        self.assertIn("virt-resize --expand", script)
        self.assertIn("linux-headers-generic", script)
        self.assertIn("apt-get -o APT::Update::Error-Mode=any update", script)
        self.assertIn("apt-get install --yes --no-install-recommends ./driver.deb", script)
        self.assertIn("gridd-unlock-patcher", script)
        self.assertIn("/usr/local/bin/gridd-unlock-patcher", script)
        self.assertIn("virt-sysprep", script)
        self.assertIn("qemu-img convert -p -f qcow2 -O qcow2 -c", script)
        self.assertIn("cp --reflink=auto --sparse=never", script)
        self.assertIn("tar --format=posix -cf", script)
        self.assertNotIn("tar --format=posix --sparse", script)
        self.assertNotIn("root-certificate", script)
        self.assertNotIn("client-token", script)
        self.assertNotIn("root.pem", script)
        self.assertNotIn("/usr/local/lib/labweaver-vgpu", script)

    def test_prepare_verifies_inputs_and_cleans_staging(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            work_dir = Path(temporary)
            files = {
                "base.qcow2": b"base",
                "driver.deb": b"driver",
                "patcher": b"patcher",
            }
            for name, contents in files.items():
                (work_dir / name).write_bytes(contents)

            docker_calls: list[list[str]] = []

            def fake_docker(arguments: list[str]):
                docker_calls.append(arguments)
                if arguments[0] == "run":
                    mount = next(value for value in arguments if value.startswith("type=bind,"))
                    mounted_work_dir = Path(mount.split("source=", 1)[1].split(",target=", 1)[0])
                    (mounted_work_dir / "prepared.qcow2").write_bytes(b"prepared")
                    (mounted_work_dir / "guest.containerdisk.tar").write_bytes(b"tar")
                    (mounted_work_dir / ".labweaver-vgpu-capacity.json").write_text(
                        '{"virtual-size": 17179869184}', encoding="utf-8"
                    )
                return mock.Mock()

            arguments = MODULE.build_parser().parse_args(
                [
                    "--work-dir",
                    str(work_dir),
                    "--base-image",
                    str(work_dir / "base.qcow2"),
                    "--driver-package",
                    str(work_dir / "driver.deb"),
                    "--patcher",
                    str(work_dir / "patcher"),
                    "--output",
                    str(work_dir / "guest.containerdisk.tar"),
                    "--prepared-disk",
                    str(work_dir / "prepared.qcow2"),
                    "--base-sha256",
                    hashlib.sha256(files["base.qcow2"]).hexdigest(),
                    "--driver-sha256",
                    hashlib.sha256(files["driver.deb"]).hexdigest(),
                    "--patcher-sha256",
                    hashlib.sha256(files["patcher"]).hexdigest(),
                ]
            )

            with mock.patch.object(MODULE, "_build_builder_image"), mock.patch.object(
                MODULE, "_docker_run", side_effect=fake_docker
            ):
                metadata = MODULE.prepare(arguments, ROOT)

            self.assertEqual(metadata["disk_sha256"], hashlib.sha256(b"prepared").hexdigest())
            self.assertEqual(metadata["disk_path"], "disk/disk.qcow2")
            self.assertEqual(metadata["capacity_bytes"], 17179869184)
            self.assertFalse((work_dir / ".labweaver-vgpu-guest-build").exists())
            self.assertTrue((work_dir / "guest.containerdisk.json").exists())
            run_arguments = docker_calls[-1]
            entrypoint_index = run_arguments.index("--entrypoint")
            image_index = run_arguments.index(MODULE.DEFAULT_BUILDER_IMAGE)
            self.assertEqual(run_arguments[entrypoint_index + 1], "/bin/bash")
            self.assertEqual(run_arguments[image_index + 1], "-ceu")

    def test_prepare_rejects_hash_mismatch_before_docker(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            work_dir = Path(temporary)
            for name in ("base.qcow2", "driver.deb", "patcher"):
                (work_dir / name).write_bytes(name.encode())
            arguments = MODULE.build_parser().parse_args(
                [
                    "--work-dir",
                    str(work_dir),
                    "--base-image",
                    str(work_dir / "base.qcow2"),
                    "--driver-package",
                    str(work_dir / "driver.deb"),
                    "--patcher",
                    str(work_dir / "patcher"),
                    "--output",
                    str(work_dir / "guest.containerdisk.tar"),
                    "--prepared-disk",
                    str(work_dir / "prepared.qcow2"),
                ]
            )
            with mock.patch.object(MODULE, "_build_builder_image") as build:
                with self.assertRaises(MODULE.GuestImageError) as context:
                    MODULE.prepare(arguments, ROOT)
            self.assertEqual(str(context.exception), "LW_VGPU_BASE_IMAGE_HASH_MISMATCH")
            build.assert_not_called()


if __name__ == "__main__":
    unittest.main()
