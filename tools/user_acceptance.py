#!/usr/bin/env python3
"""Repeatable acceptance entry point for the public LabWeaver portal (Issue #127).

The tool has two subcommands:

* ``preflight`` asserts every precondition the browser journeys need (cluster
  reachable, public portal serving, Keycloak redirect, private credentials,
  provider model, Playwright browser, writable evidence directory) and fails
  closed with a stable ``LW_ACCEPTANCE_*`` diagnostic code.
* ``run`` materialises the private credentials into a private directory,
  assembles the Playwright environment and drives each selected journey
  through the real browser harness, then collects the run evidence and writes
  ``summary.json``.

Every check fails with a stable diagnostic code so a failed run can be
classified without reading the logs.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import stat
import subprocess
import sys
import urllib.error
import urllib.request
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Callable, Mapping, Sequence
from urllib.parse import urlparse

ROOT = Path(__file__).resolve().parents[1]

KUBECTL_CONTEXT = "kubernetes-admin@kubernetes"
NAMESPACE = "labweaver-system"
BUNDLE_ANNOTATION = "labweaver.io/configuration-bundle-sha256"
MODEL_ENV = "LABWEAVER_E2E_PROVIDER_MODEL"
DEFAULT_CREDENTIALS_DIR = Path("/home/wzh/.private/labweaver-acceptance/credentials")
DEFAULT_EVIDENCE_DIR = "artifacts/acceptance"
DEFAULT_JOURNEYS = "lab,work,admin"
DEFAULT_LAB = "xv6"

# The provider model is a lazily fetched identifier: resolve it from the cluster
# so the acceptance run never hard-codes a deployment-specific model name.
MODEL_CONFIG_MAP = "agent-service-config"
MODEL_CONFIG_MAP_KEY = "anthropic-model"

# Stable diagnostics, prefixed so a caller can classify a failure mechanically.
CLUSTER_UNREACHABLE = "LW_ACCEPTANCE_CLUSTER_UNREACHABLE"
PORTAL_UNREACHABLE = "LW_ACCEPTANCE_PORTAL_UNREACHABLE"
LOGIN_REDIRECT_INVALID = "LW_ACCEPTANCE_LOGIN_REDIRECT_INVALID"
CREDENTIALS_MISSING = "LW_ACCEPTANCE_CREDENTIALS_MISSING"
MODEL_MISSING = "LW_ACCEPTANCE_MODEL_MISSING"
BROWSER_MISSING = "LW_ACCEPTANCE_BROWSER_MISSING"
EVIDENCE_DIR_UNWRITABLE = "LW_ACCEPTANCE_EVIDENCE_DIR_UNWRITABLE"
JOURNEY_UNKNOWN = "LW_ACCEPTANCE_JOURNEY_UNKNOWN"
JOURNEY_FAILED = "LW_ACCEPTANCE_JOURNEY_FAILED"
RUN_ID_INVALID = "LW_ACCEPTANCE_RUN_ID_INVALID"

# role -> private credential file name inside the credentials directory.
CREDENTIAL_FILES: Mapping[str, str] = {
    "teacher": "teacher.password",
    "student": "student.password",
    "admin": "admin.password",
}
# role -> fixed Keycloak username of the platform account.
ROLE_USERNAMES: Mapping[str, str] = {
    "teacher": "platform-teacher",
    "student": "platform-student",
    "admin": "platform-admin",
}
# role -> environment variable prefix expected by web/e2e/setup/auth.setup.mjs.
ROLE_ENV_PREFIX: Mapping[str, str] = {
    "teacher": "LABWEAVER_TEACHER",
    "student": "LABWEAVER_STUDENT",
    "admin": "LABWEAVER_PLATFORM_ADMIN",
}


class AcceptanceError(Exception):
    """Fail-closed acceptance diagnostic carrying a stable code."""

    def __init__(self, code: str, detail: str = "") -> None:
        super().__init__(code)
        self.code = code
        self.detail = detail

    def __str__(self) -> str:  # pragma: no cover - trivial
        return self.code


@dataclass(frozen=True)
class Journey:
    """One browser journey: project, spec, grep title and extra environment."""

    key: str
    project: str
    spec: str
    grep: str
    extra_env: Mapping[str, str]


# Hard-coded journey map; keys are the only accepted ``--journeys`` values.
JOURNEYS: Mapping[str, Journey] = {
    "lab": Journey(
        key="lab",
        project="teacher",
        spec="web/e2e/teacher/lab-experiment.live.spec.mjs",
        grep="student completes a published lab experiment through the browser terminal",
        extra_env={"LABWEAVER_E2E_LAB": DEFAULT_LAB},
    ),
    "work": Journey(
        key="work",
        project="student",
        spec="web/e2e/student/sprint2-flow.live.spec.mjs",
        grep="student provisions a Work environment, configures it, and releases its capacity",
        extra_env={
            "LABWEAVER_E2E_REAL_PROVIDER": "1",
            "LABWEAVER_E2E_SECURITY_BASE_IMAGE": (
                "harbor.lab.lan/labweaver-system/base-rust-builder"
                "@sha256:14bc9c5966e7b3a385794b3d5389a8765668342025fbcc7b2e3d2866ac4bd8c3"
            ),
        },
    ),
    "admin": Journey(
        key="admin",
        project="platform-admin",
        spec="web/e2e/platform-admin/resource-approval.live.spec.mjs",
        grep=(
            "platform administrator approves a real resource request and reads "
            "back its lease and charges"
        ),
        extra_env={},
    ),
    "authoring": Journey(
        key="authoring",
        project="teacher",
        spec="web/e2e/teacher/authoring.live.spec.mjs",
        grep=(
            "teacher authors an independent project and publishes its complete "
            "experiment package"
        ),
        extra_env={},
    ),
}


@dataclass(frozen=True)
class Check:
    """One preflight assertion."""

    name: str
    ok: bool
    code: str | None = None
    detail: str = ""


@dataclass
class PreflightResult:
    checks: list[Check]
    exit_code: int

    @property
    def diagnostics(self) -> list[str]:
        codes: list[str] = []
        for check in self.checks:
            if not check.ok and check.code and check.code not in codes:
                codes.append(check.code)
        return codes


@dataclass
class RunResult:
    exit_code: int
    diagnostics: list[str]
    summary: dict | None = None


def select_journeys(keys: Sequence[str]) -> list[Journey]:
    """Resolve journey keys in order, rejecting anything not in the map."""

    selected: list[Journey] = []
    seen: set[str] = set()
    for key in keys:
        journey = JOURNEYS.get(key)
        if journey is None:
            raise AcceptanceError(JOURNEY_UNKNOWN, f"unknown journey: {key}")
        if key not in seen:
            seen.add(key)
            selected.append(journey)
    if not selected:
        raise AcceptanceError(JOURNEY_UNKNOWN, "no journeys selected")
    return selected


def journey_environment(journey: Journey, lab: str) -> dict[str, str]:
    """Environment additions for one journey; ``--lab`` overrides the default."""

    environment = dict(journey.extra_env)
    if journey.key == "lab":
        environment["LABWEAVER_E2E_LAB"] = lab
    return environment


def acceptance_environment(
    *,
    base_url: str,
    credentials_dir: Path,
    model: str,
    environ: Mapping[str, str],
) -> dict[str, str]:
    """The full Playwright environment for a journey invocation."""

    environment = dict(environ)
    environment["LABWEAVER_BASE_URL"] = base_url
    environment["LABWEAVER_IGNORE_HTTPS_ERRORS"] = "1"
    environment[MODEL_ENV] = model
    for role, username in ROLE_USERNAMES.items():
        prefix = ROLE_ENV_PREFIX[role]
        environment[f"{prefix}_USERNAME"] = username
        environment[f"{prefix}_PASSWORD_FILE"] = str(credentials_dir / CREDENTIAL_FILES[role])
    return environment


def playwright_command(journey: Journey) -> list[str]:
    return [
        "node",
        "web/node_modules/@playwright/test/cli.js",
        "test",
        "--config=web/playwright.config.mjs",
        "--workers=1",
        "--project",
        journey.project,
        "--grep",
        journey.grep,
    ]


def build_summary(
    *,
    run_id: str,
    base_url: str,
    git_commit: str | None,
    package_manifest: str | None,
    helm_revision: str | None,
    bundle_sha256: str | None,
    journeys: Sequence[Mapping[str, object]],
    started_at: str,
    finished_at: str,
) -> dict:
    return {
        "run_id": run_id,
        "base_url": base_url,
        "git_commit": git_commit,
        "package_manifest": package_manifest,
        "helm_revision": helm_revision,
        "bundle_sha256": bundle_sha256,
        "started_at": started_at,
        "finished_at": finished_at,
        "journeys": list(journeys),
    }


def journey_exit_code(journeys: Sequence[Mapping[str, object]]) -> int:
    """0 only when every selected journey passed."""

    if not journeys:
        return 1
    return 0 if all(item.get("status") == "passed" for item in journeys) else 1


# --- probes ---------------------------------------------------------------


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    """Return the redirect response itself instead of following it."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):  # noqa: ANN001
        return None


def http_get(url: str, timeout: float = 15.0) -> tuple[int | None, str | None]:
    """Return ``(status, location)``; ``None`` status means no HTTP response."""

    opener = urllib.request.build_opener(_NoRedirect)
    request = urllib.request.Request(url, method="GET", headers={"Accept": "*/*"})
    try:
        with opener.open(request, timeout=timeout) as response:
            return response.status, response.headers.get("Location")
    except urllib.error.HTTPError as error:
        location = error.headers.get("Location") if error.headers else None
        return error.code, location
    except Exception:
        return None, None


def kubectl(argv: Sequence[str], timeout: float = 30.0) -> tuple[int, str, str]:
    try:
        completed = subprocess.run(
            list(argv), capture_output=True, text=True, timeout=timeout
        )
        return completed.returncode, completed.stdout.strip(), completed.stderr.strip()
    except Exception as error:  # pragma: no cover - environment dependent
        return 1, "", str(error)


def run_command(argv: Sequence[str], timeout: float = 60.0) -> tuple[int, str]:
    try:
        completed = subprocess.run(
            list(argv), capture_output=True, text=True, timeout=timeout
        )
        return completed.returncode, (completed.stdout or completed.stderr).strip()
    except Exception as error:  # pragma: no cover - environment dependent
        return 1, str(error)


def resolve_model(
    explicit: str | None,
    environ: Mapping[str, str],
    run_kubectl: Callable[[Sequence[str]], tuple[int, str, str]],
) -> str:
    """Resolve the provider model from the flag, the environment or the cluster."""

    value = (explicit or environ.get(MODEL_ENV) or "").strip()
    if value:
        return value
    code, stdout, _ = run_kubectl(
        [
            "kubectl",
            "--context",
            KUBECTL_CONTEXT,
            "-n",
            NAMESPACE,
            "get",
            "cm",
            MODEL_CONFIG_MAP,
            "-o",
            f"jsonpath={{.data.{MODEL_CONFIG_MAP_KEY}}}",
        ]
    )
    return stdout.strip() if code == 0 else ""


def probe_git_commit() -> str | None:
    try:
        completed = subprocess.run(
            ["git", "rev-parse", "HEAD"],
            cwd=ROOT,
            capture_output=True,
            text=True,
            timeout=30.0,
        )
    except Exception:  # pragma: no cover - environment dependent
        return None
    value = completed.stdout.strip()
    return value if completed.returncode == 0 and value else None


def probe_deployment_identity(
    run_kubectl: Callable[[Sequence[str]], tuple[int, str, str]],
) -> str | None:
    """The unique ``configuration-bundle-sha256`` annotation across workloads."""

    code, stdout, _ = run_kubectl(
        [
            "kubectl",
            "--context",
            KUBECTL_CONTEXT,
            "-n",
            NAMESPACE,
            "get",
            "deploy",
            "-o",
            "json",
        ]
    )
    if code != 0 or not stdout:
        return None
    try:
        payload = json.loads(stdout)
    except ValueError:
        return None
    values = set()
    for item in payload.get("items", []):
        annotations = item.get("spec", {}).get("template", {}).get("metadata", {}).get("annotations", {})
        value = annotations.get(BUNDLE_ANNOTATION)
        if value:
            values.add(value)
    return values.pop() if len(values) == 1 else None


# --- preflight checks -----------------------------------------------------


def _join(base_url: str, path: str) -> str:
    return base_url.rstrip("/") + path


def check_cluster(run_kubectl: Callable[[Sequence[str]], tuple[int, str, str]]) -> Check:
    command = [
        "kubectl",
        "--context",
        KUBECTL_CONTEXT,
        "-n",
        NAMESPACE,
        "get",
        "ns",
    ]
    code, _, stderr = run_kubectl(command)
    ok = code == 0
    detail = " ".join(command)
    if not ok:
        detail = f"{detail}: {stderr or f'exit {code}'}"
    return Check("cluster", ok, None if ok else CLUSTER_UNREACHABLE, detail)


def check_portal_health(
    base_url: str, http: Callable[[str], tuple[int | None, str | None]]
) -> Check:
    url = _join(base_url, "/health/live")
    status, _ = http(url)
    ok = status == 200
    return Check("portal-health", ok, None if ok else PORTAL_UNREACHABLE, f"GET {url} -> {status}")


def check_portal_csrf(
    base_url: str, http: Callable[[str], tuple[int | None, str | None]]
) -> Check:
    """Prove the public API route reaches access-service.

    An anonymous request has no session cookie, so access-service answers the
    CSRF bootstrap with 401; that is the reachable, authenticated-required
    contract. Only a missing answer or a server error means the route is down.
    """
    url = _join(base_url, "/api/v1/auth/csrf")
    status, _ = http(url)
    ok = status in {200, 401}
    return Check("portal-csrf", ok, None if ok else PORTAL_UNREACHABLE, f"GET {url} -> {status}")


def check_login_redirect(
    base_url: str, http: Callable[[str], tuple[int | None, str | None]]
) -> Check:
    url = _join(base_url, "/auth/login")
    status, location = http(url)
    host = urlparse(location).hostname if location else None
    ok = bool(status and 300 <= status < 400 and host and "keycloak" in host)
    detail = f"GET {url} -> {status}, Location={location}"
    return Check("login-redirect", ok, None if ok else LOGIN_REDIRECT_INVALID, detail)


def check_credentials(directory: Path) -> Check:
    problems: list[str] = []
    for role, name in CREDENTIAL_FILES.items():
        path = directory / name
        try:
            metadata = path.stat()
        except OSError:
            problems.append(f"{name}: missing")
            continue
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_size == 0:
            problems.append(f"{name}: empty")
            continue
        if metadata.st_mode & stat.S_IROTH:
            problems.append(f"{name}: world-readable")
    ok = not problems
    detail = str(directory) if ok else "; ".join(problems)
    return Check("credentials", ok, None if ok else CREDENTIALS_MISSING, detail)


def check_model(
    model: str,
    run_kubectl: Callable[[Sequence[str]], tuple[int, str, str]],
) -> Check:
    ok = bool(model.strip())
    return Check(
        "model",
        ok,
        None if ok else MODEL_MISSING,
        model or f"{MODEL_ENV} unset and {NAMESPACE}/{MODEL_CONFIG_MAP} has no model",
    )


def check_browser(
    repo_root: Path,
    browsers_path: Path,
    run_version: Callable[[Sequence[str]], tuple[int, str]],
) -> Check:
    package = repo_root / "web" / "node_modules" / "@playwright" / "test"
    binary = repo_root / "web" / "node_modules" / ".bin" / "playwright"
    problems: list[str] = []
    if not package.exists():
        problems.append("web/node_modules/@playwright/test missing")
    if not binary.exists():
        problems.append("web/node_modules/.bin/playwright missing")
    else:
        code, output = run_version([str(binary), "--version"])
        if code != 0:
            problems.append(f"playwright --version failed: {output}")
    if not browsers_path.is_dir() or not any(browsers_path.iterdir()):
        problems.append(f"no browsers in {browsers_path}")
    ok = not problems
    return Check("browser", ok, None if ok else BROWSER_MISSING, "; ".join(problems) or str(browsers_path))


def check_evidence_dir(directory: Path) -> Check:
    try:
        directory.mkdir(parents=True, exist_ok=True)
        probe = directory / ".write-probe"
        probe.write_text("", encoding="utf-8")
        probe.unlink()
    except OSError as error:
        return Check("evidence-dir", False, EVIDENCE_DIR_UNWRITABLE, f"{directory}: {error}")
    return Check("evidence-dir", True, None, str(directory))


def preflight_checks(
    args: argparse.Namespace,
    *,
    environ: Mapping[str, str],
    http: Callable[[str], tuple[int | None, str | None]],
    run_kubectl: Callable[[Sequence[str]], tuple[int, str, str]],
    run_version: Callable[[Sequence[str]], tuple[int, str]],
    repo_root: Path,
) -> list[Check]:
    model = resolve_model(args.model, environ, run_kubectl)
    browsers_path = Path(
        environ.get("PLAYWRIGHT_BROWSERS_PATH")
        or (Path.home() / ".cache" / "ms-playwright")
    )
    return [
        check_cluster(run_kubectl),
        check_portal_health(args.base_url, http),
        check_portal_csrf(args.base_url, http),
        check_login_redirect(args.base_url, http),
        check_credentials(Path(args.credentials_dir)),
        check_model(model, run_kubectl),
        check_browser(repo_root, browsers_path, run_version),
        check_evidence_dir(Path(args.evidence_dir)),
    ]


def format_check(check: Check) -> str:
    parts = ["OK" if check.ok else "FAIL", check.name]
    if check.code:
        parts.append(f"[{check.code}]")
    if check.detail:
        parts.append(check.detail)
    return " ".join(parts)


def run_preflight(
    args: argparse.Namespace,
    *,
    environ: Mapping[str, str] | None = None,
    http: Callable[[str], tuple[int | None, str | None]] | None = None,
    run_kubectl: Callable[[Sequence[str]], tuple[int, str, str]] | None = None,
    run_version: Callable[[Sequence[str]], tuple[int, str]] | None = None,
    repo_root: Path | None = None,
) -> PreflightResult:
    checks = preflight_checks(
        args,
        environ=os.environ if environ is None else environ,
        http=http_get if http is None else http,
        run_kubectl=kubectl if run_kubectl is None else run_kubectl,
        run_version=run_command if run_version is None else run_version,
        repo_root=ROOT if repo_root is None else repo_root,
    )
    exit_code = 0 if all(check.ok for check in checks) else 1
    return PreflightResult(checks=checks, exit_code=exit_code)


# --- run ------------------------------------------------------------------


def validate_run_id(run_id: str) -> str:
    value = run_id.strip()
    if not value or value in {".", ".."} or "/" in value or os.sep in value:
        raise AcceptanceError(RUN_ID_INVALID, f"invalid run id: {run_id!r}")
    return value


def prepare_run_directory(run_dir: Path) -> None:
    """Create the per-run evidence directory, failing with a stable code."""

    try:
        run_dir.mkdir(parents=True, exist_ok=True)
    except OSError as error:
        raise AcceptanceError(EVIDENCE_DIR_UNWRITABLE, f"{run_dir}: {error}") from error


def materialise_credentials(source_dir: Path, target_dir: Path) -> None:
    """Copy the private credentials into ``target_dir`` with 0700/0600 modes."""

    try:
        target_dir.mkdir(parents=True, exist_ok=True)
        os.chmod(target_dir, 0o700)
    except OSError as error:
        raise AcceptanceError(EVIDENCE_DIR_UNWRITABLE, f"{target_dir}: {error}") from error
    for name in CREDENTIAL_FILES.values():
        source = source_dir / name
        try:
            payload = source.read_bytes()
        except OSError as error:
            raise AcceptanceError(CREDENTIALS_MISSING, f"{source}: {error}") from error
        if not payload.strip():
            raise AcceptanceError(CREDENTIALS_MISSING, f"{source}: empty")
        destination = target_dir / name
        try:
            destination.write_bytes(payload)
            os.chmod(destination, 0o600)
        except OSError as error:
            raise AcceptanceError(
                EVIDENCE_DIR_UNWRITABLE, f"{destination}: {error}"
            ) from error


def collect_artifacts(report_dir: Path, results_dir: Path, target_dir: Path) -> None:
    target_dir.mkdir(parents=True, exist_ok=True)
    for name in ("report.json", "index.html"):
        source = report_dir / name
        if source.is_file():
            shutil.copy2(source, target_dir / name)
    if results_dir.is_dir():
        shutil.copytree(results_dir, target_dir / "test-results", dirs_exist_ok=True)


def execute_journey(
    command: Sequence[str],
    environment: Mapping[str, str],
    stdout_path: Path,
    stderr_path: Path,
) -> int:
    with stdout_path.open("w", encoding="utf-8") as stdout, stderr_path.open(
        "w", encoding="utf-8"
    ) as stderr:
        completed = subprocess.run(
            list(command), cwd=ROOT, env=dict(environment), stdout=stdout, stderr=stderr
        )
    return completed.returncode


def run_acceptance(
    args: argparse.Namespace,
    *,
    environ: Mapping[str, str] | None = None,
    execute: Callable[[Sequence[str], Mapping[str, str], Path, Path], int] | None = None,
    run_kubectl: Callable[[Sequence[str]], tuple[int, str, str]] | None = None,
    git_commit: str | None = None,
    deployment_identity: str | None = None,
) -> RunResult:
    environment = dict(os.environ if environ is None else environ)
    run_kubectl = kubectl if run_kubectl is None else run_kubectl
    execute = execute_journey if execute is None else execute

    # Reject unknown journeys before anything else runs.
    try:
        keys = [item.strip() for item in args.journeys.split(",") if item.strip()]
        selected = select_journeys(keys)
        run_id = validate_run_id(args.run_id)
    except AcceptanceError as error:
        return RunResult(exit_code=2, diagnostics=[error.code])

    model = resolve_model(args.model, environment, run_kubectl)
    if not model:
        return RunResult(exit_code=2, diagnostics=[MODEL_MISSING])

    evidence_root = Path(args.evidence_dir).resolve()
    run_dir = evidence_root / run_id
    credentials_dir = run_dir / ".credentials"
    try:
        prepare_run_directory(run_dir)
        materialise_credentials(Path(args.credentials_dir), credentials_dir)
    except AcceptanceError as error:
        return RunResult(exit_code=2, diagnostics=[error.code])

    environment = acceptance_environment(
        base_url=args.base_url,
        credentials_dir=credentials_dir,
        model=model,
        environ=environment,
    )

    if git_commit is None:
        git_commit = probe_git_commit()
    if deployment_identity is None:
        deployment_identity = probe_deployment_identity(run_kubectl)
    bundle_sha256 = args.bundle_sha256 or deployment_identity

    report_dir = ROOT / "web" / "playwright-report"
    results_dir = ROOT / "web" / "test-results"

    started_at = datetime.now(timezone.utc).isoformat()
    results: list[dict[str, object]] = []
    for journey in selected:
        journey_env = dict(environment)
        journey_env.update(journey_environment(journey, args.lab))
        stdout_path = run_dir / f"{journey.key}.stdout.log"
        stderr_path = run_dir / f"{journey.key}.stderr.log"
        returncode = execute(playwright_command(journey), journey_env, stdout_path, stderr_path)
        passed = returncode == 0
        results.append(
            {
                "key": journey.key,
                "project": journey.project,
                "spec": journey.spec,
                "grep": journey.grep,
                "status": "passed" if passed else "failed",
                "diagnostic": None if passed else JOURNEY_FAILED,
                "exit_code": returncode,
            }
        )
        collect_artifacts(report_dir, results_dir, run_dir / journey.key)
        print(f"{'PASS' if passed else 'FAIL'} {journey.key} ({journey.project})")
    finished_at = datetime.now(timezone.utc).isoformat()

    summary = build_summary(
        run_id=run_id,
        base_url=args.base_url,
        git_commit=git_commit,
        package_manifest=args.package_manifest,
        helm_revision=deployment_identity,
        bundle_sha256=bundle_sha256,
        journeys=results,
        started_at=started_at,
        finished_at=finished_at,
    )
    (run_dir / "summary.json").write_text(
        json.dumps(summary, indent=2) + "\n", encoding="utf-8"
    )

    exit_code = journey_exit_code(results)
    diagnostics = [] if exit_code == 0 else [
        f"{item['key']}:{JOURNEY_FAILED}" for item in results if item["status"] != "passed"
    ]
    return RunResult(exit_code=exit_code, diagnostics=diagnostics, summary=summary)


# --- CLI ------------------------------------------------------------------


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Repeatable acceptance entry point for the public LabWeaver portal."
    )
    subparsers = parser.add_subparsers(dest="command", required=True)

    preflight = subparsers.add_parser(
        "preflight", help="assert every acceptance precondition"
    )
    preflight.add_argument("--base-url", required=True)
    preflight.add_argument("--credentials-dir", default=str(DEFAULT_CREDENTIALS_DIR))
    preflight.add_argument("--model", default=None)
    preflight.add_argument("--evidence-dir", default=DEFAULT_EVIDENCE_DIR)

    run = subparsers.add_parser("run", help="run the selected browser journeys")
    run.add_argument("--base-url", required=True)
    run.add_argument("--run-id", required=True)
    run.add_argument("--journeys", default=DEFAULT_JOURNEYS)
    run.add_argument("--lab", default=DEFAULT_LAB)
    run.add_argument("--evidence-dir", default=DEFAULT_EVIDENCE_DIR)
    run.add_argument("--model", default=None)
    run.add_argument("--credentials-dir", default=str(DEFAULT_CREDENTIALS_DIR))
    run.add_argument("--package-manifest", default=None)
    run.add_argument("--bundle-sha256", default=None)

    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    if args.command == "preflight":
        result = run_preflight(args)
        for check in result.checks:
            print(format_check(check))
        for code in result.diagnostics:
            print(code, file=sys.stderr)
        return result.exit_code

    result = run_acceptance(args)
    for code in result.diagnostics:
        print(code, file=sys.stderr)
    return result.exit_code


if __name__ == "__main__":
    raise SystemExit(main())
