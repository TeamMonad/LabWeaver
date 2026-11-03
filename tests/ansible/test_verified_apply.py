"""Executor identity and encrypted-input regressions; Control behavior lives in PG tests."""
from contextlib import ExitStack
import tempfile
import json
import subprocess
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

import yaml

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools"))
import verified_apply as executor
import test_deployment_bundle as fixtures
import local_dev


class VerifiedApplyIdentityTests(unittest.TestCase):
    def test_executor_has_only_exact_control_report_role_and_no_browser_delegation(self):
        defaults = yaml.safe_load((ROOT / "deploy/ansible/roles/identity_foundation/defaults/main.yml").read_text())
        client = next(c for c in defaults["identity_service_clients"] if c["client_id"] == executor.CLIENT)
        self.assertEqual(client["secret_key"], "deployment-executor-client-secret")
        assignments = [a for a in defaults["identity_service_role_assignments"] if a["caller"] == executor.CLIENT]
        self.assertEqual(assignments, [{"caller": executor.CLIENT, "target": "labweaver-control", "roles": [executor.PERMISSION]}])
        self.assertFalse(any(a["caller"] == executor.CLIENT for a in defaults["identity_service_realm_role_assignments"]))

    def test_real_local_realm_has_the_same_restricted_executor_account(self):
        from test_local_dev import LocalDevBundleTests
        applied = LocalDevBundleTests(methodName="runTest")._render_foundation_objects()
        realm_object = next(document for objects in applied for document in objects
                            if document.get("metadata", {}).get("name") == "keycloak-realm")
        import json
        realm = json.loads(realm_object["data"]["workloads-realm.json"])
        client = next(c for c in realm["clients"] if c["clientId"] == executor.CLIENT)
        account = next(u for u in realm["users"] if u.get("serviceAccountClientId") == executor.CLIENT)
        self.assertFalse(client["fullScopeAllowed"])
        self.assertFalse(client["standardFlowEnabled"])
        self.assertFalse(client["directAccessGrantsEnabled"])
        self.assertTrue(client["serviceAccountsEnabled"])
        self.assertEqual(account["clientRoles"], {"labweaver-control": [executor.PERMISSION]})
        self.assertIn({"client": executor.CLIENT, "roles": [executor.PERMISSION]}, realm["clientScopeMappings"]["labweaver-control"])

    def test_executor_token_exchange_uses_client_roles_not_oauth_business_scope(self):
        calls = []
        model = {"configurationExecutor": {"clientId": executor.CLIENT, "clientSecretFile": "secrets/executor"},
                 "application": {"workloadInputs": {"control-service": {"env": {"LABWEAVER_SERVICE_OIDC_ISSUER": "https://identity.example/realms/workloads"}}},
                                 "secrets": {"control-service-secrets": {"oidc-ca.pem": "secrets/ca"}}}}
        def identity(url, _ca, **kwargs):
            calls.append((url, kwargs))
            if len(calls) == 1:
                return {"issuer": "https://identity.example/realms/workloads", "token_endpoint": "https://identity.example/realms/workloads/protocol/openid-connect/token"}
            return {"access_token": "external-identity-token", "token_type": "Bearer"}
        with patch.object(executor, "https_json", side_effect=identity):
            self.assertEqual(executor.service_token(model, {"secrets/executor": b"secret", "secrets/ca": b"ca"}), "external-identity-token")
        from urllib.parse import parse_qs
        request = parse_qs(calls[1][1]["payload"])
        self.assertEqual(request, {"client_id": [executor.CLIENT], "client_secret": ["secret"], "grant_type": ["client_credentials"]})


class VerifiedApplyReleaseTests(unittest.TestCase):
    def test_release_metadata_read_failure_and_foreign_installation_fail_closed(self):
        target = {"namespace": "science", "releaseName": "science-platform",
                  "installationId": "019fa9d0-0000-7000-8000-000000000142"}
        metadata = {"name": target["releaseName"], "namespace": target["namespace"],
                    "description": "labweaver-installation:" + target["installationId"]}
        with patch.object(executor.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, json.dumps(metadata).encode(), b"")):
            executor.verify_release(target)
        for result in (
            subprocess.CompletedProcess([], 1, b"", b"synthetic-private-transport-error"),
            subprocess.CompletedProcess([], 0, json.dumps(dict(metadata, description="foreign")).encode(), b""),
            subprocess.CompletedProcess([], 0, json.dumps(dict(metadata, namespace="another")).encode(), b""),
        ):
            with self.subTest(result=result.returncode), patch.object(executor.subprocess, "run", return_value=result):
                with self.assertRaisesRegex(executor.ApplyError, "release-ownership"):
                    executor.verify_release(target)


class VerifiedApplyBundleTests(unittest.TestCase):
    def setUp(self):
        self.fixture = fixtures.DeploymentBundleTests(methodName="runTest")
        self.fixture.setUp()
        self.addCleanup(self.fixture.tearDown)

    def test_authenticated_ciphertext_and_executor_secret_share_the_actual_bundle(self):
        fixture = self.fixture
        fixture.model["configurationExecutor"] = {"clientId": executor.CLIENT, "clientSecretFile": "secrets/executor"}
        fixtures.write(fixture.root / "secrets/executor", b"synthetic-executor-secret")
        fixture.save_model()
        fixtures.bundle.seal_bundle(fixture.root, fixture.archive, [fixture.key()], fixture.runtime)
        with fixtures.bundle.opened_bundle(fixture.archive, fixture.identity, fixture.runtime) as (root, rendered, _metadata):
            self.assertEqual((root.parent / "source.age").read_bytes(), fixture.archive.read_bytes())
            values = yaml.safe_load((rendered / "helm-values.yaml").read_bytes())
            allowed = values["workloadInputs"]["control-service"]["env"]["LABWEAVER_SERVICE_ALLOWED_CLIENT_IDS"].split(",")
            self.assertIn(executor.CLIENT, allowed)
            self.assertNotIn("synthetic-executor-secret", (rendered / "helm-values.yaml").read_text())
        self.assertFalse(any(fixture.runtime.iterdir()))

    def test_local_managed_bundle_uses_actual_application_files_and_saved_version(self):
        from test_local_dev import LocalDevBundleTests, make_app_input_with_local_cidrs
        work = self.fixture.private / "local"
        work.mkdir(mode=0o700)
        foundation = self.fixture.directory / "foundation"
        LocalDevBundleTests._write_foundation_fixture(foundation)
        clients = {name: f"{name}-secret" for name in ("web", "access", "agent", "control", "environment", "evaluation", "resource", "deployment-executor")}
        (work / "client-secrets.json").write_text(json.dumps(clients))
        (work / "database-passwords.json").write_text(json.dumps({name + "-service": name + "-password" for name in ("access", "agent", "control", "environment", "evaluation", "resource")}))
        for name in ("ssh_host_ed25519_key", "target_key", "target_key.pub", "target_key-cert.pub"):
            (work / name).write_bytes(("synthetic-ssh-material-" + fixtures.hashlib.sha256(name.encode()).hexdigest()).encode())
        defaults = yaml.safe_load(fixtures.bundle.WORKLOADS_FILE.read_bytes())
        images = {name: "registry.local/" + name + "@sha256:" + "a" * 64 for name in defaults["images"]}
        images["evaluation_runner"] = "registry.local/runner@sha256:" + "a" * 64
        images["authoring_sandbox"] = "registry.local/sandbox@sha256:" + "a" * 64
        make_app_input_with_local_cidrs(work, foundation, images, local_dev.fixture_provider_environment())
        version = dict(self.fixture.version, createdBy="019fa9d0-0000-7000-8000-000000000142")
        archive, identity, manifest = local_dev.prepare_local_managed_bundle(work, images, version)
        with fixtures.bundle.opened_bundle(archive, identity, work / "bundle-runtime", manifest) as (root, rendered, metadata):
            source = json.loads((rendered / "platform-configuration-source.json").read_bytes())
            model, files = fixtures.bundle.load_bundle(root, manifest)
            self.assertEqual(source["configurationSha256"], self.fixture.version["configurationSha256"])
            self.assertEqual(source["encryptedBundleSha256"], metadata["encryptedBundleSha256"])
            actual = yaml.safe_load(files[model["application"]["configMaps"]["control-service-config"]["config.yaml"]])
            expected = yaml.safe_load((work / "app-input/configmaps/control-service-config/config.yaml").read_bytes())
            expected["nats"].pop("server")
            expected["nats"]["serverFile"] = "/etc/labweaver/secrets/nats-server"
            self.assertEqual(actual, expected)
            objects = list(yaml.safe_load_all((rendered / "application.yaml").read_bytes()))
            endpoints = [obj for obj in objects if obj["kind"] == "Secret" and "nats-server" in obj["data"]]
            self.assertGreaterEqual(len(endpoints), 5)
            self.assertEqual(files[model["configurationExecutor"]["clientSecretFile"]], clients["deployment-executor"].encode())


class LocalUpExecutorTests(unittest.TestCase):
    def test_up_reaches_verified_apply_before_marking_ready(self):
        class ReachedExecutor(Exception):
            pass

        with tempfile.TemporaryDirectory() as temporary, ExitStack() as patches:
            root = Path(temporary)
            state = root / "state"
            for name, value in {"ROOT": root, "STATE_DIR": state, "STATE_FILE": state / "state.json",
                                "PRIVATE_DIR": root / ".private/local-dev"}.items():
                patches.enter_context(patch.object(local_dev, name, value))
            # These replace only local process/Kubernetes/bootstrap boundaries.
            # No Control response or business authorization is manufactured.
            for name in ("need_tools", "require_psutil", "verify_loopback_nip_io", "author_foundation",
                         "preload_images", "configure_local_portal_dns", "start_foundation", "apply_migrations",
                         "start_build_executor_fixture", "bootstrap_evaluation_runner_resources",
                         "bootstrap_authoring_sandbox_resources", "deploy_local_portal", "wait_local_oidc_issuer", "deploy", "cleanup"):
                patches.enter_context(patch.object(local_dev, name))
            patches.enter_context(patch.object(local_dev, "resolve_provider_environment", return_value={}))
            patches.enter_context(patch.object(local_dev, "free_port", return_value="12345"))
            patches.enter_context(patch.object(local_dev, "create_cluster", return_value="192.0.2.1"))
            patches.enter_context(patch.object(local_dev, "prepare_and_build_images", return_value={"control_service": "image@sha256:" + "a" * 64}))
            patches.enter_context(patch.object(local_dev, "build_work_runtime_fixture", return_value="fixture-image"))
            patches.enter_context(patch.object(local_dev, "make_app_input", return_value=(root / "platform.yaml", root / "resource.yaml", "a" * 64)))
            patches.enter_context(patch.object(local_dev, "cert", return_value=(root / "key", root / "crt")))
            patches.enter_context(patch.object(local_dev, "kubectl", return_value=subprocess.CompletedProcess([], 0)))
            patches.enter_context(patch.object(local_dev.shutil, "which", return_value="/synthetic/kubectl"))
            patches.enter_context(patch.object(local_dev, "start_owned_process", return_value={"pid": 1}))
            invoked = patches.enter_context(patch.object(local_dev, "apply_local_platform_configuration", side_effect=ReachedExecutor))
            with self.assertRaises(ReachedExecutor):
                local_dev.up(external_fixtures=True)
            invoked.assert_called_once()
            self.assertNotEqual(json.loads(local_dev.STATE_FILE.read_bytes())["phase"], "ready")


class ManagedObjectOwnershipTests(unittest.TestCase):
    TARGET = {"namespace": "science", "installationId": "019fa9d0-0000-7000-8000-000000000142"}

    def configmap(self, name, data):
        return {"apiVersion": "v1", "kind": "ConfigMap", "metadata": {
            "name": name, "namespace": "science",
            "annotations": {"labweaver.io/installation-id": self.TARGET["installationId"]}}, "data": data}

    def test_apply_requires_owned_reads_and_uses_uid_revision_preconditions(self):
        target = self.TARGET
        desired = self.configmap("control-service-config", {"config.yaml": "configuration"})
        names = {("ConfigMap", "control-service-config")}
        current = {"metadata": dict(desired["metadata"], uid=target["installationId"], resourceVersion="7")}
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / ".private/objects.yaml"
            fixtures.write(path, yaml.safe_dump(desired).encode())
            for observed, operation in ((b"", "create"), (json.dumps(current).encode(), "replace")):
                with patch.object(executor.subprocess, "run", side_effect=[
                    subprocess.CompletedProcess([], 0, observed, b""), subprocess.CompletedProcess([], 0, b"", b""),
                ]) as commands:
                    executor.apply_objects(path, target, names)
                    self.assertEqual(commands.call_args.args[0][1], operation)
                    written = json.loads(commands.call_args.kwargs["input"])
                    if operation == "replace":
                        self.assertEqual(written["metadata"]["uid"], current["metadata"]["uid"])
                        self.assertEqual(written["metadata"]["resourceVersion"], "7")
            foreign = {"metadata": dict(current["metadata"], annotations={"labweaver.io/installation-id": "foreign"})}
            for read in (subprocess.CompletedProcess([], 1, b"", b"unreachable"),
                         subprocess.CompletedProcess([], 0, json.dumps(foreign).encode(), b"")):
                with patch.object(executor.subprocess, "run", return_value=read) as commands:
                    with self.assertRaisesRegex(executor.ApplyError, "release-ownership"):
                        executor.apply_objects(path, target, names)
                    self.assertEqual(commands.call_count, 1)

    def test_configuration_stage_never_rewrites_package_bound_application_objects(self):
        # The rendered application objects carry the raw Bundle values; the application role
        # later binds package images into these ConfigMaps. The configuration stage must not
        # restore the raw values over those bindings.
        bundle_objects = [
            self.configmap("control-service-config", {"config.yaml": "control"}),
            self.configmap("evaluation-service-config", {"config.yaml": "coordinator:\n  workerImage: example/worker:raw\n"}),
            self.configmap("build-executor-config", {"config.yaml": "executor:\n  serviceImage: example/runner:raw\n"}),
            self.configmap("agent-service-config", {"config.yaml": "sandbox:\n  image: example/sandbox:raw\n"}),
        ]
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / ".private/objects.yaml"
            fixtures.write(path, yaml.safe_dump_all(bundle_objects).encode())
            written = []

            def kubectl(command, **kwargs):
                if command[1] in ("create", "replace"):
                    written.append(json.loads(kwargs["input"])["metadata"]["name"])
                return subprocess.CompletedProcess(command, 0, b"", b"")

            with patch.object(executor.subprocess, "run", side_effect=kubectl):
                executor.apply_objects(path, self.TARGET, executor.CONFIGURATION_OBJECTS)
        self.assertEqual(written, ["control-service-config"])

    def test_missing_configuration_object_fails_closed(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / ".private/objects.yaml"
            fixtures.write(path, yaml.safe_dump(self.configmap("agent-service-config", {})).encode())
            with patch.object(executor.subprocess, "run") as commands:
                with self.assertRaisesRegex(executor.ApplyError, "release-ownership"):
                    executor.apply_objects(path, self.TARGET, executor.CONFIGURATION_OBJECTS)
                commands.assert_not_called()


class SucceededIntentReplayTests(unittest.TestCase):
    SOURCE = {"desiredRevision": 3, "encryptedBundleSha256": "b" * 64, "configurationSha256": "c" * 64}

    def control(self, stage):
        test = self

        class Recording:
            source = test.SOURCE
            apply_id = "019fa9d0-0000-7000-8000-0000000000aa"
            calls = []
            observed = 0

            def request(self, method, path, payload=None, revision=None, key=None):
                self.calls.append(method)
                return {"revision": 9, "configurationSha256": test.SOURCE["configurationSha256"], "attempt": {
                    "id": self.apply_id, "revision": 5, "desiredRevision": 3,
                    "encryptedBundleSha256": test.SOURCE["encryptedBundleSha256"], "stage": stage,
                    "executorClientId": executor.CLIENT}}

            def observe(self):
                self.observed += 1

        return Recording()

    def test_succeeded_attempt_is_confirmed_without_writes(self):
        control = self.control("succeeded")
        self.assertTrue(executor.already_verified(control))
        self.assertEqual(control.calls, ["GET"])
        self.assertEqual(control.observed, 1)

    def test_succeeded_attempt_cannot_be_prepared_for_another_deployment(self):
        control = self.control("succeeded")
        with self.assertRaisesRegex(executor.ApplyError, "LW_PLATFORM_CONFIGURATION_APPLY_CONFLICT:prepare"):
            executor.prepare(control)
        self.assertEqual(control.calls, ["GET"])

    def test_unfinished_attempt_proceeds_to_the_normal_sequence(self):
        control = self.control("applying")
        self.assertFalse(executor.already_verified(control))
        self.assertEqual(control.observed, 0)


if __name__ == "__main__":
    unittest.main()
