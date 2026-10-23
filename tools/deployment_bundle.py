#!/usr/bin/env python3
"""Validate, derive and use one portable application configuration with age."""

from __future__ import annotations

import argparse
from contextlib import contextmanager
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import signal
import stat
import subprocess
import sys
import tarfile
import tempfile
from typing import Any, Iterator
from urllib.parse import urlsplit

import yaml

try:
    import render_platform_bundle as objects
    import render_resource_bundle as resource_objects
except ModuleNotFoundError as error:
    if error.name != "render_platform_bundle":
        raise
    from tools import render_platform_bundle as objects
    from tools import render_resource_bundle as resource_objects

API_VERSION = "deploy.labweaver.io/bundle/v1"
MODEL_FILE = "config/bundle.yaml"
MAX_FILES = 512
MAX_TOTAL_BYTES = 64 * 1024 * 1024
MAX_ARCHIVE_BYTES = MAX_TOTAL_BYTES + MAX_FILES * 10240
DEFAULT_MANIFEST = (
    Path(__file__).resolve().parents[1] / "deploy/config/platform-bundle-manifest.json"
)
WORKLOADS_FILE = (
    Path(__file__).resolve().parents[1] / "deploy/helm/labweaver/values.yaml"
)
RESOURCE_MANIFEST = (
    Path(__file__).resolve().parents[1] / "deploy/config/resource-bundle-manifest.json"
)
ENVIRONMENT_EXECUTOR_SHARED = (
    "LABWEAVER_SERVICE_OIDC_ISSUER",
    "LABWEAVER_SERVICE_OIDC_CA",
    "LABWEAVER_SERVICE_CLIENT_ID",
    "LABWEAVER_SERVICE_CLIENT_SECRET_FILE",
    "LABWEAVER_SERVICE_TOKEN_REFRESH_SKEW_SECONDS",
    "LABWEAVER_RESOURCE_USAGE_BASE_URI",
    "LABWEAVER_RESOURCE_USAGE_CA_PATH",
    "LABWEAVER_RESOURCE_USAGE_AUDIENCE",
    "LABWEAVER_RESOURCE_USAGE_SCOPES",
    "LABWEAVER_RESOURCE_USAGE_TIMEOUT_MILLISECONDS",
)
RESOURCE_RESERVED = {
    "resource_application_namespace",
    "resource_application_release_name",
    "resource_application_configuration_bundle",
    "resource_application_configuration_manifest",
    "resource_application_values_file",
    "resource_application_versions_lock_file",
    "resource_application_package_manifest",
    "resource_application_access_seed_file",
    "resource_application_vars_file",
    "resource_application_reviewed_vars",
    "resource_application_workloads",
}
RESERVED_VARIABLES = {
    "platform_application_namespace",
    "platform_application_release_name",
    "platform_application_configuration_bundle",
    "platform_application_configuration_manifest",
    "platform_application_values_files",
    "platform_application_versions_lock_file",
    "platform_application_package_manifest",
    "platform_application_lock",
    "platform_application_vars_file",
    "platform_application_reviewed_vars",
}
SECRET_FIELD = re.compile(
    r"(?:password|token|private_key|client_secret|secret_key|api_key|auth_token)$", re.I
)
PRIVATE_MARKERS = (
    b"PRIVATE KEY-----",
    b"AGE-SECRET-KEY-",
    b"BEGIN NATS USER NKEY SEED",
)


class BundleError(Exception):
    """A diagnostic that contains no input data or child-process output."""


class StrictLoader(yaml.SafeLoader):
    """Reject duplicate keys instead of silently replacing configuration."""


def _unique_mapping(loader: StrictLoader, node: yaml.MappingNode) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key_node, value_node in node.value:
        key = loader.construct_object(key_node)
        if not isinstance(key, str) or key in result:
            raise BundleError("LW_BUNDLE_DOCUMENT_INVALID")
        result[key] = loader.construct_object(value_node)
    return result


def _agent_control_inputs(
    application: dict[str, Any],
    inputs: dict[str, Any],
    workloads: dict[str, Any],
    overrides: dict[str, Any],
) -> None:
    """Reuse the existing Control connection while retaining Agent's own identity."""
    if not overrides.get("agent-service", {}).get(
        "enabled", workloads["agent-service"]["enabled"]
    ):
        return
    environment = inputs["environment-service"]["env"]
    control = inputs["control-service"]["env"]
    agent = inputs["agent-service"]
    connection = {
        key: environment.get(key)
        for key in (
            "LABWEAVER_CONTROL_SERVICE_BASE_URI",
            "LABWEAVER_CONTROL_SERVICE_CA_PATH",
            "LABWEAVER_CONTROL_SERVICE_AUDIENCE",
            "LABWEAVER_CONTROL_SERVICE_TIMEOUT_MILLISECONDS",
        )
    }
    if not all(isinstance(value, str) for value in connection.values()):
        raise BundleError("LW_BUNDLE_AGENT_CONTROL_INVALID")
    try:
        origin = urlsplit(connection["LABWEAVER_CONTROL_SERVICE_BASE_URI"])
        timeout = int(connection["LABWEAVER_CONTROL_SERVICE_TIMEOUT_MILLISECONDS"])
        valid_port = origin.port
    except ValueError as error:
        raise BundleError("LW_BUNDLE_AGENT_CONTROL_INVALID") from error
    if (
        origin.scheme != "https"
        or not origin.hostname
        or origin.username is not None
        or origin.password is not None
        or origin.path not in {"", "/"}
        or origin.query
        or origin.fragment
        or valid_port == 0
        or not re.fullmatch(
            r"[0-9]+", connection["LABWEAVER_CONTROL_SERVICE_TIMEOUT_MILLISECONDS"]
        )
        or not 1 <= timeout <= 5000
        or connection["LABWEAVER_CONTROL_SERVICE_AUDIENCE"]
        != control.get("LABWEAVER_SERVICE_AUDIENCE")
        or connection["LABWEAVER_CONTROL_SERVICE_CA_PATH"]
        != "/etc/labweaver/secrets/mtls-ca.pem"
    ):
        raise BundleError("LW_BUNDLE_AGENT_CONTROL_INVALID")
    if (
        "control.agent_run.read"
        not in {
            s.strip()
            for s in agent["env"].get("LABWEAVER_SERVICE_SCOPES", "").split(",")
        }
        or agent["env"].get("LABWEAVER_SERVICE_CLIENT_ID")
        not in {
            s.strip()
            for s in control.get("LABWEAVER_SERVICE_ALLOWED_CLIENT_IDS", "").split(",")
        }
        or application["secrets"][workloads["agent-service"]["secret"]]["mtls-ca.pem"]
        != application["secrets"][workloads["environment-service"]["secret"]][
            "mtls-ca.pem"
        ]
    ):
        raise BundleError("LW_BUNDLE_AGENT_CONTROL_IDENTITY_MISMATCH")
    env = dict(agent["env"])
    for key, value in connection.items():
        if key in agent["secretEnv"] or (key in env and env[key] != value):
            raise BundleError("LW_BUNDLE_AGENT_CONTROL_CONFLICT")
        env[key] = value
    inputs["agent-service"] = {"env": env, "secretEnv": agent["secretEnv"]}


StrictLoader.add_constructor(
    yaml.resolver.BaseResolver.DEFAULT_MAPPING_TAG, _unique_mapping
)


class SecretText(str):
    """Use Ansible's unsafe scalar tag so secret text cannot become a template."""


class AnsibleDumper(yaml.SafeDumper):
    """Emit secret values as literal, non-templated Ansible input."""


AnsibleDumper.add_representer(
    SecretText, lambda dumper, value: dumper.represent_scalar("!unsafe", value)
)


def _document(payload: bytes) -> Any:
    try:
        if any(
            isinstance(token, (yaml.AliasToken, yaml.AnchorToken))
            for token in yaml.scan(payload)
        ):
            raise BundleError("LW_BUNDLE_DOCUMENT_INVALID")
        return yaml.load(payload, Loader=StrictLoader)
    except (yaml.YAMLError, UnicodeError, RecursionError) as error:
        raise BundleError("LW_BUNDLE_DOCUMENT_INVALID") from error


def _relative(value: Any, prefix: str | None = None) -> str:
    if not isinstance(value, str) or not re.fullmatch(r"[A-Za-z0-9._/-]+", value):
        raise BundleError("LW_BUNDLE_REFERENCE_INVALID")
    path = PurePosixPath(value)
    if (
        path.is_absolute()
        or str(path) != value
        or any(p in (".", "..") for p in path.parts)
        or len(value) > 255
        or len(path.name) > 100
        or len(str(path.parent)) > 155
    ):
        raise BundleError("LW_BUNDLE_REFERENCE_INVALID")
    if len(path.parts) < 2 or path.parts[0] not in {
        "config",
        "secrets",
        "versions",
        "docs",
    }:
        raise BundleError("LW_BUNDLE_REFERENCE_INVALID")
    if prefix is not None and path.parts[0] != prefix:
        raise BundleError("LW_BUNDLE_REFERENCE_INVALID")
    return value


def _no_symlink_ancestors(path: Path) -> None:
    for part in (path, *path.parents):
        if part.is_symlink():
            raise BundleError("LW_BUNDLE_PATH_INVALID")


def _private_directory(path: Path, create: bool = False) -> Path:
    path = path.absolute()
    _no_symlink_ancestors(path)
    path = path.resolve()
    if ".private" not in path.parts:
        raise BundleError("LW_BUNDLE_PRIVATE_DIRECTORY_REQUIRED")
    private_root = next(p for p in (path, *path.parents) if p.name == ".private")
    # Walk from the private root. Never change permissions on an operator-owned directory.
    current = private_root
    for component in (None, *path.relative_to(private_root).parts):
        if component is not None:
            current /= component
        if create and not current.exists():
            current.mkdir(mode=0o700)
        info = current.stat()
        if (
            not stat.S_ISDIR(info.st_mode)
            or info.st_uid != os.getuid()
            or info.st_mode & 0o077
        ):
            raise BundleError("LW_BUNDLE_PERMISSIONS_INVALID")
    return path


def _files(root: Path) -> dict[str, bytes]:
    result: dict[str, bytes] = {}
    total = 0
    for path in sorted(root.rglob("*")):
        if path.is_symlink():
            raise BundleError("LW_BUNDLE_PATH_INVALID")
        info = path.stat()
        if info.st_uid != os.getuid() or info.st_mode & 0o077:
            raise BundleError("LW_BUNDLE_PERMISSIONS_INVALID")
        if stat.S_ISDIR(info.st_mode):
            continue
        if (
            not stat.S_ISREG(info.st_mode)
            or not 0 < info.st_size <= objects.MAX_FILE_BYTES
        ):
            raise BundleError("LW_BUNDLE_FILE_INVALID")
        name = _relative(path.relative_to(root).as_posix())
        total += info.st_size
        if len(result) >= MAX_FILES or total > MAX_TOTAL_BYTES:
            raise BundleError("LW_BUNDLE_LIMIT_EXCEEDED")
        result[name] = path.read_bytes()
    return result


def _exact(value: Any, keys: set[str]) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != keys:
        raise BundleError("LW_BUNDLE_MODEL_INVALID")
    return value


def _value_references(value: Any, references: set[str]) -> None:
    if isinstance(value, dict):
        if set(value) in ({"file"}, {"secret"}):
            kind = next(iter(value))
            path = _relative(value[kind], "secrets" if kind == "secret" else None)
            references.add(path)
            return
        for key, item in value.items():
            if not isinstance(key, str) or (
                SECRET_FIELD.search(key)
                and not (isinstance(item, dict) and set(item) == {"secret"})
            ):
                raise BundleError("LW_BUNDLE_SECRET_REFERENCE_REQUIRED")
            _value_references(item, references)
    elif isinstance(value, list):
        for item in value:
            _value_references(item, references)
    elif isinstance(value, str) and ("{{" in value or "{%" in value):
        raise BundleError("LW_BUNDLE_TEMPLATE_FORBIDDEN")


CONTROLLER_LOCATORS = {
    "harbor_secret_locator_path": "harbor_secret_locator",
    "public_ingress_cloudflare_token_locator": "public_ingress_cloudflare_locator",
}

INFRASTRUCTURE_DERIVED = (
    {
        "labweaver_versions_lock_file",
        "identity_secret_locator_controller",
        "identity_secret_locator_sha256",
        "identity_secret_locator_controller_tmp",
        "harbor_secret_locator_revision",
    }
    | {
        f"{prefix}_{suffix}"
        for prefix in CONTROLLER_LOCATORS.values()
        for suffix in ("controller", "sha256", "controller_tmp")
    }
    | RESERVED_VARIABLES
    | RESOURCE_RESERVED
)


def _inventory_references(value: Any, references: set[str]) -> None:
    if isinstance(value, dict):
        if set(value) == {"file"}:
            references.add(_relative(value["file"], "secrets"))
            return
        if set(value) == {"secret"}:
            references.add(_relative(value["secret"], "secrets"))
            return
        for key, item in value.items():
            if not isinstance(key, str) or key in INFRASTRUCTURE_DERIVED:
                raise BundleError("LW_BUNDLE_DERIVED_OVERRIDE_FORBIDDEN")
            if key in {
                "ansible_ssh_args",
                "ansible_ssh_common_args",
                "ansible_ssh_extra_args",
                "ansible_host_key_checking",
                "ansible_ssh_host_key_checking",
            }:
                # Host-key policy remains in the repository's Ansible configuration.
                raise BundleError("LW_BUNDLE_SSH_OVERRIDE_FORBIDDEN")
            file_key = (
                key in CONTROLLER_LOCATORS
                or key
                in {
                    "ansible_ssh_private_key_file",
                    "ansible_private_key_file",
                }
                or key.endswith(("_credentials_file", "_kubeconfig_path", "_bundle"))
            )
            if file_key and not (isinstance(item, dict) and set(item) == {"file"}):
                raise BundleError("LW_BUNDLE_CREDENTIAL_FILE_REFERENCE_REQUIRED")
            if (
                key not in CONTROLLER_LOCATORS
                and SECRET_FIELD.search(key)
                and not (isinstance(item, dict) and set(item) == {"secret"})
            ):
                raise BundleError("LW_BUNDLE_SECRET_REFERENCE_REQUIRED")
            _inventory_references(item, references)
    elif isinstance(value, list):
        for item in value:
            _inventory_references(item, references)
    elif isinstance(value, str) and ("{{" in value or "{%" in value):
        raise BundleError("LW_BUNDLE_TEMPLATE_FORBIDDEN")


def _infrastructure_model(
    model: dict[str, Any], files: dict[str, bytes], references: set[str]
) -> None:
    infrastructure = _exact(
        model["infrastructure"],
        {"inventoryFile", "vaultPasswordFile", "identitySecretFile"},
    )
    inventory_file = _relative(infrastructure["inventoryFile"], "config")
    references.add(inventory_file)
    if inventory_file not in files:
        raise BundleError("LW_BUNDLE_INVENTORY_MISMATCH")
    inventory = _document(files[inventory_file])
    if not isinstance(inventory, dict) or set(inventory) != {"all"}:
        raise BundleError("LW_BUNDLE_INFRASTRUCTURE_INVENTORY_INVALID")
    _inventory_group(inventory["all"])
    _inventory_references(inventory, references)
    for name in ("vaultPasswordFile", "identitySecretFile"):
        if infrastructure[name] is not None:
            references.add(_relative(infrastructure[name], "secrets"))


def _inventory_group(group: Any) -> None:
    if not isinstance(group, dict) or set(group) - {"vars", "hosts", "children"}:
        raise BundleError("LW_BUNDLE_INFRASTRUCTURE_INVENTORY_INVALID")
    for key in ("vars", "hosts", "children"):
        entries = group.get(key, {})
        if not isinstance(entries, dict) or any(
            not isinstance(name, str) or not re.fullmatch(r"[A-Za-z0-9_.-]+", name)
            for name in entries
        ):
            raise BundleError("LW_BUNDLE_INFRASTRUCTURE_INVENTORY_INVALID")
    for variables in group.get("hosts", {}).values():
        if variables is not None and not isinstance(variables, dict):
            raise BundleError("LW_BUNDLE_INFRASTRUCTURE_INVENTORY_INVALID")
    for child in group.get("children", {}).values():
        _inventory_group(child)


def _derive_controller_locators(
    source: dict[str, Any],
    inventory: dict[str, Any],
    files: dict[str, bytes],
    output: Path,
) -> None:
    """Bind existing role locators to the same decrypted inventory, including host overrides."""
    temporary = str(output / "infrastructure/ansible-remote-tmp")

    def variables(raw: dict[str, Any], derived: dict[str, Any]) -> None:
        for key, prefix in CONTROLLER_LOCATORS.items():
            if key not in raw:
                continue
            name = raw[key]["file"]
            digest = "sha256:" + hashlib.sha256(files[name]).hexdigest()
            derived[f"{prefix}_controller"] = True
            derived[f"{prefix}_sha256"] = digest
            derived[f"{prefix}_controller_tmp"] = temporary
            if key == "harbor_secret_locator_path":
                derived["harbor_secret_locator_revision"] = digest

    def group(raw: dict[str, Any], derived: dict[str, Any]) -> None:
        variables(raw.get("vars", {}), derived.setdefault("vars", {}))
        for name, host in raw.get("hosts", {}).items():
            if host is not None:
                variables(host, derived["hosts"][name])
        for name, child in raw.get("children", {}).items():
            group(child, derived["children"][name])

    defaults = inventory["all"].setdefault("vars", {})
    # Absence must fail at the role, rather than fall back to a remote root file.
    for key, prefix in CONTROLLER_LOCATORS.items():
        if key not in source["all"].get("vars", {}):
            defaults[key] = ""
        defaults[f"{prefix}_controller"] = True
        defaults[f"{prefix}_sha256"] = ""
        defaults[f"{prefix}_controller_tmp"] = temporary
    defaults["harbor_secret_locator_revision"] = ""
    group(source["all"], inventory["all"])


def _configuration_version(
    payload: bytes, *, initial: bool = False, defaults: bool = False
) -> dict[str, Any]:
    """Use Control's Rust contract and canonical digest, without a second policy parser."""
    target = Path(
        os.environ.get("CARGO_TARGET_DIR", WORKLOADS_FILE.parents[3] / "target")
    )
    tool = Path(
        os.environ.get("LABWEAVER_BUNDLE_CONFIGURATION_TOOL", target / "debug/xtask")
    )
    try:
        result = subprocess.run(
            [
                str(tool),
                "bundle-configuration",
                *(["--defaults"] if defaults else ["--initial"] if initial else []),
            ],
            input=payload,
            capture_output=True,
            timeout=30,
        )
    except FileNotFoundError as error:
        raise BundleError("LW_BUNDLE_CONFIGURATION_TOOL_REQUIRED") from error
    except (OSError, subprocess.SubprocessError) as error:
        raise BundleError("LW_BUNDLE_CONFIGURATION_INVALID") from error
    if result.returncode:
        code = "LW_BUNDLE_CONFIGURATION_INVALID"
        for diagnostic in (
            "LW_BUNDLE_CONFIGURATION_DIGEST_MISMATCH",
            "LW_BUNDLE_CONFIGURATION_LIMIT",
            "LW_BUNDLE_CONFIGURATION_IO_FAILED",
            "LW_BUNDLE_INITIAL_REVISION_INVALID",
        ):
            if diagnostic.encode() in result.stderr:
                code = diagnostic
        raise BundleError(code)
    try:
        return json.loads(result.stdout)
    except (ValueError, UnicodeError) as error:
        raise BundleError("LW_BUNDLE_CONFIGURATION_INVALID") from error


def _workload_inputs(
    application: dict[str, Any], manifest: dict[str, Any], names: set[str] | None = None
) -> dict[str, Any]:
    workloads = _document(WORKLOADS_FILE.read_bytes())["workloads"]
    if names is not None:
        workloads = {name: workloads[name] for name in names}
    inputs = _exact(application["workloadInputs"], set(workloads))
    overrides = application["helmValues"].get("workloads", {})
    if not isinstance(overrides, dict) or set(overrides) - set(workloads):
        raise BundleError("LW_BUNDLE_WORKLOAD_INVALID")
    downward = {
        "LABWEAVER_EXECUTOR_POD_NAME",
        "LABWEAVER_EXECUTOR_POD_NAMESPACE",
        "LABWEAVER_EXECUTOR_POD_UID",
        "LABWEAVER_EXECUTOR_CONTAINER_NAME",
        "LABWEAVER_CONTROL_POD_NAME",
        "LABWEAVER_CONTROL_POD_NAMESPACE",
        "LABWEAVER_CONTROL_POD_UID",
        "LABWEAVER_PLATFORM_CONFIGURATION_VERSION_FILE",
        "LABWEAVER_PLATFORM_CONFIGURATION_SOURCE_FILE",
        "LABWEAVER_PLATFORM_CONFIGURATION_PROJECTED_DIR",
        "LABWEAVER_PLATFORM_CONFIGURATION_RUNTIME_DIR",
        "LABWEAVER_PLATFORM_CONFIGURATION_PROJECTED_SHA256",
    }
    result = {}
    for name, defaults in workloads.items():
        override = overrides.get(name, {})
        if not isinstance(override, dict) or {
            "env",
            "secretEnv",
            "configMap",
            "secret",
        } & set(override):
            raise BundleError("LW_BUNDLE_DERIVED_OVERRIDE_FORBIDDEN")
        configuration = dict(defaults, **override)
        source = _exact(inputs[name], {"env", "secretEnv"})
        env = source["env"]
        secret_env = source["secretEnv"]
        if not isinstance(env, dict) or not isinstance(secret_env, dict):
            raise BundleError("LW_BUNDLE_WORKLOAD_INVALID")
        if type(configuration.get("enabled")) is not bool:
            raise BundleError("LW_BUNDLE_WORKLOAD_INVALID")
        if configuration["enabled"]:
            inherited = (
                set(ENVIRONMENT_EXECUTOR_SHARED)
                if name in {"container-executor", "kubevirt-executor"}
                else set()
            )
            if set(defaults.get("env", {})) - set(env) - inherited or set(
                defaults.get("secretEnv", {})
            ) - set(secret_env):
                raise BundleError("LW_BUNDLE_WORKLOAD_INPUT_MISSING")
            for group, field in (("configMaps", "configMap"), ("secrets", "secret")):
                if (
                    configuration.get(field)
                    and configuration[field] not in manifest[group]
                ):
                    raise BundleError("LW_BUNDLE_WORKLOAD_OBJECT_MISSING")
        if set(env) & set(secret_env) or downward & (set(env) | set(secret_env)):
            raise BundleError("LW_BUNDLE_WORKLOAD_INVALID")
        for key, value in env.items():
            if (
                not isinstance(key, str)
                or not re.fullmatch(r"[A-Z_][A-Z0-9_]*", key)
                or not isinstance(value, str)
                or "\x00" in value
                or "{{" in value
                or "{%" in value
            ):
                raise BundleError("LW_BUNDLE_WORKLOAD_INVALID")
            if SECRET_FIELD.search(key):
                raise BundleError("LW_BUNDLE_SECRET_REFERENCE_REQUIRED")
        for key, reference in secret_env.items():
            if not isinstance(key, str) or not re.fullmatch(r"[A-Z_][A-Z0-9_]*", key):
                raise BundleError("LW_BUNDLE_WORKLOAD_INVALID")
            reference = _exact(reference, {"secret", "key"})
            if (
                not isinstance(reference["secret"], str)
                or not isinstance(reference["key"], str)
                or reference["secret"] not in manifest["secrets"]
                or reference["key"] not in manifest["secrets"][reference["secret"]]
            ):
                raise BundleError("LW_BUNDLE_SECRET_REFERENCE_INVALID")
        result[name] = source
    if names is None:
        _agent_control_inputs(application, result, workloads, overrides)
    return result


def _settings(model: dict[str, Any], files: dict[str, bytes]) -> dict[str, Any]:
    mode = model.get("configurationMode", "managed_version")
    if not isinstance(mode, str) or mode not in {"managed_version", "initial_settings"}:
        raise BundleError("LW_BUNDLE_CONFIGURATION_INVALID")
    document = _configuration_version(
        files[model["configurationFile"]], initial=mode == "initial_settings"
    )
    if mode == "initial_settings":
        if model["revision"] != 1:
            raise BundleError("LW_BUNDLE_INITIAL_REVISION_INVALID")
        return {
            "configuration": document["request"]["configuration"],
            "configurationSha256": document["configurationSha256"],
            "request": document["request"],
            "configurationMode": mode,
        }
    if document["revision"] != model["revision"]:
        raise BundleError("LW_BUNDLE_CONFIGURATION_REVISION_MISMATCH")
    return dict(document, configurationMode=mode)

def _configuration_policy(version: dict[str, Any]) -> dict[str, Any]:
    """Return the non-secret policy projection from the validated Rust model."""
    configuration = version.get("configuration")
    digest = version.get("configurationSha256")
    if not isinstance(configuration, dict):
        raise BundleError("LW_BUNDLE_CONFIGURATION_POLICY_INVALID: configuration")
    if not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest):
        raise BundleError("LW_BUNDLE_CONFIGURATION_POLICY_INVALID: configurationSha256")
    required_groups = {
        "lifecycle": {
            "readOnlyRetentionSeconds",
            "detailedLogRetentionSeconds",
            "expiryReminderSeconds",
            "preemptionGraceSeconds",
        },
        "runtimePolicy": {
            "defaultContainerUid",
            "allowedCapabilities",
            "preauthorizations",
            "privilegedAllowed",
            "privilegedExecutionLocation",
        },
        "builderPolicy": {
            "capabilities",
            "allowPrivilegeEscalation",
            "entitlements",
        },
    }
    for group, fields in required_groups.items():
        value = configuration.get(group)
        if not isinstance(value, dict):
            raise BundleError(
                f"LW_BUNDLE_CONFIGURATION_POLICY_INVALID: missing {group}"
            )
        missing = sorted(fields - set(value))
        if missing:
            raise BundleError(
                "LW_BUNDLE_CONFIGURATION_POLICY_INVALID: "
                f"{group} missing {','.join(missing)}"
            )
    runtime = configuration["runtimePolicy"]
    builder = configuration["builderPolicy"]
    if runtime["defaultContainerUid"] != 0:
        raise BundleError(
            "LW_PLATFORM_CONFIGURATION_POLICY_UNSUPPORTED: "
            f"runtimePolicy.defaultContainerUid={runtime['defaultContainerUid']} has no "
            "execution-side consumer (see #207)"
        )
    unsupported = (
        (runtime["allowedCapabilities"], "runtimePolicy.allowedCapabilities"),
        (runtime["preauthorizations"], "runtimePolicy.preauthorizations"),
        (builder["capabilities"], "builderPolicy.capabilities"),
        (builder["entitlements"], "builderPolicy.entitlements"),
    )
    for value, field in unsupported:
        if value:
            raise BundleError(
                "LW_PLATFORM_CONFIGURATION_POLICY_UNSUPPORTED: "
                f"{field} has no execution-side consumer (see #207)"
            )
    if builder["allowPrivilegeEscalation"]:
        raise BundleError(
            "LW_PLATFORM_CONFIGURATION_POLICY_UNSUPPORTED: "
            "builderPolicy.allowPrivilegeEscalation has no execution-side consumer (see #207)"
        )
    location = runtime["privilegedExecutionLocation"]
    if location is not None and location.get("kind") != "dedicated_node_pool":
        raise BundleError(
            "LW_PLATFORM_CONFIGURATION_POLICY_UNSUPPORTED: "
            "runtimePolicy.privilegedExecutionLocation has no execution-side consumer (see #207)"
        )
    return {
        "apiVersion": "deploy.labweaver.io/platform-policy/v1",
        "configurationSha256": digest,
        "lifecycle": configuration["lifecycle"],
        "runtimePolicy": configuration["runtimePolicy"],
        "builderPolicy": configuration["builderPolicy"],
    }


def _environment_executor_bindings(
    model: dict[str, Any], files: dict[str, bytes], manifest: dict[str, Any]
) -> tuple[dict[str, Any], dict[str, bytes], dict[str, Any]]:
    """Bind existing executor startup inputs to the Environment owner's source."""
    application = model["application"]
    inputs = _workload_inputs(application, manifest)
    defaults = _document(WORKLOADS_FILE.read_bytes())["workloads"]
    overrides = application["helmValues"].get("workloads", {})
    executors = [
        name
        for name in ("container-executor", "kubevirt-executor")
        if overrides.get(name, {}).get("enabled", defaults[name]["enabled"])
    ]
    if not executors:
        return inputs, {}, {}
    if not overrides.get("environment-service", {}).get(
        "enabled", defaults["environment-service"]["enabled"]
    ):
        raise BundleError("LW_BUNDLE_ENVIRONMENT_OWNER_REQUIRED")
    owner = inputs["environment-service"]
    owner_secret = defaults["environment-service"]["secret"]
    source_secrets = application["secrets"]
    authority_path = "/etc/labweaver/environment-authority/"
    required_scopes = {
        "resource.usage.record",
        "resource.environment.resolve",
        "resource.environment.release",
    }
    if not required_scopes <= set(
        owner["env"]["LABWEAVER_RESOURCE_USAGE_SCOPES"].split()
    ):
        raise BundleError("LW_BUNDLE_ENVIRONMENT_AUTHORITY_INVALID")
    references = {
        "database-url": owner["secretEnv"]["LABWEAVER_DATABASE_URL"],
        "nats-server": owner["secretEnv"]["LABWEAVER_NATS_SERVER"],
    }
    for variable, key in (
        ("LABWEAVER_SERVICE_OIDC_CA", "oidc-ca.pem"),
        ("LABWEAVER_SERVICE_CLIENT_SECRET_FILE", "service-client-secret"),
        ("LABWEAVER_RESOURCE_USAGE_CA_PATH", "mtls-ca.pem"),
        ("LABWEAVER_NATS_CA_PATH", "nats-ca.pem"),
    ):
        if owner["env"][variable] != "/etc/labweaver/secrets/" + key:
            raise BundleError("LW_BUNDLE_ENVIRONMENT_AUTHORITY_INVALID")
        references[key] = {"secret": owner_secret, "key": key}
    endpoint_ref = references["nats-server"]
    try:
        endpoint = (
            files[source_secrets[endpoint_ref["secret"]][endpoint_ref["key"]]]
            .decode()
            .strip()
        )
    except UnicodeError as error:
        raise BundleError("LW_BUNDLE_ENVIRONMENT_AUTHORITY_INVALID") from error
    if not endpoint.startswith("tls://") or any(c.isspace() for c in endpoint):
        raise BundleError("LW_BUNDLE_ENVIRONMENT_AUTHORITY_INVALID")
    projected: dict[str, list[dict[str, str]]] = {}
    for target, reference in references.items():
        projected.setdefault(reference["secret"], []).append(
            {"key": reference["key"], "path": target}
        )
    authority = {
        "secretSources": [
            {"name": name, "items": items} for name, items in sorted(projected.items())
        ]
    }
    providers = _document(
        files[application["configMaps"]["environment-service-config"]["providers.json"]]
    )
    if not isinstance(providers, list) or any(
        not isinstance(p, dict) for p in providers
    ):
        raise BundleError("LW_BUNDLE_ENVIRONMENT_EXECUTOR_INVALID")
    configs = {}
    for name in executors:
        source = inputs[name]
        if {"LABWEAVER_DATABASE_URL", "LABWEAVER_NATS_SERVER"} & (
            set(source["env"]) | set(source["secretEnv"])
        ):
            raise BundleError("LW_BUNDLE_ENVIRONMENT_AUTHORITY_CONFLICT")
        env = dict(source["env"])
        for variable in ENVIRONMENT_EXECUTOR_SHARED:
            if variable in source["secretEnv"] or (
                variable in env and env[variable] != owner["env"][variable]
            ):
                raise BundleError("LW_BUNDLE_ENVIRONMENT_AUTHORITY_CONFLICT")
            env[variable] = owner["env"][variable]
        for variable, key in (
            ("LABWEAVER_SERVICE_OIDC_CA", "oidc-ca.pem"),
            ("LABWEAVER_SERVICE_CLIENT_SECRET_FILE", "service-client-secret"),
            ("LABWEAVER_RESOURCE_USAGE_CA_PATH", "mtls-ca.pem"),
        ):
            env[variable] = authority_path + key
        inputs[name] = {"env": env, "secretEnv": dict(source["secretEnv"])}
        executor_secrets = source_secrets[defaults[name]["secret"]]
        for key in ("database-url", "postgres-ca.pem", "nats-ca.pem"):
            reference = references.get(key, {"secret": owner_secret, "key": key})
            if (
                executor_secrets[key]
                != source_secrets[reference["secret"]][reference["key"]]
            ):
                raise BundleError("LW_BUNDLE_ENVIRONMENT_AUTHORITY_CONFLICT")
        config_ref = application["configMaps"][defaults[name]["configMap"]][
            "config.yaml"
        ]
        config = _document(files[config_ref])
        if not isinstance(config, dict) or not isinstance(config.get("nats"), dict):
            raise BundleError("LW_BUNDLE_ENVIRONMENT_EXECUTOR_INVALID")
        nats = dict(config["nats"])
        kind, subject_field = (
            ("container", "containerRequestSubject")
            if name == "container-executor"
            else ("kubevirt", "kubevirtRequestSubject")
        )
        subjects = {
            p.get("subject") for p in providers if p.get("providerKind") == kind
        }
        if subjects != {nats.get(subject_field)} or not all(
            isinstance(s, str) and s and not any(c.isspace() for c in s)
            for s in subjects
        ):
            raise BundleError("LW_BUNDLE_ENVIRONMENT_AUTHORITY_CONFLICT")
        if config.get("databaseUrlFile") not in (
            "/etc/labweaver/secrets/database-url",
            authority_path + "database-url",
        ):
            raise BundleError("LW_BUNDLE_ENVIRONMENT_AUTHORITY_CONFLICT")
        if not (
            (nats.get("server") == endpoint and "serverFile" not in nats)
            or (
                nats.get("serverFile") == authority_path + "nats-server"
                and "server" not in nats
            )
        ):
            raise BundleError("LW_BUNDLE_ENVIRONMENT_AUTHORITY_CONFLICT")
        for field, key in (
            ("caFile", "nats-ca.pem"),
            ("clientCertificateFile", "nats-client.crt"),
            ("clientPrivateKeyFile", "nats-client.key"),
            ("credentialsFile", "nats.creds"),
        ):
            if nats.get(field) != "/etc/labweaver/secrets/" + key:
                raise BundleError("LW_BUNDLE_ENVIRONMENT_AUTHORITY_CONFLICT")
        nats.pop("server", None)
        nats["serverFile"] = authority_path + "nats-server"
        nats["caFile"] = authority_path + "nats-ca.pem"
        configs[config_ref] = yaml.safe_dump(
            dict(config, databaseUrlFile=authority_path + "database-url", nats=nats)
        ).encode()
    if "resource" in model:
        resource_env = model["resource"]["workloadInputs"]["resource-service"]["env"]
        client = owner["env"]["LABWEAVER_SERVICE_CLIENT_ID"]
        if (
            resource_env.get("LABWEAVER_ENVIRONMENT_SERVICE_CLIENT_ID") != client
            or client
            not in resource_env.get("LABWEAVER_SERVICE_ALLOWED_CLIENT_IDS", "").split(
                ","
            )
            or resource_env.get("LABWEAVER_SERVICE_AUDIENCE")
            != owner["env"]["LABWEAVER_RESOURCE_USAGE_AUDIENCE"]
            or resource_env.get("LABWEAVER_SERVICE_OIDC_ISSUER")
            != owner["env"]["LABWEAVER_SERVICE_OIDC_ISSUER"]
            or resource_env.get("LABWEAVER_SERVICE_OIDC_CA")
            != "/etc/labweaver/secrets/oidc-ca.pem"
            or model["resource"]["secrets"]["resource-service-secrets"]["oidc-ca.pem"]
            != source_secrets[owner_secret]["oidc-ca.pem"]
        ):
            raise BundleError("LW_BUNDLE_ENVIRONMENT_AUTHORITY_CONFLICT")
    return inputs, configs, authority


def _resource_model(
    model: dict[str, Any], files: dict[str, bytes], references: set[str]
) -> None:
    profile = _exact(
        model["resource"],
        {
            "target",
            "packageFile",
            "accessSeedFile",
            "natsIssuer",
            "configMaps",
            "secrets",
            "helmValues",
            "ansibleVariables",
            "workloadInputs",
        },
    )
    target = _exact(profile["target"], {"namespace", "releaseName"})
    if any(
        not isinstance(v, str) or not objects.DNS_LABEL.fullmatch(v)
        for v in target.values()
    ):
        raise BundleError("LW_BUNDLE_TARGET_INVALID")
    if target == model["target"]:
        raise BundleError("LW_BUNDLE_PROFILE_OWNERSHIP_CONFLICT")
    if not isinstance(profile["natsIssuer"], str) or not re.fullmatch(
        r"A[A-Z2-7]{55}", profile["natsIssuer"]
    ):
        raise BundleError("LW_BUNDLE_RESOURCE_ISSUER_REQUIRED")
    references.add(_relative(profile["packageFile"], "versions"))
    if profile["accessSeedFile"] is not None:
        references.add(_relative(profile["accessSeedFile"], "config"))
    manifest = resource_objects.load_manifest(RESOURCE_MANIFEST)
    for group, prefix in (("configMaps", "config"), ("secrets", "secrets")):
        bindings = _exact(profile[group], set(manifest[group]))
        for name, keys in manifest[group].items():
            for ref in _exact(bindings[name], set(keys)).values():
                references.add(_relative(ref, prefix))
    if not isinstance(profile["helmValues"], dict) or not isinstance(
        profile["ansibleVariables"], dict
    ):
        raise BundleError("LW_BUNDLE_MODEL_INVALID")
    if {"deploymentIdentity", "images", "workloadInputs"} & set(profile["helmValues"]):
        raise BundleError("LW_BUNDLE_DERIVED_OVERRIDE_FORBIDDEN")
    if any(
        not isinstance(profile["helmValues"].get(key, {}), dict)
        for key in ("network", "internalIdentityProxy")
    ):
        raise BundleError("LW_BUNDLE_MODEL_INVALID")
    for key in profile["ansibleVariables"]:
        if not key.startswith("resource_application_") or key in RESOURCE_RESERVED:
            raise BundleError("LW_BUNDLE_DERIVED_OVERRIDE_FORBIDDEN")
    _value_references(profile["ansibleVariables"], references)
    helm_references: set[str] = set()
    _value_references(profile["helmValues"], helm_references)
    if helm_references:
        raise BundleError("LW_BUNDLE_HELM_SECRET_FORBIDDEN")
    overrides = profile["helmValues"].get("workloads", {})
    if not isinstance(overrides, dict) or set(overrides) - {"resource-service"}:
        raise BundleError("LW_BUNDLE_PROFILE_OWNERSHIP_CONFLICT")
    resource_override = overrides.get("resource-service", {})
    if not isinstance(resource_override, dict) or "enabled" in resource_override:
        raise BundleError("LW_BUNDLE_DERIVED_OVERRIDE_FORBIDDEN")
    validation = dict(
        profile,
        helmValues=dict(
            profile["helmValues"],
            workloads={"resource-service": dict(resource_override, enabled=True)},
        ),
    )
    _workload_inputs(validation, manifest, {"resource-service"})
    package_name = profile["packageFile"]
    if package_name not in files:
        raise BundleError("LW_BUNDLE_INVENTORY_MISMATCH")
    package = _document(files[package_name])
    if (
        not isinstance(package, dict)
        or package.get("schema_version") != "platform-image-package-manifest.v2"
        or package.get("profile") != "resource"
        or not isinstance(package.get("images"), list)
        or len(package["images"]) != 1
        or not isinstance(package["images"][0], dict)
        or package["images"][0].get("component") != "resource-service"
        or not isinstance(package["images"][0].get("reference"), str)
        or not re.fullmatch(
            r"[^\s@]+@sha256:[0-9a-f]{64}", package["images"][0]["reference"]
        )
    ):
        raise BundleError("LW_BUNDLE_VERSIONS_INVALID")


def load_bundle(
    root: Path, manifest_path: Path = DEFAULT_MANIFEST
) -> tuple[dict[str, Any], dict[str, bytes]]:
    """Validate the complete source inventory and the existing application object contract."""
    files = _files(_private_directory(root))
    if MODEL_FILE not in files:
        raise BundleError("LW_BUNDLE_MODEL_MISSING")
    document = _document(files[MODEL_FILE])
    optional = {"resource", "configurationMode", "infrastructure"}
    model = _exact(
        document,
        {
            "apiVersion",
            "revision",
            "target",
            "versions",
            "docs",
            "application",
            "configurationFile",
        }
        | (set(document) & optional if isinstance(document, dict) else set()),
    )
    if (
        model["apiVersion"] != API_VERSION
        or type(model["revision"]) is not int
        or not 1 <= model["revision"] <= 2**63 - 1
    ):
        raise BundleError("LW_BUNDLE_VERSION_INVALID")
    target = _exact(model["target"], {"namespace", "releaseName"})
    if any(
        not isinstance(v, str) or not objects.DNS_LABEL.fullmatch(v)
        for v in target.values()
    ):
        raise BundleError("LW_BUNDLE_TARGET_INVALID")
    versions = _exact(model["versions"], {"lockFile", "packageFile"})
    docs = _exact(model["docs"], {"operationsFile"})
    references = {
        MODEL_FILE,
        _relative(model["configurationFile"], "config"),
        _relative(versions["lockFile"], "versions"),
        _relative(versions["packageFile"], "versions"),
        _relative(docs["operationsFile"], "docs"),
    }
    application = _exact(
        model["application"],
        {"configMaps", "secrets", "helmValues", "ansibleVariables", "workloadInputs"},
    )
    manifest = objects._load_manifest(manifest_path)
    for group, prefix in (("configMaps", "config"), ("secrets", "secrets")):
        bindings = _exact(application[group], set(manifest[group]))
        for name, keys in manifest[group].items():
            for ref in _exact(bindings[name], set(keys)).values():
                references.add(_relative(ref, prefix))
    if not isinstance(application["helmValues"], dict) or not isinstance(
        application["ansibleVariables"], dict
    ):
        raise BundleError("LW_BUNDLE_MODEL_INVALID")
    if {
        "deploymentIdentity",
        "images",
        "workloadInputs",
        "managedControlConfiguration",
        "environmentExecutorAuthority",
        "platformConfigurationPolicy",
    } & set(application["helmValues"]):
        raise BundleError("LW_BUNDLE_DERIVED_OVERRIDE_FORBIDDEN")
    for key in application["ansibleVariables"]:
        if not key.startswith("platform_application_") or key in RESERVED_VARIABLES:
            raise BundleError("LW_BUNDLE_DERIVED_OVERRIDE_FORBIDDEN")
    _value_references(application["ansibleVariables"], references)
    helm_references: set[str] = set()
    _value_references(application["helmValues"], helm_references)
    if helm_references:
        raise BundleError("LW_BUNDLE_HELM_SECRET_FORBIDDEN")
    overrides = application["helmValues"].get("workloads", {})
    if not isinstance(overrides, dict):
        raise BundleError("LW_BUNDLE_WORKLOAD_INVALID")
    resource_override = overrides.get("resource-service", {})
    if not isinstance(resource_override, dict) or resource_override.get(
        "enabled", False
    ):
        raise BundleError("LW_BUNDLE_PROFILE_OWNERSHIP_CONFLICT")
    control_replicas = overrides.get("control-service", {}).get("replicas", 1)
    if type(control_replicas) is not int or not 1 <= control_replicas <= 16:
        raise BundleError("LW_BUNDLE_WORKLOAD_INVALID")
    if "resource" in model:
        _resource_model(model, files, references)
    if "infrastructure" in model:
        _infrastructure_model(model, files, references)
    if set(files) != references:
        raise BundleError("LW_BUNDLE_INVENTORY_MISMATCH")
    _configuration_policy(_settings(model, files))
    _workload_inputs(application, manifest)
    _environment_executor_bindings(model, files, manifest)
    lock = _document(files[versions["lockFile"]])
    package = _document(files[versions["packageFile"]])
    if (
        not isinstance(lock, dict)
        or lock.get("schema_version") != 1
        or not isinstance(package, dict)
        or not isinstance(package.get("images"), list)
    ):
        raise BundleError("LW_BUNDLE_VERSIONS_INVALID")
    components: set[str] = set()
    for image in package["images"]:
        if (
            not isinstance(image, dict)
            or not isinstance(image.get("component"), str)
            or not objects.DNS_LABEL.fullmatch(image["component"])
            or image["component"] in components
            or not isinstance(image.get("reference"), str)
            or not re.fullmatch(r"[^\s@]+@sha256:[0-9a-f]{64}", image["reference"])
        ):
            raise BundleError("LW_BUNDLE_VERSIONS_INVALID")
        components.add(image["component"])
    workloads = _document(WORKLOADS_FILE.read_bytes())["workloads"]
    overrides = application["helmValues"].get("workloads", {})
    required_images = {
        workload["imageKey"]
        for name, workload in workloads.items()
        if overrides.get(name, {}).get("enabled", workload["enabled"])
    }
    if required_images - {component.replace("-", "_") for component in components}:
        raise BundleError("LW_BUNDLE_WORKLOAD_IMAGE_MISSING")
    secret_payloads = {
        payload
        for name, payload in files.items()
        if name.startswith("secrets/") and len(payload) >= 16
    }
    for name, payload in files.items():
        if b"AGE-SECRET-KEY-" in payload or payload.startswith(
            b"age-encryption.org/v1\n"
        ):
            raise BundleError("LW_BUNDLE_IDENTITY_MUST_BE_SEPARATE")
        if not name.startswith("secrets/") and (
            any(marker in payload for marker in PRIVATE_MARKERS)
            or any(secret in payload for secret in secret_payloads)
        ):
            raise BundleError("LW_BUNDLE_SECRET_LOCATION_INVALID")
    return model, files


def _write(path: Path, payload: bytes) -> None:
    _private_directory(path.parent, create=True)
    try:
        objects._write_exclusive(path, payload)
    except FileExistsError as error:
        raise BundleError("LW_BUNDLE_OUTPUT_EXISTS") from error


def _resolve_values(value: Any, root: Path, files: dict[str, bytes]) -> Any:
    if isinstance(value, dict):
        if set(value) == {"file"}:
            return str(root / value["file"])
        if set(value) == {"secret"}:
            try:
                return SecretText(files[value["secret"]].decode("utf-8").rstrip("\n"))
            except UnicodeError as error:
                raise BundleError("LW_BUNDLE_SECRET_TEXT_INVALID") from error
        return {key: _resolve_values(item, root, files) for key, item in value.items()}
    if isinstance(value, list):
        return [_resolve_values(item, root, files) for item in value]
    return value


def derive_bundle(
    root: Path,
    output: Path,
    manifest_path: Path = DEFAULT_MANIFEST,
    *,
    _authenticated_ciphertext_sha256: str | None = None,
) -> dict[str, Any]:
    """Derive application objects, Helm values and Ansible inputs from the same source."""
    model, files = load_bundle(root, manifest_path)
    application = model["application"]
    _private_directory(output.parent)
    _no_symlink_ancestors(output.absolute())
    output.mkdir(mode=0o700)
    manifest = objects._load_manifest(manifest_path)
    workload_inputs, executor_configs, executor_authority = (
        _environment_executor_bindings(model, files, manifest)
    )
    manifest["namespace"] = model["target"]["namespace"]
    manifest_output = output / "object-manifest.json"
    _write(manifest_output, json.dumps(manifest, sort_keys=True).encode())
    inputs = output / "object-inputs"
    for group, directory in (("configMaps", "configmaps"), ("secrets", "secrets")):
        for name, bindings in application[group].items():
            for key, ref in bindings.items():
                _write(
                    inputs / directory / name / key,
                    executor_configs.get(ref, files[ref]),
                )
    payload = objects.render_bundle(manifest_output, inputs)
    object_output = output / "application.yaml"
    _write(object_output, payload)
    # The object inputs are derived scratch, never a second editable configuration.
    shutil.rmtree(inputs)
    digest = "sha256:" + hashlib.sha256(payload).hexdigest()
    values = dict(application["helmValues"])
    values["workloadInputs"] = workload_inputs
    if executor_authority:
        values["environmentExecutorAuthority"] = executor_authority
    package = _document(files[model["versions"]["packageFile"]])
    values["images"] = {
        image["component"].replace("-", "_"): image["reference"]
        for image in package["images"]
    }
    values["deploymentIdentity"] = {"configurationBundleSha256": digest}
    version = _settings(model, files)
    policy = _configuration_policy(version)
    policy_payload = json.dumps(policy, sort_keys=True).encode()
    _write(output / "platform-configuration-policy.json", policy_payload)
    values["platformConfigurationPolicy"] = policy
    _write(
        output / "platform-configuration.json",
        json.dumps(
            (
                version["request"]
                if "request" in version
                else {k: v for k, v in version.items() if k != "configurationMode"}
            ),
            sort_keys=True,
        ).encode(),
    )
    if (
        _authenticated_ciphertext_sha256 is not None
        and version["configurationMode"] == "managed_version"
    ):
        source = {
            "apiVersion": "deploy.labweaver.io/loaded-source/v1",
            "desiredRevision": model["revision"],
            "configurationSha256": version["configurationSha256"],
            "encryptedBundleSha256": _authenticated_ciphertext_sha256,
            "installation": model["target"],
            "expectedControlReplicas": application["helmValues"]
            .get("workloads", {})
            .get("control-service", {})
            .get("replicas", 1),
        }
        source_payload = json.dumps(source, sort_keys=True).encode()
        _write(output / "platform-configuration-source.json", source_payload)
        if (
            application["helmValues"]
            .get("workloads", {})
            .get("control-service", {})
            .get("enabled", True)
        ):
            values["managedControlConfiguration"] = {
                "versionJson": (output / "platform-configuration.json").read_text(),
                "sourceJson": source_payload.decode(),
            }
    values_output = output / "helm-values.yaml"
    _write(values_output, yaml.safe_dump(values, sort_keys=True).encode())
    variables = _resolve_values(application["ansibleVariables"], root, files)
    variables.update(
        {
            "platform_application_namespace": model["target"]["namespace"],
            "platform_application_release_name": model["target"]["releaseName"],
            "platform_application_versions_lock_file": str(
                root / model["versions"]["lockFile"]
            ),
            "platform_application_package_manifest": str(
                root / model["versions"]["packageFile"]
            ),
            "platform_application_configuration_manifest": str(manifest_output),
            "platform_application_configuration_bundle": str(object_output),
            "platform_application_values_files": [str(values_output)],
        }
    )
    _write(
        output / "ansible-vars.yaml",
        yaml.dump(variables, Dumper=AnsibleDumper, sort_keys=True).encode(),
    )
    if "resource" in model:
        _derive_resource(root, output / "resource", model, files)
    if "infrastructure" in model:
        infrastructure = model["infrastructure"]
        source_inventory = _document(files[infrastructure["inventoryFile"]])
        inventory = _resolve_values(source_inventory, root, files)
        _derive_controller_locators(source_inventory, inventory, files, output)
        variables = inventory["all"].setdefault("vars", {})
        variables["labweaver_versions_lock_file"] = str(
            root / model["versions"]["lockFile"]
        )
        variables["identity_secret_locator_controller"] = (
            infrastructure["identitySecretFile"] is not None
        )
        variables["identity_secret_locator_controller_tmp"] = str(
            output / "infrastructure/ansible-remote-tmp"
        )
        if infrastructure["identitySecretFile"] is not None:
            variables["identity_secret_locator_sha256"] = (
                "sha256:"
                + hashlib.sha256(
                    files[infrastructure["identitySecretFile"]]
                ).hexdigest()
            )
        _write(
            output / "infrastructure/inventory.yaml",
            yaml.dump(inventory, Dumper=AnsibleDumper, sort_keys=True).encode(),
        )
    return {
        "revision": model["revision"],
        "applicationObjectsSha256": digest,
        "files": len(files),
        "managedConfigurationSha256": version["configurationSha256"],
        "configurationMode": version["configurationMode"],
    }


def _derive_resource(
    root: Path, output: Path, model: dict[str, Any], files: dict[str, bytes]
) -> None:
    profile = model["resource"]
    manifest = resource_objects.load_manifest(RESOURCE_MANIFEST)
    manifest["namespace"] = profile["target"]["namespace"]
    manifest_output = output / "object-manifest.json"
    _write(manifest_output, json.dumps(manifest, sort_keys=True).encode())
    inputs = output / "object-inputs"
    for group, directory in (("configMaps", "configmaps"), ("secrets", "secrets")):
        for name, bindings in profile[group].items():
            for key, ref in bindings.items():
                _write(inputs / directory / name / key, files[ref])
    payload = resource_objects.render(manifest_output, inputs, profile["natsIssuer"])
    bundle_output = output / "application.yaml"
    _write(bundle_output, payload)
    shutil.rmtree(inputs)
    workloads = _document(WORKLOADS_FILE.read_bytes())["workloads"]
    overrides = profile["helmValues"].get("workloads", {})
    values = dict(profile["helmValues"])
    values["workloads"] = {
        name: dict(overrides.get(name, {}), enabled=name == "resource-service")
        for name in workloads
    }
    values["workloadInputs"] = profile["workloadInputs"]
    values["images"] = {
        "resource_service": _document(files[profile["packageFile"]])["images"][0][
            "reference"
        ]
    }
    values["deploymentIdentity"] = {
        "configurationBundleSha256": "sha256:" + hashlib.sha256(payload).hexdigest()
    }
    values["internalIdentityProxy"] = dict(
        values.get("internalIdentityProxy", {}), enabled=False
    )
    values["network"] = dict(
        values.get("network", {}),
        defaultDenyEnabled=False,
        cdiCloneSourcePolicyEnabled=False,
        ciliumIngressPolicyEnabled=False,
        resourceKubernetesApiCiliumPolicyEnabled=True,
    )
    values_output = output / "helm-values.yaml"
    _write(values_output, yaml.safe_dump(values, sort_keys=True).encode())
    variables = _resolve_values(profile["ansibleVariables"], root, files)
    variables.update(
        {
            "resource_application_namespace": profile["target"]["namespace"],
            "resource_application_release_name": profile["target"]["releaseName"],
            "resource_application_versions_lock_file": str(
                root / model["versions"]["lockFile"]
            ),
            "resource_application_package_manifest": str(root / profile["packageFile"]),
            "resource_application_access_seed_file": (
                str(root / profile["accessSeedFile"])
                if profile["accessSeedFile"] is not None
                else ""
            ),
            "resource_application_configuration_manifest": str(manifest_output),
            "resource_application_configuration_bundle": str(bundle_output),
            "resource_application_values_file": str(values_output),
        }
    )
    _write(
        output / "ansible-vars.yaml",
        yaml.dump(variables, Dumper=AnsibleDumper, sort_keys=True).encode(),
    )


def _private_file(path: Path) -> bytes:
    _private_directory(path.absolute().parent)
    _no_symlink_ancestors(path.absolute())
    info = path.stat()
    if (
        not stat.S_ISREG(info.st_mode)
        or info.st_uid != os.getuid()
        or info.st_mode & 0o077
        or not 0 < info.st_size <= objects.MAX_FILE_BYTES
    ):
        raise BundleError("LW_BUNDLE_PERMISSIONS_INVALID")
    return path.read_bytes()


def _publish_private(path: Path, payload: bytes) -> None:
    """Publish a complete request or snapshot without replacing existing material."""
    _private_directory(path.parent, create=True)
    _no_symlink_ancestors(path)
    descriptor, pending_name = tempfile.mkstemp(prefix=".settings-", dir=path.parent)
    pending = Path(pending_name)
    try:
        with os.fdopen(descriptor, "wb") as output:
            output.write(payload)
            output.flush()
            os.fsync(output.fileno())
        os.link(pending, path)
    except FileExistsError as error:
        raise BundleError("LW_BUNDLE_OUTPUT_EXISTS") from error
    finally:
        pending.unlink(missing_ok=True)


def export_settings(version_file: Path, output: Path) -> dict[str, Any]:
    """Save only a verified immutable desired version, never applied or deployment history."""
    version = _configuration_version(_private_file(version_file))
    _publish_private(output.absolute(), json.dumps(version, sort_keys=True).encode())
    return {
        "revision": version["revision"],
        "managedConfigurationSha256": version["configurationSha256"],
    }


def initialize_settings(settings_file: Path | None, output: Path) -> dict[str, Any]:
    """Prepare unsaved settings. Only the real administrator API caller can create history."""
    payload = (
        json.dumps(
            {
                "expectedRevision": 0,
                "configuration": _document(_private_file(settings_file)),
            }
        ).encode()
        if settings_file
        else b""
    )
    document = _configuration_version(
        payload, initial=settings_file is not None, defaults=settings_file is None
    )
    _publish_private(
        output.absolute(), json.dumps(document["request"], sort_keys=True).encode()
    )
    return {
        "configurationMode": "initial_settings",
        "expectedRevision": 0,
        "managedConfigurationSha256": document["configurationSha256"],
    }


def import_settings(
    archive: Path, identity: Path, runtime: Path, expected_revision: int, output: Path
) -> dict[str, Any]:
    """Prepare the existing conditional update request. Authentication remains at the API."""
    if type(expected_revision) is not int or not 0 <= expected_revision <= 2**63 - 1:
        raise BundleError("LW_BUNDLE_VERSION_INVALID")
    with opened_bundle(archive, identity, runtime) as (root, _rendered, metadata):
        model, files = load_bundle(root)
        version = _settings(model, files)
        request = {
            "expectedRevision": expected_revision,
            "configuration": version["configuration"],
        }
        _publish_private(
            output.absolute(), json.dumps(request, sort_keys=True).encode()
        )
    return dict(metadata, expectedRevision=expected_revision)


def _age(argv: list[str], **kwargs: Any) -> None:
    try:
        subprocess.run(
            ["age", *argv], check=True, timeout=120, stderr=subprocess.DEVNULL, **kwargs
        )
    except FileNotFoundError as error:
        raise BundleError("LW_BUNDLE_AGE_REQUIRED") from error
    except (subprocess.SubprocessError, OSError) as error:
        raise BundleError("LW_BUNDLE_AGE_FAILED") from error


def seal_bundle(
    root: Path,
    output: Path,
    recipients: list[str],
    runtime: Path,
    manifest_path: Path = DEFAULT_MANIFEST,
) -> dict[str, Any]:
    """Encrypt the entire validated portable directory. Never replace an existing archive."""
    model, files = load_bundle(root, manifest_path)
    version = _settings(model, files)
    if not recipients or any(
        not re.fullmatch(r"age1[0-9a-z]+", recipient) for recipient in recipients
    ):
        raise BundleError("LW_BUNDLE_RECIPIENT_REQUIRED")
    runtime = _private_directory(runtime, create=True)
    _no_symlink_ancestors(output.absolute())
    if output.exists():
        raise BundleError("LW_BUNDLE_OUTPUT_EXISTS")
    # Validate the actual deployment object path before exporting, including NATS permissions.
    with tempfile.TemporaryDirectory(prefix="bundle-", dir=runtime) as temporary:
        temporary_root = Path(temporary)
        source_root = temporary_root / "bundle"
        for name, payload in files.items():
            _write(source_root / name, payload)
        derive_bundle(source_root, temporary_root / "rendered", manifest_path)
        archive_path = temporary_root / "source.tar"
        _write(archive_path, b"")
        with tarfile.open(
            archive_path, mode="w", format=tarfile.USTAR_FORMAT
        ) as archive:
            for name in sorted(files):
                with io.BytesIO(files[name]) as source:
                    info = tarfile.TarInfo(name)
                    info.size = len(files[name])
                    info.mode = 0o600
                    archive.addfile(info, source)
        # Ciphertext is staged on the destination filesystem for an atomic no-replace publish.
        output.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        descriptor, pending_name = tempfile.mkstemp(
            prefix=".bundle-", dir=output.parent
        )
        pending = Path(pending_name)
        try:
            with os.fdopen(descriptor, "wb") as encrypted, archive_path.open(
                "rb"
            ) as source:
                _age(
                    [
                        "--encrypt",
                        *[arg for recipient in recipients for arg in ("-r", recipient)],
                    ],
                    stdin=source,
                    stdout=encrypted,
                )
                encrypted.flush()
                os.fsync(encrypted.fileno())
            ciphertext_digest = hashlib.sha256()
            with pending.open("rb") as ciphertext:
                while chunk := ciphertext.read(1024 * 1024):
                    ciphertext_digest.update(chunk)
            os.link(pending, output)
        except FileExistsError as error:
            raise BundleError("LW_BUNDLE_OUTPUT_EXISTS") from error
        finally:
            pending.unlink(missing_ok=True)
    return {
        "revision": model["revision"],
        "files": len(files),
        "managedConfigurationSha256": version["configurationSha256"],
        "encryptedBundleSha256": ciphertext_digest.hexdigest(),
        "configurationMode": version["configurationMode"],
    }


def _extract(archive_path: Path, destination: Path) -> None:
    destination.mkdir(mode=0o700)
    names: set[str] = set()
    total = 0
    try:
        with tarfile.open(archive_path, mode="r:") as archive:
            for member in archive:
                name = _relative(member.name)
                if (
                    not member.isfile()
                    or name in names
                    or not 0 < member.size <= objects.MAX_FILE_BYTES
                ):
                    raise BundleError("LW_BUNDLE_ARCHIVE_INVALID")
                names.add(name)
                total += member.size
                if len(names) > MAX_FILES or total > MAX_TOTAL_BYTES:
                    raise BundleError("LW_BUNDLE_LIMIT_EXCEEDED")
                source = archive.extractfile(member)
                if source is None:
                    raise BundleError("LW_BUNDLE_ARCHIVE_INVALID")
                with source:
                    payload = source.read(member.size + 1)
                if len(payload) != member.size:
                    raise BundleError("LW_BUNDLE_ARCHIVE_INVALID")
                _write(destination / name, payload)
    except (tarfile.TarError, OSError) as error:
        raise BundleError("LW_BUNDLE_ARCHIVE_INVALID") from error


@contextmanager
def opened_bundle(
    archive: Path, identity: Path, runtime: Path, manifest_path: Path = DEFAULT_MANIFEST
) -> Iterator[tuple[Path, Path, dict[str, Any]]]:
    """Authenticate, validate and derive in a fresh runtime. Cleanup covers all exceptions."""
    _no_symlink_ancestors(archive.absolute())
    _no_symlink_ancestors(identity.absolute())
    archive = archive.resolve()
    identity = identity.resolve()
    if not identity.is_file() or identity.stat().st_mode & 0o077:
        raise BundleError("LW_BUNDLE_IDENTITY_REQUIRED")
    if not archive.is_file() or not 0 < archive.stat().st_size <= MAX_ARCHIVE_BYTES:
        raise BundleError("LW_BUNDLE_ARCHIVE_INVALID")
    runtime = _private_directory(runtime, create=True)
    with tempfile.TemporaryDirectory(prefix="bundle-", dir=runtime) as temporary:
        temporary_root = Path(temporary)
        if identity.absolute().is_relative_to(
            temporary_root
        ) or identity.absolute().is_relative_to(runtime):
            raise BundleError("LW_BUNDLE_IDENTITY_MUST_BE_SEPARATE")
        ciphertext_path = temporary_root / "source.age"
        _write(ciphertext_path, b"")
        encrypted_digest = hashlib.sha256()
        encrypted_bytes = 0
        # Hash and decrypt the same bounded snapshot, even if the input path is replaced.
        with archive.open("rb") as source, ciphertext_path.open("wb") as captured:
            while chunk := source.read(1024 * 1024):
                encrypted_bytes += len(chunk)
                if encrypted_bytes > MAX_ARCHIVE_BYTES:
                    raise BundleError("LW_BUNDLE_ARCHIVE_INVALID")
                encrypted_digest.update(chunk)
                captured.write(chunk)
        archive_path = temporary_root / "source.tar"
        _write(archive_path, b"")
        with archive_path.open("wb") as plaintext, ciphertext_path.open(
            "rb"
        ) as ciphertext:
            _age(
                ["--decrypt", "--identity", str(identity.absolute())],
                stdin=ciphertext,
                stdout=plaintext,
            )
        ciphertext_path.unlink()
        if archive_path.stat().st_size > MAX_ARCHIVE_BYTES:
            raise BundleError("LW_BUNDLE_LIMIT_EXCEEDED")
        root = temporary_root / "bundle"
        _extract(archive_path, root)
        archive_path.unlink()
        output = temporary_root / "rendered"
        metadata = derive_bundle(
            root,
            output,
            manifest_path,
            _authenticated_ciphertext_sha256=encrypted_digest.hexdigest(),
        )
        metadata["encryptedBundleSha256"] = encrypted_digest.hexdigest()
        yield root, output, metadata


def _interrupt(signum: int, _frame: Any) -> None:
    raise BundleError("LW_BUNDLE_INTERRUPTED")


def _consume(command: list[str], environment: dict[str, str]) -> None:
    # Keep descendants in one process group. They must stop before plaintext disappears.
    with subprocess.Popen(
        command,
        env=environment,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        start_new_session=True,
    ) as process:
        try:
            if process.wait():
                raise BundleError("LW_BUNDLE_COMMAND_FAILED")
        finally:
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                pass
            # Also stop detached-in-group descendants after the main consumer exited.
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runtime", type=Path, default=Path(".private/bundle-runtime"))
    subcommands = parser.add_subparsers(dest="operation", required=True)
    validate = subcommands.add_parser("validate")
    validate.add_argument("--bundle", type=Path, required=True)
    seal = subcommands.add_parser("seal")
    seal.add_argument("--bundle", type=Path, required=True)
    seal.add_argument("--recipient", action="append", required=True)
    seal.add_argument("--output", type=Path, required=True)
    use = subcommands.add_parser("exec")
    use.add_argument("--archive", type=Path, required=True)
    use.add_argument("--identity", type=Path, required=True)
    use.add_argument("command", nargs=argparse.REMAINDER)
    export = subcommands.add_parser("settings-export")
    export.add_argument("--version-file", type=Path, required=True)
    export.add_argument("--output", type=Path, required=True)
    restore = subcommands.add_parser("settings-import")
    restore.add_argument("--archive", type=Path, required=True)
    restore.add_argument("--identity", type=Path, required=True)
    restore.add_argument("--expected-revision", type=int, required=True)
    restore.add_argument("--output", type=Path, required=True)
    initialize = subcommands.add_parser("settings-init")
    initial_source = initialize.add_mutually_exclusive_group(required=True)
    initial_source.add_argument("--defaults", action="store_true")
    initial_source.add_argument("--settings-file", type=Path)
    initialize.add_argument("--output", type=Path, required=True)
    arguments = parser.parse_args()
    previous = {
        sig: signal.signal(sig, _interrupt) for sig in (signal.SIGINT, signal.SIGTERM)
    }
    try:
        if arguments.operation == "validate":
            # Validation includes the same derived object checks used during encrypted execution.
            runtime = _private_directory(arguments.runtime, create=True)
            with tempfile.TemporaryDirectory(
                prefix="bundle-", dir=runtime
            ) as temporary:
                metadata = derive_bundle(
                    arguments.bundle.absolute(), Path(temporary) / "rendered"
                )
        elif arguments.operation == "seal":
            metadata = seal_bundle(
                arguments.bundle.absolute(),
                arguments.output.absolute(),
                arguments.recipient,
                arguments.runtime,
            )
        elif arguments.operation == "settings-export":
            metadata = export_settings(arguments.version_file, arguments.output)
        elif arguments.operation == "settings-import":
            metadata = import_settings(
                arguments.archive,
                arguments.identity,
                arguments.runtime,
                arguments.expected_revision,
                arguments.output,
            )
        elif arguments.operation == "settings-init":
            metadata = initialize_settings(arguments.settings_file, arguments.output)
        else:
            with opened_bundle(
                arguments.archive, arguments.identity, arguments.runtime
            ) as (root, rendered, metadata):
                command = arguments.command
                if command[:1] == ["--"]:
                    command = command[1:]
                if command:
                    environment = dict(
                        os.environ,
                        LW_BUNDLE_ROOT=str(root),
                        LW_BUNDLE_RENDERED_ROOT=str(rendered),
                        LABWEAVER_APPLICATION_VARS_FILE=str(
                            rendered / "ansible-vars.yaml"
                        ),
                    )
                    for key in (
                        "LABWEAVER_PLATFORM_CONFIGURATION_VERSION_FILE",
                        "LABWEAVER_PLATFORM_CONFIGURATION_SOURCE_FILE",
                        "LABWEAVER_PLATFORM_CONFIGURATION_UPDATE_FILE",
                        "LABWEAVER_RESOURCE_VARS_FILE",
                        "LABWEAVER_BUNDLE_INVENTORY_FILE",
                        "LABWEAVER_BUNDLE_VAULT_PASSWORD_FILE",
                        "LABWEAVER_IDENTITY_SECRET_LOCATOR",
                        "ANSIBLE_INVENTORY",
                        "ANSIBLE_VAULT_PASSWORD_FILE",
                        "ANSIBLE_VAULT_IDENTITY_LIST",
                        "ANSIBLE_PRIVATE_KEY_FILE",
                        "ANSIBLE_HOST_KEY_CHECKING",
                        "ANSIBLE_SSH_HOST_KEY_CHECKING",
                        "ANSIBLE_SSH_ARGS",
                        "ANSIBLE_SSH_COMMON_ARGS",
                        "ANSIBLE_SSH_EXTRA_ARGS",
                        "ANSIBLE_LOG_PATH",
                    ):
                        environment.pop(key, None)
                    setting_key = (
                        "LABWEAVER_PLATFORM_CONFIGURATION_UPDATE_FILE"
                        if metadata["configurationMode"] == "initial_settings"
                        else "LABWEAVER_PLATFORM_CONFIGURATION_VERSION_FILE"
                    )
                    environment[setting_key] = str(
                        rendered / "platform-configuration.json"
                    )
                    if metadata["configurationMode"] == "managed_version":
                        environment["LABWEAVER_PLATFORM_CONFIGURATION_SOURCE_FILE"] = (
                            str(rendered / "platform-configuration-source.json")
                        )
                    if (rendered / "resource/ansible-vars.yaml").is_file():
                        environment["LABWEAVER_RESOURCE_VARS_FILE"] = str(
                            rendered / "resource/ansible-vars.yaml"
                        )
                    model, _ = load_bundle(root)
                    if "infrastructure" in model:
                        infrastructure = model["infrastructure"]
                        environment["ANSIBLE_CONFIG"] = str(
                            Path(__file__).resolve().parents[1]
                            / "deploy/ansible/ansible.cfg"
                        )
                        environment["ANSIBLE_LOCAL_TEMP"] = str(
                            _private_directory(
                                rendered / "infrastructure/ansible-local-tmp",
                                create=True,
                            )
                        )
                        environment["LABWEAVER_BUNDLE_INVENTORY_FILE"] = str(
                            rendered / "infrastructure/inventory.yaml"
                        )
                        environment["ANSIBLE_INVENTORY"] = environment[
                            "LABWEAVER_BUNDLE_INVENTORY_FILE"
                        ]
                        for field, variable in (
                            (
                                "vaultPasswordFile",
                                "LABWEAVER_BUNDLE_VAULT_PASSWORD_FILE",
                            ),
                            ("identitySecretFile", "LABWEAVER_IDENTITY_SECRET_LOCATOR"),
                        ):
                            if infrastructure[field] is not None:
                                environment[variable] = str(
                                    root / infrastructure[field]
                                )
                        if "LABWEAVER_BUNDLE_VAULT_PASSWORD_FILE" in environment:
                            environment["ANSIBLE_VAULT_PASSWORD_FILE"] = environment[
                                "LABWEAVER_BUNDLE_VAULT_PASSWORD_FILE"
                            ]
                    # A consumer can read secrets. Do not forward its stdout/stderr to ordinary logs.
                    _consume(command, environment)
        print(json.dumps(metadata, sort_keys=True))
        return 0
    except (
        BundleError,
        objects.BundleError,
        resource_objects.BundleError,
        OSError,
        KeyboardInterrupt,
    ) as error:
        diagnostic = (
            str(error)
            if isinstance(
                error, (BundleError, objects.BundleError, resource_objects.BundleError)
            )
            else "LW_BUNDLE_IO_FAILED"
        )
        print(diagnostic, file=sys.stderr)
        return 1
    finally:
        for sig, handler in previous.items():
            signal.signal(sig, handler)


if __name__ == "__main__":
    raise SystemExit(main())
