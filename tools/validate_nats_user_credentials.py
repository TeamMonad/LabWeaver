#!/usr/bin/env python3
"""Validate bounded NATS user permissions without exposing credential material."""

from __future__ import annotations

import argparse
import base64
import json
import re
import sys
from typing import Any


class CredentialError(Exception):
    """Stable fail-closed credential validation error."""


def _decode_claims(credentials: str) -> dict[str, Any]:
    match = re.search(
        r"-----BEGIN NATS USER JWT-----\s*([^\s]+)\s*------END NATS USER JWT------",
        credentials,
    )
    if match is None:
        raise CredentialError("LW_NATS_USER_CREDENTIAL_INVALID")
    parts = match.group(1).split(".")
    if len(parts) != 3:
        raise CredentialError("LW_NATS_USER_CREDENTIAL_INVALID")
    payload = parts[1]
    payload += "=" * (-len(payload) % 4)
    try:
        value = json.loads(base64.urlsafe_b64decode(payload).decode("utf-8"))
    except (ValueError, UnicodeError, json.JSONDecodeError) as error:
        raise CredentialError("LW_NATS_USER_CREDENTIAL_INVALID") from error
    if not isinstance(value, dict):
        raise CredentialError("LW_NATS_USER_CREDENTIAL_INVALID")
    return value


def _permission_covers(pattern: str, subject: str) -> bool:
    pattern_tokens = pattern.split(".")
    subject_tokens = subject.split(".")
    for index, token in enumerate(pattern_tokens):
        if token == ">":
            return index == len(pattern_tokens) - 1 and index < len(subject_tokens)
        if index >= len(subject_tokens) or (token != "*" and token != subject_tokens[index]):
            return False
    return len(pattern_tokens) == len(subject_tokens)


def validate(credentials: str, required_publish: list[str]) -> dict[str, int]:
    claims = _decode_claims(credentials)
    nats = claims.get("nats")
    publish = nats.get("pub") if isinstance(nats, dict) else None
    allow = publish.get("allow") if isinstance(publish, dict) else None
    if not isinstance(allow, list) or not all(isinstance(item, str) for item in allow):
        raise CredentialError("LW_NATS_USER_PUBLISH_PERMISSIONS_INVALID")
    missing = [
        subject
        for subject in required_publish
        if not any(_permission_covers(pattern, subject) for pattern in allow)
    ]
    if missing:
        raise CredentialError("LW_NATS_USER_PUBLISH_PERMISSION_MISSING")
    return {"allowedPatterns": len(allow), "requiredSubjects": len(required_publish)}


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--allow-pub", action="append", default=[], required=True)
    arguments = parser.parse_args()
    try:
        result = validate(sys.stdin.read(), arguments.allow_pub)
    except CredentialError as error:
        print(str(error), file=sys.stderr)
        return 2
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
