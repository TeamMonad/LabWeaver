#!/usr/bin/env python3
"""Stamp Helm's rendered roots with the authenticated deployment installation ID."""
import os
import re
import sys

import yaml


def stamp(documents, installation):
    if not re.fullmatch(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}", installation):
        raise ValueError("INFRASTRUCTURE_INSTALLATION_ID_REQUIRED")
    result = []
    for document in documents:
        if document is None:
            continue
        if not isinstance(document, dict) or not document.get("apiVersion") or not document.get("kind"):
            raise ValueError("INFRASTRUCTURE_RENDERED_OBJECT_INVALID")
        metadata = document.setdefault("metadata", {})
        if not metadata.get("name"):
            raise ValueError("INFRASTRUCTURE_RENDERED_OBJECT_NAME_REQUIRED")
        annotations = metadata.setdefault("annotations", {})
        existing = annotations.get("labweaver.io/installation-id")
        if existing is not None and existing != installation:
            raise ValueError("INFRASTRUCTURE_RENDERED_INSTALLATION_CONFLICT")
        annotations["labweaver.io/installation-id"] = installation
        result.append(document)
    return result


if __name__ == "__main__":
    try:
        yaml.safe_dump_all(stamp(yaml.safe_load_all(sys.stdin), os.environ.get("LW_BUNDLE_INSTALLATION_ID", "")), sys.stdout)
    except (ValueError, yaml.YAMLError) as error:
        print(str(error), file=sys.stderr)
        raise SystemExit(1) from error
