#!/usr/bin/env python3
"""Create private rootless BuildKit mTLS and reviewed bundle inputs."""

from __future__ import annotations

import argparse
import hashlib
import ipaddress
import json
import os
import re
import shutil
import stat
import subprocess
import sys
from pathlib import Path


class BuildkitAuthoringError(Exception):
    """Stable fail-closed BuildKit authoring diagnostic."""


def _private_output(path: Path) -> Path:
    resolved = path.resolve()
    if not any(part in {".private", "private"} for part in resolved.parts):
        raise BuildkitAuthoringError("LW_PLATFORM_BUILDKIT_PRIVATE_PATH_REQUIRED")
    if resolved.exists():
        raise BuildkitAuthoringError("LW_PLATFORM_BUILDKIT_OUTPUT_EXISTS")
    if not resolved.parent.is_dir():
        raise BuildkitAuthoringError("LW_PLATFORM_BUILDKIT_PARENT_MISSING")
    return resolved


def _trusted_openssl(path: Path) -> Path:
    if not path.is_absolute():
        raise BuildkitAuthoringError("LW_PLATFORM_BUILDKIT_OPENSSL_INVALID")
    try:
        resolved = path.resolve(strict=True)
        mode = resolved.stat().st_mode
    except OSError as error:
        raise BuildkitAuthoringError("LW_PLATFORM_BUILDKIT_OPENSSL_INVALID") from error
    if not resolved.is_file() or resolved.name != "openssl" or mode & (stat.S_IWGRP | stat.S_IWOTH):
        raise BuildkitAuthoringError("LW_PLATFORM_BUILDKIT_OPENSSL_INVALID")
    return resolved


def _write(path: Path, payload: bytes, mode: int = 0o600) -> None:
    path.parent.mkdir(parents=True, mode=0o700, exist_ok=True)
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, mode)
    with os.fdopen(descriptor, "wb") as handle:
        handle.write(payload)
        handle.flush()
        os.fsync(handle.fileno())


def _run(openssl: Path, arguments: list[str], private_home: Path) -> None:
    environment = {
        "HOME": str(private_home),
        "LANG": "C.UTF-8",
        "LC_ALL": "C.UTF-8",
        "PATH": "/usr/bin:/bin",
    }
    try:
        subprocess.run(
            [str(openssl), *arguments],
            check=True,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            env=environment,
            timeout=60,
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise BuildkitAuthoringError("LW_PLATFORM_BUILDKIT_OPENSSL_FAILED") from error


def _copy(source: Path, destination: Path) -> None:
    _write(destination, source.read_bytes())


def _issue(
    openssl: Path,
    home: Path,
    authority: Path,
    name: str,
    extended_usage: str,
    subject_alt_name: str,
    days: int,
) -> tuple[Path, Path]:
    key = authority / f"{name}.key"
    request = authority / f"{name}.csr"
    extension = authority / f"{name}.ext"
    certificate = authority / f"{name}.crt"
    _write(
        extension,
        (
            "[v3_req]\n"
            "basicConstraints=critical,CA:FALSE\n"
            "keyUsage=critical,digitalSignature,keyEncipherment\n"
            f"extendedKeyUsage={extended_usage}\n"
            f"subjectAltName={subject_alt_name}\n"
        ).encode(),
    )
    _run(openssl, ["genpkey", "-algorithm", "RSA", "-pkeyopt", "rsa_keygen_bits:3072", "-out", str(key)], home)
    _run(openssl, ["req", "-new", "-key", str(key), "-subj", f"/CN={name}", "-out", str(request)], home)
    _run(
        openssl,
        [
            "x509", "-req", "-in", str(request), "-CA", str(authority / "ca.crt"),
            "-CAkey", str(authority / "ca.key"), "-CAcreateserial", "-days", str(days),
            "-sha256", "-extfile", str(extension), "-out", str(certificate),
            "-extensions", "v3_req",
        ],
        home,
    )
    return key, certificate


def prepare(
    output: Path,
    openssl: Path,
    days: int,
    registry_host: str,
    dns_nameserver: str,
    registry_ca: Path,
) -> dict[str, object]:
    if not 30 <= days <= 825:
        raise BuildkitAuthoringError("LW_PLATFORM_BUILDKIT_VALIDITY_INVALID")
    if not re.fullmatch(r"[a-z0-9](?:[a-z0-9.-]{0,251}[a-z0-9])?", registry_host):
        raise BuildkitAuthoringError("LW_PLATFORM_BUILDKIT_REGISTRY_HOST_INVALID")
    try:
        nameserver = str(ipaddress.ip_address(dns_nameserver))
    except ValueError as error:
        raise BuildkitAuthoringError("LW_PLATFORM_BUILDKIT_DNS_NAMESERVER_INVALID") from error
    try:
        registry_ca = registry_ca.resolve(strict=True)
    except OSError as error:
        raise BuildkitAuthoringError("LW_PLATFORM_BUILDKIT_REGISTRY_CA_INVALID") from error
    if not registry_ca.is_file() or registry_ca.stat().st_size > 1024 * 1024:
        raise BuildkitAuthoringError("LW_PLATFORM_BUILDKIT_REGISTRY_CA_INVALID")
    home = output / "home"
    authority = output / "authority"
    render_input = output / "render-input"
    client = output / "build-executor-client"
    for directory in (home, authority, render_input, client):
        directory.mkdir(parents=True, mode=0o700)

    ca_key = authority / "ca.key"
    ca_certificate = authority / "ca.crt"
    _run(openssl, ["genpkey", "-algorithm", "RSA", "-pkeyopt", "rsa_keygen_bits:4096", "-out", str(ca_key)], home)
    _run(
        openssl,
        [
            "req", "-x509", "-new", "-key", str(ca_key), "-sha256", "-days", str(days),
            "-subj", "/CN=LabWeaver Sprint 2 BuildKit CA", "-out", str(ca_certificate),
        ],
        home,
    )

    server_key, server_certificate = _issue(
        openssl,
        home,
        authority,
        "buildkit",
        "serverAuth",
        "DNS:buildkit,DNS:buildkit.labweaver-build,DNS:buildkit.labweaver-build.svc",
        days,
    )
    health_key, health_certificate = _issue(
        openssl, home, authority, "buildkit-health", "clientAuth", "URI:spiffe://labweaver/buildkit-health", days
    )
    client_key, client_certificate = _issue(
        openssl, home, authority, "build-executor", "clientAuth", "URI:spiffe://labweaver/build-executor", days
    )

    secret = render_input / "secrets" / "buildkit-server-secrets"
    for source, name in (
        (ca_certificate, "ca.crt"),
        (server_certificate, "tls.crt"),
        (server_key, "tls.key"),
        (health_certificate, "health.crt"),
        (health_key, "health.key"),
        (registry_ca, "registry-ca.crt"),
    ):
        _copy(source, secret / name)
    for source, name in (
        (ca_certificate, "ca.crt"),
        (client_certificate, "tls.crt"),
        (client_key, "tls.key"),
    ):
        _copy(source, client / name)

    configuration = f'''debug = false
root = "/home/user/.local/share/buildkit"

[grpc]
  address = ["tcp4://0.0.0.0:1234"]
  [grpc.tls]
    cert = "/etc/buildkit/tls/tls.crt"
    key = "/etc/buildkit/tls/tls.key"
    ca = "/etc/buildkit/tls/ca.crt"

[registry."{registry_host}"]
  ca = ["/etc/buildkit/tls/registry-ca.crt"]

[dns]
  nameservers = ["{nameserver}"]

[worker.oci]
  enabled = true
  rootless = true
  noProcessSandbox = true
  gc = true
'''.encode()
    _write(render_input / "configmaps" / "buildkit-config" / "buildkitd.toml", configuration)

    return {
        "ca_sha256": hashlib.sha256(ca_certificate.read_bytes()).hexdigest(),
        "config_sha256": hashlib.sha256(configuration).hexdigest(),
        "render_input": "render-input",
        "client_material": "build-executor-client",
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--openssl", type=Path, default=Path("/usr/bin/openssl"))
    parser.add_argument("--valid-days", type=int, default=365)
    parser.add_argument("--registry-host", required=True)
    parser.add_argument("--dns-nameserver", required=True)
    parser.add_argument("--registry-ca", type=Path, required=True)
    arguments = parser.parse_args()
    output: Path | None = None
    try:
        output = _private_output(arguments.output)
        openssl = _trusted_openssl(arguments.openssl)
        output.mkdir(mode=0o700)
        result = prepare(
            output,
            openssl,
            arguments.valid_days,
            arguments.registry_host,
            arguments.dns_nameserver,
            arguments.registry_ca,
        )
    except (BuildkitAuthoringError, OSError) as error:
        if output is not None and output.is_dir():
            shutil.rmtree(output)
        diagnostic = str(error) if isinstance(error, BuildkitAuthoringError) else "LW_PLATFORM_BUILDKIT_AUTHORING_FAILED"
        print(diagnostic, file=sys.stderr)
        return 1
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
