#!/usr/bin/env python3
"""Format gate and exact flag comparison for the CTF web lab."""

from __future__ import annotations

import re
import sys

FLAG_PATTERN = re.compile(r"^FLAG\{[A-Za-z0-9_:-]{1,120}\}$")


def read(path: str) -> str:
    with open(path, encoding="utf-8", errors="replace") as handle:
        return handle.read()


def compile_mode(path: str) -> int:
    value = read(path).strip()
    if FLAG_PATTERN.match(value):
        return 0
    sys.stderr.write("ctf_check: submission is not a well-formed flag\n")
    return 3


def check_mode(submitted: str, expected: str) -> int:
    expected_value = read(expected).strip()
    if read(submitted).strip() == expected_value:
        sys.stdout.write(expected_value + "\n")
    return 0


def main(argv: list[str]) -> int:
    if len(argv) == 3 and argv[1] == "compile":
        return compile_mode(argv[2])
    if len(argv) == 4 and argv[1] == "check":
        return check_mode(argv[2], argv[3])
    sys.stderr.write("ctf_check: usage: compile <submission> | check <submission> <expected>\n")
    return 64


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
