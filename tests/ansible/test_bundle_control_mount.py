"""Render actual authenticated Bundle inputs into only the existing Control workload."""

import json
import subprocess
import unittest

import yaml

import test_deployment_bundle as fixtures


class BundleControlMountTests(unittest.TestCase):
    def setUp(self):
        self.fixture = fixtures.DeploymentBundleTests(methodName="runTest")
        self.fixture.setUp()
        self.addCleanup(self.fixture.tearDown)

    def render(self, values):
        return subprocess.run(
            [
                "helm",
                "template",
                "labweaver",
                "deploy/helm/labweaver",
                "--namespace",
                "labweaver-system",
                "--values",
                str(values),
            ],
            cwd=fixtures.REPOSITORY,
            capture_output=True,
            text=True,
        )

    def test_managed_inputs_mount_same_replica_source_without_granting_permissions(
        self,
    ):
        f = self.fixture
        f.model["application"]["helmValues"]["workloads"] = {
            "control-service": {"replicas": 2}
        }
        f.save_model()
        recipient = f.key()
        fixtures.bundle.seal_bundle(f.root, f.archive, [recipient], f.runtime)
        with fixtures.bundle.opened_bundle(f.archive, f.identity, f.runtime) as (
            _,
            loaded,
            metadata,
        ):
            rendered = self.render(loaded / "helm-values.yaml")
            self.assertEqual(rendered.returncode, 0, rendered.stderr)
            objects = list(yaml.safe_load_all(rendered.stdout))
            control = next(
                o
                for o in objects
                if o
                and o["kind"] == "Deployment"
                and o["metadata"]["name"] == "control-service"
            )
            source_map = next(
                o
                for o in objects
                if o
                and o["kind"] == "ConfigMap"
                and "platform-configuration-source.json" in o.get("data", {})
            )
            self.assertTrue(source_map["immutable"])
            source = json.loads(
                source_map["data"]["platform-configuration-source.json"]
            )
            self.assertEqual(
                source["encryptedBundleSha256"], metadata["encryptedBundleSha256"]
            )
            self.assertEqual(source["expectedControlReplicas"], 2)
            self.assertEqual(
                source_map["data"]["platform-configuration.json"].encode(),
                (loaded / "platform-configuration.json").read_bytes(),
            )
            policy = json.loads(
                source_map["data"]["platform-configuration-policy.json"]
            )
            self.assertEqual(
                policy,
                json.loads(
                    (loaded / "platform-configuration-policy.json").read_bytes()
                ),
            )
            self.assertEqual(
                policy["configurationSha256"],
                f.version["configurationSha256"],
            )
            pod = control["spec"]["template"]["spec"]
            self.assertEqual(
                control["spec"]["template"]["metadata"]["annotations"][
                    "labweaver.io/platform-policy-sha256"
                ],
                f.version["configurationSha256"],
            )
            main = pod["containers"][0]
            initialize = pod["initContainers"][0]
            self.assertEqual(main["image"], initialize["image"])
            self.assertEqual(initialize["args"], ["--prepare-managed-configuration"])
            self.assertFalse(pod["automountServiceAccountToken"])
            self.assertTrue(pod["securityContext"]["runAsNonRoot"])
            self.assertEqual(
                initialize["securityContext"]["capabilities"], {"drop": ["ALL"]}
            )
            self.assertFalse(initialize["securityContext"]["allowPrivilegeEscalation"])
            self.assertTrue(initialize["securityContext"]["readOnlyRootFilesystem"])
            volume = next(v for v in pod["volumes"] if v["name"] == "managed-input")
            self.assertEqual(
                volume["configMap"]["name"], source_map["metadata"]["name"]
            )
            mount = next(
                v for v in main["volumeMounts"] if v["name"] == "managed-runtime"
            )
            self.assertTrue(mount["readOnly"])
            env = {v["name"]: v for v in main["env"]}
            self.assertEqual(
                env["LABWEAVER_CONTROL_POD_UID"]["valueFrom"]["fieldRef"]["fieldPath"],
                "metadata.uid",
            )
            self.assertTrue(
                env["LABWEAVER_PLATFORM_CONFIGURATION_VERSION_FILE"]["value"].endswith(
                    "/current/platform-configuration.json"
                )
            )
            for obj in objects:
                if (
                    obj
                    and obj["kind"] == "Deployment"
                    and obj["metadata"]["name"] != "control-service"
                ):
                    self.assertNotIn(
                        "managed-input",
                        {
                            v["name"]
                            for v in obj["spec"]["template"]["spec"].get("volumes", [])
                        },
                    )
            self.assertNotIn(f.secret.decode(), json.dumps(source_map))
        f.clean_runtime()

    def test_privileged_runtime_uses_the_bundle_dedicated_node_pool(self):
        f = self.fixture
        f.configuration["runtimePolicy"].update(
            privilegedAllowed=True,
            privilegedExecutionLocation={
                "kind": "dedicated_node_pool",
                "nodeSelector": {"labweaver.io/pool": "research"},
                "taintKey": "labweaver.io/research",
                "taintValue": "reserved",
            },
        )
        f.save_version()
        recipient = f.key()
        fixtures.bundle.seal_bundle(f.root, f.archive, [recipient], f.runtime)
        with fixtures.bundle.opened_bundle(f.archive, f.identity, f.runtime) as (
            _,
            loaded,
            _,
        ):
            rendered = self.render(loaded / "helm-values.yaml")
            self.assertEqual(rendered.returncode, 0, rendered.stderr)
            objects = list(yaml.safe_load_all(rendered.stdout))
            executor = next(
                item
                for item in objects
                if item
                and item["kind"] == "Deployment"
                and item["metadata"]["name"] == "kubevirt-executor"
            )
            pod = executor["spec"]["template"]["spec"]
            self.assertEqual(pod["nodeSelector"], {"labweaver.io/pool": "research"})
            self.assertEqual(
                pod["tolerations"],
                [
                    {
                        "key": "labweaver.io/research",
                        "operator": "Equal",
                        "value": "reserved",
                        "effect": "NoSchedule",
                    }
                ],
            )
        f.clean_runtime()

    def test_missing_source_wrong_binding_or_override_fails_actual_helm(self):
        f = self.fixture
        recipient = f.key()
        fixtures.bundle.seal_bundle(f.root, f.archive, [recipient], f.runtime)
        with fixtures.bundle.opened_bundle(f.archive, f.identity, f.runtime) as (
            _,
            loaded,
            _,
        ):
            original = yaml.safe_load((loaded / "helm-values.yaml").read_bytes())
            for change in (
                "missing",
                "binding",
                "policy",
                "policy-top-level",
                "policy-api",
                "replicas",
                "namespace",
                "override",
            ):
                values = json.loads(json.dumps(original))
                if change == "missing":
                    del values["managedControlConfiguration"]["sourceJson"]
                elif change == "policy":
                    values["platformConfigurationPolicy"]["runtimePolicy"][
                        "allowedCapabilities"
                    ] = ["NET_RAW"]
                elif change == "policy-top-level":
                    values["platformConfigurationPolicy"]["unexpected"] = "tampered"
                elif change == "policy-api":
                    values["platformConfigurationPolicy"]["apiVersion"] = "tampered/v9"
                elif change == "override":
                    values["workloadInputs"]["control-service"]["env"][
                        "LABWEAVER_PLATFORM_CONFIGURATION_SOURCE_FILE"
                    ] = "/old-source"
                else:
                    source = json.loads(
                        values["managedControlConfiguration"]["sourceJson"]
                    )
                    if change == "binding":
                        source["configurationSha256"] = "0" * 64
                    elif change == "replicas":
                        source["expectedControlReplicas"] = 2
                    else:
                        source["installation"]["namespace"] = "elsewhere"
                    values["managedControlConfiguration"]["sourceJson"] = json.dumps(
                        source
                    )
                path = loaded / f"{change}.yaml"
                fixtures.write(path, yaml.safe_dump(values).encode())
                rendered = self.render(path)
                self.assertNotEqual(rendered.returncode, 0, change)
                self.assertNotIn(f.secret.decode(), rendered.stderr)

    def test_plain_derivation_and_initial_settings_do_not_claim_authenticated_source(
        self,
    ):
        f = self.fixture
        output = f.private / "derive-only"
        fixtures.bundle.derive_bundle(f.root, output)
        rendered = self.render(output / "helm-values.yaml")
        self.assertEqual(rendered.returncode, 0, rendered.stderr)
        self.assertNotIn("prepare-managed-configuration", rendered.stdout)
        f.model["application"]["helmValues"]["managedControlConfiguration"] = {
            "sourceJson": "handwritten"
        }
        f.save_model()
        with self.assertRaisesRegex(
            fixtures.bundle.BundleError, "LW_BUNDLE_DERIVED_OVERRIDE_FORBIDDEN"
        ):
            fixtures.bundle.load_bundle(f.root)


if __name__ == "__main__":
    unittest.main()
