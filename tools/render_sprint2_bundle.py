#!/usr/bin/env python3
"""Render the reviewed Sprint 2 ConfigMap/Secret bundle without logging values."""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import re
import sys
from pathlib import Path
from typing import Any

MAX_FILE_BYTES = 1024 * 1024
DNS_LABEL = re.compile(r"^[a-z0-9](?:[-a-z0-9]{0,61}[a-z0-9])?$")
DATA_KEY = re.compile(r"^[A-Za-z0-9._-]+$")
API_VERSION = "deploy.labweaver.io/sprint2-bundle-manifest/v1"
JETSTREAM_CONSUMER_SECRETS = {
    "control-service-secrets",
    "agent-service-secrets",
    "environment-service-secrets",
    "evaluation-service-secrets",
}
NATS_USER_JWT = re.compile(
    rb"-----BEGIN NATS USER JWT-----\s+([A-Za-z0-9._-]+)\s+------END NATS USER JWT------"
)


class BundleError(Exception):
    """A stable fail-closed bundle diagnostic."""


def _load_manifest(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise BundleError("LW_SPRINT2_BUNDLE_MANIFEST_INVALID") from error
    if not isinstance(value, dict) or set(value) != {
        "apiVersion",
        "namespace",
        "configMaps",
        "secrets",
    }:
        raise BundleError("LW_SPRINT2_BUNDLE_MANIFEST_INVALID")
    if value["apiVersion"] != API_VERSION or not DNS_LABEL.fullmatch(value["namespace"]):
        raise BundleError("LW_SPRINT2_BUNDLE_MANIFEST_INVALID")
    for group in ("configMaps", "secrets"):
        objects = value[group]
        if not isinstance(objects, dict) or not objects:
            raise BundleError("LW_SPRINT2_BUNDLE_MANIFEST_INVALID")
        for name, keys in objects.items():
            if (
                not isinstance(name, str)
                or not DNS_LABEL.fullmatch(name)
                or not isinstance(keys, list)
                or not keys
                or len(keys) != len(set(keys))
                or any(not isinstance(key, str) or not DATA_KEY.fullmatch(key) for key in keys)
            ):
                raise BundleError("LW_SPRINT2_BUNDLE_MANIFEST_INVALID")
    if set(value["configMaps"]) & set(value["secrets"]):
        raise BundleError("LW_SPRINT2_BUNDLE_MANIFEST_INVALID")
    return value


def _read_exact_files(directory: Path, keys: list[str], binary: bool) -> dict[str, str]:
    if not directory.is_dir() or directory.is_symlink():
        raise BundleError("LW_SPRINT2_BUNDLE_INPUT_INVALID")
    entries = {entry.name for entry in directory.iterdir()}
    if entries != set(keys):
        raise BundleError("LW_SPRINT2_BUNDLE_INPUT_INCOMPLETE")
    result: dict[str, str] = {}
    for key in sorted(keys):
        path = directory / key
        if path.is_symlink() or not path.is_file():
            raise BundleError("LW_SPRINT2_BUNDLE_INPUT_INVALID")
        size = path.stat().st_size
        if size == 0 or size > MAX_FILE_BYTES:
            raise BundleError("LW_SPRINT2_BUNDLE_INPUT_INVALID")
        payload = path.read_bytes()
        if binary:
            result[key] = base64.b64encode(payload).decode("ascii")
        else:
            try:
                result[key] = payload.decode("utf-8")
            except UnicodeDecodeError as error:
                raise BundleError("LW_SPRINT2_BUNDLE_INPUT_INVALID") from error
    return result


def _validate_jetstream_ack_permission(secret_name: str, directory: Path) -> None:
    if secret_name not in JETSTREAM_CONSUMER_SECRETS:
        return
    try:
        credentials = (directory / "nats.creds").read_bytes()
        match = NATS_USER_JWT.search(credentials)
        if match is None:
            raise ValueError
        encoded_payload = match.group(1).split(b".")[1]
        encoded_payload += b"=" * (-len(encoded_payload) % 4)
        claims = json.loads(base64.urlsafe_b64decode(encoded_payload))
        publish_allow = claims["nats"]["pub"]["allow"]
    except (OSError, ValueError, IndexError, KeyError, TypeError, json.JSONDecodeError) as error:
        raise BundleError("LW_SPRINT2_NATS_CREDENTIALS_INVALID") from error
    if not isinstance(publish_allow, list) or "$JS.ACK.>" not in publish_allow:
        raise BundleError("LW_SPRINT2_NATS_ACK_PERMISSION_REQUIRED")


def render_bundle(manifest_path: Path, input_root: Path) -> bytes:
    manifest = _load_manifest(manifest_path)
    if not input_root.is_dir() or input_root.is_symlink():
        raise BundleError("LW_SPRINT2_BUNDLE_INPUT_INVALID")
    expected_roots = {"configmaps", "secrets"}
    if {entry.name for entry in input_root.iterdir()} != expected_roots:
        raise BundleError("LW_SPRINT2_BUNDLE_INPUT_INCOMPLETE")

    objects: list[dict[str, Any]] = []
    namespace = manifest["namespace"]
    for kind, manifest_key, directory_name, data_field, binary in (
        ("ConfigMap", "configMaps", "configmaps", "data", False),
        ("Secret", "secrets", "secrets", "data", True),
    ):
        group_root = input_root / directory_name
        if not group_root.is_dir() or group_root.is_symlink():
            raise BundleError("LW_SPRINT2_BUNDLE_INPUT_INVALID")
        expected_names = set(manifest[manifest_key])
        if {entry.name for entry in group_root.iterdir()} != expected_names:
            raise BundleError("LW_SPRINT2_BUNDLE_INPUT_INCOMPLETE")
        for name in sorted(expected_names):
            if kind == "Secret":
                _validate_jetstream_ack_permission(name, group_root / name)
            data = _read_exact_files(group_root / name, manifest[manifest_key][name], binary)
            value: dict[str, Any] = {
                "apiVersion": "v1",
                "kind": kind,
                "metadata": {
                    "name": name,
                    "namespace": namespace,
                    "labels": {
                        "app.kubernetes.io/part-of": "labweaver",
                        "labweaver.io/sprint": "sprint2",
                    },
                },
                data_field: data,
            }
            if kind == "Secret":
                value["type"] = "Opaque"
            objects.append(value)
    return b"".join(
        b"---\n" + json.dumps(value, sort_keys=True, separators=(",", ":")).encode("utf-8") + b"\n"
        for value in objects
    )


def _write_exclusive(path: Path, payload: bytes) -> None:
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    descriptor = os.open(path, flags, 0o600)
    try:
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(payload)
            handle.flush()
            os.fsync(handle.fileno())
    except BaseException:
        path.unlink(missing_ok=True)
        raise


def _require_private_path(path: Path) -> None:
    if ".private" not in path.parts:
        raise BundleError("LW_SPRINT2_BUNDLE_PRIVATE_PATH_REQUIRED")


def main() -> int:
    root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", type=Path, default=root / "deploy/config/sprint2-bundle-manifest.json")
    parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    arguments = parser.parse_args()
    try:
        input_root = arguments.input.resolve()
        output = arguments.output.resolve()
        _require_private_path(input_root)
        _require_private_path(output)
        payload = render_bundle(arguments.manifest.resolve(), input_root)
        _write_exclusive(output, payload)
    except (BundleError, OSError) as error:
        diagnostic = str(error) if isinstance(error, BundleError) else "LW_SPRINT2_BUNDLE_WRITE_FAILED"
        print(diagnostic, file=sys.stderr)
        return 1
    print(json.dumps({"sha256": hashlib.sha256(payload).hexdigest(), "objects": payload.count(b"---\n")}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
