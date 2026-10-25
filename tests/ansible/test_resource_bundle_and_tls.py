import base64
import json
import os
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from tools import issue_resource_tls_identity as tls
from tools import render_resource_bundle as bundle


def _manifest_root(tmp_path: Path) -> Path:
    root = tmp_path / ".private" / "input"
    (root / "configmaps" / "resource-service-config").mkdir(parents=True)
    (root / "secrets" / "resource-service-secrets").mkdir(parents=True)
    (root / "configmaps" / "resource-service-config" / "capacity.json").write_text("{}")
    (root / "configmaps" / "resource-service-config" / "http.yaml").write_text(
        "bind_addr: 0.0.0.0:9448\n"
        "server_certificate_file: /etc/labweaver/secrets/tls.crt\n"
        "server_key_file: /etc/labweaver/secrets/tls.key\n"
        "delegation_key_file: /etc/labweaver/secrets/resource-delegation-key\n"
    )
    for key in json.loads(Path("deploy/config/resource-bundle-manifest.json").read_text())["secrets"]["resource-service-secrets"]:
        (root / "secrets" / "resource-service-secrets" / key).write_bytes(b"x")
    return root


class ResourceBundleAndTlsTests(unittest.TestCase):
    def test_bundle_permission_contract_matches_resource_identity(self) -> None:
        from tools import prepare_platform_foundation as foundation

        expected = foundation.NATS_USERS["resource-service"]
        self.assertEqual(
            bundle.RESOURCE_PUBLISH_SUBJECTS,
            expected[0],
        )
        self.assertEqual(
            bundle.RESOURCE_SUBSCRIBE_SUBJECTS,
            expected[1],
        )
        self.assertEqual(
            bundle.RESOURCE_RESPONSE_PERMISSION,
            expected[2],
        )

    def test_created_and_fenced_permissions_are_required_without_expanding_other_subjects(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            original = list(bundle.RESOURCE_PUBLISH_SUBJECTS)
            for missing in (None, "labweaver.resource.lease.created.v1", "labweaver.resource.lease.fenced.v1", "extra"):
                publishes = [subject for subject in original if subject != missing]
                if missing == "extra":
                    publishes.append("labweaver.resource.unreviewed.v1")
                claims = {"iss": "fixture-account", "nats": {"pub": {"allow": publishes}, "sub": {"allow": list(bundle.RESOURCE_SUBSCRIBE_SUBJECTS)}, "resp": {"max": 1}}}
                payload = base64.urlsafe_b64encode(json.dumps(claims).encode()).rstrip(b"=")
                (directory / "nats.creds").write_bytes(b"-----BEGIN NATS USER JWT-----\na." + payload + b".signature\n------END NATS USER JWT------\n")
                if missing is None:
                    bundle.validate_creds(directory, "fixture-account")
                else:
                    with self.subTest(missing=missing), self.assertRaisesRegex(bundle.BundleError, "LW_RESOURCE_NATS_PERMISSIONS_INVALID"):
                        bundle.validate_creds(directory, "fixture-account")

    def test_resource_manifest_rejects_extra_input(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = _manifest_root(Path(temporary))
            (root / "secrets" / "extra").mkdir()
            with self.assertRaisesRegex(bundle.BundleError, "INPUT_INCOMPLETE"):
                bundle.render(
                    Path("deploy/config/resource-bundle-manifest.json"), root, None
                )

    def test_resource_manifest_requires_private_output(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = _manifest_root(Path(temporary))
            with self.assertRaisesRegex(
                bundle.BundleError, "NATS_CREDENTIALS_INVALID"
            ):
                bundle.render(
                    Path("deploy/config/resource-bundle-manifest.json"), root, None
                )

    def test_tls_private_path_guard(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            with self.assertRaisesRegex(
                tls.TlsIssuanceError, "PRIVATE_PATH_REQUIRED"
            ):
                tls.private_path(Path(temporary) / "output")

    def test_tls_issuer_rejects_existing_output(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            private = Path(temporary) / ".private"
            authority = private / "authority"
            platform = private / "platform"
            authority.mkdir(parents=True)
            platform.mkdir()
            for directory in (authority, platform):
                (directory / "ca.key").write_text("key")
                (directory / "ca.crt").write_text("cert")
                os.chmod(directory, 0o700)
                os.chmod(directory / "ca.key", 0o600)
                os.chmod(directory / "ca.crt", 0o600)
            output = private / "out"
            output.mkdir()
            with mock.patch.object(
                tls,
                "secure_ca",
                return_value=(authority / "ca.key", authority / "ca.crt"),
            ):
                with self.assertRaisesRegex(tls.TlsIssuanceError, "OUTPUT_EXISTS"):
                    tls.issue(
                        authority, platform, Path("/usr/bin/openssl"), output, 365
                    )


if __name__ == "__main__":
    unittest.main()
