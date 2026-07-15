#!/usr/bin/env python3
"""Apply reviewed scaffold corrections that upstream values cannot express."""

from __future__ import annotations

import sys

import yaml


documents = list(yaml.safe_load_all(sys.stdin))
rekor_patched = 0
fulcio_patched = 0
for document in documents:
    if not isinstance(document, dict):
        continue
    metadata = document.get("metadata", {})
    if document.get("kind") != "Deployment":
        continue
    if metadata.get("name") == "fulcio-server":
        template_metadata = document["spec"]["template"].setdefault("metadata", {})
        template_metadata.setdefault("annotations", {})[
            "labweaver.io/post-renderer-contract"
        ] = "identity-ca-v1"
        pod_spec = document["spec"]["template"]["spec"]
        containers = pod_spec.get("containers", [])
        fulcio = next((item for item in containers if item.get("name") == "fulcio-server"), None)
        if fulcio is None:
            raise SystemExit("SIGSTORE_POST_RENDERER_FULCIO_CONTAINER_INVALID")
        volume_name = "labweaver-identity-ca"
        if any(item.get("name") == volume_name for item in pod_spec.get("volumes", [])):
            raise SystemExit("SIGSTORE_POST_RENDERER_IDENTITY_CA_VOLUME_CONFLICT")
        pod_spec.setdefault("volumes", []).append(
            {
                "name": volume_name,
                "configMap": {
                    "name": "private-sigstore-keycloak-ca",
                    "items": [{"key": "ca.crt", "path": "ca.crt"}],
                },
            }
        )
        fulcio.setdefault("volumeMounts", []).append(
            {
                "name": volume_name,
                "mountPath": "/var/run/labweaver-identity-ca",
                "readOnly": True,
            }
        )
        fulcio.setdefault("env", []).append(
            {
                "name": "SSL_CERT_FILE",
                "value": "/var/run/labweaver-identity-ca/ca.crt",
            }
        )
        fulcio_patched += 1
        continue
    if metadata.get("name") != "rekor-server":
        continue
    pod_spec = document["spec"]["template"]["spec"]
    signer_volume = None
    for volume in pod_spec.get("volumes", []):
        secret = volume.get("secret")
        if secret and secret.get("secretName") == "private-sigstore-rekor-signer":
            secret["defaultMode"] = 0o640
            signer_volume = volume

    if signer_volume is None:
        continue

    runtime_volume_name = signer_volume["name"]
    source_volume_name = f"{runtime_volume_name}-source"
    secret_items = signer_volume["secret"].get("items", [])
    if len(secret_items) != 1 or secret_items[0].get("key") != "private":
        raise SystemExit("SIGSTORE_POST_RENDERER_SIGNER_ITEM_INVALID")
    source_key_path = secret_items[0].get("path")
    if not source_key_path or "/" in source_key_path:
        raise SystemExit("SIGSTORE_POST_RENDERER_SIGNER_PATH_INVALID")
    signer_volume["name"] = source_volume_name
    pod_spec["volumes"].append(
        {"name": runtime_volume_name, "emptyDir": {"medium": "Memory"}}
    )

    existing_init = pod_spec.get("initContainers", [])
    if not existing_init or "@sha256:" not in existing_init[0].get("image", ""):
        raise SystemExit("SIGSTORE_POST_RENDERER_INIT_IMAGE_INVALID")
    materializer = {
        "name": "materialize-rekor-signer",
        "image": existing_init[0]["image"],
        "imagePullPolicy": "IfNotPresent",
        "command": ["sh", "-ec"],
        "args": [
            f"umask 077; cp /signer-source/{source_key_path} "
            "/signer-runtime/private-key.pem; "
            "chmod 0600 /signer-runtime/private-key.pem; "
            "test -r /signer-runtime/private-key.pem"
        ],
        "securityContext": {
            "allowPrivilegeEscalation": False,
            "readOnlyRootFilesystem": True,
            "runAsNonRoot": True,
            "runAsUser": 65533,
            "runAsGroup": 65533,
            "capabilities": {"drop": ["ALL"]},
            "seccompProfile": {"type": "RuntimeDefault"},
        },
        "volumeMounts": [
            {"name": source_volume_name, "mountPath": "/signer-source", "readOnly": True},
            {"name": runtime_volume_name, "mountPath": "/signer-runtime"},
        ],
    }
    pod_spec.setdefault("initContainers", []).append(materializer)
    rekor_patched += 1

if rekor_patched != 1:
    raise SystemExit("SIGSTORE_POST_RENDERER_SIGNER_VOLUME_INVALID")
if fulcio_patched != 1:
    raise SystemExit("SIGSTORE_POST_RENDERER_FULCIO_DEPLOYMENT_INVALID")

yaml.safe_dump_all(documents, sys.stdout, explicit_start=True, sort_keys=False)
