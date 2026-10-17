#!/usr/bin/env python3
"""Read an authorized Control Deployment roster and live process snapshots; never apply."""

from __future__ import annotations

import argparse
import datetime as dt
import http.client
import ipaddress
import json
import os
from pathlib import Path
import socket
import ssl
import subprocess
import sys
import uuid

import deployment_bundle as bundle


class ObservationError(Exception):
    """Bounded observation failure without credentials, response bodies or command output."""


def fail() -> None:
    raise ObservationError("LW_CONTROL_REPLICA_OBSERVATION_FAILED")


def _uid(value):
    try:
        return (
            isinstance(value, str)
            and uuid.UUID(value).int != 0
            and str(uuid.UUID(value)) == value
        )
    except (ValueError, AttributeError):
        return False


def roster(deployment, replica_sets, pods, source):
    """Require the exact active roster, including old or terminating rollout Pods."""
    try:
        metadata, spec = deployment["metadata"], deployment["spec"]
        namespace = source["installation"]["namespace"]
        count = source["expectedControlReplicas"]
        labels = spec["selector"]["matchLabels"]
        if (
            metadata["namespace"] != namespace
            or metadata["name"] != "control-service"
            or not _uid(metadata["uid"])
            or metadata["generation"] < 1
            or type(count) is not int
            or not 1 <= count <= 16
            or spec.get("replicas", 1) != count
            or deployment["status"]["observedGeneration"] < metadata["generation"]
            or not labels
            or spec["selector"].get("matchExpressions")
        ):
            fail()
        owned = {
            rs["metadata"]["uid"]
            for rs in replica_sets["items"]
            if rs["metadata"]["namespace"] == namespace
            and any(
                owner.get("controller") is True
                and owner["kind"] == "Deployment"
                and owner["uid"] == metadata["uid"]
                for owner in rs["metadata"].get("ownerReferences", [])
            )
        }
        selected = []
        for pod in pods["items"]:
            identity = pod["metadata"]
            if identity["namespace"] != namespace or any(
                identity.get("labels", {}).get(k) != v for k, v in labels.items()
            ):
                continue
            if pod.get("status", {}).get("phase") in {"Succeeded", "Failed"}:
                continue
            if (
                identity.get("deletionTimestamp")
                or pod.get("status", {}).get("phase") != "Running"
            ):
                fail()
            if not any(
                c.get("type") == "Ready" and c.get("status") == "True"
                for c in pod["status"].get("conditions", [])
            ):
                fail()
            if not any(
                o.get("controller") is True
                and o["kind"] == "ReplicaSet"
                and o["uid"] in owned
                for o in identity.get("ownerReferences", [])
            ):
                fail()
            if not _uid(identity["uid"]):
                fail()
            ipaddress.ip_address(pod["status"]["podIP"])
            selected.append(pod)
        if (
            len(selected) != count
            or len({p["metadata"]["uid"] for p in selected}) != count
        ):
            fail()
        return sorted(selected, key=lambda p: p["metadata"]["uid"])
    except (KeyError, TypeError, ValueError):
        fail()


def collect(source, read_kubernetes, read_peer, clock=None):
    """Only external Kubernetes/HTTPS reads are replaceable in isolated fixtures."""
    current_time = clock or (lambda: dt.datetime.now(dt.timezone.utc))
    initial = read_kubernetes()
    selected = roster(*initial, source)
    observations = []
    instances = set()
    keys = {
        "podName",
        "podUid",
        "namespace",
        "instanceId",
        "desiredRevision",
        "configurationSha256",
        "encryptedBundleSha256",
        "observedAt",
    }
    for pod in selected:
        observed = read_peer(pod)
        try:
            if (
                set(observed) != keys
                or observed["podUid"] != pod["metadata"]["uid"]
                or observed["podName"] != pod["metadata"]["name"]
            ):
                fail()
            if (
                observed["namespace"] != source["installation"]["namespace"]
                or not _uid(observed["instanceId"])
                or observed["instanceId"] in instances
            ):
                fail()
            instances.add(observed["instanceId"])
            for field in (
                "desiredRevision",
                "configurationSha256",
                "encryptedBundleSha256",
            ):
                if observed[field] != source[field]:
                    fail()
            timestamp = dt.datetime.fromisoformat(
                observed["observedAt"].replace("Z", "+00:00")
            )
            elapsed = (current_time() - timestamp).total_seconds()
            if timestamp.tzinfo is None or not 0 <= elapsed <= 30:
                fail()
        except (KeyError, TypeError, ValueError, AttributeError):
            fail()
        observations.append(observed)
    final = read_kubernetes()
    final_selected = roster(*final, source)

    def fence(objects, selected_pods):
        deployment = objects[0]
        return (
            deployment["metadata"]["uid"],
            deployment["metadata"]["generation"],
            deployment["spec"],
            [
                (
                    p["metadata"]["uid"],
                    p["status"]["podIP"],
                    p["status"].get("containerStatuses"),
                )
                for p in selected_pods
            ],
        )

    if fence(initial, selected) != fence(final, final_selected):
        fail()
    # The original report receiver also checks its own current boot instance.
    return {
        "deploymentUid": initial[0]["metadata"]["uid"],
        "deploymentGeneration": initial[0]["metadata"]["generation"],
        "expectedReplicas": source["expectedControlReplicas"],
        "namespace": source["installation"]["namespace"],
        "releaseName": source["installation"]["releaseName"],
        "replicas": observations,
    }


class PodConnection(http.client.HTTPSConnection):
    """Use the roster IP for transport and the existing service DNS identity for TLS."""

    def __init__(self, address, hostname, port, context):
        super().__init__(hostname, port, timeout=5, context=context)
        self.address = address

    def connect(self):
        connection = socket.create_connection((self.address, self.port), self.timeout)
        try:
            self.sock = self._context.wrap_socket(connection, server_hostname=self.host)
        except BaseException:
            connection.close()
            raise


def peer_reader(token, ca_file, hostname, port):
    context = ssl.create_default_context(cafile=str(ca_file))

    def read(pod):
        connection = PodConnection(pod["status"]["podIP"], hostname, port, context)
        try:
            connection.request(
                "GET",
                "/internal/v1/platform-configuration/loaded",
                headers={"Authorization": f"Bearer {token}"},
            )
            response = connection.getresponse()
            payload = response.read(65537)
            if response.status != 200 or len(payload) > 65536:
                fail()
            return json.loads(payload)
        finally:
            connection.close()

    return read


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-file", type=Path, required=True)
    parser.add_argument("--token-file", type=Path, required=True)
    parser.add_argument("--ca-file", type=Path, required=True)
    parser.add_argument("--tls-server-name", required=True)
    parser.add_argument("--port", type=int, default=9444)
    parser.add_argument("--output", type=Path, required=True)
    arguments = parser.parse_args()
    try:
        for key in ("LW_BUNDLE_ROOT", "LW_BUNDLE_RENDERED_ROOT"):
            if os.environ.get(key) and arguments.output.absolute().is_relative_to(
                Path(os.environ[key]).absolute().parent
            ):
                fail()
        source = json.loads(bundle._private_file(arguments.source_file))
        namespace = source["installation"]["namespace"]
        if (
            not bundle.objects.DNS_LABEL.fullmatch(namespace)
            or arguments.tls_server_name
            not in {
                f"control-service.{namespace}.svc",
                f"control-service.{namespace}.svc.cluster.local",
            }
            or not 1 <= arguments.port <= 65535
        ):
            fail()
        token = bundle._private_file(arguments.token_file).decode().strip()
        if not token or any(c.isspace() for c in token):
            fail()

        def read_kubernetes():
            outputs = []
            for kind in ("deployment/control-service", "replicasets", "pods"):
                result = subprocess.run(
                    ["kubectl", "get", kind, "-n", namespace, "-o", "json"],
                    capture_output=True,
                    timeout=10,
                )
                if result.returncode or len(result.stdout) > 4 * 1024 * 1024:
                    fail()
                outputs.append(json.loads(result.stdout))
            return tuple(outputs)

        observed = collect(
            source,
            read_kubernetes,
            peer_reader(
                token, arguments.ca_file, arguments.tls_server_name, arguments.port
            ),
        )
        bundle._publish_private(
            arguments.output, json.dumps(observed, sort_keys=True).encode()
        )
        return 0
    except (
        ObservationError,
        bundle.BundleError,
        OSError,
        ValueError,
        KeyError,
        TypeError,
        subprocess.SubprocessError,
        http.client.HTTPException,
    ):
        print("LW_CONTROL_REPLICA_OBSERVATION_FAILED", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
