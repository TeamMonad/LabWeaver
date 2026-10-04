"""Focused tests for the local public acceptance harness."""

from __future__ import annotations

import importlib.util
import io
import json
import os
import sys
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "tools/user_acceptance.py"
SPEC = importlib.util.spec_from_file_location("user_acceptance", SCRIPT)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)


def make_credentials(directory: Path) -> Path:
    directory.mkdir(parents=True, exist_ok=True)
    for name in MODULE.CREDENTIAL_FILES.values():
        path = directory / name
        path.write_text("secret", encoding="utf-8")
        os.chmod(path, 0o600)
    return directory


def make_repo_root(directory: Path) -> Path:
    (directory / "web" / "node_modules" / "@playwright" / "test").mkdir(
        parents=True
    )
    binary = directory / "web" / "node_modules" / ".bin" / "playwright"
    binary.parent.mkdir(parents=True, exist_ok=True)
    binary.write_text("#!/usr/bin/env node\n", encoding="utf-8")
    return directory


def ok_http(url: str) -> tuple[int, str | None]:
    if url.endswith("/health/live") or url.endswith("/api/v1/auth/csrf"):
        return 200, None
    if url.endswith("/auth/login"):
        return (
            302,
            "https://keycloak.labweaver.example/realms/workloads/protocol/openid-connect/auth",
        )
    return 404, None


def ok_kubectl(argv: list[str]) -> tuple[int, str, str]:
    return 0, "", ""


def ok_version(argv: list[str]) -> tuple[int, str]:
    return 0, "Version 1.55.0"


class AcceptanceHarnessTest(unittest.TestCase):
    def build_args(
        self,
        credentials: Path,
        journeys: str = "lab,admin",
        resume_target: tuple[str, str] | None = None,
    ) -> object:
        values = [
            "run",
            "--no-queue-wait",
            "--base-url",
            "https://portal.example.test",
            "--run-id",
            "run-1",
            "--journeys",
            journeys,
            "--credentials-dir",
            str(credentials),
            "--model",
            "glm-5.3",
            "--provider-binding",
            "container-primary-v1",
        ]
        if resume_target:
            values.extend(["--resume-project-id", resume_target[0], "--resume-run-id", resume_target[1]])
        return MODULE.build_parser().parse_args(values)

    def test_journey_map_contains_real_business_slices(self) -> None:
        self.assertEqual(
            MODULE.JOURNEYS["lab"].spec,
            "web/e2e/teacher/lab-experiment.live.spec.mjs",
        )
        self.assertEqual(
            MODULE.JOURNEYS["work"].spec,
            "web/e2e/student/sprint2-flow.live.spec.mjs",
        )
        self.assertEqual(MODULE.JOURNEYS["admin"].project, "platform-admin")

    def test_model_is_read_from_anthropic_model_without_cluster_fallback(self) -> None:
        self.assertEqual(MODULE.resolve_model(None, {"ANTHROPIC_MODEL": "glm-5.3"}), "glm-5.3")
        self.assertEqual(
            MODULE.resolve_model(
                None,
                {},
                lambda argv: (0, "qwen3.6:27b", ""),
            ),
            "",
        )

    def test_unknown_journey_is_rejected_before_subprocess(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            credentials = make_credentials(Path(temporary) / "credentials")
            args = self.build_args(credentials, "unknown")
            calls: list[object] = []
            result = MODULE.run_acceptance(
                args,
                environ={},
                execute=lambda *values: calls.append(values) or 0,
            )
        self.assertEqual(result.diagnostics, [MODULE.JOURNEY_UNKNOWN])
        self.assertEqual(calls, [])

    def test_existing_run_target_is_passed_only_to_the_lab_journey(self) -> None:
        project_id = "0197f0e0-0000-7000-8000-000000000013"
        run_id = "0197f0e0-0000-7000-8000-000000000014"
        environments: list[dict[str, str]] = []

        with tempfile.TemporaryDirectory() as temporary:
            credentials = make_credentials(Path(temporary) / "credentials")
            result = MODULE.run_acceptance(
                self.build_args(credentials, "lab,admin", (project_id, run_id)),
                environ={},
                execute=lambda _command, environment, _stdout, _stderr: environments.append(dict(environment)) or 0,
                run_kubectl=ok_kubectl,
                git_commit="abc123",
                deployment_identity="sha256:bundle",
            )

        self.assertEqual(result.exit_code, 0)
        self.assertEqual(environments[0]["LABWEAVER_E2E_LAB_RESUME_PROJECT_ID"], project_id)
        self.assertEqual(environments[0]["LABWEAVER_E2E_LAB_RESUME_RUN_ID"], run_id)
        self.assertNotIn("LABWEAVER_E2E_LAB_RESUME_PROJECT_ID", environments[1])
        self.assertNotIn("LABWEAVER_E2E_LAB_RESUME_RUN_ID", environments[1])
        self.assertNotIn("LABWEAVER_E2E_RESUME_PROJECT_ID", environments[0])
        self.assertNotIn("LABWEAVER_E2E_RESUME_RUN_ID", environments[0])

    def test_incomplete_or_unscoped_resume_target_is_rejected_before_subprocess(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            credentials = make_credentials(Path(temporary) / "credentials")
            calls: list[object] = []
            incomplete = self.build_args(credentials)
            incomplete.resume_project_id = "0197f0e0-0000-7000-8000-000000000013"
            invalid = MODULE.run_acceptance(
                incomplete,
                environ={},
                execute=lambda *values: calls.append(values) or 0,
            )
            unscoped = MODULE.run_acceptance(
                self.build_args(
                    credentials,
                    "admin",
                    ("0197f0e0-0000-7000-8000-000000000013", "0197f0e0-0000-7000-8000-000000000014"),
                ),
                environ={},
                execute=lambda *values: calls.append(values) or 0,
            )

        self.assertEqual(invalid.diagnostics, [MODULE.RESUME_TARGET_INVALID])
        self.assertEqual(unscoped.diagnostics, [MODULE.RESUME_TARGET_INVALID])
        self.assertEqual(calls, [])

    def test_run_removes_credentials_browser_state_and_logs(self) -> None:
        seen_paths: list[Path] = []

        def execute(command, environment, stdout, stderr):  # noqa: ANN001
            seen_paths.extend(
                [
                    Path(environment["LABWEAVER_TEACHER_PASSWORD_FILE"]),
                    Path(environment["LABWEAVER_AUTH_DIR"]),
                    Path(environment["LABWEAVER_PLAYWRIGHT_OUTPUT_DIR"]),
                    Path(stdout),
                    Path(stderr),
                ]
            )
            Path(stdout).write_text("console output", encoding="utf-8")
            Path(stderr).write_text("", encoding="utf-8")
            return 0

        with tempfile.TemporaryDirectory() as temporary:
            credentials = make_credentials(Path(temporary) / "credentials")
            result = MODULE.run_acceptance(
                self.build_args(credentials, "lab"),
                environ={},
                execute=execute,
                run_kubectl=ok_kubectl,
                git_commit="abc123",
                deployment_identity="sha256:bundle",
            )

        self.assertEqual(result.exit_code, 0)
        self.assertIsNotNone(result.summary)
        self.assertEqual(result.summary["run_id"], "run-1")
        self.assertEqual(result.summary["journeys"][0]["status"], "passed")
        self.assertTrue(seen_paths)
        self.assertTrue(all(not path.exists() for path in seen_paths))

    def test_failed_journey_is_not_reported_as_success(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            credentials = make_credentials(Path(temporary) / "credentials")
            result = MODULE.run_acceptance(
                self.build_args(credentials, "admin"),
                environ={},
                execute=lambda *values: 17,
                run_kubectl=ok_kubectl,
                git_commit="abc123",
                deployment_identity=None,
            )
        self.assertEqual(result.exit_code, 1)
        self.assertEqual(result.diagnostics, ["admin:LW_ACCEPTANCE_JOURNEY_FAILED"])
        self.assertEqual(result.summary["journeys"][0]["exit_code"], 17)

    def test_failed_journey_output_redacts_bearer_and_environment_secrets(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            credentials = make_credentials(Path(temporary) / "credentials")

            def execute(command, environment, stdout, stderr):  # noqa: ANN001
                Path(stdout).write_text(
                    "authorization: Bearer bearer-secret\nANTHROPIC_AUTH_TOKEN=env-secret\n",
                    encoding="utf-8",
                )
                Path(stderr).write_text("browser failed\n", encoding="utf-8")
                return 17

            output = io.StringIO()
            errors = io.StringIO()
            with redirect_stdout(output), redirect_stderr(errors):
                result = MODULE.run_acceptance(
                    self.build_args(credentials, "lab"),
                    environ={"ANTHROPIC_AUTH_TOKEN": "env-secret"},
                    execute=execute,
                    run_kubectl=ok_kubectl,
                    git_commit="abc123",
                    deployment_identity="sha256:bundle",
                )

        self.assertEqual(result.exit_code, 1)
        rendered = output.getvalue() + errors.getvalue()
        self.assertIn("authorization: <redacted>", rendered)
        self.assertIn("ANTHROPIC_AUTH_TOKEN=<redacted>", rendered)
        self.assertNotIn("bearer-secret", rendered)
        self.assertNotIn("env-secret", rendered)

    def test_cancel_resource_requires_exact_project_and_request_scope(self) -> None:
        calls: list[tuple[str, str]] = []

        def http(url, cookie, **kwargs):  # noqa: ANN001, ANN003
            calls.append((url, kwargs.get("method", "GET")))
            if url.endswith("/csrf"):
                return 200, b'{"csrfToken":"token"}', {}
            if kwargs.get("method") == "POST":
                return 202, b'{"state":"cancelled"}', {}
            return (
                200,
                b'{"projectId":"project-1","requestKey":"run-1:resource-1","state":"reviewing"}',
                {"etag": '"rev-3"'},
            )

        with tempfile.TemporaryDirectory() as temporary:
            auth_state = Path(temporary) / "admin.json"
            auth_state.write_text(
                json.dumps({"cookies": [{"name": "session", "value": "v"}]}),
                encoding="utf-8",
            )
            with mock.patch.object(MODULE, "_http", http):
                result = MODULE.cancel_resource_request(
                    base_url="https://portal.example.test",
                    auth_state=auth_state,
                    project_id="project-1",
                    request_id="resource-1",
                    request_key="run-1:resource-1",
                )

        self.assertEqual(result["state"], "cancelled")
        self.assertEqual(calls[-1][1], "POST")

    def test_cancel_resource_does_not_mutate_mismatched_request(self) -> None:
        calls: list[tuple[str, str]] = []

        def http(url, cookie, **kwargs):  # noqa: ANN001, ANN003
            calls.append((url, kwargs.get("method", "GET")))
            return (
                200,
                b'{"projectId":"other-project","requestKey":"other-key","state":"reviewing"}',
                {"etag": '"rev-1"'},
            )

        with tempfile.TemporaryDirectory() as temporary:
            auth_state = Path(temporary) / "admin.json"
            auth_state.write_text(
                json.dumps({"cookies": [{"name": "session", "value": "v"}]}),
                encoding="utf-8",
            )
            with mock.patch.object(MODULE, "_http", http):
                result = MODULE.cancel_resource_request(
                    base_url="https://portal.example.test",
                    auth_state=auth_state,
                    project_id="project-1",
                    request_id="resource-1",
                    request_key="run-1:resource-1",
                )

        self.assertIsNone(result)
        self.assertEqual(
            calls,
            [("https://portal.example.test/api/v1/resource-requests/resource-1", "GET")],
        )

    def test_execute_journey_stops_a_hung_browser(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            stdout = Path(temporary) / "stdout.log"
            stderr = Path(temporary) / "stderr.log"
            code = MODULE.execute_journey(
                [sys.executable, "-c", "import time; time.sleep(30)"],
                {},
                stdout,
                stderr,
                timeout=0.05,
            )
        self.assertEqual(code, MODULE.JOURNEY_TIMEOUT_EXIT_CODE)


class PreflightTest(unittest.TestCase):
    def test_preflight_passes_with_explicit_model_and_private_credentials(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = make_repo_root(Path(temporary) / "repo")
            browsers = Path(temporary) / "browsers"
            (browsers / "chromium").mkdir(parents=True)
            credentials = make_credentials(Path(temporary) / "credentials")
            args = MODULE.build_parser().parse_args(
                [
                    "preflight",
                    "--base-url",
                    "https://portal.example.test",
                    "--credentials-dir",
                    str(credentials),
                    "--model",
                    "glm-5.3",
                ]
            )
            result = MODULE.run_preflight(
                args,
                environ={"PLAYWRIGHT_BROWSERS_PATH": str(browsers)},
                http=ok_http,
                run_kubectl=ok_kubectl,
                run_version=ok_version,
                repo_root=root,
            )
        self.assertEqual(result.exit_code, 0)
        self.assertEqual(result.diagnostics, [])
        self.assertEqual(
            [check.name for check in result.checks],
            [
                "cluster",
                "portal-health",
                "portal-csrf",
                "login-redirect",
                "credentials",
                "model",
                "browser",
                "authoring_queue",
            ],
        )

    def test_preflight_reports_missing_model_without_cluster_lookup(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = make_repo_root(Path(temporary) / "repo")
            browsers = Path(temporary) / "browsers"
            (browsers / "chromium").mkdir(parents=True)
            credentials = make_credentials(Path(temporary) / "credentials")
            args = MODULE.build_parser().parse_args(
                [
                    "preflight",
                    "--base-url",
                    "https://portal.example.test",
                    "--credentials-dir",
                    str(credentials),
                ]
            )
            calls: list[list[str]] = []

            def kubectl(argv):  # noqa: ANN001
                calls.append(list(argv))
                return 0, "", ""

            result = MODULE.run_preflight(
                args,
                environ={"PLAYWRIGHT_BROWSERS_PATH": str(browsers)},
                http=ok_http,
                run_kubectl=kubectl,
                run_version=ok_version,
                repo_root=root,
            )
        self.assertIn(MODULE.MODEL_MISSING, result.diagnostics)
        self.assertTrue(calls)
        self.assertFalse(
            any("agent-service-config" in item for call in calls for item in call)
        )


if __name__ == "__main__":
    unittest.main()
