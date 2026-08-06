#!/usr/bin/env python3
"""Render one root-only Resource acceptance profile from reviewed private inputs."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import tempfile
import uuid
from pathlib import Path
from typing import Any

from validate_resource_acceptance_profile import ProfileError, validate


class RenderError(Exception):
    """Stable non-secret renderer diagnostic."""


def private(path: Path, code: str) -> Path:
    resolved = path.resolve()
    if ".private" not in resolved.parts or not resolved.is_file():
        raise RenderError(code)
    return resolved


def document(path: Path, code: str) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise RenderError(code) from error
    if not isinstance(value, dict):
        raise RenderError(code)
    return value


def render(seed: dict[str, Any], config: dict[str, Any], material: Path) -> dict[str, Any]:
    memberships = seed.get("courseMemberships")
    if not isinstance(memberships, list) or len(memberships) != 3:
        raise RenderError("LW_RESOURCE_PROFILE_RENDER_ACCESS_SEED_INVALID")
    course_ids = {item.get("courseId") for item in memberships if isinstance(item, dict)}
    if len(course_ids) != 1 or not isinstance(next(iter(course_ids)), str):
        raise RenderError("LW_RESOURCE_PROFILE_RENDER_ACCESS_SEED_INVALID")
    course_id = next(iter(course_ids))
    try:
        if uuid.UUID(course_id).version != 7:
            raise ValueError("not v7")
    except ValueError as error:
        raise RenderError("LW_RESOURCE_PROFILE_RENDER_ACCESS_SEED_INVALID") from error
    for key in ("runtimeKind", "resources", "durationSeconds", "configurationSha256", "projectId", "providerBinding", "policy"):
        if key not in config:
            raise RenderError("LW_RESOURCE_PROFILE_RENDER_CONFIGURATION_INVALID")
    if config["runtimeKind"] not in {"container", "virtual_machine"}:
        raise RenderError("LW_RESOURCE_PROFILE_RENDER_CONFIGURATION_INVALID")
    if not isinstance(config["resources"], dict) or not isinstance(config["durationSeconds"], int):
        raise RenderError("LW_RESOURCE_PROFILE_RENDER_CONFIGURATION_INVALID")
    if not isinstance(config["configurationSha256"], str) or len(config["configurationSha256"]) != 64:
        raise RenderError("LW_RESOURCE_PROFILE_RENDER_CONFIGURATION_INVALID")
    if not isinstance(config["projectId"], str) or not isinstance(config["providerBinding"], str):
        raise RenderError("LW_RESOURCE_PROFILE_RENDER_CONFIGURATION_INVALID")
    if not isinstance(config["policy"], dict):
        raise RenderError("LW_RESOURCE_PROFILE_RENDER_CONFIGURATION_INVALID")
    material_meta = config.get("material")
    if not isinstance(material_meta, dict) or not isinstance(material_meta.get("description"), str) or not isinstance(material_meta.get("mediaType"), str):
        raise RenderError("LW_RESOURCE_PROFILE_RENDER_CONFIGURATION_INVALID")
    profile = {
        "apiVersion": "deploy.labweaver.io/resource-acceptance-profile/v1",
        "courseId": course_id,
        "runtimeKind": config["runtimeKind"],
        "resources": config["resources"],
        "durationSeconds": config["durationSeconds"],
        "material": {
            "description": material_meta["description"],
            "descriptionSha256": hashlib.sha256(material_meta["description"].encode()).hexdigest(),
        },
        "configurationSha256": config["configurationSha256"],
        "replay": {
            "projectId": config["projectId"],
            "providerBinding": config["providerBinding"],
            "materialFile": {
                "relativePath": material.name,
                "mediaType": material_meta["mediaType"],
                "sha256": hashlib.sha256(material.read_bytes()).hexdigest(),
            },
            "policy": config["policy"],
        },
        "courseMemberships": memberships,
    }
    try:
        validate(profile, seed)
    except ProfileError as error:
        raise RenderError(str(error)) from error
    return profile


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--access-seed", type=Path, required=True)
    parser.add_argument("--configuration", type=Path, required=True)
    parser.add_argument("--material", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    try:
        seed = document(private(args.access_seed, "LW_RESOURCE_PROFILE_RENDER_ACCESS_SEED_INVALID"), "LW_RESOURCE_PROFILE_RENDER_ACCESS_SEED_INVALID")
        configuration = document(private(args.configuration, "LW_RESOURCE_PROFILE_RENDER_CONFIGURATION_INVALID"), "LW_RESOURCE_PROFILE_RENDER_CONFIGURATION_INVALID")
        material = private(args.material, "LW_RESOURCE_PROFILE_RENDER_MATERIAL_INVALID")
        output = args.output.resolve()
        if ".private" not in output.parts:
            raise RenderError("LW_RESOURCE_PROFILE_RENDER_OUTPUT_INVALID")
        output.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        profile = render(seed, configuration, material)
        descriptor, temporary = tempfile.mkstemp(prefix=".resource-profile-", dir=output.parent)
        try:
            with os.fdopen(descriptor, "w", encoding="utf-8") as handle:
                json.dump(profile, handle, sort_keys=True, separators=(",", ":"))
                handle.write("\n")
            os.chmod(temporary, 0o600)
            os.replace(temporary, output)
        finally:
            if os.path.exists(temporary):
                os.unlink(temporary)
    except (RenderError, OSError) as error:
        raise SystemExit(str(error)) from error
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
