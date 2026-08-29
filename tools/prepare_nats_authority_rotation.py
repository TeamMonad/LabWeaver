#!/usr/bin/env python3
"""Build private NATS authority-rotation bundles without exposing credentials."""

from __future__ import annotations

import argparse
import base64
import json
import os
import re
import stat
import sys
from pathlib import Path
from typing import Any

import yaml

from prepare_platform_foundation import (
    NATS_ADMIN_PUBLISH,
    NATS_ADMIN_SUBSCRIBE,
    NATS_ADMIN_USER,
    NATS_USERS,
)


class RotationError(Exception):
    """Stable fail-closed NATS authority-rotation diagnostic."""


APPLICATION_IDENTITIES = {
    "control-service": "control-service-secrets",
    "access-service": "access-service-secrets",
    "agent-service": "agent-service-secrets",
    "build-executor": "build-executor-secrets",
    "environment-service": "environment-service-secrets",
    "evaluation-service": "evaluation-service-secrets",
    "container-executor": "container-executor-secrets",
    "kubevirt-executor": "kubevirt-executor-secrets",
}
NATS_SECRET_KEYS = {
    "nats.creds": "nats.creds",
    "nats-ca.pem": "nats-ca.pem",
    "nats-client.crt": "nats-client.crt",
    "nats-client.key": "nats-client.key",
}
PLATFORM_ROTATION_IDENTITIES = {
    "control-service": "control-service-secrets",
    "access-service": "access-service-secrets",
    "agent-service": "agent-service-secrets",
    "environment-service": "environment-service-secrets",
    "container-executor": "container-executor-secrets",
    "kubevirt-console-executor": "kubevirt-console-executor-secrets",
    "evaluation-service": "evaluation-service-secrets",
    "openssh-gateway": "openssh-gateway-secrets",
}
PLATFORM_SECRET_KEYS = {
    "mtls-ca.pem": "ca.pem",
    "tls.crt": "certificate.pem",
    "tls.key": "key.pem",
}
PLATFORM_SECRET_KEYS_BY_IDENTITY = {
    "openssh-gateway": {
        "mtls-ca.pem": "ca.pem",
        "mtls.crt": "certificate.pem",
        "mtls.key": "key.pem",
    },
}
JWT_PATTERN = re.compile(
    rb"-----BEGIN NATS USER JWT-----\s+([A-Za-z0-9._-]+)"
)


def _private_file(path: Path) -> Path:
    if not path.is_absolute():
        raise RotationError("LW_NATS_ROTATION_PRIVATE_INPUT_INVALID")
    try:
        resolved = path.resolve(strict=True)
        metadata = resolved.stat()
    except OSError as error:
        raise RotationError("LW_NATS_ROTATION_PRIVATE_INPUT_INVALID") from error
    if (
        not resolved.is_file()
        or (os.name != "nt" and metadata.st_mode & (stat.S_IRWXG | stat.S_IRWXO))
    ):
        raise RotationError("LW_NATS_ROTATION_PRIVATE_INPUT_INVALID")
    return resolved


def _private_directory(path: Path) -> Path:
    if not path.is_absolute():
        raise RotationError("LW_NATS_ROTATION_PRIVATE_INPUT_INVALID")
    try:
        resolved = path.resolve(strict=True)
        metadata = resolved.stat()
    except OSError as error:
        raise RotationError("LW_NATS_ROTATION_PRIVATE_INPUT_INVALID") from error
    if (
        not resolved.is_dir()
        or (os.name != "nt" and metadata.st_mode & (stat.S_IRWXG | stat.S_IRWXO))
    ):
        raise RotationError("LW_NATS_ROTATION_PRIVATE_INPUT_INVALID")
    return resolved


def _contract_file(path: Path) -> Path:
    """Resolve a checked-in, non-secret contract file."""
    if not path.is_absolute():
        raise RotationError("LW_NATS_ROTATION_CONTRACT_INVALID")
    try:
        resolved = path.resolve(strict=True)
        metadata = resolved.stat()
    except OSError as error:
        raise RotationError("LW_NATS_ROTATION_CONTRACT_INVALID") from error
    if not resolved.is_file() or metadata.st_mode & stat.S_IRWXO == 0o111:
        raise RotationError("LW_NATS_ROTATION_CONTRACT_INVALID")
    return resolved


def _new_private_output(path: Path) -> Path:
    if not path.is_absolute() or not any(
        part in {".private", "private"} for part in path.parts
    ):
        raise RotationError("LW_NATS_ROTATION_PRIVATE_OUTPUT_INVALID")
    resolved = path.resolve()
    if resolved.exists() or not resolved.parent.is_dir():
        raise RotationError("LW_NATS_ROTATION_PRIVATE_OUTPUT_INVALID")
    resolved.mkdir(mode=0o700)
    return resolved


def _load_bundle(path: Path) -> list[dict[str, Any]]:
    try:
        documents = list(yaml.safe_load_all(path.read_text(encoding="utf-8")))
    except (OSError, UnicodeError, yaml.YAMLError) as error:
        raise RotationError("LW_NATS_ROTATION_BUNDLE_INVALID") from error
    if (
        not documents
        or any(not isinstance(document, dict) for document in documents)
        or any(
            document.get("apiVersion") != "v1"
            or document.get("kind") not in {"ConfigMap", "Secret"}
            for document in documents
        )
    ):
        raise RotationError("LW_NATS_ROTATION_BUNDLE_INVALID")
    identities = [
        (document["kind"], document.get("metadata", {}).get("name"))
        for document in documents
    ]
    if any(not name for _, name in identities) or len(identities) != len(
        set(identities)
    ):
        raise RotationError("LW_NATS_ROTATION_BUNDLE_INVALID")
    return documents


def _object(
    documents: list[dict[str, Any]], kind: str, name: str
) -> dict[str, Any]:
    matches = [
        document
        for document in documents
        if document["kind"] == kind
        and document.get("metadata", {}).get("name") == name
    ]
    if len(matches) != 1 or not isinstance(matches[0].get("data"), dict):
        raise RotationError("LW_NATS_ROTATION_BUNDLE_INVALID")
    return matches[0]


def _read(path: Path) -> bytes:
    try:
        payload = path.read_bytes()
    except OSError as error:
        raise RotationError("LW_NATS_ROTATION_MATERIAL_INVALID") from error
    if not payload or len(payload) > 1024 * 1024:
        raise RotationError("LW_NATS_ROTATION_MATERIAL_INVALID")
    return payload


def _replace_client(
    secret: dict[str, Any], client_directory: Path
) -> None:
    data = secret["data"]
    if not set(NATS_SECRET_KEYS).issubset(data):
        raise RotationError("LW_NATS_ROTATION_BUNDLE_INVALID")
    for data_key, filename in NATS_SECRET_KEYS.items():
        data[data_key] = base64.b64encode(
            _read(client_directory / filename)
        ).decode("ascii")


def _replace_platform_identity(
    secret: dict[str, Any], identity_directory: Path, keys: dict[str, str]
) -> None:
    data = secret["data"]
    if not set(keys).issubset(data):
        raise RotationError("LW_NATS_ROTATION_BUNDLE_INVALID")
    for data_key, filename in keys.items():
        data[data_key] = base64.b64encode(
            _read(identity_directory / filename)
        ).decode("ascii")


def _decode_jwt(token: str) -> dict[str, Any]:
    try:
        payload = token.split(".")[1]
        payload += "=" * (-len(payload) % 4)
        value = json.loads(base64.urlsafe_b64decode(payload))
    except (IndexError, ValueError, json.JSONDecodeError) as error:
        raise RotationError("LW_NATS_ROTATION_JWT_INVALID") from error
    if not isinstance(value, dict):
        raise RotationError("LW_NATS_ROTATION_JWT_INVALID")
    return value


def _credential_claims(path: Path) -> dict[str, Any]:
    match = JWT_PATTERN.search(_read(path))
    if not match:
        raise RotationError("LW_NATS_ROTATION_JWT_INVALID")
    return _decode_jwt(match.group(1).decode("ascii"))


def _assert_permissions(
    name: str,
    claims: dict[str, Any],
    publish: tuple[str, ...],
    subscribe: tuple[str, ...],
    response: bool,
    account_public: str,
) -> dict[str, Any]:
    try:
        nats = claims["nats"]
        actual_publish = tuple(sorted(nats.get("pub", {}).get("allow", [])))
        actual_subscribe = tuple(sorted(nats.get("sub", {}).get("allow", [])))
        response_max = nats.get("resp", {}).get("max")
        public = claims["sub"]
        issuer = claims["iss"]
    except (KeyError, TypeError) as error:
        raise RotationError("LW_NATS_ROTATION_JWT_INVALID") from error
    if (
        issuer != account_public
        or actual_publish != tuple(sorted(publish))
        or actual_subscribe != tuple(sorted(subscribe))
        or (response and response_max != 1)
        or (not response and response_max not in (None, 0))
    ):
        raise RotationError("LW_NATS_ROTATION_PERMISSIONS_INVALID")
    return {
        "identity": name,
        "public": public,
        "issuer": issuer,
        "publish": list(actual_publish),
        "subscribe": list(actual_subscribe),
        "response_max": response_max,
    }


def _write(path: Path, payload: bytes) -> None:
    descriptor = os.open(
        path,
        os.O_WRONLY | os.O_CREAT | os.O_EXCL,
        0o600,
    )
    with os.fdopen(descriptor, "wb") as handle:
        handle.write(payload)
        handle.flush()
        os.fsync(handle.fileno())


def _dump_bundle(path: Path, documents: list[dict[str, Any]]) -> None:
    payload = yaml.safe_dump_all(
        documents,
        explicit_start=True,
        sort_keys=True,
        allow_unicode=False,
    ).encode("utf-8")
    _write(path, payload)


def _ensure_access_resource_gateway(
    application_bundle: list[dict[str, Any]], example_path: Path
) -> None:
    """Apply the reviewed Access schema, aliases, and Resource gateway contract.

    Private deployment inputs can outlive the checked-in Access contract.  The
    rotation boundary is the last deterministic point before a bundle can be
    applied, so it must materialize newly-added non-secret fields from the
    reviewed example while rejecting conflicting values.  This keeps a stale
    private input from becoming a candidate that only fails after deployment
    has started.
    """
    try:
        example_config = yaml.safe_load(
            _contract_file(example_path).read_text(encoding="utf-8")
        )
        access = _object(application_bundle, "ConfigMap", "access-service-config")
        config = yaml.safe_load(access["data"]["config.yaml"])
        grants = config.get("grants")
        example_grants = example_config.get("grants")
        if not isinstance(grants, dict) or not isinstance(example_grants, dict):
            raise RotationError("LW_NATS_ROTATION_CONTRACT_INVALID")
        for field in (
            "max_console_sessions",
            "environment_state_stream",
            "environment_state_consumer",
        ):
            expected = example_grants.get(field)
            if expected is None:
                raise RotationError("LW_NATS_ROTATION_CONTRACT_INVALID")
            existing = grants.get(field)
            if existing is not None and existing != expected:
                raise RotationError("LW_NATS_ROTATION_CONTRACT_INVALID")
            grants[field] = expected
        resource_gateway = example_config.get("resource_gateway")
        if not isinstance(resource_gateway, dict):
            raise RotationError("LW_NATS_ROTATION_CONTRACT_INVALID")
        existing = config.get("resource_gateway")
        if existing is None:
            config["resource_gateway"] = resource_gateway
        elif existing != resource_gateway:
            raise RotationError("LW_NATS_ROTATION_CONTRACT_INVALID")
        oidc = config.get("oidc")
        role_mappings = oidc.get("role_mappings") if isinstance(oidc, dict) else None
        if not isinstance(role_mappings, dict) or role_mappings.get("platform-admin") != "platform_admin":
            raise RotationError("LW_NATS_ROTATION_CONTRACT_INVALID")
        if "platform_admin" in role_mappings and role_mappings["platform_admin"] != "platform_admin":
            raise RotationError("LW_NATS_ROTATION_CONTRACT_INVALID")
        role_mappings["platform_admin"] = "platform_admin"
        access["data"]["config.yaml"] = yaml.safe_dump(
            config, sort_keys=False, allow_unicode=False
        )
    except (KeyError, TypeError, StopIteration, yaml.YAMLError, OSError) as error:
        raise RotationError("LW_NATS_ROTATION_CONTRACT_INVALID") from error


def prepare(
    foundation_bundle_path: Path,
    application_bundle_path: Path,
    resource_bundle_path: Path,
    authority_root: Path,
    output_path: Path,
    access_config_example_path: Path | None = None,
) -> dict[str, Any]:
    foundation_bundle = _load_bundle(_private_file(foundation_bundle_path))
    application_bundle = _load_bundle(_private_file(application_bundle_path))
    if access_config_example_path is not None:
        _ensure_access_resource_gateway(application_bundle, access_config_example_path)
    resource_bundle = _load_bundle(_private_file(resource_bundle_path))
    authority = _private_directory(authority_root)
    output = _new_private_output(output_path)

    generated_config = _read(
        authority
        / "render-input"
        / "configmaps"
        / "nats-config"
        / "nats-server.conf"
    )
    _object(foundation_bundle, "ConfigMap", "nats-config")["data"][
        "nats-server.conf"
    ] = generated_config.decode("utf-8")
    nats_server_secret = _object(
        foundation_bundle, "Secret", "nats-server-secrets"
    )
    generated_server_secret = (
        authority / "render-input" / "secrets" / "nats-server-secrets"
    )
    for key in ("ca.crt", "tls.crt", "tls.key"):
        if key not in nats_server_secret["data"]:
            raise RotationError("LW_NATS_ROTATION_BUNDLE_INVALID")
        nats_server_secret["data"][key] = base64.b64encode(
            _read(generated_server_secret / key)
        ).decode("ascii")

    clients = authority / "nats-clients"
    platform_identities = authority / "platform-identities"
    identity_records: list[dict[str, Any]] = []
    account_public = None
    for identity, secret_name in APPLICATION_IDENTITIES.items():
        claims = _credential_claims(clients / identity / "nats.creds")
        if account_public is None:
            account_public = claims.get("iss")
        publish, subscribe, response = NATS_USERS[identity]
        identity_records.append(
            _assert_permissions(
                identity,
                claims,
                publish,
                subscribe,
                response,
                account_public,
            )
        )
        _replace_client(
            _object(application_bundle, "Secret", secret_name),
            clients / identity,
        )
        if identity in PLATFORM_ROTATION_IDENTITIES:
            _replace_platform_identity(
                _object(application_bundle, "Secret", secret_name),
                platform_identities / identity,
                PLATFORM_SECRET_KEYS_BY_IDENTITY.get(
                    identity, PLATFORM_SECRET_KEYS
                ),
            )

    for identity, secret_name in PLATFORM_ROTATION_IDENTITIES.items():
        if identity not in APPLICATION_IDENTITIES:
            _replace_platform_identity(
                _object(application_bundle, "Secret", secret_name),
                platform_identities / identity,
                PLATFORM_SECRET_KEYS_BY_IDENTITY.get(
                    identity, PLATFORM_SECRET_KEYS
                ),
            )

    resource_claims = _credential_claims(
        clients / "resource-service" / "nats.creds"
    )
    publish, subscribe, response = NATS_USERS["resource-service"]
    identity_records.append(
        _assert_permissions(
            "resource-service",
            resource_claims,
            publish,
            subscribe,
            response,
            account_public or "",
        )
    )
    _replace_client(
        _object(resource_bundle, "Secret", "resource-service-secrets"),
        clients / "resource-service",
    )
    _replace_platform_identity(
        _object(resource_bundle, "Secret", "resource-service-secrets"),
        platform_identities / "resource-service",
        PLATFORM_SECRET_KEYS,
    )

    admin_claims = _credential_claims(
        clients / NATS_ADMIN_USER / "nats.creds"
    )
    identity_records.append(
        _assert_permissions(
            NATS_ADMIN_USER,
            admin_claims,
            NATS_ADMIN_PUBLISH,
            NATS_ADMIN_SUBSCRIBE,
            False,
            account_public or "",
        )
    )

    config_tokens = re.findall(
        r"eyJ[A-Za-z0-9._-]+", generated_config.decode("utf-8")
    )
    config_claims = [_decode_jwt(token) for token in config_tokens]
    operator = next(
        (
            value
            for value in config_claims
            if value.get("nats", {}).get("type") == "operator"
        ),
        None,
    )
    accounts = {
        value.get("name"): value.get("sub")
        for value in config_claims
        if value.get("nats", {}).get("type") == "account"
    }
    if (
        operator is None
        or accounts.get("WORKLOADS") != account_public
        or set(accounts) != {"SYS", "WORKLOADS"}
    ):
        raise RotationError("LW_NATS_ROTATION_AUTHORITY_INVALID")

    _dump_bundle(output / "foundation-bundle.yaml", foundation_bundle)
    _dump_bundle(output / "application-bundle.yaml", application_bundle)
    _dump_bundle(output / "resource-bundle.yaml", resource_bundle)
    admin_directory = output / "nats-admin"
    admin_directory.mkdir(mode=0o700)
    for filename in NATS_SECRET_KEYS.values():
        _write(
            admin_directory / filename,
            _read(clients / NATS_ADMIN_USER / filename),
        )

    record = {
        "schema_version": "nats-authority-rotation.v1",
        "status": "prepared",
        "authority_locator": str(authority),
        "foundation_bundle_locator": str(output / "foundation-bundle.yaml"),
        "application_bundle_locator": str(output / "application-bundle.yaml"),
        "resource_bundle_locator": str(output / "resource-bundle.yaml"),
        "admin_identity_locator": str(admin_directory),
        "operator_public": operator["sub"],
        "system_account_public": accounts["SYS"],
        "workloads_account_public": accounts["WORKLOADS"],
        "identities": sorted(
            identity_records,
            key=lambda value: value["identity"],
        ),
    }
    _write(
        output / "rotation-record.json",
        (json.dumps(record, indent=2, sort_keys=True) + "\n").encode("utf-8"),
    )
    return record


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--foundation-bundle", type=Path, required=True)
    parser.add_argument("--application-bundle", type=Path, required=True)
    parser.add_argument("--resource-bundle", type=Path, required=True)
    parser.add_argument("--authority-root", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--access-config-example", type=Path, required=True)
    arguments = parser.parse_args()
    try:
        result = prepare(
            arguments.foundation_bundle,
            arguments.application_bundle,
            arguments.resource_bundle,
            arguments.authority_root,
            arguments.output,
            arguments.access_config_example,
        )
    except RotationError as error:
        print(str(error), file=sys.stderr)
        return 2
    print(
        json.dumps(
            {
                "status": result["status"],
                "operator_public": result["operator_public"],
                "workloads_account_public": result[
                    "workloads_account_public"
                ],
                "identities": len(result["identities"]),
            },
            sort_keys=True,
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
