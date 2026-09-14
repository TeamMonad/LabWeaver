#!/usr/bin/env python3
"""Provision the real Harbor and BuildKit services used by local acceptance.

The normal Kind profile owns a plain registry for platform images.  This
module is the small, explicit boundary for a real candidate build: it obtains
the locked Harbor chart, installs a private single-instance Harbor with its
scanner enabled, creates the scoped project credentials, and applies the
rootless BuildKit workload generated from the deployment template.

The caller owns the Kind cluster and its complete cleanup.  All files written
by this module live below the caller's private run directory.
"""

from __future__ import annotations

import base64
from contextlib import contextmanager
from dataclasses import dataclass
import hashlib
import importlib.util
import ipaddress
import json
import os
from pathlib import Path
import secrets
import shutil
import socket
import ssl
import subprocess
import time
from typing import Any, Iterator, Mapping
from urllib.error import HTTPError, URLError
from urllib.parse import quote
from urllib.request import Request, urlopen

import yaml

try:
    import urllib3
except ImportError:  # pragma: no cover - reported when real bootstrap is requested
    urllib3 = None  # type: ignore[assignment]

try:
    from jinja2 import Environment, FileSystemLoader, StrictUndefined
except ImportError:  # pragma: no cover - reported when real bootstrap is requested
    Environment = None  # type: ignore[assignment]
    FileSystemLoader = None  # type: ignore[assignment]
    StrictUndefined = None  # type: ignore[assignment]


ROOT = Path(__file__).resolve().parents[1]
VERSIONS_LOCK = ROOT / "deploy" / "versions.lock.yml"
HARBOR_ROLE_DEFAULTS = ROOT / "deploy" / "ansible" / "roles" / "harbor" / "defaults" / "main.yml"
HARBOR_QUOTA_TEMPLATE = ROOT / "deploy" / "ansible" / "roles" / "harbor" / "templates" / "quota.yml.j2"
BUILDKIT_TEMPLATE = (
    ROOT / "deploy" / "ansible" / "roles" / "platform_buildkit" / "templates" / "workloads.yml.j2"
)
BUILDKIT_MANIFEST = ROOT / "deploy" / "config" / "platform-buildkit-bundle-manifest.json"
BUILDKIT_AUTHORING = ROOT / "tools" / "prepare_platform_buildkit.py"
HARBOR_CHART_BASE_URL = "https://helm.goharbor.io"
HARBOR_HOST = "harbor.lab.lan"
HARBOR_NAMESPACE = "harbor"
HARBOR_PROJECT = "labweaver-system"
BUILDKIT_NAMESPACE = "labweaver-build"
BUILDKIT_ADDRESS = "tcp://buildkit.labweaver-build.svc:1234"
PROJECT_QUOTA_BYTES = 4 * 1024 * 1024 * 1024
HARBOR_NAMESPACE_STORAGE_QUOTA = "12Gi"
BUILDKIT_NETWORK_POLICY_MODE = "kindnet-network-policy-unenforced;cilium-unavailable"


class LocalBuildProviderError(RuntimeError):
    """Stable fail-closed diagnostics for the local real-provider bootstrap."""

    def __init__(self, code: str, detail: str | None = None) -> None:
        self.code = code
        self.detail = detail.strip()[-1000:] if detail else ""
        super().__init__(f"{code}: {self.detail}" if self.detail else code)


@dataclass(frozen=True)
class RealBuildProvider:
    """Endpoints and private files consumed by the existing build executor."""

    registry_host: str
    registry_service_ip: str
    harbor_api: str
    buildkit_address: str
    harbor_ca_file: Path
    builder_username_file: Path
    builder_password_file: Path
    runtime_username_file: Path
    runtime_password_file: Path
    registry_pull_config_file: Path
    buildkit_ca_file: Path
    buildkit_client_certificate_file: Path
    buildkit_client_private_key_file: Path
    chart_archive: Path
    project_storage_quota_bytes: int
    buildkit_network_policy_mode: str


def _read_yaml(path: Path) -> dict[str, Any]:
    try:
        value = yaml.safe_load(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, yaml.YAMLError) as error:
        raise LocalBuildProviderError("LW_LOCAL_BUILD_INPUT_INVALID", path.name) from error
    if not isinstance(value, dict):
        raise LocalBuildProviderError("LW_LOCAL_BUILD_INPUT_INVALID", path.name)
    return value


def _locked_versions() -> dict[str, Any]:
    document = _read_yaml(VERSIONS_LOCK)
    harbor = document.get("harbor")
    foundation = document.get("platform_foundation")
    if not isinstance(harbor, dict) or not isinstance(foundation, dict):
        raise LocalBuildProviderError("LW_LOCAL_BUILD_LOCK_INVALID")
    chart = harbor.get("chart")
    chart_hash = harbor.get("chart_archive_sha256")
    images = harbor.get("images")
    buildkit = foundation.get("buildkit_rootless")
    if (
        not isinstance(chart, str)
        or not isinstance(chart_hash, str)
        or not chart_hash.startswith("sha256:")
        or len(chart_hash) != len("sha256:") + 64
        or any(character not in "0123456789abcdef" for character in chart_hash.removeprefix("sha256:"))
        or not isinstance(images, dict)
        or len(images) != 10
        or any(
            not isinstance(value, str)
            or not value.startswith("sha256:")
            or len(value) != len("sha256:") + 64
            or any(character not in "0123456789abcdef" for character in value.removeprefix("sha256:"))
            for value in images.values()
        )
        or not isinstance(buildkit, str)
        or not buildkit.rsplit("@sha256:", 1)[-1]
        or len(buildkit.rsplit("@sha256:", 1)[-1]) != 64
        or any(character not in "0123456789abcdef" for character in buildkit.rsplit("@sha256:", 1)[-1])
    ):
        raise LocalBuildProviderError("LW_LOCAL_BUILD_LOCK_INVALID")
    return {
        "harbor_chart": chart,
        "harbor_chart_sha256": chart_hash.removeprefix("sha256:"),
        "harbor_images": images,
        "harbor_app": str(harbor.get("app", "")),
        "buildkit_image": buildkit,
    }


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    try:
        with path.open("rb") as handle:
            for block in iter(lambda: handle.read(1024 * 1024), b""):
                digest.update(block)
    except OSError as error:
        raise LocalBuildProviderError("LW_LOCAL_BUILD_CHART_READ_FAILED") from error
    return digest.hexdigest()


def _private_run_directory(path: Path) -> Path:
    resolved = path.resolve()
    if not any(part in {".private", "private"} for part in resolved.parts):
        raise LocalBuildProviderError("LW_LOCAL_BUILD_PRIVATE_PATH_REQUIRED")
    resolved.mkdir(parents=True, mode=0o700, exist_ok=True)
    return resolved


def _harbor_chart_url(version: str) -> str:
    return f"{HARBOR_CHART_BASE_URL}/harbor-{version}.tgz"


def ensure_harbor_chart(work: Path, chart_archive: Path | None = None) -> Path:
    """Return a hash-verified chart, downloading the locked official archive."""

    work = _private_run_directory(work)
    lock = _locked_versions()
    expected = lock["harbor_chart_sha256"]
    if chart_archive is not None:
        try:
            candidate = chart_archive.resolve(strict=True)
        except OSError as error:
            raise LocalBuildProviderError("LW_LOCAL_BUILD_CHART_MISSING") from error
        if not candidate.is_file() or _sha256(candidate) != expected:
            raise LocalBuildProviderError("LW_LOCAL_BUILD_CHART_HASH_MISMATCH")
        return candidate

    target = work / f"harbor-{lock['harbor_chart']}.tgz"
    partial = target.with_suffix(target.suffix + ".part")
    try:
        request = Request(
            _harbor_chart_url(lock["harbor_chart"]),
            headers={"User-Agent": "LabWeaver-local-real-build/1"},
        )
        with urlopen(request, timeout=90) as response, partial.open("wb") as handle:
            while True:
                block = response.read(1024 * 1024)
                if not block:
                    break
                handle.write(block)
        if _sha256(partial) != expected:
            raise LocalBuildProviderError("LW_LOCAL_BUILD_CHART_HASH_MISMATCH")
        partial.replace(target)
    except LocalBuildProviderError:
        partial.unlink(missing_ok=True)
        raise
    except (OSError, HTTPError, URLError, TimeoutError) as error:
        partial.unlink(missing_ok=True)
        raise LocalBuildProviderError("LW_LOCAL_BUILD_CHART_DOWNLOAD_FAILED", type(error).__name__) from error
    return target


def inspect_real_build_prerequisites(work: Path, chart_archive: Path | None = None) -> Path:
    """Check tools and obtain the chart before any Kubernetes side effect."""

    for name in ("docker", "kind", "kubectl", "helm", "openssl", "ssh-keygen"):
        if shutil.which(name) is None:
            raise LocalBuildProviderError("LW_LOCAL_BUILD_TOOL_MISSING", name)
    return ensure_harbor_chart(work, chart_archive)


def _run(
    argv: list[str],
    *,
    code: str,
    timeout: float = 120,
    input_text: str | None = None,
) -> str:
    try:
        result = subprocess.run(
            argv,
            check=False,
            input=input_text,
            text=True,
            capture_output=True,
            timeout=timeout,
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise LocalBuildProviderError(code, type(error).__name__) from error
    if result.returncode != 0:
        detail = (result.stderr or result.stdout or "command failed").strip()
        raise LocalBuildProviderError(code, detail)
    return result.stdout


def _kubectl(kubeconfig: Path, args: list[str], *, timeout: float = 120, input_text: str | None = None) -> str:
    return _run(
        ["kubectl", "--kubeconfig", str(kubeconfig), *args],
        code="LW_LOCAL_BUILD_KUBECTL_FAILED",
        timeout=timeout,
        input_text=input_text,
    )


def _kubectl_json(kubeconfig: Path, args: list[str]) -> dict[str, Any]:
    try:
        value = json.loads(_kubectl(kubeconfig, [*args, "-o", "json"]))
    except (json.JSONDecodeError, LocalBuildProviderError) as error:
        if isinstance(error, LocalBuildProviderError):
            raise
        raise LocalBuildProviderError("LW_LOCAL_BUILD_KUBERNETES_OUTPUT_INVALID") from error
    if not isinstance(value, dict):
        raise LocalBuildProviderError("LW_LOCAL_BUILD_KUBERNETES_OUTPUT_INVALID")
    return value


def _apply(kubeconfig: Path, objects: list[dict[str, Any]]) -> None:
    payload = yaml.safe_dump_all(objects, sort_keys=False)
    _kubectl(kubeconfig, ["apply", "--filename", "-"], input_text=payload)


def _service_ip(kubeconfig: Path, namespace: str, name: str) -> str:
    service = _kubectl_json(kubeconfig, ["get", "service", name, "--namespace", namespace])
    ip = service.get("spec", {}).get("clusterIP")
    if not isinstance(ip, str) or not ip or ip == "None":
        raise LocalBuildProviderError("LW_LOCAL_BUILD_SERVICE_IP_MISSING", f"{namespace}/{name}")
    return ip


def _ensure_namespace(kubeconfig: Path, name: str, run_id: str, *, privileged: bool = False) -> None:
    labels = {
        "app.kubernetes.io/part-of": "labweaver-infrastructure",
        "labweaver.io/infrastructure": "true",
        "labweaver.local-dev.run-id": run_id,
    }
    if privileged:
        labels.update(
            {
                "labweaver.io/security-exception": "rootless-buildkit-no-process-sandbox",
                "pod-security.kubernetes.io/enforce": "privileged",
                "pod-security.kubernetes.io/audit": "restricted",
                "pod-security.kubernetes.io/warn": "restricted",
            }
        )
    _apply(
        kubeconfig,
        [
            {
                "apiVersion": "v1",
                "kind": "Namespace",
                "metadata": {"name": name, "labels": labels},
            }
        ],
    )


def _write_private(path: Path, payload: bytes | str) -> None:
    path.parent.mkdir(parents=True, mode=0o700, exist_ok=True)
    data = payload.encode() if isinstance(payload, str) else payload
    try:
        descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    except OSError as error:
        raise LocalBuildProviderError("LW_LOCAL_BUILD_PRIVATE_WRITE_FAILED", path.name) from error
    try:
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(data)
            handle.flush()
            os.fsync(handle.fileno())
    except OSError as error:
        path.unlink(missing_ok=True)
        raise LocalBuildProviderError("LW_LOCAL_BUILD_PRIVATE_WRITE_FAILED", path.name) from error


def _harbor_values(lock: Mapping[str, Any], passwords: Mapping[str, str]) -> dict[str, Any]:
    defaults = _read_yaml(HARBOR_ROLE_DEFAULTS)
    resources = defaults.get("harbor_component_resources")
    if not isinstance(resources, dict):
        raise LocalBuildProviderError("LW_LOCAL_BUILD_HARBOR_DEFAULTS_INVALID")
    app_version = lock["harbor_app"]
    images = lock["harbor_images"]

    def component(name: str, image_key: str) -> dict[str, Any]:
        resource = resources.get(name)
        digest = images.get(image_key)
        if not isinstance(resource, dict) or not isinstance(digest, str):
            raise LocalBuildProviderError("LW_LOCAL_BUILD_HARBOR_DEFAULTS_INVALID", name)
        return {"image": {"tag": f"{app_version}@{digest}"}, "resources": resource}

    database_permissions = resources.get("database_permissions")
    if not isinstance(database_permissions, dict):
        raise LocalBuildProviderError("LW_LOCAL_BUILD_HARBOR_DEFAULTS_INVALID", "database_permissions")

    return {
        "externalURL": f"https://{HARBOR_HOST}",
        "expose": {
            "type": "clusterIP",
            "tls": {
                "enabled": True,
                "certSource": "auto",
                "auto": {"commonName": HARBOR_HOST},
            },
        },
        "harborAdminPassword": passwords["admin"],
        "persistence": {
            "persistentVolumeClaim": {
                "registry": {"storageClass": "standard", "size": "4Gi"},
                "trivy": {"storageClass": "standard", "size": "3Gi"},
                "database": {"storageClass": "standard", "size": "1Gi"},
                "redis": {"storageClass": "standard", "size": "1Gi"},
            }
        },
        "nginx": component("nginx", "nginx"),
        "portal": component("portal", "portal"),
        "core": component("core", "core"),
        "jobservice": component("jobservice", "jobservice"),
        "registry": {
            "registry": component("registry", "registry"),
            "controller": component("registryctl", "registryctl"),
        },
        "trivy": {"enabled": True, **component("trivy", "trivy")},
        "database": {
            "type": "internal",
            "internal": {
                "password": passwords["database"],
                **component("database", "database"),
                "initContainer": {"permissions": {"resources": database_permissions}},
            },
        },
        "redis": {
            "type": "internal",
            "internal": {"password": passwords["redis"], **component("redis", "redis")},
        },
        "exporter": component("exporter", "exporter"),
    }


def _render_harbor_quota() -> dict[str, Any]:
    if Environment is None or FileSystemLoader is None or StrictUndefined is None:
        raise LocalBuildProviderError("LW_LOCAL_BUILD_JINJA_MISSING")
    environment = Environment(
        loader=FileSystemLoader(str(HARBOR_QUOTA_TEMPLATE.parent)),
        undefined=StrictUndefined,
        keep_trailing_newline=True,
    )
    rendered = environment.get_template(HARBOR_QUOTA_TEMPLATE.name).render(
        harbor_namespace=HARBOR_NAMESPACE,
        harbor_resource_quota_storage=HARBOR_NAMESPACE_STORAGE_QUOTA,
    )
    # quota.yml.j2 uses fixed production values; the local chart PVC sizes are
    # the effective disk bound. Parse it here to retain the role template.
    try:
        value = yaml.safe_load(rendered)
    except yaml.YAMLError as error:
        raise LocalBuildProviderError("LW_LOCAL_BUILD_QUOTA_INVALID") from error
    if not isinstance(value, dict):
        raise LocalBuildProviderError("LW_LOCAL_BUILD_QUOTA_INVALID")
    hard = value.get("spec", {}).get("hard")
    if not isinstance(hard, dict):
        raise LocalBuildProviderError("LW_LOCAL_BUILD_QUOTA_INVALID")
    # The shared role template currently carries the production ceiling.  The
    # local Harbor PVCs are intentionally smaller and must be bounded by the
    # same run-owned namespace quota.
    hard["requests.storage"] = HARBOR_NAMESPACE_STORAGE_QUOTA
    return value


def _buildkit_authoring_module() -> Any:
    spec = importlib.util.spec_from_file_location("prepare_platform_buildkit", BUILDKIT_AUTHORING)
    if spec is None or spec.loader is None:
        raise LocalBuildProviderError("LW_LOCAL_BUILD_AUTHORING_UNAVAILABLE")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _buildkit_objects(
    *,
    buildkit_output: Path,
    lock: Mapping[str, Any],
    dns_nameserver: str,
    harbor_service_ip: str,
    storage: str = "4Gi",
) -> list[dict[str, Any]]:
    if Environment is None or FileSystemLoader is None or StrictUndefined is None:
        raise LocalBuildProviderError("LW_LOCAL_BUILD_JINJA_MISSING")
    for value, code in (
        (dns_nameserver, "LW_LOCAL_BUILD_BUILDKIT_DNS_INVALID"),
        (harbor_service_ip, "LW_LOCAL_BUILD_BUILDKIT_REGISTRY_IP_INVALID"),
    ):
        try:
            address = ipaddress.ip_address(value)
        except ValueError as error:
            raise LocalBuildProviderError(code) from error
        if address.version != 4:
            raise LocalBuildProviderError(code)
    try:
        manifest = json.loads(BUILDKIT_MANIFEST.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise LocalBuildProviderError("LW_LOCAL_BUILD_MANIFEST_INVALID") from error
    if (
        not isinstance(manifest, dict)
        or manifest.get("apiVersion") != "deploy.labweaver.io/platform-bundle-manifest/v1"
        or manifest.get("namespace") != BUILDKIT_NAMESPACE
    ):
        raise LocalBuildProviderError("LW_LOCAL_BUILD_MANIFEST_INVALID")
    config_maps = manifest.get("configMaps")
    secrets = manifest.get("secrets")
    config_keys = config_maps.get("buildkit-config") if isinstance(config_maps, dict) else None
    secret_keys = secrets.get("buildkit-server-secrets") if isinstance(secrets, dict) else None
    if (
        not isinstance(config_keys, list)
        or not config_keys
        or any(not isinstance(name, str) or not name for name in config_keys)
        or not isinstance(secret_keys, list)
        or not secret_keys
        or any(not isinstance(name, str) or not name for name in secret_keys)
    ):
        raise LocalBuildProviderError("LW_LOCAL_BUILD_MANIFEST_INVALID")
    config_dir = buildkit_output / "render-input" / "configmaps" / "buildkit-config"
    secret_dir = buildkit_output / "render-input" / "secrets" / "buildkit-server-secrets"
    try:
        config_data = {name: (config_dir / name).read_text(encoding="utf-8") for name in config_keys}
        secret_data = {
            name: base64.b64encode((secret_dir / name).read_bytes()).decode() for name in secret_keys
        }
    except (OSError, UnicodeError) as error:
        raise LocalBuildProviderError("LW_LOCAL_BUILD_BUILDKIT_INPUT_MISSING") from error
    config_object = {
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": {
            "name": "buildkit-config",
            "namespace": BUILDKIT_NAMESPACE,
            "labels": {"app.kubernetes.io/part-of": "labweaver-infrastructure"},
        },
        "data": config_data,
    }
    secret_object = {
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": "buildkit-server-secrets",
            "namespace": BUILDKIT_NAMESPACE,
            "labels": {"app.kubernetes.io/part-of": "labweaver-infrastructure"},
        },
        "type": "Opaque",
        "data": secret_data,
    }
    bundle_sha256 = hashlib.sha256(
        yaml.safe_dump_all([config_object, secret_object], sort_keys=False).encode("utf-8")
    ).hexdigest()
    environment = Environment(
        loader=FileSystemLoader(str(BUILDKIT_TEMPLATE.parent)),
        undefined=StrictUndefined,
        keep_trailing_newline=True,
    )
    rendered = environment.get_template(BUILDKIT_TEMPLATE.name).render(
        platform_buildkit_namespace=BUILDKIT_NAMESPACE,
        platform_buildkit_lock={"platform_foundation": {"buildkit_rootless": lock["buildkit_image"]}},
        platform_buildkit_bundle_sha256=bundle_sha256,
        platform_buildkit_storage_class="standard",
        platform_buildkit_storage=storage,
        platform_buildkit_registry_cidr=f"{harbor_service_ip}/32",
        platform_buildkit_dns_nameserver=dns_nameserver,
        platform_buildkit_harbor_service_ip=harbor_service_ip,
        platform_buildkit_admin_cidr="",
    )
    try:
        objects = [value for value in yaml.safe_load_all(rendered) if isinstance(value, dict)]
    except (OSError, UnicodeError, yaml.YAMLError) as error:
        raise LocalBuildProviderError("LW_LOCAL_BUILD_BUILDKIT_RENDER_INVALID") from error
    # Kind's default kindnet CNI has no Cilium CRD and does not enforce
    # Kubernetes NetworkPolicy. Keep the role's policy objects as declarative
    # intent, but expose the unverified isolation boundary to the caller.
    objects = [value for value in objects if value.get("kind") != "CiliumNetworkPolicy"]
    objects.insert(0, config_object)
    objects.insert(1, secret_object)
    return objects


def _decode_secret_file(kubeconfig: Path, namespace: str, name: str, key: str, output: Path) -> None:
    secret = _kubectl_json(kubeconfig, ["get", "secret", name, "--namespace", namespace])
    data = secret.get("data")
    if not isinstance(data, dict):
        raise LocalBuildProviderError("LW_LOCAL_BUILD_HARBOR_CA_MISSING", key)
    encoded = data.get(key)
    if not isinstance(encoded, str):
        raise LocalBuildProviderError("LW_LOCAL_BUILD_HARBOR_CA_MISSING", key)
    try:
        payload = base64.b64decode(encoded, validate=True)
    except (ValueError, TypeError) as error:
        raise LocalBuildProviderError("LW_LOCAL_BUILD_HARBOR_CA_INVALID") from error
    _write_private(output, payload)


def _free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def _wait_port(process: subprocess.Popen[str], port: int, timeout: float = 60) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise LocalBuildProviderError("LW_LOCAL_BUILD_PORT_FORWARD_FAILED")
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=1):
                return
        except OSError:
            time.sleep(0.25)
    raise LocalBuildProviderError("LW_LOCAL_BUILD_PORT_FORWARD_TIMEOUT")


@contextmanager
def _harbor_port_forward(kubeconfig: Path, work: Path) -> Iterator[int]:
    port = _free_port()
    log = work / "harbor-port-forward.log"
    handle = None
    try:
        handle = log.open("w", encoding="utf-8")
        process = subprocess.Popen(
            [
                "kubectl",
                "--kubeconfig",
                str(kubeconfig),
                "--namespace",
                HARBOR_NAMESPACE,
                "port-forward",
                "service/harbor",
                f"{port}:443",
            ],
            stdin=subprocess.DEVNULL,
            stdout=handle,
            stderr=subprocess.STDOUT,
            text=True,
        )
    except OSError as error:
        if handle is not None:
            handle.close()
        raise LocalBuildProviderError("LW_LOCAL_BUILD_PORT_FORWARD_FAILED") from error
    try:
        _wait_port(process, port)
        yield port
    finally:
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=10)
        handle.close()


def _harbor_request(
    port: int,
    ca_file: Path,
    username: str,
    password: str,
    method: str,
    path: str,
    body: dict[str, Any] | None = None,
) -> tuple[int, bytes]:
    if urllib3 is None:
        raise LocalBuildProviderError("LW_LOCAL_BUILD_HTTP_CLIENT_MISSING")
    try:
        ca_path = ca_file.resolve(strict=True)
    except OSError as error:
        raise LocalBuildProviderError("LW_LOCAL_BUILD_HARBOR_CA_INVALID") from error
    if not ca_path.is_file():
        raise LocalBuildProviderError("LW_LOCAL_BUILD_HARBOR_CA_INVALID")
    payload = json.dumps(body, separators=(",", ":")).encode() if body is not None else None
    headers = urllib3.util.make_headers(basic_auth=f"{username}:{password}")
    headers.update({"Accept": "application/json", "Host": HARBOR_HOST})
    if payload is not None:
        headers["Content-Type"] = "application/json"
    pool = urllib3.HTTPSConnectionPool(
        "127.0.0.1",
        port=port,
        timeout=urllib3.Timeout(connect=10, read=30),
        maxsize=1,
        block=True,
        retries=False,
        cert_reqs=ssl.CERT_REQUIRED,
        ca_certs=str(ca_path),
        assert_hostname=HARBOR_HOST,
        server_hostname=HARBOR_HOST,
    )
    try:
        response = pool.request(
            method,
            path,
            body=payload,
            headers=headers,
            retries=False,
            redirect=False,
            preload_content=True,
        )
        content = response.data
        if len(content) > 2 * 1024 * 1024:
            raise LocalBuildProviderError("LW_LOCAL_BUILD_HARBOR_API_OUTPUT_TOO_LARGE")
        return response.status, content
    except LocalBuildProviderError:
        raise
    except urllib3.exceptions.MaxRetryError as error:
        reason = error.reason
        if isinstance(reason, (urllib3.exceptions.SSLError, ssl.SSLError, ssl.CertificateError)):
            raise LocalBuildProviderError("LW_LOCAL_BUILD_HARBOR_TLS_FAILED", str(reason)) from error
        raise LocalBuildProviderError("LW_LOCAL_BUILD_HARBOR_API_TRANSPORT_RETRYABLE", str(reason)) from error
    except (urllib3.exceptions.SSLError, ssl.SSLError, ssl.CertificateError) as error:
        raise LocalBuildProviderError("LW_LOCAL_BUILD_HARBOR_TLS_FAILED", str(error)) from error
    except (
        urllib3.exceptions.ConnectTimeoutError,
        urllib3.exceptions.NewConnectionError,
        urllib3.exceptions.ProtocolError,
        urllib3.exceptions.ReadTimeoutError,
        urllib3.exceptions.TimeoutError,
        OSError,
    ) as error:
        raise LocalBuildProviderError("LW_LOCAL_BUILD_HARBOR_API_TRANSPORT_RETRYABLE", str(error)) from error
    finally:
        pool.close()


def _harbor_json(
    port: int,
    ca_file: Path,
    username: str,
    password: str,
    method: str,
    path: str,
    body: dict[str, Any] | None = None,
    *,
    expected: set[int] | None = None,
) -> Any:
    status, content = _harbor_request(port, ca_file, username, password, method, path, body)
    if expected is None:
        expected = {200, 201, 202, 204}
    if status not in expected:
        raise LocalBuildProviderError("LW_LOCAL_BUILD_HARBOR_API_REJECTED", f"HTTP {status}")
    if status == 204 or not content:
        return None
    try:
        return json.loads(content)
    except json.JSONDecodeError as error:
        raise LocalBuildProviderError("LW_LOCAL_BUILD_HARBOR_API_OUTPUT_INVALID") from error


def _wait_harbor_api(port: int, ca_file: Path, password: str) -> None:
    deadline = time.monotonic() + 120
    last_detail = "no response"
    while time.monotonic() < deadline:
        try:
            status, _ = _harbor_request(port, ca_file, "admin", password, "GET", "/api/v2.0/systeminfo")
            if status == 200:
                return
            if status in {401, 403}:
                raise LocalBuildProviderError("LW_LOCAL_BUILD_HARBOR_API_AUTH_FAILED", f"HTTP {status}")
            if status not in {404, 408, 425, 429, 500, 502, 503, 504}:
                raise LocalBuildProviderError("LW_LOCAL_BUILD_HARBOR_API_REJECTED", f"HTTP {status}")
            last_detail = f"HTTP {status}"
        except LocalBuildProviderError as error:
            if error.code != "LW_LOCAL_BUILD_HARBOR_API_TRANSPORT_RETRYABLE":
                raise
            last_detail = str(error)
        time.sleep(1)
    raise LocalBuildProviderError("LW_LOCAL_BUILD_HARBOR_API_TIMEOUT", last_detail)


def _create_project_and_robots(
    *,
    port: int,
    ca_file: Path,
    admin_password: str,
) -> tuple[str, str, str, str, str]:
    projects = _harbor_json(
        port,
        ca_file,
        "admin",
        admin_password,
        "GET",
        f"/api/v2.0/projects?name={quote(HARBOR_PROJECT, safe='')}&page=1&page_size=10",
        expected={200},
    )
    if not isinstance(projects, list):
        raise LocalBuildProviderError("LW_LOCAL_BUILD_HARBOR_API_OUTPUT_INVALID")
    if len(projects) > 1:
        raise LocalBuildProviderError("LW_LOCAL_BUILD_HARBOR_PROJECT_INVALID")
    if projects:
        project = projects[0]
        if not isinstance(project, dict):
            raise LocalBuildProviderError("LW_LOCAL_BUILD_HARBOR_PROJECT_INVALID")
        project_id = project.get("project_id")
        metadata = project.get("metadata", {})
        if not isinstance(metadata, dict):
            raise LocalBuildProviderError("LW_LOCAL_BUILD_HARBOR_PROJECT_INVALID")
        if metadata.get("public") in {True, "true"} or project.get("storage_limit") not in {
            PROJECT_QUOTA_BYTES,
            str(PROJECT_QUOTA_BYTES),
        }:
            raise LocalBuildProviderError("LW_LOCAL_BUILD_HARBOR_PROJECT_INVALID")
    else:
        _harbor_json(
            port,
            ca_file,
            "admin",
            admin_password,
            "POST",
            "/api/v2.0/projects",
            {
                "project_name": HARBOR_PROJECT,
                "public": False,
                "storage_limit": PROJECT_QUOTA_BYTES,
                "metadata": {"public": "false", "auto_scan": "true"},
            },
            expected={201},
        )
        project = _harbor_json(
            port,
            ca_file,
            "admin",
            admin_password,
            "GET",
            f"/api/v2.0/projects?name={quote(HARBOR_PROJECT, safe='')}&page=1&page_size=10",
            expected={200},
        )
        if not isinstance(project, list) or len(project) != 1 or not isinstance(project[0], dict):
            raise LocalBuildProviderError("LW_LOCAL_BUILD_HARBOR_PROJECT_INVALID")
        project_id = project[0].get("project_id")
    if not isinstance(project_id, int):
        raise LocalBuildProviderError("LW_LOCAL_BUILD_HARBOR_PROJECT_INVALID")

    def create_robot(name: str, actions: list[dict[str, str]]) -> tuple[str, str]:
        response = _harbor_json(
            port,
            ca_file,
            "admin",
            admin_password,
            "POST",
            "/api/v2.0/robots",
            {
                "name": name,
                "description": "run-owned LabWeaver local acceptance credential",
                "duration": -1,
                "level": "project",
                "permissions": [
                    {"kind": "project", "namespace": HARBOR_PROJECT, "access": actions}
                ],
            },
            expected={201},
        )
        if not isinstance(response, dict):
            raise LocalBuildProviderError("LW_LOCAL_BUILD_HARBOR_ROBOT_INVALID")
        token = response.get("secret")
        robot_name = response.get("name")
        if not isinstance(token, str) or not token or not isinstance(robot_name, str) or not robot_name:
            raise LocalBuildProviderError("LW_LOCAL_BUILD_HARBOR_ROBOT_INVALID")
        return robot_name, token

    builder_name, builder_token = create_robot(
        "platform-build-executor",
        [
            {"resource": "repository", "action": "push"},
            {"resource": "repository", "action": "pull"},
            {"resource": "artifact", "action": "read"},
            {"resource": "tag", "action": "delete"},
        ],
    )
    runtime_name, runtime_token = create_robot(
        "runtime-puller",
        [
            {"resource": "repository", "action": "pull"},
            {"resource": "artifact", "action": "read"},
        ],
    )
    return str(project_id), builder_name, builder_token, runtime_name, runtime_token


def bootstrap_real_build_provider(
    kubeconfig: Path,
    work: Path,
    run_id: str,
    *,
    chart_archive: Path | None = None,
    openssl: Path | None = None,
) -> RealBuildProvider:
    """Install actual local Harbor and BuildKit and return executor inputs."""

    work = _private_run_directory(work)
    chart = ensure_harbor_chart(work, chart_archive)
    lock = _locked_versions()
    for name in ("helm", "kubectl"):
        if shutil.which(name) is None:
            raise LocalBuildProviderError("LW_LOCAL_BUILD_TOOL_MISSING", name)
    openssl_candidate = openssl
    if openssl_candidate is None:
        discovered_openssl = shutil.which("openssl")
        if discovered_openssl is None:
            raise LocalBuildProviderError("LW_LOCAL_BUILD_TOOL_MISSING", "openssl")
        openssl_candidate = Path(discovered_openssl)
    try:
        openssl_path = openssl_candidate.resolve(strict=True)
    except OSError as error:
        raise LocalBuildProviderError("LW_LOCAL_BUILD_TOOL_MISSING", "openssl") from error
    if not openssl_path.is_file():
        raise LocalBuildProviderError("LW_LOCAL_BUILD_TOOL_MISSING", "openssl")
    passwords = {
        "admin": secrets.token_urlsafe(32),
        "database": secrets.token_urlsafe(32),
        "redis": secrets.token_urlsafe(32),
    }
    values_file = work / "harbor-values.yaml"
    _write_private(values_file, yaml.safe_dump(_harbor_values(lock, passwords), sort_keys=False))
    _ensure_namespace(kubeconfig, HARBOR_NAMESPACE, run_id)
    _run(
        [
            "helm",
            "upgrade",
            "--install",
            "harbor",
            str(chart),
            "--namespace",
            HARBOR_NAMESPACE,
            "--create-namespace",
            "--kubeconfig",
            str(kubeconfig),
            "--values",
            str(values_file),
            "--wait",
            "--timeout",
            "15m",
        ],
        code="LW_LOCAL_BUILD_HARBOR_INSTALL_FAILED",
        timeout=960,
    )
    _run(
        [
            "kubectl",
            "--kubeconfig",
            str(kubeconfig),
            "label",
            "namespace",
            HARBOR_NAMESPACE,
            "app.kubernetes.io/part-of=labweaver-infrastructure",
            f"labweaver.local-dev.run-id={run_id}",
            "--overwrite",
        ],
        code="LW_LOCAL_BUILD_KUBECTL_FAILED",
    )
    _apply(kubeconfig, [_render_harbor_quota()])
    harbor_service_ip = _service_ip(kubeconfig, HARBOR_NAMESPACE, "harbor")
    harbor_ca = work / "harbor-ca.crt"
    _decode_secret_file(kubeconfig, HARBOR_NAMESPACE, "harbor-nginx", "ca.crt", harbor_ca)
    with _harbor_port_forward(kubeconfig, work) as port:
        _wait_harbor_api(port, harbor_ca, passwords["admin"])
        _, builder_name, builder_password, runtime_name, runtime_password = _create_project_and_robots(
            port=port,
            ca_file=harbor_ca,
            admin_password=passwords["admin"],
        )
    builder_user_file = work / "harbor-builder-username"
    builder_password_file = work / "harbor-builder-password"
    runtime_user_file = work / "harbor-runtime-username"
    runtime_password_file = work / "harbor-runtime-password"
    _write_private(builder_user_file, builder_name + "\n")
    _write_private(builder_password_file, builder_password + "\n")
    _write_private(runtime_user_file, runtime_name + "\n")
    _write_private(runtime_password_file, runtime_password + "\n")
    auth = base64.b64encode(f"{runtime_name}:{runtime_password}".encode()).decode()
    pull_config = work / "registry-pull-config.json"
    _write_private(pull_config, json.dumps({"auths": {HARBOR_HOST: {"auth": auth}}}, indent=2) + "\n")

    buildkit_output = work / "buildkit"
    try:
        authoring = _buildkit_authoring_module()
        authoring.prepare(
            buildkit_output,
            openssl_path,
            365,
            HARBOR_HOST,
            _service_ip(kubeconfig, "kube-system", "kube-dns"),
            harbor_ca,
        )
    except Exception as error:
        if isinstance(error, LocalBuildProviderError):
            raise
        raise LocalBuildProviderError("LW_LOCAL_BUILD_BUILDKIT_AUTHORING_FAILED", type(error).__name__) from error
    _ensure_namespace(kubeconfig, BUILDKIT_NAMESPACE, run_id, privileged=True)
    objects = _buildkit_objects(
        buildkit_output=buildkit_output,
        lock=lock,
        dns_nameserver=_service_ip(kubeconfig, "kube-system", "kube-dns"),
        harbor_service_ip=harbor_service_ip,
    )
    _apply(kubeconfig, objects)
    _run(
        [
            "kubectl",
            "--kubeconfig",
            str(kubeconfig),
            "rollout",
            "status",
            "deployment/buildkit",
            "--namespace",
            BUILDKIT_NAMESPACE,
            "--timeout=10m",
        ],
        code="LW_LOCAL_BUILD_BUILDKIT_NOT_READY",
        timeout=660,
    )
    client = buildkit_output / "build-executor-client"
    return RealBuildProvider(
        registry_host=HARBOR_HOST,
        registry_service_ip=harbor_service_ip,
        harbor_api=f"https://{HARBOR_HOST}/",
        buildkit_address=BUILDKIT_ADDRESS,
        harbor_ca_file=harbor_ca,
        builder_username_file=builder_user_file,
        builder_password_file=builder_password_file,
        runtime_username_file=runtime_user_file,
        runtime_password_file=runtime_password_file,
        registry_pull_config_file=pull_config,
        buildkit_ca_file=client / "ca.crt",
        buildkit_client_certificate_file=client / "tls.crt",
        buildkit_client_private_key_file=client / "tls.key",
        chart_archive=chart,
        project_storage_quota_bytes=PROJECT_QUOTA_BYTES,
        buildkit_network_policy_mode=BUILDKIT_NETWORK_POLICY_MODE,
    )
