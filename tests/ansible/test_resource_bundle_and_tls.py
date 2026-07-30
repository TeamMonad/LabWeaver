import base64
import json
import os
from pathlib import Path

import pytest

from tools import issue_resource_tls_identity as tls
from tools import render_resource_bundle as bundle


def _manifest_root(tmp_path: Path) -> Path:
    root = tmp_path / ".private" / "input"
    (root / "configmaps" / "resource-service-config").mkdir(parents=True)
    (root / "secrets" / "resource-service-secrets").mkdir(parents=True)
    (root / "configmaps" / "resource-service-config" / "capacity.json").write_text("{}")
    for key in json.loads(Path("deploy/config/resource-bundle-manifest.json").read_text())["secrets"]["resource-service-secrets"]:
        (root / "secrets" / "resource-service-secrets" / key).write_bytes(b"x")
    return root


def test_resource_manifest_rejects_extra_input(tmp_path):
    root = _manifest_root(tmp_path)
    (root / "secrets" / "extra").mkdir()
    with pytest.raises(bundle.BundleError, match="INPUT_INCOMPLETE"):
        bundle.render(Path("deploy/config/resource-bundle-manifest.json"), root, None)


def test_resource_manifest_requires_private_output(tmp_path):
    root = _manifest_root(tmp_path)
    with pytest.raises(bundle.BundleError, match="NATS_CREDENTIALS_INVALID"):
        bundle.render(Path("deploy/config/resource-bundle-manifest.json"), root, None)


def test_tls_private_path_guard(tmp_path):
    with pytest.raises(tls.TlsIssuanceError, match="PRIVATE_PATH_REQUIRED"):
        tls.private_path(tmp_path / "output")


def test_tls_issuer_rejects_existing_output(tmp_path, monkeypatch):
    private = tmp_path / ".private"
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
    monkeypatch.setattr(tls, "secure_ca", lambda _: (authority / "ca.key", authority / "ca.crt"))
    with pytest.raises(tls.TlsIssuanceError, match="OUTPUT_EXISTS"):
        tls.issue(authority, platform, Path("/usr/bin/openssl"), output, 365)
