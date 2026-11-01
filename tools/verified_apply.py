#!/usr/bin/env python3
"""Execute an administrator's platform configuration intent; never create approval."""
from __future__ import annotations

import argparse
import hashlib
import http.client
import json
import os
from pathlib import Path
import re
import ssl
import subprocess
import sys
import uuid
from urllib.parse import urlencode, urlsplit

import deployment_bundle as bundle
import observe_control_configuration as observer

CLIENT = "labweaver-deployment-executor"
PERMISSION = "control.platform_configuration.apply"
PREFIX = "/internal/v1/platform-configuration"


class ApplyError(Exception):
    """Only stable diagnostics and non-secret stage names cross the CLI boundary."""


def fail(stage, code="LW_PLATFORM_CONFIGURATION_EXECUTOR_FAILED"):
    raise ApplyError(f"{code}:{stage}")


def https_json(url, ca, *, method="GET", payload=None, headers=None):
    parsed = urlsplit(url)
    if parsed.scheme != "https" or not parsed.hostname or parsed.username or parsed.password or parsed.fragment:
        fail("identity", "LW_PLATFORM_CONFIGURATION_EXECUTOR_TLS_REQUIRED")
    connection = http.client.HTTPSConnection(
        parsed.hostname, parsed.port or 443, timeout=10,
        context=ssl.create_default_context(cadata=ca.decode()),
    )
    try:
        connection.request(method, parsed.path + ("?" + parsed.query if parsed.query else ""), payload, headers or {})
        response = connection.getresponse()
        body = response.read(1048577)
        if response.status != 200 or len(body) > 1048576:
            fail("identity")
        return json.loads(body)
    finally:
        connection.close()


def service_token(model, files):
    settings = model["configurationExecutor"]
    if settings["clientId"] != CLIENT:
        fail("identity")
    env = model["application"]["workloadInputs"]["control-service"]["env"]
    issuer = env["LABWEAVER_SERVICE_OIDC_ISSUER"].rstrip("/")
    ca = files[model["application"]["secrets"]["control-service-secrets"]["oidc-ca.pem"]]
    discovery = https_json(issuer + "/.well-known/openid-configuration", ca)
    endpoint = discovery["token_endpoint"]
    if discovery["issuer"] != issuer or urlsplit(endpoint).netloc != urlsplit(issuer).netloc:
        fail("identity")
    secret = files[settings["clientSecretFile"]].decode().strip()
    if not secret:
        fail("identity")
    token = https_json(endpoint, ca, method="POST", payload=urlencode({
        "grant_type": "client_credentials", "client_id": CLIENT,
        "client_secret": secret,
    }), headers={"Content-Type": "application/x-www-form-urlencoded"})
    value = token["access_token"]
    if token.get("token_type", "").lower() != "bearer" or not isinstance(value, str) or not value or any(c.isspace() for c in value):
        fail("identity")
    return value


def kubernetes_reader(namespace):
    def read():
        objects = []
        for kind in ("deployment/control-service", "replicasets", "pods"):
            result = subprocess.run(
                ["kubectl", "get", kind, "-n", namespace, "-o", "json"],
                capture_output=True, timeout=10, check=False,
            )
            if result.returncode or len(result.stdout) > 4 * 1024 * 1024:
                fail("replica-roster")
            objects.append(json.loads(result.stdout))
        return tuple(objects)
    return read


class Control:
    def __init__(self, source, token, ca_file, apply_id):
        self.source, self.token, self.ca_file = source, token, ca_file
        self.apply_id = apply_id
        self.namespace = source["installation"]["namespace"]
        self.hostname = f"control-service.{self.namespace}.svc"
        self.read_kubernetes = kubernetes_reader(self.namespace)

    def request(self, method, path, payload=None, revision=None, key=None):
        # A rollout may change the active roster. Re-read the current Deployment
        # for each call; only verification requires the complete stable roster.
        deployment, replicasets, pods = self.read_kubernetes()
        metadata = deployment["metadata"]
        if (metadata.get("namespace") != self.namespace
                or metadata.get("name") != "control-service"
                or metadata.get("annotations", {}).get("meta.helm.sh/release-name") != self.source["installation"]["releaseName"]
                or metadata.get("annotations", {}).get("meta.helm.sh/release-namespace") != self.namespace):
            fail("release-ownership")
        uid = deployment["metadata"]["uid"]
        owners = {r["metadata"]["uid"] for r in replicasets["items"]
                  if r["metadata"].get("namespace") == self.namespace and any(
                      o.get("controller") is True and o.get("kind") == "Deployment" and o.get("uid") == uid
                      for o in r["metadata"].get("ownerReferences", []))}
        selected = [p for p in pods["items"] if p["metadata"].get("namespace") == self.namespace
                    and p.get("status", {}).get("phase") == "Running"
                    and not p["metadata"].get("deletionTimestamp") and any(
                        o.get("controller") is True and o.get("kind") == "ReplicaSet" and o.get("uid") in owners
                        for o in p["metadata"].get("ownerReferences", []))]
        if not selected:
            fail("control-unreachable")
        headers = {"Authorization": f"Bearer {self.token}", "Content-Type": "application/json"}
        if revision is not None:
            headers["If-Match"] = f'"rev-{revision}"'
        if key is not None:
            headers["Idempotency-Key"] = key
        with observer.pod_forward(self.namespace, selected[0]["metadata"]["name"]) as port:
            connection = observer.PodConnection("127.0.0.1", self.hostname, port, ssl.create_default_context(cafile=str(self.ca_file)))
            try:
                connection.request(method, path, None if payload is None else json.dumps(payload), headers)
                response = connection.getresponse()
                body = response.read(1048577)
                if response.status != 200 or len(body) > 1048576:
                    code = "LW_PLATFORM_CONFIGURATION_EXECUTOR_HTTP_FAILED"
                    try:
                        diagnostic = json.loads(body).get("diagnosticCode")
                        if isinstance(diagnostic, str) and re.fullmatch(r"[A-Z][A-Z0-9_]{1,127}", diagnostic):
                            code = diagnostic
                    except (ValueError, AttributeError):
                        pass
                    fail("control-api", code)
                return json.loads(body)
            finally:
                connection.close()

    def observe(self):
        return observer.collect(self.source, self.read_kubernetes,
                                observer.forwarded_peer_reader(self.token, self.ca_file, self.hostname))


def bound_attempt(status, source):
    attempt = status["latestApply"]
    if (attempt["desiredRevision"] != source["desiredRevision"]
            or attempt["encryptedBundleSha256"] != source["encryptedBundleSha256"]
            or attempt.get("executorClientId") not in (None, CLIENT)):
        fail("intent-binding", "LW_PLATFORM_CONFIGURATION_OBSERVATION_MISMATCH")
    return attempt


def report(control, status, stage, *, observed=None, reason=None):
    attempt = bound_attempt(status, control.source)
    payload = {
        "expectedRevision": attempt["revision"], "stage": stage,
        "failureReason": reason,
        "observedConfigurationSha256": control.source["configurationSha256"] if observed else None,
        "observedEncryptedBundleSha256": control.source["encryptedBundleSha256"] if observed else None,
        "controlReplicaObservations": observed,
    }
    # Exactly the same request has exactly the same key, including after a lost
    # response. A fresh observation has a different key, never an unsafe replay.
    digest = hashlib.sha256(json.dumps(payload, sort_keys=True).encode()).hexdigest()
    return control.request("PUT", f'{PREFIX}/applies/{attempt["id"]}', payload,
                           attempt["revision"], f'verified-apply-{attempt["id"]}-{digest}')


def read_intent(control):
    status = control.request("GET", f"{PREFIX}/applies/{control.apply_id}")
    if status["latestApply"]["id"] != control.apply_id:
        fail("intent-binding")
    return status


def prepare(control):
    status = read_intent(control)
    attempt = bound_attempt(status, control.source)
    if attempt["stage"] == "requested":
        status = report(control, status, "validating")
    if status["latestApply"]["stage"] == "validating":
        status = report(control, status, "applying")
    if status["latestApply"]["stage"] not in ("applying", "verifying", "succeeded"):
        fail("prepare", "LW_PLATFORM_CONFIGURATION_APPLY_CONFLICT")
    return status


def finish(control):
    status = read_intent(control)
    attempt = bound_attempt(status, control.source)
    if attempt["stage"] == "succeeded":
        # Re-entry is not another success report. Confirm the actual live set.
        control.observe()
        return status
    try:
        if attempt["stage"] == "applying":
            status = report(control, status, "verifying")
        if status["latestApply"]["stage"] != "verifying":
            fail("verify", "LW_PLATFORM_CONFIGURATION_APPLY_CONFLICT")
        observed = control.observe()
        return report(control, status, "succeeded", observed=observed)
    except (ApplyError, observer.ObservationError, OSError, ValueError, TypeError, KeyError, http.client.HTTPException, subprocess.SubprocessError) as error:
        # If reporting also fails, leave the attempt unverified rather than
        # pretending the external result is known. Preserve the original stage.
        try:
            report(control, status, "failed", reason="verification_failed")
        except (ApplyError, OSError, ValueError, TypeError, KeyError, http.client.HTTPException, subprocess.SubprocessError):
            pass
        if isinstance(error, ApplyError):
            raise
        raise ApplyError("LW_CONTROL_REPLICA_OBSERVATION_FAILED:verifying") from error


def verify_release(target):
    installation = target.get("installationId")
    if not isinstance(installation, str) or not re.fullmatch(r"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}", installation):
        fail("release-ownership")
    result = subprocess.run([
        "helm", "get", "metadata", target["releaseName"],
        "--namespace", target["namespace"], "--output", "json",
    ], capture_output=True, timeout=15, check=False)
    if result.returncode or len(result.stdout) > 65536:
        fail("release-ownership")
    metadata = json.loads(result.stdout)
    if (metadata.get("name") != target["releaseName"]
            or metadata.get("namespace") != target["namespace"]
            or metadata.get("description") != "labweaver-installation:" + installation):
        fail("release-ownership")


def run(action):
    root = Path(os.environ["LW_BUNDLE_ROOT"])
    rendered = Path(os.environ["LW_BUNDLE_RENDERED_ROOT"])
    model, files = bundle.load_bundle(root, Path(os.environ.get("LW_BUNDLE_MANIFEST_FILE", bundle.DEFAULT_MANIFEST)))
    source = json.loads(bundle._private_file(rendered / "platform-configuration-source.json"))
    ciphertext = Path(os.environ["LW_BUNDLE_CIPHERTEXT_FILE"])
    if ciphertext.resolve() != root.parent / "source.age":
        fail("ciphertext-binding")
    digest = hashlib.sha256()
    count = 0
    with ciphertext.open("rb") as captured:
        while chunk := captured.read(1024 * 1024):
            count += len(chunk)
            if count > bundle.MAX_ARCHIVE_BYTES:
                fail("ciphertext-binding")
            digest.update(chunk)
    if digest.hexdigest() != source["encryptedBundleSha256"]:
        fail("ciphertext-binding")
    verify_release(model["target"])
    ca_file = root / model["application"]["secrets"]["control-service-secrets"]["mtls-ca.pem"]
    apply_id = os.environ["LABWEAVER_PLATFORM_CONFIGURATION_APPLY_ID"]
    if str(uuid.UUID(apply_id)) != apply_id or uuid.UUID(apply_id).int == 0:
        fail("intent-binding")
    control = Control(source, service_token(model, files), ca_file, apply_id)
    if action == "prepare":
        prepare(control)
    elif action == "finish":
        finish(control)
    elif action == "apply":
        status = read_intent(control)
        if bound_attempt(status, source)["stage"] not in ("applying", "verifying", "succeeded"):
            fail("apply", "LW_PLATFORM_CONFIGURATION_APPLY_CONFLICT")
        values = bundle._document(bundle._private_file(rendered / "helm-values.yaml"))
        overlay = {
            "managedControlConfiguration": values["managedControlConfiguration"],
            "platformConfigurationPolicy": values["platformConfigurationPolicy"],
            "workloads": {"control-service": {"env": {
                "LABWEAVER_SERVICE_ALLOWED_CLIENT_IDS": values["workloadInputs"]["control-service"]["env"]["LABWEAVER_SERVICE_ALLOWED_CLIENT_IDS"],
            }}},
        }
        overlay_file = rendered / "verified-control-values.yaml"
        bundle._write(overlay_file, bundle.yaml.safe_dump(overlay).encode())
        result = subprocess.run([
            "helm", "upgrade", source["installation"]["releaseName"],
            str(Path(__file__).resolve().parents[1] / "deploy/helm/labweaver"),
            "--namespace", source["installation"]["namespace"],
            "--description", "labweaver-installation:" + model["target"]["installationId"],
            "--reuse-values", "--values", str(overlay_file), "--wait", "--timeout", "10m",
        ], capture_output=True, timeout=660, check=False)
        if result.returncode:
            fail("apply")
    else:
        status = read_intent(control)
        report(control, status, "failed", reason="external_result_unknown")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("prepare", "apply", "finish", "fail"))
    arguments = parser.parse_args()
    try:
        run(arguments.action)
        return 0
    except (ApplyError, bundle.BundleError, observer.ObservationError, OSError, ValueError, TypeError, KeyError, http.client.HTTPException, subprocess.SubprocessError) as error:
        print(str(error) if isinstance(error, ApplyError) else "LW_PLATFORM_CONFIGURATION_EXECUTOR_FAILED:inputs-or-transport", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
