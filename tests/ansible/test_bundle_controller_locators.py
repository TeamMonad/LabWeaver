"""Use real age and the actual role inputs; no cluster actions or real credentials."""

import copy
import hashlib
import json
from pathlib import Path
import subprocess
import unittest

import yaml

import test_bundle_infrastructure as infrastructure
import test_deployment_bundle as fixtures

bundle = fixtures.bundle


class BundleControllerLocatorTests(unittest.TestCase):
    def setUp(self) -> None:
        self.inputs = infrastructure.BundleInfrastructureTests(methodName="runTest")
        self.inputs.setUp()
        self.addCleanup(self.inputs.doCleanups)
        self.fixture = self.inputs.fixture
        self.secret = "synthetic-locator-secret-203"
        self.payload = (
            f"HARBOR_ADMIN_PASSWORD={self.secret}\n"
            f"HARBOR_DATABASE_PASSWORD={self.secret}\n"
            f"HARBOR_REDIS_PASSWORD={self.secret}\n"
            f"CLOUDFLARE_API_TOKEN={self.secret}\n"
        ).encode()
        self.locator = "secrets/controller-locators.env"
        fixtures.write(self.fixture.root / self.locator, self.payload)
        variables = self.inputs.inventory["all"]["vars"]
        variables["harbor_secret_locator_path"] = {"file": self.locator}
        variables["public_ingress_cloudflare_token_locator"] = {"file": self.locator}
        self.inputs.save()

    def inventory(self, rendered: Path) -> dict:
        return json.loads(
            subprocess.run(
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
            ).stdout
        )

    def playbook(self, rendered: Path, roles: tuple[str, ...]) -> Path:
        tasks = []
        for role in roles:
            tasks.append(
                {
                    "name": f"Read only {role} inputs",
                    "ansible.builtin.include_tasks": str(
                        fixtures.REPOSITORY
                        / "deploy/ansible/roles"
                        / role
                        / "tasks/secret-locator.yml"
                    ),
                }
            )
        tasks.append(
            {
                "name": "Verify the single source without printing it",
                "ansible.builtin.assert": {
                    "that": [
                        f"{name} | hash('sha256') == '{hashlib.sha256(self.secret.encode()).hexdigest()}'"
                        for role in roles
                        for name in (
                            {
                                "harbor": (
                                    "harbor_admin_password",
                                    "harbor_database_password",
                                    "harbor_redis_password",
                                ),
                                "public_ingress": ("public_ingress_cloudflare_token",),
                            }[role]
                        )
                    ]
                },
                "no_log": True,
            }
        )
        play = [
            {
                "name": "Read only controller locators for a disconnected target",
                "hosts": "fixture-cp",
                "gather_facts": False,
                "become": True,
                "vars": {"public_ingress_cloudflare_token_key": "CLOUDFLARE_API_TOKEN"},
                "tasks": tasks,
            }
        ]
        path = rendered / "controller-locators-test.yml"
        fixtures.write(path, yaml.safe_dump(play).encode())
        return path

    def run_roles(
        self, rendered: Path, roles: tuple[str, ...] = ("harbor", "public_ingress")
    ) -> subprocess.CompletedProcess:
        result = subprocess.run(
            [
                "ansible-playbook",
                "-i",
                str(rendered / "infrastructure/inventory.yaml"),
                str(self.playbook(rendered, roles)),
            ],
            capture_output=True,
            text=True,
            timeout=60,
        )
        self.assertNotIn(self.secret, result.stdout + result.stderr)
        self.assertNotIn(self.payload.decode(), result.stdout + result.stderr)
        return result

    def test_actual_roles_read_one_restored_source_without_remote_privileges(
        self,
    ) -> None:
        self.fixture.seal()
        with bundle.opened_bundle(
            self.fixture.archive, self.fixture.identity, self.fixture.runtime
        ) as (root, rendered, _):
            inventory = self.inventory(rendered)
            variables = inventory["all"]["vars"]
            digest = "sha256:" + hashlib.sha256(self.payload).hexdigest()
            self.assertEqual(
                variables["harbor_secret_locator_path"], str(root / self.locator)
            )
            self.assertEqual(
                variables["public_ingress_cloudflare_token_locator"],
                str(root / self.locator),
            )
            self.assertEqual(variables["harbor_secret_locator_revision"], digest)
            for prefix in bundle.CONTROLLER_LOCATORS.values():
                self.assertTrue(variables[f"{prefix}_controller"])
                self.assertEqual(variables[f"{prefix}_sha256"], digest)
            result = self.run_roles(rendered)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.fixture.clean_runtime()

    def test_changed_permissions_missing_and_symlink_sources_fail_without_values(
        self,
    ) -> None:
        self.fixture.seal()
        for defect in ("changed", "permissions", "missing", "symlink"):
            with self.subTest(defect=defect), bundle.opened_bundle(
                self.fixture.archive, self.fixture.identity, self.fixture.runtime
            ) as (root, rendered, _):
                locator = root / self.locator
                if defect == "changed":
                    fixtures.write(locator, self.payload + b"ROTATED=synthetic\n")
                elif defect == "permissions":
                    locator.chmod(0o644)
                elif defect == "missing":
                    locator.unlink()
                else:
                    locator.unlink()
                    other = self.fixture.private / "same-synthetic-input"
                    fixtures.write(other, self.payload)
                    locator.symlink_to(other)
                for role in ("harbor", "public_ingress"):
                    result = self.run_roles(rendered, (role,))
                    self.assertNotEqual(result.returncode, 0)
                    self.assertNotIn("UNREACHABLE", result.stdout)
            self.fixture.clean_runtime()

    def test_absent_bundle_locator_cannot_select_a_remote_default(self) -> None:
        for key in bundle.CONTROLLER_LOCATORS:
            del self.inputs.inventory["all"]["vars"][key]
        self.inputs.save()
        (self.fixture.root / self.locator).unlink()
        self.fixture.seal()
        with bundle.opened_bundle(
            self.fixture.archive, self.fixture.identity, self.fixture.runtime
        ) as (_, rendered, _):
            variables = self.inventory(rendered)["all"]["vars"]
            self.assertEqual(variables["harbor_secret_locator_path"], "")
            self.assertEqual(variables["public_ingress_cloudflare_token_locator"], "")
            self.assertTrue(variables["harbor_secret_locator_controller"])
            self.assertTrue(variables["public_ingress_cloudflare_locator_controller"])
            for role in ("harbor", "public_ingress"):
                result = self.run_roles(rendered, (role,))
                self.assertNotEqual(result.returncode, 0)
                self.assertNotIn("UNREACHABLE", result.stdout)
        self.fixture.clean_runtime()

    def test_locator_values_remain_literal_without_template_expansion(self) -> None:
        literal = "synthetic-locator-203{{ DO_NOT_EXPAND_203 }}"
        self.payload = self.payload.replace(self.secret.encode(), literal.encode())
        self.secret = literal
        fixtures.write(self.fixture.root / self.locator, self.payload)
        self.fixture.seal()
        with bundle.opened_bundle(
            self.fixture.archive, self.fixture.identity, self.fixture.runtime
        ) as (_, rendered, _):
            result = self.run_roles(rendered)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.fixture.clean_runtime()

    def test_duplicate_or_missing_credential_keys_are_rejected(self) -> None:
        self.fixture.seal()
        for role, key in (
            ("harbor", "HARBOR_ADMIN_PASSWORD"),
            ("public_ingress", "CLOUDFLARE_API_TOKEN"),
        ):
            for defect in ("duplicate", "missing"):
                with self.subTest(role=role, defect=defect), bundle.opened_bundle(
                    self.fixture.archive, self.fixture.identity, self.fixture.runtime
                ) as (root, rendered, _):
                    value = (
                        self.payload + f"{key}=synthetic-duplicate\n".encode()
                        if defect == "duplicate"
                        else b"\n".join(
                            line
                            for line in self.payload.splitlines()
                            if not line.startswith(f"{key}=".encode())
                        )
                        + b"\n"
                    )
                    fixtures.write(root / self.locator, value)
                    # Update the binding as a newly authored Bundle would, so this
                    # exercises the existing credential parser rather than hash drift.
                    inventory_path = rendered / "infrastructure/inventory.yaml"
                    original_digest = hashlib.sha256(self.payload).hexdigest()
                    inventory_path.write_text(
                        inventory_path.read_text().replace(
                            original_digest, hashlib.sha256(value).hexdigest()
                        )
                    )
                    result = self.run_roles(rendered, (role,))
                    self.assertNotEqual(result.returncode, 0)
                self.fixture.clean_runtime()

    def test_rotation_derives_both_bindings_and_host_override_identity(self) -> None:
        rotated = self.payload.replace(
            self.secret.encode(), b"synthetic-rotated-locator-203"
        )
        fixtures.write(self.fixture.root / self.locator, rotated)
        other = "secrets/host-locator.env"
        fixtures.write(self.fixture.root / other, self.payload)
        host = self.inputs.inventory["all"]["children"]["control_plane"]["hosts"][
            "fixture-cp"
        ]
        host["harbor_secret_locator_path"] = {"file": other}
        self.inputs.save()
        self.fixture.seal()
        with bundle.opened_bundle(
            self.fixture.archive, self.fixture.identity, self.fixture.runtime
        ) as (root, rendered, _):
            inventory = self.inventory(rendered)
            variables = inventory["all"]["vars"]
            self.assertEqual(
                variables["harbor_secret_locator_sha256"],
                "sha256:" + hashlib.sha256(rotated).hexdigest(),
            )
            self.assertEqual(
                variables["public_ingress_cloudflare_locator_sha256"],
                variables["harbor_secret_locator_sha256"],
            )
            actual = inventory["_meta"]["hostvars"]["fixture-cp"]
            self.assertEqual(actual["harbor_secret_locator_path"], str(root / other))
            self.assertEqual(
                actual["harbor_secret_locator_sha256"],
                "sha256:" + hashlib.sha256(self.payload).hexdigest(),
            )
            self.assertEqual(
                actual["harbor_secret_locator_revision"],
                actual["harbor_secret_locator_sha256"],
            )
        self.fixture.clean_runtime()

    def test_remote_strings_and_derived_overrides_are_rejected(self) -> None:
        original = copy.deepcopy(self.inputs.inventory)
        for key, value in (
            ("harbor_secret_locator_path", "/root/private/harbor.env"),
            ("public_ingress_cloudflare_token_locator", {"secret": self.locator}),
            ("harbor_secret_locator_controller", False),
            ("public_ingress_cloudflare_locator_sha256", "sha256:" + "0" * 64),
            ("harbor_secret_locator_revision", "handwritten-duplicate"),
        ):
            with self.subTest(key=key):
                self.inputs.inventory = copy.deepcopy(original)
                self.inputs.inventory["all"]["vars"][key] = value
                self.inputs.save()
                with self.assertRaises(bundle.BundleError):
                    bundle.load_bundle(self.fixture.root)


if __name__ == "__main__":
    unittest.main()
