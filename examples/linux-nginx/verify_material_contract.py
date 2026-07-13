#!/usr/bin/env python3
"""Validate only the public-safe Linux Nginx material contract package."""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import tempfile
from html.parser import HTMLParser
from pathlib import Path


ROOT = Path(__file__).resolve().parent
PUBLIC_FILES = (
    Path("statement.md"),
    Path("materials/index.html"),
    Path("submission.yaml"),
    Path("evaluation-scenarios.json"),
)
REQUIRED_DIAGNOSTICS = {
    "nginx-not-listening": "LW_LINUX_LAB_NGINX_NOT_LISTENING",
    "site-mismatch": "LW_LINUX_LAB_SITE_MISMATCH",
}
FORBIDDEN_MARKERS = ("BEGIN PRIVATE KEY", "BEGIN OPENSSH PRIVATE KEY")


class Markers(HTMLParser):
    def __init__(self) -> None:
        super().__init__()
        self.title: str | None = None
        self.heading: str | None = None
        self.lab_id: str | None = None
        self._capture: str | None = None

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        values = dict(attrs)
        if tag == "html":
            self.lab_id = values.get("data-lab-id")
        if tag in {"title", "h1"}:
            self._capture = tag

    def handle_endtag(self, tag: str) -> None:
        if self._capture == tag:
            self._capture = None

    def handle_data(self, data: str) -> None:
        if self._capture == "title":
            self.title = (self.title or "") + data.strip()
        if self._capture == "h1":
            self.heading = (self.heading or "") + data.strip()


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def fail(message: str) -> None:
    raise ValueError(message)


def validate(root: Path, report: Path | None = None) -> None:
    manifest_path = root / "material-manifest.json"
    if not manifest_path.is_file():
        fail("LW_LINUX_LAB_MATERIAL_MISSING: material manifest is missing")
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    if manifest.get("schemaVersion") != "linux-nginx-material-manifest/v1alpha1":
        fail("LW_LINUX_LAB_MANIFEST_INVALID: unexpected manifest schema version")
    if manifest.get("labId") != "linux-nginx-v1" or manifest.get("status") != "candidate":
        fail("LW_LINUX_LAB_MANIFEST_INVALID: lab identity or candidate state is invalid")

    hashes = {entry.get("path"): entry.get("sha256") for entry in manifest["publicArtifacts"]}
    for relative in PUBLIC_FILES:
        path = root / relative
        if not path.is_file():
            fail(f"LW_LINUX_LAB_MATERIAL_MISSING: {relative.as_posix()} is missing")
        if hashes.get(relative.as_posix()) != digest(path):
            fail(f"LW_LINUX_LAB_TEMPLATE_HASH_MISMATCH: {relative.as_posix()}")
        text = path.read_text(encoding="utf-8")
        if any(marker in text for marker in FORBIDDEN_MARKERS):
            fail(f"LW_LINUX_LAB_RESTRICTED_CONTENT: {relative.as_posix()}")

    parser = Markers()
    parser.feed((root / "materials/index.html").read_text(encoding="utf-8"))
    if (parser.title, parser.heading, parser.lab_id) != (
        "Nginx Lab",
        "Nginx Lab",
        "linux-nginx-v1",
    ):
        fail("LW_LINUX_LAB_TEMPLATE_MARKER_MISMATCH")

    submission = (root / "submission.yaml").read_text(encoding="utf-8")
    required_lines = (
        "status: candidate",
        "- report.md",
        "report.md: 65536",
        "diagnostic: LW_LINUX_LAB_REPORT_MISSING",
        "report.md: excluded",
    )
    if any(line not in submission for line in required_lines):
        fail("LW_LINUX_LAB_SUBMISSION_CANDIDATE_INVALID")
    if report is not None:
        if not report.is_file():
            fail("LW_LINUX_LAB_REPORT_MISSING")
        if report.stat().st_size > 65536:
            fail("LW_LINUX_LAB_REPORT_TOO_LARGE")
        if any(marker in report.read_text(encoding="utf-8") for marker in FORBIDDEN_MARKERS):
            fail("LW_LINUX_LAB_RESTRICTED_CONTENT: report.md")

    scenarios = json.loads((root / "evaluation-scenarios.json").read_text(encoding="utf-8"))
    actual = {entry["id"]: entry["diagnostic"] for entry in scenarios["negative"]}
    if actual != REQUIRED_DIAGNOSTICS:
        fail("LW_LINUX_LAB_SCENARIO_MAPPING_INVALID")

    for controlled in manifest["controlledArtifacts"]:
        if not controlled["controlledLocator"].startswith("private://"):
            fail("LW_LINUX_LAB_CONTROLLED_LOCATOR_INVALID")
        if controlled["sha256"] is not None or not controlled["bindingState"].startswith("blocked-"):
            fail("LW_LINUX_LAB_CONTROLLED_ARTIFACT_INVALID")


def replace_manifest_hash(root: Path, relative: Path) -> None:
    path = root / "material-manifest.json"
    manifest = json.loads(path.read_text(encoding="utf-8"))
    for entry in manifest["publicArtifacts"]:
        if entry["path"] == relative.as_posix():
            entry["sha256"] = digest(root / relative)
    path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")


def self_test() -> None:
    validate(ROOT)
    mutations = (
        (Path("materials/index.html"), "<title>Wrong</title>", "LW_LINUX_LAB_TEMPLATE_MARKER_MISMATCH"),
        (Path("statement.md"), "", "LW_LINUX_LAB_MATERIAL_MISSING"),
        (Path("submission.yaml"), "report.md: 65537", "LW_LINUX_LAB_SUBMISSION_CANDIDATE_INVALID"),
        (Path("materials/index.html"), "-----BEGIN PRIVATE KEY-----", "LW_LINUX_LAB_RESTRICTED_CONTENT"),
    )
    for relative, replacement, expected in mutations:
        with tempfile.TemporaryDirectory() as directory:
            copied = Path(directory) / "linux-nginx"
            shutil.copytree(ROOT, copied)
            target = copied / relative
            if replacement:
                target.write_text(replacement, encoding="utf-8")
                replace_manifest_hash(copied, relative)
            else:
                target.unlink()
            try:
                validate(copied)
            except ValueError as error:
                if expected not in str(error):
                    raise AssertionError(f"expected {expected}, got {error}") from error
            else:
                raise AssertionError(f"mutation {relative} unexpectedly validated")
    with tempfile.TemporaryDirectory() as directory:
        missing_report = Path(directory) / "missing-report.md"
        try:
            validate(ROOT, missing_report)
        except ValueError as error:
            if "LW_LINUX_LAB_REPORT_MISSING" not in str(error):
                raise AssertionError(f"expected missing report error, got {error}") from error
        else:
            raise AssertionError("missing report unexpectedly validated")

        report = Path(directory) / "report.md"
        report.write_bytes(b"x" * 65537)
        try:
            validate(ROOT, report)
        except ValueError as error:
            if "LW_LINUX_LAB_REPORT_TOO_LARGE" not in str(error):
                raise AssertionError(f"expected report limit error, got {error}") from error
        else:
            raise AssertionError("oversized report unexpectedly validated")
    print("linux-nginx material contract: normal and negative checks passed")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--report", type=Path)
    arguments = parser.parse_args()
    if arguments.self_test:
        self_test()
    else:
        validate(ROOT, arguments.report)
        print("linux-nginx material contract: valid")


if __name__ == "__main__":
    main()
