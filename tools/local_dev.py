#!/usr/bin/env python3
"""Bring up a disposable local LabWeaver stack on Kind.\n\nAll generated state is kept below .tmp/local-dev and .private/local-dev.  The\ncommand owns the named cluster, registry, namespaces and port-forward it\ncreates, and never touches a user's default kubeconfig or Kubernetes context.\n"""
from __future__ import annotations

import argparse
import base64
import hashlib
import importlib.util
import json
import math
import os
import re
import secrets
import shutil
import socket
import subprocess
import sys
import time
import uuid
from pathlib import Path
from typing import Any, Iterable

try:
    import psutil
except ImportError:  # pragma: no cover - reported when the command is invoked
    psutil = None  # type: ignore[assignment]

try:
    import yaml
except ImportError:  # pragma: no cover - reported when the command is invoked
    yaml = None  # type: ignore[assignment]

ROOT = Path(__file__).resolve().parents[1]
STATE_DIR = ROOT / ".tmp" / "local-dev"
STATE_FILE = STATE_DIR / "state.json"
PRIVATE_DIR = ROOT / ".private" / "local-dev"
NAMESPACE = "labweaver-system"
DATA_NAMESPACE = "labweaver-data"
IDENTITY_NAMESPACE = "keycloak-system"
CLUSTER = "labweaver-local"
REGISTRY = "labweaver-local-registry"
REGISTRY_PORT = "5001"
PORTAL_PORT = "38080"
ACCESS_PORT = "38081"
WEB_PORT = "38082"
RUN_ID = "bootstrap"
KUBERNETES_OWNER_LABEL_VALUE = "tools.local_dev.py"
# The local Kind profile points every enabled workload at the local Keycloak
# issuer through the portal edge. Keep this list aligned with
# values.local-kind.yaml: the disabled BuildKit and KubeVirt executors must not
# receive an allowance that cannot be exercised by the disposable stack.
LOCAL_OIDC_CALLER_WORKLOADS = (
    "control-service",
    "access-service",
    "agent-service",
    "environment-service",
    "evaluation-service",
    "resource-service",
    "container-executor",
    "openssh-gateway",
)
LABEL_NAME_PATTERN = re.compile(r"^[A-Za-z0-9](?:[-A-Za-z0-9_.]*[A-Za-z0-9])?$")
DNS_SUBDOMAIN_LABEL_PATTERN = re.compile(r"^[a-z0-9](?:[-a-z0-9]*[a-z0-9])?$")
REGISTRY_IMAGE = "docker.io/library/registry:3.0.0@sha256:6c5666b861f3505b116bb9aa9b25175e71210414bd010d92035ff64018f9457e"
KIND_IMAGE = "kindest/node:v1.35.0@sha256:452d707d4862f52530247495d180205e029056831160e22870e37e3f6c1ac31f"
POSTGRES_IMAGE = "docker.io/library/postgres:17.6-alpine@sha256:747d5ed1fdeeb124b880fbe3d7c6557d2c4064ae41d6b6297d417882effce4be"
NATS_IMAGE = "docker.io/library/nats:2.14.1-scratch@sha256:4223c8fa116891628611e154fb66570cad599d8f8b3b131b82caf10f378e9dcf"
NATS_BOX_IMAGE = "docker.io/natsio/nats-box:0.18.0@sha256:abdc9f9f0120bb8adfbf674eb037d1551db55356eb198b7bd4ffed377f6950a6"
MINIO_IMAGE = "quay.io/minio/minio:RELEASE.2025-09-07T16-13-09Z@sha256:14cea493d9a34af32f524e538b8346cf79f3321eff8e708c1e2960462bd8936e"
KEYCLOAK_IMAGE = "docker.io/keycloak/keycloak:26.7.0@sha256:1362a9d9f13ab325231ea133610cc905e12805804abc7acbef552dd613720aa6"
CLAUDE_CODE_VERSION = "2.1.215"
CLAUDE_CODE_LINUX_X64_SHA512 = "cf00de4e2b500f7bf4fc6c57de19753d3639e23ee2177fe103d40614cf79ca29ddf61064bcbcafe9a53d59d1fbd3cf6cdc02027c8ea167be00157b41469e9bcd"

class LocalDevError(RuntimeError):
    pass

def fail(message: str) -> None:
    raise LocalDevError(message)

def run(argv: list[str], *, input_text: str | None = None, capture: bool = False,
        check: bool = True) -> subprocess.CompletedProcess[str]:
    try:
        result = subprocess.run(argv, cwd=ROOT, input=input_text, text=True,
                                capture_output=capture, check=False)
    except FileNotFoundError:
        fail("required executable is unavailable: " + argv[0])
    if check and result.returncode:
        detail = (result.stderr or result.stdout or "").strip()[-4000:]
        fail(f"command failed ({result.returncode}): {' '.join(argv)}\n{detail}")
    return result

def need_tools(names: Iterable[str]) -> None:
    missing = [name for name in names if shutil.which(name) is None]
    if missing:
        fail("missing local tools: " + ", ".join(missing))

def require_psutil() -> Any:
    if psutil is None:
        fail("missing Python dependency: psutil")
    return psutil

def load_yaml(path: Path) -> Any:
    if yaml is None:
        fail("missing Python dependency: PyYAML")
    try:
        return yaml.safe_load(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, yaml.YAMLError) as error:
        fail(f"cannot read YAML configuration: {path}: {error}")

def write(path: Path, data: bytes | str, mode: int = 0o600) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data.encode() if isinstance(data, str) else data)
    try:
        os.chmod(path, mode)
    except OSError:
        pass

def digest(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for block in iter(lambda: f.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()

def encode(data: bytes | str) -> str:
    return base64.b64encode(data.encode() if isinstance(data, str) else data).decode()

def secret(name: str, namespace: str, values: dict[str, bytes | str]) -> dict[str, Any]:
    return {"apiVersion": "v1", "kind": "Secret",
            "metadata": {"name": name, "namespace": namespace}, "type": "Opaque",
            "data": {k: encode(v) for k, v in values.items()}}

def config(name: str, namespace: str, values: dict[str, str]) -> dict[str, Any]:
    return {"apiVersion": "v1", "kind": "ConfigMap",
            "metadata": {"name": name, "namespace": namespace}, "data": values}

def _validate_label_key(key: Any, path: str) -> None:
    if not isinstance(key, str) or not key:
        fail(f"invalid Kubernetes label key at {path}: {key!r}")
    if key.count("/") > 1:
        fail(f"invalid Kubernetes label key at {path}: {key!r}")
    if "/" in key:
        prefix, name = key.split("/", 1)
        prefix_parts = prefix.split(".")
        if len(prefix) > 253 or any(
            len(part) > 63 or DNS_SUBDOMAIN_LABEL_PATTERN.fullmatch(part) is None
            for part in prefix_parts
        ):
            fail(f"invalid Kubernetes label key at {path}: {key!r}")
    else:
        name = key
    if len(name) > 63 or LABEL_NAME_PATTERN.fullmatch(name) is None:
        fail(f"invalid Kubernetes label key at {path}: {key!r}")


def _validate_label_map(labels: Any, path: str) -> None:
    if not isinstance(labels, dict):
        fail(f"invalid Kubernetes label map at {path}: expected an object")
    for key, value in labels.items():
        _validate_label_key(key, f"{path}.key")
        if not isinstance(value, str) or len(value) > 63 or (
            value and LABEL_NAME_PATTERN.fullmatch(value) is None
        ):
            fail(f"invalid Kubernetes label value at {path}[{key!r}]: {value!r}")


def validate_kubernetes_label_maps(objects: list[dict[str, Any]]) -> None:
    """Fail before kubectl on any invalid metadata label or selector value."""

    def visit(value: Any, path: str) -> None:
        if isinstance(value, dict):
            for key, child in value.items():
                child_path = f"{path}.{key}"
                if key in ("labels", "matchLabels"):
                    _validate_label_map(child, child_path)
                elif key == "selector" and isinstance(child, dict):
                    if "matchLabels" in child:
                        # The nested matchLabels map is visited on its own.
                        pass
                    elif all(isinstance(item, str) for item in child.values()):
                        _validate_label_map(child, child_path)
                elif key == "matchExpressions" and isinstance(child, list):
                    for index, expression in enumerate(child):
                        if not isinstance(expression, dict):
                            fail(f"invalid Kubernetes selector expression at {child_path}[{index}]")
                        _validate_label_key(expression.get("key"), f"{child_path}[{index}]")
                        values = expression.get("values", [])
                        if not isinstance(values, list):
                            fail(f"invalid Kubernetes selector values at {child_path}[{index}]")
                        for value_index, item in enumerate(values):
                            _validate_label_map(
                                {expression["key"]: item},
                                f"{child_path}[{index}].values[{value_index}]",
                            )
                visit(child, child_path)
        elif isinstance(value, list):
            for index, child in enumerate(value):
                visit(child, f"{path}[{index}]")

    visit(objects, "objects")


def apply(kubeconfig: Path, objects: list[dict[str, Any]]) -> None:
    validate_kubernetes_label_maps(objects)
    payload = "".join("---\n" + json.dumps(obj, sort_keys=True) + "\n" for obj in objects)
    run(["kubectl", "--kubeconfig", str(kubeconfig), "apply", "-f", "-"],
        input_text=payload)

def kubectl(kubeconfig: Path, args: list[str], *, input_text: str | None = None,
            capture: bool = False, check: bool = True) -> subprocess.CompletedProcess[str]:
    return run(["kubectl", "--kubeconfig", str(kubeconfig), *args],
               input_text=input_text, capture=capture, check=check)

def wait_rollout(kubeconfig: Path, kind: str, name: str, namespace: str) -> None:
    result = kubectl(
        kubeconfig,
        ["-n", namespace, "rollout", "status", f"{kind}/{name}", "--timeout=300s"],
        capture=True,
        check=False,
    )
    if result.returncode == 0:
        return

    # Rollout status only reports that the deadline expired.  Capture the
    # small set of facts needed to diagnose a disposable stack before `up`
    # tears down its owned cluster.  Do not use `describe`, inspect Pod env,
    # or read Secret objects here: those forms routinely put credentials in
    # the failure output.
    diagnostics = rollout_failure_diagnostics(kubeconfig, kind, name, namespace)
    detail = _bounded_diagnostic_text(result.stderr or result.stdout or "rollout status failed")
    if diagnostics:
        detail = f"{detail}\n{diagnostics}"
    fail(f"rollout failed for {kind}/{name} in {namespace}:\n{detail}")


def _bounded_diagnostic_text(value: str, limit: int = 4000) -> str:
    """Keep startup diagnostics useful without allowing unbounded output."""

    text = value.strip()
    # Logs are gathered for observability, but accidental credential-shaped
    # values must not be copied into a startup error.  The Kubernetes object
    # queries below deliberately never request env or Secret data.
    text = re.sub(
        r"(?i)([\"']?(?:password|secret|token|authorization|private[_-]?key)[\"']?\s*[:=]\s*)(?:\"[^\"]*\"|'[^']*'|[^\s,;}]+)",
        r"\1<redacted>",
        text,
    )
    if len(text) > limit:
        return "…" + text[-limit:]
    return text


def _rollout_selector(kubeconfig: Path, kind: str, name: str,
                      namespace: str) -> tuple[dict[str, str], str | None]:
    """Read only the workload selector used to find its Pods."""

    result = kubectl(
        kubeconfig,
        ["-n", namespace, "get", f"{kind}/{name}", "-o", "json",
         "--request-timeout=10s"],
        capture=True,
        check=False,
    )
    if result.returncode:
        return {}, f"workload lookup failed: {_bounded_diagnostic_text(result.stderr or result.stdout)}"
    try:
        workload = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        return {}, f"workload lookup returned invalid JSON: {error}"
    selector = workload.get("spec", {}).get("selector", {})
    labels = selector.get("matchLabels") if isinstance(selector, dict) else None
    if not isinstance(labels, dict) or not labels:
        return {}, "workload has no matchLabels selector"
    normalized = {
        str(key): str(value)
        for key, value in labels.items()
        if isinstance(key, str) and isinstance(value, (str, int, float, bool))
    }
    if len(normalized) != len(labels):
        return {}, "workload selector contains an invalid label"
    return normalized, None


def _pod_diagnostic_summary(pod: dict[str, Any]) -> dict[str, Any]:
    status = pod.get("status") if isinstance(pod.get("status"), dict) else {}

    def condition_summary(item: Any) -> dict[str, str]:
        if not isinstance(item, dict):
            return {}
        return {
            key: _bounded_diagnostic_text(str(item[key]), 800)
            for key in ("type", "status", "reason", "message", "lastTransitionTime")
            if item.get(key) is not None
        }

    def container_summary(item: Any) -> dict[str, Any]:
        if not isinstance(item, dict):
            return {}
        state = item.get("state") if isinstance(item.get("state"), dict) else {}
        state_name, state_data = next(iter(state.items()), ("unknown", {}))
        if not isinstance(state_data, dict):
            state_data = {}
        return {
            "name": str(item.get("name", "unknown")),
            "ready": bool(item.get("ready", False)),
            "restartCount": item.get("restartCount", 0),
            "state": state_name,
            "reason": _bounded_diagnostic_text(str(state_data.get("reason", "")), 400),
            "message": _bounded_diagnostic_text(str(state_data.get("message", "")), 800),
        }

    metadata = pod.get("metadata") if isinstance(pod.get("metadata"), dict) else {}
    return {
        "name": str(metadata.get("name", "unknown")),
        "phase": str(status.get("phase", "unknown")),
        "conditions": [condition_summary(item) for item in status.get("conditions", [])],
        "initContainers": [container_summary(item) for item in status.get("initContainerStatuses", [])],
        "containers": [container_summary(item) for item in status.get("containerStatuses", [])],
    }


def _rollout_pods(kubeconfig: Path, namespace: str,
                  selector: dict[str, str]) -> tuple[list[dict[str, Any]], str | None]:
    selector_text = ",".join(f"{key}={value}" for key, value in sorted(selector.items()))
    result = kubectl(
        kubeconfig,
        ["-n", namespace, "get", "pods", "-l", selector_text, "-o", "json",
         "--request-timeout=10s"],
        capture=True,
        check=False,
    )
    if result.returncode:
        return [], f"Pod lookup failed: {_bounded_diagnostic_text(result.stderr or result.stdout)}"
    try:
        payload = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        return [], f"Pod lookup returned invalid JSON: {error}"
    items = payload.get("items")
    if not isinstance(items, list):
        return [], "Pod lookup returned no item list"
    return [_pod_diagnostic_summary(item) for item in items[:10] if isinstance(item, dict)], None


def _rollout_events(kubeconfig: Path, namespace: str) -> tuple[list[dict[str, str]], str | None]:
    result = kubectl(
        kubeconfig,
        ["-n", namespace, "get", "events", "--sort-by=.lastTimestamp", "-o", "json",
         "--request-timeout=10s", "--chunk-size=50"],
        capture=True,
        check=False,
    )
    if result.returncode:
        return [], f"Event lookup failed: {_bounded_diagnostic_text(result.stderr or result.stdout)}"
    try:
        payload = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        return [], f"Event lookup returned invalid JSON: {error}"
    items = payload.get("items")
    if not isinstance(items, list):
        return [], "Event lookup returned no item list"
    events: list[dict[str, str]] = []
    for item in items[-20:]:
        if not isinstance(item, dict):
            continue
        involved = item.get("involvedObject") if isinstance(item.get("involvedObject"), dict) else {}
        events.append({
            "time": str(item.get("lastTimestamp") or item.get("eventTime") or item.get("metadata", {}).get("creationTimestamp", "")),
            "type": str(item.get("type", "")),
            "reason": str(item.get("reason", "")),
            "object": str(involved.get("name", "unknown")),
            "message": _bounded_diagnostic_text(str(item.get("message", "")), 1000),
        })
    return events, None


def _rollout_logs(kubeconfig: Path, namespace: str,
                  pods: list[dict[str, Any]]) -> tuple[list[dict[str, str]], list[str]]:
    logs: list[dict[str, str]] = []
    errors: list[str] = []
    for pod in pods:
        name = pod.get("name")
        if not isinstance(name, str) or not name or name == "unknown":
            continue
        result = kubectl(
            kubeconfig,
            ["-n", namespace, "logs", name, "--all-containers=true", "--tail=100",
             "--limit-bytes=65536", "--request-timeout=10s"],
            capture=True,
            check=False,
        )
        if result.returncode:
            errors.append(f"{name}: {_bounded_diagnostic_text(result.stderr or result.stdout, 1200)}")
        else:
            logs.append({"pod": name, "tail": _bounded_diagnostic_text(result.stdout, 4000)})
    return logs, errors


def rollout_failure_diagnostics(kubeconfig: Path, kind: str, name: str,
                                namespace: str) -> str:
    selector, selector_error = _rollout_selector(kubeconfig, kind, name, namespace)
    if selector_error:
        return f"rollout diagnostics unavailable: {selector_error}"
    pods, pod_error = _rollout_pods(kubeconfig, namespace, selector)
    events, event_error = _rollout_events(kubeconfig, namespace)
    logs, log_errors = _rollout_logs(kubeconfig, namespace, pods)
    payload: dict[str, Any] = {
        "rolloutPods": pods,
        "namespaceEvents": events,
        "containerLogsTail": logs,
    }
    errors = [error for error in (pod_error, event_error) if error]
    errors.extend(log_errors)
    if errors:
        payload["diagnosticErrors"] = errors
    return "rollout diagnostics:\n" + _bounded_diagnostic_text(
        json.dumps(payload, ensure_ascii=False, sort_keys=True), 12000
    )


def wait_command(kubeconfig: Path, args: list[str], label: str,
                 *, timeout: float = 180.0) -> None:
    """Wait for a real service probe instead of assuming a running process is ready."""

    deadline = time.monotonic() + timeout
    last_detail = "no response"
    while True:
        result = kubectl(kubeconfig, args, capture=True, check=False)
        if result.returncode == 0:
            return
        last_detail = (result.stderr or result.stdout or "no response").strip()[-1000:]
        if time.monotonic() >= deadline:
            fail(f"{label} did not become ready: {last_detail}")
        time.sleep(2)


def wait_postgres_ready(kubeconfig: Path) -> None:
    wait_command(
        kubeconfig,
        ["-n", DATA_NAMESPACE, "exec", "statefulset/postgres", "--",
         "pg_isready", "-U", "postgres", "-d", "labweaver"],
        "PostgreSQL",
    )


def wait_local_oidc_issuer(kubeconfig: Path, port: int, foundation: Path) -> None:
    """Verify Kind can reach the loopback-only portal OIDC issuer."""

    name = f"local-dev-oidc-probe-{RUN_ID}"
    probe_policy = f"{name}-egress"
    # The OIDC probe only needs the public CA used to validate the portal
    # certificate. Bootstrap's NATS and MinIO credentials are deliberately
    # kept out of this short-lived Pod.
    apply(kubeconfig, [local_oidc_probe_secret(foundation)])
    apply(kubeconfig, [{
        "apiVersion": "networking.k8s.io/v1",
        "kind": "NetworkPolicy",
        "metadata": {"name": probe_policy, "namespace": DATA_NAMESPACE},
        "spec": {
            "podSelector": {"matchLabels": {
                "labweaver.local-dev.run-id": RUN_ID,
                "labweaver.local-dev.owner": KUBERNETES_OWNER_LABEL_VALUE,
            }},
            "policyTypes": ["Egress"],
            "egress": [
                {"to": [{"namespaceSelector": {"matchLabels": {
                    "kubernetes.io/metadata.name": NAMESPACE,
                }}, "podSelector": {"matchLabels": {"app": "local-dev-portal"}}}],
                 "ports": [{"protocol": "TCP", "port": 8443}]},
                {"to": [{"namespaceSelector": {"matchLabels": {
                    "kubernetes.io/metadata.name": "kube-system",
                }}, "podSelector": {"matchLabels": {"k8s-app": "kube-dns"}}}],
                 "ports": [{"protocol": "UDP", "port": 53}, {"protocol": "TCP", "port": 53}]},
            ],
        },
    }])
    try:
        start_admin_pod(
            kubeconfig,
            name,
            NATS_BOX_IMAGE,
            secret_name=local_oidc_probe_secret_name(),
        )
        wait_command(
            kubeconfig,
            ["-n", DATA_NAMESPACE, "exec", name, "--", "curl", "-fsS",
             "--cacert", "/etc/labweaver/admin/keycloak-ca.pem",
             f"https://127.0.0.1.nip.io:{port}/identity/realms/workloads/.well-known/openid-configuration"],
             "loopback-only portal OIDC issuer",
        )
    finally:
        delete_admin_pod(kubeconfig, name)
        kubectl(kubeconfig, ["-n", DATA_NAMESPACE, "delete", "secret",
                             local_oidc_probe_secret_name(), "--ignore-not-found"], check=False)
        kubectl(kubeconfig, ["-n", DATA_NAMESPACE, "delete", "networkpolicy", probe_policy,
                             "--ignore-not-found"], check=False)


def local_platform_defaults() -> dict[str, Any]:
    defaults = load_yaml(
        ROOT / "deploy" / "ansible" / "roles" / "platform_application" /
        "defaults" / "main.yml"
    )
    if not isinstance(defaults, dict):
        fail("platform application defaults must be a YAML object")
    return defaults


def _required_private_file(path: Path) -> bytes:
    try:
        value = path.read_bytes()
    except (OSError, UnicodeError) as error:
        fail(f"local foundation file is unavailable: {path}: {error}")
    if not value:
        fail(f"local foundation file is empty: {path}")
    return value


def local_admin_secret(foundation: Path) -> dict[str, Any]:
    """Expose only short-lived, local admin material to bootstrap Pods."""

    nats_admin = foundation / "nats-clients" / "platform-admin"
    minio_secrets = foundation / "render-input" / "secrets" / "minio-secrets"
    minio_password = _required_private_file(minio_secrets / "root-password").decode().strip()
    if not minio_password:
        fail("local MinIO root password is empty")
    mc_config = {
        "version": "10",
        "aliases": {
            "local": {
                "url": "https://minio.labweaver-data.svc:9000/",
                "accessKey": "labweaver-root",
                "secretKey": minio_password,
                "api": "S3v4",
                "path": "on",
            }
        },
    }
    return secret(
        f"local-dev-admin-{RUN_ID}",
        DATA_NAMESPACE,
        {
            "nats.creds": _required_private_file(nats_admin / "nats.creds"),
            "nats-ca.pem": _required_private_file(nats_admin / "nats-ca.pem"),
            "nats-client.crt": _required_private_file(nats_admin / "nats-client.crt"),
            "nats-client.key": _required_private_file(nats_admin / "nats-client.key"),
            "minio-ca.pem": _required_private_file(minio_secrets / "ca.crt"),
            "keycloak-ca.pem": _required_private_file(foundation / "authority" / "ca.crt"),
            "config.json": json.dumps(mc_config, sort_keys=True),
        },
    )


def local_admin_secret_name() -> str:
    return f"local-dev-admin-{RUN_ID}"


def local_oidc_probe_secret_name() -> str:
    return f"local-dev-oidc-probe-{RUN_ID}"


def local_oidc_probe_secret(foundation: Path) -> dict[str, Any]:
    """Return the probe-only Secret containing the portal trust anchor."""

    return secret(
        local_oidc_probe_secret_name(),
        DATA_NAMESPACE,
        {"keycloak-ca.pem": _required_private_file(foundation / "authority" / "ca.crt")},
    )


def admin_pod(name: str, image: str, command: list[str], *,
              secret_name: str | None = None) -> dict[str, Any]:
    return {
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": {
            "name": name,
            "namespace": DATA_NAMESPACE,
            "labels": {
                "labweaver.local-dev.run-id": RUN_ID,
                "labweaver.local-dev.owner": KUBERNETES_OWNER_LABEL_VALUE,
            },
        },
        "spec": {
            "automountServiceAccountToken": False,
            "restartPolicy": "Never",
            "containers": [{
                "name": "admin",
                "image": image,
                "imagePullPolicy": "IfNotPresent",
                "command": command,
                "volumeMounts": [{
                    "name": "admin-material",
                    "mountPath": "/etc/labweaver/admin",
                    "readOnly": True,
                }, {
                    "name": "writable-admin",
                    "mountPath": "/tmp/labweaver-mc",
                }],
            }],
            "volumes": [
                {
                    "name": "admin-material",
                    "secret": {
                        "secretName": secret_name or local_admin_secret_name(),
                        "defaultMode": 0o400,
                    },
                },
                {"name": "writable-admin", "emptyDir": {}},
            ],
        },
    }


def start_admin_pod(kubeconfig: Path, name: str, image: str,
                    command: list[str] | None = None, *,
                    secret_name: str | None = None) -> None:
    # An interrupted local bootstrap can leave the short-lived helper behind;
    # remove that exact run-owned Pod before recreating its immutable spec.
    delete_admin_pod(kubeconfig, name)
    apply(kubeconfig, [admin_pod(name, image, command or ["sleep", "3600"],
                                 secret_name=secret_name)])
    deadline = time.monotonic() + 180
    last_detail = "pod has not been observed"
    while True:
        result = kubectl(
            kubeconfig,
            ["-n", DATA_NAMESPACE, "get", "pod", name, "-o", "json"],
            capture=True,
            check=False,
        )
        if result.returncode == 0:
            try:
                pod = json.loads(result.stdout)
            except json.JSONDecodeError as error:
                fail(f"bootstrap Pod status is invalid: {error}")
            phase = pod.get("status", {}).get("phase")
            if phase == "Running":
                return
            if phase in {"Succeeded", "Failed"}:
                fail(f"bootstrap Pod {name} exited before readiness (phase={phase})")
            last_detail = f"phase={phase or 'unknown'}"
        else:
            last_detail = (result.stderr or result.stdout or "pod lookup failed").strip()[-1000:]
        if time.monotonic() >= deadline:
            fail(f"bootstrap Pod {name} did not become ready: {last_detail}")
        time.sleep(2)


def admin_exec(kubeconfig: Path, pod: str, command: list[str], *,
               check: bool = True) -> subprocess.CompletedProcess[str]:
    return kubectl(
        kubeconfig,
        ["-n", DATA_NAMESPACE, "exec", pod, "--", *command],
        capture=True,
        check=check,
    )


def delete_admin_pod(kubeconfig: Path, name: str) -> None:
    kubectl(kubeconfig, ["-n", DATA_NAMESPACE, "delete", "pod", name,
                         "--ignore-not-found", "--wait=true"], check=False)


def nats_admin_command(command: list[str]) -> list[str]:
    return [
        "nats",
        "--server", "tls://nats.labweaver-data.svc:4222",
        "--creds", "/etc/labweaver/admin/nats.creds",
        "--tlsca", "/etc/labweaver/admin/nats-ca.pem",
        "--tlscert", "/etc/labweaver/admin/nats-client.crt",
        "--tlskey", "/etc/labweaver/admin/nats-client.key",
        *command,
    ]


def minio_admin_command(command: list[str]) -> list[str]:
    return [
        "env", "SSL_CERT_FILE=/etc/labweaver/admin/minio-ca.pem",
        "MC_CONFIG_DIR=/tmp/labweaver-mc", "mc", "-C", "/tmp/labweaver-mc", *command,
    ]


def _command_detail(result: subprocess.CompletedProcess[str]) -> str:
    return (result.stderr or result.stdout or "no response").strip()[-2000:]


def _minio_json_lines(result: subprocess.CompletedProcess[str], label: str) -> list[dict[str, Any]]:
    """Decode the JSON-lines output emitted by the fixed MinIO client."""

    documents: list[dict[str, Any]] = []
    for line_number, line in enumerate(result.stdout.splitlines(), 1):
        if not line.strip():
            continue
        try:
            document = json.loads(line)
        except json.JSONDecodeError as error:
            fail(f"{label} returned invalid JSON on line {line_number}: {error}")
        if not isinstance(document, dict):
            fail(f"{label} returned a non-object JSON value on line {line_number}")
        documents.append(document)
    return documents


def _minio_json_object(result: subprocess.CompletedProcess[str], label: str) -> dict[str, Any]:
    try:
        document = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        fail(f"{label} returned invalid JSON: {error}")
    if not isinstance(document, dict):
        fail(f"{label} returned a non-object JSON value")
    return document


def bootstrap_minio_bucket(minio: Any, bucket: str) -> None:
    """Create and verify the local artifact bucket's immutable-store contract."""

    if not re.fullmatch(r"[a-z0-9][a-z0-9.-]{1,61}[a-z0-9]", bucket):
        fail("platform application MinIO bucket name is invalid")
    bucket_target = f"local/{bucket}"

    # Enumerate the bucket root first.  This keeps an existing bucket on the
    # read-only path: `mc mb --ignore-existing --with-lock` is intentionally
    # never used as a way to upgrade or migrate an existing bucket.
    listed = minio(["ls", "--json", "local"], check=False)
    if listed.returncode != 0:
        fail(f"cannot inspect MinIO buckets: {_command_detail(listed)}")
    bucket_exists = any(
        entry.get("type") == "folder" and entry.get("key") == f"{bucket}/"
        for entry in _minio_json_lines(listed, "MinIO bucket listing")
    )
    if not bucket_exists:
        created = minio(["mb", "--with-lock", "--ignore-existing", bucket_target], check=False)
        if created.returncode != 0:
            fail(f"cannot create MinIO Object Lock bucket {bucket}: {_command_detail(created)}")

    # `--default` reads the bucket Object Lock configuration.  A successful
    # response with `enabled=Enabled` proves the capability while leaving the
    # retention mode and validity untouched for per-object freeze requests.
    locking = minio(
        ["retention", "info", "--default", bucket_target, "--json"],
        check=False,
    )
    if locking.returncode != 0:
        fail(f"MinIO bucket {bucket} Object Lock is not enabled: {_command_detail(locking)}")
    lock_info = _minio_json_object(locking, f"MinIO bucket {bucket} Object Lock status")
    if (not isinstance(lock_info, dict)
            or lock_info.get("status") != "success"
            or lock_info.get("enabled") != "Enabled"):
        fail(f"MinIO bucket {bucket} Object Lock is not enabled: {_command_detail(locking)}")

    enabled = minio(["version", "enable", bucket_target], check=False)
    if enabled.returncode != 0:
        fail(f"cannot enable MinIO bucket versioning: {_command_detail(enabled)}")
    version = minio(["version", "info", bucket_target, "--json"], check=False)
    version_info = (
        _minio_json_object(version, f"MinIO bucket {bucket} versioning status")
        if version.returncode == 0
        else None
    )
    versioning = version_info.get("versioning") if version_info is not None else None
    if (version.returncode != 0 or version_info is None
            or version_info.get("status") != "success"
            or not isinstance(versioning, dict)
            or versioning.get("status") != "Enabled"):
        fail(f"MinIO bucket {bucket} versioning was not enabled: {_command_detail(version)}")


def bootstrap_nats_and_minio(kubeconfig: Path, foundation: Path) -> None:
    defaults = local_platform_defaults()
    streams = defaults.get("platform_application_nats_streams")
    consumers = defaults.get("platform_application_nats_consumers")
    if not isinstance(streams, list) or not streams or not isinstance(consumers, list):
        fail("platform application NATS defaults are incomplete")
    for item in streams:
        if (not isinstance(item, dict) or not isinstance(item.get("name"), str)
                or not isinstance(item.get("subjects"), list)
                or not item.get("subjects") or not isinstance(item.get("max_age"), str)):
            fail("platform application NATS stream defaults are invalid")
    for item in consumers:
        if (not isinstance(item, dict) or not isinstance(item.get("name"), str)
                or not isinstance(item.get("stream"), str)
                or not isinstance(item.get("filters"), list) or not item.get("filters")):
            fail("platform application NATS consumer defaults are invalid")

    admin = local_admin_secret(foundation)
    apply(kubeconfig, [admin])
    nats_pod = f"local-dev-nats-admin-{RUN_ID}"
    minio_pod = f"local-dev-minio-admin-{RUN_ID}"
    try:
        start_admin_pod(kubeconfig, nats_pod, NATS_BOX_IMAGE)
        start_admin_pod(
            kubeconfig,
            minio_pod,
            MINIO_IMAGE,
            ["sh", "-c", "cp /etc/labweaver/admin/config.json /tmp/labweaver-mc/config.json && exec sleep 3600"],
        )

        # Probe both the NATS monitor health endpoint and an authenticated
        # JetStream request.  The latter catches TLS/JWT wiring errors that a
        # process-level Kubernetes probe cannot see.
        wait_command(
            kubeconfig,
            ["-n", DATA_NAMESPACE, "exec", nats_pod, "--", "curl", "-fsS",
             "http://nats.labweaver-data.svc:8222/healthz"],
            "NATS health endpoint",
        )
        wait_command(kubeconfig, ["-n", DATA_NAMESPACE, "exec", nats_pod, "--",
                                  *nats_admin_command(["stream", "ls", "--json"])],
                     "NATS authenticated JetStream")

        def nats(command: list[str], *, check: bool = True) -> subprocess.CompletedProcess[str]:
            return admin_exec(kubeconfig, nats_pod, nats_admin_command(command), check=check)

        for item in streams:
            name = item["name"]
            current = nats(["stream", "info", name, "--json"], check=False)
            if current.returncode != 0:
                detail = _command_detail(current).lower()
                if not any(marker in detail for marker in ("not found", "does not exist", "no stream")):
                    fail(f"cannot inspect NATS stream {name}: {_command_detail(current)}")
                created = nats([
                    "stream", "add", name, "--subjects", ",".join(item["subjects"]),
                    "--storage", "file", "--retention", "limits", "--max-age",
                    item["max_age"], "--discard", "old", "--defaults",
                ])
                if created.returncode != 0:
                    fail(f"cannot create NATS stream {name}: {_command_detail(created)}")
            else:
                try:
                    current_config = json.loads(current.stdout).get("config", {})
                except json.JSONDecodeError as error:
                    fail(f"NATS stream {name} returned invalid JSON: {error}")
                subjects = current_config.get("subjects", [])
                retained = item.get("retained_subjects")
                if not isinstance(subjects, list) or any(not isinstance(subject, str) for subject in subjects):
                    fail(f"NATS stream {name} returned an invalid subject list")
                if sorted(subjects) != sorted(item["subjects"]):
                    if not isinstance(retained, list) or sorted(subjects) != sorted(retained):
                        fail(f"NATS stream {name} conflicts with the deployment contract")
                    edited = nats([
                        "stream", "edit", name, "--subjects", ",".join(item["subjects"]),
                        "--force",
                    ])
                    if edited.returncode != 0:
                        fail(f"cannot expand NATS stream {name}: {_command_detail(edited)}")
            verified = nats(["stream", "info", name, "--json"])
            try:
                config = json.loads(verified.stdout).get("config", {})
            except json.JSONDecodeError as error:
                fail(f"NATS stream {name} returned invalid JSON after bootstrap: {error}")
            actual_subjects = config.get("subjects", [])
            if (not isinstance(actual_subjects, list)
                    or any(not isinstance(subject, str) for subject in actual_subjects)
                    or config.get("name") != name
                    or sorted(actual_subjects) != sorted(item["subjects"])
                    or config.get("storage") != "file" or config.get("retention") != "limits"
                    or config.get("discard") != "old"):
                fail(f"NATS stream {name} does not match the deployment contract")

        for item in consumers:
            name = item["name"]
            stream = item["stream"]
            current = nats(["consumer", "info", stream, name, "--json"], check=False)
            consumer_config = {
                "ack_policy": "explicit",
                "ack_wait": 30_000_000_000,
                "deliver_policy": "all",
                "durable_name": name,
                "name": name,
                "filter_subjects": item["filters"],
                "max_ack_pending": 1000,
                "max_deliver": 10,
                "max_waiting": 512,
                "replay_policy": "instant",
                "num_replicas": 0,
                "pause_until": "0001-01-01T00:00:00Z",
            }
            config_path = f"/tmp/labweaver-consumer-{name}.json"
            encoded_config = base64.b64encode(
                json.dumps(consumer_config, separators=(",", ":")).encode("utf-8")
            ).decode("ascii")
            write_config = admin_exec(
                kubeconfig,
                nats_pod,
                ["sh", "-c", f"printf '%s' '{encoded_config}' | base64 -d > {config_path}"],
            )
            if write_config.returncode != 0:
                fail(f"cannot materialize NATS consumer config {stream}/{name}: {_command_detail(write_config)}")
            if current.returncode != 0:
                detail = _command_detail(current).lower()
                if not any(marker in detail for marker in ("not found", "does not exist", "no consumer")):
                    fail(f"cannot inspect NATS consumer {stream}/{name}: {_command_detail(current)}")
                created = nats([
                    "consumer", "add", stream,
                    f"--config={config_path}", "--defaults",
                ])
                if created.returncode != 0:
                    fail(f"cannot create NATS consumer {stream}/{name}: {_command_detail(created)}")
            else:
                try:
                    current_consumer = json.loads(current.stdout)
                    current_config = current_consumer.get("config", {})
                except json.JSONDecodeError as error:
                    fail(f"NATS consumer {stream}/{name} returned invalid JSON: {error}")
                current_filters = current_config.get("filter_subjects", [])
                if not current_filters and current_config.get("filter_subject"):
                    current_filters = [current_config["filter_subject"]]
                if (current_config.get("filter_subject")
                        or sorted(current_filters) != sorted(item["filters"])):
                    edited = nats([
                        "consumer", "edit", stream, name,
                        f"--config={config_path}", "--force",
                    ])
                    if edited.returncode != 0:
                        fail(f"cannot update NATS consumer {stream}/{name}: {_command_detail(edited)}")
            verified = nats(["consumer", "info", stream, name, "--json"])
            try:
                consumer = json.loads(verified.stdout)
                config = consumer.get("config", {})
            except json.JSONDecodeError as error:
                fail(f"NATS consumer {stream}/{name} returned invalid JSON: {error}")
            if (consumer.get("name") != name or consumer.get("stream_name") != stream
                    or config.get("durable_name") != name or config.get("ack_policy") != "explicit"
                    or config.get("filter_subject")
                    or sorted(config.get("filter_subjects", [])) != sorted(item["filters"])
                    or config.get("deliver_policy") not in ("all", "ALL")):
                fail(f"NATS consumer {stream}/{name} does not match the deployment contract")

        def minio(command: list[str], *, check: bool = True) -> subprocess.CompletedProcess[str]:
            return admin_exec(kubeconfig, minio_pod, minio_admin_command(command), check=check)

        wait_command(kubeconfig, ["-n", DATA_NAMESPACE, "exec", minio_pod, "--",
                                  *minio_admin_command(["ready", "local"])],
                     "MinIO ready endpoint")
        bucket = str(defaults.get("platform_application_minio_bucket", ""))
        bootstrap_minio_bucket(minio, bucket)
    finally:
        delete_admin_pod(kubeconfig, nats_pod)
        delete_admin_pod(kubeconfig, minio_pod)
        kubectl(kubeconfig, ["-n", DATA_NAMESPACE, "delete", "secret",
                             local_admin_secret_name(), "--ignore-not-found"], check=False)

def cert(output: Path, ca_key: Path, ca_crt: Path, name: str,
         sans: list[str], usage: str = "serverAuth,clientAuth") -> tuple[Path, Path]:
    output.mkdir(parents=True, exist_ok=True)
    key, csr, crt, ext = (output / f"{name}{suffix}" for suffix in (".key", ".csr", ".crt", ".ext"))
    run(["openssl", "genpkey", "-algorithm", "RSA", "-pkeyopt", "rsa_keygen_bits:2048", "-out", str(key)])
    if not key.is_file() or key.stat().st_size == 0:
        fail(f"local certificate key was not generated: {key.name}")
    run(["openssl", "req", "-new", "-key", str(key), "-subj", f"/CN={name}", "-out", str(csr)])
    write(ext, "basicConstraints=CA:FALSE\nkeyUsage=digitalSignature,keyEncipherment\n"
               f"extendedKeyUsage={usage}\nsubjectKeyIdentifier=hash\n"
               "authorityKeyIdentifier=keyid,issuer\n"
               f"subjectAltName={','.join(sans)}\n")
    run(["openssl", "x509", "-req", "-in", str(csr), "-CA", str(ca_crt),
         "-CAkey", str(ca_key), "-CAcreateserial", "-days", "825", "-sha256",
         "-extfile", str(ext), "-out", str(crt)])
    if not crt.is_file() or crt.stat().st_size == 0:
        fail(f"local certificate was not generated: {crt.name}")
    csr.unlink(missing_ok=True)
    ext.unlink(missing_ok=True)
    return key, crt

def author_foundation(output: Path) -> None:
    spec = importlib.util.spec_from_file_location(
        "prepare_platform_foundation", ROOT / "tools" / "prepare_platform_foundation.py")
    if spec is None or spec.loader is None:
        fail("cannot load platform foundation authoring module")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    base = output.resolve()

    def docker_nsc(_nsc: Path, _store: Path, args: list[str], _home: Path) -> None:
        mapped: list[str] = []
        for arg in args:
            try:
                rel = Path(arg).resolve().relative_to(base)
            except (ValueError, OSError):
                mapped.append(arg)
            else:
                mapped.append("/work/" + rel.as_posix())
        run(["docker", "run", "--rm", "-v", f"{base}:/work", "-w", "/work",
             NATS_BOX_IMAGE, "nsc", "--all-dirs", "/work/nsc", *mapped])

    module._nsc = docker_nsc
    openssl = shutil.which("openssl")
    ssh_keygen = shutil.which("ssh-keygen")
    if not openssl or not ssh_keygen:
        fail("openssl and ssh-keygen are required for local PKI")
    module.prepare(output, Path(openssl), Path(ssh_keygen), Path("nsc"), 365)

def free_port() -> str:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as listener:
        listener.bind(("127.0.0.1", 0))
        return str(listener.getsockname()[1])


def verify_loopback_nip_io() -> None:
    """Require nip.io's convenience name to remain a loopback-only alias."""

    try:
        addresses = {
            item[4][0]
            for item in socket.getaddrinfo("127.0.0.1.nip.io", None, type=socket.SOCK_STREAM)
        }
    except OSError as error:
        fail(f"cannot resolve the local portal hostname 127.0.0.1.nip.io: {error}")
    if addresses != {"127.0.0.1"}:
        fail("127.0.0.1.nip.io did not resolve exclusively to 127.0.0.1")


def create_cluster(kubeconfig: Path, *, expose_registry: bool) -> str | None:
    kind_config = STATE_DIR / "kind-config.yaml"
    write(kind_config, f"""kind: Cluster\napiVersion: kind.x-k8s.io/v1alpha4\nname: {CLUSTER}\nnodes:\n- role: control-plane\n  image: {KIND_IMAGE}\n""")
    run(["kind", "create", "cluster", "--name", CLUSTER, "--config",
         str(kind_config), "--kubeconfig", str(kubeconfig), "--wait", "120s"])
    run(["docker", "run", "-d", "--restart=always", "--name", REGISTRY,
         "--label", f"labweaver.local-dev.run-id={RUN_ID}",
         "--label", "labweaver.local-dev.owner=tools/local_dev.py",
         "--network", "kind", "-p", f"127.0.0.1:{REGISTRY_PORT}:5000", REGISTRY_IMAGE])
    nodes = kind_nodes()
    hosts_path = f"/etc/containerd/certs.d/localhost:{REGISTRY_PORT}/hosts.toml"
    hosts_toml = (
        f'server = "http://{REGISTRY}:5000"\n\n'
        f'[host."http://{REGISTRY}:5000"]\n'
        '  capabilities = ["pull", "resolve"]\n'
    )
    hosts_dir = hosts_path.rsplit("/", 1)[0]
    for node in nodes:
        node = node.strip()
        run(["docker", "exec", node, "mkdir", "-p", hosts_dir])
        run(["docker", "exec", "-i", node, "sh", "-c", f"cat > {hosts_path}"],
            input_text=hosts_toml)
    if not expose_registry:
        return None
    inspection = run(
        ["docker", "inspect", "--format",
         '{{(index .NetworkSettings.Networks "kind").IPAddress}}', REGISTRY],
        capture=True,
    )
    registry_ip = inspection.stdout.strip()
    if not re.fullmatch(r"(?:[0-9]{1,3}\.){3}[0-9]{1,3}", registry_ip):
        fail("local registry did not expose a valid Kind-network address")
    return registry_ip


def preload_foundation_images() -> None:
    """Pull exact foundation digests into every owned Kind node.

    Loading a digest-only Docker reference with ``kind load`` can import it
    under a generated ``import-YYYY-MM-DD`` name.  Containerd then has the
    image content but cannot resolve the digest reference used by the Pod
    spec.  Pulling from the node's containerd resolver preserves the registry
    identity and digest, so kubelet can use the image without a second pull.
    """

    images = (
        POSTGRES_IMAGE,
        NATS_IMAGE,
        MINIO_IMAGE,
        KEYCLOAK_IMAGE,
        NATS_BOX_IMAGE,
    )
    nodes = kind_nodes()
    for image in images:
        for node in nodes:
            run(["docker", "exec", node, "crictl", "pull", image])


def kind_nodes() -> list[str]:
    """Return the non-empty node names for the owned Kind cluster."""

    result = run(["kind", "get", "nodes", "--name", CLUSTER], capture=True)
    nodes = [node.strip() for node in result.stdout.splitlines() if node.strip()]
    if not nodes:
        fail("Kind did not return the owned node list")
    return nodes


def configure_local_portal_dns(kubeconfig: Path) -> None:
    """Make the public loopback hostname resolve to the in-cluster portal."""

    rewrite = (
        "rewrite name exact 127.0.0.1.nip.io "
        "local-dev-portal.labweaver-system.svc.cluster.local"
    )
    current = kubectl(kubeconfig, ["-n", "kube-system", "get", "configmap", "coredns",
                                   "-o", "json"], capture=True)
    try:
        current_config = json.loads(current.stdout)
    except json.JSONDecodeError as error:
        fail(f"Kind CoreDNS ConfigMap returned invalid JSON: {error}")
    data = current_config.get("data")
    if not isinstance(data, dict):
        fail("Kind CoreDNS ConfigMap has invalid data")
    corefile = data.get("Corefile")
    if not isinstance(corefile, str) or not corefile.strip():
        fail("Kind CoreDNS ConfigMap has no Corefile")
    if rewrite not in corefile.splitlines():
        lines = corefile.splitlines()
        try:
            kubernetes_index = next(i for i, line in enumerate(lines)
                                    if line.strip().startswith("kubernetes cluster.local"))
        except StopIteration:
            fail("Kind CoreDNS Corefile has no kubernetes plugin")
        lines.insert(kubernetes_index, f"    {rewrite}")
        apply(kubeconfig, [config("coredns", "kube-system", {
            "Corefile": "\n".join(lines) + "\n",
            **{key: value for key, value in data.items() if key != "Corefile"},
        })])
        kubectl(kubeconfig, ["-n", "kube-system", "rollout", "restart",
                             "deployment/coredns"])
        wait_rollout(kubeconfig, "deployment", "coredns", "kube-system")

def foundation_objects(foundation: Path) -> list[dict[str, Any]]:
    result: list[dict[str, Any]] = []
    for group, kind in (("configmaps", "ConfigMap"), ("secrets", "Secret")):
        for obj in sorted((foundation / "render-input" / group).iterdir()):
            vals: dict[str, bytes | str] = {}
            for item in obj.iterdir():
                vals[item.name] = item.read_bytes() if kind == "Secret" else item.read_text()
            result.append(secret(obj.name, DATA_NAMESPACE, vals) if kind == "Secret"
                          else config(obj.name, DATA_NAMESPACE, vals))
    return result

def local_registry_objects(registry_ip: str) -> list[dict[str, Any]]:
    """Expose the run-owned Docker registry to local fixture Pods.

    The registry container is outside Kubernetes on the Kind Docker network.
    An explicit Service plus EndpointSlice object keeps the fixture's HTTP
    copy path inside the cluster and avoids depending on host-only names.
    """

    if not re.fullmatch(r"(?:[0-9]{1,3}\.){3}[0-9]{1,3}", registry_ip):
        fail("local registry endpoint address is invalid")
    labels = {
        "labweaver.local-dev.run-id": RUN_ID,
        # Kubernetes label values may not contain a slash.  Keep the dotted
        # owner value consistent with the other local-dev workload labels;
        # the source path remains available as an annotation for inspection.
        "labweaver.local-dev.owner": KUBERNETES_OWNER_LABEL_VALUE,
    }
    annotations = {"labweaver.local-dev.owner": "tools/local_dev.py"}
    return [
        {
            "apiVersion": "v1",
            "kind": "Service",
            "metadata": {"name": "local-registry", "namespace": DATA_NAMESPACE,
                          "labels": labels, "annotations": annotations},
            "spec": {"ports": [{"name": "registry", "port": 5000,
                                  "targetPort": 5000}]},
        },
        {
            "apiVersion": "discovery.k8s.io/v1",
            "kind": "EndpointSlice",
            "metadata": {"name": "local-registry", "namespace": DATA_NAMESPACE,
                          "labels": {**labels, "kubernetes.io/service-name": "local-registry"},
                          "annotations": annotations},
            "addressType": "IPv4",
            "ports": [{"name": "registry", "protocol": "TCP", "port": 5000}],
            "endpoints": [{"addresses": [registry_ip],
                            "conditions": {"ready": True, "serving": True,
                                           "terminating": False}}],
        },
    ]


def start_foundation(kubeconfig: Path, foundation: Path, work: Path,
                     registry_ip: str | None = None) -> None:
    for namespace in (DATA_NAMESPACE, IDENTITY_NAMESPACE):
        kubectl(kubeconfig, ["create", "namespace", namespace], check=False)
    kubectl(kubeconfig, ["label", "namespace", DATA_NAMESPACE,
                         "labweaver.io/infrastructure=true", "--overwrite"], check=False)
    objects = foundation_objects(foundation)
    if registry_ip is not None:
        objects = local_registry_objects(registry_ip) + objects
    apply(kubeconfig, objects)
    objects: list[dict[str, Any]] = [
      {"apiVersion":"apps/v1","kind":"StatefulSet","metadata":{"name":"postgres","namespace":DATA_NAMESPACE},
       "spec":{"serviceName":"postgres","replicas":1,"selector":{"matchLabels":{"app":"postgres"}},
       "template":{"metadata":{"labels":{"app":"postgres"}},"spec":{"containers":[
       {"name":"postgres","image":POSTGRES_IMAGE,"env":[{"name":"POSTGRES_USER","value":"postgres"},
       {"name":"POSTGRES_PASSWORD","valueFrom":{"secretKeyRef":{"name":"postgres-secrets",
       "key":"postgres-password"}}},{"name":"POSTGRES_DB","value":"labweaver"}],
       "ports":[{"containerPort":5432}],
       "readinessProbe":{"exec":{"command":["pg_isready","-U","postgres","-d","labweaver"]},
                          "periodSeconds":2,"failureThreshold":30},
       "livenessProbe":{"exec":{"command":["pg_isready","-U","postgres","-d","labweaver"]},
                         "periodSeconds":10,"failureThreshold":6},
       "volumeMounts":[{"name":"data","mountPath":"/var/lib/postgresql/data"}]}]}},
       "volumeClaimTemplates":[{"metadata":{"name":"data"},"spec":{"accessModes":["ReadWriteOnce"],
       "resources":{"requests":{"storage":"2Gi"}}}}]}},
      {"apiVersion":"v1","kind":"Service","metadata":{"name":"postgres","namespace":DATA_NAMESPACE},
       "spec":{"selector":{"app":"postgres"},"ports":[{"port":5432,"targetPort":5432}]}},
      {"apiVersion":"apps/v1","kind":"Deployment","metadata":{"name":"nats","namespace":DATA_NAMESPACE},
       "spec":{"replicas":1,"selector":{"matchLabels":{"app":"nats"}},"template":{"metadata":{"labels":{"app":"nats"}},
       "spec":{"containers":[{"name":"nats","image":NATS_IMAGE,"args":["-c","/etc/nats/nats-server.conf"],
       "ports":[{"name":"client","containerPort":4222},{"name":"monitor","containerPort":8222}],
       "readinessProbe":{"httpGet":{"path":"/healthz","port":"monitor"},
                          "periodSeconds":2,"failureThreshold":30},
       "livenessProbe":{"httpGet":{"path":"/healthz","port":"monitor"},
                         "periodSeconds":10,"failureThreshold":6},
       "volumeMounts":[{"name":"config","mountPath":"/etc/nats","readOnly":True},
       {"name":"tls","mountPath":"/etc/nats/tls","readOnly":True},{"name":"data","mountPath":"/data"}]}],
       "volumes":[{"name":"config","configMap":{"name":"nats-config"}},{"name":"tls","secret":{"secretName":"nats-server-secrets"}},
       {"name":"data","emptyDir":{}}]}}}},
      {"apiVersion":"v1","kind":"Service","metadata":{"name":"nats","namespace":DATA_NAMESPACE},
       "spec":{"selector":{"app":"nats"},"ports":[{"name":"client","port":4222,"targetPort":4222},
       {"name":"monitor","port":8222,"targetPort":8222}]}},
      {"apiVersion":"apps/v1","kind":"Deployment","metadata":{"name":"minio","namespace":DATA_NAMESPACE},
       "spec":{"replicas":1,"selector":{"matchLabels":{"app":"minio"}},"template":{"metadata":{"labels":{"app":"minio"}},
       "spec":{"containers":[{"name":"minio","image":MINIO_IMAGE,"args":["server","/data","--console-address",":9001"],
       "env":[{"name":"MINIO_ROOT_USER","valueFrom":{"secretKeyRef":{"name":"minio-secrets","key":"root-user"}}},
       {"name":"MINIO_ROOT_PASSWORD","valueFrom":{"secretKeyRef":{"name":"minio-secrets","key":"root-password"}}}],
       "ports":[{"name":"api","containerPort":9000},{"name":"console","containerPort":9001}],
       "readinessProbe":{"httpGet":{"scheme":"HTTPS","path":"/minio/health/ready","port":"api"},
                          "periodSeconds":2,"failureThreshold":30},
       "livenessProbe":{"httpGet":{"scheme":"HTTPS","path":"/minio/health/live","port":"api"},
                         "periodSeconds":10,"failureThreshold":6},
       "volumeMounts":[{"name":"data","mountPath":"/data"},{"name":"tls","mountPath":"/root/.minio/certs","readOnly":True}]}],
       "volumes":[{"name":"data","emptyDir":{}},{"name":"tls","secret":{"secretName":"minio-secrets",
       "items":[{"key":"public.crt","path":"public.crt"},{"key":"private.key","path":"private.key"},
       {"key":"ca.crt","path":"CAs/ca.crt"}]}}]}}}},
      {"apiVersion":"v1","kind":"Service","metadata":{"name":"minio","namespace":DATA_NAMESPACE},
       "spec":{"selector":{"app":"minio"},"ports":[{"name":"api","port":9000,"targetPort":9000},
       {"name":"console","port":9001,"targetPort":9001}]}}
    ]
    apply(kubeconfig, objects)
    wait_rollout(kubeconfig, "statefulset", "postgres", DATA_NAMESPACE)
    wait_postgres_ready(kubeconfig)
    wait_rollout(kubeconfig, "deployment", "nats", DATA_NAMESPACE)
    wait_rollout(kubeconfig, "deployment", "minio", DATA_NAMESPACE)
    bootstrap_nats_and_minio(kubeconfig, foundation)
    bootstrap = ROOT / "migrations" / "bootstrap" / "0001_roles_and_schemas.sql"
    if bootstrap.exists():
        db_passwords = {name: secrets.token_urlsafe(32) for name in
                        ("control-service", "access-service", "agent-service", "environment-service",
                         "evaluation-service", "resource-service")}
        write(work / "database-passwords.json", json.dumps(db_passwords, sort_keys=True) + "\n")
        roles = {"control-service": "lw_control_runtime", "access-service": "lw_access_runtime",
                 "agent-service": "lw_agent_runtime", "environment-service": "lw_environment_runtime",
                 "evaluation-service": "lw_evaluation_runtime", "resource-service": "lw_resource_runtime"}
        password_sql = "\n".join(
            f"ALTER ROLE {roles[name]} PASSWORD '{value}';"
            for name, value in db_passwords.items()
        )
        kubectl(kubeconfig, ["-n",DATA_NAMESPACE,"exec","-i","statefulset/postgres","--",
                             "psql","-U","postgres","-d","labweaver","-v","ON_ERROR_STOP=1","-f","-"],
                input_text=bootstrap.read_text() + "\n" + password_sql + "\n")
    (work / "keycloak").mkdir(parents=True, exist_ok=True)
    key, crt = cert(work / "keycloak", foundation / "authority" / "ca.key",
                    foundation / "authority" / "ca.crt", "keycloak",
                    ["DNS:keycloak", "DNS:keycloak.keycloak-system",
                     "DNS:keycloak.keycloak-system.svc", "DNS:127.0.0.1.nip.io",
                     "DNS:keycloak.keycloak-system.svc.cluster.local",
                     "DNS:localhost", "IP:127.0.0.1"])
    realm = json.loads((ROOT / "tests" / "fixtures" / "keycloak" / "labweaver-test-realm.json").read_text(encoding="utf-8"))
    realm["realm"] = "workloads"
    portal_origin = f"https://127.0.0.1.nip.io:{PORTAL_PORT}"
    realm["users"] = [
        {"username": "teacher", "firstName": "Local", "lastName": "Teacher", "enabled": True,
         "email": "teacher@local.invalid", "emailVerified": True,
         "credentials": [{"type": "password", "value": "local-teacher", "temporary": False}],
         "realmRoles": ["teacher"]},
        {"username": "student", "firstName": "Local", "lastName": "Student", "enabled": True,
         "email": "student@local.invalid", "emailVerified": True,
         "credentials": [{"type": "password", "value": "local-student", "temporary": False}],
         "realmRoles": ["student"]},
        {"username": "platform-admin", "firstName": "Local", "lastName": "Administrator", "enabled": True,
         "email": "platform-admin@local.invalid", "emailVerified": True,
         "credentials": [{"type": "password", "value": "local-platform-admin", "temporary": False}],
         "realmRoles": ["platform-admin"]},
    ]
    identity_defaults = load_yaml(ROOT / "deploy" / "ansible" / "roles" /
                                  "identity_foundation" / "defaults" / "main.yml")
    service_clients = identity_defaults.get("identity_service_clients")
    assignments = identity_defaults.get("identity_service_role_assignments")
    if not isinstance(service_clients, list) or not isinstance(assignments, list):
        fail("identity defaults do not contain the service client contract")
    client_ids = [item.get("client_id") for item in service_clients if isinstance(item, dict)]
    expected_client_ids = [f"labweaver-{name}" for name in
                           ("access", "agent", "control", "environment", "evaluation", "resource")]
    if client_ids != expected_client_ids:
        fail("local Keycloak clients do not match identity defaults")
    client_secrets = {"web": secrets.token_urlsafe(32),
                      **{client_id.removeprefix("labweaver-"): secrets.token_urlsafe(32)
                         for client_id in client_ids}}
    write(work / "client-secrets.json", json.dumps(client_secrets, sort_keys=True) + "\n")
    clients = []
    web_client = next((client for client in realm["clients"]
                       if client.get("clientId") == "labweaver-web"), None)
    if not isinstance(web_client, dict):
        fail("local Keycloak realm has no web client")
    web_client.update({"enabled": True, "publicClient": False,
                       "clientAuthenticatorType": "client-secret",
                       "secret": client_secrets["web"], "fullScopeAllowed": False,
                       "redirectUris": [f"{portal_origin}/auth/callback"],
                       "webOrigins": [portal_origin],
                       "attributes": {"post.logout.redirect.uris": f"{portal_origin}/*"}})
    client_roles: dict[str, set[str]] = {client_id: set() for client_id in client_ids}
    service_account_roles: dict[str, dict[str, set[str]]] = {
        client_id: {} for client_id in client_ids
    }
    for assignment in assignments:
        if not isinstance(assignment, dict):
            fail("identity role assignment is invalid")
        caller = assignment.get("caller")
        target = assignment.get("target")
        roles = assignment.get("roles")
        if caller not in client_ids or target not in client_ids or not isinstance(roles, list) or not roles:
            fail("identity role assignment references an unknown client")
        client_roles[target].update(roles)
        service_account_roles[caller].setdefault(target, set()).update(roles)
    realm_roles = realm.setdefault("roles", {})
    if not isinstance(realm_roles, dict):
        fail("local Keycloak realm roles are invalid")
    realm_roles["client"] = {
        client_id: [{"name": role, "clientRole": True, "composite": False}
                    for role in sorted(client_roles[client_id])]
        for client_id in client_ids
    }
    # Keep the browser client least-privileged while explicitly mapping its
    # user-facing realm roles.  This is a RealmRepresentation scope mapping;
    # relying on fullScopeAllowed would silently broaden every client.
    realm["scopeMappings"] = [{
        "client": "labweaver-web",
        "roles": ["teacher", "student", "platform-admin"],
    }]
    # A service token must carry the target client roles that authorize the
    # call.  Keycloak's audience-resolve mapper derives the corresponding
    # `aud` values from these mappings.  Keep this in the realm import itself
    # so a disposable deployment and a retained realm have the same source of
    # truth; client-local mappers alone only add the token owner's audience.
    client_scope_mappings: dict[str, list[dict[str, Any]]] = {}
    for caller, roles_by_target in service_account_roles.items():
        for target, roles in sorted(roles_by_target.items()):
            # RealmRepresentation.clientScopeMappings is keyed by the
            # role-owner client. Each entry names the client receiving its
            # scope. This is the import representation of the REST scope
            # mapping managed by the identity provisioner.
            client_scope_mappings.setdefault(target, []).append(
                {"client": caller, "roles": sorted(roles)}
            )
    realm["clientScopeMappings"] = client_scope_mappings
    self_target_clients = {
        caller
        for caller, roles_by_target in service_account_roles.items()
        if caller in roles_by_target
    }
    for item in service_clients:
        client_id = item["client_id"]
        client_secret = client_secrets[client_id.removeprefix("labweaver-")]
        audience = item.get("audience")
        if audience != client_id:
            fail(f"identity audience does not match client ID: {client_id}")
        protocol_mappers = [{"name": f"labweaver-audience-{client_id}",
                             "protocol": "openid-connect",
                             "protocolMapper": "oidc-audience-resolve-mapper",
                             "consentRequired": False,
                             "config": {"access.token.claim": "true",
                                        "id.token.claim": "false"}}]
        if client_id in self_target_clients:
            protocol_mappers.append(
                {"name": f"labweaver-self-audience-{client_id}",
                 "protocol": "openid-connect",
                 "protocolMapper": "oidc-audience-mapper",
                 "consentRequired": False,
                 "config": {"included.custom.audience": audience,
                            "access.token.claim": "true",
                            "id.token.claim": "false"}}
            )
        clients.append({"clientId": client_id, "secret": client_secret,
                        "enabled": True, "publicClient": False, "protocol": "openid-connect",
                        "serviceAccountsEnabled": True, "standardFlowEnabled": False,
                        "directAccessGrantsEnabled": False, "fullScopeAllowed": False,
                        "protocolMappers": protocol_mappers})
    for client_id, roles_by_target in service_account_roles.items():
        realm["users"].append({
            "username": f"service-account-{client_id}",
            "enabled": True,
            "serviceAccountClientId": client_id,
            "clientRoles": {target: sorted(roles) for target, roles in roles_by_target.items()},
        })
    realm.setdefault("clients", []).extend(clients)
    apply(kubeconfig, [secret("keycloak-tls", IDENTITY_NAMESPACE,
                               {"tls.crt":crt.read_bytes(),"tls.key":key.read_bytes()}),
                       secret("keycloak-admin", IDENTITY_NAMESPACE,
                              {"username":"admin", "password":secrets.token_urlsafe(32)}),
                       config("keycloak-realm", IDENTITY_NAMESPACE,
                              {"workloads-realm.json":json.dumps(realm)})])
    apply(kubeconfig, [
      {"apiVersion":"apps/v1","kind":"Deployment","metadata":{"name":"keycloak","namespace":IDENTITY_NAMESPACE},
       "spec":{"replicas":1,"selector":{"matchLabels":{"app":"keycloak"}},"template":{"metadata":{"labels":{"app":"keycloak"}},
       "spec":{"containers":[{"name":"keycloak","image":KEYCLOAK_IMAGE,
       "args":["start-dev","--https-port=8443","--http-enabled=false","--hostname-strict=true",
               f"--hostname={portal_origin}/identity","--import-realm"],
       "env":[{"name":"KEYCLOAK_ADMIN","valueFrom":{"secretKeyRef":{"name":"keycloak-admin","key":"username"}}},
       {"name":"KEYCLOAK_ADMIN_PASSWORD","valueFrom":{"secretKeyRef":{"name":"keycloak-admin","key":"password"}}},
       {"name":"KC_HEALTH_ENABLED","value":"true"},
       # Keycloak keeps the management interface private to the cluster.  Its
       # health endpoints are served on port 9000 without the public
       # `/identity` relative path; the user-facing OIDC interface remains
       # HTTPS on port 8443 with KC_HTTP_RELATIVE_PATH=/identity.
       {"name":"KC_HTTP_MANAGEMENT_SCHEME","value":"http"},
       {"name":"KC_HTTP_MANAGEMENT_RELATIVE_PATH","value":"/"},
       {"name":"KC_HTTP_RELATIVE_PATH","value":"/identity"},
       {"name":"KC_HTTPS_CERTIFICATE_FILE","value":"/opt/keycloak/conf/tls.crt"},
       {"name":"KC_HTTPS_CERTIFICATE_KEY_FILE","value":"/opt/keycloak/conf/tls.key"}],
       "ports":[{"name":"https","containerPort":8443},{"name":"management","containerPort":9000}],
       "startupProbe":{"httpGet":{"path":"/health/started","port":"management"},
                        "periodSeconds":2,"failureThreshold":60},
       "readinessProbe":{"httpGet":{"path":"/health/ready","port":"management"},
                          "periodSeconds":2,"failureThreshold":30},
       "livenessProbe":{"httpGet":{"path":"/health/live","port":"management"},
                         "periodSeconds":10,"failureThreshold":6},
       "volumeMounts":[
       {"name":"tls","mountPath":"/opt/keycloak/conf/tls.crt","subPath":"tls.crt","readOnly":True},
       {"name":"tls","mountPath":"/opt/keycloak/conf/tls.key","subPath":"tls.key","readOnly":True},
       {"name":"realm","mountPath":"/opt/keycloak/data/import/workloads-realm.json","subPath":"workloads-realm.json","readOnly":True}]}],
       "volumes":[{"name":"tls","secret":{"secretName":"keycloak-tls"}},
       {"name":"realm","configMap":{"name":"keycloak-realm"}}]}}}},
      {"apiVersion":"v1","kind":"Service","metadata":{"name":"keycloak","namespace":IDENTITY_NAMESPACE},
       "spec":{"selector":{"app":"keycloak"},"ports":[{"name":"https","port":8443,"targetPort":"https"},
                                                    {"name":"management","port":9000,"targetPort":"management"}]}}
    ])
    wait_rollout(kubeconfig, "deployment", "keycloak", IDENTITY_NAMESPACE)


def apply_migrations(kubeconfig: Path, work: Path) -> None:
    """Apply the checked-in catalog through the shared SQL renderer."""
    catalog = ROOT / "migrations" / "catalog.yaml"
    if not catalog.exists():
        fail("the checked-in migration catalog is required for local development")
    sys.path.insert(0, str(ROOT / "tools"))
    try:
        from render_migration_sql import MigrationRenderError, render
    except ImportError:
        fail("the shared migration SQL renderer is unavailable")
    try:
        sql_text, _catalog_sha = render(
            catalog,
            executor_identity="local-dev",
            release_id=str(uuid.uuid4()),
        )
    except MigrationRenderError as error:
        fail(str(error))
    script_path = work / "migrations.sql"
    write(script_path, sql_text)
    kubectl(kubeconfig, ["-n", DATA_NAMESPACE, "exec", "-i", "statefulset/postgres", "--",
                         "psql", "-U", "postgres", "-d", "labweaver", "-v", "ON_ERROR_STOP=1", "-f", "-"],
            input_text=script_path.read_text(encoding="utf-8"))


def start_build_executor_fixture(kubeconfig: Path, foundation: Path,
                                  source_image: str) -> None:
    """Start the bounded local NATS build provider after service images exist."""

    if not isinstance(source_image, str) or "@sha256:" not in source_image:
        fail("local build fixture requires an immutable source image")
    nats_client = foundation / "nats-clients" / "build-executor"
    script = ROOT / "tools" / "local-dev" / "build-executor-fixture.sh"
    if not script.is_file():
        fail("local build fixture script is missing")
    fixture_secret = secret(
        "local-dev-build-executor-fixture",
        DATA_NAMESPACE,
        {
            "nats.creds": _required_private_file(nats_client / "nats.creds"),
            "nats-ca.pem": _required_private_file(nats_client / "nats-ca.pem"),
            "nats-client.crt": _required_private_file(nats_client / "nats-client.crt"),
            "nats-client.key": _required_private_file(nats_client / "nats-client.key"),
        },
    )
    fixture_config = config(
        "local-dev-build-executor-fixture",
        DATA_NAMESPACE,
        {"build-executor-fixture.sh": script.read_text(encoding="utf-8")},
    )
    labels = {
        "app": "local-dev-build-executor-fixture",
        "labweaver.local-dev.run-id": RUN_ID,
        "labweaver.local-dev.owner": KUBERNETES_OWNER_LABEL_VALUE,
    }
    annotations = {"labweaver.local-dev.owner": "tools/local_dev.py"}
    deployment = {
        "apiVersion": "apps/v1",
        "kind": "Deployment",
        "metadata": {"name": "local-dev-build-executor-fixture", "namespace": DATA_NAMESPACE,
                      "labels": labels, "annotations": annotations},
        "spec": {
            "replicas": 1,
            "selector": {"matchLabels": {"app": labels["app"]}},
            "template": {
                "metadata": {"labels": labels, "annotations": annotations},
                "spec": {
                    "containers": [{
                        "name": "build-executor-fixture",
                        "image": NATS_BOX_IMAGE,
                        "command": ["/bin/sh", "/etc/labweaver/fixture-script/build-executor-fixture.sh"],
                        "env": [
                            {"name": "NATS_SERVER", "value": "tls://nats.labweaver-data.svc:4222"},
                            {"name": "NATS_SUBJECT", "value": "labweaver.provider.container_build.execute.v1"},
                            {"name": "FIXTURE_REGISTRY_BASE",
                             "value": "http://local-registry.labweaver-data.svc.cluster.local:5000"},
                            {"name": "FIXTURE_SOURCE_IMAGE", "value": source_image},
                        ],
                        "volumeMounts": [
                            {"name": "script", "mountPath": "/etc/labweaver/fixture-script",
                             "readOnly": True},
                            {"name": "credentials", "mountPath": "/etc/labweaver/fixture",
                             "readOnly": True},
                        ],
                    }],
                    "volumes": [
                        {"name": "script", "configMap": {"name": "local-dev-build-executor-fixture",
                                                               "defaultMode": 0o555}},
                        {"name": "credentials", "secret": {"secretName": "local-dev-build-executor-fixture",
                                                                 "defaultMode": 0o400}},
                    ],
                },
            },
        },
    }
    apply(kubeconfig, [fixture_secret, fixture_config, deployment])
    wait_rollout(kubeconfig, "deployment", "local-dev-build-executor-fixture", DATA_NAMESPACE)


def build_images(*, external_fixtures: bool) -> dict[str, str]:
    commit = run(["git","rev-parse","--short=12","HEAD"], capture=True).stdout.strip()
    epoch = run(["git", "show", "-s", "--format=%ct", "HEAD"], capture=True).stdout.strip()
    if not re.fullmatch(r"[0-9]+", epoch):
        fail("git did not return a numeric commit timestamp for image metadata")
    root = f"localhost:{REGISTRY_PORT}/labweaver/local"
    agent_target = "agent-runtime-fixture" if external_fixtures else "agent-runtime"
    specs = {"control_service":("control-service","runtime-base"),
             "access_service":("access-service","runtime-base"),
             "agent_service":("agent-service",agent_target),
             "environment_service":("environment-service","runtime-base"),
             "evaluation_service":("evaluation-service","runtime-base"),
             "resource_service":("resource-service","runtime-base")}
    images: dict[str, str] = {}
    for key, (service, target) in specs.items():
        tag = f"{root}/{service}:{commit}"
        build_args=["docker","buildx","build","--load","--target",target,"--build-arg",f"SERVICE={service}",
             "--build-arg",f"SOURCE_COMMIT={commit}","--build-arg",f"SOURCE_DATE_EPOCH={epoch}",
             "--tag",tag,"--file","containers/Containerfile.rust","."]
        if key == "agent_service":
            build_args[6:6] = ["--build-arg", f"CLAUDE_CODE_VERSION={CLAUDE_CODE_VERSION}",
                               "--build-arg", f"CLAUDE_CODE_LINUX_X64_SHA512={CLAUDE_CODE_LINUX_X64_SHA512}"]
        run(build_args)
        run(["docker","push",tag])
        ref = run(["docker","inspect","--format","{{index .RepoDigests 0}}",tag],capture=True).stdout.strip()
        match = re.search(r"@sha256:[0-9a-f]{64}$", ref)
        if not match: fail("registry did not return an immutable digest for " + service)
        images[key] = f"{root}/{service}{match.group(0)}"
    evaluation_runner_tag = f"{root}/evaluation-runner:{commit}"
    run(["docker", "buildx", "build", "--load", "--target", "evaluation-runtime",
         "--build-arg", f"SOURCE_COMMIT={commit}",
         "--build-arg", f"SOURCE_DATE_EPOCH={epoch}",
         "--tag", evaluation_runner_tag, "--file", "containers/Containerfile.ansible-probe", "."])
    run(["docker", "push", evaluation_runner_tag])
    ref = run(["docker", "inspect", "--format", "{{index .RepoDigests 0}}",
               evaluation_runner_tag], capture=True).stdout.strip()
    match = re.search(r"@sha256:[0-9a-f]{64}$", ref)
    if not match:
        fail("registry did not return an immutable digest for evaluation-runner")
    images["evaluation_runner"] = f"{root}/evaluation-runner{match.group(0)}"
    for key, name, file in (("web","web","containers/Containerfile.web"),
                            ("openssh_gateway","openssh-gateway","access-gateway/Dockerfile")):
        tag=f"{root}/{name}:{commit}"
        run(["docker","buildx","build","--load","--build-arg",f"SOURCE_COMMIT={commit}",
             "--build-arg",f"SOURCE_DATE_EPOCH={epoch}",
             "--tag",tag,"--file",file,"."])
        run(["docker","push",tag])
        ref=run(["docker","inspect","--format","{{index .RepoDigests 0}}",tag],capture=True).stdout.strip()
        match=re.search(r"@sha256:[0-9a-f]{64}$",ref)
        if not match: fail("registry did not return an immutable digest for "+name)
        images[key]=f"{root}/{name}{match.group(0)}"
    return images


def build_work_runtime_fixture() -> str:
    """Build and publish the seed-initialized Nginx image copied by work fixtures."""

    commit = run(["git", "rev-parse", "--short=12", "HEAD"], capture=True).stdout.strip()
    epoch = run(["git", "show", "-s", "--format=%ct", "HEAD"], capture=True).stdout.strip()
    if not re.fullmatch(r"[0-9]+", epoch):
        fail("git did not return a numeric commit timestamp for image metadata")
    root = f"localhost:{REGISTRY_PORT}/labweaver/local"
    tag = f"{root}/work-runtime-fixture:{commit}"
    run([
        "docker", "buildx", "build", "--load", "--target", "work-runtime-fixture",
        "--build-arg", f"SOURCE_COMMIT={commit}",
        "--build-arg", f"SOURCE_DATE_EPOCH={epoch}",
        "--tag", tag, "--file", "containers/Containerfile.web", ".",
    ])
    run(["docker", "push", tag])
    ref = run(
        ["docker", "inspect", "--format", "{{index .RepoDigests 0}}", tag],
        capture=True,
    ).stdout.strip()
    match = re.search(r"@sha256:[0-9a-f]{64}$", ref)
    if not match:
        fail("registry did not return an immutable digest for work-runtime-fixture")
    return f"{root}/work-runtime-fixture{match.group(0)}"


def local_container_image_repository_prefix() -> str:
    """Resolve the Environment container prefix from the Control deployment config.

    Control is the authority that chooses the repository where an approved
    candidate is built.  The local provider must consume that same prefix,
    with only the deployment-owned Harbor host replaced by this run's local
    registry endpoint.
    """

    control_config = load_yaml(ROOT / "deploy/config/control-plane.yaml.example")
    if not isinstance(control_config, dict):
        fail("control deployment configuration must be a mapping")
    control = control_config.get("control")
    container_build = control.get("containerBuild") if isinstance(control, dict) else None
    prefix = container_build.get("outputRepositoryPrefix") if isinstance(container_build, dict) else None
    if not isinstance(prefix, str) or not prefix.strip():
        fail("control deployment configuration has no containerBuild.outputRepositoryPrefix")
    local_prefix = (
        prefix.strip()
        .replace("harbor.example.invalid", f"localhost:{REGISTRY_PORT}")
        .replace("harbor.internal", f"localhost:{REGISTRY_PORT}")
    )
    if not local_prefix.startswith(f"localhost:{REGISTRY_PORT}/"):
        fail("local container repository prefix does not target the run-owned registry")
    return local_prefix


def local_kind_workspace_configuration() -> tuple[str, str]:
    """Read the local Kind workspace StorageClass and access mode from Helm values."""

    profile_values = load_yaml(ROOT / "deploy/helm/labweaver/values.local-kind.yaml")
    local_validation = (
        profile_values.get("localValidation")
        if isinstance(profile_values, dict)
        else None
    )
    if not isinstance(local_validation, dict) or local_validation.get("mode") != "kind":
        fail("local Kind profile has no localValidation.kind configuration")
    storage_class_name = local_validation.get("storageClassName")
    access_mode = local_validation.get("accessMode")
    if (
        not isinstance(storage_class_name, str)
        or not storage_class_name.strip()
        or not isinstance(access_mode, str)
        or access_mode not in {"ReadWriteOnce", "ReadWriteMany"}
    ):
        fail("local Kind profile has no valid localValidation workspace configuration")
    return storage_class_name, access_mode


def _local_platform_manifest() -> dict[str, Any]:
    """Select bundle objects for every enabled local Kind workload.

    The checked-in platform manifest is the production contract and includes
    executor objects that the disposable Kind profile deliberately disables.
    Keeping those objects in the local input tree makes bundle generation
    manufacture credentials for workloads that Helm will never create.  Read
    the effective profile here so a changed enablement cannot silently leave a
    stale object behind or introduce an unhandled config/secret reference.
    Resource Service owns a separate bundle, so its objects are validated
    against that manifest and are not copied into this platform manifest.
    """
    manifest = json.loads(
        (ROOT / "deploy/config/platform-bundle-manifest.json").read_text(encoding="utf-8")
    )
    base_values = load_yaml(ROOT / "deploy/helm/labweaver/values.yaml")
    profile_values = load_yaml(ROOT / "deploy/helm/labweaver/values.local-kind.yaml")
    base_workloads = base_values.get("workloads") if isinstance(base_values, dict) else None
    profile_workloads = (
        profile_values.get("workloads") if isinstance(profile_values, dict) else None
    )
    if not isinstance(base_workloads, dict) or not isinstance(profile_workloads, dict):
        fail("local Kind profile does not contain workload configuration")
    unknown = set(profile_workloads) - set(base_workloads)
    if unknown:
        fail("local Kind profile references unknown workloads: " + ", ".join(sorted(unknown)))

    referenced = {"configMaps": set(), "secrets": set()}
    for name, base_configuration in base_workloads.items():
        if not isinstance(base_configuration, dict):
            fail(f"workloads.{name} must be a mapping")
        overlay = profile_workloads.get(name, {})
        if not isinstance(overlay, dict):
            fail(f"local Kind workloads.{name} must be a mapping")
        configuration = {**base_configuration, **overlay}
        if not configuration.get("enabled", False):
            continue
        for field, manifest_key in (("configMap", "configMaps"), ("secret", "secrets")):
            object_name = configuration.get(field)
            if object_name:
                referenced[manifest_key].add(object_name)

    resource_manifest = json.loads(
        (ROOT / "deploy/config/resource-bundle-manifest.json").read_text(encoding="utf-8")
    )
    resource_objects = {
        object_name
        for manifest_key in ("configMaps", "secrets")
        for object_name in resource_manifest.get(manifest_key, {})
    }
    for manifest_key, object_names in referenced.items():
        platform_objects = manifest.get(manifest_key)
        if not isinstance(platform_objects, dict):
            fail(f"platform bundle has no {manifest_key} mapping")
        for object_name in sorted(object_names - resource_objects):
            if object_name not in platform_objects:
                fail(f"enabled local workload references missing {manifest_key} object: {object_name}")
        unexpected_resource_objects = object_names & resource_objects
        if unexpected_resource_objects and any(
            object_name in platform_objects for object_name in unexpected_resource_objects
        ):
            fail("resource bundle object is duplicated in platform bundle: " +
                 ", ".join(sorted(unexpected_resource_objects)))
        manifest[manifest_key] = {
            object_name: values
            for object_name, values in platform_objects.items()
            if object_name in object_names and object_name not in resource_objects
        }
    return manifest


def make_app_input(
    work: Path,
    foundation: Path,
    images: dict[str, str],
) -> tuple[Path, Path, str]:
    manifest = _local_platform_manifest()
    # The Kind profile owns a plain local OCI registry only.  BuildKit and
    # Harbor are deployment-owned services and are intentionally absent from
    # this disposable stack.  Render a profile-specific bundle so their
    # configuration and credentials are not manufactured for an executor that
    # cannot run here.
    manifest_path = work / "local-platform-bundle-manifest.json"
    write(manifest_path, json.dumps(manifest, indent=2) + "\n")
    portal_origin = f"https://127.0.0.1.nip.io:{PORTAL_PORT}"
    public_issuer = f"{portal_origin}/identity/realms/workloads"
    container_image_repository_prefix = local_container_image_repository_prefix()
    root=work/"app-input"
    for section, manifest_key in (("configmaps", "configMaps"), ("secrets", "secrets")):
        for name in manifest[manifest_key]:
            (root/section/name).mkdir(parents=True,exist_ok=True)
    sources={"control-service-config/config.yaml":"control-plane.yaml.example",
             "access-service-config/config.yaml":"access-auth.yaml.example",
             "agent-service-config/config.yaml":"agent-control-plane.yaml.example",
             "environment-service-config/providers.json":"environment-providers.local-hostpath.example.json",
             "evaluation-service-config/config.yaml":"evaluation-service.yaml.example",
             "evaluation-service-config/worker.yaml":"evaluation-freeze-worker.yaml.example",
             "container-executor-config/config.yaml":"runtime-executor.yaml.example",
             "kubevirt-executor-config/config.yaml":"runtime-executor.yaml.example",
             "kubevirt-console-executor-config/config.yaml":"kubevirt-console-executor.yaml.example",
             "web-config/deployment.json":"web-deployment.json.example"}
    generated_config_targets = {
        "agent-service-config/anthropic-base-url",
        "agent-service-config/anthropic-model",
    }
    for target, source in sources.items():
        object_name, _key = target.split("/", 1)
        if object_name not in manifest["configMaps"]:
            continue
        data=(ROOT/"deploy/config"/source).read_text()
        data=data.replace("https://keycloak.example.invalid/realms/labweaver", public_issuer)
        data=data.replace("https://keycloak.example.invalid/realms/workloads", public_issuer)
        data=data.replace("https://portal.example.invalid", portal_origin)
        data=data.replace("https://demo.lab.example/", portal_origin + "/")
        data=data.replace("harbor.example.invalid",f"localhost:{REGISTRY_PORT}").replace("harbor.internal",f"localhost:{REGISTRY_PORT}")
        if source == "control-plane.yaml.example":
            evaluation_runner_image = images.get("evaluation_runner")
            if not isinstance(evaluation_runner_image, str) or not re.fullmatch(
                r"[^\s@]+(?:/[^\s@]+)*@sha256:[0-9a-f]{64}", evaluation_runner_image
            ):
                fail(
                    "local control runtime image must be an immutable evaluation_runner image"
                )
            runner_image_pattern = re.compile(r"(?m)^(\s*runnerImage:\s*)[^\r\n]+$")
            data, replacements = runner_image_pattern.subn(
                rf"\g<1>{evaluation_runner_image}", data, count=1
            )
            if replacements != 1:
                fail("control plane configuration has no evaluationRuntime.runnerImage")
        if source == "evaluation-service.yaml.example":
            evaluation_image = images.get("evaluation_service")
            if not isinstance(evaluation_image, str) or not re.fullmatch(
                r"[^\s@]+(?:/[^\s@]+)*@sha256:[0-9a-f]{64}", evaluation_image
            ):
                fail(
                    "local evaluation worker image must be an immutable evaluation_service image"
                )
            worker_image_pattern = re.compile(r"(?m)^(\s*workerImage:\s*)[^\r\n]+$")
            data, replacements = worker_image_pattern.subn(
                rf"\g<1>{evaluation_image}", data, count=1
            )
            if replacements != 1:
                fail("evaluation service configuration has no coordinator.workerImage")
        if source.startswith("environment-providers"):
            data=(ROOT/"deploy/config/environment-providers.local-hostpath.example.json").read_text()
            providers = json.loads(data)
            if not isinstance(providers, list) or not providers:
                fail("local environment provider profile must be a non-empty list")
            container_providers = [
                provider for provider in providers
                if isinstance(provider, dict) and provider.get("providerKind") == "container"
            ]
            if not container_providers:
                fail("local environment provider profile has no container provider")
            storage_class_name, access_mode = local_kind_workspace_configuration()
            # Do not carry the host-path example's storage binding into Kind.
            # Both values remain explicit in the generated provider config so
            # the Environment process cannot silently choose a mode.
            for provider in container_providers:
                provider["workspaceStorageClassName"] = storage_class_name
                provider["workspaceAccessMode"] = access_mode
                provider["imageRepositoryPrefix"] = container_image_repository_prefix
            data=json.dumps(providers, indent=2) + "\n"
        write(root/"configmaps"/target,data)
    # The local profile has no provider credential by default.  Keep the
    # provider boundary explicit and fail closed instead of sending a task to
    # a billable remote endpoint accidentally.
    for target, data in {
        "agent-service-config/anthropic-base-url": "https://127.0.0.1:9/v1\n",
        "agent-service-config/anthropic-model": "claude-sonnet-4-5\n",
    }.items():
        object_name, _key = target.split("/", 1)
        if target in generated_config_targets and object_name in manifest["configMaps"]:
            write(root / "configmaps" / target, data)
    ca=(foundation/"authority/ca.crt").read_bytes()
    platform_ca=(foundation/"platform-authority/ca.crt").read_bytes()
    identities=foundation/"platform-identities"
    nats=foundation/"nats-clients"
    names={p.name for p in nats.iterdir()}
    clients_raw=json.loads((work/"client-secrets.json").read_text())
    clients={f"{name}-service":value for name,value in clients_raw.items()}
    db_passwords=json.loads((work/"database-passwords.json").read_text())
    keyring="platform-primary:"+base64.urlsafe_b64encode(secrets.token_bytes(32)).decode().rstrip("=")+"\n"
    for object_name, keys in manifest["secrets"].items():
        service=object_name.removesuffix("-secrets")
        nats_name=service if service in names else ("environment-service" if service=="container-executor" else service)
        values:dict[str,bytes|str]={}
        for key in keys:
            if key=="database-url":
                db_service = {"container-executor":"environment-service"}.get(service, service)
                db_role = {"control-service":"lw_control_runtime", "access-service":"lw_access_runtime",
                           "agent-service":"lw_agent_runtime", "environment-service":"lw_environment_runtime",
                           "evaluation-service":"lw_evaluation_runtime", "resource-service":"lw_resource_runtime"}[db_service]
                values[key]=f"postgresql://{db_role}:{db_passwords[db_service]}@postgres.{DATA_NAMESPACE}.svc:5432/labweaver?sslmode=disable"
            elif key in ("mtls-ca.pem",): values[key]=platform_ca
            elif key in ("postgres-ca.pem","minio-ca.pem","nats-ca.pem","oidc-ca.pem","service-oidc-ca.pem","outbound-ca.pem"):
                values[key]=ca
            elif key=="nats-server": values[key]=f"tls://nats.{DATA_NAMESPACE}.svc:4222"
            elif key in ("nats.creds","nats-client.crt","nats-client.key"):
                values[key]=(nats/nats_name/key.replace("nats-client.","nats-client.")).read_bytes()
            elif key=="tls.crt":
                cert_service = {"kubevirt-executor":"environment-service"}.get(service, service)
                values[key]=(identities/cert_service/"certificate.pem").read_bytes()
            elif key=="tls.key":
                cert_service = {"kubevirt-executor":"environment-service"}.get(service, service)
                values[key]=(identities/cert_service/"key.pem").read_bytes()
            elif key=="service-client-secret":
                client_service = {"container-executor":"environment-service",
                                  "openssh-gateway":"access-service"}.get(service, service)
                values[key]=clients[client_service]
            elif key=="oidc-client-secret": values[key]=clients["web-service"]
            elif key=="resource-delegation-key": values[key]=base64.urlsafe_b64encode(secrets.token_bytes(32)).decode()
            elif key=="session-keyring.json": values[key]=keyring
            elif key=="anthropic-auth-token": values[key]=os.environ.get("LABWEAVER_ANTHROPIC_AUTH_TOKEN","unconfigured-local-provider")
            elif key=="minio-access-key": values[key]="labweaver-root"
            elif key=="minio-secret-key": values[key]=(foundation/"render-input/secrets/minio-secrets/root-password").read_bytes()
            elif key=="registry-pull-config.json":
                values[key]=json.dumps({"auths": {f"localhost:{REGISTRY_PORT}":
                                                   {"auth": encode("local-dev:local-dev")}}})
            elif key=="system-actor-id": values[key]="00000000-0000-7000-8000-000000000001"
            elif key=="collector-ssh-user-ca-key": values[key]=(foundation/"ssh-authority/collector-ca").read_bytes()
            elif key in ("mtls.crt","mtls.key"): values[key]=(identities/"openssh-gateway"/("certificate.pem" if key=="mtls.crt" else "key.pem")).read_bytes()
            elif key=="ssh_host_ed25519_key":
                host=work/"ssh_host_ed25519_key"
                if not host.exists(): run(["ssh-keygen","-q","-t","ed25519","-N","","-f",str(host)])
                values[key]=host.read_bytes()
            elif key=="target_key":
                target=work/"target_key"
                if not target.exists(): run(["ssh-keygen","-q","-t","ed25519","-N","","-f",str(target)])
                values[key]=target.read_bytes()
            elif key=="target_key-cert.pub":
                target=work/"target_key"
                target_public=Path(str(target)+".pub")
                certificate=Path(str(target)+"-cert.pub")
                if not target.exists() or not target_public.exists():
                    fail("target SSH key must be generated before its certificate")
                if not certificate.exists():
                    run(["ssh-keygen", "-q", "-s", str(foundation/"ssh-authority"/"collector-ca"),
                         "-I", "labweaver-local-gateway", "-n", "labweaver-gateway",
                         "-V", "-5m:+365d", str(target_public)])
                if not certificate.exists():
                    fail("ssh-keygen did not produce the target SSH certificate")
                values[key]=certificate.read_bytes()
            else:
                fail(f"unsupported local bundle secret key: {object_name}/{key}")
        for key,value in values.items(): write(root/"secrets"/object_name/key,value)
    resource_root = work / "resource-input"
    (resource_root / "configmaps" / "resource-service-config").mkdir(parents=True, exist_ok=True)
    (resource_root / "secrets" / "resource-service-secrets").mkdir(parents=True, exist_ok=True)
    http_config = (ROOT / "deploy/config/resource-service.yaml.example").read_text()
    capacity_config = (ROOT / "deploy/config/resource-capacity.json.example").read_text()
    write(resource_root / "configmaps/resource-service-config/http.yaml", http_config)
    write(resource_root / "configmaps/resource-service-config/capacity.json", capacity_config)
    resource_keys = json.loads((ROOT / "deploy/config/resource-bundle-manifest.json").read_text())["secrets"]["resource-service-secrets"]
    resource_values: dict[str, bytes | str] = {}
    for key in resource_keys:
        if key == "database-url":
            resource_values[key] = f"postgresql://lw_resource_runtime:{db_passwords['resource-service']}@postgres.{DATA_NAMESPACE}.svc:5432/labweaver?sslmode=disable"
        elif key == "nats-server": resource_values[key] = (root / "secrets/environment-service-secrets" / key).read_bytes()
        elif key == "nats-ca.pem": resource_values[key] = ca
        elif key in ("nats-client.crt", "nats-client.key", "nats.creds"):
            resource_values[key] = (nats / "resource-service" / key).read_bytes()
        elif key == "mtls-ca.pem": resource_values[key] = platform_ca
        elif key == "oidc-ca.pem": resource_values[key] = ca
        elif key == "service-client-secret": resource_values[key] = clients["resource-service"]
        elif key == "tls.crt": resource_values[key] = (identities / "resource-service" / "certificate.pem").read_bytes()
        elif key == "tls.key": resource_values[key] = (identities / "resource-service" / "key.pem").read_bytes()
        elif key == "resource-delegation-key": resource_values[key] = (root / "secrets/access-service-secrets/resource-delegation-key").read_bytes()
        else: fail(f"unsupported resource bundle secret key: {key}")
    for key, value in resource_values.items():
        write(resource_root / "secrets/resource-service-secrets" / key, value)
    sys.path.insert(0,str(ROOT/"tools"))
    from render_platform_bundle import render_bundle
    payload=render_bundle(manifest_path,root)
    bundle=work/"platform-bundle.yaml"
    write(bundle,payload)
    from render_resource_bundle import render as render_resource_bundle
    resource_payload=render_resource_bundle(ROOT/"deploy/config/resource-bundle-manifest.json",resource_root,None)
    resource_bundle=work/"resource-bundle.yaml"
    write(resource_bundle,resource_payload)
    return bundle,resource_bundle,hashlib.sha256(payload).hexdigest()

def deploy(kubeconfig: Path, images: dict[str,str], bundle: Path, resource_bundle: Path,
           bundle_sha: str, public_issuer: str) -> None:
    kubectl(kubeconfig,["create","namespace",NAMESPACE],check=False)
    kubectl(kubeconfig,["label","namespace",NAMESPACE,"labweaver.io/edge=true","--overwrite"],check=False)
    kubectl(kubeconfig,["apply","-f",str(bundle)])
    kubectl(kubeconfig,["apply","-f",str(resource_bundle)])
    args=["helm","upgrade","--install","labweaver-local","deploy/helm/labweaver",
          "--namespace",NAMESPACE,"--create-namespace","--kubeconfig",str(kubeconfig),
          "--values","deploy/helm/labweaver/values.local-kind.yaml",
          "--set-string",f"deploymentIdentity.configurationBundleSha256=sha256:{bundle_sha}"]
    for workload in ("control-service", "access-service", "agent-service", "environment-service",
                     "container-executor", "evaluation-service", "resource-service", "openssh-gateway"):
        args.extend(["--set-string",
                     f"workloads.{workload}.env.LABWEAVER_SERVICE_OIDC_ISSUER={public_issuer}"])
    for key,value in images.items(): args.extend(["--set-string",f"images.{key}={value}"])
    run(args)
    for name in ("control-service","access-service","agent-service",
                 "environment-service","container-executor","web","evaluation-service",
                 "resource-service","openssh-gateway"):
        wait_rollout(kubeconfig,"deployment",name,NAMESPACE)


def start_owned_process(command: list[str], log_path: Path, port: int,
                        label: str, *, bind: str = "127.0.0.1") -> dict[str, Any]:
    """Start one run-owned listener and wait until its socket is accepting."""

    log = log_path.open("w", encoding="utf-8")
    try:
        process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT)
    except OSError as error:
        log.close()
        fail(f"cannot start {label}: {error}")
    try:
        process_api = require_psutil()
        process_create_time = process_api.Process(process.pid).create_time()
    except (OSError, ValueError) as error:
        process.terminate()
        log.close()
        fail(f"cannot record {label} process identity: {error}")
    deadline = time.monotonic() + 60
    last_detail = "listener has not started"
    while True:
        if process.poll() is not None:
            try:
                detail = log_path.read_text(encoding="utf-8", errors="replace").strip()[-2000:]
            except OSError:
                detail = "process exited without a log"
            log.close()
            fail(f"{label} exited before readiness: {detail}")
        try:
            with socket.create_connection((bind, port), timeout=1):
                log.close()
                return {"pid": process.pid, "createTime": process_create_time,
                        "cmdline": command, "label": label}
        except OSError as error:
            last_detail = str(error)
        if time.monotonic() >= deadline:
            process.terminate()
            log.close()
            fail(f"{label} did not become ready: {last_detail}")
        time.sleep(0.5)


def record_port_forward(record: dict[str, Any]) -> None:
    """Persist a listener immediately so an interrupted startup can clean it."""

    if not STATE_FILE.exists():
        fail("local development state disappeared while starting listeners")
    info = json.loads(STATE_FILE.read_text(encoding="utf-8"))
    forwards = info.setdefault("portForwards", [])
    if not isinstance(forwards, list):
        fail("local development state has invalid port-forward records")
    forwards.append(record)
    write(STATE_FILE, json.dumps(info, indent=2) + "\n")


def deploy_local_portal(kubeconfig: Path, work: Path, images: dict[str, str],
                        portal_key: Path, portal_crt: Path, portal_ca: Path) -> None:
    """Run the browser edge through the same Nginx proxy used in production."""

    nginx_config = """pid /tmp/nginx.pid;
error_log /dev/stderr warn;
events { worker_connections 1024; }
http {
  include /etc/nginx/mime.types;
  default_type application/octet-stream;
  server_tokens off;
  access_log /dev/stdout;
  sendfile on;
  client_body_temp_path /tmp/client_temp;
  proxy_temp_path /tmp/proxy_temp;
  fastcgi_temp_path /tmp/fastcgi_temp;
  uwsgi_temp_path /tmp/uwsgi_temp;
  scgi_temp_path /tmp/scgi_temp;
  proxy_http_version 1.1;
  proxy_buffering off;
  map $http_upgrade $connection_upgrade { default upgrade; '' close; }
  resolver kube-dns.kube-system.svc.cluster.local valid=5s ipv6=off;
  server {
    listen 8443 ssl;
    ssl_certificate /etc/labweaver/tls/tls.crt;
    ssl_certificate_key /etc/labweaver/tls/tls.key;
    ssl_protocols TLSv1.2 TLSv1.3;
    set $labweaver_access access-service.labweaver-system.svc.cluster.local:8080;
    set $labweaver_web web.labweaver-system.svc.cluster.local:8080;
    set $labweaver_keycloak keycloak.keycloak-system.svc.cluster.local:8443;
    set $labweaver_minio minio.labweaver-data.svc.cluster.local:9000;
    location = /health/live {
      access_log off;
      default_type application/json;
      return 200 '{"service":"local-dev-portal","status":"live"}';
    }
    location = /health/ready {
      access_log off;
      default_type application/json;
      return 200 '{"service":"local-dev-portal","status":"ready"}';
    }
    location = /identity {
      return 308 /identity/;
    }
    location /identity/ {
      proxy_pass https://$labweaver_keycloak;
      proxy_set_header Host $http_host;
      proxy_set_header X-Forwarded-Proto https;
      proxy_set_header X-Forwarded-Host $http_host;
      proxy_set_header X-Forwarded-Prefix /identity;
      proxy_ssl_server_name on;
      proxy_ssl_name keycloak.keycloak-system.svc.cluster.local;
      proxy_ssl_verify on;
      proxy_ssl_verify_depth 2;
      proxy_ssl_trusted_certificate /etc/labweaver/trust/ca.crt;
    }
    # Control signs browser uploads for this exact same-origin path. Keep the
    # Host and URI intact while validating MinIO with the local private CA.
    location /labweaver-artifacts {
      client_max_body_size 256m;
      proxy_http_version 1.1;
      proxy_request_buffering off;
      proxy_buffering off;
      proxy_set_header Host $http_host;
      proxy_ssl_server_name on;
      proxy_ssl_name minio.labweaver-data.svc;
      proxy_ssl_verify on;
      proxy_ssl_verify_depth 2;
      proxy_ssl_trusted_certificate /etc/labweaver/trust/ca.crt;
      proxy_pass https://$labweaver_minio;
    }
    location ~ ^/(api|auth|connect)(/|$) {
      proxy_pass http://$labweaver_access;
      proxy_set_header Host $http_host;
      proxy_set_header X-Forwarded-Proto https;
      proxy_set_header X-Forwarded-Host $http_host;
      proxy_set_header Upgrade $http_upgrade;
      proxy_set_header Connection $connection_upgrade;
    }
    location / {
      proxy_pass http://$labweaver_web;
      proxy_set_header Host $http_host;
      proxy_set_header X-Forwarded-Proto https;
      proxy_set_header X-Forwarded-Host $http_host;
      proxy_set_header Upgrade $http_upgrade;
      proxy_set_header Connection $connection_upgrade;
    }
  }
}
"""
    objects = [
        config("local-dev-portal-nginx", NAMESPACE, {"nginx.conf": nginx_config}),
        secret("local-dev-portal-tls", NAMESPACE,
               {"tls.crt": portal_crt.read_bytes(), "tls.key": portal_key.read_bytes()}),
        secret("local-dev-portal-ca", NAMESPACE,
               {"ca.crt": portal_ca.read_bytes()}),
        {"apiVersion": "apps/v1", "kind": "Deployment",
         "metadata": {"name": "local-dev-portal", "namespace": NAMESPACE,
                       "labels": {"labweaver.local-dev.run-id": RUN_ID,
                                  "labweaver.local-dev.owner": KUBERNETES_OWNER_LABEL_VALUE}},
         "spec": {"replicas": 1, "selector": {"matchLabels": {"app": "local-dev-portal"}},
                  "template": {"metadata": {"labels": {"app": "local-dev-portal",
                                                               "app.kubernetes.io/part-of": "labweaver"}},
                               "spec": {"securityContext": {"runAsNonRoot": True,
                                                              "runAsUser": 101,
                                                              "runAsGroup": 101,
                                                              "fsGroup": 101,
                                                              "seccompProfile": {"type": "RuntimeDefault"}},
                                        "containers": [{"name": "nginx", "image": images["web"],
                                                        "ports": [{"name": "https", "containerPort": 8443}],
                                                        "readinessProbe": {"httpGet": {"scheme": "HTTPS",
                                                                                         "path": "/health/ready",
                                                                                         "port": "https"},
                                                                            "periodSeconds": 2,
                                                                            "failureThreshold": 30},
                                                        "securityContext": {"allowPrivilegeEscalation": False,
                                                                             "readOnlyRootFilesystem": True,
                                                                             "runAsNonRoot": True,
                                                                             "capabilities": {"drop": ["ALL"]}},
                                                        "volumeMounts": [{"name": "nginx", "mountPath": "/etc/nginx/nginx.conf",
                                                                           "subPath": "nginx.conf", "readOnly": True},
                                                                          {"name": "tls", "mountPath": "/etc/labweaver/tls",
                                                                           "readOnly": True},
                                                                          {"name": "portal-ca", "mountPath": "/etc/labweaver/trust",
                                                                           "readOnly": True},
                                                                          {"name": "tmp", "mountPath": "/tmp"}]}],
                                        "volumes": [{"name": "nginx", "configMap": {"name": "local-dev-portal-nginx"}},
                                                    {"name": "tls", "secret": {"secretName": "local-dev-portal-tls",
                                                                                   "defaultMode": 0o444}},
                                                    {"name": "portal-ca", "secret": {"secretName": "local-dev-portal-ca",
                                                                                         "defaultMode": 0o444}},
                                                    {"name": "tmp", "emptyDir": {}}]}}}},
        {"apiVersion": "v1", "kind": "Service",
         "metadata": {"name": "local-dev-portal", "namespace": NAMESPACE,
                       "labels": {"labweaver.local-dev.run-id": RUN_ID,
                                  "labweaver.local-dev.owner": KUBERNETES_OWNER_LABEL_VALUE}},
         "spec": {"selector": {"app": "local-dev-portal"},
                  "ports": [{"name": "https", "port": int(PORTAL_PORT), "targetPort": "https"}]}},
        {"apiVersion": "networking.k8s.io/v1", "kind": "NetworkPolicy",
         "metadata": {"name": "local-dev-portal", "namespace": NAMESPACE,
                       "labels": {"labweaver.local-dev.run-id": RUN_ID,
                                  "labweaver.local-dev.owner": KUBERNETES_OWNER_LABEL_VALUE}},
         "spec": {"podSelector": {"matchLabels": {"app": "local-dev-portal"}},
                  "policyTypes": ["Ingress", "Egress"],
                  "ingress": [{"from": [{"podSelector": {}},
                                           {"namespaceSelector": {"matchLabels": {
                                               "kubernetes.io/metadata.name": DATA_NAMESPACE}},
                                            "podSelector": {"matchLabels": {
                                                "labweaver.local-dev.owner": KUBERNETES_OWNER_LABEL_VALUE}}}],
                                "ports": [{"protocol": "TCP", "port": 8443}]}],
                  "egress": [
                      {"to": [{"podSelector": {"matchLabels": {"app.kubernetes.io/name": "web"}}}],
                       "ports": [{"protocol": "TCP", "port": 8080}]},
                      {"to": [{"podSelector": {"matchLabels": {"app.kubernetes.io/name": "access-service"}}}],
                       "ports": [{"protocol": "TCP", "port": 8080}]},
                      {"to": [{"namespaceSelector": {"matchLabels": {"labweaver.io/infrastructure": "true"}}}],
                       "ports": [{"protocol": "TCP", "port": 9000}]},
                      {"to": [{"namespaceSelector": {"matchLabels": {"kubernetes.io/metadata.name": IDENTITY_NAMESPACE}}}],
                       "ports": [{"protocol": "TCP", "port": 8443}]},
                       {"to": [{"namespaceSelector": {"matchLabels": {"kubernetes.io/metadata.name": "kube-system"}},
                                "podSelector": {"matchLabels": {"k8s-app": "kube-dns"}}}],
                        "ports": [{"protocol": "UDP", "port": 53}, {"protocol": "TCP", "port": 53}]},
                   ]}},
        # The local profile uses the portal as the in-cluster OIDC endpoint.
        # This allowance is deliberately scoped to the actual enabled callers
        # and the portal's TLS port; it does not grant a general new egress
        # path to the local workload set.
        {"apiVersion": "networking.k8s.io/v1", "kind": "NetworkPolicy",
         "metadata": {"name": "local-dev-oidc-egress", "namespace": NAMESPACE,
                       "labels": {"labweaver.local-dev.run-id": RUN_ID,
                                  "labweaver.local-dev.owner": KUBERNETES_OWNER_LABEL_VALUE}},
         "spec": {"podSelector": {"matchExpressions": [
                       {"key": "app.kubernetes.io/name", "operator": "In",
                        "values": list(LOCAL_OIDC_CALLER_WORKLOADS)}]},
                   "policyTypes": ["Egress"],
                   "egress": [{"to": [{"podSelector": {"matchLabels": {"app": "local-dev-portal"}}}],
                               "ports": [{"protocol": "TCP", "port": 8443}]}]}},
     ]
    apply(kubeconfig, objects)
    wait_rollout(kubeconfig, "deployment", "local-dev-portal", NAMESPACE)

def up(*, external_fixtures: bool = False) -> None:
    global CLUSTER, REGISTRY, REGISTRY_PORT, PORTAL_PORT, ACCESS_PORT, WEB_PORT, RUN_ID
    need_tools(["docker","kind","kubectl","helm","openssl","ssh-keygen"])
    require_psutil()
    verify_loopback_nip_io()
    if STATE_FILE.exists(): fail("local development stack already has state; run status or down first")
    STATE_DIR.mkdir(parents=True,exist_ok=True); PRIVATE_DIR.mkdir(parents=True,exist_ok=True)
    RUN_ID=uuid.uuid4().hex[:12]
    CLUSTER=f"labweaver-local-{RUN_ID}"
    REGISTRY=f"labweaver-local-registry-{RUN_ID}"
    REGISTRY_PORT=free_port()
    PORTAL_PORT=free_port()
    ACCESS_PORT=free_port()
    WEB_PORT=free_port()
    work=PRIVATE_DIR/RUN_ID; work.mkdir(parents=True)
    kubeconfig=STATE_DIR/"kubeconfig"
    profile = "external-fixtures" if external_fixtures else "default"
    write(STATE_FILE,json.dumps({"phase":"starting","profile":profile,
                                 "externalFixtures":external_fixtures,"runId":RUN_ID,"cluster":CLUSTER,
                                 "registry":REGISTRY,"registryPort":int(REGISTRY_PORT),
                                 "namespace":NAMESPACE,"dataNamespace":DATA_NAMESPACE,
                                 "identityNamespace":IDENTITY_NAMESPACE,
                                 "kubeconfig":str(kubeconfig.relative_to(ROOT)),
                                 "runRoot":str(work.relative_to(ROOT)),"portForwards":[]},indent=2)+"\n")
    try:
        # Author the local PKI and NATS credentials before creating Kind.  This
        # work only needs local/Docker tooling, so an authoring failure should
        # not leave a cluster to tear down.
        foundation=work/"foundation"; foundation.mkdir()
        author_foundation(foundation)
        registry_ip = create_cluster(kubeconfig, expose_registry=external_fixtures)
        preload_foundation_images()
        configure_local_portal_dns(kubeconfig)
        info=json.loads(STATE_FILE.read_text()); info["phase"]="foundation"; write(STATE_FILE,json.dumps(info,indent=2)+"\n")
        start_foundation(kubeconfig,foundation,work,registry_ip)
        kubectl_path = shutil.which("kubectl")
        if not kubectl_path:
            fail("required executable is unavailable: kubectl")
        apply_migrations(kubeconfig,work)
        info=json.loads(STATE_FILE.read_text()); info["phase"]="building"; write(STATE_FILE,json.dumps(info,indent=2)+"\n")
        images=build_images(external_fixtures=external_fixtures)
        if external_fixtures:
            source_image = build_work_runtime_fixture()
            start_build_executor_fixture(kubeconfig, foundation, source_image)
        bundle,resource_bundle,bundle_sha=make_app_input(work,foundation,images)
        kubectl(kubeconfig, ["create", "namespace", NAMESPACE], check=False)
        kubectl(kubeconfig, ["label", "namespace", NAMESPACE, "labweaver.io/edge=true",
                             "--overwrite"], check=False)
        portal_key, portal_crt = cert(
            work / "portal", foundation / "authority" / "ca.key",
            foundation / "authority" / "ca.crt", "portal",
            ["DNS:127.0.0.1.nip.io", "DNS:localhost", "IP:127.0.0.1"],
            usage="serverAuth",
        )
        deploy_local_portal(kubeconfig, work, images, portal_key, portal_crt,
                            foundation / "authority" / "ca.crt")
        portal_forward = start_owned_process(
            [kubectl_path, "--kubeconfig", str(kubeconfig), "-n", NAMESPACE,
             "port-forward", "svc/local-dev-portal", f"{PORTAL_PORT}:{PORTAL_PORT}"],
            work / "portal-port-forward.log", int(PORTAL_PORT), "HTTPS portal port-forward",
        )
        record_port_forward(portal_forward)
        wait_local_oidc_issuer(kubeconfig, int(PORTAL_PORT), foundation)
        info=json.loads(STATE_FILE.read_text()); info["phase"]="deploying"; write(STATE_FILE,json.dumps(info,indent=2)+"\n")
        public_issuer = f"https://127.0.0.1.nip.io:{PORTAL_PORT}/identity/realms/workloads"
        deploy(kubeconfig,images,bundle,resource_bundle,bundle_sha,public_issuer)
        access_forward = start_owned_process(
            [kubectl_path, "--kubeconfig", str(kubeconfig), "-n", NAMESPACE,
             "port-forward", "svc/access-service", f"{ACCESS_PORT}:8080"],
            work / "access-port-forward.log", int(ACCESS_PORT), "Access port-forward",
        )
        record_port_forward(access_forward)
        web_forward = start_owned_process(
            [kubectl_path, "--kubeconfig", str(kubeconfig), "-n", NAMESPACE,
             "port-forward", "svc/web", f"{WEB_PORT}:8080"],
            work / "web-port-forward.log", int(WEB_PORT), "Web port-forward",
        )
        record_port_forward(web_forward)
        state={"phase":"ready","profile":profile,"externalFixtures":external_fixtures,
               "runId":RUN_ID,"cluster":CLUSTER,"registry":REGISTRY,
               "registryPort":int(REGISTRY_PORT),"namespace":NAMESPACE,
               "dataNamespace":DATA_NAMESPACE,"identityNamespace":IDENTITY_NAMESPACE,
               "kubeconfig":str(kubeconfig.relative_to(ROOT)),
               "runRoot":str(work.relative_to(ROOT)),"portForwards":[
                   access_forward, web_forward, portal_forward,
               ],
               "portalPort":int(PORTAL_PORT),
               "accessPort":int(ACCESS_PORT),"webPort":int(WEB_PORT),
               "images":images,"bundleSha256":bundle_sha}
        write(STATE_FILE,json.dumps(state,indent=2)+"\n")
        ready = {"status":"ready","profile":profile,
                 "url":f"https://127.0.0.1.nip.io:{PORTAL_PORT}","cluster":CLUSTER}
        if external_fixtures:
            ready["externalModel"] = "claude-stream-json-fixture"
            ready["externalBuildProvider"] = "nats-oci-copy-fixture"
        print(json.dumps(ready,indent=2))
    except BaseException as startup_error:
        try:
            cleanup(kubeconfig, remove_state=True)
        except BaseException as cleanup_error:
            startup_message = str(startup_error) or type(startup_error).__name__
            cleanup_message = str(cleanup_error) or type(cleanup_error).__name__
            raise LocalDevError(
                f"local development startup failed: {startup_message}; "
                f"cleanup also failed: {cleanup_message}"
            ) from startup_error
        raise

def state() -> dict[str,Any]:
    if not STATE_FILE.exists(): fail("local development stack is not up")
    try:
        value = json.loads(STATE_FILE.read_text(encoding="utf-8"))
    except (OSError,json.JSONDecodeError) as error: fail("local development state is invalid: "+str(error))
    if not isinstance(value, dict):
        fail("local development state is invalid: expected an object")
    return value

def state_path(info: dict[str, Any], key: str, base: Path) -> Path:
    value = info.get(key)
    if not isinstance(value, str) or not value or Path(value).is_absolute():
        fail(f"local development state has an invalid {key} path")
    relative = Path(value)
    if any(part in ("", ".", "..") for part in relative.parts):
        fail(f"local development state has an invalid {key} path")
    resolved = (ROOT / relative).resolve()
    try:
        resolved.relative_to(base.resolve())
    except ValueError:
        fail(f"local development state {key} path is outside its owned directory")
    return resolved

def status() -> None:
    info=state(); kc=state_path(info, "kubeconfig", STATE_DIR)
    result=kubectl(kc,["get","deploy","-n",info["namespace"],"-o","json"],capture=True,check=False)
    if result.returncode:
        detail=(result.stderr or result.stdout or "").strip()[-2000:]
        fail(f"local development status unavailable (phase={info.get('phase','unknown')}): {detail}")
    workloads=[]
    payload=json.loads(result.stdout)
    workloads=[{"name":item["metadata"]["name"],"ready":item["status"].get("readyReplicas",0),
                "desired":item["spec"].get("replicas",1)} for item in payload.get("items",[])]
    status = {"state":info.get("phase","unknown"),"profile":info.get("profile","default"),
              "cluster":info["cluster"],"workloads":workloads}
    if info.get("externalFixtures"):
        status["externalModel"] = "claude-stream-json-fixture"
        status["externalBuildProvider"] = "nats-oci-copy-fixture"
    print(json.dumps(status,indent=2))

def cleanup(kubeconfig: Path, remove_state: bool = True) -> None:
    info=json.loads(STATE_FILE.read_text()) if STATE_FILE.exists() else None
    if info:
        expected_cluster=f"labweaver-local-{info.get('runId','')}"
        expected_registry=f"labweaver-local-registry-{info.get('runId','')}"
        if info.get("cluster") != expected_cluster or info.get("registry") != expected_registry:
            fail("local development state names are not owned by this entry point")
        run_root = state_path(info, "runRoot", PRIVATE_DIR)
        errors=[]
        for pid in info.get("portForwards",[]):
            if not stop_port_forward(pid):
                process_id = pid.get("pid") if isinstance(pid, dict) else pid
                errors.append(f"port-forward process {process_id} was not verified")
        inspection=run(["docker","inspect","--format",'{{index .Config.Labels "labweaver.local-dev.run-id"}}',info["registry"]],capture=True,check=False)
        registry_owned = False
        if inspection.returncode == 0:
            if inspection.stdout.strip() != info.get("runId"):
                errors.append("refusing to remove a registry without the recorded local-dev ownership label")
            else:
                registry_owned = True
        else:
            # A container that was already removed is a safe idempotent end
            # state.  Treat every other inspect failure as an ownership check
            # failure; in particular, never mistake an unavailable daemon for
            # an absent registry.
            listed=run(["docker","ps","-a","--format","{{.Names}}"],capture=True,check=False)
            if listed.returncode:
                errors.append("cannot verify local-dev registry ownership: " +
                              (listed.stderr or listed.stdout or "docker ps failed").strip()[-1000:])
            elif info["registry"] in (listed.stdout or "").splitlines():
                errors.append("cannot inspect the recorded local-dev registry: " +
                              (inspection.stderr or inspection.stdout or "docker inspect failed").strip()[-1000:])
        cluster=run(["kind","delete","cluster","--name",info["cluster"]],check=False)
        if cluster.returncode:
            if cluster_exists_named(info["cluster"], kubeconfig):
                errors.append("Kind cluster cleanup failed")
        elif cluster_exists_named(info["cluster"], kubeconfig):
            errors.append("Kind cluster remains after cleanup")
        if registry_owned:
            registry=run(["docker","rm","-f",info["registry"]],check=False)
            if registry.returncode:
                errors.append("registry cleanup failed")
        if not errors:
            # The run root contains the generated PKI, service credentials, and
            # kubeconfig used only by this local stack.  Remove it only after
            # every owned external resource has been cleaned up so an
            # incomplete cleanup retains the state needed for a later retry.
            try:
                if run_root.exists():
                    shutil.rmtree(run_root)
                kubeconfig.unlink(missing_ok=True)
                (STATE_DIR / "kind-config.yaml").unlink(missing_ok=True)
            except OSError as error:
                errors.append(f"local development secret cleanup failed: {error}")
        if errors:
            fail("cleanup incomplete; recorded ownership was retained: " + "; ".join(errors))
    if remove_state: STATE_FILE.unlink(missing_ok=True)


def cluster_exists_named(name: str, kubeconfig: Path) -> bool:
    result=run(["kind","get","clusters"],capture=True,check=False)
    if result.returncode:
        detail=(result.stderr or result.stdout or "kind get clusters failed").strip()[-1000:]
        fail("cannot verify local-dev Kind cluster ownership: " + detail)
    return name in (result.stdout or "").split()


def stop_port_forward(record: dict[str, Any]) -> bool:
    if not isinstance(record, dict): return False
    try:
        pid = int(record["pid"])
        expected_create_time = float(record["createTime"])
        expected_cmdline = [str(item) for item in record["cmdline"]]
    except (KeyError, TypeError, ValueError):
        return False
    if pid <= 0 or not expected_cmdline or expected_create_time <= 0: return False
    process_api = require_psutil()
    try:
        process = process_api.Process(pid)
        if not math.isclose(process.create_time(), expected_create_time, rel_tol=0.0, abs_tol=0.01):
            return False
        actual_cmdline = process.cmdline()
        if len(actual_cmdline) != len(expected_cmdline):
            return False
        if Path(actual_cmdline[0]).name.casefold() != Path(expected_cmdline[0]).name.casefold():
            return False
        if actual_cmdline[1:] != expected_cmdline[1:]:
            return False
        process.terminate()
        try:
            process.wait(timeout=5)
        except process_api.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)
        return True
    except process_api.NoSuchProcess:
        return True
    except (process_api.AccessDenied, OSError, ValueError):
        return False

def down() -> None:
    info=state()
    cleanup(state_path(info, "kubeconfig", STATE_DIR))
    print(json.dumps({"status":"down"},indent=2))

def main() -> int:
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command",choices=("up","status","down"))
    parser.add_argument(
        "--external-fixtures",
        action="store_true",
        help="use the explicit local Claude/NATS build fixtures for integration tests",
    )
    args=parser.parse_args()
    if args.external_fixtures and args.command != "up":
        parser.error("--external-fixtures is valid only with the up command")
    try:
        if args.command == "up":
            up(external_fixtures=args.external_fixtures)
        elif args.command == "status":
            status()
        else:
            down()
    except LocalDevError as error:
        print(str(error),file=sys.stderr); return 1
    return 0

if __name__=="__main__": raise SystemExit(main())
