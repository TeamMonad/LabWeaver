"""Safety contracts for complete NATS authority rotation."""

from __future__ import annotations

import base64
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest

import yaml


ROOT = Path(__file__).resolve().parents[2]
TOOLS = ROOT / "tools"
sys.path.insert(0, str(TOOLS))
SCRIPT = TOOLS / "prepare_nats_authority_rotation.py"
SPEC = importlib.util.spec_from_file_location(
    "prepare_nats_authority_rotation", SCRIPT
)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("NATS rotation module could not be loaded")
ROTATION = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = ROTATION
SPEC.loader.exec_module(ROTATION)


def token(claims: dict) -> str:
    def encoded(value: dict) -> str:
        return base64.urlsafe_b64encode(
            json.dumps(value, separators=(",", ":")).encode()
        ).decode().rstrip("=")

    return f"{encoded({'alg': 'ed25519', 'typ': 'JWT'})}.{encoded(claims)}.sig"


def credentials(claims: dict) -> bytes:
    return (
        "-----BEGIN NATS USER JWT-----\n"
        f"{token(claims)}\n"
        "------END NATS USER JWT------\n"
        "-----BEGIN USER NKEY SEED-----\n"
        "test-only\n"
        "------END USER NKEY SEED------\n"
    ).encode()


class NatsAuthorityRotationTests(unittest.TestCase):
    def test_access_rotation_adds_canonical_platform_admin_alias(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            example = root / "access-auth.example.yaml"
            example.write_text(
                "resource_gateway:\n"
                "  base_uri: https://resource-service:9448/\n",
                encoding="utf-8",
            )
            application = [
                {
                    "apiVersion": "v1",
                    "kind": "ConfigMap",
                    "metadata": {"name": "access-service-config"},
                    "data": {
                        "config.yaml": yaml.safe_dump(
                            {
                                "oidc": {
                                    "role_mappings": {
                                        "platform-admin": "platform_admin"
                                    }
                                }
                            }
                        )
                    },
                }
            ]
            example.chmod(0o600)
            ROTATION._ensure_access_resource_gateway(application, example)
            config = yaml.safe_load(application[0]["data"]["config.yaml"])
            self.assertEqual(
                config["oidc"]["role_mappings"]["platform_admin"],
                "platform_admin",
            )

    def test_access_rotation_rejects_conflicting_platform_admin_alias(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            example = root / "access-auth.example.yaml"
            example.write_text(
                "resource_gateway:\n"
                "  base_uri: https://resource-service:9448/\n",
                encoding="utf-8",
            )
            application = [
                {
                    "apiVersion": "v1",
                    "kind": "ConfigMap",
                    "metadata": {"name": "access-service-config"},
                    "data": {
                        "config.yaml": yaml.safe_dump(
                            {
                                "oidc": {
                                    "role_mappings": {
                                        "platform-admin": "platform_admin",
                                        "platform_admin": "teacher",
                                    }
                                }
                            }
                        )
                    },
                }
            ]
            example.chmod(0o600)
            with self.assertRaisesRegex(
                ROTATION.RotationError, "LW_NATS_ROTATION_CONTRACT_INVALID"
            ):
                ROTATION._ensure_access_resource_gateway(application, example)

    def test_rotation_preserves_workloads_account_and_replaces_every_client(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            private = Path(temporary) / ".private"
            private.mkdir()
            authority = private / "authority"
            clients = authority / "nats-clients"
            platform_identities = authority / "platform-identities"
            server_config = (
                authority / "render-input/configmaps/nats-config"
            )
            server_secret = (
                authority / "render-input/secrets/nats-server-secrets"
            )
            server_config.mkdir(parents=True)
            server_secret.mkdir(parents=True)

            account = "A" + "D" * 55
            operator = "O" + "C" * 55
            system = "A" + "S" * 55
            config = "\n".join(
                [
                    "operator: " + token(
                        {
                            "name": "LABWEAVER",
                            "sub": operator,
                            "nats": {"type": "operator"},
                        }
                    ),
                    "resolver_preload: {",
                    "  " + system + ": " + token(
                        {
                            "name": "SYS",
                            "sub": system,
                            "nats": {"type": "account"},
                        }
                    ),
                    "  " + account + ": " + token(
                        {
                            "name": "WORKLOADS",
                            "sub": account,
                            "nats": {"type": "account"},
                        }
                    ),
                    "}",
                ]
            )
            (server_config / "nats-server.conf").write_text(
                config, encoding="utf-8"
            )
            for filename in ("ca.crt", "tls.crt", "tls.key"):
                (server_secret / filename).write_bytes(
                    f"new-{filename}".encode()
                )

            identities = {
                **ROTATION.APPLICATION_IDENTITIES,
                "resource-service": "resource-service-secrets",
                ROTATION.NATS_ADMIN_USER: "unused",
            }
            for index, identity in enumerate(identities):
                directory = clients / identity
                directory.mkdir(parents=True)
                if identity == ROTATION.NATS_ADMIN_USER:
                    publish = ROTATION.NATS_ADMIN_PUBLISH
                    subscribe = ROTATION.NATS_ADMIN_SUBSCRIBE
                    response = False
                else:
                    publish, subscribe, response = ROTATION.NATS_USERS[
                        identity
                    ]
                nats = {
                    "pub": {"allow": list(publish)},
                    "sub": {"allow": list(subscribe)},
                }
                if response:
                    nats["resp"] = {"max": 1}
                (directory / "nats.creds").write_bytes(
                    credentials(
                        {
                            "iss": account,
                            "sub": "U" + f"{index:055d}",
                            "nats": nats,
                        }
                    )
                )
                for filename in (
                    "nats-ca.pem",
                    "nats-client.crt",
                    "nats-client.key",
                ):
                    (directory / filename).write_bytes(
                        f"{identity}-{filename}".encode()
                    )

            for identity in ROTATION.PLATFORM_ROTATION_IDENTITIES:
                directory = platform_identities / identity
                directory.mkdir(parents=True)
                for filename in ROTATION.PLATFORM_SECRET_KEYS.values():
                    (directory / filename).write_bytes(
                        f"{identity}-{filename}".encode()
                    )

            foundation = private / "foundation.yaml"
            foundation.write_text(
                yaml.safe_dump_all(
                    [
                        {
                            "apiVersion": "v1",
                            "kind": "ConfigMap",
                            "metadata": {"name": "nats-config"},
                            "data": {"nats-server.conf": "old"},
                        },
                        {
                            "apiVersion": "v1",
                            "kind": "Secret",
                            "metadata": {"name": "nats-server-secrets"},
                            "data": {
                                "ca.crt": "old",
                                "tls.crt": "old",
                                "tls.key": "old",
                            },
                        },
                    ],
                    explicit_start=True,
                ),
                encoding="utf-8",
            )
            application = private / "application.yaml"
            application.write_text(
                yaml.safe_dump_all(
                    [
                        {
                            "apiVersion": "v1",
                            "kind": "Secret",
                            "metadata": {"name": secret_name},
                            "data": {
                                **{
                                    key: "old"
                                    for key in ROTATION.NATS_SECRET_KEYS
                                },
                                **{
                                    key: "old-platform"
                                    for key in ROTATION.PLATFORM_SECRET_KEYS
                                },
                                "unrelated": "preserved",
                            },
                        }
                        for secret_name in ROTATION.APPLICATION_IDENTITIES.values()
                    ],
                    explicit_start=True,
                ),
                encoding="utf-8",
            )
            resource = private / "resource.yaml"
            resource.write_text(
                yaml.safe_dump_all(
                    [
                        {
                            "apiVersion": "v1",
                            "kind": "Secret",
                            "metadata": {
                                "name": "resource-service-secrets"
                            },
                            "data": {
                                **{
                                    key: "old"
                                    for key in ROTATION.NATS_SECRET_KEYS
                                },
                                "database-url": "preserved",
                            },
                        }
                    ],
                    explicit_start=True,
                ),
                encoding="utf-8",
            )
            for path in (authority, foundation, application, resource):
                path.chmod(0o700 if path.is_dir() else 0o600)

            output = private / "rotation"
            record = ROTATION.prepare(
                foundation.resolve(),
                application.resolve(),
                resource.resolve(),
                authority.resolve(),
                output.resolve(),
            )

            self.assertEqual(record["operator_public"], operator)
            self.assertEqual(record["workloads_account_public"], account)
            self.assertEqual(len(record["identities"]), 10)
            if ROTATION.os.name != "nt":
                self.assertEqual(output.stat().st_mode & 0o777, 0o700)
                self.assertEqual(
                    (output / "rotation-record.json").stat().st_mode
                    & 0o777,
                    0o600,
                )
            rendered = list(
                yaml.safe_load_all(
                    (output / "application-bundle.yaml").read_text()
                )
            )
            for document in rendered:
                self.assertEqual(document["data"]["unrelated"], "preserved")
                self.assertNotEqual(document["data"]["nats.creds"], "old")
                if document["metadata"]["name"] in ROTATION.PLATFORM_ROTATION_IDENTITIES.values():
                    self.assertNotEqual(document["data"]["tls.crt"], "old-platform")

    def test_rotation_refuses_non_private_output(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            with self.assertRaisesRegex(
                ROTATION.RotationError,
                "LW_NATS_ROTATION_PRIVATE_OUTPUT_INVALID",
            ):
                ROTATION._new_private_output(
                    (Path(temporary) / "rotation").resolve()
                )


if __name__ == "__main__":
    unittest.main()
