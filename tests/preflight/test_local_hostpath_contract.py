from __future__ import annotations

import json
import unittest
from pathlib import Path
import importlib.util
import sys


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "validate_local_preflight_report", ROOT / "tools/validate_local_preflight_report.py"
)
assert SPEC is not None and SPEC.loader is not None
VALIDATOR = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = VALIDATOR
SPEC.loader.exec_module(VALIDATOR)


class LocalHostpathContractTests(unittest.TestCase):
    def test_report_schema_is_non_release_and_does_not_accept_secrets(self) -> None:
        schema = json.loads(
            (ROOT / "schemas/results/local-connected-non-release.v1.schema.json").read_text(
                encoding="utf-8"
            )
        )
        self.assertEqual(schema["properties"]["releaseEligible"]["const"], False)
        self.assertNotIn("token", json.dumps(schema).lower())
        self.assertNotIn("password", json.dumps(schema).lower())

    def test_sample_report_validates_and_cannot_be_promoted(self) -> None:
        sample = {
            "schemaVersion": "local-connected-non-release.v1",
            "mode": "local-hostpath",
            "releaseEligible": False,
            "sourceCommit": "a" * 40,
            "runId": "019fd0f9-a1ac-73d0-99bd-42cbc82c66e9",
            "dockerContext": "desktop-linux",
            "kubernetesContext": "docker-desktop",
            "nodeCount": 1,
            "readyNodeCount": 1,
            "storageClasses": ["hostpath"],
            "capabilities": {
                "singleReadyNode": True,
                "hostpath": True,
                "nfsRwx": False,
                "kubevirt": False,
                "cdi": False,
                "ecnuApiKey": False,
            },
            "capabilityGaps": ["LW_LOCAL_PREFLIGHT_NFS_RWX_UNAVAILABLE"],
            "blockers": ["LW_LOCAL_PREFLIGHT_NFS_RWX_UNAVAILABLE"],
            "identity": {
                "kind": "local-preflight",
                "sourceCommit": "a" * 40,
                "runId": "019fd0f9-a1ac-73d0-99bd-42cbc82c66e9",
            },
        }
        with self.subTest("valid report"):
            import tempfile

            with tempfile.TemporaryDirectory() as directory:
                path = Path(directory) / "report.json"
                path.write_text(json.dumps(sample), encoding="utf-8")
                VALIDATOR.validate(path)
        with self.subTest("promotion rejected"):
            sample["releaseEligible"] = True
            with tempfile.TemporaryDirectory() as directory:
                path = Path(directory) / "report.json"
                path.write_text(json.dumps(sample), encoding="utf-8")
                with self.assertRaisesRegex(ValueError, "SCHEMA_INVALID"):
                    VALIDATOR.validate(path)

    def test_resource_replay_identity_is_sanitized_and_required(self) -> None:
        schema = json.loads(
            (ROOT / "schemas/results/local-connected-non-release.v1.schema.json").read_text(
                encoding="utf-8"
            )
        )
        from jsonschema import Draft202012Validator

        validator = Draft202012Validator(schema)
        identity = {
            "kind": "resource-replay-plan",
            "sourceCommit": "a" * 40,
            "runId": "019fd0f9-a1ac-73d0-99bd-42cbc82c66e9",
            "profile": {"path": ".private/resource/profile.json", "sha256": "sha256:" + "a" * 64},
            "authentication": {"path": ".private/resource/auth.json", "sha256": "sha256:" + "b" * 64},
            "deploymentManifest": {"path": "artifacts/resource/deployment.json", "sha256": "sha256:" + "c" * 64},
            "packageManifest": {"path": "artifacts/resource/package.json", "sha256": "sha256:" + "d" * 64},
            "resourceImage": "harbor.example/labweaver/resource-service@sha256:" + "e" * 64,
            "configurationBundleSha256": "sha256:" + "f" * 64,
        }
        report = {
            "schemaVersion": "local-connected-non-release.v1",
            "mode": "local-hostpath",
            "releaseEligible": False,
            "sourceCommit": "a" * 40,
            "runId": "019fd0f9-a1ac-73d0-99bd-42cbc82c66e9",
            "dockerContext": "desktop-linux",
            "kubernetesContext": "docker-desktop",
            "nodeCount": 1,
            "readyNodeCount": 1,
            "storageClasses": ["hostpath"],
            "capabilities": {
                "singleReadyNode": True,
                "hostpath": True,
                "nfsRwx": False,
                "kubevirt": False,
                "cdi": False,
                "ecnuApiKey": False,
            },
            "capabilityGaps": ["LW_LOCAL_PREFLIGHT_NFS_RWX_UNAVAILABLE"],
            "blockers": ["LW_LOCAL_PREFLIGHT_NFS_RWX_UNAVAILABLE"],
            "identity": identity,
        }
        validator.validate(report)
        identity["profile"]["path"] = "C:/private/profile.json"
        with self.assertRaises(Exception):
            validator.validate(report)

    def test_local_provider_overlay_disables_kubevirt_and_uses_hostpath(self) -> None:
        profile = json.loads(
            (ROOT / "deploy/config/environment-providers.local-hostpath.example.json").read_text(
                encoding="utf-8"
            )
        )
        self.assertEqual(profile["mode"], "local-hostpath")
        self.assertFalse(profile["releaseEligible"])
        self.assertEqual(profile["container"]["workspaceStorageClassName"], "hostpath")
        self.assertFalse(profile["kubevirt"]["enabled"])

        values = (ROOT / "deploy/helm/labweaver/values.local-hostpath.yaml").read_text(
            encoding="utf-8"
        )
        self.assertIn("storageClassName: hostpath", values)
        self.assertIn("resource_service: harbor.internal.example/labweaver-system/resource-service@sha256:", values)
        self.assertIn("kubevirt-executor:", values)
        self.assertIn("enabled: false", values)

    def test_controller_runner_never_uses_ssh_or_wsl(self) -> None:
        runner = (ROOT / "tools/docker_controller.py").read_text(encoding="utf-8")
        self.assertNotIn('"ssh"', runner.lower())
        self.assertNotIn('"wsl"', runner.lower())
        self.assertIn("--env-file", runner)
        self.assertIn("readonly", runner)
        self.assertIn('"docker",\n                "build"', runner)

    def test_controller_image_and_playbook_are_pinned_read_only_inputs(self) -> None:
        containerfile = (ROOT / "containers/Containerfile.controller").read_text(
            encoding="utf-8"
        )
        self.assertIn("kubectl:v1.34.1@sha256:", containerfile)
        self.assertIn("jsonschema==4.24.0", containerfile)
        self.assertIn("CARGO_HOME=/tmp/cargo", containerfile)
        self.assertIn("CARGO_TARGET_DIR=/workspace/target/controller", containerfile)
        self.assertIn(
            'cargo", "+1.97.1-x86_64-unknown-linux-gnu", "run"', containerfile
        )
        self.assertIn("CARGO_REGISTRIES_CRATES_IO_PROTOCOL=sparse", containerfile)
        self.assertNotIn("--privileged", containerfile)
        self.assertNotIn("hostNetwork", containerfile)

        playbook = (
            ROOT / "deploy/ansible/playbooks/local-hostpath-preflight.yml"
        ).read_text(encoding="utf-8")
        self.assertIn("LW_LOCAL_PREFLIGHT_NFS_RWX_UNAVAILABLE", playbook)
        self.assertIn("LW_LOCAL_PREFLIGHT_KUBEVIRT_CDI_UNAVAILABLE", playbook)
        self.assertNotIn("kubernetes.core.k8s:", playbook)
        self.assertNotIn("kubectl apply", playbook)

        controller_lock = (
            ROOT / "deploy/ansible/controller.lock.yml"
        ).read_text(encoding="utf-8")
        versions_lock = (ROOT / "deploy/versions.lock.yml").read_text(encoding="utf-8")
        for fragment in (
            "ansible_core: 2.18.6",
            "ansible_lint: 25.6.1",
            "python_kubernetes: 34.1.0",
            "python_jsonschema: 4.24.0",
            "helm: v3.21.3",
            "kubectl: v1.34.1",
        ):
            self.assertIn(fragment, versions_lock)
        self.assertIn("helm_version: 3.21.3", controller_lock)
        self.assertIn("python_jsonschema_version: 4.24.0", controller_lock)
        self.assertIn(
            "helm_linux_amd64_sha256: 35da09ba0716fc7c3cd63b6b31ee380a9c7662e95f29ab0e4ae962420afd315b",
            controller_lock,
        )

    def test_platform_image_workflow_builds_resource_service(self) -> None:
        workflow = (ROOT / ".github/workflows/platform-images.yml").read_text(
            encoding="utf-8"
        )
        self.assertIn("- resource-service", workflow)
        self.assertIn("services/**", workflow)
        self.assertIn("Cargo.lock", workflow)

    def test_verifier_workflow_installs_its_pinned_schema_dependency(self) -> None:
        workflow = (ROOT / ".github/workflows/vm01a-preflight.yml").read_text(
            encoding="utf-8"
        )
        requirements = (ROOT / "tools/requirements-preflight.txt").read_text(
            encoding="utf-8"
        )
        self.assertIn("requirements-preflight.txt", workflow)
        self.assertIn("jsonschema==4.24.0", requirements)

    def test_local_stack_overlay_and_plan_are_non_release_and_no_write(self) -> None:
        schema = json.loads(
            (ROOT / "schemas/infrastructure/local-hostpath-stack-overlay.v1.schema.json").read_text(
                encoding="utf-8"
            )
        )
        overlay = json.loads(
            (ROOT / "deploy/config/local-hostpath-stack.overlay.json").read_text(
                encoding="utf-8"
            )
        )
        from jsonschema import Draft202012Validator

        Draft202012Validator(schema).validate(overlay)
        self.assertFalse(overlay["spec"]["releaseEligible"])
        self.assertFalse(overlay["spec"]["writePolicy"]["apply"])
        self.assertFalse(overlay["spec"]["writePolicy"]["delete"])
        self.assertEqual(
            {item["name"] for item in overlay["spec"]["dependencies"]},
            {"postgresql", "nats", "minio", "keycloak", "harbor", "buildkit", "labweaver"},
        )

        plan = (ROOT / "deploy/ansible/playbooks/local-hostpath-stack-plan.yml").read_text(
            encoding="utf-8"
        )
        self.assertIn("plan-only", plan)
        self.assertNotIn("kubernetes.core.k8s:", plan)
        self.assertNotIn("kubectl apply", plan)
        self.assertNotIn("kubectl delete", plan)
        self.assertNotIn("helm upgrade", plan)
        self.assertNotIn("helm uninstall", plan)


if __name__ == "__main__":
    unittest.main()
