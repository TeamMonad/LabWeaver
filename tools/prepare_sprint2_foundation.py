#!/usr/bin/env python3
"""Create private Sprint 2 foundation PKI, NATS JWTs, and bundle inputs."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import secrets
import shutil
import stat
import subprocess
import sys
from pathlib import Path


class FoundationError(Exception):
    """Stable fail-closed foundation-authoring diagnostic."""


NATS_USERS: dict[str, tuple[tuple[str, ...], tuple[str, ...], bool]] = {
    "control-service": (
        (
            "$JS.API.>",
            "$JS.ACK.>",
            "labweaver.control.>",
            "labweaver.agent.quarantine.>",
            "labweaver.agent.run.requested.v1",
        ),
        ("_INBOX.>", "labweaver.agent.run.>", "labweaver.agent.build.>"),
        False,
    ),
    "access-service": (
        ("$JS.API.>", "$JS.ACK.>", "labweaver.access.>"),
        ("_INBOX.>", "labweaver.service.access.revoke.v1"),
        True,
    ),
    "agent-service": (
        (
            "$JS.API.>",
            "$JS.ACK.>",
            "labweaver.agent.>",
            "labweaver.provider.container_build.execute.v1",
        ),
        ("_INBOX.>", "labweaver.control.agent_build.requested.v1"),
        False,
    ),
    "build-executor": ((), ("_INBOX.>", "labweaver.provider.container_build.execute.v1"), True),
    "environment-service": (
        (
            "$JS.API.>",
            "$JS.ACK.>",
            "labweaver.environment.>",
            "labweaver.service.access.revoke.v1",
            "labweaver.provider.kubernetes.container.v1",
            "labweaver.provider.kubevirt.vm.v1",
            "labweaver.resource.lease.verify.v1",
        ),
        (
            "_INBOX.>",
            "labweaver.access.>",
            "labweaver.control.environment_template_release.>",
            "labweaver.environment.instance.lifecycle_requested.v1",
        ),
        False,
    ),
    "evaluation-service": (
        (
            "$JS.API.>",
            "$JS.ACK.>",
            "labweaver.evaluation.submission.freeze_requested.v1",
            "labweaver.evaluation.submission.frozen.v1",
        ),
        ("_INBOX.>", "labweaver.evaluation.submission.freeze_requested.v1"),
        False,
    ),
    "container-executor": ((), ("_INBOX.>", "labweaver.provider.kubernetes.container.v1"), True),
    "kubevirt-executor": ((), ("_INBOX.>", "labweaver.provider.kubevirt.vm.v1"), True),
    # Resource is the lease authority. It publishes its transactional outbox
    # through JetStream and consumes verification requests/replies.
    "resource-service": (
        (
            "$JS.API.>",
            "$JS.ACK.>",
            "labweaver.resource.request.submitted.v1",
            "labweaver.resource.request.approved.v1",
        ),
        ("_INBOX.>", "labweaver.resource.lease.verify.v1"),
        True,
    ),
}

PLATFORM_IDENTITIES: dict[str, tuple[tuple[str, ...], str]] = {
    "control-service": (
        (
            "DNS:control-service",
            "DNS:control-service.labweaver-system.svc",
            "URI:spiffe://labweaver/control-service",
        ),
        "serverAuth,clientAuth",
    ),
    "resource-service": (
        (
            "DNS:resource-service",
            "DNS:resource-service.labweaver-system.svc",
            "URI:spiffe://labweaver/resource-service",
        ),
        "serverAuth,clientAuth",
    ),
    "access-service": (
        (
            "DNS:access-service",
            "DNS:access-service.labweaver-system.svc",
            "URI:spiffe://labweaver/access-service",
        ),
        "serverAuth,clientAuth",
    ),
    "agent-service": (
        (
            "DNS:agent-service",
            "DNS:agent-service.labweaver-system.svc",
            "URI:spiffe://labweaver/agent-service",
        ),
        "serverAuth,clientAuth",
    ),
    "environment-service": (
        ("DNS:environment-service", "DNS:environment-service.labweaver-system.svc"),
        "serverAuth",
    ),
    "evaluation-service": (
        (
            "DNS:evaluation-service",
            "DNS:evaluation-service.labweaver-system.svc",
            "URI:spiffe://labweaver/evaluation-service",
        ),
        "serverAuth,clientAuth",
    ),
    "openssh-gateway": (("URI:spiffe://labweaver/openssh-gateway",), "clientAuth"),
}

NATS_ADMIN_TLS_IDENTITY = "sprint2-admin"
NATS_ADMIN_USER = "sprint2-admin"
NATS_ADMIN_PUBLISH = ("$JS.API.>", "$JS.ACK.>")
NATS_ADMIN_SUBSCRIBE = ("_INBOX.>",)
NATS_ACCOUNT_PUBLIC_PATTERN = re.compile(r"^A[A-Z2-7]{55}$")

NATS_ACCOUNT_JETSTREAM_LIMITS = (
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
)


def _private_output(path: Path) -> Path:
    resolved = path.resolve()
    if not any(part in {".private", "private"} for part in resolved.parts):
        raise FoundationError("LW_SPRINT2_FOUNDATION_PRIVATE_PATH_REQUIRED")
    if resolved.exists():
        raise FoundationError("LW_SPRINT2_FOUNDATION_OUTPUT_EXISTS")
    if not resolved.parent.is_dir():
        raise FoundationError("LW_SPRINT2_FOUNDATION_PARENT_MISSING")
    return resolved


def _trusted_binary(path: Path, expected_name: str) -> Path:
    if not path.is_absolute():
        raise FoundationError("LW_SPRINT2_FOUNDATION_TOOL_INVALID")
    try:
        resolved = path.resolve(strict=True)
        mode = resolved.stat().st_mode
    except OSError as error:
        raise FoundationError("LW_SPRINT2_FOUNDATION_TOOL_INVALID") from error
    if not resolved.is_file() or resolved.name != expected_name or mode & (stat.S_IWGRP | stat.S_IWOTH):
        raise FoundationError("LW_SPRINT2_FOUNDATION_TOOL_INVALID")
    return resolved


def _run(binary: Path, arguments: list[str], private_home: Path) -> None:
    environment = {
        "HOME": str(private_home),
        "LANG": "C.UTF-8",
        "LC_ALL": "C.UTF-8",
        "PATH": "/usr/local/bin:/usr/bin:/bin",
    }
    try:
        subprocess.run(
            [str(binary), *arguments],
            check=True,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            env=environment,
            timeout=60,
        )
    except (OSError, subprocess.SubprocessError) as error:
        operation = "-".join(
            token.lstrip("-").replace("_", "-")
            for token in arguments[:2]
            if token and len(token) <= 32 and token.replace("-", "").isalnum()
        )
        suffix = f":{binary.name}:{operation}" if operation else f":{binary.name}"
        raise FoundationError(f"LW_SPRINT2_FOUNDATION_TOOL_FAILED{suffix}") from error


def _write(path: Path, payload: bytes, mode: int = 0o600) -> None:
    path.parent.mkdir(parents=True, mode=0o700, exist_ok=True)
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, mode)
    with os.fdopen(descriptor, "wb") as handle:
        handle.write(payload)
        handle.flush()
        os.fsync(handle.fileno())


def _certificate(
    openssl: Path,
    private_home: Path,
    authority: Path,
    output: Path,
    common_name: str,
    sans: tuple[str, ...],
    usage: str,
    days: int,
) -> tuple[Path, Path]:
    key = authority / f"{common_name}.key"
    request = authority / f"{common_name}.csr"
    extension = authority / f"{common_name}.ext"
    certificate = authority / f"{common_name}.crt"
    _write(
        extension,
        (
            "[v3_req]\n"
            "basicConstraints=critical,CA:FALSE\n"
            "keyUsage=critical,digitalSignature,keyEncipherment\n"
            f"extendedKeyUsage={usage}\n"
            f"subjectAltName={','.join(sans)}\n"
        ).encode(),
    )
    _run(openssl, ["genpkey", "-algorithm", "RSA", "-pkeyopt", "rsa_keygen_bits:3072", "-out", str(key)], private_home)
    _run(openssl, ["req", "-new", "-key", str(key), "-subj", f"/CN={common_name}", "-out", str(request)], private_home)
    _run(
        openssl,
        [
            "x509", "-req", "-in", str(request), "-CA", str(authority / "ca.crt"),
            "-CAkey", str(authority / "ca.key"), "-CAcreateserial", "-days", str(days),
            "-sha256", "-extfile", str(extension), "-out", str(certificate),
            "-extensions", "v3_req",
        ],
        private_home,
    )
    shutil.copyfile(key, output / "key")
    shutil.copyfile(certificate, output / "certificate")
    os.chmod(output / "key", 0o600)
    os.chmod(output / "certificate", 0o600)
    return key, certificate


def _copy(source: Path, destination: Path) -> None:
    destination.parent.mkdir(parents=True, mode=0o700, exist_ok=True)
    with source.open("rb") as reader:
        _write(destination, reader.read())


def _workloads_seed_source(path: Path) -> tuple[Path, str]:
    if not path.is_absolute():
        raise FoundationError("LW_SPRINT2_FOUNDATION_WORKLOADS_SEED_INVALID")
    try:
        resolved = path.resolve(strict=True)
        metadata = resolved.stat()
    except OSError as error:
        raise FoundationError("LW_SPRINT2_FOUNDATION_WORKLOADS_SEED_INVALID") from error
    public_key = resolved.stem
    if (
        not resolved.is_file()
        or not NATS_ACCOUNT_PUBLIC_PATTERN.fullmatch(public_key)
        or (
            os.name != "nt"
            and metadata.st_mode & (stat.S_IRWXG | stat.S_IRWXO)
        )
    ):
        raise FoundationError("LW_SPRINT2_FOUNDATION_WORKLOADS_SEED_INVALID")
    return resolved, public_key


def _nsc(nsc: Path, store: Path, arguments: list[str], private_home: Path) -> None:
    _run(nsc, ["--all-dirs", str(store), *arguments], private_home)


def prepare(
    output: Path,
    openssl: Path,
    ssh_keygen: Path,
    nsc: Path,
    days: int,
    workloads_seed_file: Path | None = None,
) -> dict[str, object]:
    if not 30 <= days <= 825:
        raise FoundationError("LW_SPRINT2_FOUNDATION_VALIDITY_INVALID")
    private_home = output / "home"
    authority = output / "authority"
    nsc_store = output / "nsc"
    render_input = output / "render-input"
    clients = output / "nats-clients"
    platform_authority = output / "platform-authority"
    platform_identities = output / "platform-identities"
    ssh_authority = output / "ssh-authority"
    for directory in (
        private_home,
        authority,
        nsc_store,
        render_input,
        clients,
        platform_authority,
        platform_identities,
        ssh_authority,
    ):
        directory.mkdir(parents=True, mode=0o700)

    ca_key = authority / "ca.key"
    ca_certificate = authority / "ca.crt"
    _run(openssl, ["genpkey", "-algorithm", "RSA", "-pkeyopt", "rsa_keygen_bits:4096", "-out", str(ca_key)], private_home)
    _run(
        openssl,
        [
            "req", "-x509", "-new", "-key", str(ca_key), "-sha256", "-days", str(days),
            "-subj", "/CN=LabWeaver Sprint 2 Internal CA", "-out", str(ca_certificate),
        ],
        private_home,
    )

    platform_ca_key = platform_authority / "ca.key"
    platform_ca_certificate = platform_authority / "ca.crt"
    _run(
        openssl,
        ["genpkey", "-algorithm", "RSA", "-pkeyopt", "rsa_keygen_bits:4096", "-out", str(platform_ca_key)],
        private_home,
    )
    _run(
        openssl,
        [
            "req", "-x509", "-new", "-key", str(platform_ca_key), "-sha256", "-days", str(days),
            "-subj", "/CN=LabWeaver Sprint 2 Platform CA", "-out", str(platform_ca_certificate),
        ],
        private_home,
    )
    for name, (sans, usage) in PLATFORM_IDENTITIES.items():
        material = platform_authority / f"issued-{name}"
        material.mkdir(mode=0o700)
        key, certificate = _certificate(
            openssl,
            private_home,
            platform_authority,
            material,
            name,
            sans,
            usage,
            days,
        )
        _copy(key, platform_identities / name / "key.pem")
        _copy(certificate, platform_identities / name / "certificate.pem")
        _copy(platform_ca_certificate, platform_identities / name / "ca.pem")

    collector_ca_key = ssh_authority / "collector-ca"
    _run(
        ssh_keygen,
        ["-q", "-t", "ed25519", "-N", "", "-C", "labweaver-collector-ca", "-f", str(collector_ca_key)],
        private_home,
    )

    for service in ("postgres", "nats", "minio"):
        material = authority / f"issued-{service}"
        material.mkdir(mode=0o700)
        _, certificate = _certificate(
            openssl,
            private_home,
            authority,
            material,
            service,
            (
                f"DNS:{service}",
                f"DNS:{service}.labweaver-data",
                f"DNS:{service}.labweaver-data.svc",
            ),
            "serverAuth",
            days,
        )
        secret_root = render_input / "secrets" / (
            "minio-secrets" if service == "minio" else f"{service}-server-secrets" if service == "nats" else "postgres-secrets"
        )
        if service == "minio":
            _copy(material / "key", secret_root / "private.key")
            _copy(certificate, secret_root / "public.crt")
            _copy(ca_certificate, secret_root / "ca.crt")
            _write(secret_root / "root-user", b"labweaver-root")
            _write(secret_root / "root-password", secrets.token_urlsafe(48).encode())
        else:
            _copy(material / "key", secret_root / "tls.key")
            _copy(certificate, secret_root / "tls.crt")
            _copy(ca_certificate, secret_root / "ca.crt")
            if service == "postgres":
                _write(secret_root / "postgres-password", secrets.token_urlsafe(48).encode())

    _nsc(nsc, nsc_store, ["add", "operator", "--name", "LABWEAVER", "--sys", "--generate-signing-key", "--expiry", f"{days}d"], private_home)
    workloads_account_public = None
    if workloads_seed_file is None:
        _nsc(nsc, nsc_store, ["add", "account", "--name", "WORKLOADS", "--expiry", f"{days}d"], private_home)
    else:
        source_seed, workloads_account_public = _workloads_seed_source(workloads_seed_file)
        _nsc(
            nsc,
            nsc_store,
            [
                "add",
                "account",
                "--name",
                "WORKLOADS",
                "--public-key",
                workloads_account_public,
                "--expiry",
                f"{days}d",
            ],
            private_home,
        )
        _copy(
            source_seed,
            nsc_store
            / "keys"
            / "A"
            / workloads_account_public[1:3]
            / f"{workloads_account_public}.nk",
        )
    _nsc(
        nsc,
        nsc_store,
        ["edit", "account", "--name", "WORKLOADS", "--js-enable", "0"],
        private_home,
    )
    _nsc(
        nsc,
        nsc_store,
        ["edit", "account", "--name", "WORKLOADS", *NATS_ACCOUNT_JETSTREAM_LIMITS],
        private_home,
    )
    for name, (publish, subscribe, response) in NATS_USERS.items():
        arguments = ["add", "user", "--account", "WORKLOADS", "--name", name, "--expiry", f"{days}d"]
        for subject in publish:
            arguments.extend(["--allow-pub", subject])
        for subject in subscribe:
            arguments.extend(["--allow-sub", subject])
        if response:
            arguments.append("--allow-pub-response")
        _nsc(nsc, nsc_store, arguments, private_home)
        # `nsc add user --allow-pub-response` normalizes away an explicitly
        # supplied `_INBOX.>` subscription. Access Service also uses the NATS
        # request API for JetStream and must retain its bounded reply inbox
        # subscription, so restore it after response permissions are applied.
        if response and "_INBOX.>" in subscribe:
            _nsc(
                nsc,
                nsc_store,
                [
                    "edit",
                    "user",
                    "--account",
                    "WORKLOADS",
                    "--name",
                    name,
                    "--allow-sub",
                    "_INBOX.>",
                ],
                private_home,
            )
        credentials = clients / name / "nats.creds"
        credentials.parent.mkdir(mode=0o700)
        _nsc(nsc, nsc_store, ["generate", "creds", "--account", "WORKLOADS", "--name", name, "--output-file", str(credentials)], private_home)
        os.chmod(credentials, 0o600)
        client_material = authority / f"issued-{name}"
        client_material.mkdir(mode=0o700)
        _, client_certificate = _certificate(
            openssl,
            private_home,
            authority,
            client_material,
            name,
            (f"URI:spiffe://labweaver/{name}",),
            "clientAuth",
            days,
        )
        _copy(client_material / "key", clients / name / "nats-client.key")
        _copy(client_certificate, clients / name / "nats-client.crt")
        _copy(ca_certificate, clients / name / "nats-ca.pem")

    admin_arguments = [
        "add",
        "user",
        "--account",
        "WORKLOADS",
        "--name",
        NATS_ADMIN_USER,
        "--expiry",
        f"{days}d",
    ]
    for subject in NATS_ADMIN_PUBLISH:
        admin_arguments.extend(["--allow-pub", subject])
    for subject in NATS_ADMIN_SUBSCRIBE:
        admin_arguments.extend(["--allow-sub", subject])
    _nsc(nsc, nsc_store, admin_arguments, private_home)
    admin_credentials = clients / NATS_ADMIN_USER / "nats.creds"
    admin_credentials.parent.mkdir(mode=0o700, exist_ok=True)
    _nsc(
        nsc,
        nsc_store,
        [
            "generate",
            "creds",
            "--account",
            "WORKLOADS",
            "--name",
            NATS_ADMIN_USER,
            "--output-file",
            str(admin_credentials),
        ],
        private_home,
    )
    os.chmod(admin_credentials, 0o600)

    admin_material = authority / f"issued-{NATS_ADMIN_TLS_IDENTITY}"
    admin_material.mkdir(mode=0o700)
    _, admin_certificate = _certificate(
        openssl,
        private_home,
        authority,
        admin_material,
        NATS_ADMIN_TLS_IDENTITY,
        (f"URI:spiffe://labweaver/{NATS_ADMIN_TLS_IDENTITY}",),
        "clientAuth",
        days,
    )
    _copy(admin_material / "key", clients / NATS_ADMIN_TLS_IDENTITY / "nats-client.key")
    _copy(admin_certificate, clients / NATS_ADMIN_TLS_IDENTITY / "nats-client.crt")
    _copy(ca_certificate, clients / NATS_ADMIN_TLS_IDENTITY / "nats-ca.pem")

    generated_config = authority / "nats-generated.conf"
    _nsc(
        nsc,
        nsc_store,
        ["generate", "config", "--mem-resolver", "--sys-account", "SYS", "--config-file", str(generated_config)],
        private_home,
    )
    nats_config = generated_config.read_text(encoding="utf-8") + """
server_name: sprint2-nats
listen: 0.0.0.0:4222
http: 0.0.0.0:8222
jetstream {
  store_dir: "/data"
  max_file_store: 8GB
  max_memory_store: 256MB
}
tls {
  cert_file: "/etc/nats/tls/tls.crt"
  key_file: "/etc/nats/tls/tls.key"
  ca_file: "/etc/nats/tls/ca.crt"
  verify: true
  timeout: 2
}
"""
    _write(render_input / "configmaps" / "nats-config" / "nats-server.conf", nats_config.encode())

    return {
        "ca_sha256": hashlib.sha256(ca_certificate.read_bytes()).hexdigest(),
        "nats_config_sha256": hashlib.sha256(nats_config.encode()).hexdigest(),
        "nats_clients": len(NATS_USERS),
        "nats_admin_jwt_clients": 1,
        "nats_admin_tls_clients": 1,
        "preserved_workloads_account": workloads_seed_file is not None,
        "workloads_account_public": workloads_account_public,
        "platform_ca_sha256": hashlib.sha256(platform_ca_certificate.read_bytes()).hexdigest(),
        "platform_identities": len(PLATFORM_IDENTITIES),
        "collector_ssh_ca_public_sha256": hashlib.sha256((ssh_authority / "collector-ca.pub").read_bytes()).hexdigest(),
        "render_input": "render-input",
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--openssl", type=Path, default=Path("/usr/bin/openssl"))
    parser.add_argument("--ssh-keygen", type=Path, default=Path("/usr/bin/ssh-keygen"))
    parser.add_argument("--nsc", type=Path, default=Path("/usr/local/bin/nsc"))
    parser.add_argument("--workloads-seed-file", type=Path)
    parser.add_argument("--valid-days", type=int, default=365)
    arguments = parser.parse_args()
    output: Path | None = None
    try:
        output = _private_output(arguments.output)
        openssl = _trusted_binary(arguments.openssl, "openssl")
        ssh_keygen = _trusted_binary(arguments.ssh_keygen, "ssh-keygen")
        nsc = _trusted_binary(arguments.nsc, "nsc")
        output.mkdir(mode=0o700)
        result = prepare(
            output,
            openssl,
            ssh_keygen,
            nsc,
            arguments.valid_days,
            arguments.workloads_seed_file,
        )
    except (FoundationError, OSError, UnicodeError) as error:
        if output is not None and output.is_dir():
            shutil.rmtree(output)
        diagnostic = str(error) if isinstance(error, FoundationError) else "LW_SPRINT2_FOUNDATION_AUTHORING_FAILED"
        print(diagnostic, file=sys.stderr)
        return 1
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
