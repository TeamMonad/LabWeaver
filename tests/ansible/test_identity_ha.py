"""Render the identity role and validate real Bundle boundaries, without database mocks."""
import copy
import json
from pathlib import Path
import sys
import unittest
from urllib.parse import urlsplit

from jinja2 import Environment, StrictUndefined
import yaml

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))
import ha_foundation as ha
import test_bundle_infrastructure as fixtures

ROLE = ROOT / "deploy/ansible/roles/identity_foundation"


def configuration():
    return {"failureDomain": "production-a", "productionObjectEndpoint": "https://objects.example.invalid",
            "backup": {"endpoint": "https://backup.example.invalid", "destination": "s3://identity-backups/keycloak/",
                       "failureDomain": "backup-b", "credentialsSecret": "identity-backup-secrets", "caSecret": "identity-backup-secrets"}}


def render(template, context):
    environment = Environment(undefined=StrictUndefined)
    environment.filters["to_json"] = json.dumps
    environment.filters["urlsplit"] = lambda value, field: getattr(urlsplit(value), field)
    return list(yaml.safe_load_all(environment.from_string((ROLE / "templates" / template).read_text()).render(**context)))


class IdentityHATests(unittest.TestCase):
    def setUp(self):
        lock = yaml.safe_load((ROOT / "deploy/versions.lock.yml").read_text())
        self.context = yaml.safe_load((ROLE / "defaults/main.yml").read_text())
        self.context.update(identity_foundation_ha=ha.validate_identity(configuration()), identity_lock=lock["identity_foundation"], identity_ha_lock=lock["high_availability"])

    def test_independent_three_instance_database_and_daily_wal_backup(self):
        objects = render("database.yml.j2", self.context)
        cluster = next(item for item in objects if item["kind"] == "Cluster")
        self.assertEqual(cluster["metadata"]["name"], "identity-postgres")
        self.assertEqual(cluster["metadata"]["namespace"], self.context["identity_namespace"])
        spec = cluster["spec"]
        self.assertEqual(spec["instances"], 3)
        self.assertEqual(spec["storage"], {"storageClass": "ceph-rbd", "size": "20Gi"})
        self.assertEqual(spec["affinity"]["podAntiAffinityType"], "required")
        self.assertEqual(spec["affinity"]["topologyKey"], "kubernetes.io/hostname")
        self.assertEqual(spec["postgresql"]["synchronous"], {"method": "any", "number": 1, "dataDurability": "required", "failoverQuorum": True})
        self.assertEqual(spec["postgresql"]["parameters"], {"synchronous_commit": "on", "archive_timeout": "300s"})
        self.assertFalse(spec["enableSuperuserAccess"])
        self.assertEqual(spec["bootstrap"]["initdb"], {"database": "keycloak", "owner": "keycloak", "secret": {"name": self.context["identity_database_secret_name"]}})
        self.assertTrue(spec["plugins"][0]["isWALArchiver"])
        store = next(item for item in objects if item["kind"] == "ObjectStore")["spec"]
        self.assertEqual(store["retentionPolicy"], "30d")
        self.assertEqual(store["configuration"]["destinationPath"], configuration()["backup"]["destination"])
        self.assertEqual(store["configuration"]["s3Credentials"]["accessKeyId"]["name"], "identity-backup-secrets")
        schedule = next(item for item in objects if item["kind"] == "ScheduledBackup")["spec"]
        self.assertEqual(schedule["schedule"], "0 0 2 * * *")
        self.assertEqual(schedule["cluster"]["name"], cluster["metadata"]["name"])
        self.assertEqual(schedule["method"], "plugin")

    def test_keycloak_uses_primary_service_authenticated_tls_and_spread(self):
        objects = render("workloads.yml.j2", self.context)
        self.assertFalse(any(item["kind"] == "StatefulSet" for item in objects))
        self.assertFalse(any(item["kind"] == "Service" and item["metadata"]["name"] == "identity-postgres" for item in objects))
        pod = next(item for item in objects if item["kind"] == "Deployment")["spec"]["template"]["spec"]
        environment = {item["name"]: item for item in pod["containers"][0]["env"]}
        self.assertEqual(environment["KC_DB_URL_HOST"]["value"], "identity-postgres-rw")
        self.assertIn("sslmode=verify-full", environment["KC_DB_URL_PROPERTIES"]["value"])
        init = pod["initContainers"][0]
        self.assertIn("psql -h identity-postgres-rw", init["command"][-1])
        self.assertEqual(init["env"][0]["valueFrom"]["secretKeyRef"], environment["KC_DB_PASSWORD"]["valueFrom"]["secretKeyRef"])
        self.assertEqual(pod["volumes"][1]["secret"]["secretName"], "identity-postgres-ca")
        self.assertEqual(pod["affinity"]["podAntiAffinity"]["requiredDuringSchedulingIgnoredDuringExecution"][0]["topologyKey"], "kubernetes.io/hostname")

    def test_database_policy_permits_operator_api_dns_and_only_backup_endpoint(self):
        policies = render("policy.yml.j2", self.context)
        policy = next(item for item in policies if item["metadata"]["name"] == "identity-cnpg")["spec"]
        self.assertEqual(policy["endpointSelector"]["matchLabels"], {"cnpg.io/cluster": "identity-postgres"})
        self.assertIn({"toEntities": ["kube-apiserver"]}, policy["egress"])
        self.assertEqual(policy["egress"][-1]["toFQDNs"], [{"matchName": "backup.example.invalid"}])
        self.assertEqual(policy["egress"][-1]["toPorts"][0]["ports"], [{"port": "443", "protocol": "TCP"}])
        self.assertIn({"matchLabels": {"k8s:io.kubernetes.pod.namespace": "cnpg-system"}}, policy["ingress"][0]["fromEndpoints"])

    def test_cutover_is_non_destructive_and_database_ready_precedes_keycloak(self):
        tasks = yaml.safe_load((ROLE / "tasks/main.yml").read_text())
        block = next(item["block"] for item in tasks if item.get("name") == "Reconcile identity foundation")
        names = [item["name"] for item in block]
        self.assertLess(names.index("Require an explicit identity database data cutover window"), names.index("Apply the independent identity CNPG cluster and backup configuration"))
        self.assertLess(names.index("Wait for the identity database primary and replicas before starting Keycloak"), names.index("Apply Keycloak workloads after the identity database is ready"))
        guard = next(item for item in block if item["name"] == "Require an explicit identity database data cutover window")
        self.assertEqual(guard["ansible.builtin.assert"]["fail_msg"], "IDENTITY_CNPG_CUTOVER_REQUIRED_NO_AUTOMATIC_DATA_DELETION")
        self.assertFalse(any(item.get("kubernetes.core.k8s", {}).get("state") == "absent" for item in block))
        secrets = next(item for item in block if item["name"] == "Install identity database and bootstrap credentials from the root-only locator")
        self.assertTrue(secrets["no_log"])
        self.assertEqual(secrets["loop"][0]["type"], "kubernetes.io/basic-auth")
        self.assertEqual(secrets["loop"][0]["data"]["username"], "keycloak")

    def test_backup_boundaries_and_defaults(self):
        for field, value in (("endpoint", ""), ("endpoint", configuration()["productionObjectEndpoint"]), ("endpoint", "http://backup.invalid"), ("failureDomain", "production-a"), ("retentionDays", 6), ("retentionDays", True), ("archiveTimeoutSeconds", 301), ("archiveTimeoutSeconds", 0), ("baseBackupSchedule", "0 0 2 1 * *"), ("credentialsSecret", "independent-backup-secrets")):
            with self.subTest(field=field, value=value):
                raw = configuration()
                raw["backup"][field] = value
                with self.assertRaisesRegex(ha.HAError, f"identity_foundation_ha.backup.{field}"):
                    ha.validate_identity(raw)
        raw = configuration()
        raw["backup"].update(retentionDays=7, archiveTimeoutSeconds=60, baseBackupSchedule="0 15 1 * * *")
        self.assertEqual(ha.validate_identity(raw)["backup"]["retentionDays"], 7)

    def test_bundle_fails_closed_before_execution_and_preserves_separate_backup(self):
        fixture = fixtures.BundleInfrastructureTests(methodName="runTest")
        fixture.setUp()
        self.addCleanup(fixture.doCleanups)
        source = fixture.fixture
        variables = fixture.inventory["all"]["vars"]
        variables["identity_foundation_enabled"] = True
        fixture.save()
        with self.assertRaisesRegex(fixtures.bundle.BundleError, "identity_foundation_ha"):
            fixtures.bundle.load_bundle(source.root)
        variables["identity_foundation_ha"] = configuration()
        fixture.save()
        with self.assertRaisesRegex(fixtures.bundle.BundleError, "backup_access_key"):
            fixtures.bundle.load_bundle(source.root)
        (source.root / "secrets/identity.yaml").write_text(yaml.safe_dump({"backup_access_key": "external-boundary-key", "backup_secret_key": "external-boundary-secret", "backup_ca_crt": "external-boundary-ca"}))
        (source.root / source.model["versions"]["lockFile"]).write_bytes((ROOT / "deploy/versions.lock.yml").read_bytes())
        fixtures.bundle.load_bundle(source.root)
        original = copy.deepcopy(variables["identity_foundation_ha"])
        del variables["identity_foundation_ha"]["backup"]["endpoint"]
        fixture.save()
        with self.assertRaisesRegex(fixtures.bundle.BundleError, "identity_foundation_ha.backup.endpoint"):
            fixtures.bundle.load_bundle(source.root)
        variables["identity_foundation_ha"] = original
        variables["platform_foundation_ha"] = {"backup": {"destination": original["backup"]["destination"] + "nested/"}}
        fixture.save()
        # Exercise the same Bundle boundary directly; the business profile here is
        # intentionally incomplete because this test isolates destination ownership.
        with self.assertRaisesRegex(fixtures.bundle.BundleError, "identity_foundation_ha.backup.destination"):
            fixtures.bundle._identity_ha_configuration(source.model, {"config/inventory.yaml": (source.root / "config/inventory.yaml").read_bytes(), "secrets/identity.yaml": (source.root / "secrets/identity.yaml").read_bytes()})
