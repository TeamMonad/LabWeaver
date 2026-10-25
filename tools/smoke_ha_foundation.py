#!/usr/bin/env python3
"""Smoke real CNPG and JetStream on a disposable, explicitly named three-node Kind.

No Ceph/backup/failure recovery claim: Kind uses its test local-path PVCs. All
kubectl/Helm calls use a private kubeconfig; existing clusters are never reused.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

import yaml
from jinja2 import Environment, StrictUndefined

import ha_foundation as ha
import prepare_platform_foundation as foundation

ROOT = Path(__file__).resolve().parents[1]


def run(arguments, *, environment, payload=None, capture=False, timeout=600):
    return subprocess.run(arguments, input=payload, text=True, check=True,
                          env=environment, capture_output=capture, timeout=timeout)


def smoke(name: str, charts: Path) -> None:
    if not re.fullmatch(r"labweaver-ha-[a-z0-9-]+", name):
        raise ValueError("cluster name must start with labweaver-ha-")
    lock = yaml.safe_load((ROOT / "deploy/versions.lock.yml").read_text())
    environment = dict(os.environ)
    existing = run(["kind", "get", "clusters"], environment=environment, capture=True).stdout.splitlines()
    if name in existing:
        raise ValueError("refusing to reuse an existing cluster")
    temporary_root = ROOT / ".tmp"
    temporary_root.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="ha-smoke-", dir=temporary_root) as temporary:
        directory = Path(temporary)
        environment["KUBECONFIG"] = str(directory / "kubeconfig")
        config = directory / "kind.yaml"
        config.write_text(yaml.safe_dump({"kind": "Cluster", "apiVersion": "kind.x-k8s.io/v1alpha4", "nodes": [
            {"role": "control-plane", "kubeadmConfigPatches": ["kind: InitConfiguration\nnodeRegistration:\n  taints: []\n"]},
            {"role": "worker"}, {"role": "worker"},
        ]}))
        created = False
        try:
            run(["kind", "create", "cluster", "--name", name, "--image", lock["test_images"]["kind_node"], "--config", str(config), "--kubeconfig", environment["KUBECONFIG"], "--wait", "180s"], environment=environment)
            created = True
            kubectl = ["kubectl", "--kubeconfig", environment["KUBECONFIG"]]
            # Kind's control-plane taint may also be injected by kubeadm join defaults.
            node = json.loads(run([*kubectl, "get", "node", f"{name}-control-plane", "-o", "json"], environment=environment, capture=True).stdout)
            if any(taint["key"] == "node-role.kubernetes.io/control-plane" for taint in node["spec"].get("taints", [])):
                run([*kubectl, "taint", "nodes", f"{name}-control-plane", "node-role.kubernetes.io/control-plane-"], environment=environment)
            nodes = run(["kind", "get", "nodes", "--name", name], environment=environment, capture=True).stdout.splitlines()
            for image in (lock["high_availability"]["cnpg"]["image"], lock["high_availability"]["cnpg"]["postgres_image"], lock["platform_foundation"]["nats"], lock["platform_foundation"]["nats_box"]):
                for owned_node in nodes:
                    # Follow local_dev.py's digest-preserving CRI pull, not Docker's partial
                    # multi-platform image archive (kind load --all-platforms rejects it).
                    run(["docker", "exec", owned_node, "crictl", "pull", image], environment=environment, timeout=180)
            values = directory / "cnpg.json"
            values.write_text(json.dumps(ha.chart_values(lock)["cnpg"]))
            archive = charts / f"cloudnative-pg-{lock['high_availability']['cnpg']['chart']}.tgz"
            import hashlib
            if hashlib.sha256(archive.read_bytes()).hexdigest() != lock["high_availability"]["cnpg"]["chart_sha256"]:
                raise ValueError("CNPG chart checksum mismatch")
            run(["helm", "upgrade", "--install", "cnpg", str(archive), "--kubeconfig", environment["KUBECONFIG"], "--namespace", "cnpg-system", "--create-namespace", "--values", str(values), "--wait", "--timeout", "300s"], environment=environment)
            private = directory / ".private"
            private.mkdir(mode=0o700)
            material = private / "foundation"
            material.mkdir(mode=0o700)
            foundation.prepare(material, Path(shutil.which("openssl")), Path(shutil.which("ssh-keygen")), Path(shutil.which("nsc")), 30, None)
            from render_platform_bundle import render_bundle
            objects = list(yaml.safe_load_all(render_bundle(ROOT / "deploy/config/platform-foundation-bundle-manifest.json", material / "render-input")))
            projected = ha.clustered_foundation(objects)
            # The disposable component smoke uses standard local-path, not a fictitious RBD
            # driver. Production rendering is tested separately with the unmodified profile.
            cluster = {"apiVersion": "postgresql.cnpg.io/v1", "kind": "Cluster", "metadata": {"name": "ha-smoke", "namespace": "labweaver-data"}, "spec": {
                "instances": 3, "imageName": lock["high_availability"]["cnpg"]["postgres_image"],
                "storage": {"storageClass": "standard", "size": "1Gi"},
                "affinity": {"enablePodAntiAffinity": True, "podAntiAffinityType": "required"},
                "postgresql": {"synchronous": {"method": "any", "number": 1, "dataDurability": "required", "failoverQuorum": True}},
                "enablePDB": True,
            }}
            run([*kubectl, "create", "namespace", "labweaver-data"], environment=environment)
            run([*kubectl, "apply", "-f", "-"], environment=environment, payload=yaml.safe_dump_all([cluster, *projected]))
            role = ROOT / "deploy/ansible/roles/platform_foundation"
            variables = yaml.safe_load((role / "defaults/main.yml").read_text())
            variables.update(platform_foundation_ha={"natsStorage": "1Gi"}, platform_foundation_lock=lock,
                             platform_foundation_workload_configuration_sha256={key: "a" * 64 for key in ("postgres", "nats", "minio")})
            workloads = list(yaml.safe_load_all(Environment(undefined=StrictUndefined).from_string((role / "templates/workloads.yml.j2").read_text()).render(**variables)))
            nats = [obj for obj in workloads if obj and obj["kind"] in {"StatefulSet", "Service"} and obj["metadata"]["name"] == "nats"]
            for obj in nats:
                if obj["kind"] == "StatefulSet":
                    obj["spec"]["volumeClaimTemplates"][0]["spec"]["storageClassName"] = "standard"
            run([*kubectl, "apply", "-f", "-"], environment=environment, payload=yaml.safe_dump_all(nats))
            run([*kubectl, "wait", "cluster/ha-smoke", "--namespace", "labweaver-data", "--for=condition=Ready", "--timeout=600s"], environment=environment)
            run([*kubectl, "rollout", "status", "statefulset/nats", "--namespace", "labweaver-data", "--timeout=300s"], environment=environment)
            run([*kubectl, "get", "pods", "--namespace", "labweaver-data", "-o", "wide"], environment=environment)
            status = json.loads(run([*kubectl, "get", "cluster/ha-smoke", "--namespace", "labweaver-data", "-o", "json"], environment=environment, capture=True).stdout)["status"]
            if status["readyInstances"] != 3:
                raise ValueError("CNPG did not form three Ready instances")
            write = run([*kubectl, "exec", "--namespace", "labweaver-data", status["currentPrimary"], "--", "psql", "-U", "postgres", "-v", "ON_ERROR_STOP=1", "-Atc", "SHOW synchronous_standby_names; SHOW synchronous_commit; CREATE TABLE public.ha_smoke(value text); INSERT INTO public.ha_smoke VALUES ('confirmed'); SELECT * FROM public.ha_smoke;"], environment=environment, capture=True).stdout
            print(write, end="")
            lines = write.splitlines()
            if len(lines) < 3 or not lines[0].startswith("ANY 1 (") or lines[1] != "on" or lines[-1] != "confirmed":
                raise ValueError("CNPG did not acknowledge the configured synchronous write")
            # A real authenticated JetStream client, with exactly the existing admin JWT and TLS
            # identity. This is not an anonymous or fabricated business principal.
            admin = material / "nats-clients/platform-admin"
            data = {key: (admin / file).read_bytes() for key, file in {"nats.creds": "nats.creds", "ca.crt": "nats-ca.pem", "tls.crt": "nats-client.crt", "tls.key": "nats-client.key"}.items()}
            import base64
            secret = {"apiVersion": "v1", "kind": "Secret", "metadata": {"name": "ha-smoke-admin", "namespace": "labweaver-data"}, "data": {key: base64.b64encode(value).decode() for key, value in data.items()}}
            pod = {"apiVersion": "v1", "kind": "Pod", "metadata": {"name": "ha-smoke-admin", "namespace": "labweaver-data"}, "spec": {"restartPolicy": "Never", "containers": [{"name": "nats", "image": lock["platform_foundation"]["nats_box"], "command": ["sleep", "600"], "volumeMounts": [{"name": "admin", "mountPath": "/admin", "readOnly": True}]}], "volumes": [{"name": "admin", "secret": {"secretName": "ha-smoke-admin"}}]}}
            run([*kubectl, "apply", "-f", "-"], environment=environment, payload=yaml.safe_dump_all([secret, pod]))
            run([*kubectl, "wait", "pod/ha-smoke-admin", "--namespace", "labweaver-data", "--for=condition=Ready", "--timeout=180s"], environment=environment)
            client = [*kubectl, "exec", "--namespace", "labweaver-data", "ha-smoke-admin", "--", "nats", "--server", "tls://nats.labweaver-data.svc:4222", "--creds", "/admin/nats.creds", "--tlsca", "/admin/ca.crt", "--tlscert", "/admin/tls.crt", "--tlskey", "/admin/tls.key"]
            run([*client, "stream", "add", "HA_SMOKE", "--subjects", "labweaver.ha.smoke", "--storage", "file", "--replicas", "3", "--defaults"], environment=environment)
            info = json.loads(run([*client, "stream", "info", "HA_SMOKE", "--json"], environment=environment, capture=True).stdout)
            if info["config"]["num_replicas"] != 3 or len(info["cluster"].get("replicas", [])) != 2:
                raise ValueError("JetStream did not form three replicas")
            print("CNPG: three Ready instances and synchronous write; JetStream: three Ready members and R3 stream. No node failure, Ceph, backup or RPO/RTO verification.")
        except (subprocess.CalledProcessError, subprocess.TimeoutExpired, ValueError):
            if created:
                subprocess.run(["kubectl", "--kubeconfig", environment["KUBECONFIG"], "get", "pods", "-A", "-o", "wide"], env=environment, check=False)
                subprocess.run(["kubectl", "--kubeconfig", environment["KUBECONFIG"], "get", "events", "-A", "--sort-by=.metadata.creationTimestamp"], env=environment, check=False)
            raise
        finally:
            if created:
                run(["kind", "delete", "cluster", "--name", name, "--kubeconfig", environment["KUBECONFIG"]], environment=environment)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--name", default="labweaver-ha-smoke")
    parser.add_argument("--charts", type=Path, required=True)
    arguments = parser.parse_args()
    smoke(arguments.name, arguments.charts.resolve())
