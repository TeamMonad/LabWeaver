"""Check image CI branch/path coverage offline, without running GitHub events or builds."""

import fnmatch
from pathlib import Path
import shlex
import subprocess
import unittest

import yaml


ROOT = Path(__file__).resolve().parents[2]


def path_matches(path, pattern):
    """Match positive path globs by segment: only ** can cross a slash."""
    def match(parts, patterns):
        if not patterns:
            return not parts
        if patterns[0] == "**":
            return any(match(parts[index:], patterns[1:]) for index in range(len(parts) + 1))
        return (
            bool(parts)
            and fnmatch.fnmatchcase(parts[0], patterns[0])
            and match(parts[1:], patterns[1:])
        )
    return match(path.split("/"), pattern.split("/"))


class ImageEventCoverageTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.workflow = yaml.load(
            (ROOT / ".github/workflows/platform-images.yml").read_text(),
            Loader=yaml.BaseLoader,
        )

    def covered(self, event, branch, path):
        """Inspect this workflow's exact branches and positive path filters only."""
        filters = self.workflow["on"][event] or {}
        self.assertNotIn("branches-ignore", filters)
        self.assertNotIn("paths-ignore", filters)
        patterns = filters["paths"]
        self.assertFalse(any(pattern.startswith("!") for pattern in patterns))
        self.assertFalse(
            any(any(char in pattern for char in "?+[]{}") for pattern in patterns),
            "path-glob syntax beyond literal segments, * and ** needs explicit review",
        )
        branches = filters.get("branches")
        return (
            branches is None
            or any(fnmatch.fnmatchcase(branch, pattern) for pattern in branches)
        ) and any(path_matches(path, pattern) for pattern in patterns)

    def test_automated_workflows_check_stacked_pr_targets(self):
        for workflow_name in (
            "rust.yml",
            "web-ci.yml",
            "playwright-ci.yml",
            "ansible.yml",
            "platform-images.yml",
        ):
            with self.subTest(workflow=workflow_name):
                workflow = yaml.load(
                    (ROOT / ".github/workflows" / workflow_name).read_text(),
                    Loader=yaml.BaseLoader,
                )
                pull_request = workflow["on"].get("pull_request") or {}
                self.assertNotIn(
                    "branches",
                    pull_request,
                    "stacked PRs must not lose required checks because of their target branch",
                )
                push = workflow["on"].get("push") or {}
                self.assertEqual(push.get("branches"), ["main", "develop"])

    def test_single_star_does_not_hide_nested_path_gaps(self):
        self.assertTrue(path_matches("crates/auth/src/lib.rs", "crates/**"))
        self.assertFalse(path_matches("crates/auth/src/lib.rs", "crates/*"))
        pattern = "schemas/results/platform-image-*.json"
        self.assertTrue(path_matches("schemas/results/platform-image-package.json", pattern))
        self.assertFalse(path_matches("schemas/results/platform-image-extra/package.json", pattern))

    def test_copied_rust_inputs_cover_main_and_develop_prs_and_pushes(self):
        sources = []
        in_builder = False
        recipe = (ROOT / "containers/Containerfile.rust").read_text()
        for line in recipe.splitlines():
            if not line.startswith(("FROM ", "COPY ")):
                continue
            words = shlex.split(line, comments=True)
            if not words:
                continue
            if words[0] == "FROM":
                if in_builder:
                    break
                in_builder = words[-2:] == ["AS", "builder"]
            elif in_builder and words[0] == "COPY":
                sources.extend(words[1:-1])
        self.assertTrue(sources, "no Rust builder inputs were inspected")
        paths = {".dockerignore"}
        for source in sources:
            self.assertNotIn("$", source, "dynamic builder input requires explicit review")
            tracked = subprocess.check_output(
                ["git", "ls-files", "-z", "--", source], cwd=ROOT
            )
            files = {part.decode() for part in tracked.split(b"\0") if part}
            self.assertTrue(files, f"builder input has no tracked files: {source}")
            paths.update(files)
        for event in ("pull_request", "push"):
            for branch in ("main", "develop"):
                with self.subTest(event=event, branch=branch):
                    missing = sorted(path for path in paths if not self.covered(event, branch, path))
                    self.assertEqual(len(missing), 0,
                                     f"copied build inputs miss image CI, first 12: {missing[:12]}")

    def test_context_exclusion_changes_keep_image_checks(self):
        for event in ("pull_request", "push"):
            for branch in ("main", "develop"):
                with self.subTest(event=event, branch=branch):
                    self.assertTrue(self.covered(event, branch, ".dockerignore"))

    def test_other_branch_pushes_do_not_start_image_builds(self):
        self.assertFalse(self.covered("push", "codex/local-only", "Cargo.lock"))

    def test_documentation_still_does_not_replay_platform_images(self):
        for event in ("pull_request", "push"):
            for branch in ("main", "develop"):
                with self.subTest(event=event, branch=branch):
                    self.assertFalse(self.covered(event, branch, "docs/development/README.md"))


if __name__ == "__main__":
    unittest.main()
