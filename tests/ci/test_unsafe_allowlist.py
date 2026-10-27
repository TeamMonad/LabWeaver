"""Keep the workspace `unsafe_code = "deny"` exception limited to the reviewed mmap function."""
from pathlib import Path
import re
import subprocess
import unittest

ROOT = Path(__file__).resolve().parents[2]
ALLOWED = {("services/agent-service/src/build_executor.rs", "map_spool")}
ALLOW_ATTRIBUTE = re.compile(r"#!?\[\s*allow\s*\([^)]*\bunsafe_code\b")
FN_NAME = re.compile(r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)")


def tracked_rust_files():
    output = subprocess.run(
        ["git", "ls-files", "-z", "--", "*.rs"],
        cwd=ROOT, check=True, capture_output=True,
    ).stdout
    return [path for path in output.decode().split("\0") if path]


def allow_sites():
    sites = set()
    for relative in tracked_rust_files():
        lines = (ROOT / relative).read_text(encoding="utf-8").splitlines()
        for index, line in enumerate(lines):
            if not ALLOW_ATTRIBUTE.search(line):
                continue
            # Attribute applies to the next item; an inner `#![allow]` names no function.
            target = None
            if not line.lstrip().startswith("#!"):
                for following in lines[index + 1:index + 6]:
                    match = FN_NAME.search(following)
                    if match:
                        target = match.group(1)
                        break
            sites.add((relative, target))
    return sites


class UnsafeAllowlistTests(unittest.TestCase):
    def test_workspace_denies_unsafe_code(self):
        manifest = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
        self.assertRegex(manifest, r'(?m)^\s*unsafe_code\s*=\s*"(deny|forbid)"')

    def test_only_reviewed_function_allows_unsafe_code(self):
        self.assertEqual(allow_sites(), ALLOWED)


if __name__ == "__main__":
    unittest.main()
