#!/usr/bin/env python3
"""Fail-closed inspection for a disk that may be destructively formatted."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
from typing import Any


class UnsafeStorage(ValueError):
    """The requested disk is not safe to format."""


def _flatten(nodes: list[dict[str, Any]]) -> list[dict[str, Any]]:
    flattened: list[dict[str, Any]] = []
    for node in nodes:
        flattened.append(node)
        flattened.extend(_flatten(node.get("children", [])))
    return flattened


def validate(
    blockdevices: list[dict[str, Any]], device: str, expected_wwn: str, expected_size: int,
    root_source: str, holders: list[str],
) -> dict[str, Any]:
    canonical = os.path.realpath(device)
    matches = [node for node in _flatten(blockdevices) if os.path.realpath(node.get("path", "")) == canonical]
    if len(matches) != 1:
        raise UnsafeStorage("STORAGE_DEVICE_IDENTITY_AMBIGUOUS")
    node = matches[0]
    if node.get("type") != "disk":
        raise UnsafeStorage("STORAGE_DEVICE_NOT_WHOLE_DISK")
    if node.get("fstype"):
        raise UnsafeStorage("STORAGE_DEVICE_HAS_FILESYSTEM")
    if node.get("children") or node.get("pkname"):
        raise UnsafeStorage("STORAGE_DEVICE_HAS_PARTITIONS_OR_STACK")
    if node.get("wwn") != expected_wwn:
        raise UnsafeStorage("STORAGE_DEVICE_WWN_MISMATCH")
    if int(node.get("size", 0)) != expected_size:
        raise UnsafeStorage("STORAGE_DEVICE_SIZE_MISMATCH")
    mounts = node.get("mountpoints") or []
    if any(mounts):
        raise UnsafeStorage("STORAGE_DEVICE_MOUNTED")
    if holders:
        raise UnsafeStorage("STORAGE_DEVICE_HAS_HOLDERS")
    if root_source and os.path.realpath(root_source) == canonical:
        raise UnsafeStorage("STORAGE_DEVICE_IS_ROOT")
    return {
        "device": canonical,
        "wwn": expected_wwn,
        "size_bytes": expected_size,
        "safe_to_format": True,
    }


def _output(command: list[str]) -> str:
    return subprocess.run(command, check=True, text=True, capture_output=True).stdout


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--device", required=True)
    parser.add_argument("--expected-wwn", required=True)
    parser.add_argument("--expected-size-bytes", required=True, type=int)
    args = parser.parse_args()
    try:
        blockdevices = json.loads(_output([
            "lsblk", "--json", "--bytes",
            "--output", "PATH,TYPE,FSTYPE,WWN,SIZE,PKNAME,MOUNTPOINTS",
        ]))["blockdevices"]
        root_source = _output(["findmnt", "--noheadings", "--output", "SOURCE", "/"]).strip()
        holders = list((Path("/sys/class/block") / Path(os.path.realpath(args.device)).name / "holders").iterdir())
        result = validate(blockdevices, args.device, args.expected_wwn, args.expected_size_bytes, root_source, [str(item) for item in holders])
    except (UnsafeStorage, subprocess.CalledProcessError, KeyError, OSError, ValueError) as error:
        print(json.dumps({"safe_to_format": False, "diagnostic": str(error)}))
        return 2
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
