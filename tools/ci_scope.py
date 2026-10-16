#!/usr/bin/env python3
"""Select full Rust CI unless every changed path has a known non-Rust scope."""

import os
from pathlib import Path, PurePosixPath
import re
import subprocess
from typing import Mapping, NamedTuple, Sequence


class Scope(NamedTuple):
    rust_required: bool
    reason: str


WEB_SUFFIXES = frozenset({
    ".vue", ".ts", ".tsx", ".js", ".mjs", ".cjs", ".html", ".css",
    ".scss", ".sass", ".less", ".json", ".yaml", ".yml", ".md", ".txt",
    ".svg", ".png", ".jpg", ".jpeg", ".gif", ".webp", ".avif", ".ico",
    ".woff", ".woff2", ".ttf", ".snap",
})


def classify_paths(paths: Sequence[str]) -> Scope:
    """Keep shared authority, generated contracts and unknown inputs on full CI."""
    if not paths:
        return Scope(True, "no_diff")
    for path in paths:
        if (any(part in ("", ".", "..") for part in path.split("/"))
                or any(ord(char) < 32 for char in path) or "\\" in path):
            return Scope(True, "unexpected_path")
        if (path.startswith("web/") and path != "web/src/generated"
                and not path.startswith("web/src/generated/")
                and PurePosixPath(path).suffix in WEB_SUFFIXES):
            continue
        if path == "README.md" or (path.startswith("docs/") and path.endswith(".md")):
            continue
        return Scope(True, "rust_or_unknown")
    return Scope(False, "documentation_or_web")


def _git(root: Path, *args: str) -> bytes:
    return subprocess.check_output(
        ["git", *args], cwd=root, stderr=subprocess.DEVNULL, timeout=30
    )


def _commit(value: str) -> bool:
    return bool(re.fullmatch(r"[0-9a-f]{40}(?:[0-9a-f]{24})?", value)) and set(value) != {"0"}


def select_scope(env: Mapping[str, str], root: Path) -> Scope:
    """Compare the PR merge base or complete push range, with full CI on failure."""
    event = env.get("EVENT_NAME", "")
    if event == "pull_request":
        base, head = env.get("BASE_SHA", ""), env.get("HEAD_SHA", "")
    elif event == "push":
        base, head = env.get("BEFORE_SHA", ""), env.get("AFTER_SHA", "")
    else:
        return Scope(True, "unknown_event")
    if not _commit(base) or not _commit(head):
        return Scope(True, "comparison_unavailable")
    try:
        if event == "pull_request":
            base = _git(root, "merge-base", base, head).decode("ascii").strip()
        # Disable rename detection so moving a Rust file into web keeps both paths.
        raw = _git(root, "diff", "--no-ext-diff", "--no-renames", "--name-only", "-z", base, head)
        paths = [part.decode("utf-8") for part in raw.split(b"\0") if part]
        return classify_paths(paths)
    except (OSError, subprocess.SubprocessError, UnicodeError):
        return Scope(True, "comparison_unavailable")


if __name__ == "__main__":
    scope = select_scope(os.environ, Path.cwd())
    print(f"rust_required={str(scope.rust_required).lower()}")
    print(f"scope_reason={scope.reason}")
