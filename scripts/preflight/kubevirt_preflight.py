#!/usr/bin/env python3
"""Fail-fast, sanitised VM-01a preflight and bounded E3 verifier.

The E3 mode is intentionally narrow: it accepts only Issue #15, creates a
run-scoped namespace, and deletes that namespace before reporting success.
"""
from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Sequence

ISSUE = "15"
RUN_RE = re.compile(r"^[a-z0-9]([a-z0-9-]{0,30}[a-z0-9])?$")
REDACTIONS = (
    (re.compile(r"(?<![\w.])(?:\d{1,3}\.){3}\d{1,3}(?![\w.])"), "<REDACTED_IP>"),
    (re.compile(r"(?i)(token|secret|password|client-secret)\s*[:=]\s*\S+"), r"\1=<REDACTED>"),
    (re.compile(r"(?i)(/home/|[a-z]:\\users\\)[^\s]+"), "<USER_HOME>"),
)


class VerificationError(RuntimeError):
    """A stable, reportable verification failure."""


def sanitize(value: str) -> str:
    for pattern, replacement in REDACTIONS:
        value = pattern.sub(replacement, value)
    return value


@dataclass
class CommandResult:
    command: list[str]
    code: int
    output: str


@dataclass
class Report:
    mode: str
    run_id: str
    namespace: str
    checks: list[dict[str, object]] = field(default_factory=list)
    cleanup: str = "not-started"
    outcome: str = "failed"

    def record(self, name: str, result: CommandResult, required: bool = True) -> None:
        output = "<structured output omitted>" if result.output.lstrip().startswith(("{", "[")) else sanitize(result.output)[-2000:]
        self.checks.append({"name": name, "required": required, "exit_code": result.code,
                            "output": output})


class Kubectl:
    def __init__(self, report: Report, execute=subprocess.run) -> None:
        self.report, self.execute = report, execute

    def run(self, name: str, args: Sequence[str], required: bool = True, input_text: str | None = None) -> CommandResult:
        command = list(args)
        completed = self.execute(command, input=input_text, text=True, capture_output=True, check=False)
        result = CommandResult(command, completed.returncode, (completed.stdout + completed.stderr).strip())
        self.report.record(name, result, required)
        if required and result.code != 0:
            raise VerificationError(f"VM01A_{name.upper().replace('-', '_')}_FAILED: {sanitize(result.output)[-500:]}")
        return result

    def kubectl(self, name: str, *args: str, required: bool = True, input_text: str | None = None) -> CommandResult:
        return self.run(name, ("kubectl", *args), required, input_text)


def require(value: bool, code: str) -> None:
    if not value:
        raise VerificationError(code)


def resource(kind: str, name: str, namespace: str, run_id: str, spec: dict[str, object]) -> dict[str, object]:
    return {"apiVersion": "v1", "kind": kind, "metadata": {"name": name, "namespace": namespace,
            "labels": {"app.kubernetes.io/managed-by": "labweaver-vm01a", "labweaver.io/run-id": run_id}}, "spec": spec}


def apply(k: Kubectl, name: str, document: dict[str, object]) -> None:
    k.kubectl(name, "apply", "-f", "-", input_text=json.dumps(document))


def wait_ready(k: Kubectl, name: str, namespace: str, timeout: str = "180s") -> None:
    k.kubectl(name, "wait", "--for=condition=Ready", f"pod/{name}", "-n", namespace, f"--timeout={timeout}")


def preflight(k: Kubectl, namespace: str) -> None:
    k.kubectl("api-ready", "get", "--raw=/readyz")
    nodes = k.kubectl("nodes", "get", "nodes", "-o", "json").output
    require('"devices.kubevirt.io/kvm"' in nodes and re.search(r'"devices.kubevirt.io/kvm"\s*:\s*"[1-9]', nodes) is not None,
            "VM01A_KVM_CAPACITY_MISSING")
    k.kubectl("kubevirt", "get", "kubevirt", "-A")
    k.kubectl("cdi", "get", "cdi", "-A")
    classes = k.kubectl("storage-classes", "get", "storageclass", "-o", "json").output
    require('"local-path"' in classes and '"nfs-rwx"' in classes, "VM01A_REQUIRED_STORAGE_CLASS_MISSING")
    vm_permission = k.kubectl("vm-create-permission", "auth", "can-i", "create", "virtualmachines.kubevirt.io", "-n", namespace)
    require(vm_permission.output.strip() == "yes", "VM01A_VM_CREATE_FORBIDDEN")
    pvc_permission = k.kubectl("pvc-create-permission", "auth", "can-i", "create", "persistentvolumeclaims", "-n", namespace)
    require(pvc_permission.output.strip() == "yes", "VM01A_PVC_CREATE_FORBIDDEN")
    gateways = k.kubectl("gateway", "get", "gateway", "-n", "labweaver-demo", "public-gateway", "-o", "json").output
    require('"Programmed"' in gateways and '"True"' in gateways, "VM01A_GATEWAY_NOT_PROGRAMMED")
    k.kubectl("http-route", "get", "httproute", "-n", "labweaver-demo", "web-demo")


def storage_checks(k: Kubectl, namespace: str, run_id: str, worker_a: str, worker_b: str, image: str) -> None:
    for name, storage_class, node in (("rwo", "local-path", worker_a), ("rwx", "nfs-rwx", worker_a)):
        apply(k, f"{name}-pvc", resource("PersistentVolumeClaim", f"{name}-claim", namespace, run_id,
            {"accessModes": ["ReadWriteOnce" if name == "rwo" else "ReadWriteMany"], "storageClassName": storage_class,
             "resources": {"requests": {"storage": "64Mi"}}}))
        pod = resource("Pod", f"{name}-writer", namespace, run_id, {"nodeSelector": {"kubernetes.io/hostname": node},
            "restartPolicy": "Never", "containers": [{"name": "writer", "image": image, "command": ["sh", "-ec", "echo vm01a-${HOSTNAME} > /data/proof; cat /data/proof; sleep 180"],
            "resources": {"requests": {"cpu": "10m", "memory": "32Mi"}, "limits": {"cpu": "100m", "memory": "64Mi"}},
            "volumeMounts": [{"name": "data", "mountPath": "/data"}]}], "volumes": [{"name": "data", "persistentVolumeClaim": {"claimName": f"{name}-claim"}}]})
        apply(k, f"{name}-writer", pod)
        k.kubectl(f"{name}-bound", "wait", "--for=jsonpath={.status.phase}=Bound", f"pvc/{name}-claim", "-n", namespace, "--timeout=180s")
        wait_ready(k, f"{name}-writer", namespace)
        k.kubectl(f"{name}-writer-read", "exec", "-n", namespace, f"pod/{name}-writer", "--", "cat", "/data/proof")
    reader = resource("Pod", "rwx-reader", namespace, run_id, {"nodeSelector": {"kubernetes.io/hostname": worker_b}, "restartPolicy": "Never",
        "containers": [{"name": "reader", "image": image, "command": ["sh", "-ec", "cat /data/proof; sleep 180"], "resources": {"requests": {"cpu": "10m", "memory": "32Mi"}, "limits": {"cpu": "100m", "memory": "64Mi"}}, "volumeMounts": [{"name": "data", "mountPath": "/data"}]}], "volumes": [{"name": "data", "persistentVolumeClaim": {"claimName": "rwx-claim"}}]})
    apply(k, "rwx-reader", reader); wait_ready(k, "rwx-reader", namespace)
    k.kubectl("rwx-cross-worker-read", "exec", "-n", namespace, "pod/rwx-reader", "--", "cat", "/data/proof")


def vm_check(k: Kubectl, namespace: str, run_id: str, image: str) -> None:
    vm = {"apiVersion": "kubevirt.io/v1", "kind": "VirtualMachine", "metadata": {"name": "vm01a", "namespace": namespace, "labels": {"app.kubernetes.io/managed-by": "labweaver-vm01a", "labweaver.io/run-id": run_id}}, "spec": {"runStrategy": "Manual", "template": {"spec": {"domain": {"resources": {"requests": {"memory": "256Mi", "devices.kubevirt.io/kvm": "1"}}, "devices": {"disks": [{"name": "containerdisk", "disk": {"bus": "virtio"}}]}}, "volumes": [{"name": "containerdisk", "containerDisk": {"image": image}}]}}}}
    apply(k, "vm", vm)
    for attempt in ("first", "second"):
        k.run(f"vm-{attempt}-start", ("virtctl", "start", "vm01a", "-n", namespace))
        k.kubectl(f"vm-{attempt}-running", "wait", "--for=jsonpath={.status.phase}=Running", "vmi/vm01a", "-n", namespace, "--timeout=300s")
        k.kubectl(f"vm-{attempt}-kvm", "get", "vmi/vm01a", "-n", namespace, "-o", "json")
        if attempt == "first":
            console = k.run("vm-console", ("timeout", "35", "virtctl", "console", "vm01a", "-n", namespace, "--timeout", "30"), required=False)
            require(console.code in (0, 124) and "Successfully connected" in console.output, "VM01A_VM_CONSOLE_FAILED")
        k.run(f"vm-{attempt}-stop", ("virtctl", "stop", "vm01a", "-n", namespace))
        k.kubectl(f"vm-{attempt}-stopped", "wait", "--for=delete", "vmi/vm01a", "-n", namespace, "--timeout=180s")


def cleanup(k: Kubectl, report: Report) -> None:
    if report.mode != "e3":
        report.cleanup = "not-required"; return
    result = k.kubectl("cleanup", "delete", "namespace", report.namespace, "--wait=true", "--timeout=180s", required=False)
    report.cleanup = "passed" if result.code == 0 else "failed"
    if result.code != 0:
        raise VerificationError("VM01A_CLEANUP_FAILED")


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--mode", choices=("readonly", "e3"), required=True)
    parser.add_argument("--run-id", required=True)
    parser.add_argument("--namespace")
    parser.add_argument("--issue", default=ISSUE)
    parser.add_argument("--workload-image")
    parser.add_argument("--gateway-url")
    parser.add_argument("--gateway-host")
    parser.add_argument("--vm-image", default="quay.io/kubevirt/cirros-container-disk-demo@sha256:e2a45211b1f4a73e40b5356e503786c6dc7b5fb003b5d1d4ffa0a450a3dfdefe")
    parser.add_argument("--evidence", type=Path, required=True)
    args = parser.parse_args(argv)
    namespace = args.namespace or f"labweaver-verify-{args.run_id}"
    report = Report(args.mode, args.run_id, namespace)
    try:
        require(bool(RUN_RE.fullmatch(args.run_id)), "VM01A_INVALID_RUN_ID")
        require(args.issue == ISSUE, "VM01A_UNAUTHORIZED_ISSUE")
        require(namespace == f"labweaver-verify-{args.run_id}", "VM01A_UNAUTHORIZED_NAMESPACE")
        if args.mode == "e3":
            require(bool(args.workload_image and "@sha256:" in args.workload_image), "VM01A_WORKLOAD_IMAGE_DIGEST_REQUIRED")
            require("@sha256:" in args.vm_image, "VM01A_VM_IMAGE_DIGEST_REQUIRED")
            require(bool(args.gateway_url and args.gateway_host), "VM01A_GATEWAY_REQUEST_INPUT_REQUIRED")
        k = Kubectl(report)
        if args.mode == "e3": k.kubectl("namespace-create", "create", "namespace", namespace)
        preflight(k, namespace)
        if args.mode == "e3":
            nodes = k.kubectl("eligible-workers", "get", "nodes", "-l", "kubevirt.io/schedulable=true", "-o", "jsonpath={.items[*].metadata.name}").output.split()
            require(len(nodes) >= 2, "VM01A_TWO_WORKERS_REQUIRED")
            storage_checks(k, namespace, args.run_id, nodes[0], nodes[1], args.workload_image)
            vm_check(k, namespace, args.run_id, args.vm_image)
            k.run("gateway-request", ("curl", "--fail", "--silent", "--show-error", "--header", f"Host: {args.gateway_host}", args.gateway_url))
        report.outcome = "passed"
    except (VerificationError, OSError) as error:
        report.checks.append({"name": "failure", "required": True, "exit_code": 1, "output": sanitize(str(error))})
    finally:
        try: cleanup(Kubectl(report), report)
        except VerificationError as error: report.checks.append({"name": "cleanup", "required": True, "exit_code": 1, "output": str(error)})
        if report.cleanup == "failed": report.outcome = "failed"
        args.evidence.parent.mkdir(parents=True, exist_ok=True)
        args.evidence.write_text(json.dumps(report.__dict__, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return 0 if report.outcome == "passed" and report.cleanup != "failed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
