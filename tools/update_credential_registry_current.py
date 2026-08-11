#!/usr/bin/env python3
"""Atomically adopt and verify the controller's current credential registry.

The registry is intentionally a locator boundary, not another credential
store.  ``current`` contains only controlled links and ``current.sha256``
contains relative locators plus content hashes.  Secret material is never
loaded into the manifest or printed by this tool.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import secrets
import stat
import sys
from pathlib import Path
from typing import Iterable


RUN_ID = re.compile(r"^[a-z0-9][a-z0-9-]{2,62}$")
SOURCE_COMMIT = re.compile(r"^[0-9a-f]{40,64}$")
NAMES = (
    "nats-authority-source",
    "deployment-bundle",
    "resource-profile",
    "replay-authentication",
    "controller-identity",
)


class RegistryError(Exception):
    """A stable fail-closed registry diagnostic."""


def _mode(path: Path) -> int:
    return stat.S_IMODE(path.stat().st_mode)


def _owner_is_controlled(path: Path, *, require_root: bool) -> bool:
    item = path.stat()
    if require_root:
        return item.st_uid == 0 and item.st_gid == 0
    return item.st_uid == os.getuid() and item.st_gid == os.getgid()


def _private_path(path: Path) -> Path:
    resolved = path.resolve()
    if ".private" not in resolved.parts:
        raise RegistryError("LW_CREDENTIAL_REGISTRY_PRIVATE_PATH_REQUIRED")
    return resolved


def _validate_tree(path: Path, *, require_root: bool) -> None:
    if path.is_symlink() or not path.is_dir():
        raise RegistryError("LW_CREDENTIAL_REGISTRY_TARGET_INVALID")
    if not _owner_is_controlled(path, require_root=require_root) or _mode(path) & 0o077:
        raise RegistryError("LW_CREDENTIAL_REGISTRY_TARGET_PERMISSIONS_INVALID")
    for child in sorted(path.rglob("*")):
        if child.is_symlink():
            raise RegistryError("LW_CREDENTIAL_REGISTRY_TARGET_INVALID")
        if not _owner_is_controlled(child, require_root=require_root):
            raise RegistryError("LW_CREDENTIAL_REGISTRY_TARGET_PERMISSIONS_INVALID")
        if child.is_dir():
            if _mode(child) & 0o077:
                raise RegistryError("LW_CREDENTIAL_REGISTRY_TARGET_PERMISSIONS_INVALID")
        elif child.is_file():
            if _mode(child) & 0o077:
                raise RegistryError("LW_CREDENTIAL_REGISTRY_TARGET_PERMISSIONS_INVALID")
        else:
            raise RegistryError("LW_CREDENTIAL_REGISTRY_TARGET_INVALID")


def _validate_target(path: Path, *, require_root: bool) -> Path:
    if path.is_symlink():
        raise RegistryError("LW_CREDENTIAL_REGISTRY_TARGET_INVALID")
    resolved = _private_path(path)
    if not resolved.exists() or resolved.is_symlink():
        raise RegistryError("LW_CREDENTIAL_REGISTRY_TARGET_INVALID")
    if resolved.is_dir():
        _validate_tree(resolved, require_root=require_root)
    elif resolved.is_file():
        if not _owner_is_controlled(resolved, require_root=require_root) or _mode(resolved) & 0o077:
            raise RegistryError("LW_CREDENTIAL_REGISTRY_TARGET_PERMISSIONS_INVALID")
    else:
        raise RegistryError("LW_CREDENTIAL_REGISTRY_TARGET_INVALID")
    return resolved


def _file_digest(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        while chunk := handle.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def _content_digest(path: Path) -> str:
    """Hash a file or a complete private tree deterministically."""

    if path.is_file():
        return _file_digest(path)
    digest = hashlib.sha256(b"labweaver-credential-registry-tree-v1\n")
    entries: list[tuple[str, str, str]] = []
    for child in sorted(path.rglob("*")):
        relative = child.relative_to(path).as_posix()
        if child.is_dir():
            entries.append((f"d/{relative}", format(_mode(child), "04o"), ""))
        elif child.is_file():
            entries.append((f"f/{relative}", format(_mode(child), "04o"), _file_digest(child)))
        else:
            raise RegistryError("LW_CREDENTIAL_REGISTRY_TARGET_INVALID")
    for relative, mode, child_digest in entries:
        digest.update(f"{relative}\0{mode}\0{child_digest}\n".encode("utf-8"))
    return digest.hexdigest()


def _manifest(entries: Iterable[tuple[str, str]]) -> bytes:
    lines = [f"current/{name} sha256:{digest}" for name, digest in entries]
    return ("\n".join(lines) + "\n").encode("utf-8")


def _write_new(path: Path, payload: bytes, mode: int) -> None:
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, mode)
    try:
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(payload)
            handle.flush()
            os.fsync(handle.fileno())
    except BaseException:
        path.unlink(missing_ok=True)
        raise


def _registry_root(path: Path, *, require_root: bool) -> Path:
    if path.is_symlink():
        raise RegistryError("LW_CREDENTIAL_REGISTRY_ROOT_INVALID")
    path.mkdir(mode=0o700, parents=True, exist_ok=True)
    if not path.is_dir() or not _owner_is_controlled(path, require_root=require_root) or _mode(path) != 0o700:
        raise RegistryError("LW_CREDENTIAL_REGISTRY_ROOT_PERMISSIONS_INVALID")
    versions = path / "versions"
    versions.mkdir(mode=0o700, exist_ok=True)
    if not _owner_is_controlled(versions, require_root=require_root) or _mode(versions) != 0o700:
        raise RegistryError("LW_CREDENTIAL_REGISTRY_ROOT_PERMISSIONS_INVALID")
    return path


def _verify_current(
    root: Path,
    *,
    expected_run_id: str | None = None,
    require_root: bool,
) -> dict[str, str | int]:
    current = root / "current"
    manifest_path = root / "current.sha256"
    if not current.is_symlink() or not current.is_dir() or not manifest_path.is_file():
        raise RegistryError("LW_CREDENTIAL_REGISTRY_CURRENT_MISSING")
    if not _owner_is_controlled(root, require_root=require_root) or _mode(root) != 0o700:
        raise RegistryError("LW_CREDENTIAL_REGISTRY_ROOT_PERMISSIONS_INVALID")
    if not _owner_is_controlled(manifest_path, require_root=require_root) or _mode(manifest_path) != 0o600:
        raise RegistryError("LW_CREDENTIAL_REGISTRY_MANIFEST_PERMISSIONS_INVALID")
    target_dir = current.resolve()
    if target_dir.parent.parent != (root / "versions").resolve():
        raise RegistryError("LW_CREDENTIAL_REGISTRY_CURRENT_INVALID")
    if not _owner_is_controlled(target_dir, require_root=require_root) or _mode(target_dir) != 0o700:
        raise RegistryError("LW_CREDENTIAL_REGISTRY_CURRENT_PERMISSIONS_INVALID")
    run_id = target_dir.parent.name
    if expected_run_id is not None and run_id != expected_run_id:
        raise RegistryError("LW_CREDENTIAL_REGISTRY_RUN_ID_MISMATCH")
    if set(p.name for p in target_dir.iterdir()) != set(NAMES):
        raise RegistryError("LW_CREDENTIAL_REGISTRY_CURRENT_INVALID")
    parsed: dict[str, str] = {}
    for line in manifest_path.read_text(encoding="utf-8").splitlines():
        parts = line.split()
        if len(parts) != 2 or not parts[0].startswith("current/") or not parts[1].startswith("sha256:"):
            raise RegistryError("LW_CREDENTIAL_REGISTRY_MANIFEST_INVALID")
        name = parts[0].removeprefix("current/")
        digest = parts[1].removeprefix("sha256:")
        if name not in NAMES or not re.fullmatch(r"[0-9a-f]{64}", digest) or name in parsed:
            raise RegistryError("LW_CREDENTIAL_REGISTRY_MANIFEST_INVALID")
        parsed[name] = digest
    if set(parsed) != set(NAMES):
        raise RegistryError("LW_CREDENTIAL_REGISTRY_MANIFEST_INVALID")
    for name in NAMES:
        link = target_dir / name
        if not link.is_symlink():
            raise RegistryError("LW_CREDENTIAL_REGISTRY_CURRENT_INVALID")
        target = _validate_target(link.resolve(), require_root=require_root)
        if _content_digest(target) != parsed[name]:
            raise RegistryError("LW_CREDENTIAL_REGISTRY_HASH_MISMATCH")
    return {
        "run_id": run_id,
        "entry_count": len(parsed),
        "manifest_sha256": _file_digest(manifest_path),
    }


def adopt(
    *,
    root: Path,
    run_id: str,
    source_commit: str,
    targets: dict[str, Path],
    require_root: bool = True,
) -> dict[str, str | int]:
    if not RUN_ID.fullmatch(run_id):
        raise RegistryError("LW_CREDENTIAL_REGISTRY_RUN_ID_INVALID")
    if not SOURCE_COMMIT.fullmatch(source_commit):
        raise RegistryError("LW_CREDENTIAL_REGISTRY_COMMIT_INVALID")
    if set(targets) != set(NAMES):
        raise RegistryError("LW_CREDENTIAL_REGISTRY_TARGET_SET_INVALID")
    root = _registry_root(root, require_root=require_root)
    resolved = {name: _validate_target(path, require_root=require_root) for name, path in targets.items()}
    version = root / "versions" / run_id
    if version.exists() or version.is_symlink():
        raise RegistryError("LW_CREDENTIAL_REGISTRY_RUN_EXISTS")
    version.mkdir(mode=0o700)
    links = version / "current"
    links.mkdir(mode=0o700)
    try:
        entries = [(name, _content_digest(resolved[name])) for name in NAMES]
        for name in NAMES:
            (links / name).symlink_to(resolved[name])
        staged_current = root / f".current.{secrets.token_hex(8)}.new"
        staged_manifest = root / f".current.sha256.{secrets.token_hex(8)}.new"
        staged_current.symlink_to(Path("versions") / run_id / "current")
        os.replace(staged_current, root / "current")
        _write_new(staged_manifest, _manifest(entries), 0o600)
        os.replace(staged_manifest, root / "current.sha256")
    except BaseException:
        for staged in root.glob(".current*.new"):
            staged.unlink(missing_ok=True)
        raise
    return _verify_current(root, expected_run_id=run_id, require_root=require_root) | {
        "source_commit": source_commit
    }


def _arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--registry-root", type=Path, required=True)
    parser.add_argument("--run-id", required=True)
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--nats-authority-source", type=Path, required=True)
    parser.add_argument("--deployment-bundle", type=Path, required=True)
    parser.add_argument("--resource-profile", type=Path, required=True)
    parser.add_argument("--replay-authentication", type=Path, required=True)
    parser.add_argument("--controller-identity", type=Path, required=True)
    parser.add_argument("--verify", action="store_true")
    return parser.parse_args()


def main() -> int:
    arguments = _arguments()
    try:
        if arguments.verify:
            result = _verify_current(
                arguments.registry_root,
                expected_run_id=arguments.run_id,
                require_root=True,
            )
        else:
            result = adopt(
                root=arguments.registry_root,
                run_id=arguments.run_id,
                source_commit=arguments.source_commit,
                targets={
                    "nats-authority-source": arguments.nats_authority_source,
                    "deployment-bundle": arguments.deployment_bundle,
                    "resource-profile": arguments.resource_profile,
                    "replay-authentication": arguments.replay_authentication,
                    "controller-identity": arguments.controller_identity,
                },
            )
    except (OSError, UnicodeError, RegistryError) as error:
        print(str(error) if isinstance(error, RegistryError) else "LW_CREDENTIAL_REGISTRY_OPERATION_FAILED", file=sys.stderr)
        return 1
    print(json.dumps(result, sort_keys=True, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
