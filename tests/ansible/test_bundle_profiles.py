"""Recover Resource and unsaved first-install settings from one synthetic archive."""

import base64
import copy
import json
import shutil
import subprocess
import unittest

import yaml

import test_deployment_bundle as fixtures

bundle = fixtures.bundle
write = fixtures.write


class BundleProfileTests(unittest.TestCase):
    def setUp(self) -> None:
        self.fixture = fixtures.DeploymentBundleTests()
        self.fixture.setUp()

    def tearDown(self) -> None:
        self.fixture.tearDown()

    def resource(self) -> dict:
        f = self.fixture
        manifest = bundle.resource_objects.load_manifest(bundle.RESOURCE_MANIFEST)
        issuer = "A" + "B" * 55
        claims = {
            "iss": issuer,
            "nats": {
                "pub": {"allow": bundle.resource_objects.RESOURCE_PUBLISH_SUBJECTS},
                "sub": {"allow": bundle.resource_objects.RESOURCE_SUBSCRIBE_SUBJECTS},
                "resp": {"max": 1},
            },
        }
        encoded = base64.urlsafe_b64encode(json.dumps(claims).encode()).rstrip(b"=")
        credentials = b"-----BEGIN NATS USER JWT-----\na." + encoded + b".synthetic\n"
        write(f.root / "secrets/resource-nats.creds", credentials)
        profile = {
            "target": {
                "namespace": "labweaver-system",
                "releaseName": "labweaver-resource",
            },
            "packageFile": "versions/resource-package.json",
            "accessSeedFile": None,
            "natsIssuer": issuer,
            "configMaps": {},
            "secrets": {},
            "helmValues": {"network": {"kubernetesApiCidrs": ["192.0.2.10/32"]}},
            "ansibleVariables": {
                "resource_application_run_id": "bundle-first-install-0001",
                "resource_application_postgres_service": "synthetic-admin",
                "resource_application_postgres_service_file": {
                    "file": "secrets/shared-token"
                },
            },
            "workloadInputs": {},
        }
        for group in ("configMaps", "secrets"):
            for name, keys in manifest[group].items():
                profile[group][name] = {}
                for key in keys:
                    reference = (
                        f"config/resource/{key}"
                        if group == "configMaps"
                        else (
                            "secrets/resource-nats.creds"
                            if key == "nats.creds"
                            else "secrets/shared-token"
                        )
                    )
                    profile[group][name][key] = reference
                    if group == "configMaps":
                        # An opaque synthetic rate marker must survive without rewriting.
                        payload = (
                            b'{"gpuCatalogSeed":[],"syntheticRateMarker":1234567}\n'
                            if key == "capacity.json"
                            else b"bind_addr: 0.0.0.0:9448\n"
                        )
                        write(f.root / reference, payload)
        workload = yaml.safe_load(bundle.WORKLOADS_FILE.read_bytes())["workloads"][
            "resource-service"
        ]
        profile["workloadInputs"] = {
            "resource-service": {
                "env": dict(workload["env"]),
                "secretEnv": {
                    key: {
                        "secret": reference.get("secret", workload["secret"]),
                        "key": reference["key"],
                    }
                    for key, reference in workload["secretEnv"].items()
                },
            }
        }
        package = {
            "schema_version": "platform-image-package-manifest.v2",
            "profile": "resource",
            "images": [
                {
                    "component": "resource-service",
                    "reference": "example.invalid/resource@sha256:" + "a" * 64,
                }
            ],
        }
        write(f.root / profile["packageFile"], json.dumps(package).encode())
        f.model["resource"] = profile
        f.save_model()
        return profile

    def initial(self) -> dict:
        f = self.fixture
        staged = f.private / "initial/request.json"
        result = bundle.initialize_settings(None, staged)
        request = json.loads(staged.read_bytes())
        write(f.root / f.model["configurationFile"], staged.read_bytes())
        f.model.update(configurationMode="initial_settings", revision=1)
        f.save_model()
        self.assertEqual(
            result["managedConfigurationSha256"], f.version["configurationSha256"]
        )
        return request

    def test_resource_allowlist_and_oidc_source_match_executor_owner(self) -> None:
        profile = self.resource()
        original = copy.deepcopy(profile)
        for field, value in (
            ("LABWEAVER_ENVIRONMENT_SERVICE_CLIENT_ID", "another-client"),
            ("LABWEAVER_SERVICE_ALLOWED_CLIENT_IDS", "labweaver-access"),
            ("LABWEAVER_SERVICE_AUDIENCE", "another-audience"),
            ("LABWEAVER_SERVICE_OIDC_ISSUER", "https://another.invalid/"),
            ("LABWEAVER_SERVICE_OIDC_CA", "/tmp/other.pem"),
        ):
            with self.subTest(field=field):
                self.fixture.model["resource"] = copy.deepcopy(original)
                self.fixture.model["resource"]["workloadInputs"]["resource-service"][
                    "env"
                ][field] = value
                self.fixture.save_model()
                with self.assertRaisesRegex(
                    bundle.BundleError, "LW_BUNDLE_ENVIRONMENT_AUTHORITY_CONFLICT"
                ):
                    bundle.load_bundle(self.fixture.root)

    def test_first_install_defaults_are_unsaved_and_restore_as_the_existing_update(
        self,
    ) -> None:
        f = self.fixture
        request = self.initial()
        self.assertEqual(
            request, {"expectedRevision": 0, "configuration": f.configuration}
        )
        self.assertEqual(set(request), {"expectedRevision", "configuration"})
        self.resource()
        sealed = bundle.seal_bundle(f.root, f.archive, [f.key()], f.runtime)
        self.assertEqual(sealed["configurationMode"], "initial_settings")
        self.assertEqual(sealed["revision"], 1)
        shutil.rmtree(f.root)
        update = f.private / "new-install/update.json"
        imported = bundle.import_settings(f.archive, f.identity, f.runtime, 0, update)
        self.assertEqual(json.loads(update.read_bytes()), request)
        self.assertEqual(imported["configurationMode"], "initial_settings")
        self.assertEqual(
            imported["managedConfigurationSha256"], f.version["configurationSha256"]
        )
        self.assertFalse(
            any(
                word in json.dumps(request)
                for word in ("createdBy", "createdAt", "applied")
            )
        )
        f.clean_runtime()

    def test_first_install_custom_settings_use_the_same_policy_validation(self) -> None:
        f = self.fixture
        settings = copy.deepcopy(f.configuration)
        settings["lifecycle"]["preemptionGraceSeconds"] = 90
        source = f.private / "custom/settings.yaml"
        write(source, yaml.safe_dump(settings).encode())
        output = f.private / "custom/request.json"
        bundle.initialize_settings(source, output)
        self.assertEqual(
            json.loads(output.read_bytes()),
            {"expectedRevision": 0, "configuration": settings},
        )
        settings["runtimePolicy"]["allowedCapabilities"] = ["ALL"]
        write(source, yaml.safe_dump(settings).encode())
        rejected = f.private / "custom/rejected.json"
        with self.assertRaisesRegex(
            bundle.BundleError, "LW_BUNDLE_CONFIGURATION_INVALID"
        ):
            bundle.initialize_settings(source, rejected)
        self.assertFalse(rejected.exists())

    def test_initial_mode_cannot_smuggle_history_or_a_nonzero_initial_fence(
        self,
    ) -> None:
        f = self.fixture
        self.initial()
        for expected in (1, 12):
            write(
                f.root / f.model["configurationFile"],
                json.dumps(
                    {"expectedRevision": expected, "configuration": f.configuration}
                ).encode(),
            )
            with self.assertRaisesRegex(
                bundle.BundleError, "LW_BUNDLE_INITIAL_REVISION_INVALID"
            ):
                bundle.load_bundle(f.root)
        write(f.root / f.model["configurationFile"], json.dumps(f.version).encode())
        with self.assertRaisesRegex(
            bundle.BundleError, "LW_BUNDLE_CONFIGURATION_INVALID"
        ):
            bundle.load_bundle(f.root)

    def test_resource_reuses_renderer_and_one_lock_without_changing_capacity_or_prices(
        self,
    ) -> None:
        f = self.fixture
        profile = self.resource()
        source_capacity = (
            f.root / profile["configMaps"]["resource-service-config"]["capacity.json"]
        ).read_bytes()
        output = f.private / "rendered"
        bundle.derive_bundle(f.root, output)
        objects = list(
            yaml.safe_load_all((output / "resource/application.yaml").read_bytes())
        )
        self.assertEqual(
            {item["metadata"]["name"] for item in objects},
            {"resource-service-config", "resource-service-secrets"},
        )
        capacity = next(item for item in objects if item["kind"] == "ConfigMap")[
            "data"
        ]["capacity.json"]
        self.assertEqual(capacity.encode(), source_capacity)
        variables = yaml.safe_load((output / "resource/ansible-vars.yaml").read_bytes())
        self.assertEqual(
            variables["resource_application_versions_lock_file"],
            str(f.root / f.model["versions"]["lockFile"]),
        )
        self.assertEqual(variables["resource_application_access_seed_file"], "")
        self.assertEqual(
            variables["resource_application_release_name"], "labweaver-resource"
        )
        self.assertFalse((output / "resource/object-inputs").exists())

    def test_resource_helm_owns_only_resource_and_keeps_shared_objects_in_platform_release(
        self,
    ) -> None:
        f = self.fixture
        self.resource()
        output = f.private / "helm"
        bundle.derive_bundle(f.root, output)
        result = subprocess.run(
            [
                shutil.which("helm"),
                "template",
                "labweaver-resource",
                str(bundle.WORKLOADS_FILE.parent),
                "--namespace",
                "labweaver-system",
                "--values",
                str(output / "resource/helm-values.yaml"),
            ],
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        objects = [item for item in yaml.safe_load_all(result.stdout) if item]
        self.assertEqual(
            [
                item["metadata"]["name"]
                for item in objects
                if item["kind"] == "Deployment"
            ],
            ["resource-service"],
        )
        self.assertNotIn(
            "keycloak-internal-proxy", {item["metadata"]["name"] for item in objects}
        )
        values = yaml.safe_load((output / "resource/helm-values.yaml").read_bytes())
        self.assertFalse(values["network"]["defaultDenyEnabled"])
        self.assertTrue(values["network"]["resourceKubernetesApiCiliumPolicyEnabled"])

    def test_resource_wrong_issuer_fails_before_ciphertext_publish(
        self,
    ) -> None:
        f = self.fixture
        profile = self.resource()
        profile["natsIssuer"] = "A" + "C" * 55
        f.save_model()
        with self.assertRaisesRegex(
            bundle.resource_objects.BundleError, "LW_RESOURCE_NATS_ISSUER_MISMATCH"
        ):
            bundle.seal_bundle(f.root, f.archive, [f.key()], f.runtime)
        self.assertFalse(f.archive.exists())
        f.clean_runtime()

    def test_profiles_cannot_take_each_others_release_or_workloads(self) -> None:
        f = self.fixture
        profile = self.resource()
        original = copy.deepcopy(profile)
        profile["target"] = dict(f.model["target"])
        f.save_model()
        with self.assertRaisesRegex(
            bundle.BundleError, "LW_BUNDLE_PROFILE_OWNERSHIP_CONFLICT"
        ):
            bundle.load_bundle(f.root)
        f.model["resource"] = original
        original["helmValues"]["workloads"] = {"control-service": {"enabled": True}}
        f.save_model()
        with self.assertRaisesRegex(
            bundle.BundleError, "LW_BUNDLE_PROFILE_OWNERSHIP_CONFLICT"
        ):
            bundle.load_bundle(f.root)

    def test_resource_extra_nats_permissions_and_malformed_layout_are_rejected(
        self,
    ) -> None:
        f = self.fixture
        profile = self.resource()
        credentials = f.root / "secrets/resource-nats.creds"
        payload = credentials.read_bytes().split(b"\n")[1].split(b".")[1]
        claims = json.loads(
            base64.urlsafe_b64decode(payload + b"=" * (-len(payload) % 4))
        )
        claims["nats"]["pub"]["allow"].append("unapproved.>")
        encoded = base64.urlsafe_b64encode(json.dumps(claims).encode()).rstrip(b"=")
        write(
            credentials,
            b"-----BEGIN NATS USER JWT-----\na." + encoded + b".synthetic\n",
        )
        with self.assertRaisesRegex(
            bundle.resource_objects.BundleError, "LW_RESOURCE_NATS_PERMISSIONS_INVALID"
        ):
            bundle.derive_bundle(f.root, f.private / "rejected-permissions")
        profile["helmValues"]["network"] = "not-a-network-map"
        f.save_model()
        with self.assertRaisesRegex(bundle.BundleError, "LW_BUNDLE_MODEL_INVALID"):
            bundle.load_bundle(f.root)

    def test_resource_role_loads_generated_inputs_and_selected_lock_without_access_seed(
        self,
    ) -> None:
        f = self.fixture
        self.resource()
        output = f.private / "ansible"
        bundle.derive_bundle(f.root, output)
        tasks = yaml.safe_load(
            (
                bundle.WORKLOADS_FILE.parents[2]
                / "ansible/roles/resource_application/tasks/main.yml"
            ).read_bytes()
        )[:3]
        playbook = f.private / "role-input-check.yaml"
        write(
            playbook,
            yaml.safe_dump(
                [
                    {
                        "hosts": "localhost",
                        "connection": "local",
                        "gather_facts": False,
                        "vars": {
                            "resource_application_vars_file": str(
                                output / "resource/ansible-vars.yaml"
                            )
                        },
                        "tasks": tasks
                        + [
                            {
                                "name": "Check selected inputs",
                                "ansible.builtin.assert": {
                                    "that": [
                                        "resource_application_lock.schema_version == 1",
                                        "resource_application_access_seed_file == ''",
                                        "resource_application_release_name == 'labweaver-resource'",
                                    ]
                                },
                            }
                        ],
                    }
                ]
            ).encode(),
        )
        result = subprocess.run(
            ["ansible-playbook", "-i", "localhost,", str(playbook)],
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
