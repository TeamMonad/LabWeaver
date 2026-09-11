"""Safety contracts for private Sprint 2 foundation authoring."""

from __future__ import annotations

import importlib.util
import inspect
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "tools/prepare_platform_foundation.py"
SPEC = importlib.util.spec_from_file_location("prepare_platform_foundation", SCRIPT)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("foundation authoring module could not be loaded")
FOUNDATION = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = FOUNDATION
SPEC.loader.exec_module(FOUNDATION)


class FoundationAuthoringTests(unittest.TestCase):
    def test_child_environment_keeps_runtime_bindings_without_ambient_secrets(self) -> None:
        with patch.dict(
            FOUNDATION.os.environ,
            {
                "PATH": "fixture-path",
                "SystemRoot": "C:\\Windows",
                "WINDIR": "C:\\Windows",
                "TEMP": "C:\\Temp",
                "TMP": "C:\\Temp",
                "PROGRAMDATA": "C:\\ProgramData",
                "OPENSSL_CONF": "C:\\OpenSSL\\openssl.cnf",
                "OPENSSL_MODULES": "C:\\OpenSSL\\modules",
                "LABWEAVER_FIXTURE_SECRET": "must-not-cross-process-boundary",
            },
            clear=True,
        ):
            environment = FOUNDATION._child_environment(Path("C:/private-home"))
        self.assertEqual(environment["PATH"], "fixture-path")
        self.assertEqual(environment["SystemRoot"], "C:\\Windows")
        self.assertEqual(environment["WINDIR"], "C:\\Windows")
        self.assertEqual(environment["TEMP"], "C:\\Temp")
        self.assertEqual(environment["TMP"], "C:\\Temp")
        self.assertEqual(environment["PROGRAMDATA"], "C:\\ProgramData")
        self.assertEqual(environment["OPENSSL_CONF"], "C:\\OpenSSL\\openssl.cnf")
        self.assertEqual(environment["OPENSSL_MODULES"], "C:\\OpenSSL\\modules")
        self.assertNotIn("LABWEAVER_FIXTURE_SECRET", environment)

    def test_failed_tool_keeps_bounded_safe_stderr(self) -> None:
        private_home = Path("C:/private-home")
        failure = subprocess.CalledProcessError(
            1,
            ["openssl"],
            stderr=("password=top-secret C:/private-home/secret.key\x01\n" * 100).encode(),
        )
        with patch.object(FOUNDATION.subprocess, "run", side_effect=failure):
            with self.assertRaises(FOUNDATION.FoundationError) as raised:
                FOUNDATION._run(Path("C:/tools/openssl"), ["genpkey"], private_home)
        diagnostic = str(raised.exception)
        self.assertIn("LW_PLATFORM_FOUNDATION_TOOL_FAILED:openssl:genpkey", diagnostic)
        self.assertNotIn("top-secret", diagnostic)
        self.assertNotIn("C:/private-home", diagnostic)
        self.assertNotIn("\x01", diagnostic)
        self.assertLessEqual(len(diagnostic.rsplit(":", maxsplit=1)[-1]), 1024)

    def test_output_must_be_new_and_private(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            with self.assertRaisesRegex(
                FOUNDATION.FoundationError,
                "LW_PLATFORM_FOUNDATION_PRIVATE_PATH_REQUIRED",
            ):
                FOUNDATION._private_output(root / "foundation")

            private = root / ".private"
            private.mkdir()
            output = private / "foundation"
            self.assertEqual(FOUNDATION._private_output(output), output.resolve())
            output.mkdir()
            with self.assertRaisesRegex(
                FOUNDATION.FoundationError,
                "LW_PLATFORM_FOUNDATION_OUTPUT_EXISTS",
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
                "container-executor",
                "evaluation-service",
                "kubevirt-executor",
                "resource-service",
            },
        )
        for publish, subscribe, _ in FOUNDATION.NATS_USERS.values():
            self.assertNotIn(">", publish)
            self.assertNotIn(">", subscribe)
            self.assertNotIn("*", publish)
            self.assertNotIn("*", subscribe)

        self.assertEqual(FOUNDATION.NATS_ADMIN_TLS_IDENTITY, "platform-admin")
        self.assertEqual(FOUNDATION.NATS_ADMIN_USER, "platform-admin")
        self.assertNotIn(FOUNDATION.NATS_ADMIN_TLS_IDENTITY, FOUNDATION.NATS_USERS)
        self.assertEqual(FOUNDATION.NATS_ADMIN_PUBLISH, ("$JS.API.>", "$JS.ACK.>"))
        self.assertEqual(FOUNDATION.NATS_ADMIN_SUBSCRIBE, ("_INBOX.>",))

        control_publish, _, _ = FOUNDATION.NATS_USERS["control-service"]
        self.assertIn("labweaver.agent.quarantine.>", control_publish)

        access_publish, access_subscribe, access_response = FOUNDATION.NATS_USERS["access-service"]
        self.assertEqual(access_publish, ("$JS.API.>", "$JS.ACK.>", "labweaver.access.>"))
        self.assertEqual(
            access_subscribe,
            (
                "_INBOX.>",
                "labweaver.service.access.revoke.v1",
                "labweaver.environment.instance.state_changed.v1",
            ),
        )
        self.assertTrue(access_response)
        self.assertFalse(access_subscribe[0].startswith("labweaver.access."))

        environment_publish, _, _ = FOUNDATION.NATS_USERS["environment-service"]
        self.assertIn("labweaver.service.access.revoke.v1", environment_publish)
        self.assertIn("labweaver.resource.lease.verify.v1", environment_publish)

        evaluation_publish, evaluation_subscribe, evaluation_response = FOUNDATION.NATS_USERS[
            "evaluation-service"
        ]
        self.assertEqual(
            evaluation_publish,
            (
                "$JS.API.>",
                "$JS.ACK.>",
                "labweaver.evaluation.submission.freeze_requested.v1",
                "labweaver.evaluation.submission.frozen.v1",
                "labweaver.evaluation.release.published.v1",
                "labweaver.evaluation.run.requested.v1",
                "labweaver.evaluation.run.state_changed.v1",
                "labweaver.evaluation.step_run.state_changed.v1",
            ),
        )
        self.assertEqual(
            evaluation_subscribe,
            ("_INBOX.>", "labweaver.evaluation.submission.freeze_requested.v1"),
        )
        self.assertFalse(evaluation_response)

        for consumer in (
            "control-service",
            "agent-service",
            "environment-service",
            "evaluation-service",
        ):
            publish, _, _ = FOUNDATION.NATS_USERS[consumer]
            self.assertIn("$JS.ACK.>", publish)
        for non_consumer in (
            "build-executor",
            "container-executor",
            "kubevirt-executor",
        ):
            publish, _, _ = FOUNDATION.NATS_USERS[non_consumer]
            self.assertNotIn("$JS.ACK.>", publish)

        resource_publish, resource_subscribe, resource_response = FOUNDATION.NATS_USERS[
            "resource-service"
        ]
        self.assertEqual(
            resource_publish,
            (
                "$JS.API.>",
                "$JS.ACK.>",
                "labweaver.resource.request.submitted.v1",
                "labweaver.resource.request.approved.v1",
                "labweaver.resource.request.rejected.v1",
                "labweaver.resource.request.cancelled.v1",
                "labweaver.resource.request.state_changed.v1",
                "labweaver.resource.lease.activated.v1",
                "labweaver.resource.lease.renewed.v1",
                "labweaver.resource.lease.revoked.v1",
                "labweaver.resource.lease.expiring.v1",
                "labweaver.resource.lease.expired.v1",
            ),
        )
        self.assertEqual(resource_subscribe, ("_INBOX.>", "labweaver.resource.lease.verify.v1"))
        self.assertTrue(resource_response)

    def test_reused_workloads_seed_requires_private_account_key_locator(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            public_key = "A" + "A" * 55
            source = root / f"{public_key}.nk"
            source.write_text("test-only", encoding="utf-8")
            source.chmod(0o600)
            resolved, observed_public_key = FOUNDATION._workloads_seed_source(
                source.resolve()
            )
            self.assertEqual(resolved, source.resolve())
            self.assertEqual(observed_public_key, public_key)

            if FOUNDATION.os.name != "nt":
                source.chmod(0o640)
                with self.assertRaisesRegex(
                    FOUNDATION.FoundationError,
                    "LW_PLATFORM_FOUNDATION_WORKLOADS_SEED_INVALID",
                ):
                    FOUNDATION._workloads_seed_source(source.resolve())

    def test_reused_workloads_seed_must_be_account_key(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            source = Path(temporary) / f"O{'A' * 55}.nk"
            source.write_text("test-only", encoding="utf-8")
            source.chmod(0o600)
            with self.assertRaisesRegex(
                FOUNDATION.FoundationError,
                "LW_PLATFORM_FOUNDATION_WORKLOADS_SEED_INVALID",
            ):
                FOUNDATION._workloads_seed_source(source.resolve())

    def test_workloads_account_has_bounded_jetstream_limits(self) -> None:
        limits = FOUNDATION.NATS_ACCOUNT_JETSTREAM_LIMITS
        self.assertEqual(
            limits,
            (
                "--js-disk-storage",
                "8G",
                "--js-mem-storage",
                "64M",
                "--js-streams",
                "16",
                "--js-consumer",
                "64",
                "--js-max-ack-pending",
                "4096",
            ),
        )

    def test_platform_identities_have_exact_service_and_client_boundaries(self) -> None:
        identities = FOUNDATION.PLATFORM_IDENTITIES
        self.assertEqual(
            set(identities),
            {
                "control-service",
                "access-service",
                "agent-service",
                "environment-service",
                "container-executor",
                "kubevirt-console-executor",
                "evaluation-service",
                "resource-service",
                "openssh-gateway",
            },
        )
        self.assertEqual(identities["openssh-gateway"][1], "clientAuth")
        self.assertIn("URI:spiffe://labweaver/access-service", identities["access-service"][0])
        self.assertIn("URI:spiffe://labweaver/control-service", identities["control-service"][0])
        self.assertIn("URI:spiffe://labweaver/agent-service", identities["agent-service"][0])
        self.assertEqual(identities["agent-service"][1], "serverAuth,clientAuth")
        self.assertIn(
            "URI:spiffe://labweaver/environment-service",
            identities["environment-service"][0],
        )
        self.assertEqual(
            identities["environment-service"][1], "serverAuth,clientAuth"
        )
        self.assertIn(
            "DNS:container-executor.labweaver-system.svc",
            identities["container-executor"][0],
        )
        self.assertEqual(
            identities["container-executor"][1], "serverAuth,clientAuth"
        )
        self.assertIn(
            "DNS:kubevirt-console-executor.labweaver-system.svc",
            identities["kubevirt-console-executor"][0],
        )
        self.assertEqual(
            identities["kubevirt-console-executor"][1], "serverAuth"
        )

    def test_certificate_authoring_activates_san_extension_section(self) -> None:
        source = inspect.getsource(FOUNDATION._certificate)
        self.assertIn('"[v3_req]\\n"', source)
        self.assertIn('"-extensions", "v3_req"', source)


if __name__ == "__main__":
    unittest.main()
