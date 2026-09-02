#!/usr/bin/env python3
"""Create stable, root-only inputs for the Resource connected acceptance replay."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import secrets
import tempfile
import time
import uuid
from pathlib import Path
from typing import Any

from validate_resource_acceptance_profile import ProfileError, validate


class PrepareError(Exception):
    """Stable, non-secret acceptance-profile preparation diagnostic."""


def uuid7() -> str:
    milliseconds = time.time_ns() // 1_000_000
    if not 0 <= milliseconds < 1 << 48:
        raise PrepareError("LW_RESOURCE_PROFILE_PREPARE_CLOCK_INVALID")
    value = milliseconds << 80
    value |= 0x7 << 76
    value |= secrets.randbits(12) << 64
    value |= 0b10 << 62
    value |= secrets.randbits(62)
    return str(uuid.UUID(int=value))


def private_file(path: Path, code: str, *, existing: bool = True) -> Path:
    resolved = path.resolve()
    if ".private" not in resolved.parts or (existing and not resolved.is_file()):
        raise PrepareError(code)
    return resolved


def load_json(path: Path, code: str) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise PrepareError(code) from error
    if not isinstance(value, dict):
        raise PrepareError(code)
    return value


def atomic_json(path: Path, value: dict[str, Any]) -> None:
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(prefix=".resource-profile-", dir=path.parent)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as handle:
            json.dump(value, handle, sort_keys=True, separators=(",", ":"))
            handle.write("\n")
        os.chmod(temporary, 0o600)
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def atomic_material(path: Path) -> None:
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(prefix=".resource-material-", dir=path.parent)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as handle:
            handle.write(
                "# Reviewed Resource acceptance work environment\n\n"
                "Generate exactly one work-class container environment for this course. "
                "The environment must provide a non-root writable /workspace and seed "
                "/workspace/README.md with this assignment. Use the approved build pipeline; "
                "do not request GPU, public exposure, privileged execution, or VM runtime.\n"
            )
        os.chmod(temporary, 0o600)
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def build_configuration(seed: dict[str, Any], runtime_config: Path, image_sha256: str, version: str, model: str, binding: str, provider: str) -> dict[str, Any]:
    members = seed.get("courseMemberships")
    if not isinstance(members, list) or len(members) != 3:
        raise PrepareError("LW_RESOURCE_PROFILE_PREPARE_ACCESS_SEED_INVALID")
    course_ids = {member.get("courseId") for member in members if isinstance(member, dict)}
    if len(course_ids) != 1 or not isinstance(next(iter(course_ids)), str):
        raise PrepareError("LW_RESOURCE_PROFILE_PREPARE_ACCESS_SEED_INVALID")
    course_id = next(iter(course_ids))
    try:
        if uuid.UUID(course_id).version != 7:
            raise ValueError("not UUIDv7")
    except ValueError as error:
        raise PrepareError("LW_RESOURCE_PROFILE_PREPARE_ACCESS_SEED_INVALID") from error
    if len(image_sha256) != 64 or any(character not in "0123456789abcdef" for character in image_sha256):
        raise PrepareError("LW_RESOURCE_PROFILE_PREPARE_IMAGE_INVALID")
    if not all(value.strip() for value in (version, model, binding, provider)) or "://" in binding:
        raise PrepareError("LW_RESOURCE_PROFILE_PREPARE_RUNTIME_INVALID")
    runtime_hash = hashlib.sha256(runtime_config.read_bytes()).hexdigest()
    return {
        "runtimeKind": "container",
        "resources": {"cpuMillicores": 1000, "memoryBytes": 2147483648, "storageBytes": 10737418240},
        "durationSeconds": 600,
        "configurationSha256": runtime_hash,
        "projectId": uuid7(),
        "providerBinding": provider,
        "policy": {
            "id": uuid7(), "courseId": course_id, "revision": 1,
            "binding": {
                "runtimeBinding": binding, "model": model, "claudeCodeVersion": version,
                "workerImageSha256": image_sha256, "runtimeConfigSha256": runtime_hash,
                "maxInFlightPerWorker": 1,
            },
            "budget": {
                "maxInputTokens": 100000, "maxOutputTokens": 16000, "maxRequests": 4,
                "maxCostMicrousd": 2000000, "timeoutMilliseconds": 120000,
                "maxTransientRetries": 1, "maxSchemaRepairs": 1,
            },
            "deniedDataClasses": [
                "secret", "token", "private_key", "personally_identifiable_information",
                "unallowlisted_student_submission",
            ],
            "studentContentMode": "manifest_allowlist_only",
            "activatedAt": time.strftime("%Y-%m-%dT%H:%M:%S.000Z", time.gmtime()),
        },
        "material": {
            "description": "Reviewed non-secret Resource acceptance work material generated by the deployment role.",
            "mediaType": "text/markdown",
        },
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--access-seed", type=Path, required=True)
    parser.add_argument("--runtime-configuration", type=Path, required=True)
    parser.add_argument("--agent-image-sha256", required=True)
    parser.add_argument("--claude-code-version", required=True)
    parser.add_argument("--model", required=True)
    parser.add_argument("--runtime-binding", required=True)
    parser.add_argument("--provider-binding", required=True)
    parser.add_argument("--configuration-output", type=Path, required=True)
    parser.add_argument("--material-output", type=Path, required=True)
    arguments = parser.parse_args()
    try:
        seed_path = private_file(arguments.access_seed, "LW_RESOURCE_PROFILE_PREPARE_ACCESS_SEED_INVALID")
        runtime_path = private_file(arguments.runtime_configuration, "LW_RESOURCE_PROFILE_PREPARE_RUNTIME_CONFIG_INVALID")
        configuration_path = private_file(arguments.configuration_output, "LW_RESOURCE_PROFILE_PREPARE_OUTPUT_INVALID", existing=False)
        material_path = private_file(arguments.material_output, "LW_RESOURCE_PROFILE_PREPARE_OUTPUT_INVALID", existing=False)
        if configuration_path.exists() != material_path.exists():
            raise PrepareError("LW_RESOURCE_PROFILE_PREPARE_OUTPUT_CONFLICT")
        seed = load_json(seed_path, "LW_RESOURCE_PROFILE_PREPARE_ACCESS_SEED_INVALID")
        if configuration_path.exists():
            configuration = load_json(configuration_path, "LW_RESOURCE_PROFILE_PREPARE_OUTPUT_INVALID")
            material = material_path.read_bytes()
            if not material:
                raise PrepareError("LW_RESOURCE_PROFILE_PREPARE_OUTPUT_INVALID")
            binding_value = configuration.get("policy", {}).get("binding", {})
            if (
                configuration.get("configurationSha256") != hashlib.sha256(runtime_path.read_bytes()).hexdigest()
                or binding_value.get("workerImageSha256") != arguments.agent_image_sha256
                or binding_value.get("claudeCodeVersion") != arguments.claude_code_version
                or binding_value.get("model") != arguments.model
                or binding_value.get("runtimeBinding") != arguments.runtime_binding
                or configuration.get("providerBinding") != arguments.provider_binding
            ):
                raise PrepareError("LW_RESOURCE_PROFILE_PREPARE_RUNTIME_IDENTITY_DRIFT")
        else:
            configuration = build_configuration(seed, runtime_path, arguments.agent_image_sha256, arguments.claude_code_version, arguments.model, arguments.runtime_binding, arguments.provider_binding)
            atomic_json(configuration_path, configuration)
            atomic_material(material_path)
        profile = {
            "apiVersion": "deploy.labweaver.io/resource-acceptance-profile/v1",
            "courseId": configuration["policy"]["courseId"],
            "runtimeKind": configuration["runtimeKind"],
            "resources": configuration["resources"],
            "durationSeconds": configuration["durationSeconds"],
            "material": {"description": configuration["material"]["description"], "descriptionSha256": hashlib.sha256(configuration["material"]["description"].encode()).hexdigest()},
            "configurationSha256": configuration["configurationSha256"],
            "replay": {"projectId": configuration["projectId"], "providerBinding": configuration["providerBinding"], "materialFile": {"relativePath": material_path.name, "mediaType": configuration["material"]["mediaType"], "sha256": hashlib.sha256(material_path.read_bytes()).hexdigest()}, "policy": configuration["policy"]},
            "courseMemberships": seed["courseMemberships"],
        }
        validate(profile, seed)
    except (OSError, KeyError, PrepareError, ProfileError) as error:
        raise SystemExit(str(error)) from error
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
