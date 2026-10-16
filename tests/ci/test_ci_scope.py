"""Exercise CI selection with real Git histories and conservative fallback cases."""
import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("ci_scope", ROOT / "tools/ci_scope.py")
ci_scope = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = ci_scope
SPEC.loader.exec_module(ci_scope)


class PathSelectionTests(unittest.TestCase):
    def test_documentation_and_handwritten_web_do_not_require_rust(self):
        for paths in (["README.md"], ["docs/development/README.md"],
                      ["web/src/views/Work.vue", "web/tests/work.spec.ts"],
                      ["docs/contracts/api.md", "web/package.json"]):
            with self.subTest(paths=paths):
                self.assertFalse(ci_scope.classify_paths(paths).rust_required)

    def test_shared_authority_execution_and_unknown_paths_require_full_rust(self):
        for path in ("crates/contracts/src/http.rs", "crates/auth/src/lib.rs",
                     "crates/persistence-sqlx/src/lib.rs", "Cargo.lock",
                     "services/resource-service/src/credits/mod.rs",
                     "services/agent-service/tests/platform_image_import_postgres.rs",
                     "migrations/catalog.yaml", "schemas/openapi/api.json",
                     "vendor/sqlx-postgres/src/transaction.rs", "xtask/src/main.rs",
                     "deploy/versions.lock.yml", "new-module/check.py",
                     "tools/ci_scope.py", "tests/ci/test_ci_scope.py",
                     ".github/workflows/rust.yml", "AGENTS.md",
                     "web/wasm/src/lib.rs", "web/wasm/Cargo.toml", "web/new.unknown"):
            with self.subTest(path=path):
                self.assertTrue(ci_scope.classify_paths([path]).rust_required)

    def test_generated_sdk_still_checks_its_rust_source(self):
        for path in ("web/src/generated/contracts/types.gen.ts", "web/src/generated"):
            with self.subTest(path=path):
                self.assertTrue(ci_scope.classify_paths([path]).rust_required)

    def test_mixed_changes_keep_affected_rust_checks(self):
        self.assertTrue(ci_scope.classify_paths([
            "web/src/App.vue", "docs/test.md", "Cargo.toml"]).rust_required)

    def test_empty_or_unexpected_paths_cannot_disable_checks(self):
        for paths in ([], [""], ["web/../Cargo.toml"], ["/web/file"],
                      ["web/a\nb.vue"], ["web//App.vue"], ["docs/code.py"]):
            with self.subTest(paths=paths):
                self.assertTrue(ci_scope.classify_paths(paths).rust_required)


class GitSelectionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name)
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.name", "CI fixture")
        self.git("config", "user.email", "ci@example.invalid")
        self.initial = self.commit("Cargo.toml", "initial")

    def git(self, *args):
        return subprocess.check_output(["git", *args], cwd=self.repo,
                                       stderr=subprocess.DEVNULL).decode().strip()

    def commit(self, path, content):
        file = self.repo / path
        file.parent.mkdir(parents=True, exist_ok=True)
        file.write_text(content)
        self.git("add", "--all")
        self.git("commit", "-qm", "fixture change")
        return self.git("rev-parse", "HEAD")

    def select(self, event="pull_request", **kwargs):
        env = {"EVENT_NAME": event, "BASE_SHA": self.initial,
               "HEAD_SHA": self.git("rev-parse", "HEAD")}
        env.update(kwargs)
        return ci_scope.select_scope(env, self.repo)

    def test_frontend_pr_uses_merge_base_when_target_branch_advances(self):
        self.git("checkout", "-qb", "feature")
        head = self.commit("web/src/App.vue", "frontend")
        self.git("checkout", "-q", "main")
        base = self.commit("crates/auth/src/lib.rs", "other PR")
        self.assertFalse(self.select(BASE_SHA=base, HEAD_SHA=head).rust_required)

    def test_push_includes_every_commit_since_before(self):
        self.commit("services/agent-service/src/lib.rs", "backend")
        head = self.commit("docs/readme.md", "docs")
        self.assertTrue(self.select(event="push", BEFORE_SHA=self.initial,
                                    AFTER_SHA=head).rust_required)

    def test_documentation_push_can_omit_rust(self):
        head = self.commit("docs/operations.md", "docs")
        self.assertFalse(self.select(event="push", BEFORE_SHA=self.initial,
                                     AFTER_SHA=head).rust_required)

    def test_rust_to_web_rename_keeps_the_deleted_source(self):
        base = self.commit("services/agent-service/src/original.rs", "same content")
        target = self.repo / "web/original.vue"
        target.parent.mkdir(parents=True)
        self.git("mv", "services/agent-service/src/original.rs", "web/original.vue")
        self.git("commit", "-qm", "rename")
        self.assertTrue(self.select(BASE_SHA=base).rust_required)

    def test_rust_deletion_is_not_lost(self):
        base = self.commit("crates/auth/src/old.rs", "old")
        self.git("rm", "crates/auth/src/old.rs")
        self.git("commit", "-qm", "delete")
        self.assertTrue(self.select(BASE_SHA=base).rust_required)

    def test_first_push_and_unknown_events_require_full_checks(self):
        self.assertTrue(self.select(event="push", BEFORE_SHA="0" * 40,
                                    AFTER_SHA=self.initial).rust_required)
        self.assertTrue(self.select(event="workflow_dispatch").rust_required)

    def test_missing_invalid_or_unavailable_commits_require_full_checks(self):
        for value in ("", "f" * 40, "--output=unexpected"):
            with self.subTest(value=value):
                self.assertTrue(self.select(BASE_SHA=value).rust_required)

    def test_unavailable_repository_requires_full_checks(self):
        self.assertTrue(ci_scope.select_scope({"EVENT_NAME": "pull_request",
                        "BASE_SHA": self.initial, "HEAD_SHA": self.initial},
                        self.repo / "missing").rust_required)

    def test_cli_emits_only_fixed_scope_fields(self):
        head = self.commit("docs/operations.md", "docs")
        env = dict(os.environ, EVENT_NAME="push", BEFORE_SHA=self.initial,
                   AFTER_SHA=head)
        output = subprocess.check_output([sys.executable, str(ROOT / "tools/ci_scope.py")],
                                         cwd=self.repo, env=env).decode().splitlines()
        self.assertEqual(output, ["rust_required=false", "scope_reason=documentation_or_web"])


if __name__ == "__main__":
    unittest.main()
