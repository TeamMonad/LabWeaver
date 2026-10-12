#!/usr/bin/env python3
"""Prepare a bounded, isolated Kubernetes restore smoke test; never contact a cluster."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import sys
from typing import Any

import yaml

try:
    import deployment_bundle as bundle
except ModuleNotFoundError:
    from tools import deployment_bundle as bundle


def prepare(namespace: str, output: Path) -> dict[str, Any]:
    if not namespace.startswith(
        "lw-bundle-203-"
    ) or not bundle.objects.DNS_LABEL.fullmatch(namespace):
        raise bundle.BundleError("LW_BUNDLE_ACCEPTANCE_NAMESPACE_INVALID")
    if not os.environ.get("LW_BUNDLE_ROOT") or not os.environ.get(
        "LW_BUNDLE_RENDERED_ROOT"
    ):
        raise bundle.BundleError("LW_BUNDLE_ACCEPTANCE_EXEC_REQUIRED")
    root = Path(os.environ["LW_BUNDLE_ROOT"])
    rendered = Path(os.environ["LW_BUNDLE_RENDERED_ROOT"])
    model, files = bundle.load_bundle(root)
    if namespace in {
        model["target"]["namespace"],
        model.get("resource", {}).get("target", {}).get("namespace"),
    }:
        raise bundle.BundleError("LW_BUNDLE_ACCEPTANCE_NAMESPACE_INVALID")
    configuration = bundle._private_file(rendered / "platform-configuration.json")
    settings = bundle._settings(model, files)
    expected = settings.get(
        "request",
        {key: value for key, value in settings.items() if key != "configurationMode"},
    )
    if configuration != json.dumps(expected, sort_keys=True).encode():
        raise bundle.BundleError("LW_BUNDLE_ACCEPTANCE_CONFIG_CHANGED")
    if len(configuration) > 900_000:
        raise bundle.BundleError("LW_BUNDLE_ACCEPTANCE_CONFIG_TOO_LARGE")
    lock = bundle._document(files[model["versions"]["lockFile"]])
    image = lock.get("test_images", {}).get("busybox")
    if not isinstance(image, str) or not re.fullmatch(
        r"[^\s@]+@sha256:[0-9a-f]{64}", image
    ):
        raise bundle.BundleError("LW_BUNDLE_ACCEPTANCE_IMAGE_MISSING")
    digest = hashlib.sha256(configuration).hexdigest()
    mode = model.get("configurationMode", "managed_version")
    labels = {
        "app.kubernetes.io/name": "labweaver-bundle-acceptance",
        "labweaver.io/issue": "203",
    }
    metadata = {"namespace": namespace, "labels": labels}
    script = 'test "$(sha256sum /input/platform-configuration.json | cut -d " " -f 1)" = "$1"; echo BUNDLE_RESTORE_SMOKE_OK'
    resources = [
        {
            "apiVersion": "v1",
            "kind": "Namespace",
            "metadata": {
                "name": namespace,
                "labels": {
                    **labels,
                    "pod-security.kubernetes.io/enforce": "restricted",
                    "pod-security.kubernetes.io/enforce-version": "v1.35",
                },
            },
        },
        {
            "apiVersion": "v1",
            "kind": "ResourceQuota",
            "metadata": {**metadata, "name": "bundle-acceptance"},
            "spec": {
                "hard": {
                    "pods": "2",
                    "requests.cpu": "500m",
                    "limits.cpu": "1",
                    "requests.memory": "256Mi",
                    "limits.memory": "512Mi",
                    "requests.ephemeral-storage": "128Mi",
                    "limits.ephemeral-storage": "256Mi",
                    "count/jobs.batch": "1",
                    "count/configmaps": "1",
                    "count/secrets": "0",
                    "persistentvolumeclaims": "0",
                }
            },
        },
        {
            "apiVersion": "networking.k8s.io/v1",
            "kind": "NetworkPolicy",
            "metadata": {**metadata, "name": "bundle-acceptance-deny-all"},
            "spec": {"podSelector": {}, "policyTypes": ["Ingress", "Egress"]},
        },
        {
            "apiVersion": "v1",
            "kind": "ConfigMap",
            "metadata": {**metadata, "name": "bundle-configuration"},
            "immutable": True,
            "data": {"platform-configuration.json": configuration.decode("utf-8")},
        },
        {
            "apiVersion": "batch/v1",
            "kind": "Job",
            "metadata": {**metadata, "name": "bundle-restore"},
            "spec": {
                "backoffLimit": 0,
                "activeDeadlineSeconds": 180,
                "template": {
                    "metadata": {"labels": labels},
                    "spec": {
                        "automountServiceAccountToken": False,
                        "restartPolicy": "Never",
                        "nodeSelector": {
                            "kubernetes.io/os": "linux",
                            "kubernetes.io/arch": "amd64",
                        },
                        "securityContext": {
                            "runAsNonRoot": True,
                            "runAsUser": 65532,
                            "runAsGroup": 65532,
                            "seccompProfile": {"type": "RuntimeDefault"},
                        },
                        "containers": [
                            {
                                "name": "restore",
                                "image": image,
                                "imagePullPolicy": "IfNotPresent",
                                "command": ["/bin/sh"],
                                "args": [
                                    "-ec",
                                    script,
                                    "bundle-restore",
                                    digest,
                                ],
                                "securityContext": {
                                    "allowPrivilegeEscalation": False,
                                    "readOnlyRootFilesystem": True,
                                    "capabilities": {"drop": ["ALL"]},
                                },
                                "resources": {
                                    "requests": {
                                        "cpu": "50m",
                                        "memory": "64Mi",
                                        "ephemeral-storage": "16Mi",
                                    },
                                    "limits": {
                                        "cpu": "250m",
                                        "memory": "128Mi",
                                        "ephemeral-storage": "64Mi",
                                    },
                                },
                                "volumeMounts": [
                                    {
                                        "name": "configuration",
                                        "mountPath": "/input",
                                        "readOnly": True,
                                    }
                                ],
                            }
                        ],
                        "volumes": [
                            {
                                "name": "configuration",
                                "configMap": {
                                    "name": "bundle-configuration",
                                    "defaultMode": 0o444,
                                },
                            }
                        ],
                    },
                },
            },
        },
    ]
    bundle._publish_private(
        output, yaml.safe_dump_all(resources, sort_keys=False).encode()
    )
    return {
        "namespace": namespace,
        "objects": len(resources),
        "configurationFileSha256": digest,
        "revision": model["revision"],
        "configurationMode": mode,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--namespace", required=True)
    parser.add_argument("--output", type=Path, required=True)
    arguments = parser.parse_args()
    try:
        print(
            json.dumps(prepare(arguments.namespace, arguments.output), sort_keys=True)
        )
        return 0
    except (bundle.BundleError, OSError) as error:
        print(
            (
                str(error)
                if isinstance(error, bundle.BundleError)
                else "LW_BUNDLE_ACCEPTANCE_PREPARE_FAILED"
            ),
            file=sys.stderr,
        )
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
