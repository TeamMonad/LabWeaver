#!/usr/bin/env python3
"""Issue the Resource service TLS identities from reviewed private CAs.

The command copies no CA private key to the output.  It creates one NATS client
certificate and one platform server/client certificate, with bounded SAN/EKU
profiles.  All inputs and outputs must live below a private locator and every
failure is reported without exposing certificate or key material.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import stat
import subprocess
from pathlib import Path


class TlsIssuanceError(RuntimeError):
    """Stable fail-closed TLS issuance diagnostic."""


def private_path(path: Path) -> Path:
    resolved = path.resolve()
    if not any(part in {".private", "private"} for part in resolved.parts):
        raise TlsIssuanceError("LW_RESOURCE_TLS_PRIVATE_PATH_REQUIRED")
    if not resolved.parent.is_dir():
        raise TlsIssuanceError("LW_RESOURCE_TLS_PRIVATE_PARENT_MISSING")
    return resolved


def trusted_tool(path: Path, name: str) -> Path:
    resolved = path.resolve(strict=True)
    mode = resolved.stat().st_mode
    if (
        resolved.name != name
        or not resolved.is_file()
        or mode & (stat.S_IWGRP | stat.S_IWOTH)
    ):
        raise TlsIssuanceError("LW_RESOURCE_TLS_TOOL_INVALID")
    return resolved


def secure_ca(directory: Path) -> tuple[Path, Path]:
    directory = directory.resolve(strict=True)
    if not directory.is_dir() or directory.is_symlink() or directory.stat().st_mode & (stat.S_IWGRP | stat.S_IWOTH):
        raise TlsIssuanceError("LW_RESOURCE_TLS_CA_INVALID")
    key = directory / "ca.key"
    certificate = directory / "ca.crt"
    for path in (key, certificate):
        mode = path.stat().st_mode
        if path.is_symlink() or not path.is_file() or mode & (stat.S_IWGRP | stat.S_IWOTH):
            raise TlsIssuanceError("LW_RESOURCE_TLS_CA_INVALID")
    return key, certificate


def run_openssl(openssl: Path, arguments: list[str], home: Path) -> None:
    environment = {
        "HOME": str(home),
        "LANG": "C.UTF-8",
        "LC_ALL": "C.UTF-8",
        "PATH": "/usr/bin:/bin",
    }
    result = subprocess.run(
        [str(openssl), *arguments],
        env=environment,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        check=False,
        text=True,
        timeout=60,
    )
    if result.returncode:
        raise TlsIssuanceError("LW_RESOURCE_TLS_SIGNING_FAILED")


def issue_certificate(
    openssl: Path,
    ca_key: Path,
    ca_certificate: Path,
    output: Path,
    common_name: str,
    sans: tuple[str, ...],
    eku: str,
    days: int,
    home: Path,
) -> None:
    key = output / "key"
    csr = output / "request.csr"
    certificate = output / "certificate"
    extension = output / "extension.cnf"
    serial = output / "serial"
    extension.write_text(
        "[v3_req]\n"
        f"subjectAltName={','.join(sans)}\n"
        f"extendedKeyUsage={eku}\n",
        encoding="utf-8",
    )
    os.chmod(extension, 0o600)
    run_openssl(
        openssl,
        ["genpkey", "-algorithm", "RSA", "-pkeyopt", "rsa_keygen_bits:3072", "-out", str(key)],
        home,
    )
    run_openssl(
        openssl,
        ["req", "-new", "-key", str(key), "-subj", f"/CN={common_name}", "-out", str(csr)],
        home,
    )
    run_openssl(
        openssl,
        [
            "x509", "-req", "-in", str(csr), "-CA", str(ca_certificate),
            "-CAkey", str(ca_key), "-CAcreateserial", "-CAserial", str(serial),
            "-days", str(days), "-sha256", "-extfile", str(extension),
            "-extensions", "v3_req", "-out", str(certificate),
        ],
        home,
    )
    for path in (key, certificate):
        os.chmod(path, 0o600)


def issue(authority: Path, platform_authority: Path, openssl: Path, output: Path, days: int) -> dict[str, object]:
    if not 30 <= days <= 825:
        raise TlsIssuanceError("LW_RESOURCE_TLS_VALIDITY_INVALID")
    ca_key, ca_certificate = secure_ca(authority)
    platform_key, platform_certificate = secure_ca(platform_authority)
    output = private_path(output)
    if output.exists():
        raise TlsIssuanceError("LW_RESOURCE_TLS_OUTPUT_EXISTS")
    output.mkdir(mode=0o700)
    home = output / "home"
    home.mkdir(mode=0o700)
    nats = output / "nats"
    platform = output / "platform"
    nats.mkdir(mode=0o700)
    platform.mkdir(mode=0o700)
    try:
        issue_certificate(
            openssl, ca_key, ca_certificate, nats, "resource-service",
            ("URI:spiffe://labweaver/resource-service",), "clientAuth", days, home,
        )
        issue_certificate(
            openssl, platform_key, platform_certificate, platform, "resource-service",
            (
                "DNS:resource-service",
                "DNS:resource-service.labweaver-system.svc",
                "URI:spiffe://labweaver/resource-service",
            ),
            "serverAuth,clientAuth", days, home,
        )
        shutil.copyfile(ca_certificate, nats / "ca.pem")
        shutil.copyfile(platform_certificate, platform / "ca.pem")
        for path in (nats / "ca.pem", platform / "ca.pem"):
            os.chmod(path, 0o600)
    except Exception:
        shutil.rmtree(output, ignore_errors=True)
        raise
    for path in output.rglob("*"):
        if path.is_file() and path.name not in {"certificate", "key", "ca.pem"}:
            path.unlink()
    if home.exists():
        home.rmdir()
    return {
        "status": "issued",
        "identity": "resource-service",
        "nats_client_locator": "nats/{key,certificate,ca.pem}",
        "platform_identity_locator": "platform/{key,certificate,ca.pem}",
        "valid_days": days,
        "secret_material_in_record": False,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--authority", type=Path, required=True)
    parser.add_argument("--platform-authority", type=Path, required=True)
    parser.add_argument("--openssl", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--valid-days", type=int, default=365)
    args = parser.parse_args()
    try:
        result = issue(
            args.authority,
            args.platform_authority,
            trusted_tool(args.openssl, "openssl"),
            args.output,
            args.valid_days,
        )
    except (OSError, subprocess.SubprocessError, TlsIssuanceError) as error:
        print(str(error), file=os.sys.stderr)
        return 2
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
