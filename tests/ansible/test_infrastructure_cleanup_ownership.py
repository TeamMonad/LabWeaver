"""Execute ownership guards and native task arguments at the Kubernetes boundary.

The command fixture replaces only Kubernetes transport. It implements API UID /
resourceVersion preconditions and JSON Patch test operations, so a replacement
between observation and deletion/patch fails without recording a mutation.
Stat, slurp, systemctl loop evaluation and OpenSSL key generation run for real.
"""
from __future__ import annotations

import base64
import copy
import json
import os
from pathlib import Path
import sys
import subprocess
import tempfile
import unittest

from test_deployment_install_modes import INSTALLATION, ROOT, named, run_assertion, tasks
from test_deployment_ownership_execution import run_tasks

ROLES = ROOT / "deploy/ansible/roles"
BOUNDARY = '''import copy, json, os, pathlib, sys
payload = json.load(sys.stdin)
current = json.loads(os.environ['CURRENT_OBJECT'])
updated = copy.deepcopy(current)
try:
    if payload.get('state') == 'absent':
        expected = payload.get('delete_options', {}).get('preconditions', {})
        for field in ('uid', 'resourceVersion'):
            if field in expected:
                assert expected[field] == current['metadata'][field], field
        updated = None
    else:
        for operation in payload['patch']:
            parts = [part.replace('~1', '/').replace('~0', '~') for part in operation['path'].split('/')[1:]]
            parent = updated
            for part in parts[:-1]:
                parent = parent[part]
            key = parts[-1]
            if operation['op'] == 'test':
                assert parent[key] == operation['value'], operation['path']
            elif operation['op'] == 'remove':
                del parent[key]
            elif operation['op'] == 'add':
                parent[key] = operation['value']
            else:
                raise AssertionError(operation['op'])
except (AssertionError, KeyError) as error:
    print('Kubernetes precondition rejected:', error, file=sys.stderr)
    sys.exit(1)
pathlib.Path(os.environ['CAPTURE']).write_text(json.dumps({'payload': payload, 'object': updated}))
'''


def role_tasks(role: str, source: str = "main.yml") -> list[dict]:
    return tasks(f"deploy/ansible/roles/{role}/tasks/{source}")


def observed(kind: str, name: str = "target", installation: str = INSTALLATION) -> dict:
    return {"apiVersion": "apps/v1" if kind == "DaemonSet" else "v1", "kind": kind,
            "metadata": {"name": name, "namespace": "labweaver-system", "uid": "observed-uid",
                         "resourceVersion": "17", "annotations": {"labweaver.io/installation-id": installation}}}


def native_kubernetes_boundary(task: dict, command: Path) -> dict:
    """Keep the real task loop/when and native arguments; replace external I/O."""
    result = copy.deepcopy(task)
    module = next(key for key in result if key.startswith("kubernetes.core."))
    payload = result.pop(module)
    result.setdefault("vars", {})["boundary_payload"] = payload
    result["ansible.builtin.command"] = {
        "argv": [sys.executable, str(command)], "stdin": "{{ boundary_payload | to_json }}",
    }
    return result


def localize_template_lookups(value, role: str):
    if isinstance(value, dict):
        return {key: localize_template_lookups(child, role) for key, child in value.items()}
    if isinstance(value, list):
        return [localize_template_lookups(child, role) for child in value]
    if isinstance(value, str):
        return value.replace("lookup('template', '", f"lookup('template', '{ROLES / role / 'templates'}/")
    return value


class InfrastructureCleanupOwnershipTests(unittest.TestCase):
    def assert_gateway_consumer_accepts(self, gateway):
        consumer = named(role_tasks('platform_application', 'ownership.yml'),
                         'Reject foreign cross-namespace application mutation targets')
        result = run_assertion(consumer, {
            'platform_application_installation_id': INSTALLATION,
            'platform_application_release_name': 'labweaver', 'platform_application_namespace': 'labweaver-system',
            'platform_application_cross_namespace_ownership': {'results': [{'resources': [gateway]}]},
        })
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_namespace_and_full_target_guards_reject_foreign_objects(self):
        for role, guards, observation in (
            ("public_ingress", ("Refuse foreign public ingress namespaces",
                                "Refuse to overwrite or delete any foreign public ingress object"),
             ("public_ingress_target_namespaces", "public_ingress_object_ownership")),
            ("gpu_device_plugin", ("Refuse to overwrite or delete any foreign GPU object",),
             ("gpu_device_plugin_object_ownership",)),
        ):
            source = role_tasks(role, "ownership.yml")
            for guard_name, variable in zip(guards, observation):
                guard = named(source, guard_name)
                for kind in ("Namespace", "Gateway", "Secret", "ConfigMap", "DaemonSet", "ClusterIssuer"):
                    for installation, accepted in ((INSTALLATION, True), ("foreign", False), ("", False)):
                        with self.subTest(role=role, guard=guard_name, kind=kind, identity=installation):
                            entry = {"resources": [observed(kind, installation=installation)]}
                            result = run_assertion(guard, {"labweaver_installation_id": INSTALLATION,
                                                           variable: {"results": [entry]}})
                            self.assertEqual(result.returncode == 0, accepted, result.stdout + result.stderr)
                result = run_assertion(guard, {"labweaver_installation_id": INSTALLATION,
                                               variable: {"results": [{"resources": []}]}})
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_public_inventory_and_gateway_producers_cover_actual_targets(self):
        role = "public_ingress"
        defaults = tasks("deploy/ansible/roles/public_ingress/defaults/main.yml")
        variables = dict(defaults, labweaver_installation_id=INSTALLATION,
                         public_ingress_runtime_base_hostname="runtime.example.test")
        source = role_tasks(role, "ownership.yml")
        bind = [task for task in source if "ansible.builtin.set_fact" in task]
        for mode in ("selfsigned", "acme"):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as directory:
                variables["public_ingress_tls_mode"] = mode
                capture = Path(directory) / "inventory.json"
                record = {"name": "Capture complete mutation inventory", "ansible.builtin.copy": {
                    "dest": str(capture), "content": "{{ public_ingress_planned_objects | to_json }}", "mode": "0600"}}
                result = run_tasks(localize_template_lookups(bind, role) + [record], variables)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                inventory = json.loads(capture.read_text())
                targets = {(item["kind"], item["metadata"].get("namespace"), item["metadata"]["name"]) for item in inventory}
                self.assertIn(("Secret", variables["public_ingress_namespace"], variables["public_ingress_certificate_secret"]), targets)
                for kind in ("DaemonSet", "ConfigMap"):
                    for namespace in (variables["public_ingress_namespace"], variables["public_ingress_entry_namespace"]):
                        self.assertIn((kind, namespace, variables["public_ingress_entry_name"]), targets)
                for route in variables["public_ingress_routes"]:
                    self.assertIn(("HTTPRoute", route["namespace"], route["name"]), targets)
                for item in inventory:
                    if item["kind"] in ("Gateway", "HTTPRoute", "Certificate", "ClusterIssuer"):
                        self.assertEqual(item["metadata"]["annotations"]["labweaver.io/installation-id"], INSTALLATION)
                    if item["kind"] == "Certificate":
                        self.assertEqual(item["spec"]["secretTemplate"]["annotations"]["labweaver.io/installation-id"], INSTALLATION)
                    if item['kind'] == 'Gateway':
                        self.assert_gateway_consumer_accepts(item)
                if mode == "acme":
                    self.assertIn(("Secret", "cert-manager", variables["public_ingress_acme_issuer"] + "-account-key"), targets)
                    self.assertIn(("Secret", "cert-manager", variables["public_ingress_cloudflare_secret_name"]), targets)
        with tempfile.TemporaryDirectory() as directory:
            capture = Path(directory) / "gateway.json"
            result = run_tasks([{"name": "Render infrastructure Gateway producer", "ansible.builtin.copy": {
                "dest": str(capture), "mode": "0600", "content":
                "{{ lookup('template', '" + str(ROLES / "cluster_addons/templates/gateway.yml.j2") + "') | from_yaml_all | list | to_json }}"}}],
                {"labweaver_installation_id": INSTALLATION, "labweaver_namespaces": ["labweaver-demo"], "gateway_vip": "10.0.0.2"})
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            gateway = next(item for item in json.loads(capture.read_text()) if item["kind"] == "Gateway")
            self.assertEqual(gateway["metadata"]["annotations"]["labweaver.io/installation-id"], INSTALLATION)
            self.assert_gateway_consumer_accepts(gateway)

    def test_acme_account_key_has_a_real_producer_and_never_replaces_existing_secret(self):
        source = role_tasks('public_ingress')
        entries = [named(source, name) for name in (
            'Select the verified ACME account key observation',
            'Generate the ACME account private key only for a verified absent target',
            'Create the installation-owned ACME account key without replacing an existing Secret',
        )]
        with tempfile.TemporaryDirectory() as directory:
            capture, kubectl = Path(directory) / 'account.json', Path(directory) / 'kubectl'
            kubectl.write_text(f'#!{sys.executable}\nimport os, sys\nassert sys.argv[-3:] == ["create", "-f", "-"]\nwith open(os.environ["CAPTURE"], "x") as output:\n    output.write(sys.stdin.read())\n')
            kubectl.chmod(0o755)
            variables = {'labweaver_installation_id': INSTALLATION, 'public_ingress_tls_mode': 'acme',
                         'public_ingress_acme_issuer': 'labweaver-acme', 'public_ingress_object_ownership': {'results': [{
                             'item': {'kind': 'Secret', 'metadata': {'name': 'labweaver-acme-account-key', 'namespace': 'cert-manager'}},
                             'resources': []}]}}
            environment = {'PATH': directory + os.pathsep + os.environ['PATH'], 'CAPTURE': str(capture)}
            result = run_tasks(entries, variables, environment)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            secret = json.loads(capture.read_text())
            self.assertEqual(secret['metadata']['annotations']['labweaver.io/installation-id'], INSTALLATION)
            key = base64.b64decode(secret['data']['tls.key'])
            valid = subprocess.run(['openssl', 'pkey', '-check', '-noout'], input=key, capture_output=True, check=False)
            self.assertEqual(valid.returncode, 0, valid.stderr.decode())
            capture.unlink()
            variables['public_ingress_object_ownership']['results'][0]['resources'] = [secret]
            result = run_tasks(entries, variables, environment)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertFalse(capture.exists(), 'existing account credentials must not be regenerated or overwritten')

    def test_legacy_deletions_bind_observed_uid_and_revision(self):
        for role, name in (("gpu_device_plugin", "Remove the legacy single-mode NVIDIA device-plugin DaemonSet"),
                           ("public_ingress", "Remove the legacy public entry objects from the application namespace")):
            task = named(role_tasks(role), name)
            target = observed("DaemonSet", "nvidia-gpu-device-plugin" if role == "gpu_device_plugin" else "labweaver-public-entry")
            observation = {"item": {"kind": target["kind"], "metadata": target["metadata"]}, "resources": [target]}
            variables = {"gpu_device_plugin_enabled": True, "gpu_device_plugin_remove_legacy_daemonset": True,
                         "gpu_device_plugin_namespace": "labweaver-system", "gpu_device_plugin_kubeconfig": "/unused",
                         "gpu_device_plugin_legacy": observation, "public_ingress_entry_enabled": True,
                         "public_ingress_namespace": "labweaver-system", "public_ingress_entry_namespace": "entry",
                         "public_ingress_entry_name": target["metadata"]["name"],
                         "public_ingress_object_ownership": {"results": [observation]}}
            for field in (None, "uid", "resourceVersion"):
                with self.subTest(role=role, replacement=field), tempfile.TemporaryDirectory() as directory:
                    command, capture = Path(directory) / "kubernetes.py", Path(directory) / "mutation.json"
                    command.write_text(BOUNDARY)
                    current = copy.deepcopy(target)
                    if field:
                        current["metadata"][field] = "replaced"
                    result = run_tasks([native_kubernetes_boundary(task, command)], variables,
                                       {"CURRENT_OBJECT": json.dumps(current), "CAPTURE": str(capture)})
                    self.assertEqual(result.returncode == 0, field is None, result.stdout + result.stderr)
                    self.assertEqual(capture.exists(), field is None)

    def test_gpu_selected_node_conflicts_and_atomic_bounded_patches(self):
        source = role_tasks("gpu_device_plugin")
        guard = named(role_tasks("gpu_device_plugin", "ownership.yml"), "Refuse conflicting GPU label ownership on selected nodes")
        node = observed("Node", "gpu-worker")
        node["metadata"]["labels"] = {"unrelated": "preserved"}
        node["metadata"]["annotations"] = {"unrelated": "preserved"}
        observation = {"item": "gpu-worker", "resources": [node]}
        variables = {"labweaver_installation_id": INSTALLATION, "gpu_device_plugin_node_label": "labweaver.io/gpu-mode",
                     "gpu_device_plugin_selected_observations": {"results": [observation]}, "gpu_device_plugin_enabled": True,
                     "gpu_device_plugin_kubeconfig": "/unused", "gpu_device_plugin_exclusive_nodes": ["gpu-worker"],
                     "gpu_device_plugin_shared_nodes": ["gpu-worker"]}
        for owner, has_label, accepted in ((None, False, True), (None, True, False), (INSTALLATION, True, True), ("foreign", False, False)):
            if owner is None:
                node["metadata"]["annotations"].pop("labweaver.io/gpu-plugin-installation-id", None)
            else:
                node["metadata"]["annotations"]["labweaver.io/gpu-plugin-installation-id"] = owner
            if has_label:
                node["metadata"]["labels"]["labweaver.io/gpu-mode"] = "shared"
            else:
                node["metadata"]["labels"].pop("labweaver.io/gpu-mode", None)
            result = run_assertion(guard, variables)
            self.assertEqual(result.returncode == 0, accepted, result.stdout + result.stderr)
        node["metadata"]["annotations"]["labweaver.io/gpu-plugin-installation-id"] = INSTALLATION
        for mode in ("exclusive", "shared"):
            task = named(source, f"Label nodes selected for {mode} GPU allocation")
            for field in (None, "uid", "resourceVersion"):
                with self.subTest(mode=mode, replacement=field), tempfile.TemporaryDirectory() as directory:
                    command, capture = Path(directory) / "kubernetes.py", Path(directory) / "mutation.json"
                    command.write_text(BOUNDARY)
                    current = copy.deepcopy(node)
                    if field:
                        current["metadata"][field] = "replaced"
                    result = run_tasks([native_kubernetes_boundary(task, command)], variables,
                                       {"CURRENT_OBJECT": json.dumps(current), "CAPTURE": str(capture)})
                    self.assertEqual(result.returncode == 0, field is None, result.stdout + result.stderr)
                    self.assertEqual(capture.exists(), field is None)
                    if field is None:
                        patched = json.loads(capture.read_text())["object"]["metadata"]
                        self.assertEqual(patched["labels"]["labweaver.io/gpu-mode"], mode)
                        self.assertEqual(patched["labels"]["unrelated"], "preserved")
                        self.assertEqual(patched["annotations"]["unrelated"], "preserved")
                        self.assertEqual(patched["annotations"]["labweaver.io/gpu-plugin-installation-id"], INSTALLATION)

    def test_gpu_stale_cleanup_ignores_other_installations_and_rejects_replacement(self):
        task = named(role_tasks("gpu_device_plugin"), "Remove stale GPU mode labels owned by this role")
        node = observed("Node", "old-gpu")
        node["metadata"]["labels"] = {"labweaver.io/gpu-mode": "shared", "labweaver.io/gpu-plugin-managed": "true", "unrelated": "preserved"}
        variables = {"labweaver_installation_id": INSTALLATION, "gpu_device_plugin_enabled": True,
                     "gpu_device_plugin_kubeconfig": "/unused", "gpu_device_plugin_node_label": "labweaver.io/gpu-mode",
                     "gpu_device_plugin_selected_nodes": [], "gpu_device_plugin_managed_nodes": {"resources": [node]}}
        for owner, replacement in ((INSTALLATION, None), ("foreign", None), ("", None), (INSTALLATION, "uid"), (INSTALLATION, "resourceVersion")):
            with self.subTest(owner=owner, replacement=replacement), tempfile.TemporaryDirectory() as directory:
                node["metadata"]["annotations"]["labweaver.io/gpu-plugin-installation-id"] = owner
                current = copy.deepcopy(node)
                if replacement:
                    current["metadata"][replacement] = "replaced"
                command, capture = Path(directory) / "kubernetes.py", Path(directory) / "mutation.json"
                command.write_text(BOUNDARY)
                result = run_tasks([native_kubernetes_boundary(task, command)], variables,
                                   {"CURRENT_OBJECT": json.dumps(current), "CAPTURE": str(capture)})
                self.assertEqual(result.returncode == 0, replacement is None, result.stdout + result.stderr)
                self.assertEqual(capture.exists(), owner == INSTALLATION and replacement is None)
                if capture.exists():
                    self.assertEqual(json.loads(capture.read_text())["object"]["metadata"]["labels"], {"unrelated": "preserved"})

    def test_sandbox_runtimeclass_boundary_and_producer(self):
        source = role_tasks("sandbox_runtime")
        guard = named(source, "Refuse a foreign sandbox RuntimeClass")
        for installation, accepted in ((INSTALLATION, True), ("foreign", False), ("", False)):
            result = run_assertion(guard, {"labweaver_installation_id": INSTALLATION,
                "sandbox_runtime_existing_runtimeclass": {"resources": [observed("RuntimeClass", installation=installation)]}})
            self.assertEqual(result.returncode == 0, accepted, result.stdout + result.stderr)
        block = named(source, "Publish the sandbox RuntimeClass from the control plane")["block"]
        producer = named(block, "Reconcile the sandbox RuntimeClass")["kubernetes.core.k8s"]["definition"]
        with tempfile.TemporaryDirectory() as directory:
            capture = Path(directory) / "runtimeclass.json"
            result = run_tasks([{"name": "Capture RuntimeClass producer", "ansible.builtin.copy": {
                "dest": str(capture), "content": "{{ producer | to_json }}", "mode": "0600"}}],
                {"producer": producer, "labweaver_installation_id": INSTALLATION, "sandbox_runtime_name": "labweaver-sandbox"})
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual(json.loads(capture.read_text())["metadata"]["annotations"]["labweaver.io/installation-id"], INSTALLATION)

    def test_gvisor_stamp_absence_is_distinct_from_read_error(self):
        block = named(role_tasks("sandbox_runtime"), "Install the sandbox runtime on sandbox-capable workers")["block"]
        entries = [named(block, name) for name in ("Inspect whether a gVisor release stamp exists", "Read the installed gVisor release stamp",
                                                  "Decide whether the locked gVisor release must be installed")]
        for state, expected in (("absent", True), ("current", False), ("old", True), ("directory", None)):
            with self.subTest(state=state), tempfile.TemporaryDirectory() as directory:
                stamp, capture = Path(directory) / ".gvisor-version", Path(directory) / "decision.json"
                if state == "directory":
                    stamp.mkdir()
                elif state != "absent":
                    stamp.write_text("release-20251001.0" if state == "current" else "release-20240901.0")
                variables = {"sandbox_runtime_install_dir": directory, "sandbox_runtime_lock": {"gvisor": {"version": "release-20251001.0"}},
                             "sandbox_runtime_installed": {"results": [{"stat": {"exists": True}}]}}
                record = {"name": "Capture installation decision only after successful read", "ansible.builtin.copy": {
                    "dest": str(capture), "content": "{{ sandbox_runtime_needs_install | to_json }}", "mode": "0600"}}
                result = run_tasks(entries + [record], variables)
                self.assertEqual(result.returncode == 0, expected is not None, result.stdout + result.stderr)
                self.assertEqual(capture.exists(), expected is not None)
                if capture.exists():
                    self.assertEqual(json.loads(capture.read_text()), expected)

    def test_runtime_service_loop_accepts_only_explicit_active_and_inactive_status(self):
        block = named(role_tasks("sandbox_runtime"), "Install the sandbox runtime on sandbox-capable workers")["block"]
        entries = [named(block, "Read the container runtime and kubelet service states"),
                   named(block, "Require the container runtime and kubelet to be the active runtimes")]
        with tempfile.TemporaryDirectory() as directory:
            systemctl = Path(directory) / "systemctl"
            systemctl.write_text(f"#!{sys.executable}\nimport os, sys\nold = sys.argv[-1] == 'crio'\nprint(os.environ['OLD_STATE'] if old else 'active')\nsys.exit(int(os.environ['OLD_RC']) if old else 0)\n")
            systemctl.chmod(0o755)
            for rc, stdout, accepted in ((3, "inactive", True), (0, "active", False), (4, "unknown", False), (1, "inactive", False), (2, "permission denied", False)):
                with self.subTest(rc=rc, stdout=stdout):
                    result = run_tasks(entries, {"sandbox_runtime_containerd_service": "containerd", "sandbox_runtime_superseded_service": "crio"},
                                       {"PATH": directory + os.pathsep + os.environ["PATH"], "OLD_RC": str(rc), "OLD_STATE": stdout})
                    self.assertEqual(result.returncode == 0, accepted, result.stdout + result.stderr)
