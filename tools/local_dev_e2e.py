#!/usr/bin/env python3
"""Build and run the real Playwright suite inside the owned local Kind stack.

The ``build`` command builds the pinned official Playwright image and pushes
it to the registry recorded in the local-dev state.  The ``run`` command
requires that same state to be ready, creates a short-lived run-owned Job and
its narrowly scoped network policy, and returns the Playwright container exit
code.  No default kubeconfig or cluster is ever used.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable


ROOT = Path(__file__).resolve().parents[1]
STATE_DIR = ROOT / ".tmp" / "local-dev"
PRIVATE_DIR = ROOT / ".private" / "local-dev"
DEFAULT_STATE_FILE = Path(".tmp") / "local-dev" / "state.json"
CONTAINERFILE = Path("tools") / "local-dev" / "Containerfile.playwright"

KUBERNETES_OWNER = "tools.local_dev_e2e.py"
LOCAL_DEV_OWNER = "tools.local_dev.py"
DOCKER_REGISTRY_OWNER = "tools/local_dev.py"
PLAYWRIGHT_BASE_TAG = "mcr.microsoft.com/playwright:v1.61.1-noble"
PLAYWRIGHT_BASE_IMAGE = (
    f"{PLAYWRIGHT_BASE_TAG}@"
    "sha256:cf0daee9b994042e011bc29f20cdff1a9f682a039b43fcd738f7d8a9d3bcd9d6"
)
DEFAULT_IMAGE_REPOSITORY = "labweaver/local/playwright-e2e"
DEFAULT_PROJECTS = ("setup", "teacher", "student", "platform-admin")
AUTH_ACTORS = (
    ("teacher", "teacher", "LABWEAVER_TEACHER_USERNAME", "LABWEAVER_TEACHER_PASSWORD_FILE"),
    ("student", "student", "LABWEAVER_STUDENT_USERNAME", "LABWEAVER_STUDENT_PASSWORD_FILE"),
    (
        "platform-admin",
        "platform-admin",
        "LABWEAVER_PLATFORM_ADMIN_USERNAME",
        "LABWEAVER_PLATFORM_ADMIN_PASSWORD_FILE",
    ),
)
CA_FILE_IN_JOB = "/run/secrets/labweaver-ca/ca.crt"
CREDENTIALS_DIR_IN_JOB = "/run/secrets/labweaver-credentials"
LABEL_RUN_ID = "labweaver.local-dev.run-id"
LABEL_OWNER = "labweaver.local-dev.owner"
IMAGE_REF_PATTERN = re.compile(
    r"^localhost:(?P<port>[0-9]{1,5})/(?P<repository>[a-z0-9](?:[a-z0-9._/-]*[a-z0-9])?)"
    r"(?::(?P<tag>[A-Za-z0-9_][A-Za-z0-9_.-]*))?"
    r"(?:@(?P<digest>sha256:[0-9a-f]{64}))?$"
)
RUN_ID_PATTERN = re.compile(r"^[0-9a-f]{12}$")
ENV_NAME_PATTERN = re.compile(r"^[A-Z][A-Z0-9_]*$")
SENSITIVE_ENV_PATTERN = re.compile(
    r"(?:PASSWORD|SECRET|TOKEN|PRIVATE_KEY|CLIENT_KEY|AUTHORIZATION|COOKIE)",
    re.IGNORECASE,
)
CONTROLLED_ENV_NAMES = frozenset(
    {
        "CI",
        "HOME",
        "XDG_CONFIG_HOME",
        "NODE_EXTRA_CA_CERTS",
        "SSL_CERT_FILE",
        "LABWEAVER_BASE_URL",
        "LABWEAVER_CA_FILE",
        *(item[2] for item in AUTH_ACTORS),
        *(item[3] for item in AUTH_ACTORS),
    }
)

DETAIL_LIMIT = 4000
DETAIL_TRUNCATION_MARKER = "\n[... output truncated; showing beginning and end ...]\n"
COMMAND_ENCODING = "utf-8"


class HarnessError(RuntimeError):
    """A user-actionable harness failure."""


def fail(message: str) -> None:
    raise HarnessError(message)


def _bounded_detail(value: str, limit: int = DETAIL_LIMIT) -> str:
    text = value.strip()
    text = re.sub(
        r"(?i)([\"']?(?:password|secret|token|authorization|private[_-]?key)[\"']?\s*[:=]\s*)"
        r"(?:\"[^\"]*\"|'[^']*'|[^\s,;}]+)",
        r"\1<redacted>",
        text,
    )
    if len(text) > limit:
        if limit <= 0:
            return ""
        if limit <= len(DETAIL_TRUNCATION_MARKER):
            return DETAIL_TRUNCATION_MARKER[:limit]
        available = limit - len(DETAIL_TRUNCATION_MARKER)
        head_limit = (available + 1) // 2
        tail_limit = available - head_limit
        return (
            text[:head_limit]
            + DETAIL_TRUNCATION_MARKER
            + (text[-tail_limit:] if tail_limit else "")
        )
    return text


def _redact_values(text: str, values: Iterable[str]) -> str:
    for value in values:
        if value:
            text = text.replace(value, "<redacted>")
    return _bounded_detail(text)


def run_command(
    argv: list[str],
    *,
    input_text: str | None = None,
    capture: bool = False,
    check: bool = True,
    timeout: float | None = None,
) -> subprocess.CompletedProcess[str]:
    try:
        encoded_input = input_text.encode(COMMAND_ENCODING) if input_text is not None else None
        result = subprocess.run(
            argv,
            cwd=ROOT,
            input=encoded_input,
            text=False,
            capture_output=capture,
            check=False,
            timeout=timeout,
        )
    except FileNotFoundError:
        fail("required executable is unavailable: " + argv[0])
    except subprocess.TimeoutExpired:
        fail("command timed out: " + argv[0])
    except UnicodeError as error:
        fail(f"command output is not valid UTF-8: {argv[0]}: {error}")
    stdout = result.stdout
    stderr = result.stderr
    try:
        if isinstance(stdout, bytes):
            stdout = stdout.decode(COMMAND_ENCODING, errors="strict")
        if isinstance(stderr, bytes):
            stderr = stderr.decode(COMMAND_ENCODING, errors="strict")
    except UnicodeError as error:
        fail(f"command output is not valid UTF-8: {argv[0]}: {error}")
    result = subprocess.CompletedProcess(result.args, result.returncode, stdout, stderr)
    if check and result.returncode:
        output = (result.stderr or "") + "\n" + (result.stdout or "")
        detail = _bounded_detail(output)
        suffix = f"\n{detail}" if detail else ""
        fail(f"command failed ({result.returncode}): {' '.join(argv)}{suffix}")
    return result


def require_tools(names: Iterable[str]) -> None:
    missing = [name for name in names if shutil.which(name) is None]
    if missing:
        fail("missing local tools: " + ", ".join(missing))


def resolve_workspace_path(value: str | Path, *, base: Path, label: str) -> Path:
    raw = Path(value)
    candidate = raw.resolve() if raw.is_absolute() else (ROOT / raw).resolve()
    try:
        candidate.relative_to(base.resolve())
    except ValueError:
        fail(f"{label} is outside its owned workspace directory")
    return candidate


def _read_json(path: Path, label: str) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        fail(f"cannot read {label}: {error}")
    if not isinstance(value, dict):
        fail(f"{label} must contain a JSON object")
    return value


def _state_relative_path(state: dict[str, Any], key: str, base: Path) -> Path:
    value = state.get(key)
    if not isinstance(value, str) or not value or Path(value).is_absolute():
        fail(f"local development state has an invalid {key} path")
    return resolve_workspace_path(value, base=base, label=f"state {key} path")


@dataclass(frozen=True)
class Stack:
    state_file: Path
    state: dict[str, Any]
    kubeconfig: Path
    run_root: Path
    run_id: str
    namespace: str
    identity_namespace: str
    registry: str
    registry_port: int
    portal_port: int | None


def load_stack(state_file_value: str | Path, *, require_ready: bool) -> Stack:
    state_file = resolve_workspace_path(state_file_value, base=STATE_DIR, label="state file")
    if not state_file.is_file():
        fail("local development state file does not exist")
    state = _read_json(state_file, "local development state")
    phase = state.get("phase")
    if require_ready and phase != "ready":
        fail(f"local development stack is not ready (phase={phase!r})")
    if not isinstance(phase, str) or phase not in {
        "starting",
        "foundation",
        "building",
        "deploying",
        "ready",
    }:
        fail("local development state has an invalid phase")

    run_id = state.get("runId")
    if not isinstance(run_id, str) or not RUN_ID_PATTERN.fullmatch(run_id):
        fail("local development state has an invalid runId")
    expected_registry = f"labweaver-local-registry-{run_id}"
    expected_cluster = f"labweaver-local-{run_id}"
    if state.get("registry") != expected_registry or state.get("cluster") != expected_cluster:
        fail("local development state names are not owned by tools/local_dev.py")

    registry_port = state.get("registryPort")
    if isinstance(registry_port, bool) or not isinstance(registry_port, int) or not 1 <= registry_port <= 65535:
        fail("local development state has an invalid registryPort")
    namespace = state.get("namespace")
    identity_namespace = state.get("identityNamespace")
    if not isinstance(namespace, str) or not namespace or not isinstance(identity_namespace, str) or not identity_namespace:
        fail("local development state has invalid namespaces")

    kubeconfig = _state_relative_path(state, "kubeconfig", STATE_DIR)
    run_root = _state_relative_path(state, "runRoot", PRIVATE_DIR)
    if not run_root.is_dir():
        fail("local development run root does not exist")
    if require_ready and not kubeconfig.is_file():
        fail("local development kubeconfig does not exist")

    portal_port: int | None = None
    if require_ready:
        value = state.get("portalPort")
        if isinstance(value, bool) or not isinstance(value, int) or not 1 <= value <= 65535:
            fail("local development state has an invalid portalPort")
        portal_port = value
    return Stack(
        state_file=state_file,
        state=state,
        kubeconfig=kubeconfig,
        run_root=run_root,
        run_id=run_id,
        namespace=namespace,
        identity_namespace=identity_namespace,
        registry=expected_registry,
        registry_port=registry_port,
        portal_port=portal_port,
    )


def assert_same_run(before: Stack, after: Stack) -> None:
    if before.run_id != after.run_id or before.registry != after.registry:
        fail("local development state changed to a different run")


def kubectl(stack: Stack, args: list[str], *, capture: bool = True, check: bool = True) -> subprocess.CompletedProcess[str]:
    return run_command(
        ["kubectl", "--kubeconfig", str(stack.kubeconfig), *args],
        capture=capture,
        check=check,
    )


def kubectl_json(
    stack: Stack,
    args: list[str],
    *,
    allow_not_found: bool = False,
) -> dict[str, Any] | None:
    result = kubectl(stack, [*args, "-o", "json"], check=False)
    if result.returncode:
        detail = ((result.stderr or "") + "\n" + (result.stdout or "")).strip()
        if allow_not_found and re.search(r"not found|notfound", detail, re.IGNORECASE):
            return None
        fail("kubectl query failed: " + _bounded_detail(detail))
    try:
        value = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        fail(f"kubectl returned invalid JSON: {error}")
    if not isinstance(value, dict):
        fail("kubectl returned a non-object JSON value")
    return value


def get_namespaced_object(stack: Stack, kind: str, name: str, namespace: str) -> dict[str, Any] | None:
    return kubectl_json(stack, ["-n", namespace, "get", f"{kind}/{name}"], allow_not_found=True)


def apply_object(stack: Stack, obj: dict[str, Any]) -> None:
    payload = json.dumps(obj, ensure_ascii=False, separators=(",", ":")) + "\n"
    result = run_command(
        ["kubectl", "--kubeconfig", str(stack.kubeconfig), "apply", "-f", "-"],
        input_text=payload,
        capture=True,
        check=False,
    )
    if result.returncode:
        detail = ((result.stderr or "") + "\n" + (result.stdout or "")).strip()
        fail("kubectl apply failed: " + _bounded_detail(detail))


def owned_labels(run_id: str, *, include_app: bool = False) -> dict[str, str]:
    labels = {LABEL_RUN_ID: run_id, LABEL_OWNER: KUBERNETES_OWNER}
    if include_app:
        labels["app.kubernetes.io/name"] = "local-dev-playwright-e2e"
        labels["app.kubernetes.io/part-of"] = "labweaver"
    return labels


def verify_labels(obj: dict[str, Any], stack: Stack, *, include_app: bool = False) -> None:
    metadata = obj.get("metadata")
    labels = metadata.get("labels") if isinstance(metadata, dict) else None
    expected = owned_labels(stack.run_id, include_app=include_app)
    if not isinstance(labels, dict) or any(labels.get(key) != value for key, value in expected.items()):
        fail("refusing to use a Kubernetes object without the current harness ownership labels")


def delete_owned_object(stack: Stack, kind: str, name: str, namespace: str, *, include_app: bool = False) -> None:
    obj = get_namespaced_object(stack, kind, name, namespace)
    if obj is None:
        return
    verify_labels(obj, stack, include_app=include_app)
    result = kubectl(
        stack,
        [
            "-n",
            namespace,
            "delete",
            f"{kind}/{name}",
            "--ignore-not-found",
            "--wait=true",
            "--timeout=30s",
            "--cascade=foreground",
        ],
        check=False,
    )
    if result.returncode:
        detail = ((result.stderr or "") + "\n" + (result.stdout or "")).strip()
        fail(f"owned Kubernetes cleanup failed for {kind}/{name}: {_bounded_detail(detail)}")


def prepare_name(stack: Stack, kind: str, name: str, namespace: str, *, include_app: bool = False) -> None:
    obj = get_namespaced_object(stack, kind, name, namespace)
    if obj is None:
        return
    verify_labels(obj, stack, include_app=include_app)
    if kind == "job":
        status = obj.get("status")
        if isinstance(status, dict) and status.get("active", 0):
            fail(f"owned Playwright Job is already active: {name}")
    delete_owned_object(stack, kind, name, namespace, include_app=include_app)


def verify_registry_owner(stack: Stack) -> None:
    result = run_command(
        ["docker", "inspect", "--format", "{{json .Config.Labels}}", stack.registry],
        capture=True,
        check=False,
    )
    if result.returncode:
        fail("cannot inspect the recorded local registry")
    try:
        labels = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        fail(f"local registry returned invalid ownership metadata: {error}")
    if not isinstance(labels, dict) or labels.get(LABEL_RUN_ID) != stack.run_id or labels.get(LABEL_OWNER) != DOCKER_REGISTRY_OWNER:
        fail("refusing to use a registry without the current local-dev ownership labels")


def verify_ready_portal_owner(stack: Stack) -> None:
    obj = get_namespaced_object(stack, "deployment", "local-dev-portal", stack.namespace)
    if obj is None:
        fail("local-dev portal deployment does not exist")
    metadata = obj.get("metadata")
    labels = metadata.get("labels") if isinstance(metadata, dict) else None
    if (
        not isinstance(labels, dict)
        or labels.get(LABEL_RUN_ID) != stack.run_id
        or labels.get(LABEL_OWNER) != LOCAL_DEV_OWNER
    ):
        fail("refusing to use a portal deployment without the current local-dev ownership labels")


def verify_playwright_base() -> None:
    """Confirm the pinned official image is still present for linux/amd64."""

    result = run_command(
        ["docker", "buildx", "imagetools", "inspect", PLAYWRIGHT_BASE_TAG],
        capture=True,
        check=False,
    )
    if result.returncode:
        fail("cannot verify the official Playwright base image")
    expected_digest = PLAYWRIGHT_BASE_IMAGE.rsplit("@", 1)[1]
    output = result.stdout or ""
    if expected_digest not in output or "linux/amd64" not in output:
        fail("the verified Playwright tag no longer contains the pinned linux/amd64 digest")


def git_build_metadata() -> tuple[str, str]:
    commit = run_command(["git", "rev-parse", "--short=12", "HEAD"], capture=True).stdout.strip()
    epoch = run_command(["git", "show", "-s", "--format=%ct", "HEAD"], capture=True).stdout.strip()
    if not re.fullmatch(r"[0-9a-f]{7,40}", commit) or not re.fullmatch(r"[0-9]+", epoch):
        fail("git did not return valid image metadata")
    return commit, epoch


def _validate_image_ref(value: str, stack: Stack, *, require_digest: bool = False) -> re.Match[str]:
    if not value or any(character.isspace() for character in value):
        fail("image reference is empty or contains whitespace")
    match = IMAGE_REF_PATTERN.fullmatch(value)
    if not match or int(match.group("port")) != stack.registry_port:
        fail("image reference must use the current localhost registry endpoint")
    if require_digest and not match.group("digest"):
        fail("Job image references must be immutable sha256 digests")
    if not match.group("tag") and not match.group("digest"):
        fail("image reference must contain a tag or digest")
    return match


def resolve_pushed_image(stack: Stack, value: str) -> str:
    match = _validate_image_ref(value, stack)
    if match.group("digest"):
        return value
    result = run_command(
        ["docker", "image", "inspect", "--format", "{{json .RepoDigests}}", value],
        capture=True,
        check=False,
    )
    if result.returncode:
        fail("cannot inspect the requested Playwright image tag")
    try:
        repo_digests = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        fail(f"Docker returned invalid image digest metadata: {error}")
    if not isinstance(repo_digests, list):
        fail("Docker did not return image digest metadata")
    host_prefix = f"localhost:{stack.registry_port}/"
    for item in repo_digests:
        if isinstance(item, str) and item.startswith(host_prefix) and re.search(r"@sha256:[0-9a-f]{64}$", item):
            return item
    fail("requested Playwright image tag has not been pushed to the current registry")


def build_image(stack: Stack, explicit_tag: str | None = None) -> tuple[str, str]:
    require_tools(["docker", "git"])
    verify_registry_owner(stack)
    verify_playwright_base()
    host = f"localhost:{stack.registry_port}"
    tag = explicit_tag or f"{host}/{DEFAULT_IMAGE_REPOSITORY}:{stack.run_id}"
    _validate_image_ref(tag, stack)
    match = IMAGE_REF_PATTERN.fullmatch(tag)
    if not match or not match.group("tag"):
        fail("image build tag must include a tag and must not include a digest")
    commit, epoch = git_build_metadata()
    containerfile = str(CONTAINERFILE).replace(os.sep, "/")
    run_command(
        [
            "docker",
            "buildx",
            "build",
            "--load",
            "--tag",
            tag,
            "--build-arg",
            f"SOURCE_COMMIT={commit}",
            "--build-arg",
            f"SOURCE_DATE_EPOCH={epoch}",
            "--file",
            containerfile,
            ".",
        ],
        capture=False,
    )
    run_command(["docker", "push", tag], capture=False)
    image = resolve_pushed_image(stack, tag)
    return tag, image


def read_ca_certificate(stack: Stack) -> str:
    path = stack.run_root / "foundation" / "authority" / "ca.crt"
    try:
        value = path.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as error:
        fail(f"cannot read the run-owned local CA: {error}")
    if not value.startswith("-----BEGIN CERTIFICATE-----") or "-----END CERTIFICATE-----" not in value:
        fail("the run-owned local CA is not a PEM certificate")
    return value


def read_realm_credentials(stack: Stack) -> dict[str, dict[str, str]]:
    realm_config = get_namespaced_object(
        stack,
        "configmap",
        "keycloak-realm",
        stack.identity_namespace,
    )
    if realm_config is None:
        fail("local Keycloak realm ConfigMap does not exist")
    data = realm_config.get("data")
    realm_text = data.get("workloads-realm.json") if isinstance(data, dict) else None
    if not isinstance(realm_text, str):
        fail("local Keycloak realm ConfigMap has no realm JSON")
    try:
        realm = json.loads(realm_text)
    except json.JSONDecodeError as error:
        fail(f"local Keycloak realm JSON is invalid: {error}")
    users = realm.get("users") if isinstance(realm, dict) else None
    if not isinstance(users, list):
        fail("local Keycloak realm has no users list")

    credentials: dict[str, dict[str, str]] = {}
    for actor, role, _username_env, _password_env in AUTH_ACTORS:
        candidates = []
        for user in users:
            if not isinstance(user, dict) or role not in (user.get("realmRoles") or []):
                continue
            username = user.get("username")
            user_credentials = user.get("credentials")
            if not isinstance(username, str) or not isinstance(user_credentials, list):
                continue
            passwords = [
                item.get("value")
                for item in user_credentials
                if isinstance(item, dict) and item.get("type") == "password"
            ]
            if len(passwords) == 1 and isinstance(passwords[0], str):
                candidates.append((username, passwords[0]))
        if len(candidates) != 1:
            fail(f"local Keycloak realm does not contain one valid {actor} browser account")
        username, password = candidates[0]
        if (
            not username.strip()
            or not password
            or "\0" in password
            or "\r" in password
            or "\n" in password
            or len(password) > 4096
        ):
            fail(f"local Keycloak {actor} account contains an invalid password")
        credentials[actor] = {"username": username, "password": password}
    return credentials


def _parse_env_assignment(value: str) -> tuple[str, str]:
    key, separator, env_value = value.partition("=")
    if not separator or not ENV_NAME_PATTERN.fullmatch(key):
        fail("--env values must use KEY=VALUE with an uppercase environment name")
    if key in CONTROLLED_ENV_NAMES or SENSITIVE_ENV_PATTERN.search(key):
        fail(f"--env cannot set a credential or harness-controlled variable: {key}")
    if len(env_value) > 8192:
        fail(f"--env value is too long: {key}")
    return key, env_value


def parse_extra_environment(assignments: list[str], files: list[str]) -> dict[str, str]:
    values: dict[str, str] = {}
    for assignment in assignments:
        key, value = _parse_env_assignment(assignment)
        values[key] = value
    for file_value in files:
        path = Path(file_value)
        if not path.is_absolute():
            path = (ROOT / path).resolve()
        else:
            path = path.resolve()
        if not path.is_file():
            fail("the requested Playwright environment file does not exist")
        try:
            lines = path.read_text(encoding="utf-8").splitlines()
        except (OSError, UnicodeError) as error:
            fail(f"cannot read the Playwright environment file: {error}")
        for line_number, line in enumerate(lines, 1):
            stripped = line.strip()
            if not stripped or stripped.startswith("#"):
                continue
            if stripped.startswith("export "):
                stripped = stripped[7:].lstrip()
            try:
                key, value = _parse_env_assignment(stripped)
            except HarnessError as error:
                fail(f"invalid Playwright environment file entry {line_number}: {error}")
            if len(value) >= 2 and value[0] == value[-1] and value[0] in {"'", '"'}:
                value = value[1:-1]
            values[key] = value
    return values


def _secret_ref(secret_name: str, key: str) -> dict[str, Any]:
    return {"secretKeyRef": {"name": secret_name, "key": key}}


def _common_job_labels(stack: Stack) -> dict[str, str]:
    return owned_labels(stack.run_id, include_app=True)


def portal_probe_script() -> str:
    return (
        "const https = require('node:https');"
        "const base = process.env.LABWEAVER_BASE_URL;"
        "if (!base) process.exit(1);"
        "const request = https.get(new URL('/health/ready', base), response => {"
        "let body = ''; response.setEncoding('utf8');"
        "response.on('data', chunk => { body += chunk; });"
        "response.on('end', () => {"
        "let payload; try { payload = JSON.parse(body); } catch { process.exit(1); }"
        "process.exit(response.statusCode === 200 && payload && payload.status === 'ready' ? 0 : 1);"
        "}); });"
        "request.setTimeout(10000, () => { request.destroy(); process.exit(1); });"
        "request.on('error', () => process.exit(1));"
    )


def make_network_policy(stack: Stack, job_name: str) -> dict[str, Any]:
    labels = _common_job_labels(stack)
    return {
        "apiVersion": "networking.k8s.io/v1",
        "kind": "NetworkPolicy",
        "metadata": {
            "name": job_name,
            "namespace": stack.namespace,
            "labels": labels,
        },
        "spec": {
            "podSelector": {"matchLabels": labels},
            "policyTypes": ["Egress"],
            "egress": [
                {
                    "to": [{"podSelector": {"matchLabels": {"app": "local-dev-portal"}}}],
                    "ports": [{"protocol": "TCP", "port": 8443}],
                },
                {
                    "to": [
                        {
                            "namespaceSelector": {
                                "matchLabels": {"kubernetes.io/metadata.name": "kube-system"}
                            },
                            "podSelector": {"matchLabels": {"k8s-app": "kube-dns"}},
                        }
                    ],
                    "ports": [
                        {"protocol": "UDP", "port": 53},
                        {"protocol": "TCP", "port": 53},
                    ],
                },
            ],
        },
    }


def make_job(
    stack: Stack,
    *,
    job_name: str,
    image: str,
    credentials_secret: str,
    ca_secret: str,
    projects: list[str],
    grep: str | None,
    grep_invert: str | None,
    extra_environment: dict[str, str],
    skip_vm: bool,
    timeout_seconds: int,
) -> dict[str, Any]:
    if stack.portal_port is None:
        fail("portalPort is required for a ready local development stack")
    base_url = f"https://127.0.0.1.nip.io:{stack.portal_port}"
    labels = _common_job_labels(stack)
    common_mount = {"name": "ca", "mountPath": "/run/secrets/labweaver-ca", "readOnly": True}
    probe = {
        "name": "portal-probe",
        "image": image,
        "imagePullPolicy": "IfNotPresent",
        "command": ["/usr/local/bin/labweaver-playwright-entrypoint"],
        "args": ["node", "-e", portal_probe_script()],
        "env": [
            {"name": "LABWEAVER_BASE_URL", "value": base_url},
            {"name": "LABWEAVER_CA_FILE", "value": CA_FILE_IN_JOB},
        ],
        "volumeMounts": [common_mount],
        "securityContext": {
            "allowPrivilegeEscalation": False,
            "capabilities": {"drop": ["ALL"]},
        },
    }

    env: list[dict[str, Any]] = [
        {"name": "CI", "value": "true"},
        {"name": "LABWEAVER_BASE_URL", "value": base_url},
        {"name": "LABWEAVER_CA_FILE", "value": CA_FILE_IN_JOB},
    ]
    for actor, _role, username_env, password_file_env in AUTH_ACTORS:
        env.append({"name": username_env, "valueFrom": _secret_ref(credentials_secret, f"{actor}-username")})
        env.append({"name": password_file_env, "value": f"{CREDENTIALS_DIR_IN_JOB}/{actor}-password"})
    for key, value in sorted(extra_environment.items()):
        env.append({"name": key, "value": value})
    if skip_vm:
        env.append({"name": "LABWEAVER_E2E_SKIP_VM", "value": "true"})

    command = [
        "node",
        "node_modules/@playwright/test/cli.js",
        "test",
        "--config=playwright.config.mjs",
        "--workers=1",
    ]
    for project in projects:
        command.extend(["--project", project])
    if grep is not None:
        command.extend(["--grep", grep])
    if grep_invert is not None:
        command.extend(["--grep-invert", grep_invert])

    main_container = {
        "name": "playwright",
        "image": image,
        "imagePullPolicy": "IfNotPresent",
        "args": command,
        "env": env,
        "volumeMounts": [
            common_mount,
            {
                "name": "credentials",
                "mountPath": CREDENTIALS_DIR_IN_JOB,
                "readOnly": True,
            },
        ],
        "securityContext": {
            "allowPrivilegeEscalation": False,
            "capabilities": {"drop": ["ALL"]},
        },
    }
    return {
        "apiVersion": "batch/v1",
        "kind": "Job",
        "metadata": {"name": job_name, "namespace": stack.namespace, "labels": labels},
        "spec": {
            "backoffLimit": 0,
            "activeDeadlineSeconds": timeout_seconds + 30,
            "template": {
                "metadata": {"labels": labels},
                "spec": {
                    "restartPolicy": "Never",
                    "automountServiceAccountToken": False,
                    "securityContext": {"seccompProfile": {"type": "RuntimeDefault"}},
                    "initContainers": [probe],
                    "containers": [main_container],
                    "volumes": [
                        {
                            "name": "ca",
                            "secret": {"secretName": ca_secret, "defaultMode": 0o444},
                        },
                        {
                            "name": "credentials",
                            "secret": {"secretName": credentials_secret, "defaultMode": 0o400},
                        },
                    ],
                    "terminationGracePeriodSeconds": 10,
                },
            },
        },
    }


def make_secret(name: str, namespace: str, run_id: str, values: dict[str, str]) -> dict[str, Any]:
    return {
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {"name": name, "namespace": namespace, "labels": owned_labels(run_id)},
        "type": "Opaque",
        "immutable": True,
        "stringData": values,
    }


def wait_for_job(stack: Stack, job_name: str, timeout_seconds: int) -> tuple[int, bool]:
    deadline = time.monotonic() + timeout_seconds + 45
    while True:
        job = get_namespaced_object(stack, "job", job_name, stack.namespace)
        if job is None:
            fail("Playwright Job disappeared before completion")
        status = job.get("status")
        if isinstance(status, dict):
            conditions = status.get("conditions")
            if isinstance(conditions, list):
                if any(isinstance(item, dict) and item.get("type") == "Complete" and item.get("status") == "True" for item in conditions):
                    return job_exit_code(stack, job_name), False
                if any(isinstance(item, dict) and item.get("type") == "Failed" and item.get("status") == "True" for item in conditions):
                    return job_exit_code(stack, job_name), False
            if status.get("succeeded", 0) or status.get("failed", 0):
                return job_exit_code(stack, job_name), False
        if time.monotonic() >= deadline:
            return 124, True
        time.sleep(2)


def job_exit_code(stack: Stack, job_name: str) -> int:
    pods = kubectl_json(
        stack,
        ["-n", stack.namespace, "get", "pods", "-l", f"job-name={job_name}"],
    )
    if pods is None:
        fail("cannot read Playwright Job Pods")
    items = pods.get("items")
    if not isinstance(items, list):
        fail("Kubernetes returned an invalid Playwright Pod list")
    init_failure: int | None = None
    for pod in items:
        if not isinstance(pod, dict):
            continue
        metadata = pod.get("metadata")
        if not isinstance(metadata, dict):
            continue
        labels = metadata.get("labels")
        if not isinstance(labels, dict) or labels.get(LABEL_RUN_ID) != stack.run_id or labels.get(LABEL_OWNER) != KUBERNETES_OWNER:
            continue
        status = pod.get("status")
        if not isinstance(status, dict):
            continue
        container_statuses = status.get("containerStatuses")
        if isinstance(container_statuses, list):
            for container_status in container_statuses:
                if not isinstance(container_status, dict) or container_status.get("name") != "playwright":
                    continue
                state = container_status.get("state")
                terminated = state.get("terminated") if isinstance(state, dict) else None
                if isinstance(terminated, dict) and isinstance(terminated.get("exitCode"), int):
                    return max(0, min(255, terminated["exitCode"]))
        init_statuses = status.get("initContainerStatuses")
        if isinstance(init_statuses, list):
            for init_status in init_statuses:
                if not isinstance(init_status, dict):
                    continue
                state = init_status.get("state")
                terminated = state.get("terminated") if isinstance(state, dict) else None
                if isinstance(terminated, dict) and isinstance(terminated.get("exitCode"), int):
                    code = max(0, min(255, terminated["exitCode"]))
                    if code != 0:
                        init_failure = code
    # A successful init container only proves that the portal probe passed;
    # it must never stand in for the Playwright suite's exit code.
    return init_failure if init_failure is not None else 1


def job_logs(stack: Stack, job_name: str, secret_values: Iterable[str]) -> str:
    result = kubectl(
        stack,
        ["-n", stack.namespace, "logs", f"job/{job_name}", "--all-containers=true", "--prefix=false"],
        check=False,
    )
    if result.returncode:
        detail = ((result.stderr or "") + "\n" + (result.stdout or "")).strip()
    else:
        detail = result.stdout or ""
    return _redact_values(detail, secret_values)


PLAYWRIGHT_COUNT_PATTERN = re.compile(
    r"\b([0-9]+)\s+(passed|failed|skipped|flaky|interrupted|timed out)\b"
)
PLAYWRIGHT_SUMMARY_LINE_PATTERN = re.compile(
    r"^\s*(?P<counts>[0-9]+\s+(?:passed|failed|skipped|flaky|interrupted|timed out)"
    r"(?:\s*,?\s+[0-9]+\s+(?:passed|failed|skipped|flaky|interrupted|timed out))*)"
    r"\s*(?P<duration>\(\s*[0-9]+(?:\.[0-9]+)?\s*[smh]\s*\))?\s*$"
)
PLAYWRIGHT_TEST_DETAIL_PATTERN = re.compile(
    r"^\s*(?:[0-9]+\)\s*)?\[[^\]\r\n]+\]\s+›\s+\S.*$"
)


def playwright_summary(logs: str) -> str | None:
    """Extract the complete terminal Playwright count block.

    The list reporter prints failures and successful tests on separate lines.
    Looking only at the last count line can therefore turn a failed run into a
    misleading ``3 passed`` summary.  Restrict parsing to the final contiguous
    block of count-only lines so diagnostic text elsewhere in the log is not
    mistaken for the result.
    """

    ansi_free = re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", logs)
    lines = ansi_free.splitlines()
    summary_indexes = [
        index
        for index, line in enumerate(lines)
        if PLAYWRIGHT_SUMMARY_LINE_PATTERN.fullmatch(line)
    ]
    if not summary_indexes:
        return None

    start = last = summary_indexes[-1]
    for index in range(last - 1, -1, -1):
        if (
            not lines[index].strip()
            or PLAYWRIGHT_SUMMARY_LINE_PATTERN.fullmatch(lines[index])
            or PLAYWRIGHT_TEST_DETAIL_PATTERN.fullmatch(lines[index])
        ):
            start = index
            continue
        break

    pieces: list[str] = []
    duration: str | None = None
    for line in lines[start : last + 1]:
        match = PLAYWRIGHT_SUMMARY_LINE_PATTERN.fullmatch(line)
        if match is None:
            continue
        pieces.extend(
            f"{count} {label}"
            for count, label in PLAYWRIGHT_COUNT_PATTERN.findall(match.group("counts"))
        )
        if match.group("duration"):
            duration = match.group("duration")
    if duration:
        pieces.append(duration)
    return ", ".join(pieces) if pieces else None


def cleanup_resources(stack: Stack, names: dict[str, str], created: set[str]) -> list[str]:
    errors: list[str] = []
    ordered = (
        ("job", names["job"], True),
        ("networkpolicy", names["networkpolicy"], True),
        ("secret", names["credentials"], False),
        ("secret", names["ca"], False),
    )
    for resource_key, (kind, name, include_app) in zip(
        ("job", "networkpolicy", "credentials", "ca"), ordered
    ):
        if resource_key not in created:
            continue
        try:
            delete_owned_object(stack, kind, name, stack.namespace, include_app=include_app)
        except HarnessError as error:
            errors.append(f"{kind}/{name}: {error}")
    return errors


def run_tests(args: argparse.Namespace) -> int:
    stack = load_stack(args.state_file, require_ready=True)
    require_tools(["kubectl", "docker"])
    verify_registry_owner(stack)
    verify_ready_portal_owner(stack)
    extra_environment = parse_extra_environment(args.env, args.env_file)
    if args.skip_vm:
        extra_environment.pop("LABWEAVER_E2E_SKIP_VM", None)
    if args.project:
        projects = list(args.project)
    else:
        projects = list(DEFAULT_PROJECTS)
    for project in projects:
        if not re.fullmatch(r"[A-Za-z0-9_-]+", project):
            fail(f"invalid Playwright project name: {project}")
    if args.grep is not None and len(args.grep) > 4096:
        fail("--grep is too long")
    if args.grep_invert is not None and len(args.grep_invert) > 4096:
        fail("--grep-invert is too long")

    if args.image:
        image = resolve_pushed_image(stack, args.image)
        tag = None
    else:
        tag, image = build_image(stack)
    del tag

    after_build = load_stack(args.state_file, require_ready=True)
    assert_same_run(stack, after_build)
    stack = after_build
    credentials = read_realm_credentials(stack)
    ca_certificate = read_ca_certificate(stack)
    job_name = f"local-dev-playwright-e2e-{stack.run_id}"
    names = {
        "job": job_name,
        "networkpolicy": job_name,
        "credentials": f"{job_name}-credentials",
        "ca": f"{job_name}-ca",
    }
    created: set[str] = set()
    primary_error: HarnessError | None = None
    exit_code = 1
    timed_out = False
    diagnostics = ""
    summary: str | None = None
    try:
        # Check the Job before touching any Secret or policy from an earlier
        # invocation.  An active owned Job must remain intact for its owner.
        prepare_name(stack, "job", names["job"], stack.namespace, include_app=True)
        prepare_name(stack, "secret", names["credentials"], stack.namespace)
        prepare_name(stack, "secret", names["ca"], stack.namespace)
        prepare_name(stack, "networkpolicy", names["networkpolicy"], stack.namespace, include_app=True)

        credential_values: dict[str, str] = {}
        credential_secret_values: dict[str, str] = {}
        for actor, values in credentials.items():
            credential_secret_values[f"{actor}-username"] = values["username"]
            credential_secret_values[f"{actor}-password"] = values["password"]
            credential_values[actor] = values["password"]
        # Reject accidental attempts to smuggle one of the generated secrets
        # into a normal Job environment variable.
        if any(value in extra_environment.values() for value in credential_values.values()):
            fail("Playwright environment values must not contain generated account passwords")

        created.add("credentials")
        apply_object(
            stack,
            make_secret(names["credentials"], stack.namespace, stack.run_id, credential_secret_values),
        )
        created.add("ca")
        apply_object(
            stack,
            make_secret(names["ca"], stack.namespace, stack.run_id, {"ca.crt": ca_certificate}),
        )
        created.add("networkpolicy")
        apply_object(stack, make_network_policy(stack, job_name))
        created.add("job")
        apply_object(
            stack,
            make_job(
                stack,
                job_name=job_name,
                image=image,
                credentials_secret=names["credentials"],
                ca_secret=names["ca"],
                projects=projects,
                grep=args.grep,
                grep_invert=args.grep_invert,
                extra_environment=extra_environment,
                skip_vm=args.skip_vm,
                timeout_seconds=args.timeout_seconds,
            ),
        )
        exit_code, timed_out = wait_for_job(stack, job_name, args.timeout_seconds)
        logs = job_logs(stack, job_name, credential_values.values())
        summary = playwright_summary(logs)
        if exit_code == 0 and summary is None:
            fail("Playwright Job completed without a test summary")
        if exit_code != 0:
            diagnostics = logs
    except HarnessError as error:
        primary_error = error
    finally:
        cleanup_errors = cleanup_resources(stack, names, created)

    if primary_error is not None:
        if cleanup_errors:
            fail(f"{primary_error}; cleanup failed: {'; '.join(cleanup_errors)}")
        raise primary_error
    if cleanup_errors:
        fail("Playwright cleanup failed: " + "; ".join(cleanup_errors))

    result: dict[str, Any] = {
        "status": "failed" if timed_out or exit_code else "passed",
        "exitCode": exit_code,
        "image": image,
        "job": job_name,
        "projects": projects,
    }
    if summary:
        result["summary"] = summary
    if timed_out:
        result["reason"] = "timeout"
    if diagnostics:
        result["diagnostic"] = diagnostics
    print(json.dumps(result, ensure_ascii=False))
    return exit_code


def build_only(args: argparse.Namespace) -> int:
    stack = load_stack(args.state_file, require_ready=False)
    _tag, image = build_image(stack, args.tag)
    print(json.dumps({"image": image}, ensure_ascii=False))
    return 0


def positive_timeout(value: str) -> int:
    try:
        parsed = int(value)
    except ValueError:
        raise argparse.ArgumentTypeError("timeout must be an integer") from None
    if not 60 <= parsed <= 7200:
        raise argparse.ArgumentTypeError("timeout must be between 60 and 7200 seconds")
    return parsed


def create_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command")

    build = subparsers.add_parser("build", aliases=["build-image"], help="build and push the pinned Playwright image")
    build.add_argument("--state-file", default=str(DEFAULT_STATE_FILE))
    build.add_argument("--tag", help="optional tag under the current localhost registry")

    run = subparsers.add_parser("run", help="run Playwright in a run-owned Kubernetes Job")
    run.add_argument("--state-file", default=str(DEFAULT_STATE_FILE))
    run.add_argument("--image", help="pushed tag or immutable digest under the current localhost registry")
    run.add_argument("--project", action="append", help="Playwright project; repeat for multiple projects")
    run.add_argument("--grep")
    run.add_argument("--grep-invert")
    run.add_argument("--env", action="append", default=[], metavar="KEY=VALUE")
    run.add_argument("--env-file", action="append", default=[], metavar="PATH")
    run.add_argument("--skip-vm", action="store_true", help="set LABWEAVER_E2E_SKIP_VM=true")
    run.add_argument("--timeout-seconds", type=positive_timeout, default=1200)
    return parser


def main(argv: list[str] | None = None) -> int:
    parser = create_parser()
    arguments = list(sys.argv[1:] if argv is None else argv)
    if not arguments:
        parser.print_help()
        return 0
    if arguments[0].startswith("-") and arguments[0] not in {"-h", "--help"}:
        arguments.insert(0, "run")
    args = parser.parse_args(arguments)
    if args.command is None:
        parser.print_help()
        return 0
    try:
        if args.command in {"build", "build-image"}:
            return build_only(args)
        return run_tests(args)
    except HarnessError as error:
        print("E2E_HARNESS_ERROR:" + str(error), file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
