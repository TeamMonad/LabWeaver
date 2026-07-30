#!/usr/bin/env python3
"""Issue the bounded Resource NATS user from an existing private NSC store.

The command deliberately requires an operator/account signing key in the
provided store. A service ``.creds`` file or a public operator JWT is not a
signing source and fails with a stable diagnostic.
"""

from __future__ import annotations

import argparse
import json
import os
import stat
import subprocess
from pathlib import Path


IDENTITY = "resource-service"
SUBJECT = "labweaver.resource.lease.verify.v1"


class IssuanceError(RuntimeError):
    """Fail-closed identity issuance error."""


def private_path(path: Path) -> Path:
    resolved = path.resolve()
    if not any(part in {".private", "private"} for part in resolved.parts):
        raise IssuanceError("LW_RESOURCE_NATS_PRIVATE_PATH_REQUIRED")
    if not resolved.parent.is_dir():
        raise IssuanceError("LW_RESOURCE_NATS_PRIVATE_PARENT_MISSING")
    return resolved


def trusted_nsc(path: Path) -> Path:
    resolved = path.resolve(strict=True)
    mode = resolved.stat().st_mode
    if resolved.name != "nsc" or not resolved.is_file() or mode & (stat.S_IWGRP | stat.S_IWOTH):
        raise IssuanceError("LW_RESOURCE_NATS_NSC_INVALID")
    return resolved


def run_nsc(nsc: Path, store: Path, args: list[str], home: Path) -> None:
    env = {"HOME": str(home), "LANG": "C.UTF-8", "LC_ALL": "C.UTF-8", "PATH": "/usr/bin:/bin"}
    result = subprocess.run(
        [str(nsc), "--all-dirs", str(store), *args],
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        check=False,
        text=True,
        timeout=60,
    )
    if result.returncode:
        raise IssuanceError("LW_RESOURCE_NATS_SIGNING_SOURCE_UNAVAILABLE")


def issue(store: Path, nsc: Path, output: Path, valid_days: int) -> dict[str, object]:
    store = store.resolve(strict=True)
    if not store.is_dir():
        raise IssuanceError("LW_RESOURCE_NATS_NSC_STORE_INVALID")
    output = private_path(output)
    if output.exists():
        raise IssuanceError("LW_RESOURCE_NATS_OUTPUT_EXISTS")
    output.mkdir(mode=0o700)
    home = output / "home"
    home.mkdir(mode=0o700)
    credentials = output / "resource-service.nats.creds"
    try:
        run_nsc(nsc, store, ["add", "user", "--account", "WORKLOADS", "--name", IDENTITY, "--expiry", f"{valid_days}d", "--allow-sub", "_INBOX.>", "--allow-sub", SUBJECT, "--allow-pub-response"], home)
        run_nsc(nsc, store, ["generate", "creds", "--account", "WORKLOADS", "--name", IDENTITY, "--output-file", str(credentials)], home)
    except Exception:
        if credentials.exists():
            credentials.unlink()
        raise
    os.chmod(credentials, 0o600)
    (output / "issuance-record.json").write_text(
        json.dumps(
            {
                "schema_version": "resource-nats-identity-issuance.v1",
                "status": "issued",
                "identity": IDENTITY,
                "account": "WORKLOADS",
                "subjects": {"publish": [], "subscribe": ["_INBOX.>", SUBJECT], "response": True},
                "credential_locator": "resource-service.nats.creds",
                "credential_mode": "0600",
                "secret_material_in_record": False,
            },
            indent=2,
        )
        + "\n",
        encoding="utf-8",
    )
    os.chmod(output / "issuance-record.json", 0o600)
    return {"identity": IDENTITY, "account": "WORKLOADS", "credential": str(credentials), "status": "issued"}


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--store", type=Path, required=True)
    parser.add_argument("--nsc", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--valid-days", type=int, default=365)
    args = parser.parse_args()
    try:
        result = issue(args.store, trusted_nsc(args.nsc), args.output, args.valid_days)
    except (OSError, subprocess.SubprocessError, IssuanceError) as error:
        print(str(error), file=os.sys.stderr)
        return 2
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
