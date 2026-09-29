#!/usr/bin/env python3
"""Prepare an NVIDIA vGPU Ubuntu guest disk through a disposable Docker builder.

The command keeps the host unchanged: only the caller's work directory and
``/dev/kvm`` are made visible to the builder. It verifies the three reviewed
input hashes, inspects and resizes the base disk to a standalone qcow2, installs the
matching kernel/header set, driver package, and gridd patcher, then emits the
prepared disk plus the one-file tar archive accepted by the Agent containerdisk
import path. The DLS token and DLS root certificate are deliberately not
inputs to this command; the VM bootstrap applies licensing material later.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
from pathlib import Path, PurePosixPath
import shlex
import shutil
import subprocess
import sys


DEFAULT_BASE_SHA256 = "ffe6203da54deeb6db5d2a98a83f9ec8e55f149d3f7ba622e1abe5fa966ee3d6"
DEFAULT_DRIVER_SHA256 = "e208b0dd684b0596254903702758e12254fe55b041b4fd41792fb29eb42f1c94"
DEFAULT_PATCHER_SHA256 = "e811c3896b434c399a079378559ff05778129c0a5bcc21b9ee7c8be13b83d05b"
DEFAULT_BUILDER_IMAGE = "labweaver/vgpu-guest-builder:ubuntu-24.04"
CONTAINERDISK_PATH = "disk/disk.qcow2"
CAPACITY_INFO_PATH = ".labweaver-vgpu-capacity.json"
GUEST_PATCHER_PATH = "/usr/local/bin/gridd-unlock-patcher"


class GuestImageError(Exception):
    """A fail-closed guest image preparation error."""


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _hash_input(path: Path, expected: str, label: str) -> str:
    if len(expected) != 64 or any(character not in "0123456789abcdef" for character in expected):
        raise GuestImageError(f"LW_VGPU_{label.upper()}_HASH_INVALID")
    actual = sha256_file(path)
    if actual != expected:
        raise GuestImageError(f"LW_VGPU_{label.upper()}_HASH_MISMATCH")
    return actual


def _relative_file(path: Path, work_dir: Path, label: str) -> tuple[Path, str]:
    if path.is_symlink():
        raise GuestImageError(f"LW_VGPU_{label.upper()}_INVALID")
    try:
        resolved = path.resolve(strict=True)
    except OSError as error:
        raise GuestImageError(f"LW_VGPU_{label.upper()}_MISSING") from error
    if not resolved.is_file():
        raise GuestImageError(f"LW_VGPU_{label.upper()}_INVALID")
    try:
        relative = resolved.relative_to(work_dir)
    except ValueError as error:
        raise GuestImageError(f"LW_VGPU_{label.upper()}_OUTSIDE_WORK_DIR") from error
    if not relative.parts or any(part in {".", ".."} for part in relative.parts):
        raise GuestImageError(f"LW_VGPU_{label.upper()}_INVALID")
    return resolved, PurePosixPath(*relative.parts).as_posix()


def _relative_output(path: Path, work_dir: Path, label: str) -> tuple[Path, str]:
    resolved = path.resolve()
    try:
        relative = resolved.relative_to(work_dir)
    except ValueError as error:
        raise GuestImageError(f"LW_VGPU_{label.upper()}_OUTSIDE_WORK_DIR") from error
    if not relative.parts or any(part in {".", ".."} for part in relative.parts):
        raise GuestImageError(f"LW_VGPU_{label.upper()}_INVALID")
    if resolved.exists():
        raise GuestImageError(f"LW_VGPU_{label.upper()}_ALREADY_EXISTS")
    resolved.parent.mkdir(parents=True, exist_ok=True)
    return resolved, PurePosixPath(*relative.parts).as_posix()


def _docker_run(arguments: list[str]) -> subprocess.CompletedProcess[str]:
    if shutil.which("docker") is None:
        raise GuestImageError("LW_VGPU_DOCKER_REQUIRED")
    try:
        return subprocess.run(
            ["docker", *arguments],
            check=True,
            text=True,
            stdin=subprocess.DEVNULL,
        )
    except OSError as error:
        raise GuestImageError("LW_VGPU_DOCKER_FAILED") from error
    except subprocess.CalledProcessError as error:
        raise GuestImageError("LW_VGPU_BUILDER_FAILED") from error


def _build_builder_image(root: Path, image: str) -> None:
    containerfile = root / "containers" / "Containerfile.vgpu-guest-builder"
    if not containerfile.is_file():
        raise GuestImageError("LW_VGPU_BUILDER_CONTAINERFILE_MISSING")
    _docker_run(
        [
            "build",
            "--platform",
            "linux/amd64",
            "--pull=false",
            "--file",
            str(containerfile),
            "--tag",
            image,
            str(containerfile.parent),
        ]
    )


def _builder_script(
    *,
    base: str,
    driver: str,
    patcher: str,
    stage: str,
    output: str,
    prepared_disk: str,
    capacity_info: str,
    base_sha256: str,
    driver_sha256: str,
    patcher_sha256: str,
) -> str:
    patcher_guest = "/var/tmp/labweaver-vgpu/patcher"
    marker = json.dumps(
        {
            "schema_version": "labweaver-vgpu-guest-image.v1",
            "base_sha256": base_sha256,
            "driver_sha256": driver_sha256,
            "patcher_sha256": patcher_sha256,
            "license_material": "bootstrap-only",
            "containerdisk_path": CONTAINERDISK_PATH,
            "patcher_path": GUEST_PATCHER_PATH,
        },
        sort_keys=True,
        separators=(",", ":"),
    )
    marker_b64 = base64.b64encode(marker.encode("utf-8")).decode("ascii")
    return f"""#!/usr/bin/env bash
set -Eeuo pipefail

base={shlex.quote("/work/" + base)}
driver={shlex.quote("/work/" + driver)}
patcher={shlex.quote("/work/" + patcher)}
stage={shlex.quote("/work/" + stage)}
output={shlex.quote("/work/" + output)}
prepared={shlex.quote("/work/" + prepared_disk)}
capacity_info={shlex.quote("/work/" + capacity_info)}
guest_patcher={shlex.quote(patcher_guest)}
marker_b64={shlex.quote(marker_b64)}

cleanup() {{
    rm -rf -- "$stage"
}}
trap cleanup EXIT

rm -rf -- "$stage"
mkdir -p -- "$stage/disk"
qemu-img check "$base"
base_virtual_size=$(qemu-img info --output=json "$base" | python3 -c \\
    'import json, sys; print(json.load(sys.stdin)["virtual-size"])')
test "$base_virtual_size" -gt 0
target_virtual_size=$((base_virtual_size + 4 * 1024 * 1024 * 1024))
root_device=$(virt-inspector --no-applications --no-icon -a "$base" \\
    | virt-inspector --xpath 'string(//mountpoint[text()="/"]/@dev)')
test -n "$root_device"
qemu-img create -f qcow2 -o preallocation=metadata \\
    "$stage/guest.qcow2" "$target_virtual_size"
if [[ "$root_device" =~ ^/dev/[^/]+[0-9]+$ ]]; then
    virt-resize --expand "$root_device" "$base" "$stage/guest.qcow2"
elif [[ "$root_device" == /dev/*/* ]]; then
        pv_partition=$(virt-filesystems --long --all --parts --blkdevs --pvs \\
            --vgs --lvs -a "$base" | awk '$2 == "pv" {{ print $1; count += 1 }} END {{ if (count != 1) exit 1 }}')
        test -n "$pv_partition"
        virt-resize --expand "$pv_partition" --LV-expand "$root_device" \\
            "$base" "$stage/guest.qcow2"
else
    echo "unsupported guest root device: $root_device" >&2
    exit 1
fi

cp -- "$driver" "$stage/driver.deb"
cp -- "$patcher" "$stage/patcher"

virt-customize --format=qcow2 --network -a "$stage/guest.qcow2" \\
    --mkdir /var/tmp/labweaver-vgpu \\
    --copy-in "$stage/driver.deb:/var/tmp/labweaver-vgpu" \\
    --copy-in "$stage/patcher:/var/tmp/labweaver-vgpu" \\
    --run-command 'DEBIAN_FRONTEND=noninteractive apt-get -o APT::Update::Error-Mode=any update' \\
    --run-command 'DEBIAN_FRONTEND=noninteractive apt-get install --yes --no-install-recommends dkms build-essential linux-image-generic linux-headers-generic kmod' \\
    --run-command 'cd /var/tmp/labweaver-vgpu && DEBIAN_FRONTEND=noninteractive apt-get install --yes --no-install-recommends ./driver.deb' \\
    --run-command "install -D -m 0755 $guest_patcher {GUEST_PATCHER_PATH}" \\
    --run-command "command -v nvidia-gridd >/dev/null" \\
    --run-command "printf '%s' $marker_b64 | base64 --decode > /etc/labweaver-vgpu-image.json" \\
    --run-command 'rm -rf /var/tmp/labweaver-vgpu /var/lib/apt/lists/*'

virt-sysprep --format=qcow2 -a "$stage/guest.qcow2" \\
    --operations machine-id,net-hwaddr,ssh-hostkeys

qemu-img convert -p -f qcow2 -O qcow2 -c \
    "$stage/guest.qcow2" "$stage/{CONTAINERDISK_PATH}"
cp --reflink=auto --sparse=never "$stage/{CONTAINERDISK_PATH}" "$prepared"
qemu-img info --output=json "$prepared" > "$capacity_info"
tar --format=posix -cf "$output" -C "$stage" {CONTAINERDISK_PATH}
"""


def prepare(arguments: argparse.Namespace, root: Path) -> dict[str, object]:
    work_dir = arguments.work_dir.resolve(strict=True)
    if not work_dir.is_dir():
        raise GuestImageError("LW_VGPU_WORK_DIR_INVALID")

    base, base_rel = _relative_file(arguments.base_image, work_dir, "base_image")
    driver, driver_rel = _relative_file(arguments.driver_package, work_dir, "driver")
    patcher, patcher_rel = _relative_file(arguments.patcher, work_dir, "patcher")
    output, output_rel = _relative_output(arguments.output, work_dir, "output")
    prepared, prepared_rel = _relative_output(arguments.prepared_disk, work_dir, "prepared_disk")
    capacity_info, capacity_info_rel = _relative_output(
        work_dir / CAPACITY_INFO_PATH, work_dir, "capacity_info"
    )
    if (
        len({base, driver, patcher}) != 3
        or output in {base, driver, patcher}
        or prepared in {base, driver, patcher}
    ):
        raise GuestImageError("LW_VGPU_OUTPUT_OVERWRITES_INPUT")
    if output == prepared:
        raise GuestImageError("LW_VGPU_OUTPUTS_MUST_DIFFER")

    base_sha256 = _hash_input(base, arguments.base_sha256, "base_image")
    driver_sha256 = _hash_input(driver, arguments.driver_sha256, "driver")
    patcher_sha256 = _hash_input(patcher, arguments.patcher_sha256, "patcher")

    stage_rel = ".labweaver-vgpu-guest-build"
    stage = work_dir / stage_rel
    if stage.exists():
        if not stage.is_dir() or stage.is_symlink():
            raise GuestImageError("LW_VGPU_STAGING_PATH_INVALID")
        shutil.rmtree(stage)

    if arguments.manifest is not None:
        manifest, _ = _relative_output(arguments.manifest, work_dir, "manifest")
    else:
        manifest = output.with_suffix(".json")
        if manifest.exists():
            raise GuestImageError("LW_VGPU_MANIFEST_ALREADY_EXISTS")
    if capacity_info in {base, driver, patcher, output, prepared, manifest}:
        raise GuestImageError("LW_VGPU_CAPACITY_INFO_PATH_CONFLICT")

    try:
        _build_builder_image(root, arguments.builder_image)
        script = _builder_script(
            base=base_rel,
            driver=driver_rel,
            patcher=patcher_rel,
            stage=stage_rel,
            output=output_rel,
            prepared_disk=prepared_rel,
            capacity_info=capacity_info_rel,
            base_sha256=base_sha256,
            driver_sha256=driver_sha256,
            patcher_sha256=patcher_sha256,
        )
        _docker_run(
            [
                "run",
                "--rm",
                "--platform",
                "linux/amd64",
                "--network",
                "default",
                "--device",
                "/dev/kvm:/dev/kvm",
                "--mount",
                f"type=bind,source={work_dir},target=/work",
                "--workdir",
                "/work",
                "--entrypoint",
                "/bin/bash",
                arguments.builder_image,
                "-ceu",
                script,
            ]
        )
        try:
            capacity_data = json.loads(capacity_info.read_text(encoding="utf-8"))
            capacity_bytes = capacity_data["virtual-size"]
        except (OSError, KeyError, TypeError, ValueError) as error:
            raise GuestImageError("LW_VGPU_CAPACITY_INFO_INVALID") from error
        if not isinstance(capacity_bytes, int) or capacity_bytes <= 0:
            raise GuestImageError("LW_VGPU_CAPACITY_INFO_INVALID")
        disk_sha256 = sha256_file(prepared)
        metadata = {
            "schema_version": "labweaver-vgpu-guest-image.v1",
            "base_sha256": base_sha256,
            "driver_sha256": driver_sha256,
            "patcher_sha256": patcher_sha256,
            "disk_sha256": disk_sha256,
            "disk_format": "qcow2",
            "disk_path": CONTAINERDISK_PATH,
            "capacity_bytes": capacity_bytes,
            "license_material": "bootstrap-only",
            "output": output.name,
            "prepared_disk": prepared.name,
        }
        manifest.write_text(json.dumps(metadata, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        return metadata
    except Exception:
        for path in (output, prepared, manifest):
            if path is not None and path.exists() and path.is_file():
                path.unlink()
        raise
    finally:
        if capacity_info.exists() and capacity_info.is_file():
            capacity_info.unlink()
        if stage.exists() and stage.is_dir() and not stage.is_symlink():
            shutil.rmtree(stage)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--work-dir", type=Path, required=True)
    parser.add_argument("--base-image", type=Path, required=True)
    parser.add_argument("--driver-package", type=Path, required=True)
    parser.add_argument("--patcher", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True, help="Agent containerdisk tar archive")
    parser.add_argument("--prepared-disk", type=Path, required=True, help="Standalone prepared qcow2")
    parser.add_argument("--manifest", type=Path)
    parser.add_argument("--builder-image", default=DEFAULT_BUILDER_IMAGE)
    parser.add_argument("--base-sha256", default=DEFAULT_BASE_SHA256)
    parser.add_argument("--driver-sha256", default=DEFAULT_DRIVER_SHA256)
    parser.add_argument("--patcher-sha256", default=DEFAULT_PATCHER_SHA256)
    return parser


def main(argv: list[str] | None = None) -> int:
    arguments = build_parser().parse_args(argv)
    root = Path(__file__).resolve().parents[1]
    try:
        metadata = prepare(arguments, root)
    except (GuestImageError, OSError, subprocess.SubprocessError) as error:
        print(str(error) or "LW_VGPU_GUEST_IMAGE_FAILED", file=sys.stderr)
        return 1
    print(json.dumps(metadata, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
