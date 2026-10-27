"""Enforce exactly one reviewed mmap exception to the workspace unsafe-code lint."""
from pathlib import Path
import re
import subprocess
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[2]
ALLOWED_FILE = "services/agent-service/src/build_executor.rs"
ATTRIBUTE = re.compile(r"#(!?)\[\s*allow\s*\(([^)]*)\)\s*\]", re.DOTALL)
# Only whitespace, doc comments and other attributes may separate the allow from the fn.
DECORATION = re.compile(r"(?:\s|///[^\n]*|//![^\n]*|#\[[^\]]*\])*fn\s+map_spool\b")


def tracked_rust_files():
    output = subprocess.run(
        ["git", "ls-files", "-z", "--", "*.rs"],
        cwd=ROOT, check=True, capture_output=True,
    ).stdout
    return [path for path in output.decode().split("\0") if path]


def allow_sites(sources):
    sites = []
    for relative, text in sources.items():
        for match in ATTRIBUTE.finditer(text):
            if not re.search(r"\bunsafe_code\b", match.group(2)):
                continue
            valid = (
                relative == ALLOWED_FILE
                and not match.group(1)
                and DECORATION.match(text, match.end()) is not None
            )
            sites.append(valid)
    return sites


def workspace_denies_unsafe(manifest):
    return tomllib.loads(manifest)["workspace"]["lints"]["rust"].get("unsafe_code") == "deny"


class UnsafeAllowlistTests(unittest.TestCase):
    def test_workspace_denies_unsafe_code(self):
        manifest = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
        self.assertTrue(workspace_denies_unsafe(manifest))

    def test_only_reviewed_function_allows_unsafe_code(self):
        sources = {
            relative: (ROOT / relative).read_text(encoding="utf-8")
            for relative in tracked_rust_files()
        }
        self.assertEqual(allow_sites(sources), [True])

    def test_scratch_second_allow_turns_red(self):
        source = "#[allow(unsafe_code)]\nfn map_spool() {}\n#[allow(unsafe_code)]\nfn other() {}"
        self.assertNotEqual(allow_sites({ALLOWED_FILE: source}), [True])

    def test_scratch_multiline_allow_is_detected(self):
        source = "#[allow(unsafe_code)]\nfn map_spool() {}\n#[allow(\n unsafe_code\n)]\nfn other() {}"
        self.assertNotEqual(allow_sites({ALLOWED_FILE: source}), [True])

    def test_scratch_moved_allow_turns_red(self):
        source = "#[allow(unsafe_code)]\n/// Documentation\n#[inline]\nfn other() {}\nfn map_spool() {}"
        self.assertNotEqual(allow_sites({ALLOWED_FILE: source}), [True])

    def test_scratch_deny_outside_rust_table_turns_red(self):
        self.assertFalse(workspace_denies_unsafe('[workspace.lints.rust]\nmissing_docs = "warn"\n[workspace.lints.clippy]\nunsafe_code = "deny"'))


if __name__ == "__main__":
    unittest.main()
