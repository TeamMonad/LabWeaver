#!/usr/bin/env python3
"""Validate the portable, public part of an approved LabWeaver package.

The service remains the authority for package publication. This small helper
only checks the repository examples: manifest structure, relative paths,
payload coverage, and the recorded byte digests. It deliberately does not
resolve private locators or turn a local check into a publication approval.
"""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
from typing import Any


PACKAGE_DOCUMENTS = {"README.md", "manifest.json"}


def fail(message: str) -> None:
    raise ValueError(message)


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def relative_path(value: Any) -> str:
    if not isinstance(value, str) or not value or "\\" in value:
        fail("LW_PACKAGE_PATH_INVALID")
    path = PurePosixPath(value)
    if path.is_absolute() or any(part in {"", ".", ".."} for part in path.parts):
        fail("LW_PACKAGE_PATH_INVALID")
    return value


def package_files(root: Path) -> list[str]:
    """Return the complete payload set for the checked-in example package.

    The README and manifest describe the package but are not evaluator inputs.
    Every other regular file is payload and therefore must be hash covered.  A
    symlink is rejected by the per-entry check below rather than being silently
    followed while building this list.
    """

    paths = []
    for path in root.rglob("*"):
        if not path.is_file() or path.is_symlink():
            continue
        relative = path.relative_to(root).as_posix()
        if relative in PACKAGE_DOCUMENTS:
            continue
        paths.append(relative)
    return sorted(paths)


def iter_string_values(value: Any):
    if isinstance(value, dict):
        for child in value.values():
            yield from iter_string_values(child)
    elif isinstance(value, list):
        for child in value:
            yield from iter_string_values(child)
    elif isinstance(value, str):
        yield value


def validate_profile_support_files(root: Path, listed: set[str]) -> None:
    for profile_path in sorted(path for path in listed if path.startswith("profiles/") and path.endswith(".json")):
        profile = json.loads((root / profile_path).read_text(encoding="utf-8"))
        support_files = profile.get("supportFiles", [])
        if not isinstance(support_files, list):
            fail("LW_PACKAGE_PROFILE_SUPPORT_FILES_INVALID")
        for support_file in support_files:
            support_path = relative_path(support_file)
            if support_path not in listed:
                fail(f"LW_PACKAGE_PROFILE_SUPPORT_FILE_UNLISTED:{support_path}")


def validate_evaluator_test_sources(root: Path, evaluation: dict[str, Any], listed: set[str]) -> None:
    for value in iter_string_values(evaluation):
        if not value.startswith("evaluator://tests/"):
            continue
        source = value.removeprefix("evaluator://")
        for suffix in (".in", ".out"):
            test_path = f"{source}{suffix}"
            target = root / Path(*test_path.split("/"))
            if not target.is_file() or target.is_symlink():
                fail(f"LW_PACKAGE_TEST_SOURCE_MISSING:{test_path}")
            if test_path not in listed:
                fail(f"LW_PACKAGE_TEST_SOURCE_UNLISTED:{test_path}")


def validate(root: Path) -> None:
    manifest_path = root / "manifest.json"
    if not manifest_path.is_file():
        fail("LW_PACKAGE_MANIFEST_MISSING")
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    if manifest.get("apiVersion") != "labweaver.io/problem-package/v1":
        fail("LW_PACKAGE_MANIFEST_VERSION_INVALID")
    if manifest.get("kind") != "ProblemPackage":
        fail("LW_PACKAGE_KIND_INVALID")
    metadata = manifest.get("metadata")
    if not isinstance(metadata, dict) or not metadata.get("name") or not metadata.get("version"):
        fail("LW_PACKAGE_METADATA_INVALID")
    spec = manifest.get("spec")
    if not isinstance(spec, dict):
        fail("LW_PACKAGE_SPEC_INVALID")
    runtime = spec.get("runtime")
    if not isinstance(runtime, dict) or runtime.get("kind") != "container":
        fail("LW_PACKAGE_RUNTIME_INVALID")
    image = runtime.get("runnerImage", "")
    if not isinstance(image, str) or image.count("@sha256:") != 1:
        fail("LW_PACKAGE_RUNNER_IMAGE_INVALID")
    digest_value = image.rsplit("@sha256:", 1)[1]
    if len(digest_value) != 64 or any(char not in "0123456789abcdef" for char in digest_value):
        fail("LW_PACKAGE_RUNNER_IMAGE_INVALID")
    security = spec.get("security")
    if not isinstance(security, dict) or security.get("runAsNonRoot") is not True:
        fail("LW_PACKAGE_SECURITY_INVALID")
    if security.get("readOnlyRootFilesystem") is not True:
        fail("LW_PACKAGE_SECURITY_INVALID")
    if security.get("allowPrivilegeEscalation") is not False:
        fail("LW_PACKAGE_SECURITY_INVALID")
    if security.get("networkPolicyBinding", "").strip() == "":
        fail("LW_PACKAGE_SECURITY_INVALID")

    files = spec.get("files")
    if not isinstance(files, list) or not files:
        fail("LW_PACKAGE_FILES_INVALID")
    listed: list[str] = []
    for entry in files:
        if not isinstance(entry, dict):
            fail("LW_PACKAGE_FILES_INVALID")
        path = relative_path(entry.get("path"))
        if path in listed or entry.get("role") not in {"public", "controlled"}:
            fail("LW_PACKAGE_FILES_INVALID")
        recorded = entry.get("sha256")
        if not isinstance(recorded, str) or len(recorded) != 64:
            fail("LW_PACKAGE_DIGEST_INVALID")
        target = root.joinpath(*path.split("/"))
        if not target.is_file() or target.is_symlink():
            fail(f"LW_PACKAGE_FILE_MISSING:{path}")
        if digest(target) != recorded:
            fail(f"LW_PACKAGE_DIGEST_MISMATCH:{path}")
        listed.append(path)
    if listed != sorted(listed):
        fail("LW_PACKAGE_FILES_UNSORTED")
    expected = package_files(root)
    if listed != expected:
        fail("LW_PACKAGE_FILES_COVERAGE_INVALID")

    listed_set = set(listed)
    validate_profile_support_files(root, listed_set)
    evaluation = yaml_like_evaluation(root / "evaluation.yaml")
    validate_evaluator_test_sources(root, evaluation, listed_set)

    controlled = spec.get("controlledArtifacts")
    if not isinstance(controlled, list) or not controlled:
        fail("LW_PACKAGE_CONTROLLED_ARTIFACTS_INVALID")
    for artifact in controlled:
        if not isinstance(artifact, dict) or not artifact.get("name") or not artifact.get("binding"):
            fail("LW_PACKAGE_CONTROLLED_ARTIFACTS_INVALID")
        locator = artifact.get("locator")
        if not isinstance(locator, str) or not locator.startswith("private://"):
            fail("LW_PACKAGE_CONTROLLED_LOCATOR_INVALID")

    required = {"environment.yaml", "evaluation.yaml"}
    if not required.issubset(listed) or not any(path.startswith("profiles/") for path in listed):
        fail("LW_PACKAGE_LAYOUT_INVALID")


def yaml_like_evaluation(path: Path) -> dict[str, Any]:
    """Load the small YAML document without adding a runtime dependency."""

    try:
        import yaml  # type: ignore[import-not-found]
    except ImportError as error:
        fail(f"LW_PACKAGE_YAML_READER_MISSING:{error}")
    value = yaml.safe_load(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        fail("LW_PACKAGE_EVALUATION_INVALID")
    return value


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("packages", nargs="+", type=Path)
    arguments = parser.parse_args()
    for package in arguments.packages:
        validate(package.resolve())
        print(f"approved package contract: {package}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
