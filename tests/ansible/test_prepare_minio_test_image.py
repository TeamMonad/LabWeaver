"""Checks for the pinned source-built MinIO test image preparation entrypoint."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "prepare_minio_test_image", ROOT / "tools/prepare_minio_test_image.py"
)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class PrepareMinioTestImageTests(unittest.TestCase):
    def test_build_command_pins_official_source_and_base_images(self) -> None:
        lock = MODULE.load_lock(ROOT)
        image = MODULE.default_image(ROOT)
        command = MODULE.build_command(ROOT, image)

        self.assertEqual(command[:4], ["docker", "buildx", "build", "--file"])
        self.assertIn("containers/Containerfile.minio-source", command)
        self.assertIn(f"MINIO_RELEASE={lock['release']}", command)
        self.assertIn(f"MINIO_SOURCE_COMMIT={lock['source_commit']}", command)
        self.assertIn(f"MINIO_GO_IMAGE={lock['builder_image']}", command)
        self.assertIn(f"MINIO_RUNTIME_IMAGE={lock['runtime_image']}", command)
        self.assertIn("--load", command)
        self.assertEqual(command[-3:-1], ["--tag", image])

    def test_containerfile_uses_the_same_pinned_source(self) -> None:
        containerfile = (ROOT / "containers/Containerfile.minio-source").read_text(
            encoding="utf-8"
        )

        self.assertIn("github.com/minio/minio.git", containerfile)
        self.assertIn('git fetch --depth=1 origin "${MINIO_SOURCE_COMMIT}"', containerfile)
        self.assertIn("go build -tags kqueue -trimpath", containerfile)
        self.assertIn('ENTRYPOINT ["/usr/bin/minio"]', containerfile)


if __name__ == "__main__":
    unittest.main()
