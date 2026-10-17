"""Use authenticated synthetic Bundle objects and actual Helm executor projections."""

import copy
import json
import subprocess
import unittest

import yaml
import test_deployment_bundle as fixtures


class BundleEnvironmentExecutorTests(unittest.TestCase):
    def setUp(self):
        self.f = fixtures.DeploymentBundleTests(methodName="runTest")
        self.f.setUp()
        self.addCleanup(self.f.tearDown)

    def test_authenticated_bundle_projects_only_existing_owner_authority(self):
        f = self.f
        app = f.model["application"]
        for role in ("container-executor", "kubevirt-executor"):
            for key in fixtures.bundle.ENVIRONMENT_EXECUTOR_SHARED:
                app["workloadInputs"][role]["env"].pop(key, None)
        f.save_model()
        fixtures.bundle.seal_bundle(f.root, f.archive, [f.key()], f.runtime)
        with fixtures.bundle.opened_bundle(f.archive, f.identity, f.runtime) as (
            _,
            rendered,
            metadata,
        ):
            values = yaml.safe_load((rendered / "helm-values.yaml").read_bytes())
            objects = {
                o["metadata"]["name"]: o
                for o in yaml.safe_load_all(
                    (rendered / "application.yaml").read_bytes()
                )
            }
            result = subprocess.run(
                [
                    "helm",
                    "template",
                    "labweaver",
                    str(fixtures.bundle.WORKLOADS_FILE.parent),
                    "--namespace",
                    "labweaver-system",
                    "--values",
                    str(rendered / "helm-values.yaml"),
                ],
                capture_output=True,
                text=True,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            deployments = {
                o["metadata"]["name"]: o
                for o in yaml.safe_load_all(result.stdout)
                if o and o["kind"] == "Deployment"
            }
            for role in ("container-executor", "kubevirt-executor"):
                pod = deployments[role]["spec"]["template"]["spec"]
                container = pod["containers"][0]
                mounts = {m["name"]: m for m in container["volumeMounts"]}
                self.assertTrue(mounts["environment-authority"]["readOnly"])
                volume = next(
                    v for v in pod["volumes"] if v["name"] == "environment-authority"
                )
                source = volume["projected"]["sources"][0]["secret"]
                self.assertEqual(source["name"], "environment-service-secrets")
                self.assertEqual(
                    {i["key"] for i in source["items"]},
                    {
                        "database-url",
                        "nats-server",
                        "oidc-ca.pem",
                        "mtls-ca.pem",
                        "service-client-secret",
                        "nats-ca.pem",
                    },
                )
                self.assertEqual(
                    container["securityContext"]["capabilities"], {"drop": ["ALL"]}
                )
                self.assertFalse(
                    container["securityContext"]["allowPrivilegeEscalation"]
                )
                env = {e["name"]: e["value"] for e in container["env"] if "value" in e}
                for key in (
                    "LABWEAVER_SERVICE_CLIENT_ID",
                    "LABWEAVER_RESOURCE_USAGE_BASE_URI",
                    "LABWEAVER_RESOURCE_USAGE_SCOPES",
                ):
                    self.assertEqual(
                        env[key],
                        app["workloadInputs"]["environment-service"]["env"][key],
                    )
                config = yaml.safe_load(
                    objects[role + "-config"]["data"]["config.yaml"]
                )
                self.assertEqual(
                    config["databaseUrlFile"],
                    "/etc/labweaver/environment-authority/database-url",
                )
                self.assertEqual(
                    config["nats"]["serverFile"],
                    "/etc/labweaver/environment-authority/nats-server",
                )
                self.assertNotIn("server", config["nats"])
                self.assertEqual(
                    config["nats"]["credentialsFile"],
                    "/etc/labweaver/secrets/nats.creds",
                )
            for role in (
                "control-service",
                "environment-service",
                "build-executor",
                "kubevirt-console-executor",
            ):
                self.assertNotIn(
                    "environment-authority",
                    {
                        v["name"]
                        for v in deployments[role]["spec"]["template"]["spec"][
                            "volumes"
                        ]
                    },
                )
            source = json.loads(
                (rendered / "platform-configuration-source.json").read_bytes()
            )
            self.assertEqual(
                source["encryptedBundleSha256"], metadata["encryptedBundleSha256"]
            )
            self.assertNotIn(f.secret.decode(), result.stdout + result.stderr)
        f.clean_runtime()

    def test_executor_cannot_shadow_owner_client_endpoint_scope_or_database(self):
        app = self.f.model["application"]
        original = copy.deepcopy(app["workloadInputs"])
        for key, value in (
            ("LABWEAVER_SERVICE_CLIENT_ID", "other-client"),
            ("LABWEAVER_RESOURCE_USAGE_BASE_URI", "https://other.invalid/"),
            ("LABWEAVER_RESOURCE_USAGE_SCOPES", "resource.environment.resolve"),
            ("LABWEAVER_DATABASE_URL", "independent-db"),
        ):
            with self.subTest(key=key):
                app["workloadInputs"] = copy.deepcopy(original)
                app["workloadInputs"]["kubevirt-executor"]["env"][key] = value
                self.f.save_model()
                with self.assertRaisesRegex(
                    fixtures.bundle.BundleError,
                    "LW_BUNDLE_ENVIRONMENT_AUTHORITY_CONFLICT",
                ):
                    fixtures.bundle.load_bundle(self.f.root)

    def test_database_and_ca_aliases_cannot_refer_to_independent_files(self):
        app = self.f.model["application"]
        fixtures.write(
            self.f.root / "secrets/independent", b"synthetic-independent-input"
        )
        original = copy.deepcopy(app["secrets"])
        for key in ("database-url", "postgres-ca.pem", "nats-ca.pem"):
            with self.subTest(key=key):
                app["secrets"] = copy.deepcopy(original)
                app["secrets"]["container-executor-secrets"][
                    key
                ] = "secrets/independent"
                self.f.save_model()
                with self.assertRaisesRegex(
                    fixtures.bundle.BundleError,
                    "LW_BUNDLE_ENVIRONMENT_AUTHORITY_CONFLICT",
                ):
                    fixtures.bundle.load_bundle(self.f.root)

    def test_nats_subject_and_role_credential_paths_cannot_drift(self):
        path = (
            self.f.root
            / self.f.model["application"]["configMaps"]["container-executor-config"][
                "config.yaml"
            ]
        )
        original = yaml.safe_load(path.read_bytes())
        for key, value in (
            ("containerRequestSubject", "another.subject"),
            ("credentialsFile", "/tmp/other.creds"),
            ("server", "tls://other.invalid:4222"),
        ):
            with self.subTest(key=key):
                config = copy.deepcopy(original)
                config["nats"][key] = value
                fixtures.write(path, yaml.safe_dump(config).encode())
                with self.assertRaisesRegex(
                    fixtures.bundle.BundleError,
                    "LW_BUNDLE_ENVIRONMENT_AUTHORITY_CONFLICT",
                ):
                    fixtures.bundle.load_bundle(self.f.root)

    def test_executor_authority_projection_cannot_be_manually_overridden(self):
        self.f.model["application"]["helmValues"]["environmentExecutorAuthority"] = {
            "secretSources": []
        }
        self.f.save_model()
        with self.assertRaisesRegex(
            fixtures.bundle.BundleError, "LW_BUNDLE_DERIVED_OVERRIDE_FORBIDDEN"
        ):
            fixtures.bundle.load_bundle(self.f.root)

    def test_enabled_executor_requires_its_existing_owner(self):
        self.f.model["application"]["helmValues"]["workloads"] = {
            "environment-service": {"enabled": False}
        }
        self.f.save_model()
        with self.assertRaisesRegex(
            fixtures.bundle.BundleError, "LW_BUNDLE_ENVIRONMENT_OWNER_REQUIRED"
        ):
            fixtures.bundle.load_bundle(self.f.root)
