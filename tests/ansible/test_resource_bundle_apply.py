"""Resource deployment keeps its explicit Bundle namespace at the Kubernetes boundary."""

from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import yaml

ROOT = Path(__file__).resolve().parents[2]
MODULE_PATH = ROOT / "deploy/ansible/library/labweaver_resource_bundle_apply.py"
SPEC = importlib.util.spec_from_file_location("resource_bundle_apply_test", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
APPLY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(APPLY)


class ModuleResult(Exception):
    def __init__(self, payload: dict) -> None:
        super().__init__(payload)
        self.payload = payload


class ResourceBundleApplyTests(unittest.TestCase):
    def test_namespace_is_required_without_default(self) -> None:
        result = subprocess.run(
            [sys.executable, str(MODULE_PATH)],
            input=json.dumps({"ANSIBLE_MODULE_ARGS": {"kubeconfig": "unused", "bundle": "unused"}}),
            text=True, capture_output=True, check=False,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("namespace", json.loads(result.stdout)["msg"])

    def exercise_apply(self, namespace: str, document_namespace: str, *, secret_matches: bool = True) -> tuple[dict, list[list[str]], list[dict]]:
        with tempfile.TemporaryDirectory() as directory:
            kubeconfig = Path(directory) / "kubeconfig"
            kubeconfig.write_text("synthetic Kubernetes boundary")
            bundle = Path(directory) / "bundle.yaml"
            secret_data = {"nats.creds": "c3ludGhldGlj"}
            documents = [
                {"apiVersion": "v1", "kind": "ConfigMap", "metadata": {"name": "resource-service-config", "namespace": document_namespace}, "data": {"resource.json": "{}"}},
                {"apiVersion": "v1", "kind": "Secret", "metadata": {"name": "resource-service-secrets", "namespace": document_namespace}, "data": secret_data},
            ]
            bundle.write_text(yaml.safe_dump_all(documents))
            calls: list[list[str]] = []
            applied: list[dict] = []

            class Invocation:
                params = {"kubeconfig": str(kubeconfig), "bundle": str(bundle), "namespace": namespace}

                def fail_json(self, **payload: object) -> None:
                    raise ModuleResult({"failed": True, **payload})

                def exit_json(self, **payload: object) -> None:
                    raise ModuleResult(payload)

            def kubernetes(arguments: list[str], _diagnostic: str) -> subprocess.CompletedProcess[str]:
                calls.append(arguments)
                if "apply" in arguments:
                    applied.append(yaml.safe_load(Path(arguments[-1]).read_text()))
                    return subprocess.CompletedProcess(arguments, 0, "configured", "")
                observed = secret_data if secret_matches else {"nats.creds": "foreign"}
                return subprocess.CompletedProcess(arguments, 0, json.dumps({"data": observed}), "")

            with patch.object(APPLY, "AnsibleModule", return_value=Invocation()), patch.object(APPLY, "command", side_effect=kubernetes):
                with self.assertRaises(ModuleResult) as result:
                    APPLY.main()
            return result.exception.payload, calls, applied

    def test_default_and_nondefault_namespace_are_used_for_get_and_apply(self) -> None:
        for namespace in ("labweaver-system", "science-sandbox"):
            with self.subTest(namespace=namespace):
                result, calls, applied = self.exercise_apply(namespace, namespace)
                self.assertFalse(result.get("failed", False), result)
                self.assertEqual(result["diagnostic_code"], "RESOURCE_APPLICATION_BUNDLE_APPLIED")
                self.assertEqual(len(calls), 2)
                for call in calls:
                    self.assertEqual(call[call.index("--namespace") + 1], namespace)
                self.assertEqual([document["kind"] for document in applied], ["ConfigMap"])
                self.assertEqual(applied[0]["metadata"]["namespace"], namespace)

    def test_mismatched_bundle_namespace_blocks_all_kubernetes_calls(self) -> None:
        result, calls, applied = self.exercise_apply("science-sandbox", "labweaver-system")
        self.assertTrue(result["failed"])
        self.assertEqual(result["diagnostic_code"], "RESOURCE_APPLICATION_BUNDLE_INPUT_INVALID")
        self.assertEqual(calls, [])
        self.assertEqual(applied, [])

    def test_secret_conflict_prevents_configmap_apply(self) -> None:
        result, calls, applied = self.exercise_apply("science-sandbox", "science-sandbox", secret_matches=False)
        self.assertTrue(result["failed"])
        self.assertEqual(result["diagnostic_code"], "RESOURCE_APPLICATION_SECRET_OWNERSHIP_CONFLICT")
        self.assertEqual(len(calls), 1)
        self.assertEqual(applied, [])

    def test_role_passes_explicit_namespace(self) -> None:
        tasks = yaml.safe_load((ROOT / "deploy/ansible/roles/resource_application/tasks/main.yml").read_text())
        invocation = next(task["labweaver_resource_bundle_apply"] for task in tasks if "labweaver_resource_bundle_apply" in task)
        self.assertEqual(invocation["namespace"], "{{ resource_application_namespace }}")


if __name__ == "__main__":
    unittest.main()
