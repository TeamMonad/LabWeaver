"""Enforce exactly one reviewed mmap exception to the workspace unsafe-code lint."""
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
ALLOWED_FILE = "services/agent-service/src/build_executor.rs"
ATTRIBUTE = re.compile(r"#(!?)\s*\[\s*allow\s*\(([^)]*)\)\s*\]", re.DOTALL)
# Only whitespace, doc comments and other attributes may separate the allow from the fn.
DECORATION = re.compile(r"(?:\s|///[^\n]*|/\*\*.*?\*/|#\[[^\]]*\])*fn\s+map_spool\b", re.DOTALL)


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
    table = re.search(r"(?ms)^\[workspace\.lints\.rust\]\s*\n(.*?)(?=^\[|\Z)", manifest)
    return table is not None and re.search(
        r'(?m)^unsafe_code\s*=\s*"deny"\s*(?:#.*)?$', table.group(1)
    ) is not None


class UnsafeAllowlistTests(unittest.TestCase):
    def assert_scratch_gate_red(self, label, source, manifest=None):
        """Run the real git-file scanner in an isolated scratch repository, then remove it."""
        with tempfile.TemporaryDirectory(prefix="unsafe-allowlist-") as temporary:
            root = Path(temporary)
            script = root / "tests/ci/test_unsafe_allowlist.py"
            script.parent.mkdir(parents=True)
            script.write_text(Path(__file__).read_text(encoding="utf-8"), encoding="utf-8")
            rust = root / ALLOWED_FILE
            rust.parent.mkdir(parents=True)
            rust.write_text(source, encoding="utf-8")
            (root / "Cargo.toml").write_text(
                manifest or '[workspace.lints.rust]\nunsafe_code = "deny"\n', encoding="utf-8"
            )
            subprocess.run(["git", "init", "--quiet"], cwd=root, check=True)
            subprocess.run(["git", "add", "."], cwd=root, check=True)
            result = subprocess.run(
                [sys.executable, str(script),
                 "UnsafeAllowlistTests.test_only_reviewed_function_allows_unsafe_code",
                 "UnsafeAllowlistTests.test_workspace_denies_unsafe_code"],
                cwd=root, capture_output=True, text=True,
            )
            self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
            self.assertIn("FAILED (failures=1)", result.stderr)
            print(f"scratch {label}: gate RC={result.returncode} RED (expected)\n{result.stderr}")
        self.assertFalse(root.exists(), "scratch repository must be removed")

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
        self.assert_scratch_gate_red("second allow", source)

    def test_scratch_multiline_allow_is_detected(self):
        source = "#[allow(unsafe_code)]\nfn map_spool() {}\n#[allow(\n unsafe_code\n)]\nfn other() {}"
        self.assert_scratch_gate_red("multi-line second allow", source)

    def test_scratch_moved_allow_turns_red(self):
        source = "#[allow(unsafe_code)]\n/// Documentation\n#[inline]\nfn other() {}\nfn map_spool() {}"
        self.assert_scratch_gate_red("allow on another fn", source)

    def test_scratch_deny_outside_rust_table_turns_red(self):
        self.assert_scratch_gate_red(
            "deny outside rust table", "#[allow(unsafe_code)]\nfn map_spool() {}",
            '[workspace.lints.rust]\nmissing_docs = "warn"\n[workspace.lints.clippy]\nunsafe_code = "deny"',
        )

    def test_multiline_single_allow_and_intervening_docs_and_attributes(self):
        source = "#[allow(\n unsafe_code\n)]\n/** Documentation */\n#[inline]\nfn map_spool() {}"
        self.assertEqual(allow_sites({ALLOWED_FILE: source}), [True])

    def test_other_items_cannot_separate_the_allow_from_map_spool(self):
        source = "#[allow(unsafe_code)]\nconst OTHER: bool = true;\nfn map_spool() {}"
        self.assertNotEqual(allow_sites({ALLOWED_FILE: source}), [True])


if __name__ == "__main__":
    unittest.main()
