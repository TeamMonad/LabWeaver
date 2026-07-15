"""Fail-closed checks for the pinned Private Sigstore Helm render."""

from __future__ import annotations

import re
import sys
from pathlib import Path


def main() -> int:
    rendered = Path(sys.argv[1]).read_text(encoding="utf-8")
    images = re.findall(r"(?m)^\s*image:\s*[\"']?([^\s\"']+)", rendered)
    if not images:
        raise SystemExit("SIGSTORE_RENDER_HAS_NO_IMAGES")
    mutable = [image for image in images if not re.search(r"(?:@|:)sha256:[0-9a-f]{64}$", image)]
    if mutable:
        raise SystemExit("SIGSTORE_RENDER_MUTABLE_IMAGE_FORBIDDEN")
    forbidden = {
        "SIGSTORE_RENDER_NODEPORT_FORBIDDEN": r"(?m)^\s*type:\s*NodePort\s*$",
        "SIGSTORE_RENDER_LOADBALANCER_FORBIDDEN": r"(?m)^\s*type:\s*LoadBalancer\s*$",
        "SIGSTORE_RENDER_PUBLIC_FALLBACK_FORBIDDEN": r"sigstore\.dev",
    }
    for diagnostic, pattern in forbidden.items():
        if re.search(pattern, rendered, re.IGNORECASE):
            raise SystemExit(diagnostic)
    print(f"validated {len(images)} digest-pinned rendered images")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
