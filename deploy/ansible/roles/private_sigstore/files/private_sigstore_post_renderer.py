#!/usr/bin/env python3
"""Apply reviewed scaffold corrections that upstream values cannot express."""

from __future__ import annotations

import sys

import yaml


documents = list(yaml.safe_load_all(sys.stdin))
patched = 0
for document in documents:
    if not isinstance(document, dict):
        continue
    metadata = document.get("metadata", {})
    if document.get("kind") != "Deployment" or metadata.get("name") != "rekor-server":
        continue
    pod_spec = document["spec"]["template"]["spec"]
    for volume in pod_spec.get("volumes", []):
        secret = volume.get("secret")
        if secret and secret.get("secretName") == "private-sigstore-rekor-signer":
            secret["defaultMode"] = 0o600
            patched += 1

if patched != 1:
    raise SystemExit("SIGSTORE_POST_RENDERER_SIGNER_VOLUME_INVALID")

yaml.safe_dump_all(documents, sys.stdout, explicit_start=True, sort_keys=False)
