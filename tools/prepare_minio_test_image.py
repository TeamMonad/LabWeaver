#!/usr/bin/env python3
"""Build the pinned official-source MinIO image used by artifact-store tests."""

from __future__ import annotations

import argparse
from pathlib import Path
import subprocess


MINIO_TEST_LOCK = "deploy/versions.lock.yml"


def load_lock(root: Path) -> dict[str, str]:
    """Read the flat minio_test mapping without requiring a YAML package."""

    values: dict[str, str] = {}
    in_section = False
    for line in (root / MINIO_TEST_LOCK).read_text(encoding="utf-8").splitlines():
        if line == "  minio_test:":
            in_section = True
            continue
        if in_section and line.startswith("  ") and not line.startswith("    "):
            break
        if in_section and line.startswith("    "):
            key, separator, value = line.strip().partition(":")
            if separator and key in {"release", "source_commit", "builder_image", "runtime_image"}:
                values[key] = value.strip()
    required = {"release", "source_commit", "builder_image", "runtime_image"}
    if set(values) != required:
        raise ValueError("deploy/versions.lock.yml minio_test lock is incomplete")
    return values


def build_command(root: Path, image: str) -> list[str]:
    """Return the deterministic local BuildKit command for the test image."""

    lock = load_lock(root)
    return [
        "docker",
        "buildx",
        "build",
        "--file",
        "containers/Containerfile.minio-source",
        "--platform",
        "linux/amd64",
        "--pull=false",
        "--provenance=false",
        "--load",
        "--build-arg",
        f"MINIO_RELEASE={lock['release']}",
        "--build-arg",
        f"MINIO_SOURCE_COMMIT={lock['source_commit']}",
        "--build-arg",
        f"MINIO_GO_IMAGE={lock['builder_image']}",
        "--build-arg",
        f"MINIO_RUNTIME_IMAGE={lock['runtime_image']}",
        "--tag",
        image,
        ".",
    ]


def build(root: Path, image: str) -> None:
    subprocess.run(build_command(root, image), cwd=root, check=True)
    subprocess.run(["docker", "image", "inspect", image], cwd=root, check=True)


def default_image(root: Path) -> str:
    return f"labweaver/minio-test:{load_lock(root)['source_commit'][:12]}"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    root = Path(__file__).resolve().parents[1]
    parser.add_argument("--image", default=default_image(root))
    arguments = parser.parse_args()
    build(root, arguments.image)
    print(arguments.image)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
