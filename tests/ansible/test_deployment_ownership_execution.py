"""Execute Bundle ownership tasks against external command/HTTP observations."""
from __future__ import annotations

import copy
import http.server
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import unittest

import yaml

from test_deployment_install_modes import INSTALLATION, named, probe_command, run_assertion, sample_object, tasks


def run_tasks(entries: list[dict], variables: dict, environment: dict | None = None):
    with tempfile.TemporaryDirectory() as directory:
        playbook = Path(directory) / "check.yml"
        playbook.write_text(yaml.safe_dump([{
            "name": "Exercise deployment ownership boundary", "hosts": "localhost",
            "connection": "local", "gather_facts": False, "tasks": entries,
            "vars": variables, "environment": environment or {}, "become": False,
        }]))
        return subprocess.run(
            ["ansible-playbook", "-i", "localhost,", str(playbook)],
            text=True, capture_output=True, check=False,
            env=dict(os.environ, **(environment or {})),
        )


class OwnershipExecutionTests(unittest.TestCase):
    def test_actual_nats_loop_tasks_use_each_registered_result(self):
        source = tasks("deploy/ansible/roles/platform_application/tasks/main.yml")
        for task_name, config_name, config, absent in (
            ("Inspect exact configured streams without mutation", "platform_application_nats_streams",
             [{"name": "events"}, {"name": "commands"}], "stream not found"),
            ("Inspect exact configured durable consumers without mutation", "platform_application_nats_consumers",
             [{"name": "consumer", "stream": "events"}, {"name": "second", "stream": "commands"}], "consumer not found"),
        ):
            task = named(source, task_name)
            with tempfile.TemporaryDirectory() as directory:
                command = Path(directory) / "nats"
                command.write_text(f"#!{sys.executable}\nimport os, sys\nprint(os.environ['PROBE_OUTPUT'])\nsys.exit(int(os.environ['PROBE_RC']))\n")
                command.chmod(0o755)
                variables = {
                    config_name: config, "platform_application_nats": str(command),
                    "platform_application_nats_probe_server": "tls://localhost:4222",
                    "platform_application_nats_credentials_file": "credentials",
                    "platform_application_nats_ca_file": "ca.crt",
                    "platform_application_nats_client_certificate_file": "tls.crt",
                    "platform_application_nats_client_private_key_file": "tls.key",
                }
                for rc, output, accepted in ((0, "{}", True), (1, absent, True), (2, "permission denied", False)):
                    with self.subTest(task=task_name, rc=rc):
                        result = run_tasks([task], variables, {"PROBE_RC": str(rc), "PROBE_OUTPUT": output})
                        self.assertEqual(result.returncode == 0, accepted, result.stdout + result.stderr)

    def test_actual_digest_loop_accepts_200_and_rejects_absence_and_server_errors(self):
        source = tasks("deploy/ansible/roles/platform_application/tasks/main.yml")
        probe = named(source, "Verify the reviewed container base image digests are present in the retained Harbor project")
        require = named(source, "Require every reviewed container base image digest in the retained Harbor project")
        status = [200]

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                self.send_response(status[0])
                self.end_headers()

            def log_message(self, *_args):
                pass

        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            variables = {
                "platform_application_harbor_api": f"http://127.0.0.1:{server.server_port}",
                "platform_application_harbor_project": "reviewed", "platform_application_harbor_admin_username": "admin",
                "platform_application_harbor_admin_password_raw": {"content": "c2VjcmV0"},
                "platform_application_harbor_ca_file": "/etc/ssl/certs/ca-certificates.crt",
                "platform_application_container_images": [
                    {"name": "base", "binding": "base", "reviewed_digest": "sha256:" + "a" * 64},
                    {"name": "other", "binding": "other", "reviewed_digest": "sha256:" + "b" * 64},
                ],
            }
            for code, accepted_probe in ((200, True), (404, True), (500, False)):
                with self.subTest(status=code):
                    status[0] = code
                    result = run_tasks([probe], variables)
                    self.assertEqual(result.returncode == 0, accepted_probe, result.stdout + result.stderr)
                    if code in (200, 404):
                        result = run_tasks([probe, require], variables)
                        self.assertEqual(result.returncode == 0, code == 200, result.stdout + result.stderr)
        finally:
            server.shutdown()
            server.server_close()
            thread.join()

    def test_harbor_namespace_and_exact_release_reject_other_installations(self):
        source = tasks("deploy/ansible/roles/harbor/tasks/main.yml")
        guard = named(source, "Reject an existing namespace not labelled as LabWeaver infrastructure")
        namespace = {"metadata": {"name": "harbor", "labels": {"app.kubernetes.io/part-of": "labweaver-infrastructure"},
                                  "annotations": {"labweaver.io/installation-id": INSTALLATION}}}
        variables = {"harbor_namespace": "harbor", "labweaver_installation_id": INSTALLATION,
                     "harbor_namespace_read": {"rc": 0, "stdout": json.dumps(namespace)}}
        self.assertEqual(run_assertion(guard, variables).returncode, 0)
        namespace["metadata"]["annotations"]["labweaver.io/installation-id"] = "foreign"
        variables["harbor_namespace_read"]["stdout"] = json.dumps(namespace)
        self.assertNotEqual(run_assertion(guard, variables).returncode, 0)
        for observation in ({"rc": 1, "stderr": "forbidden"}, {"rc": 2, "stderr": "connection refused"}):
            variables["harbor_namespace_read"] = observation
            self.assertNotEqual(run_assertion(guard, variables).returncode, 0)
        release_guard = named(source, "Refuse a foreign Harbor Helm installation")
        variables.update({"harbor_release_name": "harbor", "harbor_release_list": {"stdout": '[{"name":"harbor"}]'},
                          "harbor_release_metadata": {"stdout": json.dumps({"name": "harbor", "namespace": "harbor"})}})
        for installation, namespace_name, accepted in ((INSTALLATION, "harbor", True), ("foreign", "harbor", False), (INSTALLATION, "other", False)):
            variables["harbor_release_values"] = {"stdout": json.dumps({"labweaverInstallationId": installation})}
            variables["harbor_release_metadata"]["stdout"] = json.dumps({"name": "harbor", "namespace": namespace_name})
            self.assertEqual(run_assertion(release_guard, variables).returncode == 0, accepted)
        for task_name in ("List the exact Harbor Helm release before any mutation", "Read existing Harbor Helm release metadata", "Read the existing Harbor release installation identity"):
            self.assertNotIn("failed_when", named(source, task_name))
        self.assertLess(source.index(release_guard), source.index(named(source, "Load Harbor credentials from the configured source")))

    def test_harbor_namespace_creation_produces_the_route_guard_identity(self):
        source = tasks("deploy/ansible/roles/harbor/tasks/main.yml")
        producer = named(source, "Bind the new Harbor namespace ownership boundary")
        create = named(source, "Create the isolated Harbor namespace only when verified absent")
        route_guard = named(tasks("deploy/ansible/roles/platform_harbor_route/tasks/main.yml"), "Reject an incomplete or unmanaged Harbor installation")
        with tempfile.TemporaryDirectory() as directory:
            captured = Path(directory) / "namespace.json"
            boundary = Path(directory) / "kubectl"
            boundary.write_text(f"#!{sys.executable}\nimport pathlib, sys\nassert sys.argv[-3:] == ['create', '-f', '-']\npathlib.Path({str(captured)!r}).write_text(sys.stdin.read())\n")
            boundary.chmod(0o755)
            variables = {"harbor_namespace": "harbor", "labweaver_installation_id": INSTALLATION,
                         "harbor_namespace_read": {"rc": 1}, "harbor_kubectl_path": str(boundary), "harbor_kubeconfig_path": "unused"}
            result = run_tasks([producer, create], variables)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            namespace = json.loads(captured.read_text())
            service = {"metadata": {"name": "harbor", "namespace": "harbor", "labels": {"app.kubernetes.io/managed-by": "Helm"},
                        "annotations": {"meta.helm.sh/release-name": "harbor", "meta.helm.sh/release-namespace": "harbor"}},
                       "spec": {"ports": [{"port": 80}, {"port": 443}]}}
            variables.update({"harbor_release_name": "harbor", "platform_harbor_namespace_object": namespace,
                              "platform_harbor_service_object": service,
                              "platform_harbor_endpoint_slices": [{"endpoints": [{"conditions": {"ready": True}}]}]})
            result = run_assertion(route_guard, variables)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            captured.unlink()
            variables["harbor_namespace_read"]["rc"] = 0
            result = run_tasks([producer, create], variables)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertFalse(captured.exists(), "an existing namespace must never be stamped or replaced")
        helm = named(source, "Install or reconcile Harbor with pinned chart and internal dependencies")["kubernetes.core.helm"]
        self.assertEqual(helm["values"]["labweaverInstallationId"], "{{ labweaver_installation_id }}")

    def test_controller_children_resolve_by_uid_to_verified_roots(self):
        for role, prefix in (("platform_application", "platform_application"), ("resource_application", "resource_application")):
            source = tasks(f"deploy/ansible/roles/{role}/tasks/main.yml")
            probe = next(task for task in source if task.get("name", "").startswith("Refuse to adopt unowned") and "objects" in task["name"].lower())
            command = probe_command(probe, {
                f"{prefix}_kubectl": "kubectl", f"{prefix}_kubeconfig": "config", f"{prefix}_namespace": "science",
                f"{prefix}_release_name": "reviewed", f"{prefix}_installation_id": INSTALLATION,
                "installation_boundary_verified_release_names": ["reviewed"],
            })
            program = command.split("jq -e ", 1)[1].strip().strip("'")
            root = sample_object(release="reviewed", namespace="science")
            root["kind"] = "Deployment"
            root["metadata"]["uid"] = "deployment-uid"
            replica = {"kind": "ReplicaSet", "metadata": {"uid": "replica-uid", "name": "replica", "ownerReferences": [{"uid": "deployment-uid"}]}}
            pod = {"kind": "Pod", "metadata": {"uid": "pod-uid", "name": "pod", "ownerReferences": [{"uid": "replica-uid"}]}}
            for owner_uid, accepted in (("replica-uid", True), ("replaced-replica-uid", False), ("pod-uid", False)):
                pod["metadata"]["ownerReferences"][0]["uid"] = owner_uid
                result = subprocess.run(["jq", "-e", program], input=json.dumps({"items": [root, replica, pod]}), text=True, capture_output=True, timeout=10, check=False)
                self.assertEqual(result.returncode == 0, accepted, result.stderr)
            pod["metadata"]["ownerReferences"][0]["uid"] = "replica-uid"
            root["metadata"]["annotations"]["meta.helm.sh/release-name"] = "foreign"
            result = subprocess.run(["jq", "-e", program], input=json.dumps({"items": [root, replica, pod]}), text=True, capture_output=True, check=False)
            self.assertNotEqual(result.returncode, 0)

    def test_cross_namespace_targets_and_final_chart_inventory_fail_closed(self):
        source = tasks("deploy/ansible/roles/platform_application/tasks/ownership.yml")
        namespace_guard = named(source, "Reject foreign application namespace mutation boundaries")
        object_guard = named(source, "Reject foreign cross-namespace application mutation targets")
        for namespace in ("portal", "evaluation", "authoring", "data"):
            for installation, accepted in ((INSTALLATION, True), ("foreign", False)):
                observed = {"item": namespace, "resources": [{"metadata": {"name": namespace, "annotations": {"labweaver.io/installation-id": installation}}}]}
                variables = {"platform_application_installation_id": INSTALLATION, "platform_application_mutation_namespaces": {"results": [observed]}}
                self.assertEqual(run_assertion(namespace_guard, variables).returncode == 0, accepted)
        for kind, name in (("Secret", "portal-tls"), ("CiliumNetworkPolicy", "admin-probes"), ("NetworkPolicy", "oj-runner-default-deny"), ("ConfigMap", "buildkit"), ("RoleBinding", "authoring")):
            observed = {"item": {"kind": kind}, "resources": [{"kind": kind, "metadata": {"name": name, "annotations": {"labweaver.io/installation-id": "foreign"}}}]}
            variables = {"platform_application_installation_id": INSTALLATION, "platform_application_release_name": "reviewed", "platform_application_namespace": "science",
                         "platform_application_cross_namespace_ownership": {"results": [observed]}}
            self.assertNotEqual(run_assertion(object_guard, variables).returncode, 0)
            observed["resources"][0]["metadata"]["annotations"]["labweaver.io/installation-id"] = INSTALLATION
            self.assertEqual(run_assertion(object_guard, variables).returncode, 0)
        chart = tasks("deploy/ansible/roles/platform_application/tasks/chart-ownership.yml")
        chart_probe = named(chart, "Inspect every final chart mutation target including cluster RBAC")
        self.assertEqual(chart_probe["loop"], "{{ platform_application_chart_objects }}")
        self.assertNotIn("failed_when", chart_probe)
        guard = named(chart, "Reject foreign final chart mutation targets")
        variables["platform_application_chart_ownership"] = variables["platform_application_cross_namespace_ownership"]
        observed["resources"][0]["metadata"]["annotations"]["labweaver.io/installation-id"] = "foreign"
        self.assertNotEqual(run_assertion(guard, variables).returncode, 0)
        main = tasks("deploy/ansible/roles/platform_application/tasks/main.yml")
        chart_check = named(main, "Verify every final Helm mutation target before Kubernetes reconciliation")
        first_mutation = next(task for task in main if "kubernetes.core.k8s" in task)
        self.assertLess(main.index(chart_check), main.index(first_mutation))
        clone = named(main, "Require the retained CDI clone source network policy to match the chart")
        policy = {"metadata": {"annotations": {}}, "spec": {"podSelector": {"matchLabels": {"cdi.kubevirt.io": "cdi-clone-source"}}, "policyTypes": ["Egress"], "egress": [
            {"to": [{"namespaceSelector": {"matchLabels": {"kubernetes.io/metadata.name": "kube-system"}}, "podSelector": {"matchLabels": {"k8s-app": "kube-dns"}}}]},
            {"to": [{"namespaceSelector": {"matchLabels": {"labweaver.io/environment": "true"}}, "podSelector": {"matchLabels": {"cdi.kubevirt.io": "cdi-upload-server"}}}]},
        ]}}
        variables["platform_application_cdi_clone_source_policy"] = {"resources": [policy]}
        self.assertNotEqual(run_assertion(clone, variables).returncode, 0)
        policy["metadata"]["annotations"]["labweaver.io/installation-id"] = INSTALLATION
        self.assertEqual(run_assertion(clone, variables).returncode, 0)

    def test_identity_namespace_credentials_cluster_workloads_and_rbac_require_installation(self):
        main = tasks("deploy/ansible/roles/identity_foundation/tasks/main.yml")
        namespace_guard = named(main, "Refuse an identity namespace owned by another installation")
        for installation, accepted in ((INSTALLATION, True), ("foreign", False)):
            variables = {"labweaver_installation_id": INSTALLATION, "identity_existing_namespace": {"resources": [{"metadata": {"annotations": {"labweaver.io/installation-id": installation}}}]}}
            self.assertEqual(run_assertion(namespace_guard, variables).returncode == 0, accepted)
        source = tasks("deploy/ansible/roles/identity_foundation/tasks/ownership.yml")
        guard = named(source, "Refuse to overwrite any unowned identity object")
        for kind in ("Secret", "Cluster", "Deployment", "ClusterRoleBinding"):
            for installation, accepted in ((INSTALLATION, True), ("foreign", False)):
                observed = {"item": {"kind": kind}, "resources": [{"kind": kind, "metadata": {"annotations": {"labweaver.io/installation-id": installation}}}]}
                variables = {"labweaver_installation_id": INSTALLATION, "identity_object_ownership": {"results": [observed]}}
                self.assertEqual(run_assertion(guard, variables).returncode == 0, accepted)
        reconcile = named(main, "Reconcile identity foundation")["block"]
        self.assertLess(next(i for i, task in enumerate(reconcile) if task.get("ansible.builtin.include_tasks") == "ownership.yml"), next(i for i, task in enumerate(reconcile) if "kubernetes.core.k8s" in task))
        self.assertNotIn("failed_when", named(source, "Inspect every planned identity object before mutation"))
        role_only = named(main, "Apply only the reviewed organization-directory reader mapping")["block"]
        self.assertEqual(role_only[0]["ansible.builtin.include_tasks"], "ownership.yml")
        dns = named(reconcile, "Read existing Keycloak DNS records")
        self.assertEqual(dns["failed_when"], "identity_dns_records.rc not in [0, 1]")
        read_error = copy.deepcopy(dns)
        read_error["ansible.builtin.command"] = {"argv": [sys.executable, "-c", "raise SystemExit(2)"]}
        read_error.pop("delegate_to")
        read_error.pop("become")
        result = run_tasks([read_error], {"identity_dnsmasq_enabled": True})
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
