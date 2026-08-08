"""Safety contracts for private Sprint 2 foundation authoring."""

from __future__ import annotations

import importlib.util
import inspect
from pathlib import Path
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "tools/prepare_platform_foundation.py"
SPEC = importlib.util.spec_from_file_location("prepare_platform_foundation", SCRIPT)
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
