#!/usr/bin/env python3
"""Repeatable acceptance entry point for the public LabWeaver portal (Issue #127).

The tool has two subcommands:

* ``preflight`` asserts every precondition the browser journeys need (cluster
  reachable, public portal serving, Keycloak redirect, private credentials,
  provider model and Playwright browser) and fails closed with a stable
  ``LW_ACCEPTANCE_*`` diagnostic code.
* ``run`` materialises the private credentials into a temporary directory,
  assembles the Playwright environment and drives each selected journey
  through the real browser harness. Credentials, browser state and test output
  are removed when the run exits.

Every check fails with a stable diagnostic code so a failed run can be
classified without reading the logs.
"""

from __future__ import annotations

import argparse
import json
import os
import stat
import ssl
import subprocess
import tempfile
import time
import sys
import urllib.error
import urllib.request
import uuid
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Callable, Mapping, Sequence
from urllib.parse import urlparse

ROOT = Path(__file__).resolve().parents[1]

KUBECTL_CONTEXT = "kubernetes-admin@kubernetes"
NAMESPACE = "labweaver-system"
# Business facts live in the data namespace; the authoring queue is read there.
DATA_NAMESPACE = "labweaver-data"
# Helm release that owns the platform profile workloads; the Resource profile
# renders the same chart under its own release and bundle identity.
PLATFORM_RELEASE = "labweaver"
BUNDLE_ANNOTATION = "labweaver.io/configuration-bundle-sha256"
MODEL_ENV = "LABWEAVER_E2E_PROVIDER_MODEL"
ANTHROPIC_MODEL_ENV = "ANTHROPIC_MODEL"
DEFAULT_CREDENTIALS_DIR = Path(".private/labweaver-acceptance/credentials")
DEFAULT_JOURNEYS = "lab,work,admin"
DEFAULT_LAB = "xv6"

# The container provider binding is cluster-specific too: the shipped example
# packages target the local development stack, so the acceptance run resolves
# the binding the live environment service actually registers.
PROVIDER_BINDING_ENV = "LABWEAVER_E2E_PROVIDER_BINDING"
PROVIDER_CONFIG_MAP = "environment-service-config"
PROVIDER_CONFIG_MAP_KEY = "providers.json"

# Stable diagnostics, prefixed so a caller can classify a failure mechanically.
CLUSTER_UNREACHABLE = "LW_ACCEPTANCE_CLUSTER_UNREACHABLE"
PORTAL_UNREACHABLE = "LW_ACCEPTANCE_PORTAL_UNREACHABLE"
LOGIN_REDIRECT_INVALID = "LW_ACCEPTANCE_LOGIN_REDIRECT_INVALID"
CREDENTIALS_MISSING = "LW_ACCEPTANCE_CREDENTIALS_MISSING"
MODEL_MISSING = "LW_ACCEPTANCE_MODEL_MISSING"
AUTHORING_QUEUE_BUSY = "LW_ACCEPTANCE_AUTHORING_QUEUE_BUSY"
# The worker serves one reserved dispatch at a time, so a run waits for the
# queue in front of it; see docs/deployment/runbook.md.
QUEUE_WAIT_SECONDS = 3600.0
QUEUE_WAIT_POLL_SECONDS = 30.0
BROWSER_MISSING = "LW_ACCEPTANCE_BROWSER_MISSING"
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
    auth_dir: Path,
    output_dir: Path,
    model: str,
    environ: Mapping[str, str],
    provider_binding: str = "",
) -> dict[str, str]:
    """The full Playwright environment for a journey invocation."""

    environment = dict(environ)
    environment["LABWEAVER_BASE_URL"] = base_url
    environment["LABWEAVER_IGNORE_HTTPS_ERRORS"] = "1"
    environment[ANTHROPIC_MODEL_ENV] = model
    environment[MODEL_ENV] = model
    environment["LABWEAVER_AUTH_DIR"] = str(auth_dir)
    environment["LABWEAVER_PLAYWRIGHT_OUTPUT_DIR"] = str(output_dir)
    if provider_binding:
        environment[PROVIDER_BINDING_ENV] = provider_binding
    # The provider budget ceilings live with the harness that builds the project
    # policy (`web/e2e/support/live.mjs`) so there is exactly one definition.
    # Callers override them through LABWEAVER_E2E_LLM_MAX_* in the ambient
    # environment, which is inherited here unchanged.
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
    provider_binding: str = "",
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
        "provider_binding": provider_binding or None,
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


def load_dotenv(path: Path) -> dict[str, str]:
    """Read the small local environment file without invoking a shell."""

    values: dict[str, str] = {}
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except OSError:
        return values
    for line in lines:
        text = line.strip()
        if not text or text.startswith("#"):
            continue
        if text.startswith("export "):
            text = text[7:].lstrip()
        key, separator, value = text.partition("=")
        key = key.strip()
        if not separator or not key or any(character.isspace() for character in key):
            continue
        value = value.strip()
        if len(value) >= 2 and value[0] == value[-1] and value[0] in {"'", '"'}:
            value = value[1:-1]
        values[key] = value
    return values


def load_acceptance_environment(environ: Mapping[str, str] | None) -> dict[str, str]:
    """Return process variables, loading the root `.env` only for real CLI runs."""

    if environ is not None:
        return dict(environ)
    values = load_dotenv(ROOT / ".env")
    values.update(os.environ)
    return values


def resolve_model(
    explicit: str | None,
    environ: Mapping[str, str],
    _run_kubectl: Callable[[Sequence[str]], tuple[int, str, str]] | None = None,
) -> str:
    """Resolve the provider model from the root `ANTHROPIC_MODEL` target only."""

    return (explicit or environ.get(ANTHROPIC_MODEL_ENV) or "").strip()


def resolve_provider_binding(
    explicit: str | None,
    environ: Mapping[str, str],
    run_kubectl: Callable[[Sequence[str]], tuple[int, str, str]],
) -> str:
    """Resolve the live container provider binding from the flag, env or cluster."""

    value = (explicit or environ.get(PROVIDER_BINDING_ENV) or "").strip()
    if value:
        return value
    escaped_key = PROVIDER_CONFIG_MAP_KEY.replace(".", "\\.")
    code, stdout, _ = run_kubectl(
        [
            "kubectl",
            "--context",
            KUBECTL_CONTEXT,
            "-n",
            NAMESPACE,
            "get",
            "cm",
            PROVIDER_CONFIG_MAP,
            "-o",
            "jsonpath={.data." + escaped_key + "}",
        ]
    )
    if code != 0 or not stdout.strip():
        return ""
    try:
        providers = json.loads(stdout)
    except ValueError:
        return ""
    for provider in providers:
        if isinstance(provider, dict) and provider.get("providerKind") == "container":
            binding = str(provider.get("binding", "")).strip()
            if binding:
                return binding
    return ""


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
    """The unique ``configuration-bundle-sha256`` annotation across workloads.

    Only the platform release is read: the Resource release renders the same
    chart with its own bundle, so mixing both would never yield one identity.
    """

    code, stdout, _ = run_kubectl(
        [
            "kubectl",
            "--context",
            KUBECTL_CONTEXT,
            "-n",
            NAMESPACE,
            "get",
            "deploy",
            "-l",
            f"app.kubernetes.io/instance={PLATFORM_RELEASE}",
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


# The specs own their chain budget (`FULL_CHAIN_TIMEOUT_MS` is four hours in the
# student and admin specs, and the lab spec allows thirty minutes per attempt
# plus retries), so this ceiling only exists to keep a hung browser from
# stalling the whole suite. It must not be tighter than the spec it wraps:
# a 45-minute cap killed a legitimately running admin journey
# (`LW_ACCEPTANCE_JOURNEY_TIMEOUT`) while the spec still had budget left.
JOURNEY_TIMEOUT_SECONDS = 15_000.0
JOURNEY_TIMEOUT = "LW_ACCEPTANCE_JOURNEY_TIMEOUT"
JOURNEY_TIMEOUT_EXIT_CODE = 124

RESOURCE_CANCEL_REASON = "acceptance harness cleanup of this run's resource request"


def _auth_cookie(role_file: Path) -> str | None:
    """Session cookie header from a Playwright storage state, if it exists."""

    try:
        state = json.loads(role_file.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None
    cookies = state.get("cookies") or []
    if not cookies:
        return None
    return "; ".join(f"{cookie['name']}={cookie['value']}" for cookie in cookies)


def _http(
    url: str,
    cookie: str,
    *,
    origin: str,
    method: str = "GET",
    body: Mapping[str, object] | None = None,
    token: str | None = None,
    etag: str | None = None,
) -> tuple[int, bytes, Mapping[str, str]]:
    headers = {
        "Cookie": cookie,
        "Accept": "application/json",
        "Origin": origin,
        "Content-Type": "application/json",
    }
    if token:
        headers["X-CSRF-Token"] = token
    if etag:
        headers["If-Match"] = etag
    if method == "POST":
        headers["Idempotency-Key"] = str(uuid.uuid4())
    request = urllib.request.Request(
        url,
        method=method,
        data=json.dumps(body).encode() if body is not None else None,
        headers=headers,
    )
    context = ssl.create_default_context()
    # Resource request details can contain long provider metadata. Keep the
    # response bounded without truncating a normal request payload.
    max_body_bytes = 8 * 1024 * 1024
    try:
        with urllib.request.urlopen(request, context=context, timeout=25.0) as response:
            return response.status, response.read(max_body_bytes), dict(response.headers)
    except urllib.error.HTTPError as error:
        return error.code, error.read(max_body_bytes), dict(error.headers)


def cancel_resource_request(
    *,
    base_url: str,
    auth_state: Path,
    project_id: str,
    request_id: str,
    request_key: str,
) -> dict[str, object] | None:
    """Cancel one request after checking its exact project and request key.

    This command is intentionally incapable of discovering or selecting stale
    requests. The caller must pass the identifiers returned by the current
    journey; a mismatched project or request key is rejected before mutation.
    """

    if not project_id.strip() or not request_id.strip() or not request_key.strip():
        raise AcceptanceError(JOURNEY_FAILED, "project, request and request key are required")
    cookie = _auth_cookie(auth_state)
    if not cookie:
        return None
    status, body, headers = _http(
        _join(base_url, f"/api/v1/resource-requests/{request_id}"),
        cookie,
        origin=base_url,
    )
    if status != 200:
        return None
    try:
        detail = json.loads(body)
    except ValueError:
        return None
    if (
        detail.get("projectId") != project_id
        or detail.get("requestKey") != request_key
        or detail.get("state") not in {"requested", "reviewing"}
    ):
        return None
    _, csrf_body, _ = _http(_join(base_url, "/api/v1/auth/csrf"), cookie, origin=base_url)
    try:
        csrf = json.loads(csrf_body)
    except ValueError:
        return None
    token = csrf.get("csrfToken") or csrf.get("token")
    etag = headers.get("etag") or headers.get("ETag")
    if not token or not etag:
        return None
    cancel_status, cancel_body, _ = _http(
        _join(base_url, f"/api/v1/resource-requests/{request_id}/cancel"),
        cookie,
        origin=base_url,
        method="POST",
        body={"reason": RESOURCE_CANCEL_REASON},
        token=token,
        etag=etag,
    )
    try:
        result = json.loads(cancel_body)
    except ValueError:
        result = {"status": cancel_status}
    return result if cancel_status in {200, 201, 202} else None


def queued_dispatch_count(
    run_kubectl: Callable[[Sequence[str]], tuple[int, str, str]],
) -> int | None:
    """Number of reserved dispatches the agent worker has not finished yet."""

    query = (
        "select count(*) from agent.agent_run_dispatches "
        "where state in ('pending','preparing','claimed')"
    )
    code, stdout, _ = run_kubectl(
        [
            "kubectl",
            "--context",
            KUBECTL_CONTEXT,
            "-n",
            DATA_NAMESPACE,
            "exec",
            "postgres-0",
            "--",
            "psql",
            "-U",
            "postgres",
            "-d",
            "labweaver",
            "-tAc",
            query,
        ]
    )
    if code != 0 or not stdout.strip().isdigit():
        return None
    return int(stdout.strip())


def wait_for_authoring_queue(
    run_kubectl: Callable[[Sequence[str]], tuple[int, str, str]],
    timeout: float,
) -> int | None:
    """Wait until no queued dispatch is left, or the timeout elapses.

    The agent worker runs one reserved dispatch at a time in ``created_at``
    order, so a journey started while dispatches are queued waits behind them
    and can exceed its own poll ceiling. Returns the last observed count.
    """

    deadline = time.monotonic() + timeout
    reported: int | None = None
    while True:
        pending = queued_dispatch_count(run_kubectl)
        if pending is None or pending == 0 or time.monotonic() >= deadline:
            return pending
        if pending != reported:
            print(f"waiting for {pending} queued authoring dispatch(es) to finish")
            reported = pending
        time.sleep(QUEUE_WAIT_POLL_SECONDS)


def check_authoring_queue(
    run_kubectl: Callable[[Sequence[str]], tuple[int, str, str]],
) -> Check:
    """Report queued authoring dispatches."""

    pending = queued_dispatch_count(run_kubectl)
    if pending is None:
        return Check("authoring_queue", True, None, "queue state unavailable")
    return Check(
        "authoring_queue",
        True,
        AUTHORING_QUEUE_BUSY if pending else None,
        f"{pending} queued authoring dispatch(es)",
    )


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
        # Windows does not expose POSIX read bits through ``stat``; the file
        # is still private because it is created in the caller's private
        # directory and is removed with the temporary run directory.
        if hasattr(os, "getuid") and metadata.st_mode & stat.S_IROTH:
            problems.append(f"{name}: world-readable")
    ok = not problems
    detail = str(directory) if ok else "; ".join(problems)
    return Check("credentials", ok, None if ok else CREDENTIALS_MISSING, detail)


def check_model(
    model: str,
) -> Check:
    ok = bool(model.strip())
    return Check(
        "model",
        ok,
        None if ok else MODEL_MISSING,
        model or f"{ANTHROPIC_MODEL_ENV} unset",
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
        check_model(model),
        check_browser(repo_root, browsers_path, run_version),
        check_authoring_queue(run_kubectl),
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
        environ=load_acceptance_environment(environ),
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


def materialise_credentials(source_dir: Path, target_dir: Path) -> None:
    """Copy the private credentials into ``target_dir`` with 0700/0600 modes."""

    try:
        target_dir.mkdir(parents=True, exist_ok=True)
        os.chmod(target_dir, 0o700)
    except OSError as error:
        raise AcceptanceError(CREDENTIALS_MISSING, f"{target_dir}: {error}") from error
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
                CREDENTIALS_MISSING, f"{destination}: {error}"
            ) from error


def execute_journey(
    command: Sequence[str],
    environment: Mapping[str, str],
    stdout_path: Path,
    stderr_path: Path,
    timeout: float = JOURNEY_TIMEOUT_SECONDS,
) -> int:
    """Run one journey, bounded so a hung browser cannot stall the whole suite."""

    with stdout_path.open("w", encoding="utf-8") as stdout, stderr_path.open(
        "w", encoding="utf-8"
    ) as stderr:
        try:
            completed = subprocess.run(
                list(command),
                cwd=ROOT,
                env=dict(environment),
                stdout=stdout,
                stderr=stderr,
                timeout=timeout,
            )
        except subprocess.TimeoutExpired:
            print(
                f"journey exceeded {timeout:.0f}s and was stopped",
                file=sys.stderr,
            )
            return JOURNEY_TIMEOUT_EXIT_CODE
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
    environment = load_acceptance_environment(environ)
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

    provider_binding = resolve_provider_binding(
        getattr(args, "provider_binding", None), environment, run_kubectl
    )
    try:
        with tempfile.TemporaryDirectory(prefix="labweaver-acceptance-") as temporary_root:
            run_dir = Path(temporary_root)
            credentials_dir = run_dir / ".credentials"
            auth_dir = run_dir / ".auth"
            output_dir = run_dir / "playwright-output"
            materialise_credentials(Path(args.credentials_dir), credentials_dir)
            auth_dir.mkdir(mode=0o700)
            output_dir.mkdir(mode=0o700)

            journey_environment_base = acceptance_environment(
                base_url=args.base_url,
                credentials_dir=credentials_dir,
                auth_dir=auth_dir,
                output_dir=output_dir,
                model=model,
                environ=environment,
                provider_binding=provider_binding,
            )

            if git_commit is None:
                git_commit = probe_git_commit()
            if deployment_identity is None:
                deployment_identity = probe_deployment_identity(run_kubectl)
            bundle_sha256 = args.bundle_sha256 or deployment_identity

            if not getattr(args, "no_queue_wait", False):
                pending = wait_for_authoring_queue(run_kubectl, QUEUE_WAIT_SECONDS)
                if pending:
                    print(f"authoring queue still busy: {pending} dispatch(es) after waiting")
            started_at = datetime.now(timezone.utc).isoformat()
            print(f"provider_binding={provider_binding or '<unset>'} model={model}")
            results: list[dict[str, object]] = []
            for journey in selected:
                journey_env = dict(journey_environment_base)
                journey_env.update(journey_environment(journey, args.lab))
                stdout_path = run_dir / f"{journey.key}.stdout.log"
                stderr_path = run_dir / f"{journey.key}.stderr.log"
                returncode = execute(
                    playwright_command(journey), journey_env, stdout_path, stderr_path
                )
                passed = returncode == 0
                results.append(
                    {
                        "key": journey.key,
                        "project": journey.project,
                        "spec": journey.spec,
                        "grep": journey.grep,
                        "status": "passed" if passed else "failed",
                        "diagnostic": (
                            None
                            if passed
                            else JOURNEY_TIMEOUT
                            if returncode == JOURNEY_TIMEOUT_EXIT_CODE
                            else JOURNEY_FAILED
                        ),
                        "exit_code": returncode,
                    }
                )
                print(f"{'PASS' if passed else 'FAIL'} {journey.key} ({journey.project})")
            finished_at = datetime.now(timezone.utc).isoformat()

            summary = build_summary(
                run_id=run_id,
                base_url=args.base_url,
                git_commit=git_commit,
                package_manifest=args.package_manifest,
                helm_revision=deployment_identity,
                bundle_sha256=bundle_sha256,
                provider_binding=provider_binding,
                journeys=results,
                started_at=started_at,
                finished_at=finished_at,
            )

            exit_code = journey_exit_code(results)
            diagnostics = [] if exit_code == 0 else [
                f"{item['key']}:{item.get('diagnostic') or JOURNEY_FAILED}"
                for item in results
                if item["status"] != "passed"
            ]
            return RunResult(exit_code=exit_code, diagnostics=diagnostics, summary=summary)
    except AcceptanceError as error:
        return RunResult(exit_code=2, diagnostics=[error.code])


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

    cancel = subparsers.add_parser(
        "cancel-resource",
        help="cancel one resource request identified by this run",
    )
    cancel.add_argument("--base-url", required=True)
    cancel.add_argument("--auth-state", required=True)
    cancel.add_argument("--project-id", required=True)
    cancel.add_argument("--request-id", required=True)
    cancel.add_argument("--request-key", required=True)

    run = subparsers.add_parser("run", help="run the selected browser journeys")
    run.add_argument("--base-url", required=True)
    run.add_argument("--run-id", required=True)
    run.add_argument("--journeys", default=DEFAULT_JOURNEYS)
    run.add_argument("--lab", default=DEFAULT_LAB)
    run.add_argument("--model", default=None)
    run.add_argument(
        "--no-queue-wait",
        action="store_true",
        help="start without waiting for queued authoring dispatches to drain",
    )
    run.add_argument(
        "--provider-binding",
        default=None,
        help="container provider binding; defaults to the one the live environment service registers",
    )
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

    if args.command == "cancel-resource":
        result = cancel_resource_request(
            base_url=args.base_url,
            auth_state=Path(args.auth_state),
            project_id=args.project_id,
            request_id=args.request_id,
            request_key=args.request_key,
        )
        if result is None:
            print("resource request was not cancelled", file=sys.stderr)
            return 1
        print(json.dumps(result, sort_keys=True))
        return 0

    result = run_acceptance(args)
    for code in result.diagnostics:
        print(code, file=sys.stderr)
    return result.exit_code


if __name__ == "__main__":
    raise SystemExit(main())
