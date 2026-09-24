"""Behavioural tests for the repeatable acceptance entry point (Issue #127).

These tests pin the parts a real defect would break: the journey map, the
fail-closed diagnostic codes, and the ``summary.json`` assembly and exit-code
rule. They never exercise the live cluster or the browser.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import sys
import tempfile
import unittest
from unittest import mock
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "tools/user_acceptance.py"
SPEC = importlib.util.spec_from_file_location("user_acceptance", SCRIPT)
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
# dataclasses resolve type annotations through sys.modules, so the module must
# be registered before it is executed.
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)


def write_private(path: Path, content: str = "secret") -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")
    os.chmod(path, 0o600)
    return path


def make_credentials(directory: Path) -> Path:
    for name in MODULE.CREDENTIAL_FILES.values():
        write_private(directory / name)
    return directory


def make_repo_root(directory: Path) -> Path:
    (directory / "web" / "node_modules" / "@playwright" / "test").mkdir(parents=True)
    binary = directory / "web" / "node_modules" / ".bin" / "playwright"
    binary.parent.mkdir(parents=True, exist_ok=True)
    binary.write_text("#!/usr/bin/env node\n", encoding="utf-8")
    return directory


def ok_http(url: str) -> tuple[int, str | None]:
    if url.endswith("/health/live") or url.endswith("/api/v1/auth/csrf"):
        return 200, None
    if url.endswith("/auth/login"):
        return 302, "https://keycloak.labweaver.2018wzh.top/realms/workloads/protocol/openid-connect/auth"
    return 404, None


def ok_kubectl(argv: list[str]) -> tuple[int, str, str]:
    return 0, "", ""


def ok_version(argv: list[str]) -> tuple[int, str]:
    return 0, "Version 1.55.0"


class JourneyMapTest(unittest.TestCase):
    def test_journey_map_matches_the_acceptance_table(self) -> None:
        expected = {
            "lab": (
                "teacher",
                "web/e2e/teacher/lab-experiment.live.spec.mjs",
                "student completes a published lab experiment through the browser terminal",
                {"LABWEAVER_E2E_LAB": "xv6"},
            ),
            "work": (
                "student",
                "web/e2e/student/sprint2-flow.live.spec.mjs",
                "student provisions a Work environment, configures it, and releases its capacity",
                {
                    "LABWEAVER_E2E_REAL_PROVIDER": "1",
                    "LABWEAVER_E2E_SECURITY_BASE_IMAGE": (
                        "harbor.lab.lan/labweaver-system/base-rust-builder"
                        "@sha256:14bc9c5966e7b3a385794b3d5389a8765668342025fbcc7b2e3d2866ac4bd8c3"
                    ),
                },
            ),
            "admin": (
                "platform-admin",
                "web/e2e/platform-admin/resource-approval.live.spec.mjs",
                "platform administrator approves a real resource request and reads back its lease and charges",
                {},
            ),
            "authoring": (
                "teacher",
                "web/e2e/teacher/authoring.live.spec.mjs",
                "teacher authors an independent project and publishes its complete experiment package",
                {},
            ),
        }
        self.assertEqual(set(MODULE.JOURNEYS), set(expected))
        for key, (project, spec, grep, extra) in expected.items():
            journey = MODULE.JOURNEYS[key]
            self.assertEqual(journey.project, project)
            self.assertEqual(journey.spec, spec)
            self.assertEqual(journey.grep, grep)
            self.assertEqual(dict(journey.extra_env), extra)

    def test_lab_journey_uses_the_requested_lab(self) -> None:
        environment = MODULE.journey_environment(MODULE.JOURNEYS["lab"], "cuda")
        self.assertEqual(environment["LABWEAVER_E2E_LAB"], "cuda")
        self.assertEqual(
            MODULE.journey_environment(MODULE.JOURNEYS["admin"], "cuda"), {}
        )

    def test_playwright_command_targets_the_journey_project_and_title(self) -> None:
        command = MODULE.playwright_command(MODULE.JOURNEYS["admin"])
        self.assertIn("web/node_modules/@playwright/test/cli.js", command)
        self.assertIn("--config=web/playwright.config.mjs", command)
        self.assertIn("--workers=1", command)
        self.assertEqual(command[command.index("--project") + 1], "platform-admin")
        self.assertEqual(
            command[command.index("--grep") + 1],
            "platform administrator approves a real resource request and reads back its lease and charges",
        )

    def test_unknown_journey_is_rejected(self) -> None:
        with self.assertRaises(MODULE.AcceptanceError) as context:
            MODULE.select_journeys(["lab", "nope"])
        self.assertEqual(str(context.exception), MODULE.JOURNEY_UNKNOWN)


class RunJourneyTest(unittest.TestCase):
    def build_args(self, tmp: Path, journeys: str = "lab,admin") -> object:
        credentials = make_credentials(tmp / "creds")
        return MODULE.build_parser().parse_args(
            [
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
                "--evidence-dir",
                str(tmp / "evidence"),
                "--model",
                "qwen3.6:27b",
            ]
        )

    def test_run_rejects_unknown_journey_before_running(self) -> None:
        calls: list[object] = []

        def execute(command, environment, stdout, stderr):  # noqa: ANN001
            calls.append(command)
            return 0

        with tempfile.TemporaryDirectory() as tmp:
            args = self.build_args(Path(tmp), journeys="nope")
            result = MODULE.run_acceptance(args, environ={}, execute=execute)
        self.assertNotEqual(result.exit_code, 0)
        self.assertEqual(result.diagnostics, [MODULE.JOURNEY_UNKNOWN])
        self.assertEqual(calls, [])

    def test_cli_fails_fast_on_unknown_journey(self) -> None:
        exit_code = MODULE.main(
            ["run", "--no-queue-wait", "--journeys", "nope", "--base-url", "https://example.invalid", "--run-id", "x"]
        )
        self.assertNotEqual(exit_code, 0)

    def test_run_reports_missing_credentials(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            args = MODULE.build_parser().parse_args(
                [
                    "run",
                    "--no-queue-wait",
                "--no-queue-wait",
                    "--no-queue-wait",
                    "--base-url",
                    "https://portal.example.test",
                    "--run-id",
                    "run-1",
                    "--journeys",
                    "admin",
                    "--credentials-dir",
                    str(Path(tmp) / "absent"),
                    "--evidence-dir",
                    str(Path(tmp) / "evidence"),
                    "--model",
                    "qwen3.6:27b",
                ]
            )
            result = MODULE.run_acceptance(args, environ={}, execute=lambda *args: 0)
        self.assertNotEqual(result.exit_code, 0)
        self.assertEqual(result.diagnostics, [MODULE.CREDENTIALS_MISSING])

    def test_run_reports_unwritable_evidence_directory(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            blocked = Path(tmp) / "evidence"
            blocked.write_text("not a directory", encoding="utf-8")
            args = self.build_args(Path(tmp), journeys="admin")
            args.evidence_dir = str(blocked)
            result = MODULE.run_acceptance(args, environ={}, execute=lambda *args: 0)
        self.assertNotEqual(result.exit_code, 0)
        self.assertEqual(result.diagnostics, [MODULE.EVIDENCE_DIR_UNWRITABLE])

    def test_run_waits_for_the_queue_before_starting_journeys(self) -> None:
        providers = json.dumps([{"providerKind": "container", "binding": "container-live-v1"}])

        def kubectl(argv):  # noqa: ANN001
            joined = " ".join(argv)
            if "agent-service-config" in argv:
                return 0, "qwen3.6:27b", ""
            if "providers" in joined:
                return 0, providers, ""
            if "agent_run_dispatches" in joined:
                return 0, "0", ""
            return 0, "{}", ""

        with tempfile.TemporaryDirectory() as tmp:
            credentials = make_credentials(Path(tmp) / "creds")
            args = MODULE.build_parser().parse_args(
                [
                    "run",
                    "--base-url",
                    "https://portal.example.test",
                    "--run-id",
                    "run-1",
                    "--journeys",
                    "admin",
                    "--credentials-dir",
                    str(credentials),
                    "--evidence-dir",
                    str(Path(tmp) / "evidence"),
                ]
            )
            with mock.patch.object(
                MODULE, "wait_for_authoring_queue", return_value=0
            ) as waiter:
                result = MODULE.run_acceptance(
                    args, environ={}, execute=lambda *args: 0, run_kubectl=kubectl
                )
        self.assertEqual(result.exit_code, 0)
        self.assertEqual(waiter.call_count, 1)

    def test_run_can_skip_the_queue_wait(self) -> None:
        providers = json.dumps([{"providerKind": "container", "binding": "container-live-v1"}])

        def kubectl(argv):  # noqa: ANN001
            joined = " ".join(argv)
            if "agent-service-config" in argv:
                return 0, "qwen3.6:27b", ""
            if "providers" in joined:
                return 0, providers, ""
            if "agent_run_dispatches" in joined:
                return 0, "0", ""
            return 0, "{}", ""

        with tempfile.TemporaryDirectory() as tmp:
            credentials = make_credentials(Path(tmp) / "creds")
            args = MODULE.build_parser().parse_args(
                [
                    "run",
                    "--base-url",
                    "https://portal.example.test",
                    "--run-id",
                    "run-1",
                    "--journeys",
                    "admin",
                    "--no-queue-wait",
                    "--credentials-dir",
                    str(credentials),
                    "--evidence-dir",
                    str(Path(tmp) / "evidence"),
                ]
            )
            with mock.patch.object(
                MODULE, "wait_for_authoring_queue", return_value=0
            ) as waiter:
                result = MODULE.run_acceptance(
                    args, environ={}, execute=lambda *args: 0, run_kubectl=kubectl
                )
        self.assertEqual(result.exit_code, 0)
        self.assertEqual(waiter.call_count, 0)

    def test_cancel_stale_cancels_every_non_terminal_run(self) -> None:
        calls: list[tuple[str, str]] = []

        def kubectl(argv):  # noqa: ANN001
            return 0, "run-a|project-a\nrun-b|project-b", ""

        def http(url, cookie, **kwargs):  # noqa: ANN001, ANN003
            calls.append((url.rsplit("/", 1)[-1], kwargs.get("method", "GET")))
            if url.endswith("/csrf"):
                return 200, b'{"token":"t"}', {}
            if kwargs.get("method") == "POST":
                return 202, b"{}", {}
            return 200, b"{}", {"etag": '"rev-1"'}

        with tempfile.TemporaryDirectory() as tmp:
            auth = Path(tmp)
            (auth / "student.json").write_text(
                json.dumps({"cookies": [{"name": "__Host-labweaver_session", "value": "v"}]}),
                encoding="utf-8",
            )
            with mock.patch.object(MODULE, "_http", http):
                results = MODULE.cancel_superseded_runs(
                    base_url="https://portal.example.test",
                    auth_dir=auth,
                    run_kubectl=kubectl,
                )

        self.assertEqual([r["outcome"] for r in results], ["http-202", "http-202"])
        self.assertEqual(calls.count(("cancel", "POST")), 2)

    def test_cancel_stale_keeps_the_requested_prefix(self) -> None:
        def kubectl(argv):  # noqa: ANN001
            return 0, "run-a|project-a\nrun-b|project-b", ""

        def http(url, cookie, **kwargs):  # noqa: ANN001, ANN003
            if url.endswith("/csrf"):
                return 200, b'{"token":"t"}', {}
            if kwargs.get("method") == "POST":
                return 202, b"{}", {}
            return 200, b"{}", {"etag": '"rev-1"'}

        with tempfile.TemporaryDirectory() as tmp:
            auth = Path(tmp)
            (auth / "student.json").write_text(
                json.dumps({"cookies": [{"name": "__Host-labweaver_session", "value": "v"}]}),
                encoding="utf-8",
            )
            with mock.patch.object(MODULE, "_http", http):
                results = MODULE.cancel_superseded_runs(
                    base_url="https://portal.example.test",
                    auth_dir=auth,
                    run_kubectl=kubectl,
                    keep_prefix="run-b",
                )

        self.assertEqual([r["run_id"] for r in results], ["run-a"])

    def test_execute_journey_stops_a_hung_browser(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            stdout_path = Path(tmp) / "journey.stdout.log"
            stderr_path = Path(tmp) / "journey.stderr.log"
            code = MODULE.execute_journey(
                [sys.executable, "-c", "import time; time.sleep(30)"],
                {},
                stdout_path,
                stderr_path,
                timeout=0.5,
            )
            self.assertEqual(code, MODULE.JOURNEY_TIMEOUT_EXIT_CODE)
            self.assertTrue(stderr_path.exists())

    def test_a_timed_out_journey_reports_its_own_diagnostic(self) -> None:
        def execute(command, environment, stdout_path, stderr_path):  # noqa: ANN001
            Path(stdout_path).write_text("", encoding="utf-8")
            Path(stderr_path).write_text("", encoding="utf-8")
            return MODULE.JOURNEY_TIMEOUT_EXIT_CODE

        def kubectl(argv):  # noqa: ANN001
            return 1, "", ""

        with tempfile.TemporaryDirectory() as tmp:
            args = argparse.Namespace(
                base_url="https://portal.example.test",
                run_id="run-timeout",
                journeys="lab",
                lab="xv6",
                evidence_dir=tmp,
                credentials_dir=Path(tmp) / "creds",
                model="qwen3.6:27b",
                provider_binding="container-primary-v1",
                no_queue_wait=True,
                bundle_sha256=None,
                package_manifest=None,
            )
            (Path(tmp) / "creds").mkdir(parents=True, exist_ok=True)
            for name in ("teacher.password", "student.password", "admin.password"):
                (Path(tmp) / "creds" / name).write_text("secret", encoding="utf-8")
            result = MODULE.run_acceptance(
                args,
                environ={MODULE.MODEL_ENV: "qwen3.6:27b"},
                execute=execute,
                run_kubectl=kubectl,
                git_commit=None,
                deployment_identity=None,
            )

        self.assertEqual(result.exit_code, 1)
        self.assertEqual(result.diagnostics, [f"lab:{MODULE.JOURNEY_TIMEOUT}"])
        self.assertEqual(result.summary["journeys"][0]["diagnostic"], MODULE.JOURNEY_TIMEOUT)

    def test_approve_pending_resource_requests_approves_reviewing_leases_only(self) -> None:
        approved_posts: list[str] = []

        def http(url, cookie, **kwargs):  # noqa: ANN001, ANN003
            if url.endswith("/resource-requests"):
                return 200, json.dumps(
                    [
                        {"id": "request-reviewing", "state": "reviewing", "targetKind": "task"},
                        {"id": "request-active", "state": "active", "targetKind": "task"},
                        {"id": "request-environment", "state": "reviewing", "targetKind": "environment"},
                    ]
                ).encode(), {}
            if url.endswith("/csrf"):
                return 200, b'{"csrfToken":"t"}', {}
            if kwargs.get("method") == "POST":
                approved_posts.append(url.split("/")[-2])
                return 202, b"{}", {}
            return 200, json.dumps(
                {
                    "id": url.rsplit("/", 1)[-1],
                    "state": "reviewing",
                    "targetKind": "task",
                    "revision": 3,
                    "requestedResources": {"cpuMillicores": 2000},
                    "requestedDurationSeconds": 3600,
                }
            ).encode(), {"etag": '"rev-3"'}

        with tempfile.TemporaryDirectory() as tmp:
            auth = Path(tmp) / "platform-admin.json"
            auth.write_text(
                json.dumps({"cookies": [{"name": "__Host-labweaver_session", "value": "v"}]}),
                encoding="utf-8",
            )
            with mock.patch.object(MODULE, "_http", http):
                approved = MODULE.approve_pending_resource_requests(
                    "https://portal.example.test", auth, "container-primary-v1"
                )

        self.assertEqual(approved, ["request-reviewing"])
        self.assertEqual(approved_posts, ["request-reviewing"])

    def test_approve_pending_resource_requests_needs_a_session(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            auth = Path(tmp) / "platform-admin.json"
            auth.write_text(json.dumps({"cookies": []}), encoding="utf-8")
            self.assertEqual(
                MODULE.approve_pending_resource_requests(
                    "https://portal.example.test", auth, "container-primary-v1"
                ),
                [],
            )

    def test_queue_wait_returns_as_soon_as_the_queue_is_empty(self) -> None:
        counts = iter([2, 1, 0])

        def kubectl(argv):  # noqa: ANN001
            return 0, str(next(counts, 0)), ""

        with mock.patch.object(MODULE, "QUEUE_WAIT_POLL_SECONDS", 0.0):
            self.assertEqual(MODULE.wait_for_authoring_queue(kubectl, 30.0), 0)

    def test_queue_wait_gives_up_at_the_timeout(self) -> None:
        def kubectl(argv):  # noqa: ANN001
            return 0, "3", ""

        # A busy queue must not spin forever: the wait is bounded and reports the
        # last observed depth so the caller can still start the run.
        self.assertEqual(MODULE.wait_for_authoring_queue(kubectl, 0.0), 3)

    def test_queue_wait_treats_an_unreadable_registry_as_empty(self) -> None:
        def kubectl(argv):  # noqa: ANN001
            return 1, "", "no postgres"

        self.assertIsNone(MODULE.wait_for_authoring_queue(kubectl, 30.0))

    def test_provider_binding_is_resolved_from_the_live_provider_registry(self) -> None:
        providers = json.dumps(
            [
                {"providerKind": "container", "binding": "container-live-v1"},
                {"providerKind": "kubevirt", "binding": "vm-primary-v1"},
            ]
        )

        def kubectl(argv):  # noqa: ANN001
            return 0, providers, ""

        self.assertEqual(
            MODULE.resolve_provider_binding(None, {}, kubectl), "container-live-v1"
        )
        self.assertEqual(
            MODULE.resolve_provider_binding(None, {"LABWEAVER_E2E_PROVIDER_BINDING": "env-v1"}, kubectl),
            "env-v1",
        )
        self.assertEqual(
            MODULE.resolve_provider_binding("flag-v1", {}, kubectl), "flag-v1"
        )

    def test_provider_binding_is_absent_when_the_registry_has_no_container_provider(self) -> None:
        def kubectl(argv):  # noqa: ANN001
            return 0, json.dumps([{"providerKind": "kubevirt", "binding": "vm-primary-v1"}]), ""

        self.assertEqual(MODULE.resolve_provider_binding(None, {}, kubectl), "")

    def test_run_reports_missing_model(self) -> None:
        def kubectl(argv):  # noqa: ANN001
            return 1, "", "no configmap"

        with tempfile.TemporaryDirectory() as tmp:
            credentials = make_credentials(Path(tmp) / "creds")
            args = MODULE.build_parser().parse_args(
                [
                    "run",
                    "--no-queue-wait",
                "--no-queue-wait",
                    "--no-queue-wait",
                    "--base-url",
                    "https://portal.example.test",
                    "--run-id",
                    "run-1",
                    "--journeys",
                    "admin",
                    "--credentials-dir",
                    str(credentials),
                    "--evidence-dir",
                    str(Path(tmp) / "evidence"),
                ]
            )
            result = MODULE.run_acceptance(
                args, environ={}, execute=lambda *args: 0, run_kubectl=kubectl
            )
        self.assertNotEqual(result.exit_code, 0)
        self.assertEqual(result.diagnostics, [MODULE.MODEL_MISSING])

    def test_summary_records_pass_through_identity_and_passing_status(self) -> None:
        environments: list[dict] = []

        def execute(command, environment, stdout, stderr):  # noqa: ANN001
            environments.append(environment)
            return 0

        with tempfile.TemporaryDirectory() as tmp:
            args = self.build_args(Path(tmp))
            args.package_manifest = "artifacts/package/manifest.json"
            args.bundle_sha256 = "sha256:deadbeef"
            args.lab = "cuda"
            result = MODULE.run_acceptance(
                args,
                environ={},
                execute=execute,
                git_commit="abc123",
                deployment_identity="sha256:live",
            )
            summary = json.loads(
                (Path(tmp) / "evidence" / "run-1" / "summary.json").read_text(encoding="utf-8")
            )
        self.assertEqual(result.exit_code, 0)
        self.assertEqual(summary["run_id"], "run-1")
        self.assertEqual(summary["base_url"], "https://portal.example.test")
        self.assertEqual(summary["git_commit"], "abc123")
        self.assertEqual(summary["package_manifest"], "artifacts/package/manifest.json")
        self.assertEqual(summary["helm_revision"], "sha256:live")
        self.assertEqual(summary["bundle_sha256"], "sha256:deadbeef")
        self.assertEqual([item["status"] for item in summary["journeys"]], ["passed", "passed"])
        self.assertEqual([item["key"] for item in summary["journeys"]], ["lab", "admin"])
        self.assertTrue(summary["started_at"])
        self.assertTrue(summary["finished_at"])
        self.assertEqual(environments[0]["LABWEAVER_E2E_LAB"], "cuda")
        self.assertEqual(environments[1]["LABWEAVER_E2E_PROVIDER_MODEL"], "qwen3.6:27b")
        self.assertNotIn("LABWEAVER_E2E_LAB", environments[1])
        self.assertEqual(environments[0]["LABWEAVER_BASE_URL"], "https://portal.example.test")
        self.assertEqual(environments[0]["LABWEAVER_IGNORE_HTTPS_ERRORS"], "1")
        self.assertEqual(environments[0]["LABWEAVER_TEACHER_USERNAME"], "platform-teacher")
        self.assertEqual(environments[0]["LABWEAVER_STUDENT_USERNAME"], "platform-student")
        self.assertEqual(environments[0]["LABWEAVER_PLATFORM_ADMIN_USERNAME"], "platform-admin")
        for role, name in MODULE.CREDENTIAL_FILES.items():
            variable = f"{MODULE.ROLE_ENV_PREFIX[role]}_PASSWORD_FILE"
            self.assertTrue(environments[0][variable].endswith(f"run-1/.credentials/{name}"))

    def test_any_failed_journey_fails_the_run(self) -> None:
        codes = iter([0, 1])

        def execute(command, environment, stdout, stderr):  # noqa: ANN001
            return next(codes)

        with tempfile.TemporaryDirectory() as tmp:
            args = self.build_args(Path(tmp))
            result = MODULE.run_acceptance(
                args,
                environ={},
                execute=execute,
                git_commit="abc123",
                deployment_identity=None,
                run_kubectl=lambda argv: (0, "", ""),
            )
        self.assertNotEqual(result.exit_code, 0)
        self.assertEqual(
            [item["status"] for item in result.summary["journeys"]], ["passed", "failed"]
        )
        self.assertEqual(result.summary["journeys"][1]["diagnostic"], MODULE.JOURNEY_FAILED)


class PreflightTest(unittest.TestCase):
    def preflight(self, tmp: Path, **overrides):  # noqa: ANN001
        repo_root = make_repo_root(tmp / "repo")
        browsers = tmp / "browsers"
        (browsers / "chromium-1228").mkdir(parents=True, exist_ok=True)
        credentials = make_credentials(tmp / "creds")
        arguments = [
            "preflight",
            "--base-url",
            "https://portal.example.test",
            "--credentials-dir",
            str(credentials),
            "--evidence-dir",
            str(tmp / "evidence"),
            "--model",
            "qwen3.6:27b",
        ]
        args = MODULE.build_parser().parse_args(arguments)
        environment = {"PLAYWRIGHT_BROWSERS_PATH": str(browsers)}
        probes = {
            "http": ok_http,
            "run_kubectl": ok_kubectl,
            "run_version": ok_version,
            "repo_root": repo_root,
        }
        probes.update(overrides)
        return MODULE.run_preflight(args, environ=environment, **probes)

    def test_preflight_passes_when_every_precondition_holds(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = self.preflight(Path(tmp))
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
                "evidence-dir",
                "authoring_queue",
            ],
        )

    def test_unreachable_cluster_is_reported(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = self.preflight(
                Path(tmp), run_kubectl=lambda argv: (1, "", "connection refused")
            )
        self.assertNotEqual(result.exit_code, 0)
        self.assertIn(MODULE.CLUSTER_UNREACHABLE, result.diagnostics)

    def test_unreachable_portal_is_reported(self) -> None:
        def http(url: str) -> tuple[int, str | None]:
            return 503, None

        with tempfile.TemporaryDirectory() as tmp:
            result = self.preflight(Path(tmp), http=http)
        self.assertNotEqual(result.exit_code, 0)
        self.assertIn(MODULE.PORTAL_UNREACHABLE, result.diagnostics)

    def test_login_redirect_to_a_non_keycloak_host_is_reported(self) -> None:
        def http(url: str) -> tuple[int, str | None]:
            if url.endswith("/auth/login"):
                return 302, "https://portal.example.test/auth/callback"
            return 200, None

        with tempfile.TemporaryDirectory() as tmp:
            result = self.preflight(Path(tmp), http=http)
        self.assertNotEqual(result.exit_code, 0)
        self.assertIn(MODULE.LOGIN_REDIRECT_INVALID, result.diagnostics)

    def test_missing_credentials_are_reported(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            args = MODULE.build_parser().parse_args(
                [
                    "preflight",
                    "--base-url",
                    "https://portal.example.test",
                    "--credentials-dir",
                    str(Path(tmp) / "absent"),
                    "--evidence-dir",
                    str(Path(tmp) / "evidence"),
                    "--model",
                    "qwen3.6:27b",
                ]
            )
            result = MODULE.run_preflight(
                args,
                environ={"PLAYWRIGHT_BROWSERS_PATH": str(Path(tmp) / "browsers")},
                http=ok_http,
                run_kubectl=ok_kubectl,
                run_version=ok_version,
                repo_root=make_repo_root(Path(tmp) / "repo"),
            )
        self.assertNotEqual(result.exit_code, 0)
        self.assertIn(MODULE.CREDENTIALS_MISSING, result.diagnostics)

    def test_world_readable_credentials_are_reported(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            credentials = make_credentials(Path(tmp) / "creds")
            os.chmod(credentials / "teacher.password", 0o644)
            repo_root = make_repo_root(Path(tmp) / "repo")
            browsers = Path(tmp) / "browsers"
            (browsers / "chromium-1228").mkdir(parents=True)
            args = MODULE.build_parser().parse_args(
                [
                    "preflight",
                    "--base-url",
                    "https://portal.example.test",
                    "--credentials-dir",
                    str(credentials),
                    "--evidence-dir",
                    str(Path(tmp) / "evidence"),
                    "--model",
                    "qwen3.6:27b",
                ]
            )
            result = MODULE.run_preflight(
                args,
                environ={"PLAYWRIGHT_BROWSERS_PATH": str(browsers)},
                http=ok_http,
                run_kubectl=ok_kubectl,
                run_version=ok_version,
                repo_root=repo_root,
            )
        self.assertNotEqual(result.exit_code, 0)
        self.assertIn(MODULE.CREDENTIALS_MISSING, result.diagnostics)

    def test_missing_model_is_reported(self) -> None:
        def kubectl(argv: list[str]) -> tuple[int, str, str]:
            return 1, "", "not found"

        with tempfile.TemporaryDirectory() as tmp:
            repo_root = make_repo_root(Path(tmp) / "repo")
            browsers = Path(tmp) / "browsers"
            (browsers / "chromium-1228").mkdir(parents=True)
            credentials = make_credentials(Path(tmp) / "creds")
            args = MODULE.build_parser().parse_args(
                [
                    "preflight",
                    "--base-url",
                    "https://portal.example.test",
                    "--credentials-dir",
                    str(credentials),
                    "--evidence-dir",
                    str(Path(tmp) / "evidence"),
                ]
            )
            result = MODULE.run_preflight(
                args,
                environ={"PLAYWRIGHT_BROWSERS_PATH": str(browsers)},
                http=ok_http,
                run_kubectl=kubectl,
                run_version=ok_version,
                repo_root=repo_root,
            )
        self.assertNotEqual(result.exit_code, 0)
        self.assertIn(MODULE.MODEL_MISSING, result.diagnostics)

    def test_missing_browser_is_reported(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            repo_root = Path(tmp) / "repo"
            (repo_root / "web" / "node_modules").mkdir(parents=True)
            browsers = Path(tmp) / "browsers"
            browsers.mkdir()
            credentials = make_credentials(Path(tmp) / "creds")
            args = MODULE.build_parser().parse_args(
                [
                    "preflight",
                    "--base-url",
                    "https://portal.example.test",
                    "--credentials-dir",
                    str(credentials),
                    "--evidence-dir",
                    str(Path(tmp) / "evidence"),
                    "--model",
                    "qwen3.6:27b",
                ]
            )
            result = MODULE.run_preflight(
                args,
                environ={"PLAYWRIGHT_BROWSERS_PATH": str(browsers)},
                http=ok_http,
                run_kubectl=ok_kubectl,
                run_version=ok_version,
                repo_root=repo_root,
            )
        self.assertNotEqual(result.exit_code, 0)
        self.assertIn(MODULE.BROWSER_MISSING, result.diagnostics)

    def test_unwritable_evidence_directory_is_reported(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            blocked = Path(tmp) / "evidence"
            blocked.write_text("not a directory", encoding="utf-8")
            repo_root = make_repo_root(Path(tmp) / "repo")
            browsers = Path(tmp) / "browsers"
            (browsers / "chromium-1228").mkdir(parents=True)
            credentials = make_credentials(Path(tmp) / "creds")
            args = MODULE.build_parser().parse_args(
                [
                    "preflight",
                    "--base-url",
                    "https://portal.example.test",
                    "--credentials-dir",
                    str(credentials),
                    "--evidence-dir",
                    str(blocked),
                    "--model",
                    "qwen3.6:27b",
                ]
            )
            result = MODULE.run_preflight(
                args,
                environ={"PLAYWRIGHT_BROWSERS_PATH": str(browsers)},
                http=ok_http,
                run_kubectl=ok_kubectl,
                run_version=ok_version,
                repo_root=repo_root,
            )
        self.assertNotEqual(result.exit_code, 0)
        self.assertIn(MODULE.EVIDENCE_DIR_UNWRITABLE, result.diagnostics)

    def test_anonymous_csrf_401_counts_as_reachable(self) -> None:
        """The public API route is up when access-service answers 401."""

        def http(url: str) -> tuple[int, str | None]:
            if url.endswith("/api/v1/auth/csrf"):
                return 401, None
            return ok_http(url)

        with tempfile.TemporaryDirectory() as tmp:
            result = self.preflight(Path(tmp), http=http)
        self.assertEqual(result.exit_code, 0)
        self.assertEqual(result.diagnostics, [])

    def test_deployment_identity_reads_only_the_platform_release(self) -> None:
        """The Resource release's own bundle must not hide the platform identity."""
        seen: list[list[str]] = []
        platform = {
            "metadata": {"name": "access-service"},
            "spec": {
                "template": {
                    "metadata": {"annotations": {MODULE.BUNDLE_ANNOTATION: "sha256:platform"}}
                }
            },
        }
        resource = {
            "metadata": {"name": "resource-service"},
            "spec": {
                "template": {
                    "metadata": {"annotations": {MODULE.BUNDLE_ANNOTATION: "sha256:resource"}}
                }
            },
        }

        def kubectl(argv: list[str]) -> tuple[int, str, str]:
            seen.append(list(argv))
            # Emulate the API server applying the requested label selector.
            selector = argv[argv.index("-l") + 1] if "-l" in argv else ""
            if selector == f"app.kubernetes.io/instance={MODULE.PLATFORM_RELEASE}":
                return 0, json.dumps({"items": [platform]}), ""
            return 0, json.dumps({"items": [platform, resource]}), ""

        identity = MODULE.probe_deployment_identity(kubectl)
        self.assertEqual(identity, "sha256:platform")
        self.assertIn(f"app.kubernetes.io/instance={MODULE.PLATFORM_RELEASE}", seen[0])


if __name__ == "__main__":
    unittest.main()
