#!/usr/bin/env python3
"""Validate the private, non-secret Resource connected-replay profile."""

from __future__ import annotations

import hashlib
import json
import uuid
from pathlib import Path
from typing import Any


class ProfileError(Exception):
    """Stable fail-closed Resource profile diagnostic."""


API_VERSION = "deploy.labweaver.io/resource-acceptance-profile/v1"
ROLES = {"teacher", "student", "platform_admin"}


def _uuid7(value: object) -> str:
    if not isinstance(value, str):
        raise ProfileError("LW_RESOURCE_ACCEPTANCE_PROFILE_ID_INVALID")
    try:
        parsed = uuid.UUID(value)
    except ValueError as error:
        raise ProfileError("LW_RESOURCE_ACCEPTANCE_PROFILE_ID_INVALID") from error
    if parsed.version != 7:
        raise ProfileError("LW_RESOURCE_ACCEPTANCE_PROFILE_ID_INVALID")
    return str(parsed)


def _sha256(value: object) -> str:
    if not isinstance(value, str) or len(value) != 64:
        raise ProfileError("LW_RESOURCE_ACCEPTANCE_PROFILE_HASH_INVALID")
    try:
        int(value, 16)
    except ValueError as error:
        raise ProfileError("LW_RESOURCE_ACCEPTANCE_PROFILE_HASH_INVALID") from error
    return value


def load(path: Path) -> dict[str, Any]:
    resolved = path.resolve()
    if not any(part in {".private", "private"} for part in resolved.parts):
        raise ProfileError("LW_RESOURCE_ACCEPTANCE_PROFILE_PRIVATE_PATH_REQUIRED")
    try:
        value = json.loads(resolved.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ProfileError("LW_RESOURCE_ACCEPTANCE_PROFILE_UNREADABLE") from error
    if not isinstance(value, dict):
        raise ProfileError("LW_RESOURCE_ACCEPTANCE_PROFILE_INVALID")
    return value


def validate(profile: dict[str, Any], access_seed: dict[str, Any]) -> str:
    if profile.get("apiVersion") != API_VERSION:
        raise ProfileError("LW_RESOURCE_ACCEPTANCE_PROFILE_VERSION_INVALID")
    course_id = _uuid7(profile.get("courseId"))
    if profile.get("runtimeKind") not in {"container", "virtual_machine"}:
        raise ProfileError("LW_RESOURCE_ACCEPTANCE_PROFILE_RUNTIME_INVALID")
    resources = profile.get("resources")
    if not isinstance(resources, dict) or any(
        not isinstance(resources.get(field), int) or resources[field] <= 0
        for field in ("cpuMillicores", "memoryBytes", "storageBytes")
    ):
        raise ProfileError("LW_RESOURCE_ACCEPTANCE_PROFILE_RESOURCES_INVALID")
    if not isinstance(profile.get("durationSeconds"), int) or not 1 <= profile["durationSeconds"] <= 86_400:
        raise ProfileError("LW_RESOURCE_ACCEPTANCE_PROFILE_DURATION_INVALID")
    material = profile.get("material")
    if not isinstance(material, dict) or not isinstance(material.get("description"), str) or not material["description"].strip():
        raise ProfileError("LW_RESOURCE_ACCEPTANCE_PROFILE_MATERIAL_INVALID")
    description_sha256 = _sha256(material.get("descriptionSha256"))
    if hashlib.sha256(material["description"].encode("utf-8")).hexdigest() != description_sha256:
        raise ProfileError("LW_RESOURCE_ACCEPTANCE_PROFILE_MATERIAL_HASH_MISMATCH")
    _sha256(profile.get("configurationSha256"))
    members = profile.get("courseMemberships")
    seed_members = access_seed.get("courseMemberships") if isinstance(access_seed, dict) else None
    if not isinstance(members, list) or not isinstance(seed_members, list) or len(members) != 3:
        raise ProfileError("LW_RESOURCE_ACCEPTANCE_PROFILE_MEMBERSHIP_INVALID")
    expected: set[tuple[str, str, str, str]] = set()
    for member in members:
        if not isinstance(member, dict):
            raise ProfileError("LW_RESOURCE_ACCEPTANCE_PROFILE_MEMBERSHIP_INVALID")
        role = member.get("role")
        issuer = member.get("issuer")
        subject = member.get("subjectSha256")
        if role not in ROLES or not isinstance(issuer, str) or not issuer.startswith("https://"):
            raise ProfileError("LW_RESOURCE_ACCEPTANCE_PROFILE_MEMBERSHIP_INVALID")
        _uuid7(member.get("actorId"))
        _sha256(subject)
        if member.get("courseId") != course_id:
            raise ProfileError("LW_RESOURCE_ACCEPTANCE_PROFILE_COURSE_MISMATCH")
        expected.add((member["actorId"], issuer, subject, role))
    actual = {
        (item.get("actorId"), item.get("issuer"), item.get("subjectSha256"), item.get("role"))
        for item in seed_members
        if isinstance(item, dict) and item.get("courseId") == course_id
    }
    if expected != actual or len(expected) != len(members):
        raise ProfileError("LW_RESOURCE_ACCEPTANCE_PROFILE_ACCESS_SEED_MISMATCH")
    canonical = json.dumps(profile, sort_keys=True, separators=(",", ":")).encode("utf-8")
    return hashlib.sha256(canonical).hexdigest()


def main() -> int:
    import argparse

    parser = argparse.ArgumentParser()
    parser.add_argument("--profile", type=Path, required=True)
    parser.add_argument("--access-seed", type=Path, required=True)
    arguments = parser.parse_args()
    try:
        profile = load(arguments.profile)
        seed = load(arguments.access_seed)
        print(validate(profile, seed))
    except ProfileError as error:
        raise SystemExit(str(error)) from error
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
