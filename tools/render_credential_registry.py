#!/usr/bin/env python3
"""Render a sanitized credential registry for the Issue #152 rotation.

The registry records only locators, file modes, sizes, SHA-256 hashes and run
identity for every file under a private rotation directory. File contents are
never read into the output, so the registry itself can be referenced from
Issues, reports and repositories without leaking credential material.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

MAX_FILE_BYTES = 1024 * 1024
RUN_ID = re.compile(r"^[a-z0-9][a-z0-9-]{2,62}$")
SOURCE_COMMIT = re.compile(r"^[0-9a-f]{40,64}$")
SCHEMA_VERSION = "deploy.labweaver.io/credential-registry/v1"


class RegistryError(Exception):
    """A stable fail-closed credential-registry diagnostic."""


def _require_private_path(path: Path) -> Path:
    resolved = path.resolve()
    if not any(part in {".private", "private"} for part in resolved.parts):
        raise RegistryError("LW_CREDENTIAL_REGISTRY_PRIVATE_PATH_REQUIRED")
    return resolved


def _scan_entry(root: Path, path: Path) -> dict[str, Any]:
    if path.is_symlink():
        raise RegistryError("LW_CREDENTIAL_REGISTRY_INPUT_INVALID")
    stat = path.stat()
    if not path.is_file() or stat.st_size == 0 or stat.st_size > MAX_FILE_BYTES:
        raise RegistryError("LW_CREDENTIAL_REGISTRY_INPUT_INVALID")
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        while chunk := handle.read(65536):
            digest.update(chunk)
    return {
        "path": path.relative_to(root).as_posix(),
        "size": stat.st_size,
        "mode": format(stat.st_mode & 0o777, "04o"),
        "sha256": digest.hexdigest(),
    }


def render_registry(
    input_root: Path, run_id: str, source_commit: str, generated_at: str | None = None
) -> dict[str, Any]:
    if not RUN_ID.fullmatch(run_id):
        raise RegistryError("LW_CREDENTIAL_REGISTRY_RUN_ID_INVALID")
    if not SOURCE_COMMIT.fullmatch(source_commit):
        raise RegistryError("LW_CREDENTIAL_REGISTRY_COMMIT_INVALID")
    if not input_root.is_dir() or input_root.is_symlink():
        raise RegistryError("LW_CREDENTIAL_REGISTRY_INPUT_INVALID")
    entries = [
        _scan_entry(input_root, path)
        for path in sorted(input_root.rglob("*"))
        if path.is_file()
    ]
    if not entries:
        raise RegistryError("LW_CREDENTIAL_REGISTRY_INPUT_INVALID")
    for entry in entries:
        if path_is_hidden(entry["path"]):
            raise RegistryError("LW_CREDENTIAL_REGISTRY_INPUT_INVALID")
    return {
        "schema_version": SCHEMA_VERSION,
        "run_id": run_id,
        "source_commit": source_commit,
        "generated_at": generated_at
        or datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "entries": entries,
        "entry_count": len(entries),
        "total_bytes": sum(entry["size"] for entry in entries),
    }


def path_is_hidden(relative: str) -> bool:
    return any(part.startswith(".") for part in Path(relative).parts)


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


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--run-id", required=True)
    parser.add_argument("--source-commit", required=True)
    arguments = parser.parse_args()
    try:
        input_root = _require_private_path(arguments.input)
        output = _require_private_path(arguments.output)
        registry = render_registry(input_root, arguments.run_id, arguments.source_commit)
        _write_exclusive(output, (json.dumps(registry, sort_keys=True, indent=2) + "\n").encode())
    except (RegistryError, OSError) as error:
        diagnostic = (
            str(error) if isinstance(error, RegistryError) else "LW_CREDENTIAL_REGISTRY_WRITE_FAILED"
        )
        print(diagnostic, file=sys.stderr)
        return 1
    print(
        json.dumps(
            {
                "run_id": registry["run_id"],
                "entry_count": registry["entry_count"],
                "registry_sha256": hashlib.sha256(
                    (json.dumps(registry, sort_keys=True, indent=2) + "\n").encode()
                ).hexdigest(),
            },
            sort_keys=True,
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
