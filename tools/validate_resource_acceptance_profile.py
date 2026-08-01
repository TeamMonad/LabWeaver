#!/usr/bin/env python3
"""Validate the private, non-secret Resource connected-replay profile."""

from __future__ import annotations

import hashlib
import json
import re
import uuid
from pathlib import Path
from typing import Any


class ProfileError(Exception):
    """Stable fail-closed Resource profile diagnostic."""


API_VERSION = "deploy.labweaver.io/resource-acceptance-profile/v1"
ROLES = {"teacher", "student", "platform_admin"}
RUNTIME = {"container", "virtual_machine"}
SHA256 = re.compile(r"^[0-9a-f]{64}$")
RFC3339_MILLIS = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$")
DENIED_DATA_CLASSES = {
    "secret",
    "token",
    "private_key",
    "personally_identifiable_information",
    "unallowlisted_student_submission",
}


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
    if not isinstance(value, str) or not SHA256.fullmatch(value):
        raise ProfileError("LW_RESOURCE_ACCEPTANCE_PROFILE_HASH_INVALID")
    return value


def _policy(policy: object, course_id: str) -> None:
    if not isinstance(policy, dict):
        raise ProfileError("LW_RESOURCE_ACCEPTANCE_PROFILE_POLICY_INVALID")
    if _uuid7(policy.get("id")) is None or policy.get("courseId") != course_id:
        raise ProfileError("LW_RESOURCE_ACCEPTANCE_PROFILE_POLICY_INVALID")
    if not isinstance(policy.get("revision"), int) or policy["revision"] < 1:
        raise ProfileError("LW_RESOURCE_ACCEPTANCE_PROFILE_POLICY_INVALID")
    binding = policy.get("binding")
    if not isinstance(binding, dict):
        raise ProfileError("LW_RESOURCE_ACCEPTANCE_PROFILE_POLICY_INVALID")
    required_binding = ("runtimeBinding", "model", "claudeCodeVersion")
    if any(not isinstance(binding.get(field), str) or not binding[field].strip() for field in required_binding):
        raise ProfileError("LW_RESOURCE_ACCEPTANCE_PROFILE_POLICY_INVALID")
    if "://" in binding["runtimeBinding"] or binding["model"].strip().lower() in {"latest", "default"}:
        raise ProfileError("LW_RESOURCE_ACCEPTANCE_PROFILE_POLICY_INVALID")
    _sha256(binding.get("workerImageSha256"))
    _sha256(binding.get("runtimeConfigSha256"))
    if not isinstance(binding.get("maxInFlightPerWorker"), int) or binding["maxInFlightPerWorker"] < 1:
        raise ProfileError("LW_RESOURCE_ACCEPTANCE_PROFILE_POLICY_INVALID")
    budget = policy.get("budget")
    if not isinstance(budget, dict) or any(
        not isinstance(budget.get(field), int) or budget[field] < 1
        for field in (
            "maxInputTokens", "maxOutputTokens", "maxRequests", "maxCostMicrousd", "timeoutMilliseconds",
        )
    ):
        raise ProfileError("LW_RESOURCE_ACCEPTANCE_PROFILE_POLICY_INVALID")
    if any(
        not isinstance(budget.get(field), int) or budget[field] < 0
        for field in ("maxTransientRetries", "maxSchemaRepairs")
    ):
        raise ProfileError("LW_RESOURCE_ACCEPTANCE_PROFILE_POLICY_INVALID")
    denied = policy.get("deniedDataClasses")
    if not isinstance(denied, list) or set(denied) != DENIED_DATA_CLASSES or len(denied) != len(DENIED_DATA_CLASSES):
        raise ProfileError("LW_RESOURCE_ACCEPTANCE_PROFILE_POLICY_INVALID")
    if policy.get("studentContentMode") != "manifest_allowlist_only":
        raise ProfileError("LW_RESOURCE_ACCEPTANCE_PROFILE_POLICY_INVALID")
    if not isinstance(policy.get("activatedAt"), str) or not RFC3339_MILLIS.fullmatch(policy["activatedAt"]):
        raise ProfileError("LW_RESOURCE_ACCEPTANCE_PROFILE_POLICY_INVALID")


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
    if profile.get("runtimeKind") not in RUNTIME:
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
    replay = profile.get("replay")
    if not isinstance(replay, dict):
        raise ProfileError("LW_RESOURCE_ACCEPTANCE_PROFILE_REPLAY_INVALID")
    if not isinstance(replay.get("projectId"), str):
        raise ProfileError("LW_RESOURCE_ACCEPTANCE_PROFILE_REPLAY_INVALID")
    _uuid7(replay["projectId"])
    if not isinstance(replay.get("providerBinding"), str) or not replay["providerBinding"].strip():
        raise ProfileError("LW_RESOURCE_ACCEPTANCE_PROFILE_REPLAY_INVALID")
    material_file = replay.get("materialFile")
    if not isinstance(material_file, dict):
        raise ProfileError("LW_RESOURCE_ACCEPTANCE_PROFILE_REPLAY_INVALID")
    relative_path = material_file.get("relativePath")
    if (
        not isinstance(relative_path, str)
        or not relative_path
        or Path(relative_path).is_absolute()
        or ".." in Path(relative_path).parts
        or not isinstance(material_file.get("mediaType"), str)
        or not material_file["mediaType"].strip()
    ):
        raise ProfileError("LW_RESOURCE_ACCEPTANCE_PROFILE_REPLAY_INVALID")
    _sha256(material_file.get("sha256"))
    _policy(replay.get("policy"), course_id)
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
