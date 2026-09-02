#!/usr/bin/python3
"""Apply the Resource ConfigMap while preserving NATS authority ownership.

The Resource bundle contains a ConfigMap and NATS credential Secret.  The
authority rotation owns the Secret fields, so Resource deployment verifies their
exact current content and applies only the ConfigMap.  It never force-takes
server-side-apply ownership of credential material.
"""

from __future__ import annotations

import json
import os
import subprocess
import tempfile
from pathlib import Path
from typing import Any

import yaml
from ansible.module_utils.basic import AnsibleModule


def fail(module: AnsibleModule, code: str) -> None:
    module.fail_json(msg=code, diagnostic_code=code)


def command(arguments: list[str], code: str) -> subprocess.CompletedProcess[str]:
    try:
        result = subprocess.run(
            arguments,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=30,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise RuntimeError(code) from error
    if result.returncode != 0:
        raise RuntimeError(code)
    return result


def main() -> None:
    module = AnsibleModule(
        argument_spec={
            "kubeconfig": {"type": "path", "required": True},
            "bundle": {"type": "path", "required": True},
        },
        supports_check_mode=False,
    )
    kubeconfig = Path(module.params["kubeconfig"])
    bundle = Path(module.params["bundle"])
    if not kubeconfig.is_file() or kubeconfig.is_symlink() or not bundle.is_file() or bundle.is_symlink():
        fail(module, "RESOURCE_APPLICATION_BUNDLE_INPUT_INVALID")
    try:
        documents = list(yaml.safe_load_all(bundle.read_text(encoding="utf-8")))
    except (OSError, yaml.YAMLError) as error:
        fail(module, "RESOURCE_APPLICATION_BUNDLE_INPUT_INVALID")
        raise error
    if len(documents) != 2 or not all(isinstance(value, dict) for value in documents):
        fail(module, "RESOURCE_APPLICATION_BUNDLE_INPUT_INVALID")
    indexed: dict[tuple[str, str], dict[str, Any]] = {}
    for document in documents:
        metadata = document.get("metadata")
        if not isinstance(metadata, dict) or metadata.get("namespace") != "labweaver-system":
            fail(module, "RESOURCE_APPLICATION_BUNDLE_INPUT_INVALID")
        kind, name = document.get("kind"), metadata.get("name")
        if not isinstance(kind, str) or not isinstance(name, str):
            fail(module, "RESOURCE_APPLICATION_BUNDLE_INPUT_INVALID")
        indexed[(kind, name)] = document
    configmap = indexed.get(("ConfigMap", "resource-service-config"))
    secret = indexed.get(("Secret", "resource-service-secrets"))
    if configmap is None or secret is None or len(indexed) != 2:
        fail(module, "RESOURCE_APPLICATION_BUNDLE_INPUT_INVALID")
    expected_secret_data = secret.get("data")
    if not isinstance(expected_secret_data, dict) or not expected_secret_data:
        fail(module, "RESOURCE_APPLICATION_BUNDLE_INPUT_INVALID")
    base = ["/usr/bin/kubectl", "--kubeconfig", str(kubeconfig), "--namespace", "labweaver-system"]
    try:
        observed = command(base + ["get", "secret/resource-service-secrets", "--output", "json"], "RESOURCE_APPLICATION_SECRET_UNAVAILABLE")
        observed_document = json.loads(observed.stdout)
    except (RuntimeError, json.JSONDecodeError):
        fail(module, "RESOURCE_APPLICATION_SECRET_UNAVAILABLE")
    if not isinstance(observed_document, dict) or observed_document.get("data") != expected_secret_data:
        fail(module, "RESOURCE_APPLICATION_SECRET_OWNERSHIP_CONFLICT")

    descriptor, temporary = tempfile.mkstemp(prefix=".resource-configmap-", suffix=".yaml")
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as handle:
            yaml.safe_dump(configmap, handle, sort_keys=True)
        os.chmod(temporary, 0o600)
        command(
            base + ["apply", "--server-side", "--field-manager=labweaver-resource-application", "--filename", temporary],
            "RESOURCE_APPLICATION_CONFIGMAP_APPLY_FAILED",
        )
    except (OSError, yaml.YAMLError, RuntimeError):
        fail(module, "RESOURCE_APPLICATION_CONFIGMAP_APPLY_FAILED")
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)
    module.exit_json(changed=True, diagnostic_code="RESOURCE_APPLICATION_BUNDLE_APPLIED")


if __name__ == "__main__":
    main()
