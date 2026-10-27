"""Render the identity role and validate real Bundle boundaries, without database mocks."""
import copy
import json
import re
from pathlib import Path
import sys
import subprocess
import tempfile
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
    environment.filters["bool"] = lambda value: value is True or str(value).lower() == "true"
    return list(yaml.safe_load_all(environment.from_string((ROLE / "templates" / template).read_text()).render(**context)))

def missing_runtime_service_grants(values, assignments):
    missing = []
    declared_callers = set()
    for workload in values["workloads"].values():
        environment = workload.get("env", {})
        caller = environment.get("LABWEAVER_SERVICE_CLIENT_ID")
        raw_scopes = environment.get("LABWEAVER_SERVICE_SCOPES")
        if not caller or not raw_scopes:
            continue
        declared_callers.add(caller)
        for key, raw_scopes in environment.items():
            if not key.endswith("_SCOPES"):
                continue
            for role in re.split(r"[,\s]+", raw_scopes):
                target = None if key == "LABWEAVER_SERVICE_SCOPES" else environment.get(key.removesuffix("_SCOPES") + "_AUDIENCE")
                if target is None:
                    parts = role.split(".")
                    # These routes are owned by Environment, not the caller
                    # named in the first permission segment.
                    owner = "environment" if len(parts) > 1 and parts[1] == "environment" else parts[0].split(":")[0]
                    target = "labweaver-" + owner
                # The Agent's generic list includes its inbound API role,
                # which no Agent client requests from itself.
                if caller == target == "labweaver-agent" and role == "agent.control.invoke":
                    continue
                if not any(
                    item["caller"] == caller
                    and item["target"] == target
                    and role in item["roles"]
                    for item in assignments
                ):
                    missing.append((caller, target, role))
    return missing, declared_callers


def source_token_requests():
    """Read native literal targets/roles alongside the dynamic Helm scopes."""
    requests = set()
    for source in (ROOT / "services").glob("*-service/src/**/*.rs"):
        caller = "labweaver-" + source.relative_to(ROOT / "services").parts[0].removesuffix("-service")
        text = source.read_text()
        for target, scopes in re.findall(
            r'ServiceTokenTarget\s*\{\s*audience:\s*"([^"]+)",\s*scopes:\s*&\[([^]]+)\]',
            text,
        ):
            for role in re.findall(r'"([^"]+)"', scopes):
                requests.add((caller, target, role))
        groups = re.findall(
            r'const\s+(?:[A-Z_]*REQUIRED_SCOPES?|ENVIRONMENT_FREEZE_SCOPE)\s*:[^=]+=(.*?);',
            text,
            re.DOTALL,
        )
        groups.extend(re.findall(
            r'\.bearer_auth_for\([^;]*?&BTreeSet::from\(\[(.*?)\]\)',
            text,
            re.DOTALL,
        ))
        for group in groups:
            for role in re.findall(r'"([a-z]+[.:][a-z_.:]+)"', group):
                parts = role.split(".")
                owner = "environment" if len(parts) > 1 and parts[1] == "environment" else parts[0].split(":")[0]
                requests.add((caller, "labweaver-" + owner, role))
    return requests


class IdentityHATests(unittest.TestCase):
    def test_local_realm_agent_control_roles_and_scope_mapping_enable_audience(self):
        from test_local_dev import LocalDevBundleTests

        applied = LocalDevBundleTests(methodName="runTest")._render_foundation_objects()
        realm_config = next(
            document for objects in applied for document in objects
            if document.get("metadata", {}).get("name") == "keycloak-realm"
        )
        realm = json.loads(realm_config["data"]["workloads-realm.json"])
        agent = next(client for client in realm["clients"] if client["clientId"] == "labweaver-agent")
        account = next(user for user in realm["users"] if user.get("serviceAccountClientId") == "labweaver-agent")
        self.assertFalse(agent["fullScopeAllowed"])
        self.assertEqual(account["clientRoles"]["labweaver-control"], ["control.agent_run.read"])
        self.assertIn(
            {"client": "labweaver-agent", "roles": ["control.agent_run.read"]},
            realm["clientScopeMappings"]["labweaver-control"],
        )
        self.assertTrue(any(
            mapper["protocolMapper"] == "oidc-audience-resolve-mapper"
            and mapper["config"]["access.token.claim"] == "true"
            for mapper in agent["protocolMappers"]
        ))

    def test_runtime_service_scopes_have_matching_identity_grants_for_all_services(self):
        values = yaml.safe_load((ROOT / "deploy/helm/labweaver/values.yaml").read_text())
        assignments = self.context["identity_service_role_assignments"]
        service_clients = {
            service["client_id"]
            for service in self.context["identity_service_clients"]
        }
        missing, declared_callers = missing_runtime_service_grants(values, assignments)
        self.assertEqual(missing, [])
        self.assertEqual(
            declared_callers,
            {"labweaver-access", "labweaver-agent", "labweaver-control",
             "labweaver-environment", "labweaver-evaluation", "labweaver-resource"},
        )
        self.assertTrue(declared_callers <= service_clients)
        native_requests = source_token_requests()
        self.assertIn(("labweaver-agent", "labweaver-control", "control.agent_run.read"), native_requests)
        for caller, target, role in native_requests:
            self.assertTrue(
                any(item["caller"] == caller and item["target"] == target and role in item["roles"] for item in assignments),
                f"native token request {caller} -> {target}: {role} has no identity grant",
            )
        without_agent_control = [
            item for item in assignments
            if not (item["caller"] == "labweaver-agent" and item["target"] == "labweaver-control")
        ]
        missing_without_grant, _ = missing_runtime_service_grants(values, without_agent_control)
        self.assertIn(
            ("labweaver-agent", "labweaver-control", "control.agent_run.read"),
            missing_without_grant,
        )

    def test_agent_control_grant_is_rendered_for_control_audience(self):
        # Reuse the identity-template regression from 721b0a0c's
        # test_bundle_agent_control.py; the old frozen-tree injector is not
        # suitable for the now-merged native consumer.
        context = dict(
            self.context,
            identity_run_suffix="regression",
            identity_run_id="regression",
            identity_cluster_uid={"stdout": "cluster"},
            identity_secret_locator_sha256="sha256:" + "a" * 64,
        )
        assignment = next(
            item for item in self.context["identity_service_role_assignments"]
            if item["caller"] == "labweaver-agent"
            and item["target"] == "labweaver-control"
        )
        self.assertEqual(assignment["roles"], ["control.agent_run.read"])
        client = next(
            item for item in self.context["identity_service_clients"]
            if item["client_id"] == "labweaver-control"
        )
        self.assertEqual(client["audience"], "labweaver-control")
        manifest = render("deployment-manifest.json.j2", context)[0]
        self.assertIn(assignment, manifest["role_assignments"])
        control_env = yaml.safe_load((ROOT / "deploy/helm/labweaver/values.yaml").read_text())["workloads"]["control-service"]["env"]
        self.assertIn("labweaver-agent", control_env["LABWEAVER_SERVICE_ALLOWED_CLIENT_IDS"].split(","))
        network_policy = (ROOT / "deploy/helm/labweaver/templates/network-policy.yaml").read_text()
        self.assertIn("values: [access-service, agent-service, environment-service, evaluation-service]", network_policy)
        self.assertIn("ports: [{protocol: TCP, port: 9444}]", network_policy)
        agent_client = (ROOT / "services/agent-service/src/model_input_authorization.rs").read_text()
        self.assertIn('"LABWEAVER_CONTROL_SERVICE_AUDIENCE"', agent_client)
        self.assertIn('"control.agent_run.read".to_owned()', agent_client)
        provision_job = render("provision-job.yml.j2", context)[0]
        provision_script = provision_job["spec"]["template"]["spec"]["containers"][0]["args"][0]
        self.assertIn("protocolMapper=oidc-audience-resolve-mapper", provision_script)
        self.assertIn('service-account-labweaver-agent" --cclientid "labweaver-control" --rolename "control.agent_run.read"', provision_script)
        self.assertIn("fullScopeAllowed=false", provision_script)
        self.assertNotIn("fullScopeAllowed=true", provision_script)
        self.assertNotIn("composite=true", provision_script)

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
        # CNPG's CA Secret also holds ca.key. Every application CA volume must
        # use a positive certificate-only projection, including init containers.
        for volume in pod["volumes"]:
            secret = volume.get("secret", {})
            if secret.get("secretName", "").endswith("-ca"):
                self.assertEqual(secret.get("items"), [{"key": "ca.crt", "path": "ca.crt"}])
        for container in [*pod["initContainers"], *pod["containers"]]:
            self.assertFalse(any(item.get("valueFrom", {}).get("secretKeyRef", {}).get("key", "").endswith(".key") for item in container.get("env", [])))
        self.assertEqual(pod["affinity"]["podAntiAffinity"]["requiredDuringSchedulingIgnoredDuringExecution"][0]["topologyKey"], "kubernetes.io/hostname")

    def test_database_policy_permits_operator_api_dns_and_only_backup_endpoint(self):
        policies = render("policy.yml.j2", self.context)
        policy = next(item for item in policies if item["metadata"]["name"] == "identity-cnpg")["spec"]
        self.assertEqual(policy["endpointSelector"]["matchLabels"], {"cnpg.io/cluster": "identity-postgres"})
        self.assertIn({"toEntities": ["kube-apiserver"]}, policy["egress"])
        self.assertEqual(policy["egress"][-1]["toFQDNs"], [{"matchName": "backup.example.invalid"}])
        self.assertEqual(policy["egress"][-1]["toPorts"][0]["ports"], [{"port": "443", "protocol": "TCP"}])
        self.assertIn({"matchLabels": {"k8s:io.kubernetes.pod.namespace": "cnpg-system"}}, policy["ingress"][0]["fromEndpoints"])

    def test_rollout_frees_a_host_without_exhausting_available_replicas(self):
        # The supported two replicas can update on three hosts normally, or
        # on the two remaining schedulable hosts during single-node maintenance.
        for replicas, available_hosts in ((2, 3), (2, 2)):
            with self.subTest(replicas=replicas, available_hosts=available_hosts):
                objects = render("workloads.yml.j2", dict(self.context, identity_keycloak_replicas=replicas))
                deployment = next(item["spec"] for item in objects if item["kind"] == "Deployment")
                budget = deployment["strategy"]["rollingUpdate"]
                survivors = deployment["replicas"] - budget["maxUnavailable"]
                self.assertGreater(min(available_hosts, replicas + budget["maxSurge"]), survivors)
                self.assertGreaterEqual(survivors, 1)
                pdb = next(item["spec"] for item in objects if item["kind"] == "PodDisruptionBudget")
                self.assertGreaterEqual(survivors, pdb["minAvailable"])

    def test_three_desired_replicas_with_two_schedulable_hosts_are_rejected(self):
        tasks = yaml.safe_load((ROLE / "tasks/main.yml").read_text())
        validation = next(task for task in tasks if "ansible.builtin.assert" in task)
        for replicas, available_hosts in ((3, 2), (3, 3), (1, 2), (4, 3), (True, 2), ("2", 2), (2, 2)):
            with self.subTest(replicas=replicas, available_hosts=available_hosts), tempfile.TemporaryDirectory() as directory:
                # Execute the full production input assertion. The key regression
                # retains desired=3 with only two schedulable hosts; it must fail
                # before any Deployment change rather than silently become (2,2).
                context = dict(self.context, identity_foundation_enabled=True, identity_run_id="infra-regression",
                               identity_secret_locator="/private/identity.yaml", identity_secret_locator_sha256="sha256:" + "a" * 64,
                               identity_keycloak_replicas=replicas)
                path = Path(directory) / "replicas.yml"
                path.write_text(yaml.safe_dump([{"hosts": "localhost", "gather_facts": False, "vars": context,
                    "tasks": [validation, {"ansible.builtin.debug": {"msg": "DEPLOYMENT_CHANGE_REACHED"}}]}]))
                result = subprocess.run(["ansible-playbook", "-i", "localhost,", "-c", "local", str(path)], capture_output=True, text=True, timeout=60)
                output = result.stdout + result.stderr
                if type(replicas) is int and replicas == 2:
                    self.assertEqual(result.returncode, 0, output)
                    self.assertIn("DEPLOYMENT_CHANGE_REACHED", output)
                else:
                    self.assertNotEqual(result.returncode, 0, output)
                    self.assertIn("IDENTITY_CONFIGURATION_INVALID", output)
                    self.assertNotIn("DEPLOYMENT_CHANGE_REACHED", output)

    def test_legacy_database_objects_reject_before_any_database_or_keycloak_change(self):
        tasks = yaml.safe_load((ROLE / "tasks/main.yml").read_text())
        block = next(item["block"] for item in tasks if "block" in item)
        guard = next(item for item in block if item.get("ansible.builtin.assert", {}).get("fail_msg") == "IDENTITY_CNPG_CUTOVER_REQUIRED_NO_AUTOMATIC_DATA_DELETION")
        guarded_changes = []
        for task in block:
            if task is guard:
                guarded_changes.append(task)
            else:
                definition = task.get("kubernetes.core.k8s", {}).get("definition", "")
                for template in ("database.yml.j2", "workloads.yml.j2"):
                    if template in definition:
                        # Only replace the external Kubernetes mutation boundary;
                        # execute the real Ansible assert and original task order.
                        guarded_changes.append({"ansible.builtin.debug": {"msg": "CHANGE_REACHED:" + template}})
        self.assertEqual(len(guarded_changes), 3)
        for statefulset, service in ((True, False), (False, True), (False, False)):
            with self.subTest(statefulset=statefulset, service=service), tempfile.TemporaryDirectory() as directory:
                path = Path(directory) / "cutover.yml"
                result_inputs = {"results": [{"resources": [{"kind": "StatefulSet"}] if statefulset else []}, {"resources": [{"kind": "Service"}] if service else []}]}
                path.write_text(yaml.safe_dump([{"hosts": "localhost", "gather_facts": False, "vars": {"identity_legacy_database": result_inputs}, "tasks": guarded_changes}]))
                result = subprocess.run(["ansible-playbook", "-i", "localhost,", "-c", "local", str(path)], capture_output=True, text=True, timeout=60)
                output = result.stdout + result.stderr
                if statefulset or service:
                    self.assertNotEqual(result.returncode, 0, output)
                    self.assertIn("IDENTITY_CNPG_CUTOVER_REQUIRED_NO_AUTOMATIC_DATA_DELETION", output)
                    self.assertNotIn("CHANGE_REACHED:", output)
                else:
                    self.assertEqual(result.returncode, 0, output)
                    self.assertIn("CHANGE_REACHED:database.yml.j2", output)
                    self.assertIn("CHANGE_REACHED:workloads.yml.j2", output)

    def test_database_ready_precedes_keycloak_and_credentials_are_not_logged(self):
        tasks = yaml.safe_load((ROLE / "tasks/main.yml").read_text())
        block = next(item["block"] for item in tasks if "block" in item)
        ready_index = next(index for index, task in enumerate(block) if "cluster/identity-postgres" in task.get("ansible.builtin.command", {}).get("argv", []))
        keycloak_index = next(index for index, task in enumerate(block) if "workloads.yml.j2" in task.get("kubernetes.core.k8s", {}).get("definition", ""))
        self.assertLess(ready_index, keycloak_index)
        self.assertFalse(any(item.get("kubernetes.core.k8s", {}).get("state") == "absent" for item in block))
        secrets = next(item for item in block if any(secret.get("type") == "kubernetes.io/basic-auth" for secret in item.get("loop", []) if isinstance(secret, dict)))
        self.assertTrue(secrets["no_log"])
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
