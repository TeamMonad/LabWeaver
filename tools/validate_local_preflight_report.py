#!/usr/bin/env python3
"""Validate the sanitized Docker Desktop local non-release report."""

from __future__ import annotations

import argparse
import json
from pathlib import Path

from jsonschema import Draft202012Validator


ROOT = Path(__file__).resolve().parents[1]
SCHEMA = ROOT / "schemas/results/local-connected-non-release.v1.schema.json"


def validate(report_path: Path) -> None:
    if report_path.is_symlink() or not report_path.is_file():
        raise ValueError("LW_LOCAL_PREFLIGHT_REPORT_INVALID")
    try:
        report = json.loads(report_path.read_text(encoding="utf-8"))
        schema = json.loads(SCHEMA.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ValueError("LW_LOCAL_PREFLIGHT_REPORT_INVALID") from error
    errors = sorted(Draft202012Validator(schema).iter_errors(report), key=str)
    if errors:
        raise ValueError("LW_LOCAL_PREFLIGHT_REPORT_SCHEMA_INVALID")
    if report.get("releaseEligible") is not False or report.get("mode") != "local-hostpath":
        raise ValueError("LW_LOCAL_PREFLIGHT_REPORT_RELEASE_BOUNDARY_INVALID")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    validate(args.report)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
