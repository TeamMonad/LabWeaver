"""Unit coverage for payload-safe NATS credential permission validation."""

from __future__ import annotations

import base64
import importlib.util
import json
from pathlib import Path
import sys
import unittest


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "tools/validate_nats_user_credentials.py"
SPEC = importlib.util.spec_from_file_location("validate_nats_user_credentials", SCRIPT)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("NATS credential validator could not be loaded")
VALIDATOR = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = VALIDATOR
SPEC.loader.exec_module(VALIDATOR)


def credentials(allow: list[str]) -> str:
    header = base64.urlsafe_b64encode(b"{}").decode().rstrip("=")
    payload = base64.urlsafe_b64encode(
        json.dumps({"nats": {"pub": {"allow": allow}}}).encode()
    ).decode().rstrip("=")
    return (
        "-----BEGIN NATS USER JWT-----\n"
        f"{header}.{payload}.signature\n"
        "------END NATS USER JWT------\n"
    )


class NatsCredentialValidationTests(unittest.TestCase):
    def test_bounded_quarantine_wildcard_covers_both_control_subjects(self) -> None:
        result = VALIDATOR.validate(
            credentials(["labweaver.agent.quarantine.>"]),
            [
                "labweaver.agent.quarantine.control_agent_run.v1",
                "labweaver.agent.quarantine.control_agent_build.v1",
            ],
        )
        self.assertEqual(result["requiredSubjects"], 2)

    def test_unrelated_control_permission_is_rejected(self) -> None:
        with self.assertRaisesRegex(
            VALIDATOR.CredentialError,
            "LW_NATS_USER_PUBLISH_PERMISSION_MISSING",
        ):
            VALIDATOR.validate(
                credentials(["labweaver.control.>"]),
                ["labweaver.agent.quarantine.control_agent_run.v1"],
            )

    def test_invalid_credentials_fail_closed(self) -> None:
        with self.assertRaisesRegex(
            VALIDATOR.CredentialError,
            "LW_NATS_USER_CREDENTIAL_INVALID",
        ):
            VALIDATOR.validate("not credentials", ["labweaver.agent.quarantine.test.v1"])


if __name__ == "__main__":
    unittest.main()
