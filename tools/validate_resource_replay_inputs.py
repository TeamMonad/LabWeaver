#!/usr/bin/env python3
"""Fail-closed validation for the private Resource Lease replay inputs.

This tool validates only locator shape, public identity and hashes.  It never prints
cookie values, private-key material, JWTs, uploaded content, or direct database URLs.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import re
import sys
import time
import uuid
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
PROFILE_VALIDATOR = ROOT / "tools" / "validate_resource_acceptance_profile.py"
_SPEC = importlib.util.spec_from_file_location("resource_acceptance_profile", PROFILE_VALIDATOR)
if _SPEC is None or _SPEC.loader is None:
    raise RuntimeError("resource profile validator is unavailable")
_MODULE = importlib.util.module_from_spec(_SPEC)
sys.modules[_SPEC.name] = _MODULE
_SPEC.loader.exec_module(_MODULE)

RUN_ID = re.compile(r"^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$")
COMMIT = re.compile(r"^[0-9a-f]{40,64}$")
IMAGE = re.compile(r"^[^@]+@sha256:[0-9a-f]{64}$")


class ReplayInputError(Exception):
    """Stable diagnostic emitted without sensitive data."""


def private_file(path: Path, code: str) -> Path:
    resolved = path.resolve()
    if ".private" not in resolved.parts or not resolved.is_file():
        raise ReplayInputError(code)
    return resolved


def regular_file(path: Path, code: str) -> Path:
    if path.is_symlink() or not path.is_file():
        raise ReplayInputError(code)
    return path.resolve()


def json_file(path: Path, code: str) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ReplayInputError(code) from error
    if not isinstance(value, dict):
        raise ReplayInputError(code)
    return value


def validate_authentication(path: Path) -> dict[str, Any]:
    auth = json_file(private_file(path, "LW_RESOURCE_REPLAY_AUTHENTICATION_INVALID"), "LW_RESOURCE_REPLAY_AUTHENTICATION_INVALID")
    if auth.get("apiVersion") != "deploy.labweaver.io/resource-replay-auth/v1":
        raise ReplayInputError("LW_RESOURCE_REPLAY_AUTHENTICATION_INVALID")
    base_url = auth.get("baseUrl")
    if not isinstance(base_url, str) or not re.fullmatch(r"https?://[a-z0-9][a-z0-9.-]*(?::[0-9]+)?", base_url):
        raise ReplayInputError("LW_RESOURCE_REPLAY_AUTHENTICATION_INVALID")
    for role in ("teacher", "student", "platformAdmin"):
        locator = auth.get(f"{role}StorageState")
        if not isinstance(locator, str):
            raise ReplayInputError("LW_RESOURCE_REPLAY_AUTHENTICATION_INVALID")
        state_path = private_file(Path(locator), "LW_RESOURCE_REPLAY_AUTHENTICATION_INVALID")
        state = json_file(state_path, "LW_RESOURCE_REPLAY_AUTHENTICATION_INVALID")
        cookies = state.get("cookies")
        if not isinstance(cookies, list) or not cookies:
            raise ReplayInputError("LW_RESOURCE_REPLAY_AUTHENTICATION_INVALID")
        expirations: list[float] = []
        for cookie in cookies:
            if not isinstance(cookie, dict):
                raise ReplayInputError("LW_RESOURCE_REPLAY_AUTHENTICATION_INVALID")
            name = cookie.get("name")
            value = cookie.get("value")
            if not isinstance(name, str) or not name or not isinstance(value, str) or not value:
                raise ReplayInputError("LW_RESOURCE_REPLAY_AUTHENTICATION_INVALID")
            expires = cookie.get("expires")
            if isinstance(expires, bool):
                raise ReplayInputError("LW_RESOURCE_REPLAY_AUTHENTICATION_INVALID")
            if isinstance(expires, (int, float)) and expires > 0:
                expirations.append(float(expires))
            elif expires is not None and not isinstance(expires, (int, float)):
                raise ReplayInputError("LW_RESOURCE_REPLAY_AUTHENTICATION_INVALID")
        # Session cookies use -1 and cannot be checked locally. When the state
        # contains expiry-bearing cookies, fail before acquiring the connected
        # ledger lease if every such cookie is already expired. This prevents a
        # stale browser session from consuming a replay attempt.
        if expirations and max(expirations) <= time.time() + 30:
            raise ReplayInputError("LW_RESOURCE_REPLAY_AUTHENTICATION_EXPIRED")
    return auth


def validate_package(path: Path, source_commit: str) -> dict[str, Any]:
    manifest = json_file(regular_file(path, "LW_RESOURCE_REPLAY_PACKAGE_INVALID"), "LW_RESOURCE_REPLAY_PACKAGE_INVALID")
    images = manifest.get("images")
    if (
        manifest.get("schema_version") != "platform-image-package-manifest.v1"
        or manifest.get("profile") != "resource"
        or manifest.get("source_commit") != source_commit
        or manifest.get("overall") != "passed"
        or not isinstance(images, list)
        or len(images) != 1
        or not isinstance(images[0], dict)
        or images[0].get("component") != "resource-service"
        or not isinstance(images[0].get("reference"), str)
        or not IMAGE.fullmatch(images[0]["reference"])
    ):
        raise ReplayInputError("LW_RESOURCE_REPLAY_PACKAGE_INVALID")
    return manifest


def validate_deployment(path: Path, source_commit: str, run_id: str, package: dict[str, Any]) -> dict[str, Any]:
    deployment = json_file(regular_file(path, "LW_RESOURCE_REPLAY_DEPLOYMENT_MANIFEST_INVALID"), "LW_RESOURCE_REPLAY_DEPLOYMENT_MANIFEST_INVALID")
    package_image = package["images"][0]
    deployment_image = deployment.get("image")
    if (
        deployment.get("schemaVersion") != "resource-deployment-manifest.v1"
        or deployment.get("sourceCommit") != source_commit
        or deployment.get("runId") != run_id
        or not isinstance(deployment_image, dict)
        or set(deployment_image) != {"component", "reference"}
        or deployment_image.get("component") != package_image.get("component")
        or deployment_image.get("reference") != package_image.get("reference")
    ):
        raise ReplayInputError("LW_RESOURCE_REPLAY_DEPLOYMENT_MANIFEST_INVALID")
    return deployment


def validate(arguments: argparse.Namespace) -> dict[str, str]:
    if not RUN_ID.fullmatch(arguments.run_id) or not COMMIT.fullmatch(arguments.source_commit):
        raise ReplayInputError("LW_RESOURCE_REPLAY_IDENTITY_INVALID")
    profile_path = private_file(arguments.profile, "LW_RESOURCE_REPLAY_PROFILE_INVALID")
    profile = json_file(profile_path, "LW_RESOURCE_REPLAY_PROFILE_INVALID")
    # The acceptance profile intentionally has no embedded credentials.  Its associated
    # Access seed was already validated and applied by resource_application.
    _MODULE.validate(profile, profile)
    auth = validate_authentication(arguments.authentication)
    package = validate_package(arguments.package_manifest, arguments.source_commit)
    deployment = validate_deployment(
        arguments.deployment_manifest, arguments.source_commit, arguments.run_id, package
    )
    return {
        "profileSha256": hashlib.sha256(profile_path.read_bytes()).hexdigest(),
        "authenticationLocatorSha256": hashlib.sha256(str(arguments.authentication.resolve()).encode()).hexdigest(),
        "packageManifestSha256": hashlib.sha256(arguments.package_manifest.read_bytes()).hexdigest(),
        "deploymentManifestSha256": hashlib.sha256(arguments.deployment_manifest.read_bytes()).hexdigest(),
        "baseUrlSha256": hashlib.sha256(auth["baseUrl"].encode()).hexdigest(),
        "resourceImage": package["images"][0]["reference"],
        "runId": arguments.run_id,
        "sourceCommit": arguments.source_commit,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--profile", type=Path, required=True)
    parser.add_argument("--authentication", type=Path, required=True)
    parser.add_argument("--deployment-manifest", type=Path, required=True)
    parser.add_argument("--package-manifest", type=Path, required=True)
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--run-id", required=True)
    arguments = parser.parse_args()
    try:
        print(json.dumps(validate(arguments), sort_keys=True, separators=(",", ":")))
    except (ReplayInputError, _MODULE.ProfileError) as error:
        raise SystemExit(str(error)) from error
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
