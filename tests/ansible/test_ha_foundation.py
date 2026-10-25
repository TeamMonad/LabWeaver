"""Render the actual Bundle HA projection and foundation template, without a business mock."""
import base64
import copy
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest

from jinja2 import Environment, StrictUndefined
import yaml

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))
import ha_foundation as ha
import deployment_bundle as bundle


def configuration():
    return {
        "failureDomain": "production-a",
        "productionObjectEndpoint": "https://objects.example.invalid",
        "nodes": [{"name": f"node-{number}", "devices": [f"/dev/disk/by-id/ssd-{number}"]} for number in range(3)],
        "backup": {
            "endpoint": "https://backup.example.invalid",
            "destination": "s3://independent-backup/postgres/",
            "failureDomain": "backup-b",
            "credentialsSecret": "independent-backup-secrets",
            "caSecret": "independent-backup-secrets",
        },
    }


def foundation():
    return [
        {"kind": "Secret", "metadata": {"name": "postgres-secrets"}, "data": {
            key: base64.b64encode(b"external-boundary-test-material").decode()
            for key in ("postgres-password", "backup-access-key", "backup-secret-key", "backup-ca.crt", "replication.crt", "replication.key", "tls.crt", "tls.key", "ca.crt")
        }},
        {"kind": "ConfigMap", "metadata": {"name": "nats-config"}, "data": {"nats-server.conf": "server_name: platform-nats\njetstream {store_dir: /data}\noperator: reviewed-operator\n"}},
    ]


class HAFoundationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.lock = yaml.safe_load((ROOT / "deploy/versions.lock.yml").read_text())

    def objects(self):
        return ha.resources(configuration(), self.lock, foundation(), "application-test", "resource-test")

    def test_ceph_replica_host_quorum_capacity_and_reclaim(self):
        objects = self.objects()
        cluster = next(item for item in objects if item["kind"] == "CephCluster")["spec"]
        self.assertEqual(cluster["mon"], {"count": 3, "allowMultiplePerNode": False})
        self.assertFalse(cluster["storage"]["useAllNodes"])
        self.assertFalse(cluster["storage"]["useAllDevices"])
        self.assertEqual(len(cluster["storage"]["nodes"]), 3)
        self.assertTrue(cluster["disruptionManagement"]["managePodBudgets"])
        pool = next(item for item in objects if item["kind"] == "CephBlockPool")["spec"]
        self.assertEqual(pool["failureDomain"], "host")
        self.assertEqual(pool["replicated"]["size"], 3)
        self.assertEqual(pool["parameters"]["min_size"], "2")
        storage = next(item for item in objects if item["kind"] == "StorageClass")
        self.assertEqual(storage["provisioner"], "rook-ceph.rbd.csi.ceph.com")
        self.assertEqual(storage["reclaimPolicy"], "Retain")
        override = next(item for item in objects if item["kind"] == "ConfigMap")["data"]["config"]
        self.assertIn("mon_osd_nearfull_ratio = 0.80", override)
        self.assertIn("mon_osd_full_ratio = 0.90", override)

    def test_cnpg_requires_durable_sync_quorum_and_independent_wal_target(self):
        objects = self.objects()
        database = next(item for item in objects if item["kind"] == "Cluster")["spec"]
        self.assertEqual(database["instances"], 3)
        self.assertEqual(database["affinity"]["podAntiAffinityType"], "required")
        self.assertTrue(database["enablePDB"])
        self.assertEqual(database["smartShutdownTimeout"], 0)
        self.assertEqual(database["postgresql"]["synchronous"], {"method": "any", "number": 1, "dataDurability": "required", "failoverQuorum": True})
        self.assertEqual(database["postgresql"]["parameters"]["synchronous_commit"], "on")
        self.assertEqual(database["postgresql"]["pg_hba"], ["hostssl all postgres-admin all cert"])
        secrets = {item["metadata"]["name"]: item for item in objects if item["kind"] == "Secret"}
        for field in ("serverTLSSecret", "replicationTLSSecret"):
            self.assertEqual(secrets[database["certificates"][field]]["type"], "kubernetes.io/tls")
        self.assertEqual(database["certificates"]["clientCASecret"], "postgres-ca")
        self.assertEqual(database["postgresql"]["parameters"]["archive_timeout"], "300s")
        self.assertTrue(database["plugins"][0]["isWALArchiver"])
        store = next(item for item in objects if item["kind"] == "ObjectStore")["spec"]
        self.assertEqual(store["configuration"]["endpointURL"], configuration()["backup"]["endpoint"])
        self.assertEqual(store["retentionPolicy"], "30d")
        schedule = next(item for item in objects if item["kind"] == "ScheduledBackup")["spec"]
        self.assertEqual(schedule["schedule"], "0 0 2 * * *")
        self.assertEqual(schedule["method"], "plugin")

    def test_profile_rejects_missing_backup_shared_domain_and_unsafe_devices(self):
        original = configuration()
        for field in ("endpoint", "destination", "failureDomain", "credentialsSecret", "caSecret"):
            invalid = copy.deepcopy(original)
            del invalid["backup"][field]
            with self.subTest(field=field), self.assertRaisesRegex(ha.HAError, re.escape(f"backup.{field}")):
                ha.validate(invalid)
        for path, value in (
            (("backup", "endpoint"), original["productionObjectEndpoint"]),
            (("backup", "failureDomain"), original["failureDomain"]),
            (("backup", "archiveTimeoutSeconds"), 301),
            (("backup", "baseBackupSchedule"), "0 0 2 1 * *"),
            (("backup", "retentionDays"), 6),
            (("backup", "retentionDays"), True),
            (("backup", "retentionDays"), 7.5),
            (("backup", "userDataRetentionDays"), 0),
            (("backup", "userDataRetentionDays"), "30"),
        ):
            invalid = copy.deepcopy(original)
            invalid[path[0]][path[1]] = value
            with self.subTest(path=path), self.assertRaises(ha.HAError):
                ha.validate(invalid)
        invalid = copy.deepcopy(original)
        invalid["nodes"][0]["devices"] = ["/dev/sda"]
        with self.assertRaisesRegex(ha.HAError, "nodes.devices"):
            ha.validate(invalid)
        invalid = copy.deepcopy(original)
        invalid["nodes"][1]["name"] = invalid["nodes"][0]["name"]
        with self.assertRaisesRegex(ha.HAError, "nodes.name"):
            ha.validate(invalid)
        for days in (7, 30, 365):
            valid = copy.deepcopy(original)
            valid["backup"]["retentionDays"] = days
            valid["backup"]["userDataRetentionDays"] = days
            self.assertEqual(ha.validate(valid)["backup"]["retentionDays"], days)

    def test_nats_template_is_three_hosts_with_budget_and_authenticated_routes(self):
        role = ROOT / "deploy/ansible/roles/platform_foundation"
        variables = yaml.safe_load((role / "defaults/main.yml").read_text())
        variables.update(platform_foundation_ha=configuration(), platform_foundation_lock=self.lock,
                         platform_foundation_workload_configuration_sha256={name: "a" * 64 for name in ("postgres", "nats", "minio")})
        rendered = Environment(undefined=StrictUndefined).from_string((role / "templates/workloads.yml.j2").read_text()).render(**variables)
        objects = [item for item in yaml.safe_load_all(rendered) if item]
        self.assertFalse(any(item["kind"] == "StatefulSet" and item["metadata"]["name"] == "postgres" for item in objects))
        nats = next(item for item in objects if item["kind"] == "StatefulSet" and item["metadata"]["name"] == "nats")["spec"]
        self.assertEqual(nats["replicas"], 3)
        self.assertEqual(nats["podManagementPolicy"], "Parallel")
        self.assertEqual(nats["template"]["spec"]["affinity"]["podAntiAffinity"]["requiredDuringSchedulingIgnoredDuringExecution"][0]["topologyKey"], "kubernetes.io/hostname")
        self.assertEqual(nats["volumeClaimTemplates"][0]["spec"]["storageClassName"], "ceph-rbd")
        budget = next(item for item in self.objects() if item["kind"] == "PodDisruptionBudget")
        self.assertEqual(budget["spec"]["minAvailable"], 2)
        projected = ha.clustered_foundation(foundation())
        config = next(item for item in projected if item["kind"] == "ConfigMap")["data"]["nats-server.conf"]
        self.assertIn("server_name: $POD_NAME", config)
        self.assertIn("operator: reviewed-operator", config)
        self.assertIn("verify: true", config)
        for number in range(3):
            self.assertIn(f"nats-{number}.nats.labweaver-data.svc:6222", config)
        self.assertIn("server_name: platform-nats", foundation()[1]["data"]["nats-server.conf"])

    def test_health_rbac_is_read_only_and_uses_both_bundle_targets(self):
        role = next(item for item in self.objects() if item["kind"] == "Role")
        self.assertEqual(role["rules"], [{"apiGroups": ["ceph.rook.io"], "resources": ["cephclusters"], "resourceNames": ["rook-ceph"], "verbs": ["get"]}])
        binding = next(item for item in self.objects() if item["kind"] == "RoleBinding")
        self.assertIn({"kind": "ServiceAccount", "name": "resource-service", "namespace": "resource-test"}, binding["subjects"])
        model = {"infrastructure": {"inventoryFile": "config/inventory.yaml"}}
        files = {"config/inventory.yaml": yaml.safe_dump({"all": {"vars": {"platform_foundation_ha": configuration()}}}).encode()}
        inputs = {name: {"env": {}, "secretEnv": {}} for name in ("environment-service", "resource-service", "container-executor", "kubevirt-executor")}
        projected = bundle._ha_workload_inputs(model, files, inputs)
        for value in projected.values():
            self.assertTrue(value["env"]["LABWEAVER_STORAGE_HEALTH_CEPH_URL"].endswith("/cephclusters/rook-ceph"))
        self.assertEqual(inputs["resource-service"]["env"], {})
        projected["resource-service"]["env"]["LABWEAVER_STORAGE_HEALTH_CEPH_URL"] = "https://wrong.invalid/"
        with self.assertRaisesRegex(bundle.BundleError, "HEALTH_OVERRIDE_FORBIDDEN"):
            bundle._ha_workload_inputs(model, files, projected)

    def test_encrypted_bundle_derives_health_identity_without_another_values_source(self):
        import test_bundle_infrastructure as fixtures
        from ansible.parsing.dataloader import DataLoader
        fixture = fixtures.BundleInfrastructureTests(methodName="runTest")
        fixture.setUp()
        try:
            source = fixture.fixture
            fixture.inventory["all"]["vars"]["platform_foundation_ha"] = configuration()
            source.model["application"]["helmValues"]["network"] = {
                "kubernetesApiCidrs": ["192.0.2.7/32"], "kubernetesApiPorts": [443, 6443],
            }
            (source.root / source.model["versions"]["lockFile"]).write_text(yaml.safe_dump(self.lock))
            fixture.save()
            provider_path = source.root / source.model["application"]["configMaps"]["environment-service-config"]["providers.json"]
            providers = yaml.safe_load(provider_path.read_text())
            for provider in providers:
                if provider["providerKind"] == "container":
                    provider.update(workspaceStorageClassName="ceph-rbd", workspaceAccessMode="ReadWriteOnce")
            provider_path.write_text(json.dumps(providers))
            source.seal()
            with bundle.opened_bundle(source.archive, source.identity, source.runtime) as (_, derived, _):
                values = yaml.safe_load((derived / "helm-values.yaml").read_text())
                for name in ("environment-service", "container-executor", "kubevirt-executor"):
                    self.assertTrue(values["workloads"][name]["kubernetesApi"])
                    self.assertIn("LABWEAVER_STORAGE_HEALTH_CEPH_URL", values["workloadInputs"][name]["env"])
                inventory = DataLoader().load((derived / "infrastructure/inventory.yaml").read_text())
                self.assertEqual(inventory["all"]["vars"]["platform_foundation_application_namespace"], source.model["target"]["namespace"])
                self.assertEqual(inventory["all"]["vars"]["platform_foundation_ha"]["backup"]["endpoint"], configuration()["backup"]["endpoint"])
                subprocess.run(["helm", "template", source.model["target"]["releaseName"], str(ROOT / "deploy/helm/labweaver"), "--namespace", source.model["target"]["namespace"], "--values", str(derived / "helm-values.yaml")], check=True, capture_output=True, text=True)
            del fixture.inventory["all"]["vars"]["platform_foundation_ha"]["backup"]["endpoint"]
            fixture.save()
            with self.assertRaisesRegex(bundle.BundleError, "backup.endpoint"):
                bundle.load_bundle(source.root)
        finally:
            fixture.doCleanups()

    def test_ha_health_does_not_authorize_unrelated_or_multiwriter_storage(self):
        good = {"providerKind": "container", "workspaceStorageClassName": "ceph-rbd", "workspaceAccessMode": "ReadWriteOnce"}
        ha.validate_storage([good])
        for field, value in (("workspaceStorageClassName", "nfs-rwx"), ("workspaceAccessMode", "ReadWriteMany")):
            with self.subTest(field=field), self.assertRaisesRegex(ha.HAError, field):
                ha.validate_storage([dict(good, **{field: value})])
        with self.assertRaisesRegex(ha.HAError, "baseDisks.0.storageClassName"):
            ha.validate_storage([{"providerKind": "kubevirt", "baseDisks": [{"storageClassName": "local-path"}]}])

    def test_every_ha_operator_image_and_chart_has_immutable_identity(self):
        for key in ("rook", "cnpg", "barman"):
            entry = self.lock["high_availability"][key]
            self.assertRegex(entry["chart_sha256"], r"^[a-f0-9]{64}$")
            self.assertRegex(entry["image"], r"@sha256:[a-f0-9]{64}$")
            self.assertTrue(entry["source"].startswith("https://"))
        for image in self.lock["high_availability"]["ceph_csi"]["images"].values():
            self.assertRegex(image, r"@sha256:[a-f0-9]{64}$")

    @unittest.skipUnless(os.environ.get("LABWEAVER_HA_CHART_DIRECTORY"), "set LABWEAVER_HA_CHART_DIRECTORY to checksum-locked chart archives for upstream Helm rendering")
    def test_actual_pinned_operator_charts_render_digest_images(self):
        chart_directory = Path(os.environ["LABWEAVER_HA_CHART_DIRECTORY"])
        values = ha.chart_values(self.lock)
        for key, name in (("rook", "rook-ceph"), ("cnpg", "cloudnative-pg"), ("barman", "plugin-barman-cloud")):
            with self.subTest(chart=key), tempfile.TemporaryDirectory() as temporary:
                entry = self.lock["high_availability"][key]
                archive = chart_directory / f"{name}-{entry['chart']}.tgz"
                import hashlib
                self.assertEqual(hashlib.sha256(archive.read_bytes()).hexdigest(), entry["chart_sha256"])
                path = Path(temporary) / "values.json"
                path.write_text(json.dumps(values[key]))
                output = subprocess.run(["helm", "template", name, str(archive), "--namespace", "rook-ceph" if key == "rook" else "cnpg-system", "--values", str(path)], check=True, capture_output=True, text=True).stdout
                images = []
                def collect(value):
                    if isinstance(value, dict):
                        images.extend([value["image"]] if isinstance(value.get("image"), str) else [])
                        for child in value.values():
                            collect(child)
                    elif isinstance(value, list):
                        for child in value:
                            collect(child)
                for document in yaml.safe_load_all(output):
                    collect(document)
                self.assertTrue(images)
                self.assertTrue(all("@sha256:" in image for image in images), images)


if __name__ == "__main__":
    unittest.main()
