"""Focused contracts for the real local Harbor and BuildKit provider."""

from __future__ import annotations

import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import socket
import ssl
import subprocess
import sys
import tempfile
import threading
import unittest
from unittest.mock import patch

import yaml


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "tools/local_dev_build.py"
SPEC = importlib.util.spec_from_file_location("local_dev_build", SCRIPT)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("local real-build provider could not be loaded")
LOCAL_BUILD = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = LOCAL_BUILD
SPEC.loader.exec_module(LOCAL_BUILD)


class LocalDevBuildTests(unittest.TestCase):
    def test_chart_archive_reuses_only_the_locked_digest(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            work = root / ".private" / "run"
            archive = root / "harbor.tgz"
            archive.write_bytes(b"locked chart")
            expected = hashlib.sha256(archive.read_bytes()).hexdigest()
            lock = {"harbor_chart": "1.19.1", "harbor_chart_sha256": expected}
            with patch.object(LOCAL_BUILD, "_locked_versions", return_value=lock):
                self.assertEqual(LOCAL_BUILD.ensure_harbor_chart(work, archive), archive.resolve())
                archive.write_bytes(b"changed chart")
                with self.assertRaisesRegex(LOCAL_BUILD.LocalBuildProviderError, "LW_LOCAL_BUILD_CHART_HASH_MISMATCH"):
                    LOCAL_BUILD.ensure_harbor_chart(work, archive)

    def test_harbor_values_keep_pinned_images_and_local_limits(self) -> None:
        lock = LOCAL_BUILD._locked_versions()
        values = LOCAL_BUILD._harbor_values(
            lock,
            {"admin": "admin-secret", "database": "database-secret", "redis": "redis-secret"},
        )
        self.assertEqual(values["externalURL"], "https://harbor.lab.lan")
        self.assertTrue(values["trivy"]["enabled"])
        self.assertEqual(
            values["nginx"]["image"]["tag"],
            f"{lock['harbor_app']}@{lock['harbor_images']['nginx']}",
        )
        self.assertEqual(values["persistence"]["persistentVolumeClaim"]["registry"]["size"], "4Gi")
        self.assertEqual(
            values["database"]["internal"]["initContainer"]["permissions"]["resources"]["limits"]["memory"],
            "256Mi",
        )

    def test_harbor_namespace_quota_is_bounded_for_local_storage(self) -> None:
        quota = LOCAL_BUILD._render_harbor_quota()
        self.assertEqual(quota["metadata"]["namespace"], "harbor")
        self.assertEqual(quota["spec"]["hard"]["requests.storage"], "12Gi")

    def test_kindnet_policy_boundary_is_reported_as_unenforced(self) -> None:
        self.assertEqual(
            LOCAL_BUILD.BUILDKIT_NETWORK_POLICY_MODE,
            "kindnet-network-policy-unenforced;cilium-unavailable",
        )

    def test_buildkit_objects_hash_the_applied_bundle_and_keep_kubernetes_policy(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / ".private" / "buildkit"
            config_dir = output / "render-input" / "configmaps" / "buildkit-config"
            secret_dir = output / "render-input" / "secrets" / "buildkit-server-secrets"
            config_dir.mkdir(parents=True)
            secret_dir.mkdir(parents=True)
            (config_dir / "buildkitd.toml").write_text("rootless = true\n", encoding="utf-8")
            for name in ("ca.crt", "health.crt", "health.key", "registry-ca.crt", "tls.crt", "tls.key"):
                (secret_dir / name).write_bytes(name.encode())
            objects = LOCAL_BUILD._buildkit_objects(
                buildkit_output=output,
                lock={"buildkit_image": "docker.io/moby/buildkit:v0.31.1-rootless@sha256:" + "a" * 64},
                dns_nameserver="10.96.0.10",
                harbor_service_ip="10.96.0.20",
            )
            self.assertEqual(objects[0]["kind"], "ConfigMap")
            self.assertEqual(objects[1]["kind"], "Secret")
            self.assertTrue(all(obj.get("kind") != "CiliumNetworkPolicy" for obj in objects))
            bundle_hash = hashlib.sha256(yaml.safe_dump_all(objects[:2], sort_keys=False).encode("utf-8")).hexdigest()
            deployment = next(obj for obj in objects if obj.get("kind") == "Deployment")
            self.assertEqual(
                deployment["spec"]["template"]["metadata"]["annotations"]["labweaver.io/configuration-sha256"],
                bundle_hash,
            )
            network_policy = next(obj for obj in objects if obj.get("kind") == "NetworkPolicy" and obj["metadata"]["name"] == "buildkit")
            self.assertTrue(network_policy["spec"]["ingress"])

    def test_robot_credentials_use_harbor_returned_tokens(self) -> None:
        project = {"project_id": 7, "storage_limit": LOCAL_BUILD.PROJECT_QUOTA_BYTES, "metadata": {"public": "false"}}
        responses = [
            [],
            None,
            [project],
            {"name": "robot$labweaver-system+platform-build-executor", "secret": "builder-secret"},
            {"name": "robot$labweaver-system+runtime-puller", "secret": "runtime-secret"},
        ]
        with patch.object(LOCAL_BUILD, "_harbor_json", side_effect=responses) as request:
            result = LOCAL_BUILD._create_project_and_robots(
                port=1,
                ca_file=Path("ca.crt"),
                admin_password="admin-secret",
            )
        self.assertEqual(
            result,
            (
                "7",
                "robot$labweaver-system+platform-build-executor",
                "builder-secret",
                "robot$labweaver-system+runtime-puller",
                "runtime-secret",
            ),
        )
        robot_calls = [call for call in request.call_args_list if "/robots" in call.args[5]]
        self.assertEqual([call.args[6]["name"] for call in robot_calls], ["platform-build-executor", "runtime-puller"])
        self.assertEqual(
            robot_calls[0].args[6]["permissions"][0]["access"],
            [
                {"resource": "repository", "action": "push"},
                {"resource": "repository", "action": "pull"},
                {"resource": "artifact", "action": "read"},
                {"resource": "tag", "action": "delete"},
            ],
        )

    def test_repeated_bootstrap_recreates_builder_with_tag_delete_only(self) -> None:
        project = {"project_id": 7, "storage_limit": LOCAL_BUILD.PROJECT_QUOTA_BYTES, "metadata": {"public": "false"}}
        responses = [
            [], None, [project],
            {"name": "robot$labweaver-system+platform-build-executor", "secret": "builder-secret-1"},
            {"name": "robot$labweaver-system+runtime-puller", "secret": "runtime-secret-1"},
            [project],
            {"name": "robot$labweaver-system+platform-build-executor-2", "secret": "builder-secret-2"},
            {"name": "robot$labweaver-system+runtime-puller-2", "secret": "runtime-secret-2"},
        ]
        with patch.object(LOCAL_BUILD, "_harbor_json", side_effect=responses) as request:
            LOCAL_BUILD._create_project_and_robots(port=1, ca_file=Path("ca.crt"), admin_password="admin-secret")
            LOCAL_BUILD._create_project_and_robots(port=1, ca_file=Path("ca.crt"), admin_password="admin-secret")

        robot_calls = [call for call in request.call_args_list if "/robots" in call.args[5]]
        self.assertEqual(len(robot_calls), 4)
        for call in robot_calls[::2]:
            self.assertEqual(
                call.args[6]["permissions"][0]["access"],
                [
                    {"resource": "repository", "action": "push"},
                    {"resource": "repository", "action": "pull"},
                    {"resource": "artifact", "action": "read"},
                    {"resource": "tag", "action": "delete"},
                ],
            )

    def test_invalid_harbor_project_metadata_fails_closed(self) -> None:
        project = {"project_id": 7, "storage_limit": LOCAL_BUILD.PROJECT_QUOTA_BYTES, "metadata": []}
        with patch.object(LOCAL_BUILD, "_harbor_json", side_effect=[[project]]):
            with self.assertRaisesRegex(
                LOCAL_BUILD.LocalBuildProviderError,
                "LW_LOCAL_BUILD_HARBOR_PROJECT_INVALID",
            ):
                LOCAL_BUILD._create_project_and_robots(
                    port=1,
                    ca_file=Path("ca.crt"),
                    admin_password="admin-secret",
                )


class HarborTlsTests(unittest.TestCase):
    """Exercise the port-forward client with certificate and hostname checks."""

    @classmethod
    def setUpClass(cls) -> None:
        cls.openssl = shutil.which("openssl")
        if cls.openssl is None:
            raise unittest.SkipTest("openssl is required for the Harbor TLS contract")

    def _run_openssl(self, *arguments: str) -> None:
        subprocess.run([self.openssl, *arguments], check=True, capture_output=True)

    def _certificate_bundle(self, root: Path, hostname: str) -> tuple[Path, Path, Path]:
        root.mkdir(parents=True, exist_ok=True)
        ca_key = root / "ca.key"
        ca_crt = root / "ca.crt"
        server_key = root / "server.key"
        server_csr = root / "server.csr"
        server_crt = root / "server.crt"
        extension = root / "server.ext"
        self._run_openssl("genpkey", "-algorithm", "RSA", "-pkeyopt", "rsa_keygen_bits:2048", "-out", str(ca_key))
        self._run_openssl(
            "req", "-x509", "-new", "-key", str(ca_key), "-sha256", "-days", "1",
            "-subj", "/CN=LabWeaver test CA",
            "-addext", "basicConstraints=critical,CA:true",
            "-addext", "keyUsage=critical,keyCertSign,cRLSign",
            "-addext", "subjectKeyIdentifier=hash",
            "-addext", "authorityKeyIdentifier=keyid:always,issuer",
            "-out", str(ca_crt),
        )
        self._run_openssl("genpkey", "-algorithm", "RSA", "-pkeyopt", "rsa_keygen_bits:2048", "-out", str(server_key))
        self._run_openssl("req", "-new", "-key", str(server_key), "-subj", f"/CN={hostname}", "-out", str(server_csr))
        extension.write_text(
            "[v3_req]\n"
            "basicConstraints=critical,CA:FALSE\n"
            "keyUsage=critical,digitalSignature,keyEncipherment\n"
            "extendedKeyUsage=serverAuth\n"
            f"subjectAltName=DNS:{hostname}\n"
            "subjectKeyIdentifier=hash\n"
            "authorityKeyIdentifier=keyid,issuer\n",
            encoding="utf-8",
        )
        self._run_openssl(
            "x509", "-req", "-in", str(server_csr), "-CA", str(ca_crt), "-CAkey", str(ca_key),
            "-CAcreateserial", "-days", "1", "-sha256", "-extfile", str(extension),
            "-extensions", "v3_req", "-out", str(server_crt),
        )
        return ca_crt, server_crt, server_key

    def _serve_once(self, certificate: Path, key: Path) -> int:
        listener = socket.socket()
        listener.bind(("127.0.0.1", 0))
        listener.listen(1)
        port = listener.getsockname()[1]

        def serve() -> None:
            try:
                connection, _ = listener.accept()
                context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
                context.load_cert_chain(certificate, key)
                try:
                    with context.wrap_socket(connection, server_side=True) as tls:
                        tls.recv(65536)
                        tls.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                except (ssl.SSLError, OSError):
                    # Expected for the wrong-CA contract.
                    connection.close()
            finally:
                listener.close()

        threading.Thread(target=serve, daemon=True).start()
        return port

    def test_right_ca_and_san_are_required(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            ca, certificate, key = self._certificate_bundle(root, LOCAL_BUILD.HARBOR_HOST)
            port = self._serve_once(certificate, key)
            status, content = LOCAL_BUILD._harbor_request(port, ca, "admin", "secret", "GET", "/api/v2.0/systeminfo")
            self.assertEqual((status, content), (200, b"{}"))

            wrong_ca, _, _ = self._certificate_bundle(root / "wrong", "wrong-ca.lan")
            port = self._serve_once(certificate, key)
            with self.assertRaisesRegex(LOCAL_BUILD.LocalBuildProviderError, "LW_LOCAL_BUILD_HARBOR_TLS_FAILED"):
                LOCAL_BUILD._harbor_request(port, wrong_ca, "admin", "secret", "GET", "/api/v2.0/systeminfo")

            wrong_san_root = root / "wrong-san"
            _, wrong_certificate, wrong_key = self._certificate_bundle(wrong_san_root, "wrong.lan")
            # Re-issue the wrong-name leaf from the trusted CA so the next
            # failure proves SAN validation rather than CA validation.
            self._run_openssl(
                "x509", "-req", "-in", str(wrong_san_root / "server.csr"),
                "-CA", str(ca), "-CAkey", str(root / "ca.key"), "-CAcreateserial", "-days", "1",
                "-sha256", "-extfile", str(wrong_san_root / "server.ext"), "-extensions", "v3_req",
                "-out", str(wrong_certificate),
            )
            port = self._serve_once(wrong_certificate, wrong_key)
            with self.assertRaisesRegex(LOCAL_BUILD.LocalBuildProviderError, "LW_LOCAL_BUILD_HARBOR_TLS_FAILED"):
                LOCAL_BUILD._harbor_request(port, ca, "admin", "secret", "GET", "/api/v2.0/systeminfo")


if __name__ == "__main__":
    unittest.main()
