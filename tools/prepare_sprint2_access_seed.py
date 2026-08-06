#!/usr/bin/env python3
"""Derive private Access membership seeds from the reviewed Keycloak realm."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import secrets
import stat
import time
import uuid
from pathlib import Path
from urllib.parse import urlparse


class AccessSeedError(Exception):
    """Stable fail-closed Access seed authoring diagnostic."""


def private_output(path: Path) -> Path:
    resolved = path.resolve()
    if not any(part in {".private", "private"} for part in resolved.parts):
        raise AccessSeedError("LW_SPRINT2_ACCESS_SEED_PRIVATE_PATH_REQUIRED")
    if resolved.exists() or not resolved.parent.is_dir():
        raise AccessSeedError("LW_SPRINT2_ACCESS_SEED_OUTPUT_INVALID")
    return resolved


def validate_issuer(value: str) -> str:
    parsed = urlparse(value)
    if (
        parsed.scheme != "https"
        or not parsed.hostname
        or parsed.query
        or parsed.fragment
        or parsed.username
        or parsed.password
    ):
        raise AccessSeedError("LW_SPRINT2_ACCESS_SEED_ISSUER_INVALID")
    return value.rstrip("/")


def find_user(realm: dict[str, object], username: str, role: str) -> str:
    users = realm.get("users")
    if not isinstance(users, list):
        raise AccessSeedError("LW_SPRINT2_ACCESS_SEED_REALM_INVALID")
    matches = [user for user in users if isinstance(user, dict) and user.get("username") == username]
    if len(matches) != 1:
        raise AccessSeedError("LW_SPRINT2_ACCESS_SEED_USER_INVALID")
    user = matches[0]
    subject = user.get("id")
    roles = user.get("realmRoles")
    if not isinstance(subject, str) or not subject or not isinstance(roles, list) or role not in roles:
        raise AccessSeedError("LW_SPRINT2_ACCESS_SEED_USER_INVALID")
    return subject


def uuid7() -> uuid.UUID:
    """Generate an RFC 9562 UUIDv7 without relying on Python 3.14's uuid.uuid7."""
    unix_milliseconds = time.time_ns() // 1_000_000
    if not 0 <= unix_milliseconds < 1 << 48:
        raise AccessSeedError("LW_SPRINT2_ACCESS_SEED_CLOCK_INVALID")
    value = unix_milliseconds << 80
    value |= 0x7 << 76
    value |= secrets.randbits(12) << 64
    value |= 0b10 << 62
    value |= secrets.randbits(62)
    return uuid.UUID(int=value)


def build_seed(
    realm: dict[str, object],
    issuer: str,
    course_id: str,
    teacher_username: str,
    student_username: str,
    admin_username: str,
) -> dict[str, object]:
    issuer = validate_issuer(issuer)
    try:
        parsed_course_id = uuid.UUID(course_id)
    except ValueError as error:
        raise AccessSeedError("LW_SPRINT2_ACCESS_SEED_COURSE_INVALID") from error
    if parsed_course_id.version != 7:
        raise AccessSeedError("LW_SPRINT2_ACCESS_SEED_COURSE_INVALID")
    memberships: list[dict[str, str]] = []
    for username, role in (
        (teacher_username, "teacher"),
        (student_username, "student"),
        (admin_username, "platform_admin"),
    ):
        subject = find_user(realm, username, role)
        actor_id = uuid7()
        memberships.append(
            {
                "actorId": str(actor_id),
                "issuer": issuer,
                "subjectSha256": hashlib.sha256(subject.encode("utf-8")).hexdigest(),
                "courseId": str(parsed_course_id),
                "role": role,
            }
        )
    return {
        "apiVersion": "deploy.labweaver.io/sprint2-access-seed/v1",
        "courseMemberships": memberships,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--realm-file", type=Path, required=True)
    parser.add_argument("--issuer", required=True)
    parser.add_argument("--course-id", required=True)
    parser.add_argument("--teacher-username", required=True)
    parser.add_argument("--student-username", required=True)
    parser.add_argument("--admin-username", required=True)
    parser.add_argument("--output", type=Path, required=True)
    arguments = parser.parse_args()
    try:
        realm = json.loads(arguments.realm_file.read_text(encoding="utf-8"))
        if not isinstance(realm, dict):
            raise AccessSeedError("LW_SPRINT2_ACCESS_SEED_REALM_INVALID")
        output = private_output(arguments.output)
        seed = build_seed(
            realm,
            arguments.issuer,
            arguments.course_id,
            arguments.teacher_username,
            arguments.student_username,
            arguments.admin_username,
        )
        descriptor = os.open(output, os.O_WRONLY | os.O_CREAT | os.O_EXCL, stat.S_IRUSR | stat.S_IWUSR)
        with os.fdopen(descriptor, "w", encoding="utf-8", newline="\n") as handle:
            json.dump(seed, handle, indent=2, sort_keys=True)
            handle.write("\n")
    except (OSError, json.JSONDecodeError) as error:
        raise AccessSeedError("LW_SPRINT2_ACCESS_SEED_IO_FAILED") from error
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except AccessSeedError as error:
        raise SystemExit(str(error)) from error
