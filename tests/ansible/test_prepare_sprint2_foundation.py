"""Safety contracts for private Sprint 2 foundation authoring."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "tools/prepare_sprint2_foundation.py"
SPEC = importlib.util.spec_from_file_location("prepare_sprint2_foundation", SCRIPT)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("foundation authoring module could not be loaded")
FOUNDATION = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = FOUNDATION
SPEC.loader.exec_module(FOUNDATION)


class FoundationAuthoringTests(unittest.TestCase):
    def test_output_must_be_new_and_private(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            with self.assertRaisesRegex(
                FOUNDATION.FoundationError,
                "LW_SPRINT2_FOUNDATION_PRIVATE_PATH_REQUIRED",
            ):
                FOUNDATION._private_output(root / "foundation")

            private = root / ".private"
            private.mkdir()
            output = private / "foundation"
            self.assertEqual(FOUNDATION._private_output(output), output.resolve())
            output.mkdir()
            with self.assertRaisesRegex(
                FOUNDATION.FoundationError,
                "LW_SPRINT2_FOUNDATION_OUTPUT_EXISTS",
            ):
                FOUNDATION._private_output(output)

    def test_nats_users_are_separate_and_bounded(self) -> None:
        self.assertEqual(
            set(FOUNDATION.NATS_USERS),
            {
                "control-service",
                "access-service",
                "agent-service",
                "build-executor",
                "environment-service",
                "evaluation-service",
                "container-executor",
                "kubevirt-executor",
            },
        )
        for publish, subscribe, _ in FOUNDATION.NATS_USERS.values():
            self.assertNotIn(">", publish)
            self.assertNotIn(">", subscribe)
            self.assertNotIn("*", publish)
            self.assertNotIn("*", subscribe)


if __name__ == "__main__":
    unittest.main()
