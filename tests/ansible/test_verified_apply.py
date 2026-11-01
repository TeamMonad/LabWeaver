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
        try:
            archive, identity, manifest = local_dev.prepare_local_managed_bundle(work, images, version)
        except fixtures.bundle.BundleError:
            diagnostic_files = fixtures.bundle._files(work / "managed-bundle")
            collisions = [(public_name, secret_name) for public_name, content in diagnostic_files.items()
                          if not public_name.startswith("secrets/")
                          for secret_name, secret in diagnostic_files.items()
                          if secret_name.startswith("secrets/") and len(secret) >= 16 and secret in content]
            self.fail("fixture credential locations: " + str(collisions))
        with fixtures.bundle.opened_bundle(archive, identity, work / "bundle-runtime", manifest) as (root, rendered, metadata):
            source = json.loads((rendered / "platform-configuration-source.json").read_bytes())
            model, files = fixtures.bundle.load_bundle(root, manifest)
            self.assertEqual(source["configurationSha256"], self.fixture.version["configurationSha256"])
            self.assertEqual(source["encryptedBundleSha256"], metadata["encryptedBundleSha256"])
            self.assertEqual(files[model["application"]["configMaps"]["control-service-config"]["config.yaml"]],
                             (work / "app-input/configmaps/control-service-config/config.yaml").read_bytes())
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


if __name__ == "__main__":
    unittest.main()
