#!/usr/bin/env python3
"""Add the CRI-standard annotation names gVisor reads to a CRI-O container bundle.

CRI-O writes `io.kubernetes.cri-o.ContainerType`, `...SandboxID`, `...SandboxName`,
`...Namespace` and `...ContainerName`; gVisor's OCI runtime reads the CRI-standard
`io.kubernetes.cri.container-type`, `io.kubernetes.cri.sandbox-id`,
`io.kubernetes.cri.sandbox-name`, `io.kubernetes.cri.sandbox-namespace` and
`io.kubernetes.cri.container-name`. Without the translation `runsc` treats a Pod
sandbox as an ordinary container and never boots it.

Existing standard annotations are never overwritten, and the bundle is only
rewritten when something actually changes.

Usage: translate-cri-annotations.py <bundle>/config.json
"""

from __future__ import annotations

import json
import os
import sys

# CRI-O annotation -> CRI-standard annotation read by gVisor.
TRANSLATIONS = {
    "io.kubernetes.cri-o.ContainerType": "io.kubernetes.cri.container-type",
    "io.kubernetes.cri-o.SandboxID": "io.kubernetes.cri.sandbox-id",
    "io.kubernetes.cri-o.SandboxName": "io.kubernetes.cri.sandbox-name",
    "io.kubernetes.cri-o.Namespace": "io.kubernetes.cri.sandbox-namespace",
    "io.kubernetes.cri-o.ContainerName": "io.kubernetes.cri.container-name",
}


def translate(path: str) -> int:
    with open(path, encoding="utf-8") as handle:
        spec = json.load(handle)
    if not isinstance(spec, dict):
        raise ValueError("bundle spec is not an object")
    annotations = spec.get("annotations")
    if annotations is None:
        return 0
    if not isinstance(annotations, dict):
        raise ValueError("bundle annotations are not an object")
    changed = False
    for source, target in TRANSLATIONS.items():
        value = annotations.get(source)
        if isinstance(value, str) and value and target not in annotations:
            annotations[target] = value
            changed = True
    if not changed:
        return 0
    spec["annotations"] = annotations
    temporary = f"{path}.labweaver-sandbox"
    with open(temporary, "w", encoding="utf-8") as handle:
        json.dump(spec, handle)
    os.chmod(temporary, 0o644)
    os.replace(temporary, path)
    return 0


def main() -> int:
    if len(sys.argv) != 2:
        sys.stderr.write("usage: translate-cri-annotations.py <bundle>/config.json\n")
        return 2
    try:
        return translate(sys.argv[1])
    except (OSError, ValueError) as error:
        sys.stderr.write(f"cri annotation translation failed: {error}\n")
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
