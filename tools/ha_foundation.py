#!/usr/bin/env python3
"""Derive the three-host foundation from Bundle inventory; never maintain another values source."""
from __future__ import annotations

import copy
import json
import re
import sys
from urllib.parse import urlsplit


class HAError(ValueError):
    pass


def require(condition: bool, field: str) -> None:
    if not condition:
        raise HAError(f"LW_BUNDLE_HA_INVALID:platform_foundation_ha.{field}")


def validate_lock(lock: dict) -> None:
    selected = lock.get("high_availability", {})
    require(isinstance(selected, dict), "versions")
    image_pattern = r"[^\s]+@sha256:[0-9a-f]{64}"
    for component in ("rook", "cnpg", "barman"):
        entry = selected.get(component, {})
        require(isinstance(entry, dict), f"versions.{component}")
        require(isinstance(entry.get("image"), str) and bool(re.fullmatch(image_pattern, entry["image"])), f"versions.{component}.image")
        require(isinstance(entry.get("chart_sha256"), str) and bool(re.fullmatch(r"[0-9a-f]{64}", entry["chart_sha256"])), f"versions.{component}.chart_sha256")
        for key in ("chart", "version"):
            require(isinstance(entry.get(key), str) and bool(entry[key]), f"versions.{component}.{key}")
        https_endpoint(entry.get("chart_url", ""), f"versions.{component}.chart_url")
    for component, key in (("ceph", "image"), ("cnpg", "postgres_image"), ("barman", "sidecar_image")):
        entry = selected.get(component, {})
        require(isinstance(entry, dict), f"versions.{component}")
        require(isinstance(entry.get(key), str) and bool(re.fullmatch(image_pattern, entry[key])), f"versions.{component}.{key}")
    csi = selected.get("ceph_csi", {})
    require(isinstance(csi, dict), "versions.ceph_csi")
    images = csi.get("images", {})
    require(isinstance(images, dict) and set(images) == {"cephcsi", "registrar", "provisioner", "attacher", "resizer", "snapshotter"}, "versions.ceph_csi.images")
    require(all(isinstance(value, str) and bool(re.fullmatch(image_pattern, value)) for value in images.values()), "versions.ceph_csi.images")


def https_endpoint(value: str, field: str):
    require(isinstance(value, str), field)
    try:
        endpoint = urlsplit(value)
        port = endpoint.port
    except ValueError as error:
        raise HAError(f"LW_BUNDLE_HA_INVALID:platform_foundation_ha.{field}") from error
    require(endpoint.scheme == "https" and bool(endpoint.hostname)
            and endpoint.username is None and endpoint.password is None
            and not endpoint.query and not endpoint.fragment
            and (port is None or port > 0), field)
    return endpoint


def validate(raw: dict) -> dict:
    require(isinstance(raw, dict), "profile")
    config = copy.deepcopy(raw)
    require(set(config) <= {"nodes", "failureDomain", "productionObjectEndpoint", "backup", "postgresStorage", "natsStorage"}, "profile")
    nodes = config.get("nodes")
    require(isinstance(nodes, list) and len(nodes) == 3, "nodes")
    names = set()
    for node in nodes:
        require(isinstance(node, dict) and set(node) == {"name", "devices"}, "nodes")
        require(isinstance(node["name"], str) and bool(re.fullmatch(r"[a-z0-9][a-z0-9.-]{0,252}", node["name"])), "nodes.name")
        require(node["name"] not in names, "nodes.name")
        names.add(node["name"])
        require(isinstance(node["devices"], list) and bool(node["devices"]), "nodes.devices")
        require(all(isinstance(device, str) and bool(re.fullmatch(r"/dev/disk/by-id/[A-Za-z0-9_.:-]+", device)) for device in node["devices"]), "nodes.devices")
    require(isinstance(config.get("failureDomain"), str) and bool(config["failureDomain"]), "failureDomain")
    production = https_endpoint(config.get("productionObjectEndpoint", ""), "productionObjectEndpoint")
    backup = config.setdefault("backup", {})
    require(isinstance(backup, dict), "backup")
    allowed = {"endpoint", "destination", "failureDomain", "credentialsSecret", "caSecret", "archiveTimeoutSeconds", "baseBackupSchedule", "retentionDays", "userDataSchedule", "userDataRetentionDays"}
    require(not set(backup) - allowed, "backup")
    require(backup.get("credentialsSecret") == "independent-backup-secrets", "backup.credentialsSecret")
    require(backup.get("caSecret") == "independent-backup-secrets", "backup.caSecret")
    endpoint = https_endpoint(backup.get("endpoint", ""), "backup.endpoint")
    require(endpoint.hostname != production.hostname, "backup.endpoint")
    require(isinstance(backup.get("failureDomain"), str) and bool(backup["failureDomain"]) and backup["failureDomain"] != config["failureDomain"], "backup.failureDomain")
    require(isinstance(backup.get("destination"), str) and bool(re.fullmatch(r"s3://[a-z0-9][a-z0-9.-]+/[A-Za-z0-9_./-]*", backup["destination"])), "backup.destination")
    for field in ("credentialsSecret", "caSecret"):
        require(isinstance(backup.get(field), str) and bool(re.fullmatch(r"[a-z0-9][a-z0-9-]{0,62}", backup[field])), f"backup.{field}")
    backup.setdefault("archiveTimeoutSeconds", 300)
    require(type(backup["archiveTimeoutSeconds"]) is int and 1 <= backup["archiveTimeoutSeconds"] <= 300, "backup.archiveTimeoutSeconds")
    # Six-field CNPG cron: second, minute, hour, day, month, weekday. Restrict to
    # daily schedules rather than accepting a cron that quietly exceeds the RPO.
    backup.setdefault("baseBackupSchedule", "0 0 2 * * *")
    backup.setdefault("userDataSchedule", "0 0 3 * * *")
    for field in ("baseBackupSchedule", "userDataSchedule"):
        require(isinstance(backup[field], str) and bool(re.fullmatch(r"[0-5]?\d [0-5]?\d (?:[01]?\d|2[0-3]) \* \* \*", backup[field])), f"backup.{field}")
    for field in ("retentionDays", "userDataRetentionDays"):
        backup.setdefault(field, 30)
        require(type(backup[field]) is int and backup[field] >= 7, f"backup.{field}")
    for field, default in (("postgresStorage", "100Gi"), ("natsStorage", "20Gi")):
        config.setdefault(field, default)
        require(isinstance(config[field], str) and bool(re.fullmatch(r"[1-9][0-9]*(Gi|Ti)", config[field])), field)
    return config


def validate_storage(providers: list[dict]) -> None:
    for index, provider in enumerate(providers):
        field = f"providers.{index}"
        if provider.get("providerKind") == "container":
            require(provider.get("workspaceStorageClassName") == "ceph-rbd", field + ".workspaceStorageClassName")
            require(provider.get("workspaceAccessMode") == "ReadWriteOnce", field + ".workspaceAccessMode")
        elif provider.get("providerKind") == "kubevirt":
            disks = provider.get("baseDisks", [])
            require(isinstance(disks, list) and all(isinstance(disk, dict) for disk in disks), field + ".baseDisks")
            for disk_index, disk in enumerate(disks):
                require(disk.get("storageClassName") == "ceph-rbd", f"{field}.baseDisks.{disk_index}.storageClassName")
            if provider.get("runtimeVmBase") is not None:
                require(isinstance(provider["runtimeVmBase"], dict), field + ".runtimeVmBase")
                require(provider["runtimeVmBase"].get("storageClassName") == "ceph-rbd", field + ".runtimeVmBase.storageClassName")


def chart_values(lock: dict) -> dict:
    validate_lock(lock)
    ha = lock["high_availability"]
    csi = ha["ceph_csi"]
    return {
        "rook": {"image": {"repository": "docker.io/rook/ceph", "tag": ha["rook"]["version"] + "@" + ha["rook"]["image"].split("@", 1)[1]},
                 "csi": {"rookUseCsiOperator": False, "enableCephfsDriver": False, "enableRbdDriver": True, "provisionerReplicas": 3,
                         "enableRBDSnapshotter": False, "enableVolumeGroupSnapshot": False,
                         **{name: {"repository": image.split("@", 1)[0].rsplit(":", 1)[0], "tag": image.split("@", 1)[0].rsplit(":", 1)[1] + "@" + image.split("@", 1)[1]} for name, image in csi["images"].items()}},
                 "allowLoopDevices": False},
        "cnpg": {"image": {"repository": "ghcr.io/cloudnative-pg/cloudnative-pg", "tag": ha["cnpg"]["version"] + "@" + ha["cnpg"]["image"].split("@", 1)[1]}, "replicaCount": 2,
                 "affinity": {"podAntiAffinity": {"requiredDuringSchedulingIgnoredDuringExecution": [{"labelSelector": {"matchLabels": {"app.kubernetes.io/name": "cloudnative-pg"}}, "topologyKey": "kubernetes.io/hostname"}]}},
                 "resources": {"requests": {"cpu": "100m", "memory": "256Mi"}, "limits": {"memory": "512Mi"}}},
        "barman": {"image": {"tag": ha["barman"]["version"] + "@" + ha["barman"]["image"].split("@", 1)[1]},
                   "sidecarImage": {"tag": ha["barman"]["version"] + "@" + ha["barman"]["sidecar_image"].split("@", 1)[1]},
                   "resources": {"requests": {"cpu": "100m", "memory": "128Mi"}, "limits": {"memory": "512Mi"}}},
    }


def resources(raw: dict, lock: dict, foundation: list[dict], application_namespace: str,
              resource_namespace: str | None = None, database_name: str = "labweaver",
              admin_role: str = "postgres-admin") -> list[dict]:
    config = validate(raw)
    validate_lock(lock)
    require(bool(re.fullmatch(r"[a-z][a-z0-9_]{0,62}", database_name)), "postgresDatabase")
    require(bool(re.fullmatch(r"[a-z][a-z0-9-]{0,62}", admin_role)), "postgresAdminRole")
    ha = lock["high_availability"]
    backup = config["backup"]
    namespace = "labweaver-data"
    def obj(api: str, kind: str, name: str, ns: str, spec: dict) -> dict:
        return {"apiVersion": api, "kind": kind, "metadata": {"name": name, "namespace": ns}, "spec": spec}
    cluster = obj("ceph.rook.io/v1", "CephCluster", "rook-ceph", "rook-ceph", {
        "cephVersion": {"image": ha["ceph"]["image"], "allowUnsupported": False},
        "dataDirHostPath": "/var/lib/rook", "mon": {"count": 3, "allowMultiplePerNode": False},
        "mgr": {"count": 2, "allowMultiplePerNode": False},
        "storage": {"useAllNodes": False, "useAllDevices": False,
                    "nodes": [{"name": node["name"], "devices": [{"name": device} for device in node["devices"]]} for node in config["nodes"]]},
        "network": {"connections": {"requireMsgr2": True, "encryption": {"enabled": True}}},
        "disruptionManagement": {"managePodBudgets": True, "osdMaintenanceTimeout": 30},
        "healthCheck": {"daemonHealth": {"mon": {"interval": "10s"}, "osd": {"interval": "10s"}, "status": {"interval": "10s"}}},
        "resources": {"mon": {"requests": {"cpu": "500m", "memory": "1Gi"}, "limits": {"memory": "2Gi"}},
                      "mgr": {"requests": {"cpu": "500m", "memory": "512Mi"}, "limits": {"memory": "1Gi"}},
                      "osd": {"requests": {"cpu": "1", "memory": "4Gi"}, "limits": {"memory": "4Gi"}}},
        "cleanupPolicy": {"confirmation": "", "allowUninstallWithVolumes": False},
    })
    result = [
        {"apiVersion": "v1", "kind": "ConfigMap", "metadata": {"name": "rook-config-override", "namespace": "rook-ceph"},
         "data": {"config": "[global]\nosd_pool_default_size = 3\nosd_pool_default_min_size = 2\nmon_osd_nearfull_ratio = 0.80\nmon_osd_backfillfull_ratio = 0.85\nmon_osd_full_ratio = 0.90\n"}},
        cluster,
        obj("ceph.rook.io/v1", "CephBlockPool", "work", "rook-ceph", {"failureDomain": "host", "replicated": {"size": 3, "requireSafeReplicaSize": True}, "parameters": {"min_size": "2"}}),
        {"apiVersion": "storage.k8s.io/v1", "kind": "StorageClass", "metadata": {"name": "ceph-rbd"},
         "provisioner": "rook-ceph.rbd.csi.ceph.com", "reclaimPolicy": "Retain", "allowVolumeExpansion": True, "volumeBindingMode": "WaitForFirstConsumer",
         "parameters": {"clusterID": "rook-ceph", "pool": "work", "imageFormat": "2", "imageFeatures": "layering,exclusive-lock,object-map,fast-diff", "csi.storage.k8s.io/fstype": "ext4",
                        "csi.storage.k8s.io/provisioner-secret-name": "rook-csi-rbd-provisioner", "csi.storage.k8s.io/provisioner-secret-namespace": "rook-ceph",
                        "csi.storage.k8s.io/controller-expand-secret-name": "rook-csi-rbd-provisioner", "csi.storage.k8s.io/controller-expand-secret-namespace": "rook-ceph",
                        "csi.storage.k8s.io/node-stage-secret-name": "rook-csi-rbd-node", "csi.storage.k8s.io/node-stage-secret-namespace": "rook-ceph"}},
    ]
    postgres = next(item for item in foundation if item["kind"] == "Secret" and item["metadata"]["name"] == "postgres-secrets")["data"]
    for key in ("backup-access-key", "backup-secret-key", "backup-ca.crt", "replication.crt", "replication.key"):
        require(isinstance(postgres.get(key), str) and bool(postgres[key]), "postgres-secrets." + key)
    import base64
    result.extend([
        {"apiVersion": "v1", "kind": "Secret", "metadata": {"name": "postgres-server-tls", "namespace": namespace, "labels": {"cnpg.io/reload": ""}}, "type": "kubernetes.io/tls",
         "data": {"tls.crt": postgres["tls.crt"], "tls.key": postgres["tls.key"]}},
        {"apiVersion": "v1", "kind": "Secret", "metadata": {"name": "postgres-replication-tls", "namespace": namespace, "labels": {"cnpg.io/reload": ""}}, "type": "kubernetes.io/tls",
         "data": {"tls.crt": postgres["replication.crt"], "tls.key": postgres["replication.key"]}},
        {"apiVersion": "v1", "kind": "Secret", "metadata": {"name": "postgres-ca", "namespace": namespace, "labels": {"cnpg.io/reload": ""}}, "type": "Opaque", "data": {"ca.crt": postgres["ca.crt"]}},
        {"apiVersion": "v1", "kind": "Secret", "metadata": {"name": "independent-backup-secrets", "namespace": namespace}, "type": "Opaque",
         "data": {"access-key": postgres["backup-access-key"], "secret-key": postgres["backup-secret-key"], "ca.crt": postgres["backup-ca.crt"]}},
        {"apiVersion": "v1", "kind": "Secret", "metadata": {"name": "postgres-superuser", "namespace": namespace}, "type": "kubernetes.io/basic-auth",
         "data": {"username": base64.b64encode(b"postgres").decode(), "password": postgres["postgres-password"]}},
        obj("barmancloud.cnpg.io/v1", "ObjectStore", "independent-backup", namespace, {
            "configuration": {"destinationPath": backup["destination"], "endpointURL": backup["endpoint"], "endpointCA": {"name": backup["caSecret"], "key": "ca.crt"},
                              "s3Credentials": {"accessKeyId": {"name": backup["credentialsSecret"], "key": "access-key"}, "secretAccessKey": {"name": backup["credentialsSecret"], "key": "secret-key"}},
                              "wal": {"compression": "gzip"}, "data": {"compression": "gzip"}}, "retentionPolicy": f"{backup['retentionDays']}d"}),
        obj("postgresql.cnpg.io/v1", "Cluster", "postgres", namespace, {
            "instances": 3, "imageName": ha["cnpg"]["postgres_image"], "enableSuperuserAccess": True, "superuserSecret": {"name": "postgres-superuser"},
            "bootstrap": {"initdb": {"database": database_name, "owner": "labweaver"}},
            "certificates": {"serverTLSSecret": "postgres-server-tls", "serverCASecret": "postgres-ca", "clientCASecret": "postgres-ca", "replicationTLSSecret": "postgres-replication-tls"},
            "storage": {"storageClass": "ceph-rbd", "size": config["postgresStorage"]},
            "affinity": {"enablePodAntiAffinity": True, "podAntiAffinityType": "required", "topologyKey": "kubernetes.io/hostname"},
            "enablePDB": True, "smartShutdownTimeout": 0, "failoverDelay": 0,
            "postgresql": {"synchronous": {"method": "any", "number": 1, "dataDurability": "required", "failoverQuorum": True},
                           "pg_hba": [f"hostssl all {admin_role} all cert"],
                           "parameters": {"synchronous_commit": "on", "archive_timeout": str(backup["archiveTimeoutSeconds"]) + "s"}},
            "plugins": [{"name": "barman-cloud.cloudnative-pg.io", "isWALArchiver": True, "parameters": {"barmanObjectName": "independent-backup"}}],
            "resources": {"requests": {"cpu": "1", "memory": "2Gi"}, "limits": {"memory": "4Gi"}},
        }),
        obj("v1", "Service", "postgres", namespace, {"type": "ExternalName", "externalName": "postgres-rw.labweaver-data.svc.cluster.local", "ports": [{"port": 5432}]}),
        obj("postgresql.cnpg.io/v1", "ScheduledBackup", "postgres-daily", namespace, {
            "schedule": backup["baseBackupSchedule"], "backupOwnerReference": "cluster", "cluster": {"name": "postgres"}, "method": "plugin", "pluginConfiguration": {"name": "barman-cloud.cloudnative-pg.io"}}),
        obj("policy/v1", "PodDisruptionBudget", "nats", namespace, {"minAvailable": 2, "selector": {"matchLabels": {"app.kubernetes.io/name": "nats"}}}),
    ])
    result.extend([
        {"apiVersion": "rbac.authorization.k8s.io/v1", "kind": "Role", "metadata": {"name": "storage-health-reader", "namespace": "rook-ceph"}, "rules": [{"apiGroups": ["ceph.rook.io"], "resources": ["cephclusters"], "resourceNames": ["rook-ceph"], "verbs": ["get"]}]},
        {"apiVersion": "rbac.authorization.k8s.io/v1", "kind": "RoleBinding", "metadata": {"name": "storage-health-reader", "namespace": "rook-ceph"}, "roleRef": {"apiGroup": "rbac.authorization.k8s.io", "kind": "Role", "name": "storage-health-reader"},
         "subjects": [{"kind": "ServiceAccount", "name": name, "namespace": application_namespace} for name in ("environment-service", "container-executor", "kubevirt-executor")] + [{"kind": "ServiceAccount", "name": "resource-service", "namespace": resource_namespace or application_namespace}]},
    ])
    result.append({
        "apiVersion": "cilium.io/v2", "kind": "CiliumNetworkPolicy", "metadata": {"name": "cnpg", "namespace": namespace},
        "spec": {
            "endpointSelector": {"matchLabels": {"cnpg.io/cluster": "postgres"}},
            "ingress": [{"fromEndpoints": [{"matchLabels": {"k8s:io.kubernetes.pod.namespace": ns}} for ns in sorted({namespace, application_namespace, resource_namespace or application_namespace, "cnpg-system", "labweaver-authoring"})], "toPorts": [{"ports": [{"port": "5432", "protocol": "TCP"}, {"port": "8000", "protocol": "TCP"}]}]},
                        {"fromEntities": ["host", "remote-node"]}],
            "egress": [{"toEndpoints": [{"matchLabels": {"k8s:io.kubernetes.pod.namespace": namespace}}]},
                       {"toEndpoints": [{"matchLabels": {"k8s:io.kubernetes.pod.namespace": "kube-system", "k8s:k8s-app": "kube-dns"}}], "toPorts": [{"ports": [{"port": "53", "protocol": "ANY"}], "rules": {"dns": [{"matchPattern": "*"}]}}]},
                       {"toEntities": ["kube-apiserver"]},
                       {"toFQDNs": [{"matchName": urlsplit(backup["endpoint"]).hostname}], "toPorts": [{"ports": [{"port": str(urlsplit(backup["endpoint"]).port or 443), "protocol": "TCP"}]}]}],
        },
    })
    return result


def clustered_foundation(foundation: list[dict]) -> list[dict]:
    projected = copy.deepcopy(foundation)
    nats = next(item for item in projected if item["kind"] == "ConfigMap" and item["metadata"]["name"] == "nats-config")
    config = nats["data"]["nats-server.conf"]
    require("server_name: platform-nats" in config and "cluster {" not in config, "natsConfig")
    config = config.replace("server_name: platform-nats", "server_name: $POD_NAME")
    config += '''
cluster {
  name: labweaver
  listen: 0.0.0.0:6222
  routes: [nats://nats-0.nats.labweaver-data.svc:6222, nats://nats-1.nats.labweaver-data.svc:6222, nats://nats-2.nats.labweaver-data.svc:6222]
  tls {
    cert_file: "/etc/nats/tls/tls.crt"
    key_file: "/etc/nats/tls/tls.key"
    ca_file: "/etc/nats/tls/ca.crt"
    verify: true
  }
}
'''
    nats["data"]["nats-server.conf"] = config
    return projected


def main() -> int:
    try:
        value = json.load(sys.stdin)
        print(json.dumps({"objects": resources(value["configuration"], value["lock"], value["foundation"], value["applicationNamespace"], value.get("resourceNamespace"), value["databaseName"], value["adminRole"]), "charts": chart_values(value["lock"]), "foundation": clustered_foundation(value["foundation"])}))
    except (HAError, KeyError, TypeError, ValueError) as error:
        print(str(error) if isinstance(error, HAError) else "LW_BUNDLE_HA_RENDER_INVALID", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
