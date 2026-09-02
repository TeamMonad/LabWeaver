#!/usr/bin/env python3
"""Fail closed validation for the sanitized Resource connected replay report."""

from __future__ import annotations

import argparse
import json
import re
from pathlib import Path
from typing import Any

RUN_ID = re.compile(r"^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$")
COMMIT = re.compile(r"^[0-9a-f]{40,64}$")
CHECKS = {"work-agent-run", "dual-approval", "work-release", "resource-request", "lease-renew", "lease-revoke", "environment-tombstone"}


class ReportError(Exception):
    """Stable report validation diagnostic."""


def validate(report: dict[str, Any], source_commit: str, run_id: str) -> None:
    if not COMMIT.fullmatch(source_commit) or not RUN_ID.fullmatch(run_id):
        raise ReportError("LW_RESOURCE_REPLAY_REPORT_IDENTITY_INVALID")
    if report.get("schemaVersion") != "resource-lease-replay-report.v1":
        raise ReportError("LW_RESOURCE_REPLAY_REPORT_INVALID")
    if report.get("sourceCommit") != source_commit or report.get("runId") != run_id:
        raise ReportError("LW_RESOURCE_REPLAY_REPORT_IDENTITY_INVALID")
    if set(report.get("checks", [])) != CHECKS:
        raise ReportError("LW_RESOURCE_REPLAY_REPORT_CHECKS_INVALID")
    if report.get("counts") != {"uploadedFiles": 1, "agentTracks": 2, "resourceRequests": 1, "leases": 1, "environmentTombstones": 1}:
        raise ReportError("LW_RESOURCE_REPLAY_REPORT_COUNTS_INVALID")
    if report.get("cleanup") != {"observedState": "deleted"}:
        raise ReportError("LW_RESOURCE_REPLAY_REPORT_CLEANUP_INVALID")
    identity = report.get("identity")
    if not isinstance(identity, dict) or identity.get("sourceCommit") != source_commit or identity.get("runId") != run_id:
        raise ReportError("LW_RESOURCE_REPLAY_REPORT_IDENTITY_INVALID")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--run-id", required=True)
    args = parser.parse_args()
    try:
        if args.report.is_symlink() or not args.report.is_file():
            raise ReportError("LW_RESOURCE_REPLAY_REPORT_INVALID")
        value = json.loads(args.report.read_text(encoding="utf-8"))
        if not isinstance(value, dict):
            raise ReportError("LW_RESOURCE_REPLAY_REPORT_INVALID")
        validate(value, args.source_commit, args.run_id)
    except (OSError, json.JSONDecodeError, ReportError) as error:
        raise SystemExit(str(error)) from error
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
