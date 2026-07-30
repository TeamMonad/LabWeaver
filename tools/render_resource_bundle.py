#!/usr/bin/env python3
"""Render the Resource Service bundle from exact private input files."""
from __future__ import annotations

import argparse, base64, hashlib, json, os, re, sys
from pathlib import Path
from typing import Any

MAX_FILE_BYTES = 1024 * 1024
API_VERSION = "deploy.labweaver.io/resource-bundle-manifest/v1"
DNS_LABEL = re.compile(r"^[a-z0-9](?:[-a-z0-9]{0,61}[a-z0-9])?$")
DATA_KEY = re.compile(r"^[A-Za-z0-9._-]+$")
JWT = re.compile(rb"-----BEGIN NATS USER JWT-----\s+([A-Za-z0-9._-]+)")

class BundleError(Exception):
    pass

def load_manifest(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise BundleError("LW_RESOURCE_BUNDLE_MANIFEST_INVALID") from error
    if not isinstance(value, dict) or set(value) != {"apiVersion", "namespace", "configMaps", "secrets"}:
        raise BundleError("LW_RESOURCE_BUNDLE_MANIFEST_INVALID")
    if value["apiVersion"] != API_VERSION or not DNS_LABEL.fullmatch(value["namespace"]):
        raise BundleError("LW_RESOURCE_BUNDLE_MANIFEST_INVALID")
    for group in ("configMaps", "secrets"):
        objects = value[group]
        if not isinstance(objects, dict) or not objects:
            raise BundleError("LW_RESOURCE_BUNDLE_MANIFEST_INVALID")
        for name, keys in objects.items():
            if not DNS_LABEL.fullmatch(name) or not isinstance(keys, list) or not keys or len(keys) != len(set(keys)):
                raise BundleError("LW_RESOURCE_BUNDLE_MANIFEST_INVALID")
            if any(not isinstance(key, str) or not DATA_KEY.fullmatch(key) for key in keys):
                raise BundleError("LW_RESOURCE_BUNDLE_MANIFEST_INVALID")
    return value

def read_exact(directory: Path, keys: list[str], binary: bool) -> dict[str, str]:
    if not directory.is_dir() or directory.is_symlink() or {p.name for p in directory.iterdir()} != set(keys):
        raise BundleError("LW_RESOURCE_BUNDLE_INPUT_INCOMPLETE")
    result = {}
    for key in sorted(keys):
        path = directory / key
        if path.is_symlink() or not path.is_file():
            raise BundleError("LW_RESOURCE_BUNDLE_INPUT_INVALID")
        payload = path.read_bytes()
        if not payload or len(payload) > MAX_FILE_BYTES:
            raise BundleError("LW_RESOURCE_BUNDLE_INPUT_INVALID")
        if binary:
            result[key] = base64.b64encode(payload).decode("ascii")
        else:
            try:
                result[key] = payload.decode("utf-8")
            except UnicodeDecodeError as error:
                raise BundleError("LW_RESOURCE_BUNDLE_INPUT_INVALID") from error
    return result

def validate_creds(directory: Path, expected_issuer: str | None) -> None:
    try:
        match = JWT.search((directory / "nats.creds").read_bytes())
        if not match:
            raise ValueError
        part = match.group(1).split(b".")[1]
        part += b"=" * (-len(part) % 4)
        claims = json.loads(base64.urlsafe_b64decode(part))
        nats = claims["nats"]
        issuer = claims["iss"]
        publishes = nats.get("pub", {}).get("allow", [])
        subscribes = nats.get("sub", {}).get("allow", [])
        responses = nats.get("resp", {})
    except (OSError, ValueError, IndexError, KeyError, TypeError, json.JSONDecodeError) as error:
        raise BundleError("LW_RESOURCE_NATS_CREDENTIALS_INVALID") from error
    if expected_issuer and issuer != expected_issuer:
        raise BundleError("LW_RESOURCE_NATS_ISSUER_MISMATCH")
    if publishes not in ([], None) or sorted(subscribes) != ["_INBOX.>", "labweaver.resource.lease.verify.v1"]:
        raise BundleError("LW_RESOURCE_NATS_PERMISSIONS_INVALID")
    if responses.get("max") != 1:
        raise BundleError("LW_RESOURCE_NATS_RESPONSE_PERMISSION_INVALID")

def render(manifest_path: Path, input_root: Path, expected_issuer: str | None) -> bytes:
    manifest = load_manifest(manifest_path)
    if not input_root.is_dir() or input_root.is_symlink() or {p.name for p in input_root.iterdir()} != {"configmaps", "secrets"}:
        raise BundleError("LW_RESOURCE_BUNDLE_INPUT_INCOMPLETE")
    objects = []
    for kind, section, dirname, binary in (("ConfigMap", "configMaps", "configmaps", False), ("Secret", "secrets", "secrets", True)):
        root = input_root / dirname
        names = set(manifest[section])
        if not root.is_dir() or {p.name for p in root.iterdir()} != names:
            raise BundleError("LW_RESOURCE_BUNDLE_INPUT_INCOMPLETE")
        for name in sorted(names):
            if kind == "Secret":
                validate_creds(root / name, expected_issuer)
            value = {"apiVersion":"v1","kind":kind,"metadata":{"name":name,"namespace":manifest["namespace"],"labels":{"app.kubernetes.io/part-of":"labweaver","labweaver.io/sprint":"sprint2"}},"data":read_exact(root / name, manifest[section][name], binary)}
            if kind == "Secret": value["type"] = "Opaque"
            objects.append(value)
    return b"".join(b"---\n" + json.dumps(v, sort_keys=True, separators=(",", ":")).encode() + b"\n" for v in objects)

def main() -> int:
    root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", type=Path, default=root / "deploy/config/resource-bundle-manifest.json")
    parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--expected-issuer")
    args = parser.parse_args()
    try:
        if ".private" not in args.input.resolve().parts or ".private" not in args.output.resolve().parts:
            raise BundleError("LW_RESOURCE_BUNDLE_PRIVATE_PATH_REQUIRED")
        payload = render(args.manifest.resolve(), args.input.resolve(), args.expected_issuer)
        args.output.resolve().parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        fd = os.open(args.output.resolve(), os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, "wb") as handle:
            handle.write(payload); handle.flush(); os.fsync(handle.fileno())
    except (BundleError, OSError) as error:
        print(str(error) if isinstance(error, BundleError) else "LW_RESOURCE_BUNDLE_WRITE_FAILED", file=sys.stderr)
        return 1
    print(json.dumps({"sha256": hashlib.sha256(payload).hexdigest(), "objects": payload.count(b"---\n")}))
    return 0

if __name__ == "__main__": raise SystemExit(main())
