#!/usr/bin/env python3
"""Compare the submitted CUDA result statistics against the expected values."""

from __future__ import annotations

import sys

KEYS = ("N", "sum", "max")


def parse(path: str) -> dict[str, int]:
    values: dict[str, int] = {}
    with open(path, encoding="utf-8", errors="replace") as handle:
        for line in handle:
            line = line.strip()
            if not line or "=" not in line:
                continue
            key, _, raw = line.partition("=")
            key = key.strip()
            if key not in KEYS:
                continue
            try:
                values[key] = int(raw.strip(), 10)
            except ValueError:
                continue
    return values


def main(argv: list[str]) -> int:
    if len(argv) != 3:
        sys.stderr.write("check_result: usage: <submitted> <expected>\n")
        return 64
    expected = parse(argv[2])
    submitted = parse(argv[1])
    if all(key in expected and submitted.get(key) == expected[key] for key in KEYS):
        for key in KEYS:
            sys.stdout.write(f"{key}={expected[key]}\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
