"""Exercise only synthetic credentials, real age and the local Ansible inventory parser."""

import copy
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import unittest
from unittest.mock import patch

import yaml

import test_deployment_bundle as fixtures

sys.path.insert(0, str(fixtures.REPOSITORY / "tools"))
import prepare_bundle_acceptance as acceptance

bundle = fixtures.bundle


class BundleInfrastructureTests(unittest.TestCase):
    def setUp(self) -> None:
        self.fixture = fixtures.DeploymentBundleTests(methodName="runTest")
        self.fixture.setUp()
        self.addCleanup(self.fixture.tearDown)
        self.inventory = {
            "all": {
                "vars": {
                    "ansible_user": "synthetic-deploy",
                    "ansible_ssh_private_key_file": {"file": "secrets/ssh-key"},
                    "ansible_password": {"secret": "secrets/shared-token"},
                    "platform_foundation_bundle": {"file": "secrets/foundation.yaml"},
                },
                "children": {
                    "control_plane": {
                        "hosts": {"fixture-cp": {"ansible_host": "192.0.2.7"}}
                    }
                },
            }
        }
        self.fixture.model["infrastructure"] = {
            "inventoryFile": "config/inventory.yaml",
            "vaultPasswordFile": None,
            "identitySecretFile": "secrets/identity.yaml",
        }
        fixtures.write(
            self.fixture.root / "versions/lock.yaml",
            yaml.safe_dump(
                {
                    "schema_version": 1,
                    "test_images": yaml.safe_load(
                        (fixtures.REPOSITORY / "deploy/versions.lock.yml").read_text()
                    )["test_images"],
                }
            ).encode(),
        )
        for name, payload in {
            "ssh-key": b"-----BEGIN OPENSSH PRIVATE KEY-----\nsynthetic-not-a-usable-key\n",
            "identity.yaml": b"bootstrap_password: synthetic-identity-password\n",
            "foundation.yaml": b"kind: Secret\ndata: {}\n",
        }.items():
            fixtures.write(self.fixture.root / "secrets" / name, payload)
        self.save()

    def save(self) -> None:
        fixtures.write(
            self.fixture.root / "config/inventory.yaml",
            yaml.safe_dump(self.inventory).encode(),
        )
        self.fixture.save_model()

    def test_real_age_restores_credentials_paths_and_ansible_unsafe_values(
        self,
    ) -> None:
        self.fixture.seal()
        with bundle.opened_bundle(
            self.fixture.archive, self.fixture.identity, self.fixture.runtime
        ) as (root, rendered, _):
            result = subprocess.run(
                [
                    "ansible-inventory",
                    "-i",
                    str(rendered / "infrastructure/inventory.yaml"),
                    "--list",
                    "--export",
                ],
                capture_output=True,
                text=True,
                check=True,
            )
            inventory = json.loads(result.stdout)
            variables = inventory["all"]["vars"]
            self.assertEqual(
                variables["ansible_ssh_private_key_file"], str(root / "secrets/ssh-key")
            )
            self.assertEqual(
                variables["ansible_password"],
                {"__ansible_unsafe": self.fixture.secret.decode()},
            )
            self.assertEqual(
                variables["labweaver_versions_lock_file"],
                str(root / "versions/lock.yaml"),
            )
            self.assertTrue(variables["identity_secret_locator_controller"])
            self.assertIn(
                "!unsafe", (rendered / "infrastructure/inventory.yaml").read_text()
            )
            self.assertEqual(
                inventory["_meta"]["hostvars"]["fixture-cp"]["ansible_host"],
                "192.0.2.7",
            )
        self.fixture.clean_runtime()

    def test_one_rotated_source_updates_all_inventory_references(self) -> None:
        self.inventory["all"]["vars"]["platform_buildkit_bundle"] = {
            "file": "secrets/foundation.yaml"
        }
        self.save()
        fixtures.write(
            self.fixture.root / "secrets/foundation.yaml",
            b"kind: Secret\ndata: {rotated: synthetic}\n",
        )
        self.fixture.seal()
        with bundle.opened_bundle(
            self.fixture.archive, self.fixture.identity, self.fixture.runtime
        ) as (root, rendered, _):
            result = subprocess.run(
                [
                    "ansible-inventory",
                    "-i",
                    str(rendered / "infrastructure/inventory.yaml"),
                    "--list",
                    "--export",
                ],
                capture_output=True,
                text=True,
                check=True,
            )
            variables = json.loads(result.stdout)["all"]["vars"]
            self.assertEqual(
                variables["platform_buildkit_bundle"],
                variables["platform_foundation_bundle"],
            )
            self.assertIn(b"rotated", (root / "secrets/foundation.yaml").read_bytes())

    def test_missing_credentials_and_external_file_paths_fail_without_values(
        self,
    ) -> None:
        original = copy.deepcopy(self.inventory)
        for key, value, diagnostic in (
            (
                "ansible_ssh_private_key_file",
                "/outside/real-key",
                "CREDENTIAL_FILE_REFERENCE_REQUIRED",
            ),
            (
                "ansible_password",
                "synthetic-inline-secret",
                "SECRET_REFERENCE_REQUIRED",
            ),
            (
                "labweaver_versions_lock_file",
                {"file": "versions/other.yaml"},
                "DERIVED_OVERRIDE_FORBIDDEN",
            ),
            (
                "ansible_ssh_common_args",
                "-o StrictHostKeyChecking=no",
                "SSH_OVERRIDE_FORBIDDEN",
            ),
        ):
            with self.subTest(key=key):
                self.inventory = copy.deepcopy(original)
                self.inventory["all"]["vars"][key] = value
                self.save()
                with self.assertRaisesRegex(bundle.BundleError, diagnostic):
                    bundle.load_bundle(self.fixture.root)
        self.inventory = original
        self.save()
        (self.fixture.root / "secrets/ssh-key").unlink()
        with self.assertRaisesRegex(bundle.BundleError, "INVENTORY_MISMATCH"):
            bundle.load_bundle(self.fixture.root)

    def test_wrong_key_and_failed_consumer_remove_inventory_and_plaintext(self) -> None:
        self.fixture.seal()
        wrong = self.fixture.directory / "wrong-key"
        self.fixture.key(wrong)
        failed = self.fixture.cli(
            "exec",
            "--archive",
            str(self.fixture.archive),
            "--identity",
            str(wrong),
            "--",
            sys.executable,
            "-c",
            "raise SystemExit(0)",
        )
        self.assertNotEqual(failed.returncode, 0)
        self.fixture.clean_runtime()
        failed = self.fixture.cli(
            "exec",
            "--archive",
            str(self.fixture.archive),
            "--identity",
            str(self.fixture.identity),
            "--",
            sys.executable,
            "-c",
            "import os,pathlib; print(pathlib.Path(os.environ['LABWEAVER_IDENTITY_SECRET_LOCATOR']).read_text()); raise SystemExit(9)",
        )
        self.assertNotEqual(failed.returncode, 0)
        self.assertNotIn("synthetic-identity-password", failed.stdout + failed.stderr)
        self.fixture.clean_runtime()

    def test_exec_selects_bundle_inventory_and_clears_ambient_vault(self) -> None:
        self.fixture.seal()
        marker = self.fixture.directory / "consumer-ok"
        script = """import os,pathlib
assert os.environ['ANSIBLE_INVENTORY']==os.environ['LABWEAVER_BUNDLE_INVENTORY_FILE']
assert 'ANSIBLE_VAULT_PASSWORD_FILE' not in os.environ
assert 'LABWEAVER_BUNDLE_VAULT_PASSWORD_FILE' not in os.environ
assert 'ANSIBLE_HOST_KEY_CHECKING' not in os.environ
assert 'ANSIBLE_LOG_PATH' not in os.environ
assert pathlib.Path(os.environ['ANSIBLE_LOCAL_TEMP']).is_dir()
assert os.environ['ANSIBLE_CONFIG'].endswith('/deploy/ansible/ansible.cfg')
assert pathlib.Path(os.environ['LABWEAVER_IDENTITY_SECRET_LOCATOR']).read_text().startswith('bootstrap_password:')
pathlib.Path(__import__('sys').argv[1]).write_text('ok')
"""
        with patch.dict(
            os.environ,
            {
                "ANSIBLE_VAULT_PASSWORD_FILE": "/ambient/secret",
                "LABWEAVER_BUNDLE_VAULT_PASSWORD_FILE": "/ambient/secret",
                "LABWEAVER_IDENTITY_SECRET_LOCATOR": "/ambient/locator",
                "ANSIBLE_HOST_KEY_CHECKING": "false",
                "ANSIBLE_CONFIG": "/ambient/insecure-config",
                "ANSIBLE_LOG_PATH": "/ambient/ordinary-log",
            },
        ):
            result = self.fixture.cli(
                "exec",
                "--archive",
                str(self.fixture.archive),
                "--identity",
                str(self.fixture.identity),
                "--",
                sys.executable,
                "-c",
                script,
                str(marker),
            )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(marker.read_text(), "ok")
        self.fixture.clean_runtime()

    def test_acceptance_manifest_is_bounded_and_does_not_contain_credentials(
        self,
    ) -> None:
        self.fixture.seal()
        output = self.fixture.private / "acceptance.yaml"
        with bundle.opened_bundle(
            self.fixture.archive, self.fixture.identity, self.fixture.runtime
        ) as (root, rendered, _), patch.dict(
            os.environ,
            {"LW_BUNDLE_ROOT": str(root), "LW_BUNDLE_RENDERED_ROOT": str(rendered)},
        ):
            metadata = acceptance.prepare("lw-bundle-203-synthetic", output)
            with self.assertRaisesRegex(bundle.BundleError, "OUTPUT_EXISTS"):
                acceptance.prepare("lw-bundle-203-synthetic", output)
            with self.assertRaisesRegex(bundle.BundleError, "NAMESPACE_INVALID"):
                acceptance.prepare(
                    "labweaver-system", self.fixture.private / "bad.yaml"
                )
            created = self.fixture.private / "create-namespace.yaml"
            created_metadata = acceptance.prepare(
                "lw-bundle-203-synthetic", created, create_namespace=True
            )
            self.assertEqual(created_metadata["objects"], 5)
            self.assertEqual(created_metadata["namespaceMode"], "create")
            self.assertEqual(
                next(yaml.safe_load_all(created.read_bytes()))["kind"], "Namespace"
            )
            configuration_file = rendered / "platform-configuration.json"
            configuration_file.write_bytes(configuration_file.read_bytes() + b"\n")
            with self.assertRaisesRegex(bundle.BundleError, "CONFIG_CHANGED"):
                acceptance.prepare(
                    "lw-bundle-203-synthetic", self.fixture.private / "changed.yaml"
                )
        documents = list(yaml.safe_load_all(output.read_bytes()))
        self.assertEqual(metadata["objects"], 4)
        self.assertEqual(metadata["namespaceMode"], "existing")
        self.assertEqual(
            metadata["cleanupObjects"],
            [
                "job/bundle-restore",
                "configmap/bundle-configuration",
                "resourcequota/bundle-acceptance",
            ],
        )
        self.assertNotIn(self.fixture.secret, output.read_bytes())
        self.assertNotIn(b"PRIVATE KEY", output.read_bytes())
        self.assertEqual(
            {obj["kind"] for obj in documents},
            {"ResourceQuota", "NetworkPolicy", "ConfigMap", "Job"},
        )
        job = next(obj for obj in documents if obj["kind"] == "Job")
        quota = next(obj for obj in documents if obj["kind"] == "ResourceQuota")
        fixture_maps = sum(obj["kind"] == "ConfigMap" for obj in documents)
        self.assertEqual(
            int(quota["spec"]["hard"]["count/configmaps"]), fixture_maps + 1
        )
        pod = job["spec"]["template"]["spec"]
        self.assertFalse(pod["automountServiceAccountToken"])
        container = pod["containers"][0]
        self.assertEqual(
            container["securityContext"]["capabilities"], {"drop": ["ALL"]}
        )
        self.assertIn("@sha256:", container["image"])
        self.assertEqual(output.stat().st_mode & 0o777, 0o600)

    def test_explicit_vault_file_is_restored_without_an_external_password(self) -> None:
        self.fixture.model["infrastructure"][
            "vaultPasswordFile"
        ] = "secrets/vault-password"
        fixtures.write(
            self.fixture.root / "secrets/vault-password", b"synthetic-vault-password\n"
        )
        self.save()
        self.fixture.seal()
        marker = self.fixture.directory / "vault-consumer-ok"
        script = "import os,pathlib,sys; p=pathlib.Path(os.environ['ANSIBLE_VAULT_PASSWORD_FILE']); assert str(p)==os.environ['LABWEAVER_BUNDLE_VAULT_PASSWORD_FILE']; assert p.read_text()=='synthetic-vault-password\\n'; pathlib.Path(sys.argv[1]).write_text('ok')"
        with patch.dict(
            os.environ, {"ANSIBLE_VAULT_PASSWORD_FILE": "/ambient/unrelated"}
        ):
            result = self.fixture.cli(
                "exec",
                "--archive",
                str(self.fixture.archive),
                "--identity",
                str(self.fixture.identity),
                "--",
                sys.executable,
                "-c",
                script,
                str(marker),
            )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(marker.read_text(), "ok")
        self.assertNotIn("synthetic-vault-password", result.stdout + result.stderr)
        self.fixture.clean_runtime()

    def test_ansible_roles_read_the_bundle_lock_and_local_identity_locator(
        self,
    ) -> None:
        sentinel = "bundle-only-version"
        lock = {
            "schema_version": 1,
            "cilium": {"sentinel": sentinel},
            "identity_foundation": {"sentinel": sentinel},
            "high_availability": {"sentinel": sentinel},
            "platform_foundation": {"admin_tools": {"sentinel": sentinel}},
            "harbor": {
                "chart_archive_sha256": "b" * 64,
                "images": {"sentinel": sentinel},
            },
            "test_images": {"busybox": "example.invalid/busybox@sha256:" + "b" * 64},
        }
        fixtures.write(
            self.fixture.root / "versions/lock.yaml", yaml.safe_dump(lock).encode()
        )
        tasks = []
        roles = [
            "cluster_network",
            "identity_foundation",
            "verify",
            "platform_admin_tools",
            "platform_foundation",
            "harbor",
            "sandbox_runtime",
            "platform_buildkit",
        ]
        for role in roles:
            source = yaml.safe_load(
                (
                    fixtures.REPOSITORY
                    / "deploy/ansible/roles"
                    / role
                    / "tasks/main.yml"
                ).read_text()
            )
            tasks.extend(source[:2] if role == "verify" else source[:1])
        identity_tasks = yaml.safe_load(
            (
                fixtures.REPOSITORY
                / "deploy/ansible/roles/identity_foundation/tasks/main.yml"
            ).read_text()
        )
        locator_tasks = identity_tasks[2:6]
        tasks.extend(locator_tasks)
        checks = [
            "cluster_network_cilium_lock.sentinel == 'bundle-only-version'",
            "identity_lock.sentinel == 'bundle-only-version'",
            "identity_ha_lock.sentinel == 'bundle-only-version'",
            "platform_admin_tools_lock.sentinel == 'bundle-only-version'",
            "platform_foundation_lock.cilium.sentinel == 'bundle-only-version'",
            "platform_buildkit_lock.cilium.sentinel == 'bundle-only-version'",
            "sandbox_runtime_lock.cilium.sentinel == 'bundle-only-version'",
            "harbor_locked_images.sentinel == 'bundle-only-version'",
            "verify_busybox_image == 'example.invalid/busybox@sha256:" + "b" * 64 + "'",
            "identity_secrets.bootstrap_password == 'synthetic-identity-password'",
        ]
        tasks.append(
            {
                "name": "Verify the selected input facts",
                "ansible.builtin.assert": {"that": checks},
                "no_log": True,
            }
        )
        locator = self.fixture.root / "secrets/identity.yaml"
        play = [
            {
                "name": "Verify only local Bundle input loading",
                "hosts": "localhost",
                "connection": "local",
                "gather_facts": False,
                "become": False,
                "vars": {
                    "labweaver_versions_lock_file": str(
                        self.fixture.root / "versions/lock.yaml"
                    ),
                    "identity_secret_locator_controller": True,
                    "identity_secret_locator_controller_tmp": str(
                        self.fixture.private / "ansible-remote-tmp"
                    ),
                    "identity_secret_locator_sha256": "sha256:"
                    + hashlib.sha256(locator.read_bytes()).hexdigest(),
                },
                "tasks": tasks,
            }
        ]
        playbook = self.fixture.directory / "input-only.yml"
        playbook.write_text(yaml.safe_dump(play))
        with patch.dict(
            os.environ, {"LABWEAVER_IDENTITY_SECRET_LOCATOR": str(locator)}
        ):
            result = subprocess.run(
                ["ansible-playbook", "-i", "localhost,", str(playbook)],
                capture_output=True,
                text=True,
            )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertNotIn("synthetic-identity-password", result.stdout + result.stderr)
        locator.chmod(0o644)
        with patch.dict(
            os.environ, {"LABWEAVER_IDENTITY_SECRET_LOCATOR": str(locator)}
        ):
            failed = subprocess.run(
                ["ansible-playbook", "-i", "localhost,", str(playbook)],
                capture_output=True,
                text=True,
            )
        self.assertNotEqual(failed.returncode, 0)
        self.assertNotIn("synthetic-identity-password", failed.stdout + failed.stderr)


    def test_deploy_wipe_uses_only_authenticated_bundle_installation(self) -> None:
        installation_id = "019fa9d0-0000-7000-8000-000000000142"
        self.fixture.model["target"]["installationId"] = installation_id
        self.save()
        self.fixture.seal()
        argv = [
            "deployment_bundle.py", "--runtime", str(self.fixture.runtime),
            "deploy", "--archive", str(self.fixture.archive),
            "--identity", str(self.fixture.identity), "--env", "demo",
            "--mode", "wipe", "--target", "labweaver", "--scope", "platform", "--yes",
        ]
        with patch.object(sys, "argv", argv), patch.object(bundle, "_consume") as consumer:
            self.assertEqual(bundle.main(), 0)
            self.assertEqual(consumer.call_count, 1)
            command, environment = consumer.call_args.args
            self.assertEqual(environment["LW_BUNDLE_INSTALLATION_ID"], installation_id)
            self.assertEqual(environment["LABWEAVER_BUNDLE_INVENTORY_FILE"], environment["ANSIBLE_INVENTORY"])
            self.assertEqual(command[:3], ["cargo", "xtask", "deploy"])
            self.assertIn("wipe", command)
        self.fixture.clean_runtime()

        # A different sealed source lacking the installation id cannot mutate.
        self.fixture.model["target"].pop("installationId")
        self.fixture.archive = self.fixture.directory / "missing-identity.age"
        self.fixture.identity = self.fixture.directory / "missing-id-key.txt"
        self.save()
        self.fixture.seal()
        rejected_argv = argv.copy()
        rejected_argv[5] = str(self.fixture.archive)
        rejected_argv[7] = str(self.fixture.identity)
        with patch.object(sys, "argv", rejected_argv), patch.object(bundle, "_consume") as consumer:
            self.assertNotEqual(bundle.main(), 0)
            consumer.assert_not_called()
        self.fixture.clean_runtime()

if __name__ == "__main__":
    unittest.main()
